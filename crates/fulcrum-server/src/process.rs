//! 處理程序託管：讓產品二進位真的能被 systemd 管起來（G78）。
//!
//! 少了其中任何一件，`systemctl start` 就會 `Result=timeout`（而資料面本身好好地在聽）：
//!
//! | # | 什麼 | 不補的後果 |
//! |---|---|---|
//! | ① | `sd_notify(READY=1)` | `Type=notify` 永遠等不到就緒 ⇒ `systemctl start` 超時失敗 |
//! | ② | 寫 pid 檔案 | `ExecReload` 找不到當前這一代 ⇒ **零停機換代做不了** |
//! | ③ | `SIGUSR2` 換代觸發器 | ⚠ 比「什麼都不做」更糟：它的預設動作是**終止處理程序** |
//! | ④ | 停機時長的兩個值 | pingora 預設 grace 是 **300 秒**，`TimeoutStopSec=60` 會在排空到一半時 SIGKILL |
//!
//! > ★ ★ **一個 spike 證明的是「這條路走得通」，不是「產品走在這條路上」。**
//! > M1 的 systemd 場景一度跑的是 spike 二進位，而 spike 把 ①②③ 自己實作了一遍。
//! > **夾具餵給門的那個二進位，本身也是夾具的一部分。**
//!
//! # 逐條繼承 spike 趟過的坑
//!
//! - `NOTIFY_SOCKET` 不在時 `sd_notify::notify()` 回傳 `Ok(())` 而**不是**錯誤；
//! - **絕不能**用 `notify_and_unset_env()`（下一代靠繼承拿這個變數）；
//! - pid 檔案必須 write + rename（`ExecReload` 會 `cat` 它，可能撞上寫到一半）；
//! - 必須在服務**真的起來之後**才發 READY、才寫 pid 檔案；
//! - `spawn` 失敗**絕不能**再給自己發 SIGQUIT（那會把「升級失敗」變成「服務沒了」）。
//!
//! ⚠ **不要照抄 MAINPID 交接**：G37 已否掉它（交接過去的 pid 不是 systemd 親生的，
//! 會被標成 alien，此後停機不再等排空），`tests/m1/mainpid-handover.sh` 釘著它。

use std::process::Command;
use std::thread;

use log::{error, info, warn};
use pingora_core::server::ExecutionPhase;
use pingora_core::server::configuration::ServerConf;
use tokio::sync::broadcast::error::RecvError;

/// 排空視窗的預設值（秒）。
///
/// ⚠ **不是隨手填的**：不給的話 pingora 用自己的 `EXIT_TIMEOUT = 300 秒`，
/// 於是按 `TimeoutStopSec=60` 寫的 unit 會在排空進行到 1/5 時被 SIGKILL，
/// `systemctl stop` 以 `failed (signal)` 收場，**連接被硬斷**。
/// 30 與 `process-model.md` 那句「35–65 秒量級」的下端一致（30 + 5）。
/// 要改用別的值，在 DSL 裡寫 `grace_period`。
pub const DEFAULT_GRACE_PERIOD_SECS: u64 = 30;

// ── 執行緒模型（G35 結案 · G140 完成實作）────────────────────────────────────────
//
// ★ ★ ★ **最要緊的一條機制事實：執行緒不跨 service 共享。** pingora `ServerConf` 的註解
//   原話是 "The threads are not shared across services" —— 每個 service 各起一套
//   runtime ⇒ **總執行緒數 ≈ Σ(各 service 的 threads)**。樞衡註定有 4+ 個 service，
//   ⇒ **把一個籠統的全域 `threads` 設成核數會直接超訂**（4 核 × 4 service = 16 個 worker）。
//   這正是 DSL 面按角色分三格、⛔ 而沒有一個全域 `threads` 的全部理由。
//   推導見 `docs/architecture/process-model.md` 的「執行緒模型」一節。
//
// ⚠ ★ **初值不是結論。** `process-model.md` 明寫判據在 M3 的對拍資料裡，
//   而那些數今天還不存在（合格宿主不存在，G132）。⛔ 別把下面三個常數讀成
//   「量過之後定下來的」—— 它們是 G35 拍的**起步值**。

