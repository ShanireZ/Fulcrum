//! L4 面的**构建期**判据（M2 批 A / B / C）。
//!
//! ★ 这一层测的是「运行时图建得对不对」，端到端那半边在 `tests/l4/run.sh`：
//! 那边验「真字节真的被搬过去了、换代时连接没断」，这边验
//! 「监听地址拆得对、上游进了该进的表、错的配置在**装载期**就红」。
//! ⚠ 两边都要有：只有端到端，一条判据红了只会得到「连不上」，查起来要靠猜；
//! 只有这一层，一个把图建得完美却根本没起监听器的实现照样全绿。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, normalize_l4_upstream, parse_l4_listen};

fn build(src: &str) -> Result<Runtime, String> {
    let o = compile_str("t.Fulcrumfile", src);
    let cfg = o.config.as_ref().ok_or_else(|| o.render_diagnostics())?;
    Runtime::build(cfg).map_err(|errs| {
        errs.iter()
            .map(|e| format!("{e}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[test]
fn 监听地址的两种写法都认() {
    assert_eq!(parse_l4_listen(":3306"), Ok((None, 3306)));
    assert_eq!(
        parse_l4_listen("127.0.0.1:3306"),
        Ok((Some("127.0.0.1".to_string()), 3306))
    );
}

/// ⚠ **端口必须写**：L4 上没有「默认端口」这回事 —— HTTP 那边缺端口能按 scheme
/// 补 80/443，而一个裸 TCP 监听器没有 scheme。
/// ★ 判据钉的是「报错」，不是「悄悄补一个」：后者会让 `l4 { tcp :  { … } }` 这种笔误
/// 变成一个**监听在别处**的服务，而配置读起来完全正常。
#[test]
fn 监听地址缺端口或端口非法都要红() {
    for bad in [
        "", "3306", "abc", ":0", ":70000", ":-1", "/tmp/x", "[::1]:1",
    ] {
        assert!(
            parse_l4_listen(bad).is_err(),
            "`{bad}` 应当被拒，却过了：{:?}",
            parse_l4_listen(bad)
        );
    }
}

#[test]
fn l4上游必须写明端口() {
    assert_eq!(
        normalize_l4_upstream("10.0.0.5:3306"),
        Ok("10.0.0.5:3306".to_string())
    );
    assert_eq!(
        normalize_l4_upstream("db.internal:5432"),
        Ok("db.internal:5432".to_string())
    );
    // ★ 与 HTTP 那边**有意不同**的一处：那边 `backend` 会补成 `backend:80`。
    assert!(normalize_l4_upstream("db.internal").is_err());
    assert!(normalize_l4_upstream("10.0.0.5:0").is_err());
    assert!(normalize_l4_upstream("10.0.0.5:70000").is_err());
}

#[test]
fn 纯tcp的块建出监听器并且ip字面量当场解析好() {
    let rt = build("l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306 10.0.0.6:3306\n  }\n}\n").unwrap();
    assert_eq!(rt.l4_listeners.len(), 1);
    let l = &rt.l4_listeners[0];
    assert_eq!(l.listen_port, 3306);
    assert_eq!(l.listen_host, None);
    // ★ 批 C 起 `target` 是 `Option`：没有兜底是一种**合法配置**（「只服务我认得的名字」）。
    let t = l.target.as_ref().expect("这份配置写了兜底 proxy");
    assert_eq!(t.upstreams.len(), 2);
    // ★ IP 字面量在构建期就填好槽：它不需要 DNS，也永远不会变。
    //   ⚠ 而域名一律留空 —— `fulcrum validate` 必须离线就能说话。
    assert_eq!(t.upstreams[0].dial_candidates().len(), 1);
}

/// ★ ★ **L4 的上游必须出现在 `all_upstreams()` 里**，否则域名形式的 L4 上游
/// 永远不会被解析、也不会被后台重解析 —— 而现场表现是「那个端口连上就断」，
/// 配置、日志、健康检查全都正常。
/// ⚠ 这条判据存在的理由与批 10 给回落后端补的那条逐字相同：
/// **那个函数是 DNS 解析唯一的输入，谁不在里面谁就不存在。**
#[test]
fn l4的域名上游进得了dns那张表() {
    let rt = build("l4 {\n  tcp :3306 {\n    proxy db.internal:5432\n  }\n}\n").unwrap();
    let names: Vec<&str> = rt.all_upstreams().iter().map(|u| u.addr.as_str()).collect();
    assert!(
        names.contains(&"db.internal:5432"),
        "L4 上游没进 all_upstreams：{names:?}"
    );
    // 域名在构建期**不解析**（validate 要离线可用）。
    let l = &rt.l4_listeners[0];
    let t = l.target.as_ref().expect("这份配置写了兜底 proxy");
    assert!(t.upstreams[0].dial_candidates().is_empty());
}

/// `udp` 也建监听器（**M2 批 B**），而装载期判据两种协议一视同仁。
///
/// ⚠ **旧契约（批 A）**：`udp` **不建**监听器（它还在 `UNWIRED` 里），
/// 但那时装载期判据就已经对它照跑 —— 理由是「等 UDP 接线那天，
/// 一份早就写错的 `udp` 块不该在那天才一次性冒出一堆错误」。
/// ★ 那个决定在这一批**兑现了**：接线时判据一条都不用补，只删掉那个 `continue`。
#[test]
fn udp也建监听器而且错的配置照样红() {
    let rt = build("l4 {\n  udp :53 {\n    proxy 10.0.0.6:53\n  }\n}\n").unwrap();
    assert_eq!(rt.l4_listeners.len(), 1, "UDP 现在也要建出监听器");
    assert_eq!(rt.l4_listeners[0].proto.as_str(), "udp");
    assert_eq!(rt.l4_listeners[0].listen_port, 53);
    // 上游写错了 ⇒ 装载期就红（哪怕它是 udp）。
    let err = build("l4 {\n  udp :53 {\n    proxy 10.0.0.6\n  }\n}\n").unwrap_err();
    assert!(err.contains("缺端口"), "错误信息没说到点子上：{err}");

    // ★ ★ **同一个端口号上 tcp 与 udp 可以并存** —— 它们是两个地址族命名空间，
    //   内核不会冲突。⚠ 一个「只按端口号查重」的实现会把这份合法配置判红，
    //   而 DNS（53/udp + 53/tcp）恰恰是最常见的 L4 用法之一。
    let both = build(
        "l4 {\n  tcp :53 {\n    proxy 10.0.0.6:53\n  }\n  udp :53 {\n    proxy 10.0.0.6:53\n  }\n}\n",
    )
    .expect("同端口的 tcp 与 udp 应当并存");
    assert_eq!(both.l4_listeners.len(), 2);
}

#[test]
fn 两个监听器抢同一个端口装载期就红() {
    let err = build(
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n  tcp :3306 {\n    proxy 10.0.0.6:3306\n  }\n}\n",
    )
    .unwrap_err();
    assert!(err.contains("已经被"), "错误信息没说到点子上：{err}");
}

/// ★ L4 端口撞上 HTTP 站点端口 ⇒ 同一个进程里两个监听器抢同一个端口，起不来。
/// ⚠ 不在装载期拦的话，现场是「服务起来了，但那个端口时好时坏」——
/// 取决于哪个监听器先 bind 成功。
#[test]
fn l4端口撞上http站点端口就红() {
    let err = build("http://a.com:8080 {\n  respond 200\n}\nl4 {\n  tcp :8080 {\n    proxy 10.0.0.5:3306\n  }\n}\n")
        .unwrap_err();
    assert!(err.contains("HTTP 站点"), "错误信息没说到点子上：{err}");
}

/// 一个上游都没有的 `tcp` 块 ⇒ 红。
/// ⚠ DSL 层已经会报一次（`至少要有一条 proxy`），这里守的是**结构化入口**那一半：
/// 机器直接写结构化配置时不经过 DSL 前端（G11），而一个没有上游的监听器
/// 会接受连接然后立刻关掉 —— 现场表现与「上游全挂」一模一样。
#[test]
fn 没有上游的tcp块在结构化层也要红() {
    // ★ 从一份合法配置出发，**在结构化那一层**把上游抹掉 ——
    //   这正是机器直接写结构化配置时可能交出来的东西，而它不经过 DSL 前端。
    let o = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n}\n",
    );
    let mut cfg = o.config.expect("这份 DSL 应当编得过");
    cfg.l4
        .as_mut()
        .expect("应当有 l4")
        .listeners
        .get_mut(0)
        .expect("应当有一个监听器")
        .upstreams
        .clear();
    let err = Runtime::build(&cfg).unwrap_err();
    assert!(
        err.iter()
            .any(|e| format!("{e}").contains("一个可用的上游都没有")),
        "错误信息没说到点子上：{err:?}"
    );
}
// ══════════════════════════════════════════════════════════════════════════
// SNI / ALPN 分流（M2 批 C）
// ══════════════════════════════════════════════════════════════════════════

const SNI_DSL: &str = "l4 {\n  tcp :443 {\n    sni api.example.com {\n      proxy 10.0.0.1:8443\n    }\n    sni *.internal.example.com {\n      proxy 10.0.0.2:8443\n    }\n    alpn h2 {\n      proxy 10.0.0.3:8443\n    }\n    proxy 10.0.0.9:8443\n  }\n}\n";

#[test]
fn 规则建得出来而且顺序就是书写顺序() {
    let rt = build(SNI_DSL).unwrap();
    let l = &rt.l4_listeners[0];
    assert_eq!(l.rules.len(), 3);
    // ★ 顺序即语义：DSL 里怎么写，这里就怎么排 —— 数据面按这个顺序取第一个命中。
    assert_eq!(l.rules[0].values, vec!["api.example.com"]);
    assert_eq!(l.rules[1].values, vec!["*.internal.example.com"]);
    assert_eq!(l.rules[2].values, vec!["h2"]);
    assert!(l.target.is_some(), "写了裸 proxy 就该有兜底");
}

/// SNI 的匹配语义：精确、大小写不敏感、通配**只吃一层**。
///
/// ★ ★ 最后一条是 **G66 / D18** 那条纪律的第三个落点：站点索引、证书解析、
/// 现在是 L4 分流 —— 三处**共用同一份实现**（`wildcard_covers`）。
/// ⚠ 另写一份「差不多的」匹配，就是把 D18 那个洞在第三个地方再挖一次。
#[test]
fn sni_精确与通配的语义与站点索引一致() {
    let rt = build(SNI_DSL).unwrap();
    let l = &rt.l4_listeners[0];
    let exact = &l.rules[0];
    let wild = &l.rules[1];

    assert!(exact.matches(Some("api.example.com"), &[]));
    // ⚠ DNS 名不区分大小写 —— 一个只会 `==` 的实现在 `API.example.com` 上会漏。
    assert!(exact.matches(Some("API.Example.COM"), &[]));
    assert!(!exact.matches(Some("other.example.com"), &[]));
    assert!(!exact.matches(None, &[]), "没带 SNI 不该命中任何 sni 规则");

    assert!(wild.matches(Some("a.internal.example.com"), &[]));
    assert!(
        !wild.matches(Some("a.b.internal.example.com"), &[]),
        "通配只吃一层（G66）"
    );
    assert!(
        !wild.matches(Some("internal.example.com"), &[]),
        "裸域不该被 `*.` 覆盖"
    );
}

/// ALPN 是**逐字节相等**，不是前缀。
///
/// ⚠ `h2` 与 `h2c` 是两个不同的协议标识；前缀匹配会把明文 h2c 的连接
/// 送到只会 h2-over-TLS 的后端去，而现场表现是「偶尔握手失败」。
#[test]
fn alpn_逐字节相等而不是前缀() {
    let rt = build(SNI_DSL).unwrap();
    let alpn_rule = &rt.l4_listeners[0].rules[2];
    assert!(alpn_rule.matches(None, &[b"h2".to_vec()]));
    assert!(
        alpn_rule.matches(None, &[b"http/1.1".to_vec(), b"h2".to_vec()]),
        "清单里有就算命中"
    );
    assert!(!alpn_rule.matches(None, &[b"h2c".to_vec()]), "h2c 不是 h2");
    assert!(!alpn_rule.matches(None, &[b"http/1.1".to_vec()]));
    assert!(
        !alpn_rule.matches(Some("api.example.com"), &[]),
        "alpn 规则不看 SNI"
    );
}

/// 分流规则里的上游**也要进 DNS 那张表**。
///
/// ⚠ 少了这一格，一个域名形式的 `sni` 上游永远解析不出地址 ——
/// 而现场是「那个名字连上就断」，兜底那条却一切正常，看起来像 SNI 匹配错了。
#[test]
fn 规则里的域名上游也进得了dns那张表() {
    let rt = build(
        "l4 {\n  tcp :443 {\n    sni a.com {\n      proxy inner.svc:8443\n    }\n    proxy 10.0.0.9:8443\n  }\n}\n",
    )
    .unwrap();
    let names: Vec<&str> = rt.all_upstreams().iter().map(|u| u.addr.as_str()).collect();
    assert!(
        names.contains(&"inner.svc:8443"),
        "规则里的上游没进 all_upstreams：{names:?}"
    );
}

/// **只有规则、没有兜底**是一种合法配置（「只服务我认得的那几个名字」）。
/// ⚠ 而**两者都没有**不是：那样的监听器接受连接之后只能立刻关掉。
#[test]
fn 没有兜底合法但两者都没有不合法() {
    let ok_rt =
        build("l4 {\n  tcp :443 {\n    sni a.com {\n      proxy 10.0.0.1:8443\n    }\n  }\n}\n")
            .expect("只有规则、没有兜底 —— 合法");
    assert!(ok_rt.l4_listeners[0].target.is_none());
    assert_eq!(ok_rt.l4_listeners[0].rules.len(), 1);

    // 结构化入口那一半（机器直接写这一层，不经过 DSL 前端）。
    let o = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  tcp :443 {\n    proxy 10.0.0.1:8443\n  }\n}\n",
    );
    let mut cfg = o.config.expect("这份 DSL 应当编得过");
    let l = cfg.l4.as_mut().unwrap().listeners.get_mut(0).unwrap();
    l.upstreams.clear();
    l.rules.clear();
    let err = Runtime::build(&cfg).unwrap_err();
    assert!(
        err.iter()
            .any(|e| format!("{e}").contains("一个可用的上游都没有")),
        "错误信息没说到点子上：{err:?}"
    );
}

