//! keep-alive 空閒視窗的計時器：**每條連接一個，跨請求帶著走**（G160）。
//!
//! pingora 在「限時 d」下給每一次讀請求頭各包一個快計時器 ⇒ 每條請求一整套登記與登出
//! （兩次裝箱、兩次讀鍾、`Notify` 鏈表掛上又摘下），而 60 秒的計時器在請求不斷的連接上從來不到期。
//! 這裡改成：讀請求頭之前把 pingora 切到不限時，自己用一個 [`Sleep`] 包住 `read_request()`；
//! 它**到點時才複核**「這一次已經空閒了多久」，不滿 d 就按餘量重設 ⇒ 登記每連接每 d 至多一次，
//! 穩態每條請求唯讀一次鍾、poll 一個早已登記好的計時器。
//!
//! ⚠ **語義收緊一處**（owner 2026-10-08 拍「方案 P」）：pingora 是「每一次讀各給 d」，
//!   這裡包不住它內部的每次讀 ⇒ 變成「從開始等到請求頭讀完，**合計** d」。正常用戶端沒有區別；
//!   空閒很久才慢慢發頭、或逐段慢發頭的用戶端會被更早關掉（後者在舊口徑下能無限拖住一條連接）。
//!
//! 計時器經 pingora 的連接級 `user_context` 帶到下一條請求：本條
//! [`carry`] 進 [`HttpPersistentSettings`]，pingora 的 `apply_to_session` 交給下一條的
//! `ServerSession`，[`read_request`] 再取出來。⚠ 那一格從此歸它用。

use pingora_core::Result;
use pingora_core::apps::HttpPersistentSettings;
use std::any::Any;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;
use std::time::Duration;
use tokio::time::{Instant, Sleep};

/// 本模組要從連接上用到的幾樣，**僅此而已**；實作在 `lib.rs`。
///
/// ⚠ 有意不直接拿 `ServerSession`：漏斗之外的資料面拿不到裸 session（G110，
/// `crates/fulcrum/tests/response_gate.rs` 門 1），而這裡也用不著寫回應。
/// ⛔ 別往這裡加寫回應的方法 —— 那等於開一個不帶 `Alt-Svc` 的出口。
pub(crate) trait Conn {
    /// pingora 的 keep-alive 狀態：`None` 不續 · `Some(0)` 不限時 · `Some(秒)` 限時。
    fn keepalive(&self) -> Option<u64>;
    fn set_keepalive(&mut self, secs: Option<u64>);
    /// 上一條請求經 `HttpPersistentSettings::set_user_context` 帶過來的東西。
    fn take_carried(&mut self) -> Option<Box<dyn Any + Send + Sync>>;
    /// 讀 h1 請求頭時要不要另建保留頭名原樣大小寫的表（G163，fork 改動 18）；h2 / h3 上 no-op。
    fn set_preserve_header_case(&mut self, preserve: bool);
    fn read_request(&mut self) -> impl Future<Output = Result<bool>> + Send;
}

/// 跨請求帶著走的那個計時器。
pub(crate) struct IdleTimer {
    sleep: Pin<Box<Sleep>>,
}

impl IdleTimer {
    fn new(deadline: Instant) -> Box<Self> {
        Box::new(IdleTimer {
            sleep: Box::pin(tokio::time::sleep_until(deadline)),
        })
    }
}

/// 讀請求頭。進來時 pingora 若是「限時 d」，就換成本模組的計時器；
/// 回傳讀的結果，以及要 [`carry`] 給下一條請求的計時器。
///
/// 其他狀態（不續 / 不限時 / h2 / h3）照舊直接讀，帶過來的計時器丟掉 ——
/// 那幾種狀態下這條連接本來就不靠它關。
///
/// `preserve_case`（G163）：這條請求的頭名要不要保留原樣大小寫 —— 只有原樣轉發給 h1 上游時才用得到，
/// 呼叫方按連接埠定（連接埠上沒有反代站台就不保留，見 `SharedRuntime::port_has_proxy`）。
/// ★ 放進本函式的參數而不是讓呼叫方另設一行：每條請求的工作階段都是新建的、預設「保留」，
///   漏設一行線上上看不出任何差別（只是每請求多兩次分配）⇒ 讓編譯器逼著每個呼叫方表態。
pub(crate) async fn read_request<C: Conn>(
    conn: &mut C,
    preserve_case: bool,
) -> (Result<bool>, Option<Box<IdleTimer>>) {
    conn.set_preserve_header_case(preserve_case);
    let carried = conn
        .take_carried()
        .and_then(|c| c.downcast::<IdleTimer>().ok());
    match take_over(conn) {
        Some(d) => read_within(conn, d, carried).await,
        None => (conn.read_request().await, None),
    }
}

