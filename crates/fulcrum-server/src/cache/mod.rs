//! 自研 HTTP 快取（**M2 批 G**，G82–G84 + G95–G98）。
//!
//! ## 這一批做了什麼
//!
//! **語義層**（RFC 9111：可快取性 · 新鮮度 · 快取鍵 · `Vary` · 條件重驗證 ·
//! 上游回應頭裡的**全套** `Cache-Control` 指令，G97）+ **記憶體後端** +
//! **防驚群** + 管理面 `POST /purge`。
//!
//! **磁碟後端是批 H**（G95 的切法）：形狀已由 G83/G84 定死 ——
//! 兩級分片目錄、meta 與 body 兩檔案、`tmp` 後 `rename`、啟動不掃盤。
//!
//! ## ★ ★ ★ 判據為什麼寫成這個樣子
//!
//! G82 拍板時把代價寫在了明處：
//! > RFC 9111 語義、`Vary`、防驚群、中繼資料序列化 ≈4000 行全自己寫，
//! > **而快取的錯表現為「偶爾給錯內容」** —— 不像轉發的錯那樣當場可見。
//!
//! ⇒ 每一條規則的判據都要說清**「寫錯了會怎樣」**，而不只是「這樣寫是對的」。
//! 本模組四個子模組裡每一條 `#[test]` 都照這個寫。
//!
//! ## 我們是**共享**快取
//!
//! 這一句改變好幾處判斷，逐條寫在 [`policy`] 頂部 —— 其中最貴的一條是
//! **帶 `Authorization` 的請求，其回應預設不可快取**（RFC 9111 §3.5）：
//! 漏了它就是把一個人的私有頁面發給下一個人，**而不會有任何回報錯誤**。

pub mod cc;
pub mod coalesce;
pub mod disk;
pub mod key;
pub mod policy;
pub mod store;

use log::{error, info, warn};
use std::path::Path;
use std::sync::Arc;

/// 解析一個 HTTP 日期成 unix 秒。
///
/// ★ ★ **直接用靜態檔案那一批寫的解析器**（[`crate::files::httpdate`]，G93）——
/// 它已經把三種格式與 RFC 850 兩位年那條規則都做了，還有 10 條單測釘著。
/// ⚠ 在這裡再寫一份的代價很具體：`Expires` 與 `Last-Modified` 是**同一種**日期，
/// 兩份實作遲早在閏年或兩位年上分家，而現場表現是「某些回應的快取壽命算錯了」。
/// ★ 這與批 C 重複使用 `wildcard_covers` 是同一條紀律：**讓分家在結構上做不到**。
pub fn files_httpdate(s: &str) -> Option<i64> {
    crate::files::httpdate::parse(s)
}

/// 這份設定下**整個處理程序**的快取容量上限。
///
/// ## ★ ★ ★ 為什麼是一個函式而不是兩處各寫一遍
///
/// 它有**兩個**呼叫點：啟動時 `serve()` 建後端，以及 `POST /load` 換設定之後
/// （D19）。⚠ 兩處各寫一遍的失效形態不是回報錯誤 —— 是有一天一處改了另一處沒改，
/// 於是**啟動時算出來的容量與 load 之後算出來的不是同一個數**，而兩處各自都自洽。
/// ★ 本儲存庫 2026-09-05 當天剛栽過一次同形狀的（一個分解式抄了三處，中間項全錯而
/// 和恰好還對）⇒ 這一次從一開始就只留一處。
///
/// ## ⚠ 多個塊寫不同容量時取**最大**
///
/// 取最小會讓寫了大容量的站台悄悄拿不到空間，而那件事沒有任何東西會說。
/// ★ `disk` 走另一條路 —— 多個 `cache` 寫不同目錄是**編譯期錯誤**（`FUL-DSL-0035`）。
/// 兩者處置不同是因為代價不同：容量取大一點不傷誰，而目錄取一個會讓另一個站台的
/// 快取整個落在別處。
///
/// ⚠ 一個 `cache` 塊都沒有時回傳**預設容量**而不是 0：回傳 0 的話快取會變成
/// 「什麼都存不下」，而那不會回報錯誤，只會靜靜地不再命中。
pub fn effective_capacity(settings: &[(&str, &fulcrum_runtime::CacheRt)]) -> u64 {
    settings
        .iter()
        .map(|(_, c)| c.capacity_bytes)
        .max()
        .unwrap_or(fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES)
}

