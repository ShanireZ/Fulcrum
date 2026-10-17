//! HTTP 面的 PROXY protocol —— **「收」那半邊**（M2 批 L 第 ① 步）。
//!
//! # 它是什麼
//!
//! 全域塊的 `proxy_protocol_from <网段…>` 宣告「**這些網段的流量是經代理來的**」。
//! 落到這裡，就是：這條連接的對端在清單裡時，**在 TLS 握手之前**讀掉它的 PROXY 頭，
//! 並把這條連接的「對端位址」換成頭裡報的那個真實用戶端。
//!
//! ⇒ 於是 `remote_ip` 匹配器與（批 L 第 ② 步的）訪問紀錄拿到的是**真實用戶端**，
//! 而不是那臺 LB 自己的位址。
//!
//! # ★ ★ 本模組只有「串接」，沒有「解析」，也沒有「判斷」
//!
//! | 誰 | 幹什麼 |
//! |---|---|
//! | [`fulcrum_runtime::proxyproto`] | v1/v2 **編解碼**（純邏輯，28 條單測）|
//! | [`fulcrum_runtime::Runtime::trusts_proxy_protocol`] | **唯一**的信任判斷入口 |
//! | `pingora_core::listeners`（fork 改動 12，形狀 B）| 只做一件事：上游的 pre-TLS 回呼在**明文連接埠也調** |
//! | **本模組** | 實作上游的 `PreTlsProcess`：循環讀 + 把多讀的位元組還回去 + 覆蓋位址，**一行協定邏輯都沒有** |
//!
//! ★ 2026-09-24（pingora 0.9.0）起，循環讀與覆蓋位址從 fork 搬到了這裡：上游 0.9.0 自己加了
//!   `PreTlsProcess` 接縫，只是只在 TLS 分支裡調 ⇒ fork 只剩「挪到 TLS 分支之前」那幾行，見 FORK.md §12。
//!
//! # ⚠ ⚠ 一條會被讀錯的語義：**清單內的來源不發頭 ⇒ 關連接**
//!
//! 那不是缺陷，是 owner 拍板的口徑（§10）：若允許清單內的來源**選擇性地**
//! 不發頭，它就能讓樞衡改用 socket 對端 —— 而那個位址正是 LB 自己，
//! 於是一條 `remote_ip 10.0.0.0/8` 規則會**命中它**。
//! ⇒ 「可選」把一個顯式的信任宣告，變成一個**可以被對端單方面關掉的開關**。

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

/// 掛到監聽器上的策略。
///
/// # ★ ★ ★ 為什麼拿的是 `SharedRuntime` 而不是一份 `Vec<Cidr>` 快照
///
/// 快照會在**裝載時**定死，於是改了 `proxy_protocol_from` 再 `POST /load`
/// **不生效** —— 而設定檔案上完全看不出來。
/// ⚠ 那正是 **D19** 那個形狀（`cache { capacity }` 改了要重新啟動）—— 它已由 **G135** 結案。
/// ⛔ **這條交叉引用有意留著**：結案的是那一個執行個體，**形狀本身照舊會復發** ——
/// 「裝載時定死一份快照」這種寫法在任何新程式碼裡都會長回同一個樣子。
/// ⇒ 這裡每條連接現讀一次當前快照，**換設定立刻生效**，不新欠一條 D 號。
///
/// ★ 代價寫在明處：每條**新連接**多一次 `RwLock` 讀鎖 + 一次 `Arc` 克隆。
/// 它落在 accept 之後、TLS 握手之前那一步上，而那一步本來就要做幾十微秒的密碼學。
pub(crate) struct HttpProxyProtocol {
    rt: Arc<SharedRuntime>,
}

impl HttpProxyProtocol {
    pub(crate) fn new(rt: Arc<SharedRuntime>) -> Arc<HttpProxyProtocol> {
        Arc::new(HttpProxyProtocol { rt })
    }
}

impl std::fmt::Debug for HttpProxyProtocol {
    /// ⚠ 手寫而不是 `derive`：`SharedRuntime` 裡是整張執行環境圖，
    /// 把它打進紀錄既沒用又可能帶出設定內容（安全基線：設定預覽必須脫敏）。
    /// ★ 打出來的是**當前清單有幾條** —— 那才是疑難排解時想知道的。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HttpProxyProtocol({} 个受信网段)",
            self.rt.current().proxy_protocol_from.len()
        )
    }
}

