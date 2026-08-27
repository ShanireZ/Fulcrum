//! 出站 HTTPS 的连接器：**BoringSSL**（G104 第 ③ 处）。
//!
//! §5.1 第 1 条「三处全换」的第 ③ 处：本 crate 往外发的两类请求
//! （ACME 协议本身与原生 DNS 供应商的 HTTPS API）共用这一个连接器，
//! 也就共用同一套信任库、同一套校验、同一份 ALPN。
//!
//! # ★ 我们写的是**连接器胶水**，不是 TLS
//!
//! 校验全部交给 [`SslConnector`]：证书链走 `builder()` 里那句 `set_default_verify_paths()`，
//! 主机名走 `configure()?.verify_hostname(true)` → `into_ssl(domain)`。
//! ⚠ **安全基线点名不许手写的就是这一类** —— 这个文件里没有一行在比对名字或验链。
//!
//! # ⛔ 为什么不用 `hyper-boring`
//!
//! 实测 **+9 个包**（整套旧版 hyper 0.14）。根因：它的 feature 表写的是
//! `runtime = [hyper_old/runtime]` —— **没有 `dep:` 前缀**，意思是 hyper 0.14 本身是必需依赖，
//! `default-features = false` 拦不住。⇒ 自己写一个 [`tower_service::Service<Uri>`]，
//! 底下用 fork 里已有的 [`pingora_boringssl::tokio_ssl::SslStream`]，**新增 0 个包**。
//!
//! ★ 可带走的一条：**看 feature 表要看有没有 `dep:` 前缀**，而识破它要**真加进去 resolve
//! 一遍再 diff 锁** —— 清单上写着 `hyper1 = [dep:http, dep:hyper, …]`，
//! 读起来像是「不开就不进来」。
//!
//! # ⚠ 三处「不写就悄悄错」的地方
//!
//! 1. **`HttpConnector` 默认 `enforce_http = true`** —— 它一看见 `https://` 就报
//!    「invalid URL, scheme is not http」。必须 `enforce_http(false)`，
//!    ⇒ **明文的那道闸就得由我们自己守**（[`BoringHttpsConnector::call`] 第一件事
//!    就是拒非 `https`）。★ 这两条是同一枚硬币：关掉上游的检查就必须自己接住。
//! 2. **ALPN 钉死 `http/1.1`**。产品这一侧只开了 `hyper/http1`，
//!    协商到 h2 的话拿到的是一条我们不会说的协议。
//!    ⚠ 不设 ALPN 多数服务器也会回落到 HTTP/1.1，但那是**对端的默认值**，不是我们的判据。
//! 3. **主机名要去掉 IPv6 字面量的方括号**：`Uri::host()` 给的是 `[::1]`，
//!    而 `into_ssl` 要的是 `::1`（它自己按 `IpAddr::from_str` 判断要不要发 SNI）。
//!
//! # ★ 判据都配对照
//!
//! 每一条反向都配一条「把被测性质关掉」的对照，否则说明它根本没测到被测性质：
//! 自签证书连不上 / 关掉验证就连得上 · 名字不对连不上 / 同一个 CA 名字对的连得上。
//!
//! ⚠ ⚠ **「关掉验证」只存在于 `#[cfg(test)]` 里** —— 产品面上
//! [`BoringHttpsConnector::new`] 是唯一构造入口，没有任何旋钮。

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

/// ALPN 的线上格式：一个长度字节 + 协议名。
///
/// ★ 只提供 `http/1.1`，理由见模块文档第 2 条。
const ALPN_HTTP11: &[u8] = b"\x08http/1.1";

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 出站 TLS 的上下文。★ 一个进程建一次，之后每条连接从它 `configure()` 派生。
///
/// ⚠ 这里**什么都不放宽**：`SslConnector::builder` 自己就调了
/// `set_default_verify_paths()` 并把 `SSL_VERIFY_PEER` 打开
/// （boring 4.22.0 `ssl/connector.rs`），我们只额外钉一条 ALPN。
fn client_context() -> Result<SslConnector, String> {
    let mut b = SslConnector::builder(SslMethod::tls_client())
        .map_err(|e| format!("建不出出站 TLS 上下文：{e}"))?;
    b.set_alpn_protos(ALPN_HTTP11)
        .map_err(|e| format!("出站 TLS 设不上 ALPN：{e}"))?;
    Ok(b.build())
}