/// 讀完請求頭之後定這條連接續不續、續多久（G161）：**pingora 判了「不續」就不續，樞衡只管時長**；
/// 停機視窗內一律不續，讓連接自己收斂而不是等被砍斷。
///
/// ★ pingora 的判定在 `read_request` 裡（`respect_keepalive`，再加 TE 與 CL 同在時關）：
///   HTTP/1.0 沒要求 keep-alive · `Connection: close` · TE 與 CL 同在（RFC 9112 §6.1，防請求走私）都是「不續」。
/// ⚠ 這裡曾無條件設成「續 `secs` 秒」，把頭一類與第三類蓋掉了（`Connection: close` 沒受影響：
///   pingora 的 `set_server_keepalive` 自己認它）。
pub(crate) fn settle_after_read<C: Conn>(conn: &mut C, shutting_down: bool, secs: u64) {
    if shutting_down || conn.keepalive().is_none() {
        conn.set_keepalive(None);
    } else {
        conn.set_keepalive(Some(secs));
    }
}

/// 把計時器交給下一條請求（經 pingora 的 `HttpPersistentSettings`）。
pub(crate) fn carry(persistent: &mut HttpPersistentSettings, timer: Option<Box<IdleTimer>>) {
    if let Some(t) = timer {
        persistent.set_user_context(t);
    }
}

/// pingora 是「限時 d」就切成不限時（它便不再每次讀各登記一個計時器），回傳 d。
fn take_over<C: Conn>(conn: &mut C) -> Option<Duration> {
    match conn.keepalive() {
        Some(secs) if secs > 0 => {
            conn.set_keepalive(Some(0));
            Some(Duration::from_secs(secs))
        }
        _ => None,
    }
}

async fn read_within<C: Conn>(
    conn: &mut C,
    d: Duration,
    carried: Option<Box<IdleTimer>>,
) -> (Result<bool>, Option<Box<IdleTimer>>) {
    let mut timer = carried;
    let mut wait_start = None;
    let mut read = pin!(conn.read_request());
    let res = poll_fn(|cx| {
        // ★ 先讀：讀一次就好了（下一條早已到了）就一下都不碰計時器 —— 與 pingora 原本的惰性相同。
        if let Poll::Ready(r) = read.as_mut().poll(cx) {
            return Poll::Ready(r);
        }
        let start = match wait_start {
            Some(s) => s,
            None => {
                // 第一次掛起 = 這一次開始等：只在這裡讀一次鍾。
                let s = Instant::now();
                wait_start = Some(s);
                if let Some(t) = timer.as_mut()
                    && too_late(t.sleep.deadline(), s, d)
                {
                    t.sleep.as_mut().reset(s + d);
                }
                s
            }
        };
        let t = timer.get_or_insert_with(|| IdleTimer::new(start + d));
        loop {
            match t.sleep.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(()) => match on_fire(start, Instant::now(), d) {
                    // 與 pingora 自己的 keep-alive 超時同一個回傳：讀不到請求頭 ⇒ 連接關。
                    OnFire::Expire => return Poll::Ready(Ok(false)),
                    OnFire::Rearm(at) => t.sleep.as_mut().reset(at),
                },
            }
        }
    })
    .await;
    (res, timer)
}

/// 開始等時：帶過來的到點若晚於「現在 + d」（d 變短了）就得重設，否則會關得比 d 晚。
fn too_late(deadline: Instant, wait_start: Instant, d: Duration) -> bool {
    deadline > wait_start + d
}

#[derive(Debug, PartialEq)]
enum OnFire {
    Expire,
    Rearm(Instant),
}

