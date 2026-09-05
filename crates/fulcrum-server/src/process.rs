//! 进程托管：让产品二进制真的能被 systemd 管起来（G78）。
//!
//! 少了其中任何一件，`systemctl start` 就会 `Result=timeout`（而数据面本身好好地在听）：
//!
//! | # | 什么 | 不补的后果 |
//! |---|---|---|
//! | ① | `sd_notify(READY=1)` | `Type=notify` 永远等不到就绪 ⇒ `systemctl start` 超时失败 |
//! | ② | 写 pid 文件 | `ExecReload` 找不到当前这一代 ⇒ **零停机换代做不了** |
//! | ③ | `SIGUSR2` 换代触发器 | ⚠ 比「什么都不做」更糟：它的默认动作是**终止进程** |
//! | ④ | 停机时长的两个值 | pingora 默认 grace 是 **300 秒**，`TimeoutStopSec=60` 会在排空到一半时 SIGKILL |
//!
//! > ★ ★ **一个 spike 证明的是「这条路走得通」，不是「产品走在这条路上」。**
//! > M1 的 systemd 场景一度跑的是 spike 二进制，而 spike 把 ①②③ 自己实现了一遍。
//! > **夹具喂给门的那个二进制，本身也是夹具的一部分。**
//!
//! # 逐条继承 spike 趟过的坑
//!
//! - `NOTIFY_SOCKET` 不在时 `sd_notify::notify()` 返回 `Ok(())` 而**不是**错误；
//! - **绝不能**用 `notify_and_unset_env()`（下一代靠继承拿这个变量）；
//! - pid 文件必须 write + rename（`ExecReload` 会 `cat` 它，可能撞上写到一半）；
//! - 必须在服务**真的起来之后**才发 READY、才写 pid 文件；
//! - `spawn` 失败**绝不能**再给自己发 SIGQUIT（那会把「升级失败」变成「服务没了」）。
//!
//! ⚠ **不要照抄 MAINPID 交接**：G37 已否掉它（交接过去的 pid 不是 systemd 亲生的，
//! 会被标成 alien，此后停机不再等排空），`tests/m1/mainpid-handover.sh` 钉着它。

use std::process::Command;
use std::thread;

use log::{error, info, warn};
use pingora_core::server::ExecutionPhase;
use pingora_core::server::configuration::ServerConf;
use tokio::sync::broadcast::error::RecvError;

/// 排空窗口的默认值（秒）。
///
/// ⚠ **不是随手填的**：不给的话 pingora 用自己的 `EXIT_TIMEOUT = 300 秒`，
/// 于是按 `TimeoutStopSec=60` 写的 unit 会在排空进行到 1/5 时被 SIGKILL，
/// `systemctl stop` 以 `failed (signal)` 收场，**连接被硬断**。
/// 30 与 `process-model.md` 那句「35–65 秒量级」的下端一致（30 + 5）。
/// 要改用别的值，在 DSL 里写 `grace_period`。
pub const DEFAULT_GRACE_PERIOD_SECS: u64 = 30;

// ── 线程模型（G35 结案 · G140 落地）────────────────────────────────────────
//
// ★ ★ ★ **最要紧的一条机制事实：线程不跨 service 共享。** pingora `ServerConf` 的注释
//   原话是 "The threads are not shared across services" —— 每个 service 各起一套
//   runtime ⇒ **总线程数 ≈ Σ(各 service 的 threads)**。枢衡注定有 4+ 个 service，
//   ⇒ **把一个笼统的全局 `threads` 设成核数会直接超订**（4 核 × 4 service = 16 个 worker）。
//   这正是 DSL 面按角色分三格、⛔ 而没有一个全局 `threads` 的全部理由。
//   推导见 `docs/architecture/process-model.md` 的「线程模型」一节。
//
// ⚠ ★ **初值不是结论。** `process-model.md` 明写判据在 M3 的对拍数据里，
//   而那些数今天还不存在（合格宿主不存在，G132）。⛔ 别把下面三个常量读成
//   「量过之后定下来的」—— 它们是 G35 拍的**起步值**。

/// L4 面（TCP/UDP 透传）每个 service 的缺省线程数（**G35**）。
///
/// ★ 取 2 而不是核数：L4 那条路径是**字节搬运**，没有 HTTP 解析、没有路由匹配，
/// 单位工作量小得多；而它每多一个线程就是一整套 runtime（见上面那条机制事实）。
pub const DEFAULT_L4_THREADS: usize = 2;

