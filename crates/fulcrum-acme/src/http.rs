//! 一个够用就好的 HTTPS 客户端，只给原生 DNS 供应商（G57）调 API 用。
//!
//! ★ **不拉 `reqwest`**：`hyper` + `hyper-util` + `http-body-util` 已经在产品图里，
//! 直接用它们**新增 0 个包**。代价是自己写大约一百行，只做四件事：
//! 建连接器、发请求、收 body、给上界。TLS 那一格是 [`crate::https::BoringHttpsConnector`]。
//!
//! ⚠ ★ `hyper-util` 的 `http1` feature 必须由本仓库显式开，不能靠别人顺带，
//! 关掉那个 feature 之后必须自己在根清单里写上，否则 `Client` 连 HTTP/1 都不会说。
//! **「能编过是因为别人替我开了」在本仓库出现过两次**（另一次是 `tokio` 的 `signal`）。
//!
//! # ⚠ ⚠ [`HttpTransport`] 这个接缝**证不了**什么
//!
//! 假实现能断言「我们发出去的 URL / 方法 / 头 / body 正是我们以为的那些」——
//! 那是**内部自洽**，不是「我们对它家 API 的理解是对的」。
//! 假服务的行为也是我们自己想出来的，两边同时错的时候它照样全绿
//! （**判据挂在替身上等于没有判据**）。
//! ★ 后者的判据只能是真域名上的一次真签发。

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::time::Duration;

/// 响应 body 最多收多少。★ 与 exec hook 那条同一个理由：
/// 一个疯掉（或被劫持）的对端不该能把内存吃光。
/// 这几家的 JSON 响应都是几 KB 量级，1 MiB 已经宽出两个数量级。
pub const MAX_BODY: usize = 1024 * 1024;