/// L4 面（TCP/UDP 透傳）每個 service 的預設執行緒數（**G35**）。
///
/// ★ 取 2 而不是核數：L4 那條路徑是**位元組搬運**，沒有 HTTP 解析、沒有路由匹配，
/// 單位工作量小得多；而它每多一個執行緒就是一整套 runtime（見上面那條機制事實）。
pub const DEFAULT_L4_THREADS: usize = 2;

/// 管理面與後台任務的預設執行緒數（**G35**）。
///
/// ★ ★ 它同時是 pingora `ServerConf.threads` 的取值 ⇒ **一切沒有被逐 service 覆蓋的
/// 東西都跟著它**（後台服務就是這一類：`background_service()` 不設 `threads`，
/// `Service::threads()` 回傳 `None` 並回落到全域值）。⇒ 它是「其餘一切」那一格。
pub const DEFAULT_ADMIN_THREADS: usize = 1;

/// service 的角色（**G35 / G140**）。
///
/// ⚠ 只有三個，⛔ **h3 不單列**：它就是資料面入口，與 h1/h2 服務同一批流量、
/// 走同一條執行鏈（`tests/h3/run.sh` 裡那條「h3 與 h1 跑同一條執行鏈」的判據），
/// ⇒ 歸 [`ThreadRole::L7`]（G140 ③，owner 拍板）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    /// L7 資料面：h1/h2 監聽器、**h3 監聽器**。
    L7,
    /// L4 面：TCP 透傳、UDP 透傳。
    L4,
    /// 管理面 socket，以及一切沒被逐 service 覆蓋的東西（後台任務）。
    AdminAndBackground,
}

/// 角色預設。**這是那些數字唯一的一處**。
///
/// ⛔ 不許在 `Global::default()` 或別處再寫一份：預設值住兩處，就會有一天兩處分家
/// 而彼此都自洽 —— 本儲存庫 2026-09-05 當天剛栽過一次同形狀的（G135 的分解式抄了三處）。
///
/// ★ `cpus` 由呼叫方傳進來（[`detected_cpus`]），**不在這裡去問系統**：
/// 判據要能在兩個方向上被喂合成輸入，而「這臺機器有幾個核」是判不了兩次的。
pub fn default_threads(role: ThreadRole, cpus: usize) -> usize {
    match role {
        // ⚠ 夾在值域裡：`cpus` 是探測來的，而設定那條路的合法值域是
        //   `[MIN_THREADS, MAX_THREADS]` —— 兩條路給出的數不該落在不同的值域裡。
        //   （一臺 2048 核的機器上，不夾就會得到一個設定面**寫不出來**的值。）
        ThreadRole::L7 => cpus.clamp(
            fulcrum_config::model::MIN_THREADS,
            fulcrum_config::model::MAX_THREADS,
        ),
        ThreadRole::L4 => DEFAULT_L4_THREADS,
        ThreadRole::AdminAndBackground => DEFAULT_ADMIN_THREADS,
    }
}

/// 探測可用並行度。
///
/// ★ 用 `std::thread::available_parallelism()`，⛔ 不引第三方 crate：它是 std，
/// 且在 Linux 上**認 cgroup 配額** —— 容器裡給了 2 核就報 2，而不是宿主機的核數。
///
/// ⚠ 探測失敗時回落到 1 **並打一行 warn**：這一格沒有「寧可多」的安全側 ——
/// 超訂會讓所有 service 一起變慢，而回落到 1 至少與今天的行為相同。
/// ⛔ 但不許靜默：一個悄悄退成 1 的資料面，與「設定生效了」在啟動紀錄裡長得一樣。
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

