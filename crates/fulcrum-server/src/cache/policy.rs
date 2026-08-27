//! RFC 9111 的**可缓存性**与**新鲜度**判定。全是纯函数。
//!
//! ★ ★ ★ 这个模块是整批里最该有判据的地方。G82 拍板时就把代价写在了明处：
//! > **缓存的每一条错都表现为「偶尔给错内容」** —— 不像转发的错那样当场可见。
//!
//! ⇒ 下面每一条规则都带一条**说清「写错了会怎样」**的判据，
//! 而不只是「这样写是对的」。
//!
//! ## 我们是**共享**缓存，这一条改变好几处判断
//!
//! - `private` ⇒ **不许存**（浏览器可以，我们不行）。
//! - `s-maxage` **压过** `max-age`。
//! - `proxy-revalidate` 与 `must-revalidate` 对我们**等价**。
//! - 带 `Authorization` 的请求，其响应**默认不可缓存**（RFC 9111 §3.5）——
//!   ⚠ 这一条最容易漏，而漏了就是**把一个人的私有页面发给下一个人**。

use super::cc::{RequestCc, ResponseCc};

/// 一次响应能不能进缓存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storable {
    /// 能存，新鲜期是 `fresh_for_secs`（从 `Date` 起算）。
    Yes { fresh_for_secs: u64 },
    /// 不能存，附带原因（进日志，也进判据）。
    No(&'static str),
}

/// 判定的输入。★ 打包成一个结构是为了让**每一条输入都在签名里看得见** ——
/// 一个漏掉 `has_authorization` 的调用点，会安静地开始缓存私有页面。
#[derive(Debug, Clone)]
pub struct StoreInput<'a> {
    pub method: &'a str,
    pub status: u16,
    /// 请求带没带 `Authorization`（RFC 9111 §3.5）。
    pub has_authorization: bool,
    /// 响应的 `Cache-Control`。
    pub resp_cc: &'a ResponseCc,
    /// 请求的 `Cache-Control`。
    pub req_cc: &'a RequestCc,
    /// `Expires` 头解析出来的 unix 秒（如果有）。
    pub expires: Option<i64>,
    /// `Date` 头解析出来的 unix 秒；没有就用收到响应的时刻。
    pub date: i64,
    /// 响应体字节数；`None` = 还不知道（分块）。
    pub body_len: Option<u64>,
    /// 单条目上限。
    pub max_size: u64,
    /// 配置里的兜底 TTL（G96）。`None` = 上游没说就不缓存。
    pub default_ttl_secs: Option<u64>,
    /// 响应里有没有 `Vary: *`。
    pub vary_star: bool,
    /// 响应里有没有 `Set-Cookie`。
    pub has_set_cookie: bool,
}

/// RFC 9111 §4.2.2 那张「默认可缓存」的状态码表（启发式那一档）。
///
/// ⚠ 这张表**只在上游给了新鲜度、或配了兜底 TTL 时**才有意义 ——
/// 我们**不做启发式新鲜度**（不拿 `Last-Modified` 去猜 10% 寿命），
/// 那一条留给以后，理由写在 `Storable::No` 的原因串里。
const CACHEABLE_STATUS: &[u16] = &[
    200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 451, 501,
];

