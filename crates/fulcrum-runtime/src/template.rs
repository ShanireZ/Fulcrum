//! 占位符展开。**在构建期把字符串编译成段**，请求路径上只做拼接。
//!
//! ★ 为什么要预编译：`{host}` 这类东西出现在 `rewrite` / `redir` / `header` 的取值里，
//! 而这些是**每个请求都要走一遍**的。每次请求重新扫一遍字符串找花括号，
//! 是把配置解析的成本搬到了热路径上。
//!
//! ★ ★ **时间从参数进来，不从函数里取。** `{time}` 的取值是 `SystemTime`，
//! 由调用方传入而不是在这里 `SystemTime::now()`——否则这个函数就没法被确定性地测，
//! 而「判据不能挂在会抖的量上」是本仓库的一条老纪律。

use crate::request::{RequestCtx, ResponseCtx};
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Lit(String),
    Host,
    Uri,
    Path,
    Query,
    Method,
    Scheme,
    RemoteIp,
    RemotePort,
    Time,
    Header(String),
    RespHeader(String),
    /// `{path.N}` —— `path_regexp` 的第 N 个捕获组。
    Capture(usize),
    Status,
    Upstream,
}

/// 一个可能含占位符的取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segs: Vec<Seg>,
}

impl Template {
    /// 编译。★ 认不出来的花括号**原样留着**——配置层已经把「未知占位符」判成编译错误了
    /// （`FUL-DSL-0016`），能走到运行时的就只有字面量花括号（JSON 体那种）。
    /// ⚠ 这两层的分工必须写清楚：**这里不报错，是因为报错的地方在上游，不是因为不在乎**。
    pub fn parse(s: &str) -> Template {
        let b = s.as_bytes();
        let mut segs = Vec::new();
        let mut lit = String::new();
        let mut i = 0usize;
        while i < b.len() {
            if b[i] != b'{' {
                let ch = s[i..].chars().next().unwrap();
                lit.push(ch);
                i += ch.len_utf8();
                continue;
            }
            if i + 1 < b.len() && b[i + 1] == b'{' {
                lit.push('{'); // `{{` 是字面量
                i += 2;
                continue;
            }
            let Some(rel) = s[i + 1..].find('}') else {
                lit.push('{');
                i += 1;
                continue;
            };
            let inner = &s[i + 1..i + 1 + rel];
            match classify(inner) {
                Some(seg) => {
                    if !lit.is_empty() {
                        segs.push(Seg::Lit(std::mem::take(&mut lit)));
                    }
                    segs.push(seg);
                    i = i + 1 + rel + 1;
                }
                None => {
                    lit.push('{');
                    i += 1;
                }
            }
        }
        if !lit.is_empty() {
            segs.push(Seg::Lit(lit));
        }
        Template { segs }
    }

    /// 完全没有占位符时返回那段字面量——调用方可以省掉一次分配。
    pub fn as_literal(&self) -> Option<&str> {
        match self.segs.as_slice() {
            [] => Some(""),
            [Seg::Lit(s)] => Some(s),
            _ => None,
        }
    }

    /// 展开。
    ///
    /// ⚠ **取不到的值展开成空串**，而这**不是**在放任 G61 那条纪律：
    /// 「用在不可用的位置」已经被配置层判成编译错误；能走到这里的
    /// `{status}` / `{upstream}` 一定在允许的位置上，只是那一刻的值可能确实没有
    /// （例如上游根本没连上）。**那是「值为空」，不是「位置错了」。**
    pub fn expand(
        &self,
        req: &RequestCtx<'_>,
        resp: &ResponseCtx<'_>,
        caps: &[String],
        now: SystemTime,
    ) -> String {
        let mut out = String::new();
        for seg in &self.segs {
            match seg {
                Seg::Lit(s) => out.push_str(s),
                Seg::Host => out.push_str(req.host),
                Seg::Uri => out.push_str(&req.uri()),
                Seg::Path => out.push_str(req.path),
                Seg::Query => out.push_str(req.query),
                Seg::Method => out.push_str(req.method),
                Seg::Scheme => out.push_str(req.scheme),
                Seg::RemoteIp => {
                    if let Some(ip) = req.remote_ip {
                        let _ = write!(out, "{ip}");
                    }
                }
                Seg::RemotePort => {
                    let _ = write!(out, "{}", req.remote_port);
                }
                Seg::Time => out.push_str(&format_rfc3339(now)),
                Seg::Header(name) => {
                    if let Some(v) = req.headers.get(name) {
                        out.push_str(v);
                    }
                }
                Seg::RespHeader(name) => {
                    if let Some(h) = resp.headers
                        && let Some(v) = h.get(name)
                    {
                        out.push_str(v);
                    }
                }
                Seg::Capture(n) => {
                    // `{path.1}` 是第 1 个捕获组 → caps[0]
                    if let Some(v) = n.checked_sub(1).and_then(|i| caps.get(i)) {
                        out.push_str(v);
                    }
                }
                Seg::Status => {
                    if let Some(s) = resp.status {
                        let _ = write!(out, "{s}");
                    }
                }
                Seg::Upstream => {
                    if let Some(u) = resp.upstream {
                        out.push_str(u);
                    }
                }
            }
        }
        out
    }
}

