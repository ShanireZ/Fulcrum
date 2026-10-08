//! keep-alive 空闲窗口的计时器：**每条连接一个，跨请求带着走**（G160）。
//!
//! pingora 在「限时 d」下给每一次读请求头各包一个快计时器 ⇒ 每条请求一整套登记与注销
//! （两次装箱、两次读钟、`Notify` 链表挂上又摘下），而 60 秒的计时器在请求不断的连接上从来不到期。
//! 这里改成：读请求头之前把 pingora 切到不限时，自己用一个 [`Sleep`] 包住 `read_request()`；
//! 它**到点时才复核**「这一次已经空闲了多久」，不满 d 就按余量重设 ⇒ 登记每连接每 d 至多一次，
//! 稳态每条请求只读一次钟、poll 一个早已登记好的计时器。
//!
//! ⚠ **语义收紧一处**（owner 2026-10-08 拍「方案 P」）：pingora 是「每一次读各给 d」，
//!   这里包不住它内部的每次读 ⇒ 变成「从开始等到请求头读完，**合计** d」。正常客户端没有区别；
//!   空闲很久才慢慢发头、或逐段慢发头的客户端会被更早关掉（后者在旧口径下能无限拖住一条连接）。
//!
//! 计时器经 pingora 的连接级 `user_context` 带到下一条请求：本条
//! [`carry`] 进 [`HttpPersistentSettings`]，pingora 的 `apply_to_session` 交给下一条的
//! `ServerSession`，[`read_request`] 再取出来。⚠ 那一格从此归它用。

use pingora_core::Result;
use pingora_core::apps::HttpPersistentSettings;
use std::any::Any;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;
use std::time::Duration;
use tokio::time::{Instant, Sleep};

/// 本模块要从连接上用到的几样，**仅此而已**；实现在 `lib.rs`。
///
/// ⚠ 有意不直接拿 `ServerSession`：漏斗之外的数据面拿不到裸 session（G110，
/// `crates/fulcrum/tests/response_gate.rs` 门 1），而这里也用不着写响应。
/// ⛔ 别往这里加写响应的方法 —— 那等于开一个不带 `Alt-Svc` 的出口。
pub(crate) trait Conn {
    /// pingora 的 keep-alive 状态：`None` 不续 · `Some(0)` 不限时 · `Some(秒)` 限时。
    fn keepalive(&self) -> Option<u64>;
    fn set_keepalive(&mut self, secs: Option<u64>);
    /// 上一条请求经 `HttpPersistentSettings::set_user_context` 带过来的东西。
    fn take_carried(&mut self) -> Option<Box<dyn Any + Send + Sync>>;
    fn read_request(&mut self) -> impl Future<Output = Result<bool>> + Send;
}

/// 跨请求带着走的那个计时器。
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

/// 读请求头。进来时 pingora 若是「限时 d」，就换成本模块的计时器；
/// 返回读的结果，以及要 [`carry`] 给下一条请求的计时器。
///
/// 其他状态（不续 / 不限时 / h2 / h3）照旧直接读，带过来的计时器丢掉 ——
/// 那几种状态下这条连接本来就不靠它关。
pub(crate) async fn read_request<C: Conn>(conn: &mut C) -> (Result<bool>, Option<Box<IdleTimer>>) {
    let carried = conn
        .take_carried()
        .and_then(|c| c.downcast::<IdleTimer>().ok());
    match take_over(conn) {
        Some(d) => read_within(conn, d, carried).await,
        None => (conn.read_request().await, None),
    }
}

/// 读完请求头之后定这条连接续不续、续多久（G161）：**pingora 判了「不续」就不续，枢衡只管时长**；
/// 停机窗口内一律不续，让连接自己收敛而不是等被砍断。
///
/// ★ pingora 的判定在 `read_request` 里（`respect_keepalive`，再加 TE 与 CL 同在时关）：
///   HTTP/1.0 没要求 keep-alive · `Connection: close` · TE 与 CL 同在（RFC 9112 §6.1，防请求走私）都是「不续」。
/// ⚠ 这里曾无条件设成「续 `secs` 秒」，把头一类与第三类盖掉了（`Connection: close` 没受影响：
///   pingora 的 `set_server_keepalive` 自己认它）。
pub(crate) fn settle_after_read<C: Conn>(conn: &mut C, shutting_down: bool, secs: u64) {
    if shutting_down || conn.keepalive().is_none() {
        conn.set_keepalive(None);
    } else {
        conn.set_keepalive(Some(secs));
    }
}

