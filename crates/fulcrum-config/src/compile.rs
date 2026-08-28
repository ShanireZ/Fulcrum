//! 语义分析 + 编译：AST → [`crate::model::StructuredConfig`]。
//!
//! 这一层做四件事：
//!
//! 1. **认指令**（G60 的清单）。不认识的报 [`DiagCode::UNKNOWN_DIRECTIVE`] 并给建议。
//! 2. **按执行顺序表排序**（G49）。★ ★ 这是顺序表**唯一的真实消费者**——
//!    一张没有人读的表就是一条注释，而本仓库反复抓到的正是「声明了却没人接」。
//! 3. ⛔ **隐式回落（G47）—— 整层已删除**（G98）。★ 编号有意保留。
//! 4. **一次报全**（G51）：语义错误全部收集，不在第一条停下。
//!
//! ★ ★ ★ 下面那个对 [`ChainDirective`] 的 `match` **没有 `_` 兜底臂**，这是有意的：
//! 加了一条指令却没教编译器拿它怎么办，它当场编不过。
//! 「在表里有位置」和「有人接」是两件事，两道门缺一不可。

use crate::ast::{Arg, File, MatcherDef, Node, Site, Stmt, TopBlock};
use crate::diag::{DiagCode, Diagnostic, Diagnostics, Span};
use crate::directive::{
    ArgType, CACHE_SUBS, ChainDirective, Directive, FILE_SERVER_SUBS, GLOBAL_OPTIONS, Kind,
    LOG_SUBS, REVERSE_PROXY_SUBS, SENSITIVE_HEADERS, SiteDirective, SubSpec, TLS_SUBS,
    check_arg_type, lookup_sub, parse_bool, parse_duration_ms, parse_size_bytes, suggest_directive,
};
use crate::model::*;
use crate::placeholder::{self, Ctx};
use crate::secret::Secret;
use std::collections::BTreeMap;

/// 匹配器块里允许的条件（DSL 参考 §五）。
const MATCHER_CONDITIONS: &[&str] = &[
    "path",
    "path_regexp",
    "host",
    "method",
    "header",
    "query",
    "remote_ip",
    "not",
];

/// 给自动 HTTPS 的站点合成一个 `:80` 上的 308 重定向站点（G12 的后半句）。
///
/// 返回 `None` 的三种情况：关掉了、没有自动 HTTPS 的站点、所有主机名都已被
/// 用户自己写的 `http://` 站点接走。
///
/// ★ **它顺带修好了 HTTP-01**：G54 把 HTTP-01 定为「备」，而没有 80 端口就没有落脚点。
/// 挑战应答在路由**之前**，不会被这条 308 吃掉。
fn synthesize_http_redirect(
    global: &Global,
    sites: &[SiteConfig],
    seen_addresses: &BTreeMap<String, Span>,
    cx: &mut Cx<'_>,
) -> Option<SiteConfig> {
    if !global.auto_http_redirect {
        return None;
    }
    // 用户自己在 :80 上写过的主机名 —— 那些不碰。
    let mut taken: BTreeMap<String, ()> = BTreeMap::new();
    let mut catch_all_80: Option<String> = None;
    for site in sites {
        for a in &site.addresses {
            if a.port == 80 && !a.auto_https {
                if a.host.is_empty() {
                    catch_all_80 = Some(a.raw.clone());
                } else {
                    taken.insert(a.host.to_ascii_lowercase(), ());
                }
            }
        }
    }

    let mut hosts: Vec<String> = Vec::new();
    for site in sites {
        for a in &site.addresses {
            if !a.auto_https || a.host.is_empty() {
                continue;
            }
            let h = a.host.to_ascii_lowercase();
            if taken.contains_key(&h) || hosts.contains(&h) {
                continue;
            }
            hosts.push(h);
        }
    }
    if hosts.is_empty() {
        return None;
    }

    // ⚠ ⚠ 站点索引的顺序是**精确 → 通配 → 端口兜底**，所以合成出来的这个块
    //   会盖过用户写的 `:80` 兜底站点 —— 对这几个主机名而言。
    //   ★ 这件事必须**说出来**：它是一次行为改变，而沉默地改掉别人配置的行为，
    //   正是本仓库反复点名的那种「现场看不出问题」。
    if let Some(raw) = &catch_all_80 {
        // ★ 指到**用户写的那个兜底站点**上。
        // ⚠ 退而求其次也不能指文件开头：一条指错地方的诊断比没有诊断更费时间 ——
        //   人会先去看它指的那一行，而那一行与这件事无关。
        let span = seen_addresses.get(raw).copied().unwrap_or(Span::new(0, 0));
        cx.diags.push(
            Diagnostic::warning(
                DiagCode::AUTO_REDIRECT_SHADOWS,
                span,
                format!(
                    "自动 HTTP 重定向会在 :80 上接走这些主机名：{} —— 它们不再落到 `{raw}`",
                    hosts.join(" ")
                ),
            )
            .note(
                "★ 站点索引的顺序是「精确 → 通配 → 端口兜底」，而合成出来的是精确匹配；\
                 不想要就在全局块里写 `auto_http_redirect false`，或者自己写 `http://<主机名>` 站点",
            ),
        );
    }

    let addresses: Vec<Address> = hosts
        .iter()
        .map(|h| Address {
            raw: format!("http://{h}"),
            scheme: "http".to_string(),
            host: h.clone(),
            port: 80,
            wildcard: h.starts_with("*."),
            // ★ 合成出来的这个站点**自己不要证书**：它就是那条跳转。
            auto_https: false,
        })
        .collect();

    Some(SiteConfig {
        addresses,
        matchers: BTreeMap::new(),
        // ⚠ ⚠ **必须显式写 Off**：`TlsConfig::default()` 是 `Automatic`，
        //   于是这个纯 HTTP 的跳转站点会被 TLS 那一层当成「要自动签发」，
        //   装载日志里多出一条「站点 http://… 要自动签发，而存储里还没有它的证书」。
        //   ★ 那条假的 ⏳ 是自己写完当场看见的 —— `plan` 把它印出来了。
        tls: TlsConfig {
            mode: TlsMode::Off,
            ..TlsConfig::default()
        },
        log: None,
        chain: vec![Step {
            order: 60,
            matcher: None,
            // 308 而不是 301：**保留请求方法与 body**。
            // ⚠ 301/302 会让一个 POST 变成 GET，而那类故障的现场是「表单提交丢了」。
            body: StepBody::Redir {
                to: "https://{host}{uri}".to_string(),
                code: 308,
            },
        }],
        error_handler: Vec::new(),
    })
}

pub fn compile(file: &File, diags: &mut Diagnostics) -> StructuredConfig {
    let mut cx = Cx {
        diags,
        cache_backend: None,
    };
    let global = cx.global(file);
    let mut sites = Vec::new();
    let mut l4: Option<L4Config> = None;
    let mut seen_addresses: BTreeMap<String, Span> = BTreeMap::new();

    for block in &file.blocks {
        match block {
            TopBlock::Site(site) => {
                if let Some(s) = cx.site(site, &mut seen_addresses) {
                    sites.push(s);
                }
            }
            TopBlock::L4(node) => {
                if l4.is_some() {
                    cx.diags.push(
                        Diagnostic::error(
                            DiagCode::DUPLICATE_DIRECTIVE,
                            node.name_span,
                            "`l4` 块只能有一个",
                        )
                        .label("前面已经有一个了"),
                    );
                }
                l4 = Some(cx.l4(node));
            }
        }
    }

    // 自动 HTTP 重定向（G12 的后半句）——**合成一个真的站点块**。
    // ★ 合成在**编译期**而不是数据面加特判：合成物会原样出现在 `fulcrum compile` 的
    //   JSON 与 `fulcrum plan` 的输出里 ⇒ **它是可以被读到、被 diff、被质疑的事实**。
    //   ⚠ 一条看不见的特判，在它某天行为不对时，用户手上没有任何东西可查。
    let redirect_site = synthesize_http_redirect(&global, &sites, &seen_addresses, &mut cx);
    if let Some(site) = redirect_site {
        sites.push(site);
    }

    StructuredConfig {
        schema_version: SCHEMA_VERSION,
        global,
        defaults: Defaults::default(),
        sites,
        l4,
    }
}

struct Cx<'a> {
    diags: &'a mut Diagnostics,
    /// **第一个** `cache` 块选的后端（`Some(dir)` = 磁盘，`None` = 内存）与它的 span。
    ///
    /// ★ ★ 缓存后端是**进程级**的，所以这条检查天然是跨站点的 —— 它不可能
    /// 在「编译一条 `cache` 指令」那个函数里做完，那里看不见别的站点。
    /// ⚠ 记的是**第一个**，而诊断指着**后面那个不一样的**：错的那一条才是要改的。
    cache_backend: Option<(Option<String>, Span)>,
}

