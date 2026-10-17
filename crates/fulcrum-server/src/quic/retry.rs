//! Retry（位址驗證）的 token —— RFC 9000 §8.1，**G109 要求必須開的那一條**。
//!
//! # 為什麼非開不可（兩件事，一個機制）
//!
//! 1. **抗放大**：沒有位址驗證時，一個偽造源位址的 Initial 能讓我們朝受害者發出
//!    數倍於它的握手資料。Retry 是 RFC 9000 §8 推薦的做法。
//! 2. ★ ★ **前綴判定的完整性**（G109）：開了 Retry 之後，除第一發 Initial 外
//!    **所有包的 DCID 都是我們決定的** —— 而 [`super::gen_id`] 的整條判定就建在
//!    「DCID 是我們選的」這個前提上。**一件事同時解決兩個問題。**
//!
//! # token 的形狀
//!
//! ```text
//! token = nonce(12) ‖ tag(16) ‖ 密文
//! 明文  = issued_at(8, BE) ‖ odcid_len(1) ‖ odcid(≤20)
//! AAD   = 客户端地址（族标记 1 ‖ IP 字节 ‖ 端口 2, BE）
//! ```
//!
//! ★ ★ **位址驗證由 AEAD 的 tag 檢核完成，不是由我們比較位址完成**：用戶端位址進 **AAD**
//! 而不是明文 —— 換一個位址來用同一個 token，解密直接失敗。
//! ⇒ **本模組一行密碼學都不寫**（常數時間比較在 BoringSSL 內部）。
//!
//! # ⚠ token 不跨代，而這與 G109 合得上
//!
//! [`RetryKey`] 每代各生成一把 ⇒ 新一代驗不了老一代的 token，**而它永遠不需要驗**：
//! Retry 包裡的 SCID 是老一代鑄的（帶老一代的 `gen_id` 前綴），用戶端此後拿它當 DCID
//! ⇒ 那一發 Initial 會被前綴判定**轉交回老一代**。
//! ★ 老一代已退出時退化成丟包，用戶端超時後重新握手 —— 與協定預期一致。

use boring::symm::{Cipher, decrypt_aead, encrypt_aead};
use rand::Rng;
use std::net::{IpAddr, SocketAddr};

/// token 的有效期。
///
/// ⚠ 定這個數要在兩頭之間取：太短會讓 RTT 大的用戶端剛拿到就過期（真實使用者被擋在門外），
/// 太長會拉長「同一個位址可以重放這張票」的視窗。
/// ★ 10 秒是一個 RTT + 用戶端處理時間的寬裕上界；RFC 9000 沒有規定具體值。
pub const TOKEN_TTL_SECS: u64 = 10;

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;
/// `issued_at(8) + odcid_len(1)`
const PLAIN_HEAD: usize = 9;

/// 拒絕的原因。
///
/// ⚠ ⚠ ★ **只給紀錄與判據用，絕不能影響回給對端的東西** —— 對端拿到的永遠只是
/// 「這張票不認」。把原因洩漏出去等於給攻擊者一臺預言機。
///
/// ★ 它存在的理由是判據那一側：斷言「被拒了」**太寬** —— 一個把所有輸入都拒掉的
/// 實作照樣全綠。本儲存庫記過同形的一條（「一個更寬的斷言會讓一個壞夾具
/// 悄悄通過」），這裡讓每條用例都能釘住**該紅的那個原因**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// 長度不夠放下 nonce + tag + 至少一個位元組的密文。
    TooShort,
    /// 解密或 tag 檢核失敗 —— 換了位址、改了位元組、或者根本不是我們簽發的。
    ///
    /// ⚠ **這三種情形在這裡是同一格，而且必須是同一格**：分得開就等於告訴對端
    /// 「你只是位址不對」還是「你這張票是偽造的」。
    BadSeal,
    /// 解開了，但裡面的結構不對（長度欄位與實際對不上）。
    Malformed,
    /// 解開了、結構也對，但過期了。
    Expired,
}

/// 一代處理程序的 Retry 簽發金鑰。**處理程序啟動時生成一次。**
pub struct RetryKey([u8; KEY_LEN]);

impl RetryKey {
    pub fn random() -> Self {
        let mut k = [0u8; KEY_LEN];
        rand::rng().fill_bytes(&mut k);
        RetryKey(k)
    }

