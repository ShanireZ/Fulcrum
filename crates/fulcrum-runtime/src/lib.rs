//! 枢衡的**运行时对象图**：结构化配置 → 站点索引 / 匹配器 / 执行链。
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
//! # 三条设计约束
//!
//! 1. **不引用 `pingora-core`** —— 路由语义长在 `HttpServerApp` 里就只能靠真流量测，
//!    而那样红了指不到具体哪条规则。
//! 2. **构建期把话说完** —— 正则、CIDR、上游地址全在 [`Runtime::build`] 里解析并报错。
//!    留到请求路径上发现的配置错误，就是把配置问题变成了线上事故。
//! 3. **「解析得过」不等于「接上了」** —— 未接线的能力由 [`UNWIRED`] 逐条列出，
//!    并由一条测试钉住。

pub mod glob;
pub mod matcher;
pub mod proxyproto;
pub mod request;
pub mod template;

use fulcrum_config::host::wildcard_covers;
// ★ 与 `remote_ip` 匹配器**同一份** CIDR 实现（G50 的邻居）——
//   另写一份「差不多的」网段匹配，就是把 v4/v6 不互通那条坑再挖一次。
use fulcrum_config::model::{
    Defaults, HeaderOp, L4Config, MatcherRef, Step, StepBody, StructuredConfig, TlsConfig, TlsMode,
};
use glob::Cidr;
use matcher::{BuildError, CompiledMatcher};
use request::{RequestCtx, ResponseCtx};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use template::Template;

/// DSL 认得、但**运行时还不做**的能力。
///
/// ★ 未实现要在**装载时**可见（同 G52 的回落清单）：一条「编译通过、运行时静静什么都不做」
/// 的指令，正是本仓库反复抓到的「声明了却没人接」。
///
/// ⚠ `tests/unwired_contract.rs` **逐字钉住**这份清单：接线时必须同时删掉这里的条目，
/// 于是「实现了忘了删」与「删了没实现」都过不去。
/// ⚠ 这张表只登记「**DSL 认得、运行时不做**」—— 一个连语法都还不存在的能力不属于这一类
/// （写进来反而会让人以为写得出来），一个「做了但有一块够不着」的能力也不属于（那是 D 号）。
pub const UNWIRED: &[(&str, &str)] = &[
    (
        "tls_internal",
        "`tls internal` 的自签要一个证书生成器，随 ACME 那一批一起做",
    ),
    // ⚠ 别照抄旧理由。旧的是「`resolve()` 是同步的，需要一座桥」，那是 rustls 那侧的形状；
    //   换 BoringSSL 之后桥本来就在（`set_async_select_certificate_callback` 原生异步）。
    //   ★ **一条阻塞理由消失不等于那件事做完了** —— 剩下的活儿见登记词。
    (
        "on_demand",
        "握手期按 SNI 现签（G15）—— 欠 `ask` 端点、并发闸门与失败缓存",
    ),
    ("tracing", "预留指令，M1 不产生行为（G60）"),
    (
        "passive_fail",
        "被动熔断不在 M1 清单上（G17 里它与健康检查是两件事）；等 M2 排期",
    ),
];

/// 负载均衡策略。
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

/// 一个上游。
///
/// ⚠ ⚠ **请求路径只读 [`Upstream::dial_addr`]，永不做 DNS。** `HttpPeer::new` 里是
/// `address.to_socket_addrs().unwrap()` —— 阻塞的 `getaddrinfo`，而且失败就 panic
/// ⇒ 一个解析不了的域名上游会让那个站点**每个请求 panic 一次**。
/// 解析由 [`resolve_upstreams`] 在启动时、全量 load 时与后台任务里做（后者即 `dns_refresh`）；
/// 解析不出来的上游 [`ProxyTarget::pick`] 跳过，全都跳过就回干净的 502。
#[derive(Debug)]
pub struct Upstream {
    /// 配置里写的 `host:port`，**原样保留**。
    ///
    /// ⚠ 它仍然是 **SNI 的来源**：上游走 https 时要拿域名去握手，不能拿 IP。
    pub addr: String,
    /// 解析好的地址。`None` = 还没解析出来，或上一次解析失败。
    ///
    /// ★ IP 字面量在 [`Runtime::build`] 里就填好了（那不需要 DNS，也不会变）；
    /// ⚠ **域名一律留空**，由 `serve` 那一侧去解析 —— 否则 `fulcrum validate`
    /// 就会变成一个要联网的命令，而它的全部价值在于**离线**就能说话。
    /// ⚠ ⚠ **存的是全部候选，不是第一个** —— `localhost` 的第一个地址可能是 `[::1]`
    /// 而上游只听 `127.0.0.1`。只取第一个会挑中一个对端根本没在听的地址族，
    /// 而现场只有一句「连不上上游」。⇒ 连接失败就试下一个。
    resolved: std::sync::RwLock<Vec<std::net::SocketAddr>>,
    /// 当前在飞的连接数，`least_conn` 用。由数据面维护。
    inflight: AtomicUsize,
    /// 主动健康检查的判定。★ **初值是 `true`（健康）**。
    ///
    /// ⚠ 取 `false` 会把「探测还没跑」变成一次**确定的全站 502**；取 `true` 只是把
    /// 那段窗口退化成「没有健康检查」。nginx 与 Caddy 同取后者。
    /// ⚠ 代价：**刚启动的那一个探测周期内 `health_uri` 等于没有**。
    /// ★ 没配 `health_uri` 的目标永不被探测，这一格恒为 `true`。
    healthy: std::sync::atomic::AtomicBool,
}

impl Upstream {
    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Relaxed)
    }

    /// 这个上游当前的**全部**候选地址。空 = 现在用不了。
    pub fn dial_candidates(&self) -> Vec<std::net::SocketAddr> {
        match self.resolved.read() {
            Ok(g) => g.clone(),
            // 锁中毒不该让转发停摆；当成「暂时用不了」，交给别的上游。
            Err(p) => p.into_inner().clone(),
        }
    }

    /// 第一个候选。`None` = 这个上游现在用不了。
    ///
    /// ⚠ 转发路径**不要只用它**——见 `resolved` 字段上那段：
    /// 第一个候选可能是一个对端根本没在听的地址族。
    pub fn dial_addr(&self) -> Option<std::net::SocketAddr> {
        self.dial_candidates().into_iter().next()
    }

    /// 记下一次解析结果。空向量 = 这次没解析出来。
    pub fn set_resolved(&self, addrs: Vec<std::net::SocketAddr>) {
        match self.resolved.write() {
            Ok(mut g) => *g = addrs,
            Err(p) => *p.into_inner() = addrs,
        }
    }

    /// 这个上游是不是 IP 字面量（那就永远不必再解析）。
    pub fn is_literal_ip(&self) -> bool {
        self.addr.parse::<std::net::SocketAddr>().is_ok()
    }

    /// 主动健康检查判它活着吗。没配 `health_uri` 时恒 `true`。
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// 记下一次探测结论。**返回它是不是翻转了**。
    ///
    /// ★ 返回翻转与否，是为了让调用方能做到「只在状态翻转时说话」。
    /// ⚠ 批 10 那条教训：一个从头坏到尾的上游每一轮 warn 一行，
    /// 一天就是上万行，而日志被淹掉之后真正**新**出现的那一条也没人看得见。
    /// 把「翻没翻」算在这里而不是让每个调用方自己记上一次的值，
    /// 是因为后者会有第二份状态，而两份状态迟早对不上。
    pub fn set_healthy(&self, v: bool) -> bool {
        self.healthy.swap(v, Ordering::Relaxed) != v
    }
    /// 借一个位置。数据面在建立上游连接前调用。
    pub fn acquire(&self) {
        self.inflight.fetch_add(1, Ordering::Relaxed);
    }
    /// 还回来。★ 必须与 [`Self::acquire`] 成对，否则 `least_conn` 会**单调漂移**
    /// 到永远不选这个上游——而那不会有任何报错。
    pub fn release(&self) {
        // saturating：配对错了也不该 wrap 成一个天文数字。
        let _ = self
            .inflight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }
}

