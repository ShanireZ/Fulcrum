//! 三道門，守住 **fork 改動 14**（`SslDigest` 多兩格 `sni` / `alpn`）與它換來的那條結論。
//!
//! # 背景：為什麼訪問紀錄的 `tls_sni` / `tls_alpn` 走 fork 而不是走回呼
//!
//! 上游把這兩格留給了 `TlsAccept::handshake_complete_callback` + `SslDigest.extension`。
//! ⚠ ⚠ 走那條路要求監聽器**帶回呼**（`TlsSettings::with_callbacks(…)`），而帶回呼時
//! 上游走的是 `handshake_with_callback()` —— 它的第一行 `start_accept()` **無條件**裝一個
//! 恆回 `-1` 的 `cert_cb`（`SSL_set_cert_cb(raw_cert_block)`）⇒ **每條 TLS 連接都要多走一趟
//! 「掛起 → `certificate_callback` → `resume_accept`」**（§10 實測，D28）。
//!
//! ⇒ （D27 + D28 一起結案）改成 **fork 改動 14**：`SslDigest` 直接多兩格，
//! 由 `from_ssl()` 在握手結束後填 —— 那裡本來就握著 `&SslRef`，**一分額外開銷都沒有**。
//! ★ 順帶 h3 也能自己造一份同類型的 `SslDigest`（`quic_digest`），
//! 於是 h1/h2 與 h3 在訪問紀錄那一層**走同一段程式碼**。
//!
//! # 三道門各答一個**不同**的問題（★ 不許混著說）
//!
//! | 門 | 它答的問題 | 它答不了的 |
//! |---|---|---|
//! | 1 | 上游 `SslDigest` 上**還有沒有**那兩格 | 它們有沒有被填 |
//! | 2 | 上游 `from_ssl()` **有沒有在填**它們 | 執行環境真的填對了沒有 |
//! | 3 | 資料面有沒有**又**去掛那份回呼（把 D28 的開銷請回來）| 同上 |
//!
//! ⚠ 三道都是**文字判據**。「執行環境真的填對了」由**第二十三個場景**
//! [`tests/log/run.sh`] 在真握手上量（h1/h2 四格、h3 三格）。
//!
//! ★ ★ **門 1 看起來多餘（我們的程式碼讀 `ssl.sni`，欄位沒了編譯器就會紅）——
//! 而它留著是有理由的**：編譯器給的是「`SslDigest` 沒有 `sni` 欄位」，
//! 這道門給的是「**fork 改動 14 被 rebase 沖掉了**」。⚠ 前者會讓人去改自己的程式碼。

/// 儲存庫根。★ `CARGO_MANIFEST_DIR` 是 `crates/fulcrum`。
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

/// 一句「fork 改動 14 沒了」的話，三道門共用。
fn fork14(what: &str) -> String {
    format!(
        "\n{what}\n\
         ⇒ **fork 改动 14 多半被一次 rebase 冲掉了**（见 vendor/pingora/FORK.md）。\n\
         ⚠ ⚠ 它的失效形态**不是编译错误**：访问日志里 tls_sni / tls_alpn 会静静地变成\n\
         「这条连接没发 SNI / 没协商 ALPN」，而那两句话读起来完全成立。"
    )
}

// ── 門 1：那兩格還在 ────────────────────────────────────────────────────────

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

// ── 門 2：`from_ssl()` 真的在填 ─────────────────────────────────────────────

#[test]
fn 上游的_from_ssl_真的在填那两格() {
    let src = read(STREAM_RS);
    let start = src
        .find("    pub fn from_ssl(ssl: &SslRef) -> Self {")
        .expect("上游 `SslDigest::from_ssl` 不见了 —— 先去看它变成了什么");
    // ★ 切到縮進四格的收尾花括號為止：函式內部的閉合花括號都比它深。
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
    // ⚠ 光看「有賦值」不夠 —— 一次「賦成 None」的錯誤 rebase 會通過上面那條。
    //   ★ 所以再釘住它**取自哪兩個 SslRef 方法**。
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
    // ⚠ 切法壞掉時最像樣的錯答案是「切出來一大段、裡面碰巧有那幾個串」。
    let src = read(STREAM_RS);
    let start = src
        .find("    pub fn from_ssl(ssl: &SslRef) -> Self {")
        .expect("找不到它");
    let body_end = src[start..].find("\n    }\n").expect("切不出函数体");
    let body = &src[start..start + body_end];
    // ⚠ 上界取 2400 而不是「函式原本多長」：`from_ssl` 裡那段解釋 fork 改動 14 的
    //   註解就佔了一半多。★ 它要擋的是「切出了半個檔案」，不是「註解寫長了」——
    //   一個跟著註解長度走的閾值會在下一次補一句話時紅，而那次紅什麼都不說明。
    assert!(
        body.len() < 2400,
        "切出来 {} 字节，对 `from_ssl` 来说太长了 —— 切法多半坏了",
        body.len()
    );
    assert!(
        body.contains("SslDigest::new("),
        "切出来的这段不像 `from_ssl` 的函数体：\n{body}"
    );
    // ★ 鄰居函式不許被包進來（否則上面幾條可能是在看別人）。
    assert!(
        !body.contains("impl<T> SslStream<T>"),
        "切过头了，把邻居也包进来了"
    );
}

// ── 門 3：別把那趟握手開銷請回來 ────────────────────────────────────────────

/// 把一段原始碼裡的**整行註解**去掉。
///
/// # ★ ★ ★ 這道門的前身第一次跑就紅了，而紅它的是**解釋它自己的那段註解**
///
/// `lib.rs` 裡那段說明「為什麼這裡不用 `with_callbacks`」的註解，字面就帶著那個串。
/// ⚠ 一個文字判據**看得見註解**，於是它給了一個讀起來完全成立的錯答案。
///
/// > ★ ★ **先問這把尺子在量什麼，再問它量出來對不對。**
///
/// ⚠ 只去**整行**註解：本儲存庫不用塊註解，而**行尾**註解那一行上必然也有程式碼，
/// 那時紅是對的。⇒ 尺子有意留在「寧可多報，不可漏報」這一側。
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 資料面裡所有 `.rs`（`vendor/` 不算 —— 那是上游的程式碼），**註解已去掉**。
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
    // ★ 下界：一個檔案都沒掃到時，下面那道門會拿空集比，看起來永遠是綠的。
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
    // ★ ★ ★ 這一條是前身那道門第一次跑就紅了之後加的：紅它的是解釋它自己的那段註解。
    //   ⇒ 尺子必須先拿已知樣本自證，而不是只在真檔案上跑過。
    let s = strip_line_comments(
        "let a = 1;\n\
         // 这行注释里写着 with_callbacks(cb)\n\
         \x20   //   缩进的整行注释也算：TlsAccept\n\
         let b = TlsSettings::from(builder);\n",
    );
    assert!(!s.contains("with_callbacks("), "整行注释没被去掉：\n{s}");
    assert!(!s.contains("TlsAccept"), "缩进的整行注释没被去掉：\n{s}");
    assert!(s.contains("TlsSettings::from("), "代码被误删了：\n{s}");
    // ⚠ 而**行尾**註解有意不處理 —— 那一行上必然也有程式碼，紅是對的。
    let tail = strip_line_comments("let x = with_callbacks(cb); // 说明\n");
    assert!(
        tail.contains("with_callbacks("),
        "行尾注释那一行连代码一起被吞了 —— 尺子太宽，会漏报"
    );
    // 空輸入不能被當成「查過了、沒問題」。
    assert!(strip_line_comments("").is_empty());
}
