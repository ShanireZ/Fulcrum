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
        /// 这一条 `reverse_proxy` 的**稳定 id**（`id` 子指令，**M2 批 N 任务 2.8**，
        /// 裁决 R6 ⇒ G125）。`None` = 没写。
        ///
        /// # 它是干什么用的
        ///
        /// 管理面的临时覆盖层（G18）要寻址「**这个站点块里的那台机器**」，键是
        /// `(站点名, id, 归一化后的上游地址)`。没有 id 时同一站点里两条 `reverse_proxy`
        /// 写了同一个上游就分不开 —— 它们**共享同一个覆盖格子**，一次 `disable`
        /// 把它们一起摘掉（裁决 R6 ③ **第二轮**）。⇒ 想分开就写 `id`。
        ///
        /// ⚠ ⚠ **键相同不是错误、也不给告警**：「一个后端挂在几组 `handle` 路由后面」
        /// 是反代最常见的写法 ⇒ **现有配置一个字节都不用改**。
        /// 「这一格管着几条」由 `fulcrum_runtime::Runtime::proxy_key_fanout` 算出来，
        /// 经 `/stats` 显示（G18「不持久化但永远可见」）。
        ///
        /// # ⚠ ⚠ 键相不相同这件事**看不见于这一层**
        ///
        /// 键里的上游地址取**归一化之后**的那个串（`fulcrum_runtime::normalize_upstream`
        /// 把 `backend` 补成 `backend:80`）⇒ 两条 `reverse_proxy` 一条写 `backend`、
        /// 另一条写 `backend:80`，**原文不同而键相同**。在这一层拿原文 token 比对
        /// 会把那一对算成两格，而它们在运行时是同一台机器
        /// ⇒ 键只在 `fulcrum_runtime` 建图那条路上成形。
        ///
        /// # ⚠ ⚠ 序列化：`None` 时**整个键都不出现**
        ///
        /// 与本模块 `Global` 那条「`None` 写成 `null` 好让键集合稳定」**有意相反**，
        /// 理由与 [`UpstreamSpec`] 的「权重为 1 写回裸字符串」逐字相同：
        /// **没写 `id` 的配置 `compile` 出来的 JSON 一个字节都不许变** ——
        /// 现有夹具、磁盘上那份结构化配置、`POST /load` 的旧载荷全靠它。
        /// ⇒ 这一批「有没有顺手改掉别的东西」因此看得出来。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// 上游清单。★ 每一项自带**配置权重**（[`UpstreamSpec`]，**M2 批 N**）。
        ///
        /// ⚠ ⚠ 取「每项自带权重」而不是「再加一个平行的 `weights: Vec<u32>`」：
        /// 两个等长向量迟早不等长，而不等长的表现是**权重悄悄错位**，不是报错。
        upstreams: Vec<UpstreamSpec>,
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

/// 没写 `weight` 的上游是这个权重（**M2 批 N**，裁决 R1）。
pub const DEFAULT_UPSTREAM_WEIGHT: u32 = 1;

/// 权重值域的下界（裁决 R3）。
///
/// ★ ★ **`0` 有意不合法**：「这台不参与调度」**只有一种表达方式** —— 管理面覆盖层的
/// `disable`。让 `weight 0` 也表示摘掉，就是两条路做同一件事，
/// 而两条路迟早分家（一条改了、另一条没改，且没有任何东西会说）。
pub const MIN_UPSTREAM_WEIGHT: u32 = 1;

/// 权重值域的上界（裁决 R3）。
///
/// ★ 它与管理面 `set_weight` 的值域**是同一对常量**，不是两处各写一个 65535。
pub const MAX_UPSTREAM_WEIGHT: u32 = 65_535;

/// `passive_fail` 值域的下界（**G136**）。
///
/// ★ ★ **`0` 有意不合法，理由与 [`MIN_UPSTREAM_WEIGHT`] 逐字同源**：
/// 「这条 `reverse_proxy` 不做被动熔断」**只有一种表达方式** —— 不写 `passive_fail`。
pub const MIN_PASSIVE_FAIL: u32 = 1;

