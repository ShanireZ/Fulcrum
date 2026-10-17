//! Prometheus 指標：處理程序級註冊表（按執行緒分片，G156）+ text exposition 的**自研**繪製器（**M2 批 M**，G117）。
//!
//! 指標清單、每一格的基數上界與端點形態定稿在
//! [`docs/architecture/observability.md`](../../../docs/architecture/observability.md)
//! —— **那一份是權威**，本模組是它的實作。
//!
//! # ★ ★ 三件被有意分開的事
//!
//! | | 誰 |
//! |---|---|
//! | 有哪些族、每個族帶哪些標籤 | `FAMILIES`：**族的唯一宣告處** |
//! | 一次事件把數記進哪一格 | [`Family::inc`] / [`Family::inc_by`] / [`Family::set`] / [`Family::observe`] |
//! | 抓取時那一坨文字長什麼樣 | [`render`] |
//!
//! ★ 分開的理由是它們各自會獨立變化：加一個族不該動繪製器，改繪製細節不該碰宣告表。
//!
//! # ⚠ ⚠ 標籤的個數與順序由宣告定死，對不上**就地 panic**
//!
//! `{site,outcome}` 與 `{outcome,site}` 繪製出來是**兩條不同的 series**，
//! 而這種錯**在輸出裡看起來完全正常**：抓取端照收、圖照畫，只是畫的是另一件事。
//! ⇒ 唯一逮得住它的地方是寫入那一刻。
//! ★ 用 `assert!` 而不是 `debug_assert!`：後者在 release 下整條消失，
//! 於是「會喊出來」的那個構型正好是產品不跑的那一個。
//!
//! # ⚠ 繪製順序必須是確定的
//!
//! 族按宣告表的順序出，同一個族裡的 series 按標籤值排序（兩層都是 `BTreeMap`）。
//! ★ 換成 `HashMap` 不會讓任何一條斷言變紅，代價是**兩次抓取的 diff 裡全是噪聲** ——
//! 而人正是靠 diff 看指標出沒出新東西的。
//!
//! # 格式自研、零新依賴（G117）
//!
//! text exposition 是行式純文字，自己寫一百行。★ 它**不是安全敏感協定棧**
//! （安全基線第 5 條管的是 TLS / HPACK / QUIC 那一類）⇒ 供應鏈門不用動，
//! musl 靜態產物也不受影響。⚠ 代價：**直方圖分桶、標籤值轉義、`_total` 後綴要自己釘住** ——
//! 釘子在本檔案末尾的單測裡。
//!
//! # ⚠ 基數：每一格的上界都由設定定，不由訪問者定
//!
//! 標籤值只許來自**閉集**（`outcome` / `status_class` / `proto` / 快取事件）或**設定裡的
//! 位址字面值**。⛔ 任何形態都不加 `uri` 標籤。★ 這條紀律的執行者在取數點那一側；
//! 本模組的責任是不給它開後門 —— 寫入 API 收的是 `&[&str]`，個數與順序都由宣告表定死。
//!
//! # ★ ★ ★ 一個族的數只有兩種來處，由宣告表裡的 [`Source`] 定死
//!
//! **① 事件點記賬（[`Source::Event`]）** —— 發生一次就往本執行緒那一片註冊表里加一筆，
//! 繪製只是把數抄出來：
//!
//! | 族 | 取數點 |
//! |---|---|
//! | `fulcrum_requests_total` · `fulcrum_request_duration_seconds` · `fulcrum_tls_requests_total` | [`crate::access_log::Record::finish`]，**只有那一處** |
//! | `fulcrum_no_site_match_total` | `lib.rs` 裡寫 `outcome = "no_site_match"` 的同一處 |
//! | `fulcrum_cache_events_total` | `hit` / `stale` 在 `write_cached`，`miss` 在回源那一處 |
//! | `fulcrum_cache_purged_entries_total` | `POST /purge` 收尾那一處 |
//!
//! **② 抓取時去問活體（[`Source::Live`]）** —— 註冊表裡**永遠沒有它們的數**，
//! [`render`] 那一刻現問 [`LiveSources`] 裡那幾個物件：
//!
//! | 族 | 問誰 |
//! |---|---|
//! | `fulcrum_upstream_inflight` · `fulcrum_upstream_healthy` · `fulcrum_upstream_passive_open` | 當前 `Runtime` 快照裡的每個 `Upstream`，**按位址歸併**：在途數求和、健康位取**合取**、被動熔斷位取**析取**（兩者方向相反而紀律相同：都往「說得出故障」那一側倒）|
//! | `fulcrum_cert_expiry_seconds` | `SniResolver::expiries()`（R5：值是 `notAfter` 的**絕對 Unix 秒**）|
//! | `fulcrum_acme_issue_total` | `AcmeManager::issue_counts()` |
//! | `fulcrum_build_info` | 這個二進位自己（`FULCRUM_BUILD_VERSION`，見 `build.rs`），不需要任何活體源 |
//! | `fulcrum_overrides_active` | `SharedRuntime::override_entries_of()` 的條目數（懸空的照樣計入，裁決 R13）|
//!
//! ★ ★ ★ **第二類為什麼不在事件點記賬**：能從被測物件本身問到的東西，
//! 就不要在旁邊再記一份 —— 否則兩份遲早不一致，而**不一致的那天沒有任何東西會說**。
//! （[`crate::access_log`] 已經把這條落在 `status` / `resp_size` 上。）
//! ⚠ 代價說在明處：讀數是**抓取那一刻**的瞬時值，兩次抓取之間發生過什麼看不見。
//! 對「在途數」「還有多久到期」這類量而言那本來就是全部真相。
//!
//! # ★ ★ 註冊表按執行緒分片（G156）
//!
//! 每條請求收尾都要寫兩筆（TLS 三筆），寫路徑因此是**請求路徑**：它不許拿全處理程序共用的鎖，
//! 也不許在穩態下分配。
//!
//! | | 怎麼做 |
//! |---|---|
//! | 片 | 事件點那一類的註冊表分成 **64 片**（[`SHARDS`]，寫死），每片按 128 位元組對齊（防 false sharing）；執行緒第一次寫時輪流領一個片號，之後一直寫這一片 ⇒ 鎖只有自己用 |
//! | gauge | **不分片**，單獨一張全域表（[`GAUGES`]）：「最後一次寫贏」在多片之間沒有定義，而產品裡沒有在事件點寫的 gauge |
//! | 鍵 | [`LabelKey`]：能拿寫入方手裡的 `&[&str]` 直接**借用查找** ⇒ 一條 series 只在第一次出現時分配 |
//! | 斷言 | 族類型與標籤個數的 `assert!` 在**拿鎖之前**判 ⇒ 斷言失敗的 panic 不毒化任何一片 |
//! | 繪製 | [`merged_events`] **逐片**鎖、合併、放鎖，再交給 `render_into` ⇒ 輸出與一張表時逐位元組相同 |
//!
//! ⚠ 兩處與「一張表」不同的語義，寫在明處：
//! - 一次抓取**不是全處理程序的原子快照**：各片在各自被合併的那一刻一致。★ 對外可見的一致性沒有變差 ——
//!   同一條請求的 `requests_total` 與 `duration` 本來就是兩次寫，一次抓取本來就可能落在兩次之間。
//! - 直方圖的 `_sum` 由各片的和相加，浮點加法順序不同 ⇒ **末位可能有差**；單執行緒寫入時逐位元組相同。
//!
//! ★ 記憶體：series 數（基數）不變，每條 series 在寫過它的片裡各存一份，最多 64 份。
//!
//! # ⚠ 公開面是 `pub` 而不是 `pub(crate)`
//!
//! ★ 不是「對外暴露」的意思：本 crate `publish = false`，`pub` 的作用域就是同一個
//! workspace 裡的那個二進位，而它別的模組（[`crate::access_log`] 一族）本來就是 `pub mod`。

use fulcrum_acme::AcmeManager;
use fulcrum_runtime::SharedRuntime;
use fulcrum_tls::SniResolver;
use log::warn;
use std::borrow::Borrow;
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicUsize};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 族的類型。★ 它同時定死兩件事：`# TYPE` 那一行怎麼寫，以及這個族**能被怎麼寫入**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 只增不減的 `u64`。⚠ 名字**必須**以 `_total` 結尾 —— 由單測守。
    Counter,
    /// 會上下動的 `f64`。
    Gauge,
    /// 固定桶的分佈：`_bucket{le=…}` + `_sum` + `_count`。
    Histogram,
}

impl Kind {
    /// `# TYPE` 行裡的那個詞。★ 它同時被 panic 文案用著 —— 一處定義，兩處消費。
    fn as_str(self) -> &'static str {
        match self {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        }
    }
}

/// 一個族的數**從哪來**。★ 它把本模組那條中心區分寫進了宣告表本身。
///
/// ⚠ ⚠ 一個族**只能有一種來處**：繪製時按它二選一去取數，
/// 於是「事件點也記一筆、抓取時又問一遍」在結構上做不到 ——
/// 那種錯的表現是**同一條 series 的值忽大忽小**，而兩邊各自都言之鑿鑿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// **事件點記賬**：發生一次就往本執行緒那一片註冊表里加一筆（G156）。
    Event,
    /// **抓取時問活體**：處理程序級註冊表裡永遠沒有它的數，[`render`] 那一刻現問。
    Live,
}

/// ACME 那一族 `result` 標籤的三個取值。★ 繪製那一處直接用這三個常數 ⇒
/// [`ACME_RESULTS`] 與產生它們的程式碼是同一批名字，⛔ 不是抄件。
pub const ACME_RESULT_OK: &str = "ok";
/// 見 [`ACME_RESULT_OK`]。
pub const ACME_RESULT_FAIL: &str = "fail";
/// 見 [`ACME_RESULT_OK`]。
pub const ACME_RESULT_DEFERRED: &str = "deferred";
/// `fulcrum_acme_issue_total{result}` 的取值閉集。見 [`ACME_RESULT_OK`]。
pub const ACME_RESULTS: &[&str] = &[ACME_RESULT_OK, ACME_RESULT_FAIL, ACME_RESULT_DEFERRED];

/// 一個標籤的**取值域** —— `observability.md` 那張基數表第 5 列裡那些數字的事實源。
///
/// # ★ ★ ★ 它存在的理由
///
/// 第 5 列此前只被一條**單向蘊含**釘著（無標籤 ⇒ 恆為 1），而列裡的 `8 × 6 × 3`
/// 是**手抄**的。⚠ `status_class` 從 5 變 6 的那一刻那張表就說了假話，
/// **而那正是這道門當初被立起來的起因** —— 它卻逮不到自己的起因。
/// ⇒ 把每個標籤的取值域宣告出來，第 5 列裡的整數由它算。
#[derive(Debug, Clone, Copy)]
pub enum Domain {
    /// 程式碼裡的**閉集**。判據：上界那一格必須出現 `len()` 這個整數，
    /// **或**逐字列出全部取值（真表裡兩種寫法都有：`× 6` 與 `hit`/`miss`/`stale`）。
    Closed(&'static [&'static str]),
    /// 由**設定**定（站台位址、上游名、網域、監聽位址…）。
    /// ⇒ 那一格寫的是散文（`配置定`、`address 数 + 1`），⛔ 本門不判它。
    Config,
    /// 由**編譯期**定（建置版本號、編進去的 TLS 版本表與套件表）。
    /// ⚠ 它的大小會隨建置設定變，釘住只會變成噪音 ⇒ ⛔ 本門不判它。
    Compiled,
}

/// 一個指標族的**全部宣告**：名字、類型、數從哪來、HELP 文字、標籤名清單。
///
/// ⚠ ⚠ `labels` 那一行是**契約**：寫入時給的標籤值要與它**逐項對上**（個數與順序），
/// 對不上就地 panic —— 理由見本檔案頂部。
pub struct Family {
    name: &'static str,
    kind: Kind,
    /// 這個族的數從哪來。見 [`Source`]。
    source: Source,
    /// `# HELP` 行的原文。⚠ **這一行不轉義**（它不是標籤值）⇒ 約束落在宣告這一側：
    /// 帶換行或反斜槓的 HELP 會當場把 exposition 撕壞。由單測守。
    help: &'static str,
    labels: &'static [&'static str],
    /// 每個標籤的取值域，**與 [`Family::labels`] 一一對應、同序**。
    ///
    /// ⚠ 長度必須相等 —— 由單測 `每个族的 domains 与 labels 一一对应` 守著；
    /// ⛔ 少一格的話，後面那些標籤會**靜默地錯位對到別人的域上**。
    ///
    /// ⚠ ⚠ **只有基數表那道門（`#[cfg(test)]`）讀它** ⇒ 非測試建置裡它確實沒人讀。
    /// ★ `allow` 寫在這一格上而不是模組級，免得把別的死程式碼一起放過去。
    /// ⛔ **有意不在寫入路徑上檢核標籤值屬不屬於它的域**：那是每條請求都要付的錢，
    /// 而它擋的那件事（貼一個域外的標籤值）由各自的閉集在**源頭**就擋住了。
    #[allow(dead_code)]
    domains: &'static [Domain],
}

/// 直方圖的桶邊界，**單位是秒**（繪製時再補一格 `+Inf`）。
///
/// ★ 寫死是 G117 點名要釘住的一條：桶邊界一改，抓取端那邊的歷史時序就接不上了 ——
/// 而它接不上的樣子是**圖變了**，不是**回報錯誤**。
const BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// ★ ★ **族的唯一宣告處**，一個族一格；繪製順序 = 這張表的順序。
///
/// ⚠ 下面那幾個 `pub const` 句柄按**下標**指進來 —— 重排這張表就會讓某個句柄換個族，
/// 而那種錯**在輸出裡看起來完全正常**（數字照漲，只是漲在另一條 series 上）。
/// ⇒ 單測 `族句柄指的就是它名字上那个族` 把每個句柄的名字逐個釘住。
const FAMILIES: [Family; 15] = [
    Family {
        name: "fulcrum_requests_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "请求总数，按站点地址字面量、结果、状态码类与协议分。",
        labels: &["site", "outcome", "status_class", "proto"],
        domains: &[
            Domain::Config,
            Domain::Closed(crate::OUTCOMES),
            Domain::Closed(crate::access_log::STATUS_CLASSES),
            Domain::Closed(crate::access_log::PROTOS),
        ],
    },
    Family {
        name: "fulcrum_request_duration_seconds",
        kind: Kind::Histogram,
        source: Source::Event,
        help: "请求耗时分布，单位秒。",
        labels: &["site", "outcome"],
        domains: &[Domain::Config, Domain::Closed(crate::OUTCOMES)],
    },
    Family {
        name: "fulcrum_cache_events_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "HTTP 缓存事件数：命中、回源、重验证后发出；三个值同一个分母（一条请求）。",
        labels: &["event"],
        domains: &[Domain::Closed(crate::cache::CACHE_EVENTS)],
    },
    Family {
        name: "fulcrum_no_site_match_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "没匹配到任何站点的请求数；host 只有出现在配置里的才带真值，其余归 <other>。",
        labels: &["host"],
        domains: &[Domain::Config],
    },
    Family {
        name: "fulcrum_upstream_inflight",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "每个上游地址当前在飞的连接数；同一地址被多处引用时求和。",
        labels: &["upstream"],
        domains: &[Domain::Config],
    },
    Family {
        name: "fulcrum_upstream_healthy",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "上游地址的健康位，1 为健康；同一地址被多处引用时全都健康才为 1；没配 health_uri 的恒为 1。",
        labels: &["upstream"],
        domains: &[Domain::Config],
    },
    Family {
        name: "fulcrum_cert_expiry_seconds",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "已装载证书的 notAfter，取绝对 Unix 秒；抓取端减去 time() 就是剩余量。",
        labels: &["domain"],
        domains: &[Domain::Config],
    },
    Family {
        name: "fulcrum_acme_issue_total",
        kind: Kind::Counter,
        source: Source::Live,
        help: "ACME 巡检的签发结果计数；deferred 是退避中或这一批接不了，不含还不到续期点的。",
        labels: &["result"],
        domains: &[Domain::Closed(ACME_RESULTS)],
    },
    Family {
        name: "fulcrum_build_info",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "恒为 1 的版本标记；标签只有 version，别往里加会随换代变化的东西。",
        labels: &["version"],
        domains: &[Domain::Compiled],
    },
    Family {
        name: "fulcrum_overrides_active",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "当前生效中的临时覆盖总数；悬空的（键指向的上游已经不在当前配置里）照样计入（裁决 R13）。",
        labels: &[],
        domains: &[],
    },
    Family {
        name: "fulcrum_cache_purged_entries_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "被 POST /purge 清掉的缓存条目数；单位是条目，不是请求。",
        labels: &[],
        domains: &[],
    },
    Family {
        name: "fulcrum_tls_requests_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "走 TLS 的请求数，按协商出来的版本与密码套件分；明文请求不计，问不出来的那一格记 <unknown>。",
        labels: &["version", "cipher"],
        domains: &[Domain::Compiled, Domain::Compiled],
    },
    Family {
        name: "fulcrum_connections_total",
        kind: Kind::Counter,
        source: Source::Live,
        help: "累计接进来过多少条连接，按监听地址与入口种类分；计在握手之前。",
        labels: &["listen", "entrypoint"],
        domains: &[
            Domain::Config,
            Domain::Closed(crate::conn_stats::ENTRYPOINTS),
        ],
    },
    Family {
        name: "fulcrum_connections_active",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "此刻还活着的连接数，按监听地址与入口种类分；含还在握手的那些。l4_udp 那一格数的是会话。",
        labels: &["listen", "entrypoint"],
        domains: &[
            Domain::Config,
            Domain::Closed(crate::conn_stats::ENTRYPOINTS),
        ],
    },
    // ★ ★ **G136 有意追加在末尾**：上面那串 `const X: &Family = &FAMILIES[i]` 按**下標**取，
    //   插在中間會讓它們**整體錯位**，而錯位之後每一條指標都還繪製得出來 ——
    //   只是名字與數對不上，且沒有任何東西會回報錯誤。
    //   （`FAMILIES` 的名字斷言那幾條測試守著這件事，但別指望它備援：先別插。）
    Family {
        name: "fulcrum_upstream_passive_open",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "上游地址此刻是否被被动熔断摘出调度，1 为已摘；同一地址被多处引用时任一处熔断即为 1；没配 passive_fail 的恒为 0。",
        labels: &["upstream"],
        domains: &[Domain::Config],
    },
];

