//! 结构化配置 —— **唯一内部事实**（G11），格式是 **JSON**（G48）。
//!
//! ★ 两个入口都合法：人写 DSL，机器直接写这一层。**DSL 是它的一个前端，不是它的替代。**
//! diff、版本化、原子回滚全部建在这一层上，所以：
//!
//! - ★ ★ **`None` 照样序列化成 `null`，不用 `skip_serializing_if`。**
//!   键集合稳定，两个版本的 diff 才只反映真实变化；否则「把某项从有改成无」
//!   会表现为**删掉一行**，与「这个版本的结构变了」在 diff 里长得一模一样。
//! - **不做反向生成**（结构化 → DSL）：注释、缩进、简写在往返中必然丢失。
//!
//! ⚠ 字段会随自研进度增加，
//! **稳定性承诺是 D9（M4）** —— 现在还没有承诺，`schema_version` 先占住位置。

use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 当前结构化配置的版本号。改动字段含义时要动它。
pub const SCHEMA_VERSION: u32 = 1;

/// 序列化成人读的 JSON。
///
/// ★ 放在这里而不是让调用方自己 `serde_json::to_string_pretty`：
/// 「结构化那份是唯一内部事实」意味着**它长什么样是本层的责任**。
/// 由调用方各写各的，缩进与键序迟早会分叉，而 diff 与原子回滚全建在这份文本上。
pub fn to_pretty_json(cfg: &StructuredConfig) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(cfg)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredConfig {
    pub schema_version: u32,
    pub global: Global,
    pub defaults: Defaults,
    pub sites: Vec<SiteConfig>,
    pub l4: Option<L4Config>,
}

// ⚠ **回落层已整层删除**（G98）：它的三个用户（`l4` / `file_server` / `cache`）
// 逐块改成自研之后一个不剩。代价写在明处：**写了 `fallback_nginx` /
// `fallback_caddy` 的配置会编译不过** —— 而那两条不是被静默丢弃的，
// `compile.rs` 里有一条专门的诊断告诉人它去哪了。

/// 默认响应（**G63**）。★ **默认值本身就是一份契约**，所以它进结构化配置，
/// 不只活在代码里——机器写这一层的时候，看得见自己继承了什么。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    /// 无站点匹配 → **421 Misdirected Request**。
    ///
    /// ★ 不静默交给某个站点：后者是 nginx `default_server` 那类行为的温床——
    /// 请求被交给一个用户没打算让它去的地方，且没有任何提示。
    pub no_site_match: u16,
    /// 站点内无路由匹配 → 404。
    pub no_route_match: u16,
    /// 上游全部不健康 → 502。
    pub all_upstreams_down: u16,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            no_site_match: 421,
            no_route_match: 404,
            all_upstreams_down: 502,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Global {
    pub acme_email: Option<String>,
    pub acme_ca: Option<String>,
    pub admin: Option<String>,
    pub default_sni: Option<String>,
    pub grace_period_ms: Option<u64>,
    /// 自动把 HTTP 重定向到 HTTPS（G12 的后半句）。**默认 true**。
    ///
    /// ⚠ ⚠ `#[serde(default = "yes")]` 不是可有可无的：结构化配置是**公开入口**（G11），
    /// 机器直接写它。一份**没有这个字段**的旧 JSON 灌进来时，
    /// 默认必须是 `true` —— 否则「升级一次枢衡，站点的 HTTP 跳转就没了」，
    /// 而配置一个字都没改过。
    #[serde(default = "yes")]
    pub auto_http_redirect: bool,
    /// ★ ★ **PROXY protocol 的信任清单，管的是 HTTP 面的全部监听端口**（**M2 批 D**）。
    ///
    /// 空 = **谁发来的 PROXY 头都不看**（默认，也是 owner 拍板的口径）。
    ///
    /// ⚠ ⚠ **「不在清单里就当没有这个头」在这里有一个字面的落法：一个字节都不读。**
    /// 与 XFF 那条（[安全基线]）不同的是，XFF 是 HTTP 头、忽略它无害；
    /// 而 PROXY 头是**连接开头的字节**，「读掉丢弃」与「一个字节都不读」
    /// 是**给上游两条完全不同的流**。取后者的理由写在
    /// `fulcrum_runtime::proxyproto` 与 §10：
    /// v2 头自带长度字段，「读掉丢弃」必须先解析攻击者控制的那两个字节才知道丢多少。
    ///
    /// ★ 它与 `l4` 里那份清单**有意分开**：信任 A 发 PROXY 头 ≠ 信任 A 对另一个端口
    /// 也这么干，更 ≠ 信任 A 写的 XFF（那是请求级的，这是连接级的）。
    #[serde(default)]
    pub proxy_protocol_from: Vec<String>,
}

