//! 端到端：一段 DSL 進去，**路由決策**出來。
//!
//! ★ 這一層測的是「決策」，不是「位元組」。中間不起監聽、不發流量、不解析回應——
//! 於是一條規則錯了，紅的那一行就直接指著它。
//! 真流量那一層由 `tests/serve/run.sh` 單獨覆蓋（那裡測的是**資料面把決策執行對沒有**）。

use fulcrum_config::compile_str;
use fulcrum_runtime::request::{HeaderList, RequestCtx};
use fulcrum_runtime::template::Template;
use fulcrum_runtime::{
    LbPolicy, Outcome, Routed, Runtime, SiteMatch, ip_hash_source, less_loaded, weighted_slot,
};
use std::net::IpAddr;
use std::time::UNIX_EPOCH;

/// DSL → 執行環境圖。建置期回報錯誤就 panic（測試裡那是寫錯了案例）。
fn rt(dsl: &str) -> Runtime {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(
        !o.diagnostics.has_errors(),
        "DSL 编译不过：\n{}",
        o.render_diagnostics()
    );
    Runtime::build(&o.config.unwrap()).expect("运行时图应当能建起来")
}

/// DSL → 建置期錯誤（用來測「結構化層也會檢核」）。
fn build_errors(dsl: &str) -> Vec<String> {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    match Runtime::build(&o.config.unwrap()) {
        Ok(_) => Vec::new(),
        Err(e) => e.iter().map(|x| x.to_string()).collect(),
    }
}

struct Req {
    host: String,
    port: u16,
    method: String,
    path: String,
    query: String,
    ip: Option<IpAddr>,
}

impl Req {
    fn new(host: &str, path: &str) -> Req {
        Req {
            host: host.to_string(),
            port: 443,
            method: "GET".to_string(),
            path: path.to_string(),
            query: String::new(),
            ip: Some("10.1.2.3".parse().unwrap()),
        }
    }
    fn port(mut self, p: u16) -> Req {
        self.port = p;
        self
    }
    fn method(mut self, m: &str) -> Req {
        self.method = m.to_string();
        self
    }
    fn ip(mut self, ip: &str) -> Req {
        self.ip = Some(ip.parse().unwrap());
        self
    }
}

fn route<'r>(rt: &'r Runtime, r: &Req, headers: &HeaderList<'_>) -> Option<Routed<'r>> {
    let ctx = RequestCtx {
        host: &r.host,
        port: r.port,
        scheme: "https",
        method: &r.method,
        path: &r.path,
        query: &r.query,
        headers,
        remote_ip: r.ip,
        remote_port: 40000,
    };
    rt.route(&ctx)
}

fn no_headers() -> HeaderList<'static> {
    HeaderList(&[])
}

/// 便捷：只關心終結成什麼。
fn outcome_name(o: &Outcome<'_>) -> &'static str {
    match o {
        Outcome::Respond { .. } => "respond",
        Outcome::Redirect { .. } => "redirect",
        Outcome::Proxy(_) => "proxy",
        Outcome::FileServer(_) => "file_server",
        Outcome::Metrics => "metrics",
        Outcome::NoRouteMatch => "no_route",
    }
}

// ── 站台解析（G63 的 421 由這裡的 None 承擔）─────────────────────────────────

#[test]
fn 精确匹配优先于通配() {
    let r = rt("a.example.com {\n  respond 200 exact\n}\n*.example.com {\n  respond 200 wild\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.example.com", "/"), &h).unwrap();
    assert_eq!(got.site_match, SiteMatch::Exact);
    let got = route(&r, &Req::new("b.example.com", "/"), &h).unwrap();
    assert_eq!(got.site_match, SiteMatch::Wildcard);
}

#[test]
fn 通配不匹配裸域() {
    // ★ `*.example.com` 不覆蓋 `example.com` 自己——DNS 與證書裡的通配符都是這個語義。
    //   少這一條會讓裸域悄悄落進通配站台。
    let r = rt("*.example.com {\n  respond 200\n}\n");
    let h = no_headers();
    assert!(route(&r, &Req::new("x.example.com", "/"), &h).is_some());
    assert!(
        route(&r, &Req::new("example.com", "/"), &h).is_none(),
        "裸域不该落进通配站点"
    );
}

#[test]
fn 通配只吃一层_多层子域不落进通配站点() {
    // ★ ★ ★ D18 / G66。這條守的是**路由與證書兩側語義一致**。
    //
    // 之前站台索引用的是 `ends_with` 後綴匹配，於是
    // `a.b.example.com` 會被路由到 `*.example.com` 這個站台；
    // 而證書那側按 RFC 6125 只吃一層，給不出可用證書。
    // ⚠ 現場表現是**一次握手失敗，而設定裡沒有任何一行看得出問題**——
    // 請求確實進來了、站台確實匹配上了，只是沒有證書能裝。
    let r = rt("*.example.com {\n  respond 200\n}\n");
    let h = no_headers();
    assert!(
        route(&r, &Req::new("x.example.com", "/"), &h).is_some(),
        "一层子域必须中"
    );
    assert!(
        route(&r, &Req::new("a.b.example.com", "/"), &h).is_none(),
        "多层子域不该落进通配站点 —— 落进去就会拿不到证书"
    );
    assert!(
        route(&r, &Req::new("a.b.c.example.com", "/"), &h).is_none(),
        "更多层同理"
    );
}

#[test]
fn 更具体的通配站点接得住它自己那一层() {
    // ★ 「只吃一層」之後，兩個通配站台**不可能同時匹配同一個 host**：
    //   `x.a.example.com` 去掉 `.a.example.com` 只剩 `x`（中），
    //   去掉 `.example.com` 剩 `x.a`（含點，不中）。
    //   ⚠ 所以這條已經不是在測「誰優先」了，它測的是
    //   **寫了 `*.a.example.com` 的人確實拿得到 `x.a.example.com`**。
    //   （索引裡那次按後綴長度排序因此成了冗餘而非承重——留著無害，
    //     但別再把它當成「更具體者優先」的實作。）
    let r = rt("*.a.example.com {\n  respond 201\n}\n*.example.com {\n  respond 202\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("x.a.example.com", "/"), &h).unwrap();
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 201);
    // 而 `*.example.com` 只接它自己那一層。
    let got = route(&r, &Req::new("y.example.com", "/"), &h).unwrap();
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 202);
}

#[test]
fn 端口兜底与端口隔离() {
    let r = rt(":8080 {\n  respond 200\n}\na.com {\n  respond 201\n}\n");
    let h = no_headers();
    // 任意 Host 打 8080 → 備援站台
    let got = route(&r, &Req::new("whatever", "/").port(8080), &h).unwrap();
    assert_eq!(got.site_match, SiteMatch::CatchAll);
    // a.com 打 443 → 精確
    assert_eq!(
        route(&r, &Req::new("a.com", "/"), &h).unwrap().site_match,
        SiteMatch::Exact
    );
    // ★ a.com 打 8080 → 落到備援，**不是**落到 a.com：站台索引按 (host, 連接埠) 兩維。
    assert_eq!(
        route(&r, &Req::new("a.com", "/").port(8080), &h)
            .unwrap()
            .site_match,
        SiteMatch::CatchAll
    );
    // 誰都不中 → None → 呼叫方回 421
    assert!(route(&r, &Req::new("a.com", "/").port(9999), &h).is_none());
}

#[test]
fn host_大小写不敏感() {
    let r = rt("A.Example.COM {\n  respond 200\n}\n");
    let h = no_headers();
    assert!(route(&r, &Req::new("a.example.com", "/"), &h).is_some());
    assert!(route(&r, &Req::new("A.EXAMPLE.COM", "/"), &h).is_some());
}

// ── G121：`site_addr` = 請求實際匹配到的那條位址字面值 ─────────────────────────