impl Cx<'_> {
    // ── 全局选项块 ──────────────────────────────────────────────────────────
    fn global(&mut self, file: &File) -> Global {
        let mut g = Global::default();
        let Some(block) = &file.global else {
            return g;
        };
        for stmt in block {
            let Stmt::Node(node) = stmt else {
                self.err_matcher_here(stmt);
                continue;
            };
            // ★ ★ **被删掉的选项，要说清它去哪了。**
            //   ⚠ 只回一句「未知的全局选项」，等于让写过它的人以为自己打错了字，
            //   而那写法在当时是对的。一个公开配置面消失时，最起码要告诉人
            //   「它没了、为什么、现在该怎么写」—— 与 G52（回落必须可见）同一条纪律。
            if let Some(why) = removed_global_option(&node.name) {
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::REMOVED_GLOBAL_OPTION,
                        node.name_span,
                        format!("`{}` 已经删掉了", node.name),
                    )
                    .label("这一行现在没有任何作用")
                    .note(why),
                );
                continue;
            }
            let spec = match lookup_sub(GLOBAL_OPTIONS, &node.name) {
                Ok(s) => s,
                Err(hint) => {
                    let mut d = Diagnostic::error(
                        DiagCode::UNKNOWN_GLOBAL_OPTION,
                        node.name_span,
                        format!("未知的全局选项 `{}`", node.name),
                    )
                    .label("全局选项块里没有这一项")
                    .note("全局选项见 docs/architecture/dsl-reference.md §一");
                    if let Some(h) = hint {
                        d = d.help(format!("你是不是想写 `{h}`？"));
                    }
                    self.diags.push(d);
                    continue;
                }
            };
            if !self.check_sub_args(node, spec) {
                continue;
            }
            let v = node.args.first().map(|a| a.value.clone());
            match node.name.as_str() {
                "acme_email" => g.acme_email = v,
                "acme_ca" => g.acme_ca = v,
                "admin" => g.admin = v,
                "default_sni" => g.default_sni = v,
                "grace_period" => {
                    g.grace_period_ms = node.args.first().and_then(|a| parse_duration_ms(&a.value))
                }
                "auto_http_redirect" => {
                    // ★ 不写值就是「开」（`auto_http_redirect` 单写一行 = on），
                    //   与 `on_demand` 那条同形。
                    g.auto_http_redirect = node
                        .args
                        .first()
                        .is_none_or(|a| parse_bool(&a.value) == Some(true))
                }
                // ★ 收下全部参数（`v` 只是第一个）。网段的合法性在运行时图那一层
                //   与 `remote_ip` 共用 `Cidr::parse` 校验 —— 不在这里另写一份。
                "proxy_protocol_from" => {
                    g.proxy_protocol_from = node.args.iter().map(|a| a.value.clone()).collect()
                }
                // ⚠ 这条 `_ => {}` 是「表里有位置、没人接」的温床（批 6 在 `tls` 上栽过）。
                //   守着它的是 compile_behaviour.rs 的 `全局选项全部有人接`：
                //   GLOBAL_OPTIONS 里多一条而这里没接，那条测试**直接 panic**。
                _ => {}
            }
        }
        g
    }

    // ── 站点块 ──────────────────────────────────────────────────────────────
    fn site(
        &mut self,
        site: &Site,
        seen_addresses: &mut BTreeMap<String, Span>,
    ) -> Option<SiteConfig> {
        let mut addresses = Vec::new();
        for addr in &site.addresses {
            match parse_address(&addr.text) {
                Ok(a) => {
                    if let Some(_prev) = seen_addresses.insert(addr.text.clone(), addr.span) {
                        self.diags.push(
                            Diagnostic::error(
                                DiagCode::DUPLICATE_SITE_ADDRESS,
                                addr.span,
                                format!("地址 `{}` 在多个站点块里出现", addr.text),
                            )
                            .label("同一个地址只能属于一个站点块")
                            .help("要给同一批地址写多条规则，把它们合进同一个块"),
                        );
                    }
                    addresses.push(a);
                }
                Err(msg) => {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::BAD_SITE_ADDRESS,
                            addr.span,
                            format!("站点地址 `{}` 不合法", addr.text),
                        )
                        .label(msg)
                        .note("地址写法见 docs/architecture/dsl-reference.md §二"),
                    );
                }
            }
        }
        if addresses.is_empty() {
            return None;
        }

        let mut matchers: BTreeMap<String, Matcher> = BTreeMap::new();
        let mut matcher_spans: BTreeMap<String, Span> = BTreeMap::new();
        for stmt in &site.body {
            if let Stmt::Matcher(m) = stmt {
                self.matcher_def(m, &mut matchers, &mut matcher_spans);
            }
        }

        let mut tls = TlsConfig::default();
        let mut log: Option<LogConfig> = None;
        let mut error_handler: Vec<Step> = Vec::new();
        let mut seen_site_directives: BTreeMap<&'static str, Span> = BTreeMap::new();
        let mut tls_written = false;

        // 站点级指令先摘出来，剩下的才是链上的。
        let mut chain_nodes: Vec<&Node> = Vec::new();
        for stmt in &site.body {
            let Stmt::Node(node) = stmt else { continue };
            match Directive::from_name(&node.name) {
                Some(Directive::Site(sd)) => {
                    if let Some(_prev) = seen_site_directives.insert(sd.name(), node.name_span) {
                        self.diags.push(
                            Diagnostic::error(
                                DiagCode::DUPLICATE_DIRECTIVE,
                                node.name_span,
                                format!("`{}` 在同一个站点里只能出现一次", sd.name()),
                            )
                            .label("前面已经有一条了"),
                        );
                        continue;
                    }
                    match sd {
                        SiteDirective::Tls => {
                            tls = self.tls(node);
                            tls_written = true;
                        }
                        SiteDirective::Log => log = Some(self.log(node)),
                        SiteDirective::HandleErrors => {
                            error_handler = self.handle_errors(node, &matchers);
                        }
                    }
                }
                _ => chain_nodes.push(node),
            }
        }

        // `http://` 地址不签证书、不升级；DSL 里没写 `tls` 时按地址推出模式。
        if !tls_written && addresses.iter().all(|a| !a.auto_https) {
            tls.mode = TlsMode::Off;
        }

        // ★ `guarded = false`：站点块顶层没有任何外层匹配器罩着。
        let chain = self.chain(&chain_nodes, &matchers, Ctx::Request, true, false);

        if chain.is_empty() && error_handler.is_empty() {
            self.diags.push(
                Diagnostic::warning(
                    DiagCode::EMPTY_SITE,
                    site.header_span,
                    "这个站点块里没有任何指令",
                )
                .label("它会对所有请求返回 404")
                .note("站点内无路由匹配时的默认响应是 404（G63）"),
            );
        }

        Some(SiteConfig {
            addresses,
            matchers,
            tls,
            log,
            chain,
            error_handler,
        })
    }

    // ── 匹配器 ──────────────────────────────────────────────────────────────
    fn matcher_def(
        &mut self,
        m: &MatcherDef,
        out: &mut BTreeMap<String, Matcher>,
        spans: &mut BTreeMap<String, Span>,
    ) {
        if spans.contains_key(&m.name) {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::DUPLICATE_MATCHER,
                    m.span,
                    format!("匹配器 `@{}` 重复定义", m.name),
                )
                .label("前面已经定义过一个同名的")
                .help("同一块里的多个条件本来就是 AND，写进同一个 `@name { … }` 即可"),
            );
            return;
        }
        spans.insert(m.name.clone(), m.span);
        let mut conditions = Vec::new();
        for c in &m.conditions {
            self.condition(c, false, &mut conditions);
        }
        out.insert(m.name.clone(), Matcher { conditions });
    }

    fn condition(&mut self, node: &Node, negate: bool, out: &mut Vec<Condition>) {
        if !MATCHER_CONDITIONS.contains(&node.name.as_str()) {
            let mut d = Diagnostic::error(
                DiagCode::UNKNOWN_MATCHER_CONDITION,
                node.name_span,
                format!("未知的匹配条件 `{}`", node.name),
            )
            .label("匹配器块里只能写条件")
            .note("全部条件见 docs/architecture/dsl-reference.md §五");
            if let Some(h) = crate::diag::suggest(&node.name, MATCHER_CONDITIONS.iter().copied()) {
                d = d.help(format!("你是不是想写 `{h}`？"));
            }
            self.diags.push(d);
            return;
        }

        if node.name == "not" {
            let Some(block) = &node.block else {
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::BLOCK_MISMATCH,
                        node.name_span,
                        "`not` 后面要跟一个 `{ … }` 块",
                    )
                    .label("这里缺一个块"),
                );
                return;
            };
            for stmt in block {
                match stmt {
                    Stmt::Node(inner) => self.condition(inner, !negate, out),
                    other => self.err_matcher_here(other),
                }
            }
            return;
        }

        if node.args.is_empty() {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_ARITY,
                    node.name_span,
                    format!("条件 `{}` 少了取值", node.name),
                )
                .label("后面至少要跟一个值"),
            );
            return;
        }
        for a in &node.args {
            self.check_placeholders(a, Ctx::Request);
        }
        out.push(Condition {
            kind: node.name.clone(),
            values: node.args.iter().map(|a| a.value.clone()).collect(),
            negate,
        });
    }

    // ── 执行链 ──────────────────────────────────────────────────────────────

    /// 把一组节点编译成执行链。
    ///
    /// `sorted = true` 时按执行顺序表排序（站点块顶层、`handle` 的每个 arm）；
    /// `sorted = false` 时保持书写顺序（`route { … }` 内部——**G49 的逃生口**）。
    ///
    /// `guarded` = **外层容器已经把来源限制住了**（[`restricts_source`]）。
    /// ★ 它只服务于 G116 那条诊断，而那条诊断问的是「这一步对**谁**开着」。
    ///
    /// 要沿链下传，是因为 `handle @internal { metrics }` 里那一步**自己**没有匹配器，
    /// ⚠ 但它显然是被圈住的 —— 只看这一步自己的匹配器会把它误报成裸奔，
    /// 而一条稳定误报的诊断很快就没人看了。
    ///
    /// ⚠ ⚠ **口径是「限制得了来源」，不是「有没有匹配器」。**
    /// `handle /metrics { … }` 里那个路径匹配器是**任何客户端都能照着发**的东西，
    /// 它一个人都挡不住。⇒ 判据落在 [`restricts_source`] 上，
    /// **不是** [`is_unconditional`]（那一条问的是另一件事，见它自己的文档）。
    fn chain(
        &mut self,
        nodes: &[&Node],
        matchers: &BTreeMap<String, Matcher>,
        ctx: Ctx,
        sorted: bool,
        guarded: bool,
    ) -> Vec<Step> {
        // ★ 所有 `handle` 合成**一个**互斥组，放在 `handle` 的表位上。
        //   它们分散写在各处也是同一组——这正是「互斥容器」的含义。
        let mut handle_arms: Vec<HandleArm> = Vec::new();
        let mut handle_span: Option<Span> = None;
        // ★ 每一步都拖着它在源码里的位置。诊断要能说出「它实际跑在第几步」，
        //   而排完序之后**书写顺序已经丢了**——位置若不在这时候带上，后面就再也拿不到。
        let mut steps: Vec<(Step, Span)> = Vec::new();

        for node in nodes {
            if node.name == "import" {
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::IMPORT_NOT_SUPPORTED,
                        node.name_span,
                        "M1 只认一份配置文件，不支持 `import`",
                    )
                    .label("这条指令 M1 不做")
                    .note(
                        "G62：`import` 会一次性引入跨文件错误定位、循环引用与相对路径三件事，\
                         恰好都打在错误提示刚把标准定高的那个面上。M2 再说",
                    ),
                );
                continue;
            }

            let d = match Directive::from_name(&node.name) {
                Some(Directive::Chain(d)) => d,
                Some(Directive::Site(sd)) => {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::BAD_PLACEMENT,
                            node.name_span,
                            format!("`{}` 只能写在站点块的顶层", sd.name()),
                        )
                        .label("它是站点级指令，不在执行链上")
                        .note("站点级指令见 docs/architecture/dsl-reference.md §4.4"),
                    );
                    continue;
                }
                None => {
                    let mut diag = Diagnostic::error(
                        DiagCode::UNKNOWN_DIRECTIVE,
                        node.name_span,
                        format!("unknown directive `{}`", node.name),
                    )
                    .label("未知指令")
                    .note("全部指令见 docs/architecture/dsl-reference.md §4");
                    if let Some(h) = suggest_directive(&node.name) {
                        diag = diag.help(format!("你是不是想写 `{h}`？"));
                    }
                    self.diags.push(diag);
                    continue;
                }
            };

            if d == ChainDirective::Handle {
                handle_span.get_or_insert(node.name_span);
                if let Some(arm) = self.handle_arm(node, matchers, ctx, guarded) {
                    handle_arms.push(arm);
                }
                continue;
            }

            if let Some(step) = self.step(d, node, matchers, ctx, guarded) {
                steps.push((step, node.name_span));
            }
        }

        if !handle_arms.is_empty()
            && let Some(span) = handle_span
        {
            steps.push((
                Step {
                    order: ChainDirective::Handle.order(),
                    matcher: None,
                    body: StepBody::Handle { arms: handle_arms },
                },
                span,
            ));
        }

        if sorted {
            // ★ **稳定排序**：同序号的多条（比如两条 `header`）保持书写顺序。
            //   不稳定的排序会让「两条 header 谁先跑」随实现细节抖动，
            //   而那种抖动在配置层是查不出来的。
            steps.sort_by_key(|(s, _)| s.order);
            self.warn_unreachable(&steps);
        }
        steps.into_iter().map(|(s, _)| s).collect()
    }

    /// `handle` 的一个分支。
    fn handle_arm(
        &mut self,
        node: &Node,
        matchers: &BTreeMap<String, Matcher>,
        ctx: Ctx,
        guarded: bool,
    ) -> Option<HandleArm> {
        let matcher = self.matcher_ref(node, matchers);
        // 这个分支限制得了来源 ⇒ 块里的每一步都被它罩着。见 `restricts_source`。
        let guarded = guarded || restricts_source(&matcher, matchers);
        let Some(block) = &node.block else {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BLOCK_MISMATCH,
                    node.name_span,
                    "`handle` 后面要跟一个 `{ … }` 块",
                )
                .label("这里缺一个块"),
            );
            return None;
        };
        let inner: Vec<&Node> = block
            .iter()
            .filter_map(|s| match s {
                Stmt::Node(n) => Some(n),
                Stmt::Matcher(m) => {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::BAD_PLACEMENT,
                            m.span,
                            "匹配器要定义在站点块的顶层",
                        )
                        .label("这里不能定义匹配器")
                        .help("把 `@name { … }` 提到站点块的第一层"),
                    );
                    None
                }
            })
            .collect();
        Some(HandleArm {
            matcher,
            steps: self.chain(&inner, matchers, ctx, true, guarded),
        })
    }

    /// 编译一条链上指令。
    ///
    /// ★ ★ ★ 这个 `match` **没有 `_` 臂**。新增一条链上指令时它会当场编不过——
    /// 这就是 G49 那条「新增指令必须在表里有位置」的第二半：
    /// **有位置** ≠ **有人接**，两者都要编译器来管。
    fn step(
        &mut self,
        d: ChainDirective,
        node: &Node,
        matchers: &BTreeMap<String, Matcher>,
        ctx: Ctx,
        guarded: bool,
    ) -> Option<Step> {
        let matcher = self.matcher_ref(node, matchers);
        let args = node.rest_args();
        // 这一步的来源被限制住了吗：自己写了一个限制得了来源的匹配器，
        // 或者外层容器带着一个。⚠ 「有匹配器」不等于「限制了来源」，见 `restricts_source`。
        let guarded = guarded || restricts_source(&matcher, matchers);

        let body = match d {
            ChainDirective::Tracing => {
                self.expect_no_block(node, d);
                StepBody::Tracing
            }
            ChainDirective::Header => {
                let ops = self.header_ops(node, args, resp_ctx(ctx));
                if ops.is_empty() {
                    return None;
                }
                StepBody::Header { ops }
            }
            ChainDirective::Rewrite => {
                self.expect_no_block(node, d);
                let to = self.exactly_one(node, args, "改写到的路径")?;
                self.check_placeholders(to, ctx);
                StepBody::Rewrite {
                    to: to.value.clone(),
                }
            }
            ChainDirective::Encode => {
                self.expect_no_block(node, d);
                if args.is_empty() {
                    self.arity_error(node, "至少写一种编码，如 `encode gzip zstd`");
                    return None;
                }
                let mut encodings = Vec::new();
                for a in args {
                    if let Err((code, msg)) =
                        check_arg_type(ArgType::Enum(&["gzip", "zstd", "br"]), &a.value)
                    {
                        self.diags.push(
                            Diagnostic::error(
                                code,
                                a.span,
                                format!("`{}` 不是支持的编码", a.value),
                            )
                            .label(msg),
                        );
                        continue;
                    }
                    encodings.push(a.value.clone());
                }
                StepBody::Encode { encodings }
            }
            ChainDirective::Cache => {
                let subs = self.sub_block(node, CACHE_SUBS);
                let disk_dir = subs.get("disk").cloned();
                self.check_cache_backend(node, &disk_dir);
                StepBody::Cache {
                    ttl_ms: subs.get("ttl").and_then(|v| parse_duration_ms(v)),
                    max_size_bytes: subs.get("max_size").and_then(|v| parse_size_bytes(v)),
                    capacity_bytes: subs.get("capacity").and_then(|v| parse_size_bytes(v)),
                    disk_dir,
                }
            }
            // `handle` 在 `chain()` 里先被摘出去合并成互斥组，不会走到这里。
            ChainDirective::Handle => return None,
            ChainDirective::Route => {
                let Some(block) = &node.block else {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::BLOCK_MISMATCH,
                            node.name_span,
                            "`route` 后面要跟一个 `{ … }` 块",
                        )
                        .label("这里缺一个块"),
                    );
                    return None;
                };
                let inner: Vec<&Node> = block
                    .iter()
                    .filter_map(|s| match s {
                        Stmt::Node(n) => Some(n),
                        Stmt::Matcher(m) => {
                            self.diags.push(
                                Diagnostic::error(
                                    DiagCode::BAD_PLACEMENT,
                                    m.span,
                                    "匹配器要定义在站点块的顶层",
                                )
                                .label("这里不能定义匹配器"),
                            );
                            None
                        }
                    })
                    .collect();
                // ★ `sorted = false`：这就是逃生口本身。
                StepBody::Route {
                    steps: self.chain(&inner, matchers, ctx, false, guarded),
                }
            }
            ChainDirective::Redir => {
                self.expect_no_block(node, d);
                if args.is_empty() || args.len() > 2 {
                    self.arity_error(node, "写法是 `redir [matcher] <to> [code]`");
                    return None;
                }
                self.check_placeholders(&args[0], ctx);
                let code = match args.get(1) {
                    None => 302,
                    Some(a) => match a.value.parse::<u16>() {
                        Ok(n) if (300..=399).contains(&n) => n,
                        _ => {
                            self.diags.push(
                                Diagnostic::error(
                                    DiagCode::BAD_STATUS,
                                    a.span,
                                    "重定向状态码必须是 3xx",
                                )
                                .label("写 301 / 302 / 307 / 308 之一")
                                .help("不写的话默认是 302"),
                            );
                            302
                        }
                    },
                };
                StepBody::Redir {
                    to: args[0].value.clone(),
                    code,
                }
            }
            ChainDirective::Respond => {
                self.expect_no_block(node, d);
                if args.is_empty() || args.len() > 2 {
                    self.arity_error(node, "写法是 `respond [matcher] <status> [body]`");
                    return None;
                }
                let status = match args[0].value.parse::<u16>() {
                    Ok(n) if (100..=599).contains(&n) => n,
                    _ => {
                        self.diags.push(
                            Diagnostic::error(
                                DiagCode::BAD_STATUS,
                                args[0].span,
                                "第一个参数是状态码",
                            )
                            .label("100–599 之间的整数")
                            .note("写法是 `respond [matcher] <status> [body]`"),
                        );
                        return None;
                    }
                };
                if let Some(b) = args.get(1) {
                    self.check_placeholders(b, ctx);
                }
                StepBody::Respond {
                    status,
                    body: args.get(1).map(|a| a.value.clone()),
                }
            }
            // ── Prometheus 抓取端点（**M2 批 M**，G116）──────────────────────
            ChainDirective::Metrics => {
                self.expect_no_block(node, d);
                if !args.is_empty() {
                    // ⚠ 一个参数都不收。⇒ `metrics /x` 里那个 `/x` 走的是**行内匹配器**
                    //   那条路（G50），压根到不了这里；到这里的都是真的多余参数。
                    self.arity_error(node, "写法就是 `metrics`，它不收任何参数");
                    return None;
                }
                if !guarded {
                    self.diags.push(
                        Diagnostic::warning(
                            DiagCode::METRICS_UNGUARDED,
                            node.name_span,
                            "`metrics` 的来源没有被任何匹配器限制住",
                        )
                        .label("凡是连得到这个监听端口的人都能抓走全部指标")
                        .help(
                            "用 `remote_ip` 或 `header` 圈住来源，例如：\
                             `@internal remote_ip 10.0.0.0/8` 加上 \
                             `handle @internal { metrics }`",
                        )
                        .note(
                            "G116：指标端点与业务共用监听器，这一条只能靠文档与诊断兜。\
                             ★ 算数的只有 `remote_ip`（socket 对端，伪造不了）与 `header`\
                             （可以放一个共享密钥）；`path` / `path_regexp` / `host` / \
                             `method` / `query` **都不算** —— 它们是请求里的东西，\
                             任何客户端都能照着发一份，只决定端点摆在哪，不减少能碰到它的人。\
                             ⚠ 匹配器写得**对不对**（网段圈没圈对）本诊断仍然判不动",
                        ),
                    );
                }
                StepBody::Metrics
            }
            ChainDirective::ReverseProxy => self.reverse_proxy(node, args, ctx),
            ChainDirective::FileServer => self.file_server(node, args),
        };

        Some(Step {
            order: d.order(),
            matcher,
            body,
        })
    }

    /// `file_server` 指令（M2 批 F，G87–G91）。
    ///
    /// ★ 这里**没有**走 `sub_block()`，是有原因的：那个 helper 把子块读成
    /// 「名字 → 值」的 `BTreeMap`，于是**同名子指令写两遍时后一条覆盖前一条**。
    /// `hide` 的语义是「再多挡一样」，写两行 `hide` 却只有第二行生效，
    /// 就是一次静默行为——正是 G88 那张表里「可见性」那一格要堵的东西。
    /// ⇒ 照 `reverse_proxy` 的做法自己遍历，`hide` 跨行累加。
    fn file_server(&mut self, node: &Node, args: &[Arg]) -> StepBody {
        let browse = args.iter().any(|a| a.value == "browse");
        for a in args {
            if a.value != "browse" {
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::BAD_ARITY,
                        a.span,
                        format!("`file_server` 不认识参数 `{}`", a.value),
                    )
                    .label("只有 `browse` 一个标志")
                    .note("根目录写在子块里：`file_server { root /srv/www }`"),
                );
            }
        }

        let mut root: Option<String> = None;
        let mut root_span: Option<Span> = None;
        let mut index: Vec<String> = Vec::new();
        let mut follow_symlinks = true;
        let mut hide: Vec<String> = Vec::new();
        let mut hide_defaults = true;
        let mut precompressed: Vec<String> = Vec::new();

        if let Some(block) = &node.block {
            for stmt in block {
                let Stmt::Node(sub) = stmt else {
                    self.err_matcher_here(stmt);
                    continue;
                };
                let spec = match lookup_sub(FILE_SERVER_SUBS, &sub.name) {
                    Ok(s) => s,
                    Err(hint) => {
                        self.unknown_sub(sub, "file_server", hint);
                        continue;
                    }
                };
                if !self.check_sub_args(sub, spec) {
                    continue;
                }
                match sub.name.as_str() {
                    "root" => {
                        if let Some(a) = sub.args.first() {
                            root = Some(a.value.clone());
                            root_span = Some(a.span);
                        }
                    }
                    "index" => {
                        index = sub.args.iter().map(|a| a.value.clone()).collect();
                    }
                    // ⚠ `check_sub_args` 已经把非 `true`/`false` 拦下并 `continue` 了，
                    //   所以这里 `parse_bool` 不会是 `None`；用 `is_none_or(..)` 兜一下
                    //   只是为了不在这条路上再造一个 `unwrap`。
                    "follow_symlinks" => {
                        follow_symlinks = sub
                            .args
                            .first()
                            .is_none_or(|a| parse_bool(&a.value) == Some(true));
                    }
                    "hide_defaults" => {
                        hide_defaults = sub
                            .args
                            .first()
                            .is_none_or(|a| parse_bool(&a.value) == Some(true));
                    }
                    // ★ 追加，不是替换（G88）——多行 `hide` 叠起来。
                    "hide" => hide.extend(sub.args.iter().map(|a| a.value.clone())),
                    // ★ M2 批 I：预压缩旁文件。同样**追加**，与 `hide` 同款。
                    "precompressed" => {
                        precompressed.extend(sub.args.iter().map(|a| a.value.clone()))
                    }
                    _ => {}
                }
            }
        }

        // ── root 必填（G89 自研之后没有 root 就没有东西可发）─────────────────
        match &root {
            None => self.diags.push(
                Diagnostic::error(
                    DiagCode::MISSING_REQUIRED_SUB,
                    node.name_span,
                    "`file_server` 少了必填的 `root`",
                )
                .label("自研之后没有 root 就没有东西可发")
                .note("写成 `file_server { root /srv/www }`（M2 批 F 起 root 必填）"),
            ),
            // ── 必须绝对路径（G91）────────────────────────────────────────
            Some(r) if !r.starts_with('/') => self.diags.push(
                Diagnostic::error(
                    DiagCode::PATH_NOT_ABSOLUTE,
                    root_span.unwrap_or(node.name_span),
                    format!("`root` 必须是绝对路径，`{r}` 不是"),
                )
                .label("相对路径按进程 cwd 解析")
                .note(
                    "而 systemd 下 cwd 是 `/`、开发机上是项目目录 —— \
                     同一份配置会在两处指向两个地方，现场只看得到 404",
                ),
            ),
            Some(_) => {}
        }

        StepBody::FileServer {
            root,
            browse,
            index,
            follow_symlinks,
            hide,
            hide_defaults,
            precompressed,
        }
    }

    fn reverse_proxy(&mut self, node: &Node, args: &[Arg], ctx: Ctx) -> StepBody {
        let mut upstreams: Vec<String> = Vec::new();
        for a in args {
            self.check_placeholders(a, ctx);
            upstreams.push(a.value.clone());
        }
        if upstreams.is_empty() {
            self.arity_error(node, "至少写一个上游，如 `reverse_proxy 127.0.0.1:3000`");
        }

        let mut lb_policy = "round_robin".to_string();
        let mut health = HealthCheck::default();
        let mut dns_refresh_ms = 30_000u64;
        let mut passive = Passive {
            fail_threshold: None,
            window_ms: None,
        };
        let mut header_up: Vec<HeaderOp> = Vec::new();
        let mut header_down: Vec<HeaderOp> = Vec::new();
        let mut transport = "http".to_string();
        let mut tls_insecure_skip_verify = false;
        let mut proxy_protocol: Option<String> = None;

        if let Some(block) = &node.block {
            for stmt in block {
                let Stmt::Node(sub) = stmt else {
                    self.err_matcher_here(stmt);
                    continue;
                };
                let spec = match lookup_sub(REVERSE_PROXY_SUBS, &sub.name) {
                    Ok(s) => s,
                    Err(hint) => {
                        self.unknown_sub(sub, "reverse_proxy", hint);
                        continue;
                    }
                };
                if !self.check_sub_args(sub, spec) {
                    continue;
                }
                let first = sub
                    .args
                    .first()
                    .map(|a| a.value.clone())
                    .unwrap_or_default();
                match sub.name.as_str() {
                    "lb_policy" => lb_policy = first,
                    "health_uri" => health.uri = Some(first),
                    "health_interval" => {
                        health.interval_ms = parse_duration_ms(&first).unwrap_or(health.interval_ms)
                    }
                    "health_timeout" => {
                        health.timeout_ms = parse_duration_ms(&first).unwrap_or(health.timeout_ms)
                    }
                    "health_status" => health.status = first,
                    "dns_refresh" => {
                        dns_refresh_ms = parse_duration_ms(&first).unwrap_or(dns_refresh_ms)
                    }
                    "passive_fail" => passive.fail_threshold = first.parse().ok(),
                    "passive_window" => passive.window_ms = parse_duration_ms(&first),
                    "header_up" => {
                        if let Some(op) = self.header_op(&sub.args, ctx) {
                            header_up.push(op);
                        }
                    }
                    "header_down" => {
                        if let Some(op) = self.header_op(&sub.args, resp_ctx(ctx)) {
                            header_down.push(op);
                        }
                    }
                    "transport" => transport = first,
                    "tls_insecure_skip_verify" => {
                        tls_insecure_skip_verify = sub
                            .args
                            .first()
                            .is_none_or(|a| parse_bool(&a.value) == Some(true));
                    }
                    // ★ 参数可省 ⇒ 省了就是 v2（owner 拍板的默认）。
                    //   ⚠ 默认值写在**这一处**，而 `fulcrum_runtime::proxyproto::Version`
                    //   的 `Default` 也是 v2 —— 两处一致由那边一条单测钉着
                    //   （`版本名与_dsl_里写的那个词一一对应`）。
                    "proxy_protocol" => {
                        proxy_protocol = Some(
                            sub.args
                                .first()
                                .map(|a| a.value.clone())
                                .unwrap_or_else(|| DEFAULT_PROXY_PROTOCOL.to_string()),
                        );
                    }
                    _ => {}
                }
            }
        }

        StepBody::ReverseProxy {
            upstreams,
            lb_policy,
            health,
            dns_refresh_ms,
            passive,
            header_up,
            header_down,
            transport,
            tls_insecure_skip_verify,
            proxy_protocol,
        }
    }

    // ── 站点级指令 ──────────────────────────────────────────────────────────
    fn tls(&mut self, node: &Node) -> TlsConfig {
        let mut cfg = TlsConfig::default();
        match node.args.len() {
            0 => {}
            1 if node.args[0].value == "internal" => cfg.mode = TlsMode::Internal,
            2 => {
                cfg.mode = TlsMode::Manual {
                    cert: node.args[0].value.clone(),
                    key: node.args[1].value.clone(),
                }
            }
            _ => {
                self.diags.push(
                    Diagnostic::error(DiagCode::BAD_ARITY, node.span, "`tls` 的参数不对")
                        .label("写法是 `tls` / `tls internal` / `tls <cert> <key>`")
                        .note("自动 HTTPS 是默认行为，不写 `tls` 就是它"),
                );
            }
        }
        if let Some(block) = &node.block {
            for stmt in block {
                let Stmt::Node(sub) = stmt else {
                    self.err_matcher_here(stmt);
                    continue;
                };
                let spec = match lookup_sub(TLS_SUBS, &sub.name) {
                    Ok(s) => s,
                    Err(hint) => {
                        self.unknown_sub(sub, "tls", hint);
                        continue;
                    }
                };
                if !self.check_sub_args(sub, spec) {
                    continue;
                }
                match sub.name.as_str() {
                    "on_demand" => {
                        cfg.on_demand = sub
                            .args
                            .first()
                            .is_none_or(|a| parse_bool(&a.value) == Some(true))
                    }
                    "ask" => cfg.ask = sub.args.first().map(|a| a.value.clone()),
                    "dns" => {
                        cfg.dns_provider = sub.args.first().map(|a| a.value.clone());
                        // ★ 按供应商分岔：`exec` 的第二个参数是**程序路径**，不是凭据。
                        //   ⚠ 一刀切会让「没有秘密的配置」也撞上权限门（门禁实测抓到）。
                        let is_exec = sub.args.first().map(|a| a.value.as_str()) == Some("exec");
                        cfg.dns_arg = sub.args.get(1).map(|a| {
                            if is_exec {
                                Secret::path(&a.value)
                            } else {
                                Secret::parse(&a.value)
                            }
                        });
                    }
                    "resolvers" => {
                        // ★ ★ ：形状校验搬到这里。
                        //
                        //   在此之前它长在**装载期**，而且失败的处置是
                        //   「打一行 error，本站点的 DNS-01 不启用」——
                        //   ⚠ `validate` 退出码仍然是 0，站点照常起来，
                        //   于是一份写错 `resolvers` 的配置**在每一处都显得正常**，
                        //   直到那张证书永远签不下来。
                        //   ★ 编译期判形状不需要网络，所以它可以在这里判；
                        //   真去解析主机名要网络，那一步留到签发那一刻（见 host.rs 注释）。
                        for a in &sub.args {
                            if let Err(why) = crate::host::parse_resolver(&a.value) {
                                self.diags.push(
                                    Diagnostic::error(
                                        DiagCode::BAD_RESOLVER,
                                        a.span,
                                        format!("`resolvers {}` 写得不对：{why}", a.value),
                                    )
                                    .label("要写成 `<IPv4 或主机名>[:端口]`，不写端口就是 53")
                                    .note(
                                        "★ 主机名是可以的（`drummer.dnspod.net:53`）—— \
                                         它在**每次签发那一刻**解析，而不是钉成启动时的那个 IP：\
                                         anycast 权威的地址是会变的",
                                    ),
                                );
                            }
                        }
                        cfg.resolvers = sub.args.iter().map(|a| a.value.clone()).collect()
                    }
                    "zones" => {
                        cfg.zones = sub
                            .args
                            .iter()
                            .map(|a| a.value.to_ascii_lowercase())
                            .collect()
                    }
                    // ⚠ ⚠ 这一臂**必须响**，不能是 `_ => {}` —— 那样子指令会被**静默丢掉**
                    //   （DSL 认得、`TLS_SUBS` 里有、文档写着，而运行时从来没见过）。
                    //   这里 match 的是 `&str`，语言给不出穷尽性检查 ⇒ 两道：
                    //   ① 走到这里报一条内部错误；② `tests/compile_behaviour.rs` 的
                    //   `tls子指令全部有人接` 逐条枚举 `TLS_SUBS`，判据是**值落到了 `TlsConfig` 上**。
                    // ★ 第 ① 道单独不够：改回 `_ => {}` 之后内部错误根本不会发出，
                    //   于是任何「断言没有内部错误」的门都照常给绿。
                    // ⚠ ★ 而第 ② 道必须真的存在：一条断言「某道门存在」的注释，
                    //   在门不存在时长得和门存在时一模一样，比没有注释更糟。
                    other => self.diags.push(
                        Diagnostic::error(
                            DiagCode::UNKNOWN_SUBDIRECTIVE,
                            sub.name_span,
                            format!("`tls {other}` 在指令表里，但编译器没有接它"),
                        )
                        .note("这是枢衡自己的缺陷，不是配置写错了；请连同这份配置一起报告"),
                    ),
                }
            }
        }
        // ★ G15：On-Demand 没配准入就拒绝启动。这里在**编译期**就把它挡下——
        //   「错误在启动时暴露，不等被滥用才发现」，而编译期比启动更早。
        if cfg.on_demand && cfg.ask.is_none() {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_ARITY,
                    node.name_span,
                    "开了 On-Demand TLS 却没配准入端点",
                )
                .label("`on_demand` 必须和 `ask` 一起写")
                .note("G15：没有准入控制的 On-Demand 会被任意 SNI 刷爆，并把 CA 速率配额耗光"),
            );
        }
        // ★ ★ G58：配了 DNS-01 就必须配权威 NS。形状照 G15——**编译期拒绝**。
        //
        //   G58 写死了「确认 TXT 可见必须真去问权威 NS，绝不能只 sleep 一个固定秒数」。
        //   ⚠ 少了 `resolvers`，实现要么退回固定 sleep（G58 明确禁止），
        //   要么在签发中途才报错——而那时已经在 CA 那边开了一张订单、
        //   并且往 DNS 上写了一条记录。**在编译期挡下，代价只是一条诊断。**
        if cfg.dns_provider.is_some() && cfg.resolvers.is_empty() {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_ARITY,
                    node.name_span,
                    "配了 DNS-01 却没说去问哪些权威 NS",
                )
                .label("`dns` 必须和 `resolvers` 一起写")
                .note(
                    "G58：TXT 写上去不等于可见，必须向权威 NS 轮询确认；\
                     固定 sleep 在快时浪费时间、在慢时直接签失败，而失败要消耗 CA 的速率配额",
                ),
            );
        }
        // ── ★ ★ G59：原生供应商的两条硬约束，都在**编译期**挡下（形状照 G15）─────
        //
        // ⚠ 理由不是洁癖：拿到某域的 DNS 写权限 = **能为该域签发任意证书**，
        //   还能改 MX 劫持邮件。它比 On-Demand 被刷爆严重得多。
        if matches!(
            cfg.dns_provider.as_deref(),
            Some("cloudflare") | Some("dnspod")
        ) {
            let provider = cfg.dns_provider.clone().unwrap_or_default();
            // G59 第 1 条：凭据绝不写进 DSL。只认 `env:` / `file:` 两种**来源**写法。
            //
            // ★ ★ 判据是**白名单**，不是「看起来像 token 就报错」的黑名单——
            //   后者要去猜什么样子算 token，而猜错的那一次恰恰就是真 token 被放行的那一次。
            match cfg.dns_arg.as_ref() {
                None => self.diags.push(
                    Diagnostic::error(
                        DiagCode::BAD_ARITY,
                        node.name_span,
                        format!("`dns {provider}` 没说凭据从哪儿来"),
                    )
                    .label("写成 `dns cloudflare env:变量名` 或 `dns dnspod file:路径`")
                    .note(
                        "G59 第 1 条：凭据绝不写进 DSL —— DSL 是要被 diff、\
                         被贴进 issue、被版本控制的东西",
                    ),
                ),
                // ★ ★ ★ **口径变了**（owner 拍板）：字面量凭据**写得下**了 ——
                //   一份配置文件就能跑完，形状照 Caddy。
                //   ⚠ 而剩下的这一条判据比原来那条更难写对：既然不写前缀就是字面量，
                //   那么**一个写错的前缀**（`fil:/path`、`ENV:NAME`）就会被当成凭据本身，
                //   然后带着一个根本不是凭据的字符串去打 CA —— 现场是「凭据不对」，
                //   而真正的原因是打错了三个字母。
                //   ⇒ 判据：**长得像来源前缀、而那个前缀我们不认识** ⇒ 编译期报错，
                //   并把「如果这真的就是凭据本身」的出路（`literal:`）写在提示里。
                Some(a) => {
                    let looks_like_prefix = a
                        .expose()
                        .split_once(':')
                        .map(|(head, _)| {
                            !head.is_empty()
                                && head
                                    .chars()
                                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                        })
                        .unwrap_or(false);
                    let known = a.expose().starts_with("env:")
                        || a.expose().starts_with("file:")
                        || a.expose().starts_with("literal:");
                    if looks_like_prefix && !known {
                        let head = a.expose().split_once(':').map(|(h, _)| h).unwrap_or("");
                        self.diags.push(
                            Diagnostic::error(
                                DiagCode::BAD_CREDENTIAL_SOURCE,
                                node.name_span,
                                format!("不认识的凭据来源前缀 `{head}:`"),
                            )
                            .label("认得的是 `env:变量名`、`file:路径`，或者直接写值")
                            .note(
                                "⚠ 不写前缀就是**值本身**（Caddy 形状）；\
                                 而一个打错的前缀会被当成值发给对端，\
                                 现场表现是「凭据不对」而真正的原因是打错了几个字母。\
                                 ★ 如果这真的就是凭据本身且它带冒号，写成 `literal:<值>`",
                            ),
                        );
                    }
                }
            }
            // G59 第 3 条：必须显式声明这份凭据覆盖哪些 zone。
            //
            // ★ 对 DNSPod 这是唯一的范围约束（它的 token 是账号级的，没有可问的端点）；
            //   对 Cloudflare 它与启动时校验（第 2 条）叠加。⚠ 两家都要求，
            //   比 G59 字面更严一点——统一一条规则，比「哪家要哪家不要」好记也好审。
            if cfg.zones.is_empty() {
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::BAD_ARITY,
                        node.name_span,
                        format!("`dns {provider}` 没有声明这份凭据覆盖哪些 zone"),
                    )
                    .label("`dns cloudflare|dnspod` 必须和 `zones …` 一起写")
                    .note(
                        "G59 第 3 条：超出声明范围一律拒绝，把「凭据能干什么」\
                         变成配置里可读的事实。⚠ 拿到某域的 DNS 写权限 = \
                         能为该域签发任意证书，还能改 MX 劫持邮件",
                    ),
                );
            }
        }

        // ★ `dns exec` 不给程序路径 = 没人去改 TXT。同样在编译期挡下。
        if cfg.dns_provider.as_deref() == Some("exec") && cfg.dns_arg.is_none() {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_ARITY,
                    node.name_span,
                    "`dns exec` 没给要执行的程序",
                )
                .label("写成 `dns exec /path/to/hook`")
                .note("G57：其余服务商走 exec hook —— 签发时会以 `<程序> set|clear <记录名> <值>` 调用它"),
            );
        }
        cfg
    }

    fn log(&mut self, node: &Node) -> LogConfig {
        let subs = self.sub_block(node, LOG_SUBS);
        LogConfig {
            output: subs.get("output").cloned(),
            level: subs.get("level").cloned(),
            headers: self.header_whitelist(node, &subs, "headers"),
            resp_headers: self.header_whitelist(node, &subs, "resp_headers"),
        }
    }

    /// `log { headers … }` / `log { resp_headers … }` 的白名单（**M2 批 L 第 ③ 步**）。
    ///
    /// ⚠ **值不从 `subs` 里取**（[`Cx::sub_block`] 把参数用空格连成一串）：
    /// ① 拆回来的东西没有位置，而这一格要报一条**指着某一个名字**的诊断；
    /// ② 「连起来再拆开」在参数里出现空格的那天会静静地错。
    /// ⚠ 但**收不收**仍以 `subs` 为准 —— 参数个数不对时它已被 `check_sub_args` 拦掉。
    fn header_whitelist(
        &mut self,
        node: &Node,
        subs: &BTreeMap<String, String>,
        which: &str,
    ) -> Vec<String> {
        if !subs.contains_key(which) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let Some(block) = node.block.as_ref() else {
            return out;
        };
        for stmt in block {
            let Stmt::Node(sub) = stmt else { continue };
            if sub.name != which {
                continue;
            }
            for a in &sub.args {
                // ★ ★ ★ 四个名字是**编译期错误**（G114 那一半没被推翻的理由）。
                //   ⚠ 大小写不敏感：HTTP 头名本来就不区分大小写，
                //     一条「写 `Cookie` 会红、写 `cookie` 不会」的规则等于没有规则。
                if SENSITIVE_HEADERS.contains(&a.value.to_ascii_lowercase().as_str()) {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::SENSITIVE_HEADER_LOGGED,
                            a.span,
                            format!("`log {{ {which} … }}` 里不许写 `{}`", a.value),
                        )
                        .label("这个头带的是凭据，不是可观测信息")
                        .note(
                            "★ 不许记的四个：`Authorization` · `Cookie` · `Set-Cookie` · \
                             `Proxy-Authorization`（大小写不敏感）。\
                             ⚠ 取「编译期拒绝」而不是「运行时脱敏」：后者要求脱敏表跟得上\
                             每一个新的敏感头名，而**漏一个就是一次静默泄漏**；\
                             前者的失效形态是「这份配置装不上」，当场可见",
                        ),
                    );
                    continue;
                }
                out.push(a.value.clone());
            }
        }
        out
    }

    fn handle_errors(&mut self, node: &Node, matchers: &BTreeMap<String, Matcher>) -> Vec<Step> {
        let Some(block) = &node.block else {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BLOCK_MISMATCH,
                    node.name_span,
                    "`handle_errors` 后面要跟一个 `{ … }` 块",
                )
                .label("这里缺一个块"),
            );
            return Vec::new();
        };
        let inner: Vec<&Node> = block
            .iter()
            .filter_map(|s| match s {
                Stmt::Node(n) => Some(n),
                Stmt::Matcher(m) => {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::BAD_PLACEMENT,
                            m.span,
                            "匹配器要定义在站点块的顶层",
                        )
                        .label("这里不能定义匹配器"),
                    );
                    None
                }
            })
            .collect();
        // ★ 块内一律按 `ErrorHandler` 上下文校验占位符——`{status}` 只在这里可用。
        // ⚠ `guarded = false`：`handle_errors` 是**按状态码**进来的，不是按匹配器 ——
        //   它对「谁能连到这个端口」一个字都没说。★ 把它算成一层保护，等于让
        //   一句 `handle_errors { metrics }` 悄悄躲开 G116 那条诊断。
        self.chain(&inner, matchers, Ctx::ErrorHandler, true, false)
    }

    // ── L4（M2 自研，M1 编译成回落）─────────────────────────────────────────
    fn l4(&mut self, node: &Node) -> L4Config {
        let mut listeners = Vec::new();
        if let Some(block) = &node.block {
            for stmt in block {
                let Stmt::Node(sub) = stmt else {
                    self.err_matcher_here(stmt);
                    continue;
                };
                if sub.name != "tcp" && sub.name != "udp" {
                    let mut d = Diagnostic::error(
                        DiagCode::UNKNOWN_SUBDIRECTIVE,
                        sub.name_span,
                        format!("`l4` 块里没有 `{}`", sub.name),
                    )
                    .label("只能是 `tcp` 或 `udp`")
                    .note("L4 面的写法见 docs/architecture/dsl-reference.md §4.5");
                    if let Some(h) = crate::diag::suggest(&sub.name, ["tcp", "udp"]) {
                        d = d.help(format!("你是不是想写 `{h}`？"));
                    }
                    self.diags.push(d);
                    continue;
                }
                let Some(listen) = sub.args.first() else {
                    self.arity_error(sub, "写法是 `tcp :3306 { proxy … }`");
                    continue;
                };
                let mut upstreams = Vec::new();
                let mut rules: Vec<L4Rule> = Vec::new();
                let mut pp_from: Vec<String> = Vec::new();
                let mut pp_send: Option<String> = None;
                if let Some(inner) = &sub.block {
                    for s in inner {
                        let Stmt::Node(p) = s else {
                            self.err_matcher_here(s);
                            continue;
                        };
                        match p.name.as_str() {
                            "proxy" => {
                                upstreams.extend(p.args.iter().map(|a| a.value.clone()));
                            }
                            // ★ ★ M2 批 C：`sni` / `alpn` 嵌套匹配块（owner 拍板取的形状）。
                            //   `sni api.example.com *.internal { proxy 10.0.0.1:8443 }`
                            //   ⚠ 只有 `tcp` 有这两条：UDP 上没有 ClientHello，
                            //   而 QUIC 的 Initial 是**加密**的 —— 那不是「以后再做」，
                            //   是**做不到**，所以这里要说清楚，不能只说「不支持」。
                            "sni" | "alpn" => {
                                if sub.name == "udp" {
                                    self.diags.push(
                                        Diagnostic::error(
                                            DiagCode::BAD_PLACEMENT,
                                            p.name_span,
                                            format!("`udp` 块里不能有 `{}`", p.name),
                                        )
                                        .label("这一条只在 `tcp` 里有意义")
                                        .note(
                                            "UDP 上没有 TLS ClientHello 可看；QUIC 的 Initial 是加密的，\
                                             分流不到那一层",
                                        ),
                                    );
                                    continue;
                                }
                                if p.args.is_empty() {
                                    self.arity_error(
                                        p,
                                        "写法是 `sni <名字…> { proxy … }` 或 `alpn <协议…> { proxy … }`",
                                    );
                                    continue;
                                }
                                let mut rule_ups = Vec::new();
                                if let Some(rule_block) = &p.block {
                                    for rs in rule_block {
                                        let Stmt::Node(rp) = rs else {
                                            self.err_matcher_here(rs);
                                            continue;
                                        };
                                        if rp.name != "proxy" {
                                            self.diags.push(
                                                Diagnostic::error(
                                                    DiagCode::UNKNOWN_SUBDIRECTIVE,
                                                    rp.name_span,
                                                    format!("`{}` 块里没有 `{}`", p.name, rp.name),
                                                )
                                                .label("只能是 `proxy`"),
                                            );
                                            continue;
                                        }
                                        rule_ups.extend(rp.args.iter().map(|a| a.value.clone()));
                                    }
                                } else {
                                    self.arity_error(p, "它后面要跟一个 `{ proxy … }` 块");
                                    continue;
                                }
                                if rule_ups.is_empty() {
                                    self.arity_error(p, "块里至少要有一条 `proxy <上游>`");
                                    continue;
                                }
                                rules.push(L4Rule {
                                    kind: p.name.clone(),
                                    values: p.args.iter().map(|a| a.value.clone()).collect(),
                                    upstreams: rule_ups,
                                });
                            }
                            // ── M2 批 D：PROXY protocol ─────────────────────
                            //
                            // ⚠ ⚠ **两条都只在 `tcp` 上有意义，而这不是「以后再做」。**
                            //   PROXY protocol 是**面向连接**的：头在连接开头发一次。
                            //   UDP 上没有「连接开头」—— 每个数据报都是独立的，
                            //   「只发一次」没有落点，「每个都发」不是这个协议规定的东西。
                            //   ★ 与批 C 的 `sni` / `alpn` 同一条纪律：诊断要说出**为什么**，
                            //     只说「不支持」会让人以为等等就有了。
                            "proxy_protocol_from" | "proxy_protocol" => {
                                if sub.name == "udp" {
                                    self.diags.push(
                                        Diagnostic::error(
                                            DiagCode::BAD_PLACEMENT,
                                            p.name_span,
                                            format!("`udp` 块里不能有 `{}`", p.name),
                                        )
                                        .label("这一条只在 `tcp` 里有意义")
                                        .note(
                                            "PROXY protocol 是面向连接的：头在连接开头发一次。\
                                             UDP 上没有连接开头，每个数据报都是独立的",
                                        ),
                                    );
                                    continue;
                                }
                                if p.name == "proxy_protocol_from" {
                                    if p.args.is_empty() {
                                        self.arity_error(
                                            p,
                                            "写法是 `proxy_protocol_from <网段…>`，如 `proxy_protocol_from 10.0.0.0/8`",
                                        );
                                        continue;
                                    }
                                    // ★ 网段本身的合法性在**运行时图**那一层校验
                                    //   （与 `remote_ip` 共用 `Cidr::parse`，见 G50 那条
                                    //   「结构化配置是公开入口，那一层也要校验」）——
                                    //   这里只收字面量，不另写一份解析。
                                    pp_from.extend(p.args.iter().map(|a| a.value.clone()));
                                } else {
                                    let v = p
                                        .args
                                        .first()
                                        .map(|a| a.value.clone())
                                        .unwrap_or_else(|| DEFAULT_PROXY_PROTOCOL.to_string());
                                    if !PROXY_PROTOCOL_VERSIONS.contains(&v.as_str()) {
                                        self.diags.push(
                                            Diagnostic::error(
                                                DiagCode::BAD_ENUM,
                                                p.name_span,
                                                format!(
                                                    "`proxy_protocol` 的版本只能是 {}，写的是 `{v}`",
                                                    PROXY_PROTOCOL_VERSIONS
                                                        .iter()
                                                        .map(|s| format!("`{s}`"))
                                                        .collect::<Vec<_>>()
                                                        .join(" / ")
                                                ),
                                            )
                                            .label(format!(
                                                "省略参数就是 `{DEFAULT_PROXY_PROTOCOL}`"
                                            )),
                                        );
                                        continue;
                                    }
                                    pp_send = Some(v);
                                }
                            }
                            other => {
                                let mut d = Diagnostic::error(
                                    DiagCode::UNKNOWN_SUBDIRECTIVE,
                                    p.name_span,
                                    format!("`{}` 块里没有 `{other}`", sub.name),
                                )
                                .label(if sub.name == "tcp" {
                                    "只能是 `proxy` / `sni` / `alpn` / `proxy_protocol_from` / `proxy_protocol`"
                                } else {
                                    "只能是 `proxy`"
                                });
                                let known: &[&str] = if sub.name == "tcp" {
                                    &[
                                        "proxy",
                                        "sni",
                                        "alpn",
                                        "proxy_protocol_from",
                                        "proxy_protocol",
                                    ]
                                } else {
                                    &["proxy"]
                                };
                                if let Some(h) = crate::diag::suggest(other, known.iter().copied())
                                {
                                    d = d.help(format!("你是不是想写 `{h}`？"));
                                }
                                self.diags.push(d);
                            }
                        }
                    }
                }
                // ⚠ 兜底可以没有（「只服务我认得的那几个名字」），但**总得有人能接**：
                //   既没有兜底又没有规则的监听器，接受连接之后只能立刻关掉。
                if upstreams.is_empty() && rules.is_empty() {
                    self.arity_error(
                        sub,
                        "至少要有一条 `proxy <上游>`，或者一条 `sni` / `alpn` 规则",
                    );
                }
                listeners.push(L4Listener {
                    proto: sub.name.clone(),
                    listen: listen.value.clone(),
                    upstreams,
                    rules,
                    proxy_protocol_from: pp_from,
                    proxy_protocol: pp_send,
                });
            }
        }
        // ★ ★ ★ M2 批 B：**`l4` 不再产生任何回落标记** ——
        //   TCP（批 A）与 UDP（批 B）都自研完了，`L4Config` 里那一格也随之删掉。
        //   ⚠ 这改的是 `compile` 的 JSON 产物（G11/G48 的公开契约），
        //   dsl-reference §4.5 的状态那行跟着改，两边都有测试盯着。
        L4Config { listeners }
    }

    // ── 公共小工具 ──────────────────────────────────────────────────────────

    /// 解析匹配器位（G50）。
    fn matcher_ref(
        &mut self,
        node: &Node,
        matchers: &BTreeMap<String, Matcher>,
    ) -> Option<MatcherRef> {
        let arg = node.matcher_arg()?;
        if let Some(name) = arg.value.strip_prefix('@') {
            if !matchers.contains_key(name) {
                let mut d = Diagnostic::error(
                    DiagCode::UNKNOWN_MATCHER,
                    arg.span,
                    format!("没有定义过匹配器 `@{name}`"),
                )
                .label("先在站点块里写 `@name { … }`")
                .note("匹配器写法见 docs/architecture/dsl-reference.md §五");
                if let Some(h) = crate::diag::suggest(name, matchers.keys().map(String::as_str)) {
                    d = d.help(format!("你是不是想写 `@{h}`？"));
                }
                self.diags.push(d);
                return None;
            }
            return Some(MatcherRef::Named(name.to_string()));
        }
        Some(MatcherRef::Path(arg.value.clone()))
    }

    /// 一个指令后面不该跟块，跟了就报。
    fn expect_no_block(&mut self, node: &Node, d: ChainDirective) {
        if node.block.is_some() {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BLOCK_MISMATCH,
                    node.name_span,
                    format!("`{}` 不接受 `{{ … }}` 子块", d.name()),
                )
                .label("这条指令的参数写在同一行"),
            );
        }
    }

    fn arity_error(&mut self, node: &Node, hint: &str) {
        let mut d = Diagnostic::error(
            DiagCode::BAD_ARITY,
            node.name_span,
            format!("`{}` 的参数不对", node.name),
        )
        .label(hint.to_string());
        // ★ ★ 这条 help 挡的是一个真实的死胡同：`rewrite /new/x` 里那个 `/new/x`
        //   会被「第一个以 `/` 开头的参数就是匹配器」的规则吃掉，于是参数看起来少了一个，
        //   而报出来的只是一句「参数不对」——**看不出真正该怎么写**。
        if node.rest_args().is_empty()
            && let Some(m) = node.matcher_arg()
            && !m.value.starts_with('@')
            && m.value != "*"
        {
            d = d.help(format!(
                "`{}` 被当成了**行内匹配器**（G50：第一个以 `/` 开头的参数是匹配器）。\
                 如果它其实是这条指令的取值，用 `*` 显式占住匹配器位：`{} * {}`",
                m.value, node.name, m.value
            ));
        }
        self.diags.push(d);
    }

    fn exactly_one<'b>(&mut self, node: &Node, args: &'b [Arg], what: &str) -> Option<&'b Arg> {
        if args.len() != 1 {
            self.arity_error(node, &format!("正好要一个参数：{what}"));
            return None;
        }
        Some(&args[0])
    }

    fn unknown_sub(&mut self, sub: &Node, parent: &str, hint: Option<&'static str>) {
        let mut d = Diagnostic::error(
            DiagCode::UNKNOWN_SUBDIRECTIVE,
            sub.name_span,
            format!("`{parent}` 的子块里没有 `{}`", sub.name),
        )
        .label("未知的子指令")
        .note("子块清单见 docs/architecture/dsl-reference.md §四");
        if let Some(h) = hint {
            d = d.help(format!("你是不是想写 `{h}`？"));
        }
        self.diags.push(d);
    }

    /// 按规格校验一条子指令的参数个数与类型。返回 `false` 表示这条不可用。
    fn check_sub_args(&mut self, node: &Node, spec: &SubSpec) -> bool {
        if node.args.len() < spec.min_args || node.args.len() > spec.max_args {
            let expect = if spec.min_args == spec.max_args {
                format!("要 {} 个参数", spec.min_args)
            } else if spec.max_args == usize::MAX {
                format!("至少要 {} 个参数", spec.min_args)
            } else {
                format!("要 {}–{} 个参数", spec.min_args, spec.max_args)
            };
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_ARITY,
                    node.name_span,
                    format!("`{}` 的参数个数不对", node.name),
                )
                .label(format!("{expect}，实际给了 {}", node.args.len())),
            );
            return false;
        }
        let mut ok = true;
        // 类型只校验第一个参数——多参的那几条（header_up/down、index）第一个是名字，
        // 后面是值，逐个套同一个类型没有意义。
        if let Some(a) = node.args.first()
            && let Err((code, msg)) = check_arg_type(spec.arg_type, &a.value)
        {
            self.diags.push(
                Diagnostic::error(code, a.span, format!("`{}` 的取值不合法", node.name)).label(msg),
            );
            ok = false;
        }
        ok
    }

    /// 把一个子块读成「名字 → 全部参数用空格连起来」的表。
    fn sub_block(&mut self, node: &Node, table: &'static [SubSpec]) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(block) = &node.block else {
            return out;
        };
        for stmt in block {
            let Stmt::Node(sub) = stmt else {
                self.err_matcher_here(stmt);
                continue;
            };
            let spec = match lookup_sub(table, &sub.name) {
                Ok(s) => s,
                Err(hint) => {
                    self.unknown_sub(sub, &node.name, hint);
                    continue;
                }
            };
            if !self.check_sub_args(sub, spec) {
                continue;
            }
            out.insert(
                sub.name.clone(),
                sub.args
                    .iter()
                    .map(|a| a.value.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        out
    }

    /// `cache { disk … }` 的两道检查（M2 批 H）。
    ///
    /// ① 必须绝对路径（G91 那条理由一字不改地适用：相对路径按进程 cwd 解析，
    ///    而 systemd 下 cwd 是 `/`、开发机上是项目目录 ⇒ 同一份配置指向两个地方）。
    /// ② ★ ★ **全进程只有一个缓存后端**，所以多个 `cache` 块必须选同一个。
    fn check_cache_backend(&mut self, node: &Node, disk_dir: &Option<String>) {
        if let Some(d) = disk_dir
            && !d.starts_with('/')
        {
            let span = sub_arg_span(node, "disk").unwrap_or(node.name_span);
            self.diags.push(
                Diagnostic::error(
                    DiagCode::PATH_NOT_ABSOLUTE,
                    span,
                    format!("`cache` 的 `disk` 必须是绝对路径，`{d}` 不是"),
                )
                .label("相对路径按进程 cwd 解析")
                .note(
                    "而 systemd 下 cwd 是 `/`、开发机上是项目目录 —— \
                     同一份配置会把缓存落到两个地方，而两处都「看起来正常」",
                ),
            );
        }
        // ⚠ 只有**真收下了** `disk` 才指着那个参数：写了 `disk` 但参数个数不对时
        //   它已经被 `check_sub_args` 拦掉、不在 `subs` 里，这时这个块选的其实是内存，
        //   指着那行反而是在说一件不成立的事。
        let here = match disk_dir {
            Some(_) => sub_arg_span(node, "disk").unwrap_or(node.name_span),
            None => node.name_span,
        };
        match &self.cache_backend {
            None => self.cache_backend = Some((disk_dir.clone(), here)),
            // 一样就一样，什么都不用说。
            Some((first, _)) if first == disk_dir => {}
            Some((first, _)) => {
                let (a, b) = (describe_backend(first), describe_backend(disk_dir));
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::CACHE_BACKEND_CONFLICT,
                        here,
                        format!("这份配置里的 `cache` 块选了两个不同的后端：{a} 与 {b}"),
                    )
                    .label("这一条与前面那条不一样")
                    .note(
                        "★ 缓存后端是**进程级**的（整个进程只有一份存储），\
                         所以两个不同的值里必有一个是「你以为生效、其实没有」的。\
                         ⇒ 把它们改成同一个；真要分开存，起两个 fulcrum 进程",
                    ),
                );
            }
        }
    }

    /// `header` 指令：行内一条，或子块里多条。
    fn header_ops(&mut self, node: &Node, args: &[Arg], ctx: Ctx) -> Vec<HeaderOp> {
        let mut ops = Vec::new();
        if !args.is_empty()
            && let Some(op) = self.header_op(args, ctx)
        {
            ops.push(op);
        }
        if let Some(block) = &node.block {
            for stmt in block {
                let Stmt::Node(sub) = stmt else {
                    self.err_matcher_here(stmt);
                    continue;
                };
                // 子块里每一行的形状是 `[+-]<name> [value]`，指令名本身就是头名。
                let mut all = vec![Arg {
                    value: sub.name.clone(),
                    span: sub.name_span,
                    quoted: false,
                }];
                all.extend(sub.args.iter().cloned());
                if let Some(op) = self.header_op(&all, ctx) {
                    ops.push(op);
                }
            }
        }
        if ops.is_empty() {
            self.arity_error(node, "写法是 `header [matcher] [+-]<name> [value]`");
        }
        ops
    }

    fn header_op(&mut self, args: &[Arg], ctx: Ctx) -> Option<HeaderOp> {
        let name_arg = args.first()?;
        let raw = &name_arg.value;
        let (op, name) = if let Some(rest) = raw.strip_prefix('+') {
            ("add", rest)
        } else if let Some(rest) = raw.strip_prefix('-') {
            ("remove", rest)
        } else {
            ("set", raw.as_str())
        };
        if name.is_empty() {
            self.diags.push(
                Diagnostic::error(DiagCode::BAD_ARITY, name_arg.span, "头名是空的")
                    .label("`+`/`-` 后面要跟头名"),
            );
            return None;
        }
        if op == "remove" && args.len() > 1 {
            self.diags.push(
                Diagnostic::error(DiagCode::BAD_ARITY, args[1].span, "删除某个头时不写取值")
                    .label("多余的参数"),
            );
        }
        if let Some(v) = args.get(1) {
            self.check_placeholders(v, ctx);
        }
        Some(HeaderOp {
            op: op.to_string(),
            name: name.to_string(),
            value: args.get(1).map(|a| a.value.clone()),
        })
    }

    fn check_placeholders(&mut self, arg: &Arg, ctx: Ctx) {
        let mut out = Vec::new();
        placeholder::check(&arg.value, arg.span, ctx, &mut out);
        for d in out {
            self.diags.push(d);
        }
    }

    fn err_matcher_here(&mut self, stmt: &Stmt) {
        if let Stmt::Matcher(m) = stmt {
            self.diags.push(
                Diagnostic::error(DiagCode::BAD_PLACEMENT, m.span, "这里不能定义匹配器")
                    .label("匹配器只能定义在站点块的顶层"),
            );
        }
    }

    /// ★ ★ G49 配套第 4 条：**诊断必须能解释「它实际跑在第几步」**。
    ///
    /// 内建顺序表把书写顺序和执行顺序拆开之后，最容易出现的意外就是
    /// 「我明明写在后面的兜底，怎么先跑了」。这里在编译期就把它说出来。
    fn warn_unreachable(&mut self, steps: &[(Step, Span)]) {
        let mut blocker: Option<(&'static str, u16)> = None;
        for (step, span) in steps {
            let name = step.body.directive_name();
            if let Some((prev_name, prev_order)) = blocker {
                self.diags.push(
                    Diagnostic::warning(
                        DiagCode::UNREACHABLE_STEP,
                        *span,
                        format!("`{name}` 永远不会执行"),
                    )
                    .label(format!(
                        "它排在执行顺序表第 {} 步，而第 {prev_order} 步的 `{prev_name}` 没有匹配器、会先终结请求",
                        step.order
                    ))
                    .note(
                        "★ 站点块内按**内建顺序表**执行，不按书写顺序（G49），顺序表见 \
                         docs/architecture/dsl-reference.md §三。\
                         要让这一条有机会执行：给前面那条兜底加一个匹配器，\
                         或者把两条放进同一个 `route { … }`（块内按书写顺序）",
                    ),
                );
                continue;
            }
            if terminates_unconditionally(step) {
                blocker = Some((name, step.order));
            }
        }
    }
}

