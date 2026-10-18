//! M1 第一個 spike（G31 強制）：**systemd `Type=notify` 前台執行下的 MainPID 交接**。
//!
//! # 它要回答的問題
//!
//! `PLAN.md` G31 把處理程序模型定成 systemd `Type=notify` + 前台執行，但同時記下了一條
//! **純靠讀文件與程式碼推出來、沒有跑過**的衝突：
//!
//! > pingora 的零停機升級是**外部拉起新處理程序**（`-u`），而 `Type=notify` 下老處理程序一退出，
//! > unit 即被判定結束、預設 `KillMode=control-group` 會殺掉整個 cgroup。要讓升級活下來，
//! > 新處理程序必須**落在同一個 cgroup 內**並搶過 MainPID。
//!
//! ★ **推斷不是證據**——M0 那次正是靠 spike 額外撈出了 UDP 分流那條風險。所以 G31 拍板時
//! 附了一條強制條件：這件事必須先做 spike，判據照 M0 的形式寫死。本 crate 就是那個 spike。
//!
//! # 形狀
//!
//! 照 nginx `USR2` 那一套，**由老處理程序自己拉起下一代**——這是「新處理程序落在同一個 cgroup 內」
//! 唯一不需要額外機制的做法（fork 出來的子處理程序天然繼承 cgroup）。
//!
//! ```text
//!   systemctl reload
//!        │  ExecReload=/bin/kill -USR2 $MAINPID
//!        ▼
//!   gen1（MainPID）── SIGUSR2 ──▶ fork+exec  gen2 = 自己 + `-u`
//!        │                                      │
//!        │  ★ 只有 spawn 成功了才給自己發 SIGQUIT   │ 綁 upgrade sock、等 fd
//!        ▼                                      │
//!   pingora 的 SIGQUIT 路徑：把監聽 fd 經         │
//!   SCM_RIGHTS 送過去，然後開始排空 ─────────────▶│
//!                                                ▼
//!                                       服務起來 → ExecutionPhase::Running
//!                                                ▼
//!                                       sd_notify(MAINPID=self, READY=1)
//!                                                ▼
//!                                       systemd 把 MainPID 換成 gen2
//!   gen1 排空完退出 ──▶ systemd 看到的只是「一個普通子進程退了」，unit 照常 active
//! ```
//!
//! # 三個可切換的口徑（判據靠它們才能反證）
//!
//! | 環境變數 | 作用 |
//! |---|---|
//! | `M1_MAINPID=off` | **不發 `MAINPID=`**。★ 反證用：這時 unit 必須在 gen1 退出時死掉，並把 gen2 一起帶走。 |
//! | `M1_BIND_HOST` | 監聽位址，預設迴環（同 M0，理由見 `m0-seam`）|
//! | `NotifyAccess=` | 不在程式碼裡，在 unit 檔案裡。`all` 與 `main` 的差別由 `tests/m1/notify-access.sh` 單獨量 |

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

