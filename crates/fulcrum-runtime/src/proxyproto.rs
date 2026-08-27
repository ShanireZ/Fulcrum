//! PROXY protocol v1 / v2 的**编解码**（**M2 批 D**）。
//!
//! # 这个模块只做纯逻辑
//!
//! **不碰网络、不碰 socket**：喂进一段字节，回答「这是不是 PROXY 头、真实客户端是谁、
//! 这个头占了多少字节」。★ **这是解析攻击者控制的字节，它必须能被单测按字节喂** ——
//! 长在 socket 循环里的解析器只能靠真连接去测，而那样红了指不到是哪一条边界。
//!
//! # ⚠ 为什么自己写，而基线第 5 条说「不自己实现协议」
//!
//! 基线点名的是 TLS / HTTP2 状态机 / HPACK / QUIC —— 那些是**密码学 + 复杂状态机**。
//! PROXY protocol 是一个定长头加一段地址，**没有状态机**。
//! ★ 全部风险都在「长度字段是攻击者给的」这一件事上，自己写才能把那几条上界写在明处
//! 并逐条设门（见文件末尾那组「坏头」单测）。⚠ 代价：**每一条边界检查都是我们的责任**。
//!
//! # 规格来源
//!
//! HAProxy 的 `doc/proxy-protocol.txt`（v1 §2.1、v2 §2.2）。三处各有一条单测钉住：
//!
//! 1. **v2 签名**是 12 个固定字节 `0D 0A 0D 0A 00 0D 0A 51 55 49 54 0A`
//!    —— 它被特意选成「在 v1 里、在 HTTP 里、在 SMTP 里都不可能是合法开头」。
//! 2. **`LOCAL` 命令**（v2 cmd=0）与 **`PROXY UNKNOWN`**（v1）表示
//!    「这条连接是代理自己发起的，没有真实客户端」——★ 典型来源是**上游 LB 的健康检查**。
//!    ⚠ 它们**不是错误**：正确处置是「头照样吃掉，客户端地址仍用 socket 对端」。
//!    把它判成坏头会让每一次健康检查都断连。
//! 3. **v2 的 `len` 之后可以跟 TLV**（TLS 信息、命名空间等）。我们**不解析 TLV**，
//!    但必须**照 `len` 把它跳过去** —— 少跳一个字节，上游收到的应用流就从中间开始。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// v2 的 12 字节签名。
const V2_SIG: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

/// v1 的前缀。
const V1_SIG: &[u8] = b"PROXY ";

/// v2 固定头的长度：12 字节签名 + `ver_cmd` + `fam` + 2 字节 `len`。
const V2_FIXED: usize = 16;

/// ★ v1 的规格上限：最长的一行是 IPv6 的那种，**107 字节**（含 `\r\n`）。
///
/// ⚠ 这个数字是规格给的，不是估的 —— 规格里逐字算过：
/// `PROXY TCP6 ` + 两个 45 字符的 v6 地址 + 两个 5 位端口 + 三个空格 + `\r\n`。
const V1_MAX: usize = 107;

/// ★ ★ v2 payload 的上限，**这是我们自己定的，规格没有**。
///
/// 规格里 `len` 是 u16 ⇒ 攻击者可以声明 **65535** 字节的 payload，
/// 而我们会为此缓存那么多字节、并从应用流里吃掉那么多字节。
/// ⚠ 地址部分最大是 UNIX 那种（216 字节），其余全是 TLV；
/// 一个正常的发送方（HAProxy / nginx / 云 LB）不会超过几百字节。
/// ⇒ 上界取 **1024**：够所有真实用法，又把「一个头吃掉 64 KiB」挡在外面。
///
/// ★ 超了不是「截断」而是**判坏头**：截断会让我们与上游对这条流的起点看法不一致，
/// 那正是这类缺陷最难查的形态。
const V2_MAX_PAYLOAD: usize = 1024;

/// 一个 PROXY 头最多可能有多长 —— 调用方按它设读缓冲上限。
pub const MAX_HEADER: usize = V2_FIXED + V2_MAX_PAYLOAD;

/// 喂一段字节之后的裁决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 还判断不了，**至少**还要这么多字节。
    ///
    /// ★ 是「至少」不是「正好」：v1 要一路找 `\r\n`，找到之前只能一个字节一个字节要。
    Need(usize),
    /// 这是一个合法的 PROXY 头。
    Done {
        /// 真实客户端地址。
        ///
        /// ⚠ ⚠ **`None` 是正常结果，不是失败**：`LOCAL` 命令与 `PROXY UNKNOWN`
        /// 都表示「没有真实客户端」（健康检查就长这样）。
        /// 调用方此时应当**继续用 socket 对端**，而不是断开连接。
        client: Option<SocketAddr>,
        /// 这个头占了多少字节 —— 调用方要从应用流里把它们吃掉。
        consumed: usize,
    },
    /// 不是 PROXY 头，或者是一个坏头。
    ///
    /// ★ 带一句人话的理由：这条会被记进日志，而「解析失败」四个字帮不了任何人。
    Invalid(&'static str),
}

