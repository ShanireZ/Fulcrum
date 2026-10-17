//! **`quiche::h3` 事件循環**（M2 批 J 第五步）——一條 QUIC 連接的任務本體。
//!
//! ```text
//!   UDP 数据报 ──▶ 连接任务（独占 quiche::Connection + quiche::h3::Connection）
//!                    │                                   ▲
//!                    │ Headers/Data/Finished             │ (stream_id, FromSession)
//!                    ▼                                   │
//!                 H3Session ──ServerSession::new_custom──▶ H3RequestHandler
//! ```
//!
//! # 三條這一層必須自己負責的事（h3 與 h1 在這裡**不一樣**）
//!
//! 1. ★ **`recv_body` 要讀到 `Done` 為止** —— quiche 的 `Data` 事件**不會重新武裝**，
//!    少讀一次，這條流剩下的體就永遠不會再來一個事件通知你。
//! 2. ★ ★ **`send_body` 會短寫**：它回的是「這次寫進去幾個位元組」，流控擋住時會小於給它的長度。
//!    ⇒ 每條流留一個待發佇列，寫不完就留著下一輪再寫。
//!    ⚠ 不留的話，**大回應會被靜默截斷** —— 而用戶端看到的是「連接好好的，內容少了一截」。
//! 3. ★ ★ ★ **逐跳頭（`Connection` / `Keep-Alive` / `Transfer-Encoding` / `Upgrade` /
//!    `Proxy-Connection`）在 HTTP/3 裡是禁止的**（RFC 9114 §4.2）——發出去對端會重置這條流。
//!    ⚠ 而我們的執行鏈是為 h1/h2 寫的，**它完全可能帶上這些頭**。⇒ 這一層負責濾掉。
//!
//! [`H3RequestHandler`] 由 `crate::FulcrumApp` 實作 —— **同一個 app 執行個體、同一條執行鏈**，
//! 與 h1/h2 的唯一差別是不發 `Alt-Svc`。
//!
//! ⚠ ★ 上面第 3 條的真實危險面來自使用者設定：一句 `header Connection "keep-alive"`
//! 在 h1 上完全合法。⇒ `tests/h3/run.sh` 拿的正是這條設定，並且**同時**驗 h1 上
//! 那個頭真的在 —— 沒有那條對照，「h3 上沒有它」什麼也證明不了。

use crate::quic::h3_session::{FromSession, H3Session, ToSession};
use async_trait::async_trait;
use bytes::Bytes;
use http::HeaderMap;
use log::{debug, info, warn};
use pingora_core::protocols::http::ServerSession;
// ★ `h3::Header` 的 `name()` / `value()` 掛在這個 trait 上，不 import 就調不著。
use pingora_http::{RequestHeader, ResponseHeader};
use quiche::h3::NameValue;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// 一個 UDP 資料報的上限（與 [`super::listener`] 那份同值，理由也一樣）。
const DATAGRAM_MAX: usize = 65535;
/// 一次從 h3 讀體的塊大小。
const BODY_CHUNK: usize = 16 * 1024;
/// 每條連接的出站佇列深度。★ 它同時是**背壓**：滿了之後工作階段側的 `write_body` 會等。
const OUT_QUEUE: usize = 256;
/// 每條流的請求體佇列深度。
const BODY_QUEUE: usize = 32;

/// 一條 h3 請求交給誰去跑。
///
/// ★ ★ **這一層存在的理由是方向**：`quic` 模組不該知道 `FulcrumApp` ——
/// 那會讓 HTTP/3 入口反向相依資料面本體，而資料面本體又要相依這個入口。
/// ⇒ 入口只認這個 trait，誰來實作由串接那一步決定（G110）。
#[async_trait]
pub trait H3RequestHandler: Send + Sync + 'static {
    async fn handle(&self, session: ServerSession);
}

/// **HTTP/3 裡禁止出現的逐跳頭**（RFC 9114 §4.2）。
///
/// ⚠ ⚠ ★ 發出去不是「對端忽略它」，是**對端按協定錯誤重置這條流** ——
/// 而我們的執行鏈是為 h1/h2 寫的，它完全可能帶上這些頭。
/// ★ `TE` 是個例外：RFC 9114 允許它，但**只允許值恰好是 `trailers`**。
/// 這裡的處置是**一律濾掉** —— 少發一個 `TE` 不會讓任何東西壞掉，
/// 而發錯一個會把整條流打掉。
const FORBIDDEN_IN_H3: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "te",
];

