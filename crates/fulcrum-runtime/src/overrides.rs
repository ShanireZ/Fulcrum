//! 运行时的**临时覆盖层**（**M2 批 N 任务 3**，G18 / 裁决 R7）。
//!
//! G18 原话：**运行时改动的归宿 = 显式临时覆盖层。不持久化，但永远可见。**
//!
//! # ★ ★ ★ 它是一张「格子登记处」，不是一份「待应用的清单」
//!
//! ```text
//! OverrideLayer:  (站点名, reverse_proxy 的 id, 归一化后的上游地址) → Arc<UpstreamOverride>
//!                                                                        ▲
//!                 Upstream ─────────────────────────────────────────────┘
//!                 （每个 HTTP 上游**直接拿着**登记处里的那一格）
//! ```
//!
//! ⇒ 「覆盖层」与「上游身上那两个量」在结构上**是同一份**：
//! 不存在「改了列表忘了应用」，也不存在「应用了没记进列表」。
//!
//! ⛔ **不许改成「一份清单 + 一个应用步骤」**：那个形状的失效现场是
//! 「配置照过、命令回 200、而那台机器还在收流量」，且没有任何东西会报错。
//!
//! # ⚠ 键相同就是同一格，这是设计不是意外
//!
//! 同一个站点里两条 `reverse_proxy` 指着同一台机器而都没写 `id` 时，它们的键
//! **真的相同** ⇒ 它们从登记处取到**同一个** [`Arc`] ⇒ 一次 `disable` 两条一起生效
//! （裁决 R6 ③ 第二轮 ⇒ G125）。★ 这个语义**从本模块的设计里免费掉出来**，
//! ⛔ 别在别处补一条「键撞了要怎么办」的分支。
//!
//! # ⛔ 不持久化
//!
//! 这里的东西**只活在进程里**。G18 的死线：⛔ 不许出现第二条持久化写路径。
//! 「永远可见」由 `/stats` 与「每一次管理面响应都带覆盖层计数」承担（任务 5 / 6）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 覆盖层的键：`(站点名, reverse_proxy 的 id, 归一化后的上游地址)`（裁决 R6 ⇒ G125）。
///
/// 三格分别是 [`crate::SiteRt::name`] · [`crate::ProxyTarget::id`] · [`crate::Upstream::addr`]。
///
/// ⚠ ⚠ **三格都只能从 [`crate::Runtime::keyed_proxies`] 那条遍历上取**
/// —— ⛔ 别在别处再拼一份「差不多的」，两份拼法迟早在某一格上分家。
///
/// ⚠ 第三格是**归一化之后**那个串（`backend` → `backend:80`）：管理面对着的是**运行时**，
/// 运行时里那个上游就叫 `backend:80`。⇒ 管理面收到的地址要**先过
/// [`crate::normalize_upstream`]** 再拿来比键，否则运维照着配置写 `backend` 会被告知「找不到」。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OverrideKey {
    /// 键的**第一格**：站点名（第一条地址的原文，与访问日志的 `site` 字段逐字同口径）。
    pub site: String,
    /// 键的**第二格**：这条 `reverse_proxy` 的 id，**没写就是空串**。
    pub id: String,
    /// 键的**第三格**：**归一化之后**的上游地址。
    pub addr: String,
}

impl OverrideKey {
    pub fn new(site: &str, id: &str, addr: &str) -> OverrideKey {
        OverrideKey {
            site: site.to_string(),
            id: id.to_string(),
            addr: addr.to_string(),
        }
    }
}

impl std::fmt::Display for OverrideKey {
    /// 给报文与日志用的可读形态。⚠ 三格**原样**印出来，运维照抄即可（裁决 R6 ⑤）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.id.is_empty() {
            write!(f, "站点 {} · 上游 {}", self.site, self.addr)
        } else {
            write!(
                f,
                "站点 {} · id {} · 上游 {}",
                self.site, self.id, self.addr
            )
        }
    }
}

/// 登记处里的**一格**：一个上游身上「运维改过的那两个量」。
///
/// ★ ★ 它被 [`Arc`] 共享：登记处里那一格与 [`crate::Upstream`] 身上那一格
/// **是同一个对象**。⇒ 改这里，那个上游立刻算数；没有第二步。
///
/// ⚠ 两个量都用 `Relaxed`，与 [`crate::Upstream`] 的 `healthy` / `inflight` 同一条理由：
/// 它们是**建议性**的读数，晚一个调度周期生效不构成任何问题，
/// 而在请求热路径上加内存序只会白花钱。
#[derive(Debug, Default)]
pub struct UpstreamOverride {
    /// 运维把这个上游摘掉了吗。
    disabled: AtomicBool,
    /// 覆盖权重。★ ★ **`0` 是哨兵，意思是「没覆盖，用配置里那个」** ——
    /// 而不是「权重 0」：裁决 R3 明写 `0` 不是合法权重，
    /// 「不参与调度」**只有一种表达方式**，就是 `disable`。
    weight: AtomicU32,
}

