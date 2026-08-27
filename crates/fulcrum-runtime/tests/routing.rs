//! 端到端：一段 DSL 进去，**路由决策**出来。
//!
//! ★ 这一层测的是「决策」，不是「字节」。中间不起监听、不发流量、不解析响应——
//! 于是一条规则错了，红的那一行就直接指着它。
//! 真流量那一层由 `tests/serve/run.sh` 单独覆盖（那里测的是**数据面把决策执行对没有**）。

use fulcrum_config::compile_str;
use fulcrum_runtime::request::{HeaderList, RequestCtx};
use fulcrum_runtime::template::Template;
use fulcrum_runtime::{LbPolicy, Outcome, Routed, Runtime, SiteMatch};
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

// ── 构建期校验：DSL 层查不到、而结构化层是公开入口 ──────────────────────────

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
