//! URL 路径 → 磁盘路径的那几步，**全是纯函数**（G87 / G88）。
//!
//! ★ ★ ★ 这几步的**顺序本身就是判据**，写反了等于没做：
//!
//! 1. **先百分号解码** —— 不然 `%2e%2e%2f` 根本不长得像 `../`，
//!    后面那步归一会原样放过去。⇒ 解码**在归一之前**。
//! 2. **再词法归一** —— 按 `/` 切段、丢 `.`、`..` 回退一级，回退到根之外就拒。
//!    ⚠ 这一步是**纯词法**的，不碰磁盘：磁盘那侧的符号链接由 G87 单管。
//! 3. **再查 hide 清单** —— 按**路径段**比，命中回 404（不是 403）。
//!
//! ⚠ ⚠ `%2f`（编码过的斜杠）解码之后**当分隔符**用。这是有意的：
//! 只有把它当分隔符，`a%2f..%2fb` 里那个 `..` 才会被第 2 步看见并处理。
//! 当成普通字符（"路径里真有个斜杠的文件"）反而是那条经典绕过路。

/// 归一之后的路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    /// 归一后的各段，**不含空段**。根目录本身是空 `Vec`。
    pub segments: Vec<String>,
    /// 原始 URL 末尾有没有 `/`。★ 目录的 301 判据要用它，
    /// 而它在归一之后就看不出来了 —— 所以在这里就记下来。
    pub trailing_slash: bool,
}

/// 解码 + 归一。返回 `None` = **拒**（400）。
///
/// 拒的三种情况：百分号写法坏了、解码出 `%00`、`..` 回退到根之外。
pub fn decode_and_normalize(raw: &str) -> Option<Normalized> {
    let decoded = percent_decode(raw)?;
    // ⚠ NUL 在这里拒掉。它到不了 `open()`（Rust 的 `CString` 会拒），
    //   但**在到达之前**它已经能骗过一堆按字符串比的检查了。
    if decoded.contains('\0') {
        return None;
    }
    let trailing_slash = decoded.ends_with('/');
    let mut segments: Vec<String> = Vec::new();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // ★ 越界即拒，**不是**默默停在根上。
                //   悄悄夹住等于把一次穿越尝试变成一次正常请求。
                segments.pop()?;
            }
            s => segments.push(s.to_string()),
        }
    }
    Some(Normalized {
        segments,
        trailing_slash,
    })
}

/// 百分号解码。写法坏了（`%` 后面不是两位十六进制）→ `None`。
///
/// ⚠ 解码出来的字节按 UTF-8 拼回字符串；不是合法 UTF-8 → `None`。
/// 路径在本进程里全程是 `String`，放一个非 UTF-8 进去只会把问题推到更远的地方。
fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hi = hex(*b.get(i + 1)?)?;
            let lo = hex(*b.get(i + 2)?)?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 任一路径段命中 hide 清单？（G88：**按段**比，不是按前缀。）
