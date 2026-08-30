//! 临时覆盖层的判据（**M2 批 N 任务 3**，G18 / 裁决 R7 · R8）。
//!
//! ★ ★ ★ 这一组守的核心是一句话：**覆盖层与上游身上那两个量在结构上是同一份。**
//! 做成「一份清单 + 一个应用步骤」的实现，会在「漏了应用那一步」时**完全静默** ——
//! 键对得上、命令回 200，而那台机器还在收流量。⇒ 第一条判据比的是**对象身份**
//! （`Arc::ptr_eq`），⛔ 不是「两格的值相等」：值相等在两者都是默认值时恒真，
//! 那是一把在好坏两种情况下读数相同的尺（AGENTS.md 门禁纪律第五条）。

use fulcrum_config::compile_str;
use fulcrum_runtime::overrides::{OverrideKey, OverrideLayer, RuntimeAction, RuntimeOp};
use fulcrum_runtime::{Runtime, SharedRuntime};
use std::sync::Arc;

/// DSL → 结构化配置。编译不过就是夹具写错了。
fn 配置(dsl: &str) -> fulcrum_config::StructuredConfig {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    o.config.expect("夹具应当能编译")
}

/// DSL → 挂好覆盖格子的运行时图。**装不上就把每一条装载错误逐条打出来。**
fn 建(dsl: &str, layer: &OverrideLayer) -> Runtime {
    match Runtime::build_with_overrides(&配置(dsl), layer) {
        Ok(r) => r,
        Err(e) => panic!(
            "这份配置必须装得上，而它报了 {} 条装载错误：\n{}",
            e.len(),
            e.iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

/// 一份把**每一种容器**都用上的夹具，另加「同键共享」「写了 id」「跨站点同地址」三种形状。
///
/// ⚠ ⚠ 它的形状是判据 1 的全部力量所在：挂格子走的是同一张图的**可变**走法，
/// 而漏掉任何一个分支（`handle` / `route` / `handle_errors` / 第二个站点）
/// 都是**完全静默**的 —— 那里的上游会悄悄留着构造点上那格私有的占位格子。
const 夹具: &str = "\
a.com {
  handle /api/* {
    reverse_proxy 10.0.0.2:2 {
      id pool_api
    }
  }
  handle /web/* {
    reverse_proxy 10.0.0.1:1
  }
  handle /dup/* {
    reverse_proxy 10.0.0.1:1
  }
  route {
    reverse_proxy 10.0.0.3:3
  }
  handle_errors {
    reverse_proxy 10.0.0.4:4
  }
}
b.com {
  reverse_proxy 10.0.0.1:1 10.0.0.5:5
}
";

/// 走一遍 `keyed_proxies()`，逐个比「上游身上那一格」与「登记处按它的键取到的那一格」。
/// 返回比了几个上游。
fn 逐个比对象身份(r: &Runtime, layer: &OverrideLayer) -> usize {
    let mut n = 0;
    for p in r.keyed_proxies() {
        for u in &p.target.upstreams {
            let key = OverrideKey::new(p.site, p.id, &u.addr);
            let 登记处那一格 = layer
                .get(&key)
                .unwrap_or_else(|| panic!("登记处里根本没有这个键：{key}"));
            assert!(
                Arc::ptr_eq(u.override_slot(), &登记处那一格),
                "{key}：上游身上那一格与登记处那一格**不是同一个对象** —— \
                 管理面改登记处对这个上游毫无作用，而没有任何东西会报错"
            );
            n += 1;
        }
    }
    n
}

// ── 判据 1 ─────────────────────────────────────────────────────────────────

/// ★ ★ ★ **每个 HTTP 上游身上的格子 = 登记处按它的键取到的那一格。**
///
/// 这是本任务唯一不可替代的一条。`build_step` 拿不到站点名 ⇒ 格子是「建完再挂」的，
/// 而**某条路径漏了「挂」这一步**的现场是：那个上游拿着一格谁也够不着的私有格子，
/// 管理面照样回 200，那台机器照样收流量，**没有任何东西会报错**。
#[test]
fn 判据1_每个上游身上的格子就是登记处按它的键取到的那一格() {
    let layer = OverrideLayer::new();
    let r = 建(夹具, &layer);
    let n = 逐个比对象身份(&r, &layer);
    // ⚠ 数目写死：一条「一个上游都没比到」的判据在任何实现下都是绿的。
    assert_eq!(n, 7, "夹具里应当有 7 个 HTTP 上游实例");
    assert_eq!(r.override_keys().len(), 6, "去重之后应当是 6 个键");
}

/// ★ ★ ★ **`SharedRuntime::new` 要从建好的运行时身上把格子认领过来。**
///
/// ⚠ `serve` 启动走的是 `Runtime::build`（那条路上还没有登记处）。不认领的话，
/// 启动之后第一条 `POST /runtime` 会在一张空登记表里**新建**一格 ——
/// 键对得上、命令回 200，而那台机器还在收流量。这是「漏挂」的另一副面孔。
#[test]
fn 判据1b_shared_runtime_从建好的运行时身上认领格子() {
    let rt = Arc::new(Runtime::build(&配置(夹具)).expect("装得上"));
    let shared = SharedRuntime::new(rt.clone());
    let n = 逐个比对象身份(&rt, shared.overrides());
    assert_eq!(n, 7);
    // 认领来的都是空格子 ⇒ 一项覆盖都没有。
    assert_eq!(shared.overrides().active_count(), 0);
    // ★ 反向半边：在登记处上摘一格，**这份正在服务的运行时**当场认。
    let k = OverrideKey::new("b.com", "", "10.0.0.5:5");
    shared.overrides().slot(&k).set_disabled(true);
    let 上游 = rt
        .keyed_proxies()
        .into_iter()
        .filter(|p| p.site == "b.com")
        .flat_map(|p| p.target.upstreams.iter())
        .find(|u| u.addr == "10.0.0.5:5")
        .expect("夹具里有这个上游");
    assert!(上游.is_disabled(), "改登记处必须立刻作用到那个上游身上");
}

// ── 判据 2 / 3：disable 的语义 ──────────────────────────────────────────────

#[test]
fn 判据2_设了disabled的上游掉出可用集() {
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        &layer,
    );
    let t = r.keyed_proxies()[0].target;
    let 数 = |n: usize| {
        let mut hits = [0usize; 2];
        for _ in 0..n {
            match t.pick_index_by(None) {
                Some(i) => hits[i] += 1,
                None => return None,
            }
        }
        Some(hits)
    };
    // ★ 先证明摘之前两个都在，否则下面那半说明不了什么。
    assert_eq!(数(10), Some([5, 5]));
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    assert_eq!(数(10), Some([0, 10]), "摘掉的那个不该再被选中");
    // 全摘光 ⇒ 一个可用的都没有 ⇒ `None`，调用方回 502。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.2:2"))
        .set_disabled(true);
    assert_eq!(t.pick_index_by(None), None);
    // ★ 放回去就该回来 —— 「摘掉」必须是可撤销的。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(false);
    assert_eq!(数(4), Some([4, 0]));
}

/// ★ ★ ★ **`disable` 让权重一起出局。**
///
/// 判据照抄任务 2 那条「摘掉的上游的权重不再计入总权重」，只是这次摘它的是覆盖层。
/// ⚠ 少了这一条，「按 6:2:2 分而那个 6 没在跑」会让 **60% 的请求落空**，
/// 而现场只看得到「一部分请求 502」——配置、健康检查、权重三处读起来全都正常。
#[test]
fn 判据3_disable让权重一起出局() {
    const N: usize = 100;
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 10.0.0.3:3 {\n    weight 10.0.0.1:1 3\n  }\n}\n",
        &layer,
    );
    let t = r.keyed_proxies()[0].target;
    let 数 = || {
        let mut hits = [0usize; 3];
        for _ in 0..N {
            hits[t.pick_index_by(None).unwrap()] += 1;
        }
        hits
    };
    // 摘之前：3:1:1（总权重 5）。
    assert_eq!(数(), [60, 20, 20]);
    // 把**权重最大的那个**摘掉。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    // 之后：总权重是 2 而不是 5 ⇒ 剩下两个严格 1:1，**一次都不落空**。
    assert_eq!(数(), [0, 50, 50], "被摘的那个的权重必须一起出局");
}

// ── 判据 4 / 5：覆盖权重 ───────────────────────────────────────────────────

#[test]
fn 判据4_设了覆盖权重之后取的是覆盖值而不是配置值() {
    const N: usize = 80;
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2 {\n    weight 10.0.0.1:1 3\n  }\n}\n",
        &layer,
    );
    let t = r.keyed_proxies()[0].target;
    let 数 = || {
        let mut hits = [0usize; 2];
        for _ in 0..N {
            hits[t.pick_index_by(None).unwrap()] += 1;
        }
        hits
    };
    assert_eq!(t.upstreams[0].weight(), 3, "没覆盖时取配置权重");
    assert_eq!(数(), [60, 20], "3:1（总权重 4）");
    let 格 = layer.slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"));
    格.set_weight(7).expect("7 在值域里");
    // ★ ★ 有效权重只有 `Upstream::weight()` 一个出口 ⇒ 调度立刻认。
    assert_eq!(t.upstreams[0].weight(), 7, "覆盖权重必须盖过配置权重");
    assert_eq!(数(), [70, 10], "7:1（总权重 8）");
    // ⚠ 没被覆盖的那个一个字都不许变。
    assert_eq!(t.upstreams[1].weight(), 1);
}

/// ★ 覆盖层里的 `0` 是**哨兵**（「没覆盖」）而不是权重（裁决 R3：`0` 不是合法权重）。
#[test]
fn 判据5_覆盖权重0是哨兵不是权重() {
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  reverse_proxy 10.0.0.1:1 {\n    weight 10.0.0.1:1 3\n  }\n}\n",
        &layer,
    );
    let t = r.keyed_proxies()[0].target;
    let 格 = layer.slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"));
    // ① 值域两端：`0` 与 `65536` 拒，`1` 与 `65535` 收。⛔ 值域常量只有一份。
    let e = 格.set_weight(0).expect_err("0 不是合法权重");
    assert!(
        e.contains('1') && e.contains("65535"),
        "报文要点名值域：{e}"
    );
    assert!(格.set_weight(65_536).is_err());
    assert!(格.set_weight(65_535).is_ok());
    assert!(格.set_weight(1).is_ok());
    // ② 被拒的那几次**一个字都没写进去**：现在是 1，不是 0 也不是 65536。
    assert_eq!(t.upstreams[0].weight(), 1);
    // ③ 撤掉覆盖 ⇒ 回到配置值 3，且这一格重新变成**空格子**。
    格.set_weight(9).unwrap();
    assert_eq!(t.upstreams[0].weight(), 9);
    assert!(格.has_override());
    格.clear_weight();
    assert_eq!(t.upstreams[0].weight(), 3, "撤掉覆盖要回到配置权重");
    assert!(!格.has_override(), "两个量都回到默认 ⇒ 不再是一项覆盖");
    assert_eq!(layer.active_count(), 0);
}

// ── 判据 6 / 7：同键共享 与 写了 id 就分开 ─────────────────────────────────

/// ★ ★ 同键共享：两条 `reverse_proxy` 撞同一格 ⇒ `disable` 一次，**两条都掉出可用集**。
///
/// ⚠ 这个语义从格子登记处的设计里**免费掉出来**（键相同 ⇒ 取到同一个 `Arc`），
/// ⛔ 别在别处补一条「键撞了要怎么办」的分支 —— 撞了就是同一格，那是设计不是意外。
#[test]
fn 判据6_同键的两条reverse_proxy共享一格_摘一次两条都掉出去() {
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  handle /api/* {\n    reverse_proxy 10.0.0.1:1\n  }\n  \
         handle /web/* {\n    reverse_proxy 10.0.0.1:1\n  }\n}\n",
        &layer,
    );
    let ps = r.keyed_proxies();
    assert_eq!(ps.len(), 2, "两条 reverse_proxy 都该在");
    // ① 两条身上那一格**是同一个对象**（不是「两格恰好都是默认值」）。
    assert!(Arc::ptr_eq(
        ps[0].target.upstreams[0].override_slot(),
        ps[1].target.upstreams[0].override_slot()
    ));
    assert_eq!(layer.slot_count(), 1, "两条撞同一个键 ⇒ 登记处只有一格");
    // ② 摘之前两条都挑得出上游。
    assert!(ps[0].target.pick_index_by(None).is_some());
    assert!(ps[1].target.pick_index_by(None).is_some());
    // ③ 摘一次 ⇒ 两条一起掉出可用集。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    assert_eq!(ps[0].target.pick_index_by(None), None);
    assert_eq!(ps[1].target.pick_index_by(None), None);
}

/// ★ 写了不同 `id` ⇒ 各占一格，`disable` 一条**不影响**另一条。
#[test]
fn 判据7_不同id各占一格_摘一条不影响另一条() {
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  handle /api/* {\n    reverse_proxy 10.0.0.1:1 {\n      id pool_a\n    }\n  }\n  \
         handle /web/* {\n    reverse_proxy 10.0.0.1:1 {\n      id pool_b\n    }\n  }\n}\n",
        &layer,
    );
    let ps = r.keyed_proxies();
    assert_eq!(layer.slot_count(), 2, "两个 id ⇒ 两格");
    assert!(
        !Arc::ptr_eq(
            ps[0].target.upstreams[0].override_slot(),
            ps[1].target.upstreams[0].override_slot()
        ),
        "写了不同 id 的两条不许共享格子"
    );
    layer
        .slot(&OverrideKey::new("a.com", "pool_a", "10.0.0.1:1"))
        .set_disabled(true);
    let 甲 = ps.iter().find(|p| p.id == "pool_a").unwrap();
    let 乙 = ps.iter().find(|p| p.id == "pool_b").unwrap();
    assert_eq!(甲.target.pick_index_by(None), None, "摘的是 pool_a");
    assert!(乙.target.pick_index_by(None).is_some(), "pool_b 不该受影响");
}

// ── 判据 8：悬空（裁决 R8）─────────────────────────────────────────────────

/// ★ 悬空覆盖**不删**，标 `dangling` 并且**仍在清单里**。
///
/// ⚠ 与 HAProxy 那个「runtime 改动 reload 之后无声消失」正相反：`keep` 就是 keep，
/// 而「它现在管不到谁」这件事**说出来**。
#[test]
fn 判据8_键落不到任何上游的覆盖被判为悬空且仍在清单里() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    shared.overrides().slot(&k1).set_disabled(true);
    // 现在它还落得到 ⇒ 不悬空。
    let es = shared.override_entries();
    assert_eq!(es.len(), 1);
    assert!(!es[0].dangling, "这时它还管得到那个上游");
    assert_eq!(shared.override_counts(), (1, 0));
    // 换成一份**不含那台机器**的配置（`overrides=keep` 那条路的形状）。
    let next = Runtime::build_with_overrides(
        &配置("a.com {\n  reverse_proxy 10.0.0.9:9\n}\n"),
        shared.overrides(),
    )
    .unwrap();
    shared.swap(next);
    let es = shared.override_entries();
    assert_eq!(es.len(), 1, "悬空的必须**仍在清单里**：{es:?}");
    assert_eq!(es[0].key, k1);
    assert!(es[0].disabled);
    assert!(es[0].dangling, "键落不到任何上游 ⇒ 悬空");
    // ⚠ 悬空的**照样算生效中**（裁决 R13）。
    assert_eq!(shared.override_counts(), (1, 1));
}

// ── 判据 9 / 10 / 11：空格子的生命周期（裁决 R7 末尾）─────────────────────

/// ★ 建 Runtime **失败**之后，覆盖计数仍然是 0（G8：建到一半失败不许留痕迹）。
#[test]
fn 判据9_建失败之后覆盖计数仍然是0() {
    let layer = OverrideLayer::new();
    // ① 一份 DSL 编得过、`Runtime::build` 建不起来的配置（第二条上游端口越界），
    //    而它里面**有一条完全正常的 `reverse_proxy`** —— 于是「建到一半登记了几格」
    //    这种实现在这里是有东西可登记的。
    let bad = 配置(
        "a.com {\n  handle /api/* {\n    reverse_proxy 10.0.0.1:1\n  }\n  \
         handle /web/* {\n    reverse_proxy x:70000\n  }\n}\n",
    );
    assert!(Runtime::build_with_overrides(&bad, &layer).is_err());
    assert_eq!(layer.active_count(), 0, "失败的 build 不许让覆盖计数动一下");
    assert!(layer.entries(&Default::default()).is_empty());

    // ② ★ ★ 反向半边：一次**成功的** build 会登记好几格 —— 而它们**照样不计数**。
    //    ⚠ 少了这一半，「一格都不登记」的实现与「空格子不计数」的实现读数完全一样。
    let r = 建(夹具, &layer);
    assert_eq!(layer.slot_count(), 6, "六个键各一格，全是空格子");
    assert_eq!(layer.active_count(), 0, "空格子不计数");
    assert!(
        layer.entries(&r.override_keys()).is_empty(),
        "空格子不进清单"
    );
}

/// ★ 成功 swap 之后：**没设过覆盖且新图不引用**的空格子被清掉。
///
/// ⚠ ⚠ 判据分两半，⛔ 不许合成一条（见下一条判据 11）：
/// 「清掉没人要的」与「别把悬空的一起清了」是两件事，一条判据只能验其中一件。
#[test]
fn 判据10_成功swap之后没设过覆盖又没人引用的空格子被清掉() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let k2 = OverrideKey::new("a.com", "", "10.0.0.2:2");
    let k3 = OverrideKey::new("a.com", "", "10.0.0.3:3");
    assert_eq!(shared.overrides().slot_count(), 2, "认领来两格空格子");
    // 换成一份只含第三台机器的配置。
    let next = Runtime::build_with_overrides(
        &配置("a.com {\n  reverse_proxy 10.0.0.3:3\n}\n"),
        shared.overrides(),
    )
    .unwrap();
    shared.swap(next);
    assert!(shared.overrides().get(&k1).is_none(), "空格子该被清掉");
    assert!(shared.overrides().get(&k2).is_none(), "空格子该被清掉");
    // ⚠ ⚠ **新图正引用的那一格不许清** —— 清了的话，管理面按键找到的会是一格
    //   新建的格子，改它对那个上游毫无作用。
    assert!(
        shared.overrides().get(&k3).is_some(),
        "新图引用的那格要留着"
    );
    assert_eq!(shared.overrides().slot_count(), 1);
    // ★ 换代之后判据 1 仍然成立。
    let n = 逐个比对象身份(&shared.current(), shared.overrides());
    assert_eq!(n, 1);
}

/// ★ ★ 成功 swap 之后：**设过覆盖但没人引用**的格子**留着** —— 那是悬空，不是垃圾。
#[test]
fn 判据11_成功swap之后设过覆盖但没人引用的格子留着() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let k2 = OverrideKey::new("a.com", "", "10.0.0.2:2");
    // k1 设过覆盖，k2 没有。两个都不在新图里。
    shared.overrides().slot(&k1).set_weight(5).unwrap();
    let next = Runtime::build_with_overrides(
        &配置("a.com {\n  reverse_proxy 10.0.0.3:3\n}\n"),
        shared.overrides(),
    )
    .unwrap();
    shared.swap(next);
    let 留下的 = shared.overrides().get(&k1).expect("设过覆盖的必须留着");
    assert_eq!(留下的.weight(), Some(5), "留下来的那一格连值一起留着");
    assert!(shared.overrides().get(&k2).is_none(), "没设过覆盖的才清");
    assert_eq!(shared.override_counts(), (1, 1), "它现在是一项悬空覆盖");
}

// ── 判据 12 / 13：两条不许被碰的路（计划 §2 的 S6 / S7）───────────────────

/// ★ **L4 那条路一个字节没变**（S6）。
///
/// L4 的上游没有站点 ⇒ 按裁决 R6 的键根本寻址不到它们。
#[test]
fn 判据12_l4那条路一个字节没变() {
    let layer = OverrideLayer::new();
    let r = 建(
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306 10.0.0.6:3306\n  }\n  \
         tcp :3307 {\n    sni a.example {\n      proxy 10.0.0.7:3306\n    }\n  }\n}\n",
        &layer,
    );
    // ① ★ ★ 登记处**一格都没有** —— 这一条同时挡住「把 L4 也收进键里」的实现：
    //    那种实现下这个数会 ≥ 1，而①之外的判据对它毫无感觉。
    assert_eq!(layer.slot_count(), 0, "L4 的上游不该进登记处");
    assert!(r.override_keys().is_empty());
    let t = r.l4_listeners[0].target.as_ref().expect("有兜底 proxy");
    // ② 有效权重恒等于配置权重（L4 恒 1），也没被摘。
    for u in &t.upstreams {
        assert_eq!(u.weight(), 1);
        assert!(!u.is_disabled());
    }
    // ③ 落点与批 N 之前逐字相同：等权轮询就是取模。
    let seq: Vec<usize> = (0..6).map(|_| t.pick_index_by(None).unwrap()).collect();
    assert_eq!(seq, vec![0, 1, 0, 1, 0, 1]);
    // ④ 分流规则里那一组也一样。
    let s = &r.l4_listeners[1].rules[0].target;
    assert_eq!(s.upstreams[0].weight(), 1);
    assert!(!s.upstreams[0].is_disabled());
}

/// ★ **`Runtime::build` 那条路与覆盖层完全无关**（S7：`fulcrum validate` 必须继续
/// 离线可跑，输出一个字节都不变）。
#[test]
fn 判据13_runtime_build那条路与覆盖层完全无关() {
    let layer = OverrideLayer::new();
    let 带登记处 = 建(夹具, &layer);
    let 不带 = Runtime::build(&配置(夹具)).expect("装得上");
    // ① 两条路建出来的图，键与权重逐项相同。
    assert_eq!(带登记处.override_keys(), 不带.override_keys());
    let 权重们 = |r: &Runtime| -> Vec<u32> {
        r.keyed_proxies()
            .iter()
            .flat_map(|p| p.target.upstreams.iter())
            .map(|u| u.weight())
            .collect()
    };
    let 摘之前的权重 = 权重们(&不带);
    assert_eq!(权重们(&带登记处), 摘之前的权重);
    // ② ★ ★ 反向半边：把登记处里**每一格**都摘掉。
    //    带登记处那一份当场全线掉出可用集，而 `Runtime::build` 那一份**一个字都不变**
    //    —— 后者走的是一次性登记表，谁也够不着。
    for k in 带登记处.override_keys() {
        layer.slot(&k).set_disabled(true);
    }
    for p in 带登记处.keyed_proxies() {
        assert_eq!(p.target.pick_index_by(None), None, "带登记处的该全摘掉了");
    }
    for p in 不带.keyed_proxies() {
        assert!(
            p.target.pick_index_by(None).is_some(),
            "`Runtime::build` 出来的图不许被任何登记处影响"
        );
    }
    assert_eq!(权重们(&不带), 摘之前的权重, "权重也一个字都不许变");
}

// ── 任务 4：`OverrideLayer::apply_all`（`POST /runtime` 的复合方法）─────────
//
// ★ 这一组只守 `apply_all` 自己的契约（全有或全无 · 寻址口径 · TOCTOU 安全），
// 不碰 JSON / verb 解析——那部分的判据在 `fulcrum-server/src/admin.rs`。

/// 全部指得到 ⇒ 在同一次持锁内逐条施加，两种操作都要生效。
#[test]
fn 任务4_apply_all_全部指得到就全部生效() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let k2 = OverrideKey::new("a.com", "", "10.0.0.2:2");
    let live = shared.current().override_keys();
    let actions = vec![
        RuntimeAction {
            key: k1.clone(),
            op: RuntimeOp::SetDisabled(true),
        },
        RuntimeAction {
            key: k2.clone(),
            op: RuntimeOp::SetWeight(9),
        },
    ];
    shared
        .overrides()
        .apply_all(&live, &actions)
        .expect("两条都指得到，应当整批生效");
    assert!(shared.overrides().get(&k1).unwrap().is_disabled());
    assert_eq!(shared.overrides().get(&k2).unwrap().weight(), Some(9));
}

/// ★ ★ 全有或全无：第二条指不到 ⇒ 第一条也不生效。
#[test]
fn 任务4_apply_all_全有或全无_一条指不到就一条都不生效() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let 查无此键 = OverrideKey::new("a.com", "", "10.9.9.9:9");
    let live = shared.current().override_keys();
    let actions = vec![
        RuntimeAction {
            key: k1.clone(),
            op: RuntimeOp::SetDisabled(true),
        },
        RuntimeAction {
            key: 查无此键.clone(),
            op: RuntimeOp::SetDisabled(true),
        },
    ];
    let err = shared
        .overrides()
        .apply_all(&live, &actions)
        .expect_err("第二条指不到，整批应当失败");
    assert_eq!(err, vec![查无此键]);
    assert!(
        !shared.overrides().get(&k1).unwrap().is_disabled(),
        "第一条不该生效——全有或全无"
    );
}

/// ★ 悬空的键（登记处里还在、但当前运行时的 `override_keys()` 已经不认）
/// 不算「指得到」——`/runtime` 不是 `keep` 的操作对象。
#[test]
fn 任务4_apply_all_悬空的键不算指得到() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    shared.overrides().slot(&k1).set_disabled(true);
    // 换一份不再有这台机器的配置——k1 现在悬空：设过覆盖，`retain_after_swap`
    // 把它留着，但当前运行时的 `override_keys()` 已经不认它。
    let next = Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.9:9\n}\n")).unwrap();
    shared.swap(next);
    assert!(
        shared.overrides().get(&k1).is_some(),
        "悬空覆盖不删，登记处里还在"
    );
    let live = shared.current().override_keys();
    assert!(!live.contains(&k1), "悬空的键不该在 live 集合里");
    let actions = vec![RuntimeAction {
        key: k1.clone(),
        op: RuntimeOp::SetDisabled(false),
    }];
    let err = shared
        .overrides()
        .apply_all(&live, &actions)
        .expect_err("悬空的键不该能被 /runtime 寻址到");
    assert_eq!(err, vec![k1.clone()]);
    // ⚠ 而且没有被动过——它应当仍然是 disabled。
    assert!(shared.overrides().get(&k1).unwrap().is_disabled());
}

/// ★ ★ ★ 直接复现评审点名的 TOCTOU 现场（不靠真的并发）：拿一份**旧的** `live`
/// 快照（那一刻这个键确实是活的），随后让它在登记处里被真实收走（一次成功的
/// swap，且这一格从没设过覆盖 ⇒ `retain_after_swap` 会把它真的清掉——不是
/// 悬空，悬空要求「设过覆盖」），再拿这份**过期**的 `live` 去调 `apply_all`。
///
/// 如果实现只信调用方传来的 `live`、不在持锁期间回头再看登记处一眼，这里就会
/// 「查」到（因为 `live` 说它在）却在「改」的时候悄悄改不到东西——一个只检查
/// `live` 而在改的时候对 `None` 静默 `continue` 的实现会让这条测试看到 `Ok(())`
/// 而不是期望的 `Err`。
#[test]
fn 任务4_apply_all_live快照过期而登记处已经收走时仍然拒绝() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let 过期的_live = shared.current().override_keys();
    assert!(过期的_live.contains(&k1), "快照那一刻它确实是活的");

    let next = Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.9:9\n}\n")).unwrap();
    shared.swap(next);
    assert!(
        shared.overrides().get(&k1).is_none(),
        "夹具前提：这一格必须真的已经被收走，不然这条测试没测到 TOCTOU"
    );

    let actions = vec![RuntimeAction {
        key: k1.clone(),
        op: RuntimeOp::SetDisabled(true),
    }];
    let err = shared
        .overrides()
        .apply_all(&过期的_live, &actions)
        .expect_err("live 快照过期、登记处已经收走 ⇒ 必须拒绝，不能悄悄改一个孤儿");
    assert_eq!(err, vec![k1]);
}

/// 指不到时不留垃圾格子：`apply_all` 只读 `grids`，从不调用会现建格子的
/// [`OverrideLayer::slot`]。
#[test]
fn 任务4_apply_all_指不到时不留垃圾格子() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let before = shared.overrides().slot_count();
    let 查无此键 = OverrideKey::new("a.com", "", "10.9.9.9:9");
    let live = shared.current().override_keys();
    let actions = vec![RuntimeAction {
        key: 查无此键,
        op: RuntimeOp::SetDisabled(true),
    }];
    shared.overrides().apply_all(&live, &actions).unwrap_err();
    assert_eq!(
        shared.overrides().slot_count(),
        before,
        "失败的 apply_all 不该留下垃圾格子"
    );
}

/// ★ ★ ★ 修复轮 1，评审 M4：`apply_all` 自己**不信任调用方**——
/// 越界的 `SetWeight` 必须被拒绝，⛔ 不许静默吞掉之后仍然回 `Ok`。
///
/// `RuntimeOp` 与 `apply_all` 都是 `pub`，`admin.rs` 的解析阶段那道值域预检
/// 不是唯一防线：这里直接绕过 `admin.rs`，构造一个 `SetWeight(0)`（越界，
/// 裁决 R3：`0` 不是合法权重）直接调 `apply_all`，断言它必须 `Err`，
/// 而且**一格都没改**（`weight()` 仍然是 `None`）。
#[test]
fn 任务4_apply_all_越界的set_weight被拒而不是静默吞掉() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let live = shared.current().override_keys();
    for bad in [0u32, 65536] {
        let actions = vec![RuntimeAction {
            key: k1.clone(),
            op: RuntimeOp::SetWeight(bad),
        }];
        shared
            .overrides()
            .apply_all(&live, &actions)
            .expect_err(&format!("越界权重 {bad} 必须被 apply_all 自己拒绝"));
        assert_eq!(
            shared.overrides().get(&k1).unwrap().weight(),
            None,
            "越界权重 {bad} 一格都不该改——不能回 Err 却仍然把值写进去"
        );
    }
}
