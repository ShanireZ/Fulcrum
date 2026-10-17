//! 樞衡的**執行環境物件圖**：結構化設定 → 站台索引 / 匹配器 / 執行鏈。
//!
//! ```text
//! DSL ──编译──▶ 结构化配置（JSON，唯一内部事实）
//!                      │  ← 本 crate 从这里开始
//!                      ▼
//!               运行时对象图（站点索引、匹配器、执行链、上游池）
//!                      │
//!                      ▼
//!               数据面（Pingora；见 crates/fulcrum-server）
//! ```
//!
//! # 三條設計約束
//!
//! 1. **不引用 `pingora-core`** —— 路由語義長在 `HttpServerApp` 裡就只能靠真流量測，
//!    而那樣紅了指不到具體哪條規則。
//! 2. **建置期把話說完** —— 正則、CIDR、上游位址全在 [`Runtime::build`] 裡解析並回報錯誤。
//!    留到請求路徑上發現的設定錯誤，就是把設定問題變成了線上事故。
//! 3. **「解析得過」不等於「接上了」** —— 未串接的能力由 [`UNWIRED`] 逐條列出，
//!    並由一條測試釘住。

pub mod glob;
pub mod matcher;
pub mod overrides;
pub mod proxyproto;
pub mod request;
pub mod template;

use fulcrum_config::host::wildcard_covers;
// ★ 與 `remote_ip` 匹配器**同一份** CIDR 實作（G50 的鄰居）——
//   另寫一份「差不多的」網段匹配，就是把 v4/v6 不互通那條坑再挖一次。
use fulcrum_config::model::{
    Defaults, HeaderOp, L4Config, MatcherRef, Step, StepBody, StructuredConfig, TlsConfig, TlsMode,
};
use glob::Cidr;
use matcher::{BuildError, CompiledMatcher};
use overrides::{OverrideEntry, OverrideKey, OverrideLayer, UpstreamOverride};
use request::{RequestCtx, ResponseCtx};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use template::Template;

/// DSL 認得、但**執行環境還不做**的能力。
///
/// ★ 未實作要在**裝載時**可見（同 G52 的回落清單）：一條「編譯通過、執行環境靜靜什麼都不做」
/// 的指令，正是本儲存庫反覆抓到的「宣告了卻沒人接」。
///
/// ⚠ `tests/unwired_contract.rs` **逐字釘住**這份清單：串接時必須同時刪掉這裡的條目，
/// 於是「實作了忘了刪」與「刪了沒實作」都過不去。
/// ⚠ 這張表只登記「**DSL 認得、執行環境不做**」—— 一個連語法都還不存在的能力不屬於這一類
/// （寫進來反而會讓人以為寫得出來），一個「做了但有一塊夠不著」的能力也不屬於（那是 D 號）。
pub const UNWIRED: &[(&str, &str)] = &[
    (
        "tls_internal",
        "`tls internal` 的自签要一个证书生成器，随 ACME 那一批一起做",
    ),
    // ⚠ 別照抄舊理由。舊的是「`resolve()` 是同步的，需要一座橋」，那是 rustls 那側的形狀；
    //   換 BoringSSL 之後橋本來就在（`set_async_select_certificate_callback` 原生非同步）。
    //   ★ **一條阻塞理由消失不等於那件事做完了** —— 剩下的活兒見登記詞。
    (
        "on_demand",
        "握手期按 SNI 现签（G15）—— 欠 `ask` 端点、并发闸门与失败缓存",
    ),
    ("tracing", "预留指令，M1 不产生行为（G60）"),
    // ★ ★ **`passive_fail` 在 G136 銷號**：篩子第四條（`Upstream::passive_ok`）真的擋得住，
    //   落賬在 `fulcrum-server` 的收尾處。⚠ 與它一起刪掉的還有 `dsl-reference.md`
    //   那句未串接清單裡的 `passive_fail`，以及下面 `used.insert("passive_fail")` 那一格 ——
    //   `tests/unwired_contract.rs` 兩個方向都釘著，少改一處就紅。
    // ★ ★ **`weight` 在 M2 批 N 任務 2 銷號**：四種 `lb_policy` 全部按累積權重挑
    //   （[`weighted_slot`] / [`less_loaded`]），於是它不再是「DSL 認得、執行環境不做」。
    //   ⚠ 與它一起刪掉的還有文件那句未串接清單裡的 `weight`（`tests/unwired_contract.rs`
    //   兩個方向都釘著），以及子指令表那一行的收尾話。
];

/// 負載均衡策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbPolicy {
    RoundRobin,
    LeastConn,
    IpHash,
    Random,
}

impl LbPolicy {
    fn parse(s: &str) -> Option<LbPolicy> {
        Some(match s {
            "round_robin" => LbPolicy::RoundRobin,
            "least_conn" => LbPolicy::LeastConn,
            "ip_hash" => LbPolicy::IpHash,
            "random" => LbPolicy::Random,
            _ => return None,
        })
    }
}

/// 一個上游。
///
/// ⚠ ⚠ **請求路徑唯讀 [`Upstream::dial_addr`]，永不做 DNS。** `HttpPeer::new` 裡是
/// `address.to_socket_addrs().unwrap()` —— 阻塞的 `getaddrinfo`，而且失敗就 panic
/// ⇒ 一個解析不了的網域上游會讓那個站台**每個請求 panic 一次**。
/// 解析由 [`resolve_upstreams`] 在啟動時、全量 load 時與後台任務裡做（後者即 `dns_refresh`）；
/// 解析不出來的上游 [`ProxyTarget::pick`] 跳過，全都跳過就回乾淨的 502。
/// 處理程序啟動那一刻。★ ★ 被動熔斷的所有時間判斷都從這裡起算。
///
/// ⚠ ⚠ **⛔ 不用系統時鐘（`SystemTime` / `UNIX_EPOCH`）**：維運校一次時間、或者一次 NTP
/// 跳變，就會讓一個熔斷中的上游**永久卡在熔斷裡**（`open_until` 被推到很遠的未來），
/// 或者反過來**瞬間全部解除**。兩種都不回報錯誤、都沒有症狀指向時鐘。
/// ⇒ 用單調時鐘，它對系統時間的改動免疫。
static PASSIVE_EPOCH: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// 從處理程序啟動起算的毫秒數，**恆 ≥ 1**。★ 只在被動熔斷這一族用。
///
/// ⚠ ⚠ ⚠ **那個 `+ 1` 是判據本身，不是湊數。**
/// [`PassiveState::open_until_ms`] 拿 **`0` 當「沒在熔斷」的哨兵**，而處理程序剛起來時
/// `elapsed()` 真的會回傳 **0 毫秒** ⇒ 若此刻發生一次冷卻為 0 的熔斷，
/// `open_until` 正好落在哨兵上，**熔斷當場被讀成「沒熔斷」**。
/// ★ 這不是推理：寫這一族時它當場被 `tests/passive.rs` 那兩條並行/半開判據逮住了
/// （`assertion failed: u.passive_open()`）。
/// ⇒ 讓時間戳恆 ≥ 1，`0` 就**只可能**是哨兵。
fn passive_now_ms() -> u64 {
    (PASSIVE_EPOCH.elapsed().as_millis() as u64).saturating_add(1)
}

/// 被動熔斷的**策略**（每條 `reverse_proxy` 一份，G136）。
///
/// ★ 它在 [`ProxyTarget`] 上而狀態在 [`Upstream`] 上，因為這兩件事的粒度不同：
/// 閾值是**這條 `reverse_proxy` 的設定**，而失敗計數是**每個上游各自的**。
#[derive(Debug, Clone, Copy)]
pub struct PassivePolicy {
    /// 視窗內失敗幾次就熔斷。
    pub threshold: u32,
    /// 數失敗的視窗。
    pub window_ms: u64,
    /// 熔斷後歇多久，然後放**一個**半開探針。
    pub cooldown_ms: u64,
}

/// 一個上游的被動熔斷**狀態**（G136）。
///
/// ★ ★ **它與 [`Upstream::healthy`] 各佔一格、互不覆蓋**，這是 G136 的承重決策：
/// 主動健康檢查探的是一條專門的 `health_uri`，而被動熔斷看的是**真實流量**。
/// 讓主動探測成功去清掉被動熔斷，等於在「`/health` 回 200 而真實業務在 500」
/// 這個**唯一讓被動熔斷存在的場景**上把它關掉。
///
/// ⚠ 全部用原子量、⛔ 不上鎖：它讀在 [`ProxyTarget::pick_index_by`] 這條熱路徑上。
#[derive(Debug, Default)]
pub struct PassiveState {
    /// 當前視窗內的失敗數。
    fails: std::sync::atomic::AtomicU32,
    /// 當前視窗的起點（`passive_now_ms`）。
    window_start_ms: std::sync::atomic::AtomicU64,
    /// 熔斷到什麼時候為止。**`0` = 沒在熔斷**。
    ///
    /// ★ ★ ★ **半開的「恰好放一個」就落在這一個原子量上**：冷卻期滿之後，
    /// 第一個把它從 `until` CAS 成 `now + cooldown` 的請求當探針，其餘讀到新值照舊被擋。
    /// ⛔ 換成「到期就全放」的話，一個仍然壞著的上游每個冷卻週期要收走 `threshold` 個
    /// 使用者可見的 5xx —— 本儲存庫**不換上游重試**，那些全是真錯。
    ///
    /// ⚠ ⚠ **為什麼不另設一個 `probe_taken` 布爾：那會死鎖。** 權杖是在**篩選**時搶的，
    /// 而被篩進來的上游未必被 `lb_policy` 選中 ⇒ 沒被選中的那個權杖**再也沒人還**，
    /// 該上游從此永久出局，且沒有任何症狀指向這裡。
    /// ★ 對 `open_until` 本身 CAS 就沒有這個形狀：探針資格丟了也只是**多等一個冷卻週期**。
    /// ⚠ 代價寫在明處：同一瞬間有多個上游同時到期時，它們會各消耗一次探針資格，
    /// 而只有一個真的收到請求 —— 後果是那幾個多等一輪，⛔ 不是卡死。
    open_until_ms: std::sync::atomic::AtomicU64,
}

#[derive(Debug)]
pub struct Upstream {
    /// **歸一化之後**的 `host:port`（[`normalize_upstream`]：`backend` → `backend:80`）。
    ///
    /// ⚠ 它仍然是 **SNI 的來源**：上游走 https 時要拿網域去握手，不能拿 IP。
    /// ★ ★ 它同時是**覆蓋層鍵的第三格**（G125 / 裁決 R6 ⑤）——「執行環境裡那個上游叫什麼」
    /// 只有這一個答案，管理面對著的是執行環境。
    /// ⇒ 一條寫 `backend`、另一條寫 `backend:80` 的兩條 `reverse_proxy`
    /// 在這一格上**逐字相同** ⇒ 它們拿到的是**同一個**覆蓋格子（裁決 R6 ③ 第二輪）。
    /// ⚠ ⚠ **設定那一行上的原文 token 在執行環境裡沒有留檔**（M2 批 N 任務 2.9 刪掉了
    /// 任務 2.8 加的 `raw_addr`）：它當時只有一個消費者 —— 撞鍵那條**拒絕訊息** ——
    /// 而拒絕已經拆掉。⇒ 鍵的第三格因此**在結構上不可能**取到原文 token：
    /// 那個串根本不在 [`SiteRt`] 這張圖裡。若哪天 `/stats` 要回顯設定原文，
    /// 那是重新加一格欄位 + 兩處建構點各一行，**並且那時它才有消費者**。
    pub addr: String,
    /// **設定權重**（`weight` 子指令，裁決 R1/R3），值域
    /// `[fulcrum_config::model::MIN_UPSTREAM_WEIGHT, MAX_UPSTREAM_WEIGHT]`，沒寫就是 1。
    ///
    /// ★ 私有 + [`Upstream::weight`] 取值，是為了讓「排程實際用哪個權重」**只有一個出口**：
    /// M2 批 N 任務 3 的臨時覆蓋層要讓維運改過的權重蓋過設定權重，那時改的是那個方法，
    /// 而不是讓每個呼叫點各自記得「先看覆蓋層再看設定」——後者遲早漏一處，
    /// 而漏掉的表現是「改了權重沒生效」，不回報錯誤。
    weight: u32,
    /// 解析好的位址。`None` = 還沒解析出來，或上一次解析失敗。
    ///
    /// ★ IP 字面值在 [`Runtime::build`] 裡就填好了（那不需要 DNS，也不會變）；
    /// ⚠ **網域一律留空**，由 `serve` 那一側去解析 —— 否則 `fulcrum validate`
    /// 就會變成一個要聯網的命令，而它的全部價值在於**離線**就能說話。
    /// ⚠ ⚠ **存的是全部候選，不是第一個** —— `localhost` 的第一個位址可能是 `[::1]`
    /// 而上游只聽 `127.0.0.1`。只取第一個會挑中一個對端根本沒在聽的位址族，
    /// 而現場只有一句「連不上上游」。⇒ 連接失敗就試下一個。
    resolved: std::sync::RwLock<Vec<std::net::SocketAddr>>,
    /// 當前在飛的連接數，`least_conn` 用。由資料面維護。
    inflight: AtomicUsize,
    /// 主動健康檢查的判定。★ **初值是 `true`（健康）**。
    ///
    /// ⚠ 取 `false` 會把「探測還沒跑」變成一次**確定的全站 502**；取 `true` 只是把
    /// 那段視窗退化成「沒有健康檢查」。nginx 與 Caddy 同取後者。
    /// ⚠ 代價：**剛啟動的那一個探測週期內 `health_uri` 等於沒有**。
    /// ★ 沒配 `health_uri` 的目標永不被探測，這一格恆為 `true`。
    healthy: std::sync::atomic::AtomicBool,
    /// 被動熔斷的狀態（**G136**）。★ 與上面 `healthy` **各佔一格、互不覆蓋** ——
    /// 理由見 [`PassiveState`]：讓主動探測清掉被動熔斷，等於在唯一需要它的場景上關掉它。
    ///
    /// ⚠ 沒配 `passive_fail` 時這一格**一次都不會被寫**（[`Upstream::passive_ok`] 第一行
    /// 就按策略是不是 `None` 短路）⇒ 既有設定的行為一個字都不變。
    passive: PassiveState,
    /// **臨時覆蓋層裡屬於這個上游的那一格**（**M2 批 N 任務 3**，G18 / 裁決 R7）。
    ///
    /// ★ ★ ★ 它**就是**登記處 [`OverrideLayer`] 裡按
    /// `(站点名, reverse_proxy 的 id, 本上游的 addr)` 取到的那個 [`std::sync::Arc`] ——
    /// 「覆蓋層」與「上游身上那兩個量」在結構上是**同一份**，不存在
    /// 「改了列表忘了應用」，也不存在「應用了沒記進列表」。
    ///
    /// ⚠ ⚠ **建構點上先放一格私有的預設格子，真正的那一格由
    /// [`Runtime::attach_overrides`] 在圖建完之後掛上去** —— 因為 `build_step`
    /// 拿不到站台名（它只在 `Runtime::build` 的循環裡）。
    /// ⇒ **某條路徑漏了「掛」這一步的話，這個上游會悄悄拿著一個誰也夠不著的私有格子**：
    /// 管理面改登記處那一格對它毫無作用，而沒有任何東西會回報錯誤
    /// —— 「設定照過、命令回 200、而那臺機器還在收流量」。
    /// ★ 擋它的是 `tests/overrides.rs` 那條 `Arc::ptr_eq` 判據（比**物件身分**，
    /// ⛔ 不比「兩格的值相等」：值相等在兩者都是預設值時恆真）。
    ///
    /// ⚠ L4 那條路上**這一格就是最終值**：L4 的上游沒有站台，按 R6 的鍵根本尋址不到
    /// （見 [`ProxyTarget::id`]）⇒ 它永遠拿著自己那格預設的、誰也改不了的空格子。
    ov: std::sync::Arc<UpstreamOverride>,
}

impl Upstream {
    /// 被動熔斷這一格放不放行 —— [`ProxyTarget::pick_index_by`] 篩子的**第四條**（G136）。
    ///
    /// ⚠ ⚠ **它有副作用，而且只在恰好一種狀態下有**：冷卻期滿時，搶到探針資格的那一次
    /// 呼叫會把 `open_until` 重新上滿。⇒ [`ProxyTarget::pick_index_by`] 因此**不再是**
    /// 一個純函式。這一條寫在這裡，是因為讀篩子那一行的人看不見它。
    pub fn passive_ok(&self, policy: Option<&PassivePolicy>) -> bool {
        // ★ 沒配 `passive_fail` ⇒ 恆放行，且下面一個原子量都不碰（G136：不寫就是關閉）。
        let Some(p) = policy else {
            return true;
        };
        let until = self.passive.open_until_ms.load(Ordering::Acquire);
        if until == 0 {
            return true; // 閉合
        }
        let now = passive_now_ms();
        if now < until {
            return false; // 熔斷中
        }
        // 冷卻期滿：誰 CAS 成功誰當探針，其餘讀到新值繼續被擋。
        self.passive
            .open_until_ms
            .compare_exchange(
                until,
                now.saturating_add(p.cooldown_ms),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 這個上游此刻是不是被被動熔斷摘著（`/stats` 與指標用）。
    pub fn passive_open(&self) -> bool {
        self.passive.open_until_ms.load(Ordering::Relaxed) != 0
    }

    /// 當前視窗內已記了幾次失敗（`/stats` 用）。
    pub fn passive_fails(&self) -> u32 {
        self.passive.fails.load(Ordering::Relaxed)
    }

    /// 記一次**成功**。回傳 `true` = 它剛從熔斷裡回來（呼叫方據此打一行紀錄）。
    pub fn record_passive_success(&self, policy: Option<&PassivePolicy>) -> bool {
        if policy.is_none() {
            return false;
        }
        self.passive.fails.store(0, Ordering::Relaxed);
        self.passive.open_until_ms.swap(0, Ordering::AcqRel) != 0
    }

    /// 記一次**失敗**。回傳 `true` = 這一次把它熔斷了（呼叫方據此打一行紀錄）。
    ///
    /// ⚠ 計數是**近似**的：並行失敗之間沒有互斥，同一瞬間的兩次失敗可能都讀到同一個
    /// 視窗起點。⛔ 有意不為它加鎖 —— 這條路在資料面收尾處，而「閾值 5 實際在 4 或 6 觸發」
    /// 對熔斷這件事無害，一把鎖對吞吐有害。★ 說出來，不假裝它是精確的。
    pub fn record_passive_failure(&self, policy: Option<&PassivePolicy>) -> bool {
        let Some(p) = policy else {
            return false;
        };
        let now = passive_now_ms();
        // 已經在熔斷裡（含剛被 `passive_ok` 放出去的那個探針）⇒ 重新上滿冷卻。
        if self.passive.open_until_ms.load(Ordering::Acquire) != 0 {
            self.passive
                .open_until_ms
                .store(now.saturating_add(p.cooldown_ms), Ordering::Release);
            return false; // ⚠ 不是「這一次熔斷的」——它本來就斷著
        }
        // ★ `fails == 0` 就是「當前沒有視窗」。⛔ 不拿 `window_start == 0` 當判據：
        //   處理程序啟動後第一毫秒內它是一個合法的視窗起點。
        let start = self.passive.window_start_ms.load(Ordering::Relaxed);
        let n = if self.passive.fails.load(Ordering::Relaxed) == 0
            || now.saturating_sub(start) > p.window_ms
        {
            self.passive.window_start_ms.store(now, Ordering::Relaxed);
            self.passive.fails.store(1, Ordering::Relaxed);
            1
        } else {
            self.passive.fails.fetch_add(1, Ordering::Relaxed) + 1
        };
        if n >= p.threshold {
            self.passive.fails.store(0, Ordering::Relaxed);
            self.passive
                .open_until_ms
                .store(now.saturating_add(p.cooldown_ms), Ordering::Release);
            return true;
        }
        false
    }

    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Relaxed)
    }

    /// 排程用的**有效**權重：**覆蓋層裡有就用它，沒有才用設定權重**
    /// （**M2 批 N 任務 3**，G18 / 裁決 R7）。
    ///
    /// ★ ★ **排程只從這裡取權重**（[`ProxyTarget::pick_index_by`] 的四條分支都走它），
    /// 而 `weight` 欄位是**私有**的 —— 於是「某個呼叫點忘了先看覆蓋層」在**結構上做不到**。
    /// ⇒ 接覆蓋層只改了這一個函式，呼叫點一行都沒動。
    ///
    /// ⚠ 覆蓋層裡的 `0` 是**哨兵**（「沒覆蓋」），不是權重：裁決 R3 裡 `0` 不是合法權重，
    /// 「不參與排程」只有 `disable` 一種表達方式。
    pub fn weight(&self) -> u32 {
        self.ov.weight().unwrap_or(self.weight)
    }

    /// 維運在臨時覆蓋層裡把這個上游摘掉了嗎（**M2 批 N 任務 3**，G18）。
    ///
    /// ⚠ ⚠ 它與 [`Self::is_healthy`] 是**兩件事**：健康檢查說的是「它自己活著嗎」，
    /// 這裡說的是「維運要不要它收流量」。⇒ 兩條篩子都在
    /// [`ProxyTarget::pick_index_by`] 的那一個 `filter` 上，**摘掉的上游連它的權重一起出局**
    /// （否則「按 6:2:2 分而那個 6 沒在跑」會讓 60% 的請求落空）。
    pub fn is_disabled(&self) -> bool {
        self.ov.is_disabled()
    }

