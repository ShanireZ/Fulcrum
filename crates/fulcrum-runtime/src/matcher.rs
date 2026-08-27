//! 编译后的匹配器（G50）。
//!
//! ★ ★ ★ **这一层会报错，而这不是重复劳动。**
//! DSL 前端已经查过「条件名认不认得」「有没有取值」，但它**没有查取值本身**——
//! `remote_ip 10.0.0.0/99` 与 `path_regexp (` 在 DSL 层都是合法的一个词。
//!
//! 而结构化配置那一层是**公开入口**（G11：机器直接写它），所以
//! **校验不能只长在 DSL 前端**：机器写进来的一份配置根本不经过词法与语法。
//! → 结论：**构建运行时图这一步就是结构化层的校验器**，`fulcrum validate` 要把它一起跑。
//!
//! ⚠ 反过来说也成立：这里报的错必须**在装载时**报完，不能留到请求路径上。
//! 一个「第一次有请求打到这条规则才发现正则编不过」的实现，
//! 等于把配置错误变成了线上事故。

use crate::glob::{Cidr, glob_match};
use crate::request::RequestCtx;
use fulcrum_config::model::{Condition, Matcher};
use regex::Regex;

/// 构建运行时图时发现的问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    /// 出问题的位置，形如 `sites[0] "example.com" · @internal · remote_ip`。
    pub at: String,
    pub message: String,
}

impl BuildError {
    pub fn new(at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            at: at.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}：{}", self.at, self.message)
    }
}

#[derive(Debug)]
enum Cond {
    Path {
        globs: Vec<String>,
        negate: bool,
    },
    PathRegexp {
        re: Regex,
        negate: bool,
    },
    Host {
        values: Vec<String>,
        negate: bool,
    },
    Method {
        values: Vec<String>,
        negate: bool,
    },
    Header {
        name: String,
        value: Option<String>,
        negate: bool,
    },
    Query {
        key: String,
        value: String,
        negate: bool,
    },
    RemoteIp {
        nets: Vec<Cidr>,
        negate: bool,
    },
}

/// 一个编译好的命名匹配器。
#[derive(Debug)]
pub struct CompiledMatcher {
    conds: Vec<Cond>,
}

impl CompiledMatcher {
    pub fn build(at: &str, m: &Matcher, errors: &mut Vec<BuildError>) -> CompiledMatcher {
        let mut conds = Vec::new();
        for c in &m.conditions {
            match compile_cond(at, c) {
                Ok(cond) => conds.push(cond),
                Err(e) => errors.push(e),
            }
        }
        CompiledMatcher { conds }
    }

    /// 求值。★ 同一匹配器内**多条件是 AND**；同一条件的多个值是 OR（G50）。
    ///
    /// `caps` 用来收 `path_regexp` 的捕获组，给 `{path.N}` 用。
    /// ⚠ **只有整体匹配成功时 `caps` 才有意义**：中途失败会留下已经写进去的组，
    /// 所以调用方在失败时必须丢弃它——这里用「成功才提交」的写法把这件事挡住。
    pub fn matches(&self, req: &RequestCtx<'_>, caps: &mut Vec<String>) -> bool {
        let mut staged: Vec<String> = Vec::new();
        for cond in &self.conds {
            if !eval(cond, req, &mut staged) {
                return false;
            }
        }
        if !staged.is_empty() {
            *caps = staged;
        }
        true
    }

    /// 没有任何条件的匹配器命中一切。★ 单独说一句是因为它容易被当成 bug：
    /// 一个空的 `@name { }` 在语义上就是「无条件」，而不是「永不命中」。
    pub fn is_empty(&self) -> bool {
        self.conds.is_empty()
    }
}

