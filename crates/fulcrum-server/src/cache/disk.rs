//! 磁碟後端（**M2 批 H**，形狀由 **G83 / G84** 定死）。
//!
//! ## 形狀不是本批拍的，是 D8 結案時拍的
//!
//! | | G83/G84 定下來的 | 這裡落在哪 |
//! |---|---|---|
//! | 分片 | 快取鍵 hash 的**兩級分片目錄** | [`DiskStore::shard_of`] |
//! | 佈局 | **meta 與 body 兩個檔案** | [`FamilyMeta`] / `*.body` |
//! | 完成實作 | `tmp` 後 `rename` | [`DiskStore::atomic_write`] |
//! | 重驗證 | **只改 meta 不動 body** | [`DiskStore::refresh`] |
//! | 崩潰恢復 | **啟動不掃盤**：讀時檢核 + 後台漸進重建 | [`DiskStore::get`] / [`DiskStore::rebuild_step`] |
//! | `purge` | 走管理面（批 G 已做）| `admin.rs` |
//!
//! ## ⚠ 一處偏離：meta 是**一族一個**，不是一條一個
//!
//! body 是一條一個，meta 是一個**主鍵**一個（裝著該主鍵下全部 `Vary` 變體）。
//! 沒有 `Vary` 時 —— 絕大多數回應 —— 它就是「兩個檔案」。
//! ★ 換不了：請求到達時我們**還不知道 `Vary`**（那是回應告訴我們的），
//! 於是次級鍵算不出來、檔案名拼不出來。
//! ★ 而 G83 給出的**理由**（「重驗證只改 meta 不動 body」）在這裡一字不差地成立。
//!
//! ## ⚠ ⚠ hash 會撞，撞了的後果是「把別人的內容發給你」
//!
//! **正確性不建在「不會撞」上面**：[`FamilyMeta::primary`] 存**主鍵原文**，
//! 查的時候逐字比一遍，對不上就當沒有。
//!
//! ## ⚠ 磁碟 I/O 是**同步**的，代價寫在明處
//!
//! 七個操作的簽名與 [`super::store::MemStore`] 逐字相同，而那組簽名是同步的。
//! 改成 async 等於把介面劈成兩份 ⇒ 資料面要為「記憶體還是磁碟」分出兩條路。
//! ⚠ **代價**：一次冷盤讀會佔住一條 tokio 工作執行緒。管理面那兩條會走遍整棵目錄樹，
//! 所以它們在呼叫處包了 `block_in_place`；請求路徑上唯讀兩個小檔案，沒有包。

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

/// meta 檔案的格式版本。
///
/// ★ ★ 它不是裝飾：加一個欄位之後，舊處理程序寫下的 meta 與新程式碼**長得不一樣**，
/// 而 serde 對著一份少了欄位的 JSON 會給出一個**看起來很正常的錯誤**或者一份
/// 半對的結構。⇒ 版本對不上就當這條不存在（當成未命中，後台清理會收走它）。
/// ⚠ 代價認下：改這個號 = 換代之後快取整個冷啟動一次。那是**該付**的代價 ——
/// 另一種做法是「儘量相容」，而它的失敗形態是把一份舊語義的中繼資料按新語義解釋。
const META_VERSION: u32 = 1;

/// 一族（同一個主鍵）的中繼資料。★ 與 body **分開**的那一半（G83）。
#[derive(Debug, Serialize, Deserialize)]
struct FamilyMeta {
    v: u32,
    /// ⚠ **主鍵原文**。檔案名是 hash，而 hash 會撞 —— 這一行是撞了之後唯一的防線。
    primary: String,
    variants: Vec<VariantMeta>,
}

/// 一個 `Vary` 變體的中繼資料。
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
    /// body 檔案**應該**有多長。★ 「讀時檢核」（G84）就是拿它比的。
    body_len: u64,
}