/// `udp` 上不能按 SNI/ALPN 分流 —— 而这**不是「以后再做」，是做不到**。
///
/// ★ 判据钉在**结构化层**：DSL 那一侧已经拦过一次，而结构化配置是公开入口（G11）。
#[test]
fn udp上不能分流并且理由要说出来() {
    // DSL 那一半
    let o = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  udp :53 {\n    sni a.com {\n      proxy 10.0.0.1:53\n    }\n    proxy 10.0.0.2:53\n  }\n}\n",
    );
    assert!(o.diagnostics.has_errors(), "udp 里写 sni 应当在 DSL 层就红");
    let text = o.render_diagnostics();
    assert!(
        text.contains("ClientHello") || text.contains("加密"),
        "诊断要说出**为什么**做不到，而不只是「不支持」：{text}"
    );

    // 结构化那一半
    let o2 = compile_str(
        "t.Fulcrumfile",
        "l4 {\n  tcp :443 {\n    sni a.com {\n      proxy 10.0.0.1:8443\n    }\n  }\n}\n",
    );
    let mut cfg = o2.config.expect("这份 DSL 应当编得过");
    cfg.l4.as_mut().unwrap().listeners[0].proto = "udp".to_string();
    let err = Runtime::build(&cfg).unwrap_err();
    assert!(
        err.iter().any(|e| format!("{e}").contains("ClientHello")),
        "结构化层也要拦，且说出理由：{err:?}"
    );
}