/// 把计时器交给下一条请求（经 pingora 的 `HttpPersistentSettings`）。
pub(crate) fn carry(persistent: &mut HttpPersistentSettings, timer: Option<Box<IdleTimer>>) {
    if let Some(t) = timer {
        persistent.set_user_context(t);
    }
}

/// pingora 是「限时 d」就切成不限时（它便不再每次读各登记一个计时器），返回 d。
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
        // ★ 先读：读一次就好了（下一条早已到了）就一下都不碰计时器 —— 与 pingora 原本的惰性相同。
        if let Poll::Ready(r) = read.as_mut().poll(cx) {
            return Poll::Ready(r);
        }
        let start = match wait_start {
            Some(s) => s,
            None => {
                // 第一次挂起 = 这一次开始等：只在这里读一次钟。
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
                    // 与 pingora 自己的 keep-alive 超时同一个返回：读不到请求头 ⇒ 连接关。
                    OnFire::Expire => return Poll::Ready(Ok(false)),
                    OnFire::Rearm(at) => t.sleep.as_mut().reset(at),
                },
            }
        }
    })
    .await;
    (res, timer)
}

/// 开始等时：带过来的到点若晚于「现在 + d」（d 变短了）就得重设，否则会关得比 d 晚。
fn too_late(deadline: Instant, wait_start: Instant, d: Duration) -> bool {
    deadline > wait_start + d
}

#[derive(Debug, PartialEq)]
enum OnFire {
    Expire,
    Rearm(Instant),
}

