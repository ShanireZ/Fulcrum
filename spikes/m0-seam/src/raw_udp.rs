//! 自建的裸 UDP 回声服务。
//!
//! ★ **这是整个 M0 里最不确定、也最值钱的一条。** Pingora 自己只移交 TCP/Unix 监听器；
//! 但它的 fd 表 `Fds` 是 `HashMap<String, RawFd>`，对协议零假设，而 `RawFd` 只是个 `i32`，
//! 传输走 `SCM_RIGHTS`——本来就能传任意 fd。这里要用真流量把这个推断坐实。
//!
//! ⚠ **一个 UDP 独有、TCP 没有的性质**：升级窗口内两代进程持有的是**同一个** socket，
//! 于是两边都会 `recv_from`，数据报会在两代之间被**分流**。对回声服务无所谓（谁答都一样，
//! 因此零丢失），但对 QUIC 是真问题——连接状态只存在于某一代进程里。这条要带给 M2（D11）。

use crate::raw_tcp::backoff_if_fd_exhausted;
use async_trait::async_trait;
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;
use std::mem::ManuallyDrop;
use std::os::unix::io::{AsRawFd, FromRawFd};
use tokio::net::UdpSocket;

pub struct UdpEchoService {
    bind: String,
    key: String,
    /// 见 `raw_tcp.rs` 同名字段：用来区分「首次启动没有 fd 表」与「升级时交接失败」。
    upgrading: bool,
}

impl UdpEchoService {
    pub fn new(bind: &str, upgrading: bool) -> Self {
        Self {
            bind: bind.to_string(),
            key: format!("m0-raw-udp:{bind}"),
            upgrading,
        }
    }
}

async fn build_socket(
    bind: &str,
    key: &str,
    fds: Option<ListenFds>,
    upgrading: bool,
) -> std::io::Result<UdpSocket> {
    let Some(table) = fds else {
        // ★ 理由同 raw_tcp：表缺失时新 bind 的 socket 不会被注册，也就不参与下一次移交。
        if upgrading {
            return Err(std::io::Error::other(format!(
                "以 -u 升级启动却拿不到 fd 表，无法继承 {bind}：socket 移交已经失败了"
            )));
        }
        log::warn!(
            "[raw-udp] 没有 fd 表，只能在 {bind} 上新 bind —— \
             ★ 这个监听器**不会参与下一次 socket 移交**"
        );
        return UdpSocket::bind(bind).await;
    };

    let mut table = table.lock().await;

    if let Some(&fd) = table.get(key) {
        log::info!("[raw-udp] INHERITED fd={fd} for key={key}");
        // SAFETY: 同 raw_tcp——fd 由上一代经 SCM_RIGHTS 传来，此处接管所有权且只接管一次。
        // ★ ManuallyDrop 的理由完全同 raw_tcp：提前析构 = close(fd)，而表里那条记录还在。
        let std_sock = ManuallyDrop::new(unsafe { std::net::UdpSocket::from_raw_fd(fd) });
        std_sock.set_nonblocking(true)?; // 失败时不析构，fd 保持有效

        let owned = ManuallyDrop::into_inner(std_sock);
        return match UdpSocket::from_std(owned) {
            Ok(s) => Ok(s),
            Err(e) => {
                log::error!(
                    "[raw-udp] from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    let sock = UdpSocket::bind(bind).await?;
    let fd = sock.as_raw_fd();
    table.add(key.to_string(), fd);
    log::info!("[raw-udp] bound fresh on {bind}, registered fd={fd} as key={key}");
    Ok(sock)
}

#[async_trait]
impl Service for UdpEchoService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        mut shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        // ★ 理由同 raw_tcp：不实现就不要假装支持。UDP 这边尤其要注意——多个任务
        //   在同一个 socket 上 `recv_from` 会把数据报分流，语义与 TCP 的多 accept 不同。
        if listeners_per_fd > 1 {
            log::error!(
                "[raw-udp] listener_tasks_per_fd={listeners_per_fd} 不被本服务支持（它只会开 1 个接收任务）。\
                 拒绝启动，以免配置与实际行为不符。"
            );
            return;
        }

        let sock = match build_socket(&self.bind, &self.key, fds, self.upgrading).await {
            Ok(s) => s,
            Err(e) => {
                log::error!("[raw-udp] failed to build socket on {}: {e}", self.bind);
                return;
            }
        };

        let mut buf = [0u8; 2048];
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    log::info!("[raw-udp] shutdown signalled, stop receiving");
                    break;
                }
                received = sock.recv_from(&mut buf) => {
                    match received {
                        Ok((n, peer)) => {
                            // ★ 这里的 `.await` **是有意保留的**，不是漏了优化。
                            //   socket 是非阻塞的，send_to 只在内核发送缓冲写满时才真的等；
                            //   而那意味着下游拥塞——此刻停下收包正是**正确的背压**。
                            //   改成 spawn 只会把拥塞变成无界内存增长。
                            //   ⚠ QUIC 不能照抄这个结构（一条连接的状态不能被另一条的发送卡住），
                            //     那是 D11 选库时要单独设计的事。
                            if let Err(e) = sock.send_to(&buf[..n], peer).await {
                                log::error!("[raw-udp] send_to {peer} failed: {e}");
                            }
                        }
                        Err(e) => {
                            log::error!("[raw-udp] recv_from error: {e}");
                            backoff_if_fd_exhausted(&e, "raw-udp").await;
                        }
                    }
                }
            }
        }
    }

    fn name(&self) -> &str {
        "m0-raw-udp"
    }
}
