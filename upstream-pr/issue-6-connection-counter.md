# 投稿六（**材料准备中，⛔ 未发**）· 监听器上的连接计数接缝

> 对应 fork **改动 15**（[`../vendor/pingora/FORK.md`](../vendor/pingora/FORK.md)）。
> 依据 `PLAN.md` §10 **G122**：「投不投上游**等 rebase 读过上游 `main` 之后再判**」。
> ⛔ **本轮只备材料、只做 G32/G46 那三项必查，不发 issue、不发 PR**（发要 owner 授权，G40）。

---

## 0. ⚠ ⚠ 先说两件把这份材料的形状改掉的事（2026-09-03 实测）

### ① ★★★ G122 里那句「上游 `main` 已把 `prometheus` 整条删掉」，在今天的 `main` 上**不成立**

实测（克隆 `https://github.com/cloudflare/pingora.git`，`main` = **`09696b5`**，2026-08-24）：

| 查什么 | 实测 |
|---|---|
| `pingora-prometheus/src/lib.rs` | **在**，131 行 |
| 谁依赖它 | `pingora/Cargo.toml:48` 与 `pingora-proxy/Cargo.toml:53`（`pingora-prometheus = "0.8.0"`），外加 `prometheus = "0.14"` |

⚠ ⚠ **这不是「G122 错了」，而是「G122 的一条前提今天需要 owner 重新看一眼」**：
那句话是**投不投上游**这个判断的**全部理由**（「口味未知」）。前提变了，判断的依据就变了。
⛔ **我没有替它改结论** —— 投不投仍然是 owner 的决定。
★ 顺带：本仓库那条老纪律在这里第 N 次兑现 ——
**一条待办躺着的理由，常常不是它难，而是它被绑在一个没人回头核的前提上。**

### ② ★★★ 上游**已经有**一个位置几乎相同的接缝：`ConnectionFilter`

`pingora-core/src/listeners/connection_filter.rs`（feature `connection_filter`，
`pingora-core/Cargo.toml:122` 写着 `connection_filter = []` ⇒ **默认关**）：

```rust
pub trait ConnectionFilter: Debug + Send + Sync {
    /// This method is called after a TCP connection is accepted but before
    /// any further processing (including TLS handshake).
    async fn should_accept(&self, addr: Option<&SocketAddr>) -> bool;
}
```

调用点在 `pingora-core/src/listeners/l4.rs:472–486`，**每条连接一次**，
装配入口是 `Listeners::set_connection_filter` / `ListeningService::set_connection_filter`。

⇒ **那正是我们 fork 改动 15 里 `+1` 的那一刻**。

#### ⚠ 但它只覆盖一半，而缺的那一半正是最贵的

| 我们要的 | `ConnectionFilter` 给不给得了 |
|---|---|
| `+1`（接进来一条）| ✅ 位置逐字相同 |
| **`-1`（连接结束）** | ❌ **没有对应的钩子** —— 而 `_active` 那一格的减一**必须**由 `Drop` 守卫做（G122：那个连接任务有三条退出路径，手写三处 `fetch_sub` 正是 D18/G66 那个分家形状）|
| **哪个监听地址**（`listen` 标签）| ❌ 只给 peer addr；而一个 `Listeners` 可以有多个监听地址（`conn_stats.rs` 顶部那条注释正是为此写的）|

⇒ 立论的形状因此**不是**「上游缺一个接缝」，而是
**「上游有半个，而另外半个（结束时机 + 监听地址）恰好是做连接计数必须的那半」**。
★ 这比原来那份「我们加了一个新 trait」的说法**更有说服力**，也更容易被接受 ——
它变成了「把已有的 `ConnectionFilter` 补完」而不是「再开一个平行的扩展点」。

---

## 1. ⏳ 因此这份材料**现在停在这里**，等 owner 拍三件事

