//! 自研静态文件服务（**M2 批 F**，G87–G91）。
//!
//! 在这一批之前，`file_server` 编译成一个带回落标记的终结类步骤、由 nginx 顶班。
//! 现在它自己发文件。
//!
//! ## 做到哪（G89）
//!
//! 发文件 · 目录索引 · 目录不带尾斜杠 **301** 到带斜杠 · `browse` 目录列表 ·
//! `ETag` / `Last-Modified` / `If-None-Match` / `If-Modified-Since` → **304** ·
//! **单段** `Range` 与 `If-Range` · 方法只认 `GET`/`HEAD`，其余 **405** 带 `Allow`。
//!
//! **明确不做**（G89，代价已认下）：多段 `multipart/byteranges`（回 200 全量）、
//! 预压缩旁文件 `.gz`/`.br`（`encode` 本身还在 `UNWIRED` 里，
//! 先做旁文件会让「压缩」有两个互不知道的实现）。
//!
//! ## 请求路径的次序 —— ★ ★ 顺序本身就是判据
//!
//! 1. 方法 → 2. 解码 → 3. 归一 → 4. hide → 5. 符号链接 → 6. metadata
//!    → 7. 目录（301 / index / browse）→ 8. 文件头 → 9. 条件请求 → 10. Range → 11. 发送
//!
//! 前四步全在 [`path`] 里，是纯函数、有自己的单测。
//!
//! ## ⚠ 阻塞 IO 走 `spawn_blocking`
//!
//! 这里的 `std::fs` 调用全部包在 [`tokio::task::spawn_blocking`] 里。
//! 直接在 async 里做文件 IO 会**占住一个工作线程** —— 页缓存命中时只是几微秒，
//! 而一次冷读（或一块坏盘）能把它占到毫秒级，那时整个 worker 上的其它连接一起卡住。
//! ★ 代价认下：每 64 KiB 一次 `spawn_blocking`。这一批**不为吞吐做优化**，
//! 性能是 M3 的事 —— 而在有数字之前先猜一个实现，是本仓库反复点名的那种做法。

pub mod httpdate;
pub mod mime;
pub mod path;
pub mod range;

use crate::Downstream;
use bytes::Bytes;
use fulcrum_runtime::FileServerRt;
use log::{debug, warn};
use pingora_http::{RequestHeader, ResponseHeader};
use std::fs::Metadata;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一次读多少。★ 64 KiB 是个**没有实测依据**的起点，写在这里是为了让它可被质疑。
const CHUNK: usize = 64 * 1024;

