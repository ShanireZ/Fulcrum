//! 執行環境的**臨時覆蓋層**（**M2 批 N 任務 3**，G18 / 裁決 R7）。
//!
//! G18 原話：**執行環境改動的歸宿 = 顯式臨時覆蓋層。不持久化，但永遠可見。**
//!
//! # ★ ★ ★ 它是一張「格子登記處」，不是一份「待應用的清單」
//!
//! ```text
//! OverrideLayer:  (站點名, reverse_proxy 的 id, 歸一化後的上游地址) → Arc<UpstreamOverride>
//!                                                                        ▲
//!                 Upstream ─────────────────────────────────────────────┘
//!                 （每個 HTTP 上游**直接拿著**登記處裡的那一格）
//! ```
//!
//! ⇒ 「覆蓋層」與「上游身上那兩個量」在結構上**是同一份**：
//! 不存在「改了列表忘了應用」，也不存在「應用了沒記進列表」。
//!
//! ⛔ **不許改成「一份清單 + 一個應用步驟」**：那個形狀的失效現場是
//! 「設定照過、命令回 200、而那臺機器還在收流量」，且沒有任何東西會回報錯誤。
//!
//! # ⚠ 鍵相同就是同一格，這是設計不是意外
//!
//! 同一個站台裡兩條 `reverse_proxy` 指著同一臺機器而都沒寫 `id` 時，它們的鍵
//! **真的相同** ⇒ 它們從登記處取到**同一個** [`Arc`] ⇒ 一次 `disable` 兩條一起生效
//! （裁決 R6 ③ 第二輪 ⇒ G125）。★ 這個語義**從本模組的設計裡免費掉出來**，
//! ⛔ 別在別處補一條「鍵撞了要怎麼辦」的分支。
//!
//! # ⛔ 不持久化
//!
//! 這裡的東西**只活在處理程序裡**。G18 的死線：⛔ 不許出現第二條持久化寫路徑。
//! 「永遠可見」由 `/stats` 與「每一次管理面回應都帶覆蓋層計數」承擔（任務 5 / 6）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// 覆蓋層的鍵：`(站点名, reverse_proxy 的 id, 归一化后的上游地址)`（裁決 R6 ⇒ G125）。
///
/// 三格分別是 [`crate::SiteRt::name`] · [`crate::ProxyTarget::id`] · [`crate::Upstream::addr`]。
///
/// ⚠ ⚠ **三格都只能從 [`crate::Runtime::keyed_proxies`] 那條遍歷上取**
/// —— ⛔ 別在別處再拼一份「差不多的」，兩份拼法遲早在某一格上分家。
///
/// ⚠ 第三格是**歸一化之後**那個串（`backend` → `backend:80`）：管理面對著的是**執行環境**，
/// 執行環境裡那個上游就叫 `backend:80`。⇒ 維運照著設定原文寫 `backend` 會被告知「找不到」——
///
/// ★ ★ ★ **這是有意的邊界，不是缺陷**（裁決 R6 ⑤，M2 批 N 任務 4 的主 agent 裁決）：
/// 管理面**不做歸一化**，`(站点名, id, 上游地址)` 三格全部逐字精確匹配。
/// ⛔ ⛔ **別在管理面那一側加 `crate::normalize_upstream` 再拿來比鍵** ——
/// 指不到時的解藥不是"幫維運把位址補全"，而是 `POST /runtime` 的 400 訊息
/// **把該站台下全部合法的鍵逐條列出來**（`crates/fulcrum-server/src/admin.rs`
/// 的 `runtime()`），維運照抄即可。做了歸一化，那份"列出全部合法鍵"的清單就
/// 變成永遠走不到的死程式碼——而這與 Ruling 1（管理面不發明規則、只精確匹配）
/// 是同一條紀律：輸入處理越少，鍵就越是唯一的一個東西。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OverrideKey {
    /// 鍵的**第一格**：站台名（第一條位址的原文，與訪問紀錄的 `site` 欄位逐字同口徑）。
    pub site: String,
    /// 鍵的**第二格**：這條 `reverse_proxy` 的 id，**沒寫就是空串**。
    pub id: String,
    /// 鍵的**第三格**：**歸一化之後**的上游位址。
    pub addr: String,
}

