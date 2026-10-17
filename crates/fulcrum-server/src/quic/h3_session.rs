//! **h3 → 現有執行鏈的那座橋**（M2 批 J）。
//!
//! 接縫是 `HttpServerApp` / `ServerSession` —— 後者是個 **enum**，帶
//! `Custom(Box<dyn SessionCustom>)` 變體與公開的 `Session::new_custom()`。
//! ⇒ 橋的形狀：**為 h3 實作 `SessionCustom`，包起來原樣餵給現有的 `serve_one`。**
//! （⚠ 別信「執行鏈是為 `ProxyHttp` 建的」那種說法 —— `ProxyHttp` 在本儲存庫零命中。）
//!
//! ⚠ **代價寫在明處**：那個 trait 在上游標著 `#[doc(hidden)]`，上游可以不加 semver 就改。
//! 我們 vendor 了 pingora 所以換代時機可控，但 rebase 時這一頁要跟著看。
//! ★ 與 fork「加能力」不同：**這裡一行 fork 都沒改。**
//!
//! # 形狀：連接任務持有 quiche，工作階段只拿兩根管子
//!
//! `SessionCustom` 要求 `Send + Sync + Unpin + 'static`，而 `quiche::Connection`
//! 是單執行緒的、由連接任務獨佔。⇒ 每條請求給一個 [`H3Session`]，它與連接任務之間
//! 只有兩條 mpsc：請求體進來、回應出去。
//!
//! ```text
//!   连接任务（独占 quiche::Connection + quiche::h3::Connection）
//!        │  ToSession::Body/End/Reset          ▲  FromSession::Header/Body/Trailers/Finish
//!        ▼                                     │
//!   H3Session  ──包进 ServerSession::new_custom──▶  FulcrumApp::serve_one（**一行没改**）
//! ```
//!
//! # ⚠ 有幾個方法**不兌現它名字的承諾**，而這一條有門看著
//!
//! [`NOT_HONORED`] 那幾個是「有定義的行為 + 安全回傳值」，**不寫 `unimplemented!()`** ——
//! 照上游那份全 `unreachable!()` 的 stub 會**編得過、門也會綠**，然後在某條今天沒人走的
//! 路徑上炸。⇒ 每一個都回傳定義好的安全值，並由本頁末尾那道門釘住
//! 「**現有執行鏈不許調它們**」。

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
// ★ h3 的 TLS 摘要（D27 結案）。⚠ 經 `pingora_core` 的 re-export 拿，
//   與 `Digest` 同一條紀律。
use pingora_core::protocols::tls::SslDigest;
// ★ 經 `pingora_core` 的 re-export 拿，而不是再加一條 `pingora-error` 直接依賴 ——
//   與「BoringSSL 類型只經 pingora-boringssl 拿」同一條紀律（G111 破的是那一條，不是這一條）。
use pingora_core::{Error, ErrorType, Result};
use pingora_http::{RequestHeader, ResponseHeader};
use std::time::Duration;
use tokio::sync::mpsc;

/// 連接任務 → 工作階段。
#[derive(Debug)]
pub enum ToSession {
    /// 一段請求體。
    Body(Bytes),
    /// 請求體到此結束（h3 的 `Finished` 事件）。
    End,
    /// 流或連接壞了，工作階段該收攤。
    Reset(String),
}

/// 工作階段 → 連接任務。
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
    /// 工作階段側收攤了（正常結束或被掐斷）。
    Finish,
}