// ── M2 批 D：PROXY protocol 的构建期判据 ────────────────────────────────────

#[test]
fn 收那侧的信任清单进了运行时图() {
    let rt = build(
        "l4 {\n  tcp :5432 {\n    proxy_protocol_from 10.0.0.0/8 192.168.1.7\n    proxy 127.0.0.1:15432\n  }\n}\n",
    )
    .unwrap();
    let l = &rt.l4_listeners[0];
    assert_eq!(l.proxy_protocol_from.len(), 2, "两条都要在");
    // ★ 判据落在**行为**（信不信这个 IP）上，不落在「清单里有几条」上：
    //   后者一个把网段解析成 /32 的实现也能过。
    assert!(l.trusts_proxy_protocol("10.9.9.9".parse().unwrap()));
    assert!(l.trusts_proxy_protocol("192.168.1.7".parse().unwrap()));
    assert!(!l.trusts_proxy_protocol("192.168.1.8".parse().unwrap()));
    assert!(!l.trusts_proxy_protocol("203.0.113.1".parse().unwrap()));
}

/// ★ ★ **这一条是整批最要紧的判据**：一份**没写** `proxy_protocol_from` 的配置
/// 必须谁都不信 —— 而不是「没配 ⇒ 不检查 ⇒ 都信」。
///
/// ⚠ 后者的现场表现是：**任何人都能自称自己是任何 IP**，而站点照常服务、
/// 日志照常有行、每一道功能门都是绿的。
#[test]
fn 没写清单就是谁都不信而不是谁都信() {
    let rt = build("l4 {\n  tcp :5432 {\n    proxy 127.0.0.1:15432\n  }\n}\n").unwrap();
    let l = &rt.l4_listeners[0];
    assert!(l.proxy_protocol_from.is_empty());
    for ip in ["127.0.0.1", "10.0.0.1", "::1", "203.0.113.1"] {
        assert!(
            !l.trusts_proxy_protocol(ip.parse().unwrap()),
            "空清单却信了 {ip}"
        );
    }
}

