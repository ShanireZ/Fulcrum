//! 兩個小原語：路徑通配與 CIDR 歸屬。
//!
//! ★ 兩者都**沒有引依賴**，理由不同：
//! - 通配只需要 `*` 一種元字元（DSL 參考 §五），拉一個 glob crate 進來是用大炮打蚊子，
//!   而 G29 那套「追新 + 24 小時隔離 + 每週檢查」是按依賴條數收費的；
//! - CIDR 是四十行位運算，而它的正確性可以被窮盡地測出來。
//!
//! ⚠ **但「自己寫」不等於「自己拍腦袋」**：下面兩個函式各帶一組反向用例
//! （必須不匹配的輸入），因為這一類函式最常見的壞法是**過度匹配**——
//! 而過度匹配的 `remote_ip` 就是一條把內網規則套給公用網路用戶端的安全缺陷。

use std::net::IpAddr;

/// `*` 通配匹配。`*` 匹配任意（含空）字元序列，其餘字元必須逐字相等。
///
/// ★ 用貪婪回溯的雙指針寫法，不用遞歸：模式裡 `*` 的個數由設定決定，
/// 遞歸深度就跟著設定走——**讓外部輸入決定棧深度是一類可被利用的形狀**。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // 最近一個 `*` 的位置，以及它當時匹配到 text 的哪裡。回溯就回到這裡。
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            // 讓那個 `*` 多吃一個字元再試
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    // text 走完了，模式剩下的必須全是 `*`
    p[pi..].iter().all(|c| *c == '*')
}

/// 一個 CIDR 網段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    base: IpAddr,
    /// 前綴長度（位）。
    prefix: u8,
}

impl Cidr {
    /// 解析 `10.0.0.0/8` / `2001:db8::/32` / 裸位址（當成 /32 或 /128）。
    pub fn parse(s: &str) -> Option<Cidr> {
        let (addr_part, prefix_part) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let base: IpAddr = addr_part.parse().ok()?;
        let max = if base.is_ipv4() { 32u8 } else { 128u8 };
        let prefix = match prefix_part {
            None => max,
            Some(p) => {
                let n: u8 = p.parse().ok()?;
                if n > max {
                    return None;
                }
                n
            }
        };
        Some(Cidr { base, prefix })
    }

    /// 某個位址是否落在本網段內。
    ///
    /// ⚠ **v4 與 v6 不互通**：`10.0.0.0/8` 不包含任何 v6 位址，
    /// 反之亦然。★ 這一條要顯式寫出來——把 v4 映射成 `::ffff:a.b.c.d` 再比，
    /// 會讓一條 `remote_ip 10.0.0.0/8` 規則**意外命中 v6 用戶端**。
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.base, ip) {
            (IpAddr::V4(b), IpAddr::V4(x)) => prefix_eq(&b.octets(), &x.octets(), self.prefix),
            (IpAddr::V6(b), IpAddr::V6(x)) => prefix_eq(&b.octets(), &x.octets(), self.prefix),
            _ => false,
        }
    }
}

/// 前 `prefix` 位是否相同。
fn prefix_eq(a: &[u8], b: &[u8], prefix: u8) -> bool {
    let full = (prefix / 8) as usize;
    if a[..full] != b[..full] {
        return false;
    }
    let rest = prefix % 8;
    if rest == 0 {
        return true;
    }
    // 高 rest 位的掩碼。★ `0xFFu8 << (8 - rest)` 在 rest==0 時會是 `<< 8`（溢出），
    //   所以上面必須先把 rest==0 擋掉——這不是多餘的分支。
    let mask = 0xFFu8 << (8 - rest);
    (a[full] & mask) == (b[full] & mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_基本命中() {
        assert!(glob_match("/static/*", "/static/a.css"));
        assert!(glob_match("/static/*", "/static/"));
        assert!(glob_match("*", "/anything"));
        assert!(glob_match("*.png", "/img/a.png"));
        assert!(glob_match("/a/*/b", "/a/x/b"));
        assert!(glob_match("/a/*/b", "/a/x/y/b"));
        assert!(glob_match("/exact", "/exact"));
    }

    #[test]
    fn glob_必须不命中的那些() {
        // ★ 反向用例是重點：這類函式最常見的壞法是過度匹配。
        assert!(!glob_match("/static/*", "/other/a.css"));
        assert!(!glob_match("/exact", "/exact/more"));
        assert!(!glob_match("/exact", "/exac"));
        assert!(!glob_match("*.png", "/img/a.png.txt"));
        assert!(!glob_match("/a/*/b", "/a/x/c"));
        assert!(!glob_match("", "/x"));
        assert!(glob_match("", ""));
    }

    #[test]
    fn glob_多个星号不炸栈也不误判() {
        assert!(glob_match("/*/*/*", "/a/b/c"));
        assert!(!glob_match("/*/*/*/d", "/a/b/c"));
        // 病態輸入：一長串 `*` 後跟一個不可能匹配的字元。
        let pat = "*".repeat(64) + "z";
        assert!(!glob_match(&pat, &"a".repeat(200)));
        assert!(glob_match(&pat, &("a".repeat(200) + "z")));
    }

    #[test]
    fn cidr_解析与归属() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains("10.1.2.3".parse().unwrap()));
        assert!(c.contains("10.255.255.255".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert!(!c.contains("9.255.255.255".parse().unwrap()));

        // 非 8 的倍數，走掩碼那條路
        let c = Cidr::parse("192.168.1.0/23").unwrap();
        assert!(c.contains("192.168.0.5".parse().unwrap()));
        assert!(c.contains("192.168.1.5".parse().unwrap()));
        assert!(!c.contains("192.168.2.5".parse().unwrap()));

        // /32 與裸位址
        assert!(
            Cidr::parse("1.2.3.4")
                .unwrap()
                .contains("1.2.3.4".parse().unwrap())
        );
        assert!(
            !Cidr::parse("1.2.3.4")
                .unwrap()
                .contains("1.2.3.5".parse().unwrap())
        );
        // /0 全命中
        assert!(
            Cidr::parse("0.0.0.0/0")
                .unwrap()
                .contains("8.8.8.8".parse().unwrap())
        );
    }

    #[test]
    fn cidr_v4_与_v6_不互通() {
        // ★ 這一條防的是一條真實的安全缺陷：內網規則意外命中 v6 用戶端。
        let v4 = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(!v4.contains("::ffff:10.1.2.3".parse().unwrap()));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains("2001:db8::1".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));
        assert!(!v6.contains("10.1.2.3".parse().unwrap()));
    }

    #[test]
    fn cidr_坏输入被拒() {
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("2001:db8::/129").is_none());
        assert!(Cidr::parse("not-an-ip").is_none());
        assert!(Cidr::parse("10.0.0.0/x").is_none());
        assert!(Cidr::parse("").is_none());
    }
}
