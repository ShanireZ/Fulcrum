//! TLS-ALPN-01（G54 的**主**）—— RFC 8737。
//!
//! # 它是什麼
//!
//! CA 向 `<域名>:443` 發起一次 TLS 握手，ALPN 裡**只**提供 `acme-tls/1`。
//! 伺服器必須協商到這個協定，並回一張**自簽**證書，其中帶一條**critical** 的
//! `id-pe-acmeIdentifier` 擴充（OID `1.3.6.1.5.5.7.1.31`），內容是
//! key authorization 的 SHA-256。CA 看完這張證書就斷開——**這次握手不傳任何資料**。
//!
//! # 為什麼 G54 把它定成「主」
//!
//! | | TLS-ALPN-01 | HTTP-01 |
//! |---|---|---|
//! | 連接埠 | 只要 443 | **要 80** |
//! | 佔用路由 | **零** —— 整件事在握手裡完成 | 要佔 `/.well-known/acme-challenge/` |
//! | 使用者設定能不能擋住它 | 擋不住 | ★ 能（一份 `respond 403` 就夠）|
//!
//! ★ 第一行對 `.cn` 網域尤其要緊：大陸機房 80 連接埠常年受限，而 443 是通的。
//!
//! # ⚠ 三條不能想當然的地方
//!
//! 1. **挑戰證書與真證書必須是兩張互不相通的表。** 協商到 `acme-tls/1` 時只查挑戰表，
//!    否則會把使用者的真證書交給一個只需要驗證網域歸屬的對端；沒協商到 `acme-tls/1`
//!    時絕不能查挑戰表，否則普通訪客會拿到一張自簽證書 —— 瀏覽器當場回報錯誤，
//!    而伺服器端紀錄裡是一次**成功**的握手。兩個方向都由斷言釘著。
//! 2. **那條擴充必須是 critical**（RFC 8737 §3）。不 critical 的話，不認識它的驗證端會
//!    **忽略它**然後判通過 —— 一條本該失敗的驗證悄悄成功了。
//!    ★ 不自己拼 DER：`rcgen::CustomExtension::new_acme_identifier` 就是幹這個的。
//! 3. **證書用完即摘，摘由 `Drop` 管。** 手寫一對 insert/remove 遲早會漏一次。
//!
//! ⚠ ⚠ **判據要掛在 CA 自己的記錄上。** 把第 2 條那個擴充去掉做反證時門全綠 ——
//! 破壞確實生效了（pebble 紀錄 `authz … set INVALID`），只是退避之後 G54 的「備」
//! 把它接住了。★ **一道分不出「成功」與「失敗了但被兜住」的門，對這一層是瞎的**；
//! 同理單測量的必須是 [`build_challenge_cert`] 的產物，**判據掛在替身上等於沒有判據**。
//!
//! ★ `rcgen` 已經在相依圖裡（`instant-acme` 拉的），新增 0 個包。⚠ 但必須
//! `default-features = false` —— 它另有一個 `aws_lc_rs` feature，而把 aws-lc-rs 趕出
//! 相依圖的成果不能被順手推翻。守它的是 `crates/fulcrum/tests/supply_gates.rs`。

use fulcrum_tls::{ChallengeCert, SniResolver, cert_key_from_der};
use log::debug;
use std::sync::Arc;

/// 造一張 RFC 8737 的挑戰證書，掛到解析器上，回傳一個到期自動摘掉的守衛。
///
/// `digest` 是 key authorization 的 SHA-256（32 位元組）——
/// `instant_acme::KeyAuthorization::digest()` 給的正是它。
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

/// 造那張證書本身，回傳 `(证书 DER, PKCS#8 私钥 DER)`。
///
/// ★ ★ **它從 [`provision`] 裡拆出來，是為了讓判據能掛在產物上。**
/// ⚠ 測試若自己用 rcgen 拼一張一模一樣的證書去檢查擴充，測的是 **rcgen 的行為**，
/// 不是這個函式的：把下面那行 `custom_extensions` 改成 `vec![]`，那種測試**照樣全綠**，
/// 而真的 CA 當場判 INVALID。**判據掛在替身上等於沒有判據。**
pub fn build_challenge_cert(domain: &str, digest: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    // ⚠ 先自己擋一道。`new_acme_identifier` 裡是 `assert_eq!`，那會 **panic**，
    //   而在簽發路徑上 panic 會把整個巡檢任務帶走。
    if digest.len() != 32 {
        return Err(format!(
            "key authorization 的摘要应当是 32 字节，实际 {} 字节",
            digest.len()
        ));
    }
    let key_pair = rcgen::KeyPair::generate().map_err(|e| format!("挑战证书生成密钥失败：{e}"))?;
    let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])
        .map_err(|e| format!("挑战证书的 SAN 不合法（{domain}）：{e}"))?;
    // ★ ★ 這一條就是 RFC 8737 §3 的全部內容。critical 由 rcgen 置上。
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
        // ★ 守衛析構之後必須摘乾淨——留著就是一條不該留的面。
        assert_eq!(r.challenge_len(), 0);
    }

    #[test]
    fn 摘掉是按域名摘的不是清空() {
        let r = Arc::new(SniResolver::new());
        let a = provision(&r, "a.example.com", &digest32()).unwrap();
        let b = provision(&r, "b.example.com", &digest32()).unwrap();
        assert_eq!(r.challenge_len(), 2);
        drop(a);
        // ⚠ 判據是「另一張還在」：一個把表清空的實作，在只有一個網域時表現完全相同。
        assert_eq!(r.challenge_len(), 1);
        drop(b);
        assert_eq!(r.challenge_len(), 0);
    }

    #[test]
    fn 摘要长度不对时返回错误而不是_panic() {
        // ⚠ `rcgen::CustomExtension::new_acme_identifier` 裡是 assert_eq!，
        //   在簽發路徑上 panic 會把整個巡檢任務帶走。這條釘住我們先擋了一道。
        let r = Arc::new(SniResolver::new());
        let e = provision(&r, "a.example.com", &[0u8; 31]).unwrap_err();
        assert!(e.contains("32 字节"), "{e}");
        assert_eq!(r.challenge_len(), 0);
    }

    #[test]
    fn 造出来的证书带着那条_acme_扩展且是_critical() {
        // ★ ★ ★ 判據掛在**我們這個函式造出來的東西**上。
        //   ⚠ 不許在這裡自己用 rcgen 拼一張一樣的證書去檢查 —— 那測的是 rcgen 的行為，
        //   不是 `build_challenge_cert` 的：把產品程式碼裡那行 `custom_extensions`
        //   改成 `vec![]`，那種寫法照樣全綠，而門禁裡真的 CA 當場判 INVALID。
        //
        //   RFC 8737 §3 要求這條擴充 critical，而不 critical 的話，
        //   一個不認識它的驗證端會忽略它然後判通過——本該失敗的驗證悄悄成功。
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
        // 擴充內容是一個 DER OCTET STRING，裡面就是那 32 位元組。
        assert!(
            ext.value.ends_with(&digest),
            "扩展里装的不是我们给的摘要：{:02x?}",
            ext.value
        );
    }
}
