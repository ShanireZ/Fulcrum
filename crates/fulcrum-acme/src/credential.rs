//! DNS 供应商凭据的来源（**G59 第 1 条**）。
//!
//! > 凭据绝不写进 DSL——DSL 是要被 diff、被贴进 issue、被版本控制的东西；
//! > 只从**文件或环境变量**读。
//!
//! ★ 拿到某个域的 DNS 写权限，等于**能为该域签发任意证书**，还能改 MX 劫持邮件。
//!
//! # ⚠ 所以 DSL 里必须**写不下**字面量，而不是「建议不要写」
//!
//! 只认两种写法：
//!
//! ```text
//! dns cloudflare env:CF_API_TOKEN
//! dns dnspod     file:/run/secrets/dnspod-token
//! ```
//!
//! 一个**没有前缀**的值一律是编译期错误（错误在最早的时刻暴露）。
//! ⚠ 反过来写（「看起来像 token 就报错」）是错的 —— 那要去猜什么样子算 token，
//! 而猜错的那一次恰恰就是真 token 被放行的那一次。**白名单，不是黑名单。**
//!
//! ★ **值本身永不进日志**：本模块不实现 `Debug`/`Display`，
//! 错误信息里只出现来源（变量名 / 文件路径）。

use std::path::PathBuf;

/// 凭据从哪儿来。
///
/// ⚠ ⚠ **加了第三种：值本身**（owner 拍板，Caddy 形状 —— 一份配置文件就能跑完）。
/// 口径因此从「DSL 里写不下凭据」变成「写得下，而配置文件从此是秘密」：
/// 装载期有一道**权限门**（配置文件对 other 可读就拒绝启动），
/// `fulcrum compile` 默认脱敏，露真值要 `--with-secrets`。见 `fulcrum_config::secret`。
#[derive(Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// `env:NAME`
    Env(String),
    /// `file:/path/to/secret`
    File(PathBuf),
    /// 值本身（DSL 里直接写，或 `literal:` 前缀）。
    Literal(String),
}

/// ⚠ 手写 `Debug`：`derive` 会把 `Literal` 里的真值打出来，
/// 而这个类型会出现在任何一处 `{:?}` 了配置或错误的地方。
impl std::fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialSource::Env(n) => write!(f, "Env({n})"),
            CredentialSource::File(p) => write!(f, "File({})", p.display()),
            // ★ 长度都不给：它也是信息。
            CredentialSource::Literal(_) => write!(f, "Literal(«已脱敏»)"),
        }
    }
}

impl CredentialSource {
    /// 解析 DSL 里那个第二参数。
    ///
    /// ⚠ 认不出前缀就是错误，**不做任何兜底猜测**。
    pub fn parse(spec: &str) -> Result<CredentialSource, String> {
        if let Some(name) = spec.strip_prefix("env:") {
            if name.is_empty() {
                return Err("`env:` 后面没有变量名".to_string());
            }
            return Ok(CredentialSource::Env(name.to_string()));
        }
        if let Some(path) = spec.strip_prefix("file:") {
            if path.is_empty() {
                return Err("`file:` 后面没有路径".to_string());
            }
            return Ok(CredentialSource::File(PathBuf::from(path)));
        }
        // ★ 显式前缀：值本身且带冒号时用它，免得被当成写错的来源。
        if let Some(v) = spec.strip_prefix("literal:") {
            if v.is_empty() {
                return Err("`literal:` 后面是空的".to_string());
            }
            return Ok(CredentialSource::Literal(v.to_string()));
        }
        // ⚠ 走到这里 = 没有任何前缀 ⇒ **值本身**（Caddy 形状）。
        //   空值仍然是错误：一个空凭据只会在对端那里变成一句指不出原因的拒绝。
        if spec.is_empty() {
            return Err("凭据是空的".to_string());
        }
        Ok(CredentialSource::Literal(spec.to_string()))
    }

    /// 这条凭据从哪儿来，**只用于日志与错误信息**。★ 不含值。
    pub fn describe(&self) -> String {
        match self {
            CredentialSource::Env(n) => format!("环境变量 {n}"),
            CredentialSource::File(p) => format!("文件 {}", p.display()),
            // ⚠ ⚠ 字面量的「来源」就是配置文件本身 —— 这句话在排查时很要紧：
            //   它告诉运维「去看 Fulcrumfile」，而不是去翻环境变量和 /run/secrets。
            //   ★ 但**一个字的值都不能出现在这里**：describe() 会进日志。
            CredentialSource::Literal(_) => "配置文件里的字面量".to_string(),
        }
    }

