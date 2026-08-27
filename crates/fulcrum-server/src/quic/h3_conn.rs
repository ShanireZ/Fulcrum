//! **`quiche::h3` 事件循环**（M2 批 J 第五步）——一条 QUIC 连接的任务本体。
//!
//! ```text
//!   UDP 数据报 ──▶ 连接任务（独占 quiche::Connection + quiche::h3::Connection）
//!                    │                                   ▲
//!                    │ Headers/Data/Finished             │ (stream_id, FromSession)
//!                    ▼                                   │
//!                 H3Session ──ServerSession::new_custom──▶ H3RequestHandler
//! ```
//!
//! # 三条这一层必须自己负责的事（h3 与 h1 在这里**不一样**）
//!
//! 1. ★ **`recv_body` 要读到 `Done` 为止** —— quiche 的 `Data` 事件**不会重新武装**，
//!    少读一次，这条流剩下的体就永远不会再来一个事件通知你。
//! 2. ★ ★ **`send_body` 会短写**：它回的是「这次写进去几个字节」，流控挡住时会小于给它的长度。
//!    ⇒ 每条流留一个待发队列，写不完就留着下一轮再写。
//!    ⚠ 不留的话，**大响应会被静默截断** —— 而客户端看到的是「连接好好的，内容少了一截」。
//! 3. ★ ★ ★ **逐跳头（`Connection` / `Keep-Alive` / `Transfer-Encoding` / `Upgrade` /
//!    `Proxy-Connection`）在 HTTP/3 里是禁止的**（RFC 9114 §4.2）——发出去对端会重置这条流。
//!    ⚠ 而我们的执行链是为 h1/h2 写的，**它完全可能带上这些头**。⇒ 这一层负责滤掉。
//!
//! [`H3RequestHandler`] 由 `crate::FulcrumApp` 实现 —— **同一个 app 实例、同一条执行链**，
//! 与 h1/h2 的唯一差别是不发 `Alt-Svc`。
//!
//! ⚠ ★ 上面第 3 条的真实危险面来自用户配置：一句 `header Connection "keep-alive"`
//! 在 h1 上完全合法。⇒ `tests/h3/run.sh` 拿的正是这条配置，并且**同时**验 h1 上
//! 那个头真的在 —— 没有那条对照，「h3 上没有它」什么也证明不了。

use crate::quic::h3_session::{FromSession, H3Session, ToSession};
use async_trait::async_trait;
use bytes::Bytes;
use http::HeaderMap;
use log::{debug, info, warn};
use pingora_core::protocols::http::ServerSession;
// ★ `h3::Header` 的 `name()` / `value()` 挂在这个 trait 上，不 import 就调不着。
use pingora_http::{RequestHeader, ResponseHeader};
use quiche::h3::NameValue;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// 一个 UDP 数据报的上限（与 [`super::listener`] 那份同值，理由也一样）。
const DATAGRAM_MAX: usize = 65535;
/// 一次从 h3 读体的块大小。
const BODY_CHUNK: usize = 16 * 1024;
/// 每条连接的出站队列深度。★ 它同时是**背压**：满了之后会话侧的 `write_body` 会等。
const OUT_QUEUE: usize = 256;
/// 每条流的请求体队列深度。
const BODY_QUEUE: usize = 32;

/// 一条 h3 请求交给谁去跑。
///
/// ★ ★ **这一层存在的理由是方向**：`quic` 模块不该知道 `FulcrumApp` ——
/// 那会让 HTTP/3 入口反向依赖数据面本体，而数据面本体又要依赖这个入口。
/// ⇒ 入口只认这个 trait，谁来实现由接线那一步决定（G110）。
#[async_trait]
pub trait H3RequestHandler: Send + Sync + 'static {
    async fn handle(&self, session: ServerSession);
}