/// 到点时：这一次空闲满 d 就关；没满（它是上一条请求时登记的）就重设到「这一次开始等 + d」。
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
    /// 暂停的 tokio 时间里计时器按毫秒取整 ⇒ 关的时刻允许晚这么一点。
    const SLACK: Duration = Duration::from_millis(5);

    fn conn() -> (ServerSession, DuplexStream) {
        let (server, client) = duplex(64 * 1024);
        let mut s = ServerSession::new_http1(Box::new(server));
        // 与 pingora `apps/mod.rs` 给新连接第一条请求设的一样。
        s.set_keepalive(Some(D.as_secs()));
        (s, client)
    }

    /// 回一条空响应，再照 pingora `apps/mod.rs` 的循环 + 枢衡 `process_new_http` 的收尾，
    /// 把连接交给下一条请求的 `ServerSession`。
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

    /// 客户端在 `at`（从测试开始算）写 `bytes`；连接留着，⛔ 不关。
    /// ⚠ 写失败不判：服务端按判据已经关了的话这一写本来就该失败，判据看的是服务端那一侧。
    async fn send_at(client: &mut DuplexStream, t0: Instant, at: Duration, bytes: &[u8]) {
        tokio::time::sleep_until(t0 + at).await;
        let _ = client.write_all(bytes).await;
    }

    /// 外层上限：实现坏成「永远在续」时让测试红，⛔ 不让它在暂停的时间里一直续下去、把门禁挂住。
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

    /// ★ 接管之后 pingora 侧是不限时：它不再每次读各登记一个计时器（B1 省下的就是这一截）。
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

    /// 判据 1：新连接什么都不发 ⇒ 正好 d 之后关。
    #[tokio::test(start_paused = true)]
    async fn 空闲到d就关() {
        let t0 = Instant::now();
        let (mut s, _client) = conn();
        let (r, _) = bounded(read_request(&mut s)).await;
        assert!(!r.unwrap(), "空闲到 d 应当读不到请求头");
        at_about(Instant::now(), t0, D);
    }

    /// 判据 2：计时器是第一条请求开始等时登记的（到点 t0 + 60）。第二条在 36 s 读完，
    /// 第三次从 36 s 开始等 ⇒ 60 s 到点时只空闲了 24 s，⛔ 不能关，要到 96 s。
    #[tokio::test(start_paused = true)]
    async fn 到点复核_带过来的计时器先到点也不提前关() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let server = async move {
            let mut s = s;
            for _ in 0..2 {
                let (r, timer) = read_request(&mut s).await;
                assert!(r.unwrap());
                s = next(s, timer).await;
            }
            let (r, _) = read_request(&mut s).await;
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

    /// 判据 3：同一条连接上几条请求用的是**同一个**计时器，且一直没重设过（到点仍是第一次登记的那个）。
    #[tokio::test(start_paused = true)]
    async fn 跨请求是同一个计时器_只登记一次() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let server = async move {
            let mut s = s;
            let mut seen: Vec<(*const IdleTimer, Instant)> = Vec::new();
            for _ in 0..3 {
                let (r, timer) = read_request(&mut s).await;
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

    /// 判据 4：带过来的到点比「这一次开始等 + d」还晚（d 变短了）⇒ 按新的 d 关，⛔ 不等旧的到点。
    #[tokio::test(start_paused = true)]
    async fn 带过来的到点太晚就重设() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let server = async move {
            let mut s = s;
            let (r, timer) = read_request(&mut s).await;
            assert!(r.unwrap());
            let mut s = next(s, timer).await;
            s.set_keepalive(Some(1));
            let (r, _) = read_request(&mut s).await;
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

    /// 判据 5（新口径钉成断言）：请求头分两段、都在 d 内到 ⇒ 照常读完。
    #[tokio::test(start_paused = true)]
    async fn 请求头分段到_合计在d内照常读完() {
        let t0 = Instant::now();
        let (mut s, mut client) = conn();
        let (head, tail) = REQ.split_at(20);
        let server = async move { read_request(&mut s).await.0.unwrap() };
        let client = async move {
            send_at(&mut client, t0, Duration::from_secs(10), head).await;
            send_at(&mut client, t0, Duration::from_secs(20), tail).await;
            client
        };
        let (read, _client) = tokio::join!(bounded(server), client);
        assert!(read);
    }

    /// 判据 5 的另一半：空闲 54 s 才发前一段、后一段在开始等之后 61 s 才到 ⇒ 60 s 关。
    /// ⚠ 这是**新口径**：pingora 的「每一次读各给 d」下，前一段读到后会重新给 60 s，这条连接不会关。
    #[tokio::test(start_paused = true)]
    async fn 请求头从开始等到读完合计d_超过就关() {
        let t0 = Instant::now();
        let (s, mut client) = conn();
        let (head, tail) = REQ.split_at(20);
        let server = async move {
            let mut s = s;
            let (r, _) = read_request(&mut s).await;
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

    /// 读一条原始请求（真 pingora 解析），再按 `process_new_http` 的收尾定续不续；
    /// 返回 pingora 那一格：`None` = 不续。
    async fn verdict(raw: &[u8], shutting_down: bool) -> Option<u64> {
        let (mut s, mut client) = conn();
        client.write_all(raw).await.unwrap();
        let (r, _) = read_request(&mut s).await;
        assert!(r.unwrap(), "请求头应当读得出来");
        settle_after_read(&mut s, shutting_down, D.as_secs());
        s.get_keepalive()
    }

    /// G161：pingora 判了「不续」的，枢衡 ⛔ 不能把它改回续 —— 三类各一条，各自出结果。
    /// HTTP/1.0 没要求 keep-alive ⇒ 回完就关（RFC 9112 §9.3）。
    #[tokio::test]
    async fn http10没要求keepalive就不续() {
        assert_eq!(
            verdict(b"GET / HTTP/1.0\r\nHost: a.example\r\n\r\n", false).await,
            None
        );
    }

    /// TE 与 CL 同在 ⇒ 回完必须关（RFC 9112 §6.1，防请求走私）。
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

    /// `Connection: close`（旧代码在这条上本来就对：pingora 的 `set_server_keepalive` 自己认它）。
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

    /// G161 的另一半：pingora 判「续」的，续多久由枢衡定 ⇒ 一个「什么都关」的实现在这里红。
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
