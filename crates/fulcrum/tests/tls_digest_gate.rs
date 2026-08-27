//! 三道门，守住 **fork 改动 14**（`SslDigest` 多两格 `sni` / `alpn`）与它换来的那条结论。
//!
//! # 背景：为什么访问日志的 `tls_sni` / `tls_alpn` 走 fork 而不是走回调
//!
//! 上游把这两格留给了 `TlsAccept::handshake_complete_callback` + `SslDigest.extension`。
//! ⚠ ⚠ 走那条路要求监听器**带回调**（`TlsSettings::with_callbacks(…)`），而带回调时
//! 上游走的是 `handshake_with_callback()` —— 它的第一行 `start_accept()` **无条件**装一个
//! 恒回 `-1` 的 `cert_cb`（`SSL_set_cert_cb(raw_cert_block)`）⇒ **每条 TLS 连接都要多走一趟
//! 「挂起 → `certificate_callback` → `resume_accept`」**（§10 实测，D28）。
//!
//! ⇒ （D27 + D28 一起结案）改成 **fork 改动 14**：`SslDigest` 直接多两格，
//! 由 `from_ssl()` 在握手结束后填 —— 那里本来就握着 `&SslRef`，**一分额外开销都没有**。
//! ★ 顺带 h3 也能自己造一份同类型的 `SslDigest`（`quic_digest`），
//! 于是 h1/h2 与 h3 在访问日志那一层**走同一段代码**。
//!
//! # 三道门各答一个**不同**的问题（★ 不许混着说）
//!
//! | 门 | 它答的问题 | 它答不了的 |
//! |---|---|---|
//! | 1 | 上游 `SslDigest` 上**还有没有**那两格 | 它们有没有被填 |
//! | 2 | 上游 `from_ssl()` **有没有在填**它们 | 运行时真的填对了没有 |
//! | 3 | 数据面有没有**又**去挂那份回调（把 D28 的开销请回来）| 同上 |
//!
//! ⚠ 三道都是**文本判据**。「运行时真的填对了」由**第二十三个场景**
//! [`tests/log/run.sh`] 在真握手上量（h1/h2 四格、h3 三格）。
//!
//! ★ ★ **门 1 看起来多余（我们的代码读 `ssl.sni`，字段没了编译器就会红）——
//! 而它留着是有理由的**：编译器给的是「`SslDigest` 没有 `sni` 字段」，
//! 这道门给的是「**fork 改动 14 被 rebase 冲掉了**」。⚠ 前者会让人去改自己的代码。

/// 仓库根。★ `CARGO_MANIFEST_DIR` 是 `crates/fulcrum`。
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("找不到仓库根")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}：{e}", p.display()))
}

const DIGEST_RS: &str = "vendor/pingora/pingora-core/src/protocols/tls/digest.rs";
const STREAM_RS: &str = "vendor/pingora/pingora-core/src/protocols/tls/boringssl_openssl/stream.rs";

/// 一句「fork 改动 14 没了」的话，三道门共用。
fn fork14(what: &str) -> String {
    format!(
        "\n{what}\n\
         ⇒ **fork 改动 14 多半被一次 rebase 冲掉了**（见 vendor/pingora/FORK.md）。\n\
         ⚠ ⚠ 它的失效形态**不是编译错误**：访问日志里 tls_sni / tls_alpn 会静静地变成\n\
         「这条连接没发 SNI / 没协商 ALPN」，而那两句话读起来完全成立。"
    )
}

// ── 门 1：那两格还在 ────────────────────────────────────────────────────────

#[test]
fn 上游的_ssldigest_上还有_sni_与_alpn_两格() {
    let src = read(DIGEST_RS);
    let start = src
        .find("pub struct SslDigest {")
        .expect("上游连 SslDigest 都没有了 —— 先去看它变成了什么");
    let end = start
        + src[start..]
            .find("\n}\n")
            .expect("SslDigest 的结构体没有顶层收尾的花括号，切法失效了");
    let body = &src[start..end];
    for f in ["pub sni: Option<String>", "pub alpn: Option<String>"] {
        assert!(
            body.contains(f),
            "{}",
            fork14(&format!("上游 `SslDigest` 上找不到 `{f}`。"))
        );
    }
}

// ── 门 2：`from_ssl()` 真的在填 ─────────────────────────────────────────────

#[test]
fn 上游的_from_ssl_真的在填那两格() {
    let src = read(STREAM_RS);
    let start = src
        .find("    pub fn from_ssl(ssl: &SslRef) -> Self {")
        .expect("上游 `SslDigest::from_ssl` 不见了 —— 先去看它变成了什么");
    // ★ 切到缩进四格的收尾花括号为止：函数内部的闭合花括号都比它深。
    let body_end = src[start..]
        .find("\n    }\n")
        .expect("`from_ssl` 的函数体没有缩进四格的收尾花括号，切法失效了");
    let body = &src[start..start + body_end];
    for f in ["digest.sni = ", "digest.alpn = "] {
        assert!(
            body.contains(f),
            "{}",
            fork14(&format!("`from_ssl()` 里找不到 `{f}`。"))
        );
    }
    // ⚠ 光看「有赋值」不够 —— 一次「赋成 None」的错误 rebase 会通过上面那条。
    //   ★ 所以再钉住它**取自哪两个 SslRef 方法**。
    for f in ["servername(", "selected_alpn_protocol("] {
        assert!(
            body.contains(f),
            "{}",
            fork14(&format!("`from_ssl()` 里那两格不再取自 `{f}`。"))
        );
    }
}