/// 能不能把这次响应存下来。
pub fn is_storable(i: &StoreInput<'_>) -> Storable {
    // ── 请求侧的一票否决 ──────────────────────────────────────────────
    if i.req_cc.no_store {
        return Storable::No("请求带 no-store");
    }
    // ⚠ 只缓存 GET / HEAD。⚠ HEAD 的响应**没有体**，存下来只能用于回答 HEAD ——
    //   这一批不做（存了会让「HEAD 之后的 GET 命中一个空体」）。
    if i.method != "GET" {
        return Storable::No("只缓存 GET");
    }

    // ── 响应侧的一票否决 ──────────────────────────────────────────────
    if i.resp_cc.no_store {
        return Storable::No("响应带 no-store");
    }
    // ★ ★ 我们是**共享**缓存 —— `private` 对我们是禁令。
    if i.resp_cc.private {
        return Storable::No("响应带 private，而我们是共享缓存");
    }
    // ★ ★ ★ RFC 9111 §3.5：带 `Authorization` 的请求，其响应对共享缓存
    //   **默认不可存**，除非响应明确说了 `public` / `s-maxage` / `must-revalidate`。
    //   ⚠ 漏掉这一条就是把一个人的私有页面发给下一个人 —— 而它**不会报错**，
    //   只会在两个用户之间偶尔串一次内容。
    if i.has_authorization
        && !(i.resp_cc.public || i.resp_cc.s_maxage.is_some() || i.resp_cc.must_revalidate)
    {
        return Storable::No("请求带 Authorization，而响应没有明确允许共享缓存");
    }
    // ⚠ `Vary: *` = 「这个响应取决于我说不出来的东西」⇒ 永远不可能安全复用。
    if i.vary_star {
        return Storable::No("Vary: *");
    }
    // ★ `Set-Cookie` 通常是给**这一个**客户端的。RFC 允许存（只要不发给别人），
    //   但那要求剥头，而剥错一次就是串号。⇒ 这一批**直接不存**，写在明处。
    if i.has_set_cookie && !i.resp_cc.public {
        return Storable::No("响应带 Set-Cookie 且没写 public");
    }
    if !CACHEABLE_STATUS.contains(&i.status) {
        return Storable::No("状态码不在可缓存表里");
    }
    // ⚠ 206 要靠 Content-Range 拼装，这一批不做。
    if i.status == 206 {
        return Storable::No("206 部分响应这一批不缓存");
    }
    if let Some(n) = i.body_len
        && n > i.max_size
    {
        return Storable::No("超过单条目大小上限");
    }

    // ── 新鲜期 ────────────────────────────────────────────────────────
    match freshness_lifetime(i) {
        Some(0) => {
            // ★ `max-age=0` 是合法的：它意味着「存下来，但每次用之前重验证」。
            //   ⇒ 存，新鲜期 0。⚠ 判成「不可存」会让重验证这条路整个用不上。
            Storable::Yes { fresh_for_secs: 0 }
        }
        Some(n) => Storable::Yes { fresh_for_secs: n },
        None => Storable::No("上游没给新鲜度，且没有配 ttl 兜底"),
    }
}

/// 新鲜期（秒）。★ 优先级照 RFC 9111 §4.2.1，再加上 G96 的兜底。
///
/// `s-maxage` → `max-age` → `Expires - Date` → **配置的 `ttl`（兜底）** → 无。
pub fn freshness_lifetime(i: &StoreInput<'_>) -> Option<u64> {
    if let Some(n) = i.resp_cc.shared_max_age() {
        return Some(n);
    }
    if let Some(exp) = i.expires {
        // ⚠ `Expires` 早于 `Date` ⇒ 已经过期，新鲜期 0（不是「无」）。
        return Some((exp - i.date).max(0) as u64);
    }
    // ★ ★ G96：**兜底，不是覆盖** —— 走到这里才用它，也就是上游一个字都没说。
    //   ⚠ 放到最前面就变成了「覆盖」，而那会让一个带 no-store 的响应
    //   也拿到新鲜期（虽然上面已经拦住了 no-store，但 private / Authorization
    //   那几条的边界会跟着松掉）。
    i.default_ttl_secs
}

/// 缓存里那一条的当前状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// 可以直接用。
    Fresh,
    /// 过期了，但可以拿去**重验证**（发条件请求给上游）。
    Stale,
    /// 不许用（`no-cache`、或请求要求回源）。
    MustRevalidate,
}

