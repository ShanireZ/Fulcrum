//! ★ 樞衡 fork 自己加的測試（`vendor/pingora/FORK.md` 改動 16 ③ G157 · 改動 18 G163 · 改動 19 G165）：
//! `HttpSession::read_request` 解析一條普通請求時，一共分配幾次；關掉保留大小寫時少幾次；
//! `write_response_header` 寫一條 h1 回應頭時分配幾次、寫出去的位元組是什麼。
//!
//! 改動 16 ③ 把頭偏移從每條請求一個 `Vec<KVRef>` 挪到了棧上 ⇒ 上游原樣的程式碼在這裡多一次。
//! 改動 19 讓寫回應頭不再拷 `Connection` 的值、不再 `freeze` ⇒ 上游原樣的程式碼在那一條多兩次。
//! ⚠ 數的是**整個** `read_request` / `write_response_header`（讀緩衝、請求頭、頭表、保留大小寫的表、
//! 序列化緩衝……都在裡面），不只是那一處 ⇒ rebase 後上遊自己增減了分配，這裡也會紅 ——
//! 那時先核是誰動的，再改期望值，⛔ 別為了讓它綠直接改數。
//!
//! ★ 單獨一個測試二進位：`#[global_allocator]` 是整個二進位一份。
//! ★ 計數器只數打開了開關的那個執行緒（與 `crates/fulcrum-server/tests/metrics_alloc.rs` 同一手法）；
//! 執行環境用 current-thread ⇒ 被測的 future 就在這個執行緒上被輪詢。

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

/// 本執行緒開著開關時記一次。`try_with`：執行緒拆 TLS 的時候分配器照樣會被調到。
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

/// 只數 `f` 被輪詢期間本執行緒的分配次數。
async fn counted<T>(f: impl Future<Output = T>) -> (T, u64) {
    N.with(|n| n.set(0));
    ON.with(|on| on.set(true));
    let out = f.await;
    ON.with(|on| on.set(false));
    (out, N.with(|n| n.get()))
}

const REQUEST: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nUser-Agent: t\r\nAccept: */*\r\n\r\n";

/// 改動 19 用：要求不續的那一種（pingora 由此寫 `Connection: close`）。
const CLOSE_REQUEST: &[u8] =
    b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";

/// 一條完整請求一次讀完（mock 立刻就緒 ⇒ 不登記計時器），數 `read_request` 的分配次數。
/// `preserve` = 樞衡改動 18 的開關（預設 `true`）。
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

/// ★ 內置反證：分配器真的在數。少了它，下面那條的數也可能只是「什麼都沒數」。
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

/// ★ 樞衡改動 16 ③：上游原樣（偏移放在新分配的 `Vec` 裡）是 `EXPECTED + 1`。
/// 8 = 2026-10-06 在本 fork 上實測；裡面有讀緩衝、請求頭、頭表與保留大小寫的表等，⚠ 組成沒有逐項拆。
#[test]
fn 枢衡改动16_读请求头不再为偏移单独分配() {
    const EXPECTED: u64 = 8;
    // 先跑一次不計：第一次可能有處理程序級的一次性分配（紀錄、執行緒本機）。
    let _ = read_request_allocs();
    let n = read_request_allocs();
    assert_eq!(
        EXPECTED, n,
        "read_request 分配了 {n} 次（期望 {EXPECTED}）—— 多一次多半是改动 16 ③ 被 rebase 合掉了"
    );
}

/// ★ 樞衡改動 18（G163）：關掉保留大小寫之後，那張表（一個 `HeaderMap`：下標與條目兩塊）不再分配 ⇒ 比上面那條少 2。
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

/// 改動 19 用的連接：請求的位元組在前，寫出去的位元組接在後面。容量先留夠 ⇒ 寫的時候連接自己一次都不分配，
/// 數到的只有 pingora 的。
fn cursor_session(request: &[u8]) -> HttpSession {
    let mut wire = Vec::with_capacity(64 * 1024);
    wire.extend_from_slice(request);
    HttpSession::new(Box::new(Cursor::new(wire)))
}

/// 寫出去的那一段（接在請求後面的位元組）。
fn written(session: &HttpSession, request_len: usize) -> Vec<u8> {
    let wire = (**session.stream())
        .as_any()
        .downcast_ref::<Cursor<Vec<u8>>>()
        .expect("连接应当还是那个 Cursor<Vec<u8>>");
    wire.get_ref()[request_len..].to_vec()
}

/// 一條最普通的回應：200、沒有正文。不建大小寫表（與樞衡自己構的回應一樣，G162）。
fn ok_response() -> Box<ResponseHeader> {
    let mut resp = ResponseHeader::build_no_case(200, Some(4)).unwrap();
    resp.insert_header(http::header::CONTENT_LENGTH, HeaderValue::from_static("0"))
        .unwrap();
    Box::new(resp)
}

/// 讀完 `request` 之後，數 `write_response_header` 的分配次數（回應頭在計數之外先建好）。
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

/// ★ 樞衡改動 19（G165）：寫一條 h1 回應頭只剩序列化用的那塊暫存緩衝這一次分配。
/// 上游原樣是 `EXPECTED + 2`：`Connection` 的值每次從 `&str` 拷一份（`HeaderValue::from_str`），
/// 序列化完又 `freeze` 一次（緩衝長度 ≠ 容量 ⇒ 另分配一塊共享頭）。續與不續兩種都數（`keep-alive` / `close`）。
/// ⚠ `Date` 是執行緒本機快取、每秒換一次：換的那一秒裡第一次寫會多分配幾次 ⇒ 先寫一次熱身，再取三次裡最少的
/// （三次前後只差幾微秒，最多一次撞上換秒）。
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

/// ★ 樞衡改動 19（G165）的另一半：線上位元組照舊。本 fork 8 個 crate 裡寫回應頭的測試幾乎都把 `update_resp_headers`
/// 關了，沒有一條釘 `Date` / `Connection` 那兩行（上游只在沒進 fork 的 `pingora-proxy` 整合測試裡查 `Connection` 的值）
/// ⇒ 這裡釘：續與不續各寫一次，除 `Date` 的值以外逐位元組對上。
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
