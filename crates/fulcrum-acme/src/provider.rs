//! DNS-01 的「谁去改那条 TXT」（G57）。
//!
//! # ★ G57 的形状是「不先建体系」
//!
//! > 原生支持按**真实用到的**一家一家加。§6.2 明确不做服务发现集成，
//! > **一个 DNS 供应商动物园是同一个形状**。
//!
//! 所以这里是一个**枚举**而不是一个 `dyn Trait` 的插件框架：
//! 现在只有 exec hook 一个变体，加 Cloudflare / DNSPod 时就多一个变体。
//! ⚠ 先摆一个 trait 出来，等于先把动物园的笼子盖好——那正是 G57 拒绝的东西。
//!
//! # ⚠ exec hook 是新的攻击面，G57 点名要过安全基线
//!
//! | 要求 | 这里怎么做的 |
//! |---|---|
//! | 不经 shell 解释 | `Command::new(程序)` + `.arg()`，**从不拼接命令行字符串** |
//! | 参数不拼字符串 | 域名与值各自是一个独立 `arg`，引号/分号/反引号一律只是普通字节 |
//! | 超时 | `tokio::time::timeout`，到点**杀掉**（`kill_on_drop`）|
//! | 输出上限 | stdout/stderr 各自只读前 [`MAX_OUTPUT`] 字节 |
//! | 以运行用户身份跑 | 不做任何提权，环境原样继承（G59：凭据正是从环境或文件来的）|
//!
//! ★ **不记录环境变量**（基线第 3 条）：hook 的凭据就在里面。

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Linux 的 `ETXTBSY`。⚠ 直接写数字而不引 `libc`：为一个常量新增一个包，
/// 撞的是「新增 0 个包」那条纪律，而这个值是 ABI 的一部分，不会变。
const ETXTBSY: i32 = 26;

