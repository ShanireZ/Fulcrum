//! `Range` 頭：**只做單段**（G89）。
//!
//! ⚠ 多段 `multipart/byteranges` **不做** —— 實作與判據成本與真實用途不成比例。
//! 收到多段請求時**回 200 全量**，那是 RFC 9110 明確允許的
//! （「接收方可以忽略 Range 頭」），而不是一個錯誤。
//!
//! ★ ★ 這裡最容易寫錯的是**邊界**，而邊界錯了不會回報錯誤、只會少發或多發幾個位元組：
//! `bytes=0-0` 是 **1 個位元組**（閉區間），`bytes=-0` 是**不可滿足**（要末尾 0 個位元組）。
//! 下面每一條都有判據。

/// 一次單段 Range 的判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeVerdict {
    /// 就按 `[start, end]`（**閉區間**）發，回 206。
    Single { start: u64, end: u64 },
    /// 語法認得，但落在檔案之外 → **416 + `Content-Range: bytes */len`**。
    Unsatisfiable,
    /// 忽略這個頭，照常回 200 全量。
    ///
    /// ★ 三種情況都落這裡，而它們**不該**被區分開：語法不認識、
    /// 單位不是 `bytes`、是多段。RFC 允許忽略，忽略之後的行為完全一樣。
    Ignore,
}

/// 解析 `Range` 頭。`len` 是檔案位元組數。
pub fn parse(header: &str, len: u64) -> RangeVerdict {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        // 單位不是 bytes（RFC 只定義了 bytes 一種）。
        return RangeVerdict::Ignore;
    };
    let mut parts = spec.split(',');
    let Some(first) = parts.next() else {
        return RangeVerdict::Ignore;
    };
    // ⚠ 多段 → 忽略、回 200 全量（G89）。
    if parts.next().is_some() {
        return RangeVerdict::Ignore;
    }
    let first = first.trim();
    let Some((a, b)) = first.split_once('-') else {
        return RangeVerdict::Ignore;
    };
    let (a, b) = (a.trim(), b.trim());

    match (a.is_empty(), b.is_empty()) {
        // `-n`：末尾 n 個位元組。
        (true, false) => {
            let Ok(n) = b.parse::<u64>() else {
                return RangeVerdict::Ignore;
            };
            // ★ `bytes=-0` = 「末尾 0 個位元組」= 不可滿足，**不是**整個檔案。
            if n == 0 || len == 0 {
                return RangeVerdict::Unsatisfiable;
            }
            let start = len.saturating_sub(n);
            RangeVerdict::Single {
                start,
                end: len - 1,
            }
        }
        // `a-`：從 a 到末尾。
        (false, true) => {
            let Ok(start) = a.parse::<u64>() else {
                return RangeVerdict::Ignore;
            };
            if start >= len {
                return RangeVerdict::Unsatisfiable;
            }
            RangeVerdict::Single {
                start,
                end: len - 1,
            }
        }
        // `a-b`：閉區間。
        (false, false) => {
            let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) else {
                return RangeVerdict::Ignore;
            };
            if start > end || start >= len {
                return RangeVerdict::Unsatisfiable;
            }
            // ⚠ 末端**截到檔案尾**，不是拒 —— `bytes=0-999999` 要的是整個檔案。
            RangeVerdict::Single {
                start,
                end: end.min(len - 1),
            }
        }
        // `-` 光一個連字元：語法不對。
        (true, true) => RangeVerdict::Ignore,
    }
}

/// 416 要帶的 `Content-Range: bytes */len`。
pub fn unsatisfiable_header(len: u64) -> String {
    format!("bytes */{len}")
}

/// 206 要帶的 `Content-Range: bytes start-end/len`。
pub fn content_range(start: u64, end: u64, len: u64) -> String {
    format!("bytes {start}-{end}/{len}")
}

#[cfg(test)]
mod tests {
    use super::RangeVerdict::*;
    use super::*;

