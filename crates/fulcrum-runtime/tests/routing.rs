//! 端到端：一段 DSL 进去，**路由决策**出来。
//!
//! ★ 这一层测的是「决策」，不是「字节」。中间不起监听、不发流量、不解析响应——
//! 于是一条规则错了，红的那一行就直接指着它。
//! 真流量那一层由 `tests/serve/run.sh` 单独覆盖（那里测的是**数据面把决策执行对没有**）。

use fulcrum_config::compile_str;
use fulcrum_runtime::request::{HeaderList, RequestCtx};
use fulcrum_runtime::template::Template;
use fulcrum_runtime::{
    LbPolicy, Outcome, Routed, Runtime, SiteMatch, ip_hash_source, less_loaded, weighted_slot,
};
use std::net::IpAddr;
use std::time::UNIX_EPOCH;

/// DSL → 运行时图。构建期报错就 panic（测试里那是写错了用例）。
fn rt(dsl: &str) -> Runtime {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(
        !o.diagnostics.has_errors(),
        "DSL 编译不过：\n{}",
        o.render_diagnostics()
    );
    Runtime::build(&o.config.unwrap()).expect("运行时图应当能建起来")
}

/// DSL → 构建期错误（用来测「结构化层也会校验」）。
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

/// 便捷：只关心终结成什么。
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

// ── 站点解析（G63 的 421 由这里的 None 承担）─────────────────────────────────

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
    // ★ `*.example.com` 不覆盖 `example.com` 自己——DNS 与证书里的通配符都是这个语义。
    //   少这一条会让裸域悄悄落进通配站点。
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
    // ★ ★ ★ D18 / G66。这条守的是**路由与证书两侧语义一致**。
    //
    // 之前站点索引用的是 `ends_with` 后缀匹配，于是
    // `a.b.example.com` 会被路由到 `*.example.com` 这个站点；
    // 而证书那侧按 RFC 6125 只吃一层，给不出可用证书。
    // ⚠ 现场表现是**一次握手失败，而配置里没有任何一行看得出问题**——
    // 请求确实进来了、站点确实匹配上了，只是没有证书能装。
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
    // ★ 「只吃一层」之后，两个通配站点**不可能同时匹配同一个 host**：
    //   `x.a.example.com` 去掉 `.a.example.com` 只剩 `x`（中），
    //   去掉 `.example.com` 剩 `x.a`（含点，不中）。
    //   ⚠ 所以这条已经不是在测「谁优先」了，它测的是
    //   **写了 `*.a.example.com` 的人确实拿得到 `x.a.example.com`**。
    //   （索引里那次按后缀长度排序因此成了冗余而非承重——留着无害，
    //     但别再把它当成「更具体者优先」的实现。）
    let r = rt("*.a.example.com {\n  respond 201\n}\n*.example.com {\n  respond 202\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("x.a.example.com", "/"), &h).unwrap();
    let Outcome::Respond { status, .. } = got.outcome else {
        panic!()
    };
    assert_eq!(status, 201);
    // 而 `*.example.com` 只接它自己那一层。
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
    // 任意 Host 打 8080 → 兜底站点
    let got = route(&r, &Req::new("whatever", "/").port(8080), &h).unwrap();
    assert_eq!(got.site_match, SiteMatch::CatchAll);
    // a.com 打 443 → 精确
    assert_eq!(
        route(&r, &Req::new("a.com", "/"), &h).unwrap().site_match,
        SiteMatch::Exact
    );
    // ★ a.com 打 8080 → 落到兜底，**不是**落到 a.com：站点索引按 (host, 端口) 两维。
    assert_eq!(
        route(&r, &Req::new("a.com", "/").port(8080), &h)
            .unwrap()
            .site_match,
        SiteMatch::CatchAll
    );
    // 谁都不中 → None → 调用方回 421
    assert!(route(&r, &Req::new("a.com", "/").port(9999), &h).is_none());
}

#[test]
fn host_大小写不敏感() {
    let r = rt("A.Example.COM {\n  respond 200\n}\n");
    let h = no_headers();
    assert!(route(&r, &Req::new("a.example.com", "/"), &h).is_some());
    assert!(route(&r, &Req::new("A.EXAMPLE.COM", "/"), &h).is_some());
}

// ── G121：`site_addr` = 请求实际匹配到的那条地址字面量 ─────────────────────────

#[test]
fn site_addr取命中的那条地址而不是站点的第一条_g121() {
    // ★ ★ ★ 本任务唯一非做不可的判据：G121 明文「不能用站点块的第一个地址」。
    //   一个站点配两条地址，请求命中第二条时 `site_addr` 必须是第二条的字面量，
    //   而 `site.name`（访问日志的 `site` 字段用它，R3）**仍然**是第一条地址的原文——
    //   两者在这里必须给出不同的值。那不是巧合，是 R3 那条裁决明写的形状：
    //   访问日志的 `site` 与指标的 `site_addr` 长得像是同一件事，其实不是。
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
    // ⚠ 反向判据：`site_addr` 若不慎取成请求的 host，通配站点下的 series
    //   会随子域名无限增长——这正是 G121 要挡的那个基数坑。
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
    // ★ 取小写而不是原文：路由本来就按小写匹配（同上 `host_大小写不敏感`），
    //   保留原文会让只有大小写不同的两份配置在指标上产生两条时序。
    let r = rt("A.Example {\n  respond 200\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.example", "/"), &h).unwrap();
    assert_eq!(&*got.site_addr, "a.example");
}

// ── G118：`fulcrum_no_site_match_total{host}` 的封顶 ─────────────────────────