/// 管理面与后台任务的缺省线程数（**G35**）。
///
/// ★ ★ 它同时是 pingora `ServerConf.threads` 的取值 ⇒ **一切没有被逐 service 覆盖的
/// 东西都跟着它**（后台服务就是这一类：`background_service()` 不设 `threads`，
/// `Service::threads()` 返回 `None` 并回落到全局值）。⇒ 它是「其余一切」那一格。
pub const DEFAULT_ADMIN_THREADS: usize = 1;

/// service 的角色（**G35 / G140**）。
///
/// ⚠ 只有三个，⛔ **h3 不单列**：它就是数据面入口，与 h1/h2 服务同一批流量、
/// 走同一条执行链（`tests/h3/run.sh` 里那条「h3 与 h1 跑同一条执行链」的判据），
/// ⇒ 归 [`ThreadRole::L7`]（G140 ③，owner 拍板）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    /// L7 数据面：h1/h2 监听器、**h3 监听器**。
    L7,
    /// L4 面：TCP 透传、UDP 透传。
    L4,
    /// 管理面 socket，以及一切没被逐 service 覆盖的东西（后台任务）。
    AdminAndBackground,
}

/// 角色缺省。**这是那些数字唯一的一处**。
///
/// ⛔ 不许在 `Global::default()` 或别处再写一份：缺省值住两处，就会有一天两处分家
/// 而彼此都自洽 —— 本仓 2026-09-05 当天刚栽过一次同形状的（G135 的分解式抄了三处）。
///
/// ★ `cpus` 由调用方传进来（[`detected_cpus`]），**不在这里去问系统**：
/// 判据要能在两个方向上被喂合成输入，而「这台机器有几个核」是判不了两次的。
pub fn default_threads(role: ThreadRole, cpus: usize) -> usize {
    match role {
        // ⚠ 夹在值域里：`cpus` 是探测来的，而配置那条路的合法值域是
        //   `[MIN_THREADS, MAX_THREADS]` —— 两条路给出的数不该落在不同的值域里。
        //   （一台 2048 核的机器上，不夹就会得到一个配置面**写不出来**的值。）
        ThreadRole::L7 => cpus.clamp(
            fulcrum_config::model::MIN_THREADS,
            fulcrum_config::model::MAX_THREADS,
        ),
        ThreadRole::L4 => DEFAULT_L4_THREADS,
        ThreadRole::AdminAndBackground => DEFAULT_ADMIN_THREADS,
    }
}

/// 探测可用并行度。
///
/// ★ 用 `std::thread::available_parallelism()`，⛔ 不引第三方 crate：它是 std，
/// 且在 Linux 上**认 cgroup 配额** —— 容器里给了 2 核就报 2，而不是宿主机的核数。
///
/// ⚠ 探测失败时回落到 1 **并打一行 warn**：这一格没有「宁可多」的安全侧 ——
/// 超订会让所有 service 一起变慢，而回落到 1 至少与今天的行为相同。
/// ⛔ 但不许静默：一个悄悄退成 1 的数据面，与「配置生效了」在启动日志里长得一样。
pub fn detected_cpus() -> usize {
    match std::thread::available_parallelism() {
        Ok(n) => n.get(),
        Err(e) => {
            warn!(
                "问不出可用并行度（{e}）——L7 的线程数回落到 1。\
                 要指定就在全局块里写 `threads_l7 <n>`"
            );
            1
        }
    }
}

/// 某个角色**这一趟真正用几个线程**：配置里写了就用配置的，没写就用角色缺省。
pub fn threads_for(role: ThreadRole, g: &fulcrum_config::model::Global, cpus: usize) -> usize {
    let configured = match role {
        ThreadRole::L7 => g.threads_l7,
        ThreadRole::L4 => g.threads_l4,
        ThreadRole::AdminAndBackground => g.threads_admin,
    };
    configured.unwrap_or_else(|| default_threads(role, cpus))
}

/// 排空之后、等各 runtime 退出的最后一段（秒）。
///
/// ★ 这个数**与 pingora 的默认值相同**（5），所以显式接上它不改变任何行为。
/// 那为什么还要接：**停机预算这件事必须有一个能被读到的来源。**
/// 不接的话，「停机要花多久」= 我们的 grace + 一个藏在上游源码里的常量，
/// 而运维要按这个和去设 `TimeoutStopSec`。现在两个加数都在这个文件里，
/// [`shutdown_budget_secs`] 负责把和算出来，`serve()` 启动时把它打进日志。
pub const GRACEFUL_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

