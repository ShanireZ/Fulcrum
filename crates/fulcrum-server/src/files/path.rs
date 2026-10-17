//! URL 路徑 → 磁碟路徑的那幾步，**全是純函式**（G87 / G88）。
//!
//! ★ ★ ★ 這幾步的**順序本身就是判據**，寫反了等於沒做：
//!
//! 1. **先百分號解碼** —— 不然 `%2e%2e%2f` 根本不長得像 `../`，
//!    後面那步歸一會原樣放過去。⇒ 解碼**在歸一之前**。
//! 2. **再詞法歸一** —— 按 `/` 切段、丟 `.`、`..` 退回一級，退回到根之外就拒。
//!    ⚠ 這一步是**純詞法**的，不碰磁碟：磁碟那側的符號連結由 G87 單管。
//! 3. **再查 hide 清單** —— 按**路徑段**比，命中回 404（不是 403）。
//!
//! ⚠ ⚠ `%2f`（編碼過的斜槓）解碼之後**當分隔符**用。這是有意的：
//! 只有把它當分隔符，`a%2f..%2fb` 裡那個 `..` 才會被第 2 步看見並處理。
//! 當成普通字元（"路徑裡真有個斜槓的檔案"）反而是那條經典繞過路。

use std::path::PathBuf;

/// 歸一之後的路徑。各段用 [`Normalized::segments`] 取。
#[derive(Debug, Clone)]
pub struct Normalized<'a> {
    segs: Segs<'a>,
    /// 原始 URL 末尾有沒有 `/`。★ 目錄的 301 判據要用它，
    /// 而它在歸一之後就看不出來了 —— 所以在這裡就記下來。
    pub trailing_slash: bool,
}

#[derive(Debug, Clone)]
enum Segs<'a> {
    /// URL 裡這一段原樣就是歸一好的 ⇒ 直接借，按 `/` 切、丟空段與 `.` 段。
    Borrowed(&'a str),
    /// 解碼或退回過的，各段各自一份。
    Owned(Vec<String>),
}

impl<'a> Normalized<'a> {
    /// 歸一後的各段，**不含空段**。根目錄本身一段都沒有。
    pub fn segments(&self) -> SegIter<'_> {
        match &self.segs {
            Segs::Borrowed(s) => SegIter::Borrowed(s.split('/')),
            Segs::Owned(v) => SegIter::Owned(v.iter()),
        }
    }
}

/// 兩種表示之間按「段與尾斜槓」比，⛔ 不按表示比 —— 同一條路徑走哪條路得出來都該相等。
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

/// 解碼 + 歸一。回傳 `None` = **拒**（400）。
///
/// 拒的三種情況：百分號寫法壞了、解碼出 `%00`、`..` 退回到根之外。
pub fn decode_and_normalize(raw: &str) -> Option<Normalized<'_>> {
    // ★ 快路徑（10-14 · 檔案那條路剩下的分配）：沒有 `%`（不用解碼）、沒有 NUL、沒有 `..` 段（不用退回）
    //   ⇒ 下面那條慢路徑對它做的事只剩「按 `/` 切、丟空段與 `.` 段」，而那件事藉著原串就能做
    //   ⇒ 一次都不分配。三個條件缺一個就走慢路徑，拒與不拒、切出來的段都與慢路徑逐個相同
    //   （判據：本檔案的 `快路径与慢路径逐个相同`，窮舉一個小字母表上的全部短串）。
    if !raw.contains(['%', '\0']) && !raw.split('/').any(|s| s == "..") {
        return Some(Normalized {
            segs: Segs::Borrowed(raw),
            trailing_slash: raw.ends_with('/'),
        });
    }
    decode_and_normalize_owned(raw)
}

