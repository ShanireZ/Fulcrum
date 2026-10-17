//! DNS 供應商憑證的來源（**G59 第 1 條**）。
//!
//! > 憑證絕不寫進 DSL——DSL 是要被 diff、被貼進 issue、被版本控制的東西；
//! > 只從**檔案或環境變數**讀。
//!
//! ★ 拿到某個域的 DNS 寫權限，等於**能為該域簽發任意證書**，還能改 MX 劫持電子郵件。
//!
//! # ⚠ 所以 DSL 裡必須**寫不下**字面值，而不是「建議不要寫」
//!
//! 只認兩種寫法：
//!
//! ```text
//! dns cloudflare env:CF_API_TOKEN
//! dns dnspod     file:/run/secrets/dnspod-token
//! ```
//!
//! 一個**沒有前綴**的值一律是編譯期錯誤（錯誤在最早的時刻暴露）。
//! ⚠ 反過來寫（「看起來像 token 就回報錯誤」）是錯的 —— 那要去猜什麼樣子算 token，
//! 而猜錯的那一次恰恰就是真 token 被放行的那一次。**白名單，不是黑名單。**
//!
//! ★ **值本身永不進紀錄**：本模組不實作 `Debug`/`Display`，
//! 錯誤資訊裡只出現來源（變數名 / 檔案路徑）。

use std::path::PathBuf;

/// 憑證從哪兒來。
///
/// ⚠ ⚠ **加了第三種：值本身**（owner 拍板，Caddy 形狀 —— 一份設定檔案就能跑完）。
/// 口徑因此從「DSL 裡寫不下憑證」變成「寫得下，而設定檔案從此是秘密」：
/// 裝載期有一道**權限門**（設定檔案對 other 可讀就拒絕啟動），
/// `fulcrum compile` 預設脫敏，露真值要 `--with-secrets`。見 `fulcrum_config::secret`。
#[derive(Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// `env:NAME`
    Env(String),
    /// `file:/path/to/secret`
    File(PathBuf),
    /// 值本身（DSL 裡直接寫，或 `literal:` 前綴）。
    Literal(String),
}

/// ⚠ 手寫 `Debug`：`derive` 會把 `Literal` 裡的真值打出來，
/// 而這個類型會出現在任何一處 `{:?}` 了設定或錯誤的地方。
impl std::fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialSource::Env(n) => write!(f, "Env({n})"),
            CredentialSource::File(p) => write!(f, "File({})", p.display()),
            // ★ 長度都不給：它也是資訊。
            CredentialSource::Literal(_) => write!(f, "Literal(«已脱敏»)"),
        }
    }
}

