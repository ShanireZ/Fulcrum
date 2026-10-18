//! L4 面：TCP 與 UDP 透傳，都是自研。
//!
//! # 它與 HTTP 那一側的分工
//!
//! | | HTTP 資料面 | 這裡 |
//! |---|---|---|
//! | 入口 | Pingora 的 `ListeningService` | **自建 `Service`**，直接用 `tokio::net::TcpListener` |
//! | 路由 | 站台索引（Host + 連接埠）| **沒有路由**——一條 TCP 連接上沒有 Host、沒有路徑 |
//! | 挑上游 | [`ProxyTarget::pick`] | [`ProxyTarget::pick_by`]（**同一份實作**，見下） |
//! | 改內容 | `rewrite` / `header_up` / … | **一個位元組都不看也不改** |
//!
//! ★ **挑上游那一格是共用的，不是抄的**：`pick` 拆成「取用戶端 IP」+ `pick_by`，
//! L4 走後者 ⇒ 篩子與四種 `lb_policy` 只有一份實作。⚠ 抄一份的代價不在今天，
//! 兩份在**下一次改動**時分家。
//!
//! # ★ ★ 自建監聽器必須參與 socket 移交
//!
//! 表裡有這個鍵就繼承，沒有就自己 bind **再放回表裡**。⚠ 少了「放回去」那一步，
//! 第一次升級時這個連接埠會重新 bind，那一刻所有 L4 長連接一起斷 —— 而 HTTP 一切正常。
//!
//! # 有意沒有的東西
//!
//! - **`lb_policy` 之外的旋鈕**：UDP 空閒超時與並行上限、TCP 連上游超時都是常數。
//!   ⏳ 讓它們可配要先給 `l4` 塊加選項位置。
//! - **TCP 的空閒超時**：⚠ L4 的典型物件是資料庫連接，空閒幾小時是正常的；
//!   給一個「看起來安全」的 30 秒，等於去殺一條上游認為健康的連接，
//!   現場表現是「資料庫偶爾斷線」。⇒ 空閒由兩端自己管。

use fulcrum_runtime::SharedRuntime;
use fulcrum_tls::alpn_list;
use log::{debug, error, info, warn};
use pingora_boringssl::ssl::{
    ErrorCode, ExtensionType, NameType, SelectCertError, Ssl, SslContext, SslContextBuilder,
    SslMethod, SslStream,
};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::net::SocketAddr;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use async_trait::async_trait;
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;

/// 連上游的超時。
///
/// ★ 它與「空閒超時」是兩件事：**建連接**卡住是上游出了問題（半死的機器、
/// 被丟包的路徑），而卡住期間用戶端只會覺得「這個連接埠沒反應」。
/// ⚠ 沒有這條時，一個 SYN 被靜默丟棄的上游會讓每條連接掛到系統預設的
/// TCP 重傳耗盡為止（Linux 上約 130 秒），而輪詢會**繼續往它身上送**。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 一個 `l4 tcp` 監聽器對應的服務。
pub struct TcpProxyService {
    /// 這個 service 用幾個執行緒（**G35 / G140**）。`None` = 跟全域 `conf.threads`。
    ///
    /// ★ 與 pingora `ListeningService.threads` 同名同義 —— 自建 service 也參與
    /// 同一套角色分配，⛔ 不許在這裡另立一套。由 `serve()` 在建好之後設。
    pub threads: Option<usize>,
    /// ★ 拿的是**共享**執行環境，不是啟動時那一份：`POST /load` 換設定之後，
    /// 下一條連接就該按新的上游走。⚠ 監聽位址本身換不了（啟動時綁定），
    /// 管理面為此把 L4 連接埠納入了「連接埠集變了就 409」的判據。
    shared: Arc<SharedRuntime>,
    /// 設定裡寫的監聽位址原樣（`:3306`），**在當前設定裡找回自己**用的鍵。
    listen: String,
    /// 真正 bind 的位址（`--bind-host` 補齊之後）。
    bind: String,
    /// fd 表裡的鍵。★ 加前綴是為了與 Pingora 原生服務的裸 `addr:port` 分開 ——
    /// 撞鍵的後果是兩個服務搶同一個 fd，而症狀會出現在**另一個**服務上。
    fd_key: String,
    /// 本處理程序是不是以 `-u` 起來的。★ 只有知道這一點，才能把「fd 表裡沒有」
    /// 分成「首次啟動，正常」與「升級時交接失敗，必須回報錯誤」兩種。
    upgrading: bool,
    name: String,
    /// 這個監聽器的連接計數格（**M2 批 O**）。★ 與 HTTP 那一側**共用同一個** `ConnGuard`。
    conn: crate::conn_stats::BoundConn,
}

impl TcpProxyService {
    pub fn new(
        shared: Arc<SharedRuntime>,
        listen: &str,
        bind: String,
        upgrading: bool,
        conn_reg: Arc<crate::conn_stats::ConnRegistry>,
    ) -> Self {
        // ★ `bind()` 順手**宣告**這一格 ⇒ 這個監聽器從第一秒起就有一條 `0` 的樣本，
        //   而不是「有連接了才出現」。
        let conn = conn_reg.bind(crate::conn_stats::Entrypoint::L4Tcp, &bind);
        Self {
            // ★ 預設 `None` = 跟全域；由 `serve()` 按角色設（G140）。
            threads: None,
            shared,
            listen: listen.to_string(),
            fd_key: format!("fulcrum-l4-tcp:{bind}"),
            name: format!("fulcrum-l4-tcp-{bind}"),
            bind,
            upgrading,
            conn,
        }
    }
}