/// 請求總數。
///
/// ★ ★ 取數點**只能有一處**（`access_log::Record` 收尾的那一處）：兩處各算一遍
/// `outcome` / `status` 遲早分家，而**分家的表現是兩個數字都言之鑿鑿、卻對不上**。
pub const REQUESTS_TOTAL: &Family = &FAMILIES[0];

/// 請求耗時分佈。⚠ 單位是**秒**，與 `BUCKETS` 和名字裡的 `_seconds` 同一口徑。
pub const REQUEST_DURATION_SECONDS: &Family = &FAMILIES[1];

/// 快取事件數。⚠ `event` 是閉集，摺疊函式不許有備援臂 —— 否則將來新增的一種狀態
/// 會**靜默地被算成別的**。
///
/// ★ ★ **三個值同一個分母**（**M2 批 M′ 任務 2**，G123 / D31 結案）：`hit` · `miss` ·
/// `stale` 數的都是「**一條請求**」⇒ `sum(fulcrum_cache_events_total)` 就是
/// 「查過快取並且已經定下內容從哪來的請求數」，命中率直接可寫。
/// ⇒ 「被清掉的條目數」搬去了 [`CACHE_PURGED_ENTRIES_TOTAL`]，因為它數的是**條目**。
/// ★ 這是把一個**靠使用者記住**的陷阱換成一個**結構上不存在**的陷阱：
/// 兩個不同的分母不再共用一個族，也就沒有「求和求出個沒意義的數」這回事了。
pub const CACHE_EVENTS_TOTAL: &Family = &FAMILIES[2];

/// 沒匹配到站台的請求數（G118）。⚠ `host` 由請求方給 ⇒ 只有出現在設定裡的才帶真值，
/// 其餘一律 `<other>`：**上界由設定定、不由訪問者定**。
pub const NO_SITE_MATCH_TOTAL: &Family = &FAMILIES[3];

/// 每個上游**位址**當前在飛的連接數。⚠ 抓取時問 `Upstream::inflight()`，**不在這裡另記一份**。
///
/// ★ 同一個位址被多處引用時**求和** —— 它是個計數，聚合只有這一種說得通的做法。
const UPSTREAM_INFLIGHT: &Family = &FAMILIES[4];

/// 每個上游**位址**的健康位。★ 沒配 `health_uri` 的上游恆為 1（那與執行環境那一側的初值同一口徑）。
///
/// ★ 同一個位址被多處引用時取**合取**（全都健康才是 1）—— 它是個布爾，⛔ 不求和。
const UPSTREAM_HEALTHY: &Family = &FAMILIES[5];

/// 已裝載證書的 `notAfter`，**絕對 Unix 秒**（裁決 R5）。
///
/// ★ 取絕對值而不是「還剩多少秒」：絕對值不隨時間漂，抓取端一句 `- time()` 就得到剩餘量；
/// 而「剩餘量」需要有人定期刷新 —— 那**等於在旁邊再記一份會過期的東西**。
const CERT_EXPIRY_SECONDS: &Family = &FAMILIES[6];

/// ACME 巡檢的簽發結果計數。⚠ `deferred` 的定義寫死在 `fulcrum_acme::IssueCounts` 上。
const ACME_ISSUE_TOTAL: &Family = &FAMILIES[7];

/// 恆為 1 的版本標記。
///
/// ⛔ **不帶 `gen_id`、不帶 pid**：那會讓每一次換代長出一條新 series，
/// 而舊的那條從此再也不更新 —— 抓取端看到的是一堆看起來還活著的殭屍時序。
const BUILD_INFO: &Family = &FAMILIES[8];

/// [`BUILD_INFO`] 的 `version` 取什麼值 —— 由 [`build.rs`](../build.rs) 在編譯期算好。
///
/// ⛔ ★ **這是「跑的是哪一次建置」，不是「這個版本承諾了什麼」**（`G141`）。
/// 取值鏈、為什麼用 semver 的建置中繼資料語法、以及各個面上什麼算 breaking，
/// 都在 `docs/governance/compatibility.md`。
///
/// ⚠ 取不到時它是 `<包版本>+unknown` 而**不是**裸的包版本 ——
/// 「讀不到」與「讀到了 0.0.0」必須區分得開。
const BUILD_VERSION: &str = env!("FULCRUM_BUILD_VERSION");

/// 當前生效中的臨時覆蓋總數（**M2 批 N 任務 6.5**，G126 / 裁決 R13）。
///
/// ⚠ ⚠ **懸空的照樣計入**：R13 把「懸空的算不算生效中」定死為「算」——它確實
/// 還在登記處佔著一格，且 `/load` 已經逐條點過名。⇒ 這一格的值**必須**是
/// [`SharedRuntime::override_entries_of`] 那份列表的**長度**（全部條目數），
/// ⛔ 不是「非懸空數」，也不是「懸空數」。
/// ★ 它與 [`SharedRuntime::override_counts`] 的 `.0` 恆等（後者就是
/// `override_entries().len()`），但 `override_counts` 內部會**自己再取一次快照**
/// ⇒ 抓取路徑上只許用 `_of` 這一版，理由見 `snapshot()` 裡那段註解。
///
/// ⛔ **不按 `(站点, 上游)` 打標籤**：那等於把 `/stats` 的 `overrides` 一節整個
/// 搬進指標 —— 兩份實作同一件事，而「是哪幾項」本來就該去 `/stats` 看。
/// ⇒ 這個族**無標籤、基數恆為 1**。
const OVERRIDES_ACTIVE: &Family = &FAMILIES[9];
/// ★ G136。⚠ 下標是 `FAMILIES.len() - 1` —— 它追加在末尾，理由見那一條上面的註解。
const UPSTREAM_PASSIVE_OPEN: &Family = &FAMILIES[FAMILIES.len() - 1];

/// 被 `POST /purge` 清掉的快取**條目**數（**M2 批 M′ 任務 2**，G123 / D31 結案）。
///
/// ⚠ ⚠ **單位是「條目」，不是「請求」** —— 這正是它必須自成一族的全部理由：
/// 它此前是 `fulcrum_cache_events_total{event="purge"}` 的一格，而那個族另外三格
/// 數的是請求 ⇒ `sum(cache_events_total)` 得到的是**請求數與條目數相加**的一個數，
/// 它長得完全正常、也會隨流量漲，而一次 `POST /purge all` 清掉十萬條時那個尖峰
/// **讀起來像是流量突增**。
///
/// ★ 記的是**被清掉的條目數**，不是「purge 被調了幾次」—— 問「清掉了多少」的人
/// 遠多於問「這個介面被調了幾次」的人。
/// ⚠ **一條都沒清掉時也照記 `+0`**：那讓這條 series **存在**，而「這個處理程序從來沒
/// purge 過」與「purge 過、只是沒清到東西」是兩件事，在抓取端要分得開。
/// ★ 這與 `hit`/`miss`/`stale`「發生了才出現」**有意不對稱**。
///
/// ⛔ **無標籤**：`POST /purge` 的三種粒度（`key` / `prefix` / `all`）不做標籤 ——
/// 「這一次清的是哪種粒度」是呼叫方自己剛說過的話，指標上再說一遍資訊量為零。
pub const CACHE_PURGED_ENTRIES_TOTAL: &Family = &FAMILIES[10];

/// 走 TLS 的請求數，按協商出來的 `version` / `cipher` 分（**M2 批 M′ 任務 3**，
/// G122 的 **TLS 那半** / G127）。
///
/// # ⚠ ⚠ 它的總數**不等於** [`REQUESTS_TOTAL`] 的總數，而且那是有意的
///
/// 明文請求的 `Record::tls` 是 `None` ⇒ **一筆都不記**。
/// ⛔ 因此**不許寫「這兩個族的總數相等」那種判據** —— 它會在第一條明文請求上紅，
/// 而那條請求什麼問題都沒有。★ 說得出口的關係只有一條：
/// `sum(tls_requests_total) ≤ sum(requests_total)`。
///
/// # ★ 基數上界由**伺服器端**定，不由訪問者定
///
/// 兩個標籤都是**協商結果**：`version` 來自 TLS 版本（我們編進去的那幾個），
/// `cipher` 來自我們編進去的套件表 ⇒ 上界是兩張表的乘積，訪問者一個值都添不進來。
/// ⛔ **`sni` / `alpn` 不當標籤**：那兩個是**訪問者給的**，與 G121 擋住 `host` 是同一件事。
///
/// # ⚠ 名字裡是 `requests` 不是 `handshakes`
///
/// 取數點在 [`crate::access_log::Record::finish`]（**與 [`REQUESTS_TOTAL`] 逐字同一處**）
/// ⇒ 數的是**請求**。keep-alive 下一次握手對應多條請求，叫 `handshakes`
/// 會是一句**讀起來完全成立的假話**。
///
/// # ★ ★ 取不到的那一格記 `<unknown>`，⛔ 不記空串（G127）
///
/// h3 走 quiche，而 `Handshake::cipher()` 鎖在私有 `mod tls` 裡 ⇒ 那一格問不出來。
/// ⚠ ⚠ **在 Prometheus 資料模型裡空標籤值等同於「這個標籤不存在」** ⇒ 記空串的話，
/// 「今天這一格取不到」與「明天把 `cipher` 整個刪掉」在抓取端**分不開**；
/// 而 `cipher=""` 恰好是 PromQL 裡「該標籤不存在」的慣用寫法，維運照著寫出來的過濾器
/// 會命中一批他沒打算命中的 series。
///
/// ⚠ ⚠ **判據落在「值為空」上，⛔ 不落在「是不是 h3」上**：後者是把一個今天成立的
/// 巧合釘進程式碼 —— 哪天別的傳輸也讀不出套件，它會走到分支外面記一個空串，
/// **而那時不會有任何東西紅**。⇒ 落法見 [`label_or_unknown`]。
///
/// ⚠ **它與訪問紀錄那一側的處置有意不同，別當成同一條**：紀錄裡空串 = **那一格不出現**
/// （`tests/log/run.sh` 有一條反向斷言防著有人給它編一個值）；而指標**沒有「那一格不
/// 出現」這回事** —— 每條 series 都必須給每個標籤一個值。⇒ 同一個空串，兩處兩種處置。
pub const TLS_REQUESTS_TOTAL: &Family = &FAMILIES[11];

/// 累計接進來過多少條連接（**M2 批 O**，G122 的**連接那半**）。
///
/// # ⚠ ⚠ 它是**四處** accept 循環記的，而不是一處
///
/// pingora 的 `Service::run_endpoint`（h1/h2 與 admin，經 **fork 改動 15** 的接縫）·
/// L4 TCP · L4 UDP · QUIC —— 後三者各有各的循環，pingora 那個一次都不經過。
/// ★ 四處只負責把數加到**同一個** [`crate::conn_stats::ConnRegistry`] 上，
/// 而標籤定義只有一份。
///
/// # ★ 計在**握手之前**
///
/// `enter` 的呼叫點在 accept 之後、TLS 握手之前 ⇒ 一條握手失敗的連接
/// **也會** +1。★ 那是有意的：握手階段的堆積（打爆 TLS、slowloris）正是最該
/// 看得見的東西，而計在握手之後就把它整段藏起來了。
pub const CONNECTIONS_TOTAL: &Family = &FAMILIES[12];

/// 此刻還活著的連接數（**M2 批 O**）。
///
/// # ★ ★ ★ 減一**只有一個**呼叫點
///
/// 那條連接任務有三條退出路徑（握手超時 / 握手失敗 / 正常結束），
/// 而減一全部由 fork 那側的 `ConnGuard` 的 `Drop` 做 —— 手寫三處 `fetch_sub`
/// 正是 D18/G66 那個分家形狀，⚠ 而分家時的表現是**這個數只漲不降**，
/// counter 照常，**沒有任何東西會說**。
///
/// # ⚠ `entrypoint="l4_udp"` 那一格數的是**工作階段**，不是連接
///
/// UDP 上沒有連接。那一格從 L4 UDP 的工作階段表 `sessions.len()` **派生**
/// （[`crate::conn_stats::ConnRegistry::set_active`]），⛔ 不是 `+1/-1` ——
/// 因為 `sessions.len()` 本身就是權威，在旁邊再記一份會讓清掃、到上限被拒、
/// 停機不再收包三條路各需要記得減一。
///
/// # ⚠ ⛔ 標籤叫 `entrypoint`，不叫 `proto`
///
/// [`REQUESTS_TOTAL`] 已經有一個 `proto`，取值是 `HTTP/1.1` 那一族。同名不同值域
/// 會讓維運跨族寫同一個過濾器時**拿到空集而不回報錯誤**。
pub const CONNECTIONS_ACTIVE: &Family = &FAMILIES[13];

/// 標籤值**取不到**時記的那個記號（G127）。
///
/// ★ 尖括號形狀隨 G118 的 `<other>` 與 G121 的 `<none>`：尖括號在真實的套件名
/// （`TLS_AES_256_GCM_SHA384`）、位址字面值與主機名裡都不可能出現 ⇒ 撞不上真值。
/// ⚠ **三個記號動機各不相同，別併成一條**：`<other>` 擋的是**訪問者可控的無界基數**、
/// `<none>` 說的是**這條請求沒有匹配到站台**、`<unknown>` 說的是**我們問不出來**。
pub const UNKNOWN_LABEL: &str = "<unknown>";

/// 取不到（空串）就換成 [`UNKNOWN_LABEL`]，否則原樣回傳（G127）。
///
/// ⚠ ⚠ **判據是「值為空」，⛔ 不是「這條請求走的是哪種傳輸」** —— 後者會把一個
/// 今天成立的巧合釘進程式碼。⇒ 這個函式**看不見**呼叫它的是 h1/h2 還是 h3，
/// 那正是它的全部意義。
///
/// ⛔ **不在 [`Family::inc`] 裡統一替換**：那會讓「記了個空串」這件事在**任何**族上
/// 都變得不可能，包括將來某個把空串當成一個正當取值的族 —— 而它替換掉的那一刻
/// 不會有任何東西說。★ 記號是取數點的決定，落在取數點那一行上，與 `<none>` 同一種寫法。
pub fn label_or_unknown(v: &str) -> &str {
    if v.is_empty() { UNKNOWN_LABEL } else { v }
}

/// 一條直方圖 series。
#[derive(Clone)]
struct Hist {
    /// 與 `BUCKETS` 等長的**非累積**計數：一次觀測只碰一格。
    /// ★ 累積放到繪製時算 —— 觀測在請求路徑上（一次二分 + 一次自增），
    /// 而繪製一次抓取才發生一回。
    per_bucket: Vec<u64>,
    sum: f64,
    /// 全部觀測數，**含超出最大桶的那些** ⇒ `le="+Inf"` 那一格就是它。
    count: u64,
}

impl Default for Hist {
    fn default() -> Hist {
        Hist {
            per_bucket: vec![0; BUCKETS.len()],
            sum: 0.0,
            count: 0,
        }
    }
}

/// 註冊表裡**一組標籤值**（G156）。
///
/// ★ ★ 它存在的唯一理由是**借用查找**：寫入方手裡是 `&[&str]`，而鍵若是 `Vec<String>`，
///   就只能拿一個 `Vec<String>` 去查 ⇒ 每次寫都得先分配一份鍵、查到了再扔掉（每請求 8 次）。
///   本類型實作 `Borrow<dyn LabelView>`，`&[&str]` 也實作 [`LabelView`] ⇒ 直接拿 `&[&str]` 查，
///   只有這條 series **第一次**出現時才分配。
/// ⚠ 派生的 `Ord` 是切片的字典序（與 `Vec<String>` 的序逐條相同）—— 繪製順序就是這個序。
///   [`LabelView`] 那一側的 `Ord` 必須與它一致（`Borrow` 的契約），由單測
///   `标签键的序与_vec_string_一致_借用查找也命中` 釘住。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LabelKey(Box<[Box<str>]>);

impl LabelKey {
    fn from_labels(labels: &[&str]) -> LabelKey {
        LabelKey(labels.iter().map(|s| Box::from(*s)).collect())
    }

    fn as_slice(&self) -> &[Box<str>] {
        &self.0
    }
}

/// 「一組標籤值」的唯讀視圖：[`LabelKey`] 與寫入方手裡的 `&[&str]` 共用它來比較。
///
/// ⚠ 實作在 `&[&str]`（引用類型）上而不是 `[&str]` 上：`&[&str]` 轉不成 `&dyn` ——
///   切片本身不定長。呼叫方寫 `&labels as &dyn LabelView`（`labels: &[&str]`）。
trait LabelView {
    fn width(&self) -> usize;
    fn value(&self, i: usize) -> &str;
}

impl LabelView for LabelKey {
    fn width(&self) -> usize {
        self.0.len()
    }

    fn value(&self, i: usize) -> &str {
        &self.0[i]
    }
}

impl LabelView for &[&str] {
    fn width(&self) -> usize {
        self.len()
    }

    fn value(&self, i: usize) -> &str {
        self[i]
    }
}

/// ⚠ ⚠ **必須與切片的字典序逐條相同**：先逐元素比，前綴相同再比寬度。
///   「先比寬度」讀起來同樣合理，而它會讓借用查找在寬度不同的鍵之間靜靜地查不到
///   （同一個族的鍵寬度相同，所以真表裡一時看不出來 —— 判據裡有意放了寬度不同的樣本）。
impl Ord for dyn LabelView + '_ {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.width().min(other.width());
        for i in 0..n {
            match self.value(i).cmp(other.value(i)) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        self.width().cmp(&other.width())
    }
}

impl PartialOrd for dyn LabelView + '_ {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for dyn LabelView + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for dyn LabelView + '_ {}

impl<'a> Borrow<dyn LabelView + 'a> for LabelKey {
    fn borrow(&self) -> &(dyn LabelView + 'a) {
        self
    }
}

/// 註冊表：**族名 → 這個族下的每一組標籤值 → 讀數**。
///
/// ★ 三種族各一張表，而不是一張 `BTreeMap<…, 枚举>`：這樣「按 gauge 寫一個 counter」
/// 在結構上就落不進同一格，繪製時也不必寫一條永遠走不到的 `unreachable` 臂。
struct Registry {
    counters: BTreeMap<&'static str, BTreeMap<LabelKey, u64>>,
    gauges: BTreeMap<&'static str, BTreeMap<LabelKey, f64>>,
    histograms: BTreeMap<&'static str, BTreeMap<LabelKey, Hist>>,
}

