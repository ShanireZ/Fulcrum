//! 設定檔案的權限門（**批 22**）。
//!
//! # ★ ★ ★ 允許內聯憑證，真正的代價是這一條
//!
//! 字面值憑證寫得進 Fulcrumfile 之後，**這份檔案的性質就變了** ——
//! 它從「可以隨便看的東西」變成了「秘密」。而 Unix 上「是不是秘密」只有一個客觀判據：
//! **別人讀不讀得到**。
//!
//! ⇒ 裝載時檢查：**只要設定裡出現了字面值憑證**，這份檔案就不許對 `other` 可讀。
//!
//! | | 判據 |
//! |---|---|
//! | 沒有字面值憑證 | **一個字都不查** —— 不去管別人怎麼放一份沒有秘密的設定 |
//! | 有字面值憑證，`o` 位有任何權限 | **拒絕啟動**（形狀照 G15：錯誤在最早的時刻暴露）|
//! | 有字面值憑證，group 可讀 | 放行 —— `0640 root:fulcrum` 是正當形狀（root 擁有、服務讀）|
//!
//! # ⚠ 為什麼不乾脆要求 0600
//!
//! 因為那會**逼人把設定檔案交給服務使用者擁有**，而設定本該由 root 管、服務唯讀。
//! 一條把人推向更差實踐的規則，比沒有規則更糟。
//!
//! # ★ 這道門為什麼不在編譯期
//!
//! 編譯期不知道檔案在哪（`compile_str` 連路徑都沒有），而且**權限是執行環境的事實**：
//! 同一份設定，今天 0600、明天被 `chmod 644` —— 判據必須在**每次裝載**時重新問一遍。

use crate::model::StructuredConfig;

/// 設定裡有沒有「真值就在檔案裡」的憑證。
pub fn has_inline_secret(cfg: &StructuredConfig) -> bool {
    cfg.sites.iter().any(|s| {
        s.tls
            .dns_arg
            .as_ref()
            .is_some_and(|a| a.is_sensitive() && !a.is_redacted())
    })
}

/// 設定裡有沒有**脫敏過的**憑證（`«已脱敏»`）。
///
/// # ★ ★ ★ 為什麼它必須是硬錯誤
///
/// `fulcrum compile` 預設吐脫敏產物，而那份 JSON 正是 `POST /load` 的載荷。
/// 少了這一條，`«已脱敏»` 會被當成憑證發給 CA —— 現場表現是「**憑證不對**」，
/// 而沒有任何一處會說「你 load 的是一份脫敏產物」。
/// ⚠ 它與「憑證真的寫錯了」長得一模一樣，而處置完全不同：
/// 一個要去 CA 那邊查權限，另一個只要重新 `compile --with-secrets`。
pub fn redacted_secrets(cfg: &StructuredConfig) -> Vec<String> {
    let mut out = Vec::new();
    for s in &cfg.sites {
        if s.tls.dns_arg.as_ref().is_some_and(|a| a.is_redacted()) {
            out.push(
                s.addresses
                    .first()
                    .map(|a| a.raw.clone())
                    .unwrap_or_else(|| "<无地址>".to_string()),
            );
        }
    }
    out
}

