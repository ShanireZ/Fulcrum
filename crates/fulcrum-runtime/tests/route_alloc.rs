//! 按 `(Host, 端口)` 找站点，在 Host 已是小写时**一次都不分配**（HTTP 层诊断 2026-10-05 · 请求路径上的字符串分配）。
//!
//! ★ 单独一个测试二进制：`#[global_allocator]` 是整个二进制一份（同 `fulcrum-server/tests/metrics_alloc.rs`）。
//! ★ ★ 计数器**只数打开了开关的那个线程**：测试框架自己的线程与被测无关。
//!
//! ⚠ 起因：`resolve_site` 原先每请求恒分配两次 —— `to_ascii_lowercase()` 一次，
//! 精确表的键是 `(String, u16)`、查一次要 `h.clone()` 拼键又一次。
//! 绝大多数请求的 Host 本来就是小写，这两次都是白花的。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, SiteMatch};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static N: Cell<u64> = const { Cell::new(0) };
}

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

    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn count_allocs<T>(f: impl FnOnce() -> T) -> (T, u64) {
    N.with(|n| n.set(0));
    ON.with(|on| on.set(true));
    let out = f();
    ON.with(|on| on.set(false));
    (out, N.with(|n| n.get()))
}

fn rt() -> Runtime {
    let o = compile_str(
        "t.Fulcrumfile",
        "a.example {\n  respond 200\n}\n*.wild.example {\n  respond 201\n}\n:9000 {\n  respond 202\n}\n",
    );
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    Runtime::build(&o.config.unwrap()).expect("运行时图应当能建起来")
}

/// ★ 内置反证：分配器真的在数。少了它，下面那几条的 0 也可能只是「什么都没数」。
#[test]
fn 计数分配器自己数得到分配() {
    let (_, n) = count_allocs(|| std::hint::black_box(Vec::<u8>::with_capacity(16)));
    assert!(
        n >= 1,
        "计数分配器一次都没数到 —— 下面那几条的 0 就不说明任何事"
    );
}

#[test]
fn host_已是小写时三种匹配都不分配() {
    let r = rt();
    for (host, port, want) in [
        ("a.example", 443, SiteMatch::Exact),
        ("x.wild.example", 443, SiteMatch::Wildcard),
        ("whatever", 9000, SiteMatch::CatchAll),
    ] {
        let (got, n) = count_allocs(|| r.resolve_site(host, port));
        let (_, how, _) = got.unwrap_or_else(|| panic!("{host}:{port} 应当命中"));
        assert_eq!(how, want, "{host}:{port}");
        assert_eq!(
            n, 0,
            "{host}:{port} 走 {want:?} 分配了 {n} 次（Host 已是小写，不该分配）"
        );
    }
    // 谁都不中也不该为此分配。
    let (got, n) = count_allocs(|| r.resolve_site("nope.example", 443));
    assert!(got.is_none());
    assert_eq!(n, 0, "没命中的那一趟分配了 {n} 次");
}

/// 反方向：省分配不许省掉大小写不敏感（`tests/routing.rs` 的 `host_大小写不敏感` 判精确那一种，这里补通配）。
#[test]
fn host_带大写时照样命中() {
    let r = rt();
    let (_, how, _) = r
        .resolve_site("A.Example", 443)
        .expect("精确：大写 Host 应当命中");
    assert_eq!(how, SiteMatch::Exact);
    let (_, how, _) = r
        .resolve_site("X.WILD.Example", 443)
        .expect("通配：大写 Host 应当命中");
    assert_eq!(how, SiteMatch::Wildcard);
    assert!(
        r.resolve_site("A.Example", 8443).is_none(),
        "端口不对仍不许命中"
    );
}
