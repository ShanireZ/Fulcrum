//! 詞法：把 DSL 文字切成 token。
//!
//! 形狀照 Caddyfile（G20）：**空白分隔的詞** + `{` / `}` + **有意義的換行**。
//!
//! ★ ★ **`{` 只有在它自己單獨成詞的時候才是塊的開始。** 這條不是實作細節，
//! 它是佔位符能存在的前提——`{host}` 與 `{"a":1}` 都是**一個詞**，
//! 而 `reverse_proxy 127.0.0.1:3000 {` 裡最後那個才是塊。
//! 若按「見到 `{` 就當塊開始」來切，`respond 403 {"error":"x"}` 會變成一個開塊，
//! 而它會一路吃到檔案末尾才報「塊沒閉合」——錯誤提示指向的位置離真兇十幾行遠。
//!
//! ★ 換行有意義：一條指令到行尾為止。這讓「少寫一個參數」這類錯誤能就地報出來，
//! 而不是把下一行的指令名當成本行的參數（那正是 nginx 缺了分號時的體驗）。

use crate::diag::{DiagCode, Diagnostic, Diagnostics, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// 一個詞（裸詞或引號串，`text` 裡是**去引號、解轉義後**的值）。
    Word,
    /// 單獨成詞的 `{`。
    OpenBrace,
    /// 單獨成詞的 `}`。
    CloseBrace,
    /// 一個或多個換行（連續空行摺疊成一個）。
    Newline,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// 詞的值。`OpenBrace` / `CloseBrace` / `Newline` / `Eof` 為空串。
    pub text: String,
    /// 這個詞原文是不是帶引號的。
    ///
    /// ★ 有用：帶引號的 `"{"` **不是**塊的開始，而佔位符檢核也要知道
    /// 一個值是不是使用者顯式引起來的。
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
                // 連續空行摺疊：它們對語法沒有意義。順帶吃掉下一行開頭的縮進也沒關係。
                // ★ 註解不在這裡處理——下一輪 `skip_blanks_and_comments` 會吃掉它，
                //   於是「空行 + 註解行 + 空行」可能切出兩個 Newline。**解析器必須容忍
                //   連續的 Newline**，這比在詞法裡再多一層狀態便宜。
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

    /// 跳過行內空白與註解。★ **不跳換行**——換行是 token。
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
        self.pos += 1; // 開引號
        let mut value = String::new();
        loop {
            if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                // ★ 引號不跨行。跨行的引號幾乎總是漏打了一個引號，而按「一直找到下一個引號」
                //   處理會把後面好幾行吞進一個字串裡，回報錯誤位置離真兇很遠。
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
            // 逐**字元**推進，不能逐位元組：多位元組字元會被劈開，push 出來是亂碼。
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
        // ★ 這條是佔位符能存在的前提。
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
        // ★ 關鍵：下一行仍然被正常切開，而不是被吞進字串。
        assert!(toks.iter().any(|t| t.text == "header"));
    }

    #[test]
    fn crlf_survives() {
        // 設定檔案在 Windows 上被編輯過就會帶 CRLF。它不該改變詞法結果。
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
