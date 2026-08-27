//! `Range` 头：**只做单段**（G89）。
//!
//! ⚠ 多段 `multipart/byteranges` **不做** —— 实现与判据成本与真实用途不成比例。
//! 收到多段请求时**回 200 全量**，那是 RFC 9110 明确允许的
//! （「接收方可以忽略 Range 头」），而不是一个错误。
//!
//! ★ ★ 这里最容易写错的是**边界**，而边界错了不会报错、只会少发或多发几个字节：
//! `bytes=0-0` 是 **1 个字节**（闭区间），`bytes=-0` 是**不可满足**（要末尾 0 个字节）。
//! 下面每一条都有判据。

/// 一次单段 Range 的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeVerdict {
    /// 就按 `[start, end]`（**闭区间**）发，回 206。
    Single { start: u64, end: u64 },
    /// 语法认得，但落在文件之外 → **416 + `Content-Range: bytes */len`**。
    Unsatisfiable,
    /// 忽略这个头，照常回 200 全量。
    ///
    /// ★ 三种情况都落这里，而它们**不该**被区分开：语法不认识、
    /// 单位不是 `bytes`、是多段。RFC 允许忽略，忽略之后的行为完全一样。
    Ignore,
}

/// 解析 `Range` 头。`len` 是文件字节数。
pub fn parse(header: &str, len: u64) -> RangeVerdict {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        // 单位不是 bytes（RFC 只定义了 bytes 一种）。
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
        // `-n`：末尾 n 个字节。
        (true, false) => {
            let Ok(n) = b.parse::<u64>() else {
                return RangeVerdict::Ignore;
            };
            // ★ `bytes=-0` = 「末尾 0 个字节」= 不可满足，**不是**整个文件。
            if n == 0 || len == 0 {
                return RangeVerdict::Unsatisfiable;
            }
            let start = len.saturating_sub(n);
            RangeVerdict::Single {
                start,
                end: len - 1,
            }
        }
        // `a-`：从 a 到末尾。
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
        // `a-b`：闭区间。
        (false, false) => {
            let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) else {
                return RangeVerdict::Ignore;
            };
            if start > end || start >= len {
                return RangeVerdict::Unsatisfiable;
            }
            // ⚠ 末端**截到文件尾**，不是拒 —— `bytes=0-999999` 要的是整个文件。
            RangeVerdict::Single {
                start,
                end: end.min(len - 1),
            }
        }
        // `-` 光一个连字符：语法不对。
        (true, true) => RangeVerdict::Ignore,
    }
}

/// 416 要带的 `Content-Range: bytes */len`。
pub fn unsatisfiable_header(len: u64) -> String {
    format!("bytes */{len}")
}

/// 206 要带的 `Content-Range: bytes start-end/len`。
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

    // ★ 闭区间：`0-0` 是**一个**字节。差一错就在这里。
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

    // ★ ★ `bytes=-0` 要的是「末尾 0 个字节」⇒ **不可满足**，不是整个文件。
    //   ⚠ 少了这条判据，`-0` 很容易被 `len - 0` 算成 `start = len` 再往下走。
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

    // `-n` 比文件还大 ⇒ 就是整个文件（RFC 9110 明说要截）。
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

    // ★ 多段忽略 ⇒ 回 200 全量（G89 明确不做 multipart/byteranges）。
    #[test]
    fn 多段被忽略而不是报错() {
        assert_eq!(parse("bytes=0-99,200-299", 1000), Ignore);
    }

    #[test]
    fn 不认识的写法一律忽略() {
        for bad in [
            "items=0-10", // 单位不是 bytes
            "bytes=abc",  // 没有连字符
            "bytes=-",    // 光一个连字符
            "bytes=a-b",  // 不是数字
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

    // ★ ★ 长度自证：`Single` 的字节数必须等于 `end - start + 1`，
    //   而且必须落在文件之内。⚠ 这条是拿**全部**判据的输出再核一遍 ——
    //   单看每条断言都容易只盯住自己关心的那一端。
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
