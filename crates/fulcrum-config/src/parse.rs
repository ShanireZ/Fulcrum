//! 語法分析：token 流 → [`ast::File`]。
//!
//! ★ **帶錯誤恢復**（G51 的「一次報全」）。遇到語法錯時不停下，而是丟棄到
//! **本行末尾**或**本塊結束**再繼續。判斷標準很簡單：
//! 一條指令的邊界是換行，一個塊的邊界是花括號——這兩樣都不依賴語義，
//! 所以就算前面剛報過錯，恢復點仍然是可信的。
//!
//! ⚠ 恢復不是免費的：報出來的第 2 條以後的診斷可能是第 1 條的餘波。
//! 處置是**不發明**——只報確實看見的形狀問題，不猜使用者想寫什麼。

use crate::ast::{Addr, Arg, Block, File, MatcherDef, Node, Site, Stmt, TopBlock};
use crate::diag::{DiagCode, Diagnostic, Diagnostics, Source, Span};
use crate::lex::{Token, TokenKind, tokenize};

pub fn parse(src: &Source, diags: &mut Diagnostics) -> File {
    let tokens = tokenize(src.text(), diags);
    Parser {
        tokens,
        pos: 0,
        diags,
    }
    .file()
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    diags: &'a mut Diagnostics,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn advance(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.advance();
        }
    }

    /// 丟棄到本行末尾（含那個換行）。恢復點之一。
    fn skip_to_line_end(&mut self) {
        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
            // ★ 花括號也要跟著走，否則一條壞行裡的 `{` 會讓後面整塊結構錯位。
            if self.at(TokenKind::OpenBrace) {
                self.skip_balanced_block();
                continue;
            }
            if self.at(TokenKind::CloseBrace) {
                return;
            }
            self.advance();
        }
        self.skip_newlines();
    }

    /// 從當前的 `{` 開始，跳過配平的一整塊。
    fn skip_balanced_block(&mut self) {
        if !self.at(TokenKind::OpenBrace) {
            return;
        }
        let mut depth = 0usize;
        loop {
            match self.peek().kind {
                TokenKind::OpenBrace => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::CloseBrace => {
                    self.advance();
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                TokenKind::Eof => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn file(mut self) -> File {
        let mut file = File::default();
        self.skip_newlines();

        // 全域選項塊：檔案裡第一個非空 token 就是 `{`。
        if self.at(TokenKind::OpenBrace) {
            let open = self.advance();
            file.global = Some(self.block_body(open.span));
        }

        loop {
            self.skip_newlines();
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::CloseBrace => {
                    let t = self.advance();
                    self.diags.push(
                        Diagnostic::error(DiagCode::UNEXPECTED_CLOSE, t.span, "多出来的 `}`")
                            .label("这里没有需要闭合的块"),
                    );
                }
                TokenKind::OpenBrace => {
                    let t = self.peek().clone();
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::GLOBAL_BLOCK_POSITION,
                            t.span,
                            "全局选项块必须写在文件最前面，且只能有一个",
                        )
                        .label("这个匿名块不在文件开头")
                        .note("全局选项见 docs/architecture/dsl-reference.md §一"),
                    );
                    self.skip_balanced_block();
                }
                _ => {
                    if let Some(b) = self.top_block() {
                        file.blocks.push(b);
                    }
                }
            }
        }
        file
    }

    /// 一個頂層塊：站台塊，或 `l4 { … }`。
    fn top_block(&mut self) -> Option<TopBlock> {
        let start = self.peek().span;
        let mut header: Vec<Token> = Vec::new();
        while self.at(TokenKind::Word) {
            header.push(self.advance());
        }

        if header.is_empty() {
            // 只可能是 Newline/Eof 之外的怪東西，交給上面的分支處理。
            self.advance();
            return None;
        }

        let header_span = header.iter().fold(start, |acc, t| acc.join(t.span));

        if !self.at(TokenKind::OpenBrace) {
            let here = self.peek().span;
            self.diags.push(
                Diagnostic::error(
                    DiagCode::SITE_BLOCK_EXPECTED,
                    Span::new(header_span.end, here.end.max(header_span.end + 1)),
                    "站点块缺少 `{`",
                )
                .label("这里应当是 `{`")
                .note("站点块的形状是 `example.com { … }`（DSL 参考 §二）"),
            );
            self.skip_to_line_end();
            return None;
        }
        let open = self.advance();
        let body = self.block_body(open.span);
        let span = header_span.join(open.span);

        // ★ `l4` 是**頂層非站台塊**（DSL 參考 §4.5），不是一個叫 l4 的站台。
        //   判據取「塊頭恰好是一個詞 `l4`」——站台位址不可能長這樣（沒有點、沒有冒號）。
        if header.len() == 1 && header[0].text == "l4" {
            return Some(TopBlock::L4(Node {
                name: "l4".to_string(),
                name_span: header[0].span,
                args: Vec::new(),
                block: Some(body),
                span,
            }));
        }

        let addresses = split_addresses(&header);
        Some(TopBlock::Site(Site {
            addresses,
            header_span,
            body,
            span,
        }))
    }

    /// `{` 已經消費掉了，讀到配對的 `}` 為止。
    fn block_body(&mut self, open_span: Span) -> Block {
        let mut out: Block = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek().kind {
                TokenKind::CloseBrace => {
                    self.advance();
                    return out;
                }
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::UNCLOSED_BLOCK,
                            open_span,
                            "这个块到文件末尾都没有闭合",
                        )
                        .label("从这里开始")
                        .help("检查是不是少了一个 `}`"),
                    );
                    return out;
                }
                TokenKind::OpenBrace => {
                    let t = self.peek().clone();
                    self.diags.push(
                        Diagnostic::error(
                            DiagCode::BLOCK_MISMATCH,
                            t.span,
                            "这里出现了一个没有主人的 `{`",
                        )
                        .label("`{` 前面应当有指令名")
                        .note("子块要跟在指令后面，如 `reverse_proxy … { … }`"),
                    );
                    self.skip_balanced_block();
                }
                _ => {
                    if let Some(s) = self.stmt() {
                        out.push(s);
                    }
                }
            }
        }
    }

    fn stmt(&mut self) -> Option<Stmt> {
        let tok = self.peek().clone();
        if tok.kind == TokenKind::Word && !tok.quoted && tok.text.starts_with('@') {
            return self.matcher_def().map(Stmt::Matcher);
        }
        self.node().map(Stmt::Node)
    }

    fn matcher_def(&mut self) -> Option<MatcherDef> {
        let head = self.advance();
        let name = head.text.trim_start_matches('@').to_string();
        let mut conditions = Vec::new();

        if self.at(TokenKind::OpenBrace) {
            let open = self.advance();
            for s in self.block_body(open.span) {
                match s {
                    Stmt::Node(n) => conditions.push(n),
                    Stmt::Matcher(m) => {
                        self.diags.push(
                            Diagnostic::error(
                                DiagCode::UNKNOWN_MATCHER_CONDITION,
                                m.span,
                                "匹配器块里不能再定义匹配器",
                            )
                            .label("这里只能写条件"),
                        );
                    }
                }
            }
        } else if self.at(TokenKind::Word) {
            // 一行式：`@name path /x`
            if let Some(n) = self.node() {
                conditions.push(n);
            }
        } else {
            self.diags.push(
                Diagnostic::error(
                    DiagCode::UNKNOWN_MATCHER_CONDITION,
                    head.span,
                    "这个匹配器没有任何条件",
                )
                .label("后面要跟条件，或者一个 `{ … }` 块")
                .note("匹配器写法见 docs/architecture/dsl-reference.md §五"),
            );
            self.skip_to_line_end();
        }

        Some(MatcherDef {
            name,
            span: head.span,
            conditions,
        })
    }

    fn node(&mut self) -> Option<Node> {
        let head = self.advance();
        if head.kind != TokenKind::Word {
            // 不該走到這裡；保底防死循環。
            return None;
        }
        let mut args: Vec<Arg> = Vec::new();
        while self.at(TokenKind::Word) {
            let t = self.advance();
            args.push(Arg {
                value: t.text,
                span: t.span,
                quoted: t.quoted,
            });
        }

        let mut block = None;
        let mut span = args.iter().fold(head.span, |acc, a| acc.join(a.span));
        if self.at(TokenKind::OpenBrace) {
            let open = self.advance();
            span = span.join(open.span);
            block = Some(self.block_body(open.span));
        }

        Some(Node {
            name: head.text,
            name_span: head.span,
            args,
            block,
            span,
        })
    }
}