/// **HTTP/3 里禁止出现的逐跳头**（RFC 9114 §4.2）。
///
/// ⚠ ⚠ ★ 发出去不是「对端忽略它」，是**对端按协议错误重置这条流** ——
/// 而我们的执行链是为 h1/h2 写的，它完全可能带上这些头。
/// ★ `TE` 是个例外：RFC 9114 允许它，但**只允许值恰好是 `trailers`**。
/// 这里的处置是**一律滤掉** —— 少发一个 `TE` 不会让任何东西坏掉，
/// 而发错一个会把整条流打掉。
const FORBIDDEN_IN_H3: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "te",
];

/// 把 h3 的请求头列表翻成执行链认得的 [`RequestHeader`]。
///
/// ⚠ `:authority` 落成 `host` 头 —— 我们的站点索引读的是 `Host`
/// （`lib.rs::host_of`），h3 上没有那个头，只有伪头。
pub fn to_request_header(list: &[quiche::h3::Header]) -> Result<Box<RequestHeader>, String> {
    let mut method: Option<Vec<u8>> = None;
    let mut path: Option<Vec<u8>> = None;
    let mut authority: Option<Vec<u8>> = None;
    let mut normal: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    for h in list {
        let (name, value) = (h.name(), h.value());
        match name {
            b":method" => method = Some(value.to_vec()),
            b":path" => path = Some(value.to_vec()),
            b":authority" => authority = Some(value.to_vec()),
            // `:scheme` 我们不用：入口本身就决定了它是 https。
            b":scheme" | b":protocol" => {}
            // ⚠ 认不得的伪头一律拒绝：RFC 9114 §4.3.1 明确要求把它当格式错误。
            _ if name.starts_with(b":") => {
                return Err(format!("认不得的伪头 {}", String::from_utf8_lossy(name)));
            }
            _ => normal.push((name.to_vec(), value.to_vec())),
        }
    }

    let method = method.ok_or_else(|| "请求头里没有 :method".to_string())?;
    let path = path.ok_or_else(|| "请求头里没有 :path".to_string())?;

    let mut req = RequestHeader::build(method.as_slice(), path.as_slice(), None)
        .map_err(|e| format!("这组伪头拼不出请求行：{e}"))?;

    // ★ `:authority` 先落，再落普通头 —— 万一对端**同时**给了 `host`，
    //   后者会覆盖它，与 h2 那侧的惯例一致。
    if let Some(a) = authority
        && let Err(e) = req.insert_header("host", a.as_slice())
    {
        return Err(format!(":authority 装不进 host 头：{e}"));
    }
    for (n, v) in normal {
        // ⚠ `append_header` 收的是 `HeaderName`，不收裸字节 —— 而这一步顺带把
        //   **非法头名挡在门外**：h3 那侧只保证它是字节串。
        let name = match http::HeaderName::from_bytes(&n) {
            Ok(h) => h,
            Err(e) => {
                return Err(format!(
                    "请求头名 {} 不合法：{e}",
                    String::from_utf8_lossy(&n)
                ));
            }
        };
        if let Err(e) = req.append_header(name, v) {
            return Err(format!(
                "请求头 {} 装不进去：{e}",
                String::from_utf8_lossy(&n)
            ));
        }
    }
    Ok(Box::new(req))
}

/// 把 [`ResponseHeader`] 翻成 h3 的头列表。**逐跳头在这里被滤掉。**
pub fn to_h3_headers(resp: &ResponseHeader) -> Vec<quiche::h3::Header> {
    let mut out = Vec::with_capacity(resp.headers.len() + 1);
    out.push(quiche::h3::Header::new(
        b":status",
        resp.status.as_str().as_bytes(),
    ));
    for (name, value) in resp.headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if FORBIDDEN_IN_H3.contains(&lower.as_str()) {
            debug!("[h3] 滤掉逐跳头 {lower}（RFC 9114 §4.2 在 HTTP/3 里禁止它）");
            continue;
        }
        out.push(quiche::h3::Header::new(lower.as_bytes(), value.as_bytes()));
    }
    out
}

