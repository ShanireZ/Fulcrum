//! 被動熔斷（`passive_fail`，**G136**）的狀態機與篩子第四條。
//!
//! ★ ★ **這裡全部判據都不 sleep 到某個閾值上**：本機常年 20+ 工作階段並行，實測比空載慢
//! 3.75 倍 ⇒ 按空載餘量設的超時會**週期性假紅**，而一道假紅過幾次的門會被人忽略。
//! ⇒ 時間那一維靠**把策略裡的時長設成 0**（立刻到期）或**設成 60s**（一定沒到期）來取，
//! 唯一用到 `sleep` 的那一條斷言是**單邊**的（「過了 0 毫秒」），任何真實的睡眠都滿足它。
//!
//! ⚠ `PassivePolicy` 是**每次呼叫傳進去的參數**而不是存在狀態裡，
//! 所以一條判據可以用兩個不同的策略把狀態推到想要的那一格 —— 下面並行那條就靠這個。

use fulcrum_config::compile_str;
use fulcrum_runtime::{PassivePolicy, ProxyTarget, Runtime};

fn rt(dsl: &str) -> Runtime {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(
        !o.diagnostics.has_errors(),
        "DSL 编译不过：\n{}",
        o.render_diagnostics()
    );
    Runtime::build(&o.config.unwrap()).expect("运行时图应当能建起来")
}

/// 取第一條 `reverse_proxy` 的目標。
fn target(r: &Runtime) -> &ProxyTarget {
    r.keyed_proxies()[0].target
}

/// 立刻到期的冷卻 ⇒ 熔斷之後馬上就是「半開可搶」那一格。
const EXPIRED: PassivePolicy = PassivePolicy {
    threshold: 2,
    window_ms: 60_000,
    cooldown_ms: 0,
};

/// 一定沒到期的冷卻。
const LONG: PassivePolicy = PassivePolicy {
    threshold: 2,
    window_ms: 60_000,
    cooldown_ms: 60_000,
};

// ── 關閉那一側 ────────────────────────────────────────────────────────────

/// ⚠ ⚠ **不寫 `passive_fail` ⇒ 策略是 `None`，篩子第四條恆放行。**
///
/// ★ 這是「既有設定行為一個字不變」在執行環境這一層的判據。G136 選「預設關閉」
/// 而不是 nginx 那種「預設開」，全部理由就在這一條上。
#[test]
fn 没配_passive_fail_时策略是_none_且怎么失败都不熔断() {
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n");
    let t = target(&r);
    assert!(t.passive.is_none(), "没写 passive_fail 却建出了策略");
    let u = &t.upstreams[0];
    for _ in 0..100 {
        assert!(
            !u.record_passive_failure(t.passive.as_ref()),
            "没配策略时落账必须是空操作"
        );
    }
    assert!(!u.passive_open(), "没配策略却被熔断了");
    assert!(
        u.passive_ok(t.passive.as_ref()),
        "没配策略时筛子第四条必须恒放行"
    );
    assert_eq!(t.pick_index_by(None), Some(0), "它必须照样被选中");
}

/// 配了就該建出策略，且三個值逐個落對 —— ⚠ 三個數**互不相同**，
/// 否則「兩個旋鈕接反了」在這裡看不出來。
#[test]
fn 配了_passive_fail_就建出策略且三个值各就各位() {
    let r = rt(
        "a.com {\n  reverse_proxy 10.0.0.1:1 {\n    passive_fail 7\n    passive_window 11s\n    passive_cooldown 23s\n  }\n}\n",
    );
    let p = target(&r).passive.expect("配了就该有策略");
    assert_eq!(p.threshold, 7);
    assert_eq!(p.window_ms, 11_000);
    assert_eq!(p.cooldown_ms, 23_000);
}

// ── 熔斷那一側 ────────────────────────────────────────────────────────────

fn one_upstream() -> Runtime {
    rt("a.com {\n  reverse_proxy 10.0.0.1:1 {\n    passive_fail 2\n  }\n}\n")
}

