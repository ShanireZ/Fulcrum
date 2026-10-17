//! DNS-01 的「誰去改那條 TXT」（G57）。
//!
//! # ★ G57 的形狀是「不先建體系」
//!
//! > 原生支援按**真實用到的**一家一家加。§6.2 明確不做服務發現整合，
//! > **一個 DNS 供應商動物園是同一個形狀**。
//!
//! 所以這裡是一個**枚舉**而不是一個 `dyn Trait` 的外掛框架：
//! 現在只有 exec hook 一個變體，加 Cloudflare / DNSPod 時就多一個變體。
//! ⚠ 先擺一個 trait 出來，等於先把動物園的籠子蓋好——那正是 G57 拒絕的東西。
//!
//! # ⚠ exec hook 是新的攻擊面，G57 點名要過安全基線
//!
//! | 要求 | 這裡怎麼做的 |
//! |---|---|
//! | 不經 shell 解釋 | `Command::new(程序)` + `.arg()`，**從不拼接命令行字串** |
//! | 參數不拼字串 | 網域與值各自是一個獨立 `arg`，引號/分號/反引號一律只是普通位元組 |
//! | 超時 | `tokio::time::timeout`，到點**殺掉**（`kill_on_drop`）|
//! | 輸出上限 | stdout/stderr 各自唯讀前 [`MAX_OUTPUT`] 位元組 |
//! | 以執行使用者身分跑 | 不做任何提權，環境原樣繼承（G59：憑證正是從環境或檔案來的）|
//!
//! ★ **不記錄環境變數**（基線第 3 條）：hook 的憑證就在裡面。

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Linux 的 `ETXTBSY`。⚠ 直接寫數字而不引 `libc`：為一個常數新增一個包，
/// 撞的是「新增 0 個包」那條紀律，而這個值是 ABI 的一部分，不會變。
const ETXTBSY: i32 = 26;

