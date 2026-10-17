//! `outcome` 閉集：那八個取值，以及**只能由這裡造出取值**的那個類型。
//!
//! # ★ ★ ★ 為什麼它是一個獨立模組，而不是 `lib.rs` 裡的幾十行
//!
//! [`OutcomeName`] 的欄位是私有的，而 **Rust 的私有是模組級的** ——
//! 把它和 `outcome_name` 放在同一個模組裡，`OutcomeName("随便什么")` 在那個模組內部
//! 照樣建構得出來，於是「裸字面值繞過常數表」這條路**只對別的模組關上了**，
//! 偏偏對最需要關上的那一處（`outcome_name` 自己）敞著。
//! ⚠ ⚠ 這不是推理出來的：2026-09-04 實測過一次 —— 把一條臂改成
//! `Outcome::Metrics => OutcomeName("x_smuggled")`，整棵樹 `RC=0` 編得過。
//! ⇒ 邊界必須**恰好畫在這個類型周圍**，屋裡只許有宏和它生成的東西。

/// 訪問紀錄 `outcome` 那一格、也是 `fulcrum_requests_total{outcome}` 那個**閉集**裡的一個取值。
///
/// ★ ★ ★ **欄位私有，而唯一的建構點是 [`outcomes!`] 生成的那批常數。**
/// ⇒ `Outcome::Foo => "foo"` 這種**繞過常數表往閉集裡塞一個新值**的寫法**編不過**。
///
/// ⚠ ⚠ 這一條比「宏把常數與陣列一次生成」更靠前，兩者缺一不可：
/// 宏只擋得住「宣告了常數卻忘了寫進陣列」，**擋不住裸字面值** ——
/// 而本 crate 的 `#[cfg(test)]` 夾具裡一度就是那麼寫的（`r.outcome = "reverse_proxy"`），
/// ⇒ 那條逃逸路不是理論上的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutcomeName(&'static str);

impl OutcomeName {
    /// 繪製成紀錄欄位值 / 指標標籤值。
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// 生成 `outcome` 閉集：**一行一個取值**，常數與 [`OUTCOMES`] 同時長出來。
///
/// ★ 與 `fulcrum-config` 的 `chain_directives!` 是同一手法，理由也逐字相同：
/// **一行少了字面值就編不過，於是「加了取值卻沒進閉集」這件事無從發生。**
/// ⇒ [`OUTCOMES`] 與那批常數讀的是**同一個** `$lit` token，⛔ 不是兩份要靠人對齊的表。
macro_rules! outcomes {
    ( $( $konst:ident $lit:literal ),* $(,)? ) => {
        $(
            #[doc = concat!("`outcome` 闭集里的 `", $lit, "`。见 [`OUTCOMES`]。")]
            pub const $konst: OutcomeName = OutcomeName($lit);
        )*

        /// `fulcrum_requests_total{outcome}` 的**取值閉集**，`metrics.rs` 的基數表判據按它算上界。
        ///
        /// ⛔ **它不是一份手抄**：本常數與上面那批常數由 [`outcomes!`] 從同一行生成。
        pub const OUTCOMES: &[&str] = &[ $( $lit ),* ];
    };
}

outcomes! {
    // ── 由 `outcome_name` 從 `Outcome` 映射來的 ───────────────────────────
    OUTCOME_RESPOND       "respond",
    OUTCOME_REDIR         "redir",
    OUTCOME_REVERSE_PROXY "reverse_proxy",
    OUTCOME_FILE_SERVER   "file_server",
    // ★ 閉集的**第 8 個值**（M2 批 M，G116）。
    OUTCOME_METRICS       "metrics",
    // ── ⚠ 下面三個是**路由之前、或路由失敗之後直接產出**的 ────────────────
    //    那時根本沒有 `Outcome` 可映射。
    OUTCOME_ERROR         "error",
    OUTCOME_ACME_HTTP01   "acme_http01",
    OUTCOME_NO_SITE_MATCH "no_site_match",
}