/// 判定一条缓存条目现在能不能直接用。
///
/// `age_secs` = 这条条目已经存在多久（RFC 9111 §4.2.3 的 Age）。
pub fn freshness(
    resp_cc: &ResponseCc,
    req_cc: &RequestCc,
    fresh_for_secs: u64,
    age_secs: u64,
) -> Freshness {
    // ★ 响应侧 `no-cache`（**不带参数**那种）⇒ 每次用之前都要重验证。
    if resp_cc.no_cache {
        return Freshness::MustRevalidate;
    }
    // ★ 请求侧 `no-cache` ⇒ 客户端要求回源。
    if req_cc.no_cache {
        return Freshness::MustRevalidate;
    }
    // 客户端可以把新鲜期**收紧**（`max-age` / `min-fresh`），但不能放宽。
    let mut limit = fresh_for_secs;
    if let Some(m) = req_cc.max_age {
        limit = limit.min(m);
    }
    if let Some(mf) = req_cc.min_fresh {
        // 要求「至少还能新鲜 mf 秒」⇒ 相当于把新鲜期提前 mf 秒结束。
        limit = limit.saturating_sub(mf);
    }
    if age_secs < limit {
        return Freshness::Fresh;
    }
    // ── 过期之后 ──────────────────────────────────────────────────────
    // ★ ★ `must-revalidate` / `proxy-revalidate` ⇒ **不许**发陈内容。
    //   ⚠ 对共享缓存这两条等价，合并处理。
    if resp_cc.must_revalidate || resp_cc.proxy_revalidate {
        return Freshness::MustRevalidate;
    }
    // 客户端明确接受陈内容？
    if let Some(ms) = req_cc.max_stale {
        let stale_by = age_secs - limit;
        let ok = match ms {
            None => true, // `max-stale` 不带参数 = 不限
            Some(n) => stale_by <= n,
        };
        if ok {
            return Freshness::Fresh;
        }
    }
    Freshness::Stale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::cc;

    fn input(status: u16, resp_header: &str) -> (ResponseCc, RequestCc) {
        let _ = status;
        (cc::parse_response(resp_header), RequestCc::default())
    }

    fn mk<'a>(resp_cc: &'a ResponseCc, req_cc: &'a RequestCc, status: u16) -> StoreInput<'a> {
        StoreInput {
            method: "GET",
            status,
            has_authorization: false,
            resp_cc,
            req_cc,
            expires: None,
            date: 1_000_000,
            body_len: Some(100),
            max_size: 8 * 1024 * 1024,
            default_ttl_secs: None,
            vary_star: false,
            has_set_cookie: false,
        }
    }

    #[test]
    fn 上游给了_max_age_就能存() {
        let (r, q) = input(200, "max-age=60");
        assert_eq!(
            is_storable(&mk(&r, &q, 200)),
            Storable::Yes { fresh_for_secs: 60 }
        );
    }

    // ★ ★ G96 的兜底：上游一个字都没说时才用配置的 ttl。
    #[test]
    fn ttl_是兜底_上游说了就听上游的() {
        let (r, q) = input(200, "");
        let mut i = mk(&r, &q, 200);
        assert!(
            matches!(is_storable(&i), Storable::No(_)),
            "没配 ttl ⇒ 不存"
        );
        i.default_ttl_secs = Some(300);
        assert_eq!(
            is_storable(&i),
            Storable::Yes {
                fresh_for_secs: 300
            }
        );

        // ⚠ 上游说了 60，兜底是 300 ⇒ **听上游的 60**。
        //   反过来（用 300）就是「覆盖」语义，而 G96 明确取的是兜底。
        let (r2, q2) = input(200, "max-age=60");
        let mut i2 = mk(&r2, &q2, 200);
        i2.default_ttl_secs = Some(300);
        assert_eq!(is_storable(&i2), Storable::Yes { fresh_for_secs: 60 });
    }

    // ★ ★ ★ 最贵的那一条：带 Authorization 的请求，响应默认不可存。
    //   ⚠ 漏了它 = 把一个人的私有页面发给下一个人，而不会有任何报错。
    #[test]
    fn 带_authorization_的响应默认不可存() {
        let (r, q) = input(200, "max-age=60");
        let mut i = mk(&r, &q, 200);
        i.has_authorization = true;
        assert!(matches!(is_storable(&i), Storable::No(_)));

        // 三条例外各验一次（RFC 9111 §3.5）。
        for header in [
            "max-age=60, public",
            "s-maxage=60",
            "max-age=60, must-revalidate",
        ] {
            let (r2, q2) = input(200, header);
            let mut i2 = mk(&r2, &q2, 200);
            i2.has_authorization = true;
            assert!(
                matches!(is_storable(&i2), Storable::Yes { .. }),
                "`{header}` 该被允许"
            );
        }
    }

    #[test]
    fn private_对共享缓存是禁令() {
        let (r, q) = input(200, "private, max-age=60");
        assert!(matches!(is_storable(&mk(&r, &q, 200)), Storable::No(_)));
        // 而带字段名的 private **不是**整条禁令。
        let (r2, q2) = input(200, "private=\"X-Foo\", max-age=60");
        assert!(matches!(
            is_storable(&mk(&r2, &q2, 200)),
            Storable::Yes { .. }
        ));
    }

    #[test]
    fn no_store_两侧都能否决() {
        let (r, q) = input(200, "no-store, max-age=60");
        assert!(matches!(is_storable(&mk(&r, &q, 200)), Storable::No(_)));

        let (r2, _) = input(200, "max-age=60");
        let q2 = cc::parse_request("no-store");
        assert!(matches!(is_storable(&mk(&r2, &q2, 200)), Storable::No(_)));
    }

    // ★ `max-age=0` 是「存下来但每次重验证」，**不是**「不可存」。
    //   ⚠ 判成不可存，重验证这条路整个用不上（每次都全量回源）。
    #[test]
    fn max_age_0_能存_新鲜期是零() {
        let (r, q) = input(200, "max-age=0");
        assert_eq!(
            is_storable(&mk(&r, &q, 200)),
            Storable::Yes { fresh_for_secs: 0 }
        );
    }

    #[test]
    fn vary_星号与_set_cookie_不存() {
        let (r, q) = input(200, "max-age=60");
        let mut i = mk(&r, &q, 200);
        i.vary_star = true;
        assert!(matches!(is_storable(&i), Storable::No(_)));

        let mut i2 = mk(&r, &q, 200);
        i2.has_set_cookie = true;
        assert!(matches!(is_storable(&i2), Storable::No(_)));
        // 明写 public 的话可以（部署方自己说了算）。
        let (r3, q3) = input(200, "max-age=60, public");
        let mut i3 = mk(&r3, &q3, 200);
        i3.has_set_cookie = true;
        assert!(matches!(is_storable(&i3), Storable::Yes { .. }));
    }

    #[test]
    fn 方法与状态码的门() {
        let (r, q) = input(200, "max-age=60");
        let mut i = mk(&r, &q, 200);
        i.method = "POST";
        assert!(matches!(is_storable(&i), Storable::No(_)));
        i.method = "HEAD";
        assert!(
            matches!(is_storable(&i), Storable::No(_)),
            "HEAD 这一批不缓存"
        );

        for bad in [500u16, 502, 401, 403, 206] {
            assert!(
                matches!(is_storable(&mk(&r, &q, bad)), Storable::No(_)),
                "{bad} 不该可缓存"
            );
        }
        for good in [200u16, 301, 404, 410] {
            assert!(
                matches!(is_storable(&mk(&r, &q, good)), Storable::Yes { .. }),
                "{good} 该可缓存"
            );
        }
    }

    #[test]
    fn 超过单条目上限不存() {
        let (r, q) = input(200, "max-age=60");
        let mut i = mk(&r, &q, 200);
        i.max_size = 50;
        i.body_len = Some(51);
        assert!(matches!(is_storable(&i), Storable::No(_)));
        i.body_len = Some(50);
        assert!(
            matches!(is_storable(&i), Storable::Yes { .. }),
            "刚好等于上限要放行"
        );
    }

    // ★ Expires 早于 Date ⇒ 新鲜期 0，**不是**「没有新鲜度」。
    #[test]
    fn expires_的两个方向() {
        let (r, q) = input(200, "");
        let mut i = mk(&r, &q, 200);
        i.expires = Some(i.date + 120);
        assert_eq!(
            is_storable(&i),
            Storable::Yes {
                fresh_for_secs: 120
            }
        );
        i.expires = Some(i.date - 120);
        assert_eq!(
            is_storable(&i),
            Storable::Yes { fresh_for_secs: 0 },
            "已过期 ⇒ 0，不是「无」"
        );
    }

    // ── 新鲜度判定 ────────────────────────────────────────────────────
    #[test]
    fn 新鲜与过期的边界() {
        let r = cc::parse_response("max-age=60");
        let q = RequestCc::default();
        assert_eq!(freshness(&r, &q, 60, 59), Freshness::Fresh);
        assert_eq!(
            freshness(&r, &q, 60, 60),
            Freshness::Stale,
            "age == 新鲜期 ⇒ 已过期"
        );
        assert_eq!(freshness(&r, &q, 60, 61), Freshness::Stale);
    }

    #[test]
    fn 响应侧_no_cache_每次都要重验证() {
        let r = cc::parse_response("no-cache, max-age=600");
        let q = RequestCc::default();
        assert_eq!(freshness(&r, &q, 600, 0), Freshness::MustRevalidate);

        // ⚠ 带字段名的 no-cache **不**触发它。
        let r2 = cc::parse_response("no-cache=\"Set-Cookie\", max-age=600");
        assert_eq!(freshness(&r2, &q, 600, 0), Freshness::Fresh);
    }

    #[test]
    fn must_revalidate_过期之后不许发陈内容() {
        let r = cc::parse_response("max-age=60, must-revalidate");
        let q = cc::parse_request("max-stale");
        assert_eq!(
            freshness(&r, &q, 60, 600),
            Freshness::MustRevalidate,
            "客户端说接受陈的也不行"
        );
        // 没有 must-revalidate 时，max-stale 说了算。
        let r2 = cc::parse_response("max-age=60");
        assert_eq!(freshness(&r2, &q, 60, 600), Freshness::Fresh);
    }

    #[test]
    fn 客户端只能收紧不能放宽() {
        let r = cc::parse_response("max-age=600");
        // max-age=60：客户端不接受比 60 秒更旧的 ⇒ age=100 时已经不新鲜。
        let q = cc::parse_request("max-age=60");
        assert_eq!(freshness(&r, &q, 600, 100), Freshness::Stale);
        assert_eq!(freshness(&r, &q, 600, 30), Freshness::Fresh);
        // min-fresh=100：要求还能新鲜 100 秒 ⇒ age=550 时不够了。
        let q2 = cc::parse_request("min-fresh=100");
        assert_eq!(freshness(&r, &q2, 600, 550), Freshness::Stale);
        assert_eq!(freshness(&r, &q2, 600, 400), Freshness::Fresh);
    }

    #[test]
    fn max_stale_带参数与不带参数() {
        let r = cc::parse_response("max-age=60");
        let bounded = cc::parse_request("max-stale=30");
        assert_eq!(
            freshness(&r, &bounded, 60, 80),
            Freshness::Fresh,
            "过期 20s，允许 30s"
        );
        assert_eq!(
            freshness(&r, &bounded, 60, 100),
            Freshness::Stale,
            "过期 40s，超了"
        );
        let unbounded = cc::parse_request("max-stale");
        assert_eq!(freshness(&r, &unbounded, 60, 100_000), Freshness::Fresh);
    }

    // ★ ★ 覆盖自证：可缓存状态码表里的每一条都真的走得通，
    //   而表外随便抽的几条都走不通。⚠ 少了它，一次手滑把表清空
    //   会让「缓存完全不生效」这件事悄悄发生 —— 门全绿，只是再也没有命中。
    #[test]
    fn 可缓存状态码表自证() {
        let r = cc::parse_response("max-age=60");
        let q = RequestCc::default();
        for s in CACHEABLE_STATUS {
            let got = is_storable(&mk(&r, &q, *s));
            if *s == 206 {
                assert!(matches!(got, Storable::No(_)), "206 有意排除");
            } else {
                assert!(matches!(got, Storable::Yes { .. }), "{s} 在表里却存不了");
            }
        }
        assert!(!CACHEABLE_STATUS.is_empty(), "表被清空了");
    }
}