/// 发文件。
///
/// `Ok(())` = 已经把响应写完了。
/// `Err(status)` = 请调用方按**站点的 `handle_errors`** 渲染这个状态码
/// （与 `Outcome::NoRouteMatch` 一致）。
///
/// ⚠ **405 不走这条 `Err` 路**：RFC 9110 要求 405 必须带 `Allow`，
/// 而错误处理器那条路没有地方放额外的头。⇒ 405 在这里直接写掉。
/// ★ 代价写在明处：站点的 `handle_errors` 定制不到 405 那一页。
//
// ⚠ **`pub(crate)` 而不是 `pub`**（批 J 第六步）：它收的是
// `crate::Downstream`，而那个类型是 `pub(crate)` 的 —— 一个 `pub` 函数收
// 一个够不到的类型，crate 外面拿它无处可用，`private_interfaces` 会判警告。
// ★ 收窄的是**没人在用的那一半可见性**：本 crate 之外一处调用都没有。
pub(crate) async fn serve(
    session: &mut Downstream<'_>,
    req: &RequestHeader,
    fs: &FileServerRt,
    url_path: &str,
    query: &str,
    encoder: Option<crate::encode::Encoder>,
) -> Result<(), u16> {
    // ── 1. 方法 ────────────────────────────────────────────────────────────
    let method = req.method.as_str();
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            debug!("file_server：不认识的方法 {method} → 405");
            write_head(
                session,
                405,
                vec![
                    ("Allow".into(), "GET, HEAD".into()),
                    ("Content-Length".into(), "0".into()),
                ],
            )
            .await;
            return Ok(());
        }
    };

    // ── 2–4. 解码 → 归一 → hide ────────────────────────────────────────────
    let Some(norm) = path::decode_and_normalize(url_path) else {
        debug!("file_server：路径解不开或越界 → 400（{url_path}）");
        return Err(400);
    };
    if path::is_hidden(&norm.segments, &fs.hide) {
        // ★ ★ 回 **404 不是 403**（G88）：403 等于确认「这个文件在」。
        debug!("file_server：命中 hide 清单 → 404（{url_path}）");
        return Err(404);
    }

    // ── 5. join 到 root，必要时按 canonicalize 校验 ─────────────────────────
    let mut full = PathBuf::from(&fs.root);
    for s in &norm.segments {
        full.push(s);
    }
    if !fs.follow_symlinks && !within_root(&fs.root, &full).await {
        // ★ 这里回 403 而不是 404，与 hide 那条**有意不同**：
        //   hide 挡的是「访客不该知道它在」，而这里是「部署方自己放的一条链接
        //   指到了 root 之外」—— 那是一条要让运维看见的配置事实，藏起来只会让人查半天。
        warn!("file_server：`{}` 解析到 root 之外 → 403", full.display());
        return Err(403);
    }

    // ── 6. metadata ───────────────────────────────────────────────────────
    let Some(meta) = stat(&full).await else {
        debug!("file_server：不存在 → 404（{}）", full.display());
        return Err(404);
    };

    // ── 7. 目录 ───────────────────────────────────────────────────────────
    if meta.is_dir() {
        // 不带尾斜杠 → 301 到带斜杠。★ **查询串要保留**。
        if !norm.trailing_slash {
            let mut to = String::from(url_path);
            to.push('/');
            if !query.is_empty() {
                to.push('?');
                to.push_str(query);
            }
            debug!("file_server：目录缺尾斜杠 → 301 {to}");
            write_head(
                session,
                301,
                vec![
                    ("Location".into(), to),
                    ("Content-Length".into(), "0".into()),
                ],
            )
            .await;
            return Ok(());
        }
        // 按 index 顺序找。
        for name in &fs.index {
            let cand = full.join(name);
            if let Some(m) = stat(&cand).await
                && m.is_file()
            {
                return send_file(session, req, fs, &cand, &m, head_only, encoder).await;
            }
        }
        if fs.browse {
            return browse(session, &full, url_path, head_only, encoder).await;
        }
        debug!(
            "file_server：目录没有索引且没开 browse → 404（{}）",
            full.display()
        );
        return Err(404);
    }

    if !meta.is_file() {
        // 设备文件、socket、FIFO —— 发不了，也不该假装它不在之外的任何事。
        debug!("file_server：不是普通文件 → 404（{}）", full.display());
        return Err(404);
    }

    // ── 8–11 ──────────────────────────────────────────────────────────────
    send_file(session, req, fs, &full, &meta, head_only, encoder).await
}

/// `follow_symlinks false` 时的校验：canonicalize 之后必须仍在 root 之内。
///
/// ⚠ ⚠ 用的是 [`Path::starts_with`]（**按路径段**比），不是字符串前缀比。
/// 字符串前缀会让 root=`/srv/www` 放过 `/srv/wwwevil` —— 而两者都以那 8 个字符开头。
async fn within_root(root: &str, full: &Path) -> bool {
    let root = root.to_string();
    let full = full.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let Ok(root_c) = std::fs::canonicalize(&root) else {
            return false;
        };
        match std::fs::canonicalize(&full) {
            Ok(c) => c.starts_with(&root_c),
            // 不存在 ⇒ 交给下一步的 metadata 去回 404。
            // ★ 这里回 true 不是「放行」，是「这一关不作判断」——
            //   回 false 会把一次 404 变成一次 403，两件事在现场必须分得开。
            Err(_) => true,
        }
    })
    .await
    .unwrap_or(false)
}

