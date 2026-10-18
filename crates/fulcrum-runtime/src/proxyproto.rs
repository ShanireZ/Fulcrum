//! PROXY protocol v1 / v2 的**編解碼**（**M2 批 D**）。
//!
//! # 這個模組只做純邏輯
//!
//! **不碰網路、不碰 socket**：喂進一段位元組，回答「這是不是 PROXY 頭、真實用戶端是誰、
//! 這個頭佔了多少位元組」。★ **這是解析攻擊者控制的位元組，它必須能被單測按位元組喂** ——
//! 長在 socket 循環裡的解析器只能靠真連接去測，而那樣紅了指不到是哪一條邊界。
//!
//! # ⚠ 為什麼自己寫，而基線第 5 條說「不自己實作協定」
//!
//! 基線點名的是 TLS / HTTP2 狀態機 / HPACK / QUIC —— 那些是**密碼學 + 複雜狀態機**。
//! PROXY protocol 是一個定長頭加一段位址，**沒有狀態機**。
//! ★ 全部風險都在「長度欄位是攻擊者給的」這一件事上，自己寫才能把那幾條上界寫在明處
//! 並逐條設門（見檔案末尾那組「壞頭」單測）。⚠ 代價：**每一條邊界檢查都是我們的責任**。
//!
//! # 規格來源
//!
//! HAProxy 的 `doc/proxy-protocol.txt`（v1 §2.1、v2 §2.2）。三處各有一條單測釘住：
//!
//! 1. **v2 簽名**是 12 個固定位元組 `0D 0A 0D 0A 00 0D 0A 51 55 49 54 0A`
//!    —— 它被特意選成「在 v1 裡、在 HTTP 裡、在 SMTP 裡都不可能是合法開頭」。
//! 2. **`LOCAL` 命令**（v2 cmd=0）與 **`PROXY UNKNOWN`**（v1）表示
//!    「這條連接是代理自己發起的，沒有真實用戶端」——★ 典型來源是**上游 LB 的健康檢查**。
//!    ⚠ 它們**不是錯誤**：正確處置是「頭照樣吃掉，用戶端位址仍用 socket 對端」。
//!    把它判成壞頭會讓每一次健康檢查都斷連。
//! 3. **v2 的 `len` 之後可以跟 TLV**（TLS 資訊、命名空間等）。我們**不解析 TLV**，
//!    但必須**照 `len` 把它跳過去** —— 少跳一個位元組，上游收到的應用流就從中間開始。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// v2 的 12 位元組簽名。
const V2_SIG: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

/// v1 的前綴。
const V1_SIG: &[u8] = b"PROXY ";

/// v2 固定頭的長度：12 位元組簽名 + `ver_cmd` + `fam` + 2 位元組 `len`。
const V2_FIXED: usize = 16;

/// ★ v1 的規格上限：最長的一行是 IPv6 的那種，**107 位元組**（含 `\r\n`）。
///
/// ⚠ 這個數字是規格給的，不是估的 —— 規格裡逐字算過：
/// `PROXY TCP6 ` + 兩個 45 字元的 v6 位址 + 兩個 5 位連接埠 + 三個空格 + `\r\n`。
const V1_MAX: usize = 107;

/// ★ ★ v2 payload 的上限，**這是我們自己定的，規格沒有**。
///
/// 規格里 `len` 是 u16 ⇒ 攻擊者可以宣告 **65535** 位元組的 payload，
/// 而我們會為此快取那麼多位元組、並從應用流裡吃掉那麼多位元組。
/// ⚠ 位址部分最大是 UNIX 那種（216 位元組），其餘全是 TLV；
/// 一個正常的傳送方（HAProxy / nginx / 雲 LB）不會超過幾百位元組。
/// ⇒ 上界取 **1024**：夠所有真實用法，又把「一個頭吃掉 64 KiB」擋在外面。
///
/// ★ 超了不是「截斷」而是**判壞頭**：截斷會讓我們與上游對這條流的起點看法不一致，
/// 那正是這類缺陷最難查的形態。
const V2_MAX_PAYLOAD: usize = 1024;

