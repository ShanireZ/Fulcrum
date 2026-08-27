//! M1 第一个 spike（G31 强制）：**systemd `Type=notify` 前台运行下的 MainPID 交接**。
//!
//! # 它要回答的问题
//!
//! `PLAN.md` G31 把进程模型定成 systemd `Type=notify` + 前台运行，但同时记下了一条
//! **纯靠读文档与代码推出来、没有跑过**的冲突：
//!
//! > pingora 的零停机升级是**外部拉起新进程**（`-u`），而 `Type=notify` 下老进程一退出，
//! > unit 即被判定结束、默认 `KillMode=control-group` 会杀掉整个 cgroup。要让升级活下来，
//! > 新进程必须**落在同一个 cgroup 内**并抢过 MainPID。
//!
//! ★ **推断不是证据**——M0 那次正是靠 spike 额外捞出了 UDP 分流那条风险。所以 G31 拍板时
//! 附了一条强制条件：这件事必须先做 spike，判据照 M0 的形式写死。本 crate 就是那个 spike。
//!
//! # 形状
//!
//! 照 nginx `USR2` 那一套，**由老进程自己拉起下一代**——这是「新进程落在同一个 cgroup 内」
//! 唯一不需要额外机制的做法（fork 出来的子进程天然继承 cgroup）。
//!
//! ```text
//!   systemctl reload
//!        │  ExecReload=/bin/kill -USR2 $MAINPID
//!        ▼
//!   gen1（MainPID）── SIGUSR2 ──▶ fork+exec  gen2 = 自己 + `-u`
//!        │                                      │
//!        │  ★ 只有 spawn 成功了才给自己发 SIGQUIT   │ 绑 upgrade sock、等 fd
//!        ▼                                      │
//!   pingora 的 SIGQUIT 路径：把监听 fd 经         │
//!   SCM_RIGHTS 送过去，然后开始排空 ─────────────▶│
//!                                                ▼
//!                                       服务起来 → ExecutionPhase::Running
//!                                                ▼
//!                                       sd_notify(MAINPID=self, READY=1)
//!                                                ▼
//!                                       systemd 把 MainPID 换成 gen2
//!   gen1 排空完退出 ──▶ systemd 看到的只是「一个普通子进程退了」，unit 照常 active
//! ```
//!
//! # 三个可切换的口径（判据靠它们才能反证）
//!
//! | 环境变量 | 作用 |
//! |---|---|
//! | `M1_MAINPID=off` | **不发 `MAINPID=`**。★ 反证用：这时 unit 必须在 gen1 退出时死掉，并把 gen2 一起带走。 |
//! | `M1_BIND_HOST` | 监听地址，默认回环（同 M0，理由见 `m0-seam`）|
//! | `NotifyAccess=` | 不在代码里，在 unit 文件里。`all` 与 `main` 的差别由 `tests/m1/notify-access.sh` 单独量 |

use std::io::Write;
use std::process::Command;
use std::thread;

use m0_seam::{fd_inspect, http_app, raw_tcp, raw_udp};
use pingora_core::server::ExecutionPhase;
use pingora_core::server::Server;
use pingora_core::server::configuration::Opt;
use pingora_core::services::listening::Service as ListeningService;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;

