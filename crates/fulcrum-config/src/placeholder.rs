//! 占位符（G61）：**小而固定的一组，无表达式、无函数、无条件**。
//!
//! ★ 避开的是 nginx 那个被点名的坑的近亲：DSL 一旦长出图灵完备的表达式层，
//! 它就变成一门没人测试的编程语言，而错误提示会跟着彻底失控。
//! 固定一组换来的好处很具体——候选集有限，才提得出「你是不是想写 X」。
//!
//! ★ ★ **用在不可用的位置是编译错误，不是空串。**
//! 空串会让误用 `{status}` 的配置**看起来能跑**，只是输出里少一段——
//! 那正是本仓库反复抓的那种无声失败。

use crate::diag::{DiagCode, Diagnostic, Span, suggest};

/// 占位符能用在哪一侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// 请求侧：绝大多数指令。
    Request,
    /// 响应侧：`header`（改回给客户端的头）、`header_down` 之类。
    Response,
    /// `handle_errors { … }` 块内。
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

/// 一个占位符的规格。
struct Spec {
    /// 名字。带 `.` 后缀的写成前缀形式，例如 `header.`。
    key: &'static str,
    /// 是不是「前缀 + 任意后缀」的形式（`{header.X-Foo}`）。
    prefixed: bool,
    /// 允许出现在哪些上下文。
    ctxs: &'static [Ctx],
}

const ALL_CTX: &[Ctx] = &[Ctx::Request, Ctx::Response, Ctx::ErrorHandler];

/// 全表。★ **文档列全**（DSL 参考 §六），这里就是那张表。
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
    // ★ `{path.N}` 是 `path_regexp` 的第 N 个捕获组。它与 `{path}` 同名不同物，
    //   靠「有没有后缀」区分——所以两条都要在表里，且前缀那条必须写在后面。
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

/// 每个占位符在文档里必然出现的那一小段。
///
/// ★ 带后缀的只给到点号（`{header.`），**不比后缀怎么写**：
/// 文档里是 `{header.<Name>}`，代码里是任意后缀，把这两者也拿去逐字比对
/// 只会让契约测试红在排版上，而排版不是契约。
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

/// 候选名，供「你是不是想写 X」用。
fn candidates() -> Vec<&'static str> {
    TABLE.iter().map(|s| s.key).collect()
}

/// 扫一个字符串里的占位符，逐个校验。
///
/// ★ 判据只认**占位符形状**的花括号：`{` 后面紧跟 `[a-z_]`，中间只有
/// 字母数字 `_` `-` `.`，然后一个 `}`。理由是实打实的——
/// `respond 403 {"error":"nope"}` 里那对花括号不是占位符，
/// 若一律当占位符扫，用户写个 JSON 体就会收到一条莫名其妙的「未知占位符」。
///
/// ★ 要写字面量的 `{`，用 `{{`。
pub fn check(value: &str, span: Span, ctx: Ctx, out: &mut Vec<Diagnostic>) {
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            i += 2; // `{{` 是字面量的 `{`
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
    // 先按前缀匹配（长的优先），再按整名匹配。
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
        // ★ 反向判据：不这样做的话，`respond 403 {"error":"nope"}` 会收到
        //   一条完全莫名其妙的「未知占位符」。
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
        // `{path}` 与 `{path.1}` 同名不同物，两条都必须认。
        assert!(run("{path}", Ctx::Request).is_empty());
        assert!(run("{path.2}", Ctx::Request).is_empty());
        // 只有前缀、没有后缀的 `{header.}` 不成立。
        assert_eq!(run("{header.}", Ctx::Request).len(), 1);
    }

    #[test]
    fn doc_tokens_cover_the_whole_table() {
        // 这张表是文档 §六那张表的代码侧；数量对不上就说明有一侧漏了。
        assert_eq!(doc_tokens().len(), TABLE.len());
        assert!(doc_tokens().contains(&"{host}".to_string()));
        assert!(doc_tokens().contains(&"{header.".to_string()));
    }
}