#[test]
fn http_面那份清单也是全局的而且默认谁都不信() {
    let rt = build("{\n  proxy_protocol_from 10.0.0.0/8\n}\nhttp://a.com {\n  respond 200\n}\n")
        .unwrap();
    assert!(rt.trusts_proxy_protocol("10.1.2.3".parse().unwrap()));
    assert!(!rt.trusts_proxy_protocol("11.1.2.3".parse().unwrap()));

    let bare = build("http://a.com {\n  respond 200\n}\n").unwrap();
    assert!(!bare.trusts_proxy_protocol("10.1.2.3".parse().unwrap()));
    assert!(!bare.trusts_proxy_protocol("127.0.0.1".parse().unwrap()));
}

/// ⚠ **v4 与 v6 不互通**（与 `remote_ip` 同一份 `Cidr`）：
/// 一条 `10.0.0.0/8` 不许命中任何 v6 客户端。
/// ★ 把 v4 映射成 `::ffff:a.b.c.d` 再比，会让这条清单在**双栈监听器**上
/// 对着 v6 客户端全线放行 —— 而 `--bind-host [::]` 正是本项目生产机上的写法。
#[test]
fn 信任清单的_v4_与_v6_不互通() {
    let rt = build(
        "l4 {\n  tcp :5432 {\n    proxy_protocol_from 10.0.0.0/8\n    proxy 127.0.0.1:15432\n  }\n}\n",
    )
    .unwrap();
    let l = &rt.l4_listeners[0];
    assert!(!l.trusts_proxy_protocol("::ffff:10.0.0.1".parse().unwrap()));
    assert!(!l.trusts_proxy_protocol("::1".parse().unwrap()));
}

