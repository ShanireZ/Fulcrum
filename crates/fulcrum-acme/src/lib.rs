//! 枢衡的 ACME 层（G53/G54/G56）：**自动签发与续期**。
//!
//! ```text
//! fulcrum-tls   证书存储（G55）· 按 SNI 挑证书 · 续期判定（G56，纯函数）
//!      ▲
//!      │ 往里写
//! fulcrum-acme  账户 · 订单 · 挑战求解 · 什么时候该签
//! ```
//!
//! # ★ 这个 crate 不依赖 pingora
//!
//! 与 [`fulcrum_runtime`] 同一条纪律：它是「什么时候该签、怎么签」的**逻辑**，
//! 不是「挂在哪个进程模型上」。停机信号用 [`tokio::sync::watch::Receiver<bool>`] 接——
//! 那恰好就是 `pingora_core::server::ShutdownWatch` 的真身，
//! 所以 `fulcrum-server` 把它直接传进来就行，两边都不必知道对方。
//!
//! 三种挑战都已端到端可签（门禁里由 pebble 这个真 CA 验过）：
//! **TLS-ALPN-01**（G54 的主）· **HTTP-01**（备）· **DNS-01 / 通配符**（G57/G58）。
//!
//! ★ **没有端到端判据的 ACME 代码正是本仓库最不该写的东西** —— 分批的判据一直是
//! 「哪一条现在就能被真跑一遍」，不是「哪一条更重要」。
//!
//! # ★ ★ 与 G55 怎么对接
//!
//! 升级窗口里两代进程共享同一个证书目录 ⇒ 签发路径**整条**握着 [`CertStore::lock`]
//! 那把跨进程文件锁：拿不到就等，等到了**先重读一次存储** —— 多半另一代已经签好了。
//! 那不只是省一次请求：CA 的速率配额**按账户**算，两代各签一张会同时记账。

pub mod account;
pub mod cloudflare;
pub mod credential;
pub mod dns;
pub mod dnspod;
pub mod http;
pub mod http01;
pub mod https;
pub mod issue;
pub mod provider;
pub mod tlsalpn01;
pub mod zone_scope;

pub use account::AccountStore;
pub use dns::TxtChecker;
pub use http01::Http01Store;
pub use provider::{DnsProvider, ExecHook};

use fulcrum_tls::renewal::Backoff;
use fulcrum_tls::{CertStore, SniResolver};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Let's Encrypt 生产目录。`acme_ca` 没写时用它。
pub const LETSENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt staging 目录。
pub const LETSENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// 两次巡检之间最长等多久。★ 不是「多久续一次」——续期时刻由 [`fulcrum_tls::renewal`] 定，
/// 这里只是「多久回来看一眼」的上限。
pub const MAX_IDLE: Duration = Duration::from_secs(12 * 3600);
/// 最短巡检间隔。挡的是「刚失败就立刻重来」把 CA 打成尖峰。
pub const MIN_IDLE: Duration = Duration::from_secs(30);

/// ACME 的全局设定，来自 DSL 的全局选项块。
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// `acme_ca <目录 URL>`。
    pub directory_url: String,
    /// `acme_email <邮箱>`。★ 没写不是错误：ACME 不强制联系方式，
    /// 但**没有它就收不到 CA 的到期与撤销通知**，所以装载时要说一声。
    pub contact: Option<String>,
    /// 状态目录（G33）。证书在 `<state>/certs/`，账户在 `<state>/acme/`。
    pub state_dir: PathBuf,
}

impl AcmeConfig {
    /// 从全局选项造一份。`acme_ca` 空着就用 Let's Encrypt 生产目录。
    pub fn new(acme_ca: Option<&str>, acme_email: Option<&str>, state_dir: &str) -> AcmeConfig {
        AcmeConfig {
            directory_url: acme_ca.unwrap_or(LETSENCRYPT_PRODUCTION).to_string(),
            contact: acme_email.map(|s| s.to_string()),
            state_dir: PathBuf::from(state_dir),
        }
    }

    /// 证书存储的根：`<state>/certs`。
    pub fn cert_root(&self) -> PathBuf {
        self.state_dir.join("certs")
    }

