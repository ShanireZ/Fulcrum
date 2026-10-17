//! 臨時覆蓋層的判據（**M2 批 N 任務 3**，G18 / 裁決 R7 · R8）。
//!
//! ★ ★ ★ 這一組守的核心是一句話：**覆蓋層與上游身上那兩個量在結構上是同一份。**
//! 做成「一份清單 + 一個應用步驟」的實作，會在「漏了應用那一步」時**完全靜默** ——
//! 鍵對得上、命令回 200，而那臺機器還在收流量。⇒ 第一條判據比的是**物件身分**
//! （`Arc::ptr_eq`），⛔ 不是「兩格的值相等」：值相等在兩者都是預設值時恆真，
//! 那是一把在好壞兩種情況下讀數相同的尺（AGENTS.md 門禁紀律第五條）。

use fulcrum_config::compile_str;
use fulcrum_runtime::overrides::{OverrideKey, OverrideLayer, RuntimeAction, RuntimeOp};
use fulcrum_runtime::{Runtime, SharedRuntime};
use std::sync::Arc;

/// DSL → 結構化設定。編譯不過就是夾具寫錯了。
fn 配置(dsl: &str) -> fulcrum_config::StructuredConfig {
    let o = compile_str("t.Fulcrumfile", dsl);
    assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
    o.config.expect("夹具应当能编译")
}

/// DSL → 掛好覆蓋格子的執行環境圖。**裝不上就把每一條裝載錯誤逐條打出來。**
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

/// 一份把**每一種容器**都用上的夾具，另加「同鍵共享」「寫了 id」「跨站台同位址」三種形狀。
///
/// ⚠ ⚠ 它的形狀是判據 1 的全部力量所在：掛格子走的是同一張圖的**可變**走法，
/// 而漏掉任何一個分支（`handle` / `route` / `handle_errors` / 第二個站台）
/// 都是**完全靜默**的 —— 那裡的上游會悄悄留著建構點上那格私有的佔位格子。
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

/// 走一遍 `keyed_proxies()`，逐個比「上游身上那一格」與「登記處按它的鍵取到的那一格」。
/// 回傳比了幾個上游。
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

// ── 判據 1 ─────────────────────────────────────────────────────────────────

/// ★ ★ ★ **每個 HTTP 上游身上的格子 = 登記處按它的鍵取到的那一格。**
///
/// 這是本任務唯一不可替代的一條。`build_step` 拿不到站台名 ⇒ 格子是「建完再掛」的，
/// 而**某條路徑漏了「掛」這一步**的現場是：那個上游拿著一格誰也夠不著的私有格子，
/// 管理面照樣回 200，那臺機器照樣收流量，**沒有任何東西會回報錯誤**。
#[test]
fn 判据1_每个上游身上的格子就是登记处按它的键取到的那一格() {
    let layer = OverrideLayer::new();
    let r = 建(夹具, &layer);
    let n = 逐个比对象身份(&r, &layer);
    // ⚠ 數目寫死：一條「一個上游都沒比到」的判據在任何實作下都是綠的。
    assert_eq!(n, 7, "夹具里应当有 7 个 HTTP 上游实例");
    assert_eq!(r.override_keys().len(), 6, "去重之后应当是 6 个键");
}