fn compile_cond(at: &str, c: &Condition) -> Result<Cond, BuildError> {
    let negate = c.negate;
    let where_ = |k: &str| format!("{at} · {k}");
    match c.kind.as_str() {
        "path" => Ok(Cond::Path {
            globs: c.values.clone(),
            negate,
        }),
        "path_regexp" => {
            // ★ DSL 允许 `path_regexp [name] <re>`：两个值时第一个是名字。
            //   这里取**最后**一个当正则，不取第一个——名字是可选的，
            //   而「取第一个」在带名字时会把名字当正则去编译，报一句莫名其妙的错。
            let pat = c
                .values
                .last()
                .ok_or_else(|| BuildError::new(where_("path_regexp"), "缺少正则"))?;
            let re = Regex::new(pat).map_err(|e| {
                BuildError::new(
                    where_("path_regexp"),
                    format!("正则编不过：{}", first_line(&e.to_string())),
                )
            })?;
            Ok(Cond::PathRegexp { re, negate })
        }
        "host" => Ok(Cond::Host {
            values: c.values.iter().map(|v| v.to_ascii_lowercase()).collect(),
            negate,
        }),
        "method" => Ok(Cond::Method {
            values: c.values.iter().map(|v| v.to_ascii_uppercase()).collect(),
            negate,
        }),
        "header" => {
            let name = c
                .values
                .first()
                .cloned()
                .ok_or_else(|| BuildError::new(where_("header"), "缺少头名"))?;
            Ok(Cond::Header {
                name,
                value: c.values.get(1).cloned(),
                negate,
            })
        }
        "query" => {
            let raw = c
                .values
                .first()
                .ok_or_else(|| BuildError::new(where_("query"), "缺少 `键=值`"))?;
            let (k, v) = raw.split_once('=').ok_or_else(|| {
                BuildError::new(where_("query"), format!("`{raw}` 不是 `键=值` 的形状"))
            })?;
            Ok(Cond::Query {
                key: k.to_string(),
                value: v.to_string(),
                negate,
            })
        }
        "remote_ip" => {
            let mut nets = Vec::new();
            for v in &c.values {
                let net = Cidr::parse(v).ok_or_else(|| {
                    BuildError::new(
                        where_("remote_ip"),
                        format!("`{v}` 不是合法的 IP 或 CIDR（如 `10.0.0.0/8`）"),
                    )
                })?;
                nets.push(net);
            }
            if nets.is_empty() {
                return Err(BuildError::new(where_("remote_ip"), "缺少网段"));
            }
            Ok(Cond::RemoteIp { nets, negate })
        }
        other => Err(BuildError::new(
            where_(other),
            "未知的匹配条件（结构化配置是公开入口，这一层也要校验）",
        )),
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn eval(cond: &Cond, req: &RequestCtx<'_>, caps: &mut Vec<String>) -> bool {
    let hit = match cond {
        Cond::Path { globs, .. } => globs.iter().any(|g| glob_match(g, req.path)),
        Cond::PathRegexp { re, .. } => match re.captures(req.path) {
            None => false,
            Some(c) => {
                // 第 0 组是整体匹配，`{path.N}` 说的是第 N 个**捕获**组。
                for i in 1..c.len() {
                    caps.push(c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default());
                }
                true
            }
        },
        Cond::Host { values, .. } => {
            let h = req.host.to_ascii_lowercase();
            values.contains(&h)
        }
        Cond::Method { values, .. } => {
            let m = req.method.to_ascii_uppercase();
            values.contains(&m)
        }
        Cond::Header { name, value, .. } => match req.headers.get(name) {
            None => false,
            Some(actual) => match value {
                None => true, // 只要求「存在」
                Some(want) => actual == want,
            },
        },
        Cond::Query { key, value, .. } => {
            query_pairs(req.query).any(|(k, v)| k == key && v == value)
        }
        Cond::RemoteIp { nets, .. } => match req.remote_ip {
            // ★ 取不到客户端 IP 时**判不命中**，不判命中。`remote_ip` 几乎总是用来
            //   放行内网，宁可放行失败也不能放行错人。
            None => false,
            Some(ip) => nets.iter().any(|n| n.contains(ip)),
        },
    };
    hit != negate_of(cond)
}

fn negate_of(cond: &Cond) -> bool {
    match cond {
        Cond::Path { negate, .. }
        | Cond::PathRegexp { negate, .. }
        | Cond::Host { negate, .. }
        | Cond::Method { negate, .. }
        | Cond::Header { negate, .. }
        | Cond::Query { negate, .. }
        | Cond::RemoteIp { negate, .. } => *negate,
    }
}

fn query_pairs(q: &str) -> impl Iterator<Item = (&str, &str)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::HeaderList;
    use fulcrum_config::model::Condition;

    fn cond(kind: &str, values: &[&str], negate: bool) -> Condition {
        Condition {
            kind: kind.to_string(),
            values: values.iter().map(|s| s.to_string()).collect(),
            negate,
        }
    }

    fn build(conds: Vec<Condition>) -> (CompiledMatcher, Vec<BuildError>) {
        let mut errs = Vec::new();
        let m = CompiledMatcher::build("t", &Matcher { conditions: conds }, &mut errs);
        (m, errs)
    }

    fn req<'a>(path: &'a str, method: &'a str, headers: &'a HeaderList<'a>) -> RequestCtx<'a> {
        RequestCtx {
            host: "a.com",
            port: 443,
            scheme: "https",
            method,
            path,
            query: "k=v&flag=1",
            headers,
            remote_ip: Some("10.1.2.3".parse().unwrap()),
            remote_port: 1,
        }
    }

    #[test]
    fn 多条件是_and_同条件多值是_or() {
        let (m, e) = build(vec![
            cond("path", &["/a/*", "/b/*"], false),
            cond("method", &["POST", "PUT"], false),
        ]);
        assert!(e.is_empty());
        let mut c = Vec::new();
        assert!(m.matches(&req("/a/x", "POST", &HeaderList(&[])), &mut c));
        assert!(m.matches(&req("/b/x", "PUT", &HeaderList(&[])), &mut c));
        // path 命中但 method 不命中 → AND 失败
        assert!(!m.matches(&req("/a/x", "GET", &HeaderList(&[])), &mut c));
        // method 命中但 path 不命中
        assert!(!m.matches(&req("/c/x", "POST", &HeaderList(&[])), &mut c));
    }

    #[test]
    fn 取反() {
        let (m, _) = build(vec![cond("path", &["/admin/*"], true)]);
        let mut c = Vec::new();
        assert!(m.matches(&req("/public", "GET", &HeaderList(&[])), &mut c));
        assert!(!m.matches(&req("/admin/x", "GET", &HeaderList(&[])), &mut c));
    }

    #[test]
    fn header_条件区分存在与等于() {
        let (exists, _) = build(vec![cond("header", &["X-Flag"], false)]);
        let (equals, _) = build(vec![cond("header", &["X-Flag", "yes"], false)]);
        let mut c = Vec::new();
        assert!(exists.matches(&req("/", "GET", &HeaderList(&[("x-flag", "no")])), &mut c));
        assert!(!equals.matches(&req("/", "GET", &HeaderList(&[("x-flag", "no")])), &mut c));
        assert!(equals.matches(&req("/", "GET", &HeaderList(&[("X-Flag", "yes")])), &mut c));
    }

    #[test]
    fn method_与_host_大小写不敏感() {
        let (m, _) = build(vec![cond("method", &["post"], false)]);
        let mut c = Vec::new();
        assert!(m.matches(&req("/", "POST", &HeaderList(&[])), &mut c));
        let (h, _) = build(vec![cond("host", &["A.COM"], false)]);
        assert!(h.matches(&req("/", "GET", &HeaderList(&[])), &mut c));
    }

    #[test]
    fn query_按键值对比而不是子串() {
        let (m, _) = build(vec![cond("query", &["flag=1"], false)]);
        let mut c = Vec::new();
        assert!(m.matches(&req("/", "GET", &HeaderList(&[])), &mut c));
        let (n, _) = build(vec![cond("query", &["flag=2"], false)]);
        assert!(!n.matches(&req("/", "GET", &HeaderList(&[])), &mut c));
        // ★ 反向：`k=v&flag=1` 里不存在键 `lag`，尽管它是个子串
        let (s, _) = build(vec![cond("query", &["lag=1"], false)]);
        assert!(!s.matches(&req("/", "GET", &HeaderList(&[])), &mut c));
    }

    #[test]
    fn 捕获组只在整体命中时提交() {
        let (m, e) = build(vec![
            cond("path_regexp", &["^/u/([0-9]+)/(.*)$"], false),
            // 第二条一定不命中 → 整体失败
            cond("method", &["DELETE"], false),
        ]);
        assert!(e.is_empty());
        let mut caps = vec!["原来的".to_string()];
        assert!(!m.matches(&req("/u/42/x", "GET", &HeaderList(&[])), &mut caps));
        assert_eq!(caps, vec!["原来的".to_string()], "失败时不该污染捕获组");

        let (ok, _) = build(vec![cond("path_regexp", &["^/u/([0-9]+)/(.*)$"], false)]);
        let mut caps2 = Vec::new();
        assert!(ok.matches(&req("/u/42/abc", "GET", &HeaderList(&[])), &mut caps2));
        assert_eq!(caps2, vec!["42".to_string(), "abc".to_string()]);
    }

    #[test]
    fn path_regexp_带名字时取最后一个当正则() {
        let (m, e) = build(vec![cond("path_regexp", &["myname", "^/x/(.+)$"], false)]);
        assert!(e.is_empty(), "{e:?}");
        let mut c = Vec::new();
        assert!(m.matches(&req("/x/y", "GET", &HeaderList(&[])), &mut c));
        assert_eq!(c, vec!["y".to_string()]);
    }

    #[test]
    fn remote_ip_取不到时判不命中() {
        let (m, _) = build(vec![cond("remote_ip", &["10.0.0.0/8"], false)]);
        let h = HeaderList(&[]);
        let mut ctx = req("/", "GET", &h);
        let mut c = Vec::new();
        assert!(m.matches(&ctx, &mut c));
        ctx.remote_ip = None;
        // ★ 宁可放行失败，也不能放行错人。
        assert!(!m.matches(&ctx, &mut c));
    }

    // ── 构建期校验：DSL 层查不到的那些 ──────────────────────────────────────

    #[test]
    fn 坏正则在构建期就报() {
        let (_, e) = build(vec![cond("path_regexp", &["^/(unclosed"], false)]);
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("正则编不过"), "{:?}", e[0]);
    }

    #[test]
    fn 坏_cidr_在构建期就报() {
        for bad in ["10.0.0.0/99", "not-an-ip", "10.0.0.0/x"] {
            let (_, e) = build(vec![cond("remote_ip", &[bad], false)]);
            assert_eq!(e.len(), 1, "{bad} 应当被拒");
        }
        let (_, e) = build(vec![cond(
            "remote_ip",
            &["10.0.0.0/8", "192.168.0.0/16"],
            false,
        )]);
        assert!(e.is_empty());
    }

    #[test]
    fn 结构化层直接写进来的未知条件也要被拒() {
        // ★ 机器写的那一份不经过 DSL 词法语法，所以这一层必须自己认。
        let (_, e) = build(vec![cond("hostt", &["a.com"], false)]);
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("未知的匹配条件"));
    }

    #[test]
    fn query_条件缺等号要报() {
        let (_, e) = build(vec![cond("query", &["flag"], false)]);
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("键=值"));
    }

    #[test]
    fn 空匹配器命中一切() {
        let (m, _) = build(vec![]);
        assert!(m.is_empty());
        let mut c = Vec::new();
        assert!(m.matches(&req("/anything", "GET", &HeaderList(&[])), &mut c));
    }
}
