//! 热路径取运行时快照：[`SharedRuntime::snapshot`] 与它的每线程缓存。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Runtime, SharedRuntime};

/// 实例身份的发号器：进程内唯一，⛔ 不用指针地址（实例释放后地址会被复用）。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// `SharedRuntime` 的世代：[`SharedRuntime::swap`] 每换一次加一。
///
/// ★ 单独占一条缓存行：`SharedRuntime` 住在 `Arc` 里，不对齐的话世代字可能与
///   `Arc<SharedRuntime>` 的引用计数同一行，而后者每有人克隆一次就被写一次。
#[repr(align(64))]
pub(crate) struct Generation {
    epoch: AtomicU64,
    id: u64,
}

impl Generation {
    pub(crate) fn new() -> Generation {
        Generation {
            epoch: AtomicU64::new(0),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// 换代之后调：⚠ 必须在新的那一份写进 `inner` **之后**（见 [`SharedRuntime::snapshot`]）。
    pub(crate) fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
    }
}

/// 一条请求（或一条连接）的配置快照：拿到之后一直指着同一份运行时，换代也不变（G8）。
///
/// ★ 两层 `Arc` 是有意的（所以压掉 clippy 那条）：外层是**本线程私有**的那一个，命中时只增减它的计数；
///   拆成一层就回到「所有工人共写内层计数」，那正是要省掉的东西。
#[allow(clippy::redundant_allocation)]
pub struct RuntimeSnapshot(Arc<Arc<Runtime>>);

impl std::ops::Deref for RuntimeSnapshot {
    type Target = Runtime;
    fn deref(&self) -> &Runtime {
        &self.0
    }
}

/// 本线程最后取到的那一份：`(实例身份, 世代, 快照)`。只存一格 —— 生产上每个进程只有一个 `SharedRuntime`。
type Cached = (u64, u64, Arc<Arc<Runtime>>);

thread_local! {
    static SNAP: std::cell::RefCell<Option<Cached>> = const { std::cell::RefCell::new(None) };
}

impl SharedRuntime {
    /// 取一份快照。**每个请求只调一次**，理由与 [`SharedRuntime::current`] 相同。
    ///
    /// ★ 命中（本线程那一格的实例与世代都对得上）：⛔ 不碰读锁，只克隆本线程的外层 `Arc` ——
    ///   它的引用计数只在本线程的缓存行上。未命中才拿一次读锁、换上新的一格。
    /// ⚠ 世代**先读**、存进那一格的也是**这一个**值：`swap()` 先写 `inner` 再加世代 ⇒
    ///   读到了第 k 次加一，锁里那一份就不旧于第 k 次；反过来只会多刷新一次，不会把旧图标成新世代。
    /// ⚠ 线程正在拆 TLS 时不缓存，直接按锁里那一份给。
    pub fn snapshot(&self) -> RuntimeSnapshot {
        let epoch = self.generation.epoch.load(Ordering::Acquire);
        let id = self.generation.id;
        let cached = SNAP.try_with(|slot| {
            let mut slot = slot.borrow_mut();
            match &*slot {
                Some((i, e, snap)) if *i == id && *e == epoch => Arc::clone(snap),
                _ => {
                    let fresh = Arc::new(self.current());
                    *slot = Some((id, epoch, Arc::clone(&fresh)));
                    fresh
                }
            }
        });
        RuntimeSnapshot(cached.unwrap_or_else(|_| Arc::new(self.current())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(2);

    /// 一个只在 `port` 上 `respond` 的运行时；换代前后靠端口分得开。
    fn rt_on(port: u16) -> Runtime {
        let o = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            &format!(":{port} {{\n  respond 200 ok\n}}\n"),
        );
        Runtime::build(&o.config.expect("DSL 应当编得过")).expect("运行时图应当能建起来")
    }

    fn port_of(r: &Runtime) -> u16 {
        r.listen_ports[0].0
    }

    /// ★ 自带反证：写锁握着时，冷线程**拿不到**快照 —— 证明这条判据看得见锁；暖线程照样拿到。
    #[test]
    fn 命中不碰锁_冷线程才等锁() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        std::thread::scope(|s| {
            let (warm_tx, warm_rx) = mpsc::channel();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let (done_tx, done_rx) = mpsc::channel();
            let sh = &shared;
            s.spawn(move || {
                drop(sh.snapshot());
                warm_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                done_tx.send(port_of(&sh.snapshot())).unwrap();
            });
            warm_rx.recv_timeout(WAIT).expect("暖线程没暖好");
            let guard = shared.inner.write().unwrap();
            go_tx.send(()).unwrap();
            let hot = done_rx.recv_timeout(WAIT);
            let (cold_tx, cold_rx) = mpsc::channel();
            s.spawn(move || cold_tx.send(port_of(&sh.snapshot())).unwrap());
            let cold_while_locked = cold_rx.recv_timeout(Duration::from_millis(300));
            drop(guard);
            assert_eq!(hot, Ok(8001), "写锁握着时，暖线程的快照应当照样拿得到");
            assert!(
                cold_while_locked.is_err(),
                "写锁握着时，冷线程不该拿得到 —— 这条判据看不见锁了"
            );
            assert_eq!(
                cold_rx.recv_timeout(WAIT),
                Ok(8001),
                "放锁之后冷线程应当拿到"
            );
        });
    }

    /// 命中时只碰本线程的外层 `Arc`：内层（全进程共写的那一个）计数一次都不动。
    #[test]
    fn 命中不碰进程共写的计数() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        let base = shared.current();
        let warm = shared.snapshot();
        let n0 = Arc::strong_count(&base);
        let held: Vec<RuntimeSnapshot> = (0..100).map(|_| shared.snapshot()).collect();
        assert_eq!(Arc::strong_count(&base), n0, "命中时动了内层的引用计数");
        for s in &held {
            assert!(Arc::ptr_eq(&s.0, &warm.0), "命中时应当克隆同一个外层 Arc");
        }
    }

