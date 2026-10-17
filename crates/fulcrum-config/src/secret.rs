//! 憑證值（**G59 第 1 條的修訂**）。
//!
//! # ★ ★ ★ 口徑變了：從「寫不下字面值」變成「寫得下，但它變成秘密」
//!
//! 在此之前，DSL 裡寫憑證字面值是**編譯期錯誤**，只認 `env:` / `file:` 兩種來源。
//! owner 拍板改成 Caddy 的形狀：**一份設定檔案就能跑完**。
//!
//! ⚠ 而這句話真正的代價不是「多一種寫法」，是**設定檔案的性質變了** ——
//! 它從「可以隨便看的東西」變成了「秘密」。而樞衡裡有四條路會把設定原樣吐出來：
//!
//! | 路 | 原本會洩漏什麼 |
//! |---|---|
//! | `fulcrum compile` | 整份結構化 JSON（憑證在裡面）|
//! | `POST /load` 的載荷 | 同上，還會穿過管理面 |
//! | 診斷 | **出錯那一行會被原樣打出來**（帶 caret）—— 一個語法錯誤就能把 token 打進 journald |
//! | `Debug` / 紀錄 | 任何 `{:?}` 一整份設定的地方 |
//!
//! ⇒ 所以本類型的預設行為是**脫敏**，露出真值要顯式進 [`reveal`] 作用域。
//! ★ 判據不是「我們記得在這四處脫敏」，而是**預設就不出真值** ——
//! 一條要在 N 個地方記住的規則，遲早會在其中一個地方被忘掉。

use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use std::cell::Cell;
use std::fmt;

/// 脫敏之後印出來的樣子。
///
/// ★ 有意選一個**不可能是合法憑證**的串：它出現在哪兒，哪兒就是脫敏過的，
/// 而不是「某個恰好長這樣的 token」。
pub const REDACTED: &str = "«已脱敏»";

thread_local! {
    /// 當前執行緒是否在 [`reveal`] 作用域裡。
    ///
    /// ⚠ 用 thread-local 而不是給 `Serialize` 加參數：serde 的 `Serialize` 簽名是固定的，
    /// 而**預設脫敏**這件事必須落在 `Serialize` 自己身上 —— 落在呼叫方就等於
    /// 「每個呼叫方都要記得」，那正是這裡要避免的。
    static REVEAL: Cell<bool> = const { Cell::new(false) };
}

/// 在這個閉包裡，[`Secret`] 序列化出**真值**。
///
/// ★ 唯一的用法是 `fulcrum compile --with-secrets` 與管理面的 load 載荷生成 ——
/// 兩處都是**使用者顯式要求**的時刻。
pub fn reveal<T>(f: impl FnOnce() -> T) -> T {
    REVEAL.with(|r| r.set(true));
    let out = f();
    REVEAL.with(|r| r.set(false));
    out
}

fn revealing() -> bool {
    REVEAL.with(|r| r.get())
}

/// 一個憑證「從哪兒來」——可能是指針（`env:` / `file:`），也可能就是值本身。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    raw: String,
    /// 真值就在 `raw` 裡（字面值）⇒ 印出去要脫敏。
    sensitive: bool,
    /// ★ 這一份是**從脫敏過的產物讀回來的** —— 裡面沒有真值。
    ///
    /// ⚠ 它必須與「普通字面值」分開：把 `«已脱敏»` 當成憑證發給 CA，
    /// 現場表現是「憑證不對」，而沒有任何一處會說「你 load 的是一份脫敏產物」。
    redacted: bool,
}

impl Secret {
    /// 從 DSL / JSON 裡那個詞造一個。
    ///
    /// | 寫法 | 結果 |
    /// |---|---|
    /// | `env:NAME` / `file:/path` | 指針，不脫敏（它本來就不是秘密）|
    /// | `literal:<值>` | 字面值，脫敏 |
    /// | `«已脱敏»` | ★ 標記為「讀回來的脫敏產物」，用它去簽發會**明確回報錯誤** |
    /// | 其它任何值 | 字面值，脫敏（Caddy 形狀：不寫前綴就是值本身）|
    pub fn parse(raw: &str) -> Secret {
        if raw == REDACTED {
            return Secret {
                raw: raw.to_string(),
                sensitive: true,
                redacted: true,
            };
        }
        let sensitive = !(raw.starts_with("env:") || raw.starts_with("file:"));
        Secret {
            raw: raw.to_string(),
            sensitive,
            redacted: false,
        }
    }

