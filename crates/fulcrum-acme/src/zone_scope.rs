//! 「这条记录名落在被声明的哪个 zone 里」—— **G59 第 3 条**的判定本体。
//!
//! # ★ 为什么它单独一个模块
//!
//! Cloudflare 与 DNSPod 两家都要问同一个问题。**抄两份是必然分叉**，
//! 而分叉之后**两边都还是绿的**——这正是 D18/G66 那次的处置理由：
//! 让分家在结构上做不到，比让两份互相钉着更可靠。
//!
//! # ⚠ ⚠ 判据必须按**标签边界**判，不能用 `ends_with`
//!
//! `"evilexample.net".ends_with("example.net")` 是 `true`。
//! 而这条判定存在的**全部理由**，就是不让一份凭据被用在没声明的域上——
//! 用后缀匹配等于把这条理由本身抹掉，且没有任何症状。
//!
//! ★ 同一族的坑在站点索引上发生过（那里的通配符也曾是 `ends_with`）。

/// 找出 `record` 属于 `zones` 里的哪一个；不属于任何一个就是 `None`。
///
/// ★ 同时声明了父子 zone（`a.com` 与 `x.a.com`）时**取最长的那个**——
/// 否则 `_acme-challenge.x.a.com` 会被算到 `a.com` 名下，
/// 而那两份凭据可能根本不是同一份。
pub fn zone_for<'a>(zones: &'a [String], record: &str) -> Option<&'a str> {
    let r = record.trim_end_matches('.').to_ascii_lowercase();
    zones
        .iter()
        .filter(|z| r == **z || r.ends_with(&format!(".{z}")))
        .max_by_key(|z| z.len())
        .map(|z| z.as_str())
}

/// 越权时那条错误。★ 把「声明了哪些」一起说出来——
/// 一个只说「越权」的错误，等于让人去猜是记录名写错了还是 zone 漏配了。
pub fn out_of_scope(record: &str, zones: &[String]) -> String {
    format!(
        "{record} 不在这份凭据声明覆盖的 zone 里（声明的是 {}）—— G59 第 3 条：\
         超出声明范围一律拒绝，把「凭据能干什么」变成配置里可读的事实",
        if zones.is_empty() {
            "（空）".to_string()
        } else {
            zones.join(" ")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 按标签边界判而不是后缀() {
        let zones = z(&["example.net"]);
        assert_eq!(
            zone_for(&zones, "_acme-challenge.example.net"),
            Some("example.net")
        );
        assert_eq!(zone_for(&zones, "example.net"), Some("example.net"));
        assert_eq!(zone_for(&zones, "a.b.example.net"), Some("example.net"));
        // ★ ★ 这一条是本模块存在的理由。`ends_with` 会让它变成 Some。
        assert_eq!(zone_for(&zones, "evilexample.net"), None);
        assert_eq!(zone_for(&zones, "example.net.evil.com"), None);
        assert_eq!(zone_for(&zones, "example.com"), None);
    }

    #[test]
    fn 大小写与末尾那个点都不影响() {
        let zones = z(&["Example.NET"]);
        // ⚠ 调用方存进来时已经小写化了；这里再验一次「记录名侧」的大小写与根点。
        let zones: Vec<String> = zones.iter().map(|s| s.to_ascii_lowercase()).collect();
        assert_eq!(
            zone_for(&zones, "_ACME-Challenge.EXAMPLE.net."),
            Some("example.net")
        );
    }

    #[test]
    fn 父子_zone_都声明时取最长的() {
        let zones = z(&["a.com", "x.a.com"]);
        assert_eq!(zone_for(&zones, "_acme-challenge.x.a.com"), Some("x.a.com"));
        assert_eq!(zone_for(&zones, "_acme-challenge.y.a.com"), Some("a.com"));
    }

    #[test]
    fn 空清单谁都不覆盖() {
        // ⚠ 判据的反面：一个「空清单＝全放行」的实现会让 G59 第 3 条整条失效，
        //   而它在有声明时表现完全正常。
        assert_eq!(zone_for(&[], "anything.com"), None);
    }

    #[test]
    fn 越权错误里说得出声明了哪些() {
        let e = out_of_scope("x.example.com", &z(&["example.net"]));
        assert!(e.contains("G59"), "{e}");
        assert!(e.contains("example.net"), "要说出声明了哪些：{e}");
        assert!(out_of_scope("x", &[]).contains("（空）"));
    }
}
