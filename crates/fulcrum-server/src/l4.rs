//! L4 面：TCP 与 UDP 透传，都是自研。
//!
//! # 它与 HTTP 那一侧的分工
//!
//! | | HTTP 数据面 | 这里 |
//! |---|---|---|
//! | 入口 | Pingora 的 `ListeningService` | **自建 `Service`**，直接用 `tokio::net::TcpListener` |
//! | 路由 | 站点索引（Host + 端口）| **没有路由**——一条 TCP 连接上没有 Host、没有路径 |
//! | 挑上游 | [`ProxyTarget::pick`] | [`ProxyTarget::pick_by`]（**同一份实现**，见下） |
//! | 改内容 | `rewrite` / `header_up` / … | **一个字节都不看也不改** |
//!
//! ★ **挑上游那一格是共用的，不是抄的**：`pick` 拆成「取客户端 IP」+ `pick_by`，
//! L4 走后者 ⇒ 筛子与四种 `lb_policy` 只有一份实现。⚠ 抄一份的代价不在今天，
//! 两份在**下一次改动**时分家。
//!
//! # ★ ★ 自建监听器必须参与 socket 移交
//!
//! 表里有这个键就继承，没有就自己 bind **再放回表里**。⚠ 少了「放回去」那一步，
//! 第一次升级时这个端口会重新 bind，那一刻所有 L4 长连接一起断 —— 而 HTTP 一切正常。
//!
//! # 有意没有的东西
//!
//! - **`lb_policy` 之外的旋钮**：UDP 空闲超时与并发上限、TCP 连上游超时都是常量。
//!   ⏳ 让它们可配要先给 `l4` 块加选项位置。
//! - **TCP 的空闲超时**：⚠ L4 的典型对象是数据库连接，空闲几小时是正常的；
//!   给一个「看起来安全」的 30 秒，等于去杀一条上游认为健康的连接，
//!   现场表现是「数据库偶尔断线」。⇒ 空闲由两端自己管。

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

