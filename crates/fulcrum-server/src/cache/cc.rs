//! `Cache-Control` 頭的解析（RFC 9111 §5.2）。**純函式**。
//!
//! ★ ★ G97 拍的是「最小集 **+ 上游回應頭裡的全套 `Cache-Control` 指令**」，
//! 所以這裡把回應側那一整張表都實作了，而不是只挑常見的幾條。
//!
//! ⚠ ⚠ **兩條最容易被寫錯、而且錯了不會回報錯誤的規則：**
//!
//! 1. **`no-cache` 有兩種形態，含義完全不同**：不帶參數 = 「可以存，但每次用之前
//!    必須重驗證」；帶參數 `no-cache="Set-Cookie"` = 「可以存**也可以直接用**，
//!    只是列出來的那幾個頭不許發給用戶端」。★ 把後者當成前者，快取等於沒開；
//!    把前者當成後者，**會把過期內容直接發出去**。
//! 2. **`no-store` 與 `no-cache` 不是一回事**：`no-store` 是「一個位元組都不許完成實作」。
//!
//! ⚠ **指令名大小寫不敏感**，參數可以帶引號，`=` 兩邊可以有空格 ——
//! 這三條裡漏掉任何一條，都會讓一個**合法**的頭被當成沒有。

use serde::{Deserialize, Serialize};

/// 回應側 `Cache-Control` 的全套指令（RFC 9111 §5.2.2 + RFC 5861）。
///
/// ★ ★ **`Serialize`/`Deserialize` 是磁碟後端（批 H）要的**：一條快取條目的
/// 中繼資料裡必須帶著**存下來時**那份 `Cache-Control` —— 重驗證與 `must-revalidate`
/// 的判定都要用它。⚠ 不存而是「讀的時候從頭裡再解析一遍」會有個坑：
/// 我們發給用戶端的頭是**剝過**的（`no-cache="字段名"` 點名的那幾個已經不在了），
/// 於是再解析一遍得到的不是同一份。
/// ★ 結構體級的 `#[serde(default)]`：少一個欄位時給零值而不是整條解析失敗 ——
/// ⚠ 而**真正的**版本相容靠 `disk::META_VERSION` 那個號，不靠這裡的寬容。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResponseCc {
    /// `no-store`：一個位元組都不許完成實作。
    pub no_store: bool,
    /// `no-cache` **不帶參數**：可以存，但每次用之前必須重驗證。
    pub no_cache: bool,
    /// `no-cache="a, b"`：可以存也可以直接用，但這幾個頭不許發給用戶端。
    ///
    /// ★ 與 `no_cache` **互斥地**記錄 —— 它們是兩條不同的規則，合成一個 bool
    /// 就再也分不開了。
    pub no_cache_fields: Vec<String>,
    /// `private`：共享快取（我們就是）不許存。
    pub private: bool,
    /// `private="a, b"`：可以存，但這幾個頭不許存。
    pub private_fields: Vec<String>,
    /// `public`：即使按預設規則不可快取，也可以存。
    pub public: bool,
    /// `max-age=<秒>`。
    pub max_age: Option<u64>,
    /// `s-maxage=<秒>` —— ★ **共享快取優先用它**，它壓過 `max-age`。
    pub s_maxage: Option<u64>,
    /// `must-revalidate`：過期之後**不許**發陳內容。
    pub must_revalidate: bool,
    /// `proxy-revalidate`：同上，但只約束共享快取。
    pub proxy_revalidate: bool,
    /// `immutable`：新鮮期內不必重驗證。
    pub immutable: bool,
    /// `no-transform`：不許改內容編碼等。
    pub no_transform: bool,
    /// `stale-while-revalidate=<秒>`（RFC 5861）。★ 這一批**解析但不施加**。
    pub stale_while_revalidate: Option<u64>,
    /// `stale-if-error=<秒>`（RFC 5861）。★ 這一批**解析但不施加**。
    pub stale_if_error: Option<u64>,
    /// 解析到了至少一條我們認識的指令。
    ///
    /// ★ 它與「頭存在」不是一回事：`Cache-Control: ` 空值、或者一整頭全是
    /// 我們不認識的擴充指令時，這裡是 `false`。判「上游有沒有說過新鮮度」要用
    /// [`Self::has_freshness`]，不是這一條。
    pub any: bool,
}