/// 某個角色**這一趟真正用幾個執行緒**：設定裡寫了就用設定的，沒寫就用角色預設。
pub fn threads_for(role: ThreadRole, g: &fulcrum_config::model::Global, cpus: usize) -> usize {
    let configured = match role {
        ThreadRole::L7 => g.threads_l7,
        ThreadRole::L4 => g.threads_l4,
        ThreadRole::AdminAndBackground => g.threads_admin,
    };
    configured.unwrap_or_else(|| default_threads(role, cpus))
}

/// 排空之後、等各 runtime 退出的最後一段（秒）。
///
/// ★ 這個數**與 pingora 的預設值相同**（5），所以顯式接上它不改變任何行為。
/// 那為什麼還要接：**停機預算這件事必須有一個能被讀到的來源。**
/// 不接的話，「停機要花多久」= 我們的 grace + 一個藏在上游原始碼裡的常數，
/// 而維運要按這個和去設 `TimeoutStopSec`。現在兩個加數都在這個檔案裡，
/// [`shutdown_budget_secs`] 負責把和算出來，`serve()` 啟動時把它打進紀錄。
pub const GRACEFUL_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

/// pingora 在 `grace_period_seconds` 是 `None` 時會用的值（`EXIT_TIMEOUT = 60 * 5`）。
///
/// ★ ★ 它出現在這裡**只為一件事**：讓 [`shutdown_budget_secs`] 報的是
/// **pingora 真的會等多久**，而不是「我們希望它等多久」。
/// ⚠ 回落到 [`DEFAULT_GRACE_PERIOD_SECS`] 是錯的 —— 那樣一來，
/// 無論 `grace_period_seconds` 是 `Some(30)` 還是 `None`，這把尺子都報 35，
/// 於是啟動紀錄裡那行數字**對本批要防的那個缺陷完全是瞎的**。
/// 一把兩種情況下讀數相同的尺子，量不出這兩種情況的差別。
const PINGORA_DEFAULT_GRACE_SECS: u64 = 300;
/// pingora 在 `graceful_shutdown_timeout_seconds` 是 `None` 時會用的值。
///
/// ⚠ 它與 [`GRACEFUL_SHUTDOWN_TIMEOUT_SECS`] **數值相同純屬巧合**，含義相反：
/// 那個是「我們要它等多久」，這個是「不告訴它的話它等多久」。
const PINGORA_DEFAULT_GRACEFUL_SECS: u64 = 5;

/// 造 Pingora 的 [`ServerConf`]。
///
/// ★ ★ **抽成一個純函式，是為了讓判據掛在產品函式的產物上。**
/// 這條教訓是批 7（TLS-ALPN-01）那天付學費買的：當時那個單測
/// 自己用 `rcgen` 拼了一張一模一樣的證書去檢查擴充，**測的是 rcgen 的行為，
/// 不是我們的函式**。`serve()` 回傳 `!`、裡面還要綁連接埠，單測碰不了它；
/// 而「停機預算」「daemon 恆 false」這些恰恰是最需要門的東西。
pub fn build_server_conf(
    cfg: &fulcrum_config::StructuredConfig,
    opts: &crate::ServeOptions,
    cpus: usize,
) -> ServerConf {
    ServerConf {
        // ★ ★ 全域 `threads` = **管理面與後台**那個角色（G35 / G140）。
        //   pingora 拿它當**回落值**：`Service::threads()` 回傳 `None` 的 service
        //   （後台任務全是這一類）都跟著它 ⇒ 它是「其餘一切」那一格，
        //   ⛔ 不是「所有 service 的執行緒數」。L7 與 L4 各自逐 service 覆蓋它。
        // ⚠ ⚠ 這裡**絕不能**填核數：執行緒不跨 service 共享 ⇒ 那會讓每一個
        //   沒被覆蓋的 service 都起一整套核數大小的 runtime。
        threads: threads_for(ThreadRole::AdminAndBackground, &cfg.global, cpus),
        pid_file: opts.pid_file.clone(),
        upgrade_sock: opts.upgrade_sock.clone(),
        // 全域選項裡的 `grace_period` 映到 Pingora 的排空視窗；沒寫就用我們自己的預設值。
        // ⚠ **不能留 `None`**——那是上面 DEFAULT_GRACE_PERIOD_SECS 文件裡說的那個陷阱。
        grace_period_seconds: Some(
            cfg.global
                .grace_period_ms
                .map(|ms| ms / 1000)
                .unwrap_or(DEFAULT_GRACE_PERIOD_SECS),
        ),
        graceful_shutdown_timeout_seconds: Some(GRACEFUL_SHUTDOWN_TIMEOUT_SECS),
        // ★ `daemon` 恆 false 是整條處理程序模型的前提（G31/G33），不是偏好：
        //   `daemonize()` 會 fork，而 pingora 自己的註解寫著「`run()` 之前建立的
        //   任何執行緒都會丟失」——下面那個 SIGUSR2 觸發器正是在 `run()` 之前建的。
        //   一旦它變成 true，**換代觸發器會安靜地消失**：處理程序照常服務流量，
        //   只是 `systemctl reload` 從此什麼都不做。產品不從 YAML 讀 `ServerConf`
        //   （`Opt::default()` 的 `conf` 是 `None`），所以這裡是它唯一的來源，
        //   由下面的單測釘住。
        ..Default::default()
    }
}

