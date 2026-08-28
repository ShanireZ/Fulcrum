//! 数据面：把[运行时对象图](fulcrum_runtime::Runtime)挂到 Pingora 上。
//!
//! ```text
//! 结构化配置 ──▶ Runtime（纯逻辑，决定「该做什么」）
//!                   │
//!                   ▼
//!            本 crate（用 Pingora 把它做出来）
//! ```
//!
//! # 模块地图
//!
//! | 模块 | 管什么 |
//! |---|---|
//! | 本文件 | h1/h2 入口、执行链、路由、装载摘要 |
//! | [`l4`] | L4 面：TCP/UDP 透传、SNI/ALPN 分流、PROXY protocol |
//! | [`files`] | 静态文件 |
//! | [`cache`] | HTTP 缓存（语义层 + 内存/磁盘后端 + 防惊群） |
//! | [`encode`] | 响应压缩与预压缩旁文件 |
//! | [`quic`] | HTTP/3 入口与换代时的跨进程连接转交 |
//! | [`access_log`] | 结构化访问日志 |
//! | [`metrics`] | Prometheus 指标：注册表 + text exposition 渲染 |
//! | [`acme`] · [`health`] · [`dns`] | 自动 HTTPS · 主动健康检查 · 上游域名重解析 |
//! | [`admin`] · [`process`] | 管理面 · 进程托管（`sd_notify` / pid 文件 / `SIGUSR2` 换代） |
//!
//! ⚠ **哪些能力认得但还没接线，以 [`fulcrum_runtime::UNWIRED`] 为准** ——
//! 这里不复述一份会烂掉的清单。
//!
//! # ⛔ 回落层已整层删除（G98）
//!
//! 写了 `fallback_nginx` / `fallback_caddy` 的配置会编译不过，并给一条专门的诊断
//! （`compile.rs` 的 `removed_global_option`）。★ 它留下的两句话在别处照样成立：
//! ① **501 与 502 是两个不同的事实，不许合并**（配置遗漏 vs 后端故障）；
//! ② **一个静静躺着的死配置，与一个正在生效的配置，在配置文件里长得一模一样。**
//!
//! # ★ 客户端 IP 只取 socket 对端，绝不取 XFF
//!
//! `X-Forwarded-For` 的最左项**客户端可以随便写** —— 拿它喂 `remote_ip` 匹配器，
//! 等于让任何人自称来自 `10.0.0.0/8`。⚠ 将来支持「信任前置代理」时，
//! 判据是**最近一跳**，且必须显式配置。

pub mod access_log;
pub mod acme;
pub mod admin;
pub mod cache;
pub mod dns;
/// 响应压缩（M2 批 I）：`encode` 接线，用的是 fork 里那份压缩模块（G100）。
pub mod encode;
pub mod files;
pub mod health;
/// L4 面（M2 批 A：TCP，批 B：UDP）：自建监听器 + socket 移交。
pub mod l4;
/// Prometheus 指标（M2 批 M）：进程级注册表 + text exposition 的自研渲染器（G117）。
pub mod metrics;
pub mod process;
mod proxyproto;
/// HTTP/3 入口（M2 批 J）：QUIC 传输层 + `quiche::h3`。
pub mod quic;
pub mod tls;

use async_trait::async_trait;
use bytes::Bytes;
use fulcrum_acme::Http01Store;
use fulcrum_runtime::request::{Headers, RequestCtx, ResponseCtx};
use fulcrum_runtime::template::Template;
use fulcrum_runtime::{
    CacheRt, HeaderOpRt, Outcome, ProxyTarget, Routed, Runtime, SiteRt, Upstream,
};
use log::{debug, error, info, warn};
use pingora_core::apps::{HttpPersistentSettings, HttpServerApp, ReusedHttpStream};
use pingora_core::connectors::http::Connector;
use pingora_core::protocols::http::ServerSession;
use pingora_core::server::ShutdownWatch;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::SystemTime;

/// keep-alive 空闲窗口。★ 停机时会被改成 `None`（不续），让连接自然收敛。
const KEEPALIVE_SECS: u64 = 60;

/// `&RequestHeader` 的 [`Headers`] 适配器。零拷贝。
struct ReqHeaders<'a>(&'a RequestHeader);

impl Headers for ReqHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        // `http::HeaderMap` 的查找本身就是大小写不敏感的。
        self.0.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// `&ResponseHeader` 的适配器，给 `{resp_header.*}` 用。
struct RespHeaders<'a>(&'a ResponseHeader);

impl Headers for RespHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// 在飞连接计数的 RAII 守卫。
///
/// ★ ★ **为什么必须是守卫而不是手写一对调用**：转发路径上每一步都可能 `?` 提前返回，
/// 而漏掉一次 `release` 会让 `least_conn` 的计数**单调漂移**，最终永远不选这个上游——
/// **而这件事不会有任何报错**。把配对交给 `Drop`，就没有「忘了」这个选项。
struct Inflight<'a>(&'a Upstream);

impl<'a> Inflight<'a> {
    fn acquire(u: &'a Upstream) -> Inflight<'a> {
        u.acquire();
        Inflight(u)
    }
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// 下游响应的**唯一出口**（**M2 批 J 第六步**，G110 的落点）。
///
/// # 为什么是一个类型，而不是「记得在四个地方各加一行」
///
/// G110 要求 h1/h2 的**每一条**响应都发 `Alt-Svc`，而写响应头的地方有四处，
/// 第五处随时会出现。⇒ 换成这个类型：它 `Deref` 到 `ServerSession`，
/// 而 [`Downstream::write_response_header`] 是**内在方法**
/// ⇒ **内在方法优先于 `Deref` 过去的同名方法**，既有调用点一个字都不用改。
///
/// ⚠ 同名是**有意的**：换个名字等于要求下一个人记得用新名字。
/// 第二道保险是 `crates/fulcrum/tests/response_gate.rs`，它钉住
/// 「`.write_response_header(` 只许出现在这一处实现里」。
///
/// ⚠ **h1/h2 用 [`Downstream::advertising`]，h3 用 [`Downstream::plain`]** ——
/// 已经在 h3 上的客户端不需要被告知有 h3。★ 这个差别落在两个调用点上，
/// 而不是落在一处「这条连接是不是 h3」的推断上。
pub(crate) struct Downstream<'a> {
    session: &'a mut ServerSession,
    /// 要发的 `Alt-Svc` 值；`None` = 不发。
    alt_svc: Option<&'a str>,
    /// 这一次请求在访问日志里的样子，**逐段填**（M2 批 L 第 ② 步）。
    pub(crate) record: access_log::Record,
}

impl<'a> Downstream<'a> {
    /// **h1/h2 入口**。
    ///
    /// ⚠ ⚠ 从 `advertising` 改名成 `h1h2`，而改名是有理由的：
    /// 这个构造函数现在决定**两件**事 —— 发不发 `Alt-Svc`（G110），
    /// 以及访问日志里的 `proto`。★ 两件事按**同一个岔口**分开
    /// （h1/h2 入口 vs h3 入口），所以名字应当叫那个岔口，
    /// 而不是叫其中一件事的效果 —— 否则下一个人会以为另一件事是「顺带的」。
    pub(crate) fn h1h2(session: &'a mut ServerSession, alt_svc: Option<&'a str>) -> Downstream<'a> {
        // ★ h1 与 h2 的区别问 session 自己 —— 那是它**知道**的事，不是推断。
        let proto = if session.is_http2() {
            "HTTP/2.0"
        } else {
            "HTTP/1.1"
        };
        Downstream {
            session,
            alt_svc,
            record: access_log::Record::new(proto),
        }
    }

    /// **h3 入口**：不发 `Alt-Svc`（客户端已经在 h3 上了），`proto` 恒为 `HTTP/3.0`。
    pub(crate) fn h3(session: &'a mut ServerSession) -> Downstream<'a> {
        Downstream {
            session,
            alt_svc: None,
            record: access_log::Record::new("HTTP/3.0"),
        }
    }

    /// ★ ★ **跑一次请求，然后一定收尾一次** —— 两件事绑在同一个方法里，
    /// 于是「每一条请求都记一行、都记一次数」不是一件要靠记性的事，
    /// 加新入口时这里不用改。
    ///
    /// ⚠ 收尾走 [`access_log::Record::finish`] 而不是 `emit`：**指标不跟着
    /// `log` 配置早退**（没配日志的站点照样要有指标），理由写在 `finish` 上。
    ///
    /// ⚠ `status` 与 `resp_size` **现问 `ServerSession`**，不在 `Record` 里另存一份 ——
    /// 两份迟早不一致，而不一致的那天没有任何东西会说。
    /// ★ 没写出任何响应头时 `status` 是 **0**：那不是「未知」，是「什么都没发生」，
    /// `outcome` 会写 `aborted`。
    pub(crate) async fn serve(&mut self, app: &FulcrumApp) {
        app.serve_one(self).await;
        let status = self
            .session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        let size = self.session.body_bytes_sent();
        // ── 白名单头与 TLS 信息（**M2 批 L 第 ③ 步**）──────────────────────
        //
        // ★ ★ 三样都在**这里**问会话，而不是在执行链中途各记一份 ——
        //   与 `status` / `resp_size` 逐字同一条理由。
        // ⚠ 而它们**只能**在 `serve_one` 之后问，各有各的原因：
        //   ① 白名单是**路由之后**才知道的（`log` 是站点块内的指令 ⇒ `target` 那时才有）；
        //   ② 响应头要等响应真的写出去；
        //   ③ TLS 那份 digest 从连接建起来就有，放这里只是为了三件事在一处。
        if let Some(cfg) = self.record.target.clone() {
            if !cfg.req_headers.is_empty() {
                let picked =
                    access_log::collect(&cfg.req_headers, &self.session.req_header().headers);
                self.record.req_headers = picked;
            }
            if !cfg.resp_headers.is_empty()
                && let Some(resp) = self.session.response_written()
            {
                let picked = access_log::collect(&cfg.resp_headers, &resp.headers);
                self.record.resp_headers = picked;
            }
            self.record.tls = tls_fields(self.session);
        }
        self.record.finish(status, size);
    }

    /// 写下游响应头 —— **本 crate 数据面唯一的一处**。
    ///
    /// ⚠ 与 `ServerSession::write_response_header` **同名是有意的**，见类型文档。
    ///
    /// ★ 用 `insert_header` 而不是 `append_header`：一条响应上出现两个 `Alt-Svc`
    /// 不是错误（RFC 7838 允许），但**上游若已经发了一个，那是它的 h3、不是我们的**
    /// —— 我们是终结这条 TLS 连接的人，端口的话语权在我们这里。
    pub(crate) async fn write_response_header(
        &mut self,
        mut resp: Box<ResponseHeader>,
    ) -> pingora_core::Result<()> {
        if let Some(v) = self.alt_svc {
            // ⚠ 失败只可能是头值非法，而这个值是我们自己拼的常量形状 ⇒ 不该发生；
            //   真发生了也不能把整条响应搭进去。
            if let Err(e) = resp.insert_header("Alt-Svc", v) {
                warn!("Alt-Svc 装不进响应头（{v}）：{e}");
            }
        }
        self.session.write_response_header(resp).await
    }
}

impl std::ops::Deref for Downstream<'_> {
    type Target = ServerSession;
    fn deref(&self) -> &ServerSession {
        self.session
    }
}

impl std::ops::DerefMut for Downstream<'_> {
    fn deref_mut(&mut self) -> &mut ServerSession {
        self.session
    }
}

/// 一次请求的只读视图，打包传给转发路径。
///
/// ★ clippy 的 `too_many_arguments` 在这里说得对：八个参数里有四个是
/// **同一次请求的不同侧面**，本来就该是一个东西。把它们捆起来之后，
/// 转发函数的签名也读得出「它需要请求的哪几个面」。
#[derive(Clone, Copy)]
struct ReqView<'a> {
    ctx: &'a RequestCtx<'a>,
    downstream_req: &'a RequestHeader,
    effective_path: &'a str,
    started: SystemTime,
}

