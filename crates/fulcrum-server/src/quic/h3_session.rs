//! **h3 → 现有执行链的那座桥**（M2 批 J）。
//!
//! 接缝是 `HttpServerApp` / `ServerSession` —— 后者是个 **enum**，带
//! `Custom(Box<dyn SessionCustom>)` 变体与公开的 `Session::new_custom()`。
//! ⇒ 桥的形状：**为 h3 实现 `SessionCustom`，包起来原样喂给现有的 `serve_one`。**
//! （⚠ 别信「执行链是为 `ProxyHttp` 建的」那种说法 —— `ProxyHttp` 在本仓库零命中。）
//!
//! ⚠ **代价写在明处**：那个 trait 在上游标着 `#[doc(hidden)]`，上游可以不加 semver 就改。
//! 我们 vendor 了 pingora 所以换代时机可控，但 rebase 时这一页要跟着看。
//! ★ 与 fork「加能力」不同：**这里一行 fork 都没改。**
//!
//! # 形状：连接任务持有 quiche，会话只拿两根管子
//!
//! `SessionCustom` 要求 `Send + Sync + Unpin + 'static`，而 `quiche::Connection`
//! 是单线程的、由连接任务独占。⇒ 每条请求给一个 [`H3Session`]，它与连接任务之间
//! 只有两条 mpsc：请求体进来、响应出去。
//!
//! ```text
//!   连接任务（独占 quiche::Connection + quiche::h3::Connection）
//!        │  ToSession::Body/End/Reset          ▲  FromSession::Header/Body/Trailers/Finish
//!        ▼                                     │
//!   H3Session  ──包进 ServerSession::new_custom──▶  FulcrumApp::serve_one（**一行没改**）
//! ```
//!
//! # ⚠ 有几个方法**不兑现它名字的承诺**，而这一条有门看着
//!
//! [`NOT_HONORED`] 那几个是「有定义的行为 + 安全返回值」，**不写 `unimplemented!()`** ——
//! 照上游那份全 `unreachable!()` 的 stub 会**编得过、门也会绿**，然后在某条今天没人走的
//! 路径上炸。⇒ 每一个都返回定义好的安全值，并由本页末尾那道门钉住
//! 「**现有执行链不许调它们**」。

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use http::HeaderMap;
use log::debug;
use pingora_core::protocols::Digest;
use pingora_core::protocols::http::HttpTask;
use pingora_core::protocols::http::custom::CustomMessageWrite;
use pingora_core::protocols::http::custom::server::Session as SessionCustom;
use pingora_core::protocols::http::v1::client::http_req_header_to_wire;
use pingora_core::protocols::l4::socket::SocketAddr;
// ★ h3 的 TLS 摘要（D27 结案）。⚠ 经 `pingora_core` 的 re-export 拿，
//   与 `Digest` 同一条纪律。
use pingora_core::protocols::tls::SslDigest;
// ★ 经 `pingora_core` 的 re-export 拿，而不是再加一条 `pingora-error` 直接依赖 ——
//   与「BoringSSL 类型只经 pingora-boringssl 拿」同一条纪律（G111 破的是那一条，不是这一条）。
use pingora_core::{Error, ErrorType, Result};
use pingora_http::{RequestHeader, ResponseHeader};
use std::time::Duration;
use tokio::sync::mpsc;

/// 连接任务 → 会话。
#[derive(Debug)]
pub enum ToSession {
    /// 一段请求体。
    Body(Bytes),
    /// 请求体到此结束（h3 的 `Finished` 事件）。
    End,
    /// 流或连接坏了，会话该收摊。
    Reset(String),
}

/// 会话 → 连接任务。
#[derive(Debug)]
pub enum FromSession {
    Header {
        resp: Box<ResponseHeader>,
        end: bool,
    },
    Body {
        data: Bytes,
        end: bool,
    },
    Trailers(HeaderMap),
    /// 会话侧收摊了（正常结束或被掐断）。
    Finish,
}

