//! 證書儲存（G55）：`<state>/certs/<issuer>/<domain>/{cert.pem, key.pem, meta.json}`。
//!
//! ★ 選「每域一目錄的 PEM」而不是某種自訂容器格式，理由是**出事時能手動救**：
//! 人可以直接 `openssl x509 -in cert.pem -text` 看，可以直接 `cp` 備份，
//! 可以直接把買來的證書放進去。
//!
//! ★ 未選「相容 Caddy 的目錄佈局」：那等於把別人的內部佈局當成契約，他們改了我們就碎。
//!
//! # ★ ★ 兩條實作約束不是潔癖，是 M0 的直接後果
//!
//! **升級視窗內兩代處理程序共享同一個證書目錄**（M0 已證兩代並存），
//! 而 On-Demand 簽發是**執行期**行為——兩代可能同時寫同一個網域。所以：
//!
//! 1. 寫入一律**寫暫存檔再 `rename`**（同目錄內原子替換）。半個檔案被另一代讀到，
//!    表現是「證書解析失敗」，而它只在升級視窗裡出現，幾乎不可能被複現。
//! 2. 跨處理程序用 **`flock`** 串行化同一網域的簽發。否則兩代各簽一張、互相覆蓋，
//!    **還各自消耗 CA 的速率配額**——而速率配額用完之後，簽不出來的不只是這一個網域。

use crate::renewal::RenewalState;
use log::debug;
use pingora_boringssl::pkey::{PKey, Private};
use pingora_boringssl::x509::X509;
use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `meta.json` 的內容。
///
/// ★ 它記的是**證書裡沒有的東西**：ARI 給的視窗、續期失敗計數、上次嘗試時刻。
/// 有效期與 SAN 不進來——那些從證書本身讀，**兩份會分叉**。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// 簽發者目錄名（如 `letsencrypt`）。
    pub issuer: String,
    /// ★ ★ 簽發它的那個 **ACME 目錄 URL 原文**。
    ///
    /// 目錄名是可讀性優先的有損映射（`ca-<host>`），兩個不同的 CA **可能落到同一個目錄名**。
    /// 與其去發明一套「保證不撞」的命名，不如把真實來源記下來：裝載時比對，
    /// 對不上就當成外來證書重簽，並說出來。
    /// ★ 這是本儲存庫那條紀律的一次應用——**判據掛在資料上，不掛在命名約定上**。
    /// `None` = 這張證書不是本處理程序簽的（手動放進來的、或舊版本寫的）。
    #[serde(default)]
    pub issuer_url: Option<String>,
    /// ARI 建議的續期視窗（Unix 秒）。
    pub ari_start: Option<u64>,
    pub ari_end: Option<u64>,
    #[serde(default)]
    pub renewal: RenewalState,
    /// 上一次**失敗**的那次簽發用的是哪種挑戰（`"tls-alpn-01"` / `"http-01"`）。
    ///
    /// ★ ★ 這一格就是 G54 那句「HTTP-01 是**備**」的落點。沒有它，「主/備」只是措辭：
    /// 一個永遠只試 TLS-ALPN-01 的實作，在 443 被擋住的機器上會**永遠簽不出來**，
    /// 而紀錄裡每一輪都只說「驗不過」。
    /// 成功即清空 —— 否則一次偶發失敗會把這個網域永久釘在備用挑戰上。
    #[serde(default)]
    pub last_challenge_failed: Option<String>,
}

/// 一張讀出來的證書。
pub struct StoredCert {
    /// 證書鏈，**leaf 在第一個**（PEM 檔案裡的順序）。
    ///
    /// ⚠ ⚠ **（G104）：類型從 rustls 的 `CertificateDer` 換成 BoringSSL 的 `X509`。**
    /// ★ 換的只是載體，**不是語義** —— 有效期與 SAN 仍然由 `validity_of()` 用
    /// `x509-parser` 從 DER 裡讀，逐字未改。那一段是被 16 條特徵化測試釘著的，
    /// 換實作等於把「我們對證書的理解」也一起換掉，而這一批換的是 TLS 後端。
    pub chain: Vec<X509>,
    pub key: PKey<Private>,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
    /// 證書裡的 SAN（DNS 名），**不是**目錄名。
    pub domains: Vec<String>,
    pub meta: Meta,
}

