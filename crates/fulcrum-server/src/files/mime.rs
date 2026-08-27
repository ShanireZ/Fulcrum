//! 扩展名 → `Content-Type`（G90）。
//!
//! ★ ★ **自带一张小表，新增 0 个包** —— 与自研 DNS 客户端（§10）、
//! 自研 PROXY protocol 编解码同一条口径：
//! **一个只需要查表的能力，不值得为它抬依赖面**（`mime_guess` 要 +2 个包，
//! 且要按 G29 纳入每周检查与 24 小时怀疑期）。
//!
//! ⚠ 代价认下：表会过期，新格式要手工补。而这一条与「猜错一个 MIME」相比是便宜的一侧。

/// 查不到时回这个。★ 不猜 —— 猜错一个 MIME 比回 `octet-stream` 更贵。
pub const DEFAULT: &str = "application/octet-stream";

/// `(小写扩展名, Content-Type)`，**按扩展名升序**。
///
/// ⚠ 带 `charset=utf-8` 的只有那几类**文本**格式：
/// 给二进制加 charset 是错的，而给文本不加会让浏览器去猜编码。
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

/// 按文件名末段的扩展名查表。查不到 → [`DEFAULT`]。
///
/// ⚠ 扩展名**大小写不敏感**（`FOO.PNG` 与 `foo.png` 同一个类型），
/// 而表里存的一律是小写。
pub fn for_name(name: &str) -> &'static str {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // ★ ★ 这条是**查表法自己的判据**：`for_name` 用的是二分查找，
    //   而二分查找在表没排序时不会报错 —— 它只是**查不到本来在表里的那几条**。
    //   于是现场是「某几个扩展名莫名其妙变成 octet-stream」，没有任何东西会红。
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

    // ⚠ `.tar.gz` 只看最后一段 ⇒ `application/gzip`。这是有意的，钉住免得有人"顺手改成"
    //   多段匹配 —— 多段匹配要回答 `.tar.gz` 是 tar 还是 gzip，而那没有唯一答案。
    #[test]
    fn 只看最后一段扩展名() {
        assert_eq!(for_name("a.tar.gz"), "application/gzip");
    }

    // ★ 隐藏文件（点开头、没有第二个点）不该被当成「扩展名 = 文件名」。
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
