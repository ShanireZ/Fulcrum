//! HTTP 日期：**发**一种、**收**三种（G93）。
//!
//! 图里没有日期库（G29 的依赖天花板），所以格式化与解析都手写。
//!
//! ★ ★ RFC 9110 §5.6.7 对两个方向说的**不是同一句话**，这一点值得写在明处：
//!
//! - **发**：`Last-Modified` 等时间戳 **MUST** 用 IMF-fixdate
//!   （`Sun, 06 Nov 1994 08:49:37 GMT`）。⇒ [`format_imf`] 只产这一种。
//! - **收**：接收方 **MUST** 接受三种 —— IMF-fixdate、RFC 850、asctime。
//!   ⇒ [`parse`] 三种都认。
//!
//! ⚠ ⚠ 这里最容易错的是 RFC 850 那个**两位年**。RFC 9110 的规则是：
//! 「看起来在未来 50 年以上的时间戳，必须理解成**最近的那个同末两位的过去年份**」。
//! 于是 `parse` 的结果**依赖"现在"**——所以真正干活的是 [`parse_with_now`]，
//! 它把"现在"当参数收，好让判据能在离线、确定的输入上钉住它。

use std::time::{SystemTime, UNIX_EPOCH};

/// ★ 只用于**格式化**。解析时星期名是被**忽略**的 —— RFC 9110 说接收方
/// 不该拿它当判据（报文里的星期名可能与日期本身矛盾）。
const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// 把一个时刻格式化成 IMF-fixdate。
///
/// 早于 1970 的时刻（时钟错乱、或文件被人为设成很早）回 `None` ——
/// ★ 不假装它是 epoch：一个错的时间戳会被下游当成真的，
/// 而缺一个 `Last-Modified` 头只是少一次 304。
pub fn format_imf(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(format_imf_secs(secs))
}

/// 同上，输入是 unix 秒。分出来是为了让判据能喂确定的数。
pub fn format_imf_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    // 1970-01-01 是星期四 ⇒ 偏移 3（表里 Mon 是 0）。
    let dow = (days + 3).rem_euclid(7) as usize;
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAY_NAMES[dow],
        d,
        MONTH_NAMES[(m - 1) as usize],
        y,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
    )
}

/// 解析三种 HTTP 日期，返回 unix 秒。用系统时钟当"现在"。
pub fn parse(s: &str) -> Option<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    parse_with_now(s, now)
}

/// 解析三种 HTTP 日期。`now_secs` 只被 RFC 850 那一档用到（两位年补世纪）。
pub fn parse_with_now(s: &str, now_secs: i64) -> Option<i64> {
    let s = s.trim();
    // 三种格式的分辨点在**第一个逗号**：
    //   IMF-fixdate  `Sun, 06 Nov 1994 08:49:37 GMT`   —— 逗号前 3 个字母
    //   RFC 850      `Sunday, 06-Nov-94 08:49:37 GMT`  —— 逗号前是全写
    //   asctime      `Sun Nov  6 08:49:37 1994`        —— 没有逗号
    match s.split_once(',') {
        None => parse_asctime(s),
        Some((dow, rest)) => {
            let rest = rest.trim_start();
            // ★ 判据不是"逗号前有多长"，而是**日期那一段自己长什么样**：
            //   `06-Nov-94` 里有连字符，`06 Nov 1994` 里没有。
            //   按星期名长度分反而会被 `Sun,` 与 `Sunday,` 之外的写法骗到。
            if !dow.trim().is_empty() && rest.get(2..3) == Some("-") {
                parse_rfc850(rest, now_secs)
            } else {
                parse_imf(rest)
            }
        }
    }
}

/// `06 Nov 1994 08:49:37 GMT`
fn parse_imf(s: &str) -> Option<i64> {
    let mut it = s.split_ascii_whitespace();
    let d: u32 = it.next()?.parse().ok()?;
    let m = month_from_name(it.next()?)?;
    let y: i64 = it.next()?.parse().ok()?;
    let tod = parse_hms(it.next()?)?;
    // ⚠ 尾巴必须是 GMT。RFC 9110 说得很明确：HTTP 日期**永远**是 GMT，
    //   带别的时区的那份报文是坏的 —— 认了它等于替对方猜一个偏移量。
    if !it.next()?.eq_ignore_ascii_case("GMT") {
        return None;
    }
    to_unix(y, m, d, tod)
}

