//! ★ 枢衡 fork 自己加的测试（`vendor/pingora/FORK.md` 改动 16 ③，G157）：
//! `HttpSession::read_request` 解析一条普通请求时，一共分配几次。
//!
//! 改动 16 ③ 把头偏移从每条请求一个 `Vec<KVRef>` 挪到了栈上 ⇒ 上游原样的代码在这里多一次。
//! ⚠ 数的是**整个** `read_request`（读缓冲、请求头、头表、保留大小写的表……都在里面），
//! 不只是那一处 ⇒ rebase 后上游自己增减了分配，这里也会红 —— 那时先核是谁动的，再改期望值，
//! ⛔ 别为了让它绿直接改数。
//!
//! ★ 单独一个测试二进制：`#[global_allocator]` 是整个二进制一份。
//! ★ 计数器只数打开了开关的那个线程（与 `crates/fulcrum-server/tests/metrics_alloc.rs` 同一手法）；
//! 运行时用 current-thread ⇒ 被测的 future 就在这个线程上被轮询。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::future::Future;

use pingora_core::protocols::http::v1::server::HttpSession;
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

/// 一条完整请求一次读完（mock 立刻就绪 ⇒ 不登记计时器），数 `read_request` 的分配次数。
fn read_request_allocs() -> u64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mock = Builder::new().read(REQUEST).build();
    let mut session = HttpSession::new(Box::new(mock));
    let (res, n) = rt.block_on(counted(session.read_request()));
    assert_eq!(
        Some(REQUEST.len()),
        res.unwrap(),
        "这条请求应当一次就解析完"
    );
    assert_eq!(3, session.req_header().headers.len());
    n
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
