//! 代标识（`gen_id`）与连接 ID 的形状 —— **G109 ① 的落点**（§10）。
//!
//! # 为什么这一块必须在批 J 就做完
//!
//! 换代时两代进程持有**同一个** UDP socket（这正是 fd 移交成功的表现），两边都在
//! `recv_from`，内核把数据报**任意**分给其中一代；而 QUIC 的连接状态（密钥、流、
//! 拥塞窗口）只在某一代的内存里。⇒ 必须能从**数据报本身**看出它属于哪一代。
//!
//! G109 取的办法是把代标识编进**我们自选的连接 ID 前缀**里。
//! ⚠ ⚠ ★ **CID 的形状是对外可见的**：客户端会把我们给的 SCID 原样当作后续包的 DCID。
//! 等批 K 再改前缀，会让批 J 期间发出去的连接在换代时**全部认不出来** ——
//! 所以「转交」本身是批 K，而**前缀必须是批 J**。
//!
//! # 这里只回答「这个数据报属于谁」
//!
//! 真正把它送过去的是 [`super::relay`]（批 K）。
//! ⚠ **「前缀已经带上了」读起来很像「转交已经能用了」** —— 那是两件事，
//! 而这个模块只做前一件。

use rand::Rng;

/// 代标识在 CID 前缀里占的字节数（G109 ①：8 字节）。
pub const GEN_ID_LEN: usize = 8;

/// 服务端自选的连接 ID 总长度。
///
/// ★ 前 [`GEN_ID_LEN`] 字节是代标识，其余是随机尾巴。
/// ⚠ RFC 9000 §17.2 允许 CID 最长 20 字节；取 16 是为了给尾巴留够 8 字节熵，
/// 同时让每个包的头部少背 4 个字节。
pub const SCID_LEN: usize = 16;

const _: () = assert!(SCID_LEN > GEN_ID_LEN, "CID 的随机尾巴不能是空的");
const _: () = assert!(
    SCID_LEN <= quiche::MAX_CONN_ID_LEN,
    "CID 超出 RFC 9000 的上限"
);

/// 一代进程的身份。进程启动时随机生成一次，此后不变。
///
/// ⚠ **它不是秘密，但必须不可预测**：能预测下一代的 `gen_id` 就能让我们把数据报
/// 转交给一个不存在的地方。⇒ 走 CSPRNG（见 [`GenId::random`]）。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenId([u8; GEN_ID_LEN]);

impl GenId {
    /// 生成本代的标识。
    ///
    /// ⚠ 取 `rand::rng()`（`ThreadRng`，OS 播种的 ChaCha，`impl TryCryptoRng`），
    /// **不是** `SmallRng` —— 理由见本类型的文档注释。
    pub fn random() -> Self {
        let mut b = [0u8; GEN_ID_LEN];
        rand::rng().fill_bytes(&mut b);
        GenId(b)
    }

    /// 从固定字节构造。★ 只给单测与「从 DCID 前缀还原出对方是谁」用。
    pub fn from_bytes(b: [u8; GEN_ID_LEN]) -> Self {
        GenId(b)
    }

    pub fn as_bytes(&self) -> &[u8; GEN_ID_LEN] {
        &self.0
    }

    /// 十六进制形式 —— 转交通道的 socket 路径由它推导
    /// （`<run_dir>/quic-relay-<gen_id_hex>.sock`，G109 ②）。
    ///
    /// ★ ★ **路径就在 DCID 那 8 个字节里，所以两代之间不需要任何握手或协商。**
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(GEN_ID_LEN * 2);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// 铸一个本代的服务端连接 ID：`gen_id ‖ 随机`。
    pub fn mint_scid(&self) -> quiche::ConnectionId<'static> {
        let mut cid = [0u8; SCID_LEN];
        cid[..GEN_ID_LEN].copy_from_slice(&self.0);
        rand::rng().fill_bytes(&mut cid[GEN_ID_LEN..]);
        quiche::ConnectionId::from_ref(&cid).into_owned()
    }

    /// 这个 DCID 的前缀是不是本代？
    ///
    /// ⚠ **短于 [`GEN_ID_LEN`] 的 DCID 一律回 `false`** —— 它判不出来，
    /// 而「判不出来」绝不能被读成「是我的」。
    pub fn owns(&self, dcid: &[u8]) -> bool {
        prefix_of(dcid).is_some_and(|g| g == *self)
    }
}