fn classify(inner: &str) -> Option<Seg> {
    if let Some(name) = inner.strip_prefix("header.") {
        return (!name.is_empty()).then(|| Seg::Header(name.to_string()));
    }
    if let Some(name) = inner.strip_prefix("resp_header.") {
        return (!name.is_empty()).then(|| Seg::RespHeader(name.to_string()));
    }
    if let Some(n) = inner.strip_prefix("path.") {
        return n.parse::<usize>().ok().map(Seg::Capture);
    }
    Some(match inner {
        "host" => Seg::Host,
        "uri" => Seg::Uri,
        "path" => Seg::Path,
        "query" => Seg::Query,
        "method" => Seg::Method,
        "scheme" => Seg::Scheme,
        "remote_ip" => Seg::RemoteIp,
        "remote_port" => Seg::RemotePort,
        "time" => Seg::Time,
        "status" => Seg::Status,
        "upstream" => Seg::Upstream,
        _ => return None,
    })
}

/// UTC 的 RFC 3339，秒级精度。
///
/// ★ 手写而不是拉 `chrono` / `jiff`：只需要「Unix 秒 → 年月日时分秒」这一件事，
/// 算法是公开的（Howard Hinnant 的 `civil_from_days`），而且**可以被穷尽地测**——
/// 下面钉了 6 个已知时刻，含两个闰年边界。
pub fn format_rfc3339(t: SystemTime) -> String {
    format_rfc3339_inner(t, false)
}

/// UTC 的 RFC 3339，**毫秒**精度（**M2 批 L 第 ② 步**：访问日志的 `ts` 字段）。
///
/// # ⚠ ⚠ ★ 为什么它是**另一个函数**，而不是给 [`format_rfc3339`] 加一个参数
///
/// [`format_rfc3339`] 是 DSL 里 `{time}` 占位符的输出，而**那是一份公开契约**：
/// 给它加精度，就是悄悄改掉每一个用了 `{time}` 的人的输出 ——
/// 而那种改动不会让任何东西变红（它仍然是一个合法的 RFC 3339）。
/// ★ ★ 两者**共用同一段历法计算**（[`civil_from_days`]），
/// 所以「两处会漂」这件事在结构上做不到。
pub fn format_rfc3339_millis(t: SystemTime) -> String {
    format_rfc3339_inner(t, true)
}

