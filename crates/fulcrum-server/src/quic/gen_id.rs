//! 代標識（`gen_id`）與連接 ID 的形狀 —— **G109 ① 的落點**（§10）。
//!
//! # 為什麼這一塊必須在批 J 就做完
//!
//! 換代時兩代處理程序持有**同一個** UDP socket（這正是 fd 移交成功的表現），兩邊都在
//! `recv_from`，核心把資料報**任意**分給其中一代；而 QUIC 的連接狀態（金鑰、流、
//! 擁塞視窗）只在某一代的記憶體裡。⇒ 必須能從**資料報本身**看出它屬於哪一代。
//!
//! G109 取的辦法是把代標識編進**我們自選的連接 ID 前綴**裡。
//! ⚠ ⚠ ★ **CID 的形狀是對外可見的**：用戶端會把我們給的 SCID 原樣當作後續包的 DCID。
//! 等批 K 再改前綴，會讓批 J 期間發出去的連接在換代時**全部認不出來** ——
//! 所以「轉交」本身是批 K，而**前綴必須是批 J**。
//!
//! # 這裡只回答「這個資料報屬於誰」
//!
//! 真正把它送過去的是 [`super::relay`]（批 K）。
//! ⚠ **「前綴已經帶上了」讀起來很像「轉交已經能用了」** —— 那是兩件事，
//! 而這個模組只做前一件。

use rand::Rng;

/// 代標識在 CID 前綴裡佔的位元組數（G109 ①：8 位元組）。
pub const GEN_ID_LEN: usize = 8;

/// 伺服器端自選的連接 ID 總長度。
///
/// ★ 前 [`GEN_ID_LEN`] 位元組是代標識，其餘是隨機尾巴。
/// ⚠ RFC 9000 §17.2 允許 CID 最長 20 位元組；取 16 是為了給尾巴留夠 8 位元組熵，
/// 同時讓每個包的頭部少背 4 個位元組。
pub const SCID_LEN: usize = 16;

const _: () = assert!(SCID_LEN > GEN_ID_LEN, "CID 的随机尾巴不能是空的");
const _: () = assert!(
    SCID_LEN <= quiche::MAX_CONN_ID_LEN,
    "CID 超出 RFC 9000 的上限"
);

/// 一代處理程序的身分。處理程序啟動時隨機生成一次，此後不變。
///
/// ⚠ **它不是秘密，但必須不可預測**：能預測下一代的 `gen_id` 就能讓我們把資料報
/// 轉交給一個不存在的地方。⇒ 走 CSPRNG（見 [`GenId::random`]）。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenId([u8; GEN_ID_LEN]);

impl GenId {
    /// 生成本代的標識。
    ///
    /// ⚠ 取 `rand::rng()`（`ThreadRng`，OS 播種的 ChaCha，`impl TryCryptoRng`），
    /// **不是** `SmallRng` —— 理由見本類型的文件註解。
    pub fn random() -> Self {
        let mut b = [0u8; GEN_ID_LEN];
        rand::rng().fill_bytes(&mut b);
        GenId(b)
    }

    /// 從固定位元組建構。★ 只給單測與「從 DCID 前綴還原出對方是誰」用。
    pub fn from_bytes(b: [u8; GEN_ID_LEN]) -> Self {
        GenId(b)
    }

    pub fn as_bytes(&self) -> &[u8; GEN_ID_LEN] {
        &self.0
    }

    /// 十六進制形式 —— 轉交通道的 socket 路徑由它推導
    /// （`<run_dir>/quic-relay-<gen_id_hex>.sock`，G109 ②）。
    ///
    /// ★ ★ **路徑就在 DCID 那 8 個位元組裡，所以兩代之間不需要任何握手或協商。**
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(GEN_ID_LEN * 2);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// 鑄一個本代的伺服器端連接 ID：`gen_id ‖ 随机`。
    pub fn mint_scid(&self) -> quiche::ConnectionId<'static> {
        let mut cid = [0u8; SCID_LEN];
        cid[..GEN_ID_LEN].copy_from_slice(&self.0);
        rand::rng().fill_bytes(&mut cid[GEN_ID_LEN..]);
        quiche::ConnectionId::from_ref(&cid).into_owned()
    }

    /// 這個 DCID 的前綴是不是本代？
    ///
    /// ⚠ **短於 [`GEN_ID_LEN`] 的 DCID 一律回 `false`** —— 它判不出來，
    /// 而「判不出來」絕不能被讀成「是我的」。
    pub fn owns(&self, dcid: &[u8]) -> bool {
        prefix_of(dcid).is_some_and(|g| g == *self)
    }
}

impl std::fmt::Debug for GenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gen:{}", self.hex())
    }
}

/// 從一個 DCID 裡摳出代標識前綴。**短於 [`GEN_ID_LEN`] 時回 `None`。**
pub fn prefix_of(dcid: &[u8]) -> Option<GenId> {
    let head: [u8; GEN_ID_LEN] = dcid.get(..GEN_ID_LEN)?.try_into().ok()?;
    Some(GenId(head))
}

