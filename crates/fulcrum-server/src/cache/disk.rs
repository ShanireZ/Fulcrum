//! 磁盘后端（**M2 批 H**，形状由 **G83 / G84** 定死）。
//!
//! ## 形状不是本批拍的，是 D8 结案时拍的
//!
//! | | G83/G84 定下来的 | 这里落在哪 |
//! |---|---|---|
//! | 分片 | 缓存键 hash 的**两级分片目录** | [`DiskStore::shard_of`] |
//! | 布局 | **meta 与 body 两个文件** | [`FamilyMeta`] / `*.body` |
//! | 落地 | `tmp` 后 `rename` | [`DiskStore::atomic_write`] |
//! | 重验证 | **只改 meta 不动 body** | [`DiskStore::refresh`] |
//! | 崩溃恢复 | **启动不扫盘**：读时校验 + 后台渐进重建 | [`DiskStore::get`] / [`DiskStore::rebuild_step`] |
//! | `purge` | 走管理面（批 G 已做）| `admin.rs` |
//!
//! ## ⚠ 一处偏离：meta 是**一族一个**，不是一条一个
//!
//! body 是一条一个，meta 是一个**主键**一个（装着该主键下全部 `Vary` 变体）。
//! 没有 `Vary` 时 —— 绝大多数响应 —— 它就是「两个文件」。
//! ★ 换不了：请求到达时我们**还不知道 `Vary`**（那是响应告诉我们的），
//! 于是次级键算不出来、文件名拼不出来。
//! ★ 而 G83 给出的**理由**（「重验证只改 meta 不动 body」）在这里一字不差地成立。
//!
//! ## ⚠ ⚠ hash 会撞，撞了的后果是「把别人的内容发给你」
//!
//! **正确性不建在「不会撞」上面**：[`FamilyMeta::primary`] 存**主键原文**，
//! 查的时候逐字比一遍，对不上就当没有。
//!
//! ## ⚠ 磁盘 I/O 是**同步**的，代价写在明处
//!
//! 七个操作的签名与 [`super::store::MemStore`] 逐字相同，而那组签名是同步的。
//! 改成 async 等于把接口劈成两份 ⇒ 数据面要为「内存还是磁盘」分出两条路。
//! ⚠ **代价**：一次冷盘读会占住一条 tokio 工作线程。管理面那两条会走遍整棵目录树，
//! 所以它们在调用处包了 `block_in_place`；请求路径上只读两个小文件，没有包。

use super::cc::ResponseCc;
use super::store::{Entry, Lookup};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// meta 文件的格式版本。
///
/// ★ ★ 它不是装饰：加一个字段之后，旧进程写下的 meta 与新代码**长得不一样**，
/// 而 serde 对着一份少了字段的 JSON 会给出一个**看起来很正常的错误**或者一份
/// 半对的结构。⇒ 版本对不上就当这条不存在（当成未命中，后台清理会收走它）。
/// ⚠ 代价认下：改这个号 = 换代之后缓存整个冷启动一次。那是**该付**的代价 ——
/// 另一种做法是「尽量兼容」，而它的失败形态是把一份旧语义的元数据按新语义解释。
const META_VERSION: u32 = 1;

/// 一族（同一个主键）的元数据。★ 与 body **分开**的那一半（G83）。
#[derive(Debug, Serialize, Deserialize)]
struct FamilyMeta {
    v: u32,
    /// ⚠ **主键原文**。文件名是 hash，而 hash 会撞 —— 这一行是撞了之后唯一的防线。
    primary: String,
    variants: Vec<VariantMeta>,
}

/// 一个 `Vary` 变体的元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VariantMeta {
    secondary: String,
    status: u16,
    headers: Vec<(String, String)>,
    stored_at: i64,
    fresh_for: u64,
    cc: ResponseCc,
    etag: Option<String>,
    last_modified: Option<String>,
    vary: Vec<String>,
    /// body 文件**应该**有多长。★ 「读时校验」（G84）就是拿它比的。
    body_len: u64,
}

impl VariantMeta {
    /// 这条占多少字节（口径与 [`Entry::footprint`] 必须一致）。
    ///
    /// ⚠ ⚠ 两处各算各的话，`put` 加进去的数与 `evict` 减掉的数会**慢慢分家**，
    /// 而现场是「缓存越用越满，最后一条都存不进去」，`stats()` 里的条数却是对的。
    fn footprint(&self) -> u64 {
        let h: usize = self
            .headers
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum();
        self.body_len + h as u64
    }
}

/// FNV-1a 64 位。
///
/// ★ ★ **有意不用 `DefaultHasher`**：`std` 明确不保证它跨版本稳定，而这里的 hash
/// 决定的是**磁盘上的文件名** —— 换一次工具链，全盘缓存就集体改名，
/// ⚠ 而现场表现是「升级之后缓存命中率掉到 0」，没有任何东西会说为什么。
/// FNV-1a 写死在这里，它十年后还是同一个值。
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn hex16(v: u64) -> String {
    format!("{v:016x}")
}

/// 淘汰索引（**G84**：走 save/load，不靠启动扫盘）。
///
/// ⚠ ⚠ **它不在读路径上。** 读走的是「按键算出文件名、直接开」那条路 ——
/// 于是**重启之后第一个请求就能命中磁盘上的东西**，而不必等索引重建完。
/// ★ 索引只管一件事：淘汰时知道该先扔谁、以及现在占了多少。
/// ⇒ 索引不全的代价就是 G84 写在明处的那一条：**刚重启时少算占用，淘汰比稳态晚一点**。
#[derive(Debug, Default, Serialize, Deserialize)]
struct DiskIndex {
    /// `(主键, 次级键) → 占用字节`。
    sizes: HashMap<String, u64>,
    /// LRU 顺序：最近用过的排在后面。存 `"主键\u{1}次级键"`。
    lru: Vec<String>,
    used: u64,
}

impl DiskIndex {
    fn slot(primary: &str, secondary: &str) -> String {
        format!("{primary}\u{1}{secondary}")
    }

