//! 内存后端 + LRU 淘汰（**M2 批 G**）。
//!
//! ★ ★ 这一批**只做内存层**（G95 拍板的切法，也是 `data-path.md` 那条缓解顺序）。
//! 磁盘后端是批 H，形状已由 G83/G84 定死（两级分片目录 / meta 与 body 两文件 /
//! `tmp` 后 `rename` / 启动不扫盘）。
//!
//! ⚠ ⚠ **容量上限不是可选项**：一个没有上限的内存缓存就是一处内存泄漏，
//! 而那种泄漏的现场是「跑几天之后被 OOM 杀掉」——★ 与缓存本身看不出关系，
//! 查的人会先去翻上游连接池和 fd。

use super::cc::ResponseCc;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// 一条缓存条目。
#[derive(Debug, Clone)]
pub struct Entry {
    pub status: u16,
    /// 要回给客户端的头（**已经剥掉逐跳头与 `no-cache="…"` 点名的那几个**）。
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 存进来的那一刻（unix 秒）。★ Age 从它算。
    pub stored_at: i64,
    /// 新鲜期（秒）。
    pub fresh_for: u64,
    /// 存下来时那份 `Cache-Control` —— 重验证与 `must-revalidate` 判定要用。
    pub cc: ResponseCc,
    /// `ETag`（重验证时发 `If-None-Match`）。
    pub etag: Option<String>,
    /// `Last-Modified`（重验证时发 `If-Modified-Since`）。
    pub last_modified: Option<String>,
    /// 这条对应的 `Vary` 头名列表（小写）。
    pub vary: Vec<String>,
}

impl Entry {
    /// 这条占多少字节（用于容量核算）。★ 只算体与头的粗略量 ——
    /// 精确到字节没有意义，而**完全不算头**会让一堆小体大头的条目撑爆上限。
    pub fn footprint(&self) -> u64 {
        let h: usize = self
            .headers
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum();
        (self.body.len() + h) as u64
    }
}

/// 内存缓存。★ 主键 → （次级键 → 条目）两层，理由见 [`super::key`]。
#[derive(Debug)]
pub struct MemStore {
    inner: Mutex<Inner>,
    /// ★ ★ **原子而不是普通字段，因为 `POST /load` 要改得动它**（D19）。
    ///   ⚠ 它有意**不在** `Inner` 的锁里：读它的两处都在 `put` 的热路径上，
    ///   而把它挪进锁会让每一次 `capacity()`（装载日志、管理面回话）都去抢那把锁。
    capacity: AtomicU64,
}

#[derive(Debug, Default)]
struct Inner {
    /// `主键 → Vec<(次级键, 条目)>`。
    ///
    /// ⚠ 内层用 `Vec` 而不是 `HashMap`：同一个主键下的次级键**通常只有一两个**
    /// （`Accept-Encoding` 那种），而我们还要能整族清掉（purge）。
    map: HashMap<String, Vec<(String, Entry)>>,
    /// LRU 顺序：最近用过的排在后面。存 `(主键, 次级键)`。
    lru: Vec<(String, String)>,
    used: u64,
}

/// 查询结果。
///
/// ⚠ `Hit` 里那份 `Entry` 装了箱：它比另外两个变体大一个数量级，
/// 而**这个枚举在每一次未命中时也要被构造** —— 不装箱的话，
/// 每个 miss 都要在栈上腾出一整条缓存条目的空间。
#[derive(Debug)]
pub enum Lookup {
    /// 找到了这一族里匹配次级键的那条。
    Hit(Box<Entry>),
    /// 主键那一族在，但没有匹配的次级键 —— ★ 与「整族都不在」是两件事：
    /// 前者说明 `Vary` 生效了，后者说明这条 URL 从没被缓存过。
    /// ⚠ 合成一个 `Miss` 的话，命中率日志会把两种完全不同的原因混在一起。
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

    /// 换一个容量上限（`POST /load` 走这里，D19）。
    ///
    /// ★ ★ ★ **有意不当场淘汰** —— owner 2026-09-05 拍板：`POST /load` 是接管流量
    /// 时的关键路径，在那里拿缓存锁扫一遍 LRU 会把一个 O(1) 的动作变成随缓存大小
    /// 线性增长，与 G84「启动不扫盘」是同一条纪律。
    /// ⚠ **代价写在明处**：调小之后到下一次写入之前，占用仍可能高于新上限；
    /// 下一次 `put` 的淘汰循环会把它收紧。判据见本文件测试
    /// `调小容量是惰性的_下一次写入才把占用压回去`。
    ///
    /// ⚠ `Relaxed` 够用：这里没有「靠它来发布别的数据」的语义，
    /// 读侧要的只是「早晚会看到新值」，而 `load` 与 `put` 之间没有其它顺序依赖。
    pub fn set_capacity(&self, capacity: u64) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    /// 当前占用与条目数（给日志与判据用）。
    pub fn stats(&self) -> (u64, usize) {
        let g = self.lock();
        (g.used, g.lru.len())
    }

    // ⚠ ⚠ 锁中毒不 panic：一个 panic 过的缓存不该把**整个服务**也拖下水。
    //   ★ `into_inner()` 拿回数据继续用 —— 缓存里最坏的情况是多回一次源。
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 按主键 + 一个「给我某个请求头的值」的闭包去找。
    pub fn get<'a, F>(&self, primary: &str, mut get_header: F) -> Lookup
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        let mut g = self.lock();
        let Some(family) = g.map.get(primary) else {
            return Lookup::Miss;
        };
        // ★ 同一族里所有条目的 `vary` 都应当相同（它来自上游的同一份响应），
        //   但**不假设**这一点：逐条按它自己的 vary 算次级键。
        //   ⚠ 假设它们相同的写法，在上游改了 Vary 的那一刻会开始命中错的条目。
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