/// 一条流待发的东西。
enum Out {
    Header(Box<ResponseHeader>, bool),
    Body(Vec<u8>, bool),
    Trailers(HeaderMap),
}

/// 一条流的出站状态。
#[derive(Default)]
struct StreamOut {
    queue: VecDeque<Out>,
    /// 已经把 fin 发出去了吗。★ 用来决定 `Finish` 要不要补一个空的 fin。
    fin_sent: bool,
}

/// 连接任务的本体。
#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut conn: quiche::Connection,
    sock: Arc<UdpSocket>,
    // ★ ★ **每个数据报自带 `(from, to)`**（**M2 批 K**）—— 而不是用建连时那一对。
    //   ⚠ 转交进来的数据报是**别的进程**收到的，它的 `from` 只能由报文自己带；
    //   ★ 顺带把「客户端换了地址」做对了：此前 `RecvInfo` 恒用建连时那一对。
    mut inbound: mpsc::Receiver<(Vec<u8>, SocketAddr, SocketAddr)>,
    local: SocketAddr,
    peer: SocketAddr,
    handler: Arc<dyn H3RequestHandler>,
) {
    let h3_cfg = match quiche::h3::Config::new() {
        Ok(c) => c,
        Err(e) => {
            warn!("[h3] 建不出 h3 配置：{e}");
            return;
        }
    };
    let mut h3: Option<quiche::h3::Connection> = None;
    // ★ ★ 这条连接的 TLS 摘要（**，D27 结案**）——**每条连接算一次**，
    //   逐流克隆（里面那份 `SslDigest` 是 `Arc`，克隆只是一次引用计数）。
    //   ⚠ 在 h3 层建起来的那一刻取，不能更早：握手没完时 `server_name()` /
    //     `application_proto()` 还没有值，而**那时取到的空值会被原样带进日志**。
    let mut tls_digest: Option<pingora_core::protocols::Digest> = None;
    let mut bodies: HashMap<u64, mpsc::Sender<ToSession>> = HashMap::new();
    let mut outs: HashMap<u64, StreamOut> = HashMap::new();
    let (out_tx, mut out_rx) = mpsc::channel::<(u64, FromSession)>(OUT_QUEUE);

    loop {
        // ① 握完手就把 h3 层建起来。
        if h3.is_none() && (conn.is_established() || conn.is_in_early_data()) {
            match quiche::h3::Connection::with_transport(&mut conn, &h3_cfg) {
                Ok(c) => {
                    let alpn = conn.application_proto();
                    info!(
                        "[h3] {peer} 的 h3 层起来了（ALPN={}）",
                        String::from_utf8_lossy(alpn)
                    );
                    // ★ 空的 ALPN 记成「没协商出来」而不是空串 —— 与 h1/h2 那侧
                    //   `selected_alpn_protocol()` 回 `None` 的形状对齐。
                    tls_digest = Some(crate::quic::h3_session::quic_digest(
                        conn.server_name().map(str::to_string),
                        (!alpn.is_empty()).then(|| String::from_utf8_lossy(alpn).into_owned()),
                    ));
                    h3 = Some(c);
                }
                Err(e) => {
                    debug!("[h3] {peer} 建不出 h3 连接：{e}");
                    conn.close(true, 0x10c, b"h3 setup failed").ok();
                }
            }
        }

        // ② 把 h3 事件全 poll 出来。★ 必须 poll 到 Done。
        if let Some(h) = h3.as_mut() {
            loop {
                match h.poll(&mut conn) {
                    Ok((sid, ev)) => {
                        on_event(
                            sid,
                            ev,
                            h,
                            &mut conn,
                            &mut bodies,
                            &mut outs,
                            &out_tx,
                            &handler,
                            peer,
                            local,
                            tls_digest.as_ref(),
                        )
                        .await;
                    }
                    Err(quiche::h3::Error::Done) => break,
                    Err(e) => {
                        debug!("[h3] {peer} poll 出错：{e}");
                        break;
                    }
                }
            }
            // ③ 把待发的东西尽量写出去（`send_body` 会短写）。
            drain_all(h, &mut conn, &mut outs);
        }

        // ④ 把该发的数据报发出去。
        flush(&mut conn, &sock).await;

        if conn.is_closed() {
            debug!("[h3] {peer} 的连接已关闭：{:?}", conn.stats());
            return;
        }

        // ⑤ 等下一件事。
        tokio::select! {
            got = inbound.recv() => match got {
                Some((mut pkt, from, to)) => {
                    let info = quiche::RecvInfo { from, to };
                    if let Err(e) = conn.recv(&mut pkt, info) {
                        debug!("[h3] {peer} recv 出错：{e}");
                    }
                    // 一次可能来了好几个，趁热都收掉，省一轮 select。
                    // ⚠ 每一个都用**它自己**那一对地址 —— 一批里可能既有本代直收的、
                    //   也有转交进来的，而它们的 `from` 不一定相同。
                    while let Ok((mut more, f2, t2)) = inbound.try_recv() {
                        let _ = conn.recv(&mut more, quiche::RecvInfo { from: f2, to: t2 });
                    }
                }
                None => {
                    conn.close(false, 0x0, b"listener gone").ok();
                    flush(&mut conn, &sock).await;
                    return;
                }
            },
            Some((sid, msg)) = out_rx.recv() => {
                enqueue(&mut outs, sid, msg);
                // 同样趁热多收几条，避免一条响应被拆成很多轮。
                while let Ok((sid2, msg2)) = out_rx.try_recv() {
                    enqueue(&mut outs, sid2, msg2);
                }
            },
            _ = sleep_opt(conn.timeout()) => conn.on_timeout(),
        }
    }
}