async fn stat(p: &Path) -> Option<Metadata> {
    let p = p.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::metadata(&p).ok())
        .await
        .ok()
        .flatten()
}

/// 预压缩旁文件的后缀（**M2 批 I**，G99）。
///
/// ★ 与 Caddy 的 `precompressed` 同名同后缀 —— 写文档的人不必再记一套。
const SIDECAR_EXT: &[(&str, &str)] = &[("gzip", "gz"), ("br", "br"), ("zstd", "zst")];

/// 这一次要发的**表示**（representation）。
///
/// ★ ★ 预压缩旁文件在这里只是「换一个要读的文件」—— 于是 ETag、`Content-Length`、
/// 304、Range、分块发送**全部自动落在旁文件上**，而那正是对的：
/// 被选中的表示就是旁文件那几个字节。
/// ⇒ ★ **预压缩比现压严格更好**：现压会掉强 ETag、掉 `Content-Length`、
/// 掉 `Accept-Ranges`，而旁文件一样都不掉。
struct Repr<'a> {
    /// 真正要读的那个文件（预压缩时是旁文件）。
    path: &'a Path,
    meta: &'a Metadata,
    /// `Content-Type` 按**原始**文件名算。
    ///
    /// ⚠ ★ 旁文件叫 `x.css.br`，按它算会得到 `application/octet-stream` ——
    /// 浏览器就不把它当 CSS 用了，而页面只是「样式没生效」，不报任何错。
    ctype: &'static str,
    /// 非 `None` = 这是预压缩旁文件，要宣布这个 `Content-Encoding`。
    encoding: Option<&'static str>,
}

/// 按客户端的 `Accept-Encoding` 顺序挑一个**真的存在且不陈**的旁文件。
///
/// # ★ ★ 为什么这里比 nginx / Caddy 多一道 mtime 检查
///
/// 那两家默认**不比**旁文件与原文件的时间。⚠ 于是一个改了 `x.css`
/// 却忘了重新生成 `x.css.br` 的部署，会让**只有支持 br 的那部分用户**
/// 拿到旧内容 —— 而它不报任何错、日志里一行都没有、开发者自己的浏览器
/// 还很可能拿到新的。★ 那正是本仓库反复点名的那一类。
/// ⇒ 旁文件**旧于**原文件就当它不存在。代价是一次 `stat`，
/// 而那次 `stat` 本来就要做（要拿它的大小与 mtime）。
async fn pick_sidecar(
    fs: &FileServerRt,
    req: &RequestHeader,
    full: &Path,
    meta: &Metadata,
) -> Option<(PathBuf, Metadata, &'static str)> {
    if fs.precompressed.is_empty() {
        return None;
    }
    let accept = header(req, "accept-encoding")?;
    let base_mtime = meta.modified().ok();
    // ★ 按**客户端的**顺序走，不是按配置的顺序 ——
    //   `Accept-Encoding` 里的顺序是客户端的偏好，而那是它的事不是我们的。
    for token in accept.split(',') {
        let name = token
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let Some((_, ext)) = SIDECAR_EXT.iter().find(|(algo, _)| *algo == name) else {
            continue;
        };
        if !fs.precompressed.iter().any(|p| p == &name) {
            continue;
        }
        let mut cand = full.as_os_str().to_os_string();
        cand.push(".");
        cand.push(ext);
        let cand = PathBuf::from(cand);
        let Some(m) = stat(&cand).await else { continue };
        if !m.is_file() {
            continue;
        }
        // ★ 陈旧就当不存在（理由见上）。
        if let (Some(bm), Ok(sm)) = (base_mtime, m.modified())
            && sm < bm
        {
            debug!(
                "file_server：旁文件 {} 比原文件旧 —— 当它不存在",
                cand.display()
            );
            continue;
        }
        let algo: &'static str = SIDECAR_EXT
            .iter()
            .find(|(a, _)| *a == name)
            .map(|(a, _)| *a)?;
        return Some((cand, m, algo));
    }
    None
}