/// 喂进已经读到的字节，问「这是不是一个 PROXY 头」。
///
/// ★ ★ **它是纯函数**：同样的输入永远给同样的答案，调用方可以在每次 `read` 之后
/// 原样再问一遍。⇒ 不需要在解析器里维护任何状态。
pub fn decode(buf: &[u8]) -> Verdict {
    // ── 先判是 v2 还是 v1 ────────────────────────────────────────────────
    //
    // ★ 判别只看前缀，而两种前缀**互相不可能是对方的前缀**（v2 签名第一个字节是
    //   0x0D，v1 是 'P'）⇒ 一个字节就能分岔，不存在「读多了才知道猜错」。
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

// ── v1：一行文本 ──────────────────────────────────────────────────────────

fn decode_v1(buf: &[u8]) -> Verdict {
    // 前缀还没读全。
    let n = buf.len().min(V1_SIG.len());
    if buf[..n] != V1_SIG[..n] {
        return Verdict::Invalid("v1 前缀不是 `PROXY `");
    }
    if buf.len() < V1_SIG.len() {
        return Verdict::Need(V1_SIG.len() - buf.len());
    }

    // ★ 找行尾。⚠ 规格要求**必须**是 `\r\n`，单独一个 `\n` 不算 ——
    //   不较这个真的话，一个「差不多对」的发送方会在两端产生不同的消费长度。
    let end = match find_crlf(buf) {
        Some(i) => i,
        None => {
            // 还没找到行尾：只要还没超上限，就继续要字节。
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

    // `PROXY ` 之后到 CRLF 之前。
    let line = &buf[V1_SIG.len()..end];
    let line = match std::str::from_utf8(line) {
        Ok(s) => s,
        Err(_) => return Verdict::Invalid("v1 行里有非 ASCII 字节"),
    };

    // ⚠ 用 `split(' ')` 而不是 `split_whitespace()`：后者会把连续空格、制表符
    //   都当分隔符，于是 `PROXY  TCP4 …`（两个空格）会被我们接受而被别人拒绝。
    //   ★ 在这种「两端必须逐字节同意」的协议上，宽容就是分歧。
    let mut parts = line.split(' ');
    let proto = parts.next().unwrap_or("");

    // `PROXY UNKNOWN` —— 合法，但没有真实客户端。
    //
    // ⚠ 规格允许 UNKNOWN 后面跟任意内容（发送方可以把它知道的东西写上去），
    //   接收方**必须忽略到行尾** ⇒ 这里不校验剩下的部分。
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
    // `dst` 与 `dport` 规格要求存在且合法，但我们只用 src ——
    // ★ 仍然**校验**它们：一个我们不看的字段如果是垃圾，说明这个头整体不可信。
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
    // ⚠ `"0080".parse::<u16>()` 是 80，而规格要求端口**不带前导零**。
    //   ★ 两端对同一个头算出不同的字节数是这类协议最坏的错法，所以这里较真。
    if sport.len() > 1 && sport.starts_with('0') {
        return Verdict::Invalid("v1 的源端口有前导零");
    }

    Verdict::Done {
        client: Some(SocketAddr::new(ip, port)),
        consumed,
    }
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    // ★ 有意不用正则也不用 memchr：两个字节的固定串，写清楚比引一个依赖便宜。
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
    // ⚠ ⚠ **必须按声明的族分别解析，不能只 `s.parse::<IpAddr>()`**：
    //   后者会让 `PROXY TCP4 ::1 …` 通过，而那是一个自相矛盾的头。
    //   ★ 一个自相矛盾的头说明发送方与我们对这条连接的理解不一致，
    //     此时「宽容地接受」等于把不一致带进下游。
    if want_v6 {
        s.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
    } else {
        s.parse::<Ipv4Addr>().ok().map(IpAddr::V4)
    }
}

// ── v2：二进制 ────────────────────────────────────────────────────────────

fn decode_v2(buf: &[u8]) -> Verdict {
    let n = buf.len().min(V2_SIG.len());
    if buf[..n] != V2_SIG[..n] {
        return Verdict::Invalid("v2 签名对不上");
    }
    if buf.len() < V2_FIXED {
        return Verdict::Need(V2_FIXED - buf.len());
    }

    let ver_cmd = buf[12];
    // 高 4 位是版本，必须是 2。
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
    // 0 = LOCAL：连接由代理自己发起（健康检查）⇒ **没有真实客户端**。
    // 1 = PROXY：头里带着真实客户端。
    match cmd {
        0 => {
            // ⚠ ⚠ LOCAL 时**规格要求接收方忽略地址部分**（哪怕它填了东西）。
            //   ★ 照 `len` 把它跳过去仍然是必须的 —— 忽略的是内容，不是长度。
            return Verdict::Done {
                client: None,
                consumed,
            };
        }
        1 => {}
        _ => return Verdict::Invalid("v2 头里的命令既不是 LOCAL 也不是 PROXY"),
    }

    // ── 地址族与协议 ────────────────────────────────────────────────────
    //
    // 高 4 位：0=UNSPEC 1=INET 2=INET6 3=UNIX；低 4 位：0=UNSPEC 1=STREAM 2=DGRAM。
    let payload = &buf[V2_FIXED..consumed];
    let client = match fam >> 4 {
        0x1 => {
            // IPv4：4 + 4 + 2 + 2 = 12 字节
            if payload.len() < 12 {
                return Verdict::Invalid("v2 声明的是 IPv4，但 payload 不足 12 字节");
            }
            let src = Ipv4Addr::new(payload[0], payload[1], payload[2], payload[3]);
            let port = u16::from_be_bytes([payload[8], payload[9]]);
            Some(SocketAddr::new(IpAddr::V4(src), port))
        }
        0x2 => {
            // IPv6：16 + 16 + 2 + 2 = 36 字节
            if payload.len() < 36 {
                return Verdict::Invalid("v2 声明的是 IPv6，但 payload 不足 36 字节");
            }
            let mut o = [0u8; 16];
            o.copy_from_slice(&payload[0..16]);
            let port = u16::from_be_bytes([payload[32], payload[33]]);
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), port))
        }
        // UNSPEC(0) 与 UNIX(3)：**都不是错误**，只是没有我们用得上的地址。
        //
        // ⚠ UNSPEC 是规格建议的「发送方也不知道」的写法，UNIX 是本机 socket。
        //   ★ 两者都按「照 len 吃掉、客户端仍用 socket 对端」处理 ——
        //     与 LOCAL 同一条口径。
        0x0 | 0x3 => None,
        _ => return Verdict::Invalid("v2 头里的地址族不认得"),
    };

    Verdict::Done { client, consumed }
}