/// 把一条会话消息塞进它那条流的待发队列。
fn enqueue(outs: &mut HashMap<u64, StreamOut>, sid: u64, msg: FromSession) {
    let s = outs.entry(sid).or_default();
    debug!(
        "[h3] 流 {sid} 入队：{}（队里已有 {}，fin_sent={}）",
        match &msg {
            FromSession::Header { .. } => "Header",
            FromSession::Body { .. } => "Body",
            FromSession::Trailers(_) => "Trailers",
            FromSession::Finish => "Finish",
        },
        s.queue.len(),
        s.fin_sent
    );
    match msg {
        FromSession::Header { resp, end } => s.queue.push_back(Out::Header(resp, end)),
        FromSession::Body { data, end } => s.queue.push_back(Out::Body(data.to_vec(), end)),
        FromSession::Trailers(t) => s.queue.push_back(Out::Trailers(t)),
        FromSession::Finish => {
            // ★ 会话收摊了。**只有在还没发过 fin 时**才补一个空的 fin ——
            //   重复发 fin 是协议错误。
            if !s.fin_sent && s.queue.is_empty() {
                s.queue.push_back(Out::Body(Vec::new(), true));
            }
        }
    }
}

/// 尽量把每条流的待发队列写出去。写不完的留到下一轮。
fn drain_all(
    h: &mut quiche::h3::Connection,
    conn: &mut quiche::Connection,
    outs: &mut HashMap<u64, StreamOut>,
) {
    outs.retain(|&sid, s| {
        while let Some(front) = s.queue.front_mut() {
            match front {
                Out::Header(resp, end) => {
                    let hs = to_h3_headers(resp);
                    match h.send_response(conn, sid, &hs, *end) {
                        Ok(()) => {
                            s.fin_sent |= *end;
                            s.queue.pop_front();
                        }
                        // 流被挡住了 —— 下一轮再来，**不是错误**。
                        Err(quiche::h3::Error::StreamBlocked) | Err(quiche::h3::Error::Done) => {
                            return true;
                        }
                        Err(e) => {
                            debug!("[h3] 流 {sid} 发响应头失败：{e}");
                            s.queue.clear();
                            return false;
                        }
                    }
                }
                Out::Body(data, end) => {
                    match h.send_body(conn, sid, data, *end) {
                        // ★ ★ 短写：只写进去 n 个字节，剩下的留着。
                        Ok(n) if n < data.len() => {
                            data.drain(..n);
                            return true;
                        }
                        Ok(_) => {
                            s.fin_sent |= *end;
                            s.queue.pop_front();
                        }
                        Err(quiche::h3::Error::Done) => return true,
                        Err(e) => {
                            debug!("[h3] 流 {sid} 发体失败：{e}");
                            s.queue.clear();
                            return false;
                        }
                    }
                }
                Out::Trailers(t) => {
                    let hs: Vec<quiche::h3::Header> = t
                        .iter()
                        .map(|(n, v)| {
                            quiche::h3::Header::new(
                                n.as_str().to_ascii_lowercase().as_bytes(),
                                v.as_bytes(),
                            )
                        })
                        .collect();
                    match h.send_additional_headers(conn, sid, &hs, true, true) {
                        Ok(()) => {
                            s.fin_sent = true;
                            s.queue.pop_front();
                        }
                        Err(quiche::h3::Error::Done) => return true,
                        Err(e) => {
                            debug!("[h3] 流 {sid} 发 trailer 失败：{e}");
                            s.queue.clear();
                            return false;
                        }
                    }
                }
            }
        }
        true
    });
}

