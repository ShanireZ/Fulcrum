//! 原生 Cloudflare（G57 兩家之一 —— `example.net` 就在這家）。
//!
//! 用的是 API v4 的 bearer token 形態：
//!
//! | 幹什麼 | 請求 |
//! |---|---|
//! | 檢核憑證（**G59 第 2 條**）| `GET /client/v4/zones?name=<声明的每个 zone>` |
//! | 查 zone id | `GET /client/v4/zones?name=<zone>` |
//! | 掛 TXT | `POST /client/v4/zones/<id>/dns_records` |
//! | 摘 TXT | `DELETE /client/v4/zones/<id>/dns_records/<record>` |
//!
//! # ⚠ ⚠ 這個模組的判據到哪兒為止
//!
//! 單測能證明的是**內部自洽**：我們發出去的 URL / 方法 / 頭 / body 正是我們以為的那些，
//! 給定一段文件裡那種形狀的回應、解析出來的東西正是我們以為的那些。
//!
//! > **它證不了「我們對 Cloudflare API 的理解是對的」** —— 那要拿真的對端來測，
//! > 而門禁裡沒有。★ 那一條的判據是真網域上的一次真簽發（G57）。
//!
//! # ★ 摘 TXT 失敗不讓簽發失敗
//!
//! 與 exec hook 那條同一個理由：證書已經到手了，一條留在那裡的 TXT 是**衛生問題**
//! 不是可用性問題。升級成錯誤會把一次**成功**的簽發記成失敗 ⇒ 進退避、進計數、進告警。

use crate::credential::CredentialSource;
use crate::http::{HttpRequest, HttpTransport};
use crate::provider::VerifyError;
use log::debug;
use std::sync::Arc;

const API: &str = "https://api.cloudflare.com/client/v4";

/// TXT 記錄的 TTL。★ 取最小值：挑戰記錄活不過一次簽發，
/// 而 TTL 大了會讓**下一次**簽發被快取裡的舊值絆住。
const TTL: u32 = 60;

#[derive(Debug, Clone)]
pub struct Cloudflare {
    transport: Arc<dyn HttpTransport>,
    source: CredentialSource,
    /// 這份憑證被**宣告**覆蓋哪些 zone（G59 第 3 條）。
    zones: Vec<String>,
    /// 覆蓋 API 根，只給測試用。⚠ 產品裡沒有任何設定項能改它。
    base: String,
}

