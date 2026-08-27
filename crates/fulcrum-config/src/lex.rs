//! 词法：把 DSL 文本切成 token。
//!
//! 形状照 Caddyfile（G20）：**空白分隔的词** + `{` / `}` + **有意义的换行**。
//!
//! ★ ★ **`{` 只有在它自己单独成词的时候才是块的开始。** 这条不是实现细节，
//! 它是占位符能存在的前提——`{host}` 与 `{"a":1}` 都是**一个词**，
//! 而 `reverse_proxy 127.0.0.1:3000 {` 里最后那个才是块。
//! 若按「见到 `{` 就当块开始」来切，`respond 403 {"error":"x"}` 会变成一个开块，
//! 而它会一路吃到文件末尾才报「块没闭合」——错误提示指向的位置离真凶十几行远。
//!
//! ★ 换行有意义：一条指令到行尾为止。这让「少写一个参数」这类错误能就地报出来，
//! 而不是把下一行的指令名当成本行的参数（那正是 nginx 缺了分号时的体验）。

use crate::diag::{DiagCode, Diagnostic, Diagnostics, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// 一个词（裸词或引号串，`text` 里是**去引号、解转义后**的值）。
    Word,
    /// 单独成词的 `{`。
    OpenBrace,
    /// 单独成词的 `}`。
    CloseBrace,
    /// 一个或多个换行（连续空行折叠成一个）。
    Newline,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// 词的值。`OpenBrace` / `CloseBrace` / `Newline` / `Eof` 为空串。
    pub text: String,
    /// 这个词原文是不是带引号的。
    ///
    /// ★ 有用：带引号的 `"{"` **不是**块的开始，而占位符校验也要知道
    /// 一个值是不是用户显式引起来的。
    pub quoted: bool,
}

impl Token {
    pub fn is(&self, kind: TokenKind) -> bool {
        self.kind == kind
    }
}

pub fn tokenize(text: &str, diags: &mut Diagnostics) -> Vec<Token> {
    Lexer {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        diags,
    }
    .run()
}

struct Lexer<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    diags: &'a mut Diagnostics,
}

