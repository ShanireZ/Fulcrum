//! 語法樹。
//!
//! ★ **刻意保持「無語義」**：解析階段只認形狀（名字 + 參數 + 可選子塊），
//! 一個字都不判斷它是不是已知指令。理由是 G51 的「一次報全」——
//! 語法層若順手做語義判斷，遇到第一個未知指令就會開始猜結構，
//! 於是後面所有真實錯誤的位置都不可信了。**分兩遍，語法錯與語義錯各報各的。**

use crate::diag::Span;

/// 一份設定檔案。
#[derive(Debug, Clone, Default)]
pub struct File {
    /// 全域選項塊（可選，必須在最前）。
    pub global: Option<Block>,
    pub blocks: Vec<TopBlock>,
}

/// 頂層的塊。M1 只有站台塊與 `l4` 塊兩種。
#[derive(Debug, Clone)]
pub enum TopBlock {
    Site(Site),
    /// ★ L4 面是**頂層非站台塊**（DSL 參考 §4.5）。M2 自研，M1 解析得過、編譯成回落。
    L4(Node),
}

/// 一個站台塊。
#[derive(Debug, Clone)]
pub struct Site {
    pub addresses: Vec<Addr>,
    /// 塊頭（位址那一段）的位置，報「這個站台塊……」時指這裡。
    pub header_span: Span,
    pub body: Block,
    pub span: Span,
}

/// 一個位址字面值。
#[derive(Debug, Clone)]
pub struct Addr {
    pub text: String,
    pub span: Span,
}

/// 一個塊的內容。
pub type Block = Vec<Stmt>;

/// 塊裡的一條語句。
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `@name { … }` 或 `@name path /x`
    Matcher(MatcherDef),
    /// 一條指令（也用來表示子指令、匹配條件——形狀相同）。
    Node(Node),
}

/// 命名匹配器的定義（G50）。
#[derive(Debug, Clone)]
pub struct MatcherDef {
    /// 不含 `@` 的名字。
    pub name: String,
    /// 含 `@` 的那個詞的位置。
    pub span: Span,
    /// 條件。★ 同一塊內**多條件是 AND**，同一條件寫多個值是 OR。
    pub conditions: Vec<Node>,
}

/// 「名字 + 參數 + 可選子塊」——指令、子指令、匹配條件共用這一個形狀。
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub name_span: Span,
    pub args: Vec<Arg>,
    /// `None` = 這條後面沒有跟 `{ … }`。
    pub block: Option<Block>,
    pub span: Span,
}

impl Node {
    /// 第一個參數是不是匹配器引用（`@name`、行內路徑，或 `*`）。
    ///
    /// ★ G50：**行內只允許路徑**。這裡只判「像不像一個匹配器位」，
    /// 「像路徑但不是路徑」的判定與回報錯誤在 compile 階段做——那裡才有能力給出好建議。
    ///
    /// ★ ★ **`*` 也算匹配器位，而它是必需的**（照 Caddy）。因為「第一個以 `/` 開頭的參數
    /// 就是匹配器」這條規則會把 `rewrite /new/x` 裡那個**目標路徑**吃掉，
    /// 於是最自然的寫法反而不可用。有了 `*`，使用者可以顯式說「全都匹配」：
    /// `rewrite * /new/x`。⚠ 少了這條逃生口，這條規則就是個死衚衕——
    /// 而死衚衕的表現是一句「參數不對」，看不出真正該怎麼寫。
    pub fn matcher_arg(&self) -> Option<&Arg> {
        self.args.first().filter(|a| {
            !a.quoted && (a.value.starts_with('@') || a.value.starts_with('/') || a.value == "*")
        })
    }

    /// 去掉匹配器位之後剩下的參數。
    pub fn rest_args(&self) -> &[Arg] {
        if self.matcher_arg().is_some() {
            &self.args[1..]
        } else {
            &self.args
        }
    }
}

/// 一個參數。
#[derive(Debug, Clone)]
pub struct Arg {
    /// 去引號、解轉義之後的值。
    pub value: String,
    pub span: Span,
    /// 原文是否帶引號。
    pub quoted: bool,
}