/// `06-Nov-94 08:49:37 GMT`
fn parse_rfc850(s: &str, now_secs: i64) -> Option<i64> {
    let mut it = s.split_ascii_whitespace();
    let date = it.next()?;
    let tod = parse_hms(it.next()?)?;
    if !it.next()?.eq_ignore_ascii_case("GMT") {
        return None;
    }
    let mut parts = date.split('-');
    let d: u32 = parts.next()?.parse().ok()?;
    let m = month_from_name(parts.next()?)?;
    let yy: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(0..=99).contains(&yy) {
        return None;
    }
    to_unix(expand_two_digit_year(yy, now_secs), m, d, tod)
}

/// `Nov  6 08:49:37 1994`（日那一栏是**空格补位**，不是补零）
fn parse_asctime(s: &str) -> Option<i64> {
    let mut it = s.split_ascii_whitespace();
    // 星期名被忽略（见 DAY_NAMES 上那条注释），但**必须在**——少了它就不是 asctime。
    if it.next()?.len() < 3 {
        return None;
    }
    let m = month_from_name(it.next()?)?;
    // ⚠ 日那一栏是**空格补位**（`Nov  6`），所以这里靠 `split_ascii_whitespace`
    //   把连续空格吃掉；换成按固定列切会在个位数日期上错一格。
    let d: u32 = it.next()?.parse().ok()?;
    let tod = parse_hms(it.next()?)?;
    let y: i64 = it.next()?.parse().ok()?;
    to_unix(y, m, d, tod)
}

/// RFC 9110 §5.6.7 的两位年规则：
/// **看起来在未来 50 年以上的，取最近的那个同末两位的过去年份。**
fn expand_two_digit_year(yy: i64, now_secs: i64) -> i64 {
    let (now_year, _, _) = civil_from_days(now_secs.div_euclid(86_400));
    let mut y = now_year.div_euclid(100) * 100 + yy;
    if y > now_year + 50 {
        y -= 100;
    }
    y
}

fn month_from_name(s: &str) -> Option<u32> {
    MONTH_NAMES
        .iter()
        .position(|m| m.eq_ignore_ascii_case(s))
        .map(|i| i as u32 + 1)
}

/// `08:49:37` → 当天秒数。
fn parse_hms(s: &str) -> Option<i64> {
    let mut it = s.split(':');
    let h: i64 = it.next()?.parse().ok()?;
    let mi: i64 = it.next()?.parse().ok()?;
    let se: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    // ⚠ 闰秒：`60` 是合法的 HTTP 时间戳。把它折成 59 —— 拒掉它会让一次
    //   条件请求整个失效，而差一秒不影响 `If-Modified-Since` 的判断。
    let se = if se == 60 { 59 } else { se };
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&se) {
        return None;
    }
    Some(h * 3600 + mi * 60 + se)
}