#[test]
fn site_addr取命中的那条地址而不是站点的第一条_g121() {
    // ★ ★ ★ 本任務唯一非做不可的判據：G121 明文「不能用站台塊的第一個位址」。
    //   一個站台配兩條位址，請求命中第二條時 `site_addr` 必須是第二條的字面值，
    //   而 `site.name`（訪問紀錄的 `site` 欄位用它，R3）**仍然**是第一條位址的原文——
    //   兩者在這裡必須給出不同的值。那不是巧合，是 R3 那條裁決明寫的形狀：
    //   訪問紀錄的 `site` 與指標的 `site_addr` 長得像是同一件事，其實不是。
    let r = rt("http://a.example:9000, http://b.example:9000 {\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("b.example", "/").port(9000), &h).unwrap();
    assert_eq!(
        &*got.site_addr, "b.example",
        "应当取命中的那一条，而不是站点的第一条"
    );
    assert_eq!(
        got.site.name, "http://a.example:9000",
        "site.name 不该受影响——它按契约仍是第一条地址的原文"
    );
}

#[test]
fn site_addr在通配站点上折叠成自己的字面量而不是请求的_host() {
    // ⚠ 反向判據：`site_addr` 若不慎取成請求的 host，通配站台下的 series
    //   會隨子網域無限增長——這正是 G121 要擋的那個基數坑。
    let r = rt("http://*.wild.example:9000 {\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("x.wild.example", "/").port(9000), &h).unwrap();
    assert_eq!(
        &*got.site_addr, "*.wild.example",
        "通配地址折叠成它自己的字面量，不是命中它的那个具体 host"
    );
}

#[test]
fn site_addr在兜底地址上是冒号加端口() {
    let r = rt(":9000 {\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("whatever", "/").port(9000), &h).unwrap();
    assert_eq!(&*got.site_addr, ":9000");
}

#[test]
fn site_addr按小写折叠_r6() {
    // ★ 取小寫而不是原文：路由本來就按小寫匹配（同上 `host_大小写不敏感`），
    //   保留原文會讓只有大小寫不同的兩份設定在指標上產生兩條時序。
    let r = rt("A.Example {\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.example", "/"), &h).unwrap();
    assert_eq!(&*got.site_addr, "a.example");
}

// ── G118：`fulcrum_no_site_match_total{host}` 的封頂 ─────────────────────────

#[test]
fn has_address_literal只认配置里写着的那些字面量_g118() {
    let r = rt(
        "http://a.example:9000, http://B.Example:9000 {\n  respond 200\n}\n\
                http://*.wild.example:9000 {\n  respond 200\n}\n\
                :9001 {\n  respond 200\n}\n",
    );

    // 正向：設定裡寫著的三種形態都認得。
    assert!(r.has_address_literal("a.example"));
    assert!(r.has_address_literal("b.example"), "字面量是折成小写存的");
    assert!(
        r.has_address_literal("A.Example"),
        "传进来的大小写不该影响判定 —— 否则同一个 host 会有两条时序"
    );
    assert!(
        r.has_address_literal("*.wild.example"),
        "通配地址**自己**是一条字面量"
    );
    assert!(
        r.has_address_literal(":9001"),
        "不带主机名的兜底地址，字面量是 `:port`（R6）"
    );

    // ★ ★ ★ 反向那半 —— 這就是 G118 的全部意義：
    //   通配字面值的**子網域**判**未知**。它路由得到，但它不是一條位址字面值，
    //   而子網域由請求方隨便寫 ⇒ 認它就等於把上界重新交回給了訪問者。
    // ⚠ 少了這一條，一個改用 `resolve_site` 實作的版本會讓上面全綠，
    //   而 `fulcrum_no_site_match_total` 的 series 會隨子網域無限增長。
    assert!(!r.has_address_literal("x.wild.example"));
    assert!(!r.has_address_literal("wild.example"), "通配不覆盖裸域");
    assert!(!r.has_address_literal("nobody.example"));
    assert!(!r.has_address_literal(""));
    assert!(
        !r.has_address_literal("a.example:9000"),
        "问的是主机名，不是带端口的原文"
    );

    // ★ 而 `x.wild.example` 確實**路由得到** —— 這一條讓上面那句
    //   「路由得到、但不是字面值」不是一句空話。
    let h = no_headers();
    assert!(route(&r, &Req::new("x.wild.example", "/").port(9000), &h).is_some());
}

#[test]
fn 默认响应码就是_g63_写下的那三个() {
    let r = rt("a.com {\n  respond 200\n}\n");
    assert_eq!(r.defaults.no_site_match, 421);
    assert_eq!(r.defaults.no_route_match, 404);
    assert_eq!(r.defaults.all_upstreams_down, 502);
}

// ── 執行鏈 ──────────────────────────────────────────────────────────────────

#[test]
fn 中间件累积而终结类第一个匹配即停() {
    // ★ ★ 這條案例本身就是 G49 的演示：`respond` 寫在前面，`redir` 寫在後面，
    //   而**執行時 redir 先跑**——因為順序表裡 redir 是 60、respond 是 70。
    //   ⚠ **書寫順序的直覺是錯的** —— 那正是 G49 換來「使用者隨便寫也能跑對」
    //   時付出的那份代價。
    let r =
        rt("a.com {\n  header X-A 1\n  header X-B 2\n  respond 200 first\n  redir * /never\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/"), &h).unwrap();
    assert_eq!(got.response_headers.len(), 2, "两条 header 都要累积上");
    assert_eq!(got.response_headers[0].name, "X-A");
    assert_eq!(got.response_headers[1].name, "X-B");
    assert_eq!(outcome_name(&got.outcome), "redirect");
    assert_eq!(
        got.terminal_order,
        Some(60),
        "redir 在第 60 步，先于 respond 的 70"
    );
}

#[test]
fn 星号匹配器算兜底所以后面那条会被判为不可达() {
    // ⚠ 反向判據：`redir * /x` 是無條件終結，它後面（表裡更靠後）的終結指令
    //   永遠跑不到。**這一條必須在編譯期被說出來**——G49 配套第 4 條。
    let o = compile_str(
        "t.Fulcrumfile",
        "a.com {\n  redir * /x\n  reverse_proxy y:1\n}\n",
    );
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let w = o
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == fulcrum_config::DiagCode::UNREACHABLE_STEP)
        .expect("应当警告 reverse_proxy 跑不到");
    assert!(w.label.contains("第 60 步"), "{}", w.label);
}

#[test]
fn 站点内没有任何路由匹配时是_no_route() {
    let r = rt("a.com {\n  respond /only-here 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/elsewhere"), &h).unwrap();
    assert_eq!(outcome_name(&got.outcome), "no_route");
    assert_eq!(got.terminal_order, None);
}

#[test]
fn rewrite_之后后续匹配器看到的是新路径() {
    // ★ 這是 Caddy 的語義，也是唯一自洽的一種：否則 rewrite 就成了只影響上游、
    //   不影響本機路由的半吊子操作。
    let r = rt("a.com {\n  rewrite * /new/x\n  respond /new/* 201\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/old"), &h).unwrap();
    assert_eq!(got.rewritten_path.as_deref(), Some("/new/x"));
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 201, "改写后的路径必须参与后面的匹配");
}

#[test]
fn handle_是互斥的只有第一个匹配的分支执行() {
    let r = rt(
        "a.com {\n  handle /a/* {\n    respond 201\n  }\n  handle /* {\n    respond 202\n  }\n}\n",
    );
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/a/x"), &h).unwrap();
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 201);
    let got = route(&r, &Req::new("a.com", "/b"), &h).unwrap();
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 202);
}

#[test]
fn handle_分支不匹配时继续往后走() {
    let r = rt("a.com {\n  handle /a/* {\n    respond 201\n  }\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/b"), &h).unwrap();
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 200, "handle 一个分支都没中，应当落到后面的兜底");
}

#[test]
fn route_块内按书写顺序() {
    // ★ 逃生口：塊內 respond(70) 寫在 header(20) 前面，就先跑 respond。
    let r = rt("a.com {\n  route {\n    respond 200\n    header X-Never 1\n  }\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/"), &h).unwrap();
    assert_eq!(outcome_name(&got.outcome), "respond");
    assert!(
        got.response_headers.is_empty(),
        "respond 先终结，后面那条 header 不该跑"
    );
}

#[test]
fn 顺序表把书写顺序重排了() {
    // 倒著寫：respond 在最前，header 在最後。執行時 header 先跑（20 < 70）。
    let r = rt("a.com {\n  respond 200\n  header X-A 1\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/"), &h).unwrap();
    assert_eq!(got.response_headers.len(), 1, "header 排在 respond 之前");
}

// ⚠ ⚠ ⚠ **這條測試的夾具在兩天裡換過兩次，第二次它換掉的是「驗什麼」。**
//   回落層整層刪除之後，「該回落的指令」這個主語沒了，
//   於是它換成釘 `cache` 現在給出什麼 ——
//   ⚠ 而「假裝自己做完了」這個反面，由第二個斷言接住：
//   一個把 `cache` 當中介軟體卻什麼都不記的實作，會讓 `routed.cache` 是 `None`，
//   於是資料面走**不帶快取**的那條路 —— 設定裡寫了 `cache` 而它一點作用都沒有，
//   **且沒有任何東西會紅**。那正是這條測試從第一天起就在守的東西。
#[test]
fn 回落指令给出_fallback_而不是假装成功() {
    let r = rt(
        "a.com {\n  cache {\n    ttl 5m\n    max_size 1MB\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n",
    );
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/x"), &h).unwrap();
    // ★ `cache` 是**中介軟體**：它不佔 `outcome`，終結的仍是 `reverse_proxy`。
    assert_eq!(outcome_name(&got.outcome), "proxy");
    // ★ ★ 而它必須在 `routed.cache` 上留下痕跡 —— 否則它「假裝自己做完了」。
    let c = got.cache.expect("写了 cache 却没在 Routed 上留下任何痕迹");
    assert_eq!(c.ttl_ms, Some(300_000));
    assert_eq!(c.max_size_bytes, 1_000_000);
    // 沒寫 capacity ⇒ 用預設值，而**預設值在構圖時就算完**（不是留 None 給請求路徑）。
    assert_eq!(
        c.capacity_bytes,
        fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES
    );

    // ★ 反向：沒寫 `cache` 的鏈上，`routed.cache` 必須是 `None`。
    //   ⚠ 少了它，一個「所有鏈都當成有快取」的實作會讓上面那條全綠，
    //   而每一個沒配快取的站台都會開始快取 —— 那是最貴的那一類錯。
    let bare = rt("a.com {\n  reverse_proxy 127.0.0.1:3000\n}\n");
    let g2 = route(&bare, &Req::new("a.com", "/x"), &h).unwrap();
    assert!(g2.cache.is_none(), "没写 cache 却开了缓存");
}

// ── M2 批 H：磁碟後端那一格 ─────────────────────────────────────────────────

/// `disk` 要一路帶到 `CacheRt` 上。
///
/// ⚠ 少了它，一份寫了 `disk` 的設定會安靜地跑成記憶體後端 —— 而**設定檔案、
/// `validate`、`compile` 三處都看不出問題**，只有重新啟動一次之後快取空了才知道。
#[test]
fn disk_一路带到运行时图上() {
    let h = no_headers();
    let r = rt(
        "a.com {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n",
    );
    let got = route(&r, &Req::new("a.com", "/x"), &h).unwrap();
    let c = got.cache.expect("写了 cache 却没留下痕迹");
    assert_eq!(c.disk_dir.as_deref(), Some("/var/cache/fulcrum"));

    // ★ 反向：沒寫 `disk` ⇒ `None`（記憶體後端）。⚠ 少了它，一個把所有 `cache`
    //   都當磁碟的實作會讓上面那條全綠，而每個沒配 `disk` 的站台都會開始寫入磁碟。
    let mem = rt("a.com {\n  cache {\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n");
    let g2 = route(&mem, &Req::new("a.com", "/x"), &h).unwrap();
    assert_eq!(g2.cache.expect("该有 cache").disk_dir, None);
}

/// ⚠ ⚠ **結構化設定是公開入口**（G11）：DSL 那側的「必須絕對路徑」在這裡必須
/// **再有一道**，否則一份手寫 JSON 就能繞過去。
///
/// ★ 與 `file_server` 的 root 同款 —— 而繞過去之後的現場是
/// 「快取目錄建在處理程序 cwd 底下」，**一個字的回報錯誤都不會有**。
#[test]
fn 结构化层也拦相对的_disk() {
    // ★ 從一份**編譯得過**的設定出發，然後像手寫 JSON 那樣把值改壞 ——
    //   這正是這道門要擋的那種輸入（DSL 層根本產不出它來）。
    let o = compile_str(
        "t.Fulcrumfile",
        "a.com {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n",
    );
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let mut cfg = o.config.unwrap();
    let mut patched = false;
    for site in &mut cfg.sites {
        for step in &mut site.chain {
            if let fulcrum_config::model::StepBody::Cache { disk_dir, .. } = &mut step.body {
                *disk_dir = Some("var/cache/fulcrum".to_string());
                patched = true;
            }
        }
    }
    assert!(patched, "夹具没改到东西 —— 那这条判据什么都没验");
    let errs = match Runtime::build(&cfg) {
        Ok(_) => Vec::new(),
        Err(e) => e.iter().map(|x| x.to_string()).collect(),
    };
    assert!(
        errs.iter().any(|e| e.contains("绝对路径")),
        "结构化层放过了一个相对的 disk：{errs:?}"
    );
}

