//! `respond` 那條路上樞衡自己的分配（2026-10-15 · respond 剩的分配）。
//!
//! ★ 單獨一個測試二進位：`#[global_allocator]` 是整個二進位一份（同 `metrics_alloc.rs`、`files_alloc.rs`）。
//! ★ 只數打開了開關的那個執行緒；整條請求在 current-thread 執行環境裡跑 ⇒ 都落在本執行緒。
//! ⚠ **不釘整條請求的絕對次數**：那裡一大半是 pingora / tokio / http 的，升一次依賴就會變。
//!   釘的是兩處「差」—— 頭表之外只多 `Content-Length` 一個值 · h1 入口那一層只多 pingora 要的那一次裝箱。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use fulcrum_server::files::content_length_value;
use fulcrum_server::{FulcrumApp, H1Entry, simple_response_header};
use pingora_core::apps::HttpServerApp;
use pingora_core::protocols::http::ServerSession;
use pingora_http::ResponseHeader;

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

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }

    // ⚠ realloc 也算一次：一個「先小後大」的實作會在這裡而不是 alloc 裡花錢。
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn start() {
    N.with(|n| n.set(0));
    ON.with(|on| on.set(true));
}

fn stop() -> u64 {
    ON.with(|on| on.set(false));
    N.with(|n| n.get())
}

/// 只數 `f` 執行期間本執行緒的分配次數。
fn counted<T>(f: impl FnOnce() -> T) -> (T, u64) {
    start();
    let out = f();
    (out, stop())
}

/// ★ 內置反證：計數器真的在數。少了它，下面那幾條的「差」也可能只是兩邊都數成了 0。
#[test]
fn 计数分配器自己数得到分配() {
    let (_, n) = counted(|| std::hint::black_box(Vec::<u8>::with_capacity(64)));
    assert!(n >= 1, "分配一次都没数到");
}

/// `Content-Length` 的值：恰好一次分配，且與十進位寫法逐位元組相同。
#[test]
fn content_length_值只分配一次_与十进制逐字节相同() {
    for n in [0u64, 7, 9, 10, 99, 100, 4096, 1 << 32, u64::MAX] {
        let (v, a) = counted(|| content_length_value(n));
        assert_eq!(v.as_bytes(), n.to_string().as_bytes(), "{n}");
        assert_eq!(a, 1, "{n}：分配 {a} 次");
    }
}

/// `respond` 一類的回應頭：頭表之外只多 `Content-Length` 那一個值 —— 帶正文時補的 `Content-Type` 不分配。
#[test]
fn 自建响应头在头表之外只多_content_length_一个值() {
    // 對照：同一個狀態碼的空頭表（表本身那幾次）。
    let (_, table) = counted(|| ResponseHeader::build_no_case(200, None).unwrap());
    let (h, with_body) = counted(|| simple_response_header(200, Some(4096), &[]).unwrap());
    assert_eq!(
        h.headers.get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    assert_eq!(h.headers.get("content-length").unwrap(), "4096");
    let (h, no_body) = counted(|| simple_response_header(204, None, &[]).unwrap());
    assert!(h.headers.get("content-type").is_none(), "没有正文不该补");
    assert_eq!(h.headers.get("content-length").unwrap(), "0");
    assert_eq!(
        with_body,
        table + 1,
        "带正文：头表 {table} 次之外又分配了 {} 次",
        with_body.saturating_sub(table)
    );
    assert_eq!(
        no_body,
        table + 1,
        "没有正文：头表 {table} 次之外又分配了 {} 次",
        no_body.saturating_sub(table)
    );
}

fn app() -> Arc<FulcrumApp> {
    let cfg = fulcrum_config::compile_str("t.Fulcrumfile", ":8080 {\n  respond 200 ok\n}\n")
        .config
        .unwrap();
    Arc::new(FulcrumApp::new(
        fulcrum_runtime::SharedRuntime::new(Arc::new(
            fulcrum_runtime::Runtime::build(&cfg).unwrap(),
        )),
        8080,
        false,
        Arc::new(fulcrum_acme::Http01Store::new()),
        fulcrum_server::cache::CacheHandle::new(fulcrum_server::cache::Backend::open(None, 4096)),
        None,
        1,
    ))
}

/// 一條已經寫進連接裡的請求。用戶端那一半交回去留著：丟了它，伺服器端讀到的就是 EOF。
async fn pending_request() -> (ServerSession, tokio::io::DuplexStream) {
    let (server, mut client) = tokio::io::duplex(64 * 1024);
    tokio::io::AsyncWriteExt::write_all(&mut client, b"GET / HTTP/1.1\r\nHost: a.example\r\n\r\n")
        .await
        .unwrap();
    let mut session = ServerSession::new_http1(Box::new(server));
    // 與 pingora `apps/mod.rs` 給新連接第一條請求設的一樣。
    session.set_keepalive(Some(60));
    (session, client)
}

/// h1 入口（pingora 實際調的 [`H1Entry`]）比直接調 [`FulcrumApp::serve_h1`] 只多 pingora 調它時那一次裝箱：
/// ⛔ 不再經 `HttpServerApp` 轉一手（`#[async_trait]` 每過一層裝一次箱 ⇒ 那是每條請求再多一次分配）。
#[test]
fn h1_入口那一层只多_pingora_要的那一次装箱() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let app = app();
        let entry = Arc::new(H1Entry::new(app.clone()));
        let (_tx, shutdown) = tokio::sync::watch::channel(false);
        let (mut via_entry, mut direct) = (u64::MAX, u64::MAX);
        // 兩條路交替各跑 5 輪、各取最小：一次性的分配（第一次用到的執行緒局部與指標序列、
        // 跨秒那一刻 pingora 重排 `Date` 快取）只會讓某一輪多、不會讓它少。
        for _ in 0..5 {
            let (s, _c) = pending_request().await;
            start();
            let out = entry.process_new_http(s, &shutdown).await;
            via_entry = via_entry.min(stop());
            assert!(out.is_some(), "HTTP/1.1 这条应当续");

            let (s, _c) = pending_request().await;
            start();
            let out = app.serve_h1(s, &shutdown).await;
            direct = direct.min(stop());
            assert!(out.is_some(), "HTTP/1.1 这条应当续");
        }
        assert!(direct > 0, "一条请求一次分配都没数到 —— 计数没开着");
        assert_eq!(
            via_entry,
            direct + 1,
            "经 H1Entry 比直接调多了 {} 次（只该多 pingora 调它时那一次装箱）",
            via_entry.saturating_sub(direct)
        );
    });
}
