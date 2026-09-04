// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The listening endpoints (TCP and TLS) and their configurations.
//!
//! This module provides the infrastructure for setting up network listeners
//! that accept incoming connections. It supports TCP, Unix domain sockets,
//! and TLS endpoints.
//!
//! # Connection Filtering
//!
//! With the `connection_filter` feature enabled, this module also provides
//! early connection filtering capabilities through the [`ConnectionFilter`] trait.
//! This allows dropping unwanted connections at the TCP level before any
//! expensive operations like TLS handshakes.
//!
//! ## Example with Connection Filtering
//!
//! ```rust,no_run
//! # #[cfg(feature = "connection_filter")]
//! # {
//! use pingora_core::listeners::{Listeners, ConnectionFilter};
//! use std::sync::Arc;
//!
//! // Create a custom filter
//! let filter = Arc::new(MyCustomFilter::new());
//!
//! // Apply to listeners
//! let mut listeners = Listeners::new();
//! listeners.set_connection_filter(filter);
//! listeners.add_tcp("0.0.0.0:8080");
//! # }
//! ```

mod l4;

#[cfg(feature = "connection_filter")]
pub mod connection_filter;

#[cfg(feature = "connection_filter")]
pub use connection_filter::{AcceptAllFilter, ConnectionFilter};

#[cfg(not(feature = "connection_filter"))]
#[derive(Debug, Clone)]
pub struct AcceptAllFilter;

#[cfg(not(feature = "connection_filter"))]
pub trait ConnectionFilter: std::fmt::Debug + Send + Sync {
    fn should_accept(&self, _addr: &std::net::SocketAddr) -> bool {
        true
    }
}

#[cfg(not(feature = "connection_filter"))]
impl ConnectionFilter for AcceptAllFilter {
    fn should_accept(&self, _addr: &std::net::SocketAddr) -> bool {
        true
    }
}
#[cfg(feature = "any_tls")]
pub mod tls;

#[cfg(not(feature = "any_tls"))]
pub use crate::tls::listeners as tls;

use crate::protocols::{l4::socket::SocketAddr, tls::TlsRef, Stream};

#[cfg(unix)]
use crate::server::ListenFds;

use async_trait::async_trait;
use pingora_error::Result;
use std::{any::Any, fs::Permissions, sync::Arc};

use l4::{ListenerEndpoint, Stream as L4Stream};
use tls::{Acceptor, TlsSettings};

pub use crate::protocols::tls::ALPN;
use crate::protocols::{GetSocketDigest, SocketDigest};
pub use l4::{ServerAddress, TcpSocketOptions};

use pingora_error::{Error, ErrorType};
use tokio::io::AsyncReadExt;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

// ─── ★ ★ ★ 枢衡改动 12：PROXY protocol 的「收」半边（2026-08-27）──────────────
//
// 上游**完全不支持 PROXY protocol**（全库零命中）。枢衡要在 HTTP 面收它，
// 而唯一的正确位置是 **TLS 握手之前**拿到裸 `L4Stream` 的这一处
// —— 它是 `pub(crate)`，外部够不到，所以必须动 fork。
//
// ★ ★ **这里放的是接缝，不是判断**：信任清单与 v1/v2 解析**一行都不进 pingora**，
//   它们留在 `fulcrum_runtime::proxyproto`（已有 28 条单测）。
//   本文件只负责「循环读 + 调用 + 覆盖地址」，而这三件事都与协议内容无关。
//
// ⚠ rebase 时要重做的是：本段、`Listeners` / `TransportStackBuilder` /
//   `TransportStack` / `UninitializedStream` 四处各一个字段、`set_proxy_protocol`、
//   以及 `handshake()` 里那一次调用。

/// 本模块自己的错误类型。⚠ 命名沿用 `TLS_CONF_ERR` 的形状。
pub const PROXY_PROTOCOL_ERR: ErrorType = ErrorType::Custom("ProxyProtocolError");

/// 读一次 PROXY 头读到哪儿了。
#[derive(Debug)]
pub enum ProxyProtocolVerdict {
    /// 还判不出来，要更多字节。里面那个数只是「至少还要几个」的提示，**不是承诺**。
    Need(usize),
    /// 判完了。
    ///
    /// ⚠ `client` 为 `None` 是**正常结果**：`LOCAL` 与 `PROXY UNKNOWN` 表示
    /// 「这条连接没有真实客户端」（上游 LB 的健康检查就长这样）
    /// ⇒ 此时**不覆盖**对端地址，继续用 socket 对端。
    Done {
        client: Option<std::net::SocketAddr>,
        /// 这个头一共吃掉了前面几个字节。**后面的都是应用数据，一个都不能丢。**
        consumed: usize,
    },
    /// 这不是一个合法的 PROXY 头 ⇒ 关连接。
    Invalid(String),
}