/// 視窗內失敗數到閾值 ⇒ 熔斷，且**篩子當場擋住它**。
///
/// ⚠ 判據分兩句：`passive_open()` 說的是狀態，`pick_index_by` 說的是**排程真的認這件事**。
/// ★ 少了後一句，一個「狀態記對了、篩子卻沒接上」的實作照樣全綠 ——
/// 而那正是這個特性唯一要做的事。
#[test]
fn 失败数到阈值就熔断并且被筛子挡住() {
    let r = one_upstream();
    let t = target(&r);
    let u = &t.upstreams[0];

    assert!(
        !u.record_passive_failure(Some(&LONG)),
        "第 1 次失败不该熔断（阈值 2）"
    );
    assert!(u.passive_ok(Some(&LONG)), "还没到阈值就被挡住了");
    assert_eq!(t.pick_index_by(None), Some(0));

    assert!(u.record_passive_failure(Some(&LONG)), "第 2 次失败该熔断");
    assert!(u.passive_open(), "熔断了却没记上");
    assert!(!u.passive_ok(Some(&LONG)), "熔断中却放行了");
    assert_eq!(
        t.pick_index_by(None),
        None,
        "唯一的上游被熔断 ⇒ 一个可用的都没有 ⇒ 调用方回 502"
    );
}

/// ⚠ **視窗過期之後計數重來** —— 稀疏的偶發失敗不該攢夠一次熔斷。
///
/// ★ 判據是**單邊**的：`window_ms = 0` 配上一次真實的 `sleep`，
/// 「已經過了 0 毫秒」這件事任何真實睡眠都滿足 ⇒ ⛔ 不依賴任何餘量。
#[test]
fn 窗口过期之后计数重来() {
    let r = one_upstream();
    let u = &target(&r).upstreams[0];
    let zero_window = PassivePolicy {
        threshold: 2,
        window_ms: 0,
        cooldown_ms: 60_000,
    };
    assert!(!u.record_passive_failure(Some(&zero_window)));
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(
        !u.record_passive_failure(Some(&zero_window)),
        "窗口已经过期，这一次该开一个**新**窗口，⛔ 不该接着上一次数"
    );
    assert!(!u.passive_open(), "两次失败分属两个窗口，不该熔断");
}

/// 冷卻期沒滿 ⇒ 一律擋住，⛔ 不許有請求漏過去。
#[test]
fn 冷却期没满就一律挡住() {
    let r = one_upstream();
    let u = &target(&r).upstreams[0];
    u.record_passive_failure(Some(&LONG));
    assert!(u.record_passive_failure(Some(&LONG)));
    for _ in 0..50 {
        assert!(!u.passive_ok(Some(&LONG)), "冷却期内漏过去了");
    }
}

// ── 半開那一側 ────────────────────────────────────────────────────────────

