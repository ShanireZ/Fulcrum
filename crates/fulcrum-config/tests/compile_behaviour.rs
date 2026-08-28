//! 端到端：一段 DSL 进去，结构化配置或诊断出来。
//!
//! ★ 这一层测的是**决策落没落地**，不是函数对不对：
//! G49（顺序表真的在排序、`route` 真的保序）、G47/G52（隐式回落带标记）、
//! G50（行内只允许路径）、G51（一次报全 + 诊断长什么样）、
//! G61（占位符不可用位置是错误）、G62（不支持 import）、G63（默认值）。

use fulcrum_config::compile_str;
use fulcrum_config::diag::DiagCode;
use fulcrum_config::directive::{
    CACHE_SUBS, FILE_SERVER_SUBS, GLOBAL_OPTIONS, LOG_SUBS, REVERSE_PROXY_SUBS, TLS_SUBS,
};
use fulcrum_config::model::{MatcherRef, StepBody};

fn ok(src: &str) -> fulcrum_config::model::StructuredConfig {
    let o = compile_str("test.Fulcrumfile", src);
    assert!(
        !o.diagnostics.has_errors(),
        "不该有错误，实际：\n{}",
        o.render_diagnostics()
    );
    o.config.expect("没有错误就该有产物")
}

fn codes(src: &str) -> Vec<DiagCode> {
    let o = compile_str("test.Fulcrumfile", src);
    o.diagnostics.items().iter().map(|d| d.code).collect()
}

// ── G49：顺序表真的在排序 ───────────────────────────────────────────────────

#[test]
fn 书写顺序被内建顺序表重排() {
    // 倒着写：终结类在最前，中间件在最后。
    let cfg = ok("a.com {\n  reverse_proxy 127.0.0.1:3000\n  encode gzip\n  header X-A 1\n}\n");
    let orders: Vec<u16> = cfg.sites[0].chain.iter().map(|s| s.order).collect();
    assert_eq!(
        orders,
        vec![20, 40, 80],
        "必须按顺序表重排，而不是按书写顺序"
    );
    assert_eq!(cfg.sites[0].chain[0].body.directive_name(), "header");
}

#[test]
fn 同序号的多条保持书写顺序() {
    // ★ 稳定排序：两条 `header` 谁先跑必须由书写顺序决定，
    //   否则它会随实现细节抖动，而那种抖动在配置层查不出来。
    let cfg = ok("a.com {\n  header X-First 1\n  header X-Second 2\n  respond 200\n}\n");
    let StepBody::Header { ops } = &cfg.sites[0].chain[0].body else {
        panic!("第一条应当是 header")
    };
    assert_eq!(ops[0].name, "X-First");
    let StepBody::Header { ops } = &cfg.sites[0].chain[1].body else {
        panic!()
    };
    assert_eq!(ops[0].name, "X-Second");
}

#[test]
fn route_块内保持书写顺序() {
    let cfg = ok("a.com {\n  route {\n    respond 200\n    header X-A 1\n  }\n}\n");
    let StepBody::Route { steps } = &cfg.sites[0].chain[0].body else {
        panic!("应当是 route 容器")
    };
    // 逃生口：块内不重排，所以 respond(70) 仍在 header(20) 前面。
    assert_eq!(
        steps.iter().map(|s| s.order).collect::<Vec<_>>(),
        vec![70, 20]
    );
}

#[test]
fn 容器排在终结类之前所以文档首页那个例子成立() {
    // handle @internal { … } + respond 403 兜底 —— DSL 参考 §一的示例。
    let cfg = ok(
        "api.com {\n  @internal remote_ip 10.0.0.0/8\n  handle @internal {\n    reverse_proxy 10.0.0.1:8080\n  }\n  respond 403\n}\n",
    );
    let names: Vec<&str> = cfg.sites[0]
        .chain
        .iter()
        .map(|s| s.body.directive_name())
        .collect();
    assert_eq!(names, vec!["handle", "respond"], "handle 必须先于 respond");
}

#[test]
fn 多个handle合成一个互斥组() {
    let cfg = ok(
        "a.com {\n  handle /api/* {\n    respond 200\n  }\n  handle {\n    respond 404\n  }\n}\n",
    );
    assert_eq!(
        cfg.sites[0].chain.len(),
        1,
        "两个 handle 是同一组，不是两步"
    );
    let StepBody::Handle { arms } = &cfg.sites[0].chain[0].body else {
        panic!()
    };
    assert_eq!(arms.len(), 2);
    assert_eq!(arms[0].matcher, Some(MatcherRef::Path("/api/*".into())));
    assert_eq!(arms[1].matcher, None, "第二个是兜底分支");
}

#[test]
fn 被兜底终结指令挡住的那条会被警告并说出第几步() {
    let o = compile_str(
        "t.Fulcrumfile",
        "a.com {\n  respond 200\n  reverse_proxy 127.0.0.1:3000\n}\n",
    );
    assert!(!o.diagnostics.has_errors());
    let w = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNREACHABLE_STEP)
        .expect("应当警告 reverse_proxy 永远跑不到");
    assert!(w.label.contains("第 80 步"), "{}", w.label);
    assert!(w.label.contains("第 70 步"), "{}", w.label);
}

// ── G47 / G52：隐式回落 ─────────────────────────────────────────────────────

/// ⚠ **这条判据现在验的是「归零」**：`file_server` 与 `cache` 都改自研之后，
/// 回落层整块删除（G98）⇒ 那两条指令编译得过、且**一条回落标记都产生不出来**。
/// ★ 测试名保留原样是有意的：它记着这条判据为什么存在。
///
/// > ★ 一条被删掉的判据，与一个从来没有过判据的功能，事后看起来一模一样。
/// > ⇒ 所以这条留着，而不是删掉。
#[test]
fn file_server与cache编译成回落且被逐条列出() {
    let cfg =
        ok("a.com {\n  cache {\n    ttl 5m\n  }\n  file_server {\n    root /srv/www\n  }\n}\n");
    // ★ 两条都编译得过，而且都**不是**回落 —— 归属表里没有「回落」那一档了。
    assert_eq!(cfg.sites.len(), 2, "a.com 会带出一个 :80 的自动跳转站点");
    let names: Vec<&str> = cfg.sites[0]
        .chain
        .iter()
        .map(|s| s.body.directive_name())
        .collect();
    assert!(
        names.contains(&"cache") && names.contains(&"file_server"),
        "{names:?}"
    );
    use fulcrum_config::directive::ChainDirective;
    for d in [ChainDirective::Cache, ChainDirective::FileServer] {
        assert!(
            !d.owner().doc_label().contains("回落"),
            "`{}` 还挂在回落上",
            d.name()
        );
    }
}

/// L4 整面自研 + 回落层删除之后，这条判据只剩「`l4` 块本身编译对了」那一半。
/// ⚠ 一条被删掉的判据，与一个从来没有过判据的功能，事后看起来一模一样。
#[test]
fn l4块不再产生任何回落路由() {
    let both = ok(
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n  udp :53 {\n    proxy 10.0.0.6:53\n  }\n}\n",
    );
    let l4 = both.l4.clone().expect("应当有 l4");
    assert_eq!(l4.listeners.len(), 2);
    assert_eq!(l4.listeners[0].proto, "tcp");
    assert_eq!(l4.listeners[1].proto, "udp");

    let tcp_only = ok("l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n}\n");
    assert_eq!(tcp_only.l4.expect("应当有 l4").listeners.len(), 1);

    // ★ 它的反向那一半在 `crates/fulcrum-runtime/tests/fallback.rs`：
    //   「归属表里不许再出现回落那一档」。
}

#[test]
fn 自研的指令没有回落标记() {
    // ★ 回落层删除之后**所有**指令都没有回落标记 —— 所以这是一条全表断言。
    use fulcrum_config::directive::ChainDirective;
    for d in ChainDirective::ALL {
        assert!(
            !d.owner().doc_label().contains("回落"),
            "`{}` 的归属是「{}」",
            d.name(),
            d.owner().doc_label()
        );
    }
}

// ── G50：匹配器 ─────────────────────────────────────────────────────────────

#[test]
fn 命名匹配器与行内路径() {
    let cfg = ok(
        "a.com {\n  @m {\n    path /x/*\n    method POST\n  }\n  respond @m 200\n  reverse_proxy /api/* 127.0.0.1:1\n}\n",
    );
    assert_eq!(cfg.sites[0].matchers["m"].conditions.len(), 2);
    let respond = cfg.sites[0]
        .chain
        .iter()
        .find(|s| s.body.directive_name() == "respond")
        .unwrap();
    assert_eq!(respond.matcher, Some(MatcherRef::Named("m".into())));
}

#[test]
fn 引用没定义过的匹配器要报错() {
    assert!(codes("a.com {\n  respond @nope 200\n}\n").contains(&DiagCode::UNKNOWN_MATCHER));
}