/// 处理一个 h3 事件。
#[allow(clippy::too_many_arguments)]
async fn on_event(
    sid: u64,
    ev: quiche::h3::Event,
    h: &mut quiche::h3::Connection,
    conn: &mut quiche::Connection,
    bodies: &mut HashMap<u64, mpsc::Sender<ToSession>>,
    outs: &mut HashMap<u64, StreamOut>,
    out_tx: &mpsc::Sender<(u64, FromSession)>,
    handler: &Arc<dyn H3RequestHandler>,
    peer: SocketAddr,
    local: SocketAddr,
    tls_digest: Option<&pingora_core::protocols::Digest>,
) {
    // ⚠ ⚠ **每一个 h3 事件都记一笔**（批 K 排查时补的）。
    //   ★ 少了它，「第二个请求到底有没有到」与「到了但没被处理」分不开 ——
    //     而那两件事要查的地方完全不同。
    debug!("[h3] {peer} 流 {sid} 事件：{}", event_name(&ev));
    match ev {
        quiche::h3::Event::Headers { list, more_frames } => {
            let req = match to_request_header(&list) {
                Ok(r) => r,
                Err(why) => {
                    debug!("[h3] {peer} 流 {sid} 的请求头不合法（{why}），重置这条流");
                    // 0x0105 = H3_MESSAGE_ERROR
                    let _ = conn.stream_shutdown(sid, quiche::Shutdown::Read, 0x0105);
                    let _ = conn.stream_shutdown(sid, quiche::Shutdown::Write, 0x0105);
                    return;
                }
            };
            let (body_tx, body_rx) = mpsc::channel(BODY_QUEUE);
            if more_frames {
                bodies.insert(sid, body_tx);
            }
            outs.entry(sid).or_default();
            let session = H3Session::new(
                req,
                more_frames,
                Some(peer),
                Some(local),
                body_rx,
                out_tx.clone(),
                sid,
                tls_digest.cloned(),
            );
            let handler = handler.clone();
            // ★ 每条请求一个任务：执行链是 async 的，而连接任务不能被它挡住 ——
            //   一条慢请求会拖住同一条连接上的其它流（h3 的整个卖点就是它们互不阻塞）。
            tokio::spawn(async move {
                handler
                    .handle(ServerSession::new_custom(Box::new(session)))
                    .await;
            });
        }
        quiche::h3::Event::Data => {
            // ★ ★ **必须读到 Done**：这个事件不会重新武装。
            let mut chunk = vec![0u8; BODY_CHUNK];
            loop {
                match h.recv_body(conn, sid, &mut chunk) {
                    Ok(n) => {
                        if let Some(tx) = bodies.get(&sid) {
                            // ⚠ 会话侧读得慢时这里会等 —— 那是**对的**，它就是背压。
                            if tx
                                .send(ToSession::Body(Bytes::copy_from_slice(&chunk[..n])))
                                .await
                                .is_err()
                            {
                                bodies.remove(&sid);
                                break;
                            }
                        }
                    }
                    Err(quiche::h3::Error::Done) => break,
                    Err(e) => {
                        debug!("[h3] 流 {sid} 读体出错：{e}");
                        if let Some(tx) = bodies.remove(&sid) {
                            let _ = tx.send(ToSession::Reset(e.to_string())).await;
                        }
                        break;
                    }
                }
            }
        }
        quiche::h3::Event::Finished => {
            if let Some(tx) = bodies.remove(&sid) {
                let _ = tx.send(ToSession::End).await;
            }
        }
        quiche::h3::Event::Reset(code) => {
            debug!("[h3] {peer} 重置了流 {sid}（code={code}）");
            if let Some(tx) = bodies.remove(&sid) {
                let _ = tx
                    .send(ToSession::Reset(format!("对端重置，code={code}")))
                    .await;
            }
            outs.remove(&sid);
        }
        // ★ 这两个本批不处理，但**要说出来** —— 静默忽略与「处理过了」读起来一样。
        quiche::h3::Event::PriorityUpdate => {
            debug!("[h3] {peer} 流 {sid} 的优先级更新（本批不据它排序）");
        }
        quiche::h3::Event::GoAway => {
            debug!("[h3] {peer} 发来 GOAWAY");
        }
    }
}