    /// 存一条。次级键由调用方按响应的 `Vary` 算好。
    pub fn put(&self, primary: &str, secondary: String, entry: Entry) {
        let size = entry.footprint();
        // ★ 容量**在这一次 put 里只读一次**：它是原子的、可能被 `POST /load` 改
        //   （D19），而守卫与淘汰循环用两个不同的值会让「刚存下又被立刻挤掉」
        //   这种只在换配置那一瞬出现的行为变得无法复现。
        let capacity = self.capacity();
        // ⚠ 一条比整个容量还大的条目**不存** —— 存了会把别的全挤掉，
        //   然后它自己也马上被挤掉。★ 这不是理论情况：`max_size` 配大于
        //   `capacity` 时就会撞上，而两条都是用户写的数。
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

    /// 只更新一条的**元数据**（重验证收到 304 时走这里）。
    ///
    /// ★ 与 G83 「meta 与 body 分开」是同一条理由的内存版：
    /// 重验证是缓存最常见的写操作之一，而它**不该动 body**。
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

    /// 按主键清掉一整族。返回清掉几条。
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

    /// 按主键前缀清。返回清掉几条。
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

    /// 淘汰最久没用的那条。返回 `false` = 没得淘汰了。
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

    // ★ ★ 「整族不在」与「族在但 Vary 不匹配」必须分得开 ——
    //   ⚠ 合成一个 Miss 的话，命中率日志会把两种完全不同的原因混在一起，
    //   而它们的处置也完全不同（前者要看为什么没缓存，后者要看 Vary 配得对不对）。
    #[test]
    fn vary_不匹配与整族不在是两件事() {
        let s = MemStore::new(1024);
        let vary = vec!["accept-encoding".to_string()];
        let sk = super::super::key::secondary(&vary, |_| Some("gzip"));
        s.put("k", sk, entry(b"gz", vary.clone()));

        // 同样带 gzip ⇒ 命中
        assert!(matches!(s.get("k", |_| Some("gzip")), Lookup::Hit(_)));
        // 不带 ⇒ VaryMiss（不是 Miss）
        match s.get("k", none) {
            Lookup::VaryMiss { vary: v } => assert_eq!(v, vary),
            other => panic!("该是 VaryMiss，实际 {other:?}"),
        }
        // 整族不在 ⇒ Miss
        assert!(matches!(s.get("other", none), Lookup::Miss));
    }

    // ── D19：容量改得动（owner 2026-09-05 拍板 ①+③）────────────────────────
    //
    // ★ ★ 缺陷的形状是「两条子指令行为不同，而配置文件上看不出来」：`ttl` / `max_size`
    //   每请求现读、改了立刻生效，而 `capacity` 在启动时被抄进了后端的一个字段
    //   ⇒ 一次 `POST /load` 回 200、`plan` 显示新值，**实际仍是旧容量，没有任何东西会说**。
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

    // ★ ★ ★ **调小容量是惰性的** —— owner 2026-09-05 拍板的语义，本条就是它的判据。
    //
    //   ⚠ `set_capacity` **有意不扫 LRU**：`POST /load` 是接管流量时的关键路径，
    //   在那里拿缓存锁扫一遍 LRU 会把一个 O(1) 的动作变成随缓存大小线性 ——
    //   与 G84「启动不扫盘」同一条纪律。
    //   ⇒ 代价写在明处：**调小之后到下一次写入之前，占用仍可能高于新上限**。
    //   ★ 这条测试两半都验：`set` 当场不动（⚠ 承重，写成「立刻压回去」就把语义搞反了）·
    //     下一次 `put` 把它收紧。
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

        // 下一次写入把它收紧：新条目 50 字节 ⇒ 淘汰循环把旧的 200 挤掉。
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

    // ★ LRU：装满之后淘汰**最久没用**的那条，而不是最先存的那条。
    //   ⚠ 少了 `touch`，它就退化成 FIFO —— 热点条目会被反复淘汰再取回，
    //   命中率掉下来而没有任何东西会红。
    #[test]
    fn 淘汰的是最久没用的不是最先存的() {
        let s = MemStore::new(30);
        for k in ["a", "b", "c"] {
            s.put(k, String::new(), entry(&[0u8; 10], vec![]));
        }
        assert_eq!(s.stats().1, 3);
        // 用一下 a ⇒ 它变成最近用过的
        assert!(matches!(s.get("a", none), Lookup::Hit(_)));
        // 再存一条 ⇒ 该被淘汰的是 b（最久没用），不是 a（最先存）
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

    // ★ 重存同一个键不该把占用算两遍。
    #[test]
    fn 重存同一个键占用不翻倍() {
        let s = MemStore::new(1000);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        let (u1, n1) = s.stats();
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        let (u2, n2) = s.stats();
        assert_eq!((u1, n1), (u2, n2), "重存把占用算了两遍");
    }

    // ★ ★ refresh 只改 meta，**不动 body**（G83 那条理由的内存版）。
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

    // ★ purge 之后占用要真的降下来 —— ⚠ 只从 map 里删而忘了减 `used`，
    //   缓存会「越清越满」，最后一条都存不进去，而 `map` 是空的。
    #[test]
    fn purge_之后占用归零() {
        let s = MemStore::new(1000);
        for i in 0..5 {
            s.put(&format!("k{i}"), String::new(), entry(&[0u8; 50], vec![]));
        }
        assert!(s.stats().0 > 0);
        s.purge_all();
        assert_eq!(s.stats().0, 0);
        // 清完之后还能正常存。
        s.put("again", String::new(), entry(&[0u8; 50], vec![]));
        assert!(matches!(s.get("again", none), Lookup::Hit(_)));
    }
}