#[test]
fn has_address_literal只认配置里写着的那些字面量_g118() {
    let r = rt(
        "http://a.example:9000, http://B.Example:9000 {\n  respond 200\n}\n\
                http://*.wild.example:9000 {\n  respond 200\n}\n\
                :9001 {\n  respond 200\n}\n",
    );

    // 正向：配置里写着的三种形态都认得。
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

    // ★ ★ ★ 反向那半 —— 这就是 G118 的全部意义：
    //   通配字面量的**子域名**判**未知**。它路由得到，但它不是一条地址字面量，
    //   而子域名由请求方随便写 ⇒ 认它就等于把上界重新交回给了访问者。
    // ⚠ 少了这一条，一个改用 `resolve_site` 实现的版本会让上面全绿，
    //   而 `fulcrum_no_site_match_total` 的 series 会随子域名无限增长。
    assert!(!r.has_address_literal("x.wild.example"));
    assert!(!r.has_address_literal("wild.example"), "通配不覆盖裸域");
    assert!(!r.has_address_literal("nobody.example"));
    assert!(!r.has_address_literal(""));
    assert!(
        !r.has_address_literal("a.example:9000"),
        "问的是主机名，不是带端口的原文"
    );

    // ★ 而 `x.wild.example` 确实**路由得到** —— 这一条让上面那句
    //   「路由得到、但不是字面量」不是一句空话。
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

// ── 执行链 ──────────────────────────────────────────────────────────────────

#[test]
fn 中间件累积而终结类第一个匹配即停() {
    // ★ ★ 这条用例本身就是 G49 的演示：`respond` 写在前面，`redir` 写在后面，
    //   而**执行时 redir 先跑**——因为顺序表里 redir 是 60、respond 是 70。
    //   ⚠ **书写顺序的直觉是错的** —— 那正是 G49 换来「用户随便写也能跑对」
    //   时付出的那份代价。
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
    // ⚠ 反向判据：`redir * /x` 是无条件终结，它后面（表里更靠后）的终结指令
    //   永远跑不到。**这一条必须在编译期被说出来**——G49 配套第 4 条。
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
    // ★ 这是 Caddy 的语义，也是唯一自洽的一种：否则 rewrite 就成了只影响上游、
    //   不影响本地路由的半吊子操作。
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
    // ★ 逃生口：块内 respond(70) 写在 header(20) 前面，就先跑 respond。
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
    // 倒着写：respond 在最前，header 在最后。执行时 header 先跑（20 < 70）。
    let r = rt("a.com {\n  respond 200\n  header X-A 1\n}\n");
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/"), &h).unwrap();
    assert_eq!(got.response_headers.len(), 1, "header 排在 respond 之前");
}

// ⚠ ⚠ ⚠ **这条测试的夹具在两天里换过两次，第二次它换掉的是「验什么」。**
//   回落层整层删除之后，「该回落的指令」这个主语没了，
//   于是它换成钉 `cache` 现在给出什么 ——
//   ⚠ 而「假装自己做完了」这个反面，由第二个断言接住：
//   一个把 `cache` 当中间件却什么都不记的实现，会让 `routed.cache` 是 `None`，
//   于是数据面走**不带缓存**的那条路 —— 配置里写了 `cache` 而它一点作用都没有，
//   **且没有任何东西会红**。那正是这条测试从第一天起就在守的东西。
#[test]
fn 回落指令给出_fallback_而不是假装成功() {
    let r = rt(
        "a.com {\n  cache {\n    ttl 5m\n    max_size 1MB\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n",
    );
    let h = no_headers();
    let got = route(&r, &Req::new("a.com", "/x"), &h).unwrap();
    // ★ `cache` 是**中间件**：它不占 `outcome`，终结的仍是 `reverse_proxy`。
    assert_eq!(outcome_name(&got.outcome), "proxy");
    // ★ ★ 而它必须在 `routed.cache` 上留下痕迹 —— 否则它「假装自己做完了」。
    let c = got.cache.expect("写了 cache 却没在 Routed 上留下任何痕迹");
    assert_eq!(c.ttl_ms, Some(300_000));
    assert_eq!(c.max_size_bytes, 1_000_000);
    // 没写 capacity ⇒ 用默认值，而**默认值在构图时就算完**（不是留 None 给请求路径）。
    assert_eq!(
        c.capacity_bytes,
        fulcrum_config::directive::CACHE_DEFAULT_CAPACITY_BYTES
    );

    // ★ 反向：没写 `cache` 的链上，`routed.cache` 必须是 `None`。
    //   ⚠ 少了它，一个「所有链都当成有缓存」的实现会让上面那条全绿，
    //   而每一个没配缓存的站点都会开始缓存 —— 那是最贵的那一类错。
    let bare = rt("a.com {\n  reverse_proxy 127.0.0.1:3000\n}\n");
    let g2 = route(&bare, &Req::new("a.com", "/x"), &h).unwrap();
    assert!(g2.cache.is_none(), "没写 cache 却开了缓存");
}

// ── M2 批 H：磁盘后端那一格 ─────────────────────────────────────────────────

/// `disk` 要一路带到 `CacheRt` 上。
///
/// ⚠ 少了它，一份写了 `disk` 的配置会安静地跑成内存后端 —— 而**配置文件、
/// `validate`、`compile` 三处都看不出问题**，只有重启一次之后缓存空了才知道。
#[test]
fn disk_一路带到运行时图上() {
    let h = no_headers();
    let r = rt(
        "a.com {\n  cache {\n    disk /var/cache/fulcrum\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n",
    );
    let got = route(&r, &Req::new("a.com", "/x"), &h).unwrap();
    let c = got.cache.expect("写了 cache 却没留下痕迹");
    assert_eq!(c.disk_dir.as_deref(), Some("/var/cache/fulcrum"));

    // ★ 反向：没写 `disk` ⇒ `None`（内存后端）。⚠ 少了它，一个把所有 `cache`
    //   都当磁盘的实现会让上面那条全绿，而每个没配 `disk` 的站点都会开始落盘。
    let mem = rt("a.com {\n  cache {\n    ttl 1m\n  }\n  reverse_proxy 127.0.0.1:3000\n}\n");
    let g2 = route(&mem, &Req::new("a.com", "/x"), &h).unwrap();
    assert_eq!(g2.cache.expect("该有 cache").disk_dir, None);
}

/// ⚠ ⚠ **结构化配置是公开入口**（G11）：DSL 那侧的「必须绝对路径」在这里必须
/// **再有一道**，否则一份手写 JSON 就能绕过去。
///
/// ★ 与 `file_server` 的 root 同款 —— 而绕过去之后的现场是
/// 「缓存目录建在进程 cwd 底下」，**一个字的报错都不会有**。
#[test]
fn 结构化层也拦相对的_disk() {
    // ★ 从一份**编译得过**的配置出发，然后像手写 JSON 那样把值改坏 ——
    //   这正是这道门要挡的那种输入（DSL 层根本产不出它来）。
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

/// ⚠ ⚠ **结构化层也要拦「两个 `cache` 块选了两个后端」**（G11：公开入口）。
///
/// DSL 那侧有 `FUL-DSL-0035`，而它**只活在编译层** —— 一份手写 JSON 绕得过去，
/// 绕过去之后 `serve()` 会取到**第一个**目录，另一个站点的缓存整个落在别处，
/// ★ 而配置文件、`validate`、装载日志三处都显得正常。
#[test]
fn 结构化层也拦两个不同的缓存后端() {
    let o = compile_str(
        "t.Fulcrumfile",
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    let mut cfg = o.config.unwrap();
    // 把**第二个**站点的目录改掉 —— DSL 层根本产不出这份配置来。
    let mut patched = 0;
    for site in &mut cfg.sites {
        // ⚠ `SiteConfig` 没有 `name`，站点身份在 `addresses[].host` 上。
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

    // ★ 反向那一半：一致的那份必须建得起来。⚠ 少了它，一条「只要有两个 cache
    //   就报错」的实现会让上面那条全绿，而它把一份完全正确的配置也拦了。
    let ok = rt(
        "a.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:1\n}\n\
         b.com {\n  cache {\n    disk /var/cache/one\n  }\n  reverse_proxy 127.0.0.1:2\n}\n",
    );
    assert_eq!(ok.cache_settings().len(), 2);
}

// ★ `cache` 在 `handle` / `route` 容器里也要被记到（走的是同一条递归）。
#[test]
fn 容器里的_cache_也记得到() {
    let h = no_headers();
    let r = rt(
        "a.com {\n  handle /api/* {\n    cache {\n      ttl 1m\n    }\n    reverse_proxy 127.0.0.1:3000\n  }\n  handle {\n    reverse_proxy 127.0.0.1:3001\n  }\n}\n",
    );
    let hit = route(&r, &Req::new("a.com", "/api/x"), &h).unwrap();
    assert!(hit.cache.is_some(), "handle 里的 cache 没被记到");
    // ⚠ 另一个分支**不该**沾上缓存 —— 互斥容器里只有命中的那一支执行。
    let miss = route(&r, &Req::new("a.com", "/other"), &h).unwrap();
    assert!(miss.cache.is_none(), "没走到的分支上的 cache 不该生效");
}

// ★ 上面那条腾出来的位置由这一条接上：`file_server` 现在必须给出**自研**那个结果。
//   ⚠ 少了它，「把 file_server 改回回落」会一声不响地通过。
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
    // 没写 follow_symlinks / hide_defaults ⇒ 两条默认都是 true（G87 / G88）。
    assert!(fs.follow_symlinks, "缺省跟随符号链接");
    // ★ ★ 默认 hide 清单**非空**，而且必须是「合并好的最终清单」——
    //   运行时不该在请求路径上再算一次。
    assert!(
        fs.hide.contains(&".git".to_string()),
        "默认表该并进来了：{:?}",
        fs.hide
    );
    assert!(fs.hide.contains(&".env".to_string()));
}

// ★ ★ G88 的两条形状，各钉一条：`hide` 是**追加**、`hide_defaults false` 能**关掉默认表**。
#[test]
fn hide_是追加而_hide_defaults_false_关掉默认表() {
    let r =
        rt("a.com {\n  file_server {\n    root /srv/www\n    hide secret\n    hide a b\n  }\n}\n");
    let h = no_headers();
    let Outcome::FileServer(fs) = route(&r, &Req::new("a.com", "/x"), &h).unwrap().outcome else {
        panic!("应当是自研 file_server")
    };
    // 追加：默认表还在，用户那三段也在。
    assert!(
        fs.hide.contains(&".git".to_string()),
        "追加不该把默认表挤掉"
    );
    for w in ["secret", "a", "b"] {
        assert!(fs.hide.contains(&w.to_string()), "少了 {w}：{:?}", fs.hide);
    }
    // ⚠ 两行 `hide` 必须都生效 —— 后一行覆盖前一行是本条最想挡住的那种写法。
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

// ★ 没写 index ⇒ 运行时补成 `index.html`（缺省值只在**一个地方**算）。
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

// ── 匹配器与占位符 ──────────────────────────────────────────────────────────

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
    // 纯字面量能免掉分配这条路也走一遍
    assert_eq!(Template::parse("plain").as_literal(), Some("plain"));
}

// ── 上游与负载均衡 ──────────────────────────────────────────────────────────

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
    // ⚠ ⚠ 上游写成 **IP 字面量**，不是域名。
    //   批 10 起，`pick()` 会跳过「解析不出地址」的上游，
    //   而域名在单测里（离线、没跑过解析）永远是那种状态。
    //   ★ 这三条 lb 测试当时**当场红了** —— 那正是新行为该有的样子：
    //   它们此前测的是「所有上游永远可用」那个世界。
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
    // 同一个 IP 连问十次必须是同一个上游
    let first = pick("10.1.2.3");
    for _ in 0..10 {
        assert_eq!(pick("10.1.2.3"), first);
    }
    // ★ 而且不是「所有 IP 都落同一个」——那样粘性是有的、均衡是没的。
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
    // 都是 0 → 取下标最小
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
    t.upstreams[0].acquire();
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.2:2");
    t.upstreams[0].release();
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
    // ★ release 多调一次不会 wrap 成天文数字（那会让这个上游永远不被选）
    t.upstreams[0].release();
    assert_eq!(t.upstreams[0].inflight(), 0);
    assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
}

// ── 加权调度（M2 批 N 任务 2；裁决 R4 / R5）──────────────────────────────────
//
// ★ ★ ★ 这一组里**最要紧的是「等权回归护栏」那四条**：全部权重为 1 时，
// 逐个请求的落点必须与这一批之前**逐字一致**。它保证接权重这件事
// **没有顺手改掉已经在生产上跑着的行为** —— 而那正是一次「顺手」最容易毁掉的东西。
//
// ⚠ 四条护栏都把**今天的规则**（`x % n`）逐字写在判据里，再与实现的输出比，
// ⛔ 不拿 `weighted_slot` 去算期望 —— 那是拿被测的东西证明被测的东西。
// ★ `ip_hash` 与 `random` 的「取数」因此被拆成 `ip_hash_source` / `random_source`
// 两个纯函数：判据自己再实现一遍哈希就是第二份实现，而两份实现迟早分家。

/// ★ ★ ★ **等权回归护栏（纯函数那一半，穷举）**：权重全是 1 时，
/// [`weighted_slot`] 逐字等于四条分支这一批之前各自写的 `% n`。
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

/// 不等权时的落点是**累积区间**，并且**连发**（裁决 R5 的代价，写在判据里）。
#[test]
fn 累积权重的落点是连发而不是平滑() {
    // 3:1 ⇒ 一轮四个位置是 a a a b。⛔ 平滑加权会给 a a b a。
    let got: Vec<usize> = (0..8).map(|k| weighted_slot(k, &[3, 1])).collect();
    assert_eq!(got, vec![0, 0, 0, 1, 0, 0, 0, 1]);
    // 中间那一格宽 2：1:2:1 的一轮是 a b b c。
    let got: Vec<usize> = (0..4).map(|k| weighted_slot(k, &[1, 2, 1])).collect();
    assert_eq!(got, vec![0, 1, 1, 2]);
}

/// ★ ★ **等权回归 ①/四**：`round_robin` 的游标序列逐字不变。
#[test]
fn 等权回归_round_robin_的游标序列逐字不变() {
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    // ★ 这一批之前的规则，逐字写出来：游标从 0 起，落点 = `游标 % 3`。
    let expected: Vec<usize> = (0..12).map(|c| c % 3).collect();
    let got: Vec<usize> = (0..12).map(|_| t.pick_index_by(None).unwrap()).collect();
    assert_eq!(got, expected);
}

/// ★ ★ **等权回归 ②/四**：`ip_hash` 同一个 IP 的落点 = 这一批之前的 `hash % n`。
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

/// ★ ★ **等权回归 ③/四**：`least_conn` 逐个请求的落点，含**平票取下标最小**。
#[test]
fn 等权回归_least_conn_平票取下标最小() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    lb_policy least_conn\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    // 全 0 ⇒ 恒取下标最小的那个，问多少次都是它（`least_conn` 不消耗游标）。
    for _ in 0..5 {
        assert_eq!(t.pick_index_by(None), Some(0));
    }
    t.upstreams[0].acquire();
    assert_eq!(t.pick_index_by(None), Some(1));
    t.upstreams[1].acquire();
    assert_eq!(t.pick_index_by(None), Some(2));
    t.upstreams[2].acquire();
    // 三家都在飞 1 条 ⇒ 又是平票 ⇒ 下标最小。
    assert_eq!(t.pick_index_by(None), Some(0));
}

/// ★ ★ **等权回归 ④/四**：`random` 的序列逐字不变。
///
/// ⚠ 种子每进程随机（`RandomState::new()`），所以**没有跨进程字面量可钉**；
/// 判据写成「同一个进程里，落点 = `random_source(游标) % n`」——
/// 右边那个 `% n` 正是这一批之前的那一行。
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
    // ★ 顺带钉住它没退化成常数（一个恒返回 0 的实现也会让上面那条绿）。
    let mut seen: std::collections::BTreeSet<usize> = Default::default();
    seen.extend(got.iter().copied());
    assert_eq!(seen.len(), 3, "300 次抽样只落到 {seen:?}");
}