pub struct CertStore {
    root: PathBuf,
}

/// `flock` 守衛。★ 鎖在 `Drop` 裡釋放（關閉 fd 即釋放），所以
/// 「簽發路徑上任何一處提前回傳都會漏鎖」這件事無從發生。
pub struct DomainLock {
    file: fs::File,
}

impl Drop for DomainLock {
    fn drop(&mut self) {
        // SAFETY: fd 來自一個仍然活著的 File；LOCK_UN 對任意 fd 都是安全呼叫。
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl CertStore {
    pub fn new(root: impl Into<PathBuf>) -> CertStore {
        CertStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir_for(&self, issuer: &str, domain: &str) -> PathBuf {
        self.root.join(issuer).join(sanitize(domain))
    }

    /// 拿住某個網域的簽發鎖。**阻塞**直到拿到。
    ///
    /// ⚠ 用阻塞而不是 `LOCK_NB`：拿不到鎖說明**另一代處理程序正在給同一個網域簽發**，
    /// 那時正確的行為是等它簽完然後用它的結果，而不是自己也去簽一張。
    pub fn lock(&self, issuer: &str, domain: &str) -> io::Result<DomainLock> {
        let dir = self.root.join("locks");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}__{}.lock", sanitize(issuer), sanitize(domain)));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        // SAFETY: fd 來自上面剛打開的 File，在本作用域內一直活著。
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        debug!("拿到签发锁 {}", path.display());
        Ok(DomainLock { file })
    }

    /// 唯讀 `meta.json`，**不要求證書存在**。讀不到或讀壞了都給一份預設值。
    ///
    /// ★ ★ **「證書還沒簽下來」與「這個網域的續期狀態」是兩件事**，而
    /// [`CertStore::load`] 把它們綁在了一起：缺 `cert.pem` 就回傳 `None`，
    /// 於是連 `meta.json` 裡那份**失敗計數**也一起讀不到了。
    /// ⚠ 後果是退避在「從來沒簽成過」的網域上完全失效——每一輪都從「零次失敗」重新開始。
    /// 這條是 ACME 門禁的反證跑抓到的：連著三輪紀錄都寫「連續第 1 次失敗」。
    pub fn load_meta(&self, issuer: &str, domain: &str) -> Meta {
        let dir = self.dir_for(issuer, domain);
        let fallback = || Meta {
            issuer: issuer.to_string(),
            ..Default::default()
        };
        match fs::read_to_string(dir.join("meta.json")) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                // ⚠ meta 壞了**不該讓證書不可用**：它只是續期狀態，
                //   而證書本身還好著。用預設值繼續，並說出來。
                log::warn!(
                    "{} 的 meta.json 读不动（{e}），按默认续期状态处理",
                    dir.display()
                );
                fallback()
            }),
            Err(_) => fallback(),
        }
    }

    /// 讀一張證書。缺檔案回傳 `Ok(None)`；檔案在但壞了回傳 `Err`。
    ///
    /// ★ 這兩種情況必須分開：「還沒簽」是正常狀態，「簽了但讀不出來」是要人看的問題。
    /// 把後者也當成 `None` 會讓一張壞證書表現為「反覆重新簽發」。
    pub fn load(&self, issuer: &str, domain: &str) -> io::Result<Option<StoredCert>> {
        let dir = self.dir_for(issuer, domain);
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        if !cert_path.exists() || !key_path.exists() {
            return Ok(None);
        }
        // ⚠ ⚠ **G104 之前這裡調的是 `pingora_rustls::load_certs_and_key_files`。**
        //   換成 BoringSSL 之後沒有等價的一站式函式，於是兩個檔案各讀各的 ——
        //   ★ 而「證書為空」與「私鑰為空」這兩種情況仍然分開報，
        //   因為它們指向的維運動作不同（前者是簽發沒成功，後者多半是檔案被截斷）。
        let cert_pem = fs::read(&cert_path)?;
        let key_pem = fs::read(&key_path)?;
        let chain = X509::stack_from_pem(&cert_pem)
            .map_err(|e| io::Error::other(format!("{} 的证书解析失败：{e}", dir.display())))?;
        if chain.is_empty() {
            return Err(io::Error::other(format!(
                "{} 里一张证书都没有",
                dir.display()
            )));
        }
        let key = PKey::private_key_from_pem(&key_pem)
            .map_err(|e| io::Error::other(format!("{} 的私钥解析失败：{e}", dir.display())))?;
        let first = chain.first().ok_or_else(|| {
            io::Error::other(format!("{} 的证书链里一张证书都没有", dir.display()))
        })?;
        let (not_before, not_after, domains) = validity_of(first)
            .map_err(|e| io::Error::other(format!("{} 的证书读不出有效期：{e}", dir.display())))?;

        let meta = self.load_meta(issuer, domain);

        Ok(Some(StoredCert {
            chain,
            key,
            not_before,
            not_after,
            domains,
            meta,
        }))
    }

    /// 原子地寫一張證書。
    ///
    /// ★ 三件事的順序是有講究的：**先寫私鑰、再寫證書、最後寫 meta**。
    /// 中途崩潰時留下的組合分別是「只有私鑰」（`load` 判缺證書 → `None`，會重簽）、
    /// 「私鑰 + 證書」（可用，meta 走預設值）——兩種都不會讓服務拿到一張對不上的證書。
    /// 反過來先寫證書，崩潰就可能留下「證書 + 舊私鑰」，**而那是能裝載卻握手失敗的**。
    pub fn save(
        &self,
        issuer: &str,
        domain: &str,
        cert_pem: &str,
        key_pem: &str,
        meta: &Meta,
    ) -> io::Result<()> {
        let dir = self.dir_for(issuer, domain);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("key.pem"), key_pem.as_bytes(), 0o600)?;
        atomic_write(&dir.join("cert.pem"), cert_pem.as_bytes(), 0o644)?;
        let meta_s = serde_json::to_string_pretty(meta).map_err(io::Error::other)?;
        atomic_write(&dir.join("meta.json"), meta_s.as_bytes(), 0o644)?;
        Ok(())
    }

    /// 只更新續期狀態，不動證書。
    pub fn save_meta(&self, issuer: &str, domain: &str, meta: &Meta) -> io::Result<()> {
        let dir = self.dir_for(issuer, domain);
        fs::create_dir_all(&dir)?;
        let s = serde_json::to_string_pretty(meta).map_err(io::Error::other)?;
        atomic_write(&dir.join("meta.json"), s.as_bytes(), 0o644)
    }

    /// 列出某個簽發者下已存的網域目錄名（**已做過檔案名轉義**）。
    pub fn list(&self, issuer: &str) -> io::Result<Vec<String>> {
        let dir = self.root.join(issuer);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            if e.path().is_dir()
                && let Some(name) = e.file_name().to_str()
            {
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }
}