/// ★ ★ ★ **`SharedRuntime::new` 要從建好的執行環境身上把格子認領過來。**
///
/// ⚠ `serve` 啟動走的是 `Runtime::build`（那條路上還沒有登記處）。不認領的話，
/// 啟動之後第一條 `POST /runtime` 會在一張空登記表裡**新建**一格 ——
/// 鍵對得上、命令回 200，而那臺機器還在收流量。這是「漏掛」的另一副面孔。
#[test]
fn 判据1b_shared_runtime_从建好的运行时身上认领格子() {
    let rt = Arc::new(Runtime::build(&配置(夹具)).expect("装得上"));
    let shared = SharedRuntime::new(rt.clone());
    let n = 逐个比对象身份(&rt, shared.overrides());
    assert_eq!(n, 7);
    // 認領來的都是空格子 ⇒ 一項覆蓋都沒有。
    assert_eq!(shared.overrides().active_count(), 0);
    // ★ 反向半邊：在登記處上摘一格，**這份正在服務的執行環境**當場認。
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

// ── 判據 2 / 3：disable 的語義 ──────────────────────────────────────────────

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
    // ★ 先證明摘之前兩個都在，否則下面那半說明不了什麼。
    assert_eq!(数(10), Some([5, 5]));
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    assert_eq!(数(10), Some([0, 10]), "摘掉的那个不该再被选中");
    // 全摘光 ⇒ 一個可用的都沒有 ⇒ `None`，呼叫方回 502。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.2:2"))
        .set_disabled(true);
    assert_eq!(t.pick_index_by(None), None);
    // ★ 放回去就該回來 —— 「摘掉」必須是可撤銷的。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(false);
    assert_eq!(数(4), Some([4, 0]));
}

/// ★ ★ ★ **`disable` 讓權重一起出局。**
///
/// 判據照抄任務 2 那條「摘掉的上游的權重不再計入總權重」，只是這次摘它的是覆蓋層。
/// ⚠ 少了這一條，「按 6:2:2 分而那個 6 沒在跑」會讓 **60% 的請求落空**，
/// 而現場只看得到「一部分請求 502」——設定、健康檢查、權重三處讀起來全都正常。
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
    // 摘之前：3:1:1（總權重 5）。
    assert_eq!(数(), [60, 20, 20]);
    // 把**權重最大的那個**摘掉。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    // 之後：總權重是 2 而不是 5 ⇒ 剩下兩個嚴格 1:1，**一次都不落空**。
    assert_eq!(数(), [0, 50, 50], "被摘的那个的权重必须一起出局");
}

// ── 判據 4 / 5：覆蓋權重 ───────────────────────────────────────────────────

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
    // ★ ★ 有效權重只有 `Upstream::weight()` 一個出口 ⇒ 排程立刻認。
    assert_eq!(t.upstreams[0].weight(), 7, "覆盖权重必须盖过配置权重");
    assert_eq!(数(), [70, 10], "7:1（总权重 8）");
    // ⚠ 沒被覆蓋的那個一個字都不許變。
    assert_eq!(t.upstreams[1].weight(), 1);
}

/// ★ 覆蓋層裡的 `0` 是**哨兵**（「沒覆蓋」）而不是權重（裁決 R3：`0` 不是合法權重）。
#[test]
fn 判据5_覆盖权重0是哨兵不是权重() {
    let layer = OverrideLayer::new();
    let r = 建(
        "a.com {\n  reverse_proxy 10.0.0.1:1 {\n    weight 10.0.0.1:1 3\n  }\n}\n",
        &layer,
    );
    let t = r.keyed_proxies()[0].target;
    let 格 = layer.slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"));
    // ① 值域兩端：`0` 與 `65536` 拒，`1` 與 `65535` 收。⛔ 值域常數只有一份。
    let e = 格.set_weight(0).expect_err("0 不是合法权重");
    assert!(
        e.contains('1') && e.contains("65535"),
        "报文要点名值域：{e}"
    );
    assert!(格.set_weight(65_536).is_err());
    assert!(格.set_weight(65_535).is_ok());
    assert!(格.set_weight(1).is_ok());
    // ② 被拒的那幾次**一個字都沒寫進去**：現在是 1，不是 0 也不是 65536。
    assert_eq!(t.upstreams[0].weight(), 1);
    // ③ 撤掉覆蓋 ⇒ 回到設定值 3，且這一格重新變成**空格子**。
    格.set_weight(9).unwrap();
    assert_eq!(t.upstreams[0].weight(), 9);
    assert!(格.has_override());
    格.clear_weight();
    assert_eq!(t.upstreams[0].weight(), 3, "撤掉覆盖要回到配置权重");
    assert!(!格.has_override(), "两个量都回到默认 ⇒ 不再是一项覆盖");
    assert_eq!(layer.active_count(), 0);
}