/// 監聽位址。預設迴環，理由與 `m0-seam` 同（UDP 回聲是可被偽造源位址的反射面）。
fn bind_host() -> String {
    std::env::var("M1_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 是否發 `MAINPID=`。
///
/// ★ ★ **預設關**，而 G31 拍板時假定的是「必須搶 MainPID」。這個預設值是本 spike 實測之後
///   反過來改的：交接確實能讓 unit 活過老代退出，**但它會同時毀掉優雅停機**——
///   systemd 把非親生的 MainPID 標成 alien 之後，`systemctl stop` 不再等排空，
///   SIGTERM 與 SIGKILL 幾乎同時發出，unit 以 `failed (signal)` 收場。
///   真正解決「unit 活過老代退出」的是 `ExitType=cgroup`，它不需要交接。
///   完整對照表見 `docs/verification/m1-systemd.md`。
///
/// 留著這個開關是為了**把被否掉的那條路釘住**（`tests/m1/mainpid-handover.sh`），
/// 免得將來有人照著 G31 的原文又實作一遍。
fn mainpid_handover_enabled() -> bool {
    std::env::var("M1_MAINPID").as_deref() == Ok("claim")
}

fn main() {
    // 紀錄格式與 m0-seam 一致：每行帶 pid。
    // ★ 理由在 M1 這裡比在 M0 更硬：升級視窗內**兩代處理程序往同一個 journal 流裡寫**
    //   （子處理程序繼承了 systemd 給的 stderr），沒有 pid 就沒有任何辦法把某一行釘到某一代身上。
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

    // ★ ★ 前台執行是整條設計的**前提**，不是偏好，所以它必須是一道門而不是一句註解。
    //
    //   `daemonize()` 會 fork，而 pingora 自己的文件註解寫著「`run()` 之前建立的任何執行緒
    //   都會丟失」——下面那個 SIGUSR2 執行緒正是在 `run()` 之前建立的。一旦有人把
    //   `daemon: true` 寫回設定，**升級觸發器會安靜地消失**：處理程序照常啟動、照常服務流量，
    //   只是 `systemctl reload` 從此什麼都不做。這種失效沒有任何症狀，正是本儲存庫反覆吃虧的形狀。
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

    // 就緒信號必須在 `run_forever()`（它會 move 掉 server）之前訂閱。
    // ★ 訂閱的是 `ExecutionPhase::Running`，**不是 sleep 一會兒**：pingora 在所有 service
    //   都啟動完之後才發這個相位（`server/mod.rs` 的 `main_loop` 開頭）。對升級而言這正是
    //   要的那一刻——新一代已經把 fd 取走並開始 accept 了，此時交接 MainPID 才是誠實的。
    let phase_rx = server.watch_execution_phase();
    // ★ pid 檔案的路徑來自 pingora 的 `pid_file` 設定項，但**寫它的人是我們自己**。
    //   前台模式下 pingora 根本不碰這個欄位（只有 daemonize() 讀），於是它會安靜地失效——
    //   設定裡寫了一個路徑，檔案永遠不出現。產品要麼自己兌現這個欄位，要麼拒絕它；
    //   這裡選擇兌現，因為 `systemctl reload` 正需要一個穩定的「當前這一代在哪」。
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

/// 把「當前這一代是誰」原子地寫進 pid 檔案。
///
/// ★ ★ 為什麼必須是 write + rename 而不是直接寫：`ExecReload` 會去 `cat` 這個檔案，
///   而它隨時可能與新一代的寫撞上。直接寫會讓 reload 讀到半截內容（甚至空檔案），
///   於是 `kill -USR2 ""` 靜默失敗——**一次什麼都沒做的 reload**，沒有任何症狀。
///   rename 在同一檔案系統上是原子的，讀者要麼看到舊的一代，要麼看到新的一代。
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

/// 等服務真的起來，再告訴 systemd。
fn notify_when_running(
    mut phase_rx: tokio::sync::broadcast::Receiver<ExecutionPhase>,
    pid_file: String,
) {
    loop {
        match phase_rx.blocking_recv() {
            Ok(ExecutionPhase::Running) => break,
            // ★ `Lagged` 不能當成結束：廣播緩衝區滿時會丟掉中間的相位，但 `Running`
            //   可能就在被丟掉的那批裡之後——繼續收即可。當成錯誤退出會讓 unit 永遠等不到 READY。
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

    // ★ 順序是有意的：**先落 pid 檔案，再報 READY**。
    //   反過來的話，systemd 認為服務已就緒的那一刻，`systemctl reload` 就已經可用了，
    //   而它要讀的檔案可能還不存在 —— 一個只在「起來的瞬間就 reload」時才出現的競態。
    if let Err(e) = write_pid_file(&pid_file) {
        // 寫不了就必須喊出來：reload 整條路都建在這個檔案上。
        log::error!("写 pid 文件 {pid_file} 失败：{e} —— systemctl reload 将无法找到本代进程");
    } else if !pid_file.is_empty() {
        log::info!("pid 文件已更新：{pid_file} → {pid}");
    }

    let mut state = Vec::new();
    if mainpid_handover_enabled() {
        state.push(sd_notify::NotifyState::MainPid(pid));
    }
    state.push(sd_notify::NotifyState::Ready);

    // ★ ★ **`NOTIFY_SOCKET` 不在時 `notify()` 回傳的是 `Ok(())`，不是錯誤。**
    //   （實測 sd-notify 0.5.0 原始碼：`let Some(..) = env::var_os(NOTIFY_SOCKET) else
    //   { return Ok(()) };`）所以「呼叫成功」並不等於「訊息發出去了」——照著回傳值打紀錄，
    //   會在什麼都沒發生的時候列印一句很有依據感的「已發出」。
    //   這正是本儲存庫反覆抓到的那個形狀，所以這裡**自己去看這個變數**，分開報告兩件事。
    let has_socket = std::env::var_os("NOTIFY_SOCKET").is_some();

    // ★ ★ 絕不能用 `notify_and_unset_env()`。它發完會把 `NOTIFY_SOCKET` 從本處理程序環境裡刪掉，
    //   而**下一代是本處理程序 fork 出來的，靠繼承拿到這個變數**——刪了它，下一代永遠無法交接
    //   MainPID，而當代一切正常。這種失效只在下一次升級時才顯形，是最難查的那一類。
    //   （它在 0.5.0 裡是 `unsafe fn`，理由是 `remove_var`；但對樞衡來說更硬的理由是上面這條。）
    let result = sd_notify::notify(&state);

    let what = if mainpid_handover_enabled() {
        format!("READY=1 MAINPID={pid}")
    } else {
        "READY=1（★ 未发 MAINPID：M1_MAINPID=off，反证模式）".to_string()
    };
    match (result, has_socket) {
        (Ok(()), true) => log::info!("sd_notify 已发出：{what}"),
        // 不在 systemd 下跑（手動起處理程序）時就是這一支。不該讓處理程序退出，但必須說實話。
        (Ok(()), false) => log::warn!(
            "sd_notify **什么都没发**：环境里没有 NOTIFY_SOCKET（不在 systemd 下跑就是这样）。\
             本该发的是：{what}"
        ),
        (Err(e), _) => log::error!("sd_notify 失败：{e}（本该发的是：{what}）"),
    }
}

/// 裝 SIGUSR2 處理：拉起下一代，然後讓 pingora 自己的 SIGQUIT 路徑去送 fd。
///
/// ★ 為什麼要多一個信號，而不是直接用 SIGQUIT：SIGQUIT 被 pingora 佔著，收到就立刻開始
///   「送 fd + 排空」。可送給誰？必須先有一個正在監聽 upgrade sock 的下一代。
///   M0 的指令碼是**外部**先 SIGQUIT 再從命令行起第二代——那條路在 systemd 下不成立，
///   因為從 CLI 起的處理程序不在 unit 的 cgroup 裡。所以順序必須反過來，且拉起動作必須由本處理程序做。
fn spawn_upgrade_trigger() {
    thread::Builder::new()
        .name("upgrade-trigger".into())
        .spawn(|| {
            // ★ 自帶一個 current_thread runtime，不借 pingora 的：pingora 的 runtime 在
            //   `run()` 裡才建，而這個執行緒要在 `run()` 之前就位——否則啟動後到 `run()`
            //   之間的那一小段時間裡 reload 會丟。
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
                            // SAFETY: kill(2) 只做「向一個 pid 發信號」，無記憶體語義；
                            // 目標是本處理程序自己，pid 必然有效。
                            unsafe {
                                libc::kill(std::process::id() as libc::pid_t, libc::SIGQUIT);
                            }
                        }
                        // ★ ★ spawn 失敗**絕不能**再發 SIGQUIT。
                        //   SIGQUIT 會讓本代把 fd 送出去（送給誰都沒有）然後開始排空並退出，
                        //   於是「升級失敗」會變成「服務沒了」。寧可 reload 無事發生。
                        Err(e) => log::error!("拉起下一代失败，本次升级放弃，本代继续服务：{e}"),
                    }
                }
            });
        })
        .expect("failed to spawn upgrade-trigger thread");
}

/// 用**和自己完全一樣的命令行**再起一個處理程序，只多一個 `-u`。
fn spawn_next_generation() -> std::io::Result<u32> {
    let exe = std::env::current_exe()?;
    // ★ 先把已有的 `-u` 濾掉：gen2 再升級到 gen3 時，自己的 argv 裡本來就有一個。
    //   （只認獨立的長短形態；本 spike 的 unit 檔案不用 `-uc` 這種粘連寫法。）
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "-u" && a != "--upgrade")
        .collect();
    let child = Command::new(&exe).args(&args).arg("-u").spawn()?;
    Ok(child.id())
}