/// 挂在一个监听端口上的枢衡应用。
///
/// ⚠ **每个端口一个实例**：站点索引是按 `(Host, 本地端口)` 两维查的，
/// 而 Pingora 的 `ServerSession` 拿不到「这条连接落在哪个监听器上」。
/// 把端口写进 app 里，是唯一不靠猜的办法。
pub struct FulcrumApp {
    /// ★ **可在运行期整体换掉**（G8 的全量原子 load）。
    /// ⚠ 每个请求只在入口取一次快照——理由见 `SharedRuntime` 的类型文档：
    /// 一次发生在请求中途的 load 会让同一个请求按旧配置路由、按新配置转发，
    /// 那正是 G8 明令禁止的「部分生效」。
    rt: Arc<fulcrum_runtime::SharedRuntime>,
    port: u16,
    /// 这个监听端口是不是 TLS。★ 它决定 `{scheme}` 展开成什么，
    /// 也决定 `redir` 里的 `https://{host}` 是否自洽。
    https: bool,
    connector: Connector,
    /// ACME 的 HTTP-01 应答表。★ 没开自动签发时它是空的，而**空表什么都不接**——
    /// 于是「有没有 ACME」不改变数据面的任何一条行为。
    http01: Arc<Http01Store>,
    /// 自研 HTTP 缓存（M2 批 G）。
    ///
    /// ⚠ ⚠ **它挂在 app 上而不是在 `Runtime` 里**，这一点是有意的：
    /// G8 的全量原子 load 会把整棵运行时图换掉，而**缓存内容不该跟着没** ——
    /// 每 reload 一次就清空缓存，等于每次改配置都让上游挨一次满负荷。
    /// ★ 于是「换配置」与「缓存里有什么」是两件独立的事，正如它们本来就该是。
    cache: Arc<cache::CacheHandle>,
    /// 这个端口上要对 h1/h2 响应发的 `Alt-Svc`（**G110**）；`None` = 不发。
    ///
    /// ★ 它是**接线那一步**算好的常量串，不是每条响应现拼：值只取决于
    /// 「这个监听端口开没开 h3」，而端口集变了只能走真的换代。
    /// ⇒ 数据面这一路上没有一处需要再判「h3 开了吗」。
    ///
    /// ⚠ **不发它，浏览器永远不会主动尝试 h3** —— h3 入口对真实用户等于不存在。
    alt_svc: Option<String>,
}

impl FulcrumApp {
    pub fn new(
        rt: Arc<fulcrum_runtime::SharedRuntime>,
        port: u16,
        https: bool,
        http01: Arc<Http01Store>,
        cache: Arc<cache::CacheHandle>,
        alt_svc: Option<String>,
    ) -> FulcrumApp {
        FulcrumApp {
            rt,
            port,
            https,
            connector: Connector::new(None),
            http01,
            cache,
            alt_svc,
        }
    }
}

/// h1/h2 那一侧交给 Pingora 的入口 —— 只是把 [`FulcrumApp`] 包了一层。
///
/// # 它存在的唯一理由：让两个入口共用**同一个** app 实例
///
/// h3 入口要 `Arc<FulcrumApp>`，而 pingora 的 `Service::new` 收的是 `A` 不是 `Arc<A>`。
/// ⇒ 不套这一层就得建**两个** `FulcrumApp`，也就是**两个上游连接池**
/// （`Connector` 每实例一个）—— ⚠ 这件事不会有任何报错，只表现为上游连接数翻倍。
/// ★ 更贵的是「两个实例必须构造得一模一样」又是一条靠人记着的约束。
///
/// ⚠ 孤儿规则挡住了更直接的写法（`impl HttpServerApp for Arc<FulcrumApp>` 不合法），
/// 所以必须是一个本地 newtype。
pub struct H1Entry(Arc<FulcrumApp>);

impl H1Entry {
    pub fn new(app: Arc<FulcrumApp>) -> H1Entry {
        H1Entry(app)
    }
}

#[async_trait]
impl HttpServerApp for H1Entry {
    async fn process_new_http(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        // ⚠ 必须写成 UFCS：`self.0.process_new_http(…)` 会先撞上
        //   `Arc<H1Entry>` 自己的那一个（方法解析从 `Self` 开始），成了递归。
        HttpServerApp::process_new_http(&self.0, session, shutdown).await
    }
}

/// h3 那一侧的入口 —— **同一个 [`FulcrumApp`]、同一条执行链**。
///
/// ★ 依赖方向：`quic` 模块只认 [`quic::h3_conn::H3RequestHandler`] 这个 trait，
/// **不认识** `FulcrumApp` ⇒ HTTP/3 入口不会反向依赖数据面本体。
///
/// ⚠ 与 h1/h2 的**唯一**差别是 [`Downstream::plain`]（已经在 h3 上的客户端
/// 不需要再被告知有 h3），而它落在**这一行**上。
#[async_trait]
impl quic::h3_conn::H3RequestHandler for FulcrumApp {
    async fn handle(&self, mut session: ServerSession) {
        // ⚠ 请求头由 h3 那一层在建 `H3Session` 时就填好了 —— 这里**不能**再
        //   `read_request()`：h1 那侧它是「从 socket 上读一个请求头」，
        //   而 h3 上没有那个动作。
        {
            let mut out = Downstream::h3(&mut session);
            out.serve(self).await;
        }
        // ★ h3 上 `finish()` 收的是「这条流完了」，不是「这条连接完了」——
        //   一条 QUIC 连接上还会有别的流。
        if let Err(e) = session.finish().await {
            debug!("h3 收尾失败：{e}");
        }
    }
}

#[async_trait]
impl HttpServerApp for FulcrumApp {
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        match session.read_request().await {
            Ok(true) => {}
            Ok(false) => {
                debug!("读不到请求头");
                return None;
            }
            Err(e) => {
                debug!("下游读失败：{e}");
                return None;
            }
        }

        // ★ 停机窗口内不再续 keep-alive：让连接自己收敛，而不是等被砍断。
        if *shutdown.borrow() {
            session.set_keepalive(None);
        } else {
            session.set_keepalive(Some(KEEPALIVE_SECS));
        }

        // ★ ★ `Alt-Svc` 在这里接上（**G110**）：h1/h2 的每一条响应都带它，
        //   而「每一条」是由 `Downstream` 这个类型保证的，不是由记性保证的。
        {
            let mut out = Downstream::h1h2(&mut session, self.alt_svc.as_deref());
            out.serve(self).await;
        }

        let persistent = HttpPersistentSettings::for_session(&session);
        match session.finish().await {
            Ok(c) => c.map(|s| ReusedHttpStream::new(s, Some(persistent))),
            Err(e) => {
                debug!("收尾失败：{e}");
                None
            }
        }
    }
}

impl FulcrumApp {
    pub(crate) async fn serve_one(&self, session: &mut Downstream<'_>) {
        let started = SystemTime::now();
        // ★ ★ **整次请求的配置快照，只在这里取一次。**
        //   之后所有阶段用的都是 `rt` 这一份，而不是再去问 `self.rt`。
        //   ⚠ 每阶段各取一次的话，一次发生在请求中途的全量 load 会让**同一个请求**
        //   按旧配置路由、按新配置转发——G8 明令禁止的「部分生效」。
        let rt = self.rt.current();
        // ── 1. 把 Pingora 的请求头翻成 RequestCtx ──────────────────────────
        //
        // ★ ★ **请求头先克隆一份**。原本想零拷贝地借用 `session.req_header()`，
        //   但那个借用要活到整次请求结束（匹配器可能查任意一个头），
        //   而写响应需要 `&mut session` —— 借用检查器当场把这条路堵死。
        //   ⚠ 这不是 borrow checker 在挑刺：转发时本来就要把请求头**改一份**
        //   发给上游（`header_up`），所以这一份克隆下面还要再用一次，并不白花。
        let req = session.req_header().clone();
        let (remote_ip, remote_port) = client_addr(session);
        let headers = ReqHeaders(&req);
        let host_raw = host_of(&req);
        // Host 里可能带端口（`a.com:8443`）——站点索引只认主机名。
        let host = host_raw.split(':').next().unwrap_or("").to_string();
        let path = req.uri.path().to_string();
        let query = req.uri.query().unwrap_or("").to_string();
        let method = req.method.as_str().to_string();

        let ctx = RequestCtx {
            host: &host,
            port: self.port,
            scheme: if self.https { "https" } else { "http" },
            method: &method,
            path: &path,
            query: &query,
            headers: &headers,
            remote_ip,
            remote_port,
        };

        // ── 访问日志：请求那一半（**M2 批 L 第 ② 步**）────────────────────
        //
        // ★ `uri` 取的是**原始**请求目标（`rewrite` 之前）—— 契约里写死的。
        //   ⚠ 取 `effective_path` 的话，一条 `rewrite` 会让日志说出一个
        //   **客户端从没请求过**的地址，而排障时那是最误导人的一种。
        session.record.method = method.clone();
        session.record.host = host.clone();
        session.record.uri = if query.is_empty() {
            path.clone()
        } else {
            format!("{path}?{query}")
        };
        session.record.remote_ip = remote_ip;
        session.record.remote_port = remote_port;

        // ── 1.5 ACME 的 HTTP-01 挑战（RFC 8555 §8.3）────────────────────────
        //
        // ★ ★ **必须在路由之前**。一份 `respond 403` 的配置完全合法，
        //   而如果挑战应答走正常路由，**用户的配置会把自己的证书签发挡掉**——
        //   现场看到的只是「CA 说验不过」，配置里没有任何一行看得出问题。
        // ★ 表空的时候（没开自动签发、或已经签完摘掉了）这里什么都不接，
        //   请求原样落回路由：**有没有 ACME 不改变数据面的任何一条行为**。
        if let Some(key_auth) = self.http01.answer(&path) {
            debug!("HTTP-01 应答 {path}");
            // ⚠ 它在路由**之前**，所以没有站点、也没有站点的 `log` 配置
            //   ⇒ 这一条记不进访问日志（与 421 同一个形状，见 §11 D26）。
            //   ★ 仍然把 `outcome` 填对：将来 D26 拍板之后它就是现成的。
            session.record.outcome = "acme_http01";
            // RFC 8555 §8.3 建议 `application/octet-stream`。
            write_with_headers(
                session,
                200,
                Some(key_auth),
                vec![("Content-Type".into(), "application/octet-stream".into())],
            )
            .await;
            return;
        }

        // ── 2. 路由 ────────────────────────────────────────────────────────
        let Some(routed) = rt.route(&ctx) else {
            // ★ 无站点匹配 → 421（G63）。**不静默交给某个站点**——那是 nginx
            //   `default_server` 那类行为的温床。
            let status = rt.defaults.no_site_match;
            debug!("无站点匹配：Host={host} port={}", self.port);
            session.record.outcome = "no_site_match";
            // ── `fulcrum_no_site_match_total{host}`（G118）─────────────────
            //
            // ★ 记在**写 `outcome` 的同一处**：这两句说的是同一件事，
            //   分开就有一天会有人只改其中一句。
            // ⚠ ⚠ `host` 由请求方给、攻击者可控 ⇒ **只有出现在配置里的地址字面量
            //   才带真值**，其余一律 `<other>` —— 上界由配置定、不由访问者定。
            //   ★ 通配字面量 `*.wild.example` 的子域名判**未知**：它不是一条地址
            //     字面量，而那正是这一句话的全部含义（见 `has_address_literal`）。
            // ⚠ 集合**从 `rt` 现问**，不在指标那边另存一份 —— 换一次配置那份就过期，
            //   而过期的样子是「某个 host 从此归 <other>」，没有任何东西会说。
            let h = host.to_ascii_lowercase();
            let label = if rt.has_address_literal(&h) {
                h.as_str()
            } else {
                "<other>"
            };
            metrics::NO_SITE_MATCH_TOTAL.inc(&[label]);
            write_simple(session, status, None, &[], &ctx, started).await;
            return;
        };

        // ── 访问日志：站点那一半 ──────────────────────────────────────────
        //
        // ★ ★ `outcome` 从 [`Outcome`] 那个枚举**一次算出来**，而不是在下面
        //   五个分支里各写一行。⇒ 将来加一种终结方式时，
        //   `outcome_name` 的穷尽匹配会**编不过**，而那正是契约里
        //   「`outcome` 是闭集」那句话的落法。
        session.record.site = Some(routed.site.name.clone());
        session.record.site_addr = Some(routed.site_addr.clone());
        session.record.target = routed.site.log.clone();
        session.record.outcome = outcome_name(&routed.outcome);

        // 改写过的路径要带给上游。
        let effective_path = routed
            .rewritten_path
            .clone()
            .unwrap_or_else(|| path.clone());

        match &routed.outcome {
            Outcome::Respond { status, body } => {
                let text = body
                    .map(|t| t.expand(&ctx, &resp_ctx(*status, None), &routed.captures, started));
                write_simple(
                    session,
                    *status,
                    text,
                    &routed.response_headers,
                    &ctx,
                    started,
                )
                .await;
            }
            Outcome::Redirect { to, code } => {
                let rc = resp_ctx(*code, None);
                let target = to.expand(&ctx, &rc, &routed.captures, started);
                let mut extra: Vec<(String, String)> = vec![("Location".into(), target)];
                collect_ops(
                    routed.response_headers.iter().copied(),
                    &ctx,
                    &rc,
                    &routed.captures,
                    started,
                    &mut extra,
                );
                write_with_headers(session, *code, None, extra).await;
            }
            // ── Prometheus 抓取端点（M2 批 M，G116）────────────────────────
            //
            // ★ 走的是 `Outcome::Redirect` 那条一模一样的路：先摆自己那个头，
            //   再让站点的 `header` 指令叠上去，最后交给 `write_with_headers`。
            //   ⚠ **不新开一条写响应的路** —— 另起一条的代价不是重复代码，
            //   而是访问日志的 `resp_size`、压缩、头处理三件事会在这一条路上
            //   与别的终结类**慢慢长得不一样**，而不一样的那天没有任何东西会说。
            // ⚠ `Content-Type` 摆在 `collect_ops` **之前**：写了
            //   `header Content-Type …` 的人是在明确要求换掉它，而
            //   `write_with_headers` 里那个 `insert_header` 是覆盖语义。
            Outcome::Metrics => {
                let rc = resp_ctx(200, None);
                let mut extra: Vec<(String, String)> = vec![(
                    "Content-Type".into(),
                    // ⚠ 这一串是 exposition 格式的版本标识，**不是随便写的 MIME**：
                    //   抓取端按它决定用哪个解析器。
                    "text/plain; version=0.0.4; charset=utf-8".into(),
                )];
                collect_ops(
                    routed.response_headers.iter().copied(),
                    &ctx,
                    &rc,
                    &routed.captures,
                    started,
                    &mut extra,
                );
                write_with_headers(session, 200, Some(metrics::render()), extra).await;
            }
            Outcome::NoRouteMatch => {
                let status = rt.defaults.no_route_match;
                self.write_error(&rt, session, routed.site, status, &routed, &ctx, started)
                    .await;
            }
            // ── 自研静态文件（M2 批 F）─────────────────────────────────────
            //
            // ★ 路径用的是 `effective_path`，也就是 `rewrite` **之后**那一个 ——
            //   与转发那条路取同一个值。⚠ 取 `ctx.path` 的话，一条
            //   `rewrite` 就会在「转发」与「发文件」两条路上给出两个不同的答案。
            // ★ 404 / 403 / 400 交回给站点的 `handle_errors`，与 `NoRouteMatch`
            //   同一条路 —— 一个站点的错误页不该因为它是静态站就长得不一样。
            Outcome::FileServer(fs) => {
                // ★ ★ 压缩（M2 批 I）：`encode` 是中间件，静态文件是终结类 ——
                //   与 Caddy 一样，中间件裹在终结类外面。
                //   ⚠ 而**预压缩旁文件优先**：那条路在 `files` 里面判，
                //   判中了它会把这个 encoder 丢掉（旁文件已经是压好的）。
                let enc = encode::Encoder::new(encode::wanted(&routed), &req);
                if let Err(status) =
                    files::serve(session, &req, fs, &effective_path, ctx.query, enc).await
                {
                    self.write_error(&rt, session, routed.site, status, &routed, &ctx, started)
                        .await;
                }
            }
            // ── 转发（可能被缓存裹住，M2 批 G）──────────────────────────────
            //
            // ★ ★ `cache` 是**中间件**，所以它不在这个 `match` 里占一支 ——
            //   它在 `routed.cache` 上，由这里决定要不要走带缓存的那条路。
            // ⚠ 缓存**只裹 `reverse_proxy`**：`respond` / `redir` 没有上游可省，
            //   而 `file_server` 的字节已经在本机磁盘上、再存一份内存是净亏。
            //   ★ 写在别处时装载日志会说出来（`log_load_summary`），不是静默忽略。
            Outcome::Proxy(target) => {
                let view = ReqView {
                    ctx: &ctx,
                    downstream_req: &req,
                    effective_path: &effective_path,
                    started,
                };
                let r = match routed.cache {
                    Some(c) => {
                        self.proxy_cached(&rt, session, target, &routed, &view, c)
                            .await
                    }
                    None => self.proxy(&rt, session, target, &routed, &view).await,
                };
                if let Err(status) = r {
                    self.write_error(&rt, session, routed.site, status, &routed, &ctx, started)
                        .await;
                }
            }
        }
    }