/// 寫暫存檔再 `rename`。**同目錄內**，否則 `rename` 不是原子的（跨設備會退化成拷貝）。
///
/// ★ 公開出去是給 `fulcrum-acme` 寫帳戶憑證用的：那也是一份**私鑰**，
/// 也要「權限在 create 時給」「寫完再改名」。同一條規則有兩份實作，遲早會分叉一份。
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径没有父目录"))?;
    // ★ 臨時名帶 pid：升級視窗裡兩代同時寫時，各寫各的暫存檔，
    //   最後誰先 rename 誰的結果就是最終態——而任何一個都是完整的。
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        use std::io::Write as _;
        // ★ 權限在 **create 時**給，不是寫完再 chmod：後者留下一個
        //   「內容已經在裡面、權限還是 0644」的視窗，而私鑰在那個視窗裡是可讀的。
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        // ⚠ 不 fsync 就 rename，崩潰時可能得到一個「已改名但內容是空的」檔案。
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 把網域變成一個安全的目錄名。
///
/// ⚠ ⚠ **必須是單射的**：兩個不同的網域撞到同一個目錄，就會共用一張證書——
/// 那是**安全缺陷**，不是顯示問題。
///
/// 編碼兩步：先 `_` → `__`，再 `*` → `_wildcard_`。
///
/// ⚠ ⚠ **兩步缺一不可。** 只做第二步的話，`_wildcard_.example.com`
/// 與 `*.example.com` 會**撞到同一個目錄** —— DNS 名裡的下劃線很常見
/// （`_acme-challenge.`、SRV 記錄），「不可能出現」那句話是錯的。
///
/// ★ 證明方式不是「幾個樣本沒撞」，而是 [`unsanitize`] 能把它**逐字還原** ——
/// 存在反函式就等於單射。
pub fn sanitize(domain: &str) -> String {
    domain.replace('_', "__").replace('*', "_wildcard_")
}

