//! Prometheus 指标：进程级注册表 + text exposition 的**自研**渲染器（**M2 批 M**，G117）。
//!
//! 指标清单、每一格的基数上界与端点形态定稿在
//! [`docs/architecture/observability.md`](../../../docs/architecture/observability.md)
//! —— **那一份是权威**，本模块是它的实现。
//!
//! # ★ ★ 三件被有意分开的事
//!
//! | | 谁 |
//! |---|---|
//! | 有哪些族、每个族带哪些标签 | `FAMILIES`：**族的唯一声明处** |
//! | 一次事件把数记进哪一格 | [`Family::inc`] / [`Family::inc_by`] / [`Family::set`] / [`Family::observe`] |
//! | 抓取时那一坨文本长什么样 | [`render`] |
//!
//! ★ 分开的理由是它们各自会独立变化：加一个族不该动渲染器，改渲染细节不该碰声明表。
//!
//! # ⚠ ⚠ 标签的个数与顺序由声明定死，对不上**就地 panic**
//!
//! `{site,outcome}` 与 `{outcome,site}` 渲染出来是**两条不同的 series**，
//! 而这种错**在输出里看起来完全正常**：抓取端照收、图照画，只是画的是另一件事。
//! ⇒ 唯一逮得住它的地方是写入那一刻。
//! ★ 用 `assert!` 而不是 `debug_assert!`：后者在 release 下整条消失，
//! 于是「会喊出来」的那个构型正好是产品不跑的那一个。
//!
//! # ⚠ 渲染顺序必须是确定的
//!
//! 族按声明表的顺序出，同一个族里的 series 按标签值排序（两层都是 `BTreeMap`）。
//! ★ 换成 `HashMap` 不会让任何一条断言变红，代价是**两次抓取的 diff 里全是噪声** ——
//! 而人正是靠 diff 看指标出没出新东西的。
//!
//! # 格式自研、零新依赖（G117）
//!
//! text exposition 是行式纯文本，自己写一百行。★ 它**不是安全敏感协议栈**
//! （安全基线第 5 条管的是 TLS / HPACK / QUIC 那一类）⇒ 供应链门不用动，
//! musl 静态产物也不受影响。⚠ 代价：**直方图分桶、标签值转义、`_total` 后缀要自己钉住** ——
//! 钉子在本文件末尾的单测里。
//!
//! # ⚠ 基数：每一格的上界都由配置定，不由访问者定
//!
//! 标签值只许来自**闭集**（`outcome` / `status_class` / `proto` / 缓存事件）或**配置里的
//! 地址字面量**。⛔ 任何形态都不加 `uri` 标签。★ 这条纪律的执行者在取数点那一侧；
//! 本模块的责任是不给它开后门 —— 写入 API 收的是 `&[&str]`，个数与顺序都由声明表定死。
//!
//! # ★ ★ ★ 一个族的数只有两种来处，由声明表里的 [`Source`] 定死
//!
//! **① 事件点记账（[`Source::Event`]）** —— 发生一次就往进程级注册表里加一笔，
//! 渲染只是把数抄出来：
//!
//! | 族 | 取数点 |
//! |---|---|
//! | `fulcrum_requests_total` · `fulcrum_request_duration_seconds` | [`crate::access_log::Record::finish`]，**只有那一处** |
//! | `fulcrum_no_site_match_total` | `lib.rs` 里写 `outcome = "no_site_match"` 的同一处 |
//! | `fulcrum_cache_events_total` | `hit` / `stale` 在 `write_cached`，`miss` 在回源那一处，`purge` 在 `POST /purge` |
//!
//! **② 抓取时去问活体（[`Source::Live`]）** —— 注册表里**永远没有它们的数**，
//! [`render`] 那一刻现问 [`LiveSources`] 里那几个对象：
//!
//! | 族 | 问谁 |
//! |---|---|
//! | `fulcrum_upstream_inflight` · `fulcrum_upstream_healthy` | 当前 `Runtime` 快照里的每个 `Upstream`，**按地址归并**：在途数求和、健康位取合取 |
//! | `fulcrum_cert_expiry_seconds` | `SniResolver::expiries()`（R5：值是 `notAfter` 的**绝对 Unix 秒**）|
//! | `fulcrum_acme_issue_total` | `AcmeManager::issue_counts()` |
//! | `fulcrum_build_info` | 这个二进制自己（`CARGO_PKG_VERSION`），不需要任何活体源 |
//!
//! ★ ★ ★ **第二类为什么不在事件点记账**：能从被测对象本身问到的东西，
//! 就不要在旁边再记一份 —— 否则两份迟早不一致，而**不一致的那天没有任何东西会说**。
//! （[`crate::access_log`] 已经把这条落在 `status` / `resp_size` 上。）
//! ⚠ 代价说在明处：读数是**抓取那一刻**的瞬时值，两次抓取之间发生过什么看不见。
//! 对「在途数」「还有多久到期」这类量而言那本来就是全部真相。
//!
//! # ⚠ 公开面是 `pub` 而不是 `pub(crate)`
//!
//! ★ 不是「对外暴露」的意思：本 crate `publish = false`，`pub` 的作用域就是同一个
//! workspace 里的那个二进制，而它别的模块（[`crate::access_log`] 一族）本来就是 `pub mod`。

use fulcrum_acme::AcmeManager;
use fulcrum_runtime::SharedRuntime;
use fulcrum_tls::SniResolver;
use log::warn;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 族的类型。★ 它同时定死两件事：`# TYPE` 那一行怎么写，以及这个族**能被怎么写入**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 只增不减的 `u64`。⚠ 名字**必须**以 `_total` 结尾 —— 由单测守。
    Counter,
    /// 会上下动的 `f64`。
    Gauge,
    /// 固定桶的分布：`_bucket{le=…}` + `_sum` + `_count`。
    Histogram,
}

impl Kind {
    /// `# TYPE` 行里的那个词。★ 它同时被 panic 文案用着 —— 一处定义，两处消费。
    fn as_str(self) -> &'static str {
        match self {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        }
    }
}

/// 一个族的数**从哪来**。★ 它把本模块那条中心区分写进了声明表本身。
///
/// ⚠ ⚠ 一个族**只能有一种来处**：渲染时按它二选一去取数，
/// 于是「事件点也记一笔、抓取时又问一遍」在结构上做不到 ——
/// 那种错的表现是**同一条 series 的值忽大忽小**，而两边各自都言之凿凿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// **事件点记账**：发生一次就往进程级注册表里加一笔。
    Event,
    /// **抓取时问活体**：进程级注册表里永远没有它的数，[`render`] 那一刻现问。
    Live,
}

