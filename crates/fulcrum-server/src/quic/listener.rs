//! QUIC 监听器（**M2 批 J 第四步**）—— 自建 `Service` + **参与 socket 移交**。
//!
//! # 这一页分成三层，而分层本身是判据的前提
//!
//! | 层 | 内容 | 测得动吗 |
//! |---|---|---|
//! | **判定** | [`decide`]：一个数据报进来该做什么 —— **纯函数，不做任何 I/O** | ✅ 单测 |
//! | **配置** | [`build_quic_config`]：G104 的落点，把 `SniResolver` 挂进 quiche | ✅ 单测 |
//! | **I/O** | [`QuicListenerService`]：收包、按判定分发、per-连接任务 | ✅ 端到端 |
//!
//! ★ ★ **把判定从 I/O 里拆出来，与 `fulcrum-runtime`「纯逻辑不引用 pingora」是同一条理由**：
//! 它一旦长在收包循环里，就只能靠真流量去测，而那样红了指不到具体哪条规则。
//!
//! # 这里做了什么
//!
//! - fd 移交、Retry、版本协商、归属分发、**QUIC 传输层握手真的能走完**；
//! - **h3 事件循环** —— 见 [`super::h3_conn`]，请求经 [`H3RequestHandler`] 交出去；
//! - **按 G110 接线**：有 `tls` 的端口自动开，接线在 `crate::run` 里，
//!   端到端判据是 `tests/h3/run.sh`；
//! - **[`Action::Relay`] 把数据报送给它那一代**（[`super::relay`]）——
//!   而**老一代收到停机信号之后继续收转交，直到自己的连接排空**。
//!
//! # ★ 一条会影响 G109 的硬约束：**本批不签发额外的连接 ID**
//!
//! 连接表按**我们在 accept 时铸的那一个 SCID** 索引。这成立的前提是我们**从不**调用
//! `Connection::new_source_cid()` —— 一旦签发额外 CID，客户端可能换用它，
//! 而那个 CID 不在表里、也**不一定带 `gen_id` 前缀**。
//! ⇒ ★ ★ **将来要签发额外 CID 时，它必须同样出自 [`GenId::mint_scid`]**，
//! 否则换代时那条连接会认不出来 —— 这与「SCID 前缀必须在批 J 就做」是同一条理由。

use crate::quic::gen_id::{GenId, Ownership, SCID_LEN, ownership};
use crate::quic::h3_conn::{self, H3RequestHandler};
use crate::quic::relay::{RelayInbox, RelayOutbox, Relayed};
use crate::quic::retry::{self, RetryKey};
use async_trait::async_trait;
use boring::ssl::{SslContextBuilder, SslMethod};
use fulcrum_tls::resolver::SniResolver;
use log::{debug, error, info, warn};
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// 一个 UDP 数据报的上限。
///
/// ⚠ 取 1500 是不够的：QUIC 允许对端用更大的 MTU，而**读短了会把包截断**，
/// 截断之后 `Header::from_slice` 仍可能成功 ⇒ 得到一个「看起来正常」的坏包。
/// ⇒ 取 65535（UDP 载荷上限），宁可多占一点栈外内存。
const DATAGRAM_MAX: usize = 65535;

/// 每条连接的入站队列深度。★ 满了就丢 —— QUIC 自己会重传，而无界队列会被用来打内存。
const CONN_QUEUE: usize = 128;

/// 一个数据报被丢弃的原因。
///
/// ⚠ ⚠ ★ **只给日志与判据用，绝不影响回给对端的东西** —— 与 [`retry::Reject`] 同一条纪律。
/// ★ 它存在的理由也一样：断言「被丢了」**太宽**，一个把所有输入都丢掉的实现照样全绿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropWhy {
    /// 包头解析不出来。
    Unparseable,
    /// 判不出归属（DCID 太短、或服务端本就不该收到这种包）。
    Undecidable,
    /// Retry token 没通过。★ 带上原因**只为日志**。
    BadToken(retry::Reject),
    /// 前缀是本代、但表里没有这条连接（老连接的残包、或已经关掉了）。
    NoSuchConnection,
}

/// 收到一个数据报之后该做什么。**纯判定的产物，不含任何 I/O。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 客户端要的 QUIC 版本我们不支持 ⇒ 回一个版本协商包。
    NegotiateVersion,
    /// 首发 Initial ⇒ 回一个 Retry（地址验证）。**G109 要求必须开。**
    Retry,
    /// 交给本代已有的那条连接。
    Existing,
    /// 建一条新连接。`odcid` 是从 token 里取回来的**客户端第一发 Initial 的 DCID**
    /// （RFC 9000 §7.3 要求它进 `original_destination_connection_id`）。
    Accept { odcid: Vec<u8> },
    /// 属于另一代 ⇒ 转交：走 [`super::relay`] 那条 unix datagram 通道（**M2 批 K**）。
    Relay(GenId),
    /// 丢弃，并说清为什么。
    Drop(DropWhy),
}

/// 一个数据报该怎么处置 —— **纯函数**。
///
/// `known` 回答「本代的连接表里有没有这个 DCID」。
///
/// # 判定顺序，以及为什么是这个顺序
///
/// 1. **版本**只对 `Initial` 查。⚠ 短包没有版本字段（`hdr.version` 是 0），
///    对它查版本会把每一个正常的短包都判成「版本不支持」——
///    ★ 而那个错误**看起来完全像是对端的问题**。
/// 2. 然后才是归属（[`ownership`]）。
/// 3. `Local` 且表里有 ⇒ 交给它；表里没有 ⇒ 只有「带 token 的 Initial」才可能是
///    Retry 之后那一发，验票通过才 accept；其余一律丢。
pub fn decide(
    me: &GenId,
    key: &RetryKey,
    peer: &SocketAddr,
    now_secs: u64,
    hdr: &quiche::Header<'_>,
    known: &dyn Fn(&[u8]) -> bool,
) -> Action {
    // ① 版本 —— 只对 Initial。
    if hdr.ty == quiche::Type::Initial && !quiche::version_is_supported(hdr.version) {
        return Action::NegotiateVersion;
    }

    let token = hdr.token.as_deref().unwrap_or(&[]);
    let has_token = !token.is_empty();

    // ② 归属。
    match ownership(me, hdr.ty, &hdr.dcid, has_token) {
        Ownership::Relay(g) => Action::Relay(g),
        Ownership::Undecidable => Action::Drop(DropWhy::Undecidable),
        // 首发 Initial：客户端自选的 DCID，它还没有状态 ⇒ 本代回 Retry。
        Ownership::NewConnection => Action::Retry,
        Ownership::Local => {
            if known(&hdr.dcid) {
                return Action::Existing;
            }
            // 表里没有：只有 Retry 之后那一发 Initial 才该在这里出现。
            if hdr.ty == quiche::Type::Initial && has_token {
                match key.validate(peer, token, now_secs) {
                    Ok(odcid) => Action::Accept { odcid },
                    Err(why) => Action::Drop(DropWhy::BadToken(why)),
                }
            } else {
                Action::Drop(DropWhy::NoSuchConnection)
            }
        }
    }
}