impl OverrideKey {
    pub fn new(site: &str, id: &str, addr: &str) -> OverrideKey {
        OverrideKey {
            site: site.to_string(),
            id: id.to_string(),
            addr: addr.to_string(),
        }
    }
}

impl std::fmt::Display for OverrideKey {
    /// 給訊息與紀錄用的可讀形態。⚠ 三格**原樣**印出來，維運照抄即可（裁決 R6 ⑤）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.id.is_empty() {
            write!(f, "站点 {} · 上游 {}", self.site, self.addr)
        } else {
            write!(
                f,
                "站点 {} · id {} · 上游 {}",
                self.site, self.id, self.addr
            )
        }
    }
}

/// 登記處裡的**一格**：一個上游身上「維運改過的那兩個量」。
///
/// ★ ★ 它被 [`Arc`] 共享：登記處裡那一格與 [`crate::Upstream`] 身上那一格
/// **是同一個物件**。⇒ 改這裡，那個上游立刻算數；沒有第二步。
///
/// ⚠ 兩個量都用 `Relaxed`，與 [`crate::Upstream`] 的 `healthy` / `inflight` 同一條理由：
/// 它們是**建議性**的讀數，晚一個排程週期生效不構成任何問題，
/// 而在請求熱路徑上加記憶體序只會白花錢。
#[derive(Debug, Default)]
pub struct UpstreamOverride {
    /// 維運把這個上游摘掉了嗎。
    disabled: AtomicBool,
    /// 覆蓋權重。★ ★ **`0` 是哨兵，意思是「沒覆蓋，用設定裡那個」** ——
    /// 而不是「權重 0」：裁決 R3 明寫 `0` 不是合法權重，
    /// 「不參與排程」**只有一種表達方式**，就是 `disable`。
    weight: AtomicU32,
    /// 這一格**最後一次被設定**的牆鍾時刻，`UNIX_EPOCH` 起的**納秒**。`0` = 從來沒設過。
    ///
    /// ★ ★ ★ 語義是 **「最後一次改動」**（owner 拍板，R6 留下的那條），
    /// ⛔ 不是「首次被覆蓋」：維運問的那句話是「這臺機器摘掉多久了」，
    /// 而那句話問的是**當前形態**的年齡，不是這一格的年齡。
    /// ⇒ 連著調 `set_weight` 會讓它一直往前走 —— 那**正確**，形態確實一直在變。
    ///
    /// ⚠ 取**牆鍾**而不是 `Instant`：這個值是給人看的、要能與紀錄對時間。
    /// 代價寫在明處：**牆鍾會跳**（NTP 校正、手動改時間）⇒ 極端情況下兩項覆蓋的
    /// 先後與它們的時間戳順序可以不一致。⛔ 不為此引入第二個單調時鐘：
    /// 那會讓 `/stats` 出現兩種時間，而「這兩個為什麼不一樣」沒人答得出來。
    ///
    /// ⚠ 納秒裝進 `u64` 到 **2554 年**溢出。⛔ 不存秒：與
    /// [`crate::SharedRuntime::loaded_at`] 同一條理由 —— 秒級粒度在一次跑得很快的
    /// 判據裡會落在同一秒，那會讓「改動會推進它」這條判據偶發抖動。
    set_at_nanos: AtomicU64,
}

impl UpstreamOverride {
    /// 蓋一次時間戳。**每一個會改動這一格的入口都要調它。**
    ///
    /// ⚠ 「設成同一個值」也算一次改動：管理面分不出「設成 X」與「又設了一次 X」，
    /// 兩者都是維運對這一格動的一次手。
    fn 盖时间戳(&self) {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.set_at_nanos.store(n, Ordering::Relaxed);
    }

    /// 這一格最後一次被設定的牆鍾時刻。沒設過就是 `UNIX_EPOCH`。
    ///
    /// ★ 出現在清單裡的每一格都設過（[`Self::has_override`] 為真的唯一途徑就是
    /// 走過下面那幾個 setter），所以 [`OverrideEntry::set_at`] 上不需要 `Option`。
    pub fn set_at(&self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH
            + std::time::Duration::from_nanos(self.set_at_nanos.load(Ordering::Relaxed))
    }

