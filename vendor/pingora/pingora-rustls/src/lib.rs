// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! This module contains all the rustls specific pingora integration for things
//! like loading certificates and private keys

#![warn(clippy::all)]

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use log::warn;
pub use no_debug::{Ellipses, NoDebug, WithTypeInfo};
use pingora_error::{Error, ErrorType, OrErr, Result};

pub use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
pub use rustls::server::{ClientCertVerifierBuilder, WebPkiClientVerifier};
pub use rustls::{
    client::WebPkiServerVerifier, crypto::CryptoProvider, version, CertificateError, ClientConfig,
    DigitallySignedStruct, Error as RusTlsError, KeyLogFile, RootCertStore, ServerConfig,
    SignatureScheme, Stream,
};

/// Install the default `ring` CryptoProvider for rustls.
///
/// rustls 0.23+ requires an explicit provider. This function installs `ring`
/// as the process-level default. Safe to call multiple times — subsequent
/// calls are no-ops.
pub fn install_default_crypto_provider() {
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}
pub use rustls_native_certs::load_native_certs;
use rustls_pki_types::pem::{PemObject, SectionKind};
pub use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

/// rustls-pemfile 的 `Item` 枚举覆盖到的段类型，**逐项照抄**它的 `Item::from_kind()`。
///
/// ★ 枢衡改动（G45）：`rustls-pemfile` 失维（RUSTSEC-2025-0134，仓库 2025-08 归档、
/// 无补丁版本），改用 `rustls-pki-types` 自带的 `PemObject`——公告原文说得很清楚，
/// 2.2.0 本来就是 pki-types **同一份解析代码**的薄包装，所以这不是换实现。
///
/// ⚠ **但有一处必须逐字保留，否则就是行为变更而不是适配**：
/// `Item::from_kind()` 对 `SectionKind::EchConfigList` 返回 `None`，而它上面那层
/// `read_one` 写的是 `None => continue`——**ECHCONFIG 段会被静默跳过**。
/// pki-types 的 `(SectionKind, Vec<u8>)` 迭代器会把它交出来，于是
/// [`load_ca_file_into_store`] 的「非证书即报错」会对一个含 ECHCONFIG 的 CA 文件判红。
///
/// ★ 这里用**白名单**而不是把 `EchConfigList` 单独排掉：`SectionKind` 是
/// `#[non_exhaustive]`，白名单能让将来新增的段类型继续被跳过，
/// 与 `rustls-pemfile` **冻结下来**的行为保持一致（它已归档，不会再长出新的 `Item`）。
///
/// 由 `tests::ca_store_silently_skips_echconfig` 守着。
const RUSTLS_PEMFILE_ITEM_KINDS: &[SectionKind] = &[
    SectionKind::Certificate,
    SectionKind::PublicKey,
    SectionKind::RsaPrivateKey,
    SectionKind::PrivateKey,
    SectionKind::EcPrivateKey,
    SectionKind::Crl,
    SectionKind::Csr,
];
pub use tokio_rustls::client::TlsStream as ClientTlsStream;
pub use tokio_rustls::server::TlsStream as ServerTlsStream;
pub use tokio_rustls::{Accept, Connect, TlsAcceptor, TlsConnector, TlsStream};

// This allows to skip certificate verification. Be highly cautious.
pub use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};

/// Load the given file from disk as a buffered reader and use the pingora Error
/// type instead of the std::io version
fn load_file<P>(path: P) -> Result<BufReader<File>>
where
    P: AsRef<Path>,
{
    File::open(path)
        .or_err(ErrorType::FileReadError, "Failed to load file")
        .map(BufReader::new)
}

