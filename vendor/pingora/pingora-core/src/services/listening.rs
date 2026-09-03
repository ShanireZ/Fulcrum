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

//! The listening service
//!
//! A [Service] (listening service) responds to incoming requests on its endpoints.
//! Each [Service] can be configured with custom application logic (e.g. an `HTTPProxy`) and one or
//! more endpoints to listen to.

use crate::apps::ServerApp;
use crate::listeners::tls::TlsSettings;
#[cfg(feature = "connection_filter")]
use crate::listeners::AcceptAllFilter;
use crate::listeners::{
    // ★ 枢衡改动 15：`ConnGuard`。
    ConnGuard,
    ConnectionFilter,
    Listeners,
    ServerAddress,
    TcpSocketOptions,
    TransportStack,
};
use crate::protocols::Stream;
#[cfg(unix)]
use crate::server::ListenFds;
use crate::server::ShutdownWatch;
use crate::services::Service as ServiceTrait;

use async_trait::async_trait;
use log::{debug, error, info};
use pingora_error::Result;
use pingora_runtime::current_handle;
use pingora_timeout::timeout;
use std::fs::Permissions;
use std::sync::Arc;
use std::time::Duration;

/// The type of service that is associated with a list of listening endpoints and a particular application
pub struct Service<A> {
    name: String,
    listeners: Listeners,
    app_logic: Option<A>,
    /// The number of preferred threads. `None` to follow global setting.
    pub threads: Option<usize>,
    #[cfg(feature = "connection_filter")]
    connection_filter: Arc<dyn ConnectionFilter>,
}

impl<A> Service<A> {
    /// Create a new [`Service`] with the given application (see [`crate::apps`]).
    pub fn new(name: String, app_logic: A) -> Self {
        Service {
            name,
            listeners: Listeners::new(),
            app_logic: Some(app_logic),
            threads: None,
            #[cfg(feature = "connection_filter")]
            connection_filter: Arc::new(AcceptAllFilter),
        }
    }

    /// Create a new [`Service`] with the given application (see [`crate::apps`]) and the given
    /// [`Listeners`].
    pub fn with_listeners(name: String, listeners: Listeners, app_logic: A) -> Self {
        Service {
            name,
            listeners,
            app_logic: Some(app_logic),
            threads: None,
            #[cfg(feature = "connection_filter")]
            connection_filter: Arc::new(AcceptAllFilter),
        }
    }

    /// Set a custom connection filter for this service.
    ///
    /// The connection filter will be applied to all incoming connections
    /// on all endpoints of this service. Connections that don't pass the
    /// filter will be dropped immediately at the TCP level, before TLS
    /// handshake or any HTTP processing.
    ///
    /// # Feature Flag
    ///
    /// This method requires the `connection_filter` feature to be enabled.
    /// When the feature is disabled, this method is a no-op.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use pingora_core::listeners::{ConnectionFilter, AcceptAllFilter};
    /// # struct MyService;
    /// # impl MyService {
    /// #   fn new() -> Self { MyService }
    /// # }
    /// let mut service = MyService::new();
    /// let filter = Arc::new(AcceptAllFilter);
    /// service.set_connection_filter(filter);
    /// ```
    #[cfg(feature = "connection_filter")]
    pub fn set_connection_filter(&mut self, filter: Arc<dyn ConnectionFilter>) {
        self.connection_filter = filter.clone();
        self.listeners.set_connection_filter(filter);
    }

    #[cfg(not(feature = "connection_filter"))]
    pub fn set_connection_filter(&mut self, _filter: Arc<dyn ConnectionFilter>) {}

    /// Get the [`Listeners`], mostly to add more endpoints.
    pub fn endpoints(&mut self) -> &mut Listeners {
        &mut self.listeners
    }

    // the follow add* function has no effect if the server is already started

    /// Add a TCP listening endpoint with the given address (e.g., `127.0.0.1:8000`).
    pub fn add_tcp(&mut self, addr: &str) {
        self.listeners.add_tcp(addr);
    }

    /// Add a TCP listening endpoint with the given [`TcpSocketOptions`].
    pub fn add_tcp_with_settings(&mut self, addr: &str, sock_opt: TcpSocketOptions) {
        self.listeners.add_tcp_with_settings(addr, sock_opt);
    }

    /// Add a Unix domain socket listening endpoint with the given path.
    ///
    /// Optionally take a permission of the socket file. The default is read and write access for
    /// everyone (0o666).
    #[cfg(unix)]
    pub fn add_uds(&mut self, addr: &str, perm: Option<Permissions>) {
        self.listeners.add_uds(addr, perm);
    }

    /// Add a TLS listening endpoint with the given certificate and key paths.
    pub fn add_tls(&mut self, addr: &str, cert_path: &str, key_path: &str) -> Result<()> {
        self.listeners.add_tls(addr, cert_path, key_path)
    }

    /// Add a TLS listening endpoint with the given [`TlsSettings`] and [`TcpSocketOptions`].
    pub fn add_tls_with_settings(
        &mut self,
        addr: &str,
        sock_opt: Option<TcpSocketOptions>,
        settings: TlsSettings,
    ) {
        self.listeners
            .add_tls_with_settings(addr, sock_opt, settings)
    }

    /// Add an endpoint according to the given [`ServerAddress`]
    pub fn add_address(&mut self, addr: ServerAddress) {
        self.listeners.add_address(addr);
    }

    /// Get a reference to the application inside this service
    pub fn app_logic(&self) -> Option<&A> {
        self.app_logic.as_ref()
    }

