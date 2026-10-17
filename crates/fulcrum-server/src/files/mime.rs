//! 副檔名 → `Content-Type`（G90）。
//!
//! ★ ★ **自帶一張小表，新增 0 個包** —— 與自研 DNS 用戶端（§10）、
//! 自研 PROXY protocol 編解碼同一條口徑：
//! **一個只需要查表的能力，不值得為它抬相依面**（`mime_guess` 要 +2 個包，
//! 且要按 G29 納入每週檢查與 24 小時懷疑期）。
//!
//! ⚠ 代價認下：表會過期，新格式要手動補。而這一條與「猜錯一個 MIME」相比是便宜的一側。

/// 查不到時回這個。★ 不猜 —— 猜錯一個 MIME 比回 `octet-stream` 更貴。
pub const DEFAULT: &str = "application/octet-stream";

/// `(小写扩展名, Content-Type)`，**按副檔名升序**。
///
/// ⚠ 帶 `charset=utf-8` 的只有那幾類**文字**格式：
/// 給二進位加 charset 是錯的，而給文字不加會讓瀏覽器去猜編碼。
const TABLE: &[(&str, &str)] = &[
    ("7z", "application/x-7z-compressed"),
    ("aac", "audio/aac"),
    ("apng", "image/apng"),
    ("atom", "application/atom+xml"),
    ("avi", "video/x-msvideo"),
    ("avif", "image/avif"),
    ("bin", "application/octet-stream"),
    ("bmp", "image/bmp"),
    ("bz2", "application/x-bzip2"),
    ("css", "text/css; charset=utf-8"),
    ("csv", "text/csv; charset=utf-8"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("eot", "application/vnd.ms-fontobject"),
    ("epub", "application/epub+zip"),
    ("flac", "audio/flac"),
    ("gif", "image/gif"),
    ("gz", "application/gzip"),
    ("htm", "text/html; charset=utf-8"),
    ("html", "text/html; charset=utf-8"),
    ("ico", "image/x-icon"),
    ("ics", "text/calendar; charset=utf-8"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("js", "text/javascript; charset=utf-8"),
    ("json", "application/json"),
    ("jsonld", "application/ld+json"),
    ("m4a", "audio/mp4"),
    ("map", "application/json"),
    ("md", "text/markdown; charset=utf-8"),
    ("mjs", "text/javascript; charset=utf-8"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("mpeg", "video/mpeg"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("ogv", "video/ogg"),
    ("opus", "audio/ogg"),
    ("otf", "font/otf"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("rar", "application/vnd.rar"),
    ("rss", "application/rss+xml"),
    ("rtf", "application/rtf"),
    ("svg", "image/svg+xml"),
    ("tar", "application/x-tar"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("toml", "application/toml"),
    ("ts", "video/mp2t"),
    ("ttf", "font/ttf"),
    ("txt", "text/plain; charset=utf-8"),
    ("wasm", "application/wasm"),
    ("wav", "audio/wav"),
    ("weba", "audio/webm"),
    ("webm", "video/webm"),
    ("webp", "image/webp"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("xhtml", "application/xhtml+xml"),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xml", "text/xml; charset=utf-8"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("zip", "application/zip"),
    ("zst", "application/zstd"),
];

/// 按檔案名末段的副檔名查表。查不到 → [`DEFAULT`]。
///
/// ⚠ 副檔名**大小寫不敏感**（`FOO.PNG` 與 `foo.png` 同一個類型），
/// 而表裡存的一律是小寫。
pub fn for_name(name: &str) -> &'static str {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return DEFAULT;
    };
    if ext.is_empty() {
        return DEFAULT;
    }
    // ★ 轉小寫寫進棧上的緩衝（10-14 · 檔案那條路剩下的分配），⛔ 不每請求分配一個 `String`。
    //   比 [`MAX_EXT`] 長的副檔名一定不在表裡（`表里的扩展名都放得进栈上那块缓冲` 釘著）⇒ 直接回預設。
    //   按位元組比與原來按 `&str` 比是同一個序（`str` 的 `Ord` 就是按位元組）。
    let mut buf = [0u8; MAX_EXT];
    let Some(lower) = buf.get_mut(..ext.len()) else {
        return DEFAULT;
    };
    for (d, s) in lower.iter_mut().zip(ext.bytes()) {
        *d = s.to_ascii_lowercase();
    }
    match TABLE.binary_search_by(|(k, _)| k.as_bytes().cmp(lower)) {
        Ok(i) => TABLE[i].1,
        Err(_) => DEFAULT,
    }
}

/// 棧上那塊緩衝的長度：表裡最長的副檔名放得下就夠。
const MAX_EXT: usize = 16;

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ 「比緩衝長的一定不在表裡」那條捷徑的前提；也釘住表裡一律是小寫（查表前轉的就是小寫）。
    #[test]
    fn 表里的扩展名都放得进栈上那块缓冲() {
        for (k, _) in TABLE {
            assert!(k.len() <= MAX_EXT, "{k} 比 MAX_EXT={MAX_EXT} 长");
            assert_eq!(*k, k.to_ascii_lowercase(), "{k} 不是小写");
        }
    }

    /// 大小寫不敏感、表裡與表外、長副檔名，與原來「轉成 `String` 再查」的寫法逐個相同。
    #[test]
    fn 与原来转成_string_再查的写法相同() {
        let old = |name: &str| -> &'static str {
            let Some((_, ext)) = name.rsplit_once('.') else {
                return DEFAULT;
            };
            if ext.is_empty() {
                return DEFAULT;
            }
            let lower = ext.to_ascii_lowercase();
            match TABLE.binary_search_by(|(k, _)| (*k).cmp(lower.as_str())) {
                Ok(i) => TABLE[i].1,
                Err(_) => DEFAULT,
            }
        };
        let mut names: Vec<String> = TABLE.iter().map(|(k, _)| format!("f.{k}")).collect();
        names.extend(
            TABLE
                .iter()
                .map(|(k, _)| format!("F.{}", k.to_ascii_uppercase())),
        );
        names.extend(
            [
                "noext",
                "trailing.",
                ".hidden",
                "a.b.HTML",
                "x.unknown",
                "x.Ünï",
                "x.中文",
                "x.aaaaaaaaaaaaaaaa",
                "x.aaaaaaaaaaaaaaaaa",
                "x.JSONLD",
            ]
            .map(String::from),
        );
        for n in &names {
            assert_eq!(for_name(n), old(n), "{n}");
        }
    }

    // ★ ★ 這條是**查表法自己的判據**：`for_name` 用的是二分查找，
    //   而二分查找在表沒排序時不會回報錯誤 —— 它只是**查不到本來在表裡的那幾條**。
    //   於是現場是「某幾個副檔名莫名其妙變成 octet-stream」，沒有任何東西會紅。
    #[test]
    fn 表必须按扩展名升序_否则二分查找会安静地查不到() {
        for w in TABLE.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "表没排序：`{}` 排在 `{}` 前面",
                w[0].0,
                w[1].0
            );
        }
    }

    /// ★ `file_server` 用 `HeaderValue::from_static` 發這張表裡的值（10-05 · 構回應頭不再分配），
    ///   而 `from_static` 遇到非法字元會**在請求路徑上 panic** ⇒ 每個值連同預設值都釘住。
    #[test]
    fn 表里每个值都是合法头值() {
        for (ext, ty) in TABLE.iter().chain(std::iter::once(&("（缺省）", DEFAULT))) {
            assert!(http::HeaderValue::from_str(ty).is_ok(), ".{ext} → {ty}");
        }
    }

    #[test]
    fn 表里每一条都真的查得到() {
        for (ext, ty) in TABLE {
            assert_eq!(for_name(&format!("x.{ext}")), *ty, "查不到 .{ext}");
        }
    }

    #[test]
    fn 扩展名大小写不敏感() {
        assert_eq!(for_name("A.PNG"), "image/png");
        assert_eq!(for_name("a.PnG"), "image/png");
    }

    #[test]
    fn 查不到与没有扩展名都回默认值() {
        assert_eq!(for_name("README"), DEFAULT);
        assert_eq!(for_name("x.qqq"), DEFAULT);
        assert_eq!(for_name("x."), DEFAULT);
        assert_eq!(for_name(""), DEFAULT);
    }

    // ⚠ `.tar.gz` 只看最後一段 ⇒ `application/gzip`。這是有意的，釘住免得有人"順手改成"
    //   多段匹配 —— 多段匹配要回答 `.tar.gz` 是 tar 還是 gzip，而那沒有唯一答案。
    #[test]
    fn 只看最后一段扩展名() {
        assert_eq!(for_name("a.tar.gz"), "application/gzip");
    }

    // ★ 隱藏檔案（點開頭、沒有第二個點）不該被當成「副檔名 = 檔案名」。
    #[test]
    fn 点开头的文件名不算扩展名() {
        assert_eq!(for_name(".env"), DEFAULT);
        assert_eq!(for_name(".gitignore"), DEFAULT);
    }

    #[test]
    fn 文本类带_charset_二进制类不带() {
        assert!(for_name("a.html").contains("charset=utf-8"));
        assert!(for_name("a.css").contains("charset=utf-8"));
        assert!(!for_name("a.png").contains("charset"));
        assert!(!for_name("a.wasm").contains("charset"));
    }
}