fn format_rfc3339_inner(t: SystemTime, with_millis: bool) -> String {
    let (secs, millis) = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_millis()),
        // 1970 之前：本项目的日志里不该出现，但不能 panic。
        // ⚠ 这一支有意丢掉亚秒（它本来就只是个不该发生的兜底）。
        Err(e) => (-(e.duration().as_secs() as i64), 0),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let frac = if with_millis {
        format!(".{millis:03}")
    } else {
        String::new()
    };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{frac}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 从「1970-01-01 起的天数」算出 (年, 月, 日)。Hinnant 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::HeaderList;
    use std::time::Duration;

    fn req<'a>(headers: &'a HeaderList<'a>) -> RequestCtx<'a> {
        RequestCtx {
            host: "a.com",
            port: 443,
            scheme: "https",
            method: "POST",
            path: "/old/thing",
            query: "k=v",
            headers,
            remote_ip: Some("10.1.2.3".parse().unwrap()),
            remote_port: 51234,
        }
    }

    fn expand(t: &str, headers: &[(&str, &str)], caps: &[String]) -> String {
        let h = HeaderList(headers);
        Template::parse(t).expand(&req(&h), &ResponseCtx::default(), caps, UNIX_EPOCH)
    }

    #[test]
    fn 请求侧占位符全部展开() {
        assert_eq!(expand("{host}", &[], &[]), "a.com");
        assert_eq!(
            expand("{scheme}://{host}{uri}", &[], &[]),
            "https://a.com/old/thing?k=v"
        );
        assert_eq!(expand("{path}", &[], &[]), "/old/thing");
        assert_eq!(expand("{query}", &[], &[]), "k=v");
        assert_eq!(expand("{method}", &[], &[]), "POST");
        assert_eq!(
            expand("{remote_ip}:{remote_port}", &[], &[]),
            "10.1.2.3:51234"
        );
        assert_eq!(expand("{header.X-Foo}", &[("x-foo", "bar")], &[]), "bar");
    }

    #[test]
    fn 捕获组从_1_开始() {
        let caps = vec!["aa".to_string(), "bb".to_string()];
        assert_eq!(expand("{path.1}-{path.2}", &[], &caps), "aa-bb");
        // 越界不 panic，展开成空
        assert_eq!(expand("{path.3}", &[], &caps), "");
        // `{path.0}` 没有对应的组
        assert_eq!(expand("{path.0}", &[], &caps), "");
    }

    #[test]
    fn 响应侧占位符要等到有值才出来() {
        let t = Template::parse("{status}|{upstream}");
        let h = HeaderList(&[]);
        let r = req(&h);
        // 路由阶段：还没有值
        assert_eq!(t.expand(&r, &ResponseCtx::default(), &[], UNIX_EPOCH), "|");
        // 写响应阶段：有了
        let resp = ResponseCtx {
            status: Some(502),
            upstream: Some("10.0.0.1:8080"),
            headers: None,
        };
        assert_eq!(t.expand(&r, &resp, &[], UNIX_EPOCH), "502|10.0.0.1:8080");
    }

    #[test]
    fn 字面量花括号原样留着() {
        // ★ `respond 403 {"error":"nope"}` 里那对花括号不是占位符。
        assert_eq!(
            expand("{\"error\":\"nope\"}", &[], &[]),
            "{\"error\":\"nope\"}"
        );
        assert_eq!(expand("{{host}", &[], &[]), "{host}");
        assert_eq!(expand("{unclosed", &[], &[]), "{unclosed");
        assert_eq!(expand("{Host}", &[], &[]), "{Host}");
    }

    #[test]
    fn 纯字面量能免掉分配() {
        assert_eq!(
            Template::parse("/static/a.css").as_literal(),
            Some("/static/a.css")
        );
        assert_eq!(Template::parse("").as_literal(), Some(""));
        assert_eq!(Template::parse("{host}").as_literal(), None);
        assert_eq!(Template::parse("x{host}").as_literal(), None);
    }

    #[test]
    fn rfc3339_钉住六个已知时刻() {
        let at = |s: u64| format_rfc3339(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(1), "1970-01-01T00:00:01Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z"); // 闰年（能被 400 整除）
        assert_eq!(at(1_709_164_800), "2024-02-29T00:00:00Z"); // 闰年
        assert_eq!(at(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(at(2_147_483_647), "2038-01-19T03:14:07Z"); // 32 位边界
    }

    #[test]
    fn 毫秒那一版与秒那一版逐字同源() {
        // ★ ★ 判据取的是**关系**不是两个字面量：毫秒版必须等于「秒版去掉 Z + .mmm + Z」。
        //   ⚠ 各钉一个字面量的话，两者哪天漂开了，两条判据会**一起绿**。
        let t = UNIX_EPOCH + Duration::from_millis(1_787_000_000_123);
        let s = format_rfc3339(t);
        let ms = format_rfc3339_millis(t);
        assert_eq!(ms, format!("{}.123Z", s.trim_end_matches('Z')));
        // 整秒时刻也要有那三位（定长，日志里对齐好看）。
        // ⚠ ⚠ 这里**有意不写字面量**：手算出来的时刻字面量
        //   而它错了七天。★ 那个字面量不带任何本函数的信息 ——
        //   「年月日算得对不对」由上面 `rfc3339_钉住六个已知时刻` 独立守着，
        //   本条要守的只是「毫秒那一版比秒那一版多了定长的三位」。
        //   ⇒ **判据只断言它自己负责的那一段**，别顺手把别人的责任也抄一份进来。
        let t2 = UNIX_EPOCH + Duration::from_secs(1_787_000_000);
        let ms2 = format_rfc3339_millis(t2);
        assert!(ms2.ends_with(".000Z"), "整秒时刻也要有三位：{ms2}");
        assert_eq!(
            ms2,
            format!("{}.000Z", format_rfc3339(t2).trim_end_matches('Z'))
        );
    }

    #[test]
    fn rfc3339_不在_1970_之前_panic() {
        let t = UNIX_EPOCH - Duration::from_secs(86_400);
        assert_eq!(format_rfc3339(t), "1969-12-31T00:00:00Z");
    }
}