/// **比例判据 ①**：`round_robin` 的 3:1。样本量写死在判据里。
#[test]
fn 三比一下_round_robin_的落点比例() {
    const N: usize = 4000;
    let r =
        rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    weight 10.0.0.1:1 3\n  }\n}\n");
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let picks: Vec<usize> = (0..N).map(|_| t.pick_index_by(None).unwrap()).collect();
    // 连发（R5 的代价）：头八个是 a a a b a a a b。
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

/// **比例判据 ②**：`ip_hash` 的 3:1，用**一批不同的客户端 IP**。
///
/// ⚠ IP 取一串**散开**的地址而不是连续地址：连续地址的低位与总权重同周期，
/// 读数会恰好等于理想值，而一把只在特意挑过的输入上准的尺子不算尺子。
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

/// ★ ★ ★ **权重与可用性筛选的交互**：被摘掉的上游**连它的权重一起出局**。
///
/// ⚠ 这一条是「先筛可用集、再按权重挑」那个顺序的**全部意义**：
/// 反过来的话，「按 3:1 分而那个 3 没在跑」会让 3/4 的请求落空 ——
/// 而现场只看得到「一部分请求 502」，配置、健康检查、权重全都读起来正常。
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
    // 摘之前：3:1:1（总权重 5）。★ 先证明这一半成立，否则下面的对比说明不了什么。
    assert_eq!(count(t), [60, 20, 20]);
    // 把**权重最大的那个**判死。
    t.upstreams[0].set_healthy(false);
    // 之后：总权重是 2 而不是 5 ⇒ 剩下两个严格 1:1，**一次都不落空**。
    assert_eq!(count(t), [0, 50, 50]);
}