/// **本批不兌現其名字承諾的方法**。
///
/// ⚠ ⚠ 這不是「以後再說」的清單，它是**判據的輸入** —— 本頁末尾那道門會掃現有執行鏈，
/// 一旦有人開始調它們就判紅。⇒ 加一條到這裡，等於給它配一道門；從這裡刪一條，
/// 等於聲稱它已經真的實作了。
///
/// | 方法 | 本批的行為 | 為什麼可以是這樣 |
/// |---|---|---|
/// | `enable_retry_buffering` | 空操作 | 我們不做請求體重放（換上游重試）|
/// | `retry_buffer_truncated` | `false` | 與 h2 在「從沒開過緩衝」時同值 |
/// | `get_retry_buffer` | `None` | 同上：沒有緩衝 ⇒ 呼叫方不會拿它去重試 |
/// | ~~`digest` / `digest_mut`~~ | ✅ **已兌現（D27 結案）** | ⚠ ★ ★ 曾經的理由是「pingora 的 `Digest` 是給 h1/h2 的連接摘要，h3 這側沒有對應物」——**那句話是錯的**：h3 連接一樣有 TLS 版本、SNI 與 ALPN，缺的只是一條把它們送出去的路。⇒ [`quic_digest`] 現造一份，`digest()` / `digest_mut()` 真的回傳它，**兩個名字都離開了這張表**。⚠ 只有 `cipher` 仍是空的（quiche 的 `Handshake::cipher()` 鎖在私有 `mod tls` 裡）|
/// | `take_custom_message_*` | `None` | 「自定義訊息」通道本儲存庫一處都沒用 |
/// | `restore_custom_message_*` | `Err` | ★ 恢復一個從來沒被取走的東西是**呼叫方的邏輯錯**，不能靜默成功 |
pub const NOT_HONORED: &[&str] = &[
    "enable_retry_buffering",
    "retry_buffer_truncated",
    "get_retry_buffer",
    // ★ ★ ★ `digest` / `digest_mut` **於 G128（§10，D27 結案）
    //   從這張表上刪掉了** —— 不是「讓門閉嘴」，是它們**真的被實作出來了**。
    //   ⚠ 判據：`不兑现承诺的那一组回安全值而不是_panic` 現在斷言 `digest().is_some()`，
    //     而訪問紀錄那一格（`tests/log/run.sh`）在**真的 h3 請求**上量 `tls_*` 三格。
    "take_custom_message_reader",
    "restore_custom_message_reader",
    "take_custom_message_writer",
    "restore_custom_message_writer",
];

/// 一條 h3 請求，裝成現有執行鏈認得的樣子。
pub struct H3Session {
    req: Box<RequestHeader>,
    client: Option<SocketAddr>,
    server: Option<SocketAddr>,
    body_rx: mpsc::Receiver<ToSession>,
    /// ★ ★ **一條連接上的所有流共用同一個出站通道**，所以每條訊息都要帶 stream_id。
    /// ⚠ 每條流各開一個通道的話，連接任務就要在**動態數量**的接收端上 select ——
    /// 那是「聽起來更乾淨」與「寫得出來」不一致的地方。
    out: mpsc::Sender<(u64, FromSession)>,
    /// 這條請求在 QUIC 上的流號。
    stream_id: u64,
    written: Option<ResponseHeader>,
    /// h3 的 `Headers` 事件自帶 `has_body`，所以這一條**開局就知道**，
    /// 不像 h1 要等讀一次才分得清。
    body_empty: bool,
    body_done: bool,
    read_n: usize,
    sent_n: usize,
    finished: bool,
    // ⚠ 下面三個**存得下、而本批沒有任何人照著它做事** —— 超時歸連接任務管
    //   （quiche 的定時器是連接級的，不是流級的）。它們也在 NOT_HONORED 的精神之內，
    //   但**沒有列進去**：列進去會讓那道門去禁止一件將來必然要做的事。
    //   ⇒ 留一條 TODO 比留一道假門好。
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    drain_timeout: Option<Duration>,
    /// 這條 QUIC 連接的 TLS 摘要（**G128，D27 結案**）。
    ///
    /// ★ 由 [`quic_digest`] 在**連接**那一層造一份，每條流克隆一個
    /// （`Digest` 裡那份 `SslDigest` 是 `Arc`，克隆只是一次引用計數）。
    /// ⚠ `None` 只會出現在單測裡 —— 真流量上 h3 層建起來時它一定有值。
    digest: Option<Digest>,
}

