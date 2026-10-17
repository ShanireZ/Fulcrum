//! ⚠ **回落層已整層刪除**（G98），本檔案原來那七條判據沒了物件。
//!
//! ★ ★ **其中一條被救了下來**：自環檢測此前只守回完成實作址，而
//! `reverse_proxy 127.0.0.1:443`（指回樞衡自己在監聽的連接埠）是**同一個事故** ——
//! 請求打回自己 → 再轉發 → 再打回自己，直到 fd 或記憶體耗盡，
//! 而紀錄裡只有源源不斷的**正常**轉發記錄。
//! ⇒ 它挪到了上游那一側（`self_loop_warnings`），本檔案跟著改成釘那一側。
//!
//! > ★ **一條判據被刪掉的時候，它守著的那件事不會跟著消失。**
//!
//! ⚠ 另外留一條**歸零判據**：整張歸屬表裡不許再出現「回落」那一檔。
//! 哪天有人把某條指令改回回落，它會當場紅 —— 而那時要做的第一件事，
//! 是把上面那六條端到端判據按它的形狀重新寫出來，而不是讓那一層無聲地回來。

use fulcrum_config::compile_str;
use fulcrum_runtime::{Runtime, self_loop_port};

fn build(src: &str) -> Result<Runtime, String> {
    let out = compile_str("t.Fulcrumfile", src);
    let cfg = out.config.expect("编译应当通过");
    Runtime::build(&cfg).map_err(|errs| {
        errs.iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// ★ ★ ★ **歸零判據**：歸屬表裡不許再有「回落」那一檔。
///
/// ⚠ 它替代的是原來那七條 —— 那些判據的**物件**沒了，而這一條守的是
/// 「物件別悄悄回來」。★ 一個被刪掉的層若無聲地復活，最先出問題的
/// 不是它自己，而是所有假設它不存在的程式碼（比如 `serve_one` 裡那個 `match`）。
#[test]
fn 回落层已经归零而且不许悄悄回来() {
    use fulcrum_config::directive::{ChainDirective, SiteDirective};
    for d in ChainDirective::ALL {
        let label = d.owner().doc_label();
        assert!(
            !label.contains("回落"),
            "`{}` 的归属是「{label}」—— 回落层在M2 批 G 删除。\
             要让它回来的话，先把 tests/fallback.rs 顶部那六条端到端判据重新写出来。",
            d.name()
        );
    }
    for d in SiteDirective::ALL {
        assert!(!d.owner().doc_label().contains("回落"), "{}", d.name());
    }
}

/// ★ ★ ★ **`fallback_nginx` / `fallback_caddy` 現在編譯不過 —— 而且要說清它去哪了。**
///
/// ⚠ ⚠ 這一條是**刪除一個公開設定面**時最容易漏的那一步：
/// 只把選項從表裡拿掉的話，寫過它的人拿到的是「未知的全域選項，你是不是想寫 XXX」——
/// 一句聽起來像「你打錯字了」的話，而那寫法曾經是對的。
#[test]
fn 写了老的_fallback_选项要说清它去哪了() {
    for opt in ["fallback_nginx", "fallback_caddy"] {
        let out = compile_str(
            "t.Fulcrumfile",
            &format!("{{\n  {opt} 127.0.0.1:8081\n}}\n:9999 {{\n  respond 200 ok\n}}\n"),
        );
        assert!(out.diagnostics.has_errors(), "`{opt}` 现在应当编译不过");
        let text = out.render_diagnostics();
        assert!(
            text.contains("已经删掉了"),
            "`{opt}` 的报错没说它被删掉了：\n{text}"
        );
        assert!(
            text.contains("批 G") || text.contains("回落层"),
            "`{opt}` 的报错没说这一层去哪了：\n{text}"
        );
        // ⚠ 反向：它**不該**退化成「未知的全域選項 + 你是不是想寫…」。
        assert!(
            !text.contains("你是不是想写"),
            "`{opt}` 被当成打错字了：\n{text}"
        );
    }
}

/// ★ ★ ★ **自環檢測挪到 `reverse_proxy` 上之後，仍然報得出來。**
///
/// ⚠ 與回落那側**有意不同**：這裡是 **warning 不是 error** ——
/// 一條指回自己的 `reverse_proxy` 可能是有意的（自己終止 TLS 再回自己的明文口）。
/// ★ 而回落那側能判 error，是因為回落是**編譯器自己加的**，使用者沒得選。
#[test]
fn reverse_proxy_指回自己会被说出来() {
    let rt =
        build(":9999 {\n  reverse_proxy 127.0.0.1:9999\n}\n").expect("这是 warning 不是 error");
    assert_eq!(
        rt.self_loop_warnings.len(),
        1,
        "指回自己却一句话都没说：{:?}",
        rt.self_loop_warnings
    );
    let w = &rt.self_loop_warnings[0];
    assert!(w.contains("9999"), "没说是哪个端口：{w}");
    assert!(w.contains("转发回自己"), "没说是自环：{w}");

    // ★ ★ 反向那一半：**指向別處的上游不許被說**。
    //   ⚠ 少了它，一個恆報警的實作會讓上面那條全綠，
    //   而每一份正常設定都會在裝載紀錄裡挨一句莫名其妙的警告 ——
    //   ★ 而噪音會把真警告一起埋掉（本儲存庫為「假警告」刪過一整行紀錄）。
    let ok = build(":9999 {\n  reverse_proxy 127.0.0.1:8081\n}\n").unwrap();
    assert!(
        ok.self_loop_warnings.is_empty(),
        "指向别的端口不该报警：{:?}",
        ok.self_loop_warnings
    );
    let ok2 = build(":9999 {\n  reverse_proxy other.internal:9999\n}\n").unwrap();
    assert!(
        ok2.self_loop_warnings.is_empty(),
        "同端口但在别的机器上不该报警：{:?}",
        ok2.self_loop_warnings
    );
}

/// ★ ★ ★ **自環判據的自證：它必須兩個方向都報得出來。**
///
/// 一個恆回傳 `Some` 的實作會把所有合法設定都拒掉；一個恆回傳 `None` 的實作
/// 等於沒有這道檢查。⚠ 後者是更可能發生的那一種，也更難發現。
///
/// ★ 回落層刪掉之後它**原樣保留**：函式沒變，它守的東西也沒變，變的只是誰在呼叫它。
#[test]
fn 自环判据自证能命中也能错过() {
    let listening = [(80u16, false), (443u16, true)];

    // 命中：本機形式 + 連接埠在監聽集裡。四種本機寫法都要認得。
    for host in ["127.0.0.1", "localhost", "0.0.0.0", "LocalHost"] {
        assert_eq!(
            self_loop_port(&format!("{host}:443"), &listening),
            Some(443),
            "{host}:443 应当被判为自环"
        );
    }
    // 錯過之一：連接埠不在監聽集裡（上游在本機的另一個連接埠上，這是最常見的正常設定）。
    assert_eq!(self_loop_port("127.0.0.1:8081", &listening), None);
    // 錯過之二：連接埠相同但**在另一臺機器上** —— 兩個處理程序不可能綁同一位址的同一連接埠，
    // 所以這不是自環，判它自環就是誤報，而誤報會讓一份合法設定起不到。
    assert_eq!(self_loop_port("nginx.internal:443", &listening), None);
    assert_eq!(self_loop_port("10.0.0.5:80", &listening), None);
    // 監聽集為空時永遠不該命中。
    assert_eq!(self_loop_port("127.0.0.1:443", &[]), None);
}