/// `ETXTBSY` 的**现场取证**：到底是谁以写方式开着这个文件。
///
/// # ★ ★ ★ 它为什么存在
///
/// 门禁里有一条**偶发**红：`起不来 DNS hook …：Text file busy (os error 26)`，
/// 只在并发跑全套单测时出现，单独复跑全绿。⛔ **owner 明令不许加重试** ——
/// 重试会把这条测试真正守着的那个缺陷一起盖掉。
/// ⇒ 那就**先建证据**：`ETXTBSY` 在 Linux 上的含义是**有进程以写方式打开着这个文件**，
/// 而「是谁」`/proc` 答得出来。★ 根因没定位之前的修复是猜。
///
/// # ⚠ 它答不了什么
///
/// 取证发生在**失败之后**，那一刻持有者**可能已经关掉了**（如果它是一个瞬时的写句柄，
/// 那正是本条最可能的形态）⇒ **扫不到持有者不等于当时没有**。
/// ⛔ 因此「没找到」要说成「没找到」，不许说成「没有人持有」。
///
/// ⚠ 只在 Linux 上有 `/proc`；别的平台回空串（本产品只发行 Linux，G13）。
fn etxtbsy_现场(program: &Path, e: &std::io::Error) -> String {
    if e.raw_os_error() != Some(ETXTBSY) {
        return String::new();
    }
    // ⚠ `/proc/<pid>/fd/<n>` 是**解析过**的路径 ⇒ 比较之前两边都要归一化，
    //   否则 `/tmp/...` 与 `/private/tmp/...` 这类差别会让取证**永远扫不到**，
    //   而它给出的答案（「没找到持有者」）读起来完全正常。
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
                continue; // 别的用户的进程，读不了 —— ⚠ 这也是「扫不到」的一种
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
                // O_WRONLY = 1、O_RDWR = 2 ⇒ 低两位非零就是「可写」。
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

/// hook 的 stdout / stderr 各自最多收多少字节。
///
/// ★ 够放一条错误信息，又不至于让一个疯掉的 hook 把内存吃光。
const MAX_OUTPUT: u64 = 8 * 1024;

/// 启动时校验（**G59 第 2 条**）的结论。
///
/// ★ ★ **这两种必须分开，而且分法要挂在类型上，不是在调用方 match 错误串。**
///
/// | | 意思 | 处置 |
/// |---|---|---|
/// | [`VerifyError::Fatal`] | 对端**回话了**，说这份凭据不行；或者凭据根本读不出来 | **拒绝启动**（形状照 G15）|
/// | [`VerifyError::Inconclusive`] | 压根没收到回话（网络不通、超时）| 打一条 **error** 继续跑 |
///
/// ⚠ ⚠ 为什么第二种不拒绝启动：一次网络抖动不该让整台机器上**所有**站点都起不来，
/// 包括那些用静态证书、跟 ACME 毫无关系的。★ **这是一处知情的取舍**——
/// 它与本仓库那条「『没能检查』当成『检查通过』是栽过的形状」是有张力的，
/// 所以处置不是「悄悄跳过」，而是**打 error 说清楚这份凭据没被验过**。
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

/// 谁去改 TXT 记录。
///
/// ★ 批 8：多了两家**原生**（G57）。它仍然是一个**枚举**而不是
/// `dyn Trait` 的插件框架——G57 的形状是「不先建体系，按真实用到的一家一家加」。
#[derive(Debug, Clone)]
pub enum DnsProvider {
    /// `tls { dns exec <程序> }` —— 调一个外部程序。
    Exec(ExecHook),
    /// `tls { dns cloudflare env:… }` —— `example.net` 在这家。
    Cloudflare(crate::cloudflare::Cloudflare),
    /// `tls { dns dnspod file:… }` —— `example.com` 在这家。
    Dnspod(crate::dnspod::Dnspod),
}

impl DnsProvider {
    /// 供应商的名字，只进日志。
    pub fn name(&self) -> &'static str {
        match self {
            DnsProvider::Exec(_) => "exec",
            DnsProvider::Cloudflare(_) => "cloudflare",
            DnsProvider::Dnspod(_) => "dnspod",
        }
    }

    /// **G59 第 2 条**：启动时能校验的就校验。
    ///
    /// ⚠ exec hook 没有可校验的东西（它是一个外部程序，我们不知道它要什么凭据），
    /// 所以它这一支是 `Ok(())` —— ★ **不是「跳过检查」，是「这里没有可检查的东西」**。
    /// 两者的区别在于：前者应当被记下来，后者不应当。
    pub async fn verify(&self) -> Result<(), VerifyError> {
        match self {
            DnsProvider::Exec(_) => Ok(()),
            DnsProvider::Cloudflare(c) => c.verify().await,
            DnsProvider::Dnspod(d) => d.verify().await,
        }
    }

    /// 挂上一条 TXT。
    pub async fn set_txt(&self, name: &str, value: &str) -> Result<(), String> {
        match self {
            DnsProvider::Exec(h) => h.run("set", name, value).await,
            DnsProvider::Cloudflare(c) => c.set_txt(name, value).await,
            DnsProvider::Dnspod(d) => d.set_txt(name, value).await,
        }
    }

    /// 摘掉一条 TXT。
    ///
    /// ⚠ 失败**不该让签发失败**——证书已经签下来了，一条留在那里的 TXT
    /// 是卫生问题不是可用性问题。调用方只记一条 warn。
    pub async fn clear_txt(&self, name: &str, value: &str) -> Result<(), String> {
        match self {
            DnsProvider::Exec(h) => h.run("clear", name, value).await,
            DnsProvider::Cloudflare(c) => c.clear_txt(name, value).await,
            DnsProvider::Dnspod(d) => d.clear_txt(name, value).await,
        }
    }
}

/// 一个外部程序。
#[derive(Debug, Clone)]
pub struct ExecHook {
    /// 程序路径。★ 是**路径**不是命令行——没有 shell 参与，所以不存在「怎么分词」。
    pub program: PathBuf,
    /// 单次调用的超时。
    pub timeout: Duration,
}

impl ExecHook {
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> ExecHook {
        ExecHook {
            program: program.into(),
            timeout,
        }
    }

