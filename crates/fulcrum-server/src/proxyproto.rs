//! HTTP 面的 PROXY protocol —— **「收」那半边**（M2 批 L 第 ① 步）。
//!
//! # 它是什么
//!
//! 全局块的 `proxy_protocol_from <网段…>` 声明「**这些网段的流量是经代理来的**」。
//! 落到这里，就是：这条连接的对端在清单里时，**在 TLS 握手之前**读掉它的 PROXY 头，
//! 并把这条连接的「对端地址」换成头里报的那个真实客户端。
//!
//! ⇒ 于是 `remote_ip` 匹配器与（批 L 第 ② 步的）访问日志拿到的是**真实客户端**，
//! 而不是那台 LB 自己的地址。
//!
//! # ★ ★ 本模块只有「接线」，没有「解析」，也没有「判断」
//!
//! | 谁 | 干什么 |
//! |---|---|
//! | [`fulcrum_runtime::proxyproto`] | v1/v2 **编解码**（纯逻辑，28 条单测）|
//! | [`fulcrum_runtime::Runtime::trusts_proxy_protocol`] | **唯一**的信任判断入口 |
//! | `pingora_core::listeners`（fork 改动 12，形状 B）| 只做一件事：上游的 pre-TLS 回调在**明文端口也调** |
//! | **本模块** | 实现上游的 `PreTlsProcess`：循环读 + 把多读的字节还回去 + 覆盖地址，**一行协议逻辑都没有** |
//!
//! ★ 2026-09-24（pingora 0.9.0）起，循环读与覆盖地址从 fork 搬到了这里：上游 0.9.0 自己加了
//!   `PreTlsProcess` 接缝，只是只在 TLS 分支里调 ⇒ fork 只剩「挪到 TLS 分支之前」那几行，见 FORK.md §12。
//!
//! # ⚠ ⚠ 一条会被读错的语义：**清单内的来源不发头 ⇒ 关连接**
//!
//! 那不是缺陷，是 owner 拍板的口径（§10）：若允许清单内的来源**选择性地**
//! 不发头，它就能让枢衡改用 socket 对端 —— 而那个地址正是 LB 自己，
//! 于是一条 `remote_ip 10.0.0.0/8` 规则会**命中它**。
//! ⇒ 「可选」把一个显式的信任声明，变成一个**可以被对端单方面关掉的开关**。

use async_trait::async_trait;
use fulcrum_runtime::SharedRuntime;
use fulcrum_runtime::proxyproto::{self, Verdict};
use pingora_core::listeners::PreTlsProcess;
use pingora_core::protocols::l4::socket::SocketAddr;
use pingora_core::protocols::l4::stream::Stream as L4Stream;
use pingora_core::protocols::{GetSocketDigest, SocketDigest};
use pingora_core::{Error, ErrorType};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

/// 挂到监听器上的策略。
///
/// # ★ ★ ★ 为什么拿的是 `SharedRuntime` 而不是一份 `Vec<Cidr>` 快照
///
/// 快照会在**装载时**定死，于是改了 `proxy_protocol_from` 再 `POST /load`
/// **不生效** —— 而配置文件上完全看不出来。
/// ⚠ 那正是 **D19** 那个形状（`cache { capacity }` 改了要重启）—— 它已由 **G135** 结案。
/// ⛔ **这条交叉引用有意留着**：结案的是那一个实例，**形状本身照旧会复发** ——
/// 「装载时定死一份快照」这种写法在任何新代码里都会长回同一个样子。
/// ⇒ 这里每条连接现读一次当前快照，**换配置立刻生效**，不新欠一条 D 号。
///
/// ★ 代价写在明处：每条**新连接**多一次 `RwLock` 读锁 + 一次 `Arc` 克隆。
/// 它落在 accept 之后、TLS 握手之前那一步上，而那一步本来就要做几十微秒的密码学。
pub(crate) struct HttpProxyProtocol {
    rt: Arc<SharedRuntime>,
}

