//! 快取鍵與 `Vary`（RFC 9111 §4.1）。**純函式**。
//!
//! ★ 鍵分兩層，這一點是有意的：
//!
//! - **主鍵**（`primary`）＝ 方法 + scheme + host + 路徑 + 查詢串。
//!   一條 URL 在快取裡只有一個主鍵。
//! - **次級鍵**（`secondary`）＝ 回應的 `Vary` 點名的那幾個請求頭的值。
//!   同一個主鍵下可以有多份，各自對應一組請求頭。
//!
//! ⚠ ⚠ **為什麼不合成一個鍵**：`Vary` 是**回應**告訴我們的，而請求到達時我們
//! 還不知道它 —— 必須先按主鍵找到那一族，才知道要比哪幾個頭。
//! ★ 合成一個鍵的寫法只能在「已經知道 Vary」時算得出來，於是它必然要麼
//! 猜一個 Vary、要麼根本命中不了。
//!
//! ⚠ ⚠ **`Vary` 的兩條容易寫錯、而且錯了不會回報錯誤的規則**：
//!
//! 1. **頭名大小寫不敏感，值大小寫敏感。** 把值也折成小寫，
//!    `Accept-Encoding: GZIP` 與 `gzip` 會共用一份 —— 通常沒事，直到某個
//!    上游真的按大小寫給不同內容。
//! 2. **缺席與空串是兩回事。** 一個**沒發** `Accept-Encoding` 的請求，
//!    與一個發了空值的請求，RFC 上是兩個不同的次級鍵。
//!    ★ 都折成空串的話，前者會命中後者的那份 —— 而後者可能是壓縮過的。

/// 主鍵：一條 URL 在快取裡的身分。
pub fn primary(method: &str, scheme: &str, host: &str, path: &str, query: &str) -> String {
    // ⚠ 分隔符用 `\u{1}`（不會出現在這幾樣裡的位元組），而不是 `:` 或 `|` ——
    //   host 裡有冒號、query 裡什麼都可能有。★ 一個可以出現在成分裡的分隔符，
    //   意味著兩組不同的成分能拼出同一個鍵，也就是**兩條 URL 共用一份快取**。
    let mut s = String::with_capacity(method.len() + host.len() + path.len() + query.len() + 8);
    s.push_str(method);
    s.push('\u{1}');
    s.push_str(scheme);
    s.push('\u{1}');
    // host 折小寫：主機名本來就大小寫不敏感。
    for c in host.chars() {
        s.push(c.to_ascii_lowercase());
    }
    s.push('\u{1}');
    s.push_str(path);
    s.push('\u{1}');
    s.push_str(query);
    s
}