/// Read the pem file at the given path from disk
///
/// ★ 枢衡改动（G45）：返回类型由 `Vec<Item>` 换成 `Vec<(SectionKind, Vec<u8>)>`——
/// pki-types 没有 `Item` 那个枚举。过滤规则见 [`RUSTLS_PEMFILE_ITEM_KINDS`]。
fn load_pem_file<P>(path: P) -> Result<Vec<(SectionKind, Vec<u8>)>>
where
    P: AsRef<Path>,
{
    <(SectionKind, Vec<u8>)>::pem_reader_iter(load_file(path)?)
        // ★ 解析错误一律往上抛（与 rustls-pemfile 的 read_all 一致）；
        //   只有「解析成功但类型是 Item 覆盖不到的」才丢掉。
        .filter(|res| match res {
            Ok((kind, _)) => RUSTLS_PEMFILE_ITEM_KINDS.contains(kind),
            Err(_) => true,
        })
        .map(|item_res| {
            item_res.or_err(
                ErrorType::InvalidCert,
                "Certificate in pem file could not be read",
            )
        })
        .collect()
}

/// Load the certificates from the given pem file path into the given
/// certificate store
pub fn load_ca_file_into_store<P>(path: P, cert_store: &mut RootCertStore) -> Result<()>
where
    P: AsRef<Path>,
{
    for (kind, der) in load_pem_file(path)? {
        // only loading certificates, handling a CA file
        if kind != SectionKind::Certificate {
            return Error::e_explain(
                ErrorType::InvalidCert,
                "Pem file contains un-loadable certificate type",
            );
        }
        cert_store.add(CertificateDer::from(der)).or_err(
            ErrorType::InvalidCert,
            "Failed to load X509 certificate into root store",
        )?;
    }

    Ok(())
}

/// Attempt to load the native cas into the given root-certificate store
pub fn load_platform_certs_incl_env_into_store(ca_certs: &mut RootCertStore) -> Result<()> {
    // this includes handling of ENV vars SSL_CERT_FILE & SSL_CERT_DIR
    //
    // ★ 枢衡调用点适配（G41）：rustls-native-certs 0.8 起，load_native_certs() 不再返回
    //   Result，而是返回 CertificateResult { certs, errors } —— **部分成功从此是可表达的**。
    //
    // ★ ★ 语义按 0.7.3 逐字保留，而 0.7.3 的语义不是「有错就失败」：
    //     它的 CertPaths::load() 结尾那句注释是 `promote first error if we have no certs
    //     to return`，即 **只有一张都没读到时才返回 Err**；只要读到了哪怕一张，
    //     错误就被咽掉。照搬「errors 非空即失败」会让本函数比 0.7.3 **更严**——
    //     /etc/ssl/certs 里一个读不动的文件就能让反代起不来。那是行为变更，不是适配。
    let result = load_native_certs();

    if result.certs.is_empty() && !result.errors.is_empty() {
        return Error::e_explain(
            ErrorType::InvalidCert,
            format!("Failed to load native certificates: {:?}", result.errors),
        );
    }

    // 0.7.3 把这些非致命错误直接丢掉。0.8 既然把它们暴露了出来，就至少留下痕迹——
    // ★ 控制流与 0.7.3 完全一致，只是不再无声。
    for err in &result.errors {
        warn!("ignored error while loading native certificates: {err}");
    }

    for cert in result.certs {
        ca_certs.add(cert).or_err(
            ErrorType::InvalidCert,
            "Failed to load native certificate into root store",
        )?;
    }

    Ok(())
}