/// pingora 在 `grace_period_seconds` 是 `None` 时会用的值（`EXIT_TIMEOUT = 60 * 5`）。
///
/// ★ ★ 它出现在这里**只为一件事**：让 [`shutdown_budget_secs`] 报的是
/// **pingora 真的会等多久**，而不是「我们希望它等多久」。
/// ⚠ 回落到 [`DEFAULT_GRACE_PERIOD_SECS`] 是错的 —— 那样一来，
/// 无论 `grace_period_seconds` 是 `Some(30)` 还是 `None`，这把尺子都报 35，
/// 于是启动日志里那行数字**对本批要防的那个缺陷完全是瞎的**。
/// 一把两种情况下读数相同的尺子，量不出这两种情况的差别。
const PINGORA_DEFAULT_GRACE_SECS: u64 = 300;
/// pingora 在 `graceful_shutdown_timeout_seconds` 是 `None` 时会用的值。
///
/// ⚠ 它与 [`GRACEFUL_SHUTDOWN_TIMEOUT_SECS`] **数值相同纯属巧合**，含义相反：
/// 那个是「我们要它等多久」，这个是「不告诉它的话它等多久」。
const PINGORA_DEFAULT_GRACEFUL_SECS: u64 = 5;

/// 造 Pingora 的 [`ServerConf`]。
///
/// ★ ★ **抽成一个纯函数，是为了让判据挂在产品函数的产物上。**
/// 这条教训是批 7（TLS-ALPN-01）那天付学费买的：当时那个单测
/// 自己用 `rcgen` 拼了一张一模一样的证书去检查扩展，**测的是 rcgen 的行为，
/// 不是我们的函数**。`serve()` 返回 `!`、里面还要绑端口，单测碰不了它；
/// 而「停机预算」「daemon 恒 false」这些恰恰是最需要门的东西。
pub fn build_server_conf(
    cfg: &fulcrum_config::StructuredConfig,
    opts: &crate::ServeOptions,
    cpus: usize,
) -> ServerConf {
    ServerConf {
        // ★ ★ 全局 `threads` = **管理面与后台**那个角色（G35 / G140）。
        //   pingora 拿它当**回落值**：`Service::threads()` 返回 `None` 的 service
        //   （后台任务全是这一类）都跟着它 ⇒ 它是「其余一切」那一格，
        //   ⛔ 不是「所有 service 的线程数」。L7 与 L4 各自逐 service 覆盖它。
        // ⚠ ⚠ 这里**绝不能**填核数：线程不跨 service 共享 ⇒ 那会让每一个
        //   没被覆盖的 service 都起一整套核数大小的 runtime。
        threads: threads_for(ThreadRole::AdminAndBackground, &cfg.global, cpus),
        pid_file: opts.pid_file.clone(),
        upgrade_sock: opts.upgrade_sock.clone(),
        // 全局选项里的 `grace_period` 映到 Pingora 的排空窗口；没写就用我们自己的默认值。
        // ⚠ **不能留 `None`**——那是上面 DEFAULT_GRACE_PERIOD_SECS 文档里说的那个陷阱。
        grace_period_seconds: Some(
            cfg.global
                .grace_period_ms
                .map(|ms| ms / 1000)
                .unwrap_or(DEFAULT_GRACE_PERIOD_SECS),
        ),
        graceful_shutdown_timeout_seconds: Some(GRACEFUL_SHUTDOWN_TIMEOUT_SECS),
        // ★ `daemon` 恒 false 是整条进程模型的前提（G31/G33），不是偏好：
        //   `daemonize()` 会 fork，而 pingora 自己的注释写着「`run()` 之前创建的
        //   任何线程都会丢失」——下面那个 SIGUSR2 触发器正是在 `run()` 之前建的。
        //   一旦它变成 true，**换代触发器会安静地消失**：进程照常服务流量，
        //   只是 `systemctl reload` 从此什么都不做。产品不从 YAML 读 `ServerConf`
        //   （`Opt::default()` 的 `conf` 是 `None`），所以这里是它唯一的来源，
        //   由下面的单测钉住。
        ..Default::default()
    }
}

/// 一次优雅停机最多花多久（秒）——`TimeoutStopSec` 必须大于它。
///
/// ★ ★ **回落值取的是 pingora 的，不是我们的。** 这把尺子要能量出
/// 「忘了给 `grace_period_seconds` 赋值」这件事——那正是本批要防的缺陷，
/// 而它的现场表现就是这个数从 35 跳到 305。回落到我们自己的默认值会让两种情况读数相同。
///
/// ⚠ 这不是全部：**换代**时老一代还要多等 `CLOSE_TIMEOUT`（5 秒，pingora 写死在
/// `server/mod.rs`）。那一段发生在 `systemctl reload` 里，不在 `systemctl stop` 的
/// 计时窗口内，所以不算进这个和。
pub fn shutdown_budget_secs(conf: &ServerConf) -> u64 {
    conf.grace_period_seconds
        .unwrap_or(PINGORA_DEFAULT_GRACE_SECS)
        .saturating_add(
            conf.graceful_shutdown_timeout_seconds
                .unwrap_or(PINGORA_DEFAULT_GRACEFUL_SECS),
        )
}