/// `least_conn` 认权重：比的是 `inflight / weight`。
#[test]
fn least_conn_比的是_inflight_除以权重() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    lb_policy least_conn\n    weight 10.0.0.2:2 3\n  }\n}\n",
    );
    let h = no_headers();
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    // 两边都在飞 3 条：3/1 = 3 对 3/3 = 1 ⇒ 取权重大的那个。
    for _ in 0..3 {
        t.upstreams[0].acquire();
        t.upstreams[1].acquire();
    }
    assert_eq!(t.pick_index_by(None), Some(1));
    // 比值相等（1/1 与 3/3）⇒ 平票，取**下标最小**。
    t.upstreams[0].release();
    t.upstreams[0].release();
    assert_eq!(
        (t.upstreams[0].inflight(), t.upstreams[1].inflight()),
        (1, 3)
    );
    assert_eq!(t.pick_index_by(None), Some(0));
}

/// ★ ★ `least_conn` 的比较**不许走浮点**。
///
/// ⚠ 判据挑的是一对 `f64` **分不开、而整数分得开**的数：`2⁵³` 与 `2⁵³ + 1`
/// 在 `f64` 里是同一个数 ⇒ 一个改用浮点的实现会在这里判反。
#[test]
fn least_conn_的比较不许走浮点() {
    let a = 1usize << 53;
    assert!(
        less_loaded(a, 1, a + 1, 1),
        "2⁵³ 比 2⁵³+1 更空 —— 整数比得出来，浮点比不出来"
    );
    assert!(!less_loaded(a + 1, 1, a, 1));
    // 反证这一对数**确实**能咬到浮点：它们在 f64 里连位模式都一模一样
    //   ⇒ 任何走浮点的比较都分不开它们，于是上面那条断言会判反。
    assert_eq!(
        (a as f64).to_bits(),
        ((a + 1) as f64).to_bits(),
        "这一对数在 f64 里应当是同一个数，否则这条判据钉不住任何东西"
    );
    // 乘上权重也不许溢出：`inflight` 是 usize，乘 65535 会把 u64 撑爆
    // （debug 下 panic、release 下静默回绕成「这个上游永远被选中」）。
    assert!(less_loaded(usize::MAX - 1, 65_535, usize::MAX, 65_535));
}