impl Cloudflare {
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        source: CredentialSource,
        zones: Vec<String>,
    ) -> Cloudflare {
        Cloudflare {
            transport,
            source,
            zones: zones.iter().map(|z| z.to_ascii_lowercase()).collect(),
            base: API.to_string(),
        }
    }

    #[cfg(test)]
    fn with_base(mut self, base: &str) -> Cloudflare {
        self.base = base.to_string();
        self
    }

    fn auth(&self) -> Result<String, String> {
        Ok(format!("Bearer {}", self.source.load()?))
    }

    /// 這條記錄名落在被宣告的 zone 裡嗎（**G59 第 3 條**）。
    ///
    /// ★ 與 DNSPod 那份**共用同一份實作**（[`crate::zone_scope::zone_for`]），
    /// 不是各寫一遍——理由與 G66 同族：兩份遲早會分叉，而分叉之後兩邊都還是綠的。
    pub fn zone_for(&self, record: &str) -> Option<&str> {
        crate::zone_scope::zone_for(&self.zones, record)
    }

    /// **G59 第 2 條**：啟動時檢核憑證。
    ///
    /// # ★ ★ ★ 驗的是「我們接下來真要做的那件事」，不是「有沒有某個介面認它」
    ///
    /// 在 `example.net` 上撞到的：本方法原先打的是
    /// `GET /user/tokens/verify` —— 而 Cloudflare 2026 年起把權杖分成了
    /// **使用者級 `cfut_`** 與 **帳號級 `cfat_`** 兩種，後者在使用者級端點上**必然**
    /// 回 `1000 Invalid API Token`。⚠ 而 G59 把「對端說不行」判成 Fatal ⇒
    /// **一把完全好用的帳號級權杖會讓樞衡拒絕啟動**，錯誤資訊還指著「憑證不可用」。
    ///
    /// ⇒ 改成對**宣告過的每一個 zone** 做一次 `GET /zones?name=<zone>`。這條判據嚴格更強：
    ///
    /// | 它順帶證明了 | 原來那條證不了 |
    /// |---|---|
    /// | 權杖真的能用（兩種權杖都走得通）| — |
    /// | 權杖**夠得著這個 zone**（Zone:Read 有、範圍包含它）| ✅ 原來要等第一次簽發才發現 |
    /// | `zones` 宣告裡的名字**真的存在**（打錯一個字母立刻現形）| ✅ 同上 |
    ///
    /// ★ 「查 zone id」正是簽發路徑上的第一步（見 [`Self::zone_id`]），
    /// 所以這條檢核與真實用法**共用同一個請求形狀** —— 檢核過而簽發掛不了。
    pub async fn verify(&self) -> Result<(), VerifyError> {
        // ⚠ 憑證讀不出來是**本機的、確定的**問題 ⇒ Fatal，不是 Inconclusive。
        let auth = self.auth().map_err(VerifyError::Fatal)?;
        // ⚠ 一個 zone 都沒宣告時不該悄悄通過：G59 第 3 條要求原生供應商必填 zones，
        //   走到這裡說明上游檢核漏了，說出來比默默放行強。
        if self.zones.is_empty() {
            return Err(VerifyError::Fatal(
                "没有声明 `zones` —— 原生供应商必填（G59 第 3 条），无从校验".to_string(),
            ));
        }
        for zone in &self.zones {
            let req = HttpRequest::get(format!("{}/zones?name={zone}", self.base))
                .header("authorization", auth.clone())
                .header("accept", "application/json");
            // 發不出去 / 收不到 ⇒ 分不出是對端掛了還是我們這兒網不通 ⇒ Inconclusive。
            let rsp = self
                .transport
                .send(req)
                .await
                .map_err(VerifyError::Inconclusive)?;
            let text = rsp.body_text();
            if rsp.status != 200 {
                return Err(VerifyError::Fatal(format!(
                    "Cloudflare 拒绝了这份凭据（HTTP {}，来自 {}）：{}",
                    rsp.status,
                    self.source.describe(),
                    first_error(&text)
                )));
            }
            // ⚠ 光看 200 不夠：Cloudflare 在**成功的 HTTP 狀態裡**也會回傳 `success:false`。
            //   一個只看狀態碼的實作會把一份壞憑證判成好的，而症狀要到第一次簽發才出現。
            if !text.contains("\"success\":true") && !text.contains("\"success\": true") {
                return Err(VerifyError::Fatal(format!(
                    "Cloudflare 说这份凭据不可用（来自 {}）：{}",
                    self.source.describe(),
                    first_error(&text)
                )));
            }
            // ★ ★ success:true **但列不出這個 zone** 是一種獨立的失敗，且最常見：
            //   權杖有效、但沒有 Zone:Read，或者資源範圍裡沒圈上這個 zone，
            //   又或者 `zones` 裡的名字打錯了。⚠ 三種的處置都是「現在就說」，
            //   而不是等第一次簽發時得到一句「查 zone 失敗」。
            if json_str_field(&text, "id").is_none() {
                return Err(VerifyError::Fatal(format!(
                    "这份凭据（来自 {}）列不出 zone {zone} —— \
                     要么它没有 Zone:Read / 资源范围不含这个 zone，\
                     要么 `zones {zone}` 这个名字写错了",
                    self.source.describe()
                )));
            }
            debug!(
                "Cloudflare 凭据对 zone {zone} 校验通过（{}）",
                self.source.describe()
            );
        }
        Ok(())
    }

    async fn zone_id(&self, zone: &str) -> Result<String, String> {
        let req = HttpRequest::get(format!("{}/zones?name={zone}", self.base))
            .header("authorization", self.auth()?)
            .header("accept", "application/json");
        let rsp = self.transport.send(req).await?;
        let text = rsp.body_text();
        if rsp.status != 200 {
            return Err(format!(
                "查 zone {zone} 失败（HTTP {}）：{}",
                rsp.status,
                first_error(&text)
            ));
        }
        json_str_field(&text, "id").ok_or_else(|| {
            format!("Cloudflare 没有返回 zone {zone} 的 id —— 这份凭据多半看不到这个 zone")
        })
    }

    pub async fn set_txt(&self, name: &str, value: &str) -> Result<(), String> {
        let zone = self
            .zone_for(name)
            .ok_or_else(|| crate::zone_scope::out_of_scope(name, &self.zones))?
            .to_string();
        let id = self.zone_id(&zone).await?;
        // ⚠ 用 serde_json 拼 body，不手拼字串：值是 base64url，雖然不含引號，
        //   但「這次不含」不是判據 —— 手拼的注入面永遠只差一個新呼叫點。
        let body = serde_json::json!({
            "type": "TXT",
            "name": name,
            "content": value,
            "ttl": TTL,
        });
        let req = HttpRequest::post(
            format!("{}/zones/{id}/dns_records", self.base),
            "application/json",
            serde_json::to_vec(&body).map_err(|e| format!("拼 JSON 失败：{e}"))?,
        )
        .header("authorization", self.auth()?);
        let rsp = self.transport.send(req).await?;
        if rsp.status != 200 && rsp.status != 201 {
            return Err(format!(
                "Cloudflare 挂 TXT 失败（HTTP {}）：{}",
                rsp.status,
                first_error(&rsp.body_text())
            ));
        }
        Ok(())
    }

    pub async fn clear_txt(&self, name: &str, value: &str) -> Result<(), String> {
        let zone = self
            .zone_for(name)
            .ok_or_else(|| crate::zone_scope::out_of_scope(name, &self.zones))?
            .to_string();
        let id = self.zone_id(&zone).await?;
        // 先查出 record id。★ 帶上 content：同一個記錄名下可能同時掛著**多條** TXT
        //   （兩張證書並行簽發時就會），刪錯一條會讓另一條簽發失敗。
        let req = HttpRequest::get(format!(
            "{}/zones/{id}/dns_records?type=TXT&name={name}&content={value}",
            self.base
        ))
        .header("authorization", self.auth()?)
        .header("accept", "application/json");
        let rsp = self.transport.send(req).await?;
        let text = rsp.body_text();
        let Some(rec) = json_str_field(&text, "id") else {
            // 查不到就當已經沒了 —— 這是清理，不是可用性。
            debug!("Cloudflare 上没找到 {name} 的那条 TXT，当作已经摘掉");
            return Ok(());
        };
        let req = HttpRequest::delete(format!("{}/zones/{id}/dns_records/{rec}", self.base))
            .header("authorization", self.auth()?);
        let rsp = self.transport.send(req).await?;
        if rsp.status != 200 {
            return Err(format!(
                "Cloudflare 摘 TXT 失败（HTTP {}）：{}",
                rsp.status,
                first_error(&rsp.body_text())
            ));
        }
        Ok(())
    }
}

