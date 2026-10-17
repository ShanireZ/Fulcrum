//! 防驚群（request coalescing）——**M2 批 G**，G95 拍進這一批。
//!
//! ## 它擋的是什麼
//!
//! 一條熱門 URL 的快取過期的**那一瞬間**，此刻在飛的每一個請求都會發現「不新鮮」，
//! 於是**同時**打到上游。★ 快取命中率越高、這條 URL 越熱，這一下就越猛 ——
//! 一個平時被快取擋住 99% 流量的上游，會在過期瞬間收到全部 100%。
//!
//! ⚠ ⚠ **它不是正確性問題，所以不會有任何東西回報錯誤** —— 現場是「上游每隔 N 秒
//! 抖一下」，而 N 恰好等於 TTL。★ 這一條與本儲存庫反覆點名的那一族同形：
//! 壞掉的時候它不出聲，只是把代價轉移到別處。
//!
//! ## 做法：**每個鍵一把閘**
//!
//! 第一個到的請求拿到「我去取」的授權（leader），其餘的等它。
//! leader 取回來存進快取之後叫醒大家，大家**重新查一次快取**。
//!
//! ⚠ ⚠ **等的人必須重新查快取，而不是直接用 leader 遞過來的東西**：
//! leader 可能取回了一個**不可快取**的回應（`no-store`），那時快取裡什麼都沒有，
//! 等的人必須自己回源。★ 直接重複使用 leader 的結果，等於把一個 `private` 回應
//! 發給了另外 N 個用戶端 —— 而那正是「偶爾給錯內容」裡最貴的一種。
//!
//! ⚠ **leader 必須在任何路徑上都放閘**：它出錯、被取消、上游超時都要放。
//! ⇒ 用 RAII 守衛，與 `Inflight` 那處同一條理由（每一步都可能提前回傳）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// 一把「按鍵」的閘。
#[derive(Debug, Default)]
pub struct Coalescer {
    inner: Mutex<HashMap<String, Arc<Notify>>>,
}

/// leader 的授權。**Drop 時放閘並叫醒所有等的人。**
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
        // ★ `notify_waiters` 只叫醒**此刻已經在等**的；而閘已經先移出表了，
        //   所以之後來的人會直接拿到新的 leader 位，不會永遠等下去。
        self.notify.notify_waiters();
    }
}

/// 搶閘的結果。
pub enum Slot<'a> {
    /// 你是 leader，去取吧。
    Leader(Leader<'a>),
    /// 已經有人在取了 —— `await` 這個再**重新查一次快取**。
    Follower(Arc<Notify>),
}

impl Coalescer {
    pub fn new() -> Coalescer {
        Coalescer::default()
    }

    // 鎖中毒不 panic：最壞的結果是多回一次源。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Notify>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 搶這個鍵的閘。
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

    /// 現在有幾個鍵在飛（給紀錄與判據用）。
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
        // 不同的鍵互不影響。
        assert!(matches!(c.acquire("other"), Slot::Leader(_)));
    }

    // ★ ★ leader 走了就要放閘 —— ⚠ 忘了放的話，這個鍵此後**永遠**是 Follower，
    //   而所有請求會永遠等下去。那是一次徹底的服務停擺，且隻影響那一條 URL。
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

    // ★ ★ ★ 就算 leader 是**panic 著**走的，閘也要放。
    //   ⚠ RAII 守衛存在的全部理由就是這一條：轉發路徑上每一步都可能提前回傳。
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

    // ★ follower 等到之後必須**重新查快取**，這一點由呼叫方保證；
    //   這裡釘的是通知真的到得了（否則 follower 會一直掛著）。
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
        // 給 follower 一點時間真的掛到 notified() 上。
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

    // ⚠ 閘是**按鍵**的：一個鍵在飛不該擋住另一個鍵。
    //   ★ 少了這條判據，一個「全域一把鎖」的實作會讓整個快取退化成串行。
    #[test]
    fn 闸是按键的不是全局一把() {
        let c = Coalescer::new();
        let _a = c.acquire("a");
        let _b = c.acquire("b");
        assert_eq!(c.inflight(), 2);
        assert!(matches!(c.acquire("c"), Slot::Leader(_)));
    }
}