    /// 账户存储的根：`<state>/acme`。
    pub fn account_root(&self) -> PathBuf {
        self.state_dir.join("acme")
    }

    /// 这个 CA 在存储里占哪一个目录名。
    pub fn issuer(&self) -> String {
        issuer_slug(&self.directory_url)
    }
}

/// 一个 CA 的目录 URL → 存储里的目录名。
///
/// ★ ★ **这是一个有损映射，而且是故意的。** 可读性在这里是有价值的——G55 选
/// 「每域一目录的 PEM」的全部理由就是**出事时人能直接进去看**，
/// 而 `ca-localhost_3a14000_2fdir` 这种可逆转义把那点价值抵消掉了。
///
/// ⚠ 有损就意味着**两个不同的 CA 可能落到同一个目录名**（同主机不同路径）。
/// 那个风险不是靠命名挡掉的，是靠 [`fulcrum_tls::Meta::issuer_url`] 记下真实来源、
/// 装载时比对挡掉的——**判据挂在数据上，不挂在命名约定上**。
///
/// ★ 顺带：这也是与 `store::sanitize` 那条「必须单射」的**区别所在**。
/// 那边单射是安全要求（两个域名共用一张证书 = 安全缺陷），这边不是：
/// 撞了的后果只是「发现来源对不上、重签一张」。**同样的问题在不同位置上答案不同。**
pub fn issuer_slug(directory_url: &str) -> String {
    match directory_url {
        LETSENCRYPT_PRODUCTION => return "letsencrypt".to_string(),
        LETSENCRYPT_STAGING => return "letsencrypt-staging".to_string(),
        _ => {}
    }
    // 抠出 host[:port]：去掉 scheme，取到第一个 `/` 为止。
    let rest = directory_url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(directory_url);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let mut out = String::from("ca-");
    for ch in hostport.chars() {
        // ★ 白名单而不是黑名单：路径分隔符、`..`、控制字符、非 ASCII 一律不进目录名。
        //   黑名单式的「把这几个危险字符换掉」漏一个就是一次目录穿越。
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    // 全是分隔符时别留下一个空目录名。
    if out == "ca-" {
        out.push_str("unknown");
    }
    out
}

/// DNS-01 需要的两半：**谁去改记录**，与**怎么确认它可见了**。
///
/// ★ ★ 两半缺一不可，而第二半正是 G58 写死的那条：
/// 「绝不能只 sleep 一个固定秒数」。所以这个结构里没有「等几秒」这种字段——
/// 只有一个会真去问权威 NS 的 [`TxtChecker`]。
#[derive(Debug)]
pub struct Dns01 {
    pub provider: DnsProvider,
    pub checker: TxtChecker,
}

/// 一个要维护证书的域名。
#[derive(Debug)]
pub struct Target {
    /// 证书上的域名。可能是 `*.example.com`。
    pub domain: String,
    /// 它是哪个站点要的 —— 只进日志，让人知道该去配置的哪一行看。
    pub site: String,
    /// 这个站点配的 DNS-01。`None` = 没配 `tls { dns … }`。
    ///
    /// ★ 挂在 target 上而不是全局：`dns` 与 `resolvers` 在 DSL 里是**站点级**的，
    /// 不同站点完全可以托管在不同服务商。做成全局会在两家托管商的场景下直接错。
    pub dns01: Option<std::sync::Arc<Dns01>>,
}

impl Target {
    /// 通配符**只能**走 DNS-01（G54）——HTTP-01 与 TLS-ALPN-01 都证明不了「整个子域」。
    pub fn is_wildcard(&self) -> bool {
        self.domain.starts_with("*.")
    }

    /// 这一轮接得了吗。
    ///
    /// ★ 判据是「**这个域名要用的那种挑战，现在有没有人接**」：
    /// 通配符必须有 DNS-01；非通配符没配 DNS 就走 HTTP-01（已接线）。
    pub fn actionable(&self) -> bool {
        !self.is_wildcard() || self.dns01.is_some()
    }

    /// 走哪种挑战。
    ///
    /// ★ **配了 `dns` 就走 DNS-01**，即使不是通配符——那是配置里明写的意图
    /// （常见理由：这台机器的 80 端口根本不对公网开）。没配就走 HTTP-01。
    pub fn use_dns01(&self) -> bool {
        self.dns01.is_some()
    }

    /// DNS-01 要改的那条记录叫什么。
    ///
    /// ★ ★ **通配符要把 `*.` 剥掉**：`*.example.com` 的挑战记录是
    /// `_acme-challenge.example.com`，**不是** `_acme-challenge.*.example.com`
    /// （那甚至不是一个合法的域名）。RFC 8555 §8.4 就是这么定义的。
    /// ⚠ 写错这一条的症状是「CA 说验不过」，而 DNS 那边一切正常——
    /// 因为记录确实写上去了，只是写在了一个没人会查的名字上。
    pub fn challenge_record(&self) -> String {
        let base = self.domain.strip_prefix("*.").unwrap_or(&self.domain);
        format!("_acme-challenge.{base}")
    }
}

/// 一次巡检的结果。★ 它是给日志与测试看的，不是给控制流看的。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 这一轮真的签下来的域名。
    pub issued: Vec<String>,
    /// 存储里已经有、且还不到续期时刻的。
    pub fresh: Vec<String>,
    /// 这一批还接不了的（通配符要 DNS-01）。
    pub deferred: Vec<String>,
    /// 被退避压住、这一轮**根本没有去试**的。
    ///
    /// ★ ★ 它与 [`Report::failed`] 必须分开：「这一次又失败了」要计数、要告警，
    /// 而「上次失败之后还在等」是退避**正在正常工作**的样子。
    /// 混在一起会让一次失败在监控里变成每分钟一条告警——而那恰好是退避要消灭的东西。
    pub backed_off: Vec<String>,
    /// 出错的：`(域名, 原因)`。
    pub failed: Vec<(String, String)>,
    /// 下一次巡检最晚不该迟于此。`None` = 没有任何域名要盯着。
    pub next_check: Option<Duration>,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.issued.is_empty()
            && self.fresh.is_empty()
            && self.deferred.is_empty()
            && self.backed_off.is_empty()
            && self.failed.is_empty()
    }

    /// 收一条「下次多久再看」的建议，取最紧的那条。
    pub fn note_next_check(&mut self, d: Duration) {
        self.next_check = Some(match self.next_check {
            None => d,
            Some(cur) => cur.min(d),
        });
    }

    /// 这一轮之后该睡多久。
    ///
    /// ★ 夹在 [`MIN_IDLE`] 与 [`MAX_IDLE`] 之间：下界挡「刚失败就立刻重来」，
    /// 上界保证**即使所有判定都说不用管**，也仍然会定期回来看一眼——
    /// 一个「等到需要时才醒」的调度器，在时钟跳变或判定写错时会**永远睡下去**，
    /// 而那种失败没有任何症状。
    pub fn sleep_for(&self) -> Duration {
        self.next_check
            .unwrap_or(MAX_IDLE)
            .clamp(MIN_IDLE, MAX_IDLE)
    }
}

