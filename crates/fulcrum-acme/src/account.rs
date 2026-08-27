//! ACME 账户：建一次，存下来，之后一直用同一个。
//!
//! 路径：`<state>/acme/<issuer>/account.json`，**0600**。
//!
//! # ★ ★ 为什么必须存
//!
//! 每建一个账户 = CA 那边多一条记录 + 一次速率配额。进程重启就新建一个账户，
//! 表现是「跑了几天之后突然签不出来了」，而那时的错误信息是速率限制，
//! **指向的是域名而不是账户**——排查方向会整个跑偏。
//!
//! ⚠ 而且账户密钥是**撤销证书的凭据**：换了账户，之前签的证书就再也撤不掉了。
//!
//! # ★ 文件权限与证书私钥同级
//!
//! 用的是 [`fulcrum_tls::store::atomic_write`]，与私钥同一份实现——
//! 「权限在 create 时给，不是写完再 chmod」那条规则只有一份代码，就不会有一半忘了。

use fulcrum_tls::store::atomic_write;
use instant_acme::{Account, AccountBuilder, AccountCredentials, NewAccount};
use log::{info, warn};
use std::io;
use std::path::{Path, PathBuf};

/// 造一个 `instant-acme` 的账户构造器，**HTTPS 客户端由我们自己给**。
///
/// # ★ ★ 为什么不是 `Account::builder()`
///
/// **G104 第 ③ 处**：`instant-acme` 的 `hyper-rustls` feature 已经关掉了
/// （它 gate 的是 `dep:hyper` / `dep:hyper-rustls` / `dep:hyper-util` / `dep:rustls`），
/// 而 `Account::builder()` 与 `Account::builder_with_root()` **两个入口都挂在那个 feature 上**
/// ⇒ 现在只剩 `builder_with_http()` 这一条，而它要一个
/// [`instant_acme::HttpClient`] —— 就是 [`crate::https::AcmeHttpClient`]。
///
/// ⚠ ⚠ **这不是「换个写法」，它换掉的是 ACME 协议那条路的整套 TLS**：
/// 信任库、链校验、主机名校验、ALPN 从此与原生 DNS 供应商那条路是**同一份**。
/// ★ 而「同一份」是结构上的，不是靠两处注释互相钉着 —— 两条路只有一个连接器构造入口。
fn new_builder() -> Result<AccountBuilder, String> {
    let http = crate::https::AcmeHttpClient::new()
        .map_err(|e| format!("建不出 ACME 的 HTTPS 客户端：{e}"))?;
    Ok(Account::builder_with_http(Box::new(http)))
}

/// 账户文件的存放处。
pub struct AccountStore {
    root: PathBuf,
}

impl AccountStore {
    pub fn new(root: impl Into<PathBuf>) -> AccountStore {
        AccountStore { root: root.into() }
    }

    pub fn path_for(&self, issuer: &str) -> PathBuf {
        self.root.join(issuer).join("account.json")
    }

    /// 拿到账户：存储里有就用它，没有就建一个并存下来。
    ///
    /// ⚠ **存储里有但读不出来（坏了 / 权限不对）是硬错误，不是「那就重建一个」**。
    /// 重建会静默地丢掉撤销能力，并多消耗一次账户配额，而现场毫无提示。
    /// 这与 [`fulcrum_tls::CertStore::load`] 那条「缺文件是 None、坏文件是 Err」同形。
    pub async fn load_or_create(
        &self,
        issuer: &str,
        directory_url: &str,
        contact: Option<&str>,
    ) -> Result<Account, String> {
        let path = self.path_for(issuer);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("读不动 ACME 账户 {}：{e}", path.display()))?;
            let creds: AccountCredentials = serde_json::from_str(&raw).map_err(|e| {
                format!(
                    "ACME 账户 {} 解析不了：{e}\n\
                     ★ 这是硬错误而不是「重建一个」：重建会丢掉撤销既有证书的能力，\
                     还会再消耗一次账户配额，而现场看不出发生过什么。",
                    path.display()
                )
            })?;
            let account = new_builder()?
                .from_credentials(creds)
                .await
                .map_err(|e| format!("用 {} 里的凭据登不上 CA：{e}", path.display()))?;
            info!(
                "复用已存的 ACME 账户 {}（{}）",
                account.id(),
                path.display()
            );
            return Ok(account);
        }

        // ── 建一个新的 ──────────────────────────────────────────────────────
        //
        // ★ `terms_of_service_agreed: true`：ACME 协议要求显式同意。
        //   把它写成配置项没有意义——不同意就一张证书都签不出来，
        //   而**用户运行 `fulcrum serve` 并配了自动 HTTPS，本身就是那个意思**。
        //   ⚠ 但 CA 的 TOS URL 要在装载日志里说出来，见 `issue.rs` 的装载摘要。
        let contacts: Vec<String> = contact
            .map(|c| vec![format!("mailto:{c}")])
            .unwrap_or_default();
        let contact_refs: Vec<&str> = contacts.iter().map(String::as_str).collect();
        if contact_refs.is_empty() {
            warn!(
                "没有配 `acme_email` —— 账户不带联系方式。\
                 ★ 代价是**收不到 CA 的到期与撤销通知**：出事时你会在监控里先看到，而不是在邮件里。"
            );
        }
        let (account, creds) = new_builder()?
            .create(
                &NewAccount {
                    contact: &contact_refs,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                directory_url.to_string(),
                None,
            )
            .await
            .map_err(|e| format!("在 {directory_url} 上建 ACME 账户失败：{e}"))?;

        let json =
            serde_json::to_string_pretty(&creds).map_err(|e| format!("账户凭据序列化失败：{e}"))?;
        save_secret(&path, json.as_bytes())
            .map_err(|e| format!("写不进 ACME 账户 {}：{e}", path.display()))?;
        info!(
            "新建 ACME 账户 {}，凭据存在 {}（0600）",
            account.id(),
            path.display()
        );
        Ok(account)
    }
}

/// 写一份**私密**文件：目录先建好，内容原子替换，权限 0600。
fn save_secret(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write(path, bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fulcrum-acct-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn 账户文件的权限是_0600() {
        // ⚠ 它里面是账户私钥 —— 与证书私钥同级，不是「一份配置」。
        let root = tmpdir();
        let s = AccountStore::new(&root);
        let p = s.path_for("letsencrypt");
        save_secret(&p, b"{}").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "账户凭据的权限是 {mode:o}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 每个签发者一个账户目录() {
        let s = AccountStore::new("/var/lib/fulcrum/acme");
        assert!(
            s.path_for("letsencrypt")
                .ends_with("letsencrypt/account.json")
        );
        assert_ne!(s.path_for("letsencrypt"), s.path_for("letsencrypt-staging"));
        // ★ 换 CA 就是换账户 —— 拿 staging 的账户去生产签是签不出来的，
        //   而如果两者共用一个文件，那个错误会在**第一次生产签发**时才出现。
        assert_ne!(s.path_for("letsencrypt"), s.path_for("ca-localhost-14000"));
    }

    #[test]
    fn 写完之后没有临时文件残留() {
        let root = tmpdir();
        let s = AccountStore::new(&root);
        let p = s.path_for("x");
        save_secret(&p, b"{}").unwrap();
        let leftovers: Vec<String> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "有临时文件残留：{leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
