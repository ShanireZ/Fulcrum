//! 指令表与**执行顺序表**。
//!
//! ★ ★ ★ **这一页是 G49 的承重墙。** 站点块内的指令按内建顺序表执行、不按书写顺序
//! （照 Caddy），换来「用户随便写也能跑对」与「Caddyfile 迁移零意外」。代价是
//! **书写顺序 ≠ 执行顺序**，而 `PLAN.md` §3 恰恰点名批评过 nginx 的 `location`
//! 匹配顺序反直觉。四条配套约束里最要紧的一条是：
//!
//! > **新增指令必须在表里有位置，否则编译不过。**
//!
//! 因为「不在表里就排最后」会让新指令**悄悄跑在错误的位置上**——配置能装载、
//! 请求能通、日志一行不报。这与本仓库反复抓到的「声明了却没人接」完全同形。
//!
//! # 这条约束在这里是怎么落地的
//!
//! 分成两半，**两半都要有，缺一半就只挡住了一种走样**：
//!
//! 1. **进不了表就不存在。** [`ChainDirective`] 的每个变体都由下面 `chain_directives!`
//!    的一行生成，而那一行的**第一个字段就是序号**。想加一条链上指令，就必须同时给它
//!    一个位置——不是「忘了会怎样」，是**没有「忘了」这个选项**。
//! 2. **有位置不等于有人接。** [`crate::compile`] 里对 `ChainDirective` 的 `match`
//!    **没有 `_` 兜底臂**。加了变体却没教编译器拿它怎么办，那个 `match` 当场编不过。
//!    ★ 这一半是**可以被证红**的：加一个变体、`cargo build`，就能看见它红。
//!
//! ⚠ 顺序表同时是**公开契约**，与 [DSL 指令集参考](../../../docs/architecture/dsl-reference.md)
//! §三同级维护。`tests/doc_contract.rs` 把两边逐行比对——代码改了文档没改，或者反过来，
//! 都会红。

use crate::diag::{Span, suggest};

/// 指令在执行链上的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 中间件：按序依次执行，可叠加。
    Middleware,
    /// 终结类：**第一个匹配即停**，其后的终结指令不再考虑。
    Terminal,
    /// 容器：本身不产生行为，决定块内那些指令怎么跑。
    Container,
}

impl Kind {
    /// 文档 §三「类别」列印的字。★ 契约的一部分，`tests/doc_contract.rs` 逐字比对。
    pub const fn doc_label(self) -> &'static str {
        match self {
            Kind::Middleware => "中间件",
            Kind::Terminal => "终结",
            Kind::Container => "容器",
        }
    }
}

/// 这条指令由谁承担。
///
/// ⚠ ⚠ **（M2 批 G）起这个枚举只剩三档**：G47 那个「隐式回落」
/// 已经没有对象了 —— 回落层整块删除（G98），详见 `Reserved` 上那条注释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// M1 自研。
    SelfBuilt,
    /// M2 自研（G92）。
    ///
    /// ★ 单独一档而不是复用 `SelfBuilt`，是因为 `doc_label()` 是**逐字契约**：
    /// 拿「M1 自研」去印一件 M2 才做出来的事，文档表里就多了一句假话，
    /// 而那张表恰恰是本仓库用来读进度的地方。`cache` 做 D8 时复用这一档。
    SelfBuiltM2,
    /// 语法上认得、M1 不产生任何行为（`tracing` 就是这一档）。
    ///
    /// ⚠ ⚠ **`FallbackNginx` / `FallbackCaddy` 两档（M2 批 G）删除**（G98）：
    /// §6.3 那个过渡回落层的用户逐块归零 —— `l4` 在批 B 拆掉、`file_server` 在批 F
    /// 改自研、`cache` 在批 G 改自研 ⇒ **一个都不剩**，整层随之删除。
    /// ★ 于是 `fallback_engine()` 这个函数也一并没了：一个所有输入都返回 `None`
    /// 的函数，与一段死代码的区别只在于它读起来像还有用。
    Reserved,
}

impl Owner {
    /// 文档 §三「归属」列印的字。★ 契约的一部分，`tests/doc_contract.rs` 逐字比对。
    ///
    /// ⚠ 列头由「M1 归属」改成「归属」（G92）：一列既然要装
    /// 「M2 自研」，它的名字就不能再自称 M1。
    pub const fn doc_label(self) -> &'static str {
        match self {
            Owner::SelfBuilt => "M1 自研",
            Owner::SelfBuiltM2 => "M2 自研",
            Owner::Reserved => "预留",
        }
    }
}