/// 把「当前这一代是谁」原子地写进 pid 文件。
///
/// ★ ★ 为什么必须是 write + rename 而不是直接写：`ExecReload` 会去 `cat` 这个文件，
/// 而它随时可能与新一代的写撞上。直接写会让 reload 读到半截内容（甚至空文件），
/// 于是 `kill -USR2 ""` 静默失败——**一次什么都没做的 reload**，没有任何症状。
/// rename 在同一文件系统上是原子的，读者要么看到旧的一代，要么看到新的一代。
fn write_pid_file(path: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if path.is_empty() {
        return Ok(());
    }
    let tmp = format!("{path}.tmp.{}", std::process::id());
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{}", std::process::id())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// 起一个线程：等服务真的起来，写 pid 文件，然后告诉 systemd 就绪。
///
/// ⚠ **必须在 `run_forever()` 之前调用**——它会 move 掉 `server`，
/// 而 `phase_rx` 只能从 `server` 上订阅。
pub fn spawn_readiness(
    phase_rx: tokio::sync::broadcast::Receiver<ExecutionPhase>,
    pid_file: String,
) {
    if let Err(e) = thread::Builder::new()
        .name("fulcrum-sd-notify".into())
        .spawn(move || notify_when_running(phase_rx, pid_file))
    {
        // ⚠ 起不来就必须喊：`Type=notify` 下这等于「这个进程永远不会就绪」。
        error!("起不来就绪通知线程（{e}）—— systemd 将等不到 READY=1，systemctl start 会超时失败");
    }
}

/// 等 [`ExecutionPhase::Running`]，再写 pid 文件、再发 `READY=1`。
///
/// ★ 订阅的是相位广播，**不是 sleep 一会儿**：pingora 在所有 service 都启动完之后
/// 才发 `Running`。对换代而言这正是要的那一刻——新一代已经把 fd 取走并开始 accept 了。
fn notify_when_running(
    mut phase_rx: tokio::sync::broadcast::Receiver<ExecutionPhase>,
    pid_file: String,
) {
    loop {
        match phase_rx.blocking_recv() {
            Ok(ExecutionPhase::Running) => break,
            // ★ `Lagged` 不能当成结束：广播缓冲区满时会丢掉中间的相位，但 `Running`
            //   可能就在被丢掉的那批之后——继续收即可。当成错误退出会让 unit 永远等不到 READY。
            Err(RecvError::Lagged(n)) => {
                warn!("execution phase 广播落后 {n} 条，继续等 Running");
            }
            Err(RecvError::Closed) => {
                error!("execution phase 广播已关闭，永远等不到 Running，不发 READY");
                return;
            }
            Ok(_) => {}
        }
    }

    let pid = std::process::id();

    // ★ 顺序是有意的：**先落 pid 文件，再报 READY**。
    //   反过来的话，systemd 认为服务已就绪的那一刻 `systemctl reload` 就已经可用了，
    //   而它要读的文件可能还不存在——一个只在「起来的瞬间就 reload」时才出现的竞态。
    if let Err(e) = write_pid_file(&pid_file) {
        error!("写 pid 文件 {pid_file} 失败：{e} —— systemctl reload 将无法找到本代进程");
    } else if !pid_file.is_empty() {
        info!("pid 文件已更新：{pid_file} → {pid}");
    }

    // ★ ★ **`NOTIFY_SOCKET` 不在时 `notify()` 返回的是 `Ok(())`，不是错误**
    //   （sd-notify 0.5.0 源码：`let Some(..) = env::var_os(NOTIFY_SOCKET) else
    //   { return Ok(()) };`）。所以「调用成功」并不等于「消息发出去了」——照着返回值
    //   打日志，会在什么都没发生的时候打印一句很有依据感的「已发出」。
    //   ⇒ 自己去看这个变量，把两件事分开报告。
    let has_socket = std::env::var_os("NOTIFY_SOCKET").is_some();

    // ★ ★ 绝不能用 `notify_and_unset_env()`。它发完会把 `NOTIFY_SOCKET` 从本进程环境里
    //   删掉，而**下一代是本进程 fork 出来的、靠继承拿到这个变量**——删了它，
    //   下一代永远发不出 READY，而当代一切正常。这种失效只在下一次换代时才显形。
    //
    // ★ 不发 `MAINPID=`（G37 否掉了交接）：交接过去的 pid 不是 systemd 亲生的，
    //   会被标成 alien，此后 `systemctl stop` 不再等排空。让 unit 活过换代的是
    //   unit 文件里的 `ExitType=cgroup`，不是交接。
    let result = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

    match (result, has_socket) {
        (Ok(()), true) => info!("sd_notify 已发出：READY=1（pid={pid}）"),
        // 不在 systemd 下跑（手工起进程、端到端测试）时就是这一支。
        // 不该让进程退出，但必须说实话。
        (Ok(()), false) => {
            info!("没有 NOTIFY_SOCKET，**没有**发 READY=1（不在 systemd 下跑就是这样，pid={pid}）")
        }
        (Err(e), _) => error!("sd_notify 失败：{e}（本该发的是 READY=1，pid={pid}）"),
    }
}

/// 装 `SIGUSR2` 处理：拉起下一代，然后让 pingora 自己的 SIGQUIT 路径去送 fd。
///
/// ⚠ ⚠ **不接这个信号比「reload 什么都不做」更糟** —— `SIGUSR2` 的默认动作是终止进程，
/// 于是 `ExecReload=kill -USR2 …` 一旦找到当前这一代就会**把服务打死**。
///
/// ★ 为什么多一个信号而不是直接用 SIGQUIT：SIGQUIT 被 pingora 占着，收到就立刻
/// 「送 fd + 排空」—— 可必须先有一个正在监听 upgrade sock 的下一代。而从 CLI 起的进程
/// 不在 unit 的 cgroup 里 ⇒ 拉起动作必须由本进程做（fork 出的子进程天然继承 cgroup）。
///
/// ⚠ **必须在 `run_forever()` 之前调用**，理由见函数体里那段注释。
pub fn spawn_upgrade_trigger() {
    let spawned = thread::Builder::new()
        .name("fulcrum-upgrade".into())
        .spawn(|| {
            // ★ 自带一个 current_thread runtime，不借 pingora 的：pingora 的 runtime 在
            //   `run()` 里才建，而这个线程要在 `run()` 之前就位——否则启动后到 `run()`
            //   之间的那一小段时间里 reload 会丢。
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("换代触发器建不出 runtime，systemctl reload 将永远无效：{e}");
                    return;
                }
            };
            rt.block_on(async {
                let mut sigusr2 = match tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::user_defined2(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("装不上 SIGUSR2 处理器，systemctl reload 将永远无效：{e}");
                        return;
                    }
                };
                while sigusr2.recv().await.is_some() {
                    info!("收到 SIGUSR2：开始换代，先拉起下一代");
                    match spawn_next_generation() {
                        Ok(child_pid) => {
                            info!("下一代已拉起 pid={child_pid}，现在给自己发 SIGQUIT 送 fd");
                            // SAFETY: kill(2) 只做「向一个 pid 发信号」，无内存语义；
                            // 目标是本进程自己，pid 必然有效。
                            unsafe {
                                libc::kill(std::process::id() as libc::pid_t, libc::SIGQUIT);
                            }
                        }
                        // ★ ★ spawn 失败**绝不能**再发 SIGQUIT。
                        //   SIGQUIT 会让本代把 fd 送出去（送给谁都没有）然后开始排空并退出，
                        //   于是「换代失败」会变成「服务没了」。宁可 reload 无事发生。
                        Err(e) => error!("拉起下一代失败，本次换代放弃，本代继续服务：{e}"),
                    }
                }
            });
        });
    if let Err(e) = spawned {
        error!("起不来换代触发器线程（{e}）—— systemctl reload 将永远无效");
    }
}

