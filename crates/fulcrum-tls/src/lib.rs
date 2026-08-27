//! 枢衡的 TLS 层。
//!
//! | 模块 | 管什么 | 拍板 |
//! |---|---|---|
//! | [`store`] | 证书存储：每域一目录的 PEM、原子写、文件锁 | G55 |
//! | [`resolver`] | 按 SNI 挑证书（BoringSSL 的 `select_certificate_callback`）| **G104** / G12 |
//! | [`renewal`] | 续期判定：ARI 优先，回退剩余寿命 1/3，退避 + 抖动 | G56 |
//!
//! # ⚠ ⚠ 后端是 BoringSSL（G104）
//!
//! §5.1 第 1 条原本把 TLS 后端锁死在 rustls（理由：rustls 不支持 `certificate_callback`，
//! 动态证书选择必须实现 `ResolvesServerCert`）。**G104 推翻了它** —— 本项目第一次推翻
//! 一条写着「不可回头」的约束，成因是 G103 取 `quiche` 做 HTTP/3 而 quiche 用 BoringSSL。
//!
//! ⇒ 本 crate 的载体类型整条换过：
//!
//! | 原来（rustls）| 现在（BoringSSL）|
//! |---|---|
//! | `CertifiedKey`（链 + 签名密钥）| [`CertKey`]（`Vec<X509>` + `PKey<Private>`）|
//! | `impl ResolvesServerCert`（同步 `resolve()`）| [`SniResolver::install_into`] 挂 `set_select_certificate_callback` |
//! | `CertificateDer` / `PrivateKeyDer` | `X509` / `PKey<Private>` |
//!
//! ★ ★ **换的是载体，不是对证书的理解**：有效期与 SAN 仍由 [`store::validity_of`]
//! 走 `x509-parser` 从 DER 里读，逐字未改。⇒ 这一批红了，红的一定是后端那一层。
//!
//! ★ ★ **BoringSSL 的类型只经 `pingora-boringssl` 拿**，本 crate 不直接依赖 `boring`。
//! 理由与「只经 `pingora-rustls` 拿 rustls」完全一样，而且更硬：
//! `boring` 的版本必须与 `quiche` 解到同一个（G103/G104），
//! **多一处版本声明就多一个把它们劈成两份的机会**，而劈开之后
//! `SslContextBuilder` 同名却是两个类型。

pub mod renewal;
pub mod resolver;
pub mod store;

pub use resolver::{ACME_TLS_ALPN, CertKey, ChallengeCert, SniResolver, alpn_list, offers_alpn};
pub use store::{CertStore, Meta, StoredCert};

use pingora_boringssl::pkey::{PKey, Private};
use pingora_boringssl::x509::X509;
use std::sync::Arc;
use std::time::SystemTime;

/// 一张装好的证书，连同它自己声明覆盖哪些域名。
pub struct LoadedCert {
    pub key: Arc<CertKey>,
    /// 来自证书的 SAN，**不是**配置里写的站点名。
    pub domains: Vec<String>,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

/// 把证书链与私钥变成握手期能用的 [`CertKey`]。
///
/// ★ ★ **G104 之前这里要先 `install_default_crypto_provider()`** ——
/// rustls 0.23+ 把「用哪个 crypto provider」做成一个进程级的全局选择，
/// 不装就会在**第一次签名时**才炸，而那时已经在握手路径上了。
/// **BoringSSL 没有这个概念**：算法实现就在库里，没有第二个候选，也就没有装错的可能。
/// ⇒ 这个函数因此变成纯粹的搬运，而**那条「不调就会在握手时才炸」的坑整条消失了**。
pub fn cert_key(chain: Vec<X509>, key: PKey<Private>) -> Result<Arc<CertKey>, String> {
    if chain.is_empty() {
        return Err("证书链是空的".to_string());
    }
    Ok(Arc::new(CertKey { chain, key }))
}

/// 把一张**裸 DER** 证书与一份 PKCS#8 DER 私钥变成 [`CertKey`]。
///
/// ★ 只给 TLS-ALPN-01 的挑战证书用（RFC 8737）：那张证书是现造的、只活几秒、
/// 从不落盘，走 PEM 编解码一圈纯属多余。
/// ⚠ 它**不做任何校验**——调用方造出来的东西自己负责。真证书那条路走
/// [`load_pem_pair`] / [`to_loaded`]，前面还有 SAN 抽取与有效期检查。
pub fn cert_key_from_der(
    cert_der: Vec<u8>,
    pkcs8_key_der: Vec<u8>,
) -> Result<Arc<CertKey>, String> {
    let cert = X509::from_der(&cert_der).map_err(|e| format!("挑战证书不是合法 DER：{e}"))?;
    let key = PKey::private_key_from_pkcs8(&pkcs8_key_der)
        .map_err(|e| format!("挑战证书的私钥不是合法 PKCS#8 DER：{e}"))?;
    cert_key(vec![cert], key)
}

/// 读一对 PEM 文件（`tls <cert> <key>`）。
///
/// ★ ★ **这一步必须在装载时做，不能等到握手**：一份路径写错、权限不对、或者
/// 私钥与证书对不上的配置，应当在 `fulcrum validate` 就红，
/// 而不是在第一个客户端连上来的时候变成一次握手失败。
pub fn load_pem_pair(cert_path: &str, key_path: &str) -> Result<LoadedCert, String> {
    let cert_pem = std::fs::read(cert_path).map_err(|e| format!("读不动 `{cert_path}`：{e}"))?;
    let key_pem = std::fs::read(key_path).map_err(|e| format!("读不动 `{key_path}`：{e}"))?;
    let chain =
        X509::stack_from_pem(&cert_pem).map_err(|e| format!("`{cert_path}` 解析失败：{e}"))?;
    if chain.is_empty() {
        return Err(format!("`{cert_path}` 里没有证书"));
    }
    let key = PKey::private_key_from_pem(&key_pem)
        .map_err(|e| format!("`{key_path}` 解析失败（里面没有私钥？）：{e}"))?;
    let first = chain
        .first()
        .ok_or_else(|| format!("`{cert_path}` 的证书链是空的"))?;
    let (not_before, not_after, domains) = store::validity_of(first)?;
    let key = cert_key(chain, key)?;
    Ok(LoadedCert {
        key,
        domains,
        not_before,
        not_after,
    })
}

/// 把存储里读出来的一张证书变成可装载的形态。
pub fn to_loaded(sc: StoredCert) -> Result<LoadedCert, String> {
    let StoredCert {
        chain,
        key,
        not_before,
        not_after,
        domains,
        ..
    } = sc;
    let key = cert_key(chain, key)?;
    Ok(LoadedCert {
        key,
        domains,
        not_before,
        not_after,
    })
}