/// 後端：記憶體（批 G）或磁碟（批 H），**全處理程序只有一個**。
///
/// ## ★ ★ ★ 為什麼是一個 enum，而不是 `Box<dyn Store>`
///
/// 磁碟層要實作的就是 `MemStore` 那一組介面，而其中 [`store::MemStore::get`] 帶一個泛型閉包 ——
/// ⚠ **它不是物件安全的**，`dyn` 要麼把閉包裝箱、要麼把簽名改掉。
/// ★ enum 換來兩樣東西：呼叫點不變（還是那七個同名方法），
/// 而且**再加一個後端時，漏掉哪一個方法是編譯錯誤**。
///
/// ⚠ ⚠ 第三檔 [`Backend::Off`] 不是湊數的，理由見 [`Backend::open`]。
#[derive(Debug)]
pub enum Backend {
    Mem(store::MemStore),
    Disk(disk::DiskStore),
    /// 配了 `disk` 而那個目錄用不了 —— **不快取，但照常轉發**。
    Off,
}

impl Backend {
    /// 按設定挑一個後端。
    ///
    /// ## ★ ★ ★ 目錄用不了的時候，三種做法裡為什麼取這一種
    ///
    /// | 做法 | 失敗形態 |
    /// |---|---|
    /// | 退回記憶體 | ⚠ ⚠ 使用者按**磁碟**寫的 `capacity`（比如 50GB）會變成記憶體預算 ⇒ **OOM** |
    /// | `exit(1)` | ⚠ ⚠ 換代時新一代拒絕啟動 = **服務整體中斷**，而 `validate` 是綠的 |
    /// | **關掉快取，照常轉發** | 變慢，僅此而已 |
    ///
    /// ★ 而「關掉」必須**說得出來**，否則它就是本儲存庫反覆點名的那種靜默失能：
    /// 裝載紀錄有一行 `error`，執行環境 `X-Fulcrum-Cache` 那個頭**不再出現** ——
    /// 兩個獨立的信號，一個給啟動那一刻，一個給之後的任何時刻。
    pub fn open(disk_dir: Option<&str>, capacity: u64) -> Backend {
        let Some(dir) = disk_dir else {
            return Backend::Mem(store::MemStore::new(capacity));
        };
        match disk::DiskStore::open(Path::new(dir), capacity) {
            Ok(s) => Backend::Disk(s),
            Err(e) => {
                error!(
                    "★ 缓存磁盘目录 `{dir}` 用不了（{e}）—— 本进程不再缓存任何东西，\
                     但请求照常转发。修好目录（存在、可写、属主对）之后重启生效"
                );
                Backend::Off
            }
        }
    }

    /// 這個後端此刻的容量上限。`None` = 後端關著（沒有容量可言）。
    ///
    /// ★ 回傳 `Option` 而不是拿 0 頂替：0 與「關掉了」是兩件事，而把它們合成一個數
    /// 之後，管理面回話會打出「生效容量 0B」—— 一句讀起來完全成立、卻指向
    /// 完全錯誤方向的話。
    pub fn capacity(&self) -> Option<u64> {
        match self {
            Backend::Mem(s) => Some(s.capacity()),
            Backend::Disk(s) => Some(s.capacity()),
            Backend::Off => None,
        }
    }

    /// 換一個容量上限（`POST /load` 走這裡，D19）。
    ///
    /// ⚠ `Off` 那一檔是**空操作**，⛔ 不是 panic：一次 `POST /load` 把管理面打死，
    /// 比容量沒改過去壞得多。★ 它也不需要回報錯誤 —— 後端關著這件事，
    /// 裝載紀錄已經用一行 `error` 說過了，這裡再說一遍只會稀釋那句話。
    pub fn set_capacity(&self, capacity: u64) {
        match self {
            Backend::Mem(s) => s.set_capacity(capacity),
            Backend::Disk(s) => s.set_capacity(capacity),
            Backend::Off => {}
        }
    }

