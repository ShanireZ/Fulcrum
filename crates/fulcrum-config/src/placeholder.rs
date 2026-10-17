//! 佔位符（G61）：**小而固定的一組，無表達式、無函式、無條件**。
//!
//! ★ 避開的是 nginx 那個被點名的坑的近親：DSL 一旦長出圖靈完備的表達式層，
//! 它就變成一門沒人測試的程式設計語言，而錯誤提示會跟著徹底失控。
//! 固定一組換來的好處很具體——候選集有限，才提得出「你是不是想寫 X」。
//!
//! ★ ★ **用在不可用的位置是編譯錯誤，不是空串。**
//! 空串會讓誤用 `{status}` 的設定**看起來能跑**，只是輸出裡少一段——
//! 那正是本儲存庫反覆抓的那種無聲失敗。

use crate::diag::{DiagCode, Diagnostic, Span, suggest};

/// 佔位符能用在哪一側。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// 請求側：絕大多數指令。
    Request,
    /// 回應側：`header`（改回給用戶端的頭）、`header_down` 之類。
    Response,
    /// `handle_errors { … }` 塊內。
    ErrorHandler,
}

impl Ctx {
    fn label(self) -> &'static str {
        match self {
            Ctx::Request => "请求侧",
            Ctx::Response => "响应侧",
            Ctx::ErrorHandler => "handle_errors 块内",
        }
    }
}

/// 一個佔位符的規格。
struct Spec {
    /// 名字。帶 `.` 後綴的寫成前綴形式，例如 `header.`。
    key: &'static str,
    /// 是不是「前綴 + 任意後綴」的形式（`{header.X-Foo}`）。
    prefixed: bool,
    /// 允許出現在哪些上下文。
    ctxs: &'static [Ctx],
}

const ALL_CTX: &[Ctx] = &[Ctx::Request, Ctx::Response, Ctx::ErrorHandler];

/// 全表。★ **文件列全**（DSL 參考 §六），這裡就是那張表。
const TABLE: &[Spec] = &[
    Spec {
        key: "host",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "uri",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "path",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "query",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "method",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "scheme",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "remote_ip",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "remote_port",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "time",
        prefixed: false,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "header.",
        prefixed: true,
        ctxs: ALL_CTX,
    },
    // ★ `{path.N}` 是 `path_regexp` 的第 N 個捕獲組。它與 `{path}` 同名不同物，
    //   靠「有沒有後綴」區分——所以兩條都要在表裡，且前綴那條必須寫在後面。
    Spec {
        key: "path.",
        prefixed: true,
        ctxs: ALL_CTX,
    },
    Spec {
        key: "resp_header.",
        prefixed: true,
        ctxs: &[Ctx::Response, Ctx::ErrorHandler],
    },
    Spec {
        key: "upstream",
        prefixed: false,
        ctxs: &[Ctx::Response],
    },
    Spec {
        key: "status",
        prefixed: false,
        ctxs: &[Ctx::ErrorHandler],
    },
];

/// 每個佔位符在文件裡必然出現的那一小段。
///
/// ★ 帶後綴的只給到點號（`{header.`），**不比後綴怎麼寫**：
/// 文件裡是 `{header.<Name>}`，程式碼裡是任意後綴，把這兩者也拿去逐字比對
/// 只會讓契約測試紅在排版上，而排版不是契約。
pub fn doc_tokens() -> Vec<String> {
    TABLE
        .iter()
        .map(|s| {
            if s.prefixed {
                format!("{{{}", s.key)
            } else {
                format!("{{{}}}", s.key)
            }
        })
        .collect()
}

/// 候選名，供「你是不是想寫 X」用。
fn candidates() -> Vec<&'static str> {
    TABLE.iter().map(|s| s.key).collect()
}

/// 掃一個字串裡的佔位符，逐個檢核。
///
/// ★ 判據只認**佔位符形狀**的花括號：`{` 後面緊跟 `[a-z_]`，中間只有
/// 字母數字 `_` `-` `.`，然後一個 `}`。理由是實打實的——
/// `respond 403 {"error":"nope"}` 裡那對花括號不是佔位符，
/// 若一律當佔位符掃，使用者寫個 JSON 體就會收到一條莫名其妙的「未知佔位符」。
///
/// ★ 要寫字面值的 `{`，用 `{{`。
pub fn check(value: &str, span: Span, ctx: Ctx, out: &mut Vec<Diagnostic>) {
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            i += 2; // `{{` 是字面值的 `{`
            continue;
        }
        let Some(rel_end) = value[i + 1..].find('}') else {
            i += 1;
            continue;
        };
        let inner = &value[i + 1..i + 1 + rel_end];
        i = i + 1 + rel_end + 1;
        if !is_placeholder_shaped(inner) {
            continue;
        }
        check_one(inner, span, ctx, out);
    }
}

