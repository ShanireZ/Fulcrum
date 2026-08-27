//! `log { headers … }` / `resp_headers` 的白名单，在**运行时图**这一层（**M2 批 L 第 ③ 步**）。
//!
//! ★ ★ ★ **为什么这一层要有自己的判据，而不是「DSL 那道拦住了就行」**：
//! 运行时图是**公开入口**（G11）—— `POST /load` 收的是结构化配置 JSON，
//! 它可以是任何人生成的，**不必经过 `fulcrum compile`**。
//! ⇒ 编译期那道 `FUL-DSL-0036` 在那条路上一次都不会跑。
//!
//! ⚠ ⚠ 而这一格比 `output` 那三条更要紧：`output` 写错的后果是配置装不上（吵闹），
//! **这一条写错的后果是凭据静静地进了日志**。

use fulcrum_config::compile_str;
use fulcrum_runtime::Runtime;

/// DSL → 结构化配置。★ 这里**故意不建运行时图**：下面几条要先改一改那份结构化配置，
/// 而「改完再建」正是模拟 `POST /load` 收到一份没经过 DSL 的配置。
fn cfg(dsl: &str) -> fulcrum_config::model::StructuredConfig {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(
        !o.diagnostics.has_errors(),
        "DSL 编译不过：\n{}",
        o.render_diagnostics()
    );
    o.config.expect("没有错误就该有产物")
}

fn errors_of(cfg: &fulcrum_config::model::StructuredConfig) -> String {
    match Runtime::build(cfg) {
        Ok(_) => String::new(),
        Err(e) => e
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

const SRC: &str = "http://a.com {\n  log {\n    output stderr\n  }\n  respond 200\n}\n";

#[test]
fn 白名单在装载时就算成最终形态() {
    let rt = Runtime::build(&cfg(
        "http://a.com {\n  log {\n    output stderr\n    headers User-Agent X-Request-Id\n    \
         resp_headers Content-Type\n  }\n  respond 200\n}\n",
    ))
    .expect("应当建得起来");
    let log = rt.sites()[0].log.as_ref().expect("log 该落地");
    // ★ ★ 两个字段是**两件事**：查哪个头（小写、带连字符）与日志里那一格叫什么
    //   （小写、下划线、带前缀）。⚠ 断言两个，是因为只断言一个的话，
    //   一个把 `lookup` 也换成下划线的实现会通过 —— 而它永远查不到任何头。
    assert_eq!(log.req_headers[0].lookup, "user-agent");
    assert_eq!(log.req_headers[0].key, "req_hdr_user_agent");
    assert_eq!(log.req_headers[1].lookup, "x-request-id");
    assert_eq!(log.req_headers[1].key, "req_hdr_x_request_id");
    assert_eq!(log.resp_headers[0].lookup, "content-type");
    assert_eq!(log.resp_headers[0].key, "resp_hdr_content_type");
    // ★ 顺序 = 配置里写的顺序，不是字典序（日志那一行按它排）。
    assert_eq!(log.req_headers.len(), 2);
}

#[test]
fn 默认一个头都不记() {
    // ⚠ ⚠ 这是契约里写死的默认值，而它**只能反向证**：
    //   一个「不看配置、见头就记」的实现在上面那条里是全绿的。
    let rt = Runtime::build(&cfg(SRC)).expect("应当建得起来");
    let log = rt.sites()[0].log.as_ref().expect("log 该落地");
    assert!(log.req_headers.is_empty(), "默认不该记任何请求头");
    assert!(log.resp_headers.is_empty(), "默认不该记任何响应头");
}

#[test]
fn 结构化那条路上敏感头也被拦住() {
    // ★ 这里改的是**编译之后**的那份结构化配置 —— 编译期那道门已经跑完了，
    //   于是这一条量的确实是运行时图自己那道。
    for name in [
        "Authorization",
        "cookie",
        "Set-Cookie",
        "PROXY-AUTHORIZATION",
    ] {
        for which in ["headers", "resp_headers"] {
            let mut c = cfg(SRC);
            let log = c.sites[0].log.as_mut().expect("log 该落地");
            if which == "headers" {
                log.headers = vec![name.to_string()];
            } else {
                log.resp_headers = vec![name.to_string()];
            }
            let msg = errors_of(&c);
            assert!(
                msg.contains(name),
                "结构化那条路上 `{which} {name}` 没被拦住（错误：{msg:?}）"
            );
        }
    }
}

#[test]
fn 一个普通头名不许被当成敏感头() {
    // ★ ★ 反证。⚠ 少了它，一条「见 `headers` 非空就报错」的实现会让上面那条全绿 ——
    //   **「挡住坏的」与「别连好的一起挡掉」要各自一条判据**。
    let mut c = cfg(SRC);
    c.sites[0].log.as_mut().unwrap().headers = vec!["X-Forwarded-For".into()];
    assert_eq!(errors_of(&c), "", "一个普通头名不该让配置装不上");
}

#[test]
fn 同一个头写两遍只留一份() {
    // ⚠ 两条同名的日志键会让那一行 JSON 里出现两个一样的 key，
    //   而**哪一个会被解析器留下没有定义**。★ 大小写不同也是同一个头。
    let mut c = cfg(SRC);
    c.sites[0].log.as_mut().unwrap().headers = vec![
        "User-Agent".into(),
        "user-agent".into(),
        "USER-AGENT".into(),
    ];
    let rt = Runtime::build(&c).expect("应当建得起来");
    let log = rt.sites()[0].log.as_ref().unwrap();
    assert_eq!(log.req_headers.len(), 1, "同一个头只该留一份：{log:?}");
}

#[test]
fn 不是合法头名的东西是错误_而不是静静地什么都不记() {
    // ★ ★ ★ 「查不到」在这份契约里与「这条请求上没有这个头」**长得一模一样** ——
    //   ⇒ 一个拼错的名字会静静地什么都不记，而运维看到的是「我配了它怎么没有」。
    for bad in ["User Agent", "x:y", "", "头"] {
        let mut c = cfg(SRC);
        c.sites[0].log.as_mut().unwrap().headers = vec![bad.to_string()];
        assert!(
            !errors_of(&c).is_empty(),
            "`{bad}` 不是合法 HTTP 头名，应当装不上"
        );
    }
}