/// 谁可以对本监听器发 PROXY 头，以及那串字节怎么解析。
///
/// ★ 两个方法都由使用方实现；本 crate 不认识 PROXY protocol 的任何一个字节。
pub trait ProxyProtocolPolicy: std::fmt::Debug + Send + Sync {
    /// 这个对端在信任清单里吗？
    ///
    /// ⚠ ⚠ **返回 `false` 时本 crate 一个字节都不会读**（不是「读掉丢弃」）。
    /// 那是有意的：v2 头自带一个 u16 长度字段，「读掉丢弃」必须先解析
    /// **攻击者控制**的那两个字节才知道丢多少，而「不读」完全不碰。
    ///
    /// ⚠ `peer` 为 `None` = 拿不到 inet 对端（例如 Unix domain socket）。
    fn trusts(&self, peer: Option<&std::net::SocketAddr>) -> bool;

    /// 喂进**已经读到的全部字节**，问它判得出来了没有。
    ///
    /// ⚠ ⚠ **必须是纯函数**：同一条连接上它会被反复调用，每次带着更长的前缀，
    /// 而本 crate 依赖「同样的输入给同样的答案」。
    fn feed(&self, buf: &[u8]) -> ProxyProtocolVerdict;
}

/// 这个监听器上多了一条 / 少了一条连接（**★ 枢衡改动 15**）。
///
/// ★ ★ **本 crate 只知道「多了一条 / 少了一条」**：它不认识 counter 与 gauge、
/// 不认识标签、不知道 Prometheus 存在。`listen` 递的是 [`TransportStack::as_str`]
/// 给出的那个**监听地址原样**，怎么用由使用方决定。
///
/// ⚠ 一个 [`Listeners`] 可以有多个监听地址，而 [`Listeners::set_connection_counter`]
/// 给它们设的是**同一个**实现 ⇒ 实现方必须按 `listen` 分格，
/// ⛔ 不能假设「一个计数器只服务一个地址」。
pub trait ConnectionCounter: Send + Sync {
    /// 一条连接开始了。
    ///
    /// ⚠ 调用点在 **accept 之后、握手之前** —— 于是「还在握手的连接」也算在里面。
    /// ★ 那是有意的：TLS 握手被打爆时，这条路上的堆积正是最该看得见的东西。
    fn enter(&self, listen: &str);

    /// 那条连接结束了。
    ///
    /// ⛔ ⛔ **不要手写调用它** —— 唯一的调用点是 [`ConnGuard`] 的 `Drop`，理由见那里。
    fn leave(&self, listen: &str);
}

/// [`ConnectionCounter::leave`] 的**唯一**调用点（**★ 枢衡改动 15**）。
///
/// ★ ★ ★ 它存在的全部理由：那条连接任务有**三条退出路径**
/// （握手超时 / 握手失败 / 正常结束），手写三处减一是一个「三处迟早分家」的形状 ——
/// ⚠ 而分家时的表现是 **gauge 只涨不降**，counter 照常，**没有任何东西会说**。
///
/// **构造即 `enter`，析构即 `leave`** —— 两半绑在同一个值的生命期上，
/// 于是「加了没减」在结构上做不到。
pub struct ConnGuard {
    counter: Arc<dyn ConnectionCounter>,
    listen: Arc<str>,
}

impl ConnGuard {
    /// 构造即 [`ConnectionCounter::enter`]。
    pub fn new(counter: Arc<dyn ConnectionCounter>, listen: Arc<str>) -> ConnGuard {
        counter.enter(&listen);
        ConnGuard { counter, listen }
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.counter.leave(&self.listen);
    }
}

/// 读 PROXY 头时缓冲区的**硬上界**。
///
/// ★ 它不是协议上界（那由 [`ProxyProtocolPolicy::feed`] 自己的 `Invalid` 给），
/// 而是**这个循环一定会停**的最后一道保证 —— 与 `fulcrum-server::l4` 里那个循环同构。
/// ⚠ 正常情况下走不到：v1 的一行 ≤107 字节，v2 是 16 字节固定头 + payload。
const PROXY_PROTOCOL_HARD_CAP: usize = 4096;