impl Registry {
    /// `const` 版的空表：靜態初始化要用（`Default` 不是 `const`）。
    const fn new() -> Registry {
        Registry {
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
            histograms: BTreeMap::new(),
        }
    }
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::new()
    }
}

/// 事件點那一類的註冊表分幾片（G156）。
///
/// ★ 寫死，⛔ 不按核數算：上界與機器無關，判據好寫。寫執行緒多於片數時幾個執行緒共用一片 ——
///   仍然正確，只是那幾片回到有人爭。今天的寫執行緒（L7 / L4 / QUIC / 管理面與後台幾組
///   runtime 的執行緒）正常遠少於 64。
const SHARDS: usize = 64;

/// 一片 = 一張註冊表 + 一把**只有領了這一片的執行緒用**的鎖。
///
/// ⚠ ⚠ 對齊到 128 位元組：相鄰兩片的鎖落在同一條快取行上，兩個核各寫各的片也會把那條行
///   來回搶（false sharing）—— 等於把分片要消滅的跨核爭用換個地方帶回來。
///   128 而不是 64：x86 的相鄰行預取按 128 位元組成對拉。
#[repr(align(128))]
struct Shard(Mutex<Registry>);

static SHARD_TABLE: [Shard; SHARDS] = [const { Shard(Mutex::new(Registry::new())) }; SHARDS];

/// gauge **不分片**：「最後一次寫贏」在多片之間沒有定義，而產品裡沒有在事件點寫的
/// gauge（活體族在抓取時現建的臨時表裡寫）⇒ 它不在請求路徑上，單獨一張全域小表即可。
static GAUGES: Mutex<Registry> = Mutex::new(Registry::new());

static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// 本執行緒領到的片號；`usize::MAX` = 還沒領。const 初始化、無析構 ⇒ 取值不分配。
    static MY_SHARD: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// 本執行緒寫哪一片：第一次寫時輪流領一個，之後一直是它。
///
/// ⚠ `try_with` 失敗 = `MY_SHARD` 自己已經被拆了。今天它是無析構的 `Cell`，在有原生 TLS 的
///   目標上永遠不會被拆 ⇒ 這一支今天走不到；它防的是 `MY_SHARD` 將來換成帶析構的類型 ——
///   那時它可能先於別的 thread-local 被拆，而那些 thread-local 的析構裡還會寫指標。
///   走到這一支時不快取，直接再領一個：那一筆照樣記得上，落在哪一片無所謂。
fn my_shard() -> usize {
    let take = || NEXT_SHARD.fetch_add(1, atomic::Ordering::Relaxed) % SHARDS;
    MY_SHARD
        .try_with(|c| {
            let i = c.get();
            if i != usize::MAX {
                return i;
            }
            let i = take();
            c.set(i);
            i
        })
        .unwrap_or_else(|_| take())
}

/// 鎖一片（或 gauge 表）。
///
/// ⚠ 鎖中毒時接著用裡面那份資料（與 `access_log` 逐字同一口徑）：**指標少幾格，
/// 也不該把請求路徑帶崩** —— 觀測面的失效不許升級成資料面的失效。
fn lock_tolerant(m: &'static Mutex<Registry>) -> MutexGuard<'static, Registry> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 抓取時：把各片與 gauge 表合併成一張臨時表。
///
/// ⚠ ⚠ **任何時刻只握一片**：鎖一片、合併、放鎖，再下一片。同時握兩片就是給片排出了
///   一個鎖序，而那個順序沒有任何東西在守（寫路徑只握自己那一片，測試鉤子按下標順序握）。
/// ★ 代價寫在明處：這份表**不是全處理程序的原子快照**，各片在各自被合併的那一刻一致。
fn merged_events() -> Registry {
    let mut merged = Registry::new();
    for s in &SHARD_TABLE {
        merged.merge_from(&lock_tolerant(&s.0));
    }
    merged.merge_from(&lock_tolerant(&GAUGES));
    merged
}

/// 只給測試用：按下標順序握住除 `keep` 以外的所有片，再握 gauge 表。
#[cfg(test)]
fn lock_all_but(keep: usize) -> Vec<MutexGuard<'static, Registry>> {
    let mut guards: Vec<_> = SHARD_TABLE
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != keep)
        .map(|(_, s)| lock_tolerant(&s.0))
        .collect();
    guards.push(lock_tolerant(&GAUGES));
    guards
}

/// [`Source::Live`] 那幾個族**去問誰**。
///
/// ⚠ ⚠ **三個都可以缺席**，缺席的那個族就只出 HELP/TYPE、不出樣本：
/// ⛔ 不 panic（觀測面的失效不許升級成資料面的失效），
/// ⛔ 也不讓整族消失 —— 整族消失會讓「沒接上」與「沒資料」在抓取端**看起來一模一樣**，
/// 而那是兩件完全不同的事：前者要改程式碼，後者什麼都不用做。
///
/// ★ 存 `Arc` 而不是 `Weak`：這三個物件與處理程序同壽（資料面、TLS 解析器、ACME 巡檢
/// 各自都被別處握著），拿 `Weak` 只會多出一條**永遠走不到的** upgrade 失敗分支 ——
/// 而走不到的分支既測不了，也遲早寫錯。
#[derive(Default)]
pub struct LiveSources {
    /// 上游那兩個族。⚠ 存的是 [`SharedRuntime`] 而不是某一份 `Runtime` 快照：
    /// 全量 load 換掉的正是它裡面那一份，握著快照等於指標停在換設定之前。
    pub runtime: Option<Arc<SharedRuntime>>,
    /// 證書到期時刻。
    pub resolver: Option<Arc<SniResolver>>,
    /// ACME 簽發計數。`None` = 這份設定裡沒有自動簽發（`acme::build` 回傳 `None`）。
    pub acme: Option<Arc<AcmeManager>>,
    /// 連接計數的登記處（**M2 批 O**，G122 的連接那半）。
    ///
    /// ★ 四處 accept 循環都往它上面加數，抓取那一刻現問 —— fork 夠不到本模組，
    /// 所以只能走這條「它加、我們問」的路（與 `upstream_inflight` 同一條路子）。
    pub conn: Option<Arc<crate::conn_stats::ConnRegistry>>,
}

fn live() -> &'static OnceLock<LiveSources> {
    static LIVE: OnceLock<LiveSources> = OnceLock::new();
    &LIVE
}

/// 在串接處登記一次活體源。
///
/// ⚠ **只認第一次**，重複登記會被丟掉並打一行 warn。★ 這是有意的：一個能被換掉的
/// 活體源等於給「指標現在在問誰」再開一條狀態通道，而換設定那條通道已經存在 ——
/// 它就是 [`SharedRuntime`] 自己（全量 load 換的是它裡面那一份，不是這個句柄）。
pub fn register_live(sources: LiveSources) {
    if live().set(sources).is_err() {
        warn!("指标的活体源已经登记过一次了，这一次被忽略（登记只认第一次）");
    }
}

/// 抓取那一刻去問活體，得到一份**臨時**註冊表。
///
/// ★ 它不進處理程序級註冊表：這幾個族的數**不屬於我們**，屬於被問的那幾個物件。
/// ⚠ 這份表每次抓取新建 ⇒ 下面的 `inc_by` 在這裡等價於「寫入」，不是「累加」。
fn snapshot(src: &LiveSources) -> Registry {
    let mut r = Registry::default();

    // ★ `build_info` 不需要任何活體源 —— 被問的物件就是這個二進位自己。
    //   ⚠ 於是它是**唯一一個「沒登記也照樣有樣本」**的活體族。
    r.set(BUILD_INFO, &[BUILD_VERSION], 1.0);

    if let Some(rt) = &src.runtime {
        // ⚠ 一次抓取只取一份快照：分兩次取的話，兩個族可能落在**兩份不同的設定**上，
        //   於是同一個上游在 `inflight` 裡在、在 `healthy` 裡不在。
        let snap = rt.current();
        // ⚠ ⚠ 同一個位址被多個站台（或同一個站台的多條 `reverse_proxy`）寫到時，
        //   在執行環境圖裡是**多個 `Upstream` 物件、多份各自獨立的狀態** ——
        //   而這兩個族按**位址**出一條 series（`upstream` 標籤就是那個位址串，
        //   也是 `least_conn` 與健康檢查共用的那個身分）。
        //   ⇒ 先歸併，再出樣本。**兩個族的歸併方式不一樣，各有各的理由**：
        //
        // ★ ★ `inflight` 是**計數** ⇒ **求和**。挑其中一份報出去，得到的是一個
        //   **錯的數，而它長得和對的數一模一樣** —— 讀這條 series 的人只會有一個理解
        //   （「打到這個位址、現在還在飛的連接一共有多少」），沒有任何東西會說它只算了一半。
        //   ⚠ 共享後端（兩個站台各寫一條 `reverse_proxy 127.0.0.1:3000`）是很常見的設定。
        //
        // ★ ★ `healthy` 是**布爾** ⇒ **取合取**（全都健康才算健康），⛔ 不是求和
        //   （兩份健康位求和會得到 2，那根本不是這個族的值域）。
        //   ★ 取合取而不是析取，是因為**混配**：站台 A 配了 `health_uri`、站台 B 沒配
        //   ⇒ B 那一份**恆為 1**（執行環境那側的初值，「沒配就永不探測」）。
        //   合取給出的是**真的探過的那一側**探到的狀態；析取會讓一個
        //   **根本沒在探測**的物件把一次真實的故障蓋掉。⇒ 悲觀那一側是安全的那一側。
        //
        // ★ 歸併之後「留第一份還是留最後一份」這個問題不再存在 —— 它本來就不該存在。
        let mut by_addr: BTreeMap<&str, (u64, bool, bool)> = BTreeMap::new();
        for t in snap.all_proxy_targets() {
            for up in &t.upstreams {
                let e = by_addr.entry(up.addr.as_str()).or_insert((0, true, false));
                e.0 += up.inflight() as u64;
                e.1 &= up.is_healthy();
                // ★ ★ G136：歸併取**析取**，與上面 `healthy` 的合取方向相反 —— 而兩者
                //   其實是**同一條紀律**：往「說得出故障」那一側倒。
                //   `healthy` 的 1 是「好」，所以合取（有一處壞就報壞）；
                //   `passive_open` 的 1 是「壞」，所以析取（有一處熔斷就報熔斷）。
                //   ⚠ 寫成合取的話，一條沒配 `passive_fail` 的引用（恆 false）
                //   會把另一條真實的熔斷**蓋掉**，而那正是這個指標唯一要說的事。
                e.2 |= up.passive_open();
            }
        }
        for (addr, (inflight, healthy, passive_open)) in by_addr {
            r.set(UPSTREAM_INFLIGHT, &[addr], inflight as f64);
            r.set(UPSTREAM_HEALTHY, &[addr], if healthy { 1.0 } else { 0.0 });
            r.set(
                UPSTREAM_PASSIVE_OPEN,
                &[addr],
                if passive_open { 1.0 } else { 0.0 },
            );
        }

        // ── fulcrum_overrides_active：取數口是 SharedRuntime::override_entries_of ──
        //   ⛔ 不在這裡重新遍歷 OverrideLayer、也不另記一份計數：道理與上面 upstream
        //   兩個族的註解同一條——能從被測物件本身問到的東西，就不要在旁邊再記一份。
        //   ★ ★ **與 `/stats` 同源，而且是同一個函式**：`admin.rs` 的 `stats()` 也走
        //   `override_entries_of(&rt)`，兩處落在同一個 `OverrideLayer::entries()` 上。
        //   ⛔ ⛔ **不許改調 `override_counts()` / `override_entries()`**：那兩個內部
        //   會再調一次 `current()`，而本塊開頭（`let snap`）已經取過一份 ——
        //   一次抓取取兩份快照正是上面那條註解擋的東西，也正是任務 6 修復輪 1
        //   剛從 `/stats` 上刪掉的同一處缺陷。★ `SharedRuntime::override_entries`
        //   的文件**逐字點名了 `metrics.rs`** 要求已持快照的呼叫方改用本函式。
        //   ⚠ ⚠ 值是**全部條目數**：`entries()` 只按 `has_override()` 過濾，懸空的
        //   只是被打上 `dangling` 標記、照樣在列表裡（裁決 R13 要的就是這個數）。
        //   ★ 無條件呼叫、無條件寫入：0 覆蓋時也要出一條樣本（值是 0），不能因為
        //   「沒有覆蓋」就讓整族的這一格消失——那會讓「沒接上」與「沒資料」
        //   在抓取端看起來一模一樣。
        let overrides_active = rt.override_entries_of(&snap).len();
        r.set(OVERRIDES_ACTIVE, &[], overrides_active as f64);
    }

    // ── 連接那兩個族（**M2 批 O**）──────────────────────────────────────────
    //
    // ★ ★ 每一格在**串接那一刻**就被宣告過了 ⇒ 一個監聽器從處理程序起來的第一秒
    //   就有一條 `0` 的樣本，而不是「有連接了才出現」。
    //   ⚠ 那不是可有可無的：沒有 series 與「有 series 但值是 0」在抓取端**分不開**，
    //   而「我配了這個連接埠怎麼沒流量」正是要靠這個區分來答的第一個問題。
    // ⚠ `total` 是 counter 卻走 `Source::Live`：數累加在**註冊表**裡而不是本模組的
    //   `Registry` 裡 ⇒ 從本模組看它就是「抓取時問活體」。先例是 `acme_issue_total`。
    // ⚠ ⚠ `total` 走 `inc_by` 而不是 `set`：`Registry::set` 那一處 `assert!` 只收
    //   gauge（寫錯時**當場 panic**，不是靜默寫進另一個桶）—— ★ 這一批實測撞過一次：
    //   寫成 `set` 之後 `/metrics` 的連接被丟棄、curl 拿到 000，而正文裡什麼都沒有。
    //   ⇒ 這裡與 `acme_issue_total`（同樣是 Live + Counter）逐字同一種寫法。
    // ★ 那份 `snapshot()` 是**這一刻**的全量讀數，而本函式每次抓取都新建一個
    //   `Registry` ⇒ `inc_by` 從 0 起加，得到的就是那個絕對值，不會跨抓取累積。
    if let Some(conn) = &src.conn {
        for (ep, listen, total, active) in conn.snapshot() {
            r.inc_by(CONNECTIONS_TOTAL, &[&listen, ep.as_str()], total);
            r.set(CONNECTIONS_ACTIVE, &[&listen, ep.as_str()], active as f64);
        }
    }

    if let Some(resolver) = &src.resolver {
        for (domain, not_after) in resolver.expiries() {
            r.set(
                CERT_EXPIRY_SECONDS,
                &[domain.as_str()],
                unix_secs(not_after),
            );
        }
    }

    if let Some(acme) = &src.acme {
        let (ok, fail, deferred) = acme.issue_counts().snapshot();
        // ★ 三格**無條件都出**，哪怕是 0：一條從來沒出現過的 series 與一條恆為 0 的
        //   series，在告警規則裡是兩種完全不同的東西（前者讓 `rate()` 直接沒有資料）。
        // ★ 用常數而不是字面值：`ACME_RESULTS` 是基數表那道門算上界用的閉集，
        //   而它必須與**產生這些值的這一處**是同一批名字，⛔ 不能是一份抄件。
        r.inc_by(ACME_ISSUE_TOTAL, &[ACME_RESULT_OK], ok);
        r.inc_by(ACME_ISSUE_TOTAL, &[ACME_RESULT_FAIL], fail);
        r.inc_by(ACME_ISSUE_TOTAL, &[ACME_RESULT_DEFERRED], deferred);
    }

    r
}

/// `SystemTime` → Unix 秒。
///
/// ⚠ 1970 之前的 `notAfter` 不可能來自一張真證書，但它在類型上是可表達的 ——
/// ★ 靜靜回 0 會把它說成「1970 年到期」，而一個負數一眼就看得出不對。
///
/// ★ `pub(crate)`（**M2 批 N 任務 6**）：`/stats`（`admin.rs`）繪製證書到期與
/// 設定裝載時間時重複使用**這一個**轉換 —— 兩處各寫一份「差不多的」`SystemTime → f64`
/// 遲早會在閏秒/精度上分家，而這幾行本來就沒有第二種寫法可選。
pub(crate) fn unix_secs(t: SystemTime) -> f64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

impl Family {
    /// +1。
    pub fn inc(&self, labels: &[&str]) {
        self.inc_by(labels, 1);
    }

    /// +n。★ 有 `inc_by` 是因為有些事件天生成批（一次清理清掉了 N 條快取條目），
    /// 而循環 N 次 `inc` 要多拿 N 次鎖。
    pub fn inc_by(&self, labels: &[&str], n: u64) {
        // ★ 先在鎖外判：斷言失敗的 panic 不該毒化任何一把鎖（鎖裡的方法會再判一次）。
        self.check(Kind::Counter, labels);
        lock_tolerant(&SHARD_TABLE[my_shard()].0).inc_by(self, labels, n);
    }

    /// 直接寫一個 gauge 讀數。⚠ 寫的是全域 gauge 表，不分片（見 [`GAUGES`]）。
    pub fn set(&self, labels: &[&str], v: f64) {
        self.check(Kind::Gauge, labels);
        lock_tolerant(&GAUGES).set(self, labels, v);
    }

    /// 記一次觀測。
    pub fn observe(&self, labels: &[&str], v: f64) {
        self.check(Kind::Histogram, labels);
        lock_tolerant(&SHARD_TABLE[my_shard()].0).observe(self, labels, v);
    }

    /// 把兩條契約**當場判死**：族的類型，與標籤的個數。
    ///
    /// ⚠ 兩條都用 `assert!` 而不是 `debug_assert!`：見本檔案頂部那一節。
    /// ★ 它不再順手造鍵：鍵只在一條 series 第一次出現時才造（[`LabelKey`]）。
    fn check(&self, want: Kind, labels: &[&str]) {
        assert!(
            self.kind == want,
            "指标族 `{}` 是 {}，不能按 {} 写入",
            self.name,
            self.kind.as_str(),
            want.as_str()
        );
        assert_eq!(
            labels.len(),
            self.labels.len(),
            "指标族 `{}` 声明了 {} 个标签 {:?}，这次给了 {} 个 —— 个数与顺序由声明定死",
            self.name,
            self.labels.len(),
            self.labels,
            labels.len()
        );
    }
}

