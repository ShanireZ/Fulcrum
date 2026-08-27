//! `Cache-Control` 头的解析（RFC 9111 §5.2）。**纯函数**。
//!
//! ★ ★ G97 拍的是「最小集 **+ 上游响应头里的全套 `Cache-Control` 指令**」，
//! 所以这里把响应侧那一整张表都实现了，而不是只挑常见的几条。
//!
//! ⚠ ⚠ **两条最容易被写错、而且错了不会报错的规则：**
//!
//! 1. **`no-cache` 有两种形态，含义完全不同**：不带参数 = 「可以存，但每次用之前
//!    必须重验证」；带参数 `no-cache="Set-Cookie"` = 「可以存**也可以直接用**，
//!    只是列出来的那几个头不许发给客户端」。★ 把后者当成前者，缓存等于没开；
//!    把前者当成后者，**会把过期内容直接发出去**。
//! 2. **`no-store` 与 `no-cache` 不是一回事**：`no-store` 是「一个字节都不许落地」。
//!
//! ⚠ **指令名大小写不敏感**，参数可以带引号，`=` 两边可以有空格 ——
//! 这三条里漏掉任何一条，都会让一个**合法**的头被当成没有。

use serde::{Deserialize, Serialize};

/// 响应侧 `Cache-Control` 的全套指令（RFC 9111 §5.2.2 + RFC 5861）。
///
/// ★ ★ **`Serialize`/`Deserialize` 是磁盘后端（批 H）要的**：一条缓存条目的
/// 元数据里必须带着**存下来时**那份 `Cache-Control` —— 重验证与 `must-revalidate`
/// 的判定都要用它。⚠ 不存而是「读的时候从头里再解析一遍」会有个坑：
/// 我们发给客户端的头是**剥过**的（`no-cache="字段名"` 点名的那几个已经不在了），
/// 于是再解析一遍得到的不是同一份。
/// ★ 结构体级的 `#[serde(default)]`：少一个字段时给零值而不是整条解析失败 ——
/// ⚠ 而**真正的**版本兼容靠 `disk::META_VERSION` 那个号，不靠这里的宽容。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResponseCc {
    /// `no-store`：一个字节都不许落地。
    pub no_store: bool,
    /// `no-cache` **不带参数**：可以存，但每次用之前必须重验证。
    pub no_cache: bool,
    /// `no-cache="a, b"`：可以存也可以直接用，但这几个头不许发给客户端。
    ///
    /// ★ 与 `no_cache` **互斥地**记录 —— 它们是两条不同的规则，合成一个 bool
    /// 就再也分不开了。
    pub no_cache_fields: Vec<String>,
    /// `private`：共享缓存（我们就是）不许存。
    pub private: bool,
    /// `private="a, b"`：可以存，但这几个头不许存。
    pub private_fields: Vec<String>,
    /// `public`：即使按默认规则不可缓存，也可以存。
    pub public: bool,
    /// `max-age=<秒>`。
    pub max_age: Option<u64>,
    /// `s-maxage=<秒>` —— ★ **共享缓存优先用它**，它压过 `max-age`。
    pub s_maxage: Option<u64>,
    /// `must-revalidate`：过期之后**不许**发陈内容。
    pub must_revalidate: bool,
    /// `proxy-revalidate`：同上，但只约束共享缓存。
    pub proxy_revalidate: bool,
    /// `immutable`：新鲜期内不必重验证。
    pub immutable: bool,
    /// `no-transform`：不许改内容编码等。
    pub no_transform: bool,
    /// `stale-while-revalidate=<秒>`（RFC 5861）。★ 这一批**解析但不施加**。
    pub stale_while_revalidate: Option<u64>,
    /// `stale-if-error=<秒>`（RFC 5861）。★ 这一批**解析但不施加**。
    pub stale_if_error: Option<u64>,
    /// 解析到了至少一条我们认识的指令。
    ///
    /// ★ 它与「头存在」不是一回事：`Cache-Control: ` 空值、或者一整头全是
    /// 我们不认识的扩展指令时，这里是 `false`。判「上游有没有说过新鲜度」要用
    /// [`Self::has_freshness`]，不是这一条。
    pub any: bool,
}