// ── 判據 6 / 7：同鍵共享 與 寫了 id 就分開 ─────────────────────────────────

/// ★ ★ 同鍵共享：兩條 `reverse_proxy` 撞同一格 ⇒ `disable` 一次，**兩條都掉出可用集**。
///
/// ⚠ 這個語義從格子登記處的設計裡**免費掉出來**（鍵相同 ⇒ 取到同一個 `Arc`），
/// ⛔ 別在別處補一條「鍵撞了要怎麼辦」的分支 —— 撞了就是同一格，那是設計不是意外。
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
    // ① 兩條身上那一格**是同一個物件**（不是「兩格恰好都是預設值」）。
    assert!(Arc::ptr_eq(
        ps[0].target.upstreams[0].override_slot(),
        ps[1].target.upstreams[0].override_slot()
    ));
    assert_eq!(layer.slot_count(), 1, "两条撞同一个键 ⇒ 登记处只有一格");
    // ② 摘之前兩條都挑得出上游。
    assert!(ps[0].target.pick_index_by(None).is_some());
    assert!(ps[1].target.pick_index_by(None).is_some());
    // ③ 摘一次 ⇒ 兩條一起掉出可用集。
    layer
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    assert_eq!(ps[0].target.pick_index_by(None), None);
    assert_eq!(ps[1].target.pick_index_by(None), None);
}

/// ★ 寫了不同 `id` ⇒ 各佔一格，`disable` 一條**不影響**另一條。
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

// ── 判據 8：懸空（裁決 R8）─────────────────────────────────────────────────

/// ★ 懸空覆蓋**不刪**，標 `dangling` 並且**仍在清單裡**。
///
/// ⚠ 與 HAProxy 那個「runtime 改動 reload 之後無聲消失」正相反：`keep` 就是 keep，
/// 而「它現在管不到誰」這件事**說出來**。
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
    // 現在它還落得到 ⇒ 不懸空。
    let es = shared.override_entries();
    assert_eq!(es.len(), 1);
    assert!(!es[0].dangling, "这时它还管得到那个上游");
    assert_eq!(shared.override_counts(), (1, 0));
    // 換成一份**不含那臺機器**的設定（`overrides=keep` 那條路的形狀）。
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
    // ⚠ 懸空的**照樣算生效中**（裁決 R13）。
    assert_eq!(shared.override_counts(), (1, 1));
}

// ── 判據 9 / 10 / 11：空格子的生命週期（裁決 R7 末尾）─────────────────────

/// ★ 建 Runtime **失敗**之後，覆蓋計數仍然是 0（G8：建到一半失敗不許留痕跡）。
#[test]
fn 判据9_建失败之后覆盖计数仍然是0() {
    let layer = OverrideLayer::new();
    // ① 一份 DSL 編得過、`Runtime::build` 建不起來的設定（第二條上游連接埠越界），
    //    而它裡面**有一條完全正常的 `reverse_proxy`** —— 於是「建到一半登記了幾格」
    //    這種實作在這裡是有東西可登記的。
    let bad = 配置(
        "a.com {\n  handle /api/* {\n    reverse_proxy 10.0.0.1:1\n  }\n  \
         handle /web/* {\n    reverse_proxy x:70000\n  }\n}\n",
    );
    assert!(Runtime::build_with_overrides(&bad, &layer).is_err());
    assert_eq!(layer.active_count(), 0, "失败的 build 不许让覆盖计数动一下");
    assert!(layer.entries(&Default::default()).is_empty());

    // ② ★ ★ 反向半邊：一次**成功的** build 會登記好幾格 —— 而它們**照樣不計數**。
    //    ⚠ 少了這一半，「一格都不登記」的實作與「空格子不計數」的實作讀數完全一樣。
    let r = 建(夹具, &layer);
    assert_eq!(layer.slot_count(), 6, "六个键各一格，全是空格子");
    assert_eq!(layer.active_count(), 0, "空格子不计数");
    assert!(
        layer.entries(&r.override_keys()).is_empty(),
        "空格子不进清单"
    );
}