///
/// ★ `/x/.git/config` 命中 `.git`；而 `.gitlab-ci.yml` **不该**被 `.git` 命中 ——
/// 这正是「按段」与「按前缀」的差别，也是这条规则唯一容易写错的地方。
pub fn is_hidden(segments: &[String], hide: &[String]) -> bool {
    segments.iter().any(|s| hide.iter().any(|h| h == s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> Option<Vec<String>> {
        decode_and_normalize(s).map(|n| n.segments)
    }

    #[test]
    fn 普通路径原样切段() {
        assert_eq!(
            norm("/a/b/c.txt"),
            Some(vec!["a".into(), "b".into(), "c.txt".into()])
        );
        assert_eq!(norm("/"), Some(vec![]));
        assert_eq!(norm(""), Some(vec![]));
    }

    #[test]
    fn 空段与点段被丢掉() {
        assert_eq!(
            norm("/a//b/./c"),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn 双点回退一级() {
        assert_eq!(norm("/a/b/../c"), Some(vec!["a".into(), "c".into()]));
        assert_eq!(norm("/a/b/.."), Some(vec!["a".into()]));
    }

    // ★ ★ 越界即拒，不是夹在根上。
    #[test]
    fn 双点越界即拒() {
        assert_eq!(norm("/.."), None);
        assert_eq!(norm("/a/../.."), None);
        assert_eq!(norm("/../etc/passwd"), None);
    }

    // ★ ★ ★ 这条钉的是**顺序**：解码必须在归一之前。
    //   ⚠ 反过来的话 `%2e%2e%2f` 在归一那步根本不长得像 `../`，
    //   会被当成一个普通的段名放过去，解码之后才变成穿越 —— 而那时已经没人看它了。
    #[test]
    fn 编码过的双点也要被归一看见() {
        assert_eq!(norm("/a/%2e%2e/b"), Some(vec!["b".into()]));
        assert_eq!(norm("/%2e%2e/etc"), None);
        assert_eq!(norm("/%2E%2E/etc"), None, "十六进制大写也一样");
    }

    // ⚠ 编码过的斜杠当分隔符 —— 只有这样 `..` 才藏不住。
    #[test]
    fn 编码过的斜杠当分隔符() {
        assert_eq!(norm("/a%2fb"), Some(vec!["a".into(), "b".into()]));
        assert_eq!(norm("/a%2f..%2f..%2fetc"), None);
    }

    #[test]
    fn 百分号写法坏了就拒() {
        assert_eq!(norm("/a%"), None);
        assert_eq!(norm("/a%2"), None);
        assert_eq!(norm("/a%zz"), None);
        assert_eq!(norm("/a%2g"), None);
    }

    #[test]
    fn nul_直接拒() {
        assert_eq!(norm("/a%00b"), None);
        assert_eq!(norm("/%00"), None);
    }

    #[test]
    fn 非_utf8_的解码结果拒掉() {
        assert_eq!(norm("/%ff%fe"), None);
    }

    #[test]
    fn 正常的中文路径解得开() {
        assert_eq!(
            norm("/%E4%B8%AD%E6%96%87.txt"),
            Some(vec!["中文.txt".into()])
        );
    }

    #[test]
    fn 尾斜杠被记下来() {
        assert!(decode_and_normalize("/a/").unwrap().trailing_slash);
        assert!(!decode_and_normalize("/a").unwrap().trailing_slash);
        assert!(decode_and_normalize("/").unwrap().trailing_slash);
    }

    // ★ ★ G88 的那一格：按段比，不是按前缀比。
    #[test]
    fn hide_按路径段比而不是按前缀比() {
        let hide = vec![".git".to_string(), ".env".to_string()];
        let seg = |s: &str| decode_and_normalize(s).unwrap().segments;

        assert!(is_hidden(&seg("/.git/config"), &hide), "根下的 .git 要命中");
        assert!(
            is_hidden(&seg("/x/.git/config"), &hide),
            "深处的 .git 也要命中"
        );
        assert!(is_hidden(&seg("/.git"), &hide), ".git 本身要命中");
        assert!(is_hidden(&seg("/a/.env"), &hide));

        // ⚠ ⚠ 这两条是「按前缀」写法唯一会栽的地方 —— 而按前缀写完全跑得通，
        //   只是会把两个正当文件挡成 404。
        assert!(
            !is_hidden(&seg("/.gitlab-ci.yml"), &hide),
            ".gitlab-ci.yml 不该被 .git 命中"
        );
        assert!(
            !is_hidden(&seg("/.environment"), &hide),
            ".environment 不该被 .env 命中"
        );
        assert!(
            !is_hidden(&seg("/git/config"), &hide),
            "不带点的 git 不该命中"
        );
    }

    // ★ 编码过的 hide 也要挡住 —— 解码在前，所以这是免费的；
    //   钉一条是因为「先查 hide 再解码」是另一种很自然、而且是**错**的写法。
    #[test]
    fn 编码过的_hide_段也挡得住() {
        let hide = vec![".git".to_string()];
        assert!(is_hidden(
            &decode_and_normalize("/%2egit/config").unwrap().segments,
            &hide
        ));
    }

    #[test]
    fn 空清单谁都不挡() {
        assert!(!is_hidden(&["a".to_string(), ".git".to_string()], &[]));
    }
}
