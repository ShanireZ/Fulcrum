//! 原生 DNSPod（G57 兩家之一 —— `example.com` 就在這家）。
//!
//! 用的是 dnsapi.cn 那套：**表單 POST**、`login_token=ID,Token`、`format=json`。
//!
//! | 幹什麼 | 請求 |
//! |---|---|
//! | 檢核憑證（G59 第 2 條，**只能驗「能用」，驗不了範圍**）| `POST /Info.Version` |
//! | 掛 TXT | `POST /Record.Create` |
//! | 查記錄 | `POST /Record.List` |
//! | 摘 TXT | `POST /Record.Remove` |
//!
//! # ⚠ ⚠ 與 Cloudflare 的關鍵差別：**這份憑證是帳號級的**
//!
//! DNSPod 的 token 覆蓋帳號下**全部**網域，沒有「只給這個 zone」的形態，
//! 也沒有能問出「它覆蓋哪些 zone」的端點。⇒ **G59 第 3 條對它是硬要求**：
//! 設定裡必須顯式宣告這份憑證對應哪些 zone，超出範圍一律拒絕。
//! ★ 這不是把安全性做出來了，是把它**寫下來了** —— 真正的權限仍然是帳號級的，
//! 宣告的價值在於越權那一刻是我們自己拒絕的，理由在設定裡看得見。
//!
//! # ⚠ 憑證在 **body** 裡，不在頭裡
//!
//! 一個「只擋 `Authorization` 頭」的脫敏實作在這家上**完全失效**，
//! 而失效的表現是憑證出現在紀錄裡、沒有任何回報錯誤。
//! ⇒ [`crate::http::HttpRequest::redacted`] 連 body 都不打，只打長度。
//!
//! ⚠ 單測證明的是**內部自洽**；「我們對 dnsapi.cn 的理解是對的」只能靠真網域上的真簽發。

use crate::credential::CredentialSource;
use crate::http::{HttpRequest, HttpTransport};
use crate::provider::VerifyError;
use log::debug;
use std::sync::Arc;

const API: &str = "https://dnsapi.cn";