/// ★ 成功 swap 之後：**沒設過覆蓋且新圖不引用**的空格子被清掉。
///
/// ⚠ ⚠ 判據分兩半，⛔ 不許合成一條（見下一條判據 11）：
/// 「清掉沒人要的」與「別把懸空的一起清了」是兩件事，一條判據只能驗其中一件。
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
    // 換成一份只含第三臺機器的設定。
    let next = Runtime::build_with_overrides(
        &配置("a.com {\n  reverse_proxy 10.0.0.3:3\n}\n"),
        shared.overrides(),
    )
    .unwrap();
    shared.swap(next);
    assert!(shared.overrides().get(&k1).is_none(), "空格子该被清掉");
    assert!(shared.overrides().get(&k2).is_none(), "空格子该被清掉");
    // ⚠ ⚠ **新圖正引用的那一格不許清** —— 清了的話，管理面按鍵找到的會是一格
    //   新建的格子，改它對那個上游毫無作用。
    assert!(
        shared.overrides().get(&k3).is_some(),
        "新图引用的那格要留着"
    );
    assert_eq!(shared.overrides().slot_count(), 1);
    // ★ 換代之後判據 1 仍然成立。
    let n = 逐个比对象身份(&shared.current(), shared.overrides());
    assert_eq!(n, 1);
}

/// ★ ★ 成功 swap 之後：**設過覆蓋但沒人引用**的格子**留著** —— 那是懸空，不是垃圾。
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
    // k1 設過覆蓋，k2 沒有。兩個都不在新圖裡。
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

// ── 判據 12 / 13：兩條不許被碰的路（計畫 §2 的 S6 / S7）───────────────────

/// ★ **L4 那條路一個位元組沒變**（S6）。
///
/// L4 的上游沒有站台 ⇒ 按裁決 R6 的鍵根本尋址不到它們。
#[test]
fn 判据12_l4那条路一个字节没变() {
    let layer = OverrideLayer::new();
    let r = 建(
        "l4 {\n  tcp :3306 {\n    proxy 10.0.0.5:3306 10.0.0.6:3306\n  }\n  \
         tcp :3307 {\n    sni a.example {\n      proxy 10.0.0.7:3306\n    }\n  }\n}\n",
        &layer,
    );
    // ① ★ ★ 登記處**一格都沒有** —— 這一條同時擋住「把 L4 也收進鍵裡」的實作：
    //    那種實作下這個數會 ≥ 1，而①之外的判據對它毫無感覺。
    assert_eq!(layer.slot_count(), 0, "L4 的上游不该进登记处");
    assert!(r.override_keys().is_empty());
    let t = r.l4_listeners[0].target.as_ref().expect("有兜底 proxy");
    // ② 有效權重恆等於設定權重（L4 恆 1），也沒被摘。
    for u in &t.upstreams {
        assert_eq!(u.weight(), 1);
        assert!(!u.is_disabled());
    }
    // ③ 落點與批 N 之前逐字相同：等權輪詢就是取模。
    let seq: Vec<usize> = (0..6).map(|_| t.pick_index_by(None).unwrap()).collect();
    assert_eq!(seq, vec![0, 1, 0, 1, 0, 1]);
    // ④ 分流規則裡那一組也一樣。
    let s = &r.l4_listeners[1].rules[0].target;
    assert_eq!(s.upstreams[0].weight(), 1);
    assert!(!s.upstreams[0].is_disabled());
}