/// `ETXTBSY` 的**現場取證**：到底是誰以寫方式開著這個檔案。
///
/// # ★ ★ ★ 它為什麼存在
///
/// 門禁裡有一條**偶發**紅：`起不来 DNS hook …：Text file busy (os error 26)`，
/// 只在並行跑全套單測時出現，單獨復跑全綠。⛔ **owner 明令不許加重試** ——
/// 重試會把這條測試真正守著的那個缺陷一起蓋掉。
/// ⇒ 那就**先建證據**：`ETXTBSY` 在 Linux 上的含義是**有處理程序以寫方式打開著這個檔案**，
/// 而「是誰」`/proc` 答得出來。★ 根因沒定位之前的修復是猜。
///
/// # ✅ ★ ★ ★ 它把根因抓出來了：**測試自己在對方的寫句柄上 fork**
///
/// 現場是取證第一次報出一個**持有者**：`可写=是`，而 `cmd` 恰好是**測試二進位自己**。
/// ⇒ 那不是「本處理程序沒關乾淨」，是一個 `fork` 出來、**還沒 exec** 的子處理程序：
/// 它的 fd 表是父處理程序的一份副本，而 `O_CLOEXEC` 要到 exec 那一刻才生效，
/// `/proc/<子>/cmdline` 在那之前也還是父處理程序的。
///
/// 於是這條偶發紅的完整機制是：
///
/// 1. 某條測試在 `Sandbox::script()` 裡 `File::create(hook.sh)`，那一瞬握著寫句柄；
/// 2. **另一條不拿串行鎖的測試**在同一時刻 `spawn` —— ⚠ `spawn` 失敗也要先 `fork`，
///    所以連「處理程序不存在」那條也算 ⇒ 子處理程序繼承了那個寫句柄；
/// 3. 第 1 條隨後去 `exec hook.sh`，而此刻確實有處理程序以寫方式開著它 ⇒ 核心回 `ETXTBSY`；
/// 4. 取證在失敗**之後**才跑，那時子處理程序已經 exec 完、句柄隨 `O_CLOEXEC` 消失
///    ⇒ 它報「沒找到」—— 而那句話是**真話**，只是晚了一步。
///
/// ⇒ 處置是把本模組**每一條**測試都拉進同一把串行鎖，並加一條**確定性**的結構判據
/// 守著「以後有人忘了拿」。⛔ 不是加重試。
/// 實測（同一棵樹、同一個鏡像，並行跑整套 lib）：**修前 ≈ 5/210 紅，修後 0/300**。
///
/// # ⚠ 它答不了什麼
///
/// 取證發生在**失敗之後**，那一刻持有者**可能已經關掉了**（如果它是一個瞬時的寫句柄，
/// 那正是本條最可能的形態）⇒ **掃不到持有者不等於當時沒有**。
/// ⛔ 因此「沒找到」要說成「沒找到」，不許說成「沒有人持有」。
///
/// ⚠ 只在 Linux 上有 `/proc`；別的平台回空串（本產品只發行 Linux，G13）。
fn etxtbsy_现场(program: &Path, e: &std::io::Error) -> String {
    if e.raw_os_error() != Some(ETXTBSY) {
        return String::new();
    }
    // ⚠ `/proc/<pid>/fd/<n>` 是**解析過**的路徑 ⇒ 比較之前兩邊都要歸一化，
    //   否則 `/tmp/...` 與 `/private/tmp/...` 這類差別會讓取證**永遠掃不到**，
    //   而它給出的答案（「沒找到持有者」）讀起來完全正常。
    let program = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    let program = program.as_path();
    let mut 持有者 = Vec::new();
    let mut 扫过的进程 = 0usize;
    if let Ok(procs) = std::fs::read_dir("/proc") {
        for p in procs.flatten() {
            let 名 = p.file_name();
            let Some(pid) = 名
                .to_str()
                .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            扫过的进程 += 1;
            let fd_dir = format!("/proc/{pid}/fd");
            let Ok(fds) = std::fs::read_dir(&fd_dir) else {
                continue; // 別的使用者的處理程序，讀不了 —— ⚠ 這也是「掃不到」的一種
            };
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                if target != program {
                    continue;
                }
                let fd_名 = fd.file_name().to_string_lossy().into_owned();
                let flags = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd_名}"))
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find_map(|l| l.strip_prefix("flags:"))
                            .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
                    });
                // O_WRONLY = 1、O_RDWR = 2 ⇒ 低兩位非零就是「可寫」。
                let 可写 = flags.map(|f| f & 0o3 != 0);
                let cmd = std::fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|b| {
                        String::from_utf8_lossy(&b)
                            .replace('\0', " ")
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_default();
                持有者.push(format!(
                    "pid={pid} fd={fd_名} 可写={} cmd={cmd:?}",
                    match 可写 {
                        Some(true) => "是",
                        Some(false) => "否",
                        None => "读不出",
                    }
                ));
            }
        }
    }
    if 持有者.is_empty() {
        format!(
            "\n  ★ ETXTBSY 现场：扫了 {扫过的进程} 个进程的 /proc/*/fd，\
             **没找到**还开着这个文件的进程。\
             ⚠ 这不等于当时没有 —— 取证发生在失败之后，瞬时的写句柄可能已经关掉了；\
             读不了的进程（别的用户）也数在「扫过」里而看不见它的 fd。"
        )
    } else {
        format!(
            "\n  ★ ETXTBSY 现场（扫了 {扫过的进程} 个进程）：\n    {}",
            持有者.join("\n    ")
        )
    }
}

/// hook 的 stdout / stderr 各自最多收多少位元組。
///
/// ★ 夠放一條錯誤資訊，又不至於讓一個瘋掉的 hook 把記憶體吃光。
const MAX_OUTPUT: u64 = 8 * 1024;

/// 啟動時檢核（**G59 第 2 條**）的結論。
///
/// ★ ★ **這兩種必須分開，而且分法要掛在類型上，不是在呼叫方 match 錯誤串。**
///
/// | | 意思 | 處置 |
/// |---|---|---|
/// | [`VerifyError::Fatal`] | 對端**回話了**，說這份憑證不行；或者憑證根本讀不出來 | **拒絕啟動**（形狀照 G15）|
/// | [`VerifyError::Inconclusive`] | 壓根沒收到回話（網路不通、超時）| 打一條 **error** 繼續跑 |
///
/// ⚠ ⚠ 為什麼第二種不拒絕啟動：一次網路抖動不該讓整臺機器上**所有**站台都起不來，
/// 包括那些用靜態證書、跟 ACME 毫無關係的。★ **這是一處知情的取捨**——
/// 它與本儲存庫那條「『沒能檢查』當成『檢查通過』是栽過的形狀」是有張力的，
/// 所以處置不是「悄悄跳過」，而是**打 error 說清楚這份憑證沒被驗過**。
#[derive(Debug)]
pub enum VerifyError {
    Fatal(String),
    Inconclusive(String),
}