impl HttpProxyProtocol {
    /// 這個對端在信任清單裡嗎？
    ///
    /// ⚠ ⚠ `peer` 為 `None`（拿不到 inet 對端，例如 Unix domain socket）⇒ **不信任**。
    ///
    /// ★ 與「清單為空時恆 false」同一條紀律：**拿不到證據不等於證據成立**。
    pub(crate) fn trusts(&self, peer: Option<&std::net::SocketAddr>) -> bool {
        match peer {
            Some(a) => self.rt.current().trusts_proxy_protocol(a.ip()),
            None => false,
        }
    }
}

/// 本模組的錯誤類型（原來在 fork 裡，隨讀取邏輯一起搬過來）。
const PROXY_PROTOCOL_ERR: ErrorType = ErrorType::Custom("ProxyProtocolError");

/// 讀 PROXY 頭時緩衝區的**硬上界**。
///
/// ★ 它不是協定上界（那由 [`proxyproto::decode`] 自己的 `Invalid` 給：v1 ≤ 107 位元組，
/// v2 ≤ [`proxyproto::MAX_HEADER`]），而是**這個循環一定會停**的最後一道保證。
/// ⚠ 經真實解碼器走不到（`MAX_HEADER` 遠小於它）—— 留著是防線，⛔ 不是判據。
const PROXY_PROTOCOL_HARD_CAP: usize = 4096;

