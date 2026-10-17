//! 樞衡的 ACME 層（G53/G54/G56）：**自動簽發與續期**。
//!
//! ```text
//! fulcrum-tls   证书存储（G55）· 按 SNI 挑证书 · 续期判定（G56，纯函数）
//!      ▲
//!      │ 往里写
//! fulcrum-acme  账户 · 订单 · 挑战求解 · 什么时候该签
//! ```
//!
//! # ★ 這個 crate 不依賴 pingora
//!
//! 與 [`fulcrum_runtime`] 同一條紀律：它是「什麼時候該籤、怎麼籤」的**邏輯**，
//! 不是「掛在哪個處理程序模型上」。停機信號用 [`tokio::sync::watch::Receiver<bool>`] 接——
//! 那恰好就是 `pingora_core::server::ShutdownWatch` 的真身，
//! 所以 `fulcrum-server` 把它直接傳進來就行，兩邊都不必知道對方。
//!
//! 三種挑戰都已端到端可籤（門禁裡由 pebble 這個真 CA 驗過）：
//! **TLS-ALPN-01**（G54 的主）· **HTTP-01**（備）· **DNS-01 / 通配符**（G57/G58）。
//!
//! ★ **沒有端到端判據的 ACME 程式碼正是本儲存庫最不該寫的東西** —— 分批的判據一直是
//! 「哪一條現在就能被真跑一遍」，不是「哪一條更重要」。
//!
//! # ★ ★ 與 G55 怎麼對接
//!
//! 升級視窗裡兩代處理程序共享同一個證書目錄 ⇒ 簽發路徑**整條**握著 [`CertStore::lock`]
//! 那把跨處理程序檔案鎖：拿不到就等，等到了**先重讀一次儲存** —— 多半另一代已經簽好了。
//! 那不只是省一次請求：CA 的速率配額**按帳戶**算，兩代各簽一張會同時記賬。

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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Let's Encrypt 生產目錄。`acme_ca` 沒寫時用它。
pub const LETSENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt staging 目錄。
pub const LETSENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// 兩次巡檢之間最長等多久。★ 不是「多久續一次」——續期時刻由 [`fulcrum_tls::renewal`] 定，
/// 這裡只是「多久回來看一眼」的上限。
pub const MAX_IDLE: Duration = Duration::from_secs(12 * 3600);
/// 最短巡檢間隔。擋的是「剛失敗就立刻重來」把 CA 打成尖峰。
pub const MIN_IDLE: Duration = Duration::from_secs(30);

/// ACME 的全域設定，來自 DSL 的全域選項塊。
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// `acme_ca <目录 URL>`。
    pub directory_url: String,
    /// `acme_email <邮箱>`。★ 沒寫不是錯誤：ACME 不強制聯繫方式，
    /// 但**沒有它就收不到 CA 的到期與撤銷通知**，所以裝載時要說一聲。
    pub contact: Option<String>,
    /// 狀態目錄（G33）。證書在 `<state>/certs/`，帳戶在 `<state>/acme/`。
    pub state_dir: PathBuf,
}

impl AcmeConfig {
    /// 從全域選項造一份。`acme_ca` 空著就用 Let's Encrypt 生產目錄。
    pub fn new(acme_ca: Option<&str>, acme_email: Option<&str>, state_dir: &str) -> AcmeConfig {
        AcmeConfig {
            directory_url: acme_ca.unwrap_or(LETSENCRYPT_PRODUCTION).to_string(),
            contact: acme_email.map(|s| s.to_string()),
            state_dir: PathBuf::from(state_dir),
        }
    }

    /// 證書儲存的根：`<state>/certs`。
    pub fn cert_root(&self) -> PathBuf {
        self.state_dir.join("certs")
    }

    /// 帳戶儲存的根：`<state>/acme`。
    pub fn account_root(&self) -> PathBuf {
        self.state_dir.join("acme")
    }

    /// 這個 CA 在儲存裡佔哪一個目錄名。
    pub fn issuer(&self) -> String {
        issuer_slug(&self.directory_url)
    }
}