/// 一个把 TCP 连接升级成 TLS 的 [`tower_service::Service`]，喂给
/// [`hyper_util::client::legacy::Client`] 当连接器用。
#[derive(Clone)]
pub struct BoringHttpsConnector {
    http: HttpConnector,
    /// ★ [`SslConnector`] 是 `Clone` 的（内部就是一次 `SSL_CTX` 引用计数），
    ///   所以不需要再包一层 `Arc`。
    ssl: SslConnector,
}

impl BoringHttpsConnector {
    /// **产品面唯一的构造入口**：系统信任库 + 链校验 + 主机名校验，全部开着。
    pub fn new() -> Result<BoringHttpsConnector, String> {
        Ok(BoringHttpsConnector::with_context(client_context()?))
    }

    /// ⚠ 私有：给 [`BoringHttpsConnector::new`] 与本文件的判据共用。
    /// 判据要拿一个**故意配坏的**上下文来做对照，而那条路不许出现在产品面上。
    fn with_context(ssl: SslConnector) -> BoringHttpsConnector {
        let mut http = HttpConnector::new();
        // ⚠ 见模块文档第 1 条：不关掉它，`https://` 在建 TCP 之前就被上游拒了。
        //   关掉之后拒明文是我们自己的事，见 `call()` 的第一句。
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
    /// ⚠ `Pin<Box<…>>` 不只是图省事：`hyper_util` 的 `Connect` 要求
    /// `S::Future: Unpin + Send`，而 `Pin<Box<dyn Future + Send>>` 两条都满足。
    type Future = Pin<Box<dyn Future<Output = Result<TokioIo<BoringStream>, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Service::poll_ready(&mut self.http, cx).map_err(BoxError::from)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let ssl = self.ssl.clone();
        let mut http = self.http.clone();
        Box::pin(async move {
            // ⚠ ⚠ 这一句是 `enforce_http(false)` 的另一半，见模块文档第 1 条。
            if uri.scheme_str() != Some("https") {
                return Err(BoxError::from(format!("拒绝发非 HTTPS 请求：{uri}")));
            }
            let Some(host) = uri.host() else {
                return Err(BoxError::from(format!("URL 里没有主机名：{uri}")));
            };
            // IPv6 字面量在 `Uri::host()` 里是带方括号的，见模块文档第 3 条。
            let host = host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string();

            let tcp = http.call(uri).await?.into_inner();

            // ★ 校验全在这三行里，而这三行一个字都不是我们自己写的逻辑。
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

/// 一条已经握完手的出站 TLS 连接。
///
/// ★ ★ **它存在的唯一理由是孤儿规则**：`hyper_util` 的 [`Connection`] 与
/// `pingora-boringssl` 的 [`SslStream`] 都是外部类型，本 crate 只能给自己的类型实现它。
/// 而 `hyper_util` 那条 `impl<T: Connection> Connection for TokioIo<T>` 会把它接上去。
pub struct BoringStream(SslStream<TcpStream>);

impl Connection for BoringStream {
    fn connected(&self) -> Connected {
        // ★ 转交给底下那条 TCP：`Connected` 里带的是本地/对端地址，
        //   与 TLS 无关，而 hyper 的连接池要用它。
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

/// 给 `instant-acme` 用的 HTTP 客户端。
///
/// # ★ ★ 为什么要有这个类型
///
/// `instant-acme` 的 `hyper-rustls` feature 一关，`Account::builder()` 与
/// `BytesResponse::try_from` 就一起没了（两者都挂在那个 feature 上）
/// ⇒ 必须自己实现 [`instant_acme::HttpClient`]，并走
/// `Account::builder_with_http()` 那个入口。
///
/// ★ 而 `BytesResponse` 的 `From<http::Response<B>>` **不在 feature 门里**，
/// 所以这一层只有十来行：把 `hyper` 的响应交出去，把错误包成 `Error::Other`。
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

    /// 一个测试用的 CA + 一张由它签出来的叶子证书。
    struct TestPki {
        ca_der: Vec<u8>,
        leaf_der: Vec<u8>,
        leaf_key_der: Vec<u8>,
    }

    /// 一个只有 CN 的主体名。
    fn dn(cn: &str) -> rcgen::DistinguishedName {
        let mut d = rcgen::DistinguishedName::new();
        d.push(rcgen::DnType::CommonName, cn);
        d
    }

    /// 造一张 SAN 为 `san` 的叶子证书，外加签它的那个 CA。
    ///
    /// ★ 用 `rcgen`，它**已经在依赖图里**（`instant-acme` 拉的，见 `tlsalpn01.rs`）。
    ///
    /// ⚠ ⚠ **两个主体名必须不一样，而这是被一次红教出来的**：`CertificateParams`
    /// 的默认主体名是同一个常量，于是 CA 与叶子的 subject **逐字相同** ——
    /// 校验器会把叶子当成自签（issuer == subject）、根本不去找那个 CA，
    /// 报的是 `CERTIFICATE_VERIFY_FAILED`。
    /// ★ 抓到它的是**正向那一条**（名字对的也该连得上），而不是被测的反向那一条 ——
    /// 没有它，一次「CA 根本没生效」会被读成「主机名校验生效了」。
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

    /// 一张**自签**证书（没有 CA）。用来做「链校验开着没有」那一条。
    fn make_self_signed(san: &str) -> (Vec<u8>, Vec<u8>) {
        let key = rcgen::KeyPair::generate().expect("自签密钥");
        let params = rcgen::CertificateParams::new(vec![san.to_string()]).expect("自签参数");
        let cert = params.self_signed(&key).expect("自签");
        (cert.der().to_vec(), key.serialize_der())
    }

    /// 服务端上下文：出示 `cert_der` / `key_der`，不要求客户端证书。
    fn server_ctx(cert_der: &[u8], key_der: &[u8]) -> SslContext {
        let mut b = SslContextBuilder::new(SslMethod::tls_server()).expect("服务端 ctx");
        let cert = X509::from_der(cert_der).expect("服务端证书");
        let key = PKey::private_key_from_der(key_der).expect("服务端私钥");
        b.set_certificate(&cert).expect("装证书");
        b.set_private_key(&key).expect("装私钥");
        b.build()
    }

    /// ⚠ **只在测试里存在的对照**：把证书校验整个关掉。
    /// 产品面上没有任何一条路能造出这样一个连接器。
    fn insecure_connector() -> BoringHttpsConnector {
        let mut b = SslConnector::builder(SslMethod::tls_client()).expect("客户端 ctx");
        b.set_verify(SslVerifyMode::NONE);
        b.set_alpn_protos(ALPN_HTTP11).expect("ALPN");
        BoringHttpsConnector::with_context(b.build())
    }

    /// ⚠ **只在测试里存在**：只信 `ca_der` 这一个根，**校验照常全开**。
    /// ★ 它是「名字不对要红」那一条的**正向对照**：同一个客户端、同一个 CA，
    ///   名字对的时候必须是绿的 —— 否则那条红说明不了任何事。
    fn ca_pinned_connector(ca_der: &[u8]) -> BoringHttpsConnector {
        let mut b = SslConnector::builder(SslMethod::tls_client()).expect("客户端 ctx");
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

    /// 起一台只接一条连接的 TLS 服务器，让 `connector` 去连 `https://<host>:<port>/`，
    /// 返回客户端这一侧的结果。
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
                // ⚠ 客户端拒绝时这里必然报错，而那正是被测的情形 —— 不能 unwrap。
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

        // ★ 被测的那一条：产品面的连接器（系统信任库）。
        let e = handshake(
            BoringHttpsConnector::new().expect("产品连接器"),
            "localhost",
            cert.clone(),
            key.clone(),
        )
        .expect_err("自签证书竟然连上了 —— 链校验没开");
        // ⚠ 钉 `CERTIFICATE_VERIFY_FAILED` 而不是「握手失败」：后者连
        //   「对端根本没出示证书」也算，而那与本条要测的性质无关。
        assert!(e.contains("CERTIFICATE_VERIFY_FAILED"), "{e}");

        // ★ ★ ★ 对照：把「被测性质」关掉，同一台服务器必须连得上。
        //   ⚠ 没有这一条，上面那条红可能只是「服务器根本没起来 / 端口不通」，
        //     而**红色比绿色更容易让人停止追问**（§10）。
        handshake(insecure_connector(), "localhost", cert, key)
            .expect("关掉验证之后还是连不上 —— 那说明上面那条红与证书校验无关");
    }

    #[test]
    fn 证书名字不对连不上_而同一个_ca_下名字对的连得上() {
        // 正向：SAN 就是我们要连的名字。
        let good = make_pki("localhost");
        handshake(
            ca_pinned_connector(&good.ca_der),
            "localhost",
            good.leaf_der,
            good.leaf_key_der,
        )
        .expect("名字对的都连不上 —— 那下面那条红说明不了任何事");

        // 反向：**同一套机制**，只把 SAN 换成另一个名字。
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
        // ⚠ `HttpConnector` 的 `enforce_http` 被我们关掉了，这道闸就只剩这一条。
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