    /// 错误响应。优先用站点的 `handle_errors`。
    // ⚠ clippy 说它 8 个参数太多。这里放行而不是硬拆：其中 7 个本来就是
    //   「同一次请求的不同侧面」，而 `rt` 是批 9 加的**配置快照** ——
    //   把它塞进 `Routed` 或 `ReqView` 会让那两个类型多背一份生命周期，
    //   换来的只是参数表短一格。★ 真正要守的是「整次请求用同一份快照」，
    //   而那由 `serve_one` 里那一次 `current()` 保证，不是由参数个数保证。
    #[allow(clippy::too_many_arguments)]
    async fn write_error(
        &self,
        rt: &Runtime,
        // ⚠ 进到这里就意味着这一条最终是**错误页**，无论它原本要去哪 ——
        //   所以 `outcome` 在函数体第一行被改写成 `error`（见下）。
        session: &mut Downstream<'_>,
        site: &SiteRt,
        status: u16,
        routed: &Routed<'_>,
        ctx: &RequestCtx<'_>,
        started: SystemTime,
    ) {
        // ★ 进到这里就意味着这一条最终是**错误页** —— 无论它原本要去哪。
        //   ⚠ 写在函数第一行而不是每个调用点各写一次：调用点有四处，
        //   而「第五处随时会出现」（G110 那次的原话）。
        session.record.outcome = "error";
        match rt.error_page(site) {
            Some(page) => {
                // ★ `handle_errors` 块内 `{status}` 指的是**原始**错误码，
                //   不是它自己那条 respond 的码——否则那个占位符毫无用处。
                let rc = resp_ctx(status, None);
                let body = page
                    .body
                    .map(|t| t.expand(ctx, &rc, &routed.captures, started));
                write_simple(
                    session,
                    page.status,
                    body,
                    &routed.response_headers,
                    ctx,
                    started,
                )
                .await;
            }
            None => {
                write_simple(
                    session,
                    status,
                    None,
                    &routed.response_headers,
                    ctx,
                    started,
                )
                .await
            }
        }
    }

    /// 带缓存的转发。出错时返回该回给下游的状态码。
    ///
    /// 次序：算键 → 查缓存 →（新鲜 ⇒ 直接发）→（陈的 ⇒ 带校验器去重验证）
    /// →（没有 ⇒ 抢防惊群的闸，leader 回源、follower 等完**重新查一次**）。
    ///
    /// ⚠ ⚠ **follower 等完必须重新查缓存**，不能用 leader 递过来的东西 ——
    /// leader 可能取回了 `no-store` / `private` 的响应，复用它等于把一个私有响应
    /// 发给了另外 N 个客户端。
    async fn proxy_cached(
        &self,
        rt: &Runtime,
        session: &mut Downstream<'_>,
        target: &ProxyTarget,
        routed: &Routed<'_>,
        view: &ReqView<'_>,
        cfg: &CacheRt,
    ) -> Result<(), u16> {
        let ctx = view.ctx;
        let req = view.downstream_req;
        let primary = cache::key::primary(
            ctx.method,
            ctx.scheme,
            ctx.host,
            view.effective_path,
            ctx.query,
        );
        // ★ 这条链上 `encode` 要求的算法 —— 次级键归一化要用（G101）。
        //   ⚠ 查与存必须用**同一个**列表，所以它从这里一路带下去，
        //   而不是在两处各自从 `Routed` 取一遍。
        let encodings = encode::wanted(routed);
        let req_cc = cache::cc::parse_request(header_str(req, "cache-control").unwrap_or(""));

        // ── 第一次查 ──────────────────────────────────────────────────
        match self
            .cache_try_serve(session, &primary, req, &req_cc, encodings)
            .await
        {
            CacheAttempt::Served(r) => return r,
            CacheAttempt::Revalidate { secondary, entry } => {
                let pass = CachePass {
                    cfg,
                    handle: &self.cache,
                    primary: primary.clone(),
                    secondary,
                    revalidate: Some(entry),
                    req_cc: req_cc.clone(),
                    encodings: encodings.to_vec(),
                };
                return self
                    .proxy_with(rt, session, target, routed, view, Some(pass))
                    .await;
            }
            CacheAttempt::Miss => {}
        }

        // ⚠ ⚠ `only-if-cached`：客户端说「只要缓存里的」。没有就回 **504**
        //   （RFC 9111 §5.2.1.7），**不是**回源。
        if req_cc.only_if_cached {
            debug!("only-if-cached 而缓存里没有 → 504（{primary}）");
            return Err(504);
        }

        // ── 防惊群 ────────────────────────────────────────────────────
        let slot = self.cache.coalescer.acquire(&primary);
        let _leader = match slot {
            cache::coalesce::Slot::Leader(l) => Some(l),
            cache::coalesce::Slot::Follower(n) => {
                // ★ 等 leader 放闸。⚠ 带超时：leader 若卡在一个不回话的上游上，
                //   follower 不该跟着一起挂 —— 超时之后自己回源，那只是多一次回源。
                let waited =
                    tokio::time::timeout(std::time::Duration::from_secs(15), n.notified()).await;
                if waited.is_err() {
                    debug!("等 leader 超时，自己回源（{primary}）");
                }
                // ★ ★ **重新查**，而不是用 leader 的结果（理由见本函数文档）。
                if let CacheAttempt::Served(r) = self
                    .cache_try_serve(session, &primary, req, &req_cc, encodings)
                    .await
                {
                    return r;
                }
                None
            }
        };

        let pass = CachePass {
            cfg,
            handle: &self.cache,
            primary,
            secondary: String::new(),
            revalidate: None,
            req_cc,
            encodings: encodings.to_vec(),
        };
        self.proxy_with(rt, session, target, routed, view, Some(pass))
            .await
    }

    /// 查一次缓存，能直接发就发了。
    async fn cache_try_serve(
        &self,
        session: &mut Downstream<'_>,
        primary: &str,
        req: &RequestHeader,
        req_cc: &cache::cc::RequestCc,
        encodings: &[String],
    ) -> CacheAttempt {
        let hit = self
            .cache
            .store
            .get(primary, cache_key_header(req, encodings));
        let entry = match hit {
            cache::store::Lookup::Hit(e) => e,
            // ★ 两种未命中在日志里是分开的（见 `store::Lookup` 的注释），
            //   而在这里的处置相同：回源。
            cache::store::Lookup::VaryMiss { .. } | cache::store::Lookup::Miss => {
                return CacheAttempt::Miss;
            }
        };
        let now = now_unix();
        let age = (now - entry.stored_at).max(0) as u64;
        match cache::policy::freshness(&entry.cc, req_cc, entry.fresh_for, age) {
            cache::policy::Freshness::Fresh => CacheAttempt::Served(
                write_cached(
                    session,
                    &entry,
                    now,
                    cache::CacheState::Hit,
                    &self.cache.store,
                )
                .await,
            ),
            // ⚠ 陈的与「必须重验证」在这里**同一条路**：都去问上游。
            //   ★ 差别在于陈的那条**可以**在上游挂掉时退回来用（stale-if-error），
            //   而那一条这一批不做（G97 的最小集之外），所以现在两者确实一样。
            cache::policy::Freshness::Stale | cache::policy::Freshness::MustRevalidate => {
                let secondary =
                    cache::key::secondary(&entry.vary, cache_key_header(req, encodings));
                CacheAttempt::Revalidate { secondary, entry }
            }
        }
    }

    /// 原样转发（不缓存）。
    async fn proxy(
        &self,
        rt: &Runtime,
        session: &mut Downstream<'_>,
        target: &ProxyTarget,
        routed: &Routed<'_>,
        view: &ReqView<'_>,
    ) -> Result<(), u16> {
        self.proxy_with(rt, session, target, routed, view, None)
            .await
    }