/// 取 fd（繼承）或新建監聽器，並把新建的那個放回表裡供下一代繼承。
///
/// ⚠ ⚠ 這段的每一條註解都在描述一個**已經發生過**的失效，別刪。
async fn build_listener(
    bind: &str,
    key: &str,
    fds: Option<ListenFds>,
    upgrading: bool,
) -> std::io::Result<TcpListener> {
    let Some(table) = fds else {
        // ★ 表缺失時**不能裝作沒事**：這條路徑下 bind 出來的監聽器不會被註冊，
        //   於是它不參與下一次 socket 移交 —— 升級時連接被重置，而且悄無聲息。
        if upgrading {
            return Err(std::io::Error::other(format!(
                "以 -u 升级启动却拿不到 fd 表，无法继承 {bind}：socket 移交已经失败了"
            )));
        }
        warn!("[l4] 没有 fd 表，只能在 {bind} 上新 bind —— ★ 这个监听器不会参与下一次 socket 移交");
        return TcpListener::bind(bind).await;
    };

    // ★ 0.9.0（上游 1d93711）：ListenFds 換成 parking_lot 鎖 ⇒ 守衛不 Send、⛔ 不許跨 `.await`。
    //   只在兩個同步的瞬間持鎖（查表、登記）；bind 期間不持鎖 —— 每個服務的鍵獨佔，不需要按位址串行。
    let inherited = table.lock().get(key).copied();

    if let Some(fd) = inherited {
        info!("[l4] 继承了监听 fd={fd}（{key}）—— 升级窗口内这个端口没有重新 bind 过");
        // SAFETY: fd 由上一代經 SCM_RIGHTS 傳來，此處接管其所有權，且同一個鍵只取用一次。
        //
        // ★ ManuallyDrop：從這裡到成功交出所有權之間，**任何提前析構都會 `close(fd)`**，
        //   而 `Fds` 表裡那條記錄仍指著這個號碼 —— 號碼隨後會被任意 `open()` 重複使用，
        //   下一次升級就把一個無關的 fd 當成監聽 socket 發給下一代。`Fds` 沒有 `remove()`。
        let std_listener = ManuallyDrop::new(unsafe { std::net::TcpListener::from_raw_fd(fd) });
        std_listener.set_nonblocking(true)?; // 失敗時不析構，fd 保持有效
        let owned = ManuallyDrop::into_inner(std_listener);
        return match TcpListener::from_std(owned) {
            Ok(l) => Ok(l),
            Err(e) => {
                // ⚠ 殘餘風險：`from_std` 消耗了 owned，失敗時 fd 已被關閉，而表裡那條
                //   記錄仍指向它。只在 tokio reactor 註冊失敗（資源耗盡）時出現，
                //   在不改 `Fds` 公開介面的前提下修不掉 —— 所以至少讓它**大聲說出來**。
                error!(
                    "[l4] from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    let listener = TcpListener::bind(bind).await?;
    let fd = listener.as_raw_fd();
    table.lock().add(key.to_string(), fd);
    info!("[l4] 监听 {bind}（TCP 透传），fd={fd} 已登记为 {key}，下一代继承得到");
    Ok(listener)
}

/// accept 出錯時該不該歇一下。
///
/// ★ ★ **fd 耗盡（EMFILE/ENFILE）時 `accept()` 會立刻回傳錯誤且不阻塞**，
/// 於是「記一條紀錄然後繼續循環」就變成滿速空轉 + 紀錄洪水 ——
/// 把「fd 快用完了」放大成「這臺機器廢了」，而且恰好在資源最緊張的時候。
/// 實測過：無退避時 369,156 條紀錄/秒。Pingora 自己的 accept 循環帶著同一道防護。
async fn backoff_if_fd_exhausted(e: &std::io::Error, tag: &str) {
    // 24 = EMFILE（本處理程序 fd 用盡），23 = ENFILE（系統級用盡）
    if matches!(e.raw_os_error(), Some(24) | Some(23)) {
        error!("[{tag}] fd 耗尽（{e}），退避 1 秒——否则这个循环会满速空转");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// 看一眼 ClientHello 的超時。
///
/// ★ 它與 TCP 那條「有意不設空閒超時」**不衝突**，因為這一段的性質不同：
/// 連接已經接受、而上游**還沒被選出來** —— 一個連上來什麼都不發的對端，
/// 在這一段裡佔的是樞衡自己的任務，且沒有任何對端資源與之對應。
/// ⚠ 沒有這條時，它就是 L4 面上的 slowloris：一條不發位元組的連接可以永遠掛著。
const PEEK_TIMEOUT: Duration = Duration::from_secs(3);

/// 看 ClientHello 時最多快取多少位元組。
///
/// ★ 16 KiB 是一個 TLS record 的上限量級，而 ClientHello 通常 < 2 KiB。
/// ⚠ 超過就**不再等**，按「不是 TLS」處理走備援 —— 快取無上限的話，
/// 一條慢慢吐位元組的連接能把記憶體吃到天上，而它連一個上游都還沒佔用。
const PEEK_MAX: usize = 16 * 1024;

/// 等一個 PROXY 頭最多多久（**M2 批 D**）。
///
/// ⚠ ⚠ **它與 `PEEK_TIMEOUT` 是兩段，不是一段**：一條既要收 PROXY 頭、
/// 又配了 `sni` 分流的連接，最壞要 **3 + 3 = 6 秒**才選得出上游。
/// ★ 這個數字寫在這裡也寫進 DSL 參考 —— 一個「加起來是多少」只能靠讀程式碼得出的預算，
/// 遲早會被當成 3 秒。
const PP_TIMEOUT: Duration = Duration::from_secs(3);

/// 收下來的 PROXY 頭。
struct Received {
    /// 真實用戶端位址。
    ///
    /// ⚠ **`None` 是正常結果**：`LOCAL` 與 `PROXY UNKNOWN` 表示「這條連接沒有真實用戶端」
    /// （上游 LB 的健康檢查就長這樣）⇒ 呼叫方**繼續用 socket 對端**。
    client: Option<SocketAddr>,
    /// 讀頭時**多讀到**的位元組 —— 它們已經是應用資料了，一個都不能丟。
    ///
    /// ★ ★ 這一格是本函式最容易漏的地方：TCP 是流，一次 `read` 很可能把
    /// PROXY 頭與它後面的 ClientHello **一起**讀回來。少了這一格，
    /// 上游收到的流就從 ClientHello 中間開始 —— 與批 C 那條「不重放」的缺陷同一個形狀。
    leftover: Vec<u8>,
}

/// 讀一個 PROXY 頭。**只在 peer 落在信任清單裡時才會被呼叫。**
///
/// ★ ★ ★ **不在清單裡的連接，這個函式一次都不會被呼叫 —— 一個位元組都不讀。**
/// 那是 owner 拍板的口徑（與安全基線裡 XFF 那條同源），落法見 `dsl-reference.md`。
/// ⚠ 「讀掉丟棄」需要先解析**攻擊者控制**的長度欄位才知道丟多少；「不讀」完全不碰。
///
/// 回傳 `Err` = **關掉這條連接**。⚠ 這裡與「不在清單裡」有意相反：
/// 一個**在信任清單裡**的對端發來壞頭，說明設定或對端出了問題，而此時我們
/// **已經吃掉了一部分位元組、還原不回去** —— 硬把殘缺的流轉給上游只會把問題推遠。
async fn read_proxy_header(client: &mut TcpStream) -> Result<Received, String> {
    use fulcrum_runtime::proxyproto::{self, Verdict};
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 256];
    let started = Instant::now();
    loop {
        // 先拿已有的位元組問一次 —— ★ `decode` 是純函式，重複問不要錢。
        match proxyproto::decode(&buf) {
            Verdict::Done { client, consumed } => {
                return Ok(Received {
                    client,
                    leftover: buf[consumed..].to_vec(),
                });
            }
            Verdict::Invalid(why) => return Err(why.to_string()),
            Verdict::Need(_) => {}
        }
        if buf.len() >= proxyproto::MAX_HEADER {
            // ★ 走不到（`decode` 自己的兩條上界更早觸發），但留著：
            //   它是這個循環**一定會停**的最後一道保證。
            return Err("PROXY 头超过上限仍没读完".into());
        }
        let left = match PP_TIMEOUT.checked_sub(started.elapsed()) {
            Some(d) if !d.is_zero() => d,
            _ => return Err(format!("等 PROXY 头超时（{PP_TIMEOUT:?}）")),
        };
        match tokio::time::timeout(left, client.read(&mut chunk)).await {
            Ok(Ok(0)) => return Err("读 PROXY 头时对端关闭了连接".into()),
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            Ok(Err(e)) => return Err(format!("读 PROXY 头出错：{e}")),
            Err(_) => return Err(format!("等 PROXY 头超时（{PP_TIMEOUT:?}）")),
        }
    }
}

/// ClientHello 裡我們要的那兩樣。
struct Peeked {
    /// 已經從用戶端讀走的**原始位元組**。★ 必須原樣重放給上游 ——
    /// 我們是透傳，不是終止 TLS：少一個位元組，上游的握手就廢了。
    raw: Vec<u8>,
    /// `server_name` 擴充。`None` = 沒帶 SNI（合法，比如按 IP 直連）。
    sni: Option<String>,
    /// ALPN 清單，**原樣的位元組**（`h2` / `http/1.1` / …）。
    alpn: Vec<Vec<u8>>,
    /// 這真的是一個 TLS ClientHello 嗎。`false` = 不是 TLS、或超時/超限。
    is_tls: bool,
}

/// 餵給 BoringSSL 的**記憶體傳輸**。
///
/// ⚠ ⚠ **寫那一半一律丟掉，這是安全屬性不是省事**：我們不是這條 TLS 連接的對端，
/// 往真用戶端寫一個位元組（哪怕只是一個 alert）都等於冒充上游說話。
/// ★ 於是「不回 alert」這條紀律從「記得別寫」變成了**寫不出去** ——
/// 這個結構裡根本沒有通向用戶端的那一頭。
#[derive(Default)]
struct HelloFeed {
    /// 已經從用戶端讀到、準備餵給 BoringSSL 的位元組。★ 有 [`PEEK_MAX`] 封頂。
    buf: Vec<u8>,
    /// BoringSSL 已經消化到哪兒。
    pos: usize,
}

impl std::io::Read for HelloFeed {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            // ⚠ ⚠ **必須是 `WouldBlock`，不能是 `Ok(0)`**：後者是 EOF，
            //   BoringSSL 會當成對端關了連接（不可重試）而把握手判死；
            //   而我們的意思只是「還沒讀到更多」。
            //   ★ boring 的 BIO 墊片認得這個 kind（`retriable_error`）並置重試位，
            //     於是 `accept()` 乾淨地回 `WANT_READ`。
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "ClientHello 还没读全",
            ));
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl std::io::Write for HelloFeed {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 早回呼看到的那兩樣。
struct SeenHello {
    sni: Option<String>,
    alpn: Vec<Vec<u8>>,
}

thread_local! {
    /// 早回呼把看到的東西丟在這裡，由 [`drive`] 當場取走。
    static SEEN: std::cell::RefCell<Option<SeenHello>> = const { std::cell::RefCell::new(None) };
}

/// 預讀用的那個 `SSL_CTX`：**整個處理程序一個**，只掛一個早回呼、不裝任何證書。
///
/// ★ 不需要證書：`SSL_CTX_set_select_certificate_cb` 是**早回呼**，
/// 在幾乎所有 ClientHello 處理之前觸發，而我們在它裡面就把握手掐掉了。
fn peek_ctx() -> Option<&'static SslContext> {
    static CTX: OnceLock<Option<SslContext>> = OnceLock::new();
    CTX.get_or_init(|| {
        // ★ boring 5 刪了 `SslMethod::tls_server()` ⇒ 用 `tls()`；這個 ctx 只經 `SslStream::accept()` 用，
        //   而 accept 自己把 SSL 設成伺服器端模式 ⇒ 行為不變。
        let mut b = match SslContextBuilder::new(SslMethod::tls()) {
            Ok(b) => b,
            Err(e) => {
                error!("[l4] 建不出预读用的 SSL_CTX：{e}");
                return None;
            }
        };
        b.set_select_certificate_callback(|hello| {
            let sni = hello
                .servername(NameType::HOST_NAME)
                .map(|s| s.to_ascii_lowercase());
            // ⚠ 這裡拿到的是「**用戶端提供了**哪些」，不是協商結果 ——
            //   早回呼發生在 ALPN 協商之前。分流規則要的正是前者。
            // ★ 讀法與 fulcrum-tls 挑證書那條路**共用同一份** RFC 7301 框架實作，
            //   不是各寫一份（D18/G66 同一條理由）。
            let alpn = hello
                .get_extension(ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION)
                .and_then(alpn_list)
                .unwrap_or_default();
            SEEN.with(|s| *s.borrow_mut() = Some(SeenHello { sni, alpn }));
            // ★ ★ ★ **永遠中止**。我們只是看一眼：這條連接的 TLS 要由上游去終止，
            //   繼續下去就變成中間人了。
            Err(SelectCertError::ERROR)
        });
        Some(b.build())
    })
    .as_ref()
}

/// 推進一次握手之後的結論。
enum Step {
    /// 早回呼開過火 ⇒ 這是一個 TLS ClientHello，而且我們看清了。
    Saw(SeenHello),
    /// 還沒讀全，再喂。
    NeedMore,
    /// 這不像 TLS（或者讀到一半壞了）。帶上說得清的原因。
    NotTls(String),
}

/// 把已經喂進去的位元組推一次握手，並把早回呼看到的東西取回來。
///
/// ⚠ ⚠ **本函式不是 `async fn`，而這就是整段的安全論證**：早回呼由 BoringSSL 在
/// `accept()` 裡直接呼叫（不換執行緒），而非 async 函式裡寫不出 `.await`
/// ⇒「清槽 → 握手 → 取槽」三步之間 thread-local 槽不可能被別的連接插進來。
/// **改成 `async fn` 會讓論證失效，表現是串臺**：一條連接讀到另一條的 SNI，都不回報錯誤。
///
/// ★ 先看槽、再看回傳碼：早回呼開過火就說明東西到手了，而這時握手必然失敗 ——
/// 那不是錯誤，正是我們要的。
fn drive(tls: &mut SslStream<HelloFeed>) -> Step {
    // ⚠ 先清：同一個執行緒上一條連接可能留下過東西。
    SEEN.with(|s| *s.borrow_mut() = None);
    let outcome = tls.accept();
    let seen = SEEN.with(|s| s.borrow_mut().take());
    match (seen, outcome) {
        (Some(h), _) => Step::Saw(h),
        // ⚠ 走不到：早回呼永遠回 `Err`。真走到了說明回呼沒掛上，
        //   那是**設定錯**而不是「對端不是 TLS」，所以它有自己的措辭。
        (None, Ok(())) => Step::NotTls("握手居然完成了 —— 早回调没有挂上".into()),
        (None, Err(e)) if e.code() == ErrorCode::WANT_READ => Step::NeedMore,
        (None, Err(e)) => Step::NotTls(format!("{e}")),
    }
}

fn not_tls(raw: Vec<u8>) -> Peeked {
    Peeked {
        raw,
        sni: None,
        alpn: Vec::new(),
        is_tls: false,
    }
}

/// 唯讀 ClientHello，**不完成握手**。
///
/// 走的是 **BoringSSL 的早回呼**（G104 第 ② 處）：一臺真的握手狀態機 + 一條記憶體傳輸，
/// 位元組喂進 [`HelloFeed`]，`SSL_CTX_set_select_certificate_cb` 在 ClientHello 解完時觸發，
/// 抄走 SNI 與 ALPN 之後**當場把握手掐掉**。
/// ★ 不手寫 ClientHello 解析（那是在解析攻擊者控制的二進位）。
///
/// ★ **一處行為差異說在明處**：早回呼發生在幾乎所有 ClientHello 檢核**之前**，
/// 於是一個 rustls 會當場拒掉的 ClientHello 在這一側照樣能被讀出 SNI 並分流。
/// **這正是想要的語義** —— 樞衡不替上游拒絕一個它自己也許能處理的用戶端。
///
/// ⚠ ⚠ **無論結果如何，讀走的位元組都在 `raw` 裡**（走備援時要重放），
/// `already` 是收 PROXY 頭時多讀到的那一段 —— 它必須**先餵給狀態機、也進 `raw`**，
/// 否則「先 PROXY 頭再 ClientHello」的連接會全部走備援，
/// **而紀錄上看起來只是「這個用戶端沒帶 SNI」**。
async fn peek_client_hello(client: &mut TcpStream, already: Vec<u8>) -> Peeked {
    let mut raw: Vec<u8> = Vec::with_capacity(2048);
    let Some(tls) = new_peek_stream() else {
        // ⚠ 建不出狀態機時**照樣把位元組原樣帶走**：這條連接仍然要能走備援，
        //   而不是被丟掉開頭幾個位元組。原因已經在 error! 裡說清了。
        return not_tls(raw);
    };
    let mut tls = tls;
    let mut chunk = [0u8; 4096];
    let started = Instant::now();
    // ★ 把上一段多讀到的位元組當成「剛讀到的一塊」走同一條路 ——
    //   有意不另寫一份喂法：兩份「差不多的」喂法會在下一次改動時分家。
    let mut pending: Option<Vec<u8>> = if already.is_empty() {
        None
    } else {
        Some(already)
    };
    loop {
        let left = match PEEK_TIMEOUT.checked_sub(started.elapsed()) {
            Some(d) if !d.is_zero() => d,
            _ => {
                debug!("[l4] 等 ClientHello 超时（{PEEK_TIMEOUT:?}）—— 按「不是 TLS」处理");
                return not_tls(raw);
            }
        };
        // ★ 有 `pending` 就先消化它，一個位元組都不去 socket 上讀。
        let fresh: Vec<u8> = match pending.take() {
            Some(b) => b,
            None => {
                let n = match tokio::time::timeout(left, client.read(&mut chunk)).await {
                    Ok(Ok(0)) | Err(_) => 0,
                    Ok(Ok(n)) => n,
                    Ok(Err(e)) => {
                        debug!("[l4] 读 ClientHello 出错：{e}");
                        0
                    }
                };
                if n == 0 {
                    return not_tls(raw);
                }
                chunk[..n].to_vec()
            }
        };
        raw.extend_from_slice(&fresh);
        if raw.len() > PEEK_MAX {
            debug!("[l4] ClientHello 超过 {PEEK_MAX} 字节还没读完 —— 按「不是 TLS」处理");
            return not_tls(raw);
        }
        // ⚠ **只追加新讀到的那一段**：`HelloFeed` 自己記著消化到哪兒了，
        //   重喂整個緩衝區會讓狀態機把同樣的位元組吃兩遍。
        tls.get_mut().buf.extend_from_slice(&fresh);
        match drive(&mut tls) {
            Step::Saw(h) => {
                return Peeked {
                    raw,
                    sni: h.sni,
                    alpn: h.alpn,
                    is_tls: true,
                };
            }
            Step::NeedMore => {}
            Step::NotTls(why) => {
                debug!("[l4] 这不像 TLS（{why}）—— 走兜底");
                return not_tls(raw);
            }
        }
    }
}

/// 起一臺只用來看一眼的握手狀態機。`None` = 起不來（已 `error!`）。
fn new_peek_stream() -> Option<SslStream<HelloFeed>> {
    let ctx = peek_ctx()?;
    let ssl = Ssl::new(ctx)
        .map_err(|e| error!("[l4] 建不出预读用的 SSL：{e}"))
        .ok()?;
    SslStream::new(ssl, HelloFeed::default())
        .map_err(|e| error!("[l4] 建不出预读用的内存传输：{e}"))
        .ok()
}

/// 把一條已接受的連接接到某個上游去，然後雙向搬位元組。
///
/// ★ 「挑哪個上游」在**每條連接**上決定一次，取的是當時的執行環境 ——
/// 於是 `POST /load` 換掉上游之後，新連接立刻走新的，老連接不受影響。
async fn handle_conn(shared: Arc<SharedRuntime>, listen: String, mut client: TcpStream) {
    let peer_ip = client.peer_addr().ok().map(|a| a.ip());
    let rt = shared.current();
    let Some(l) = rt.l4_listeners.iter().find(|l| l.listen == listen) else {
        // ⚠ 走到這裡說明當前設定裡已經沒有這個監聽器了。★ 管理面擋著連接埠集變化，
        //   所以這**不該**發生；真發生了就把連接關掉並說清楚，而不是靜默丟棄。
        error!(
            "[l4] 当前配置里找不到监听器 {listen} —— 关闭这条连接（这不该发生，管理面本该挡住端口集变化）"
        );
        return;
    };
    // ── 收 PROXY 頭（批 D）──────────────────────────────────────────────
    //
    // ★ ★ ★ **不在信任清單裡 ⇒ 這一整段一步都不走，一個位元組都不讀。**
    //   那 12/16 位元組（如果真有的話）會原樣當成應用資料流給上游 ——
    //   owner 拍板的口徑，理由見 `dsl-reference.md` §二那一節。
    //   ⚠ 清單為空時 `trusts_proxy_protocol` 恆 false，所以「沒配」與「配了但不匹配」
    //     走的是同一條路 —— 這是有意的：**一份空清單不是「信任所有人」**。
    let mut real_client: Option<SocketAddr> = None;
    let mut carried: Vec<u8> = Vec::new();
    if let Some(ip) = peer_ip
        && l.trusts_proxy_protocol(ip)
    {
        match read_proxy_header(&mut client).await {
            Ok(r) => {
                // ⚠ `client` 為 `None` 是 LOCAL / UNKNOWN —— **不是錯誤**，
                //   此時繼續用 socket 對端（健康檢查就長這樣）。
                real_client = r.client;
                carried = r.leftover;
                debug!(
                    "[l4] {listen}：收下 PROXY 头，真实客户端 {}",
                    match real_client {
                        Some(a) => a.to_string(),
                        None => "（头里没有，用 socket 对端）".to_string(),
                    }
                );
            }
            Err(why) => {
                // ★ 在清單裡卻發來壞頭 ⇒ 關連接。理由見 `read_proxy_header` 的文件：
                //   我們已經吃掉了一部分位元組，還原不回去。
                warn!("[l4] {listen}：信任的来源 {ip} 发来的 PROXY 头有问题（{why}），关闭连接");
                return;
            }
        }
    }
    // ★ ★ 從這裡往下，「用戶端是誰」一律用 `effective_ip` —— **不用 `peer_ip`**。
    //   ⚠ 挑上游那一步尤其要緊：`lb_policy ip_hash` 拿 socket 對端去算，
    //   會讓**前面那臺 LB 的所有連接雜湊到同一個上游**，而現場表現只是「負載不均」。
    let effective_ip = real_client.map(|a| a.ip()).or(peer_ip);
    // ── 選目標（批 C：可能要先看一眼 ClientHello）────────────────────────
    //
    // ★ **沒有分流規則時一步都不多走**：不讀、不等、不快取 —— 批 A 的形狀原樣保留。
    //   ⚠ 「順手都 peek 一下反正也不貴」會給每一條 L4 連接加上一次讀與一個超時預算，
    //   而資料庫連接那類對端**可能根本不先說話**（伺服器端先發 greeting），那樣就死等到超時。
    let (target, prelude) = if l.rules.is_empty() {
        // ⚠ 沒有分流規則時，收 PROXY 頭多讀到的那一段**仍然要重放** ——
        //   它已經是應用資料了。★ 少了這個 `carried`，一條「LB 發 PROXY 頭 +
        //   緊跟著第一批資料」的連接會丟掉開頭那幾個位元組。
        match &l.target {
            Some(t) => (t, std::mem::take(&mut carried)),
            None => {
                warn!("[l4] {listen}：既没有分流规则也没有兜底上游，关闭连接");
                return;
            }
        }
    } else {
        let peeked = peek_client_hello(&mut client, std::mem::take(&mut carried)).await;
        let hit = if peeked.is_tls {
            // ★ **按書寫順序，第一個命中即用**（DSL 參考 §4.5）。
            l.rules
                .iter()
                .find(|r| r.matches(peeked.sni.as_deref(), &peeked.alpn))
        } else {
            None
        };
        match hit {
            Some(r) => {
                debug!(
                    "[l4] {listen}：ClientHello sni={:?} alpn={:?} 命中 `{} {}`",
                    peeked.sni,
                    peeked
                        .alpn
                        .iter()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .collect::<Vec<_>>(),
                    r.kind.as_str(),
                    r.values.join(" ")
                );
                (&r.target, peeked.raw)
            }
            None => match &l.target {
                Some(t) => {
                    debug!(
                        "[l4] {listen}：没有规则命中（is_tls={} sni={:?}）—— 走兜底",
                        peeked.is_tls, peeked.sni
                    );
                    (t, peeked.raw)
                }
                None => {
                    // ★ 「只服務我認得的那幾個名字」是一種合法設定 ——
                    //   但它必須**說出來**，否則現場只有「連上就斷」。
                    warn!(
                        "[l4] {listen}：没有规则命中且没配兜底（is_tls={} sni={:?}），关闭连接",
                        peeked.is_tls, peeked.sni
                    );
                    return;
                }
            },
        }
    };
    let Some(start) = target.pick_index_by(effective_ip) else {
        // ★ 與 HTTP 那邊的 502 同一條語義：一個可用的上游都沒有。
        //   ⚠ L4 上沒有狀態碼可回，能做的只有**乾淨地關掉** —— 而且要留一行紀錄，
        //   否則現場是「連上就斷」，看不出是上游全掛還是連接埠配錯。
        warn!("[l4] {listen}：一个可用的上游都没有（解析不出地址或被判死），关闭连接");
        return;
    };
    // ★ ★ **建連階段可以換上游，這一點與 HTTP 那邊有意不同。**
    //
    //   HTTP 那邊一個上游的候選全失敗就回 502，**不換上游** —— 理由是那條路上
    //   「換一個再來一次」會滑向重試語義（請求體可能已經發出去了）。
    //   而 L4 在建連之前**一個位元組都還沒走**：用戶端還沒發任何東西，上游也還沒被打擾，
    //   換一個是完全透明的。⇒ 從策略挑中的那個開始，按上游列表順序往後試一圈。
    //
    //   ⚠ 不這麼做的代價是具體的：兩個上游、掛掉一個，用戶端會**每兩條連接壞一條**，
    //   而樞衡自己一切正常 —— 這正是「L4 面能不能真用」的分界線。
    let n = target.upstreams.len();
    let mut chosen: Option<(&fulcrum_runtime::Upstream, TcpStream)> = None;
    let mut last_err: Option<String> = None;
    'outer: for k in 0..n {
        let up = &target.upstreams[(start + k) % n];
        let candidates = up.dial_candidates();
        if candidates.is_empty() || !up.is_healthy() {
            // 與 `pick_index_by` 同一把篩子：解析不出位址、或被健康檢查判死的，跳過。
            continue;
        }
        // ★ 逐個候選試，不是只試第一個。理由與 HTTP 那邊逐字相同：
        //   `localhost` 解出來第一個可能是 `[::1]`，而上游只監聽 `127.0.0.1`。
        for (i, dial) in candidates.iter().enumerate() {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(dial)).await {
                Ok(Ok(s)) => {
                    if k > 0 || i > 0 {
                        debug!(
                            "[l4] {listen}：第 {} 个上游 {} 的第 {} 个候选 {dial} 连上了",
                            k + 1,
                            up.addr,
                            i + 1
                        );
                    }
                    up.acquire();
                    chosen = Some((up, s));
                    break 'outer;
                }
                Ok(Err(e)) => last_err = Some(format!("{} {dial}：{e}", up.addr)),
                Err(_) => {
                    last_err = Some(format!(
                        "{} {dial}：连接超时（{CONNECT_TIMEOUT:?}）",
                        up.addr
                    ))
                }
            }
        }
    }
    let Some((up, mut upstream)) = chosen else {
        warn!(
            "[l4] {listen}：{n} 个上游全都连不上，最后一条：{}",
            last_err.unwrap_or_else(|| "（没有错误信息）".into())
        );
        return;
    };
    // ── 發 PROXY 頭（批 D）──────────────────────────────────────────────
    //
    // ★ ★ ★ **它必須是這條連接上的第一批位元組 —— 在重放 prelude 之前。**
    //   ⚠ 順序反了，上游會把 ClientHello 的開頭當成 PROXY 頭去解析，
    //   而現場表現是「上游握手失敗」，與「我們沒重放」長得一模一樣。
    //
    // ★ 報給上游的是 `effective_ip` 那一側的位址：我們自己也收了一個 PROXY 頭時，
    //   寫出去的是**頭裡那個用戶端**（鏈式傳遞）—— 隔了幾層，上游看到的仍是最初那個人。
    if let Some(ver) = l.proxy_protocol {
        // ⚠ ⚠ 造頭要**兩個**位址，且族必須一致；拿不到本機位址時不硬湊
        //   （`encode` 會退成 LOCAL / UNKNOWN，讓上游知道「沒有可報的用戶端」）。
        let local = client
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let src = real_client
            .or_else(|| client.peer_addr().ok())
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
        let header = fulcrum_runtime::proxyproto::encode(ver, src, local);
        if let Err(e) = upstream.write_all(&header).await {
            debug!("[l4] {listen}：写 PROXY 头失败（{e}）");
            up.release();
            return;
        }
        debug!(
            "[l4] {listen} → {}：已发 PROXY {} 头（客户端 {src}）",
            up.addr,
            ver.as_str()
        );
    }
    // ★ ★ ★ **先把 peek 時讀走的位元組原樣送過去**，再進雙向搬運。
    //   ⚠ 少了這一步，上游收到的 TLS 流會**從 ClientHello 中間開始** ——
    //   現場表現是「握手失敗」，而樞衡這邊一切正常：連接建立了、位元組也在流動。
    //   ★ 這是「透傳」這兩個字的全部重量：我們看了，但**不許吃掉**。
    if !prelude.is_empty()
        && let Err(e) = upstream.write_all(&prelude).await
    {
        debug!("[l4] {listen}：重放 ClientHello 失败（{e}）");
        up.release();
        return;
    }
    debug!("[l4] {listen} → {}（透传）", up.addr);
    // ★ `copy_bidirectional` 一端讀到 EOF 就把另一端 shutdown 寫方向，
    //   於是**半關閉**（一端說完了、另一端還在說）能透過去 —— 這不是細節：
    //   不少協定（含某些資料庫用戶端）靠半關閉表示「我說完了」。
    match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        // ⚠ `c2u` **不含**上面重放的那一段：那是我們自己寫過去的，不經過 `copy_bidirectional`。
        //   ⇒ 把重放量單獨報出來，否則紀錄會說「用戶端→上游 0B」而上游明明收到了 ClientHello。
        Ok((c2u, u2c)) => debug!(
            "[l4] {listen} 连接结束：客户端→上游 {c2u}B（另有重放 {}B），上游→客户端 {u2c}B",
            prelude.len()
        ),
        // ⚠ 這裡**只用 debug**：連接被任一端重置在 L4 上是家常便飯
        //   （用戶端 Ctrl-C、上游重新啟動）。★ 把它記成 error 會讓紀錄裡全是噪音，
        //   而噪音會把真正該看的那幾行埋掉。
        Err(e) => debug!("[l4] {listen} 连接结束于错误：{e}"),
    }
    up.release();
}

#[async_trait]
impl Service for TcpProxyService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        mut shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        // ★ 這個參數由 `listener_tasks_per_fd` 設定驅動，Pingora 原生服務會真的按它
        //   開多個 accept 任務。本服務**沒有實作**它 —— 與其一聲不響地只開一個
        //   （設定就成了謊話），不如直接拒絕啟動。
        if listeners_per_fd > 1 {
            error!(
                "[l4] listener_tasks_per_fd={listeners_per_fd} 不被 L4 服务支持（它只会开 1 个 accept 任务）。\
                 拒绝启动，以免配置与实际行为不符。"
            );
            return;
        }

        let listener = match build_listener(&self.bind, &self.fd_key, fds, self.upgrading).await {
            Ok(l) => l,
            Err(e) => {
                // ⚠ 起不來就是起不來，**不許降級成一條警告**：一個「HTTP 好好的、
                //   而那個資料庫連接埠沒人在聽」的處理程序，查起來會先懷疑防火牆。
                error!("[l4] 在 {} 上建不出监听器：{e}", self.bind);
                return;
            }
        };

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    // 停止 accept，**已建立的連接留在本代繼續跑完**
                    //（排空由 Pingora 的 grace period 管，與 HTTP 那邊同一個預算）。
                    info!("[l4] {} 收到停机信号，停止 accept（已建立的连接继续）", self.bind);
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((sock, _peer)) => {
                            // ★ Nagle 關掉：L4 透傳裡典型的是小包一來一回（資料庫協定、
                            //   遊戲心跳），而 Nagle 會為了湊滿一個段**故意等** ——
                            //   代價是延遲，收益在這裡幾乎為零。
                            if let Err(e) = sock.set_nodelay(true) {
                                debug!("[l4] set_nodelay 失败（{e}）—— 继续，它只影响延迟");
                            }
                            let shared = self.shared.clone();
                            let listen = self.listen.clone();
                            // ★ 連接計數（**M2 批 O**）：包一層而不改 `handle_conn` 的簽名。
                            let g = self.conn.guard();
                            tokio::spawn(async move {
                                // ⚠ ⚠ **必須綁名字**：寫成 `let _ = g;` 會當場 drop，
                                //   於是 active 恆為 0 而 total 照漲，且不會有東西紅。
                                let _g = g;
                                handle_conn(shared, listen, sock).await
                            });
                        }
                        Err(e) => {
                            error!("[l4] accept 出错：{e}");
                            backoff_if_fd_exhausted(&e, "l4").await;
                        }
                    }
                }
            }
        }
    }

    /// 這個 service 用幾個執行緒（**G35 / G140**）。⚠ 回傳 `Some` 時 pingora
    /// **不再看**全域 `conf.threads` —— 見 `server/mod.rs` 的
    /// `service.threads().unwrap_or(conf.threads)`。
    fn threads(&self) -> Option<usize> {
        self.threads
    }

    fn name(&self) -> &str {
        &self.name
    }

    /// pingora 0.9.0（上游 `f82478a`）：新一代據此關掉「沒有任何服務認領」的繼承 fd。
    /// ⚠ 必須是 fd 表的**鍵**（帶前綴），⛔ 不是 bind 位址 —— 寫成位址，新一代會先關掉自己要繼承的
    ///   那個 fd；不實作（預設 `None`）會讓**整個處理程序**的清理靜默關閉。釘在單測 `*_服务声明的正是它的_fd_表键`。
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![self.fd_key.clone()])
    }
}
// ══════════════════════════════════════════════════════════════════════════
// UDP 透傳（M2 批 B）
// ══════════════════════════════════════════════════════════════════════════
//
// ★ ★ **UDP 與 TCP 不是「同一件事換個協定」**，三處差別都會咬人：
//
// | | TCP | UDP |
// |---|---|---|
// | 工作階段邊界 | 核心給的（accept / FIN）| **沒有** —— 自己按用戶端位址維護工作階段表 |
// | 空閒超時 | 有意不設 | **必須有**，否則每個出現過的位址都永久佔一個 socket |
// | 停機 | 停止 `accept` | ★ **停止 `recv_from`** —— 不停的話老一代會偷走新一代的資料報 |
//
// ⚠ 最後一行：換代時兩代在 `recv_from` **同一個** socket，誰先醒誰拿走。
// 老一代吃掉本該屬於新一代的工作階段首包，而用戶端只看到「換代那幾秒有些請求沒回應」，
// 兩邊紀錄都正常。