/// 一個 CA 的目錄 URL → 儲存裡的目錄名。
///
/// ★ ★ **這是一個有損映射，而且是故意的。** 可讀性在這裡是有價值的——G55 選
/// 「每域一目錄的 PEM」的全部理由就是**出事時人能直接進去看**，
/// 而 `ca-localhost_3a14000_2fdir` 這種可逆轉義把那點價值抵消掉了。
///
/// ⚠ 有損就意味著**兩個不同的 CA 可能落到同一個目錄名**（同主機不同路徑）。
/// 那個風險不是靠命名擋掉的，是靠 [`fulcrum_tls::Meta::issuer_url`] 記下真實來源、
/// 裝載時比對擋掉的——**判據掛在資料上，不掛在命名約定上**。
///
/// ★ 順帶：這也是與 `store::sanitize` 那條「必須單射」的**區別所在**。
/// 那邊單射是安全要求（兩個網域共用一張證書 = 安全缺陷），這邊不是：
/// 撞了的後果只是「發現來源對不上、重籤一張」。**同樣的問題在不同位置上答案不同。**
pub fn issuer_slug(directory_url: &str) -> String {
    match directory_url {
        LETSENCRYPT_PRODUCTION => return "letsencrypt".to_string(),
        LETSENCRYPT_STAGING => return "letsencrypt-staging".to_string(),
        _ => {}
    }
    // 摳出 host[:port]：去掉 scheme，取到第一個 `/` 為止。
    let rest = directory_url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(directory_url);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let mut out = String::from("ca-");
    for ch in hostport.chars() {
        // ★ 白名單而不是黑名單：路徑分隔符、`..`、控制字元、非 ASCII 一律不進目錄名。
        //   黑名單式的「把這幾個危險字元換掉」漏一個就是一次目錄穿越。
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    // 全是分隔符時別留下一個空目錄名。
    if out == "ca-" {
        out.push_str("unknown");
    }
    out
}

/// DNS-01 需要的兩半：**誰去改記錄**，與**怎麼確認它可見了**。
///
/// ★ ★ 兩半缺一不可，而第二半正是 G58 寫死的那條：
/// 「絕不能只 sleep 一個固定秒數」。所以這個結構裡沒有「等幾秒」這種欄位——
/// 只有一個會真去問權威 NS 的 [`TxtChecker`]。
#[derive(Debug)]
pub struct Dns01 {
    pub provider: DnsProvider,
    pub checker: TxtChecker,
}

/// 一個要維護證書的網域。
#[derive(Debug)]
pub struct Target {
    /// 證書上的網域。可能是 `*.example.com`。
    pub domain: String,
    /// 它是哪個站台要的 —— 只進紀錄，讓人知道該去設定的哪一行看。
    pub site: String,
    /// 這個站台配的 DNS-01。`None` = 沒配 `tls { dns … }`。
    ///
    /// ★ 掛在 target 上而不是全域：`dns` 與 `resolvers` 在 DSL 裡是**站台級**的，
    /// 不同站台完全可以託管在不同服務商。做成全域會在兩家託管商的場景下直接錯。
    pub dns01: Option<std::sync::Arc<Dns01>>,
}

impl Target {
    /// 通配符**只能**走 DNS-01（G54）——HTTP-01 與 TLS-ALPN-01 都證明不了「整個子域」。
    pub fn is_wildcard(&self) -> bool {
        self.domain.starts_with("*.")
    }

    /// 這一輪接得了嗎。
    ///
    /// ★ 判據是「**這個網域要用的那種挑戰，現在有沒有人接**」：
    /// 通配符必須有 DNS-01；非通配符沒配 DNS 就走 HTTP-01（已串接）。
    pub fn actionable(&self) -> bool {
        !self.is_wildcard() || self.dns01.is_some()
    }

    /// 走哪種挑戰。
    ///
    /// ★ **配了 `dns` 就走 DNS-01**，即使不是通配符——那是設定裡明寫的意圖
    /// （常見理由：這臺機器的 80 連接埠根本不對公用網路開）。沒配就走 HTTP-01。
    pub fn use_dns01(&self) -> bool {
        self.dns01.is_some()
    }

    /// DNS-01 要改的那條記錄叫什麼。
    ///
    /// ★ ★ **通配符要把 `*.` 剝掉**：`*.example.com` 的挑戰記錄是
    /// `_acme-challenge.example.com`，**不是** `_acme-challenge.*.example.com`
    /// （那甚至不是一個合法的網域）。RFC 8555 §8.4 就是這麼定義的。
    /// ⚠ 寫錯這一條的症狀是「CA 說驗不過」，而 DNS 那邊一切正常——
    /// 因為記錄確實寫上去了，只是寫在了一個沒人會查的名字上。
    pub fn challenge_record(&self) -> String {
        let base = self.domain.strip_prefix("*.").unwrap_or(&self.domain);
        format!("_acme-challenge.{base}")
    }
}

/// 一次巡檢的結果。★ 它是給紀錄與測試看的，不是給控制流看的。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 這一輪真的簽下來的網域。
    pub issued: Vec<String>,
    /// 儲存裡已經有、且還不到續期時刻的。
    pub fresh: Vec<String>,
    /// 這一批還接不了的（通配符要 DNS-01）。
    pub deferred: Vec<String>,
    /// 被退避壓住、這一輪**根本沒有去試**的。
    ///
    /// ★ ★ 它與 [`Report::failed`] 必須分開：「這一次又失敗了」要計數、要告警，
    /// 而「上次失敗之後還在等」是退避**正在正常工作**的樣子。
    /// 混在一起會讓一次失敗在監控裡變成每分鐘一條告警——而那恰好是退避要消滅的東西。
    pub backed_off: Vec<String>,
    /// 出錯的：`(域名, 原因)`。
    pub failed: Vec<(String, String)>,
    /// 下一次巡檢最晚不該遲於此。`None` = 沒有任何網域要盯著。
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

    /// 收一條「下次多久再看」的建議，取最緊的那條。
    pub fn note_next_check(&mut self, d: Duration) {
        self.next_check = Some(match self.next_check {
            None => d,
            Some(cur) => cur.min(d),
        });
    }

    /// 這一輪之後該睡多久。
    ///
    /// ★ 夾在 [`MIN_IDLE`] 與 [`MAX_IDLE`] 之間：下界擋「剛失敗就立刻重來」，
    /// 上界保證**即使所有判定都說不用管**，也仍然會定期回來看一眼——
    /// 一個「等到需要時才醒」的排程器，在時鐘跳變或判定寫錯時會**永遠睡下去**，
    /// 而那種失敗沒有任何症狀。
    pub fn sleep_for(&self) -> Duration {
        self.next_check
            .unwrap_or(MAX_IDLE)
            .clamp(MIN_IDLE, MAX_IDLE)
    }
}