    /// 這個上游在臨時覆蓋層裡佔的**那一格**（**M2 批 N 任務 3**）。
    ///
    /// ★ 它與登記處按這個上游的鍵取到的那一格必須是**同一個物件**
    /// —— 由 `tests/overrides.rs` 那條 `Arc::ptr_eq` 判據釘住。
    pub fn override_slot(&self) -> &std::sync::Arc<UpstreamOverride> {
        &self.ov
    }

    /// 這個上游當前的**全部**候選位址。空 = 現在用不了。
    pub fn dial_candidates(&self) -> Vec<std::net::SocketAddr> {
        match self.resolved.read() {
            Ok(g) => g.clone(),
            // 鎖中毒不該讓轉發停擺；當成「暫時用不了」，交給別的上游。
            Err(p) => p.into_inner().clone(),
        }
    }

    /// 第一個候選。`None` = 這個上游現在用不了。
    ///
    /// ⚠ 轉發路徑**不要只用它**——見 `resolved` 欄位上那段：
    /// 第一個候選可能是一個對端根本沒在聽的位址族。
    pub fn dial_addr(&self) -> Option<std::net::SocketAddr> {
        self.dial_candidates().into_iter().next()
    }

    /// 記下一次解析結果。空向量 = 這次沒解析出來。
    pub fn set_resolved(&self, addrs: Vec<std::net::SocketAddr>) {
        match self.resolved.write() {
            Ok(mut g) => *g = addrs,
            Err(p) => *p.into_inner() = addrs,
        }
    }

    /// 這個上游是不是 IP 字面值（那就永遠不必再解析）。
    pub fn is_literal_ip(&self) -> bool {
        self.addr.parse::<std::net::SocketAddr>().is_ok()
    }

    /// 主動健康檢查判它活著嗎。沒配 `health_uri` 時恆 `true`。
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// 記下一次探測結論。**回傳它是不是翻轉了**。
    ///
    /// ★ 回傳翻轉與否，是為了讓呼叫方能做到「只在狀態翻轉時說話」。
    /// ⚠ 批 10 那條教訓：一個從頭壞到尾的上游每一輪 warn 一行，
    /// 一天就是上萬行，而紀錄被淹掉之後真正**新**出現的那一條也沒人看得見。
    /// 把「翻沒翻」算在這裡而不是讓每個呼叫方自己記上一次的值，
    /// 是因為後者會有第二份狀態，而兩份狀態遲早對不上。
    pub fn set_healthy(&self, v: bool) -> bool {
        self.healthy.swap(v, Ordering::Relaxed) != v
    }
    /// 借一個位置。資料面在建立上游連接前呼叫。
    pub fn acquire(&self) {
        self.inflight.fetch_add(1, Ordering::Relaxed);
    }
    /// 還回來。★ 必須與 [`Self::acquire`] 成對，否則 `least_conn` 會**單調漂移**
    /// 到永遠不選這個上游——而那不會有任何回報錯誤。
    pub fn release(&self) {
        // saturating：配對錯了也不該 wrap 成一個天文數字。
        let _ = self
            .inflight
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }
}

/// 一條 `reverse_proxy` 的執行環境形態。
#[derive(Debug)]
pub struct ProxyTarget {
    /// 這一條 `reverse_proxy` 的**穩定 id**（`id` 子指令，**M2 批 N 任務 2.8**，
    /// 裁決 R6 ⇒ G125）。**沒寫就是空串**。
    ///
    /// ★ ★ 空串**不是**「跳過」，它是鍵空間裡正正經經的一格：覆蓋層的鍵是
    /// `(站点名, id, 归一化后的上游地址)`，兩條都沒寫 id 的 `reverse_proxy`
    /// 指著同一臺機器時，它們的鍵**真的相同** ⇒ 它們**共享同一個覆蓋格子**
    /// （裁決 R6 ③ 第二輪 ⇒ G125）：一次 `disable` 把它們一起摘掉。
    /// ⇒ ⛔ 不許改成 `Option<String>` 再在別處「`None` 就不查」：
    /// 那會讓**主場景**（兩條都沒寫 id）靜靜地落到別的分支上。
    ///
    /// ⚠ ⚠ **撞鍵不是錯誤**：一個後端掛在幾組 `handle` 路由後面是反代最常見的寫法
    /// （本儲存庫四個端到端場景裡就有四份）。「一起摘掉」多半正是要的語義 ——
    /// 那臺機器壞了，它不該從任何路由收流量。想細分的人寫 `id` 就能分開。
    /// ⇒ 「這一格覆蓋管著幾條」由 [`Runtime::proxy_key_fanout`] 算出來，
    /// 經 `/stats` 顯示（G18「不持久化但永遠可見」），⛔ 不用裝載期 warning。
    ///
    /// ⚠ L4 那條路也用 [`ProxyTarget`]，那邊恆為空串：`l4` 塊裡沒有 `reverse_proxy`，
    /// 也就沒有 id；L4 的上游沒有站台，按 R6 的鍵根本尋址不到它們。
    pub id: String,
    pub upstreams: Vec<Upstream>,
    pub policy: LbPolicy,
    pub header_up: Vec<HeaderOpRt>,
    pub header_down: Vec<HeaderOpRt>,
    /// 上游用 https。
    pub tls: bool,
    pub tls_insecure_skip_verify: bool,
    /// 主動健康檢查。`None` = 沒配 `health_uri`，這一組上游永遠不被探測。
    pub health: Option<HealthPolicy>,
    /// 被動熔斷策略（**G136**）。**`None` = 這條 `reverse_proxy` 不做被動熔斷。**
    ///
    /// ★ 它與 `health` 有意是兩個 `Option`：兩套判定各說各的，⛔ 互不覆蓋。
    pub passive: Option<PassivePolicy>,
    /// 上一次探測這一組上游的時刻。`None` = 還沒探過。
    ///
    /// ★ 它存在**目標**上而不是全域，是為了讓每條 `reverse_proxy` 各自的
    /// `health_interval` 真的算數。⚠ 與 `dns_refresh` 那邊的簡化**有意不同**：
    /// 那邊「刷得更勤」只是多幾次 `getaddrinfo`，而這邊刷得更勤是**打在別人服務上的流量**——
    /// 配了 `30s` 的後端被每 2 秒探一次，是一個使用者沒有要求的行為改變。
    last_probe: std::sync::Mutex<Option<std::time::Instant>>,
    /// `round_robin` 的遊標。
    cursor: AtomicUsize,
    /// `random` 的種子（處理程序啟動時定一次）。
    seed: u64,
}

/// 一組上游的主動健康檢查參數（`health_*`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthPolicy {
    /// 探測請求的路徑。**它就是 `health_uri` 的值**。
    pub uri: String,
    pub interval: std::time::Duration,
    pub timeout: std::time::Duration,
    /// 認為「健康」的狀態碼。
    pub status: StatusPattern,
}

/// `health_status` 的兩種寫法：一個具體狀態碼（`200`）或一族（`2xx`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPattern {
    Exact(u16),
    /// 首位數字，`2` = `2xx`。
    Family(u16),
}

impl StatusPattern {
    /// 解析 `health_status` 的值。設定層已經檢核過形狀（`ArgType::StatusPattern`），
    /// 這裡再解一次是因為**結構化層是公開入口**（G11）——
    /// 一份手寫的 JSON 可以帶任何字串進來。
    pub fn parse(s: &str) -> Option<StatusPattern> {
        let b = s.as_bytes();
        if b.len() != 3 {
            return None;
        }
        if b[1] == b'x' && b[2] == b'x' {
            let d = (b[0] as char).to_digit(10)?;
            if !(1..=5).contains(&d) {
                return None;
            }
            return Some(StatusPattern::Family(d as u16));
        }
        let n: u16 = s.parse().ok()?;
        if (100..=599).contains(&n) {
            Some(StatusPattern::Exact(n))
        } else {
            None
        }
    }

    pub fn matches(&self, code: u16) -> bool {
        match self {
            StatusPattern::Exact(n) => *n == code,
            StatusPattern::Family(d) => code / 100 == *d,
        }
    }
}

/// 這一組上游現在該探了嗎。
///
/// ★ **純函式，時間從參數進來**（本儲存庫的老規矩）：不這樣的話，
/// 「配了 30s 的目標不會被每 2 秒探一次」這條判據就只能靠真的等 30 秒來測。
pub fn probe_due(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    interval: std::time::Duration,
) -> bool {
    match last {
        // 還沒探過 —— 立刻探。⚠ 不要「等一個週期再開始」：
        //   那會讓 `health_interval 30s` 的目標在啟動後半分鐘內完全沒有保護，
        //   而那恰好是剛換代、最可能有上游沒起來的時候。
        None => true,
        Some(t) => now.duration_since(t) >= interval,
    }
}

/// 在**累積權重**上落一個點：`pos` 先對總權重取模，再看它掉進第幾段。
///
/// ★ ★ **這是裁決 R5 的全部內容**：加權輪詢取「累積權重 + 遊標」，
/// ⛔ **不取平滑加權（smooth WRR）**。⚠ 代價寫在明處：3:1 的落點是 `a a a b`
/// 而不是 `a a b a`（**連發**）。
///
/// ⛔ ⛔ **不要相信「換成平滑加權只是換這一個函式體」** —— 這句話在這裡寫過一次，
/// **實測不成立**，它現在被換成了實情：
/// 平滑加權**本質有狀態**（要每上游可變狀態 + 臨界區），而本函式今天是純函式；
/// 且它的四個呼叫點裡**有兩個傳進來的是雜湊/隨機值而不是序號**
/// （`ip_hash` 與 `random`），平滑加權對那兩個**根本沒有意義**。
/// ⇒ 真要換，第一件事是**決定它只接在 `round_robin` 上**，第二件事是拍板
/// 「請求熱路徑上能不能加一把鎖」（今天 `ProxyTarget::cursor` 是無鎖 `fetch_add`），
/// 而按 `PLAN.md` §8，那一批**要帶著可重現的端到端測量開工**。
/// ★ ★ ★ 記住這條的形狀：**一句寫在註解裡的代價估計沒有任何門守著** ——
/// 它錯了不會紅，而下一個決定會建立在它上面。
///
/// ★ ★ ★ **等權迴歸**：`weights` 全是 1 時，總權重就是 `weights.len()`，
/// 每一段寬度都是 1 ⇒ 回傳值**逐字等於 `pos % weights.len()`**，
/// 也就是這一批之前四種策略各自那一行的原樣。這條恆等式由
/// `tests/routing.rs::等权时_weighted_slot_就是取模` 窮舉釘住 ——
/// 它是「這一批沒有順手改掉已經在生產上的行為」唯一可測的形式。
///
/// ⚠ ⚠ **`pos` 收 `usize` 不收 `u64`**：呼叫點此前寫的是 `(x as usize) % n`，
/// 收 `u64` 會在 32 位目標上與舊行為分家（`as usize` 會截斷），
/// 而那種分家在 64 位機上一輩子測不出來。
///
/// `weights` 為空時回傳 0（呼叫方保證非空；這裡不 panic 也不除零）。
pub fn weighted_slot(pos: usize, weights: &[u32]) -> usize {
    let total: usize = weights.iter().map(|w| *w as usize).sum();
    if total == 0 {
        // 走不到：可用集非空、且權重下界是 1（[`fulcrum_config::model::MIN_UPSTREAM_WEIGHT`]）。
        // ⚠ 但「走不到」不該由一次除零來證明。
        return 0;
    }
    let mut k = pos % total;
    for (i, w) in weights.iter().enumerate() {
        let w = *w as usize;
        if k < w {
            return i;
        }
        k -= w;
    }
    // 同樣走不到（`k < total`）。取最後一格而不是 panic。
    weights.len() - 1
}

/// `least_conn` 的比較：`a` 比 `b` **更空**嗎（`inflight / weight` 更小）。
///
/// ★ ★ **交叉相乘，⛔ 不許有浮點**：`a.inflight * b.weight < b.inflight * a.weight`。
/// 浮點在這裡不是「精度差一點」而是**判反**——
/// `tests/routing.rs::least_conn_的比较不许走浮点` 拿一對 `f64` 分不開、
/// 而整數分得開的數釘住這一點。
/// ⚠ 提升到 `u128`：`inflight` 是 `usize`（可到 2⁶⁴），乘 65535 會溢出 `u64`，
/// 而溢出在 release 下是**靜默迴繞**，表現為「某個上游突然永遠被選中」。
///
/// ★ **嚴格小於**：等值時**不換**，於是平票取**下標最小**的那個 —— 與這一批之前逐字相同。
pub fn less_loaded(a_inflight: usize, a_weight: u32, b_inflight: usize, b_weight: u32) -> bool {
    (a_inflight as u128) * (b_weight as u128) < (b_inflight as u128) * (a_weight as u128)
}

/// `ip_hash` 的取數：把用戶端 IP 雜湊成一個 64 位數。
///
/// ★ **純函式、公開**（同 [`probe_due`] 的老規矩）：等權迴歸護欄要能把**今天的規則**
/// （`hash % n`）逐字寫在判據裡，而那要求「取數」與「落點」分得開。
/// 判據自己再實作一遍雜湊是不行的——那就是第二份實作。
pub fn ip_hash_source(ip: IpAddr) -> u64 {
    let mut h = RandomStateless::default();
    ip.hash(&mut h);
    h.finish()
}

impl ProxyTarget {
    /// `random` 的取數：由這條 `reverse_proxy` 的種子與一個遊標值算一個 64 位數。
    ///
    /// ★ 理由同 [`ip_hash_source`]，只是種子在目標身上（建置期取一次）⇒ 它是個方法。
    /// ⚠ 種子**每處理程序隨機**，所以 `random` 的落點沒有跨處理程序字面值可釘；
    /// 等權迴歸護欄因此只能寫成「同一個處理程序裡，落點等於 `本函数(c) % n`」。
    pub fn random_source(&self, cursor: u64) -> u64 {
        // xorshift，種子在建置期取一次。不引 `rand`：這裡要的是「大致均勻」，
        // 不是密碼學隨機，而多一條相依要按 G29 的整套流程養。
        let mut x = self.seed ^ cursor.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 33;
        x
    }

    /// 選一個上游。回傳 `None` = **一個可用的都沒有**，呼叫方回 502。
    ///
    /// 篩子有四條，見 [`pick_index_by`](Self::pick_index_by)：位址解析得出來 ·
    /// 主動健康檢查沒判死（[`HealthPolicy`]）· 維運沒在覆蓋層摘掉 ·
    /// **被動熔斷沒摘掉**（[`PassivePolicy`]，G136）。
    ///
    /// ⚠ ⚠ 第四條讓本函式**不再是純函式**：冷卻期滿時它會搶一次半開探針資格
    /// （見 [`Upstream::passive_ok`]）。⛔ 別把它當成一個可以隨便多調幾次的查詢。
    pub fn pick(&self, req: &RequestCtx<'_>) -> Option<&Upstream> {
        self.pick_by(req.remote_ip)
    }

    /// [`pick`](Self::pick) 的核心，**不要求有一個 HTTP 請求**。
    ///
    /// L4 那條路上沒有 [`RequestCtx`]，但要的是同一個篩子與同一套 `lb_policy`。
    /// ⚠ 不讓 L4 自己寫一個小輪詢：那會是**第二份**「怎麼挑上游」的實作，
    /// 兩份一開始總是一致的，在**下一次改動**時分家（比如給 `pick` 加 `passive_fail` 那天）。
    /// ⇒ 讓分家在結構上做不到，比讓兩份互相釘著可靠。
    pub fn pick_by(&self, client_ip: Option<IpAddr>) -> Option<&Upstream> {
        self.pick_index_by(client_ip)
            .and_then(|i| self.upstreams.get(i))
    }

    /// 同 [`pick_by`](Self::pick_by)，但回傳**下標**。
    ///
    /// ★ L4 那條路要它：一條 TCP 連接在**建連之前**換上游是安全的（一個位元組都還沒走），
    /// 於是它從這個下標開始、按上游列表順序往後試。⚠ HTTP 那邊**有意不這麼做** ——
    /// 那裡「連不上就換一個上游」會滑向重試語義（請求體可能已經發出去了），
    /// 而這兩件事的邊界必須畫在明處，不能靠「反正差不多」。
    pub fn pick_index_by(&self, client_ip: Option<IpAddr>) -> Option<usize> {
        // ★ ★ **先篩掉用不了的，再按策略在剩下的裡面挑。**
        //   ⚠ 順序反過來（先挑再看能不能用）會讓一個用不了的上游把請求吞掉——
        //   而它在輪詢裡佔著一格，症狀是「N 個上游裡每 N 個請求壞一個」。
        //
        //   四條：① 網域解析得出來（批 10）；② 健康檢查沒判死（批 11）；
        //        ③ ★ 維運沒在臨時覆蓋層裡把它摘掉（**批 N 任務 3**，G18 / 裁決 R7）；
        //        ④ ★ ★ 被動熔斷沒把它摘掉（**G136**）。
        //   ★ 沒配 `health_uri` 時 `is_healthy()` 恆 true，所以第二條對它是空操作；
        //     沒設過覆蓋時 `is_disabled()` 恆 false，第三條同理；
        //     沒配 `passive_fail` 時 `passive_ok()` 恆 true，第四條同理。
        //   ⚠ ⚠ **第四條有副作用，而前三條沒有**：冷卻期滿時它會搶一次半開探針資格
        //     （見 `Upstream::passive_ok`）⇒ **本函式因此不是純函式**。
        //     ★ 它放在**最後**：`&&` 短路 ⇒ 一個解析不出位址、或已被主動檢查判死、
        //     或被維運摘掉的上游，不會白白消耗掉自己的探針資格。
        //   ⚠ ⚠ 第三條**必須在這裡**、與前兩條同一個篩子上：`disable` 要讓那個上游
        //     **連它的權重一起出局**（下面那段註解說的就是這件事）。掛在別處的話，
        //     「按 6:2:2 分而那個 6 被摘了」會讓 60% 的請求落空。
        let eligible: Vec<usize> = self
            .upstreams
            .iter()
            .enumerate()
            .filter(|(_, u)| {
                !u.dial_candidates().is_empty()
                    && u.is_healthy()
                    && !u.is_disabled()
                    && u.passive_ok(self.passive.as_ref())
            })
            .map(|(i, _)| i)
            .collect();
        // ⚠ 一個都不剩就回傳 None —— 呼叫方會回 `defaults.all_upstreams_down`（502）。
        //   ★ 這比「硬著頭皮連一個解析不了的位址」好：後者在改之前是**每請求一次 panic**。
        let n = eligible.len();
        if n == 0 {
            return None;
        }
        // ★ ★ **權重只在可用集上算**（裁決 R4/R5）：被摘掉的上游連同它的權重一起出局。
        //   ⚠ 順序反過來（先按全表的總權重挑、再看挑中的能不能用）會讓「按 3:1 分、
        //   而那個 3 沒在跑」變成 **3/4 的請求落空**——那正是「先篩再挑」這個順序的全部意義。
        //   下標與 `eligible` 一一對應，`weighted_slot` 回傳的是**可用集裡的第幾個**。
        let weights = || -> Vec<u32> {
            eligible
                .iter()
                .map(|i| self.upstreams[*i].weight())
                .collect()
        };
        let slot = match self.policy {
            LbPolicy::RoundRobin => {
                weighted_slot(self.cursor.fetch_add(1, Ordering::Relaxed), &weights())
            }
            LbPolicy::LeastConn => {
                // ★ 比的是 `inflight / weight`（[`less_loaded`]，整數交叉相乘）。
                //   ★ 平票時取**下標最小**的那個，不取「第一個碰到的」——
                //   後者相依迭代順序，讀起來像一樣，但一旦並行遍歷就不確定了。
                //   ⚠ 全部權重為 1 時它退化成「比 `inflight`」，與這一批之前逐字相同。
                let mut best = 0usize;
                let mut best_load: Option<(usize, u32)> = None;
                for (slot, i) in eligible.iter().enumerate() {
                    let u = &self.upstreams[*i];
                    let (v, w) = (u.inflight(), u.weight());
                    let better = match best_load {
                        None => true,
                        Some((bv, bw)) => less_loaded(v, w, bv, bw),
                    };
                    if better {
                        best_load = Some((v, w));
                        best = slot;
                    }
                }
                best
            }
            LbPolicy::IpHash => {
                // 取不到用戶端 IP 時退回 round_robin：hash 一個恆定值會把所有
                // 匿名請求釘在同一個上游上。
                //
                // ⚠ **一個上游掉出可用集時，雜湊映射會整體錯位**（同 nginx）。
                //   這是 `ip_hash` 與「可用性篩選」組合的固有性質，不是缺陷——
                //   但要寫下來：工作階段粘性在上游變動時**不保證保持**。
                // ⚠ ⚠ **改權重是同一個性質**：總權重變了，落點整體錯位（裁決 R4 明寫代價）。
                match client_ip {
                    None => weighted_slot(self.cursor.fetch_add(1, Ordering::Relaxed), &weights()),
                    Some(ip) => weighted_slot(ip_hash_source(ip) as usize, &weights()),
                }
            }
            LbPolicy::Random => {
                let c = self.cursor.fetch_add(1, Ordering::Relaxed) as u64;
                weighted_slot(self.random_source(c) as usize, &weights())
            }
        };
        Some(eligible[slot])
    }