/// ★ **`Runtime::build` 那條路與覆蓋層完全無關**（S7：`fulcrum validate` 必須繼續
/// 離線可跑，輸出一個位元組都不變）。
#[test]
fn 判据13_runtime_build那条路与覆盖层完全无关() {
    let layer = OverrideLayer::new();
    let 带登记处 = 建(夹具, &layer);
    let 不带 = Runtime::build(&配置(夹具)).expect("装得上");
    // ① 兩條路建出來的圖，鍵與權重逐項相同。
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
    // ② ★ ★ 反向半邊：把登記處裡**每一格**都摘掉。
    //    帶登記處那一份當場全線掉出可用集，而 `Runtime::build` 那一份**一個字都不變**
    //    —— 後者走的是一次性登記表，誰也夠不著。
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

// ── 任務 4：`OverrideLayer::apply_all`（`POST /runtime` 的複合方法）─────────
//
// ★ 這一組只守 `apply_all` 自己的契約（全有或全無 · 尋址口徑 · TOCTOU 安全），
// 不碰 JSON / verb 解析——那部分的判據在 `fulcrum-server/src/admin.rs`。

/// 全部指得到 ⇒ 在同一次持鎖內逐條施加，兩種操作都要生效。
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

/// ★ ★ 全有或全無：第二條指不到 ⇒ 第一條也不生效。
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

/// ★ 懸空的鍵（登記處裡還在、但當前執行環境的 `override_keys()` 已經不認）
/// 不算「指得到」——`/runtime` 不是 `keep` 的操作物件。
#[test]
fn 任务4_apply_all_悬空的键不算指得到() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    shared.overrides().slot(&k1).set_disabled(true);
    // 換一份不再有這臺機器的設定——k1 現在懸空：設過覆蓋，`retain_after_swap`
    // 把它留著，但當前執行環境的 `override_keys()` 已經不認它。
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
    // ⚠ 而且沒有被動過——它應當仍然是 disabled。
    assert!(shared.overrides().get(&k1).unwrap().is_disabled());
}

/// ★ ★ ★ 直接重現評審點名的 TOCTOU 現場（不靠真的並行）：拿一份**舊的** `live`
/// 快照（那一刻這個鍵確實是活的），隨後讓它在登記處裡被真實收走（一次成功的
/// swap，且這一格從沒設過覆蓋 ⇒ `retain_after_swap` 會把它真的清掉——不是
/// 懸空，懸空要求「設過覆蓋」），再拿這份**過期**的 `live` 去調 `apply_all`。
///
/// 如果實作只信呼叫方傳來的 `live`、不在持鎖期間回頭再看登記處一眼，這裡就會
/// 「查」到（因為 `live` 說它在）卻在「改」的時候悄悄改不到東西——一個只檢查
/// `live` 而在改的時候對 `None` 靜默 `continue` 的實作會讓這條測試看到 `Ok(())`
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

/// 指不到時不留垃圾格子：`apply_all` 唯讀 `grids`，從不呼叫會現建格子的
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

/// ★ ★ ★ 修復輪 1，評審 M4：`apply_all` 自己**不信任呼叫方**——
/// 越界的 `SetWeight` 必須被拒絕，⛔ 不許靜默吞掉之後仍然回 `Ok`。
///
/// `RuntimeOp` 與 `apply_all` 都是 `pub`，`admin.rs` 的解析階段那道值域預檢
/// 不是唯一防線：這裡直接繞過 `admin.rs`，建構一個 `SetWeight(0)`（越界，
/// 裁決 R3：`0` 不是合法權重）直接調 `apply_all`，斷言它必須 `Err`，
/// 而且**一格都沒改**（`weight()` 仍然是 `None`）。
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

// ── 任務 5：`OverrideLayer::clear_all`（`overrides=clear`，G120）──────────────