    /// 這個上游被維運摘掉了嗎。
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }

    /// 摘掉（`true`）或放回（`false`）。
    ///
    /// ⚠ 放回 = **撤銷這一半覆蓋**，不是「加一條 enable 覆蓋」：
    /// 一格上兩個量都回到預設值之後，它就不再是一項覆蓋了（見 [`Self::has_override`]）。
    pub fn set_disabled(&self, v: bool) {
        self.disabled.store(v, Ordering::Relaxed);
        self.盖时间戳();
    }

    /// 覆蓋權重。`None` = **沒覆蓋**，排程該用設定裡那個。
    pub fn weight(&self) -> Option<u32> {
        match self.weight.load(Ordering::Relaxed) {
            0 => None,
            w => Some(w),
        }
    }

    /// 設一個覆蓋權重。**值域與設定權重逐字相同**（裁決 R3：`[1, 65535]`）。
    ///
    /// ★ ★ 值域檢查落在**這一處**，⛔ 任務 4 的 `POST /runtime` 不許再寫一份：
    /// 兩份「差不多的」值域檢查遲早分家，而分家的表現是
    /// 「管理面收下了一個設定層根本寫不出來的權重」。
    /// ⚠ 值域常數只有一份，在 `fulcrum_config::model` 裡。
    ///
    /// ⚠ `0` 在這裡是**錯誤**而不是「清除覆蓋」：清除走 [`Self::clear_weight`]。
    /// 兩件事共用一個入參值，就等於讓「手滑寫了 0」與「我要撤銷」長得一模一樣。
    pub fn set_weight(&self, w: u32) -> Result<(), String> {
        use fulcrum_config::model::{MAX_UPSTREAM_WEIGHT, MIN_UPSTREAM_WEIGHT};
        if !(MIN_UPSTREAM_WEIGHT..=MAX_UPSTREAM_WEIGHT).contains(&w) {
            return Err(format!(
                "权重 {w} 不在 [{MIN_UPSTREAM_WEIGHT}, {MAX_UPSTREAM_WEIGHT}] 内\
                 （`0` 不是「不参与调度」——那要用 disable）"
            ));
        }
        self.weight.store(w, Ordering::Relaxed);
        self.盖时间戳();
        Ok(())
    }

    /// 撤掉覆蓋權重，回到設定裡那個。
    pub fn clear_weight(&self) {
        self.weight.store(0, Ordering::Relaxed);
        self.盖时间戳();
    }

    /// 這一格**現在**帶著覆蓋嗎。
    ///
    /// ★ ★ 問的是「現在」，不是「有沒有人碰過」：`disable` 之後又 `enable` 回去，
    /// 這一格就重新變回**空格子**。⇒ 這與 G18 那句「當前有幾項臨時覆蓋生效中」
    /// 是同一個問題，而 `/stats` 的條目數、`/load` 的回話計數、
    /// `fulcrum_overrides_active` 三處都必須是**這一個**答案。
    pub fn has_override(&self) -> bool {
        self.is_disabled() || self.weight().is_some()
    }
}

/// 登記處裡的一條**清單項**：一格覆蓋，連同它現在還管不管得到誰（裁決 R8）。
///
/// ⚠ 清單**只列設過覆蓋的那些格子**（[`UpstreamOverride::has_override`]）——
/// 空格子是惰性的，不計數、不出現在任何清單裡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideEntry {
    pub key: OverrideKey,
    pub disabled: bool,
    /// 覆蓋權重，`None` = 這一格沒覆蓋權重（只是 `disable`）。
    pub weight: Option<u32>,
    /// ★ **懸空**（裁決 R8）：這個鍵在**當前**執行環境裡落不到任何上游。
    ///
    /// ⚠ ⚠ 懸空的覆蓋**不刪**：與 HAProxy 那個「runtime 改動 reload 之後無聲消失」
    /// 正相反 —— `keep` 就是 keep，而「它現在管不到誰」這件事**說出來**。
    /// ⚠ 懸空的**照樣算「生效中」**（R13 明寫）：它確實還在登記處佔著一格。
    pub dangling: bool,
    /// 這一格**最後一次被設定**的牆鍾時刻（R6 留下的那條，owner 拍板取「最後一次改動」）。
    ///
    /// ⚠ ⚠ 它答的是「**這臺機器現在這個樣子擺了多久**」，⛔ 不是「這一格多老」——
    /// 先改 `weight` 再 `disable`，取「首次」的讀數會讓人以為它已經被摘掉兩天了，
    /// 而其實兩分鐘前才摘。
    ///
    /// ★ 換成絕對 Unix 秒是**落到 `/stats` 那一刻**才做的事（`metrics::unix_secs`，
    /// 與 `config_loaded_at_unix` / `not_after_unix` 同一個換算）——⛔ 這裡不存秒，
    /// 理由與 [`crate::SharedRuntime::loaded_at`] 逐字相同。
    pub set_at: std::time::SystemTime,
}