/// ★ ★ ★ **承重：冷卻期滿之後，並行裡恰好一個拿到探針資格。**
///
/// ⚠ 這是 G136 選「真半開」而不是「到期全量放回」的**全部理由**：樞衡不換上游重試
/// ⇒ 打到壞上游的每一個請求都是使用者可見的錯。「全量放回」每個冷卻週期要付
/// `threshold` 個，半開把它釘在 **1** 個，而且是**結構上**的 1，與並行無關。
///
/// ★ 兩個策略有意不同：`EXPIRED` 把狀態推進「已到期的熔斷」那一格，
/// `LONG` 讓搶到探針的那一個把冷卻重新上滿 ⇒ ⛔ 整條判據不依賴任何 sleep。
#[test]
fn 冷却期满之后并发里恰好一个拿到探针资格() {
    let r = one_upstream();
    let t = target(&r);
    let u = &t.upstreams[0];
    u.record_passive_failure(Some(&EXPIRED));
    assert!(u.record_passive_failure(Some(&EXPIRED)), "该熔断");
    assert!(u.passive_open());

    const N: usize = 32;
    let passed = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..N {
            s.spawn(|| {
                if u.passive_ok(Some(&LONG)) {
                    passed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(
        passed.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "{N} 个并发里必须**恰好**一个拿到探针资格"
    );
}

/// 半開探針**成功** ⇒ 完全恢復，且回傳 `true` 讓呼叫方打一行「回來了」。
#[test]
fn 半开探针成功就完全恢复() {
    let r = one_upstream();
    let t = target(&r);
    let u = &t.upstreams[0];
    u.record_passive_failure(Some(&LONG));
    u.record_passive_failure(Some(&LONG));
    assert!(u.passive_open());

    assert!(
        u.record_passive_success(Some(&LONG)),
        "从熔断里回来时必须返回 true —— 调用方靠它打那一行日志"
    );
    assert!(!u.passive_open(), "成功之后还挂着熔断");
    assert!(u.passive_ok(Some(&LONG)));
    assert_eq!(t.pick_index_by(None), Some(0), "恢复之后必须重新进调度");
    // ⚠ 反方向：本來就沒熔斷的成功不該報「回來了」，否則每個成功請求都打一行紀錄。
    assert!(!u.record_passive_success(Some(&LONG)));
}

/// 半開探針**失敗** ⇒ 重新熔斷，冷卻重新上滿。
#[test]
fn 半开探针失败就重新熔断() {
    let r = one_upstream();
    let u = &target(&r).upstreams[0];
    u.record_passive_failure(Some(&EXPIRED));
    u.record_passive_failure(Some(&EXPIRED));
    assert!(
        u.passive_ok(Some(&LONG)),
        "冷却已过期，这一次该拿到探针资格"
    );
    // 探針失敗。
    assert!(
        !u.record_passive_failure(Some(&LONG)),
        "它本来就断着 ⇒ 这一次不是「刚熔断」，不该再打一行熔断日志"
    );
    assert!(u.passive_open(), "探针失败之后必须还断着");
    for _ in 0..20 {
        assert!(!u.passive_ok(Some(&LONG)), "冷却应当被重新上满");
    }
}

// ── 與主動健康檢查的邊界 ──────────────────────────────────────────────────

/// ★ ★ ★ **被動熔斷與主動健康檢查各佔一格、互不覆蓋**（G136 的承重決策）。
///
/// ⚠ 它要是共用 `healthy` 那一格，下一次主動探測成功就會把熔斷悄悄解除 ——
/// 而 PLAN 給這個特性的立身理由恰恰是「一個 `/health` 回 200 而真實業務在 500 的上游，
/// 主動檢查看不出來」⇒ 那樣它會在**唯一需要它的場景**上失效。
#[test]
fn 主动健康检查不许解除被动熔断() {
    let r = one_upstream();
    let t = target(&r);
    let u = &t.upstreams[0];
    u.record_passive_failure(Some(&LONG));
    u.record_passive_failure(Some(&LONG));
    assert!(u.passive_open());

    // 主動檢查說「健康」——它探的是另一條路。
    u.set_healthy(true);
    assert!(u.is_healthy(), "主动检查那一格该是健康");
    assert!(u.passive_open(), "主动检查不许清掉被动熔断");
    assert_eq!(
        t.pick_index_by(None),
        None,
        "两格里有一格说不行，筛子就该挡住"
    );

    // 反方向：被動熔斷也不許寫 `healthy` 那一格。
    let r2 = one_upstream();
    let u2 = &target(&r2).upstreams[0];
    u2.record_passive_failure(Some(&LONG));
    u2.record_passive_failure(Some(&LONG));
    assert!(u2.passive_open());
    assert!(u2.is_healthy(), "被动熔断不许去写主动检查那一格");
}

/// 多個上游時，熔斷掉一個 ⇒ 剩下的照常收流量（⛔ 不是整條 `reverse_proxy` 停擺）。
#[test]
fn 熔断一个之后剩下的照常收流量() {
    let r = rt("a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    passive_fail 2\n  }\n}\n");
    let t = target(&r);
    t.upstreams[0].record_passive_failure(Some(&LONG));
    t.upstreams[0].record_passive_failure(Some(&LONG));
    for _ in 0..20 {
        assert_eq!(
            t.pick_index_by(None),
            Some(1),
            "0 号被熔断，所有请求都该落到 1 号"
        );
    }
}