    fn split(slot: &str) -> (String, String) {
        match slot.split_once('\u{1}') {
            Some((p, s)) => (p.to_string(), s.to_string()),
            None => (slot.to_string(), String::new()),
        }
    }

    fn touch(&mut self, slot: &str) {
        if let Some(i) = self.lru.iter().position(|k| k == slot) {
            let k = self.lru.remove(i);
            self.lru.push(k);
        }
    }

    fn insert(&mut self, slot: String, size: u64) {
        self.remove(&slot);
        self.used += size;
        self.sizes.insert(slot.clone(), size);
        self.lru.push(slot);
    }

    fn remove(&mut self, slot: &str) {
        if let Some(sz) = self.sizes.remove(slot) {
            self.used = self.used.saturating_sub(sz);
        }
        self.lru.retain(|k| k != slot);
    }
}

/// 后台渐进重建的进度（**G84**）。
#[derive(Debug, Default)]
struct RebuildState {
    /// 本轮要走的一级分片目录，倒序弹出。
    todo: Vec<PathBuf>,
    /// 起过一轮没有。
    started: bool,
    /// 走完过几轮（给日志与判据用）。
    passes: u64,
}

/// 磁盘缓存。★ 七个操作的签名与 [`super::store::MemStore`] **逐字相同**。
#[derive(Debug)]
pub struct DiskStore {
    root: PathBuf,
    /// ★ 原子而不是普通字段 —— 与 [`super::store::MemStore`] 同一条理由（D19）。
    /// ⚠ ⚠ **两个后端必须一起改**：它们对 `capacity` 的三个读点形状逐字相同，
    /// 只改一个的话症状是「换成磁盘后端就不生效」，那是最难查的那种形状。
    capacity: AtomicU64,
    index: Mutex<DiskIndex>,
    /// meta 的「读—改—写」互斥。
    ///
    /// ⚠ 没有它的话，两个并发 `put` 会各自读到同一份旧 meta、各写一份新的，
    /// **后写的那份把先写的那个变体抹掉** —— 而它的 body 文件还在盘上，
    /// 于是变成一条谁也找不到的垃圾。★ 那不是「给错内容」，只是浪费，
    /// 但它是一条**不会被任何判据看见**的浪费，所以在结构上堵掉。
    meta_lock: Mutex<()>,
    rebuild: Mutex<RebuildState>,
}