/// 挑戰記錄的 TTL。
///
/// ⚠ ⚠ **這個值不能按「越小越好」拍，DNSPod 按套餐給 TTL 下限。**
/// 免費版的下限是 **600 秒**，寫 60 會被直接拒：
/// `Record.Create` 回 `code 32 / Record TTL value exceeded limit`，
/// 於是**原生 DNSPod 在免費帳號上一張證書都簽不下來**。
///
/// ★ 在 `example.com` 上實測撞到的 —— 而門禁裡**撞不到**：
/// 那裡的對端是 pebble + challtestsrv，沒有套餐這個概念。
/// 這正是 G57 把「我們對 dnsapi.cn 的理解是對的」押在真網域上的原因，
/// 而這一條恰恰是那句話裡**理解錯了的那一小塊**。
///
/// ★ 取 600 而不是「按套餐探測」：TTL 只影響**遞歸解析器的快取**，
/// 而 G58 的可見性判據是**直接問權威 NS**，CA 也一樣 ⇒ 抬到 600 對簽發幾乎零成本，
/// 而它在所有套餐上都合法。**一個在所有帳號上都能用的常數，勝過一次需要猜的探測。**
const TTL: &str = "600";
/// 表單裡那個「線路」。DNSPod 必填，預設線路的名字就是這兩個漢字。
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

    /// 與 Cloudflare 那份**共用同一份語義**（G59 第 3 條）。
    ///
    /// ★ 共用的是 [`crate::zone_scope::zone_for`] 一份實作，不是各寫一遍 ——
    /// 理由與 G66 同族：兩份「按標籤邊界判歸屬」的實作遲早會分叉，
    /// 而分叉之後**兩邊都還是綠的**。
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
        // ⚠ 必須 percent-encode：記錄值是 base64url（含 `-` `_`，安全），
        //   但**網域與線路名不是** —— `默认` 是多位元組，直接拼進去 body 就壞了。
        //   ★ 這裡自己寫編碼而不是拉一個 crate：規則是 RFC 3986 的
        //   unreserved 集合，短、固定、沒有歧義（與手寫 DNS 用戶端同一條判據）。
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
        // dnsapi.cn 要求帶 User-Agent，否則會直接拒。
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
        // ⚠ ⚠ **dnsapi.cn 永遠回 HTTP 200**，真正的結果在 body 的 `status.code` 裡
        //   （`"1"` 才是成功）。一個只看狀態碼的實作會把每一次失敗都當成功 ——
        //   而症狀是「TXT 一直沒出現」，與這一步已經隔了很遠。
        let code = status_code(&text).unwrap_or_default();
        if code != "1" {
            return Err(format!(
                "DNSPod {action} 被拒（code {code}）：{}",
                status_message(&text).unwrap_or_else(|| brief(&text))
            ));
        }
        Ok(text)
    }

    /// **G59 第 2 條**能做到的那一半：驗憑證**能用**。
    ///
    /// ⚠ 驗不了**範圍** —— DNSPod 的 token 是帳號級的，沒有可問的端點。
    /// 那一半由 G59 第 3 條的顯式 zone 宣告頂上（`zones` 在編譯期就是必填）。
    pub async fn verify(&self) -> Result<(), VerifyError> {
        // ⚠ 憑證讀不出來是本機的、確定的問題 ⇒ Fatal。先單獨讀一次，
        //   否則它會被下面那層裹成「呼叫失敗」，與網路不通混成一類。
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
                // ★ `call` 裡 HTTP 層失敗與「對端回話說不行」都走同一個 Err，
                //   這裡按前綴分開。⚠ 這是本檔案裡唯一一處按串分類，
                //   而它有一條**自證**（下面 `网络不通判成_inconclusive_而不是_fatal`）。
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
    /// ★ DNSPod 的 API 要 `domain` + `sub_domain` 兩段，而拆點**必須是宣告過的 zone**，
    /// 不能靠「取最後兩段」猜 —— `a.com.cn` 那種會拆錯，而拆錯的表現是
    /// 「記錄掛到了另一個域上」。
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
        // 先按「名字 + 類型」列出來，再在本機按**值**挑中那一條。
        // ⚠ 同一個記錄名下可能同時掛著多條 TXT（兩張證書並行簽發時就會），
        //   刪錯一條會讓另一條簽發失敗。
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
                // 列不出來就當已經沒了 —— 這是清理，不是可用性。
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

/// RFC 3986 的 `application/x-www-form-urlencoded`：unreserved 直出，空格是 `+`，其餘百分號編碼。
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

/// 在 `Record.List` 的結果裡找**值等於** `value` 的那條，回傳它的 id。
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
        // ★ 「預設」是三位元組 × 2。⚠ 直接拼進 body 的話 DNSPod 收到的是亂碼，
        //   而它回的錯誤只說「線路不存在」。
        assert_eq!(form_encode(LINE), "%E9%BB%98%E8%AE%A4");
    }

    #[test]
    fn 拆域名按声明的_zone_拆而不是取最后两段() {
        // ⚠ 「取最後兩段」在 `a.com.cn` 上會拆成 `com.cn` —— 記錄會掛到別人的域上。
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
        // ★ ★ dnsapi.cn 永遠回 HTTP 200。一個只看狀態碼的實作會把每一次失敗
        //   都當成功，而症狀是「TXT 一直沒出現」，與這一步已經隔了很遠。
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
        // ⚠ 同名多條 TXT 時刪錯一條會讓另一條簽發失敗。
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

    /// 一個「壓根連不上」的 transport。★ 錯誤串刻意長得像 `HyperTransport`
    /// 真正發不出去時的那一條，否則這條自證測的就不是同一件事。
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
        // ★ ★ ★ 這條是 `verify()` 裡那處**按錯誤串分類**的自證。
        //   ⚠ 兩個方向都要測：只測 Fatal 那一半的話，一個「永遠 Fatal」的實作
        //   照樣綠——而它會讓一次網路抖動把整臺機器上所有站台都擋在啟動之外。
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
        // ⚠ 反面：這是**本機的、確定的**問題，網路再好也不會變對。
        //   一個把它歸成 Inconclusive 的實作會讓「憑證檔案沒掛上」這種事
        //   變成一條 error 紀錄然後繼續跑——而那臺機器一張證書都簽不出來。
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