#[async_trait]
impl PreTlsProcess for HttpProxyProtocol {
    /// 在 TLS 握手之前（明文連接埠：在 HTTP 解析之前）讀掉一個 PROXY 頭，並把對端位址換成它報的那個。
    ///
    /// 回傳 `Err` = **關掉這條連接**。⚠ 這與「不在清單裡」有意相反：一個**在信任清單裡**的
    /// 對端發來壞頭（或乾脆不發完），說明設定或對端出了問題，而此時我們**已經吃掉了一部分位元組、
    /// 還原不回去** —— 把殘缺的流交給上層只會把問題推遠。
    async fn process(&self, stream: &mut L4Stream) -> pingora_core::Result<()> {
        let peer = stream
            .get_socket_digest()
            .and_then(|d| d.peer_addr().and_then(|a| a.as_inet().copied()));
        if !self.trusts(peer.as_ref()) {
            // ★ ★ 一個位元組都不讀，原樣交給上層。
            return Ok(());
        }

        let mut buf: Vec<u8> = Vec::with_capacity(64);
        let mut chunk = [0u8; 256];
        loop {
            // ★ 先拿已有的位元組問一次 —— `decode` 是純函式，重複問不要錢。
            match proxyproto::decode(&buf) {
                Verdict::Done { client, consumed } => {
                    // ★ ★ ★ **多讀到的位元組必須還回去。** TCP 是流，一次 read 很可能把
                    //   PROXY 頭與它後面的 ClientHello（或請求行）**一起**讀回來。
                    //   少了這一步，上層拿到的流就從半截開始 —— 而那不會有任何回報錯誤，
                    //   只表現為「TLS 握手莫名其妙失敗」。
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

            // ⚠ 這裡**故意沒有自己的超時**：pingora 的 `services/listening.rs` 已經把整個
            //   `handshake()` 包在一個 60s 的 timeout 裡，本回呼就在 `handshake()` 裡被調。
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

/// 把這條連接的對端位址換成 PROXY 頭報的那個。
///
/// # ⚠ ⚠ ★ 為什麼是「換一整份 digest」而不是 `peer_addr.set(...)`
///
/// `SocketDigest.peer_addr` 是 `pub` 的 `OnceCell`，看起來 `set()` 一下就行 ——
/// **而那行不通，因為它已經被填過了**：pingora 的 `services/listening.rs` 在
/// `io.handshake()` **之前**調 `io.peer_addr()`（為了握手失敗時那行紀錄裡能帶上位址），
/// 而 `SocketDigest::peer_addr()` 是 `get_or_init`。
/// ⇒ `OnceCell::set()` 到這一步必然回傳 `Err`，**而它的回傳值很容易被忽略**，
///    於是「位址沒換成」會是一次完全無聲的失效。
///
/// ⇒ 換一整份。`local_addr` / `original_dst` 都會從**同一個 fd** 重新惰性派生，什麼都不丟。
fn override_peer_addr(stream: &mut L4Stream, client: std::net::SocketAddr) {
    let digest = SocketDigest::from_raw_fd(stream.as_raw_fd());
    // ★ 新造的 digest，這個 `set` 一定成功；寫成 `let _ =` 是因為回傳值確實無話可說。
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
        // ★ ★ 一份空清單**不是**「信任所有人」—— 這條如果反了，
        //   任何人都能自稱是任意 IP，而 `remote_ip` 匹配器會當真。
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
        // ⚠ 連 `0.0.0.0/0` 都不該讓「拿不到對端」變成信任 ——
        //   那是 UDS 之類的形態，它根本沒有 inet 對端可比。
        assert!(!p.trusts(None), "拿不到 inet 对端时必须不信任");
    }

    #[test]
    fn 换配置立刻生效_不需要重启() {
        let shared = rt_with("");
        let p = HttpProxyProtocol::new(shared.clone());
        let a: std::net::SocketAddr = "10.1.2.3:1234".parse().unwrap();
        assert!(!p.trusts(Some(&a)), "换之前不该信任");

        // ★ ★ ★ 這一條守的是 D19 那個形狀：**改了設定再 load 卻不生效**。
        //   ⚠ D19 本身已由 G135 結案（快取容量現在改得動），但**這條測試守的是形狀
        //   不是那個執行個體** —— 它在本檔案裡擋的是 `proxy_protocol_from`，與快取無關。
        //   若 `HttpProxyProtocol` 拿的是一份快照而不是 `SharedRuntime`，
        //   下面這次替換之後 `trusts` 仍然是 false —— 而設定檔案上看不出任何問題。
        let src =
            "{\n  proxy_protocol_from 10.0.0.0/8\n}\nhttp://a.com {\n  respond 200 \"ok\"\n}\n";
        let o = compile_str("t.Fulcrumfile", src);
        let cfg = o.config.expect("编得过");
        shared.swap(fulcrum_runtime::Runtime::build(&cfg).expect("建得出"));

        assert!(p.trusts(Some(&a)), "★ 换过配置之后必须立刻生效");
    }

    // ── `PreTlsProcess` 的實作（2026-09-24 從 fork 搬來的循環讀 + 覆蓋位址）─────────────
    //
    // ★ 夾具是**真的一對 TCP 連接**，⛔ 不是假流：「多讀的位元組還回去」（`rewind`）與
    //   「換掉對端位址」都是 pingora `L4Stream` / `SocketDigest` 自己的行為，用替身測等於沒測。
    // ★ 頭用真樣本而不是自己造的形狀 —— 夾具寫錯時，一個更寬的斷言會讓它悄悄通過。

    // `GetSocketDigest` / `SocketDigest` / `AsRawFd` / `AsyncReadExt` 經 `use super::*` 拿到。
    use tokio::io::AsyncWriteExt;

    const V1: &[u8] = b"PROXY TCP4 192.0.2.7 10.0.0.1 56324 443\r\n";
    const 后续: &[u8] = b"GET / HTTP/1.1\r\n";

    fn 信任回环() -> Arc<HttpProxyProtocol> {
        HttpProxyProtocol::new(rt_with("{\n  proxy_protocol_from 127.0.0.0/8\n}"))
    }

    /// 一對真的 TCP 連接：（伺服器端那頭的 `L4Stream`，用戶端那頭）。
    /// ★ 伺服器端那頭**自己裝上 `SocketDigest`**：監聽器路徑上 pingora 在 accept 時裝，
    ///   測試裡不裝的話 `get_socket_digest()` 是 `None`，信任判斷根本拿不到對端位址。
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

    /// `process()` 之後流裡還剩什麼：關掉用戶端，讀到 EOF。
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
        // ★ 一次寫完：頭與後續資料多半在**同一次** read 裡回來 —— 正是「多讀的必須還回去」那種情形。
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
        // ★ 清單內的來源不發完整的頭 ⇒ 關連接（owner 拍板的口徑，見模組頭）；
        //   ⛔ 不能退回用 socket 對端 —— 那個位址正是 LB 自己。
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
        // ★ `UNKNOWN` / `LOCAL` = 這條連接沒有真實用戶端 ⇒ 照舊用 socket 對端。
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
        // ★ ★ 「一個位元組都不讀」（⛔ 不是「讀掉丟棄」）：v2 頭的長度欄位由攻擊者控制，
        //   「讀掉丟棄」必須先解析那兩個位元組才知道丟多少，而「不讀」完全不碰。
        assert_eq!(
            剩下的(&mut s, c).await,
            全部,
            "清单外的对端，本模块一个字节都不该读"
        );
    }
}
