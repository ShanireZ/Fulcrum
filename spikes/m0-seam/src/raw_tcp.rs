//! 自建的裸 TCP 回声服务——**不经过 Pingora 的 listener 体系**，直接用 `tokio::net::TcpListener`。
//!
//! 取/放 fd 的写法照抄 Pingora 自己在 `listeners/l4.rs::listen()` 里的规范做法：
//! 表里有就继承，没有就自己 bind 再放回去，这样它才会被传给下一代进程。

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
    /// fd 表里的键。刻意加 `m0-raw-tcp:` 前缀——Pingora 原生服务用的是裸 `addr:port`，
    /// 加前缀就不会相撞，同时也证明键空间是自由的。
    key: String,
    /// 本进程是不是以 `-u` 启动的。★ 只有知道这一点，才能把「fd 表缺失」
    /// 分成「首次启动，正常」与「升级时交接失败，必须报错」两种情形。
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
        // ★ 表缺失时**不能装作没事**：这条路径下 bind 出来的监听器不会被注册，
        //   于是它不参与下一次 socket 移交，升级时连接会被重置——而且悄无声息。
        //   上游 `b633683b` 描述的 `bootstrap_as_a_service` 场景就会真的走到这里。
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

    let mut table = table.lock().await;

    if let Some(&fd) = table.get(key) {
        // ── 继承路径：这一条走通，就等于证明了自建 TCP 监听器参与了 socket 移交
        log::info!("[raw-tcp] INHERITED fd={fd} for key={key}");
        // SAFETY: fd 由上一代进程经 SCM_RIGHTS 传来，此处接管其所有权，且不会被重复接管
        //（同一个键在表里只取用一次）。
        //
        // ★ ★ 用 ManuallyDrop 包住：从这里到成功交出所有权之间，**任何提前析构都会
        //   `close(fd)`**，而 `Fds` 表里那条记录仍指着这个号码——号码随后可能被任意
        //   `open()` 复用，下一次升级就会把一个无关的 fd（日志文件、升级 socket）
        //   当成监听 socket 发给下一代。`Fds` 没有 `remove()`，所以只能不让它被关。
        let std_listener = ManuallyDrop::new(unsafe { std::net::TcpListener::from_raw_fd(fd) });
        std_listener.set_nonblocking(true)?; // 失败时不析构，fd 保持有效

        // 成功走到这里才真正交出所有权。
        let owned = ManuallyDrop::into_inner(std_listener);
        return match TcpListener::from_std(owned) {
            Ok(l) => Ok(l),
            Err(e) => {
                // ⚠ 残余风险：`from_std` 消耗了 owned，失败时 fd 已被关闭，而表里那条
                //   记录仍指向它。这条路径只在 tokio reactor 注册失败时出现（资源耗尽），
                //   无法在不改 Fds 公开接口的前提下修掉——所以至少让它**大声说出来**。
                log::error!(
                    "[raw-tcp] from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    // ── 首次启动路径：自己 bind，然后把 fd 放回表里，供下一代继承
    let listener = TcpListener::bind(bind).await?;
    let fd = listener.as_raw_fd();
    table.add(key.to_string(), fd);
    log::info!("[raw-tcp] bound fresh on {bind}, registered fd={fd} as key={key}");
    Ok(listener)
}

/// 空闲多久就断开。★ 没有这条时，一个连上来什么都不发的对端会**永久**占住一个任务——
/// 慢速连接耗尽（slowloris 那一类）的最基本形态。M0 的探针最长间隔 20ms，30 秒有巨大余量。
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

async fn echo(mut sock: TcpStream) {
    let mut buf = [0u8; 4096];
    loop {
        match tokio::time::timeout(IDLE_TIMEOUT, sock.read(&mut buf)).await {
            Err(_) => return, // 空闲超时
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

/// accept()/recv() 出错时该不该歇一下。
///
/// ★ ★ **fd 耗尽（EMFILE/ENFILE）时这两个系统调用会立刻返回错误且不阻塞**，
/// 于是「记一条日志然后继续循环」就变成满速空转 + 日志洪水——把「fd 快用完了」
/// 放大成「这台机器废了」，而且恰好发生在资源最紧张的时候。
///
/// 这不是推演：Pingora 自己的 accept 循环就带着这道防护，注释写的是同一件事
/// （`vendor/pingora/pingora-core/src/services/listening.rs`：
/// *"24: too many open files. In this case accept() will continue return this error
/// without blocking, which could use up all the resources"*），处理方式也是 sleep 1 秒。
///
/// ★ 本文件开头写着「取/放 fd 的写法照抄 Pingora 的规范做法」——fd 那半照抄了，
///   accept 循环的硬化很容易漏掉。M1 的 L4 监听器以它为模板，所以在这里补齐。
pub async fn backoff_if_fd_exhausted(e: &std::io::Error, tag: &str) {
    // 24 = EMFILE（本进程 fd 用尽），23 = ENFILE（系统级用尽）
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
        // ★ 这个参数由 `listener_tasks_per_fd` 配置驱动，Pingora 原生服务会真的按它开
        //   多个 accept 任务。本服务**没有实现**它——与其一声不响地只开一个（配置就成了谎话），
        //   不如直接拒绝启动。M1 写真的 L4/QUIC 服务时要么实现它，要么保留这道拒绝。
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
                    // 停止 accept，但已建立的连接留在本代进程上继续跑完
                    //（优雅退出的排空由 Pingora 的 grace period 控制）
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
}