/// Load the certificates and private key files
pub fn load_certs_and_key_files<'a>(
    cert: &str,
    key: &str,
) -> Result<Option<(Vec<CertificateDer<'a>>, PrivateKeyDer<'a>)>> {
    let certs_file = load_pem_file(cert)?;
    let key_file = load_pem_file(key)?;

    let certs = certs_file
        .into_iter()
        .filter_map(|(kind, der)| {
            if kind == SectionKind::Certificate {
                Some(CertificateDer::from(der))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    // These are the currently supported pk types -
    // [https://doc.servo.org/rustls/key/struct.PrivateKey.html]
    //
    // ★ 枢衡改动（G45）：`PrivateKeyDer::from_pem()` 认的**恰好**是
    //   RsaPrivateKey / EcPrivateKey / PrivateKey 三种，与原来那个手写的
    //   Pkcs1/Sec1/Pkcs8 三分支逐项一致（pki-types 源码里就是这三条 match 臂）。
    //   仍然取**文件里的第一把**，不按类型排优先级。
    let private_key_opt = key_file
        .into_iter()
        .find_map(|(kind, der)| PrivateKeyDer::from_pem(kind, der));

    if let (Some(private_key), false) = (private_key_opt, certs.is_empty()) {
        Ok(Some((certs, private_key)))
    } else {
        Ok(None)
    }
}

/// Load the certificate
///
/// ★ 枢衡改动（G45）：`rustls_pemfile::certs()` → `CertificateDer::pem_reader_iter()`。
/// 两者形状相同——非证书段跳过、解析错误交出来——★ 且**仍然读完整个文件**，
/// 所以第一张证书之后的语法错误照样判红（原实现先 `collect::<Result<Vec<_>>>()?`
/// 再取 `first()`，这一点容易在「取到就返回」的重写里悄悄丢掉）。
pub fn load_pem_file_ca(path: &String) -> Result<Vec<u8>> {
    let mut first = None;
    for cert_res in CertificateDer::pem_reader_iter(load_file(path)?) {
        let cert = cert_res.or_err(
            ErrorType::InvalidCert,
            "Failed to load certificate from file",
        )?;
        if first.is_none() {
            first = Some(cert.to_vec());
        }
    }

    Ok(first.unwrap_or_default())
}

/// ★ 枢衡改动（G45）：`rustls_pemfile::private_key()` → `PrivateKeyDer::pem_reader_iter()`。
/// 原实现在拿到第一把密钥时就返回，**不再读后面的内容**，所以后面的语法错误看不见；
/// 这里保持同样的早退。一把都没有时返回**空 Vec 而不是错误**，同样照旧。
pub fn load_pem_file_private_key(path: &String) -> Result<Vec<u8>> {
    for key_res in PrivateKeyDer::pem_reader_iter(load_file(path)?) {
        let key = key_res.or_err(
            ErrorType::InvalidCert,
            "Failed to load private key from file",
        )?;
        return Ok(key.secret_der().to_vec());
    }

    Ok(Vec::new())
}

pub fn hash_certificate(cert: &CertificateDer) -> Vec<u8> {
    let hash = ring::digest::digest(&ring::digest::SHA256, cert.as_ref());
    hash.as_ref().to_vec()
}

// ─────────────────────────────────────────────────────────────────────────────
// ★ 枢衡改动 3/3（G45）：PEM 加载的特征化测试。
//
// 这些测试**先照着 rustls-pemfile 的现有行为写并跑绿**，然后才做迁移
// （RUSTSEC-2025-0134，rustls-pemfile 失维）。理由是一条实测出来的事实：
// **迁移之前，pingora-rustls 这个 crate 一条测试都没有**——
// 而「一个没有任何测试的 crate」与「一个没有门的改动」在退出码上完全一样。
//
// ★ 最要紧的一条是 `ca_store_silently_skips_echconfig`：
// rustls-pemfile 的 `Item::from_kind()` 对 `SectionKind::EchConfigList` 返回 `None`，
// 而 `read_one` 那一层写着 `None => continue`——所以 `read_all` **静默跳过 ECHCONFIG 段**。
// pki-types 的 `(SectionKind, Vec<u8>)` 迭代器会把它**吐出来**，
// 于是 `load_ca_file_into_store()` 的「非证书即报错」会对含 ECHCONFIG 的 CA 文件判红。
// 那是行为变更，不是适配。这条测试就是拿来钉住它的。
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 抄自 `pingora-core/tests/keys/server.crt`（上游自带的自签测试证书）。
    /// ★ 只有需要 `RootCertStore::add()` 真的解析成功的用例才用它；
    ///   其余用例用结构合法但内容随意的假 PEM——那几个函数不做 DER 校验，
    ///   用假的反而能证明「测的是 PEM 分派，不是 rustls 的证书解析」。
    const REAL_CERT: &str = "\
-----BEGIN CERTIFICATE-----
MIIB9zCCAZ2gAwIBAgIUMI7aLvTxyRFCHhw57hGt4U6yupcwCgYIKoZIzj0EAwIw
ZDELMAkGA1UEBhMCVVMxCzAJBgNVBAgMAkNBMRYwFAYDVQQHDA1TYW4gRnJhbmNp
c2NvMRgwFgYDVQQKDA9DbG91ZGZsYXJlLCBJbmMxFjAUBgNVBAMMDW9wZW5ydXN0
eS5vcmcwHhcNMjIwNDExMjExMzEzWhcNMzIwNDA4MjExMzEzWjBkMQswCQYDVQQG
EwJVUzELMAkGA1UECAwCQ0ExFjAUBgNVBAcMDVNhbiBGcmFuY2lzY28xGDAWBgNV
BAoMD0Nsb3VkZmxhcmUsIEluYzEWMBQGA1UEAwwNb3BlbnJ1c3R5Lm9yZzBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABNn/9RZtR48knaJD6tk9BdccaJfZ0hGEPn6B
SDXmlmJPhcTBqa4iUwW/ABpGvO3FpJcNWasrX2k+qZLq3g205MKjLTArMCkGA1Ud
EQQiMCCCDyoub3BlbnJ1c3R5Lm9yZ4INb3BlbnJ1c3R5Lm9yZzAKBggqhkjOPQQD
AgNIADBFAiAjISZ9aEKmobKGlT76idO740J6jPaX/hOrm41MLeg69AIhAJqKrSyz
wD/AAF5fR6tXmBqlnpQOmtxfdy13wDr4MT3h
-----END CERTIFICATE-----
";

    const FAKE_CERT_A: &str = "-----BEGIN CERTIFICATE-----\nYWFh\n-----END CERTIFICATE-----\n";
    const FAKE_CERT_B: &str = "-----BEGIN CERTIFICATE-----\nYmJi\n-----END CERTIFICATE-----\n";
    const PKCS8_KEY: &str = "-----BEGIN PRIVATE KEY-----\ncGs4\n-----END PRIVATE KEY-----\n";
    const PKCS1_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\ncGsx\n-----END RSA PRIVATE KEY-----\n";
    const SEC1_KEY: &str = "-----BEGIN EC PRIVATE KEY-----\nc2Mx\n-----END EC PRIVATE KEY-----\n";
    const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\ncHVi\n-----END PUBLIC KEY-----\n";
    /// ★ rustls-pemfile 的 `Item` 覆盖不到的段类型——它会被静默跳过。
    const ECH_CONFIG: &str = "-----BEGIN ECHCONFIG-----\nZWNo\n-----END ECHCONFIG-----\n";

    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// 写一个临时 PEM 文件并返回路径。★ 不引 tempfile 依赖：
    /// 给一个失维包搬家的改动，不该顺手再添一个新依赖。
    fn pem_file(body: &str) -> String {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "pingora-rustls-pemtest-{}-{}.pem",
            std::process::id(),
            n
        ));
        let mut f = File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path.to_str().unwrap().to_string()
    }

    // ── load_certs_and_key_files ─────────────────────────────────────────────

    #[test]
    fn certs_and_key_loads_all_certs_and_the_first_key() {
        let certs = pem_file(&format!("{FAKE_CERT_A}{FAKE_CERT_B}"));
        let key = pem_file(PKCS8_KEY);

        let (loaded_certs, loaded_key) = load_certs_and_key_files(&certs, &key)
            .unwrap()
            .expect("both files parse, so this must be Some");

        assert_eq!(loaded_certs.len(), 2);
        assert_eq!(loaded_certs[0].as_ref(), b"aaa");
        assert_eq!(loaded_certs[1].as_ref(), b"bbb");
        assert_eq!(loaded_key.secret_der(), b"pk8");
    }

    #[test]
    fn certs_and_key_accepts_all_three_key_encodings() {
        let certs = pem_file(FAKE_CERT_A);
        for (body, want) in [
            (PKCS1_KEY, &b"pk1"[..]),
            (PKCS8_KEY, &b"pk8"[..]),
            (SEC1_KEY, &b"sc1"[..]),
        ] {
            let key = pem_file(body);
            let (_, loaded) = load_certs_and_key_files(&certs, &key).unwrap().unwrap();
            assert_eq!(loaded.secret_der(), want);
        }
    }

    #[test]
    fn certs_and_key_takes_the_first_key_in_file_order() {
        // ★ 现有实现是 `.filter_map(..).next()`，即**文件里的第一把**，
        //   与密钥类型的偏好无关。迁移后必须还是同一把。
        let certs = pem_file(FAKE_CERT_A);
        let key = pem_file(&format!("{SEC1_KEY}{PKCS8_KEY}{PKCS1_KEY}"));
        let (_, loaded) = load_certs_and_key_files(&certs, &key).unwrap().unwrap();
        assert_eq!(loaded.secret_der(), b"sc1");
    }

    #[test]
    fn certs_and_key_is_none_when_a_half_is_missing() {
        // 有证书没密钥
        let certs = pem_file(FAKE_CERT_A);
        let no_key = pem_file(PUBLIC_KEY);
        assert!(load_certs_and_key_files(&certs, &no_key).unwrap().is_none());

        // 有密钥没证书
        let no_cert = pem_file(PUBLIC_KEY);
        let key = pem_file(PKCS8_KEY);
        assert!(load_certs_and_key_files(&no_cert, &key).unwrap().is_none());
    }

    #[test]
    fn certs_and_key_ignores_non_certificate_sections_in_the_cert_file() {
        // ★ 与 load_ca_file_into_store 不同：这一个是 filter_map，多余的段被**丢掉**而不是报错。
        //   两个函数对同一份文件的态度不一样，这本身就值得钉住。
        let certs = pem_file(&format!("{PUBLIC_KEY}{FAKE_CERT_A}"));
        let key = pem_file(PKCS8_KEY);
        let (loaded, _) = load_certs_and_key_files(&certs, &key).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].as_ref(), b"aaa");
    }

    // ── load_ca_file_into_store ──────────────────────────────────────────────

    #[test]
    fn ca_store_loads_a_real_certificate() {
        let path = pem_file(REAL_CERT);
        let mut store = RootCertStore::empty();
        load_ca_file_into_store(&path, &mut store).unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn ca_store_rejects_a_recognised_non_certificate_section() {
        let path = pem_file(PUBLIC_KEY);
        let mut store = RootCertStore::empty();
        assert!(
            load_ca_file_into_store(&path, &mut store).is_err(),
            "a PUBLIC KEY section must be rejected, not skipped"
        );
    }

    #[test]
    fn ca_store_silently_skips_echconfig() {
        // ★ ★ ★ 本文件里最要紧的一条。rustls-pemfile 的 Item 覆盖不到 ECHCONFIG，
        //   于是 read_all **根本不会把它交出来**，循环体一次都不进，函数返回 Ok(())。
        //   pki-types 的 (SectionKind, Vec<u8>) 迭代器**会**把它交出来——照直迁移会判红。
        let path = pem_file(ECH_CONFIG);
        let mut store = RootCertStore::empty();
        load_ca_file_into_store(&path, &mut store)
            .expect("an ECHCONFIG section is skipped, not an error");
        assert_eq!(store.len(), 0);

        // 与真证书混在一起时也一样：证书进店，ECHCONFIG 无声消失。
        let mixed = pem_file(&format!("{ECH_CONFIG}{REAL_CERT}"));
        let mut store = RootCertStore::empty();
        load_ca_file_into_store(&mixed, &mut store).unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn ca_store_accepts_an_empty_file() {
        let path = pem_file("");
        let mut store = RootCertStore::empty();
        load_ca_file_into_store(&path, &mut store).unwrap();
        assert_eq!(store.len(), 0);
    }

    // ── load_pem_file_ca / load_pem_file_private_key ─────────────────────────

    #[test]
    fn pem_file_ca_returns_the_first_certificate_only() {
        let path = pem_file(&format!("{FAKE_CERT_A}{FAKE_CERT_B}"));
        assert_eq!(load_pem_file_ca(&path).unwrap(), b"aaa");
    }

    #[test]
    fn pem_file_ca_still_errors_on_a_malformed_section_after_the_first_cert() {
        // ★ 原实现是 `collect::<Result<Vec<_>>>()?` 之后才取 `first()`，也就是**读完整个文件**。
        //   把它重写成「取到第一张就返回」看起来等价，实际会把这里的错误吞掉。
        //   这条测试是为了不让那句注释停留在断言上——**断言型注释比没注释更危险**。
        let path = pem_file(&format!("{FAKE_CERT_A}-----BEGIN CERTIFICATE-----\nYWFh\n"));
        assert!(
            load_pem_file_ca(&path).is_err(),
            "a truncated section after the first certificate must still be an error"
        );
    }

    #[test]
    fn pem_file_private_key_stops_at_the_first_key() {
        // ★ 反过来：私钥那个函数**是**早退的，第一把之后的语法错误看不见。
        //   两个函数在这一点上不一样，而这不对称正是最容易在重写时被「统一」掉的东西。
        let path = pem_file(&format!("{PKCS8_KEY}-----BEGIN CERTIFICATE-----\nYWFh\n"));
        assert_eq!(
            load_pem_file_private_key(&path).unwrap(),
            b"pk8",
            "the malformed tail is never reached, so this must succeed"
        );
    }

    #[test]
    fn pem_file_ca_returns_empty_when_there_is_no_certificate() {
        // ★ 注意是**空 Vec 而不是错误**——`certs()` 过滤掉非证书段，`first()` 给 None，
        //   然后 `unwrap_or_default()`。调用方拿到的是空，不是失败。
        let path = pem_file(PUBLIC_KEY);
        assert!(load_pem_file_ca(&path).unwrap().is_empty());
    }

    #[test]
    fn pem_file_private_key_returns_the_first_key_of_any_encoding() {
        for (body, want) in [
            (PKCS1_KEY, &b"pk1"[..]),
            (PKCS8_KEY, &b"pk8"[..]),
            (SEC1_KEY, &b"sc1"[..]),
        ] {
            let path = pem_file(body);
            assert_eq!(load_pem_file_private_key(&path).unwrap(), want);
        }
        // 跳过前面的非密钥段，取第一把密钥
        let path = pem_file(&format!("{FAKE_CERT_A}{PUBLIC_KEY}{SEC1_KEY}{PKCS8_KEY}"));
        assert_eq!(load_pem_file_private_key(&path).unwrap(), b"sc1");
    }

    #[test]
    fn pem_file_private_key_returns_empty_when_there_is_no_key() {
        let path = pem_file(FAKE_CERT_A);
        assert!(load_pem_file_private_key(&path).unwrap().is_empty());
    }

    // ── 错误路径 ─────────────────────────────────────────────────────────────

    #[test]
    fn a_missing_end_marker_is_an_error_everywhere() {
        let truncated = "-----BEGIN CERTIFICATE-----\nYWFh\n";
        let path = pem_file(truncated);

        let mut store = RootCertStore::empty();
        assert!(load_ca_file_into_store(&path, &mut store).is_err());
        assert!(load_pem_file_ca(&path).is_err());

        let key = pem_file(PKCS8_KEY);
        assert!(load_certs_and_key_files(&path, &key).is_err());
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let missing = "/definitely/not/here/absent.pem".to_string();
        let mut store = RootCertStore::empty();
        assert!(load_ca_file_into_store(&missing, &mut store).is_err());
        assert!(load_pem_file_ca(&missing).is_err());
        assert!(load_pem_file_private_key(&missing).is_err());
    }

    #[test]
    fn junk_outside_sections_is_ignored() {
        // PEM 解析器允许段与段之间有任意垃圾文本。
        let path = pem_file(&format!("hello\n{FAKE_CERT_A}goodbye\n{FAKE_CERT_B}trailing"));
        let key = pem_file(PKCS8_KEY);
        let (certs, _) = load_certs_and_key_files(&path, &key).unwrap().unwrap();
        assert_eq!(certs.len(), 2);
    }
}