impl Hist {
    /// 記一次觀測。
    fn record(&mut self, v: f64) {
        // ⚠ 桶的語義是 `le`（**小於等於**）：觀測值正好落在某個邊界上時歸**那一格**，
        //   不是下一格。`partition_point` 給的正是第一個 `bound >= v` 的下標。
        let i = BUCKETS.partition_point(|b| *b < v);
        if let Some(c) = self.per_bucket.get_mut(i) {
            *c += 1;
        }
        // ★ 超出最大桶的觀測**不進任何一格**，但照樣進 `sum` 與 `count` ——
        //   `le="+Inf"` 那一格就是靠 `count` 出來的，兩者因此恆等。
        self.sum += v;
        self.count += 1;
    }

    /// 把另一片裡同一條 series 的讀數並進來：逐桶、`sum`、`count` 各自相加。
    fn add(&mut self, other: &Hist) {
        for (a, b) in self.per_bucket.iter_mut().zip(&other.per_bucket) {
            *a += b;
        }
        self.sum += other.sum;
        self.count += other.count;
    }
}

/// ★ 三個寫方法同一個形狀：族名那一層用 `&'static str` 作鍵（`entry` 不分配），
/// 標籤那一層先拿 `&[&str]` **借用查找**，查不到才造一份 [`LabelKey`] 插進去
/// ⇒ 一條 series 只在第一次出現時分配。
impl Registry {
    fn inc_by(&mut self, f: &Family, labels: &[&str], n: u64) {
        f.check(Kind::Counter, labels);
        let m = self.counters.entry(f.name).or_default();
        if let Some(c) = m.get_mut(&labels as &dyn LabelView) {
            *c += n;
        } else {
            m.insert(LabelKey::from_labels(labels), n);
        }
    }

    fn set(&mut self, f: &Family, labels: &[&str], v: f64) {
        f.check(Kind::Gauge, labels);
        let m = self.gauges.entry(f.name).or_default();
        if let Some(g) = m.get_mut(&labels as &dyn LabelView) {
            *g = v;
        } else {
            m.insert(LabelKey::from_labels(labels), v);
        }
    }

    fn observe(&mut self, f: &Family, labels: &[&str], v: f64) {
        f.check(Kind::Histogram, labels);
        let m = self.histograms.entry(f.name).or_default();
        if let Some(h) = m.get_mut(&labels as &dyn LabelView) {
            h.record(v);
        } else {
            let mut h = Hist::default();
            h.record(v);
            m.insert(LabelKey::from_labels(labels), h);
        }
    }

    /// 抓取時把另一張表並進來（[`merged_events`] 用）：計數相加；直方圖逐桶、`sum`、`count`
    /// 相加；gauge 覆蓋（只有全域 gauge 表裡有 gauge，並進來的時候目標裡還沒有）。
    /// ★ 這裡克隆鍵是正當的：它只在抓取時跑，不在請求路徑上。
    /// ⚠ `_sum` 由各片的和相加 ⇒ 浮點加法順序與「一張表從頭加到尾」不同，末位可能有差；
    ///   單執行緒寫入時只有一片有數，結果逐位元組相同。
    fn merge_from(&mut self, other: &Registry) {
        for (name, series) in &other.counters {
            let m = self.counters.entry(*name).or_default();
            for (k, v) in series {
                *m.entry(k.clone()).or_default() += v;
            }
        }
        for (name, series) in &other.histograms {
            let m = self.histograms.entry(*name).or_default();
            for (k, h) in series {
                match m.get_mut(k) {
                    Some(t) => t.add(h),
                    None => {
                        m.insert(k.clone(), h.clone());
                    }
                }
            }
        }
        for (name, series) in &other.gauges {
            let m = self.gauges.entry(*name).or_default();
            for (k, v) in series {
                m.insert(k.clone(), *v);
            }
        }
    }

    /// 把 `families` 這張宣告表按順序繪製進 `out`。
    ///
    /// `self` 是 [`Source::Event`] 那一類的數（抓取時由 [`merged_events`] 從各片合併而來），
    /// `live` 是這一次抓取現問出來的那份臨時表（[`Source::Live`] 那一類）。
    ///
    /// ★ 收一張表當參數、而不是直接讀 `FAMILIES`：單測因此可以拿一張**小表**
    /// 逐位元組釘住輸出，而不必跟著真表一起長。
    fn render_into(&self, families: &[Family], live: &Registry, out: &mut String) {
        for f in families {
            // ★ ★ 一個族的數只從**一處**來，宣告表裡那個 `source` 說了算 ——
            //   而不是「兩邊都找一遍，哪邊有用哪邊」：後者會讓一次誤串接
            //   （在事件點寫了一個活體族）靜靜地變成一條時對時錯的 series。
            let r = match f.source {
                Source::Event => self,
                Source::Live => live,
            };
            // ★ ★ 沒有樣本的族**照樣出 HELP/TYPE**：這樣「這個指標存在，只是還沒發生過」
            //   與「名字拼錯了」在抓取端看得出區別 —— 後者是整族不見。
            //   ⚠ 活體那幾個族在**沒登記活體源**時走的正是這一支。
            let _ = writeln!(out, "# HELP {} {}", f.name, f.help);
            let _ = writeln!(out, "# TYPE {} {}", f.name, f.kind.as_str());
            match f.kind {
                Kind::Counter => {
                    for (labels, v) in r.counters.get(f.name).into_iter().flatten() {
                        write_series(out, f.name, f.labels, labels.as_slice(), None);
                        let _ = writeln!(out, " {v}");
                    }
                }
                Kind::Gauge => {
                    for (labels, v) in r.gauges.get(f.name).into_iter().flatten() {
                        write_series(out, f.name, f.labels, labels.as_slice(), None);
                        out.push(' ');
                        write_f64(out, *v);
                        out.push('\n');
                    }
                }
                Kind::Histogram => {
                    for (labels, h) in r.histograms.get(f.name).into_iter().flatten() {
                        let bucket = format!("{}_bucket", f.name);
                        let mut cum = 0u64;
                        for (bound, n) in BUCKETS.iter().zip(&h.per_bucket) {
                            cum += n;
                            write_series(
                                out,
                                &bucket,
                                f.labels,
                                labels.as_slice(),
                                Some(&format!("{bound}")),
                            );
                            let _ = writeln!(out, " {cum}");
                        }
                        // ⚠ `+Inf` 那一格寫的是 `count` 而**不是** `cum`：超出最大桶的觀測
                        //   不在任何一格里，兩者本來就不相等，而「`+Inf` 恆等於 `_count`」
                        //   是格式的要求 ⇒ 讓它們共用同一個數，別去湊。
                        write_series(out, &bucket, f.labels, labels.as_slice(), Some("+Inf"));
                        let _ = writeln!(out, " {}", h.count);
                        write_series(
                            out,
                            &format!("{}_sum", f.name),
                            f.labels,
                            labels.as_slice(),
                            None,
                        );
                        out.push(' ');
                        write_f64(out, h.sum);
                        out.push('\n');
                        write_series(
                            out,
                            &format!("{}_count", f.name),
                            f.labels,
                            labels.as_slice(),
                            None,
                        );
                        let _ = writeln!(out, " {}", h.count);
                    }
                }
            }
        }
    }
}

/// 寫一個 `f64` 讀數。
///
/// # ⚠ ⚠ Rust 的 `Display` 與 exposition 在無窮大上**不一致**
///
/// `f64::INFINITY` 被 `Display` 印成 `inf`，而 exposition 要的是 `+Inf`
/// （`NaN` 兩邊碰巧一致，不必管）。⚠ 抓取端讀到 `inf` 是**解析失敗**，
/// 而解析失敗發生在抓取端、不在這裡 —— 現場是「這一次抓取整個沒了」。
///
/// ★ 在批 M 任務 1 那一版裡這條路走不到：真表裡一個 gauge 族都沒有，
/// 而直方圖的 `_sum` 永遠是有限秒數。**任務 5 引入 gauge 之後它就走得到了。**
fn write_f64(out: &mut String, v: f64) {
    if v.is_infinite() {
        out.push_str(if v > 0.0 { "+Inf" } else { "-Inf" });
    } else {
        let _ = write!(out, "{v}");
    }
}

/// 寫出 `<name>{k="v",k2="v2"}` 那一段（**不含**後面的值）。
///
/// `le` 是直方圖那一格額外的標籤，**排在宣告的標籤後面** —— 那是 exposition 的慣例。
/// ★ 它的值是我們自己造的（桶邊界或 `+Inf`），不經轉義。
fn write_series(
    out: &mut String,
    name: &str,
    label_names: &[&str],
    values: &[Box<str>],
    le: Option<&str>,
) {
    // ⚠ 個數對不上在**寫入那一刻**已經 panic 過了（`Family::key`）。這裡是繪製路徑，
    //   一次抓取上的 panic 會把整個端點帶下去 ⇒ 只在 debug 下判。
    debug_assert_eq!(label_names.len(), values.len());
    out.push_str(name);
    if label_names.is_empty() && le.is_none() {
        return;
    }
    out.push('{');
    for (i, (k, v)) in label_names.iter().zip(values).enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{k}=\"");
        escape_into(v, out);
        out.push('"');
    }
    if let Some(le) = le {
        if !label_names.is_empty() {
            out.push(',');
        }
        let _ = write!(out, "le=\"{le}\"");
    }
    out.push('}');
}

/// 標籤值的轉義 —— **這三條就是 exposition 格式的全部**。
///
/// ⚠ 多轉一種或少轉一種，抓取端解析出來的都是**另一個值**，而不是一個錯誤。
fn escape_into(v: &str, out: &mut String) {
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
}

/// 抓取時那一坨文字（Prometheus text exposition，`version=0.0.4`）。
///
/// ★ 事件點那一類的數由 [`merged_events`] **逐片**合併而來：每一片只在合併它的那一下被鎖住，
/// 請求路徑上的寫最多被擋一片的合併時長，⛔ 不是整個繪製過程。格式化在任何鎖之外。
/// ★ ★ **問活體那一步在拿任何註冊表鎖之前**：`Upstream::inflight()` 與 `SniResolver::expiries()`
/// 各有自己的鎖，而在註冊表的鎖裡面去拿別人的鎖，就是把兩把鎖排出了一個順序 ——
/// 而那個順序**沒有任何東西在守**。
pub fn render() -> String {
    let live = match live().get() {
        Some(src) => snapshot(src),
        // ⚠ 沒登記 ⇒ 那幾個族只出 HELP/TYPE、不出樣本（`build_info` 除外，它不需要活體源）。
        None => snapshot(&LiveSources::default()),
    };
    let events = merged_events();
    let mut out = String::with_capacity(4096);
    events.render_into(&FAMILIES, &live, &mut out);
    out
}

/// 只給測試用：按給定的活體源把**真表**（`FAMILIES`）渲成文字，**繞開**
/// [`live()`] 那個處理程序級 `OnceLock`（**M2 批 N 任務 6**）。
///
/// ★ ★ ★ **為什麼不能讓判據走 [`register_live`] + [`render`]**：`register_live`
/// 全處理程序只認第一次，而 `cargo test` 裡同一個二進位裝著幾十個測試——誰先跑到
/// 誰就把這個 `OnceLock` 焊死，後面的測試要麼讀到別人的活體源，要麼在它已經
/// 焊死之後徒勞地 `register_live` 一次、只落一行 warn。⇒ 本函式直接走
/// `snapshot()` + `render_into()`，與 [`render`] 唯一的差別是資料源從參數來，
/// 不摸 `live()`——本模組自己的判據（見下面 `按活体源渲真表`）早就是這麼測的，
/// 這裡只是把同一個手法開一個 `pub(crate)` 口子給 `admin.rs` 用。
///
/// # 用途：`admin.rs` 的 R12「同源」判據
///
/// `/stats` 按站台 × 上游列（不歸併），`/metrics` 按位址歸併——判據要證明
/// 兩者讀的是**同一組原子量**。⇒ 判據裡真正呼叫**這一個**函式、而不是在
/// `admin.rs` 裡手寫一份「求和 / 取合取」——那樣寫出來的判據只是在跟自己的
/// 抄件比對，量不到 `/metrics` 那一側的真實實作有沒有走岔。
#[cfg(test)]
pub(crate) fn render_snapshot_for_test(src: &LiveSources) -> String {
    let mut out = String::new();
    Registry::default().render_into(&FAMILIES, &snapshot(src), &mut out);
    out
}