    /// ★ ★ ★ **转发只有这一条路径，缓存叠在它上面。**
    ///
    /// 这不是省事：**能力翻译与语义映射一旦有了第二条路径就必然发生** ——
    /// 一个「带缓存的转发」与一个「不带缓存的转发」若是两份代码，
    /// 它们对 `header_up` / `rewrite` / 候选地址回退的处理迟早会分家，
    /// ⚠ 而现场表现是「配了缓存之后某个头就没了」。
    async fn proxy_with(
        &self,
        rt: &Runtime,
        session: &mut Downstream<'_>,
        target: &ProxyTarget,
        routed: &Routed<'_>,
        view: &ReqView<'_>,
        mut pass: Option<CachePass<'_>>,
    ) -> Result<(), u16> {
        let ReqView {
            ctx,
            downstream_req,
            effective_path,
            started,
        } = *view;
        let Some(up) = target.pick(ctx) else {
            error!("站点 {} 的 reverse_proxy 没有可用上游", routed.site.name);
            return Err(rt.defaults.all_upstreams_down);
        };
        let _guard = Inflight::acquire(up);

        // SNI 取上游**主机名**；`transport http` 时用不到。
        // ⚠ SNI 必须来自配置里写的那个名字，**不能用解析出来的 IP** ——
        //   拿 IP 去握手，上游的证书一定对不上。
        let sni = up.addr.split(':').next().unwrap_or("").to_string();
        // ★ ★ ★ **请求路径上绝不做 DNS**：这里拿的是后台任务解析好的地址。
        //   ⚠ 改之前这里是 `HttpPeer::new(up.addr.clone(), …)`，而
        //   `HttpPeer::new` 里是 `to_socket_addrs().unwrap()` ——
        //   于是每个请求做一次**阻塞** getaddrinfo，解析不了就 **panic**（实测过）。
        //   现在传的是 `SocketAddr`，它的 `to_socket_addrs()` 不会失败。
        //   ★ `pick()` 已经筛掉了解析不出来的上游，所以走到这里通常是 Some；
        //   ⚠ 但不 unwrap —— **「通常」不是判据**。
        let candidates = up.dial_candidates();
        if candidates.is_empty() {
            error!(
                "上游 {} 还没解析出地址（DNS 没通？）—— 本次请求回 {}",
                up.addr, rt.defaults.all_upstreams_down
            );
            return Err(rt.defaults.all_upstreams_down);
        }

        // ★ **逐个候选试，不是只试第一个** —— `localhost` 的第一个地址可能是 `[::1]`
        //   而上游只听 `127.0.0.1`，现场只有一句「连不上上游」。
        // ⚠ 只在**建连接**这一步回退：请求发出去之后再换上游那是重试语义，不归这里。
        let mut last_err: Option<String> = None;
        let mut got = None;
        // ⚠ 连上的**那一个** peer 要留着：收尾归还连接（`release_http_session`）
        //   必须用同一个 peer，否则连接池会按另一个 key 归档 —— 那等于池永远不复用。
        let mut used_peer: Option<HttpPeer> = None;
        // ★ 访问日志要的是**地址**。⚠ 别拿 `peer.to_string()` —— 实测它给的是
        //   `addr: …, scheme: HTTP, sni: …` 那样一串给人看的调试文本，
        //   而这一格是要被机器消费的。
        let mut used_dial: Option<std::net::SocketAddr> = None;
        for (i, dial) in candidates.iter().enumerate() {
            let peer = HttpPeer::new(*dial, target.tls, sni.clone());
            match self.connector.get_http_session(&peer).await {
                Ok(v) => {
                    if i > 0 {
                        debug!("上游 {} 的第 {} 个候选 {dial} 连上了", up.addr, i + 1);
                    }
                    got = Some(v);
                    used_dial = Some(*dial);
                    used_peer = Some(peer);
                    break;
                }
                Err(e) => last_err = Some(format!("{dial}：{e}")),
            }
        }
        let (Some((mut upstream, reused)), Some(peer)) = (got, used_peer) else {
            error!(
                "连不上上游 {}（{} 个候选全都失败，最后一条：{}）",
                up.addr,
                candidates.len(),
                last_err.unwrap_or_else(|| "（没有错误信息）".into())
            );
            return Err(rt.defaults.all_upstreams_down);
        };
        debug!("上游 {} （复用={reused}）", up.addr);
        // ★ 记**真的连上的那一个**，不是配置里写的那串候选 ——
        //   排障时想知道的是「这条请求最后落到谁身上」。
        session.record.upstream = used_dial.map(|d| d.to_string());

        // ── 上游请求头 ────────────────────────────────────────────────────
        let mut ureq = downstream_req.clone();
        if effective_path != ctx.path {
            // `rewrite` 改过路径：要带到上游去。查询串按原样保留。
            let new_uri = if ctx.query.is_empty() {
                effective_path.to_string()
            } else {
                format!("{effective_path}?{}", ctx.query)
            };
            match new_uri.parse::<http::Uri>() {
                Ok(u) => ureq.set_uri(u),
                Err(e) => {
                    error!("改写后的路径不是合法 URI（{new_uri}）：{e}");
                    return Err(500);
                }
            }
        }
        apply_ops_to_request(&mut ureq, &target.header_up, ctx, &routed.captures, started);
        // ★ 重验证：把缓存里那条的校验器带给上游（M2 批 G）。
        //   ⚠ 顺序在 `header_up` **之后**：用户显式写的 `header_up If-None-Match`
        //   应当赢 —— 那是他明确要求的，而这几条是我们替他加的。
        if let Some(cp) = &pass
            && let Some(e) = &cp.revalidate
        {
            if let Some(t) = &e.etag {
                let _ = ureq.insert_header("If-None-Match", t.clone());
            }
            if let Some(lm) = &e.last_modified {
                let _ = ureq.insert_header("If-Modified-Since", lm.clone());
            }
        }

        if let Err(e) = upstream.write_request_header(Box::new(ureq)).await {
            error!("写上游请求头失败：{e}");
            return Err(rt.defaults.all_upstreams_down);
        }

        // ── 请求体 ────────────────────────────────────────────────────────
        loop {
            match session.read_request_body().await {
                Ok(Some(chunk)) => {
                    if let Err(e) = upstream.write_request_body(chunk, false).await {
                        error!("写上游请求体失败：{e}");
                        return Err(rt.defaults.all_upstreams_down);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    debug!("读下游请求体失败：{e}");
                    return Err(400);
                }
            }
        }
        if let Err(e) = upstream.finish_request_body().await {
            error!("收尾上游请求体失败：{e}");
            return Err(rt.defaults.all_upstreams_down);
        }

        // ── 响应头 ────────────────────────────────────────────────────────
        if let Err(e) = upstream.read_response_header().await {
            error!("读上游响应头失败：{e}");
            return Err(rt.defaults.all_upstreams_down);
        }
        let Some(uresp) = upstream.response_header() else {
            error!("上游没给响应头");
            return Err(rt.defaults.all_upstreams_down);
        };
        let mut dresp = uresp.clone();
        let status = dresp.status.as_u16();

        // ── ★ ★ 重验证命中：304 ⇒ **只更新元数据，不动 body**（G83 那条理由的内存版）
        //
        // ⚠ ⚠ 这里回给客户端的是**缓存里那份完整响应**，不是上游这个 304 ——
        //   304 是给我们看的，不是给客户端看的。★ 直接把 304 转下去，
        //   客户端会拿到一个「没变」的答复，而它手上**根本没有那份内容**。
        if status == 304
            && let Some(cp) = &pass
            && let Some(entry) = cp.revalidate.clone()
        {
            let fresh_for = cache_freshness_of(&uresp.headers, cp.cfg, now_unix());
            let cc = cache_cc_of(&uresp.headers);
            cp.handle
                .store
                .refresh(&cp.primary, &cp.secondary, fresh_for, now_unix(), cc);
            debug!("缓存重验证命中 304 —— 只更新元数据（{}）", cp.primary);
            self.connector
                .release_http_session(upstream, &peer, None)
                .await;
            return write_cached(
                session,
                &entry,
                now_unix(),
                cache::CacheState::Revalidated,
                &cp.handle.store,
            )
            .await;
        }

        // ── 缓存事件：回源（R7 的 `miss`）──────────────────────────────────
        //
        // ★ ★ **这一行是「这条响应的内容来自上游」唯一的判定点**，而那正是 `miss`
        //   的定义。上面那个 304 分支已经把「重验证成功、发出去的还是缓存里那一份」
        //   摘走了（它在 `write_cached` 里记 `stale`）⇒ 走到这里的每一条都在往下游
        //   发上游给的字节，无论它此前是没命中还是重验证没验住。
        // ⚠ 记在这里而不是「查缓存没命中」那一处：那一处之后还可能拐弯 ——
        //   `only-if-cached` 回 504（根本没回源）、防惊群的 follower 等完重查一次
        //   **命中了**。在那里记，两种都会被算成 `miss`，而**数字看起来完全正常**。
        // ⚠ `pass` 是 `None` 说明这个站点根本没配 `cache`：它没有「命中/未命中」
        //   可言，一条都不记。★ 记了的话，`hit/(hit+miss)` 会被没开缓存的流量
        //   稀释成一个假比例 —— 而那个比例读起来同样完全正常。
        if pass.is_some() {
            cache::CacheEvent::Miss.record(1);
        }

        // ★ 存进缓存的是**改完头之后**那一份（`header_down` 已经施加）——
        //   ⚠ 存改头之前那份的话，命中时那些头会**消失**：一条 `header_down`
        //   在未命中时生效、命中时不生效，而两次请求的配置一模一样。
        //   ⇒ 两份快照都留着：`uresp_snapshot` 判可缓存性（那是上游说的话），
        //   `dresp_snapshot` 是要存下来的内容。
        let uresp_snapshot = uresp.clone();
        {
            // ★ 先算好取值再改头：`{resp_header.X}` 说的是**上游给的**那份，
            //   而不是我们改到一半的那份。借用规则恰好把这件事逼成显式的。
            let snapshot = dresp.clone();
            let snap_headers = RespHeaders(&snapshot);
            let rc = ResponseCtx {
                status: Some(status),
                upstream: Some(&up.addr),
                headers: Some(&snap_headers),
            };
            let mut pairs = Vec::new();
            collect_ops(
                &target.header_down,
                ctx,
                &rc,
                &routed.captures,
                started,
                &mut pairs,
            );
            collect_ops(
                routed.response_headers.iter().copied(),
                ctx,
                &rc,
                &routed.captures,
                started,
                &mut pairs,
            );
            apply_pairs(&mut dresp, &pairs);
        }
        // ── ★ ★ 压缩（**M2 批 I**，G99–G102）────────────────────────────────
        //
        // 它的位置是判据的一部分，两侧都不能挪：
        // · 排在 `header_down` **之后** —— 用户改的头要被压缩层看见
        //   （`Content-Type` 决定压不压、`Content-Encoding` 决定要不要放过）；
        // · 排在缓存捕获**之前** —— G101 拍的是「压完再存」，
        //   ⇒ 缓存里那份就是压缩后的字节，命中时直接发、不必再压一遍。
        // ⚠ `status_has_no_body`：没有体的响应（204/304）不许被加上 `chunked`。
        let mut encoder = encode::Encoder::new(encode::wanted(routed), downstream_req);
        if let Some(enc) = &mut encoder {
            enc.header_filter(&mut dresp, encode::status_has_no_body(status));
        }
        let dresp_snapshot = dresp.clone();
        if let Err(e) = session.write_response_header(Box::new(dresp)).await {
            debug!("写下游响应头失败：{e}");
            return Ok(()); // 下游没了，不是上游的错
        }

        // ── 响应体 ────────────────────────────────────────────────────────
        //
        // ★ ★ **边发边捕获**，而不是先全收下来再发：一个正在下载 2GB 的客户端
        //   不该等我们把 2GB 收完。⚠ 捕获只到 `max_size` 为止 ——
        //   超过就**放弃缓存**并继续照发，而不是把内存吃光。
        let mut captured: Option<Vec<u8>> = pass.as_ref().map(|_| Vec::new());
        let cap_limit = pass.as_ref().map(|p| p.cfg.max_size_bytes).unwrap_or(0);
        loop {
            match upstream.read_response_body().await {
                Ok(Some(chunk)) => {
                    // ★ ★ 压缩排在捕获之前（G101：压完再存）。
                    //   ⚠ 压缩层会**攒**数据：一块进去可能什么都不出来，
                    //   而**空块不能写下去** —— HTTP/1.1 的分块编码里
                    //   一个零长块就是**体结束**，写下去等于把响应截断在这里。
                    let out = match &mut encoder {
                        Some(e) => e.body_filter(Some(&chunk), false),
                        None => None,
                    };
                    let chunk = out.unwrap_or(chunk);
                    if chunk.is_empty() {
                        continue;
                    }
                    if let Some(buf) = &mut captured {
                        if buf.len() as u64 + chunk.len() as u64 <= cap_limit {
                            buf.extend_from_slice(&chunk);
                        } else {
                            // ⚠ 一超上限就**整条放弃**，而不是存半截。
                            //   ★ 存半截 = 此后每个命中都拿到一个被截断的响应，
                            //   而 `Content-Length` 还是原来那个 —— 客户端会挂在那儿等。
                            debug!("响应超过 max_size，本条不缓存");
                            captured = None;
                        }
                    }
                    if let Err(e) = session.write_response_body(chunk, false).await {
                        debug!("写下游响应体失败：{e}");
                        return Ok(());
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    error!("读上游响应体失败：{e}");
                    // 头已经发出去了，改不了状态码——只能断开。
                    // ⚠ 这时**不许存**：我们手上是个不完整的响应。
                    return Ok(());
                }
            }
        }
        // ★ ★ 压缩的收尾：压缩层手上还攒着最后一段（gzip 的 footer、brotli 的收尾块）。
        //   ⚠ 漏了这一步，客户端拿到的是一个**少了尾巴**的压缩流 —— 而
        //   `Content-Length` 已经被压缩层去掉了，所以它不会「长度对不上」，
        //   只会在解压到最后一刻报「意外的流结束」。★ 缓存里那份同样会少尾巴，
        //   于是**每一次命中都重现同一个坏响应**。
        if let Some(enc) = &mut encoder
            && let Some(tail) = enc.body_filter(None, true)
            && !tail.is_empty()
        {
            if let Some(buf) = &mut captured {
                if buf.len() as u64 + tail.len() as u64 <= cap_limit {
                    buf.extend_from_slice(&tail);
                } else {
                    captured = None;
                }
            }
            if let Err(e) = session.write_response_body(tail, false).await {
                debug!("写下游压缩收尾失败：{e}");
                return Ok(());
            }
        }
        let _ = session.write_response_body(Bytes::new(), true).await;

        // ── 存进缓存 ──────────────────────────────────────────────────────
        if let (Some(cp), Some(body)) = (pass.take(), captured) {
            store_if_allowed(
                &cp,
                &uresp_snapshot,
                &dresp_snapshot,
                body,
                ctx,
                downstream_req,
            );
        }

        // ★ 归还连接到池子里。失败不影响本次请求，但要说出来。
        self.connector
            .release_http_session(upstream, &peer, None)
            .await;
        Ok(())
    }
}

/// 一次「带缓存的转发」要带着的东西。
struct CachePass<'a> {
    cfg: &'a CacheRt,
    handle: &'a cache::CacheHandle,
    primary: String,
    /// 重验证时是那条已有条目的次级键；未命中时先空着，存的时候按响应的 `Vary` 算。
    secondary: String,
    /// 非 `None` = 这次是**重验证**，缓存里已经有一条。
    revalidate: Option<Box<cache::store::Entry>>,
    req_cc: cache::cc::RequestCc,
    /// 这条链上 `encode` 要求的算法（**M2 批 I**）。
    ///
    /// ⚠ ⚠ 它进这个结构**只为一件事**：存的时候算次级键要用它归一化
    /// `Accept-Encoding`（G101）—— 而查的时候用的必须是**同一个**列表，
    /// 否则存与查落在两个键上，缓存永远不命中。
    /// ★ 带进来而不是在 `store_if_allowed` 里从 `Routed` 现取，是因为那里拿不到 `Routed`。
    encodings: Vec<String>,
}

/// 查缓存的三种结果。
enum CacheAttempt {
    /// 已经发完了。
    Served(Result<(), u16>),
    /// 缓存里有，但要先问上游。
    ///
    /// ⚠ `entry` 装了箱，理由与 `store::Lookup::Hit` 一样：这个枚举在
    /// **每一次未命中时也要被构造**，而条目本身比别的变体大一个数量级。
    Revalidate {
        secondary: String,
        entry: Box<cache::store::Entry>,
    },
    Miss,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn header_str<'a>(req: &'a RequestHeader, name: &str) -> Option<&'a str> {
    req.headers.get(name).and_then(|v| v.to_str().ok())
}