    /// 簽發一張票：把 `odcid` 與簽發時刻封進去，用戶端位址進 AAD。
    ///
    /// `odcid` 是用戶端**第一發 Initial 裡的 DCID** —— RFC 9000 §7.3 要求伺服器端
    /// 之後把它放進 `original_destination_connection_id` 傳輸參數，
    /// 所以它必須能從 token 裡原樣取回來。
    pub fn mint(&self, peer: &SocketAddr, odcid: &[u8], now_secs: u64) -> Result<Vec<u8>, String> {
        if odcid.len() > quiche::MAX_CONN_ID_LEN {
            return Err(format!(
                "odcid 长 {} 字节，超出 RFC 9000 的上限 {}",
                odcid.len(),
                quiche::MAX_CONN_ID_LEN
            ));
        }
        let mut plain = Vec::with_capacity(PLAIN_HEAD + odcid.len());
        plain.extend_from_slice(&now_secs.to_be_bytes());
        // 上面剛判過 ≤ MAX_CONN_ID_LEN（20），轉 u8 不會截斷。
        plain.push(odcid.len() as u8);
        plain.extend_from_slice(odcid);

        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let mut tag = [0u8; TAG_LEN];
        let sealed = encrypt_aead(
            Cipher::aes_256_gcm(),
            &self.0,
            Some(&nonce),
            &aad_of(peer),
            &plain,
            &mut tag,
        )
        .map_err(|e| format!("Retry token 封装失败：{e}"))?;

        let mut token = Vec::with_capacity(NONCE_LEN + TAG_LEN + sealed.len());
        token.extend_from_slice(&nonce);
        token.extend_from_slice(&tag);
        token.extend_from_slice(&sealed);
        Ok(token)
    }

    /// 驗一張票，通過就把 `odcid` 還回來。
    ///
    /// ⚠ **`peer` 必須是這個資料報的真實來源位址**，不能是包裡寫的任何東西 ——
    /// 整條位址驗證的意義就在這一句上。
    pub fn validate(
        &self,
        peer: &SocketAddr,
        token: &[u8],
        now_secs: u64,
    ) -> Result<Vec<u8>, Reject> {
        if token.len() <= NONCE_LEN + TAG_LEN {
            return Err(Reject::TooShort);
        }
        let (nonce, rest) = token.split_at(NONCE_LEN);
        let (tag, sealed) = rest.split_at(TAG_LEN);

        let plain = decrypt_aead(
            Cipher::aes_256_gcm(),
            &self.0,
            Some(nonce),
            &aad_of(peer),
            sealed,
            tag,
        )
        .map_err(|_| Reject::BadSeal)?;

        if plain.len() < PLAIN_HEAD {
            return Err(Reject::Malformed);
        }
        let issued: [u8; 8] = plain[..8].try_into().map_err(|_| Reject::Malformed)?;
        let issued = u64::from_be_bytes(issued);
        let n = usize::from(plain[8]);
        let odcid = plain
            .get(PLAIN_HEAD..PLAIN_HEAD + n)
            .ok_or(Reject::Malformed)?;
        // ⚠ 多餘的尾巴也算畸形：一張我們自己簽發的票不會有尾巴，
        //   而「寬容地忽略尾巴」會讓同一個 odcid 有無數種合法編碼。
        if plain.len() != PLAIN_HEAD + n {
            return Err(Reject::Malformed);
        }

        // ★ 用 `saturating_sub`：機器的鐘往回跳時（NTP 校時）不能讓 token 變成永久有效，
        //   也不能 panic。now < issued ⇒ 差值 0 ⇒ 判成「剛簽發」，這是安全的一側。
        if now_secs.saturating_sub(issued) > TOKEN_TTL_SECS {
            return Err(Reject::Expired);
        }
        Ok(odcid.to_vec())
    }
}