/// 自动签发与续期的执行者。
pub struct AcmeManager {
    cfg: AcmeConfig,
    issuer: String,
    store: CertStore,
    accounts: AccountStore,
    resolver: Arc<SniResolver>,
    http01: Arc<Http01Store>,
    targets: Vec<Target>,
    backoff: Backoff,
    jitter: Jitter,
    /// **强制续期**队列（G74）。管理面往里放域名，巡检取走。
    ///
    /// ★ 它不是「为测试造的旋钮」：证书出事（密钥泄露、CA 吊销、SAN 要改）时，
    /// 运维本来就需要「立刻重签一张」，而在它之前唯一的办法是删文件再等巡检。
    /// ⚠ 它也是 G58 那条「在真域名上续期一次」**唯一能在冒烟里做到**的路——
    /// 真证书 90 天、ARI 窗口约 60 天后才开，等不到。
    /// ★ 值是 **要不要连退避一起清**（加的第二档，见 `request_renew`）。
    force: std::sync::Mutex<std::collections::BTreeMap<String, bool>>,
    /// 叫醒巡检。★ 没有它，一次强制续期最长要等 12 小时（[`MAX_IDLE`]）才生效——
    /// 而「按了没反应」会让人以为这个口子坏了，然后去动别的东西。
    wake: tokio::sync::Notify,
}

