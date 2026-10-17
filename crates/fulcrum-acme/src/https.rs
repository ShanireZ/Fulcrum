//! 出站 HTTPS 的連接器：**BoringSSL**（G104 第 ③ 處）。
//!
//! §5.1 第 1 條「三處全換」的第 ③ 處：本 crate 往外發的兩類請求
//! （ACME 協定本身與原生 DNS 供應商的 HTTPS API）共用這一個連接器，
//! 也就共用同一套信任庫、同一套檢核、同一份 ALPN。
//!
//! # ★ 我們寫的是**連接器膠水**，不是 TLS
//!
//! 檢核全部交給 [`SslConnector`]：證書鏈走 `builder()` 裡那句 `set_default_verify_paths()`，
//! 主機名走 `configure()?.verify_hostname(true)` → `into_ssl(domain)`。
//! ⚠ **安全基線點名不許手寫的就是這一類** —— 這個檔案裡沒有一行在比對名字或驗鏈。
//!
//! # ⛔ 為什麼不用 `hyper-boring`
//!
//! 實測 **+9 個包**（整套舊版 hyper 0.14）。根因：它的 feature 表寫的是
//! `runtime = [hyper_old/runtime]` —— **沒有 `dep:` 前綴**，意思是 hyper 0.14 本身是必需依賴，
//! `default-features = false` 攔不住。⇒ 自己寫一個 [`tower_service::Service<Uri>`]，
//! 底下用 fork 裡已有的 [`pingora_boringssl::tokio_ssl::SslStream`]，**新增 0 個包**。
//!
//! ★ 可帶走的一條：**看 feature 表要看有沒有 `dep:` 前綴**，而識破它要**真加進去 resolve
//! 一遍再 diff 鎖** —— 清單上寫著 `hyper1 = [dep:http, dep:hyper, …]`，
//! 讀起來像是「不開就不進來」。
//!
//! # ⚠ 三處「不寫就悄悄錯」的地方
//!
//! 1. **`HttpConnector` 預設 `enforce_http = true`** —— 它一看見 `https://` 就報
//!    「invalid URL, scheme is not http」。必須 `enforce_http(false)`，
//!    ⇒ **明文的那道閘就得由我們自己守**（[`BoringHttpsConnector::call`] 第一件事
//!    就是拒非 `https`）。★ 這兩條是同一枚硬幣：關掉上游的檢查就必須自己接住。
//! 2. **ALPN 釘死 `http/1.1`**。產品這一側只開了 `hyper/http1`，
//!    協商到 h2 的話拿到的是一條我們不會說的協定。
//!    ⚠ 不設 ALPN 多數伺服器也會回落到 HTTP/1.1，但那是**對端的預設值**，不是我們的判據。
//! 3. **主機名要去掉 IPv6 字面值的方括號**：`Uri::host()` 給的是 `[::1]`，
//!    而 `into_ssl` 要的是 `::1`（它自己按 `IpAddr::from_str` 判斷要不要發 SNI）。
//!
//! # ★ 判據都配對照
//!
//! 每一條反向都配一條「把被測性質關掉」的對照，否則說明它根本沒測到被測性質：
//! 自簽證書連不上 / 關掉驗證就連得上 · 名字不對連不上 / 同一個 CA 名字對的連得上。
//!
//! ⚠ ⚠ **「關掉驗證」只存在於 `#[cfg(test)]` 裡** —— 產品面上
//! [`BoringHttpsConnector::new`] 是唯一建構入口，沒有任何旋鈕。

