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
pub use rustls::server::{
    ClientCertVerifierBuilder, ClientHello, ResolvesServerCert, WebPkiClientVerifier,
};
pub use rustls::sign;
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

/// rustls-pemfile 的 `Item` 枚舉覆蓋到的段類型，**逐項照抄**它的 `Item::from_kind()`。
///
/// ★ 樞衡改動（G45）：`rustls-pemfile` 失維（RUSTSEC-2025-0134，儲存庫 2025-08 歸檔、
/// 無補丁版本），改用 `rustls-pki-types` 自帶的 `PemObject`——公告原文說得很清楚，
/// 2.2.0 本來就是 pki-types **同一份解析程式碼**的薄包裝，所以這不是換實作。
///
/// ⚠ **但有一處必須逐字保留，否則就是行為變更而不是適配**：
/// `Item::from_kind()` 對 `SectionKind::EchConfigList` 回傳 `None`，而它上面那層
/// `read_one` 寫的是 `None => continue`——**ECHCONFIG 段會被靜默跳過**。
/// pki-types 的 `(SectionKind, Vec<u8>)` 迭代器會把它交出來，於是
/// [`load_ca_file_into_store`] 的「非證書即回報錯誤」會對一個含 ECHCONFIG 的 CA 檔案判紅。
///
/// ★ 這裡用**白名單**而不是把 `EchConfigList` 單獨排掉：`SectionKind` 是
/// `#[non_exhaustive]`，白名單能讓將來新增的段類型繼續被跳過，
/// 與 `rustls-pemfile` **凍結下來**的行為保持一致（它已歸檔，不會再長出新的 `Item`）。
///
/// 由 `tests::ca_store_silently_skips_echconfig` 守著。
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
/// ★ 樞衡改動（G45）：回傳類型由 `Vec<Item>` 換成 `Vec<(SectionKind, Vec<u8>)>`——
/// pki-types 沒有 `Item` 那個枚舉。過濾規則見 [`RUSTLS_PEMFILE_ITEM_KINDS`]。
fn load_pem_file<P>(path: P) -> Result<Vec<(SectionKind, Vec<u8>)>>
where
    P: AsRef<Path>,
{
    <(SectionKind, Vec<u8>)>::pem_reader_iter(load_file(path)?)
        // ★ 解析錯誤一律往上拋（與 rustls-pemfile 的 read_all 一致）；
        //   只有「解析成功但類型是 Item 覆蓋不到的」才丟掉。
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
    // ★ 樞衡呼叫點適配（G41）：rustls-native-certs 0.8 起，load_native_certs() 不再回傳
    //   Result，而是回傳 CertificateResult { certs, errors } —— **部分成功從此是可表達的**。
    //
    // ★ ★ 語義按 0.7.3 逐字保留，而 0.7.3 的語義不是「有錯就失敗」：
    //     它的 CertPaths::load() 結尾那句註解是 `promote first error if we have no certs
    //     to return`，即 **只有一張都沒讀到時才回傳 Err**；只要讀到了哪怕一張，
    //     錯誤就被咽掉。照搬「errors 非空即失敗」會讓本函式比 0.7.3 **更嚴**——
    //     /etc/ssl/certs 裡一個讀不動的檔案就能讓反代起不來。那是行為變更，不是適配。
    let result = load_native_certs();

    if result.certs.is_empty() && !result.errors.is_empty() {
        return Error::e_explain(
            ErrorType::InvalidCert,
            format!("Failed to load native certificates: {:?}", result.errors),
        );
    }

    // 0.7.3 把這些非致命錯誤直接丟掉。0.8 既然把它們暴露了出來，就至少留下痕跡——
    // ★ 控制流與 0.7.3 完全一致，只是不再無聲。
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
    // ★ 樞衡改動（G45）：`PrivateKeyDer::from_pem()` 認的**恰好**是
    //   RsaPrivateKey / EcPrivateKey / PrivateKey 三種，與原來那個手寫的
    //   Pkcs1/Sec1/Pkcs8 三分支逐項一致（pki-types 原始碼裡就是這三條 match 臂）。
    //   仍然取**檔案裡的第一把**，不按類型排優先級。
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
/// ★ 樞衡改動（G45）：`rustls_pemfile::certs()` → `CertificateDer::pem_reader_iter()`。
/// 兩者形狀相同——非證書段跳過、解析錯誤交出來——★ 且**仍然讀完整個檔案**，
/// 所以第一張證書之後的語法錯誤照樣判紅（原實作先 `collect::<Result<Vec<_>>>()?`
/// 再取 `first()`，這一點容易在「取到就回傳」的重寫裡悄悄丟掉）。
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

/// ★ 樞衡改動（G45）：`rustls_pemfile::private_key()` → `PrivateKeyDer::pem_reader_iter()`。
/// 原實作在拿到第一把金鑰時就回傳，**不再讀後面的內容**，所以後面的語法錯誤看不見；
/// 這裡保持同樣的早退。一把都沒有時回傳**空 Vec 而不是錯誤**，同樣照舊。
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
// ★ 樞衡改動 3/3（G45）：PEM 載入的特徵化測試。
//
// 這些測試**先照著 rustls-pemfile 的現有行為寫並跑綠**，然後才做遷移
// （RUSTSEC-2025-0134，rustls-pemfile 失維）。理由是一條實測出來的事實：
// **遷移之前，pingora-rustls 這個 crate 一條測試都沒有**——
// 而「一個沒有任何測試的 crate」與「一個沒有門的改動」在結束代碼上完全一樣。
//
// ★ 最要緊的一條是 `ca_store_silently_skips_echconfig`：
// rustls-pemfile 的 `Item::from_kind()` 對 `SectionKind::EchConfigList` 回傳 `None`，
// 而 `read_one` 那一層寫著 `None => continue`——所以 `read_all` **靜默跳過 ECHCONFIG 段**。
// pki-types 的 `(SectionKind, Vec<u8>)` 迭代器會把它**吐出來**，
// 於是 `load_ca_file_into_store()` 的「非證書即回報錯誤」會對含 ECHCONFIG 的 CA 檔案判紅。
// 那是行為變更，不是適配。這條測試就是拿來釘住它的。
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 抄自 `pingora-core/tests/keys/server.crt`（上游自帶的自籤測試證書）。
    /// ★ 只有需要 `RootCertStore::add()` 真的解析成功的用例才用它；
    ///   其餘用例用結構合法但內容隨意的假 PEM——那幾個函式不做 DER 檢核，
    ///   用假的反而能證明「測的是 PEM 分派，不是 rustls 的證書解析」。
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
    const PKCS1_KEY: &str =
        "-----BEGIN RSA PRIVATE KEY-----\ncGsx\n-----END RSA PRIVATE KEY-----\n";
    const SEC1_KEY: &str = "-----BEGIN EC PRIVATE KEY-----\nc2Mx\n-----END EC PRIVATE KEY-----\n";
    const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\ncHVi\n-----END PUBLIC KEY-----\n";
    /// ★ rustls-pemfile 的 `Item` 覆蓋不到的段類型——它會被靜默跳過。
    const ECH_CONFIG: &str = "-----BEGIN ECHCONFIG-----\nZWNo\n-----END ECHCONFIG-----\n";

    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// 寫一個臨時 PEM 檔案並回傳路徑。★ 不引 tempfile 依賴：
    /// 給一個失維包搬家的改動，不該順手再添一個新依賴。
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
        // ★ 現有實作是 `.filter_map(..).next()`，即**檔案裡的第一把**，
        //   與金鑰類型的偏好無關。遷移後必須還是同一把。
        let certs = pem_file(FAKE_CERT_A);
        let key = pem_file(&format!("{SEC1_KEY}{PKCS8_KEY}{PKCS1_KEY}"));
        let (_, loaded) = load_certs_and_key_files(&certs, &key).unwrap().unwrap();
        assert_eq!(loaded.secret_der(), b"sc1");
    }

    #[test]
    fn certs_and_key_is_none_when_a_half_is_missing() {
        // 有證書沒金鑰
        let certs = pem_file(FAKE_CERT_A);
        let no_key = pem_file(PUBLIC_KEY);
        assert!(load_certs_and_key_files(&certs, &no_key).unwrap().is_none());

        // 有金鑰沒證書
        let no_cert = pem_file(PUBLIC_KEY);
        let key = pem_file(PKCS8_KEY);
        assert!(load_certs_and_key_files(&no_cert, &key).unwrap().is_none());
    }

    #[test]
    fn certs_and_key_ignores_non_certificate_sections_in_the_cert_file() {
        // ★ 與 load_ca_file_into_store 不同：這一個是 filter_map，多餘的段被**丟掉**而不是回報錯誤。
        //   兩個函式對同一份檔案的態度不一樣，這本身就值得釘住。
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
        // ★ ★ ★ 本檔案裡最要緊的一條。rustls-pemfile 的 Item 覆蓋不到 ECHCONFIG，
        //   於是 read_all **根本不會把它交出來**，循環體一次都不進，函式回傳 Ok(())。
        //   pki-types 的 (SectionKind, Vec<u8>) 迭代器**會**把它交出來——照直遷移會判紅。
        let path = pem_file(ECH_CONFIG);
        let mut store = RootCertStore::empty();
        load_ca_file_into_store(&path, &mut store)
            .expect("an ECHCONFIG section is skipped, not an error");
        assert_eq!(store.len(), 0);

        // 與真證書混在一起時也一樣：證書進店，ECHCONFIG 無聲消失。
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
        // ★ 原實作是 `collect::<Result<Vec<_>>>()?` 之後才取 `first()`，也就是**讀完整個檔案**。
        //   把它重寫成「取到第一張就回傳」看起來等價，實際會把這裡的錯誤吞掉。
        //   這條測試是為了不讓那句註解停留在斷言上——**斷言型註解比沒註解更危險**。
        let path = pem_file(&format!("{FAKE_CERT_A}-----BEGIN CERTIFICATE-----\nYWFh\n"));
        assert!(
            load_pem_file_ca(&path).is_err(),
            "a truncated section after the first certificate must still be an error"
        );
    }

    #[test]
    fn pem_file_private_key_stops_at_the_first_key() {
        // ★ 反過來：私鑰那個函式**是**早退的，第一把之後的語法錯誤看不見。
        //   兩個函式在這一點上不一樣，而這不對稱正是最容易在重寫時被「統一」掉的東西。
        let path = pem_file(&format!("{PKCS8_KEY}-----BEGIN CERTIFICATE-----\nYWFh\n"));
        assert_eq!(
            load_pem_file_private_key(&path).unwrap(),
            b"pk8",
            "the malformed tail is never reached, so this must succeed"
        );
    }

    #[test]
    fn pem_file_ca_returns_empty_when_there_is_no_certificate() {
        // ★ 注意是**空 Vec 而不是錯誤**——`certs()` 過濾掉非證書段，`first()` 給 None，
        //   然後 `unwrap_or_default()`。呼叫方拿到的是空，不是失敗。
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
        // 跳過前面的非金鑰段，取第一把金鑰
        let path = pem_file(&format!("{FAKE_CERT_A}{PUBLIC_KEY}{SEC1_KEY}{PKCS8_KEY}"));
        assert_eq!(load_pem_file_private_key(&path).unwrap(), b"sc1");
    }

    #[test]
    fn pem_file_private_key_returns_empty_when_there_is_no_key() {
        let path = pem_file(FAKE_CERT_A);
        assert!(load_pem_file_private_key(&path).unwrap().is_empty());
    }

    // ── 錯誤路徑 ─────────────────────────────────────────────────────────────

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
        // PEM 解析器允許段與段之間有任意垃圾文字。
        let path = pem_file(&format!(
            "hello\n{FAKE_CERT_A}goodbye\n{FAKE_CERT_B}trailing"
        ));
        let key = pem_file(PKCS8_KEY);
        let (certs, _) = load_certs_and_key_files(&path, &key).unwrap().unwrap();
        assert_eq!(certs.len(), 2);
    }
}