/// `passive_fail` 值域的上界（**G136**）。
///
/// ⚠ 上界的价值**不在于挡住一个荒谬的大数**（阈值 65535 只是永远熔不断，不危险），
/// 而在于让「不是一个整数」这一类手滑走同一条诊断 —— 那一类才是真正危险的：
/// 在 G136 之前它静默变成 `None`，而 `None` 的语义恰好是**整个特性关掉**。
pub const MAX_PASSIVE_FAIL: u32 = 65_535;

/// `reverse_proxy { id … }` 的长度上限（owner 拍板，R6 那三条小项之三）。
pub const MAX_PROXY_ID_LEN: usize = 64;

/// 这个 `id` 在取值域里吗：`[A-Za-z0-9_.-]`，长度 `1..=`[`MAX_PROXY_ID_LEN`]。
///
/// # ★ ★ ★ 收紧的理由只有一条半，⛔ 别把它记成「基数风险」
///
/// 1. **硬的那条 —— 与兜底记号撞车**：本仓用 `<other>` / `<none>` / `<unknown>` /
///    `<undeclared>` 表示「取不到 / 兜底」，靠的是**尖括号在真值里不可能出现**。
///    而在这条之前，`id <none>` 是合法的。
/// 2. **软的那半条 —— 可读性**：`id` 里的换行会让 `/stats` 与日志上那一行
///    在人眼里断成两行。
///
/// ⛔ **不是**指标基数：G126 明写 `fulcrum_overrides_active` **无标签**，
/// `id` 一个字都不进指标。★ 一个假理由会让下一个人理直气壮地把限制放宽回去。
///
/// # ⚠ 空串在这里回 `false`，但它有**自己的**诊断
///
/// `id ""` 与「根本没写」是同一个键（[`crate::diag::DiagCode::EMPTY_PROXY_ID`]
/// 那段解释是它的全部价值）⇒ 调用方要**先**判空串，⛔ 别让这条泛泛的
/// 「不合法」把那句话顶掉。
///
/// # ⚠ 两条路都要调它
///
/// DSL 那条在 `compile.rs`，**结构化配置**那条（`POST /load`，G11 的公开入口，
/// 不经过 `fulcrum compile`）在 `fulcrum-runtime` 建图时。
/// ★ 两处调的是**这一个**函数，不是两份手写的平行逻辑 —— 与
/// [`MIN_UPSTREAM_WEIGHT`] / [`MAX_UPSTREAM_WEIGHT`] 同一条纪律。
pub fn is_valid_proxy_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PROXY_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// 那句「合法的 id 长什么样」的话，**只有一份**。
///
/// ★ 两条路的诊断/装载错误都拼它 —— ⛔ 两处各写一句，改的时候必定只改一处。
pub fn proxy_id_shape() -> String {
    format!("合法的 `id` 只含 `A-Za-z0-9_.-`，长度 1–{MAX_PROXY_ID_LEN}（例 `pool_web` / `web-1`）")
}

/// 一条 `reverse_proxy` 上的一个上游：地址 + **配置权重**（**M2 批 N**，裁决 R1/R2）。
///
/// # ★ ★ 序列化契约（G11 的公开入口，两个方向都钉住）
///
/// | 情形 | JSON |
/// |---|---|
/// | 反序列化：裸字符串 `"10.0.0.1:8080"` | 收，权重 = [`DEFAULT_UPSTREAM_WEIGHT`] |
/// | 反序列化：对象 `{"addr":"10.0.0.1:8080","weight":3}` | 收 |
/// | 序列化：权重 == 1 | **写回裸字符串** |
/// | 序列化：权重 != 1 | 写成对象 |
///
/// ⇒ ① 旧载荷继续能 `POST /load`（那是个活着的接口，不是内部结构）；
/// ② 没配 `weight` 的配置 `compile` 出来的 JSON **一个字节都不变**。
/// ⚠ ②不是「省事」，它是**判据的一部分**：现有夹具、`plan` 的输出与磁盘上那份结构化配置
/// 全都因此逐字不变，于是这一批**有没有顺手改掉别的东西**看得出来。
///
/// ⚠ 代价写在明处：**`weight 1` 与不写 `weight` 在结构化配置里完全同形**，
/// 装载时也就分不出来。★ 这不是缺口 —— 全部权重都是 1 的调度与今天逐字相同，
/// 「分不出来」的那两种情形本来就没有任何行为差别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSpec {
    /// DSL 里 `reverse_proxy` 那一行上写的**原文 token**。
    ///
    /// ⚠ ⚠ **这一层不做任何归一化**（`backend` 不会变成 `backend:80`）：
    /// 归一化住在 `fulcrum_runtime::normalize_upstream`，在这边再写一份「差不多的」
    /// 就是分家，而分家的现场是「写了 `weight backend 3`，配置照过，权重没生效」。
    /// ⇒ `weight` 的地址比对因此是**逐字相同**，对不上是装载期错误。
    pub addr: String,
    /// 配置权重，值域 `[MIN_UPSTREAM_WEIGHT, MAX_UPSTREAM_WEIGHT]`。
    pub weight: u32,
}