impl ResponseCc {
    /// 上游有没有**明确给出**新鲜度？（G96 的兜底判据用它。）
    pub fn has_freshness(&self) -> bool {
        self.max_age.is_some() || self.s_maxage.is_some()
    }

    /// 共享缓存该用哪个新鲜度秒数。★ `s-maxage` 压过 `max-age`（RFC 9111 §5.2.2.10）。
    pub fn shared_max_age(&self) -> Option<u64> {
        self.s_maxage.or(self.max_age)
    }
}

/// 请求侧 `Cache-Control`（RFC 9111 §5.2.1）。
///
/// ★ 只做真正会改变行为的那几条。⚠ **`no-cache` 在请求侧的含义与响应侧相反**：
/// 请求侧是「别给我缓存的，去问上游」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestCc {
    /// `no-cache`：本次必须回源（或至少重验证）。
    pub no_cache: bool,
    /// `no-store`：本次的请求与响应都不许落地。
    pub no_store: bool,
    /// `only-if-cached`：只要缓存里的；没有就回 **504**（RFC 9111 §5.2.1.7）。
    pub only_if_cached: bool,
    /// `max-age=<秒>`：客户端不接受比这更旧的。
    pub max_age: Option<u64>,
    /// `min-fresh=<秒>`：客户端要求至少还能新鲜这么久。
    pub min_fresh: Option<u64>,
    /// `max-stale` / `max-stale=<秒>`：客户端接受过期多久的。
    /// `Some(None)` = 不限。
    #[allow(clippy::option_option)]
    pub max_stale: Option<Option<u64>>,
}