/// 8–11 步：选表示 → 头 → 条件请求 → Range → 发送。
async fn send_file(
    session: &mut Downstream<'_>,
    req: &RequestHeader,
    fs: &FileServerRt,
    full: &Path,
    meta: &Metadata,
    head_only: bool,
    encoder: Option<crate::encode::Encoder>,
) -> Result<(), u16> {
    let base_name = full.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let ctype = mime::for_name(base_name);
    // ★ ★ 预压缩旁文件优先（**G99**）：它既省 CPU，又把强 ETag / Range /
    //   `Content-Length` 全部保住 —— 而现压那三样都保不住。
    let side = pick_sidecar(fs, req, full, meta).await;
    let repr = match &side {
        Some((p, m, algo)) => Repr {
            path: p,
            meta: m,
            ctype,
            encoding: Some(algo),
        },
        None => Repr {
            path: full,
            meta,
            ctype,
            encoding: None,
        },
    };
    // ⚠ 已经是预压缩的就不再现压一遍。
    //   ★ 它不靠这一行也不会双重压缩（压缩层看见 `Content-Encoding` 就放行），
    //   但显式写出来比靠另一层的行为可靠。
    let encoder = if side.is_some() { None } else { encoder };

    let len = repr.meta.len();
    let ctype = repr.ctype;
    let mtime = repr.meta.modified().ok();
    let etag = etag_of(repr.meta);
    let last_mod = mtime.and_then(httpdate::format_imf);
    let full = repr.path;

    // ── 9. 条件请求 ───────────────────────────────────────────────────────
    //
    // ★ 次序照 RFC 9110 §13.2.2：`If-None-Match` **优先**，
    //   它在场时 `If-Modified-Since` 一个字都不看。
    let fresh = match header(req, "if-none-match") {
        Some(inm) => etag.as_deref().is_some_and(|e| if_none_match_hit(inm, e)),
        None => match (header(req, "if-modified-since"), mtime) {
            (Some(ims), Some(mt)) => match (httpdate::parse(ims), unix_secs(mt)) {
                // ⚠ 比的是**秒**：`Last-Modified` 本来就只有秒精度，
                //   拿纳秒去比会让「同一秒内的文件」永远不新鲜。
                (Some(since), Some(m)) => m <= since,
                _ => false,
            },
            _ => false,
        },
    };
    if fresh {
        let mut extra = vec![];
        push_validators(&mut extra, &etag, &last_mod);
        debug!("file_server：条件请求命中 → 304（{}）", full.display());
        write_head(session, 304, extra).await;
        return Ok(());
    }

    // ── 10. Range ─────────────────────────────────────────────────────────
    //
    // `If-Range` 不匹配 ⇒ **忽略 Range**、回 200 全量（RFC 9110 §13.1.5）。
    let range_ok = match header(req, "if-range") {
        None => true,
        Some(ir) => if_range_matches(ir, &etag, mtime),
    };
    let verdict = match (range_ok, header(req, "range")) {
        (true, Some(r)) => range::parse(r, len),
        _ => range::RangeVerdict::Ignore,
    };

    let (status, start, end) = match verdict {
        range::RangeVerdict::Unsatisfiable => {
            debug!("file_server：Range 不可满足 → 416（{}）", full.display());
            let mut extra = vec![
                ("Content-Range".into(), range::unsatisfiable_header(len)),
                ("Content-Length".into(), "0".into()),
                ("Accept-Ranges".into(), "bytes".into()),
            ];
            push_validators(&mut extra, &etag, &last_mod);
            write_head(session, 416, extra).await;
            return Ok(());
        }
        range::RangeVerdict::Single { start, end } => (206, start, end),
        // 空文件走这里：`0..=-1` 表示不出来，所以用 `len == 0` 单独判。
        range::RangeVerdict::Ignore if len == 0 => (200, 0, 0),
        range::RangeVerdict::Ignore => (200, 0, len - 1),
    };
    let body_len = if len == 0 { 0 } else { end - start + 1 };

    let mut extra = vec![
        ("Content-Type".into(), ctype.to_string()),
        ("Content-Length".into(), body_len.to_string()),
        ("Accept-Ranges".into(), "bytes".into()),
    ];
    push_validators(&mut extra, &etag, &last_mod);
    if status == 206 {
        extra.push((
            "Content-Range".into(),
            range::content_range(start, end, len),
        ));
    }
    // ★ 预压缩旁文件：宣布编码，并按 RFC 9110 补 `Vary`。
    //   ⚠ 少了 `Vary`，下游任何一层缓存都会把这份 br 的字节发给不认 br 的客户端。
    if let Some(algo) = repr.encoding {
        extra.push(("Content-Encoding".into(), algo.to_string()));
        extra.push(("Vary".into(), "Accept-Encoding".into()));
    }

    // ⚠ ⚠ **206 不现压**：`Content-Range` 说的是**这个表示**的字节区间，
    //   而现压会把字节整个换掉 —— 两者放在同一个响应里必然对不上。
    //   ★ 预压缩那条路不受影响：旁文件本身就是那个表示，区间是它自己的。
    let mut encoder = if status == 206 { None } else { encoder };
    write_head_encoded(session, status, extra, &mut encoder).await;

    // ── 11. 发送 ──────────────────────────────────────────────────────────
    if head_only || body_len == 0 {
        let _ = session.write_response_body(Bytes::new(), true).await;
        return Ok(());
    }
    stream_body(session, full, start, body_len, &mut encoder).await;
    Ok(())
}