// ── 构建期校验：DSL 层查不到、而结构化层是公开入口 ──────────────────────────

/// ⚠ 权重的值域在 DSL 那边由 `FUL-DSL-0040` 挡、在 JSON 那边由 `UpstreamSpec`
/// 的反序列化挡，而一份**在进程里手搓**的结构化配置两道都不经过（G11）。
/// ★ 权重 0 的表现不是报错而是**这台机器永远不被选中**，静默。
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

// ── 监听端口 ────────────────────────────────────────────────────────────────

// ── 上游地址解析（批 10）──────────────────────────────────────────────────
//
// ⚠ 这一组守的是一条**实测出来的真缺陷**：改之前，域名上游是在每个请求上由
//   `HttpPeer::new` 阻塞解析一次，失败还 panic（每请求一次）。
//   见 `fulcrum_runtime::Upstream` 与 `fulcrum_server::dns` 的类型/模块文档。

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
    // ★ 而且 `resolve_upstreams` 根本不该为它去查 DNS。
    let rep = fulcrum_runtime::resolve_upstreams(&r);
    assert_eq!(rep.queried, 0, "IP 字面量不该走 DNS：{rep:?}");
}

#[test]
fn 域名上游在解析之前拿不到地址而且会被跳过() {
    // ★ ★ **这是「validate 必须离线」那条约束的直接后果**：
    //   `Runtime::build` 不做 DNS，所以域名上游一开始就是「没有地址」。
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
    // ⚠ 判据是 **None**，而不是「随便给一个」。改之前这里给出的是那个域名，
    //   而数据面拿它去 `HttpPeer::new` 就 panic 了。
    assert!(
        t.pick(&ctx).is_none(),
        "解析不出地址的上游必须被跳过（调用方回 502）"
    );
}

#[test]
fn 一个解析出来了就只选它() {
    // ⚠ 反向那一半：只验「全都解析不出来时返回 None」的话，
    //   一个「永远返回 None」的实现照样绿 —— 而那会让所有反代全挂。
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
    // 让第二个「解析出来」。
    t.upstreams[1].set_resolved(vec!["10.9.9.9:80".parse().unwrap()]);
    for _ in 0..5 {
        assert_eq!(
            t.pick(&ctx).unwrap().addr,
            "h2.invalid:80",
            "只有它可用，轮询也只能一直选它"
        );
    }
    // ★ 再让它掉出去，又该没得选了 —— 两个方向都要能动。
    t.upstreams[1].set_resolved(Vec::new());
    assert!(t.pick(&ctx).is_none());
}

