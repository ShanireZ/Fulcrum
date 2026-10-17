//! 佔位符展開。**在建置期把字串編譯成段**，請求路徑上只做拼接。
//!
//! ★ 為什麼要預編譯：`{host}` 這類東西出現在 `rewrite` / `redir` / `header` 的取值裡，
//! 而這些是**每個請求都要走一遍**的。每次請求重新掃一遍字串找花括號，
//! 是把設定解析的成本搬到了熱路徑上。
//!
//! ★ ★ **時間從參數進來，不從函式里取。** `{time}` 的取值是 `SystemTime`，
//! 由呼叫方傳入而不是在這裡 `SystemTime::now()`——否則這個函式就沒法被確定性地測，
//! 而「判據不能掛在會抖的量上」是本儲存庫的一條老紀律。

use crate::request::{RequestCtx, ResponseCtx};
use std::fmt::Write as _;
use std::sync::Arc;
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
    /// `{path.N}` —— `path_regexp` 的第 N 個捕獲組。
    Capture(usize),
    Status,
    Upstream,
}

/// 一個可能含佔位符的取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segs: Vec<Seg>,
    /// 整段都是字面值時，那段字面值的一份**共享**副本（parse 時建一次，由 `segs` 派生）。
    literal: Option<Arc<[u8]>>,
}

impl Template {
    /// 編譯。★ 認不出來的花括號**原樣留著**——設定層已經把「未知佔位符」判成編譯錯誤了
    /// （`FUL-DSL-0016`），能走到執行環境的就只有字面值花括號（JSON 體那種）。
    /// ⚠ 這兩層的分工必須寫清楚：**這裡不回報錯誤，是因為回報錯誤的地方在上游，不是因為不在乎**。
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
                lit.push('{'); // `{{` 是字面值
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
        let mut t = Template {
            segs,
            literal: None,
        };
        // ★ 「是不是純字面值」只有 `as_literal` 一處說了算，這裡照它派生，⛔ 不另寫一遍。
        t.literal = t.as_literal().map(|s| Arc::from(s.as_bytes()));
        t
    }

    /// 完全沒有佔位符時回傳那段字面值——呼叫方可以省掉一次分配。
    pub fn as_literal(&self) -> Option<&str> {
        match self.segs.as_slice() {
            [] => Some(""),
            [Seg::Lit(s)] => Some(s),
            _ => None,
        }
    }

    /// 與 [`Template::as_literal`] 同一段位元組，只是裝載時就放進了 `Arc`。
    ///
    /// ★ 給資料面當回應正文用：拿它包成 `Bytes`，每個請求就不必再把字面值拷一遍
    ///   （`respond` 那條路，2026-09-24 HTTP 層診斷的發現 2）。
    pub fn shared_literal(&self) -> Option<&Arc<[u8]>> {
        self.literal.as_ref()
    }

    /// 展開。
    ///
    /// ⚠ **取不到的值展開成空串**，而這**不是**在放任 G61 那條紀律：
    /// 「用在不可用的位置」已經被設定層判成編譯錯誤；能走到這裡的
    /// `{status}` / `{upstream}` 一定在允許的位置上，只是那一刻的值可能確實沒有
    /// （例如上游根本沒連上）。**那是「值為空」，不是「位置錯了」。**
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
                    // `{path.1}` 是第 1 個捕獲組 → caps[0]
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

/// UTC 的 RFC 3339，秒級精度。
///
/// ★ 手寫而不是拉 `chrono` / `jiff`：只需要「Unix 秒 → 年月日時分秒」這一件事，
/// 演算法是公開的（Howard Hinnant 的 `civil_from_days`），而且**可以被窮盡地測**——
/// 下面釘了 6 個已知時刻，含兩個閏年邊界。
pub fn format_rfc3339(t: SystemTime) -> String {
    format_rfc3339_inner(t, false)
}