/// 慢路徑 —— 也是 10-14 之前唯一的那條路，原樣未改：解碼、拒 NUL、按段歸一，各段各自一份。
fn decode_and_normalize_owned(raw: &str) -> Option<Normalized<'static>> {
    let decoded = percent_decode(raw)?;
    // ⚠ NUL 在這裡拒掉。它到不了 `open()`（Rust 的 `CString` 會拒），
    //   但**在到達之前**它已經能騙過一堆按字串比的檢查了。
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
                //   悄悄夾住等於把一次穿越嘗試變成一次正常請求。
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

/// 把歸一後的各段拼到站台根下。語義就是 `PathBuf::from(root)` 再逐段 `push` —— 用的就是同一個 `push`。
///
/// ★ 容量事先算夠（根 + 每段一個分隔符與段本身）⇒ 一次分配、⛔ 不讓 `push` 一段段地長
///   （原來那樣每段要長兩次：分隔符一次、段一次）。
pub fn join_under(root: &str, norm: &Normalized<'_>) -> PathBuf {
    let cap = root.len() + norm.segments().map(|s| s.len() + 1).sum::<usize>();
    let mut full = PathBuf::with_capacity(cap);
    full.push(root);
    for s in norm.segments() {
        full.push(s);
    }
    full
}

/// 百分號解碼。寫法壞了（`%` 後面不是兩位十六進制）→ `None`。
///
/// ⚠ 解碼出來的位元組按 UTF-8 拼回字串；不是合法 UTF-8 → `None`。
/// 路徑在本處理程序裡全程是 `String`，放一個非 UTF-8 進去只會把問題推到更遠的地方。
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

/// 任一路徑段命中 hide 清單？（G88：**按段**比，不是按前綴。）
///
/// ★ `/x/.git/config` 命中 `.git`；而 `.gitlab-ci.yml` **不該**被 `.git` 命中 ——
/// 這正是「按段」與「按前綴」的差別，也是這條規則唯一容易寫錯的地方。
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

    // ★ ★ 越界即拒，不是夾在根上。
    #[test]
    fn 双点越界即拒() {
        assert_eq!(norm("/.."), None);
        assert_eq!(norm("/a/../.."), None);
        assert_eq!(norm("/../etc/passwd"), None);
    }

    // ★ ★ ★ 這條釘的是**順序**：解碼必須在歸一之前。
    //   ⚠ 反過來的話 `%2e%2e%2f` 在歸一那步根本不長得像 `../`，
    //   會被當成一個普通的段名放過去，解碼之後才變成穿越 —— 而那時已經沒人看它了。
    #[test]
    fn 编码过的双点也要被归一看见() {
        assert_eq!(norm("/a/%2e%2e/b"), Some(vec!["b".into()]));
        assert_eq!(norm("/%2e%2e/etc"), None);
        assert_eq!(norm("/%2E%2E/etc"), None, "十六进制大写也一样");
    }

    // ⚠ 編碼過的斜槓當分隔符 —— 只有這樣 `..` 才藏不住。
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

    /// ★ 快路徑的判據：在一個夠刁的小字母表上**窮舉全部短串**，快路徑（`decode_and_normalize`）
    ///   與慢路徑（原來那條，`decode_and_normalize_owned`）拒與不拒、切出來的段、尾斜槓逐個相同。
    ///   字母表含分隔符、點、百分號與它能拼出的 `%2e` / `%2f` / `%00`、NUL、普通字母 ⇒ `..`、`.`、
    ///   空段、編碼過的穿越、壞的百分號、NUL 這幾類都在裡面。另外自證快路徑真的被走到過（⛔ 不是恆走慢路徑）。
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

    // ★ ★ G88 的那一格：按段比，不是按前綴比。
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

        // ⚠ ⚠ 這兩條是「按前綴」寫法唯一會栽的地方 —— 而按前綴寫完全跑得通，
        //   只是會把兩個正當檔案擋成 404。
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

    // ★ 編碼過的 hide 也要擋住 —— 解碼在前，所以這是免費的；
    //   釘一條是因為「先查 hide 再解碼」是另一種很自然、而且是**錯**的寫法。
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