/// 工作階段空閒多久回收。
///
/// ★ TCP 那邊**有意不設**空閒超時，UDP 這邊**必須有**：UDP 沒有 FIN，
/// 不回收的話，每一個出現過的用戶端位址都會永遠佔著一個上游 socket 與一個任務。
/// ⚠ ⚠ 而 **UDP 源位址是可偽造的** ⇒ 那是一條**遠程可觸發**的 fd 耗盡路徑，
/// 不是理論風險。
/// ⚠ 60 秒是折中：DNS 查詢在毫秒級、遊戲心跳通常遠短於它；
/// ⏳ 讓它可配是後續批的事（`l4` 塊目前沒有放選項的位置，見 dsl-reference §4.5）。
const UDP_SESSION_IDLE: Duration = Duration::from_secs(60);

/// 每個監聽器的並行工作階段上限。
///
/// ★ 與上面那條是同一件事的兩半：超時管「什麼時候放」，上限管「最多同時佔多少」。
/// ⚠ 沒有上限時，一次偽造源位址的洪水能在超時到期**之前**就把 fd 吃光。
const UDP_MAX_SESSIONS: usize = 1024;

/// 單個資料報緩衝區。
///
/// ★ 取 65535（UDP 載荷上限）而不是「夠用就好」的 2 KiB：`recv` 讀不下的部分
/// 會被**靜默丟棄**（不帶 `MSG_TRUNC` 就看不出來），現場表現是「大包偶爾損壞」——
/// 那種缺陷會被歸咎於網路。
/// ⚠ 代價寫在明處：每條工作階段一份 ⇒ 上限 1024 × 64 KiB ≈ **64 MiB** 的最壞佔用。
const UDP_DATAGRAM_MAX: usize = 65535;