/// 建一份 QUIC 配置 —— ★ ★ ★ **G104 的机械落点就在这里**。
///
/// h1/h2 入口与 h3 入口挂的是**同一个** [`SniResolver`]，
/// 于是「两个入口各有一套挑证书实现」在结构上做不到
/// （`crates/fulcrum-server/src/lib.rs` 里那行 `plan.resolver.install_into(&mut settings)`
/// 与这里这行 `resolver.install_into(&mut builder)` 收的是**同一个类型**）。
///
/// ⚠ **`SslContextBuilder::new(SslMethod::tls())` 是照着 quiche 自己 `Context::new()` 来的**
/// （它内部就是 `SSL_CTX_new(TLS_method())`）。⚠ 它**不加载 CA 证书** ——
/// quiche 的 `from_boring()` 也不加载，而服务端本就不验客户端证书。
pub fn build_quic_config(resolver: &Arc<SniResolver>) -> Result<quiche::Config, String> {
    let mut builder =
        SslContextBuilder::new(SslMethod::tls()).map_err(|e| format!("建不出 SSL context：{e}"))?;
    resolver.install_into(&mut builder);

    let mut cfg = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)
        .map_err(|e| format!("建不出 quiche 配置：{e}"))?;

    // ALPN：h3 只有这一个。★ 与 h1/h2 入口那份**有意分开** —— 那边要
    //   `h2` / `http/1.1` / `acme-tls/1`，而它们在 QUIC 上一个都不成立。
    //   ⚠ `APPLICATION_PROTOCOL` 本身就是 `&[&[u8]]`（＝那张清单），**不要再包一层**。
    cfg.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
        .map_err(|e| format!("设不上 ALPN：{e}"))?;

    // 传输参数。★ 这几个数与 h1/h2 那侧的 keep-alive 不是一回事，别照抄。
    cfg.set_max_idle_timeout(30_000);
    cfg.set_max_recv_udp_payload_size(1350);
    cfg.set_max_send_udp_payload_size(1350);
    cfg.set_initial_max_data(10_000_000);
    cfg.set_initial_max_stream_data_bidi_local(1_000_000);
    cfg.set_initial_max_stream_data_bidi_remote(1_000_000);
    cfg.set_initial_max_stream_data_uni(1_000_000);
    cfg.set_initial_max_streams_bidi(100);
    // ⚠ h3 要三条单向流（控制流 + QPACK 编/解码器），**给少了握不完手**。
    cfg.set_initial_max_streams_uni(100);
    // ⚠ ⚠ **不禁用主动迁移**：客户端换网（Wi-Fi → 蜂窝）时 QUIC 的整个卖点就在这儿。
    cfg.set_disable_active_migration(false);

    Ok(cfg)
}