/// 把连接里排着的包全发出去。
async fn flush(conn: &mut quiche::Connection, sock: &UdpSocket) {
    let mut out = vec![0u8; DATAGRAM_MAX];
    loop {
        match conn.send(&mut out) {
            Ok((n, info)) => {
                if let Err(e) = sock.send_to(&out[..n], info.to).await {
                    debug!("[h3] 发包失败（{}）：{e}", info.to);
                    return;
                }
            }
            Err(quiche::Error::Done) => return,
            Err(e) => {
                debug!("[h3] send 出错：{e}");
                conn.close(false, 0x1, b"send failed").ok();
                return;
            }
        }
    }
}

/// 一个 h3 事件的名字，只给日志用。
///
/// ★ 写成穷尽 `match` 而不是 `{ev:?}`：加一种事件时这里会**编不过**，
/// 而那正好是「有一种新事件我们没处理」该被看见的时刻。
fn event_name(ev: &quiche::h3::Event) -> &'static str {
    match ev {
        quiche::h3::Event::Headers { .. } => "Headers",
        quiche::h3::Event::Data => "Data",
        quiche::h3::Event::Finished => "Finished",
        quiche::h3::Event::Reset(_) => "Reset",
        quiche::h3::Event::PriorityUpdate => "PriorityUpdate",
        quiche::h3::Event::GoAway => "GoAway",
    }
}