/// 这一步会不会**无条件**终结请求——即它之后的任何一步都再也轮不到。
///
/// ★ 「无条件」有两层：这一步自己没有匹配器，**并且**它真的会终结。
/// 容器要递归看：`route { respond 200 }` 终结，`route { respond /x 200 }` 不终结
/// （里面那条带匹配器）；`handle` 组只有**兜底分支**里终结才算。
///
/// ⚠ 只认「终结类指令 + 带兜底分支的 handle 组」是不够的 —— `handle { … 兜底 … }`
/// 后面跟一个 `route { … }` 时会一声不吭，而那个 route 永远进不去。
/// ★ **判据只认得一种形状**是本仓库反复抓到的那类缺陷。
fn terminates_unconditionally(step: &Step) -> bool {
    if !is_unconditional(&step.matcher) {
        return false;
    }
    match &step.body {
        StepBody::Route { steps } => steps.iter().any(terminates_unconditionally),
        StepBody::Handle { arms } => arms.iter().any(|a| {
            is_unconditional(&a.matcher) && a.steps.iter().any(terminates_unconditionally)
        }),
        other => ChainDirective::from_name(other.directive_name())
            .is_some_and(|d| d.kind() == Kind::Terminal),
    }
}

/// 「没有匹配器」与「匹配器是 `*`」是同一件事。
///
/// ⚠ 只认前者的话，`redir * /x` 不算兜底，它后面那条永远跑不到的指令
/// **一声不吾** —— 而 `*` 正是文档教用户写的那个写法。
fn is_unconditional(m: &Option<MatcherRef>) -> bool {
    match m {
        None => true,
        Some(MatcherRef::Path(p)) => p == "*",
        Some(MatcherRef::Named(_)) => false,
    }
}