    /// 造一個**明確不是秘密**的值（例如 `dns exec` 的處理程序路徑）。
    ///
    /// # ⚠ ⚠ 為什麼需要它 —— 一刀切「無前綴就是憑證」會誤傷
    ///
    /// `dns` 的第二個參數**按供應商有兩種含義**：
    ///
    /// | 供應商 | 第二個參數是 | 是秘密嗎 |
    /// |---|---|---|
    /// | `cloudflare` / `dnspod` | 憑證 | ✅ |
    /// | `exec` | **hook 處理程序的路徑** | ❌ |
    ///
    /// 不分這個岔的話，`dns exec /etc/fulcrum/dns-hook.sh`
    /// 被當成了字面值憑證 —— **門禁當場紅**：ACME 那個場景的設定是 0644，
    /// 而它裡面根本沒有秘密。
    /// ★ ★ 一道會在**沒有秘密的設定**上開火的門，會訓練人去無腦 chmod、
    /// 或者乾脆無視它 —— 那時它擋不住真正該擋的那一次。
    pub fn path(raw: &str) -> Secret {
        Secret {
            raw: raw.to_string(),
            sensitive: false,
            redacted: false,
        }
    }

    /// 真值（含 `env:` / `file:` 前綴那種指針寫法）。
    ///
    /// ⚠ 拿到它就有責任不把它打出去。★ 名字裡帶 `expose` 是有意的：
    /// grep 一下就知道全倉有哪幾處碰過真值。
    pub fn expose(&self) -> &str {
        &self.raw
    }

    /// 是不是字面值（真值就在設定檔案裡）。★ 裝載期的權限門看這一條。
    pub fn is_sensitive(&self) -> bool {
        self.sensitive
    }

    /// 是不是從脫敏產物讀回來的。
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }

    /// 印出來給人看的樣子：字面值一律脫敏，指針原樣（它不是秘密，而且它是**線索**）。
    pub fn display(&self) -> &str {
        if self.sensitive { REDACTED } else { &self.raw }
    }
}

/// ⚠ `Debug` 也脫敏：一個 `{:?}` 一整份設定的紀錄語句，是最容易被忘掉的那條路。
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({})", self.display())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display())
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.sensitive && !revealing() {
            return s.serialize_str(REDACTED);
        }
        // ★ 露真值時給字面值補上 `literal:` 前綴：結構化層是**公開入口**（G11），
        //   而一個沒有前綴的裸值在那裡是有歧義的（它是值？還是某種沒寫對的來源？）。
        if self.sensitive && !self.raw.starts_with("literal:") {
            return s.serialize_str(&format!("literal:{}", self.raw));
        }
        s.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        let raw = String::deserialize(d)?;
        if let Some(v) = raw.strip_prefix("literal:") {
            if v.is_empty() {
                return Err(de::Error::custom("`literal:` 后面是空的"));
            }
            return Ok(Secret {
                raw: v.to_string(),
                sensitive: true,
                redacted: false,
            });
        }
        Ok(Secret::parse(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 字面量默认脱敏_而指针原样() {
        let lit = Secret::parse("cfat_realtoken12345");
        assert!(lit.is_sensitive());
        assert_eq!(lit.display(), REDACTED);
        assert_eq!(format!("{lit}"), REDACTED);
        assert_eq!(format!("{lit:?}"), format!("Secret({REDACTED})"));
        assert_eq!(
            serde_json::to_string(&lit).unwrap(),
            format!("\"{REDACTED}\"")
        );

        // ★ 指針不是秘密，而且它是**線索**：出錯時「從哪個變數/檔案讀的」必須說得出來。
        let ptr = Secret::parse("env:CF_API_TOKEN");
        assert!(!ptr.is_sensitive());
        assert_eq!(ptr.display(), "env:CF_API_TOKEN");
        assert_eq!(serde_json::to_string(&ptr).unwrap(), "\"env:CF_API_TOKEN\"");
    }

    #[test]
    fn 只有显式进入_reveal_才吐真值() {
        let lit = Secret::parse("cfat_realtoken12345");
        let hidden = serde_json::to_string(&lit).unwrap();
        let shown = reveal(|| serde_json::to_string(&lit).unwrap());
        assert_eq!(hidden, format!("\"{REDACTED}\""));
        assert_eq!(shown, "\"literal:cfat_realtoken12345\"");
        // ⚠ 出了作用域必須立刻恢復 —— 否則一次 `--with-secrets` 會把整個處理程序後續
        //   所有序列化都變成明文。
        assert_eq!(
            serde_json::to_string(&lit).unwrap(),
            format!("\"{REDACTED}\"")
        );
    }

    #[test]
    fn 脱敏产物读回来要认得出是脱敏的() {
        // ★ ★ 這一條擋的是最壞的那種失敗：把 `«已脱敏»` 當成憑證發給 CA，
        //   現場表現是「憑證不對」，而沒有任何一處會說「你 load 的是一份脫敏產物」。
        let back: Secret = serde_json::from_str(&format!("\"{REDACTED}\"")).unwrap();
        assert!(back.is_redacted());
        assert!(back.is_sensitive());
    }

    #[test]
    fn 带_literal_前缀的写回来还是同一个值() {
        let lit = Secret::parse("literal:abc123");
        // `literal:` 前綴在 parse 時不剝離（DSL 側由 compile 負責），
        // 但 JSON 往返必須**穩定**：寫出去帶前綴，讀回來剝掉，值不變。
        let json = reveal(|| serde_json::to_string(&lit).unwrap());
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), "abc123");
        assert!(back.is_sensitive());
        assert!(!back.is_redacted());
    }
}