/// **本批不兑现其名字承诺的方法**。
///
/// ⚠ ⚠ 这不是「以后再说」的清单，它是**判据的输入** —— 本页末尾那道门会扫现有执行链，
/// 一旦有人开始调它们就判红。⇒ 加一条到这里，等于给它配一道门；从这里删一条，
/// 等于声称它已经真的实现了。
///
/// | 方法 | 本批的行为 | 为什么可以是这样 |
/// |---|---|---|
/// | `enable_retry_buffering` | 空操作 | 我们不做请求体重放（换上游重试）|
/// | `retry_buffer_truncated` | `false` | 与 h2 在「从没开过缓冲」时同值 |
/// | `get_retry_buffer` | `None` | 同上：没有缓冲 ⇒ 调用方不会拿它去重试 |
/// | ~~`digest` / `digest_mut`~~ | ✅ **已兑现（D27 结案）** | ⚠ ★ ★ 曾经的理由是「pingora 的 `Digest` 是给 h1/h2 的连接摘要，h3 这侧没有对应物」——**那句话是错的**：h3 连接一样有 TLS 版本、SNI 与 ALPN，缺的只是一条把它们送出去的路。⇒ [`quic_digest`] 现造一份，`digest()` / `digest_mut()` 真的返回它，**两个名字都离开了这张表**。⚠ 只有 `cipher` 仍是空的（quiche 的 `Handshake::cipher()` 锁在私有 `mod tls` 里）|
/// | `take_custom_message_*` | `None` | 「自定义消息」通道本仓库一处都没用 |
/// | `restore_custom_message_*` | `Err` | ★ 恢复一个从来没被取走的东西是**调用方的逻辑错**，不能静默成功 |
pub const NOT_HONORED: &[&str] = &[
    "enable_retry_buffering",
    "retry_buffer_truncated",
    "get_retry_buffer",
    // ★ ★ ★ `digest` / `digest_mut` **于（§10，D27 结案）
    //   从这张表上删掉了** —— 不是「让门闭嘴」，是它们**真的被实现出来了**。
    //   ⚠ 判据：`不兑现承诺的那一组回安全值而不是_panic` 现在断言 `digest().is_some()`，
    //     而访问日志那一格（`tests/log/run.sh`）在**真的 h3 请求**上量 `tls_*` 三格。
    "take_custom_message_reader",
    "restore_custom_message_reader",
    "take_custom_message_writer",
    "restore_custom_message_writer",
];

/// 一条 h3 请求，装成现有执行链认得的样子。
pub struct H3Session {
    req: Box<RequestHeader>,
    client: Option<SocketAddr>,
    server: Option<SocketAddr>,
    body_rx: mpsc::Receiver<ToSession>,
    /// ★ ★ **一条连接上的所有流共用同一个出站通道**，所以每条消息都要带 stream_id。
    /// ⚠ 每条流各开一个通道的话，连接任务就要在**动态数量**的接收端上 select ——
    /// 那是「听起来更干净」与「写得出来」不一致的地方。
    out: mpsc::Sender<(u64, FromSession)>,
    /// 这条请求在 QUIC 上的流号。
    stream_id: u64,
    written: Option<ResponseHeader>,
    /// h3 的 `Headers` 事件自带 `has_body`，所以这一条**开局就知道**，
    /// 不像 h1 要等读一次才分得清。
    body_empty: bool,
    body_done: bool,
    read_n: usize,
    sent_n: usize,
    finished: bool,
    // ⚠ 下面三个**存得下、而本批没有任何人照着它做事** —— 超时归连接任务管
    //   （quiche 的定时器是连接级的，不是流级的）。它们也在 NOT_HONORED 的精神之内，
    //   但**没有列进去**：列进去会让那道门去禁止一件将来必然要做的事。
    //   ⇒ 留一条 TODO 比留一道假门好。
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    drain_timeout: Option<Duration>,
    /// 这条 QUIC 连接的 TLS 摘要（**，D27 结案**）。
    ///
    /// ★ 由 [`quic_digest`] 在**连接**那一层造一份，每条流克隆一个
    /// （`Digest` 里那份 `SslDigest` 是 `Arc`，克隆只是一次引用计数）。
    /// ⚠ `None` 只会出现在单测里 —— 真流量上 h3 层建起来时它一定有值。
    digest: Option<Digest>,
}