/// 连上游的超时。
///
/// ★ 它与「空闲超时」是两件事：**建连接**卡住是上游出了问题（半死的机器、
/// 被丢包的路径），而卡住期间客户端只会觉得「这个端口没反应」。
/// ⚠ 没有这条时，一个 SYN 被静默丢弃的上游会让每条连接挂到系统默认的
/// TCP 重传耗尽为止（Linux 上约 130 秒），而轮询会**继续往它身上送**。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 一个 `l4 tcp` 监听器对应的服务。
pub struct TcpProxyService {
    /// 这个 service 用几个线程（**G35 / G140**）。`None` = 跟全局 `conf.threads`。
    ///
    /// ★ 与 pingora `ListeningService.threads` 同名同义 —— 自建 service 也参与
    /// 同一套角色分配，⛔ 不许在这里另立一套。由 `serve()` 在建好之后设。
    pub threads: Option<usize>,
    /// ★ 拿的是**共享**运行时，不是启动时那一份：`POST /load` 换配置之后，
    /// 下一条连接就该按新的上游走。⚠ 监听地址本身换不了（启动时绑定），
    /// 管理面为此把 L4 端口纳入了「端口集变了就 409」的判据。
    shared: Arc<SharedRuntime>,
    /// 配置里写的监听地址原样（`:3306`），**在当前配置里找回自己**用的键。
    listen: String,
    /// 真正 bind 的地址（`--bind-host` 补齐之后）。
    bind: String,
    /// fd 表里的键。★ 加前缀是为了与 Pingora 原生服务的裸 `addr:port` 分开 ——
    /// 撞键的后果是两个服务抢同一个 fd，而症状会出现在**另一个**服务上。
    fd_key: String,
    /// 本进程是不是以 `-u` 起来的。★ 只有知道这一点，才能把「fd 表里没有」
    /// 分成「首次启动，正常」与「升级时交接失败，必须报错」两种。
    upgrading: bool,
    name: String,
    /// 这个监听器的连接计数格（**M2 批 O**）。★ 与 HTTP 那一侧**共用同一个** `ConnGuard`。
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
        // ★ `bind()` 顺手**声明**这一格 ⇒ 这个监听器从第一秒起就有一条 `0` 的样本，
        //   而不是「有连接了才出现」。
        let conn = conn_reg.bind(crate::conn_stats::Entrypoint::L4Tcp, &bind);
        Self {
            // ★ 缺省 `None` = 跟全局；由 `serve()` 按角色设（G140）。
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

/// 取 fd（继承）或新建监听器，并把新建的那个放回表里供下一代继承。
///
/// ⚠ ⚠ 这段的每一条注释都在描述一个**已经发生过**的失效，别删。
async fn build_listener(
    bind: &str,
    key: &str,
    fds: Option<ListenFds>,
    upgrading: bool,
) -> std::io::Result<TcpListener> {
    let Some(table) = fds else {
        // ★ 表缺失时**不能装作没事**：这条路径下 bind 出来的监听器不会被注册，
        //   于是它不参与下一次 socket 移交 —— 升级时连接被重置，而且悄无声息。
        if upgrading {
            return Err(std::io::Error::other(format!(
                "以 -u 升级启动却拿不到 fd 表，无法继承 {bind}：socket 移交已经失败了"
            )));
        }
        warn!("[l4] 没有 fd 表，只能在 {bind} 上新 bind —— ★ 这个监听器不会参与下一次 socket 移交");
        return TcpListener::bind(bind).await;
    };

    let mut table = table.lock().await;

    if let Some(&fd) = table.get(key) {
        info!("[l4] 继承了监听 fd={fd}（{key}）—— 升级窗口内这个端口没有重新 bind 过");
        // SAFETY: fd 由上一代经 SCM_RIGHTS 传来，此处接管其所有权，且同一个键只取用一次。
        //
        // ★ ManuallyDrop：从这里到成功交出所有权之间，**任何提前析构都会 `close(fd)`**，
        //   而 `Fds` 表里那条记录仍指着这个号码 —— 号码随后会被任意 `open()` 复用，
        //   下一次升级就把一个无关的 fd 当成监听 socket 发给下一代。`Fds` 没有 `remove()`。
        let std_listener = ManuallyDrop::new(unsafe { std::net::TcpListener::from_raw_fd(fd) });
        std_listener.set_nonblocking(true)?; // 失败时不析构，fd 保持有效
        let owned = ManuallyDrop::into_inner(std_listener);
        return match TcpListener::from_std(owned) {
            Ok(l) => Ok(l),
            Err(e) => {
                // ⚠ 残余风险：`from_std` 消耗了 owned，失败时 fd 已被关闭，而表里那条
                //   记录仍指向它。只在 tokio reactor 注册失败（资源耗尽）时出现，
                //   在不改 `Fds` 公开接口的前提下修不掉 —— 所以至少让它**大声说出来**。
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
    table.add(key.to_string(), fd);
    info!("[l4] 监听 {bind}（TCP 透传），fd={fd} 已登记为 {key}，下一代继承得到");
    Ok(listener)
}

/// accept 出错时该不该歇一下。
///
/// ★ ★ **fd 耗尽（EMFILE/ENFILE）时 `accept()` 会立刻返回错误且不阻塞**，
/// 于是「记一条日志然后继续循环」就变成满速空转 + 日志洪水 ——
/// 把「fd 快用完了」放大成「这台机器废了」，而且恰好在资源最紧张的时候。
/// 实测过：无退避时 369,156 条日志/秒。Pingora 自己的 accept 循环带着同一道防护。
async fn backoff_if_fd_exhausted(e: &std::io::Error, tag: &str) {
    // 24 = EMFILE（本进程 fd 用尽），23 = ENFILE（系统级用尽）
    if matches!(e.raw_os_error(), Some(24) | Some(23)) {
        error!("[{tag}] fd 耗尽（{e}），退避 1 秒——否则这个循环会满速空转");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// 看一眼 ClientHello 的超时。
///
/// ★ 它与 TCP 那条「有意不设空闲超时」**不冲突**，因为这一段的性质不同：
/// 连接已经接受、而上游**还没被选出来** —— 一个连上来什么都不发的对端，
/// 在这一段里占的是枢衡自己的任务，且没有任何对端资源与之对应。
/// ⚠ 没有这条时，它就是 L4 面上的 slowloris：一条不发字节的连接可以永远挂着。
const PEEK_TIMEOUT: Duration = Duration::from_secs(3);

/// 看 ClientHello 时最多缓存多少字节。
///
/// ★ 16 KiB 是一个 TLS record 的上限量级，而 ClientHello 通常 < 2 KiB。
/// ⚠ 超过就**不再等**，按「不是 TLS」处理走兜底 —— 缓存无上限的话，
/// 一条慢慢吐字节的连接能把内存吃到天上，而它连一个上游都还没占用。
const PEEK_MAX: usize = 16 * 1024;

/// 等一个 PROXY 头最多多久（**M2 批 D**）。
///
/// ⚠ ⚠ **它与 `PEEK_TIMEOUT` 是两段，不是一段**：一条既要收 PROXY 头、
/// 又配了 `sni` 分流的连接，最坏要 **3 + 3 = 6 秒**才选得出上游。
/// ★ 这个数字写在这里也写进 DSL 参考 —— 一个「加起来是多少」只能靠读代码得出的预算，
/// 迟早会被当成 3 秒。
const PP_TIMEOUT: Duration = Duration::from_secs(3);

/// 收下来的 PROXY 头。
struct Received {
    /// 真实客户端地址。
    ///
    /// ⚠ **`None` 是正常结果**：`LOCAL` 与 `PROXY UNKNOWN` 表示「这条连接没有真实客户端」
    /// （上游 LB 的健康检查就长这样）⇒ 调用方**继续用 socket 对端**。
    client: Option<SocketAddr>,
    /// 读头时**多读到**的字节 —— 它们已经是应用数据了，一个都不能丢。
    ///
    /// ★ ★ 这一格是本函数最容易漏的地方：TCP 是流，一次 `read` 很可能把
    /// PROXY 头与它后面的 ClientHello **一起**读回来。少了这一格，
    /// 上游收到的流就从 ClientHello 中间开始 —— 与批 C 那条「不重放」的缺陷同一个形状。
    leftover: Vec<u8>,
}

/// 读一个 PROXY 头。**只在 peer 落在信任清单里时才会被调用。**
///
/// ★ ★ ★ **不在清单里的连接，这个函数一次都不会被调用 —— 一个字节都不读。**
/// 那是 owner 拍板的口径（与安全基线里 XFF 那条同源），落法见 `dsl-reference.md`。
/// ⚠ 「读掉丢弃」需要先解析**攻击者控制**的长度字段才知道丢多少；「不读」完全不碰。
///
/// 返回 `Err` = **关掉这条连接**。⚠ 这里与「不在清单里」有意相反：
/// 一个**在信任清单里**的对端发来坏头，说明配置或对端出了问题，而此时我们
/// **已经吃掉了一部分字节、还原不回去** —— 硬把残缺的流转给上游只会把问题推远。
async fn read_proxy_header(client: &mut TcpStream) -> Result<Received, String> {
    use fulcrum_runtime::proxyproto::{self, Verdict};
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 256];
    let started = Instant::now();
    loop {
        // 先拿已有的字节问一次 —— ★ `decode` 是纯函数，重复问不要钱。
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
            // ★ 走不到（`decode` 自己的两条上界更早触发），但留着：
            //   它是这个循环**一定会停**的最后一道保证。
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

/// ClientHello 里我们要的那两样。
struct Peeked {
    /// 已经从客户端读走的**原始字节**。★ 必须原样重放给上游 ——
    /// 我们是透传，不是终止 TLS：少一个字节，上游的握手就废了。
    raw: Vec<u8>,
    /// `server_name` 扩展。`None` = 没带 SNI（合法，比如按 IP 直连）。
    sni: Option<String>,
    /// ALPN 清单，**原样的字节**（`h2` / `http/1.1` / …）。
    alpn: Vec<Vec<u8>>,
    /// 这真的是一个 TLS ClientHello 吗。`false` = 不是 TLS、或超时/超限。
    is_tls: bool,
}

/// 喂给 BoringSSL 的**内存传输**。
///
/// ⚠ ⚠ **写那一半一律丢掉，这是安全属性不是省事**：我们不是这条 TLS 连接的对端，
/// 往真客户端写一个字节（哪怕只是一个 alert）都等于冒充上游说话。
/// ★ 于是「不回 alert」这条纪律从「记得别写」变成了**写不出去** ——
/// 这个结构里根本没有通向客户端的那一头。
#[derive(Default)]
struct HelloFeed {
    /// 已经从客户端读到、准备喂给 BoringSSL 的字节。★ 有 [`PEEK_MAX`] 封顶。
    buf: Vec<u8>,
    /// BoringSSL 已经消化到哪儿。
    pos: usize,
}

impl std::io::Read for HelloFeed {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            // ⚠ ⚠ **必须是 `WouldBlock`，不能是 `Ok(0)`**：后者是 EOF，
            //   BoringSSL 会当成对端关了连接（不可重试）而把握手判死；
            //   而我们的意思只是「还没读到更多」。
            //   ★ boring 的 BIO 垫片认得这个 kind（`retriable_error`）并置重试位，
            //     于是 `accept()` 干净地回 `WANT_READ`。
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

/// 早回调看到的那两样。
struct SeenHello {
    sni: Option<String>,
    alpn: Vec<Vec<u8>>,
}

thread_local! {
    /// 早回调把看到的东西丢在这里，由 [`drive`] 当场取走。
    static SEEN: std::cell::RefCell<Option<SeenHello>> = const { std::cell::RefCell::new(None) };
}

/// 预读用的那个 `SSL_CTX`：**整个进程一个**，只挂一个早回调、不装任何证书。
///
/// ★ 不需要证书：`SSL_CTX_set_select_certificate_cb` 是**早回调**，
/// 在几乎所有 ClientHello 处理之前触发，而我们在它里面就把握手掐掉了。
fn peek_ctx() -> Option<&'static SslContext> {
    static CTX: OnceLock<Option<SslContext>> = OnceLock::new();
    CTX.get_or_init(|| {
        let mut b = match SslContextBuilder::new(SslMethod::tls_server()) {
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
            // ⚠ 这里拿到的是「**客户端提供了**哪些」，不是协商结果 ——
            //   早回调发生在 ALPN 协商之前。分流规则要的正是前者。
            // ★ 读法与 fulcrum-tls 挑证书那条路**共用同一份** RFC 7301 框架实现，
            //   不是各写一份（D18/G66 同一条理由）。
            let alpn = hello
                .get_extension(ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION)
                .and_then(alpn_list)
                .unwrap_or_default();
            SEEN.with(|s| *s.borrow_mut() = Some(SeenHello { sni, alpn }));
            // ★ ★ ★ **永远中止**。我们只是看一眼：这条连接的 TLS 要由上游去终止，
            //   继续下去就变成中间人了。
            Err(SelectCertError::ERROR)
        });
        Some(b.build())
    })
    .as_ref()
}

/// 推进一次握手之后的结论。
enum Step {
    /// 早回调开过火 ⇒ 这是一个 TLS ClientHello，而且我们看清了。
    Saw(SeenHello),
    /// 还没读全，再喂。
    NeedMore,
    /// 这不像 TLS（或者读到一半坏了）。带上说得清的原因。
    NotTls(String),
}

/// 把已经喂进去的字节推一次握手，并把早回调看到的东西取回来。
///
/// ⚠ ⚠ **本函数不是 `async fn`，而这就是整段的安全论证**：早回调由 BoringSSL 在
/// `accept()` 里直接调用（不换线程），而非 async 函数里写不出 `.await`
/// ⇒「清槽 → 握手 → 取槽」三步之间 thread-local 槽不可能被别的连接插进来。
/// **改成 `async fn` 会让论证失效，表现是串台**：一条连接读到另一条的 SNI，都不报错。
///
/// ★ 先看槽、再看返回码：早回调开过火就说明东西到手了，而这时握手必然失败 ——
/// 那不是错误，正是我们要的。
fn drive(tls: &mut SslStream<HelloFeed>) -> Step {
    // ⚠ 先清：同一个线程上一条连接可能留下过东西。
    SEEN.with(|s| *s.borrow_mut() = None);
    let outcome = tls.accept();
    let seen = SEEN.with(|s| s.borrow_mut().take());
    match (seen, outcome) {
        (Some(h), _) => Step::Saw(h),
        // ⚠ 走不到：早回调永远回 `Err`。真走到了说明回调没挂上，
        //   那是**配置错**而不是「对端不是 TLS」，所以它有自己的措辞。
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

/// 只读 ClientHello，**不完成握手**。
///
/// 走的是 **BoringSSL 的早回调**（G104 第 ② 处）：一台真的握手状态机 + 一条内存传输，
/// 字节喂进 [`HelloFeed`]，`SSL_CTX_set_select_certificate_cb` 在 ClientHello 解完时触发，
/// 抄走 SNI 与 ALPN 之后**当场把握手掐掉**。
/// ★ 不手写 ClientHello 解析（那是在解析攻击者控制的二进制）。
///
/// ★ **一处行为差异说在明处**：早回调发生在几乎所有 ClientHello 校验**之前**，
/// 于是一个 rustls 会当场拒掉的 ClientHello 在这一侧照样能被读出 SNI 并分流。
/// **这正是想要的语义** —— 枢衡不替上游拒绝一个它自己也许能处理的客户端。
///
/// ⚠ ⚠ **无论结果如何，读走的字节都在 `raw` 里**（走兜底时要重放），
/// `already` 是收 PROXY 头时多读到的那一段 —— 它必须**先喂给状态机、也进 `raw`**，
/// 否则「先 PROXY 头再 ClientHello」的连接会全部走兜底，
/// **而日志上看起来只是「这个客户端没带 SNI」**。
async fn peek_client_hello(client: &mut TcpStream, already: Vec<u8>) -> Peeked {
    let mut raw: Vec<u8> = Vec::with_capacity(2048);
    let Some(tls) = new_peek_stream() else {
        // ⚠ 建不出状态机时**照样把字节原样带走**：这条连接仍然要能走兜底，
        //   而不是被丢掉开头几个字节。原因已经在 error! 里说清了。
        return not_tls(raw);
    };
    let mut tls = tls;
    let mut chunk = [0u8; 4096];
    let started = Instant::now();
    // ★ 把上一段多读到的字节当成「刚读到的一块」走同一条路 ——
    //   有意不另写一份喂法：两份「差不多的」喂法会在下一次改动时分家。
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
        // ★ 有 `pending` 就先消化它，一个字节都不去 socket 上读。
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
        // ⚠ **只追加新读到的那一段**：`HelloFeed` 自己记着消化到哪儿了，
        //   重喂整个缓冲区会让状态机把同样的字节吃两遍。
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

/// 起一台只用来看一眼的握手状态机。`None` = 起不来（已 `error!`）。
fn new_peek_stream() -> Option<SslStream<HelloFeed>> {
    let ctx = peek_ctx()?;
    let ssl = Ssl::new(ctx)
        .map_err(|e| error!("[l4] 建不出预读用的 SSL：{e}"))
        .ok()?;
    SslStream::new(ssl, HelloFeed::default())
        .map_err(|e| error!("[l4] 建不出预读用的内存传输：{e}"))
        .ok()
}

/// 把一条已接受的连接接到某个上游去，然后双向搬字节。
///
/// ★ 「挑哪个上游」在**每条连接**上决定一次，取的是当时的运行时 ——
/// 于是 `POST /load` 换掉上游之后，新连接立刻走新的，老连接不受影响。
async fn handle_conn(shared: Arc<SharedRuntime>, listen: String, mut client: TcpStream) {
    let peer_ip = client.peer_addr().ok().map(|a| a.ip());
    let rt = shared.current();
    let Some(l) = rt.l4_listeners.iter().find(|l| l.listen == listen) else {
        // ⚠ 走到这里说明当前配置里已经没有这个监听器了。★ 管理面挡着端口集变化，
        //   所以这**不该**发生；真发生了就把连接关掉并说清楚，而不是静默丢弃。
        error!(
            "[l4] 当前配置里找不到监听器 {listen} —— 关闭这条连接（这不该发生，管理面本该挡住端口集变化）"
        );
        return;
    };
    // ── 收 PROXY 头（批 D）──────────────────────────────────────────────
    //
    // ★ ★ ★ **不在信任清单里 ⇒ 这一整段一步都不走，一个字节都不读。**
    //   那 12/16 字节（如果真有的话）会原样当成应用数据流给上游 ——
    //   owner 拍板的口径，理由见 `dsl-reference.md` §二那一节。
    //   ⚠ 清单为空时 `trusts_proxy_protocol` 恒 false，所以「没配」与「配了但不匹配」
    //     走的是同一条路 —— 这是有意的：**一份空清单不是「信任所有人」**。
    let mut real_client: Option<SocketAddr> = None;
    let mut carried: Vec<u8> = Vec::new();
    if let Some(ip) = peer_ip
        && l.trusts_proxy_protocol(ip)
    {
        match read_proxy_header(&mut client).await {
            Ok(r) => {
                // ⚠ `client` 为 `None` 是 LOCAL / UNKNOWN —— **不是错误**，
                //   此时继续用 socket 对端（健康检查就长这样）。
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
                // ★ 在清单里却发来坏头 ⇒ 关连接。理由见 `read_proxy_header` 的文档：
                //   我们已经吃掉了一部分字节，还原不回去。
                warn!("[l4] {listen}：信任的来源 {ip} 发来的 PROXY 头有问题（{why}），关闭连接");
                return;
            }
        }
    }
    // ★ ★ 从这里往下，「客户端是谁」一律用 `effective_ip` —— **不用 `peer_ip`**。
    //   ⚠ 挑上游那一步尤其要紧：`lb_policy ip_hash` 拿 socket 对端去算，
    //   会让**前面那台 LB 的所有连接哈希到同一个上游**，而现场表现只是「负载不均」。
    let effective_ip = real_client.map(|a| a.ip()).or(peer_ip);
    // ── 选目标（批 C：可能要先看一眼 ClientHello）────────────────────────
    //
    // ★ **没有分流规则时一步都不多走**：不读、不等、不缓存 —— 批 A 的形状原样保留。
    //   ⚠ 「顺手都 peek 一下反正也不贵」会给每一条 L4 连接加上一次读与一个超时预算，
    //   而数据库连接那类对端**可能根本不先说话**（服务端先发 greeting），那样就死等到超时。
    let (target, prelude) = if l.rules.is_empty() {
        // ⚠ 没有分流规则时，收 PROXY 头多读到的那一段**仍然要重放** ——
        //   它已经是应用数据了。★ 少了这个 `carried`，一条「LB 发 PROXY 头 +
        //   紧跟着第一批数据」的连接会丢掉开头那几个字节。
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
            // ★ **按书写顺序，第一个命中即用**（DSL 参考 §4.5）。
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
                    // ★ 「只服务我认得的那几个名字」是一种合法配置 ——
                    //   但它必须**说出来**，否则现场只有「连上就断」。
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
        // ★ 与 HTTP 那边的 502 同一条语义：一个可用的上游都没有。
        //   ⚠ L4 上没有状态码可回，能做的只有**干净地关掉** —— 而且要留一行日志，
        //   否则现场是「连上就断」，看不出是上游全挂还是端口配错。
        warn!("[l4] {listen}：一个可用的上游都没有（解析不出地址或被判死），关闭连接");
        return;
    };
    // ★ ★ **建连阶段可以换上游，这一点与 HTTP 那边有意不同。**
    //
    //   HTTP 那边一个上游的候选全失败就回 502，**不换上游** —— 理由是那条路上
    //   「换一个再来一次」会滑向重试语义（请求体可能已经发出去了）。
    //   而 L4 在建连之前**一个字节都还没走**：客户端还没发任何东西，上游也还没被打扰，
    //   换一个是完全透明的。⇒ 从策略挑中的那个开始，按上游列表顺序往后试一圈。
    //
    //   ⚠ 不这么做的代价是具体的：两个上游、挂掉一个，客户端会**每两条连接坏一条**，
    //   而枢衡自己一切正常 —— 这正是「L4 面能不能真用」的分界线。
    let n = target.upstreams.len();
    let mut chosen: Option<(&fulcrum_runtime::Upstream, TcpStream)> = None;
    let mut last_err: Option<String> = None;
    'outer: for k in 0..n {
        let up = &target.upstreams[(start + k) % n];
        let candidates = up.dial_candidates();
        if candidates.is_empty() || !up.is_healthy() {
            // 与 `pick_index_by` 同一把筛子：解析不出地址、或被健康检查判死的，跳过。
            continue;
        }
        // ★ 逐个候选试，不是只试第一个。理由与 HTTP 那边逐字相同：
        //   `localhost` 解出来第一个可能是 `[::1]`，而上游只监听 `127.0.0.1`。
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
    // ── 发 PROXY 头（批 D）──────────────────────────────────────────────
    //
    // ★ ★ ★ **它必须是这条连接上的第一批字节 —— 在重放 prelude 之前。**
    //   ⚠ 顺序反了，上游会把 ClientHello 的开头当成 PROXY 头去解析，
    //   而现场表现是「上游握手失败」，与「我们没重放」长得一模一样。
    //
    // ★ 报给上游的是 `effective_ip` 那一侧的地址：我们自己也收了一个 PROXY 头时，
    //   写出去的是**头里那个客户端**（链式传递）—— 隔了几层，上游看到的仍是最初那个人。
    if let Some(ver) = l.proxy_protocol {
        // ⚠ ⚠ 造头要**两个**地址，且族必须一致；拿不到本地地址时不硬凑
        //   （`encode` 会退成 LOCAL / UNKNOWN，让上游知道「没有可报的客户端」）。
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
    // ★ ★ ★ **先把 peek 时读走的字节原样送过去**，再进双向搬运。
    //   ⚠ 少了这一步，上游收到的 TLS 流会**从 ClientHello 中间开始** ——
    //   现场表现是「握手失败」，而枢衡这边一切正常：连接建立了、字节也在流动。
    //   ★ 这是「透传」这两个字的全部重量：我们看了，但**不许吃掉**。
    if !prelude.is_empty()
        && let Err(e) = upstream.write_all(&prelude).await
    {
        debug!("[l4] {listen}：重放 ClientHello 失败（{e}）");
        up.release();
        return;
    }
    debug!("[l4] {listen} → {}（透传）", up.addr);
    // ★ `copy_bidirectional` 一端读到 EOF 就把另一端 shutdown 写方向，
    //   于是**半关闭**（一端说完了、另一端还在说）能透过去 —— 这不是细节：
    //   不少协议（含某些数据库客户端）靠半关闭表示「我说完了」。
    match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        // ⚠ `c2u` **不含**上面重放的那一段：那是我们自己写过去的，不经过 `copy_bidirectional`。
        //   ⇒ 把重放量单独报出来，否则日志会说「客户端→上游 0B」而上游明明收到了 ClientHello。
        Ok((c2u, u2c)) => debug!(
            "[l4] {listen} 连接结束：客户端→上游 {c2u}B（另有重放 {}B），上游→客户端 {u2c}B",
            prelude.len()
        ),
        // ⚠ 这里**只用 debug**：连接被任一端重置在 L4 上是家常便饭
        //   （客户端 Ctrl-C、上游重启）。★ 把它记成 error 会让日志里全是噪音，
        //   而噪音会把真正该看的那几行埋掉。
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
        // ★ 这个参数由 `listener_tasks_per_fd` 配置驱动，Pingora 原生服务会真的按它
        //   开多个 accept 任务。本服务**没有实现**它 —— 与其一声不响地只开一个
        //   （配置就成了谎话），不如直接拒绝启动。
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
                // ⚠ 起不来就是起不来，**不许降级成一条警告**：一个「HTTP 好好的、
                //   而那个数据库端口没人在听」的进程，查起来会先怀疑防火墙。
                error!("[l4] 在 {} 上建不出监听器：{e}", self.bind);
                return;
            }
        };

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    // 停止 accept，**已建立的连接留在本代继续跑完**
                    //（排空由 Pingora 的 grace period 管，与 HTTP 那边同一个预算）。
                    info!("[l4] {} 收到停机信号，停止 accept（已建立的连接继续）", self.bind);
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((sock, _peer)) => {
                            // ★ Nagle 关掉：L4 透传里典型的是小包一来一回（数据库协议、
                            //   游戏心跳），而 Nagle 会为了凑满一个段**故意等** ——
                            //   代价是延迟，收益在这里几乎为零。
                            if let Err(e) = sock.set_nodelay(true) {
                                debug!("[l4] set_nodelay 失败（{e}）—— 继续，它只影响延迟");
                            }
                            let shared = self.shared.clone();
                            let listen = self.listen.clone();
                            // ★ 连接计数（**M2 批 O**）：包一层而不改 `handle_conn` 的签名。
                            let g = self.conn.guard();
                            tokio::spawn(async move {
                                // ⚠ ⚠ **必须绑名字**：写成 `let _ = g;` 会当场 drop，
                                //   于是 active 恒为 0 而 total 照涨，且不会有东西红。
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

    /// 这个 service 用几个线程（**G35 / G140**）。⚠ 返回 `Some` 时 pingora
    /// **不再看**全局 `conf.threads` —— 见 `server/mod.rs` 的
    /// `service.threads().unwrap_or(conf.threads)`。
    fn threads(&self) -> Option<usize> {
        self.threads
    }

    fn name(&self) -> &str {
        &self.name
    }
}
// ══════════════════════════════════════════════════════════════════════════
// UDP 透传（M2 批 B）
// ══════════════════════════════════════════════════════════════════════════
//
// ★ ★ **UDP 与 TCP 不是「同一件事换个协议」**，三处差别都会咬人：
//
// | | TCP | UDP |
// |---|---|---|
// | 会话边界 | 内核给的（accept / FIN）| **没有** —— 自己按客户端地址维护会话表 |
// | 空闲超时 | 有意不设 | **必须有**，否则每个出现过的地址都永久占一个 socket |
// | 停机 | 停止 `accept` | ★ **停止 `recv_from`** —— 不停的话老一代会偷走新一代的数据报 |
//
// ⚠ 最后一行：换代时两代在 `recv_from` **同一个** socket，谁先醒谁拿走。
// 老一代吃掉本该属于新一代的会话首包，而客户端只看到「换代那几秒有些请求没回应」，
// 两边日志都正常。

/// 会话空闲多久回收。
///
/// ★ TCP 那边**有意不设**空闲超时，UDP 这边**必须有**：UDP 没有 FIN，
/// 不回收的话，每一个出现过的客户端地址都会永远占着一个上游 socket 与一个任务。
/// ⚠ ⚠ 而 **UDP 源地址是可伪造的** ⇒ 那是一条**远程可触发**的 fd 耗尽路径，
/// 不是理论风险。
/// ⚠ 60 秒是折中：DNS 查询在毫秒级、游戏心跳通常远短于它；
/// ⏳ 让它可配是后续批的事（`l4` 块目前没有放选项的位置，见 dsl-reference §4.5）。
const UDP_SESSION_IDLE: Duration = Duration::from_secs(60);

/// 每个监听器的并发会话上限。
///
/// ★ 与上面那条是同一件事的两半：超时管「什么时候放」，上限管「最多同时占多少」。
/// ⚠ 没有上限时，一次伪造源地址的洪水能在超时到期**之前**就把 fd 吃光。
const UDP_MAX_SESSIONS: usize = 1024;

/// 单个数据报缓冲区。
///
/// ★ 取 65535（UDP 载荷上限）而不是「够用就好」的 2 KiB：`recv` 读不下的部分
/// 会被**静默丢弃**（不带 `MSG_TRUNC` 就看不出来），现场表现是「大包偶尔损坏」——
/// 那种缺陷会被归咎于网络。
/// ⚠ 代价写在明处：每条会话一份 ⇒ 上限 1024 × 64 KiB ≈ **64 MiB** 的最坏占用。
const UDP_DATAGRAM_MAX: usize = 65535;

/// 会话清扫的打点间隔。★ 与空闲超时分开：超时是**判据**，这个是**检查频率**。
const UDP_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// 会话表里的一格。
struct Session<T> {
    payload: T,
    last_seen: Instant,
}

/// 插入的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum UdpAdmit {
    Ok,
    /// 到达并发上限，**没有插入**。
    AtCapacity,
}

/// UDP 会话表。
///
/// ★ ★ 它被**故意做成不碰网络、不看时钟**的结构：`now` 从参数进来、载荷是泛型，
/// 于是「到点回收」「到顶拒绝」这两条可以**确定性地单测**，
/// 不必在门禁里真等 60 秒。⚠ 这是 G56 那一块（续期判定模块里没有一个
/// `SystemTime::now()`）的同一招 —— **时间是参数，不是环境**。
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

    /// 已有会话就**续期**并把载荷给出去；没有就 `None`。
    pub fn touch(&mut self, peer: &SocketAddr, now: Instant) -> Option<&T> {
        let s = self.map.get_mut(peer)?;
        s.last_seen = now;
        Some(&s.payload)
    }

    /// 收一条新会话。
    ///
    /// ★ 到顶时**拒绝新的**，而不是踢掉一条老的 —— 后者会让一次洪水把正常客户端
    /// 全部挤出去（LRU 在这里是攻击者的朋友）。
    /// ⚠ 拒绝时**不接管载荷**：谁创建谁负责收尾，比「谁拒绝谁负责」少一条隐式约定。
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

    /// 回收空闲超过 `idle` 的会话，把载荷交还给调用方处理（比如 `abort` 任务）。
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

/// 生产环境里一条会话真正带着的东西。
pub struct LiveSession {
    /// 连到上游的那个 socket。转发用它。
    up: Arc<UdpSocket>,
    /// 回包任务。★ 回收会话时必须 `abort()` —— 否则任务继续持有那个 socket，
    /// 而表里已经没有它了：**表看起来在回收，fd 却没还回来**。
    task: tokio::task::JoinHandle<()>,
    /// 上游地址，只用于日志。
    upstream: String,
}

/// 一个 `l4 udp` 监听器对应的服务。
pub struct UdpProxyService {
    /// 这个 service 用几个线程（**G35 / G140**）。`None` = 跟全局 `conf.threads`。
    ///
    /// ★ 与 pingora `ListeningService.threads` 同名同义 —— 自建 service 也参与
    /// 同一套角色分配，⛔ 不许在这里另立一套。由 `serve()` 在建好之后设。
    pub threads: Option<usize>,
    shared: Arc<SharedRuntime>,
    listen: String,
    bind: String,
    fd_key: String,
    upgrading: bool,
    name: String,
    /// 这个监听器的连接计数格（**M2 批 O**）。⚠ UDP 那一格数的是**会话**，且是从
    /// `sessions.len()` **派生**的 —— 见 `run()` 里循环开头那一行。
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
            // ★ 缺省 `None` = 跟全局；由 `serve()` 按角色设（G140）。
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

/// 取 fd（继承）或新建 UDP 监听 socket，并把新建的放回表里供下一代继承。
///
/// ⚠ 与 TCP 那一份**逐字同构**，只是类型不同。两份都留着而不是抽成泛型：
/// `TcpListener` 与 `UdpSocket` 的接管路径（`from_std`）不共享 trait，
/// 硬抽出来的那一层只会把 SAFETY 注释挤走 —— 而那几段注释是这里最值钱的东西。
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

    let mut table = table.lock().await;

    if let Some(&fd) = table.get(key) {
        info!("[l4] 继承了 UDP 监听 fd={fd}（{key}）—— 升级窗口内这个端口没有重新 bind 过");
        // SAFETY: fd 由上一代经 SCM_RIGHTS 传来，此处接管其所有权，且同一个键只取用一次。
        // ★ ManuallyDrop 的理由与 TCP 那一份完全相同：提前析构会 `close(fd)`，
        //   而表里那条记录仍指着这个号码，下一次升级就会把一个无关的 fd 传下去。
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
    table.add(key.to_string(), fd);
    info!("[l4] 监听 {bind}（UDP 透传），fd={fd} 已登记为 {key}，下一代继承得到");
    Ok(sock)
}

/// 给一条新客户端地址建会话：连上游 + 起回包任务。
async fn open_udp_session(
    listener: Arc<UdpSocket>,
    listen: &str,
    peer: SocketAddr,
    upstream_addr: &str,
    dial: SocketAddr,
) -> std::io::Result<LiveSession> {
    // ★ 按上游地址族选本地绑定地址：给 IPv6 上游 bind 一个 `0.0.0.0` 会直接失败。
    let local: SocketAddr = if dial.is_ipv4() {
        "0.0.0.0:0".parse().expect("常量地址")
    } else {
        "[::]:0".parse().expect("常量地址")
    };
    let up = UdpSocket::bind(local).await?;
    // ⚠ UDP 的 `connect` **不是握手**，它只是记下默认对端（顺带让内核过滤掉别人发来的包）。
    //   ★ 所以它几乎不会失败，也**证明不了上游活着** —— 这正是为什么 TCP 那边的
    //   「建连阶段换上游」在这里**没有对应物**：UDP 上没有任何东西能在发包之前
    //   告诉我们这个上游是死的。⇒ 挑一次就用它，换上游要等被动熔断（`passive_fail`，还没做）。
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
        // ⚠ tokio 的第一次 `tick()` 立即返回，跳掉它 —— 否则服务一起来就先扫一遍空表。
        sweep.tick().await;
        // ★ 到顶时**限流地说**：不限流的话，一次洪水会让日志本身变成第二次攻击。
        let mut last_cap_warn: Option<Instant> = None;

        loop {
            // ── 连接计数：`active` 从会话表**派生**（**M2 批 O**）──────────────
            //
            // ★ ★ ⛔ **不在旁边再记一份 `+1/-1`**：`sessions.len()` 本身就是权威，
            //   而另记一份会让清扫 `abort()`、到上限被拒、停机不再收包这三条路
            //   各需要记得减一 —— 那正是 D18/G66 那个分家形状。
            // ⚠ ⚠ **写在循环体开头而不是末尾**：下面 recv 那一支有七八处 `continue`，
            //   而 `continue` **跳过循环体末尾** ⇒ 写在末尾的话，「到上限被拒」
            //   「找不到上游」「建 socket 失败」这些路径上它根本不执行。
            // ★ 开头则**每一轮迭代必经**；而阻塞在 `select!` 期间 `sessions` 不可能变
            //   （这个循环是它唯一的改写者）⇒ 这个读数是**精确**的，不是陈旧的。
            self.conn.set_active(sessions.len());
            tokio::select! {
                _ = shutdown.changed() => {
                    // ★ ★ ★ **这一支是 UDP 与 TCP 最重要的差别**：停机信号一到就
                    //   **不再 `recv_from`**。⚠ 继续收的话，老一代会与新一代抢同一个
                    //   socket 上的数据报（两代持有的是同一个 fd），把本该属于新一代的
                    //   首包吃掉 —— 客户端只会看到「换代那几秒有些请求没回应」。
                    //   ⇒ 已建立会话的**回包任务不动**（它们只 `send_to`，不抢 `recv`），
                    //     在飞的请求还能拿到应答，与 TCP 那边「老连接继续跑完」一致。
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
                        // ★ 必须 abort：任务持有那个上游 socket，光从表里删掉
                        //   只是把 fd 从「看得见」变成「看不见」。
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
                    // ① 老会话：续期后直接转发。
                    if let Some(s) = sessions.touch(&peer, now) {
                        let up = s.up.clone();
                        if let Err(e) = up.send(&buf[..n]).await {
                            debug!("[l4] {}：转发到上游失败（{e}）", self.bind);
                        }
                        continue;
                    }
                    // ② 新会话：先看上限，再挑上游。
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
                    // ★ UDP **没有分流**：ClientHello 是 TLS 的东西，而 QUIC 的 Initial 是加密的。
                    //   ⇒ 这里只有兜底那一组上游；配置层与装载期都拦过 `udp` 上写 `sni`/`alpn`。
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
                    // ★ `total` 是**事件点**（这一处），而 `active` 是派生的 ——
                    //   两者有意走不同的路：前者要累计、后者要与会话表恒等。
                    self.conn.bump_total();
                    if sessions.admit(peer, session, now) == UdpAdmit::AtCapacity {
                        // ⚠ 走不到（上面已经查过上限），但**不假设**：真到了这里
                        //   就把刚起的任务收掉，否则它会成为一个没人认领的 fd。
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

    /// 这个 service 用几个线程（**G35 / G140**）。⚠ 返回 `Some` 时 pingora
    /// **不再看**全局 `conf.threads` —— 见 `server/mod.rs` 的
    /// `service.threads().unwrap_or(conf.threads)`。
    fn threads(&self) -> Option<usize> {
        self.threads
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// ClientHello 预读的单测。**全部脱网**：客户端那一半也走内存传输。
///
/// ⚠ ⚠ 在这一批之前，预读这一整块**只有端到端判据**（`tests/l4/run.sh` 那七条），
/// 而端到端判据的夹具是一份**手工拼出来的** ClientHello ——
/// ★ 那份字节是「我以为的 ClientHello」，它与「真客户端发来的那一份」
/// 恰好一致过，不等于永远一致。这里让 **BoringSSL 自己**去造那一份。
#[cfg(test)]
mod peek_tests {
    use super::*;
    use pingora_boringssl::ssl::SslVerifyMode;
    use std::io::{Read, Write};

    /// 客户端那一半的内存传输：写下来的攒着，读永远说「还没有」。
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

    /// 用 **BoringSSL 自己**造一个真的 ClientHello 出来。
    fn real_client_hello(sni: Option<&str>, alpn: &[&[u8]]) -> Vec<u8> {
        let mut b = SslContextBuilder::new(SslMethod::tls_client()).expect("建不出客户端 ctx");
        // ★ 我们永远不回一个字节，所以校验根本走不到；关掉只是别让它去找信任库。
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
        // 必然回 WANT_READ：ClientHello 写出去了，而我们不给它任何回应。
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
        // ★ 分流规则那边也会小写化，两边都做才是幂等的；这里钉的是本侧。
        let h = saw(peek(&real_client_hello(Some("API.Example.COM"), &[])));
        assert_eq!(h.sni.as_deref(), Some("api.example.com"));
    }

    #[test]
    fn 没有_sni_只有_alpn() {
        // 对应 tests/l4/run.sh ④：无 SNI + ALPN=h2 也要能分流。
        let h = saw(peek(&real_client_hello(None, &[b"h2"])));
        assert_eq!(h.sni, None);
        assert_eq!(h.alpn, vec![b"h2".to_vec()]);
    }

    #[test]
    fn 两样都没有也是一个合法的_clienthello() {
        // ⚠ 「看清了但两样都没有」与「没看清」是两件事：前者应当走兜底规则，
        //   后者才是「不是 TLS」。判据把它们分开。
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
        // ★ ★ 这一条挡的是最贵的一种失效：把「还没读全」判成「不是 TLS」，
        //   于是**每一条**分片到达的真连接都走兜底 —— 而现场表现只是
        //   「SNI 分流偶尔不生效」，日志里一个错都没有。
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
        // 每次只喂 7 个字节，逼它走完整条「还要更多」的路。
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
        // ★ ★ 这是 `drive()` 开头那句「先清槽」的**唯一**判据。
        //   `drive()` 结尾的 `take()` 在每一条非 panic 路径上都已清空槽 ⇒ 正常预读
        //   永远留不下残留，「连着预读两条」那种判据**够不着**这一句。
        //   真正能留下残留的只有一条路：上一次 `drive()` 在 `accept()` 与 `take()` 之间
        //   恐慌展开 ⇒ 这里**直接把残留摆进槽里**。
        // > ★ ★ ★ **一道门的注释写「本门守 X」，不等于它判得动 X** ——
        // > 唯一的验法是把 X 撤掉看它红不红。
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
        // ★ 这一条钉的是 `drive()` 那条「非 async ⇒ 不换线程」论证的**另一半**：
        //   同一个线程上前后几条连接，各自读到自己的东西。
        // ⚠ ⚠ **它够不到「先清槽」那一句**（撤掉那一句本条照样绿，实测）——
        //   守那一句的是上面那条。两条判据各守各的，别把功劳记混。
        let a = saw(peek(&real_client_hello(
            Some("first.example.com"),
            &[b"h2"],
        )));
        assert_eq!(a.sni.as_deref(), Some("first.example.com"));
        let b = saw(peek(&real_client_hello(Some("second.example.com"), &[])));
        assert_eq!(b.sni.as_deref(), Some("second.example.com"));
        assert!(b.alpn.is_empty(), "第二条没带 ALPN，不该拿到第一条的");
        // 再来一条根本不是 TLS 的：它必须判「不是 TLS」，而不是捡到上一条的 SNI。
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

    /// ★ 载荷用 `&str` 而不是真会话：这张表的判据与网络无关，
    /// 而一个需要起 socket 才能测的「表」会逼着判据去测别的东西。
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
        // 59 秒：一条都不该走。
        assert!(t.sweep(t0 + Duration::from_secs(59)).is_empty());
        // 给 peer(1) 续一次期，然后跨过 60 秒：只有 peer(2) 该被回收。
        assert_eq!(t.touch(&peer(1), t0 + Duration::from_secs(59)), Some(&"a"));
        let dead = t.sweep(t0 + Duration::from_secs(61));
        assert_eq!(dead.len(), 1, "该回收的只有没续期的那条");
        assert_eq!(dead[0].0, peer(2));
        assert_eq!(t.len(), 1);
    }

    /// ⚠ **到顶要拒绝新的，不许踢老的**：LRU 在这里是攻击者的朋友 ——
    /// 一次伪造源地址的洪水会把正常客户端全部挤出去。
    #[test]
    fn 到顶拒绝新会话而老会话不受影响() {
        let t0 = Instant::now();
        let mut t = table(2, 60);
        assert_eq!(t.admit(peer(1), "a", t0), UdpAdmit::Ok);
        assert_eq!(t.admit(peer(2), "b", t0), UdpAdmit::Ok);
        assert_eq!(t.admit(peer(3), "c", t0), UdpAdmit::AtCapacity);
        assert_eq!(t.len(), 2, "被拒的那条不许留在表里");
        // 老的两条照常可用 —— 这一半才是「拒绝而不是踢掉」的判据。
        assert_eq!(t.touch(&peer(1), t0), Some(&"a"));
        assert_eq!(t.touch(&peer(2), t0), Some(&"b"));
        assert_eq!(t.touch(&peer(3), t0), None);
    }

    /// 回收之后腾出来的位置要能再被用上（否则上限会变成「一辈子只能接 N 条」）。
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