/// 一個資料報該由誰處理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// 新連接：用戶端自選的 DCID，它還沒有任何狀態，落到哪一代都行 ⇒ 本代處理。
    NewConnection,
    /// DCID 是**我們**選的，且前綴就是本代。
    Local,
    /// DCID 是**我們**選的，但屬於另一代 ⇒ 轉交給它。
    /// 送出去的那一半在 [`super::relay`]。
    Relay(GenId),
    /// 判不出來（包頭形狀不對、DCID 太短、或伺服器端本就不該收到這種包）。
    ///
    /// ⚠ **它與 `NewConnection` 必須分開**：把判不出來的包當新連接處理，
    /// 等於讓任何一個畸形資料報都能讓我們建一份狀態。
    Undecidable,
}

/// 判定一個資料報屬於哪一代。
///
/// 收的是**原始類型**而不是 `quiche::Header`，理由很實際：`Header` 帶私有欄位，
/// 單測裡建構不出來 —— 而一個建構不出輸入的判定函式，就只能靠真流量去測，
/// 那樣紅了指不到具體哪條規則（與 `fulcrum-runtime` 拆出來的理由同源）。
///
/// # ★ ★ ★ Initial 那一格不是「一律本代」，而施工圖上寫的是「一律本代」
///
/// G109 那張表把 Initial 整行寫成「用戶端選的 DCID ⇒ 新連接 ⇒ 本代處理」。
/// **開了 Retry 之後這句話有一半不成立**：Retry 交換之後用戶端的第二發 Initial
/// 帶的 DCID 是**我們**在 Retry 包裡給它的，屬於發 Retry 的那一代。
/// 把它當新連接處理，等於讓新一代去接一個老一代已經發過 Retry、
/// 並且正在等它回來的握手 —— **而 Retry 恰恰是 G109 自己要求必須開的**。
///
/// ⇒ 這裡按 **token 在不在**分兩格：
///
/// | 包 | token | 處置 |
/// |---|---|---|
/// | Initial | 無 | `NewConnection`（首度推出，本代回 Retry）|
/// | Initial | 有 | 前綴判定（Retry 之後，DCID 是我們給的）|
/// | Handshake / 0-RTT / Short | — | 前綴判定 |
/// | Retry / VersionNegotiation | — | `Undecidable`（伺服器端不該收到）|
///
/// ⚠ **0-RTT 的首度推出那一格是有意讓它走「前綴判定 ⇒ 轉交 ⇒ 丟棄」的**：
/// 開了 Retry 之後首度推出 0-RTT 本來就會被丟（用戶端必須先完成 Retry 交換），
/// 讓它落進一個不存在的 `gen_id` 從而被丟棄，與協定要求的結果一致，
/// 而**不需要為它單開一條路**。
pub fn ownership(me: &GenId, ty: quiche::Type, dcid: &[u8], has_token: bool) -> Ownership {
    use quiche::Type;
    match ty {
        Type::Initial if !has_token => Ownership::NewConnection,
        Type::Initial | Type::Handshake | Type::ZeroRTT | Type::Short => match prefix_of(dcid) {
            Some(g) if g == *me => Ownership::Local,
            Some(g) => Ownership::Relay(g),
            // DCID 短於 8 位元組 —— 我們自選的 CID 永遠是 SCID_LEN 長，所以這不是我們發的。
            None => Ownership::Undecidable,
        },
        // 伺服器端不會收到這兩種；收到了說明對端在亂發，不給它任何處理路徑。
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

    /// 一個前綴是 `who`、長度合法的 DCID。
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

    /// ★ 命中與錯過都要覆蓋：上面兩條就是同一個判定的兩個方向。
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
        // ★ 就算前綴恰好像是別代的，也仍然是新連接 —— 那 8 個位元組是用戶端隨機選的。
        assert_eq!(
            ownership(&me, Type::Initial, &cid_of(&other), false),
            Ownership::NewConnection
        );
    }

    /// ★ ★ ★ 這一條就是施工圖那張表漏掉的那一格。
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
        // ⚠ `g(0)` 的位元組全是 0，而短 DCID 補零之後看起來正好像它 ——
        //   這條案例挑 0 就是為了把「補零當成命中」這種寫法釘死。
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

    /// ★ 證明尾巴**真的是隨機的** —— 一個把尾巴寫死成常數的實作，
    /// 上面那條「長度對且前綴對」照樣全綠。
    #[test]
    fn 两次铸出来的_scid_尾巴不同() {
        let me = GenId::random();
        let (a, b) = (me.mint_scid(), me.mint_scid());
        assert_ne!(
            &a[GEN_ID_LEN..],
            &b[GEN_ID_LEN..],
            "CID 的随机尾巴两次相同 —— 随机源多半没接上"
        );
        // 而前綴必須兩次都一樣。
        assert_eq!(&a[..GEN_ID_LEN], &b[..GEN_ID_LEN]);
    }

    /// ★ 同理：兩代的標識必須分得開，否則整條轉交判定恆為 `Local`。
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