/// UTC 的 RFC 3339，**毫秒**精度（**M2 批 L 第 ② 步**：訪問紀錄的 `ts` 欄位）。
///
/// # ⚠ ⚠ ★ 為什麼它是**另一個函式**，而不是給 [`format_rfc3339`] 加一個參數
///
/// [`format_rfc3339`] 是 DSL 裡 `{time}` 佔位符的輸出，而**那是一份公開契約**：
/// 給它加精度，就是悄悄改掉每一個用了 `{time}` 的人的輸出 ——
/// 而那種改動不會讓任何東西變紅（它仍然是一個合法的 RFC 3339）。
/// ★ ★ 兩者**共用同一段曆法計算**（[`civil_from_days`]），
/// 所以「兩處會漂」這件事在結構上做不到。
pub fn format_rfc3339_millis(t: SystemTime) -> String {
    format_rfc3339_inner(t, true)
}

fn format_rfc3339_inner(t: SystemTime, with_millis: bool) -> String {
    let (secs, millis) = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_millis()),
        // 1970 之前：本專案的紀錄裡不該出現，但不能 panic。
        // ⚠ 這一支有意丟掉亞秒（它本來就只是個不該發生的備援）。
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

/// 從「1970-01-01 起的天數」算出 (年, 月, 日)。Hinnant 演算法。
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
        // 越界不 panic，展開成空
        assert_eq!(expand("{path.3}", &[], &caps), "");
        // `{path.0}` 沒有對應的組
        assert_eq!(expand("{path.0}", &[], &caps), "");
    }

    #[test]
    fn 响应侧占位符要等到有值才出来() {
        let t = Template::parse("{status}|{upstream}");
        let h = HeaderList(&[]);
        let r = req(&h);
        // 路由階段：還沒有值
        assert_eq!(t.expand(&r, &ResponseCtx::default(), &[], UNIX_EPOCH), "|");
        // 寫回應階段：有了
        let resp = ResponseCtx {
            status: Some(502),
            upstream: Some("10.0.0.1:8080"),
            headers: None,
        };
        assert_eq!(t.expand(&r, &resp, &[], UNIX_EPOCH), "502|10.0.0.1:8080");
    }

    #[test]
    fn 字面量花括号原样留着() {
        // ★ `respond 403 {"error":"nope"}` 裡那對花括號不是佔位符。
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

    /// ★ 共享副本是**派生**出來的：它與 `as_literal` 必須逐位元組相同、有無一致。
    #[test]
    fn 共享字面量与_as_literal_是同一段字节() {
        for s in ["", "plain", "/static/a.css", "{{host}", "{host}", "x{host}"] {
            let t = Template::parse(s);
            assert_eq!(
                t.shared_literal().map(|b| &b[..]),
                t.as_literal().map(str::as_bytes),
                "{s:?}"
            );
        }
    }

    #[test]
    fn rfc3339_钉住六个已知时刻() {
        let at = |s: u64| format_rfc3339(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(1), "1970-01-01T00:00:01Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z"); // 閏年（能被 400 整除）
        assert_eq!(at(1_709_164_800), "2024-02-29T00:00:00Z"); // 閏年
        assert_eq!(at(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(at(2_147_483_647), "2038-01-19T03:14:07Z"); // 32 位邊界
    }

    #[test]
    fn 毫秒那一版与秒那一版逐字同源() {
        // ★ ★ 判據取的是**關係**不是兩個字面值：毫秒版必須等於「秒版去掉 Z + .mmm + Z」。
        //   ⚠ 各釘一個字面值的話，兩者哪天漂開了，兩條判據會**一起綠**。
        let t = UNIX_EPOCH + Duration::from_millis(1_787_000_000_123);
        let s = format_rfc3339(t);
        let ms = format_rfc3339_millis(t);
        assert_eq!(ms, format!("{}.123Z", s.trim_end_matches('Z')));
        // 整秒時刻也要有那三位（定長，紀錄裡對齊好看）。
        // ⚠ ⚠ 這裡**有意不寫字面值**：手算出來的時刻字面值
        //   而它錯了七天。★ 那個字面值不帶任何本函式的資訊 ——
        //   「年月日算得對不對」由上面 `rfc3339_钉住六个已知时刻` 獨立守著，
        //   本條要守的只是「毫秒那一版比秒那一版多了定長的三位」。
        //   ⇒ **判據只斷言它自己負責的那一段**，別順手把別人的責任也抄一份進來。
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