impl AcmeManager {
    pub fn new(
        cfg: AcmeConfig,
        resolver: Arc<SniResolver>,
        http01: Arc<Http01Store>,
        targets: Vec<Target>,
    ) -> AcmeManager {
        let store = CertStore::new(cfg.cert_root());
        let accounts = AccountStore::new(cfg.account_root());
        let issuer = cfg.issuer();
        AcmeManager {
            cfg,
            issuer,
            store,
            accounts,
            resolver,
            http01,
            targets,
            backoff: Backoff::default(),
            jitter: Jitter::from_clock(),
            force: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            wake: tokio::sync::Notify::new(),
        }
    }

    pub fn config(&self) -> &AcmeConfig {
        &self.cfg
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    pub fn http01(&self) -> &Arc<Http01Store> {
        &self.http01
    }
}

/// 抖动源。
///
/// ★ **不拉 `rand`**：这里要的不是密码学随机，是「几十个域名同时到期时别一起重试」。
/// 一个 splitmix64 就够，而少一条依赖就少一次 G29 的每周检查与一次供应链审计。
/// ⚠ 而它**只用在退避时长上**——任何与密钥、nonce、token 有关的随机都由
/// `instant-acme` / `ring` 出，绝不由这里出。
#[derive(Debug)]
pub struct Jitter {
    state: std::sync::atomic::AtomicU64,
}

impl Jitter {
    /// 用「时刻 + pid」做种。★ 加 pid 是必需的：升级窗口里两代进程**同时**起来，
    /// 只用时刻做种时它们很可能拿到同一个种子，于是抖动完全同步——那就等于没抖。
    pub fn from_clock() -> Jitter {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Jitter::with_seed(nanos ^ (u64::from(std::process::id()) << 32))
    }

    pub fn with_seed(seed: u64) -> Jitter {
        Jitter {
            state: std::sync::atomic::AtomicU64::new(seed | 1),
        }
    }

    /// 取一个 `[0, 1)` 的数。
    pub fn unit(&self) -> f64 {
        use std::sync::atomic::Ordering;
        let mut z = self
            .state
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // 取高 53 位：f64 的尾数就是 53 位，用低位会在转换时被丢掉。
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 两家_lets_encrypt_有可读的目录名() {
        assert_eq!(issuer_slug(LETSENCRYPT_PRODUCTION), "letsencrypt");
        assert_eq!(issuer_slug(LETSENCRYPT_STAGING), "letsencrypt-staging");
    }

    #[test]
    fn 别的_ca_落到_ca_前缀且不含路径分隔符() {
        // ★ 判据不是「长得好看」，是**不含任何能穿出目录的东西**。
        for url in [
            "https://localhost:14000/dir",
            "https://ca.internal/acme/directory",
            "http://10.0.0.1:8080/dir",
            // ⚠ 恶性输入：路径穿越、绝对路径、NUL 之外的控制字符
            "https://../../etc/dir",
            "https://a/../../b/dir",
            "https://ca.example.com/../../../../root",
        ] {
            let slug = issuer_slug(url);
            assert!(slug.starts_with("ca-"), "{url} → {slug}");
            assert!(!slug.contains('/'), "{url} → {slug} 里有路径分隔符");
            assert!(!slug.contains('\\'), "{url} → {slug} 里有反斜杠");
            // ★ ★ 判据是**「这个目录名本身能不能往上走」**，不是「它里面有没有出现两个点」。
            //   ⚠ 写成「按 `-` 切开之后不许有一段等于 `..`」是错的：`ca-..` 会被判红，
            //   而它是一个**字面量目录名**，`join` 上去只得到一个叫 `ca-..` 的普通目录。
            //   ★ **判据要照着「危险的定义」写，不是照着「危险的样子」写** —— 真正危险的
            //   只有 `.` 与 `..` 这两个名字本身，而 `ca-` 前缀已经让它们不可能出现：
            //   **前缀在这里是安全机制，不是装饰**。
            assert!(
                slug != "." && slug != "..",
                "{url} → {slug} 是一个能往上走的名字"
            );
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
                "{url} → {slug} 里有白名单之外的字符"
            );
        }
        // 反证：把 `ca-` 前缀去掉，那个恶性输入确实就会变成 `..` 本身。
        // ★ 没有这一条，上面那句 `slug != ".."` 就是一句永远为真的空话——
        //   而「一条永远为真的断言」与「没有断言」无法区分。
        assert_eq!(issuer_slug("https://../../etc/dir"), "ca-..");
        assert_eq!(&issuer_slug("https://../../etc/dir")["ca-".len()..], "..");
    }