/// ⚠ ⚠ **結構化層也要攔「兩個 `cache` 塊選了兩個後端」**（G11：公開入口）。
///
/// DSL 那側有 `FUL-DSL-0035`，而它**只活在編譯層** —— 一份手寫 JSON 繞得過去，
/// 繞過去之後 `serve()` 會取到**第一個**目錄，另一個站台的快取整個落在別處，
/// ★ 而設定檔案、`validate`、裝載紀錄三處都顯得正常。
#[test]
fn 结构化层也拦两个不同的缓存后端() {
    let o = compile_str(
        "t.Fulcrumfile",
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let mut cfg = o.config.unwrap();
    // 把**第二個**站台的目錄改掉 —— DSL 層根本產不出這份設定來。
    let mut patched = 0;
    for site in &mut cfg.sites {
        // ⚠ `SiteConfig` 沒有 `name`，站台身分在 `addresses[].host` 上。
        if !site.addresses.iter().any(|a| a.host == "b.com") {
            continue;
        }
        for step in &mut site.chain {
            if let fulcrum_config::model::StepBody::Cache { disk_dir, .. } = &mut step.body {
                *disk_dir = Some("/var/cache/two".to_string());
                patched += 1;
            }
        }
    }
    assert_eq!(patched, 1, "夹具没改到东西 —— 那这条判据什么都没验");
    let errs = match Runtime::build(&cfg) {
        Ok(_) => Vec::new(),
        Err(e) => e.iter().map(|x| x.to_string()).collect(),
    };
    assert!(
        errs.iter().any(|e| e.contains("进程级")),
        "结构化层放过了两个不同的缓存后端：{errs:?}"
    );

    // ★ 反向那一半：一致的那份必須建得起來。⚠ 少了它，一條「只要有兩個 cache
    //   就回報錯誤」的實作會讓上面那條全綠，而它把一份完全正確的設定也攔了。
    let ok = rt(
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    assert_eq!(ok.cache_settings().len(), 2);
}

// ★ `cache` 在 `handle` / `route` 容器裡也要被記到（走的是同一條遞歸）。
#[test]
fn 容器里的_cache_也记得到() {
    let h = no_headers();
    let r = rt(
        "a.com {\n  handle /api/* {\n    cache {\n      ttl 1m\n    }\n    reverse_proxy 127.0.0.1:3000\n  }\n  handle {\n    reverse_proxy 127.0.0.1:3001\n  }\n}\n",
    );
    let hit = route(&r, &Req::new("a.com", "/api/x"), &h).unwrap();
    assert!(hit.cache.is_some(), "handle 里的 cache 没被记到");
    // ⚠ 另一個分支**不該**沾上快取 —— 互斥容器裡只有命中的那一支執行。
    let miss = route(&r, &Req::new("a.com", "/other"), &h).unwrap();
    assert!(miss.cache.is_none(), "没走到的分支上的 cache 不该生效");
}

// ★ 上面那條騰出來的位置由這一條接上：`file_server` 現在必須給出**自研**那個結果。
//   ⚠ 少了它，「把 file_server 改回回落」會一聲不響地通過。
#[test]
fn file_server_给出自研的_fileserver_而不再回落() {
    let r =
        rt("a.com {\n  file_server browse {\n    root /srv/www\n    index a.html b.html\n  }\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/x"), &h).unwrap();
    let Outcome::FileServer(fs) = got.outcome else {
        panic!(
            "应当是自研 file_server，实际 {}",
            outcome_name(&got.outcome)
        )
    };
    assert_eq!(fs.root, "/srv/www");
    assert!(fs.browse);
    assert_eq!(fs.index, ["a.html", "b.html"]);
    // 沒寫 follow_symlinks / hide_defaults ⇒ 兩條預設都是 true（G87 / G88）。
    assert!(fs.follow_symlinks, "缺省跟随符号链接");
    // ★ ★ 預設 hide 清單**非空**，而且必須是「合併好的最終清單」——
    //   執行環境不該在請求路徑上再算一次。
    assert!(
        fs.hide.contains(&".git".to_string()),
        "默认表该并进来了：{:?}",
        fs.hide
    );
    assert!(fs.hide.contains(&".env".to_string()));
}

// ★ ★ G88 的兩條形狀，各釘一條：`hide` 是**追加**、`hide_defaults false` 能**關掉預設表**。
#[test]
fn hide_是追加而_hide_defaults_false_关掉默认表() {
    let r =
        rt("a.com {\n  file_server {\n    root /srv/www\n    hide secret\n    hide a b\n  }\n}\n");
    let h = no_headers();
    let Outcome::FileServer(fs) = route(&r, &Req::new("a.com", "/x"), &h).unwrap().outcome else {
        panic!("应当是自研 file_server")
    };
    // 追加：預設表還在，使用者那三段也在。
    assert!(
        fs.hide.contains(&".git".to_string()),
        "追加不该把默认表挤掉"
    );
    for w in ["secret", "a", "b"] {
        assert!(fs.hide.contains(&w.to_string()), "少了 {w}：{:?}", fs.hide);
    }
    // ⚠ 兩行 `hide` 必須都生效 —— 後一行覆蓋前一行是本條最想擋住的那種寫法。
    assert!(fs.hide.contains(&"secret".to_string()) && fs.hide.contains(&"a".to_string()));

    let r2 = rt(
        "a.com {\n  file_server {\n    root /srv/www\n    hide only\n    hide_defaults false\n  }\n}\n",
    );
    let Outcome::FileServer(fs2) = route(&r2, &Req::new("a.com", "/x"), &h).unwrap().outcome else {
        panic!("应当是自研 file_server")
    };
    assert_eq!(
        fs2.hide,
        ["only"],
        "关掉默认表之后只剩用户写的：{:?}",
        fs2.hide
    );
}

// ★ 沒寫 index ⇒ 執行環境補成 `index.html`（預設值只在**一個地方**算）。
#[test]
fn 没写_index_时补成_index_html() {
    let r = rt("a.com {\n  file_server {\n    root /srv/www\n  }\n}\n");
    let h = no_headers();
    let Outcome::FileServer(fs) = route(&r, &Req::new("a.com", "/"), &h).unwrap().outcome else {
        panic!("应当是自研 file_server")
    };
    assert_eq!(fs.index, ["index.html"]);
    assert!(!fs.browse);
}

// ── 匹配器與佔位符 ──────────────────────────────────────────────────────────

#[test]
fn 命名匹配器多条件是_and() {
    let r = rt(
        "a.com {\n  @m {\n    path /x/*\n    method POST\n    remote_ip 10.0.0.0/8\n  }\n  respond @m 201\n  respond 200\n}\n",
    );
    let h = no_headers();
    let hit = |req: Req| {
        let g = route(&r, &req, &h).unwrap();
        let Outcome::Respond { status, .. } = g.outcome else {
            panic!()
        };
        status
    };
    assert_eq!(hit(Req::new("a.com", "/x/y").method("POST")), 201);
    assert_eq!(hit(Req::new("a.com", "/x/y").method("GET")), 200);
    assert_eq!(hit(Req::new("a.com", "/z").method("POST")), 200);
    assert_eq!(
        hit(Req::new("a.com", "/x/y").method("POST").ip("8.8.8.8")),
        200
    );
}

#[test]
fn 捕获组能被_path_点_n_引用() {
    let r = rt(
        "a.com {\n  @u path_regexp ^/u/([0-9]+)/(.*)$\n  rewrite @u /users/{path.1}?tail={path.2}\n  respond 200\n}\n",
    );
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/u/42/abc"), &h).unwrap();
    assert_eq!(got.captures, vec!["42".to_string(), "abc".to_string()]);
    assert_eq!(got.rewritten_path.as_deref(), Some("/users/42?tail=abc"));
}

#[test]
fn 响应头模板在写响应时才展开() {
    let r = rt("a.com {\n  header X-Where {host}{path}\n  respond 200\n}\n");
    let h = no_headers();
    let req = Req::new("a.com", "/p");
    let got = route(&r, &req, &h).unwrap();
    let op = got.response_headers[0];
    let ctx = RequestCtx {
        host: &req.host,
        port: req.port,
        scheme: "https",
        method: &req.method,
        path: &req.path,
        query: "",
        headers: &h,
        remote_ip: req.ip,
        remote_port: 1,
    };
    let v = op
        .value
        .as_ref()
        .unwrap()
        .expand(&ctx, &Default::default(), &[], UNIX_EPOCH);
    assert_eq!(v, "a.com/p");
    // 純字面值能免掉分配這條路也走一遍
    assert_eq!(Template::parse("plain").as_literal(), Some("plain"));
}

// ── 上游與負載均衡 ──────────────────────────────────────────────────────────

fn proxy_of<'r>(r: &'r Runtime, req: &Req, h: &HeaderList<'_>) -> &'r fulcrum_runtime::ProxyTarget {
    let got = route(r, req, h).unwrap();
    match got.outcome {
        Outcome::Proxy(t) => t,
        other => panic!("应当是 proxy，实际 {}", outcome_name(&other)),
    }
}

#[test]
fn 上游地址被归一成_主机冒号端口() {
    let r = rt("a.com {\n  reverse_proxy backend 10.0.0.1:8080\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let addrs: Vec<&str> = t.upstreams.iter().map(|u| u.addr.as_str()).collect();
    assert_eq!(
        addrs,
        vec!["backend:80", "10.0.0.1:8080"],
        "缺端口按 transport 补"
    );
}

#[test]
fn https_上游缺端口补_443() {
    let r = rt("a.com {\n  reverse_proxy backend {\n    transport https\n  }\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    assert_eq!(t.upstreams[0].addr, "backend:443");
    assert!(t.tls);
}

#[test]
fn round_robin_轮转() {
    // ⚠ ⚠ 上游寫成 **IP 字面值**，不是網域。
    //   批 10 起，`pick()` 會跳過「解析不出位址」的上游，
    //   而網域在單測裡（離線、沒跑過解析）永遠是那種狀態。
    //   ★ 這三條 lb 測試當時**當場紅了** —— 那正是新行為該有的樣子：
    //   它們此前測的是「所有上游永遠可用」那個世界。
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    assert_eq!(t.policy, LbPolicy::RoundRobin);
    let req = Req::new("a.com", "/");
    let ctx_of = |req: &Req| RequestCtx {
        host: "a.com",
        port: 443,
        scheme: "https",
        method: "GET",
        path: "/",
        query: "",
        headers: &h,
        remote_ip: req.ip,
        remote_port: 1,
    };
    let picks: Vec<&str> = (0..6)
        .map(|_| t.pick(&ctx_of(&req)).unwrap().addr.as_str())
        .collect();
    assert_eq!(
        picks,
        vec![
            "10.0.0.1:1",
            "10.0.0.2:2",
            "10.0.0.3:3",
            "10.0.0.1:1",
            "10.0.0.2:2",
            "10.0.0.3:3"
        ]
    );
}

#[test]
fn ip_hash_对同一个客户端稳定且跨进程一致() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    lb_policy ip_hash\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let pick = |ip: &str| {
        let ipa: IpAddr = ip.parse().unwrap();
        let ctx = RequestCtx {
            host: "a.com",
            port: 443,
            scheme: "https",
            method: "GET",
            path: "/",
            query: "",
            headers: &h,
            remote_ip: Some(ipa),
            remote_port: 1,
        };
        t.pick(&ctx).unwrap().addr.clone()
    };
    // 同一個 IP 連問十次必須是同一個上游
    let first = pick("10.1.2.3");
    for _ in 0..10 {
        assert_eq!(pick("10.1.2.3"), first);
    }
    // ★ 而且不是「所有 IP 都落同一個」——那樣粘性是有的、均衡是沒的。
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..30 {
        seen.insert(pick(&format!("10.1.2.{i}")));
    }
    assert!(
        seen.len() >= 2,
        "30 个不同的 IP 全落在同一个上游上：{seen:?}"
    );
}

#[test]
fn least_conn_选在飞连接最少的() {
    let r =
        rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    lb_policy least_conn\n  }\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let ctx = RequestCtx {
        host: "a.com",
        port: 443,
        scheme: "https",
        method: "GET",
        path: "/",
        query: "",
        headers: &h,
        remote_ip: None,
        remote_port: 1,
    };
    // 都是 0 → 取下標最小
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
    t.upstreams[0].acquire();
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.2:2");
    t.upstreams[0].release();
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
    // ★ release 多調一次不會 wrap 成天文數字（那會讓這個上游永遠不被選）
    t.upstreams[0].release();
    assert_eq!(t.upstreams[0].inflight(), 0);
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
}

// ── 加權排程（M2 批 N 任務 2；裁決 R4 / R5）──────────────────────────────────
//
// ★ ★ ★ 這一組裡**最要緊的是「等權迴歸護欄」那四條**：全部權重為 1 時，
// 逐個請求的落點必須與這一批之前**逐字一致**。它保證接權重這件事
// **沒有順手改掉已經在生產上跑著的行為** —— 而那正是一次「順手」最容易毀掉的東西。
//
// ⚠ 四條護欄都把**今天的規則**（`x % n`）逐字寫在判據裡，再與實作的輸出比，
// ⛔ 不拿 `weighted_slot` 去算期望 —— 那是拿被測的東西證明被測的東西。
// ★ `ip_hash` 與 `random` 的「取數」因此被拆成 `ip_hash_source` / `random_source`
// 兩個純函式：判據自己再實作一遍雜湊就是第二份實作，而兩份實作遲早分家。

/// ★ ★ ★ **等權迴歸護欄（純函式那一半，窮舉）**：權重全是 1 時，
/// [`weighted_slot`] 逐字等於四條分支這一批之前各自寫的 `% n`。
#[test]
fn 等权时_weighted_slot_就是取模() {
    for n in 1..=8usize {
        let w = vec![1u32; n];
        for pos in 0..1000usize {
            assert_eq!(
                weighted_slot(pos, &w),
                pos % n,
                "n={n} pos={pos}：等权时累积权重必须退化成取模"
            );
        }
    }
}

/// 不等權時的落點是**累積區間**，並且**連發**（裁決 R5 的代價，寫在判據裡）。
#[test]
fn 累积权重的落点是连发而不是平滑() {
    // 3:1 ⇒ 一輪四個位置是 a a a b。⛔ 平滑加權會給 a a b a。
    let got: Vec<usize> = (0..8).map(|k| weighted_slot(k, &[3, 1])).collect();
    assert_eq!(got, vec![0, 0, 0, 1, 0, 0, 0, 1]);
    // 中間那一格寬 2：1:2:1 的一輪是 a b b c。
    let got: Vec<usize> = (0..4).map(|k| weighted_slot(k, &[1, 2, 1])).collect();
    assert_eq!(got, vec![0, 1, 1, 2]);
}

/// ★ ★ **等權迴歸 ①/四**：`round_robin` 的遊標序列逐字不變。
#[test]
fn 等权回归_round_robin_的游标序列逐字不变() {
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    // ★ 這一批之前的規則，逐字寫出來：遊標從 0 起，落點 = `游标 % 3`。
    let expected: Vec<usize> = (0..12).map(|c| c % 3).collect();
    let got: Vec<usize> = (0..12).map(|_| t.pick_index_by(None).unwrap()).collect();
    assert_eq!(got, expected);
}

/// ★ ★ **等權迴歸 ②/四**：`ip_hash` 同一個 IP 的落點 = 這一批之前的 `hash % n`。
#[test]
fn 等权回归_ip_hash_的落点等于哈希取模() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    lb_policy ip_hash\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    for i in 0..64u32 {
        let ip = IpAddr::V4(std::net::Ipv4Addr::from(0x0A09_0000 + i * 7919));
        assert_eq!(
            t.pick_index_by(Some(ip)).unwrap(),
            (ip_hash_source(ip) as usize) % 3,
            "{ip} 的落点变了"
        );
    }
}