/// 一条匹配条件**减少得了「谁能碰到这一步」吗**（只服务于 G116 那条诊断）。
///
/// ★ ★ 判据是一句可验证的问话：**它能不能把两个发同样请求的客户端分开？**
/// - `remote_ip` 能 —— 它读的是 socket 对端，在这一层伪造不了。
/// - `header` 能 —— 一个共享密钥可以放在这里。
/// - `path` / `path_regexp` / `host` / `method` / `query` **都不能**：
///   它们全都是请求行与请求头里的东西，**任何客户端都能照着发一份**
///   ⇒ 它们只决定端点摆在哪，不减少能碰到它的人。
///
/// ⚠ ⚠ `not` **不会出现在这张表要比的位置上**：`condition()` 是把 `not { … }`
/// 拆开、给里面每一条打上 `negate` 之后再存的，所以 `not { remote_ip … }` 存下来的
/// `kind` 就是 `remote_ip`。⇒ 按 `kind` 比对时它照样算保护，**不需要为 `not` 写一条特例**。
const SOURCE_CONDITIONS: &[&str] = &["remote_ip", "header"];

/// 这个匹配器位**限制得了来源吗**（G116 那条诊断专用）。
///
/// ⚠ ⚠ **它与 [`is_unconditional`] 问的是两件不同的事，别合并**：
/// 那一条问「这一步会不会把它后面的都吃掉」（`warn_unreachable` 用），
/// 这一条问「这一步对谁开着」。两条判据合用一个谓词，就是让它们将来一起漂走。
///
/// ★ 只有**命名匹配器**才可能算数：行内匹配器按 G50 只能是路径，而路径不限制来源。
/// ⚠ 查不到那个名字（引用了没定义过的 `@name`）当作**不算保护** ——
/// 那条路上 `matcher_ref` 已经报过 `UNKNOWN_MATCHER` 了，这里不必再说一遍；
/// ★ 而在「不确定」时倒向报警告，是这条诊断唯一安全的那一侧。
fn restricts_source(m: &Option<MatcherRef>, matchers: &BTreeMap<String, Matcher>) -> bool {
    let Some(MatcherRef::Named(name)) = m else {
        return false;
    };
    matchers.get(name).is_some_and(|m| {
        m.conditions
            .iter()
            .any(|c| SOURCE_CONDITIONS.contains(&c.kind.as_str()))
    })
}