impl ResponseCc {
    /// 上游有沒有**明確給出**新鮮度？（G96 的備援判據用它。）
    pub fn has_freshness(&self) -> bool {
        self.max_age.is_some() || self.s_maxage.is_some()
    }

    /// 共享快取該用哪個新鮮度秒數。★ `s-maxage` 壓過 `max-age`（RFC 9111 §5.2.2.10）。
    pub fn shared_max_age(&self) -> Option<u64> {
        self.s_maxage.or(self.max_age)
    }
}

/// 請求側 `Cache-Control`（RFC 9111 §5.2.1）。
///
/// ★ 只做真正會改變行為的那幾條。⚠ **`no-cache` 在請求側的含義與回應側相反**：
/// 請求側是「別給我快取的，去問上游」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestCc {
    /// `no-cache`：本次必須回源（或至少重驗證）。
    pub no_cache: bool,
    /// `no-store`：本次的請求與回應都不許完成實作。
    pub no_store: bool,
    /// `only-if-cached`：只要快取裡的；沒有就回 **504**（RFC 9111 §5.2.1.7）。
    pub only_if_cached: bool,
    /// `max-age=<秒>`：用戶端不接受比這更舊的。
    pub max_age: Option<u64>,
    /// `min-fresh=<秒>`：用戶端要求至少還能新鮮這麼久。
    pub min_fresh: Option<u64>,
    /// `max-stale` / `max-stale=<秒>`：用戶端接受過期多久的。
    /// `Some(None)` = 不限。
    #[allow(clippy::option_option)]
    pub max_stale: Option<Option<u64>>,
}