/// `Option<Duration>` 的睡眠：`None` ⇒ **永远不就绪**。
///
/// ⚠ 写成 `sleep(d.unwrap_or_default())` 会在没有定时器时变成 0 秒睡眠 ⇒ 忙等，
/// 而症状是「某条连接把一个核吃满」，看起来完全不像定时器的问题。
async fn sleep_opt(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// 头翻译层的判据。**纯函数，不碰网络。**
#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: &str, v: &str) -> quiche::h3::Header {
        quiche::h3::Header::new(n.as_bytes(), v.as_bytes())
    }

    #[test]
    fn 伪头翻成请求行而_authority_落成_host() {
        let req = to_request_header(&[
            h(":method", "GET"),
            h(":scheme", "https"),
            h(":authority", "example.com"),
            h(":path", "/a?b=1"),
            h("user-agent", "t"),
        ])
        .expect("翻译");
        assert_eq!(req.method, "GET");
        assert_eq!(req.uri.path(), "/a");
        assert_eq!(req.uri.query(), Some("b=1"));
        // ★ 站点索引读的是 Host，而 h3 上只有 :authority —— 这一步是它唯一的来源。
        assert_eq!(
            req.headers.get("host").map(|v| v.as_bytes()),
            Some(&b"example.com"[..])
        );
        assert_eq!(
            req.headers.get("user-agent").map(|v| v.as_bytes()),
            Some(&b"t"[..])
        );
    }

    #[test]
    fn 缺了必需的伪头要拒绝而不是拼一个出来() {
        assert!(to_request_header(&[h(":path", "/")]).is_err(), "少 :method");
        assert!(
            to_request_header(&[h(":method", "GET")]).is_err(),
            "少 :path"
        );
    }

    /// RFC 9114 §4.3.1：认不得的伪头是格式错误，**不能当普通头收下**。
    #[test]
    fn 认不得的伪头要拒绝() {
        let e = to_request_header(&[h(":method", "GET"), h(":path", "/"), h(":bogus", "x")]);
        assert!(e.is_err(), "认不得的伪头被收下了");
    }

    /// h3 那侧只保证头名是字节串 —— 非法头名要挡在门外。
    #[test]
    fn 非法头名要拒绝而不是_panic() {
        let bad = quiche::h3::Header::new(b"a b", b"v");
        assert!(to_request_header(&[h(":method", "GET"), h(":path", "/"), bad]).is_err());
    }

    /// ★★★ RFC 9114 §4.2：逐跳头在 HTTP/3 里是禁止的 ——
    /// 发出去**不是被忽略，是对端重置这条流**。而我们的执行链是为 h1/h2 写的。
    #[test]
    fn 逐跳头必须被滤掉() {
        let mut resp = ResponseHeader::build(200, None).expect("造响应头");
        resp.insert_header("connection", "keep-alive").expect("插");
        resp.insert_header("keep-alive", "timeout=60").expect("插");
        resp.insert_header("transfer-encoding", "chunked")
            .expect("插");
        resp.insert_header("upgrade", "h2c").expect("插");
        resp.insert_header("proxy-connection", "x").expect("插");
        resp.insert_header("te", "trailers").expect("插");
        // 一条正常的头，用来证明这个过滤器**不是把所有东西都滤掉**。
        resp.insert_header("content-type", "text/plain")
            .expect("插");

        let hs = to_h3_headers(&resp);
        let names: Vec<String> = hs
            .iter()
            .map(|x| String::from_utf8_lossy(x.name()).into_owned())
            .collect();

        assert_eq!(names[0], ":status", ":status 必须在第一条");
        for bad in FORBIDDEN_IN_H3 {
            assert!(
                !names.iter().any(|n| n == bad),
                "逐跳头 {bad} 没被滤掉 —— 发出去对端会重置这条流"
            );
        }
        // ★ 反向的一半：正常的头不能跟着被滤掉，否则这条判据与「全滤掉」无法区分。
        assert!(
            names.iter().any(|n| n == "content-type"),
            "正常的头也被滤掉了"
        );
    }

    #[test]
    fn 状态码原样落进冒号_status() {
        let resp = ResponseHeader::build(404, None).expect("造响应头");
        let hs = to_h3_headers(&resp);
        assert_eq!(hs[0].name(), b":status");
        assert_eq!(hs[0].value(), b"404");
    }

    /// ★ 头名一律小写：h3/QPACK 要求如此，大写会被对端当格式错误。
    #[test]
    fn 头名一律转小写() {
        let mut resp = ResponseHeader::build(200, None).expect("造响应头");
        resp.insert_header("X-Fulcrum-Test", "1").expect("插");
        let hs = to_h3_headers(&resp);
        assert!(
            hs.iter()
                .any(|x| x.name() == b"x-fulcrum-test" && x.value() == b"1"),
            "头名没转小写：{:?}",
            hs.iter()
                .map(|x| String::from_utf8_lossy(x.name()).into_owned())
                .collect::<Vec<_>>()
        );
    }
}
