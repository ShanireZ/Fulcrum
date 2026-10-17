//! 「這條記錄名落在被宣告的哪個 zone 裡」—— **G59 第 3 條**的判定本體。
//!
//! # ★ 為什麼它單獨一個模組
//!
//! Cloudflare 與 DNSPod 兩家都要問同一個問題。**抄兩份是必然分叉**，
//! 而分叉之後**兩邊都還是綠的**——這正是 D18/G66 那次的處置理由：
//! 讓分家在結構上做不到，比讓兩份互相釘著更可靠。
//!
//! # ⚠ ⚠ 判據必須按**標籤邊界**判，不能用 `ends_with`
//!
//! `"evilexample.net".ends_with("example.net")` 是 `true`。
//! 而這條判定存在的**全部理由**，就是不讓一份憑證被用在沒宣告的域上——
//! 用後綴匹配等於把這條理由本身抹掉，且沒有任何症狀。
//!
//! ★ 同一族的坑在站台索引上發生過（那裡的通配符也曾是 `ends_with`）。

/// 找出 `record` 屬於 `zones` 裡的哪一個；不屬於任何一個就是 `None`。
///
/// ★ 同時宣告了父子 zone（`a.com` 與 `x.a.com`）時**取最長的那個**——
/// 否則 `_acme-challenge.x.a.com` 會被算到 `a.com` 名下，
/// 而那兩份憑證可能根本不是同一份。
pub fn zone_for<'a>(zones: &'a [String], record: &str) -> Option<&'a str> {
    let r = record.trim_end_matches('.').to_ascii_lowercase();
    zones
        .iter()
        .filter(|z| r == **z || r.ends_with(&format!(".{z}")))
        .max_by_key(|z| z.len())
        .map(|z| z.as_str())
}

/// 越權時那條錯誤。★ 把「宣告了哪些」一起說出來——
/// 一個只說「越權」的錯誤，等於讓人去猜是記錄名寫錯了還是 zone 漏配了。
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
        // ★ ★ 這一條是本模組存在的理由。`ends_with` 會讓它變成 Some。
        assert_eq!(zone_for(&zones, "evilexample.net"), None);
        assert_eq!(zone_for(&zones, "example.net.evil.com"), None);
        assert_eq!(zone_for(&zones, "example.com"), None);
    }

    #[test]
    fn 大小写与末尾那个点都不影响() {
        let zones = z(&["Example.NET"]);
        // ⚠ 呼叫方存進來時已經小寫化了；這裡再驗一次「記錄名側」的大小寫與根點。
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
        // ⚠ 判據的反面：一個「空清單＝全放行」的實作會讓 G59 第 3 條整條失效，
        //   而它在有宣告時表現完全正常。
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