/// 在 TLS 握手之前读掉一个 PROXY 头，并把对端地址换成它报的那个。
///
/// 返回 `Err` = **关掉这条连接**。⚠ 这与「不在清单里」有意相反：一个**在信任清单里**的
/// 对端发来坏头（或干脆不发），说明配置或对端出了问题，而此时我们**已经吃掉了一部分字节、
/// 还原不回去** —— 把残缺的流交给上层只会把问题推远。
async fn read_proxy_protocol(
    stream: &mut L4Stream,
    policy: &dyn ProxyProtocolPolicy,
) -> Result<()> {
    let peer = stream
        .get_socket_digest()
        .and_then(|d| d.peer_addr().and_then(|a| a.as_inet().copied()));
    if !policy.trusts(peer.as_ref()) {
        // ★ ★ 一个字节都不读，原样交给上层。
        return Ok(());
    }

    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 256];
    loop {
        // ★ 先拿已有的字节问一次 —— `feed` 是纯函数，重复问不要钱。
        match policy.feed(&buf) {
            ProxyProtocolVerdict::Done { client, consumed } => {
                // ★ ★ ★ **多读到的字节必须还回去。** TCP 是流，一次 read 很可能把
                //   PROXY 头与它后面的 ClientHello（或请求行）**一起**读回来。
                //   少了这一步，上层拿到的流就从半截开始 —— 而那不会有任何报错，
                //   只表现为「TLS 握手莫名其妙失败」。
                if consumed < buf.len() {
                    stream.rewind(&buf[consumed..]);
                }
                if let Some(addr) = client {
                    override_peer_addr(stream, addr);
                }
                return Ok(());
            }
            ProxyProtocolVerdict::Invalid(why) => {
                return Error::e_explain(PROXY_PROTOCOL_ERR, format!("bad PROXY header: {why}"));
            }
            ProxyProtocolVerdict::Need(_) => {}
        }

        if buf.len() >= PROXY_PROTOCOL_HARD_CAP {
            return Error::e_explain(
                PROXY_PROTOCOL_ERR,
                format!("PROXY header exceeded {PROXY_PROTOCOL_HARD_CAP} bytes"),
            );
        }

        // ⚠ 这里**故意没有自己的超时**：`services/listening.rs` 已经把整个
        //   `handshake()` 包在一个 60s 的 timeout 里，而多一个数字就多一处要同步的地方。
        match stream.read(&mut chunk).await {
            Ok(0) => {
                return Error::e_explain(
                    PROXY_PROTOCOL_ERR,
                    "peer closed the connection while we waited for its PROXY header",
                );
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => {
                return Error::e_explain(PROXY_PROTOCOL_ERR, format!("read failed: {e}"));
            }
        }
    }
}

/// 把这条连接的对端地址换成 PROXY 头报的那个。
///
/// # ⚠ ⚠ ★ ★ ★ 为什么是「换一整份 digest」而不是 `peer_addr.set(...)`
///
/// `SocketDigest.peer_addr` 是 `pub` 的 `OnceCell`，看起来 `set()` 一下就行 ——
/// **而那行不通，因为它已经被填过了**：
/// `services/listening.rs` 在 `io.handshake()` **之前**调 `io.peer_addr()`
/// （为了握手失败时那行日志里能带上地址），而 `SocketDigest::peer_addr()` 是 `get_or_init`。
/// ⇒ `OnceCell::set()` 到这一步必然返回 `Err`，**而它的返回值很容易被忽略**，
///    于是「地址没换成」会是一次完全无声的失效。
///
/// ⇒ 换一整份。`local_addr` / `original_dst` 都会从**同一个 fd** 重新惰性派生，
/// 什么都不丢。
fn override_peer_addr(stream: &mut L4Stream, client: std::net::SocketAddr) {
    #[cfg(unix)]
    let digest = SocketDigest::from_raw_fd(stream.as_raw_fd());
    #[cfg(windows)]
    let digest = SocketDigest::from_raw_socket(stream.as_raw_socket());
    // ★ 新造的 digest，这个 `set` 一定成功；写成 `let _ =` 是因为返回值确实无话可说。
    let _ = digest.peer_addr.set(Some(SocketAddr::Inet(client)));
    stream.set_socket_digest(digest);
}