/// 生成**链上指令**的枚举与顺序表。
///
/// 每一行的形状是：`<序号> <变体> <DSL 名> <类别> <M1 归属>`。
/// ★ 序号写在最前面不是排版偏好——它是这条约束的执行方式：
///   **一行少了序号就编不过，于是「加了指令却没给位置」这件事无从发生。**
macro_rules! chain_directives {
    (
        $( $order:literal $variant:ident $name:literal $kind:ident $owner:ident ),* $(,)?
    ) => {
        /// 站点块的执行链上可以出现的指令。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum ChainDirective {
            $( #[doc = $name] $variant ),*
        }

        impl ChainDirective {
            /// 全表，**按声明顺序**（也就是按序号递增）。
            pub const ALL: &'static [ChainDirective] = &[ $( ChainDirective::$variant ),* ];

            /// 执行顺序表里的序号。★ 这个 `match` 由宏生成、没有 `_` 臂。
            pub const fn order(self) -> u16 {
                match self { $( ChainDirective::$variant => $order ),* }
            }

            /// DSL 里写出来的名字。
            pub const fn name(self) -> &'static str {
                match self { $( ChainDirective::$variant => $name ),* }
            }

            pub const fn kind(self) -> Kind {
                match self { $( ChainDirective::$variant => Kind::$kind ),* }
            }

            pub const fn owner(self) -> Owner {
                match self { $( ChainDirective::$variant => Owner::$owner ),* }
            }

            pub fn from_name(name: &str) -> Option<ChainDirective> {
                match name { $( $name => Some(ChainDirective::$variant), )* _ => None }
            }
        }
    };
}

chain_directives! {
    10 Tracing      "tracing"       Middleware Reserved,
    20 Header       "header"        Middleware SelfBuilt,
    30 Rewrite      "rewrite"       Middleware SelfBuilt,
    40 Encode       "encode"        Middleware SelfBuiltM2,
    50 Cache        "cache"         Middleware SelfBuiltM2,
    55 Handle       "handle"        Container  SelfBuilt,
    56 Route        "route"         Container  SelfBuilt,
    60 Redir        "redir"         Terminal   SelfBuilt,
    70 Respond      "respond"       Terminal   SelfBuilt,
    75 Metrics      "metrics"       Terminal   SelfBuiltM2,
    80 ReverseProxy "reverse_proxy" Terminal   SelfBuilt,
    90 FileServer   "file_server"   Terminal   SelfBuiltM2,
}

/// 生成**站点级指令**的枚举。
///
/// ★ 它们**不在执行链上**，所以没有序号——`tls` 与 `log` 是站点的属性，
/// `handle_errors` 是错误处理器。给它们编一个假的序号，只会让顺序表这份契约变脏。
macro_rules! site_directives {
    ( $( $variant:ident $name:literal $owner:ident ),* $(,)? ) => {
        /// 只能出现在站点块顶层的指令。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum SiteDirective {
            $( #[doc = $name] $variant ),*
        }

        impl SiteDirective {
            pub const ALL: &'static [SiteDirective] = &[ $( SiteDirective::$variant ),* ];

            pub const fn name(self) -> &'static str {
                match self { $( SiteDirective::$variant => $name ),* }
            }

            pub const fn owner(self) -> Owner {
                match self { $( SiteDirective::$variant => Owner::$owner ),* }
            }

            pub fn from_name(name: &str) -> Option<SiteDirective> {
                match name { $( $name => Some(SiteDirective::$variant), )* _ => None }
            }
        }
    };
}

site_directives! {
    Tls          "tls"           SelfBuilt,
    Log          "log"           SelfBuilt,
    HandleErrors "handle_errors" SelfBuilt,
}

/// 一条指令的身份：要么在链上，要么是站点级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    Chain(ChainDirective),
    Site(SiteDirective),
}

impl Directive {
    pub fn from_name(name: &str) -> Option<Directive> {
        ChainDirective::from_name(name)
            .map(Directive::Chain)
            .or_else(|| SiteDirective::from_name(name).map(Directive::Site))
    }

    pub const fn name(self) -> &'static str {
        match self {
            Directive::Chain(d) => d.name(),
            Directive::Site(d) => d.name(),
        }
    }

    pub const fn owner(self) -> Owner {
        match self {
            Directive::Chain(d) => d.owner(),
            Directive::Site(d) => d.owner(),
        }
    }
}

/// 全部指令名（链上 + 站点级），给「你是不是想写 X」用。
pub fn all_names() -> Vec<&'static str> {
    ChainDirective::ALL
        .iter()
        .map(|d| d.name())
        .chain(SiteDirective::ALL.iter().map(|d| d.name()))
        .collect()
}

