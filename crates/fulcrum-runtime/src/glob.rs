//! 两个小原语：路径通配与 CIDR 归属。
//!
//! ★ 两者都**没有引依赖**，理由不同：
//! - 通配只需要 `*` 一种元字符（DSL 参考 §五），拉一个 glob crate 进来是用大炮打蚊子，
//!   而 G29 那套「追新 + 24 小时隔离 + 每周检查」是按依赖条数收费的；
//! - CIDR 是四十行位运算，而它的正确性可以被穷尽地测出来。
//!
//! ⚠ **但「自己写」不等于「自己拍脑袋」**：下面两个函数各带一组反向用例
//! （必须不匹配的输入），因为这一类函数最常见的坏法是**过度匹配**——
//! 而过度匹配的 `remote_ip` 就是一条把内网规则套给公网客户端的安全缺陷。

use std::net::IpAddr;

/// `*` 通配匹配。`*` 匹配任意（含空）字符序列，其余字符必须逐字相等。
///
/// ★ 用贪婪回溯的双指针写法，不用递归：模式里 `*` 的个数由配置决定，
/// 递归深度就跟着配置走——**让外部输入决定栈深度是一类可被利用的形状**。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // 最近一个 `*` 的位置，以及它当时匹配到 text 的哪里。回溯就回到这里。
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            // 让那个 `*` 多吃一个字符再试
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    // text 走完了，模式剩下的必须全是 `*`
    p[pi..].iter().all(|c| *c == '*')
}

/// 一个 CIDR 网段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    base: IpAddr,
    /// 前缀长度（位）。
    prefix: u8,
}

impl Cidr {
    /// 解析 `10.0.0.0/8` / `2001:db8::/32` / 裸地址（当成 /32 或 /128）。
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

    /// 某个地址是否落在本网段内。
    ///
    /// ⚠ **v4 与 v6 不互通**：`10.0.0.0/8` 不包含任何 v6 地址，
    /// 反之亦然。★ 这一条要显式写出来——把 v4 映射成 `::ffff:a.b.c.d` 再比，
    /// 会让一条 `remote_ip 10.0.0.0/8` 规则**意外命中 v6 客户端**。
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
    // 高 rest 位的掩码。★ `0xFFu8 << (8 - rest)` 在 rest==0 时会是 `<< 8`（溢出），
    //   所以上面必须先把 rest==0 挡掉——这不是多余的分支。
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
        // ★ 反向用例是重点：这类函数最常见的坏法是过度匹配。
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
        // 病态输入：一长串 `*` 后跟一个不可能匹配的字符。
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

        // 非 8 的倍数，走掩码那条路
        let c = Cidr::parse("192.168.1.0/23").unwrap();
        assert!(c.contains("192.168.0.5".parse().unwrap()));
        assert!(c.contains("192.168.1.5".parse().unwrap()));
        assert!(!c.contains("192.168.2.5".parse().unwrap()));

        // /32 与裸地址
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
        // ★ 这一条防的是一条真实的安全缺陷：内网规则意外命中 v6 客户端。
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
