//! 主機名匹配的**唯一**一份語義（D18 / G66）。
//!
//! # ★ ★ 為什麼這個三行函式值得單獨一個模組
//!
//! 「`*.example.com` 覆蓋誰」有**兩個**消費者：站台索引（判錯 ⇒ 路由到不該去的站台）
//! 與 SNI 解析器（判錯 ⇒ 挑了一張用戶端會拒絕的證書）。
//! 兩邊各寫一份而且不一樣的話（一個按 RFC 6125 只吃一層，一個是 `ends_with`），
//! 於是 `a.b.example.com` 被路由到通配站台然後拿不到證書 —— 現場是一次握手失敗，
//! 而設定裡沒有任何一行看得出問題（D18）。
//!
//! ⚠ 修法不是「兩邊都改對」再用契約測試釘住：**兩份一致不代表它對，而且它們會在
//! 下一次改動時分家**。只留一份 ⇒ **分家變成結構上做不到的事**。

/// `*.example.com`（這裡傳的是去掉 `*` 之後的 `.example.com`）覆蓋誰。
///
/// 覆蓋 `a.example.com`；**不覆蓋** `example.com`（裸域），也**不覆蓋**
/// `a.b.example.com`（多一層）。
///
/// ★ 「只吃一層」是 **RFC 6125 與瀏覽器的實際行為**，也是 Caddy 的行為。
/// ⚠ 放寬成後綴匹配的後果兩側不同且都難查：證書側是伺服器端挑了一張用戶端會拒絕的證書；
/// 站台索引側是請求被路由到一個使用者沒打算讓它去的站台。
///
/// ★ 大小寫由**呼叫方**負責 —— 在這裡再轉一次只會掩蓋「誰忘了轉」。
///
/// ```
/// use fulcrum_config::host::wildcard_covers;
/// assert!(wildcard_covers(".example.com", "a.example.com"));
/// assert!(!wildcard_covers(".example.com", "example.com"));
/// assert!(!wildcard_covers(".example.com", "a.b.example.com"));
/// ```
/// 拆一條 `resolvers` 裡的位址：`<IPv4 或主机名>[:端口]`，不帶連接埠補 `:53`。
///
/// # ★ ★ 為什麼這件事必須**兩邊共用同一份**
///
/// 編譯期要判「寫得對不對」，執行環境要拿它去連 —— 而這兩處一旦各寫一份，
/// 遲早會在某個邊角上分家：那時**設定能編過、執行環境卻用不了**，
/// 現場表現是「DNS-01 不啟用」，而設定看起來完全正常。
///
/// # ⚠ 這裡**只判形狀，不做解析（resolve）**
///
/// 判形狀不需要網路，所以它可以在編譯期跑；真去解析主機名要網路，
/// 而**編譯期不許相依網路**（一次 DNS 抖動會讓一份沒改過的設定突然編不過）。
/// ⇒ 主機名什麼時候解析：**每次簽發的那一刻**（見 `fulcrum-acme` 的 `TxtChecker`）。
/// ★ 這一條與 G76 是同一個道理：把名字釘成啟動那一刻的 IP，是在賭它永遠不變，
/// 而 DNSPod 這類 anycast 權威的位址**是會變的**。
///
/// 回傳 `(主机或 IP, 端口)`；寫得不對時回傳一句**能直接印給使用者看**的話。
pub fn parse_resolver(raw: &str) -> Result<(String, u16), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("是空的".to_string());
    }
    // ⚠ IPv6 字面值單獨點名：M1 不支援（與站台位址那一條一致），
    //   而如果不點名，它會掉進「主機名裡有非法字元」那句裡，指錯方向。
    if raw.contains('[') || raw.matches(':').count() > 1 {
        return Err("看起来是 IPv6 字面量 —— M1 还不支持（与站点地址那一条一致）".to_string());
    }
    let (host, port) = match raw.split_once(':') {
        None => (raw, 53u16),
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| format!("端口 `{p}` 不是 1–65535 的数字"))?;
            if port == 0 {
                return Err("端口不能是 0".to_string());
            }
            (h, port)
        }
    };
    if host.is_empty() {
        return Err("冒号前面没有主机名或 IP".to_string());
    }
    // IPv4 字面值：直接放行。
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Ok((host.to_string(), port));
    }
    // 否則按主機名判：總長 ≤253，每段 1–63，只有字母數字與連字元，且不以連字元開頭/結尾。
    if host.len() > 253 {
        return Err("主机名太长（超过 253 个字符）".to_string());
    }
    let host_no_dot = host.strip_suffix('.').unwrap_or(host);
    if host_no_dot.is_empty() {
        return Err("主机名只有一个点".to_string());
    }
    for label in host_no_dot.split('.') {
        if label.is_empty() {
            return Err("主机名里有空的一段（连着两个点，或者以点开头）".to_string());
        }
        if label.len() > 63 {
            return Err(format!("主机名里的 `{label}` 超过 63 个字符"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!("主机名里的 `{label}` 不能以连字符开头或结尾"));
        }
        if let Some(bad) = label
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && *c != '-')
        {
            return Err(format!("主机名里有不该出现的字符 `{bad}`"));
        }
    }
    // ⚠ 純數字的最後一段說明這是個寫壞的 IP（比如 `1.2.3`），不是主機名 ——
    //   放它過去，執行環境會去解析一個根本不存在的名字，而錯誤指向 DNS。
    if host_no_dot
        .rsplit('.')
        .next()
        .is_some_and(|last| last.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "`{host}` 像是写坏的 IP（最后一段是纯数字），既不是合法 IPv4 也不像主机名"
        ));
    }
    Ok((host.to_string(), port))
}