    /// 這個後端叫什麼（裝載紀錄與 `X-Fulcrum-Cache` 都取它）。
    ///
    /// ★ ★ 它說的是**真的挑中了哪一個**，不是設定裡寫了什麼 ——
    /// 那正是上面那張表裡「關掉」這一檔能被看見的原因。
    pub fn label(&self) -> &'static str {
        match self {
            Backend::Mem(_) => "memory",
            Backend::Disk(_) => "disk",
            Backend::Off => "off",
        }
    }

    /// 給 `X-Fulcrum-Cache` 用的狀態值：磁碟後端帶一個後綴。
    ///
    /// ⚠ **判據要能分辨「從記憶體來的」與「從磁碟來的」**，而那件事靠狀態碼看不出來。
    /// ★ 但這個頭只是**方便**的那一半：它信的是「本處理程序挑中了哪個後端」。
    /// 真正證明「東西在盤上」的判據是**把處理程序殺掉再起來，它還在** ——
    /// 那一條記憶體後端**不可能**通過。兩條一起用。
    ///
    /// ⚠ 收 [`CacheState`] 而不是 `&str`，理由寫在那個枚舉上：**這一串與
    /// `fulcrum_cache_events_total{event}` 那一格是同一件事的兩種粒度**，
    /// 而兩處都從同一個枚舉窮盡出來時，「加了一種狀態卻漏了一處」編不過。
    pub fn state(&self, base: CacheState) -> String {
        match self {
            Backend::Disk(_) => format!("{}-DISK", base.header_base()),
            _ => base.header_base().to_string(),
        }
    }

    // ── 七個操作。★ 與 `MemStore` 逐字同簽名，逐條轉發。────────────────────

    pub fn get<'a, F>(&self, primary: &str, get_header: F) -> store::Lookup
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        match self {
            Backend::Mem(s) => s.get(primary, get_header),
            Backend::Disk(s) => s.get(primary, get_header),
            Backend::Off => store::Lookup::Miss,
        }
    }

    pub fn put(&self, primary: &str, secondary: String, entry: store::Entry) {
        match self {
            Backend::Mem(s) => s.put(primary, secondary, entry),
            Backend::Disk(s) => s.put(primary, secondary, entry),
            Backend::Off => {}
        }
    }

    pub fn refresh(
        &self,
        primary: &str,
        secondary: &str,
        fresh_for: u64,
        now: i64,
        cc: cc::ResponseCc,
    ) {
        match self {
            Backend::Mem(s) => s.refresh(primary, secondary, fresh_for, now, cc),
            Backend::Disk(s) => s.refresh(primary, secondary, fresh_for, now, cc),
            Backend::Off => {}
        }
    }

    pub fn purge_primary(&self, primary: &str) -> usize {
        match self {
            Backend::Mem(s) => s.purge_primary(primary),
            Backend::Disk(s) => s.purge_primary(primary),
            Backend::Off => 0,
        }
    }

    pub fn purge_prefix(&self, prefix: &str) -> usize {
        match self {
            Backend::Mem(s) => s.purge_prefix(prefix),
            Backend::Disk(s) => s.purge_prefix(prefix),
            Backend::Off => 0,
        }
    }

    pub fn purge_all(&self) -> usize {
        match self {
            Backend::Mem(s) => s.purge_all(),
            Backend::Disk(s) => s.purge_all(),
            Backend::Off => 0,
        }
    }

    pub fn stats(&self) -> (u64, usize) {
        match self {
            Backend::Mem(s) => s.stats(),
            Backend::Disk(s) => s.stats(),
            Backend::Off => (0, 0),
        }
    }
}