/// 從一段 JSON 裡摳出第一個 `"<field>": "<值>"`。
///
/// ⚠ ⚠ **這是一個刻意做小的取值器，不是 JSON 解析器**，而它的取捨要說清楚：
/// 這幾家的回應裡 `result` 是陣列或物件，我們只要裡面第一條的 `id`。
/// 用 `serde_json::Value` 走一遍也行，但那要為每家的嵌套結構寫一份路徑，
/// 而路徑寫錯的表現與這裡取錯的表現完全一樣。
/// ★ 所以判據不在「怎麼取」，在**「取出來的東西對不對」由單測逐欄位釘住**。
fn json_str_field(text: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    fn walk(v: &serde_json::Value, field: &str) -> Option<String> {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(serde_json::Value::String(s)) = m.get(field) {
                    return Some(s.clone());
                }
                m.values().find_map(|x| walk(x, field))
            }
            serde_json::Value::Array(a) => a.iter().find_map(|x| walk(x, field)),
            _ => None,
        }
    }
    walk(&v, field)
}

/// 把對端給的第一條錯誤訊息帶出來。★ 一個只說「失敗」的錯誤，
/// 等於讓人自己去猜是憑證錯了、zone 不對、還是被限流了。
fn first_error(text: &str) -> String {
    if let Some(m) = json_str_field(text, "message") {
        return m;
    }
    text.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpResponse, HttpTransport};
    use std::sync::Mutex;

    /// 記錄型假實作。★ 它證明的是**內部自洽**，不是「我們對它家 API 的理解是對的」。
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
                return Err("假实现的应答用完了 —— 说明发出去的请求比预期多".into());
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

    fn cf(fake: Arc<Fake>, zones: &[&str]) -> Cloudflare {
        // SAFETY-ish：測試處理程序內設環境變數。★ 用一個測試專用的名字，避免與別的測試互相踩。
        unsafe { std::env::set_var("FULCRUM_TEST_CF_TOKEN", "tok-abc") };
        Cloudflare::new(
            fake,
            CredentialSource::Env("FULCRUM_TEST_CF_TOKEN".into()),
            zones.iter().map(|z| z.to_string()).collect(),
        )
        .with_base("https://api.example/client/v4")
    }

    // ── G59 第 3 條：zone 範圍 ───────────────────────────────────────────────

    #[test]
    fn zone_范围按标签边界判而不是后缀() {
        // ★ ★ 這條判據存在的全部理由，就是不讓憑證被用在沒宣告的域上。
        //   ⚠ `ends_with("example.net")` 會把 `evilexample.net` 算進來 —— 那正是要防的。
        let c = cf(Fake::with(vec![]), &["example.net"]);
        assert_eq!(
            c.zone_for("_acme-challenge.example.net"),
            Some("example.net")
        );
        assert_eq!(c.zone_for("example.net"), Some("example.net"));
        assert_eq!(c.zone_for("a.b.example.net"), Some("example.net"));
        assert_eq!(
            c.zone_for("evilexample.net"),
            None,
            "后缀匹配把别人的域算进来了"
        );
        assert_eq!(c.zone_for("example.net.evil.com"), None);
        assert_eq!(c.zone_for("example.com"), None);
    }

    #[test]
    fn 同时声明父子_zone_时取最长的那个() {
        let c = cf(Fake::with(vec![]), &["a.com", "x.a.com"]);
        assert_eq!(c.zone_for("_acme-challenge.x.a.com"), Some("x.a.com"));
        assert_eq!(c.zone_for("_acme-challenge.y.a.com"), Some("a.com"));
    }

    #[test]
    fn 超出声明范围的记录名直接拒绝而且一个请求都不发() {
        let fake = Fake::with(vec![]);
        let c = cf(fake.clone(), &["example.net"]);
        let e = rt()
            .block_on(c.set_txt("_acme-challenge.example.com", "v"))
            .unwrap_err();
        assert!(e.contains("G59"), "{e}");
        // ⚠ 判據的第二半：**一個請求都不許發出去**。
        //   一個「先發出去、再看回傳」的實作在這條上表現完全不同，
        //   而它已經把憑證用在了沒宣告的域上。
        assert!(
            fake.seen().is_empty(),
            "越权时不该发任何请求：{:?}",
            fake.seen()
        );
    }

    // ── G59 第 2 條：啟動時檢核 ─────────────────────────────────────────────

    #[test]
    fn 令牌有效但列不出那个_zone_也要拒绝启动() {
        // ⚠ ⚠ 這是最常見的那一種，而之前**檢核根本看不見它**：
        //   `/user/tokens/verify` 只回答「這把鑰匙存在嗎」，不回答「它開得了這扇門嗎」。
        //   現場表現因此被推遲到第一次簽發：一句「查 zone 失敗」，
        //   而那時人已經在查 ACME、查 DNS、查防火牆了。
        //
        //   三種成因共用這一條錯：沒有 Zone:Read / 資源範圍不含它 / `zones` 名字打錯。
        let fake = Fake::with(vec![(200, r#"{"success":true,"result":[]}"#)]);
        let c = cf(fake.clone(), &["example.net"]);
        let e = match rt().block_on(c.verify()) {
            Err(VerifyError::Fatal(e)) => e,
            other => panic!("应当是 Fatal（拒绝启动），拿到 {other:?}"),
        };
        assert!(e.contains("列不出 zone example.net"), "{e}");
        assert!(e.contains("Zone:Read"), "错误里要说清怎么办：{e}");
    }

    #[test]
    fn 声明了多个_zone_就每个都要验() {
        // ★ 判據掛在「請求條數」上：少驗一個 zone 的實作，
        //   會讓那個 zone 的問題一路潛伏到它第一次要簽證書的那天。
        let fake = Fake::with(vec![
            (
                200,
                r#"{"success":true,"result":[{"id":"z1","name":"a.example"}]}"#,
            ),
            (
                200,
                r#"{"success":true,"result":[{"id":"z2","name":"b.example"}]}"#,
            ),
        ]);
        let c = cf(fake.clone(), &["a.example", "b.example"]);
        rt().block_on(c.verify()).expect("两个都能列出来，应当通过");
        let seen = fake.seen();
        assert_eq!(seen.len(), 2, "只验了 {} 个 zone", seen.len());
        assert!(
            seen[0].url.ends_with("zones?name=a.example"),
            "{:?}",
            seen[0].url
        );
        assert!(
            seen[1].url.ends_with("zones?name=b.example"),
            "{:?}",
            seen[1].url
        );
    }

    #[test]
    fn 校验通过时请求形状是我们以为的那个() {
        // ★ ★ 換了判據：從 `/user/tokens/verify` 換成對**宣告的每個 zone**
        //   查一次 id。理由見 `verify()` 的文件註解 —— 帳號級權杖（`cfat_`）在
        //   使用者級端點上必然被判 Invalid，而 G59 把那個判成 Fatal ⇒ 拒絕啟動。
        let fake = Fake::with(vec![(
            200,
            r#"{"success":true,"result":[{"id":"zone-1","name":"example.net"}]}"#,
        )]);
        let c = cf(fake.clone(), &["example.net"]);
        rt().block_on(c.verify()).expect("应当通过");
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "GET");
        assert_eq!(
            seen[0].url, "https://api.example/client/v4/zones?name=example.net",
            "★ 校验必须与签发路径上「查 zone id」用同一个请求形状"
        );
        assert!(
            seen[0]
                .headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer tok-abc"),
            "凭据没按 bearer 形态带上：{:?}",
            seen[0].headers
        );
    }

    #[test]
    fn 状态码_200_但_success_false_也要判失败() {
        // ★ ★ Cloudflare 在**成功的 HTTP 狀態裡**也會回傳 `success:false`。
        //   一個只看狀態碼的實作會把壞憑證判成好的，而症狀要到第一次簽發才出現。
        let fake = Fake::with(vec![(
            200,
            r#"{"success":false,"errors":[{"code":1000,"message":"Invalid API Token"}]}"#,
        )]);
        let c = cf(fake, &["example.net"]);
        let e = rt().block_on(c.verify()).unwrap_err();
        assert!(
            matches!(e, VerifyError::Fatal(_)),
            "对端回话说不行 ⇒ 必须是 Fatal：{e}"
        );
        let e = e.message().to_string();
        assert!(e.contains("不可用"), "{e}");
        // ★ 把對端說的話帶出來 —— 否則排查時只能猜。
        assert!(e.contains("Invalid API Token"), "{e}");
    }

    #[test]
    fn 校验失败的错误里有来源但没有值() {
        let fake = Fake::with(vec![(
            403,
            r#"{"success":false,"errors":[{"message":"nope"}]}"#,
        )]);
        let c = cf(fake, &["example.net"]);
        let e = rt().block_on(c.verify()).unwrap_err().message().to_string();
        assert!(
            e.contains("FULCRUM_TEST_CF_TOKEN"),
            "应当说清凭据来自哪儿：{e}"
        );
        assert!(!e.contains("tok-abc"), "凭据的**值**漏进错误信息了：{e}");
    }

    // ── 掛 / 摘 TXT ─────────────────────────────────────────────────────────

    #[test]
    fn 挂_txt_先查_zone_id_再发_post_而且_body_逐字段对() {
        let fake = Fake::with(vec![
            (
                200,
                r#"{"success":true,"result":[{"id":"zone123","name":"example.net"}]}"#,
            ),
            (200, r#"{"success":true,"result":{"id":"rec456"}}"#),
        ]);
        let c = cf(fake.clone(), &["example.net"]);
        rt().block_on(c.set_txt("_acme-challenge.example.net", "the-value"))
            .expect("应当成功");
        let seen = fake.seen();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(
            seen[0].url,
            "https://api.example/client/v4/zones?name=example.net"
        );
        assert_eq!(seen[1].method, "POST");
        assert_eq!(
            seen[1].url,
            "https://api.example/client/v4/zones/zone123/dns_records"
        );
        let body: serde_json::Value =
            serde_json::from_slice(seen[1].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "TXT");
        assert_eq!(body["name"], "_acme-challenge.example.net");
        assert_eq!(body["content"], "the-value");
        assert_eq!(body["ttl"], 60);
    }

    #[test]
    fn 摘_txt_按_内容_定位而不是只按记录名() {
        // ⚠ 同一個記錄名下可能同時掛著多條 TXT（兩張證書並行簽發時就會），
        //   只按名字刪會把別人那條刪掉，而那次簽發會失敗得毫無頭緒。
        let fake = Fake::with(vec![
            (200, r#"{"success":true,"result":[{"id":"zone123"}]}"#),
            (200, r#"{"success":true,"result":[{"id":"rec789"}]}"#),
            (200, r#"{"success":true}"#),
        ]);
        let c = cf(fake.clone(), &["example.net"]);
        rt().block_on(c.clear_txt("_acme-challenge.example.net", "the-value"))
            .expect("应当成功");
        let seen = fake.seen();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(
            seen[1].url.contains("content=the-value"),
            "查记录时没带上内容：{}",
            seen[1].url
        );
        assert_eq!(seen[2].method, "DELETE");
        assert!(
            seen[2].url.ends_with("/dns_records/rec789"),
            "{}",
            seen[2].url
        );
    }

    #[test]
    fn 摘_txt_时对端说没有就当已经摘了() {
        // 清理是衛生問題不是可用性問題 —— 找不到不該回報錯誤。
        let fake = Fake::with(vec![
            (200, r#"{"success":true,"result":[{"id":"zone123"}]}"#),
            (200, r#"{"success":true,"result":[]}"#),
        ]);
        let c = cf(fake, &["example.net"]);
        rt().block_on(c.clear_txt("_acme-challenge.example.net", "v"))
            .expect("找不到不该是错误");
    }

    #[test]
    fn 查不到_zone_id_时给一条能看懂的错() {
        let fake = Fake::with(vec![(200, r#"{"success":true,"result":[]}"#)]);
        let c = cf(fake, &["example.net"]);
        let e = rt()
            .block_on(c.set_txt("_acme-challenge.example.net", "v"))
            .unwrap_err();
        assert!(e.contains("看不到这个 zone"), "{e}");
    }
}
