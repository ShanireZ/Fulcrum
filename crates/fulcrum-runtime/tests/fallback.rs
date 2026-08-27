//! ⚠ **回落层已整层删除**（G98），本文件原来那七条判据没了对象。
//!
//! ★ ★ **其中一条被救了下来**：自环检测此前只守回落地址，而
//! `reverse_proxy 127.0.0.1:443`（指回枢衡自己在监听的端口）是**同一个事故** ——
//! 请求打回自己 → 再转发 → 再打回自己，直到 fd 或内存耗尽，
//! 而日志里只有源源不断的**正常**转发记录。
//! ⇒ 它挪到了上游那一侧（`self_loop_warnings`），本文件跟着改成钉那一侧。
//!
//! > ★ **一条判据被删掉的时候，它守着的那件事不会跟着消失。**
//!
//! ⚠ 另外留一条**归零判据**：整张归属表里不许再出现「回落」那一档。
//! 哪天有人把某条指令改回回落，它会当场红 —— 而那时要做的第一件事，
//! 是把上面那六条端到端判据按它的形状重新写出来，而不是让那一层无声地回来。

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

/// ★ ★ ★ **归零判据**：归属表里不许再有「回落」那一档。
///
/// ⚠ 它替代的是原来那七条 —— 那些判据的**对象**没了，而这一条守的是
/// 「对象别悄悄回来」。★ 一个被删掉的层若无声地复活，最先出问题的
/// 不是它自己，而是所有假设它不存在的代码（比如 `serve_one` 里那个 `match`）。
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

/// ★ ★ ★ **`fallback_nginx` / `fallback_caddy` 现在编译不过 —— 而且要说清它去哪了。**
///
/// ⚠ ⚠ 这一条是**删除一个公开配置面**时最容易漏的那一步：
/// 只把选项从表里拿掉的话，写过它的人拿到的是「未知的全局选项，你是不是想写 XXX」——
/// 一句听起来像「你打错字了」的话，而那写法曾经是对的。
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
        // ⚠ 反向：它**不该**退化成「未知的全局选项 + 你是不是想写…」。
        assert!(
            !text.contains("你是不是想写"),
            "`{opt}` 被当成打错字了：\n{text}"
        );
    }
}

/// ★ ★ ★ **自环检测挪到 `reverse_proxy` 上之后，仍然报得出来。**
///
/// ⚠ 与回落那侧**有意不同**：这里是 **warning 不是 error** ——
/// 一条指回自己的 `reverse_proxy` 可能是有意的（自己终止 TLS 再回自己的明文口）。
/// ★ 而回落那侧能判 error，是因为回落是**编译器自己加的**，用户没得选。
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

    // ★ ★ 反向那一半：**指向别处的上游不许被说**。
    //   ⚠ 少了它，一个恒报警的实现会让上面那条全绿，
    //   而每一份正常配置都会在装载日志里挨一句莫名其妙的警告 ——
    //   ★ 而噪音会把真警告一起埋掉（本仓库为「假警告」删过一整行日志）。
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

/// ★ ★ ★ **自环判据的自证：它必须两个方向都报得出来。**
///
/// 一个恒返回 `Some` 的实现会把所有合法配置都拒掉；一个恒返回 `None` 的实现
/// 等于没有这道检查。⚠ 后者是更可能发生的那一种，也更难发现。
///
/// ★ 回落层删掉之后它**原样保留**：函数没变，它守的东西也没变，变的只是谁在调用它。
#[test]
fn 自环判据自证能命中也能错过() {
    let listening = [(80u16, false), (443u16, true)];

    // 命中：本机形式 + 端口在监听集里。四种本机写法都要认得。
    for host in ["127.0.0.1", "localhost", "0.0.0.0", "LocalHost"] {
        assert_eq!(
            self_loop_port(&format!("{host}:443"), &listening),
            Some(443),
            "{host}:443 应当被判为自环"
        );
    }
    // 错过之一：端口不在监听集里（上游在本机的另一个端口上，这是最常见的正常配置）。
    assert_eq!(self_loop_port("127.0.0.1:8081", &listening), None);
    // 错过之二：端口相同但**在另一台机器上** —— 两个进程不可能绑同一地址的同一端口，
    // 所以这不是自环，判它自环就是误报，而误报会让一份合法配置起不到。
    assert_eq!(self_loop_port("nginx.internal:443", &listening), None);
    assert_eq!(self_loop_port("10.0.0.5:80", &listening), None);
    // 监听集为空时永远不该命中。
    assert_eq!(self_loop_port("127.0.0.1:443", &[]), None);
}