/// 一个指标族的**全部声明**：名字、类型、数从哪来、HELP 文本、标签名清单。
///
/// ⚠ ⚠ `labels` 那一行是**契约**：写入时给的标签值要与它**逐项对上**（个数与顺序），
/// 对不上就地 panic —— 理由见本文件顶部。
pub struct Family {
    name: &'static str,
    kind: Kind,
    /// 这个族的数从哪来。见 [`Source`]。
    source: Source,
    /// `# HELP` 行的原文。⚠ **这一行不转义**（它不是标签值）⇒ 约束落在声明这一侧：
    /// 带换行或反斜杠的 HELP 会当场把 exposition 撕坏。由单测守。
    help: &'static str,
    labels: &'static [&'static str],
}

/// 直方图的桶边界，**单位是秒**（渲染时再补一格 `+Inf`）。
///
/// ★ 写死是 G117 点名要钉住的一条：桶边界一改，抓取端那边的历史时序就接不上了 ——
/// 而它接不上的样子是**图变了**，不是**报错**。
const BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// ★ ★ **族的唯一声明处**，一个族一格；渲染顺序 = 这张表的顺序。
///
/// ⚠ 下面那几个 `pub const` 句柄按**下标**指进来 —— 重排这张表就会让某个句柄换个族，
/// 而那种错**在输出里看起来完全正常**（数字照涨，只是涨在另一条 series 上）。
/// ⇒ 单测 `族句柄指的就是它名字上那个族` 把每个句柄的名字逐个钉住。
const FAMILIES: [Family; 9] = [
    Family {
        name: "fulcrum_requests_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "请求总数，按站点地址字面量、结果、状态码类与协议分。",
        labels: &["site", "outcome", "status_class", "proto"],
    },
    Family {
        name: "fulcrum_request_duration_seconds",
        kind: Kind::Histogram,
        source: Source::Event,
        help: "请求耗时分布，单位秒。",
        labels: &["site", "outcome"],
    },
    Family {
        name: "fulcrum_cache_events_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "HTTP 缓存事件数：命中、回源、重验证后发出、被清掉的条目。",
        labels: &["event"],
    },
    Family {
        name: "fulcrum_no_site_match_total",
        kind: Kind::Counter,
        source: Source::Event,
        help: "没匹配到任何站点的请求数；host 只有出现在配置里的才带真值，其余归 <other>。",
        labels: &["host"],
    },
    Family {
        name: "fulcrum_upstream_inflight",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "每个上游地址当前在飞的连接数；同一地址被多处引用时求和。",
        labels: &["upstream"],
    },
    Family {
        name: "fulcrum_upstream_healthy",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "上游地址的健康位，1 为健康；同一地址被多处引用时全都健康才为 1；没配 health_uri 的恒为 1。",
        labels: &["upstream"],
    },
    Family {
        name: "fulcrum_cert_expiry_seconds",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "已装载证书的 notAfter，取绝对 Unix 秒；抓取端减去 time() 就是剩余量。",
        labels: &["domain"],
    },
    Family {
        name: "fulcrum_acme_issue_total",
        kind: Kind::Counter,
        source: Source::Live,
        help: "ACME 巡检的签发结果计数；deferred 是退避中或这一批接不了，不含还不到续期点的。",
        labels: &["result"],
    },
    Family {
        name: "fulcrum_build_info",
        kind: Kind::Gauge,
        source: Source::Live,
        help: "恒为 1 的版本标记；标签只有 version，别往里加会随换代变化的东西。",
        labels: &["version"],
    },
];

/// 请求总数。
///
/// ★ ★ 取数点**只能有一处**（`access_log::Record` 收尾的那一处）：两处各算一遍
/// `outcome` / `status` 迟早分家，而**分家的表现是两个数字都言之凿凿、却对不上**。
pub const REQUESTS_TOTAL: &Family = &FAMILIES[0];

/// 请求耗时分布。⚠ 单位是**秒**，与 `BUCKETS` 和名字里的 `_seconds` 同一口径。
pub const REQUEST_DURATION_SECONDS: &Family = &FAMILIES[1];

/// 缓存事件数。⚠ `event` 是闭集，折叠函数不许有兜底臂 —— 否则将来新增的一种状态
/// 会**静默地被算成别的**。
pub const CACHE_EVENTS_TOTAL: &Family = &FAMILIES[2];

/// 没匹配到站点的请求数（G118）。⚠ `host` 由请求方给 ⇒ 只有出现在配置里的才带真值，
/// 其余一律 `<other>`：**上界由配置定、不由访问者定**。
pub const NO_SITE_MATCH_TOTAL: &Family = &FAMILIES[3];

/// 每个上游**地址**当前在飞的连接数。⚠ 抓取时问 `Upstream::inflight()`，**不在这里另记一份**。
///
/// ★ 同一个地址被多处引用时**求和** —— 它是个计数，聚合只有这一种说得通的做法。
const UPSTREAM_INFLIGHT: &Family = &FAMILIES[4];

/// 每个上游**地址**的健康位。★ 没配 `health_uri` 的上游恒为 1（那与运行时那一侧的初值同一口径）。
///
/// ★ 同一个地址被多处引用时取**合取**（全都健康才是 1）—— 它是个布尔，⛔ 不求和。
const UPSTREAM_HEALTHY: &Family = &FAMILIES[5];

/// 已装载证书的 `notAfter`，**绝对 Unix 秒**（裁决 R5）。
///
/// ★ 取绝对值而不是「还剩多少秒」：绝对值不随时间漂，抓取端一句 `- time()` 就得到剩余量；
/// 而「剩余量」需要有人定期刷新 —— 那**等于在旁边再记一份会过期的东西**。
const CERT_EXPIRY_SECONDS: &Family = &FAMILIES[6];

/// ACME 巡检的签发结果计数。⚠ `deferred` 的定义写死在 `fulcrum_acme::IssueCounts` 上。
const ACME_ISSUE_TOTAL: &Family = &FAMILIES[7];

/// 恒为 1 的版本标记。
///
/// ⛔ **不带 `gen_id`、不带 pid**：那会让每一次换代长出一条新 series，
/// 而旧的那条从此再也不更新 —— 抓取端看到的是一堆看起来还活着的僵尸时序。
const BUILD_INFO: &Family = &FAMILIES[8];