/// 响应侧上下文：在 `handle_errors` 里就还是 `handle_errors`。
fn resp_ctx(base: Ctx) -> Ctx {
    match base {
        Ctx::ErrorHandler => Ctx::ErrorHandler,
        _ => Ctx::Response,
    }
}

/// 曾经存在、现在删掉了的全局选项 → 一句「它去哪了」。
///
/// ★ ★ 这张表**只增不删**：一条选项被删掉之后，写过它的配置文件不会跟着消失。
/// ⚠ 把它从这里拿走，那些配置就退回到「未知的全局选项」——
/// 一句听起来像「你打错字了」的话，而那写法曾经是对的。
fn removed_global_option(name: &str) -> Option<&'static str> {
    match name {
        "fallback_nginx" | "fallback_caddy" => Some(
            "回落层（M2 批 G）整块删除：
             §6.3 那个过渡层的三个用户都自研完了 —— `l4`（批 B）、`file_server`（批 F）、
             `cache`（批 G）⇒ 一个都不剩。⇒ **把这一行删掉即可**，
             那两个后端进程也可以停掉了（枢衡从不拉起、不监管它们，G34）。",
        ),
        _ => None,
    }
}

/// 站点地址解析（DSL 参考 §二）。
pub fn parse_address(raw: &str) -> Result<Address, &'static str> {
    let (scheme_given, rest) = if let Some(r) = raw.strip_prefix("https://") {
        (Some("https"), r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        (Some("http"), r)
    } else if raw.contains("://") {
        return Err("只认 `http://` 与 `https://` 两种前缀");
    } else {
        (None, raw)
    };

    if rest.is_empty() {
        return Err("地址是空的");
    }
    if rest.contains('/') {
        return Err("站点地址里不能带路径——路径用匹配器表达");
    }
    // ★ IPv6 字面量要**在拆端口之前**判掉。`[::1]` 里的冒号会被 `rsplit_once(':')`
    //   当成端口分隔符，于是用户收到的是一句「端口要是 1–65535 之间的整数」——
    //   **指向一个根本不存在的问题**。
    //   ⚠ 不支持它是**已知限制**，不是疏漏：DSL 参考 §二把支持的写法列全了，里面没有它。
    if rest.contains('[') || rest.contains("::") {
        return Err("M1 还不支持 IPv6 字面量地址（DSL 参考 §二列全了支持的写法）");
    }

    let (host, port_str) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };

    let port = match port_str {
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => Some(n),
            _ => return Err("端口要是 1–65535 之间的整数"),
        },
        None => None,
    };

    let wildcard = host.starts_with("*.");
    let bare = host.strip_prefix("*.").unwrap_or(host);
    if !bare.is_empty() && !valid_hostname(bare) {
        return Err("主机名只能由字母、数字、`-` 与 `.` 组成");
    }
    if host.is_empty() && port.is_none() {
        return Err("至少要写主机名或端口");
    }

    // ★ 没有主机名就签不出证书，所以 `:8080` 这种一律是 HTTP、不自动升级。
    //   这不是取舍，是 ACME 的事实。
    let scheme = scheme_given.unwrap_or(if host.is_empty() { "http" } else { "https" });
    let auto_https = scheme == "https" && !host.is_empty();
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });

    Ok(Address {
        raw: raw.to_string(),
        scheme: scheme.to_string(),
        host: host.to_string(),
        port,
        wildcard,
        auto_https,
    })
}

fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !h.starts_with('.')
        && !h.ends_with('.')
}

/// 从一个子块里取某条子指令**第一个参数**的 span。
///
/// ⚠ [`Cx::sub_block`] 只吐值不吐位置，而**一条指错地方的诊断比没有诊断更费时间**
/// —— 人会先去看它指的那一行，而那一行与这件事无关。
fn sub_arg_span(node: &Node, name: &str) -> Option<Span> {
    node.block.as_ref()?.iter().find_map(|s| match s {
        Stmt::Node(n) if n.name == name => n.args.first().map(|a| a.span),
        _ => None,
    })
}

/// 把一个缓存后端选择说成人话（给 `FUL-DSL-0035` 的标题行用）。
fn describe_backend(d: &Option<String>) -> String {
    match d {
        Some(dir) => format!("磁盘 `{dir}`"),
        None => "内存（没写 `disk`）".to_string(),
    }
}