/// 塊頭的那些詞切成位址。`a.com, b.com` / `a.com,b.com` / `a.com , b.com` 都認。
///
/// ★ 逗號只是分隔符，不帶語義。位址本身合不合法在 compile 階段判——
/// 那裡能給出「你是不是想寫 `http://…`」這種建議，詞法層給不出。
fn split_addresses(header: &[Token]) -> Vec<Addr> {
    let mut out = Vec::new();
    for t in header {
        for piece in t.text.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            out.push(Addr {
                text: piece.to_string(),
                span: t.span,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> (File, Diagnostics) {
        let src = Source::new("test.Fulcrumfile", s);
        let mut d = Diagnostics::new();
        let f = parse(&src, &mut d);
        (f, d)
    }

    #[test]
    fn basic_site() {
        let (f, d) = parse_str("example.com {\n    reverse_proxy 127.0.0.1:3000\n}\n");
        assert!(d.is_empty(), "{:?}", d.items());
        assert_eq!(f.blocks.len(), 1);
        let TopBlock::Site(site) = &f.blocks[0] else {
            panic!("应当是站点块")
        };
        assert_eq!(site.addresses[0].text, "example.com");
        assert_eq!(site.body.len(), 1);
    }

    #[test]
    fn global_block_first() {
        let (f, d) = parse_str("{\n    acme_email a@b.c\n}\n\nx.com {\n    respond 200\n}\n");
        assert!(d.is_empty(), "{:?}", d.items());
        assert_eq!(f.global.as_ref().unwrap().len(), 1);
        assert_eq!(f.blocks.len(), 1);
    }

    #[test]
    fn global_block_not_first_is_an_error() {
        let (_, d) = parse_str("x.com {\n    respond 200\n}\n{\n    acme_email a@b.c\n}\n");
        assert_eq!(d.error_count(), 1);
        assert_eq!(d.items()[0].code, DiagCode::GLOBAL_BLOCK_POSITION);
    }

    #[test]
    fn multiple_addresses() {
        let (f, d) = parse_str("a.com, b.com {\n    respond 200\n}\n");
        assert!(d.is_empty());
        let TopBlock::Site(site) = &f.blocks[0] else {
            panic!()
        };
        assert_eq!(site.addresses.len(), 2);
        let (f2, _) = parse_str("a.com,b.com {\n    respond 200\n}\n");
        let TopBlock::Site(site2) = &f2.blocks[0] else {
            panic!()
        };
        assert_eq!(site2.addresses.len(), 2);
    }

    #[test]
    fn matcher_block_and_oneliner() {
        let (f, d) = parse_str(
            "a.com {\n  @m {\n    path /x\n    method POST\n  }\n  @n path /y\n  respond 200\n}\n",
        );
        assert!(d.is_empty(), "{:?}", d.items());
        let TopBlock::Site(site) = &f.blocks[0] else {
            panic!()
        };
        let Stmt::Matcher(m) = &site.body[0] else {
            panic!()
        };
        assert_eq!(m.name, "m");
        assert_eq!(m.conditions.len(), 2);
        let Stmt::Matcher(n) = &site.body[1] else {
            panic!()
        };
        assert_eq!(n.conditions.len(), 1);
    }

    #[test]
    fn nested_blocks() {
        let (f, d) = parse_str(
            "a.com {\n  handle /api/* {\n    reverse_proxy x:1 y:2 {\n      lb_policy least_conn\n    }\n  }\n}\n",
        );
        assert!(d.is_empty(), "{:?}", d.items());
        let TopBlock::Site(site) = &f.blocks[0] else {
            panic!()
        };
        let Stmt::Node(handle) = &site.body[0] else {
            panic!()
        };
        assert_eq!(handle.name, "handle");
        assert_eq!(handle.block.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn l4_is_a_top_level_block_not_a_site() {
        let (f, d) = parse_str("l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306\n  }\n}\n");
        assert!(d.is_empty(), "{:?}", d.items());
        assert!(matches!(f.blocks[0], TopBlock::L4(_)));
    }

    #[test]
    fn missing_open_brace_reports_once_and_recovers() {
        // ★ 一次報全：第一行壞了，第二個站台塊仍然要被解析出來。
        let (f, d) = parse_str("a.com\nb.com {\n  respond 200\n}\n");
        assert!(
            d.items()
                .iter()
                .any(|x| x.code == DiagCode::SITE_BLOCK_EXPECTED),
            "{:?}",
            d.items()
        );
        assert_eq!(f.blocks.len(), 1, "坏行之后的块仍然要解析出来");
    }

    #[test]
    fn unclosed_block_is_reported_at_the_open_brace() {
        let (_, d) = parse_str("a.com {\n  respond 200\n");
        assert_eq!(d.error_count(), 1);
        assert_eq!(d.items()[0].code, DiagCode::UNCLOSED_BLOCK);
        // 位置指向 `{` 那一行，不是檔案末尾。
        let src = Source::new("t", "a.com {\n  respond 200\n");
        assert_eq!(src.location(d.items()[0].span.start).line, 1);
    }

    #[test]
    fn stray_close_brace() {
        let (_, d) = parse_str("}\n");
        assert_eq!(d.items()[0].code, DiagCode::UNEXPECTED_CLOSE);
    }

    #[test]
    fn inline_block_on_one_line() {
        let (f, d) = parse_str("a.com { respond 200 }\n");
        assert!(d.is_empty(), "{:?}", d.items());
        let TopBlock::Site(site) = &f.blocks[0] else {
            panic!()
        };
        assert_eq!(site.body.len(), 1);
    }
}