/// 一条直方图 series。
struct Hist {
    /// 与 `BUCKETS` 等长的**非累积**计数：一次观测只碰一格。
    /// ★ 累积放到渲染时算 —— 观测在请求路径上（一次二分 + 一次自增），
    /// 而渲染一次抓取才发生一回。
    per_bucket: Vec<u64>,
    sum: f64,
    /// 全部观测数，**含超出最大桶的那些** ⇒ `le="+Inf"` 那一格就是它。
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

/// 注册表：**族名 → 这个族下的每一组标签值 → 读数**。
///
/// ★ 三种族各一张表，而不是一张 `BTreeMap<…, 枚举>`：这样「按 gauge 写一个 counter」
/// 在结构上就落不进同一格，渲染时也不必写一条永远走不到的 `unreachable` 臂。
#[derive(Default)]
struct Registry {
    counters: BTreeMap<&'static str, BTreeMap<Vec<String>, u64>>,
    gauges: BTreeMap<&'static str, BTreeMap<Vec<String>, f64>>,
    histograms: BTreeMap<&'static str, BTreeMap<Vec<String>, Hist>>,
}

/// 进程级注册表。形状照 [`crate::access_log`] 里那张文件句柄表（`OnceLock<Mutex<…>>`）。
fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(Registry::default()))
}

/// 拿着那把锁做一件事。
///
/// ⚠ 锁中毒时接着用里面那份数据（与 `access_log` 逐字同一口径）：**指标少几格，
/// 也不该把请求路径带崩** —— 观测面的失效不许升级成数据面的失效。
fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = registry().lock().unwrap_or_else(|p| p.into_inner());
    f(&mut g)
}

/// [`Source::Live`] 那几个族**去问谁**。
///
/// ⚠ ⚠ **三个都可以缺席**，缺席的那个族就只出 HELP/TYPE、不出样本：
/// ⛔ 不 panic（观测面的失效不许升级成数据面的失效），
/// ⛔ 也不让整族消失 —— 整族消失会让「没接上」与「没数据」在抓取端**看起来一模一样**，
/// 而那是两件完全不同的事：前者要改代码，后者什么都不用做。
///
/// ★ 存 `Arc` 而不是 `Weak`：这三个对象与进程同寿（数据面、TLS 解析器、ACME 巡检
/// 各自都被别处握着），拿 `Weak` 只会多出一条**永远走不到的** upgrade 失败分支 ——
/// 而走不到的分支既测不了，也迟早写错。
#[derive(Default)]
pub struct LiveSources {
    /// 上游那两个族。⚠ 存的是 [`SharedRuntime`] 而不是某一份 `Runtime` 快照：
    /// 全量 load 换掉的正是它里面那一份，握着快照等于指标停在换配置之前。
    pub runtime: Option<Arc<SharedRuntime>>,
    /// 证书到期时刻。
    pub resolver: Option<Arc<SniResolver>>,
    /// ACME 签发计数。`None` = 这份配置里没有自动签发（`acme::build` 返回 `None`）。
    pub acme: Option<Arc<AcmeManager>>,
}

fn live() -> &'static OnceLock<LiveSources> {
    static LIVE: OnceLock<LiveSources> = OnceLock::new();
    &LIVE
}

/// 在接线处登记一次活体源。
///
/// ⚠ **只认第一次**，重复登记会被丢掉并打一行 warn。★ 这是有意的：一个能被换掉的
/// 活体源等于给「指标现在在问谁」再开一条状态通道，而换配置那条通道已经存在 ——
/// 它就是 [`SharedRuntime`] 自己（全量 load 换的是它里面那一份，不是这个句柄）。
pub fn register_live(sources: LiveSources) {
    if live().set(sources).is_err() {
        warn!("指标的活体源已经登记过一次了，这一次被忽略（登记只认第一次）");
    }
}

/// 抓取那一刻去问活体，得到一份**临时**注册表。
///
/// ★ 它不进进程级注册表：这几个族的数**不属于我们**，属于被问的那几个对象。
/// ⚠ 这份表每次抓取新建 ⇒ 下面的 `inc_by` 在这里等价于「写入」，不是「累加」。
fn snapshot(src: &LiveSources) -> Registry {
    let mut r = Registry::default();

    // ★ `build_info` 不需要任何活体源 —— 被问的对象就是这个二进制自己。
    //   ⚠ 于是它是**唯一一个「没登记也照样有样本」**的活体族。
    r.set(BUILD_INFO, &[env!("CARGO_PKG_VERSION")], 1.0);

    if let Some(rt) = &src.runtime {
        // ⚠ 一次抓取只取一份快照：分两次取的话，两个族可能落在**两份不同的配置**上，
        //   于是同一个上游在 `inflight` 里在、在 `healthy` 里不在。
        let snap = rt.current();
        // ⚠ ⚠ 同一个地址被多个站点（或同一个站点的多条 `reverse_proxy`）写到时，
        //   在运行时图里是**多个 `Upstream` 对象、多份各自独立的状态** ——
        //   而这两个族按**地址**出一条 series（`upstream` 标签就是那个地址串，
        //   也是 `least_conn` 与健康检查共用的那个身份）。
        //   ⇒ 先归并，再出样本。**两个族的归并方式不一样，各有各的理由**：
        //
        // ★ ★ `inflight` 是**计数** ⇒ **求和**。挑其中一份报出去，得到的是一个
        //   **错的数，而它长得和对的数一模一样** —— 读这条 series 的人只会有一个理解
        //   （「打到这个地址、现在还在飞的连接一共有多少」），没有任何东西会说它只算了一半。
        //   ⚠ 共享后端（两个站点各写一条 `reverse_proxy 127.0.0.1:3000`）是很常见的配置。
        //
        // ★ ★ `healthy` 是**布尔** ⇒ **取合取**（全都健康才算健康），⛔ 不是求和
        //   （两份健康位求和会得到 2，那根本不是这个族的值域）。
        //   ★ 取合取而不是析取，是因为**混配**：站点 A 配了 `health_uri`、站点 B 没配
        //   ⇒ B 那一份**恒为 1**（运行时那侧的初值，「没配就永不探测」）。
        //   合取给出的是**真的探过的那一侧**探到的状态；析取会让一个
        //   **根本没在探测**的对象把一次真实的故障盖掉。⇒ 悲观那一侧是安全的那一侧。
        //
        // ★ 归并之后「留第一份还是留最后一份」这个问题不再存在 —— 它本来就不该存在。
        let mut by_addr: BTreeMap<&str, (u64, bool)> = BTreeMap::new();
        for t in snap.all_proxy_targets() {
            for up in &t.upstreams {
                let e = by_addr.entry(up.addr.as_str()).or_insert((0, true));
                e.0 += up.inflight() as u64;
                e.1 &= up.is_healthy();
            }
        }
        for (addr, (inflight, healthy)) in by_addr {
            r.set(UPSTREAM_INFLIGHT, &[addr], inflight as f64);
            r.set(UPSTREAM_HEALTHY, &[addr], if healthy { 1.0 } else { 0.0 });
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
        // ★ 三格**无条件都出**，哪怕是 0：一条从来没出现过的 series 与一条恒为 0 的
        //   series，在告警规则里是两种完全不同的东西（前者让 `rate()` 直接没有数据）。
        r.inc_by(ACME_ISSUE_TOTAL, &["ok"], ok);
        r.inc_by(ACME_ISSUE_TOTAL, &["fail"], fail);
        r.inc_by(ACME_ISSUE_TOTAL, &["deferred"], deferred);
    }

    r
}

/// `SystemTime` → Unix 秒。
///
/// ⚠ 1970 之前的 `notAfter` 不可能来自一张真证书，但它在类型上是可表达的 ——
/// ★ 静静回 0 会把它说成「1970 年到期」，而一个负数一眼就看得出不对。
///
/// ★ `pub(crate)`（**M2 批 N 任务 6**）：`/stats`（`admin.rs`）渲染证书到期与
/// 配置装载时间时复用**这一个**转换 —— 两处各写一份「差不多的」`SystemTime → f64`
/// 迟早会在闰秒/精度上分家，而这几行本来就没有第二种写法可选。
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

    /// +n。★ 有 `inc_by` 是因为有些事件天生成批（一次清理清掉了 N 条缓存条目），
    /// 而循环 N 次 `inc` 要多拿 N 次锁。
    pub fn inc_by(&self, labels: &[&str], n: u64) {
        with_registry(|r| r.inc_by(self, labels, n));
    }

    /// 直接写一个 gauge 读数。
    pub fn set(&self, labels: &[&str], v: f64) {
        with_registry(|r| r.set(self, labels, v));
    }

    /// 记一次观测。
    pub fn observe(&self, labels: &[&str], v: f64) {
        with_registry(|r| r.observe(self, labels, v));
    }

    /// 把一组标签值变成注册表里的键，**顺便把两条契约当场判死**。
    ///
    /// ⚠ 两条都用 `assert!` 而不是 `debug_assert!`：见本文件顶部那一节。
    fn key(&self, want: Kind, labels: &[&str]) -> Vec<String> {
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
        labels.iter().map(|s| s.to_string()).collect()
    }
}