/// The APIs to customize things like certificate during TLS server side handshake
#[async_trait]
pub trait TlsAccept {
    // TODO: return error?
    /// This function is called in the middle of a TLS handshake. Structs who
    /// implement this function should provide tls certificate and key to the
    /// [TlsRef] via `ssl_use_certificate` and `ssl_use_private_key`.
    /// Note. This is only supported for openssl and boringssl
    async fn certificate_callback(&self, _ssl: &mut TlsRef) -> () {
        // does nothing by default
    }

    /// This function is called after the TLS handshake is complete.
    ///
    /// Any value returned from this function (other than `None`) will be stored in the
    /// `extension` field of `SslDigest`. This allows you to attach custom application-specific
    /// data to the TLS connection, which will be accessible from the HTTP layer via the
    /// `SslDigest` attached to the session digest.
    async fn handshake_complete_callback(
        &self,
        _ssl: &TlsRef,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

pub type TlsAcceptCallbacks = Box<dyn TlsAccept + Send + Sync>;

struct TransportStackBuilder {
    l4: ServerAddress,
    tls: Option<TlsSettings>,
    #[cfg(feature = "connection_filter")]
    connection_filter: Option<Arc<dyn ConnectionFilter>>,
    /// ★ 枢衡改动 12：PROXY protocol 的「收」半边。`None` = 这个端口不收。
    proxy_protocol: Option<Arc<dyn ProxyProtocolPolicy>>,
    /// ★ 枢衡改动 15：连接计数。`None` = 这个端口不数。
    connection_counter: Option<Arc<dyn ConnectionCounter>>,
}

impl TransportStackBuilder {
    pub async fn build(
        &mut self,
        #[cfg(unix)] upgrade_listeners: Option<ListenFds>,
    ) -> Result<TransportStack> {
        let mut builder = ListenerEndpoint::builder();

        builder.listen_addr(self.l4.clone());

        #[cfg(feature = "connection_filter")]
        if let Some(filter) = &self.connection_filter {
            builder.connection_filter(filter.clone());
        }

        #[cfg(unix)]
        let l4 = builder.listen(upgrade_listeners).await?;

        #[cfg(windows)]
        let l4 = builder.listen().await?;

        Ok(TransportStack {
            l4,
            tls: self.tls.take().map(|tls| Arc::new(tls.build())),
            proxy_protocol: self.proxy_protocol.clone(),
            connection_counter: self.connection_counter.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct TransportStack {
    l4: ListenerEndpoint,
    tls: Option<Arc<Acceptor>>,
    /// ★ 枢衡改动 12。
    proxy_protocol: Option<Arc<dyn ProxyProtocolPolicy>>,
    /// ★ 枢衡改动 15。
    connection_counter: Option<Arc<dyn ConnectionCounter>>,
}

impl TransportStack {
    pub fn as_str(&self) -> &str {
        self.l4.as_str()
    }

    /// ★ 枢衡改动 15：这个端点上的连接计数器（`None` = 不数）。
    pub fn connection_counter(&self) -> Option<&Arc<dyn ConnectionCounter>> {
        self.connection_counter.as_ref()
    }

    pub async fn accept(&self) -> Result<UninitializedStream> {
        let stream = self.l4.accept().await?;
        Ok(UninitializedStream {
            l4: stream,
            tls: self.tls.clone(),
            proxy_protocol: self.proxy_protocol.clone(),
        })
    }

    pub fn cleanup(&mut self) {
        // placeholder
    }
}

pub(crate) struct UninitializedStream {
    l4: L4Stream,
    tls: Option<Arc<Acceptor>>,
    /// ★ 枢衡改动 12。
    proxy_protocol: Option<Arc<dyn ProxyProtocolPolicy>>,
}

impl UninitializedStream {
    pub async fn handshake(mut self) -> Result<Stream> {
        self.l4.set_buffer();
        // ★ ★ 枢衡改动 12：PROXY 头在这里读 —— **TLS 握手之前**。
        //   ⚠ 位置不能挪到 `accept()` 里：那条路是**串行的接受循环**，
        //     在那儿等一个慢客户端会把整个监听器堵住。这里已经是 per-connection
        //     的 spawn，而且上层还包了一个 60s 的 timeout。
        if let Some(policy) = self.proxy_protocol.take() {
            read_proxy_protocol(&mut self.l4, policy.as_ref()).await?;
        }
        if let Some(tls) = self.tls {
            let tls_stream = tls.tls_handshake(self.l4).await?;
            Ok(Box::new(tls_stream))
        } else {
            Ok(Box::new(self.l4))
        }
    }

    /// Get the peer address of the connection if available
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.l4
            .get_socket_digest()
            .and_then(|d| d.peer_addr().cloned())
    }
}

/// The struct to hold one more multiple listening endpoints
pub struct Listeners {
    stacks: Vec<TransportStackBuilder>,
    #[cfg(feature = "connection_filter")]
    connection_filter: Option<Arc<dyn ConnectionFilter>>,
    /// ★ 枢衡改动 12。
    proxy_protocol: Option<Arc<dyn ProxyProtocolPolicy>>,
    /// ★ 枢衡改动 15。
    connection_counter: Option<Arc<dyn ConnectionCounter>>,
}

impl Listeners {
    /// Create a new [`Listeners`] with no listening endpoints.
    pub fn new() -> Self {
        Listeners {
            stacks: vec![],
            #[cfg(feature = "connection_filter")]
            connection_filter: None,
            proxy_protocol: None,
            connection_counter: None,
        }
    }
    /// Create a new [`Listeners`] with a TCP server endpoint from the given string.
    pub fn tcp(addr: &str) -> Self {
        let mut listeners = Self::new();
        listeners.add_tcp(addr);
        listeners
    }

    /// Create a new [`Listeners`] with a Unix domain socket endpoint from the given string.
    #[cfg(unix)]
    pub fn uds(addr: &str, perm: Option<Permissions>) -> Self {
        let mut listeners = Self::new();
        listeners.add_uds(addr, perm);
        listeners
    }

    /// Create a new [`Listeners`] with a TLS (TCP) endpoint with the given address string,
    /// and path to the certificate/private key pairs.
    /// This endpoint will adopt the [Mozilla Intermediate](https://wiki.mozilla.org/Security/Server_Side_TLS#Intermediate_compatibility_.28recommended.29)
    /// server side TLS settings.
    pub fn tls(addr: &str, cert_path: &str, key_path: &str) -> Result<Self> {
        let mut listeners = Self::new();
        listeners.add_tls(addr, cert_path, key_path)?;
        Ok(listeners)
    }

    /// Add a TCP endpoint to `self`.
    pub fn add_tcp(&mut self, addr: &str) {
        self.add_address(ServerAddress::Tcp(addr.into(), None));
    }

    /// Add a TCP endpoint to `self`, with the given [`TcpSocketOptions`].
    pub fn add_tcp_with_settings(&mut self, addr: &str, sock_opt: TcpSocketOptions) {
        self.add_address(ServerAddress::Tcp(addr.into(), Some(sock_opt)));
    }

    /// Add a Unix domain socket endpoint to `self`.
    #[cfg(unix)]
    pub fn add_uds(&mut self, addr: &str, perm: Option<Permissions>) {
        self.add_address(ServerAddress::Uds(addr.into(), perm));
    }

    /// Add a TLS endpoint to `self` with the [Mozilla Intermediate](https://wiki.mozilla.org/Security/Server_Side_TLS#Intermediate_compatibility_.28recommended.29)
    /// server side TLS settings.
    pub fn add_tls(&mut self, addr: &str, cert_path: &str, key_path: &str) -> Result<()> {
        self.add_tls_with_settings(addr, None, TlsSettings::intermediate(cert_path, key_path)?);
        Ok(())
    }

    /// Add a TLS endpoint to `self` with the given socket and server side TLS settings.
    /// See [`TlsSettings`] and [`TcpSocketOptions`] for more details.
    pub fn add_tls_with_settings(
        &mut self,
        addr: &str,
        sock_opt: Option<TcpSocketOptions>,
        settings: TlsSettings,
    ) {
        self.add_endpoint(ServerAddress::Tcp(addr.into(), sock_opt), Some(settings));
    }

    /// Add the given [`ServerAddress`] to `self`.
    pub fn add_address(&mut self, addr: ServerAddress) {
        self.add_endpoint(addr, None);
    }

    /// Set a connection filter for all endpoints in this listener collection
    #[cfg(feature = "connection_filter")]
    pub fn set_connection_filter(&mut self, filter: Arc<dyn ConnectionFilter>) {
        log::debug!("Setting connection filter on Listeners");

        // Store the filter for future endpoints
        self.connection_filter = Some(filter.clone());

        // Apply to existing stacks
        for stack in &mut self.stacks {
            stack.connection_filter = Some(filter.clone());
        }
    }

    /// ★ 枢衡改动 12：给**所有**端点（已有的与之后加的）设 PROXY protocol 的收取策略。
    ///
    /// ⚠ ⚠ 它是**全局**的，与 `set_connection_filter` 同一个形状，而这不是省事：
    /// 收不收 PROXY 头是**连接级**判断 —— 一条连接上还没有 Host，还不知道会落到哪个站点。
    pub fn set_proxy_protocol(&mut self, policy: Arc<dyn ProxyProtocolPolicy>) {
        self.proxy_protocol = Some(policy.clone());
        for stack in &mut self.stacks {
            stack.proxy_protocol = Some(policy.clone());
        }
    }

    /// ★ 枢衡改动 15：给**所有**端点（已有的与之后加的）设连接计数器。
    ///
    /// ⚠ 与 `set_proxy_protocol` 及上游自己的 `set_connection_filter` 同一个形状，
    /// 而这不是省事：连接计数是**连接级**的 —— 一条连接上还没有 Host，
    /// 还不知道它会落到哪个站点。
    ///
    /// ⚠ ⚠ 这里设的是**同一个**实现，而 `self.stacks` 可以有多个监听地址
    /// ⇒ 实现方必须按 [`ConnectionCounter::enter`] 收到的 `listen` 分格。
    pub fn set_connection_counter(&mut self, counter: Arc<dyn ConnectionCounter>) {
        self.connection_counter = Some(counter.clone());
        for stack in &mut self.stacks {
            stack.connection_counter = Some(counter.clone());
        }
    }

    /// Add the given [`ServerAddress`] to `self` with the given [`TlsSettings`] if provided
    pub fn add_endpoint(&mut self, l4: ServerAddress, tls: Option<TlsSettings>) {
        self.stacks.push(TransportStackBuilder {
            l4,
            tls,
            #[cfg(feature = "connection_filter")]
            connection_filter: self.connection_filter.clone(),
            proxy_protocol: self.proxy_protocol.clone(),
            connection_counter: self.connection_counter.clone(),
        })
    }

    pub(crate) async fn build(
        &mut self,
        #[cfg(unix)] upgrade_listeners: Option<ListenFds>,
    ) -> Result<Vec<TransportStack>> {
        let mut stacks = Vec::with_capacity(self.stacks.len());

        for b in self.stacks.iter_mut() {
            let new_stack = b
                .build(
                    #[cfg(unix)]
                    upgrade_listeners.clone(),
                )
                .await?;

            stacks.push(new_stack);
        }

        Ok(stacks)
    }

    pub(crate) fn cleanup(&self) {
        // placeholder
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[cfg(feature = "connection_filter")]
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(feature = "any_tls")]
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tokio::time::{sleep, Duration};

    /// ★ 枢衡改动 15 的回归守卫（手法照改动 13：**长在上游自己的测试模块里**）。
    ///
    /// ⚠ **它只覆盖 [`ConnGuard`] 本身，到不了 `run_endpoint` 里那个调用点** ——
    /// 那一段由下面 [`枢衡改动15_守卫必须被移进任务且绑了名字`] 守（2026-09-04 补）。
    /// ⛔ **两条别合并**：这一条验**行为**（构造即 enter、drop 即 leave），
    /// 那一条验**接线**（那个值真的被移进了任务）—— 合并之后哪一半坏了都分不出来。
    /// ★ 端到端那一层仍然有它自己的判据（`tests/metrics/run.sh` 里
    /// 「连上 TLS 端口什么都不发 ⇒ active +1」），三层各守一段。
    #[test]
    fn 枢衡改动15_连接守卫在_drop_时减一() {
        use std::sync::atomic::{AtomicUsize as 计数, Ordering as 序};
        #[derive(Default)]
        struct 假计数器 {
            进: 计数,
            出: 计数,
            最后地址: std::sync::Mutex<String>,
        }
        impl ConnectionCounter for 假计数器 {
            fn enter(&self, listen: &str) {
                self.进.fetch_add(1, 序::Relaxed);
                *self.最后地址.lock().unwrap() = listen.to_string();
            }
            fn leave(&self, _listen: &str) {
                self.出.fetch_add(1, 序::Relaxed);
            }
        }

        let c = Arc::new(假计数器::default());
        {
            let _g = ConnGuard::new(c.clone(), Arc::from("127.0.0.1:1"));
            assert_eq!(c.进.load(序::Relaxed), 1, "构造 guard 就该 enter 一次");
            assert_eq!(c.出.load(序::Relaxed), 0, "还没 drop，不该 leave");
        }
        // ★ ★ 这一条是整条改动的全部意义：**离开作用域就减一，无论那条路怎么走**。
        assert_eq!(c.出.load(序::Relaxed), 1, "drop 之后必须 leave 一次");
        assert_eq!(
            *c.最后地址.lock().unwrap(),
            "127.0.0.1:1",
            "递过去的是监听地址原样"
        );
    }

    /// ★ ★ ★ 枢衡改动 15 的**第二道**守卫：`run_endpoint` 里那个守卫必须真的被
    /// **移进** spawn 出去的任务，并且绑在一个**有名字**的变量上（2026-09-04 补）。
    ///
    /// ⚠ ⚠ 它守的是一个**编译器不会说、门禁也不会红**的失效，而且有两种形态：
    ///
    /// 1. `let _conn_guard = conn_guard;` 写成裸 `let _ = conn_guard;` ⇒ **当场 drop**；
    /// 2. 把那一行**整个删掉** ⇒ 那个值就不再被移进 `async move` 块，
    ///    于是在 accept 循环这一轮末尾就 drop 了 —— 后果与第 1 种**一模一样**。
    ///
    /// 两种的现场都是 **gauge 恒为 0 而 counter 照涨**：正文格式合法、series 都在，
    /// 只有一个数字永远是 0。⇒ 这正是「一道只见过绿的门」最爱藏的地方。
    #[test]
    fn 枢衡改动15_守卫必须被移进任务且绑了名字() {
        // 把空白全去掉再判：`rustfmt` 换行与缩进变化不会误伤，
        // 而两种形态在归一之后仍然是**不同**的字符串。
        fn 归一(s: &str) -> String {
            s.chars().filter(|c| !c.is_whitespace()).collect()
        }
        // ★ ★ ★ 判「有没有做某件事」之前**必须先剥掉整行注释**。
        //   ⚠ ⚠ 这不是洁癖：`listening.rs` 里那句**警告**本身就写着
        //   「⛔ 写成裸 `let _ = conn_guard;` 会当场 drop」——**本门第一次跑就红在它上面**，
        //   判据把那句警告当成了坏代码。
        //   ⚠ **只剥整行**（`^\s*//`）：行内 `//` 未必是注释（`http://`、字符串里的 `//`），
        //   一刀切会把真代码剪掉 —— 那是另一个方向的假绿（判据看不见真做了的事）。
        //   ⚠ 已知边界：**行尾注释剥不掉**。真要藏一句坏形态在行尾注释里能骗过它，
        //   而本文件不写行尾注释，且那需要有人刻意去做 —— 写在明处，不假装它全能。
        fn 去整行注释(src: &str) -> String {
            src.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        }
        // 一份源码里那个守卫接得对不对。★ 正向与反向**走同一个函数**，
        // 否则「反向能红」证明不了「正向判得动」。
        fn 接得对(src: &str) -> bool {
            let s = 归一(&去整行注释(src));
            s.contains("ConnGuard::new(")
                && s.contains("let_conn_guard=conn_guard;")
                && !s.contains("let_=conn_guard")
        }

        // ★ 剥离器自己的两条自证（剥得掉整行 ∧ 剥不掉行内的 `//`）。
        assert_eq!(
            去整行注释("  // 整行注释\nlet a = 1;"),
            "let a = 1;",
            "整行注释没被剥掉 ⇒ 下面那条正向判据会被注释里的警告误伤"
        );
        assert!(
            去整行注释("let u = \"http://x\";").contains("http://x"),
            "把行内的 `//` 也当注释剪掉了 ⇒ 判据会看不见真代码"
        );

        // ★ ★ **反向那一半，与正向同一次运行**（照 `tests/m0/unclaimed.sh` 的先例）。
        //   ⛔ 少了它，一个恒回 `true` 的判据在下面那条正向上是绿的。
        //   ⚠ 这几份夹具里出现的坏形态**不会误伤正向**：正向读的是
        //     `services/listening.rs`，⛔ **别把正向改成读本文件**。
        assert!(
            接得对("let conn_guard = ConnGuard::new(c, a);\nlet _conn_guard = conn_guard;"),
            "判据把正确的接法判成错的 ⇒ 它守不住任何东西"
        );
        assert!(
            !接得对("let conn_guard = ConnGuard::new(c, a);\nlet _ = conn_guard;"),
            "形态 1（裸 `let _ =`，当场 drop）没被认出来"
        );
        assert!(
            !接得对("let conn_guard = ConnGuard::new(c, a);\n// 那一行被删了"),
            "形态 2（守卫压根没被移进任务）没被认出来"
        );
        // ★ ★ ★ 第四份夹具＝**本门第一次跑时那个 bug 的回归测试**：
        //   接法正确、而**注释里提到了坏形态**的源码必须被判为**对的**。
        //   ⚠ 少了它，有人「顺手简化」掉 `去整行注释` 之后不会有任何东西红 ——
        //   而那时这道门会对着一份完全正确的 `listening.rs` 判红。
        assert!(
            接得对(
                "// ⛔ 写成裸 `let _ = conn_guard;` 会当场 drop\n\
                 let conn_guard = ConnGuard::new(c, a);\nlet _conn_guard = conn_guard;"
            ),
            "注释里提到坏形态的正确源码被判成了错的 ⇒ `去整行注释` 没在起作用"
        );

        // 正向：真的那份源码。
        assert!(
            接得对(include_str!("../services/listening.rs")),
            "`services/listening.rs` 里那个连接守卫没被移进任务、或没绑名字 ⇒ \
             active 会恒为 0 而 total 照涨，且不会有任何东西报错"
        );
    }

    #[tokio::test]
    async fn test_listen_tcp() {
        let addr1 = "127.0.0.1:7101";
        let addr2 = "127.0.0.1:7102";
        let mut listeners = Listeners::tcp(addr1);
        listeners.add_tcp(addr2);

        let listeners = listeners
            .build(
                #[cfg(unix)]
                None,
            )
            .await
            .unwrap();

        assert_eq!(listeners.len(), 2);
        for listener in listeners {
            tokio::spawn(async move {
                // just try to accept once
                let stream = listener.accept().await.unwrap();
                stream.handshake().await.unwrap();
            });
        }

        // make sure the above starts before the lines below
        sleep(Duration::from_millis(10)).await;

        TcpStream::connect(addr1).await.unwrap();
        TcpStream::connect(addr2).await.unwrap();
    }

    #[tokio::test]
    #[cfg(feature = "any_tls")]
    async fn test_listen_tls() {
        use tokio::io::AsyncReadExt;

        let addr = "127.0.0.1:7103";
        let cert_path = format!("{}/tests/keys/server.crt", env!("CARGO_MANIFEST_DIR"));
        let key_path = format!("{}/tests/keys/key.pem", env!("CARGO_MANIFEST_DIR"));
        let mut listeners = Listeners::tls(addr, &cert_path, &key_path).unwrap();
        let listener = listeners
            .build(
                #[cfg(unix)]
                None,
            )
            .await
            .unwrap()
            .pop()
            .unwrap();

        tokio::spawn(async move {
            // just try to accept once
            let stream = listener.accept().await.unwrap();
            let mut stream = stream.handshake().await.unwrap();
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na")
                .await
                .unwrap();
        });
        // make sure the above starts before the lines below
        sleep(Duration::from_millis(10)).await;

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

        let res = client.get(format!("https://{addr}")).send().await.unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::OK);
    }

    #[cfg(feature = "connection_filter")]
    #[test]
    fn test_connection_filter_inheritance() {
        #[derive(Debug, Clone)]
        struct TestFilter {
            counter: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ConnectionFilter for TestFilter {
            async fn should_accept(&self, _addr: Option<&std::net::SocketAddr>) -> bool {
                self.counter.fetch_add(1, Ordering::SeqCst);
                true
            }
        }

        let mut listeners = Listeners::new();

        // Add an endpoint before setting filter
        listeners.add_tcp("127.0.0.1:7104");

        // Set the connection filter
        let filter = Arc::new(TestFilter {
            counter: Arc::new(AtomicUsize::new(0)),
        });
        listeners.set_connection_filter(filter.clone());

        // Add endpoints after setting filter
        listeners.add_tcp("127.0.0.1:7105");
        #[cfg(feature = "any_tls")]
        {
            // Only test TLS if the feature is enabled
            if let Ok(tls_settings) = TlsSettings::intermediate(
                &format!("{}/tests/keys/server.crt", env!("CARGO_MANIFEST_DIR")),
                &format!("{}/tests/keys/key.pem", env!("CARGO_MANIFEST_DIR")),
            ) {
                listeners.add_tls_with_settings("127.0.0.1:7106", None, tls_settings);
            }
        }

        // Verify all stacks have the filter (only when feature is enabled)
        for stack in &listeners.stacks {
            assert!(
                stack.connection_filter.is_some(),
                "All stacks should have the connection filter set"
            );
        }
    }
}