/// 「這一條是從快取發出去的」有哪幾種 —— `X-Fulcrum-Cache` 那個頭的**基值**
/// （不含後端後綴）。
///
/// # ★ ★ ★ 為什麼是枚舉而不是一個 `&str`
///
/// 同一件事要在兩個粒度上說出來：回應頭要分辨記憶體與磁碟（`HIT` / `HIT-DISK`），
/// 而 `fulcrum_cache_events_total{event}` 那張表把上界寫死成一個閉集 ⇒ 兩者折成一格。
/// ⚠ ⚠ 若把狀態當字串傳，那個摺疊就只能寫成 `match s { … , _ => … }` ——
/// 而 `&str` 的 `match` **必須**有備援臂，於是**將來新增的一種狀態會靜默地被算成別的**：
/// 指標數字照漲，只是漲錯了格，沒有任何東西會說出來（R7 明令禁止的形狀）。
/// ★ 換成枚舉之後，[`Backend::state`] 與 [`CacheState::event`] 兩處一起窮盡 ——
/// 加一個變體時**兩處都編不過**，那正是我們要的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// 新鮮，直接從快取發出去。
    Hit,
    /// 陳了，帶檢核器問過上游拿到 304 之後，把快取裡那份發出去。
    Revalidated,
}

impl CacheState {
    /// `X-Fulcrum-Cache` 裡那個基值。⚠ 這兩個字串是**對外契約**
    /// （判據與使用者的告警規則都按它們寫），改一個字就是改契約。
    fn header_base(self) -> &'static str {
        match self {
            CacheState::Hit => "HIT",
            CacheState::Revalidated => "REVALIDATED",
        }
    }

    /// 折成 `fulcrum_cache_events_total{event}` 的那一格（R7）。
    ///
    /// ⚠ 折得比回應頭**粗**：`HIT` 與 `HIT-DISK` 在指標上是同一格。
    /// ★ 想知道「是哪一種命中」去看訪問紀錄 —— 它記的是 `cache` 的原值。
    pub fn event(self) -> CacheEvent {
        match self {
            CacheState::Hit => CacheEvent::Hit,
            CacheState::Revalidated => CacheEvent::Stale,
        }
    }
}

/// `fulcrum_cache_events_total{event}` 的取值 —— **閉集**（R7）。
///
/// # ★ ★ 三個值**同一個分母**（G123，D31 結案）
///
/// `Hit` / `Miss` / `Stale` 的單位都是「**一條請求**」
/// ⇒ `sum(fulcrum_cache_events_total)` 就是「查過快取**並且**已經定下這條回應的內容
/// 從哪來的請求數」，命中率直接可寫。權威是
/// [`docs/architecture/observability.md`](../../../../docs/architecture/observability.md)
/// 與 `PLAN.md` §10 **G123**。
///
/// ⚠ ⚠ **「被清掉的條目數」不在這裡** —— 它數的是**條目**不是請求，
/// 在 [`record_purged_entries`] 那一族。
/// ★ ★ 那次拆分**把一個靠使用者記住的陷阱換成了一個結構上不存在的陷阱**：
/// 兩個不同的分母不再共用一個族，於是「求和求出個沒意義的數」這件事**做不到了**。
/// ⛔ **不要把它搬回來**，哪怕只是「順手多一個 `event` 值」——
/// 那一句「單位都是『條』」對請求與條目都成立，於是整段讀起來自洽，
/// **而它斷言的那個等價關係是假的**。
///
/// ⚠ 只有 `Hit` 與 `Stale` 在 `X-Fulcrum-Cache` 裡露過面（見 [`CacheState`]）：
/// **回源那條路沒有這個頭**（契約裡寫死的，所以訪問紀錄裡也沒有 `cache` 那一格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEvent {
    /// 從快取發出去（含磁碟後端）。
    Hit,
    /// 回源：這條回應的內容來自上游。
    Miss,
    /// 重驗證之後把快取裡那份發出去。
    Stale,
}

/// `fulcrum_cache_events_total{event}` 的**取值閉集**，`metrics.rs` 的基數表判據按它算上界。
///
/// ⚠ ⚠ ⛔ **它不是一份手抄**：單測 `event 闭集与 CacheEvent::label 对得上` 把
/// [`CacheEvent::ALL`] 逐個映射一遍，與本常數**互為子集**；而 `ALL` 自己由一條**窮盡
/// match** 守著 ⇒ 加一個變體就編不過，⛔ 不是靜默多出一個沒人認得的標籤值。
pub const CACHE_EVENTS: &[&str] = &["hit", "miss", "stale"];