/// [`sanitize`] 的反函式。
///
/// ⚠ 解碼必須**從左到右掃**，而且 `__` 要先於 `_wildcard_` 判。
/// 直接 `replace("_wildcard_", "*")` 會把 `__wildcard__`（即字面值 `_wildcard_`）
/// 裡從第 2 個字元開始的那一段也認成通配標記 —— 那時反函式就不成立了。
pub fn unsanitize(name: &str) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while i < name.len() {
        let rest = &name[i..];
        if let Some(r) = rest.strip_prefix("__") {
            out.push('_');
            i = name.len() - r.len();
            continue;
        }
        if let Some(r) = rest.strip_prefix("_wildcard_") {
            out.push('*');
            i = name.len() - r.len();
            continue;
        }
        let ch = rest.chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 從一張證書裡讀有效期與 DNS SAN。
///
/// ★ ★ **G104 之後它收的是 `X509`（BoringSSL），而裡面仍然走 `x509-parser` 讀 DER。**
/// ⚠ 這不是沒順手改乾淨 —— BoringSSL 自己也能讀這兩樣，而**換掉它等於同時換掉
/// 「我們對證書的理解」**：SAN 的取法、時間的邊界、非 DNS 類型 SAN 怎麼處理，
/// 這些今天由一組測試釘著，換實作它們就要重新證一遍。
/// ⇒ **這一批換的是 TLS 後端，不是證書解析。** 兩件事分開做，紅了才分得清是哪一件。
pub fn validity_of(cert: &X509) -> Result<(SystemTime, SystemTime, Vec<String>), String> {
    use x509_parser::prelude::*;
    let der = cert.to_der().map_err(|e| e.to_string())?;
    let (_, cert) = X509Certificate::from_der(&der).map_err(|e| e.to_string())?;
    let nb = ts(cert.validity().not_before.timestamp());
    let na = ts(cert.validity().not_after.timestamp());
    let mut domains = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for gn in &san.value.general_names {
            if let GeneralName::DNSName(d) = gn {
                domains.push(d.to_string());
            }
        }
    }
    Ok((nb, na, domains))
}