// ── 编码（发那一侧）───────────────────────────────────────────────────────

/// 要写哪个版本。**默认 v2**（owner 拍板）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// 文本，人眼可读，抓包好看。
    V1,
    /// 二进制，定长头。**默认**。
    V2,
}

impl Default for Version {
    fn default() -> Self {
        // ★ ★ **有意不写 `#[derive(Default)]` + `#[default] V2`。**
        //   默认值只有一份，住在 `fulcrum_config::model::DEFAULT_PROXY_PROTOCOL`
        //   （DSL 里 `proxy_protocol` 省略参数时填的也是它）——
        //   ⚠ 两处各写一个 `V2` 的代价很具体：将来改默认版本时改漏一处，
        //   现场表现是「省略参数与显式写 v2 行为不同」，没有任何一道门会说出来。
        //   ★ `unwrap_or` 那一支由下面 `默认值那个常量必须是_parse_认得的词` 钉住。
        Version::parse(fulcrum_config::model::DEFAULT_PROXY_PROTOCOL).unwrap_or(Version::V2)
    }
}

impl Version {
    /// DSL 里写的那个词。
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

/// 造一个要发给上游的 PROXY 头。
///
/// - `client`：**真实客户端** —— 如果枢衡自己也收了一个 PROXY 头，
///   这里要放**那个头里的地址**，而不是 socket 对端。★ 这就是「链式传递」，
///   也是这整件事存在的理由：中间隔几层，上游看到的仍是最初那个人。
/// - `local`：客户端连到的那个本地地址（枢衡的监听地址）。
///
/// ⚠ ⚠ **两个地址的协议族必须一致**，否则造不出合法的头。族不一致时返回
/// `LOCAL` / `UNKNOWN`（**而不是硬凑一个**）—— 上游会正确地理解成
/// 「这条连接没有可报的客户端」，而一个硬凑的头会让它相信一个假地址。
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
        // ★ 族不一致（v4 客户端连到 v6 监听器上的映射地址等）⇒ 诚实地说不知道。
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
            // ★ LOCAL + UNSPEC + 零长 payload —— 规格里「我没有可报的地址」的标准写法。
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
        // ★ 有意把头与后面的 HTTP 请求分开写，再让断言自己去算 ——
        //   一个手抄的字节数写错了，测试会红在一个与它无关的地方（例如：
        //   写了 42，而实际是 43，红出来的样子像是解析器多吃了一个字节）。
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
        // ★ 这一条最容易写错，而写错的现场表现是「每一次健康检查都断连」。
        assert_eq!(
            decode(b"PROXY UNKNOWN\r\n"),
            Verdict::Done {
                client: None,
                consumed: 15,
            }
        );
        // 规格允许 UNKNOWN 后面跟东西，接收方忽略到行尾。
        assert!(matches!(
            decode(b"PROXY UNKNOWN 1.2.3.4 5.6.7.8 1 2\r\n"),
            Verdict::Done { client: None, .. }
        ));
    }

    #[test]
    fn v1_一个字节一个字节喂也能收敛() {
        // ★ 这是它作为纯函数的意义所在：调用方每读一次就原样再问一遍。
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
        // ⚠ 不较真的话，两端会对「这个头有多长」有不同看法。
        let b = b"PROXY TCP4 1.2.3.4 5.6.7.8 1111 443\n";
        assert!(matches!(decode(b), Verdict::Need(_)));
    }

    #[test]
    fn v1_族与地址对不上要判坏() {
        // ★ 一个自相矛盾的头 ⇒ 发送方与我们的理解不一致，不许宽容地放过去。
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
        // ⚠ `split_whitespace()` 会放过它，而别的实现不会 —— 宽容就是分歧。
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
        // ★ 少了这条上界，一个只发 `PROXY ` 再也不说话的对端会让我们一直要字节。
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
        // ★ 少跳一个字节，上游收到的应用流就从中间开始 —— 现场表现是
        //   「上游偶尔报协议错误」，而枢衡这边一切正常。
        let mut h = encode(Version::V2, v4("1.2.3.4", 1111), v4("5.6.7.8", 443));
        // 把 len 从 12 改成 12+5，并追加 5 字节 TLV。
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
        // ⚠ ⚠ 这是本模块最要紧的一条：`len` 是攻击者给的 u16。
        //   ★ 判坏而不是截断 —— 截断会让我们与上游对流的起点看法不一致。
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x21, 0x11]);
        h.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(decode(&h), Verdict::Invalid(_)));
    }

    #[test]
    fn v2_声明的族与_payload_长度对不上要判坏() {
        let mut h = V2_SIG.to_vec();
        h.extend_from_slice(&[0x21, 0x21]); // 说是 IPv6
        h.extend_from_slice(&12u16.to_be_bytes()); // 却只给 12 字节
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

    // ── 判别与拒绝 ──────────────────────────────────────────────────────

    #[test]
    fn 一个普通的_http_请求当场被判成不是_proxy_头() {
        // ★ 这一条决定了「不在信任清单就一个字节都不读」那条纪律的代价有多大：
        //   即便读了，一个 HTTP 请求也是当场被拒的，不会被误当成头吃掉。
        assert!(matches!(decode(b"GET / HTTP/1.1\r\n"), Verdict::Invalid(_)));
        // TLS ClientHello 的第一个字节是 0x16。
        assert!(matches!(decode(&[0x16, 0x03, 0x01]), Verdict::Invalid(_)));
    }

    #[test]
    fn 空输入要一个字节而不是判坏() {
        assert_eq!(decode(b""), Verdict::Need(1));
    }

    #[test]
    fn v2_签名的第一个字节与_v1_不同所以一个字节就能分岔() {
        // ★ 这是「判别只看前缀」那句话的判据。
        assert_ne!(V2_SIG[0], V1_SIG[0]);
    }

    // ── 编码 ────────────────────────────────────────────────────────────

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
        // ⚠ ⚠ 硬凑会让上游相信一个假地址；说不知道则让它退回用 socket 对端。
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
        // ★ `Version::default()` 里那个 `unwrap_or` 是一条静默回退：
        //   常量被改成一个 parse 不认的词时，它会**安静地**给出 V2。
        //   ⇒ 这条把那个静默变成一次当场的红。
        assert!(
            Version::parse(fulcrum_config::model::DEFAULT_PROXY_PROTOCOL).is_some(),
            "DEFAULT_PROXY_PROTOCOL 是 `{}`，而 Version::parse 不认得它",
            fulcrum_config::model::DEFAULT_PROXY_PROTOCOL
        );
        // ★ 另一半：`PROXY_PROTOCOL_VERSIONS` 里每一个词也都必须解析得出来 ——
        //   否则 DSL 的编译期校验会放过一个运行时用不了的版本。
        for v in fulcrum_config::model::PROXY_PROTOCOL_VERSIONS {
            assert!(
                Version::parse(v).is_some(),
                "DSL 允许写 `{v}`，而 Version::parse 不认得它"
            );
        }
    }

    #[test]
    fn 上限是一个具体的数而不是无界() {
        // ★ 调用方按 MAX_HEADER 设读缓冲；它变了这条会红，提醒去看调用方。
        assert_eq!(MAX_HEADER, 16 + 1024);
    }
}