/// 一個 PROXY 頭最多可能有多長 —— 呼叫方按它設讀緩衝上限。
pub const MAX_HEADER: usize = V2_FIXED + V2_MAX_PAYLOAD;

/// 喂一段位元組之後的裁決。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 還判斷不了，**至少**還要這麼多位元組。
    ///
    /// ★ 是「至少」不是「正好」：v1 要一路找 `\r\n`，找到之前只能一個位元組一個位元組要。
    Need(usize),
    /// 這是一個合法的 PROXY 頭。
    Done {
        /// 真實用戶端位址。
        ///
        /// ⚠ ⚠ **`None` 是正常結果，不是失敗**：`LOCAL` 命令與 `PROXY UNKNOWN`
        /// 都表示「沒有真實用戶端」（健康檢查就長這樣）。
        /// 呼叫方此時應當**繼續用 socket 對端**，而不是斷開連接。
        client: Option<SocketAddr>,
        /// 這個頭佔了多少位元組 —— 呼叫方要從應用流裡把它們吃掉。
        consumed: usize,
    },
    /// 不是 PROXY 頭，或者是一個壞頭。
    ///
    /// ★ 帶一句人話的理由：這條會被記進紀錄，而「解析失敗」四個字幫不了任何人。
    Invalid(&'static str),
}

/// 喂進已經讀到的位元組，問「這是不是一個 PROXY 頭」。
///
/// ★ ★ **它是純函式**：同樣的輸入永遠給同樣的答案，呼叫方可以在每次 `read` 之後
/// 原樣再問一遍。⇒ 不需要在解析器裡維護任何狀態。
pub fn decode(buf: &[u8]) -> Verdict {
    // ── 先判是 v2 還是 v1 ────────────────────────────────────────────────
    //
    // ★ 判別只看前綴，而兩種前綴**互相不可能是對方的前綴**（v2 簽名第一個位元組是
    //   0x0D，v1 是 'P'）⇒ 一個位元組就能分岔，不存在「讀多了才知道猜錯」。
    if buf.is_empty() {
        return Verdict::Need(1);
    }
    if buf[0] == V2_SIG[0] {
        decode_v2(buf)
    } else if buf[0] == V1_SIG[0] {
        decode_v1(buf)
    } else {
        Verdict::Invalid("既不是 PROXY protocol v1 也不是 v2 的开头")
    }
}

// ── v1：一行文字 ──────────────────────────────────────────────────────────

