//! 一道門，守住 **G168** 的那條結構：兩個全域 TLS 設定在 h1/h2 與 h3 兩個入口上走**同一個**
//! `TlsPolicy::apply_to`，且都套在**掛了證書回呼的那個 builder** 上。
//!
//! ★ 與 G104「兩個入口掛同一個 `install_into`」同一個形狀：失效形態**不是編譯錯誤** ——
//!   哪天有人在其中一處漏了那一行（或另寫一份套用程式碼），h1/h2 釘了組而 h3 沒釘，
//!   兩個入口各自都「能用」，只有拿 `--curves` 去打 h3 的人才看得出來。
//!
//! ⚠ 這是**文字判據**：它答「兩處都叫了」，答不了「叫了之後握手真的照做」——
//!   後者由 `fulcrum-tls` 的握手單測與 `tests/tls-policy/run.sh`（openssl / curl `--http3-only`）量。

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

/// 去掉全部空白之後數 `needle` 出現幾次 —— ⇒ rustfmt 把呼叫拆成幾行不影響判據。
fn count(src: &str, needle: &str) -> usize {
    let flat: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    let needle: String = needle.chars().filter(|c| !c.is_whitespace()).collect();
    flat.matches(&needle).count()
}

const LIB_RS: &str = "crates/fulcrum-server/src/lib.rs";
const QUIC_RS: &str = "crates/fulcrum-server/src/quic/listener.rs";

/// （入口, 檔案, 掛證書回呼那一行, 套政策那一行）—— 兩行必須落在**同一個** builder 上。
const ENTRIES: [(&str, &str, &str, &str); 2] = [
    (
        "h1/h2",
        LIB_RS,
        "plan.resolver.install_into(&mut settings);",
        "tls_policy.apply_to(&mut settings)",
    ),
    (
        "h3",
        QUIC_RS,
        "resolver.install_into(&mut builder);",
        "policy.apply_to(&mut builder)",
    ),
];

#[test]
fn 两个入口都在挂证书回调的那个_builder_上套了_tls_政策() {
    let mut problems = Vec::new();
    for (entry, file, install, apply) in ENTRIES {
        let src = read(file);
        let n_install = count(&src, install);
        let n_apply = count(&src, apply);
        if n_install != 1 || n_apply != 1 {
            problems.push(format!(
                "{entry}（{file}）：`{install}` 出现 {n_install} 次、`{apply}` 出现 {n_apply} 次（都该恰好 1 次）"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "\n{}\n⇒ 两个入口的 TLS 政策分家了（G168）：一处漏了套用，或另写了一份。",
        problems.join("\n")
    );
}

/// ★ 這道門自己判得動：同一份 `lib.rs` / `quic/listener.rs` 各拿掉那一行 ⇒ 各數到 0。少了它，`count` 寫錯成恆回 1 也是綠的。
#[test]
fn 这道门判得动_拿掉那一行就数到零() {
    let src = read(LIB_RS);
    let (_, _, _, apply) = ENTRIES[0];
    let doctored = src.replacen(
        "tls_policy.apply_to(&mut settings)",
        "tls_policy.describe()",
        1,
    );
    assert_eq!(count(&src, apply), 1);
    assert_eq!(count(&doctored, apply), 0, "拿掉之后还数得到 —— 判据失效");
    // h3 那一處同樣判得動（2026-10-23 措辭過目後補：原先反證只做了 h1/h2）。
    let quic = read(QUIC_RS);
    let (_, _, _, apply_h3) = ENTRIES[1];
    let doctored_h3 = quic.replacen(".apply_to(&mut builder)", ".describe()", 1);
    assert_eq!(count(&quic, apply_h3), 1);
    assert_eq!(
        count(&doctored_h3, apply_h3),
        0,
        "h3：拿掉之后还数得到 —— 判据失效"
    );
    // 拆成幾行也數得到（rustfmt 的形狀）。
    assert_eq!(
        count(
            "policy\n        .apply_to(&mut builder)",
            "policy.apply_to(&mut builder)"
        ),
        1
    );
}