/// 到點時：這一次空閒滿 d 就關；沒滿（它是上一條請求時登記的）就重設到「這一次開始等 + d」。
fn on_fire(wait_start: Instant, now: Instant, d: Duration) -> OnFire {
    if now.duration_since(wait_start) >= d {
        OnFire::Expire
    } else {
        OnFire::Rearm(wait_start + d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use pingora_core::apps::ReusedHttpStream;
    use pingora_core::protocols::http::ServerSession;
    use pingora_http::ResponseHeader;
    use tokio::io::{AsyncWriteExt, DuplexStream, duplex};

    const D: Duration = Duration::from_secs(60);
    const REQ: &[u8] = b"GET / HTTP/1.1\r\nHost: a.example\r\n\r\n";
    /// 暫停的 tokio 時間裡計時器按毫秒取整 ⇒ 關的時刻允許晚這麼一點。
    const SLACK: Duration = Duration::from_millis(5);

    fn conn() -> (ServerSession, DuplexStream) {
        let (server, client) = duplex(64 * 1024);
        let mut s = ServerSession::new_http1(Box::new(server));
        // 與 pingora `apps/mod.rs` 給新連接第一條請求設的一樣。
        s.set_keepalive(Some(D.as_secs()));
        (s, client)
    }

    /// 回一條空回應，再照 pingora `apps/mod.rs` 的循環 + 樞衡 `process_new_http` 的收尾，
    /// 把連接交給下一條請求的 `ServerSession`。
    async fn next(mut s: ServerSession, timer: Option<Box<IdleTimer>>) -> ServerSession {
        s.set_keepalive(Some(D.as_secs()));
        let mut resp = ResponseHeader::build(200, None).unwrap();
        resp.insert_header("Content-Length", "0").unwrap();
        s.write_response_header(Box::new(resp)).await.unwrap();
        s.write_response_body(Bytes::new(), true).await.unwrap();
        let mut persistent = HttpPersistentSettings::for_session(&s);
        carry(&mut persistent, timer);
        let reusable = s.finish().await.unwrap().expect("这条连接应当可续");
        let (stream, settings) =
            ReusedHttpStream::from_reusable_stream(reusable, persistent).consume();
        let mut next = ServerSession::new_http1(stream);
        settings.expect("带着设置").apply_to_session(&mut next);
        next
    }

    /// 用戶端在 `at`（從測試開始算）寫 `bytes`；連接留著，⛔ 不關。
    /// ⚠ 寫失敗不判：伺服器端按判據已經關了的話這一寫本來就該失敗，判據看的是伺服器端那一側。
    async fn send_at(client: &mut DuplexStream, t0: Instant, at: Duration, bytes: &[u8]) {
        tokio::time::sleep_until(t0 + at).await;
        let _ = client.write_all(bytes).await;
    }

    /// 外層上限：實作壞成「永遠在續」時讓測試紅，⛔ 不讓它在暫停的時間裡一直續下去、把門禁掛住。
    async fn bounded<F: Future>(f: F) -> F::Output {
        tokio::time::timeout(D * 10, f)
            .await
            .expect("10 个 d 都过去了还没有结果")
    }

    fn at_about(t: Instant, t0: Instant, want: Duration) {
        let got = t.duration_since(t0);
        assert!(
            got >= want && got <= want + SLACK,
            "关在 {got:?}，应在 {want:?}"
        );
    }

    #[test]
    fn 到点时空闲满d才关_不满就重设到这一次开始等加d() {
        let t0 = Instant::now();
        assert_eq!(on_fire(t0, t0 + D, D), OnFire::Expire);
        assert_eq!(
            on_fire(t0, t0 + D - Duration::from_millis(1), D),
            OnFire::Rearm(t0 + D)
        );
        assert!(too_late(t0 + D + Duration::from_millis(1), t0, D));
        assert!(!too_late(t0 + D, t0, D));
    }

    /// ★ 接管之後 pingora 側是不限時：它不再每次讀各登記一個計時器（B1 省下的就是這一截）。
    #[tokio::test]
    async fn 限时才接管_接管后pingora侧不限时() {
        let (mut s, _c) = conn();
        assert_eq!(take_over(&mut s), Some(D));
        assert_eq!(s.get_keepalive(), Some(0));

        let (mut s, _c) = conn();
        s.set_keepalive(None);
        assert_eq!(take_over(&mut s), None);
        assert_eq!(s.get_keepalive(), None);
    }

    /// G163：`preserve_case` 真的在讀之前設到連接上（真 pingora 解析）—— 不保留時請求頭沒有那張表，名與值照舊查得到。
    #[tokio::test]
    async fn preserve_case_在读之前设到连接上() {
        for preserve in [true, false] {
            let (mut s, mut client) = conn();
            client
                .write_all(b"GET / HTTP/1.1\r\nHost: a.example\r\nX-Mixed-Case: 1\r\n\r\n")
                .await
                .unwrap();
            let (r, _) = read_request(&mut s, preserve).await;
            assert!(r.unwrap(), "preserve={preserve}");
            let req = s.req_header();
            assert_eq!(req.headers.get("x-mixed-case").unwrap(), "1");
            assert_eq!(
                req.case_header_iter().next().is_some(),
                preserve,
                "preserve={preserve}：有没有那张表"
            );
        }
    }

    /// 判據 1：新連接什麼都不發 ⇒ 正好 d 之後關。
    #[tokio::test(start_paused = true)]
    async fn 空闲到d就关() {
        let t0 = Instant::now();
        let (mut s, _client) = conn();
        let (r, _) = bounded(read_request(&mut s, true)).await;
        assert!(!r.unwrap(), "空闲到 d 应当读不到请求头");
        at_about(Instant::now(), t0, D);
    }

    /// 判據 2：計時器是第一條請求開始等時登記的（到點 t0 + 60）。第二條在 36 s 讀完，
    /// 第三次從 36 s 開始等 ⇒ 60 s 到點時只空閒了 24 s，⛔ 不能關，要到 96 s。
    #[tokio::test(start_paused = true)]
    async fn 到点复核_带过来的计时器先到点也不提前关() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let server = async move {
            let mut s = s;
            for _ in 0..2 {
                let (r, timer) = read_request(&mut s, true).await;
                assert!(r.unwrap());
                s = next(s, timer).await;
            }
            let (r, _) = read_request(&mut s, true).await;
            assert!(!r.unwrap());
            Instant::now()
        };
        let client = async move {
            send_at(&mut client, t0, Duration::from_secs(10), REQ).await;
            send_at(&mut client, t0, Duration::from_secs(36), REQ).await;
            client
        };
        let (closed, _client) = tokio::join!(bounded(server), client);
        at_about(closed, t0, Duration::from_secs(36) + D);
    }

    /// 判據 3：同一條連接上幾條請求用的是**同一個**計時器，且一直沒重設過（到點仍是第一次登記的那個）。
    #[tokio::test(start_paused = true)]
    async fn 跨请求是同一个计时器_只登记一次() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let server = async move {
            let mut s = s;
            let mut seen: Vec<(*const IdleTimer, Instant)> = Vec::new();
            for _ in 0..3 {
                let (r, timer) = read_request(&mut s, true).await;
                assert!(r.unwrap());
                let t = timer.as_ref().expect("等过就该有计时器");
                seen.push((&**t as *const IdleTimer, t.sleep.deadline()));
                s = next(s, timer).await;
            }
            seen
        };
        let client = async move {
            for at in [1, 2, 3] {
                send_at(&mut client, t0, Duration::from_secs(at), REQ).await;
            }
            client
        };
        let (seen, _client) = tokio::join!(bounded(server), client);
        assert!(
            seen.iter().all(|x| x.0 == seen[0].0),
            "换了计时器：{seen:?}"
        );
        assert!(seen.iter().all(|x| x.1 == t0 + D), "重设过：{seen:?}");
    }

    /// 判據 4：帶過來的到點比「這一次開始等 + d」還晚（d 變短了）⇒ 按新的 d 關，⛔ 不等舊的到點。
    #[tokio::test(start_paused = true)]
    async fn 带过来的到点太晚就重设() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let server = async move {
            let mut s = s;
            let (r, timer) = read_request(&mut s, true).await;
            assert!(r.unwrap());
            let mut s = next(s, timer).await;
            s.set_keepalive(Some(1));
            let (r, _) = read_request(&mut s, true).await;
            assert!(!r.unwrap());
            Instant::now()
        };
        let client = async move {
            send_at(&mut client, t0, Duration::from_secs(1), REQ).await;
            client
        };
        let (closed, _client) = tokio::join!(bounded(server), client);
        at_about(closed, t0, Duration::from_secs(2));
    }

    /// 判據 5（新口徑釘成斷言）：請求頭分兩段、都在 d 內到 ⇒ 照常讀完。
    #[tokio::test(start_paused = true)]
    async fn 请求头分段到_合计在d内照常读完() {
        let t0 = Instant::now();
        let (mut s, mut client) = conn();
        let (head, tail) = REQ.split_at(20);
        let server = async move { read_request(&mut s, true).await.0.unwrap() };
        let client = async move {
            send_at(&mut client, t0, Duration::from_secs(10), head).await;
            send_at(&mut client, t0, Duration::from_secs(20), tail).await;
            client
        };
        let (read, _client) = tokio::join!(bounded(server), client);
        assert!(read);
    }

    /// 判據 5 的另一半：空閒 54 s 才發前一段、後一段在開始等之後 61 s 才到 ⇒ 60 s 關。
    /// ⚠ 這是**新口徑**：pingora 的「每一次讀各給 d」下，前一段讀到後會重新給 60 s，這條連接不會關。
    #[tokio::test(start_paused = true)]
    async fn 请求头从开始等到读完合计d_超过就关() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let (head, tail) = REQ.split_at(20);
        let server = async move {
            let mut s = s;
            let (r, _) = read_request(&mut s, true).await;
            assert!(!r.unwrap(), "合计超过 d 应当关");
            Instant::now()
        };
        let client = async move {
            send_at(&mut client, t0, Duration::from_secs(54), head).await;
            send_at(&mut client, t0, Duration::from_secs(61), tail).await;
            client
        };
        let (closed, _client) = tokio::join!(bounded(server), client);
        at_about(closed, t0, D);
    }

    /// 讀一條原始請求（真 pingora 解析），再按 `process_new_http` 的收尾定續不續；
    /// 回傳 pingora 那一格：`None` = 不續。
    async fn verdict(raw: &[u8], shutting_down: bool) -> Option<u64> {
        let (mut s, mut client) = conn();
        client.write_all(raw).await.unwrap();
        let (r, _) = read_request(&mut s, true).await;
        assert!(r.unwrap(), "请求头应当读得出来");
        settle_after_read(&mut s, shutting_down, D.as_secs());
        s.get_keepalive()
    }

    /// G161：pingora 判了「不續」的，樞衡 ⛔ 不能把它改回續 —— 三類各一條，各自出結果。
    /// HTTP/1.0 沒要求 keep-alive ⇒ 回完就關（RFC 9112 §9.3）。
    #[tokio::test]
    async fn http10没要求keepalive就不续() {
        assert_eq!(
            verdict(b"GET / HTTP/1.0\r\nHost: a.example\r\n\r\n", false).await,
            None
        );
    }

    /// TE 與 CL 同在 ⇒ 回完必須關（RFC 9112 §6.1，防請求走私）。
    #[tokio::test]
    async fn te与cl同在就不续() {
        assert_eq!(
            verdict(
                b"POST / HTTP/1.1\r\nHost: a.example\r\nTransfer-Encoding: chunked\r\nContent-Length: 5\r\n\r\n0\r\n\r\n",
                false
            )
            .await,
            None
        );
    }

    /// `Connection: close`（舊程式碼在這條上本來就對：pingora 的 `set_server_keepalive` 自己認它）。
    #[tokio::test]
    async fn connection_close不续() {
        assert_eq!(
            verdict(
                b"GET / HTTP/1.1\r\nHost: a.example\r\nConnection: close\r\n\r\n",
                false
            )
            .await,
            None
        );
    }

    /// G161 的另一半：pingora 判「續」的，續多久由樞衡定 ⇒ 一個「什麼都關」的實作在這裡紅。
    #[tokio::test]
    async fn pingora判续的续d() {
        let d = Some(D.as_secs());
        assert_eq!(
            verdict(b"GET / HTTP/1.1\r\nHost: a.example\r\n\r\n", false).await,
            d,
            "HTTP/1.1 缺省续"
        );
        assert_eq!(
            verdict(
                b"GET / HTTP/1.0\r\nHost: a.example\r\nConnection: keep-alive\r\n\r\n",
                false
            )
            .await,
            d,
            "HTTP/1.0 要求了 keep-alive"
        );
    }

    #[tokio::test]
    async fn 停机窗口内一律不续() {
        assert_eq!(
            verdict(b"GET / HTTP/1.1\r\nHost: a.example\r\n\r\n", true).await,
            None
        );
    }
}