impl VerifyError {
    pub fn message(&self) -> &str {
        match self {
            VerifyError::Fatal(m) | VerifyError::Inconclusive(m) => m,
        }
    }
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// 誰去改 TXT 記錄。
///
/// ★ 批 8：多了兩家**原生**（G57）。它仍然是一個**枚舉**而不是
/// `dyn Trait` 的外掛框架——G57 的形狀是「不先建體系，按真實用到的一家一家加」。
#[derive(Debug, Clone)]
pub enum DnsProvider {
    /// `tls { dns exec <程序> }` —— 調一個外部處理程序。
    Exec(ExecHook),
    /// `tls { dns cloudflare env:… }` —— `example.net` 在這家。
    Cloudflare(crate::cloudflare::Cloudflare),
    /// `tls { dns dnspod file:… }` —— `example.com` 在這家。
    Dnspod(crate::dnspod::Dnspod),
}

impl DnsProvider {
    /// 供應商的名字，只進紀錄。
    pub fn name(&self) -> &'static str {
        match self {
            DnsProvider::Exec(_) => "exec",
            DnsProvider::Cloudflare(_) => "cloudflare",
            DnsProvider::Dnspod(_) => "dnspod",
        }
    }

    /// **G59 第 2 條**：啟動時能檢核的就檢核。
    ///
    /// ⚠ exec hook 沒有可檢核的東西（它是一個外部處理程序，我們不知道它要什麼憑證），
    /// 所以它這一支是 `Ok(())` —— ★ **不是「跳過檢查」，是「這裡沒有可檢查的東西」**。
    /// 兩者的區別在於：前者應當被記下來，後者不應當。
    pub async fn verify(&self) -> Result<(), VerifyError> {
        match self {
            DnsProvider::Exec(_) => Ok(()),
            DnsProvider::Cloudflare(c) => c.verify().await,
            DnsProvider::Dnspod(d) => d.verify().await,
        }
    }

    /// 掛上一條 TXT。
    pub async fn set_txt(&self, name: &str, value: &str) -> Result<(), String> {
        match self {
            DnsProvider::Exec(h) => h.run("set", name, value).await,
            DnsProvider::Cloudflare(c) => c.set_txt(name, value).await,
            DnsProvider::Dnspod(d) => d.set_txt(name, value).await,
        }
    }

    /// 摘掉一條 TXT。
    ///
    /// ⚠ 失敗**不該讓簽發失敗**——證書已經簽下來了，一條留在那裡的 TXT
    /// 是衛生問題不是可用性問題。呼叫方只記一條 warn。
    pub async fn clear_txt(&self, name: &str, value: &str) -> Result<(), String> {
        match self {
            DnsProvider::Exec(h) => h.run("clear", name, value).await,
            DnsProvider::Cloudflare(c) => c.clear_txt(name, value).await,
            DnsProvider::Dnspod(d) => d.clear_txt(name, value).await,
        }
    }
}

/// 一個外部處理程序。
#[derive(Debug, Clone)]
pub struct ExecHook {
    /// 處理程序路徑。★ 是**路徑**不是命令行——沒有 shell 參與，所以不存在「怎麼分詞」。
    pub program: PathBuf,
    /// 單次呼叫的超時。
    pub timeout: Duration,
}

impl ExecHook {
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> ExecHook {
        ExecHook {
            program: program.into(),
            timeout,
        }
    }