/// 給一條 QUIC 連接造一份 `Digest`（**G128，D27 結案**）。
///
/// # ★ ★ 三格從哪來，第四格為什麼沒有
///
/// | 格 | 來源 |
/// |---|---|
/// | `version` | **恆為 `TLSv1.3`** —— RFC 9001 §4.2 寫死了 QUIC 只能用 TLS 1.3。★ 這不是猜，是規範 |
/// | `sni` | `quiche::Connection::server_name()` |
/// | `alpn` | `quiche::Connection::application_proto()` |
/// | `cipher` | ⚠ **拿不到**：quiche 的 `Handshake::cipher()` 在**私有** `mod tls` 裡，而 quiche 一個 TLS 出口都沒有 re-export ⇒ 留空 = 那一格不出現 |
///
/// ★ 與 h1/h2 那側**匯到同一個類型**（`SslDigest`），於是訪問紀錄那一層
/// 一行分支都不用寫 —— 「同一格資料兩個填法」在結構上做不到。
pub fn quic_digest(sni: Option<String>, alpn: Option<String>) -> Digest {
    let mut ssl = SslDigest::new("", "TLSv1.3", None, None, Vec::new());
    // ★ ★ `sni` / `alpn` 是 **fork 改動 14** 給 `SslDigest` 加的兩格 ——
    //   上游沒有它們，見 `vendor/pingora/FORK.md`。
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

    /// 往連接任務發一條。
    ///
    /// ⚠ 對端沒了 ⇒ 連接已經塌了，這是 `WriteError` 而不是內部錯誤 ——
    /// **兩者在紀錄裡必須分得開**：前者是網路的常態，後者是我們的缺陷。
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
            // 連接任務先走了 —— 與 Reset 同義，但措辭要分得開。
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

    /// 批量寫。★ 回傳值是「**回應到此為止了嗎**」——呼叫方據它決定還要不要再喂。
    async fn response_duplex_vec(&mut self, tasks: Vec<HttpTask>) -> Result<bool> {
        let mut end = false;
        for t in tasks {
            match t {
                HttpTask::Header(resp, e) => {
                    self.write_response_header(resp, e).await?;
                    end |= e;
                }
                HttpTask::Body(data, e) => {
                    // ⚠ `Body(None, true)` 是「沒有資料、但到此結束」——它必須被送出去，
                    //   否則對端永遠等不到流的結尾。
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
                // ⚠ h3 上沒有 h1 的協定升級（websocket 走的是 Extended CONNECT，
                //   與這一條不是同一件事）。當成普通體寫出去比靜默丟棄安全。
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
        // ⚠ 這裡**故意不管發沒發出去**：收攤路徑上再報一次「發不出去」沒有任何用處，
        //   而它會蓋住真正的那條錯誤。
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
        // ★ h1 那側這一條要在「等下一個請求」與「讀體」之間做選擇；
        //   h3 上一條流就是一次請求，沒有那個歧義 ⇒ 直接讀體。
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

    /// ✅ **此後真的兌現**（D27 結案）：一條 h3 連接的 TLS 摘要，
    /// 由 [`quic_digest`] 在連接那一層造好。★ 它讓訪問紀錄的 `tls_*` 那幾格
    /// 在 h3 上與 h1/h2 走**同一段程式碼**。
    fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }
    fn digest_mut(&mut self) -> Option<&mut Digest> {
        self.digest.as_mut()
    }

    // ── 下面是 NOT_HONORED 那一組：有定義的行為 + 安全回傳值 ──────────────

    fn client_addr(&self) -> Option<&SocketAddr> {
        self.client.as_ref()
    }
    fn server_addr(&self) -> Option<&SocketAddr> {
        self.server.as_ref()
    }

    /// h2 那側就是這麼做的（`http_req_header_to_wire`）——把 h2/h3 的請求頭
    /// 印成一行 h1 的樣子，給紀錄與需要原始形態的地方用。
    fn pseudo_raw_h1_request_header(&self) -> Bytes {
        http_req_header_to_wire(&self.req)
            .map(|b| b.freeze())
            .unwrap_or_default()
    }

    fn enable_retry_buffering(&mut self) {
        debug!("h3 会话不做请求体重放缓冲（NOT_HONORED）");
    }
    fn retry_buffer_truncated(&self) -> bool {
        // ★ 與 h2 在「從沒開過緩衝」時同值：false。
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
        // ⚠ **不能靜默成功**：還回一個從來沒被取走的東西，說明呼叫方的狀態機與我們
        //   的假設對不上 —— 靜默吞掉會讓那個不一致一路帶到別處才炸。
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

// ── 門：現有執行鏈不許調 NOT_HONORED 裡的任何一個 ──────────────────────────
//
// ★ ★ ★ 這就是「有定義的行為 + **一條斷言**」裡的那條斷言。沒有它的話，
//   上面那一組只是一堆「讀起來很合理」的回傳值 —— 而本儲存庫第 67/69 輪各記過一次
//   「一個防禦措施配一句聽起來很對的理由，比沒有理由更難查」。

/// 現有執行鏈裡所有 `session.<方法>(` 的呼叫點。
///
/// ⚠ 只在測試建置裡存在：它 `include_str!` 三份產品原始碼，沒有理由進產物。
#[cfg(test)]
const CHAIN_SOURCES: &[(&str, &str)] = &[
    ("lib.rs", include_str!("../lib.rs")),
    ("files/mod.rs", include_str!("../files/mod.rs")),
    ("admin.rs", include_str!("../admin.rs")),
];

// ⚠ ★ **這裡曾經有一份叫 `NONE_IS_A_DEFINED_ANSWER` 的豁免名單，它活了不到一天。**
//   訪問紀錄要 TLS 四格時 `session.digest()` 進了執行鏈，而 h3 走得到 ⇒ 這道門當場紅了；
//   當時給它開了一條「它回的值在呼叫處是**有定義的答案**」的豁免。
//   ✅ D27 結案之後 `digest` 被**真的實作出來了**（[`quic_digest`]），豁免沒有物件，
//   這道門回到滿強度。
//
//   > ★ ★ **一個「有定義的答案」型豁免，多半是一處還沒做的功能穿著判據的衣服。**
//   > ⚠ 而它真的在一天後被刪掉，唯一的原因是**過期日期寫進了豁免自己的文件**。

/// 從一段原始碼裡摳出所有 `session.<名字>(` 的方法名。
///
/// ⚠ 只認 `session.` 這一個前綴是**夠的，而這一條是查實過的**：產品裡每一個
/// `ServerSession` 類型的綁定都字面叫 `session`（§10 核過）。
/// ★ 但這份「夠」會過期 —— 所以下面那道門帶一條下界斷言，改名之後它會先紅。
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

    /// ★★★ **這一條才是那句訂正的證明本身。**
    ///
    /// §10 寫下「h3 接進執行環境圖不需要第 13 處 fork『加能力』改動，
    /// 它是上游現成的擴充點」時，那還只是**讀碼結論**。
    /// 這裡把 `H3Session` 真的包進 `ServerSession::new_custom()`，
    /// 並**逐個走一遍現有執行鏈真正會調的那 8 個方法** ——
    /// ⚠ trait 實作編得過**不等於**這件事成立：`ServerSession` 是個 enum，
    /// 它的每個方法都在 `match` 裡分派，`Custom` 那一支完全可以是個空殼。
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

        // ① read_request —— Custom 支回 Ok(true)（建構時請求頭已經讀好了）
        assert!(s.read_request().await.expect("read_request"));
        // ② req_header
        assert_eq!(s.req_header().uri.path(), "/hello");
        // ③ client_addr
        assert_eq!(
            s.client_addr().and_then(|a| a.as_inet()).map(|a| a.port()),
            Some(44300)
        );
        // ④ set_keepalive —— Custom 支是空操作，不該 panic
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

        // ⑧ finish —— h3 上沒有可重複使用的連接，回 None 是對的
        assert!(s.finish().await.expect("收尾").is_none());
        assert!(matches!(
            out_rx.recv().await.expect("收尾信号该到了"),
            (0, FromSession::Finish)
        ));
    }

    /// 連接任務先走了 ⇒ 讀體要給一個**說得清的錯**，而不是掛住或 panic。
    #[tokio::test]
    async fn 连接任务先走了读体给错而不是挂住() {
        let (body_tx, body_rx) = mpsc::channel(1);
        let (out_tx, _out_rx) = mpsc::channel(1);
        let mut sess = H3Session::new(a_request(), true, None, None, body_rx, out_tx, 0, None);
        drop(body_tx);
        assert!(sess.read_body_bytes().await.is_err());
        // ★ 再讀一次也不許掛住 —— body_done 已經置上了。
        assert_eq!(sess.read_body_bytes().await.expect("第二次读"), None);
    }

    /// `has_body=false` 時開局就是「體讀完了」，不該去等一條永遠不來的訊息。
    #[tokio::test]
    async fn 没有请求体时开局就是读完了() {
        let (_body_tx, body_rx) = mpsc::channel(1);
        let (out_tx, _out_rx) = mpsc::channel(1);
        let mut sess = H3Session::new(a_request(), false, None, None, body_rx, out_tx, 0, None);
        assert!(sess.is_body_empty());
        assert!(sess.is_body_done());
        assert_eq!(sess.read_body_bytes().await.expect("读体"), None);
    }

    /// ⚠ `Body(None, true)` 是「沒有資料、但到此結束」—— 它必須被送出去。
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

    /// NOT_HONORED 那一組要**回定義好的安全值**，而不是 panic。
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
        // ★ 而「還回一個從沒被取走的東西」必須是錯，不能靜默成功 —— 兩條都要驗。
        assert!(
            sess.restore_custom_message_reader(Box::new(futures::stream::empty()))
                .is_err()
        );
        assert!(
            sess.restore_custom_message_writer(Box::new(NoWriter))
                .is_err()
        );
    }

    /// 給上面那條用的最小 `CustomMessageWrite`。
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
        // 錯過：不是呼叫（後面不是括號）
        assert!(!got.contains(&"not_a_call".to_string()));
        // 錯過：不是 session
        assert!(!got.contains(&"other_name".to_string()));
        // 空輸入不能被當成「查過了、沒問題」
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

        // ★ 下界一：一個都沒摳出來時，下面那條交集判定會拿空集比，看起來永遠是綠的。
        assert!(
            all.len() >= 5,
            "只从执行链里抠出 {} 个方法调用（{all:?}），扫描多半坏了；本次检查不能采信",
            all.len()
        );
        // ★ 下界二：光有數量不夠 —— 釘一個必然在裡面、而與本門無關的名字。
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

    /// `digest` 真的被兌現了（**G128，D27 結案**），而不是「從名單裡刪掉了名字」。
    ///
    /// ★ ★ 這一條是上面那道門的**另一半**：門只管「執行鏈別調沒兌現的」，
    /// 而**「它到底兌現沒有」得由這裡說**。⚠ 少了它，一次「把名字從 NOT_HONORED 刪掉
    /// 而行為一點沒變」的改動會讓門變綠 —— 那正是門自己的失敗訊息在警告的那件事。
    #[test]
    fn digest_真的兑现了_三格有值而_cipher_有意留空() {
        let d = quic_digest(Some("A.Example".into()), Some("h3".into()));
        let ssl = d
            .ssl_digest
            .as_ref()
            .expect("h3 的 digest 必须带 SslDigest");
        // RFC 9001 §4.2：QUIC 只能用 TLS 1.3 —— 這是規範，不是猜。
        assert_eq!(ssl.version, "TLSv1.3");
        // ⚠ SNI 壓小寫，與 h1/h2 那側（fork 改動 14 的 `from_ssl`）逐字同源。
        assert_eq!(ssl.sni.as_deref(), Some("a.example"));
        assert_eq!(ssl.alpn.as_deref(), Some("h3"));
        // ★ ★ `cipher` **有意是空的**：quiche 的 `Handshake::cipher()` 鎖在私有 `mod tls` 裡。
        //   ⚠ 空串在訪問紀錄那一層的意思是「這一格不出現」，而**不是**「套件叫空」。
        assert_eq!(ssl.cipher, "", "cipher 拿不到时必须留空，不许编一个值");

        // 反向：沒 SNI / 沒 ALPN 的連接照樣有 digest，只是那兩格是 None。
        let bare = quic_digest(None, None);
        let ssl = bare.ssl_digest.as_ref().expect("仍然要有");
        assert!(ssl.sni.is_none() && ssl.alpn.is_none());
        assert_eq!(ssl.version, "TLSv1.3", "版本与有没有 SNI 无关");
    }

    /// ★ 反向的一半：把一個**確實被呼叫**的名字塞進 NOT_HONORED，門必須紅。
    /// 沒有這一條的話，上面那道門與「恆為真」無法區分。
    #[test]
    fn 门自证它抓得住_把一个真调用点塞进名单就该红() {
        let mut all: Vec<String> = Vec::new();
        for (_, src) in CHAIN_SOURCES {
            all.extend(session_calls(src));
        }
        // `req_header` 是執行鏈真的在調的
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
