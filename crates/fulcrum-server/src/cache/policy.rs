//! RFC 9111 的**可快取性**與**新鮮度**判定。全是純函式。
//!
//! ★ ★ ★ 這個模組是整批裡最該有判據的地方。G82 拍板時就把代價寫在了明處：
//! > **快取的每一條錯都表現為「偶爾給錯內容」** —— 不像轉發的錯那樣當場可見。
//!
//! ⇒ 下面每一條規則都帶一條**說清「寫錯了會怎樣」**的判據，
//! 而不只是「這樣寫是對的」。
//!
//! ## 我們是**共享**快取，這一條改變好幾處判斷
//!
//! - `private` ⇒ **不許存**（瀏覽器可以，我們不行）。
//! - `s-maxage` **壓過** `max-age`。
//! - `proxy-revalidate` 與 `must-revalidate` 對我們**等價**。
//! - 帶 `Authorization` 的請求，其回應**預設不可快取**（RFC 9111 §3.5）——
//!   ⚠ 這一條最容易漏，而漏了就是**把一個人的私有頁面發給下一個人**。

use super::cc::{RequestCc, ResponseCc};

/// 一次回應能不能進快取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storable {
    /// 能存，新鮮期是 `fresh_for_secs`（從 `Date` 起算）。
    Yes { fresh_for_secs: u64 },
    /// 不能存，附帶原因（進紀錄，也進判據）。
    No(&'static str),
}

/// 判定的輸入。★ 打包成一個結構是為了讓**每一條輸入都在簽名裡看得見** ——
/// 一個漏掉 `has_authorization` 的呼叫點，會安靜地開始快取私有頁面。
#[derive(Debug, Clone)]
pub struct StoreInput<'a> {
    pub method: &'a str,
    pub status: u16,
    /// 請求帶沒帶 `Authorization`（RFC 9111 §3.5）。
    pub has_authorization: bool,
    /// 回應的 `Cache-Control`。
    pub resp_cc: &'a ResponseCc,
    /// 請求的 `Cache-Control`。
    pub req_cc: &'a RequestCc,
    /// `Expires` 頭解析出來的 unix 秒（如果有）。
    pub expires: Option<i64>,
    /// `Date` 頭解析出來的 unix 秒；沒有就用收到回應的時刻。
    pub date: i64,
    /// 回應體位元組數；`None` = 還不知道（分塊）。
    pub body_len: Option<u64>,
    /// 單條目上限。
    pub max_size: u64,
    /// 設定裡的備援 TTL（G96）。`None` = 上游沒說就不快取。
    pub default_ttl_secs: Option<u64>,
    /// 回應裡有沒有 `Vary: *`。
    pub vary_star: bool,
    /// 回應裡有沒有 `Set-Cookie`。
    pub has_set_cookie: bool,
}

/// RFC 9111 §4.2.2 那張「預設可快取」的狀態碼錶（啟發式那一檔）。
///
/// ⚠ 這張表**只在上游給了新鮮度、或配了備援 TTL 時**才有意義 ——
/// 我們**不做啟發式新鮮度**（不拿 `Last-Modified` 去猜 10% 壽命），
/// 那一條留給以後，理由寫在 `Storable::No` 的原因串裡。
const CACHEABLE_STATUS: &[u16] = &[
    200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 451, 501,
];