/// ★ ★ ★ 判據 9（brief §5）：`clear_all` 之後，登記處按鍵取到的格子與
/// **仍在服務的那個 `Upstream`** 身上那一格必須**還是同一個物件**
/// （`Arc::ptr_eq`）。⛔ 不許比值相等——清空之後兩邊都是預設值，值相等
/// **恆真**，那是一把在好壞兩種情況下讀數相同的尺。
///
/// 反證時把 `clear_all` 誤寫成「記錄 + `grids.retain(...)` 物理摘掉」的形狀，
/// 這條測試必須紅：`逐个比对象身份` 會在 `layer.get(&key)` 那一步直接
/// panic——登記處已經查不到這個鍵了。
#[test]
fn 任务5_clear_all之后格子身份不变() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let k2 = OverrideKey::new("a.com", "", "10.0.0.2:2");
    shared.overrides().slot(&k1).set_disabled(true);
    shared.overrides().slot(&k2).set_weight(9).unwrap();

    let live = shared.current().override_keys();
    let cleared = shared.overrides().clear_all(&live);
    assert_eq!(cleared.len(), 2, "两格都设过覆盖，应该都在被清掉的清单里");

    // ★ ★ ★ 身分判據：`逐个比对象身份` 逐一比對「登記處按鍵取到的格子」與
    //   「Upstream 身上那一格」，clear 之後這條必須仍然成立。
    let n = 逐个比对象身份(&shared.current(), shared.overrides());
    assert_eq!(n, 2, "两个上游都应该还能比对上——身份没断");

    // 反面確認：確實清空了（否則「身分不變」只是因為根本沒清）。
    assert!(!shared.overrides().get(&k1).unwrap().is_disabled());
    assert_eq!(shared.overrides().get(&k2).unwrap().weight(), None);
    assert_eq!(
        shared.overrides().active_count(),
        0,
        "清完之后没有生效中的覆盖"
    );
}

/// 判據 10（brief §5）：`clear_all` 之後再設一次覆蓋，**資料面當場生效**——
/// 判據 9（身分沒斷）的行為側對照，從 `Upstream::pick_index_by` 這個真正的
/// 排程讀數上看。
#[test]
fn 任务5_clear_all之后再设一次覆盖数据面当场生效() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    shared.overrides().slot(&k1).set_disabled(true);

    let rt = shared.current();
    let t = rt
        .keyed_proxies()
        .into_iter()
        .find(|p| p.site == "a.com")
        .unwrap()
        .target;
    assert_eq!(t.pick_index_by(None), None, "clear 之前应该已经摘掉了");

    let live = rt.override_keys();
    shared.overrides().clear_all(&live);
    assert_eq!(t.pick_index_by(None), Some(0), "clear 之后应该恢复可用");

    // ★ 判據 10：clear 之後再設一次覆蓋，資料面當場生效——身分沒斷的
    //   可觀察後果。如果 `clear_all` 把這一格的身分弄斷了（比如物理刪格子
    //   之後 `slot()` 現建了一格新的），這裡改的是一個孤兒，下面的斷言會紅。
    shared.overrides().slot(&k1).set_disabled(true);
    assert_eq!(
        t.pick_index_by(None),
        None,
        "clear 之后再设一次覆盖，应该当场又生效"
    );
}