/// 簽發嘗試的三個計數：`ok` / `fail` / `deferred`（**M2 批 M**）。
///
/// ★ ★ **它掛在 [`AcmeManager`] 上，由抓取時來問** —— `fulcrum_acme_issue_total`
/// 那一族不在指標那一側另記一份。能從被測物件本身問到的東西，就不要在旁邊再記一份：
/// 兩份遲早不一致，而不一致的那天沒有任何東西會說。
///
/// # ⚠ ⚠ `deferred` 在這裡的定義（寫死，別按字面猜）
///
/// **`deferred` = 這一輪「決定不去找 CA 試」，且原因是一條攔阻規則**，兩種：
///
/// | 來源 | 什麼形狀 |
/// |---|---|
/// | [`Report::backed_off`] | 上次失敗之後還在退避視窗裡，本輪根本沒去試 |
/// | [`Report::deferred`] | 這一批接不了（通配符站台沒配 `tls { dns … }`）|
///
/// ⛔ **不含 [`Report::fresh`]**（證書還新、沒到續期點）。那是**穩態**：
/// 每一輪、每一個健康網域都會落在那裡。把它算進來，這個計數器在一切正常時也單調上漲，
/// 於是「有東西被攔住了」這個信號就沒了 —— 與「一條在事實已經變了之後照舊輸出的警告
/// 會訓練人忽略整張表」是同一族的錯。
///
/// ⇒ 三個數**不構成對網域的劃分**（`ok + fail + deferred` ≠ 每輪的網域數），這是有意的。
#[derive(Debug, Default)]
pub struct IssueCounts {
    ok: AtomicU64,
    fail: AtomicU64,
    deferred: AtomicU64,
}

impl IssueCounts {
    /// 把一輪巡檢的結果記進來。**每輪一次，在 [`AcmeManager::run_once`] 收尾那一處。**
    fn record(&self, report: &Report) {
        let bump = |c: &AtomicU64, n: usize| {
            c.fetch_add(n as u64, Ordering::Relaxed);
        };
        bump(&self.ok, report.issued.len());
        bump(&self.fail, report.failed.len());
        bump(
            &self.deferred,
            report.backed_off.len() + report.deferred.len(),
        );
    }

    /// 唯讀快照：`(ok, fail, deferred)`。給指標繪製用。
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.ok.load(Ordering::Relaxed),
            self.fail.load(Ordering::Relaxed),
            self.deferred.load(Ordering::Relaxed),
        )
    }
}