/// `serde(default)` 用的 `true`。★ 单独一个函数是因为 serde 的 default 只能给函数名。
fn yes() -> bool {
    true
}

/// `proxy_protocol` 不带参数时写哪个版本（**owner 拍板：v2**，M2 批 D）。
///
/// ★ ★ **它住在这里，而 `fulcrum_runtime::proxyproto::Version::default()` 引用它** ——
/// 不是两处各写一个 `"v2"` 再拿一条契约测试去钉。⚠ 两份「一开始总是一致的」值，
/// 会在**下一次改动**时分家，而这一条分家的现场表现是
/// 「DSL 里省略参数与显式写 v2 行为不同」—— 没有任何一道门会说出来。
/// ★ 这与批 C 复用 `wildcard_covers`（G66）是同一条纪律：**让分家在结构上做不到**。
pub const DEFAULT_PROXY_PROTOCOL: &str = "v2";

/// `proxy_protocol` 认得的版本。
///
/// ★ ★ 它有**两个**使用者：`reverse_proxy` 的子指令表（走 `ArgType::Enum`）
/// 与 `l4` 块里那段手写的校验（`l4` 的子指令不走子指令表）。
/// ⚠ 让它们各写一份 `["v1", "v2"]` 的代价很具体：**将来加 v3 时只改一处**，
/// 而两处的现场表现是「同一个词在 HTTP 面能写、在 L4 面报错」。
pub const PROXY_PROTOCOL_VERSIONS: &[&str] = &["v1", "v2"];