    /// 現在該探這一組上游了嗎。**該探就順手把時刻記下**（回傳 `true` 時）。
    ///
    /// ⚠ 「判斷」與「記錄」合成一個動作是有意的：分開寫的話，
    /// 呼叫方忘了記時刻就變成**每一輪都探**——而那不會有任何回報錯誤，
    /// 只會讓配了 `30s` 的後端被按打點節奏打，**症狀在被探的那臺機器上，不在這裡**。
    pub fn take_probe_slot(&self, now: std::time::Instant) -> bool {
        let Some(h) = &self.health else {
            return false;
        };
        let mut g = match self.last_probe.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if probe_due(*g, now, h.interval) {
            *g = Some(now);
            true
        } else {
            false
        }
    }
}

/// 一個穩定的、無隨機化的 hasher —— `ip_hash` 必須**跨處理程序穩定**，
/// 否則同一個用戶端在升級換代之後會被分到另一個上游，粘性就沒了。
///
/// ⚠ 用 `std` 的 `RandomState` 會每處理程序換一個 key，正好破壞這一點。
#[derive(Default)]
struct RandomStateless(u64);

impl Hasher for RandomStateless {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        // FNV-1a：短小、穩定、夠均勻。
        let mut h = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        self.0 = h;
    }
}

/// 一條頭操作的執行環境形態。
#[derive(Debug, Clone)]
pub struct HeaderOpRt {
    /// `set` / `add` / `remove`
    pub op: String,
    pub name: String,
    pub value: Option<Template>,
}

impl HeaderOpRt {
    fn build(o: &HeaderOp) -> HeaderOpRt {
        HeaderOpRt {
            op: o.op.clone(),
            name: o.name.clone(),
            value: o.value.as_deref().map(Template::parse),
        }
    }
}

#[derive(Debug)]
enum MatcherKey {
    Named(String),
    Path(String),
}

#[derive(Debug)]
struct StepRt {
    order: u16,
    matcher: Option<MatcherKey>,
    body: BodyRt,
}

#[derive(Debug)]
enum BodyRt {
    /// 預留：語法認得，不產生行為。
    Tracing,
    Header(Vec<HeaderOpRt>),
    Rewrite(Template),
    Encode(Vec<String>),
    /// 互斥容器。
    Handle(Vec<ArmRt>),
    /// 保序容器：`steps` 的**陣列順序就是執行順序**。
    Route(Vec<StepRt>),
    Redir {
        to: Template,
        code: u16,
    },
    Respond {
        status: u16,
        body: Option<Template>,
    },
    /// Prometheus 抓取端點（**M2 批 M**，G116）。**沒有任何執行環境狀態。**
    ///
    /// ★ 指標註冊表是**處理程序級**的（在 `fulcrum-server` 那一側），不掛在執行環境圖上 ——
    /// ⚠ 掛上去的話，一次 `POST /load` 就會把計數器整個換掉，
    /// 而 counter 歸零在抓取端讀起來與「處理程序重新啟動過」一模一樣。
    Metrics,
    Proxy(ProxyTarget),
    /// 自研靜態檔案服務（M2 批 F）。
    FileServer(FileServerRt),
    /// 自研 HTTP 快取（M2 批 G）。
    ///
    /// ⚠ ⚠ 它是**中介軟體**不是終結類 —— 與 `file_server` 架構上不同。
    /// 中介軟體不設 `outcome`，它只在 [`Routed`] 上記一筆「這條鏈要快取」，
    /// 由資料面在拿到終結類結果時決定怎麼裹。★ 這與 `encode` / `header`
    /// 記狀態是同一條路子，不是新發明的機制。
    Cache(CacheRt),
}

/// `cache` 的執行環境形態（M2 批 G）。
///
/// ★ 與 [`FileServerRt`] 一樣，這裡存的每一樣都是**已經算完的最終值** ——
/// 預設值在構圖時就並進來，請求路徑上不再算第二遍，裝載紀錄打的也是這一份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRt {
    /// **備援**新鮮度（G96）。`None` = 上游沒給新鮮度就不快取。
    /// ⚠ 它**不覆蓋**上游說的 `max-age`/`s-maxage`/`Expires`。
    pub ttl_ms: Option<u64>,
    /// 單條目大小上限。
    pub max_size_bytes: u64,
    /// 整個快取的容量上限。★ 磁碟後端時它是**磁碟預算**，記憶體後端時是**記憶體預算**
    /// —— 同一條指令、兩種介質，裝載紀錄會說清這一份設定裡它是哪一種。
    pub capacity_bytes: u64,
    /// 磁碟後端的根目錄（M2 批 H，G83/G84）。`None` = 記憶體後端。
    ///
    /// ⚠ ⚠ 它是**處理程序級**的一件事，卻寫在**站台級**的指令裡 —— 設定層為此
    /// 有一條跨塊一致性檢查（`FUL-DSL-0035`）。★ 這裡仍然逐站台存著，
    /// 因為裝載紀錄要能說出「這個站台寫的是哪一個」，而不是隻說一個彙總值。
    pub disk_dir: Option<String>,
}

/// `file_server` 的執行環境形態（M2 批 F）。
///
/// ★ ★ 這裡存的每一樣都是**已經算完的最終值**，請求路徑上不再算第二遍：
/// `index` 空了就在這裡補成 `["index.html"]`，`hide` 在這裡就把預設表並進去。
/// 讓「預設值是什麼」只有一個答案，而那個答案在裝載紀錄裡說得出來。
#[derive(Debug, Clone, PartialEq)]
pub struct FileServerRt {
    /// 絕對路徑（G91，編譯層與這裡各攔一道 —— 結構化設定是公開入口）。
    pub root: String,
    pub browse: bool,
    /// 目錄命中時按**這個順序**找索引檔案。**不會為空**。
    pub index: Vec<String>,
    /// G87。`false` 時按 canonicalize 檢核結果仍在 `root` 之內。
    pub follow_symlinks: bool,
    /// G88 的**最終**清單：預設表 ∪ 使用者寫的（`hide_defaults false` 時只有使用者寫的）。
    /// 按**路徑段**匹配，命中回 404。已排序去重，裝載紀錄逐字打它。
    pub hide: Vec<String>,
    /// 預壓縮旁檔案認哪幾種編碼（**M2 批 I**）。★ 已經是**算完的最終值**。
    pub precompressed: Vec<String>,
}

#[derive(Debug)]
struct ArmRt {
    matcher: Option<MatcherKey>,
    steps: Vec<StepRt>,
}

/// 訪問紀錄往哪寫（**M2 批 L 第 ② 步**，G113）。
///
/// ★ ★ 這裡只是**資料** —— 真正打開檔案是資料面的事，本 crate 一行 I/O 都不做
/// （與「執行環境圖是純邏輯、不引用 pingora」同一條紀律）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOutput {
    /// 預設。systemd 下進 journal，別處進終端機。
    Stderr,
    /// 追加寫到這個**絕對路徑**。
    ///
    /// ⚠ 必須絕對，理由與 `cache { disk … }` 那條（G91）逐字相同：相對路徑按處理程序 cwd 解析，
    /// 而 systemd 下 cwd 是 `/`、開發機上是專案目錄 ⇒ **同一份設定指向兩個地方**。
    File(String),
}

/// 訪問紀錄的級別**閾值**。
///
/// ⚠ 它不是「這一行的級別」——每行的級別按狀態碼派生（見 [`LogLevel::name_for`]），
/// 這個值只決定**哪些行會被寫出來**。兩者讀者不同，合成一個就沒法表達「只記錯誤」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// 記全部。
    ///
    /// ⚠ **`debug` 與 `info` 在訪問紀錄上等價**，兩者都落到這裡。
    /// ★ 保留 `debug` 只是因為它本來就在 `log { level … }` 的取值表裡（G62）——
    /// **多一個取值不多一種行為**，而把它判成錯誤會讓既有設定裝不上。
    All,
    /// 只記 4xx 與 5xx。
    Warn,
    /// 只記 5xx。
    Error,
}

impl LogLevel {
    /// 認一個 `level` 取值。`None` = 認不出來。
    pub fn parse(s: &str) -> Option<LogLevel> {
        match s {
            "debug" | "info" => Some(LogLevel::All),
            "warn" => Some(LogLevel::Warn),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }

    /// 這個狀態碼該不該被記下來。
    pub fn records(self, status: u16) -> bool {
        match self {
            LogLevel::All => true,
            LogLevel::Warn => status >= 400,
            LogLevel::Error => status >= 500,
        }
    }

    /// **這一行**的級別名 —— 按狀態碼派生，與閾值無關。
    ///
    /// ★ 它是關聯函式而不是方法，正是為了讓「它與閾值無關」在類型上看得出來。
    pub fn name_for(status: u16) -> &'static str {
        if status >= 500 {
            "error"
        } else if status >= 400 {
            "warn"
        } else {
            "info"
        }
    }
}

/// 白名單裡的一個頭，**已經算成最終形態**（**M2 批 L 第 ③ 步**）。
///
/// ★ ★ 兩個欄位是**兩件事**，而把它們分開正是為了讓「查哪個頭」與
/// 「紀錄裡那一格叫什麼」不會在某一天各自漂走：
/// 前者是 HTTP 的事（大小寫不敏感、帶連字元），後者是紀錄契約的事
/// （小寫、下劃線、帶前綴）。⚠ 合成一個的話，規範化規則會被複製到用它的每一處。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPick {
    /// 拿它去 `HeaderMap` 裡查 —— **小寫**。
    ///
    /// ★ 查找本來就大小寫不敏感，存小寫是為了讓**去重**在這一層就做得掉：
    /// 使用者寫了 `User-Agent` 又寫了 `user-agent` 時，紀錄裡不能出現兩個同名鍵
    /// （那樣的 JSON 解析器只會留下一個，而是哪一個沒有定義）。
    pub lookup: String,
    /// 紀錄裡那一格的鍵：`req_hdr_user_agent` / `resp_hdr_content_type`。
    pub key: String,
}

/// 一個站台的訪問紀錄設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRt {
    pub output: LogOutput,
    pub level: LogLevel,
    /// 請求頭白名單（**M2 批 L 第 ③ 步**）。★ 空 = **一個頭都不記**，那是預設。
    pub req_headers: Vec<HeaderPick>,
    /// 回應頭白名單。同上。
    pub resp_headers: Vec<HeaderPick>,
}

impl LogRt {
    /// 把 `log { … }` 那個塊翻成執行環境設定；認不出來的寫法進 `errors`。
    ///
    /// ⚠ `output` 到這裡是**已經用空格拼好**的一串（`sub_block` 那麼給的）——
    /// `stderr` 或 `file /var/log/x.json`。
    /// ★ 語法層面的檢查在 `fulcrum-config` 裡（那裡報的是帶位置的診斷）；
    /// 這裡**再判一次**是因為執行環境圖是公開入口（G11），
    /// **它不能假設喂進來的結構化設定一定經過了 DSL 那條路**。
    fn build(
        at: &str,
        c: &fulcrum_config::model::LogConfig,
        errors: &mut Vec<BuildError>,
    ) -> Option<LogRt> {
        let output = match c.output.as_deref() {
            None | Some("stderr") => LogOutput::Stderr,
            Some(s) => {
                let rest = s.strip_prefix("file ").map(str::trim).unwrap_or("");
                if s == "file" || (s.starts_with("file ") && rest.is_empty()) {
                    errors.push(BuildError::new(
                        at,
                        "`log { output file … }` 后面要跟一个路径",
                    ));
                    return None;
                }
                if !s.starts_with("file ") {
                    errors.push(BuildError::new(
                        at,
                        format!(
                            "`log {{ output {s} }}` 认不出来——只能是 `stderr` 或 `file <绝对路径>`"
                        ),
                    ));
                    return None;
                }
                if !rest.starts_with('/') {
                    errors.push(BuildError::new(
                        at,
                        format!(
                            "`log {{ output file {rest} }}` 要绝对路径——相对路径按进程 cwd 解析，\
                             而 systemd 下 cwd 是 `/`、开发机上是项目目录，同一份配置会指向两个地方"
                        ),
                    ));
                    return None;
                }
                LogOutput::File(rest.to_string())
            }
        };
        let level = match c.level.as_deref() {
            None => LogLevel::All,
            Some(s) => match LogLevel::parse(s) {
                Some(l) => l,
                None => {
                    errors.push(BuildError::new(
                        at,
                        format!("`log {{ level {s} }}` 认不出来——只能是 `debug` / `info` / `warn` / `error`"),
                    ));
                    return None;
                }
            },
        };
        let req_headers = Self::picks(at, &c.headers, "headers", "req_hdr_", errors)?;
        let resp_headers = Self::picks(at, &c.resp_headers, "resp_headers", "resp_hdr_", errors)?;
        Some(LogRt {
            output,
            level,
            req_headers,
            resp_headers,
        })
    }

    /// 把一份白名單算成最終形態：**規範化 → 查敏感頭 → 去重**。
    ///
    /// ⚠ ⚠ **這一層也要判敏感頭**：編譯期那道（`FUL-DSL-0036`）只在 DSL 那條路上，
    /// 而執行環境圖是**公開入口**（G11）—— `POST /load` 收的 JSON 不必經過 `fulcrum compile`。
    /// ★ 它比 `output` 那三條更要緊：那些寫錯了設定裝不上（吵鬧），
    /// **這一條寫錯，憑證靜靜地進了紀錄**。
    fn picks(
        at: &str,
        names: &[String],
        which: &str,
        prefix: &str,
        errors: &mut Vec<BuildError>,
    ) -> Option<Vec<HeaderPick>> {
        let mut out: Vec<HeaderPick> = Vec::new();
        let mut bad = false;
        for raw in names {
            let lookup = raw.to_ascii_lowercase();
            if fulcrum_config::directive::SENSITIVE_HEADERS.contains(&lookup.as_str()) {
                errors.push(BuildError::new(
                    at,
                    format!(
                        "`log {{ {which} … }}` 里不许写 `{raw}`——它带的是凭据，不是可观测信息。\
                         不许记的四个：Authorization / Cookie / Set-Cookie / Proxy-Authorization"
                    ),
                ));
                bad = true;
                continue;
            }
            // ⚠ 不是合法 HTTP 頭名的東西**永遠查不到**，而「查不到」在這份契約裡
            //   與「這條請求上沒有這個頭」長得一模一樣 ⇒ 一個拼錯的名字會靜靜地
            //   什麼都不記。★ 所以它是錯誤，不是警告。
            if !is_header_token(&lookup) {
                errors.push(BuildError::new(
                    at,
                    format!(
                        "`log {{ {which} {raw} }}` 不是一个合法的 HTTP 头名——\
                         它永远查不到任何东西，而那与「这条请求上没有这个头」在日志里长得一样"
                    ),
                ));
                bad = true;
                continue;
            }
            // ★ 去重按 `lookup` 算：`User-Agent` 與 `user-agent` 是同一個頭，
            //   而兩條同名的紀錄鍵會讓那一行的 JSON 有兩個一樣的 key。
            if out.iter().any(|p| p.lookup == lookup) {
                continue;
            }
            let key = format!("{prefix}{}", lookup.replace('-', "_"));
            out.push(HeaderPick { lookup, key });
        }
        if bad { None } else { Some(out) }
    }
}

/// RFC 9110 §5.1 的 `field-name` = RFC 9110 `token`。
///
/// ★ 自己寫而不是拉一個 `http::HeaderName`：本 crate 是**純邏輯、不碰網路**，
/// 而這條規則是一張字元表，抄它比多一條相依便宜。
fn is_header_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// 一個站台的執行環境形態。
#[derive(Debug)]
pub struct SiteRt {
    /// 給紀錄用的名字（第一個位址的原文）。
    pub name: String,
    /// 本站台的全部主機名（含 `*.` 形式），**不含**不帶主機名的 `:port` 那種。
    /// ★ TLS 層要用它決定「這張證書該裝在哪些 SNI 上」。
    pub hostnames: Vec<String>,
    /// 每條位址的**標籤字面值**（G121 · `fulcrum_requests_total{site=...}` 取數用它）：
    /// `host` 非空取其小寫（通配保留 `*.` 前綴，與上面 `hostnames` 同一口徑），
    /// `host` 為空（`:8080` 這種不帶主機名的備援位址）取 `format!(":{port}")`。
    /// 下標與結構化設定裡的 `s.addresses` 一一對應，[`Runtime::resolve_site`]
    /// 回傳值的第三項就是這份下標。
    ///
    /// ⚠ ⚠ **它與 [`SiteRt::name`] 是兩件不同的事，長得像是巧合**：`name` 取
    /// **第一條**位址的原文（帶 scheme 與連接埠，訪問紀錄的 `site` 欄位用它），
    /// 這裡取的是**命中的那一條**、只留主機名 —— G121 明文要求不能用第一個位址，
    /// 否則同一塊裡的兩條位址會被混成一格，且改位址書寫順序會讓時序斷裂。
    /// ★ 用 `Arc<str>` 是為了讓每條請求帶走它時只是一次引用計數
    /// （與下面 [`SiteRt::log`] 同一條理由，這裡連 `String` 的分配都省了）。
    pub addresses: Vec<std::sync::Arc<str>>,
    pub tls: TlsConfig,
    /// 本站台的訪問紀錄設定（**M2 批 L 第 ② 步**）。`None` = 這個站台不記訪問紀錄。
    ///
    /// ⚠ ⚠ 它是**站台級**的，而這帶來一個已登記的缺口：`outcome=no_site_match`
    /// 的請求（421，G63）按定義不屬於任何站台 ⇒ **它記不進訪問紀錄**。
    /// 那是 **D26**（✅ 已由 **G118** 結案 —— 給 `no_site_match` 一個計數器），不是遺漏。
    /// ★ 用 `Arc` 是為了讓每條請求把它帶走時只是一次引用計數 ——
    /// 裡面有一個 `String`（檔案路徑），逐請求克隆它是白花的。
    pub log: Option<std::sync::Arc<LogRt>>,
    matchers: BTreeMap<String, CompiledMatcher>,
    chain: Vec<StepRt>,
    error_handler: Vec<StepRt>,
}

/// 整棵執行環境圖。**不可變**——換設定就整棵換掉（G8 的全量原子裝載）。
#[derive(Debug)]
pub struct Runtime {
    sites: Vec<SiteRt>,
    /// `port → host（小写）→ (站点下标, 命中的地址在该站点 `addresses` 里的下标)`。
    /// ⚠ 後一項是 G121 的全部落點 —— 見 [`SiteRt::addresses`]。
    /// ★ 按連接埠分兩層而不是 `(String, u16)` 一個鍵：查找時直接拿 `&str` 查，
    ///   ⛔ 不必為了拼鍵每請求克隆一次 Host（判據 `tests/route_alloc.rs`）。
    exact: BTreeMap<u16, BTreeMap<String, (usize, usize)>>,
    /// `(".example.com", port, 站点下标, 命中的地址下标)`，**按後綴長度降序**。
    wildcard: Vec<(String, u16, usize, usize)>,
    /// `port → (站点下标, 命中的地址下标)`，來自 `:8080` 這種不帶主機名的位址。
    catch_all: BTreeMap<u16, (usize, usize)>,
    pub defaults: Defaults,
    pub l4: Option<L4Config>,
    /// L4 面建好的監聽器（M2 批 A：TCP；批 B：UDP）。
    /// ★ 兩種協定**都在這裡**，由 `proto` 區分 —— 資料面按它決定起哪種服務。
    pub l4_listeners: Vec<L4ListenerRt>,
    /// 要監聽的 `(端口, 是否需要 TLS)`。
    pub listen_ports: Vec<(u16, bool)>,
    /// ★ ★ **HTTP 面的 PROXY protocol 信任清單**（**M2 批 D**，全域塊的
    /// `proxy_protocol_from`）—— 管這份設定裡 HTTP 面的**全部**監聽連接埠。
    ///
    /// ⚠ ⚠ 它與 `L4ListenerRt::proxy_protocol_from` **有意是兩份**，
    /// 不是同一份的兩個視圖：信任 A 對 :443 發 PROXY 頭 ≠ 信任 A 對 :5432 也這麼幹。
    /// ★ 而 HTTP 面這一份必須是**全域**的，因為它是**連接級**的判斷 ——
    /// 一條連接上還沒有 Host，還不知道會落到哪個站台。
    pub proxy_protocol_from: Vec<Cidr>,
    /// 裝載時要說的 warning（現在只有自環那一條，M2 批 G）。
    /// ★ 與 [`Runtime::unwired_in_use`] 同一個形狀：**本 crate 回傳話，資料面去說**。
    pub self_loop_warnings: Vec<String>,
}

/// L4 面的四層協定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4Proto {
    Tcp,
    /// ✅ **M2 批 B串接**：自建 `Service` + 工作階段表（空閒回收 + 並行上限）。
    /// ⚠ 它與 TCP 有三處**語義上的**差別，都寫在 `fulcrum_server::l4` 的 UDP 那一節裡，
    /// 其中最要緊的一條是：**停機信號一到就必須停止 `recv_from`**，
    /// 否則老一代會與新一代搶同一個 socket 上的資料報。
    Udp,
}

impl L4Proto {
    pub fn as_str(self) -> &'static str {
        match self {
            L4Proto::Tcp => "tcp",
            L4Proto::Udp => "udp",
        }
    }
}