/// `clear_all` 的回傳值：只含清空前**確實帶著覆蓋**的那些格子，懸空的也在
/// 內且標好 `dangling`；沒設過覆蓋的格子不出現在清單裡，也不被碰。
#[test]
fn 任务5_clear_all返回值只含设过覆盖的格子_悬空的也标好() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let k1 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let k2 = OverrideKey::new("a.com", "", "10.0.0.2:2");
    // k1：設過覆蓋，且當前執行環境還認得（不懸空）。
    shared.overrides().slot(&k1).set_disabled(true);
    // k2：**沒**設過覆蓋——不該出現在回傳值裡，也不該被 clear_all 碰過。
    let k2_slot_before = shared.overrides().slot(&k2);

    // 懸空鍵：登記處裡有，但不是這份執行環境的任何上游（⚠ 判據寫法紀律：
    // 用 TEST-NET-3，不會撞上任何範本字面值）。
    let dangling_key = OverrideKey::new("a.com", "", "203.0.113.77:9999");
    shared
        .overrides()
        .slot(&dangling_key)
        .set_weight(3)
        .unwrap();

    let live = shared.current().override_keys();
    assert!(
        !live.contains(&dangling_key),
        "夹具前提：这个键本来就不在任何上游上"
    );

    let cleared = shared.overrides().clear_all(&live);
    assert_eq!(cleared.len(), 2, "只有 k1 与悬空键设过覆盖：{cleared:?}");

    let k1_entry = cleared
        .iter()
        .find(|e| e.key == k1)
        .expect("k1 应该在被清掉的清单里");
    assert!(k1_entry.disabled, "k1 清空前是 disabled");
    assert!(!k1_entry.dangling, "k1 当时还管得到那个上游");

    let dangling_entry = cleared
        .iter()
        .find(|e| e.key == dangling_key)
        .expect("悬空键也该在被清掉的清单里——clear 不分悬空不悬空，全清");
    assert_eq!(dangling_entry.weight, Some(3), "清空前的权重要如实报出来");
    assert!(dangling_entry.dangling, "这个键当时确实悬空");

    // k2 沒設過覆蓋：不在回傳值裡，也沒被碰過（還是同一個空格子）。
    assert!(
        !cleared.iter().any(|e| e.key == k2),
        "k2 没设过覆盖，不该出现在清单里"
    );
    assert!(
        !k2_slot_before.has_override(),
        "k2 全程都不该变成「有覆盖」"
    );
}

/// 邊界情形：登記處裡一項覆蓋都沒有時，`clear_all` 回傳空列表，也不留痕跡
/// （格子數不變、`active_count` 仍是 0）。
#[test]
fn 任务5_clear_all没有覆盖时返回空列表且不留痕迹() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let before = shared.overrides().slot_count();
    let live = shared.current().override_keys();
    let cleared = shared.overrides().clear_all(&live);
    assert!(
        cleared.is_empty(),
        "没有任何覆盖，清单应该是空的：{cleared:?}"
    );
    assert_eq!(
        shared.overrides().slot_count(),
        before,
        "不该多出或少掉格子"
    );
    assert_eq!(shared.overrides().active_count(), 0);
}

// ── R6 留下的那條：每項覆蓋帶**設定時間**（owner 拍板：取「最後一次改動」）────
//
// ★ ★ ★ 語義是 owner 拍的：這一格記的是**它現在這個形態是什麼時候定下來的**，
//   ⛔ 不是「這一格第一次有覆蓋是什麼時候」。維運問的那句話是「這臺機器摘掉多久了」，
//   而那句話問的是**當前形態**的年齡，不是這一格的年齡。
// ⚠ 取**牆鍾**（`SystemTime`）而不是 `Instant`：這個值是給人看的、要能與紀錄對時間。
//   代價寫在明處：牆鍾會跳（NTP 校正、手動改時間）⇒ 極端情況下兩項覆蓋的先後
//   與它們的時間戳順序可以不一致。⛔ 不為此引入第二個單調時鐘 —— 那會讓 `/stats`
//   出現兩種時間，而「這兩個時間為什麼不一樣」沒人答得出來。
// ⚠ ⚠ 存的是 `SystemTime` 而**不是秒級整數**，與 `SharedRuntime::loaded_at` 同一條理由：
//   秒級粒度在一次跑得很快的測試裡會落在同一秒，那會讓下面兩條判據偶發抖動。
//   落到 `/stats` 上再由 `metrics::unix_secs` 換成絕對 Unix 秒（那個換算只有一份）。

/// ★ 設一項覆蓋 ⇒ 它的設定時間落在「動手之前 .. 現在」這個閉區間裡。
///
/// ⚠ ⛔ **不斷言等於某個具體值**：那要求測試與被測物件共用時鐘且零延遲。
#[test]
fn r6_覆盖带着设置时间且落在合理区间内() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let 之前 = std::time::SystemTime::now();
    shared
        .overrides()
        .slot(&OverrideKey::new("a.com", "", "10.0.0.1:1"))
        .set_disabled(true);
    let 之后 = std::time::SystemTime::now();
    let es = shared.override_entries();
    assert_eq!(es.len(), 1);
    assert!(
        es[0].set_at >= 之前 && es[0].set_at <= 之后,
        "设置时间 {:?} 不在 [{:?}, {:?}] 里",
        es[0].set_at,
        之前,
        之后
    );
}