/// 分块把 `[start, start+count)` 发出去。
async fn stream_body(
    session: &mut Downstream<'_>,
    full: &Path,
    start: u64,
    count: u64,
    encoder: &mut Option<crate::encode::Encoder>,
) {
    let p = full.to_path_buf();
    let opened = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::File::open(&p)?;
        f.seek(SeekFrom::Start(start))?;
        Ok::<_, std::io::Error>(f)
    })
    .await;
    let mut f = match opened {
        Ok(Ok(f)) => f,
        // ⚠ 头已经发出去了 —— 这里除了断掉连接没有别的诚实做法。
        //   ★ 客户端会看到一个「Content-Length 说了 N、实际收到 0」的响应，
        //   那正是「文件在两步之间被换掉了」应该长的样子。
        other => {
            warn!("file_server：开文件失败（头已发出）：{other:?}");
            let _ = session.write_response_body(Bytes::new(), true).await;
            return;
        }
    };

    let mut left = count;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        let read = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; want];
            let mut got = 0;
            while got < want {
                match f.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) => return (f, Err(e)),
                }
            }
            buf.truncate(got);
            (f, Ok(buf))
        })
        .await;
        let (back, buf) = match read {
            Ok((back, Ok(buf))) => (back, buf),
            Ok((_, Err(e))) => {
                warn!("file_server：读文件失败（头已发出）：{e}");
                let _ = session.write_response_body(Bytes::new(), true).await;
                return;
            }
            Err(e) => {
                warn!("file_server：读文件的任务挂了：{e}");
                let _ = session.write_response_body(Bytes::new(), true).await;
                return;
            }
        };
        f = back;
        if buf.is_empty() {
            // 文件在读的中途被截短了。
            warn!("file_server：文件比 Content-Length 说的短（头已发出）");
            break;
        }
        left -= buf.len() as u64;
        let last = left == 0;
        let raw = Bytes::from(buf);
        // ★ 压缩在写之前。⚠ 压缩层会**攒**数据：一块进去可能什么都不出来，
        //   而**空块不能带 `last=false` 写下去** —— 分块编码里零长块就是体结束。
        let out = match encoder.as_mut() {
            Some(e) => e.body_filter(Some(&raw), false),
            None => None,
        };
        let chunk = out.unwrap_or(raw);
        // ⚠ 压缩时**最后一块也不能标 `last`**：收尾（gzip 的 footer）还没吐出来。
        let mark_last = last && encoder.is_none();
        if chunk.is_empty() && !mark_last {
            continue;
        }
        if let Err(e) = session.write_response_body(chunk, mark_last).await {
            debug!("file_server：写响应体失败：{e}");
            return;
        }
    }
    // ★ 压缩的收尾：漏了它，客户端拿到的是一个**少了尾巴**的压缩流，
    //   而它只会在解压到最后一刻报「意外的流结束」。
    if let Some(enc) = encoder.as_mut() {
        if let Some(tail) = enc.body_filter(None, true)
            && !tail.is_empty()
            && let Err(e) = session.write_response_body(tail, false).await
        {
            debug!("file_server：写压缩收尾失败：{e}");
            return;
        }
        let _ = session.write_response_body(Bytes::new(), true).await;
    } else if left > 0 {
        let _ = session.write_response_body(Bytes::new(), true).await;
    }
}