impl VariantMeta {
    /// 這條佔多少位元組（口徑與 [`Entry::footprint`] 必須一致）。
    ///
    /// ⚠ ⚠ 兩處各算各的話，`put` 加進去的數與 `evict` 減掉的數會**慢慢分家**，
    /// 而現場是「快取越用越滿，最後一條都存不進去」，`stats()` 裡的條數卻是對的。
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
/// ★ ★ **有意不用 `DefaultHasher`**：`std` 明確不保證它跨版本穩定，而這裡的 hash
/// 決定的是**磁碟上的檔案名** —— 換一次工具鏈，全盤快取就集體改名，
/// ⚠ 而現場表現是「升級之後快取命中率掉到 0」，沒有任何東西會說為什麼。
/// FNV-1a 寫死在這裡，它十年後還是同一個值。
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

/// 淘汰索引（**G84**：走 save/load，不靠啟動掃盤）。
///
/// ⚠ ⚠ **它不在讀路徑上。** 讀走的是「按鍵算出檔案名、直接開」那條路 ——
/// 於是**重新啟動之後第一個請求就能命中磁碟上的東西**，而不必等索引重建完。
/// ★ 索引只管一件事：淘汰時知道該先扔誰、以及現在佔了多少。
/// ⇒ 索引不全的代價就是 G84 寫在明處的那一條：**剛重新啟動時少算佔用，淘汰比穩態晚一點**。
#[derive(Debug, Default, Serialize, Deserialize)]
struct DiskIndex {
    /// `(主键, 次级键) → 占用字节`。
    sizes: HashMap<String, u64>,
    /// LRU 順序：最近用過的排在後面。存 `"主键\u{1}次级键"`。
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

/// 後台漸進重建的進度（**G84**）。
#[derive(Debug, Default)]
struct RebuildState {
    /// 本輪要走的一級分片目錄，倒序彈出。
    todo: Vec<PathBuf>,
    /// 起過一輪沒有。
    started: bool,
    /// 走完過幾輪（給紀錄與判據用）。
    passes: u64,
}

/// 磁碟快取。★ 七個操作的簽名與 [`super::store::MemStore`] **逐字相同**。
#[derive(Debug)]
pub struct DiskStore {
    root: PathBuf,
    /// ★ 原子而不是普通欄位 —— 與 [`super::store::MemStore`] 同一條理由（D19）。
    /// ⚠ ⚠ **兩個後端必須一起改**：它們對 `capacity` 的三個讀點形狀逐字相同，
    /// 只改一個的話症狀是「換成磁碟後端就不生效」，那是最難查的那種形狀。
    capacity: AtomicU64,
    index: Mutex<DiskIndex>,
    /// meta 的「讀—改—寫」互斥。
    ///
    /// ⚠ 沒有它的話，兩個並行 `put` 會各自讀到同一份舊 meta、各寫一份新的，
    /// **後寫的那份把先寫的那個變體抹掉** —— 而它的 body 檔案還在盤上，
    /// 於是變成一條誰也找不到的垃圾。★ 那不是「給錯內容」，只是浪費，
    /// 但它是一條**不會被任何判據看見**的浪費，所以在結構上堵掉。
    meta_lock: Mutex<()>,
    rebuild: Mutex<RebuildState>,
}

impl DiskStore {
    /// 打開（或建出）一個磁碟快取。
    ///
    /// ★ ★ **啟動不掃盤**（G84）：這裡只建目錄、讀一次索引檔案，都是 O(1)。
    /// ⚠ 它與 G78 的 `sd_notify(READY=1)` 是同一件事的兩面 —— 換代要求新一代
    /// **快速就緒**，而全盤掃描的時間隨快取大小線性增長。
    pub fn open(root: &Path, capacity: u64) -> std::io::Result<DiskStore> {
        fs::create_dir_all(root)?;
        fs::create_dir_all(root.join("tmp"))?;
        // ★ ★ **真寫一次**再說這個目錄能用。
        //   ⚠ `create_dir_all` 對一個**已經存在但不可寫**的目錄是成功的 ——
        //   於是「目錄準備好了」這句話在最該報警的那一刻恰好成立，
        //   而第一次 `put` 才失敗，那時已經在收流量了。
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

    /// 換一個容量上限（`POST /load` 走這裡，D19）。
    ///
    /// ★ ★ **同樣有意不當場淘汰**，而磁碟這邊的理由更硬一層：當場收緊要**刪檔案**，
    /// 那會把 `POST /load` 的耗時掛到盤上去。⇒ 下一次 `put` 的淘汰循環收緊它。
    /// ⚠ 代價與記憶體後端逐字相同，判據見本檔案測試 `容量在线改得动_磁盘后端同样`
    /// 與 `store.rs` 的 `调小容量是惰性的_下一次写入才把占用压回去`。
    pub fn set_capacity(&self, capacity: u64) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    /// 當前佔用與條目數。
    ///
    /// ⚠ ⚠ **剛重新啟動時它會偏小**，而盤上的東西照樣命中得到 —— 這不是缺陷，
    /// 是 G84 寫在明處的那個「最終一致視窗」。後台漸進重建會把它補齊。
    pub fn stats(&self) -> (u64, usize) {
        let g = self.lock_index();
        (g.used, g.lru.len())
    }

    /// 走完過幾輪漸進重建（給裝載紀錄與判據用）。
    pub fn rebuild_passes(&self) -> u64 {
        self.rebuild.lock().map(|g| g.passes).unwrap_or(0)
    }

    // ⚠ 鎖中毒不 panic：一個 panic 過的快取不該把**整個服務**也拖下水
    //   （與 `MemStore` 同款，理由也一樣：快取裡最壞的情況是多回一次源）。
    fn lock_index(&self) -> std::sync::MutexGuard<'_, DiskIndex> {
        self.index.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── 路徑 ────────────────────────────────────────────────────────────────

    /// 兩級分片目錄（G83）。★ nginx 與 Caddy 都是這個形狀，理由是同一個：
    /// ⚠ 單目錄幾十萬檔案時，`readdir` 與 `unlink` 的代價會把清理任務本身拖垮。
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

    // ── 原子完成實作（G83：`tmp` 後 `rename`）──────────────────────────────────

    /// 寫一個檔案：先寫 `tmp/`，再 `rename` 到位。
    ///
    /// ★ `rename(2)` 在同一個檔案系統內是原子的 —— 讀的人要麼看見舊的、要麼看見新的，
    /// **永遠不會看見寫到一半的**。⚠ 這也是 `tmp/` 必須在快取根**裡面**的原因：
    /// 跨檔案系統的 rename 會退化成「拷貝 + 刪除」，那就不原子了。
    ///
    /// `fsync` 只對 **body** 做，不對 meta 做，理由見 [`DiskStore::put`]。
    fn atomic_write(&self, dst: &Path, bytes: &[u8], fsync: bool) -> std::io::Result<()> {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        // ★ 臨時名帶上處理程序號與一個自增序號：同一個處理程序裡兩次並行完成實作不能撞名，
        //   而撞了的後果是其中一次寫到另一次的半成品上。
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
        // ★ 版本對不上、或者 hash 撞到了別人頭上 —— 兩種都當「這裡沒有」。
        if fam.v != META_VERSION || fam.primary != primary {
            return None;
        }
        Some(fam)
    }

    fn write_family(&self, fam: &FamilyMeta) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(fam).map_err(std::io::Error::other)?;
        self.atomic_write(&self.meta_path(&fam.primary), &bytes, false)
    }