    #[test]
    fn 目录名不会变成空的或只剩前缀() {
        // `https://` 后面什么都没有 —— 一个空目录名会让证书写到 issuer 根上。
        assert_eq!(issuer_slug("https://"), "ca-unknown");
        assert_eq!(issuer_slug("https:///dir"), "ca-unknown");
        assert_eq!(issuer_slug(""), "ca-unknown");
    }

    fn t(d: &str) -> Target {
        Target {
            domain: d.to_string(),
            site: "s".into(),
            dns01: None,
        }
    }

    #[test]
    fn 通配符判定只看前缀() {
        assert!(t("*.example.com").is_wildcard());
        assert!(!t("example.com").is_wildcard());
        // ⚠ `*` 出现在别处不算通配符——`a*.com` 根本不是合法域名，但也不该被判成通配符
        assert!(!t("a*.example.com").is_wildcard());
    }

    #[test]
    fn 挑战记录名要把通配符前缀剥掉() {
        // ★ ★ RFC 8555 §8.4：`*.example.com` 的挑战记录是
        //   `_acme-challenge.example.com`。写成 `_acme-challenge.*.example.com`
        //   甚至不是一个合法域名，而症状只是「CA 说验不过」。
        assert_eq!(
            t("*.example.com").challenge_record(),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            t("example.com").challenge_record(),
            "_acme-challenge.example.com"
        );
        // ★ 两者**故意相同**：给裸域和它的通配符各签一张时，用的是同一条记录名。
        //   这不是缺陷，是 ACME 的设计（同名可以并存多条 TXT）。
        assert_eq!(
            t("*.a.example.com").challenge_record(),
            "_acme-challenge.a.example.com"
        );
    }

    #[test]
    fn 没配_dns_时通配符接不了_而裸域接得了() {
        // ★ 这条是「推迟」与「失败」分野的源头：通配符没有 DNS-01 就是**这一批做不了**，
        //   不是**这一次没做成**。
        assert!(!t("*.example.com").actionable());
        assert!(t("example.com").actionable());
        assert!(!t("example.com").use_dns01(), "没配 dns 就走 HTTP-01");
    }

    #[test]
    fn 抖动落在_0_到_1_之间且不是常量() {
        let j = Jitter::with_seed(12345);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let u = j.unit();
            assert!((0.0..1.0).contains(&u), "抖动跑出 [0,1)：{u}");
            seen.insert(u.to_bits());
        }
        // ★ 判据不是「看起来随机」，是**它真的在变**：一个恒返回 0.5 的实现
        //   会让全部域名的退避完全同步，而那正是抖动要防的那件事。
        assert!(seen.len() > 50, "64 次只取到 {} 个不同的值", seen.len());
    }

    #[test]
    fn 同一个种子给同一串数() {
        // 反证要可重现，就必须能指定种子。
        let a = Jitter::with_seed(7);
        let b = Jitter::with_seed(7);
        for _ in 0..8 {
            assert_eq!(a.unit().to_bits(), b.unit().to_bits());
        }
    }

    #[test]
    fn 两代进程的种子不会撞在一起() {
        // ★ `from_clock` 里那个 `pid << 32` 不是装饰：只用时刻做种时，
        //   升级窗口里同一毫秒起来的两代会拿到同一串抖动 —— 等于没抖。
        let same_instant = 1_700_000_000_000_000_000u64;
        let gen1 = Jitter::with_seed(same_instant ^ (1234u64 << 32));
        let gen2 = Jitter::with_seed(same_instant ^ (5678u64 << 32));
        assert_ne!(gen1.unit().to_bits(), gen2.unit().to_bits());
    }
}