/// ★ ★ **等權迴歸 ③/四**：`least_conn` 逐個請求的落點，含**平票取下標最小**。
#[test]
fn 等权回归_least_conn_平票取下标最小() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    lb_policy least_conn\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    // 全 0 ⇒ 恆取下標最小的那個，問多少次都是它（`least_conn` 不消耗遊標）。
    for _ in 0..5 {
        assert_eq!(t.pick_index_by(None), Some(0));
    }
    t.upstreams[0].acquire();
    assert_eq!(t.pick_index_by(None), Some(1));
    t.upstreams[1].acquire();
    assert_eq!(t.pick_index_by(None), Some(2));
    t.upstreams[2].acquire();
    // 三家都在飛 1 條 ⇒ 又是平票 ⇒ 下標最小。
    assert_eq!(t.pick_index_by(None), Some(0));
}

/// ★ ★ **等權迴歸 ④/四**：`random` 的序列逐字不變。
///
/// ⚠ 種子每處理程序隨機（`RandomState::new()`），所以**沒有跨處理程序字面值可釘**；
/// 判據寫成「同一個處理程序裡，落點 = `random_source(游标) % n`」——
/// 右邊那個 `% n` 正是這一批之前的那一行。
#[test]
fn 等权回归_random_的序列逐字不变() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    lb_policy random\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let expected: Vec<usize> = (0..300u64)
        .map(|c| (t.random_source(c) as usize) % 3)
        .collect();
    let got: Vec<usize> = (0..300).map(|_| t.pick_index_by(None).unwrap()).collect();
    assert_eq!(got, expected);
    // ★ 順帶釘住它沒退化成常數（一個恆回傳 0 的實作也會讓上面那條綠）。
    let mut seen: std::collections::BTreeSet<usize> = Default::default();
    seen.extend(got.iter().copied());
    assert_eq!(seen.len(), 3, "300 次抽样只落到 {seen:?}");
}

/// **比例判據 ①**：`round_robin` 的 3:1。樣本量寫死在判據裡。
#[test]
fn 三比一下_round_robin_的落点比例() {
    const N: usize = 4000;
    let r =
        rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    weight 10.0.0.1:1 3\n  }\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let picks: Vec<usize> = (0..N).map(|_| t.pick_index_by(None).unwrap()).collect();
    // 連發（R5 的代價）：頭八個是 a a a b a a a b。
    assert_eq!(picks[..8], [0, 0, 0, 1, 0, 0, 0, 1]);
    let a = picks.iter().filter(|i| **i == 0).count();
    let b = N - a;
    assert!(
        a.abs_diff(N * 3 / 4) * 100 <= N * 3 / 4 * 5 && b.abs_diff(N / 4) * 100 <= N / 4 * 5,
        "3:1 打了 {N} 次，落点 {a}:{b}（要 {}:{}，±5%）",
        N * 3 / 4,
        N / 4
    );
}

/// **比例判據 ②**：`ip_hash` 的 3:1，用**一批不同的用戶端 IP**。
///
/// ⚠ IP 取一串**散開**的位址而不是連續位址：連續位址的低位與總權重同週期，
/// 讀數會恰好等於理想值，而一把只在特意挑過的輸入上準的尺子不算尺子。
#[test]
fn 三比一下_ip_hash_的落点比例() {
    const N: usize = 8000;
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    lb_policy ip_hash\n    weight 10.0.0.1:1 3\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let mut a = 0usize;
    for i in 0..N as u32 {
        let scattered = i.wrapping_mul(2_654_435_761) & 0x00FF_FFFF;
        let ip = IpAddr::V4(std::net::Ipv4Addr::from(0x0A00_0000 | scattered));
        if t.pick_index_by(Some(ip)).unwrap() == 0 {
            a += 1;
        }
    }
    let b = N - a;
    assert!(
        a.abs_diff(N * 3 / 4) * 100 <= N * 3 / 4 * 5 && b.abs_diff(N / 4) * 100 <= N / 4 * 5,
        "3:1 打了 {N} 个不同的客户端 IP，落点 {a}:{b}（要 {}:{}，±5%）",
        N * 3 / 4,
        N / 4
    );
}