/// `browse`：目录列表。
async fn browse(
    session: &mut Downstream<'_>,
    dir: &Path,
    url_path: &str,
    head_only: bool,
    encoder: Option<crate::encode::Encoder>,
) -> Result<(), u16> {
    let d = dir.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || {
        let mut names: Vec<(String, bool)> = Vec::new();
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push((name, is_dir));
        }
        // 目录在前，同类按名字排。★ 有序才谈得上「两次请求给同一个答案」。
        names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Some(names)
    })
    .await;
    let Ok(Some(entries)) = entries else {
        return Err(404);
    };
    let html = render_listing(url_path, &entries);
    let bytes = Bytes::from(html);
    // ★ 目录列表是 HTML，正是最该压的那一类。
    let mut encoder = encoder;
    write_head_encoded(
        session,
        200,
        vec![
            ("Content-Type".into(), "text/html; charset=utf-8".into()),
            ("Content-Length".into(), bytes.len().to_string()),
        ],
        &mut encoder,
    )
    .await;
    if head_only {
        let _ = session.write_response_body(Bytes::new(), true).await;
        return Ok(());
    }
    match encoder.as_mut() {
        None => {
            let _ = session.write_response_body(bytes, true).await;
        }
        Some(enc) => {
            // ⚠ 一次把整个体喂进去、再拿收尾 —— 两步都可能吐东西，
            //   而**任何一步漏掉都会让解压在最后一刻失败**。
            if let Some(part) = enc.body_filter(Some(&bytes), false)
                && !part.is_empty()
            {
                let _ = session.write_response_body(part, false).await;
            }
            if let Some(tail) = enc.body_filter(None, true)
                && !tail.is_empty()
            {
                let _ = session.write_response_body(tail, false).await;
            }
            let _ = session.write_response_body(Bytes::new(), true).await;
        }
    }
    Ok(())
}

/// 目录列表的 HTML。★ 分出来是为了让它能被单测直接喂输入。
fn render_listing(url_path: &str, entries: &[(String, bool)]) -> String {
    let mut s = String::new();
    s.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<title>");
    escape_into(&mut s, url_path);
    s.push_str("</title>\n</head><body>\n<h1>");
    escape_into(&mut s, url_path);
    s.push_str("</h1>\n<ul>\n");
    if url_path != "/" {
        s.push_str("<li><a href=\"../\">../</a></li>\n");
    }
    for (name, is_dir) in entries {
        s.push_str("<li><a href=\"");
        // ⚠ href 里要的是**百分号编码**，正文里要的是 **HTML 转义** ——
        //   两种转义解决的是两件事，混用一种会各漏一半。
        url_escape_into(&mut s, name);
        if *is_dir {
            s.push('/');
        }
        s.push_str("\">");
        escape_into(&mut s, name);
        if *is_dir {
            s.push('/');
        }
        s.push_str("</a></li>\n");
    }
    s.push_str("</ul>\n</body></html>\n");
    s
}

fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// href 用的百分号编码。**只放行 URL 里无歧义的那几类字符**，其余一律编码。
fn url_escape_into(out: &mut String, s: &str) {
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
}

// ── 校验器 ────────────────────────────────────────────────────────────────

