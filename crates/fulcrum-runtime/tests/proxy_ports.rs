//! G163：哪些端口算「有反代」—— 读 h1 请求头之前按端口定要不要保留头名的原样大小写。
//!
//! ★ 两个方向都判：有反代的端口必须算进去（漏了 ⇒ 转发给上游的头名变样），
//!   没有反代的端口必须不算（多算 ⇒ 只是白建一张表，线上看不出来 —— 所以更要在这里钉住）。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, SharedRuntime};
use std::collections::BTreeSet;
use std::sync::Arc;

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

/// 8001：同一端口上一个站点 respond、另一个站点在嵌套的 `handle` 里反代（精确表）·
/// 8002：通配站点在 `route` 里反代（通配表）· 8003：端口兜底站点直接反代（兜底表）·
/// 8004 / 8005：只有 respond（精确表与兜底表各一个反向）。
const DSL: &str = "\
http://a.example:8001 {
  respond 200 a
}
http://b.example:8001 {
  handle /api/* {
    reverse_proxy 127.0.0.1:9
  }
  respond 200 b
}
http://*.wild.example:8002 {
  route {
    reverse_proxy 127.0.0.1:9
  }
}
:8003 {
  reverse_proxy 127.0.0.1:9
}
http://c.example:8004 {
  respond 200 c
}
:8005 {
  respond 200 d
}
";

#[test]
fn 三张表与嵌套里的反代都算_只有respond的端口不算() {
    assert_eq!(rt(DSL).proxy_ports(), BTreeSet::from([8001, 8002, 8003]));
}

/// 位图的两端（第 0 个字的最低位、第 1023 个字的最高位）与换代：`swap` 之后照新配置答。
#[test]
fn shared_runtime_按端口查_换代之后跟着新配置() {
    let shared = SharedRuntime::new(Arc::new(rt(DSL)));
    for (port, want) in [
        (8001, true),
        (8002, true),
        (8003, true),
        (8004, false),
        (8005, false),
        (0, false),
        (9, false),
        (65535, false),
    ] {
        assert_eq!(shared.port_has_proxy(port), want, "端口 {port}");
    }
    shared.swap(rt("\
http://c.example:8004 {
  reverse_proxy 127.0.0.1:9
}
:65535 {
  reverse_proxy 127.0.0.1:9
}
:8001 {
  respond 200 now-respond-only
}
"));
    for (port, want) in [
        (8001, false),
        (8002, false),
        (8003, false),
        (8004, true),
        (65535, true),
        (0, false),
    ] {
        assert_eq!(shared.port_has_proxy(port), want, "换代之后端口 {port}");
    }
}