/// ★ ★ ★ **權重與可用性篩選的交互**：被摘掉的上游**連它的權重一起出局**。
///
/// ⚠ 這一條是「先篩可用集、再按權重挑」那個順序的**全部意義**：
/// 反過來的話，「按 3:1 分而那個 3 沒在跑」會讓 3/4 的請求落空 ——
/// 而現場只看得到「一部分請求 502」，設定、健康檢查、權重全都讀起來正常。
#[test]
fn 摘掉的上游的权重不再计入总权重() {
    const N: usize = 100;
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    weight 10.0.0.1:1 3\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let count = |t: &fulcrum_runtime::ProxyTarget| {
        let mut hits = [0usize; 3];
        for _ in 0..N {
            hits[t.pick_index_by(None).unwrap()] += 1;
        }
        hits
    };
    // 摘之前：3:1:1（總權重 5）。★ 先證明這一半成立，否則下面的對比說明不了什麼。
    assert_eq!(count(t), [60, 20, 20]);
    // 把**權重最大的那個**判死。
    t.upstreams[0].set_healthy(false);
    // 之後：總權重是 2 而不是 5 ⇒ 剩下兩個嚴格 1:1，**一次都不落空**。
    assert_eq!(count(t), [0, 50, 50]);
}

/// `least_conn` 認權重：比的是 `inflight / weight`。
#[test]
fn least_conn_比的是_inflight_除以权重() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    lb_policy least_conn\n    weight 10.0.0.2:2 3\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    // 兩邊都在飛 3 條：3/1 = 3 對 3/3 = 1 ⇒ 取權重大的那個。
    for _ in 0..3 {
        t.upstreams[0].acquire();
        t.upstreams[1].acquire();
    }
    assert_eq!(t.pick_index_by(None), Some(1));
    // 比值相等（1/1 與 3/3）⇒ 平票，取**下標最小**。
    t.upstreams[0].release();
    t.upstreams[0].release();
    assert_eq!(
        (t.upstreams[0].inflight(), t.upstreams[1].inflight()),
        (1, 3)
    );
    assert_eq!(t.pick_index_by(None), Some(0));
}

/// ★ ★ `least_conn` 的比較**不許走浮點**。
///
/// ⚠ 判據挑的是一對 `f64` **分不開、而整數分得開**的數：`2⁵³` 與 `2⁵³ + 1`
/// 在 `f64` 裡是同一個數 ⇒ 一個改用浮點的實作會在這裡判反。
#[test]
fn least_conn_的比较不许走浮点() {
    let a = 1usize << 53;
    assert!(
        less_loaded(a, 1, a + 1, 1),
        "2⁵³ 比 2⁵³+1 更空 —— 整数比得出来，浮点比不出来"
    );
    assert!(!less_loaded(a + 1, 1, a, 1));
    // 反證這一對數**確實**能咬到浮點：它們在 f64 裡連位模式都一模一樣
    //   ⇒ 任何走浮點的比較都分不開它們，於是上面那條斷言會判反。
    assert_eq!(
        (a as f64).to_bits(),
        ((a + 1) as f64).to_bits(),
        "这一对数在 f64 里应当是同一个数，否则这条判据钉不住任何东西"
    );
    // 乘上權重也不許溢出：`inflight` 是 usize，乘 65535 會把 u64 撐爆
    // （debug 下 panic、release 下靜默迴繞成「這個上游永遠被選中」）。
    assert!(less_loaded(usize::MAX - 1, 65_535, usize::MAX, 65_535));
}

// ── 建置期檢核：DSL 層查不到、而結構化層是公開入口 ──────────────────────────

/// ⚠ 權重的值域在 DSL 那邊由 `FUL-DSL-0040` 擋、在 JSON 那邊由 `UpstreamSpec`
/// 的反序列化擋，而一份**在處理程序裡手搓**的結構化設定兩道都不經過（G11）。
/// ★ 權重 0 的表現不是回報錯誤而是**這臺機器永遠不被選中**，靜默。
#[test]
fn 结构化层写进一个越界权重是构建期错误() {
    let o = compile_str("t.Fulcrumfile", "a.com {\n  reverse_proxy 10.0.0.1:1\n}\n");
    let mut cfg = o.config.expect("夹具应当能编译");
    let mut patched = false;
    for s in &mut cfg.sites {
        for st in &mut s.chain {
            if let fulcrum_config::model::StepBody::ReverseProxy { upstreams, .. } = &mut st.body {
                upstreams[0].weight = 0;
                patched = true;
            }
        }
    }
    assert!(patched, "夹具里应当有一条 reverse_proxy");
    let errs = match Runtime::build(&cfg) {
        Ok(_) => panic!("权重 0 应当在构建期被拒，却建起来了"),
        Err(e) => e,
    };
    assert!(
        errs.iter().any(|e| e.to_string().contains("权重")),
        "报的不是权重那条：{errs:?}"
    );
}

/// 把一份編得過的設定裡那條 `reverse_proxy` 的 `id` 換成 `id`，再建圖。
///
/// ★ 改的是**編譯之後**那份結構化設定 —— 編譯期那道門已經跑完了，
/// 於是下面幾條量的確實是執行環境圖自己那道。
fn build_errors_with_id(id: Option<&str>) -> Vec<String> {
    let o = compile_str("t.Fulcrumfile", "a.com {\n  reverse_proxy 10.0.0.1:1\n}\n");
    let mut cfg = o.config.expect("夹具应当能编译");
    let mut patched = false;
    for s in &mut cfg.sites {
        for st in &mut s.chain {
            if let fulcrum_config::model::StepBody::ReverseProxy { id: slot, .. } = &mut st.body {
                *slot = id.map(str::to_string);
                patched = true;
            }
        }
    }
    assert!(patched, "夹具里应当有一条 reverse_proxy");
    match Runtime::build(&cfg) {
        Ok(_) => Vec::new(),
        Err(e) => e.iter().map(|x| x.to_string()).collect(),
    }
}

/// ⚠ `id` 的取值域在 DSL 那邊由 `FUL-DSL-0043` 擋，而一份**在處理程序裡手搓**的
/// 結構化設定那一道**一次都不經過**（G11）。
///
/// ★ ★ 這一格的失效形態是**裝得上**：`id "<none>"` 會在 `/stats` 與覆蓋層的鍵裡
/// **冒充**本儲存庫的備援記號（`<other>` / `<none>` / `<unknown>` / `<undeclared>`
/// 全靠尖括號在真值裡不可能出現），而沒有任何東西回報錯誤。
/// ⛔ 別把它讀成「基數風險」：`id` 一個字都不進指標（G126）。
#[test]
fn 结构化层写进一个不合法的_id_是构建期错误() {
    // ⚠ 六種形態各一條，⛔ 不合並：它們各自對應一個**不同的**失效現場，
    //   而一條「只判其中一種」的實作在合併寫法下照樣綠。
    for (什么, 值) in [
        ("空格", "a b".to_string()),
        ("尖括号（冒充兜底记号）", "<none>".to_string()),
        // ★ 帶**真換行**的那一格只能在這一層問：DSL 的引號串不能跨行，詞法層先回
        //   `FUL-DSL-0003` ⇒ 它壓根到不了 `proxy_id`，只有手搓的 JSON 遞得進來。
        ("真换行", "a\nb".to_string()),
        ("中文", "网页池".to_string()),
        ("65 个字符（上限 64）", "a".repeat(65)),
        // ⚠ 空串在**這一層**走同一條：執行環境側沒有診斷碼之分。DSL 那條路上它有
        //   自己的 `FUL-DSL-0042` 與自己那段解釋，而那段解釋是給**寫設定的人**的。
        ("空串", String::new()),
    ] {
        let e = build_errors_with_id(Some(&值));
        assert!(!e.is_empty(), "`id` 里的{什么}应当在构建期被拒");
        // ★ 訊息要把那個值**原樣點出來**，否則維運手上只有「有個 id 不合法」。
        //   ⚠ 斷言的是 `{:?}` 那個形態：帶換行的值必須被轉義，⛔ 不許讓一個換行
        //   把裝載錯誤那一行在人眼裡斷成兩行（那正是收緊的理由之二）。
        assert!(
            e.iter().any(|x| x.contains(&format!("{值:?}"))),
            "装载错误没点名那个不合法的 id（{什么}）：{e:?}"
        );
    }
}

#[test]
fn 不合法的_id_的装载错误说得出合法的样子长什么样() {
    // ★ 一條只說「不合法」的訊息會讓人一次次去試 —— 判據要求它**把值域說出來**。
    //   ⚠ 斷言的是「說出來了」，⛔ 不是那句話的逐字文字：它只有一份
    //   （`model::proxy_id_shape`），逐字斷言會把同一句話釘在兩處。
    let e = build_errors_with_id(Some("<none>")).join("\n");
    assert!(e.contains("64"), "没说长度上限：{e}");
    assert!(e.contains("A-Za-z0-9_.-"), "没说清允许哪些字符：{e}");
}

#[test]
fn 合法的_id_在结构化层照旧装得上() {
    // ★ ★ 反向那一半。⚠ 少了它，一個把**所有** `id` 都拒掉的實作在上面兩條上全綠 ——
    //   而它會讓 `id` 這個功能整個變成死的。
    for 名 in ["pool_web", "web-1", "a.b_c", "A", &"z".repeat(64)] {
        assert!(
            build_errors_with_id(Some(名)).is_empty(),
            "`id {名}` 该照旧装得上"
        );
    }
}

#[test]
fn 没写_id_的配置照旧装得上() {
    // ⚠ ⚠ `None` **不是**「不合法的 id」，它是「這條沒寫 id」—— 而沒寫才是絕大多數
    //   設定的樣子（`id` 選填，G125）。★ 一個把 `None` 也送進取值域檢查的實作會讓
    //   **全倉的設定**裝不上；這一條在這裡，是為了讓紅的那一行直接指著它。
    assert!(
        build_errors_with_id(None).is_empty(),
        "没写 `id` 的配置必须装得上"
    );
}

#[test]
fn 坏的_cidr_在构建期报错() {
    let e = build_errors("a.com {\n  @m remote_ip 10.0.0.0/99\n  respond @m 200\n}\n");
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].contains("remote_ip"), "{}", e[0]);
}

#[test]
fn 坏的正则在构建期报错() {
    let e = build_errors("a.com {\n  @m path_regexp ^/(unclosed\n  respond @m 200\n}\n");
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].contains("正则编不过"), "{}", e[0]);
}