fn decode_v1(buf: &[u8]) -> Verdict {
    // 前綴還沒讀全。
    let n = buf.len().min(V1_SIG.len());
    if buf[..n] != V1_SIG[..n] {
        return Verdict::Invalid("v1 前缀不是 `PROXY `");
    }
    if buf.len() < V1_SIG.len() {
        return Verdict::Need(V1_SIG.len() - buf.len());
    }

    // ★ 找行尾。⚠ 規格要求**必須**是 `\r\n`，單獨一個 `\n` 不算 ——
    //   不較這個真的話，一個「差不多對」的傳送方會在兩端產生不同的消費長度。
    let end = match find_crlf(buf) {
        Some(i) => i,
        None => {
            // 還沒找到行尾：只要還沒超上限，就繼續要位元組。
            if buf.len() >= V1_MAX {
                return Verdict::Invalid("v1 行超过 107 字节仍没有 CRLF");
            }
            return Verdict::Need(1);
        }
    };
    let consumed = end + 2;
    if consumed > V1_MAX {
        return Verdict::Invalid("v1 行超过 107 字节");
    }

    // `PROXY ` 之後到 CRLF 之前。
    let line = &buf[V1_SIG.len()..end];
    let line = match std::str::from_utf8(line) {
        Ok(s) => s,
        Err(_) => return Verdict::Invalid("v1 行里有非 ASCII 字节"),
    };

    // ⚠ 用 `split(' ')` 而不是 `split_whitespace()`：後者會把連續空格、製表符
    //   都當分隔符，於是 `PROXY  TCP4 …`（兩個空格）會被我們接受而被別人拒絕。
    //   ★ 在這種「兩端必須逐位元組同意」的協定上，寬容就是分歧。
    let mut parts = line.split(' ');
    let proto = parts.next().unwrap_or("");

    // `PROXY UNKNOWN` —— 合法，但沒有真實用戶端。
    //
    // ⚠ 規格允許 UNKNOWN 後面跟任意內容（傳送方可以把它知道的東西寫上去），
    //   接收方**必須忽略到行尾** ⇒ 這裡不檢核剩下的部分。
    if proto == "UNKNOWN" {
        return Verdict::Done {
            client: None,
            consumed,
        };
    }

    let is_v6 = match proto {
        "TCP4" => false,
        "TCP6" => true,
        _ => return Verdict::Invalid("v1 的协议只能是 TCP4 / TCP6 / UNKNOWN"),
    };

    let (Some(src), Some(dst), Some(sport), Some(dport)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Verdict::Invalid("v1 行的字段不足 5 个");
    };
    if parts.next().is_some() {
        return Verdict::Invalid("v1 行的字段多于 5 个");
    }
    // `dst` 與 `dport` 規格要求存在且合法，但我們只用 src ——
    // ★ 仍然**檢核**它們：一個我們不看的欄位如果是垃圾，說明這個頭整體不可信。
    let _ = dst;
    let _ = dport;

    let ip = match parse_ip(src, is_v6) {
        Some(ip) => ip,
        None => return Verdict::Invalid("v1 的源地址与声明的协议族对不上"),
    };
    if parse_ip(dst, is_v6).is_none() {
        return Verdict::Invalid("v1 的目的地址与声明的协议族对不上");
    }
    let Ok(port) = sport.parse::<u16>() else {
        return Verdict::Invalid("v1 的源端口不是合法端口");
    };
    if dport.parse::<u16>().is_err() {
        return Verdict::Invalid("v1 的目的端口不是合法端口");
    }
    // ⚠ `"0080".parse::<u16>()` 是 80，而規格要求連接埠**不帶前導零**。
    //   ★ 兩端對同一個頭算出不同的位元組數是這類協定最壞的錯法，所以這裡較真。
    if sport.len() > 1 && sport.starts_with('0') {
        return Verdict::Invalid("v1 的源端口有前导零");
    }

    Verdict::Done {
        client: Some(SocketAddr::new(ip, port)),
        consumed,
    }
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    // ★ 有意不用正則也不用 memchr：兩個位元組的固定串，寫清楚比引一個相依便宜。
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_ip(s: &str, want_v6: bool) -> Option<IpAddr> {
    // ⚠ ⚠ **必須按宣告的族分別解析，不能只 `s.parse::<IpAddr>()`**：
    //   後者會讓 `PROXY TCP4 ::1 …` 通過，而那是一個自相矛盾的頭。
    //   ★ 一個自相矛盾的頭說明傳送方與我們對這條連接的理解不一致，
    //     此時「寬容地接受」等於把不一致帶進下游。
    if want_v6 {
        s.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
    } else {
        s.parse::<Ipv4Addr>().ok().map(IpAddr::V4)
    }
}

// ── v2：二進位 ────────────────────────────────────────────────────────────

fn decode_v2(buf: &[u8]) -> Verdict {
    let n = buf.len().min(V2_SIG.len());
    if buf[..n] != V2_SIG[..n] {
        return Verdict::Invalid("v2 签名对不上");
    }
    if buf.len() < V2_FIXED {
        return Verdict::Need(V2_FIXED - buf.len());
    }

    let ver_cmd = buf[12];
    // 高 4 位是版本，必須是 2。
    if ver_cmd >> 4 != 0x2 {
        return Verdict::Invalid("v2 头里的版本号不是 2");
    }
    let cmd = ver_cmd & 0x0F;

    let fam = buf[13];
    // ★ `len` 是**大端** u16。
    let plen = u16::from_be_bytes([buf[14], buf[15]]) as usize;
    if plen > V2_MAX_PAYLOAD {
        return Verdict::Invalid("v2 声明的 payload 超过 1024 字节的上界");
    }
    let consumed = V2_FIXED + plen;
    if buf.len() < consumed {
        return Verdict::Need(consumed - buf.len());
    }

    // ── cmd ─────────────────────────────────────────────────────────────
    //
    // 0 = LOCAL：連接由代理自己發起（健康檢查）⇒ **沒有真實用戶端**。
    // 1 = PROXY：頭裡帶著真實用戶端。
    match cmd {
        0 => {
            // ⚠ ⚠ LOCAL 時**規格要求接收方忽略位址部分**（哪怕它填了東西）。
            //   ★ 照 `len` 把它跳過去仍然是必須的 —— 忽略的是內容，不是長度。
            return Verdict::Done {
                client: None,
                consumed,
            };
        }
        1 => {}
        _ => return Verdict::Invalid("v2 头里的命令既不是 LOCAL 也不是 PROXY"),
    }

    // ── 位址族與協定 ────────────────────────────────────────────────────
    //
    // 高 4 位：0=UNSPEC 1=INET 2=INET6 3=UNIX；低 4 位：0=UNSPEC 1=STREAM 2=DGRAM。
    let payload = &buf[V2_FIXED..consumed];
    let client = match fam >> 4 {
        0x1 => {
            // IPv4：4 + 4 + 2 + 2 = 12 位元組
            if payload.len() < 12 {
                return Verdict::Invalid("v2 声明的是 IPv4，但 payload 不足 12 字节");
            }
            let src = Ipv4Addr::new(payload[0], payload[1], payload[2], payload[3]);
            let port = u16::from_be_bytes([payload[8], payload[9]]);
            Some(SocketAddr::new(IpAddr::V4(src), port))
        }
        0x2 => {
            // IPv6：16 + 16 + 2 + 2 = 36 位元組
            if payload.len() < 36 {
                return Verdict::Invalid("v2 声明的是 IPv6，但 payload 不足 36 字节");
            }
            let mut o = [0u8; 16];
            o.copy_from_slice(&payload[0..16]);
            let port = u16::from_be_bytes([payload[32], payload[33]]);
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), port))
        }
        // UNSPEC(0) 與 UNIX(3)：**都不是錯誤**，只是沒有我們用得上的位址。
        //
        // ⚠ UNSPEC 是規格建議的「傳送方也不知道」的寫法，UNIX 是本機 socket。
        //   ★ 兩者都按「照 len 吃掉、用戶端仍用 socket 對端」處理 ——
        //     與 LOCAL 同一條口徑。
        0x0 | 0x3 => None,
        _ => return Verdict::Invalid("v2 头里的地址族不认得"),
    };

    Verdict::Done { client, consumed }
}