use hyper::Uri;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{Connected, Connection, HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use instant_acme::{BodyWrapper, BytesResponse, HttpClient};
use pingora_boringssl::ssl::{SslConnector, SslMethod};
use pingora_boringssl::tokio_ssl::SslStream;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tower_service::Service;

/// ALPN 的線上格式：一個長度位元組 + 協定名。
///
/// ★ 只提供 `http/1.1`，理由見模組文件第 2 條。
const ALPN_HTTP11: &[u8] = b"\x08http/1.1";

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 出站 TLS 的上下文。★ 一個處理程序建一次，之後每條連接從它 `configure()` 派生。
///
/// ⚠ 這裡**什麼都不放寬**：`SslConnector::builder` 自己就調了
/// `set_default_verify_paths()` 並把 `SSL_VERIFY_PEER` 打開
/// （boring 5.2.0 `ssl/connector.rs`，4.22.0 起就如此），我們只額外釘一條 ALPN。
/// ★ boring 5 刪了 `SslMethod::tls_client()` ⇒ 用 `tls()`；用戶端 / 伺服器端由 `connect()` 自己設，行為不變。
fn client_context() -> Result<SslConnector, String> {
    let mut b = SslConnector::builder(SslMethod::tls())
        .map_err(|e| format!("建不出出站 TLS 上下文：{e}"))?;
    b.set_alpn_protos(ALPN_HTTP11)
        .map_err(|e| format!("出站 TLS 设不上 ALPN：{e}"))?;
    Ok(b.build())
}

/// 一個把 TCP 連接升級成 TLS 的 [`tower_service::Service`]，餵給
/// [`hyper_util::client::legacy::Client`] 當連接器用。
#[derive(Clone)]
pub struct BoringHttpsConnector {
    http: HttpConnector,
    /// ★ [`SslConnector`] 是 `Clone` 的（內部就是一次 `SSL_CTX` 引用計數），
    ///   所以不需要再包一層 `Arc`。
    ssl: SslConnector,
}

impl BoringHttpsConnector {
    /// **產品面唯一的建構入口**：系統信任庫 + 鏈檢核 + 主機名檢核，全部開著。
    pub fn new() -> Result<BoringHttpsConnector, String> {
        Ok(BoringHttpsConnector::with_context(client_context()?))
    }

    /// ⚠ 私有：給 [`BoringHttpsConnector::new`] 與本檔案的判據共用。
    /// 判據要拿一個**故意配壞的**上下文來做對照，而那條路不許出現在產品面上。
    fn with_context(ssl: SslConnector) -> BoringHttpsConnector {
        let mut http = HttpConnector::new();
        // ⚠ 見模組文件第 1 條：不關掉它，`https://` 在建 TCP 之前就被上游拒了。
        //   關掉之後拒明文是我們自己的事，見 `call()` 的第一句。
        http.enforce_http(false);
        BoringHttpsConnector { http, ssl }
    }
}

impl std::fmt::Debug for BoringHttpsConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BoringHttpsConnector")
    }
}

impl Service<Uri> for BoringHttpsConnector {
    type Response = TokioIo<BoringStream>;
    type Error = BoxError;
    /// ⚠ `Pin<Box<…>>` 不只是圖省事：`hyper_util` 的 `Connect` 要求
    /// `S::Future: Unpin + Send`，而 `Pin<Box<dyn Future + Send>>` 兩條都滿足。
    type Future = Pin<Box<dyn Future<Output = Result<TokioIo<BoringStream>, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Service::poll_ready(&mut self.http, cx).map_err(BoxError::from)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let ssl = self.ssl.clone();
        let mut http = self.http.clone();
        Box::pin(async move {
            // ⚠ ⚠ 這一句是 `enforce_http(false)` 的另一半，見模組文件第 1 條。
            if uri.scheme_str() != Some("https") {
                return Err(BoxError::from(format!("拒绝发非 HTTPS 请求：{uri}")));
            }
            let Some(host) = uri.host() else {
                return Err(BoxError::from(format!("URL 里没有主机名：{uri}")));
            };
            // IPv6 字面值在 `Uri::host()` 裡是帶方括號的，見模組文件第 3 條。
            let host = host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string();

            let tcp = http.call(uri).await?.into_inner();

            // ★ 檢核全在這三行裡，而這三行一個字都不是我們自己寫的邏輯。
            let ssl = ssl
                .configure()
                .map_err(|e| format!("出站 TLS 派生会话失败：{e}"))?
                .verify_hostname(true)
                .into_ssl(&host)
                .map_err(|e| format!("出站 TLS 设不上主机名 {host}：{e}"))?;

            let mut stream = SslStream::new(ssl, tcp)
                .map_err(|e| format!("出站 TLS 建不出流（{host}）：{e}"))?;
            Pin::new(&mut stream)
                .connect()
                .await
                .map_err(|e| format!("与 {host} 的 TLS 握手失败：{e}"))?;
            Ok(TokioIo::new(BoringStream(stream)))
        })
    }
}

/// 一條已經握完手的出站 TLS 連接。
///
/// ★ ★ **它存在的唯一理由是孤兒規則**：`hyper_util` 的 [`Connection`] 與
/// `pingora-boringssl` 的 [`SslStream`] 都是外部類型，本 crate 只能給自己的類型實作它。
/// 而 `hyper_util` 那條 `impl<T: Connection> Connection for TokioIo<T>` 會把它接上去。
pub struct BoringStream(SslStream<TcpStream>);

impl Connection for BoringStream {
    fn connected(&self) -> Connected {
        // ★ 轉交給底下那條 TCP：`Connected` 裡帶的是本機/對端位址，
        //   與 TLS 無關，而 hyper 的連接池要用它。
        self.0.get_ref().connected()
    }
}

impl AsyncRead for BoringStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoringStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