/// ★ ★ **再改一次會把它推進** —— 這一條把「最後一次改動」這個語義釘死。
///
/// ⇒ 一個取「首次被覆蓋」的實作會**在這裡紅**，而它在上面那條上是綠的。
#[test]
fn r6_再改一次会把设置时间推进() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k = OverrideKey::new("a.com", "", "10.0.0.1:1");
    shared.overrides().slot(&k).set_weight(7).unwrap();
    let 第一次 = shared.override_entries()[0].set_at;
    std::thread::sleep(std::time::Duration::from_millis(5));
    shared.overrides().slot(&k).set_disabled(true);
    let 第二次 = shared.override_entries()[0].set_at;
    assert!(
        第二次 > 第一次,
        "形态改了而时间没往前走：第一次 {第一次:?}、第二次 {第二次:?}"
    );
}

/// ★ ★ **動別的格子不許推進本格的時間。**
///
/// ⚠ 少了這一條，一個「每次取清單就把所有時間刷成現在」的實作在上面兩條上
/// **都是綠的** —— 而它給出的讀數對每一格都是錯的。
#[test]
fn r6_动别的格子不会推进本格的时间() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置(
            "a.com {\n  reverse_proxy 10.0.0.1:1 10.0.0.2:2\n}\n",
        ))
        .unwrap(),
    ));
    let 甲 = OverrideKey::new("a.com", "", "10.0.0.1:1");
    let 乙 = OverrideKey::new("a.com", "", "10.0.0.2:2");
    shared.overrides().slot(&甲).set_disabled(true);
    let 甲时间 = shared
        .override_entries()
        .into_iter()
        .find(|e| e.key == 甲)
        .expect("甲应该在清单里")
        .set_at;
    std::thread::sleep(std::time::Duration::from_millis(5));
    shared.overrides().slot(&乙).set_disabled(true);
    let es = shared.override_entries();
    let 甲现在 = es.iter().find(|e| e.key == 甲).unwrap().set_at;
    let 乙现在 = es.iter().find(|e| e.key == 乙).unwrap().set_at;
    assert_eq!(甲现在, 甲时间, "动了乙，甲的时间被一起刷新了");
    assert!(乙现在 > 甲时间, "乙是刚设的，它的时间该比甲新");
}

/// ★ **撤銷覆蓋再設一次，讀到的是新時間**（`clear_weight` / `enable` 也算改動）。
///
/// ⚠ 它守的是一個具體現場：一格被 `disable`、`enable`、再 `disable`，
/// 維運問「摘掉多久了」時該讀到**最後那一次**，而不是第一次那個老時間。
#[test]
fn r6_撤销之后再设一次读到的是新时间() {
    let shared = SharedRuntime::new(Arc::new(
        Runtime::build(&配置("a.com {\n  reverse_proxy 10.0.0.1:1\n}\n")).unwrap(),
    ));
    let k = OverrideKey::new("a.com", "", "10.0.0.1:1");
    shared.overrides().slot(&k).set_disabled(true);
    let 第一次 = shared.override_entries()[0].set_at;
    // 放回去 ⇒ 這一格不再是一項覆蓋，清單裡就沒有它了。
    shared.overrides().slot(&k).set_disabled(false);
    assert!(
        shared.override_entries().is_empty(),
        "放回去之后它不该还算一项覆盖"
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    shared.overrides().slot(&k).set_disabled(true);
    let 第二次 = shared.override_entries()[0].set_at;
    assert!(第二次 > 第一次, "再摘一次读到的还是那个老时间");
}