/// 把一个 `Cache-Control` 头切成 `(指令名小写, 参数)`。
///
/// ⚠ ⚠ 手写而不是 `split(',')`：**参数可以带引号，而引号里可以有逗号**
/// （`no-cache="Set-Cookie, X-Foo"` 是一条指令，不是两条）。
/// ★ 直接按逗号切会把它切成 `no-cache="Set-Cookie` 与 `X-Foo"` ——
/// 前者的参数少一半、后者是个不认识的指令，**而两者都不会报错**。
fn directives(header: &str) -> Vec<(String, Option<String>)> {
    let b = header.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // 跳过分隔符与空白
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // 指令名：到 `=` 或 `,` 为止
        let name_start = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        let name = header[name_start..i].trim().to_ascii_lowercase();
        let mut value: Option<String> = None;
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && b[i] == b'"' {
                // 引号串：读到下一个未转义的引号。
                i += 1;
                let vs = i;
                let mut buf = String::new();
                while i < b.len() {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        buf.push(b[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    if b[i] == b'"' {
                        break;
                    }
                    buf.push(b[i] as char);
                    i += 1;
                }
                let _ = vs;
                if i < b.len() {
                    i += 1; // 吃掉收尾引号
                }
                value = Some(buf);
            } else {
                let vs = i;
                while i < b.len() && b[i] != b',' {
                    i += 1;
                }
                value = Some(header[vs..i].trim().to_string());
            }
        }
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// 逗号分隔的字段名列表（`no-cache="a, b"` 的参数）。全部转小写。
fn field_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn secs(v: &Option<String>) -> Option<u64> {
    // ⚠ RFC 9111 §5.2：解析不出来的 delta-seconds **当成一个很大的值**是错的，
    //   而当成「这条指令不存在」也是错的。这里取「这条指令不存在」——
    //   ★ 因为另一侧（当成很大）会让一个写坏的 `max-age=abc` 变成"永远新鲜"。
    v.as_ref()?.parse::<u64>().ok()
}

/// 解析响应侧的 `Cache-Control`。
pub fn parse_response(header: &str) -> ResponseCc {
    let mut cc = ResponseCc::default();
    for (name, value) in directives(header) {
        cc.any = true;
        match name.as_str() {
            "no-store" => cc.no_store = true,
            "no-cache" => match &value {
                // ★ ★ 带参数与不带参数是**两条不同的规则**，见本文件顶部。
                Some(v) if !v.is_empty() => cc.no_cache_fields = field_list(v),
                _ => cc.no_cache = true,
            },
            "private" => match &value {
                Some(v) if !v.is_empty() => cc.private_fields = field_list(v),
                _ => cc.private = true,
            },
            "public" => cc.public = true,
            "max-age" => cc.max_age = secs(&value),
            "s-maxage" => cc.s_maxage = secs(&value),
            "must-revalidate" => cc.must_revalidate = true,
            "proxy-revalidate" => cc.proxy_revalidate = true,
            "immutable" => cc.immutable = true,
            "no-transform" => cc.no_transform = true,
            "stale-while-revalidate" => cc.stale_while_revalidate = secs(&value),
            "stale-if-error" => cc.stale_if_error = secs(&value),
            // ⚠ 不认识的扩展指令**忽略**（RFC 9111 §5.2.3 就是这么说的），
            //   但不把 `any` 撤回去 —— 「头里有东西」与「我们认得它」是两件事。
            _ => {}
        }
    }
    cc
}

/// 解析请求侧的 `Cache-Control`。
pub fn parse_request(header: &str) -> RequestCc {
    let mut cc = RequestCc::default();
    for (name, value) in directives(header) {
        match name.as_str() {
            "no-cache" => cc.no_cache = true,
            "no-store" => cc.no_store = true,
            "only-if-cached" => cc.only_if_cached = true,
            "max-age" => cc.max_age = secs(&value),
            "min-fresh" => cc.min_fresh = secs(&value),
            "max-stale" => cc.max_stale = Some(secs(&value)),
            _ => {}
        }
    }
    cc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 常见组合解析对了() {
        let cc = parse_response("public, max-age=3600");
        assert!(cc.public);
        assert_eq!(cc.max_age, Some(3600));
        assert!(cc.has_freshness());

        let cc = parse_response("no-store");
        assert!(cc.no_store);
        assert!(!cc.has_freshness());
    }

    // ★ ★ ★ 本文件顶部那条第 1 号规则：`no-cache` 的两种形态。
    //   ⚠ 合成一个 bool 之后这两条就再也分不开了 ——
    //   而把带参数的当成不带参数的，等于缓存完全没开；
    //   反过来，会把过期内容直接发给客户端。
    #[test]
    fn no_cache_带参数与不带参数是两条不同的规则() {
        let bare = parse_response("no-cache");
        assert!(bare.no_cache, "不带参数 ⇒ 每次都要重验证");
        assert!(bare.no_cache_fields.is_empty());

        let qualified = parse_response("no-cache=\"Set-Cookie\"");
        assert!(
            !qualified.no_cache,
            "带参数的**不是**「每次重验证」—— 它只是说那几个头不许发出去"
        );
        assert_eq!(qualified.no_cache_fields, ["set-cookie"]);
    }

    // ★ ★ ★ 引号里的逗号。按逗号切会把这一条切成两条，而**两半都不报错**：
    //   前半参数少一个字段、后半是个不认识的指令 —— 于是 X-Foo 被原样发出去。
    #[test]
    fn 引号里的逗号不是分隔符() {
        let cc = parse_response("no-cache=\"Set-Cookie, X-Foo\", max-age=60");
        assert_eq!(cc.no_cache_fields, ["set-cookie", "x-foo"]);
        assert_eq!(cc.max_age, Some(60), "后面那条指令还在");
    }

    #[test]
    fn private_也有两种形态() {
        assert!(parse_response("private").private);
        let q = parse_response("private=\"Authorization\"");
        assert!(!q.private, "带参数的 private 不是「整条不许存」");
        assert_eq!(q.private_fields, ["authorization"]);
    }

    // ★ ★ 共享缓存用 s-maxage，它压过 max-age（RFC 9111 §5.2.2.10）。
    //   ⚠ 取错的话，一个「浏览器 60s、CDN 1 天」的配置会被我们按 60s 用 ——
    //   功能正常，只是缓存几乎不命中，而没有任何东西会说为什么。
    #[test]
    fn s_maxage_压过_max_age() {
        let cc = parse_response("max-age=60, s-maxage=86400");
        assert_eq!(cc.shared_max_age(), Some(86400));
        let only_max = parse_response("max-age=60");
        assert_eq!(only_max.shared_max_age(), Some(60));
    }

    #[test]
    fn 大小写不敏感_等号两边可以有空格() {
        let cc = parse_response("Public, Max-Age = 300, MUST-REVALIDATE");
        assert!(cc.public);
        assert_eq!(cc.max_age, Some(300));
        assert!(cc.must_revalidate);
    }

    #[test]
    fn 全套指令都认得() {
        let cc = parse_response(
            "public, max-age=1, s-maxage=2, must-revalidate, proxy-revalidate, \
             immutable, no-transform, stale-while-revalidate=3, stale-if-error=4",
        );
        assert!(cc.public && cc.must_revalidate && cc.proxy_revalidate);
        assert!(cc.immutable && cc.no_transform);
        assert_eq!(
            (
                cc.max_age,
                cc.s_maxage,
                cc.stale_while_revalidate,
                cc.stale_if_error
            ),
            (Some(1), Some(2), Some(3), Some(4))
        );
    }

    // ★ ★ 写坏的 delta-seconds 当成「这条指令不存在」，**不是**当成很大的值。
    //   ⚠ 另一侧会让 `max-age=abc` 变成"永远新鲜" —— 一条打错的配置换来
    //   一个永不更新的页面，而现场看不出与缓存有关。
    #[test]
    fn 写坏的秒数当成这条指令不存在() {
        let cc = parse_response("max-age=abc");
        assert_eq!(cc.max_age, None);
        assert!(!cc.has_freshness(), "写坏了 ⇒ 上游等于没给新鲜度");
        assert!(cc.any, "但「头里有东西」这件事仍然是真的");
    }

    #[test]
    fn 不认识的扩展指令被忽略而不是报错() {
        let cc = parse_response("x-vendor-thing=1, max-age=5");
        assert_eq!(cc.max_age, Some(5));
        assert!(cc.any);
    }

    #[test]
    fn 空头与全空白() {
        for h in ["", "   ", ",", " , , "] {
            let cc = parse_response(h);
            assert!(!cc.any, "{h:?} 不该算「说过什么」");
            assert!(!cc.has_freshness());
        }
    }

    // ── 请求侧 ────────────────────────────────────────────────────────────
    #[test]
    fn 请求侧那几条() {
        let cc = parse_request("no-cache, max-age=0");
        assert!(cc.no_cache);
        assert_eq!(cc.max_age, Some(0));

        let cc = parse_request("only-if-cached");
        assert!(cc.only_if_cached);

        // `max-stale` 不带参数 = 不限。
        assert_eq!(parse_request("max-stale").max_stale, Some(None));
        assert_eq!(parse_request("max-stale=60").max_stale, Some(Some(60)));
        assert_eq!(parse_request("min-fresh=30").min_fresh, Some(30));
    }

    // ⚠ 请求侧的 `no-cache` 与响应侧含义相反 —— 钉一条免得两边共用一个解析器。
    #[test]
    fn 请求侧的_no_cache_不带字段名语义() {
        let cc = parse_request("no-cache=\"Set-Cookie\"");
        assert!(cc.no_cache, "请求侧带不带参数都是「别给我缓存的」");
    }
}