impl Lexer<'_> {
    fn run(mut self) -> Vec<Token> {
        let mut out: Vec<Token> = Vec::new();
        loop {
            self.skip_blanks_and_comments();
            if self.pos >= self.bytes.len() {
                let span = Span::new(self.pos, self.pos);
                out.push(Token {
                    kind: TokenKind::Eof,
                    span,
                    text: String::new(),
                    quoted: false,
                });
                return out;
            }
            let b = self.bytes[self.pos];
            if b == b'\n' {
                let start = self.pos;
                // 连续空行折叠：它们对语法没有意义。顺带吃掉下一行开头的缩进也没关系。
                // ★ 注释不在这里处理——下一轮 `skip_blanks_and_comments` 会吃掉它，
                //   于是「空行 + 注释行 + 空行」可能切出两个 Newline。**解析器必须容忍
                //   连续的 Newline**，这比在词法里再多一层状态便宜。
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
                    self.pos += 1;
                }
                out.push(Token {
                    kind: TokenKind::Newline,
                    span: Span::new(start, self.pos),
                    text: String::new(),
                    quoted: false,
                });
                continue;
            }
            let tok = if b == b'"' {
                self.quoted_word()
            } else {
                self.bare_word()
            };
            out.push(tok);
        }
    }

    /// 跳过行内空白与注释。★ **不跳换行**——换行是 token。
    fn skip_blanks_and_comments(&mut self) {
        loop {
            while self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b' '
                    || self.bytes[self.pos] == b'\t'
                    || self.bytes[self.pos] == b'\r')
            {
                self.pos += 1;
            }
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'#' {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            return;
        }
    }

    fn quoted_word(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1; // 开引号
        let mut value = String::new();
        loop {
            if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                // ★ 引号不跨行。跨行的引号几乎总是漏打了一个引号，而按「一直找到下一个引号」
                //   处理会把后面好几行吞进一个字符串里，报错位置离真凶很远。
                let span = Span::new(start, self.pos);
                self.diags.push(
                    Diagnostic::error(DiagCode::UNCLOSED_QUOTE, span, "引号没有闭合")
                        .label("这个引号一直到行尾都没有对上的那一个")
                        .note("字符串不能跨行；含空格、`{`、`#` 的值才需要引号"),
                );
                return Token {
                    kind: TokenKind::Word,
                    span,
                    text: value,
                    quoted: true,
                };
            }
            let b = self.bytes[self.pos];
            if b == b'\\' && self.pos + 1 < self.bytes.len() {
                let next = self.bytes[self.pos + 1];
                if next == b'"' || next == b'\\' {
                    value.push(next as char);
                    self.pos += 2;
                    continue;
                }
            }
            if b == b'"' {
                self.pos += 1;
                let span = Span::new(start, self.pos);
                return Token {
                    kind: TokenKind::Word,
                    span,
                    text: value,
                    quoted: true,
                };
            }
            // 逐**字符**推进，不能逐字节：多字节字符会被劈开，push 出来是乱码。
            let ch = self.text[self.pos..].chars().next().unwrap_or('\u{fffd}');
            value.push(ch);
            self.pos += ch.len_utf8();
        }
    }

    fn bare_word(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() || b == b'#' {
                break;
            }
            self.pos += 1;
        }
        let span = Span::new(start, self.pos);
        let raw = &self.text[start..self.pos];
        let kind = match raw {
            "{" => TokenKind::OpenBrace,
            "}" => TokenKind::CloseBrace,
            _ => TokenKind::Word,
        };
        Token {
            kind,
            span,
            text: if kind == TokenKind::Word {
                raw.to_string()
            } else {
                String::new()
            },
            quoted: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> (Vec<TokenKind>, Diagnostics) {
        let mut d = Diagnostics::new();
        let toks = tokenize(src, &mut d);
        (toks.iter().map(|t| t.kind).collect(), d)
    }

    #[test]
    fn brace_is_structural_only_when_alone() {
        // ★ 这条是占位符能存在的前提。
        let (k, d) = kinds("respond 403 {\"error\":\"x\"}\n");
        assert!(d.is_empty(), "{}", d.items().len());
        assert_eq!(
            k,
            vec![
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Word,
                TokenKind::Newline,
                TokenKind::Eof
            ]
        );

        let (k, _) = kinds("example.com {\n}\n");
        assert_eq!(
            k,
            vec![
                TokenKind::Word,
                TokenKind::OpenBrace,
                TokenKind::Newline,
                TokenKind::CloseBrace,
                TokenKind::Newline,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn placeholder_is_one_word() {
        let mut d = Diagnostics::new();
        let toks = tokenize("header X-Host {host}\n", &mut d);
        assert!(d.is_empty());
        assert_eq!(toks[2].text, "{host}");
        assert_eq!(toks[2].kind, TokenKind::Word);
    }

    #[test]
    fn comments_and_quotes() {
        let mut d = Diagnostics::new();
        let toks = tokenize("a \"b c\" # 注释里的 { 不算块\nd\n", &mut d);
        assert!(d.is_empty());
        assert_eq!(toks[1].text, "b c");
        assert!(toks[1].quoted);
        assert_eq!(toks[3].text, "d");
    }

    #[test]
    fn quoted_escapes() {
        let mut d = Diagnostics::new();
        let toks = tokenize("respond \"say \\\"hi\\\"\"\n", &mut d);
        assert!(d.is_empty());
        assert_eq!(toks[1].text, "say \"hi\"");
    }

    #[test]
    fn unclosed_quote_stops_at_line_end() {
        let mut d = Diagnostics::new();
        let toks = tokenize("respond \"没闭合\nheader X 1\n", &mut d);
        assert_eq!(d.error_count(), 1);
        assert_eq!(d.items()[0].code, DiagCode::UNCLOSED_QUOTE);
        // ★ 关键：下一行仍然被正常切开，而不是被吞进字符串。
        assert!(toks.iter().any(|t| t.text == "header"));
    }

    #[test]
    fn crlf_survives() {
        // 配置文件在 Windows 上被编辑过就会带 CRLF。它不该改变词法结果。
        let mut d = Diagnostics::new();
        let a = tokenize("a b\nc\n", &mut d);
        let mut d2 = Diagnostics::new();
        let b = tokenize("a b\r\nc\r\n", &mut d2);
        let texts = |v: &Vec<Token>| v.iter().map(|t| t.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&a), texts(&b));
        assert!(d.is_empty() && d2.is_empty());
    }

    #[test]
    fn multibyte_inside_quotes_is_not_split() {
        let mut d = Diagnostics::new();
        let toks = tokenize("respond \"中文值\"\n", &mut d);
        assert!(d.is_empty());
        assert_eq!(toks[1].text, "中文值");
    }
}
