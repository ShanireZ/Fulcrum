//! 樞衡的設定層：**DSL → 結構化設定**。
//!
//! ```text
//! DSL（Caddyfile 式，人写）
//!         │  编译     ← 本 crate
//!         ▼
//! 结构化配置（JSON，唯一内部事实；机器、UI、自动化直接写这一层）
//!         │  校验 + 装载
//!         ▼
//! 运行时对象图（路由表、上游池、TLS 解析器、缓存句柄）
//! ```
//!
//! 規格見 [DSL 指令集參考](../../../docs/architecture/dsl-reference.md) 與
//! [設定分層](../../../docs/architecture/configuration.md)。
//!
//! # 三條最容易做錯的事（都已經變成門）
//!
//! 1. **加了指令卻沒在執行順序表裡給它位置** —— 見 [`directive`]：序號少了編不過；
//!    而 [`compile`] 裡的 `match` 沒有 `_` 臂，「有位置但沒人接」同樣編不過。
//! 2. **把佔位符在不可用的位置解析成空串** —— 見 [`placeholder`]，那是編譯錯誤。
//! 3. **給 `remote_ip` 取 XFF 最左項** —— 那是用戶端可偽造的。
//!
//! # 用法
//!
//! ```
//! use fulcrum_config::compile_str;
//!
//! let outcome = compile_str("Fulcrumfile", "example.com {\n    respond 200\n}\n");
//! assert!(!outcome.diagnostics.has_errors());
//! let cfg = outcome.config.unwrap();
//! assert_eq!(cfg.sites[0].addresses[0].port, 443);
//! ```

pub mod ast;
pub mod compile;
pub mod diag;
pub mod directive;
pub mod host;
pub mod lex;
pub mod model;
pub mod parse;
pub mod placeholder;
pub mod secret;
pub mod secret_guard;

pub use diag::{DiagCode, Diagnostic, Diagnostics, Severity, Source, Span};
pub use model::StructuredConfig;

/// 一次編譯的結果。
///
/// ★ **診斷與產物同時回傳**，不是二選一：G51 要求「一次報全」，
/// 而只有警告（沒有錯誤）時產物是可用的——把它們做成 `Result` 會逼呼叫方
/// 在「有警告」時丟掉一份本來能用的設定。
pub struct Outcome {
    /// 只有在**沒有 error 級診斷**時才是 `Some`。
    pub config: Option<StructuredConfig>,
    pub diagnostics: Diagnostics,
    pub source: Source,
}

impl Outcome {
    /// 把全部診斷繪製成一段可以直接列印的文字。
    pub fn render_diagnostics(&self) -> String {
        self.diagnostics.render(&self.source)
    }
}

/// 編譯一份 DSL 文字。
pub fn compile_str(path: &str, text: &str) -> Outcome {
    let source = Source::new(path, text);
    let mut diagnostics = Diagnostics::new();
    let file = parse::parse(&source, &mut diagnostics);
    let cfg = compile::compile(&file, &mut diagnostics);
    let config = if diagnostics.has_errors() {
        None
    } else {
        Some(cfg)
    };
    Outcome {
        config,
        diagnostics,
        source,
    }
}