/// 给一条 QUIC 连接造一份 `Digest`（**，D27 结案**）。
///
/// # ★ ★ 三格从哪来，第四格为什么没有
///
/// | 格 | 来源 |
/// |---|---|
/// | `version` | **恒为 `TLSv1.3`** —— RFC 9001 §4.2 写死了 QUIC 只能用 TLS 1.3。★ 这不是猜，是规范 |
/// | `sni` | `quiche::Connection::server_name()` |
/// | `alpn` | `quiche::Connection::application_proto()` |
/// | `cipher` | ⚠ **拿不到**：quiche 的 `Handshake::cipher()` 在**私有** `mod tls` 里，而 quiche 一个 TLS 出口都没有 re-export ⇒ 留空 = 那一格不出现 |
///
/// ★ 与 h1/h2 那侧**汇到同一个类型**（`SslDigest`），于是访问日志那一层
/// 一行分支都不用写 —— 「同一格数据两个填法」在结构上做不到。
pub fn quic_digest(sni: Option<String>, alpn: Option<String>) -> Digest {
    let mut ssl = SslDigest::new("", "TLSv1.3", None, None, Vec::new());
    // ★ ★ `sni` / `alpn` 是 **fork 改动 14** 给 `SslDigest` 加的两格 ——
    //   上游没有它们，见 `vendor/pingora/FORK.md`。
    ssl.sni = sni.map(|s| s.to_ascii_lowercase());
    ssl.alpn = alpn;
    Digest {
        ssl_digest: Some(std::sync::Arc::new(ssl)),
        ..Default::default()
    }
}

impl H3Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        req: Box<RequestHeader>,
        has_body: bool,
        client: Option<std::net::SocketAddr>,
        server: Option<std::net::SocketAddr>,
        body_rx: mpsc::Receiver<ToSession>,
        out: mpsc::Sender<(u64, FromSession)>,
        stream_id: u64,
        digest: Option<Digest>,
    ) -> Self {
        H3Session {
            digest,
            req,
            client: client.map(SocketAddr::Inet),
            server: server.map(SocketAddr::Inet),
            body_rx,
            out,
            stream_id,
            written: None,
            body_empty: !has_body,
            body_done: !has_body,
            read_n: 0,
            sent_n: 0,
            finished: false,
            read_timeout: None,
            write_timeout: None,
            drain_timeout: None,
        }
    }

    /// 往连接任务发一条。
    ///
    /// ⚠ 对端没了 ⇒ 连接已经塌了，这是 `WriteError` 而不是内部错误 ——
    /// **两者在日志里必须分得开**：前者是网络的常态，后者是我们的缺陷。
    async fn send(&self, msg: FromSession) -> Result<()> {
        self.out.send((self.stream_id, msg)).await.map_err(|_| {
            Error::explain(
                ErrorType::WriteError,
                "h3 连接任务已经不在了，这条响应发不出去",
            )
        })
    }
}

#[async_trait]
impl SessionCustom for H3Session {
    fn req_header(&self) -> &RequestHeader {
        &self.req
    }

    fn req_header_mut(&mut self) -> &mut RequestHeader {
        &mut self.req
    }

    async fn read_body_bytes(&mut self) -> Result<Option<Bytes>> {
        if self.body_done {
            return Ok(None);
        }
        match self.body_rx.recv().await {
            Some(ToSession::Body(b)) => {
                self.read_n += b.len();
                Ok(Some(b))
            }
            Some(ToSession::End) => {
                self.body_done = true;
                Ok(None)
            }
            Some(ToSession::Reset(why)) => {
                self.body_done = true;
                Error::e_explain(ErrorType::ReadError, format!("h3 流被掐断：{why}"))
            }
            // 连接任务先走了 —— 与 Reset 同义，但措辞要分得开。
            None => {
                self.body_done = true;
                Error::e_explain(ErrorType::ConnectionClosed, "h3 连接任务已经不在了")
            }
        }
    }

    async fn drain_request_body(&mut self) -> Result<()> {
        while self.read_body_bytes().await?.is_some() {}
        Ok(())
    }

    async fn write_response_header(&mut self, resp: Box<ResponseHeader>, end: bool) -> Result<()> {
        self.written = Some(resp.as_ref().clone());
        self.send(FromSession::Header { resp, end }).await
    }

    async fn write_response_header_ref(&mut self, resp: &ResponseHeader, end: bool) -> Result<()> {
        self.write_response_header(Box::new(resp.clone()), end)
            .await
    }