/// 未知指令时给一个建议。找不到像的就不给——乱指的 help 比没有 help 更耽误人。
pub fn suggest_directive(input: &str) -> Option<&'static str> {
    suggest(input, all_names())
}

// ── 子指令 ──────────────────────────────────────────────────────────────────

/// 参数的字面量类型（DSL 参考 §七）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    /// 任意裸词或引号串。
    Word,
    /// `5s` `1m` `500ms`。★ **裸数字是错误**，不做默认单位。
    Duration,
    /// `10MB` `1GiB`。
    Size,
    /// ★ **只认 `true` / `false`**，不接受 `yes/no/on/off`。
    Bool,
    /// HTTP 状态码，或 `2xx` 这类族。
    StatusPattern,
    /// 固定取值集合。
    Enum(&'static [&'static str]),
}

/// 一条子指令的规格。
#[derive(Debug, Clone, Copy)]
pub struct SubSpec {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub arg_type: ArgType,
}

const fn sub(name: &'static str, min_args: usize, max_args: usize, arg_type: ArgType) -> SubSpec {
    SubSpec {
        name,
        min_args,
        max_args,
        arg_type,
    }
}

/// `reverse_proxy` 的子块（DSL 参考 §4.1）。
pub const REVERSE_PROXY_SUBS: &[SubSpec] = &[
    sub(
        "lb_policy",
        1,
        1,
        ArgType::Enum(&["round_robin", "least_conn", "ip_hash", "random"]),
    ),
    sub("health_uri", 1, 1, ArgType::Word),
    sub("health_interval", 1, 1, ArgType::Duration),
    sub("health_timeout", 1, 1, ArgType::Duration),
    sub("health_status", 1, 1, ArgType::StatusPattern),
    // ★ 这一条直接消灭 nginx OSS 那个经典事故源：上游域名只在启动时解析一次。
    sub("dns_refresh", 1, 1, ArgType::Duration),
    // ⚠ `passive_fail` 的 `arg_type` 是 `Word` 而不是某种「整数」：`check_sub_args` 只做
    //   粗粒度的形状检查，取值域（`[1, 65535]`，含「不是数字」）在 `compile.rs` 里判，
    //   报 `FUL-DSL-0044`。★ 与 `weight` 那一条同一处置。
    sub("passive_fail", 1, 1, ArgType::Word),
    sub("passive_window", 1, 1, ArgType::Duration),
    // ★ ★ G136 新增：熔断之后歇多久再放**一个**半开探针。
    //   ⚠ 它与 `passive_window` 有意是两个旋钮，⛔ 不是 nginx `fail_timeout` 那样一格两用 ——
    //   窗口问「失败要多密才算一个模式」，冷却问「给上游多久缓过来」。
    sub("passive_cooldown", 1, 1, ArgType::Duration),
    // ★ ★ **M2 批 N**：`weight <上游地址> <正整数>`，可写多行，没写的上游权重是 1。
    //   ⚠ 取「地址 + 值」而不是 Caddy 那种位置式（`lb_policy weighted_round_robin 3 1`）：
    //   位置式一改地址的书写顺序就**静默换了权重**，而配置里一个字都看不出问题。
    //   ⚠ `arg_type` 是 `Word` 而不是某种「整数」：`check_sub_args` 只给**第一个**参数
    //   套类型，而第一个参数是地址。权重那一格的值域检查在 `compile.rs` 里做 ——
    //   它要说的话（值域、`0` 为什么不合法、摘节点该写什么）远超一句类型错误。
    sub("weight", 2, 2, ArgType::Word),
    // ★ ★ **M2 批 N**（裁决 R6 ⇒ G125）：给这一条 `reverse_proxy` 一个**稳定 id**，
    //   管理面的临时覆盖层用 `(站点名, id, 归一化后的上游地址)` 寻址。**选填**。
    //   ⚠ `arg_type` 是 `Word`：值域检查（不许是空串）在 `compile.rs` 里做 ——
    //   它要说的话（空串与「没写」在键空间里同形）远超一句类型错误，
    //   与上面 `weight` 把值域留给 `compile.rs` 是同一条理由。
    //   ⛔ **不自动派生**：内容哈希会让「加一台机器」把刚摘掉的坏节点的覆盖顶悬空，
    //   站点内序号会让「换一下书写顺序」静默改掉寻址 —— 两条 owner 都排除过。
    sub("id", 1, 1, ArgType::Word),
    sub("header_up", 1, 2, ArgType::Word),
    sub("header_down", 1, 2, ArgType::Word),
    sub("transport", 1, 1, ArgType::Enum(&["http", "https"])),
    sub("tls_insecure_skip_verify", 0, 1, ArgType::Bool),
    // ★ ★ **M2 批 D**：给上游发一个 PROXY 头。参数可省，省了就是 **v2**（owner 拍板）。
    //   ⚠ 用 `Enum` 而不是 `Word`：版本写错了要在**编译期**red，
    //   而不是在运行时发出一个上游读不懂的头 —— 后者的现场表现是「上游握手失败」。
    sub(
        "proxy_protocol",
        0,
        1,
        ArgType::Enum(crate::model::PROXY_PROTOCOL_VERSIONS),
    ),
];

