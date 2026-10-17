//! 記憶體後端 + LRU 淘汰（**M2 批 G**）。
//!
//! ★ ★ 這一批**只做記憶體層**（G95 拍板的切法，也是 `data-path.md` 那條緩解順序）。
//! 磁碟後端是批 H，形狀已由 G83/G84 定死（兩級分片目錄 / meta 與 body 兩檔案 /
//! `tmp` 後 `rename` / 啟動不掃盤）。
//!
//! ⚠ ⚠ **容量上限不是可選項**：一個沒有上限的記憶體快取就是一處記憶體洩漏，
//! 而那種洩漏的現場是「跑幾天之後被 OOM 殺掉」——★ 與快取本身看不出關係，
//! 查的人會先去翻上游連接池和 fd。

use super::cc::ResponseCc;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// 一條快取條目。
#[derive(Debug, Clone)]
pub struct Entry {
    pub status: u16,
    /// 要回給用戶端的頭（**已經剝掉逐跳頭與 `no-cache="…"` 點名的那幾個**）。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 存進來的那一刻（unix 秒）。★ Age 從它算。
    pub stored_at: i64,
    /// 新鮮期（秒）。
    pub fresh_for: u64,
    /// 存下來時那份 `Cache-Control` —— 重驗證與 `must-revalidate` 判定要用。
    pub cc: ResponseCc,
    /// `ETag`（重驗證時發 `If-None-Match`）。
    pub etag: Option<String>,
    /// `Last-Modified`（重驗證時發 `If-Modified-Since`）。
    pub last_modified: Option<String>,
    /// 這條對應的 `Vary` 頭名列表（小寫）。
    pub vary: Vec<String>,
}

impl Entry {
    /// 這條佔多少位元組（用於容量核算）。★ 只算體與頭的粗略量 ——
    /// 精確到位元組沒有意義，而**完全不算頭**會讓一堆小體大頭的條目撐爆上限。
    pub fn footprint(&self) -> u64 {
        let h: usize = self
            .headers
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum();
        (self.body.len() + h) as u64
    }
}

/// 記憶體快取。★ 主鍵 → （次級鍵 → 條目）兩層，理由見 [`super::key`]。
#[derive(Debug)]
pub struct MemStore {
    inner: Mutex<Inner>,
    /// ★ ★ **原子而不是普通欄位，因為 `POST /load` 要改得動它**（D19）。
    ///   ⚠ 它有意**不在** `Inner` 的鎖裡：讀它的兩處都在 `put` 的熱路徑上，
    ///   而把它挪進鎖會讓每一次 `capacity()`（裝載紀錄、管理面回話）都去搶那把鎖。
    capacity: AtomicU64,
}

#[derive(Debug, Default)]
struct Inner {
    /// `主键 → Vec<(次级键, 条目)>`。
    ///
    /// ⚠ 內層用 `Vec` 而不是 `HashMap`：同一個主鍵下的次級鍵**通常只有一兩個**
    /// （`Accept-Encoding` 那種），而我們還要能整族清掉（purge）。
    map: HashMap<String, Vec<(String, Entry)>>,
    /// LRU 順序：最近用過的排在後面。存 `(主键, 次级键)`。
    lru: Vec<(String, String)>,
    used: u64,
}

/// 查詢結果。
///
/// ⚠ `Hit` 裡那份 `Entry` 裝了箱：它比另外兩個變體大一個數量級，
/// 而**這個枚舉在每一次未命中時也要被建構** —— 不裝箱的話，
/// 每個 miss 都要在棧上騰出一整條快取條目的空間。
#[derive(Debug)]
pub enum Lookup {
    /// 找到了這一族裡匹配次級鍵的那條。
    Hit(Box<Entry>),
    /// 主鍵那一族在，但沒有匹配的次級鍵 —— ★ 與「整族都不在」是兩件事：
    /// 前者說明 `Vary` 生效了，後者說明這條 URL 從沒被快取過。
    /// ⚠ 合成一個 `Miss` 的話，命中率紀錄會把兩種完全不同的原因混在一起。
    VaryMiss {
        vary: Vec<String>,
    },
    Miss,
}