/// 一條已經解析好、準備落到某一格的**操作**（**M2 批 N 任務 4**，`POST /runtime`）。
///
/// ★ `admin.rs::runtime` 的解析階段會**先**用 [`UpstreamOverride::set_weight`] 自己的
/// 判斷檢核一遍 `weight`（對著一格臨時的 `UpstreamOverride::default()` 跑，只借判斷、
/// 不碰真實狀態）——這給出更貼切的錯誤訊息（不與"這個鍵找不到"混在一起）。
///
/// ⚠ ⚠ ⚠ **但 `RuntimeOp` 與 [`OverrideLayer::apply_all`] 都是 `pub`**（修復輪 1，
/// 評審 M4）：管理面之外的呼叫方——比如任務 5/6——完全可以繞過 `admin.rs` 那道預檢，
/// 直接建構一個越界的 `SetWeight(0)`。⇒ **值域的唯一真正守衛在 [`OverrideLayer::apply_all`]
/// 自己身上**：它在施加前會用同一個 `UpstreamOverride::set_weight` 再檢核一次，越界的
/// 一律併入尋址失敗的 `Err`，⛔ 不會靜默吞掉。這不是"兩份差不多的值域檢查"——
/// 兩處調的是**同一個函式**，不是兩份手寫的平行邏輯；`admin.rs` 的預檢只是為了更好的
/// 錯誤訊息，不是唯一的防線。
#[derive(Debug, Clone, Copy)]
pub enum RuntimeOp {
    /// 摘掉（`true`）或放回（`false`）。
    SetDisabled(bool),
    /// 設一個覆蓋權重。⚠ **值域到這裡還沒保證合法**——由 [`OverrideLayer::apply_all`]
    /// 在施加前用 [`UpstreamOverride::set_weight`] 檢核（見上）。
    SetWeight(u32),
}

/// [`RuntimeOp`] 加上它要落在登記處的哪一格。
#[derive(Debug, Clone)]
pub struct RuntimeAction {
    pub key: OverrideKey,
    pub op: RuntimeOp,
}

/// **格子登記處**：本處理程序當前的全部臨時覆蓋（G18 / 裁決 R7）。
///
/// 它**跨換代活著** —— 全量 load 換掉的是 [`crate::Runtime`]，不是這裡。
/// ⇒ 它掛在 [`crate::SharedRuntime`] 上（那是唯一跨換代活著的東西），
/// ⛔ 別在別處再存一份。
///
/// # ⚠ 空格子的生命週期
///
/// [`crate::Runtime::build_with_overrides`] 會給這份設定裡的**每一個**鍵都登記一格
/// （沒有就現建）—— 於是絕大多數格子是**空的**（沒設過任何覆蓋）。它們：
///
/// - **不計數、不進清單**（[`Self::active_count`] / [`Self::entries`] 只看設過覆蓋的）
///   ⇒ 一次建到一半失敗的 load 不會讓「有幾項覆蓋」這個數動一下（G8：不留痕跡）；
/// - 在一次**成功的** swap 之後被清掉（[`Self::retain_after_swap`]），登記處不無界增長。
///
/// ⚠ ⚠ **清的只是「沒人引用**且**沒設過覆蓋」的那些**：設過覆蓋而暫時沒人引用的
/// 是**懸空**（R8），必須留著 —— 那正是 `overrides=keep` 的全部意義。
#[derive(Debug, Default)]
pub struct OverrideLayer {
    /// ⚠ `BTreeMap` 而不是 `HashMap`：清單是「永遠可見」的東西（G18），
    /// 而每次換一個順序的清單沒法逐次比對。
    grids: std::sync::Mutex<BTreeMap<OverrideKey, Arc<UpstreamOverride>>>,
}

impl OverrideLayer {
    pub fn new() -> OverrideLayer {
        OverrideLayer::default()
    }

