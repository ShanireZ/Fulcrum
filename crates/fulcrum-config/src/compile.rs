//! 語義分析 + 編譯：AST → [`crate::model::StructuredConfig`]。
//!
//! 這一層做四件事：
//!
//! 1. **認指令**（G60 的清單）。不認識的報 [`DiagCode::UNKNOWN_DIRECTIVE`] 並給建議。
//! 2. **按執行順序表排序**（G49）。★ ★ 這是順序表**唯一的真實消費者**——
//!    一張沒有人讀的表就是一條註解，而本儲存庫反覆抓到的正是「宣告了卻沒人接」。
//! 3. ⛔ **隱式回落（G47）—— 整層已刪除**（G98）。★ 編號有意保留。
//! 4. **一次報全**（G51）：語義錯誤全部收集，不在第一條停下。
//!
//! ★ ★ ★ 下面那個對 [`ChainDirective`] 的 `match` **沒有 `_` 備援臂**，這是有意的：
//! 加了一條指令卻沒教編譯器拿它怎麼辦，它當場編不過。
//! 「在表裡有位置」和「有人接」是兩件事，兩道門缺一不可。

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

/// 匹配器塊裡允許的條件（DSL 參考 §五）。
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

/// 給自動 HTTPS 的站台合成一個 `:80` 上的 308 重定向站台（G12 的後半句）。
///
/// 回傳 `None` 的三種情況：關掉了、沒有自動 HTTPS 的站台、所有主機名都已被
/// 使用者自己寫的 `http://` 站台接走。
///
/// ★ **它順帶修好了 HTTP-01**：G54 把 HTTP-01 定為「備」，而沒有 80 連接埠就沒有落腳點。
/// 挑戰應答在路由**之前**，不會被這條 308 吃掉。
fn synthesize_http_redirect(
    global: &Global,
    sites: &[SiteConfig],
    seen_addresses: &BTreeMap<String, Span>,
    cx: &mut Cx<'_>,
) -> Option<SiteConfig> {
    if !global.auto_http_redirect {
        return None;
    }
    // 使用者自己在 :80 上寫過的主機名 —— 那些不碰。
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

    // ⚠ ⚠ 站台索引的順序是**精確 → 通配 → 連接埠備援**，所以合成出來的這個塊
    //   會蓋過使用者寫的 `:80` 備援站台 —— 對這幾個主機名而言。
    //   ★ 這件事必須**說出來**：它是一次行為改變，而沉默地改掉別人設定的行為，
    //   正是本儲存庫反覆點名的那種「現場看不出問題」。
    if let Some(raw) = &catch_all_80 {
        // ★ 指到**使用者寫的那個備援站台**上。
        // ⚠ 退而求其次也不能指檔案開頭：一條指錯地方的診斷比沒有診斷更費時間 ——
        //   人會先去看它指的那一行，而那一行與這件事無關。
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
            // ★ 合成出來的這個站台**自己不要證書**：它就是那條跳轉。
            auto_https: false,
        })
        .collect();

    Some(SiteConfig {
        addresses,
        matchers: BTreeMap::new(),
        // ⚠ ⚠ **必須顯式寫 Off**：`TlsConfig::default()` 是 `Automatic`，
        //   於是這個純 HTTP 的跳轉站台會被 TLS 那一層當成「要自動簽發」，
        //   裝載紀錄裡多出一條「站台 http://… 要自動簽發，而儲存裡還沒有它的證書」。
        //   ★ 那條假的 ⏳ 是自己寫完當場看見的 —— `plan` 把它印出來了。
        tls: TlsConfig {
            mode: TlsMode::Off,
            ..TlsConfig::default()
        },
        log: None,
        chain: vec![Step {
            order: 60,
            matcher: None,
            // 308 而不是 301：**保留請求方法與 body**。
            // ⚠ 301/302 會讓一個 POST 變成 GET，而那類故障的現場是「表單提交丟了」。
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

    // 自動 HTTP 重定向（G12 的後半句）——**合成一個真的站台塊**。
    // ★ 合成在**編譯期**而不是資料面加特判：合成物會原樣出現在 `fulcrum compile` 的
    //   JSON 與 `fulcrum plan` 的輸出裡 ⇒ **它是可以被讀到、被 diff、被質疑的事實**。
    //   ⚠ 一條看不見的特判，在它某天行為不對時，使用者手上沒有任何東西可查。
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
    /// **第一個** `cache` 塊選的後端（`Some(dir)` = 磁碟，`None` = 記憶體）與它的 span。
    ///
    /// ★ ★ 快取後端是**處理程序級**的，所以這條檢查天然是跨站台的 —— 它不可能
    /// 在「編譯一條 `cache` 指令」那個函式里做完，那裡看不見別的站台。
    /// ⚠ 記的是**第一個**，而診斷指著**後面那個不一樣的**：錯的那一條才是要改的。
    cache_backend: Option<(Option<String>, Span)>,
}

impl Cx<'_> {
    // ── 全域選項塊 ──────────────────────────────────────────────────────────
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
            // ★ ★ **被刪掉的選項，要說清它去哪了。**
            //   ⚠ 只回一句「未知的全域選項」，等於讓寫過它的人以為自己打錯了字，
            //   而那寫法在當時是對的。一個公開設定面消失時，最起碼要告訴人
            //   「它沒了、為什麼、現在該怎麼寫」—— 與 G52（回落必須可見）同一條紀律。
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
                // ★ 三格共用同一條解析與同一條診斷（G140）。⛔ 不許抄三遍：
                //   抄三遍的失效形態是「改了值域忘了改其中一格」，而三格各自都自洽。
                "threads_l7" => g.threads_l7 = self.thread_count(node),
                "threads_l4" => g.threads_l4 = self.thread_count(node),
                "threads_admin" => g.threads_admin = self.thread_count(node),
                "auto_http_redirect" => {
                    // ★ 不寫值就是「開」（`auto_http_redirect` 單寫一行 = on），
                    //   與 `on_demand` 那條同形。
                    g.auto_http_redirect = node
                        .args
                        .first()
                        .is_none_or(|a| parse_bool(&a.value) == Some(true))
                }
                // ★ 收下全部參數（`v` 只是第一個）。網段的合法性在執行環境圖那一層
                //   與 `remote_ip` 共用 `Cidr::parse` 檢核 —— 不在這裡另寫一份。
                "proxy_protocol_from" => {
                    g.proxy_protocol_from = node.args.iter().map(|a| a.value.clone()).collect()
                }
                // ⚠ 這條 `_ => {}` 是「表裡有位置、沒人接」的溫床（批 6 在 `tls` 上栽過）。
                //   守著它的是 compile_behaviour.rs 的 `全局选项全部有人接`：
                //   GLOBAL_OPTIONS 裡多一條而這裡沒接，那條測試**直接 panic**。
                _ => {}
            }
        }
        g
    }

    // ── 站台塊 ──────────────────────────────────────────────────────────────
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

        // 站台級指令先摘出來，剩下的才是鏈上的。
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

        // `http://` 位址不簽證書、不升級；DSL 裡沒寫 `tls` 時按位址推出模式。
        if !tls_written && addresses.iter().all(|a| !a.auto_https) {
            tls.mode = TlsMode::Off;
        }

        // ★ `guarded = false`：站台塊頂層沒有任何外層匹配器罩著。
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

    // ── 執行鏈 ──────────────────────────────────────────────────────────────

    /// 把一組節點編譯成執行鏈。
    ///
    /// `sorted = true` 時按執行順序表排序（站台塊頂層、`handle` 的每個 arm）；
    /// `sorted = false` 時保持書寫順序（`route { … }` 內部——**G49 的逃生口**）。
    ///
    /// `guarded` = **外層容器已經把來源限制住了**（[`restricts_source`]）。
    /// ★ 它只服務於 G116 那條診斷，而那條診斷問的是「這一步對**誰**開著」。
    ///
    /// 要沿鏈下傳，是因為 `handle @internal { metrics }` 裡那一步**自己**沒有匹配器，
    /// ⚠ 但它顯然是被圈住的 —— 只看這一步自己的匹配器會把它誤報成裸奔，
    /// 而一條穩定誤報的診斷很快就沒人看了。
    ///
    /// ⚠ ⚠ **口徑是「限制得了來源」，不是「有沒有匹配器」。**
    /// `handle /metrics { … }` 裡那個路徑匹配器是**任何用戶端都能照著發**的東西，
    /// 它一個人都擋不住。⇒ 判據落在 [`restricts_source`] 上，
    /// **不是** [`is_unconditional`]（那一條問的是另一件事，見它自己的文件）。
    fn chain(
        &mut self,
        nodes: &[&Node],
        matchers: &BTreeMap<String, Matcher>,
        ctx: Ctx,
        sorted: bool,
        guarded: bool,
    ) -> Vec<Step> {
        // ★ 所有 `handle` 合成**一個**互斥組，放在 `handle` 的表位上。
        //   它們分散寫在各處也是同一組——這正是「互斥容器」的含義。
        let mut handle_arms: Vec<HandleArm> = Vec::new();
        let mut handle_span: Option<Span> = None;
        // ★ 每一步都拖著它在原始碼裡的位置。診斷要能說出「它實際跑在第幾步」，
        //   而排完序之後**書寫順序已經丟了**——位置若不在這時候帶上，後面就再也拿不到。
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
            // ★ **穩定排序**：同序號的多條（比如兩條 `header`）保持書寫順序。
            //   不穩定的排序會讓「兩條 header 誰先跑」隨實作細節抖動，
            //   而那種抖動在設定層是查不出來的。
            steps.sort_by_key(|(s, _)| s.order);
            self.warn_unreachable(&steps);
        }
        steps.into_iter().map(|(s, _)| s).collect()
    }

    /// `handle` 的一個分支。
    fn handle_arm(
        &mut self,
        node: &Node,
        matchers: &BTreeMap<String, Matcher>,
        ctx: Ctx,
        guarded: bool,
    ) -> Option<HandleArm> {
        let matcher = self.matcher_ref(node, matchers);
        // 這個分支限制得了來源 ⇒ 塊裡的每一步都被它罩著。見 `restricts_source`。
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

    /// 編譯一條鏈上指令。
    ///
    /// ★ ★ ★ 這個 `match` **沒有 `_` 臂**。新增一條鏈上指令時它會當場編不過——
    /// 這就是 G49 那條「新增指令必須在表裡有位置」的第二半：
    /// **有位置** ≠ **有人接**，兩者都要編譯器來管。
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
        // 這一步的來源被限制住了嗎：自己寫了一個限制得了來源的匹配器，
        // 或者外層容器帶著一個。⚠ 「有匹配器」不等於「限制了來源」，見 `restricts_source`。
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
            // `handle` 在 `chain()` 裡先被摘出去合併成互斥組，不會走到這裡。
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
                // ★ `sorted = false`：這就是逃生口本身。
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
            // ── Prometheus 抓取端點（**M2 批 M**，G116）──────────────────────
            ChainDirective::Metrics => {
                self.expect_no_block(node, d);
                if !args.is_empty() {
                    // ⚠ 一個參數都不收。⇒ `metrics /x` 裡那個 `/x` 走的是**行內匹配器**
                    //   那條路（G50），壓根到不了這裡；到這裡的都是真的多餘參數。
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
    /// ★ 這裡**沒有**走 `sub_block()`，是有原因的：那個 helper 把子塊讀成
    /// 「名字 → 值」的 `BTreeMap`，於是**同名子指令寫兩遍時後一條覆蓋前一條**。
    /// `hide` 的語義是「再多擋一樣」，寫兩行 `hide` 卻只有第二行生效，
    /// 就是一次靜默行為——正是 G88 那張表裡「可見性」那一格要堵的東西。
    /// ⇒ 照 `reverse_proxy` 的做法自己遍歷，`hide` 跨行累加。
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
                    // ⚠ `check_sub_args` 已經把非 `true`/`false` 攔下並 `continue` 了，
                    //   所以這裡 `parse_bool` 不會是 `None`；用 `is_none_or(..)` 兜一下
                    //   只是為了不在這條路上再造一個 `unwrap`。
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
                    // ★ 追加，不是替換（G88）——多行 `hide` 疊起來。
                    "hide" => hide.extend(sub.args.iter().map(|a| a.value.clone())),
                    // ★ M2 批 I：預壓縮旁檔案。同樣**追加**，與 `hide` 同款。
                    "precompressed" => {
                        precompressed.extend(sub.args.iter().map(|a| a.value.clone()))
                    }
                    _ => {}
                }
            }
        }

        // ── root 必填（G89 自研之後沒有 root 就沒有東西可發）─────────────────
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
            // ── 必須絕對路徑（G91）────────────────────────────────────────
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
        let mut upstreams: Vec<UpstreamSpec> = Vec::new();
        for a in args {
            self.check_placeholders(a, ctx);
            // ★ 權重先都是預設的 1；子塊裡的 `weight` 再按**逐字相同的位址**改其中幾格。
            upstreams.push(UpstreamSpec::new(a.value.clone()));
        }
        if upstreams.is_empty() {
            self.arity_error(node, "至少写一个上游，如 `reverse_proxy 127.0.0.1:3000`");
        }

        let mut lb_policy = "round_robin".to_string();
        let mut health = HealthCheck::default();
        let mut dns_refresh_ms = 30_000u64;
        let mut passive = Passive::default();
        // ★ G136：`passive_window` / `passive_cooldown` 寫了沒有 —— ⛔ 不能拿「值 != 預設」當判據：
        //   `passive_window 10s` 是合法寫法，而它與「沒寫過」在產物上完全同形
        //   （與 `weight` 那處 `weighted` 集合同一條理由）。
        let mut passive_knob: Option<(&'static str, Span)> = None;
        let mut header_up: Vec<HeaderOp> = Vec::new();
        let mut header_down: Vec<HeaderOp> = Vec::new();
        let mut transport = "http".to_string();
        let mut tls_insecure_skip_verify = false;
        let mut proxy_protocol: Option<String> = None;
        // 已經被 `weight` 指過一次的上游位址（**逐字**）。★ 不能拿「權重 != 1」當判據：
        // `weight x 1` 是合法的寫法，而它與「沒寫過」在產物上完全同形。
        let mut weighted: std::collections::BTreeSet<String> = Default::default();
        // ★ M2 批 N 任務 2.8（裁決 R6）：這一條 `reverse_proxy` 的穩定 id。
        let mut id: Option<String> = None;

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
                    // ★ ★ G136：取值域在這裡判，⛔ 不回落成預設值。
                    //   ⚠ ⚠ 原文是 `first.parse().ok()` —— `passive_fail 五` 靜默變 `None`，
                    //   而 `None` 的語義恰好是**整個特性關掉**：一個手滑的值讓熔斷悄悄消失。
                    "passive_fail" => match first.parse::<u32>() {
                        Ok(v) if (MIN_PASSIVE_FAIL..=MAX_PASSIVE_FAIL).contains(&v) => {
                            passive.fail_threshold = Some(v)
                        }
                        _ => {
                            let span = sub.args.first().map(|a| a.span).unwrap_or(sub.name_span);
                            self.diags.push(
                                Diagnostic::error(
                                    DiagCode::BAD_PASSIVE_FAIL,
                                    span,
                                    format!(
                                        "`passive_fail` 要一个 {MIN_PASSIVE_FAIL}–{MAX_PASSIVE_FAIL} 的整数，`{first}` 不是"
                                    ),
                                )
                                .help(
                                    "要关掉被动熔断就**整行删掉** ——                                      `passive_fail 0` 不是关闭的写法（那会是第二条路，两条路迟早分家）",
                                ),
                            );
                        }
                    },
                    "passive_window" => {
                        passive.window_ms = parse_duration_ms(&first).unwrap_or(passive.window_ms);
                        passive_knob.get_or_insert((
                            "passive_window",
                            sub.args.first().map(|a| a.span).unwrap_or(sub.name_span),
                        ));
                    }
                    // ★ G136 新增：熔斷後歇多久再放一個半開探針。
                    "passive_cooldown" => {
                        passive.cooldown_ms =
                            parse_duration_ms(&first).unwrap_or(passive.cooldown_ms);
                        passive_knob.get_or_insert((
                            "passive_cooldown",
                            sub.args.first().map(|a| a.span).unwrap_or(sub.name_span),
                        ));
                    }
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
                    // ★ 參數可省 ⇒ 省了就是 v2（owner 拍板的預設）。
                    //   ⚠ 預設值寫在**這一處**，而 `fulcrum_runtime::proxyproto::Version`
                    //   的 `Default` 也是 v2 —— 兩處一致由那邊一條單測釘著
                    //   （`版本名与_dsl_里写的那个词一一对应`）。
                    "proxy_protocol" => {
                        proxy_protocol = Some(
                            sub.args
                                .first()
                                .map(|a| a.value.clone())
                                .unwrap_or_else(|| DEFAULT_PROXY_PROTOCOL.to_string()),
                        );
                    }
                    // ★ M2 批 N：`weight <上游地址> <正整数>`。三條診斷都在這裡面。
                    "weight" => self.upstream_weight(sub, &mut upstreams, &mut weighted),
                    // ★ M2 批 N 任務 2.8：`id <名字>`（裁決 R6 ⇒ G125）。
                    "id" => self.proxy_id(sub, &mut id),
                    _ => {}
                }
            }
        }

        // ★ ★ G136：旋鈕寫了、而 `passive_fail` 沒寫 ⇒ 那兩行**一個都不生效**。
        //   ⚠ 必須說出來：它與「正在生效」在設定里長得一模一樣，沉默就是讓人以為它生效了
        //   （與 `METRICS_UNGUARDED` 同一條紀律）。★ 但**從來不是** error ——
        //   判紅會把「先把旋鈕調好、待會兒再開」這種正當的分步操作一起擋掉。
        if let (None, Some((knob, span))) = (passive.fail_threshold, passive_knob) {
            self.diags.push(
                Diagnostic::warning(
                    DiagCode::PASSIVE_WITHOUT_THRESHOLD,
                    span,
                    format!("`{knob}` 此刻一步都不做：这条 `reverse_proxy` 没写 `passive_fail`"),
                )
                .label("不写 `passive_fail` 就完全不做被动熔断，这个旋钮没有消费者")
                .help(
                    "要开启就补一行 `passive_fail <次数>`；只是想先调好参数的话，这条警告可以留着",
                ),
            );
        }

        StepBody::ReverseProxy {
            id,
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

    /// `reverse_proxy { weight <上游地址> <正整数> }`（**M2 批 N**，裁決 R1/R3）。
    ///
    /// 三條都是**裝載期錯誤**，⛔ 一條都不回落成預設值：
    ///
    /// 1. 位址不在這條 `reverse_proxy` 的上游清單裡 ⇒ [`DiagCode::UNKNOWN_WEIGHT_UPSTREAM`]，
    ///    **並把清單原樣列出來**；
    /// 2. 同一個上游寫了兩次 ⇒ [`DiagCode::DUPLICATE_WEIGHT`]（⛔ 不是「後寫的贏」）；
    /// 3. 值不在 `[1, 65535]` 裡 ⇒ [`DiagCode::BAD_WEIGHT`]。
    ///
    /// ⚠ 三條各自 `return`，所以一行 `weight` 只報一條 —— 但**每一行都會被看**，
    /// 「一次報全」（G51）說的是那件事。
    /// `threads_l7` / `threads_l4` / `threads_admin` 的取值（**G140**）。
    ///
    /// 值域 `[MIN_THREADS, MAX_THREADS]`；不合法就報 [`DiagCode::BAD_THREADS`] 並回傳
    /// `None`。⚠ 回傳 `None` 的語義是**回落到角色預設** —— 與「沒寫這一行」逐字相同，
    /// 而那正是這條診斷必須存在的理由：沒有它，`threads_l7 八` 會靜默地變成「沒寫」。
    ///
    /// ⛔ **不許退回 `first.parse().ok()`**：那條路在 `passive_fail` 上真出過事（G136），
    /// 症狀是「設定照過、裝載照過，而旋鈕一步都不做」。
    fn thread_count(&mut self, node: &Node) -> Option<usize> {
        // `check_sub_args` 已經保證正好一個參數。
        let arg = node.args.first()?;
        match arg.value.parse::<usize>() {
            Ok(n) if (MIN_THREADS..=MAX_THREADS).contains(&n) => Some(n),
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        DiagCode::BAD_THREADS,
                        arg.span,
                        format!(
                            "`{}` 要一个 {MIN_THREADS}–{MAX_THREADS} 的整数，`{}` 不是",
                            node.name, arg.value
                        ),
                    )
                    .label("线程数是不带单位的正整数，如 `8`")
                    .help(
                        "★ `0` 也不合法：pingora 拿这个数去建 tokio runtime，\
                         `0` 会在**启动时 panic**，而那个报错一个字都不提配置",
                    )
                    .note(
                        "★ 不写这一行 = 用角色缺省（L7 = CPU 核数、L4 = 2、管理面与后台 = 1）。\
                         ⚠ 线程不跨 service 共享 ⇒ 总线程数 ≈ Σ(各 service)，\
                         所以这里有意分三格而没有一个笼统的全局值",
                    ),
                );
                None
            }
        }
    }

    fn upstream_weight(
        &mut self,
        sub: &Node,
        upstreams: &mut [UpstreamSpec],
        weighted: &mut std::collections::BTreeSet<String>,
    ) {
        // `check_sub_args` 已經保證正好兩個參數。
        let (addr_arg, weight_arg) = (&sub.args[0], &sub.args[1]);
        let addr = &addr_arg.value;

        // ⚠ 上游清單是空的 = `reverse_proxy` 那一行本身就寫錯了，已經報過 BAD_ARITY。
        //   這裡再報一條「不在清單裡」只是噪音，而噪音會把真的那條埋掉。
        if upstreams.is_empty() {
            return;
        }

        let Some(idx) = upstreams.iter().position(|u| u.addr == *addr) else {
            let list: Vec<&str> = upstreams.iter().map(|u| u.addr.as_str()).collect();
            self.diags.push(
                Diagnostic::error(
                    DiagCode::UNKNOWN_WEIGHT_UPSTREAM,
                    addr_arg.span,
                    format!("`weight` 指着 `{addr}`，而这条 `reverse_proxy` 没有这个上游"),
                )
                .label(format!("这条 `reverse_proxy` 的上游是：{}", list.join(" ")))
                .help(
                    "★ 地址要与 `reverse_proxy` 那一行上的 token **逐字相同** —— \
                     这里不做任何归一化，`backend` 与 `backend:80` 是两个不同的写法",
                )
                .note(
                    "归一化住在运行时那一层；配置层再写一份「差不多的」就会分家，\
                     而分家的现场是「配置照过、权重没生效」，没有任何东西会说",
                ),
            );
            return;
        };

        if !weighted.insert(addr.clone()) {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::DUPLICATE_WEIGHT,
                    sub.name_span,
                    format!("`{addr}` 的 `weight` 在这个块里写了两次"),
                )
                .label("同一个上游只能有一条 `weight`")
                .note(
                    "⛔ 不取「后写的赢」：那种规则下删掉或挪动其中一行会**静默**改掉权重，\
                     而它与「两行本来就一样」在配置里长得一模一样",
                ),
            );
            return;
        }

        match weight_arg.value.parse::<u32>() {
            Ok(w) if (MIN_UPSTREAM_WEIGHT..=MAX_UPSTREAM_WEIGHT).contains(&w) => {
                upstreams[idx].weight = w;
            }
            _ => self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_WEIGHT,
                    weight_arg.span,
                    format!(
                        "`weight` 要一个 {MIN_UPSTREAM_WEIGHT}–{MAX_UPSTREAM_WEIGHT} 的整数，`{}` 不是",
                        weight_arg.value
                    ),
                )
                .label("权重是不带单位的正整数，如 `3`")
                .help(
                    "★ `0` 也不合法：把一个上游摘出调度**只有一种写法** —— \
                     管理面临时覆盖层的 `disable`",
                )
                .note(
                    "两条路做同一件事就会分家（改了一条忘了另一条），\
                     而分家那天没有任何东西会说",
                ),
            ),
        }
    }

    /// `reverse_proxy { id <名字> }`（**M2 批 N 任務 2.8**，裁決 R6 ⇒ G125）。
    ///
    /// 這一層只管三件事，三件都是**裝載期錯誤**、⛔ 一條都不回落成預設值：
    ///
    /// 1. 寫了兩行 ⇒ [`DiagCode::DUPLICATE_PROXY_ID`]（⛔ 不是「後寫的贏」）；
    /// 2. 寫了個空串 ⇒ [`DiagCode::EMPTY_PROXY_ID`]（空串 = 「沒寫」在鍵空間裡的那一格）；
    /// 3. 不在取值域裡 ⇒ [`DiagCode::BAD_PROXY_ID`]（**M2 批 P**，裁決 R6 三之三）。
    ///
    /// ⚠ ⚠ **第 3 條在這裡只是兩處中的一處。** 結構化設定那條路（`POST /load`，
    /// G11 的公開入口）不經過 `fulcrum compile` ⇒ 這條診斷在那條路上一次都不跑，
    /// 由 `fulcrum_runtime` 建圖時調**同一個** [`is_valid_proxy_id`] 再攔一遍。
    /// ★ 兩處調的是這一個函式，⛔ 不是兩份手寫的平行邏輯 —— 與 `weight` 的值域常數
    /// 同一條紀律。
    ///
    /// # ⚠ ⚠ 這一層**看不見「兩條的鍵撞沒撞」**，也不該在這裡看
    ///
    /// 「同一站台內兩條 `reverse_proxy` 的 `(id, 上游地址)` 相同」這件事
    /// 只有 `fulcrum_runtime` 說得清：鍵裡的上游位址是**歸一化之後**的串
    /// （`backend` → `backend:80`），而歸一化在那一層。⇒ 在這裡拿原文 token 比一遍，
    /// 一條寫 `backend`、另一條寫 `backend:80` 的那一對會被算成兩個不同的鍵，
    /// 而它們在執行環境是同一臺機器。
    /// ★ 與 `weight` 的位址比對「有意只做逐字相同」是同一條邊界的兩側：
    /// 那一條要的正是原文，這一條要的正是歸一化後的串。
    ///
    /// ⚠ ⚠ **鍵相同不是錯誤**（裁決 R6 ③ **第二輪** ⇒ G125）：那幾條共享同一個
    /// 覆蓋格子，一次 `disable` 一起生效。⛔ 這一層、那一層都不許為此回報錯誤或告警 ——
    /// 「一個後端掛在幾組 `handle` 路由後面」是反代最常見的寫法。
    /// 「這一格管著幾條」由 `fulcrum_runtime::Runtime::proxy_key_fanout` 算出來，
    /// 經 `/stats` 顯示。
    fn proxy_id(&mut self, sub: &Node, id: &mut Option<String>) {
        // `check_sub_args` 已經保證正好一個參數。
        let arg = &sub.args[0];
        if id.is_some() {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::DUPLICATE_PROXY_ID,
                    sub.name_span,
                    "这个 `reverse_proxy` 块里写了两行 `id`",
                )
                .label("一条 `reverse_proxy` 只能有一个 id")
                .note(
                    "⛔ 不取「后写的赢」：那种规则下删掉或挪动其中一行会**静默**改掉这条 \
                     `reverse_proxy` 的 id，而 id 正是管理面覆盖层的寻址依据 —— \
                     现场是「`disable` 说成功了，而那台机器还在收流量」",
                ),
            );
            return;
        }
        if arg.value.is_empty() {
            self.diags.push(
                Diagnostic::error(DiagCode::EMPTY_PROXY_ID, arg.span, "`id` 不能是空串")
                    .label("写一个名字，如 `id pool_web`")
                    .help(
                        "★ 没写 `id` 的 `reverse_proxy` 在覆盖层的键里 id 那一格**就是空串** \
                     ⇒ `id \"\"` 与根本不写是同一个键，什么都没分开",
                    )
                    .note(
                        "这条最常出现在「两条 reverse_proxy 的键相同、有人补一行 id 想把它们分开」\
                     的时候 —— 而那一行不写名字的话，两条**仍然**共享同一个覆盖格子，\
                     一次 disable 还是一起生效，且不会有任何东西报错",
                    ),
            );
            return;
        }
        // ★ ★ 取值域（裁決 R6 三之三）。⚠ 判在空串**之後**：空串有它自己那條診斷
        //   與自己那段解釋，⛔ 不許被這條泛泛的「不合法」頂掉。
        if !is_valid_proxy_id(&arg.value) {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::BAD_PROXY_ID,
                    arg.span,
                    // ⚠ `{:?}` 而不是 `{}`：帶換行的 id 會把這一行診斷在人眼裡
                    //   斷成兩行 —— 而那正是收緊的理由之二。
                    format!("`id` {:?} 不在取值域里", arg.value),
                )
                .label("换一个短名字，如 `id pool_web`")
                .help(proxy_id_shape())
                .note(
                    "★ ★ 硬的那条理由是**与兜底记号撞车**：本仓用 `<other>` / `<none>` / \
                     `<unknown>` / `<undeclared>` 表示「取不到 / 兜底」，靠的是尖括号在真值里\
                     不可能出现 —— 而在这一条之前，`id <none>` 是合法的。\
                     ⛔ 不是指标基数：`id` 一个字都不进指标（G126）",
                ),
            );
            return;
        }
        *id = Some(arg.value.clone());
    }

    // ── 站台級指令 ──────────────────────────────────────────────────────────
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
                        // ★ 按供應商分岔：`exec` 的第二個參數是**處理程序路徑**，不是憑證。
                        //   ⚠ 一刀切會讓「沒有秘密的設定」也撞上權限門（門禁實測抓到）。
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
                        // ★ ★ ：形狀檢核搬到這裡。
                        //
                        //   在此之前它長在**裝載期**，而且失敗的處置是
                        //   「打一行 error，本站台的 DNS-01 不啟用」——
                        //   ⚠ `validate` 結束代碼仍然是 0，站台照常起來，
                        //   於是一份寫錯 `resolvers` 的設定**在每一處都顯得正常**，
                        //   直到那張證書永遠籤不下來。
                        //   ★ 編譯期判形狀不需要網路，所以它可以在這裡判；
                        //   真去解析主機名要網路，那一步留到簽發那一刻（見 host.rs 註解）。
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
                    // ⚠ ⚠ 這一臂**必須響**，不能是 `_ => {}` —— 那樣子指令會被**靜默丟掉**
                    //   （DSL 認得、`TLS_SUBS` 裡有、文件寫著，而執行環境從來沒見過）。
                    //   這裡 match 的是 `&str`，語言給不出窮盡性檢查 ⇒ 兩道：
                    //   ① 走到這裡報一條內部錯誤；② `tests/compile_behaviour.rs` 的
                    //   `tls子指令全部有人接` 逐條枚舉 `TLS_SUBS`，判據是**值落到了 `TlsConfig` 上**。
                    // ★ 第 ① 道單獨不夠：改回 `_ => {}` 之後內部錯誤根本不會發出，
                    //   於是任何「斷言沒有內部錯誤」的門都照常給綠。
                    // ⚠ ★ 而第 ② 道必須真的存在：一條斷言「某道門存在」的註解，
                    //   在門不存在時長得和門存在時一模一樣，比沒有註解更糟。
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
        // ★ G15：On-Demand 沒配存取資格就拒絕啟動。這裡在**編譯期**就把它擋下——
        //   「錯誤在啟動時暴露，不等被濫用才發現」，而編譯期比啟動更早。
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
        // ★ ★ G58：配了 DNS-01 就必須配權威 NS。形狀照 G15——**編譯期拒絕**。
        //
        //   G58 寫死了「確認 TXT 可見必須真去問權威 NS，絕不能只 sleep 一個固定秒數」。
        //   ⚠ 少了 `resolvers`，實作要麼退回固定 sleep（G58 明確禁止），
        //   要麼在簽發中途才回報錯誤——而那時已經在 CA 那邊開了一張訂單、
        //   並且往 DNS 上寫了一條記錄。**在編譯期擋下，代價只是一條診斷。**
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
        // ── ★ ★ G59：原生供應商的兩條硬約束，都在**編譯期**擋下（形狀照 G15）─────
        //
        // ⚠ 理由不是潔癖：拿到某域的 DNS 寫權限 = **能為該域簽發任意證書**，
        //   還能改 MX 劫持電子郵件。它比 On-Demand 被刷爆嚴重得多。
        if matches!(
            cfg.dns_provider.as_deref(),
            Some("cloudflare") | Some("dnspod")
        ) {
            let provider = cfg.dns_provider.clone().unwrap_or_default();
            // G59 第 1 條：憑證絕不寫進 DSL。只認 `env:` / `file:` 兩種**來源**寫法。
            //
            // ★ ★ 判據是**白名單**，不是「看起來像 token 就回報錯誤」的黑名單——
            //   後者要去猜什麼樣子算 token，而猜錯的那一次恰恰就是真 token 被放行的那一次。
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
                // ★ ★ ★ **口徑變了**（owner 拍板）：字面值憑證**寫得下**了 ——
                //   一份設定檔案就能跑完，形狀照 Caddy。
                //   ⚠ 而剩下的這一條判據比原來那條更難寫對：既然不寫前綴就是字面值，
                //   那麼**一個寫錯的前綴**（`fil:/path`、`ENV:NAME`）就會被當成憑證本身，
                //   然後帶著一個根本不是憑證的字串去打 CA —— 現場是「憑證不對」，
                //   而真正的原因是打錯了三個字母。
                //   ⇒ 判據：**長得像來源前綴、而那個前綴我們不認識** ⇒ 編譯期回報錯誤，
                //   並把「如果這真的就是憑證本身」的出路（`literal:`）寫在提示裡。
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
            // G59 第 3 條：必須顯式宣告這份憑證覆蓋哪些 zone。
            //
            // ★ 對 DNSPod 這是唯一的範圍約束（它的 token 是帳號級的，沒有可問的端點）；
            //   對 Cloudflare 它與啟動時檢核（第 2 條）疊加。⚠ 兩家都要求，
            //   比 G59 字面更嚴一點——統一一條規則，比「哪家要哪家不要」好記也好審。
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

        // ★ `dns exec` 不給處理程序路徑 = 沒人去改 TXT。同樣在編譯期擋下。
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

    /// `log { headers … }` / `log { resp_headers … }` 的白名單（**M2 批 L 第 ③ 步**）。
    ///
    /// ⚠ **值不從 `subs` 裡取**（[`Cx::sub_block`] 把參數用空格連成一串）：
    /// ① 拆回來的東西沒有位置，而這一格要報一條**指著某一個名字**的診斷；
    /// ② 「連起來再拆開」在參數裡出現空格的那天會靜靜地錯。
    /// ⚠ 但**收不收**仍以 `subs` 為準 —— 參數個數不對時它已被 `check_sub_args` 攔掉。
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
                // ★ ★ ★ 四個名字是**編譯期錯誤**（G114 那一半沒被推翻的理由）。
                //   ⚠ 大小寫不敏感：HTTP 頭名本來就不區分大小寫，
                //     一條「寫 `Cookie` 會紅、寫 `cookie` 不會」的規則等於沒有規則。
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
        // ★ 塊內一律按 `ErrorHandler` 上下文檢核佔位符——`{status}` 只在這裡可用。
        // ⚠ `guarded = false`：`handle_errors` 是**按狀態碼**進來的，不是按匹配器 ——
        //   它對「誰能連到這個連接埠」一個字都沒說。★ 把它算成一層保護，等於讓
        //   一句 `handle_errors { metrics }` 悄悄躲開 G116 那條診斷。
        self.chain(&inner, matchers, Ctx::ErrorHandler, true, false)
    }

    // ── L4（M2 自研，M1 編譯成回落）─────────────────────────────────────────
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
                            // ★ ★ M2 批 C：`sni` / `alpn` 嵌套匹配塊（owner 拍板取的形狀）。
                            //   `sni api.example.com *.internal { proxy 10.0.0.1:8443 }`
                            //   ⚠ 只有 `tcp` 有這兩條：UDP 上沒有 ClientHello，
                            //   而 QUIC 的 Initial 是**加密**的 —— 那不是「以後再做」，
                            //   是**做不到**，所以這裡要說清楚，不能只說「不支援」。
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
                            // ⚠ ⚠ **兩條都只在 `tcp` 上有意義，而這不是「以後再做」。**
                            //   PROXY protocol 是**面向連接**的：頭在連接開頭髮一次。
                            //   UDP 上沒有「連接開頭」—— 每個資料報都是獨立的，
                            //   「只發一次」沒有落點，「每個都發」不是這個協定規定的東西。
                            //   ★ 與批 C 的 `sni` / `alpn` 同一條紀律：診斷要說出**為什麼**，
                            //     只說「不支援」會讓人以為等等就有了。
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
                                    // ★ 網段本身的合法性在**執行環境圖**那一層檢核
                                    //   （與 `remote_ip` 共用 `Cidr::parse`，見 G50 那條
                                    //   「結構化設定是公開入口，那一層也要檢核」）——
                                    //   這裡只收字面值，不另寫一份解析。
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
                // ⚠ 備援可以沒有（「只服務我認得的那幾個名字」），但**總得有人能接**：
                //   既沒有備援又沒有規則的監聽器，接受連接之後只能立刻關掉。
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
        // ★ ★ ★ M2 批 B：**`l4` 不再產生任何回落標記** ——
        //   TCP（批 A）與 UDP（批 B）都自研完了，`L4Config` 裡那一格也隨之刪掉。
        //   ⚠ 這改的是 `compile` 的 JSON 產物（G11/G48 的公開契約），
        //   dsl-reference §4.5 的狀態那行跟著改，兩邊都有測試盯著。
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

    /// 一個指令後面不該跟塊，跟了就報。
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
        // ★ ★ 這條 help 擋的是一個真實的死衚衕：`rewrite /new/x` 裡那個 `/new/x`
        //   會被「第一個以 `/` 開頭的參數就是匹配器」的規則吃掉，於是參數看起來少了一個，
        //   而報出來的只是一句「參數不對」——**看不出真正該怎麼寫**。
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

    /// 按規格檢核一條子指令的參數個數與類型。回傳 `false` 表示這條不可用。
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
        // 類型只檢核第一個參數——多參的那幾條（header_up/down、index）第一個是名字，
        // 後面是值，逐個套同一個類型沒有意義。
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

    /// 把一個子塊讀成「名字 → 全部參數用空格連起來」的表。
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

    /// `cache { disk … }` 的兩道檢查（M2 批 H）。
    ///
    /// ① 必須絕對路徑（G91 那條理由一字不改地適用：相對路徑按處理程序 cwd 解析，
    ///    而 systemd 下 cwd 是 `/`、開發機上是專案目錄 ⇒ 同一份設定指向兩個地方）。
    /// ② ★ ★ **全處理程序只有一個快取後端**，所以多個 `cache` 塊必須選同一個。
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
        // ⚠ 只有**真收下了** `disk` 才指著那個參數：寫了 `disk` 但參數個數不對時
        //   它已經被 `check_sub_args` 攔掉、不在 `subs` 裡，這時這個塊選的其實是記憶體，
        //   指著那行反而是在說一件不成立的事。
        let here = match disk_dir {
            Some(_) => sub_arg_span(node, "disk").unwrap_or(node.name_span),
            None => node.name_span,
        };
        match &self.cache_backend {
            None => self.cache_backend = Some((disk_dir.clone(), here)),
            // 一樣就一樣，什麼都不用說。
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

    /// `header` 指令：行內一條，或子塊裡多條。
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
                // 子塊裡每一行的形狀是 `[+-]<name> [value]`，指令名本身就是頭名。
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

    /// ★ ★ G49 配套第 4 條：**診斷必須能解釋「它實際跑在第幾步」**。
    ///
    /// 內建順序表把書寫順序和執行順序拆開之後，最容易出現的意外就是
    /// 「我明明寫在後面的備援，怎麼先跑了」。這裡在編譯期就把它說出來。
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

/// 這一步會不會**無條件**終結請求——即它之後的任何一步都再也輪不到。
///
/// ★ 「無條件」有兩層：這一步自己沒有匹配器，**並且**它真的會終結。
/// 容器要遞歸看：`route { respond 200 }` 終結，`route { respond /x 200 }` 不終結
/// （裡面那條帶匹配器）；`handle` 組只有**備援分支**裡終結才算。
///
/// ⚠ 只認「終結類指令 + 帶備援分支的 handle 組」是不夠的 —— `handle { … 兜底 … }`
/// 後面跟一個 `route { … }` 時會一聲不吭，而那個 route 永遠進不去。
/// ★ **判據只認得一種形狀**是本儲存庫反覆抓到的那類缺陷。
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

/// 「沒有匹配器」與「匹配器是 `*`」是同一件事。
///
/// ⚠ 只認前者的話，`redir * /x` 不算備援，它後面那條永遠跑不到的指令
/// **一聲不吾** —— 而 `*` 正是文件教使用者寫的那個寫法。
fn is_unconditional(m: &Option<MatcherRef>) -> bool {
    match m {
        None => true,
        Some(MatcherRef::Path(p)) => p == "*",
        Some(MatcherRef::Named(_)) => false,
    }
}

/// 一條匹配條件**減少得了「誰能碰到這一步」嗎**（只服務於 G116 那條診斷）。
///
/// ★ ★ 判據是一句可驗證的問話：**它能不能把兩個發同樣請求的用戶端分開？**
/// - `remote_ip` 能 —— 它讀的是 socket 對端，在這一層偽造不了。
/// - `header` 能 —— 一個共享金鑰可以放在這裡。
/// - `path` / `path_regexp` / `host` / `method` / `query` **都不能**：
///   它們全都是請求行與請求頭裡的東西，**任何用戶端都能照著發一份**
///   ⇒ 它們只決定端點擺在哪，不減少能碰到它的人。
///
/// ⚠ ⚠ `not` **不會出現在這張表要比的位置上**：`condition()` 是把 `not { … }`
/// 拆開、給裡面每一條打上 `negate` 之後再存的，所以 `not { remote_ip … }` 存下來的
/// `kind` 就是 `remote_ip`。⇒ 按 `kind` 比對時它照樣算保護，**不需要為 `not` 寫一條特例**。
const SOURCE_CONDITIONS: &[&str] = &["remote_ip", "header"];

/// 這個匹配器位**限制得了來源嗎**（G116 那條診斷專用）。
///
/// ⚠ ⚠ **它與 [`is_unconditional`] 問的是兩件不同的事，別合併**：
/// 那一條問「這一步會不會把它後面的都吃掉」（`warn_unreachable` 用），
/// 這一條問「這一步對誰開著」。兩條判據合用一個謂詞，就是讓它們將來一起漂走。
///
/// ★ 只有**命名匹配器**才可能算數：行內匹配器按 G50 只能是路徑，而路徑不限制來源。
/// ⚠ 查不到那個名字（引用了沒定義過的 `@name`）當作**不算保護** ——
/// 那條路上 `matcher_ref` 已經報過 `UNKNOWN_MATCHER` 了，這裡不必再說一遍；
/// ★ 而在「不確定」時倒向報警告，是這條診斷唯一安全的那一側。
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

/// 回應側上下文：在 `handle_errors` 裡就還是 `handle_errors`。
fn resp_ctx(base: Ctx) -> Ctx {
    match base {
        Ctx::ErrorHandler => Ctx::ErrorHandler,
        _ => Ctx::Response,
    }
}

/// 曾經存在、現在刪掉了的全域選項 → 一句「它去哪了」。
///
/// ★ ★ 這張表**只增不刪**：一條選項被刪掉之後，寫過它的設定檔案不會跟著消失。
/// ⚠ 把它從這裡拿走，那些設定就退回到「未知的全域選項」——
/// 一句聽起來像「你打錯字了」的話，而那寫法曾經是對的。
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

/// 站台位址解析（DSL 參考 §二）。
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
    // ★ IPv6 字面值要**在拆連接埠之前**判掉。`[::1]` 裡的冒號會被 `rsplit_once(':')`
    //   當成連接埠分隔符，於是使用者收到的是一句「連接埠要是 1–65535 之間的整數」——
    //   **指向一個根本不存在的問題**。
    //   ⚠ 不支援它是**已知限制**，不是疏漏：DSL 參考 §二把支援的寫法列全了，裡面沒有它。
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

    // ★ 沒有主機名就籤不出證書，所以 `:8080` 這種一律是 HTTP、不自動升級。
    //   這不是取捨，是 ACME 的事實。
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

/// 從一個子塊裡取某條子指令**第一個參數**的 span。
///
/// ⚠ [`Cx::sub_block`] 只吐值不吐位置，而**一條指錯地方的診斷比沒有診斷更費時間**
/// —— 人會先去看它指的那一行，而那一行與這件事無關。
fn sub_arg_span(node: &Node, name: &str) -> Option<Span> {
    node.block.as_ref()?.iter().find_map(|s| match s {
        Stmt::Node(n) if n.name == name => n.args.first().map(|a| a.span),
        _ => None,
    })
}

/// 把一個快取後端選擇說成人話（給 `FUL-DSL-0035` 的標題行用）。
fn describe_backend(d: &Option<String>) -> String {
    match d {
        Some(dir) => format!("磁盘 `{dir}`"),
        None => "内存（没写 `disk`）".to_string(),
    }
}