impl Registry {
    fn inc_by(&mut self, f: &Family, labels: &[&str], n: u64) {
        let k = f.key(Kind::Counter, labels);
        *self
            .counters
            .entry(f.name)
            .or_default()
            .entry(k)
            .or_default() += n;
    }

    fn set(&mut self, f: &Family, labels: &[&str], v: f64) {
        let k = f.key(Kind::Gauge, labels);
        self.gauges.entry(f.name).or_default().insert(k, v);
    }

    fn observe(&mut self, f: &Family, labels: &[&str], v: f64) {
        let k = f.key(Kind::Histogram, labels);
        let h = self
            .histograms
            .entry(f.name)
            .or_default()
            .entry(k)
            .or_default();
        // ⚠ 桶的语义是 `le`（**小于等于**）：观测值正好落在某个边界上时归**那一格**，
        //   不是下一格。`partition_point` 给的正是第一个 `bound >= v` 的下标。
        let i = BUCKETS.partition_point(|b| *b < v);
        if let Some(c) = h.per_bucket.get_mut(i) {
            *c += 1;
        }
        // ★ 超出最大桶的观测**不进任何一格**，但照样进 `sum` 与 `count` ——
        //   `le="+Inf"` 那一格就是靠 `count` 出来的，两者因此恒等。
        h.sum += v;
        h.count += 1;
    }

    /// 把 `families` 这张声明表按顺序渲染进 `out`。
    ///
    /// `self` 是进程级注册表（[`Source::Event`] 那一类的数），
    /// `live` 是这一次抓取现问出来的那份临时表（[`Source::Live`] 那一类）。
    ///
    /// ★ 收一张表当参数、而不是直接读 `FAMILIES`：单测因此可以拿一张**小表**
    /// 逐字节钉住输出，而不必跟着真表一起长。
    fn render_into(&self, families: &[Family], live: &Registry, out: &mut String) {
        for f in families {
            // ★ ★ 一个族的数只从**一处**来，声明表里那个 `source` 说了算 ——
            //   而不是「两边都找一遍，哪边有用哪边」：后者会让一次误接线
            //   （在事件点写了一个活体族）静静地变成一条时对时错的 series。
            let r = match f.source {
                Source::Event => self,
                Source::Live => live,
            };
            // ★ ★ 没有样本的族**照样出 HELP/TYPE**：这样「这个指标存在，只是还没发生过」
            //   与「名字拼错了」在抓取端看得出区别 —— 后者是整族不见。
            //   ⚠ 活体那几个族在**没登记活体源**时走的正是这一支。
            let _ = writeln!(out, "# HELP {} {}", f.name, f.help);
            let _ = writeln!(out, "# TYPE {} {}", f.name, f.kind.as_str());
            match f.kind {
                Kind::Counter => {
                    for (labels, v) in r.counters.get(f.name).into_iter().flatten() {
                        write_series(out, f.name, f.labels, labels, None);
                        let _ = writeln!(out, " {v}");
                    }
                }
                Kind::Gauge => {
                    for (labels, v) in r.gauges.get(f.name).into_iter().flatten() {
                        write_series(out, f.name, f.labels, labels, None);
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
                            write_series(out, &bucket, f.labels, labels, Some(&format!("{bound}")));
                            let _ = writeln!(out, " {cum}");
                        }
                        // ⚠ `+Inf` 那一格写的是 `count` 而**不是** `cum`：超出最大桶的观测
                        //   不在任何一格里，两者本来就不相等，而「`+Inf` 恒等于 `_count`」
                        //   是格式的要求 ⇒ 让它们共用同一个数，别去凑。
                        write_series(out, &bucket, f.labels, labels, Some("+Inf"));
                        let _ = writeln!(out, " {}", h.count);
                        write_series(out, &format!("{}_sum", f.name), f.labels, labels, None);
                        out.push(' ');
                        write_f64(out, h.sum);
                        out.push('\n');
                        write_series(out, &format!("{}_count", f.name), f.labels, labels, None);
                        let _ = writeln!(out, " {}", h.count);
                    }
                }
            }
        }
    }
}

