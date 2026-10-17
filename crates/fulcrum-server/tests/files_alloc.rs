//! `file_server` 那條路上幾個純函式的分配次數（2026-10-14 · 檔案那條路剩下的分配）。
//!
//! ★ 單獨一個測試二進位：`#[global_allocator]` 是整個二進位一份（同 `metrics_alloc.rs`）。
//! ★ 只數打開了開關的那個執行緒；`malloc` 與 `realloc` 分開數 —— 後者正是 `PathBuf` 一段段 `push` 長出來的那一類。
//! ⚠ 這裡釘的是**每請求都走的那幾步**在普通請求上的次數；帶 `%` / `..` 的路徑、截一段的 `Range`
//!   照舊要分配（那幾條路語義複雜、不常走），只判它們的結果，不釘次數。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use bytes::Bytes;
use fulcrum_server::files::{body_range, mime, path};

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static REALLOCS: Cell<u64> = const { Cell::new(0) };
}

/// 本執行緒開著開關時記一次。`try_with`：執行緒拆 TLS 的時候分配器照樣會被調到。
fn bump(c: &'static std::thread::LocalKey<Cell<u64>>) {
    let _ = ON.try_with(|on| {
        if on.get() {
            let _ = c.try_with(|n| n.set(n.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump(&ALLOCS);
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump(&ALLOCS);
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump(&REALLOCS);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// 只數 `f` 執行期間本執行緒的（分配，重新分配）次數。
fn counted<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    ALLOCS.with(|n| n.set(0));
    REALLOCS.with(|n| n.set(0));
    ON.with(|on| on.set(true));
    let out = f();
    ON.with(|on| on.set(false));
    (out, ALLOCS.with(|n| n.get()), REALLOCS.with(|n| n.get()))
}

/// ★ 內置反證：兩個計數器真的在數。少了它，下面那幾條的 0 也可能只是「什麼都沒數」。
#[test]
fn 计数分配器自己数得到分配与重新分配() {
    let (_, a, r) = counted(|| {
        let mut v = std::hint::black_box(Vec::<u8>::with_capacity(1));
        v.extend_from_slice(&[0; 64]);
        v
    });
    assert!(a >= 1, "分配一次都没数到");
    assert!(r >= 1, "重新分配一次都没数到");
}

/// 普通路徑（沒有 `%`、沒有 `..`）解碼歸一：一次都不分配，段直接借 URL 裡的。
#[test]
fn 普通路径解码归一不分配() {
    for (raw, want) in [
        ("/payload.bin", vec!["payload.bin"]),
        ("/sub/dir/a.txt", vec!["sub", "dir", "a.txt"]),
        ("/a//b/./c/", vec!["a", "b", "c"]),
        ("/", vec![]),
    ] {
        let (n, a, r) = counted(|| path::decode_and_normalize(raw));
        let n = n.expect(raw);
        assert_eq!(n.segments().collect::<Vec<_>>(), want, "{raw}");
        assert_eq!((a, r), (0, 0), "{raw}：分配 {a} 次、重新分配 {r} 次");
    }
}

/// 拼到站台根下：容量事先算夠 ⇒ 恰好一次分配、⛔ 不再一段段地長。
#[test]
fn 拼到根下只分配一次_不再增长() {
    for (root, raw, want) in [
        ("/srv/www", "/a/b/payload.bin", "/srv/www/a/b/payload.bin"),
        ("/srv/www/", "/payload.bin", "/srv/www/payload.bin"),
        ("/srv/www", "/", "/srv/www"),
    ] {
        let n = path::decode_and_normalize(raw).unwrap();
        let (p, a, r) = counted(|| path::join_under(root, &n));
        assert_eq!(p, PathBuf::from(want));
        assert_eq!(
            (a, r),
            (1, 0),
            "{root} + {raw}：分配 {a} 次、重新分配 {r} 次"
        );
    }
}

/// 按副檔名查 MIME：大小寫不敏感，且一次都不分配。
#[test]
fn 扩展名查表不分配() {
    let (got, a, r) = counted(|| {
        [
            mime::for_name("payload.bin"),
            mime::for_name("INDEX.HTML"),
            mime::for_name("style.Css"),
        ]
    });
    assert_eq!(
        got,
        [
            mime::for_name("x.bin"),
            mime::for_name("x.html"),
            mime::for_name("x.css")
        ]
    );
    assert_ne!(
        got[1], got[0],
        "html 与 bin 查出来的应当不同（否则这条判据说明不了查表）"
    );
    assert_eq!((a, r), (0, 0), "分配 {a} 次、重新分配 {r} 次");
}

/// 整段正文原樣交出：⛔ 不 `slice`（從 `Vec` 來的 `Bytes` 第一次被 `slice` 要升級成共享的那一塊，那是一次分配）。
#[test]
fn 整段正文原样交出不分配() {
    let b = Bytes::from(vec![7u8; 4096]);
    let ptr = b.as_ptr();
    let (out, a, r) = counted(|| body_range(b, 0, 4096));
    let out = out.expect("整段在区间内");
    assert_eq!(
        (out.as_ptr(), out.len()),
        (ptr, 4096),
        "交出去的应当就是那一块"
    );
    assert_eq!((a, r), (0, 0), "分配 {a} 次、重新分配 {r} 次");
}

/// 截一段時仍指著同一塊記憶體（只增引用計數，⛔ 不另拷一份）；越界照舊回 `None`。
#[test]
fn 截一段仍指着同一块内存_越界回空() {
    let b = Bytes::from((0..4096u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
    let base = b.as_ptr();
    let part = body_range(b.clone(), 100, 200).expect("区间在内");
    assert_eq!(&part[..], &b[100..300]);
    assert_eq!(
        part.as_ptr(),
        base.wrapping_add(100),
        "截出来的应当指着原来那块内存"
    );
    assert!(body_range(b.clone(), 4000, 97).is_none(), "越界");
    assert!(body_range(b, u64::MAX, 1).is_none(), "溢出也是越界");
}
