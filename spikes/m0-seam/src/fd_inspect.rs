//! 只读的 fd 表探查服务——**什么都不认领**，只把收到的整张表打出来。
//!
//! 存在理由：`Server::listen_fds()` 是私有的，从 `main` 里看不到 fd 表。而
//! [`docs/verification/open-seams.md`](../../../docs/verification/open-seams.md) 要验的
//! 「未被认领的 fd 会怎样」，第一步就是**证明那个 fd 确实还在表里**。
//!
//! ★ 它同时也证明了一件事：`Fds::get()` **不会把条目移走**（`server/transfer_fd/mod.rs:51`
//! 返回的是引用），所以「被认领」与「留在表里」是两回事——整张表都会原样传给下一代。
//!
//! ⚠ **它看到的是自己启动那一刻的快照，不是最终状态。** 首次启动（非升级）时它多半
//! 报告 `table has 0 entries`——因为别的服务还没来得及把自己 bind 出来的 fd 注册进表。
//! 判据要写成「跨代的增量」而不是「绝对条数」，否则会随服务启动顺序变化。

use async_trait::async_trait;
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;

pub struct FdInspectService;

/// 日志前缀。测试脚本按它抓行，改动前先看 `tests/m0/unclaimed.sh`。
pub const TAG: &str = "[fd-inspect]";

#[async_trait]
impl Service for FdInspectService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        mut shutdown: ShutdownWatch,
        // ★ 本服务不持有任何监听器，所以 `listener_tasks_per_fd` 对它**确实不适用**——
        //   这与 raw_tcp/raw_udp 那两处「不支持就拒绝启动」是两回事，别照抄。
        _listeners_per_fd: usize,
    ) {
        match fds {
            None => log::info!("{TAG} no fd table (fresh start)"),
            Some(table) => {
                let (keys, values) = table.lock().await.serialize();
                log::info!("{TAG} table has {} entries", keys.len());
                for (key, fd) in keys.iter().zip(values.iter()) {
                    log::info!("{TAG} entry key={key} fd={fd}");
                }
            }
        }

        // 什么都不做，等停机。它不持有任何监听器，所以不影响别的服务。
        let _ = shutdown.changed().await;
        log::info!("{TAG} shutdown");
    }

    fn name(&self) -> &str {
        "m0-fd-inspect"
    }

    fn threads(&self) -> Option<usize> {
        // G35：线程不跨 service 共享，纯日志服务给 1 个就够。
        Some(1)
    }
}