/// 把一個 `Cache-Control` 頭切成 `(指令名小写, 参数)`。
///
/// ⚠ ⚠ 手寫而不是 `split(',')`：**參數可以帶引號，而引號裡可以有逗號**
/// （`no-cache="Set-Cookie, X-Foo"` 是一條指令，不是兩條）。
/// ★ 直接按逗號切會把它切成 `no-cache="Set-Cookie` 與 `X-Foo"` ——
/// 前者的參數少一半、後者是個不認識的指令，**而兩者都不會回報錯誤**。
fn directives(header: &str) -> Vec<(String, Option<String>)> {
    let b = header.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // 跳過分隔符與空白
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // 指令名：到 `=` 或 `,` 為止
        let name_start = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        let name = header[name_start..i].trim().to_ascii_lowercase();
        let mut value: Option<String> = None;
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && b[i] == b'"' {
                // 引號串：讀到下一個未轉義的引號。
                i += 1;
                let vs = i;
                let mut buf = String::new();
                while i < b.len() {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        buf.push(b[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    if b[i] == b'"' {
                        break;
                    }
                    buf.push(b[i] as char);
                    i += 1;
                }
                let _ = vs;
                if i < b.len() {
                    i += 1; // 吃掉收尾引號
                }
                value = Some(buf);
            } else {
                let vs = i;
                while i < b.len() && b[i] != b',' {
                    i += 1;
                }
                value = Some(header[vs..i].trim().to_string());
            }
        }
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// 逗號分隔的欄位名列表（`no-cache="a, b"` 的參數）。全部轉小寫。
fn field_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn secs(v: &Option<String>) -> Option<u64> {
    // ⚠ RFC 9111 §5.2：解析不出來的 delta-seconds **當成一個很大的值**是錯的，
    //   而當成「這條指令不存在」也是錯的。這裡取「這條指令不存在」——
    //   ★ 因為另一側（當成很大）會讓一個寫壞的 `max-age=abc` 變成"永遠新鮮"。
    v.as_ref()?.parse::<u64>().ok()
}

/// 解析回應側的 `Cache-Control`。
pub fn parse_response(header: &str) -> ResponseCc {
    let mut cc = ResponseCc::default();
    for (name, value) in directives(header) {
        cc.any = true;
        match name.as_str() {
            "no-store" => cc.no_store = true,
            "no-cache" => match &value {
                // ★ ★ 帶參數與不帶參數是**兩條不同的規則**，見本檔案頂部。
                Some(v) if !v.is_empty() => cc.no_cache_fields = field_list(v),
                _ => cc.no_cache = true,
            },
            "private" => match &value {
                Some(v) if !v.is_empty() => cc.private_fields = field_list(v),
                _ => cc.private = true,
            },
            "public" => cc.public = true,
            "max-age" => cc.max_age = secs(&value),
            "s-maxage" => cc.s_maxage = secs(&value),
            "must-revalidate" => cc.must_revalidate = true,
            "proxy-revalidate" => cc.proxy_revalidate = true,
            "immutable" => cc.immutable = true,
            "no-transform" => cc.no_transform = true,
            "stale-while-revalidate" => cc.stale_while_revalidate = secs(&value),
            "stale-if-error" => cc.stale_if_error = secs(&value),
            // ⚠ 不認識的擴充指令**忽略**（RFC 9111 §5.2.3 就是這麼說的），
            //   但不把 `any` 撤回去 —— 「頭裡有東西」與「我們認得它」是兩件事。
            _ => {}
        }
    }
    cc
}

/// 解析請求側的 `Cache-Control`。
pub fn parse_request(header: &str) -> RequestCc {
    let mut cc = RequestCc::default();
    for (name, value) in directives(header) {
        match name.as_str() {
            "no-cache" => cc.no_cache = true,
            "no-store" => cc.no_store = true,
            "only-if-cached" => cc.only_if_cached = true,
            "max-age" => cc.max_age = secs(&value),
            "min-fresh" => cc.min_fresh = secs(&value),
            "max-stale" => cc.max_stale = Some(secs(&value)),
            _ => {}
        }
    }
    cc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 常见组合解析对了() {
        let cc = parse_response("public, max-age=3600");
        assert!(cc.public);
        assert_eq!(cc.max_age, Some(3600));
        assert!(cc.has_freshness());

        let cc = parse_response("no-store");
        assert!(cc.no_store);
        assert!(!cc.has_freshness());
    }

    // ★ ★ ★ 本檔案頂部那條第 1 號規則：`no-cache` 的兩種形態。
    //   ⚠ 合成一個 bool 之後這兩條就再也分不開了 ——
    //   而把帶參數的當成不帶參數的，等於快取完全沒開；
    //   反過來，會把過期內容直接發給用戶端。
    #[test]
    fn no_cache_带参数与不带参数是两条不同的规则() {
        let bare = parse_response("no-cache");
        assert!(bare.no_cache, "不带参数 ⇒ 每次都要重验证");
        assert!(bare.no_cache_fields.is_empty());

        let qualified = parse_response("no-cache=\"Set-Cookie\"");
        assert!(
            !qualified.no_cache,
            "带参数的**不是**「每次重验证」—— 它只是说那几个头不许发出去"
        );
        assert_eq!(qualified.no_cache_fields, ["set-cookie"]);
    }

    // ★ ★ ★ 引號裡的逗號。按逗號切會把這一條切成兩條，而**兩半都不回報錯誤**：
    //   前半參數少一個欄位、後半是個不認識的指令 —— 於是 X-Foo 被原樣發出去。
    #[test]
    fn 引号里的逗号不是分隔符() {
        let cc = parse_response("no-cache=\"Set-Cookie, X-Foo\", max-age=60");
        assert_eq!(cc.no_cache_fields, ["set-cookie", "x-foo"]);
        assert_eq!(cc.max_age, Some(60), "后面那条指令还在");
    }

    #[test]
    fn private_也有两种形态() {
        assert!(parse_response("private").private);
        let q = parse_response("private=\"Authorization\"");
        assert!(!q.private, "带参数的 private 不是「整条不许存」");
        assert_eq!(q.private_fields, ["authorization"]);
    }

    // ★ ★ 共享快取用 s-maxage，它壓過 max-age（RFC 9111 §5.2.2.10）。
    //   ⚠ 取錯的話，一個「瀏覽器 60s、CDN 1 天」的設定會被我們按 60s 用 ——
    //   功能正常，只是快取幾乎不命中，而沒有任何東西會說為什麼。
    #[test]
    fn s_maxage_压过_max_age() {
        let cc = parse_response("max-age=60, s-maxage=86400");
        assert_eq!(cc.shared_max_age(), Some(86400));
        let only_max = parse_response("max-age=60");
        assert_eq!(only_max.shared_max_age(), Some(60));
    }

    #[test]
    fn 大小写不敏感_等号两边可以有空格() {
        let cc = parse_response("Public, Max-Age = 300, MUST-REVALIDATE");
        assert!(cc.public);
        assert_eq!(cc.max_age, Some(300));
        assert!(cc.must_revalidate);
    }

    #[test]
    fn 全套指令都认得() {
        let cc = parse_response(
            "public, max-age=1, s-maxage=2, must-revalidate, proxy-revalidate, \
             immutable, no-transform, stale-while-revalidate=3, stale-if-error=4",
        );
        assert!(cc.public && cc.must_revalidate && cc.proxy_revalidate);
        assert!(cc.immutable && cc.no_transform);
        assert_eq!(
            (
                cc.max_age,
                cc.s_maxage,
                cc.stale_while_revalidate,
                cc.stale_if_error
            ),
            (Some(1), Some(2), Some(3), Some(4))
        );
    }

    // ★ ★ 寫壞的 delta-seconds 當成「這條指令不存在」，**不是**當成很大的值。
    //   ⚠ 另一側會讓 `max-age=abc` 變成"永遠新鮮" —— 一條打錯的設定換來
    //   一個永不更新的頁面，而現場看不出與快取有關。
    #[test]
    fn 写坏的秒数当成这条指令不存在() {
        let cc = parse_response("max-age=abc");
        assert_eq!(cc.max_age, None);
        assert!(!cc.has_freshness(), "写坏了 ⇒ 上游等于没给新鲜度");
        assert!(cc.any, "但「头里有东西」这件事仍然是真的");
    }

    #[test]
    fn 不认识的扩展指令被忽略而不是报错() {
        let cc = parse_response("x-vendor-thing=1, max-age=5");
        assert_eq!(cc.max_age, Some(5));
        assert!(cc.any);
    }

    #[test]
    fn 空头与全空白() {
        for h in ["", "   ", ",", " , , "] {
            let cc = parse_response(h);
            assert!(!cc.any, "{h:?} 不该算「说过什么」");
            assert!(!cc.has_freshness());
        }
    }

    // ── 請求側 ────────────────────────────────────────────────────────────
    #[test]
    fn 请求侧那几条() {
        let cc = parse_request("no-cache, max-age=0");
        assert!(cc.no_cache);
        assert_eq!(cc.max_age, Some(0));

        let cc = parse_request("only-if-cached");
        assert!(cc.only_if_cached);

        // `max-stale` 不帶參數 = 不限。
        assert_eq!(parse_request("max-stale").max_stale, Some(None));
        assert_eq!(parse_request("max-stale=60").max_stale, Some(Some(60)));
        assert_eq!(parse_request("min-fresh=30").min_fresh, Some(30));
    }

    // ⚠ 請求側的 `no-cache` 與回應側含義相反 —— 釘一條免得兩邊共用一個解析器。
    #[test]
    fn 请求侧的_no_cache_不带字段名语义() {
        let cc = parse_request("no-cache=\"Set-Cookie\"");
        assert!(cc.no_cache, "请求侧带不带参数都是「别给我缓存的」");
    }
}