/// 把 h3 的請求頭列表翻成執行鏈認得的 [`RequestHeader`]。
///
/// ⚠ `:authority` 落成 `host` 頭 —— 我們的站台索引讀的是 `Host`
/// （`lib.rs::host_of`），h3 上沒有那個頭，只有偽頭。
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
            // `:scheme` 我們不用：入口本身就決定了它是 https。
            b":scheme" | b":protocol" => {}
            // ⚠ 認不得的偽頭一律拒絕：RFC 9114 §4.3.1 明確要求把它當格式錯誤。
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

    // ★ `:authority` 先落，再落普通頭 —— 萬一對端**同時**給了 `host`，
    //   後者會覆蓋它，與 h2 那側的慣例一致。
    if let Some(a) = authority
        && let Err(e) = req.insert_header("host", a.as_slice())
    {
        return Err(format!(":authority 装不进 host 头：{e}"));
    }
    for (n, v) in normal {
        // ⚠ `append_header` 收的是 `HeaderName`，不收裸位元組 —— 而這一步順帶把
        //   **非法頭名擋在門外**：h3 那側只保證它是位元組串。
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

/// 把 [`ResponseHeader`] 翻成 h3 的頭列表。**逐跳頭在這裡被濾掉。**
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

/// 一條流待發的東西。
enum Out {
    Header(Box<ResponseHeader>, bool),
    Body(Vec<u8>, bool),
    Trailers(HeaderMap),
}

/// 一條流的出站狀態。
#[derive(Default)]
struct StreamOut {
    queue: VecDeque<Out>,
    /// 已經把 fin 發出去了嗎。★ 用來決定 `Finish` 要不要補一個空的 fin。
    fin_sent: bool,
}

/// 連接任務的本體。
#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut conn: quiche::Connection,
    sock: Arc<UdpSocket>,
    // ★ ★ **每個資料報自帶 `(from, to)`**（**M2 批 K**）—— 而不是用建連時那一對。
    //   ⚠ 轉交進來的資料報是**別的處理程序**收到的，它的 `from` 只能由訊息自己帶；
    //   ★ 順帶把「用戶端換了位址」做對了：此前 `RecvInfo` 恆用建連時那一對。
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
    // ★ ★ 這條連接的 TLS 摘要（**G128，D27 結案**）——**每條連接算一次**，
    //   逐流克隆（裡面那份 `SslDigest` 是 `Arc`，克隆只是一次引用計數）。
    //   ⚠ 在 h3 層建起來的那一刻取，不能更早：握手沒完時 `server_name()` /
    //     `application_proto()` 還沒有值，而**那時取到的空值會被原樣帶進紀錄**。
    let mut tls_digest: Option<pingora_core::protocols::Digest> = None;
    let mut bodies: HashMap<u64, mpsc::Sender<ToSession>> = HashMap::new();
    let mut outs: HashMap<u64, StreamOut> = HashMap::new();
    let (out_tx, mut out_rx) = mpsc::channel::<(u64, FromSession)>(OUT_QUEUE);

    loop {
        // ① 握完手就把 h3 層建起來。
        if h3.is_none() && (conn.is_established() || conn.is_in_early_data()) {
            match quiche::h3::Connection::with_transport(&mut conn, &h3_cfg) {
                Ok(c) => {
                    let alpn = conn.application_proto();
                    info!(
                        "[h3] {peer} 的 h3 层起来了（ALPN={}）",
                        String::from_utf8_lossy(alpn)
                    );
                    // ★ 空的 ALPN 記成「沒協商出來」而不是空串 —— 與 h1/h2 那側
                    //   `selected_alpn_protocol()` 回 `None` 的形狀對齊。
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

        // ② 把 h3 事件全 poll 出來。★ 必須 poll 到 Done。
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
            // ③ 把待發的東西儘量寫出去（`send_body` 會短寫）。
            drain_all(h, &mut conn, &mut outs);
        }

        // ④ 把該發的資料報發出去。
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
                    // 一次可能來了好幾個，趁熱都收掉，省一輪 select。
                    // ⚠ 每一個都用**它自己**那一對位址 —— 一批裡可能既有本代直收的、
                    //   也有轉交進來的，而它們的 `from` 不一定相同。
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
                // 同樣趁熱多收幾條，避免一條回應被拆成很多輪。
                while let Ok((sid2, msg2)) = out_rx.try_recv() {
                    enqueue(&mut outs, sid2, msg2);
                }
            },
            _ = sleep_opt(conn.timeout()) => conn.on_timeout(),
        }
    }
}