#[test]
fn 门_2_自证它切出来的是那个函数() {
    // ⚠ 切法坏掉时最像样的错答案是「切出来一大段、里面碰巧有那几个串」。
    let src = read(STREAM_RS);
    let start = src
        .find("    pub fn from_ssl(ssl: &SslRef) -> Self {")
        .expect("找不到它");
    let body_end = src[start..].find("\n    }\n").expect("切不出函数体");
    let body = &src[start..start + body_end];
    // ⚠ 上界取 2400 而不是「函数原本多长」：`from_ssl` 里那段解释 fork 改动 14 的
    //   注释就占了一半多。★ 它要挡的是「切出了半个文件」，不是「注释写长了」——
    //   一个跟着注释长度走的阈值会在下一次补一句话时红，而那次红什么都不说明。
    assert!(
        body.len() < 2400,
        "切出来 {} 字节，对 `from_ssl` 来说太长了 —— 切法多半坏了",
        body.len()
    );
    assert!(
        body.contains("SslDigest::new("),
        "切出来的这段不像 `from_ssl` 的函数体：\n{body}"
    );
    // ★ 邻居函数不许被包进来（否则上面几条可能是在看别人）。
    assert!(
        !body.contains("impl<T> SslStream<T>"),
        "切过头了，把邻居也包进来了"
    );
}

// ── 门 3：别把那趟握手开销请回来 ────────────────────────────────────────────

/// 把一段源码里的**整行注释**去掉。
///
/// # ★ ★ ★ 这道门的前身第一次跑就红了，而红它的是**解释它自己的那段注释**
///
/// `lib.rs` 里那段说明「为什么这里不用 `with_callbacks`」的注释，字面就带着那个串。
/// ⚠ 一个文本判据**看得见注释**，于是它给了一个读起来完全成立的错答案。
///
/// > ★ ★ **先问这把尺子在量什么，再问它量出来对不对。**
///
/// ⚠ 只去**整行**注释：本仓库不用块注释，而**行尾**注释那一行上必然也有代码，
/// 那时红是对的。⇒ 尺子有意留在「宁可多报，不可漏报」这一侧。
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 数据面里所有 `.rs`（`vendor/` 不算 —— 那是上游的代码），**注释已去掉**。
fn data_plane_sources() -> Vec<(String, String)> {
    let root = repo_root().join("crates/fulcrum-server/src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).expect("读不到数据面目录") {
            let p = e.expect("目录项").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                let rel = p
                    .strip_prefix(&root)
                    .expect("前缀")
                    .to_string_lossy()
                    .replace('\\', "/");
                let src = std::fs::read_to_string(&p).expect("读源码");
                out.push((rel, strip_line_comments(&src)));
            }
        }
    }
    // ★ 下界：一个文件都没扫到时，下面那道门会拿空集比，看起来永远是绿的。
    assert!(
        out.len() >= 10,
        "只扫到 {} 份数据面源码，扫描多半坏了；本次检查不能采信",
        out.len()
    );
    out
}

#[test]
fn 数据面不许再去挂_tlsaccept_回调() {
    let bad: Vec<String> = data_plane_sources()
        .into_iter()
        .filter(|(_, src)| src.contains("with_callbacks(") || src.contains("TlsAccept"))
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        bad.is_empty(),
        "\n这些文件又去挂 `TlsAccept` 回调了：{bad:?}\n\
         ⚠ ⚠ 带回调时上游走 `handshake_with_callback()`，**每条 TLS 连接多一趟\n\
         「挂起 → certificate_callback → resume_accept」**（§10 实测，D28）。\n\
         ★ 而 SNI / ALPN 已经由 fork 改动 14 直接记进 `SslDigest` 了 —— 不需要回调。\n\
         ⇒ 真要加别的握手期能力，先读 §10 第 78/79 轮，再决定这趟开销值不值。"
    );
}

#[test]
fn 尺子自证_它看得见代码_看不见整行注释() {
    // ★ ★ ★ 这一条是前身那道门第一次跑就红了之后加的：红它的是解释它自己的那段注释。
    //   ⇒ 尺子必须先拿已知样本自证，而不是只在真文件上跑过。
    let s = strip_line_comments(
        "let a = 1;\n\
         // 这行注释里写着 with_callbacks(cb)\n\
         \x20   //   缩进的整行注释也算：TlsAccept\n\
         let b = TlsSettings::from(builder);\n",
    );
    assert!(!s.contains("with_callbacks("), "整行注释没被去掉：\n{s}");
    assert!(!s.contains("TlsAccept"), "缩进的整行注释没被去掉：\n{s}");
    assert!(s.contains("TlsSettings::from("), "代码被误删了：\n{s}");
    // ⚠ 而**行尾**注释有意不处理 —— 那一行上必然也有代码，红是对的。
    let tail = strip_line_comments("let x = with_callbacks(cb); // 说明\n");
    assert!(
        tail.contains("with_callbacks("),
        "行尾注释那一行连代码一起被吞了 —— 尺子太宽，会漏报"
    );
    // 空输入不能被当成「查过了、没问题」。
    assert!(strip_line_comments("").is_empty());
}