/// 取 fd（继承）或新建 UDP 监听 socket，并把新建的放回表里供下一代继承。
///
/// ⚠ ⚠ **这段与 `l4.rs::build_udp_listener` 逐字同构，而两份都留着是有意的** ——
/// 那里的注释每一条都在描述一个**已经发生过**的失效（`ManuallyDrop`、`from_std` 失败后的
/// 残余风险、拿不到 fd 表时不许装作没事）。抽成泛型只会把那几段注释挤走。
/// ★ 唯一的实质差别是 `key` 的前缀：`fulcrum-quic:` 与 `fulcrum-l4-udp:` **必须分开**，
/// 否则同一个端口上的两种服务会互相抢对方的 fd。
async fn build_quic_listener(
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
        warn!("[quic] 没有 fd 表，只能在 {bind} 上新 bind —— ★ 它不会参与下一次 socket 移交");
        return UdpSocket::bind(bind).await;
    };

    let mut table = table.lock().await;

    if let Some(&fd) = table.get(key) {
        info!("[quic] 继承了 UDP 监听 fd={fd}（{key}）—— 升级窗口内这个端口没有重新 bind 过");
        // SAFETY: fd 由上一代经 SCM_RIGHTS 传来，此处接管其所有权，且同一个键只取用一次。
        // ★ ManuallyDrop 的理由与 l4.rs 那两份完全相同：提前析构会 `close(fd)`，
        //   而表里那条记录仍指着这个号码 —— 号码随后会被任意 `open()` 复用，
        //   下一次升级就把一个无关的 fd 当成监听 socket 发给下一代。`Fds` 没有 `remove()`。
        let std_sock = ManuallyDrop::new(unsafe { std::net::UdpSocket::from_raw_fd(fd) });
        std_sock.set_nonblocking(true)?;
        let owned = ManuallyDrop::into_inner(std_sock);
        return match UdpSocket::from_std(owned) {
            Ok(s) => Ok(s),
            Err(e) => {
                error!(
                    "[quic] UDP from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    let sock = UdpSocket::bind(bind).await?;
    let fd = sock.as_raw_fd();
    table.add(key.to_string(), fd);
    info!("[quic] 监听 {bind}（QUIC/UDP），fd={fd} 已登记为 {key}，下一代继承得到");
    Ok(sock)
}

/// 表里的一条连接。
struct LiveConn {
    /// 入站数据报送给连接任务。
    ///
    /// ★ ★ **每一个数据报都带上自己的 `(from, to)`**（**M2 批 K**）——
    /// 而不是让连接任务用建连时那一对。⚠ 两个理由：
    /// ① 转交进来的数据报是**别的进程**收到的，它的 `from` 只能由报文自己带；
    /// ② 顺带把「客户端换了地址」这件事做对了 —— 此前 `RecvInfo` 恒用建连时那一对，
    ///    一次连接迁移会被算成原来那个对端，而**那是一处此前没人看见的旧缺口**。
    tx: mpsc::Sender<(Vec<u8>, SocketAddr, SocketAddr)>,
    task: tokio::task::AbortHandle,
}

/// QUIC/h3 的监听服务。
pub struct QuicListenerService {
    /// 这个 service 用几个线程（**G35 / G140**）。`None` = 跟全局 `conf.threads`。
    ///
    /// ★ 与 pingora `ListeningService.threads` 同名同义 —— 自建 service 也参与
    /// 同一套角色分配，⛔ 不许在这里另立一套。由 `serve()` 在建好之后设。
    pub threads: Option<usize>,
    bind: String,
    fd_key: String,
    name: String,
    upgrading: bool,
    /// 本代的身份 —— SCID 的前缀与转交路径都由它推导（G109 ①）。
    /// ⚠ 字段名不能叫 `gen`：它在 Rust 2024 edition 里是保留字。
    gen_id: GenId,
    /// Retry 的签发密钥。★ **每代一把**，理由见 [`retry`] 那一页。
    retry_key: Arc<RetryKey>,
    resolver: Arc<SniResolver>,
    /// 一条 h3 请求交给谁去跑。
    /// ★ 监听器**不认识** `FulcrumApp` —— 那会让 HTTP/3 入口反向依赖数据面本体。
    handler: Arc<dyn H3RequestHandler>,
    /// 换代转交 socket 落在哪个目录（**M2 批 K**，G109 ②）。
    ///
    /// ★ 取的是**换代 socket 的父目录** —— 两者本来就是同一件事的两半
    /// （都只在升级窗口里有意义），⇒ 一个能换代的部署，这个目录必然可写。
    run_dir: std::path::PathBuf,
    /// 这个监听器的连接计数格（**M2 批 O**）。★ 与 HTTP 那一侧共用同一个 `ConnGuard`。
    conn: crate::conn_stats::BoundConn,
}

impl QuicListenerService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bind: String,
        upgrading: bool,
        resolver: Arc<SniResolver>,
        gen_id: GenId,
        handler: Arc<dyn H3RequestHandler>,
        run_dir: std::path::PathBuf,
        conn_reg: Arc<crate::conn_stats::ConnRegistry>,
    ) -> Self {
        // ★ `bind()` 顺手声明这一格 ⇒ 从第一秒起就有一条 `0` 的样本。
        let conn = conn_reg.bind(crate::conn_stats::Entrypoint::Quic, &bind);
        QuicListenerService {
            // ★ 缺省 `None` = 跟全局；由 `serve()` 按角色设（G140）。
            threads: None,
            fd_key: format!("fulcrum-quic:{bind}"),
            name: format!("fulcrum-quic-{bind}"),
            bind,
            upgrading,
            gen_id,
            retry_key: Arc::new(RetryKey::random()),
            resolver,
            handler,
            run_dir,
            conn,
        }
    }
}

/// 换代转交 socket 落在哪个目录：**换代 socket 的父目录**。
///
/// ⚠ ⚠ **这是一条推导，所以它有自己的判据**（见本页测试）——
/// 而不是「反正 `/run/fulcrum` 就在那儿」。
/// ★ 取父目录而不是新加一个 CLI 参数，理由是两者**必然同生共死**：
/// 转交只在升级窗口里有意义，而升级本身就要那个目录可写。
/// ⇒ 多一个参数只会多一个「两个路径配到两个地方」的失效形态。
///
/// `upgrade_sock` 没有父目录（例如只写了个文件名）时回 `.` —— ★ 那是它实际会落的地方。
pub fn run_dir_of(upgrade_sock: &str) -> std::path::PathBuf {
    std::path::Path::new(upgrade_sock)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        // ⚠ 钟在 1970 之前 ⇒ 判成 0。它会让所有票立刻过期（安全的那一侧），
        //   而不是 panic 或恒久有效。
        .unwrap_or(0)
}

#[async_trait]
impl Service for QuicListenerService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        if listeners_per_fd > 1 {
            error!(
                "[quic] listener_tasks_per_fd={listeners_per_fd} 不被 QUIC 服务支持\
                 （它只会开 1 个收包任务）。拒绝启动，以免配置与实际行为不符。"
            );
            return;
        }

        let sock = match build_quic_listener(&self.bind, &self.fd_key, fds, self.upgrading).await {
            Ok(s) => Arc::new(s),
            Err(e) => {
                error!("[quic] 在 {} 上建不出 UDP 监听器：{e}", self.bind);
                return;
            }
        };
        let config = match build_quic_config(&self.resolver) {
            Ok(c) => c,
            Err(e) => {
                error!("[quic] {e}");
                return;
            }
        };

        info!(
            "[quic] 监听 {} （HTTP/3，gen={}）",
            self.bind,
            self.gen_id.hex()
        );

        // ── 换代转交（**M2 批 K**，G109 ②）────────────────────────────────
        //
        // ⚠ ⚠ **绑不上时说一句、然后照常服务 h3** —— 而这与访问日志那条
        //   「打不开就装不上」的口径**有意不同**，理由要写清楚：
        //   拒绝启动会把一个 `/run` 的权限问题变成「整个 h3 入口不存在」，
        //   而那损失更大；★ 而这条错误在**启动日志里当场可见**，
        //   不是那种「没有任何东西会说」的失效。
        // ⚠ 代价说全：没有转交口 ⇒ **换代时在飞的 h3 连接会断**。
        let relay = match RelayInbox::bind(&self.run_dir, &self.gen_id) {
            Ok(inbox) => {
                info!("[quic] 换代转交口：{}", inbox.path().display());
                Some(Relay {
                    inbox,
                    outbox: RelayOutbox::new(&self.run_dir),
                })
            }
            Err(e) => {
                error!(
                    "[quic] 建不出换代转交口（{}/quic-relay-{}.sock）：{e} ——                      h3 照常服务，但**换代时在飞的连接会断**",
                    self.run_dir.display(),
                    self.gen_id.hex()
                );
                None
            }
        };

        serve(
            sock,
            shutdown,
            self.gen_id,
            self.retry_key.clone(),
            config,
            self.handler.clone(),
            relay,
            Some(self.conn.clone()),
        )
        .await;
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

/// 一代进程的转交两半（**M2 批 K**，G109 ②）。
///
/// ★ **收件口与发件口捆在一起交给 [`serve`]**，而不是各传各的：
/// 一代要么两半都有，要么一半都没有 —— 拆成两个 `Option` 会多出两个说不出意义的状态。
pub struct Relay {
    pub inbox: RelayInbox,
    pub outbox: RelayOutbox,
}

/// 排空阶段的轮询间隔。
///
/// ⚠ 收到停机信号之后**不再 `recv_from`**，于是那条 `select!` 只剩转交一支 ——
/// 而如果没有任何转交进来，它会一直等下去、永远不回头看「连接排空了没有」。
/// ★ 这个 tick 就是那一眼。
const DRAIN_POLL: Duration = Duration::from_millis(200);

/// 从转交口收进来的一份数据报，交给本代的连接表。
///
/// # ★ ★ ★ G109 ④「转交只走一跳」的**结构性**落点
///
/// ⚠ ⚠ **本函数拿不到 `GenId`、`RetryKey`，也拿不到 [`RelayOutbox`]** ——
/// 于是「再判一次归属」「再转交一次」这两件事在这里**写不出来**。
/// ★ 那比一句「记得别再转交」可靠：环路不是靠自觉避免的，是靠**手上没有那个东西**。
///
/// 返回 `true` = 送进了某条连接。`false` = 不认识这个 DCID（丢弃）。
fn dispatch_relayed(
    conns: &HashMap<Vec<u8>, LiveConn>,
    from: SocketAddr,
    to: SocketAddr,
    pkt: &mut [u8],
) -> bool {
    // ⚠ 只为读 DCID 而解析包头 —— 解析不出来就丢，**不猜**。
    let Ok(hdr) = quiche::Header::from_slice(pkt, SCID_LEN) else {
        return false;
    };
    match conns.get(hdr.dcid.as_ref()) {
        // ⚠ `try_send` 而不是 `send`：与本代自己收包那条逐字同一条理由 ——
        //   队列满了就丢这一个数据报，绝不能把收包循环卡住。
        Some(c) => c.tx.try_send((pkt.to_vec(), from, to)).is_ok(),
        None => false,
    }
}

/// 从转交口收一份报文。
///
/// * `None` 收件口不存在或已被摘掉 ⇒ **永远不完成**（那一支在 `select!` 里等于不存在）。
/// * `Some(frame)` 收到一份。
/// * 读出错 ⇒ `None` ⇒ 调用方摘掉它。
///
/// ⚠ 写成一个 helper 而不是把 `if inbox.is_some()` 塞进 `select!` 的前置条件：
/// 那样写在 `inbox` 为 `None` 时会**没有任何分支可选**，而 `tokio::select!` 在那种情况下
/// 会 panic。★ 这里让它「永远 pending」，于是那一支只是安静地不参与。
async fn recv_relay(inbox: Option<&RelayInbox>, broken: bool) -> Option<Vec<u8>> {
    let Some(inbox) = inbox.filter(|_| !broken) else {
        return std::future::pending().await;
    };
    let mut buf = vec![0u8; DATAGRAM_MAX + crate::quic::relay::HEADER_LEN];
    match inbox.recv(&mut buf).await {
        Ok(n) => {
            buf.truncate(n);
            Some(buf)
        }
        Err(_) => None,
    }
}

/// 收包循环本体 —— **socket 与配置都由调用方给**。
///
/// ★ ★ 把「取 socket」与「在它上面服务」拆开，与本页开头那张表是同一条理由：
/// `start_service` 里那一段要 fd 表、要 `SniResolver`、要一整个 `Service` 生命周期，
/// 而**收包循环本身只需要一个 socket** —— 焊在一起的话，
/// 判据就只能从 `Service` 那一头进去，那意味着**没法给它一个自己挑的端口**。
/// ⇒ 拆开之后判据可以 bind `127.0.0.1:0`、拿到真端口，再拿真客户端打过来。
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    sock: Arc<UdpSocket>,
    mut shutdown: ShutdownWatch,
    gen_id: GenId,
    retry_key: Arc<RetryKey>,
    mut config: quiche::Config,
    handler: Arc<dyn H3RequestHandler>,
    relay: Option<Relay>,
    // 这个监听器的连接计数格（**M2 批 O**）。`None` = 不数（单测走这条）。
    // ⚠ 名字不叫 `conn`：这个函数里 `conn` 已经是**一条 quiche 连接**了。
    conn_stats_slot: Option<crate::conn_stats::BoundConn>,
) {
    let local: SocketAddr = match sock.local_addr() {
        Ok(a) => a,
        Err(e) => {
            error!("[quic] 拿不到本地地址：{e}");
            return;
        }
    };

    let mut conns: HashMap<Vec<u8>, LiveConn> = HashMap::new();
    let mut buf = vec![0u8; DATAGRAM_MAX];
    let mut out = vec![0u8; DATAGRAM_MAX];
    let (inbox, mut outbox) = match relay {
        Some(r) => (Some(r.inbox), Some(r.outbox)),
        None => (None, None),
    };
    // ⚠ ⚠ **收到停机信号之后不是 `break`，是转进「排空」**（**M2 批 K**）。
    //   ★ 那一段里本代**不再 `recv_from`**，但**继续收转交** ——
    //     否则新一代转回来的数据报没人接，而那正是「换代零中断」要防的事。
    let mut draining = false;
    // ⚠ 转交口自己读不下去了（fd 坏了那一类）⇒ 摘掉它，**而不是把整个监听器停掉**：
    //   ★ 那样至少 h3 还在服务，只是换代时会断 —— 比「h3 整个不存在」小得多。
    let mut relay_broken = false;

    loop {
        tokio::select! {
            _ = shutdown.changed(), if !draining => {
                // ★ ★ ★ **与 L4 UDP 那条纪律逐字相同**：停机信号一到就**不再 `recv_from`**。
                //   ⚠ 继续收的话，老一代会与新一代抢同一个 socket 上的数据报
                //   （两代持有的是同一个 fd），把本该属于新一代的首包吃掉。
                //   ⇒ 已建立的连接任务**不动**，它们只 `send_to`，不抢 `recv`。
                info!(
                    "[quic] {} 收到停机信号，停止收包（{} 条连接的任务继续；\
                     转交口继续收，直到它们排空）",
                    local,
                    conns.len()
                );
                draining = true;
            }
            // ★ 排空阶段的那一眼：没有它，只剩转交一支的 `select!` 会一直等下去，
            //   永远不回头看「连接排空了没有」。
            _ = tokio::time::sleep(DRAIN_POLL), if draining => {}
            // ★ ★ ★ **转交进来的那一支。** 它**不经过 `decide`** —— G109 ④ 的「只走一跳」。
            //
            // ⚠ ⚠ 这里收成一个**自有的 `Vec`** 而不是借外面那份缓冲，理由是
            //   `tokio::select!` 的规矩：分支的 future 会一直借着它捕获的东西，
            //   而处理体要用同一份缓冲 —— 借用检查器不让。
            //   ★ 代价是每一份**转交进来的**数据报一次分配，而它只在换代窗口里出现。
            got = recv_relay(inbox.as_ref(), relay_broken) => {
                match got {
                    Some(mut frame) => {
                        let n = frame.len();
                        let ok = match crate::quic::relay::decode(&frame) {
                            Some((from, to, _)) => {
                                // ⚠ 拆完头之后把载荷**原地**交出去
                                //   （`quiche::Header::from_slice` 要 `&mut [u8]`）。
                                let body = &mut frame[crate::quic::relay::HEADER_LEN..];
                                dispatch_relayed(&conns, from, to, body)
                            }
                            None => {
                                debug!("[quic] 转交口收到一份拆不开的报文（{n} 字节），丢弃");
                                false
                            }
                        };
                        // ⚠ **成功那一半也要说一句** —— 只在失败时打日志的话，
                        //   **「没有日志」既可能是「没收到」、也可能是「收到并且一切正常」**。
                        if ok {
                            debug!("[quic] 转交进来一个数据报（{n} 字节）⇒ 已送进那条连接");
                        } else {
                            debug!("[quic] 转交进来的数据报没人认领，丢弃（{n} 字节）");
                        }
                    }
                    None => {
                        error!(
                            "[quic] {local} 的转交收件口读不下去了，摘掉它 —— \
                             h3 照常服务，但**换代时在飞的连接会断**"
                        );
                        relay_broken = true;
                    }
                }
            }
            r = sock.recv_from(&mut buf), if !draining => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(e) => {
                        error!("[quic] {local} recv_from 出错：{e}");
                        // fd 耗尽时这个循环会满速空转 —— 与 l4.rs 同一条防护。
                        if matches!(e.raw_os_error(), Some(24) | Some(23)) {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                        continue;
                    }
                };
                let pkt = &mut buf[..n];

                let hdr = match quiche::Header::from_slice(pkt, SCID_LEN) {
                    Ok(h) => h,
                    Err(e) => {
                        debug!("[quic] {peer} 的包头解析不出来（{e}），丢弃");
                        continue;
                    }
                };

                let action = {
                    let known = |dcid: &[u8]| conns.contains_key(dcid);
                    decide(
                        &gen_id,
                        &retry_key,
                        &peer,
                        now_secs(),
                        &hdr,
                        &known,
                    )
                };

                match action {
                    Action::NegotiateVersion => {
                        match quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut out) {
                            Ok(len) => {
                                let _ = sock.send_to(&out[..len], peer).await;
                                debug!("[quic] 给 {peer} 回了版本协商（它要 {:x}）", hdr.version);
                            }
                            Err(e) => debug!("[quic] 造不出版本协商包：{e}"),
                        }
                    }
                    Action::Retry => {
                        // ★ 这里铸的 SCID 就是客户端此后要用的 DCID ——
                        //   它**必须带 gen_id 前缀**，否则换代时那条连接认不出来（G109 ①）。
                        let new_scid = gen_id.mint_scid();
                        let token = match retry_key.mint(&peer, &hdr.dcid, now_secs()) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!("[quic] 签不出 Retry token：{e}");
                                continue;
                            }
                        };
                        match quiche::retry(
                            &hdr.scid, &hdr.dcid, &new_scid, &token, hdr.version, &mut out,
                        ) {
                            Ok(len) => {
                                let _ = sock.send_to(&out[..len], peer).await;
                                debug!("[quic] 给 {peer} 回了 Retry");
                            }
                            Err(e) => debug!("[quic] 造不出 Retry 包：{e}"),
                        }
                    }
                    Action::Existing => {
                        let dcid = hdr.dcid.to_vec();
                        let dead = match conns.get(&dcid) {
                            // ⚠ `try_send` 而不是 `send`：队列满了就丢这一个数据报，
                            //   **绝不能把收包循环卡住** —— 一条慢连接会拖垮整个监听器。
                            //   ★ QUIC 自己会重传，丢一个是可恢复的。
                            Some(c) => c.tx.try_send((pkt.to_vec(), peer, local)).is_err(),
                            None => false,
                        };
                        if dead {
                            debug!("[quic] {peer} 那条连接的队列满了或任务没了，丢一个数据报");
                        }
                    }
                    Action::Accept { odcid } => {
                        // 客户端现在用的 DCID 就是我们在 Retry 里给它的 SCID。
                        let scid = quiche::ConnectionId::from_ref(&hdr.dcid).into_owned();
                        let odcid = quiche::ConnectionId::from_ref(&odcid).into_owned();
                        let conn = match quiche::accept(
                            &scid, Some(&odcid), local, peer, &mut config,
                        ) {
                            Ok(c) => c,
                            Err(e) => {
                                warn!("[quic] accept 失败（{peer}）：{e}");
                                continue;
                            }
                        };
                        let (tx, rx) = mpsc::channel(CONN_QUEUE);
                        // ★ 连接计数（**M2 批 O**）：包一层而不改 `h3_conn::run` 的签名。
                        let g = conn_stats_slot.as_ref().map(|b| b.guard());
                        let sock_c = sock.clone();
                        let handler_c = handler.clone();
                        let handle = tokio::spawn(async move {
                            // ⚠ ⚠ **必须绑名字**：写成 `let _ = g;` 会当场 drop，
                            //   于是 active 恒为 0 而 total 照涨，且不会有东西红。
                            let _g = g;
                            h3_conn::run(conn, sock_c, rx, local, peer, handler_c).await
                        });
                        let key = scid.to_vec();
                        // 首包要立刻喂给它，否则握手不会开始。
                        let _ = tx.try_send((pkt.to_vec(), peer, local));
                        conns.insert(
                            key,
                            LiveConn { tx, task: handle.abort_handle() },
                        );
                        debug!("[quic] 接受了来自 {peer} 的新连接（现共 {} 条）", conns.len());
                    }
                    Action::Relay(g) => {
                        // ✅ ✅ ✅ **M2 批 K：这一支从「丢弃」换成「送给它真正的主人」。**
                        //   ⚠ 批 J 时这里只有一行 debug，而那一行是「换代零中断不成立」
                        //     这句话唯一的落点。★ 现在它成立了 —— 改它之前先读 G109 ②③④。
                        match outbox.as_mut() {
                            Some(ob) => match ob.send(g, &peer, &local, pkt).await {
                                Relayed::Sent => {
                                    debug!("[quic] {peer} 的包转交给了 {}", g.hex());
                                }
                                // ★ 「那一代已经退出」是换代的**常态**：它一走，
                                //   那条连接本来就没了 ⇒ 丢一个包，不是错误。
                                Relayed::Gone => {
                                    debug!(
                                        "[quic] {peer} 的包属于 {}，而那一代已经退出，丢弃",
                                        g.hex()
                                    );
                                }
                                // ⚠ 而这一种要有人看：它不是换代的常态。
                                Relayed::Failed => {
                                    warn!("[quic] 往 {} 转交失败（不是「它已退出」那一种）", g.hex());
                                }
                            },
                            None => debug!(
                                "[quic] {peer} 的包属于另一代（{}），而本代没有转交口，丢弃",
                                g.hex()
                            ),
                        }
                    }
                    Action::Drop(why) => {
                        debug!("[quic] 丢弃来自 {peer} 的数据报：{why:?}");
                    }
                }
            }
            // ⚠ 所有分支都被前置条件关掉时 `tokio::select!` 会 panic ——
            //   这一支是那种情况的出口（没有转交口 + 已进入排空）。
            else => break,
        }

        // 顺手收掉已经结束的连接任务。★ 不做的话这张表只增不减。
        // ⚠ ⚠ **移到 `select!` 外面**（批 K）：排空阶段要靠它才看得见「排完了没有」，
        //   而它此前只在「收到一个数据报」那一支里跑。
        conns.retain(|_, c| !c.task.is_finished());

        // ★ 排空完成 —— 本代该走了。
        //   ⚠ 上界不由这里定：pingora 停机时**不等 service 返回**，
        //     它 sleep 完宽限期就强关 runtime（`server/mod.rs` 那一段）。
        //     ⇒ 这里只负责「排完就早点走」，走不掉也不会拖住谁。
        if draining && conns.is_empty() {
            info!("[quic] {local} 的连接已排空，收包任务退出");
            break;
        }
    }
}