/// 把一條工作階段訊息塞進它那條流的待發佇列。
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
            // ★ 工作階段收攤了。**只有在還沒發過 fin 時**才補一個空的 fin ——
            //   重複發 fin 是協定錯誤。
            if !s.fin_sent && s.queue.is_empty() {
                s.queue.push_back(Out::Body(Vec::new(), true));
            }
        }
    }
}

/// 儘量把每條流的待發佇列寫出去。寫不完的留到下一輪。
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
                        // 流被擋住了 —— 下一輪再來，**不是錯誤**。
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
                        // ★ ★ 短寫：只寫進去 n 個位元組，剩下的留著。
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

/// 處理一個 h3 事件。
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
    // ⚠ ⚠ **每一個 h3 事件都記一筆**（批 K 排查時補的）。
    //   ★ 少了它，「第二個請求到底有沒有到」與「到了但沒被處理」分不開 ——
    //     而那兩件事要查的地方完全不同。
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
            // ★ 每條請求一個任務：執行鏈是 async 的，而連接任務不能被它擋住 ——
            //   一條慢請求會拖住同一條連接上的其它流（h3 的整個賣點就是它們互不阻塞）。
            tokio::spawn(async move {
                handler
                    .handle(ServerSession::new_custom(Box::new(session)))
                    .await;
            });
        }
        quiche::h3::Event::Data => {
            // ★ ★ **必須讀到 Done**：這個事件不會重新武裝。
            let mut chunk = vec![0u8; BODY_CHUNK];
            loop {
                match h.recv_body(conn, sid, &mut chunk) {
                    Ok(n) => {
                        if let Some(tx) = bodies.get(&sid) {
                            // ⚠ 工作階段側讀得慢時這裡會等 —— 那是**對的**，它就是背壓。
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
        // ★ 這兩個本批不處理，但**要說出來** —— 靜默忽略與「處理過了」讀起來一樣。
        quiche::h3::Event::PriorityUpdate => {
            debug!("[h3] {peer} 流 {sid} 的优先级更新（本批不据它排序）");
        }
        quiche::h3::Event::GoAway => {
            debug!("[h3] {peer} 发来 GOAWAY");
        }
    }
}

/// 把連接裡排著的包全發出去。
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

/// 一個 h3 事件的名字，只給紀錄用。
///
/// ★ 寫成窮盡 `match` 而不是 `{ev:?}`：加一種事件時這裡會**編不過**，
/// 而那正好是「有一種新事件我們沒處理」該被看見的時刻。
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

/// `Option<Duration>` 的睡眠：`None` ⇒ **永遠不就緒**。
///
/// ⚠ 寫成 `sleep(d.unwrap_or_default())` 會在沒有定時器時變成 0 秒睡眠 ⇒ 忙等，
/// 而症狀是「某條連接把一個核吃滿」，看起來完全不像定時器的問題。
async fn sleep_opt(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// 頭翻譯層的判據。**純函式，不碰網路。**
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
        // ★ 站台索引讀的是 Host，而 h3 上只有 :authority —— 這一步是它唯一的來源。
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

    /// RFC 9114 §4.3.1：認不得的偽頭是格式錯誤，**不能當普通頭收下**。
    #[test]
    fn 认不得的伪头要拒绝() {
        let e = to_request_header(&[h(":method", "GET"), h(":path", "/"), h(":bogus", "x")]);
        assert!(e.is_err(), "认不得的伪头被收下了");
    }

    /// h3 那側只保證頭名是位元組串 —— 非法頭名要擋在門外。
    #[test]
    fn 非法头名要拒绝而不是_panic() {
        let bad = quiche::h3::Header::new(b"a b", b"v");
        assert!(to_request_header(&[h(":method", "GET"), h(":path", "/"), bad]).is_err());
    }

    /// ★★★ RFC 9114 §4.2：逐跳頭在 HTTP/3 裡是禁止的 ——
    /// 發出去**不是被忽略，是對端重置這條流**。而我們的執行鏈是為 h1/h2 寫的。
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
        // 一條正常的頭，用來證明這個過濾器**不是把所有東西都濾掉**。
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
        // ★ 反向的一半：正常的頭不能跟著被濾掉，否則這條判據與「全濾掉」無法區分。
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

    /// ★ 頭名一律小寫：h3/QPACK 要求如此，大寫會被對端當格式錯誤。
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