/// 监听地址。默认回环，理由与 `m0-seam` 同（UDP 回声是可被伪造源地址的反射面）。
fn bind_host() -> String {
    std::env::var("M1_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 是否发 `MAINPID=`。
///
/// ★ ★ **默认关**，而 G31 拍板时假定的是「必须抢 MainPID」。这个默认值是本 spike 实测之后
///   反过来改的：交接确实能让 unit 活过老代退出，**但它会同时毁掉优雅停机**——
///   systemd 把非亲生的 MainPID 标成 alien 之后，`systemctl stop` 不再等排空，
///   SIGTERM 与 SIGKILL 几乎同时发出，unit 以 `failed (signal)` 收场。
///   真正解决「unit 活过老代退出」的是 `ExitType=cgroup`，它不需要交接。
///   完整对照表见 `docs/verification/m1-systemd.md`。
///
/// 留着这个开关是为了**把被否掉的那条路钉住**（`tests/m1/mainpid-handover.sh`），
/// 免得将来有人照着 G31 的原文又实现一遍。
fn mainpid_handover_enabled() -> bool {
    std::env::var("M1_MAINPID").as_deref() == Ok("claim")
}

fn main() {
    // 日志格式与 m0-seam 一致：每行带 pid。
    // ★ 理由在 M1 这里比在 M0 更硬：升级窗口内**两代进程往同一个 journal 流里写**
    //   （子进程继承了 systemd 给的 stderr），没有 pid 就没有任何办法把某一行钉到某一代身上。
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|buf, record| {
            writeln!(
                buf,
                "[{} {:<5} pid={} {}] {}",
                buf.timestamp(),
                record.level(),
                std::process::id(),
                record.target(),
                record.args()
            )
        })
        .init();

    let opt = Opt::parse_args();
    let upgrading = opt.upgrade;
    let mut server = Server::new(Some(opt)).expect("failed to create server");
    server.bootstrap();

    // ★ ★ 前台运行是整条设计的**前提**，不是偏好，所以它必须是一道门而不是一句注释。
    //
    //   `daemonize()` 会 fork，而 pingora 自己的文档注释写着「`run()` 之前创建的任何线程
    //   都会丢失」——下面那个 SIGUSR2 线程正是在 `run()` 之前创建的。一旦有人把
    //   `daemon: true` 写回配置，**升级触发器会安静地消失**：进程照常启动、照常服务流量，
    //   只是 `systemctl reload` 从此什么都不做。这种失效没有任何症状，正是本仓库反复吃亏的形状。
    if server.configuration.daemon {
        eprintln!(
            "m1-systemd: 配置里 daemon=true，而本 spike 的全部前提是前台运行（G31）。\n\
             daemonize() 会 fork 掉 SIGUSR2 升级触发器所在的线程，升级将安静地失效。拒绝启动。"
        );
        std::process::exit(1);
    }

    let host = bind_host();
    let http_bind = format!("{host}:8080");
    let raw_tcp_bind = format!("{host}:8081");
    let raw_udp_bind = format!("{host}:8082");

    let mut http = ListeningService::new("m1-http".to_string(), http_app::MinimalHttp);
    http.add_tcp(&http_bind);
    server.add_service(http);
    server.add_service(raw_tcp::TcpEchoService::new(&raw_tcp_bind, upgrading));
    server.add_service(raw_udp::UdpEchoService::new(&raw_udp_bind, upgrading));
    server.add_service(fd_inspect::FdInspectService);

    // 就绪信号必须在 `run_forever()`（它会 move 掉 server）之前订阅。
    // ★ 订阅的是 `ExecutionPhase::Running`，**不是 sleep 一会儿**：pingora 在所有 service
    //   都启动完之后才发这个相位（`server/mod.rs` 的 `main_loop` 开头）。对升级而言这正是
    //   要的那一刻——新一代已经把 fd 取走并开始 accept 了，此时交接 MainPID 才是诚实的。
    let phase_rx = server.watch_execution_phase();
    // ★ pid 文件的路径来自 pingora 的 `pid_file` 配置项，但**写它的人是我们自己**。
    //   前台模式下 pingora 根本不碰这个字段（只有 daemonize() 读），于是它会安静地失效——
    //   配置里写了一个路径，文件永远不出现。产品要么自己兑现这个字段，要么拒绝它；
    //   这里选择兑现，因为 `systemctl reload` 正需要一个稳定的「当前这一代在哪」。
    let pid_file = server.configuration.pid_file.clone();
    thread::Builder::new()
        .name("sd-notify".into())
        .spawn(move || notify_when_running(phase_rx, pid_file))
        .expect("failed to spawn sd-notify thread");

    spawn_upgrade_trigger();

    log::info!(
        "m1-systemd up: http={http_bind} raw-tcp={raw_tcp_bind} raw-udp={raw_udp_bind} \
         upgrading={upgrading} mainpid_handover={} pid={}",
        mainpid_handover_enabled(),
        std::process::id()
    );

    server.run_forever();
}

/// 把「当前这一代是谁」原子地写进 pid 文件。
///
/// ★ ★ 为什么必须是 write + rename 而不是直接写：`ExecReload` 会去 `cat` 这个文件，
///   而它随时可能与新一代的写撞上。直接写会让 reload 读到半截内容（甚至空文件），
///   于是 `kill -USR2 ""` 静默失败——**一次什么都没做的 reload**，没有任何症状。
///   rename 在同一文件系统上是原子的，读者要么看到旧的一代，要么看到新的一代。
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

/// 等服务真的起来，再告诉 systemd。
fn notify_when_running(
    mut phase_rx: tokio::sync::broadcast::Receiver<ExecutionPhase>,
    pid_file: String,
) {
    loop {
        match phase_rx.blocking_recv() {
            Ok(ExecutionPhase::Running) => break,
            // ★ `Lagged` 不能当成结束：广播缓冲区满时会丢掉中间的相位，但 `Running`
            //   可能就在被丢掉的那批里之后——继续收即可。当成错误退出会让 unit 永远等不到 READY。
            Err(RecvError::Lagged(n)) => {
                log::warn!("execution phase 广播落后 {n} 条，继续等 Running");
            }
            Err(RecvError::Closed) => {
                log::error!("execution phase 广播已关闭，永远等不到 Running，不发 READY");
                return;
            }
            Ok(_) => {}
        }
    }

    let pid = std::process::id();

    // ★ 顺序是有意的：**先落 pid 文件，再报 READY**。
    //   反过来的话，systemd 认为服务已就绪的那一刻，`systemctl reload` 就已经可用了，
    //   而它要读的文件可能还不存在 —— 一个只在「起来的瞬间就 reload」时才出现的竞态。
    if let Err(e) = write_pid_file(&pid_file) {
        // 写不了就必须喊出来：reload 整条路都建在这个文件上。
        log::error!("写 pid 文件 {pid_file} 失败：{e} —— systemctl reload 将无法找到本代进程");
    } else if !pid_file.is_empty() {
        log::info!("pid 文件已更新：{pid_file} → {pid}");
    }

    let mut state = Vec::new();
    if mainpid_handover_enabled() {
        state.push(sd_notify::NotifyState::MainPid(pid));
    }
    state.push(sd_notify::NotifyState::Ready);

    // ★ ★ **`NOTIFY_SOCKET` 不在时 `notify()` 返回的是 `Ok(())`，不是错误。**
    //   （实测 sd-notify 0.5.0 源码：`let Some(..) = env::var_os(NOTIFY_SOCKET) else
    //   { return Ok(()) };`）所以「调用成功」并不等于「消息发出去了」——照着返回值打日志，
    //   会在什么都没发生的时候打印一句很有依据感的「已发出」。
    //   这正是本仓库反复抓到的那个形状，所以这里**自己去看这个变量**，分开报告两件事。
    let has_socket = std::env::var_os("NOTIFY_SOCKET").is_some();

    // ★ ★ 绝不能用 `notify_and_unset_env()`。它发完会把 `NOTIFY_SOCKET` 从本进程环境里删掉，
    //   而**下一代是本进程 fork 出来的，靠继承拿到这个变量**——删了它，下一代永远无法交接
    //   MainPID，而当代一切正常。这种失效只在下一次升级时才显形，是最难查的那一类。
    //   （它在 0.5.0 里是 `unsafe fn`，理由是 `remove_var`；但对枢衡来说更硬的理由是上面这条。）
    let result = sd_notify::notify(&state);

    let what = if mainpid_handover_enabled() {
        format!("READY=1 MAINPID={pid}")
    } else {
        "READY=1（★ 未发 MAINPID：M1_MAINPID=off，反证模式）".to_string()
    };
    match (result, has_socket) {
        (Ok(()), true) => log::info!("sd_notify 已发出：{what}"),
        // 不在 systemd 下跑（手工起进程）时就是这一支。不该让进程退出，但必须说实话。
        (Ok(()), false) => log::warn!(
            "sd_notify **什么都没发**：环境里没有 NOTIFY_SOCKET（不在 systemd 下跑就是这样）。\
             本该发的是：{what}"
        ),
        (Err(e), _) => log::error!("sd_notify 失败：{e}（本该发的是：{what}）"),
    }
}

/// 装 SIGUSR2 处理：拉起下一代，然后让 pingora 自己的 SIGQUIT 路径去送 fd。
///
/// ★ 为什么要多一个信号，而不是直接用 SIGQUIT：SIGQUIT 被 pingora 占着，收到就立刻开始
///   「送 fd + 排空」。可送给谁？必须先有一个正在监听 upgrade sock 的下一代。
///   M0 的脚本是**外部**先 SIGQUIT 再从命令行起第二代——那条路在 systemd 下不成立，
///   因为从 CLI 起的进程不在 unit 的 cgroup 里。所以顺序必须反过来，且拉起动作必须由本进程做。
fn spawn_upgrade_trigger() {
    thread::Builder::new()
        .name("upgrade-trigger".into())
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
                    log::error!("升级触发器建不出 runtime，reload 将永远无效：{e}");
                    return;
                }
            };
            rt.block_on(async {
                let mut sigusr2 = match signal(SignalKind::user_defined2()) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("装不上 SIGUSR2 处理器，reload 将永远无效：{e}");
                        return;
                    }
                };
                while sigusr2.recv().await.is_some() {
                    log::info!("SIGUSR2：开始升级，先拉起下一代");
                    match spawn_next_generation() {
                        Ok(child_pid) => {
                            log::info!("下一代已拉起 pid={child_pid}，现在给自己发 SIGQUIT 送 fd");
                            // SAFETY: kill(2) 只做「向一个 pid 发信号」，无内存语义；
                            // 目标是本进程自己，pid 必然有效。
                            unsafe {
                                libc::kill(std::process::id() as libc::pid_t, libc::SIGQUIT);
                            }
                        }
                        // ★ ★ spawn 失败**绝不能**再发 SIGQUIT。
                        //   SIGQUIT 会让本代把 fd 送出去（送给谁都没有）然后开始排空并退出，
                        //   于是「升级失败」会变成「服务没了」。宁可 reload 无事发生。
                        Err(e) => log::error!("拉起下一代失败，本次升级放弃，本代继续服务：{e}"),
                    }
                }
            });
        })
        .expect("failed to spawn upgrade-trigger thread");
}

/// 用**和自己完全一样的命令行**再起一个进程，只多一个 `-u`。
fn spawn_next_generation() -> std::io::Result<u32> {
    let exe = std::env::current_exe()?;
    // ★ 先把已有的 `-u` 滤掉：gen2 再升级到 gen3 时，自己的 argv 里本来就有一个。
    //   （只认独立的长短形态；本 spike 的 unit 文件不用 `-uc` 这种粘连写法。）
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "-u" && a != "--upgrade")
        .collect();
    let child = Command::new(&exe).args(&args).arg("-u").spawn()?;
    Ok(child.id())
}