/// 写一个 `f64` 读数。
///
/// # ⚠ ⚠ Rust 的 `Display` 与 exposition 在无穷大上**不一致**
///
/// `f64::INFINITY` 被 `Display` 印成 `inf`，而 exposition 要的是 `+Inf`
/// （`NaN` 两边碰巧一致，不必管）。⚠ 抓取端读到 `inf` 是**解析失败**，
/// 而解析失败发生在抓取端、不在这里 —— 现场是「这一次抓取整个没了」。
///
/// ★ 在批 M 任务 1 那一版里这条路走不到：真表里一个 gauge 族都没有，
/// 而直方图的 `_sum` 永远是有限秒数。**任务 5 引入 gauge 之后它就走得到了。**
fn write_f64(out: &mut String, v: f64) {
    if v.is_infinite() {
        out.push_str(if v > 0.0 { "+Inf" } else { "-Inf" });
    } else {
        let _ = write!(out, "{v}");
    }
}

/// 写出 `<name>{k="v",k2="v2"}` 那一段（**不含**后面的值）。
///
/// `le` 是直方图那一格额外的标签，**排在声明的标签后面** —— 那是 exposition 的惯例。
/// ★ 它的值是我们自己造的（桶边界或 `+Inf`），不经转义。
fn write_series(
    out: &mut String,
    name: &str,
    label_names: &[&str],
    values: &[String],
    le: Option<&str>,
) {
    // ⚠ 个数对不上在**写入那一刻**已经 panic 过了（`Family::key`）。这里是渲染路径，
    //   一次抓取上的 panic 会把整个端点带下去 ⇒ 只在 debug 下判。
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

/// 标签值的转义 —— **这三条就是 exposition 格式的全部**。
///
/// ⚠ 多转一种或少转一种，抓取端解析出来的都是**另一个值**，而不是一个错误。
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

/// 抓取时那一坨文本（Prometheus text exposition，`version=0.0.4`）。
///
/// ⚠ 整个渲染过程握着注册表那把锁：一次抓取是分钟级的事，而请求路径上的写是微秒级的。
/// ★ ★ **问活体那一步在拿锁之前**：`Upstream::inflight()` 与 `SniResolver::expiries()`
/// 各有自己的锁，而在注册表这把锁里面去拿别人的锁，就是把两把锁排出了一个顺序 ——
/// 而那个顺序**没有任何东西在守**。
pub fn render() -> String {
    let live = match live().get() {
        Some(src) => snapshot(src),
        // ⚠ 没登记 ⇒ 那几个族只出 HELP/TYPE、不出样本（`build_info` 除外，它不需要活体源）。
        None => snapshot(&LiveSources::default()),
    };
    let mut out = String::with_capacity(4096);
    with_registry(|r| r.render_into(&FAMILIES, &live, &mut out));
    out
}

/// 只给测试用：按给定的活体源把**真表**（`FAMILIES`）渲成文本，**绕开**
/// [`live()`] 那个进程级 `OnceLock`（**M2 批 N 任务 6**）。
///
/// ★ ★ ★ **为什么不能让判据走 [`register_live`] + [`render`]**：`register_live`
/// 全进程只认第一次，而 `cargo test` 里同一个二进制装着几十个测试——谁先跑到
/// 谁就把这个 `OnceLock` 焊死，后面的测试要么读到别人的活体源，要么在它已经
/// 焊死之后徒劳地 `register_live` 一次、只落一行 warn。⇒ 本函数直接走
/// `snapshot()` + `render_into()`，与 [`render`] 唯一的差别是数据源从参数来，
/// 不摸 `live()`——本模块自己的判据（见下面 `按活体源渲真表`）早就是这么测的，
/// 这里只是把同一个手法开一个 `pub(crate)` 口子给 `admin.rs` 用。
///
/// # 用途：`admin.rs` 的 R12「同源」判据
///
/// `/stats` 按站点 × 上游列（不归并），`/metrics` 按地址归并——判据要证明
/// 两者读的是**同一组原子量**。⇒ 判据里真正调用**这一个**函数、而不是在
/// `admin.rs` 里手写一份「求和 / 取合取」——那样写出来的判据只是在跟自己的
/// 抄件比对，量不到 `/metrics` 那一侧的真实实现有没有走岔。
#[cfg(test)]
pub(crate) fn render_snapshot_for_test(src: &LiveSources) -> String {
    let mut out = String::new();
    Registry::default().render_into(&FAMILIES, &snapshot(src), &mut out);
    out
}

/// 一张现造的自签证书。★ rcgen 已经是本 crate 的 dev-dependency（QUIC 那条判据在用）。
///
/// ★ `pub(crate)`（**M2 批 N 任务 6**）：同一个理由——`admin.rs` 的 `/stats`
/// 证书到期判据要往 `SniResolver` 里塞一张真证书，别在那边再写一份一模一样的
/// rcgen 调用。
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

    /// 一张**测试专用**的小声明表。
    ///
    /// ★ 与真表分开，是因为 golden 输出要逐字节稳定，而真表会随批 M 长大 ——
    /// 让 golden 挂在真表上，等于每加一个族就要改一次判据，而改判据的人正是加族的人。
    const T: [Family; 4] = [
        Family {
            name: "t_requests_total",
            kind: Kind::Counter,
            source: Source::Event,
            help: "请求数。",
            labels: &["site", "outcome"],
        },
        // ★ 一个**不带标签**的族：它渲染出来没有那对花括号。
        Family {
            name: "t_ready",
            kind: Kind::Gauge,
            source: Source::Event,
            help: "就绪。",
            labels: &[],
        },
        Family {
            name: "t_latency_seconds",
            kind: Kind::Histogram,
            source: Source::Event,
            help: "时延，秒。",
            labels: &["route"],
        },
        // ★ 一个**一条样本都没有**的族：它照样要出 HELP/TYPE。
        Family {
            name: "t_never_written_total",
            kind: Kind::Counter,
            source: Source::Event,
            help: "一次都没发生过。",
            labels: &["x"],
        },
    ];
    const T_REQUESTS: &Family = &T[0];
    const T_READY: &Family = &T[1];
    const T_LATENCY: &Family = &T[2];

    /// 渲染一张**只有事件点记账**的表时，活体那一半是空的。
    ///
    /// ★ 抽成一个 helper，是为了让下面那几条 golden 判据读起来仍然只有「表 + 输出」两样。
    fn render_events(r: &Registry, families: &[Family], out: &mut String) {
        r.render_into(families, &Registry::default(), out);
    }

    /// counter 的名字必须以 `_total` 结尾；gauge / histogram 一个都不许。
    fn 后缀合规(f: &Family) -> bool {
        match f.kind {
            Kind::Counter => f.name.ends_with("_total"),
            Kind::Gauge | Kind::Histogram => !f.name.ends_with("_total"),
        }
    }

    #[test]
    fn counter_都以_total_结尾_而_gauge_与_histogram_都不许() {
        // ★ 先证「样本里真有东西」：一张空表也能让下面那个 for 全绿。
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

        // ★ ★ 反向那半：**这条规则判得动坏名字**。只对真表断言的话，把 `后缀合规`
        //   改成恒 `true` 一样全绿 —— 那时这条判据就已经不存在了，而没有任何迹象。
        let 坏表 = [
            Family {
                name: "bad_counter",
                kind: Kind::Counter,
                source: Source::Event,
                help: "",
                labels: &[],
            },
            Family {
                name: "bad_gauge_total",
                kind: Kind::Gauge,
                source: Source::Event,
                help: "",
                labels: &[],
            },
            Family {
                name: "bad_hist_total",
                kind: Kind::Histogram,
                source: Source::Event,
                help: "",
                labels: &[],
            },
        ];
        for f in &坏表 {
            assert!(!后缀合规(f), "`{}` 本该被判不合规", f.name);
        }
    }

    #[test]
    fn 族句柄指的就是它名字上那个族_而且表里没有重名() {
        // ⚠ 句柄是按**下标**指进声明表的 —— 重排那张表会让句柄悄悄换个族。
        //   这一条是那件事唯一会露头的地方。
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
        // ⛔ `build_info` 只许带 `version` —— 加上 `gen_id` 或 pid 会让每次换代
        //   长出一条新 series，而旧的那条从此再也不更新。
        assert_eq!(BUILD_INFO.name, "fulcrum_build_info");
        assert_eq!(BUILD_INFO.labels, &["version"]);

        // ★ ★ 每个族的**来处**也逐个钉住：`source` 写错不会让任何一条内容断言变红 ——
        //   活体族被标成 `Event` 的表现是它**永远没有样本**，而那与「没接上活体源」
        //   长得一模一样；事件族被标成 `Live` 的表现是它**永远是 0**。
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
            ]
        );

        let mut names: Vec<&str> = FAMILIES.iter().map(|f| f.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "声明表里有重名的族");
    }

    #[test]
    fn help_文本里没有需要转义的字符() {
        // ★ `# HELP` 那一行不转义 ⇒ 约束落在声明这一侧，而不是渲染那一侧。
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
        }];
        let 原值 = "a\"b\\c\nd\te/f'g=h{i}中";
        // ★ 先证样本里真有那三种字符 —— 否则下面那条 golden 只是在量一个普通字符串。
        assert!(原值.contains('"') && 原值.contains('\\') && 原值.contains('\n'));
        // ★ 也要有**不该被碰**的字符：否则「把每个字符都转义掉」照样能通过上半条。
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
        // ★ 这一组值有意覆盖四种位置：比最小桶还小、**正好落在边界上**、
        //   夹在两个边界之间、比最大桶还大。
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
        // ⚠ 30.0 超出最大桶 ⇒ 它不在任何一格里，但必须在 `+Inf` 与 `_count` 里。
        assert!(out.contains("t_latency_seconds_bucket{route=\"/x\",le=\"+Inf\"} 8\n"));
        assert!(out.contains("t_latency_seconds_count{route=\"/x\"} 8\n"));
        // ★ 最后一个有限桶是 7 而不是 8 —— 少了这一条，「+Inf == _count」就是句废话。
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

        // ⚠ 反着写一遍：「写入顺序泄漏进输出」是换成 `HashMap` 之后的失效形态，
        //   而它**不会让任何一条内容断言变红** —— 除非像这里一样直接比两份输出。
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
        // ⚠ 有意先写 `b.` 再写 `a.`：输出必须按标签值排序，不按写入顺序。
        r.inc_by(T_REQUESTS, &["b.example", "reverse_proxy"], 2);
        r.inc_by(T_REQUESTS, &["a.example", "files"], 1);
        r.set(T_READY, &[], 1.0);
        r.observe(T_LATENCY, &["/api"], 0.25);
        r.observe(T_LATENCY, &["/api"], 3.0);

        let mut out = String::new();
        render_events(&r, &T, &mut out);

        // ★ 用数组 join 拼期望值，而不是一个多行字符串字面量：宿主是 Windows，
        //   多行字面量会把**源文件的行尾**一起量进判据里（`.gitattributes` 钉了 LF，
        //   但一条判据不该把自己的正确性挂在另一处配置上）。
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

    #[test]
    fn 写入_api_走的是进程级那张表_而_render_出全部声明的族() {
        // ⚠ 这一条走**真的**进程级注册表（别的判据都用局部注册表，否则同一个测试
        //   二进制里几条测试会把数写进对方的断言里）。
        //   ★ 下面这几组标签值只此一处用过 ⇒ 计数是确定的。
        REQUESTS_TOTAL.inc(&["<unit-test>", "metrics", "2xx", "HTTP/1.1"]);
        REQUEST_DURATION_SECONDS.observe(&["<unit-test>", "metrics"], 0.25);
        NO_SITE_MATCH_TOTAL.inc_by(&["<unit-test-other>"], 3);
        // ★ `set` 那条路也要走一遍，而真表里今天没有 gauge 族 ⇒ 借测试表的那个。
        //   它不在 `FAMILIES` 里，所以 `render()` 不会把它渲出来 —— 下面单独按 `T` 渲一次。
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
        // ★ 整体以换行收尾 —— exposition 的最后一行也得是一行。
        assert!(out.ends_with('\n'));

        let 全局按测试表渲染 = with_registry(|r| {
            let mut s = String::new();
            render_events(r, &T, &mut s);
            s
        });
        assert!(
            全局按测试表渲染.contains("\nt_ready 1\n"),
            "{全局按测试表渲染}"
        );
    }

    // ── 抓取时问活体那一半（**批 M 任务 5**）────────────────────────────────

    /// 把真表按给定的活体源渲一遍。★ 事件那一半用一张**空的**注册表，
    /// 于是每一条断言看到的都只是活体那一半 —— 否则同一个测试二进制里别的判据
    /// 写进进程级表的数会漏进来。
    ///
    /// ★ 就是 [`render_snapshot_for_test`]——**M2 批 N 任务 6**把这个手法开了
    /// 一个 `pub(crate)` 口子给 `admin.rs` 的 R12 判据用，这里改成调它，
    /// 别让同一段「渲染一张真表」的代码在本文件里存在两份。
    fn 按活体源渲真表(src: &LiveSources) -> String {
        render_snapshot_for_test(src)
    }

    /// 这个族出了几条样本行（`# HELP` / `# TYPE` 不算）。
    fn 样本行(out: &str, family: &str) -> Vec<String> {
        out.lines()
            .filter(|l| l.starts_with(family) && !l.starts_with('#'))
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn 没登记活体源时那几个族有_help_type_但一条样本都没有() {
        // ⚠ ⚠ 这一条守的是「**没接上**」与「**没数据**」在抓取端看得出区别：
        //   整族消失的话两者长得一模一样，而前者要改代码、后者什么都不用做。
        let out = 按活体源渲真表(&LiveSources::default());

        for name in [
            "fulcrum_upstream_inflight",
            "fulcrum_upstream_healthy",
            "fulcrum_cert_expiry_seconds",
            "fulcrum_acme_issue_total",
        ] {
            assert!(out.contains(&format!("# HELP {name} ")), "缺 HELP：{name}");
            assert!(out.contains(&format!("# TYPE {name} ")), "缺 TYPE：{name}");
            assert!(
                样本行(&out, name).is_empty(),
                "{name} 在没有活体源时出了样本：{:?}",
                样本行(&out, name)
            );
        }
        // ★ ★ 反向那半：`build_info` **不需要活体源**，所以它在同一次渲染里必须有样本 ——
        //   少了这一条，把整个活体渲染删掉，上面那个循环照样全绿。
        assert_eq!(样本行(&out, "fulcrum_build_info").len(), 1, "{out}");
    }

    #[test]
    fn build_info_恒为_1_且只有一条() {
        // ⛔ 不带 gen_id、不带 pid ⇒ 无论问谁、问几次，都只有这一条。
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
                    "fulcrum_build_info{{version=\"{}\"}} 1",
                    env!("CARGO_PKG_VERSION")
                )],
                "{out}"
            );
        }
    }

    /// 两个站点引用同一个地址的那份运行时图。
    ///
    /// ★ `127.0.0.1:9001` 被 `a.example` 与 `b.example` 各写一条（共享后端，很常见的配置），
    /// `127.0.0.1:9002` 只有一处 —— 于是同一次渲染里「归并过的」与「没归并过的」都在。
    fn 共享后端的运行时图() -> std::sync::Arc<fulcrum_runtime::Runtime> {
        let outcome = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "http://a.example {\n  reverse_proxy 127.0.0.1:9001 127.0.0.1:9002\n}\n\
             http://b.example {\n  reverse_proxy 127.0.0.1:9001\n}\n",
        );
        let cfg = outcome.config.expect("配置编译不过");
        std::sync::Arc::new(fulcrum_runtime::Runtime::build(&cfg).expect("运行时图建不起来"))
    }

    /// 运行时图里地址等于 `addr` 的那些 `Upstream`，**按遍历序**。
    ///
    /// ★ ★ 每条判据都先拿它证一次「样本里真有两个对象」：前提不成立的话，
    /// 「求和」「合取」都是在量一张压根没有重复的表 —— 那种判据永远绿，什么都没守。
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
        // ⚠ ⚠ series 的键是**上游地址串**，也就是 `least_conn` 与健康检查共用的那个身份。
        let rt = 共享后端的运行时图();
        assert_eq!(
            同址上游(&rt, "127.0.0.1:9001").len(),
            2,
            "前提没成立：夹具里没有被重复引用的上游"
        );
        let out = 按运行时图渲真表(rt);

        // 两个族各出**两条** —— 三个 `Upstream` 对象归并成两个地址。
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
        // 健康位：没配 `health_uri` 的上游恒为 1（运行时那一侧的初值）。
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
        // ★ ★ `inflight` 是个**计数**，聚合只有一种说得通的做法：求和。
        //   报其中一份等于报一个**错的数，而它长得和对的数一模一样** ——
        //   读这条 series 的人只会有一个理解：打到这个地址的连接一共有多少。
        let rt = 共享后端的运行时图();
        let 两份 = 同址上游(&rt, "127.0.0.1:9001");
        assert_eq!(两份.len(), 2, "前提没成立：夹具里没有被重复引用的上游");

        // ⚠ ⚠ 两份**都非 0 且互不相等**（2 与 3 ⇒ 期望 5）：
        //   取 0 与 N 的话，「只取第一份」或「只取最后一份」在某个遍历序下会**碰巧**通过 ——
        //   而碰巧通过的判据与不存在的判据无法区分。
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
        // ★ ★ `healthy` 是个**布尔**，聚合是**取合取**，⛔ 不是求和（求和会得到 2，
        //   那根本不在这个族的值域里）。
        // ★ 取合取而不是析取，因为**混配**：一个站点配了 `health_uri`、另一个没配
        //   ⇒ 后者恒为 1（「没配就永不探测」）。合取给出的是真的探过的那一侧探到的状态；
        //   析取会让一个**根本没在探测**的对象把一次真实的故障盖掉。
        //   ⇒ 悲观那一侧是安全的那一侧。
        let rt = 共享后端的运行时图();
        let 两份 = 同址上游(&rt, "127.0.0.1:9001");
        assert_eq!(两份.len(), 2, "前提没成立：夹具里没有被重复引用的上游");

        // ── 方向一：两份都健康 ⇒ 1（也就是上面那条判据的初态，这里显式再走一遍）
        assert!(
            两份[0].is_healthy() && 两份[1].is_healthy(),
            "初值应当都是健康"
        );
        assert!(
            按运行时图渲真表(rt.clone())
                .contains("fulcrum_upstream_healthy{upstream=\"127.0.0.1:9001\"} 1\n")
        );

        // ── 方向二：只有**第二份**被探测判成不健康 ⇒ 0
        //   ⚠ 挑第二份而不是第一份：挑第一份的话，一个「只看第一份」的实现照样能过。
        两份[1].set_healthy(false);
        assert!(两份[0].is_healthy(), "第一份不该被连带改掉");
        let out = 按运行时图渲真表(rt);
        assert_eq!(
            样本行(&out, "fulcrum_upstream_healthy"),
            vec![
                "fulcrum_upstream_healthy{upstream=\"127.0.0.1:9001\"} 0".to_string(),
                // ★ 没被碰过的那个地址不受影响 —— 否则「全判成 0」也能过上面那条。
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
        // ⛔ 挑战证书是一张只活几秒的一次性自签证书 ——
        //   混进这个族会让「快过期了」那类告警一直在叫，而叫的是一个不该被续期的东西。
        let _guard = resolver.provision_challenge("challenge.example", 自签("challenge.example"));
        assert_eq!(resolver.challenge_len(), 1, "前提没成立：挑战证书没挂上");

        let out = 按活体源渲真表(&LiveSources {
            resolver: Some(resolver),
            ..Default::default()
        });
        // ★ 值是 `notAfter` 的**绝对 Unix 秒**（裁决 R5），不是「还剩多少秒」。
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
        // ★ 反向那半：绝对值必须**远大于**一个「剩余量」会有的数量级。
        //   少了这一条，把 `unix_secs` 换成 `notAfter - now` 之后上面那条只需跟着改一次期望。
        assert!(
            到期.duration_since(UNIX_EPOCH).expect("同上").as_secs() > 1_700_000_000,
            "取到的不像一个绝对 Unix 秒"
        );
    }

    #[test]
    fn acme_那三格无条件都出_哪怕一次都没签过() {
        // ★ 一条**从来没出现过**的 series 与一条**恒为 0** 的 series，在告警规则里
        //   是两种完全不同的东西：前者让 `rate()` 直接没有数据可算。
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
    fn 非有限浮点按_exposition_写_而有限值原样() {
        // ⚠ ⚠ Rust 的 `Display` 把 `f64::INFINITY` 印成 `inf`，而 exposition 要 `+Inf`。
        //   ★ 批 M 任务 1 那一版走不到这条路（真表里一个 gauge 族都没有）；
        //     任务 5 引入 gauge 之后它就走得到了。
        const G: [Family; 1] = [Family {
            name: "t_inf",
            kind: Kind::Gauge,
            source: Source::Event,
            help: "无穷。",
            labels: &["v"],
        }];
        let mut r = Registry::default();
        r.set(&G[0], &["pos"], f64::INFINITY);
        r.set(&G[0], &["neg"], f64::NEG_INFINITY);
        r.set(&G[0], &["nan"], f64::NAN);
        // ★ 反向那半：有限值**不许**被改写 —— 少了它，一个「所有 gauge 都印成 +Inf」
        //   的实现照样能通过上面三条。
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

    /// ★ ★ ★ **`observability.md` 那张基数表要与 [`FAMILIES`] 逐项对得上。**
    ///
    /// # 为什么要有这一条
    ///
    /// 那张表此前写着 `8 × 5 × 3`，而 `status_class` 落地成**六个**值的那一刻它就成了假话
    /// —— 从那一刻到有人手工改掉它为止，**没有任何东西会红**。
    /// ⇒ 一条靠「下一个人记得同时改文档」维持的正确性，本仓已经实测失效过一次。
    /// ★ 文档是这个族清单的**权威**（本模块顶部就这么写的），而权威与实现之间
    /// 此前一道门都没有。
    ///
    /// # ⚠ ⚠ 两个方向都要，少一个这道门就形同虚设
    ///
    /// 只断言「每个族都在表里」⇒ **删掉一个族而忘了删表里那行**，照样全绿；
    /// 只断言「表里每行都是真族」⇒ **新增一个族而忘了写进表**，照样全绿。
    ///
    /// # ★ 钉的是**族名清单**，不是那些算式
    ///
    /// ⛔ 不钉 `8 × 6 × 3` 那种上界推导：那是写给人看的推导，钉住它只会让这道门
    /// 在每次增删一个标签值时变成噪音。**这一批真正漂掉的是「表和代码对不对得上」。**
    ///
    /// # 锚点是**表行的形状**，不是小节标题
    ///
    /// 取「首格是一个反引号包起来、`fulcrum_` 开头的名字」的表行。
    /// ⇒ 标题怎么改都不影响它；而任何一行**自称是个族**的表行都会被问一遍，
    /// 无论它出现在文档的哪一节。
    #[test]
    fn 文档里那张基数表与声明表逐项对得上() {
        /// 一行 Markdown 表行的首格里那个族名。⚠ 必须确认首格**后面紧跟 ` |`** ——
        /// 否则句子里的行内 code 也会被当成表行。
        fn 表行里的族名(行: &str) -> Option<&str> {
            let 余 = 行.strip_prefix("| `")?;
            let (名, 尾) = 余.split_once('`')?;
            if !尾.starts_with(" |") {
                return None;
            }
            名.starts_with("fulcrum_").then_some(名)
        }

        const DOC: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/architecture/observability.md"
        ));

        let 表里的: std::collections::BTreeSet<&str> =
            DOC.lines().filter_map(表行里的族名).collect();

        // ── ★ ★ 判据自身先自证：样本里真的有东西 ──────────────────────────
        //
        // ★ 本仓那条老纪律：**取样前先确认样本里真有东西** —— 一个把文档读成空串、
        //   或者一行都没命中的解析器，会让**方向 ②** 恒绿（空集里当然找不出多余的行）。
        // ⚠ 判据失效时它不会沉默，它会给一个像样的错答案：「文档里没有多余的行」。
        assert!(!DOC.is_empty(), "observability.md 读进来是空的");
        assert!(
            !表里的.is_empty(),
            "从 observability.md 里一行族名都没解析出来。\n\
             ⇒ **先怀疑解析没命中**（那张表的写法变了？），而不是文档真的一行都没有。"
        );

        // ⚠ ⚠ **这里有意不写「解析出的族名 ≥ FAMILIES.len()」那条计数自证**：
        //   它与下面的**方向 ①** 是同一句话，而写在前面会把 ① 的报错**盖掉** ——
        //   删掉表里一行时，红的会是「只解析出 8 个」而不是「少了哪一个」。
        //   ★ 一条把更精确的判据挡在后面的自证，是负收益。
        //   ⇒ 计数由方向 ① 顺带守住：① 过了就意味着 表里的 ⊇ 声明的。
        let 声明的: std::collections::BTreeSet<&str> = FAMILIES.iter().map(|f| f.name).collect();

        // ── 方向 ①：声明表里的每一个族，文档那张表里都要有一行 ────────────
        let 文档缺的: Vec<&str> = 声明的.difference(&表里的).copied().collect();
        assert!(
            文档缺的.is_empty(),
            "这些族在 `FAMILIES` 里有、而 docs/architecture/observability.md \
             的基数表里没有：{文档缺的:?}\n\
             ⇒ 新增一个族就要同时给它写上界 —— **说不出上界的标签正是那张表存在的理由**。"
        );

        // ── 方向 ②：文档那张表里的每一行，都要真的是一个族 ────────────────
        let 文档多的: Vec<&str> = 表里的.difference(&声明的).copied().collect();
        assert!(
            文档多的.is_empty(),
            "docs/architecture/observability.md 的基数表里有这些行，而 `FAMILIES` \
             里没有对应的族：{文档多的:?}\n\
             ⇒ 要么族名拼错了，要么删族时忘了删那一行。\
             ⚠ 一行描述着不存在的指标的文档，比没有那一行更贵。"
        );
    }
}