fn to_unix(y: i64, m: u32, d: u32, tod: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + tod)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant 的 `days_from_civil`：民用日期 → 距 1970-01-01 的天数。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 上面那个的反函数。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9110 §5.6.7 自己举的那个例子，三种写法是**同一个时刻**。
    const IMF: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
    const RFC850: &str = "Sunday, 06-Nov-94 08:49:37 GMT";
    const ASCTIME: &str = "Sun Nov  6 08:49:37 1994";
    const EXPECT: i64 = 784_111_777;
    /// T00:00:00Z，给两位年那条规则当"现在"。
    const NOW_2026: i64 = 1_787_616_000;

    #[test]
    fn rfc9110_那个例子三种写法解析成同一个时刻() {
        assert_eq!(parse_with_now(IMF, NOW_2026), Some(EXPECT), "IMF-fixdate");
        assert_eq!(parse_with_now(RFC850, NOW_2026), Some(EXPECT), "RFC 850");
        assert_eq!(parse_with_now(ASCTIME, NOW_2026), Some(EXPECT), "asctime");
    }

    #[test]
    fn 发出去的只有_imf_一种_并且能被自己读回来() {
        assert_eq!(format_imf_secs(EXPECT), IMF);
        assert_eq!(
            parse_with_now(&format_imf_secs(EXPECT), NOW_2026),
            Some(EXPECT)
        );
    }

    // ★ ★ 这条钉的是 G93 那半句「两位年按 RFC 9110 附录规则处理」。
    //   ⚠ 少了它，一个 1994 年的 `If-Modified-Since` 会被读成 **2094 年**
    //   ⇒ 服务器认为客户端手上的副本比文件还新，于是**永远回 304**，
    //   而客户端永远拿不到更新后的文件 —— 现场是「改了文件但页面不变」，
    //   没有任何一条日志会说是日期解析错了。
    #[test]
    fn rfc850_的两位年按_rfc9110_规则补世纪() {
        let year_of = |s: &str| civil_from_days(parse_with_now(s, NOW_2026).unwrap() / 86_400).0;
        // 现在是 2026：+50 年的界在 2076。
        assert_eq!(year_of("Sun, 06-Nov-94 08:49:37 GMT"), 1994, "94 → 1994");
        assert_eq!(year_of("Sun, 06-Nov-26 08:49:37 GMT"), 2026, "26 → 2026");
        // 正好 50 年 —— 规则说的是「50 年**以上**」，所以这一档留在未来。
        assert_eq!(year_of("Sun, 06-Nov-76 08:49:37 GMT"), 2076, "76 → 2076");
        // 51 年 ⇒ 退一个世纪。★ 边界两侧各钉一条，差一错才跑不掉。
        assert_eq!(year_of("Sun, 06-Nov-77 08:49:37 GMT"), 1977, "77 → 1977");
        assert_eq!(year_of("Sun, 06-Nov-99 08:49:37 GMT"), 1999, "99 → 1999");
    }

    #[test]
    fn 不带_gmt_的一律拒掉() {
        assert_eq!(
            parse_with_now("Sun, 06 Nov 1994 08:49:37 UTC", NOW_2026),
            None
        );
        assert_eq!(
            parse_with_now("Sun, 06 Nov 1994 08:49:37 +0800", NOW_2026),
            None
        );
        assert_eq!(parse_with_now("Sun, 06 Nov 1994 08:49:37", NOW_2026), None);
    }

    #[test]
    fn 坏日期一律拒掉而不是猜() {
        for bad in [
            "",
            "not a date",
            "Sun, 32 Nov 1994 08:49:37 GMT", // 没有 11 月 32 日
            "Sun, 29 Feb 1995 08:49:37 GMT", // 1995 不是闰年
            "Sun, 06 Xxx 1994 08:49:37 GMT", // 月份名不认识
            "Sun, 06 Nov 1994 24:49:37 GMT", // 小时越界
            "Sun, 06 Nov 1994 08:60:37 GMT", // 分钟越界
            "Sun, 06-Nov-994 08:49:37 GMT",  // RFC 850 的年必须两位
        ] {
            assert_eq!(parse_with_now(bad, NOW_2026), None, "本该拒掉：{bad:?}");
        }
    }

    #[test]
    fn 闰年的_2_月_29_日认() {
        assert!(parse_with_now("Tue, 29 Feb 2028 00:00:00 GMT", NOW_2026).is_some());
    }

    // ★ 闰秒折成 59 而不是拒 —— 拒掉会让整条条件请求失效。
    #[test]
    fn 闰秒被折成_59_而不是拒掉() {
        assert_eq!(
            parse_with_now("Sun, 31 Dec 2028 23:59:60 GMT", NOW_2026),
            parse_with_now("Sun, 31 Dec 2028 23:59:59 GMT", NOW_2026),
        );
    }

    // ★ ★ 历法自证：两个方向来回一趟必须回到原处。
    //   ⚠ 少了这条，`days_from_civil` 里一个差一的错**只会在某些月份**显形，
    //   而抽查很容易全落在没事的那几天上。
    #[test]
    fn 历法来回一趟必须回到原处() {
        // 1970-01-01 起每隔 97 天取一个点，跨 200 年 —— 覆盖闰年、世纪年、非世纪年。
        let mut days = -70_000i64;
        while days < 60_000 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m:02}-{d:02} 对不上");
            days += 97;
        }
    }

    #[test]
    fn 星期几算对了() {
        // 1970-01-01 是星期四。
        assert!(format_imf_secs(0).starts_with("Thu, 01 Jan 1970"));
        // 是星期六。
        assert!(format_imf_secs(946_684_800).starts_with("Sat, 01 Jan 2000"));
    }

    #[test]
    fn 早于_epoch_的时刻不假装成_epoch() {
        let t = UNIX_EPOCH - std::time::Duration::from_secs(1);
        assert_eq!(format_imf(t), None);
    }
}