// ── 編碼（發那一側）───────────────────────────────────────────────────────

/// 要寫哪個版本。**預設 v2**（owner 拍板）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// 文字，人眼可讀，抓包好看。
    V1,
    /// 二進位，定長頭。**預設**。
    V2,
}

impl Default for Version {
    fn default() -> Self {
        // ★ ★ **有意不寫 `#[derive(Default)]` + `#[default] V2`。**
        //   預設值只有一份，住在 `fulcrum_config::model::DEFAULT_PROXY_PROTOCOL`
        //   （DSL 裡 `proxy_protocol` 省略參數時填的也是它）——
        //   ⚠ 兩處各寫一個 `V2` 的代價很具體：將來改預設版本時改漏一處，
        //   現場表現是「省略參數與顯式寫 v2 行為不同」，沒有任何一道門會說出來。
        //   ★ `unwrap_or` 那一支由下面 `默认值那个常量必须是_parse_认得的词` 釘住。
        Version::parse(fulcrum_config::model::DEFAULT_PROXY_PROTOCOL).unwrap_or(Version::V2)
    }
}

impl Version {
    /// DSL 裡寫的那個詞。
    pub fn parse(s: &str) -> Option<Version> {
        match s {
            "v1" => Some(Version::V1),
            "v2" => Some(Version::V2),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Version::V1 => "v1",
            Version::V2 => "v2",
        }
    }
}