/// 給 `instant-acme` 用的 HTTP 用戶端。
///
/// # ★ ★ 為什麼要有這個類型
///
/// `instant-acme` 的 `hyper-rustls` feature 一關，`Account::builder()` 與
/// `BytesResponse::try_from` 就一起沒了（兩者都掛在那個 feature 上）
/// ⇒ 必須自己實作 [`instant_acme::HttpClient`]，並走
/// `Account::builder_with_http()` 那個入口。
///
/// ★ 而 `BytesResponse` 的 `From<http::Response<B>>` **不在 feature 門裡**，
/// 所以這一層只有十來行：把 `hyper` 的回應交出去，把錯誤包成 `Error::Other`。
pub struct AcmeHttpClient {
    inner: Client<BoringHttpsConnector, BodyWrapper<Bytes>>,
}

impl AcmeHttpClient {
    pub fn new() -> Result<AcmeHttpClient, String> {
        Ok(AcmeHttpClient {
            inner: Client::builder(TokioExecutor::new()).build(BoringHttpsConnector::new()?),
        })
    }
}

impl std::fmt::Debug for AcmeHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AcmeHttpClient")
    }
}

impl HttpClient for AcmeHttpClient {
    fn request(
        &self,
        req: hyper::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>> {
        let fut = self.inner.request(req);
        Box::pin(async move {
            match fut.await {
                Ok(rsp) => Ok(BytesResponse::from(rsp)),
                Err(e) => Err(instant_acme::Error::Other(Box::new(e))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingora_boringssl::pkey::PKey;
    use pingora_boringssl::ssl::{Ssl, SslContext, SslContextBuilder, SslVerifyMode};
    use pingora_boringssl::x509::X509;
    use tokio::net::TcpListener;

    /// 一個測試用的 CA + 一張由它簽出來的葉子證書。
    struct TestPki {
        ca_der: Vec<u8>,
        leaf_der: Vec<u8>,
        leaf_key_der: Vec<u8>,
    }

    /// 一個只有 CN 的主體名。
    fn dn(cn: &str) -> rcgen::DistinguishedName {
        let mut d = rcgen::DistinguishedName::new();
        d.push(rcgen::DnType::CommonName, cn);
        d
    }

    /// 造一張 SAN 為 `san` 的葉子證書，外加簽它的那個 CA。
    ///
    /// ★ 用 `rcgen`，它**已經在依賴圖裡**（`instant-acme` 拉的，見 `tlsalpn01.rs`）。
    ///
    /// ⚠ ⚠ **兩個主體名必須不一樣，而這是被一次紅教出來的**：`CertificateParams`
    /// 的預設主體名是同一個常數，於是 CA 與葉子的 subject **逐字相同** ——
    /// 檢核器會把葉子當成自籤（issuer == subject）、根本不去找那個 CA，
    /// 報的是 `CERTIFICATE_VERIFY_FAILED`。
    /// ★ 抓到它的是**正向那一條**（名字對的也該連得上），而不是被測的反向那一條 ——
    /// 沒有它，一次「CA 根本沒生效」會被讀成「主機名檢核生效了」。
    fn make_pki(san: &str) -> TestPki {
        let ca_key = rcgen::KeyPair::generate().expect("CA 密钥");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA 参数");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        ca_params.distinguished_name = dn("fulcrum-test-ca");
        let ca_cert = ca_params.self_signed(&ca_key).expect("CA 自签");
        let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);

        let leaf_key = rcgen::KeyPair::generate().expect("叶子密钥");
        let mut leaf_params =
            rcgen::CertificateParams::new(vec![san.to_string()]).expect("叶子参数");
        leaf_params.distinguished_name = dn("fulcrum-test-leaf");
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).expect("叶子签发");

        TestPki {
            ca_der: ca_cert.der().to_vec(),
            leaf_der: leaf.der().to_vec(),
            leaf_key_der: leaf_key.serialize_der(),
        }
    }

    /// 一張**自籤**證書（沒有 CA）。用來做「鏈檢核開著沒有」那一條。
    fn make_self_signed(san: &str) -> (Vec<u8>, Vec<u8>) {
        let key = rcgen::KeyPair::generate().expect("自签密钥");
        let params = rcgen::CertificateParams::new(vec![san.to_string()]).expect("自签参数");
        let cert = params.self_signed(&key).expect("自签");
        (cert.der().to_vec(), key.serialize_der())
    }

    /// 伺服器端上下文：出示 `cert_der` / `key_der`，不要求用戶端證書。
    fn server_ctx(cert_der: &[u8], key_der: &[u8]) -> SslContext {
        let mut b = SslContextBuilder::new(SslMethod::tls()).expect("服务端 ctx");
        let cert = X509::from_der(cert_der).expect("服务端证书");
        let key = PKey::private_key_from_der(key_der).expect("服务端私钥");
        b.set_certificate(&cert).expect("装证书");
        b.set_private_key(&key).expect("装私钥");
        b.build()
    }

    /// ⚠ **只在測試裡存在的對照**：把證書檢核整個關掉。
    /// 產品面上沒有任何一條路能造出這樣一個連接器。
    fn insecure_connector() -> BoringHttpsConnector {
        let mut b = SslConnector::builder(SslMethod::tls()).expect("客户端 ctx");
        b.set_verify(SslVerifyMode::NONE);
        b.set_alpn_protos(ALPN_HTTP11).expect("ALPN");
        BoringHttpsConnector::with_context(b.build())
    }

    /// ⚠ **只在測試裡存在**：只信 `ca_der` 這一個根，**檢核照常全開**。
    /// ★ 它是「名字不對要紅」那一條的**正向對照**：同一個用戶端、同一個 CA，
    ///   名字對的時候必須是綠的 —— 否則那條紅說明不了任何事。
    fn ca_pinned_connector(ca_der: &[u8]) -> BoringHttpsConnector {
        let mut b = SslConnector::builder(SslMethod::tls()).expect("客户端 ctx");
        let ca = X509::from_der(ca_der).expect("CA 证书");
        b.cert_store_mut().add_cert(ca).expect("装 CA");
        b.set_alpn_protos(ALPN_HTTP11).expect("ALPN");
        BoringHttpsConnector::with_context(b.build())
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("建不出 runtime")
    }

    /// 起一臺只接一條連接的 TLS 伺服器，讓 `connector` 去連 `https://<host>:<port>/`，
    /// 回傳用戶端這一側的結果。
    fn handshake(
        mut connector: BoringHttpsConnector,
        host: &str,
        cert_der: Vec<u8>,
        key_der: Vec<u8>,
    ) -> Result<(), String> {
        rt().block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let ctx = server_ctx(&cert_der, &key_der);
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.expect("accept");
                let ssl = Ssl::new(&ctx).expect("服务端 SSL");
                let mut s = SslStream::new(ssl, tcp).expect("服务端流");
                // ⚠ 用戶端拒絕時這裡必然回報錯誤，而那正是被測的情形 —— 不能 unwrap。
                let _ = Pin::new(&mut s).accept().await;
            });
            let uri: Uri = format!("https://{host}:{port}/").parse().expect("URL 解析");
            let out = connector
                .call(uri)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
            server.abort();
            out
        })
    }