/// 能不能把這次回應存下來。
pub fn is_storable(i: &StoreInput<'_>) -> Storable {
    // ── 請求側的一票否決 ──────────────────────────────────────────────
    if i.req_cc.no_store {
        return Storable::No("请求带 no-store");
    }
    // ⚠ 只快取 GET / HEAD。⚠ HEAD 的回應**沒有體**，存下來只能用於回答 HEAD ——
    //   這一批不做（存了會讓「HEAD 之後的 GET 命中一個空體」）。
    if i.method != "GET" {
        return Storable::No("只缓存 GET");
    }

    // ── 回應側的一票否決 ──────────────────────────────────────────────
    if i.resp_cc.no_store {
        return Storable::No("响应带 no-store");
    }
    // ★ ★ 我們是**共享**快取 —— `private` 對我們是禁令。
    if i.resp_cc.private {
        return Storable::No("响应带 private，而我们是共享缓存");
    }
    // ★ ★ ★ RFC 9111 §3.5：帶 `Authorization` 的請求，其回應對共享快取
    //   **預設不可存**，除非回應明確說了 `public` / `s-maxage` / `must-revalidate`。
    //   ⚠ 漏掉這一條就是把一個人的私有頁面發給下一個人 —— 而它**不會回報錯誤**，
    //   只會在兩個使用者之間偶爾串一次內容。
    if i.has_authorization
        && !(i.resp_cc.public || i.resp_cc.s_maxage.is_some() || i.resp_cc.must_revalidate)
    {
        return Storable::No("请求带 Authorization，而响应没有明确允许共享缓存");
    }
    // ⚠ `Vary: *` = 「這個回應取決於我說不出來的東西」⇒ 永遠不可能安全重複使用。
    if i.vary_star {
        return Storable::No("Vary: *");
    }
    // ★ `Set-Cookie` 通常是給**這一個**用戶端的。RFC 允許存（只要不發給別人），
    //   但那要求剝頭，而剝錯一次就是串號。⇒ 這一批**直接不存**，寫在明處。
    if i.has_set_cookie && !i.resp_cc.public {
        return Storable::No("响应带 Set-Cookie 且没写 public");
    }
    if !CACHEABLE_STATUS.contains(&i.status) {
        return Storable::No("状态码不在可缓存表里");
    }
    // ⚠ 206 要靠 Content-Range 拼裝，這一批不做。
    if i.status == 206 {
        return Storable::No("206 部分响应这一批不缓存");
    }
    if let Some(n) = i.body_len
        && n > i.max_size
    {
        return Storable::No("超过单条目大小上限");
    }

    // ── 新鮮期 ────────────────────────────────────────────────────────
    match freshness_lifetime(i) {
        Some(0) => {
            // ★ `max-age=0` 是合法的：它意味著「存下來，但每次用之前重驗證」。
            //   ⇒ 存，新鮮期 0。⚠ 判成「不可存」會讓重驗證這條路整個用不上。
            Storable::Yes { fresh_for_secs: 0 }
        }
        Some(n) => Storable::Yes { fresh_for_secs: n },
        None => Storable::No("上游没给新鲜度，且没有配 ttl 兜底"),
    }
}

/// 新鮮期（秒）。★ 優先級照 RFC 9111 §4.2.1，再加上 G96 的備援。
///
/// `s-maxage` → `max-age` → `Expires - Date` → **設定的 `ttl`（備援）** → 無。
pub fn freshness_lifetime(i: &StoreInput<'_>) -> Option<u64> {
    if let Some(n) = i.resp_cc.shared_max_age() {
        return Some(n);
    }
    if let Some(exp) = i.expires {
        // ⚠ `Expires` 早於 `Date` ⇒ 已經過期，新鮮期 0（不是「無」）。
        return Some((exp - i.date).max(0) as u64);
    }
    // ★ ★ G96：**備援，不是覆蓋** —— 走到這裡才用它，也就是上游一個字都沒說。
    //   ⚠ 放到最前面就變成了「覆蓋」，而那會讓一個帶 no-store 的回應
    //   也拿到新鮮期（雖然上面已經攔住了 no-store，但 private / Authorization
    //   那幾條的邊界會跟著鬆掉）。
    i.default_ttl_secs
}

/// 快取裡那一條的當前狀態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// 可以直接用。
    Fresh,
    /// 過期了，但可以拿去**重驗證**（發條件請求給上游）。
    Stale,
    /// 不許用（`no-cache`、或請求要求回源）。
    MustRevalidate,
}