    /// 呼叫約定：`<程序> <set|clear> <记录名> <值>`。
    async fn run(&self, action: &str, name: &str, value: &str) -> Result<(), String> {
        use std::process::Stdio;
        use tokio::process::Command;

        // ★ ★ 三個參數各自是一個 `arg`。**這就是「不拼字串」的全部含義**：
        //   一個叫 `a.com; rm -rf /` 的網域在這裡只是一個普通的 argv[2]，
        //   因為**根本沒有 shell 去解釋它**。
        let mut child = Command::new(&self.program)
            .arg(action)
            .arg(name)
            .arg(value)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // ⚠ 超時之後如果不殺，它會變成一個沒人管的孤兒處理程序，
            //   而且還握著我們的 DNS 憑證。
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                format!(
                    "起不来 DNS hook {}：{e}{}",
                    self.program.display(),
                    etxtbsy_现场(&self.program, &e)
                )
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // ★ ★ ★ **超時必須罩住「讀輸出」，不能只罩住 `wait()`。**
        //
        //   ⚠ 寫成「先把 stdout/stderr 讀到 EOF，再 `timeout(wait())`」是錯的：
        //   一個 `sleep 30` 的 hook **一直握著 stdout**，`read_to_string` 自己就先阻塞了，
        //   超時那一層根本沒機會開火 ⇒ `self.timeout` 變成一句空話，
        //   而它存在的全部理由就是別讓一個卡住的 hook 掛住整條簽發鏈。
        let mut out = String::new();
        let mut err = String::new();
        let collected = tokio::time::timeout(self.timeout, async {
            if let Some(s) = stdout {
                let _ = s.take(MAX_OUTPUT).read_to_string(&mut out).await;
            }
            if let Some(s) = stderr {
                let _ = s.take(MAX_OUTPUT).read_to_string(&mut err).await;
            }
            child.wait().await
        })
        .await;

        let status = match collected {
            Ok(r) => r.map_err(|e| format!("等 DNS hook 结束失败：{e}"))?,
            Err(_) => {
                // `kill_on_drop` 會在 child 完成實作時收屍，這裡顯式再殺一次更直接。
                let _ = child.kill().await;
                return Err(format!(
                    "DNS hook {} {action} 超时（{}s）—— 已杀掉",
                    self.program.display(),
                    self.timeout.as_secs()
                ));
            }
        };
        if status.success() {
            return Ok(());
        }
        // ★ 把 hook 說了什麼帶出來。一個只說「hook 失敗」的錯誤，
        //   等於讓人自己去猜是憑證錯了還是網域寫錯了。
        let tail = |s: &str| s.trim().chars().take(400).collect::<String>();
        Err(format!(
            "DNS hook {} {action} 退出码 {}；stdout: {}；stderr: {}",
            self.program.display(),
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "信号".into()),
            tail(&out),
            tail(&err)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// 給一條測試開一個**它自己的**暫存目錄。
    ///
    /// ⚠ ⚠ **不能按指令碼正文派生檔案名**（比如按位元組長度）：兩條正文恰好等長的測試會共用
    /// 同一個檔案，而每條測試收尾都會把它刪掉 —— **一條測試刪掉了另一條正在用的指令碼**。
    /// ★ 症狀極具迷惑性：**單獨跑每條都綠，一起跑才紅**，紅的那條還隨排程而變
    ///   （`cargo test` 預設多執行緒並行跑同一個二進位裡的測試）。
    /// ⇒ 每條測試一個自己的目錄，共用在結構上就不可能發生。
    struct Sandbox {
        dir: std::path::PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Sandbox {
            let dir =
                std::env::temp_dir().join(format!("fulcrum-hook-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Sandbox { dir }
        }

        /// 在本沙箱裡寫一個可執行指令碼。
        fn script(&self, body: &str) -> std::path::PathBuf {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let p = self.dir.join("hook.sh");
            let mut f = std::fs::File::create(&p).unwrap();
            writeln!(f, "#!/bin/sh\n{body}").unwrap();
            drop(f);
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 讓下面這幾條 `ExecHook` 測試**串行跑**。
    ///
    /// ⚠ ⚠ **這不是把測試靜音**：它們照跑、驗的還是同一件事，
    /// 被消除的只是**測試之間的並行** —— 而那個並行不是被測物件的一部分。
    ///
    /// 起因是一次**只改了 markdown** 的門禁：
    /// `卡住的_hook_会被超时杀掉` 報 `起不来 DNS hook …：Text file busy (os error 26)`。
    /// 單獨復跑三次全綠 ⇒ **只在並行跑全套單測時出現**。
    ///
    /// ★ **已經排除的兩條，別重走**：
    /// 五個 sandbox tag（`args`/`meta`/`fail`/`hang`/`flood`）**互不相同**，
    /// 所以不是兩條測試搶同一個 `hook.sh`（那個缺陷上面那段註解裡記著，早修過了）；
    /// `Sandbox::script` 裡有顯式 `drop(f)` 且 `set_permissions` 在它之後，
    /// 所以不是寫句柄沒關。
    ///
    /// ⚠ ⚠ **根因沒查到底。** 能確定的只有：回報錯誤來自 **exec** 那一步，
    /// 而 Linux 上 `ETXTBSY` 意味著**有處理程序以寫方式打開著這個檔案**。
    /// 剩下的候選是 docker overlayfs 的可見性延遲，或者某個還沒看見的持有者。
    ///
    /// ★ 那為什麼先上串行：它**便宜**，而且**即使根因不是並行，它也不掩蓋任何東西** ——
    /// 每一條斷言都照樣跑。⇒ 這與「加重試」或 `#[ignore]` 是相反性質的處置：
    /// 那兩種會讓這條測試守著的那個真缺陷（超時原先只罩 `wait()`，
    /// 握著 stdout 的 hook 讓 `timeout` 根本沒機會開火）失去守衛。
    static EXEC_HOOK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// ★ ★ **串行化自己的自證**：任一時刻有幾條在臨界區裡。
    ///
    /// ⚠ 少了它，串行化就是一句**沒有判據的斷言** —— 哪天有人刪掉某條測試開頭
    /// 那行 `let _serial = …`，或者把鎖換成別的東西，**一切照舊全綠**，
    /// 而那條偶發紅會在幾周後重新出現、且看不出與這次改動有關。
    /// ★ 這與本儲存庫那條「一個被豁免又不被報告的測試，等於從這道門上消失了」同源。
    static EXEC_HOOK_IN_FLIGHT: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// 持有期間計數 +1，釋放時 -1。
    struct SerialGuard {
        // ⚠ 欄位順序即析構順序：先放計數（後宣告的先析構？不 —— Rust 按宣告順序析構），
        //   所以這裡**只靠 Drop 裡顯式做減法**，不相依欄位順序。
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for SerialGuard {
        fn drop(&mut self) {
            EXEC_HOOK_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 取那把鎖。
    ///
    /// ⚠ **中毒也照常拿**：一條測試 panic 會毒化這把鎖，而預設的 `unwrap()`
    /// 會讓**後面每一條**都 panic 在鎖上 —— 那時螢幕上報出來的失敗，
    /// 與真正壞掉的那條毫無關係。★ 一道會轉述錯原因的門，比沒有這道門更耽誤人。
    fn exec_hook_guard() -> SerialGuard {
        let lock = EXEC_HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // ★ 拿到鎖之後才 +1：那一刻臨界區裡本該只有自己。
        let prev = EXEC_HOOK_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            prev, 0,
            "串行化没生效：拿到锁的这一刻，临界区里已经有 {prev} 条测试在跑"
        );
        SerialGuard { _lock: lock }
    }

    #[test]
    fn 成功的_hook_返回_ok_并且拿得到三个参数() {
        // 把參數原樣寫進檔案，再核對——**判據是「它收到的正是我們給的」**。
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("args");
        let out = sb.path("got");
        let s = sb.script(&format!(
            "printf '%s|%s|%s' \"$1\" \"$2\" \"$3\" > {}",
            out.display()
        ));
        let h = ExecHook::new(&s, Duration::from_secs(10));
        rt().block_on(h.run("set", "_acme-challenge.a.com", "TOKENVALUE"))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "set|_acme-challenge.a.com|TOKENVALUE"
        );
    }

    #[test]
    fn 参数里的_shell_元字符只是普通字节() {
        // ★ ★ 這條是 G57 那句「參數不拼字串」的判據。
        //   如果實作裡有 shell 參與，下面這個網域會把 `touch` 真的跑起來。
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("meta");
        let canary = sb.path("pwned");
        let out = sb.path("got");
        let s = sb.script(&format!("printf '%s' \"$2\" > {}", out.display()));
        let nasty = format!("a.com; touch {}", canary.display());
        let h = ExecHook::new(&s, Duration::from_secs(10));
        rt().block_on(h.run("set", &nasty, "v")).unwrap();
        assert!(!canary.exists(), "元字符被 shell 解释了 —— 这就是命令注入");
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            nasty,
            "参数被改动过"
        );
    }

    #[test]
    fn 失败的_hook_把它说的话带出来() {
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("fail");
        let s = sb.script("echo '凭据过期了' >&2; exit 3");
        let h = ExecHook::new(&s, Duration::from_secs(10));
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        assert!(e.contains("退出码 3"), "{e}");
        assert!(e.contains("凭据过期了"), "{e}");
    }

    #[test]
    fn 卡住的_hook_会被超时杀掉() {
        // ⚠ 沒有超時的話，一個卡住的 hook 會把整條簽發鏈掛住，
        //   而症狀是「證書永遠簽不下來」，看不出是 hook 的問題。
        // ★ ★ 這條測試抓到過一個真缺陷：超時原先只罩住 `wait()`，
        //   而 `sleep 30` 的 hook 一直握著 stdout，於是讀輸出那一步先阻塞了 30 秒——
        //   `timeout` 那一層**根本沒機會開火**。判據就是下面這個 elapsed 斷言。
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("hang");
        let s = sb.script("sleep 30");
        let h = ExecHook::new(&s, Duration::from_millis(300));
        let started = std::time::Instant::now();
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        assert!(e.contains("超时"), "{e}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时没有真的生效：花了 {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn 输出爆炸的_hook_不会把内存吃光() {
        // 吐 100MB，但我們只該收下 MAX_OUTPUT。
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("flood");
        let s = sb.script("head -c 104857600 /dev/zero | tr '\\0' 'x'; exit 1");
        let h = ExecHook::new(&s, Duration::from_secs(30));
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        // 錯誤資訊裡那段是再截過的，這裡只要確認它沒把 100MB 拼進來。
        assert!(e.len() < 4096, "错误信息长达 {} 字节", e.len());
    }

    /// ⚠ ⚠ ⚠ **它也要拿 [`exec_hook_guard`]，而這一條最容易被認為不需要**：
    /// 處理程序根本不存在，`exec` 必然失敗 ⇒ 看起來「它什麼都沒跑」。
    ///
    /// ★ ★ ★ 但 **`spawn` 失敗也要先 `fork`**：子處理程序先被造出來，`exec` 才失敗。
    /// 而 fork 出來、還沒 exec 的那一小段裡，子處理程序的 fd 表是**父處理程序的一份副本**
    /// —— `O_CLOEXEC` 要到 exec 那一刻才生效。⇒ 這條不拿鎖的測試
    /// **會往別的測試的寫句柄上撒一份副本**，而那正是本模組兩條偶發紅的機制：
    ///
    /// · 別的測試 `Sandbox::script()` 裡 `File::create` 那一瞬握著 `hook.sh` 的寫句柄
    ///   ⇒ 這裡 fork 出的子處理程序繼承它 ⇒ 那邊隨後去 `exec hook.sh`，核心回 **ETXTBSY**；
    /// · [`etxtbsy_取证三个方向都成立`] 判據 ② 握著寫句柄掃整個 `/proc`（毫秒級）
    ///   ⇒ 同一份副本被判據 ③ 掃到，而它的 cmdline 恰好是測試二進位自己。
    ///
    /// ⛔ **別把這條註解讀成「加了鎖就一定不再偶發」** —— 它證到的是
    /// 「本處理程序內唯一的 `spawn` 點此後全在同一把鎖裡」，見 [`每条测试都必须拿串行锁`]。
    #[test]
    fn 程序不存在时给一条能看懂的错() {
        let _serial = exec_hook_guard();
        let h = ExecHook::new("/nonexistent/fulcrum-hook", Duration::from_secs(1));
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        assert!(e.contains("起不来"), "{e}");
        assert!(e.contains("/nonexistent/fulcrum-hook"), "{e}");
    }

    /// ★ ★ ★ **結構判據：本模組每一條 `#[test]` 都必須拿 [`exec_hook_guard`]。**
    ///
    /// 理由**不是**「它們都要 exec」—— [`etxtbsy_取证三个方向都成立`] 一個 hook 都不跑。
    /// 理由是它們**共用一張 fd 表**：本模組唯一的 `spawn` 點（`ExecHook::run`）在
    /// `fork` 與 `exec` 之間，會把父處理程序當時開著的**每一個**寫句柄複製一份給子處理程序
    /// （`O_CLOEXEC` 要到 exec 那一刻才生效）⇒ 「誰在 fork」與「誰開著寫句柄」
    /// 這兩件事必須互斥，而互斥的粒度只能是**整條測試**。
    ///
    /// ⚠ ⚠ 它替代不了那把鎖，它守的是**下一個人會不會忘了拿**：
    /// 少一個 `exec_hook_guard()` **不會有任何編譯錯誤**，紅也只是偶發
    /// （實測 1–2%）—— 而一條偶發紅最後的下場是被當成噪音。
    /// ⇒ **把一條機率判據換成一條確定性判據。**
    ///
    /// ⛔ 有意**不留豁免記號**：一個能被隨手貼上去的記號會把這道門變成建議。
    /// 將來真有一條測試既不 fork、也不開寫句柄，那就讓它照拿這把鎖 —— 代價是幾微秒。
    #[test]
    fn 每条测试都必须拿串行锁() {
        let _serial = exec_hook_guard();

        /// 回：哪幾條測試的函式體裡沒有 `exec_hook_guard()`。
        ///
        /// ⚠ 函式體在 **4 空格**縮進上收口，裡層的 `}` 縮進都更深 ⇒ 第一個
        /// `\n    }` 就是這條測試的結尾。少了這一刀，一條漏拿鎖的測試會
        /// **借到下一條的鎖**，而判據照樣是綠的。
        fn 漏网的(源: &str, 标记: &str) -> Vec<String> {
            let mut out = Vec::new();
            for 块 in 源.split(标记).skip(1) {
                let 体 = match 块.find("\n    }") {
                    Some(i) => &块[..i],
                    None => 块,
                };
                let 名 = 体
                    .find("fn ")
                    .and_then(|i| {
                        let 尾 = &体[i + 3..];
                        尾.find('(').map(|j| 尾[..j].to_string())
                    })
                    .unwrap_or_else(|| "<读不出名字>".to_string());
                if !体.contains("exec_hook_guard()") {
                    out.push(名);
                }
            }
            out
        }

        // ⚠ ⚠ 標記**拼出來**，⛔ 不寫成字面值：寫成字面值的話，下面那份反例夾具
        //   自己就會被本檔案的正向掃描數進去，於是這道門每次都紅在自己身上。
        let 标记 = concat!("#", "[test]");

        // ★ ★ **反向那一半，與正向同一次執行**（照 `tests/m0/unclaimed.sh` 的先例）：
        //   一份手寫的、故意漏拿鎖的夾具必須被認出來。
        //   ⛔ 少了它，一個恆回空 `Vec` 的掃描器在下面那條正向判據上是綠的 ——
        //   而那正是「一道從來只見過綠的門」與「一道不存在的門」長得一樣的地方。
        let 夹具 = format!(
            "{标记}\n    fn 拿了的() {{\n        let _serial = exec_hook_guard();\n    }}\n\
             {标记}\n    fn 没拿的() {{\n        assert!(true);\n    }}\n"
        );
        assert_eq!(
            漏网的(&夹具, 标记),
            vec!["没拿的".to_string()],
            "扫描器认不出「漏拿锁」这件事 ⇒ 下面那条正向判据是恒真的"
        );

        // 正向：本模組一條都不許漏。
        let 测试模块 = include_str!("provider.rs")
            .split_once("#[cfg(test)]")
            .expect("本文件应当有 #[cfg(test)] 测试模块")
            .1;
        let 漏 = 漏网的(测试模块, 标记);
        assert!(
            漏.is_empty(),
            "这几条测试没拿 exec_hook_guard()：{漏:?}\n\
             ⚠ 本模块的测试共用一张 fd 表，而唯一的 spawn 点在 fork 与 exec 之间会把\
             父进程开着的写句柄复制给子进程 ⇒ 现场是 ETXTBSY，或者取证扫到「自己」。"
        );
    }

    /// ★ ★ ★ [`etxtbsy_现场`] 的自證 —— **三個方向**。
    ///
    /// ⚠ ⚠ 取證程式碼**只在已經要紅的路徑上執行** ⇒ 一趟綠的門禁從來不碰它，
    /// 它壞了要等到真出事那天才發現，**而那一次現場也就跟著白丟了**。
    /// ★ 這與 `tests/acme/self-check.sh` 掛在 lint 那一格是同一條理由。
    ///
    /// # ⚠ ⚠ ⚠ 它**必須**拿 [`exec_hook_guard`]，而理由與別的幾條不同
    ///
    /// 別的幾條拿這把鎖是因為它們**自己**要 exec；這一條一個 hook 都不跑，
    /// 卻仍然必須串行 —— 它是被**別人的 `fork`** 打中的：
    ///
    /// 1. 判據 ② 要**握著一個可寫句柄**去掃一遍整個 `/proc`，而那一趟是**毫秒級**的，
    ///    不是一瞬 ⇒ 這個句柄開著的視窗相當寬；
    /// 2. 同一個測試二進位裡那幾條 hook 測試會 `fork` 出子處理程序。**fork 之後、exec 之前**，
    ///    子處理程序的 fd 表是父處理程序的一份副本 —— `O_CLOEXEC` 要到 **exec 那一刻**才生效，
    ///    而 `/proc/<子>/cmdline` 那時**還是父處理程序的**；
    /// 3. ⇒ 判據 ③（「沒人持有」）會掃到那個**還沒 exec 的子處理程序**攥著的、
    ///    從判據 ② 繼承過去的那個句柄，而它的 cmdline 恰好就是測試二進位自己。
    ///
    /// ★ ★ **現場讀起來像「本處理程序自己沒關乾淨」，而那是假的** ——
    /// 這正是最貴的那種偶發紅：它把人指向一個不存在的缺陷。
    ///
    /// 實測（同一棵樹、同一個鏡像）：並行跑整套 lib **60 趟紅 2 趟**；
    /// 只跑這一條 **30 趟 0 紅**；`--test-threads=1` **30 趟 0 紅**。
    /// ⇒ 觸發條件是**同二進位內的並行**，不是這條判據本身。
    /// ⛔ 修法不是給它加重試、也不是把判據 ③ 放寬成「只要不是本處理程序就算沒人持有」——
    /// 後者會把**真的**有別的處理程序攥著的那種現場一起判成綠。
    #[test]
    fn etxtbsy_取证三个方向都成立() {
        let _serial = exec_hook_guard();
        let 沙箱 = Sandbox::new("forensics");
        let p = 沙箱.path("target.bin");
        std::fs::write(&p, b"x").unwrap();
        let busy = std::io::Error::from_raw_os_error(ETXTBSY);

        // ① 別的 errno **不許**觸發取證 —— 否則每一次「檔案不存在」都會去掃一遍 /proc。
        let 别的 = std::io::Error::from_raw_os_error(2); // ENOENT
        assert!(
            etxtbsy_现场(&p, &别的).is_empty(),
            "非 ETXTBSY 的错误竟然也去取证了"
        );

        // ② 命中：本處理程序以**寫**方式開著它，取證必須指出是自己。
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        let 现场 = etxtbsy_现场(&p, &busy);
        assert!(
            现场.contains(&format!("pid={}", std::process::id())),
            "取证没把持有者指成本进程：{现场}"
        );
        assert!(现场.contains("可写=是"), "取证没认出这是个可写句柄：{现场}");
        drop(f);

        // ③ 落空：沒人開著它 —— ⚠ 而措辭必須是「**沒找到**」，
        //    ⛔ 不許說成「沒有人持有」（取證發生在失敗之後，那不是同一句話）。
        let 空 = etxtbsy_现场(&p, &busy);
        assert!(
            空.contains("没找到") && !空.contains("可写=是"),
            "没人持有时取证的措辞不对：{空}"
        );
    }

    /// ★ ★ ★ **把那條偶發紅確定性地重現一次**，並證明取證真的接在錯誤資訊上。
    ///
    /// 機制：Linux 上**握著一個可寫 fd 去 exec 同一個檔案必然 `ETXTBSY`**。
    /// ⇒ 這條不是「等它偶發」，是**當場造出來**。
    ///
    /// ⚠ ⚠ 它證的是**串接**（現場進得了錯誤資訊），⛔ 它本身**不是**那條偶發紅的根因。
    /// ★ 而根因後來正是被這條串接**抓出來的**：那個「還沒 exec 的子處理程序」寫在
    /// [`etxtbsy_现场`] 的文件裡。⇒ 這條留著，因為它把「有人開著寫句柄 ⇒ `ETXTBSY`」
    /// 這半句釘成了確定性的，而那半句是整條推理的地基。
    /// ⚠ 舊的候選（docker overlayfs 的可見性延遲）**不再解釋得了它**：一次純粹的
    /// 串行化改動就把紅率從 ≈2.4% 打到 0/300，而一個檔案系統的可見性延遲不會理會一把鎖。
    #[test]
    fn 起不来时错误信息里带着_etxtbsy_现场() {
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("busy");
        let s = sb.script("exit 0");
        // ★ 就是這一句造出 ETXTBSY：寫句柄還開著，核心不許 exec 它。
        let 写句柄 = std::fs::OpenOptions::new().write(true).open(&s).unwrap();
        let h = ExecHook::new(&s, Duration::from_secs(10));
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        drop(写句柄);

        assert!(e.contains("起不来"), "{e}");
        assert!(
            e.contains("ETXTBSY 现场"),
            "ETXTBSY 起不来时错误信息里没有现场 —— 取证没接上：{e}"
        );
        assert!(
            e.contains(&format!("pid={}", std::process::id())),
            "现场没把持有者指成本进程（而本进程正握着那个写句柄）：{e}"
        );
    }
}