#[test]
fn 发那侧的版本进了运行时图而且省略参数就是_v2() {
    use fulcrum_runtime::proxyproto::Version;
    let rt = build(
        "l4 {\n  tcp :5432 {\n    proxy_protocol\n    proxy 127.0.0.1:15432\n  }\n  tcp :5433 {\n    proxy_protocol v1\n    proxy 127.0.0.1:15433\n  }\n  tcp :5434 {\n    proxy 127.0.0.1:15434\n  }\n}\n",
    )
    .unwrap();
    assert_eq!(rt.l4_listeners[0].proxy_protocol, Some(Version::V2));
    assert_eq!(rt.l4_listeners[1].proxy_protocol, Some(Version::V1));
    // ★ 没写就是**不发**，而不是「发一个默认的」。
    assert_eq!(rt.l4_listeners[2].proxy_protocol, None);
}

/// ⚠ ⚠ **写错的网段必须在装载期红，不能留到请求路径上。**
///
/// ★ 这一条的线上事故形态尤其坏：信任清单没生效 ⇒ **客户端 IP 全都取错**，
/// 而站点照常服务、访问日志照常有行 —— 没有任何东西会说出来。
#[test]
fn 写错的网段在装载期就红() {
    for bad in ["10.0.0.0/33", "not-an-ip", "10.0.0.0/", "10.0.0.256"] {
        let src = format!(
            "l4 {{\n  tcp :5432 {{\n    proxy_protocol_from {bad}\n    proxy 127.0.0.1:15432\n  }}\n}}\n"
        );
        let e = build(&src).expect_err(&format!("`{bad}` 应当在装载期被拒"));
        assert!(
            e.contains("proxy_protocol_from"),
            "{bad} 的报错没指到位：{e}"
        );
    }
    // 全局那一份走同一个 `Cidr::parse`，也要红。
    let e = build("{\n  proxy_protocol_from 10.0.0.0/33\n}\nhttp://a.com {\n  respond 200\n}\n")
        .expect_err("全局那份也要红");
    assert!(e.contains("proxy_protocol_from"), "{e}");
}