    #[test]
    fn 换代之后_暖线程都拿到新图() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        assert_eq!(port_of(&shared.snapshot()), 8001);
        std::thread::scope(|s| {
            let (warm_tx, warm_rx) = mpsc::channel();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let (done_tx, done_rx) = mpsc::channel();
            let sh = &shared;
            s.spawn(move || {
                warm_tx.send(port_of(&sh.snapshot())).unwrap();
                go_rx.recv().unwrap();
                done_tx.send(port_of(&sh.snapshot())).unwrap();
            });
            assert_eq!(warm_rx.recv_timeout(WAIT), Ok(8001));
            shared.swap(rt_on(8002));
            go_tx.send(()).unwrap();
            assert_eq!(
                done_rx.recv_timeout(WAIT),
                Ok(8002),
                "另一个暖线程换代后还拿旧的"
            );
        });
        assert_eq!(port_of(&shared.snapshot()), 8002, "本线程换代后还拿旧的");
    }

    #[test]
    fn 换代前取的快照跨过几次换代照旧指着旧图() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        let s0 = shared.snapshot();
        for p in [8002, 8003, 8004] {
            shared.swap(rt_on(p));
        }
        assert_eq!(port_of(&s0), 8001, "手里那份快照中途变了（G8）");
        assert_eq!(port_of(&shared.snapshot()), 8004);
    }

    #[test]
    fn 两个实例在同一线程上不串() {
        let a = SharedRuntime::new(Arc::new(rt_on(8001)));
        let b = SharedRuntime::new(Arc::new(rt_on(8005)));
        for _ in 0..10 {
            assert_eq!(port_of(&a.snapshot()), 8001);
            assert_eq!(port_of(&b.snapshot()), 8005);
        }
    }

    /// `swap()` 返回之后才取的快照不会旧；同一线程看到的世代单调不减。
    #[test]
    fn 并发换代_返回之后取到的不旧_各线程单调() {
        const M: u64 = 50;
        let shared = SharedRuntime::new(Arc::new(rt_on(9000)));
        let next: Vec<Runtime> = (1..=M).map(|k| rt_on(9000 + k as u16)).collect();
        let returned = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let mut last = 0;
                    while !stop.load(Ordering::Relaxed) {
                        let k = returned.load(Ordering::Acquire);
                        let got = u64::from(port_of(&shared.snapshot()) - 9000);
                        assert!(got >= k, "第 {k} 次换代已返回，却取到第 {got} 份");
                        assert!(got >= last, "同一线程先看到 {last} 又看到 {got}");
                        last = got;
                    }
                });
            }
            for (k, rt) in (1..=M).zip(next) {
                shared.swap(rt);
                returned.store(k, Ordering::Release);
                std::thread::yield_now();
            }
            stop.store(true, Ordering::Relaxed);
        });
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "这条判据跑得太久"
        );
    }

    /// 旧图由暖线程的缓存留着（已接受的代价），那个线程再取一次就放掉；退出的线程随 TLS 一起放掉。
    #[test]
    fn 旧图在暖线程刷新或退出之后放掉() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        let weak = Arc::downgrade(&shared.current());
        std::thread::scope(|s| {
            let sh = &shared;
            s.spawn(move || drop(sh.snapshot())).join().unwrap();
            let (warm_tx, warm_rx) = mpsc::channel();
            let (again_tx, again_rx) = mpsc::channel::<()>();
            let (done_tx, done_rx) = mpsc::channel();
            let (exit_tx, exit_rx) = mpsc::channel::<()>();
            s.spawn(move || {
                drop(sh.snapshot());
                warm_tx.send(()).unwrap();
                again_rx.recv().unwrap();
                drop(sh.snapshot());
                done_tx.send(()).unwrap();
                exit_rx.recv().unwrap();
            });
            warm_rx.recv_timeout(WAIT).unwrap();
            shared.swap(rt_on(8002));
            assert!(
                weak.upgrade().is_some(),
                "暖线程的缓存应当还留着旧图（已接受的代价）"
            );
            again_tx.send(()).unwrap();
            done_rx.recv_timeout(WAIT).unwrap();
            assert!(weak.upgrade().is_none(), "暖线程刷新之后旧图还没放掉");
            exit_tx.send(()).unwrap();
        });
    }

    struct SnapOnDrop(Arc<SharedRuntime>, mpsc::Sender<u16>);

    impl Drop for SnapOnDrop {
        fn drop(&mut self) {
            let _ = self.1.send(port_of(&self.0.snapshot()));
        }
    }

    thread_local! {
        static ON_EXIT: RefCell<Option<SnapOnDrop>> = const { RefCell::new(None) };
    }

    /// 线程退出、TLS 正在拆的时候取快照：⛔ 不 panic，拿到的就是锁里那一份。
    #[test]
    fn 线程析构里取快照不panic_拿到当前那一份() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        shared.swap(rt_on(8006));
        let (tx, rx) = mpsc::channel();
        let sh = shared.clone();
        std::thread::spawn(move || {
            drop(sh.snapshot());
            ON_EXIT.with(|c| *c.borrow_mut() = Some(SnapOnDrop(sh, tx)));
        })
        .join()
        .expect("线程析构里取快照 panic 了");
        assert_eq!(rx.recv_timeout(WAIT), Ok(8006));
    }

    #[test]
    fn 读锁中毒之后冷线程照样取得到() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8007)));
        std::thread::scope(|s| {
            let sh = &shared;
            let r = s
                .spawn(move || {
                    let _g = sh.inner.write().unwrap();
                    panic!("故意在握着写锁时 panic，让锁中毒");
                })
                .join();
            assert!(r.is_err());
            assert!(shared.inner.is_poisoned());
            assert_eq!(
                s.spawn(move || port_of(&sh.snapshot())).join().unwrap(),
                8007
            );
        });
    }

    /// 工作窃取会把快照带到别的线程上丢掉：原线程的缓存不受影响，下一次照样命中同一个外层 `Arc`。
    #[test]
    fn 快照在别的线程丢掉_原线程照旧命中() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        let s1 = shared.snapshot();
        let first = Arc::as_ptr(&s1.0) as usize;
        std::thread::scope(|s| s.spawn(move || drop(s1)).join().unwrap());
        let s2 = shared.snapshot();
        assert_eq!(Arc::as_ptr(&s2.0) as usize, first, "应当命中同一个外层 Arc");
        assert_eq!(
            Arc::strong_count(&s2.0),
            2,
            "应当是本线程缓存一份 + s2 一份"
        );
    }

    #[test]
    fn 两个线程同时换代_之后取到的就是锁里那一份() {
        let shared = SharedRuntime::new(Arc::new(rt_on(8001)));
        let (a, b) = (rt_on(8010), rt_on(8011));
        std::thread::scope(|s| {
            let sh = &shared;
            s.spawn(move || sh.swap(a));
            s.spawn(move || sh.swap(b));
        });
        assert_eq!(port_of(&shared.snapshot()), port_of(&shared.current()));
    }
}