/// 算缓存次级键时用的「取一个请求头」——**只有 `Accept-Encoding` 被归一化**（**G101**）。
///
/// G101 拍了「压完再存」⇒ 缓存里那份是压过的，`Vary: Accept-Encoding` 让缓存按这个头分身。
/// ⚠ 浏览器发的写法与顺序千奇百怪 —— 拿原值当次级键，同一条 URL 会存出几十份
/// **内容完全相同**的条目。
///
/// ⚠ ⚠ **「查」和「存」两处必须用同一个函数** —— 正确性就建立在这一点上。
/// ★ 它不改动 `key::secondary` 的签名，于是两个后端一个字都不用动。
fn cache_key_header<'a>(
    req: &'a RequestHeader,
    encodings: &'a [String],
) -> impl FnMut(&str) -> Option<&'a str> + 'a {
    move |name: &str| {
        if name.eq_ignore_ascii_case("accept-encoding") {
            // ★ 返回的是 `&'static str`，所以这里没有生命周期上的麻烦。
            Some(encode::preferred_encoding(header_str(req, name), encodings))
        } else {
            header_str(req, name)
        }
    }
}

fn resp_header_str<'a>(h: &'a http::HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn cache_cc_of(h: &http::HeaderMap) -> cache::cc::ResponseCc {
    cache::cc::parse_response(resp_header_str(h, "cache-control").unwrap_or(""))
}

/// 这次响应的新鲜期（秒）。★ `ttl` 是**兜底**（G96）—— 走到最后一步才用它。
fn cache_freshness_of(h: &http::HeaderMap, cfg: &CacheRt, now: i64) -> u64 {
    let cc = cache_cc_of(h);
    if let Some(n) = cc.shared_max_age() {
        return n;
    }
    if let Some(exp) = resp_header_str(h, "expires").and_then(cache::files_httpdate) {
        let date = resp_header_str(h, "date")
            .and_then(cache::files_httpdate)
            .unwrap_or(now);
        return (exp - date).max(0) as u64;
    }
    cfg.ttl_ms.map(|ms| ms / 1000).unwrap_or(0)
}

/// 把缓存里那条发给客户端。
///
/// ⚠ ⚠ **`Age` 头是必须的**（RFC 9111 §5.1）：下游可能还有一层缓存，
/// 它要拿 `Age` 去算自己的新鲜度。★ 不发的话，一条已经在我们这儿放了 59 分钟的响应，
/// 在下一层看起来是**刚出炉的** —— 于是同一份内容会被一路放大到两倍寿命。
async fn write_cached(
    session: &mut Downstream<'_>,
    entry: &cache::store::Entry,
    now: i64,
    state: cache::CacheState,
    store: &cache::Backend,
) -> Result<(), u16> {
    let age = (now - entry.stored_at).max(0);
    let mut resp = match ResponseHeader::build(entry.status, None) {
        Ok(r) => r,
        Err(e) => {
            error!("缓存命中却建不出响应头：{e}");
            return Err(500);
        }
    };
    for (k, v) in &entry.headers {
        let _ = resp.append_header(k.clone(), v);
    }
    let _ = resp.insert_header("Age", age.to_string());
    // ★ 让「这一条是从哪来的」在**响应里**看得见，而不是只在日志里。
    //   ⚠ 判据也靠它：只看状态码与体，命中与回源长得一模一样。
    // ⚠ 后端后缀（`-DISK`）由 `store` 加 —— 它知道本进程挑中了哪个后端。
    let header = store.state(state);
    let _ = resp.insert_header("X-Fulcrum-Cache", header.as_str());
    // ★ ★ ★ 响应头、访问日志那一格、指标那一格**三者取同一个 `state`，在同一处赋** ——
    //   分几处各算一遍的话，它们哪天不一致，每一处读起来都很正常（G66 的形状）。
    //   ⚠ 回源那条路**没有**这个头，所以日志里也没有 `cache` 这一格（契约里写死的）；
    //     它的 `miss` 记在 `proxy_with` 里回源那一处。
    //   ⚠ 指标那一格比响应头**粗**：`HIT` 与 `HIT-DISK` 折成同一个 `hit`（R7）。
    session.record.cache = Some(header);
    state.event().record(1);
    let _ = resp.insert_header("Content-Length", entry.body.len().to_string());
    if let Err(e) = session.write_response_header(Box::new(resp)).await {
        debug!("写缓存响应头失败：{e}");
        return Ok(());
    }
    let _ = session
        .write_response_body(Bytes::from(entry.body.clone()), true)
        .await;
    Ok(())
}

/// 逐跳头：**不许存**，也不许原样转发（RFC 9110 §7.6.1）。
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// 判一次响应能不能存，能就存。
fn store_if_allowed(
    cp: &CachePass<'_>,
    uresp: &ResponseHeader,
    dresp: &ResponseHeader,
    body: Vec<u8>,
    ctx: &RequestCtx<'_>,
    req: &RequestHeader,
) {
    let now = now_unix();
    let cc = cache_cc_of(&uresp.headers);
    // ★ ★ ★ **可缓存性看上游那份（`uresp`），次级键看我们发出去的那份（`dresp`）。**
    //   `Vary: Accept-Encoding` 是**压缩层加在 `dresp` 上的**，`uresp` 里一个字没有 ⇒
    //   从 `uresp` 取会得到空 vary ⇒ 次级键恒空 ⇒ 所有客户端共用同一条，而那条是压过的
    //   ⇒ **一个从没说过自己认 gzip 的客户端会收到 gzip 的字节**。
    //   ⚠ 现场是「有些客户端看到乱码」，而两边的头都完全正常。
    // ⇒ **承诺是谁做的，键就跟谁走。**
    // ★ 而 `vary_star`（`Vary: *`）仍看 `uresp`：那是可缓存性判断，属于上游说的话。
    let vary_raw = resp_header_str(&dresp.headers, "vary").unwrap_or("");
    let upstream_vary = resp_header_str(&uresp.headers, "vary").unwrap_or("");
    let input = cache::policy::StoreInput {
        method: ctx.method,
        status: uresp.status.as_u16(),
        has_authorization: header_str(req, "authorization").is_some(),
        resp_cc: &cc,
        req_cc: &cp.req_cc,
        expires: resp_header_str(&uresp.headers, "expires").and_then(cache::files_httpdate),
        date: resp_header_str(&uresp.headers, "date")
            .and_then(cache::files_httpdate)
            .unwrap_or(now),
        body_len: Some(body.len() as u64),
        max_size: cp.cfg.max_size_bytes,
        default_ttl_secs: cp.cfg.ttl_ms.map(|ms| ms / 1000),
        // ★ 这一条看**上游**那份（理由见上面 `upstream_vary` 那段）。
        vary_star: cache::key::is_vary_star(upstream_vary),
        has_set_cookie: uresp.headers.get("set-cookie").is_some(),
    };
    let fresh_for = match cache::policy::is_storable(&input) {
        cache::policy::Storable::Yes { fresh_for_secs } => fresh_for_secs,
        cache::policy::Storable::No(why) => {
            debug!("不缓存（{why}）：{}", cp.primary);
            return;
        }
    };

    let vary = cache::key::parse_vary(vary_raw);
    let secondary = cache::key::secondary(&vary, cache_key_header(req, &cp.encodings));

    // ★ ★ 存的是**改完头之后**那一份（`header_down` 已经施加），但要剥两类头：
    //   ① 逐跳头 —— 它们属于**这一条连接**，存下来发给别人是错的；
    //   ② `no-cache="字段名"` 与 `private="字段名"` 点名的那几个 —— RFC 9111 §5.2.2
    //      说得很明确，共享缓存不许把它们发给别人。
    //   ⚠ ⚠ ② 这一条最容易漏：`no-cache="Set-Cookie"` 的意思正是
    //   「你可以存这个响应，但**别把那个头转给下一个人**」。漏了它就是串号。
    let mut headers: Vec<(String, String)> = Vec::new();
    for (k, v) in dresp.headers.iter() {
        let name = k.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        if cc.no_cache_fields.contains(&name) || cc.private_fields.contains(&name) {
            continue;
        }
        // `Content-Length` 由 `write_cached` 自己按体长重算 —— 存下来会打架。
        if name == "content-length" || name == "age" {
            continue;
        }
        if let Ok(sv) = v.to_str() {
            headers.push((k.as_str().to_string(), sv.to_string()));
        }
    }

    let entry = cache::store::Entry {
        status: uresp.status.as_u16(),
        headers,
        body,
        stored_at: now,
        fresh_for,
        cc: cc.clone(),
        etag: resp_header_str(&uresp.headers, "etag").map(str::to_string),
        last_modified: resp_header_str(&uresp.headers, "last-modified").map(str::to_string),
        vary,
    };
    debug!("存进缓存（新鲜 {fresh_for}s）：{}", cp.primary);
    cp.handle.store.put(&cp.primary, secondary, entry);
}

/// 取 Host：h1 看 `Host` 头，h2 看 `:authority`（Pingora 会把它填进 uri）。
fn host_of(req: &RequestHeader) -> String {
    if let Some(v) = req.headers.get("host")
        && let Ok(s) = v.to_str()
        && !s.is_empty()
    {
        return s.to_string();
    }
    req.uri.host().unwrap_or("").to_string()
}