/// 单次调用的超时。★ 挂住一个 DNS API 调用 = 挂住整条签发链。
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// 一次请求。★ 刻意做成朴素的数据，好让测试能逐字段断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// ⚠ 里面会有 `Authorization`。**这个结构不实现 `Debug` 之外的打印**，
    /// 而且任何日志都不许整个打它——见下面 `redacted()`。
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> HttpRequest {
        HttpRequest {
            method: "GET",
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn post(url: impl Into<String>, content_type: &str, body: Vec<u8>) -> HttpRequest {
        HttpRequest {
            method: "POST",
            url: url.into(),
            headers: vec![("content-type".into(), content_type.into())],
            body: Some(body),
        }
    }

    pub fn delete(url: impl Into<String>) -> HttpRequest {
        HttpRequest {
            method: "DELETE",
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> HttpRequest {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// 能进日志的那一份：**头的值一律换成 `<redacted>`**。
    ///
    /// ⚠ 不是只挡 `Authorization`：DNSPod 的凭据在 **body** 里
    /// （`login_token=ID,Token`），而 Cloudflare 的在头里。
    /// 一个「只挡 Authorization」的实现在 DNSPod 上完全失效，
    /// 而失效的表现是**凭据出现在日志里**——没有任何报错。
    /// 所以这里 body 也不打，只打长度。
    pub fn redacted(&self) -> String {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        format!(
            "{} {}（头：{}；body {} 字节）",
            self.method,
            self.url,
            names.join(","),
            self.body.as_ref().map(|b| b.len()).unwrap_or(0)
        )
    }
}

/// 一次响应。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn body_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// 谁去把请求发出去。★ 生产是 [`HyperTransport`]，测试塞一个记录型假实现。
#[async_trait::async_trait]
pub trait HttpTransport: Send + Sync + std::fmt::Debug {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String>;
}

/// 真的那个。
pub struct HyperTransport {
    client: Client<crate::https::BoringHttpsConnector, Full<Bytes>>,
}

impl std::fmt::Debug for HyperTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HyperTransport")
    }
}

impl HyperTransport {
    /// ⚠ 走**系统信任库**，与产品别处连 CA 的口径一致。
    /// 两处不一致的话，「这台机器信任什么」就有两个答案，而排查时只会看其中一个。
    ///
    /// ★ ★ **G104 第 ③ 处：这里换成 BoringSSL 了。**
    /// 原先是 `hyper_rustls::HttpsConnectorBuilder::new().with_native_roots()…`；
    /// 现在与 ACME 协议本身共用同一个 [`crate::https::BoringHttpsConnector`]
    /// —— 一套信任库、一套校验、一份 ALPN。
    /// ⚠ 「口径一致」这句话此前只能靠人守着，现在**是结构上的**：两条路只有一个构造入口。
    pub fn new() -> Result<HyperTransport, String> {
        let https = crate::https::BoringHttpsConnector::new()?;
        Ok(HyperTransport {
            client: Client::builder(TokioExecutor::new()).build(https),
        })
    }
}

#[async_trait::async_trait]
impl HttpTransport for HyperTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        // ⚠ 连接器那层也挡明文（`BoringHttpsConnector::call` 的第一句），
        //   但这里再挡一次并给出**能看懂的**错误：那一层拒绝时说的是
        //   「拒绝发非 HTTPS 请求」，看不出是哪个 DNS 供应商的哪次调用。
        if !req.url.starts_with("https://") {
            return Err(format!("拒绝发明文请求：{}", req.url));
        }
        let mut builder = hyper::Request::builder().method(req.method).uri(&req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let body = Full::new(Bytes::from(req.body.clone().unwrap_or_default()));
        let request = builder
            .body(body)
            .map_err(|e| format!("请求构造失败（{}）：{e}", req.redacted()))?;

        // ★ 超时罩住**整段**（连接 + 发 + 收 body），不是只罩发出去那一下。
        //   ⚠ 这条是 exec hook 那次的教训直接搬过来的：只罩一半的超时是一句空话。
        let fut = async {
            let rsp = self
                .client
                .request(request)
                .await
                .map_err(|e| format!("请求失败（{}）：{e}", req.redacted()))?;
            let status = rsp.status().as_u16();
            let collected = rsp
                .into_body()
                .collect()
                .await
                .map_err(|e| format!("读响应失败（{}）：{e}", req.redacted()))?;
            let full = collected.to_bytes();
            // 超上界就截断，**不是报错**：一个超大的错误页不该让签发失败，
            // 而截断之后的内容仍然足够写进日志。
            let body = full.iter().copied().take(MAX_BODY).collect::<Vec<u8>>();
            Ok::<HttpResponse, String>(HttpResponse { status, body })
        };
        match tokio::time::timeout(TIMEOUT, fut).await {
            Ok(r) => r,
            Err(_) => Err(format!(
                "请求超时（{}s）：{}",
                TIMEOUT.as_secs(),
                req.redacted()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 能进日志的那一份不含任何值() {
        // ★ ★ 判据两个方向都要：**名字在、值不在**。
        //   只断言「不含 token」的话，一个把整条 redacted() 变成空串的实现照样绿。
        let req = HttpRequest::post(
            "https://api.example/x",
            "application/json",
            b"login_token=12345,SECRETTOKEN&domain=a.com".to_vec(),
        )
        .header("authorization", "Bearer SECRETTOKEN");
        let line = req.redacted();
        assert!(line.contains("POST"), "{line}");
        assert!(line.contains("https://api.example/x"), "{line}");
        assert!(line.contains("authorization"), "头的名字应当在：{line}");
        assert!(line.contains("42 字节"), "body 长度应当在：{line}");
        // ⚠ 两处凭据：Cloudflare 在头里、DNSPod 在 body 里。一个只挡头的实现
        //   在 DNSPod 上完全失效，而失效的表现是凭据出现在日志里，没有任何报错。
        assert!(!line.contains("SECRETTOKEN"), "凭据漏进日志了：{line}");
        assert!(
            !line.contains("login_token"),
            "body 不该出现在日志里：{line}"
        );
    }

    #[test]
    fn 明文一律拒绝() {
        let t = HyperTransport::new().expect("建不出 transport");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let e = rt
            .block_on(t.send(HttpRequest::get("http://api.example/x")))
            .unwrap_err();
        assert!(e.contains("明文"), "{e}");
    }
}