/// `tls` 的子块（DSL 参考 §4.4）。
pub const TLS_SUBS: &[SubSpec] = &[
    sub("on_demand", 0, 1, ArgType::Bool),
    sub("ask", 1, 1, ArgType::Word),
    sub("dns", 1, 2, ArgType::Word),
    sub("resolvers", 1, usize::MAX, ArgType::Word),
    // ★ G59 第 3 条：这份 DNS 凭据被**声明**覆盖哪些 zone。
    //   原生供应商（cloudflare / dnspod）必填——把「凭据能干什么」变成配置里可读的事实。
    sub("zones", 1, usize::MAX, ArgType::Word),
];

/// `log` 的子块。✅ 字段清单与格式由 **G113 + G114** 定稿（D7 结案）。
pub const LOG_SUBS: &[SubSpec] = &[
    sub("output", 1, 2, ArgType::Word),
    sub(
        "level",
        1,
        1,
        ArgType::Enum(&["debug", "info", "warn", "error"]),
    ),
    // ★ 白名单头（**M2 批 L 第 ③ 步**）。⚠ 默认**一个头都不记**，所以
    //   `min_args = 1`：写了 `headers` 却不给名字是个错，不是「记零个」——
    //   后者用「别写这一行」表达就够了，而前者多半是漏了参数。
    sub("headers", 1, usize::MAX, ArgType::Word),
    sub("resp_headers", 1, usize::MAX, ArgType::Word),
];

/// 任何白名单里都不许出现的头名（**G114 那一半没被推翻的理由**）。
///
/// ★ 小写形态，比对时把用户写的也压成小写 —— HTTP 头名本来就大小写不敏感，
/// 而一条「写 `Authorization` 会红、写 `authorization` 不会」的规则等于没有规则。
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
];

/// `cache` 的子块（M2 自研，M1 回落 nginx）。
pub const CACHE_SUBS: &[SubSpec] = &[
    // ⚠ ⚠ **`ttl` 是「兜底」不是「覆盖」**（G96）：只有上游**没给**
    //   新鲜度信息（`Cache-Control: max-age` / `s-maxage` / `Expires`）时才用它。
    //   ★ 取兜底而不是覆盖的理由很具体：覆盖语义会让一个带 `no-store` 的响应
    //   也被存下来 —— 而那正是「偶尔给错内容」里最贵的一种（给错人看到别人的页面）。
    //   ★ 代价认下：从 nginx `proxy_cache_valid` 迁过来的人会发现它不覆盖。
    sub("ttl", 1, 1, ArgType::Duration),
    // **单条目**大小上限：超过它的响应**不进缓存**（照发给客户端）。
    sub("max_size", 1, 1, ArgType::Size),
    // ★ M2 批 G 新增：**整个缓存**的容量上限，达到就按 LRU 淘汰。
    //   ⚠ 它是必需的而不是可选的 —— 一个没有上限的内存缓存就是一处内存泄漏，
    //   而那种泄漏的现场是「跑几天之后被 OOM 杀掉」，与缓存本身看不出关系。
    sub("capacity", 1, 1, ArgType::Size),
    // ★ M2 批 H 新增：**磁盘后端**的根目录。写了它 ⇒ 缓存落盘；不写 ⇒ 内存后端。
    //
    // ⚠ ⚠ **名字取 `disk` 而不是 `path`**：`path` 在本 DSL 里已经是**请求路径**的意思
    //   （匹配器与 `{path}` 占位符都用它），而一条子指令与一个占位符同名、含义却
    //   完全不同，是一处**读起来毫无异常**的误解来源。
    // ⚠ ⚠ **缓存后端是进程级的**（与 `capacity` 同理，见 `serve()` 里那段注释）：
    //   多个 `cache` 块写了**不同**的 `disk` 是编译期错误（`FUL-DSL-0035`），
    //   ★ 而不是像 `capacity` 那样悄悄取一个 —— 一个被悄悄忽略的目录，
    //   现场是「我的缓存怎么没落到那儿」，而配置里一个字都看不出问题。
    sub("disk", 1, 1, ArgType::Word),
];