/// ★ ★ 客户端 IP 与端口 = **socket 对端**。不看 `X-Forwarded-For`（客户端可伪造）。
///
/// ★ Unix socket 上没有 IP：返回 `None` 而不是编一个 `127.0.0.1`——
/// `remote_ip` 匹配器在取不到 IP 时判**不命中**，那是安全的一侧。
/// 把一个 [`Outcome`] 翻成访问日志契约里那个**闭集**的取值。
///
/// ★ **穷尽 `match` 是判据**：契约写着 `outcome` 是闭集，而「闭集」要有东西守着才算数
/// ⇒ 给 [`Outcome`] 加一种终结方式时**这里编不过**。若改成在每个分支各赋一次值，
/// 加一种就只是少一个赋值，不会有任何报错 —— 日志里会静静出现一个 `aborted`。
fn outcome_name(o: &Outcome<'_>) -> &'static str {
    match o {
        Outcome::Respond { .. } => "respond",
        Outcome::Redirect { .. } => "redir",
        Outcome::Proxy(_) => "reverse_proxy",
        Outcome::FileServer(_) => "file_server",
        // ★ 闭集的**第 8 个值**（M2 批 M，G116）。
        Outcome::Metrics => "metrics",
        Outcome::NoRouteMatch => "error",
    }
}

fn client_addr(session: &ServerSession) -> (Option<IpAddr>, u16) {
    match session.client_addr().and_then(|a| a.as_inet()) {
        Some(a) => (Some(a.ip()), a.port()),
        None => (None, 0),
    }
}

/// 这条连接的 TLS 四格（**M2 批 L 第 ③ 步**；**改成单一来源**）。
/// `None` = 它不是 TLS。
///
/// ★ **四格全部来自同一个 `SslDigest`**，h1/h2 与 h3 走同一段代码 —— 差别只在
/// 谁造了那份 digest（h1/h2 是上游的 `from_ssl()`，h3 是 [`crate::quic::h3_session`]）。
/// ⇒ 「同一格数据两个填法」在这一层做不到。
///
/// ⚠ **h3 上 `cipher` 是空的** —— quiche 的 `Handshake::cipher()` 锁在私有 `mod tls` 里。
/// 空串在 [`access_log::TlsFields`] 里就是「这一格不出现」。
fn tls_fields(session: &ServerSession) -> Option<access_log::TlsFields> {
    let ssl = session.digest()?.ssl_digest.as_ref()?;
    Some(access_log::TlsFields {
        version: ssl.version.to_string(),
        cipher: ssl.cipher.to_string(),
        // ★ fork 改动 14：上游的 `SslDigest` 本来没有这两格。
        sni: ssl.sni.clone(),
        alpn: ssl.alpn.clone(),
    })
}

/// 只带状态码与上游地址的响应上下文。
///
/// ⚠ **不在这里塞响应头**：`ResponseCtx.headers` 要一个 `&dyn Headers`，
/// 而它必须借用一个活着的 `RespHeaders`。⚠ 不要用 `Box::leak` 绕过它 ——
/// 那是**每个请求泄漏一次**，而一个「能编过、跑得动、内存慢慢涨」的写法
/// 正是最难在测试里看见的那种。需要响应头的地方在栈上自己建。
fn resp_ctx<'a>(status: u16, upstream: Option<&'a str>) -> ResponseCtx<'a> {
    ResponseCtx {
        status: Some(status),
        upstream,
        headers: None,
    }
}

/// 把一串头操作展开成 `(名字, 取值)`。名字前缀 `+` = 追加，`-` = 删除。
///
/// ★ 泛型接 `&HeaderOpRt` 的迭代器，于是 `&[HeaderOpRt]` 与 `&[&HeaderOpRt]`
/// 两种形状都能直接喂进来，不必为了统一类型先 clone 一份。
fn collect_ops<'o>(
    ops: impl IntoIterator<Item = &'o HeaderOpRt>,
    ctx: &RequestCtx<'_>,
    rc: &ResponseCtx<'_>,
    caps: &[String],
    now: SystemTime,
    out: &mut Vec<(String, String)>,
) {
    for op in ops {
        match op.op.as_str() {
            "remove" => out.push((format!("-{}", op.name), String::new())),
            other => {
                let v = op
                    .value
                    .as_ref()
                    .map(|t| t.expand(ctx, rc, caps, now))
                    .unwrap_or_default();
                let prefix = if other == "add" { "+" } else { "" };
                out.push((format!("{prefix}{}", op.name), v));
            }
        }
    }
}

/// 把 `(名字, 取值)` 施加到响应头上。
fn apply_pairs(resp: &mut ResponseHeader, pairs: &[(String, String)]) {
    for (name, value) in pairs {
        if let Some(n) = name.strip_prefix('-') {
            resp.remove_header(n);
        } else if let Some(n) = name.strip_prefix('+') {
            let _ = resp.append_header(n.to_string(), value);
        } else {
            let _ = resp.insert_header(name.to_string(), value);
        }
    }
}

fn apply_ops_to_request(
    req: &mut RequestHeader,
    ops: &[HeaderOpRt],
    ctx: &RequestCtx<'_>,
    caps: &[String],
    now: SystemTime,
) {
    let rc = ResponseCtx::default();
    for op in ops {
        match op.op.as_str() {
            "remove" => {
                req.remove_header(&op.name);
            }
            other => {
                let v = op
                    .value
                    .as_ref()
                    .map(|t| t.expand(ctx, &rc, caps, now))
                    .unwrap_or_default();
                if other == "add" {
                    let _ = req.append_header(op.name.clone(), &v);
                } else {
                    let _ = req.insert_header(op.name.clone(), &v);
                }
            }
        }
    }
}

async fn write_simple(
    session: &mut Downstream<'_>,
    status: u16,
    body: Option<String>,
    ops: &[&HeaderOpRt],
    ctx: &RequestCtx<'_>,
    started: SystemTime,
) {
    let mut extra = Vec::new();
    let rc = resp_ctx(status, None);
    collect_ops(ops.iter().copied(), ctx, &rc, &[], started, &mut extra);
    write_with_headers(session, status, body, extra).await;
}

async fn write_with_headers(
    session: &mut Downstream<'_>,
    status: u16,
    body: Option<String>,
    extra: Vec<(String, String)>,
) {
    let mut resp = match ResponseHeader::build(status, None) {
        Ok(r) => r,
        Err(e) => {
            error!("构不出响应头（status={status}）：{e}");
            return;
        }
    };
    let bytes = body.map(Bytes::from);
    let len = bytes.as_ref().map(|b| b.len()).unwrap_or(0);
    let _ = resp.insert_header("Content-Length", len.to_string());
    if bytes.is_some() {
        let _ = resp.insert_header("Content-Type", "text/plain; charset=utf-8");
    }
    apply_pairs(&mut resp, &extra);
    if let Err(e) = session.write_response_header(Box::new(resp)).await {
        debug!("写响应头失败：{e}");
        return;
    }
    match bytes {
        Some(b) => {
            let _ = session.write_response_body(b, true).await;
        }
        None => {
            let _ = session.write_response_body(Bytes::new(), true).await;
        }
    }
}

/// 把装载结论打出来：回落路由（G52）与未接线能力（[`fulcrum_runtime::UNWIRED`]）。
///
/// ★ 装载时说清楚，而不是等出事再查——回落是真实的性能与运维成本，
/// 未接线的能力是真实的功能缺口，两者都不该需要另外去问才知道。
pub fn log_load_summary(
    cfg: &fulcrum_config::StructuredConfig,
    rt: &Runtime,
    cache: &cache::CacheHandle,
) {
    // ★ ★ ★ **自研静态文件：把生效的 hide 清单逐字打出来**（G88 的「可见性」那一格，M2 批 F）。
    //
    // ⚠ ⚠ 默认表**非空**（`.git` `.env` `.svn` `.hg` `.bzr` `.DS_Store`），
    //   而一个不说出来的非空默认就是一次**静默行为** —— 现场是「我的文件明明在，
    //   却一直 404」，而配置文件里一个字都看不出来。批 20 为「静默失能」专门堵过一次。
    // ★ 打的是**运行时手里那一份**（`rt.file_servers()`），不是照配置再算一遍：
    //   两份各算各的时，日志说的和服务器做的可以不是一回事。
    for (site, fs) in rt.file_servers() {
        info!(
            "静态文件：站点 {site} root={} index={} browse={} follow_symlinks={}",
            fs.root,
            fs.index.join(","),
            fs.browse,
            fs.follow_symlinks,
        );
        info!(
            "  hide（生效的完整清单，按路径段匹配、命中回 404）：{}",
            fs.hide.join(" ")
        );
        // ★ 预压缩旁文件（M2 批 I）：**配了才说**，没配时不打这一行 ——
        //   一行「precompressed：（无）」对每一个静态站点都印一遍，就是噪音。
        if !fs.precompressed.is_empty() {
            info!(
                "  预压缩旁文件：{}（发 /x.css 时先找 /x.css.{{gz,br,zst}}；                 ⚠ 旁文件比原文件旧就当它不存在）",
                fs.precompressed.join(" ")
            );
        }
    }
    // ★ ★ ★ **自研压缩：把生效的算法打出来**（M2 批 I）。
    //
    // ⚠ ⚠ `encode` 从 M1 起就写得下、而运行时一步都不做（它在 `UNWIRED` 里躺了
    //   整整一段）。⇒ 它**刚刚开始真的生效**这件事必须说出来 ——
    //   一个从旧版本升上来的站点，行为在这一刻变了，而配置一个字都没改。
    // ★ 打的是**运行时手里那一份**，不是照配置再算一遍。
    for (site, algos) in rt.encodings() {
        info!(
            "压缩：站点 {site} encode={}（按客户端 `Accept-Encoding` 的**首选**挑；             ⚠ 被压的响应没有 `Content-Length` 与 Range，强 ETag 会被弱化成 W/\"…\"）",
            algos.join(" ")
        );
    }
    // ★ ★ ★ **自研缓存：把生效的设置打出来**（G96 的可见性，M2 批 G）。
    //
    // ⚠ ⚠ `ttl` 是**兜底**不是覆盖 —— 而从 nginx `proxy_cache_valid` 迁过来的人
    //   默认会以为它是覆盖。★ 一个与用户预期相反的默认，如果不说出来，
    //   现场是「我明明配了 5 分钟，怎么 30 秒就变了」，而配置里一个字都看不出问题。
    // ⚠ 这几行由 `tests/cache/run.sh` 的两条装载日志断言守着 —— 一个「写好了却没人调」
    //   的摘要函数与不存在的摘要函数，在日志上长得一模一样。
    for (site, c) in rt.cache_settings() {
        info!(
            "缓存：站点 {site} ttl={} max_size={}B capacity={}B",
            match c.ttl_ms {
                Some(ms) => format!("{}s（**兜底** —— 上游给了新鲜度就听上游的）", ms / 1000),
                None => "（未配 ⇒ 上游没给新鲜度就不缓存）".to_string(),
            },
            c.max_size_bytes,
            c.capacity_bytes,
        );
        // ★ ★ ★ **后端说的是「真的挑中了哪一个」，不是配置里写了什么。**
        //   ⚠ 两句话在一种情况下恰好不一样：配了 `disk` 而那个目录用不了 ——
        //   那时缓存是**关掉**的（照常转发，只是不再有命中），而这一行是
        //   运维在启动那一刻唯一看得见它的地方。
        //   ★ 第二个信号在运行时：`X-Fulcrum-Cache` 那个头不再出现。
        match &cache.store {
            cache::Backend::Mem(s) => info!(
                "  内存后端（没写 `disk`）；生效容量 {}B；只裹 reverse_proxy，别处不生效",
                s.capacity()
            ),
            cache::Backend::Disk(s) => info!(
                "  磁盘后端 {}（两级分片 · meta/body 两文件 · 启动不扫盘，G83/G84）；\
                 生效容量 {}B；只裹 reverse_proxy，别处不生效",
                s.root().display(),
                s.capacity()
            ),
            cache::Backend::Off => warn!(
                "  缓存后端：已关闭 —— 配了 `disk {}` 而那个目录用不了\
                 （上面那行 error 说了为什么）；请求照常转发，只是不再有命中",
                c.disk_dir.as_deref().unwrap_or("?")
            ),
        }
    }
    // ★ ★ 反向那一半：配了 `cache` 却没有 `reverse_proxy` 可裹 ⇒ **说出来**。
    //   ⚠ 一条不生效的 `cache` 与一条正在生效的，在配置文件里长得一模一样 ——
    //   与回落层那条「死配置」是同一个形状（那一层没了，教训留下）。
    if !rt.cache_settings().is_empty() && rt.all_upstreams().is_empty() {
        warn!("★ 配了 `cache`，而这份配置里**没有任何 reverse_proxy** —— 缓存不会生效");
    }
    for (k, why) in rt.unwired_in_use(cfg) {
        warn!("⏳ `{k}` 这一批还没接线：{why}");
    }
    // ⚠ 这里原来有一行「端口上有 https 站点，而 TLS 还没接线，会以明文 HTTP 监听」。
    //   TLS 接上之后它就成了**假警告**，而假警告比没有警告更糟：它训练人忽略警告，
    //   连带把真的那几条一起埋掉。TLS 的现状由 `tls::log_tls_notes` 说，
    //   它说的是「哪些站点还缺证书」——那是真的还缺的东西。
}