/// 一次優雅停機最多花多久（秒）——`TimeoutStopSec` 必須大於它。
///
/// ★ ★ **回落值取的是 pingora 的，不是我們的。** 這把尺子要能量出
/// 「忘了給 `grace_period_seconds` 賦值」這件事——那正是本批要防的缺陷，
/// 而它的現場表現就是這個數從 35 跳到 305。回落到我們自己的預設值會讓兩種情況讀數相同。
///
/// ⚠ 這不是全部：**換代**時老一代還要多等 `CLOSE_TIMEOUT`（5 秒，pingora 寫死在
/// `server/mod.rs`）。那一段發生在 `systemctl reload` 裡，不在 `systemctl stop` 的
/// 計時視窗內，所以不算進這個和。
pub fn shutdown_budget_secs(conf: &ServerConf) -> u64 {
    conf.grace_period_seconds
        .unwrap_or(PINGORA_DEFAULT_GRACE_SECS)
        .saturating_add(
            conf.graceful_shutdown_timeout_seconds
                .unwrap_or(PINGORA_DEFAULT_GRACEFUL_SECS),
        )
}

/// 把「當前這一代是誰」原子地寫進 pid 檔案。
///
/// ★ ★ 為什麼必須是 write + rename 而不是直接寫：`ExecReload` 會去 `cat` 這個檔案，
/// 而它隨時可能與新一代的寫撞上。直接寫會讓 reload 讀到半截內容（甚至空檔案），
/// 於是 `kill -USR2 ""` 靜默失敗——**一次什麼都沒做的 reload**，沒有任何症狀。
/// rename 在同一檔案系統上是原子的，讀者要麼看到舊的一代，要麼看到新的一代。
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

/// 起一個執行緒：等服務真的起來，寫 pid 檔案，然後告訴 systemd 就緒。
///
/// ⚠ **必須在 `run_forever()` 之前呼叫**——它會 move 掉 `server`，
/// 而 `phase_rx` 只能從 `server` 上訂閱。
pub fn spawn_readiness(
    phase_rx: tokio::sync::broadcast::Receiver<ExecutionPhase>,
    pid_file: String,
) {
    if let Err(e) = thread::Builder::new()
        .name("fulcrum-sd-notify".into())
        .spawn(move || notify_when_running(phase_rx, pid_file))
    {
        // ⚠ 起不來就必須喊：`Type=notify` 下這等於「這個處理程序永遠不會就緒」。
        error!("起不来就绪通知线程（{e}）—— systemd 将等不到 READY=1，systemctl start 会超时失败");
    }
}

