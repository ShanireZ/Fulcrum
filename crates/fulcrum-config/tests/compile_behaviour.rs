//! 端到端：一段 DSL 進去，結構化設定或診斷出來。
//!
//! ★ 這一層測的是**決策落沒完成實作**，不是函式對不對：
//! G49（順序表真的在排序、`route` 真的保序）、G47/G52（隱式回落帶標記）、
//! G50（行內只允許路徑）、G51（一次報全 + 診斷長什麼樣）、
//! G61（佔位符不可用位置是錯誤）、G62（不支援 import）、G63（預設值）。

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

// ── G49：順序表真的在排序 ───────────────────────────────────────────────────

#[test]
fn 书写顺序被内建顺序表重排() {
    // 倒著寫：終結類在最前，中介軟體在最後。
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
    // ★ 穩定排序：兩條 `header` 誰先跑必須由書寫順序決定，
    //   否則它會隨實作細節抖動，而那種抖動在設定層查不出來。
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
    // 逃生口：塊內不重排，所以 respond(70) 仍在 header(20) 前面。
    assert_eq!(
        steps.iter().map(|s| s.order).collect::<Vec<_>>(),
        vec![70, 20]
    );
}

#[test]
fn 容器排在终结类之前所以文档首页那个例子成立() {
    // handle @internal { … } + respond 403 備援 —— DSL 參考 §一的範例。
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

// ── G47 / G52：隱式回落 ─────────────────────────────────────────────────────

/// ⚠ **這條判據現在驗的是「歸零」**：`file_server` 與 `cache` 都改自研之後，
/// 回落層整塊刪除（G98）⇒ 那兩條指令編譯得過、且**一條回落標記都產生不出來**。
/// ★ 測試名保留原樣是有意的：它記著這條判據為什麼存在。
///
/// > ★ 一條被刪掉的判據，與一個從來沒有過判據的功能，事後看起來一模一樣。
/// > ⇒ 所以這條留著，而不是刪掉。
#[test]
fn file_server与cache编译成回落且被逐条列出() {
    let cfg =
        ok("a.com {\n  cache {\n    ttl 5m\n  }\n  file_server {\n    root /srv/www\n  }\n}\n");
    // ★ 兩條都編譯得過，而且都**不是**回落 —— 歸屬表裡沒有「回落」那一檔了。
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

/// L4 整面自研 + 回落層刪除之後，這條判據只剩「`l4` 塊本身編譯對了」那一半。
/// ⚠ 一條被刪掉的判據，與一個從來沒有過判據的功能，事後看起來一模一樣。
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
    //   「歸屬表裡不許再出現回落那一檔」。
}

#[test]
fn 自研的指令没有回落标记() {
    // ★ 回落層刪除之後**所有**指令都沒有回落標記 —— 所以這是一條全表斷言。
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

// ── G51：診斷 ───────────────────────────────────────────────────────────────

#[test]
fn 未知指令的诊断与文档样例逐字符一致() {
    // ★ DSL 參考 §九印出來的那一段就是契約。這裡把它重現出來。
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
    // 第 2 行是：`  header 中文头 "{nope}"`，診斷指著那個帶引號的取值。
    //
    // ★ 期望值是**手算出來的常數**，不是拿被測函式再算一遍：
    //   前綴 `  header 中文头 ` 的**顯示寬度** = 2 + 6 + 1 + 3×2 + 1 = **16 列**，
    //   而它的**字元數**只有 13。按字元數算 caret 會左偏 3 格——
    //   偏移量恰好等於中文字數，所以這種偏差在純 ASCII 的測試裡永遠看不見。
    let src = "a.com {\n  header 中文头 \"{nope}\"\n}\n";
    let o = compile_str("t", src);
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_PLACEHOLDER)
        .expect("应当报未知占位符");
    let rendered = d.render(&o.source);
    // 0 標題 / 1 `-->` / 2 豎線 / 3 原始碼 / 4 caret
    let caret_line = rendered.lines().nth(4).expect("应当有 caret 行");
    // 行號 `2` 寬 1 ⇒ caret 行的前綴是 `  | `（4 個字元），後面才是 pad。
    assert_eq!(
        caret_line.find('^'),
        Some(4 + 16),
        "caret 没落在那个引号下面：{caret_line:?}"
    );
}

// ── G61：佔位符 ─────────────────────────────────────────────────────────────

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

// ── G62 / G63 / 類型 ────────────────────────────────────────────────────────

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

// ── 位址 ────────────────────────────────────────────────────────────────────

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
    // ★ 沒有主機名就簽不出證書，所以 `:8080` 一律不自動升級。
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
    // ★ G15 的形狀：錯誤在啟動時暴露，不等被濫用才發現。編譯期比啟動更早。
    assert!(!codes("a.com {\n  tls {\n    on_demand\n  }\n}\n").is_empty());
    ok(
        "a.com {\n  tls {\n    on_demand\n    ask https://admin.example.com/check\n  }\n  respond 200\n}\n",
    );
}