impl HttpProxyProtocol {
    pub(crate) fn new(rt: Arc<SharedRuntime>) -> Arc<HttpProxyProtocol> {
        Arc::new(HttpProxyProtocol { rt })
    }
}

impl std::fmt::Debug for HttpProxyProtocol {
    /// ⚠ 手写而不是 `derive`：`SharedRuntime` 里是整张运行时图，
    /// 把它打进日志既没用又可能带出配置内容（安全基线：配置预览必须脱敏）。
    /// ★ 打出来的是**当前清单有几条** —— 那才是排障时想知道的。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HttpProxyProtocol({} 个受信网段)",
            self.rt.current().proxy_protocol_from.len()
        )
    }
}

impl HttpProxyProtocol {
    /// 这个对端在信任清单里吗？
    ///
    /// ⚠ ⚠ `peer` 为 `None`（拿不到 inet 对端，例如 Unix domain socket）⇒ **不信任**。
    ///
    /// ★ 与「清单为空时恒 false」同一条纪律：**拿不到证据不等于证据成立**。
    pub(crate) fn trusts(&self, peer: Option<&std::net::SocketAddr>) -> bool {
        match peer {
            Some(a) => self.rt.current().trusts_proxy_protocol(a.ip()),
            None => false,
        }
    }
}

/// 本模块的错误类型（原来在 fork 里，随读取逻辑一起搬过来）。
const PROXY_PROTOCOL_ERR: ErrorType = ErrorType::Custom("ProxyProtocolError");

/// 读 PROXY 头时缓冲区的**硬上界**。
///
/// ★ 它不是协议上界（那由 [`proxyproto::decode`] 自己的 `Invalid` 给：v1 ≤ 107 字节，
/// v2 ≤ [`proxyproto::MAX_HEADER`]），而是**这个循环一定会停**的最后一道保证。
/// ⚠ 经真实解码器走不到（`MAX_HEADER` 远小于它）—— 留着是防线，⛔ 不是判据。
const PROXY_PROTOCOL_HARD_CAP: usize = 4096;