impl DiskStore {
    /// 打开（或建出）一个磁盘缓存。
    ///
    /// ★ ★ **启动不扫盘**（G84）：这里只建目录、读一次索引文件，都是 O(1)。
    /// ⚠ 它与 G78 的 `sd_notify(READY=1)` 是同一件事的两面 —— 换代要求新一代
    /// **快速就绪**，而全盘扫描的时间随缓存大小线性增长。
    pub fn open(root: &Path, capacity: u64) -> std::io::Result<DiskStore> {
        fs::create_dir_all(root)?;
        fs::create_dir_all(root.join("tmp"))?;
        // ★ ★ **真写一次**再说这个目录能用。
        //   ⚠ `create_dir_all` 对一个**已经存在但不可写**的目录是成功的 ——
        //   于是「目录准备好了」这句话在最该报警的那一刻恰好成立，
        //   而第一次 `put` 才失败，那时已经在收流量了。
        let probe = root.join("tmp").join(".writable");
        fs::write(&probe, b"fulcrum")?;
        fs::remove_file(&probe)?;

        let index = load_index(root);
        Ok(DiskStore {
            root: root.to_path_buf(),
            capacity: AtomicU64::new(capacity),
            index: Mutex::new(index),
            meta_lock: Mutex::new(()),
            rebuild: Mutex::new(RebuildState::default()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn capacity(&self) -> u64 {
        self.capacity.load(Ordering::Relaxed)
    }

    /// 换一个容量上限（`POST /load` 走这里，D19）。
    ///
    /// ★ ★ **同样有意不当场淘汰**，而磁盘这边的理由更硬一层：当场收紧要**删文件**，
    /// 那会把 `POST /load` 的耗时挂到盘上去。⇒ 下一次 `put` 的淘汰循环收紧它。
    /// ⚠ 代价与内存后端逐字相同，判据见本文件测试 `容量在线改得动_磁盘后端同样`
    /// 与 `store.rs` 的 `调小容量是惰性的_下一次写入才把占用压回去`。
    pub fn set_capacity(&self, capacity: u64) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    /// 当前占用与条目数。
    ///
    /// ⚠ ⚠ **刚重启时它会偏小**，而盘上的东西照样命中得到 —— 这不是缺陷，
    /// 是 G84 写在明处的那个「最终一致窗口」。后台渐进重建会把它补齐。
    pub fn stats(&self) -> (u64, usize) {
        let g = self.lock_index();
        (g.used, g.lru.len())
    }

    /// 走完过几轮渐进重建（给装载日志与判据用）。
    pub fn rebuild_passes(&self) -> u64 {
        self.rebuild.lock().map(|g| g.passes).unwrap_or(0)
    }

    // ⚠ 锁中毒不 panic：一个 panic 过的缓存不该把**整个服务**也拖下水
    //   （与 `MemStore` 同款，理由也一样：缓存里最坏的情况是多回一次源）。
    fn lock_index(&self) -> std::sync::MutexGuard<'_, DiskIndex> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── 路径 ────────────────────────────────────────────────────────────────

    /// 两级分片目录（G83）。★ nginx 与 Caddy 都是这个形状，理由是同一个：
    /// ⚠ 单目录几十万文件时，`readdir` 与 `unlink` 的代价会把清理任务本身拖垮。
    fn shard_of(&self, hp: &str) -> PathBuf {
        self.root.join(&hp[0..2]).join(&hp[2..4])
    }

    fn meta_path(&self, primary: &str) -> PathBuf {
        let hp = hex16(fnv1a64(primary));
        self.shard_of(&hp).join(format!("{hp}.meta"))
    }

    fn body_path(&self, primary: &str, secondary: &str) -> PathBuf {
        let hp = hex16(fnv1a64(primary));
        let hs = hex16(fnv1a64(secondary));
        self.shard_of(&hp).join(format!("{hp}-{hs}.body"))
    }

    // ── 原子落地（G83：`tmp` 后 `rename`）──────────────────────────────────

    /// 写一个文件：先写 `tmp/`，再 `rename` 到位。
    ///
    /// ★ `rename(2)` 在同一个文件系统内是原子的 —— 读的人要么看见旧的、要么看见新的，
    /// **永远不会看见写到一半的**。⚠ 这也是 `tmp/` 必须在缓存根**里面**的原因：
    /// 跨文件系统的 rename 会退化成「拷贝 + 删除」，那就不原子了。
    ///
    /// `fsync` 只对 **body** 做，不对 meta 做，理由见 [`DiskStore::put`]。
    fn atomic_write(&self, dst: &Path, bytes: &[u8], fsync: bool) -> std::io::Result<()> {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        // ★ 临时名带上进程号与一个自增序号：同一个进程里两次并发落地不能撞名，
        //   而撞了的后果是其中一次写到另一次的半成品上。
        let seq = next_tmp_seq();
        let tmp = self
            .root
            .join("tmp")
            .join(format!("{}-{seq}.tmp", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            if fsync {
                f.sync_all()?;
            }
        }
        match fs::rename(&tmp, dst) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    fn read_family(&self, primary: &str) -> Option<FamilyMeta> {
        let raw = fs::read(self.meta_path(primary)).ok()?;
        let fam: FamilyMeta = serde_json::from_slice(&raw).ok()?;
        // ★ 版本对不上、或者 hash 撞到了别人头上 —— 两种都当「这里没有」。
        if fam.v != META_VERSION || fam.primary != primary {
            return None;
        }
        Some(fam)
    }

    fn write_family(&self, fam: &FamilyMeta) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(fam).map_err(std::io::Error::other)?;
        self.atomic_write(&self.meta_path(&fam.primary), &bytes, false)
    }

    /// 整族抹掉（meta + 全部 body）。返回抹掉几条。
    fn drop_family(&self, fam: &FamilyMeta) -> usize {
        for v in &fam.variants {
            let _ = fs::remove_file(self.body_path(&fam.primary, &v.secondary));
        }
        let _ = fs::remove_file(self.meta_path(&fam.primary));
        let mut g = self.lock_index();
        for v in &fam.variants {
            g.remove(&DiskIndex::slot(&fam.primary, &v.secondary));
        }
        fam.variants.len()
    }

    /// 把一个变体从它那一族里摘掉（读时校验失败、或淘汰时走这里）。
    fn drop_variant(&self, primary: &str, secondary: &str) {
        let _guard = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = fs::remove_file(self.body_path(primary, secondary));
        if let Some(mut fam) = self.read_family(primary) {
            fam.variants.retain(|v| v.secondary != secondary);
            if fam.variants.is_empty() {
                let _ = fs::remove_file(self.meta_path(primary));
            } else if let Err(e) = self.write_family(&fam) {
                warn!("缓存：摘掉一个变体之后写不回 meta（{primary}）：{e}");
            }
        }
        self.lock_index()
            .remove(&DiskIndex::slot(primary, secondary));
    }

    // ── 七个操作（与 `MemStore` 逐字同签名）──────────────────────────────

    /// 查一条。
    ///
    /// ★ ★ ★ **这条路不看索引**：按键算出文件名直接开 —— 于是「启动不扫盘」（G84）
    /// 与「重启之后立刻还能命中」这两件事**同时**成立。
    /// ⚠ 把读路径挂到索引上的写法，会让一次重启等于一次全量回源，
    /// 而那恰恰是磁盘缓存存在的理由。
    pub fn get<'a, F>(&self, primary: &str, mut get_header: F) -> Lookup
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        let Some(fam) = self.read_family(primary) else {
            return Lookup::Miss;
        };
        let mut vary_of_family: Vec<String> = Vec::new();
        let mut found: Option<VariantMeta> = None;
        for v in &fam.variants {
            if vary_of_family.is_empty() {
                vary_of_family = v.vary.clone();
            }
            // ★ 逐条按**它自己的** vary 算次级键，不假设一族里都一样
            //   （与 `MemStore` 同一条：上游改了 Vary 的那一刻，假设就开始命中错的条目）。
            if super::key::secondary(&v.vary, &mut get_header) == v.secondary {
                found = Some(v.clone());
                break;
            }
        }
        let Some(v) = found else {
            return Lookup::VaryMiss {
                vary: vary_of_family,
            };
        };

        // ── ★ ★ 读时校验（G84）───────────────────────────────────────────
        //   ⚠ 「meta 说有、body 不在或长度对不上」是崩溃恢复必然会遇到的状态
        //   （body 落地了而 meta 还没、或者反过来）。**即丢即删**，当成未命中。
        let body = match fs::read(self.body_path(primary, &v.secondary)) {
            Ok(b) if b.len() as u64 == v.body_len => b,
            Ok(b) => {
                warn!(
                    "缓存：坏条目即丢即删 —— body 长度 {} 与 meta 说的 {} 对不上（{primary}）",
                    b.len(),
                    v.body_len
                );
                self.drop_variant(primary, &v.secondary);
                return Lookup::Miss;
            }
            Err(e) => {
                debug!("缓存：meta 有而 body 读不出来，即丢即删（{primary}）：{e}");
                self.drop_variant(primary, &v.secondary);
                return Lookup::Miss;
            }
        };

        {
            let mut g = self.lock_index();
            let slot = DiskIndex::slot(primary, &v.secondary);
            // ⚠ 索引里可能根本没有这一条（刚重启、后台还没走到这一格）——
            //   ★ 那就**顺手把它记上**：读路径本来就已经把这条的大小算出来了，
            //   等后台走到这里再记一遍是白等一轮。
            if g.sizes.contains_key(&slot) {
                g.touch(&slot);
            } else {
                let size = v.footprint();
                g.insert(slot, size);
            }
        }

        Lookup::Hit(Box::new(Entry {
            status: v.status,
            headers: v.headers,
            body,
            stored_at: v.stored_at,
            fresh_for: v.fresh_for,
            cc: v.cc,
            etag: v.etag,
            last_modified: v.last_modified,
            vary: v.vary,
        }))
    }

    /// 存一条。
    ///
    /// 落地顺序是 **body 先、meta 后**，这一点不是随手排的：
    /// ★ meta 是**唯一的索引**，所以「meta 在而 body 不在」会被读时校验当场收掉，
    /// 而「body 在而 meta 不提它」只是一条没人认领的垃圾 —— 由后台清理收走。
    /// ⇒ 先写 body 让崩溃落在**便宜**的那一边。
    ///
    /// `fsync` **只给 body**：
    /// ⚠ 一份长度正确、内容是垃圾的 body 会被原样发给客户端 —— 那正是
    /// 「偶尔给错内容」。而一份坏掉的 meta 解析不出来，当场就被当成没有。
    /// ⇒ 把这一次 fsync 花在真正要命的那一边，不给 meta 再花一次。
    pub fn put(&self, primary: &str, secondary: String, entry: Entry) {
        let size = entry.footprint();
        // ★ 容量在这一次 put 里只读一次（与 `MemStore` 同一条理由，D19）。
        let capacity = self.capacity();
        // ⚠ 一条比整个容量还大的条目**不存**（与 `MemStore` 同一条理由）。
        if size > capacity {
            return;
        }
        let body_len = entry.body.len() as u64;
        if let Err(e) = self.atomic_write(&self.body_path(primary, &secondary), &entry.body, true) {
            warn!("缓存：body 落盘失败（{primary}）：{e}");
            return;
        }

        let vm = VariantMeta {
            secondary: secondary.clone(),
            status: entry.status,
            headers: entry.headers,
            stored_at: entry.stored_at,
            fresh_for: entry.fresh_for,
            cc: entry.cc,
            etag: entry.etag,
            last_modified: entry.last_modified,
            vary: entry.vary,
            body_len,
        };

        {
            let _guard = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
            // ★ hash 撞到别人头上时：`read_family` 已经因为主键对不上返回 `None`，
            //   于是这里会**整族改写**成我们这一族 —— 对方的 body 变成没人认领的垃圾，
            //   由后台清理收走。⚠ 另一种做法是「让我们这条永远存不进去」，更坏。
            let mut fam = self.read_family(primary).unwrap_or(FamilyMeta {
                v: META_VERSION,
                primary: primary.to_string(),
                variants: Vec::new(),
            });
            fam.variants.retain(|v| v.secondary != secondary);
            fam.variants.push(vm);
            if let Err(e) = self.write_family(&fam) {
                warn!("缓存：meta 落盘失败（{primary}）：{e}");
                let _ = fs::remove_file(self.body_path(primary, &secondary));
                return;
            }
        }

        // ── 淘汰 ────────────────────────────────────────────────────────
        //
        // ★ 顺序与 `MemStore::put` **一样**：先把旧的那条减掉、再腾地方、最后插进去。
        //   ⚠ 反过来（先插再腾）会让**刚存进来的这一条**排在 LRU 最前而被自己挤掉 ——
        //   于是「存了又马上删」，下一次请求还会再存一次，永远在原地打转。
        let victims = {
            let mut g = self.lock_index();
            let slot = DiskIndex::slot(primary, &secondary);
            g.remove(&slot);
            let mut out = Vec::new();
            while g.used + size > capacity && !g.lru.is_empty() {
                let victim = g.lru.remove(0);
                if let Some(sz) = g.sizes.remove(&victim) {
                    g.used = g.used.saturating_sub(sz);
                }
                out.push(victim);
            }
            g.insert(slot, size);
            out
        };
        // ⚠ 删文件在锁外做：I/O 拿着索引锁会把别的请求一起堵住。
        for slot in victims {
            let (p, s) = DiskIndex::split(&slot);
            self.drop_variant(&p, &s);
        }
    }

    /// **只更新元数据，不动 body**（G83 那条形状的落点）。
    ///
    /// ★ 重验证（304）是缓存最常见的写操作之一 —— 合成一个文件就得整体重写，
    /// 而一份 body 可能是几 MB。
    pub fn refresh(
        &self,
        primary: &str,
        secondary: &str,
        fresh_for: u64,
        now: i64,
        cc: ResponseCc,
    ) {
        let _guard = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut fam) = self.read_family(primary) else {
            return;
        };
        let Some(v) = fam.variants.iter_mut().find(|v| v.secondary == secondary) else {
            return;
        };
        v.stored_at = now;
        v.fresh_for = fresh_for;
        v.cc = cc;
        if let Err(e) = self.write_family(&fam) {
            warn!("缓存：重验证之后写不回 meta（{primary}）：{e}");
            return;
        }
        drop(_guard);
        self.lock_index()
            .touch(&DiskIndex::slot(primary, secondary));
    }

    /// 按主键清掉一整族。返回清掉几条。
    pub fn purge_primary(&self, primary: &str) -> usize {
        let _guard = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.read_family(primary) {
            Some(fam) => self.drop_family(&fam),
            None => 0,
        }
    }

    /// 按主键前缀清。返回清掉几条。
    ///
    /// ★ ★ ★ **它走盘，不走索引。** 理由是 G84 的直接后果：启动不扫盘 ⇒
    /// 刚重启时索引里几乎是空的，而盘上东西都在。⚠ 一个只看索引的 `purge`
    /// 会在**刚重启之后**清不掉任何东西，还回一句「清掉 0 条」——
    /// 那是一条在最该起作用的时候恰好失效、而且**看起来完全正常**的管理面。
    /// ⇒ purge 的语义是「让它不在」，所以它必须以盘为准。
    pub fn purge_prefix(&self, prefix: &str) -> usize {
        let mut n = 0;
        for fam in self.walk_families() {
            if fam.primary.starts_with(prefix) {
                n += self.drop_family(&fam);
            }
        }
        n
    }

    /// 全清。返回清掉几条。★ 同样走盘，理由同 [`Self::purge_prefix`]。
    pub fn purge_all(&self) -> usize {
        let mut n = 0;
        for fam in self.walk_families() {
            n += self.drop_family(&fam);
        }
        // 顺手把没人认领的 body 也收掉：全清之后盘上不该剩下任何缓存内容。
        for shard in self.leaf_dirs() {
            reap_orphans(&shard, &[]);
        }
        let mut g = self.lock_index();
        *g = DiskIndex::default();
        n
    }

    // ── 后台渐进重建（G84）───────────────────────────────────────────────

    /// 走一小步渐进重建：处理**一个一级分片目录**。
    ///
    /// 返回 `true` = 刚刚走完一整轮。
    ///
    /// ★ ★ 它做两件事，而**第二件只有它能做**：
    /// 1. 把索引里没有的条目补上（占用算得更准，淘汰不再偏晚）；
    /// 2. **收掉没人认领的 body** —— 崩溃在「body 写完、meta 还没写」之间时留下的，
    ///    以及淘汰/撞 hash 时留下的。⚠ 没有这一步，那些文件**永远**不会被删，
    ///    而它们不在索引里 ⇒ 占用算不到 ⇒ 淘汰也永远轮不到它们：
    ///    一处只会涨不会落的磁盘占用，**而缓存本身的行为完全正常**。
    ///
    /// ⚠ ⚠ 它**只加不减**：不去核对索引里的条目是否还在盘上。
    /// 一条已经消失的条目会让占用偏大 ⇒ 淘汰早一点触发 ⇒ 淘汰它时文件早就不在，
    /// `remove_file` 失败被忽略、条目从索引里掉出去。★ 自愈，且不需要一遍 stat 风暴。
    pub fn rebuild_step(&self) -> bool {
        let dir = {
            let mut st = self.rebuild.lock().unwrap_or_else(|e| e.into_inner());
            if !st.started || st.todo.is_empty() {
                st.todo = self.level1_dirs();
                st.started = true;
                if st.todo.is_empty() {
                    st.passes += 1;
                    return true;
                }
            }
            st.todo.pop()
        };
        let Some(dir) = dir else {
            return false;
        };
        for leaf in read_subdirs(&dir) {
            let fams = read_families_in(&leaf);
            let mut keep: Vec<String> = Vec::new();
            for fam in &fams {
                for v in &fam.variants {
                    keep.push(
                        self.body_path(&fam.primary, &v.secondary)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    );
                    let slot = DiskIndex::slot(&fam.primary, &v.secondary);
                    let mut g = self.lock_index();
                    if !g.sizes.contains_key(&slot) {
                        let size = v.footprint();
                        g.insert(slot, size);
                    }
                }
            }
            reap_orphans(&leaf, &keep);
        }
        let done = {
            let mut st = self.rebuild.lock().unwrap_or_else(|e| e.into_inner());
            if st.todo.is_empty() {
                st.passes += 1;
                true
            } else {
                false
            }
        };
        if done {
            self.save_index();
        }
        done
    }

    /// 把淘汰索引写到盘上（**G84** 的 save 那一半）。
    pub fn save_index(&self) {
        let snapshot = {
            let g = self.lock_index();
            serde_json::to_vec(&*g)
        };
        match snapshot {
            Ok(bytes) => {
                if let Err(e) = self.atomic_write(&self.root.join("index.json"), &bytes, false) {
                    warn!("缓存：淘汰索引写不下去（{}）：{e}", self.root.display());
                }
            }
            Err(e) => warn!("缓存：淘汰索引序列化失败：{e}"),
        }
    }

    // ── 目录遍历 ────────────────────────────────────────────────────────

    fn level1_dirs(&self) -> Vec<PathBuf> {
        read_subdirs(&self.root)
            .into_iter()
            // `tmp/` 不是分片目录。
            .filter(|p| p.file_name().is_some_and(|n| n != "tmp"))
            .collect()
    }

    fn leaf_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for l1 in self.level1_dirs() {
            out.extend(read_subdirs(&l1));
        }
        out
    }

    /// 走遍盘上全部 meta。★ 只给管理面那两条用 —— 它慢，而且慢得有道理。
    fn walk_families(&self) -> Vec<FamilyMeta> {
        let mut out = Vec::new();
        for leaf in self.leaf_dirs() {
            out.extend(read_families_in(&leaf));
        }
        out
    }
}

/// 读一个目录下的子目录。★ 读不出来就当空 —— 缓存里最坏的情况是多回一次源。
fn read_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    // ★ 排序只为让遍历顺序可复现：一条走盘的判据在不同机器上该看到同一份结果。
    out.sort();
    out
}

fn read_families_in(leaf: &Path) -> Vec<FamilyMeta> {
    let Ok(rd) = fs::read_dir(leaf) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "meta"))
        .collect();
    paths.sort();
    for p in paths {
        match fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<FamilyMeta>(&b).ok())
        {
            Some(fam) if fam.v == META_VERSION => out.push(fam),
            // ⚠ 解析不出来 / 版本不对 ⇒ **删掉 meta**。它的 body 随即变成
            //   没人认领的，由同一轮的 `reap_orphans` 收走 —— 两步都在这一趟里做完。
            _ => {
                let _ = fs::remove_file(&p);
            }
        }
    }
    out
}

