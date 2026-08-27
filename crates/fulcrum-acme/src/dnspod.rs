//! 原生 DNSPod（G57 两家之一 —— `example.com` 就在这家）。
//!
//! 用的是 dnsapi.cn 那套：**表单 POST**、`login_token=ID,Token`、`format=json`。
//!
//! | 干什么 | 请求 |
//! |---|---|
//! | 校验凭据（G59 第 2 条，**只能验「能用」，验不了范围**）| `POST /Info.Version` |
//! | 挂 TXT | `POST /Record.Create` |
//! | 查记录 | `POST /Record.List` |
//! | 摘 TXT | `POST /Record.Remove` |
//!
//! # ⚠ ⚠ 与 Cloudflare 的关键差别：**这份凭据是账号级的**
//!
//! DNSPod 的 token 覆盖账号下**全部**域名，没有「只给这个 zone」的形态，
//! 也没有能问出「它覆盖哪些 zone」的端点。⇒ **G59 第 3 条对它是硬要求**：
//! 配置里必须显式声明这份凭据对应哪些 zone，超出范围一律拒绝。
//! ★ 这不是把安全性做出来了，是把它**写下来了** —— 真正的权限仍然是账号级的，
//! 声明的价值在于越权那一刻是我们自己拒绝的，理由在配置里看得见。
//!
//! # ⚠ 凭据在 **body** 里，不在头里
//!
//! 一个「只挡 `Authorization` 头」的脱敏实现在这家上**完全失效**，
//! 而失效的表现是凭据出现在日志里、没有任何报错。
//! ⇒ [`crate::http::HttpRequest::redacted`] 连 body 都不打，只打长度。
//!
//! ⚠ 单测证明的是**内部自洽**；「我们对 dnsapi.cn 的理解是对的」只能靠真域名上的真签发。

use crate::credential::CredentialSource;
use crate::http::{HttpRequest, HttpTransport};
use crate::provider::VerifyError;
use log::debug;
use std::sync::Arc;

const API: &str = "https://dnsapi.cn";

/// 挑战记录的 TTL。
///
/// ⚠ ⚠ **这个值不能按「越小越好」拍，DNSPod 按套餐给 TTL 下限。**
/// 免费版的下限是 **600 秒**，写 60 会被直接拒：
/// `Record.Create` 回 `code 32 / Record TTL value exceeded limit`，
/// 于是**原生 DNSPod 在免费账号上一张证书都签不下来**。
///
/// ★ 在 `example.com` 上实测撞到的 —— 而门禁里**撞不到**：
/// 那里的对端是 pebble + challtestsrv，没有套餐这个概念。
/// 这正是 G57 把「我们对 dnsapi.cn 的理解是对的」押在真域名上的原因，
/// 而这一条恰恰是那句话里**理解错了的那一小块**。
///
/// ★ 取 600 而不是「按套餐探测」：TTL 只影响**递归解析器的缓存**，
/// 而 G58 的可见性判据是**直接问权威 NS**，CA 也一样 ⇒ 抬到 600 对签发几乎零成本，
/// 而它在所有套餐上都合法。**一个在所有账号上都能用的常量，胜过一次需要猜的探测。**
const TTL: &str = "600";
/// 表单里那个「线路」。DNSPod 必填，默认线路的名字就是这两个汉字。
const LINE: &str = "默认";

#[derive(Debug, Clone)]
pub struct Dnspod {
    transport: Arc<dyn HttpTransport>,
    source: CredentialSource,
    zones: Vec<String>,
    base: String,
}