/// 一条 `reverse_proxy` 的运行时形态。
#[derive(Debug)]
pub struct ProxyTarget {
    pub upstreams: Vec<Upstream>,
    pub policy: LbPolicy,
    pub header_up: Vec<HeaderOpRt>,
    pub header_down: Vec<HeaderOpRt>,
    /// 上游用 https。
    pub tls: bool,
    pub tls_insecure_skip_verify: bool,
    /// 主动健康检查。`None` = 没配 `health_uri`，这一组上游永远不被探测。
    pub health: Option<HealthPolicy>,
    /// 上一次探测这一组上游的时刻。`None` = 还没探过。
    ///
    /// ★ 它存在**目标**上而不是全局，是为了让每条 `reverse_proxy` 各自的
    /// `health_interval` 真的算数。⚠ 与 `dns_refresh` 那边的简化**有意不同**：
    /// 那边「刷得更勤」只是多几次 `getaddrinfo`，而这边刷得更勤是**打在别人服务上的流量**——
    /// 配了 `30s` 的后端被每 2 秒探一次，是一个用户没有要求的行为改变。
    last_probe: std::sync::Mutex<Option<std::time::Instant>>,
    /// `round_robin` 的游标。
    cursor: AtomicUsize,
    /// `random` 的种子（进程启动时定一次）。
    seed: u64,
}

/// 一组上游的主动健康检查参数（`health_*`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthPolicy {
    /// 探测请求的路径。**它就是 `health_uri` 的值**。
    pub uri: String,
    pub interval: std::time::Duration,
    pub timeout: std::time::Duration,
    /// 认为「健康」的状态码。
    pub status: StatusPattern,
}

/// `health_status` 的两种写法：一个具体状态码（`200`）或一族（`2xx`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPattern {
    Exact(u16),
    /// 首位数字，`2` = `2xx`。
    Family(u16),
}

impl StatusPattern {
    /// 解析 `health_status` 的值。配置层已经校验过形状（`ArgType::StatusPattern`），
    /// 这里再解一次是因为**结构化层是公开入口**（G11）——
    /// 一份手写的 JSON 可以带任何字符串进来。
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

/// 这一组上游现在该探了吗。
///
/// ★ **纯函数，时间从参数进来**（本仓库的老规矩）：不这样的话，
/// 「配了 30s 的目标不会被每 2 秒探一次」这条判据就只能靠真的等 30 秒来测。
pub fn probe_due(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    interval: std::time::Duration,
) -> bool {
    match last {
        // 还没探过 —— 立刻探。⚠ 不要「等一个周期再开始」：
        //   那会让 `health_interval 30s` 的目标在启动后半分钟内完全没有保护，
        //   而那恰好是刚换代、最可能有上游没起来的时候。
        None => true,
        Some(t) => now.duration_since(t) >= interval,
    }
}

impl ProxyTarget {
    /// 选一个上游。返回 `None` = **一个可用的都没有**，调用方回 502。
    ///
    /// 筛子有两条：地址解析得出来，**且**主动健康检查没把它判死（见 [`HealthPolicy`]）。
    /// ⚠ **不含**被动熔断（`passive_fail`，还在 [`UNWIRED`] 里）：一个探测路径回 200、
    /// 而真实业务在 500 的上游，这里照样会选中它。
    pub fn pick(&self, req: &RequestCtx<'_>) -> Option<&Upstream> {
        self.pick_by(req.remote_ip)
    }

    /// [`pick`](Self::pick) 的核心，**不要求有一个 HTTP 请求**。
    ///
    /// L4 那条路上没有 [`RequestCtx`]，但要的是同一个筛子与同一套 `lb_policy`。
    /// ⚠ 不让 L4 自己写一个小轮询：那会是**第二份**「怎么挑上游」的实现，
    /// 两份一开始总是一致的，在**下一次改动**时分家（比如给 `pick` 加 `passive_fail` 那天）。
    /// ⇒ 让分家在结构上做不到，比让两份互相钉着可靠。
    pub fn pick_by(&self, client_ip: Option<IpAddr>) -> Option<&Upstream> {
        self.pick_index_by(client_ip)
            .and_then(|i| self.upstreams.get(i))
    }

    /// 同 [`pick_by`](Self::pick_by)，但返回**下标**。
    ///
    /// ★ L4 那条路要它：一条 TCP 连接在**建连之前**换上游是安全的（一个字节都还没走），
    /// 于是它从这个下标开始、按上游列表顺序往后试。⚠ HTTP 那边**有意不这么做** ——
    /// 那里「连不上就换一个上游」会滑向重试语义（请求体可能已经发出去了），
    /// 而这两件事的边界必须画在明处，不能靠「反正差不多」。
    pub fn pick_index_by(&self, client_ip: Option<IpAddr>) -> Option<usize> {
        // ★ ★ **先筛掉用不了的，再按策略在剩下的里面挑。**
        //   ⚠ 顺序反过来（先挑再看能不能用）会让一个用不了的上游把请求吞掉——
        //   而它在轮询里占着一格，症状是「N 个上游里每 N 个请求坏一个」。
        //
        //   两条：① 域名解析得出来（批 10）；② 健康检查没判死（批 11）。
        //   ★ 没配 `health_uri` 时 `is_healthy()` 恒 true，所以第二条对它是空操作。
        let eligible: Vec<usize> = self
            .upstreams
            .iter()
            .enumerate()
            .filter(|(_, u)| !u.dial_candidates().is_empty() && u.is_healthy())
            .map(|(i, _)| i)
            .collect();
        // ⚠ 一个都不剩就返回 None —— 调用方会回 `defaults.all_upstreams_down`（502）。
        //   ★ 这比「硬着头皮连一个解析不了的地址」好：后者在改之前是**每请求一次 panic**。
        let n = eligible.len();
        if n == 0 {
            return None;
        }
        let slot = match self.policy {
            LbPolicy::RoundRobin => self.cursor.fetch_add(1, Ordering::Relaxed) % n,
            LbPolicy::LeastConn => {
                // ★ 平票时取**下标最小**的那个，不取「第一个碰到的」——
                //   后者依赖迭代顺序，读起来像一样，但一旦并行遍历就不确定了。
                let mut best = 0usize;
                let mut best_v = usize::MAX;
                for (slot, i) in eligible.iter().enumerate() {
                    let v = self.upstreams[*i].inflight();
                    if v < best_v {
                        best_v = v;
                        best = slot;
                    }
                }
                best
            }
            LbPolicy::IpHash => {
                // 取不到客户端 IP 时退回 round_robin：hash 一个恒定值会把所有
                // 匿名请求钉在同一个上游上。
                //
                // ⚠ **一个上游掉出可用集时，哈希映射会整体错位**（同 nginx）。
                //   这是 `ip_hash` 与「可用性筛选」组合的固有性质，不是缺陷——
                //   但要写下来：会话粘性在上游变动时**不保证保持**。
                match client_ip {
                    None => self.cursor.fetch_add(1, Ordering::Relaxed) % n,
                    Some(ip) => {
                        let mut h = RandomStateless::default();
                        ip.hash(&mut h);
                        (h.finish() as usize) % n
                    }
                }
            }
            LbPolicy::Random => {
                // xorshift，种子在构建期取一次。不引 `rand`：这里要的是「大致均匀」，
                // 不是密码学随机，而多一条依赖要按 G29 的整套流程养。
                let c = self.cursor.fetch_add(1, Ordering::Relaxed) as u64;
                let mut x = self.seed ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                x ^= x >> 33;
                x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
                x ^= x >> 33;
                (x as usize) % n
            }
        };
        Some(eligible[slot])
    }