fn is_placeholder_shaped(inner: &str) -> bool {
    let mut chars = inner.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    inner
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn check_one(inner: &str, span: Span, ctx: Ctx, out: &mut Vec<Diagnostic>) {
    // 先按前綴匹配（長的優先），再按整名匹配。
    let mut matched: Option<&Spec> = None;
    for spec in TABLE {
        if spec.prefixed {
            if let Some(rest) = inner.strip_prefix(spec.key)
                && !rest.is_empty()
                && matched.is_none_or(|m| m.key.len() < spec.key.len())
            {
                matched = Some(spec);
            }
        } else if inner == spec.key {
            matched = Some(spec);
            break;
        }
    }

    let Some(spec) = matched else {
        let mut d = Diagnostic::error(
            DiagCode::UNKNOWN_PLACEHOLDER,
            span,
            format!("未知占位符 `{{{inner}}}`"),
        )
        .label("这个占位符不在表里")
        .note("全部占位符见 docs/architecture/dsl-reference.md §六（固定一组，无表达式、无函数）");
        if let Some(s) = suggest(inner, candidates()) {
            let shown = s.strip_suffix('.').map(|p| format!("{p}.<…>"));
            d = d.help(format!(
                "你是不是想写 `{{{}}}`？",
                shown.unwrap_or_else(|| s.to_string())
            ));
        }
        out.push(d);
        return;
    };

    if !spec.ctxs.contains(&ctx) {
        let allowed: Vec<&str> = spec.ctxs.iter().map(|c| c.label()).collect();
        out.push(
            Diagnostic::error(
                DiagCode::PLACEHOLDER_NOT_AVAILABLE,
                span,
                format!("`{{{inner}}}` 不能用在{}", ctx.label()),
            )
            .label(format!("它只在 {} 可用", allowed.join(" / ")))
            .note("★ 这里是编译错误而不是空串：空串会让配置看起来能跑，只是输出里少一段"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(v: &str, ctx: Ctx) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        check(v, Span::new(0, v.len()), ctx, &mut out);
        out
    }

    #[test]
    fn known_placeholders_pass() {
        for v in [
            "{host}",
            "{path}",
            "https://{host}{uri}",
            "{header.X-Forwarded-For}",
            "{path.1}",
            "{remote_ip}:{remote_port}",
        ] {
            assert!(run(v, Ctx::Request).is_empty(), "{v} 不该报错");
        }
    }

    #[test]
    fn status_only_in_error_handler() {
        assert_eq!(run("{status}", Ctx::ErrorHandler).len(), 0);
        let d = run("{status}", Ctx::Request);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, DiagCode::PLACEHOLDER_NOT_AVAILABLE);
    }

    #[test]
    fn upstream_only_on_response_side() {
        assert!(run("{upstream}", Ctx::Response).is_empty());
        assert_eq!(run("{upstream}", Ctx::Request).len(), 1);
    }

    #[test]
    fn unknown_gets_a_suggestion() {
        let d = run("{hostt}", Ctx::Request);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, DiagCode::UNKNOWN_PLACEHOLDER);
        assert_eq!(d[0].help.as_deref(), Some("你是不是想写 `{host}`？"));
    }

    #[test]
    fn json_body_is_not_scanned_as_a_placeholder() {
        // ★ 反向判據：不這樣做的話，`respond 403 {"error":"nope"}` 會收到
        //   一條完全莫名其妙的「未知佔位符」。
        assert!(run("{\"error\":\"nope\"}", Ctx::Request).is_empty());
        assert!(run("{ }", Ctx::Request).is_empty());
        assert!(
            run("{Host}", Ctx::Request).is_empty(),
            "大写开头不是占位符形状"
        );
    }

    #[test]
    fn doubled_brace_is_a_literal() {
        assert!(run("{{host}", Ctx::Request).is_empty());
    }

    #[test]
    fn prefix_and_bare_name_do_not_collide() {
        // `{path}` 與 `{path.1}` 同名不同物，兩條都必須認。
        assert!(run("{path}", Ctx::Request).is_empty());
        assert!(run("{path.2}", Ctx::Request).is_empty());
        // 只有前綴、沒有後綴的 `{header.}` 不成立。
        assert_eq!(run("{header.}", Ctx::Request).len(), 1);
    }

    #[test]
    fn doc_tokens_cover_the_whole_table() {
        // 這張表是文件 §六那張表的程式碼側；數量對不上就說明有一側漏了。
        assert_eq!(doc_tokens().len(), TABLE.len());
        assert!(doc_tokens().contains(&"{host}".to_string()));
        assert!(doc_tokens().contains(&"{header.".to_string()));
    }
}