    /// 整族抹掉（meta + 全部 body）。回傳抹掉幾條。
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

    /// 把一個變體從它那一族裡摘掉（讀時檢核失敗、或淘汰時走這裡）。
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

    // ── 七個操作（與 `MemStore` 逐字同簽名）──────────────────────────────

    /// 查一條。
    ///
    /// ★ ★ ★ **這條路不看索引**：按鍵算出檔案名直接開 —— 於是「啟動不掃盤」（G84）
    /// 與「重新啟動之後立刻還能命中」這兩件事**同時**成立。
    /// ⚠ 把讀路徑掛到索引上的寫法，會讓一次重新啟動等於一次全量回源，
    /// 而那恰恰是磁碟快取存在的理由。
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
            // ★ 逐條按**它自己的** vary 算次級鍵，不假設一族裡都一樣
            //   （與 `MemStore` 同一條：上游改了 Vary 的那一刻，假設就開始命中錯的條目）。
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

        // ── ★ ★ 讀時檢核（G84）───────────────────────────────────────────
        //   ⚠ 「meta 說有、body 不在或長度對不上」是崩潰恢復必然會遇到的狀態
        //   （body 完成實作了而 meta 還沒、或者反過來）。**即丟即刪**，當成未命中。
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
            // ⚠ 索引裡可能根本沒有這一條（剛重新啟動、後台還沒走到這一格）——
            //   ★ 那就**順手把它記上**：讀路徑本來就已經把這條的大小算出來了，
            //   等後台走到這裡再記一遍是白等一輪。
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