impl MemStore {
    pub fn new(capacity: u64) -> MemStore {
        MemStore {
            inner: Mutex::new(Inner::default()),
            capacity: AtomicU64::new(capacity),
        }
    }

    pub fn capacity(&self) -> u64 {
        self.capacity.load(Ordering::Relaxed)
    }

    /// 換一個容量上限（`POST /load` 走這裡，D19）。
    ///
    /// ★ ★ ★ **有意不當場淘汰** —— owner 2026-09-05 拍板：`POST /load` 是接管流量
    /// 時的關鍵路徑，在那裡拿快取鎖掃一遍 LRU 會把一個 O(1) 的動作變成隨快取大小
    /// 線性增長，與 G84「啟動不掃盤」是同一條紀律。
    /// ⚠ **代價寫在明處**：調小之後到下一次寫入之前，佔用仍可能高於新上限；
    /// 下一次 `put` 的淘汰循環會把它收緊。判據見本檔案測試
    /// `调小容量是惰性的_下一次写入才把占用压回去`。
    ///
    /// ⚠ `Relaxed` 夠用：這裡沒有「靠它來發布別的資料」的語義，
    /// 讀側要的只是「早晚會看到新值」，而 `load` 與 `put` 之間沒有其它順序相依。
    pub fn set_capacity(&self, capacity: u64) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    /// 當前佔用與條目數（給紀錄與判據用）。
    pub fn stats(&self) -> (u64, usize) {
        let g = self.lock();
        (g.used, g.lru.len())
    }

    // ⚠ ⚠ 鎖中毒不 panic：一個 panic 過的快取不該把**整個服務**也拖下水。
    //   ★ `into_inner()` 拿回資料繼續用 —— 快取裡最壞的情況是多回一次源。
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 按主鍵 + 一個「給我某個請求頭的值」的閉包去找。
    pub fn get<'a, F>(&self, primary: &str, mut get_header: F) -> Lookup
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        let mut g = self.lock();
        let Some(family) = g.map.get(primary) else {
            return Lookup::Miss;
        };
        // ★ 同一族裡所有條目的 `vary` 都應當相同（它來自上游的同一份回應），
        //   但**不假設**這一點：逐條按它自己的 vary 算次級鍵。
        //   ⚠ 假設它們相同的寫法，在上游改了 Vary 的那一刻會開始命中錯的條目。
        let mut found: Option<(String, Entry)> = None;
        let mut vary_of_family: Vec<String> = Vec::new();
        for (sk, e) in family {
            if vary_of_family.is_empty() {
                vary_of_family = e.vary.clone();
            }
            let want = super::key::secondary(&e.vary, &mut get_header);
            if &want == sk {
                found = Some((sk.clone(), e.clone()));
                break;
            }
        }
        match found {
            Some((sk, e)) => {
                g.touch(primary, &sk);
                Lookup::Hit(Box::new(e))
            }
            None => Lookup::VaryMiss {
                vary: vary_of_family,
            },
        }
    }

    /// 存一條。次級鍵由呼叫方按回應的 `Vary` 算好。
    pub fn put(&self, primary: &str, secondary: String, entry: Entry) {
        let size = entry.footprint();
        // ★ 容量**在這一次 put 裡唯讀一次**：它是原子的、可能被 `POST /load` 改
        //   （D19），而守衛與淘汰循環用兩個不同的值會讓「剛存下又被立刻擠掉」
        //   這種只在換設定那一瞬出現的行為變得無法重現。
        let capacity = self.capacity();
        // ⚠ 一條比整個容量還大的條目**不存** —— 存了會把別的全擠掉，
        //   然後它自己也馬上被擠掉。★ 這不是理論情況：`max_size` 配大於
        //   `capacity` 時就會撞上，而兩條都是使用者寫的數。
        if size > capacity {
            return;
        }
        let mut g = self.lock();
        g.remove_one(primary, &secondary);
        while g.used + size > capacity {
            if !g.evict_one() {
                break;
            }
        }
        g.map
            .entry(primary.to_string())
            .or_default()
            .push((secondary.clone(), entry));
        g.used += size;
        g.lru.push((primary.to_string(), secondary));
    }

    /// 只更新一條的**中繼資料**（重驗證收到 304 時走這裡）。
    ///
    /// ★ 與 G83 「meta 與 body 分開」是同一條理由的記憶體版：
    /// 重驗證是快取最常見的寫操作之一，而它**不該動 body**。
    pub fn refresh(
        &self,
        primary: &str,
        secondary: &str,
        fresh_for: u64,
        now: i64,
        cc: ResponseCc,
    ) {
        let mut g = self.lock();
        if let Some(family) = g.map.get_mut(primary)
            && let Some((_, e)) = family.iter_mut().find(|(sk, _)| sk == secondary)
        {
            e.stored_at = now;
            e.fresh_for = fresh_for;
            e.cc = cc;
        }
        g.touch(primary, secondary);
    }

    /// 按主鍵清掉一整族。回傳清掉幾條。
    pub fn purge_primary(&self, primary: &str) -> usize {
        let mut g = self.lock();
        let Some(family) = g.map.remove(primary) else {
            return 0;
        };
        let n = family.len();
        let freed: u64 = family.iter().map(|(_, e)| e.footprint()).sum();
        g.used = g.used.saturating_sub(freed);
        g.lru.retain(|(p, _)| p != primary);
        n
    }

    /// 按主鍵前綴清。回傳清掉幾條。
    pub fn purge_prefix(&self, prefix: &str) -> usize {
        let keys: Vec<String> = {
            let g = self.lock();
            g.map
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect()
        };
        keys.iter().map(|k| self.purge_primary(k)).sum()
    }

    /// 全清。
    pub fn purge_all(&self) -> usize {
        let mut g = self.lock();
        let n = g.lru.len();
        g.map.clear();
        g.lru.clear();
        g.used = 0;
        n
    }
}

