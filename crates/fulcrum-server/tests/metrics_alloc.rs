//! 指標寫入在穩態下**一次都不分配**（G156）。
//!
//! ★ 單獨一個測試二進位：`#[global_allocator]` 是整個二進位一份，放進 `fulcrum-server`
//! 的單測二進位裡會替換掉那幾百條測試的分配器。
//!
//! ★ ★ 計數器**只數打開了開關的那個執行緒**：測試框架自己還有別的執行緒，它們的分配
//! 與被測無關，數進來就是一把時準時不準的尺子。開關與計數都是 const 初始化、
//! 無析構的 thread-local ⇒ 取值不分配，分配器裡可以安全地用。
//!
//! ⚠ 「穩態」的意思是：一條 series **第一次**出現時分配是正當的（要插一條鍵），
//! 之後每次寫都不許分配 —— 那是請求路徑上每條請求都要走的那幾筆。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

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

    // ⚠ realloc 也算一次分配：一個「先小後大」的實作會在這裡而不是 alloc 裡花錢。
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// 只數 `f` 在本執行緒裡的分配次數。
fn count_allocs(f: impl FnOnce()) -> u64 {
    N.with(|n| n.set(0));
    ON.with(|on| on.set(true));
    f();
    ON.with(|on| on.set(false));
    N.with(|n| n.get())
}

/// ★ 內建反證：分配器真的在數。少了它，下面那條的 0 也可能只是「什麼都沒數」。
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
    // 預熱：這兩條 series 第一次出現時分配是正當的。
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