/// L4 面的一個監聽器（M2 批 A）。
///
/// ★ 它**不經過站台索引**：TCP 連接上沒有任何七層資訊可供路由，
/// L4 的全部語義就是「這個連接埠進來的位元組，原樣送到那組上游之一」⇒ 編譯期定死。
///
/// ⚠ **連接埠在啟動時綁定**，`POST /load` 換不了。管理面為此把 L4 連接埠納入
/// 「連接埠集變了就 409」—— 少了那一格，改了 `l4` 的設定會**裝載成功而毫無效果**。
#[derive(Debug)]
pub struct L4ListenerRt {
    pub proto: L4Proto,
    /// 設定裡寫的那一串，**原樣保留**（診斷與紀錄要指得回去）。
    pub listen: String,
    /// 監聽位址的主機部分。`None` = 設定寫的是 `:3306`，
    /// 綁哪個位址由 `serve --bind-host` 決定（與 HTTP 那邊同一條口徑）。
    pub listen_host: Option<String>,
    pub listen_port: u16,
    /// **備援**目標：沒有任何規則命中時用它。`None` = 沒配備援 ⇒ 關掉連接。
    ///
    /// ★ ★ **與 `reverse_proxy` 用的是同一個類型**，於是「怎麼挑上游」只有一份實作
    /// （[`ProxyTarget::pick_by`]），網域上游也自動走同一套解析與重解析機械。
    /// ⚠ 其中 `header_up` / `header_down` / `tls` / `health` 對 L4 **沒有意義**，
    /// 一律留空：L4 不看也不改位元組，`health_uri` 是一個 HTTP 概念。
    pub target: Option<ProxyTarget>,
    /// SNI / ALPN 分流規則（**M2 批 C**），**按書寫順序**匹配，第一個命中即用。
    ///
    /// ⚠ 非空時資料面要**先看一眼 ClientHello** 才能決定連誰 ——
    /// 那是一段額外的讀 + 一次超時預算，所以**空的時候一步都不多走**（批 A 的形狀不變）。
    pub rules: Vec<L4RuleRt>,
    /// **收**：信任這些來源發來的 PROXY 頭（**M2 批 D**）。
    ///
    /// ★ 空 = **誰都不信**，而這不只是「預設關」：資料面在空的時候
    /// **一個位元組都不多讀**，與批 A 的形狀完全一樣。
    pub proxy_protocol_from: Vec<Cidr>,
    /// **發**：給上游發一個 PROXY 頭。`None` = 不發（**M2 批 D**）。
    pub proxy_protocol: Option<crate::proxyproto::Version>,
}

impl Runtime {
    /// HTTP 面：這個來源發來的 PROXY 頭可信嗎？
    ///
    /// ★ ★ 它是**唯一**的判斷入口 —— 資料面（含 `pingora-core` 那道接縫）
    /// 不許自己去比網段。⚠ 清單為空時恆 `false`。
    pub fn trusts_proxy_protocol(&self, peer: std::net::IpAddr) -> bool {
        self.proxy_protocol_from.iter().any(|c| c.contains(peer))
    }
}

impl L4ListenerRt {
    /// 這個來源發來的 PROXY 頭可信嗎？
    ///
    /// ★ ★ 它是**唯一**的判斷入口 —— 資料面不許自己去比網段。
    /// ⚠ 清單為空時恆 `false`：一份空清單**不是**「信任所有人」，
    /// 而這正是本條紀律要防的那件事。
    pub fn trusts_proxy_protocol(&self, peer: std::net::IpAddr) -> bool {
        self.proxy_protocol_from.iter().any(|c| c.contains(peer))
    }
}

/// 一條建好的 L4 分流規則。
#[derive(Debug)]
pub struct L4RuleRt {
    pub kind: L4MatchKind,
    /// 要匹配的值。★ SNI 的通配符走 [`fulcrum_config::host::wildcard_covers`] ——
    /// **與站台索引、證書解析同一份實作**（D18 / G66）。⚠ 另寫一份「差不多的」匹配，
    /// 就是把 D18 那個洞在第三個地方再挖一次。
    pub values: Vec<String>,
    pub target: ProxyTarget,
}

/// 分流按什麼匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4MatchKind {
    /// TLS ClientHello 裡的 `server_name`。
    Sni,
    /// TLS ClientHello 裡的 ALPN 清單，**任意一個**命中即算命中。
    Alpn,
}

impl L4MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            L4MatchKind::Sni => "sni",
            L4MatchKind::Alpn => "alpn",
        }
    }
}

impl L4RuleRt {
    /// 這條規則命中了嗎。
    ///
    /// ★ `sni` 與 `alpn` 的匹配語義**有意不同**，兩條都寫下來：
    /// - SNI：精確，或 `*.example.com` **只吃一層**（G66，共用 `wildcard_covers`）。
    ///   ⚠ 大小寫不敏感（DNS 名不區分大小寫），所以兩邊都先轉小寫。
    /// - ALPN：**逐位元組相等**。⚠ 它不是網域，沒有通配的道理；
    ///   而 `h2` 與 `h2c` 是兩個不同的協定標識，前綴匹配會把它們混起來。
    pub fn matches(&self, sni: Option<&str>, alpn: &[Vec<u8>]) -> bool {
        match self.kind {
            L4MatchKind::Sni => {
                let Some(name) = sni else { return false };
                let name = name.to_ascii_lowercase();
                self.values.iter().any(|v| {
                    let v = v.to_ascii_lowercase();
                    match v.strip_prefix('*') {
                        // `*.example.com` ⇒ 後綴是 `.example.com`，且只吃一層
                        Some(suffix) => wildcard_covers(suffix, &name),
                        None => v == name,
                    }
                })
            }
            L4MatchKind::Alpn => self
                .values
                .iter()
                .any(|v| alpn.iter().any(|p| p.as_slice() == v.as_bytes())),
        }
    }
}

/// 站台解析的結果。
#[derive(Debug, PartialEq, Eq)]
pub enum SiteMatch {
    Exact,
    Wildcard,
    CatchAll,
}

/// 一次路由的結論。
pub enum Outcome<'r> {
    Respond {
        status: u16,
        body: Option<&'r Template>,
    },
    Redirect {
        to: &'r Template,
        code: u16,
    },
    Proxy(&'r ProxyTarget),
    /// 自研靜態檔案服務（M2 批 F）。資料面按 [`FileServerRt`] 發檔案。
    FileServer(&'r FileServerRt),
    /// Prometheus 抓取端點（**M2 批 M**，G116）。資料面繪製當前指標並 200 發出去。
    ///
    /// ⚠ 它讓訪問紀錄 `outcome` 那個閉集**多第 8 個值 `metrics`**。
    /// ★ 閉集是靠資料面那個窮盡 `match` 守著的：加這一種就編不過。
    Metrics,
    /// 站台內沒有任何路由匹配 → [`Defaults::no_route_match`]。
    NoRouteMatch,
}

/// 路由結果。
pub struct Routed<'r> {
    pub site: &'r SiteRt,
    pub site_match: SiteMatch,
    /// 這次請求**實際匹配到的那條位址字面值**（G121）。
    ///
    /// ⚠ ⚠ 它**不是** `site.name`：`name` 是「站台的名字 = 第一個位址的原文」
    /// （訪問紀錄的 `site` 欄位用這個），這裡取的是**命中的那一條**——
    /// 同一個站台配多條位址時兩者會給出不同的值，那不是巧合，是 G121 明文
    /// 要求「不能用站台塊的第一個位址」換來的。
    pub site_addr: std::sync::Arc<str>,
    /// 要施加到**回應**上的頭操作，按施加順序。
    pub response_headers: Vec<&'r HeaderOpRt>,
    /// `rewrite` 之後的路徑（含查詢串按原樣保留）。`None` = 沒被改寫。
    pub rewritten_path: Option<String>,
    /// `path_regexp` 的捕獲組，給 `{path.N}` 用。
    pub captures: Vec<String>,
    /// 終結這次請求的那一步在執行順序表裡的序號（G49）。
    /// ★ 它進結果結構是為了讓訪問紀錄能回答「這個請求實際跑到第幾步」——
    /// 內建順序表把書寫順序和執行順序拆開之後，這是唯一能對上的東西。
    pub terminal_order: Option<u16>,
    /// 這條鏈上有沒有 `cache`（M2 批 G）。
    ///
    /// ★ 它是**中介軟體**，所以不佔 `outcome` —— 資料面看到它才走快取那條路。
    /// ⚠ 快取只對 `reverse_proxy` 有意義：`respond` / `redir` 本來就沒有上游可省，
    /// 而 `file_server` 的位元組已經在本機磁碟上、再存一份記憶體是淨虧。
    /// ⇒ 資料面只在 `Outcome::Proxy` 上用它，別處**裝載紀錄會說出來**。
    pub cache: Option<&'r CacheRt>,
    /// 設定裡要求的壓縮編碼。⚠ **M1 這一批不施加**（見 [`UNWIRED`]）——
    /// 帶出來是為了讓「要求了但沒做」這件事可見，而不是悄悄丟掉。
    pub requested_encodings: Vec<String>,
    pub outcome: Outcome<'r>,
}

/// 一份**可以在執行期整體換掉**的執行環境圖（G8 的「全量原子 load」那一側）。
///
/// ★ 不用 `arc-swap`：它不在相依圖裡（+1 個包），而寫入是「一天幾次」量級。
/// `RwLock<Arc<Runtime>>` 的讀路徑只是一次無競爭讀鎖 + 一次 `Arc` 克隆。
///
/// ⚠ ⚠ **一個請求必須自始至終看到同一份設定** ⇒ [`SharedRuntime::current`] 只在請求入口
/// 調一次。每階段各調一次的話，請求中途的一次 load 會讓同一個請求按舊設定路由、
/// 按新設定轉發 —— 那正是 G8 禁止的「部分生效」。
impl Runtime {
    /// 全部自研 `file_server` 步驟，連同它所在站台的名字。
    ///
    /// ★ 唯一用途是裝載紀錄（G88 的可見性）：非空的預設 hide 清單不說出來就是靜默行為。
    /// ⚠ ⚠ 紀錄必須打**執行環境手裡這一份**，不能照設定再算一遍 —— 兩份各算各的時，
    /// 紀錄說的和伺服器做的可以不是一回事。
    /// 哪些連接埠上有反代站台（G163）：其上任何一個站台的執行鏈或錯誤處理器裡（含嵌套的
    /// `handle` / `route`）有 `reverse_proxy`。
    ///
    /// ★ 從路由用的**同三張表**推出來（精確 · 通配 · 連接埠備援）⇒ 與「這個連接埠上的請求可能落到哪些站台」
    /// 逐字同一個口徑，⛔ 不另按站台位址再算一遍（兩份演算法遲早對不上）。
    /// 用處：沒有反代的連接埠上，讀 h1 請求頭時不必保留頭名的原樣大小寫 —— 只有原樣轉發給上游時才用得到它。
    pub fn proxy_ports(&self) -> BTreeSet<u16> {
        let proxied: Vec<bool> = self
            .sites
            .iter()
            .map(|s| steps_have_proxy(&s.chain) || steps_have_proxy(&s.error_handler))
            .collect();
        let mut out = BTreeSet::new();
        for (&port, by_host) in &self.exact {
            if by_host.values().any(|&(i, _)| proxied[i]) {
                out.insert(port);
            }
        }
        for &(_, port, i, _) in &self.wildcard {
            if proxied[i] {
                out.insert(port);
            }
        }
        for (&port, &(i, _)) in &self.catch_all {
            if proxied[i] {
                out.insert(port);
            }
        }
        out
    }

