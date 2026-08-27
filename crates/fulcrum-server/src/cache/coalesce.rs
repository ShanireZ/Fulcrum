//! 防惊群（request coalescing）——**M2 批 G**，G95 拍进这一批。
//!
//! ## 它挡的是什么
//!
//! 一条热门 URL 的缓存过期的**那一瞬间**，此刻在飞的每一个请求都会发现「不新鲜」，
//! 于是**同时**打到上游。★ 缓存命中率越高、这条 URL 越热，这一下就越猛 ——
//! 一个平时被缓存挡住 99% 流量的上游，会在过期瞬间收到全部 100%。
//!
//! ⚠ ⚠ **它不是正确性问题，所以不会有任何东西报错** —— 现场是「上游每隔 N 秒
//! 抖一下」，而 N 恰好等于 TTL。★ 这一条与本仓库反复点名的那一族同形：
//! 坏掉的时候它不出声，只是把代价转移到别处。
//!
//! ## 做法：**每个键一把闸**
//!
//! 第一个到的请求拿到「我去取」的许可（leader），其余的等它。
//! leader 取回来存进缓存之后叫醒大家，大家**重新查一次缓存**。
//!
//! ⚠ ⚠ **等的人必须重新查缓存，而不是直接用 leader 递过来的东西**：
//! leader 可能取回了一个**不可缓存**的响应（`no-store`），那时缓存里什么都没有，
//! 等的人必须自己回源。★ 直接复用 leader 的结果，等于把一个 `private` 响应
//! 发给了另外 N 个客户端 —— 而那正是「偶尔给错内容」里最贵的一种。
//!
//! ⚠ **leader 必须在任何路径上都放闸**：它出错、被取消、上游超时都要放。
//! ⇒ 用 RAII 守卫，与 `Inflight` 那处同一条理由（每一步都可能提前返回）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// 一把「按键」的闸。
#[derive(Debug, Default)]
pub struct Coalescer {
    inner: Mutex<HashMap<String, Arc<Notify>>>,
}

/// leader 的许可。**Drop 时放闸并叫醒所有等的人。**
pub struct Leader<'a> {
    c: &'a Coalescer,
    key: String,
    notify: Arc<Notify>,
}

impl Drop for Leader<'_> {
    fn drop(&mut self) {
        {
            let mut g = self.c.lock();
            g.remove(&self.key);
        }
        // ★ `notify_waiters` 只叫醒**此刻已经在等**的；而闸已经先移出表了，
        //   所以之后来的人会直接拿到新的 leader 位，不会永远等下去。
        self.notify.notify_waiters();
    }
}

/// 抢闸的结果。
pub enum Slot<'a> {
    /// 你是 leader，去取吧。
    Leader(Leader<'a>),
    /// 已经有人在取了 —— `await` 这个再**重新查一次缓存**。
    Follower(Arc<Notify>),
}

impl Coalescer {
    pub fn new() -> Coalescer {
        Coalescer::default()
    }

    // 锁中毒不 panic：最坏的结果是多回一次源。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Notify>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 抢这个键的闸。
    pub fn acquire(&self, key: &str) -> Slot<'_> {
        let mut g = self.lock();
        match g.get(key) {
            Some(n) => Slot::Follower(Arc::clone(n)),
            None => {
                let n = Arc::new(Notify::new());
                g.insert(key.to_string(), Arc::clone(&n));
                Slot::Leader(Leader {
                    c: self,
                    key: key.to_string(),
                    notify: n,
                })
            }
        }
    }

    /// 现在有几个键在飞（给日志与判据用）。
    pub fn inflight(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn 第一个是_leader_第二个是_follower() {
        let c = Coalescer::new();
        let a = c.acquire("k");
        assert!(matches!(a, Slot::Leader(_)));
        assert!(matches!(c.acquire("k"), Slot::Follower(_)));
        // 不同的键互不影响。
        assert!(matches!(c.acquire("other"), Slot::Leader(_)));
    }

    // ★ ★ leader 走了就要放闸 —— ⚠ 忘了放的话，这个键此后**永远**是 Follower，
    //   而所有请求会永远等下去。那是一次彻底的服务停摆，且只影响那一条 URL。
    #[test]
    fn leader_drop_之后闸放开了() {
        let c = Coalescer::new();
        {
            let _l = c.acquire("k");
            assert_eq!(c.inflight(), 1);
        }
        assert_eq!(c.inflight(), 0, "leader 走了闸没放");
        assert!(
            matches!(c.acquire("k"), Slot::Leader(_)),
            "下一个该能当 leader"
        );
    }

    // ★ ★ ★ 就算 leader 是**panic 着**走的，闸也要放。
    //   ⚠ RAII 守卫存在的全部理由就是这一条：转发路径上每一步都可能提前返回。
    #[test]
    fn leader_panic_了闸照样放() {
        let c = Coalescer::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _l = c.acquire("k");
            panic!("模拟取上游时炸了");
        }));
        assert!(r.is_err());
        assert_eq!(c.inflight(), 0, "panic 之后闸没放");
    }

    // ★ follower 等到之后必须**重新查缓存**，这一点由调用方保证；
    //   这里钉的是通知真的到得了（否则 follower 会一直挂着）。
    #[tokio::test]
    async fn follower_等得到通知() {
        let c = Arc::new(Coalescer::new());
        let woken = Arc::new(AtomicUsize::new(0));

        let leader = c.acquire("k");
        let Slot::Follower(n) = c.acquire("k") else {
            panic!("第二个该是 follower");
        };

        let w = Arc::clone(&woken);
        let h = tokio::spawn(async move {
            n.notified().await;
            w.fetch_add(1, Ordering::SeqCst);
        });
        // 给 follower 一点时间真的挂到 notified() 上。
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(woken.load(Ordering::SeqCst), 0, "还没放闸就被叫醒了");

        drop(leader);
        tokio::time::timeout(std::time::Duration::from_secs(2), h)
            .await
            .expect("follower 没被叫醒 —— 惊群闸会把请求永远挂住")
            .expect("follower 任务炸了");
        assert_eq!(woken.load(Ordering::SeqCst), 1);
    }

    // ⚠ 闸是**按键**的：一个键在飞不该挡住另一个键。
    //   ★ 少了这条判据，一个「全局一把锁」的实现会让整个缓存退化成串行。
    #[test]
    fn 闸是按键的不是全局一把() {
        let c = Coalescer::new();
        let _a = c.acquire("a");
        let _b = c.acquire("b");
        assert_eq!(c.inflight(), 2);
        assert!(matches!(c.acquire("c"), Slot::Leader(_)));
    }
}