impl Dnspod {
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        source: CredentialSource,
        zones: Vec<String>,
    ) -> Dnspod {
        Dnspod {
            transport,
            source,
            zones: zones.iter().map(|z| z.to_ascii_lowercase()).collect(),
            base: API.to_string(),
        }
    }

    #[cfg(test)]
    fn with_base(mut self, base: &str) -> Dnspod {
        self.base = base.to_string();
        self
    }

    /// 与 Cloudflare 那份**共用同一份语义**（G59 第 3 条）。
    ///
    /// ★ 共用的是 [`crate::zone_scope::zone_for`] 一份实现，不是各写一遍 ——
    /// 理由与 G66 同族：两份「按标签边界判归属」的实现迟早会分叉，
    /// 而分叉之后**两边都还是绿的**。
    pub fn zone_for(&self, record: &str) -> Option<&str> {
        crate::zone_scope::zone_for(&self.zones, record)
    }

    fn form(&self, extra: &[(&str, &str)]) -> Result<Vec<u8>, String> {
        let token = self.source.load()?;
        let mut pairs: Vec<(String, String)> = vec![
            ("login_token".into(), token),
            ("format".into(), "json".into()),
            ("lang".into(), "en".into()),
        ];
        for (k, v) in extra {
            pairs.push(((*k).to_string(), (*v).to_string()));
        }
        // ⚠ 必须 percent-encode：记录值是 base64url（含 `-` `_`，安全），
        //   但**域名与线路名不是** —— `默认` 是多字节，直接拼进去 body 就坏了。
        //   ★ 这里自己写编码而不是拉一个 crate：规则是 RFC 3986 的
        //   unreserved 集合，短、固定、没有歧义（与手写 DNS 客户端同一条判据）。
        let mut out = String::new();
        for (i, (k, v)) in pairs.iter().enumerate() {
            if i > 0 {
                out.push('&');
            }
            out.push_str(&form_encode(k));
            out.push('=');
            out.push_str(&form_encode(v));
        }
        Ok(out.into_bytes())
    }

    async fn call(&self, action: &str, extra: &[(&str, &str)]) -> Result<String, String> {
        let req = HttpRequest::post(
            format!("{}/{action}", self.base),
            "application/x-www-form-urlencoded",
            self.form(extra)?,
        )
        // dnsapi.cn 要求带 User-Agent，否则会直接拒。
        .header(
            "user-agent",
            "Fulcrum/0.0 (+https://github.com/ShanireZ/Fulcrum)",
        );
        let rsp = self.transport.send(req).await?;
        let text = rsp.body_text().to_string();
        if rsp.status != 200 {
            return Err(format!(
                "DNSPod {action} 失败（HTTP {}）：{}",
                rsp.status,
                brief(&text)
            ));
        }
        // ⚠ ⚠ **dnsapi.cn 永远回 HTTP 200**，真正的结果在 body 的 `status.code` 里
        //   （`"1"` 才是成功）。一个只看状态码的实现会把每一次失败都当成功 ——
        //   而症状是「TXT 一直没出现」，与这一步已经隔了很远。
        let code = status_code(&text).unwrap_or_default();
        if code != "1" {
            return Err(format!(
                "DNSPod {action} 被拒（code {code}）：{}",
                status_message(&text).unwrap_or_else(|| brief(&text))
            ));
        }
        Ok(text)
    }

    /// **G59 第 2 条**能做到的那一半：验凭据**能用**。
    ///
    /// ⚠ 验不了**范围** —— DNSPod 的 token 是账号级的，没有可问的端点。
    /// 那一半由 G59 第 3 条的显式 zone 声明顶上（`zones` 在编译期就是必填）。
    pub async fn verify(&self) -> Result<(), VerifyError> {
        // ⚠ 凭据读不出来是本机的、确定的问题 ⇒ Fatal。先单独读一次，
        //   否则它会被下面那层裹成「调用失败」，与网络不通混成一类。
        self.source.load().map_err(VerifyError::Fatal)?;
        match self.call("Info.Version", &[]).await {
            Ok(_) => {
                debug!("DNSPod 凭据可用（{}）", self.source.describe());
                Ok(())
            }
            Err(e) => {
                let msg = format!(
                    "DNSPod 拒绝了这份凭据（来自 {}）：{e}",
                    self.source.describe()
                );
                // ★ `call` 里 HTTP 层失败与「对端回话说不行」都走同一个 Err，
                //   这里按前缀分开。⚠ 这是本文件里唯一一处按串分类，
                //   而它有一条**自证**（下面 `网络不通判成_inconclusive_而不是_fatal`）。
                if e.starts_with("DNSPod Info.Version 被拒") || e.contains("HTTP ") {
                    Err(VerifyError::Fatal(msg))
                } else {
                    Err(VerifyError::Inconclusive(msg))
                }
            }
        }
    }

    /// 把 `_acme-challenge.a.example.com` 拆成 `(example.com, _acme-challenge.a)`。
    ///
    /// ★ DNSPod 的 API 要 `domain` + `sub_domain` 两段，而拆点**必须是声明过的 zone**，
    /// 不能靠「取最后两段」猜 —— `a.com.cn` 那种会拆错，而拆错的表现是
    /// 「记录挂到了另一个域上」。
    fn split(&self, record: &str) -> Result<(String, String), String> {
        let r = record.trim_end_matches('.').to_ascii_lowercase();
        let zone = self
            .zone_for(&r)
            .ok_or_else(|| crate::zone_scope::out_of_scope(record, &self.zones))?
            .to_string();
        let sub = if r == zone {
            "@".to_string()
        } else {
            r[..r.len() - zone.len() - 1].to_string()
        };
        Ok((zone, sub))
    }

    pub async fn set_txt(&self, name: &str, value: &str) -> Result<(), String> {
        let (domain, sub) = self.split(name)?;
        self.call(
            "Record.Create",
            &[
                ("domain", &domain),
                ("sub_domain", &sub),
                ("record_type", "TXT"),
                ("record_line", LINE),
                ("value", value),
                ("ttl", TTL),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn clear_txt(&self, name: &str, value: &str) -> Result<(), String> {
        let (domain, sub) = self.split(name)?;
        // 先按「名字 + 类型」列出来，再在本地按**值**挑中那一条。
        // ⚠ 同一个记录名下可能同时挂着多条 TXT（两张证书并行签发时就会），
        //   删错一条会让另一条签发失败。
        let listed = match self
            .call(
                "Record.List",
                &[
                    ("domain", &domain),
                    ("sub_domain", &sub),
                    ("record_type", "TXT"),
                ],
            )
            .await
        {
            Ok(t) => t,
            Err(e) => {
                // 列不出来就当已经没了 —— 这是清理，不是可用性。
                debug!("DNSPod 列不出 {name} 的 TXT（{e}），当作已经摘掉");
                return Ok(());
            }
        };
        let Some(id) = record_id_with_value(&listed, value) else {
            debug!("DNSPod 上没找到 {name} 值为该挑战的那条 TXT，当作已经摘掉");
            return Ok(());
        };
        self.call("Record.Remove", &[("domain", &domain), ("record_id", &id)])
            .await?;
        Ok(())
    }
}

/// RFC 3986 的 `application/x-www-form-urlencoded`：unreserved 直出，空格是 `+`，其余百分号编码。
fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn status_code(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("status")?
        .get("code")?
        .as_str()
        .map(|s| s.to_string())
}

fn status_message(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("status")?
        .get("message")?
        .as_str()
        .map(|s| s.to_string())
}

/// 在 `Record.List` 的结果里找**值等于** `value` 的那条，返回它的 id。
fn record_id_with_value(text: &str, value: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let records = v.get("records")?.as_array()?;
    records
        .iter()
        .find(|r| r.get("value").and_then(|x| x.as_str()) == Some(value))
        .and_then(|r| r.get("id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

fn brief(text: &str) -> String {
    text.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct Fake {
        seen: Mutex<Vec<HttpRequest>>,
        replies: Mutex<Vec<HttpResponse>>,
    }

    impl Fake {
        fn with(replies: Vec<(u16, &str)>) -> Arc<Fake> {
            Arc::new(Fake {
                seen: Mutex::new(Vec::new()),
                replies: Mutex::new(
                    replies
                        .into_iter()
                        .map(|(status, b)| HttpResponse {
                            status,
                            body: b.as_bytes().to_vec(),
                        })
                        .collect(),
                ),
            })
        }
        fn seen(&self) -> Vec<HttpRequest> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl HttpTransport for Fake {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String> {
            self.seen.lock().unwrap().push(req);
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                return Err("假实现的应答用完了".into());
            }
            Ok(r.remove(0))
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn dp(fake: Arc<Fake>, zones: &[&str]) -> Dnspod {
        unsafe { std::env::set_var("FULCRUM_TEST_DP_TOKEN", "12345,abcdef") };
        Dnspod::new(
            fake,
            CredentialSource::Env("FULCRUM_TEST_DP_TOKEN".into()),
            zones.iter().map(|z| z.to_string()).collect(),
        )
        .with_base("https://dnsapi.example")
    }

    fn body_of(req: &HttpRequest) -> String {
        String::from_utf8(req.body.clone().unwrap_or_default()).unwrap()
    }

    #[test]
    fn 表单编码按_rfc_3986_做而且多字节的线路名不会坏() {
        assert_eq!(form_encode("abc-_.~123"), "abc-_.~123");
        assert_eq!(form_encode("a b"), "a+b");
        assert_eq!(form_encode("12345,abcdef"), "12345%2Cabcdef");
        // ★ 「默认」是三字节 × 2。⚠ 直接拼进 body 的话 DNSPod 收到的是乱码，
        //   而它回的错误只说「线路不存在」。
        assert_eq!(form_encode(LINE), "%E9%BB%98%E8%AE%A4");
    }

    #[test]
    fn 拆域名按声明的_zone_拆而不是取最后两段() {
        // ⚠ 「取最后两段」在 `a.com.cn` 上会拆成 `com.cn` —— 记录会挂到别人的域上。
        let d = dp(Fake::with(vec![]), &["a.com.cn"]);
        assert_eq!(
            d.split("_acme-challenge.x.a.com.cn").unwrap(),
            ("a.com.cn".to_string(), "_acme-challenge.x".to_string())
        );
        // 裸 zone 自己 → `@`
        assert_eq!(
            d.split("a.com.cn").unwrap(),
            ("a.com.cn".to_string(), "@".to_string())
        );
    }

    #[test]
    fn 超出声明范围直接拒绝而且一个请求都不发() {
        let fake = Fake::with(vec![]);
        let d = dp(fake.clone(), &["example.com"]);
        let e = rt()
            .block_on(d.set_txt("_acme-challenge.example.net", "v"))
            .unwrap_err();
        assert!(e.contains("G59"), "{e}");
        assert!(fake.seen().is_empty(), "越权时不该发任何请求");
    }

    #[test]
    fn 永远回_200_的对端要看_body_里的_status_code() {
        // ★ ★ dnsapi.cn 永远回 HTTP 200。一个只看状态码的实现会把每一次失败
        //   都当成功，而症状是「TXT 一直没出现」，与这一步已经隔了很远。
        let fake = Fake::with(vec![(
            200,
            r#"{"status":{"code":"-1","message":"login token format error"}}"#,
        )]);
        let d = dp(fake, &["example.com"]);
        let e = rt()
            .block_on(d.set_txt("_acme-challenge.example.com", "v"))
            .unwrap_err();
        assert!(e.contains("code -1"), "{e}");
        assert!(
            e.contains("login token format error"),
            "对端的话要带出来：{e}"
        );
    }

    #[test]
    fn 挂_txt_的表单逐字段对() {
        let fake = Fake::with(vec![(
            200,
            r#"{"status":{"code":"1"},"record":{"id":"9"}}"#,
        )]);
        let d = dp(fake.clone(), &["example.com"]);
        rt().block_on(d.set_txt("_acme-challenge.example.com", "the-value"))
            .expect("应当成功");
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].url, "https://dnsapi.example/Record.Create");
        let b = body_of(&seen[0]);
        assert!(b.contains("login_token=12345%2Cabcdef"), "{b}");
        assert!(b.contains("format=json"), "{b}");
        assert!(b.contains("domain=example.com"), "{b}");
        assert!(b.contains("sub_domain=_acme-challenge"), "{b}");
        assert!(b.contains("record_type=TXT"), "{b}");
        assert!(b.contains("value=the-value"), "{b}");
        assert!(b.contains("ttl=600"), "{b}");
        assert!(b.contains("record_line=%E9%BB%98%E8%AE%A4"), "{b}");
    }

    #[test]
    fn 摘_txt_按值挑那一条() {
        // ⚠ 同名多条 TXT 时删错一条会让另一条签发失败。
        let fake = Fake::with(vec![
            (
                200,
                r#"{"status":{"code":"1"},"records":[
                    {"id":"1","value":"other"},
                    {"id":"2","value":"the-value"}
                ]}"#,
            ),
            (200, r#"{"status":{"code":"1"}}"#),
        ]);
        let d = dp(fake.clone(), &["example.com"]);
        rt().block_on(d.clear_txt("_acme-challenge.example.com", "the-value"))
            .expect("应当成功");
        let seen = fake.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].url, "https://dnsapi.example/Record.Remove");
        let b = body_of(&seen[1]);
        assert!(b.contains("record_id=2"), "挑错了那一条：{b}");
    }

    #[test]
    fn 摘_txt_时找不到就当已经摘了() {
        let fake = Fake::with(vec![(200, r#"{"status":{"code":"1"},"records":[]}"#)]);
        let d = dp(fake, &["example.com"]);
        rt().block_on(d.clear_txt("_acme-challenge.example.com", "v"))
            .expect("找不到不该是错误");
    }

    /// 一个「压根连不上」的 transport。★ 错误串刻意长得像 `HyperTransport`
    /// 真正发不出去时的那一条，否则这条自证测的就不是同一件事。
    #[derive(Debug)]
    struct Unreachable;

    #[async_trait::async_trait]
    impl HttpTransport for Unreachable {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String> {
            Err(format!(
                "请求失败（{}）：client error (Connect)",
                req.redacted()
            ))
        }
    }

    #[test]
    fn 网络不通判成_inconclusive_而不是_fatal() {
        // ★ ★ ★ 这条是 `verify()` 里那处**按错误串分类**的自证。
        //   ⚠ 两个方向都要测：只测 Fatal 那一半的话，一个「永远 Fatal」的实现
        //   照样绿——而它会让一次网络抖动把整台机器上所有站点都挡在启动之外。
        unsafe { std::env::set_var("FULCRUM_TEST_DP_TOKEN", "12345,abcdef") };
        let d = Dnspod::new(
            Arc::new(Unreachable),
            CredentialSource::Env("FULCRUM_TEST_DP_TOKEN".into()),
            vec!["example.com".into()],
        );
        let e = rt().block_on(d.verify()).unwrap_err();
        assert!(
            matches!(e, VerifyError::Inconclusive(_)),
            "连不上应当是 Inconclusive（打 error 继续），而不是 Fatal（拒绝启动）：{e}"
        );
    }

    #[test]
    fn 凭据读不出来是_fatal_而不是_inconclusive() {
        // ⚠ 反面：这是**本机的、确定的**问题，网络再好也不会变对。
        //   一个把它归成 Inconclusive 的实现会让「凭据文件没挂上」这种事
        //   变成一条 error 日志然后继续跑——而那台机器一张证书都签不出来。
        unsafe { std::env::remove_var("FULCRUM_TEST_DP_MISSING") };
        let d = Dnspod::new(
            Arc::new(Unreachable),
            CredentialSource::Env("FULCRUM_TEST_DP_MISSING".into()),
            vec!["example.com".into()],
        );
        let e = rt().block_on(d.verify()).unwrap_err();
        assert!(matches!(e, VerifyError::Fatal(_)), "{e}");
    }

    #[test]
    fn 校验失败的错误里有来源但没有值() {
        let fake = Fake::with(vec![(200, r#"{"status":{"code":"-1","message":"bad"}}"#)]);
        let d = dp(fake, &["example.com"]);
        let e = rt().block_on(d.verify()).unwrap_err();
        assert!(
            matches!(e, VerifyError::Fatal(_)),
            "对端回话说不行 ⇒ 必须是 Fatal：{e}"
        );
        let e = e.message().to_string();
        assert!(e.contains("FULCRUM_TEST_DP_TOKEN"), "{e}");
        assert!(!e.contains("abcdef"), "凭据的值漏进错误信息了：{e}");
    }
}