/// ⚠ ⚠ **`Default` 必须手写，不能 `derive`。**
///
/// `derive(Default)` 给 `bool` 的是 **`false`** —— 而这个字段的默认值是 **`true`**。
/// 一旦 derive，一份**没写全局块**的配置（最常见的那种）就会悄悄关掉 HTTP 跳转，
/// 而 DSL 参考 §二印着「自动把 HTTP 重定向过来」。
/// ★ 这正是本仓库那条「默认值本身就是契约」（§八 / G63）的又一处落点。
impl Default for Global {
    fn default() -> Self {
        Global {
            acme_email: None,
            acme_ca: None,
            admin: None,
            default_sni: None,
            grace_period_ms: None,
            auto_http_redirect: true,
            // ★ 默认**空**：不信任任何人发来的 PROXY 头。
            //   ⚠ 这与 `auto_http_redirect` 那条相反 —— 那条的默认是 `true`
            //   （关掉它是「配置一个字没改，站点的跳转没了」）；
            //   而这条的默认必须是「谁都不信」，因为一个默认信任某个网段的代理服务器，
            //   等于让那个网段里的任何人自称自己是任何 IP。
            proxy_protocol_from: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteConfig {
    pub addresses: Vec<Address>,
    pub matchers: BTreeMap<String, Matcher>,
    pub tls: TlsConfig,
    pub log: Option<LogConfig>,
    /// 已经按执行顺序表排好的执行链（G49）。
    pub chain: Vec<Step>,
    /// `handle_errors { … }` 的内容。空 = 用 [`Defaults`]。
    pub error_handler: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Address {
    /// DSL 里写的原文。
    pub raw: String,
    pub scheme: String,
    /// 空串 = 任意 Host。
    pub host: String,
    pub port: u16,
    /// `*.example.com`。★ 需要 DNS-01（G54/G57），M1 即可用（G58）。
    pub wildcard: bool,
    /// 是否自动签证书并把 HTTP 重定向过来（G12）。`http://` 前缀会关掉它。
    pub auto_https: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Matcher {
    /// ★ 同一块内**多条件是 AND**；同一条件写多个值是 OR。
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub kind: String,
    pub values: Vec<String>,
    /// `not { … }` 包着的。
    pub negate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum TlsMode {
    /// 自动 HTTPS（默认）。
    Automatic,
    /// `tls internal` —— 自签。
    Internal,
    /// `tls <cert> <key>` —— 用户自带证书。
    Manual { cert: String, key: String },
    /// `http://` 地址：不签、不升级。
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TlsConfig {
    #[serde(flatten)]
    pub mode: TlsMode,
    pub on_demand: bool,
    /// On-Demand 的准入端点。★ **G15：没配它就拒绝启动**，
    /// 所以这里是 `Option` 只是为了表达「DSL 里没写」，不是「可以不写」。
    pub ask: Option<String>,
    /// DNS-01 用哪家（G57：原生 `cloudflare` / `dnspod`，其余走 exec hook）。
    pub dns_provider: Option<String>,
    /// `dns exec <程序>` 里那个程序路径。★ 只有 `exec` 用得上。
    #[serde(default)]
    /// `dns` 的第二个参数：exec 的程序路径，或原生供应商的凭据。
    ///
    /// ⚠ ⚠ 此后它是 [`Secret`]：**字面量凭据现在写得下**（owner 拍板，
    /// Caddy 形状），而代价是这份配置本身变成了秘密 —— 所以默认序列化、
    /// `Debug`、`Display` 一律脱敏，露真值要显式进 `secret::reveal` 作用域。
    /// ★ `dns exec /path/to/hook` 那种也走这个类型，但路径不是秘密（无前缀 ⇒ 字面量
    /// ⇒ 会被脱敏）—— ⚠ 这是有意的取舍：**一条路径也可能泄露信息**，
    /// 而它作为「线索」的价值远低于凭据泄露的代价；要看它就 `--with-secrets`。
    pub dns_arg: Option<Secret>,
    /// 校验 TXT 可见性时问哪些权威 NS（`host:port`）。
    ///
    /// ★ ★ **G58 要求它必须有**：确认 TXT 可见只能靠真去问，
    /// **绝不能只 sleep 一个固定秒数**。所以配了 `dns` 却没配这个是**编译期错误**，
    /// 形状照 G15（`on_demand` 没配 `ask` 就拒绝启动）。
    #[serde(default)]
    pub resolvers: Vec<String>,
    /// 这份 DNS 凭据**被声明**覆盖哪些 zone（**G59 第 3 条**）。
    ///
    /// ★ ★ 它不是「凭据实际能干什么」——DNSPod 的 token 是账号级的，
    /// 实际权限就是账号下的全部域名。声明的价值在于：**越权那一刻是我们自己拒绝的，
    /// 而且拒绝的理由在配置里看得见**。
    /// ⚠ 原生供应商（`cloudflare` / `dnspod`）没写它是**编译期错误**，形状照 G15。
    #[serde(default)]
    pub zones: Vec<String>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            mode: TlsMode::Automatic,
            on_demand: false,
            ask: None,
            dns_provider: None,
            dns_arg: None,
            resolvers: Vec::new(),
            zones: Vec::new(),
        }
    }
}

/// ✅ 字段清单与格式已由 **G113 + G114** 定稿（D7 结案）。
/// 权威是 [`docs/architecture/observability.md`](../../../docs/architecture/observability.md)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogConfig {
    pub output: Option<String>,
    pub level: Option<String>,
    /// 请求头白名单（**M2 批 L 第 ③ 步**）。★ 默认**一个头都不记**。
    ///
    /// ⚠ 这里存的是**用户写的原文**，规范化（小写、`-` 换 `_`、加前缀）在运行时图那一层做
    /// —— 结构化配置是「配置的样子」，不是「日志的样子」。
    #[serde(default)]
    pub headers: Vec<String>,
    /// 响应头白名单（**M2 批 L 第 ③ 步**）。同上。
    #[serde(default)]
    pub resp_headers: Vec<String>,
}

/// 执行链上的一步。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// 执行顺序表里的序号（G49）。★ 它进结构化配置，是为了让「它实际跑在第几步」
    /// 成为**可被读到的事实**，而不是只能靠背那张表。
    pub order: u16,
    pub matcher: Option<MatcherRef>,
    #[serde(flatten)]
    pub body: StepBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatcherRef {
    /// `@name`
    Named(String),
    /// 行内简写，**只能是路径**（G50）。
    Path(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "directive")]
pub enum StepBody {
    /// 预留：M1 语法上认得，不产生任何行为。
    Tracing,
    Header {
        ops: Vec<HeaderOp>,
    },
    Rewrite {
        to: String,
    },
    Encode {
        encodings: Vec<String>,
    },
    Cache {
        /// **兜底**新鲜度（G96）：只有上游没给 `max-age`/`s-maxage`/`Expires` 时才用它。
        /// `None` = 上游没说就**不缓存**。
        ttl_ms: Option<u64>,
        /// 单条目大小上限。`None` ⇒ [`fulcrum_config::directive::CACHE_DEFAULT_MAX_SIZE_BYTES`]。
        max_size_bytes: Option<u64>,
        /// 整个缓存的容量上限，达到就按 LRU 淘汰。
        /// `None` ⇒ [`fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES`]。
        #[serde(default)]
        capacity_bytes: Option<u64>,
        /// 磁盘后端的根目录（M2 批 H）。`None` ⇒ 内存后端。
        ///
        /// ⚠ 它是**进程级**的：多个 `cache` 块必须写同一个值（编译期检查）。
        #[serde(default)]
        disk_dir: Option<String>,
    },
    /// 互斥容器：多个 `handle` 里只有第一个匹配的会执行。
    Handle {
        arms: Vec<HandleArm>,
    },
    /// ★ 保序容器（G49 的逃生口）：块内**按书写顺序**执行。
    ///
    /// ⚠ 读这份 JSON 的人注意：`steps` 里每一步仍然带着自己的 `order`（那是它在
    /// 顺序表里的位置，是指令的固有属性），**但数组顺序才是执行顺序**。
    /// 对 `steps` 按 `order` 排序会把逃生口的语义抹掉。
    Route {
        steps: Vec<Step>,
    },
    Redir {
        to: String,
        code: u16,
    },
    Respond {
        status: u16,
        body: Option<String>,
    },
    /// Prometheus 抓取端点（**M2 批 M**，G116）。**一个字段都没有。**
    ///
    /// ★ 它是**终结类**（序号 75），不是站点级属性也不是独立监听器：
    /// 访问控制、TLS、访问日志、压缩因此全部复用现有机制，
    /// 而 G14「管理面只绑 Unix socket」的口径一个字不动 —— 指标面不属于管理面。
    ///
    /// ⚠ 结构化配置是**公开入口**（G11），所以这个无字段变体的 JSON 形状也是契约：
    /// 内部标签制 ⇒ 它序列化成 `{"order":75,"matcher":null,"directive":"metrics"}`,
    /// **没有第二个键**。★ 与 `Tracing` 同款，不是新写法。
    Metrics,
    ReverseProxy {
        upstreams: Vec<String>,
        lb_policy: String,
        health: HealthCheck,
        /// ★ 上游域名定期重解析（G17）。**这条直接消灭 nginx OSS 那个经典事故源**。
        dns_refresh_ms: u64,
        passive: Passive,
        header_up: Vec<HeaderOp>,
        header_down: Vec<HeaderOp>,
        transport: String,
        tls_insecure_skip_verify: bool,
        /// **发**给上游一个 PROXY 头，`"v1"` / `"v2"`。`None` = 不发（**M2 批 D**）。
        ///
        /// ★ 它与 `header_up X-Forwarded-For` 解决的是同一件事，但**层不同**：
        /// XFF 是 HTTP 头（上游要会读它、且要信任我们），PROXY 头在**连接开头**，
        /// 上游把它当成 socket 层的事实 —— ⇒ 对**不解析 HTTP 的上游**（数据库、
        /// 邮件、自己也是 L4 代理的那种）它是唯一可用的那个。
        #[serde(default)]
        proxy_protocol: Option<String>,
    },
    FileServer {
        /// ⚠ **M2 批 F 起必填、且必须是绝对路径**（G91）。类型仍是 `Option`，
        /// 是因为编译出错时也要能把这一步造出来接着往下查——`None` 只出现在
        /// 已经报过 `MISSING_REQUIRED_SUB` 的那条路上，运行时看不到它。
        root: Option<String>,
        browse: bool,
        index: Vec<String>,
        /// G87。缺省 **true**（跟随），与 nginx `disable_symlinks off`、Caddy 一致。
        ///
        /// ⚠ ⚠ 结构化配置是**公开入口**（G11），默认是 `true`
        /// ⇒ 必须 `#[serde(default = "yes")]`，裸 `#[serde(default)]` 会给出 `false`，
        /// 那等于**从 JSON 进来的配置默默换了一种行为**。
        #[serde(default = "yes")]
        follow_symlinks: bool,
        /// G88。用户写的那几段，**追加**在默认表之后（不是替换）。
        #[serde(default)]
        hide: Vec<String>,
        /// G88。缺省 **true**（默认表生效）。写 `false` 关掉默认表。
        ///
        /// ★ 它把「追加」的代价补掉了：**一个关不掉的默认就是一条隐藏规则**。
        #[serde(default = "yes")]
        hide_defaults: bool,
        /// 预压缩旁文件认哪几种编码（**M2 批 I**，G99）。空 = 不找旁文件。
        ///
        /// ★ 与 `encode` 用同一张取值表（gzip / zstd / br），但两者是**独立**的：
        /// 只配 `precompressed` = 只发已经压好的旁文件、从不现压；
        /// 只配 `encode` = 全部现压；两个都配 = 有旁文件用旁文件、没有才现压。
        #[serde(default)]
        precompressed: Vec<String>,
    },
}

impl StepBody {
    pub fn directive_name(&self) -> &'static str {
        match self {
            StepBody::Tracing => "tracing",
            StepBody::Header { .. } => "header",
            StepBody::Rewrite { .. } => "rewrite",
            StepBody::Encode { .. } => "encode",
            StepBody::Cache { .. } => "cache",
            StepBody::Handle { .. } => "handle",
            StepBody::Route { .. } => "route",
            StepBody::Redir { .. } => "redir",
            StepBody::Respond { .. } => "respond",
            StepBody::Metrics => "metrics",
            StepBody::ReverseProxy { .. } => "reverse_proxy",
            StepBody::FileServer { .. } => "file_server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandleArm {
    pub matcher: Option<MatcherRef>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderOp {
    /// `set` / `add` / `remove`
    pub op: String,
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub uri: Option<String>,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub status: String,
}

impl Default for HealthCheck {
    fn default() -> Self {
        Self {
            uri: None,
            interval_ms: 10_000,
            timeout_ms: 3_000,
            status: "2xx".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Passive {
    pub fail_threshold: Option<u32>,
    pub window_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L4Config {
    pub listeners: Vec<L4Listener>,
    // ★ ★ ★ **`fallback` 这一格在被整条删掉了。**
    //
    //   M1（G60）：整个 L4 面编译成「交给 Caddy」的回落标记；
    //   批 A：TCP 自研 ⇒ 它变成 `Option`，只有 `udp` 还标回落；
    //   批 B：UDP 也自研 ⇒ **没有任何一条 L4 配置需要回落了**，这一格恒为 `None`。
    //
    //   ⚠ 留一个恒为 `None` 的字段不是「保守」，它是**一句会被当真的假话**：
    //   `plan` 与 `compile` 的产物是公开契约（G11/G48），读的人会以为那条路还在。
    //   ★ 这也是回落层第 3 条约束（「拆除回落是里程碑的一部分，不是以后再说」）
    //   第一次真的被兑现 —— 而兑现的方式是**删掉数据结构**，不是留着不填。
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L4Listener {
    /// `tcp` / `udp`
    pub proto: String,
    pub listen: String,
    /// **兜底**上游：没有任何 `sni` / `alpn` 规则命中时用它。
    ///
    /// ⚠ 它可以是空的 —— 那表示「没匹配上就关掉连接」。★ 空与「没写规则」是两回事：
    /// 后者是纯透传（批 A 的形状），前者是「只服务我认得的那几个名字」。
    pub upstreams: Vec<String>,
    /// SNI / ALPN 分流规则（**M2 批 C**）。★ **按书写顺序匹配，第一个命中即用**。
    ///
    /// ⚠ L4 没有站点块那张执行顺序表（那张表是给中间件排序的），
    /// 所以这里的顺序就是**用户写的顺序** —— 与 `route { … }` 那个保序块同源。
    #[serde(default)]
    pub rules: Vec<L4Rule>,
    /// **收**：信任这些来源发来的 PROXY 头（**M2 批 D**）。空 = 谁都不信。
    ///
    /// ★ 每个监听器**各有一份**，而不是共用 `l4` 块级的一份：
    /// 3306 前面挂的那台 LB 与 443 前面挂的那台通常不是同一台。
    #[serde(default)]
    pub proxy_protocol_from: Vec<String>,
    /// **发**：给上游发一个 PROXY 头，`"v1"` / `"v2"`。`None` = 不发（**M2 批 D**）。
    ///
    /// ⚠ ⚠ **收与发是两件独立的事，不是一个开关的两半**：
    /// 只收不发（枢衡自己要用那个 IP）、只发不收（枢衡是第一跳）、
    /// 收了再发（链式传递）三种都是真实用法。★ 因此它们是两个字段。
    #[serde(default)]
    pub proxy_protocol: Option<String>,
}

/// 一条 L4 分流规则（`sni a.com b.com { proxy … }`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L4Rule {
    /// `sni` / `alpn`。
    pub kind: String,
    /// 要匹配的值。**同一条里多个值是 OR**（与站点块内的匹配器同一条口径）。
    pub values: Vec<String>,
    pub upstreams: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_ones_g63_wrote_down() {
        let d = Defaults::default();
        assert_eq!(
            d.no_site_match, 421,
            "无站点匹配必须是 421，不是静默交给某个站点"
        );
        assert_eq!(d.no_route_match, 404);
        assert_eq!(d.all_upstreams_down, 502);
    }

    #[test]
    fn none_serializes_as_null_so_the_key_set_is_stable() {
        // ★ diff 稳定是 G48 选 JSON 的理由之一。键集合会随取值消失的话，
        //   「把某项从有改成无」在 diff 里看起来就像「结构变了」。
        let g = Global::default();
        let j = serde_json::to_string(&g).unwrap();
        assert!(j.contains("\"acme_email\":null"), "{j}");
    }

    #[test]
    fn round_trips_through_json() {
        let cfg = StructuredConfig {
            schema_version: SCHEMA_VERSION,
            global: Global::default(),
            defaults: Defaults::default(),
            sites: vec![SiteConfig {
                addresses: vec![Address {
                    raw: "example.com".into(),
                    scheme: "https".into(),
                    host: "example.com".into(),
                    port: 443,
                    wildcard: false,
                    auto_https: true,
                }],
                matchers: BTreeMap::new(),
                tls: TlsConfig::default(),
                log: None,
                chain: vec![Step {
                    order: 70,
                    matcher: None,
                    body: StepBody::Respond {
                        status: 200,
                        body: None,
                    },
                }],
                error_handler: Vec::new(),
            }],
            l4: None,
        };
        let j = serde_json::to_string(&cfg).unwrap();
        let back: StructuredConfig = serde_json::from_str(&j).unwrap();
        assert_eq!(cfg, back, "机器写这一层，往返必须无损");
    }
}