    /// ⚠ 鎖中毒不該讓轉發或管理面停擺：中毒說明某個持鎖執行緒 panic 過，
    /// 而這張表本身仍然是完整的（同 [`crate::SharedRuntime::current`] 的處置）。
    fn grids(&self) -> std::sync::MutexGuard<'_, BTreeMap<OverrideKey, Arc<UpstreamOverride>>> {
        self.grids.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 按鍵取那一格，**沒有就現建並登記**。
    ///
    /// ★ ★ 這是 R7 的核心動作：[`crate::Runtime::build_with_overrides`] 給每個上游
    /// 掛格子走的就是它 ⇒ **鍵相同的上游拿到的是同一個 [`Arc`]**。
    pub fn slot(&self, key: &OverrideKey) -> Arc<UpstreamOverride> {
        let mut g = self.grids();
        if let Some(s) = g.get(key) {
            return s.clone();
        }
        let s = Arc::new(UpstreamOverride::default());
        g.insert(key.clone(), s.clone());
        s
    }

    /// 按鍵查那一格，**不建**。`None` = 登記處裡沒有這個鍵。
    pub fn get(&self, key: &OverrideKey) -> Option<Arc<UpstreamOverride>> {
        self.grids().get(key).cloned()
    }

    /// 把一格**已經存在的**格子收進登記處。
    ///
    /// ★ 只有一個用途：[`crate::SharedRuntime::new`] 從一份**已經建好的**執行環境身上
    /// 認領它的格子 —— 那條路上 `Runtime` 已經是 `Arc`，拿不到 `&mut` 再掛一遍。
    /// ⚠ 它之所以是對的，是因為**任何一份執行環境的格子都已經按鍵共享過**
    /// （[`crate::Runtime::build`] 自己也走 [`Self::slot`]，只是那張登記表是一次性的）
    /// ⇒ 同一個鍵上認領幾次，認領到的都是同一個 [`Arc`]。
    pub fn adopt(&self, key: OverrideKey, slot: Arc<UpstreamOverride>) {
        self.grids().insert(key, slot);
    }

    /// **當前有幾項臨時覆蓋生效中**（G18 原話）。⚠ 空格子不算。
    pub fn active_count(&self) -> usize {
        self.grids().values().filter(|s| s.has_override()).count()
    }

    /// 登記處裡一共有幾格（**含空格子**）。
    ///
    /// ⚠ ⚠ 它**不是**「有幾項覆蓋」—— 那是 [`Self::active_count`]。
    /// 這個數只給判據用：一條「空格子不計數」的判據，必須能證明**當時真的有空格子**，
    /// 否則它在「一格都沒建」的實作下同樣是綠的（那是一把好壞兩種情況讀數相同的尺）。
    pub fn slot_count(&self) -> usize {
        self.grids().len()
    }

    /// 覆蓋清單（裁決 R8：懸空的**留著**並標出來）。
    ///
    /// ⚠ `live` 是**當前執行環境**裡全部的鍵（[`crate::Runtime::override_keys`]）。
    /// ★ 別自己去拼這份 `live`：走 [`crate::SharedRuntime::override_entries`]，
    /// 那裡的 `live` 一定取自**正在服務的**那一份執行環境。
    pub fn entries(&self, live: &BTreeSet<OverrideKey>) -> Vec<OverrideEntry> {
        self.grids()
            .iter()
            .filter(|(_, s)| s.has_override())
            .map(|(k, s)| OverrideEntry {
                key: k.clone(),
                disabled: s.is_disabled(),
                weight: s.weight(),
                dangling: !live.contains(k),
                // ★ 從**那一格自己**身上讀，⛔ 不在這裡取一次 `now()`：
                //   那樣每次列清單都會把所有時間刷成現在，而三條判據裡只有一條看得出來。
                set_at: s.set_at(),
            })
            .collect()
    }

    /// 一次**成功的** swap 之後收拾登記處。回傳清掉了幾格。
    ///
    /// 清掉的是「**新執行環境不引用**、**且**沒設過覆蓋」的空格子。
    ///
    /// ⚠ ⚠ 兩個條件缺一不可：
    /// - 少了「不引用」⇒ 會把新執行環境正拿著的那些空格子從登記表裡摘掉，
    ///   於是管理面按鍵找到的將是一格**新的**格子，改它對那個上游毫無作用；
    /// - 少了「沒設過覆蓋」⇒ 會把**懸空覆蓋**當垃圾清掉，那正是 R8 明令保留的東西。
    pub fn retain_after_swap(&self, live: &BTreeSet<OverrideKey>) -> usize {
        let mut g = self.grids();
        let before = g.len();
        g.retain(|k, s| live.contains(k) || s.has_override());
        before - g.len()
    }