| | 要拍的 | 为什么不能由我代拍 |
|---|---|---|
| ★★★ | **投不投**（G122 那句前提已经不成立，见 §0①）| 那是 §10 的决定 |
| ★★ | **投什么形状**：① 原样投 `ConnectionCounter`（新接缝）② 改成「给 `ConnectionFilter` 补上结束钩子与监听地址」③ 只报 issue 不带 PR | 立论完全不同，写法也完全不同 |
| ★ | **要不要顺带提 feature 默认关**：`connection_filter` 默认关 ⇒ 想用的人要显式开 feature。若我们的形状挂在它上面，这一点必须在 issue 里说清 | 涉及要不要建议上游改默认，那是口味 |

## 2. ⛔ 为什么这一轮**没有**生成补丁

按本目录 [`README.md`](README.md) 的体例，`.patch` 必须是**在上游 `main` 上回放验证过**的
`git format-patch` 输出，并逐项跑过上游 `.github/workflows/build.yml` 里那几道门。

⚠ 而今天：

1. **基线动了** —— 前四份投稿全基于 `0046038`（2026-08-07），今天 `main` 是 `09696b5`（08-24）。
   ★ 投稿四那次就因为基线/格式问题**重新生成过一遍**，教训记在 README 里。
2. **形状未定**（§1 第二行）—— 在「新接缝」与「补完 `ConnectionFilter`」之间没拍板之前，
   生成出来的补丁多半是要作废的那一份。
3. ★ 本目录的纪律是 **「先发 issue，等回应，再发 PR」**，而 issue 的正文取决于 §1。

⇒ ★ **先把「查过了什么」钉下来（本文件），补丁等形状定了再生成** ——
⛔ 一份基线过期、形状可能作废的 `.patch` 摆在这里，比没有更糟：
下一个人会以为它是可用的。

## 3. ✅ G32/G46 要求的「先查上游做没做」——已查的部分

> ⚠ G46 修订过口径：**「查过上游做没做」必须包含未合并的 issue 与 PR**，⛔ 不只是查代码。

| 查什么 | 实测（2026-09-03） |
|---|---|
| 上游有没有同名接缝 | **有半个**：`ConnectionFilter`（见 §0②）；⛔ **没有**任何 `ConnectionCounter` / 连接结束钩子 |
| 上游有没有别处在数连接 | `pingora-core/src/listeners/` 与 `services/` 里 grep `accepted_?conn` / `conn_count` / `active_conn` —— **零命中** |
| `prometheus` 还在不在 | **在**（§0①）|
| ⏳ **有没有人已经开过 issue / PR** | **本轮未查** —— 它要搜 GitHub 的 issue 与 PR（含 open 的），而这一步 [`README.md`](README.md) 记着「投稿三」就是在这里被推翻过一次。⛔ **发之前必须补上，不许跳过。** |

★ 最后一行是**有意留空而不是留白**：本轮的范围是「备材料」，而这一项的答案会直接改写 §1 第二行。

## 4. 立论骨架（形状定了之后按它写 issue）

**症状**：一个用 pingora 做入口的进程，**说不出自己有多少条连接**。
`ConnectionFilter` 能数「接进来过几条」，但数不出「此刻还有几条」——
后者要一个「连接结束」的时机，而上游今天没有任何地方给得出它。

**为什么它不该由使用者自己在外面数**：连接任务有三条退出路径（握手超时 / 握手失败 /
正常结束）。在外面数就要在三处各写一次减一，⚠ 而那正是「两处各算一遍迟早分家」的形状 ——
分家的表现是**一个恒不归零的 gauge**，看起来像连接泄漏，而其实是计数泄漏。

**我们的落法**（fork 改动 15）：一个 `+1` 的接缝 + 一个 `Drop` 守卫做 `-1`，
⛔ **减一只有守卫那一个调用点**。那段代码不认识 counter/gauge、不认识标签、
**不知道 Prometheus 存在** —— ★ 这一点要写进 issue：它不是在给上游塞一个指标体系。

**代价与边界**：⚠ 上游若接受，`connection_filter` 那个 feature 的名字与默认值要一起想 ——
「过滤」与「计数」是两件事，挂在同一个 feature 上会让只想计数的人被迫编进过滤逻辑。