#[test]
fn 坏的上游地址在构建期报错() {
    for bad in ["x:0", "x:70000", ":8080", "[::1]:80"] {
        let dsl = format!("a.com {{\n  reverse_proxy {bad}\n}}\n");
        let e = build_errors(&dsl);
        assert!(!e.is_empty(), "`{bad}` 应当在构建期被拒");
    }
    assert!(build_errors("a.com {\n  reverse_proxy ok:8080\n}\n").is_empty());
}

#[test]
fn 报错时位置指得出来() {
    let e = build_errors(
        "a.com {\n  @m remote_ip bad\n  respond @m 200\n}\nb.com {\n  @n remote_ip 10.0.0.0/8\n  respond @n 200\n}\n",
    );
    assert_eq!(e.len(), 1);
    assert!(e[0].contains("a.com"), "{}", e[0]);
    assert!(e[0].contains("@m"), "{}", e[0]);
}

// ── 監聽連接埠 ────────────────────────────────────────────────────────────────

// ── 上游位址解析（批 10）──────────────────────────────────────────────────
//
// ⚠ 這一組守的是一條**實測出來的真缺陷**：改之前，網域上游是在每個請求上由
//   `HttpPeer::new` 阻塞解析一次，失敗還 panic（每請求一次）。
//   見 `fulcrum_runtime::Upstream` 與 `fulcrum_server::dns` 的類型/模組文件。

#[test]
fn ip_字面量在构建期就填好了不必再解析() {
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:8080\n}\n");
    let ups = r.all_upstreams();
    assert_eq!(ups.len(), 1);
    assert!(ups[0].is_literal_ip());
    assert_eq!(
        ups[0].dial_addr().map(|a| a.to_string()),
        Some("10.0.0.1:8080".to_string()),
        "IP 字面量应当在 build 里就填好"
    );
    // ★ 而且 `resolve_upstreams` 根本不該為它去查 DNS。
    let rep = fulcrum_runtime::resolve_upstreams(&r);
    assert_eq!(rep.queried, 0, "IP 字面量不该走 DNS：{rep:?}");
}

#[test]
fn 域名上游在解析之前拿不到地址而且会被跳过() {
    // ★ ★ **這是「validate 必須離線」那條約束的直接後果**：
    //   `Runtime::build` 不做 DNS，所以網域上游一開始就是「沒有位址」。
    let r = rt("a.com {\n  reverse_proxy some-host.invalid:80\n}\n");
    let ups = r.all_upstreams();
    assert_eq!(ups.len(), 1);
    assert!(!ups[0].is_literal_ip());
    assert_eq!(ups[0].dial_addr(), None, "build 不该做 DNS");

    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let ctx = RequestCtx {
        host: "a.com",
        port: 443,
        scheme: "https",
        method: "GET",
        path: "/",
        query: "",
        headers: &h,
        remote_ip: None,
        remote_port: 1,
    };
    // ⚠ 判據是 **None**，而不是「隨便給一個」。改之前這裡給出的是那個網域，
    //   而資料面拿它去 `HttpPeer::new` 就 panic 了。
    assert!(
        t.pick(&ctx).is_none(),
        "解析不出地址的上游必须被跳过（调用方回 502）"
    );
}

#[test]
fn 一个解析出来了就只选它() {
    // ⚠ 反向那一半：只驗「全都解析不出來時回傳 None」的話，
    //   一個「永遠回傳 None」的實作照樣綠 —— 而那會讓所有反代全掛。
    let r = rt("a.com {\n  reverse_proxy h1.invalid:80 h2.invalid:80\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let ctx = RequestCtx {
        host: "a.com",
        port: 443,
        scheme: "https",
        method: "GET",
        path: "/",
        query: "",
        headers: &h,
        remote_ip: None,
        remote_port: 1,
    };
    assert!(t.pick(&ctx).is_none());
    // 讓第二個「解析出來」。
    t.upstreams[1].set_resolved(vec!["10.9.9.9:80".parse().unwrap()]);
    for _ in 0..5 {
        assert_eq!(
            t.pick(&ctx).unwrap().addr,
            "h2.invalid:80",
            "只有它可用，轮询也只能一直选它"
        );
    }
    // ★ 再讓它掉出去，又該沒得選了 —— 兩個方向都要能動。
    t.upstreams[1].set_resolved(Vec::new());
    assert!(t.pick(&ctx).is_none());
}

#[test]
fn 容器里面的上游也要被解析扫到() {
    // ⚠ ⚠ 這條守的是一個**具體的盲區**：只掃站台頂層的話，寫在 `handle` /
    //   `route` / `handle_errors` 裡的網域上游**永遠不會被解析** ——
    //   而它的症狀是「那條路由一直 502」，與 DNS 看起來毫無關係。
    //   ★ 同一個盲區在 `unwired_in_use` 上真的出現過一次（只掃站台不掃全域選項）。
    let r = rt(
        "a.com {\n  handle /x {\n    reverse_proxy 10.0.0.1:1\n  }\n  route {\n    reverse_proxy 10.0.0.2:2\n  }\n  handle {\n    respond 200\n  }\n  handle_errors {\n    reverse_proxy 10.0.0.3:3\n  }\n}\n",
    );
    let addrs: Vec<&str> = r.all_upstreams().iter().map(|u| u.addr.as_str()).collect();
    assert!(addrs.contains(&"10.0.0.1:1"), "handle 里的漏了：{addrs:?}");
    assert!(addrs.contains(&"10.0.0.2:2"), "route 里的漏了：{addrs:?}");
    assert!(
        addrs.contains(&"10.0.0.3:3"),
        "handle_errors 里的漏了：{addrs:?}"
    );
}

#[test]
fn 监听端口从地址推出来() {
    let r = rt(
        "a.com {\n  respond 200\n}\nhttp://b.com {\n  respond 200\n}\n:8080 {\n  respond 200\n}\n",
    );
    assert_eq!(
        r.listen_ports,
        vec![(80, false), (443, true), (8080, false)],
        "443 需要 TLS，另两个不需要"
    );
}

// ── 主動健康檢查（批 11）─────────────────────────────────────────────────────

#[test]
fn 状态码模式两种写法都认得而且不多认() {
    use fulcrum_runtime::StatusPattern as P;
    assert_eq!(P::parse("200"), Some(P::Exact(200)));
    assert_eq!(P::parse("2xx"), Some(P::Family(2)));
    assert!(P::parse("2xx").unwrap().matches(204));
    assert!(!P::parse("2xx").unwrap().matches(304));
    assert!(P::parse("200").unwrap().matches(200));
    assert!(!P::parse("200").unwrap().matches(201));
    // ★ 反向那一半：不認識的寫法必須回 None，而不是悄悄當成 2xx。
    //   ⚠ 一個「認不出就按 2xx 判」的實作，會把配了 `health_status 5xx`
    //   的那一組上游**全部判死**，而設定裡一個字都沒錯。
    for bad in ["", "20", "2000", "xxx", "0xx", "6xx", "099", "600", "2Xx"] {
        assert!(
            P::parse(bad).is_none(),
            "`{bad}` 不该被认成合法的状态码模式"
        );
    }
}

#[test]
fn 到期判定是纯函数而且第一次立刻探() {
    use fulcrum_runtime::probe_due;
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    // ★ 還沒探過 = 立刻探。⚠ 反過來（等一個週期再開始）會讓
    //   `health_interval 30s` 的目標在啟動後半分鐘內完全沒有保護，
    //   而那恰是剛換代、最可能有上游沒起來的時候。
    assert!(probe_due(None, t0, Duration::from_secs(30)));
    assert!(!probe_due(
        Some(t0),
        t0 + Duration::from_secs(29),
        Duration::from_secs(30)
    ));
    assert!(probe_due(
        Some(t0),
        t0 + Duration::from_secs(30),
        Duration::from_secs(30)
    ));
}

#[test]
fn 只有写了_health_uri_才有健康检查() {
    let h = no_headers();
    // 寫了：策略完成實作，而且 `health_*` 的值都跟著進來。
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 {\n    health_uri /h\n    health_interval 7s\n    health_timeout 2s\n    health_status 3xx\n  }\n}\n",
    );
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let p = t.health.as_ref().expect("配了 health_uri 就该有策略");
    assert_eq!(p.uri, "/h");
    assert_eq!(p.interval, std::time::Duration::from_secs(7));
    assert_eq!(p.timeout, std::time::Duration::from_secs(2));
    assert_eq!(p.status, fulcrum_runtime::StatusPattern::Family(3));

    // ★ 沒寫 `health_uri`，即便寫了別的 `health_*` 也**不探測**。
    //   ⚠ 反過來（有預設路徑就去探）會去打一個使用者從沒說過的路徑，
    //     而那條路徑在很多後端上是 404 ⇒ 全部上游被判死。
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 {\n    health_interval 7s\n  }\n}\n");
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    assert!(t.health.is_none(), "没写 health_uri 就不该有健康检查策略");
}

#[test]
fn 认不出的_health_status_是装载期错误不是回落成默认() {
    // ★ 走**結構化層**進來（G11：那是公開入口，一份手寫 JSON 可以帶任何字串進來），
    //   所以這裡不經 DSL，而是直接把編譯好的產物改掉再裝載。
    let mut cfg = compile_str(
        "t.Fulcrumfile",
        "http://a.com {
  reverse_proxy 10.0.0.1:1 {
    health_uri /h
  }
}
",
    )
    .config
    .unwrap();
    // 先自證：原樣是建得起來的。⚠ 少了這一步，一個「永遠失敗」的實作也全綠。
    assert!(Runtime::build(&cfg).is_ok(), "原样应当能建起来");
    let fulcrum_config::model::StepBody::ReverseProxy { health, .. } =
        &mut cfg.sites[0].chain[0].body
    else {
        panic!("第一条应当是 reverse_proxy")
    };
    health.status = "okay".to_string();
    let err = Runtime::build(&cfg).expect_err("`health_status: okay` 必须让装载失败");
    let msg = format!("{err:?}");
    // ⚠ 判據不是「失敗了」而是「因為它失敗的」：一個把任何設定都拒掉的實作
    //   同樣會 `expect_err` 通過。
    assert!(msg.contains("health_status"), "{msg}");
    assert!(msg.contains("okay"), "{msg}");
}

