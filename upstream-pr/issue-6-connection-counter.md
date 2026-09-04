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
| ★★ | **投什么形状**：① 原样投 `ConnectionCounter`（新接缝）② 改成「给 `ConnectionFilter` 补上结束钩子与监听地址」③ 只报 issue 不带 PR。⚠ **§3.1 把这一行的天平压向 ②**：上游 2026-08-25 刚以「#671 已经加了连接级过滤器」为由把 #118 判成 `COMPLETED` ⇒ 形状 ① 会正面撞上那句话 | 立论完全不同，写法也完全不同 |
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
| ✅ **有没有人已经开过 issue / PR** | **已查（2026-09-04）** —— 结果与方法在下面 §3.1。★ 它确实改写了 §1 第二行。|

### 3.1 ✅ G46 的那一项：查过了（2026-09-04）

**方法（写下来是为了可复现，⛔ 不是流水账）**：`gh search issues` 与 `gh search prs`，
20 个查询串 × **两条通道** → 34 条去重结果。

⚠ ⚠ **两条通道是承重的，不是稳妥起见**：**`gh search issues` 一条 PR 都不返回。**
★ 用正对照测的，⛔ 不是推断：拿 PR **#962 的原题**（`Bump lru dependency from`）去搜，
`issues` 通道 **0 条**、`prs` 通道 **1 条**（就是它）。
⇒ 只跑 `issues` 的一轮扫描**会答出「没有前人做过」，而它一个 PR 都没看过**，
偏偏 G46 要的正是「含未合并的 PR」。
★ 另一条前提也自证过：`--state` 只收 `open|closed`，**省略它才是全状态**——
自证用 `lru`：issues 10 条中 5 条已关闭、prs 9 条中 7 条已关闭。

| 命中 | 状态 | 是什么 |
|---|---|---|
| ★★★ [#118](https://github.com/cloudflare/pingora/issues/118) | **closed，`COMPLETED`，2026-08-25** | 「`"on connect"` phase for incoming connections」—— **逐字就是本接缝**。★★★ 维护者的结案留言是一句话：**「A connection level filter was added with #671.」** |
| ★★ [#295](https://github.com/cloudflare/pingora/issues/295) | closed（**被 stale 机器人关的**，⛔ 不是被解决）| 「想知道此刻有多少 **active** 连接：新连接 +1、结束 −1」。维护者给的是**用户态 `Drop` 守卫**的写法，报告者最后一句是「still relevant」|
| ★★ [#337](https://github.com/cloudflare/pingora/issues/337) | closed | 「怎么跟踪客户端 connect/disconnect（尤其 websocket）」——「`connected_to_upstream` 有，**找不到 disconnect**」|
| ★ [#671](https://github.com/cloudflare/pingora/pull/671) | closed（改动已在上游）| `ConnectionFilter` trait 本体，即 §0② 那半个接缝 |
| ★★ [#941](https://github.com/cloudflare/pingora/pull/941) | **open · 0 条评论 · 2026-07-26** | `add_endpoint_with_filter`：把过滤器挂到**单个监听地址**上 —— 正是「哪个监听地址」那一半 |
| ⚠ [#897](https://github.com/cloudflare/pingora/pull/897) | open | 标题写着「resolve #295」，⛔ **看着相关其实是纯文档**（只改 `docs/user_guide/index.md`），不是竞争实现 |

**这一轮改变了什么（三条，⛔ 都不由我拍）**

1. ⛔ **G46 不构成否决**：没有任何 open 的 issue/PR 在做「监听器上的连接计数」本身。
   ★ 与投稿五**不同** —— 那次桌上摆着 [#632](https://github.com/cloudflare/pingora/pull/632)
   这样逐点相同、而且更完整的实现，所以 owner 拍了「什么都不做」。这次没有那种东西。
2. ★★★ **但立论必须改写**：上游在 **2026-08-25**（十天前）刚把 #118 以 `COMPLETED` 关掉，
   理由就是「#671 加了连接级过滤器」。⇒ 再开一份，等于对着维护者说「你们认为已经解决的
   这件事其实没解决」。**那句话必须精确到 `should_accept` 给不出的那两样 ——
   「连接结束」与「哪个监听地址」**，⛔ 泛泛说「缺一个连接级接缝」会被正当地当成噪音。
3. ★★ **需求侧的证据变强了，而它此前不在这份材料里**：三个互不相干的使用者在两年里
   各自要过同一样东西（#118 / #295 / #337），**三条都没拿到实现** ——
   一条被 stale 机器人关掉、一条被答「没有 disconnect」、一条被判成已由 #671 解决。
   ★ 这是 issue 正文里最有力的一段。

⚠ **#941 会影响形状**：per-address 那一半若被合并，我们就只剩「连接结束」要提；
而它今天 open、0 评论、挂了六周 —— ⛔ **别假设它会落地**，也别假设它不会。

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