impl UpstreamSpec {
    /// 一个默认权重的上游。
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            weight: DEFAULT_UPSTREAM_WEIGHT,
        }
    }
}

impl Serialize for UpstreamSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        if self.weight == DEFAULT_UPSTREAM_WEIGHT {
            // ★ 这一支是「没配 weight 的配置一个字节都不变」的全部实现。
            return s.serialize_str(&self.addr);
        }
        let mut st = s.serialize_struct("UpstreamSpec", 2)?;
        st.serialize_field("addr", &self.addr)?;
        st.serialize_field("weight", &self.weight)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for UpstreamSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // ⚠ 手写 visitor 而不是 `#[serde(untagged)]`：untagged 在两支都不匹配时
        //   只会说「data did not match any variant」，而这里是 `POST /load` 的入口 ——
        //   一条说不清哪里错了的 400 会把排查成本整个推给对面。
        d.deserialize_any(UpstreamSpecVisitor)
    }
}

struct UpstreamSpecVisitor;

impl<'de> serde::de::Visitor<'de> for UpstreamSpecVisitor {
    type Value = UpstreamSpec;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("一个上游地址字符串，或 {\"addr\":\"host:port\",\"weight\":正整数}")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(UpstreamSpec::new(v))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;
        let mut addr: Option<String> = None;
        let mut weight: Option<u32> = None;
        while let Some(k) = m.next_key::<String>()? {
            match k.as_str() {
                "addr" => {
                    if addr.is_some() {
                        return Err(A::Error::duplicate_field("addr"));
                    }
                    addr = Some(m.next_value()?);
                }
                "weight" => {
                    if weight.is_some() {
                        return Err(A::Error::duplicate_field("weight"));
                    }
                    weight = Some(m.next_value()?);
                }
                // ⚠ 不静默吞掉不认识的键：一个拼错的 `weigth` 会让权重回落成 1，
                //   而那份配置在每一处都显得正常。
                other => {
                    return Err(A::Error::unknown_field(other, &["addr", "weight"]));
                }
            }
        }
        let addr = addr.ok_or_else(|| A::Error::missing_field("addr"))?;
        let weight = weight.unwrap_or(DEFAULT_UPSTREAM_WEIGHT);
        if !(MIN_UPSTREAM_WEIGHT..=MAX_UPSTREAM_WEIGHT).contains(&weight) {
            // ★ 与 DSL 那条诊断（`FUL-DSL-0040`）同一个值域：机器写这一层时
            //   也得撞上同一堵墙，否则「DSL 拒绝、JSON 收下」就是两套规则。
            return Err(A::Error::custom(format!(
                "上游 `{addr}` 的 weight 是 {weight}，要在 {MIN_UPSTREAM_WEIGHT}–{MAX_UPSTREAM_WEIGHT} 之间；\
                 0 不合法 —— 把一个上游摘出调度用管理面的 `disable`"
            )));
        }
        Ok(UpstreamSpec { addr, weight })
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
    /// 窗口内失败几次就熔断。**`None` = 这条 `reverse_proxy` 完全不做被动熔断**（G136）。
    ///
    /// ★ ★ 它是 `Option` 而下面两格不是，因为这两件事**性质不同**：`None` 在这里是
    /// **语义**（关掉），⛔ 不是「没写所以取缺省」。⇒ 既有配置一个字不改、行为一个字不变。
    /// ★ 同一条先例在 `health_uri`：不写它就完全不探测，其余 `health_*` 有缺省也不会
    /// 让探测发生。
    pub fail_threshold: Option<u32>,
    /// 数失败的窗口，缺省 **10s**（G136）。
    ///
    /// ★ ★ **缺省值在这一层就落实成具体数字，⛔ 不留 `Option` 让运行时 `unwrap_or`**：
    /// 那样缺省值会住在两处，而两处迟早分家、分家那天没有任何东西会说。
    /// ⇒ 与隔壁 [`HealthCheck::interval_ms`] 同一形状；`plan` / `compile` 的产物
    /// （G11/G48 的公开契约）因此说得出**真正生效**的值。
    pub window_ms: u64,
    /// 熔断之后歇多久，然后放**一个**半开探针，缺省 **30s**（G136）。
    ///
    /// ★ 它与 [`window_ms`](Self::window_ms) 有意是两个旋钮，⛔ 不是 nginx `fail_timeout`
    /// 那样一格两用：两者问的不是同一个问题 —— 窗口问「失败要多密才算一个模式」，
    /// 冷却问「给上游多久缓过来」，而一个真坏了的上游很少在 10s 内自愈。
    /// ★ 拉长冷却的代价被半开钉死在「每周期 1 个探针」，不会因为拉长而变贵。
    pub cooldown_ms: u64,
}