#[test]
fn 被判死的上游不会被选中而且恢复得回来() {
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    health_uri /h\n  }\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let ctx = RequestCtx {
        host: "a.com",
        port: 443,
        scheme: "https",
        method: "GET",
        path: "/",
        query: "",
        headers: &h,
        remote_ip: None,
        remote_port: 1,
    };
    // ★ 初值健康：探測還沒跑過時**兩個都能被選中**。
    //   ⚠ 反過來（未經證實即不可用）會讓處理程序剛起來到第一次探測之間全站 502。
    assert!(t.upstreams.iter().all(|u| u.is_healthy()));

    // 摘掉第一個 —— 輪詢必須一直落在第二個上。
    assert!(
        t.upstreams[0].set_healthy(false),
        "第一次改动要报「翻转了」"
    );
    assert!(
        !t.upstreams[0].set_healthy(false),
        "同一个值再写一次不算翻转 —— 这就是「只在状态翻转时说话」的依据"
    );
    for _ in 0..5 {
        assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.2:2");
    }
    // 兩個都摘掉 —— 一個可用的都沒有，呼叫方回 502。
    t.upstreams[1].set_healthy(false);
    assert!(t.pick(&ctx).is_none());
    // ★ 恢復那一半：只測「摘得掉」的話，一個永不恢復的實作照樣綠，
    //   而那意味著上游修好之後**永遠回不來**。
    t.upstreams[0].set_healthy(true);
    for _ in 0..5 {
        assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
    }
}

#[test]
fn 健康但解析不出地址的照样被跳过() {
    // ★ 兩條篩子是**並列**的，不是「有了健康檢查就不看 DNS 了」。
    //   ⚠ 少了這一條，一個 DNS 掛了但上一輪探測判過「健康」的上游會被選中，
    //     然後在建連接那一步回 502 —— 而 `pick()` 本來就該跳過它。
    let r = rt("a.com {\n  reverse_proxy h1.invalid:80 {\n    health_uri /h\n  }\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let ctx = RequestCtx {
        host: "a.com",
        port: 443,
        scheme: "https",
        method: "GET",
        path: "/",
        query: "",
        headers: &h,
        remote_ip: None,
        remote_port: 1,
    };
    assert!(t.upstreams[0].is_healthy(), "还没探过，判定应当是健康");
    assert!(t.pick(&ctx).is_none(), "但地址解析不出来，还是该跳过");
}

#[test]
fn 探测名额按每个目标各自的间隔发() {
    use std::time::{Duration, Instant};
    let r = rt(
        "a.com {\n  handle /fast/* {\n    reverse_proxy 10.0.0.1:1 {\n      health_uri /h\n      health_interval 1s\n    }\n  }\n  handle /slow/* {\n    reverse_proxy 10.0.0.2:2 {\n      health_uri /h\n      health_interval 60s\n    }\n  }\n}\n",
    );
    let h = no_headers();
    let fast = proxy_of(&r, &Req::new("a.com", "/fast/x"), &h);
    let slow = proxy_of(&r, &Req::new("a.com", "/slow/x"), &h);
    let t0 = Instant::now();
    // 第一次兩邊都要探（還沒探過）。
    assert!(fast.take_probe_slot(t0));
    assert!(slow.take_probe_slot(t0));
    // ★ ★ 2 秒之後：快的那條到期了，慢的那條**沒有**。
    //   ⚠ 這就是「不像 dns_refresh 那樣全庫取最小值」的全部理由 ——
    //     刷得更勤在這裡是**打在別人服務上的流量**，不是多幾次 getaddrinfo。
    let t1 = t0 + Duration::from_secs(2);
    assert!(fast.take_probe_slot(t1), "1s 的那条该探了");
    assert!(!slow.take_probe_slot(t1), "60s 的那条不该被顺带探一次");
    // ★ 「判斷」與「記時刻」是一個動作：同一時刻再要一次名額，要不到。
    assert!(!fast.take_probe_slot(t1), "名额已经领过了");
    // 沒配 health_uri 的目標永遠領不到名額。
    let r2 = rt("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n");
    let none = proxy_of(&r2, &Req::new("a.com", "/"), &h);
    assert!(!none.take_probe_slot(t0));
}

#[test]
fn 容器里面的目标也要被扫到() {
    // ⚠ 與 `all_upstreams` 同一個盲區：漏掉容器裡的目標，它會**永遠不被探測**，
    //   而症狀只是「它一直健康」——看起來完全正常。
    let r = rt(
        "a.com {\n  handle /x {\n    reverse_proxy 10.0.0.1:1 {\n      health_uri /h\n    }\n  }\n  route {\n    reverse_proxy 10.0.0.2:2\n  }\n  handle {\n    respond 200\n  }\n  handle_errors {\n    reverse_proxy 10.0.0.3:3 {\n      health_uri /h\n    }\n  }\n}\n",
    );
    let n = r.all_proxy_targets().len();
    assert_eq!(n, 3, "handle / route / handle_errors 里的三个都要在");
    let with_health = r
        .all_proxy_targets()
        .iter()
        .filter(|t| t.health.is_some())
        .count();
    assert_eq!(with_health, 2);
}

// ── M2 批 M：`metrics` 終結指令（G116）───────────────────────────────────────

#[test]
fn metrics终结在自己的表位上() {
    // ★ 兩件事一起驗，因為它們只有一起成立才有意義：
    //   ① `metrics` 真的終結（`Outcome::Metrics`）；
    //   ② 圈不住的人落到備援 `respond 403` 上，**而這不是靠書寫順序** ——
    //      夾具裡 `respond` 本來就寫在 `handle` 後面。管事的是順序表：
    //      `handle` 佔第 **55** 位，比 `respond` 的 70 早 ⇒ 命中 `@i` 的請求
    //      在 `handle` 那一步就終結在塊內的 `metrics` 上；圈外的請求沒有分支命中，
    //      `handle` 什麼都不產出，才輪到 70 的 `respond`。
    //   ⚠ ⚠ 所以 `metrics @internal` 與備援 `respond 403` **並排寫在站台塊頂層是錯的**：
    //      `metrics` 自己排 75，403 在第 70 步就先終結了，`metrics` 一次也跑不到。
    //      ⇒ 範例裡那個 `handle` 不是排版習慣，是唯一正確的寫法；寫錯時
    //      `FUL-DSL-0028` 會說出來（口徑在 `docs/architecture/dsl-reference.md`）。
    let r = rt(
        "a.com {\n  @i remote_ip 10.0.0.0/8\n  handle @i {\n    metrics\n  }\n  \
         respond 403\n}\n",
    );
    let h = no_headers();
    let inside = route(&r, &Req::new("a.com", "/m").ip("10.1.2.3"), &h).unwrap();
    assert_eq!(outcome_name(&inside.outcome), "metrics");

    let outside = route(&r, &Req::new("a.com", "/m").ip("203.0.113.9"), &h).unwrap();
    assert_eq!(
        outcome_name(&outside.outcome),
        "respond",
        "圈外的请求该落到兜底 respond 上"
    );
    let Outcome::Respond { status, .. } = outside.outcome else {
        unreachable!()
    };
    assert_eq!(status, 403);
}

// ── M2 批 N 任務 2.9：覆蓋層的鍵與「同鍵共享一格」（裁決 R6 ③ 第二輪 ⇒ G125）──
//
// ⚠ ⚠ 這一組判據在任務 2.9 裡**整體翻了向**，⛔ 一條都沒有被刪掉。
//   任務 2.8 照 R6 第一輪的口徑斷言「撞了 ⇒ 裝不上」；owner 重拍之後
//   同一批事實要斷言的是「撞了 ⇒ **兩條拿到同一格**」。
//   ★ 翻向不是弱化：每一條仍然要能紅，見下面第一條的說明。
//
// ★ ★ ★ 判定住在 **`fulcrum-runtime`**，因為鍵的第三格取的是
// `normalize_upstream` **之後**的串。⇒ 下面第一條（`myhost` vs `myhost:80`）
// 是這一組的核心：拿設定裡的原文 token 建鍵的話，**它會靜靜地分成兩格**，
// 而所有別的判據照樣綠 —— 那就是一道假門。

/// 一個站台，兩條 `reverse_proxy`（各在一個 `handle` 裡），上游與 `id` 都由呼叫方給。
fn 两条(u1: &str, id1: Option<&str>, u2: &str, id2: Option<&str>) -> String {
    fn blk(u: &str, id: Option<&str>) -> String {
        match id {
            Some(v) => format!("reverse_proxy {u} {{\n      id {v}\n    }}"),
            None => format!("reverse_proxy {u}"),
        }
    }
    format!(
        "a.com {{\n  handle /api/* {{\n    {}\n  }}\n  handle /web/* {{\n    {}\n  }}\n}}\n",
        blk(u1, id1),
        blk(u2, id2)
    )
}

