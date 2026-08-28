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
//! # 声明表里的族都在事件点记账
//!
//! ⇒ 渲染只是把注册表里的数抄出来。⚠ 「抓取时去问活体」那一类（上游在途数、证书到期时刻）
//! **还没有接线**，它们也还不在声明表里。
//!
//! # ⚠ 公开面是 `pub` 而不是 `pub(crate)`
//!
//! ★ 不是「对外暴露」的意思：本 crate `publish = false`，`pub` 的作用域就是同一个
//! workspace 里的那个二进制，而它别的模块（[`crate::access_log`] 一族）本来就是 `pub mod`。
//! ⚠ 写入 API 现在还没有调用方，`pub(crate)` 会被 `dead_code` 当场判死，
//! 而本仓库零 `#[allow(dead_code)]` —— 那条路是堵死的。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};

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

/// 一个指标族的**全部声明**：名字、类型、HELP 文本、标签名清单。
///
/// ⚠ ⚠ `labels` 那一行是**契约**：写入时给的标签值要与它**逐项对上**（个数与顺序），
/// 对不上就地 panic —— 理由见本文件顶部。
pub struct Family {
    name: &'static str,
    kind: Kind,
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
const FAMILIES: [Family; 4] = [
    Family {
        name: "fulcrum_requests_total",
        kind: Kind::Counter,
        help: "请求总数，按站点地址字面量、结果、状态码类与协议分。",
        labels: &["site", "outcome", "status_class", "proto"],
    },
    Family {
        name: "fulcrum_request_duration_seconds",
        kind: Kind::Histogram,
        help: "请求耗时分布，单位秒。",
        labels: &["site", "outcome"],
    },
    Family {
        name: "fulcrum_cache_events_total",
        kind: Kind::Counter,
        help: "HTTP 缓存事件数：命中、回源、重验证后发出、被清掉的条目。",
        labels: &["event"],
    },
    Family {
        name: "fulcrum_no_site_match_total",
        kind: Kind::Counter,
        help: "没匹配到任何站点的请求数；host 只有出现在配置里的才带真值，其余归 <other>。",
        labels: &["host"],
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
    /// ★ 收一张表当参数、而不是直接读 `FAMILIES`：单测因此可以拿一张**小表**
    /// 逐字节钉住输出，而不必跟着真表一起长。
    fn render_into(&self, families: &[Family], out: &mut String) {
        for f in families {
            // ★ ★ 没有样本的族**照样出 HELP/TYPE**：这样「这个指标存在，只是还没发生过」
            //   与「名字拼错了」在抓取端看得出区别 —— 后者是整族不见。
            let _ = writeln!(out, "# HELP {} {}", f.name, f.help);
            let _ = writeln!(out, "# TYPE {} {}", f.name, f.kind.as_str());
            match f.kind {
                Kind::Counter => {
                    for (labels, v) in self.counters.get(f.name).into_iter().flatten() {
                        write_series(out, f.name, f.labels, labels, None);
                        let _ = writeln!(out, " {v}");
                    }
                }
                Kind::Gauge => {
                    for (labels, v) in self.gauges.get(f.name).into_iter().flatten() {
                        write_series(out, f.name, f.labels, labels, None);
                        let _ = writeln!(out, " {v}");
                    }
                }
                Kind::Histogram => {
                    for (labels, h) in self.histograms.get(f.name).into_iter().flatten() {
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
                        let _ = writeln!(out, " {}", h.sum);
                        write_series(out, &format!("{}_count", f.name), f.labels, labels, None);
                        let _ = writeln!(out, " {}", h.count);
                    }
                }
            }
        }
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
pub fn render() -> String {
    let mut out = String::with_capacity(2048);
    with_registry(|r| r.render_into(&FAMILIES, &mut out));
    out
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
            help: "请求数。",
            labels: &["site", "outcome"],
        },
        // ★ 一个**不带标签**的族：它渲染出来没有那对花括号。
        Family {
            name: "t_ready",
            kind: Kind::Gauge,
            help: "就绪。",
            labels: &[],
        },
        Family {
            name: "t_latency_seconds",
            kind: Kind::Histogram,
            help: "时延，秒。",
            labels: &["route"],
        },
        // ★ 一个**一条样本都没有**的族：它照样要出 HELP/TYPE。
        Family {
            name: "t_never_written_total",
            kind: Kind::Counter,
            help: "一次都没发生过。",
            labels: &["x"],
        },
    ];
    const T_REQUESTS: &Family = &T[0];
    const T_READY: &Family = &T[1];
    const T_LATENCY: &Family = &T[2];

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
                help: "",
                labels: &[],
            },
            Family {
                name: "bad_gauge_total",
                kind: Kind::Gauge,
                help: "",
                labels: &[],
            },
            Family {
                name: "bad_hist_total",
                kind: Kind::Histogram,
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
        r.render_into(&E, &mut out);

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
        r.render_into(&T, &mut out);

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
        a.render_into(&T, &mut s1);
        let mut s2 = String::new();
        a.render_into(&T, &mut s2);
        let mut s3 = String::new();
        b.render_into(&T, &mut s3);

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
        r.render_into(&T, &mut out);

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
            r.render_into(&T, &mut s);
            s
        });
        assert!(
            全局按测试表渲染.contains("\nt_ready 1\n"),
            "{全局按测试表渲染}"
        );
    }
}
