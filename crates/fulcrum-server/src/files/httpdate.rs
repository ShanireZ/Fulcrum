//! HTTP 日期：**發**一種、**收**三種（G93）。
//!
//! 圖裡沒有日期庫（G29 的依賴天花板），所以格式化與解析都手寫。
//!
//! ★ ★ RFC 9110 §5.6.7 對兩個方向說的**不是同一句話**，這一點值得寫在明處：
//!
//! - **發**：`Last-Modified` 等時間戳 **MUST** 用 IMF-fixdate
//!   （`Sun, 06 Nov 1994 08:49:37 GMT`）。⇒ [`format_imf`] 只產這一種。
//! - **收**：接收方 **MUST** 接受三種 —— IMF-fixdate、RFC 850、asctime。
//!   ⇒ [`parse`] 三種都認。
//!
//! ⚠ ⚠ 這裡最容易錯的是 RFC 850 那個**兩位年**。RFC 9110 的規則是：
//! 「看起來在未來 50 年以上的時間戳，必須理解成**最近的那個同末兩位的過去年份**」。
//! 於是 `parse` 的結果**依賴"現在"**——所以真正幹活的是 [`parse_with_now`]，
//! 它把"現在"當參數收，好讓判據能在離線、確定的輸入上釘住它。

use std::time::{SystemTime, UNIX_EPOCH};

/// ★ 只用於**格式化**。解析時星期名是被**忽略**的 —— RFC 9110 說接收方
/// 不該拿它當判據（訊息裡的星期名可能與日期本身矛盾）。
const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// 把一個時刻格式化成 IMF-fixdate。
///
/// 早於 1970 的時刻（時鐘錯亂、或檔案被人為設成很早）回 `None` ——
/// ★ 不假裝它是 epoch：一個錯的時間戳會被下游當成真的，
/// 而缺一個 `Last-Modified` 頭只是少一次 304。
pub fn format_imf(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(format_imf_secs(secs))
}

/// 同上，輸入是 unix 秒。分出來是為了讓判據能喂確定的數。
pub fn format_imf_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    // 1970-01-01 是星期四 ⇒ 偏移 3（表裡 Mon 是 0）。
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

/// IMF-fixdate 的定長（年份四位時）：`Sun, 06 Nov 1994 08:49:37 GMT` 恆為 29 位元組。
pub const IMF_LEN: usize = 29;

/// 同 [`format_imf_secs`]，但寫進呼叫方給的定長緩衝、⛔ 不走 `fmt`、不分配
/// （10-05 · `file_server` 每個回應都要一個 `Last-Modified`）。
///
/// 年份不在 0000–9999 時寫不進 29 位元組 ⇒ 回 `false`，呼叫方回落到 [`format_imf_secs`]
/// （那一版照 `{:04}` 寫出五位年，位元組與以前一樣）。
/// ★ 兩版逐位元組相同由單測 `定长写法与_format_那一版逐字节相同` 釘著。
pub fn write_imf_secs(secs: i64, out: &mut [u8; IMF_LEN]) -> bool {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    if !(0..=9999).contains(&y) {
        return false;
    }
    // 1970-01-01 是星期四 ⇒ 偏移 3（表裡 Mon 是 0）。
    let dow = (days + 3).rem_euclid(7) as usize;
    out[0..3].copy_from_slice(DAY_NAMES[dow].as_bytes());
    out[3..5].copy_from_slice(b", ");
    put_decimal(&mut out[5..7], d);
    out[7] = b' ';
    out[8..11].copy_from_slice(MONTH_NAMES[(m - 1) as usize].as_bytes());
    out[11] = b' ';
    put_decimal(&mut out[12..16], y as u32);
    out[16] = b' ';
    put_decimal(&mut out[17..19], (tod / 3600) as u32);
    out[19] = b':';
    put_decimal(&mut out[20..22], ((tod % 3600) / 60) as u32);
    out[22] = b':';
    put_decimal(&mut out[23..25], (tod % 60) as u32);
    out[25..29].copy_from_slice(b" GMT");
    true
}

