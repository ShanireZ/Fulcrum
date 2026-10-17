//! 自建的裸 UDP 回聲服務。
//!
//! ★ **這是整個 M0 裡最不確定、也最值錢的一條。** Pingora 自己只移交 TCP/Unix 監聽器；
//! 但它的 fd 表 `Fds` 是 `HashMap<String, RawFd>`，對協定零假設，而 `RawFd` 只是個 `i32`，
//! 傳輸走 `SCM_RIGHTS`——本來就能傳任意 fd。這裡要用真流量把這個推斷坐實。
//!
//! ⚠ **一個 UDP 獨有、TCP 沒有的性質**：升級視窗內兩代處理程序持有的是**同一個** socket，
//! 於是兩邊都會 `recv_from`，資料報會在兩代之間被**分流**。對回聲服務無所謂（誰答都一樣，
//! 因此零丟失），但對 QUIC 是真問題——連接狀態只存在於某一代處理程序裡。這條要帶給 M2（D11）。

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
    /// 見 `raw_tcp.rs` 同名欄位：用來區分「首次啟動沒有 fd 表」與「升級時交接失敗」。
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
        // ★ 理由同 raw_tcp：表缺失時新 bind 的 socket 不會被註冊，也就不參與下一次移交。
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

    // 0.9.0：ListenFds 換成 parking_lot 鎖 ⇒ 守衛不許跨 `.await`，只在查表、登記兩個瞬間持鎖。
    let inherited = table.lock().get(key).copied();

    if let Some(fd) = inherited {
        log::info!("[raw-udp] INHERITED fd={fd} for key={key}");
        // SAFETY: 同 raw_tcp——fd 由上一代經 SCM_RIGHTS 傳來，此處接管所有權且只接管一次。
        // ★ ManuallyDrop 的理由完全同 raw_tcp：提前析構 = close(fd)，而表裡那條記錄還在。
        let std_sock = ManuallyDrop::new(unsafe { std::net::UdpSocket::from_raw_fd(fd) });
        std_sock.set_nonblocking(true)?; // 失敗時不析構，fd 保持有效

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
    table.lock().add(key.to_string(), fd);
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
        // ★ 理由同 raw_tcp：不實作就不要假裝支援。UDP 這邊尤其要注意——多個任務
        //   在同一個 socket 上 `recv_from` 會把資料報分流，語義與 TCP 的多 accept 不同。
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
                            // ★ 這裡的 `.await` **是有意保留的**，不是漏了優化。
                            //   socket 是非阻塞的，send_to 只在核心傳送緩衝寫滿時才真的等；
                            //   而那意味著下游擁塞——此刻停下收包正是**正確的背壓**。
                            //   改成 spawn 只會把擁塞變成無界記憶體增長。
                            //   ⚠ QUIC 不能照抄這個結構（一條連接的狀態不能被另一條的傳送卡住），
                            //     那是 D11 選庫時要單獨設計的事。
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

    /// pingora 0.9.0：宣告本服務認領的 fd 表鍵（理由同 raw_tcp）。
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![self.key.clone()])
    }
}