/// 用戶端位址的規範編碼 —— 進 AEAD 的 AAD。
///
/// 需要的性質只有一條：**不同的位址必須編出不同的位元組串**（否則兩個位址共用一張票）。
///
/// ⚠ ⚠ ★ **那個族標記不是承重的。** 去掉它做注入，十幾條判據一條都不會紅：
/// v4 編出 6 位元組、v6 編出 18 位元組，**長度本來就不同**，而 AAD 在 AEAD 裡是長度確定的
/// ⇒ 根本沒有那個碰撞要防。
/// ⇒ 標記留著（自描述、將來多一種位址形態時不用改口徑），
/// **但它的理由改成「顯式而已」，不許再寫成一條安全論證**。
/// ★ 可帶走的一條：**一個防禦措施配一句聽起來很對的理由，比沒有理由更難查** ——
/// 而查出來的辦法只有一個：**把它去掉，看有沒有東西變紅**。
fn aad_of(peer: &SocketAddr) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + 16 + 2);
    match peer.ip() {
        IpAddr::V4(a) => {
            v.push(4);
            v.extend_from_slice(&a.octets());
        }
        IpAddr::V6(a) => {
            v.push(6);
            v.extend_from_slice(&a.octets());
        }
    }
    v.extend_from_slice(&peer.port().to_be_bytes());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(s: &str) -> SocketAddr {
        s.parse().expect("测试地址常量")
    }

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn 签发再验证能把_odcid_原样取回来() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        let odcid = b"\x01\x02\x03\x04\x05\x06\x07\x08";
        let t = k.mint(&p, odcid, NOW).expect("签发");
        assert_eq!(k.validate(&p, &t, NOW).expect("验证"), odcid.to_vec());
    }

    /// ★★★ 這一條就是整個模組存在的理由：**換個位址來用同一張票必須失敗**。
    #[test]
    fn 换一个地址用同一张票必须被拒() {
        let k = RetryKey::random();
        let t = k
            .mint(&peer("192.0.2.10:44300"), b"abcdefgh", NOW)
            .expect("签发");
        // 換 IP
        assert_eq!(
            k.validate(&peer("192.0.2.11:44300"), &t, NOW),
            Err(Reject::BadSeal)
        );
        // ★ 只換連接埠也必須失敗 —— 連接埠在 AAD 裡
        assert_eq!(
            k.validate(&peer("192.0.2.10:44301"), &t, NOW),
            Err(Reject::BadSeal)
        );
    }

    /// AAD 要的性質只有一條：**不同位址編出不同位元組串**。
    ///
    /// ⚠ ★ **名字只聲稱它真的在驗的那一條。** 叫它「v4 與 v6 不會因為編碼相近而
    /// 互相通過」是假的 —— 把 `aad_of` 裡的族標記去掉做注入，它照樣是綠的。
    /// ★ **一條判據的名字比它的斷言寬，與它根本不存在的差別，只在出事那天才看得出來。**
    #[test]
    fn 不同地址编出的_aad_两两不同() {
        let addrs = [
            "192.0.2.10:443",
            "192.0.2.10:444",          // 只差連接埠
            "192.0.2.11:443",          // 只差最後一個八位組
            "[::ffff:192.0.2.10]:443", // v4-mapped 的同一個位址
            "[2001:db8::1]:443",
            "[2001:db8::1]:444",
        ];
        let aads: Vec<Vec<u8>> = addrs.iter().map(|s| aad_of(&peer(s))).collect();
        for i in 0..aads.len() {
            for j in (i + 1)..aads.len() {
                assert_ne!(
                    aads[i], aads[j],
                    "{} 与 {} 编出了同一串 AAD",
                    addrs[i], addrs[j]
                );
            }
        }
    }

    /// 而端到端那一半仍然要驗：AAD 不同 ⇒ 票真的換不過去。
    #[test]
    fn v4_的票在_v4_mapped_的_v6_地址上用不了() {
        let k = RetryKey::random();
        let t = k
            .mint(&peer("192.0.2.10:443"), b"abcdefgh", NOW)
            .expect("签发");
        assert_eq!(
            k.validate(&peer("[::ffff:192.0.2.10]:443"), &t, NOW),
            Err(Reject::BadSeal)
        );
    }

    #[test]
    fn 改动任何一个字节都必须被拒() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        let t = k.mint(&p, b"abcdefgh", NOW).expect("签发");
        for i in 0..t.len() {
            let mut bad = t.clone();
            bad[i] ^= 0x01;
            assert_eq!(
                k.validate(&p, &bad, NOW),
                Err(Reject::BadSeal),
                "第 {i} 个字节被改动之后仍然通过了"
            );
        }
    }

    #[test]
    fn 另一把钥匙签的票验不过() {
        let (a, b) = (RetryKey::random(), RetryKey::random());
        let p = peer("192.0.2.10:44300");
        let t = a.mint(&p, b"abcdefgh", NOW).expect("签发");
        assert_eq!(b.validate(&p, &t, NOW), Err(Reject::BadSeal));
    }

    #[test]
    fn 过期的票被拒而边界上那一秒仍然有效() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        let t = k.mint(&p, b"abcdefgh", NOW).expect("签发");
        // 邊界內
        assert!(k.validate(&p, &t, NOW + TOKEN_TTL_SECS).is_ok());
        // 越過邊界
        assert_eq!(
            k.validate(&p, &t, NOW + TOKEN_TTL_SECS + 1),
            Err(Reject::Expired)
        );
    }

    /// ★ 鍾往回跳（NTP 校時）不能讓票變成永久有效，也不能 panic。
    #[test]
    fn 钟往回跳时票仍然有效而不是恒久有效() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        let t = k.mint(&p, b"abcdefgh", NOW).expect("签发");
        assert!(k.validate(&p, &t, NOW - 5).is_ok(), "钟回跳时不该拒真票");
        assert!(k.validate(&p, &t, 0).is_ok(), "极端回跳也只是判成刚签发");
    }

    #[test]
    fn 垃圾输入被拒而不是_panic() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        assert_eq!(k.validate(&p, b"", NOW), Err(Reject::TooShort));
        // 正好等於 nonce+tag、一個位元組密文都沒有 —— 也要走 TooShort 那一格
        assert_eq!(
            k.validate(&p, &[0u8; NONCE_LEN + TAG_LEN], NOW),
            Err(Reject::TooShort)
        );
        assert_eq!(
            k.validate(&p, &[0u8; NONCE_LEN + TAG_LEN + 1], NOW),
            Err(Reject::BadSeal)
        );
        assert_eq!(k.validate(&p, &[0xABu8; 200], NOW), Err(Reject::BadSeal));
    }

    /// ★ 兩個邊界：0 長 CID 是合法的（用戶端可以不要 CID），20 是 RFC 的上限。
    #[test]
    fn odcid_的两个长度边界都能往返() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        for n in [0usize, quiche::MAX_CONN_ID_LEN] {
            let odcid = vec![0x5Au8; n];
            let t = k.mint(&p, &odcid, NOW).expect("签发");
            assert_eq!(k.validate(&p, &t, NOW).expect("验证"), odcid, "长度 {n}");
        }
    }

    #[test]
    fn 超长_odcid_签不出来() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        assert!(
            k.mint(&p, &[0u8; quiche::MAX_CONN_ID_LEN + 1], NOW)
                .is_err()
        );
    }

    /// 拿同一把鑰匙封一段**任意明文**，用來建構 `mint` 永遠不會產出的畸形票。
    ///
    /// ★ 沒有它的話 [`Reject::Malformed`] 那三條路徑**一條判據都沒有** ——
    /// 而「一個從沒紅過的門與不存在的門無法區分」。
    fn seal_raw(k: &RetryKey, p: &SocketAddr, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let mut tag = [0u8; TAG_LEN];
        let sealed = encrypt_aead(
            Cipher::aes_256_gcm(),
            &k.0,
            Some(&nonce),
            &aad_of(p),
            plain,
            &mut tag,
        )
        .expect("测试封装");
        let mut t = Vec::new();
        t.extend_from_slice(&nonce);
        t.extend_from_slice(&tag);
        t.extend_from_slice(&sealed);
        t
    }

    #[test]
    fn 解得开但结构不对的票判成_malformed_而不是_badseal() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");

        // ① 明文短於頭部
        let t = seal_raw(&k, &p, &[0u8; PLAIN_HEAD - 1]);
        assert_eq!(k.validate(&p, &t, NOW), Err(Reject::Malformed));

        // ② 長度欄位說有 8 位元組 odcid，實際只給了 3
        let mut plain = NOW.to_be_bytes().to_vec();
        plain.push(8);
        plain.extend_from_slice(&[1, 2, 3]);
        let t = seal_raw(&k, &p, &plain);
        assert_eq!(k.validate(&p, &t, NOW), Err(Reject::Malformed));

        // ③ ★ 多餘的尾巴 —— 「寬容地忽略尾巴」會讓同一個 odcid 有無數種合法編碼
        let mut plain = NOW.to_be_bytes().to_vec();
        plain.push(2);
        plain.extend_from_slice(&[1, 2, /* 尾巴 */ 0xFF]);
        let t = seal_raw(&k, &p, &plain);
        assert_eq!(k.validate(&p, &t, NOW), Err(Reject::Malformed));
    }

    /// ★ 證明 nonce 真的是隨機的 —— 一個把 nonce 寫死的實作，上面每一條都照樣全綠，
    /// 而 AES-GCM 在同一把鑰匙下重用 nonce 會**直接洩漏明文異或**。
    #[test]
    fn 同样的输入两次签出来的票不同() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        let (a, b) = (
            k.mint(&p, b"abcdefgh", NOW).expect("签发"),
            k.mint(&p, b"abcdefgh", NOW).expect("签发"),
        );
        assert_ne!(a, b, "两次签出同一张票 —— nonce 多半没接上随机源");
        // 而兩張都必須驗得過。
        assert!(k.validate(&p, &a, NOW).is_ok());
        assert!(k.validate(&p, &b, NOW).is_ok());
    }
}