impl CredentialSource {
    /// 解析 DSL 裡那個第二參數。
    ///
    /// ⚠ 認不出前綴就是錯誤，**不做任何備援猜測**。
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
        // ★ 顯式前綴：值本身且帶冒號時用它，免得被當成寫錯的來源。
        if let Some(v) = spec.strip_prefix("literal:") {
            if v.is_empty() {
                return Err("`literal:` 后面是空的".to_string());
            }
            return Ok(CredentialSource::Literal(v.to_string()));
        }
        // ⚠ 走到這裡 = 沒有任何前綴 ⇒ **值本身**（Caddy 形狀）。
        //   空值仍然是錯誤：一個空憑證只會在對端那裡變成一句指不出原因的拒絕。
        if spec.is_empty() {
            return Err("凭据是空的".to_string());
        }
        Ok(CredentialSource::Literal(spec.to_string()))
    }

    /// 這條憑證從哪兒來，**只用於紀錄與錯誤資訊**。★ 不含值。
    pub fn describe(&self) -> String {
        match self {
            CredentialSource::Env(n) => format!("环境变量 {n}"),
            CredentialSource::File(p) => format!("文件 {}", p.display()),
            // ⚠ ⚠ 字面值的「來源」就是設定檔案本身 —— 這句話在排查時很要緊：
            //   它告訴維運「去看 Fulcrumfile」，而不是去翻環境變數和 /run/secrets。
            //   ★ 但**一個字的值都不能出現在這裡**：describe() 會進紀錄。
            CredentialSource::Literal(_) => "配置文件里的字面量".to_string(),
        }
    }

    /// 真去讀一次。
    ///
    /// ⚠ 讀出來要 `trim`：`echo token > file` 會留一個換行，而帶換行的
    /// `Authorization` 頭會被 HTTP 層拒絕——現場只有一句「請求建構失敗」。
    /// ⚠ 空值當成**沒有憑證**，不是「憑證是空串」。
    pub fn load(&self) -> Result<String, String> {
        let raw = match self {
            CredentialSource::Env(name) => std::env::var(name).map_err(|_| {
                format!("环境变量 {name} 没有设置（G59：凭据只从文件或环境变量读）")
            })?,
            CredentialSource::File(path) => std::fs::read_to_string(path)
                .map_err(|e| format!("读不了凭据文件 {}：{e}", path.display()))?,
            // ★ 字面值就在手裡，不用讀任何東西。
            //   ⚠ 仍然走下面那個 trim 與空值判定：一份從 YAML/JSON 拼出來的設定
            //   完全可能帶上尾隨空白，而帶空白的 `Authorization` 頭會被 HTTP 層拒絕。
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
        // ★ ★ ★ **這條測試換過一次契約（批 22），換法本身留在這裡。**
        //
        //   舊契約：沒有前綴一律拒絕，判據是**白名單** —— 理由是「看起來像 token
        //   就回報錯誤」的黑名單要去猜什麼樣子算 token，而猜錯的那一次恰恰就是
        //   真 token 被放行的那一次，且沒有任何症狀。
        //
        //   新契約（owner 拍板，Caddy 形狀）：**沒有前綴就是值本身**。
        //   ⚠ 白名單那條理由沒有失效，它換了落點：現在要攔的是**打錯的前綴** ——
        //   `fil:/path` 會被當成憑證發給對端，現場是「憑證不對」，
        //   而真正的原因是打錯了三個字母。那一層攔在**編譯期**（`FUL-DSL-0031`）。
        //
        //   ⇒ 本函式這一層只剩兩條判據：值本身認得出來、空值仍然是錯。

        // ① 沒有前綴 = 值本身
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

        // ② `literal:` 是**帶冒號的值**的出路
        match CredentialSource::parse("literal:12345,ab:cd") {
            Ok(CredentialSource::Literal(v)) => assert_eq!(v, "12345,ab:cd"),
            other => panic!("literal: 前缀没剥掉：{other:?}"),
        }

        // ③ 空值仍然是錯：一個空憑證只會在對端那裡變成一句指不出原因的拒絕
        assert!(CredentialSource::parse("").is_err());
        assert!(CredentialSource::parse("literal:").is_err());

        // ④ ★ 兩種指針寫法照舊
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
        // ⚠ ⚠ 這個類型會出現在任何一處 `{:?}` 了設定或錯誤的地方，
        //   而 `derive(Debug)` 會把真值原樣打出來。★ 連長度都不給：它也是資訊。
        let c = CredentialSource::parse("SUPERSECRETTOKEN").unwrap();
        let printed = format!("{c:?}");
        assert!(
            !printed.contains("SUPERSECRETTOKEN"),
            "真凭据进了 Debug：{printed}"
        );
        assert!(!printed.contains("16"), "长度也不该给：{printed}");

        // ★ 而 describe()（專門給紀錄用的那個）同樣不許帶值 ——
        //   它只說「去看設定檔案」，那才是排查時要的資訊。
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
        // ⚠ `echo token > file` 會留一個換行，而帶換行的 Authorization 頭
        //   會被 HTTP 層拒絕——現場只有一句「請求建構失敗」，看不出是檔案末尾。
        let dir = std::env::temp_dir().join(format!("fulcrum-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t");
        std::fs::write(&p, "  secret-value\n").unwrap();
        let src = CredentialSource::File(p.clone());
        assert_eq!(src.load().unwrap(), "secret-value");
        // 空檔案當成「沒有憑證」
        std::fs::write(&p, "\n \n").unwrap();
        assert!(src.load().unwrap_err().contains("空的"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 描述里不含值() {
        // ★ 錯誤資訊與紀錄裡只出現來源，不出現內容（安全基線第 3 條）。
        let src = CredentialSource::Env("CF_API_TOKEN".into());
        assert_eq!(src.describe(), "环境变量 CF_API_TOKEN");
        // 變數沒設時的錯誤也只提名字。
        let e = src.load().unwrap_err();
        assert!(e.contains("CF_API_TOKEN"), "{e}");
    }
}