/// `"<mtime纳秒十六进制>-<size十六进制>"`。
///
/// ★ ★ 用**纳秒**而不是秒（nginx 用秒）：秒级精度下「同一秒内改了、大小没变」
/// 这个窗口里，ETag 不变 ⇒ 客户端永远拿旧的。纳秒把这个窗口关掉。
/// ⚠ 代价：文件系统若只记到秒（有的 tmpfs / 网络盘如此），纳秒位恒为 0，
/// 于是这条 ETag 退化成 nginx 那一档 —— 不会更差，但也不会更好。
fn etag_of(meta: &Metadata) -> Option<String> {
    let nanos = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("\"{nanos:x}-{:x}\"", meta.len()))
}

fn unix_secs(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn push_validators(
    extra: &mut Vec<(String, String)>,
    etag: &Option<String>,
    last: &Option<String>,
) {
    if let Some(e) = etag {
        extra.push(("ETag".into(), e.clone()));
    }
    if let Some(l) = last {
        extra.push(("Last-Modified".into(), l.clone()));
    }
}

/// `If-None-Match`：`*`、逗号列表、`W/` 前缀都要处理（RFC 9110 §13.1.2）。
///
/// ⚠ 这里用的是**弱比较**：`W/"x"` 与 `"x"` 算同一个。
/// RFC 对 `If-None-Match` 明确要求弱比较（`If-Match` 才是强比较）。
pub fn if_none_match_hit(header_value: &str, etag: &str) -> bool {
    let h = header_value.trim();
    if h == "*" {
        return true;
    }
    let want = strip_weak(etag);
    h.split(',').any(|c| strip_weak(c.trim()) == want)
}

fn strip_weak(s: &str) -> &str {
    s.strip_prefix("W/").unwrap_or(s)
}

/// `If-Range`：值要么是一个 entity-tag，要么是一个 HTTP 日期。
///
/// ⚠ ⚠ 这里必须用**强比较**（RFC 9110 §13.1.5 明说）：`W/"x"` 不匹配任何东西。
/// 弱 ETag 的含义是「语义等价但字节可能不同」，而 Range 要的正是**字节**。
/// ★ 拿弱 ETag 去放行一次 Range，客户端会把两个版本的字节拼在一起 ——
/// 而拼出来的文件既不报错也不是任何一个版本。
fn if_range_matches(value: &str, etag: &Option<String>, mtime: Option<SystemTime>) -> bool {
    let v = value.trim();
    if v.starts_with('"') {
        return matches!(etag, Some(e) if e == v);
    }
    if v.starts_with("W/") {
        return false;
    }
    match (httpdate::parse(v), mtime.and_then(unix_secs)) {
        (Some(d), Some(m)) => d == m,
        _ => false,
    }
}

fn header<'a>(req: &'a RequestHeader, name: &str) -> Option<&'a str> {
    req.headers.get(name).and_then(|v| v.to_str().ok())
}

async fn write_head(session: &mut Downstream<'_>, status: u16, extra: Vec<(String, String)>) {
    write_head_encoded(session, status, extra, &mut None).await;
}