/// 判定层的判据。**全部脱网**：一个 socket 都不开，而客户端那一半是**真的 quiche**。
///
/// ⚠ ⚠ ★ **首包不手工拼**：让 **quiche 自己**去造 Initial、去消化我们的 Retry
/// （`l4.rs::peek_tests` 让 BoringSSL 自己造 ClientHello，同一条纪律）。
/// ★ 一份手工拼的报文只能证明「我对格式的理解与我的实现一致」，那是同义反复。
#[cfg(test)]
mod tests {
    use super::*;
    use fulcrum_tls::cert_key_from_der;
    use pingora_core::protocols::http::ServerSession;
    // ★ `h3::Header` 的 `name()` / `value()` 挂在这个 trait 上。
    use quiche::h3::NameValue;

    const SNI: &str = "h3.test";
    const NOW: u64 = 1_800_000_000;

    fn s_addr() -> SocketAddr {
        "192.0.2.1:443".parse().expect("常量地址")
    }
    fn c_addr() -> SocketAddr {
        "192.0.2.9:44300".parse().expect("常量地址")
    }

    /// 一份自签证书 + 挂上它的 resolver + 服务端 quiche 配置。
    fn server_config() -> quiche::Config {
        let key = rcgen::KeyPair::generate().expect("测试密钥");
        let params = rcgen::CertificateParams::new(vec![SNI.to_string()]).expect("测试参数");
        let cert = params.self_signed(&key).expect("自签");
        let ck = cert_key_from_der(cert.der().to_vec(), key.serialize_der()).expect("造 CertKey");

        let resolver = Arc::new(SniResolver::new());
        // ★ 按名字装，而不是塞进 default 槽：下面的客户端**报了 SNI**（`quiche::connect`
        //   的第一个参数），所以这里走的是与真流量同一条挑证书的路。
        //   ⚠ 之前这里用的是 `set_default` —— 而那是全仓**唯一**的调用方，
        //   于是「default 槽有人填」这件事只在测试里成立过。
        resolver.install(&[SNI.to_string()], ck);
        build_quic_config(&resolver).expect("服务端配置")
    }

