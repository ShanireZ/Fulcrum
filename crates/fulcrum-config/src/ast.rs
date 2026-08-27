//! 语法树。
//!
//! ★ **刻意保持「无语义」**：解析阶段只认形状（名字 + 参数 + 可选子块），
//! 一个字都不判断它是不是已知指令。理由是 G51 的「一次报全」——
//! 语法层若顺手做语义判断，遇到第一个未知指令就会开始猜结构，
//! 于是后面所有真实错误的位置都不可信了。**分两遍，语法错与语义错各报各的。**

use crate::diag::Span;

/// 一份配置文件。
#[derive(Debug, Clone, Default)]
pub struct File {
    /// 全局选项块（可选，必须在最前）。
    pub global: Option<Block>,
    pub blocks: Vec<TopBlock>,
}

/// 顶层的块。M1 只有站点块与 `l4` 块两种。
#[derive(Debug, Clone)]
pub enum TopBlock {
    Site(Site),
    /// ★ L4 面是**顶层非站点块**（DSL 参考 §4.5）。M2 自研，M1 解析得过、编译成回落。
    L4(Node),
}

/// 一个站点块。
#[derive(Debug, Clone)]
pub struct Site {
    pub addresses: Vec<Addr>,
    /// 块头（地址那一段）的位置，报「这个站点块……」时指这里。
    pub header_span: Span,
    pub body: Block,
    pub span: Span,
}

/// 一个地址字面量。
#[derive(Debug, Clone)]
pub struct Addr {
    pub text: String,
    pub span: Span,
}

/// 一个块的内容。
pub type Block = Vec<Stmt>;

/// 块里的一条语句。
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `@name { … }` 或 `@name path /x`
    Matcher(MatcherDef),
    /// 一条指令（也用来表示子指令、匹配条件——形状相同）。
    Node(Node),
}

/// 命名匹配器的定义（G50）。
#[derive(Debug, Clone)]
pub struct MatcherDef {
    /// 不含 `@` 的名字。
    pub name: String,
    /// 含 `@` 的那个词的位置。
    pub span: Span,
    /// 条件。★ 同一块内**多条件是 AND**，同一条件写多个值是 OR。
    pub conditions: Vec<Node>,
}

/// 「名字 + 参数 + 可选子块」——指令、子指令、匹配条件共用这一个形状。
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub name_span: Span,
    pub args: Vec<Arg>,
    /// `None` = 这条后面没有跟 `{ … }`。
    pub block: Option<Block>,
    pub span: Span,
}

impl Node {
    /// 第一个参数是不是匹配器引用（`@name`、行内路径，或 `*`）。
    ///
    /// ★ G50：**行内只允许路径**。这里只判「像不像一个匹配器位」，
    /// 「像路径但不是路径」的判定与报错在 compile 阶段做——那里才有能力给出好建议。
    ///
    /// ★ ★ **`*` 也算匹配器位，而它是必需的**（照 Caddy）。因为「第一个以 `/` 开头的参数
    /// 就是匹配器」这条规则会把 `rewrite /new/x` 里那个**目标路径**吃掉，
    /// 于是最自然的写法反而不可用。有了 `*`，用户可以显式说「全都匹配」：
    /// `rewrite * /new/x`。⚠ 少了这条逃生口，这条规则就是个死胡同——
    /// 而死胡同的表现是一句「参数不对」，看不出真正该怎么写。
    pub fn matcher_arg(&self) -> Option<&Arg> {
        self.args.first().filter(|a| {
            !a.quoted && (a.value.starts_with('@') || a.value.starts_with('/') || a.value == "*")
        })
    }

    /// 去掉匹配器位之后剩下的参数。
    pub fn rest_args(&self) -> &[Arg] {
        if self.matcher_arg().is_some() {
            &self.args[1..]
        } else {
            &self.args
        }
    }
}

/// 一个参数。
#[derive(Debug, Clone)]
pub struct Arg {
    /// 去引号、解转义之后的值。
    pub value: String,
    pub span: Span,
    /// 原文是否带引号。
    pub quoted: bool,
}
