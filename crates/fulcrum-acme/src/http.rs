//! 一個夠用就好的 HTTPS 用戶端，只給原生 DNS 供應商（G57）調 API 用。
//!
//! ★ **不拉 `reqwest`**：`hyper` + `hyper-util` + `http-body-util` 已經在產品圖裡，
//! 直接用它們**新增 0 個包**。代價是自己寫大約一百行，只做四件事：
//! 建連接器、發請求、收 body、給上界。TLS 那一格是 [`crate::https::BoringHttpsConnector`]。
//!
//! ⚠ ★ `hyper-util` 的 `http1` feature 必須由本儲存庫顯式開，不能靠別人順帶，
//! 關掉那個 feature 之後必須自己在根清單裡寫上，否則 `Client` 連 HTTP/1 都不會說。
//! **「能編過是因為別人替我開了」在本儲存庫出現過兩次**（另一次是 `tokio` 的 `signal`）。
//!
//! # ⚠ ⚠ [`HttpTransport`] 這個接縫**證不了**什麼
//!
//! 假實作能斷言「我們發出去的 URL / 方法 / 頭 / body 正是我們以為的那些」——
//! 那是**內部自洽**，不是「我們對它家 API 的理解是對的」。
//! 假服務的行為也是我們自己想出來的，兩邊同時錯的時候它照樣全綠
//! （**判據掛在替身上等於沒有判據**）。
//! ★ 後者的判據只能是真網域上的一次真簽發。

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::time::Duration;

/// 回應 body 最多收多少。★ 與 exec hook 那條同一個理由：
/// 一個瘋掉（或被劫持）的對端不該能把記憶體吃光。
/// 這幾家的 JSON 回應都是幾 KB 量級，1 MiB 已經寬出兩個數量級。
pub const MAX_BODY: usize = 1024 * 1024;

/// 單次呼叫的超時。★ 掛住一個 DNS API 呼叫 = 掛住整條簽發鏈。
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// 一次請求。★ 刻意做成樸素的資料，好讓測試能逐欄位斷言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// ⚠ 裡面會有 `Authorization`。**這個結構不實作 `Debug` 之外的列印**，
    /// 而且任何紀錄都不許整個打它——見下面 `redacted()`。
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

    /// 能進紀錄的那一份：**頭的值一律換成 `<redacted>`**。
    ///
    /// ⚠ 不是只擋 `Authorization`：DNSPod 的憑證在 **body** 裡
    /// （`login_token=ID,Token`），而 Cloudflare 的在頭裡。
    /// 一個「只擋 Authorization」的實作在 DNSPod 上完全失效，
    /// 而失效的表現是**憑證出現在紀錄裡**——沒有任何回報錯誤。
    /// 所以這裡 body 也不打，只打長度。
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

/// 一次回應。
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

/// 誰去把請求發出去。★ 生產是 [`HyperTransport`]，測試塞一個記錄型假實作。
#[async_trait::async_trait]
pub trait HttpTransport: Send + Sync + std::fmt::Debug {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String>;
}

/// 真的那個。
pub struct HyperTransport {
    client: Client<crate::https::BoringHttpsConnector, Full<Bytes>>,
}

impl std::fmt::Debug for HyperTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HyperTransport")
    }
}

impl HyperTransport {
    /// ⚠ 走**系統信任庫**，與產品別處連 CA 的口徑一致。
    /// 兩處不一致的話，「這臺機器信任什麼」就有兩個答案，而排查時只會看其中一個。
    ///
    /// ★ ★ **G104 第 ③ 處：這裡換成 BoringSSL 了。**
    /// 原先是 `hyper_rustls::HttpsConnectorBuilder::new().with_native_roots()…`；
    /// 現在與 ACME 協定本身共用同一個 [`crate::https::BoringHttpsConnector`]
    /// —— 一套信任庫、一套檢核、一份 ALPN。
    /// ⚠ 「口徑一致」這句話此前只能靠人守著，現在**是結構上的**：兩條路只有一個建構入口。
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
        // ⚠ 連接器那層也擋明文（`BoringHttpsConnector::call` 的第一句），
        //   但這裡再擋一次並給出**能看懂的**錯誤：那一層拒絕時說的是
        //   「拒絕發非 HTTPS 請求」，看不出是哪個 DNS 供應商的哪次呼叫。
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

        // ★ 超時罩住**整段**（連接 + 發 + 收 body），不是只罩發出去那一下。
        //   ⚠ 這條是 exec hook 那次的教訓直接搬過來的：只罩一半的超時是一句空話。
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
            // 超上界就截斷，**不是回報錯誤**：一個超大的錯誤頁不該讓簽發失敗，
            // 而截斷之後的內容仍然足夠寫進紀錄。
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
        // ★ ★ 判據兩個方向都要：**名字在、值不在**。
        //   只斷言「不含 token」的話，一個把整條 redacted() 變成空串的實作照樣綠。
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
        // ⚠ 兩處憑證：Cloudflare 在頭裡、DNSPod 在 body 裡。一個只擋頭的實作
        //   在 DNSPod 上完全失效，而失效的表現是憑證出現在紀錄裡，沒有任何回報錯誤。
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