impl UpstreamOverride {
    /// 这个上游被运维摘掉了吗。
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }

    /// 摘掉（`true`）或放回（`false`）。
    ///
    /// ⚠ 放回 = **撤销这一半覆盖**，不是「加一条 enable 覆盖」：
    /// 一格上两个量都回到默认值之后，它就不再是一项覆盖了（见 [`Self::has_override`]）。
    pub fn set_disabled(&self, v: bool) {
        self.disabled.store(v, Ordering::Relaxed);
    }

    /// 覆盖权重。`None` = **没覆盖**，调度该用配置里那个。
    pub fn weight(&self) -> Option<u32> {
        match self.weight.load(Ordering::Relaxed) {
            0 => None,
            w => Some(w),
        }
    }

    /// 设一个覆盖权重。**值域与配置权重逐字相同**（裁决 R3：`[1, 65535]`）。
    ///
    /// ★ ★ 值域检查落在**这一处**，⛔ 任务 4 的 `POST /runtime` 不许再写一份：
    /// 两份「差不多的」值域检查迟早分家，而分家的表现是
    /// 「管理面收下了一个配置层根本写不出来的权重」。
    /// ⚠ 值域常量只有一份，在 `fulcrum_config::model` 里。
    ///
    /// ⚠ `0` 在这里是**错误**而不是「清除覆盖」：清除走 [`Self::clear_weight`]。
    /// 两件事共用一个入参值，就等于让「手滑写了 0」与「我要撤销」长得一模一样。
    pub fn set_weight(&self, w: u32) -> Result<(), String> {
        use fulcrum_config::model::{MAX_UPSTREAM_WEIGHT, MIN_UPSTREAM_WEIGHT};
        if !(MIN_UPSTREAM_WEIGHT..=MAX_UPSTREAM_WEIGHT).contains(&w) {
            return Err(format!(
                "权重 {w} 不在 [{MIN_UPSTREAM_WEIGHT}, {MAX_UPSTREAM_WEIGHT}] 内\
                 （`0` 不是「不参与调度」——那要用 disable）"
            ));
        }
        self.weight.store(w, Ordering::Relaxed);
        Ok(())
    }

    /// 撤掉覆盖权重，回到配置里那个。
    pub fn clear_weight(&self) {
        self.weight.store(0, Ordering::Relaxed);
    }

    /// 这一格**现在**带着覆盖吗。
    ///
    /// ★ ★ 问的是「现在」，不是「有没有人碰过」：`disable` 之后又 `enable` 回去，
    /// 这一格就重新变回**空格子**。⇒ 这与 G18 那句「当前有几项临时覆盖生效中」
    /// 是同一个问题，而 `/stats` 的条目数、`/load` 的回话计数、
    /// `fulcrum_overrides_active` 三处都必须是**这一个**答案。
    pub fn has_override(&self) -> bool {
        self.is_disabled() || self.weight().is_some()
    }
}

/// 登记处里的一条**清单项**：一格覆盖，连同它现在还管不管得到谁（裁决 R8）。
///
/// ⚠ 清单**只列设过覆盖的那些格子**（[`UpstreamOverride::has_override`]）——
/// 空格子是惰性的，不计数、不出现在任何清单里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideEntry {
    pub key: OverrideKey,
    pub disabled: bool,
    /// 覆盖权重，`None` = 这一格没覆盖权重（只是 `disable`）。
    pub weight: Option<u32>,
    /// ★ **悬空**（裁决 R8）：这个键在**当前**运行时里落不到任何上游。
    ///
    /// ⚠ ⚠ 悬空的覆盖**不删**：与 HAProxy 那个「runtime 改动 reload 之后无声消失」
    /// 正相反 —— `keep` 就是 keep，而「它现在管不到谁」这件事**说出来**。
    /// ⚠ 悬空的**照样算「生效中」**（R13 明写）：它确实还在登记处占着一格。
    pub dangling: bool,
}

/// **格子登记处**：本进程当前的全部临时覆盖（G18 / 裁决 R7）。
///
/// 它**跨换代活着** —— 全量 load 换掉的是 [`crate::Runtime`]，不是这里。
/// ⇒ 它挂在 [`crate::SharedRuntime`] 上（那是唯一跨换代活着的东西），
/// ⛔ 别在别处再存一份。
///
/// # ⚠ 空格子的生命周期
///
/// [`crate::Runtime::build_with_overrides`] 会给这份配置里的**每一个**键都登记一格
/// （没有就现建）—— 于是绝大多数格子是**空的**（没设过任何覆盖）。它们：
///
/// - **不计数、不进清单**（[`Self::active_count`] / [`Self::entries`] 只看设过覆盖的）
///   ⇒ 一次建到一半失败的 load 不会让「有几项覆盖」这个数动一下（G8：不留痕迹）；
/// - 在一次**成功的** swap 之后被清掉（[`Self::retain_after_swap`]），登记处不无界增长。
///
/// ⚠ ⚠ **清的只是「没人引用**且**没设过覆盖」的那些**：设过覆盖而暂时没人引用的
/// 是**悬空**（R8），必须留着 —— 那正是 `overrides=keep` 的全部意义。
#[derive(Debug, Default)]
pub struct OverrideLayer {
    /// ⚠ `BTreeMap` 而不是 `HashMap`：清单是「永远可见」的东西（G18），
    /// 而每次换一个顺序的清单没法逐次比对。
    grids: std::sync::Mutex<BTreeMap<OverrideKey, Arc<UpstreamOverride>>>,
}

