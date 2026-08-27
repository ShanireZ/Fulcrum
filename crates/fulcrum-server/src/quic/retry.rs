//! Retry（地址验证）的 token —— RFC 9000 §8.1，**G109 要求必须开的那一条**。
//!
//! # 为什么非开不可（两件事，一个机制）
//!
//! 1. **抗放大**：没有地址验证时，一个伪造源地址的 Initial 能让我们朝受害者发出
//!    数倍于它的握手数据。Retry 是 RFC 9000 §8 推荐的做法。
//! 2. ★ ★ **前缀判定的完整性**（G109）：开了 Retry 之后，除第一发 Initial 外
//!    **所有包的 DCID 都是我们决定的** —— 而 [`super::gen_id`] 的整条判定就建在
//!    「DCID 是我们选的」这个前提上。**一件事同时解决两个问题。**
//!
//! # token 的形状
//!
//! ```text
//! token = nonce(12) ‖ tag(16) ‖ 密文
//! 明文  = issued_at(8, BE) ‖ odcid_len(1) ‖ odcid(≤20)
//! AAD   = 客户端地址（族标记 1 ‖ IP 字节 ‖ 端口 2, BE）
//! ```
//!
//! ★ ★ **地址验证由 AEAD 的 tag 校验完成，不是由我们比较地址完成**：客户端地址进 **AAD**
//! 而不是明文 —— 换一个地址来用同一个 token，解密直接失败。
//! ⇒ **本模块一行密码学都不写**（常数时间比较在 BoringSSL 内部）。
//!
//! # ⚠ token 不跨代，而这与 G109 合得上
//!
//! [`RetryKey`] 每代各生成一把 ⇒ 新一代验不了老一代的 token，**而它永远不需要验**：
//! Retry 包里的 SCID 是老一代铸的（带老一代的 `gen_id` 前缀），客户端此后拿它当 DCID
//! ⇒ 那一发 Initial 会被前缀判定**转交回老一代**。
//! ★ 老一代已退出时退化成丢包，客户端超时后重新握手 —— 与协议预期一致。

use boring::symm::{Cipher, decrypt_aead, encrypt_aead};
use rand::Rng;
use std::net::{IpAddr, SocketAddr};

/// token 的有效期。
///
/// ⚠ 定这个数要在两头之间取：太短会让 RTT 大的客户端刚拿到就过期（真实用户被挡在门外），
/// 太长会拉长「同一个地址可以重放这张票」的窗口。
/// ★ 10 秒是一个 RTT + 客户端处理时间的宽裕上界；RFC 9000 没有规定具体值。
pub const TOKEN_TTL_SECS: u64 = 10;

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;
/// `issued_at(8) + odcid_len(1)`
const PLAIN_HEAD: usize = 9;

/// 拒绝的原因。
///
/// ⚠ ⚠ ★ **只给日志与判据用，绝不能影响回给对端的东西** —— 对端拿到的永远只是
/// 「这张票不认」。把原因泄漏出去等于给攻击者一台预言机。
///
/// ★ 它存在的理由是判据那一侧：断言「被拒了」**太宽** —— 一个把所有输入都拒掉的
/// 实现照样全绿。本仓库记过同形的一条（「一个更宽的断言会让一个坏夹具
/// 悄悄通过」），这里让每条用例都能钉住**该红的那个原因**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// 长度不够放下 nonce + tag + 至少一个字节的密文。
    TooShort,
    /// 解密或 tag 校验失败 —— 换了地址、改了字节、或者根本不是我们签发的。
    ///
    /// ⚠ **这三种情形在这里是同一格，而且必须是同一格**：分得开就等于告诉对端
    /// 「你只是地址不对」还是「你这张票是伪造的」。
    BadSeal,
    /// 解开了，但里面的结构不对（长度字段与实际对不上）。
    Malformed,
    /// 解开了、结构也对，但过期了。
    Expired,
}

/// 一代进程的 Retry 签发密钥。**进程启动时生成一次。**
pub struct RetryKey([u8; KEY_LEN]);

impl RetryKey {
    pub fn random() -> Self {
        let mut k = [0u8; KEY_LEN];
        rand::rng().fill_bytes(&mut k);
        RetryKey(k)
    }

    /// 签发一张票：把 `odcid` 与签发时刻封进去，客户端地址进 AAD。
    ///
    /// `odcid` 是客户端**第一发 Initial 里的 DCID** —— RFC 9000 §7.3 要求服务端
    /// 之后把它放进 `original_destination_connection_id` 传输参数，
    /// 所以它必须能从 token 里原样取回来。
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
        // 上面刚判过 ≤ MAX_CONN_ID_LEN（20），转 u8 不会截断。
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

    /// 验一张票，通过就把 `odcid` 还回来。
    ///
    /// ⚠ **`peer` 必须是这个数据报的真实来源地址**，不能是包里写的任何东西 ——
    /// 整条地址验证的意义就在这一句上。
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
        // ⚠ 多余的尾巴也算畸形：一张我们自己签发的票不会有尾巴，
        //   而「宽容地忽略尾巴」会让同一个 odcid 有无数种合法编码。
        if plain.len() != PLAIN_HEAD + n {
            return Err(Reject::Malformed);
        }

        // ★ 用 `saturating_sub`：机器的钟往回跳时（NTP 校时）不能让 token 变成永久有效，
        //   也不能 panic。now < issued ⇒ 差值 0 ⇒ 判成「刚签发」，这是安全的一侧。
        if now_secs.saturating_sub(issued) > TOKEN_TTL_SECS {
            return Err(Reject::Expired);
        }
        Ok(odcid.to_vec())
    }
}