/// 展开一个模板（给外部调用方用，例如 CLI 的自检）。
pub fn expand_for_test(t: &Template, ctx: &RequestCtx<'_>) -> String {
    t.expand(ctx, &ResponseCtx::default(), &[], SystemTime::now())
}

// ── 装配与运行 ──────────────────────────────────────────────────────────────

/// 起服务需要的、配置层管不到的那几项。
///
/// ★ 它们属于**进程模型**（G13/G31/G33/G37），而不是站点配置：
/// pid 文件与升级 socket 的路径由 systemd 托管、可覆盖，
/// 所以它们从命令行进来，不进 DSL。
pub struct ServeOptions {
    /// 监听地址的主机部分。默认 `0.0.0.0`。
    pub bind_host: String,
    pub pid_file: String,
    pub upgrade_sock: String,
    /// 状态目录（G33 的 `StateDirectory=`）。证书存在 `<state>/certs/` 下面。
    pub state_dir: String,
    /// 是否从正在跑的旧世代接管（Pingora 的 `-u`）。
    pub upgrade: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        // ★ 默认值照 G33：systemd 托管为主，全部可覆盖。
        ServeOptions {
            bind_host: "0.0.0.0".to_string(),
            pid_file: "/run/fulcrum/fulcrum.pid".to_string(),
            upgrade_sock: "/run/fulcrum/upgrade.sock".to_string(),
            state_dir: "/var/lib/fulcrum".to_string(),
            upgrade: false,
        }
    }
}