/// 同上，但让压缩层看一眼这个头（**M2 批 I**）。
///
/// ★ ★ 压缩层在这里做三件会改头的事：补 `Vary: Accept-Encoding`、
/// 把强 ETag 弱化成 `W/"…"`（**G102**）、去掉 `Content-Length` 与 `Accept-Ranges`
/// 并改成 `chunked`。⚠ 最后那一条意味着**被现压的响应没有 Range** ——
/// 这是正确的（区间说的是未压缩的字节），但用户看得见，所以写在文档里。
async fn write_head_encoded(
    session: &mut Downstream<'_>,
    status: u16,
    extra: Vec<(String, String)>,
    encoder: &mut Option<crate::encode::Encoder>,
) {
    let mut resp = match ResponseHeader::build(status, None) {
        Ok(r) => r,
        Err(e) => {
            warn!("file_server：构不出响应头（status={status}）：{e}");
            return;
        }
    };
    for (k, v) in &extra {
        let _ = resp.insert_header(k.clone(), v);
    }
    if let Some(enc) = encoder.as_mut() {
        enc.header_filter(&mut resp, crate::encode::status_has_no_body(status));
    }
    if let Err(e) = session.write_response_header(Box::new(resp)).await {
        debug!("file_server：写响应头失败：{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── If-None-Match ────────────────────────────────────────────────────
    #[test]
    fn if_none_match_认星号_列表_与弱前缀() {
        assert!(if_none_match_hit("*", "\"abc\""));
        assert!(if_none_match_hit("\"abc\"", "\"abc\""));
        assert!(if_none_match_hit("\"x\", \"abc\", \"y\"", "\"abc\""));
        assert!(
            if_none_match_hit("W/\"abc\"", "\"abc\""),
            "弱比较：W/ 前缀要脱掉"
        );
        assert!(!if_none_match_hit("\"nope\"", "\"abc\""));
        assert!(!if_none_match_hit("", "\"abc\""));
    }

    // ★ ★ `If-Range` 用**强**比较 —— 与上面那条**有意不同**。
    //   ⚠ 少了这条判据，两处很容易被"统一"成同一个 helper，
    //   而统一之后弱 ETag 会放行 Range，客户端拼出一个两个版本混在一起的文件。
    #[test]
    fn if_range_用强比较_弱_etag_一律不匹配() {
        let e = Some("\"abc\"".to_string());
        assert!(if_range_matches("\"abc\"", &e, None));
        assert!(
            !if_range_matches("W/\"abc\"", &e, None),
            "弱 ETag 不该放行 Range"
        );
        assert!(!if_range_matches("\"other\"", &e, None));
    }

    #[test]
    fn if_range_也认日期() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(784_111_777);
        assert!(if_range_matches(
            "Sun, 06 Nov 1994 08:49:37 GMT",
            &None,
            Some(t)
        ));
        assert!(!if_range_matches(
            "Sun, 06 Nov 1994 08:49:38 GMT",
            &None,
            Some(t)
        ));
        assert!(!if_range_matches("not a date", &None, Some(t)));
    }

    // ── 目录列表 ──────────────────────────────────────────────────────────
    //
    // ★ ★ 目录列表把**文件名**放进 HTML —— 而文件名是文件系统上的任意字节。
    //   一个叫 `<script>x</script>` 的文件在没有转义的列表里就是一次存储型 XSS。
    #[test]
    fn 目录列表把文件名转义掉() {
        let html = render_listing("/", &[("<script>alert(1)</script>".into(), false)]);
        assert!(
            !html.contains("<script>alert(1)"),
            "原样的 script 标签跑出去了：{html}"
        );
        assert!(html.contains("&lt;script&gt;"), "正文该 HTML 转义");
        assert!(html.contains("%3Cscript%3E"), "href 该百分号编码");
    }

    #[test]
    fn 目录列表里引号也转义() {
        let html = render_listing("/", &[("a\"b'c&d".into(), false)]);
        assert!(html.contains("&quot;") && html.contains("&#39;") && html.contains("&amp;"));
        assert!(!html.contains("a\"b"), "裸引号会把 href 提前收尾");
    }

    #[test]
    fn 目录列表给目录加尾斜杠_且根目录不给上级链接() {
        let html = render_listing("/x/", &[("sub".into(), true), ("f.txt".into(), false)]);
        assert!(html.contains("href=\"sub/\">sub/</a>"));
        assert!(html.contains("href=\"f.txt\">f.txt</a>"));
        assert!(html.contains("href=\"../\""), "非根目录要有上级链接");

        let root = render_listing("/", &[]);
        assert!(!root.contains("href=\"../\""), "根目录不该给上级链接");
    }

    // ⚠ 中文名在 href 里要编码、在正文里原样（HTML 转义不动它）。
    #[test]
    fn 中文文件名两种转义各做各的() {
        let html = render_listing("/", &[("中文.txt".into(), false)]);
        assert!(html.contains(">中文.txt<"), "正文该是原样的中文");
        assert!(
            html.contains("href=\"%E4%B8%AD%E6%96%87.txt\""),
            "href 该百分号编码"
        );
    }
}