impl CacheEvent {
    /// 全部變體。⚠ 它的完整性由 [`CacheEvent::穷尽性自证`] 那條**窮盡 match** 守著。
    pub const ALL: [CacheEvent; 3] = [CacheEvent::Hit, CacheEvent::Miss, CacheEvent::Stale];

    /// ⚠ ⚠ **這個函式存在的唯一理由是讓編譯器守住 [`CacheEvent::ALL`] 的完整性。**
    /// 加一個變體 ⇒ 這條 match 編不過 ⇒ 人被逼著回來同時改 `ALL`。
    /// ⛔ 別把它改成帶 `_ =>` 備援臂的寫法，那樣它就什麼都不守了。
    #[cfg(test)]
    pub(crate) fn 穷尽性自证(self) -> Self {
        match self {
            CacheEvent::Hit => self,
            CacheEvent::Miss => self,
            CacheEvent::Stale => self,
        }
    }

    /// 標籤值。⚠ 與 [`CacheState::header_base`] 不同，這幾個是**小寫**的 ——
    /// Prometheus 標籤值的習慣，而且它們與回應頭本來就不是同一套詞。
    pub fn label(self) -> &'static str {
        match self {
            CacheEvent::Hit => "hit",
            CacheEvent::Miss => "miss",
            CacheEvent::Stale => "stale",
        }
    }

    /// 記 `n` 次。
    ///
    /// ⚠ 今天所有呼叫方傳的都是 `1`（三個值數的都是「一條請求」）——
    /// ★ 保留 `n` 是因為簽名與 [`crate::metrics::Family::inc_by`] 同形，
    /// ⛔ **不是**留給「成批的事件」用：真成批的那一個已經搬走了。
    pub fn record(self, n: u64) {
        crate::metrics::CACHE_EVENTS_TOTAL.inc_by(&[self.label()], n);
    }
}

/// 記下 `POST /purge` 清掉了 `n` 條**快取條目**（G123）。
///
/// ⚠ ⚠ **它有意不是 [`CacheEvent`] 的一個變體**：那個族數的是請求，這裡數的是條目，
/// 兩個分母混進一個族的代價見 [`CacheEvent`] 的類型文件。
/// ★ 取數點**只有一處**（`admin.rs` 的 `purge` 收尾），與本儲存庫其餘族同一條紀律。
/// ⚠ 清掉 0 條時**照樣調**：那讓這條 series 存在，而「從來沒 purge 過」與
/// 「purge 過、只是沒清到東西」在抓取端要分得開。
pub fn record_purged_entries(n: u64) {
    crate::metrics::CACHE_PURGED_ENTRIES_TOTAL.inc_by(&[], n);
}

/// 一個站台上生效的快取執行個體。
///
/// ⚠ ⚠ **快取是處理程序級的，不是每次裝載新建的**：G8 的全量原子 load 會把整棵
/// 執行環境圖換掉，而**快取內容不該跟著沒**（換一次設定就清空快取，等於每次
/// reload 都讓上游挨一次滿負荷）。⇒ 它掛在 `FulcrumApp` 上，跨 load 存活。
#[derive(Debug)]
pub struct CacheHandle {
    pub store: Backend,
    pub coalescer: coalesce::Coalescer,
}

impl CacheHandle {
    pub fn new(store: Backend) -> Arc<CacheHandle> {
        Arc::new(CacheHandle {
            store,
            coalescer: coalesce::Coalescer::new(),
        })
    }

    /// 拿到磁碟後端（不是磁碟後端就 `None`）。
    ///
    /// ★ 後台維護任務用它決定起不起 —— **它是磁碟後端才有的事**。
    /// ⚠ 無條件起一個每秒醒一次什麼都不做的任務，會讓「有沒有磁碟快取」
    /// 在 `top` 裡看不出區別，也會讓紀錄裡那句「後台維護已啟動」變成一句空話。
    pub fn disk(&self) -> Option<&disk::DiskStore> {
        match &self.store {
            Backend::Disk(s) => Some(s),
            _ => None,
        }
    }
}

