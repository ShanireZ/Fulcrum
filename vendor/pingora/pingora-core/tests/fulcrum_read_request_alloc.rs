//! ★ 枢衡 fork 自己加的测试（`vendor/pingora/FORK.md` 改动 16 ③ G157 · 改动 18 G163 · 改动 19 G165）：
//! `HttpSession::read_request` 解析一条普通请求时，一共分配几次；关掉保留大小写时少几次；
//! `write_response_header` 写一条 h1 响应头时分配几次、写出去的字节是什么。
//!
//! 改动 16 ③ 把头偏移从每条请求一个 `Vec<KVRef>` 挪到了栈上 ⇒ 上游原样的代码在这里多一次。
//! 改动 19 让写响应头不再拷 `Connection` 的值、不再 `freeze` ⇒ 上游原样的代码在那一条多两次。
//! ⚠ 数的是**整个** `read_request` / `write_response_header`（读缓冲、请求头、头表、保留大小写的表、
//! 序列化缓冲……都在里面），不只是那一处 ⇒ rebase 后上游自己增减了分配，这里也会红 ——
//! 那时先核是谁动的，再改期望值，⛔ 别为了让它绿直接改数。
//!
//! ★ 单独一个测试二进制：`#[global_allocator]` 是整个二进制一份。
//! ★ 计数器只数打开了开关的那个线程（与 `crates/fulcrum-server/tests/metrics_alloc.rs` 同一手法）；
//! 运行时用 current-thread ⇒ 被测的 future 就在这个线程上被轮询。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::future::Future;
use std::io::Cursor;

use http::HeaderValue;
use pingora_core::protocols::http::v1::server::HttpSession;
use pingora_http::ResponseHeader;
use tokio_test::io::Builder;

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static N: Cell<u64> = const { Cell::new(0) };
}