    /// Get a mutable reference to the application inside this service
    pub fn app_logic_mut(&mut self) -> Option<&mut A> {
        self.app_logic.as_mut()
    }
}

impl<A: ServerApp + Send + Sync + 'static> Service<A> {
    pub async fn handle_event(event: Stream, app_logic: Arc<A>, shutdown: ShutdownWatch) {
        debug!("new event!");
        let mut reuse_event = app_logic.process_new(event, &shutdown).await;
        while let Some(event) = reuse_event {
            // TODO: with no steal runtime, consider spawn() the next event on
            // another thread for more evenly load balancing
            debug!("new reusable event!");
            reuse_event = app_logic.process_new(event, &shutdown).await;
        }
    }

    async fn run_endpoint(
        app_logic: Arc<A>,
        mut stack: TransportStack,
        mut shutdown: ShutdownWatch,
    ) {
        // ── ★ 枢衡改动 15：连接计数 ────────────────────────────────────────────
        //
        // ★ 句柄与地址都在**循环外**取一次 ⇒ 每条连接只 clone 两个 `Arc`，
        //   accept 这条热路径上**零分配**。
        let conn_counter = stack.connection_counter().cloned();
        let listen_addr: std::sync::Arc<str> = std::sync::Arc::from(stack.as_str());

        // the accept loop, until the system is shutting down
        loop {
            let new_io = tokio::select! { // TODO: consider biased for perf reason?
                new_io = stack.accept() => new_io,
                shutdown_signal = shutdown.changed() => {
                    match shutdown_signal {
                        Ok(()) => {
                            if !*shutdown.borrow() {
                                // happen in the initial read
                                continue;
                            }
                            info!("Shutting down {}", stack.as_str());
                            break;
                        }
                        Err(e) => {
                            error!("shutdown_signal error {e}");
                            break;
                        }
                    }
                }
            };
            match new_io {
                Ok(io) => {
                    let app = app_logic.clone();
                    let shutdown = shutdown.clone();
                    // ★ ★ ★ 枢衡改动 15：**`enter` 在 spawn 之前、握手之前**。
                    //   于是「还在握手的连接」也算进 active —— TLS 握手被打爆时
                    //   那条路上的堆积正是最该看得见的东西。
                    let conn_guard = conn_counter
                        .as_ref()
                        .map(|c| ConnGuard::new(c.clone(), listen_addr.clone()));
                    current_handle().spawn(async move {
                        // ⚠ ⚠ ⚠ **必须绑一个有名字的变量。** 写成 `let _ = conn_guard;`
                        //   会让它**当场 drop**，于是 gauge 恒为 0 而 counter 照涨 ——
                        //   ★ 而那种失效**不会有任何东西红**：正文格式合法、
                        //     counter 在动、系列也都在，只有一个数字永远是 0。
                        //   ⇒ 逮它的是枢衡那侧的端到端判据（连上 TLS 端口什么都不发 ⇒ active +1）。
                        let _conn_guard = conn_guard;
                        let peer_addr = io.peer_addr();
                        match timeout(Duration::from_secs(60), io.handshake()).await {
                            Ok(handshake) => {
                                match handshake {
                                    Ok(io) => Self::handle_event(io, app, shutdown).await,
                                    Err(e) => {
                                        // TODO: Maybe IOApp trait needs a fn to handle/filter out this error
                                        if let Some(addr) = peer_addr {
                                            error!("Downstream handshake error from {}: {e}", addr);
                                        } else {
                                            error!("Downstream handshake error: {e}");
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                error!("Downstream handshake timeout");
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("Accept() failed {e}");
                    if let Some(io_error) = e
                        .root_cause()
                        .downcast_ref::<std::io::Error>()
                        .and_then(|e| e.raw_os_error())
                    {
                        // 24: too many open files. In this case accept() will continue return this
                        // error without blocking, which could use up all the resources
                        if io_error == 24 {
                            // call sleep to calm the thread down and wait for others to release
                            // some resources
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }
            }
        }

        stack.cleanup();
    }
}

#[async_trait]
impl<A: ServerApp + Send + Sync + 'static> ServiceTrait for Service<A> {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        let runtime = current_handle();
        let endpoints = self
            .listeners
            .build(
                #[cfg(unix)]
                fds,
            )
            .await
            .expect("Failed to build listeners");

        let app_logic = self
            .app_logic
            .take()
            .expect("can only start_service() once");
        let app_logic = Arc::new(app_logic);

        let mut handlers = Vec::new();

        endpoints.into_iter().for_each(|endpoint| {
            for _ in 0..listeners_per_fd {
                let shutdown = shutdown.clone();
                let my_app_logic = app_logic.clone();
                let endpoint = endpoint.clone();

                let jh = runtime.spawn(async move {
                    Self::run_endpoint(my_app_logic, endpoint, shutdown).await;
                });

                handlers.push(jh);
            }
        });

        futures::future::join_all(handlers).await;
        self.listeners.cleanup();
        app_logic.cleanup().await;
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn threads(&self) -> Option<usize> {
        self.threads
    }
}

use crate::apps::prometheus_http_app::PrometheusServer;

impl Service<PrometheusServer> {
    /// The Prometheus HTTP server
    ///
    /// The HTTP server endpoint that reports Prometheus metrics collected in the entire service
    pub fn prometheus_http_service() -> Self {
        Service::new(
            "Prometheus metric HTTP".to_string(),
            PrometheusServer::new(),
        )
    }
}