/// 工作階段清掃的打點間隔。★ 與空閒超時分開：超時是**判據**，這個是**檢查頻率**。
const UDP_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// 工作階段表裡的一格。
struct Session<T> {
    payload: T,
    last_seen: Instant,
}

/// 插入的結果。
#[derive(Debug, PartialEq, Eq)]
pub enum UdpAdmit {
    Ok,
    /// 到達並行上限，**沒有插入**。
    AtCapacity,
}

/// UDP 工作階段表。
///
/// ★ ★ 它被**故意做成不碰網路、不看時鐘**的結構：`now` 從參數進來、載荷是泛型，
/// 於是「到點回收」「到頂拒絕」這兩條可以**確定性地單測**，
/// 不必在門禁裡真等 60 秒。⚠ 這是 G56 那一塊（續期判定模組裡沒有一個
/// `SystemTime::now()`）的同一招 —— **時間是參數，不是環境**。
pub struct UdpSessionTable<T> {
    map: HashMap<SocketAddr, Session<T>>,
    cap: usize,
    idle: Duration,
}

impl<T> UdpSessionTable<T> {
    pub fn new(cap: usize, idle: Duration) -> Self {
        Self {
            map: HashMap::new(),
            cap,
            idle,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 已有工作階段就**續期**並把載荷給出去；沒有就 `None`。
    pub fn touch(&mut self, peer: &SocketAddr, now: Instant) -> Option<&T> {
        let s = self.map.get_mut(peer)?;
        s.last_seen = now;
        Some(&s.payload)
    }

    /// 收一條新工作階段。
    ///
    /// ★ 到頂時**拒絕新的**，而不是踢掉一條老的 —— 後者會讓一次洪水把正常用戶端
    /// 全部擠出去（LRU 在這裡是攻擊者的朋友）。
    /// ⚠ 拒絕時**不接管載荷**：誰建立誰負責收尾，比「誰拒絕誰負責」少一條隱式約定。
    pub fn admit(&mut self, peer: SocketAddr, payload: T, now: Instant) -> UdpAdmit {
        if self.map.len() >= self.cap {
            return UdpAdmit::AtCapacity;
        }
        self.map.insert(
            peer,
            Session {
                payload,
                last_seen: now,
            },
        );
        UdpAdmit::Ok
    }

    /// 回收空閒超過 `idle` 的工作階段，把載荷交還給呼叫方處理（比如 `abort` 任務）。
    pub fn sweep(&mut self, now: Instant) -> Vec<(SocketAddr, T)> {
        let idle = self.idle;
        let dead: Vec<SocketAddr> = self
            .map
            .iter()
            .filter(|(_, s)| now.duration_since(s.last_seen) >= idle)
            .map(|(k, _)| *k)
            .collect();
        dead.into_iter()
            .filter_map(|k| self.map.remove(&k).map(|s| (k, s.payload)))
            .collect()
    }
}

/// 生產環境裡一條工作階段真正帶著的東西。
pub struct LiveSession {
    /// 連到上游的那個 socket。轉發用它。
    up: Arc<UdpSocket>,
    /// 回包任務。★ 回收工作階段時必須 `abort()` —— 否則任務繼續持有那個 socket，
    /// 而表裡已經沒有它了：**表看起來在回收，fd 卻沒還回來**。
    task: tokio::task::JoinHandle<()>,
    /// 上游位址，只用於紀錄。
    upstream: String,
}

/// 一個 `l4 udp` 監聽器對應的服務。
pub struct UdpProxyService {
    /// 這個 service 用幾個執行緒（**G35 / G140**）。`None` = 跟全域 `conf.threads`。
    ///
    /// ★ 與 pingora `ListeningService.threads` 同名同義 —— 自建 service 也參與
    /// 同一套角色分配，⛔ 不許在這裡另立一套。由 `serve()` 在建好之後設。
    pub threads: Option<usize>,
    shared: Arc<SharedRuntime>,
    listen: String,
    bind: String,
    fd_key: String,
    upgrading: bool,
    name: String,
    /// 這個監聽器的連接計數格（**M2 批 O**）。⚠ UDP 那一格數的是**工作階段**，且是從
    /// `sessions.len()` **派生**的 —— 見 `run()` 裡循環開頭那一行。
    conn: crate::conn_stats::BoundConn,
}

impl UdpProxyService {
    pub fn new(
        shared: Arc<SharedRuntime>,
        listen: &str,
        bind: String,
        upgrading: bool,
        conn_reg: Arc<crate::conn_stats::ConnRegistry>,
    ) -> Self {
        let conn = conn_reg.bind(crate::conn_stats::Entrypoint::L4Udp, &bind);
        Self {
            // ★ 預設 `None` = 跟全域；由 `serve()` 按角色設（G140）。
            threads: None,
            shared,
            listen: listen.to_string(),
            fd_key: format!("fulcrum-l4-udp:{bind}"),
            name: format!("fulcrum-l4-udp-{bind}"),
            bind,
            upgrading,
            conn,
        }
    }
}

/// 取 fd（繼承）或新建 UDP 監聽 socket，並把新建的放回表裡供下一代繼承。
///
/// ⚠ 與 TCP 那一份**逐字同構**，只是類型不同。兩份都留著而不是抽成泛型：
/// `TcpListener` 與 `UdpSocket` 的接管路徑（`from_std`）不共享 trait，
/// 硬抽出來的那一層只會把 SAFETY 註解擠走 —— 而那幾段註解是這裡最值錢的東西。
async fn build_udp_listener(
    bind: &str,
    key: &str,
    fds: Option<ListenFds>,
    upgrading: bool,
) -> std::io::Result<UdpSocket> {
    let Some(table) = fds else {
        if upgrading {
            return Err(std::io::Error::other(format!(
                "以 -u 升级启动却拿不到 fd 表，无法继承 {bind}：socket 移交已经失败了"
            )));
        }
        warn!("[l4] 没有 fd 表，只能在 {bind} 上新 bind（UDP）—— ★ 它不会参与下一次 socket 移交");
        return UdpSocket::bind(bind).await;
    };

    // ★ 0.9.0（上游 1d93711）：ListenFds 換成 parking_lot 鎖 ⇒ 守衛不 Send、⛔ 不許跨 `.await`。
    //   只在兩個同步的瞬間持鎖（查表、登記）；bind 期間不持鎖 —— 每個服務的鍵獨佔，不需要按位址串行。
    let inherited = table.lock().get(key).copied();

    if let Some(fd) = inherited {
        info!("[l4] 继承了 UDP 监听 fd={fd}（{key}）—— 升级窗口内这个端口没有重新 bind 过");
        // SAFETY: fd 由上一代經 SCM_RIGHTS 傳來，此處接管其所有權，且同一個鍵只取用一次。
        // ★ ManuallyDrop 的理由與 TCP 那一份完全相同：提前析構會 `close(fd)`，
        //   而表裡那條記錄仍指著這個號碼，下一次升級就會把一個無關的 fd 傳下去。
        let std_sock = ManuallyDrop::new(unsafe { std::net::UdpSocket::from_raw_fd(fd) });
        std_sock.set_nonblocking(true)?;
        let owned = ManuallyDrop::into_inner(std_sock);
        return match UdpSocket::from_std(owned) {
            Ok(s) => Ok(s),
            Err(e) => {
                error!(
                    "[l4] UDP from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    let sock = UdpSocket::bind(bind).await?;
    let fd = sock.as_raw_fd();
    table.lock().add(key.to_string(), fd);
    info!("[l4] 监听 {bind}（UDP 透传），fd={fd} 已登记为 {key}，下一代继承得到");
    Ok(sock)
}

/// 給一條新用戶端位址建工作階段：連上游 + 起回包任務。
async fn open_udp_session(
    listener: Arc<UdpSocket>,
    listen: &str,
    peer: SocketAddr,
    upstream_addr: &str,
    dial: SocketAddr,
) -> std::io::Result<LiveSession> {
    // ★ 按上游位址族選本機綁定位址：給 IPv6 上游 bind 一個 `0.0.0.0` 會直接失敗。
    let local: SocketAddr = if dial.is_ipv4() {
        "0.0.0.0:0".parse().expect("常量地址")
    } else {
        "[::]:0".parse().expect("常量地址")
    };
    let up = UdpSocket::bind(local).await?;
    // ⚠ UDP 的 `connect` **不是握手**，它只是記下預設對端（順帶讓核心過濾掉別人發來的包）。
    //   ★ 所以它幾乎不會失敗，也**證明不了上游活著** —— 這正是為什麼 TCP 那邊的
    //   「建連階段換上游」在這裡**沒有對應物**：UDP 上沒有任何東西能在發包之前
    //   告訴我們這個上游是死的。⇒ 挑一次就用它，換上游要等被動熔斷（`passive_fail`，還沒做）。
    up.connect(dial).await?;
    let up = Arc::new(up);
    let up_for_task = up.clone();
    let listen_name = listen.to_string();
    let upstream_for_task = upstream_addr.to_string();
    let task = tokio::spawn(async move {
        let mut buf = vec![0u8; UDP_DATAGRAM_MAX];
        loop {
            match up_for_task.recv(&mut buf).await {
                Ok(n) => {
                    if let Err(e) = listener.send_to(&buf[..n], peer).await {
                        debug!("[l4] {listen_name}：回包给 {peer} 失败（{e}），结束这条会话");
                        return;
                    }
                }
                Err(e) => {
                    debug!(
                        "[l4] {listen_name}：从上游 {upstream_for_task} 收包失败（{e}），结束这条会话"
                    );
                    return;
                }
            }
        }
    });
    Ok(LiveSession {
        up,
        task,
        upstream: upstream_addr.to_string(),
    })
}

#[async_trait]
impl Service for UdpProxyService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        mut shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        if listeners_per_fd > 1 {
            error!(
                "[l4] listener_tasks_per_fd={listeners_per_fd} 不被 L4 UDP 服务支持（它只会开 1 个收包任务）。\
                 拒绝启动，以免配置与实际行为不符。"
            );
            return;
        }

        let listener = match build_udp_listener(&self.bind, &self.fd_key, fds, self.upgrading).await
        {
            Ok(s) => Arc::new(s),
            Err(e) => {
                error!("[l4] 在 {} 上建不出 UDP 监听器：{e}", self.bind);
                return;
            }
        };

        let mut sessions: UdpSessionTable<LiveSession> =
            UdpSessionTable::new(UDP_MAX_SESSIONS, UDP_SESSION_IDLE);
        let mut buf = vec![0u8; UDP_DATAGRAM_MAX];
        let mut sweep = tokio::time::interval(UDP_SWEEP_INTERVAL);
        // ⚠ tokio 的第一次 `tick()` 立即回傳，跳掉它 —— 否則服務一起來就先掃一遍空表。
        sweep.tick().await;
        // ★ 到頂時**限流地說**：不限流的話，一次洪水會讓紀錄本身變成第二次攻擊。
        let mut last_cap_warn: Option<Instant> = None;

        loop {
            // ── 連接計數：`active` 從工作階段表**派生**（**M2 批 O**）──────────────
            //
            // ★ ★ ⛔ **不在旁邊再記一份 `+1/-1`**：`sessions.len()` 本身就是權威，
            //   而另記一份會讓清掃 `abort()`、到上限被拒、停機不再收包這三條路
            //   各需要記得減一 —— 那正是 D18/G66 那個分家形狀。
            // ⚠ ⚠ **寫在循環體開頭而不是末尾**：下面 recv 那一支有七八處 `continue`，
            //   而 `continue` **跳過循環體末尾** ⇒ 寫在末尾的話，「到上限被拒」
            //   「找不到上游」「建 socket 失敗」這些路徑上它根本不執行。
            // ★ 開頭則**每一輪迭代必經**；而阻塞在 `select!` 期間 `sessions` 不可能變
            //   （這個循環是它唯一的改寫者）⇒ 這個讀數是**精確**的，不是陳舊的。
            self.conn.set_active(sessions.len());
            tokio::select! {
                _ = shutdown.changed() => {
                    // ★ ★ ★ **這一支是 UDP 與 TCP 最重要的差別**：停機信號一到就
                    //   **不再 `recv_from`**。⚠ 繼續收的話，老一代會與新一代搶同一個
                    //   socket 上的資料報（兩代持有的是同一個 fd），把本該屬於新一代的
                    //   首包吃掉 —— 用戶端只會看到「換代那幾秒有些請求沒回應」。
                    //   ⇒ 已建立工作階段的**回包任務不動**（它們只 `send_to`，不搶 `recv`），
                    //     在飛的請求還能拿到應答，與 TCP 那邊「老連接繼續跑完」一致。
                    info!(
                        "[l4] {} 收到停机信号，停止收包（{} 条会话的回包任务继续）",
                        self.bind,
                        sessions.len()
                    );
                    break;
                }
                _ = sweep.tick() => {
                    let now = Instant::now();
                    for (peer, s) in sessions.sweep(now) {
                        // ★ 必須 abort：任務持有那個上游 socket，光從表裡刪掉
                        //   只是把 fd 從「看得見」變成「看不見」。
                        s.task.abort();
                        debug!("[l4] {}：会话 {peer} → {} 空闲超时，已回收", self.bind, s.upstream);
                    }
                }
                r = listener.recv_from(&mut buf) => {
                    let (n, peer) = match r {
                        Ok(v) => v,
                        Err(e) => {
                            error!("[l4] {} recv_from 出错：{e}", self.bind);
                            backoff_if_fd_exhausted(&e, "l4-udp").await;
                            continue;
                        }
                    };
                    let now = Instant::now();
                    // ① 老工作階段：續期後直接轉發。
                    if let Some(s) = sessions.touch(&peer, now) {
                        let up = s.up.clone();
                        if let Err(e) = up.send(&buf[..n]).await {
                            debug!("[l4] {}：转发到上游失败（{e}）", self.bind);
                        }
                        continue;
                    }
                    // ② 新工作階段：先看上限，再挑上游。
                    if sessions.len() >= UDP_MAX_SESSIONS {
                        let say = last_cap_warn
                            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
                        if say {
                            last_cap_warn = Some(now);
                            warn!(
                                "[l4] {}：会话数已达上限 {UDP_MAX_SESSIONS}，丢弃来自新地址的数据报 \
                                 —— ★ UDP 源地址可伪造，这条上限就是防 fd 被打光的",
                                self.bind
                            );
                        }
                        continue;
                    }
                    let rt = self.shared.current();
                    let Some(l) = rt.l4_listeners.iter().find(|l| l.listen == self.listen) else {
                        error!(
                            "[l4] 当前配置里找不到监听器 {}（UDP）—— 丢弃这个数据报",
                            self.listen
                        );
                        continue;
                    };
                    // ★ UDP **沒有分流**：ClientHello 是 TLS 的東西，而 QUIC 的 Initial 是加密的。
                    //   ⇒ 這裡只有備援那一組上游；設定層與裝載期都攔過 `udp` 上寫 `sni`/`alpn`。
                    let Some(t) = &l.target else {
                        warn!("[l4] {}：没有兜底上游，丢弃来自 {peer} 的数据报", self.bind);
                        continue;
                    };
                    let Some(idx) = t.pick_index_by(Some(peer.ip())) else {
                        warn!("[l4] {}：一个可用的上游都没有，丢弃来自 {peer} 的数据报", self.bind);
                        continue;
                    };
                    let up = &t.upstreams[idx];
                    let Some(dial) = up.dial_candidates().into_iter().next() else {
                        warn!("[l4] {}：上游 {} 没有可用地址，丢弃数据报", self.bind, up.addr);
                        continue;
                    };
                    let session = match open_udp_session(
                        listener.clone(),
                        &self.listen,
                        peer,
                        &up.addr,
                        dial,
                    )
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            warn!("[l4] {}：给 {peer} 建上游 socket 失败（{e}）", self.bind);
                            continue;
                        }
                    };
                    let sock = session.up.clone();
                    let task = session.task.abort_handle();
                    debug!("[l4] {}：新会话 {peer} → {}（UDP）", self.bind, up.addr);
                    // ★ `total` 是**事件點**（這一處），而 `active` 是派生的 ——
                    //   兩者有意走不同的路：前者要累計、後者要與工作階段表恆等。
                    self.conn.bump_total();
                    if sessions.admit(peer, session, now) == UdpAdmit::AtCapacity {
                        // ⚠ 走不到（上面已經查過上限），但**不假設**：真到了這裡
                        //   就把剛起的任務收掉，否則它會成為一個沒人認領的 fd。
                        task.abort();
                        continue;
                    }
                    if let Err(e) = sock.send(&buf[..n]).await {
                        debug!("[l4] {}：首包转发失败（{e}）", self.bind);
                    }
                }
            }
        }
    }

    /// 這個 service 用幾個執行緒（**G35 / G140**）。⚠ 回傳 `Some` 時 pingora
    /// **不再看**全域 `conf.threads` —— 見 `server/mod.rs` 的
    /// `service.threads().unwrap_or(conf.threads)`。
    fn threads(&self) -> Option<usize> {
        self.threads
    }

    fn name(&self) -> &str {
        &self.name
    }

    /// pingora 0.9.0（上游 `f82478a`）：新一代據此關掉「沒有任何服務認領」的繼承 fd。
    /// ⚠ 必須是 fd 表的**鍵**（帶前綴），⛔ 不是 bind 位址 —— 寫成位址，新一代會先關掉自己要繼承的
    ///   那個 fd；不實作（預設 `None`）會讓**整個處理程序**的清理靜默關閉。釘在單測 `*_服务声明的正是它的_fd_表键`。
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![self.fd_key.clone()])
    }
}