/// 造一個要發給上游的 PROXY 頭。
///
/// - `client`：**真實用戶端** —— 如果樞衡自己也收了一個 PROXY 頭，
///   這裡要放**那個頭裡的位址**，而不是 socket 對端。★ 這就是「鏈式傳遞」，
///   也是這整件事存在的理由：中間隔幾層，上游看到的仍是最初那個人。
/// - `local`：用戶端連到的那個本機位址（樞衡的監聽位址）。
///
/// ⚠ ⚠ **兩個位址的協定族必須一致**，否則造不出合法的頭。族不一致時回傳
/// `LOCAL` / `UNKNOWN`（**而不是硬湊一個**）—— 上游會正確地理解成
/// 「這條連接沒有可報的用戶端」，而一個硬湊的頭會讓它相信一個假位址。
pub fn encode(version: Version, client: SocketAddr, local: SocketAddr) -> Vec<u8> {
    match version {
        Version::V1 => encode_v1(client, local),
        Version::V2 => encode_v2(client, local),
    }
}

fn encode_v1(client: SocketAddr, local: SocketAddr) -> Vec<u8> {
    match (client.ip(), local.ip()) {
        (IpAddr::V4(c), IpAddr::V4(l)) => format!(
            "PROXY TCP4 {} {} {} {}\r\n",
            c,
            l,
            client.port(),
            local.port()
        )
        .into_bytes(),
        (IpAddr::V6(c), IpAddr::V6(l)) => format!(
            "PROXY TCP6 {} {} {} {}\r\n",
            c,
            l,
            client.port(),
            local.port()
        )
        .into_bytes(),
        // ★ 族不一致（v4 用戶端連到 v6 監聽器上的映射位址等）⇒ 誠實地說不知道。
        _ => b"PROXY UNKNOWN\r\n".to_vec(),
    }
}