    /// 存一條。
    ///
    /// 完成實作順序是 **body 先、meta 後**，這一點不是隨手排的：
    /// ★ meta 是**唯一的索引**，所以「meta 在而 body 不在」會被讀時檢核當場收掉，
    /// 而「body 在而 meta 不提它」只是一條沒人認領的垃圾 —— 由後台清理收走。
    /// ⇒ 先寫 body 讓崩潰落在**便宜**的那一邊。
    ///
    /// `fsync` **只給 body**：
    /// ⚠ 一份長度正確、內容是垃圾的 body 會被原樣發給用戶端 —— 那正是
    /// 「偶爾給錯內容」。而一份壞掉的 meta 解析不出來，當場就被當成沒有。
    /// ⇒ 把這一次 fsync 花在真正要命的那一邊，不給 meta 再花一次。
    pub fn put(&self, primary: &str, secondary: String, entry: Entry) {
        let size = entry.footprint();
        // ★ 容量在這一次 put 裡唯讀一次（與 `MemStore` 同一條理由，D19）。
        let capacity = self.capacity();
        // ⚠ 一條比整個容量還大的條目**不存**（與 `MemStore` 同一條理由）。
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
            // ★ hash 撞到別人頭上時：`read_family` 已經因為主鍵對不上回傳 `None`，
            //   於是這裡會**整族改寫**成我們這一族 —— 對方的 body 變成沒人認領的垃圾，
            //   由後台清理收走。⚠ 另一種做法是「讓我們這條永遠存不進去」，更壞。
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
        // ★ 順序與 `MemStore::put` **一樣**：先把舊的那條減掉、再騰地方、最後插進去。
        //   ⚠ 反過來（先插再騰）會讓**剛存進來的這一條**排在 LRU 最前而被自己擠掉 ——
        //   於是「存了又馬上刪」，下一次請求還會再存一次，永遠在原地打轉。
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
        // ⚠ 刪檔案在鎖外做：I/O 拿著索引鎖會把別的請求一起堵住。
        for slot in victims {
            let (p, s) = DiskIndex::split(&slot);
            self.drop_variant(&p, &s);
        }
    }

    /// **只更新中繼資料，不動 body**（G83 那條形狀的落點）。
    ///
    /// ★ 重驗證（304）是快取最常見的寫操作之一 —— 合成一個檔案就得整體重寫，
    /// 而一份 body 可能是幾 MB。
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

    /// 按主鍵清掉一整族。回傳清掉幾條。
    pub fn purge_primary(&self, primary: &str) -> usize {
        let _guard = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.read_family(primary) {
            Some(fam) => self.drop_family(&fam),
            None => 0,
        }
    }