    #[test]
    fn 三种单段写法各自算对() {
        assert_eq!(parse("bytes=0-499", 1000), Single { start: 0, end: 499 });
        assert_eq!(
            parse("bytes=500-", 1000),
            Single {
                start: 500,
                end: 999
            }
        );
        assert_eq!(
            parse("bytes=-500", 1000),
            Single {
                start: 500,
                end: 999
            }
        );
    }

    // ★ 閉區間：`0-0` 是**一個**位元組。差一錯就在這裡。
    #[test]
    fn 闭区间_单字节() {
        assert_eq!(parse("bytes=0-0", 1000), Single { start: 0, end: 0 });
        assert_eq!(
            parse("bytes=999-999", 1000),
            Single {
                start: 999,
                end: 999
            }
        );
    }

    // ★ ★ `bytes=-0` 要的是「末尾 0 個位元組」⇒ **不可滿足**，不是整個檔案。
    //   ⚠ 少了這條判據，`-0` 很容易被 `len - 0` 算成 `start = len` 再往下走。
    #[test]
    fn 末尾零字节是不可满足而不是整个文件() {
        assert_eq!(parse("bytes=-0", 1000), Unsatisfiable);
    }

    #[test]
    fn 末端超过文件尾要截住而不是拒() {
        assert_eq!(parse("bytes=0-999999", 1000), Single { start: 0, end: 999 });
    }

    #[test]
    fn 起点在文件之外是不可满足() {
        assert_eq!(parse("bytes=1000-", 1000), Unsatisfiable);
        assert_eq!(parse("bytes=1000-2000", 1000), Unsatisfiable);
        assert_eq!(parse("bytes=5-3", 1000), Unsatisfiable);
    }

    // `-n` 比檔案還大 ⇒ 就是整個檔案（RFC 9110 明說要截）。
    #[test]
    fn 末尾_n_大于文件长度就是整个文件() {
        assert_eq!(parse("bytes=-5000", 1000), Single { start: 0, end: 999 });
    }

    #[test]
    fn 空文件上任何范围都不可满足() {
        assert_eq!(parse("bytes=0-", 0), Unsatisfiable);
        assert_eq!(parse("bytes=-1", 0), Unsatisfiable);
        assert_eq!(parse("bytes=0-0", 0), Unsatisfiable);
    }

    // ★ 多段忽略 ⇒ 回 200 全量（G89 明確不做 multipart/byteranges）。
    #[test]
    fn 多段被忽略而不是报错() {
        assert_eq!(parse("bytes=0-99,200-299", 1000), Ignore);
    }

    #[test]
    fn 不认识的写法一律忽略() {
        for bad in [
            "items=0-10", // 單位不是 bytes
            "bytes=abc",  // 沒有連字元
            "bytes=-",    // 光一個連字元
            "bytes=a-b",  // 不是數字
            "bytes=",     // 空
            "0-499",      // 少了 bytes=
        ] {
            assert_eq!(parse(bad, 1000), Ignore, "本该忽略：{bad:?}");
        }
    }

    #[test]
    fn 两个头字段的写法() {
        assert_eq!(content_range(0, 499, 1000), "bytes 0-499/1000");
        assert_eq!(unsatisfiable_header(1000), "bytes */1000");
    }

    // ★ ★ 長度自證：`Single` 的位元組數必須等於 `end - start + 1`，
    //   而且必須落在檔案之內。⚠ 這條是拿**全部**判據的輸出再核一遍 ——
    //   單看每條斷言都容易只盯住自己關心的那一端。
    #[test]
    fn 凡是_single_都必须落在文件之内且长度自洽() {
        for spec in [
            "bytes=0-499",
            "bytes=500-",
            "bytes=-500",
            "bytes=0-0",
            "bytes=0-999999",
            "bytes=-5000",
        ] {
            let Single { start, end } = parse(spec, 1000) else {
                panic!("{spec} 应该是 Single");
            };
            assert!(start <= end, "{spec}: start > end");
            assert!(end < 1000, "{spec}: 末端跑到文件之外");
        }
    }
}