    /// 调用约定：`<程序> <set|clear> <记录名> <值>`。
    async fn run(&self, action: &str, name: &str, value: &str) -> Result<(), String> {
        use std::process::Stdio;
        use tokio::process::Command;

        // ★ ★ 三个参数各自是一个 `arg`。**这就是「不拼字符串」的全部含义**：
        //   一个叫 `a.com; rm -rf /` 的域名在这里只是一个普通的 argv[2]，
        //   因为**根本没有 shell 去解释它**。
        let mut child = Command::new(&self.program)
            .arg(action)
            .arg(name)
            .arg(value)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // ⚠ 超时之后如果不杀，它会变成一个没人管的孤儿进程，
            //   而且还握着我们的 DNS 凭据。
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

        // ★ ★ ★ **超时必须罩住「读输出」，不能只罩住 `wait()`。**
        //
        //   ⚠ 写成「先把 stdout/stderr 读到 EOF，再 `timeout(wait())`」是错的：
        //   一个 `sleep 30` 的 hook **一直握着 stdout**，`read_to_string` 自己就先阻塞了，
        //   超时那一层根本没机会开火 ⇒ `self.timeout` 变成一句空话，
        //   而它存在的全部理由就是别让一个卡住的 hook 挂住整条签发链。
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
                // `kill_on_drop` 会在 child 落地时收尸，这里显式再杀一次更直接。
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
        // ★ 把 hook 说了什么带出来。一个只说「hook 失败」的错误，
        //   等于让人自己去猜是凭据错了还是域名写错了。
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

    /// 给一条测试开一个**它自己的**临时目录。
    ///
    /// ⚠ ⚠ **不能按脚本正文派生文件名**（比如按字节长度）：两条正文恰好等长的测试会共用
    /// 同一个文件，而每条测试收尾都会把它删掉 —— **一条测试删掉了另一条正在用的脚本**。
    /// ★ 症状极具迷惑性：**单独跑每条都绿，一起跑才红**，红的那条还随调度而变
    ///   （`cargo test` 默认多线程并行跑同一个二进制里的测试）。
    /// ⇒ 每条测试一个自己的目录，共用在结构上就不可能发生。
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

        /// 在本沙箱里写一个可执行脚本。
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

    /// 让下面这几条 `ExecHook` 测试**串行跑**。
    ///
    /// ⚠ ⚠ **这不是把测试静音**：它们照跑、验的还是同一件事，
    /// 被消除的只是**测试之间的并发** —— 而那个并发不是被测对象的一部分。
    ///
    /// 起因是一次**只改了 markdown** 的门禁：
    /// `卡住的_hook_会被超时杀掉` 报 `起不来 DNS hook …：Text file busy (os error 26)`。
    /// 单独复跑三次全绿 ⇒ **只在并发跑全套单测时出现**。
    ///
    /// ★ **已经排除的两条，别重走**：
    /// 五个 sandbox tag（`args`/`meta`/`fail`/`hang`/`flood`）**互不相同**，
    /// 所以不是两条测试抢同一个 `hook.sh`（那个缺陷上面那段注释里记着，早修过了）；
    /// `Sandbox::script` 里有显式 `drop(f)` 且 `set_permissions` 在它之后，
    /// 所以不是写句柄没关。
    ///
    /// ⚠ ⚠ **根因没查到底。** 能确定的只有：报错来自 **exec** 那一步，
    /// 而 Linux 上 `ETXTBSY` 意味着**有进程以写方式打开着这个文件**。
    /// 剩下的候选是 docker overlayfs 的可见性延迟，或者某个还没看见的持有者。
    ///
    /// ★ 那为什么先上串行：它**便宜**，而且**即使根因不是并发，它也不掩盖任何东西** ——
    /// 每一条断言都照样跑。⇒ 这与「加重试」或 `#[ignore]` 是相反性质的处置：
    /// 那两种会让这条测试守着的那个真缺陷（超时原先只罩 `wait()`，
    /// 握着 stdout 的 hook 让 `timeout` 根本没机会开火）失去守卫。
    static EXEC_HOOK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// ★ ★ **串行化自己的自证**：任一时刻有几条在临界区里。
    ///
    /// ⚠ 少了它，串行化就是一句**没有判据的断言** —— 哪天有人删掉某条测试开头
    /// 那行 `let _serial = …`，或者把锁换成别的东西，**一切照旧全绿**，
    /// 而那条偶发红会在几周后重新出现、且看不出与这次改动有关。
    /// ★ 这与本仓库那条「一个被豁免又不被报告的测试，等于从这道门上消失了」同源。
    static EXEC_HOOK_IN_FLIGHT: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// 持有期间计数 +1，释放时 -1。
    struct SerialGuard {
        // ⚠ 字段顺序即析构顺序：先放计数（后声明的先析构？不 —— Rust 按声明顺序析构），
        //   所以这里**只靠 Drop 里显式做减法**，不依赖字段顺序。
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for SerialGuard {
        fn drop(&mut self) {
            EXEC_HOOK_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// 取那把锁。
    ///
    /// ⚠ **中毒也照常拿**：一条测试 panic 会毒化这把锁，而默认的 `unwrap()`
    /// 会让**后面每一条**都 panic 在锁上 —— 那时屏幕上报出来的失败，
    /// 与真正坏掉的那条毫无关系。★ 一道会转述错原因的门，比没有这道门更耽误人。
    fn exec_hook_guard() -> SerialGuard {
        let lock = EXEC_HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // ★ 拿到锁之后才 +1：那一刻临界区里本该只有自己。
        let prev = EXEC_HOOK_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            prev, 0,
            "串行化没生效：拿到锁的这一刻，临界区里已经有 {prev} 条测试在跑"
        );
        SerialGuard { _lock: lock }
    }

    #[test]
    fn 成功的_hook_返回_ok_并且拿得到三个参数() {
        // 把参数原样写进文件，再核对——**判据是「它收到的正是我们给的」**。
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
        // ★ ★ 这条是 G57 那句「参数不拼字符串」的判据。
        //   如果实现里有 shell 参与，下面这个域名会把 `touch` 真的跑起来。
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
        // ⚠ 没有超时的话，一个卡住的 hook 会把整条签发链挂住，
        //   而症状是「证书永远签不下来」，看不出是 hook 的问题。
        // ★ ★ 这条测试抓到过一个真缺陷：超时原先只罩住 `wait()`，
        //   而 `sleep 30` 的 hook 一直握着 stdout，于是读输出那一步先阻塞了 30 秒——
        //   `timeout` 那一层**根本没机会开火**。判据就是下面这个 elapsed 断言。
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
        // 吐 100MB，但我们只该收下 MAX_OUTPUT。
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("flood");
        let s = sb.script("head -c 104857600 /dev/zero | tr '\\0' 'x'; exit 1");
        let h = ExecHook::new(&s, Duration::from_secs(30));
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        // 错误信息里那段是再截过的，这里只要确认它没把 100MB 拼进来。
        assert!(e.len() < 4096, "错误信息长达 {} 字节", e.len());
    }

    #[test]
    fn 程序不存在时给一条能看懂的错() {
        let h = ExecHook::new("/nonexistent/fulcrum-hook", Duration::from_secs(1));
        let e = rt().block_on(h.run("set", "a.com", "v")).unwrap_err();
        assert!(e.contains("起不来"), "{e}");
        assert!(e.contains("/nonexistent/fulcrum-hook"), "{e}");
    }

    /// ★ ★ ★ [`etxtbsy_现场`] 的自证 —— **三个方向**。
    ///
    /// ⚠ ⚠ 取证代码**只在已经要红的路径上执行** ⇒ 一趟绿的门禁从来不碰它，
    /// 它坏了要等到真出事那天才发现，**而那一次现场也就跟着白丢了**。
    /// ★ 这与 `tests/acme/self-check.sh` 挂在 lint 那一格是同一条理由。
    #[test]
    fn etxtbsy_取证三个方向都成立() {
        let 沙箱 = Sandbox::new("forensics");
        let p = 沙箱.path("target.bin");
        std::fs::write(&p, b"x").unwrap();
        let busy = std::io::Error::from_raw_os_error(ETXTBSY);

        // ① 别的 errno **不许**触发取证 —— 否则每一次「文件不存在」都会去扫一遍 /proc。
        let 别的 = std::io::Error::from_raw_os_error(2); // ENOENT
        assert!(
            etxtbsy_现场(&p, &别的).is_empty(),
            "非 ETXTBSY 的错误竟然也去取证了"
        );

        // ② 命中：本进程以**写**方式开着它，取证必须指出是自己。
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        let 现场 = etxtbsy_现场(&p, &busy);
        assert!(
            现场.contains(&format!("pid={}", std::process::id())),
            "取证没把持有者指成本进程：{现场}"
        );
        assert!(现场.contains("可写=是"), "取证没认出这是个可写句柄：{现场}");
        drop(f);

        // ③ 落空：没人开着它 —— ⚠ 而措辞必须是「**没找到**」，
        //    ⛔ 不许说成「没有人持有」（取证发生在失败之后，那不是同一句话）。
        let 空 = etxtbsy_现场(&p, &busy);
        assert!(
            空.contains("没找到") && !空.contains("可写=是"),
            "没人持有时取证的措辞不对：{空}"
        );
    }

    /// ★ ★ ★ **把那条偶发红确定性地复现一次**，并证明取证真的接在错误信息上。
    ///
    /// 机制：Linux 上**握着一个可写 fd 去 exec 同一个文件必然 `ETXTBSY`**。
    /// ⇒ 这条不是「等它偶发」，是**当场造出来**。
    ///
    /// ⚠ ⚠ 它证的是**接线**（现场进得了错误信息），⛔ **不是**门禁里那条偶发红的根因 ——
    /// 那一条至今没定位：候选是 docker overlayfs 的可见性延迟，或某个还没看见的持有者。
    /// ★ 而这条测试正是为下一次它偶发时**留下现场**用的。
    #[test]
    fn 起不来时错误信息里带着_etxtbsy_现场() {
        let _serial = exec_hook_guard();
        let sb = Sandbox::new("busy");
        let s = sb.script("exit 0");
        // ★ 就是这一句造出 ETXTBSY：写句柄还开着，内核不许 exec 它。
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