    /// 按主鍵前綴清。回傳清掉幾條。
    ///
    /// ★ ★ ★ **它走盤，不走索引。** 理由是 G84 的直接後果：啟動不掃盤 ⇒
    /// 剛重新啟動時索引裡幾乎是空的，而盤上東西都在。⚠ 一個只看索引的 `purge`
    /// 會在**剛重新啟動之後**清不掉任何東西，還回一句「清掉 0 條」——
    /// 那是一條在最該起作用的時候恰好失效、而且**看起來完全正常**的管理面。
    /// ⇒ purge 的語義是「讓它不在」，所以它必須以盤為準。
    pub fn purge_prefix(&self, prefix: &str) -> usize {
        let mut n = 0;
        for fam in self.walk_families() {
            if fam.primary.starts_with(prefix) {
                n += self.drop_family(&fam);
            }
        }
        n
    }

    /// 全清。回傳清掉幾條。★ 同樣走盤，理由同 [`Self::purge_prefix`]。
    pub fn purge_all(&self) -> usize {
        let mut n = 0;
        for fam in self.walk_families() {
            n += self.drop_family(&fam);
        }
        // 順手把沒人認領的 body 也收掉：全清之後盤上不該剩下任何快取內容。
        for shard in self.leaf_dirs() {
            reap_orphans(&shard, &[]);
        }
        let mut g = self.lock_index();
        *g = DiskIndex::default();
        n
    }

    // ── 後台漸進重建（G84）───────────────────────────────────────────────

    /// 走一小步漸進重建：處理**一個一級分片目錄**。
    ///
    /// 回傳 `true` = 剛剛走完一整輪。
    ///
    /// ★ ★ 它做兩件事，而**第二件只有它能做**：
    /// 1. 把索引裡沒有的條目補上（佔用算得更準，淘汰不再偏晚）；
    /// 2. **收掉沒人認領的 body** —— 崩潰在「body 寫完、meta 還沒寫」之間時留下的，
    ///    以及淘汰/撞 hash 時留下的。⚠ 沒有這一步，那些檔案**永遠**不會被刪，
    ///    而它們不在索引裡 ⇒ 佔用算不到 ⇒ 淘汰也永遠輪不到它們：
    ///    一處只會漲不會落的磁碟佔用，**而快取本身的行為完全正常**。
    ///
    /// ⚠ ⚠ 它**只加不減**：不去核對索引裡的條目是否還在盤上。
    /// 一條已經消失的條目會讓佔用偏大 ⇒ 淘汰早一點觸發 ⇒ 淘汰它時檔案早就不在，
    /// `remove_file` 失敗被忽略、條目從索引裡掉出去。★ 自愈，且不需要一遍 stat 風暴。
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

    /// 把淘汰索引寫到盤上（**G84** 的 save 那一半）。
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

    // ── 目錄遍歷 ────────────────────────────────────────────────────────

    fn level1_dirs(&self) -> Vec<PathBuf> {
        read_subdirs(&self.root)
            .into_iter()
            // `tmp/` 不是分片目錄。
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

    /// 走遍盤上全部 meta。★ 只給管理面那兩條用 —— 它慢，而且慢得有道理。
    fn walk_families(&self) -> Vec<FamilyMeta> {
        let mut out = Vec::new();
        for leaf in self.leaf_dirs() {
            out.extend(read_families_in(&leaf));
        }
        out
    }
}

/// 讀一個目錄下的子目錄。★ 讀不出來就當空 —— 快取裡最壞的情況是多回一次源。
fn read_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    // ★ 排序只為讓遍歷順序可重現：一條走盤的判據在不同機器上該看到同一份結果。
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
            // ⚠ 解析不出來 / 版本不對 ⇒ **刪掉 meta**。它的 body 隨即變成
            //   沒人認領的，由同一輪的 `reap_orphans` 收走 —— 兩步都在這一趟裡做完。
            _ => {
                let _ = fs::remove_file(&p);
            }
        }
    }
    out
}