/// `cache { capacity … }` 没写时的默认整体容量。
///
/// ★ 有默认值而不是必填：`cache` 这条指令在 M1 就存在了，突然变成必填会让
/// 既有配置编不过。⚠ 而默认值**必须在装载日志里说出来**（与 G88 的 hide 清单同一条纪律）。
pub const CACHE_DEFAULT_CAPACITY_BYTES: u64 = 256 * 1024 * 1024;

/// `cache { max_size … }` 没写时的单条目上限。
pub const CACHE_DEFAULT_MAX_SIZE_BYTES: u64 = 8 * 1024 * 1024;

/// `file_server` 的子块（M2 自研，M1 回落 nginx）。
pub const FILE_SERVER_SUBS: &[SubSpec] = &[
    sub("root", 1, 1, ArgType::Word),
    sub("index", 1, usize::MAX, ArgType::Word),
    // ── M2 批 F（G87 / G88）───────────────────────────────────────────────
    // ⚠ 两条布尔都用 `ArgType::Bool`，也就是**只认 `true`/`false`**，
    //   明确拒绝 `yes/no/on/off`（G87 拍板，`tls_insecure_skip_verify` 同款）。
    //   代价认下：从 nginx 的 `disable_symlinks off` 迁过来的人会写 `off`，
    //   由 `check_arg_type` 那句「布尔只认 true 与 false」接住。
    sub("follow_symlinks", 1, 1, ArgType::Bool),
    sub("hide", 1, usize::MAX, ArgType::Word),
    sub("hide_defaults", 1, 1, ArgType::Bool),
    // ★ M2 批 I（G99）：**预压缩旁文件**。写了 `precompressed br gzip` 之后，
    //   发 `/x.css` 时先看有没有 `/x.css.br`（客户端认 br 的话），有就直接发它。
    //   ⚠ ⚠ 取值与 `encode` 用**同一张表**（gzip / zstd / br）——
    //   两处各写一张的话，`encode` 加了第四种而这里没加，现场是
    //   「配置里写得下、就是不生效」，而不会有任何报错。
    sub(
        "precompressed",
        1,
        usize::MAX,
        ArgType::Enum(&["gzip", "zstd", "br"]),
    ),
];

/// `hide` 的默认清单（G88）。**按路径段**匹配，命中回 **404 不是 403**。
///
/// ★ 回 403 等于确认「这个文件在」——那是一次信息泄漏。
/// ⚠ 它非空，所以装载时必须把生效的清单打出来：**一个不说出来的非空默认就是一次静默行为**
/// （批 20 为「静默失能」专门堵过一次）。
pub const HIDE_DEFAULTS: &[&str] = &[".git", ".env", ".svn", ".hg", ".bzr", ".DS_Store"];

/// 某条链上指令允许的子块规格。返回 `None` 表示它不接受子块。
pub const fn subs_of(d: ChainDirective) -> Option<&'static [SubSpec]> {
    match d {
        ChainDirective::ReverseProxy => Some(REVERSE_PROXY_SUBS),
        ChainDirective::Cache => Some(CACHE_SUBS),
        ChainDirective::FileServer => Some(FILE_SERVER_SUBS),
        // 容器的子块装的是别的指令，不是子指令；由 compile 单独处理。
        ChainDirective::Handle | ChainDirective::Route => None,
        ChainDirective::Tracing
        | ChainDirective::Header
        | ChainDirective::Rewrite
        | ChainDirective::Encode
        | ChainDirective::Redir
        | ChainDirective::Respond
        // ★ `metrics` 一个子指令都没有，也**不打算**有：抓取端要什么由抓取端决定，
        //   而「在配置里挑要暴露哪几个族」会让两次抓取的字段集由配置说了算 ——
        //   ⚠ 那种缺口的现场是「Grafana 上这条线断了」，而服务本身一切正常。
        | ChainDirective::Metrics => None,
    }
}

/// 在一张子指令表里查名字，并在查不到时给建议。
pub fn lookup_sub<'a>(
    table: &'a [SubSpec],
    name: &str,
) -> Result<&'a SubSpec, Option<&'static str>> {
    match table.iter().find(|s| s.name == name) {
        Some(s) => Ok(s),
        None => Err(suggest(name, table.iter().map(|s| s.name))),
    }
}

// ── 全局选项块 ──────────────────────────────────────────────────────────────