impl std::fmt::Debug for GenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gen:{}", self.hex())
    }
}

/// 从一个 DCID 里抠出代标识前缀。**短于 [`GEN_ID_LEN`] 时回 `None`。**
pub fn prefix_of(dcid: &[u8]) -> Option<GenId> {
    let head: [u8; GEN_ID_LEN] = dcid.get(..GEN_ID_LEN)?.try_into().ok()?;
    Some(GenId(head))
}

/// 一个数据报该由谁处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// 新连接：客户端自选的 DCID，它还没有任何状态，落到哪一代都行 ⇒ 本代处理。
    NewConnection,
    /// DCID 是**我们**选的，且前缀就是本代。
    Local,
    /// DCID 是**我们**选的，但属于另一代 ⇒ 转交给它。
    /// 送出去的那一半在 [`super::relay`]。
    Relay(GenId),
    /// 判不出来（包头形状不对、DCID 太短、或服务端本就不该收到这种包）。
    ///
    /// ⚠ **它与 `NewConnection` 必须分开**：把判不出来的包当新连接处理，
    /// 等于让任何一个畸形数据报都能让我们建一份状态。
    Undecidable,
}

/// 判定一个数据报属于哪一代。
///
/// 收的是**原始类型**而不是 `quiche::Header`，理由很实际：`Header` 带私有字段，
/// 单测里构造不出来 —— 而一个构造不出输入的判定函数，就只能靠真流量去测，
/// 那样红了指不到具体哪条规则（与 `fulcrum-runtime` 拆出来的理由同源）。
///
/// # ★ ★ ★ Initial 那一格不是「一律本代」，而施工图上写的是「一律本代」
///
/// G109 那张表把 Initial 整行写成「客户端选的 DCID ⇒ 新连接 ⇒ 本代处理」。
/// **开了 Retry 之后这句话有一半不成立**：Retry 交换之后客户端的第二发 Initial
/// 带的 DCID 是**我们**在 Retry 包里给它的，属于发 Retry 的那一代。
/// 把它当新连接处理，等于让新一代去接一个老一代已经发过 Retry、
/// 并且正在等它回来的握手 —— **而 Retry 恰恰是 G109 自己要求必须开的**。
///
/// ⇒ 这里按 **token 在不在**分两格：
///
/// | 包 | token | 处置 |
/// |---|---|---|
/// | Initial | 无 | `NewConnection`（首发，本代回 Retry）|
/// | Initial | 有 | 前缀判定（Retry 之后，DCID 是我们给的）|
/// | Handshake / 0-RTT / Short | — | 前缀判定 |
/// | Retry / VersionNegotiation | — | `Undecidable`（服务端不该收到）|
///
/// ⚠ **0-RTT 的首发那一格是有意让它走「前缀判定 ⇒ 转交 ⇒ 丢弃」的**：
/// 开了 Retry 之后首发 0-RTT 本来就会被丢（客户端必须先完成 Retry 交换），
/// 让它落进一个不存在的 `gen_id` 从而被丢弃，与协议要求的结果一致，
/// 而**不需要为它单开一条路**。
pub fn ownership(me: &GenId, ty: quiche::Type, dcid: &[u8], has_token: bool) -> Ownership {
    use quiche::Type;
    match ty {
        Type::Initial if !has_token => Ownership::NewConnection,
        Type::Initial | Type::Handshake | Type::ZeroRTT | Type::Short => match prefix_of(dcid) {
            Some(g) if g == *me => Ownership::Local,
            Some(g) => Ownership::Relay(g),
            // DCID 短于 8 字节 —— 我们自选的 CID 永远是 SCID_LEN 长，所以这不是我们发的。
            None => Ownership::Undecidable,
        },
        // 服务端不会收到这两种；收到了说明对端在乱发，不给它任何处理路径。
        Type::Retry | Type::VersionNegotiation => Ownership::Undecidable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quiche::Type;

    fn g(n: u8) -> GenId {
        GenId::from_bytes([n; GEN_ID_LEN])
    }

    /// 一个前缀是 `who`、长度合法的 DCID。
    fn cid_of(who: &GenId) -> Vec<u8> {
        let mut v = who.as_bytes().to_vec();
        v.extend_from_slice(&[0xAB; SCID_LEN - GEN_ID_LEN]);
        v
    }

    #[test]
    fn 本代的短包判成_local() {
        let me = g(1);
        assert_eq!(
            ownership(&me, Type::Short, &cid_of(&me), false),
            Ownership::Local
        );
    }

    #[test]
    fn 别代的短包判成_relay_并且指向那一代() {
        let (me, other) = (g(1), g(2));
        assert_eq!(
            ownership(&me, Type::Short, &cid_of(&other), false),
            Ownership::Relay(other)
        );
    }

    /// ★ 命中与错过都要覆盖：上面两条就是同一个判定的两个方向。
    #[test]
    fn 握手包与_0rtt_走的是同一条前缀判定() {
        let (me, other) = (g(7), g(8));
        for ty in [Type::Handshake, Type::ZeroRTT] {
            assert_eq!(ownership(&me, ty, &cid_of(&me), false), Ownership::Local);
            assert_eq!(
                ownership(&me, ty, &cid_of(&other), false),
                Ownership::Relay(other)
            );
        }
    }

    #[test]
    fn 首发_initial_没有_token_一律本代() {
        let (me, other) = (g(1), g(2));
        // ★ 就算前缀恰好像是别代的，也仍然是新连接 —— 那 8 个字节是客户端随机选的。
        assert_eq!(
            ownership(&me, Type::Initial, &cid_of(&other), false),
            Ownership::NewConnection
        );
    }

    /// ★ ★ ★ 这一条就是施工图那张表漏掉的那一格。
    #[test]
    fn retry_之后的_initial_带_token_要走前缀判定() {
        let (me, other) = (g(1), g(2));
        assert_eq!(
            ownership(&me, Type::Initial, &cid_of(&me), true),
            Ownership::Local
        );
        assert_eq!(
            ownership(&me, Type::Initial, &cid_of(&other), true),
            Ownership::Relay(other),
            "带 token 的 Initial 是 Retry 之后的第二发，DCID 是我们给的 —— \
             把它当新连接会让新一代去接老一代正在等的那个握手"
        );
    }

    #[test]
    fn dcid_短于八字节判不出来而不是判成本代() {
        let me = g(0);
        // ⚠ `g(0)` 的字节全是 0，而短 DCID 补零之后看起来正好像它 ——
        //   这条用例挑 0 就是为了把「补零当成命中」这种写法钉死。
        for len in 0..GEN_ID_LEN {
            assert_eq!(
                ownership(&me, Type::Short, &vec![0u8; len], false),
                Ownership::Undecidable,
                "长度 {len} 的 DCID 判不出来，不能读成「是我的」"
            );
        }
    }

    #[test]
    fn 服务端不该收到的包型不给处理路径() {
        let me = g(1);
        for ty in [Type::Retry, Type::VersionNegotiation] {
            assert_eq!(
                ownership(&me, ty, &cid_of(&me), false),
                Ownership::Undecidable
            );
        }
    }

    #[test]
    fn 铸出来的_scid_长度对且前缀是本代() {
        let me = GenId::random();
        let cid = me.mint_scid();
        assert_eq!(cid.len(), SCID_LEN);
        assert_eq!(&cid[..GEN_ID_LEN], me.as_bytes());
        assert!(me.owns(&cid));
    }

    /// ★ 证明尾巴**真的是随机的** —— 一个把尾巴写死成常量的实现，
    /// 上面那条「长度对且前缀对」照样全绿。
    #[test]
    fn 两次铸出来的_scid_尾巴不同() {
        let me = GenId::random();
        let (a, b) = (me.mint_scid(), me.mint_scid());
        assert_ne!(
            &a[GEN_ID_LEN..],
            &b[GEN_ID_LEN..],
            "CID 的随机尾巴两次相同 —— 随机源多半没接上"
        );
        // 而前缀必须两次都一样。
        assert_eq!(&a[..GEN_ID_LEN], &b[..GEN_ID_LEN]);
    }

    /// ★ 同理：两代的标识必须分得开，否则整条转交判定恒为 `Local`。
    #[test]
    fn 两次生成的代标识不同() {
        assert_ne!(GenId::random(), GenId::random());
    }

    #[test]
    fn hex_是十六个字符且能还原前缀() {
        let me = GenId::from_bytes([0x00, 0x0f, 0xa0, 0xff, 0x10, 0x20, 0x30, 0x40]);
        assert_eq!(me.hex(), "000fa0ff10203040");
        assert_eq!(prefix_of(&cid_of(&me)), Some(me));
    }
}
