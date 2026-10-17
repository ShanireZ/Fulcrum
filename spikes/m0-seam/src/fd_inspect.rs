//! 唯讀的 fd 表探查服務——**什麼都不認領**，只把收到的整張表打出來。
//!
//! 存在理由：`Server::listen_fds()` 是私有的，從 `main` 裡看不到 fd 表。而
//! [`docs/verification/open-seams.md`](../../../docs/verification/open-seams.md) 要驗的
//! 「未被認領的 fd 會怎樣」，第一步就是**證明那個 fd 確實還在表裡**。
//!
//! ★ 它同時也證明了一件事：`Fds::get()` **不會把條目移走**（`server/transfer_fd/mod.rs:51`
//! 回傳的是引用），所以「被認領」與「留在表裡」是兩回事——整張表都會原樣傳給下一代。
//!
//! ⚠ **它看到的是自己啟動那一刻的快照，不是最終狀態。** 首次啟動（非升級）時它多半
//! 報告 `table has 0 entries`——因為別的服務還沒來得及把自己 bind 出來的 fd 註冊進表。
//! 判據要寫成「跨代的增量」而不是「絕對條數」，否則會隨服務啟動順序變化。

use async_trait::async_trait;
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;

pub struct FdInspectService;

/// 紀錄前綴。測試指令碼按它抓行，改動前先看 `tests/m0/unclaimed.sh`。
pub const TAG: &str = "[fd-inspect]";

#[async_trait]
impl Service for FdInspectService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        mut shutdown: ShutdownWatch,
        // ★ 本服務不持有任何監聽器，所以 `listener_tasks_per_fd` 對它**確實不適用**——
        //   這與 raw_tcp/raw_udp 那兩處「不支援就拒絕啟動」是兩回事，別照抄。
        _listeners_per_fd: usize,
    ) {
        match fds {
            None => log::info!("{TAG} no fd table (fresh start)"),
            Some(table) => {
                let (keys, values) = table.lock().serialize();
                log::info!("{TAG} table has {} entries", keys.len());
                for (key, fd) in keys.iter().zip(values.iter()) {
                    log::info!("{TAG} entry key={key} fd={fd}");
                }
            }
        }

        // 什麼都不做，等停機。它不持有任何監聽器，所以不影響別的服務。
        let _ = shutdown.changed().await;
        log::info!("{TAG} shutdown");
    }

    fn name(&self) -> &str {
        "m0-fd-inspect"
    }

    /// pingora 0.9.0：本服務不持有任何監聽器 ⇒ 宣告一個**空**集合（⛔ 不是 `None` ——
    /// 那會讓整個處理程序的未認領 fd 清理靜默關閉，`unclaimed.sh` 就又回到重現舊缺陷了）。
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![])
    }

    fn threads(&self) -> Option<usize> {
        // G35：執行緒不跨 service 共享，純紀錄服務給 1 個就夠。
        Some(1)
    }
}