impl OverrideLayer {
    pub fn new() -> OverrideLayer {
        OverrideLayer::default()
    }

    /// ⚠ 锁中毒不该让转发或管理面停摆：中毒说明某个持锁线程 panic 过，
    /// 而这张表本身仍然是完整的（同 [`crate::SharedRuntime::current`] 的处置）。
    fn grids(&self) -> std::sync::MutexGuard<'_, BTreeMap<OverrideKey, Arc<UpstreamOverride>>> {
        self.grids.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 按键取那一格，**没有就现建并登记**。
    ///
    /// ★ ★ 这是 R7 的核心动作：[`crate::Runtime::build_with_overrides`] 给每个上游
    /// 挂格子走的就是它 ⇒ **键相同的上游拿到的是同一个 [`Arc`]**。
    pub fn slot(&self, key: &OverrideKey) -> Arc<UpstreamOverride> {
        let mut g = self.grids();
        if let Some(s) = g.get(key) {
            return s.clone();
        }
        let s = Arc::new(UpstreamOverride::default());
        g.insert(key.clone(), s.clone());
        s
    }

    /// 按键查那一格，**不建**。`None` = 登记处里没有这个键。
    pub fn get(&self, key: &OverrideKey) -> Option<Arc<UpstreamOverride>> {
        self.grids().get(key).cloned()
    }

    /// 把一格**已经存在的**格子收进登记处。
    ///
    /// ★ 只有一个用途：[`crate::SharedRuntime::new`] 从一份**已经建好的**运行时身上
    /// 认领它的格子 —— 那条路上 `Runtime` 已经是 `Arc`，拿不到 `&mut` 再挂一遍。
    /// ⚠ 它之所以是对的，是因为**任何一份运行时的格子都已经按键共享过**
    /// （[`crate::Runtime::build`] 自己也走 [`Self::slot`]，只是那张登记表是一次性的）
    /// ⇒ 同一个键上认领几次，认领到的都是同一个 [`Arc`]。
    pub fn adopt(&self, key: OverrideKey, slot: Arc<UpstreamOverride>) {
        self.grids().insert(key, slot);
    }

    /// **当前有几项临时覆盖生效中**（G18 原话）。⚠ 空格子不算。
    pub fn active_count(&self) -> usize {
        self.grids().values().filter(|s| s.has_override()).count()
    }

    /// 登记处里一共有几格（**含空格子**）。
    ///
    /// ⚠ ⚠ 它**不是**「有几项覆盖」—— 那是 [`Self::active_count`]。
    /// 这个数只给判据用：一条「空格子不计数」的判据，必须能证明**当时真的有空格子**，
    /// 否则它在「一格都没建」的实现下同样是绿的（那是一把好坏两种情况读数相同的尺）。
    pub fn slot_count(&self) -> usize {
        self.grids().len()
    }

    /// 覆盖清单（裁决 R8：悬空的**留着**并标出来）。
    ///
    /// ⚠ `live` 是**当前运行时**里全部的键（[`crate::Runtime::override_keys`]）。
    /// ★ 别自己去拼这份 `live`：走 [`crate::SharedRuntime::override_entries`]，
    /// 那里的 `live` 一定取自**正在服务的**那一份运行时。
    pub fn entries(&self, live: &BTreeSet<OverrideKey>) -> Vec<OverrideEntry> {
        self.grids()
            .iter()
            .filter(|(_, s)| s.has_override())
            .map(|(k, s)| OverrideEntry {
                key: k.clone(),
                disabled: s.is_disabled(),
                weight: s.weight(),
                dangling: !live.contains(k),
            })
            .collect()
    }

    /// 一次**成功的** swap 之后收拾登记处。返回清掉了几格。
    ///
    /// 清掉的是「**新运行时不引用**、**且**没设过覆盖」的空格子。
    ///
    /// ⚠ ⚠ 两个条件缺一不可：
    /// - 少了「不引用」⇒ 会把新运行时正拿着的那些空格子从登记表里摘掉，
    ///   于是管理面按键找到的将是一格**新的**格子，改它对那个上游毫无作用；
    /// - 少了「没设过覆盖」⇒ 会把**悬空覆盖**当垃圾清掉，那正是 R8 明令保留的东西。
    pub fn retain_after_swap(&self, live: &BTreeSet<OverrideKey>) -> usize {
        let mut g = self.grids();
        let before = g.len();
        g.retain(|k, s| live.contains(k) || s.has_override());
        before - g.len()
    }
}
