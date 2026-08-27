//! 主机名匹配的**唯一**一份语义（D18 / G66）。
//!
//! # ★ ★ 为什么这个三行函数值得单独一个模块
//!
//! 「`*.example.com` 覆盖谁」有**两个**消费者：站点索引（判错 ⇒ 路由到不该去的站点）
//! 与 SNI 解析器（判错 ⇒ 挑了一张客户端会拒绝的证书）。
//! 两边各写一份而且不一样的话（一个按 RFC 6125 只吃一层，一个是 `ends_with`），
//! 于是 `a.b.example.com` 被路由到通配站点然后拿不到证书 —— 现场是一次握手失败，
//! 而配置里没有任何一行看得出问题（D18）。
//!
//! ⚠ 修法不是「两边都改对」再用契约测试钉住：**两份一致不代表它对，而且它们会在
//! 下一次改动时分家**。只留一份 ⇒ **分家变成结构上做不到的事**。

/// `*.example.com`（这里传的是去掉 `*` 之后的 `.example.com`）覆盖谁。
///
/// 覆盖 `a.example.com`；**不覆盖** `example.com`（裸域），也**不覆盖**
/// `a.b.example.com`（多一层）。
///
/// ★ 「只吃一层」是 **RFC 6125 与浏览器的实际行为**，也是 Caddy 的行为。
/// ⚠ 放宽成后缀匹配的后果两侧不同且都难查：证书侧是服务端挑了一张客户端会拒绝的证书；
/// 站点索引侧是请求被路由到一个用户没打算让它去的站点。
///
/// ★ 大小写由**调用方**负责 —— 在这里再转一次只会掩盖「谁忘了转」。
///
/// ```
/// use fulcrum_config::host::wildcard_covers;
/// assert!(wildcard_covers(".example.com", "a.example.com"));
/// assert!(!wildcard_covers(".example.com", "example.com"));
/// assert!(!wildcard_covers(".example.com", "a.b.example.com"));
/// ```
/// 拆一条 `resolvers` 里的地址：`<IPv4 或主机名>[:端口]`，不带端口补 `:53`。
///
/// # ★ ★ 为什么这件事必须**两边共用同一份**
///
/// 编译期要判「写得对不对」，运行时要拿它去连 —— 而这两处一旦各写一份，
/// 迟早会在某个边角上分家：那时**配置能编过、运行时却用不了**，
/// 现场表现是「DNS-01 不启用」，而配置看起来完全正常。
///
/// # ⚠ 这里**只判形状，不做解析（resolve）**
///
/// 判形状不需要网络，所以它可以在编译期跑；真去解析主机名要网络，
/// 而**编译期不许依赖网络**（一次 DNS 抖动会让一份没改过的配置突然编不过）。
/// ⇒ 主机名什么时候解析：**每次签发的那一刻**（见 `fulcrum-acme` 的 `TxtChecker`）。
/// ★ 这一条与 G76 是同一个道理：把名字钉成启动那一刻的 IP，是在赌它永远不变，
/// 而 DNSPod 这类 anycast 权威的地址**是会变的**。
///
/// 返回 `(主机或 IP, 端口)`；写得不对时返回一句**能直接印给用户看**的话。
pub fn parse_resolver(raw: &str) -> Result<(String, u16), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("是空的".to_string());
    }
    // ⚠ IPv6 字面量单独点名：M1 不支持（与站点地址那一条一致），
    //   而如果不点名，它会掉进「主机名里有非法字符」那句里，指错方向。
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
    // IPv4 字面量：直接放行。
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Ok((host.to_string(), port));
    }
    // 否则按主机名判：总长 ≤253，每段 1–63，只有字母数字与连字符，且不以连字符开头/结尾。
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
    // ⚠ 纯数字的最后一段说明这是个写坏的 IP（比如 `1.2.3`），不是主机名 ——
    //   放它过去，运行时会去解析一个根本不存在的名字，而错误指向 DNS。
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
    // 剩下的必须正好是一个非空标签（不含点）。
    !label.is_empty() && !label.contains('.')
}

#[cfg(test)]
mod tests {

    // ── `resolvers` 的形状（批 20）─────────────────────────────────

    #[test]
    fn resolvers_认_ip_也认主机名() {
        // ★ 认主机名这件事本身就是判据：在此之前只认 IP:port，
        //   而 DSL 参考 §4.4 的示例写的就是主机名 —— 文档与实现对不上。
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
        // 尾点是合法的绝对域名写法
        assert!(parse_resolver("ns.example.com.").is_ok());
    }

    #[test]
    fn resolvers_写坏了要在编译期就说清楚哪里坏() {
        // ⚠ 每一条都要**指向具体哪里不对**，不能只说「不合法」——
        //   在此之前这一整类错误的现场是「本站点的 DNS-01 不会启用」，
        //   而 validate 退出码还是 0。
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
        // ★ ★ 这一条钉的是**判据的性质**，不是某个返回值：形状校验跑在编译期，
        //   而编译期不许依赖网络 —— 一次 DNS 抖动不该让一份没改过的配置突然编不过。
        //   ⇒ 一个**肯定解析不出来**的名字，形状上必须是**合法**的。
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
        // ★ 裸域不算。少了这一条，`example.com` 会悄悄落进通配站点。
        assert!(!wildcard_covers(s, "example.com"));
        // ★ 多一层也不算——这一条就是 D18 的全部内容。
        assert!(!wildcard_covers(s, "a.b.example.com"));
        assert!(!wildcard_covers(s, "a.b.c.example.com"));
        // 完全不沾边
        assert!(!wildcard_covers(s, "other.org"));
        assert!(!wildcard_covers(s, ""));
    }

    #[test]
    fn 不做字符串包含那种误判() {
        // ⚠ `notexample.com` 以 `example.com` 结尾，但它**不在**那个域下面。
        //   点号是判据的一部分，不是装饰：调用方传进来的必须是带前导点的后缀。
        assert!(!wildcard_covers(".example.com", "a.notexample.com"));
        assert!(!wildcard_covers(".example.com", "notexample.com"));
        // 反向：真的在那个域下面就要中。
        assert!(wildcard_covers(".notexample.com", "a.notexample.com"));
    }

    #[test]
    fn 大小写由调用方负责() {
        // ★ 这条断言记录的是**契约**，不是缺陷：本函数按字节比。
        //   两个调用点都在调之前把 host 转成了小写；在这里再转一次，
        //   反而会让「某个新调用点忘了转」永远不被发现。
        assert!(!wildcard_covers(".example.com", "A.EXAMPLE.COM"));
        assert!(wildcard_covers(
            ".example.com",
            "A.EXAMPLE.COM".to_ascii_lowercase().as_str()
        ));
    }
}