/// 收掉這個葉子目錄裡**沒人認領**的 body 檔案。
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

/// 讀回淘汰索引。
///
/// ★ 沒有 / 壞了都當空 —— 後台漸進重建會把它補齊，
/// 而在補齊之前**讀路徑照常命中**（它不看索引）。
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

    /// 一個用完就刪的暫存目錄。
    ///
    /// ⚠ ⚠ **有意不按 pid 派生名字**：pid 會被重複使用，而一個「按 pid 命名且從不清理」
    /// 的測試目錄，會讓今天這一跑打開**上一次**留下的狀態 —— 那是一條
    /// 時紅時綠的判據，而它教人「紅了先重跑一次」。
    /// ⇒ 名字裡帶一個處理程序內自增號 + 納秒時間戳，並且**用 `create_dir` 而不是
    /// `create_dir_all`**：撞上了就當場失敗，而不是安靜地重複使用。
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

    // ── D19：磁碟後端的容量同樣改得動 ──────────────────────────────────────
    //
    // ⚠ ⚠ **它必須與 `MemStore` 那兩條配成對**：兩個後端對 `capacity` 的三個讀點
    //   （getter · 大條目守衛 · 淘汰循環）形狀**逐字相同**，⇒ 只測一個的話，
    //   另一個漏改了照樣全綠，而症狀是「換了磁碟後端就不生效」這種最難查的形狀。
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

    // ★ ★ ★ **本模組存在的理由，一條判據說完**：換一個處理程序（這裡換一個 `DiskStore`
    //   執行個體，且**沒有**任何處理程序內狀態傳過去）之後，東西還在。
    //   ⚠ 一個記憶體後端在這條判據上必然紅 —— 也就是說它分得清「記憶體」與「磁碟」，
    //   而 `X-Fulcrum-Cache` 那個頭分不清（那個頭信的是設定說了什麼）。
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

    // ★ ★ G84 的正面判據：**新執行個體的索引是空的**（啟動不掃盤），
    //   ⚠ 而**讀路徑照樣命中** —— 上面那條已經證了後半句，這條證前半句。
    //   合起來才說明「不掃盤」不是靠犧牲命中換來的。
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
        // ⚠ 把索引檔案刪掉，模擬「上一次沒來得及 save」。
        let _ = fs::remove_file(d.0.join("index.json"));
        let s2 = open(&d, 1 << 20);
        assert_eq!(s2.stats(), (0, 0), "启动时不该扫盘（索引应当是空的）");
        assert!(
            matches!(s2.get("k3", none), Lookup::Hit(_)),
            "索引空归空，读路径必须照样命中 —— 否则一次重启等于一次全量回源"
        );
    }

    // ★ 後台漸進重建把索引補齊（G84 的第二半）。
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

    // ★ ★ ★ 沒人認領的 body 必須被收掉。
    //   ⚠ 它不在索引裡 ⇒ 佔用算不到 ⇒ 淘汰永遠輪不到它：一處**只漲不落**的磁碟佔用，
    //   而快取本身的行為完全正常。**除了這一步，沒有任何東西會刪它。**
    #[test]
    fn 没人认领的_body_会被收掉() {
        let d = TempDir::new("orphan");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"real", vec![]));
        // 手動造一個孤兒：與真條目同一個葉子目錄，但沒有任何 meta 提到它。
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

    // ★ ★ 讀時檢核（G84）：body 與 meta 對不上 ⇒ 即丟即刪，當成未命中。
    //   ⚠ 不檢核的話，一份被截斷的 body 會被**原樣發給用戶端** —— 長度對不上、
    //   狀態碼 200，而用戶端拿到半個頁面。
    #[test]
    fn body_与_meta_对不上就即丢即删() {
        let d = TempDir::new("validate");
        let s = open(&d, 1 << 20);
        s.put("k", String::new(), entry(b"0123456789", vec![]));
        let bp = s.body_path("k", "");
        fs::write(&bp, b"012").unwrap(); // 截斷
        assert!(matches!(s.get("k", none), Lookup::Miss), "坏条目该当未命中");
        assert!(!bp.exists(), "坏条目该被删掉");
        // 再查一次仍然是 Miss，而且不該 panic。
        assert!(matches!(s.get("k", none), Lookup::Miss));
    }

    // ★ body 整個不見了（meta 還在）—— 崩潰恢復裡另一半的形態。
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

    // ★ ★ Vary：兩個分支共存、互不擠掉，而且各拿各的那一份。
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
        // 一個沒寫過的 flavor ⇒ VaryMiss（不是 Miss）——兩種未命中要分得開。
        match s.get("k", |_| Some("c")) {
            Lookup::VaryMiss { vary: v } => assert_eq!(v, vary),
            other => panic!("该是 VaryMiss，实际 {other:?}"),
        }
    }

    // ★ ★ refresh 只改 meta、**不動 body**（G83 那條形狀的判據）。
    //   ⚠ 它同時釘住「body 檔案的修改時間沒變」—— 只比內容的話，
    //   一個「整條重寫」的實作照樣能給綠。
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

    // ★ ★ ★ **purge 必須以盤為準，不是以索引為準。**
    //   ⚠ 一個只看索引的實作，在**剛重新啟動之後**（索引空、盤上東西都在）
    //   會清不掉任何東西，還回一句「清掉 0 條」——
    //   而那正是有人會去按 purge 的那個時刻。
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

    // ★ LRU：裝滿之後淘汰**最久沒用**的那條，而且盤上的檔案真的少了。
    #[test]
    fn 淘汰的是最久没用的而且文件真的被删了() {
        let d = TempDir::new("lru");
        // 每條 ≈ 10（體）+ 頭開銷；給一個剛好放得下三條的容量。
        let one = entry(&[0u8; 10], vec![]).footprint();
        let s = open(&d, one * 3);
        for k in ["a", "b", "c"] {
            s.put(k, String::new(), entry(&[0u8; 10], vec![]));
        }
        assert_eq!(s.stats().1, 3);
        assert!(matches!(s.get("a", none), Lookup::Hit(_))); // a 變成最近用過的
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

    // ★ ★ 兩級分片：目錄形狀是 G83 定死的，釘住它。
    //   ⚠ 單層目錄在幾十萬檔案時會把清理任務本身拖垮 —— 那件事在門禁裡
    //   永遠不會發生（門禁只存幾條），所以**只能**在這裡釘。
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

    // ★ hash 撞了也不許給錯內容 —— 正確性不建在「不會撞」上面。
    //   ⚠ 這裡直接把一份「別人的」meta 寫到我們這個鍵的檔案名上，
    //   模擬一次撞車：結果必須是未命中，絕不能把那份內容發出去。
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

    // ★ meta 版本對不上 ⇒ 當沒有（換代之後冷啟動一次，而不是按新語義讀舊資料）。
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

    // ★ 壞掉的 meta 會被後台清理收走，連同它的 body。
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

    // ⚠ 目錄不可用時 `open` 必須**失敗**，而不是回一個什麼都不做的執行個體。
    //   ★ 這條是「靜默失能」的正面判據：呼叫方要有機會說出這件事。
    #[test]
    fn 目录不可用时_open_失败() {
        let d = TempDir::new("badroot");
        // 用一個**檔案**當快取根：create_dir_all 會失敗。
        let f = d.0.join("iam-a-file");
        fs::write(&f, b"x").unwrap();
        assert!(DiskStore::open(&f, 1 << 20).is_err());
    }
}