/// 磁碟快取的後台維護（**G84**：漸進重建索引 + 收孤兒 + save 索引）。
///
/// ★ ★ 它掛成 Pingora 的 `BackgroundService`，與 DNS 重解析、健康檢查、ACME 同款 ——
/// **不是**一個裸的 `tokio::spawn`。⚠ 差別很具體：`BackgroundService` 拿得到
/// `ShutdownWatch`，於是它能在停機時把索引 **save** 下去（G84 那一半的自然時刻），
/// 而一個裸 spawn 的任務在換代時是被**直接丟掉**的。
pub struct CacheMaintenanceService {
    handle: Arc<CacheHandle>,
    tick: std::time::Duration,
}

impl CacheMaintenanceService {
    /// 一個一級分片目錄一跳（最多 256 個）⇒ 一整輪約 4 分鐘。
    ///
    /// ⚠ 有意**不是**「一跳走完全盤」：那只是把啟動掃盤挪了個時刻做，
    /// 而 G84 拒絕的是那件事的**代價**（一次隨快取大小線性增長的長停頓），
    /// 不只是它發生在哪一刻。
    pub const TICK: std::time::Duration = std::time::Duration::from_secs(1);

    pub fn new(handle: Arc<CacheHandle>) -> CacheMaintenanceService {
        CacheMaintenanceService {
            handle,
            tick: Self::TICK,
        }
    }
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for CacheMaintenanceService {
    async fn start(&self, mut shutdown: pingora_core::server::ShutdownWatch) {
        let Some(_) = self.handle.disk() else {
            return;
        };
        info!(
            "缓存：磁盘索引后台渐进重建已启动（每 {}s 一个一级分片目录；G84：启动不扫盘）",
            self.tick.as_secs()
        );
        let mut passes = 0u64;
        let mut since_save = 0u64;
        loop {
            if *shutdown.borrow() {
                break;
            }
            // ★ `select!` 而不是先 sleep 再看信號（與 DNS 重解析同一條紀律）：
            //   一個 sleep 會把優雅停機拖長它那麼久。
            tokio::select! {
                _ = tokio::time::sleep(self.tick) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
            }
            let h = Arc::clone(&self.handle);
            // ⚠ ⚠ **走盤是阻塞 I/O**，必須扔進 blocking 池 —— 與 `getaddrinfo`
            //   那條一模一樣的理由：直接在 runtime 執行緒上遍歷一個大目錄，
            //   會把排在同一條執行緒上的**請求**一起拖住。
            let done = match tokio::task::spawn_blocking(move || {
                h.disk().map(|d| d.rebuild_step()).unwrap_or(true)
            })
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    warn!("缓存：后台重建那一跳没跑成：{e}");
                    continue;
                }
            };
            since_save += 1;
            if done {
                passes += 1;
                // ★ 只在**第一輪**走完時說一句：之後每 4 分鐘一行就是噪音，
                //   而第一輪走完這件事有意義 —— 它是「索引從此是全的」那一刻。
                if passes == 1 {
                    let (used, n) = self.handle.store.stats();
                    info!("缓存：磁盘索引首轮渐进重建完成 —— {n} 条 / {used}B");
                }
            }
            // ★ 定期 save：崩潰（不是優雅停機）時最多丟一分鐘的索引更新，
            //   而丟了的後果只是下次啟動佔用算得偏小，由漸進重建補回來。
            if since_save >= 60 {
                since_save = 0;
                let h = Arc::clone(&self.handle);
                let _ = tokio::task::spawn_blocking(move || {
                    if let Some(d) = h.disk() {
                        d.save_index();
                    }
                })
                .await;
            }
        }
        // ★ ★ **停機時把索引存下去**（G84 的 save 那一半）。
        //   ⚠ 這一步是這個 service 相對於裸 `tokio::spawn` 唯一真正多出來的能力，
        //   而它決定了下一代啟動時佔用算得準不準。
        if let Some(d) = self.handle.disk() {
            d.save_index();
            let (used, n) = self.handle.store.stats();
            info!("缓存：收到停机信号，淘汰索引已存盘（{n} 条 / {used}B）");
        }
    }
}