/// 自動簽發與續期的執行者。
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
    /// **強制續期**佇列（G74）。管理面往裡放網域，巡檢取走。
    ///
    /// ★ 它不是「為測試造的旋鈕」：證書出事（金鑰洩露、CA 吊銷、SAN 要改）時，
    /// 維運本來就需要「立刻重籤一張」，而在它之前唯一的辦法是刪檔案再等巡檢。
    /// ⚠ 它也是 G58 那條「在真網域上續期一次」**唯一能在冒煙裡做到**的路——
    /// 真證書 90 天、ARI 視窗約 60 天后才開，等不到。
    /// ★ 值是 **要不要連退避一起清**（加的第二檔，見 `request_renew`）。
    force: std::sync::Mutex<std::collections::BTreeMap<String, bool>>,
    /// 叫醒巡檢。★ 沒有它，一次強制續期最長要等 12 小時（[`MAX_IDLE`]）才生效——
    /// 而「按了沒反應」會讓人以為這個口子壞了，然後去動別的東西。
    wake: tokio::sync::Notify,
    /// 簽發嘗試的三個計數（**M2 批 M**）。定義見 [`IssueCounts`]。
    issue_counts: IssueCounts,
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
            issue_counts: IssueCounts::default(),
        }
    }

    /// 簽發嘗試的三個計數。★ 唯讀，**抓取時來問**（`fulcrum_acme_issue_total`）。
    pub fn issue_counts(&self) -> &IssueCounts {
        &self.issue_counts
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

/// 抖動源。
///
/// ★ **不拉 `rand`**：這裡要的不是密碼學隨機，是「幾十個網域同時到期時別一起重試」。
/// 一個 splitmix64 就夠，而少一條依賴就少一次 G29 的每週檢查與一次供應鏈稽核。
/// ⚠ 而它**只用在退避時長上**——任何與金鑰、nonce、token 有關的隨機都由
/// `instant-acme` / `ring` 出，絕不由這裡出。
#[derive(Debug)]
pub struct Jitter {
    state: std::sync::atomic::AtomicU64,
}

impl Jitter {
    /// 用「時刻 + pid」做種。★ 加 pid 是必需的：升級視窗裡兩代處理程序**同時**起來，
    /// 只用時刻做種時它們很可能拿到同一個種子，於是抖動完全同步——那就等於沒抖。
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

    /// 取一個 `[0, 1)` 的數。
    pub fn unit(&self) -> f64 {
        use std::sync::atomic::Ordering;
        let mut z = self
            .state
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // 取高 53 位：f64 的尾數就是 53 位，用低位會在轉換時被丟掉。
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `deferred` 的定義（**M2 批 M**）：退避 + 這一批接不了，**不含**「還不到續期點」。
    #[test]
    fn 签发计数把退避与接不了算成_deferred_而不把_fresh_算进去() {
        let c = IssueCounts::default();
        c.record(&Report {
            issued: vec!["a".into()],
            // ★ ★ 反向那半：`fresh` 是**穩態** —— 每一輪每個健康網域都落在這裡。
            //   把它算進 `deferred`，這個計數器在一切正常時也單調上漲，
            //   於是「有東西被攔住了」這個信號就沒了。
            fresh: vec!["b".into(), "c".into(), "d".into()],
            deferred: vec!["e".into()],
            backed_off: vec!["f".into(), "g".into()],
            failed: vec![("h".into(), "why".into())],
            next_check: None,
        });
        assert_eq!(c.snapshot(), (1, 1, 3));

        // ★ 計數是**累加**的（counter 語義），不是每輪覆蓋。
        c.record(&Report {
            issued: vec!["a".into()],
            ..Report::default()
        });
        assert_eq!(c.snapshot(), (2, 1, 3));

        // ★ 空的一輪什麼都不動 —— 否則「巡檢跑過一輪」會被記成「嘗試過一次」。
        c.record(&Report::default());
        assert_eq!(c.snapshot(), (2, 1, 3));
    }

    #[test]
    fn 两家_lets_encrypt_有可读的目录名() {
        assert_eq!(issuer_slug(LETSENCRYPT_PRODUCTION), "letsencrypt");
        assert_eq!(issuer_slug(LETSENCRYPT_STAGING), "letsencrypt-staging");
    }

    #[test]
    fn 别的_ca_落到_ca_前缀且不含路径分隔符() {
        // ★ 判據不是「長得好看」，是**不含任何能穿出目錄的東西**。
        for url in [
            "https://localhost:14000/dir",
            "https://ca.internal/acme/directory",
            "http://10.0.0.1:8080/dir",
            // ⚠ 惡性輸入：路徑穿越、絕對路徑、NUL 之外的控制字元
            "https://../../etc/dir",
            "https://a/../../b/dir",
            "https://ca.example.com/../../../../root",
        ] {
            let slug = issuer_slug(url);
            assert!(slug.starts_with("ca-"), "{url} → {slug}");
            assert!(!slug.contains('/'), "{url} → {slug} 里有路径分隔符");
            assert!(!slug.contains('\\'), "{url} → {slug} 里有反斜杠");
            // ★ ★ 判據是**「這個目錄名本身能不能往上走」**，不是「它裡面有沒有出現兩個點」。
            //   ⚠ 寫成「按 `-` 切開之後不許有一段等於 `..`」是錯的：`ca-..` 會被判紅，
            //   而它是一個**字面值目錄名**，`join` 上去只得到一個叫 `ca-..` 的普通目錄。
            //   ★ **判據要照著「危險的定義」寫，不是照著「危險的樣子」寫** —— 真正危險的
            //   只有 `.` 與 `..` 這兩個名字本身，而 `ca-` 前綴已經讓它們不可能出現：
            //   **前綴在這裡是安全機制，不是裝飾**。
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
        // 反證：把 `ca-` 前綴去掉，那個惡性輸入確實就會變成 `..` 本身。
        // ★ 沒有這一條，上面那句 `slug != ".."` 就是一句永遠為真的空話——
        //   而「一條永遠為真的斷言」與「沒有斷言」無法區分。
        assert_eq!(issuer_slug("https://../../etc/dir"), "ca-..");
        assert_eq!(&issuer_slug("https://../../etc/dir")["ca-".len()..], "..");
    }

    #[test]
    fn 目录名不会变成空的或只剩前缀() {
        // `https://` 後面什麼都沒有 —— 一個空目錄名會讓證書寫到 issuer 根上。
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
        // ⚠ `*` 出現在別處不算通配符——`a*.com` 根本不是合法網域，但也不該被判成通配符
        assert!(!t("a*.example.com").is_wildcard());
    }

    #[test]
    fn 挑战记录名要把通配符前缀剥掉() {
        // ★ ★ RFC 8555 §8.4：`*.example.com` 的挑戰記錄是
        //   `_acme-challenge.example.com`。寫成 `_acme-challenge.*.example.com`
        //   甚至不是一個合法網域，而症狀只是「CA 說驗不過」。
        assert_eq!(
            t("*.example.com").challenge_record(),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            t("example.com").challenge_record(),
            "_acme-challenge.example.com"
        );
        // ★ 兩者**故意相同**：給裸域和它的通配符各簽一張時，用的是同一條記錄名。
        //   這不是缺陷，是 ACME 的設計（同名可以並存多條 TXT）。
        assert_eq!(
            t("*.a.example.com").challenge_record(),
            "_acme-challenge.a.example.com"
        );
    }

    #[test]
    fn 没配_dns_时通配符接不了_而裸域接得了() {
        // ★ 這條是「推遲」與「失敗」分野的源頭：通配符沒有 DNS-01 就是**這一批做不了**，
        //   不是**這一次沒做成**。
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
        // ★ 判據不是「看起來隨機」，是**它真的在變**：一個恆回傳 0.5 的實作
        //   會讓全部網域的退避完全同步，而那正是抖動要防的那件事。
        assert!(seen.len() > 50, "64 次只取到 {} 个不同的值", seen.len());
    }

    #[test]
    fn 同一个种子给同一串数() {
        // 反證要可重現，就必須能指定種子。
        let a = Jitter::with_seed(7);
        let b = Jitter::with_seed(7);
        for _ in 0..8 {
            assert_eq!(a.unit().to_bits(), b.unit().to_bits());
        }
    }

    #[test]
    fn 两代进程的种子不会撞在一起() {
        // ★ `from_clock` 裡那個 `pid << 32` 不是裝飾：只用時刻做種時，
        //   升級視窗裡同一毫秒起來的兩代會拿到同一串抖動 —— 等於沒抖。
        let same_instant = 1_700_000_000_000_000_000u64;
        let gen1 = Jitter::with_seed(same_instant ^ (1234u64 << 32));
        let gen2 = Jitter::with_seed(same_instant ^ (5678u64 << 32));
        assert_ne!(gen1.unit().to_bits(), gen2.unit().to_bits());
    }
}