/// 等 [`ExecutionPhase::Running`]，再寫 pid 檔案、再發 `READY=1`。
///
/// ★ 訂閱的是相位廣播，**不是 sleep 一會兒**：pingora 在所有 service 都啟動完之後
/// 才發 `Running`。對換代而言這正是要的那一刻——新一代已經把 fd 取走並開始 accept 了。
fn notify_when_running(
    mut phase_rx: tokio::sync::broadcast::Receiver<ExecutionPhase>,
    pid_file: String,
) {
    loop {
        match phase_rx.blocking_recv() {
            Ok(ExecutionPhase::Running) => break,
            // ★ `Lagged` 不能當成結束：廣播緩衝區滿時會丟掉中間的相位，但 `Running`
            //   可能就在被丟掉的那批之後——繼續收即可。當成錯誤退出會讓 unit 永遠等不到 READY。
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

    // ★ 順序是有意的：**先落 pid 檔案，再報 READY**。
    //   反過來的話，systemd 認為服務已就緒的那一刻 `systemctl reload` 就已經可用了，
    //   而它要讀的檔案可能還不存在——一個只在「起來的瞬間就 reload」時才出現的競態。
    if let Err(e) = write_pid_file(&pid_file) {
        error!("写 pid 文件 {pid_file} 失败：{e} —— systemctl reload 将无法找到本代进程");
    } else if !pid_file.is_empty() {
        info!("pid 文件已更新：{pid_file} → {pid}");
    }

    // ★ ★ **`NOTIFY_SOCKET` 不在時 `notify()` 回傳的是 `Ok(())`，不是錯誤**
    //   （sd-notify 0.5.0 原始碼：`let Some(..) = env::var_os(NOTIFY_SOCKET) else
    //   { return Ok(()) };`）。所以「呼叫成功」並不等於「訊息發出去了」——照著回傳值
    //   打紀錄，會在什麼都沒發生的時候列印一句很有依據感的「已發出」。
    //   ⇒ 自己去看這個變數，把兩件事分開報告。
    let has_socket = std::env::var_os("NOTIFY_SOCKET").is_some();

    // ★ ★ 絕不能用 `notify_and_unset_env()`。它發完會把 `NOTIFY_SOCKET` 從本處理程序環境裡
    //   刪掉，而**下一代是本處理程序 fork 出來的、靠繼承拿到這個變數**——刪了它，
    //   下一代永遠發不出 READY，而當代一切正常。這種失效只在下一次換代時才顯形。
    //
    // ★ 不發 `MAINPID=`（G37 否掉了交接）：交接過去的 pid 不是 systemd 親生的，
    //   會被標成 alien，此後 `systemctl stop` 不再等排空。讓 unit 活過換代的是
    //   unit 檔案裡的 `ExitType=cgroup`，不是交接。
    let result = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

    match (result, has_socket) {
        (Ok(()), true) => info!("sd_notify 已发出：READY=1（pid={pid}）"),
        // 不在 systemd 下跑（手動起處理程序、端到端測試）時就是這一支。
        // 不該讓處理程序退出，但必須說實話。
        (Ok(()), false) => {
            info!("没有 NOTIFY_SOCKET，**没有**发 READY=1（不在 systemd 下跑就是这样，pid={pid}）")
        }
        (Err(e), _) => error!("sd_notify 失败：{e}（本该发的是 READY=1，pid={pid}）"),
    }
}

/// 裝 `SIGUSR2` 處理：拉起下一代，然後讓 pingora 自己的 SIGQUIT 路徑去送 fd。
///
/// ⚠ ⚠ **不接這個信號比「reload 什麼都不做」更糟** —— `SIGUSR2` 的預設動作是終止處理程序，
/// 於是 `ExecReload=kill -USR2 …` 一旦找到當前這一代就會**把服務打死**。
///
/// ★ 為什麼多一個信號而不是直接用 SIGQUIT：SIGQUIT 被 pingora 佔著，收到就立刻
/// 「送 fd + 排空」—— 可必須先有一個正在監聽 upgrade sock 的下一代。而從 CLI 起的處理程序
/// 不在 unit 的 cgroup 裡 ⇒ 拉起動作必須由本處理程序做（fork 出的子處理程序天然繼承 cgroup）。
///
/// ⚠ **必須在 `run_forever()` 之前呼叫**，理由見函式體裡那段註解。
pub fn spawn_upgrade_trigger() {
    let spawned = thread::Builder::new()
        .name("fulcrum-upgrade".into())
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
                            // SAFETY: kill(2) 只做「向一個 pid 發信號」，無記憶體語義；
                            // 目標是本處理程序自己，pid 必然有效。
                            unsafe {
                                libc::kill(std::process::id() as libc::pid_t, libc::SIGQUIT);
                            }
                        }
                        // ★ ★ spawn 失敗**絕不能**再發 SIGQUIT。
                        //   SIGQUIT 會讓本代把 fd 送出去（送給誰都沒有）然後開始排空並退出，
                        //   於是「換代失敗」會變成「服務沒了」。寧可 reload 無事發生。
                        Err(e) => error!("拉起下一代失败，本次换代放弃，本代继续服务：{e}"),
                    }
                }
            });
        });
    if let Err(e) = spawned {
        error!("起不来换代触发器线程（{e}）—— systemctl reload 将永远无效");
    }
}