impl Inner {
    fn touch(&mut self, primary: &str, secondary: &str) {
        if let Some(i) = self
            .lru
            .iter()
            .position(|(p, s)| p == primary && s == secondary)
        {
            let item = self.lru.remove(i);
            self.lru.push(item);
        }
    }

    fn remove_one(&mut self, primary: &str, secondary: &str) {
        if let Some(family) = self.map.get_mut(primary)
            && let Some(i) = family.iter().position(|(sk, _)| sk == secondary)
        {
            let (_, e) = family.remove(i);
            self.used = self.used.saturating_sub(e.footprint());
            if family.is_empty() {
                self.map.remove(primary);
            }
        }
        self.lru.retain(|(p, s)| !(p == primary && s == secondary));
    }

    /// 淘汰最久沒用的那條。回傳 `false` = 沒得淘汰了。
    fn evict_one(&mut self) -> bool {
        if self.lru.is_empty() {
            return false;
        }
        let (p, s) = self.lru.remove(0);
        if let Some(family) = self.map.get_mut(&p)
            && let Some(i) = family.iter().position(|(sk, _)| sk == &s)
        {
            let (_, e) = family.remove(i);
            self.used = self.used.saturating_sub(e.footprint());
            if family.is_empty() {
                self.map.remove(&p);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(body: &[u8], vary: Vec<String>) -> Entry {
        Entry {
            status: 200,
            headers: vec![],
            body: body.to_vec(),
            stored_at: 0,
            fresh_for: 60,
            cc: ResponseCc::default(),
            etag: None,
            last_modified: None,
            vary,
        }
    }

    fn none<'a>(_: &str) -> Option<&'a str> {
        None
    }

    #[test]
    fn 存了能取回来() {
        let s = MemStore::new(1024);
        s.put("k", String::new(), entry(b"hello", vec![]));
        match s.get("k", none) {
            Lookup::Hit(e) => assert_eq!(e.body, b"hello"),
            other => panic!("该命中，实际 {other:?}"),
        }
        assert!(matches!(s.get("nope", none), Lookup::Miss));
    }

    // ★ ★ 「整族不在」與「族在但 Vary 不匹配」必須分得開 ——
    //   ⚠ 合成一個 Miss 的話，命中率紀錄會把兩種完全不同的原因混在一起，
    //   而它們的處置也完全不同（前者要看為什麼沒快取，後者要看 Vary 配得對不對）。
    #[test]
    fn vary_不匹配与整族不在是两件事() {
        let s = MemStore::new(1024);
        let vary = vec!["accept-encoding".to_string()];
        let sk = super::super::key::secondary(&vary, |_| Some("gzip"));
        s.put("k", sk, entry(b"gz", vary.clone()));

        // 同樣帶 gzip ⇒ 命中
        assert!(matches!(s.get("k", |_| Some("gzip")), Lookup::Hit(_)));
        // 不帶 ⇒ VaryMiss（不是 Miss）
        match s.get("k", none) {
            Lookup::VaryMiss { vary: v } => assert_eq!(v, vary),
            other => panic!("该是 VaryMiss，实际 {other:?}"),
        }
        // 整族不在 ⇒ Miss
        assert!(matches!(s.get("other", none), Lookup::Miss));
    }

    // ── D19：容量改得動（owner 2026-09-05 拍板 ①+③）────────────────────────
    //
    // ★ ★ 缺陷的形狀是「兩條子指令行為不同，而設定檔案上看不出來」：`ttl` / `max_size`
    //   每請求現讀、改了立刻生效，而 `capacity` 在啟動時被抄進了後端的一個欄位
    //   ⇒ 一次 `POST /load` 回 200、`plan` 顯示新值，**實際仍是舊容量，沒有任何東西會說**。
    #[test]
    fn 容量在线改得动_大条目守卫用的是新值() {
        let s = MemStore::new(10);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert!(
            matches!(s.get("k", none), Lookup::Miss),
            "10 字节容量下 100 字节的条目本就不该存 —— 这是本条测试的前提，不是结论"
        );
        s.set_capacity(1024);
        assert_eq!(s.capacity(), 1024);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert!(
            matches!(s.get("k", none), Lookup::Hit(_)),
            "容量改大之后同一条目该存得下 —— 说明大条目守卫读的是新值"
        );
    }

    // ★ ★ ★ **調小容量是惰性的** —— owner 2026-09-05 拍板的語義，本條就是它的判據。
    //
    //   ⚠ `set_capacity` **有意不掃 LRU**：`POST /load` 是接管流量時的關鍵路徑，
    //   在那裡拿快取鎖掃一遍 LRU 會把一個 O(1) 的動作變成隨快取大小線性 ——
    //   與 G84「啟動不掃盤」同一條紀律。
    //   ⇒ 代價寫在明處：**調小之後到下一次寫入之前，佔用仍可能高於新上限**。
    //   ★ 這條測試兩半都驗：`set` 當場不動（⚠ 承重，寫成「立刻壓回去」就把語義搞反了）·
    //     下一次 `put` 把它收緊。
    #[test]
    fn 调小容量是惰性的_下一次写入才把占用压回去() {
        let s = MemStore::new(1024);
        s.put("a", String::new(), entry(&[0u8; 200], vec![]));
        let (used_before, n_before) = s.stats();
        assert_eq!(
            (used_before, n_before),
            (200, 1),
            "前提：一条 200 字节的条目在里面"
        );

        s.set_capacity(100);
        assert_eq!(
            s.stats(),
            (200, 1),
            "★ 承重：set_capacity 当场**不**淘汰 —— 它扫 LRU 的话这条会红，而那正是要挡的"
        );

        // 下一次寫入把它收緊：新條目 50 位元組 ⇒ 淘汰循環把舊的 200 擠掉。
        s.put("b", String::new(), entry(&[0u8; 50], vec![]));
        let (used_final, _) = s.stats();
        assert!(
            used_final <= 100,
            "下一次写入之后占用该回到新上限内，实得 {used_final}"
        );
        assert!(
            matches!(s.get("a", none), Lookup::Miss),
            "旧条目该被挤掉 —— 说明淘汰循环读的也是新值"
        );
    }

    #[test]
    fn 超过容量的单条不存() {
        let s = MemStore::new(10);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert!(matches!(s.get("k", none), Lookup::Miss));
        assert_eq!(s.stats(), (0, 0));
    }

    // ★ LRU：裝滿之後淘汰**最久沒用**的那條，而不是最先存的那條。
    //   ⚠ 少了 `touch`，它就退化成 FIFO —— 熱點條目會被反覆淘汰再取回，
    //   命中率掉下來而沒有任何東西會紅。
    #[test]
    fn 淘汰的是最久没用的不是最先存的() {
        let s = MemStore::new(30);
        for k in ["a", "b", "c"] {
            s.put(k, String::new(), entry(&[0u8; 10], vec![]));
        }
        assert_eq!(s.stats().1, 3);
        // 用一下 a ⇒ 它變成最近用過的
        assert!(matches!(s.get("a", none), Lookup::Hit(_)));
        // 再存一條 ⇒ 該被淘汰的是 b（最久沒用），不是 a（最先存）
        s.put("d", String::new(), entry(&[0u8; 10], vec![]));
        assert!(
            matches!(s.get("a", none), Lookup::Hit(_)),
            "a 刚用过，不该被淘汰"
        );
        assert!(
            matches!(s.get("b", none), Lookup::Miss),
            "b 最久没用，该被淘汰"
        );
        assert!(matches!(s.get("c", none), Lookup::Hit(_)));
        assert!(matches!(s.get("d", none), Lookup::Hit(_)));
    }

    #[test]
    fn 占用会随淘汰下降且不越界() {
        let s = MemStore::new(100);
        for i in 0..50 {
            s.put(&format!("k{i}"), String::new(), entry(&[0u8; 20], vec![]));
            let (used, _) = s.stats();
            assert!(used <= 100, "第 {i} 次之后占用 {used} 越过了上限");
        }
    }

    // ★ 重存同一個鍵不該把佔用算兩遍。
    #[test]
    fn 重存同一个键占用不翻倍() {
        let s = MemStore::new(1000);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        let (u1, n1) = s.stats();
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        let (u2, n2) = s.stats();
        assert_eq!((u1, n1), (u2, n2), "重存把占用算了两遍");
    }

    // ★ ★ refresh 只改 meta，**不動 body**（G83 那條理由的記憶體版）。
    #[test]
    fn refresh_只改元数据不动体() {
        let s = MemStore::new(1024);
        s.put("k", String::new(), entry(b"original", vec![]));
        s.refresh("k", "", 999, 12345, ResponseCc::default());
        match s.get("k", none) {
            Lookup::Hit(e) => {
                assert_eq!(e.body, b"original", "body 被动过了");
                assert_eq!(e.fresh_for, 999);
                assert_eq!(e.stored_at, 12345);
            }
            other => panic!("该命中，实际 {other:?}"),
        }
    }

    #[test]
    fn purge_三种粒度() {
        let s = MemStore::new(10_000);
        for k in ["p/a", "p/b", "q/c"] {
            s.put(k, String::new(), entry(&[0u8; 10], vec![]));
        }
        assert_eq!(s.purge_primary("p/a"), 1);
        assert!(matches!(s.get("p/a", none), Lookup::Miss));
        assert!(matches!(s.get("p/b", none), Lookup::Hit(_)));

        assert_eq!(s.purge_prefix("p/"), 1);
        assert!(matches!(s.get("p/b", none), Lookup::Miss));
        assert!(matches!(s.get("q/c", none), Lookup::Hit(_)));

        assert_eq!(s.purge_all(), 1);
        assert_eq!(s.stats(), (0, 0));
    }

    // ★ purge 之後佔用要真的降下來 —— ⚠ 只從 map 裡刪而忘了減 `used`，
    //   快取會「越清越滿」，最後一條都存不進去，而 `map` 是空的。
    #[test]
    fn purge_之后占用归零() {
        let s = MemStore::new(1000);
        for i in 0..5 {
            s.put(&format!("k{i}"), String::new(), entry(&[0u8; 50], vec![]));
        }
        assert!(s.stats().0 > 0);
        s.purge_all();
        assert_eq!(s.stats().0, 0);
        // 清完之後還能正常存。
        s.put("again", String::new(), entry(&[0u8; 50], vec![]));
        assert!(matches!(s.get("again", none), Lookup::Hit(_)));
    }
}
