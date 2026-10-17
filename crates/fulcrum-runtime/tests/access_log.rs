//! `log { headers … }` / `resp_headers` 的白名單，在**執行環境圖**這一層（**M2 批 L 第 ③ 步**）。
//!
//! ★ ★ ★ **為什麼這一層要有自己的判據，而不是「DSL 那道攔住了就行」**：
//! 執行環境圖是**公開入口**（G11）—— `POST /load` 收的是結構化設定 JSON，
//! 它可以是任何人生成的，**不必經過 `fulcrum compile`**。
//! ⇒ 編譯期那道 `FUL-DSL-0036` 在那條路上一次都不會跑。
//!
//! ⚠ ⚠ 而這一格比 `output` 那三條更要緊：`output` 寫錯的後果是設定裝不上（吵鬧），
//! **這一條寫錯的後果是憑證靜靜地進了紀錄**。

use fulcrum_config::compile_str;
use fulcrum_runtime::Runtime;

/// DSL → 結構化設定。★ 這裡**故意不建執行環境圖**：下面幾條要先改一改那份結構化設定，
/// 而「改完再建」正是模擬 `POST /load` 收到一份沒經過 DSL 的設定。
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
    // ★ ★ 兩個欄位是**兩件事**：查哪個頭（小寫、帶連字元）與紀錄裡那一格叫什麼
    //   （小寫、下劃線、帶前綴）。⚠ 斷言兩個，是因為只斷言一個的話，
    //   一個把 `lookup` 也換成下劃線的實作會通過 —— 而它永遠查不到任何頭。
    assert_eq!(log.req_headers[0].lookup, "user-agent");
    assert_eq!(log.req_headers[0].key, "req_hdr_user_agent");
    assert_eq!(log.req_headers[1].lookup, "x-request-id");
    assert_eq!(log.req_headers[1].key, "req_hdr_x_request_id");
    assert_eq!(log.resp_headers[0].lookup, "content-type");
    assert_eq!(log.resp_headers[0].key, "resp_hdr_content_type");
    // ★ 順序 = 設定裡寫的順序，不是字典序（紀錄那一行按它排）。
    assert_eq!(log.req_headers.len(), 2);
}

#[test]
fn 默认一个头都不记() {
    // ⚠ ⚠ 這是契約裡寫死的預設值，而它**只能反向證**：
    //   一個「不看設定、見頭就記」的實作在上面那條裡是全綠的。
    let rt = Runtime::build(&cfg(SRC)).expect("应当建得起来");
    let log = rt.sites()[0].log.as_ref().expect("log 该落地");
    assert!(log.req_headers.is_empty(), "默认不该记任何请求头");
    assert!(log.resp_headers.is_empty(), "默认不该记任何响应头");
}

#[test]
fn 结构化那条路上敏感头也被拦住() {
    // ★ 這裡改的是**編譯之後**的那份結構化設定 —— 編譯期那道門已經跑完了，
    //   於是這一條量的確實是執行環境圖自己那道。
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
    // ★ ★ 反證。⚠ 少了它，一條「見 `headers` 非空就回報錯誤」的實作會讓上面那條全綠 ——
    //   **「擋住壞的」與「別連好的一起擋掉」要各自一條判據**。
    let mut c = cfg(SRC);
    c.sites[0].log.as_mut().unwrap().headers = vec!["X-Forwarded-For".into()];
    assert_eq!(errors_of(&c), "", "一个普通头名不该让配置装不上");
}

#[test]
fn 同一个头写两遍只留一份() {
    // ⚠ 兩條同名的紀錄鍵會讓那一行 JSON 裡出現兩個一樣的 key，
    //   而**哪一個會被解析器留下沒有定義**。★ 大小寫不同也是同一個頭。
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
    // ★ ★ ★ 「查不到」在這份契約裡與「這條請求上沒有這個頭」**長得一模一樣** ——
    //   ⇒ 一個拼錯的名字會靜靜地什麼都不記，而維運看到的是「我配了它怎麼沒有」。
    for bad in ["User Agent", "x:y", "", "头"] {
        let mut c = cfg(SRC);
        c.sites[0].log.as_mut().unwrap().headers = vec![bad.to_string()];
        assert!(
            !errors_of(&c).is_empty(),
            "`{bad}` 不是合法 HTTP 头名，应当装不上"
        );
    }
}