/// 本线程开着开关时记一次。`try_with`：线程拆 TLS 的时候分配器照样会被调到。
fn bump() {
    let _ = ON.try_with(|on| {
        if on.get() {
            let _ = N.try_with(|n| n.set(n.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }

    // realloc 也算一次分配。
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// 只数 `f` 被轮询期间本线程的分配次数。
async fn counted<T>(f: impl Future<Output = T>) -> (T, u64) {
    N.with(|n| n.set(0));
    ON.with(|on| on.set(true));
    let out = f.await;
    ON.with(|on| on.set(false));
    (out, N.with(|n| n.get()))
}

const REQUEST: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nUser-Agent: t\r\nAccept: */*\r\n\r\n";

/// 改动 19 用：要求不续的那一种（pingora 由此写 `Connection: close`）。
const CLOSE_REQUEST: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";

/// 一条完整请求一次读完（mock 立刻就绪 ⇒ 不登记计时器），数 `read_request` 的分配次数。
/// `preserve` = 枢衡改动 18 的开关（缺省 `true`）。
fn read_request_allocs_with(preserve: bool) -> u64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mock = Builder::new().read(REQUEST).build();
    let mut session = HttpSession::new(Box::new(mock));
    session.set_preserve_header_case(preserve);
    let (res, n) = rt.block_on(counted(session.read_request()));
    assert_eq!(
        Some(REQUEST.len()),
        res.unwrap(),
        "这条请求应当一次就解析完"
    );
    assert_eq!(3, session.req_header().headers.len());
    n
}

fn read_request_allocs() -> u64 {
    read_request_allocs_with(true)
}

/// ★ 内置反证：分配器真的在数。少了它，下面那条的数也可能只是「什么都没数」。
#[test]
fn 计数分配器自己数得到分配() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (_, n) = rt.block_on(counted(async {
        std::hint::black_box(Vec::<u8>::with_capacity(16));
    }));
    assert!(
        n >= 1,
        "计数分配器一次都没数到 —— 下面那条判据就不说明任何事"
    );
}

/// ★ 枢衡改动 16 ③：上游原样（偏移放在新分配的 `Vec` 里）是 `EXPECTED + 1`。
/// 8 = 2026-10-06 在本 fork 上实测；里面有读缓冲、请求头、头表与保留大小写的表等，⚠ 组成没有逐项拆。
#[test]
fn 枢衡改动16_读请求头不再为偏移单独分配() {
    const EXPECTED: u64 = 8;
    // 先跑一次不计：第一次可能有进程级的一次性分配（日志、线程本地）。
    let _ = read_request_allocs();
    let n = read_request_allocs();
    assert_eq!(
        EXPECTED, n,
        "read_request 分配了 {n} 次（期望 {EXPECTED}）—— 多一次多半是改动 16 ③ 被 rebase 合掉了"
    );
}

/// ★ 枢衡改动 18（G163）：关掉保留大小写之后，那张表（一个 `HeaderMap`：下标与条目两块）不再分配 ⇒ 比上面那条少 2。
#[test]
fn 枢衡改动18_关掉保留大小写少两次分配() {
    let _ = read_request_allocs_with(false);
    let on = read_request_allocs_with(true);
    let off = read_request_allocs_with(false);
    assert_eq!(
        on - 2,
        off,
        "保留大小写 {on} 次、关掉 {off} 次（期望少 2）—— 少不了多半是改动 18 被 rebase 合掉了"
    );
}

/// 改动 19 用的连接：请求的字节在前，写出去的字节接在后面。容量先留够 ⇒ 写的时候连接自己一次都不分配，
/// 数到的只有 pingora 的。
fn cursor_session(request: &[u8]) -> HttpSession {
    let mut wire = Vec::with_capacity(64 * 1024);
    wire.extend_from_slice(request);
    HttpSession::new(Box::new(Cursor::new(wire)))
}

/// 写出去的那一段（接在请求后面的字节）。
fn written(session: &HttpSession, request_len: usize) -> Vec<u8> {
    let wire = (**session.stream())
        .as_any()
        .downcast_ref::<Cursor<Vec<u8>>>()
        .expect("连接应当还是那个 Cursor<Vec<u8>>");
    wire.get_ref()[request_len..].to_vec()
}

/// 一条最普通的响应：200、没有正文。不建大小写表（与枢衡自己构的响应一样，G162）。
fn ok_response() -> Box<ResponseHeader> {
    let mut resp = ResponseHeader::build_no_case(200, Some(4)).unwrap();
    resp.insert_header(http::header::CONTENT_LENGTH, HeaderValue::from_static("0"))
        .unwrap();
    Box::new(resp)
}

/// 读完 `request` 之后，数 `write_response_header` 的分配次数（响应头在计数之外先建好）。
fn write_response_header_allocs(request: &[u8]) -> u64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut session = cursor_session(request);
    rt.block_on(session.read_request()).unwrap();
    let resp = ok_response();
    let (res, n) = rt.block_on(counted(session.write_response_header(resp)));
    res.unwrap();
    n
}

/// ★ 枢衡改动 19（G165）：写一条 h1 响应头只剩序列化用的那块暂存缓冲这一次分配。
/// 上游原样是 `EXPECTED + 2`：`Connection` 的值每次从 `&str` 拷一份（`HeaderValue::from_str`），
/// 序列化完又 `freeze` 一次（缓冲长度 ≠ 容量 ⇒ 另分配一块共享头）。续与不续两种都数（`keep-alive` / `close`）。
/// ⚠ `Date` 是线程本地缓存、每秒换一次：换的那一秒里第一次写会多分配几次 ⇒ 先写一次热身，再取三次里最少的
/// （三次前后只差几微秒，最多一次撞上换秒）。
#[test]
fn 枢衡改动19_写响应头不再拷连接头的值也不再冻结缓冲() {
    const EXPECTED: u64 = 1;
    for (request, conn) in [(REQUEST, "keep-alive"), (CLOSE_REQUEST, "close")] {
        let _ = write_response_header_allocs(request);
        let n = (0..3)
            .map(|_| write_response_header_allocs(request))
            .min()
            .unwrap();
        assert_eq!(
            EXPECTED, n,
            "写 `Connection: {conn}` 的响应头分配了 {n} 次（期望 {EXPECTED}）—— 多出来的多半是改动 19 的 ① 或 ② 被 rebase 合掉了"
        );
    }
}

/// ★ 枢衡改动 19（G165）的另一半：线上字节照旧。上游自己写响应头的测试几乎都把 `update_resp_headers` 关了，
/// 没有一条钉 `Date` / `Connection` 那两行 ⇒ 这里钉：续与不续各写一次，除 `Date` 的值以外逐字节对上。
#[test]
fn 枢衡改动19_写响应头的字节照旧() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for (request, conn) in [(REQUEST, "keep-alive"), (CLOSE_REQUEST, "close")] {
        let mut session = cursor_session(request);
        rt.block_on(session.read_request()).unwrap();
        rt.block_on(session.write_response_header(ok_response()))
            .unwrap();
        let out = String::from_utf8(written(&session, request.len())).unwrap();
        let date = out
            .split("\r\n")
            .find_map(|l| l.strip_prefix("Date: "))
            .unwrap_or_else(|| panic!("响应头里没有 `Date: `：{out:?}"));
        assert!(
            httpdate::parse_http_date(date).is_ok(),
            "`Date` 的值不是 HTTP 日期：{date:?}"
        );
        assert_eq!(
            format!("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nDate: {date}\r\nConnection: {conn}\r\n\r\n"),
            out
        );
    }
}