/// ClientHello 預讀的單測。**全部脫網**：用戶端那一半也走記憶體傳輸。
///
/// ⚠ ⚠ 在這一批之前，預讀這一整塊**只有端到端判據**（`tests/l4/run.sh` 那七條），
/// 而端到端判據的夾具是一份**手動拼出來的** ClientHello ——
/// ★ 那份位元組是「我以為的 ClientHello」，它與「真用戶端發來的那一份」
/// 恰好一致過，不等於永遠一致。這裡讓 **BoringSSL 自己**去造那一份。
#[cfg(test)]
mod peek_tests {
    use super::*;
    use pingora_boringssl::ssl::SslVerifyMode;
    use std::io::{Read, Write};

    /// 用戶端那一半的記憶體傳輸：寫下來的攢著，讀永遠說「還沒有」。
    #[derive(Default)]
    struct ClientSink {
        out: Vec<u8>,
    }

    impl Read for ClientSink {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "服务端一个字节都不回",
            ))
        }
    }

    impl Write for ClientSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.out.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 用 **BoringSSL 自己**造一個真的 ClientHello 出來。
    fn real_client_hello(sni: Option<&str>, alpn: &[&[u8]]) -> Vec<u8> {
        let mut b = SslContextBuilder::new(SslMethod::tls()).expect("建不出客户端 ctx");
        // ★ 我們永遠不回一個位元組，所以檢核根本走不到；關掉只是別讓它去找信任庫。
        b.set_verify(SslVerifyMode::NONE);
        let ctx = b.build();
        let mut ssl = Ssl::new(&ctx).expect("建不出客户端 SSL");
        if let Some(h) = sni {
            ssl.set_hostname(h).expect("设不上 SNI");
        }
        if !alpn.is_empty() {
            let mut wire = Vec::new();
            for p in alpn {
                wire.push(p.len() as u8);
                wire.extend_from_slice(p);
            }
            ssl.set_alpn_protos(&wire).expect("设不上 ALPN");
        }
        let mut s = SslStream::new(ssl, ClientSink::default()).expect("建不出客户端流");
        // 必然回 WANT_READ：ClientHello 寫出去了，而我們不給它任何回應。
        let _ = s.connect();
        let out = s.into_inner().out;
        assert!(!out.is_empty(), "客户端应当已经把 ClientHello 写出来了");
        out
    }

    /// 一次性喂完，推一次。
    fn peek(bytes: &[u8]) -> Step {
        let mut tls = new_peek_stream().expect("起不来预读状态机");
        tls.get_mut().buf.extend_from_slice(bytes);
        drive(&mut tls)
    }

    fn saw(step: Step) -> SeenHello {
        match step {
            Step::Saw(h) => h,
            Step::NeedMore => panic!("应当看清了，却说还要更多"),
            Step::NotTls(why) => panic!("应当看清了，却判成不是 TLS：{why}"),
        }
    }

    #[test]
    fn 真的_clienthello_读得出_sni_与_alpn() {
        let h = saw(peek(&real_client_hello(
            Some("api.example.com"),
            &[b"h2", b"http/1.1"],
        )));
        assert_eq!(h.sni.as_deref(), Some("api.example.com"));
        assert_eq!(h.alpn, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn 大小写不同的_sni_读回来是小写() {
        // ★ 分流規則那邊也會小寫化，兩邊都做才是冪等的；這裡釘的是本側。
        let h = saw(peek(&real_client_hello(Some("API.Example.COM"), &[])));
        assert_eq!(h.sni.as_deref(), Some("api.example.com"));
    }

    #[test]
    fn 没有_sni_只有_alpn() {
        // 對應 tests/l4/run.sh ④：無 SNI + ALPN=h2 也要能分流。
        let h = saw(peek(&real_client_hello(None, &[b"h2"])));
        assert_eq!(h.sni, None);
        assert_eq!(h.alpn, vec![b"h2".to_vec()]);
    }

    #[test]
    fn 两样都没有也是一个合法的_clienthello() {
        // ⚠ 「看清了但兩樣都沒有」與「沒看清」是兩件事：前者應當走備援規則，
        //   後者才是「不是 TLS」。判據把它們分開。
        let h = saw(peek(&real_client_hello(None, &[])));
        assert_eq!(h.sni, None);
        assert!(h.alpn.is_empty());
    }

    #[test]
    fn 不是_tls_的字节判成不是_tls() {
        match peek(b"GET / HTTP/1.0\r\n\r\n") {
            Step::NotTls(_) => {}
            Step::Saw(_) => panic!("一段 HTTP 请求不该被读成 ClientHello"),
            Step::NeedMore => panic!("一段 HTTP 请求不该被判成「还要更多」—— 那会一直挂到超时"),
        }
    }

    #[test]
    fn 半截_clienthello_是还要更多_而不是不是tls() {
        // ★ ★ 這一條擋的是最貴的一種失效：把「還沒讀全」判成「不是 TLS」，
        //   於是**每一條**分片到達的真連接都走備援 —— 而現場表現只是
        //   「SNI 分流偶爾不生效」，紀錄裡一個錯都沒有。
        let full = real_client_hello(Some("api.example.com"), &[b"h2"]);
        assert!(full.len() > 20, "样本太短，这条判据会退化");
        match peek(&full[..20]) {
            Step::NeedMore => {}
            Step::Saw(_) => panic!("只喂了 20 个字节不该就看清了"),
            Step::NotTls(why) => panic!("半截 ClientHello 被判成不是 TLS：{why}"),
        }
    }

    #[test]
    fn 分片喂进去与一次喂完读出同一样东西() {
        let full = real_client_hello(Some("b.internal.example.com"), &[b"h2", b"http/1.1"]);
        let mut tls = new_peek_stream().expect("起不来");
        let mut got = None;
        // 每次只喂 7 個位元組，逼它走完整條「還要更多」的路。
        for piece in full.chunks(7) {
            tls.get_mut().buf.extend_from_slice(piece);
            match drive(&mut tls) {
                Step::NeedMore => continue,
                Step::Saw(h) => {
                    got = Some(h);
                    break;
                }
                Step::NotTls(why) => panic!("分片喂被判成不是 TLS：{why}"),
            }
        }
        let h = got.expect("喂完了还没看清");
        assert_eq!(h.sni.as_deref(), Some("b.internal.example.com"));
        assert_eq!(h.alpn, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn 槽里有残留时下一条也不会捡到它() {
        // ★ ★ 這是 `drive()` 開頭那句「先清槽」的**唯一**判據。
        //   `drive()` 結尾的 `take()` 在每一條非 panic 路徑上都已清空槽 ⇒ 正常預讀
        //   永遠留不下殘留，「連著預讀兩條」那種判據**夠不著**這一句。
        //   真正能留下殘留的只有一條路：上一次 `drive()` 在 `accept()` 與 `take()` 之間
        //   恐慌展開 ⇒ 這裡**直接把殘留擺進槽裡**。
        // > ★ ★ ★ **一道門的註解寫「本門守 X」，不等於它判得動 X** ——
        // > 唯一的驗法是把 X 撤掉看它紅不紅。
        SEEN.with(|s| {
            *s.borrow_mut() = Some(SeenHello {
                sni: Some("stale.example.com".into()),
                alpn: vec![b"h2".to_vec()],
            })
        });
        match peek(b"NOT-TLS-AT-ALL\r\n") {
            Step::NotTls(_) => {}
            Step::Saw(h) => panic!("捡到了上一条留下的残留：sni={:?}", h.sni),
            Step::NeedMore => panic!("应当判成不是 TLS"),
        }
    }

    #[test]
    fn 同一个线程上连着预读三条_各读各的() {
        // ★ 這一條釘的是 `drive()` 那條「非 async ⇒ 不換執行緒」論證的**另一半**：
        //   同一個執行緒上前後幾條連接，各自讀到自己的東西。
        // ⚠ ⚠ **它夠不到「先清槽」那一句**（撤掉那一句本條照樣綠，實測）——
        //   守那一句的是上面那條。兩條判據各守各的，別把功勞記混。
        let a = saw(peek(&real_client_hello(
            Some("first.example.com"),
            &[b"h2"],
        )));
        assert_eq!(a.sni.as_deref(), Some("first.example.com"));
        let b = saw(peek(&real_client_hello(Some("second.example.com"), &[])));
        assert_eq!(b.sni.as_deref(), Some("second.example.com"));
        assert!(b.alpn.is_empty(), "第二条没带 ALPN，不该拿到第一条的");
        // 再來一條根本不是 TLS 的：它必須判「不是 TLS」，而不是撿到上一條的 SNI。
        match peek(b"NOT-TLS-AT-ALL\r\n") {
            Step::NotTls(_) => {}
            other => panic!(
                "非 TLS 流量在预读过 TLS 之后串台了：{}",
                match other {
                    Step::Saw(h) => format!("看成了 sni={:?}", h.sni),
                    Step::NeedMore => "还要更多".into(),
                    Step::NotTls(_) => unreachable!(),
                }
            ),
        }
    }
}

#[cfg(test)]
mod udp_session_tests {
    use super::*;

    /// ★ 載荷用 `&str` 而不是真工作階段：這張表的判據與網路無關，
    /// 而一個需要起 socket 才能測的「表」會逼著判據去測別的東西。
    fn table(cap: usize, idle_secs: u64) -> UdpSessionTable<&'static str> {
        UdpSessionTable::new(cap, Duration::from_secs(idle_secs))
    }

    fn peer(n: u16) -> SocketAddr {
        format!("127.0.0.1:{n}").parse().unwrap()
    }

    #[test]
    fn 到点才回收而且回收的是空闲的那条() {
        let t0 = Instant::now();
        let mut t = table(10, 60);
        assert_eq!(t.admit(peer(1), "a", t0), UdpAdmit::Ok);
        assert_eq!(t.admit(peer(2), "b", t0), UdpAdmit::Ok);
        // 59 秒：一條都不該走。
        assert!(t.sweep(t0 + Duration::from_secs(59)).is_empty());
        // 給 peer(1) 續一次期，然後跨過 60 秒：只有 peer(2) 該被回收。
        assert_eq!(t.touch(&peer(1), t0 + Duration::from_secs(59)), Some(&"a"));
        let dead = t.sweep(t0 + Duration::from_secs(61));
        assert_eq!(dead.len(), 1, "该回收的只有没续期的那条");
        assert_eq!(dead[0].0, peer(2));
        assert_eq!(t.len(), 1);
    }

    /// ⚠ **到頂要拒絕新的，不許踢老的**：LRU 在這裡是攻擊者的朋友 ——
    /// 一次偽造源位址的洪水會把正常用戶端全部擠出去。
    #[test]
    fn 到顶拒绝新会话而老会话不受影响() {
        let t0 = Instant::now();
        let mut t = table(2, 60);
        assert_eq!(t.admit(peer(1), "a", t0), UdpAdmit::Ok);
        assert_eq!(t.admit(peer(2), "b", t0), UdpAdmit::Ok);
        assert_eq!(t.admit(peer(3), "c", t0), UdpAdmit::AtCapacity);
        assert_eq!(t.len(), 2, "被拒的那条不许留在表里");
        // 老的兩條照常可用 —— 這一半才是「拒絕而不是踢掉」的判據。
        assert_eq!(t.touch(&peer(1), t0), Some(&"a"));
        assert_eq!(t.touch(&peer(2), t0), Some(&"b"));
        assert_eq!(t.touch(&peer(3), t0), None);
    }

    /// 回收之後騰出來的位置要能再被用上（否則上限會變成「一輩子只能接 N 條」）。
    #[test]
    fn 回收之后位置能重新用() {
        let t0 = Instant::now();
        let mut t = table(1, 60);
        assert_eq!(t.admit(peer(1), "a", t0), UdpAdmit::Ok);
        assert_eq!(t.admit(peer(2), "b", t0), UdpAdmit::AtCapacity);
        let dead = t.sweep(t0 + Duration::from_secs(60));
        assert_eq!(dead.len(), 1);
        assert!(t.is_empty());
        assert_eq!(
            t.admit(peer(2), "b", t0 + Duration::from_secs(60)),
            UdpAdmit::Ok
        );
    }
}

/// ★ ★ pingora 0.9.0（上游 `f82478a`）：新一代據 `Service::listen_addresses()` 關掉
/// 「沒有任何服務認領」的繼承 fd。**任何一個**服務回傳 `None`（pingora 的預設實作）⇒
/// **整個處理程序**的清理靜默關閉；回傳的若是 bind 位址而不是 fd 表的鍵 ⇒ 新一代會先把
/// 自己要繼承的那個 fd 關掉。⇒ 兩種錯都**編譯器不報、紀錄不提**，只能在這裡釘住。
#[cfg(test)]
mod listen_addresses_tests {
    use super::*;

    fn shared() -> Arc<SharedRuntime> {
        let o = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://a.com {\n  respond 200 \"ok\"\n}\n",
        );
        let cfg = o.config.expect("编得过");
        SharedRuntime::new(Arc::new(
            fulcrum_runtime::Runtime::build(&cfg).expect("建得出"),
        ))
    }

    #[test]
    fn tcp_服务声明的正是它的_fd_表键() {
        let s = TcpProxyService::new(
            shared(),
            "tcp://127.0.0.1:19001",
            "127.0.0.1:19001".to_string(),
            false,
            crate::conn_stats::ConnRegistry::new(),
        );
        assert_eq!(
            s.listen_addresses(),
            Some(vec![s.fd_key.clone()]),
            "L4 TCP 服务必须声明它的 fd 表键（缺省的 None 会关掉整个进程的清理）"
        );
        assert_eq!(
            s.fd_key, "fulcrum-l4-tcp:127.0.0.1:19001",
            "fd 表键的形状变了 —— 继承那一侧与 listen_addresses 必须一起改"
        );
    }

    #[test]
    fn udp_服务声明的正是它的_fd_表键() {
        let s = UdpProxyService::new(
            shared(),
            "udp://127.0.0.1:19002",
            "127.0.0.1:19002".to_string(),
            false,
            crate::conn_stats::ConnRegistry::new(),
        );
        assert_eq!(
            s.listen_addresses(),
            Some(vec![s.fd_key.clone()]),
            "L4 UDP 服务必须声明它的 fd 表键（缺省的 None 会关掉整个进程的清理）"
        );
        assert_eq!(s.fd_key, "fulcrum-l4-udp:127.0.0.1:19002");
    }
}
