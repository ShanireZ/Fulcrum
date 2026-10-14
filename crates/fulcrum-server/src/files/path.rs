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

use std::path::PathBuf;

/// 归一之后的路径。各段用 [`Normalized::segments`] 取。
#[derive(Debug, Clone)]
pub struct Normalized<'a> {
    segs: Segs<'a>,
    /// 原始 URL 末尾有没有 `/`。★ 目录的 301 判据要用它，
    /// 而它在归一之后就看不出来了 —— 所以在这里就记下来。
    pub trailing_slash: bool,
}

#[derive(Debug, Clone)]
enum Segs<'a> {
    /// URL 里这一段原样就是归一好的 ⇒ 直接借，按 `/` 切、丢空段与 `.` 段。
    Borrowed(&'a str),
    /// 解码或回退过的，各段各自一份。
    Owned(Vec<String>),
}

impl<'a> Normalized<'a> {
    /// 归一后的各段，**不含空段**。根目录本身一段都没有。
    pub fn segments(&self) -> SegIter<'_> {
        match &self.segs {
            Segs::Borrowed(s) => SegIter::Borrowed(s.split('/')),
            Segs::Owned(v) => SegIter::Owned(v.iter()),
        }
    }
}

/// 两种表示之间按「段与尾斜杠」比，⛔ 不按表示比 —— 同一条路径走哪条路得出来都该相等。
impl PartialEq for Normalized<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.trailing_slash == other.trailing_slash && self.segments().eq(other.segments())
    }
}

impl Eq for Normalized<'_> {}

/// [`Normalized::segments`] 的迭代器。
pub enum SegIter<'s> {
    Borrowed(std::str::Split<'s, char>),
    Owned(std::slice::Iter<'s, String>),
}

impl<'s> Iterator for SegIter<'s> {
    type Item = &'s str;

    fn next(&mut self) -> Option<&'s str> {
        match self {
            SegIter::Borrowed(it) => it.find(|s| !s.is_empty() && *s != "."),
            SegIter::Owned(it) => it.next().map(String::as_str),
        }
    }
}

/// 解码 + 归一。返回 `None` = **拒**（400）。
///
/// 拒的三种情况：百分号写法坏了、解码出 `%00`、`..` 回退到根之外。
pub fn decode_and_normalize(raw: &str) -> Option<Normalized<'_>> {
    // ★ 快路径（10-14 · 文件那条路剩下的分配）：没有 `%`（不用解码）、没有 NUL、没有 `..` 段（不用回退）
    //   ⇒ 下面那条慢路径对它做的事只剩「按 `/` 切、丢空段与 `.` 段」，而那件事借着原串就能做
    //   ⇒ 一次都不分配。三个条件缺一个就走慢路径，拒与不拒、切出来的段都与慢路径逐个相同
    //   （判据：本文件的 `快路径与慢路径逐个相同`，穷举一个小字母表上的全部短串）。
    if !raw.contains(['%', '\0']) && !raw.split('/').any(|s| s == "..") {
        return Some(Normalized {
            segs: Segs::Borrowed(raw),
            trailing_slash: raw.ends_with('/'),
        });
    }
    decode_and_normalize_owned(raw)
}

/// 慢路径 —— 也是 10-14 之前唯一的那条路，原样未改：解码、拒 NUL、按段归一，各段各自一份。
fn decode_and_normalize_owned(raw: &str) -> Option<Normalized<'static>> {
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
        segs: Segs::Owned(segments),
        trailing_slash,
    })
}

/// 把归一后的各段拼到站点根下。语义就是 `PathBuf::from(root)` 再逐段 `push` —— 用的就是同一个 `push`。
///
/// ★ 容量事先算够（根 + 每段一个分隔符与段本身）⇒ 一次分配、⛔ 不让 `push` 一段段地长
///   （原来那样每段要长两次：分隔符一次、段一次）。
pub fn join_under(root: &str, norm: &Normalized<'_>) -> PathBuf {
    let cap = root.len() + norm.segments().map(|s| s.len() + 1).sum::<usize>();
    let mut full = PathBuf::with_capacity(cap);
    full.push(root);
    for s in norm.segments() {
        full.push(s);
    }
    full
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
pub fn is_hidden(norm: &Normalized<'_>, hide: &[String]) -> bool {
    norm.segments().any(|s| hide.iter().any(|h| h == s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> Option<Vec<String>> {
        decode_and_normalize(s).map(|n| n.segments().map(String::from).collect())
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

    /// ★ 快路径的判据：在一个够刁的小字母表上**穷举全部短串**，快路径（`decode_and_normalize`）
    ///   与慢路径（原来那条，`decode_and_normalize_owned`）拒与不拒、切出来的段、尾斜杠逐个相同。
    ///   字母表含分隔符、点、百分号与它能拼出的 `%2e` / `%2f` / `%00`、NUL、普通字母 ⇒ `..`、`.`、
    ///   空段、编码过的穿越、坏的百分号、NUL 这几类都在里面。另外自证快路径真的被走到过（⛔ 不是恒走慢路径）。
    #[test]
    fn 快路径与慢路径逐个相同() {
        const ALPHA: &[char] = &['/', '.', '%', '2', 'e', 'f', '0', 'a', '\0'];
        let mut fast = 0usize;
        let mut cur = String::new();
        fn walk(cur: &mut String, depth: usize, fast: &mut usize) {
            let got = decode_and_normalize(cur);
            let want = decode_and_normalize_owned(cur);
            assert_eq!(got, want, "输入 {cur:?}");
            if matches!(
                got,
                Some(Normalized {
                    segs: Segs::Borrowed(_),
                    ..
                })
            ) {
                *fast += 1;
            }
            if depth == 0 {
                return;
            }
            for &c in ALPHA {
                cur.push(c);
                walk(cur, depth - 1, fast);
                cur.pop();
            }
        }
        walk(&mut cur, 6, &mut fast);
        assert!(fast > 1000, "快路径只走到 {fast} 次 —— 多半恒走慢路径了");
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
        let seg = |s| decode_and_normalize(s).unwrap();

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
            &decode_and_normalize("/%2egit/config").unwrap(),
            &hide
        ));
    }

    #[test]
    fn 空清单谁都不挡() {
        assert!(!is_hidden(&decode_and_normalize("/a/.git").unwrap(), &[]));
    }
}
