//! 自建的裸 TCP 回聲服務——**不經過 Pingora 的 listener 體系**，直接用 `tokio::net::TcpListener`。
//!
//! 取/放 fd 的寫法照抄 Pingora 自己在 `listeners/l4.rs::listen()` 裡的規範做法：
//! 表裡有就繼承，沒有就自己 bind 再放回去，這樣它才會被傳給下一代處理程序。

use async_trait::async_trait;
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;
use std::mem::ManuallyDrop;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct TcpEchoService {
    bind: String,
    /// fd 表裡的鍵。刻意加 `m0-raw-tcp:` 前綴——Pingora 原生服務用的是裸 `addr:port`，
    /// 加前綴就不會相撞，同時也證明鍵空間是自由的。
    key: String,
    /// 本處理程序是不是以 `-u` 啟動的。★ 只有知道這一點，才能把「fd 表缺失」
    /// 分成「首次啟動，正常」與「升級時交接失敗，必須回報錯誤」兩種情形。
    upgrading: bool,
}

impl TcpEchoService {
    pub fn new(bind: &str, upgrading: bool) -> Self {
        Self {
            bind: bind.to_string(),
            key: format!("m0-raw-tcp:{bind}"),
            upgrading,
        }
    }
}

async fn build_listener(
    bind: &str,
    key: &str,
    fds: Option<ListenFds>,
    upgrading: bool,
) -> std::io::Result<TcpListener> {
    let Some(table) = fds else {
        // ★ 表缺失時**不能裝作沒事**：這條路徑下 bind 出來的監聽器不會被註冊，
        //   於是它不參與下一次 socket 移交，升級時連接會被重置——而且悄無聲息。
        //   上游 `b633683b` 描述的 `bootstrap_as_a_service` 場景就會真的走到這裡。
        if upgrading {
            return Err(std::io::Error::other(format!(
                "以 -u 升级启动却拿不到 fd 表，无法继承 {bind}：socket 移交已经失败了"
            )));
        }
        log::warn!(
            "[raw-tcp] 没有 fd 表，只能在 {bind} 上新 bind —— \
             ★ 这个监听器**不会参与下一次 socket 移交**"
        );
        return TcpListener::bind(bind).await;
    };

    // 0.9.0：ListenFds 換成 parking_lot 鎖 ⇒ 守衛不許跨 `.await`，只在查表、登記兩個瞬間持鎖。
    let inherited = table.lock().get(key).copied();

    if let Some(fd) = inherited {
        // ── 繼承路徑：這一條走通，就等於證明了自建 TCP 監聽器參與了 socket 移交
        log::info!("[raw-tcp] INHERITED fd={fd} for key={key}");
        // SAFETY: fd 由上一代處理程序經 SCM_RIGHTS 傳來，此處接管其所有權，且不會被重複接管
        //（同一個鍵在表裡只取用一次）。
        //
        // ★ ★ 用 ManuallyDrop 包住：從這裡到成功交出所有權之間，**任何提前析構都會
        //   `close(fd)`**，而 `Fds` 表裡那條記錄仍指著這個號碼——號碼隨後可能被任意
        //   `open()` 重複使用，下一次升級就會把一個無關的 fd（紀錄檔案、升級 socket）
        //   當成監聽 socket 發給下一代。`Fds` 沒有 `remove()`，所以只能不讓它被關。
        let std_listener = ManuallyDrop::new(unsafe { std::net::TcpListener::from_raw_fd(fd) });
        std_listener.set_nonblocking(true)?; // 失敗時不析構，fd 保持有效

        // 成功走到這裡才真正交出所有權。
        let owned = ManuallyDrop::into_inner(std_listener);
        return match TcpListener::from_std(owned) {
            Ok(l) => Ok(l),
            Err(e) => {
                // ⚠ 殘餘風險：`from_std` 消耗了 owned，失敗時 fd 已被關閉，而表裡那條
                //   記錄仍指向它。這條路徑只在 tokio reactor 註冊失敗時出現（資源耗盡），
                //   無法在不改 Fds 公開介面的前提下修掉——所以至少讓它**大聲說出來**。
                log::error!(
                    "[raw-tcp] from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    // ── 首次啟動路徑：自己 bind，然後把 fd 放回表裡，供下一代繼承
    let listener = TcpListener::bind(bind).await?;
    let fd = listener.as_raw_fd();
    table.lock().add(key.to_string(), fd);
    log::info!("[raw-tcp] bound fresh on {bind}, registered fd={fd} as key={key}");
    Ok(listener)
}

/// 空閒多久就斷開。★ 沒有這條時，一個連上來什麼都不發的對端會**永久**佔住一個任務——
/// 慢速連接耗盡（slowloris 那一類）的最基本形態。M0 的探針最長間隔 20ms，30 秒有巨大餘量。
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

async fn echo(mut sock: TcpStream) {
    let mut buf = [0u8; 4096];
    loop {
        match tokio::time::timeout(IDLE_TIMEOUT, sock.read(&mut buf)).await {
            Err(_) => return, // 空閒超時
            Ok(Ok(0)) => return,
            Ok(Ok(n)) => {
                if sock.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
            Ok(Err(_)) => return,
        }
    }
}

/// accept()/recv() 出錯時該不該歇一下。
///
/// ★ ★ **fd 耗盡（EMFILE/ENFILE）時這兩個系統呼叫會立刻回傳錯誤且不阻塞**，
/// 於是「記一條紀錄然後繼續循環」就變成滿速空轉 + 紀錄洪水——把「fd 快用完了」
/// 放大成「這臺機器廢了」，而且恰好發生在資源最緊張的時候。
///
/// 這不是推演：Pingora 自己的 accept 循環就帶著這道防護，註解寫的是同一件事
/// （`vendor/pingora/pingora-core/src/services/listening.rs`：
/// *"24: too many open files. In this case accept() will continue return this error
/// without blocking, which could use up all the resources"*），處理方式也是 sleep 1 秒。
///
/// ★ 本檔案開頭寫著「取/放 fd 的寫法照抄 Pingora 的規範做法」——fd 那半照抄了，
///   accept 循環的硬化很容易漏掉。M1 的 L4 監聽器以它為範本，所以在這裡補齊。
pub async fn backoff_if_fd_exhausted(e: &std::io::Error, tag: &str) {
    // 24 = EMFILE（本處理程序 fd 用盡），23 = ENFILE（系統級用盡）
    if matches!(e.raw_os_error(), Some(24) | Some(23)) {
        log::error!("[{tag}] fd 耗尽（{e}），退避 1 秒——否则这个循环会满速空转");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[async_trait]
impl Service for TcpEchoService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        mut shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        // ★ 這個參數由 `listener_tasks_per_fd` 設定驅動，Pingora 原生服務會真的按它開
        //   多個 accept 任務。本服務**沒有實作**它——與其一聲不響地只開一個（設定就成了謊話），
        //   不如直接拒絕啟動。M1 寫真的 L4/QUIC 服務時要麼實作它，要麼保留這道拒絕。
        if listeners_per_fd > 1 {
            log::error!(
                "[raw-tcp] listener_tasks_per_fd={listeners_per_fd} 不被本服务支持（它只会开 1 个 accept 任务）。\
                 拒绝启动，以免配置与实际行为不符。"
            );
            return;
        }

        let listener = match build_listener(&self.bind, &self.key, fds, self.upgrading).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("[raw-tcp] failed to build listener on {}: {e}", self.bind);
                return;
            }
        };

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    // 停止 accept，但已建立的連接留在本代處理程序上繼續跑完
                    //（優雅退出的排空由 Pingora 的 grace period 控制）
                    log::info!("[raw-tcp] shutdown signalled, stop accepting");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((sock, _peer)) => { tokio::spawn(echo(sock)); }
                        Err(e) => {
                            log::error!("[raw-tcp] accept error: {e}");
                            backoff_if_fd_exhausted(&e, "raw-tcp").await;
                        }
                    }
                }
            }
        }
    }

    fn name(&self) -> &str {
        "m0-raw-tcp"
    }

    /// pingora 0.9.0：宣告本服務認領的 fd 表鍵 —— 於是 gen2 丟掉 raw-tcp 時，那個 fd 會被關掉
    /// （`tests/m0/unclaimed.sh` 驗的就是這件事）。⛔ 回傳 `None` 會讓整個處理程序的清理關閉。
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![self.key.clone()])
    }
}