/// 裝載時的權限門。`Ok(None)` = 沒什麼可說的；`Ok(Some(warn))` = 有話說但不攔；
/// `Err` = **拒絕啟動**。
///
/// ⚠ 非 Unix 上直接放行：G13 的目標平台只有 Linux，而在別處**假裝檢查過**
/// 比不檢查更糟 —— 那會讓人以為有一道門。
pub fn check(path: &str, cfg: &StructuredConfig) -> Result<Option<String>, String> {
    // ★ 先擋脫敏產物：它比權限更早、也更確定 —— 那份設定**根本不可能工作**。
    let redacted = redacted_secrets(cfg);
    if !redacted.is_empty() {
        return Err(format!(
            "这些站点的凭据是**脱敏过的**（`{}`）：{}\n\
             ★ 它多半来自 `fulcrum compile` 的默认产物 —— 那份 JSON 给人看没问题，\
             但不能拿来跑。要带凭据的产物：`fulcrum compile <配置> --with-secrets`。\n\
             ⚠ 挡在这里是有意的：不挡的话，`«已脱敏»` 会被当成凭据发给 CA，\
             而现场表现是「凭据不对」——与真的凭据写错长得一模一样。",
            crate::secret::REDACTED,
            redacted.join(" ")
        ));
    }
    if !has_inline_secret(cfg) {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(path)
            .map_err(|e| format!("配置里有字面量凭据，而读不到 {path} 的权限信息：{e}"))?;
        let mode = md.permissions().mode() & 0o777;
        if mode & 0o007 != 0 {
            return Err(format!(
                "配置 {path} 里写了字面量凭据，而它的权限是 {:04o} —— **其他用户读得到**。\n\
                 ★ 这份文件从写下凭据那一刻起就是秘密。改成 0640（属主 root、组给服务用户）：\n\
                     chown root:{gid} {path} && chmod 640 {path}\n\
                 ⚠ 或者把凭据挪出去，写成 `env:变量名` / `file:路径` —— 那样这道门根本不会响。",
                mode,
                gid = md.gid(),
            ));
        }
        if mode & 0o070 != 0 {
            return Ok(Some(format!(
                "配置 {path} 里有字面量凭据，权限 {mode:04o}（同组用户读得到）—— \
                 这是正当形状（root 拥有、服务用户读），记一笔而已",
            )));
        }
        Ok(Some(format!(
            "配置 {path} 里有字面量凭据，权限 {mode:04o} —— 只有属主读得到，好",
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(Some(
            "配置里有字面量凭据，而当前平台上这道权限门不生效（G13 的目标平台只有 Linux）—— \
             ★ 说出来而不是假装检查过"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_str;

    fn cfg_of(src: &str) -> StructuredConfig {
        compile_str("t.Fulcrumfile", src).config.expect("编译不过")
    }

    const INLINE: &str = "*.a.com {\n  tls {\n    dns dnspod 12345,abcdefabcdefabcdef\n    zones a.com\n    resolvers 1.1.1.1\n  }\n  respond 200\n}\n";
    const POINTER: &str = "*.a.com {\n  tls {\n    dns dnspod file:/run/secrets/t\n    zones a.com\n    resolvers 1.1.1.1\n  }\n  respond 200\n}\n";

    #[test]
    fn 脱敏过的凭据在任何入口都要被挡下() {
        // ★ ★ 判據掛在 `check()` 上而不是只掛在 serve 的 ACME 那一層：
        //   `validate` 的全部意義就是「上線之前先問一遍」，
        //   而一份 load 不起來的設定正是它該攔住的東西。
        let src = format!(
            "*.a.com {{\n  tls {{\n    dns dnspod {}\n    zones a.com\n    resolvers 1.1.1.1\n  }}\n  respond 200\n}}\n",
            crate::secret::REDACTED
        );
        let cfg = cfg_of(&src);
        assert_eq!(redacted_secrets(&cfg), vec!["*.a.com".to_string()]);
        let e = check("/nonexistent", &cfg).expect_err("脱敏产物竟然放行了");
        assert!(e.contains("--with-secrets"), "错误里要给出怎么办：{e}");
        // ⚠ 它必須比權限門**更早**：上面那個路徑根本不存在，
        //   而如果先查權限，報出來的會是「讀不到權限資訊」——指錯方向。
        assert!(!e.contains("权限"), "报错指向了权限而不是脱敏：{e}");
    }

    #[test]
    fn exec_的程序路径不是秘密() {
        // ★ ★ 一刀切「無前綴就是憑證」是錯的：
        //   於是 `dns exec /path/to/hook` 被當成字面值，ACME 那個場景的 0644 設定當場被拒。
        //   ⚠ 一道會在沒有秘密的設定上開火的門，會訓練人無腦 chmod 或直接無視它 ——
        //   那時它擋不住真正該擋的那一次。
        let src = "*.a.com {
  tls {
    dns exec /etc/fulcrum/dns-hook.sh
    resolvers 1.1.1.1
  }
  respond 200
}
";
        let cfg = cfg_of(src);
        assert!(!has_inline_secret(&cfg), "exec 的程序路径被当成秘密了");
        // ★ 而且它**不該被脫敏**：那條路徑是排查時的線索。
        let arg = cfg.sites[0].tls.dns_arg.as_ref().unwrap();
        assert_eq!(arg.display(), "/etc/fulcrum/dns-hook.sh");
    }

    #[test]
    fn 只有内联凭据才认得出来() {
        assert!(has_inline_secret(&cfg_of(INLINE)));
        // ★ 指針寫法不算：那份檔案裡沒有秘密，不該被這道門管。
        assert!(!has_inline_secret(&cfg_of(POINTER)));
    }

    #[cfg(unix)]
    #[test]
    fn 有内联凭据时_对其他人可读就拒绝启动() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fulcrum-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Fulcrumfile");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(INLINE.as_bytes()).unwrap();
        let p = path.to_str().unwrap();

        // 0644：其他人讀得到 ⇒ 必須拒絕
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = check(p, &cfg_of(INLINE)).expect_err("0644 带着凭据竟然放行了");
        assert!(e.contains("其他用户读得到"), "{e}");
        assert!(e.contains("chmod 640"), "错误里要给出怎么办：{e}");

        // 0640：正當形狀 ⇒ 放行
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(check(p, &cfg_of(INLINE)).is_ok());

        // 0600 ⇒ 放行
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check(p, &cfg_of(INLINE)).is_ok());

        // ★ ★ 反面：同樣是 0644，但設定裡**沒有**字面值憑證 ⇒ 一個字都不該說。
        //   少了這一條，一道「對所有設定都要求 0640」的實作會讓上面三條照常綠。
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(check(p, &cfg_of(POINTER)).unwrap(), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