/// ★ ★ ★ 「表裡有位置」不等於「有人接」——`tls` 的子指令逐條都要落到結構化設定裡。
///
/// 這條門補的是一個**真發生過**的缺陷：`resolvers` 在 DSL 裡認得、
/// `TLS_SUBS` 裡有它、參考文件寫著它，而 `compile.rs` 那個 `match` 用 `_ => {}`
/// 把它**靜默丟掉**了——執行環境從來沒見過它。當時補了兩道，
/// 但**這一道只寫進了註解，人沒有真去寫**，於是「有兩道門」本身成了一句假話。
///
/// ⚠ ⚠ **判據必須掛在「值落到了 `TlsConfig` 上」，不能掛在「編譯器報了內部錯誤」。**
///   把那條臂改回 `_ => {}` 之後內部錯誤**根本不會發出**，
///   於是一道「斷言沒有內部錯誤」的門在缺陷重現時照樣給綠——
///   它恰好在唯一需要它的時刻看不見。
///
/// ⚠ ⚠ ★ **每條子指令必須有它自己的最小設定，不能幾條擠在一份裡。**
///   `on_demand` 要 `ask` 陪、`dns` 要 `resolvers` 陪（G15 / G58 的編譯期伴生檢核），
///   擠在一份裡時把 `resolvers` 改壞，**先開火的是伴生檢核**，本條根本走不到自己的斷言 ——
///   ★ **「它紅了」不等於「它紅對了地方」。**
///   現在 `resolvers` 那份**故意不寫 `dns`**，靜默丟棄只有本條看得見。
///   ★ 並且**一次報全**而不是逐條 panic：先紅的那條未必是根因那條。
///
/// ★ 反證（實做）：把 `compile.rs` 那條臂改成 `"resolvers" => {}`
///   （即原缺陷的形態：靜默丟棄、無診斷），本條報兩行 ——
///   `tls dns 那份最小配置编译不过`（伴生檢核）+
///   **`tls resolvers … 没落到 TlsConfig 上`**（根因）。改回去即綠。
///
/// ★ 新加一條子指令而忘了在這裡給判據 ⇒ 走到 `other` 那一臂直接 panic。
///   `s.name` 是 `&str`，語言層面給不出窮盡性檢查，**這一臂就是人工的那份**。
#[test]
fn tls子指令全部有人接() {
    // 空表會讓這個循環一次都不跑而依然給綠——先釘住下界（照 doc_contract 的形狀）。
    assert!(!TLS_SUBS.is_empty(), "TLS_SUBS 是空的");
    let mut problems: Vec<String> = Vec::new();
    for s in TLS_SUBS {
        #[allow(clippy::type_complexity)]
        let (src, landed): (&str, fn(&fulcrum_config::model::TlsConfig) -> bool) = match s.name {
            // 丟掉 `on_demand` ⇒ 它變 false ⇒ G15 那條伴生檢核不開火 ⇒ 只有本條看得見。
            "on_demand" => (
                "a.com {\n  tls {\n    on_demand true\n    ask https://admin.example.com/check\n  }\n  respond 200\n}\n",
                |t| t.on_demand,
            ),
            "ask" => (
                "a.com {\n  tls {\n    ask https://admin.example.com/check\n  }\n  respond 200\n}\n",
                |t| t.ask.as_deref() == Some("https://admin.example.com/check"),
            ),
            // 丟掉 `dns` ⇒ `dns_provider` 是 None ⇒ G58 那條伴生檢核不開火。
            "dns" => (
                "a.com {\n  tls {\n    dns exec /bin/true\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n",
                |t| {
                    t.dns_provider.as_deref() == Some("exec")
                        // ⚠ 此後 dns_arg 是 Secret（預設脫敏）——
                        //   判據要拿 expose()，而這個名字本身就是「這裡碰了真值」的記號。
                        && t.dns_arg.as_ref().map(|a| a.expose()) == Some("/bin/true")
                },
            ),
            // ★ ★ 批 8 新加的一條。**它是被這道門逼出來的**：
            //   `zones` 一進 `TLS_SUBS`，下面那條 `other => panic!` 當場開火，
            //   於是「加了子指令卻忘了接」在結構上做不到。這就是這道門的設計。
            //   ⚠ 這份設定**必須帶上原生供應商**：`zones` 單獨寫不違法，但
            //   `dns cloudflare` 少了 `zones` 是編譯期錯誤（G59 第 3 條），
            //   而我們要量的是「`zones` 的值落到 TlsConfig 上沒有」。
            "zones" => (
                "a.com {\n  tls {\n    dns cloudflare env:CF_TOKEN\n    zones a.com b.com\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n",
                |t| t.zones == ["a.com", "b.com"],
            ),
            // ★ 這份**故意不寫 `dns`**，理由見上面那段。
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
        // ⚠ 不在這裡 panic，**把問題收集起來一次報全**。
        //   `dns` 那份最小設定繞不開 `resolvers`（G58 要求它倆同時寫），
        //   所以 `resolvers` 一旦沒人接，`dns` 那份會先在伴生檢核上編譯失敗。
        //   逐條 panic 的話只看得見第一條，而第一條恰好不是根因那條。
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

// ── ★ ★ ★ 把上面那道門的**形狀**掃到所有同類表上（批 10）───────────
//
// ⚠ ⚠ 上面那道門只管 `tls`。而 `compile.rs` 裡**有兩處 `_ => {}` 還留著**：
//   全域選項那個 match、`reverse_proxy` 子指令那個 match。
//   它們今天都是滿的（每條都有臂），所以下面這幾道門今天全綠 ——
//   **它們守的是明天**：新加一條子指令而忘了接，在 `tls` 上會被逼出來，
//   在這幾張表上此前不會。
//
// ★ 紀律：**修完一個「形狀」要當場把同形的全掃一遍**。
//
// ★ 每張表一道門，而不是合成一道：合成之後一條紅了看不出是哪張表，
//   而這幾張表的「完成實作判據」類型本來就各不相同（落到 `Global` / 落到 `StepBody`）。

/// 取出第一個站台執行鏈上的第一步。
fn first_body(cfg: &fulcrum_config::model::StructuredConfig) -> &StepBody {
    &cfg.sites[0].chain[0].body
}

#[test]
fn reverse_proxy子指令全部有人接() {
    assert!(!REVERSE_PROXY_SUBS.is_empty(), "REVERSE_PROXY_SUBS 是空的");
    let mut problems: Vec<String> = Vec::new();
    for s in REVERSE_PROXY_SUBS {
        // ⚠ 每條判據都必須與**預設值不同**，否則「值被丟掉」與「值落對了」
        //   在產物上長得一模一樣 —— 那樣的判據在缺陷重現時照樣給綠。
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
            // ★ 批 10 串接的就是它。預設 30s，所以判據寫 5s 才分得出「丟了」。
            "dns_refresh" => (
                "dns_refresh 5s",
                |b| matches!(b, StepBody::ReverseProxy { dns_refresh_ms, .. } if *dns_refresh_ms == 5_000),
            ),
            "passive_fail" => (
                "passive_fail 3",
                |b| matches!(b, StepBody::ReverseProxy { passive, .. } if passive.fail_threshold == Some(3)),
            ),
            // ★ G136：判據寫 **9s** 而不是預設的 10s —— 預設值現在是**具體數字**
            //   （不再是 `None`），所以寫 10s 的話「落到了」與「被丟掉了」完全同形。
            "passive_window" => (
                "passive_window 9s",
                |b| matches!(b, StepBody::ReverseProxy { passive, .. } if passive.window_ms == 9_000),
            ),
            // ★ G136 新增。判據寫 **45s**：既躲開自己的預設 30s，也躲開 `window` 的預設 10s
            //   —— 否則「兩個旋鈕接反了」這種缺陷在產物上看不出來。
            "passive_cooldown" => (
                "passive_cooldown 45s",
                |b| matches!(b, StepBody::ReverseProxy { passive, .. } if passive.cooldown_ms == 45_000),
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
            // ★ 批 D。判據有意寫 **v1** 而不是省略參數：省了就是 v2，
            //   而「接上了」與「沒接上但預設恰好也是 v2」在那種寫法下**分不出來**。
            //   ⚠ 這與上面 `dns_refresh` 寫 5s 而不是 30s 是同一條理由。
            "proxy_protocol" => (
                "proxy_protocol v1",
                |b| matches!(b, StepBody::ReverseProxy { proxy_protocol, .. } if proxy_protocol.as_deref() == Some("v1")),
            ),
            // ★ 批 N。判據寫 **3** 而不是 1：預設就是 1，寫 1 的話「落到了」與
            //   「被丟掉了」在產物上完全同形 —— 與上面 `dns_refresh` 寫 5s 同一條理由。
            //   ⚠ 位址必須與上面那份夾具的 `reverse_proxy 127.0.0.1:1` **逐字相同**。
            "weight" => (
                "weight 127.0.0.1:1 3",
                |b| matches!(b, StepBody::ReverseProxy { upstreams, .. } if upstreams.len() == 1 && upstreams[0].weight == 3),
            ),
            // ★ 批 N 任務 2.8（G125）。預設是 `None`，所以寫任何一個非空名字都分得出
            //   「落到了」與「被丟掉了」—— 不像 `weight` 那樣要特意躲開預設值。
            "id" => (
                "id pool_web",
                |b| matches!(b, StepBody::ReverseProxy { id, .. } if id.as_deref() == Some("pool_web")),
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
            // ★ 它在批 9 才串接，而在那之前正是「表裡有位置、沒人接」的活樣本。
            "admin" => ("admin unix//tmp/x.sock", |g| {
                g.admin.as_deref() == Some("unix//tmp/x.sock")
            }),
            "default_sni" => ("default_sni a.com", |g| {
                g.default_sni.as_deref() == Some("a.com")
            }),
            "grace_period" => ("grace_period 11s", |g| g.grace_period_ms == Some(11_000)),
            // ★ ★ 判據故意用 `false` 而不是 `true`：這個選項**預設就是 true**，
            //   拿 `true` 去測的話，一個「表裡有位置、compile 根本沒接它」的實作
            //   照樣能過 —— 那正是這條測試存在的全部理由。
            "auto_http_redirect" => ("auto_http_redirect false", |g| !g.auto_http_redirect),
            // ⚠ `fallback_nginx` / `fallback_caddy` 兩條（M2 批 G）
            //   從 `GLOBAL_OPTIONS` 裡刪除（G98）。它們的接班判據在
            //   `crates/fulcrum-runtime/tests/fallback.rs`：**寫了它們要說清它去哪了**。
            // ★ 批 D。判據寫**兩個**網段，因為 compile 裡最容易寫錯的那一版是
            //   `g.proxy_protocol_from = v`（只收第一個參數）—— 而那一版
            //   拿一個網段去測是**過得了**的。
            "proxy_protocol_from" => ("proxy_protocol_from 10.0.0.0/8 192.168.0.0/16", |g| {
                g.proxy_protocol_from == ["10.0.0.0/8", "192.168.0.0/16"]
            }),
            // ★ ★ 三格（G140）判據各取**不同的值**，⛔ 不許三條都寫 `8`：
            //   一個把三格接串了的實作（`threads_l4` 寫進 `threads_l7`）在三條同值時
            //   照樣全過 —— 而接串正是這類「三個長得一樣的欄位」最典型的寫法錯誤。
            "threads_l7" => ("threads_l7 8", |g| g.threads_l7 == Some(8)),
            "threads_l4" => ("threads_l4 3", |g| g.threads_l4 == Some(3)),
            "threads_admin" => ("threads_admin 2", |g| g.threads_admin == Some(2)),
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
    // ★ 這三張表各只有兩條，合成一道門讀起來更清楚；
    //   出問題時訊息裡帶著表名，仍然指得到具體那一條。
    let mut problems: Vec<String> = Vec::new();

    for s in LOG_SUBS {
        #[allow(clippy::type_complexity)]
        let (line, landed): (&str, fn(&fulcrum_config::model::LogConfig) -> bool) = match s.name {
            "output" => ("output stderr", |l| l.output.as_deref() == Some("stderr")),
            "level" => ("level debug", |l| l.level.as_deref() == Some("debug")),
            // ── M2 批 L 第 ③ 步 ────────────────────────────────────────
            // ⚠ 判據釘**兩個名字**而不是 `!is_empty()`：後者對一個只收下第一個參數
            //   的實作照樣給綠，而現場是「我寫了三個頭，紀錄裡只有一個」。
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
            // ⚠ 判據用一個**具體的數**，不是 `is_some()`：`is_some()` 只證明
            //   「有人給它賦了值」，而一個把 capacity 賦成 max_size 的實作照樣過。
            "capacity" => (
                "capacity 64MB",
                |b| matches!(b, StepBody::Cache { capacity_bytes, .. } if *capacity_bytes == Some(64_000_000)),
            ),
            // ── M2 批 H ────────────────────────────────────────────────
            // ⚠ 同樣釘**具體的值**而不是 `is_some()`：一個把目錄讀成別的字串
            //   （比如順手 trim 掉了什麼）的實作，`is_some()` 照樣給綠，
            //   而現場是「快取落到了一個我沒寫過的目錄裡」。
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
            // ⚠ 三條都用**非預設值**（預設是 true / 空），否則一個「讀都沒讀、
            //   直接用預設值」的實作會照樣讓這條判據全綠。
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
            // ⚠ 同樣釘**具體的值**：一個把順序反過來（或者去重去錯）的實作，
            //   `is_empty()` 那種判據照樣給綠，而挑旁檔案走的是這個列表。
            "precompressed" => (
                "precompressed br gzip",
                |b| matches!(b, StepBody::FileServer { precompressed, .. } if precompressed == &["br", "gzip"]),
            ),
            other => panic!("`file_server {other}` 是新加的子指令，这条测试没有它的判据。"),
        };
        // ⚠ M2 批 F 起 `root` 必填 ⇒ 除了 `root` 自己那一輪，塊裡都要先墊一條。
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

// ── G59：原生 DNS 供應商的兩條編譯期硬約束 ──────────────────────────────────
//
// ⚠ 理由不是潔癖：拿到某域的 DNS 寫權限 = **能為該域簽發任意證書**，
//   還能改 MX 劫持電子郵件。它比 On-Demand 被刷爆嚴重得多。

#[test]
fn 凭据可以内联_但打错的来源前缀要红() {
    // ★ ★ ★ **這條測試換過一次契約（批 22），換法本身留在這裡。**
    //
    //   舊契約（G59 第 1 條原文）：憑證**寫不進** DSL，只認 `env:` / `file:` 兩種來源，
    //   任何沒前綴的值都是編譯期錯誤。判據是**白名單**，理由是：
    //   「看起來像 token 就回報錯誤」的黑名單要去猜什麼樣子算 token，
    //   而猜錯的那一次恰恰就是真 token 被放行的那一次。
    //
    //   新契約（owner 拍板，Caddy 形狀）：**寫得下**，一份設定檔案就能跑完。
    //   ⚠ 白名單那條理由沒有失效，它只是**換了落點**——從「攔字面值」變成
    //   「攔打錯的前綴」：既然不寫前綴就是值本身，那麼 `fil:/path` 會被
    //   當成憑證發給對端，現場是「憑證不對」而真正的原因是打錯了三個字母。
    //
    //   ⇒ 代價挪到了別處，而不是消失：設定檔案從此是秘密（裝載期權限門）、
    //   `compile` 預設脫敏、`Debug`/`Display` 預設脫敏。見 `secret.rs` 與 `secret_guard.rs`。

    // ① 內聯字面值：現在合法
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

    // ② ★ 而它**預設不會被印出來**——這是允許內聯之後唯一撐得住的東西
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

    // ③ 兩種來源寫法照舊合法，而且**不算敏感**（它們是指針，不是秘密）
    for src in [
        "a.com {\n  tls {\n    dns cloudflare env:CF_API_TOKEN\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
        "a.com {\n  tls {\n    dns dnspod file:/run/secrets/dp\n    zones a.com\n    resolvers 1.1.1.1:53\n  }\n  respond 200\n}\n",
    ] {
        let c = ok(src);
        let a = c.sites[0].tls.dns_arg.as_ref().unwrap();
        assert!(!a.is_sensitive(), "`{}` 被当成秘密了", a.expose());
    }

    // ④ ★ ★ 打錯的來源前綴必須紅 —— 這是白名單那條理由的新落點
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

    // ⑤ 自證：**真的帶冒號的憑證**有出路（`literal:`），否則第 ④ 條會誤傷
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
    // ★ ★ G59 第 3 條。對 DNSPod 這是**唯一**的範圍約束——它的 token 是帳號級的，
    //   沒有任何端點能問出「它覆蓋哪些 zone」。
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
    // ⚠ 反向判據：exec hook 的第二個參數是**處理程序路徑**，不是憑證來源；
    //   它自己去環境變數/檔案裡拿憑證。一個把這兩條約束也套到 exec 上的實作，
    //   會讓已經在用的 DNS-01 設定**全部編譯不過**——而上面那幾條測試對此完全無感。
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

// ── 產物 ────────────────────────────────────────────────────────────────────

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

// ── 自動 HTTP 重定向（G12 的後半句，批 21）──────────────────────
//
// ⚠ ⚠ 這一組測試存在的直接原因：**文件承諾了三個月，程式碼裡一行都沒有**。
//   DSL 參考 §二一直印著「`example.com` = HTTPS，並自動把 HTTP 重定向過來」，
//   而 08-22 上線當天實測 80 連接埠上根本沒有監聽。
//   ★ 所以這裡釘的不只是「功能對不對」，還有**它是可見的**：合成物必須出現在
//   結構化設定裡（`compile` / `plan` 都讀得到），而不是資料面裡一條看不見的特判。

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
    // ★ 合成站台自己**不要證書**：它就是那條跳轉。少了這一條，
    //   TLS 那一層會把它當成「要自動簽發」，裝載紀錄裡多一條假的 ⏳。
    assert!(
        r.addresses.iter().all(|a| !a.auto_https),
        "合成的跳转站点不该要证书"
    );
    assert_eq!(r.chain.len(), 1);
    match &r.chain[0].body {
        StepBody::Redir { to, code } => {
            assert_eq!(to, "https://{host}{uri}");
            // ⚠ 308 而不是 301/302：**保留方法與 body**，
            //   否則一個 POST 會變成 GET，而現場表現是「表單提交丟了」。
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
    // ⚠ ⚠ 這一條釘的是 `Default` 的實作方式：`derive(Default)` 給 bool 的是 **false**，
    //   而這個欄位的預設必須是 **true**。一旦 derive，一份**沒寫全域塊**的設定
    //   （最常見的那種）就會悄悄關掉 HTTP 跳轉，而文件印著它是開的。
    let cfg = ok("example.com {\n  respond 200\n}\n");
    assert!(cfg.global.auto_http_redirect, "默认必须是开");
    assert_eq!(cfg.sites.len(), 2);
}

#[test]
fn 用户自己写了http站点就不碰那个主机名() {
    // ★ 使用者的寫法優先：合成只補**沒人管**的那些主機名。
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
    // ⚠ 站台索引是「精確 → 通配 → 連接埠備援」，所以合成的精確站台會蓋過 `:80` 備援 ——
    //   這是一次**行為改變**，而沉默地改掉別人設定的行為正是本儲存庫反覆點名的那種。
    let cs = codes(
        "example.com {\n  respond 200\n}\n\
         :80 {\n  respond 200 \"catch-all\"\n}\n",
    );
    assert!(
        cs.contains(&DiagCode::AUTO_REDIRECT_SHADOWS),
        "盖过兜底站点却没说：{cs:?}"
    );
}

// ── M2 批 H：`cache { disk … }` 的兩道編譯期檢查 ────────────────────────────

#[test]
fn cache_的_disk_必须是绝对路径() {
    // ⚠ G91 那條理由一字不改地適用：相對路徑按處理程序 cwd 解析，而 systemd 下
    //   cwd 是 `/`、開發機上是專案目錄 ⇒ 同一份設定會把快取落到兩個地方，
    //   ★ 而兩處都「看起來正常」。
    let cs = codes("a.com {\n  cache {\n    disk var/cache\n  }\n  reverse_proxy 127.0.0.1:1\n}\n");
    assert!(
        cs.contains(&DiagCode::PATH_NOT_ABSOLUTE),
        "相对目录没被拦下：{cs:?}"
    );
    // 反向那一半：絕對路徑必須過。⚠ 少了它，一條「什麼都攔」的實作照樣全綠。
    ok("a.com {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  reverse_proxy 127.0.0.1:1\n}\n");
}

#[test]
fn 两个_cache_块写不同的_disk_是错误() {
    // ★ ★ 快取後端是**處理程序級**的：兩個不同的值裡必有一個是
    //   「你以為生效、其實沒有」的。
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
    // ⚠ 這一半最容易漏：兩條都「合法」，只是它們要的後端不是同一個。
    //   ★ 不攔的話，沒寫 disk 的那個站台會拿到一個磁碟快取（或者反過來），
    //   而設定檔案裡一個字都看不出來。
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
    // ★ 反向那一半：一致就該放行。⚠ 少了它，一條「只要有兩個 cache 就回報錯誤」
    //   的實作會讓上面兩條全綠，而它把一份完全正確的設定也攔了。
    ok(
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/one\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    // 都不寫 disk（記憶體後端）同樣一致。
    ok(
        "a.com {\n  cache {\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    ttl 2m\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
}

// ── M2 批 L 第 ③ 步：白名單頭裡的四個名字是編譯期錯誤 ────────────────────────

#[test]
fn 白名单里写敏感头是编译期错误_两条路各拦一次() {
    // ⚠ ⚠ 四個名字 × 兩條子指令 = 八種寫法，**逐個走一遍**。
    //   ★ 只測 `headers` 那一半的話，`resp_headers` 上漏一個就是一個洩漏面
    //   （`Set-Cookie` 恰恰只會出現在回應上）。
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
    // ★ ★ 反證。⚠ 少了它，一條「見 `headers` 就回報錯誤」的實作會讓上面那條全綠，
    //   而它把整個功能攔死了 —— **「擋住壞的」與「別連好的一起擋掉」要各自一條判據**。
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
    // ★ 一條指錯地方的診斷比沒有診斷更費時間 —— 人會先去看它指的那一行。
    //   ⚠ 這裡量的是**列**：`headers` 後面那個名字，而不是 `log` 或 `headers` 本身。
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

// ── M2 批 M：`metrics` 是站台塊裡的終結指令（G116）─────────────────────────────

#[test]
fn metrics进链且夹在respond与reverse_proxy之间() {
    // ★ 三條**倒著寫**，讓排序有活幹：只斷言「編譯得過」的話，序號寫成 65 或 85
    //   照樣全綠，而那時 `metrics` 已經跑在錯誤的位置上了（G49 點名的那個形狀）。
    // ⚠ 三條都帶匹配器 —— 否則第一條終結類會把後面兩條擋死，測的就不是排序了。
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
    // ⚠ `abc` 不以 `/` `@` `*` 開頭 ⇒ 它到不了行內匹配器那條路，是真的多餘參數。
    let cs = codes("http://a.com {\n  metrics abc\n}\n");
    assert!(
        cs.contains(&DiagCode::BAD_ARITY),
        "`metrics abc` 该报 arity 错误，实际：{cs:?}"
    );
}

#[test]
fn 没有匹配器的metrics要出警告() {
    let o = compile_str("t.Fulcrumfile", "http://a.com {\n  metrics\n}\n");
    // ★ 是 **warning** 不是 error：開在內網可信段上的指標端點是正當設定。
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::METRICS_UNGUARDED)
        .expect("裸 `metrics` 该出 G116 那条警告");
    // 文案要說出後果，不是只說「建議加個匹配器」。
    assert!(d.label.contains("指标"), "{}", d.label);
    // ★ ★ note 裡必須把「什麼算、什麼不算」逐個說出來 —— 那是這條診斷現在的
    //   全部價值所在：一句籠統的「加個匹配器吧」會把人直接引向
    //   `handle /metrics { metrics }`，而那正是它要抓的那個裸奔設定本身。
    let note = d.note.as_deref().unwrap_or("");
    for word in ["remote_ip", "header", "path", "host", "method", "query"] {
        assert!(note.contains(word), "note 里没说 `{word}`：{note}");
    }
}

/// 這一份裡有沒有 `FUL-DSL-0037`。⚠ 順帶釘住「它從來不是 error」——
/// 一條把設定拒掉的門會把「指標開在內網可信段上」這種正當設定一起擋掉。
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
    // ★ ★ **反證，兩個方向缺一不可。** ⚠ 少了這一條，把診斷寫成「見 `metrics` 就報」
    //   也能讓正方向那幾條全綠 —— 而一條恆報的警告等於沒有警告。
    for (what, src) in [
        // 外層 `handle` 帶 `remote_ip`（文件裡印的就是這一種）
        (
            "handle @remote_ip",
            "http://a.com {\n  @i remote_ip 10.0.0.0/8\n  handle @i {\n    metrics\n  }\n  \
             respond 403\n}\n",
        ),
        // 這一步自己帶
        (
            "metrics @remote_ip",
            "http://a.com {\n  @i remote_ip 10.0.0.0/8\n  metrics @i\n}\n",
        ),
        // 外層 `route`（保序逃生口裡也罩得住）
        (
            "route @remote_ip",
            "http://a.com {\n  @i remote_ip 10.0.0.0/8\n  route @i {\n    metrics\n  }\n}\n",
        ),
        // ★ `header`：一個共享金鑰可以放在這裡 ⇒ 它把兩個發同樣請求的用戶端分得開
        (
            "handle @header",
            "http://a.com {\n  @tok header X-Token abc\n  handle @tok {\n    metrics\n  }\n}\n",
        ),
        // ★ `not { remote_ip … }`：`condition()` 把 `not` 拆開、給裡面那條打 `negate`，
        //   ⇒ 存下來的 `kind` 就是 `remote_ip`，它照樣算保護（不需要為 `not` 寫特例）。
        (
            "not { remote_ip … }",
            "http://a.com {\n  @notlocal {\n    not {\n      remote_ip 127.0.0.1/32\n    }\n  }\n  \
             handle @notlocal {\n    metrics\n  }\n}\n",
        ),
        // ★ AND 裡只要有一條限制得了來源就夠了（多條件是 AND，見 `Matcher`）
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
    // ⚠ ⚠ ⚠ **本輪的核心。** 判據不是「有沒有匹配器」，而是
    //   「**它能不能把兩個發同樣請求的用戶端分開**」。
    //   ★ 路徑 / host / method / query 全都是**請求裡的東西，任何用戶端都能照著發一份**
    //   —— 把它們算成保護，這條診斷就在它唯一要抓的東西上沉默：
    //   從 nginx / Caddy 遷過來的人第一反應正是寫一條路徑。
    for (what, src) in [
        // ① ★ 行內路徑匹配器 —— **最可能出現的那個裸奔設定**
        ("行内路径", "http://a.com {\n  metrics /metrics\n}\n"),
        // ② 命名的路徑匹配器，套在 handle 外面（看起來最像「圈住了」的那一種）
        (
            "handle @path",
            "http://a.com {\n  @p path /metrics\n  handle @p {\n    metrics\n  }\n}\n",
        ),
        // ③ host：Host 頭是請求裡的東西，誰都能寫
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
        // ⑥ 一個匹配器都沒有
        ("裸 metrics", "http://a.com {\n  metrics\n}\n"),
        // ⑦ `handle` 的備援分支：對所有人都開著，與裸寫一模一樣
        (
            "兜底 handle 分支",
            "http://a.com {\n  handle {\n    metrics\n  }\n}\n",
        ),
        // ⑧ `route` 沒帶匹配器
        (
            "裸 route",
            "http://a.com {\n  route {\n    metrics\n  }\n}\n",
        ),
        // ⑨ 顯式 `*`
        ("显式 `*`", "http://a.com {\n  metrics *\n}\n"),
    ] {
        assert!(有裸奔警告(src), "{what} 限制不了来源，该出警告：\n{src}");
    }
}

#[test]
fn 两条判据问的是两件事_同一份配置上给出不同答案() {
    // ★ ★ ★ **這是「沒把兩條判據合併」的活證據。**
    //   `FUL-DSL-0037` 問「這一步對**誰**開著」（`restricts_source`）；
    //   `FUL-DSL-0028` 問「這一步會不會把它後面的都吃掉」（`is_unconditional`）。
    //   ⚠ 合用一個謂詞的話它們將來會一起漂走，而漂走那天沒有任何東西會說。
    //
    // 下面這一份裡，`metrics /metrics` 讓兩條判據**必須**給出相反的答案：
    //   · 對 0037：路徑匹配器限制不了來源 ⇒ **報**；
    //   · 對 0028：它帶著匹配器、不會無條件終結 ⇒ 後面那條 `reverse_proxy` **不報**。
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

    // ⇒ 反向那一半：把匹配器拿掉，兩條判據就都該報了（0028 指的是後面那條轉發）。
    let bare = "http://a.com {\n  metrics\n  reverse_proxy 127.0.0.1:3000\n}\n";
    let cs = codes(bare);
    assert!(cs.contains(&DiagCode::METRICS_UNGUARDED), "{cs:?}");
    assert!(cs.contains(&DiagCode::UNREACHABLE_STEP), "{cs:?}");
}

// ── M2 批 N 任務 1：`weight` 的三條診斷（裁決 R1 / R3）──────────────────────

#[test]
fn weight_的地址对不上时报错并且把上游清单原样列出来() {
    // ★ ★ 位址比對是**逐字相同**，不做歸一化：這份設定裡寫的是 `backend:80`，
    //   而 `weight` 指的是 `backend` —— 執行環境那邊它們會歸一成同一個，
    //   但歸一化住在 `fulcrum-runtime`，在設定層再寫一份「差不多的」就是分家。
    let src =
        "http://a.com {\n  reverse_proxy backend:80 other:80 {\n    weight backend 3\n  }\n}\n";
    let o = compile_str("test.Fulcrumfile", src);
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_WEIGHT_UPSTREAM)
        .unwrap_or_else(|| panic!("该报 FUL-DSL-0038，实际：\n{}", o.render_diagnostics()));
    // ⚠ ⚠ 判據不是「報了一條」，是**那條錯誤裡真的把清單列出來了**：
    //   只說「找不到」等於讓人去猜自己上一行寫的到底是什麼，
    //   而這條錯誤最常見的成因恰恰是兩處寫法差了一個連接埠。
    let 全文 = format!("{} {} {:?} {:?}", d.message, d.label, d.help, d.note);
    for up in ["backend:80", "other:80"] {
        assert!(全文.contains(up), "错误里没有把上游 `{up}` 列出来：{全文}");
    }
    assert!(o.diagnostics.has_errors(), "这是装载期错误，不是 warning");
    // ★ 反向那一半：**逐字相同**的那條不許被攔（否則功能整個是死的）。
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
    // ⛔ 「後寫的贏」是靜默的：刪掉或挪動其中一行會改掉權重，而設定裡看不出異常。
    let o = compile_str("t.Fulcrumfile", src);
    assert!(o.diagnostics.has_errors(), "重复必须是 error");
    // ★ 反向那一半：兩條 `weight` 指著**不同**的上游是正常寫法，不許被攔。
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
    // ★ ★ `0` 不合法是**有意的**：「不參與排程」只有一種表達方式 —— 覆蓋層的 `disable`。
    //   兩條路做同一件事就是分家，而分家那天沒有任何東西會說。
    for bad in ["0", "-1", "65536", "3s", "abc", "1.5"] {
        let src =
            format!("http://a.com {{\n  reverse_proxy x:1 {{\n    weight x:1 {bad}\n  }}\n}}\n");
        let cs = codes(&src);
        assert!(
            cs.contains(&DiagCode::BAD_WEIGHT),
            "`weight x:1 {bad}` 没被拦下：{cs:?}"
        );
    }
    // ★ 邊界的兩頭必須收：1 與 65535。
    //   ⚠ 少了這一半，一條「見 weight 就回報錯誤」的實作照樣能讓上面全綠。
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
    // ★ 一條只說「不合法」的錯誤會把人推向「那我寫 1 好了」，而他真正想要的是摘掉這臺。
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
    // ★ ★ 這是本任務的**迴歸護欄**（裁決 R2 的②）：權重為 1 時序列化回裸字串，
    //   於是現有夾具、磁碟上那份結構化設定、`/load` 的載荷全都逐字不變。
    //   ⚠ 反過來說：這條一紅，就是「所有現有設定的 JSON 都漂移了」。
    let cfg = ok("http://a.com {\n  reverse_proxy 10.0.0.1:8080 10.0.0.2:8080\n}\n");
    // ⚠ 用緊湊形態比對，不用 pretty：判據要釘的是**形狀**（裸字串陣列），
    //   拿 pretty 去比會連縮進層數一起釘進來，而那與本條要說的事無關。
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

// ── `reverse_proxy { id … }`（M2 批 N 任務 2.8，裁決 R6 ⇒ G125）────────────

#[test]
fn 同一个块里写两行_id_是错误而不是后写的赢() {
    let src = "http://a.com {\n  reverse_proxy x:1 {\n    id a\n    id b\n  }\n}\n";
    let cs = codes(src);
    assert!(
        cs.contains(&DiagCode::DUPLICATE_PROXY_ID),
        "重复的 id 没被拦下：{cs:?}"
    );
    let o = compile_str("t.Fulcrumfile", src);
    assert!(o.diagnostics.has_errors(), "重复必须是 error");
    // ⛔ 「後寫的贏」是靜默的：挪動或刪掉其中一行會改掉這條 `reverse_proxy` 的尋址，
    //   而設定裡看不出異常。⇒ 判據要釘住「沒有靜靜取了第二個」。
    assert!(
        o.config.is_none() || {
            let cfg = o.config.unwrap();
            let StepBody::ReverseProxy { id, .. } = &cfg.sites[0].chain[0].body else {
                panic!("应当是 reverse_proxy")
            };
            id.as_deref() != Some("b")
        },
        "第二行 `id` 不许赢"
    );
    // ★ 反向那一半：寫一行是正常寫法，不許被攔。
    let cfg = ok("http://a.com {\n  reverse_proxy x:1 {\n    id a\n  }\n}\n");
    let StepBody::ReverseProxy { id, .. } = &cfg.sites[0].chain[0].body else {
        panic!("应当是 reverse_proxy")
    };
    assert_eq!(id.as_deref(), Some("a"));
}

#[test]
fn 空的_id_是错误因为它与没写是同一个键() {
    // ★ ★ 沒寫 `id` 的那條在覆蓋層鍵裡 id 那一格**就是空串** ⇒ `id ""` 什麼都沒分開。
    //   ⚠ 最壞的現場恰恰是最像修好了的那一種：兩條撞了鍵，有人補一行 `id ""`。
    let o = compile_str(
        "t.Fulcrumfile",
        "http://a.com {\n  reverse_proxy x:1 {\n    id \"\"\n  }\n}\n",
    );
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::EMPTY_PROXY_ID)
        .unwrap_or_else(|| panic!("该报 FUL-DSL-0042，实际：\n{}", o.render_diagnostics()));
    let 全文 = format!("{} {} {:?} {:?}", d.message, d.label, d.help, d.note);
    assert!(
        全文.contains("空串"),
        "要说清「空串就是没写的那一格」：{全文}"
    );
    // ★ 反向那一半：非空的名字不許被攔（否則功能整個是死的）。
    ok("http://a.com {\n  reverse_proxy x:1 {\n    id pool-web\n  }\n}\n");
}

#[test]
fn 没写_id_的配置产物一个字节都不变() {
    // ★ ★ 與 `没写_weight_的配置产物一个字节都不变` 是同一條迴歸護欄（任務 2.8 §2 ②）：
    //   `id` 那一格在 `None` 時**整個鍵都不出現**，不是 `"id":null`。
    let cfg = ok("http://a.com {\n  reverse_proxy 10.0.0.1:8080\n}\n");
    let j = serde_json::to_string(&cfg).expect("该能序列化");
    assert!(
        !j.contains(r#""id""#),
        "没写 id 时 JSON 里不许出现 id 这个键：\n{j}"
    );
    // ★ 寫了就要帶得上、讀得回（反向判據：少了它，一個「永遠不序列化 id」的實作
    //   照樣能讓上面那一半綠）。
    let cfg = ok("http://a.com {\n  reverse_proxy 10.0.0.1:8080 {\n    id pool_web\n  }\n}\n");
    let j = serde_json::to_string(&cfg).expect("该能序列化");
    assert!(j.contains(r#""id":"pool_web""#), "写了的 id 没带上：\n{j}");
    let back: fulcrum_config::model::StructuredConfig =
        serde_json::from_str(&j).expect("结构化配置是公开入口，必须读得回来");
    assert_eq!(back, cfg, "往返必须无损");
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

// ── `id` 的**取值域**：`[A-Za-z0-9_.-]`、長度 1–64（owner 拍板，R6 那三條小項之三）─
//
// ★ ★ ★ **真正的理由只有一條半**，⛔ 別把它說成「基數風險」——
//   那是假話，而假理由會讓下一個人把限制放寬回去（G126 明寫
//   `fulcrum_overrides_active` **無標籤**，`id` 一個字都不進指標）：
//   ① 硬的那條：本儲存庫用 `<other>` / `<none>` / `<unknown>` / `<undeclared>` 當
//      「取不到 / 備援」的記號，靠的是**尖括號在真值裡不可能出現** ——
//      而在這條收緊之前，`id <none>` 是**合法**的；
//   ② 軟的那半條：可讀性 —— `id` 裡的換行會讓 `/stats` 那一行在人眼裡斷成兩行。
// ⚠ 這是一次**收緊**：今天能裝上的某些設定以後裝不上。M4 還沒到（沒有版本號、
//   沒有相容性承諾，PLAN.md §7）⇒ **要收就現在收最便宜**（owner 拍板）。

#[test]
fn id_里有空格或尖括号或超长都装不上() {
    // ⚠ 三種形態各一條，⛔ 不合並：它們各自對應一個**不同的**失效現場，
    //   而一條「只判其中一種」的實作在合併寫法下照樣綠。
    for (什么, 值) in [
        ("空格", "\"a b\"".to_string()),
        ("尖括号（与兜底记号撞车）", "\"<none>\"".to_string()),
        // ⚠ ⚠ **真的換行在 DSL 裡表達不出來**，⛔ 別在這裡問它：引號串不能跨行，
        //   詞法層先回 `UNCLOSED_QUOTE`（`FUL-DSL-0003`）—— 那條診斷是對的，
        //   而它壓根到不了 `proxy_id`。
        //   ⇒ 人想寫換行時**實際寫出來的**是 `\` + `n` 兩個字元（詞法層只認 `\"`
        //   與 `\\` 兩種轉義），而反斜槓不在取值域裡 ⇒ 由這一條攔住。
        //   ★ 帶**真**換行的那一格在 `fulcrum-runtime/tests/proxy_id.rs` 上：
        //   只有手搓的 JSON（`POST /load`）遞得進來。
        ("想写换行、实际写出来的反斜杠", "\"a\\nb\"".to_string()),
        ("中文", "\"网页池\"".to_string()),
        ("65 个字符（上限 64）", format!("\"{}\"", "a".repeat(65))),
    ] {
        let src = format!("http://a.com {{\n  reverse_proxy x:1 {{\n    id {值}\n  }}\n}}\n");
        let cs = codes(&src);
        assert!(
            cs.contains(&DiagCode::BAD_PROXY_ID),
            "`id` 里的{什么}没被拦下：{cs:?}"
        );
    }
}

#[test]
fn id_的诊断说得出合法的样子长什么样() {
    // ★ 一條只說「不合法」的診斷，會讓人一次次去試 —— 判據要求它**把值域說出來**。
    let o = compile_str(
        "t.Fulcrumfile",
        "http://a.com {\n  reverse_proxy x:1 {\n    id \"<none>\"\n  }\n}\n",
    );
    let d = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::BAD_PROXY_ID)
        .unwrap_or_else(|| panic!("该报 FUL-DSL-0043，实际：\n{}", o.render_diagnostics()));
    let 全文 = format!("{} {} {:?} {:?}", d.message, d.label, d.help, d.note);
    assert!(全文.contains("64"), "没说长度上限：{全文}");
    assert!(
        全文.contains("A-Za-z0-9_.-") || 全文.contains("字母"),
        "没说清允许哪些字符：{全文}"
    );
}

#[test]
fn 正常的_id_照旧装得上() {
    // ★ ★ 反向那一半。⚠ 少了它，一個把**所有** `id` 都拒掉的實作在上面兩條上全綠 ——
    //   而它會讓 `id` 這個功能整個變成死的。
    for 名 in ["pool_web", "web-1", "a.b_c", "A", &"z".repeat(64)] {
        let src = format!("http://a.com {{\n  reverse_proxy x:1 {{\n    id {名}\n  }}\n}}\n");
        let cfg = ok(&src);
        let StepBody::ReverseProxy { id, .. } = &cfg.sites[0].chain[0].body else {
            panic!("应当是 reverse_proxy")
        };
        assert_eq!(id.as_deref(), Some(名), "`id {名}` 该照旧装得上");
    }
}

#[test]
fn 空的_id_仍然报它自己那条而不是被取值域顶掉() {
    // ⚠ ⚠ 空串有它**自己的**診斷（FUL-DSL-0042）與自己的那段解釋
    //   （「`id \"\"` 與根本不寫是同一個鍵」）——那句話是這條診斷的全部價值。
    //   ⇒ 新的取值域判據不許把它頂成一句泛泛的「不合法」。
    let cs = codes("http://a.com {\n  reverse_proxy x:1 {\n    id \"\"\n  }\n}\n");
    assert!(
        cs.contains(&DiagCode::EMPTY_PROXY_ID),
        "空串该报它自己那条，而不是被取值域顶掉：{cs:?}"
    );
    assert!(
        !cs.contains(&DiagCode::BAD_PROXY_ID),
        "空串不该同时报两条：{cs:?}"
    );
}

// ── G136：被動熔斷的設定面 ─────────────────────────────────────────────────

/// 一段只有 `reverse_proxy` 的最小站台，塊裡塞給定的幾行。
fn rp(lines: &str) -> String {
    format!("http://a.com {{\n  reverse_proxy 127.0.0.1:1 {{\n{lines}\n  }}\n}}\n")
}

/// 這條 `reverse_proxy` 編出來的 `passive`。
fn passive_of(src: &str) -> fulcrum_config::model::Passive {
    match &ok(src).sites[0].chain[0].body {
        StepBody::ReverseProxy { passive, .. } => passive.clone(),
        other => panic!("第一步不是 reverse_proxy：{other:?}"),
    }
}

/// ⚠ ⚠ **`passive_fail` 的值不合法 ⇒ 裝載期錯誤**（G136）。
///
/// ★ ★ **承重的是「不是數字」那一半，不是 `0`。** 在 G136 之前那一行是
/// `first.parse().ok()` —— `passive_fail 五` 會**靜默**變成 `None`，而 `None` 的語義
/// 恰好是**整個特性關掉**：一個手滑的值讓熔斷悄悄消失，設定照過、裝載照過，
/// 而沒有任何一行字提到它。⇒ 這條判據要是沒了，那個缺陷不會有任何症狀。
///
/// ★ `0` 那一半與 [`DiagCode::BAD_WEIGHT`] 同源：「不做被動熔斷」只有一種表達方式
/// （不寫 `passive_fail`），兩條路做同一件事遲早分家。
#[test]
fn passive_fail_的值不合法是装载期错误() {
    for bad in ["0", "五", "-1", "3s", "70000"] {
        let cs = codes(&rp(&format!("    passive_fail {bad}")));
        assert!(
            cs.contains(&DiagCode::BAD_PASSIVE_FAIL),
            "`passive_fail {bad}` 该报 BAD_PASSIVE_FAIL，实际：{cs:?}"
        );
    }
}

// ── G140：執行緒數三格 ────────────────────────────────────────────────────────

/// 一份只帶一條全域執行緒指令的最小設定。
fn with_global(line: &str) -> String {
    format!("{{\n  {line}\n}}\nhttp://a.com {{\n  respond 200\n}}\n")
}

/// 三格的取值域各自都判得動（**G140**）。
///
/// ★ ★ 三個鍵各跑一遍，⛔ 不許只跑 `threads_l7`：三格在 compile.rs 裡是**三條獨立的臂**，
/// 只測一條時，一條忘了接判據的實作照樣全綠（那條會靜默變成「沒寫」）。
#[test]
fn threads_三格的值不合法都是装载期错误() {
    for key in ["threads_l7", "threads_l4", "threads_admin"] {
        // ⚠ `0` 單列一條理由：pingora 拿它建 tokio runtime 會**啟動時 panic**，
        //   而那個回報錯誤一個字都不提設定。
        // ⚠ `八` 那條是這族診斷真正的價值所在（同 G136）：寫成 `.parse().ok()` 時
        //   它會**靜默**變成 `None`，而 `None` 的語義恰好是「回落到角色預設」。
        for bad in ["0", "八", "-1", "8s", "1025"] {
            let cs = codes(&with_global(&format!("{key} {bad}")));
            assert!(
                cs.contains(&DiagCode::BAD_THREADS),
                "`{key} {bad}` 该报 BAD_THREADS，实际：{cs:?}"
            );
        }

        // ⚠ **一個值都不寫是參數個數錯，⛔ 不是 BAD_THREADS。**
        //   這一條不是湊數：`thread_count` 在這條路上**根本不會被呼叫**
        //   （`check_sub_args` 先攔住了）⇒ 把它寫進上面那個循環，
        //   就是在要求一條永遠走不到的程式碼路徑去報一條它報不出來的診斷。
        //   ★ 而它仍然必須是**裝載期錯誤** —— 一行 `threads_l7` 靜默地什麼都不做，
        //   與「寫了個數」在設定裡長得一模一樣。
        let cs = codes(&with_global(key));
        assert!(
            cs.contains(&DiagCode::BAD_ARITY),
            "`{key}` 不带值该报 BAD_ARITY，实际：{cs:?}"
        );
    }
}

/// 反方向：合法值照過，且**落到它自己那一格**（**G140**）。
///
/// ★ ★ ★ 三格取**三個不同的值**是判據本身：三格接串（`threads_l4` 寫進 `threads_l7`）
/// 是這類「三個同型欄位」最典型的寫法錯誤，而三條同值時它照樣全綠。
#[test]
fn threads_三格的合法值各落各的格() {
    let cfg = ok(
        "{\n  threads_l7 16\n  threads_l4 3\n  threads_admin 2\n}\nhttp://a.com {\n  respond 200\n}\n",
    );
    assert_eq!(cfg.global.threads_l7, Some(16), "threads_l7 没落对");
    assert_eq!(cfg.global.threads_l4, Some(3), "threads_l4 没落对");
    assert_eq!(cfg.global.threads_admin, Some(2), "threads_admin 没落对");
}

/// 邊界值照過（**G140**）：`1` 與 `1024` 都在值域內。
#[test]
fn threads_的边界值照过() {
    for good in ["1", "1024"] {
        let cfg = ok(&with_global(&format!("threads_l7 {good}")));
        assert_eq!(
            cfg.global.threads_l7,
            Some(good.parse().unwrap()),
            "`threads_l7 {good}` 该照过并落到产物里"
        );
    }
}

/// ⚠ **不寫就是 `None`，而 `None` 的語義是「用角色預設」**（**G140**）。
///
/// ★ ★ 這一條守的是「預設值只住一處」：`None` 在這裡，具體數字在
/// `fulcrum_server::process::ThreadRole` 那一處。⛔ 若哪天有人在 `Global::default()`
/// 裡把它改成 `Some(1)`，預設就住了兩處，而兩處遲早分家 ——
/// 本儲存庫 2026-09-05 當天剛栽過一次同形狀的（G135 的分解式抄了三處）。
#[test]
fn 不写线程数就是_none_而不是某个具体数字() {
    let cfg = ok("http://a.com {\n  respond 200\n}\n");
    assert_eq!(cfg.global.threads_l7, None);
    assert_eq!(cfg.global.threads_l4, None);
    assert_eq!(cfg.global.threads_admin, None);
}

/// 反方向：合法值不許被那條取值域判據順手攔掉。
#[test]
fn passive_fail_的合法值照过() {
    for good in ["1", "3", "65535"] {
        let p = passive_of(&rp(&format!("    passive_fail {good}")));
        assert_eq!(
            p.fail_threshold,
            Some(good.parse().unwrap()),
            "`passive_fail {good}` 该照过并落到产物里"
        );
    }
}

/// ⚠ **不寫 `passive_fail` ⇒ 整條 `reverse_proxy` 不做被動熔斷**（G136）。
///
/// ★ ★ 這是「**既有設定行為一個字不變**」的判據 —— G136 選「預設關閉」而不是
/// nginx 那種「預設開」，全部理由就在這一條上。它要是變成 `Some(_)`，
/// 每一份現存設定都會在某次升級後開始悄悄摘上游，而症狀是「偶爾 502」。
#[test]
fn 不写_passive_fail_就是完全关闭() {
    let p = passive_of(&rp("    lb_policy least_conn"));
    assert_eq!(
        p.fail_threshold, None,
        "没写 passive_fail 却不是 None —— 被动熔断被默认打开了"
    );
}

/// 預設值在**設定層**就落實成具體數字（G136），⛔ 不留給執行環境 `unwrap_or`。
///
/// ⚠ 判據寫死 `10_000` / `30_000` 而不是「不是 0」：預設值住兩處的失效形態是
/// 兩處分家，而分家那天兩邊各自都自洽 —— 那正是要靠這條釘死的東西。
#[test]
fn 只写_passive_fail_时两个旋钮取缺省() {
    let p = passive_of(&rp("    passive_fail 3"));
    assert_eq!(p.window_ms, 10_000, "window 缺省该是 10s");
    assert_eq!(p.cooldown_ms, 30_000, "cooldown 缺省该是 30s");
}

/// 寫了旋鈕卻沒寫 `passive_fail` ⇒ **說出來，但不判紅**（G136）。
///
/// ★ 那兩行此刻一個都不生效，而它與「正在生效」在設定裡長得一模一樣
/// ⇒ 沉默就是讓人以為它生效了（與 [`DiagCode::METRICS_UNGUARDED`] 同一條紀律）。
#[test]
fn 只写旋钮没写_passive_fail_要出警告且从来不是错误() {
    for knob in ["passive_window 5s", "passive_cooldown 5s"] {
        let src = rp(&format!("    {knob}"));
        let o = compile_str("test.Fulcrumfile", &src);
        let cs: Vec<DiagCode> = o.diagnostics.items().iter().map(|d| d.code).collect();
        assert!(
            cs.contains(&DiagCode::PASSIVE_WITHOUT_THRESHOLD),
            "`{knob}` 单独写该出警告，实际：{cs:?}"
        );
        // ★ ★ 反方向，與 `有裸奔警告` 那條同一手法：它**從來不是** error ——
        //   一條把設定拒掉的門會把「先把旋鈕調好、待會兒再開」這種正當的分步操作擋掉。
        assert!(
            !o.diagnostics.has_errors(),
            "`{knob}` 不该判红：\n{}",
            o.render_diagnostics()
        );
    }
}

/// 反方向：寫了 `passive_fail` 之後，旋鈕就是生效的 ⇒ ⛔ 不許再報那條警告。
///
/// ⚠ 少了這一條，一個「無論如何都報警告」的實作照樣全綠 —— 而那種警告
/// 會在每一份正確設定上刷屏，刷到沒人再看它。
#[test]
fn 写了_passive_fail_之后旋钮不再报警告() {
    let cs = codes(&rp(
        "    passive_fail 3\n    passive_window 5s\n    passive_cooldown 20s",
    ));
    assert!(
        !cs.contains(&DiagCode::PASSIVE_WITHOUT_THRESHOLD),
        "旋钮已经生效了还报「不生效」：{cs:?}"
    );
}

// ── M3 對拍的 `respond` 上界診斷所相依的一條 DSL 契約 ────────────────────────
//
// 對拍要拿 `respond` 量出「把回應推出去」的上界，與 `file_server` 背靠背比。
// 為此那份被測設定必須內聯一份與靜態吞吐那一類**同樣位元組數**的正文
// （`BENCH_PAYLOAD_BYTES`，預設 4096），而 `respond` 的正文**只有內聯形態**
// （`respond [matcher] <status> [body]`，⛔ 沒有 file 形式，不像 HAProxy 那條
// `http-request return … file <路径>`）。
// ⇒ 「詞法與編譯吃得下這麼長的一個 token、且一個位元組都不改」是那份設定成立的前提，
//    而它此前**沒有任何判據**。本條把它釘住。

/// `respond` 的內聯正文能原樣帶過 4096 位元組。
#[test]
fn respond_的内联正文收得下四千零九十六字节且逐字节不变() {
    // ★ 字元集與 `bench/diag/` 那邊生成 payload 的一致：base64 字母表
    //   不含 `|`、`&`、反斜槓與引號 ⇒ `sed` 替換與 DSL 的引號都不會被它撐破。
    // ⛔ 有意不用 `"A".repeat(4096)`：單一重複字元測不出「內容有沒有被改寫」，
    //   只測得出長度。
    const 字母表: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let 正文: String = (0..4096)
        .map(|i| 字母表[i % 字母表.len()] as char)
        .collect();
    assert_eq!(正文.len(), 4096, "先证明这份正文自己真的是 4096 字节");

    let cfg = ok(&format!(":9944 {{\n    respond 200 \"{正文}\"\n}}\n"));

    let step = &cfg.sites[0].chain[0];
    match &step.body {
        StepBody::Respond { status, body } => {
            assert_eq!(*status, 200);
            let 收到 = body.as_deref().expect("respond 的正文不该丢成 None");
            assert_eq!(
                收到.len(),
                4096,
                "正文长度在编译过程中变了：{} 字节",
                收到.len()
            );
            assert_eq!(收到, 正文, "正文必须逐字节原样带过来");
        }
        其它 => panic!("第 70 步该是 respond，实际是 {}", 其它.directive_name()),
    }
}
