//! 证书存储（G55）：`<state>/certs/<issuer>/<domain>/{cert.pem, key.pem, meta.json}`。
//!
//! ★ 选「每域一目录的 PEM」而不是某种自定义容器格式，理由是**出事时能手工救**：
//! 人可以直接 `openssl x509 -in cert.pem -text` 看，可以直接 `cp` 备份，
//! 可以直接把买来的证书放进去。
//!
//! ★ 未选「兼容 Caddy 的目录布局」：那等于把别人的内部布局当成契约，他们改了我们就碎。
//!
//! # ★ ★ 两条实现约束不是洁癖，是 M0 的直接后果
//!
//! **升级窗口内两代进程共享同一个证书目录**（M0 已证两代并存），
//! 而 On-Demand 签发是**运行期**行为——两代可能同时写同一个域名。所以：
//!
//! 1. 写入一律**写临时文件再 `rename`**（同目录内原子替换）。半个文件被另一代读到，
//!    表现是「证书解析失败」，而它只在升级窗口里出现，几乎不可能被复现。
//! 2. 跨进程用 **`flock`** 串行化同一域名的签发。否则两代各签一张、互相覆盖，
//!    **还各自消耗 CA 的速率配额**——而速率配额用完之后，签不出来的不只是这一个域名。

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

/// `meta.json` 的内容。
///
/// ★ 它记的是**证书里没有的东西**：ARI 给的窗口、续期失败计数、上次尝试时刻。
/// 有效期与 SAN 不进来——那些从证书本身读，**两份会分叉**。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// 签发者目录名（如 `letsencrypt`）。
    pub issuer: String,
    /// ★ ★ 签发它的那个 **ACME 目录 URL 原文**。
    ///
    /// 目录名是可读性优先的有损映射（`ca-<host>`），两个不同的 CA **可能落到同一个目录名**。
    /// 与其去发明一套「保证不撞」的命名，不如把真实来源记下来：装载时比对，
    /// 对不上就当成外来证书重签，并说出来。
    /// ★ 这是本仓库那条纪律的一次应用——**判据挂在数据上，不挂在命名约定上**。
    /// `None` = 这张证书不是本程序签的（手工放进来的、或旧版本写的）。
    #[serde(default)]
    pub issuer_url: Option<String>,
    /// ARI 建议的续期窗口（Unix 秒）。
    pub ari_start: Option<u64>,
    pub ari_end: Option<u64>,
    #[serde(default)]
    pub renewal: RenewalState,
    /// 上一次**失败**的那次签发用的是哪种挑战（`"tls-alpn-01"` / `"http-01"`）。
    ///
    /// ★ ★ 这一格就是 G54 那句「HTTP-01 是**备**」的落点。没有它，「主/备」只是措辞：
    /// 一个永远只试 TLS-ALPN-01 的实现，在 443 被挡住的机器上会**永远签不出来**，
    /// 而日志里每一轮都只说「验不过」。
    /// 成功即清空 —— 否则一次偶发失败会把这个域名永久钉在备用挑战上。
    #[serde(default)]
    pub last_challenge_failed: Option<String>,
}

/// 一张读出来的证书。
pub struct StoredCert {
    /// 证书链，**leaf 在第一个**（PEM 文件里的顺序）。
    ///
    /// ⚠ ⚠ **（G104）：类型从 rustls 的 `CertificateDer` 换成 BoringSSL 的 `X509`。**
    /// ★ 换的只是载体，**不是语义** —— 有效期与 SAN 仍然由 `validity_of()` 用
    /// `x509-parser` 从 DER 里读，逐字未改。那一段是被 16 条特征化测试钉着的，
    /// 换实现等于把「我们对证书的理解」也一起换掉，而这一批换的是 TLS 后端。
    pub chain: Vec<X509>,
    pub key: PKey<Private>,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
    /// 证书里的 SAN（DNS 名），**不是**目录名。
    pub domains: Vec<String>,
    pub meta: Meta,
}

pub struct CertStore {
    root: PathBuf,
}

/// `flock` 守卫。★ 锁在 `Drop` 里释放（关闭 fd 即释放），所以
/// 「签发路径上任何一处提前返回都会漏锁」这件事无从发生。
pub struct DomainLock {
    file: fs::File,
}

impl Drop for DomainLock {
    fn drop(&mut self) {
        // SAFETY: fd 来自一个仍然活着的 File；LOCK_UN 对任意 fd 都是安全调用。
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

    /// 拿住某个域名的签发锁。**阻塞**直到拿到。
    ///
    /// ⚠ 用阻塞而不是 `LOCK_NB`：拿不到锁说明**另一代进程正在给同一个域名签发**，
    /// 那时正确的行为是等它签完然后用它的结果，而不是自己也去签一张。
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
        // SAFETY: fd 来自上面刚打开的 File，在本作用域内一直活着。
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        debug!("拿到签发锁 {}", path.display());
        Ok(DomainLock { file })
    }

