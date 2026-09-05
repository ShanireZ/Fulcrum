//! 被动熔断（`passive_fail`，**G136**）的状态机与筛子第四条。
//!
//! ★ ★ **这里全部判据都不 sleep 到某个阈值上**：本机常年 20+ 会话并发，实测比空载慢
//! 3.75 倍 ⇒ 按空载余量设的超时会**周期性假红**，而一道假红过几次的门会被人忽略。
//! ⇒ 时间那一维靠**把策略里的时长设成 0**（立刻到期）或**设成 60s**（一定没到期）来取，
//! 唯一用到 `sleep` 的那一条断言是**单边**的（「过了 0 毫秒」），任何真实的睡眠都满足它。
//!
//! ⚠ `PassivePolicy` 是**每次调用传进去的参数**而不是存在状态里，
//! 所以一条判据可以用两个不同的策略把状态推到想要的那一格 —— 下面并发那条就靠这个。

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

/// 取第一条 `reverse_proxy` 的目标。
fn target(r: &Runtime) -> &ProxyTarget {
    r.keyed_proxies()[0].target
}

/// 立刻到期的冷却 ⇒ 熔断之后马上就是「半开可抢」那一格。
const EXPIRED: PassivePolicy = PassivePolicy {
    threshold: 2,
    window_ms: 60_000,
    cooldown_ms: 0,
};

/// 一定没到期的冷却。
const LONG: PassivePolicy = PassivePolicy {
    threshold: 2,
    window_ms: 60_000,
    cooldown_ms: 60_000,
};

// ── 关闭那一侧 ────────────────────────────────────────────────────────────

/// ⚠ ⚠ **不写 `passive_fail` ⇒ 策略是 `None`，筛子第四条恒放行。**
///
/// ★ 这是「既有配置行为一个字不变」在运行时这一层的判据。G136 选「缺省关闭」
/// 而不是 nginx 那种「缺省开」，全部理由就在这一条上。
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

/// 配了就该建出策略，且三个值逐个落对 —— ⚠ 三个数**互不相同**，
/// 否则「两个旋钮接反了」在这里看不出来。
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

// ── 熔断那一侧 ────────────────────────────────────────────────────────────

fn one_upstream() -> Runtime {
    rt("a.com {\n  reverse_proxy 10.0.0.1:1 {\n    passive_fail 2\n  }\n}\n")
}

/// 窗口内失败数到阈值 ⇒ 熔断，且**筛子当场挡住它**。
///
/// ⚠ 判据分两句：`passive_open()` 说的是状态，`pick_index_by` 说的是**调度真的认这件事**。
/// ★ 少了后一句，一个「状态记对了、筛子却没接上」的实现照样全绿 ——
/// 而那正是这个特性唯一要做的事。
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

/// ⚠ **窗口过期之后计数重来** —— 稀疏的偶发失败不该攒够一次熔断。
///
/// ★ 判据是**单边**的：`window_ms = 0` 配上一次真实的 `sleep`，
/// 「已经过了 0 毫秒」这件事任何真实睡眠都满足 ⇒ ⛔ 不依赖任何余量。
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

/// 冷却期没满 ⇒ 一律挡住，⛔ 不许有请求漏过去。
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

// ── 半开那一侧 ────────────────────────────────────────────────────────────

/// ★ ★ ★ **承重：冷却期满之后，并发里恰好一个拿到探针资格。**
///
/// ⚠ 这是 G136 选「真半开」而不是「到期全量放回」的**全部理由**：枢衡不换上游重试
/// ⇒ 打到坏上游的每一个请求都是用户可见的错。「全量放回」每个冷却周期要付
/// `threshold` 个，半开把它钉在 **1** 个，而且是**结构上**的 1，与并发无关。
///
/// ★ 两个策略有意不同：`EXPIRED` 把状态推进「已到期的熔断」那一格，
/// `LONG` 让抢到探针的那一个把冷却重新上满 ⇒ ⛔ 整条判据不依赖任何 sleep。
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

/// 半开探针**成功** ⇒ 完全恢复，且返回 `true` 让调用方打一行「回来了」。
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
    // ⚠ 反方向：本来就没熔断的成功不该报「回来了」，否则每个成功请求都打一行日志。
    assert!(!u.record_passive_success(Some(&LONG)));
}

/// 半开探针**失败** ⇒ 重新熔断，冷却重新上满。
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
    // 探针失败。
    assert!(
        !u.record_passive_failure(Some(&LONG)),
        "它本来就断着 ⇒ 这一次不是「刚熔断」，不该再打一行熔断日志"
    );
    assert!(u.passive_open(), "探针失败之后必须还断着");
    for _ in 0..20 {
        assert!(!u.passive_ok(Some(&LONG)), "冷却应当被重新上满");
    }
}

// ── 与主动健康检查的边界 ──────────────────────────────────────────────────

/// ★ ★ ★ **被动熔断与主动健康检查各占一格、互不覆盖**（G136 的承重决策）。
///
/// ⚠ 它要是共用 `healthy` 那一格，下一次主动探测成功就会把熔断悄悄解除 ——
/// 而 PLAN 给这个特性的立身理由恰恰是「一个 `/health` 回 200 而真实业务在 500 的上游，
/// 主动检查看不出来」⇒ 那样它会在**唯一需要它的场景**上失效。
#[test]
fn 主动健康检查不许解除被动熔断() {
    let r = one_upstream();
    let t = target(&r);
    let u = &t.upstreams[0];
    u.record_passive_failure(Some(&LONG));
    u.record_passive_failure(Some(&LONG));
    assert!(u.passive_open());

    // 主动检查说「健康」——它探的是另一条路。
    u.set_healthy(true);
    assert!(u.is_healthy(), "主动检查那一格该是健康");
    assert!(u.passive_open(), "主动检查不许清掉被动熔断");
    assert_eq!(
        t.pick_index_by(None),
        None,
        "两格里有一格说不行，筛子就该挡住"
    );

    // 反方向：被动熔断也不许写 `healthy` 那一格。
    let r2 = one_upstream();
    let u2 = &target(&r2).upstreams[0];
    u2.record_passive_failure(Some(&LONG));
    u2.record_passive_failure(Some(&LONG));
    assert!(u2.passive_open());
    assert!(u2.is_healthy(), "被动熔断不许去写主动检查那一格");
}

/// 多个上游时，熔断掉一个 ⇒ 剩下的照常收流量（⛔ 不是整条 `reverse_proxy` 停摆）。
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