    async fn write_body(&mut self, data: Bytes, end: bool) -> Result<()> {
        self.sent_n += data.len();
        self.send(FromSession::Body { data, end }).await
    }

    async fn write_trailers(&mut self, trailers: HeaderMap) -> Result<()> {
        self.send(FromSession::Trailers(trailers)).await
    }

    /// 批量写。★ 返回值是「**响应到此为止了吗**」——调用方据它决定还要不要再喂。
    async fn response_duplex_vec(&mut self, tasks: Vec<HttpTask>) -> Result<bool> {
        let mut end = false;
        for t in tasks {
            match t {
                HttpTask::Header(resp, e) => {
                    self.write_response_header(resp, e).await?;
                    end |= e;
                }
                HttpTask::Body(data, e) => {
                    // ⚠ `Body(None, true)` 是「没有数据、但到此结束」——它必须被送出去，
                    //   否则对端永远等不到流的结尾。
                    let data = data.unwrap_or_default();
                    if !data.is_empty() || e {
                        self.write_body(data, e).await?;
                    }
                    end |= e;
                }
                HttpTask::Trailer(Some(t)) => {
                    self.write_trailers(*t).await?;
                    end = true;
                }
                HttpTask::Trailer(None) => {}
                HttpTask::Done => {
                    end = true;
                }
                HttpTask::Failed(e) => return Err(e),
                // ⚠ h3 上没有 h1 的协议升级（websocket 走的是 Extended CONNECT，
                //   与这一条不是同一件事）。当成普通体写出去比静默丢弃安全。
                HttpTask::UpgradedBody(data, e) => {
                    let data = data.unwrap_or_default();
                    if !data.is_empty() || e {
                        self.write_body(data, e).await?;
                    }
                    end |= e;
                }
            }
        }
        Ok(end)
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.read_timeout = timeout;
    }
    fn get_read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }
    fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        self.write_timeout = timeout;
    }
    fn get_write_timeout(&self) -> Option<Duration> {
        self.write_timeout
    }
    fn set_total_drain_timeout(&mut self, timeout: Option<Duration>) {
        self.drain_timeout = timeout;
    }
    fn get_total_drain_timeout(&self) -> Option<Duration> {
        self.drain_timeout
    }

    fn request_summary(&self) -> String {
        format!(
            "{} {}, Host: {}",
            self.req.method,
            self.req.uri,
            self.req
                .headers
                .get(http::header::HOST)
                .map(|v| String::from_utf8_lossy(v.as_bytes()))
                .unwrap_or_default()
        )
    }

    fn response_written(&self) -> Option<&ResponseHeader> {
        self.written.as_ref()
    }

    async fn shutdown(&mut self, code: u32, ctx: &str) {
        debug!("h3 会话收摊（code={code}）：{ctx}");
        self.finished = true;
        // ⚠ 这里**故意不管发没发出去**：收摊路径上再报一次「发不出去」没有任何用处，
        //   而它会盖住真正的那条错误。
        let _ = self.out.send((self.stream_id, FromSession::Finish)).await;
    }

    fn is_body_done(&mut self) -> bool {
        self.body_done
    }

    async fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.send(FromSession::Finish).await
    }

    fn is_body_empty(&mut self) -> bool {
        self.body_empty
    }

    async fn read_body_or_idle(&mut self, no_body_expected: bool) -> Result<Option<Bytes>> {
        // ★ h1 那侧这一条要在「等下一个请求」与「读体」之间做选择；
        //   h3 上一条流就是一次请求，没有那个歧义 ⇒ 直接读体。
        if no_body_expected {
            return Ok(None);
        }
        self.read_body_bytes().await
    }

    fn body_bytes_sent(&self) -> usize {
        self.sent_n
    }
    fn body_bytes_read(&self) -> usize {
        self.read_n
    }

    /// ✅ **此后真的兑现**（D27 结案）：一条 h3 连接的 TLS 摘要，
    /// 由 [`quic_digest`] 在连接那一层造好。★ 它让访问日志的 `tls_*` 那几格
    /// 在 h3 上与 h1/h2 走**同一段代码**。
    fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }
    fn digest_mut(&mut self) -> Option<&mut Digest> {
        self.digest.as_mut()
    }

    // ── 下面是 NOT_HONORED 那一组：有定义的行为 + 安全返回值 ──────────────

    fn client_addr(&self) -> Option<&SocketAddr> {
        self.client.as_ref()
    }
    fn server_addr(&self) -> Option<&SocketAddr> {
        self.server.as_ref()
    }

    /// h2 那侧就是这么做的（`http_req_header_to_wire`）——把 h2/h3 的请求头
    /// 印成一行 h1 的样子，给日志与需要原始形态的地方用。
    fn pseudo_raw_h1_request_header(&self) -> Bytes {
        http_req_header_to_wire(&self.req)
            .map(|b| b.freeze())
            .unwrap_or_default()
    }

    fn enable_retry_buffering(&mut self) {
        debug!("h3 会话不做请求体重放缓冲（NOT_HONORED）");
    }
    fn retry_buffer_truncated(&self) -> bool {
        // ★ 与 h2 在「从没开过缓冲」时同值：false。
        false
    }
    fn get_retry_buffer(&self) -> Option<Bytes> {
        None
    }

    async fn finish_custom(&mut self) -> Result<()> {
        self.finish().await
    }

    fn take_custom_message_reader(
        &mut self,
    ) -> Option<Box<dyn Stream<Item = Result<Bytes>> + Unpin + Send + Sync + 'static>> {
        None
    }

    fn restore_custom_message_reader(
        &mut self,
        _reader: Box<dyn Stream<Item = Result<Bytes>> + Unpin + Send + Sync + 'static>,
    ) -> Result<()> {
        // ⚠ **不能静默成功**：还回一个从来没被取走的东西，说明调用方的状态机与我们
        //   的假设对不上 —— 静默吞掉会让那个不一致一路带到别处才炸。
        Error::e_explain(
            ErrorType::InternalError,
            "h3 会话没有自定义消息通道，restore_custom_message_reader 不该被调到",
        )
    }

    fn take_custom_message_writer(&mut self) -> Option<Box<dyn CustomMessageWrite>> {
        None
    }

    fn restore_custom_message_writer(
        &mut self,
        _writer: Box<dyn CustomMessageWrite>,
    ) -> Result<()> {
        Error::e_explain(
            ErrorType::InternalError,
            "h3 会话没有自定义消息通道，restore_custom_message_writer 不该被调到",
        )
    }
}