/// 一張現造的自簽證書。★ rcgen 已經是本 crate 的 dev-dependency（QUIC 那條判據在用）。
///
/// ★ `pub(crate)`（**M2 批 N 任務 6**）：同一個理由——`admin.rs` 的 `/stats`
/// 證書到期判據要往 `SniResolver` 裡塞一張真證書，別在那邊再寫一份一模一樣的
/// rcgen 呼叫。
#[cfg(test)]
pub(crate) fn 自签(domain: &str) -> std::sync::Arc<fulcrum_tls::CertKey> {
    let key = rcgen::KeyPair::generate().expect("测试密钥");
    let params = rcgen::CertificateParams::new(vec![domain.to_string()]).expect("测试参数");
    let cert = params.self_signed(&key).expect("自签");
    fulcrum_tls::cert_key_from_der(cert.der().to_vec(), key.serialize_der()).expect("造 CertKey")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一張**測試專用**的小宣告表。
    ///
    /// ★ 與真表分開，是因為 golden 輸出要逐位元組穩定，而真表會隨批 M 長大 ——
    /// 讓 golden 掛在真表上，等於每加一個族就要改一次判據，而改判據的人正是加族的人。
    const T: [Family; 4] = [
        Family {
            name: "t_requests_total",
            kind: Kind::Counter,
            source: Source::Event,
            help: "请求数。",
            labels: &["site", "outcome"],
            domains: &[Domain::Config, Domain::Config],
        },
        // ★ 一個**不帶標籤**的族：它繪製出來沒有那對花括號。
        Family {
            name: "t_ready",
            kind: Kind::Gauge,
            source: Source::Event,
            help: "就绪。",
            labels: &[],
            domains: &[],
        },
        Family {
            name: "t_latency_seconds",
            kind: Kind::Histogram,
            source: Source::Event,
            help: "时延，秒。",
            labels: &["route"],
            domains: &[Domain::Config],
        },
        // ★ 一個**一條樣本都沒有**的族：它照樣要出 HELP/TYPE。
        Family {
            name: "t_never_written_total",
            kind: Kind::Counter,
            source: Source::Event,
            help: "一次都没发生过。",
            labels: &["x"],
            domains: &[Domain::Config],
        },
    ];
    const T_REQUESTS: &Family = &T[0];
    const T_READY: &Family = &T[1];
    const T_LATENCY: &Family = &T[2];

    /// 繪製一張**只有事件點記賬**的表時，活體那一半是空的。
    ///
    /// ★ 抽成一個 helper，是為了讓下面那幾條 golden 判據讀起來仍然只有「表 + 輸出」兩樣。
    fn render_events(r: &Registry, families: &[Family], out: &mut String) {
        r.render_into(families, &Registry::default(), out);
    }

    /// counter 的名字必須以 `_total` 結尾；gauge / histogram 一個都不許。
    fn 后缀合规(f: &Family) -> bool {
        match f.kind {
            Kind::Counter => f.name.ends_with("_total"),
            Kind::Gauge | Kind::Histogram => !f.name.ends_with("_total"),
        }
    }

    #[test]
    fn counter_都以_total_结尾_而_gauge_与_histogram_都不许() {
        // ★ 先證「樣本里真有東西」：一張空表也能讓下面那個 for 全綠。
        let counters = FAMILIES.iter().filter(|f| f.kind == Kind::Counter).count();
        assert!(
            counters > 0,
            "声明表里一个 counter 都没有 —— 正向那半是空的"
        );
        assert!(
            FAMILIES.len() - counters > 0,
            "声明表里只有 counter —— 反向那半是空的"
        );

        for f in &FAMILIES {
            assert!(
                后缀合规(f),
                "族 `{}`（{}）的名字不合规",
                f.name,
                f.kind.as_str()
            );
        }

        // ★ ★ 反向那半：**這條規則判得動壞名字**。只對真表斷言的話，把 `后缀合规`
        //   改成恆 `true` 一樣全綠 —— 那時這條判據就已經不存在了，而沒有任何跡象。
        let 坏表 = [
            Family {
                name: "bad_counter",
                kind: Kind::Counter,
                source: Source::Event,
                help: "",
                labels: &[],
                domains: &[],
            },
            Family {
                name: "bad_gauge_total",
                kind: Kind::Gauge,
                source: Source::Event,
                help: "",
                labels: &[],
                domains: &[],
            },
            Family {
                name: "bad_hist_total",
                kind: Kind::Histogram,
                source: Source::Event,
                help: "",
                labels: &[],
                domains: &[],
            },
        ];
        for f in &坏表 {
            assert!(!后缀合规(f), "`{}` 本该被判不合规", f.name);
        }
    }

    #[test]
    fn 族句柄指的就是它名字上那个族_而且表里没有重名() {
        // ⚠ 句柄是按**下標**指進宣告表的 —— 重排那張表會讓句柄悄悄換個族。
        //   這一條是那件事唯一會露頭的地方。
        assert_eq!(REQUESTS_TOTAL.name, "fulcrum_requests_total");
        assert_eq!(REQUESTS_TOTAL.kind, Kind::Counter);
        assert_eq!(
            REQUESTS_TOTAL.labels,
            &["site", "outcome", "status_class", "proto"]
        );
        assert_eq!(
            REQUEST_DURATION_SECONDS.name,
            "fulcrum_request_duration_seconds"
        );
        assert_eq!(REQUEST_DURATION_SECONDS.kind, Kind::Histogram);
        assert_eq!(REQUEST_DURATION_SECONDS.labels, &["site", "outcome"]);
        assert_eq!(CACHE_EVENTS_TOTAL.name, "fulcrum_cache_events_total");
        assert_eq!(CACHE_EVENTS_TOTAL.labels, &["event"]);
        assert_eq!(NO_SITE_MATCH_TOTAL.name, "fulcrum_no_site_match_total");
        assert_eq!(NO_SITE_MATCH_TOTAL.labels, &["host"]);
        assert_eq!(UPSTREAM_INFLIGHT.name, "fulcrum_upstream_inflight");
        assert_eq!(UPSTREAM_INFLIGHT.labels, &["upstream"]);
        assert_eq!(UPSTREAM_HEALTHY.name, "fulcrum_upstream_healthy");
        assert_eq!(UPSTREAM_HEALTHY.labels, &["upstream"]);
        assert_eq!(CERT_EXPIRY_SECONDS.name, "fulcrum_cert_expiry_seconds");
        assert_eq!(CERT_EXPIRY_SECONDS.labels, &["domain"]);
        assert_eq!(ACME_ISSUE_TOTAL.name, "fulcrum_acme_issue_total");
        assert_eq!(ACME_ISSUE_TOTAL.labels, &["result"]);
        // ⛔ `build_info` 只許帶 `version` —— 加上 `gen_id` 或 pid 會讓每次換代
        //   長出一條新 series，而舊的那條從此再也不更新。
        assert_eq!(BUILD_INFO.name, "fulcrum_build_info");
        assert_eq!(BUILD_INFO.labels, &["version"]);
        // ⛔ 不按 (站台, 上游) 打標籤——那等於把 `/stats` 的 `overrides` 一節整個
        //   搬進指標（兩份實作同一件事）。這個族**無標籤**。
        assert_eq!(OVERRIDES_ACTIVE.name, "fulcrum_overrides_active");
        assert!(
            OVERRIDES_ACTIVE.labels.is_empty(),
            "fulcrum_overrides_active 不该带任何标签"
        );
        // ★ ★ `kind` 必須**直接**釘住（評審 M6）：把基數表釘在 `FAMILIES` 上的那道門
        //   只逐項比**族名**，`gauge / 活体 / 无 / 恒为 1` 那四格它一個都不讀 ——
        //   文件那一行寫成 `counter | 事件 | site | 无上界` 照樣是綠的。
        //   ⚠ 而 `Kind` 寫錯在別處只是**間接**露頭（`Counter` 會撞 `_total` 命名規則、
        //   `Histogram` 會讓樣本數掉到 0），間接的判據紅起來指的是別的地方。
        assert_eq!(OVERRIDES_ACTIVE.kind, Kind::Gauge);
        // ⛔ 無標籤：`POST /purge` 的三種粒度不做標籤（G123）。
        assert_eq!(
            CACHE_PURGED_ENTRIES_TOTAL.name,
            "fulcrum_cache_purged_entries_total"
        );
        assert_eq!(CACHE_PURGED_ENTRIES_TOTAL.kind, Kind::Counter);
        assert!(
            CACHE_PURGED_ENTRIES_TOTAL.labels.is_empty(),
            "fulcrum_cache_purged_entries_total 不该带任何标签"
        );
        // ⛔ 只有 `version` / `cipher` 兩格，**順序也是契約**（G122）——
        //   ⛔ 不許加 `sni` / `alpn`：那兩個是**訪問者給的**，與 G121 擋住 `host` 同一件事。
        assert_eq!(TLS_REQUESTS_TOTAL.name, "fulcrum_tls_requests_total");
        assert_eq!(TLS_REQUESTS_TOTAL.kind, Kind::Counter);
        assert_eq!(TLS_REQUESTS_TOTAL.labels, &["version", "cipher"]);
        // ⛔ 標籤叫 `entrypoint` 不叫 `proto`（`REQUESTS_TOTAL` 已佔用 `proto`，
        //   取值域完全不同 ⇒ 跨族寫同名過濾器會拿到空集而不回報錯誤）。
        //   ⚠ 順序也是契約：`{listen, entrypoint}` 與 `{entrypoint, listen}` 繪製出來
        //   是兩條不同的 series，而那種錯在輸出裡看起來完全正常。
        assert_eq!(CONNECTIONS_TOTAL.name, "fulcrum_connections_total");
        assert_eq!(CONNECTIONS_TOTAL.kind, Kind::Counter);
        assert_eq!(CONNECTIONS_TOTAL.labels, &["listen", "entrypoint"]);
        assert_eq!(CONNECTIONS_ACTIVE.name, "fulcrum_connections_active");
        assert_eq!(CONNECTIONS_ACTIVE.kind, Kind::Gauge);
        assert_eq!(CONNECTIONS_ACTIVE.labels, &["listen", "entrypoint"]);
        // ★ G136。⚠ 它的句柄取的是 `FAMILIES.len() - 1` 而不是一個寫死的下標 ——
        //   下一個人往末尾再追加一條時，這條斷言會當場紅並指著這裡。
        assert_eq!(UPSTREAM_PASSIVE_OPEN.name, "fulcrum_upstream_passive_open");
        assert_eq!(UPSTREAM_PASSIVE_OPEN.kind, Kind::Gauge);
        assert_eq!(UPSTREAM_PASSIVE_OPEN.labels, &["upstream"]);

        // ★ ★ 每個族的**來處**也逐個釘住：`source` 寫錯不會讓任何一條內容斷言變紅 ——
        //   活體族被標成 `Event` 的表現是它**永遠沒有樣本**，而那與「沒接上活體源」
        //   長得一模一樣；事件族被標成 `Live` 的表現是它**永遠是 0**。
        let live: Vec<&str> = FAMILIES
            .iter()
            .filter(|f| f.source == Source::Live)
            .map(|f| f.name)
            .collect();
        assert_eq!(
            live,
            vec![
                "fulcrum_upstream_inflight",
                "fulcrum_upstream_healthy",
                "fulcrum_cert_expiry_seconds",
                "fulcrum_acme_issue_total",
                "fulcrum_build_info",
                "fulcrum_overrides_active",
                // ⚠ counter 走 `Live` 不是筆誤：連接數累加在 `conn_stats` 的註冊表裡
                //   而不是本模組的 `Registry` 裡 ⇒ 從本模組看它就是「抓取時問活體」。
                //   先例是上面的 `fulcrum_acme_issue_total`。
                "fulcrum_connections_total",
                "fulcrum_connections_active",
                "fulcrum_upstream_passive_open",
            ]
        );

        let mut names: Vec<&str> = FAMILIES.iter().map(|f| f.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "声明表里有重名的族");
    }

    #[test]
    fn 取不到的标签值记_unknown_而不是空串() {
        // ── 正向：空串 ⇒ 記號 ────────────────────────────────────────────────
        //
        // ⚠ ⚠ 記空串在 Prometheus 資料模型裡等同於「這個標籤不存在」⇒ 「今天取不到」
        //   與「明天把這個標籤整個刪掉」在抓取端分不開（G127）。
        assert_eq!(label_or_unknown(""), "<unknown>");
        assert_eq!(label_or_unknown(""), UNKNOWN_LABEL);

        // ── 反向：**有值的一律原樣** ──────────────────────────────────────────
        //
        // ★ 少了這半條，一個「恆答 `<unknown>`」的實作會通過上半條 —— 而它把
        //   每一條 TLS series 都併成同一格，讀起來是「一個套件都沒協商出來」。
        for 真值 in ["TLSv1.3", "TLSv1.2", "TLS_AES_256_GCM_SHA384", " ", "0"] {
            assert_eq!(
                label_or_unknown(真值),
                真值,
                "有值的标签必须原样记 —— `{真值}` 被改写了"
            );
        }

        // ── ★ 記號撞不上真值 ────────────────────────────────────────────────
        //
        // 尖括號在 TLS 版本名與套件名裡不可能出現 ⇒ `<unknown>` 與任何一個真值
        // 都區分得開。⚠ 這一條守的是**記號的選取**，不是替換邏輯：換一個可能
        // 與真值撞車的記號（比如 `none`）時，上面兩條一條都不會紅。
        assert!(
            UNKNOWN_LABEL.starts_with('<') && UNKNOWN_LABEL.ends_with('>'),
            "记号要用尖括号形状，才与 <other> / <none> 同一族且撞不上真值"
        );

        // ★ ★ 「判據落在值為空上、⛔ 不落在是不是 h3 上」這條**由簽名保證**：
        //   `label_or_unknown` 收的就只有那個值，看不見呼叫方走的是哪種傳輸。
        //   ⇒ 這裡沒有對應的斷言可寫，那正是它比一條斷言更強的地方。
    }

    #[test]
    fn help_文本里没有需要转义的字符() {
        // ★ `# HELP` 那一行不轉義 ⇒ 約束落在宣告這一側，而不是繪製那一側。
        for f in &FAMILIES {
            assert!(
                !f.help.contains('\n') && !f.help.contains('\\'),
                "族 `{}` 的 HELP 文本带了会把 exposition 撕坏的字符",
                f.name
            );
        }
    }

    #[test]
    fn 标签值只转义那三种字符_其余原样保留() {
        const E: [Family; 1] = [Family {
            name: "t_escape_total",
            kind: Kind::Counter,
            source: Source::Event,
            help: "转义。",
            labels: &["v"],
            domains: &[Domain::Config],
        }];
        let 原值 = "a\"b\\c\nd\te/f'g=h{i}中";
        // ★ 先證樣本里真有那三種字元 —— 否則下面那條 golden 只是在量一個普通字串。
        assert!(原值.contains('"') && 原值.contains('\\') && 原值.contains('\n'));
        // ★ 也要有**不該被碰**的字元：否則「把每個字元都轉義掉」照樣能通過上半條。
        assert!(原值.contains('\t') && 原值.contains('\'') && 原值.contains('中'));

        let mut r = Registry::default();
        r.inc_by(&E[0], &[原值], 1);
        let mut out = String::new();
        render_events(&r, &E, &mut out);

        let 期望 = [
            "# HELP t_escape_total 转义。",
            "# TYPE t_escape_total counter",
            "t_escape_total{v=\"a\\\"b\\\\c\\nd\te/f'g=h{i}中\"} 1",
            "",
        ]
        .join("\n");
        assert_eq!(out, 期望);
    }

    #[test]
    fn 直方图的桶是累积的_inf_那格等于_count_而_sum_是观测值之和() {
        // ★ 這一組值有意覆蓋四種位置：比最小桶還小、**正好落在邊界上**、
        //   夾在兩個邊界之間、比最大桶還大。
        const V: [f64; 8] = [0.001, 0.25, 0.5, 0.75, 2.5, 5.0, 10.0, 30.0];
        let mut r = Registry::default();
        for v in V {
            r.observe(T_LATENCY, &["/x"], v);
        }
        let mut out = String::new();
        render_events(&r, &T, &mut out);

        let 期望累积: [u64; 11] = [1, 1, 1, 1, 1, 2, 3, 4, 5, 6, 7];
        assert_eq!(
            期望累积.len(),
            BUCKETS.len(),
            "桶改过了，这张期望表要一起改"
        );
        for (bound, want) in BUCKETS.iter().zip(期望累积) {
            let line = format!("t_latency_seconds_bucket{{route=\"/x\",le=\"{bound}\"}} {want}");
            assert!(
                out.lines().any(|l| l == line),
                "没找到这一行：{line}\n--- 实际输出 ---\n{out}"
            );
        }
        // ⚠ 30.0 超出最大桶 ⇒ 它不在任何一格里，但必須在 `+Inf` 與 `_count` 裡。
        assert!(out.contains("t_latency_seconds_bucket{route=\"/x\",le=\"+Inf\"} 8\n"));
        assert!(out.contains("t_latency_seconds_count{route=\"/x\"} 8\n"));
        // ★ 最後一個有限桶是 7 而不是 8 —— 少了這一條，「+Inf == _count」就是句廢話。
        assert!(out.contains("t_latency_seconds_bucket{route=\"/x\",le=\"10\"} 7\n"));

        let 期望和: f64 = V.iter().sum();
        assert!((期望和 - 49.001).abs() < 1e-9, "样本之和变了：{期望和}");
        assert!(out.contains(&format!("t_latency_seconds_sum{{route=\"/x\"}} {期望和}\n")));
    }

    #[test]
    fn 同一份数据渲染两次逐字节相同_且与写入顺序无关() {
        let mut a = Registry::default();
        a.inc_by(T_REQUESTS, &["a.example", "files"], 1);
        a.inc_by(T_REQUESTS, &["b.example", "reverse_proxy"], 2);
        a.observe(T_LATENCY, &["/z"], 0.3);
        a.set(T_READY, &[], 1.0);

        // ⚠ 反著寫一遍：「寫入順序洩漏進輸出」是換成 `HashMap` 之後的失效形態，
        //   而它**不會讓任何一條內容斷言變紅** —— 除非像這裡一樣直接比兩份輸出。
        let mut b = Registry::default();
        b.set(T_READY, &[], 1.0);
        b.observe(T_LATENCY, &["/z"], 0.3);
        b.inc_by(T_REQUESTS, &["b.example", "reverse_proxy"], 2);
        b.inc_by(T_REQUESTS, &["a.example", "files"], 1);

        let mut s1 = String::new();
        render_events(&a, &T, &mut s1);
        let mut s2 = String::new();
        render_events(&a, &T, &mut s2);
        let mut s3 = String::new();
        render_events(&b, &T, &mut s3);

        assert!(!s1.is_empty());
        assert_eq!(s1, s2, "同一份数据渲染两次不一样");
        assert_eq!(s1, s3, "写入顺序泄漏进了输出");
    }

    #[test]
    fn 一份小注册表的整段输出() {
        let mut r = Registry::default();
        // ⚠ 有意先寫 `b.` 再寫 `a.`：輸出必須按標籤值排序，不按寫入順序。
        r.inc_by(T_REQUESTS, &["b.example", "reverse_proxy"], 2);
        r.inc_by(T_REQUESTS, &["a.example", "files"], 1);
        r.set(T_READY, &[], 1.0);
        r.observe(T_LATENCY, &["/api"], 0.25);
        r.observe(T_LATENCY, &["/api"], 3.0);

        let mut out = String::new();
        render_events(&r, &T, &mut out);

        // ★ 用陣列 join 拼期望值，而不是一個多行字串字面值：宿主是 Windows，
        //   多行字面值會把**原始檔的行尾**一起量進判據裡（`.gitattributes` 釘了 LF，
        //   但一條判據不該把自己的正確性掛在另一處設定上）。
        let 期望 = [
            "# HELP t_requests_total 请求数。",
            "# TYPE t_requests_total counter",
            "t_requests_total{site=\"a.example\",outcome=\"files\"} 1",
            "t_requests_total{site=\"b.example\",outcome=\"reverse_proxy\"} 2",
            "# HELP t_ready 就绪。",
            "# TYPE t_ready gauge",
            "t_ready 1",
            "# HELP t_latency_seconds 时延，秒。",
            "# TYPE t_latency_seconds histogram",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.005\"} 0",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.01\"} 0",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.025\"} 0",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.05\"} 0",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.1\"} 0",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.25\"} 1",
            "t_latency_seconds_bucket{route=\"/api\",le=\"0.5\"} 1",
            "t_latency_seconds_bucket{route=\"/api\",le=\"1\"} 1",
            "t_latency_seconds_bucket{route=\"/api\",le=\"2.5\"} 1",
            "t_latency_seconds_bucket{route=\"/api\",le=\"5\"} 2",
            "t_latency_seconds_bucket{route=\"/api\",le=\"10\"} 2",
            "t_latency_seconds_bucket{route=\"/api\",le=\"+Inf\"} 2",
            "t_latency_seconds_sum{route=\"/api\"} 3.25",
            "t_latency_seconds_count{route=\"/api\"} 2",
            "# HELP t_never_written_total 一次都没发生过。",
            "# TYPE t_never_written_total counter",
            "",
        ]
        .join("\n");
        assert_eq!(out, 期望);
    }

    #[test]
    #[should_panic(expected = "声明了 2 个标签")]
    fn 标签个数对不上时就地_panic() {
        let mut r = Registry::default();
        r.inc_by(T_REQUESTS, &["只给了一个"], 1);
    }

    #[test]
    #[should_panic(expected = "不能按 gauge 写入")]
    fn 族的类型对不上时也就地_panic() {
        let mut r = Registry::default();
        r.set(T_REQUESTS, &["a.example", "files"], 1.0);
    }

    /// ★ ★ 新鍵的序就是繪製順序（族內 series 按標籤值排序）⇒ 它必須與舊鍵
    /// `Vec<String>` 的序**逐條相同**；而借用查找要成立，借用視圖與鍵兩兩比較的
    /// 結果也必須逐對相同（`Borrow` 的契約 —— 違反了不會回報錯誤，只會查不到）。
    /// ⚠ 樣本里有意放了寬度不同的幾條：只有它們分得開「先比寬度、再逐元素比」那種寫錯法。
    #[test]
    fn 标签键的序与_vec_string_一致_借用查找也命中() {
        let samples: Vec<Vec<&str>> = vec![
            vec!["a"],
            vec!["a", "b"],
            vec!["b"],
            vec![""],
            vec!["a.example", "x"],
            vec!["a.examplf"],
            vec!["<none>"],
            vec!["<other>"],
            vec!["<unknown>"],
            vec!["中", "a"],
            vec!["Z"],
        ];

        let mut by_vec: Vec<Vec<String>> = samples
            .iter()
            .map(|s| s.iter().map(|v| v.to_string()).collect())
            .collect();
        by_vec.sort();
        let mut by_key: Vec<LabelKey> = samples.iter().map(|s| LabelKey::from_labels(s)).collect();
        by_key.sort();
        let by_key_as_vec: Vec<Vec<String>> = by_key
            .iter()
            .map(|k| k.as_slice().iter().map(|v| v.to_string()).collect())
            .collect();
        assert_eq!(by_key_as_vec, by_vec, "新键的序与 Vec<String> 的序不同");

        // ⚠ `LabelView` 實作在 `&[&str]`（引用類型）上 ⇒ 先綁成 `&[&str]` 再取引用轉 `&dyn`。
        for a in &samples {
            for b in &samples {
                let (av, bv): (&[&str], &[&str]) = (&a[..], &b[..]);
                assert_eq!(
                    LabelKey::from_labels(av).cmp(&LabelKey::from_labels(bv)),
                    (&av as &dyn LabelView).cmp(&bv as &dyn LabelView),
                    "借用视图与键的比较结果不同：{av:?} vs {bv:?}"
                );
            }
        }

        let mut m: BTreeMap<LabelKey, usize> = BTreeMap::new();
        for (i, s) in samples.iter().enumerate() {
            m.insert(LabelKey::from_labels(s), i);
        }
        for (i, s) in samples.iter().enumerate() {
            let sv: &[&str] = &s[..];
            assert_eq!(
                m.get(&sv as &dyn LabelView),
                Some(&i),
                "借用查找没命中：{sv:?}"
            );
        }
    }

    #[test]
    fn 写入_api_走的是进程级那张表_而_render_出全部声明的族() {
        // ⚠ 這一條走**真的**處理程序級註冊表（別的判據都用局部註冊表，否則同一個測試
        //   二進位裡幾條測試會把數寫進對方的斷言裡）。
        //   ★ 下面這幾組標籤值只此一處用過 ⇒ 計數是確定的。
        REQUESTS_TOTAL.inc(&["<unit-test>", "metrics", "2xx", "HTTP/1.1"]);
        REQUEST_DURATION_SECONDS.observe(&["<unit-test>", "metrics"], 0.25);
        NO_SITE_MATCH_TOTAL.inc_by(&["<unit-test-other>"], 3);
        // ★ `set` 那條路也要走一遍，而真表裡今天沒有 gauge 族 ⇒ 借測試表的那個。
        //   它不在 `FAMILIES` 裡，所以 `render()` 不會把它渲出來 —— 下面單獨按 `T` 渲一次。
        T_READY.set(&[], 1.0);

        let out = render();
        for f in &FAMILIES {
            assert!(
                out.contains(&format!("# HELP {} ", f.name)),
                "缺 HELP：{}",
                f.name
            );
            assert!(
                out.contains(&format!("# TYPE {} {}\n", f.name, f.kind.as_str())),
                "缺 TYPE：{}",
                f.name
            );
        }
        assert!(
            out.contains(
                "fulcrum_requests_total{site=\"<unit-test>\",outcome=\"metrics\",status_class=\"2xx\",proto=\"HTTP/1.1\"} 1\n"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "fulcrum_request_duration_seconds_count{site=\"<unit-test>\",outcome=\"metrics\"} 1\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("fulcrum_no_site_match_total{host=\"<unit-test-other>\"} 3\n"),
            "{out}"
        );
        // ★ 整體以換行收尾 —— exposition 的最後一行也得是一行。
        assert!(out.ends_with('\n'));

        let 合并后 = merged_events();
        let mut 全局按测试表渲染 = String::new();
        render_events(&合并后, &T, &mut 全局按测试表渲染);
        assert!(
            全局按测试表渲染.contains("\nt_ready 1\n"),
            "{全局按测试表渲染}"
        );
    }

    // ── 註冊表按執行緒分片（G156）────────────────────────────────────────────
    //
    // ⚠ 下面幾條走**真的**處理程序級片表，標籤值都只在本條裡出現 ⇒ 別的測試並行寫進來的數
    //   落在別的 series 上，不會混進斷言。

    /// 繪製結果裡取一條計數行的值；還沒出現過就是 0。
    fn 计数行的值(out: &str, 前缀: &str) -> u64 {
        out.lines()
            .find_map(|l| l.strip_prefix(前缀))
            .map(|v| v.parse().expect("计数行的值不是整数"))
            .unwrap_or(0)
    }

    /// ★ ★ ★ 請求路徑上沒有全處理程序共用的鎖：別的 63 片與 gauge 表全被握住時，
    /// 一個執行緒照樣寫得進自己那一片。⚠ 一個「多拿了一把共用鎖」的寫路徑在這裡會卡住
    /// —— 那正是分片要消滅的東西，而輸出上它與正確的實作一模一樣。
    #[test]
    fn 写入不碰别的片也不碰任何全进程的锁() {
        let req = ["<lock-test>", "metrics", "2xx", "HTTP/1.1"];
        let dur = ["<lock-test>", "metrics"];
        let (to_main, from_b) = std::sync::mpsc::channel::<usize>();
        let (go, wait_go) = std::sync::mpsc::channel::<()>();
        let b = std::thread::spawn(move || {
            REQUESTS_TOTAL.inc(&req);
            REQUEST_DURATION_SECONDS.observe(&dur, 0.25);
            to_main.send(my_shard()).unwrap();
            wait_go.recv().unwrap();
            for _ in 0..1000 {
                REQUESTS_TOTAL.inc(&req);
                REQUEST_DURATION_SECONDS.observe(&dur, 0.25);
            }
            to_main.send(usize::MAX).unwrap();
        });
        let shard = from_b.recv().unwrap();
        let guards = lock_all_but(shard);
        go.send(()).unwrap();
        let done = from_b.recv_timeout(std::time::Duration::from_secs(10));
        drop(guards);
        b.join().unwrap();
        assert!(
            done.is_ok(),
            "握住别的 63 片与 gauge 表时，另一个线程写不进自己那一片（写路径上有一把共用的锁）"
        );
    }

    /// ★ 寫執行緒多於片數（幾個執行緒共用一片）、同時有人在抓：計數仍然精確，
    /// 相鄰兩次抓取裡同一條計數不減少，最後一次抓到的恰好是總數。
    #[test]
    fn 多于片数的线程并发写_边写边抓_合计精确() {
        const THREADS: usize = SHARDS + 16;
        const ROUNDS: u64 = 250;
        const 计数前缀: &str = "fulcrum_requests_total{site=\"<shard-test>\",outcome=\"metrics\",status_class=\"2xx\",proto=\"HTTP/1.1\"} ";
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut last = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let v = 计数行的值(&render(), 计数前缀);
                    assert!(v >= last, "相邻两次抓取里计数减少了：{last} → {v}");
                    last = v;
                }
            })
        };
        let writers: Vec<_> = (0..THREADS)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..ROUNDS {
                        REQUESTS_TOTAL.inc(&["<shard-test>", "metrics", "2xx", "HTTP/1.1"]);
                        REQUEST_DURATION_SECONDS.observe(&["<shard-test>", "metrics"], 0.5);
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();

        let out = render();
        let 总数 = THREADS as u64 * ROUNDS;
        assert_eq!(计数行的值(&out, 计数前缀), 总数, "{out}");
        let d = "{site=\"<shard-test>\",outcome=\"metrics\"";
        for line in [
            format!("fulcrum_request_duration_seconds_count{d}}} {总数}"),
            format!("fulcrum_request_duration_seconds_sum{d}}} {}", 总数 / 2),
            format!("fulcrum_request_duration_seconds_bucket{d},le=\"0.5\"}} {总数}"),
            format!("fulcrum_request_duration_seconds_bucket{d},le=\"0.25\"}} 0"),
        ] {
            assert!(out.lines().any(|l| l == line), "没找到：{line}\n{out}");
        }
    }

    /// ★ 合併對輸出透明：同一批事件拆給兩片寫再合併，與一片直寫，繪製逐位元組相同。
    /// 觀測值取二進位可精確表示的數 ⇒ `_sum` 不受加法順序影響，這裡可以要求逐位元組。
    #[test]
    fn 同一批事件分到两片合并与一片直写_渲染逐字节相同() {
        let mut a = Registry::new();
        let mut b = Registry::new();
        let mut c = Registry::new();
        a.inc_by(T_REQUESTS, &["a.example", "files"], 2);
        b.inc_by(T_REQUESTS, &["a.example", "files"], 3);
        b.inc_by(T_REQUESTS, &["b.example", "reverse_proxy"], 1);
        a.observe(T_LATENCY, &["/x"], 0.5);
        b.observe(T_LATENCY, &["/x"], 0.25);
        b.observe(T_LATENCY, &["/x"], 3.0);
        b.set(T_READY, &[], 1.0);

        c.inc_by(T_REQUESTS, &["a.example", "files"], 5);
        c.inc_by(T_REQUESTS, &["b.example", "reverse_proxy"], 1);
        c.observe(T_LATENCY, &["/x"], 0.5);
        c.observe(T_LATENCY, &["/x"], 0.25);
        c.observe(T_LATENCY, &["/x"], 3.0);
        c.set(T_READY, &[], 1.0);

        let mut merged = Registry::new();
        merged.merge_from(&a);
        merged.merge_from(&b);
        let mut s1 = String::new();
        render_events(&merged, &T, &mut s1);
        let mut s2 = String::new();
        render_events(&c, &T, &mut s2);
        assert!(
            s2.contains("t_latency_seconds_count{route=\"/x\"} 3\n"),
            "{s2}"
        );
        assert_eq!(s1, s2, "合并两片的渲染与一片直写不一样");
    }

    /// ★ 標籤對不上的 panic 發生在**拿鎖之前** ⇒ 不毒化任何一片；同一執行緒接著寫照常。
    #[test]
    fn 标签对不上的_panic_不毒化任何片() {
        let shard = std::thread::spawn(|| {
            let r = std::panic::catch_unwind(|| REQUESTS_TOTAL.inc(&["只给了一个"]));
            assert!(r.is_err(), "标签个数不对却没有 panic");
            // ★ gauge 走的是另一把鎖（全域 gauge 表）⇒ 它那條路也要真的 panic 一次，
            //   下面「gauge 表沒被毒化」那條斷言才紅得起來。`T_READY` 宣告了 0 個標籤。
            let g = std::panic::catch_unwind(|| T_READY.set(&["多给了一个"], 1.0));
            assert!(g.is_err(), "gauge 标签个数不对却没有 panic");
            REQUESTS_TOTAL.inc(&["<poison-test>", "metrics", "2xx", "HTTP/1.1"]);
            my_shard()
        })
        .join()
        .unwrap();
        assert!(!SHARD_TABLE[shard].0.is_poisoned(), "第 {shard} 片被毒化了");
        assert!(!GAUGES.is_poisoned(), "gauge 表被毒化了");
        assert!(
            render().contains(
                "fulcrum_requests_total{site=\"<poison-test>\",outcome=\"metrics\",status_class=\"2xx\",proto=\"HTTP/1.1\"} 1\n"
            ),
            "panic 之后同一线程写的那一笔没记上"
        );
    }

    /// ★ 執行緒拆 TLS 的時候還在寫（別的 thread-local 的析構裡寫指標）：不 panic、數記得上。
    ///
    /// ⚠ 順序是承重的：先摸 `ON_EXIT`（它的析構先登記）、**再**寫一筆（`MY_SHARD` 後初始化）。
    ///   TLS 析構按登記的逆序跑 ⇒ 一旦 `MY_SHARD` 換成帶析構的類型，它會先於 `ON_EXIT` 被拆，
    ///   而 `ON_EXIT` 的析構裡那一筆就得走 [`my_shard`] 的備援分支 —— 刪了備援（`try_with` 換成
    ///   `with`）就在 TLS 析構裡 panic、處理程序 abort，本條紅。今天 `MY_SHARD` 無析構，本條守的是那次改動。
    #[test]
    fn 线程退出时析构里写指标也记得上() {
        struct WriteOnExit;
        impl Drop for WriteOnExit {
            fn drop(&mut self) {
                NO_SITE_MATCH_TOTAL.inc(&["<tls-teardown>"]);
            }
        }
        thread_local! {
            static ON_EXIT: WriteOnExit = const { WriteOnExit };
        }
        std::thread::spawn(|| {
            ON_EXIT.with(|_| {});
            NO_SITE_MATCH_TOTAL.inc(&["<tls-teardown-pre>"]);
        })
        .join()
        .unwrap();
        assert!(
            render().contains("fulcrum_no_site_match_total{host=\"<tls-teardown>\"} 1\n"),
            "线程退出时析构里写的那一笔没记上"
        );
    }

    // ── 抓取時問活體那一半（**批 M 任務 5**）────────────────────────────────

    /// 把真表按給定的活體源渲一遍。★ 事件那一半用一張**空的**註冊表，
    /// 於是每一條斷言看到的都只是活體那一半 —— 否則同一個測試二進位裡別的判據
    /// 寫進處理程序級表的數會漏進來。
    ///
    /// ★ 就是 [`render_snapshot_for_test`]——**M2 批 N 任務 6**把這個手法開了
    /// 一個 `pub(crate)` 口子給 `admin.rs` 的 R12 判據用，這裡改成調它，
    /// 別讓同一段「繪製一張真表」的程式碼在本檔案裡存在兩份。
    fn 按活体源渲真表(src: &LiveSources) -> String {
        render_snapshot_for_test(src)
    }

    /// 這個族出了幾條樣本行（`# HELP` / `# TYPE` 不算）。
    fn 样本行(out: &str, family: &str) -> Vec<String> {
        out.lines()
            .filter(|l| l.starts_with(family) && !l.starts_with('#'))
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn 没登记活体源时那几个族有_help_type_但一条样本都没有() {
        // ⚠ ⚠ 這一條守的是「**沒接上**」與「**沒資料**」在抓取端看得出區別：
        //   整族消失的話兩者長得一模一樣，而前者要改程式碼、後者什麼都不用做。
        let out = 按活体源渲真表(&LiveSources::default());

        for name in [
            "fulcrum_upstream_inflight",
            "fulcrum_upstream_healthy",
            "fulcrum_cert_expiry_seconds",
            "fulcrum_acme_issue_total",
            "fulcrum_overrides_active",
        ] {
            assert!(out.contains(&format!("# HELP {name} ")), "缺 HELP：{name}");
            assert!(out.contains(&format!("# TYPE {name} ")), "缺 TYPE：{name}");
            assert!(
                样本行(&out, name).is_empty(),
                "{name} 在没有活体源时出了样本：{:?}",
                样本行(&out, name)
            );
        }
        // ★ ★ 反向那半：`build_info` **不需要活體源**，所以它在同一次繪製裡必須有樣本 ——
        //   少了這一條，把整個活體繪製刪掉，上面那個循環照樣全綠。
        assert_eq!(样本行(&out, "fulcrum_build_info").len(), 1, "{out}");
    }

    #[test]
    fn build_info的版本区分得出两次构建() {
        // `PLAN.md` §10 的 G149（原 `D9`，2026-09-07 結案）逐字點名過這個
        // 缺陷：當時六個 crate 全是 `0.0.0`
        // ⇒ 這一族**區分不出任何兩次建置**。G141 的第一部分就是修它。
        // ★ 2026-09-07 按 `G149` 把六個產品 crate 動到了 `0.1.0`，⚠ 而那**不修**本缺陷：
        //   裸的 `0.1.0` 同樣區分不出兩次建置 ⇒ 下面這條判據照舊承重。
        //   （`spikes/` 那三個仍是 `0.0.0` —— 它們不是發佈製品。）
        //
        // ⛔ ★ 判據不是「非空」——裸的 `0.0.0` 也非空。判據是**它不等於裸包版本**：
        //   三檔取值裡，② 會是 `<包版本>+g<提交号>`，③ 會是 `<包版本>+unknown`，
        //   ① 由建置環境喂真值。三檔都過得了這一條，而**退回裸 `0.0.0` 過不了**。
        assert_ne!(
            BUILD_VERSION,
            env!("CARGO_PKG_VERSION"),
            "build_info 的版本退回了裸包版本 ⇒ 又区分不出两次构建了"
        );

        // ⚠ 「取不到」必須是**說出來的** `unknown`，⛔ 不許是一個看起來像真版本號的東西。
        //   這一條守的是 build.rs 第 ③ 檔的形狀：它可以取不到，但不許假裝取到了。
        let 尾巴 = BUILD_VERSION
            .split_once('+')
            .map(|(_, meta)| meta.to_string());
        if let Some(meta) = 尾巴 {
            assert!(!meta.is_empty(), "构建元数据是空的：{BUILD_VERSION}");
        }
    }

    #[test]
    fn build_info_恒为_1_且只有一条() {
        // ⛔ 不帶 gen_id、不帶 pid ⇒ 無論問誰、問幾次，都只有這一條。
        for src in [
            LiveSources::default(),
            LiveSources {
                acme: Some(std::sync::Arc::new(fulcrum_acme::AcmeManager::new(
                    fulcrum_acme::AcmeConfig::new(None, None, "/nonexistent"),
                    std::sync::Arc::new(SniResolver::new()),
                    std::sync::Arc::new(fulcrum_acme::Http01Store::new()),
                    Vec::new(),
                ))),
                ..Default::default()
            },
        ] {
            let out = 按活体源渲真表(&src);
            assert_eq!(
                样本行(&out, "fulcrum_build_info"),
                vec![format!(
                    "fulcrum_build_info{{version=\"{BUILD_VERSION}\"}} 1"
                )],
                "{out}"
            );
        }
    }

    /// 兩個站台引用同一個位址的那份執行環境圖。
    ///
    /// ★ `127.0.0.1:9001` 被 `a.example` 與 `b.example` 各寫一條（共享後端，很常見的設定），
    /// `127.0.0.1:9002` 只有一處 —— 於是同一次繪製裡「歸併過的」與「沒歸併過的」都在。
    fn 共享后端的运行时图() -> std::sync::Arc<fulcrum_runtime::Runtime> {
        let outcome = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://a.example {\n  reverse_proxy 127.0.0.1:9001 127.0.0.1:9002\n}\n\
             http://b.example {\n  reverse_proxy 127.0.0.1:9001\n}\n",
        );
        let cfg = outcome.config.expect("配置编译不过");
        std::sync::Arc::new(fulcrum_runtime::Runtime::build(&cfg).expect("运行时图建不起来"))
    }

    /// 執行環境圖裡位址等於 `addr` 的那些 `Upstream`，**按遍歷序**。
    ///
    /// ★ ★ 每條判據都先拿它證一次「樣本里真有兩個物件」：前提不成立的話，
    /// 「求和」「合取」都是在量一張壓根沒有重複的表 —— 那種判據永遠綠，什麼都沒守。
    fn 同址上游<'a>(
        rt: &'a fulcrum_runtime::Runtime,
        addr: &str,
    ) -> Vec<&'a fulcrum_runtime::Upstream> {
        rt.all_proxy_targets()
            .into_iter()
            .flat_map(|t| t.upstreams.iter())
            .filter(|u| u.addr == addr)
            .collect()
    }

    fn 按运行时图渲真表(rt: std::sync::Arc<fulcrum_runtime::Runtime>) -> String {
        按活体源渲真表(&LiveSources {
            runtime: Some(SharedRuntime::new(rt)),
            ..Default::default()
        })
    }

    #[test]
    fn 同一个上游被两个站点引用时只出一条_series() {
        // ⚠ ⚠ series 的鍵是**上游位址串**，也就是 `least_conn` 與健康檢查共用的那個身分。
        let rt = 共享后端的运行时图();
        assert_eq!(
            同址上游(&rt, "127.0.0.1:9001").len(),
            2,
            "前提没成立：夹具里没有被重复引用的上游"
        );
        let out = 按运行时图渲真表(rt);

        // 兩個族各出**兩條** —— 三個 `Upstream` 物件歸併成兩個位址。
        assert_eq!(样本行(&out, "fulcrum_upstream_inflight").len(), 2, "{out}");
        assert_eq!(样本行(&out, "fulcrum_upstream_healthy").len(), 2, "{out}");
        assert_eq!(
            样本行(&out, "fulcrum_upstream_inflight"),
            vec![
                "fulcrum_upstream_inflight{upstream=\"127.0.0.1:9001\"} 0".to_string(),
                "fulcrum_upstream_inflight{upstream=\"127.0.0.1:9002\"} 0".to_string(),
            ],
            "{out}"
        );
        // 健康位：沒配 `health_uri` 的上游恆為 1（執行環境那一側的初值）。
        assert_eq!(
            样本行(&out, "fulcrum_upstream_healthy"),
            vec![
                "fulcrum_upstream_healthy{upstream=\"127.0.0.1:9001\"} 1".to_string(),
                "fulcrum_upstream_healthy{upstream=\"127.0.0.1:9002\"} 1".to_string(),
            ],
            "{out}"
        );
    }

    #[test]
    fn 同一个地址的在途数是全部_upstream_之和() {
        // ★ ★ `inflight` 是個**計數**，聚合只有一種說得通的做法：求和。
        //   報其中一份等於報一個**錯的數，而它長得和對的數一模一樣** ——
        //   讀這條 series 的人只會有一個理解：打到這個位址的連接一共有多少。
        let rt = 共享后端的运行时图();
        let 两份 = 同址上游(&rt, "127.0.0.1:9001");
        assert_eq!(两份.len(), 2, "前提没成立：夹具里没有被重复引用的上游");

        // ⚠ ⚠ 兩份**都非 0 且互不相等**（2 與 3 ⇒ 期望 5）：
        //   取 0 與 N 的話，「只取第一份」或「只取最後一份」在某個遍歷序下會**碰巧**通過 ——
        //   而碰巧通過的判據與不存在的判據無法區分。
        两份[0].acquire();
        两份[0].acquire();
        两份[1].acquire();
        两份[1].acquire();
        两份[1].acquire();
        assert_eq!((两份[0].inflight(), 两份[1].inflight()), (2, 3));

        let out = 按运行时图渲真表(rt);
        assert_eq!(
            样本行(&out, "fulcrum_upstream_inflight"),
            vec![
                "fulcrum_upstream_inflight{upstream=\"127.0.0.1:9001\"} 5".to_string(),
                "fulcrum_upstream_inflight{upstream=\"127.0.0.1:9002\"} 0".to_string(),
            ],
            "{out}"
        );
    }

    #[test]
    fn 同一个地址的健康位取合取_有一份不健康就是_0() {
        // ★ ★ `healthy` 是個**布爾**，聚合是**取合取**，⛔ 不是求和（求和會得到 2，
        //   那根本不在這個族的值域裡）。
        // ★ 取合取而不是析取，因為**混配**：一個站台配了 `health_uri`、另一個沒配
        //   ⇒ 後者恆為 1（「沒配就永不探測」）。合取給出的是真的探過的那一側探到的狀態；
        //   析取會讓一個**根本沒在探測**的物件把一次真實的故障蓋掉。
        //   ⇒ 悲觀那一側是安全的那一側。
        let rt = 共享后端的运行时图();
        let 两份 = 同址上游(&rt, "127.0.0.1:9001");
        assert_eq!(两份.len(), 2, "前提没成立：夹具里没有被重复引用的上游");

        // ── 方向一：兩份都健康 ⇒ 1（也就是上面那條判據的初態，這裡顯式再走一遍）
        assert!(
            两份[0].is_healthy() && 两份[1].is_healthy(),
            "初值应当都是健康"
        );
        assert!(
            按运行时图渲真表(rt.clone())
                .contains("fulcrum_upstream_healthy{upstream=\"127.0.0.1:9001\"} 1\n")
        );

        // ── 方向二：只有**第二份**被探測判成不健康 ⇒ 0
        //   ⚠ 挑第二份而不是第一份：挑第一份的話，一個「只看第一份」的實作照樣能過。
        两份[1].set_healthy(false);
        assert!(两份[0].is_healthy(), "第一份不该被连带改掉");
        let out = 按运行时图渲真表(rt);
        assert_eq!(
            样本行(&out, "fulcrum_upstream_healthy"),
            vec![
                "fulcrum_upstream_healthy{upstream=\"127.0.0.1:9001\"} 0".to_string(),
                // ★ 沒被碰過的那個位址不受影響 —— 否則「全判成 0」也能過上面那條。
                "fulcrum_upstream_healthy{upstream=\"127.0.0.1:9002\"} 1".to_string(),
            ],
            "{out}"
        );
    }

    #[test]
    fn 证书到期时刻取绝对_unix_秒_而挑战证书不进这个族() {
        let ck = 自签("cert.example");
        let 到期 = ck.not_after;
        let resolver = std::sync::Arc::new(SniResolver::new());
        resolver.install(&["cert.example".to_string()], ck);
        // ⛔ 挑戰證書是一張只活幾秒的一次性自簽證書 ——
        //   混進這個族會讓「快過期了」那類告警一直在叫，而叫的是一個不該被續期的東西。
        let _guard = resolver.provision_challenge("challenge.example", 自签("challenge.example"));
        assert_eq!(resolver.challenge_len(), 1, "前提没成立：挑战证书没挂上");

        let out = 按活体源渲真表(&LiveSources {
            resolver: Some(resolver),
            ..Default::default()
        });
        // ★ 值是 `notAfter` 的**絕對 Unix 秒**（裁決 R5），不是「還剩多少秒」。
        assert_eq!(
            样本行(&out, "fulcrum_cert_expiry_seconds"),
            vec![format!(
                "fulcrum_cert_expiry_seconds{{domain=\"cert.example\"}} {}",
                到期
                    .duration_since(UNIX_EPOCH)
                    .expect("自签证书的 notAfter 在 1970 之后")
                    .as_secs_f64()
            )],
            "{out}"
        );
        // ★ 反向那半：絕對值必須**遠大於**一個「剩餘量」會有的數量級。
        //   少了這一條，把 `unix_secs` 換成 `notAfter - now` 之後上面那條只需跟著改一次期望。
        assert!(
            到期.duration_since(UNIX_EPOCH).expect("同上").as_secs() > 1_700_000_000,
            "取到的不像一个绝对 Unix 秒"
        );
    }

    #[test]
    fn acme_那三格无条件都出_哪怕一次都没签过() {
        // ★ 一條**從來沒出現過**的 series 與一條**恆為 0** 的 series，在告警規則裡
        //   是兩種完全不同的東西：前者讓 `rate()` 直接沒有資料可算。
        let m = std::sync::Arc::new(fulcrum_acme::AcmeManager::new(
            fulcrum_acme::AcmeConfig::new(None, None, "/nonexistent"),
            std::sync::Arc::new(SniResolver::new()),
            std::sync::Arc::new(fulcrum_acme::Http01Store::new()),
            Vec::new(),
        ));
        let out = 按活体源渲真表(&LiveSources {
            acme: Some(m),
            ..Default::default()
        });
        assert_eq!(
            样本行(&out, "fulcrum_acme_issue_total"),
            vec![
                "fulcrum_acme_issue_total{result=\"deferred\"} 0".to_string(),
                "fulcrum_acme_issue_total{result=\"fail\"} 0".to_string(),
                "fulcrum_acme_issue_total{result=\"ok\"} 0".to_string(),
            ],
            "{out}"
        );
    }

    #[test]
    fn fulcrum_overrides_active的值是覆盖总数_悬空的照样计入_基数恒为1() {
        // ★ ★ 重複使用 `admin.rs`「總數與懸空數不相等」那條夾具的手法
        //   （`OverrideLayer::slot` 直接現建一格，不必走 `apply_all` / 重新
        //   load 一遍那整條路）——這個族只要 [`SharedRuntime::override_counts`]
        //   的第一個數，不必驗覆蓋對資料面的效果（那是任務 5 的判據）。
        let cfg = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://ov.example {\n  reverse_proxy 10.99.0.1:1\n}\n",
        )
        .config
        .expect("配置编译不过");
        let built = fulcrum_runtime::Runtime::build(&cfg).expect("运行时图建不起来");
        let rt = std::sync::Arc::new(built);
        let shared = SharedRuntime::new(rt);

        // ── 判據 4：0 覆蓋 ⇒ 族仍然出現，值是 0（不是整族消失）───────────────
        let out0 = 按活体源渲真表(&LiveSources {
            runtime: Some(shared.clone()),
            ..Default::default()
        });
        assert_eq!(
            样本行(&out0, "fulcrum_overrides_active"),
            vec!["fulcrum_overrides_active 0".to_string()],
            "{out0}"
        );

        // ── 判據 3：兩項覆蓋、其中一項懸空 ⇒ 總數是 2，不是 1 ───────────────
        //   ⚠ ⚠ 判據寫法紀律：總數與懸空數必須不相等，否則把兩個數讀串了
        //   （比如誤取 `override_counts().1`，或誤取「非懸空數」）也測不出來
        //   —— 批 N 任務 5 的 Critical 就是這個形狀：N 與 M 都恰好是 1。
        let k_alive =
            fulcrum_runtime::overrides::OverrideKey::new("http://ov.example", "", "10.99.0.1:1");
        let k_dangling = fulcrum_runtime::overrides::OverrideKey::new(
            "http://ov.example",
            "ghost",
            "10.99.0.2:2",
        );
        shared.overrides().slot(&k_alive).set_disabled(true);
        shared.overrides().slot(&k_dangling).set_disabled(true);
        // ⚠ ⚠ 這是一條**前提斷言**，排在真判據前面 ⇒ 它對哪一層瞎、要說清（評審 M2）：
        //   它讀的是**取數函式**，不是渲出來的文字。所以往 `snapshot()` 裡注入
        //   （比如改寫成「非懸空數」）時它照樣通過，缺陷由下面那條判據接住 —— 這正是
        //   本條存在的形態。⛔ 但往 `override_counts()` / `OverrideLayer::entries()`
        //   裡注入時，**本行先炸，下面那條判據一次都不會執行** ——
        //   在那一層做反證的人會誤以為是判據把它逮住的。
        assert_eq!(
            shared.override_counts(),
            (2, 1),
            "前提没成立：夹具没能摆出「总数 ≠ 悬空数」"
        );

        let out2 = 按活体源渲真表(&LiveSources {
            runtime: Some(shared.clone()),
            ..Default::default()
        });
        assert_eq!(
            样本行(&out2, "fulcrum_overrides_active"),
            vec!["fulcrum_overrides_active 2".to_string()],
            "★ 悬空的照样计入（裁决 R13）：应该是总数 2，不是悬空数 1，\
             也不是非悬空数 1。{out2}"
        );

        // ── 判據 5：基數恆為 1 —— 即便登記處裡有 2 項覆蓋，也只出一行 ─────────
        assert_eq!(样本行(&out2, "fulcrum_overrides_active").len(), 1, "{out2}");
    }

    #[test]
    fn 非有限浮点按_exposition_写_而有限值原样() {
        // ⚠ ⚠ Rust 的 `Display` 把 `f64::INFINITY` 印成 `inf`，而 exposition 要 `+Inf`。
        //   ★ 批 M 任務 1 那一版走不到這條路（真表裡一個 gauge 族都沒有）；
        //     任務 5 引入 gauge 之後它就走得到了。
        const G: [Family; 1] = [Family {
            name: "t_inf",
            kind: Kind::Gauge,
            source: Source::Event,
            help: "无穷。",
            labels: &["v"],
            domains: &[Domain::Config],
        }];
        let mut r = Registry::default();
        r.set(&G[0], &["pos"], f64::INFINITY);
        r.set(&G[0], &["neg"], f64::NEG_INFINITY);
        r.set(&G[0], &["nan"], f64::NAN);
        // ★ 反向那半：有限值**不許**被改寫 —— 少了它，一個「所有 gauge 都印成 +Inf」
        //   的實作照樣能通過上面三條。
        r.set(&G[0], &["finite"], 1.5);
        r.set(&G[0], &["zero"], 0.0);
        r.set(&G[0], &["neg_finite"], -2.25);

        let mut out = String::new();
        render_events(&r, &G, &mut out);
        let 期望 = [
            "# HELP t_inf 无穷。",
            "# TYPE t_inf gauge",
            "t_inf{v=\"finite\"} 1.5",
            "t_inf{v=\"nan\"} NaN",
            "t_inf{v=\"neg\"} -Inf",
            "t_inf{v=\"neg_finite\"} -2.25",
            "t_inf{v=\"pos\"} +Inf",
            "t_inf{v=\"zero\"} 0",
            "",
        ]
        .join("\n");
        assert_eq!(out, 期望);
    }

    /// ★ ★ ★ **`observability.md` 那張基數表要與 [`FAMILIES`] 逐項對得上。**
    ///
    /// # 為什麼要有這一條
    ///
    /// 那張表此前寫著 `8 × 5 × 3`，而 `status_class` 完成實作成**六個**值的那一刻它就成了假話
    /// —— 從那一刻到有人手動改掉它為止，**沒有任何東西會紅**。
    /// ⇒ 一條靠「下一個人記得同時改文件」維持的正確性，本儲存庫已經實測失效過一次。
    /// ★ 文件是這個族清單的**權威**（本模組頂部就這麼寫的），而權威與實作之間
    /// 此前一道門都沒有。
    ///
    /// # ⚠ ⚠ 兩個方向都要，少一個這道門就形同虛設
    ///
    /// 只斷言「每個族都在表裡」⇒ **刪掉一個族而忘了刪表裡那行**，照樣全綠；
    /// 只斷言「表裡每行都是真族」⇒ **新增一個族而忘了寫進表**，照樣全綠。
    ///
    /// # 釘哪幾格（**M2 批 M′ 任務 1**，owner 2026-09-02 拍板擴門）
    ///
    /// 那張表有五列：**族名 · 類型 · 數從哪來 · 標籤 · 上界**。
    ///
    /// | 列 | 怎麼釘 |
    /// |---|---|
    /// | 族名 | 兩個方向的集合相等（下面的 ① ②）|
    /// | 類型 | **逐字**等於 [`Kind::as_str`] |
    /// | 數從哪來 | **逐字**等於 [`Source`] 的文件寫法（`事件点` / `活体`）|
    /// | 標籤 | **按序逐項**等於 `Family::labels`（`无` = 空）—— ⚠ 順序是契約，不是集合 |
    /// | 上界 | ⛔ **不釘字面值**，只釘一條**單向蘊含**：無標籤 ⇒ 那一格必須說「恆為 1」|
    ///
    /// ⛔ **上界那一格有意不釘算式**：`(address 数 + 1) × 8 × 6 × 3` 是寫給人看的推導，
    /// 釘住它只會讓這道門在每次增刪一個標籤值時變成噪音。★ 而**真正會漂的是前四列** ——
    /// 它們每一格都能逐字對到 `Family` 上，對不上就是文件在說假話。
    ///
    /// ⚠ **單向蘊含只做正向**：`labels` 為空 ⇒ 只可能有一條 series ⇒ 上界必須是 1。
    /// 反過來**不成立** —— [`BUILD_INFO`] 帶著 `version` 而上界就是 1。
    /// ★ 寫成雙向的話，這道門會在一條**完全正確**的行上紅。
    ///
    /// # 錨點是**錶行的形狀**，不是小節標題
    ///
    /// 取「首格是一個反引號包起來、`fulcrum_` 開頭的名字」的錶行。
    /// ⇒ 標題怎麼改都不影響它；而任何一行**自稱是個族**的錶行都會被問一遍，
    /// 無論它出現在文件的哪一節。
    /// ⚠ 順帶要求那種行**正好五格** —— 少一格多一格都說明表的形狀變了，
    /// 而那時該先懷疑解析，不是先懷疑內容。
    #[test]
    fn 文档里那张基数表与声明表逐项对得上() {
        /// 一行 Markdown 錶行拆成各格（已 trim）。⚠ 首尾那兩根豎線之外沒有內容。
        fn 拆格(行: &str) -> Option<Vec<&str>> {
            let 行 = 行.trim();
            let 内 = 行.strip_prefix('|')?.strip_suffix('|')?;
            Some(内.split('|').map(str::trim).collect())
        }

        /// 一格里那個反引號包起來的名字。
        fn 反引号名(格: &str) -> Option<&str> {
            格.strip_prefix('`')?.strip_suffix('`')
        }

        /// 標籤那一格 → 按序的標籤名。`无` = 空。
        /// ⚠ 回傳 `None` 表示**這一格的寫法不認識** —— 那要紅，不能當成空。
        fn 解析标签格(格: &str) -> Option<Vec<&str>> {
            if 格 == "无" || 格 == "無" {
                return Some(Vec::new());
            }
            // ⚠ **空格子不當成「無標籤」**：那會讓一個被清空的格子悄悄對上
            //   任何一個零標籤的族。要說「沒有標籤」就得寫出 `无` 來。
            if 格.is_empty() {
                return None;
            }
            格.split_whitespace().map(反引号名).collect()
        }

        /// [`Source`] 在那張表裡的寫法。⚠ `match` 是窮盡的 ⇒ 將來加第三種來處時
        /// **這裡編不過**，而不是靜默地少守一格。
        fn 来处写法(s: Source) -> &'static str {
            match s {
                Source::Event => "事件点",
                Source::Live => "活体",
            }
        }

        /// 一行族的後四格。★ 抽成函式是為了**下面能拿假資料餵它一次** ——
        /// 只對真表斷言的話，把比較改成恆 `true` 一樣全綠。
        fn 对不上的地方(f: &Family, 格: &[&str]) -> Vec<String> {
            let mut 差 = Vec::new();
            if 格[1] != f.kind.as_str() {
                差.push(format!(
                    "类型：文档写 `{}`，代码是 `{}`",
                    格[1],
                    f.kind.as_str()
                ));
            }
            let 期望来处 = 来处写法(f.source);
            if 格[2].replace("事件點", "事件点").replace("活體", "活体") != 期望来处 {
                差.push(format!("数从哪来：文档写 `{}`，代码是 `{期望来处}`", 格[2]));
            }
            match 解析标签格(格[3]) {
                None => 差.push(format!(
                    "标签那一格的写法不认识：`{}`（要么是反引号包的名字，要么是 `无`）",
                    格[3]
                )),
                Some(标签) if 标签 != f.labels => 差.push(format!(
                    "标签：文档写 {:?}，代码是 {:?}（⚠ 顺序也是契约）",
                    标签, f.labels
                )),
                Some(_) => {}
            }
            // ★ 上界那一格的第一條：**無標籤 ⇒ 恆為 1**（單向蘊含，反向不成立）。
            if f.labels.is_empty()
                && !格[4].starts_with("恒为 1")
                && !格[4].starts_with("恆為 1")
            {
                差.push(format!(
                    "上界：这个族无标签 ⇒ 只可能有一条 series，那一格却写着 `{}`",
                    格[4]
                ));
            }
            // ★ ★ ★ 上界那一格的第二條（**M2 批 P**）：每個 `Closed` 標籤的**取值個數**
            //   必須在那一格里說得出來 —— 要麼出現 `len()` 這個整數，要麼逐字列出全部取值。
            for (标签, 域) in f.labels.iter().zip(f.domains) {
                let Domain::Closed(取值) = 域 else {
                    continue;
                };
                let 个数 = 取值.len();
                let 有数字 = 独立整数(格[4]).any(|n| n == 个数);
                let 有全部取值 = 取值.iter().all(|v| 格[4].contains(v));
                if !有数字 && !有全部取值 {
                    差.push(format!(
                        "上界：标签 `{标签}` 的闭集是 {个数} 个值 {取值:?}，\
                         而那一格既没出现 `{个数}`、也没逐字列出它们：`{}`\n\
                         ⇒ **先改文档，不是改这道门** —— 闭集在代码里，那一格是抄它的结果。",
                        格[4]
                    ));
                }
            }
            差
        }

        /// 一段文字里所有**獨立**的十進位整數。
        ///
        /// ⚠ 「獨立」是承重的：`× 16` 裡的 `16` ⛔ 不許被讀成 `1` 或 `6` ——
        /// 否則一個閉集從 6 個變成 16 個時，那一格照舊「命中」而它已經是假話。
        fn 独立整数(s: &str) -> impl Iterator<Item = usize> + '_ {
            s.split(|c: char| !c.is_ascii_digit())
                .filter(|t| !t.is_empty())
                .filter_map(|t| t.parse::<usize>().ok())
        }

        const DOC: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/architecture/observability.md"
        ));

        // 族名 → 那一行的五格。⚠ 只認「首格是反引號包的 `fulcrum_` 名字」的行。
        let mut 行表: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for 行 in DOC.lines() {
            let Some(格) = 拆格(行) else { continue };
            let Some(名) = 格.first().copied().and_then(反引号名) else {
                continue;
            };
            if !名.starts_with("fulcrum_") {
                continue;
            }
            // ⚠ 形狀先自證：正好五格。少一格多一格都說明那張表的寫法變了 ——
            //   ★ 那時該先懷疑解析，而不是先懷疑內容。
            assert_eq!(
                格.len(),
                5,
                "observability.md 里 `{名}` 那一行不是五格（拿到 {} 格）：{行}\n\
                 ⇒ **先怀疑表的形状变了**（列增删了？），再怀疑内容。",
                格.len()
            );
            assert!(
                行表.insert(名, 格).is_none(),
                "observability.md 里 `{名}` 出现了不止一行 —— \
                 ⚠ 两行里只要有一行是对的，这道门就永远指不出另一行错在哪。"
            );
        }
        let 表里的: std::collections::BTreeSet<&str> = 行表.keys().copied().collect();

        // ── ★ ★ 判據自身先自證：樣本里真的有東西 ──────────────────────────
        //
        // ★ 本儲存庫那條老紀律：**取樣前先確認樣本里真有東西** —— 一個把文件讀成空串、
        //   或者一行都沒命中的解析器，會讓**方向 ②** 恆綠（空集裡當然找不出多餘的行）。
        // ⚠ 判據失效時它不會沉默，它會給一個像樣的錯答案：「文件裡沒有多餘的行」。
        assert!(!DOC.is_empty(), "observability.md 读进来是空的");
        assert!(
            !表里的.is_empty(),
            "从 observability.md 里一行族名都没解析出来。\n\
             ⇒ **先怀疑解析没命中**（那张表的写法变了？），而不是文档真的一行都没有。"
        );

        // ⚠ ⚠ **這裡有意不寫「解析出的族名 ≥ FAMILIES.len()」那條計數自證**：
        //   它與下面的**方向 ①** 是同一句話，而寫在前面會把 ① 的回報錯誤**蓋掉** ——
        //   刪掉表裡一行時，紅的會是「只解析出 8 個」而不是「少了哪一個」。
        //   ★ 一條把更精確的判據擋在後面的自證，是負收益。
        //   ⇒ 計數由方向 ① 順帶守住：① 過了就意味著 表裡的 ⊇ 宣告的。
        // ★ ★ 第 5 列那條判據的「樣本里真有東西」自證（**M2 批 P**）：
        //   真表裡必須**真的有** `Closed` 域，否則那條判據在真表上是**空轉** ——
        //   ⚠ 全是 `Config` / `Compiled` 時它一個字都不查，而這道門依然全綠。
        //   ⛔ 不寫死個數（那又是一個沒有門的計數），只要求「不是零」。
        assert!(
            FAMILIES
                .iter()
                .flat_map(|f| f.domains)
                .any(|d| matches!(d, Domain::Closed(_))),
            "一个 `Domain::Closed` 都没有 —— 上界那一列的判据在真表上什么都没查"
        );

        let 声明的: std::collections::BTreeSet<&str> = FAMILIES.iter().map(|f| f.name).collect();

        // ── 方向 ①：宣告表裡的每一個族，文件那張表裡都要有一行 ────────────
        let 文档缺的: Vec<&str> = 声明的.difference(&表里的).copied().collect();
        assert!(
            文档缺的.is_empty(),
            "这些族在 `FAMILIES` 里有、而 docs/architecture/observability.md \
             的基数表里没有：{文档缺的:?}\n\
             ⇒ 新增一个族就要同时给它写上界 —— **说不出上界的标签正是那张表存在的理由**。"
        );

        // ── 方向 ②：文件那張表裡的每一行，都要真的是一個族 ────────────────
        let 文档多的: Vec<&str> = 表里的.difference(&声明的).copied().collect();
        assert!(
            文档多的.is_empty(),
            "docs/architecture/observability.md 的基数表里有这些行，而 `FAMILIES` \
             里没有对应的族：{文档多的:?}\n\
             ⇒ 要么族名拼错了，要么删族时忘了删那一行。\
             ⚠ 一行描述着不存在的指标的文档，比没有那一行更贵。"
        );

        // ── 方向 ③ ④：每一行的**後四格**也要對得上 ────────────────────────
        //
        // ★ 到這裡 ① ② 已經保證兩邊族名集合相等 ⇒ 每個族都取得到它那一行。
        let mut 全部问题: Vec<String> = Vec::new();
        for f in &FAMILIES {
            let 格 = &行表[f.name];
            for 差 in 对不上的地方(f, 格) {
                全部问题.push(format!("`{}` 的{}", f.name, 差));
            }
        }
        assert!(
            全部问题.is_empty(),
            "docs/architecture/observability.md 的基数表与 `FAMILIES` 对不上：\n  {}\n\
             ⇒ 那张表是这个族清单的**权威**（本模块顶部就这么写的）—— \
             权威说的话与实现不一致时，**先改对的那一边，不是改这道门**。",
            全部问题.join("\n  ")
        );

        // ── ★ ★ ★ 第 5 列判據的**端到端**反向（**M2 批 P**）─────────────────
        //
        // 拿**真表裡那一行**（`fulcrum_requests_total`）配一個「閉集多了一個值」的族 ⇒
        // 必須紅在上界那一格。★ 它答的是那條鏈子的最後一環：
        // 「`status_class` 真的漲到 7 個時，文件裡那個 `6` 會不會被逮到」。
        // ⛔ 不需要改任何真檔案 —— `对不上的地方` 兩個入參都能現造，
        //   而共享工作樹上「擾動真檔案再復原」是不安全的。
        {
            static 多一个: &[&str] = &["1xx", "2xx", "3xx", "4xx", "5xx", "none", "7xx"];
            static 假域: [Domain; 4] = [
                Domain::Config,
                Domain::Config,
                Domain::Closed(多一个),
                Domain::Config,
            ];
            let 假族 = Family {
                name: "fulcrum_requests_total",
                kind: Kind::Counter,
                source: Source::Event,
                help: "",
                labels: &["site", "outcome", "status_class", "proto"],
                domains: &假域,
            };
            let 差 = 对不上的地方(&假族, &行表["fulcrum_requests_total"]);
            assert!(
                差.iter().any(|d| d.starts_with("上界")),
                "把 status_class 的闭集撑到 7 个之后，真表那一行竟然还是绿的 —— \
                 那说明第 5 列这条判据在真表上没接上。实际报的是：{差:?}"
            );
        }

        // ── ★ ★ 反向那半：**這幾條判得動壞行**。────────────────────────────
        //
        // ⚠ 只對真表斷言的話，把 `对不上的地方` 改成恆回傳空表一樣全綠 ——
        //   那時這道門已經不存在了，而沒有任何跡象。
        //   ★ 每一格都單獨壞一次，且**逐條確認紅的是哪一格** ——
        //     一次把四格全改壞的話，只要有一格判得動就全綠，另外三格從沒被驗過。
        let 样板 = Family {
            name: "fulcrum_probe",
            kind: Kind::Gauge,
            source: Source::Live,
            help: "",
            labels: &[],
            domains: &[],
        };
        let 好行 = ["`fulcrum_probe`", "gauge", "活体", "无", "恒为 1"];
        assert!(
            对不上的地方(&样板, &好行).is_empty(),
            "正样本自己就不过 —— 后面那几条反向断言因此什么都证明不了"
        );
        for (坏, 该说的) in [
            (
                ["`fulcrum_probe`", "counter", "活体", "无", "恒为 1"],
                "类型",
            ),
            (
                ["`fulcrum_probe`", "gauge", "事件点", "无", "恒为 1"],
                "数从哪来",
            ),
            (
                ["`fulcrum_probe`", "gauge", "活体", "`site`", "恒为 1"],
                "标签",
            ),
            (
                ["`fulcrum_probe`", "gauge", "活体", "四个", "恒为 1"],
                "标签那一格的写法",
            ),
            // ★ 空格子**不許**被當成「無標籤」—— 否則一個被清空的格子會悄悄對上。
            (
                ["`fulcrum_probe`", "gauge", "活体", "", "恒为 1"],
                "标签那一格的写法",
            ),
            (["`fulcrum_probe`", "gauge", "活体", "无", "配置定"], "上界"),
        ] {
            let 差 = 对不上的地方(&样板, &坏);
            assert!(
                差.iter().any(|d| d.starts_with(该说的)),
                "把{该说的}那一格改坏之后，这道门没有点名它。实际报的是：{差:?}"
            );
        }
        // ★ 標籤**只換順序**也必須紅 —— 順序是契約（寫入時按序對齊），
        //   而「集合相等」會把這一類靜默放過去。
        let 两标签 = Family {
            name: "fulcrum_probe2",
            kind: Kind::Counter,
            source: Source::Event,
            help: "",
            labels: &["site", "outcome"],
            domains: &[Domain::Config, Domain::Config],
        };
        assert!(
            对不上的地方(
                &两标签,
                &[
                    "`fulcrum_probe2`",
                    "counter",
                    "事件点",
                    "`outcome` `site`",
                    "—"
                ]
            )
            .iter()
            .any(|d| d.starts_with("标签")),
            "标签换了顺序却没红 —— 那说明这道门比的是集合，而顺序才是契约"
        );

        // ── ★ ★ ★ 第 5 列那條新判據（**M2 批 P**）的反向 ────────────────────
        //
        // ⚠ 三個方向都要：① 數字對不上要紅；② 數字對得上不許紅；
        //   ③ 逐字列出全部取值這條**替代分支**也不許紅。
        //   ★ 少了 ②③，一個恆紅的實作照樣能讓 ① 通過。
        const 三个值: &[&str] = &["hit", "miss", "stale"];
        static 三值域: [Domain; 1] = [Domain::Closed(三个值)];
        let 闭集族 = Family {
            name: "fulcrum_probe3",
            kind: Kind::Counter,
            source: Source::Event,
            help: "",
            labels: &["event"],
            domains: &三值域,
        };
        let 行 = |上界: &'static str| ["`fulcrum_probe3`", "counter", "事件点", "`event`", 上界];

        assert!(
            对不上的地方(&闭集族, &行("× 3"))
                .iter()
                .all(|d| !d.starts_with("上界")),
            "闭集是 3 个而那一格写着 3，却被判红了 —— 这道门会在正确的行上报错"
        );
        assert!(
            对不上的地方(&闭集族, &行("`hit`/`miss`/`stale`"))
                .iter()
                .all(|d| !d.starts_with("上界")),
            "那一格逐字列出了全部取值，却被判红了 —— **替代分支**没接上"
        );
        assert!(
            对不上的地方(&闭集族, &行("× 5"))
                .iter()
                .any(|d| d.starts_with("上界")),
            "闭集是 3 个而那一格写着 5，竟然没红 —— 这道门是空的"
        );
        // ⚠ ⚠ 這一條守的是 `独立整数`：`× 33` 裡那個 `33` ⛔ 不許被讀成兩個 `3`。
        assert!(
            对不上的地方(&闭集族, &行("× 33"))
                .iter()
                .any(|d| d.starts_with("上界")),
            "`33` 竟然算命中了 `3` —— 那么闭集从 3 涨到 33 时这道门会安静地放过去"
        );
    }

    /// ⚠ ⚠ **`domains` 與 `labels` 必須一一對應、同序。**
    ///
    /// ⛔ 長度不等的話，後面那些標籤會**靜默地錯位對到別人的取值域上** ——
    /// 而錯位之後那道基數判據仍然會給出一個像樣的答案，只是答的是另一個問題。
    #[test]
    fn 每个族的_domains_与_labels_一一对应() {
        for f in &FAMILIES {
            assert_eq!(
                f.domains.len(),
                f.labels.len(),
                "`{}` 的 domains 有 {} 个而 labels 有 {} 个",
                f.name,
                f.domains.len(),
                f.labels.len()
            );
        }
    }

    /// ★ ★ ★ **`Closed` 宣告的那些閉集，必須與產生它們的程式碼對得上。**
    ///
    /// ⚠ 少了這一條，「把閉集掛到 `Family` 上」就只是**把手抄從文件搬進了程式碼** ——
    /// 而基數表那道門會一本正經地拿一份抄件去檢核另一份抄件。
    #[test]
    fn 闭集与产生它的代码对得上() {
        use std::collections::BTreeSet;

        // ① `status_class`：**窮舉整個 `u16`**（只有 65536 個，很便宜）。
        //    ★ 兩個方向：值域 ⊆ 常數，且常數裡每個值都取得到。
        let 实际: BTreeSet<&str> = (0..=u16::MAX)
            .map(crate::access_log::status_class)
            .collect();
        let 声明: BTreeSet<&str> = crate::access_log::STATUS_CLASSES.iter().copied().collect();
        assert_eq!(
            实际, 声明,
            "STATUS_CLASSES 与 status_class() 的值域对不上 —— \
             ⚠ 这正是那张基数表里的 `6` 当初变成假话的那条路"
        );

        // ② `event`：`ALL` 逐個映射。`ALL` 的完整性由 `穷尽性自证` 那條 match 守著。
        let 实际: BTreeSet<&str> = crate::cache::CacheEvent::ALL
            .iter()
            .map(|e| e.穷尽性自证().label())
            .collect();
        let 声明: BTreeSet<&str> = crate::cache::CACHE_EVENTS.iter().copied().collect();
        assert_eq!(实际, 声明, "CACHE_EVENTS 与 CacheEvent::label 对不上");

        // ③ `entrypoint`：同上。
        let 实际: BTreeSet<&str> = crate::conn_stats::Entrypoint::ALL
            .iter()
            .map(|e| e.穷尽性自证().as_str())
            .collect();
        let 声明: BTreeSet<&str> = crate::conn_stats::ENTRYPOINTS.iter().copied().collect();
        assert_eq!(实际, 声明, "ENTRYPOINTS 与 Entrypoint::as_str 对不上");

        // ④ `result`：繪製那三行直接用這三個常數 ⇒ 這裡只需釘住**沒有重複、沒有空值**。
        //    ⚠ 它比 ①②③ 弱，寫在明處：`ACME_RESULTS` 與那三行是同一批名字，
        //    但「有沒有第四種結果被漏掉」沒有門看得見。
        let 声明: BTreeSet<&str> = ACME_RESULTS.iter().copied().collect();
        assert_eq!(声明.len(), ACME_RESULTS.len(), "ACME_RESULTS 里有重复");
        assert!(!声明.contains(""), "ACME_RESULTS 里有空值");

        // ⑤ `proto`：同 ④，三個常數就是兩個入口建構函式用的那三個。
        let 声明: BTreeSet<&str> = crate::access_log::PROTOS.iter().copied().collect();
        assert_eq!(
            声明.len(),
            crate::access_log::PROTOS.len(),
            "PROTOS 里有重复"
        );

        // ⑥ `outcome`：⛔ **這裡有意只剩「集合自洽」這一條。**
        //
        //    ★ ★ 從前這一格還逐個斷言「每個常數都在 `OUTCOMES` 裡」，而那張清單
        //    **本身也是手寫的** ⇒ 它答不了「新增一個取值而兩處都忘了改」。
        //    今天那件事由**類型與宏**接管，兩道，缺一不可：
        //      ① `outcomes!` 從**同一行**生成常數與 `OUTCOMES` ⇒ 宣告了卻沒進陣列，
        //         在語法上表達不出來（與 `chain_directives!` 的序號同一手法）；
        //      ② `OutcomeName` 欄位私有、建構點只在那個宏裡 ⇒ `=> "foo"` 這種
        //         繞開常數表的裸字面值**編不過**，`serve_one` 的每一條回傳路徑
        //         也必須求值出一個 `OutcomeName` 才能回傳。
        //    ⇒ 再寫一遍那張清單只是同義反復，而同義反復的判據會讓人以為這一格有門。
        //
        //    ⚠ 宏擋不住的**只剩一件**：兩行寫了同一個字面值。那正是下面這一條。
        let 声明: BTreeSet<&str> = crate::OUTCOMES.iter().copied().collect();
        assert_eq!(声明.len(), crate::OUTCOMES.len(), "OUTCOMES 里有重复");
        assert!(!声明.contains(""), "OUTCOMES 里有空值");
    }
}
