//! G163：哪些連接埠算「有反代」—— 讀 h1 請求頭之前按連接埠定要不要保留頭名的原樣大小寫。
//!
//! ★ 兩個方向都判：有反代的連接埠必須算進去（漏了 ⇒ 轉發給上游的頭名變樣），
//!   沒有反代的連接埠必須不算（多算 ⇒ 只是白建一張表，線上看不出來 —— 所以更要在這裡釘住）。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, SharedRuntime};
use std::collections::BTreeSet;
use std::sync::Arc;

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

/// 8001：同一連接埠上一個站台 respond、另一個站台在嵌套的 `handle` 裡反代（精確表）·
/// 8002：通配站台在 `route` 裡反代（通配表）· 8003：連接埠備援站台直接反代（備援表）·
/// 8004 / 8005：只有 respond（精確表與備援表各一個反向）。
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

/// 點陣圖的兩端（第 0 個字的最低位、第 1023 個字的最高位）與換代：`swap` 之後照新設定答。
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