#[async_trait]
impl PreTlsProcess for HttpProxyProtocol {
    /// 在 TLS 握手之前（明文端口：在 HTTP 解析之前）读掉一个 PROXY 头，并把对端地址换成它报的那个。
    ///
    /// 返回 `Err` = **关掉这条连接**。⚠ 这与「不在清单里」有意相反：一个**在信任清单里**的
    /// 对端发来坏头（或干脆不发完），说明配置或对端出了问题，而此时我们**已经吃掉了一部分字节、
    /// 还原不回去** —— 把残缺的流交给上层只会把问题推远。
    async fn process(&self, stream: &mut L4Stream) -> pingora_core::Result<()> {
        let peer = stream
            .get_socket_digest()
            .and_then(|d| d.peer_addr().and_then(|a| a.as_inet().copied()));
        if !self.trusts(peer.as_ref()) {
            // ★ ★ 一个字节都不读，原样交给上层。
            return Ok(());
        }

        let mut buf: Vec<u8> = Vec::with_capacity(64);
        let mut chunk = [0u8; 256];
        loop {
            // ★ 先拿已有的字节问一次 —— `decode` 是纯函数，重复问不要钱。
            match proxyproto::decode(&buf) {
                Verdict::Done { client, consumed } => {
                    // ★ ★ ★ **多读到的字节必须还回去。** TCP 是流，一次 read 很可能把
                    //   PROXY 头与它后面的 ClientHello（或请求行）**一起**读回来。
                    //   少了这一步，上层拿到的流就从半截开始 —— 而那不会有任何报错，
                    //   只表现为「TLS 握手莫名其妙失败」。
                    if consumed < buf.len() {
                        stream.rewind(&buf[consumed..]);
                    }
                    if let Some(addr) = client {
                        override_peer_addr(stream, addr);
                    }
                    return Ok(());
                }
                Verdict::Invalid(why) => {
                    return Error::e_explain(
                        PROXY_PROTOCOL_ERR,
                        format!("bad PROXY header: {why}"),
                    );
                }
                Verdict::Need(_) => {}
            }

            if buf.len() >= PROXY_PROTOCOL_HARD_CAP {
                return Error::e_explain(
                    PROXY_PROTOCOL_ERR,
                    format!("PROXY header exceeded {PROXY_PROTOCOL_HARD_CAP} bytes"),
                );
            }

            // ⚠ 这里**故意没有自己的超时**：pingora 的 `services/listening.rs` 已经把整个
            //   `handshake()` 包在一个 60s 的 timeout 里，本回调就在 `handshake()` 里被调。
            match stream.read(&mut chunk).await {
                Ok(0) => {
                    return Error::e_explain(
                        PROXY_PROTOCOL_ERR,
                        "peer closed the connection while we waited for its PROXY header",
                    );
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => {
                    return Error::e_explain(PROXY_PROTOCOL_ERR, format!("read failed: {e}"));
                }
            }
        }
    }
}

/// 把这条连接的对端地址换成 PROXY 头报的那个。
///
/// # ⚠ ⚠ ★ 为什么是「换一整份 digest」而不是 `peer_addr.set(...)`
///
/// `SocketDigest.peer_addr` 是 `pub` 的 `OnceCell`，看起来 `set()` 一下就行 ——
/// **而那行不通，因为它已经被填过了**：pingora 的 `services/listening.rs` 在
/// `io.handshake()` **之前**调 `io.peer_addr()`（为了握手失败时那行日志里能带上地址），
/// 而 `SocketDigest::peer_addr()` 是 `get_or_init`。
/// ⇒ `OnceCell::set()` 到这一步必然返回 `Err`，**而它的返回值很容易被忽略**，
///    于是「地址没换成」会是一次完全无声的失效。
///
/// ⇒ 换一整份。`local_addr` / `original_dst` 都会从**同一个 fd** 重新惰性派生，什么都不丢。
fn override_peer_addr(stream: &mut L4Stream, client: std::net::SocketAddr) {
    let digest = SocketDigest::from_raw_fd(stream.as_raw_fd());
    // ★ 新造的 digest，这个 `set` 一定成功；写成 `let _ =` 是因为返回值确实无话可说。
    let _ = digest.peer_addr.set(Some(SocketAddr::Inet(client)));
    stream.set_socket_digest(digest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulcrum_config::compile_str;

    fn rt_with(global: &str) -> Arc<SharedRuntime> {
        let src = format!("{global}\nhttp://a.com {{\n  respond 200 \"ok\"\n}}\n");
        let o = compile_str("t.Fulcrumfile", &src);
        let diags = o.render_diagnostics();
        let cfg = o.config.unwrap_or_else(|| panic!("编不过：\n{diags}"));
        let rt = fulcrum_runtime::Runtime::build(&cfg).expect("建不出运行时图");
        SharedRuntime::new(Arc::new(rt))
    }

    #[test]
    fn 清单为空时谁都不信() {
        let p = HttpProxyProtocol::new(rt_with(""));
        let a: std::net::SocketAddr = "10.0.0.5:1234".parse().unwrap();
        // ★ ★ 一份空清单**不是**「信任所有人」—— 这条如果反了，
        //   任何人都能自称是任意 IP，而 `remote_ip` 匹配器会当真。
        assert!(!p.trusts(Some(&a)), "空清单必须谁都不信");
    }

    #[test]
    fn 只信清单里的网段() {
        let p = HttpProxyProtocol::new(rt_with("{\n  proxy_protocol_from 10.0.0.0/8\n}"));
        let inside: std::net::SocketAddr = "10.1.2.3:1234".parse().unwrap();
        let outside: std::net::SocketAddr = "192.168.1.1:1234".parse().unwrap();
        assert!(p.trusts(Some(&inside)), "清单内的应当被信任");
        assert!(!p.trusts(Some(&outside)), "清单外的不该被信任");
    }

    #[test]
    fn 拿不到对端地址时不信任() {
        let p = HttpProxyProtocol::new(rt_with("{\n  proxy_protocol_from 0.0.0.0/0\n}"));
        // ⚠ 连 `0.0.0.0/0` 都不该让「拿不到对端」变成信任 ——
        //   那是 UDS 之类的形态，它根本没有 inet 对端可比。
        assert!(!p.trusts(None), "拿不到 inet 对端时必须不信任");
    }

    #[test]
    fn 换配置立刻生效_不需要重启() {
        let shared = rt_with("");
        let p = HttpProxyProtocol::new(shared.clone());
        let a: std::net::SocketAddr = "10.1.2.3:1234".parse().unwrap();
        assert!(!p.trusts(Some(&a)), "换之前不该信任");

        // ★ ★ ★ 这一条守的是 D19 那个形状：**改了配置再 load 却不生效**。
        //   ⚠ D19 本身已由 G135 结案（缓存容量现在改得动），但**这条测试守的是形状
        //   不是那个实例** —— 它在本文件里挡的是 `proxy_protocol_from`，与缓存无关。
        //   若 `HttpProxyProtocol` 拿的是一份快照而不是 `SharedRuntime`，
        //   下面这次替换之后 `trusts` 仍然是 false —— 而配置文件上看不出任何问题。
        let src =
            "{\n  proxy_protocol_from 10.0.0.0/8\n}\nhttp://a.com {\n  respond 200 \"ok\"\n}\n";
        let o = compile_str("t.Fulcrumfile", src);
        let cfg = o.config.expect("编得过");
        shared.swap(fulcrum_runtime::Runtime::build(&cfg).expect("建得出"));

        assert!(p.trusts(Some(&a)), "★ 换过配置之后必须立刻生效");
    }

    // ── `PreTlsProcess` 的实现（2026-09-24 从 fork 搬来的循环读 + 覆盖地址）─────────────
    //
    // ★ 夹具是**真的一对 TCP 连接**，⛔ 不是假流：「多读的字节还回去」（`rewind`）与
    //   「换掉对端地址」都是 pingora `L4Stream` / `SocketDigest` 自己的行为，用替身测等于没测。
    // ★ 头用真样本而不是自己造的形状 —— 夹具写错时，一个更宽的断言会让它悄悄通过。

    // `GetSocketDigest` / `SocketDigest` / `AsRawFd` / `AsyncReadExt` 经 `use super::*` 拿到。
    use tokio::io::AsyncWriteExt;

    const V1: &[u8] = b"PROXY TCP4 192.0.2.7 10.0.0.1 56324 443\r\n";
    const 后续: &[u8] = b"GET / HTTP/1.1\r\n";

    fn 信任回环() -> Arc<HttpProxyProtocol> {
        HttpProxyProtocol::new(rt_with("{\n  proxy_protocol_from 127.0.0.0/8\n}"))
    }

    /// 一对真的 TCP 连接：（服务端那头的 `L4Stream`，客户端那头）。
    /// ★ 服务端那头**自己装上 `SocketDigest`**：监听器路径上 pingora 在 accept 时装，
    ///   测试里不装的话 `get_socket_digest()` 是 `None`，信任判断根本拿不到对端地址。
    async fn 一对连接() -> (L4Stream, tokio::net::TcpStream) {
        let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(ln.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = ln.accept().await.unwrap();
        let fd = server.as_raw_fd();
        let mut s = L4Stream::from(server);
        s.set_socket_digest(SocketDigest::from_raw_fd(fd));
        (s, client)
    }

    fn 对端(s: &L4Stream) -> Option<std::net::SocketAddr> {
        s.get_socket_digest()
            .and_then(|d| d.peer_addr().and_then(|a| a.as_inet().copied()))
    }

    /// `process()` 之后流里还剩什么：关掉客户端，读到 EOF。
    async fn 剩下的(s: &mut L4Stream, client: tokio::net::TcpStream) -> Vec<u8> {
        drop(client);
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn 清单内的对端_读掉头_换掉对端地址_后面的字节一个不少() {
        let p = 信任回环();
        let (mut s, mut c) = 一对连接().await;
        // ★ 一次写完：头与后续数据多半在**同一次** read 里回来 —— 正是「多读的必须还回去」那种情形。
        c.write_all(&[V1, 后续].concat()).await.unwrap();
        p.process(&mut s).await.expect("合法的头不该关连接");
        assert_eq!(
            对端(&s),
            Some("192.0.2.7:56324".parse().unwrap()),
            "对端地址没换成头里报的那个"
        );
        assert_eq!(
            剩下的(&mut s, c).await,
            后续,
            "头后面的字节必须原样还回去（多读的要 rewind）"
        );
    }

    #[tokio::test]
    async fn 头分两次到达也能读完() {
        let p = 信任回环();
        let (mut s, mut c) = 一对连接().await;
        let (前半, 后半) = V1.split_at(15);
        let 写 = async {
            c.write_all(前半).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            c.write_all(&[后半, 后续].concat()).await.unwrap();
            c
        };
        let (结果, c) = tokio::join!(p.process(&mut s), 写);
        结果.expect("分两次到达的合法头不该关连接");
        assert_eq!(对端(&s), Some("192.0.2.7:56324".parse().unwrap()));
        assert_eq!(剩下的(&mut s, c).await, 后续);
    }

    #[tokio::test]
    async fn 清单内的对端发坏头_关连接() {
        let p = 信任回环();
        let (mut s, mut c) = 一对连接().await;
        c.write_all(b"PROXY TCP4 not-an-ip 10.0.0.1 1 2\r\n")
            .await
            .unwrap();
        assert!(p.process(&mut s).await.is_err(), "坏头必须关连接");
    }

    #[tokio::test]
    async fn 清单内的对端没发完就断开_关连接() {
        let p = 信任回环();
        let (mut s, mut c) = 一对连接().await;
        c.write_all(b"PROXY TCP4 ").await.unwrap();
        c.shutdown().await.unwrap();
        // ★ 清单内的来源不发完整的头 ⇒ 关连接（owner 拍板的口径，见模块头）；
        //   ⛔ 不能退回用 socket 对端 —— 那个地址正是 LB 自己。
        assert!(p.process(&mut s).await.is_err(), "头没发完就断开必须关连接");
    }

    #[tokio::test]
    async fn unknown_头不换对端地址_后面的字节照样还回去() {
        let p = 信任回环();
        let (mut s, mut c) = 一对连接().await;
        let 客户端地址 = c.local_addr().unwrap();
        c.write_all(&[b"PROXY UNKNOWN\r\n".as_slice(), 后续].concat())
            .await
            .unwrap();
        p.process(&mut s).await.expect("PROXY UNKNOWN 是合法的头");
        // ★ `UNKNOWN` / `LOCAL` = 这条连接没有真实客户端 ⇒ 照旧用 socket 对端。
        assert_eq!(对端(&s), Some(客户端地址), "UNKNOWN 头不该动对端地址");
        assert_eq!(剩下的(&mut s, c).await, 后续, "UNKNOWN 头本身也要被读掉");
    }

    #[tokio::test]
    async fn 清单外的对端_一个字节都不读() {
        let p = HttpProxyProtocol::new(rt_with(""));
        let (mut s, mut c) = 一对连接().await;
        let 客户端地址 = c.local_addr().unwrap();
        let 全部 = [V1, 后续].concat();
        c.write_all(&全部).await.unwrap();
        p.process(&mut s).await.expect("清单外的对端不该被关");
        assert_eq!(对端(&s), Some(客户端地址), "清单外的对端，地址必须原样不动");
        // ★ ★ 「一个字节都不读」（⛔ 不是「读掉丢弃」）：v2 头的长度字段由攻击者控制，
        //   「读掉丢弃」必须先解析那两个字节才知道丢多少，而「不读」完全不碰。
        assert_eq!(
            剩下的(&mut s, c).await,
            全部,
            "清单外的对端，本模块一个字节都不该读"
        );
    }
}
