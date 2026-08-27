//! 缓存键与 `Vary`（RFC 9111 §4.1）。**纯函数**。
//!
//! ★ 键分两层，这一点是有意的：
//!
//! - **主键**（`primary`）＝ 方法 + scheme + host + 路径 + 查询串。
//!   一条 URL 在缓存里只有一个主键。
//! - **次级键**（`secondary`）＝ 响应的 `Vary` 点名的那几个请求头的值。
//!   同一个主键下可以有多份，各自对应一组请求头。
//!
//! ⚠ ⚠ **为什么不合成一个键**：`Vary` 是**响应**告诉我们的，而请求到达时我们
//! 还不知道它 —— 必须先按主键找到那一族，才知道要比哪几个头。
//! ★ 合成一个键的写法只能在「已经知道 Vary」时算得出来，于是它必然要么
//! 猜一个 Vary、要么根本命中不了。
//!
//! ⚠ ⚠ **`Vary` 的两条容易写错、而且错了不会报错的规则**：
//!
//! 1. **头名大小写不敏感，值大小写敏感。** 把值也折成小写，
//!    `Accept-Encoding: GZIP` 与 `gzip` 会共用一份 —— 通常没事，直到某个
//!    上游真的按大小写给不同内容。
//! 2. **缺席与空串是两回事。** 一个**没发** `Accept-Encoding` 的请求，
//!    与一个发了空值的请求，RFC 上是两个不同的次级键。
//!    ★ 都折成空串的话，前者会命中后者的那份 —— 而后者可能是压缩过的。

/// 主键：一条 URL 在缓存里的身份。
pub fn primary(method: &str, scheme: &str, host: &str, path: &str, query: &str) -> String {
    // ⚠ 分隔符用 `\u{1}`（不会出现在这几样里的字节），而不是 `:` 或 `|` ——
    //   host 里有冒号、query 里什么都可能有。★ 一个可以出现在成分里的分隔符，
    //   意味着两组不同的成分能拼出同一个键，也就是**两条 URL 共用一份缓存**。
    let mut s = String::with_capacity(method.len() + host.len() + path.len() + query.len() + 8);
    s.push_str(method);
    s.push('\u{1}');
    s.push_str(scheme);
    s.push('\u{1}');
    // host 折小写：主机名本来就大小写不敏感。
    for c in host.chars() {
        s.push(c.to_ascii_lowercase());
    }
    s.push('\u{1}');
    s.push_str(path);
    s.push('\u{1}');
    s.push_str(query);
    s
}

/// 解析 `Vary` 头，返回**小写**的头名列表。`Vary: *` 由调用方单独判（那条不可缓存）。
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

/// 次级键：按 `vary` 点名的头，从请求里取值拼出来。
///
/// `get` 返回 `None` 表示**该头缺席** —— 与返回 `Some("")` 是两回事（见文件顶部）。
pub fn secondary<'a, F>(vary: &[String], mut get: F) -> String
where
    F: FnMut(&str) -> Option<&'a str>,
{
    let mut s = String::new();
    for name in vary {
        s.push_str(name);
        s.push('\u{1}');
        match get(name) {
            // ★ 缺席用一个**不可能出现在头值里**的标记，而不是空串。
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

    // ★ ★ ★ 分隔符不能是可以出现在成分里的字符。
    //   ⚠ 用 `:` 的话，`host="a.com:8080" path="/x"` 与 `host="a.com" path="8080:/x"`
    //   会拼出同一个键 —— **两条 URL 共用一份缓存**，也就是给错内容。
    #[test]
    fn 分隔符不会被成分伪造出来() {
        let a = primary("GET", "https", "a.com:8080", "/x", "");
        let b = primary("GET", "https", "a.com", "8080/x", "");
        assert_ne!(a, b);
        // ⚠ 同一族的第二种：查询串与路径的边界。`path="/x" query="a=1"` 与
        //   `path="/x?a=1" query=""` 是两条不同的请求，键也必须不同。
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
    //   ⚠ 都折成空串的话，一个**没发** Accept-Encoding 的请求会命中
    //   那份压缩过的响应 —— 客户端收到一坨它没要求也解不开的字节。
    #[test]
    fn 头缺席与头是空串是两个次级键() {
        let vary = vec!["accept-encoding".to_string()];
        let absent = secondary(&vary, |_| None);
        let empty = secondary(&vary, |_| Some(""));
        assert_ne!(absent, empty, "缺席与空串必须是两个键");
    }

    // ★ 值**大小写敏感**（只有头名不敏感）。
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
        // 把两个值对调 ⇒ 必须是另一个键。
        let mut m2: HashMap<&str, &str> = HashMap::new();
        m2.insert("a", "2");
        m2.insert("b", "1");
        let k2 = secondary(&vary, |n| m2.get(n).copied());
        assert_ne!(k1, k2);

        // ⚠ 一个值里带分隔符也不能伪造出另一个键。
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