    fn client_config() -> quiche::Config {
        let mut c = quiche::Config::new(quiche::PROTOCOL_VERSION).expect("客户端配置");
        // 自签证书，测试里不验链 —— ★ 被测的是**我们的**分发与握手，不是 PKI。
        c.verify_peer(false);
        c.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
            .expect("客户端 ALPN");
        c.set_max_idle_timeout(5_000);
        c.set_max_recv_udp_payload_size(1350);
        c.set_max_send_udp_payload_size(1350);
        c.set_initial_max_data(10_000_000);
        c.set_initial_max_stream_data_bidi_local(1_000_000);
        c.set_initial_max_stream_data_bidi_remote(1_000_000);
        c.set_initial_max_stream_data_uni(1_000_000);
        c.set_initial_max_streams_bidi(100);
        c.set_initial_max_streams_uni(100);
        c
    }

    /// 造一个真的客户端，并拿到它的第一个数据报。
    ///
    /// 返回 `(连接, 首包, 它自选的 DCID)` —— 那个 DCID 就是 RFC 9000 §7.3 的 `odcid`。
    fn client_first_flight(ccfg: &mut quiche::Config) -> (quiche::Connection, Vec<u8>, Vec<u8>) {
        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut conn =
            quiche::connect(Some(SNI), &scid, c_addr(), s_addr(), ccfg).expect("造客户端");
        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n, _) = conn.send(&mut buf).expect("客户端首包");
        let pkt = buf[..n].to_vec();
        let odcid = {
            let mut probe = pkt.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析首包");
            h.dcid.to_vec()
        };
        (conn, pkt, odcid)
    }

    fn judge(me: &GenId, key: &RetryKey, pkt: &[u8], known: &dyn Fn(&[u8]) -> bool) -> Action {
        let mut buf = pkt.to_vec();
        let hdr = quiche::Header::from_slice(&mut buf, SCID_LEN).expect("解析包头");
        decide(me, key, &c_addr(), NOW, &hdr, known)
    }

    fn none_known(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn 真客户端的首包判成_retry() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let mut ccfg = client_config();
        let (_c, pkt, _) = client_first_flight(&mut ccfg);
        assert_eq!(judge(&me, &key, &pkt, &none_known), Action::Retry);
    }

    /// ★★★ 这一条走完 **Retry 那一整轮**，并且钉住 `odcid` 被原样带了回来 ——
    /// RFC 9000 §7.3 要求它进 `original_destination_connection_id`，
    /// 少带或带错的话客户端会在握手末尾判传输参数不符而断开，
    /// ⚠ **而那个失败看起来像「TLS 出了问题」**。
    #[test]
    fn retry_之后那一发判成_accept_并且_odcid_原样带回() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let mut ccfg = client_config();
        let (mut c, pkt1, odcid) = client_first_flight(&mut ccfg);
        assert_eq!(judge(&me, &key, &pkt1, &none_known), Action::Retry);

        // 我们回一个真的 Retry。
        let new_scid = me.mint_scid();
        let token = key.mint(&c_addr(), &odcid, NOW).expect("签票");
        let mut out = vec![0u8; DATAGRAM_MAX];
        let (scid1, ver) = {
            let mut probe = pkt1.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析");
            (h.scid.to_vec(), h.version)
        };
        let n = quiche::retry(
            &quiche::ConnectionId::from_ref(&scid1),
            &quiche::ConnectionId::from_ref(&odcid),
            &new_scid,
            &token,
            ver,
            &mut out,
        )
        .expect("造 Retry");

        // 交给真客户端消化 —— ★ 它认不认，是它说了算，不是我们说了算。
        c.recv(
            &mut out[..n],
            quiche::RecvInfo {
                from: s_addr(),
                to: c_addr(),
            },
        )
        .expect("客户端收 Retry");

        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n2, _) = c.send(&mut buf).expect("客户端第二发");
        match judge(&me, &key, &buf[..n2], &none_known) {
            Action::Accept { odcid: got } => assert_eq!(got, odcid, "odcid 没被原样带回来"),
            other => panic!("Retry 之后那一发应当判成 Accept，实际是 {other:?}"),
        }
    }

    /// ★★★ **端到端：一条真的 QUIC 连接握完手。**
    ///
    /// 它证的不是某一条判定，而是**整条链条合得上**：判定 → Retry → 判定 → accept →
    /// 握手。⚠ 每一步都可以单独是绿的而链条断掉（`odcid` 带错、ALPN 不匹配、
    /// 单向流给少了）—— 那几种失败**全都表现为「握手就是不完成」**，而单测看不见。
    #[test]
    fn 走完_retry_之后真的能握完手() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let mut ccfg = client_config();
        let mut scfg = server_config();

        let (mut c, pkt1, odcid) = client_first_flight(&mut ccfg);
        assert_eq!(judge(&me, &key, &pkt1, &none_known), Action::Retry);

        let new_scid = me.mint_scid();
        let token = key.mint(&c_addr(), &odcid, NOW).expect("签票");
        let mut out = vec![0u8; DATAGRAM_MAX];
        let (scid1, ver) = {
            let mut probe = pkt1.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析");
            (h.scid.to_vec(), h.version)
        };
        let n = quiche::retry(
            &quiche::ConnectionId::from_ref(&scid1),
            &quiche::ConnectionId::from_ref(&odcid),
            &new_scid,
            &token,
            ver,
            &mut out,
        )
        .expect("造 Retry");
        c.recv(
            &mut out[..n],
            quiche::RecvInfo {
                from: s_addr(),
                to: c_addr(),
            },
        )
        .expect("客户端收 Retry");

        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n2, _) = c.send(&mut buf).expect("客户端第二发");
        let got_odcid = match judge(&me, &key, &buf[..n2], &none_known) {
            Action::Accept { odcid } => odcid,
            other => panic!("应当 Accept，实际 {other:?}"),
        };

        // 按判定的产物 accept —— ★ 这几行与 `start_service` 里那几行是同一套。
        let mut s = quiche::accept(
            &new_scid,
            Some(&quiche::ConnectionId::from_ref(&got_odcid)),
            s_addr(),
            c_addr(),
            &mut scfg,
        )
        .expect("accept");
        s.recv(
            &mut buf[..n2],
            quiche::RecvInfo {
                from: c_addr(),
                to: s_addr(),
            },
        )
        .expect("服务端收第二发");

        // 手工把两边的数据报倒来倒去，直到都握完手。
        for _ in 0..40 {
            if c.is_established() && s.is_established() {
                break;
            }
            pump(&mut s, &mut c, s_addr(), c_addr());
            pump(&mut c, &mut s, c_addr(), s_addr());
        }

        assert!(s.is_established(), "服务端没握完手：{:?}", s.stats());
        assert!(c.is_established(), "客户端没握完手：{:?}", c.stats());
        assert_eq!(
            s.application_proto(),
            b"h3",
            "ALPN 没协商到 h3 —— 那样 h3 层根本起不来"
        );
        // ★ 而这条连接的 CID **必须带本代的前缀**，否则换代时它认不出来（G109 ①）。
        assert!(
            me.owns(&new_scid),
            "握成的这条连接，CID 前缀不是本代 —— G109 的整条判定会落空"
        );
    }

    /// 把 `from` 排着的包全部搬给 `to`。
    fn pump(
        from: &mut quiche::Connection,
        to: &mut quiche::Connection,
        from_addr: SocketAddr,
        to_addr: SocketAddr,
    ) {
        let mut buf = vec![0u8; DATAGRAM_MAX];
        loop {
            let (n, _) = match from.send(&mut buf) {
                Ok(v) => v,
                Err(quiche::Error::Done) => return,
                Err(e) => panic!("send 出错：{e}"),
            };
            to.recv(
                &mut buf[..n],
                quiche::RecvInfo {
                    from: from_addr,
                    to: to_addr,
                },
            )
            .expect("recv");
        }
    }

    /// ★★★ **短包不许被判成「版本不支持」。**
    ///
    /// 短包没有版本字段（`hdr.version` 是 0），而 0 不在支持列表里 ——
    /// 少了 `ty == Initial` 那道闸，**每一个正常的短包都会被回一个版本协商**，
    /// ⚠ 而那个错误看起来**完全像是对端的问题**。
    #[test]
    fn 短包不许被判成版本不支持() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let cid = me.mint_scid();
        // 一个最小的短包：首字节最高位为 0（短包），后面跟 DCID。
        let mut pkt = vec![0x40u8];
        pkt.extend_from_slice(&cid);
        pkt.extend_from_slice(&[0u8; 16]);

        let action = judge(&me, &key, &pkt, &none_known);
        assert_ne!(action, Action::NegotiateVersion, "短包被判成了版本不支持");
        // 表里没有这条连接 ⇒ 该判「没有这条连接」，而不是别的。
        assert_eq!(action, Action::Drop(DropWhy::NoSuchConnection));
        // 表里有的话就该交给它。
        let known = |_: &[u8]| true;
        assert_eq!(judge(&me, &key, &pkt, &known), Action::Existing);
    }

    #[test]
    fn 别代的短包判成_relay() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let other = GenId::random();
        let cid = other.mint_scid();
        let mut pkt = vec![0x40u8];
        pkt.extend_from_slice(&cid);
        pkt.extend_from_slice(&[0u8; 16]);
        assert_eq!(judge(&me, &key, &pkt, &none_known), Action::Relay(other));
    }

    /// 票是别人签的 / 过期了 / 被改过 ⇒ 一律 `BadToken`，而**不是** `Accept`。
    #[test]
    fn 坏票判成_badtoken_而不是_accept() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let other_key = RetryKey::random();
        let mut ccfg = client_config();
        let (mut c, pkt1, odcid) = client_first_flight(&mut ccfg);

        // 用**另一把钥匙**签票，其余流程一模一样。
        let new_scid = me.mint_scid();
        let token = other_key.mint(&c_addr(), &odcid, NOW).expect("签票");
        let mut out = vec![0u8; DATAGRAM_MAX];
        let (scid1, ver) = {
            let mut probe = pkt1.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析");
            (h.scid.to_vec(), h.version)
        };
        let n = quiche::retry(
            &quiche::ConnectionId::from_ref(&scid1),
            &quiche::ConnectionId::from_ref(&odcid),
            &new_scid,
            &token,
            ver,
            &mut out,
        )
        .expect("造 Retry");
        c.recv(
            &mut out[..n],
            quiche::RecvInfo {
                from: s_addr(),
                to: c_addr(),
            },
        )
        .expect("客户端收 Retry");
        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n2, _) = c.send(&mut buf).expect("客户端第二发");

        assert_eq!(
            judge(&me, &key, &buf[..n2], &none_known),
            Action::Drop(DropWhy::BadToken(retry::Reject::BadSeal))
        );
    }

    /// 客户端要一个我们不支持的 QUIC 版本 ⇒ 回版本协商。
    ///
    /// ⚠ ★ **写成 `if let Ok(hdr) = Header::from_slice(…) { assert!(…) }` 是错的** ——
    /// 解析失败时它一条断言都不跑，而测试照样是绿的。
    /// ★ **一道门写完后要问：它自己有没有静默分支。**
    /// ⇒ 这一条用**真客户端发一个真的坏版本**，它没有可以静默的分支。
    /// （「DCID 太短」那一条由 `gen_id::tests::dcid_短于八字节判不出来而不是判成本代`
    /// 直接钉在 `ownership` 上，而 `decide` 的版本闸只对 `Initial` 生效、绕不过它。）
    #[test]
    fn 不支持的版本判成版本协商() {
        let (me, key) = (GenId::random(), RetryKey::random());
        // ★ `0xbabababa` 是 quiche 自己文档里用的那个「保留给测试」的版本号。
        let mut ccfg = quiche::Config::new(0xbabababa).expect("坏版本的客户端配置");
        ccfg.verify_peer(false);
        ccfg.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
            .expect("ALPN");

        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut c = quiche::connect(Some(SNI), &scid, c_addr(), s_addr(), &mut ccfg)
            .expect("造坏版本客户端");
        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n, _) = c.send(&mut buf).expect("首包");

        assert_eq!(
            judge(&me, &key, &buf[..n], &none_known),
            Action::NegotiateVersion
        );
        // ★ 反向的一半：**同一条链路上**版本对的客户端不能也被判成版本协商，
        //   否则这条判据与「恒返回 NegotiateVersion」无法区分。
        let mut ok_cfg = client_config();
        let (_c2, pkt_ok, _) = client_first_flight(&mut ok_cfg);
        assert_eq!(judge(&me, &key, &pkt_ok, &none_known), Action::Retry);
    }

    #[test]
    fn 配置真的把_h3_设成了_alpn() {
        // ★ 直接的判据是上面那条端到端里 `application_proto() == b"h3"`；
        //   这里只确认配置本身建得出来（证书装得上、context 建得成）。
        let _ = server_config();
    }

    // ── 端到端：真 UDP、真 quiche 客户端、真 h3 请求与响应 ──────────────────
    //
    // ★ ★ ★ 上面那些判据各自只声称自己那一格；这一条把**整条链**跑通：
    //   收包 → 判定 → Retry → accept → 握手 → h3 层 → 翻请求头 → 执行链 →
    //   翻响应头（**滤掉逐跳头**）→ 发体 → 客户端读到。
    // ⚠ 走的是 `127.0.0.1` 上一对真 socket，**端口由内核挑**（bind `:0` 再问它） ——
    //   写死端口会在并行跑测试时互相撞，而那种红看起来像产品缺陷。

    /// 判据用的处理器：回一个固定响应，并把它看见的请求头**回传**出来。
    ///
    /// ⚠ ⚠ ★ 它**故意插一个 `connection` 逐跳头** —— 客户端如果看得见它，
    /// 说明 RFC 9114 §4.2 那道过滤器在真链路上没生效
    /// （而单测里它是绿的：那只证明函数本身对，证不了它被接在链上）。
    struct EchoHandler;

    #[async_trait]
    impl H3RequestHandler for EchoHandler {
        async fn handle(&self, mut session: ServerSession) {
            let path = session.req_header().uri.path().to_string();
            let host = session
                .req_header()
                .headers
                .get("host")
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                .unwrap_or_default();
            let ua = session
                .req_header()
                .headers
                .get("user-agent")
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                .unwrap_or_default();

            let mut resp = pingora_http::ResponseHeader::build(200, None).expect("造响应头");
            resp.insert_header("content-type", "text/plain")
                .expect("插");
            resp.insert_header("x-seen-path", path).expect("插");
            resp.insert_header("x-seen-host", host).expect("插");
            resp.insert_header("x-seen-ua", ua).expect("插");
            // ★ 故意的：它必须在客户端那侧消失。
            resp.insert_header("connection", "keep-alive").expect("插");

            let _ = session.write_response_header(Box::new(resp)).await;
            let _ = session
                .write_response_body(bytes::Bytes::from_static(b"hello from h3"), true)
                .await;
            let _ = session.finish().await;
        }
    }

    /// 客户端读到的东西。
    struct Got {
        status: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    /// 一个最小的真 h3 客户端：连上去、发一个请求、把响应读回来。
    async fn one_h3_request(server: SocketAddr, path: &str) -> Got {
        let sock = UdpSocket::bind("127.0.0.1:0").await.expect("客户端 socket");
        let local = sock.local_addr().expect("客户端地址");
        let mut cfg = client_config();
        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut conn =
            quiche::connect(Some(SNI), &scid, local, server, &mut cfg).expect("客户端连接");

        let h3_cfg = quiche::h3::Config::new().expect("h3 配置");
        let mut h3: Option<quiche::h3::Connection> = None;
        let mut sent = false;
        let mut got = Got {
            status: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let mut done = false;
        let mut out = vec![0u8; DATAGRAM_MAX];
        let mut inbuf = vec![0u8; DATAGRAM_MAX];

        while !done {
            // ① 发。
            loop {
                match conn.send(&mut out) {
                    Ok((n, info)) => {
                        sock.send_to(&out[..n], info.to).await.expect("客户端发包");
                    }
                    Err(quiche::Error::Done) => break,
                    Err(e) => panic!("客户端 send 出错：{e}"),
                }
            }
            if conn.is_closed() {
                break;
            }
            // ② h3 层 + 发请求。
            if h3.is_none() && conn.is_established() {
                h3 = Some(
                    quiche::h3::Connection::with_transport(&mut conn, &h3_cfg).expect("客户端 h3"),
                );
            }
            if let Some(h) = h3.as_mut()
                && !sent
            {
                let req = [
                    quiche::h3::Header::new(b":method", b"GET"),
                    quiche::h3::Header::new(b":scheme", b"https"),
                    quiche::h3::Header::new(b":authority", SNI.as_bytes()),
                    quiche::h3::Header::new(b":path", path.as_bytes()),
                    quiche::h3::Header::new(b"user-agent", b"fulcrum-test"),
                ];
                h.send_request(&mut conn, &req, true).expect("客户端发请求");
                sent = true;
                continue; // 让它先发出去
            }
            // ③ 收 h3 事件。
            if let Some(h) = h3.as_mut() {
                loop {
                    match h.poll(&mut conn) {
                        Ok((sid, quiche::h3::Event::Headers { list, .. })) => {
                            let _ = sid;
                            for x in list {
                                let n = String::from_utf8_lossy(x.name()).into_owned();
                                let v = String::from_utf8_lossy(x.value()).into_owned();
                                if n == ":status" {
                                    got.status = v;
                                } else {
                                    got.headers.push((n, v));
                                }
                            }
                        }
                        Ok((sid, quiche::h3::Event::Data)) => {
                            let mut chunk = vec![0u8; 4096];
                            while let Ok(n) = h.recv_body(&mut conn, sid, &mut chunk) {
                                got.body.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Ok((_, quiche::h3::Event::Finished)) => done = true,
                        Ok(_) => {}
                        Err(quiche::h3::Error::Done) => break,
                        Err(e) => panic!("客户端 poll 出错：{e}"),
                    }
                }
            }
            if done {
                break;
            }
            // ④ 等一个数据报，或者等定时器。
            let t = conn.timeout().unwrap_or(Duration::from_millis(20));
            match tokio::time::timeout(t, sock.recv_from(&mut inbuf)).await {
                Ok(Ok((n, from))) => {
                    let info = quiche::RecvInfo { from, to: local };
                    let _ = conn.recv(&mut inbuf[..n], info);
                }
                Ok(Err(e)) => panic!("客户端 recv 出错：{e}"),
                Err(_) => conn.on_timeout(),
            }
        }
        got
    }

    /// ★★★ **整条链跑通：一个真的 h3 请求进来，一个真的 h3 响应回去。**
    #[tokio::test]
    async fn 一个真的_h3_请求端到端走通() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("服务端 socket"));
        let server = sock.local_addr().expect("服务端地址");
        let (_tx, shutdown) = tokio::sync::watch::channel(false);
        let cfg = server_config();
        let gen_id = GenId::random();

        let srv = tokio::spawn(serve(
            sock,
            shutdown,
            gen_id,
            Arc::new(RetryKey::random()),
            cfg,
            Arc::new(EchoHandler),
            // ★ 这两条端到端只验 h3 本体，不验换代 ⇒ 不给转交口。
            //   ⚠ 而「没有转交口时 select! 不许 panic」正好被它们顺带钉住。
            None,
            None,
        ));

        let got = tokio::time::timeout(Duration::from_secs(20), one_h3_request(server, "/hi?x=1"))
            .await
            .expect("端到端超时了（20s）—— 链条某处没走通");

        srv.abort();

        assert_eq!(got.status, "200");
        assert_eq!(got.body, b"hello from h3");

        let find = |k: &str| {
            got.headers
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        // ★ 请求头真的翻译过去了（:path / :authority→host / 普通头）。
        assert_eq!(find("x-seen-path").as_deref(), Some("/hi"));
        assert_eq!(find("x-seen-host").as_deref(), Some(SNI));
        assert_eq!(find("x-seen-ua").as_deref(), Some("fulcrum-test"));
        // ★ 响应头也翻回来了。
        assert_eq!(find("content-type").as_deref(), Some("text/plain"));
        // ★ ★ ★ 而处理器**故意插的那个逐跳头必须消失** ——
        //   单测里那条过滤器是绿的，但那只证明函数本身对，证不了它接在链上。
        assert!(
            find("connection").is_none(),
            "逐跳头 connection 漏到了客户端 —— RFC 9114 §4.2 的过滤器没接在链上"
        );
    }

    /// 把请求体原样回显的处理器。
    struct EchoBodyHandler;

    #[async_trait]
    impl H3RequestHandler for EchoBodyHandler {
        async fn handle(&self, mut session: ServerSession) {
            let mut body = Vec::new();
            // ⚠ 读到 `None` 为止 —— 少读一次就少一段。
            while let Ok(Some(chunk)) = session.read_request_body().await {
                body.extend_from_slice(&chunk);
            }
            let mut resp = pingora_http::ResponseHeader::build(200, None).expect("造响应头");
            resp.insert_header("x-body-len", body.len().to_string())
                .expect("插");
            let _ = session.write_response_header(Box::new(resp)).await;
            let _ = session
                .write_response_body(bytes::Bytes::from(body), true)
                .await;
            let _ = session.finish().await;
        }
    }

    /// 客户端：发一个带体的请求，把响应体读回来。
    async fn one_h3_post(server: SocketAddr, payload: &[u8]) -> Got {
        let sock = UdpSocket::bind("127.0.0.1:0").await.expect("客户端 socket");
        let local = sock.local_addr().expect("客户端地址");
        let mut cfg = client_config();
        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut conn =
            quiche::connect(Some(SNI), &scid, local, server, &mut cfg).expect("客户端连接");

        let h3_cfg = quiche::h3::Config::new().expect("h3 配置");
        let mut h3: Option<quiche::h3::Connection> = None;
        let mut req_sid: Option<u64> = None;
        let mut left = payload;
        let mut got = Got {
            status: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let mut done = false;
        let mut out = vec![0u8; DATAGRAM_MAX];
        let mut inbuf = vec![0u8; DATAGRAM_MAX];

        while !done {
            loop {
                match conn.send(&mut out) {
                    Ok((n, info)) => {
                        sock.send_to(&out[..n], info.to).await.expect("客户端发包");
                    }
                    Err(quiche::Error::Done) => break,
                    Err(e) => panic!("客户端 send 出错：{e}"),
                }
            }
            if conn.is_closed() {
                break;
            }
            if h3.is_none() && conn.is_established() {
                h3 = Some(
                    quiche::h3::Connection::with_transport(&mut conn, &h3_cfg).expect("客户端 h3"),
                );
            }
            if let Some(h) = h3.as_mut() {
                if req_sid.is_none() {
                    let req = [
                        quiche::h3::Header::new(b":method", b"POST"),
                        quiche::h3::Header::new(b":scheme", b"https"),
                        quiche::h3::Header::new(b":authority", SNI.as_bytes()),
                        quiche::h3::Header::new(b":path", b"/echo"),
                    ];
                    req_sid = Some(h.send_request(&mut conn, &req, false).expect("发请求头"));
                }
                // ★ ★ 客户端这一侧同样会短写 —— 写不完就留着下一轮。
                if let Some(sid) = req_sid
                    && !left.is_empty()
                {
                    match h.send_body(&mut conn, sid, left, left.len() <= 1) {
                        Ok(n) => left = &left[n..],
                        Err(quiche::h3::Error::Done) => {}
                        Err(e) => panic!("客户端发体出错：{e}"),
                    }
                    if left.is_empty() {
                        // 体发完了，补一个空的 fin。
                        let _ = h.send_body(&mut conn, sid, &[], true);
                    }
                }
                loop {
                    match h.poll(&mut conn) {
                        Ok((_, quiche::h3::Event::Headers { list, .. })) => {
                            for x in list {
                                let n = String::from_utf8_lossy(x.name()).into_owned();
                                let v = String::from_utf8_lossy(x.value()).into_owned();
                                if n == ":status" {
                                    got.status = v;
                                } else {
                                    got.headers.push((n, v));
                                }
                            }
                        }
                        Ok((sid, quiche::h3::Event::Data)) => {
                            let mut chunk = vec![0u8; 8192];
                            while let Ok(n) = h.recv_body(&mut conn, sid, &mut chunk) {
                                got.body.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Ok((_, quiche::h3::Event::Finished)) => done = true,
                        Ok(_) => {}
                        Err(quiche::h3::Error::Done) => break,
                        Err(e) => panic!("客户端 poll 出错：{e}"),
                    }
                }
            }
            if done {
                break;
            }
            let t = conn.timeout().unwrap_or(Duration::from_millis(10));
            match tokio::time::timeout(t, sock.recv_from(&mut inbuf)).await {
                Ok(Ok((n, from))) => {
                    let info = quiche::RecvInfo { from, to: local };
                    let _ = conn.recv(&mut inbuf[..n], info);
                }
                Ok(Err(e)) => panic!("客户端 recv 出错：{e}"),
                Err(_) => conn.on_timeout(),
            }
        }
        got
    }

    /// ★★★ **一次覆盖两条已写进文档、而此前一条判据都没有的陷阱**：
    ///
    /// 1. `recv_body` **必须读到 `Done`** —— `Data` 事件不会重新武装，
    ///    少读一次这条流剩下的体就再也不会来通知；
    /// 2. `send_body` **会短写** —— 不留待发队列的话，**大响应会被静默截断**。
    ///
    /// ⚠ 两者的失效表现**都是「内容少了一截，而连接看起来好好的」** ——
    /// 小响应的判据（上面那条）**原理上看不见它们**：一次就写完了、一次就读完了。
    /// ⇒ 体积必须**大于一次收发能装下的量**，这里取 **256 KiB**
    /// （远大于 `BODY_CHUNK` 的 16 KiB，也大于初始流控窗口）。
    #[tokio::test]
    async fn 大体积请求与响应不被截断() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("服务端 socket"));
        let server = sock.local_addr().expect("服务端地址");
        let (_tx, shutdown) = tokio::sync::watch::channel(false);

        let srv = tokio::spawn(serve(
            sock,
            shutdown,
            GenId::random(),
            Arc::new(RetryKey::random()),
            server_config(),
            Arc::new(EchoBodyHandler),
            None,
            None,
        ));

        // 一段可校验的载荷：每个字节都由它的位置决定 ⇒ **错位也看得出来**，
        // 而不只是「长度对不对」。
        let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();

        let got = tokio::time::timeout(Duration::from_secs(30), one_h3_post(server, &payload))
            .await
            .expect("大体积端到端超时了（30s）");

        srv.abort();

        assert_eq!(got.status, "200");
        let seen = got
            .headers
            .iter()
            .find(|(n, _)| n == "x-body-len")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(
            seen,
            payload.len().to_string(),
            "服务端收到的请求体长度不对 —— `recv_body` 多半没读到 Done"
        );
        assert_eq!(
            got.body.len(),
            payload.len(),
            "客户端收到的响应体长度不对 —— `send_body` 的短写多半没留待发队列"
        );
        assert!(
            got.body == payload,
            "回显的内容与发出去的不一致（错位或串了）"
        );
    }

    // ── M2 批 K：转交接线的两条判据 ─────────────────────────────────────────

    /// `run_dir` 是**推导**出来的，所以它有自己的判据。
    ///
    /// ⚠ 「取 `upgrade_sock` 的父目录」是一条**推导**，要么坐实它、要么加一个显式字段，
    /// 别默认它成立 —— 这条判据就是坐实它的那一半。
    #[test]
    fn 转交目录取的是换代_socket_的父目录() {
        use std::path::Path;
        assert_eq!(
            run_dir_of("/run/fulcrum/upgrade.sock"),
            Path::new("/run/fulcrum")
        );
        // 判据里两个实例共用一个 $WORK，各给一个不同的 sock 名 —— 那时目录是同一个，
        // ★ 而它们的 `gen_id` 不同 ⇒ 路径仍然不撞。
        assert_eq!(run_dir_of("/tmp/x/a.sock"), Path::new("/tmp/x"));
        // ⚠ 没有父目录时回 `.` —— **那是它实际会落的地方**，不是一个占位。
        assert_eq!(run_dir_of("upgrade.sock"), Path::new("."));
        assert_eq!(run_dir_of(""), Path::new("."));
    }

    /// ★ ★ ★ G109 ④「转交只走一跳」—— **行为那一半**。
    ///
    /// 结构那一半由 [`dispatch_relayed`] 的签名保证（它拿不到 `GenId` / `RetryKey` /
    /// `RelayOutbox`，所以「再转交一次」写不出来）；这一条量的是它**认不出来就丢**，
    /// ⚠ 而不是「顺手当成新连接」——那样一个畸形数据报就能让我们建一份状态。
    #[tokio::test]
    async fn 转交进来的包认不出_dcid_就丢_而不是走别的路() {
        let conns: HashMap<Vec<u8>, LiveConn> = HashMap::new();
        let from: SocketAddr = "192.0.2.9:44300".parse().expect("测试地址");
        let to: SocketAddr = "192.0.2.1:443".parse().expect("测试地址");

        // ① 完全不是 QUIC 包 ⇒ 包头解析不出来 ⇒ 丢。
        let mut junk = *b"not-a-quic-datagram-at-all";
        assert!(!dispatch_relayed(&conns, from, to, &mut junk));

        // ② 是个像样的短包头、DCID 也够长，但**没有人认领** ⇒ 丢。
        //   ★ 这一条才是「只走一跳」的正体：认不出来时**没有任何后路**。
        let mut short = vec![0u8; 1 + SCID_LEN + 8];
        short[0] = 0x40; // 短包头（固定位置 1、长包头位 0）
        short[1..1 + SCID_LEN].copy_from_slice(&[0xab; SCID_LEN]);
        assert!(!dispatch_relayed(&conns, from, to, &mut short));

        // ③ 反向的一半：**认得出来的就该送进去** —— 否则上面两条与「恒回 false」无法区分。
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(async {});
        let mut conns2: HashMap<Vec<u8>, LiveConn> = HashMap::new();
        conns2.insert(
            vec![0xab; SCID_LEN],
            LiveConn {
                tx,
                task: task.abort_handle(),
            },
        );
        let mut ok = short.clone();
        assert!(
            dispatch_relayed(&conns2, from, to, &mut ok),
            "认得出的 DCID 必须送进那条连接 —— 否则上面两条什么都没证明"
        );
        let (_pkt, f, t) = rx.recv().await.expect("那条连接该收到它");
        // ★ ★ 送进去的是**报文自带的那一对地址**，不是建连时那一对 ——
        //   转交进来的数据报是别的进程收到的，只有它自己知道 `from`。
        assert_eq!((f, t), (from, to));
    }
}