/// 全局选项块里允许的键（DSL 参考 §一）。
pub const GLOBAL_OPTIONS: &[SubSpec] = &[
    sub("acme_email", 1, 1, ArgType::Word),
    sub("acme_ca", 1, 1, ArgType::Word),
    sub("admin", 1, 1, ArgType::Word),
    sub("default_sni", 1, 1, ArgType::Word),
    // ★ 自动把 HTTP 重定向到 HTTPS（G12 的后半句）。**默认开**，写 `false` 关掉。
    //   ⚠ 是 `false` 不是 `off` —— DSL 的布尔只认 true/false（§七），
    //   这句话我第一次就写错了，而是编译器自己的诊断把我拦住的。
    // ⚠ 它合成的是**真的站点块**，`fulcrum compile` 与 `plan` 都看得见 ——
    //   而不是数据面里一条看不见的特判。理由见 compile.rs 里那段。
    sub("auto_http_redirect", 1, 1, ArgType::Bool),
    sub("grace_period", 1, 1, ArgType::Duration),
    // ★ ★ 线程数按**角色**分三格（G35 结案、G140 落地），⛔ **有意没有一个笼统的
    //   全局 `threads`**：pingora 的线程不跨 service 共享（每个 service 各起一套
    //   runtime）⇒ 总线程数 ≈ Σ(各 service)，一个全局值设成核数会直接超订。
    //   推导与初值在 docs/architecture/process-model.md 的「线程模型」一节。
    // ⚠ 用 `ArgType::Word` 而不是某个「整数」类型：本表今天没有整数型（`weight`
    //   与 `passive_fail` 都走 Word + compile.rs 里一条专门的诊断），
    //   ⇒ 照既有形状走，值域判在 compile.rs，诊断是 FUL-DSL-0046。
    sub("threads_l7", 1, 1, ArgType::Word),
    sub("threads_l4", 1, 1, ArgType::Word),
    sub("threads_admin", 1, 1, ArgType::Word),
    // ★ ★ 回落后端在哪（§6.3 / G34）。**枢衡不拉起、不监管、不读它们的配置**，
    //   所以它只需要知道一个地址。⚠ 这是「边界显式」那条约束（回落层第 2 条）的落法：
    //   哪些请求走回落必须在配置里看得见，而**看得见的第一步是这一跳写在配置里**。
    // ⚠ ⚠ **`fallback_nginx` / `fallback_caddy` （M2 批 G）从这张表里删除**
    //   （G98）。★ 它们不是被静默丢弃的：`compile.rs` 有一条**专门的诊断**告诉写了它们
    //   的人这一层去哪了 —— 一个被删掉的公开配置面，只回「不认识的全局选项」是不够的。
    sub("proxy_protocol_from", 1, usize::MAX, ArgType::Word),
];

// ── 字面量校验 ──────────────────────────────────────────────────────────────