    /// `overrides=clear`（**M2 批 N 任務 5**，G120）：撤銷登記處裡**全部**格子
    /// 上的覆蓋，回傳清空前確實帶著覆蓋的那些項（供呼叫方在回話裡逐項列出，
    /// G120 明寫「必須逐項列出」，⛔ 只給個數字不算數）。
    ///
    /// # ⛔ ⛔ ★ ★ ★ 不物理刪除格子
    ///
    /// 那些格子的 [`Arc`] 還被**正在服務的** `Upstream` 攥著（`override_slot()`）
    /// —— 物理刪掉的只是登記處裡的映射：同一個鍵，登記處再也查不到，而資料面
    /// 還在用著那一格；下一次 `build_with_overrides` 拿同一個鍵調 [`Self::slot`]
    /// 會**現建一格新的 `Arc`**，與仍在服務的那份**分家**——「覆蓋層與上游身上
    /// 那兩個量在結構上是同一份」這條本模組的立身之本當場破掉，而且不回報錯誤。
    ///
    /// ⇒ 這裡對每一格調 [`UpstreamOverride::set_disabled`]`(false)` +
    /// [`UpstreamOverride::clear_weight`]，讓 [`UpstreamOverride::has_override`]
    /// 變回 `false`——格子退化成**空格子**，身分不斷、映射不斷，下一次成功的
    /// swap 由 [`Self::retain_after_swap`] 自然回收。
    ///
    /// # ⚠ ⚠ 同一次持鎖內做完「枚舉 + 清空 + 收集」
    ///
    /// 與 [`Self::apply_all`] 同一條紀律：⛔ 不許「先 [`Self::entries`] 列一遍、
    /// 再在鎖外逐個調 [`Self::slot`] 去改」——那是同一個 TOCTOU 視窗的第二個
    /// 入口（`slot()` 在鍵不存在時還會現建一格，把一次並行的 `retain_after_swap`
    /// 剛收走的鍵又救回來）。本方法用一次 [`Self::grids`] 貫穿讀（記錄清空前的
    /// `disabled`/`weight`/`dangling`）與寫（清空），中間不會被任何並行的
    /// `apply_all` / `slot` / `retain_after_swap` 插進來。
    ///
    /// `live` 與 [`Self::entries`] 同一口徑：呼叫方傳
    /// [`crate::SharedRuntime::override_entries`] 背後那份 **swap 之後**的鍵集，
    /// 用來給每一項被清掉的覆蓋標好 `dangling`。
    pub fn clear_all(&self, live: &BTreeSet<OverrideKey>) -> Vec<OverrideEntry> {
        let g = self.grids();
        let mut cleared = Vec::new();
        for (k, s) in g.iter() {
            if !s.has_override() {
                continue;
            }
            cleared.push(OverrideEntry {
                key: k.clone(),
                disabled: s.is_disabled(),
                weight: s.weight(),
                dangling: !live.contains(k),
                // ⚠ **在清掉之前讀**：這條清單說的是「被清掉的那項覆蓋當時是什麼樣」，
                //   而下面兩行清空動作本身也會蓋一次時間戳 —— 讀晚了就變成
                //   「每一項都是剛剛設的」，那對每一條都是錯的。
                set_at: s.set_at(),
            });
            s.set_disabled(false);
            s.clear_weight();
        }
        cleared
    }

