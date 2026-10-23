//! G168：兩個全域 TLS 設定在執行環境圖上 —— 驗證過、原樣帶上。
//!
//! ★ 結構化設定是公開入口（G11）：機器直接寫的 JSON 繞過了 DSL 的 `FUL-DSL-0047`，
//!   所以「認不出的組名」必須在 `Runtime::build` 這一層也報 —— 否則症狀是**啟動時**
//!   BoringSSL 的 `set_curves_list` 失敗，或 `POST /load` 把同一個組的兩種寫法判成「改了」。

use fulcrum_config::compile_str;
use fulcrum_config::model::StructuredConfig;
use fulcrum_runtime::{Runtime, TlsPolicyRt};

/// 一份合法的最小設定（不寫那兩個鍵）。
fn base() -> StructuredConfig {
    let o = compile_str("t.Fulcrumfile", "http://a.com {\n  respond 200\n}\n");
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    o.config.unwrap()
}

fn build_err(cfg: &StructuredConfig) -> String {
    match Runtime::build(cfg) {
        Ok(_) => panic!("这份配置该建不起来"),
        Err(errs) => errs
            .iter()
            .map(|e| format!("{} · {}", e.at, e.message))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[test]
fn 结构化配置里认不出的组名在装载时就报() {
    let mut cfg = base();
    cfg.global.tls_groups = vec!["P256".into()];
    let msg = build_err(&cfg);
    assert!(
        msg.contains("全局 tls_groups"),
        "没指到全局 tls_groups：{msg}"
    );
    assert!(msg.contains("`P-256`"), "没给正式名的建议：{msg}");

    // 大小寫寫法也不放行：放行的話同一個組在結構化設定裡就有兩種寫法。
    cfg.global.tls_groups = vec!["x25519".into()];
    assert!(build_err(&cfg).contains("`X25519`"));
}

#[test]
fn 重复的组名在装载时就报() {
    let mut cfg = base();
    cfg.global.tls_groups = vec!["X25519".into(), "X25519MLKEM768".into(), "X25519".into()];
    let msg = build_err(&cfg);
    assert!(msg.contains("写了两次"), "{msg}");
}

#[test]
fn 两个值原样带进运行时() {
    let mut cfg = base();
    cfg.global.tls_groups = vec!["X25519MLKEM768".into(), "P-256".into()];
    cfg.global.tls_session_resumption = false;
    let rt = Runtime::build(&cfg).expect("该建得起来");
    assert_eq!(
        rt.tls_policy,
        TlsPolicyRt {
            groups: vec!["X25519MLKEM768".into(), "P-256".into()],
            session_resumption: false,
        },
        "组的书写顺序也要原样带上（⛔ 不排序）"
    );
}

/// 不寫 ⇒ 預設政策 = 對 BoringSSL 不做任何呼叫。⚠ 反目標第一條：不寫的人什麼都不變。
#[test]
fn 不写时是缺省政策() {
    let rt = Runtime::build(&base()).expect("该建得起来");
    assert_eq!(rt.tls_policy, TlsPolicyRt::default());
    assert!(rt.tls_policy.groups.is_empty() && rt.tls_policy.session_resumption);
}