#[test]
fn 匹配器重复定义要报错() {
    assert!(
        codes("a.com {\n  @m path /a\n  @m path /b\n  respond 200\n}\n")
            .contains(&DiagCode::DUPLICATE_MATCHER)
    );
}

#[test]
fn 未知的匹配条件要报错并给建议() {
    let o = compile_str(
        "t",
        "a.com {\n  @m {\n    paths /a\n  }\n  respond @m 200\n}\n",
    );
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_MATCHER_CONDITION)
        .unwrap();
    assert_eq!(d.help.as_deref(), Some("你是不是想写 `path`？"));
}

// ── G51：诊断 ───────────────────────────────────────────────────────────────

#[test]
fn 未知指令的诊断与文档样例逐字符一致() {
    // ★ DSL 参考 §九印出来的那一段就是契约。这里把它复现出来。
    let mut src = String::new();
    for i in 1..=10 {
        src.push_str(&format!("# 占位第 {i} 行\n"));
    }
    src.push_str("example.com {\n    reverse-proxy 127.0.0.1:3000\n}\n");

    let o = compile_str("/etc/fulcrum/Fulcrumfile", &src);
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_DIRECTIVE)
        .expect("应当报未知指令");

    let expected = "\
error[FUL-DSL-0007]: unknown directive `reverse-proxy`
  --> /etc/fulcrum/Fulcrumfile:12:5
   |
12 |     reverse-proxy 127.0.0.1:3000
   |     ^^^^^^^^^^^^^ 未知指令
   |
   = help: 你是不是想写 `reverse_proxy`？
   = note: 全部指令见 docs/architecture/dsl-reference.md §4
";
    assert_eq!(d.render(&o.source), expected);
}

#[test]
fn 一次报全而不是遇到第一条就停() {
    let src = "a.com {\n  bogus1 x\n  bogus2 y\n  respond @nope 200\n  encode 不存在的编码\n}\n";
    let o = compile_str("t", src);
    assert!(
        o.diagnostics.error_count() >= 4,
        "应当一次报出 4 条以上，实际 {}：\n{}",
        o.diagnostics.error_count(),
        o.render_diagnostics()
    );
}

#[test]
fn 中文行上的caret按显示宽度对齐() {
    // 第 2 行是：`  header 中文头 "{nope}"`，诊断指着那个带引号的取值。
    //
    // ★ 期望值是**手算出来的常量**，不是拿被测函数再算一遍：
    //   前缀 `  header 中文头 ` 的**显示宽度** = 2 + 6 + 1 + 3×2 + 1 = **16 列**，
    //   而它的**字符数**只有 13。按字符数算 caret 会左偏 3 格——
    //   偏移量恰好等于中文字数，所以这种偏差在纯 ASCII 的测试里永远看不见。
    let src = "a.com {\n  header 中文头 \"{nope}\"\n}\n";
    let o = compile_str("t", src);
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_PLACEHOLDER)
        .expect("应当报未知占位符");
    let rendered = d.render(&o.source);
    // 0 标题 / 1 `-->` / 2 竖线 / 3 源码 / 4 caret
    let caret_line = rendered.lines().nth(4).expect("应当有 caret 行");
    // 行号 `2` 宽 1 ⇒ caret 行的前缀是 `  | `（4 个字符），后面才是 pad。
    assert_eq!(
        caret_line.find('^'),
        Some(4 + 16),
        "caret 没落在那个引号下面：{caret_line:?}"
    );
}

// ── G61：占位符 ─────────────────────────────────────────────────────────────

#[test]
fn status占位符只能在handle_errors里() {
    assert!(
        ok("a.com {\n  handle_errors {\n    respond 500 \"出错了：{status}\"\n  }\n}\n").sites[0]
            .error_handler
            .len()
            == 1
    );
    assert!(
        codes("a.com {\n  respond 200 \"{status}\"\n}\n")
            .contains(&DiagCode::PLACEHOLDER_NOT_AVAILABLE)
    );
}

#[test]
fn 未知占位符是错误不是空串() {
    assert!(codes("a.com {\n  rewrite {pathh}\n}\n").contains(&DiagCode::UNKNOWN_PLACEHOLDER));
}

#[test]
fn json体不会被当成占位符() {
    ok("a.com {\n  respond 403 \"{\\\"error\\\":\\\"nope\\\"}\"\n}\n");
}

// ── G62 / G63 / 类型 ────────────────────────────────────────────────────────

#[test]
fn import不被支持() {
    assert!(codes("a.com {\n  import other.conf\n}\n").contains(&DiagCode::IMPORT_NOT_SUPPORTED));
}

#[test]
fn 默认值进了结构化配置() {
    let cfg = ok("a.com {\n  respond 200\n}\n");
    assert_eq!(cfg.defaults.no_site_match, 421);
    assert_eq!(cfg.defaults.no_route_match, 404);
    assert_eq!(cfg.defaults.all_upstreams_down, 502);
}

#[test]
fn 时长的裸数字被拒() {
    assert!(
        codes("a.com {\n  reverse_proxy x:1 {\n    health_interval 5\n  }\n}\n")
            .contains(&DiagCode::BAD_DURATION)
    );
    ok("a.com {\n  reverse_proxy x:1 {\n    health_interval 5s\n  }\n}\n");
}

#[test]
fn 布尔只认true和false() {
    assert!(
        codes("a.com {\n  reverse_proxy x:1 {\n    tls_insecure_skip_verify yes\n  }\n}\n")
            .contains(&DiagCode::BAD_BOOL)
    );
}

#[test]
fn lb_policy取值错时给建议() {
    let o = compile_str(
        "t",
        "a.com {\n  reverse_proxy x:1 {\n    lb_policy least_con\n  }\n}\n",
    );
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::BAD_ENUM)
        .unwrap();
    assert!(d.label.contains("least_conn"), "{}", d.label);
}

// ── 地址 ────────────────────────────────────────────────────────────────────

#[test]
fn 地址的四种写法() {
    let cfg = ok("example.com, *.example.com, http://plain.com, :8080 {\n  respond 200\n}\n");
    let a = &cfg.sites[0].addresses;
    assert_eq!(
        (a[0].scheme.as_str(), a[0].port, a[0].auto_https),
        ("https", 443, true)
    );
    assert!(a[1].wildcard, "*.example.com 是通配符（需要 DNS-01）");
    assert_eq!(
        (a[2].scheme.as_str(), a[2].port, a[2].auto_https),
        ("http", 80, false)
    );
    // ★ 没有主机名就签不出证书，所以 `:8080` 一律不自动升级。
    assert_eq!(
        (a[3].scheme.as_str(), a[3].port, a[3].auto_https),
        ("http", 8080, false)
    );
}

#[test]
fn 带路径的地址被拒() {
    assert!(codes("example.com/api {\n  respond 200\n}\n").contains(&DiagCode::BAD_SITE_ADDRESS));
}

#[test]
fn 同一个地址不能属于两个站点块() {
    assert!(
        codes("a.com {\n  respond 200\n}\na.com {\n  respond 201\n}\n")
            .contains(&DiagCode::DUPLICATE_SITE_ADDRESS)
    );
}

// ── TLS ─────────────────────────────────────────────────────────────────────

#[test]
fn on_demand没配ask时编译期就红() {
    // ★ G15 的形状：错误在启动时暴露，不等被滥用才发现。编译期比启动更早。
    assert!(!codes("a.com {\n  tls {\n    on_demand\n  }\n}\n").is_empty());
    ok(
        "a.com {\n  tls {\n    on_demand\n    ask https://admin.example.com/check\n  }\n  respond 200\n}\n",
    );
}