#[test]
fn 容器里面的上游也要被解析扫到() {
    // ⚠ ⚠ 这条守的是一个**具体的盲区**：只扫站点顶层的话，写在 `handle` /
    //   `route` / `handle_errors` 里的域名上游**永远不会被解析** ——
    //   而它的症状是「那条路由一直 502」，与 DNS 看起来毫无关系。
    //   ★ 同一个盲区在 `unwired_in_use` 上真的出现过一次（只扫站点不扫全局选项）。
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

// ── 主动健康检查（批 11）─────────────────────────────────────────────────────

#[test]
fn 状态码模式两种写法都认得而且不多认() {
    use fulcrum_runtime::StatusPattern as P;
    assert_eq!(P::parse("200"), Some(P::Exact(200)));
    assert_eq!(P::parse("2xx"), Some(P::Family(2)));
    assert!(P::parse("2xx").unwrap().matches(204));
    assert!(!P::parse("2xx").unwrap().matches(304));
    assert!(P::parse("200").unwrap().matches(200));
    assert!(!P::parse("200").unwrap().matches(201));
    // ★ 反向那一半：不认识的写法必须回 None，而不是悄悄当成 2xx。
    //   ⚠ 一个「认不出就按 2xx 判」的实现，会把配了 `health_status 5xx`
    //   的那一组上游**全部判死**，而配置里一个字都没错。
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
    // ★ 还没探过 = 立刻探。⚠ 反过来（等一个周期再开始）会让
    //   `health_interval 30s` 的目标在启动后半分钟内完全没有保护，
    //   而那恰是刚换代、最可能有上游没起来的时候。
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
    // 写了：策略落地，而且 `health_*` 的值都跟着进来。
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 {\n    health_uri /h\n    health_interval 7s\n    health_timeout 2s\n    health_status 3xx\n  }\n}\n",
    );
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    let p = t.health.as_ref().expect("配了 health_uri 就该有策略");
    assert_eq!(p.uri, "/h");
    assert_eq!(p.interval, std::time::Duration::from_secs(7));
    assert_eq!(p.timeout, std::time::Duration::from_secs(2));
    assert_eq!(p.status, fulcrum_runtime::StatusPattern::Family(3));

    // ★ 没写 `health_uri`，即便写了别的 `health_*` 也**不探测**。
    //   ⚠ 反过来（有默认路径就去探）会去打一个用户从没说过的路径，
    //     而那条路径在很多后端上是 404 ⇒ 全部上游被判死。
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 {\n    health_interval 7s\n  }\n}\n");
    let t = proxy_of(&r, &Req::new("a.com", "/"), &h);
    assert!(t.health.is_none(), "没写 health_uri 就不该有健康检查策略");
}

#[test]
fn 认不出的_health_status_是装载期错误不是回落成默认() {
    // ★ 走**结构化层**进来（G11：那是公开入口，一份手写 JSON 可以带任何字符串进来），
    //   所以这里不经 DSL，而是直接把编译好的产物改掉再装载。
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
    // 先自证：原样是建得起来的。⚠ 少了这一步，一个「永远失败」的实现也全绿。
    assert!(Runtime::build(&cfg).is_ok(), "原样应当能建起来");
    let fulcrum_config::model::StepBody::ReverseProxy { health, .. } =
        &mut cfg.sites[0].chain[0].body
    else {
        panic!("第一条应当是 reverse_proxy")
    };
    health.status = "okay".to_string();
    let err = Runtime::build(&cfg).expect_err("`health_status: okay` 必须让装载失败");
    let msg = format!("{err:?}");
    // ⚠ 判据不是「失败了」而是「因为它失败的」：一个把任何配置都拒掉的实现
    //   同样会 `expect_err` 通过。
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
    // ★ 初值健康：探测还没跑过时**两个都能被选中**。
    //   ⚠ 反过来（未经证实即不可用）会让进程刚起来到第一次探测之间全站 502。
    assert!(t.upstreams.iter().all(|u| u.is_healthy()));

    // 摘掉第一个 —— 轮询必须一直落在第二个上。
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
    // 两个都摘掉 —— 一个可用的都没有，调用方回 502。
    t.upstreams[1].set_healthy(false);
    assert!(t.pick(&ctx).is_none());
    // ★ 恢复那一半：只测「摘得掉」的话，一个永不恢复的实现照样绿，
    //   而那意味着上游修好之后**永远回不来**。
    t.upstreams[0].set_healthy(true);
    for _ in 0..5 {
        assert_eq!(t.pick(&ctx).unwrap().addr, "10.0.0.1:1");
    }
}

#[test]
fn 健康但解析不出地址的照样被跳过() {
    // ★ 两条筛子是**并列**的，不是「有了健康检查就不看 DNS 了」。
    //   ⚠ 少了这一条，一个 DNS 挂了但上一轮探测判过「健康」的上游会被选中，
    //     然后在建连接那一步回 502 —— 而 `pick()` 本来就该跳过它。
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
    // 第一次两边都要探（还没探过）。
    assert!(fast.take_probe_slot(t0));
    assert!(slow.take_probe_slot(t0));
    // ★ ★ 2 秒之后：快的那条到期了，慢的那条**没有**。
    //   ⚠ 这就是「不像 dns_refresh 那样全库取最小值」的全部理由 ——
    //     刷得更勤在这里是**打在别人服务上的流量**，不是多几次 getaddrinfo。
    let t1 = t0 + Duration::from_secs(2);
    assert!(fast.take_probe_slot(t1), "1s 的那条该探了");
    assert!(!slow.take_probe_slot(t1), "60s 的那条不该被顺带探一次");
    // ★ 「判断」与「记时刻」是一个动作：同一时刻再要一次名额，要不到。
    assert!(!fast.take_probe_slot(t1), "名额已经领过了");
    // 没配 health_uri 的目标永远领不到名额。
    let r2 = rt("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n");
    let none = proxy_of(&r2, &Req::new("a.com", "/"), &h);
    assert!(!none.take_probe_slot(t0));
}