    /// 只读 `meta.json`，**不要求证书存在**。读不到或读坏了都给一份默认值。
    ///
    /// ★ ★ **「证书还没签下来」与「这个域名的续期状态」是两件事**，而
    /// [`CertStore::load`] 把它们绑在了一起：缺 `cert.pem` 就返回 `None`，
    /// 于是连 `meta.json` 里那份**失败计数**也一起读不到了。
    /// ⚠ 后果是退避在「从来没签成过」的域名上完全失效——每一轮都从「零次失败」重新开始。
    /// 这条是 ACME 门禁的反证跑抓到的：连着三轮日志都写「连续第 1 次失败」。
    pub fn load_meta(&self, issuer: &str, domain: &str) -> Meta {
        let dir = self.dir_for(issuer, domain);
        let fallback = || Meta {
            issuer: issuer.to_string(),
            ..Default::default()
        };
        match fs::read_to_string(dir.join("meta.json")) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                // ⚠ meta 坏了**不该让证书不可用**：它只是续期状态，
                //   而证书本身还好着。用默认值继续，并说出来。
                log::warn!(
                    "{} 的 meta.json 读不动（{e}），按默认续期状态处理",
                    dir.display()
                );
                fallback()
            }),
            Err(_) => fallback(),
        }
    }

    /// 读一张证书。缺文件返回 `Ok(None)`；文件在但坏了返回 `Err`。
    ///
    /// ★ 这两种情况必须分开：「还没签」是正常状态，「签了但读不出来」是要人看的问题。
    /// 把后者也当成 `None` 会让一张坏证书表现为「反复重新签发」。
    pub fn load(&self, issuer: &str, domain: &str) -> io::Result<Option<StoredCert>> {
        let dir = self.dir_for(issuer, domain);
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        if !cert_path.exists() || !key_path.exists() {
            return Ok(None);
        }
        // ⚠ ⚠ **G104 之前这里调的是 `pingora_rustls::load_certs_and_key_files`。**
        //   换成 BoringSSL 之后没有等价的一站式函数，于是两个文件各读各的 ——
        //   ★ 而「证书为空」与「私钥为空」这两种情况仍然分开报，
        //   因为它们指向的运维动作不同（前者是签发没成功，后者多半是文件被截断）。
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

    /// 原子地写一张证书。
    ///
    /// ★ 三件事的顺序是有讲究的：**先写私钥、再写证书、最后写 meta**。
    /// 中途崩溃时留下的组合分别是「只有私钥」（`load` 判缺证书 → `None`，会重签）、
    /// 「私钥 + 证书」（可用，meta 走默认值）——两种都不会让服务拿到一张对不上的证书。
    /// 反过来先写证书，崩溃就可能留下「证书 + 旧私钥」，**而那是能装载却握手失败的**。
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

    /// 只更新续期状态，不动证书。
    pub fn save_meta(&self, issuer: &str, domain: &str, meta: &Meta) -> io::Result<()> {
        let dir = self.dir_for(issuer, domain);
        fs::create_dir_all(&dir)?;
        let s = serde_json::to_string_pretty(meta).map_err(io::Error::other)?;
        atomic_write(&dir.join("meta.json"), s.as_bytes(), 0o644)
    }

    /// 列出某个签发者下已存的域名目录名（**已做过文件名转义**）。
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

/// 写临时文件再 `rename`。**同目录内**，否则 `rename` 不是原子的（跨设备会退化成拷贝）。
///
/// ★ 公开出去是给 `fulcrum-acme` 写账户凭据用的：那也是一份**私钥**，
/// 也要「权限在 create 时给」「写完再改名」。同一条规则有两份实现，迟早会分叉一份。
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径没有父目录"))?;
    // ★ 临时名带 pid：升级窗口里两代同时写时，各写各的临时文件，
    //   最后谁先 rename 谁的结果就是最终态——而任何一个都是完整的。
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        use std::io::Write as _;
        // ★ 权限在 **create 时**给，不是写完再 chmod：后者留下一个
        //   「内容已经在里面、权限还是 0644」的窗口，而私钥在那个窗口里是可读的。
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        // ⚠ 不 fsync 就 rename，崩溃时可能得到一个「已改名但内容是空的」文件。
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 把域名变成一个安全的目录名。
///
/// ⚠ ⚠ **必须是单射的**：两个不同的域名撞到同一个目录，就会共用一张证书——
/// 那是**安全缺陷**，不是显示问题。
///
/// 编码两步：先 `_` → `__`，再 `*` → `_wildcard_`。
///
/// ⚠ ⚠ **两步缺一不可。** 只做第二步的话，`_wildcard_.example.com`
/// 与 `*.example.com` 会**撞到同一个目录** —— DNS 名里的下划线很常见
/// （`_acme-challenge.`、SRV 记录），「不可能出现」那句话是错的。
///
/// ★ 证明方式不是「几个样本没撞」，而是 [`unsanitize`] 能把它**逐字还原** ——
/// 存在反函数就等于单射。
pub fn sanitize(domain: &str) -> String {
    domain.replace('_', "__").replace('*', "_wildcard_")
}

/// [`sanitize`] 的反函数。
///
/// ⚠ 解码必须**从左到右扫**，而且 `__` 要先于 `_wildcard_` 判。
/// 直接 `replace("_wildcard_", "*")` 会把 `__wildcard__`（即字面量 `_wildcard_`）
/// 里从第 2 个字符开始的那一段也认成通配标记 —— 那时反函数就不成立了。
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

/// 从一张证书里读有效期与 DNS SAN。
///
/// ★ ★ **G104 之后它收的是 `X509`（BoringSSL），而里面仍然走 `x509-parser` 读 DER。**
/// ⚠ 这不是没顺手改干净 —— BoringSSL 自己也能读这两样，而**换掉它等于同时换掉
/// 「我们对证书的理解」**：SAN 的取法、时间的边界、非 DNS 类型 SAN 怎么处理，
/// 这些今天由一组测试钉着，换实现它们就要重新证一遍。
/// ⇒ **这一批换的是 TLS 后端，不是证书解析。** 两件事分开做，红了才分得清是哪一件。
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

    /// 会撞的与不会撞的都要覆盖到。★ 名单里那几个「看起来不像域名」的是重点。
    const NASTY: &[&str] = &[
        "example.com",
        "*.example.com",
        "a.example.com",
        "*.a.example.com",
        // ⚠ 下面这些都是**合法的 DNS 名**，而一个不完整的转义会让它们与上面几个相撞
        "_wildcard_.example.com",
        "__wildcard__.example.com",
        "_acme-challenge.example.com",
        "_.example.com",
        "___.example.com",
        "*._wildcard_.example.com",
    ];

    #[test]
    fn 域名转义是单射的() {
        // ★ ★ 判据不是「这几个样本没撞」，而是**存在反函数**：能逐字还原 ⇒ 一定单射。
        for n in NASTY {
            assert_eq!(
                unsanitize(&sanitize(n)),
                *n,
                "{n} 转义之后还不回来了（转义结果是 {}）",
                sanitize(n)
            );
        }
        // 顺带把「不撞」也直接验一遍，免得反函数写错时两条同时错。
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
        // ⚠ 反向判据：这两个域名不同，目录必须不同。
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
        // ★ 这两种必须分开：「还没签」是正常状态，「签了但读不出来」要人看。
        //   混在一起会让一张坏证书表现为「反复重新签发」。
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
        // 没有真证书，所以这里只验 meta 的容错路径不 panic
        fs::write(dir.join("meta.json"), "{ not json").unwrap();
        // 缺 cert/key → None（而不是因为 meta 坏了就 Err）
        assert!(store.load("test", "a.com").unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 同一进程里两次上锁不会自我死锁() {
        // ⚠ `flock` 是**按 fd** 的，同一进程用两个不同的 fd 会真的互斥。
        //   所以这里验的是：拿完释放之后还能再拿到，而不是「可重入」。
        let root = tmpdir();
        let store = CertStore::new(&root);
        {
            let _g = store.lock("test", "a.com").unwrap();
        }
        let _g2 = store.lock("test", "a.com").unwrap();
        // 不同域名互不影响
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
        // ★ ★ 守的是那个缺陷的另一半：`load()` 缺 `cert.pem` 就返回 `None`，
        //   于是**从来没签成过**的域名连自己的失败计数都读不回来，
        //   每一轮都从「零次失败」重新开始 —— 退避永远长不大。
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
        // 只写 meta，**不写证书** —— 正是一个签了四次都没签下来的域名的样子。
        store.save_meta("letsencrypt", "never.com", &meta).unwrap();
        assert!(
            store.load("letsencrypt", "never.com").unwrap().is_none(),
            "没有证书，load() 本来就该给 None"
        );
        // ★ 而续期状态必须还在。
        let back = store.load_meta("letsencrypt", "never.com");
        assert_eq!(back.renewal.failures, 4);
        assert_eq!(back.renewal.last_attempt, Some(1234));
        // ★ 反向：一个**根本没记录过**的域名要拿到干净的默认值，
        //   而不是继承上一个域名的计数（否则新域名一上来就被退避挡住）。
        let none = store.load_meta("letsencrypt", "fresh.com");
        assert_eq!(none.renewal.failures, 0);
        assert_eq!(none.renewal.last_attempt, None);
        assert_eq!(none.issuer, "letsencrypt");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 老的_meta_没有_issuer_url_也读得出来() {
        // ⚠ 升级要向后兼容：批 4 之前写下的 `meta.json` 里没有 `issuer_url` 这个键。
        //   读不出来的后果不是「少一个字段」，而是**整张证书被当成坏的**（`load` 返回 Err），
        //   于是一次升级会让所有自动签发的站点同时握手失败。
        let old = r#"{"issuer":"letsencrypt","ari_start":null,"ari_end":null,
                      "renewal":{"failures":0,"last_attempt":null}}"#;
        let back: Meta = serde_json::from_str(old).expect("老格式的 meta 必须还读得出来");
        assert_eq!(back.issuer, "letsencrypt");
        // ★ `None` 的语义是「不知道是谁签的」，而不是「不是我们签的」——
        //   后者会让升级之后每个域名都被重签一遍。
        assert_eq!(back.issuer_url, None);
    }
}