/// 解析 `Vary` 頭，回傳**小寫**的頭名列表。`Vary: *` 由呼叫方單獨判（那條不可快取）。
pub fn parse_vary(header: &str) -> Vec<String> {
    header
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `Vary: *`？
pub fn is_vary_star(header: &str) -> bool {
    header.split(',').any(|s| s.trim() == "*")
}

/// 次級鍵：按 `vary` 點名的頭，從請求裡取值拼出來。
///
/// `get` 回傳 `None` 表示**該頭缺席** —— 與回傳 `Some("")` 是兩回事（見檔案頂部）。
pub fn secondary<'a, F>(vary: &[String], mut get: F) -> String
where
    F: FnMut(&str) -> Option<&'a str>,
{
    let mut s = String::new();
    for name in vary {
        s.push_str(name);
        s.push('\u{1}');
        match get(name) {
            // ★ 缺席用一個**不可能出現在頭值裡**的標記，而不是空串。
            None => s.push('\u{2}'),
            Some(v) => s.push_str(v),
        }
        s.push('\u{1}');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn 主键把五样都算进去() {
        let a = primary("GET", "https", "a.com", "/x", "");
        for other in [
            primary("HEAD", "https", "a.com", "/x", ""),
            primary("GET", "http", "a.com", "/x", ""),
            primary("GET", "https", "b.com", "/x", ""),
            primary("GET", "https", "a.com", "/y", ""),
            primary("GET", "https", "a.com", "/x", "q=1"),
        ] {
            assert_ne!(a, other, "这两个本该是不同的键");
        }
    }

    #[test]
    fn 主机名大小写不敏感() {
        assert_eq!(
            primary("GET", "https", "A.CoM", "/x", ""),
            primary("GET", "https", "a.com", "/x", "")
        );
    }

    // ★ ★ ★ 分隔符不能是可以出現在成分裡的字元。
    //   ⚠ 用 `:` 的話，`host="a.com:8080" path="/x"` 與 `host="a.com" path="8080:/x"`
    //   會拼出同一個鍵 —— **兩條 URL 共用一份快取**，也就是給錯內容。
    #[test]
    fn 分隔符不会被成分伪造出来() {
        let a = primary("GET", "https", "a.com:8080", "/x", "");
        let b = primary("GET", "https", "a.com", "8080/x", "");
        assert_ne!(a, b);
        // ⚠ 同一族的第二種：查詢串與路徑的邊界。`path="/x" query="a=1"` 與
        //   `path="/x?a=1" query=""` 是兩條不同的請求，鍵也必須不同。
        let c = primary("GET", "https", "a.com", "/x", "a=1");
        let d = primary("GET", "https", "a.com", "/x?a=1", "");
        assert_ne!(c, d);
    }

    #[test]
    fn vary_解析与星号() {
        assert_eq!(
            parse_vary("Accept-Encoding, Accept-Language"),
            ["accept-encoding", "accept-language"]
        );
        assert!(is_vary_star("*"));
        assert!(is_vary_star("Accept-Encoding, *"));
        assert!(!is_vary_star("Accept-Encoding"));
        assert!(parse_vary("").is_empty());
    }

    // ★ ★ 缺席 ≠ 空串。
    //   ⚠ 都折成空串的話，一個**沒發** Accept-Encoding 的請求會命中
    //   那份壓縮過的回應 —— 用戶端收到一坨它沒要求也解不開的位元組。
    #[test]
    fn 头缺席与头是空串是两个次级键() {
        let vary = vec!["accept-encoding".to_string()];
        let absent = secondary(&vary, |_| None);
        let empty = secondary(&vary, |_| Some(""));
        assert_ne!(absent, empty, "缺席与空串必须是两个键");
    }

    // ★ 值**大小寫敏感**（只有頭名不敏感）。
    #[test]
    fn 头值大小写敏感() {
        let vary = vec!["accept-encoding".to_string()];
        let lower = secondary(&vary, |_| Some("gzip"));
        let upper = secondary(&vary, |_| Some("GZIP"));
        assert_ne!(lower, upper);
    }

    #[test]
    fn 多个_vary_头按顺序拼且互不串味() {
        let vary = vec!["a".to_string(), "b".to_string()];
        let mut m: HashMap<&str, &str> = HashMap::new();
        m.insert("a", "1");
        m.insert("b", "2");
        let k1 = secondary(&vary, |n| m.get(n).copied());
        // 把兩個值對調 ⇒ 必須是另一個鍵。
        let mut m2: HashMap<&str, &str> = HashMap::new();
        m2.insert("a", "2");
        m2.insert("b", "1");
        let k2 = secondary(&vary, |n| m2.get(n).copied());
        assert_ne!(k1, k2);

        // ⚠ 一個值裡帶分隔符也不能偽造出另一個鍵。
        let mut m3: HashMap<&str, &str> = HashMap::new();
        m3.insert("a", "1\u{1}b\u{1}2");
        m3.insert("b", "");
        let k3 = secondary(&vary, |n| m3.get(n).copied());
        assert_ne!(k1, k3, "值里塞分隔符不该拼出别人的键");
    }

    #[test]
    fn 没有_vary_时次级键是空的() {
        assert_eq!(secondary(&[], |_| None), "");
    }
}