// ★ 快取的裝載摘要住在 `lib.rs` 的 `log_load_summary` 裡（與 hide 清單、UNWIRED 公告同一處）：
//   裝載結論只有一個出口，而不是兩個各說各的。

#[cfg(test)]
mod tests {
    //! **M2 批 M**（裁決 R7）：`X-Fulcrum-Cache` 的**完整取值集合** ↔ 四個快取事件。

    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "fulcrum-cache-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&p).expect("临时目录撞名了 —— 那说明命名不够唯一");
        p
    }

    fn rt_cache(capacity_bytes: u64) -> fulcrum_runtime::CacheRt {
        fulcrum_runtime::CacheRt {
            ttl_ms: None,
            max_size_bytes: 1024,
            capacity_bytes,
            disk_dir: None,
        }
    }

    // ── D19：「各塊取最大值」這條推導只許存在一處 ──────────────────────────
    //
    // ★ ★ ★ 它原本內聯在 `serve()` 裡。`POST /load` 現在也要算同一個值，
    //   ⇒ 在那邊再寫一遍就是**一個值抄在兩處**：上游一改兩處一起過期而彼此自洽，
    //   本儲存庫 2026-09-05 當天剛栽過一次同形狀的（噪音賬那個分解式抄了三處）。
    //   ⇒ 提成 [`effective_capacity`]，兩處都調它，本條釘住它。
    #[test]
    fn 生效容量取各块最大值_没有块时取默认() {
        let a = rt_cache(1_000);
        let b = rt_cache(9_000);
        let c = rt_cache(5_000);
        assert_eq!(
            effective_capacity(&[("s1", &a), ("s2", &b), ("s3", &c)]),
            9_000,
            "多个块写不同容量时取**最大** —— 取最小会让写了大容量的站点悄悄拿不到空间"
        );
        // ⚠ 承重的另一半：一個塊都沒有時**不是 0**，是預設值 ——
        //   回傳 0 的話快取會變成「什麼都存不下」，而那不會回報錯誤，只會靜靜地不再命中。
        assert_eq!(
            effective_capacity(&[]),
            fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES,
            "没有 cache 块时该取默认容量"
        );
    }

    // ⚠ `Backend::Off` 那一檔（配了 `disk` 而目錄用不了）也要吃得下 `set_capacity`：
    //   它沒有容量可言，但**不許 panic** —— 一次 `POST /load` 打死管理面，
    //   比容量沒改過去壞得多。
    #[test]
    fn 关掉的后端也吃得下改容量_且不_panic() {
        let off = Backend::Off;
        off.set_capacity(4096);
        let mem = Backend::open(None, 1024);
        mem.set_capacity(4096);
        assert_eq!(mem.capacity(), Some(4096));
        assert_eq!(off.capacity(), None, "关掉的后端没有容量可言");
    }

    #[test]
    fn 状态值的完整取值集合_以及它们各自折到哪个事件() {
        // ★ ★ 一次列全：兩個狀態 × 三種後端 = `X-Fulcrum-Cache` 能出現的**全部**字串。
        //   ⚠ 只列「我見過的那幾個」不算數 —— R7 要求的是**取值集合**，
        //     而集合是由這兩個枚舉與後端那三檔共同定死的。
        let dir = tmp_dir("state");
        let mem = Backend::Mem(store::MemStore::new(1 << 20));
        let off = Backend::Off;
        let disk = Backend::Disk(disk::DiskStore::open(&dir, 1 << 20).expect("打不开磁盘缓存"));

        assert_eq!(mem.state(CacheState::Hit), "HIT");
        assert_eq!(off.state(CacheState::Hit), "HIT");
        assert_eq!(disk.state(CacheState::Hit), "HIT-DISK");
        assert_eq!(mem.state(CacheState::Revalidated), "REVALIDATED");
        assert_eq!(off.state(CacheState::Revalidated), "REVALIDATED");
        assert_eq!(disk.state(CacheState::Revalidated), "REVALIDATED-DISK");

        // ★ ★ 摺疊的兩個方向都要判：
        //   ① 後端後綴**不改變**事件那一格（`HIT` 與 `HIT-DISK` 同一個 `hit`）；
        //   ② 狀態**改變**它。
        //   ⚠ 少了 ②，一個「全折成 `hit`」的實作照樣能讓 ① 全綠。
        assert_eq!(CacheState::Hit.event().label(), "hit");
        assert_eq!(CacheState::Revalidated.event().label(), "stale");
        assert_ne!(
            CacheState::Hit.event(),
            CacheState::Revalidated.event(),
            "命中与重验证被折成了同一格 —— 那两件事在指标上就再也分不开了"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 事件的标签值就是那张表写死的那几个_而且互不相同() {
        // ⚠ 上界就是這幾個：加一個變體，`label` 的 `match` 當場編不過。
        //   ★ 這條判據釘住的是**標籤值本身** —— 改一個字就是改抓取端的查詢語句。
        assert_eq!(CacheEvent::Hit.label(), "hit");
        assert_eq!(CacheEvent::Miss.label(), "miss");
        assert_eq!(CacheEvent::Stale.label(), "stale");

        // ★ ★ 反向那半：標籤值**互不相同**。
        //   ⚠ 一次複製粘貼就能讓兩個變體共用一個詞，而上面幾條只要有一條跟著改
        //     就照樣全綠 —— 現場是兩件事被加進同一格，數字看起來完全正常。
        let mut labels: Vec<&str> = [CacheEvent::Hit, CacheEvent::Miss, CacheEvent::Stale]
            .iter()
            .map(|e| e.label())
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels, ["hit", "miss", "stale"]);

        // ★ ★ ★ **`purge` 不許回到這個族裡**（G123）。
        //   ⚠ 它不是「少寫了一個變體」——兩個不同的分母共用一個族，會讓
        //     `sum(cache_events_total)` 變成請求數與條目數相加的一個數，
        //     而那個數長得完全正常。⇒ 這條判據是那次拆分的**唯一**守衛。
        assert!(
            !labels.contains(&"purge"),
            "`purge` 回到了 fulcrum_cache_events_total 里 —— \
             它数的是**条目**，另外几个数的是**请求**，两个分母不许共用一个族（G123）"
        );
    }

    #[test]
    fn 每个事件都进得了那张进程级的表_而被清掉的条目数在自己那一族里() {
        // ★ 只判「這一格存在」，不判讀數：別的判據也在往同一張表裡寫，
        //   而**存在**是單調的 ⇒ 這條判據不會因為測試跑的順序而紅。
        // ⚠ `record(0)` 照樣把這條 series 建出來 —— counter 在 Prometheus 裡
        //   本來就該儘早存在（「族在、值是 0」與「族根本沒出現過」是兩件事）。
        let all = [CacheEvent::Hit, CacheEvent::Miss, CacheEvent::Stale];
        for e in all {
            e.record(0);
        }
        record_purged_entries(0);
        let out = crate::metrics::render();
        for e in all {
            assert!(
                out.contains(&format!(
                    "fulcrum_cache_events_total{{event=\"{}\"}} ",
                    e.label()
                )),
                "缺 {} 这一格：\n{out}",
                e.label()
            );
        }
        // ★ ★ 新族**無標籤** ⇒ 渲出來是不帶花括號的單條 series（G123）。
        assert!(
            out.contains("fulcrum_cache_purged_entries_total "),
            "被清掉的条目数那一族没渲出来：\n{out}"
        );
        // ★ ★ ★ 反向那半：**它不許再以 `event` 標籤的形式出現**。
        //   ⚠ 少了這一條，一個「兩邊都記一筆」的實作在上面每一條上都是綠的 ——
        //     而那正是 G123 要消掉的那個形狀。
        assert!(
            !out.contains("fulcrum_cache_events_total{event=\"purge\"}"),
            "`purge` 又出现在 cache_events_total 里了（G123 拆走的就是它）：\n{out}"
        );
    }
}
