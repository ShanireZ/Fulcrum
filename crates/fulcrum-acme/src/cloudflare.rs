//! 原生 Cloudflare（G57 两家之一 —— `example.net` 就在这家）。
//!
//! 用的是 API v4 的 bearer token 形态：
//!
//! | 干什么 | 请求 |
//! |---|---|
//! | 校验凭据（**G59 第 2 条**）| `GET /client/v4/zones?name=<声明的每个 zone>` |
//! | 查 zone id | `GET /client/v4/zones?name=<zone>` |
//! | 挂 TXT | `POST /client/v4/zones/<id>/dns_records` |
//! | 摘 TXT | `DELETE /client/v4/zones/<id>/dns_records/<record>` |
//!
//! # ⚠ ⚠ 这个模块的判据到哪儿为止
//!
//! 单测能证明的是**内部自洽**：我们发出去的 URL / 方法 / 头 / body 正是我们以为的那些，
//! 给定一段文档里那种形状的响应、解析出来的东西正是我们以为的那些。
//!
//! > **它证不了「我们对 Cloudflare API 的理解是对的」** —— 那要拿真的对端来测，
//! > 而门禁里没有。★ 那一条的判据是真域名上的一次真签发（G57）。
//!
//! # ★ 摘 TXT 失败不让签发失败
//!
//! 与 exec hook 那条同一个理由：证书已经到手了，一条留在那里的 TXT 是**卫生问题**
//! 不是可用性问题。升级成错误会把一次**成功**的签发记成失败 ⇒ 进退避、进计数、进告警。

use crate::credential::CredentialSource;
use crate::http::{HttpRequest, HttpTransport};
use crate::provider::VerifyError;
use log::debug;
use std::sync::Arc;

const API: &str = "https://api.cloudflare.com/client/v4";

/// TXT 记录的 TTL。★ 取最小值：挑战记录活不过一次签发，
/// 而 TTL 大了会让**下一次**签发被缓存里的旧值绊住。
const TTL: u32 = 60;

#[derive(Debug, Clone)]
pub struct Cloudflare {
    transport: Arc<dyn HttpTransport>,
    source: CredentialSource,
    /// 这份凭据被**声明**覆盖哪些 zone（G59 第 3 条）。
    zones: Vec<String>,
    /// 覆盖 API 根，只给测试用。⚠ 产品里没有任何配置项能改它。
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

    /// 这条记录名落在被声明的 zone 里吗（**G59 第 3 条**）。
    ///
    /// ★ 与 DNSPod 那份**共用同一份实现**（[`crate::zone_scope::zone_for`]），
    /// 不是各写一遍——理由与 G66 同族：两份迟早会分叉，而分叉之后两边都还是绿的。
    pub fn zone_for(&self, record: &str) -> Option<&str> {
        crate::zone_scope::zone_for(&self.zones, record)
    }