fn ts(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs((-secs) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fulcrum-store-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// 會撞的與不會撞的都要覆蓋到。★ 名單裡那幾個「看起來不像網域」的是重點。
    const NASTY: &[&str] = &[
        "example.com",
        "*.example.com",
        "a.example.com",
        "*.a.example.com",
        // ⚠ 下面這些都是**合法的 DNS 名**，而一個不完整的轉義會讓它們與上面幾個相撞
        "_wildcard_.example.com",
        "__wildcard__.example.com",
        "_acme-challenge.example.com",
        "_.example.com",
        "___.example.com",
        "*._wildcard_.example.com",
    ];

    #[test]
    fn 域名转义是单射的() {
        // ★ ★ 判據不是「這幾個樣本沒撞」，而是**存在反函式**：能逐字還原 ⇒ 一定單射。
        for n in NASTY {
            assert_eq!(
                unsanitize(&sanitize(n)),
                *n,
                "{n} 转义之后还不回来了（转义结果是 {}）",
                sanitize(n)
            );
        }
        // 順帶把「不撞」也直接驗一遍，免得反函式寫錯時兩條同時錯。
        let mut seen = std::collections::BTreeSet::new();
        for n in NASTY {
            assert!(
                seen.insert(sanitize(n)),
                "{n} 的转义与别人撞了：{}",
                sanitize(n)
            );
        }
        assert_eq!(sanitize("*.example.com"), "_wildcard_.example.com");
        assert_eq!(sanitize("example.com"), "example.com");
    }

    #[test]
    fn 通配符与字面量_wildcard_不得撞目录() {
        // ⚠ 反向判據：這兩個網域不同，目錄必須不同。
        assert_ne!(
            sanitize("*.example.com"),
            sanitize("_wildcard_.example.com"),
            "通配符与一个字面量叫 _wildcard_ 的域名撞到了同一个目录 —— 那意味着两个域名共用一张证书"
        );
    }

    #[test]
    fn 私钥的权限在创建时就是_0600() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmpdir();
        let store = CertStore::new(&root);
        store
            .save("test", "a.com", "CERT", "KEY", &Meta::default())
            .unwrap();
        let key = store.dir_for("test", "a.com").join("key.pem");
        let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "私钥权限是 {mode:o}");
        let cert = store.dir_for("test", "a.com").join("cert.pem");
        let cmode = fs::metadata(&cert).unwrap().permissions().mode() & 0o777;
        assert_eq!(cmode, 0o644);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 写完之后没有临时文件残留() {
        let root = tmpdir();
        let store = CertStore::new(&root);
        store
            .save("test", "a.com", "CERT", "KEY", &Meta::default())
            .unwrap();
        let dir = store.dir_for("test", "a.com");
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "有临时文件残留：{leftovers:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 缺文件是_none_而坏文件是_err() {
        // ★ 這兩種必須分開：「還沒簽」是正常狀態，「簽了但讀不出來」要人看。
        //   混在一起會讓一張壞證書表現為「反覆重新簽發」。
        let root = tmpdir();
        let store = CertStore::new(&root);
        assert!(store.load("test", "nope.com").unwrap().is_none());

        store
            .save(
                "test",
                "bad.com",
                "not a pem at all",
                "neither",
                &Meta::default(),
            )
            .unwrap();
        let e = store.load("test", "bad.com");
        assert!(e.is_err(), "坏 PEM 应当报错而不是当成没有");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn meta_坏了不该让证书不可用() {
        let root = tmpdir();
        let store = CertStore::new(&root);
        let dir = store.dir_for("test", "a.com");
        fs::create_dir_all(&dir).unwrap();
        // 沒有真證書，所以這裡只驗 meta 的容錯路徑不 panic
        fs::write(dir.join("meta.json"), "{ not json").unwrap();
        // 缺 cert/key → None（而不是因為 meta 壞了就 Err）
        assert!(store.load("test", "a.com").unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 同一进程里两次上锁不会自我死锁() {
        // ⚠ `flock` 是**按 fd** 的，同一處理程序用兩個不同的 fd 會真的互斥。
        //   所以這裡驗的是：拿完釋放之後還能再拿到，而不是「可重入」。
        let root = tmpdir();
        let store = CertStore::new(&root);
        {
            let _g = store.lock("test", "a.com").unwrap();
        }
        let _g2 = store.lock("test", "a.com").unwrap();
        // 不同網域互不影響
        let _g3 = store.lock("test", "b.com").unwrap();
        drop(_g2);
        drop(_g3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn list_只列目录并且排序() {
        let root = tmpdir();
        let store = CertStore::new(&root);
        for d in ["b.com", "a.com", "*.c.com"] {
            store.save("test", d, "C", "K", &Meta::default()).unwrap();
        }
        fs::write(root.join("test").join("stray-file"), "x").unwrap();
        let got = store.list("test").unwrap();
        assert_eq!(got, vec!["_wildcard_.c.com", "a.com", "b.com"]);
        assert!(store.list("nonexistent-issuer").unwrap().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn meta_能往返() {
        let root = tmpdir();
        let store = CertStore::new(&root);
        let meta = Meta {
            issuer: "letsencrypt".into(),
            issuer_url: Some("https://acme-v02.api.letsencrypt.org/directory".into()),
            ari_start: Some(111),
            ari_end: Some(222),
            renewal: RenewalState {
                failures: 3,
                last_attempt: Some(999),
            },
            last_challenge_failed: Some("tls-alpn-01".into()),
        };
        store.save("letsencrypt", "a.com", "C", "K", &meta).unwrap();
        let raw =
            fs::read_to_string(store.dir_for("letsencrypt", "a.com").join("meta.json")).unwrap();
        let back: Meta = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, meta);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 没有证书时失败计数也要读得回来() {
        // ★ ★ 守的是那個缺陷的另一半：`load()` 缺 `cert.pem` 就回傳 `None`，
        //   於是**從來沒簽成過**的網域連自己的失敗計數都讀不回來，
        //   每一輪都從「零次失敗」重新開始 —— 退避永遠長不大。
        let root = tmpdir();
        let store = CertStore::new(&root);
        let meta = Meta {
            issuer: "letsencrypt".into(),
            renewal: RenewalState {
                failures: 4,
                last_attempt: Some(1234),
            },
            ..Default::default()
        };
        // 只寫 meta，**不寫證書** —— 正是一個簽了四次都沒簽下來的網域的樣子。
        store.save_meta("letsencrypt", "never.com", &meta).unwrap();
        assert!(
            store.load("letsencrypt", "never.com").unwrap().is_none(),
            "没有证书，load() 本来就该给 None"
        );
        // ★ 而續期狀態必須還在。
        let back = store.load_meta("letsencrypt", "never.com");
        assert_eq!(back.renewal.failures, 4);
        assert_eq!(back.renewal.last_attempt, Some(1234));
        // ★ 反向：一個**根本沒記錄過**的網域要拿到乾淨的預設值，
        //   而不是繼承上一個網域的計數（否則新網域一上來就被退避擋住）。
        let none = store.load_meta("letsencrypt", "fresh.com");
        assert_eq!(none.renewal.failures, 0);
        assert_eq!(none.renewal.last_attempt, None);
        assert_eq!(none.issuer, "letsencrypt");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 老的_meta_没有_issuer_url_也读得出来() {
        // ⚠ 升級要向後相容：批 4 之前寫下的 `meta.json` 裡沒有 `issuer_url` 這個鍵。
        //   讀不出來的後果不是「少一個欄位」，而是**整張證書被當成壞的**（`load` 回傳 Err），
        //   於是一次升級會讓所有自動簽發的站台同時握手失敗。
        let old = r#"{"issuer":"letsencrypt","ari_start":null,"ari_end":null,
                      "renewal":{"failures":0,"last_attempt":null}}"#;
        let back: Meta = serde_json::from_str(old).expect("老格式的 meta 必须还读得出来");
        assert_eq!(back.issuer, "letsencrypt");
        // ★ `None` 的語義是「不知道是誰簽的」，而不是「不是我們簽的」——
        //   後者會讓升級之後每個網域都被重簽一遍。
        assert_eq!(back.issuer_url, None);
    }
}
