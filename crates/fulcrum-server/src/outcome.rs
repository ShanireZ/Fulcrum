//! `outcome` 闭集：那八个取值，以及**只能由这里造出取值**的那个类型。
//!
//! # ★ ★ ★ 为什么它是一个独立模块，而不是 `lib.rs` 里的几十行
//!
//! [`OutcomeName`] 的字段是私有的，而 **Rust 的私有是模块级的** ——
//! 把它和 `outcome_name` 放在同一个模块里，`OutcomeName("随便什么")` 在那个模块内部
//! 照样构造得出来，于是「裸字面量绕过常量表」这条路**只对别的模块关上了**，
//! 偏偏对最需要关上的那一处（`outcome_name` 自己）敞着。
//! ⚠ ⚠ 这不是推理出来的：2026-09-04 实测过一次 —— 把一条臂改成
//! `Outcome::Metrics => OutcomeName("x_smuggled")`，整棵树 `RC=0` 编得过。
//! ⇒ 边界必须**恰好画在这个类型周围**，屋里只许有宏和它生成的东西。

/// 访问日志 `outcome` 那一格、也是 `fulcrum_requests_total{outcome}` 那个**闭集**里的一个取值。
///
/// ★ ★ ★ **字段私有，而唯一的构造点是 [`outcomes!`] 生成的那批常量。**
/// ⇒ `Outcome::Foo => "foo"` 这种**绕过常量表往闭集里塞一个新值**的写法**编不过**。
///
/// ⚠ ⚠ 这一条比「宏把常量与数组一次生成」更靠前，两者缺一不可：
/// 宏只挡得住「声明了常量却忘了写进数组」，**挡不住裸字面量** ——
/// 而本 crate 的 `#[cfg(test)]` 夹具里一度就是那么写的（`r.outcome = "reverse_proxy"`），
/// ⇒ 那条逃逸路不是理论上的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutcomeName(&'static str);

impl OutcomeName {
    /// 渲染成日志字段值 / 指标标签值。
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// 生成 `outcome` 闭集：**一行一个取值**，常量与 [`OUTCOMES`] 同时长出来。
///
/// ★ 与 `fulcrum-config` 的 `chain_directives!` 是同一手法，理由也逐字相同：
/// **一行少了字面量就编不过，于是「加了取值却没进闭集」这件事无从发生。**
/// ⇒ [`OUTCOMES`] 与那批常量读的是**同一个** `$lit` token，⛔ 不是两份要靠人对齐的表。
macro_rules! outcomes {
    ( $( $konst:ident $lit:literal ),* $(,)? ) => {
        $(
            #[doc = concat!("`outcome` 闭集里的 `", $lit, "`。见 [`OUTCOMES`]。")]
            pub const $konst: OutcomeName = OutcomeName($lit);
        )*

        /// `fulcrum_requests_total{outcome}` 的**取值闭集**，`metrics.rs` 的基数表判据按它算上界。
        ///
        /// ⛔ **它不是一份手抄**：本常量与上面那批常量由 [`outcomes!`] 从同一行生成。
        pub const OUTCOMES: &[&str] = &[ $( $lit ),* ];
    };
}

outcomes! {
    // ── 由 `outcome_name` 从 `Outcome` 映射来的 ───────────────────────────
    OUTCOME_RESPOND       "respond",
    OUTCOME_REDIR         "redir",
    OUTCOME_REVERSE_PROXY "reverse_proxy",
    OUTCOME_FILE_SERVER   "file_server",
    // ★ 闭集的**第 8 个值**（M2 批 M，G116）。
    OUTCOME_METRICS       "metrics",
    // ── ⚠ 下面三个是**路由之前、或路由失败之后直接产出**的 ────────────────
    //    那时根本没有 `Outcome` 可映射。
    OUTCOME_ERROR         "error",
    OUTCOME_ACME_HTTP01   "acme_http01",
    OUTCOME_NO_SITE_MATCH "no_site_match",
}