fn encode_v2(client: SocketAddr, local: SocketAddr) -> Vec<u8> {
    let mut out = Vec::with_capacity(V2_FIXED + 36);
    out.extend_from_slice(&V2_SIG);
    match (client.ip(), local.ip()) {
        (IpAddr::V4(c), IpAddr::V4(l)) => {
            out.push(0x21); // ver 2 | cmd PROXY
            out.push(0x11); // AF_INET | STREAM
            out.extend_from_slice(&12u16.to_be_bytes());
            out.extend_from_slice(&c.octets());
            out.extend_from_slice(&l.octets());
            out.extend_from_slice(&client.port().to_be_bytes());
            out.extend_from_slice(&local.port().to_be_bytes());
        }
        (IpAddr::V6(c), IpAddr::V6(l)) => {
            out.push(0x21);
            out.push(0x21); // AF_INET6 | STREAM
            out.extend_from_slice(&36u16.to_be_bytes());
            out.extend_from_slice(&c.octets());
            out.extend_from_slice(&l.octets());
            out.extend_from_slice(&client.port().to_be_bytes());
            out.extend_from_slice(&local.port().to_be_bytes());
        }
        _ => {
            // ★ LOCAL + UNSPEC + 零長 payload —— 規格里「我沒有可報的位址」的標準寫法。
            out.push(0x20); // ver 2 | cmd LOCAL
            out.push(0x00); // AF_UNSPEC
            out.extend_from_slice(&0u16.to_be_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str, p: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(s.parse().unwrap()), p)
    }
    fn v6(s: &str, p: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(s.parse().unwrap()), p)
    }

    // ── v1 ──────────────────────────────────────────────────────────────

    #[test]
    fn v1_的_tcp4_读得出来_而_consumed_只算头不算后面的应用流() {
        // ★ 有意把頭與後面的 HTTP 請求分開寫，再讓斷言自己去算 ——
        //   一個手抄的位元組數寫錯了，測試會紅在一個與它無關的地方（例如：
        //   寫了 42，而實際是 43，紅出來的樣子像是解析器多吃了一個位元組）。
        let head = b"PROXY TCP4 192.168.0.1 10.0.0.9 56324 443\r\n";
        let mut b = head.to_vec();
        b.extend_from_slice(b"GET / HTTP/1.1\r\n");
        assert_eq!(
            decode(&b),
            Verdict::Done {
                client: Some(v4("192.168.0.1", 56324)),
                consumed: head.len(),
            }
        );
    }

    #[test]
    fn v1_的_tcp6_读得出来() {
        let b = b"PROXY TCP6 2001:db8::1 2001:db8::2 56324 443\r\n";
        let Verdict::Done { client, consumed } = decode(b) else {
            panic!("应当读得出来");
        };
        assert_eq!(client, Some(v6("2001:db8::1", 56324)));
        assert_eq!(consumed, b.len());
    }

    #[test]
    fn v1_的_unknown_是没有客户端而不是坏头() {
        // ★ 這一條最容易寫錯，而寫錯的現場表現是「每一次健康檢查都斷連」。
        assert_eq!(
            decode(b"PROXY UNKNOWN\r\n"),
            Verdict::Done {
                client: None,
                consumed: 15,
            }
        );
        // 規格允許 UNKNOWN 後面跟東西，接收方忽略到行尾。
        assert!(matches!(
            decode(b"PROXY UNKNOWN 1.2.3.4 5.6.7.8 1 2\r\n"),
            Verdict::Done { client: None, .. }
        ));
    }

    #[test]
    fn v1_一个字节一个字节喂也能收敛() {
        // ★ 這是它作為純函式的意義所在：呼叫方每讀一次就原樣再問一遍。
        let full = b"PROXY TCP4 1.2.3.4 5.6.7.8 1111 443\r\n";
        for i in 0..full.len() {
            match decode(&full[..i]) {
                Verdict::Need(n) => assert!(n >= 1, "第 {i} 个字节时要 {n}"),
                other => panic!("第 {i} 个字节就下结论了：{other:?}"),
            }
        }
        assert!(matches!(decode(full), Verdict::Done { .. }));
    }

    #[test]
    fn v1_单独一个_lf_不算行尾() {
        // ⚠ 不較真的話，兩端會對「這個頭有多長」有不同看法。
        let b = b"PROXY TCP4 1.2.3.4 5.6.7.8 1111 443\n";
        assert!(matches!(decode(b), Verdict::Need(_)));
    }

    #[test]
    fn v1_族与地址对不上要判坏() {
        // ★ 一個自相矛盾的頭 ⇒ 傳送方與我們的理解不一致，不許寬容地放過去。
        assert!(matches!(
            decode(b"PROXY TCP4 ::1 ::2 1 2\r\n"),
            Verdict::Invalid(_)
        ));
        assert!(matches!(
            decode(b"PROXY TCP6 1.2.3.4 5.6.7.8 1 2\r\n"),
            Verdict::Invalid(_)
        ));
    }

    #[test]
    fn v1_端口的前导零要判坏() {
        assert!(matches!(
            decode(b"PROXY TCP4 1.2.3.4 5.6.7.8 0080 443\r\n"),
            Verdict::Invalid(_)
        ));
    }

    #[test]
    fn v1_两个空格要判坏() {
        // ⚠ `split_whitespace()` 會放過它，而別的實作不會 —— 寬容就是分歧。
        assert!(matches!(
            decode(b"PROXY  TCP4 1.2.3.4 5.6.7.8 1 2\r\n"),
            Verdict::Invalid(_)
        ));
    }

    #[test]
    fn v1_字段多一个少一个都要判坏() {
        assert!(matches!(
            decode(b"PROXY TCP4 1.2.3.4 5.6.7.8 1\r\n"),
            Verdict::Invalid(_)
        ));
        assert!(matches!(
            decode(b"PROXY TCP4 1.2.3.4 5.6.7.8 1 2 3\r\n"),
            Verdict::Invalid(_)
        ));
    }

    #[test]
    fn v1_超过一百零七字节还没换行就判坏() {
        // ★ 少了這條上界，一個只發 `PROXY ` 再也不說話的對端會讓我們一直要位元組。
        let mut b = b"PROXY TCP4 ".to_vec();
        b.extend(std::iter::repeat_n(b'x', 200));
        assert!(matches!(decode(&b), Verdict::Invalid(_)));
    }

    // ── v2 ──────────────────────────────────────────────────────────────

    #[test]
    fn v2_的_ipv4_读得出来() {
        let h = encode(Version::V2, v4("192.168.0.1", 56324), v4("10.0.0.9", 443));
        assert_eq!(h.len(), 28);
        assert_eq!(
            decode(&h),
            Verdict::Done {
                client: Some(v4("192.168.0.1", 56324)),
                consumed: 28,
            }
        );
    }

    #[test]
    fn v2_的_ipv6_读得出来() {
        let h = encode(
            Version::V2,
            v6("2001:db8::1", 56324),
            v6("2001:db8::2", 443),
        );
        assert_eq!(h.len(), 52);
        assert_eq!(
            decode(&h),
            Verdict::Done {
                client: Some(v6("2001:db8::1", 56324)),
                consumed: 52,
            }
        );
    }

    #[test]
    fn v2_的_local_是没有客户端而不是坏头() {
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x20, 0x00, 0x00, 0x00]);
        assert_eq!(
            decode(&h),
            Verdict::Done {
                client: None,
                consumed: 16,
            }
        );
    }

    #[test]
    fn v2_的_tlv_按_len_跳过去而不是解析() {
        // ★ 少跳一個位元組，上游收到的應用流就從中間開始 —— 現場表現是
        //   「上游偶爾報協定錯誤」，而樞衡這邊一切正常。
        let mut h = encode(Version::V2, v4("1.2.3.4", 1111), v4("5.6.7.8", 443));
        // 把 len 從 12 改成 12+5，並追加 5 位元組 TLV。
        let plen = 12u16 + 5;
        h[14..16].copy_from_slice(&plen.to_be_bytes());
        h.extend_from_slice(&[0x01, 0x00, 0x02, 0xAA, 0xBB]);
        assert_eq!(
            decode(&h),
            Verdict::Done {
                client: Some(v4("1.2.3.4", 1111)),
                consumed: 16 + 17,
            }
        );
    }

    #[test]
    fn v2_声明一个巨大的_payload_要判坏而不是照单全收() {
        // ⚠ ⚠ 這是本模組最要緊的一條：`len` 是攻擊者給的 u16。
        //   ★ 判壞而不是截斷 —— 截斷會讓我們與上游對流的起點看法不一致。
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x21, 0x11]);
        h.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(decode(&h), Verdict::Invalid(_)));
    }

    #[test]
    fn v2_声明的族与_payload_长度对不上要判坏() {
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x21, 0x21]); // 說是 IPv6
        h.extend_from_slice(&12u16.to_be_bytes()); // 卻只給 12 位元組
        h.extend(std::iter::repeat_n(0u8, 12));
        assert!(matches!(decode(&h), Verdict::Invalid(_)));
    }

    #[test]
    fn v2_版本号不是二要判坏() {
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x31, 0x11]);
        h.extend_from_slice(&12u16.to_be_bytes());
        h.extend(std::iter::repeat_n(0u8, 12));
        assert!(matches!(decode(&h), Verdict::Invalid(_)));
    }

    #[test]
    fn v2_不认得的命令要判坏() {
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x27, 0x11]);
        h.extend_from_slice(&12u16.to_be_bytes());
        h.extend(std::iter::repeat_n(0u8, 12));
        assert!(matches!(decode(&h), Verdict::Invalid(_)));
    }

    #[test]
    fn v2_的_unspec_与_unix_是没有客户端而不是坏头() {
        for fam in [0x01u8, 0x31u8] {
            let mut h = V2_SIG.to_vec();
            h.extend_from_slice(&[0x21, fam]);
            h.extend_from_slice(&0u16.to_be_bytes());
            assert!(
                matches!(decode(&h), Verdict::Done { client: None, .. }),
                "fam={fam:#x}"
            );
        }
    }

    #[test]
    fn v2_一个字节一个字节喂也能收敛() {
        let full = encode(Version::V2, v4("1.2.3.4", 1111), v4("5.6.7.8", 443));
        for i in 0..full.len() {
            match decode(&full[..i]) {
                Verdict::Need(n) => assert!(n >= 1),
                other => panic!("第 {i} 个字节就下结论了：{other:?}"),
            }
        }
    }

    // ── 判別與拒絕 ──────────────────────────────────────────────────────

    #[test]
    fn 一个普通的_http_请求当场被判成不是_proxy_头() {
        // ★ 這一條決定了「不在信任清單就一個位元組都不讀」那條紀律的代價有多大：
        //   即便讀了，一個 HTTP 請求也是當場被拒的，不會被誤當成頭吃掉。
        assert!(matches!(decode(b"GET / HTTP/1.1\r\n"), Verdict::Invalid(_)));
        // TLS ClientHello 的第一個位元組是 0x16。
        assert!(matches!(decode(&[0x16, 0x03, 0x01]), Verdict::Invalid(_)));
    }

    #[test]
    fn 空输入要一个字节而不是判坏() {
        assert_eq!(decode(b""), Verdict::Need(1));
    }

    #[test]
    fn v2_签名的第一个字节与_v1_不同所以一个字节就能分岔() {
        // ★ 這是「判別只看前綴」那句話的判據。
        assert_ne!(V2_SIG[0], V1_SIG[0]);
    }

    // ── 編碼 ────────────────────────────────────────────────────────────

    #[test]
    fn 编出来的头自己读得回来() {
        for ver in [Version::V1, Version::V2] {
            let c = v4("203.0.113.7", 40000);
            let l = v4("10.0.0.9", 443);
            let h = encode(ver, c, l);
            assert_eq!(
                decode(&h),
                Verdict::Done {
                    client: Some(c),
                    consumed: h.len(),
                },
                "{}",
                ver.as_str()
            );
        }
    }

    #[test]
    fn 族不一致时诚实地说不知道而不是硬凑一个() {
        // ⚠ ⚠ 硬湊會讓上游相信一個假位址；說不知道則讓它退回用 socket 對端。
        let c = v4("1.2.3.4", 1111);
        let l = v6("::1", 443);
        assert!(matches!(
            decode(&encode(Version::V1, c, l)),
            Verdict::Done { client: None, .. }
        ));
        assert!(matches!(
            decode(&encode(Version::V2, c, l)),
            Verdict::Done { client: None, .. }
        ));
    }

    #[test]
    fn 版本名与_dsl_里写的那个词一一对应() {
        assert_eq!(Version::parse("v1"), Some(Version::V1));
        assert_eq!(Version::parse("v2"), Some(Version::V2));
        assert_eq!(Version::parse("V2"), None, "大小写不该被放过");
        assert_eq!(Version::parse("2"), None);
        assert_eq!(Version::default(), Version::V2, "owner 拍板默认 v2");
    }

    #[test]
    fn 默认值那个常量必须是_parse_认得的词() {
        // ★ `Version::default()` 裡那個 `unwrap_or` 是一條靜默退回：
        //   常數被改成一個 parse 不認的詞時，它會**安靜地**給出 V2。
        //   ⇒ 這條把那個靜默變成一次當場的紅。
        assert!(
            Version::parse(fulcrum_config::model::DEFAULT_PROXY_PROTOCOL).is_some(),
            "DEFAULT_PROXY_PROTOCOL 是 `{}`，而 Version::parse 不认得它",
            fulcrum_config::model::DEFAULT_PROXY_PROTOCOL
        );
        // ★ 另一半：`PROXY_PROTOCOL_VERSIONS` 裡每一個詞也都必須解析得出來 ——
        //   否則 DSL 的編譯期檢核會放過一個執行環境用不了的版本。
        for v in fulcrum_config::model::PROXY_PROTOCOL_VERSIONS {
            assert!(
                Version::parse(v).is_some(),
                "DSL 允许写 `{v}`，而 Version::parse 不认得它"
            );
        }
    }

    #[test]
    fn 上限是一个具体的数而不是无界() {
        // ★ 呼叫方按 MAX_HEADER 設讀緩衝；它變了這條會紅，提醒去看呼叫方。
        assert_eq!(MAX_HEADER, 16 + 1024);
    }
}
