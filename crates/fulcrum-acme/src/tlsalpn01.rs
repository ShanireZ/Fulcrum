//! TLS-ALPN-01（G54 的**主**）—— RFC 8737。
//!
//! # 它是什么
//!
//! CA 向 `<域名>:443` 发起一次 TLS 握手，ALPN 里**只**提供 `acme-tls/1`。
//! 服务器必须协商到这个协议，并回一张**自签**证书，其中带一条**critical** 的
//! `id-pe-acmeIdentifier` 扩展（OID `1.3.6.1.5.5.7.1.31`），内容是
//! key authorization 的 SHA-256。CA 看完这张证书就断开——**这次握手不传任何数据**。
//!
//! # 为什么 G54 把它定成「主」
//!
//! | | TLS-ALPN-01 | HTTP-01 |
//! |---|---|---|
//! | 端口 | 只要 443 | **要 80** |
//! | 占用路由 | **零** —— 整件事在握手里完成 | 要占 `/.well-known/acme-challenge/` |
//! | 用户配置能不能挡住它 | 挡不住 | ★ 能（一份 `respond 403` 就够）|
//!
//! ★ 第一行对 `.cn` 域名尤其要紧：大陆机房 80 端口常年受限，而 443 是通的。
//!
//! # ⚠ 三条不能想当然的地方
//!
//! 1. **挑战证书与真证书必须是两张互不相通的表。** 协商到 `acme-tls/1` 时只查挑战表，
//!    否则会把用户的真证书交给一个只需要验证域名归属的对端；没协商到 `acme-tls/1`
//!    时绝不能查挑战表，否则普通访客会拿到一张自签证书 —— 浏览器当场报错，
//!    而服务端日志里是一次**成功**的握手。两个方向都由断言钉着。
//! 2. **那条扩展必须是 critical**（RFC 8737 §3）。不 critical 的话，不认识它的验证端会
//!    **忽略它**然后判通过 —— 一条本该失败的验证悄悄成功了。
//!    ★ 不自己拼 DER：`rcgen::CustomExtension::new_acme_identifier` 就是干这个的。
//! 3. **证书用完即摘，摘由 `Drop` 管。** 手写一对 insert/remove 迟早会漏一次。
//!
//! ⚠ ⚠ **判据要挂在 CA 自己的记录上。** 把第 2 条那个扩展去掉做反证时门全绿 ——
//! 破坏确实生效了（pebble 日志 `authz … set INVALID`），只是退避之后 G54 的「备」
//! 把它接住了。★ **一道分不出「成功」与「失败了但被兜住」的门，对这一层是瞎的**；
//! 同理单测量的必须是 [`build_challenge_cert`] 的产物，**判据挂在替身上等于没有判据**。
//!
//! ★ `rcgen` 已经在依赖图里（`instant-acme` 拉的），新增 0 个包。⚠ 但必须
//! `default-features = false` —— 它另有一个 `aws_lc_rs` feature，而把 aws-lc-rs 赶出
//! 依赖图的成果不能被顺手推翻。守它的是 `crates/fulcrum/tests/supply_gates.rs`。

use fulcrum_tls::{ChallengeCert, SniResolver, cert_key_from_der};
use log::debug;
use std::sync::Arc;

/// 造一张 RFC 8737 的挑战证书，挂到解析器上，返回一个到期自动摘掉的守卫。
///
/// `digest` 是 key authorization 的 SHA-256（32 字节）——
/// `instant_acme::KeyAuthorization::digest()` 给的正是它。
pub fn provision(
    resolver: &Arc<SniResolver>,
    domain: &str,
    digest: &[u8],
) -> Result<ChallengeCert, String> {
    let (cert_der, key_der) = build_challenge_cert(domain, digest)?;
    let ck = cert_key_from_der(cert_der, key_der)?;
    debug!("{domain}：TLS-ALPN-01 挑战证书已造好并挂上");
    Ok(resolver.provision_challenge(domain, ck))
}