    /// **G59 第 2 条**：启动时校验凭据。
    ///
    /// # ★ ★ ★ 验的是「我们接下来真要做的那件事」，不是「有没有某个接口认它」
    ///
    /// 在 `example.net` 上撞到的：本方法原先打的是
    /// `GET /user/tokens/verify` —— 而 Cloudflare 2026 年起把令牌分成了
    /// **用户级 `cfut_`** 与 **账号级 `cfat_`** 两种，后者在用户级端点上**必然**
    /// 回 `1000 Invalid API Token`。⚠ 而 G59 把「对端说不行」判成 Fatal ⇒
    /// **一把完全好用的账号级令牌会让枢衡拒绝启动**，错误信息还指着「凭据不可用」。
    ///
    /// ⇒ 改成对**声明过的每一个 zone** 做一次 `GET /zones?name=<zone>`。这条判据严格更强：
    ///
    /// | 它顺带证明了 | 原来那条证不了 |
    /// |---|---|
    /// | 令牌真的能用（两种令牌都走得通）| — |
    /// | 令牌**够得着这个 zone**（Zone:Read 有、范围包含它）| ✅ 原来要等第一次签发才发现 |
    /// | `zones` 声明里的名字**真的存在**（打错一个字母立刻现形）| ✅ 同上 |
    ///
    /// ★ 「查 zone id」正是签发路径上的第一步（见 [`Self::zone_id`]），
    /// 所以这条校验与真实用法**共用同一个请求形状** —— 校验过而签发挂不了。
    pub async fn verify(&self) -> Result<(), VerifyError> {
        // ⚠ 凭据读不出来是**本机的、确定的**问题 ⇒ Fatal，不是 Inconclusive。
        let auth = self.auth().map_err(VerifyError::Fatal)?;
        // ⚠ 一个 zone 都没声明时不该悄悄通过：G59 第 3 条要求原生供应商必填 zones，
        //   走到这里说明上游校验漏了，说出来比默默放行强。
        if self.zones.is_empty() {
            return Err(VerifyError::Fatal(
                "没有声明 `zones` —— 原生供应商必填（G59 第 3 条），无从校验".to_string(),
            ));
        }
        for zone in &self.zones {
            let req = HttpRequest::get(format!("{}/zones?name={zone}", self.base))
                .header("authorization", auth.clone())
                .header("accept", "application/json");
            // 发不出去 / 收不到 ⇒ 分不出是对端挂了还是我们这儿网不通 ⇒ Inconclusive。
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
            // ⚠ 光看 200 不够：Cloudflare 在**成功的 HTTP 状态里**也会返回 `success:false`。
            //   一个只看状态码的实现会把一份坏凭据判成好的，而症状要到第一次签发才出现。
            if !text.contains("\"success\":true") && !text.contains("\"success\": true") {
                return Err(VerifyError::Fatal(format!(
                    "Cloudflare 说这份凭据不可用（来自 {}）：{}",
                    self.source.describe(),
                    first_error(&text)
                )));
            }
            // ★ ★ success:true **但列不出这个 zone** 是一种独立的失败，且最常见：
            //   令牌有效、但没有 Zone:Read，或者资源范围里没圈上这个 zone，
            //   又或者 `zones` 里的名字打错了。⚠ 三种的处置都是「现在就说」，
            //   而不是等第一次签发时得到一句「查 zone 失败」。
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
        // ⚠ 用 serde_json 拼 body，不手拼字符串：值是 base64url，虽然不含引号，
        //   但「这次不含」不是判据 —— 手拼的注入面永远只差一个新调用点。
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
        // 先查出 record id。★ 带上 content：同一个记录名下可能同时挂着**多条** TXT
        //   （两张证书并行签发时就会），删错一条会让另一条签发失败。
        let req = HttpRequest::get(format!(
            "{}/zones/{id}/dns_records?type=TXT&name={name}&content={value}",
            self.base
        ))
        .header("authorization", self.auth()?)
        .header("accept", "application/json");
        let rsp = self.transport.send(req).await?;
        let text = rsp.body_text();
        let Some(rec) = json_str_field(&text, "id") else {
            // 查不到就当已经没了 —— 这是清理，不是可用性。
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

/// 从一段 JSON 里抠出第一个 `"<field>": "<值>"`。
///
/// ⚠ ⚠ **这是一个刻意做小的取值器，不是 JSON 解析器**，而它的取舍要说清楚：
/// 这几家的响应里 `result` 是数组或对象，我们只要里面第一条的 `id`。
/// 用 `serde_json::Value` 走一遍也行，但那要为每家的嵌套结构写一份路径，
/// 而路径写错的表现与这里取错的表现完全一样。
/// ★ 所以判据不在「怎么取」，在**「取出来的东西对不对」由单测逐字段钉住**。
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

/// 把对端给的第一条错误消息带出来。★ 一个只说「失败」的错误，
/// 等于让人自己去猜是凭据错了、zone 不对、还是被限流了。
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

    /// 记录型假实现。★ 它证明的是**内部自洽**，不是「我们对它家 API 的理解是对的」。
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
        // SAFETY-ish：测试进程内设环境变量。★ 用一个测试专用的名字，避免与别的测试互相踩。
        unsafe { std::env::set_var("FULCRUM_TEST_CF_TOKEN", "tok-abc") };
        Cloudflare::new(
            fake,
            CredentialSource::Env("FULCRUM_TEST_CF_TOKEN".into()),
            zones.iter().map(|z| z.to_string()).collect(),
        )
        .with_base("https://api.example/client/v4")
    }

    // ── G59 第 3 条：zone 范围 ───────────────────────────────────────────────

    #[test]
    fn zone_范围按标签边界判而不是后缀() {
        // ★ ★ 这条判据存在的全部理由，就是不让凭据被用在没声明的域上。
        //   ⚠ `ends_with("example.net")` 会把 `evilexample.net` 算进来 —— 那正是要防的。
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
        // ⚠ 判据的第二半：**一个请求都不许发出去**。
        //   一个「先发出去、再看返回」的实现在这条上表现完全不同，
        //   而它已经把凭据用在了没声明的域上。
        assert!(
            fake.seen().is_empty(),
            "越权时不该发任何请求：{:?}",
            fake.seen()
        );
    }

    // ── G59 第 2 条：启动时校验 ─────────────────────────────────────────────

    #[test]
    fn 令牌有效但列不出那个_zone_也要拒绝启动() {
        // ⚠ ⚠ 这是最常见的那一种，而之前**校验根本看不见它**：
        //   `/user/tokens/verify` 只回答「这把钥匙存在吗」，不回答「它开得了这扇门吗」。
        //   现场表现因此被推迟到第一次签发：一句「查 zone 失败」，
        //   而那时人已经在查 ACME、查 DNS、查防火墙了。
        //
        //   三种成因共用这一条错：没有 Zone:Read / 资源范围不含它 / `zones` 名字打错。
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
        // ★ 判据挂在「请求条数」上：少验一个 zone 的实现，
        //   会让那个 zone 的问题一路潜伏到它第一次要签证书的那天。
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
        // ★ ★ 换了判据：从 `/user/tokens/verify` 换成对**声明的每个 zone**
        //   查一次 id。理由见 `verify()` 的文档注释 —— 账号级令牌（`cfat_`）在
        //   用户级端点上必然被判 Invalid，而 G59 把那个判成 Fatal ⇒ 拒绝启动。
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
        // ★ ★ Cloudflare 在**成功的 HTTP 状态里**也会返回 `success:false`。
        //   一个只看状态码的实现会把坏凭据判成好的，而症状要到第一次签发才出现。
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
        // ★ 把对端说的话带出来 —— 否则排查时只能猜。
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

    // ── 挂 / 摘 TXT ─────────────────────────────────────────────────────────

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
        // ⚠ 同一个记录名下可能同时挂着多条 TXT（两张证书并行签发时就会），
        //   只按名字删会把别人那条删掉，而那次签发会失败得毫无头绪。
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
        // 清理是卫生问题不是可用性问题 —— 找不到不该报错。
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
