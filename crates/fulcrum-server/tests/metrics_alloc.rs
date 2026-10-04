//! 指标写入在稳态下**一次都不分配**（G156）。
//!
//! ★ 单独一个测试二进制：`#[global_allocator]` 是整个二进制一份，放进 `fulcrum-server`
//! 的单测二进制里会替换掉那几百条测试的分配器。
//!
//! ★ ★ 计数器**只数打开了开关的那个线程**：测试框架自己还有别的线程，它们的分配
//! 与被测无关，数进来就是一把时准时不准的尺子。开关与计数都是 const 初始化、
//! 无析构的 thread-local ⇒ 取值不分配，分配器里可以安全地用。
//!
//! ⚠ 「稳态」的意思是：一条 series **第一次**出现时分配是正当的（要插一条键），
//! 之后每次写都不许分配 —— 那是请求路径上每条请求都要走的那几笔。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

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

    // ⚠ realloc 也算一次分配：一个「先小后大」的实现会在这里而不是 alloc 里花钱。
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// 只数 `f` 在本线程里的分配次数。
fn count_allocs(f: impl FnOnce()) -> u64 {
    N.with(|n| n.set(0));
    ON.with(|on| on.set(true));
    f();
    ON.with(|on| on.set(false));
    N.with(|n| n.get())
}

/// ★ 内置反证：分配器真的在数。少了它，下面那条的 0 也可能只是「什么都没数」。
#[test]
fn 计数分配器自己数得到分配() {
    let n = count_allocs(|| {
        std::hint::black_box(Vec::<u8>::with_capacity(16));
    });
    assert!(
        n >= 1,
        "计数分配器一次都没数到 —— 下面那条判据的 0 就不说明任何事"
    );
}

#[test]
fn 稳态下写指标一次都不分配() {
    use fulcrum_server::metrics::{REQUEST_DURATION_SECONDS, REQUESTS_TOTAL};
    let req = ["<alloc-test>", "metrics", "2xx", "HTTP/1.1"];
    let dur = ["<alloc-test>", "metrics"];
    // 预热：这两条 series 第一次出现时分配是正当的。
    REQUESTS_TOTAL.inc(&req);
    REQUEST_DURATION_SECONDS.observe(&dur, 0.25);

    let n = count_allocs(|| {
        for _ in 0..1000 {
            REQUESTS_TOTAL.inc(&req);
            REQUEST_DURATION_SECONDS.observe(&dur, 0.25);
        }
    });
    assert_eq!(
        n, 0,
        "稳态下 1000 轮写入分配了 {n} 次（每请求的指标路径不该分配）"
    );
}
