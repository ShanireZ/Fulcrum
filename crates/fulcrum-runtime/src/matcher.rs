//! 編譯後的匹配器（G50）。
//!
//! ★ ★ ★ **這一層會回報錯誤，而這不是重複勞動。**
//! DSL 前端已經查過「條件名認不認得」「有沒有取值」，但它**沒有查取值本身**——
//! `remote_ip 10.0.0.0/99` 與 `path_regexp (` 在 DSL 層都是合法的一個詞。
//!
//! 而結構化設定那一層是**公開入口**（G11：機器直接寫它），所以
//! **檢核不能只長在 DSL 前端**：機器寫進來的一份設定根本不經過詞法與語法。
//! → 結論：**建置執行環境圖這一步就是結構化層的檢核器**，`fulcrum validate` 要把它一起跑。
//!
//! ⚠ 反過來說也成立：這裡報的錯必須**在裝載時**報完，不能留到請求路徑上。
//! 一個「第一次有請求打到這條規則才發現正則編不過」的實作，
//! 等於把設定錯誤變成了線上事故。

use crate::glob::{Cidr, glob_match};
use crate::request::RequestCtx;
use fulcrum_config::model::{Condition, Matcher};
use regex::Regex;

/// 建置執行環境圖時發現的問題。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    /// 出問題的位置，形如 `sites[0] "example.com" · @internal · remote_ip`。
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

/// 一個編譯好的命名匹配器。
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

    /// 求值。★ 同一匹配器內**多條件是 AND**；同一條件的多個值是 OR（G50）。
    ///
    /// `caps` 用來收 `path_regexp` 的捕獲組，給 `{path.N}` 用。
    /// ⚠ **只有整體匹配成功時 `caps` 才有意義**：中途失敗會留下已經寫進去的組，
    /// 所以呼叫方在失敗時必須丟棄它——這裡用「成功才提交」的寫法把這件事擋住。
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

    /// 沒有任何條件的匹配器命中一切。★ 單獨說一句是因為它容易被當成 bug：
    /// 一個空的 `@name { }` 在語義上就是「無條件」，而不是「永不命中」。
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
            // ★ DSL 允許 `path_regexp [name] <re>`：兩個值時第一個是名字。
            //   這裡取**最後**一個當正則，不取第一個——名字是可選的，
            //   而「取第一個」在帶名字時會把名字當正則去編譯，報一句莫名其妙的錯。
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
                // 第 0 組是整體匹配，`{path.N}` 說的是第 N 個**捕獲**組。
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
            // ★ 取不到用戶端 IP 時**判不命中**，不判命中。`remote_ip` 幾乎總是用來
            //   放行內網，寧可放行失敗也不能放行錯人。
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
        // path 命中但 method 不命中 → AND 失敗
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
        // ★ 反向：`k=v&flag=1` 裡不存在鍵 `lag`，儘管它是個子串
        let (s, _) = build(vec![cond("query", &["lag=1"], false)]);
        assert!(!s.matches(&req("/", "GET", &HeaderList(&[])), &mut c));
    }

    #[test]
    fn 捕获组只在整体命中时提交() {
        let (m, e) = build(vec![
            cond("path_regexp", &["^/u/([0-9]+)/(.*)$"], false),
            // 第二條一定不命中 → 整體失敗
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
        // ★ 寧可放行失敗，也不能放行錯人。
        assert!(!m.matches(&ctx, &mut c));
    }

    // ── 建置期檢核：DSL 層查不到的那些 ──────────────────────────────────────

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
        // ★ 機器寫的那一份不經過 DSL 詞法語法，所以這一層必須自己認。
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
