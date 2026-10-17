//! ACME 帳戶：建一次，存下來，之後一直用同一個。
//!
//! 路徑：`<state>/acme/<issuer>/account.json`，**0600**。
//!
//! # ★ ★ 為什麼必須存
//!
//! 每建一個帳戶 = CA 那邊多一條記錄 + 一次速率配額。處理程序重新啟動就新建一個帳戶，
//! 表現是「跑了幾天之後突然籤不出來了」，而那時的錯誤資訊是速率限制，
//! **指向的是網域而不是帳戶**——排查方向會整個跑偏。
//!
//! ⚠ 而且帳戶金鑰是**撤銷證書的憑證**：換了帳戶，之前籤的證書就再也撤不掉了。
//!
//! # ★ 檔案權限與證書私鑰同級
//!
//! 用的是 [`fulcrum_tls::store::atomic_write`]，與私鑰同一份實作——
//! 「權限在 create 時給，不是寫完再 chmod」那條規則只有一份程式碼，就不會有一半忘了。

use fulcrum_tls::store::atomic_write;
use instant_acme::{Account, AccountBuilder, AccountCredentials, NewAccount};
use log::{info, warn};
use std::io;
use std::path::{Path, PathBuf};

/// 造一個 `instant-acme` 的帳戶建構器，**HTTPS 用戶端由我們自己給**。
///
/// # ★ ★ 為什麼不是 `Account::builder()`
///
/// **G104 第 ③ 處**：`instant-acme` 的 `hyper-rustls` feature 已經關掉了
/// （它 gate 的是 `dep:hyper` / `dep:hyper-rustls` / `dep:hyper-util` / `dep:rustls`），
/// 而 `Account::builder()` 與 `Account::builder_with_root()` **兩個入口都掛在那個 feature 上**
/// ⇒ 現在只剩 `builder_with_http()` 這一條，而它要一個
/// [`instant_acme::HttpClient`] —— 就是 [`crate::https::AcmeHttpClient`]。
///
/// ⚠ ⚠ **這不是「換個寫法」，它換掉的是 ACME 協定那條路的整套 TLS**：
/// 信任庫、鏈檢核、主機名檢核、ALPN 從此與原生 DNS 供應商那條路是**同一份**。
/// ★ 而「同一份」是結構上的，不是靠兩處註解互相釘著 —— 兩條路只有一個連接器建構入口。
fn new_builder() -> Result<AccountBuilder, String> {
    let http = crate::https::AcmeHttpClient::new()
        .map_err(|e| format!("建不出 ACME 的 HTTPS 客户端：{e}"))?;
    Ok(Account::builder_with_http(Box::new(http)))
}

/// 帳戶檔案的存放處。
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

    /// 拿到帳戶：儲存裡有就用它，沒有就建一個並存下來。
    ///
    /// ⚠ **儲存裡有但讀不出來（壞了 / 權限不對）是硬錯誤，不是「那就重建一個」**。
    /// 重建會靜默地丟掉撤銷能力，並多消耗一次帳戶配額，而現場毫無提示。
    /// 這與 [`fulcrum_tls::CertStore::load`] 那條「缺檔案是 None、壞檔案是 Err」同形。
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

        // ── 建一個新的 ──────────────────────────────────────────────────────
        //
        // ★ `terms_of_service_agreed: true`：ACME 協定要求顯式同意。
        //   把它寫成設定項沒有意義——不同意就一張證書都籤不出來，
        //   而**使用者執行 `fulcrum serve` 並配了自動 HTTPS，本身就是那個意思**。
        //   ⚠ 但 CA 的 TOS URL 要在裝載紀錄裡說出來，見 `issue.rs` 的裝載摘要。
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

/// 寫一份**私密**檔案：目錄先建好，內容原子替換，權限 0600。
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
        // ⚠ 它裡面是帳戶私鑰 —— 與證書私鑰同級，不是「一份設定」。
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
        // ★ 換 CA 就是換帳戶 —— 拿 staging 的帳戶去生產籤是籤不出來的，
        //   而如果兩者共用一個檔案，那個錯誤會在**第一次生產簽發**時才出現。
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