/// DSL → 執行環境圖，**裝不上就把每一條裝載錯誤逐條打出來**。
///
/// ⚠ 任務 2.9 之後「同鍵」不再是裝載錯誤 ⇒ 下面每一條判據的第一句都是「裝得上」。
/// `rt()` 的 `expect` 只會說一句「執行環境圖應當能建起來」，不會說是誰擋的 ——
/// 而這一組一旦紅，第一件要知道的事恰恰是**報了哪幾條**。
fn 装得上(dsl: &str) -> Runtime {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    match Runtime::build(&o.config.unwrap()) {
        Ok(r) => r,
        Err(e) => panic!(
            "这份配置必须装得上，而它报了 {} 条装载错误：\n{}",
            e.len(),
            e.iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

/// `proxy_key_fanout()` 的可讀形態：`(站点名, id, 归一化后的上游地址, 管着几条)`。
fn 键们(r: &Runtime) -> Vec<(&str, &str, &str, usize)> {
    r.proxy_key_fanout()
        .into_iter()
        .map(|f| (f.site, f.id, f.addr, f.proxies))
        .collect()
}

#[test]
fn 核心判据_归一化之后两条拿到的键完全相同_哪怕两行原文长得不一样() {
    // ★ ★ ★ 本任務的核心判據，**從任務 2.8 原樣翻向而來**（那時斷言的是「裝不上」）。
    //   它守的東西一個字都沒變：**鍵的第三格取的是 `normalize_upstream` 之後的串，
    //   不是設定裡那個原文 token**。`backend` 與 `backend:80` 在設定裡是兩個寫法
    //   （`weight` 的位址比對就有意把它們當成兩個），在執行環境是**同一個上游**
    //   ⇒ 覆蓋層裡它們必須是**同一格**。
    // ⇒ 拿原文 token 建鍵的實作會把這一對算成兩格：維運 `disable` 了一格，
    //   另一格上的同一臺機器還在收流量，而沒有任何東西會回報錯誤。
    // ★ 實測（任務 2.8 的 INJ-1 與本任務的 INJ-1）：把鍵換成設定原文時，
    //   這一組裡**只有這一條**紅，其餘全綠。它不可替代。
    // ⚠ ⚠ ⚠ **上游名字有意不叫 `backend`**：本檔案裡別處的夾具用的就是 `backend`，
    //   而判據裡的字面值與被測物件裡的字面值重名時，那條判據可能是在驗範本不是驗資料
    //   （任務 2.8 的 INJ-13 實測栽過一次）。
    let r = 装得上(&两条("myhost", None, "myhost:80", None));
    // ① 先釘住「兩條 `reverse_proxy` 都還在」—— 否則「只有一格」可以靠**少一條**滿足。
    assert_eq!(r.keyed_proxies().len(), 2, "两条 reverse_proxy 都该在");
    // ② ★ 兩條**拿到的鍵完全相同** ⇒ 覆蓋層裡只有一格，而那一格管著兩條。
    assert_eq!(
        键们(&r),
        vec![("a.com", "", "myhost:80", 2)],
        "`myhost` 与 `myhost:80` 归一化后是同一个上游 ⇒ 必须是同一格，且管着两条"
    );
    // ③ 鍵的第三格逐字是**歸一化之後**那個串（不是 `myhost`，也不是別的什麼）。
    for p in r.keyed_proxies() {
        assert_eq!(
            p.target.upstreams[0].addr, "myhost:80",
            "键的第三格必须是归一化之后的串"
        );
    }
}

#[test]
fn 同一站点两条指着同一个上游而都没写_id_共享同一格() {
    // 主場景（任務 2.8 裡它斷言「裝不上」）：兩條原文就一模一樣。
    // ⇒ 現在斷言的是**裝得上**，且兩條落在同一格裡。
    let r = 装得上(&两条("10.0.0.1:8080", None, "10.0.0.1:8080", None));
    assert_eq!(
        键们(&r),
        vec![("a.com", "", "10.0.0.1:8080", 2)],
        "两条都没写 id ⇒ 键相同 ⇒ 同一格，一次 disable 一起生效"
    );
}

#[test]
fn 一个后端挂在四组路由后面_装得上而且共享同一格() {
    // ★ ★ 這是 `tests/serve/run.sh` 那份夾具的形狀（`/api` `/rw` `/cached` `/hc`
    //   四條 `reverse_proxy` 指著同一臺機器）—— 反代最常見的寫法。
    //   任務 2.8 照第一輪口徑把它拒了，serve / h3 / log / encode **四個端到端場景
    //   當場全部裝不上**，那正是 owner 重拍的原因。
    // ⇒ 這一條把那個形狀釘在單測裡：端到端場景紅的時候，先看它紅沒紅。
    let mut dsl = String::from("a.com {\n");
    for p in ["api", "rw", "cached", "hc"] {
        dsl.push_str(&format!(
            "  handle /{p}/* {{\n    reverse_proxy 10.0.0.1:8080\n  }}\n"
        ));
    }
    dsl.push_str("}\n");
    let r = 装得上(&dsl);
    assert_eq!(
        键们(&r),
        vec![("a.com", "", "10.0.0.1:8080", 4)],
        "四条挂在同一台后端上 ⇒ 一格，管着四条"
    );
}

#[test]
fn 给其中一条写了_id_就分成两格() {
    // ★ ★ **反向判據**：少了它，一個「所有 `reverse_proxy` 都塞進同一格」的實作
    //   照樣能讓上面每一條綠 —— 而那樣 R6 的全部價值（寫 id 就能細分）就沒了。
    let r = 装得上(&两条(
        "10.0.0.1:8080",
        None,
        "10.0.0.1:8080",
        Some("pool_web"),
    ));
    assert_eq!(
        键们(&r),
        vec![
            ("a.com", "", "10.0.0.1:8080", 1),
            ("a.com", "pool_web", "10.0.0.1:8080", 1),
        ],
        "写了 id 就分得开：两格，各管一条"
    );
    // 兩條都寫、且寫的不一樣，也分得開。
    let r = 装得上(&两条(
        "10.0.0.1:8080",
        Some("a"),
        "10.0.0.1:8080",
        Some("b"),
    ));
    assert_eq!(
        键们(&r),
        vec![
            ("a.com", "a", "10.0.0.1:8080", 1),
            ("a.com", "b", "10.0.0.1:8080", 1),
        ]
    );
}

#[test]
fn 两条写了同一个_id_照样共享同一格() {
    // ★ 反向的反向：`id` 不是「寫了就分開」的標記，它是鍵裡的一格。
    //   ⚠ 少了這一條，一個「`id` 非空就各佔一格」的實作能讓上面每一條都綠。
    let r = 装得上(&两条(
        "10.0.0.1:8080",
        Some("same"),
        "10.0.0.1:8080",
        Some("same"),
    ));
    assert_eq!(
        键们(&r),
        vec![("a.com", "same", "10.0.0.1:8080", 2)],
        "同一个 id 指着同一台机器 ⇒ 仍然是同一格"
    );
}

#[test]
fn 两条指着不同的上游而都没写_id_是两格() {
    // ★ ★ **反向判據**：少了它，一條「同一個站台裡的上游全歸一格」的實作照樣能讓
    //   主場景綠 —— 而那會讓 `disable` 一臺機器把整個站台摘掉。
    let r = 装得上(&两条("10.0.0.1:8080", None, "10.0.0.2:8080", None));
    assert_eq!(
        键们(&r),
        vec![
            ("a.com", "", "10.0.0.1:8080", 1),
            ("a.com", "", "10.0.0.2:8080", 1),
        ],
        "上游不同就是两格"
    );
}

#[test]
fn 不同站点里的同一台机器是两格() {
    // 鍵的第一格是站台名（`SiteRt::name`，裁決 R6 ④）⇒ 跨站台不併。
    let r = 装得上(
        "a.com {\n  reverse_proxy 10.0.0.1:8080\n}\nb.com {\n  reverse_proxy 10.0.0.1:8080\n}\n",
    );
    assert_eq!(
        键们(&r),
        vec![
            ("a.com", "", "10.0.0.1:8080", 1),
            ("b.com", "", "10.0.0.1:8080", 1),
        ],
        "两个站点各自指着同一台机器 ⇒ 两格，运维摘的是「这个站点块里的那台」"
    );
}

#[test]
fn 同一条_reverse_proxy_里把同一个地址写两遍只算一条() {
    // ★ 邊界（`ProxyKeyFanout::proxies` 的文件寫著）：那兩個 `Upstream` 是**同一條**
    //   `reverse_proxy` 上的同一臺機器，一次 `disable` 把它們一起摘掉本來就是要的語義
    //   ⇒ 計數裡**不該**因此顯示「這一格管著兩條 reverse_proxy」——
    //   那會讓 `/stats` 對一個完全正常的寫法喊狼來了。
    let r = 装得上("a.com {\n  reverse_proxy 10.0.0.1:8080 10.0.0.1:8080\n}\n");
    assert_eq!(
        键们(&r),
        vec![("a.com", "", "10.0.0.1:8080", 1)],
        "同一条里写两遍只算一条 reverse_proxy"
    );
}

#[test]
fn 藏在容器里的_reverse_proxy_也有键也会被数进同一格() {
    // ⚠ ⚠ 走法漏掉容器的表現是**兩件事一起靜默**：那條 `reverse_proxy` 沒有鍵
    //   （覆蓋指不到它），而 `/stats` 裡那一格的計數也少一條。
    //   ⇒ 這裡把兩條分別塞進 `route` 與 `handle_errors`。
    let r = 装得上(
        "a.com {\n  route {\n    reverse_proxy 10.0.0.1:8080\n  }\n  \
         handle_errors {\n    reverse_proxy 10.0.0.1:8080\n  }\n}\n",
    );
    assert_eq!(
        键们(&r),
        vec![("a.com", "", "10.0.0.1:8080", 2)],
        "容器里的两条也要被数进来"
    );
}

#[test]
fn 键的三格都在_keyed_proxies_上取得到() {
    // ★ 任務 3 的覆蓋層與任務 6 的 `/stats` 都從這裡取鍵 —— ⛔ 別再拼第二份。
    let r = rt(
        "a.com {\n  handle /api/* {\n    reverse_proxy backend {\n      id pool_web\n    }\n  }\n  \
         handle /web/* {\n    reverse_proxy 10.0.0.2:8080\n  }\n}\n",
    );
    let ps = r.keyed_proxies();
    assert_eq!(ps.len(), 2, "两条 reverse_proxy 都要在：{ps:?}");
    // 第一格：站台名 = `SiteRt::name`（第一條位址的原文），與訪問紀錄同一口徑。
    assert_eq!(ps[0].site, r.sites()[0].name.as_str());
    assert_eq!(ps[0].site, "a.com");
    // 第二格：寫了的取原樣，沒寫的是**空串**（不是 `None`，見 `ProxyTarget::id`）。
    assert_eq!(ps[0].id, "pool_web");
    assert_eq!(ps[1].id, "");
    // 第三格：**歸一化之後**的上游位址。
    // ⚠ 設定那一行上的原文 token（`backend`）在執行環境裡**沒有留檔** ——
    //   任務 2.8 存過一格 `raw_addr`，唯一的消費者是撞鍵那條拒絕訊息，
    //   任務 2.9 拆掉拒絕之後它就沒有消費者了，⇒ 刪。
    assert_eq!(ps[0].target.upstreams[0].addr, "backend:80");
    // ★ 走法與 `all_proxy_targets` 是**同一份實作**（任務 2.8 把那邊的第三份遍歷刪了）。
    assert_eq!(r.all_proxy_targets().len(), ps.len());
    // ★ `proxy_key_fanout` 走的也是這一份 —— 三格逐字對得上。
    assert_eq!(
        键们(&r),
        vec![
            ("a.com", "", "10.0.0.2:8080", 1),
            ("a.com", "pool_web", "backend:80", 1),
        ]
    );
}

#[test]
fn 没写_id_的那条在运行时是空串而不是别的什么() {
    // ⚠ 空串是鍵裡正正經經的一格。改成「`None` 就不查」會讓主場景靜靜漏到別的分支上。
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:8080\n}\n");
    assert_eq!(r.all_proxy_targets()[0].id, "");
}