/// 时长解析。★ **裸数字一律是错误**（DSL 参考 §七）。
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let (num, unit) = split_number_unit(s)?;
    if unit.is_empty() {
        return None; // 裸数字：不猜单位
    }
    let mult = match unit {
        "ms" => 1u64,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// 大小解析。十进制与二进制前缀都认，含义按 IEC。
pub fn parse_size_bytes(s: &str) -> Option<u64> {
    let (num, unit) = split_number_unit(s)?;
    let mult: u64 = match unit {
        "" | "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1u64 << 40,
        _ => return None,
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

/// 布尔。★ 只认这两个，避开 YAML `no → false` 那类坑。
pub fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// 状态码或状态码族（`200` / `2xx`）。
pub fn valid_status_pattern(s: &str) -> bool {
    if let Some(prefix) = s.strip_suffix("xx") {
        return matches!(prefix, "1" | "2" | "3" | "4" | "5");
    }
    matches!(s.parse::<u16>(), Ok(n) if (100..=599).contains(&n))
}

/// 切成「数字部分 + 单位部分」。全是数字时单位是空串。
///
/// ★ **不在这里判「没有单位算不算错」**——那是两种类型各自的事：
/// 大小的裸数字是字节（合法），时长的裸数字是错误（不猜单位）。
/// ⚠ 对全数字串直接返回 `None` 是错的：`max_size 512` 会被判成非法，
/// 而**时长那一侧看起来完全正常**——同一个函数服务两种类型时，
/// 把其中一种的规则写进公共部分，另一种就会安静地跟着变。
fn split_number_unit(s: &str) -> Option<(&str, &str)> {
    let idx = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if idx == 0 {
        return None; // 没有数字部分
    }
    Some((&s[..idx], &s[idx..]))
}

/// 校验一个参数是否符合它声明的类型。出错时返回一句人话。
pub fn check_arg_type(ty: ArgType, value: &str) -> Result<(), (crate::diag::DiagCode, String)> {
    use crate::diag::DiagCode;
    match ty {
        ArgType::Word => Ok(()),
        ArgType::Duration => {
            if parse_duration_ms(value).is_some() {
                Ok(())
            } else if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() {
                Err((
                    DiagCode::BAD_DURATION,
                    format!("时长必须带单位，写成 `{value}s` 或 `{value}ms`"),
                ))
            } else {
                Err((
                    DiagCode::BAD_DURATION,
                    "时长写法是 `500ms` `5s` `1m` `2h` `1d`".to_string(),
                ))
            }
        }
        ArgType::Size => {
            if parse_size_bytes(value).is_some() {
                Ok(())
            } else {
                Err((
                    DiagCode::BAD_SIZE,
                    "大小写法是 `10MB` `1GiB`（十进制与二进制前缀都认）".to_string(),
                ))
            }
        }
        ArgType::Bool => {
            if parse_bool(value).is_some() {
                Ok(())
            } else {
                Err((
                    DiagCode::BAD_BOOL,
                    "布尔只认 `true` 与 `false`（不接受 yes/no/on/off）".to_string(),
                ))
            }
        }
        ArgType::StatusPattern => {
            if valid_status_pattern(value) {
                Ok(())
            } else {
                Err((
                    DiagCode::BAD_STATUS,
                    "写一个状态码（`200`）或一族（`2xx`）".to_string(),
                ))
            }
        }
        ArgType::Enum(allowed) => {
            if allowed.contains(&value) {
                Ok(())
            } else {
                let mut msg = format!("只能是 {}", allowed.join(" / "));
                if let Some(s) = suggest(value, allowed.iter().copied()) {
                    msg.push_str(&format!("；你是不是想写 `{s}`？"));
                }
                Err((DiagCode::BAD_ENUM, msg))
            }
        }
    }
}

/// 一条子指令连同它出现的位置，供 compile 阶段报错用。
#[derive(Debug, Clone)]
pub struct SubUse {
    pub name: String,
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 顺序表严格递增且名字唯一() {
        let mut last = 0u16;
        let mut names = std::collections::BTreeSet::new();
        for d in ChainDirective::ALL {
            assert!(
                d.order() > last,
                "{} 的序号 {} 没有严格大于前一条的 {last}",
                d.name(),
                d.order()
            );
            last = d.order();
            assert!(names.insert(d.name()), "指令名 {} 重复", d.name());
        }
        for d in SiteDirective::ALL {
            assert!(names.insert(d.name()), "指令名 {} 重复", d.name());
        }
    }

    #[test]
    fn 中间件全部排在容器与终结之前() {
        // 顺序表的形状本身就是一条契约：中间件 → 容器 → 终结。
        // 它烂掉的方式是「某天插了一条中间件在终结后面」，而那不会有任何症状。
        let first_container = ChainDirective::ALL
            .iter()
            .find(|d| d.kind() == Kind::Container)
            .map(|d| d.order())
            .expect("表里应当有容器");
        let first_terminal = ChainDirective::ALL
            .iter()
            .find(|d| d.kind() == Kind::Terminal)
            .map(|d| d.order())
            .expect("表里应当有终结类");
        assert!(first_container < first_terminal);
        for d in ChainDirective::ALL {
            match d.kind() {
                Kind::Middleware => assert!(d.order() < first_container, "{}", d.name()),
                Kind::Container => assert!(d.order() < first_terminal, "{}", d.name()),
                Kind::Terminal => assert!(d.order() >= first_terminal, "{}", d.name()),
            }
        }
    }

    #[test]
    fn 容器必须排在终结类之前否则文档首页的例子就是错的() {
        // DSL 参考 §一的示例是：
        //     handle @internal { reverse_proxy … }
        //     respond 403
        // 只有 `handle` 跑在 `respond` **之前**，那个「兜底 403」才是兜底；
        // 否则 403 恒胜，handle 块永远进不去——而配置能装载、请求能通、日志一行不报。
        assert!(ChainDirective::Handle.order() < ChainDirective::Respond.order());
        assert!(ChainDirective::Route.order() < ChainDirective::Respond.order());
    }

    /// `metrics` 的三项身份（**M2 批 M**，G116）。
    ///
    /// ⚠ 上面那两条自测（序号严格递增、终结类全排在中间件与容器之后）**已经自动
    /// 覆盖了新加的这一行**，所以这里只钉它自己的三项：认得出、序号 75、终结类。
    /// ★ 序号 75 是契约的一部分（它必须夹在 `respond` 70 与 `reverse_proxy` 80 之间）
    /// —— 写死 70 < 75 < 80 而不是只写 `== 75`，是因为**光有等号看不出它排在哪**：
    /// 有朝一日 `respond` 挪到 76，等号照样绿，而那时 `metrics` 已经跑在它前面了。
    #[test]
    fn metrics是序号75的终结指令() {
        assert_eq!(
            ChainDirective::from_name("metrics"),
            Some(ChainDirective::Metrics)
        );
        assert_eq!(ChainDirective::Metrics.order(), 75);
        assert_eq!(ChainDirective::Metrics.kind(), Kind::Terminal);
        assert_eq!(ChainDirective::Metrics.owner(), Owner::SelfBuiltM2);
        assert!(ChainDirective::Respond.order() < ChainDirective::Metrics.order());
        assert!(ChainDirective::Metrics.order() < ChainDirective::ReverseProxy.order());
        // 它不收子块 —— 与 `respond` / `redir` 同一档。
        assert!(subs_of(ChainDirective::Metrics).is_none());
    }

    #[test]
    fn 时长不接受裸数字() {
        assert_eq!(parse_duration_ms("5s"), Some(5_000));
        assert_eq!(parse_duration_ms("500ms"), Some(500));
        assert_eq!(parse_duration_ms("2h"), Some(7_200_000));
        assert_eq!(parse_duration_ms("5"), None, "裸数字必须被拒");
        assert_eq!(parse_duration_ms("s"), None);
        assert_eq!(parse_duration_ms("5x"), None);
    }

    #[test]
    fn 布尔只认两个词() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        for w in ["yes", "no", "on", "off", "True", "1", "0"] {
            assert_eq!(parse_bool(w), None, "{w} 不该被接受");
        }
    }

    #[test]
    fn 大小两种前缀都认() {
        assert_eq!(parse_size_bytes("10MB"), Some(10_000_000));
        assert_eq!(parse_size_bytes("1GiB"), Some(1 << 30));
        assert_eq!(parse_size_bytes("512"), Some(512));
        assert_eq!(parse_size_bytes("10Mb"), None);
    }

    #[test]
    fn 状态码族() {
        assert!(valid_status_pattern("200"));
        assert!(valid_status_pattern("2xx"));
        assert!(!valid_status_pattern("6xx"));
        assert!(!valid_status_pattern("99"));
        assert!(!valid_status_pattern("xx"));
    }

    #[test]
    fn 建议只在真的像的时候才给() {
        assert_eq!(suggest_directive("reverse-proxy"), Some("reverse_proxy"));
        assert_eq!(suggest_directive("headr"), Some("header"));
        // ★ 反向：完全不像的不给建议。乱指的 help 会把人带到更远的地方。
        assert_eq!(suggest_directive("完全不相干的东西"), None);
        assert_eq!(suggest_directive("xyzzy"), None);
    }

    #[test]
    fn 回落归属与文档一致() {
        // ⚠ ⚠ **这条测试换了它在验什么，两版契约都留着：**
        //   · M1–批 E：`file_server` 与 `cache` 都回落给 nginx；
        //   · 批 F：`file_server` 改自研 ⇒ 反着钉「它必须没有回落引擎」；
        //   · **批 G（现行）**：`cache` 也改自研 ⇒ **整个回落层删除**，
        //     连 `fallback_engine()` 这个函数都没了。
        //   ★ 于是它现在验的是「归属表里再没有回落那一档」——
        //   ⚠ 一条「加回回落」的改动会让下面这个 `match` 编不过，那就是新的判据。
        for d in ChainDirective::ALL {
            let label = d.owner().doc_label();
            assert!(
                !label.contains("回落"),
                "`{}` 的归属是「{label}」—— 回落层已于批 G 删除",
                d.name()
            );
        }
    }

    /// ★ 三档的 `doc_label()` 互不相同 —— 它是文档表那一列的**逐字契约**，
    /// 两档撞名等于两种归属在文档里长得一模一样。
    #[test]
    fn 三档归属的文档标签互不相同() {
        let labels = [
            Owner::SelfBuilt.doc_label(),
            Owner::SelfBuiltM2.doc_label(),
            Owner::Reserved.doc_label(),
        ];
        let mut sorted = labels;
        sorted.sort_unstable();
        let before = sorted.len();
        let mut d = sorted.to_vec();
        d.dedup();
        assert_eq!(d.len(), before, "有两档的文档标签撞名了：{labels:?}");
    }
}