/// 用**和自己完全一样的命令行**再起一个进程，只多一个 `-u`。
///
/// ★ 下一代会**重新读一遍配置文件**——这正是 `systemctl reload` 该有的语义
/// （改完 `Fulcrumfile` 后 reload 一次即生效，含监听端口集变了的情形；
/// 而 `POST /load` 那条路换不了端口，会 409 指向这里）。
fn spawn_next_generation() -> std::io::Result<u32> {
    let exe = next_generation_program()?;
    // ★ 先把已有的 `-u` 滤掉：gen2 再换代到 gen3 时，自己的 argv 里本来就有一个。
    //   （只认独立的长短形态；unit 文件不用 `-uc` 这种粘连写法。）
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "-u" && a != "--upgrade")
        .collect();
    let child = Command::new(&exe).args(&args).arg("-u").spawn()?;
    Ok(child.id())
}

/// 下一代该 exec 哪个文件。
///
/// ⚠ ⚠ **这不是 `current_exe()` 一句话就完的事。** 换二进制要 rename 到原路径
/// （直接写正在跑的可执行文件会 `ETXTBSY`），而 rename **换的是 inode**
/// ⇒ `/proc/self/exe` 立刻变成 `…/fulcrum (deleted)`，`current_exe()` 原样返回它，
/// 拿去 exec 必然 `ENOENT`。
/// ⚠ 而 `systemctl reload` 返回的是**成功** —— 运维看到一次成功的升级，
/// 跑着的还是旧二进制（服务没断，但升级没发生）。
///
/// ⇒ 回落到 `argv[0]`（nginx 走的也是这条）：systemd 的 `ExecStart` 是绝对路径，
/// 于是它指向磁盘上现在那一份。★ 顺序不能反 —— `current_exe()` 是内核给的答案，
/// 只有它指向的文件已经不在时才轮到 `argv[0]`。
fn next_generation_program() -> std::io::Result<std::path::PathBuf> {
    if let Ok(p) = std::env::current_exe()
        && p.exists()
    {
        return Ok(p);
    }
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if argv0.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "current_exe() 指向的文件已不存在（多半是二进制被换过），而 argv[0] 是空的",
        ));
    }
    let p = std::path::PathBuf::from(&argv0);
    info!(
        "本进程的可执行文件已被替换，下一代改从 argv[0] 起：{}",
        p.display()
    );
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServeOptions;

    fn cfg_with_grace(ms: Option<u64>) -> fulcrum_config::StructuredConfig {
        use fulcrum_config::model::{Defaults, Global, StructuredConfig};
        StructuredConfig {
            schema_version: fulcrum_config::model::SCHEMA_VERSION,
            global: Global {
                grace_period_ms: ms,
                ..Default::default()
            },
            defaults: Defaults::default(),
            sites: Vec::new(),
            l4: None,
        }
    }

    // ── 线程模型（G35 结案 · G140 落地）────────────────────────────────────
    //
    // ★ 全部喂**合成的** `cpus`，⛔ 不去问这台机器有几个核：一条读宿主状态的判据
    //   在同一台机器上只能被观察到一种结果，而两个方向都要能验。

    fn cfg_with_threads(
        l7: Option<usize>,
        l4: Option<usize>,
        admin: Option<usize>,
    ) -> fulcrum_config::StructuredConfig {
        use fulcrum_config::model::{Defaults, Global, StructuredConfig};
        StructuredConfig {
            schema_version: fulcrum_config::model::SCHEMA_VERSION,
            global: Global {
                threads_l7: l7,
                threads_l4: l4,
                threads_admin: admin,
                ..Default::default()
            },
            defaults: Defaults::default(),
            sites: Vec::new(),
            l4: None,
        }
    }

    /// 不写配置时，三个角色各拿到**自己的**缺省（G35 的初值）。
    ///
    /// ★ ★ 三个角色的期望值**互不相同**是判据本身：一个把三个角色接串的实现
    /// （比如三条臂都读 `threads_l7`）在三个期望值相同时照样全绿。
    #[test]
    fn 不写配置时三个角色各拿各的缺省() {
        let g = &cfg_with_threads(None, None, None).global;
        assert_eq!(threads_for(ThreadRole::L7, g, 12), 12, "L7 缺省该是核数");
        assert_eq!(
            threads_for(ThreadRole::L4, g, 12),
            DEFAULT_L4_THREADS,
            "L4 缺省该是 {DEFAULT_L4_THREADS}，⛔ 不是核数"
        );
        assert_eq!(
            threads_for(ThreadRole::AdminAndBackground, g, 12),
            DEFAULT_ADMIN_THREADS,
            "管理面与后台缺省该是 {DEFAULT_ADMIN_THREADS}"
        );
    }

    /// 写了配置就按配置走，且**三格不串**。
    #[test]
    fn 配置里的线程数各覆盖各的角色() {
        let g = &cfg_with_threads(Some(16), Some(3), Some(2)).global;
        assert_eq!(threads_for(ThreadRole::L7, g, 12), 16);
        assert_eq!(threads_for(ThreadRole::L4, g, 12), 3);
        assert_eq!(threads_for(ThreadRole::AdminAndBackground, g, 12), 2);
    }

    /// ⚠ 只写其中一格时，**另外两格仍然走缺省** —— 覆盖是逐格的，不是全有或全无。
    #[test]
    fn 只写一格时另外两格照旧走缺省() {
        let g = &cfg_with_threads(Some(16), None, None).global;
        assert_eq!(threads_for(ThreadRole::L7, g, 12), 16);
        assert_eq!(threads_for(ThreadRole::L4, g, 12), DEFAULT_L4_THREADS);
        assert_eq!(
            threads_for(ThreadRole::AdminAndBackground, g, 12),
            DEFAULT_ADMIN_THREADS
        );
    }

    /// L7 的缺省被夹在**配置面的合法值域**里。
    ///
    /// ★ ★ 判据的理由：`cpus` 是探测来的，而配置那条路只允许 `[MIN, MAX]`。
    /// 不夹的话，一台 2048 核的机器上会得到一个**配置面根本写不出来**的值 ——
    /// 而那种「两条路的值域不一样」正是本仓反复抓到的那一族。
    #[test]
    fn l7的缺省被夹在配置面的值域里() {
        use fulcrum_config::model::{MAX_THREADS, MIN_THREADS};
        let g = &cfg_with_threads(None, None, None).global;
        assert_eq!(
            threads_for(ThreadRole::L7, g, 100_000),
            MAX_THREADS,
            "上界没夹"
        );
        // ⚠ `detected_cpus()` 不会返回 0，但判据不该依赖那个前提。
        assert_eq!(threads_for(ThreadRole::L7, g, 0), MIN_THREADS, "下界没夹");
    }

    /// ★ ★ ★ **全局 `ServerConf.threads` 必须是「管理面与后台」那一格，⛔ 不是核数。**
    ///
    /// 这一条是整个线程模型里最容易写错、且错了**完全没有症状**的一格：
    /// pingora 的线程**不跨 service 共享**，`conf.threads` 是所有
    /// `Service::threads() == None` 的 service 的回落值（后台任务全是这一类）。
    /// ⇒ 把核数填在这里，等于让每一个后台任务都起一整套核数大小的 runtime，
    /// 而服务照常工作、日志里一个字都不会提。
    #[test]
    fn 全局threads是管理面那一格而不是核数() {
        let conf = build_server_conf(
            &cfg_with_threads(None, None, None),
            &ServeOptions::default(),
            64,
        );
        assert_eq!(
            conf.threads, DEFAULT_ADMIN_THREADS,
            "全局 threads 该是管理面/后台那一格（{DEFAULT_ADMIN_THREADS}），\
             ⛔ 不是探测到的核数（64）"
        );

        // 反方向：写了 `threads_admin` 时它要真的落到全局那一格上
        //（后台任务是靠它生效的，没有第二条路）。
        let conf = build_server_conf(
            &cfg_with_threads(Some(16), Some(3), Some(4)),
            &ServeOptions::default(),
            64,
        );
        assert_eq!(
            conf.threads, 4,
            "threads_admin 没落到 ServerConf.threads 上"
        );
    }

    /// ★ ★ 这一条是本批的核心判据之一：**没写 `grace_period` 时排空窗口不许是 `None`。**
    ///
    /// `None` 会让 pingora 用它的 `EXIT_TIMEOUT = 300`，而那正是实测里
    /// `systemctl stop` 被 SIGKILL 的根因。反证：把 `Some(...)` 改回 `None`，这条红。
    #[test]
    fn 没配_grace_period_时用产品自己的默认值而不是上游的_300_秒() {
        let conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert_eq!(
            conf.grace_period_seconds,
            Some(DEFAULT_GRACE_PERIOD_SECS),
            "没配 grace_period 时必须给一个我们自己的默认值；留 None 会落到 pingora 的 300 秒"
        );
        // ⚠ 「这个默认值得比 pingora 的 300 小」那条断言写在
        //   `尺子自证_两段都没给值时读出的是_pingora_的_305_秒` 里，
        //   因为在这里写会是一次**常量**比较（clippy::assertions-on-constants 直接拒绝，
        //   而它拒绝得对：两个 const 之间的 assert 在编译期就有答案，运行它没有信息量）。
    }

    /// 配了就必须听配置的——否则上一条那个默认值会变成一个改不掉的硬编码。
    #[test]
    fn 配了_grace_period_就按配置来() {
        let conf = build_server_conf(&cfg_with_grace(Some(7_000)), &ServeOptions::default(), 8);
        assert_eq!(conf.grace_period_seconds, Some(7));
    }

    /// ★ 最后那一段也必须是我们自己给的值。
    ///
    /// 它与上游默认值相同，所以**这条测试不是在测行为，是在测「这个数有没有来源」**——
    /// 留 `None` 的话「停机要花多久」就有一半藏在上游源码里，
    /// 而运维要按这个和去写 `TimeoutStopSec`。
    #[test]
    fn 优雅停机的最后一段也要显式给值() {
        let conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert_eq!(
            conf.graceful_shutdown_timeout_seconds,
            Some(GRACEFUL_SHUTDOWN_TIMEOUT_SECS)
        );
    }

    /// 停机预算 = 两段之和，且它必须能从**配置**里被推上去。
    ///
    /// ⚠ 判据取两个方向：默认配置下是那个和；配了大的 `grace_period` 之后跟着变大。
    /// 只测前者的话，一个把和写死成常量的实现也全绿——而运维会照着那个假数字
    /// 去设 `TimeoutStopSec`。
    #[test]
    fn 停机预算是两段之和且跟着配置走() {
        let default_conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert_eq!(
            shutdown_budget_secs(&default_conf),
            DEFAULT_GRACE_PERIOD_SECS + GRACEFUL_SHUTDOWN_TIMEOUT_SECS
        );

        let long = build_server_conf(&cfg_with_grace(Some(120_000)), &ServeOptions::default(), 8);
        assert_eq!(
            shutdown_budget_secs(&long),
            120 + GRACEFUL_SHUTDOWN_TIMEOUT_SECS
        );
    }

    /// ★ ★ ★ **这把尺子必须量得出「谁都没给值」那一种。**
    ///
    /// 它是这一批的自证：`shutdown_budget_secs` 的回落值取的是 **pingora 的**
    /// （300 / 5），不是我们的（30 / 5）。取我们自己的话，`Some(30)` 与 `None`
    /// 会读出同一个 35 —— 于是启动日志里那行数字、以及门禁里那条断言，
    /// 对本批要防的缺陷**完全是瞎的**。
    ///
    /// ⚠ 这条测试有意**绕开 `build_server_conf`**，直接造一个 `None` 的 `ServerConf`：
    /// 它量的是尺子，不是被测物。
    #[test]
    fn 尺子自证_两段都没给值时读出的是_pingora_的_305_秒() {
        let bare = ServerConf {
            grace_period_seconds: None,
            graceful_shutdown_timeout_seconds: None,
            ..Default::default()
        };
        assert_eq!(
            shutdown_budget_secs(&bare),
            305,
            "回落值必须是 pingora 真的会等的那个（EXIT_TIMEOUT=300 + 5），\
             否则「忘了赋值」与「赋了我们的默认值」读数相同"
        );

        // 而产品造出来的那一份必须**明显更小**——两个方向合起来才说明这把尺子在工作。
        let ours = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert!(shutdown_budget_secs(&ours) < shutdown_budget_secs(&bare));
    }

    /// ★ ★ `daemon` 恒 `false` 是整条进程模型的前提（G31/G33）。
    ///
    /// `daemonize()` 会 fork，而「`run()` 之前创建的任何线程都会丢失」——
    /// 换代触发器正住在这样一个线程里。一旦它变成 true，
    /// **`systemctl reload` 会安静地什么都不做**，而进程照常服务流量。
    /// 这种失效没有任何症状，所以它必须是一道门而不是一句注释。
    #[test]
    fn daemon_恒为_false() {
        let conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert!(
            !conf.daemon,
            "daemon=true 会 fork 掉换代触发器所在的线程，systemctl reload 将安静地失效"
        );
    }

    /// pid 文件与升级 socket 的路径必须原样落到 `ServerConf` 上。
    ///
    /// ⚠ 它们是 `ExecReload` 与 `-u` 两条路的地址，抄错一个字符的症状是
    /// 「reload 没反应」——与「没实现」长得一模一样。
    #[test]
    fn pid_文件与升级_socket_的路径原样传下去() {
        let opts = ServeOptions {
            pid_file: "/run/x/y.pid".to_string(),
            upgrade_sock: "/run/x/up.sock".to_string(),
            ..Default::default()
        };
        let conf = build_server_conf(&cfg_with_grace(None), &opts, 8);
        assert_eq!(conf.pid_file, "/run/x/y.pid");
        assert_eq!(conf.upgrade_sock, "/run/x/up.sock");
    }

    /// pid 文件写出来的内容必须是**本进程**的 pid，而且是原子替换。
    ///
    /// ★ 这条同时守着「写的是自己而不是别人」——`ExecReload` 拿这个数去 `kill -USR2`，
    /// 写错的话那一刀会砍在一个无关进程上。
    #[test]
    fn pid_文件写的是本进程且能原子覆盖() {
        let dir = std::env::temp_dir().join(format!("fulcrum-pid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fulcrum.pid");
        let p = path.to_str().unwrap();

        // 先放一份旧内容，验证覆盖（rename）走得通。
        std::fs::write(&path, "99999\n").unwrap();
        write_pid_file(p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );

        // ⚠ 临时文件不许留下：它与 pid 文件同目录，而那是 systemd 的 RuntimeDirectory。
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "rename 之后不该留下临时文件：{leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 路径为空时什么都不做，而且**不报错**。
    ///
    /// ⚠ 空路径是「不要 pid 文件」的表达（比如手工前台跑）。
    /// 当成错误会在日志里刷一条吓人的假警报。
    #[test]
    fn 空路径不写也不报错() {
        write_pid_file("").expect("空路径应当是一次无操作");
    }
}