/// 起 Pingora，把每个监听端口挂上一个 [`FulcrumApp`]，然后**不返回**。
///
/// ⚠ **每个端口一个 service**：站点索引按 `(Host, 本地端口)` 两维查，
/// 而会话本身问不出「我落在哪个监听器上」。一个端口一个 app 实例是唯一不靠猜的办法。
pub fn serve(cfg: &fulcrum_config::StructuredConfig, rt: Arc<Runtime>, opts: ServeOptions) -> ! {
    use pingora_core::server::Server;
    use pingora_core::server::configuration::Opt;
    use pingora_core::services::listening::Service as ListeningService;

    // ★ 造 `ServerConf` 的活儿在 `process::build_server_conf` 里，因为**它有门**：
    //   `serve()` 返回 `!`、还要绑端口，单测碰不了它，而「排空窗口不许留 None」
    //   这类判据恰恰最需要门。见 process.rs 顶部那张四条缺口表。
    let conf = process::build_server_conf(cfg, &opts);
    let shutdown_budget = process::shutdown_budget_secs(&conf);

    let opt = Opt {
        upgrade: opts.upgrade,
        ..Default::default()
    };
    let mut server = Server::new_with_opt_and_conf(opt, conf);
    server.bootstrap();

    // ★ ★ 换代触发器要在 `run_forever()` **之前**装上：否则从「服务起来」到
    //   「run() 建好 runtime」之间的那一小段时间里，`systemctl reload` 会丢。
    //   ⚠ 更要紧的是——不装它的话 SIGUSR2 走默认动作，即**终止进程**：
    //   一次 `systemctl reload` 会把服务打死。
    process::spawn_upgrade_trigger();

    // ★ **缓存实例只建一次，全部监听端口共用一份**：每端口一份的话，同一条 URL
    //   在 :80 与 :443 上会各存一份，而容量上限也会变成配置值的 N 倍。
    // ⚠ 多个站点各写各的 `capacity` 时取**最大**：取最小会让写了大容量的站点
    //   悄悄拿不到空间，而那件事没有任何东西会说。装载日志打出生效值。
    // ★ `disk` 走另一条路 —— 多个 `cache` 写不同目录是**编译期错误**
    //   （`FUL-DSL-0035`）。两者处置不同是因为代价不同：容量取大一点不伤谁，
    //   而目录取一个会让另一个站点的缓存整个落在别处。
    let cache_capacity = rt
        .cache_settings()
        .iter()
        .map(|(_, c)| c.capacity_bytes)
        .max()
        .unwrap_or(fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES);
    let cache_dir = rt
        .cache_settings()
        .iter()
        .find_map(|(_, c)| c.disk_dir.clone());
    // ⚠ ⚠ **它必须建在 `log_load_summary` 之前**：装载日志要说的是
    //   「真的挑中了哪个后端」，而不是「配置里写了什么」。★ 目录用不了时这两句话
    //   恰好不一样 —— 而那正是最需要说出来的那一刻。
    let cache = cache::CacheHandle::new(cache::Backend::open(cache_dir.as_deref(), cache_capacity));

    log_load_summary(cfg, &rt, &cache);

    // ★ 把停机预算打出来，因为 `TimeoutStopSec` 必须按它设，而它由配置决定。
    //   ⚠ 不打的话，这个数只存在于源码里，运维只能靠猜或靠翻文档。
    info!(
        "停机预算约 {shutdown_budget}s（排空 + 收尾）—— systemd unit 的 TimeoutStopSec 要大于它；\
         换代时老一代另需最多 5s 送 fd"
    );

    // ── TLS：按 SNI 挑证书（§5.1 第 1 条锁死的那条路）────────────────────
    let cert_root = std::path::Path::new(&opts.state_dir).join("certs");
    let issuer = fulcrum_acme::issuer_slug(
        cfg.global
            .acme_ca
            .as_deref()
            .unwrap_or(fulcrum_acme::LETSENCRYPT_PRODUCTION),
    );
    let plan = match tls::plan_tls(&rt, &cert_root, &issuer, cfg.global.default_sni.as_deref()) {
        Ok(p) => p,
        Err(errs) => {
            // ⚠ 走到这里说明配置里显式给的 PEM 读不出来。**不许降级成告警**：
            //   一个「起来了但那个站点永远握手失败」的进程比起不来更难查。
            for e in &errs {
                error!("TLS 装载失败：{e}");
            }
            std::process::exit(1);
        }
    };
    tls::log_tls_notes(&plan, &format!("证书存储 {}", cert_root.display()));

    // ── 访问日志：**装载时就把每个日志文件打开一次**（M2 批 L 第 ② 步）────────
    //
    // ⚠ ⚠ 与上面 TLS 那条逐字同一个理由，而这里更刺眼：一个打不开的日志文件
    //   若拖到第一个请求才发现，现场是**服务完全正常、而日志一行都没有** ——
    //   ★ ★ ★ 一个用来「出了事你能知道」的东西，自己坏掉时没人知道。
    //   ⇒ 硬错误，不许降级成告警。
    match access_log::open_all(&rt) {
        Ok(0) => {}
        Ok(n) => info!("访问日志：打开了 {n} 个日志文件"),
        Err(errs) => {
            for e in &errs {
                error!("访问日志装载失败：{e}");
            }
            std::process::exit(1);
        }
    }

    // ── ACME：自动签发与续期（G53/G54/G56）────────────────────────────────
    //
    // ★ 巡检跑在**后台服务**里，也就是监听器起来之后——HTTP-01 要 CA 能连上来，
    //   在监听器之前签必然失败一次，还白耗一次速率配额。
    let http01 = Arc::new(fulcrum_acme::Http01Store::new());
    let acme = acme::build(
        cfg,
        &rt,
        plan.resolver.clone(),
        http01.clone(),
        &opts.state_dir,
    );

    // ★ 监听端口从**建起来的那一份**取。⚠ 换配置换不了监听端口——
    //   Pingora 在启动时绑定，端口集变了只能走 `systemctl reload`（真的换代）。
    //   全量 load 那条路会显式拒绝这种配置，理由写在 admin 模块里。
    let listen_ports = rt.listen_ports.clone();
    // 管理面要拿它当「端口集有没有变」的判据（全量 load 那条路）。
    let listen_ports_for_admin = listen_ports.clone();
    // ★ ★ **收流量之前先把域名上游解析一遍。**
    //   ⚠ 少了这一步，第一批请求会撞上「还没解析出来」而被回 502；
    //   而在改这一批之前，它们撞上的是**每请求一次 panic**（实测过）。
    dns::resolve_now(&rt, "启动");
    let dns_interval = dns::tick_interval(cfg);
    let shared = fulcrum_runtime::SharedRuntime::new(rt);

    // ── ★ ★ 指标里「抓取时去问活体」那几个族的取数对象（**M2 批 M**）───────────
    //
    // ★ ★ **登记的是句柄，不是一份读数**：上游在途数、健康位、证书到期时刻、
    //   ACME 签发计数全都在渲染那一刻现问 —— 能从被测对象本身问到的东西，
    //   就不要在旁边再记一份，否则两份迟早不一致，而不一致的那天没有任何东西会说。
    // ★ 登记 `shared` 而不是某一份 `Runtime` 快照：全量 load 换掉的正是它里面那一份。
    // ⚠ `acme` 这一格可以是 `None`（这份配置里没有自动签发）—— 那时
    //   `fulcrum_acme_issue_total` 只出 HELP/TYPE、不出样本，而**不是整族消失**：
    //   整族消失会让「没接上」与「没数据」在抓取端看起来一模一样。
    metrics::register_live(metrics::LiveSources {
        runtime: Some(shared.clone()),
        resolver: Some(plan.resolver.clone()),
        acme: acme.clone(),
    });

    // ── ★ ★ 本代的身份（**G109 ①**）——**一个进程一把，不是一个端口一把** ─────
    //
    // ⚠ 每个监听端口各铸一把也能跑，而且**判据全绿** —— 直到换代那天：
    //   转交是按 DCID 前缀反查「这是哪一代」的，一代进程有多个身份，
    //   老一代就认不全自己的连接。★ 那种红在批 K 才出现，而根因在这一行。
    // ★ 铸在循环外，是让「一代一个」在结构上成立，而不是靠每个分支都记得传同一个。
    let gen_id = quic::gen_id::GenId::random();
    // ── HTTP 面的 PROXY protocol 「收」半边（M2 批 L 第 ① 步）—─────────
    //
    // ★ 一个进程一份，所有端口共用 —— 信任清单本来就是**全局**的
    //   （它是**连接级**判断：一条连接上还没有 Host，还不知道会落到哪个站点）。
    // ☆ 它拿的是 `SharedRuntime` 不是快照，理由见 `proxyproto` 模块的类型文档。
    let proxy_protocol_policy = proxyproto::HttpProxyProtocol::new(shared.clone());
    for (port, is_tls) in listen_ports {
        // ── ★ ★ ★ **G110：HTTP/3 跟着 `tls` 自动开** ───────────────────────
        //
        // 有 TLS 的端口自动在**同一个端口号**上听 UDP，并对 h1/h2 的响应发
        // `Alt-Svc: h3=":<端口>"`。⚠ 不发它，浏览器永远不会主动尝试 h3。
        //
        // ★ `ma` 取一天（RFC 7838 §3.1 的默认值，这里写显式）：取更长只会让
        //   「哪天关掉 h3」之后客户端多试很久的 UDP，而重新广播本来就不花什么。
        let alt_svc = is_tls.then(|| format!("h3=\":{port}\"; ma=86400"));
        let app = Arc::new(FulcrumApp::new(
            shared.clone(),
            port,
            is_tls,
            http01.clone(),
            cache.clone(),
            alt_svc,
        ));
        let kind = if is_tls { "https" } else { "http" };
        // ⚠ `H1Entry` 是为了让 h3 那一侧拿到**同一个** app 实例，理由见它的类型文档。
        let mut svc =
            ListeningService::new(format!("fulcrum-{kind}-{port}"), H1Entry::new(app.clone()));

        // ── ★ ★ ★ HTTP 面的 PROXY protocol（**M2 批 L 第 ① 步**，fork 改动 12）────
        //
        // ★ ★ **无条件挂，不看清单空不空** —— 有条件挂的话，一份后来才写上
        //   `proxy_protocol_from` 的配置做 `POST /load` 会**装得上、不报错、也不生效**（D19 那个形状）。
        // ★ 「挂了」不等于「会读字节」：清单空时 `trusts()` 恒 false，
        //   而 fork 那侧在 false 时一个字节都不读。
        svc.endpoints()
            .set_proxy_protocol(proxy_protocol_policy.clone());

        let bind = format!("{}:{port}", opts.bind_host);
        if is_tls {
            // ⚠ ⚠ ★ **别把这一行换成 `TlsSettings::with_callbacks`「顺手拿点什么」。**
            //   带回调时上游走 `handshake_with_callback()`，每次握手都要多走一趟
            //   「挂起 → `certificate_callback` → `resume_accept`」
            //   （`start_accept()` 无条件装一个恒回 `-1` 的 `cert_cb`）。
            //   访问日志要的 SNI / ALPN 由 **fork 改动 14** 直接记进 `SslDigest`
            //   （`from_ssl()` 握手结束后本来就握着 `&SslRef`）⇒ 那趟开销归零。
            // ★ 守它的是 `crates/fulcrum/tests/tls_digest_gate.rs`。
            match pingora_core::tls::ssl::SslAcceptor::mozilla_intermediate_v5(
                pingora_core::tls::ssl::SslMethod::tls(),
            )
            .map(pingora_core::listeners::tls::TlsSettings::from)
            {
                Ok(mut settings) => {
                    // ★ ★ **G104 的落点就在这一行**：h1/h2 入口与（将来的）h3 入口
                    //   挂**同一个**回调，于是「两个入口各有一套挑证书实现」在结构上做不到。
                    //   那正是取「统一 BoringSSL」而不是「两套并存」的全部理由（D18/G66 同源）。
                    plan.resolver.install_into(&mut settings);

                    // h2 + http/1.1 —— ALPN 里两个都要有，否则只会 h2 的客户端连不上。
                    // ★ ★ 再加一条 `acme-tls/1`：TLS-ALPN-01（RFC 8737 / G54 的「主」）
                    //   要在**同一个端口**上完成，挑战挪不到别处（RFC 8737 §3 写死 443）。
                    // ★ 顺序即偏好序：`acme-tls/1` 放末尾，**正常流量的偏好完全不受影响**，
                    //   而只提供它的 CA 验证连接会且只会拿到它。
                    // ⚠ 这里写的是 **wire 格式**（每条前面一个长度字节），
                    //   与 `set_alpn_protos` 收的格式一致。
                    settings.set_alpn_select_callback(|_ssl, client| {
                        const PREF: &[u8] = b"h2http/1.1
acme-tls/1";
                        pingora_core::tls::ssl::select_next_proto(PREF, client)
                            .ok_or(pingora_core::tls::ssl::AlpnError::NOACK)
                    });
                    info!(
                        "监听 {bind}（HTTPS，按 SNI 动态挑证书，ALPN: h2 / http/1.1 / acme-tls/1）"
                    );
                    svc.add_tls_with_settings(&bind, None, settings);
                }
                Err(e) => {
                    error!("建不出 TLS 监听设置：{e}");
                    std::process::exit(1);
                }
            }
        } else {
            info!("监听 {bind}（HTTP）");
            svc.add_tcp(&bind);
        }
        server.add_service(svc);

        // ── ★ ★ ★ HTTP/3 入口（**M2 批 J**，G110）───────────────────────────
        //
        // ★ 它是**另一个 `Service`**：pingora 的 `ListeningService` 只管 TCP/UDS，
        //   而 QUIC 要自己收 UDP 包、按 DCID 分发 ⇒ `QuicListenerService` 自建监听器
        //   并自己参与 socket 移交（fd 键前缀 `fulcrum-quic:`）。
        // ⚠ **跟在 `add_service(svc)` 之后加**，于是启动日志里同一端口的 h1/h2 与 h3
        //   两行是挨着的 —— 「443 上到底开没开 h3」是运维第一个会问的问题。
        if is_tls {
            server.add_service(quic::listener::QuicListenerService::new(
                bind.clone(),
                opts.upgrade,
                plan.resolver.clone(),
                gen_id,
                app.clone(),
                // ★ ★ **M2 批 K**：换代转交 socket 落在**换代 socket 的父目录**里。
                //   ⚠ 那是一条推导，所以 `run_dir_of` 有自己的判据 —— 见它的文档。
                quic::listener::run_dir_of(&opts.upgrade_sock),
            ));
        }
    }

    // ── L4 面：TCP 透传（M2 批 A）────────────────────────────────────────
    //
    // ★ 排在 HTTP 监听器之后加：两者互不依赖，而按「先七层后四层」的顺序加，
    //   启动日志读起来与配置文件的形状一致。
    // ⚠ `--bind-host` 只在配置写成 `:3306`（没有主机部分）时才补上，
    //   写了 `127.0.0.1:3306` 就按写的来 —— 与 HTTP 那边**不同**：那边端口来自站点地址，
    //   主机部分永远由 `--bind-host` 决定，而 L4 的监听地址是**整串**写在配置里的。
    {
        let cur = shared.current();
        for l in &cur.l4_listeners {
            let bind = match &l.listen_host {
                Some(h) => format!("{h}:{}", l.listen_port),
                None => format!("{}:{}", opts.bind_host, l.listen_port),
            };
            // ★ ★ 装载日志要**逐条说出分流规则**：一条「按 SNI 走别处」的规则如果只活在
            //   配置文件里，运维在排查「为什么这个域名去了另一台机器」时无从下手。
            let ups: Vec<&str> = l
                .target
                .as_ref()
                .map(|t| t.upstreams.iter().map(|u| u.addr.as_str()).collect())
                .unwrap_or_default();
            if ups.is_empty() {
                info!(
                    "L4 {} {} → **没有兜底**：只服务下面这 {} 条规则认得的连接",
                    l.proto.as_str(),
                    bind,
                    l.rules.len()
                );
            } else {
                info!(
                    "L4 {} {} → 兜底 {} 个上游（{}），轮询",
                    l.proto.as_str(),
                    bind,
                    ups.len(),
                    ups.join(" ")
                );
            }
            for r in &l.rules {
                let rups: Vec<&str> = r.target.upstreams.iter().map(|u| u.addr.as_str()).collect();
                info!(
                    "  ├ {} {} → {}（按书写顺序，第一个命中即用）",
                    r.kind.as_str(),
                    r.values.join(" "),
                    rups.join(" ")
                );
            }
            // ★ 两种协议各起各的服务：它们的**失效面完全不同**（见 l4.rs 那张对照表），
            //   共用一个 `Service` 只会让两套语义在同一个 `match` 里互相打架。
            match l.proto {
                fulcrum_runtime::L4Proto::Tcp => {
                    server.add_service(l4::TcpProxyService::new(
                        shared.clone(),
                        &l.listen,
                        bind,
                        opts.upgrade,
                    ));
                }
                fulcrum_runtime::L4Proto::Udp => {
                    server.add_service(l4::UdpProxyService::new(
                        shared.clone(),
                        &l.listen,
                        bind,
                        opts.upgrade,
                    ));
                }
            }
        }
    }

    // ── ★ ★ G59 第 2 条：**启动时校验 DNS 凭据** ────────────────────────────
    //
    // 形状照 G15（On-Demand 没配准入就拒绝启动）：**错误在启动时暴露，
    // 不等被滥用才发现**。⚠ 理由不是洁癖 —— 拿到某域的 DNS 写权限
    // 等于能为该域签发任意证书，还能改 MX 劫持邮件。
    //
    // ★ 这一步跑在 `run_forever()` **之前**，所以「拒绝启动」是真的拒绝。
    //   ⚠ 这里要一个临时 runtime：校验是异步的，而 Pingora 的 runtime 还没起来。
    if let Some(manager) = &acme {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                if let Err(e) = rt.block_on(manager.verify_credentials()) {
                    error!("{e}");
                    error!("拒绝启动（G59 第 2 条：凭据校验不过就不启动）");
                    std::process::exit(1);
                }
            }
            // ⚠ 建不出 runtime 是本机资源问题，与凭据无关。说出来，不假装校验过了。
            Err(e) => error!("⚠ 建不出临时 runtime（{e}）—— 本次**没有**校验 DNS 凭据"),
        }
    }

    // ── ★ ★ 管理面（G8 的全量原子 load + G74 的强制续期），走 Unix socket（G14）──
    //
    // ⚠ 没配 `admin` 就**一个 socket 都不建**：管理面默认不出机器，
    //   而「默认开着」正是 G14 要堵的那个 Caddy 短处。
    if let Some(spec) = &cfg.global.admin {
        match admin::socket_path(spec) {
            Ok(path) => {
                // ⚠ 上一代留下的 socket 文件会让 bind 失败（EADDRINUSE），而报错只说
                //   「地址已被使用」，看不出是一个陈旧的文件。systemd 重启、非优雅退出都会留下它。
                // ★ ★ ★ **但换代（`-u`）那一趟绝对不能清**：监听 fd 是从上一代继承来的
                //   （`ListenerEndpoint::listen` 按地址字符串查 fd 表，UDS 的键就是这个路径），
                //   新一代根本不会重新 `bind()`。此时 unlink 掉，两代都在一个**没有名字的**
                //   inode 上 accept，客户端按路径连过去只拿到 ENOENT ——
                //   **一次 `systemctl reload` 之后管理面永久失联**，而日志里一切正常。
                if !opts.upgrade && std::path::Path::new(&path).exists() {
                    match std::fs::remove_file(&path) {
                        Ok(()) => info!("清掉上一代留下的管理 socket {path}"),
                        Err(e) => error!("清不掉旧的管理 socket {path}（{e}），下面多半绑不上"),
                    }
                }
                let app = admin::AdminApp::new(
                    shared.clone(),
                    listen_ports_for_admin,
                    acme.clone(),
                    Some(cache.clone()),
                );
                let mut svc = ListeningService::new("fulcrum-admin".to_string(), app);
                // ★ 0600：**这就是管理面的全部访问控制**（G14：交给文件系统 ACL）。
                svc.add_uds(
                    &path,
                    Some(std::os::unix::fs::PermissionsExt::from_mode(
                        admin::SOCKET_MODE,
                    )),
                );
                info!(
                    "管理面监听 unix:{path}（权限 {:o}）—— POST /load、POST /renew",
                    admin::SOCKET_MODE
                );
                server.add_service(svc);
            }
            // ⚠ 写错了要**拒绝启动**，不是打条警告继续跑：一个「以为管理面开着、
            //   其实没开」的进程，比一个起不来的进程难查得多。形状照 G15。
            Err(e) => {
                error!("{e}");
                std::process::exit(1);
            }
        }
    }

    // ── 主动健康检查（`health_uri`）────────────────────────────────────────
    //
    // ★ 只在**真的有目标配了 `health_uri`** 时才起（同 DNS 那条的理由）。
    //   ⚠ 判据与打点节奏共用一次判定，见 `health::any_health_check` 上那段。
    if health::any_health_check(cfg) {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-health-check",
            health::HealthCheckService::new(shared.clone(), health::tick_interval(cfg)),
        ));
    }

    // ── 磁盘缓存的后台维护（**M2 批 H**，G84）──────────────────────────────
    //
    // ★ 只在**真的是磁盘后端**时才起（同 DNS / 健康检查那条的理由）：
    //   内存后端下它没有任何事可做，而一个每秒醒一次什么都不做的任务，
    //   会让「有没有磁盘缓存」在 `top` 里看不出区别。
    // ★ ★ 它挂成 `BackgroundService` 而不是裸 `tokio::spawn`，是为了**停机那一刻**：
    //   淘汰索引要在那时存下去（G84 的 save 那一半），而裸 spawn 的任务会被直接丢掉。
    if cache.disk().is_some() {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-cache-maintenance",
            cache::CacheMaintenanceService::new(cache.clone()),
        ));
    }

    // ── 上游域名的定期重解析（`dns_refresh`）────────────────────────────────
    //
    // ★ 只在**真的有域名上游**时才起：一份全是 IP 字面量的配置起这个任务，
    //   等于每 30 秒醒来一次什么都不做，而日志里多一行没人需要的噪音。
    if shared
        .current()
        .all_upstreams()
        .iter()
        .any(|u| !u.is_literal_ip())
    {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-dns-refresh",
            dns::DnsRefreshService::new(shared.clone(), dns_interval),
        ));
    } else {
        info!("没有域名上游（全是 IP 字面量）—— 不起 DNS 重解析任务");
    }

    // ★ 排在监听器**之后**加：Pingora 按加入顺序起服务，而 HTTP-01 的应答
    //   要有人在端口上听着才有意义。
    if let Some(manager) = acme {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-acme",
            acme::AcmeService::new(manager),
        ));
    }

    // ── ★ ★ 就绪信号 + pid 文件（`Type=notify` 与 `ExecReload` 各自的那半边）──
    //
    // ⚠ **必须在 `run_forever()` 之前订阅**：它会 move 掉 `server`。
    // ★ 订阅的是 `ExecutionPhase::Running`——pingora 在所有 service 都启动完之后
    //   才发这个相位。换代时这正是要的那一刻：新一代已经把 fd 取走并开始 accept 了，
    //   此刻才写 pid 文件、才报就绪，是诚实的。
    process::spawn_readiness(server.watch_execution_phase(), opts.pid_file.clone());

    server.run_forever()
}