    pub fn file_servers(&self) -> Vec<(&str, &FileServerRt)> {
        fn walk<'a>(
            steps: &'a [StepRt],
            site: &'a str,
            out: &mut Vec<(&'a str, &'a FileServerRt)>,
        ) {
            for s in steps {
                match &s.body {
                    BodyRt::FileServer(fs) => out.push((site, fs)),
                    BodyRt::Handle(arms) => {
                        for a in arms {
                            walk(&a.steps, site, out);
                        }
                    }
                    BodyRt::Route(inner) => walk(inner, site, out),
                    // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                    //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                    //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                    //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                    // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                    //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                    //   —— 要名單就現場 grep 這兩個枚舉名。
                    BodyRt::Tracing
                    | BodyRt::Header(_)
                    | BodyRt::Rewrite(_)
                    | BodyRt::Encode(_)
                    | BodyRt::Redir { .. }
                    | BodyRt::Respond { .. }
                    | BodyRt::Metrics
                    | BodyRt::Proxy(_)
                    | BodyRt::Cache(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        for site in &self.sites {
            walk(&site.chain, &site.name, &mut out);
            walk(&site.error_handler, &site.name, &mut out);
        }
        out
    }

    /// 全部 `encode` 步驟要求的演算法，連同它所在站台的名字（**M2 批 I**）。
    ///
    /// ★ 與 [`Self::cache_settings`] / [`Self::file_servers`] 同一個形狀，
    /// 用途也一樣：**裝載紀錄**。⚠ 而這一條比那兩條更要緊 ——
    /// `encode` 在 `UNWIRED` 裡躺了整整一段，它**剛剛開始真的生效**，
    /// 而一個從舊版本升上來的站台行為就在這一刻變了、設定卻一個字沒改。
    pub fn encodings(&self) -> Vec<(&str, &[String])> {
        fn walk<'a>(steps: &'a [StepRt], site: &'a str, out: &mut Vec<(&'a str, &'a [String])>) {
            for s in steps {
                match &s.body {
                    BodyRt::Encode(list) => out.push((site, list.as_slice())),
                    BodyRt::Handle(arms) => {
                        for a in arms {
                            walk(&a.steps, site, out);
                        }
                    }
                    BodyRt::Route(inner) => walk(inner, site, out),
                    // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                    //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                    //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                    //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                    // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                    //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                    //   —— 要名單就現場 grep 這兩個枚舉名。
                    BodyRt::Tracing
                    | BodyRt::Header(_)
                    | BodyRt::Rewrite(_)
                    | BodyRt::Redir { .. }
                    | BodyRt::Respond { .. }
                    | BodyRt::Metrics
                    | BodyRt::Proxy(_)
                    | BodyRt::FileServer(_)
                    | BodyRt::Cache(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        for site in &self.sites {
            walk(&site.chain, &site.name, &mut out);
            walk(&site.error_handler, &site.name, &mut out);
        }
        out
    }

    /// 全部 `cache` 步驟，連同它所在站台的名字（M2 批 G）。
    ///
    /// ★ 與 [`Self::file_servers`] 同一個形狀，用途也一樣：**裝載紀錄**
    /// （`ttl` 是備援不是覆蓋這件事必須說出來）+ 決定快取後端的容量與目錄。
    pub fn cache_settings(&self) -> Vec<(&str, &CacheRt)> {
        cache_settings_of(&self.sites)
    }

    pub fn all_upstreams(&self) -> Vec<&Upstream> {
        fn walk<'a>(steps: &'a [StepRt], out: &mut Vec<&'a Upstream>) {
            for s in steps {
                match &s.body {
                    BodyRt::Proxy(t) => out.extend(t.upstreams.iter()),
                    BodyRt::Handle(arms) => {
                        for a in arms {
                            walk(&a.steps, out);
                        }
                    }
                    BodyRt::Route(inner) => walk(inner, out),
                    // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                    //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                    //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                    //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                    // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                    //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                    //   —— 要名單就現場 grep 這兩個枚舉名。
                    BodyRt::Tracing
                    | BodyRt::Header(_)
                    | BodyRt::Rewrite(_)
                    | BodyRt::Encode(_)
                    | BodyRt::Redir { .. }
                    | BodyRt::Respond { .. }
                    | BodyRt::Metrics
                    | BodyRt::FileServer(_)
                    | BodyRt::Cache(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        for site in &self.sites {
            walk(&site.chain, &mut out);
            walk(&site.error_handler, &mut out);
        }
        // ⚠ ⚠ **這個函式是 DNS 解析與重解析唯一的輸入，誰不在裡面誰就不存在。**
        //   漏掉一類上游（比如 L4 的）的現場是「那個連接埠連上就斷」，
        //   而設定、紀錄、健康檢查全都正常。
        for l in &self.l4_listeners {
            if let Some(t) = &l.target {
                out.extend(t.upstreams.iter());
            }
            // ⚠ **分流規則裡的上游同樣是上游**（批 C）。少了這一格，一個網域形式的
            //   `sni` 上游永遠解析不出位址 —— 而現場是「那個名字連上就斷」，
            //   備援那條卻一切正常，看起來像 SNI 匹配錯了。
            for r in &l.rules {
                out.extend(r.target.upstreams.iter());
            }
        }
        out
    }

    /// 這份圖裡所有的 `reverse_proxy` 目標。健康檢查按**目標**走
    /// （`health_*` 是每條 `reverse_proxy` 各自配的），不像 DNS 那樣按上游走。
    ///
    /// ⚠ 走法必須與 [`Self::all_upstreams`] 一致（容器裡的也要進來）——
    /// 兩個走法分家的話，一個藏在 `handle` 裡的目標會**永遠不被探測**，
    /// 而它看起來只是「一直健康」。
    pub fn all_proxy_targets(&self) -> Vec<&ProxyTarget> {
        // ⚠ ⚠ **有意不自己再走一遍**（M2 批 N 任務 2.8）：這裡原先是第三份一模一樣的
        //   遞歸遍歷。走法分家的表現是「藏在 `handle` 裡的那條 `reverse_proxy`
        //   在某一張清單裡不存在」，而它看起來只是「一直健康」/「沒有鍵」。
        keyed_proxies_of(&self.sites)
            .into_iter()
            .map(|p| p.target)
            .collect()
    }

    /// 這份圖裡所有的 `reverse_proxy`，**連同覆蓋層鍵的前兩格**
    /// （**M2 批 N 任務 2.8**，裁決 R6 ⇒ G125）。
    ///
    /// ★ 鍵的第三格在 [`KeyedProxy::target`] 的每個 [`Upstream::addr`] 上 ——
    /// 一條 `reverse_proxy` 有幾個上游就有幾個鍵。
    /// ⚠ **L4 的上游不在裡面**：它們沒有站台，按 R6 的鍵尋址不到（見 [`ProxyTarget::id`]）。
    pub fn keyed_proxies(&self) -> Vec<KeyedProxy<'_>> {
        keyed_proxies_of(&self.sites)
    }

    /// 每一格覆蓋層鍵**管著幾條 `reverse_proxy`**（**M2 批 N 任務 2.9**，
    /// 裁決 R6 ③ 第二輪 ⇒ G125）。按 `(站点名, id, 上游地址)` 升序，**去重後**每個鍵一行。
    ///
    /// ★ 這是任務 2.8 那段歧義檢測**改向**之後的產物：從「算出哪些鍵撞了要回報錯誤」
    /// 變成「算出每個鍵管著幾條」。⇒ 見 [`ProxyKeyFanout`]。
    pub fn proxy_key_fanout(&self) -> Vec<ProxyKeyFanout<'_>> {
        proxy_key_fanout_of(&self.sites)
    }
}

/// 一條 `reverse_proxy`，連同它在**覆蓋層鍵**裡佔的那兩格（G125 / 裁決 R6）。
///
/// 鍵是 `(站点名, id, 归一化后的上游地址)`：前兩格在這裡，第三格在
/// `target.upstreams[*].addr` 上。
///
/// ⚠ ⚠ **⛔ 別在別處再拼一次這個鍵。** 任務 3 的覆蓋層與任務 6 的 `/stats`
/// 都從這裡取 —— 兩份「差不多的」拼法遲早在某一格上分家，而分家的現場是
/// 「`disable` 說成功了，那臺機器還在收流量」，沒有任何東西會回報錯誤。
#[derive(Debug, Clone, Copy)]
pub struct KeyedProxy<'a> {
    /// 鍵的**第一格**：站台名 = [`SiteRt::name`]（第一條位址的原文，
    /// 與訪問紀錄的 `site` 欄位逐字同一口徑，裁決 R6 ④）。
    ///
    /// ⚠ ⛔ **不取 G121 那個「命中的位址字面值」**（[`SiteRt::addresses`]）：
    /// 那個是給指標標籤用的，一個站台塊寫兩條位址就會有兩個鍵 ——
    /// 而維運要摘的是「**這個站台塊**裡的那臺機器」。
    pub site: &'a str,
    /// 鍵的**第二格**：這條 `reverse_proxy` 的 id，**沒寫就是空串**。
    pub id: &'a str,
    pub target: &'a ProxyTarget,
}

/// 走遍一批站台，把全部 `reverse_proxy` 連同站台名收出來。
///
/// ⚠ 是**自由函式**而不是 [`Runtime`] 的方法（與 [`cache_settings_of`] 同一條理由）：
/// `Runtime::build` 那條路上 `sites` 還是一個本機 `Vec`，`Runtime` 尚未建出來 ——
/// 而任務 3 的 `build_with_overrides` 要**在那一刻**按鍵把覆蓋格子掛到每個 `Upstream` 上。
/// 抄一份 walk 過去，就等於讓「掛格子走的那張圖」與「跑起來的那張圖」有機會不是同一件事。
fn keyed_proxies_of(sites: &[SiteRt]) -> Vec<KeyedProxy<'_>> {
    fn walk<'a>(steps: &'a [StepRt], site: &'a str, out: &mut Vec<KeyedProxy<'a>>) {
        for s in steps {
            match &s.body {
                BodyRt::Proxy(t) => out.push(KeyedProxy {
                    site,
                    id: &t.id,
                    target: t,
                }),
                // ⚠ 容器裡的也要進來：一條藏在 `handle` 裡的 `reverse_proxy`
                //   漏出這張清單的話，它就**沒有鍵**——覆蓋指不到它，
                //   而它在 `proxy_key_fanout` 的計數裡也不存在，兩件事一起靜默。
                BodyRt::Handle(arms) => {
                    for a in arms {
                        walk(&a.steps, site, out);
                    }
                }
                BodyRt::Route(inner) => walk(inner, site, out),
                // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                //   —— 要名單就現場 grep 這兩個枚舉名。
                BodyRt::Tracing
                | BodyRt::Header(_)
                | BodyRt::Rewrite(_)
                | BodyRt::Encode(_)
                | BodyRt::Redir { .. }
                | BodyRt::Respond { .. }
                | BodyRt::Metrics
                | BodyRt::FileServer(_)
                | BodyRt::Cache(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    for site in sites {
        walk(&site.chain, &site.name, &mut out);
        walk(&site.error_handler, &site.name, &mut out);
    }
    out
}

/// 一格覆蓋層鍵，連同它**管著幾條 `reverse_proxy`**
/// （**M2 批 N 任務 2.9**，裁決 R6 ③ 第二輪 ⇒ **G125**）。
///
/// # 為什麼有這個東西
///
/// 管理面的臨時覆蓋層（G18）用 `(站点名, id, 归一化后的上游地址)` 尋址。
/// 同一個站台裡兩條 `reverse_proxy` 指著同一臺機器而都沒寫 `id` 時，它們的鍵
/// **真的相同** ⇒ 它們拿到的是**同一個覆蓋格子**，一次 `disable` 兩條一起生效。
///
/// ⚠ ⚠ **這不是錯誤。** 任務 2.8 曾照裁決 R6 第一輪的口徑在裝載期拒絕它，
/// 而那一輪的立論是「現有設定一個位元組都不用改」—— 實測是假的：
/// 「一個後端掛在幾組 `handle` 路由後面」是反代最常見的寫法，本儲存庫四個端到端場景
/// （serve · h3 · log · encode）裡就有四份，接上檢查當場全部裝不上。
/// ⇒ owner 重拍：**撞鍵不拒絕，同鍵共享同一個格子**。
/// 而「一起摘掉」多半正是要的語義：維運說「摘掉 `10.0.0.5:8080`」時那臺機器是壞的，
/// 它不該從任何路由收流量。想細分的人寫一行 `id` 就能分開。
///
/// ★ 但那個真實風險還在 —— 使用者可能**以為 `disable` 只影響一條**。
/// ⛔ 不用裝載期 warning 解決（那是給一個正常寫法加噪音），
/// ⇒ 由 `/stats` 把 [`ProxyKeyFanout::proxies`] 顯示出來，
/// 那才是 G18「不持久化但永遠可見」的正確形態。
///
/// # ⚠ ⚠ ⚠ 鍵的第三格為什麼必須取歸一化**之後**的串
///
/// [`normalize_upstream`] 把 `backend` 補成 `backend:80` ⇒ 兩條 `reverse_proxy`
/// **一條寫 `backend`、另一條寫 `backend:80`**，原文不同而**鍵相同**，
/// 覆蓋格子也必須是同一個。拿設定裡的原文 token 建鍵的話這一對會被算成兩格 ——
/// 於是維運 `disable` 了一格，另一格上的同一臺機器還在收流量，而沒有任何東西會回報錯誤。
/// ★ 這裡讀的 `u.addr` **就是**鍵本身，不是一個長得像它的串。
/// ⇒ 由 `tests/routing.rs` 裡那條「核心判據」釘住：那條判據在這個錯誤下**單獨變紅**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyKeyFanout<'a> {
    /// 鍵的**第一格**：站台名（[`KeyedProxy::site`]）。
    pub site: &'a str,
    /// 鍵的**第二格**：這條 `reverse_proxy` 的 id，沒寫就是空串（[`ProxyTarget::id`]）。
    pub id: &'a str,
    /// 鍵的**第三格**：**歸一化之後**的上游位址（[`Upstream::addr`]）。
    pub addr: &'a str,
    /// 這一格鍵管著幾條 `reverse_proxy`。**恆 ≥ 1**；
    /// `> 1` = 一次 `disable` 會同時摘掉這麼多條 `reverse_proxy` 上的這臺機器。
    ///
    /// ⚠ 數的是 **`reverse_proxy` 條數**，不是 `Upstream` 個數：同一條 `reverse_proxy`
    /// 裡把同一個位址寫了兩遍**只算一條** —— 那兩個 `Upstream` 本來就是同一條上的
    /// 同一臺機器，一起摘掉不是意外，而「給其中一個寫 `id`」在那種情形下無從下手。
    pub proxies: usize,
}

/// 走遍一批站台，算出每一格覆蓋層鍵管著幾條 `reverse_proxy`。見 [`ProxyKeyFanout`]。
///
/// ⚠ 與 [`keyed_proxies_of`] 同一條理由做成**自由函式**：`Runtime` 還沒建出來時也拿得到。
fn proxy_key_fanout_of(sites: &[SiteRt]) -> Vec<ProxyKeyFanout<'_>> {
    // `(站点名, id, 归一化后的上游地址)` → 管著幾條。
    // ⚠ 用 `BTreeMap` 而不是 `HashMap`：`/stats` 那一節是**永遠可見**的東西（G18），
    //   而每次換一個順序的清單沒法逐次比對。
    let mut n: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    for p in keyed_proxies_of(sites) {
        // ★ 同一條裡的重複位址先去重，理由在 `ProxyKeyFanout::proxies` 上。
        let mut here: std::collections::BTreeSet<&str> = Default::default();
        for u in &p.target.upstreams {
            if here.insert(u.addr.as_str()) {
                *n.entry((p.site, p.id, u.addr.as_str())).or_insert(0) += 1;
            }
        }
    }
    n.into_iter()
        .map(|((site, id, addr), proxies)| ProxyKeyFanout {
            site,
            id,
            addr,
            proxies,
        })
        .collect()
}

/// 一次上游解析的結果，只進紀錄。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResolveReport {
    /// 這次真去做了 DNS 的網域個數（IP 字面值不算）。
    pub queried: usize,
    /// 這次解析結果**變了**的上游：`(配置里的地址, 新的候选清单)`。
    pub changed: Vec<(String, Vec<std::net::SocketAddr>)>,
    /// 這次解析**失敗**的上游：`(配置里的地址, 原因)`。
    pub failed: Vec<(String, String)>,
}

impl ResolveReport {
    pub fn is_quiet(&self) -> bool {
        self.changed.is_empty() && self.failed.is_empty()
    }
}

/// 把一份執行環境圖裡所有**網域**上游解析一遍，把結果寫進各自的槽裡。
///
/// 它就是 `dns_refresh` 的本體。
///
/// ⚠ **它是阻塞的**，只許在三處調：`serve` 啟動時 · 全量 load 生效之前 ·
/// 後台任務裡（**要放進 `spawn_blocking`**）。★ **絕不許在請求路徑上調**（見 [`Upstream`]）。
///
/// ★ **解析失敗不清掉上一次的好結果**：DNS 抖一下就摘掉一個還在正常服務的上游，
/// 等於把一次解析器故障放大成一次服務故障。
pub fn resolve_upstreams(rt: &Runtime) -> ResolveReport {
    use std::net::ToSocketAddrs;
    let mut report = ResolveReport::default();
    for up in rt.all_upstreams() {
        if up.is_literal_ip() {
            continue; // 不需要 DNS，也不會變
        }
        report.queried += 1;
        match up.addr.to_socket_addrs() {
            Ok(it) => {
                // ★ **全部收下**，不是隻取第一個。理由見 `Upstream::resolved`。
                let addrs: Vec<std::net::SocketAddr> = it.collect();
                if addrs.is_empty() {
                    // 解析「成功」但一個位址都沒有。⚠ 這不是不可能。
                    report
                        .failed
                        .push((up.addr.clone(), "解析成功但没有任何地址".to_string()));
                    continue;
                }
                if up.dial_candidates() != addrs {
                    report.changed.push((up.addr.clone(), addrs.clone()));
                }
                up.set_resolved(addrs);
            }
            Err(e) => report.failed.push((up.addr.clone(), e.to_string())),
        }
    }
    report
}

pub struct SharedRuntime {
    inner: std::sync::RwLock<std::sync::Arc<Runtime>>,
    /// **臨時覆蓋層的格子登記處**（**M2 批 N 任務 3**，G18 / 裁決 R7）。
    ///
    /// ★ ★ 它掛在**這裡**而不是 [`Runtime`] 上，因為它必須**跨換代活著**：
    /// 全量 load 換掉的正是裡面那一份 `Runtime`，而覆蓋層要能 `keep`。
    /// ⛔ 別在管理面那邊另存一份（同裁決 R15 對「設定裝載時刻」的處置）——
    /// 兩份狀態遲早對不上，而對不上那天沒有任何東西會說。
    overrides: std::sync::Arc<OverrideLayer>,
    /// **設定裝載時刻**（**M2 批 N 任務 6**，裁決 R15）：`new` 與 [`Self::swap`]
    /// 各設一次，⛔ **不在管理面那邊另記一份**——理由與 `overrides` 那一格逐字相同：
    /// 兩份狀態遲早對不上，而對不上那天沒有任何東西會說。`/stats` 從
    /// [`Self::loaded_at`] 讀，不自己算。
    ///
    /// ⚠ 存 [`std::time::SystemTime`] 而不是秒級整數：`/stats` 的 R12 判據要能
    /// 在同一個 CI 容器裡的兩次 `POST /load` 之間看出「變了」，秒級粒度在一次
    /// 跑得很快的測試裡可能落在同一秒——那會讓一條本該穩定通過的判據偶發抖動。
    loaded_at: std::sync::RwLock<std::time::SystemTime>,
    /// 哪些連接埠上有反代站台（G163），見 [`Self::port_has_proxy`]。
    proxy_ports: PortBits,
}

/// 65536 位的連接埠點陣圖（8 KiB）。讀請求頭之前每條請求查一次 ⇒ 只是一次原子讀，
/// ⛔ 不為它再拿一次 [`SharedRuntime::current`] 的讀鎖（每請求已經拿過一次了）。
///
/// ⚠ `new` 與 [`SharedRuntime::swap`] 各寫一次，與執行環境本身的替換**不是**原子的：
/// 正落在換代那一瞬的一條請求可能按舊圖定 —— 最壞是這一條轉發到上游時頭名變成標題化表的寫法或小寫
/// （與 h2 用戶端的請求今天的樣子相同），或這一條多建了一張表。各位之間互不相干 ⇒ `Relaxed` 就夠。
struct PortBits(Box<[std::sync::atomic::AtomicU64]>);

impl PortBits {
    fn new(ports: &BTreeSet<u16>) -> PortBits {
        let bits = PortBits(
            (0..1024)
                .map(|_| std::sync::atomic::AtomicU64::new(0))
                .collect(),
        );
        bits.store(ports);
        bits
    }

    fn store(&self, ports: &BTreeSet<u16>) {
        let mut words = vec![0u64; 1024];
        for &p in ports {
            words[usize::from(p) >> 6] |= 1 << (p & 63);
        }
        for (w, v) in self.0.iter().zip(words) {
            w.store(v, Ordering::Relaxed);
        }
    }

    fn get(&self, port: u16) -> bool {
        (self.0[usize::from(port) >> 6].load(Ordering::Relaxed) >> (port & 63)) & 1 == 1
    }
}

impl SharedRuntime {
    /// 建一個共享槽，並**從這份執行環境身上認領它的覆蓋格子**（批 N 任務 3）。
    ///
    /// ★ ★ ★ 認領這一步不能省。`serve` 啟動時走的是 [`Runtime::build`]（那條路上
    /// 還沒有登記處），於是它的上游拿著的是一次性登記表裡的格子。**不認領的話**，
    /// 啟動之後第一條 `POST /runtime` 會在這張空登記表裡**新建**一格 ——
    /// 鍵對得上、命令回 200、而那臺機器還在收流量。
    /// ⇒ 認領之後，「這份執行環境的上游」與「登記處按鍵取到的格子」是同一批物件。
    ///
    /// ⚠ 認領之所以是對的，是因為**任何一份執行環境的格子都已經按鍵共享過**
    /// （[`Runtime::build`] 自己也走 [`OverrideLayer::slot`]）⇒ 同一個鍵上認領幾次，
    /// 認領到的都是同一個 [`std::sync::Arc`]。
    pub fn new(rt: std::sync::Arc<Runtime>) -> std::sync::Arc<SharedRuntime> {
        let overrides = std::sync::Arc::new(OverrideLayer::new());
        for p in rt.keyed_proxies() {
            for u in &p.target.upstreams {
                overrides.adopt(
                    OverrideKey::new(p.site, p.id, &u.addr),
                    u.override_slot().clone(),
                );
            }
        }
        let proxy_ports = PortBits::new(&rt.proxy_ports());
        std::sync::Arc::new(SharedRuntime {
            inner: std::sync::RwLock::new(rt),
            overrides,
            // ★ R15：`new` 也算一次「裝載」——處理程序剛起來時服務的那份設定
            // 同樣有一個裝載時刻，不是只有 `swap` 才算數。
            loaded_at: std::sync::RwLock::new(std::time::SystemTime::now()),
            proxy_ports,
        })
    }

    /// 這個連接埠上有沒有反代站台（G163）。讀 h1 請求頭之前就要定「要不要保留頭名的原樣大小寫」，
    /// 那時還不知道 Host ⇒ 只能按連接埠；口徑見 [`Runtime::proxy_ports`]。
    pub fn port_has_proxy(&self, port: u16) -> bool {
        self.proxy_ports.get(port)
    }

    /// 這個處理程序的**格子登記處**。管理面（任務 4 / 5 / 6）從這裡拿，⛔ 別自己建第二個。
    pub fn overrides(&self) -> &std::sync::Arc<OverrideLayer> {
        &self.overrides
    }

    /// **設定裝載時刻**（裁決 R15）：`/stats` 的「設定裝載時間」從這裡讀，
    /// ⛔ 別在管理面那邊另算一份——兩份狀態遲早對不上，理由與 `overrides`
    /// 欄位上那段逐字相同。
    pub fn loaded_at(&self) -> std::time::SystemTime {
        match self.loaded_at.read() {
            Ok(g) => *g,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// 當前的覆蓋清單，**懸空的已經標好**（裁決 R8）。
    ///
    /// ★ ★ 這是任務 5（`/load` 的回話要逐條點名懸空的）與任務 6（`/stats` 的
    /// `overrides` 一節）**唯一**的取數口：`live` 一定取自**正在服務的**那一份執行環境，
    /// ⛔ 別自己去拼 `live` 再調 [`OverrideLayer::entries`]。
    ///
    /// ⚠ ⚠ 內部調一次 [`Self::current`]。**呼叫方若已經持有一份 `Arc<Runtime>`
    /// 快照**（比如 `/stats` 的 `stats()`，它開頭先取了一份、後面全部讀取都
    /// 該基於那一份），再調這個方法會讓同一次回應裡出現**兩次** `current()`：
    /// 一次並行的 `POST /load` 落在兩次之間，`overrides` 那一節的 `dangling`
    /// 就會按**新**設定算，而別的欄位（`upstreams`/`fanout`）按**舊**設定列——
    /// 這與 [`Self::current`] 自己文件上「每個請求只調一次」、`metrics.rs`
    /// 「分兩次取會落在兩份不同的設定上」是同一條紀律。⇒ 已經持有快照的
    /// 呼叫方改用 [`Self::override_entries_of`]，把手裡那一份傳進來。
    pub fn override_entries(&self) -> Vec<OverrideEntry> {
        self.override_entries_of(&self.current())
    }

    /// 同 [`Self::override_entries`]，但 `rt` 由呼叫方給——不在內部再調一次
    /// [`Self::current`]。給**已經持有一份快照**的呼叫方用，理由見上面
    /// [`Self::override_entries`] 的文件。
    pub fn override_entries_of(&self, rt: &Runtime) -> Vec<OverrideEntry> {
        self.overrides.entries(&rt.override_keys())
    }

    /// `(生效中的覆盖项数, 其中悬空的项数)` —— 裁決 R11 那句
    /// 「當前有 N 項臨時覆蓋生效中（其中 M 項懸空）」的取數。
    ///
    /// ⚠ 懸空的**照樣算生效中**（裁決 R13 明寫）：它確實還在登記處佔著一格。
    /// ⇒ `N` 是全部條目數，`M` 是其中懸空的那些，**M ≤ N**。
    pub fn override_counts(&self) -> (usize, usize) {
        let all = self.override_entries();
        let dangling = all.iter().filter(|e| e.dangling).count();
        (all.len(), dangling)
    }

    /// 取當前那一份。**每個請求只調一次**，理由見類型文件。
    ///
    /// ⚠ 鎖中毒時**回不了「沒有設定」**——那會讓資料面整個停擺。
    /// 這裡的處置是 `unwrap_or_else(|e| e.into_inner())`：中毒說明某個持鎖執行緒 panic 過，
    /// 而那份 `Arc<Runtime>` 本身仍然是完整的（它是不可變的）。
    pub fn current(&self) -> std::sync::Arc<Runtime> {
        match self.inner.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 整體換掉。★ 呼叫方必須**先把新的建好**再調這裡 ——
    /// 建到一半失敗時不許留下任何痕跡（G8：設定變更是交易，不是檔案寫入）。
    ///
    /// # ★ ★ ★ 它收 `Runtime` 而不是 `Arc<Runtime>`，那是結構保護
    ///
    /// 裝進來之前要把**覆蓋層的格子**掛上去（批 N 任務 3），而那要 `&mut`。
    /// ⇒ 收所有權之後，「一份沒掛過格子的執行環境被裝進來服務流量」在**結構上做不到** ——
    /// 而那種執行環境的現場是「`POST /runtime` 回 200，那臺機器還在收流量」，
    /// 沒有任何東西會回報錯誤。⚠ 呼叫方本來就是「剛建好、還沒給任何人」，
    /// 這個簽名對它們不多要一分錢。
    ///
    /// ★ 換完順手收拾登記處：清掉「新執行環境不引用、**且**沒設過覆蓋」的空格子
    /// （[`OverrideLayer::retain_after_swap`]）。⚠ **設過覆蓋而沒人引用的留著** ——
    /// 那是懸空（裁決 R8），不是垃圾。
    pub fn swap(&self, rt: Runtime) {
        let mut rt = rt;
        rt.attach_overrides(&self.overrides);
        let live = rt.override_keys();
        let proxy_ports = rt.proxy_ports();
        let rt = std::sync::Arc::new(rt);
        // ★ ★ R15：**唯讀一次時鐘**（批 M 的教訓：收尾路徑上讀兩次時鐘，
        //   同一件事的兩個數會對不上）。這裡只有一個消費者（`loaded_at`），
        //   但仍然顯式只調一次 `now()`，不在下面兩處各自現調。
        let now = std::time::SystemTime::now();
        match self.inner.write() {
            Ok(mut g) => *g = rt,
            Err(poisoned) => *poisoned.into_inner() = rt,
        }
        match self.loaded_at.write() {
            Ok(mut g) => *g = now,
            Err(poisoned) => *poisoned.into_inner() = now,
        }
        self.proxy_ports.store(&proxy_ports);
        // ⚠ 順序：**換完之後**才收拾（裁決 R7 的原話是「一次成功的 swap 之後」）。
        //   反過來的話，一次在寫鎖上失敗的 swap 會先把登記處清了。
        self.overrides.retain_after_swap(&live);
    }
}

impl std::fmt::Debug for SharedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedRuntime({} 个站点)", self.current().sites.len())
    }
}

impl Runtime {
    /// 從結構化設定建置。**檢核也在這裡**——結構化層是公開入口（G11）。
    ///
    /// ⚠ ⚠ **簽名一個字都不許動**（批 N 計畫 §2 的 S7）：`fulcrum validate` 走的是這條路，
    /// 而它的全部價值在於**離線**就能說話。⇒ 想讓執行環境接上臨時覆蓋層的，
    /// 走 [`Self::build_with_overrides`]。
    ///
    /// ★ 它自己也走同一條路，只是那張登記表是**一次性的**：格子照樣按鍵共享
    /// （於是「同鍵的兩條 `reverse_proxy` 拿到同一格」這件事在這條路上也成立），
    /// 只是建完之後沒人拿得到那張表 ⇒ 誰也改不了這份執行環境的覆蓋。
    pub fn build(cfg: &StructuredConfig) -> Result<Runtime, Vec<BuildError>> {
        Runtime::build_with_overrides(cfg, &OverrideLayer::new())
    }

    /// 從結構化設定建置，並把**臨時覆蓋層的格子掛上去**（**M2 批 N 任務 3**，G18 / 裁決 R7）。
    ///
    /// ★ ★ 建完的執行環境裡，每個 HTTP 上游身上那一格與 `layer` 按它的鍵取到的那一格
    /// **是同一個物件** ⇒ 管理面改登記處，那個上游立刻算數；**沒有第二步**。
    ///
    /// ⚠ 建**失敗**時一格都不登記（`?` 在掛之前就出去了）⇒ 一次失敗的 load
    /// 不會在登記處留下任何痕跡（G8）。
    ///
    /// ⚠ 建**成功**但呼叫方隨後又不要它了時，傳進來的那個 `layer` 會多出幾格
    /// **空格子** —— 它們不計數、不進清單，並在下一次成功的 swap 時被清掉
    /// （見 [`OverrideLayer`]）。
    ///
    /// ⛔ **不要把上面那句讀成「`/load` 被 409 拒掉時活的登記處會髒」** ——
    /// 這裡寫過那句話，**實測不成立**：`/load` 走的是 [`Runtime::build`]，而它
    /// 傳的是一個**當場新建、隨後丟棄**的 `OverrideLayer::new()`（見該函式），
    /// ⇒ 409 拒掉的時候，髒掉的是那個一次性的臨時層，**活的登記處一個位元組都沒動**。
    /// ★ 事實上 `build_with_overrides` 配上**活的**登記層**沒有任何生產呼叫方**
    /// （裁決 R8：它是 brief 要求的交付物，且判據 13 靠它成立）——
    /// 空格子那條路今天只有測試走得到。
    /// ★ ★ 記住這條的形狀：**一句沒有門守著的行為描述**，讀起來完全成立，
    /// 而它描述的情形在真實呼叫路徑上根本到不了。
    pub fn build_with_overrides(
        cfg: &StructuredConfig,
        layer: &OverrideLayer,
    ) -> Result<Runtime, Vec<BuildError>> {
        let mut rt = Runtime::build_graph(cfg)?;
        rt.attach_overrides(layer);
        Ok(rt)
    }

    /// [`Self::build`] 的本體：只建圖，不碰覆蓋層。
    fn build_graph(cfg: &StructuredConfig) -> Result<Runtime, Vec<BuildError>> {
        let mut errors: Vec<BuildError> = Vec::new();
        let mut sites = Vec::new();
        let mut exact: BTreeMap<u16, BTreeMap<String, (usize, usize)>> = BTreeMap::new();
        let mut wildcard: Vec<(String, u16, usize, usize)> = Vec::new();
        let mut catch_all = BTreeMap::new();
        let mut listen: BTreeMap<u16, bool> = BTreeMap::new();

        for (si, s) in cfg.sites.iter().enumerate() {
            let name = s
                .addresses
                .first()
                .map(|a| a.raw.clone())
                .unwrap_or_else(|| format!("sites[{si}]"));
            let at = format!("sites[{si}] \"{name}\"");

            let mut matchers = BTreeMap::new();
            for (mname, m) in &s.matchers {
                let mat = CompiledMatcher::build(&format!("{at} · @{mname}"), m, &mut errors);
                matchers.insert(mname.clone(), mat);
            }

            let mut chain = Vec::new();
            for st in &s.chain {
                if let Some(step) = build_step(&at, st, &matchers, &mut errors) {
                    chain.push(step);
                }
            }
            let mut error_handler = Vec::new();
            for st in &s.error_handler {
                if let Some(step) =
                    build_step(&format!("{at} · handle_errors"), st, &matchers, &mut errors)
                {
                    error_handler.push(step);
                }
            }

            // ★ ★ 每條位址的標籤字面值（G121），下標與 `s.addresses` 一一對應 ——
            //   完整的取捨寫在 `SiteRt::addresses` 的類型文件上，這裡只管算。
            let mut addresses: Vec<std::sync::Arc<str>> = Vec::new();
            for (ai, a) in s.addresses.iter().enumerate() {
                let label = if a.host.is_empty() {
                    format!(":{}", a.port)
                } else {
                    a.host.to_ascii_lowercase()
                };
                addresses.push(std::sync::Arc::<str>::from(label));

                let needs_tls = a.scheme == "https";
                // ★ ★ 同一個連接埠不能既有 http:// 又有 https:// 的站台。
                //   ⚠ 這不是潔癖：一個監聽 socket 只能說一種協定。寫成
                //   `*t |= needs_tls`，於是「連接埠上有一個 https 站台」會把同連接埠的
                //   http 站台**靜默地也變成 TLS**——那些站台從此對明文請求全部握手失敗，
                //   而設定裡看不出任何問題。
                match listen.entry(a.port) {
                    std::collections::btree_map::Entry::Vacant(e) => {
                        e.insert(needs_tls);
                    }
                    std::collections::btree_map::Entry::Occupied(e) => {
                        if *e.get() != needs_tls {
                            errors.push(BuildError::new(
                                &at,
                                format!(
                                    "端口 {} 上同时有 http:// 与 https:// 的站点——一个监听 socket 只能说一种协议",
                                    a.port
                                ),
                            ));
                        }
                    }
                }
                if a.host.is_empty() {
                    if catch_all.insert(a.port, (si, ai)).is_some() {
                        errors.push(BuildError::new(
                            &at,
                            format!("端口 {} 上已经有一个不带主机名的站点了", a.port),
                        ));
                    }
                } else if a.wildcard {
                    // `*.example.com` → 後綴 `.example.com`
                    let suffix = a.host.trim_start_matches('*').to_string();
                    wildcard.push((suffix, a.port, si, ai));
                } else if exact
                    .entry(a.port)
                    .or_default()
                    .insert(a.host.to_ascii_lowercase(), (si, ai))
                    .is_some()
                {
                    errors.push(BuildError::new(
                        &at,
                        format!("地址 {}:{} 重复", a.host, a.port),
                    ));
                }
            }

            sites.push(SiteRt {
                name,
                hostnames: s
                    .addresses
                    .iter()
                    .filter(|a| !a.host.is_empty())
                    .map(|a| a.host.to_ascii_lowercase())
                    .collect(),
                addresses,
                tls: s.tls.clone(),
                log: s
                    .log
                    .as_ref()
                    .and_then(|c| LogRt::build(&at, c, &mut errors))
                    .map(std::sync::Arc::new),
                matchers,
                chain,
                error_handler,
            });
        }

        // ★ 後綴長的排前面：`*.a.example.com` 必須先於 `*.example.com` 被試到，
        //   否則更具體的那條永遠輪不到——而它看起來只是「設定沒生效」。
        // ★ 長後綴在前。⚠ 「只吃一層」（D18 / G66）之後這次排序**已經是冗餘的**：
        //   兩個通配後綴不可能同時覆蓋同一個 host（`x.a.example.com` 去掉
        //   `.a.example.com` 剩 `x` 中，去掉 `.example.com` 剩 `x.a` 不中）。
        //   留著是為了讓遍歷順序穩定可讀，**別再把它當成「更具體者優先」的實作**——
        //   那個性質現在由 `wildcard_covers` 自己保證。
        wildcard.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));

        let listen_ports: Vec<(u16, bool)> = listen.into_iter().collect();

        // ★ ★ ★ **一條判據被刪掉的時候，它守著的那件事不會跟著消失。**
        //   回落層刪除（G98）時，只守回完成實作址的自環檢測挪到了上游那一側
        //   （`check_self_loop_upstreams`）—— `reverse_proxy 127.0.0.1:443` 是同一個事故。
        let self_loop_warnings = self_loop_warnings(&sites, &listen_ports);

        // ── L4 面（M2 批 A）───────────────────────────────────────────────
        //
        // ★ 與上游、回落**同一條紀律**：位址在**裝載期**就要能拆開、能判死。
        //   ⚠ 一個「監聽位址寫錯了、啟動時才失敗」的 L4 塊，症狀是
        //   `systemctl reload` 之後那個連接埠靜悄悄地沒了 —— 而 HTTP 一切正常，
        //   於是沒有人會去看 L4。
        let mut l4_listeners: Vec<L4ListenerRt> = Vec::new();
        if let Some(l4) = &cfg.l4 {
            let mut seen_ports: BTreeMap<(String, u16), String> = BTreeMap::new();
            for l in &l4.listeners {
                let at = format!("l4 {} {}", l.proto, l.listen);
                let proto = match l.proto.as_str() {
                    "tcp" => L4Proto::Tcp,
                    "udp" => L4Proto::Udp,
                    other => {
                        errors.push(BuildError::new(
                            &at,
                            format!("认不出的 L4 协议 `{other}`——只能是 `tcp` 或 `udp`"),
                        ));
                        continue;
                    }
                };
                let (listen_host, listen_port) = match parse_l4_listen(&l.listen) {
                    Ok(v) => v,
                    Err(m) => {
                        errors.push(BuildError::new(
                            &at,
                            format!("监听地址 `{}`：{m}", l.listen),
                        ));
                        continue;
                    }
                };
                // ★ 同一個 `(协议, 主机, 端口)` 寫兩遍 ⇒ 第二個必然 bind 失敗，
                //   而失敗發生在啟動時、紀錄裡只有一句 EADDRINUSE。裝載期說清楚。
                let dup_key = (
                    format!(
                        "{}:{}",
                        proto.as_str(),
                        listen_host.as_deref().unwrap_or("*")
                    ),
                    listen_port,
                );
                if let Some(first) = seen_ports.get(&dup_key) {
                    errors.push(BuildError::new(
                        &at,
                        format!("{} 端口 {listen_port} 已经被 `{first}` 占了——两个监听器绑同一个端口，第二个必然起不来", proto.as_str()),
                    ));
                    continue;
                }
                seen_ports.insert(dup_key, l.listen.clone());
                // ★ ★ **L4 連接埠不能與 HTTP 監聽連接埠相撞**：兩者在同一個處理程序裡
                //   各自 bind，撞了就是啟動失敗。⚠ 只比連接埠號不比位址，理由與
                //   `self_loop_port` 那條一樣：同一處理程序內兩個監聽器搶同一個連接埠，
                //   位址再不同也沒有意義（`0.0.0.0` 與 `127.0.0.1` 會真的相撞）。
                if proto == L4Proto::Tcp && listen_ports.iter().any(|(p, _)| *p == listen_port) {
                    errors.push(BuildError::new(
                        &at,
                        format!(
                            "端口 {listen_port} 上已经有一个 HTTP 站点在监听——同一个进程里两个监听器抢同一个端口，起不来"
                        ),
                    ));
                    continue;
                }
                // ★ 備援與每條規則各建一組上游，**用的是同一個函式**（`build_l4_target`）——
                //   於是「上游怎麼規範化、IP 字面值什麼時候填槽、用哪種 lb_policy」只有一份實作。
                let default_target = build_l4_target(&l.upstreams, &at, "", &mut errors);
                let mut rules: Vec<L4RuleRt> = Vec::new();
                for r in &l.rules {
                    let kind = match r.kind.as_str() {
                        "sni" => L4MatchKind::Sni,
                        "alpn" => L4MatchKind::Alpn,
                        other => {
                            errors.push(BuildError::new(
                                &at,
                                format!("认不出的分流条件 `{other}`——只能是 `sni` 或 `alpn`"),
                            ));
                            continue;
                        }
                    };
                    // ⚠ `udp` 上沒有 ClientHello。DSL 那一側已經攔了一次，
                    //   這裡再攔一次是因為**結構化設定是公開入口**（G11）：
                    //   機器直接寫這一層時不經過 DSL 前端。
                    if proto == L4Proto::Udp {
                        errors.push(BuildError::new(
                            &at,
                            format!("`udp` 上不能按 `{}` 分流——UDP 没有 TLS ClientHello", r.kind),
                        ));
                        continue;
                    }
                    if r.values.iter().any(|v| v.trim().is_empty()) {
                        errors.push(BuildError::new(
                            &at,
                            format!("`{}` 的匹配值里有空的", r.kind),
                        ));
                        continue;
                    }
                    let Some(target) = build_l4_target(
                        &r.upstreams,
                        &at,
                        &format!("`{} {}` 的", r.kind, r.values.join(" ")),
                        &mut errors,
                    ) else {
                        errors.push(BuildError::new(
                            &at,
                            format!("`{} {}` 一个可用的上游都没有", r.kind, r.values.join(" ")),
                        ));
                        continue;
                    };
                    rules.push(L4RuleRt {
                        kind,
                        values: r.values.clone(),
                        target,
                    });
                }
                // ⚠ 備援與規則**不能都沒有**：那樣的監聽器接受連接之後只能立刻關掉。
                //   ★ 但「只有規則、沒有備援」是合法的 —— 那是「只服務我認得的那幾個名字」。
                if default_target.is_none() && rules.is_empty() {
                    errors.push(BuildError::new(&at, "一个可用的上游都没有"));
                    continue;
                }
                // ★ **網段在這裡解析，不在資料面**（本 crate 第 2 條設計約束）：
                //   這一條的線上事故形態尤其壞 —— **信任清單不生效 = 用戶端 IP 全都取錯**，
                //   而站台照常服務、紀錄照常有行。
                let mut pp_from: Vec<Cidr> = Vec::new();
                for v in &l.proxy_protocol_from {
                    match Cidr::parse(v) {
                        Some(c) => pp_from.push(c),
                        None => errors.push(BuildError::new(
                            &at,
                            format!("`proxy_protocol_from` 里的 `{v}` 不是合法的 IP 或 CIDR（如 `10.0.0.0/8`）"),
                        )),
                    }
                }
                // ⚠ ⚠ 這兩條**只在 tcp 上有意義**，而 DSL 前端已經把 udp 上的寫法
                //   攔成編譯期錯誤了。這裡再攔一次不是重複：**結構化設定是公開入口**
                //   （G11），機器可以直接寫它、繞過 DSL —— 與 `sni`/`alpn` 那條
                //   「判據在兩層都有」逐字同一條理由。
                if proto == L4Proto::Udp
                    && (!l.proxy_protocol_from.is_empty() || l.proxy_protocol.is_some())
                {
                    errors.push(BuildError::new(
                        &at,
                        "`udp` 上不能有 PROXY protocol：它是面向连接的（头在连接开头发一次），\
                         而 UDP 上没有连接开头",
                    ));
                    continue;
                }
                let pp_send = match &l.proxy_protocol {
                    None => None,
                    Some(v) => match crate::proxyproto::Version::parse(v) {
                        Some(ver) => Some(ver),
                        None => {
                            errors.push(BuildError::new(
                                &at,
                                format!("`proxy_protocol` 认不出的版本 `{v}`——只能是 `v1` 或 `v2`"),
                            ));
                            continue;
                        }
                    },
                };
                l4_listeners.push(L4ListenerRt {
                    proto,
                    listen: l.listen.clone(),
                    listen_host,
                    listen_port,
                    target: default_target,
                    rules,
                    proxy_protocol_from: pp_from,
                    proxy_protocol: pp_send,
                });
            }
        }

        // ── M2 批 D：HTTP 面的信任清單 ────────────────────────────────────
        //
        // ★ 與 L4 那份走**同一個** `Cidr::parse`，錯誤也在同一批裡報出來。
        let mut http_pp_from: Vec<Cidr> = Vec::new();
        for v in &cfg.global.proxy_protocol_from {
            match Cidr::parse(v) {
                Some(c) => http_pp_from.push(c),
                None => errors.push(BuildError::new(
                    "全局 proxy_protocol_from",
                    format!("`{v}` 不是合法的 IP 或 CIDR（如 `10.0.0.0/8`）"),
                )),
            }
        }

        // ── M2 批 H：快取後端是**處理程序級**的，全圖只能有一個 ────────────────
        //
        // ⚠ **結構化設定是公開入口**（G11）：`FUL-DSL-0035` 只活在編譯層，手寫 JSON 繞得過去。
        //   繞過去之後 `serve()` 取**第一個**目錄，另一個站台的快取整個落在別處，
        //   而設定檔案、`validate`、裝載紀錄三處都顯得正常。
        // ★ 判據走**建起來的那張圖**，不照設定再數一遍。
        {
            let mut seen: Option<(&str, Option<&str>)> = None;
            for (site, c) in cache_settings_of(&sites) {
                let here = c.disk_dir.as_deref();
                match seen {
                    None => seen = Some((site, here)),
                    Some((_, first)) if first == here => {}
                    Some((first_site, first)) => {
                        let d = |x: Option<&str>| match x {
                            Some(v) => format!("磁盘 `{v}`"),
                            None => "内存（没写 disk）".to_string(),
                        };
                        errors.push(BuildError::new(
                            site,
                            format!(
                                "`cache` 的后端是**进程级**的，而这份配置里选了两个：\
                                 站点 {first_site} 要 {}，站点 {site} 要 {} —— \
                                 两个不同的值里必有一个是「以为生效、其实没有」的",
                                d(first),
                                d(here)
                            ),
                        ));
                        break;
                    }
                }
            }
        }

        // ⚠ ⚠ **這裡有意沒有「覆蓋層的鍵撞了就拒絕」那條檢查**（M2 批 N 任務 2.9，
        //   裁決 R6 ③ **第二輪** ⇒ G125）。任務 2.8 照第一輪口徑加過一條，
        //   而它當場讓 serve / h3 / log / encode 四個端到端場景全部裝不上 ——
        //   撞的都是「一個後端掛在幾組 `handle` 路由後面」這個反代最常見的寫法。
        //   ⇒ owner 重拍：**撞鍵不拒絕，同鍵共享同一個覆蓋格子**。
        //   ★ 那段邏輯沒有刪，改向成了 `proxy_key_fanout_of`（「每個鍵管著幾條」），
        //     由 `/stats` 顯示 —— 完整的理由在 `ProxyKeyFanout` 的類型文件上。
        //   ⛔ 別在這裡補一條 warning：那是給一個正常寫法加噪音（owner 明確不要）。

        if errors.is_empty() {
            Ok(Runtime {
                sites,
                exact,
                wildcard,
                catch_all,
                defaults: cfg.defaults.clone(),
                l4: cfg.l4.clone(),
                l4_listeners,
                listen_ports,
                proxy_protocol_from: http_pp_from,
                self_loop_warnings,
            })
        } else {
            Err(errors)
        }
    }

    /// 把**臨時覆蓋層的格子**按鍵掛到每一個 HTTP 上游身上（**M2 批 N 任務 3**，裁決 R7）。
    ///
    /// # ★ ★ ★ 為什麼是「建完再掛」而不是「建的時候就掛」
    ///
    /// 鍵的第一格是**站台名**，而建 `Upstream` 的地方是 `build_step` —— 那裡既拿不到
    /// 站台名（它只在 [`Self::build_graph`] 的循環裡），也不該為此加一個參數
    /// （`build_step` 有 5 處呼叫方，多一個參數就多 5 處「可能傳錯」）。
    /// ⇒ 圖建完之後再走一遍，此時 `Runtime` 還是本機的，拿得到 `&mut`。
    ///
    /// # ⚠ ⚠ 這裡走的是**第二份**遍歷，而它由一條判據釘住
    ///
    /// [`keyed_proxies_of`] 是只讀的（`&SiteRt`），掛格子要 `&mut` ⇒ 這一段是同一張圖的
    /// 可變走法。**兩份遍歷分家的表現是完全靜默的**：漏掉一個分支（比如不下鑽進 `handle`），
    /// 那裡的上游就悄悄留著建構點上那格私有的佔位格子，管理面改登記處對它毫無作用。
    /// ⇒ `tests/overrides.rs` 那條判據**從 [`Self::keyed_proxies`] 枚舉每一個上游**，
    /// 逐個比 `Arc::ptr_eq` —— 於是兩份走法只要有一格對不上就紅。
    /// ⛔ 別把那條判據改成「比兩格的值相等」：值相等在兩者都是預設值時恆真。
    pub(crate) fn attach_overrides(&mut self, layer: &OverrideLayer) {
        fn walk(steps: &mut [StepRt], site: &str, layer: &OverrideLayer) {
            for s in steps {
                match &mut s.body {
                    BodyRt::Proxy(t) => {
                        let id = t.id.clone();
                        for u in &mut t.upstreams {
                            u.ov = layer.slot(&OverrideKey::new(site, &id, &u.addr));
                        }
                    }
                    // ⚠ 容器裡的也要掛 —— 走法必須與 [`keyed_proxies_of`] 逐字一致。
                    BodyRt::Handle(arms) => {
                        for a in arms {
                            walk(&mut a.steps, site, layer);
                        }
                    }
                    BodyRt::Route(inner) => walk(inner, site, layer),
                    // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                    //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                    //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                    //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                    // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                    //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                    //   —— 要名單就現場 grep 這兩個枚舉名。
                    BodyRt::Tracing
                    | BodyRt::Header(_)
                    | BodyRt::Rewrite(_)
                    | BodyRt::Encode(_)
                    | BodyRt::Redir { .. }
                    | BodyRt::Respond { .. }
                    | BodyRt::Metrics
                    | BodyRt::FileServer(_)
                    | BodyRt::Cache(_) => {}
                }
            }
        }
        // ⚠ **L4 有意不在這裡**（S6）：L4 的上游沒有站台，按裁決 R6 的鍵尋址不到它們
        //   ⇒ 它們永遠拿著建構點上那格預設的空格子，落點與批 N 之前逐字相同。
        for site in &mut self.sites {
            let name = site.name.clone();
            walk(&mut site.chain, &name, layer);
            walk(&mut site.error_handler, &name, layer);
        }
    }

    /// 這份執行環境裡**全部**的覆蓋層鍵（**M2 批 N 任務 3**）。
    ///
    /// ★ 它是「懸空」（裁決 R8）與「空格子回收」（裁決 R7）唯一的判據來源：
    /// 登記處裡鍵**不在這份集合裡**的那些格子，就是現在管不到任何上游的。
    /// ⛔ 別在別處再拼一份 —— 這裡走的是 [`Self::keyed_proxies`]，與掛格子同一張圖。
    pub fn override_keys(&self) -> BTreeSet<OverrideKey> {
        let mut out = BTreeSet::new();
        for p in self.keyed_proxies() {
            for u in &p.target.upstreams {
                out.insert(OverrideKey::new(p.site, p.id, &u.addr));
            }
        }
        out
    }

    pub fn sites(&self) -> &[SiteRt] {
        &self.sites
    }

    /// 這個 host **是不是設定裡寫著的一條位址字面值**（[`SiteRt::addresses`] 之並集）。
    ///
    /// # ★ ★ 它是 `fulcrum_no_site_match_total{host}` 的封頂（G118）
    ///
    /// 那一格的值來自請求方、攻擊者可控 ⇒ 只有這個函式說真的 host 才帶真值，
    /// 其餘一律 `<other>`。**上界因此由設定定、不由訪問者定** —— 那是 G118 的全部內容。
    ///
    /// # ⚠ ⚠ 問的是「字面值」，不是「路由得到嗎」
    ///
    /// ★ 通配位址 `*.wild.example` 的子網域 `x.wild.example` 在這裡判 **false**：
    /// 它（連接埠對的話）路由得到，但它**不是一條位址字面值** —— 而通配站台下
    /// 子網域由請求方隨便寫，認它就等於把上界重新交回給了訪問者。
    /// ⇒ ⛔ **這個函式不許改用 [`Self::resolve_site`] 實作**：那條路認通配、
    /// 認連接埠備援，一改就是把剛封住的口子重新打開，而指標看起來完全正常。
    ///
    /// ⚠ 按小寫比 —— 位址字面值在 [`Runtime::build`] 裡就是折成小寫存的（R6）。
    /// ⚠ 線性掃一遍全部位址：它只跑在 421 那條路上，而位址數由設定定；
    /// ★ 在別處另存一份索引會多出一個**會過期**的副本，那正是這裡要躲開的東西。
    pub fn has_address_literal(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.sites
            .iter()
            .any(|s| s.addresses.iter().any(|a| **a == *h))
    }

    /// M1 認得但這一批沒串接的能力，按本次設定**實際用到的**篩過一遍。
    ///
    /// ★ 只報設定裡真的寫了的那些：全打出來會變成噪音，而噪音會把真的那幾條一起埋掉。
    pub fn unwired_in_use(&self, cfg: &StructuredConfig) -> Vec<(&'static str, &'static str)> {
        self.unwired_in_use_of(UNWIRED, cfg)
    }

    /// 同 [`Self::unwired_in_use`]，只是**清單由呼叫方給**。
    ///
    /// ⚠ ⚠ **按結構走，不按 JSON 文字搜** —— 文字搜法一條都認不出（鍵名在 JSON 裡叫
    /// `uri` / `dns_refresh_ms` / `fail_threshold`，而 `"log": null` 又永遠在）。
    /// ★ 一個「看起來在篩、實際上篩錯了」的清單比沒有清單更糟。
    ///
    /// # ★ ★ 產品裡只有一個呼叫方，那個參數是給**門**用的
    ///
    /// 掃描的結果最後要與清單求交 ⇒ **清單裡沒有的鍵，掃出來也看不見**：
    /// 一格掃描寫對了、寫反了、根本沒寫，三種情況的外部表現完全一樣。
    /// ⚠ 而「清單裡今天有哪幾條」隨串接進度變 ⇒ 拿 [`UNWIRED`] 本身當判據的門
    /// **會隨著能力一條條串接而啞掉，且啞掉的那一刻不紅**。
    /// ⇒ `tests/unwired_contract.rs` 從這裡喂一張形狀相同、內容與串接進度無關的合成表，
    /// 於是判據不相依今天恰好哪幾條沒串接。
    pub fn unwired_in_use_of(
        &self,
        table: &[(&'static str, &'static str)],
        cfg: &StructuredConfig,
    ) -> Vec<(&'static str, &'static str)> {
        let mut used: std::collections::BTreeSet<&'static str> = Default::default();
        // ★ ★ 全域選項這一半：拿真表看，它**一條也報不出來** —— 而那不等於沒在掃。
        //   `admin` 在批 9 就串接了 ⇒ 它不在 [`UNWIRED`] 裡 ⇒ 下面這一格認不認得它，
        //   求交之後都是空的。⚠ **一段誰也看不見的程式碼與一段不存在的程式碼無從分辨**，
        //   這裡的註解上一版就是這麼爛掉的：它說「全域那一半有人守著」，
        //   而那句話在 `admin` 串接的同一刻起就不再成立，沒有任何東西會為此變紅。
        //   ⇒ 守它的是合成表那條判據，見本函式的類型文件。
        // ⚠ 這一格留著不刪：全域選項遲早會有「DSL 認得、執行環境不做」的那一條，
        //   那時這半邊是它唯一的出口 —— 而到那天補一段從來沒存在過的掃描，沒人會想起來。
        if cfg.global.admin.is_some() {
            used.insert("admin");
        }
        for s in &cfg.sites {
            if s.log.is_some() {
                used.insert("log");
            }
            // ★ TLS 那一格拆成三條，按站台**實際寫了什麼**判，不按「連接埠上有 TLS」判。
            //   ⚠ 取後者的話，一個明明給了 PEM 路徑（已串接）的站台也會被報成未串接。
            let needs_tls = s.addresses.iter().any(|a| a.scheme == "https");
            if needs_tls {
                match &s.tls.mode {
                    TlsMode::Automatic => {
                        // ⚠ **假警報會訓練人忽略這張表**，連帶把真的那幾條一起埋掉。
                        //   `TlsMode::Automatic` 這一支現在一條都不報（自動簽發全串接了）；
                        //   ★ 留一個空分支而不是刪掉 `match`，是讓下一個人看見
                        //   這裡的判定是被做完了、不是被忘了。
                    }
                    TlsMode::Internal => {
                        used.insert("tls_internal");
                    }
                    // `tls <cert> <key>` 與 `http://` 都已串接
                    TlsMode::Manual { .. } | TlsMode::Off => {}
                }
            }
            if s.tls.on_demand {
                used.insert("on_demand");
            }
            let mut stack: Vec<&Step> = s.chain.iter().chain(s.error_handler.iter()).collect();
            while let Some(st) = stack.pop() {
                match &st.body {
                    StepBody::Encode { .. } => {
                        used.insert("encode");
                    }
                    StepBody::Tracing => {
                        used.insert("tracing");
                    }
                    StepBody::ReverseProxy { .. } => {
                        // ★ 批 11：`health_uri` 接上了，這一條刪掉。
                        // ★ ★ G136：`passive_fail` 也接上了 ⇒ 這一格連同 `UNWIRED` 裡那一行
                        //   一起刪掉。⚠ 留著就是一條**假警告**，而假警告會訓練人忽略整張表
                        //   （`dns_refresh` / `encode` / `health_uri` / `weight` 都是這麼翻的方向）。
                        // ★ ★ 批 N 任務 2：`weight` **串接了**（四種 `lb_policy` 全部按累積
                        //   權重挑）⇒ 這裡原先那一格連同 `UNWIRED` 裡那一行一起刪掉。
                        //   ⚠ 留著就是一條假警告，而假警告會訓練人忽略整張表
                        //   （`dns_refresh` / `encode` / `health_uri` 都是這麼翻的方向）。
                        // ★ 批 10：這裡原先無條件報一條 `dns_refresh`
                        //   （它有預設值 30s，等於對每條 reverse_proxy 都承諾了定期重解析）。
                        //   現在它**接上了**，所以這一條刪掉 —— 留著就是一條假警告，
                        //   而假警告會訓練人忽略整張表。
                    }
                    StepBody::Route { steps } => stack.extend(steps.iter()),
                    StepBody::Handle { arms } => {
                        for a in arms {
                            stack.extend(a.steps.iter());
                        }
                    }
                    // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                    //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                    //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                    //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                    // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                    //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                    //   —— 要名單就現場 grep 這兩個枚舉名。
                    // ★ 這一格的空 body 說的是兩件事：**既不是要上報的能力，也不裝步驟**。
                    //   前半今天成立，後半會隨新變體變假 ⇒ 讓編譯器逼下一個人逐格表態一次。
                    StepBody::Header { .. }
                    | StepBody::Rewrite { .. }
                    | StepBody::Cache { .. }
                    | StepBody::Redir { .. }
                    | StepBody::Respond { .. }
                    | StepBody::Metrics
                    | StepBody::FileServer { .. } => {}
                }
            }
        }
        table
            .iter()
            .filter(|(k, _)| used.contains(k))
            .copied()
            .collect()
    }

    /// 按 `(Host, 本地端口)` 找站台。
    ///
    /// 順序：精確 → 通配（長後綴優先）→ 連接埠備援。都不中回傳 `None`
    /// → 呼叫方回 [`Defaults::no_site_match`]（**421**，G63）。
    ///
    /// ★ 回傳值的第三項是**命中的是該站台第幾條位址**（下標進 [`SiteRt::addresses`]）——
    /// G121 要指標標籤取「實際匹配到的那條位址字面值」而不是站台的第一條位址，
    /// 這個下標是那條字面值的唯一來源。
    pub fn resolve_site(&self, host: &str, port: u16) -> Option<(usize, SiteMatch, usize)> {
        // ★ 已是小寫（絕大多數請求）就直接借，⛔ 不為「轉小寫」白分配一次（判據 `tests/route_alloc.rs`）。
        let h: Cow<'_, str> = if host.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(host.to_ascii_lowercase())
        } else {
            Cow::Borrowed(host)
        };
        let h: &str = &h;
        if let Some(&(i, ai)) = self.exact.get(&port).and_then(|by_host| by_host.get(h)) {
            return Some((i, SiteMatch::Exact, ai));
        }
        for (suffix, p, i, ai) in &self.wildcard {
            // ★ ★ **只吃一層**（D18 / G66）：`*.example.com` 覆蓋 `a.example.com`，
            //   **不**覆蓋 `example.com` 自己，也**不**覆蓋 `a.b.example.com`。
            //   ⚠ 這裡原先是 `ends_with` 的後綴匹配，而證書那側按 RFC 6125 只吃一層，
            //   於是 `a.b.example.com` **被路由到通配站台、然後拿不到證書**——
            //   現場是一次握手失敗，設定裡沒有一行看得出問題。
            //   ★ 判據只有一份，就在 `fulcrum_config::host`：兩邊都調它，
            //   **分家變成結構上做不到的事**，而不是靠一條契約測試碰巧發現。
            if *p == port && wildcard_covers(suffix, h) {
                return Some((*i, SiteMatch::Wildcard, *ai));
            }
        }
        self.catch_all
            .get(&port)
            .map(|&(i, ai)| (i, SiteMatch::CatchAll, ai))
    }

    /// 走一次完整路由。
    pub fn route<'r>(&'r self, req: &RequestCtx<'_>) -> Option<Routed<'r>> {
        let (idx, how, addr_idx) = self.resolve_site(req.host, req.port)?;
        let site = &self.sites[idx];
        let mut w = Walk {
            site,
            base: *req,
            path: Cow::Borrowed(req.path),
            response_headers: Vec::new(),
            captures: Vec::new(),
            rewritten: false,
            terminal_order: None,
            encodings: Vec::new(),
            cache: None,
            outcome: None,
        };
        w.run(&site.chain);
        let Walk {
            response_headers,
            captures,
            rewritten,
            path,
            outcome,
            terminal_order,
            encodings,
            cache,
            ..
        } = w;
        Some(Routed {
            site,
            site_match: how,
            site_addr: site.addresses[addr_idx].clone(),
            response_headers,
            rewritten_path: if rewritten {
                Some(path.into_owned())
            } else {
                None
            },
            captures,
            terminal_order,
            requested_encodings: encodings,
            cache,
            outcome: outcome.unwrap_or(Outcome::NoRouteMatch),
        })
    }

    /// `handle_errors` 給的錯誤頁。`None` = 用內建預設（G63）。
    ///
    /// ⚠ M1 的最小形態：只認 `handle_errors { respond <status> [body] }`。
    /// 更復雜的錯誤頁（走 reverse_proxy / file_server）是後面的事——
    /// 而這裡回傳 `None` 時呼叫方會落到內建預設，**不會靜靜地什麼都不做**。
    pub fn error_page<'r>(&'r self, site: &'r SiteRt) -> Option<ErrorPage<'r>> {
        site.error_handler.iter().find_map(|s| match &s.body {
            BodyRt::Respond { status, body } => Some(ErrorPage {
                status: *status,
                body: body.as_ref(),
            }),
            _ => None,
        })
    }
}

/// `handle_errors` 裡那條 `respond`。
pub struct ErrorPage<'r> {
    pub status: u16,
    pub body: Option<&'r Template>,
}

/// 一次路由的可變狀態。
struct Walk<'r, 'q> {
    site: &'r SiteRt,
    /// 請求的原始視圖。`path` 欄位不直接用——用 [`Self::ctx`]。
    base: RequestCtx<'q>,
    /// 當前路徑（可能已被 `rewrite` 改過）。
    path: Cow<'q, str>,
    response_headers: Vec<&'r HeaderOpRt>,
    captures: Vec<String>,
    rewritten: bool,
    terminal_order: Option<u16>,
    encodings: Vec<String>,
    cache: Option<&'r CacheRt>,
    outcome: Option<Outcome<'r>>,
}

impl<'r, 'q> Walk<'r, 'q> {
    /// 當前的請求視圖。★ `rewrite` 之後**後續匹配器看到的是新路徑**——
    /// 這是 Caddy 的語義，也是唯一自洽的一種：否則 `rewrite` 就成了只影響上游、
    /// 不影響本機路由的半吊子操作。
    fn ctx(&self) -> RequestCtx<'_> {
        RequestCtx {
            path: &self.path,
            ..self.base
        }
    }

    fn matches(&mut self, key: Option<&MatcherKey>) -> bool {
        let Some(key) = key else { return true };
        match key {
            MatcherKey::Path(g) => glob::glob_match(g, &self.path),
            MatcherKey::Named(name) => match self.site.matchers.get(name) {
                // 建置期已經保證引用得到（設定層也查過一遍）；取不到時**判不命中**。
                None => false,
                Some(m) => {
                    let ctx = self.ctx();
                    let mut caps = Vec::new();
                    let hit = m.matches(&ctx, &mut caps);
                    if hit && !caps.is_empty() {
                        self.captures = caps;
                    }
                    hit
                }
            },
        }
    }

    /// 走一段鏈。**陣列順序就是執行順序**——站台頂層那份在編譯期已按順序表排好，
    /// `route { … }` 裡那份是書寫順序（G49 的逃生口）。回傳 `true` 表示已經終結。
    fn run(&mut self, steps: &'r [StepRt]) -> bool {
        for step in steps {
            if self.outcome.is_some() {
                return true;
            }
            if !self.matches(step.matcher.as_ref()) {
                continue;
            }
            match &step.body {
                BodyRt::Tracing => {}
                BodyRt::Header(ops) => self.response_headers.extend(ops.iter()),
                BodyRt::Rewrite(t) => {
                    let ctx = self.ctx();
                    let new = t.expand(&ctx, &ResponseCtx::default(), &self.captures, now());
                    self.path = Cow::Owned(new);
                    self.rewritten = true;
                }
                // ⚠ 記下來但不施加，見 UNWIRED。帶出去比悄悄丟掉好。
                BodyRt::Encode(list) => self.encodings = list.clone(),
                // ★ 中介軟體：只記一筆，**不終結**。
                //   ⚠ 一條鏈上寫兩個 `cache` 時後一個生效 —— 與 `encode` 同款，
                //   而順序表保證它們在同一位置，所以這隻可能來自 `route { … }`。
                BodyRt::Cache(c) => self.cache = Some(c),
                BodyRt::Handle(arms) => {
                    // ★ 互斥：只有**第一個**匹配的分支會執行。
                    for arm in arms {
                        if self.matches(arm.matcher.as_ref()) {
                            if self.run(&arm.steps) {
                                return true;
                            }
                            break;
                        }
                    }
                }
                BodyRt::Route(inner) => {
                    // ★ 保序容器：塊內按書寫順序。
                    if self.run(inner) {
                        return true;
                    }
                }
                BodyRt::Redir { to, code } => {
                    self.terminal_order = Some(step.order);
                    self.outcome = Some(Outcome::Redirect { to, code: *code });
                    return true;
                }
                BodyRt::Respond { status, body } => {
                    self.terminal_order = Some(step.order);
                    self.outcome = Some(Outcome::Respond {
                        status: *status,
                        body: body.as_ref(),
                    });
                    return true;
                }
                BodyRt::Proxy(t) => {
                    self.terminal_order = Some(step.order);
                    self.outcome = Some(Outcome::Proxy(t));
                    return true;
                }
                BodyRt::FileServer(fs) => {
                    self.terminal_order = Some(step.order);
                    self.outcome = Some(Outcome::FileServer(fs));
                    return true;
                }
                BodyRt::Metrics => {
                    self.terminal_order = Some(step.order);
                    self.outcome = Some(Outcome::Metrics);
                    return true;
                }
            }
        }
        self.outcome.is_some()
    }
}

fn now() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

fn build_matcher_key(m: &Option<MatcherRef>) -> Option<MatcherKey> {
    m.as_ref().map(|m| match m {
        MatcherRef::Named(n) => MatcherKey::Named(n.clone()),
        MatcherRef::Path(p) => MatcherKey::Path(p.clone()),
    })
}

fn build_step(
    at: &str,
    st: &Step,
    matchers: &BTreeMap<String, CompiledMatcher>,
    errors: &mut Vec<BuildError>,
) -> Option<StepRt> {
    let key = build_matcher_key(&st.matcher);
    // ★ 結構化層是公開入口：引用了不存在的匹配器要在**這裡**也被拒。
    if let Some(MatcherKey::Named(n)) = &key
        && !matchers.contains_key(n)
    {
        errors.push(BuildError::new(at, format!("引用了没定义的匹配器 @{n}")));
        return None;
    }

    let body = match &st.body {
        StepBody::Tracing => BodyRt::Tracing,
        StepBody::Header { ops } => BodyRt::Header(ops.iter().map(HeaderOpRt::build).collect()),
        StepBody::Rewrite { to } => BodyRt::Rewrite(Template::parse(to)),
        StepBody::Encode { encodings } => BodyRt::Encode(encodings.clone()),
        StepBody::Cache {
            ttl_ms,
            max_size_bytes,
            capacity_bytes,
            disk_dir,
        } => {
            // ⚠ ⚠ 結構化設定是**公開入口**（G11）：DSL 那側的「必須絕對路徑」
            //   在這裡必須再有一道，否則一份手寫 JSON 就能繞過去 ——
            //   而繞過去之後的現場是「快取目錄建在了 /proc/self/cwd 底下」，
            //   ⚠ **一個字的回報錯誤都不會有**。★ 與 `file_server` 的 root 同款。
            if let Some(d) = disk_dir
                && !d.starts_with('/')
            {
                errors.push(BuildError::new(
                    at,
                    format!("`cache` 的 disk 必须是绝对路径，`{d}` 不是"),
                ));
                return None;
            }
            BodyRt::Cache(CacheRt {
                ttl_ms: *ttl_ms,
                // ★ 預設值在**這一處**算完，之後誰都不必再想它是什麼
                //   （與 `FileServerRt` 的 index/hide 同一條紀律）。
                max_size_bytes: max_size_bytes
                    .unwrap_or(fulcrum_config::directive::CACHE_DEFAULT_MAX_SIZE_BYTES),
                capacity_bytes: capacity_bytes
                    .unwrap_or(fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES),
                disk_dir: disk_dir.clone(),
            })
        }
        StepBody::FileServer {
            root,
            browse,
            index,
            follow_symlinks,
            hide,
            hide_defaults,
            precompressed,
        } => {
            // ⚠ ⚠ 結構化設定是**公開入口**（G11）：DSL 那側的兩道檢查
            // （root 必填、必須絕對）在這裡必須各再有一道，否則一份 JSON
            // 就能繞過去，而繞過去之後的現場只是「怎麼全是 404」。
            let root = match root {
                Some(r) if r.starts_with('/') => r.clone(),
                Some(r) => {
                    errors.push(BuildError::new(
                        at,
                        format!("`file_server` 的 root 必须是绝对路径，`{r}` 不是"),
                    ));
                    return None;
                }
                None => {
                    errors.push(BuildError::new(at, "`file_server` 少了必填的 root"));
                    return None;
                }
            };
            let mut hide_final: Vec<String> = if *hide_defaults {
                fulcrum_config::directive::HIDE_DEFAULTS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect()
            } else {
                Vec::new()
            };
            hide_final.extend(hide.iter().cloned());
            hide_final.sort();
            hide_final.dedup();
            BodyRt::FileServer(FileServerRt {
                root,
                browse: *browse,
                index: if index.is_empty() {
                    vec!["index.html".to_string()]
                } else {
                    index.clone()
                },
                follow_symlinks: *follow_symlinks,
                hide: hide_final,
                // ★ 去重並按**設定裡的書寫順序**保留：挑旁檔案時走的是用戶端的
                //   `Accept-Encoding` 順序，所以這裡的順序不參與決策 ——
                //   ⚠ 但重複項會讓 `any()` 白跑幾次，而且裝載紀錄裡會印兩遍。
                precompressed: {
                    let mut v = precompressed.clone();
                    v.dedup();
                    v
                },
            })
        }
        StepBody::Handle { arms } => {
            let mut out = Vec::new();
            for arm in arms {
                let akey = build_matcher_key(&arm.matcher);
                if let Some(MatcherKey::Named(n)) = &akey
                    && !matchers.contains_key(n)
                {
                    errors.push(BuildError::new(at, format!("引用了没定义的匹配器 @{n}")));
                    continue;
                }
                let steps = arm
                    .steps
                    .iter()
                    .filter_map(|s| build_step(at, s, matchers, errors))
                    .collect();
                out.push(ArmRt {
                    matcher: akey,
                    steps,
                });
            }
            BodyRt::Handle(out)
        }
        StepBody::Route { steps } => BodyRt::Route(
            steps
                .iter()
                .filter_map(|s| build_step(at, s, matchers, errors))
                .collect(),
        ),
        StepBody::Redir { to, code } => BodyRt::Redir {
            to: Template::parse(to),
            code: *code,
        },
        StepBody::Respond { status, body } => BodyRt::Respond {
            status: *status,
            body: body.as_deref().map(Template::parse),
        },
        StepBody::Metrics => BodyRt::Metrics,
        StepBody::ReverseProxy {
            id,
            upstreams,
            lb_policy,
            health,
            passive,
            header_up,
            header_down,
            transport,
            tls_insecure_skip_verify,
            ..
        } => {
            // ★ ★ **`id` 的取值域在這裡再查一遍**（G11：結構化層是公開入口）：
            //   DSL 那邊 `FUL-DSL-0043` 擋過一次，而一份**在處理程序裡手搓**的
            //   `StructuredConfig`（`POST /load` 收的 JSON）那一道**一次都不跑**。
            //   ⚠ ⚠ 這一格的失效形態是**裝得上**：`id "<none>"` 會在 `/stats` 與
            //   覆蓋層的鍵裡**冒充**本儲存庫的備援記號（`<other>` / `<none>` / `<unknown>` /
            //   `<undeclared>` 全靠尖括號在真值裡不可能出現），而沒有任何東西回報錯誤。
            //   ⛔ 不在這裡再寫一遍那個字元集：判據與「合法的長什麼樣」那句話
            //   各只有一份（`is_valid_proxy_id` / `proxy_id_shape`）。
            //   ⚠ `None` 是「這條沒寫 id」，**不是**不合法 —— `id` 選填（G125）。
            //   ★ 判在最前面且**不 `return None`**：與 `weight` 那條一樣一次報全，
            //   否則「lb_policy 也寫錯了」會把這一條吞掉。
            if let Some(name) = id
                && !fulcrum_config::model::is_valid_proxy_id(name)
            {
                errors.push(BuildError::new(
                    at,
                    format!(
                        // ⚠ `{:?}`：帶換行的 id 會把這一行裝載錯誤在人眼裡斷成兩行。
                        "`reverse_proxy` 的 `id` {name:?} 不在取值域里——{}",
                        fulcrum_config::model::proxy_id_shape()
                    ),
                ));
            }
            let policy = match LbPolicy::parse(lb_policy) {
                Some(p) => p,
                None => {
                    errors.push(BuildError::new(
                        at,
                        format!("未知的 lb_policy `{lb_policy}`"),
                    ));
                    return None;
                }
            };
            if upstreams.is_empty() {
                errors.push(BuildError::new(at, "reverse_proxy 没有上游"));
                return None;
            }
            let mut ups = Vec::new();
            for spec in upstreams {
                let u = &spec.addr;
                // ★ ★ **值域在這裡再查一遍**（G11：結構化層是公開入口）：
                //   DSL 那邊 `FUL-DSL-0040` 擋過一次、`UpstreamSpec` 的反序列化也擋過一次，
                //   而一份**在處理程序裡手搓**的 `StructuredConfig` 兩道都不經過。
                //   ⚠ 權重 0 的表現不是回報錯誤而是**這臺機器永遠不被選中**，
                //   全是 0 時更是連總權重都沒有 —— 靜默，且現場只看得到「流量不均」。
                //   ⛔ 不在這裡再寫一個 `65535`：值域常數只有一份。
                if !(fulcrum_config::model::MIN_UPSTREAM_WEIGHT
                    ..=fulcrum_config::model::MAX_UPSTREAM_WEIGHT)
                    .contains(&spec.weight)
                {
                    errors.push(BuildError::new(
                        at,
                        format!(
                            "上游 `{u}` 的权重 {} 不在 [{}, {}] 内",
                            spec.weight,
                            fulcrum_config::model::MIN_UPSTREAM_WEIGHT,
                            fulcrum_config::model::MAX_UPSTREAM_WEIGHT
                        ),
                    ));
                }
                // ★ 上游位址必須**在裝載時**就能被解析成「主機 + 連接埠」。
                //   `10.0.0.1:8080` / `backend:80`；缺連接埠按 transport 補預設。
                match normalize_upstream(u, transport) {
                    Ok(addr) => {
                        // ★ IP 字面值當場填好：它不需要 DNS，也永遠不會變。
                        //   ⚠ **網域一律留空** —— `Runtime::build` 是 `fulcrum validate`
                        //   走的那條路，而 validate 的全部價值在於**離線**就能說話。
                        //   在這裡做 DNS 會讓它變成一個要聯網的命令。
                        let literal: Vec<std::net::SocketAddr> =
                            addr.parse::<std::net::SocketAddr>().into_iter().collect();
                        ups.push(Upstream {
                            // ★ ★ **歸一化之後**那個串，它同時是覆蓋層鍵的第三格。
                            //   ⛔ 設定那一行上的原文 token **有意不留檔**：任務 2.8 存過
                            //   一格 `raw_addr`，唯一的消費者是撞鍵那條拒絕訊息，
                            //   而任務 2.9 把拒絕拆掉了 ⇒ 見 `Upstream::addr` 的文件。
                            addr,
                            // ★ ★ 設定權重在這裡進執行環境（M2 批 N 任務 2，裁決 R4/R5）。
                            weight: spec.weight,
                            resolved: std::sync::RwLock::new(literal),
                            inflight: AtomicUsize::new(0),
                            // ★ 初值健康。理由見 `Upstream::healthy` 上那張表。
                            healthy: std::sync::atomic::AtomicBool::new(true),
                            // ★ G136：初值閉合（`open_until = 0`）。沒配 `passive_fail`
                            //   的話這一格一次都不會被寫。
                            passive: PassiveState::default(),
                            // ⚠ ⚠ **這裡放的是一格私有的佔位格子**，真正那一格由
                            //   `Runtime::attach_overrides` 在圖建完之後按鍵掛上來 ——
                            //   本函式拿不到站台名（它只在 `Runtime::build` 的循環裡）。
                            //   ★ 「漏掛」是一種**完全靜默**的失敗：管理面改登記處那一格
                            //   對這個上游毫無作用，而沒有任何東西會回報錯誤。擋它的是
                            //   `tests/overrides.rs` 那條 `Arc::ptr_eq` 判據。
                            ov: std::sync::Arc::new(UpstreamOverride::default()),
                        })
                    }
                    Err(m) => errors.push(BuildError::new(at, format!("上游 `{u}`：{m}"))),
                }
            }
            // ★ 只有寫了 `health_uri` 才有健康檢查。其餘 `health_*` 都有預設值，
            //   所以「配了 health_interval 卻沒配 health_uri」是**不探測**，
            //   而不是「按預設路徑探」——後者會去打一個使用者從沒說過的路徑。
            let health = health.uri.as_ref().and_then(|uri| {
                match StatusPattern::parse(&health.status) {
                    Some(status) => Some(HealthPolicy {
                        uri: uri.clone(),
                        interval: std::time::Duration::from_millis(health.interval_ms),
                        timeout: std::time::Duration::from_millis(health.timeout_ms),
                        status,
                    }),
                    None => {
                        // ⚠ 這裡**必須回報錯誤而不是回落成預設的 `2xx`**：
                        //   一個「看起來在按 5xx 判、其實在按 2xx 判」的健康檢查
                        //   會把所有上游一起判死，而設定裡一個字都沒錯。
                        errors.push(BuildError::new(
                            at,
                            format!(
                                "health_status `{}` 不认识：写一个状态码（`200`）或一族（`2xx`）",
                                health.status
                            ),
                        ));
                        None
                    }
                }
            });
            BodyRt::Proxy(ProxyTarget {
                // ★ ★ id 在這裡進執行環境（M2 批 N 任務 2.8，裁決 R6）。
                //   沒寫就是空串 —— 見 `ProxyTarget::id` 那段：空串是鍵裡的一格，不是「跳過」。
                id: id.clone().unwrap_or_default(),
                upstreams: ups,
                policy,
                header_up: header_up.iter().map(HeaderOpRt::build).collect(),
                header_down: header_down.iter().map(HeaderOpRt::build).collect(),
                tls: transport == "https",
                tls_insecure_skip_verify: *tls_insecure_skip_verify,
                health,
                // ★ ★ G136：`fail_threshold` 是 `None` ⇒ 整條策略是 `None`（不寫就是關閉）。
                //   ⚠ 視窗與冷卻在**設定層**就已經落實成具體數字，這裡⛔ 不再寫一份預設。
                passive: passive.fail_threshold.map(|threshold| PassivePolicy {
                    threshold,
                    window_ms: passive.window_ms,
                    cooldown_ms: passive.cooldown_ms,
                }),
                last_probe: std::sync::Mutex::new(None),
                cursor: AtomicUsize::new(0),
                seed: RandomState::new().hash_one("fulcrum-lb"),
            })
        }
    };

    Some(StepRt {
        order: st.order,
        matcher: key,
        body,
    })
}

/// 逐個站台檢查 `reverse_proxy` 的上游有沒有指回樞衡自己（**M2 批 G 從回落層挪過來**）。
///
/// 自環的現場：請求打回自己 → 再轉發 → 再打回自己，直到 fd 或記憶體耗盡，
/// 而紀錄裡只有源源不斷的**正常**轉發記錄。
///
/// ⚠ **回傳話、不自己列印**（本 crate 有意不相依 `log`）⇒ 由資料面的 `log_load_summary` 去說。
/// ⚠ **是 warning 不是 error**：指回自己的 `reverse_proxy` 可能是有意的
/// （第二趟落到另一個站台上而終止）⇒ 說出來，但不替使用者拒絕。
/// 走遍一批站台，把全部 `cache` 步驟連同站台名收出來。
///
/// ⚠ 是**自由函式**而不是 `Runtime` 的方法：那條「快取後端全圖只能有一個」的檢查
/// 必須在 `Runtime` 還沒建出來時跑。抄一份 walk 過去，就等於讓「檢查過的那張圖」
/// 與「跑起來的那張圖」有機會不是同一件事。
fn cache_settings_of(sites: &[SiteRt]) -> Vec<(&str, &CacheRt)> {
    fn walk<'a>(steps: &'a [StepRt], site: &'a str, out: &mut Vec<(&'a str, &'a CacheRt)>) {
        for s in steps {
            match &s.body {
                BodyRt::Cache(c) => out.push((site, c)),
                BodyRt::Handle(arms) => {
                    for a in arms {
                        walk(&a.steps, site, out);
                    }
                }
                BodyRt::Route(inner) => walk(inner, site, out),
                // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                //   —— 要名單就現場 grep 這兩個枚舉名。
                BodyRt::Tracing
                | BodyRt::Header(_)
                | BodyRt::Rewrite(_)
                | BodyRt::Encode(_)
                | BodyRt::Redir { .. }
                | BodyRt::Respond { .. }
                | BodyRt::Metrics
                | BodyRt::Proxy(_)
                | BodyRt::FileServer(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    for site in sites {
        walk(&site.chain, &site.name, &mut out);
        walk(&site.error_handler, &site.name, &mut out);
    }
    out
}

/// 這串步驟裡（含嵌套的 `handle` / `route`）有沒有 `reverse_proxy`（G163，[`Runtime::proxy_ports`]）。
fn steps_have_proxy(steps: &[StepRt]) -> bool {
    steps.iter().any(|s| match &s.body {
        BodyRt::Proxy(_) => true,
        BodyRt::Handle(arms) => arms.iter().any(|a| steps_have_proxy(&a.steps)),
        BodyRt::Route(inner) => steps_have_proxy(inner),
        // ⚠ 不寫 `_`：理由同 `Runtime::file_servers` 裡那段 —— 新增一個裝步驟的容器變體時，
        //   要編譯期窮舉逼著來改這裡，否則那個容器裡的 `reverse_proxy` 會被當成沒有。
        BodyRt::Tracing
        | BodyRt::Header(_)
        | BodyRt::Rewrite(_)
        | BodyRt::Encode(_)
        | BodyRt::Redir { .. }
        | BodyRt::Respond { .. }
        | BodyRt::Metrics
        | BodyRt::FileServer(_)
        | BodyRt::Cache(_) => false,
    })
}

fn self_loop_warnings(sites: &[SiteRt], listen_ports: &[(u16, bool)]) -> Vec<String> {
    fn walk(steps: &[StepRt], site: &str, lp: &[(u16, bool)], out: &mut Vec<String>) {
        for s in steps {
            match &s.body {
                BodyRt::Proxy(t) => {
                    for u in &t.upstreams {
                        if let Some(port) = self_loop_port(&u.addr, lp) {
                            out.push(format!(
                                "站点 {site} 的 reverse_proxy 指向 `{}`，而端口 {port}                                  正是枢衡自己在监听的 —— 请求会被转发回自己。                                 若这是有意的（例如自己终止 TLS 再回自己的明文口），忽略这条；                                 否则它是一个无限循环，而日志里只会看到源源不断的正常转发。",
                                u.addr
                            ));
                        }
                    }
                }
                BodyRt::Handle(arms) => {
                    for a in arms {
                        walk(&a.steps, site, lp, out);
                    }
                }
                BodyRt::Route(inner) => walk(inner, site, lp, out),
                // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                //   —— 要名單就現場 grep 這兩個枚舉名。
                // ★ 這一處漏下鑽的後果就是本函式開頭描述的那個現場：容器裡的
                //   `reverse_proxy` 指回自己，而一句警告都沒有。
                BodyRt::Tracing
                | BodyRt::Header(_)
                | BodyRt::Rewrite(_)
                | BodyRt::Encode(_)
                | BodyRt::Redir { .. }
                | BodyRt::Respond { .. }
                | BodyRt::Metrics
                | BodyRt::FileServer(_)
                | BodyRt::Cache(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    for site in sites {
        walk(&site.chain, &site.name, listen_ports, &mut out);
        walk(&site.error_handler, &site.name, listen_ports, &mut out);
    }
    out
}

/// 把上游寫法歸一成 `host:port`。
/// 這個已規範化的位址是不是指回樞衡自己？是的話回傳那個連接埠。
///
/// ★ **判據要窄，寧可漏報也不許誤報**（誤報會讓一份合法設定起不來，漏報只是一個
/// 能被紀錄看見的循環）⇒ 兩條同時成立才判：① 主機是本機形式；② 連接埠在本處理程序的監聽集裡。
/// ⚠ 第 ② 條單獨不成立 —— 別的機器可以用同一個連接埠號。
pub fn self_loop_port(addr: &str, listen_ports: &[(u16, bool)]) -> Option<u16> {
    let (host, port) = addr.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let local = matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "0.0.0.0" | "::1" | "[::1]"
    );
    if local && listen_ports.iter().any(|(p, _)| *p == port) {
        Some(port)
    } else {
        None
    }
}

/// 給一組 L4 上游建一個 [`ProxyTarget`]。一個都建不出來時回 `None`。
///
/// ★ 備援與每條 `sni` / `alpn` 規則**共用它** —— 於是「上游怎麼規範化、
/// IP 字面值什麼時候填進槽、用哪種 `lb_policy`」這幾件事只有一份實作。
/// ⚠ 它有意收 `&mut Vec<BuildError>` 而不是自己回傳錯誤：裝載期的判據是**一次報全**
/// （與 G51 那條診斷紀律同源），中途 `?` 出去只會報第一條。
fn build_l4_target(
    raw: &[String],
    at: &str,
    what: &str,
    errors: &mut Vec<BuildError>,
) -> Option<ProxyTarget> {
    let mut ups = Vec::new();
    for u in raw {
        match normalize_l4_upstream(u) {
            Ok(addr) => {
                // ★ IP 字面值當場填好：它不需要 DNS，也永遠不會變。
                //   ⚠ 網域一律留空 —— `fulcrum validate` 必須離線就能說話。
                let literal: Vec<std::net::SocketAddr> =
                    addr.parse::<std::net::SocketAddr>().into_iter().collect();
                ups.push(Upstream {
                    addr,
                    // ★ ★ **L4 的上游權重恆為 1**（計畫 §2 的 S2）：`l4` 塊裡沒有
                    //   `lb_policy` 的位置，也就沒有權重的位置 —— 給一個使用者寫不出來的
                    //   旋鈕留一格預設值，與給它留介面是兩回事。
                    //   ⇒ 這條路上 `weighted_slot` 每一段寬度都是 1，落點與批 N 之前逐字相同。
                    weight: fulcrum_config::model::DEFAULT_UPSTREAM_WEIGHT,
                    resolved: std::sync::RwLock::new(literal),
                    inflight: AtomicUsize::new(0),
                    healthy: std::sync::atomic::AtomicBool::new(true),
                    // ★ ★ G136：L4 這一格**恆為初值**。`l4` 塊裡沒有 `passive_fail` 的位置
                    //   ⇒ 那條路上 `ProxyTarget::passive` 恆為 `None`，`passive_ok()` 恆 true。
                    //   ⚠ 但 L4 **共享同一個篩子**：站台那條路把某個上游熔斷了，
                    //   那是另一個 `ProxyTarget`、另一組 `Upstream` —— 兩邊不共用物件。
                    passive: PassiveState::default(),
                    // ★ ★ **L4 這一格是最終值，永遠不會被掛上別的**（批 N 任務 3，S6）：
                    //   `attach_overrides` 只走 `sites` 那條路，而 L4 的上游沒有站台 ——
                    //   按裁決 R6 的鍵根本尋址不到它們。⇒ 這條路上 `is_disabled()` 恆 false、
                    //   有效權重恆等於設定權重（也就是 1），落點與批 N 之前逐字相同。
                    ov: std::sync::Arc::new(UpstreamOverride::default()),
                });
            }
            Err(m) => errors.push(BuildError::new(at, format!("{what}上游 `{u}`：{m}"))),
        }
    }
    if ups.is_empty() {
        return None;
    }
    Some(ProxyTarget {
        // ★ ★ **L4 的 id 恆為空串**（M2 批 N 任務 2.8）：`l4` 塊裡沒有 `reverse_proxy`，
        //   也就沒有 `id` 的位置；L4 的上游沒有站台，按 R6 的鍵根本尋址不到它們。
        //   ⇒ 覆蓋層那張鍵的清單隻走**站台**那條路（`keyed_proxies_of` /
        //     `proxy_key_fanout_of` 都只收 `SiteRt`），L4 這一格因此永遠不進去 ——
        //     由 `tests/l4.rs` 一條判據釘著。
        id: String::new(),
        upstreams: ups,
        // ★ ★ G136：**L4 那條路上恆為 `None`** —— `l4` 塊裡沒有 `passive_fail` 的位置。
        //   ⚠ 給一個使用者寫不出來的旋鈕留一格預設值，與給它留介面是兩回事
        //   （與上面 `weight` 恆為 1 那條同一理由）。
        passive: None,
        // ★ M2 只給輪詢：DSL 的 `l4` 塊裡**沒有 `lb_policy` 的位置**（dsl-reference §4.5），
        //   而給一個使用者寫不出來的旋鈕留介面，等於假裝它可配。
        policy: LbPolicy::RoundRobin,
        header_up: Vec::new(),
        header_down: Vec::new(),
        tls: false,
        tls_insecure_skip_verify: false,
        health: None,
        last_probe: std::sync::Mutex::new(None),
        cursor: AtomicUsize::new(0),
        seed: 0,
    })
}

/// 拆 `l4` 的監聽位址：`:3306` 或 `127.0.0.1:3306`。
///
/// 回傳 `(主机部分, 端口)`；主機為 `None` 表示寫的是 `:3306`，
/// 綁哪個位址交給 `serve --bind-host`（與 HTTP 那邊同一條口徑）。
///
/// ⚠ **連接埠必須寫**：L4 上沒有「預設連接埠」這回事 —— HTTP 那邊缺連接埠能補 80/443，
/// 是因為 scheme 說了算，而一個裸 TCP 監聽器沒有 scheme。
pub fn parse_l4_listen(raw: &str) -> Result<(Option<String>, u16), &'static str> {
    if raw.is_empty() {
        return Err("是空的");
    }
    if raw.contains('/') {
        return Err("不能带路径或 scheme——写成 `:3306` 或 `127.0.0.1:3306`");
    }
    // ★ 與上游那把尺子保持一致：IPv6 字面值整體還沒支援，
    //   在這裡明確說出來，好過讓它在 `rsplit_once(':')` 上被拆得面目全非。
    if raw.contains('[') || raw.contains("::") {
        return Err("还不支持 IPv6 字面量监听地址");
    }
    let Some((host, port)) = raw.rsplit_once(':') else {
        return Err("缺端口——L4 没有默认端口，写成 `:3306`");
    };
    let port = match port.parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => return Err("端口要是 1–65535 之间的整数"),
    };
    Ok((
        if host.is_empty() {
            None
        } else {
            Some(host.to_string())
        },
        port,
    ))
}

/// 規範化一條 L4 上游。**連接埠必須寫明**。
///
/// ★ 形狀那一半直接借 [`normalize_upstream`]，於是錯誤提示與 `reverse_proxy`
/// 逐字一致；這裡只多加「不許缺連接埠」這一條。
/// ⚠ 借的時候 `transport` 傳什麼都行 —— 它**只**在「缺連接埠要補預設值」那一支用得上，
/// 而那一支在這裡已經被上面擋掉了。傳 `tcp` 是為了讓讀程式碼的人不必去確認這件事。
pub fn normalize_l4_upstream(raw: &str) -> Result<String, &'static str> {
    if !raw.contains(':') {
        return Err(
            "缺端口——L4 上游要写成 `10.0.0.5:3306`（HTTP 那边能按 scheme 补 80/443，L4 补不了）",
        );
    }
    normalize_upstream(raw, "tcp")
}

pub fn normalize_upstream(raw: &str, transport: &str) -> Result<String, &'static str> {
    if raw.is_empty() {
        return Err("是空的");
    }
    if raw.contains('/') {
        return Err("不能带路径或 scheme——上游只写 `主机:端口`");
    }
    if raw.contains('[') || raw.contains("::") {
        return Err("M1 还不支持 IPv6 字面量上游");
    }
    match raw.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return Err("缺少主机名");
            }
            match port.parse::<u16>() {
                Ok(p) if p > 0 => Ok(format!("{host}:{p}")),
                _ => Err("端口要是 1–65535 之间的整数"),
            }
        }
        None => {
            let default = if transport == "https" { 443 } else { 80 };
            Ok(format!("{raw}:{default}"))
        }
    }
}