// ── 门：现有执行链不许调 NOT_HONORED 里的任何一个 ──────────────────────────
//
// ★ ★ ★ 这就是「有定义的行为 + **一条断言**」里的那条断言。没有它的话，
//   上面那一组只是一堆「读起来很合理」的返回值 —— 而本仓库第 67/69 轮各记过一次
//   「一个防御措施配一句听起来很对的理由，比没有理由更难查」。

/// 现有执行链里所有 `session.<方法>(` 的调用点。
///
/// ⚠ 只在测试构建里存在：它 `include_str!` 三份产品源码，没有理由进产物。
#[cfg(test)]
const CHAIN_SOURCES: &[(&str, &str)] = &[
    ("lib.rs", include_str!("../lib.rs")),
    ("files/mod.rs", include_str!("../files/mod.rs")),
    ("admin.rs", include_str!("../admin.rs")),
];

// ⚠ ★ **这里曾经有一份叫 `NONE_IS_A_DEFINED_ANSWER` 的豁免名单，它活了不到一天。**
//   访问日志要 TLS 四格时 `session.digest()` 进了执行链，而 h3 走得到 ⇒ 这道门当场红了；
//   当时给它开了一条「它回的值在调用处是**有定义的答案**」的豁免。
//   ✅ D27 结案之后 `digest` 被**真的实现出来了**（[`quic_digest`]），豁免没有对象，
//   这道门回到满强度。
//
//   > ★ ★ **一个「有定义的答案」型豁免，多半是一处还没做的功能穿着判据的衣服。**
//   > ⚠ 而它真的在一天后被删掉，唯一的原因是**过期日期写进了豁免自己的文档**。