    /// 现在该探这一组上游了吗。**该探就顺手把时刻记下**（返回 `true` 时）。
    ///
    /// ⚠ 「判断」与「记录」合成一个动作是有意的：分开写的话，
    /// 调用方忘了记时刻就变成**每一轮都探**——而那不会有任何报错，
    /// 只会让配了 `30s` 的后端被按打点节奏打，**症状在被探的那台机器上，不在这里**。
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

/// 一个稳定的、无随机化的 hasher —— `ip_hash` 必须**跨进程稳定**，
/// 否则同一个客户端在升级换代之后会被分到另一个上游，粘性就没了。
///
/// ⚠ 用 `std` 的 `RandomState` 会每进程换一个 key，正好破坏这一点。
#[derive(Default)]
struct RandomStateless(u64);

impl Hasher for RandomStateless {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        // FNV-1a：短小、稳定、够均匀。
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

/// 一条头操作的运行时形态。
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
    /// 预留：语法认得，不产生行为。
    Tracing,
    Header(Vec<HeaderOpRt>),
    Rewrite(Template),
    Encode(Vec<String>),
    /// 互斥容器。
    Handle(Vec<ArmRt>),
    /// 保序容器：`steps` 的**数组顺序就是执行顺序**。
    Route(Vec<StepRt>),
    Redir {
        to: Template,
        code: u16,
    },
    Respond {
        status: u16,
        body: Option<Template>,
    },
    /// Prometheus 抓取端点（**M2 批 M**，G116）。**没有任何运行时状态。**
    ///
    /// ★ 指标注册表是**进程级**的（在 `fulcrum-server` 那一侧），不挂在运行时图上 ——
    /// ⚠ 挂上去的话，一次 `POST /load` 就会把计数器整个换掉，
    /// 而 counter 归零在抓取端读起来与「进程重启过」一模一样。
    Metrics,
    Proxy(ProxyTarget),
    /// 自研静态文件服务（M2 批 F）。
    FileServer(FileServerRt),
    /// 自研 HTTP 缓存（M2 批 G）。
    ///
    /// ⚠ ⚠ 它是**中间件**不是终结类 —— 与 `file_server` 架构上不同。
    /// 中间件不设 `outcome`，它只在 [`Routed`] 上记一笔「这条链要缓存」，
    /// 由数据面在拿到终结类结果时决定怎么裹。★ 这与 `encode` / `header`
    /// 记状态是同一条路子，不是新发明的机制。
    Cache(CacheRt),
}

/// `cache` 的运行时形态（M2 批 G）。
///
/// ★ 与 [`FileServerRt`] 一样，这里存的每一样都是**已经算完的最终值** ——
/// 默认值在构图时就并进来，请求路径上不再算第二遍，装载日志打的也是这一份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRt {
    /// **兜底**新鲜度（G96）。`None` = 上游没给新鲜度就不缓存。
    /// ⚠ 它**不覆盖**上游说的 `max-age`/`s-maxage`/`Expires`。
    pub ttl_ms: Option<u64>,
    /// 单条目大小上限。
    pub max_size_bytes: u64,
    /// 整个缓存的容量上限。★ 磁盘后端时它是**磁盘预算**，内存后端时是**内存预算**
    /// —— 同一条指令、两种介质，装载日志会说清这一份配置里它是哪一种。
    pub capacity_bytes: u64,
    /// 磁盘后端的根目录（M2 批 H，G83/G84）。`None` = 内存后端。
    ///
    /// ⚠ ⚠ 它是**进程级**的一件事，却写在**站点级**的指令里 —— 配置层为此
    /// 有一条跨块一致性检查（`FUL-DSL-0035`）。★ 这里仍然逐站点存着，
    /// 因为装载日志要能说出「这个站点写的是哪一个」，而不是只说一个汇总值。
    pub disk_dir: Option<String>,
}

/// `file_server` 的运行时形态（M2 批 F）。
///
/// ★ ★ 这里存的每一样都是**已经算完的最终值**，请求路径上不再算第二遍：
/// `index` 空了就在这里补成 `["index.html"]`，`hide` 在这里就把默认表并进去。
/// 让「默认值是什么」只有一个答案，而那个答案在装载日志里说得出来。
#[derive(Debug, Clone, PartialEq)]
pub struct FileServerRt {
    /// 绝对路径（G91，编译层与这里各拦一道 —— 结构化配置是公开入口）。
    pub root: String,
    pub browse: bool,
    /// 目录命中时按**这个顺序**找索引文件。**不会为空**。
    pub index: Vec<String>,
    /// G87。`false` 时按 canonicalize 校验结果仍在 `root` 之内。
    pub follow_symlinks: bool,
    /// G88 的**最终**清单：默认表 ∪ 用户写的（`hide_defaults false` 时只有用户写的）。
    /// 按**路径段**匹配，命中回 404。已排序去重，装载日志逐字打它。
    pub hide: Vec<String>,
    /// 预压缩旁文件认哪几种编码（**M2 批 I**）。★ 已经是**算完的最终值**。
    pub precompressed: Vec<String>,
}

#[derive(Debug)]
struct ArmRt {
    matcher: Option<MatcherKey>,
    steps: Vec<StepRt>,
}

/// 访问日志往哪写（**M2 批 L 第 ② 步**，G113）。
///
/// ★ ★ 这里只是**数据** —— 真正打开文件是数据面的事，本 crate 一行 I/O 都不做
/// （与「运行时图是纯逻辑、不引用 pingora」同一条纪律）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOutput {
    /// 默认。systemd 下进 journal，别处进终端。
    Stderr,
    /// 追加写到这个**绝对路径**。
    ///
    /// ⚠ 必须绝对，理由与 `cache { disk … }` 那条（G91）逐字相同：相对路径按进程 cwd 解析，
    /// 而 systemd 下 cwd 是 `/`、开发机上是项目目录 ⇒ **同一份配置指向两个地方**。
    File(String),
}

/// 访问日志的级别**阈值**。
///
/// ⚠ 它不是「这一行的级别」——每行的级别按状态码派生（见 [`LogLevel::name_for`]），
/// 这个值只决定**哪些行会被写出来**。两者读者不同，合成一个就没法表达「只记错误」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// 记全部。
    ///
    /// ⚠ **`debug` 与 `info` 在访问日志上等价**，两者都落到这里。
    /// ★ 保留 `debug` 只是因为它本来就在 `log { level … }` 的取值表里（G62）——
    /// **多一个取值不多一种行为**，而把它判成错误会让既有配置装不上。
    All,
    /// 只记 4xx 与 5xx。
    Warn,
    /// 只记 5xx。
    Error,
}

impl LogLevel {
    /// 认一个 `level` 取值。`None` = 认不出来。
    pub fn parse(s: &str) -> Option<LogLevel> {
        match s {
            "debug" | "info" => Some(LogLevel::All),
            "warn" => Some(LogLevel::Warn),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }

    /// 这个状态码该不该被记下来。
    pub fn records(self, status: u16) -> bool {
        match self {
            LogLevel::All => true,
            LogLevel::Warn => status >= 400,
            LogLevel::Error => status >= 500,
        }
    }

    /// **这一行**的级别名 —— 按状态码派生，与阈值无关。
    ///
    /// ★ 它是关联函数而不是方法，正是为了让「它与阈值无关」在类型上看得出来。
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

/// 白名单里的一个头，**已经算成最终形态**（**M2 批 L 第 ③ 步**）。
///
/// ★ ★ 两个字段是**两件事**，而把它们分开正是为了让「查哪个头」与
/// 「日志里那一格叫什么」不会在某一天各自漂走：
/// 前者是 HTTP 的事（大小写不敏感、带连字符），后者是日志契约的事
/// （小写、下划线、带前缀）。⚠ 合成一个的话，规范化规则会被复制到用它的每一处。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPick {
    /// 拿它去 `HeaderMap` 里查 —— **小写**。
    ///
    /// ★ 查找本来就大小写不敏感，存小写是为了让**去重**在这一层就做得掉：
    /// 用户写了 `User-Agent` 又写了 `user-agent` 时，日志里不能出现两个同名键
    /// （那样的 JSON 解析器只会留下一个，而是哪一个没有定义）。
    pub lookup: String,
    /// 日志里那一格的键：`req_hdr_user_agent` / `resp_hdr_content_type`。
    pub key: String,
}

/// 一个站点的访问日志设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRt {
    pub output: LogOutput,
    pub level: LogLevel,
    /// 请求头白名单（**M2 批 L 第 ③ 步**）。★ 空 = **一个头都不记**，那是默认。
    pub req_headers: Vec<HeaderPick>,
    /// 响应头白名单。同上。
    pub resp_headers: Vec<HeaderPick>,
}

impl LogRt {
    /// 把 `log { … }` 那个块翻成运行时设置；认不出来的写法进 `errors`。
    ///
    /// ⚠ `output` 到这里是**已经用空格拼好**的一串（`sub_block` 那么给的）——
    /// `stderr` 或 `file /var/log/x.json`。
    /// ★ 语法层面的检查在 `fulcrum-config` 里（那里报的是带位置的诊断）；
    /// 这里**再判一次**是因为运行时图是公开入口（G11），
    /// **它不能假设喂进来的结构化配置一定经过了 DSL 那条路**。
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