/// 用**和自己完全一樣的命令行**再起一個處理程序，只多一個 `-u`。
///
/// ★ 下一代會**重新讀一遍設定檔案**——這正是 `systemctl reload` 該有的語義
/// （改完 `Fulcrumfile` 後 reload 一次即生效，含監聽連接埠集變了的情形；
/// 而 `POST /load` 那條路換不了連接埠，會 409 指向這裡）。
fn spawn_next_generation() -> std::io::Result<u32> {
    let exe = next_generation_program()?;
    // ★ 先把已有的 `-u` 濾掉：gen2 再換代到 gen3 時，自己的 argv 裡本來就有一個。
    //   （只認獨立的長短形態；unit 檔案不用 `-uc` 這種粘連寫法。）
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "-u" && a != "--upgrade")
        .collect();
    let child = Command::new(&exe).args(&args).arg("-u").spawn()?;
    Ok(child.id())
}

/// 下一代該 exec 哪個檔案。
///
/// ⚠ ⚠ **這不是 `current_exe()` 一句話就完的事。** 換二進位要 rename 到原路徑
/// （直接寫正在跑的執行檔會 `ETXTBSY`），而 rename **換的是 inode**
/// ⇒ `/proc/self/exe` 立刻變成 `…/fulcrum (deleted)`，`current_exe()` 原樣回傳它，
/// 拿去 exec 必然 `ENOENT`。
/// ⚠ 而 `systemctl reload` 回傳的是**成功** —— 維運看到一次成功的升級，
/// 跑著的還是舊二進位（服務沒斷，但升級沒發生）。
///
/// ⇒ 回落到 `argv[0]`（nginx 走的也是這條）：systemd 的 `ExecStart` 是絕對路徑，
/// 於是它指向磁碟上現在那一份。★ 順序不能反 —— `current_exe()` 是核心給的答案，
/// 只有它指向的檔案已經不在時才輪到 `argv[0]`。
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

    // ── 執行緒模型（G35 結案 · G140 完成實作）────────────────────────────────────
    //
    // ★ 全部喂**合成的** `cpus`，⛔ 不去問這臺機器有幾個核：一條讀宿主狀態的判據
    //   在同一臺機器上只能被觀察到一種結果，而兩個方向都要能驗。

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

    /// 不寫設定時，三個角色各拿到**自己的**預設（G35 的初值）。
    ///
    /// ★ ★ 三個角色的期望值**互不相同**是判據本身：一個把三個角色接串的實作
    /// （比如三條臂都讀 `threads_l7`）在三個期望值相同時照樣全綠。
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

    /// 寫了設定就按設定走，且**三格不串**。
    #[test]
    fn 配置里的线程数各覆盖各的角色() {
        let g = &cfg_with_threads(Some(16), Some(3), Some(2)).global;
        assert_eq!(threads_for(ThreadRole::L7, g, 12), 16);
        assert_eq!(threads_for(ThreadRole::L4, g, 12), 3);
        assert_eq!(threads_for(ThreadRole::AdminAndBackground, g, 12), 2);
    }

    /// ⚠ 只寫其中一格時，**另外兩格仍然走預設** —— 覆蓋是逐格的，不是全有或全無。
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

    /// L7 的預設被夾在**設定面的合法值域**裡。
    ///
    /// ★ ★ 判據的理由：`cpus` 是探測來的，而設定那條路只允許 `[MIN, MAX]`。
    /// 不夾的話，一臺 2048 核的機器上會得到一個**設定面根本寫不出來**的值 ——
    /// 而那種「兩條路的值域不一樣」正是本儲存庫反覆抓到的那一族。
    #[test]
    fn l7的缺省被夹在配置面的值域里() {
        use fulcrum_config::model::{MAX_THREADS, MIN_THREADS};
        let g = &cfg_with_threads(None, None, None).global;
        assert_eq!(
            threads_for(ThreadRole::L7, g, 100_000),
            MAX_THREADS,
            "上界没夹"
        );
        // ⚠ `detected_cpus()` 不會回傳 0，但判據不該相依那個前提。
        assert_eq!(threads_for(ThreadRole::L7, g, 0), MIN_THREADS, "下界没夹");
    }

    /// ★ ★ ★ **全域 `ServerConf.threads` 必須是「管理面與後台」那一格，⛔ 不是核數。**
    ///
    /// 這一條是整個執行緒模型裡最容易寫錯、且錯了**完全沒有症狀**的一格：
    /// pingora 的執行緒**不跨 service 共享**，`conf.threads` 是所有
    /// `Service::threads() == None` 的 service 的回落值（後台任務全是這一類）。
    /// ⇒ 把核數填在這裡，等於讓每一個後台任務都起一整套核數大小的 runtime，
    /// 而服務照常工作、紀錄裡一個字都不會提。
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

        // 反方向：寫了 `threads_admin` 時它要真的落到全域那一格上
        //（後台任務是靠它生效的，沒有第二條路）。
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

    /// ★ ★ 這一條是本批的核心判據之一：**沒寫 `grace_period` 時排空視窗不許是 `None`。**
    ///
    /// `None` 會讓 pingora 用它的 `EXIT_TIMEOUT = 300`，而那正是實測裡
    /// `systemctl stop` 被 SIGKILL 的根因。反證：把 `Some(...)` 改回 `None`，這條紅。
    #[test]
    fn 没配_grace_period_时用产品自己的默认值而不是上游的_300_秒() {
        let conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert_eq!(
            conf.grace_period_seconds,
            Some(DEFAULT_GRACE_PERIOD_SECS),
            "没配 grace_period 时必须给一个我们自己的默认值；留 None 会落到 pingora 的 300 秒"
        );
        // ⚠ 「這個預設值得比 pingora 的 300 小」那條斷言寫在
        //   `尺子自证_两段都没给值时读出的是_pingora_的_305_秒` 裡，
        //   因為在這裡寫會是一次**常數**比較（clippy::assertions-on-constants 直接拒絕，
        //   而它拒絕得對：兩個 const 之間的 assert 在編譯期就有答案，執行它沒有資訊量）。
    }

    /// 配了就必須聽設定的——否則上一條那個預設值會變成一個改不掉的硬編碼。
    #[test]
    fn 配了_grace_period_就按配置来() {
        let conf = build_server_conf(&cfg_with_grace(Some(7_000)), &ServeOptions::default(), 8);
        assert_eq!(conf.grace_period_seconds, Some(7));
    }

    /// ★ 最後那一段也必須是我們自己給的值。
    ///
    /// 它與上游預設值相同，所以**這條測試不是在測行為，是在測「這個數有沒有來源」**——
    /// 留 `None` 的話「停機要花多久」就有一半藏在上游原始碼裡，
    /// 而維運要按這個和去寫 `TimeoutStopSec`。
    #[test]
    fn 优雅停机的最后一段也要显式给值() {
        let conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert_eq!(
            conf.graceful_shutdown_timeout_seconds,
            Some(GRACEFUL_SHUTDOWN_TIMEOUT_SECS)
        );
    }

    /// 停機預算 = 兩段之和，且它必須能從**設定**裡被推上去。
    ///
    /// ⚠ 判據取兩個方向：預設設定下是那個和；配了大的 `grace_period` 之後跟著變大。
    /// 只測前者的話，一個把和寫死成常數的實作也全綠——而維運會照著那個假數字
    /// 去設 `TimeoutStopSec`。
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

    /// ★ ★ ★ **這把尺子必須量得出「誰都沒給值」那一種。**
    ///
    /// 它是這一批的自證：`shutdown_budget_secs` 的回落值取的是 **pingora 的**
    /// （300 / 5），不是我們的（30 / 5）。取我們自己的話，`Some(30)` 與 `None`
    /// 會讀出同一個 35 —— 於是啟動紀錄裡那行數字、以及門禁裡那條斷言，
    /// 對本批要防的缺陷**完全是瞎的**。
    ///
    /// ⚠ 這條測試有意**繞開 `build_server_conf`**，直接造一個 `None` 的 `ServerConf`：
    /// 它量的是尺子，不是被測物。
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

        // 而產品造出來的那一份必須**明顯更小**——兩個方向合起來才說明這把尺子在工作。
        let ours = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert!(shutdown_budget_secs(&ours) < shutdown_budget_secs(&bare));
    }

    /// ★ ★ `daemon` 恆 `false` 是整條處理程序模型的前提（G31/G33）。
    ///
    /// `daemonize()` 會 fork，而「`run()` 之前建立的任何執行緒都會丟失」——
    /// 換代觸發器正住在這樣一個執行緒裡。一旦它變成 true，
    /// **`systemctl reload` 會安靜地什麼都不做**，而處理程序照常服務流量。
    /// 這種失效沒有任何症狀，所以它必須是一道門而不是一句註解。
    #[test]
    fn daemon_恒为_false() {
        let conf = build_server_conf(&cfg_with_grace(None), &ServeOptions::default(), 8);
        assert!(
            !conf.daemon,
            "daemon=true 会 fork 掉换代触发器所在的线程，systemctl reload 将安静地失效"
        );
    }

    /// pid 檔案與升級 socket 的路徑必須原樣落到 `ServerConf` 上。
    ///
    /// ⚠ 它們是 `ExecReload` 與 `-u` 兩條路的位址，抄錯一個字元的症狀是
    /// 「reload 沒反應」——與「沒實作」長得一模一樣。
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

    /// pid 檔案寫出來的內容必須是**本處理程序**的 pid，而且是原子替換。
    ///
    /// ★ 這條同時守著「寫的是自己而不是別人」——`ExecReload` 拿這個數去 `kill -USR2`，
    /// 寫錯的話那一刀會砍在一個無關處理程序上。
    #[test]
    fn pid_文件写的是本进程且能原子覆盖() {
        let dir = std::env::temp_dir().join(format!("fulcrum-pid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fulcrum.pid");
        let p = path.to_str().unwrap();

        // 先放一份舊內容，驗證覆蓋（rename）走得通。
        std::fs::write(&path, "99999\n").unwrap();
        write_pid_file(p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );

        // ⚠ 暫存檔不許留下：它與 pid 檔案同目錄，而那是 systemd 的 RuntimeDirectory。
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

    /// 路徑為空時什麼都不做，而且**不回報錯誤**。
    ///
    /// ⚠ 空路徑是「不要 pid 檔案」的表達（比如手動前台跑）。
    /// 當成錯誤會在紀錄裡刷一條嚇人的假警報。
    #[test]
    fn 空路径不写也不报错() {
        write_pid_file("").expect("空路径应当是一次无操作");
    }
}