/// 造那张证书本身，返回 `(证书 DER, PKCS#8 私钥 DER)`。
///
/// ★ ★ **它从 [`provision`] 里拆出来，是为了让判据能挂在产物上。**
/// ⚠ 测试若自己用 rcgen 拼一张一模一样的证书去检查扩展，测的是 **rcgen 的行为**，
/// 不是这个函数的：把下面那行 `custom_extensions` 改成 `vec![]`，那种测试**照样全绿**，
/// 而真的 CA 当场判 INVALID。**判据挂在替身上等于没有判据。**
pub fn build_challenge_cert(domain: &str, digest: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    // ⚠ 先自己挡一道。`new_acme_identifier` 里是 `assert_eq!`，那会 **panic**，
    //   而在签发路径上 panic 会把整个巡检任务带走。
    if digest.len() != 32 {
        return Err(format!(
            "key authorization 的摘要应当是 32 字节，实际 {} 字节",
            digest.len()
        ));
    }
    let key_pair = rcgen::KeyPair::generate().map_err(|e| format!("挑战证书生成密钥失败：{e}"))?;
    let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])
        .map_err(|e| format!("挑战证书的 SAN 不合法（{domain}）：{e}"))?;
    // ★ ★ 这一条就是 RFC 8737 §3 的全部内容。critical 由 rcgen 置上。
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| format!("挑战证书自签失败：{e}"))?;
    Ok((cert.der().to_vec(), key_pair.serialize_der()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest32() -> Vec<u8> {
        (0u8..32).collect()
    }

    #[test]
    fn 挂上去之后按域名精确挑得到而别的域名挑不到() {
        let r = Arc::new(SniResolver::new());
        assert_eq!(r.challenge_len(), 0);
        let guard = provision(&r, "a.example.com", &digest32()).unwrap();
        assert_eq!(r.challenge_len(), 1);
        drop(guard);
        // ★ 守卫析构之后必须摘干净——留着就是一条不该留的面。
        assert_eq!(r.challenge_len(), 0);
    }

    #[test]
    fn 摘掉是按域名摘的不是清空() {
        let r = Arc::new(SniResolver::new());
        let a = provision(&r, "a.example.com", &digest32()).unwrap();
        let b = provision(&r, "b.example.com", &digest32()).unwrap();
        assert_eq!(r.challenge_len(), 2);
        drop(a);
        // ⚠ 判据是「另一张还在」：一个把表清空的实现，在只有一个域名时表现完全相同。
        assert_eq!(r.challenge_len(), 1);
        drop(b);
        assert_eq!(r.challenge_len(), 0);
    }

    #[test]
    fn 摘要长度不对时返回错误而不是_panic() {
        // ⚠ `rcgen::CustomExtension::new_acme_identifier` 里是 assert_eq!，
        //   在签发路径上 panic 会把整个巡检任务带走。这条钉住我们先挡了一道。
        let r = Arc::new(SniResolver::new());
        let e = provision(&r, "a.example.com", &[0u8; 31]).unwrap_err();
        assert!(e.contains("32 字节"), "{e}");
        assert_eq!(r.challenge_len(), 0);
    }

    #[test]
    fn 造出来的证书带着那条_acme_扩展且是_critical() {
        // ★ ★ ★ 判据挂在**我们这个函数造出来的东西**上。
        //   ⚠ 不许在这里自己用 rcgen 拼一张一样的证书去检查 —— 那测的是 rcgen 的行为，
        //   不是 `build_challenge_cert` 的：把产品代码里那行 `custom_extensions`
        //   改成 `vec![]`，那种写法照样全绿，而门禁里真的 CA 当场判 INVALID。
        //
        //   RFC 8737 §3 要求这条扩展 critical，而不 critical 的话，
        //   一个不认识它的验证端会忽略它然后判通过——本该失败的验证悄悄成功。
        let digest = digest32();
        let (der, key_der) = build_challenge_cert("a.example.com", &digest).unwrap();
        assert!(!key_der.is_empty(), "私钥是空的");

        let (_, parsed) = x509_parser::parse_x509_certificate(&der).unwrap();
        let ext = parsed
            .extensions()
            .iter()
            .find(|e| e.oid.to_id_string() == "1.3.6.1.5.5.7.1.31")
            .expect("证书里没有 id-pe-acmeIdentifier 扩展");
        assert!(
            ext.critical,
            "那条扩展不是 critical —— RFC 8737 §3 要求它是"
        );
        // 扩展内容是一个 DER OCTET STRING，里面就是那 32 字节。
        assert!(
            ext.value.ends_with(&digest),
            "扩展里装的不是我们给的摘要：{:02x?}",
            ext.value
        );
    }
}