/// 判定一條快取條目現在能不能直接用。
///
/// `age_secs` = 這條條目已經存在多久（RFC 9111 §4.2.3 的 Age）。
pub fn freshness(
    resp_cc: &ResponseCc,
    req_cc: &RequestCc,
    fresh_for_secs: u64,
    age_secs: u64,
) -> Freshness {
    // ★ 回應側 `no-cache`（**不帶參數**那種）⇒ 每次用之前都要重驗證。
    if resp_cc.no_cache {
        return Freshness::MustRevalidate;
    }
    // ★ 請求側 `no-cache` ⇒ 用戶端要求回源。
    if req_cc.no_cache {
        return Freshness::MustRevalidate;
    }
    // 用戶端可以把新鮮期**收緊**（`max-age` / `min-fresh`），但不能放寬。
    let mut limit = fresh_for_secs;
    if let Some(m) = req_cc.max_age {
        limit = limit.min(m);
    }
    if let Some(mf) = req_cc.min_fresh {
        // 要求「至少還能新鮮 mf 秒」⇒ 相當於把新鮮期提前 mf 秒結束。
        limit = limit.saturating_sub(mf);
    }
    if age_secs < limit {
        return Freshness::Fresh;
    }
    // ── 過期之後 ──────────────────────────────────────────────────────
    // ★ ★ `must-revalidate` / `proxy-revalidate` ⇒ **不許**發陳內容。
    //   ⚠ 對共享快取這兩條等價，合併處理。
    if resp_cc.must_revalidate || resp_cc.proxy_revalidate {
        return Freshness::MustRevalidate;
    }
    // 用戶端明確接受陳內容？
    if let Some(ms) = req_cc.max_stale {
        let stale_by = age_secs - limit;
        let ok = match ms {
            None => true, // `max-stale` 不帶參數 = 不限
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

    // ★ ★ G96 的備援：上游一個字都沒說時才用設定的 ttl。
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

        // ⚠ 上游說了 60，備援是 300 ⇒ **聽上游的 60**。
        //   反過來（用 300）就是「覆蓋」語義，而 G96 明確取的是備援。
        let (r2, q2) = input(200, "max-age=60");
        let mut i2 = mk(&r2, &q2, 200);
        i2.default_ttl_secs = Some(300);
        assert_eq!(is_storable(&i2), Storable::Yes { fresh_for_secs: 60 });
    }

    // ★ ★ ★ 最貴的那一條：帶 Authorization 的請求，回應預設不可存。
    //   ⚠ 漏了它 = 把一個人的私有頁面發給下一個人，而不會有任何回報錯誤。
    #[test]
    fn 带_authorization_的响应默认不可存() {
        let (r, q) = input(200, "max-age=60");
        let mut i = mk(&r, &q, 200);
        i.has_authorization = true;
        assert!(matches!(is_storable(&i), Storable::No(_)));

        // 三條例外各驗一次（RFC 9111 §3.5）。
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
        // 而帶欄位名的 private **不是**整條禁令。
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

    // ★ `max-age=0` 是「存下來但每次重驗證」，**不是**「不可存」。
    //   ⚠ 判成不可存，重驗證這條路整個用不上（每次都全量回源）。
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
        // 明寫 public 的話可以（部署方自己說了算）。
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

    // ★ Expires 早於 Date ⇒ 新鮮期 0，**不是**「沒有新鮮度」。
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

    // ── 新鮮度判定 ────────────────────────────────────────────────────
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

        // ⚠ 帶欄位名的 no-cache **不**觸發它。
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
        // 沒有 must-revalidate 時，max-stale 說了算。
        let r2 = cc::parse_response("max-age=60");
        assert_eq!(freshness(&r2, &q, 60, 600), Freshness::Fresh);
    }

    #[test]
    fn 客户端只能收紧不能放宽() {
        let r = cc::parse_response("max-age=600");
        // max-age=60：用戶端不接受比 60 秒更舊的 ⇒ age=100 時已經不新鮮。
        let q = cc::parse_request("max-age=60");
        assert_eq!(freshness(&r, &q, 600, 100), Freshness::Stale);
        assert_eq!(freshness(&r, &q, 600, 30), Freshness::Fresh);
        // min-fresh=100：要求還能新鮮 100 秒 ⇒ age=550 時不夠了。
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

    // ★ ★ 覆蓋自證：可快取狀態碼錶裡的每一條都真的走得通，
    //   而表外隨便抽的幾條都走不通。⚠ 少了它，一次手滑把表清空
    //   會讓「快取完全不生效」這件事悄悄發生 —— 門全綠，只是再也沒有命中。
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
