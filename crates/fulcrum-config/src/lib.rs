//! 枢衡的配置层：**DSL → 结构化配置**。
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
//! 规格见 [DSL 指令集参考](../../../docs/architecture/dsl-reference.md) 与
//! [配置分层](../../../docs/architecture/configuration.md)。
//!
//! # 三条最容易做错的事（都已经变成门）
//!
//! 1. **加了指令却没在执行顺序表里给它位置** —— 见 [`directive`]：序号少了编不过；
//!    而 [`compile`] 里的 `match` 没有 `_` 臂，「有位置但没人接」同样编不过。
//! 2. **把占位符在不可用的位置解析成空串** —— 见 [`placeholder`]，那是编译错误。
//! 3. **给 `remote_ip` 取 XFF 最左项** —— 那是客户端可伪造的。
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

/// 一次编译的结果。
///
/// ★ **诊断与产物同时返回**，不是二选一：G51 要求「一次报全」，
/// 而只有警告（没有错误）时产物是可用的——把它们做成 `Result` 会逼调用方
/// 在「有警告」时丢掉一份本来能用的配置。
pub struct Outcome {
    /// 只有在**没有 error 级诊断**时才是 `Some`。
    pub config: Option<StructuredConfig>,
    pub diagnostics: Diagnostics,
    pub source: Source,
}

impl Outcome {
    /// 把全部诊断渲染成一段可以直接打印的文本。
    pub fn render_diagnostics(&self) -> String {
        self.diagnostics.render(&self.source)
    }
}

/// 编译一份 DSL 文本。
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