/// ★ ★ ★ 「表里有位置」不等于「有人接」——`tls` 的子指令逐条都要落到结构化配置里。
///
/// 这条门补的是一个**真发生过**的缺陷：`resolvers` 在 DSL 里认得、
/// `TLS_SUBS` 里有它、参考文档写着它，而 `compile.rs` 那个 `match` 用 `_ => {}`
/// 把它**静默丢掉**了——运行时从来没见过它。当时补了两道，
/// 但**这一道只写进了注释，人没有真去写**，于是「有两道门」本身成了一句假话。
///
/// ⚠ ⚠ **判据必须挂在「值落到了 `TlsConfig` 上」，不能挂在「编译器报了内部错误」。**
///   把那条臂改回 `_ => {}` 之后内部错误**根本不会发出**，
///   于是一道「断言没有内部错误」的门在缺陷复现时照样给绿——
///   它恰好在唯一需要它的时刻看不见。
///
/// ⚠ ⚠ ★ **每条子指令必须有它自己的最小配置，不能几条挤在一份里。**
///   `on_demand` 要 `ask` 陪、`dns` 要 `resolvers` 陪（G15 / G58 的编译期伴生校验），
///   挤在一份里时把 `resolvers` 改坏，**先开火的是伴生校验**，本条根本走不到自己的断言 ——
///   ★ **「它红了」不等于「它红对了地方」。**
///   现在 `resolvers` 那份**故意不写 `dns`**，静默丢弃只有本条看得见。
///   ★ 并且**一次报全**而不是逐条 panic：先红的那条未必是根因那条。
///
/// ★ 反证（实做）：把 `compile.rs` 那条臂改成 `"resolvers" => {}`
///   （即原缺陷的形态：静默丢弃、无诊断），本条报两行 ——
///   `tls dns 那份最小配置编译不过`（伴生校验）+
///   **`tls resolvers … 没落到 TlsConfig 上`**（根因）。改回去即绿。
///
/// ★ 新加一条子指令而忘了在这里给判据 ⇒ 走到 `other` 那一臂直接 panic。
///   `s.name` 是 `&str`，语言层面给不出穷尽性检查，**这一臂就是人工的那份**。
#[test]
fn tls子指令全部有人接() {
    // 空表会让这个循环一次都不跑而依然给绿——先钉住下界（照 doc_contract 的形状）。
    assert!(!TLS_SUBS.is_empty(), "TLS_SUBS 是空的");
    let mut problems: Vec<String> = Vec::new();
    for s in TLS_SUBS {
        #[allow(clippy::type_complexity)]
        let (src, landed): (&str, fn(&fulcrum_config::model::TlsConfig) -> bool) = match s.name {
            // 丢掉 `on_demand` ⇒ 它变 false ⇒ G15 那条伴生校验不开火 ⇒ 只有本条看得见。
            "on_demand" => (
                "a.com {\n  tls {\n    on_demand true\n    ask https://admin.example.com/check\n  }\n  respond 200\n}\n",
                |t| t.on_demand,
            ),
            "ask" => (
                "a.com {\n  tls {\n    ask https://admin.example.com/check\n  }\n  respond 200\n}\n",
                |t| t.ask.as_deref() == Some("https://admin.example.com/check"),
            ),
            // 丢掉 `dns` ⇒ `dns_provider` 是 None ⇒ G58 那条伴生校验不开火。
            "dns" => (
                "a.com {\n  tls {\n    dns exec /bin/true\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n",
                |t| {
                    t.dns_provider.as_deref() == Some("exec")
                        // ⚠ 此后 dns_arg 是 Secret（默认脱敏）——
                        //   判据要拿 expose()，而这个名字本身就是「这里碰了真值」的记号。
                        && t.dns_arg.as_ref().map(|a| a.expose()) == Some("/bin/true")
                },
            ),
            // ★ ★ 批 8 新加的一条。**它是被这道门逼出来的**：
            //   `zones` 一进 `TLS_SUBS`，下面那条 `other => panic!` 当场开火，
            //   于是「加了子指令却忘了接」在结构上做不到。这就是这道门的设计。
            //   ⚠ 这份配置**必须带上原生供应商**：`zones` 单独写不违法，但
            //   `dns cloudflare` 少了 `zones` 是编译期错误（G59 第 3 条），
            //   而我们要量的是「`zones` 的值落到 TlsConfig 上没有」。
            "zones" => (
                "a.com {\n  tls {\n    dns cloudflare env:CF_TOKEN\n    zones a.com b.com\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n",
                |t| t.zones == ["a.com", "b.com"],
            ),
            // ★ 这份**故意不写 `dns`**，理由见上面那段。
            "resolvers" => (
                "a.com {\n  tls {\n    resolvers 127.0.0.1:8053 127.0.0.1:8054\n  }\n  respond 200\n}\n",
                |t| t.resolvers == ["127.0.0.1:8053", "127.0.0.1:8054"],
            ),
            other => panic!(
                "`tls {other}` 是新加的子指令，而这条测试没有它的判据。\
                 先在这里写明「它该落到 TlsConfig 的哪个字段」，再去 compile.rs 接它——\
                 顺序反过来的话，接没接上就没有任何东西看得见。"
            ),
        };
        // ⚠ 不在这里 panic，**把问题收集起来一次报全**。
        //   `dns` 那份最小配置绕不开 `resolvers`（G58 要求它俩同时写），
        //   所以 `resolvers` 一旦没人接，`dns` 那份会先在伴生校验上编译失败。
        //   逐条 panic 的话只看得见第一条，而第一条恰好不是根因那条。
        let o = compile_str("test.Fulcrumfile", src);
        match o.config {
            None => problems.push(format!(
                "`tls {}` 那份最小配置编译不过：\n{}",
                s.name,
                o.render_diagnostics()
            )),
            Some(cfg) => {
                let tls = &cfg.sites[0].tls;
                if !landed(tls) {
                    problems.push(format!(
                        "`tls {}` 在 TLS_SUBS 里有位置，但它的值没落到 TlsConfig 上 —— \
                         多半是 compile.rs 的 match 没接它。产物：{tls:?}",
                        s.name
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

// ── ★ ★ ★ 把上面那道门的**形状**扫到所有同类表上（批 10）───────────
//
// ⚠ ⚠ 上面那道门只管 `tls`。而 `compile.rs` 里**有两处 `_ => {}` 还留着**：
//   全局选项那个 match、`reverse_proxy` 子指令那个 match。
//   它们今天都是满的（每条都有臂），所以下面这几道门今天全绿 ——
//   **它们守的是明天**：新加一条子指令而忘了接，在 `tls` 上会被逼出来，
//   在这几张表上此前不会。
//
// ★ 纪律：**修完一个「形状」要当场把同形的全扫一遍**。
//
// ★ 每张表一道门，而不是合成一道：合成之后一条红了看不出是哪张表，
//   而这几张表的「落地判据」类型本来就各不相同（落到 `Global` / 落到 `StepBody`）。

/// 取出第一个站点执行链上的第一步。
fn first_body(cfg: &fulcrum_config::model::StructuredConfig) -> &StepBody {
    &cfg.sites[0].chain[0].body
}

#[test]
fn reverse_proxy子指令全部有人接() {
    assert!(!REVERSE_PROXY_SUBS.is_empty(), "REVERSE_PROXY_SUBS 是空的");
    let mut problems: Vec<String> = Vec::new();
    for s in REVERSE_PROXY_SUBS {
        // ⚠ 每条判据都必须与**默认值不同**，否则「值被丢掉」与「值落对了」
        //   在产物上长得一模一样 —— 那样的判据在缺陷复现时照样给绿。
        #[allow(clippy::type_complexity)]
        let (line, landed): (&str, fn(&StepBody) -> bool) = match s.name {
            "lb_policy" => (
                "lb_policy least_conn",
                |b| matches!(b, StepBody::ReverseProxy { lb_policy, .. } if lb_policy == "least_conn"),
            ),
            "health_uri" => (
                "health_uri /h",
                |b| matches!(b, StepBody::ReverseProxy { health, .. } if health.uri.as_deref() == Some("/h")),
            ),
            "health_interval" => (
                "health_interval 3s",
                |b| matches!(b, StepBody::ReverseProxy { health, .. } if health.interval_ms == 3_000),
            ),
            "health_timeout" => (
                "health_timeout 7s",
                |b| matches!(b, StepBody::ReverseProxy { health, .. } if health.timeout_ms == 7_000),
            ),
            "health_status" => (
                "health_status 3xx",
                |b| matches!(b, StepBody::ReverseProxy { health, .. } if health.status == "3xx"),
            ),
            // ★ 批 10 接线的就是它。默认 30s，所以判据写 5s 才分得出「丢了」。
            "dns_refresh" => (
                "dns_refresh 5s",
                |b| matches!(b, StepBody::ReverseProxy { dns_refresh_ms, .. } if *dns_refresh_ms == 5_000),
            ),
            "passive_fail" => (
                "passive_fail 3",
                |b| matches!(b, StepBody::ReverseProxy { passive, .. } if passive.fail_threshold == Some(3)),
            ),
            "passive_window" => (
                "passive_window 9s",
                |b| matches!(b, StepBody::ReverseProxy { passive, .. } if passive.window_ms == Some(9_000)),
            ),
            "header_up" => (
                "header_up X-A 1",
                |b| matches!(b, StepBody::ReverseProxy { header_up, .. } if header_up.len() == 1),
            ),
            "header_down" => (
                "header_down X-B 2",
                |b| matches!(b, StepBody::ReverseProxy { header_down, .. } if header_down.len() == 1),
            ),
            "transport" => (
                "transport https",
                |b| matches!(b, StepBody::ReverseProxy { transport, .. } if transport == "https"),
            ),
            "tls_insecure_skip_verify" => (
                "tls_insecure_skip_verify",
                |b| matches!(b, StepBody::ReverseProxy { tls_insecure_skip_verify, .. } if *tls_insecure_skip_verify),
            ),
            // ★ 批 D。判据有意写 **v1** 而不是省略参数：省了就是 v2，
            //   而「接上了」与「没接上但默认恰好也是 v2」在那种写法下**分不出来**。
            //   ⚠ 这与上面 `dns_refresh` 写 5s 而不是 30s 是同一条理由。
            "proxy_protocol" => (
                "proxy_protocol v1",
                |b| matches!(b, StepBody::ReverseProxy { proxy_protocol, .. } if proxy_protocol.as_deref() == Some("v1")),
            ),
            // ★ 批 N。判据写 **3** 而不是 1：默认就是 1，写 1 的话「落到了」与
            //   「被丢掉了」在产物上完全同形 —— 与上面 `dns_refresh` 写 5s 同一条理由。
            //   ⚠ 地址必须与上面那份夹具的 `reverse_proxy 127.0.0.1:1` **逐字相同**。
            "weight" => (
                "weight 127.0.0.1:1 3",
                |b| matches!(b, StepBody::ReverseProxy { upstreams, .. } if upstreams.len() == 1 && upstreams[0].weight == 3),
            ),
            other => panic!(
                "`reverse_proxy {other}` 是新加的子指令，而这条测试没有它的判据。\
                 先在这里写明「它该落到 StepBody::ReverseProxy 的哪个字段」，\
                 再去 compile.rs 接它 —— 顺序反过来的话，接没接上就没有任何东西看得见。"
            ),
        };
        let src =
            format!("http://a.com {{\n  reverse_proxy 127.0.0.1:1 {{\n    {line}\n  }}\n}}\n");
        let o = compile_str("test.Fulcrumfile", &src);
        match o.config {
            None => problems.push(format!(
                "`reverse_proxy {}` 那份最小配置编译不过：\n{}",
                s.name,
                o.render_diagnostics()
            )),
            Some(cfg) => {
                let b = first_body(&cfg);
                if !landed(b) {
                    problems.push(format!(
                        "`reverse_proxy {}` 在 REVERSE_PROXY_SUBS 里有位置，\
                         但它的值没落到 StepBody 上 —— 多半是 compile.rs 的 match 没接它。产物：{b:?}",
                        s.name
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn 全局选项全部有人接() {
    assert!(!GLOBAL_OPTIONS.is_empty(), "GLOBAL_OPTIONS 是空的");
    let mut problems: Vec<String> = Vec::new();
    for s in GLOBAL_OPTIONS {
        #[allow(clippy::type_complexity)]
        let (line, landed): (&str, fn(&fulcrum_config::model::Global) -> bool) = match s.name {
            "acme_email" => ("acme_email a@b.com", |g| {
                g.acme_email.as_deref() == Some("a@b.com")
            }),
            "acme_ca" => ("acme_ca https://ca.example/dir", |g| {
                g.acme_ca.as_deref() == Some("https://ca.example/dir")
            }),
            // ★ 它在批 9 才接线，而在那之前正是「表里有位置、没人接」的活样本。
            "admin" => ("admin unix//tmp/x.sock", |g| {
                g.admin.as_deref() == Some("unix//tmp/x.sock")
            }),
            "default_sni" => ("default_sni a.com", |g| {
                g.default_sni.as_deref() == Some("a.com")
            }),
            "grace_period" => ("grace_period 11s", |g| g.grace_period_ms == Some(11_000)),
            // ★ ★ 判据故意用 `false` 而不是 `true`：这个选项**默认就是 true**，
            //   拿 `true` 去测的话，一个「表里有位置、compile 根本没接它」的实现
            //   照样能过 —— 那正是这条测试存在的全部理由。
            "auto_http_redirect" => ("auto_http_redirect false", |g| !g.auto_http_redirect),
            // ⚠ `fallback_nginx` / `fallback_caddy` 两条（M2 批 G）
            //   从 `GLOBAL_OPTIONS` 里删除（G98）。它们的接班判据在
            //   `crates/fulcrum-runtime/tests/fallback.rs`：**写了它们要说清它去哪了**。
            // ★ 批 D。判据写**两个**网段，因为 compile 里最容易写错的那一版是
            //   `g.proxy_protocol_from = v`（只收第一个参数）—— 而那一版
            //   拿一个网段去测是**过得了**的。
            "proxy_protocol_from" => ("proxy_protocol_from 10.0.0.0/8 192.168.0.0/16", |g| {
                g.proxy_protocol_from == ["10.0.0.0/8", "192.168.0.0/16"]
            }),
            other => panic!(
                "全局选项 `{other}` 是新加的，而这条测试没有它的判据。\
                 先在这里写明「它该落到 Global 的哪个字段」，再去 compile.rs 接它。"
            ),
        };
        let src = format!("{{\n  {line}\n}}\nhttp://a.com {{\n  respond 200\n}}\n");
        let o = compile_str("test.Fulcrumfile", &src);
        match o.config {
            None => problems.push(format!(
                "全局选项 `{}` 那份最小配置编译不过：\n{}",
                s.name,
                o.render_diagnostics()
            )),
            Some(cfg) => {
                if !landed(&cfg.global) {
                    problems.push(format!(
                        "全局选项 `{}` 在 GLOBAL_OPTIONS 里有位置，但它的值没落到 Global 上。\
                         产物：{:?}",
                        s.name, cfg.global
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn log_cache_file_server_的子指令也全部有人接() {
    // ★ 这三张表各只有两条，合成一道门读起来更清楚；
    //   出问题时消息里带着表名，仍然指得到具体那一条。
    let mut problems: Vec<String> = Vec::new();

    for s in LOG_SUBS {
        #[allow(clippy::type_complexity)]
        let (line, landed): (&str, fn(&fulcrum_config::model::LogConfig) -> bool) = match s.name {
            "output" => ("output stderr", |l| l.output.as_deref() == Some("stderr")),
            "level" => ("level debug", |l| l.level.as_deref() == Some("debug")),
            // ── M2 批 L 第 ③ 步 ────────────────────────────────────────
            // ⚠ 判据钉**两个名字**而不是 `!is_empty()`：后者对一个只收下第一个参数
            //   的实现照样给绿，而现场是「我写了三个头，日志里只有一个」。
            "headers" => ("headers User-Agent Referer", |l| {
                l.headers == vec!["User-Agent".to_string(), "Referer".to_string()]
            }),
            "resp_headers" => ("resp_headers Content-Type X-Cache", |l| {
                l.resp_headers == vec!["Content-Type".to_string(), "X-Cache".to_string()]
            }),
            other => panic!("`log {other}` 是新加的子指令，这条测试没有它的判据。"),
        };
        let src = format!("http://a.com {{\n  log {{\n    {line}\n  }}\n  respond 200\n}}\n");
        let o = compile_str("test.Fulcrumfile", &src);
        match o.config {
            None => problems.push(format!(
                "`log {}` 编译不过：\n{}",
                s.name,
                o.render_diagnostics()
            )),
            Some(cfg) => match &cfg.sites[0].log {
                None => problems.push(format!("`log {}`：整个 log 块都没落地", s.name)),
                Some(l) => {
                    if !landed(l) {
                        problems.push(format!("`log {}` 的值没落到 LogConfig 上：{l:?}", s.name));
                    }
                }
            },
        }
    }

    for s in CACHE_SUBS {
        #[allow(clippy::type_complexity)]
        let (line, landed): (&str, fn(&StepBody) -> bool) = match s.name {
            "ttl" => (
                "ttl 12s",
                |b| matches!(b, StepBody::Cache { ttl_ms, .. } if *ttl_ms == Some(12_000)),
            ),
            "max_size" => (
                "max_size 4MB",
                |b| matches!(b, StepBody::Cache { max_size_bytes, .. } if max_size_bytes.is_some()),
            ),
            // ── M2 批 G ────────────────────────────────────────────────
            // ⚠ 判据用一个**具体的数**，不是 `is_some()`：`is_some()` 只证明
            //   「有人给它赋了值」，而一个把 capacity 赋成 max_size 的实现照样过。
            "capacity" => (
                "capacity 64MB",
                |b| matches!(b, StepBody::Cache { capacity_bytes, .. } if *capacity_bytes == Some(64_000_000)),
            ),
            // ── M2 批 H ────────────────────────────────────────────────
            // ⚠ 同样钉**具体的值**而不是 `is_some()`：一个把目录读成别的字符串
            //   （比如顺手 trim 掉了什么）的实现，`is_some()` 照样给绿，
            //   而现场是「缓存落到了一个我没写过的目录里」。
            "disk" => (
                "disk /var/cache/fulcrum",
                |b| matches!(b, StepBody::Cache { disk_dir, .. } if disk_dir.as_deref() == Some("/var/cache/fulcrum")),
            ),
            other => panic!("`cache {other}` 是新加的子指令，这条测试没有它的判据。"),
        };
        let src = format!("http://a.com {{\n  cache {{\n    {line}\n  }}\n}}\n");
        let o = compile_str("test.Fulcrumfile", &src);
        match o.config {
            None => problems.push(format!(
                "`cache {}` 编译不过：\n{}",
                s.name,
                o.render_diagnostics()
            )),
            Some(cfg) => {
                let b = first_body(&cfg);
                if !landed(b) {
                    problems.push(format!("`cache {}` 的值没落到 StepBody 上：{b:?}", s.name));
                }
            }
        }
    }

    for s in FILE_SERVER_SUBS {
        #[allow(clippy::type_complexity)]
        let (line, landed): (&str, fn(&StepBody) -> bool) = match s.name {
            "root" => (
                "root /srv",
                |b| matches!(b, StepBody::FileServer { root, .. } if root.as_deref() == Some("/srv")),
            ),
            "index" => (
                "index a.html b.html",
                |b| matches!(b, StepBody::FileServer { index, .. } if index == &["a.html", "b.html"]),
            ),
            // ── M2 批 F（G87 / G88）──────────────────────────────────────
            // ⚠ 三条都用**非默认值**（默认是 true / 空），否则一个「读都没读、
            //   直接用默认值」的实现会照样让这条判据全绿。
            "follow_symlinks" => (
                "follow_symlinks false",
                |b| matches!(b, StepBody::FileServer { follow_symlinks, .. } if !*follow_symlinks),
            ),
            "hide" => (
                "hide a b",
                |b| matches!(b, StepBody::FileServer { hide, .. } if hide == &["a", "b"]),
            ),
            "hide_defaults" => (
                "hide_defaults false",
                |b| matches!(b, StepBody::FileServer { hide_defaults, .. } if !*hide_defaults),
            ),
            // ── M2 批 I（G99）────────────────────────────────────────────
            // ⚠ 同样钉**具体的值**：一个把顺序反过来（或者去重去错）的实现，
            //   `is_empty()` 那种判据照样给绿，而挑旁文件走的是这个列表。
            "precompressed" => (
                "precompressed br gzip",
                |b| matches!(b, StepBody::FileServer { precompressed, .. } if precompressed == &["br", "gzip"]),
            ),
            other => panic!("`file_server {other}` 是新加的子指令，这条测试没有它的判据。"),
        };
        // ⚠ M2 批 F 起 `root` 必填 ⇒ 除了 `root` 自己那一轮，块里都要先垫一条。
        let block = if s.name == "root" {
            line.to_string()
        } else {
            format!("root /srv\n    {line}")
        };
        let src = format!("http://a.com {{\n  file_server {{\n    {block}\n  }}\n}}\n");
        let o = compile_str("test.Fulcrumfile", &src);
        match o.config {
            None => problems.push(format!(
                "`file_server {}` 编译不过：\n{}",
                s.name,
                o.render_diagnostics()
            )),
            Some(cfg) => {
                let b = first_body(&cfg);
                if !landed(b) {
                    problems.push(format!(
                        "`file_server {}` 的值没落到 StepBody 上：{b:?}",
                        s.name
                    ));
                }
            }
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

// ── G59：原生 DNS 供应商的两条编译期硬约束 ──────────────────────────────────
//
// ⚠ 理由不是洁癖：拿到某域的 DNS 写权限 = **能为该域签发任意证书**，
//   还能改 MX 劫持邮件。它比 On-Demand 被刷爆严重得多。

#[test]
fn 凭据可以内联_但打错的来源前缀要红() {
    // ★ ★ ★ **这条测试换过一次契约（批 22），换法本身留在这里。**
    //
    //   旧契约（G59 第 1 条原文）：凭据**写不进** DSL，只认 `env:` / `file:` 两种来源，
    //   任何没前缀的值都是编译期错误。判据是**白名单**，理由是：
    //   「看起来像 token 就报错」的黑名单要去猜什么样子算 token，
    //   而猜错的那一次恰恰就是真 token 被放行的那一次。
    //
    //   新契约（owner 拍板，Caddy 形状）：**写得下**，一份配置文件就能跑完。
    //   ⚠ 白名单那条理由没有失效，它只是**换了落点**——从「拦字面量」变成
    //   「拦打错的前缀」：既然不写前缀就是值本身，那么 `fil:/path` 会被
    //   当成凭据发给对端，现场是「凭据不对」而真正的原因是打错了三个字母。
    //
    //   ⇒ 代价挪到了别处，而不是消失：配置文件从此是秘密（装载期权限门）、
    //   `compile` 默认脱敏、`Debug`/`Display` 默认脱敏。见 `secret.rs` 与 `secret_guard.rs`。

    // ① 内联字面量：现在合法
    let cfg = ok(
        "a.com {\n  tls {\n    dns cloudflare cfat_realtoken1234567890\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
    );
    let arg = cfg.sites[0]
        .tls
        .dns_arg
        .as_ref()
        .expect("凭据没落到结构化配置上");
    assert!(arg.is_sensitive(), "内联字面量必须被标成敏感");
    assert_eq!(arg.expose(), "cfat_realtoken1234567890");

    // ② ★ 而它**默认不会被印出来**——这是允许内联之后唯一撑得住的东西
    assert_eq!(arg.display(), fulcrum_config::secret::REDACTED);
    assert_eq!(
        format!("{arg:?}"),
        format!("Secret({})", fulcrum_config::secret::REDACTED)
    );
    let json = serde_json::to_string(&cfg).expect("序列化不了");
    assert!(
        !json.contains("cfat_realtoken1234567890"),
        "★ 默认序列化把真凭据吐出来了 —— 而这份 JSON 正是 POST /load 的载荷"
    );

    // ③ 两种来源写法照旧合法，而且**不算敏感**（它们是指针，不是秘密）
    for src in [
        "a.com {\n  tls {\n    dns cloudflare env:CF_API_TOKEN\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
        "a.com {\n  tls {\n    dns dnspod file:/run/secrets/dp\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
    ] {
        let c = ok(src);
        let a = c.sites[0].tls.dns_arg.as_ref().unwrap();
        assert!(!a.is_sensitive(), "`{}` 被当成秘密了", a.expose());
    }

    // ④ ★ ★ 打错的来源前缀必须红 —— 这是白名单那条理由的新落点
    for bad in [
        "fil:/run/secrets/cf",
        "ENV:CF_API_TOKEN",
        "files:/x",
        "en:CF",
    ] {
        let src = format!(
            "a.com {{\n  tls {{\n    dns cloudflare {bad}\n    zones a.com\n    resolvers 1.1.1.1:53\n  }}\n  respond 200\n}}\n"
        );
        assert!(
            codes(&src).contains(&DiagCode::BAD_CREDENTIAL_SOURCE),
            "`{bad}` 该被当成打错的前缀拦下，而不是当成凭据发出去"
        );
    }

    // ⑤ 自证：**真的带冒号的凭据**有出路（`literal:`），否则第 ④ 条会误伤
    let c = ok(
        "a.com {\n  tls {\n    dns dnspod literal:12345,abc:def\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
    );
    assert!(c.sites[0].tls.dns_arg.as_ref().unwrap().is_sensitive());
}

#[test]
fn 原生供应商不给凭据来源也是错() {
    assert!(
        !codes("a.com {\n  tls {\n    dns cloudflare\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n").is_empty()
    );
}

#[test]
fn 原生供应商必须声明_zones() {
    // ★ ★ G59 第 3 条。对 DNSPod 这是**唯一**的范围约束——它的 token 是账号级的，
    //   没有任何端点能问出「它覆盖哪些 zone」。
    for p in ["cloudflare", "dnspod"] {
        let src = format!(
            "a.com {{\n  tls {{\n    dns {p} env:TOK\n    resolvers 1.1.1.1:53\n  }}\n  respond 200\n}}\n"
        );
        assert!(
            !codes(&src).is_empty(),
            "`dns {p}` 少了 zones 应当编译期就红"
        );
    }
}

#[test]
fn exec_hook_不受那两条约束() {
    // ⚠ 反向判据：exec hook 的第二个参数是**程序路径**，不是凭据来源；
    //   它自己去环境变量/文件里拿凭据。一个把这两条约束也套到 exec 上的实现，
    //   会让已经在用的 DNS-01 配置**全部编译不过**——而上面那几条测试对此完全无感。
    ok(
        "*.a.com {\n  tls {\n    dns exec /etc/fulcrum/hook.sh\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
    );
}

#[test]
fn http地址默认关掉tls() {
    let cfg = ok("http://a.com {\n  respond 200\n}\n");
    assert_eq!(
        serde_json::to_value(&cfg.sites[0].tls).unwrap()["mode"],
        serde_json::json!("off")
    );
}

// ── 产物 ────────────────────────────────────────────────────────────────────

#[test]
fn 产物能序列化成json并读回来() {
    let cfg = ok(
        "example.com {\n  encode gzip zstd\n  reverse_proxy 10.0.0.1:8080 10.0.0.2:8080 {\n    lb_policy least_conn\n    health_uri /health\n    dns_refresh 15s\n  }\n}\n",
    );
    let json = fulcrum_config::model::to_pretty_json(&cfg).unwrap();
    let back: fulcrum_config::model::StructuredConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
    assert!(json.contains("\"dns_refresh_ms\": 15000"), "{json}");
}

// ── 自动 HTTP 重定向（G12 的后半句，批 21）──────────────────────
//
// ⚠ ⚠ 这一组测试存在的直接原因：**文档承诺了三个月，代码里一行都没有**。
//   DSL 参考 §二一直印着「`example.com` = HTTPS，并自动把 HTTP 重定向过来」，
//   而 08-22 上线当天实测 80 端口上根本没有监听。
//   ★ 所以这里钉的不只是「功能对不对」，还有**它是可见的**：合成物必须出现在
//   结构化配置里（`compile` / `plan` 都读得到），而不是数据面里一条看不见的特判。

#[test]
fn 自动https的站点会合成一个80端口的308跳转() {
    let cfg = ok("example.com, www.example.com {\n  respond 200\n}\n");
    assert_eq!(
        cfg.sites.len(),
        2,
        "应当多出一个合成站点：{:?}",
        cfg.sites.len()
    );
    let r = &cfg.sites[1];
    let hosts: Vec<&str> = r.addresses.iter().map(|a| a.host.as_str()).collect();
    assert_eq!(hosts, vec!["example.com", "www.example.com"]);
    assert!(
        r.addresses
            .iter()
            .all(|a| a.port == 80 && a.scheme == "http")
    );
    // ★ 合成站点自己**不要证书**：它就是那条跳转。少了这一条，
    //   TLS 那一层会把它当成「要自动签发」，装载日志里多一条假的 ⏳。
    assert!(
        r.addresses.iter().all(|a| !a.auto_https),
        "合成的跳转站点不该要证书"
    );
    assert_eq!(r.chain.len(), 1);
    match &r.chain[0].body {
        StepBody::Redir { to, code } => {
            assert_eq!(to, "https://{host}{uri}");
            // ⚠ 308 而不是 301/302：**保留方法与 body**，
            //   否则一个 POST 会变成 GET，而现场表现是「表单提交丢了」。
            assert_eq!(*code, 308, "必须是 308");
        }
        other => panic!("合成的应当是 redir，拿到 {other:?}"),
    }
}

#[test]
fn 关掉之后一个都不合成() {
    let cfg = ok("{\n  auto_http_redirect false\n}\n\nexample.com {\n  respond 200\n}\n");
    assert_eq!(cfg.sites.len(), 1, "关掉了还合成：{:?}", cfg.sites.len());
    assert!(!cfg.global.auto_http_redirect);
}

#[test]
fn 默认是开的_而且没写全局块也算开() {
    // ⚠ ⚠ 这一条钉的是 `Default` 的实现方式：`derive(Default)` 给 bool 的是 **false**，
    //   而这个字段的默认必须是 **true**。一旦 derive，一份**没写全局块**的配置
    //   （最常见的那种）就会悄悄关掉 HTTP 跳转，而文档印着它是开的。
    let cfg = ok("example.com {\n  respond 200\n}\n");
    assert!(cfg.global.auto_http_redirect, "默认必须是开");
    assert_eq!(cfg.sites.len(), 2);
}

#[test]
fn 用户自己写了http站点就不碰那个主机名() {
    // ★ 用户的写法优先：合成只补**没人管**的那些主机名。
    let cfg = ok("example.com, www.example.com {\n  respond 200\n}\n\
         http://example.com {\n  respond 200 \"mine\"\n}\n");
    let synth: Vec<&fulcrum_config::model::SiteConfig> = cfg
        .sites
        .iter()
        .filter(|s| {
            matches!(
                s.chain.first().map(|st| &st.body),
                Some(StepBody::Redir { .. })
            )
        })
        .collect();
    assert_eq!(synth.len(), 1);
    let hosts: Vec<&str> = synth[0].addresses.iter().map(|a| a.host.as_str()).collect();
    assert_eq!(
        hosts,
        vec!["www.example.com"],
        "example.com 已经有人管了，不该被合成接走"
    );
}

#[test]
fn 盖过端口兜底站点时要说出来() {
    // ⚠ 站点索引是「精确 → 通配 → 端口兜底」，所以合成的精确站点会盖过 `:80` 兜底 ——
    //   这是一次**行为改变**，而沉默地改掉别人配置的行为正是本仓库反复点名的那种。
    let cs = codes(
        "example.com {\n  respond 200\n}\n\
         :80 {\n  respond 200 \"catch-all\"\n}\n",
    );
    assert!(
        cs.contains(&DiagCode::AUTO_REDIRECT_SHADOWS),
        "盖过兜底站点却没说：{cs:?}"
    );
}

// ── M2 批 H：`cache { disk … }` 的两道编译期检查 ────────────────────────────

#[test]
fn cache_的_disk_必须是绝对路径() {
    // ⚠ G91 那条理由一字不改地适用：相对路径按进程 cwd 解析，而 systemd 下
    //   cwd 是 `/`、开发机上是项目目录 ⇒ 同一份配置会把缓存落到两个地方，
    //   ★ 而两处都「看起来正常」。
    let cs = codes("a.com {\n  cache {\n    disk var/cache\n  }\n  reverse_proxy 127.0.0.1:1\n}\n");
    assert!(
        cs.contains(&DiagCode::PATH_NOT_ABSOLUTE),
        "相对目录没被拦下：{cs:?}"
    );
    // 反向那一半：绝对路径必须过。⚠ 少了它，一条「什么都拦」的实现照样全绿。
    ok("a.com {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  reverse_proxy 127.0.0.1:1\n}\n");
}

#[test]
fn 两个_cache_块写不同的_disk_是错误() {
    // ★ ★ 缓存后端是**进程级**的：两个不同的值里必有一个是
    //   「你以为生效、其实没有」的。
    let cs = codes(
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/two\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    assert!(
        cs.contains(&DiagCode::CACHE_BACKEND_CONFLICT),
        "两个不同的 disk 没被拦下：{cs:?}"
    );
}

#[test]
fn 一个写了_disk_一个没写也是错误() {
    // ⚠ 这一半最容易漏：两条都「合法」，只是它们要的后端不是同一个。
    //   ★ 不拦的话，没写 disk 的那个站点会拿到一个磁盘缓存（或者反过来），
    //   而配置文件里一个字都看不出来。
    let cs = codes(
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    assert!(
        cs.contains(&DiagCode::CACHE_BACKEND_CONFLICT),
        "「一个写了、一个没写」没被拦下：{cs:?}"
    );
}

#[test]
fn 两个_cache_块写同一个_disk_没问题() {
    // ★ 反向那一半：一致就该放行。⚠ 少了它，一条「只要有两个 cache 就报错」
    //   的实现会让上面两条全绿，而它把一份完全正确的配置也拦了。
    ok(
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/one\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    // 都不写 disk（内存后端）同样一致。
    ok(
        "a.com {\n  cache {\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    ttl 2m\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
}

// ── M2 批 L 第 ③ 步：白名单头里的四个名字是编译期错误 ────────────────────────

#[test]
fn 白名单里写敏感头是编译期错误_两条路各拦一次() {
    // ⚠ ⚠ 四个名字 × 两条子指令 = 八种写法，**逐个走一遍**。
    //   ★ 只测 `headers` 那一半的话，`resp_headers` 上漏一个就是一个泄漏面
    //   （`Set-Cookie` 恰恰只会出现在响应上）。
    for name in [
        "Authorization",
        "cookie",
        "SET-COOKIE",
        "Proxy-Authorization",
    ] {
        for which in ["headers", "resp_headers"] {
            let src =
                format!("http://a.com {{\n  log {{\n    {which} {name}\n  }}\n  respond 200\n}}\n");
            let cs = codes(&src);
            assert!(
                cs.contains(&DiagCode::SENSITIVE_HEADER_LOGGED),
                "`log {{ {which} {name} }}` 没被拦下：{cs:?}"
            );
        }
    }
}

#[test]
fn 一个普通头名不许被当成敏感头() {
    // ★ ★ 反证。⚠ 少了它，一条「见 `headers` 就报错」的实现会让上面那条全绿，
    //   而它把整个功能拦死了 —— **「挡住坏的」与「别连好的一起挡掉」要各自一条判据**。
    let cfg = ok(
        "http://a.com {\n  log {\n    headers User-Agent Referer\n    \
         resp_headers Content-Type\n  }\n  respond 200\n}\n",
    );
    let l = cfg.sites[0].log.as_ref().expect("log 块该落地");
    assert_eq!(l.headers, vec!["User-Agent", "Referer"]);
    assert_eq!(l.resp_headers, vec!["Content-Type"]);
}

#[test]
fn 敏感头的诊断指着那个名字_不是指着整个块() {
    // ★ 一条指错地方的诊断比没有诊断更费时间 —— 人会先去看它指的那一行。
    //   ⚠ 这里量的是**列**：`headers` 后面那个名字，而不是 `log` 或 `headers` 本身。
    let src = "http://a.com {\n  log {\n    headers X-Ok Cookie\n  }\n  respond 200\n}\n";
    let o = compile_str("test.Fulcrumfile", src);
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::SENSITIVE_HEADER_LOGGED)
        .expect("该有这条诊断");
    let at = &src[d.span.start..d.span.end];
    assert_eq!(at, "Cookie", "诊断指着的是「{at}」，不是那个敏感头本身");
}

// ── M2 批 M：`metrics` 是站点块里的终结指令（G116）─────────────────────────────

#[test]
fn metrics进链且夹在respond与reverse_proxy之间() {
    // ★ 三条**倒着写**，让排序有活干：只断言「编译得过」的话，序号写成 65 或 85
    //   照样全绿，而那时 `metrics` 已经跑在错误的位置上了（G49 点名的那个形状）。
    // ⚠ 三条都带匹配器 —— 否则第一条终结类会把后面两条挡死，测的就不是排序了。
    let cfg = ok(
        "http://a.com {\n  reverse_proxy /api 127.0.0.1:3000\n  metrics /metrics\n  \
         respond /x 200\n}\n",
    );
    let steps: Vec<(u16, &str)> = cfg.sites[0]
        .chain
        .iter()
        .map(|s| (s.order, s.body.directive_name()))
        .collect();
    assert_eq!(
        steps,
        vec![(70, "respond"), (75, "metrics"), (80, "reverse_proxy")]
    );
    assert!(matches!(cfg.sites[0].chain[1].body, StepBody::Metrics));
}

#[test]
fn metrics不收参数() {
    // ⚠ `abc` 不以 `/` `@` `*` 开头 ⇒ 它到不了行内匹配器那条路，是真的多余参数。
    let cs = codes("http://a.com {\n  metrics abc\n}\n");
    assert!(
        cs.contains(&DiagCode::BAD_ARITY),
        "`metrics abc` 该报 arity 错误，实际：{cs:?}"
    );
}

#[test]
fn 没有匹配器的metrics要出警告() {
    let o = compile_str("t.Fulcrumfile", "http://a.com {\n  metrics\n}\n");
    // ★ 是 **warning** 不是 error：开在内网可信段上的指标端点是正当配置。
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::METRICS_UNGUARDED)
        .expect("裸 `metrics` 该出 G116 那条警告");
    // 文案要说出后果，不是只说「建议加个匹配器」。
    assert!(d.label.contains("指标"), "{}", d.label);
    // ★ ★ note 里必须把「什么算、什么不算」逐个说出来 —— 那是这条诊断现在的
    //   全部价值所在：一句笼统的「加个匹配器吧」会把人直接引向
    //   `handle /metrics { metrics }`，而那正是它要抓的那个裸奔配置本身。
    let note = d.note.as_deref().unwrap_or("");
    for word in ["remote_ip", "header", "path", "host", "method", "query"] {
        assert!(note.contains(word), "note 里没说 `{word}`：{note}");
    }
}

/// 这一份里有没有 `FUL-DSL-0037`。⚠ 顺带钉住「它从来不是 error」——
/// 一条把配置拒掉的门会把「指标开在内网可信段上」这种正当配置一起挡掉。
fn 有裸奔警告(src: &str) -> bool {
    let o = compile_str("t.Fulcrumfile", src);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    o.diagnostics
        .items()
        .iter()
        .any(|d| d.code == DiagCode::METRICS_UNGUARDED)
}

#[test]
fn 限制得了来源的匹配器才让metrics免于警告() {
    // ★ ★ **反证，两个方向缺一不可。** ⚠ 少了这一条，把诊断写成「见 `metrics` 就报」
    //   也能让正方向那几条全绿 —— 而一条恒报的警告等于没有警告。
    for (what, src) in [
        // 外层 `handle` 带 `remote_ip`（文档里印的就是这一种）
        (
            "handle @remote_ip",
            "http://a.com {\n  @i remote_ip 10.0.0.0/8\n  handle @i {\n    metrics\n  }\n  \
             respond 403\n}\n",
        ),
        // 这一步自己带
        (
            "metrics @remote_ip",
            "http://a.com {\n  @i remote_ip 10.0.0.0/8\n  metrics @i\n}\n",
        ),
        // 外层 `route`（保序逃生口里也罩得住）
        (
            "route @remote_ip",
            "http://a.com {\n  @i remote_ip 10.0.0.0/8\n  route @i {\n    metrics\n  }\n}\n",
        ),
        // ★ `header`：一个共享密钥可以放在这里 ⇒ 它把两个发同样请求的客户端分得开
        (
            "handle @header",
            "http://a.com {\n  @tok header X-Token abc\n  handle @tok {\n    metrics\n  }\n}\n",
        ),
        // ★ `not { remote_ip … }`：`condition()` 把 `not` 拆开、给里面那条打 `negate`，
        //   ⇒ 存下来的 `kind` 就是 `remote_ip`，它照样算保护（不需要为 `not` 写特例）。
        (
            "not { remote_ip … }",
            "http://a.com {\n  @notlocal {\n    not {\n      remote_ip 127.0.0.1/32\n    }\n  }\n  \
             handle @notlocal {\n    metrics\n  }\n}\n",
        ),
        // ★ AND 里只要有一条限制得了来源就够了（多条件是 AND，见 `Matcher`）
        (
            "remote_ip + path 一起写",
            "http://a.com {\n  @i {\n    remote_ip 10.0.0.0/8\n    path /metrics\n  }\n  \
             handle @i {\n    metrics\n  }\n}\n",
        ),
    ] {
        assert!(!有裸奔警告(src), "{what} 限制得了来源，不该出警告：\n{src}");
    }
}

#[test]
fn 限制不了来源的匹配器不算保护() {
    // ⚠ ⚠ ⚠ **本轮的核心。** 判据不是「有没有匹配器」，而是
    //   「**它能不能把两个发同样请求的客户端分开**」。
    //   ★ 路径 / host / method / query 全都是**请求里的东西，任何客户端都能照着发一份**
    //   —— 把它们算成保护，这条诊断就在它唯一要抓的东西上沉默：
    //   从 nginx / Caddy 迁过来的人第一反应正是写一条路径。
    for (what, src) in [
        // ① ★ 行内路径匹配器 —— **最可能出现的那个裸奔配置**
        ("行内路径", "http://a.com {\n  metrics /metrics\n}\n"),
        // ② 命名的路径匹配器，套在 handle 外面（看起来最像「圈住了」的那一种）
        (
            "handle @path",
            "http://a.com {\n  @p path /metrics\n  handle @p {\n    metrics\n  }\n}\n",
        ),
        // ③ host：Host 头是请求里的东西，谁都能写
        (
            "handle @host",
            "http://a.com {\n  @byhost host a.example\n  handle @byhost {\n    metrics\n  }\n}\n",
        ),
        // ④ method
        (
            "handle @method",
            "http://a.com {\n  @g method GET\n  handle @g {\n    metrics\n  }\n}\n",
        ),
        // ⑤ query
        (
            "handle @query",
            "http://a.com {\n  @q query k=v\n  handle @q {\n    metrics\n  }\n}\n",
        ),
        // ⑥ 一个匹配器都没有
        ("裸 metrics", "http://a.com {\n  metrics\n}\n"),
        // ⑦ `handle` 的兜底分支：对所有人都开着，与裸写一模一样
        (
            "兜底 handle 分支",
            "http://a.com {\n  handle {\n    metrics\n  }\n}\n",
        ),
        // ⑧ `route` 没带匹配器
        (
            "裸 route",
            "http://a.com {\n  route {\n    metrics\n  }\n}\n",
        ),
        // ⑨ 显式 `*`
        ("显式 `*`", "http://a.com {\n  metrics *\n}\n"),
    ] {
        assert!(有裸奔警告(src), "{what} 限制不了来源，该出警告：\n{src}");
    }
}

#[test]
fn 两条判据问的是两件事_同一份配置上给出不同答案() {
    // ★ ★ ★ **这是「没把两条判据合并」的活证据。**
    //   `FUL-DSL-0037` 问「这一步对**谁**开着」（`restricts_source`）；
    //   `FUL-DSL-0028` 问「这一步会不会把它后面的都吃掉」（`is_unconditional`）。
    //   ⚠ 合用一个谓词的话它们将来会一起漂走，而漂走那天没有任何东西会说。
    //
    // 下面这一份里，`metrics /metrics` 让两条判据**必须**给出相反的答案：
    //   · 对 0037：路径匹配器限制不了来源 ⇒ **报**；
    //   · 对 0028：它带着匹配器、不会无条件终结 ⇒ 后面那条 `reverse_proxy` **不报**。
    let src = "http://a.com {\n  metrics /metrics\n  reverse_proxy 127.0.0.1:3000\n}\n";
    let o = compile_str("t.Fulcrumfile", src);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let cs: Vec<DiagCode> = o.diagnostics.items().iter().map(|d| d.code).collect();
    assert!(
        cs.contains(&DiagCode::METRICS_UNGUARDED),
        "路径匹配器限制不了来源，0037 该报：{cs:?}"
    );
    assert!(
        !cs.contains(&DiagCode::UNREACHABLE_STEP),
        "`metrics /metrics` 带着匹配器、不会无条件终结，0028 不该报：{cs:?}"
    );

    // ⇒ 反向那一半：把匹配器拿掉，两条判据就都该报了（0028 指的是后面那条转发）。
    let bare = "http://a.com {\n  metrics\n  reverse_proxy 127.0.0.1:3000\n}\n";
    let cs = codes(bare);
    assert!(cs.contains(&DiagCode::METRICS_UNGUARDED), "{cs:?}");
    assert!(cs.contains(&DiagCode::UNREACHABLE_STEP), "{cs:?}");
}

// ── M2 批 N 任务 1：`weight` 的三条诊断（裁决 R1 / R3）──────────────────────

#[test]
fn weight_的地址对不上时报错并且把上游清单原样列出来() {
    // ★ ★ 地址比对是**逐字相同**，不做归一化：这份配置里写的是 `backend:80`，
    //   而 `weight` 指的是 `backend` —— 运行时那边它们会归一成同一个，
    //   但归一化住在 `fulcrum-runtime`，在配置层再写一份「差不多的」就是分家。
    let src =
        "http://a.com {\n  reverse_proxy backend:80 other:80 {\n    weight backend 3\n  }\n}\n";
    let o = compile_str("test.Fulcrumfile", src);
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_WEIGHT_UPSTREAM)
        .unwrap_or_else(|| panic!("该报 FUL-DSL-0038，实际：\n{}", o.render_diagnostics()));
    // ⚠ ⚠ 判据不是「报了一条」，是**那条错误里真的把清单列出来了**：
    //   只说「找不到」等于让人去猜自己上一行写的到底是什么，
    //   而这条错误最常见的成因恰恰是两处写法差了一个端口。
    let 全文 = format!("{} {} {:?} {:?}", d.message, d.label, d.help, d.note);
    for up in ["backend:80", "other:80"] {
        assert!(全文.contains(up), "错误里没有把上游 `{up}` 列出来：{全文}");
    }
    assert!(o.diagnostics.has_errors(), "这是装载期错误，不是 warning");
    // ★ 反向那一半：**逐字相同**的那条不许被拦（否则功能整个是死的）。
    let cfg = ok(
        "http://a.com {\n  reverse_proxy backend:80 other:80 {\n    weight backend:80 3\n  }\n}\n",
    );
    let StepBody::ReverseProxy { upstreams, .. } = &cfg.sites[0].chain[0].body else {
        panic!("应当是 reverse_proxy")
    };
    assert_eq!(upstreams[0].weight, 3);
    assert_eq!(upstreams[1].weight, 1, "没写 weight 的上游权重是 1");
}

#[test]
fn 同一个上游写两次_weight_是错误而不是后写的赢() {
    let src =
        "http://a.com {\n  reverse_proxy x:1 x:2 {\n    weight x:1 3\n    weight x:1 5\n  }\n}\n";
    let cs = codes(src);
    assert!(
        cs.contains(&DiagCode::DUPLICATE_WEIGHT),
        "重复的 weight 没被拦下：{cs:?}"
    );
    // ⛔ 「后写的赢」是静默的：删掉或挪动其中一行会改掉权重，而配置里看不出异常。
    let o = compile_str("t.Fulcrumfile", src);
    assert!(o.diagnostics.has_errors(), "重复必须是 error");
    // ★ 反向那一半：两条 `weight` 指着**不同**的上游是正常写法，不许被拦。
    let cfg = ok(
        "http://a.com {\n  reverse_proxy x:1 x:2 {\n    weight x:1 3\n    weight x:2 7\n  }\n}\n",
    );
    let StepBody::ReverseProxy { upstreams, .. } = &cfg.sites[0].chain[0].body else {
        panic!("应当是 reverse_proxy")
    };
    assert_eq!(
        upstreams.iter().map(|u| u.weight).collect::<Vec<_>>(),
        vec![3, 7]
    );
}

#[test]
fn 权重值域外的写法一律是错误_包括零() {
    // ★ ★ `0` 不合法是**有意的**：「不参与调度」只有一种表达方式 —— 覆盖层的 `disable`。
    //   两条路做同一件事就是分家，而分家那天没有任何东西会说。
    for bad in ["0", "-1", "65536", "3s", "abc", "1.5"] {
        let src =
            format!("http://a.com {{\n  reverse_proxy x:1 {{\n    weight x:1 {bad}\n  }}\n}}\n");
        let cs = codes(&src);
        assert!(
            cs.contains(&DiagCode::BAD_WEIGHT),
            "`weight x:1 {bad}` 没被拦下：{cs:?}"
        );
    }
    // ★ 边界的两头必须收：1 与 65535。
    //   ⚠ 少了这一半，一条「见 weight 就报错」的实现照样能让上面全绿。
    for good in ["1", "65535", "3"] {
        let src =
            format!("http://a.com {{\n  reverse_proxy x:1 {{\n    weight x:1 {good}\n  }}\n}}\n");
        let cfg = ok(&src);
        let StepBody::ReverseProxy { upstreams, .. } = &cfg.sites[0].chain[0].body else {
            panic!("应当是 reverse_proxy")
        };
        assert_eq!(upstreams[0].weight, good.parse::<u32>().unwrap());
    }
}

#[test]
fn 零的那条诊断要说清摘节点该写什么() {
    // ★ 一条只说「不合法」的错误会把人推向「那我写 1 好了」，而他真正想要的是摘掉这台。
    let o = compile_str(
        "t.Fulcrumfile",
        "http://a.com {\n  reverse_proxy x:1 {\n    weight x:1 0\n  }\n}\n",
    );
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::BAD_WEIGHT)
        .expect("该有这条诊断");
    let 全文 = format!("{} {} {:?} {:?}", d.message, d.label, d.help, d.note);
    assert!(全文.contains("disable"), "没说摘节点该写什么：{全文}");
}

#[test]
fn 没写_weight_的配置产物一个字节都不变() {
    // ★ ★ 这是本任务的**回归护栏**（裁决 R2 的②）：权重为 1 时序列化回裸字符串，
    //   于是现有夹具、磁盘上那份结构化配置、`/load` 的载荷全都逐字不变。
    //   ⚠ 反过来说：这条一红，就是「所有现有配置的 JSON 都漂移了」。
    let cfg = ok("http://a.com {\n  reverse_proxy 10.0.0.1:8080 10.0.0.2:8080\n}\n");
    // ⚠ 用紧凑形态比对，不用 pretty：判据要钉的是**形状**（裸字符串数组），
    //   拿 pretty 去比会连缩进层数一起钉进来，而那与本条要说的事无关。
    let j = serde_json::to_string(&cfg).expect("该能序列化");
    assert!(
        j.contains(r#""upstreams":["10.0.0.1:8080","10.0.0.2:8080"]"#),
        "上游必须仍是裸字符串数组：\n{j}"
    );
    assert!(
        !j.contains("weight"),
        "没配权重时 JSON 里不许出现 weight：\n{j}"
    );
}

#[test]
fn 配了_weight_的配置序列化成对象并且能读回来() {
    let cfg = ok(
        "http://a.com {\n  reverse_proxy 10.0.0.1:8080 10.0.0.2:8080 {\n    weight 10.0.0.1:8080 3\n  }\n}\n",
    );
    let j = serde_json::to_string(&cfg).expect("该能序列化");
    assert!(
        j.contains(r#""upstreams":[{"addr":"10.0.0.1:8080","weight":3},"10.0.0.2:8080"]"#),
        "配了权重的那一项写成对象、没配的那一项仍是裸字符串：\n{j}"
    );
    let back: fulcrum_config::model::StructuredConfig =
        serde_json::from_str(&j).expect("结构化配置是公开入口，必须读得回来");
    assert_eq!(back, cfg, "往返必须无损");
}
