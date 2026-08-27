//! 配置文件的权限门（**批 22**）。
//!
//! # ★ ★ ★ 允许内联凭据，真正的代价是这一条
//!
//! 字面量凭据写得进 Fulcrumfile 之后，**这份文件的性质就变了** ——
//! 它从「可以随便看的东西」变成了「秘密」。而 Unix 上「是不是秘密」只有一个客观判据：
//! **别人读不读得到**。
//!
//! ⇒ 装载时检查：**只要配置里出现了字面量凭据**，这份文件就不许对 `other` 可读。
//!
//! | | 判据 |
//! |---|---|
//! | 没有字面量凭据 | **一个字都不查** —— 不去管别人怎么放一份没有秘密的配置 |
//! | 有字面量凭据，`o` 位有任何权限 | **拒绝启动**（形状照 G15：错误在最早的时刻暴露）|
//! | 有字面量凭据，group 可读 | 放行 —— `0640 root:fulcrum` 是正当形状（root 拥有、服务读）|
//!
//! # ⚠ 为什么不干脆要求 0600
//!
//! 因为那会**逼人把配置文件交给服务用户拥有**，而配置本该由 root 管、服务只读。
//! 一条把人推向更差实践的规则，比没有规则更糟。
//!
//! # ★ 这道门为什么不在编译期
//!
//! 编译期不知道文件在哪（`compile_str` 连路径都没有），而且**权限是运行时的事实**：
//! 同一份配置，今天 0600、明天被 `chmod 644` —— 判据必须在**每次装载**时重新问一遍。

use crate::model::StructuredConfig;

/// 配置里有没有「真值就在文件里」的凭据。
pub fn has_inline_secret(cfg: &StructuredConfig) -> bool {
    cfg.sites.iter().any(|s| {
        s.tls
            .dns_arg
            .as_ref()
            .is_some_and(|a| a.is_sensitive() && !a.is_redacted())
    })
}

/// 配置里有没有**脱敏过的**凭据（`«已脱敏»`）。
///
/// # ★ ★ ★ 为什么它必须是硬错误
///
/// `fulcrum compile` 默认吐脱敏产物，而那份 JSON 正是 `POST /load` 的载荷。
/// 少了这一条，`«已脱敏»` 会被当成凭据发给 CA —— 现场表现是「**凭据不对**」，
/// 而没有任何一处会说「你 load 的是一份脱敏产物」。
/// ⚠ 它与「凭据真的写错了」长得一模一样，而处置完全不同：
/// 一个要去 CA 那边查权限，另一个只要重新 `compile --with-secrets`。
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

/// 装载时的权限门。`Ok(None)` = 没什么可说的；`Ok(Some(warn))` = 有话说但不拦；
/// `Err` = **拒绝启动**。
///
/// ⚠ 非 Unix 上直接放行：G13 的目标平台只有 Linux，而在别处**假装检查过**
/// 比不检查更糟 —— 那会让人以为有一道门。
pub fn check(path: &str, cfg: &StructuredConfig) -> Result<Option<String>, String> {
    // ★ 先挡脱敏产物：它比权限更早、也更确定 —— 那份配置**根本不可能工作**。
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
        // ★ ★ 判据挂在 `check()` 上而不是只挂在 serve 的 ACME 那一层：
        //   `validate` 的全部意义就是「上线之前先问一遍」，
        //   而一份 load 不起来的配置正是它该拦住的东西。
        let src = format!(
            "*.a.com {{\n  tls {{\n    dns dnspod {}\n    zones a.com\n    resolvers 1.1.1.1\n  }}\n  respond 200\n}}\n",
            crate::secret::REDACTED
        );
        let cfg = cfg_of(&src);
        assert_eq!(redacted_secrets(&cfg), vec!["*.a.com".to_string()]);
        let e = check("/nonexistent", &cfg).expect_err("脱敏产物竟然放行了");
        assert!(e.contains("--with-secrets"), "错误里要给出怎么办：{e}");
        // ⚠ 它必须比权限门**更早**：上面那个路径根本不存在，
        //   而如果先查权限，报出来的会是「读不到权限信息」——指错方向。
        assert!(!e.contains("权限"), "报错指向了权限而不是脱敏：{e}");
    }

    #[test]
    fn exec_的程序路径不是秘密() {
        // ★ ★ 一刀切「无前缀就是凭据」是错的：
        //   于是 `dns exec /path/to/hook` 被当成字面量，ACME 那个场景的 0644 配置当场被拒。
        //   ⚠ 一道会在没有秘密的配置上开火的门，会训练人无脑 chmod 或直接无视它 ——
        //   那时它挡不住真正该挡的那一次。
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
        // ★ 而且它**不该被脱敏**：那条路径是排查时的线索。
        let arg = cfg.sites[0].tls.dns_arg.as_ref().unwrap();
        assert_eq!(arg.display(), "/etc/fulcrum/dns-hook.sh");
    }

    #[test]
    fn 只有内联凭据才认得出来() {
        assert!(has_inline_secret(&cfg_of(INLINE)));
        // ★ 指针写法不算：那份文件里没有秘密，不该被这道门管。
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

        // 0644：其他人读得到 ⇒ 必须拒绝
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = check(p, &cfg_of(INLINE)).expect_err("0644 带着凭据竟然放行了");
        assert!(e.contains("其他用户读得到"), "{e}");
        assert!(e.contains("chmod 640"), "错误里要给出怎么办：{e}");

        // 0640：正当形状 ⇒ 放行
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(check(p, &cfg_of(INLINE)).is_ok());

        // 0600 ⇒ 放行
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check(p, &cfg_of(INLINE)).is_ok());

        // ★ ★ 反面：同样是 0644，但配置里**没有**字面量凭据 ⇒ 一个字都不该说。
        //   少了这一条，一道「对所有配置都要求 0640」的实现会让上面三条照常绿。
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(check(p, &cfg_of(POINTER)).unwrap(), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