    /// `POST /runtime` **唯一**的寫入口（**M2 批 N 任務 4**）：一次持鎖內做完
    /// 「每一條都指得到」與「全部施加」——**全有或全無**，且不會撞上
    /// [`Self::retain_after_swap`] 的 TOCTOU（任務 3 評審開出來的口子）。
    ///
    /// # ⚠ ⚠ ⚠ 為什麼不能寫成「先 `get()` 一批 `Arc`，再逐個改」
    ///
    /// 拆成兩步的話，中間可能被一次 `/load`（內部走 [`Self::retain_after_swap`]）
    /// 插進來，把某一格從登記處移出去——呼叫方手上那個 `Arc` 還在，改的是一個
    /// 已經沒人認的孤兒：命令回 200，覆蓋靜默丟失，`/stats` 也看不到它
    /// （登記處按鍵查不到）。⇒ 「查存在」與「改」必須在**同一次**持鎖內完成，
    /// 本方法就是那個複合動作。
    ///
    /// # 尋址口徑：`live` **與** 登記處**都**要認，兩者缺一不可
    ///
    /// `live` 是**當前正在服務**那份執行環境的鍵集（呼叫方傳
    /// [`crate::Runtime::override_keys`]）。一條動作的鍵必須**同時**滿足：
    ///
    /// - **在 `live` 裡**——把「懸空」（鍵還留在登記處，但當前執行環境已經不認）
    ///   擋在外面：懸空覆蓋是 `keep` 的展示物件（裁決 R8），不是 `/runtime` 的
    ///   操作物件；
    /// - **登記處裡現在真的有這一格**（不靠 `live`，靠這一次持鎖內的 `grids`
    ///   本身）——防的正是上面那個 TOCTOU：萬一呼叫方傳來的 `live` 是舊快照，
    ///   而這一格真的已經被並行的一次 `/load` 收走了，這裡仍然能當場發現並
    ///   拒絕，不會去改一個孤兒。
    ///
    /// 任一條指不到、或任一條 `SetWeight` 越界 ⇒ **一格都不改**，回傳全部這樣的鍵
    /// （`Err`，供呼叫方拼 400 訊息時逐條列出）。全部通過 ⇒ 在這同一次持鎖內逐條
    /// 施加，`Ok(())`。
    ///
    /// # ⚠ ⚠ ⚠ 值域檢核（修復輪 1，評審 M4）：這裡**不信任呼叫方**
    ///
    /// `RuntimeOp` 與本方法都是 `pub`——`admin.rs` 不是唯一可能的呼叫方（任務 5/6
    /// 已經在排隊）。一個繞過 `admin.rs` 解析階段、直接建構 `SetWeight(0)` 的呼叫方，
    /// 如果這裡只顧著"查存在"而不回頭看值域，就會在下面的 `set_weight` 上悄悄拿到
    /// 一個 `Err` 而繼續把整批當 `Ok` 收尾——**命令回 200，權重卻沒真的變**。
    /// ⇒ 值域檢查**在這裡、與尋址判定同一次遍歷裡**做一遍：借
    /// [`UpstreamOverride::set_weight`] 自己的判斷（對著一格臨時格子跑），
    /// 不是重新手寫一份平行的邊界檢查。
    pub fn apply_all(
        &self,
        live: &BTreeSet<OverrideKey>,
        actions: &[RuntimeAction],
    ) -> Result<(), Vec<OverrideKey>> {
        let g = self.grids();
        let addressable = |k: &OverrideKey| live.contains(k) && g.contains_key(k);
        let valid = |op: &RuntimeOp| match *op {
            RuntimeOp::SetDisabled(_) => true,
            RuntimeOp::SetWeight(w) => UpstreamOverride::default().set_weight(w).is_ok(),
        };
        let bad: Vec<OverrideKey> = actions
            .iter()
            .filter(|a| !addressable(&a.key) || !valid(&a.op))
            .map(|a| a.key.clone())
            .collect();
        if !bad.is_empty() {
            return Err(bad);
        }
        // ★ 全部指得到、值域也全部通過，逐條施加。上面已經在**同一次**持鎖內
        //   確認過每一格都在 `g` 裡、每個 `SetWeight` 都合法 ⇒ 下面這兩步結構上
        //   都不會失敗（沒有任何程式碼能在這次持鎖期間插進來改 `g`，`set_weight`
        //   剛用同樣的入參驗證過一次）。`debug_assert!` 只是給這份"結構上不會
        //   失敗"的斷言留一個看得見的釘子——真出現了就是這份不變數本身被破壞，
        //   而不是把它悄悄吞掉再回 `Ok`。
        for a in actions {
            let Some(slot) = g.get(&a.key) else {
                continue;
            };
            match a.op {
                RuntimeOp::SetDisabled(v) => slot.set_disabled(v),
                RuntimeOp::SetWeight(w) => {
                    let r = slot.set_weight(w);
                    debug_assert!(r.is_ok(), "刚在同一次持锁内校验过的权重不该再被拒");
                }
            }
        }
        Ok(())
    }
}