/// 客户端地址的规范编码 —— 进 AEAD 的 AAD。
///
/// 需要的性质只有一条：**不同的地址必须编出不同的字节串**（否则两个地址共用一张票）。
///
/// ⚠ ⚠ ★ **那个族标记不是承重的。** 去掉它做注入，十几条判据一条都不会红：
/// v4 编出 6 字节、v6 编出 18 字节，**长度本来就不同**，而 AAD 在 AEAD 里是长度确定的
/// ⇒ 根本没有那个碰撞要防。
/// ⇒ 标记留着（自描述、将来多一种地址形态时不用改口径），
/// **但它的理由改成「显式而已」，不许再写成一条安全论证**。
/// ★ 可带走的一条：**一个防御措施配一句听起来很对的理由，比没有理由更难查** ——
/// 而查出来的办法只有一个：**把它去掉，看有没有东西变红**。
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

    /// ★★★ 这一条就是整个模块存在的理由：**换个地址来用同一张票必须失败**。
    #[test]
    fn 换一个地址用同一张票必须被拒() {
        let k = RetryKey::random();
        let t = k
            .mint(&peer("192.0.2.10:44300"), b"abcdefgh", NOW)
            .expect("签发");
        // 换 IP
        assert_eq!(
            k.validate(&peer("192.0.2.11:44300"), &t, NOW),
            Err(Reject::BadSeal)
        );
        // ★ 只换端口也必须失败 —— 端口在 AAD 里
        assert_eq!(
            k.validate(&peer("192.0.2.10:44301"), &t, NOW),
            Err(Reject::BadSeal)
        );
    }

    /// AAD 要的性质只有一条：**不同地址编出不同字节串**。
    ///
    /// ⚠ ★ **名字只声称它真的在验的那一条。** 叫它「v4 与 v6 不会因为编码相近而
    /// 互相通过」是假的 —— 把 `aad_of` 里的族标记去掉做注入，它照样是绿的。
    /// ★ **一条判据的名字比它的断言宽，与它根本不存在的差别，只在出事那天才看得出来。**
    #[test]
    fn 不同地址编出的_aad_两两不同() {
        let addrs = [
            "192.0.2.10:443",
            "192.0.2.10:444",          // 只差端口
            "192.0.2.11:443",          // 只差最后一个八位组
            "[::ffff:192.0.2.10]:443", // v4-mapped 的同一个地址
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

    /// 而端到端那一半仍然要验：AAD 不同 ⇒ 票真的换不过去。
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
        // 边界内
        assert!(k.validate(&p, &t, NOW + TOKEN_TTL_SECS).is_ok());
        // 越过边界
        assert_eq!(
            k.validate(&p, &t, NOW + TOKEN_TTL_SECS + 1),
            Err(Reject::Expired)
        );
    }

    /// ★ 钟往回跳（NTP 校时）不能让票变成永久有效，也不能 panic。
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
        // 正好等于 nonce+tag、一个字节密文都没有 —— 也要走 TooShort 那一格
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

    /// ★ 两个边界：0 长 CID 是合法的（客户端可以不要 CID），20 是 RFC 的上限。
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

    /// 拿同一把钥匙封一段**任意明文**，用来构造 `mint` 永远不会产出的畸形票。
    ///
    /// ★ 没有它的话 [`Reject::Malformed`] 那三条路径**一条判据都没有** ——
    /// 而「一个从没红过的门与不存在的门无法区分」。
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

        // ① 明文短于头部
        let t = seal_raw(&k, &p, &[0u8; PLAIN_HEAD - 1]);
        assert_eq!(k.validate(&p, &t, NOW), Err(Reject::Malformed));

        // ② 长度字段说有 8 字节 odcid，实际只给了 3
        let mut plain = NOW.to_be_bytes().to_vec();
        plain.push(8);
        plain.extend_from_slice(&[1, 2, 3]);
        let t = seal_raw(&k, &p, &plain);
        assert_eq!(k.validate(&p, &t, NOW), Err(Reject::Malformed));

        // ③ ★ 多余的尾巴 —— 「宽容地忽略尾巴」会让同一个 odcid 有无数种合法编码
        let mut plain = NOW.to_be_bytes().to_vec();
        plain.push(2);
        plain.extend_from_slice(&[1, 2, /* 尾巴 */ 0xFF]);
        let t = seal_raw(&k, &p, &plain);
        assert_eq!(k.validate(&p, &t, NOW), Err(Reject::Malformed));
    }

    /// ★ 证明 nonce 真的是随机的 —— 一个把 nonce 写死的实现，上面每一条都照样全绿，
    /// 而 AES-GCM 在同一把钥匙下重用 nonce 会**直接泄漏明文异或**。
    #[test]
    fn 同样的输入两次签出来的票不同() {
        let k = RetryKey::random();
        let p = peer("192.0.2.10:44300");
        let (a, b) = (
            k.mint(&p, b"abcdefgh", NOW).expect("签发"),
            k.mint(&p, b"abcdefgh", NOW).expect("签发"),
        );
        assert_ne!(a, b, "两次签出同一张票 —— nonce 多半没接上随机源");
        // 而两张都必须验得过。
        assert!(k.validate(&p, &a, NOW).is_ok());
        assert!(k.validate(&p, &b, NOW).is_ok());
    }
}