pub fn wildcard_covers(suffix_with_dot: &str, name: &str) -> bool {
    let Some(label) = name.strip_suffix(suffix_with_dot) else {
        return false;
    };
    // 剩下的必須正好是一個非空標籤（不含點）。
    !label.is_empty() && !label.contains('.')
}

#[cfg(test)]
mod tests {

    // ── `resolvers` 的形狀（批 20）─────────────────────────────────

    #[test]
    fn resolvers_认_ip_也认主机名() {
        // ★ 認主機名這件事本身就是判據：在此之前只認 IP:port，
        //   而 DSL 參考 §4.4 的範例寫的就是主機名 —— 文件與實作對不上。
        assert_eq!(
            parse_resolver("1.2.3.4"),
            Ok(("1.2.3.4".to_string(), 53)),
            "不带端口要补 53"
        );
        assert_eq!(
            parse_resolver("1.2.3.4:5353"),
            Ok(("1.2.3.4".to_string(), 5353))
        );
        assert_eq!(
            parse_resolver("drummer.dnspod.net"),
            Ok(("drummer.dnspod.net".to_string(), 53))
        );
        assert_eq!(
            parse_resolver("drummer.dnspod.net:53"),
            Ok(("drummer.dnspod.net".to_string(), 53))
        );
        // 尾點是合法的絕對網域寫法
        assert!(parse_resolver("ns.example.com.").is_ok());
    }

    #[test]
    fn resolvers_写坏了要在编译期就说清楚哪里坏() {
        // ⚠ 每一條都要**指向具體哪裡不對**，不能只說「不合法」——
        //   在此之前這一整類錯誤的現場是「本站台的 DNS-01 不會啟用」，
        //   而 validate 結束代碼還是 0。
        for (raw, expect_in_msg) in [
            ("", "空"),
            ("1.2.3", "写坏的 IP"),
            ("ns.example.com:99999", "1–65535"),
            ("ns.example.com:0", "0"),
            ("[2001:db8::1]:53", "IPv6"),
            ("2001:db8::1", "IPv6"),
            ("-bad.example.com", "连字符"),
            ("a..b.example.com", "空的一段"),
            ("ns_1.example.com", "不该出现的字符"),
            (":53", "冒号前面"),
        ] {
            let e = parse_resolver(raw).expect_err(&format!("`{raw}` 本该被判错"));
            assert!(
                e.contains(expect_in_msg),
                "`{raw}` 的错误里没提 `{expect_in_msg}`：{e}"
            );
        }
    }

    #[test]
    fn resolvers_的形状判据不碰网络() {
        // ★ ★ 這一條釘的是**判據的性質**，不是某個回傳值：形狀檢核跑在編譯期，
        //   而編譯期不許相依網路 —— 一次 DNS 抖動不該讓一份沒改過的設定突然編不過。
        //   ⇒ 一個**肯定解析不出來**的名字，形狀上必須是**合法**的。
        assert!(
            parse_resolver("this-name-does-not-exist.invalid").is_ok(),
            "形状校验去解析了 —— 那会把网络故障变成编译错误"
        );
    }

    use super::*;

    #[test]
    fn 只吃一层() {
        let s = ".example.com";
        assert!(wildcard_covers(s, "a.example.com"));
        assert!(
            wildcard_covers(s, "xn--fiqs8s.example.com"),
            "标签里可以有连字符"
        );
        // ★ 裸域不算。少了這一條，`example.com` 會悄悄落進通配站台。
        assert!(!wildcard_covers(s, "example.com"));
        // ★ 多一層也不算——這一條就是 D18 的全部內容。
        assert!(!wildcard_covers(s, "a.b.example.com"));
        assert!(!wildcard_covers(s, "a.b.c.example.com"));
        // 完全不沾邊
        assert!(!wildcard_covers(s, "other.org"));
        assert!(!wildcard_covers(s, ""));
    }

    #[test]
    fn 不做字符串包含那种误判() {
        // ⚠ `notexample.com` 以 `example.com` 結尾，但它**不在**那個域下面。
        //   點號是判據的一部分，不是裝飾：呼叫方傳進來的必須是帶前導點的後綴。
        assert!(!wildcard_covers(".example.com", "a.notexample.com"));
        assert!(!wildcard_covers(".example.com", "notexample.com"));
        // 反向：真的在那個域下面就要中。
        assert!(wildcard_covers(".notexample.com", "a.notexample.com"));
    }

    #[test]
    fn 大小写由调用方负责() {
        // ★ 這條斷言記錄的是**契約**，不是缺陷：本函式按位元組比。
        //   兩個呼叫點都在調之前把 host 轉成了小寫；在這裡再轉一次，
        //   反而會讓「某個新呼叫點忘了轉」永遠不被發現。
        assert!(!wildcard_covers(".example.com", "A.EXAMPLE.COM"));
        assert!(wildcard_covers(
            ".example.com",
            "A.EXAMPLE.COM".to_ascii_lowercase().as_str()
        ));
    }
}