/// 定寬、左補零的十進位（寬度 = `out.len()`）。
fn put_decimal(out: &mut [u8], mut v: u32) {
    for b in out.iter_mut().rev() {
        *b = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// 解析三種 HTTP 日期，回傳 unix 秒。用系統時鐘當"現在"。
pub fn parse(s: &str) -> Option<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    parse_with_now(s, now)
}

/// 解析三種 HTTP 日期。`now_secs` 只被 RFC 850 那一檔用到（兩位年補世紀）。
pub fn parse_with_now(s: &str, now_secs: i64) -> Option<i64> {
    let s = s.trim();
    // 三種格式的分辨點在**第一個逗號**：
    //   IMF-fixdate  `Sun, 06 Nov 1994 08:49:37 GMT`   —— 逗號前 3 個字母
    //   RFC 850      `Sunday, 06-Nov-94 08:49:37 GMT`  —— 逗號前是全寫
    //   asctime      `Sun Nov  6 08:49:37 1994`        —— 沒有逗號
    match s.split_once(',') {
        None => parse_asctime(s),
        Some((dow, rest)) => {
            let rest = rest.trim_start();
            // ★ 判據不是"逗號前有多長"，而是**日期那一段自己長什麼樣**：
            //   `06-Nov-94` 裡有連字元，`06 Nov 1994` 裡沒有。
            //   按星期名長度分反而會被 `Sun,` 與 `Sunday,` 之外的寫法騙到。
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
    // ⚠ 尾巴必須是 GMT。RFC 9110 說得很明確：HTTP 日期**永遠**是 GMT，
    //   帶別的時區的那份訊息是壞的 —— 認了它等於替對方猜一個偏移量。
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

/// `Nov  6 08:49:37 1994`（日那一欄是**空格補位**，不是補零）
fn parse_asctime(s: &str) -> Option<i64> {
    let mut it = s.split_ascii_whitespace();
    // 星期名被忽略（見 DAY_NAMES 上那條註解），但**必須在**——少了它就不是 asctime。
    if it.next()?.len() < 3 {
        return None;
    }
    let m = month_from_name(it.next()?)?;
    // ⚠ 日那一欄是**空格補位**（`Nov  6`），所以這裡靠 `split_ascii_whitespace`
    //   把連續空格吃掉；換成按固定列切會在個位數日期上錯一格。
    let d: u32 = it.next()?.parse().ok()?;
    let tod = parse_hms(it.next()?)?;
    let y: i64 = it.next()?.parse().ok()?;
    to_unix(y, m, d, tod)
}

/// RFC 9110 §5.6.7 的兩位年規則：
/// **看起來在未來 50 年以上的，取最近的那個同末兩位的過去年份。**
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

/// `08:49:37` → 當天秒數。
fn parse_hms(s: &str) -> Option<i64> {
    let mut it = s.split(':');
    let h: i64 = it.next()?.parse().ok()?;
    let mi: i64 = it.next()?.parse().ok()?;
    let se: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    // ⚠ 閏秒：`60` 是合法的 HTTP 時間戳。把它折成 59 —— 拒掉它會讓一次
    //   條件請求整個失效，而差一秒不影響 `If-Modified-Since` 的判斷。
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

/// Howard Hinnant 的 `days_from_civil`：民用日期 → 距 1970-01-01 的天數。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 上面那個的反函式。
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

    /// RFC 9110 §5.6.7 自己舉的那個例子，三種寫法是**同一個時刻**。
    const IMF: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
    const RFC850: &str = "Sunday, 06-Nov-94 08:49:37 GMT";
    const ASCTIME: &str = "Sun Nov  6 08:49:37 1994";
    const EXPECT: i64 = 784_111_777;
    /// T00:00:00Z，給兩位年那條規則當"現在"。
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

    // ★ ★ 這條釘的是 G93 那半句「兩位年按 RFC 9110 附錄規則處理」。
    //   ⚠ 少了它，一個 1994 年的 `If-Modified-Since` 會被讀成 **2094 年**
    //   ⇒ 伺服器認為用戶端手上的副本比檔案還新，於是**永遠回 304**，
    //   而用戶端永遠拿不到更新後的檔案 —— 現場是「改了檔案但頁面不變」，
    //   沒有任何一條紀錄會說是日期解析錯了。
    #[test]
    fn rfc850_的两位年按_rfc9110_规则补世纪() {
        let year_of = |s: &str| civil_from_days(parse_with_now(s, NOW_2026).unwrap() / 86_400).0;
        // 現在是 2026：+50 年的界在 2076。
        assert_eq!(year_of("Sun, 06-Nov-94 08:49:37 GMT"), 1994, "94 → 1994");
        assert_eq!(year_of("Sun, 06-Nov-26 08:49:37 GMT"), 2026, "26 → 2026");
        // 正好 50 年 —— 規則說的是「50 年**以上**」，所以這一檔留在未來。
        assert_eq!(year_of("Sun, 06-Nov-76 08:49:37 GMT"), 2076, "76 → 2076");
        // 51 年 ⇒ 退一個世紀。★ 邊界兩側各釘一條，差一錯才跑不掉。
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
            "Sun, 32 Nov 1994 08:49:37 GMT", // 沒有 11 月 32 日
            "Sun, 29 Feb 1995 08:49:37 GMT", // 1995 不是閏年
            "Sun, 06 Xxx 1994 08:49:37 GMT", // 月份名不認識
            "Sun, 06 Nov 1994 24:49:37 GMT", // 小時越界
            "Sun, 06 Nov 1994 08:60:37 GMT", // 分鐘越界
            "Sun, 06-Nov-994 08:49:37 GMT",  // RFC 850 的年必須兩位
        ] {
            assert_eq!(parse_with_now(bad, NOW_2026), None, "本该拒掉：{bad:?}");
        }
    }

    #[test]
    fn 闰年的_2_月_29_日认() {
        assert!(parse_with_now("Tue, 29 Feb 2028 00:00:00 GMT", NOW_2026).is_some());
    }

    // ★ 閏秒折成 59 而不是拒 —— 拒掉會讓整條條件請求失效。
    #[test]
    fn 闰秒被折成_59_而不是拒掉() {
        assert_eq!(
            parse_with_now("Sun, 31 Dec 2028 23:59:60 GMT", NOW_2026),
            parse_with_now("Sun, 31 Dec 2028 23:59:59 GMT", NOW_2026),
        );
    }

    // ★ ★ 曆法自證：兩個方向來回一趟必須回到原處。
    //   ⚠ 少了這條，`days_from_civil` 裡一個差一的錯**只會在某些月份**顯形，
    //   而抽查很容易全落在沒事的那幾天上。
    #[test]
    fn 历法来回一趟必须回到原处() {
        // 1970-01-01 起每隔 97 天取一個點，跨 200 年 —— 覆蓋閏年、世紀年、非世紀年。
        let mut days = -70_000i64;
        while days < 60_000 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m:02}-{d:02} 对不上");
            days += 97;
        }
    }

    /// ★ 寫進定長緩衝的那一版（10-05 · 構回應頭不走 `fmt`）必須與 [`format_imf_secs`]
    ///   （`format!` 那一版，RFC 的例子釘著它）**逐位元組相同**。
    ///   掃 1970–9999 年：素數步長讓每一步落在不同的日、月、閏年與時分秒上；外加幾個邊界。
    #[test]
    fn 定长写法与_format_那一版逐字节相同() {
        const LAST: i64 = 253_402_300_799; // 9999-12-31T23:59:59Z
        let mut buf = [0u8; IMF_LEN];
        let mut n = 0;
        let mut secs = 0i64;
        while secs <= LAST {
            assert!(write_imf_secs(secs, &mut buf), "secs={secs}");
            assert_eq!(
                std::str::from_utf8(&buf).unwrap(),
                format_imf_secs(secs),
                "secs={secs}"
            );
            secs += 9_999_991;
            n += 1;
        }
        assert!(n > 25_000, "扫的点数 {n}");
        for s in [0, 59, 3_599, 86_399, 86_400, 951_782_400, EXPECT, LAST] {
            assert!(write_imf_secs(s, &mut buf), "secs={s}");
            assert_eq!(
                std::str::from_utf8(&buf).unwrap(),
                format_imf_secs(s),
                "secs={s}"
            );
        }
        // 年份超出四位：定長寫不下 ⇒ 回 false，呼叫方回落到 `format!` 那一版（位元組照舊）。
        assert!(!write_imf_secs(LAST + 1, &mut buf));
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