impl Default for Passive {
    /// ⚠ ⚠ **缺省是「关闭」** —— `fail_threshold: None`。
    /// 下面两格的数字只有在 `passive_fail` 写了之后才有意义。
    fn default() -> Self {
        Self {
            fail_threshold: None,
            window_ms: 10_000,
            cooldown_ms: 30_000,
        }
    }
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

    // ── 上游权重的序列化契约（M2 批 N 任务 1，裁决 R2）────────────────────

    #[test]
    fn 裸字符串上游收得下并且权重是一() {
        // ★ 这一条守的是**旧载荷**：`POST /load` 是活着的接口，
        //   在 `weight` 出现之前发出去的那些 JSON 必须继续能进来。
        let u: UpstreamSpec = serde_json::from_str("\"10.0.0.1:8080\"").unwrap();
        assert_eq!(u.addr, "10.0.0.1:8080");
        assert_eq!(u.weight, DEFAULT_UPSTREAM_WEIGHT);
    }

    #[test]
    fn 对象形态的上游收得下并且权重留住了() {
        let u: UpstreamSpec = serde_json::from_str(r#"{"addr":"x:1","weight":3}"#).unwrap();
        assert_eq!(u.addr, "x:1");
        assert_eq!(u.weight, 3);
        // 往返回去仍是对象，且权重还是 3。
        let j = serde_json::to_string(&u).unwrap();
        assert_eq!(j, r#"{"addr":"x:1","weight":3}"#);
    }

    #[test]
    fn 权重为一时序列化回裸字符串而不是对象() {
        // ★ ★ **反向判据**：少了它，一个「永远写对象」的实现照样能让上面两条绿，
        //   而它会让现有的每一份夹具、每一份磁盘上的结构化配置集体漂移。
        let j = serde_json::to_string(&UpstreamSpec::new("x:1")).unwrap();
        assert_eq!(j, "\"x:1\"", "权重为 1 必须写回裸字符串");
        assert!(
            !j.contains("weight"),
            "权重为 1 时不许出现 weight 这个键：{j}"
        );
        assert!(!j.contains('{'), "权重为 1 时不许序列化成对象：{j}");
    }

    #[test]
    fn 裸字符串往返之后仍是裸字符串() {
        let j = "\"10.0.0.1:8080\"";
        let u: UpstreamSpec = serde_json::from_str(j).unwrap();
        assert_eq!(serde_json::to_string(&u).unwrap(), j);
    }

    #[test]
    fn json_那一侧的权重值域与_dsl_那一侧是同一堵墙() {
        // ⚠ 机器直接写结构化配置也是**公开入口**（G11）：DSL 拒绝而 JSON 收下的话，
        //   就是两套规则，而 `weight 0` 在其中一条路上会悄悄变成「摘掉」。
        for bad in [
            r#"{"addr":"x:1","weight":0}"#,
            r#"{"addr":"x:1","weight":65536}"#,
        ] {
            let e = serde_json::from_str::<UpstreamSpec>(bad).unwrap_err();
            assert!(
                e.to_string().contains("disable"),
                "0 / 越界要说清怎么改对，实际：{e}"
            );
        }
        // 拼错的键不许被静默吞掉 —— 吞掉的表现是权重回落成 1，而配置看起来完全正常。
        let e = serde_json::from_str::<UpstreamSpec>(r#"{"addr":"x:1","weigth":3}"#).unwrap_err();
        assert!(e.to_string().contains("weigth"), "{e}");
    }

    // ── `reverse_proxy` 的稳定 id（M2 批 N 任务 2.8，裁决 R6 ⇒ G125）──────────

    /// 一条最朴素的 `reverse_proxy`，`id` 由调用方给。
    fn rp(id: Option<&str>) -> StepBody {
        StepBody::ReverseProxy {
            id: id.map(str::to_string),
            upstreams: vec![UpstreamSpec::new("10.0.0.1:8080")],
            lb_policy: "round_robin".into(),
            health: HealthCheck::default(),
            dns_refresh_ms: 30_000,
            passive: Passive::default(),
            header_up: Vec::new(),
            header_down: Vec::new(),
            transport: "http".into(),
            tls_insecure_skip_verify: false,
            proxy_protocol: None,
        }
    }

    /// 没有 `id` 的那份产物**逐字**长这样。
    const RP_NO_ID: &str = concat!(
        r#"{"directive":"reverse_proxy","upstreams":["10.0.0.1:8080"],"#,
        r#""lb_policy":"round_robin","#,
        r#""health":{"uri":null,"interval_ms":10000,"timeout_ms":3000,"status":"2xx"},"#,
        r#""dns_refresh_ms":30000,"#,
        r#""passive":{"fail_threshold":null,"window_ms":10000,"cooldown_ms":30000},"#,
        r#""header_up":[],"header_down":[],"transport":"http","#,
        r#""tls_insecure_skip_verify":false,"proxy_protocol":null}"#,
    );

    #[test]
    fn 没写_id_的产物逐字不变() {
        // ★ ★ **硬判据**（任务 2.8 §2 ②）：`id` 这一格必须**完全消失**，
        //   不是写成 `"id":null`。现有夹具、磁盘上那份结构化配置、`POST /load`
        //   的旧载荷全都靠它逐字不变 —— 于是这一批「有没有顺手改掉别的东西」看得出来。
        // ⚠ 这与本模块 `none_serializes_as_null_so_the_key_set_is_stable`
        //   （`Global` 那条「键集合要稳定」）**有意相反**，与 `UpstreamSpec`
        //   的「权重为 1 写回裸字符串」是同一条取舍。
        let j = serde_json::to_string(&rp(None)).unwrap();
        assert_eq!(j, RP_NO_ID, "没写 id 的 reverse_proxy 产物变了");
        assert!(
            !j.contains("\"id\""),
            "没写 id 时不许出现 id 这个键（哪怕值是 null）：{j}"
        );
    }

    #[test]
    fn 写了_id_的产物带得上并且读得回() {
        let j = serde_json::to_string(&rp(Some("pool_web"))).unwrap();
        // ★ 反向判据：少了它，一个「永远不序列化 id」的实现照样能让上面那条绿，
        //   而那样的 id 一 round-trip 就没了。
        assert_eq!(
            j,
            RP_NO_ID.replacen(
                r#""directive":"reverse_proxy","#,
                r#""directive":"reverse_proxy","id":"pool_web","#,
                1
            ),
            "写了 id 的产物不对"
        );
        let back: StepBody = serde_json::from_str(&j).unwrap();
        assert_eq!(back, rp(Some("pool_web")));
    }

    #[test]
    fn 旧载荷没有_id_这个字段也收得下() {
        // ★ `POST /load` 是活着的接口：在 `id` 出现之前发出去的那些 JSON 必须继续能进来。
        let back: StepBody = serde_json::from_str(RP_NO_ID).unwrap();
        assert_eq!(back, rp(None));
        // 往返回去仍然一个字节不差。
        assert_eq!(serde_json::to_string(&back).unwrap(), RP_NO_ID);
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