#[test]
fn 容器里面的目标也要被扫到() {
    // ⚠ 与 `all_upstreams` 同一个盲区：漏掉容器里的目标，它会**永远不被探测**，
    //   而症状只是「它一直健康」——看起来完全正常。
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

// ── M2 批 M：`metrics` 终结指令（G116）───────────────────────────────────────

#[test]
fn metrics终结在自己的表位上() {
    // ★ 两件事一起验，因为它们只有一起成立才有意义：
    //   ① `metrics` 真的终结（`Outcome::Metrics`）；
    //   ② 圈不住的人落到兜底 `respond 403` 上，**而这不是靠书写顺序** ——
    //      夹具里 `respond` 本来就写在 `handle` 后面。管事的是顺序表：
    //      `handle` 占第 **55** 位，比 `respond` 的 70 早 ⇒ 命中 `@i` 的请求
    //      在 `handle` 那一步就终结在块内的 `metrics` 上；圈外的请求没有分支命中，
    //      `handle` 什么都不产出，才轮到 70 的 `respond`。
    //   ⚠ ⚠ 所以 `metrics @internal` 与兜底 `respond 403` **并排写在站点块顶层是错的**：
    //      `metrics` 自己排 75，403 在第 70 步就先终结了，`metrics` 一次也跑不到。
    //      ⇒ 示例里那个 `handle` 不是排版习惯，是唯一正确的写法；写错时
    //      `FUL-DSL-0028` 会说出来（口径在 `docs/architecture/dsl-reference.md`）。
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

// ── M2 批 N 任务 2.8：覆盖层键的歧义（裁决 R6 ⇒ G125）───────────────────────
//
// ★ ★ ★ 判定住在 **`fulcrum-runtime`**，因为键的第三格取的是
// `normalize_upstream` **之后**的串。⇒ 下面第一条（`backend` vs `backend:80`）
// 是这一组的核心：把判定写在配置层拿原文 token 比一遍的话，**它会静静地漏过去**，
// 而所有别的判据照样绿 —— 那就是一道假门。

/// 一个站点，两条 `reverse_proxy`（各在一个 `handle` 里），上游与 `id` 都由调用方给。
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

#[test]
fn 核心判据_归一化之后撞了就装不上_哪怕两行原文长得不一样() {
    // ★ ★ ★ 本任务的核心判据。`backend` 与 `backend:80` 在配置里是**两个不同的写法**
    //   （`weight` 的地址比对就有意把它们当成两个），而运行时里它们是**同一个上游**
    //   —— 覆盖层的键因此照撞不误。
    // ⇒ 判定必须落在 `Upstream::addr`（归一化之后那个串，也就是键本身）上，
    //   落在原文 token 上的话这一条会漏，而门是绿的。
    // ⚠ ⚠ ⚠ **上游名字有意不叫 `backend`。** 那条错误消息的模板里有一句固定的
    //   举例「配置里写 `backend` 时运行时管它叫 `backend:80`」——
    //   夹具跟着叫 `backend` 的话，下面 ①②两条断言会被**模板里那句常量**满足，
    //   于是「报文真的印出了这一份配置里的地址」这件事根本没有被验到。
    //   ★ 实测过：把 `raw_addr` 从报文里整个拿掉，那一版断言**照样全绿**。
    //   ⇒ 夹具的名字必须与报文模板里出现的任何字面量都不同。
    let e = build_errors(&两条("myhost", None, "myhost:80", None));
    assert!(
        !e.is_empty(),
        "`myhost` 与 `myhost:80` 归一化后是同一个上游，两条都没写 id ⇒ 必须拒绝装载"
    );
    let 全文 = e.join("\n");
    // ① 归一化之后那个串（= 键）要印出来。
    assert!(
        全文.contains("myhost:80"),
        "报文里没有归一化后的地址：{全文}"
    );
    // ② ★ **原文也要印出来**：用户手上是配置文件，只说 `myhost:80` 的话
    //    他拿去搜写着 `myhost` 的那一行是搜不到的（R6 ⑤ 的「边界」）。
    //    ⚠ 反引号一起比 —— 只比 `myhost` 的话，`myhost:80` 里就有它，这条断言会空转。
    assert!(全文.contains("`myhost`"), "报文里没有配置原文：{全文}");
    // ③ ★ 直说怎么修。⚠ 不比「id」两个字：那在这条报文里几乎不可能不出现，
    //    比它等于什么都没比。比的是**那句修法**本身。
    assert!(
        全文.contains("给其中一条"),
        "报文没说「给其中一条写 id」：{全文}"
    );
    // ④ 说清是哪个站点（这一格来自 `BuildError::at`，模板里没有它）。
    assert!(全文.contains("a.com"), "报文没点名站点：{全文}");
}

#[test]
fn 同一站点两条指着同一个上游而都没写_id_装不上() {
    // 主场景：两条原文就一模一样。
    let e = build_errors(&两条("10.0.0.1:8080", None, "10.0.0.1:8080", None));
    assert!(!e.is_empty(), "主场景必须被拒绝");
    assert!(
        e.join("\n").contains("都没写"),
        "两条都没写 id 时要说出来，别印一个空的 id：{e:?}"
    );
}

#[test]
fn 给其中一条写了_id_就装得上() {
    // ★ ★ **反向判据**：少了它，一个「同一个上游出现两次就拒绝」的实现照样能让
    //   上面两条绿 —— 而那样的实现把 R6 的全部价值（给一条写 id 就能分开）废掉了。
    let e = build_errors(&两条(
        "10.0.0.1:8080",
        None,
        "10.0.0.1:8080",
        Some("pool_web"),
    ));
    assert!(e.is_empty(), "写了 id 就分得开，不该拒绝：{e:?}");
    // 两条都写、且写的不一样，也分得开。
    let e = build_errors(&两条(
        "10.0.0.1:8080",
        Some("a"),
        "10.0.0.1:8080",
        Some("b"),
    ));
    assert!(e.is_empty(), "两个不同的 id 分得开：{e:?}");
}

#[test]
fn 两条写了同一个_id_照样撞() {
    // ★ 反向的反向：id 不是「写了就放行」的标记，它是键里的一格。
    //   ⚠ 少了这一条，一个「`id` 非空就跳过检查」的实现能让上面每一条都绿。
    let e = build_errors(&两条(
        "10.0.0.1:8080",
        Some("same"),
        "10.0.0.1:8080",
        Some("same"),
    ));
    assert!(!e.is_empty(), "同一个 id 指着同一台机器仍然是歧义");
    assert!(
        e.join("\n").contains("`same`"),
        "报文要把撞了的那个 id 印出来：{e:?}"
    );
}

#[test]
fn 两条指着不同的上游而都没写_id_装得上() {
    // ★ ★ **反向判据**：少了它，一条「把 id 变成必填」的实现照样能让主场景绿 ——
    //   而 owner 拍的是**选填**，现有配置一个字节都不用改。
    let e = build_errors(&两条("10.0.0.1:8080", None, "10.0.0.2:8080", None));
    assert!(e.is_empty(), "上游不同就不撞，不该拒绝：{e:?}");
}

#[test]
fn 不同站点里撞了不算撞() {
    // 键的第一格是站点名（`SiteRt::name`，裁决 R6 ④）⇒ 跨站点不比。
    let e = build_errors(
        "a.com {\n  reverse_proxy 10.0.0.1:8080\n}\nb.com {\n  reverse_proxy 10.0.0.1:8080\n}\n",
    );
    assert!(e.is_empty(), "两个站点各自指着同一台机器不是歧义：{e:?}");
}

#[test]
fn 同一条_reverse_proxy_里把同一个地址写两遍不算歧义() {
    // ★ 边界（`proxy_key_conflicts` 的类型文档写着）：那两个 `Upstream` 是**同一条**
    //   `reverse_proxy` 上的同一台机器，一次 `disable` 把它们一起摘掉正是要的语义；
    //   而「给其中一条写 `id`」在那种情形下根本无从下手 —— 报一条修不了的错更坏。
    let e = build_errors("a.com {\n  reverse_proxy 10.0.0.1:8080 10.0.0.1:8080\n}\n");
    assert!(e.is_empty(), "同一条里写两遍不是 R6 要挡的那件事：{e:?}");
}

#[test]
fn 藏在容器里的_reverse_proxy_也有键也会被比到() {
    // ⚠ ⚠ 走法漏掉容器的表现是**两件事一起静默**：那条 `reverse_proxy` 没有键
    //   （覆盖指不到它），而歧义检查也看不见它。
    //   ⇒ 这里把两条分别塞进 `route` 与 `handle_errors`。
    let e = build_errors(
        "a.com {\n  route {\n    reverse_proxy 10.0.0.1:8080\n  }\n  \
         handle_errors {\n    reverse_proxy 10.0.0.1:8080\n  }\n}\n",
    );
    assert!(!e.is_empty(), "容器里的两条撞了也必须拒绝");
}

#[test]
fn 键的三格都在_keyed_proxies_上取得到() {
    // ★ 任务 3 的覆盖层与任务 6 的 `/stats` 都从这里取键 —— ⛔ 别再拼第二份。
    let r = rt(
        "a.com {\n  handle /api/* {\n    reverse_proxy backend {\n      id pool_web\n    }\n  }\n  \
         handle /web/* {\n    reverse_proxy 10.0.0.2:8080\n  }\n}\n",
    );
    let ps = r.keyed_proxies();
    assert_eq!(ps.len(), 2, "两条 reverse_proxy 都要在：{ps:?}");
    // 第一格：站点名 = `SiteRt::name`（第一条地址的原文），与访问日志同一口径。
    assert_eq!(ps[0].site, r.sites()[0].name.as_str());
    assert_eq!(ps[0].site, "a.com");
    // 第二格：写了的取原样，没写的是**空串**（不是 `None`，见 `ProxyTarget::id`）。
    assert_eq!(ps[0].id, "pool_web");
    assert_eq!(ps[1].id, "");
    // 第三格：**归一化之后**的上游地址；原文另存一格，只给报文用。
    assert_eq!(ps[0].target.upstreams[0].addr, "backend:80");
    assert_eq!(ps[0].target.upstreams[0].raw_addr, "backend");
    // ★ 走法与 `all_proxy_targets` 是**同一份实现**（任务 2.8 把那边的第三份遍历删了）。
    assert_eq!(r.all_proxy_targets().len(), ps.len());
}

#[test]
fn 没写_id_的那条在运行时是空串而不是别的什么() {
    // ⚠ 空串是键里正正经经的一格。改成「`None` 就不查」会让主场景静静漏过去。
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:8080\n}\n");
    assert_eq!(r.all_proxy_targets()[0].id, "");
}