/// 从一段源码里抠出所有 `session.<名字>(` 的方法名。
///
/// ⚠ 只认 `session.` 这一个前缀是**够的，而这一条是查实过的**：产品里每一个
/// `ServerSession` 类型的绑定都字面叫 `session`（§10 核过）。
/// ★ 但这份「够」会过期 —— 所以下面那道门带一条下界断言，改名之后它会先红。
#[cfg(test)]
fn session_calls(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in src.match_indices("session.") {
        let rest = &src[i + "session.".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && rest[name.len()..].starts_with('(') {
            out.push(name);
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingora_core::protocols::http::ServerSession;

    fn a_request() -> Box<RequestHeader> {
        let mut r = RequestHeader::build("GET", b"/hello", None).expect("造请求头");
        r.insert_header("host", "example.com").expect("插 host");
        Box::new(r)
    }

    /// ★★★ **这一条才是那句订正的证明本身。**
    ///
    /// §10 写下「h3 接进运行时图不需要第 13 处 fork『加能力』改动，
    /// 它是上游现成的扩展点」时，那还只是**读码结论**。
    /// 这里把 `H3Session` 真的包进 `ServerSession::new_custom()`，
    /// 并**逐个走一遍现有执行链真正会调的那 8 个方法** ——
    /// ⚠ trait 实现编得过**不等于**这件事成立：`ServerSession` 是个 enum，
    /// 它的每个方法都在 `match` 里分派，`Custom` 那一支完全可以是个空壳。
    #[tokio::test]
    async fn 包进_serversession_之后执行链那八个方法逐个走得通() {
        let (body_tx, body_rx) = mpsc::channel(8);
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let sess = H3Session::new(
            a_request(),
            true,
            Some("192.0.2.7:44300".parse().expect("测试地址")),
            Some("192.0.2.1:443".parse().expect("测试地址")),
            body_rx,
            out_tx,
            0,
            Some(quic_digest(Some("A.Example".into()), Some("h3".into()))),
        );
        let mut s = ServerSession::new_custom(Box::new(sess));

        // ① read_request —— Custom 支回 Ok(true)（构造时请求头已经读好了）
        assert!(s.read_request().await.expect("read_request"));
        // ② req_header
        assert_eq!(s.req_header().uri.path(), "/hello");
        // ③ client_addr
        assert_eq!(
            s.client_addr().and_then(|a| a.as_inet()).map(|a| a.port()),
            Some(44300)
        );
        // ④ set_keepalive —— Custom 支是空操作，不该 panic
        s.set_keepalive(Some(60));

        // ⑤ read_request_body
        body_tx
            .send(ToSession::Body(Bytes::from_static(b"hi")))
            .await
            .expect("喂体");
        body_tx.send(ToSession::End).await.expect("喂结束");
        assert_eq!(
            s.read_request_body().await.expect("读体"),
            Some(Bytes::from_static(b"hi"))
        );
        assert_eq!(s.read_request_body().await.expect("读体到尾"), None);

        // ⑥ write_response_header
        let resp = ResponseHeader::build(204, None).expect("造响应头");
        s.write_response_header(Box::new(resp))
            .await
            .expect("写响应头");
        match out_rx.recv().await.expect("响应头该到了") {
            (0, FromSession::Header { resp, end }) => {
                assert_eq!(resp.status, 204);
                assert!(
                    !end,
                    "ServerSession 的 write_response_header 走的是 end=false 那一支"
                );
            }
            other => panic!("收到的不是响应头：{other:?}"),
        }

        // ⑦ write_response_body
        s.write_response_body(Bytes::from_static(b"body"), true)
            .await
            .expect("写体");
        match out_rx.recv().await.expect("体该到了") {
            (0, FromSession::Body { data, end }) => {
                assert_eq!(&data[..], b"body");
                assert!(end);
            }
            other => panic!("收到的不是体：{other:?}"),
        }

        // ⑧ finish —— h3 上没有可复用的连接，回 None 是对的
        assert!(s.finish().await.expect("收尾").is_none());
        assert!(matches!(
            out_rx.recv().await.expect("收尾信号该到了"),
            (0, FromSession::Finish)
        ));
    }

    /// 连接任务先走了 ⇒ 读体要给一个**说得清的错**，而不是挂住或 panic。
    #[tokio::test]
    async fn 连接任务先走了读体给错而不是挂住() {
        let (body_tx, body_rx) = mpsc::channel(1);
        let (out_tx, _out_rx) = mpsc::channel(1);
        let mut sess = H3Session::new(a_request(), true, None, None, body_rx, out_tx, 0, None);
        drop(body_tx);
        assert!(sess.read_body_bytes().await.is_err());
        // ★ 再读一次也不许挂住 —— body_done 已经置上了。
        assert_eq!(sess.read_body_bytes().await.expect("第二次读"), None);
    }

    /// `has_body=false` 时开局就是「体读完了」，不该去等一条永远不来的消息。
    #[tokio::test]
    async fn 没有请求体时开局就是读完了() {
        let (_body_tx, body_rx) = mpsc::channel(1);
        let (out_tx, _out_rx) = mpsc::channel(1);
        let mut sess = H3Session::new(a_request(), false, None, None, body_rx, out_tx, 0, None);
        assert!(sess.is_body_empty());
        assert!(sess.is_body_done());
        assert_eq!(sess.read_body_bytes().await.expect("读体"), None);
    }

    /// ⚠ `Body(None, true)` 是「没有数据、但到此结束」—— 它必须被送出去。
    #[tokio::test]
    async fn 空体但结束的那一格必须被送出去() {
        let (_body_tx, body_rx) = mpsc::channel(1);
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let mut sess = H3Session::new(a_request(), false, None, None, body_rx, out_tx, 0, None);
        let end = sess
            .response_duplex_vec(vec![HttpTask::Body(None, true)])
            .await
            .expect("批量写");
        assert!(end, "end 标志要被带出来");
        match out_rx.recv().await.expect("该有一条") {
            (0, FromSession::Body { data, end }) => {
                assert!(data.is_empty());
                assert!(end, "空体但结束 —— 丢掉它对端就永远等不到流的结尾");
            }
            other => panic!("收到的不是体：{other:?}"),
        }
    }

    /// NOT_HONORED 那一组要**回定义好的安全值**，而不是 panic。
    #[test]
    fn 不兑现承诺的那一组回安全值而不是_panic() {
        let (_body_tx, body_rx) = mpsc::channel(1);
        let (out_tx, _out_rx) = mpsc::channel(1);
        let mut sess = H3Session::new(a_request(), false, None, None, body_rx, out_tx, 0, None);
        sess.enable_retry_buffering(); // 空操作
        assert!(!sess.retry_buffer_truncated(), "与 h2 在没开缓冲时同值");
        assert!(sess.get_retry_buffer().is_none());
        assert!(sess.take_custom_message_reader().is_none());
        assert!(sess.take_custom_message_writer().is_none());
        // ★ 而「还回一个从没被取走的东西」必须是错，不能静默成功 —— 两条都要验。
        assert!(
            sess.restore_custom_message_reader(Box::new(futures::stream::empty()))
                .is_err()
        );
        assert!(
            sess.restore_custom_message_writer(Box::new(NoWriter))
                .is_err()
        );
    }

    /// 给上面那条用的最小 `CustomMessageWrite`。
    struct NoWriter;
    #[async_trait]
    impl CustomMessageWrite for NoWriter {
        fn set_write_timeout(&mut self, _t: Option<Duration>) {}
        async fn write_custom_message(&mut self, _data: Bytes) -> Result<()> {
            Ok(())
        }
        async fn finish_custom(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn 扫描器自证能命中也能错过() {
        let src = "a.session.read_request(); session.write_body(x); session.not_a_call = 1; \
                   sess.other_name(); session.finish()";
        let got = session_calls(src);
        // 命中
        assert!(got.contains(&"read_request".to_string()));
        assert!(got.contains(&"write_body".to_string()));
        assert!(got.contains(&"finish".to_string()));
        // 错过：不是调用（后面不是括号）
        assert!(!got.contains(&"not_a_call".to_string()));
        // 错过：不是 session
        assert!(!got.contains(&"other_name".to_string()));
        // 空输入不能被当成「查过了、没问题」
        assert!(session_calls("").is_empty());
    }

    #[test]
    fn 现有执行链不许调用_not_honored_里的任何一个() {
        let mut all: Vec<String> = Vec::new();
        for (_, src) in CHAIN_SOURCES {
            all.extend(session_calls(src));
        }
        all.sort();
        all.dedup();

        // ★ 下界一：一个都没抠出来时，下面那条交集判定会拿空集比，看起来永远是绿的。
        assert!(
            all.len() >= 5,
            "只从执行链里抠出 {} 个方法调用（{all:?}），扫描多半坏了；本次检查不能采信",
            all.len()
        );
        // ★ 下界二：光有数量不够 —— 钉一个必然在里面、而与本门无关的名字。
        assert!(
            all.contains(&"req_header".to_string()),
            "执行链里连 req_header 都没扫到 —— 扫描多半坏了，本次检查不能采信。扫到的是：{all:?}"
        );

        let bad: Vec<&String> = all
            .iter()
            .filter(|m| NOT_HONORED.contains(&m.as_str()))
            .collect();
        assert!(
            bad.is_empty(),
            "\n执行链开始调用 h3 会话**不兑现承诺**的方法了：{bad:?}\n\
             · 要么把它在 h3_session.rs 里真的实现出来，并从 NOT_HONORED 里删掉；\n\
             · 要么确认这条路径 h3 走不到，并在这里写明为什么。\n\
             ⚠ 别只把名字从 NOT_HONORED 里删掉 —— 那会让这道门闭嘴，而行为一点没变。\n\
             ⚠ ⚠ ★ 也别再发明「它回的值是有定义的答案」那种豁免：试过一次，\n\
             \x20 它活了不到一天（见 NOT_HONORED 上面那段注释）—— 那种豁免多半是\n\
             \x20 **一处还没做的功能穿着判据的衣服**。"
        );
    }

    /// `digest` 真的被兑现了（**，D27 结案**），而不是「从名单里删掉了名字」。
    ///
    /// ★ ★ 这一条是上面那道门的**另一半**：门只管「执行链别调没兑现的」，
    /// 而**「它到底兑现没有」得由这里说**。⚠ 少了它，一次「把名字从 NOT_HONORED 删掉
    /// 而行为一点没变」的改动会让门变绿 —— 那正是门自己的失败消息在警告的那件事。
    #[test]
    fn digest_真的兑现了_三格有值而_cipher_有意留空() {
        let d = quic_digest(Some("A.Example".into()), Some("h3".into()));
        let ssl = d
            .ssl_digest
            .as_ref()
            .expect("h3 的 digest 必须带 SslDigest");
        // RFC 9001 §4.2：QUIC 只能用 TLS 1.3 —— 这是规范，不是猜。
        assert_eq!(ssl.version, "TLSv1.3");
        // ⚠ SNI 压小写，与 h1/h2 那侧（fork 改动 14 的 `from_ssl`）逐字同源。
        assert_eq!(ssl.sni.as_deref(), Some("a.example"));
        assert_eq!(ssl.alpn.as_deref(), Some("h3"));
        // ★ ★ `cipher` **有意是空的**：quiche 的 `Handshake::cipher()` 锁在私有 `mod tls` 里。
        //   ⚠ 空串在访问日志那一层的意思是「这一格不出现」，而**不是**「套件叫空」。
        assert_eq!(ssl.cipher, "", "cipher 拿不到时必须留空，不许编一个值");

        // 反向：没 SNI / 没 ALPN 的连接照样有 digest，只是那两格是 None。
        let bare = quic_digest(None, None);
        let ssl = bare.ssl_digest.as_ref().expect("仍然要有");
        assert!(ssl.sni.is_none() && ssl.alpn.is_none());
        assert_eq!(ssl.version, "TLSv1.3", "版本与有没有 SNI 无关");
    }

    /// ★ 反向的一半：把一个**确实被调用**的名字塞进 NOT_HONORED，门必须红。
    /// 没有这一条的话，上面那道门与「恒为真」无法区分。
    #[test]
    fn 门自证它抓得住_把一个真调用点塞进名单就该红() {
        let mut all: Vec<String> = Vec::new();
        for (_, src) in CHAIN_SOURCES {
            all.extend(session_calls(src));
        }
        // `req_header` 是执行链真的在调的
        let fake_list = ["req_header"];
        let bad: Vec<&String> = all
            .iter()
            .filter(|m| fake_list.contains(&m.as_str()))
            .collect();
        assert!(
            !bad.is_empty(),
            "把一个真实调用点塞进名单之后这道门仍然是绿的 —— 它抓不住任何东西"
        );
    }
}