    #[test]
    fn 自签证书连不上_而关掉验证的对照连得上() {
        let (cert, key) = make_self_signed("localhost");

        // ★ 被測的那一條：產品面的連接器（系統信任庫）。
        let e = handshake(
            BoringHttpsConnector::new().expect("产品连接器"),
            "localhost",
            cert.clone(),
            key.clone(),
        )
        .expect_err("自签证书竟然连上了 —— 链校验没开");
        // ⚠ 釘 `CERTIFICATE_VERIFY_FAILED` 而不是「握手失敗」：後者連
        //   「對端根本沒出示證書」也算，而那與本條要測的性質無關。
        assert!(e.contains("CERTIFICATE_VERIFY_FAILED"), "{e}");

        // ★ ★ ★ 對照：把「被測性質」關掉，同一臺伺服器必須連得上。
        //   ⚠ 沒有這一條，上面那條紅可能只是「伺服器根本沒起來 / 連接埠不通」，
        //     而**紅色比綠色更容易讓人停止追問**（§10）。
        handshake(insecure_connector(), "localhost", cert, key)
            .expect("关掉验证之后还是连不上 —— 那说明上面那条红与证书校验无关");
    }

    #[test]
    fn 证书名字不对连不上_而同一个_ca_下名字对的连得上() {
        // 正向：SAN 就是我們要連的名字。
        let good = make_pki("localhost");
        handshake(
            ca_pinned_connector(&good.ca_der),
            "localhost",
            good.leaf_der,
            good.leaf_key_der,
        )
        .expect("名字对的都连不上 —— 那下面那条红说明不了任何事");

        // 反向：**同一套機制**，只把 SAN 換成另一個名字。
        let bad = make_pki("wrong.example");
        let e = handshake(
            ca_pinned_connector(&bad.ca_der),
            "localhost",
            bad.leaf_der,
            bad.leaf_key_der,
        )
        .expect_err("证书上写的是 wrong.example 却连上了 —— 主机名校验没开");
        assert!(e.contains("CERTIFICATE_VERIFY_FAILED"), "{e}");
    }

    #[test]
    fn 非_https_一律拒绝() {
        // ⚠ `HttpConnector` 的 `enforce_http` 被我們關掉了，這道閘就只剩這一條。
        let mut c = BoringHttpsConnector::new().expect("产品连接器");
        let e = rt().block_on(async move {
            c.call("http://example.invalid/x".parse::<Uri>().expect("URL"))
                .await
                .err()
                .expect("明文竟然没被拒")
                .to_string()
        });
        assert!(e.contains("非 HTTPS"), "{e}");
    }
}