/// 收掉这个叶子目录里**没人认领**的 body 文件。
fn reap_orphans(leaf: &Path, keep: &[String]) {
    let Ok(rd) = fs::read_dir(leaf) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "body") {
            continue;
        }
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !keep.contains(&name) {
            debug!("缓存：收掉一个没人认领的 body：{}", p.display());
            let _ = fs::remove_file(&p);
        }
    }
}

/// 读回淘汰索引。
///
/// ★ 没有 / 坏了都当空 —— 后台渐进重建会把它补齐，
/// 而在补齐之前**读路径照常命中**（它不看索引）。
fn load_index(root: &Path) -> DiskIndex {
    fs::read(root.join("index.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<DiskIndex>(&b).ok())
        .unwrap_or_default()
}

fn next_tmp_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个用完就删的临时目录。
    ///
    /// ⚠ ⚠ **有意不按 pid 派生名字**：pid 会被复用，而一个「按 pid 命名且从不清理」
    /// 的测试目录，会让今天这一跑打开**上一次**留下的状态 —— 那是一条
    /// 时红时绿的判据，而它教人「红了先重跑一次」。
    /// ⇒ 名字里带一个进程内自增号 + 纳秒时间戳，并且**用 `create_dir` 而不是
    /// `create_dir_all`**：撞上了就当场失败，而不是安静地复用。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "fulcrum-disk-{tag}-{}-{}-{nanos}",
                std::process::id(),
                next_tmp_seq()
            ));
            fs::create_dir(&p).expect("临时目录撞名了 —— 那说明命名不够唯一，不能当没看见");
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(body: &[u8], vary: Vec<String>) -> Entry {
        Entry {
            status: 200,
            headers: vec![("X-A".to_string(), "b".to_string())],
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

    fn open(d: &TempDir, cap: u64) -> DiskStore {
        DiskStore::open(&d.0, cap).expect("打不开磁盘缓存")
    }

    // ── D19：磁盘后端的容量同样改得动 ──────────────────────────────────────
    //
    // ⚠ ⚠ **它必须与 `MemStore` 那两条配成对**：两个后端对 `capacity` 的三个读点
    //   （getter · 大条目守卫 · 淘汰循环）形状**逐字相同**，⇒ 只测一个的话，
    //   另一个漏改了照样全绿，而症状是「换了磁盘后端就不生效」这种最难查的形状。
    #[test]
    fn 容量在线改得动_磁盘后端同样() {
        let d = TempDir::new("setcap");
        let s = DiskStore::open(&d.0, 10).unwrap();
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert!(
            matches!(s.get("k", none), Lookup::Miss),
            "前提：10 字节容量下 100 字节的条目不该存"
        );
        s.set_capacity(4096);
        assert_eq!(s.capacity(), 4096);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert!(
            matches!(s.get("k", none), Lookup::Hit(_)),
            "容量改大之后该存得下 —— 说明磁盘那边的守卫读的也是新值"
        );
    }

    #[test]
    fn 存了能取回来() {
        let d = TempDir::new("basic");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"hello", vec![]));
        match s.get("k", none) {
            Lookup::Hit(e) => {
                assert_eq!(e.body, b"hello");
                assert_eq!(e.headers, vec![("X-A".to_string(), "b".to_string())]);
            }
            other => panic!("该命中，实际 {other:?}"),
        }
        assert!(matches!(s.get("nope", none), Lookup::Miss));
    }

    // ★ ★ ★ **本模块存在的理由，一条判据说完**：换一个进程（这里换一个 `DiskStore`
    //   实例，且**没有**任何进程内状态传过去）之后，东西还在。
    //   ⚠ 一个内存后端在这条判据上必然红 —— 也就是说它分得清「内存」与「磁盘」，
    //   而 `X-Fulcrum-Cache` 那个头分不清（那个头信的是配置说了什么）。
    #[test]
    fn 换一个实例之后东西还在() {
        let d = TempDir::new("persist");
        {
            let s = open(&d, 1 << 20);
            s.put("k", String::new(), entry(b"survives", vec![]));
        }
        let s2 = open(&d, 1 << 20);
        match s2.get("k", none) {
            Lookup::Hit(e) => assert_eq!(e.body, b"survives"),
            other => panic!("重开之后该命中，实际 {other:?}"),
        }
    }

    // ★ ★ G84 的正面判据：**新实例的索引是空的**（启动不扫盘），
    //   ⚠ 而**读路径照样命中** —— 上面那条已经证了后半句，这条证前半句。
    //   合起来才说明「不扫盘」不是靠牺牲命中换来的。
    #[test]
    fn 启动不扫盘_索引是空的而读照样命中() {
        let d = TempDir::new("noscan");
        {
            let s = open(&d, 1 << 20);
            for i in 0..5 {
                s.put(&format!("k{i}"), String::new(), entry(b"x", vec![]));
            }
            assert_eq!(s.stats().1, 5);
        }
        // ⚠ 把索引文件删掉，模拟「上一次没来得及 save」。
        let _ = fs::remove_file(d.0.join("index.json"));
        let s2 = open(&d, 1 << 20);
        assert_eq!(s2.stats(), (0, 0), "启动时不该扫盘（索引应当是空的）");
        assert!(
            matches!(s2.get("k3", none), Lookup::Hit(_)),
            "索引空归空，读路径必须照样命中 —— 否则一次重启等于一次全量回源"
        );
    }

    // ★ 后台渐进重建把索引补齐（G84 的第二半）。
    #[test]
    fn 渐进重建把索引补齐() {
        let d = TempDir::new("rebuild");
        {
            let s = open(&d, 1 << 20);
            for i in 0..8 {
                s.put(&format!("k{i}"), String::new(), entry(b"xyz", vec![]));
            }
        }
        let _ = fs::remove_file(d.0.join("index.json"));
        let s2 = open(&d, 1 << 20);
        assert_eq!(s2.stats().1, 0);
        let mut guard = 0;
        while !s2.rebuild_step() {
            guard += 1;
            assert!(guard < 1000, "渐进重建走不完");
        }
        assert_eq!(s2.stats().1, 8, "走完一轮之后索引该是全的");
        assert!(s2.stats().0 > 0, "占用也该算出来");
    }

    // ★ ★ ★ 没人认领的 body 必须被收掉。
    //   ⚠ 它不在索引里 ⇒ 占用算不到 ⇒ 淘汰永远轮不到它：一处**只涨不落**的磁盘占用，
    //   而缓存本身的行为完全正常。**除了这一步，没有任何东西会删它。**
    #[test]
    fn 没人认领的_body_会被收掉() {
        let d = TempDir::new("orphan");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"real", vec![]));
        // 手工造一个孤儿：与真条目同一个叶子目录，但没有任何 meta 提到它。
        let orphan = s.body_path("k", "\u{1}没人提过的次级键");
        fs::write(&orphan, b"garbage").unwrap();
        assert!(orphan.exists());
        while !s.rebuild_step() {}
        assert!(!orphan.exists(), "孤儿 body 没被收掉");
        assert!(
            matches!(s.get("k", none), Lookup::Hit(_)),
            "收孤儿的时候把真条目也误伤了"
        );
    }

    // ★ ★ 读时校验（G84）：body 与 meta 对不上 ⇒ 即丢即删，当成未命中。
    //   ⚠ 不校验的话，一份被截断的 body 会被**原样发给客户端** —— 长度对不上、
    //   状态码 200，而客户端拿到半个页面。
    #[test]
    fn body_与_meta_对不上就即丢即删() {
        let d = TempDir::new("validate");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"0123456789", vec![]));
        let bp = s.body_path("k", "");
        fs::write(&bp, b"012").unwrap(); // 截断
        assert!(matches!(s.get("k", none), Lookup::Miss), "坏条目该当未命中");
        assert!(!bp.exists(), "坏条目该被删掉");
        // 再查一次仍然是 Miss，而且不该 panic。
        assert!(matches!(s.get("k", none), Lookup::Miss));
    }

    // ★ body 整个不见了（meta 还在）—— 崩溃恢复里另一半的形态。
    #[test]
    fn body_不见了也是即丢即删() {
        let d = TempDir::new("nobody");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"abc", vec![]));
        fs::remove_file(s.body_path("k", "")).unwrap();
        assert!(matches!(s.get("k", none), Lookup::Miss));
        assert!(
            !s.meta_path("k").exists(),
            "最后一个变体没了，meta 该一起走"
        );
    }

    // ★ ★ Vary：两个分支共存、互不挤掉，而且各拿各的那一份。
    #[test]
    fn vary_两个分支共存() {
        let d = TempDir::new("vary");
        let s = open(&d, 1 << 20);
        let vary = vec!["x-flavor".to_string()];
        for f in ["a", "b"] {
            let sk = super::super::key::secondary(&vary, |_| Some(f));
            s.put("k", sk, entry(f.as_bytes(), vary.clone()));
        }
        for f in ["a", "b"] {
            match s.get("k", |_| Some(f)) {
                Lookup::Hit(e) => assert_eq!(e.body, f.as_bytes(), "{f} 拿到了别人那一份"),
                other => panic!("{f} 该命中，实际 {other:?}"),
            }
        }
        // 一个没写过的 flavor ⇒ VaryMiss（不是 Miss）——两种未命中要分得开。
        match s.get("k", |_| Some("c")) {
            Lookup::VaryMiss { vary: v } => assert_eq!(v, vary),
            other => panic!("该是 VaryMiss，实际 {other:?}"),
        }
    }

    // ★ ★ refresh 只改 meta、**不动 body**（G83 那条形状的判据）。
    //   ⚠ 它同时钉住「body 文件的修改时间没变」—— 只比内容的话，
    //   一个「整条重写」的实现照样能给绿。
    #[test]
    fn refresh_只改元数据不动_body_文件() {
        let d = TempDir::new("refresh");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"original", vec![]));
        let bp = s.body_path("k", "");
        let before = fs::metadata(&bp).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        s.refresh("k", "", 999, 12345, ResponseCc::default());

        let after = fs::metadata(&bp).unwrap().modified().unwrap();
        assert_eq!(before, after, "重验证动了 body 文件（G83 说的就是别动它）");
        match s.get("k", none) {
            Lookup::Hit(e) => {
                assert_eq!(e.body, b"original");
                assert_eq!(e.fresh_for, 999);
                assert_eq!(e.stored_at, 12345);
            }
            other => panic!("该命中，实际 {other:?}"),
        }
    }

    #[test]
    fn purge_三种粒度() {
        let d = TempDir::new("purge");
        let s = open(&d, 1 << 20);
        for k in ["p/a", "p/b", "q/c"] {
            s.put(k, String::new(), entry(b"x", vec![]));
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

    // ★ ★ ★ **purge 必须以盘为准，不是以索引为准。**
    //   ⚠ 一个只看索引的实现，在**刚重启之后**（索引空、盘上东西都在）
    //   会清不掉任何东西，还回一句「清掉 0 条」——
    //   而那正是有人会去按 purge 的那个时刻。
    #[test]
    fn 索引是空的时候_purge_照样清得掉() {
        let d = TempDir::new("purge-cold");
        {
            let s = open(&d, 1 << 20);
            s.put("p/a", String::new(), entry(b"x", vec![]));
            s.put("p/b", String::new(), entry(b"y", vec![]));
        }
        let _ = fs::remove_file(d.0.join("index.json"));
        let s2 = open(&d, 1 << 20);
        assert_eq!(s2.stats().1, 0, "前提：索引确实是空的");
        assert_eq!(s2.purge_prefix("p/"), 2, "索引空的时候 purge 前缀清不掉");
        assert!(matches!(s2.get("p/a", none), Lookup::Miss));
        assert!(matches!(s2.get("p/b", none), Lookup::Miss));
    }

    #[test]
    fn 超过容量的单条不存() {
        let d = TempDir::new("toobig");
        let s = open(&d, 10);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert!(matches!(s.get("k", none), Lookup::Miss));
        assert_eq!(s.stats(), (0, 0));
    }

    // ★ LRU：装满之后淘汰**最久没用**的那条，而且盘上的文件真的少了。
    #[test]
    fn 淘汰的是最久没用的而且文件真的被删了() {
        let d = TempDir::new("lru");
        // 每条 ≈ 10（体）+ 头开销；给一个刚好放得下三条的容量。
        let one = entry(&[0u8; 10], vec![]).footprint();
        let s = open(&d, one * 3);
        for k in ["a", "b", "c"] {
            s.put(k, String::new(), entry(&[0u8; 10], vec![]));
        }
        assert_eq!(s.stats().1, 3);
        assert!(matches!(s.get("a", none), Lookup::Hit(_))); // a 变成最近用过的
        let bp_b = s.body_path("b", "");
        s.put("d", String::new(), entry(&[0u8; 10], vec![]));

        assert!(matches!(s.get("a", none), Lookup::Hit(_)), "a 刚用过");
        assert!(matches!(s.get("b", none), Lookup::Miss), "b 最久没用，该走");
        assert!(
            !bp_b.exists(),
            "淘汰只从索引里删、文件留在盘上 = 磁盘只涨不落"
        );
        assert!(matches!(s.get("c", none), Lookup::Hit(_)));
        assert!(matches!(s.get("d", none), Lookup::Hit(_)));
    }

    #[test]
    fn 占用不越界() {
        let d = TempDir::new("cap");
        let s = open(&d, 500);
        for i in 0..50 {
            s.put(&format!("k{i}"), String::new(), entry(&[0u8; 20], vec![]));
            let (used, _) = s.stats();
            assert!(used <= 500, "第 {i} 次之后占用 {used} 越过了上限");
        }
    }

    #[test]
    fn 重存同一个键占用不翻倍() {
        let d = TempDir::new("rewrite");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        let a = s.stats();
        s.put("k", String::new(), entry(&[0u8; 100], vec![]));
        assert_eq!(a, s.stats(), "重存把占用算了两遍");
    }

    // ★ ★ 两级分片：目录形状是 G83 定死的，钉住它。
    //   ⚠ 单层目录在几十万文件时会把清理任务本身拖垮 —— 那件事在门禁里
    //   永远不会发生（门禁只存几条），所以**只能**在这里钉。
    #[test]
    fn 两级分片目录() {
        let d = TempDir::new("shard");
        let s = open(&d, 1 << 20);
        s.put("some/key", String::new(), entry(b"x", vec![]));
        let mp = s.meta_path("some/key");
        let rel = mp.strip_prefix(&d.0).unwrap();
        let parts: Vec<_> = rel.components().collect();
        assert_eq!(parts.len(), 3, "该是 <aa>/<bb>/<hash>.meta，实际 {rel:?}");
        for p in &parts[0..2] {
            assert_eq!(
                p.as_os_str().len(),
                2,
                "分片目录名该是两个 hex 字符：{rel:?}"
            );
        }
    }

    // ★ hash 撞了也不许给错内容 —— 正确性不建在「不会撞」上面。
    //   ⚠ 这里直接把一份「别人的」meta 写到我们这个键的文件名上，
    //   模拟一次撞车：结果必须是未命中，绝不能把那份内容发出去。
    #[test]
    fn hash_撞了也不给错内容() {
        let d = TempDir::new("collide");
        let s = open(&d, 1 << 20);
        s.put("mine", String::new(), entry(b"mine-body", vec![]));
        let mp = s.meta_path("mine");
        let mut fam: FamilyMeta = serde_json::from_slice(&fs::read(&mp).unwrap()).unwrap();
        fam.primary = "someone-else".to_string();
        fs::write(&mp, serde_json::to_vec(&fam).unwrap()).unwrap();
        assert!(
            matches!(s.get("mine", none), Lookup::Miss),
            "主键对不上却给了内容 —— 那就是把别人的页面发给了这个人"
        );
    }

    // ★ meta 版本对不上 ⇒ 当没有（换代之后冷启动一次，而不是按新语义读旧数据）。
    #[test]
    fn meta_版本对不上就当没有() {
        let d = TempDir::new("ver");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"x", vec![]));
        let mp = s.meta_path("k");
        let raw = fs::read_to_string(&mp).unwrap();
        fs::write(&mp, raw.replace("\"v\":1", "\"v\":999")).unwrap();
        assert!(matches!(s.get("k", none), Lookup::Miss));
    }

    // ★ 坏掉的 meta 会被后台清理收走，连同它的 body。
    #[test]
    fn 坏掉的_meta_会被收走() {
        let d = TempDir::new("badmeta");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"x", vec![]));
        let mp = s.meta_path("k");
        let bp = s.body_path("k", "");
        fs::write(&mp, b"{ this is not json").unwrap();
        while !s.rebuild_step() {}
        assert!(!mp.exists(), "坏 meta 没被删");
        assert!(!bp.exists(), "坏 meta 的 body 没被当成孤儿收走");
    }

    // ★ 索引 save/load 走得通（G84 的那一半）。
    #[test]
    fn 索引存得下也读得回来() {
        let d = TempDir::new("index");
        let (used, n) = {
            let s = open(&d, 1 << 20);
            for i in 0..4 {
                s.put(&format!("k{i}"), String::new(), entry(&[0u8; 30], vec![]));
            }
            s.save_index();
            s.stats()
        };
        assert!(d.0.join("index.json").exists(), "索引文件没写出来");
        let s2 = open(&d, 1 << 20);
        assert_eq!(s2.stats(), (used, n), "读回来的索引与存下去的对不上");
    }

    // ⚠ 目录不可用时 `open` 必须**失败**，而不是回一个什么都不做的实例。
    //   ★ 这条是「静默失能」的正面判据：调用方要有机会说出这件事。
    #[test]
    fn 目录不可用时_open_失败() {
        let d = TempDir::new("badroot");
        // 用一个**文件**当缓存根：create_dir_all 会失败。
        let f = d.0.join("iam-a-file");
        fs::write(&f, b"x").unwrap();
        assert!(DiskStore::open(&f, 1 << 20).is_err());
    }
}