/// ★ ★ **判据在两层都有**（与 `sni` / `alpn` 在 udp 上那条逐字同一条理由）：
/// DSL 前端拦一次，**运行时图再拦一次** —— 因为结构化配置是**公开入口**（G11），
/// 机器可以绕过 DSL 直接写它。
#[test]
fn udp_上的_proxy_protocol_两层都拦() {
    // 第一层：DSL 前端。
    for line in ["proxy_protocol_from 10.0.0.0/8", "proxy_protocol v2"] {
        let src = format!("l4 {{\n  udp :53 {{\n    {line}\n    proxy 127.0.0.1:15353\n  }}\n}}\n");
        let e = build(&src).expect_err(&format!("udp 上的 `{line}` 应当被拒"));
        assert!(e.contains("udp"), "{line} 的报错没说是 udp 的问题：{e}");
    }

    // 第二层：直接喂结构化配置，绕过 DSL。
    let json = r#"{
      "schema_version": 1,
      "global": {"acme_email":null,"acme_ca":null,"admin":null,"default_sni":null,
                 "grace_period_ms":null,"fallback_nginx":null,"fallback_caddy":null,
                 "auto_http_redirect":true,"proxy_protocol_from":[]},
      "defaults": {"no_site_match":421,"no_route_match":404,"all_upstreams_down":502},
      "sites": [],
      "l4": {"listeners":[{"proto":"udp","listen":":53","upstreams":["127.0.0.1:15353"],
             "rules":[],"proxy_protocol_from":["10.0.0.0/8"],"proxy_protocol":null}]}
    }"#;
    let cfg: fulcrum_config::model::StructuredConfig = serde_json::from_str(json).unwrap();
    let errs = Runtime::build(&cfg).expect_err("结构化那条路也必须拦住");
    let msg = errs
        .iter()
        .map(|e| format!("{e}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(msg.contains("udp"), "{msg}");
}