    /// 真去读一次。
    ///
    /// ⚠ 读出来要 `trim`：`echo token > file` 会留一个换行，而带换行的
    /// `Authorization` 头会被 HTTP 层拒绝——现场只有一句「请求构造失败」。
    /// ⚠ 空值当成**没有凭据**，不是「凭据是空串」。
    pub fn load(&self) -> Result<String, String> {
        let raw = match self {
            CredentialSource::Env(name) => std::env::var(name).map_err(|_| {
                format!("环境变量 {name} 没有设置（G59：凭据只从文件或环境变量读）")
            })?,
            CredentialSource::File(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("读不了凭据文件 {}：{e}", path.display()))?,
            // ★ 字面量就在手里，不用读任何东西。
            //   ⚠ 仍然走下面那个 trim 与空值判定：一份从 YAML/JSON 拼出来的配置
            //   完全可能带上尾随空白，而带空白的 `Authorization` 头会被 HTTP 层拒绝。
            CredentialSource::Literal(v) => v.clone(),
        };
        let v = raw.trim().to_string();
        if v.is_empty() {
            return Err(format!("{} 里是空的", self.describe()));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 两种前缀都认得() {
        assert_eq!(
            CredentialSource::parse("env:CF_API_TOKEN").unwrap(),
            CredentialSource::Env("CF_API_TOKEN".into())
        );
        assert_eq!(
            CredentialSource::parse("file:/run/secrets/x").unwrap(),
            CredentialSource::File(PathBuf::from("/run/secrets/x"))
        );
    }

    #[test]
    fn 没有前缀的就是值本身_而打错的前缀要报错() {
        // ★ ★ ★ **这条测试换过一次契约（批 22），换法本身留在这里。**
        //
        //   旧契约：没有前缀一律拒绝，判据是**白名单** —— 理由是「看起来像 token
        //   就报错」的黑名单要去猜什么样子算 token，而猜错的那一次恰恰就是
        //   真 token 被放行的那一次，且没有任何症状。
        //
        //   新契约（owner 拍板，Caddy 形状）：**没有前缀就是值本身**。
        //   ⚠ 白名单那条理由没有失效，它换了落点：现在要拦的是**打错的前缀** ——
        //   `fil:/path` 会被当成凭据发给对端，现场是「凭据不对」，
        //   而真正的原因是打错了三个字母。那一层拦在**编译期**（`FUL-DSL-0031`）。
        //
        //   ⇒ 本函数这一层只剩两条判据：值本身认得出来、空值仍然是错。

        // ① 没有前缀 = 值本身
        for v in [
            "abcdef0123456789",
            "CF_API_TOKEN",
            "12345,abcdefabcdef",
            "cfat_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        ] {
            match CredentialSource::parse(v) {
                Ok(CredentialSource::Literal(got)) => assert_eq!(got, v),
                other => panic!("`{v}` 应当被当成值本身，拿到 {other:?}"),
            }
        }

        // ② `literal:` 是**带冒号的值**的出路
        match CredentialSource::parse("literal:12345,ab:cd") {
            Ok(CredentialSource::Literal(v)) => assert_eq!(v, "12345,ab:cd"),
            other => panic!("literal: 前缀没剥掉：{other:?}"),
        }

        // ③ 空值仍然是错：一个空凭据只会在对端那里变成一句指不出原因的拒绝
        assert!(CredentialSource::parse("").is_err());
        assert!(CredentialSource::parse("literal:").is_err());

        // ④ ★ 两种指针写法照旧
        assert_eq!(
            CredentialSource::parse("env:CF").unwrap(),
            CredentialSource::Env("CF".to_string())
        );
        assert!(matches!(
            CredentialSource::parse("file:/run/secrets/x").unwrap(),
            CredentialSource::File(_)
        ));
    }

    #[test]
    fn 字面量凭据不许出现在_debug_里() {
        // ⚠ ⚠ 这个类型会出现在任何一处 `{:?}` 了配置或错误的地方，
        //   而 `derive(Debug)` 会把真值原样打出来。★ 连长度都不给：它也是信息。
        let c = CredentialSource::parse("SUPERSECRETTOKEN").unwrap();
        let printed = format!("{c:?}");
        assert!(
            !printed.contains("SUPERSECRETTOKEN"),
            "真凭据进了 Debug：{printed}"
        );
        assert!(!printed.contains("16"), "长度也不该给：{printed}");

        // ★ 而 describe()（专门给日志用的那个）同样不许带值 ——
        //   它只说「去看配置文件」，那才是排查时要的信息。
        let d = c.describe();
        assert!(!d.contains("SUPERSECRETTOKEN"), "真凭据进了 describe：{d}");
        assert!(d.contains("配置文件"), "describe 要说清去哪儿找：{d}");
    }

    #[test]
    fn 前缀后面空着也是错() {
        assert!(CredentialSource::parse("env:").is_err());
        assert!(CredentialSource::parse("file:").is_err());
    }

    #[test]
    fn 读文件时把尾随换行去掉() {
        // ⚠ `echo token > file` 会留一个换行，而带换行的 Authorization 头
        //   会被 HTTP 层拒绝——现场只有一句「请求构造失败」，看不出是文件末尾。
        let dir = std::env::temp_dir().join(format!("fulcrum-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t");
        std::fs::write(&p, "  secret-value\n").unwrap();
        let src = CredentialSource::File(p.clone());
        assert_eq!(src.load().unwrap(), "secret-value");
        // 空文件当成「没有凭据」
        std::fs::write(&p, "\n \n").unwrap();
        assert!(src.load().unwrap_err().contains("空的"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 描述里不含值() {
        // ★ 错误信息与日志里只出现来源，不出现内容（安全基线第 3 条）。
        let src = CredentialSource::Env("CF_API_TOKEN".into());
        assert_eq!(src.describe(), "环境变量 CF_API_TOKEN");
        // 变量没设时的错误也只提名字。
        let e = src.load().unwrap_err();
        assert!(e.contains("CF_API_TOKEN"), "{e}");
    }
}