    /// 把一份白名单算成最终形态：**规范化 → 查敏感头 → 去重**。
    ///
    /// ⚠ ⚠ **这一层也要判敏感头**：编译期那道（`FUL-DSL-0036`）只在 DSL 那条路上，
    /// 而运行时图是**公开入口**（G11）—— `POST /load` 收的 JSON 不必经过 `fulcrum compile`。
    /// ★ 它比 `output` 那三条更要紧：那些写错了配置装不上（吵闹），
    /// **这一条写错，凭据静静地进了日志**。
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
            // ⚠ 不是合法 HTTP 头名的东西**永远查不到**，而「查不到」在这份契约里
            //   与「这条请求上没有这个头」长得一模一样 ⇒ 一个拼错的名字会静静地
            //   什么都不记。★ 所以它是错误，不是警告。
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
            // ★ 去重按 `lookup` 算：`User-Agent` 与 `user-agent` 是同一个头，
            //   而两条同名的日志键会让那一行的 JSON 有两个一样的 key。
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
/// ★ 自己写而不是拉一个 `http::HeaderName`：本 crate 是**纯逻辑、不碰网络**，
/// 而这条规则是一张字符表，抄它比多一条依赖便宜。
fn is_header_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// 一个站点的运行时形态。
#[derive(Debug)]
pub struct SiteRt {
    /// 给日志用的名字（第一个地址的原文）。
    pub name: String,
    /// 本站点的全部主机名（含 `*.` 形式），**不含**不带主机名的 `:port` 那种。
    /// ★ TLS 层要用它决定「这张证书该装在哪些 SNI 上」。
    pub hostnames: Vec<String>,
    /// 每条地址的**标签字面量**（G121 · `fulcrum_requests_total{site=...}` 取数用它）：
    /// `host` 非空取其小写（通配保留 `*.` 前缀，与上面 `hostnames` 同一口径），
    /// `host` 为空（`:8080` 这种不带主机名的兜底地址）取 `format!(":{port}")`。
    /// 下标与结构化配置里的 `s.addresses` 一一对应，[`Runtime::resolve_site`]
    /// 返回值的第三项就是这份下标。
    ///
    /// ⚠ ⚠ **它与 [`SiteRt::name`] 是两件不同的事，长得像是巧合**：`name` 取
    /// **第一条**地址的原文（带 scheme 与端口，访问日志的 `site` 字段用它），
    /// 这里取的是**命中的那一条**、只留主机名 —— G121 明文要求不能用第一个地址，
    /// 否则同一块里的两条地址会被混成一格，且改地址书写顺序会让时序断裂。
    /// ★ 用 `Arc<str>` 是为了让每条请求带走它时只是一次引用计数
    /// （与下面 [`SiteRt::log`] 同一条理由，这里连 `String` 的分配都省了）。
    pub addresses: Vec<std::sync::Arc<str>>,
    pub tls: TlsConfig,
    /// 本站点的访问日志设置（**M2 批 L 第 ② 步**）。`None` = 这个站点不记访问日志。
    ///
    /// ⚠ ⚠ 它是**站点级**的，而这带来一个已登记的缺口：`outcome=no_site_match`
    /// 的请求（421，G63）按定义不属于任何站点 ⇒ **它记不进访问日志**。
    /// 那是 §11 的 **D26**，不是遗漏。
    /// ★ 用 `Arc` 是为了让每条请求把它带走时只是一次引用计数 ——
    /// 里面有一个 `String`（文件路径），逐请求克隆它是白花的。
    pub log: Option<std::sync::Arc<LogRt>>,
    matchers: BTreeMap<String, CompiledMatcher>,
    chain: Vec<StepRt>,
    error_handler: Vec<StepRt>,
}

/// 整棵运行时图。**不可变**——换配置就整棵换掉（G8 的全量原子装载）。
#[derive(Debug)]
pub struct Runtime {
    sites: Vec<SiteRt>,
    /// `(host, port) → (站点下标, 命中的地址在该站点 `addresses` 里的下标)`。
    /// ⚠ 后一项是 G121 的全部落点 —— 见 [`SiteRt::addresses`]。
    exact: BTreeMap<(String, u16), (usize, usize)>,
    /// `(".example.com", port, 站点下标, 命中的地址下标)`，**按后缀长度降序**。
    wildcard: Vec<(String, u16, usize, usize)>,
    /// `port → (站点下标, 命中的地址下标)`，来自 `:8080` 这种不带主机名的地址。
    catch_all: BTreeMap<u16, (usize, usize)>,
    pub defaults: Defaults,
    pub l4: Option<L4Config>,
    /// L4 面建好的监听器（M2 批 A：TCP；批 B：UDP）。
    /// ★ 两种协议**都在这里**，由 `proto` 区分 —— 数据面按它决定起哪种服务。
    pub l4_listeners: Vec<L4ListenerRt>,
    /// 要监听的 `(端口, 是否需要 TLS)`。
    pub listen_ports: Vec<(u16, bool)>,
    /// ★ ★ **HTTP 面的 PROXY protocol 信任清单**（**M2 批 D**，全局块的
    /// `proxy_protocol_from`）—— 管这份配置里 HTTP 面的**全部**监听端口。
    ///
    /// ⚠ ⚠ 它与 `L4ListenerRt::proxy_protocol_from` **有意是两份**，
    /// 不是同一份的两个视图：信任 A 对 :443 发 PROXY 头 ≠ 信任 A 对 :5432 也这么干。
    /// ★ 而 HTTP 面这一份必须是**全局**的，因为它是**连接级**的判断 ——
    /// 一条连接上还没有 Host，还不知道会落到哪个站点。
    pub proxy_protocol_from: Vec<Cidr>,
    /// 装载时要说的 warning（现在只有自环那一条，M2 批 G）。
    /// ★ 与 [`Runtime::unwired_in_use`] 同一个形状：**本 crate 返回话，数据面去说**。
    pub self_loop_warnings: Vec<String>,
}

/// L4 面的四层协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4Proto {
    Tcp,
    /// ✅ **M2 批 B接线**：自建 `Service` + 会话表（空闲回收 + 并发上限）。
    /// ⚠ 它与 TCP 有三处**语义上的**差别，都写在 `fulcrum_server::l4` 的 UDP 那一节里，
    /// 其中最要紧的一条是：**停机信号一到就必须停止 `recv_from`**，
    /// 否则老一代会与新一代抢同一个 socket 上的数据报。
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

/// L4 面的一个监听器（M2 批 A）。
///
/// ★ 它**不经过站点索引**：TCP 连接上没有任何七层信息可供路由，
/// L4 的全部语义就是「这个端口进来的字节，原样送到那组上游之一」⇒ 编译期定死。
///
/// ⚠ **端口在启动时绑定**，`POST /load` 换不了。管理面为此把 L4 端口纳入
/// 「端口集变了就 409」—— 少了那一格，改了 `l4` 的配置会**装载成功而毫无效果**。
#[derive(Debug)]
pub struct L4ListenerRt {
    pub proto: L4Proto,
    /// 配置里写的那一串，**原样保留**（诊断与日志要指得回去）。
    pub listen: String,
    /// 监听地址的主机部分。`None` = 配置写的是 `:3306`，
    /// 绑哪个地址由 `serve --bind-host` 决定（与 HTTP 那边同一条口径）。
    pub listen_host: Option<String>,
    pub listen_port: u16,
    /// **兜底**目标：没有任何规则命中时用它。`None` = 没配兜底 ⇒ 关掉连接。
    ///
    /// ★ ★ **与 `reverse_proxy` 用的是同一个类型**，于是「怎么挑上游」只有一份实现
    /// （[`ProxyTarget::pick_by`]），域名上游也自动走同一套解析与重解析机械。
    /// ⚠ 其中 `header_up` / `header_down` / `tls` / `health` 对 L4 **没有意义**，
    /// 一律留空：L4 不看也不改字节，`health_uri` 是一个 HTTP 概念。
    pub target: Option<ProxyTarget>,
    /// SNI / ALPN 分流规则（**M2 批 C**），**按书写顺序**匹配，第一个命中即用。
    ///
    /// ⚠ 非空时数据面要**先看一眼 ClientHello** 才能决定连谁 ——
    /// 那是一段额外的读 + 一次超时预算，所以**空的时候一步都不多走**（批 A 的形状不变）。
    pub rules: Vec<L4RuleRt>,
    /// **收**：信任这些来源发来的 PROXY 头（**M2 批 D**）。
    ///
    /// ★ 空 = **谁都不信**，而这不只是「默认关」：数据面在空的时候
    /// **一个字节都不多读**，与批 A 的形状完全一样。
    pub proxy_protocol_from: Vec<Cidr>,
    /// **发**：给上游发一个 PROXY 头。`None` = 不发（**M2 批 D**）。
    pub proxy_protocol: Option<crate::proxyproto::Version>,
}

impl Runtime {
    /// HTTP 面：这个来源发来的 PROXY 头可信吗？
    ///
    /// ★ ★ 它是**唯一**的判断入口 —— 数据面（含 `pingora-core` 那道接缝）
    /// 不许自己去比网段。⚠ 清单为空时恒 `false`。
    pub fn trusts_proxy_protocol(&self, peer: std::net::IpAddr) -> bool {
        self.proxy_protocol_from.iter().any(|c| c.contains(peer))
    }
}

impl L4ListenerRt {
    /// 这个来源发来的 PROXY 头可信吗？
    ///
    /// ★ ★ 它是**唯一**的判断入口 —— 数据面不许自己去比网段。
    /// ⚠ 清单为空时恒 `false`：一份空清单**不是**「信任所有人」，
    /// 而这正是本条纪律要防的那件事。
    pub fn trusts_proxy_protocol(&self, peer: std::net::IpAddr) -> bool {
        self.proxy_protocol_from.iter().any(|c| c.contains(peer))
    }
}

/// 一条建好的 L4 分流规则。
#[derive(Debug)]
pub struct L4RuleRt {
    pub kind: L4MatchKind,
    /// 要匹配的值。★ SNI 的通配符走 [`fulcrum_config::host::wildcard_covers`] ——
    /// **与站点索引、证书解析同一份实现**（D18 / G66）。⚠ 另写一份「差不多的」匹配，
    /// 就是把 D18 那个洞在第三个地方再挖一次。
    pub values: Vec<String>,
    pub target: ProxyTarget,
}

/// 分流按什么匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4MatchKind {
    /// TLS ClientHello 里的 `server_name`。
    Sni,
    /// TLS ClientHello 里的 ALPN 清单，**任意一个**命中即算命中。
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
    /// 这条规则命中了吗。
    ///
    /// ★ `sni` 与 `alpn` 的匹配语义**有意不同**，两条都写下来：
    /// - SNI：精确，或 `*.example.com` **只吃一层**（G66，共用 `wildcard_covers`）。
    ///   ⚠ 大小写不敏感（DNS 名不区分大小写），所以两边都先转小写。
    /// - ALPN：**逐字节相等**。⚠ 它不是域名，没有通配的道理；
    ///   而 `h2` 与 `h2c` 是两个不同的协议标识，前缀匹配会把它们混起来。
    pub fn matches(&self, sni: Option<&str>, alpn: &[Vec<u8>]) -> bool {
        match self.kind {
            L4MatchKind::Sni => {
                let Some(name) = sni else { return false };
                let name = name.to_ascii_lowercase();
                self.values.iter().any(|v| {
                    let v = v.to_ascii_lowercase();
                    match v.strip_prefix('*') {
                        // `*.example.com` ⇒ 后缀是 `.example.com`，且只吃一层
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

/// 站点解析的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum SiteMatch {
    Exact,
    Wildcard,
    CatchAll,
}

/// 一次路由的结论。
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
    /// 自研静态文件服务（M2 批 F）。数据面按 [`FileServerRt`] 发文件。
    FileServer(&'r FileServerRt),
    /// Prometheus 抓取端点（**M2 批 M**，G116）。数据面渲染当前指标并 200 发出去。
    ///
    /// ⚠ 它让访问日志 `outcome` 那个闭集**多第 8 个值 `metrics`**。
    /// ★ 闭集是靠数据面那个穷尽 `match` 守着的：加这一种就编不过。
    Metrics,
    /// 站点内没有任何路由匹配 → [`Defaults::no_route_match`]。
    NoRouteMatch,
}

/// 路由结果。
pub struct Routed<'r> {
    pub site: &'r SiteRt,
    pub site_match: SiteMatch,
    /// 这次请求**实际匹配到的那条地址字面量**（G121）。
    ///
    /// ⚠ ⚠ 它**不是** `site.name`：`name` 是「站点的名字 = 第一个地址的原文」
    /// （访问日志的 `site` 字段用这个），这里取的是**命中的那一条**——
    /// 同一个站点配多条地址时两者会给出不同的值，那不是巧合，是 G121 明文
    /// 要求「不能用站点块的第一个地址」换来的。
    pub site_addr: std::sync::Arc<str>,
    /// 要施加到**响应**上的头操作，按施加顺序。
    pub response_headers: Vec<&'r HeaderOpRt>,
    /// `rewrite` 之后的路径（含查询串按原样保留）。`None` = 没被改写。
    pub rewritten_path: Option<String>,
    /// `path_regexp` 的捕获组，给 `{path.N}` 用。
    pub captures: Vec<String>,
    /// 终结这次请求的那一步在执行顺序表里的序号（G49）。
    /// ★ 它进结果结构是为了让访问日志能回答「这个请求实际跑到第几步」——
    /// 内建顺序表把书写顺序和执行顺序拆开之后，这是唯一能对上的东西。
    pub terminal_order: Option<u16>,
    /// 这条链上有没有 `cache`（M2 批 G）。
    ///
    /// ★ 它是**中间件**，所以不占 `outcome` —— 数据面看到它才走缓存那条路。
    /// ⚠ 缓存只对 `reverse_proxy` 有意义：`respond` / `redir` 本来就没有上游可省，
    /// 而 `file_server` 的字节已经在本机磁盘上、再存一份内存是净亏。
    /// ⇒ 数据面只在 `Outcome::Proxy` 上用它，别处**装载日志会说出来**。
    pub cache: Option<&'r CacheRt>,
    /// 配置里要求的压缩编码。⚠ **M1 这一批不施加**（见 [`UNWIRED`]）——
    /// 带出来是为了让「要求了但没做」这件事可见，而不是悄悄丢掉。
    pub requested_encodings: Vec<String>,
    pub outcome: Outcome<'r>,
}

/// 一份**可以在运行期整体换掉**的运行时图（G8 的「全量原子 load」那一侧）。
///
/// ★ 不用 `arc-swap`：它不在依赖图里（+1 个包），而写入是「一天几次」量级。
/// `RwLock<Arc<Runtime>>` 的读路径只是一次无竞争读锁 + 一次 `Arc` 克隆。
///
/// ⚠ ⚠ **一个请求必须自始至终看到同一份配置** ⇒ [`SharedRuntime::current`] 只在请求入口
/// 调一次。每阶段各调一次的话，请求中途的一次 load 会让同一个请求按旧配置路由、
/// 按新配置转发 —— 那正是 G8 禁止的「部分生效」。
impl Runtime {
    /// 全部自研 `file_server` 步骤，连同它所在站点的名字。
    ///
    /// ★ 唯一用途是装载日志（G88 的可见性）：非空的默认 hide 清单不说出来就是静默行为。
    /// ⚠ ⚠ 日志必须打**运行时手里这一份**，不能照配置再算一遍 —— 两份各算各的时，
    /// 日志说的和服务器做的可以不是一回事。
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
                    _ => {}
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

    /// 全部 `encode` 步骤要求的算法，连同它所在站点的名字（**M2 批 I**）。
    ///
    /// ★ 与 [`Self::cache_settings`] / [`Self::file_servers`] 同一个形状，
    /// 用途也一样：**装载日志**。⚠ 而这一条比那两条更要紧 ——
    /// `encode` 在 `UNWIRED` 里躺了整整一段，它**刚刚开始真的生效**，
    /// 而一个从旧版本升上来的站点行为就在这一刻变了、配置却一个字没改。
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
                    _ => {}
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

    /// 全部 `cache` 步骤，连同它所在站点的名字（M2 批 G）。
    ///
    /// ★ 与 [`Self::file_servers`] 同一个形状，用途也一样：**装载日志**
    /// （`ttl` 是兜底不是覆盖这件事必须说出来）+ 决定缓存后端的容量与目录。
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
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        for site in &self.sites {
            walk(&site.chain, &mut out);
            walk(&site.error_handler, &mut out);
        }
        // ⚠ ⚠ **这个函数是 DNS 解析与重解析唯一的输入，谁不在里面谁就不存在。**
        //   漏掉一类上游（比如 L4 的）的现场是「那个端口连上就断」，
        //   而配置、日志、健康检查全都正常。
        for l in &self.l4_listeners {
            if let Some(t) = &l.target {
                out.extend(t.upstreams.iter());
            }
            // ⚠ **分流规则里的上游同样是上游**（批 C）。少了这一格，一个域名形式的
            //   `sni` 上游永远解析不出地址 —— 而现场是「那个名字连上就断」，
            //   兜底那条却一切正常，看起来像 SNI 匹配错了。
            for r in &l.rules {
                out.extend(r.target.upstreams.iter());
            }
        }
        out
    }

    /// 这份图里所有的 `reverse_proxy` 目标。健康检查按**目标**走
    /// （`health_*` 是每条 `reverse_proxy` 各自配的），不像 DNS 那样按上游走。
    ///
    /// ⚠ 走法必须与 [`Self::all_upstreams`] 一致（容器里的也要进来）——
    /// 两个走法分家的话，一个藏在 `handle` 里的目标会**永远不被探测**，
    /// 而它看起来只是「一直健康」。
    pub fn all_proxy_targets(&self) -> Vec<&ProxyTarget> {
        fn walk<'a>(steps: &'a [StepRt], out: &mut Vec<&'a ProxyTarget>) {
            for s in steps {
                match &s.body {
                    BodyRt::Proxy(t) => out.push(t),
                    BodyRt::Handle(arms) => {
                        for a in arms {
                            walk(&a.steps, out);
                        }
                    }
                    BodyRt::Route(inner) => walk(inner, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        for site in &self.sites {
            walk(&site.chain, &mut out);
            walk(&site.error_handler, &mut out);
        }
        out
    }
}

/// 一次上游解析的结果，只进日志。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResolveReport {
    /// 这次真去做了 DNS 的域名个数（IP 字面量不算）。
    pub queried: usize,
    /// 这次解析结果**变了**的上游：`(配置里的地址, 新的候选清单)`。
    pub changed: Vec<(String, Vec<std::net::SocketAddr>)>,
    /// 这次解析**失败**的上游：`(配置里的地址, 原因)`。
    pub failed: Vec<(String, String)>,
}

impl ResolveReport {
    pub fn is_quiet(&self) -> bool {
        self.changed.is_empty() && self.failed.is_empty()
    }
}

/// 把一份运行时图里所有**域名**上游解析一遍，把结果写进各自的槽里。
///
/// 它就是 `dns_refresh` 的本体。
///
/// ⚠ **它是阻塞的**，只许在三处调：`serve` 启动时 · 全量 load 生效之前 ·
/// 后台任务里（**要放进 `spawn_blocking`**）。★ **绝不许在请求路径上调**（见 [`Upstream`]）。
///
/// ★ **解析失败不清掉上一次的好结果**：DNS 抖一下就摘掉一个还在正常服务的上游，
/// 等于把一次解析器故障放大成一次服务故障。
pub fn resolve_upstreams(rt: &Runtime) -> ResolveReport {
    use std::net::ToSocketAddrs;
    let mut report = ResolveReport::default();
    for up in rt.all_upstreams() {
        if up.is_literal_ip() {
            continue; // 不需要 DNS，也不会变
        }
        report.queried += 1;
        match up.addr.to_socket_addrs() {
            Ok(it) => {
                // ★ **全部收下**，不是只取第一个。理由见 `Upstream::resolved`。
                let addrs: Vec<std::net::SocketAddr> = it.collect();
                if addrs.is_empty() {
                    // 解析「成功」但一个地址都没有。⚠ 这不是不可能。
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
}

impl SharedRuntime {
    pub fn new(rt: std::sync::Arc<Runtime>) -> std::sync::Arc<SharedRuntime> {
        std::sync::Arc::new(SharedRuntime {
            inner: std::sync::RwLock::new(rt),
        })
    }

    /// 取当前那一份。**每个请求只调一次**，理由见类型文档。
    ///
    /// ⚠ 锁中毒时**回不了「没有配置」**——那会让数据面整个停摆。
    /// 这里的处置是 `unwrap_or_else(|e| e.into_inner())`：中毒说明某个持锁线程 panic 过，
    /// 而那份 `Arc<Runtime>` 本身仍然是完整的（它是不可变的）。
    pub fn current(&self) -> std::sync::Arc<Runtime> {
        match self.inner.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 整体换掉。★ 调用方必须**先把新的建好**再调这里 ——
    /// 建到一半失败时不许留下任何痕迹（G8：配置变更是事务，不是文件写入）。
    pub fn swap(&self, rt: std::sync::Arc<Runtime>) {
        match self.inner.write() {
            Ok(mut g) => *g = rt,
            Err(poisoned) => *poisoned.into_inner() = rt,
        }
    }
}

impl std::fmt::Debug for SharedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedRuntime({} 个站点)", self.current().sites.len())
    }
}

impl Runtime {
    /// 从结构化配置构建。**校验也在这里**——结构化层是公开入口（G11）。
    pub fn build(cfg: &StructuredConfig) -> Result<Runtime, Vec<BuildError>> {
        let mut errors: Vec<BuildError> = Vec::new();
        let mut sites = Vec::new();
        let mut exact = BTreeMap::new();
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

            // ★ ★ 每条地址的标签字面量（G121），下标与 `s.addresses` 一一对应 ——
            //   完整的取舍写在 `SiteRt::addresses` 的类型文档上，这里只管算。
            let mut addresses: Vec<std::sync::Arc<str>> = Vec::new();
            for (ai, a) in s.addresses.iter().enumerate() {
                let label = if a.host.is_empty() {
                    format!(":{}", a.port)
                } else {
                    a.host.to_ascii_lowercase()
                };
                addresses.push(std::sync::Arc::<str>::from(label));

                let needs_tls = a.scheme == "https";
                // ★ ★ 同一个端口不能既有 http:// 又有 https:// 的站点。
                //   ⚠ 这不是洁癖：一个监听 socket 只能说一种协议。写成
                //   `*t |= needs_tls`，于是「端口上有一个 https 站点」会把同端口的
                //   http 站点**静默地也变成 TLS**——那些站点从此对明文请求全部握手失败，
                //   而配置里看不出任何问题。
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
                    // `*.example.com` → 后缀 `.example.com`
                    let suffix = a.host.trim_start_matches('*').to_string();
                    wildcard.push((suffix, a.port, si, ai));
                } else if exact
                    .insert((a.host.to_ascii_lowercase(), a.port), (si, ai))
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

        // ★ 后缀长的排前面：`*.a.example.com` 必须先于 `*.example.com` 被试到，
        //   否则更具体的那条永远轮不到——而它看起来只是「配置没生效」。
        // ★ 长后缀在前。⚠ 「只吃一层」（D18 / G66）之后这次排序**已经是冗余的**：
        //   两个通配后缀不可能同时覆盖同一个 host（`x.a.example.com` 去掉
        //   `.a.example.com` 剩 `x` 中，去掉 `.example.com` 剩 `x.a` 不中）。
        //   留着是为了让遍历顺序稳定可读，**别再把它当成「更具体者优先」的实现**——
        //   那个性质现在由 `wildcard_covers` 自己保证。
        wildcard.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));

        let listen_ports: Vec<(u16, bool)> = listen.into_iter().collect();

        // ★ ★ ★ **一条判据被删掉的时候，它守着的那件事不会跟着消失。**
        //   回落层删除（G98）时，只守回落地址的自环检测挪到了上游那一侧
        //   （`check_self_loop_upstreams`）—— `reverse_proxy 127.0.0.1:443` 是同一个事故。
        let self_loop_warnings = self_loop_warnings(&sites, &listen_ports);

        // ── L4 面（M2 批 A）───────────────────────────────────────────────
        //
        // ★ 与上游、回落**同一条纪律**：地址在**装载期**就要能拆开、能判死。
        //   ⚠ 一个「监听地址写错了、启动时才失败」的 L4 块，症状是
        //   `systemctl reload` 之后那个端口静悄悄地没了 —— 而 HTTP 一切正常，
        //   于是没有人会去看 L4。
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
                // ★ 同一个 `(协议, 主机, 端口)` 写两遍 ⇒ 第二个必然 bind 失败，
                //   而失败发生在启动时、日志里只有一句 EADDRINUSE。装载期说清楚。
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
                // ★ ★ **L4 端口不能与 HTTP 监听端口相撞**：两者在同一个进程里
                //   各自 bind，撞了就是启动失败。⚠ 只比端口号不比地址，理由与
                //   `self_loop_port` 那条一样：同一进程内两个监听器抢同一个端口，
                //   地址再不同也没有意义（`0.0.0.0` 与 `127.0.0.1` 会真的相撞）。
                if proto == L4Proto::Tcp && listen_ports.iter().any(|(p, _)| *p == listen_port) {
                    errors.push(BuildError::new(
                        &at,
                        format!(
                            "端口 {listen_port} 上已经有一个 HTTP 站点在监听——同一个进程里两个监听器抢同一个端口，起不来"
                        ),
                    ));
                    continue;
                }
                // ★ 兜底与每条规则各建一组上游，**用的是同一个函数**（`build_l4_target`）——
                //   于是「上游怎么规范化、IP 字面量什么时候填槽、用哪种 lb_policy」只有一份实现。
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
                    // ⚠ `udp` 上没有 ClientHello。DSL 那一侧已经拦了一次，
                    //   这里再拦一次是因为**结构化配置是公开入口**（G11）：
                    //   机器直接写这一层时不经过 DSL 前端。
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
                // ⚠ 兜底与规则**不能都没有**：那样的监听器接受连接之后只能立刻关掉。
                //   ★ 但「只有规则、没有兜底」是合法的 —— 那是「只服务我认得的那几个名字」。
                if default_target.is_none() && rules.is_empty() {
                    errors.push(BuildError::new(&at, "一个可用的上游都没有"));
                    continue;
                }
                // ★ **网段在这里解析，不在数据面**（本 crate 第 2 条设计约束）：
                //   这一条的线上事故形态尤其坏 —— **信任清单不生效 = 客户端 IP 全都取错**，
                //   而站点照常服务、日志照常有行。
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
                // ⚠ ⚠ 这两条**只在 tcp 上有意义**，而 DSL 前端已经把 udp 上的写法
                //   拦成编译期错误了。这里再拦一次不是重复：**结构化配置是公开入口**
                //   （G11），机器可以直接写它、绕过 DSL —— 与 `sni`/`alpn` 那条
                //   「判据在两层都有」逐字同一条理由。
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

        // ── M2 批 D：HTTP 面的信任清单 ────────────────────────────────────
        //
        // ★ 与 L4 那份走**同一个** `Cidr::parse`，错误也在同一批里报出来。
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

        // ── M2 批 H：缓存后端是**进程级**的，全图只能有一个 ────────────────
        //
        // ⚠ **结构化配置是公开入口**（G11）：`FUL-DSL-0035` 只活在编译层，手写 JSON 绕得过去。
        //   绕过去之后 `serve()` 取**第一个**目录，另一个站点的缓存整个落在别处，
        //   而配置文件、`validate`、装载日志三处都显得正常。
        // ★ 判据走**建起来的那张图**，不照配置再数一遍。
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

    pub fn sites(&self) -> &[SiteRt] {
        &self.sites
    }

    /// M1 认得但这一批没接线的能力，按本次配置**实际用到的**筛过一遍。
    ///
    /// ★ 只报配置里真的写了的那些：全打出来会变成噪音，而噪音会把真的那几条一起埋掉。
    /// ⚠ ⚠ **按结构走，不按 JSON 文本搜** —— 文本搜法一条都认不出（键名在 JSON 里叫
    /// `uri` / `dns_refresh_ms` / `fail_threshold`，而 `"log": null` 又永远在）。
    /// ★ 一个「看起来在筛、实际上筛错了」的清单比没有清单更糟。
    pub fn unwired_in_use(&self, cfg: &StructuredConfig) -> Vec<(&'static str, &'static str)> {
        let mut used: std::collections::BTreeSet<&'static str> = Default::default();
        // ★ **一条判据的寿命常常比写它的人预期的短**，而过期的判据长得和有效的一模一样。
        // ⚠ **全局选项也要看** —— 只扫站点的话，`admin` 这种全局项一行代码没人接
        //   也永远不会被报出来。一张只扫一半的表，在没扫的那一半上与不存在没有区别。
        if cfg.global.admin.is_some() {
            used.insert("admin");
        }
        // HTTP-01 只可能被验在 **80 端口**上（RFC 8555 §8.3：CA 固定连 80）。
        for s in &cfg.sites {
            if s.log.is_some() {
                used.insert("log");
            }
            // ★ TLS 那一格拆成三条，按站点**实际写了什么**判，不按「端口上有 TLS」判。
            //   ⚠ 取后者的话，一个明明给了 PEM 路径（已接线）的站点也会被报成未接线。
            let needs_tls = s.addresses.iter().any(|a| a.scheme == "https");
            if needs_tls {
                match &s.tls.mode {
                    TlsMode::Automatic => {
                        // ⚠ **假警报会训练人忽略这张表**，连带把真的那几条一起埋掉。
                        //   `TlsMode::Automatic` 这一支现在一条都不报（自动签发全接线了）；
                        //   ★ 留一个空分支而不是删掉 `match`，是让下一个人看见
                        //   这里的判定是被做完了、不是被忘了。
                    }
                    TlsMode::Internal => {
                        used.insert("tls_internal");
                    }
                    // `tls <cert> <key>` 与 `http://` 都已接线
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
                    StepBody::ReverseProxy { passive, .. } => {
                        // ★ 批 11：`health_uri` 接上了，这一条删掉。
                        if passive.fail_threshold.is_some() {
                            used.insert("passive_fail");
                        }
                        // ★ 批 10：这里原先无条件报一条 `dns_refresh`
                        //   （它有默认值 30s，等于对每条 reverse_proxy 都承诺了定期重解析）。
                        //   现在它**接上了**，所以这一条删掉 —— 留着就是一条假警告，
                        //   而假警告会训练人忽略整张表。
                    }
                    StepBody::Route { steps } => stack.extend(steps.iter()),
                    StepBody::Handle { arms } => {
                        for a in arms {
                            stack.extend(a.steps.iter());
                        }
                    }
                    _ => {}
                }
            }
        }
        UNWIRED
            .iter()
            .filter(|(k, _)| used.contains(k))
            .copied()
            .collect()
    }

    /// 按 `(Host, 本地端口)` 找站点。
    ///
    /// 顺序：精确 → 通配（长后缀优先）→ 端口兜底。都不中返回 `None`
    /// → 调用方回 [`Defaults::no_site_match`]（**421**，G63）。
    ///
    /// ★ 返回值的第三项是**命中的是该站点第几条地址**（下标进 [`SiteRt::addresses`]）——
    /// G121 要指标标签取「实际匹配到的那条地址字面量」而不是站点的第一条地址，
    /// 这个下标是那条字面量的唯一来源。
    pub fn resolve_site(&self, host: &str, port: u16) -> Option<(usize, SiteMatch, usize)> {
        let h = host.to_ascii_lowercase();
        if let Some(&(i, ai)) = self.exact.get(&(h.clone(), port)) {
            return Some((i, SiteMatch::Exact, ai));
        }
        for (suffix, p, i, ai) in &self.wildcard {
            // ★ ★ **只吃一层**（D18 / G66）：`*.example.com` 覆盖 `a.example.com`，
            //   **不**覆盖 `example.com` 自己，也**不**覆盖 `a.b.example.com`。
            //   ⚠ 这里原先是 `ends_with` 的后缀匹配，而证书那侧按 RFC 6125 只吃一层，
            //   于是 `a.b.example.com` **被路由到通配站点、然后拿不到证书**——
            //   现场是一次握手失败，配置里没有一行看得出问题。
            //   ★ 判据只有一份，就在 `fulcrum_config::host`：两边都调它，
            //   **分家变成结构上做不到的事**，而不是靠一条契约测试碰巧发现。
            if *p == port && wildcard_covers(suffix, &h) {
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

    /// `handle_errors` 给的错误页。`None` = 用内置默认（G63）。
    ///
    /// ⚠ M1 的最小形态：只认 `handle_errors { respond <status> [body] }`。
    /// 更复杂的错误页（走 reverse_proxy / file_server）是后面的事——
    /// 而这里返回 `None` 时调用方会落到内置默认，**不会静静地什么都不做**。
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

/// `handle_errors` 里那条 `respond`。
pub struct ErrorPage<'r> {
    pub status: u16,
    pub body: Option<&'r Template>,
}

/// 一次路由的可变状态。
struct Walk<'r, 'q> {
    site: &'r SiteRt,
    /// 请求的原始视图。`path` 字段不直接用——用 [`Self::ctx`]。
    base: RequestCtx<'q>,
    /// 当前路径（可能已被 `rewrite` 改过）。
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
    /// 当前的请求视图。★ `rewrite` 之后**后续匹配器看到的是新路径**——
    /// 这是 Caddy 的语义，也是唯一自洽的一种：否则 `rewrite` 就成了只影响上游、
    /// 不影响本地路由的半吊子操作。
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
                // 构建期已经保证引用得到（配置层也查过一遍）；取不到时**判不命中**。
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

    /// 走一段链。**数组顺序就是执行顺序**——站点顶层那份在编译期已按顺序表排好，
    /// `route { … }` 里那份是书写顺序（G49 的逃生口）。返回 `true` 表示已经终结。
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
                // ⚠ 记下来但不施加，见 UNWIRED。带出去比悄悄丢掉好。
                BodyRt::Encode(list) => self.encodings = list.clone(),
                // ★ 中间件：只记一笔，**不终结**。
                //   ⚠ 一条链上写两个 `cache` 时后一个生效 —— 与 `encode` 同款，
                //   而顺序表保证它们在同一位置，所以这只可能来自 `route { … }`。
                BodyRt::Cache(c) => self.cache = Some(c),
                BodyRt::Handle(arms) => {
                    // ★ 互斥：只有**第一个**匹配的分支会执行。
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
                    // ★ 保序容器：块内按书写顺序。
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
    // ★ 结构化层是公开入口：引用了不存在的匹配器要在**这里**也被拒。
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
            // ⚠ ⚠ 结构化配置是**公开入口**（G11）：DSL 那侧的「必须绝对路径」
            //   在这里必须再有一道，否则一份手写 JSON 就能绕过去 ——
            //   而绕过去之后的现场是「缓存目录建在了 /proc/self/cwd 底下」，
            //   ⚠ **一个字的报错都不会有**。★ 与 `file_server` 的 root 同款。
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
                // ★ 默认值在**这一处**算完，之后谁都不必再想它是什么
                //   （与 `FileServerRt` 的 index/hide 同一条纪律）。
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
            // ⚠ ⚠ 结构化配置是**公开入口**（G11）：DSL 那侧的两道检查
            // （root 必填、必须绝对）在这里必须各再有一道，否则一份 JSON
            // 就能绕过去，而绕过去之后的现场只是「怎么全是 404」。
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
                // ★ 去重并按**配置里的书写顺序**保留：挑旁文件时走的是客户端的
                //   `Accept-Encoding` 顺序，所以这里的顺序不参与决策 ——
                //   ⚠ 但重复项会让 `any()` 白跑几次，而且装载日志里会印两遍。
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
            upstreams,
            lb_policy,
            health,
            header_up,
            header_down,
            transport,
            tls_insecure_skip_verify,
            ..
        } => {
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
            for u in upstreams {
                // ★ 上游地址必须**在装载时**就能被解析成「主机 + 端口」。
                //   `10.0.0.1:8080` / `backend:80`；缺端口按 transport 补默认。
                match normalize_upstream(u, transport) {
                    Ok(addr) => {
                        // ★ IP 字面量当场填好：它不需要 DNS，也永远不会变。
                        //   ⚠ **域名一律留空** —— `Runtime::build` 是 `fulcrum validate`
                        //   走的那条路，而 validate 的全部价值在于**离线**就能说话。
                        //   在这里做 DNS 会让它变成一个要联网的命令。
                        let literal: Vec<std::net::SocketAddr> =
                            addr.parse::<std::net::SocketAddr>().into_iter().collect();
                        ups.push(Upstream {
                            addr,
                            resolved: std::sync::RwLock::new(literal),
                            inflight: AtomicUsize::new(0),
                            // ★ 初值健康。理由见 `Upstream::healthy` 上那张表。
                            healthy: std::sync::atomic::AtomicBool::new(true),
                        })
                    }
                    Err(m) => errors.push(BuildError::new(at, format!("上游 `{u}`：{m}"))),
                }
            }
            // ★ 只有写了 `health_uri` 才有健康检查。其余 `health_*` 都有默认值，
            //   所以「配了 health_interval 却没配 health_uri」是**不探测**，
            //   而不是「按默认路径探」——后者会去打一个用户从没说过的路径。
            let health = health.uri.as_ref().and_then(|uri| {
                match StatusPattern::parse(&health.status) {
                    Some(status) => Some(HealthPolicy {
                        uri: uri.clone(),
                        interval: std::time::Duration::from_millis(health.interval_ms),
                        timeout: std::time::Duration::from_millis(health.timeout_ms),
                        status,
                    }),
                    None => {
                        // ⚠ 这里**必须报错而不是回落成默认的 `2xx`**：
                        //   一个「看起来在按 5xx 判、其实在按 2xx 判」的健康检查
                        //   会把所有上游一起判死，而配置里一个字都没错。
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
                upstreams: ups,
                policy,
                header_up: header_up.iter().map(HeaderOpRt::build).collect(),
                header_down: header_down.iter().map(HeaderOpRt::build).collect(),
                tls: transport == "https",
                tls_insecure_skip_verify: *tls_insecure_skip_verify,
                health,
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

/// 逐个站点检查 `reverse_proxy` 的上游有没有指回枢衡自己（**M2 批 G 从回落层挪过来**）。
///
/// 自环的现场：请求打回自己 → 再转发 → 再打回自己，直到 fd 或内存耗尽，
/// 而日志里只有源源不断的**正常**转发记录。
///
/// ⚠ **返回话、不自己打印**（本 crate 有意不依赖 `log`）⇒ 由数据面的 `log_load_summary` 去说。
/// ⚠ **是 warning 不是 error**：指回自己的 `reverse_proxy` 可能是有意的
/// （第二趟落到另一个站点上而终止）⇒ 说出来，但不替用户拒绝。
/// 走遍一批站点，把全部 `cache` 步骤连同站点名收出来。
///
/// ⚠ 是**自由函数**而不是 `Runtime` 的方法：那条「缓存后端全图只能有一个」的检查
/// 必须在 `Runtime` 还没建出来时跑。抄一份 walk 过去，就等于让「检查过的那张图」
/// 与「跑起来的那张图」有机会不是同一件事。
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
                _ => {}
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
                _ => {}
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

/// 把上游写法归一成 `host:port`。
/// 这个已规范化的地址是不是指回枢衡自己？是的话返回那个端口。
///
/// ★ **判据要窄，宁可漏报也不许误报**（误报会让一份合法配置起不来，漏报只是一个
/// 能被日志看见的循环）⇒ 两条同时成立才判：① 主机是本机形式；② 端口在本进程的监听集里。
/// ⚠ 第 ② 条单独不成立 —— 别的机器可以用同一个端口号。
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

/// 给一组 L4 上游建一个 [`ProxyTarget`]。一个都建不出来时回 `None`。
///
/// ★ 兜底与每条 `sni` / `alpn` 规则**共用它** —— 于是「上游怎么规范化、
/// IP 字面量什么时候填进槽、用哪种 `lb_policy`」这几件事只有一份实现。
/// ⚠ 它有意收 `&mut Vec<BuildError>` 而不是自己返回错误：装载期的判据是**一次报全**
/// （与 G51 那条诊断纪律同源），中途 `?` 出去只会报第一条。
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
                // ★ IP 字面量当场填好：它不需要 DNS，也永远不会变。
                //   ⚠ 域名一律留空 —— `fulcrum validate` 必须离线就能说话。
                let literal: Vec<std::net::SocketAddr> =
                    addr.parse::<std::net::SocketAddr>().into_iter().collect();
                ups.push(Upstream {
                    addr,
                    resolved: std::sync::RwLock::new(literal),
                    inflight: AtomicUsize::new(0),
                    healthy: std::sync::atomic::AtomicBool::new(true),
                });
            }
            Err(m) => errors.push(BuildError::new(at, format!("{what}上游 `{u}`：{m}"))),
        }
    }
    if ups.is_empty() {
        return None;
    }
    Some(ProxyTarget {
        upstreams: ups,
        // ★ M2 只给轮询：DSL 的 `l4` 块里**没有 `lb_policy` 的位置**（dsl-reference §4.5），
        //   而给一个用户写不出来的旋钮留接口，等于假装它可配。
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

/// 拆 `l4` 的监听地址：`:3306` 或 `127.0.0.1:3306`。
///
/// 返回 `(主机部分, 端口)`；主机为 `None` 表示写的是 `:3306`，
/// 绑哪个地址交给 `serve --bind-host`（与 HTTP 那边同一条口径）。
///
/// ⚠ **端口必须写**：L4 上没有「默认端口」这回事 —— HTTP 那边缺端口能补 80/443，
/// 是因为 scheme 说了算，而一个裸 TCP 监听器没有 scheme。
pub fn parse_l4_listen(raw: &str) -> Result<(Option<String>, u16), &'static str> {
    if raw.is_empty() {
        return Err("是空的");
    }
    if raw.contains('/') {
        return Err("不能带路径或 scheme——写成 `:3306` 或 `127.0.0.1:3306`");
    }
    // ★ 与上游那把尺子保持一致：IPv6 字面量整体还没支持，
    //   在这里明确说出来，好过让它在 `rsplit_once(':')` 上被拆得面目全非。
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

/// 规范化一条 L4 上游。**端口必须写明**。
///
/// ★ 形状那一半直接借 [`normalize_upstream`]，于是错误提示与 `reverse_proxy`
/// 逐字一致；这里只多加「不许缺端口」这一条。
/// ⚠ 借的时候 `transport` 传什么都行 —— 它**只**在「缺端口要补默认值」那一支用得上，
/// 而那一支在这里已经被上面挡掉了。传 `tcp` 是为了让读代码的人不必去确认这件事。
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
