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

#### ①a ★★★ 重判材料（owner 2026-09-04 点名要「重新看一眼」）：**那句话是一次重构被读成了删除**

`pingora-prometheus` 这条路径上**只有一个提交** ——
[`842ddd9`](https://github.com/cloudflare/pingora/commit/842ddd9)（2026-04-01）
**"Split out pingora-prometheus into a separate crate"**。它**从来没被删过，它是那天被拆出来新建的**。

那一笔的文件清单（`gh api repos/.../commits/842ddd9`）：

| 动作 | 文件 |
|---|---|
| **removed** | `pingora-core/src/apps/prometheus_http_app.rs`（−66）|
| modified | `pingora-core/src/services/listening.rs`（**−16**）· `pingora-core/Cargo.toml`（−2）· `apps/mod.rs`（−2）|
| **added** | `pingora-prometheus/Cargo.toml`（+22）· `pingora-prometheus/src/lib.rs`（+131）|

⇒ ★★★ **只看 `pingora-core` 的话，prometheus 确实「整条消失」了** —— 那句前提几乎必定是这么写下的。
而在 workspace 层面它是**被搬走**：`pingora-prometheus` 是 workspace 成员，
`pingora` 与 `pingora-proxy` 今天都依赖它，而 `pingora-core` 对它**零引用**。
⚠ 那 −16 行还正好在 `services/listening.rs` —— **fork 改动 15 动的就是这个文件**。

**⇒ 「口味未知」这个推断今天有了精确的答案，而且方向对我们有利**

那次拆分把上游的口味说得很清楚：**core 只暴露公开接缝，指标实现住在一个消费它的独立 crate 里。**
`pingora-prometheus/src/lib.rs` 整个建在 core 的公开 API 上
（`apps::http_app::{HttpServer, ServeHttp}` · `modules::http::compression` ·
`protocols::http::ServerSession` · `services::listening::Service`）。
★ 而形状 ② 向 core 要的正是**一个钩子**，counter/gauge、标签、命名全留在调用方
（草稿里那句 "Nothing in what we are describing knows about metrics" 就是这个意思）——
**我们站在那条线的同一侧。**

**另外三条不依赖 prometheus 的口味证据**（全部当天实测）：

| | 证据 |
|---|---|
| ★★ | `ConnectionFilter`（#671）本身就是**外部贡献者**加进 core 的监听器级接缝，走 opt-in feature |
| ★★ | `upstreams::peer::Tracing { on_connected, on_disconnected }` **已经在 core 里** ⇒ 连接生命周期钩子在 core 里是既有形态 |
| ★★★ | **`listeners/` 2026 年一直在动，而且加的正是「接缝」与「按监听器配置」**：`600c5c0` pre-TLS 回调（04-03）· `c0845a8` per-listener L4 缓冲配置（05-05）· `79771f5` rustls 的 TlsAcceptCallbacks（06-12）· `8aeef34` 下行 TLS 握手卸载（06-30）· `f82478a` 关掉未被认领的继承 socket（07-23）|

★★★ **最硬的一条：我们自己的投稿一已经落进 `main`** ——
[`6463ad6`](https://github.com/cloudflare/pingora/commit/6463ad6)（2026-08-14），
author `Shanire <shanire86@gmail.com>`、committer 是维护者、我们的 `Signed-off-by` 原样保留。
⇒ 「我们投的东西会不会被重放进 main」这件事**已经有一个正例，不再是推测**。

**⚠ 诚实的另一侧（⛔ 不给单边材料）**

1. ⚠ 那次拆分是上游在**让 core 变小**，而我们要往 core 的一个 trait 上**加两样**。
   ★ 反驳是：搬走的是一个 **Prometheus 专用的 HTTP app**，不是接缝 —— 那一刀切在
   「实现」与「接缝」之间，而我们要的是接缝。但这个读法是真实存在的，⛔ 别装作没有。
2. ⚠ ⚠ **真正的风险不是口味，是评审带宽**：#941（per-address 过滤器）open 六周、**0 条评论**；
   #295 是被 stale 机器人关的；CONTRIBUTING 明说不承诺及时评审。
   ★ 而**我们已发的三份里只有一份落地**：投稿二（fd 泄漏）与投稿三（aws-lc-rs）的主题
   在 `main` 的提交里**零命中**（⚠ 带正对照：同一条检索能搜到已落地的那笔 lru）。
   ⇒ 发出去的预期结果是「**它可能就躺着**」。那是时间成本问题，不是口味问题。

**⇒ 要 owner 拍的两件**（⛔ §10 只有 owner 能改，我一个字没动）

1. **那句前提本身**：它作为陈述是**假的**。三条路 ——
   (a) 划掉那句、换成今天的证据说得出的话；
   (b) 划掉那句，「投不投」改由别的依据来判；
   (c) 行不动，把纠正写在别处 —— ⚠ 那会让 §10 留着一句假话，
   **而那正是 G128 / G129 两次补记要治的毛病。**
2. **投不投** —— 那句前提造出来的那道闸门已经不存在了。

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
| ✅ | ~~**投什么形状**~~ —— **owner 2026-09-04 拍板：形状 ②**（给 `ConnectionFilter` 补上结束钩子与监听地址）。正文草稿在 §5 | 已决 |
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
| ★★★ [#337](https://github.com/cloudflare/pingora/issues/337) | closed，**`COMPLETED`** | 「怎么跟踪**客户端** connect/disconnect（尤其 websocket）」。★★★ 维护者的回话是 **「Disconnect is tracked via the drop of the `Tracer` object」** ⇒ **上游对「怎么数连接」的既有答案就是 `Tracer`** —— 而它是 `PeerOptions` 上的字段、数的是**到后端**的连接（`peer.rs:555`，下行三个文件对它零引用）|
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
   各自要过同一样东西（#118 / #295 / #337）。
   ⚠ ⚠ **纠正我 2026-09-04 早些时候写下的一句**：当时写的是「三条都没拿到实现」——
   **那不准确**（`5c4676f` 的提交信息里留着那句错话，⛔ 绝不 amend，就地在这里改准）。
   实际是**三条都被结掉了，而给出的答案各自只覆盖问题的一部分**：
   #118 → #671（真实现，只覆盖 **accept 那一刻**）· #337 → `Tracer`（真实现，
   只覆盖**出站那一侧**）· #295 → 只有它是被 stale 机器人关的。
   ★ 这个更准的形状**对我们更有利**：不是「上游不理」，而是「每个答案都答的是相邻的问题」。

4. ★★★ **本轮第二序的发现：上游对「怎么数连接」的标准答案是 `Tracer`。**
   同一条评论（#245 的 `2129977269`）被 #295 与 #337 反复引用，而它明说是
   「when you try to connect to your **upstream** with `HttpPeer`, you can put a `Tracer`
   object into it」。⇒ ⚠ **正文若不正面处理这一点，第一条回复就会是「用 Tracer」** ——
   已在草稿的 alternatives 一节里放在**第一条**，并写明它数的是哪一群连接、
   以及哪几类下行连接它一次事件都不会产生。

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

---

## 5. 正文草稿（**形状 ②**，owner 2026-09-04 拍板）

> ⛔ **未发。** 发要 owner 按 **G40** 单独授权 —— 本节只是草稿。
> ★ 下面「正文」一节是**逐字要发出去的英文**，它上面的中文都是给我们自己看的账。

### 5.1 写之前按上游 `main` = `09696b5` 逐条核过的事实

⚠ ⛔ **一条都不是从 09-03 那份快照抄的**，全部当天重取（`gh api .../contents/...?ref=main`）。

| 事实 | 出处 |
|---|---|
| `async fn should_accept(&self, _addr: Option<&SocketAddr>) -> bool { true }` | `listeners/connection_filter.rs` |
| 调用点在 `ListenerEndpoint::accept()` 里，**而 `self.listen_addr: ServerAddress` 就在同一个结构体上**，且 `impl AsRef<str> for ServerAddress` | `listeners/l4.rs:464-494` · `l4.rs:75` |
| ★★★ **没有 socket digest、或拿不到 peer addr 时，过滤器根本不被调用**（两个 `else` 都直接 `true`）| `l4.rs:471-484` |
| `run_endpoint` 每条连接 spawn 一个任务，**三条退出路径**（握手超时 / 握手失败 / `handle_event` 返回），**没有任何结束通知** | `services/listening.rs:202-252` |
| 应用只在握手**成功**那一支才拿到连接（`Ok(io) => Self::handle_event(...)`）| `services/listening.rs:237` |
| `connection_filter` 是 **opt-in**：`default = []` | `pingora-core/Cargo.toml` |
| 上游自己已有同形状的 trait，但只在**出站**那一侧：`upstreams::peer::Tracing { on_connected, on_disconnected }` | `upstreams/peer.rs:70` ⚠ 我们 fork 里是 `:69`，**别抄 fork 的行号** |
| `Listeners::set_connection_filter` 与 `ListenerEndpointBuilder::connection_filter` 都在 | `listeners/mod.rs:439` · `l4.rs:329` |
| `Tracer` 是 `PeerOptions` 上的字段（出站侧）| `upstreams/peer.rs:555` |
| ★ 下行路径**一次都没引用**它 —— ⚠ 这是**普查**不是抽查：`listeners/` 下全部四个文件（`connection_filter.rs` · `l4.rs` · `mod.rs` · `tls/mod.rs`）加 `services/listening.rs`，`tracer\|Tracing` **全部 0 命中** | 逐文件取 `?ref=main` 后 grep |
| 那条被 #295 与 #337 反复引用的标准答案 | issue comment `2129977269`（#245 下）|

★★★ **第三行是本轮新查到的，而且它是正文里最有说服力的一条**：
`should_accept` 作为**过滤器**，「拿不到地址就放行」是稳妥的默认；
而作为**计数接缝**，那等于**一部分连接永远不进账，且没有任何东西会说**。
⚠ 更耐人寻味的是签名已经是 `Option<&SocketAddr>`（表达得出「没有地址」），
**调用点却在此之前就短路了**。

### 5.2 ⛔ 有意**没有**写进正文的三件

1. ⛔ **不提 `ConnectionCounter` 这个名字，也不提「新接缝」** —— 形状 ② 的全部要点就是
   「补完已有的」。提了就正面撞上上游 2026-08-25 那句「#671 已经加了连接级过滤器」。
2. ⛔ **一个性能数字都没有**（`PLAN.md` §8：没有可复现的端到端测量就不写）。
3. ⚠ 顺带看到 trait 文档里那段示例的签名是**旧的**（写着 `&SocketAddr`，实际是
   `Option<&SocketAddr>`）—— 是个真的小错，但**与本 issue 无关，⛔ 不夹带**：
   夹带会稀释立论，而这正是投稿一栽过的形状。

---

## 正文（GitHub issue，逐字）

**Template**: `.github/ISSUE_TEMPLATE/feature_request.md`

**Title**: `Listener connection accounting: ConnectionFilter has no end-of-connection hook and does not receive the listen address`

## What is the problem your feature solves, or the need it fulfills?

A process that uses Pingora as its entry point cannot report how many downstream
connections it currently has, broken down by the address each connection arrived on.
Today a connection *starting* can very nearly be observed; a connection *ending* cannot be
observed at all.

`ConnectionFilter::should_accept` (added by #671, behind the `connection_filter` feature)
is called after `accept()` and before the TLS handshake, which is the right moment to
observe a connection starting. Three things stand between that and being able to account
for connections:

1. **The filter is not told which listener the connection arrived on.** It receives only
   the peer address. The listener's own address is right there at the call site —
   `ListenerEndpoint` holds `listen_addr: ServerAddress`, and `ServerAddress: AsRef<str>` —
   but it is not passed. An entry point listening on several addresses cannot tell them
   apart.

2. **Nothing is called when a connection ends.** `run_endpoint` spawns one task per
   connection, and that task has three exits: handshake timeout, handshake error, and
   `handle_event` returning. None of them notifies anything, so a number built on
   `should_accept` alone can only go up.

3. **The filter is skipped entirely when the peer address is unavailable.** In
   `ListenerEndpoint::accept()`, if the stream has no socket digest, or the digest has no
   peer address, `should_accept` is not called at all and the connection is accepted by
   default. For a *filter* that is a safe default. For accounting it means some
   connections would never be counted, silently. (Interestingly `should_accept` already
   takes `Option<&SocketAddr>`, so the trait can express "no address" — the call site
   short-circuits before reaching it.)

This capability has been asked for more than once:

- #118, "on connect phase for incoming connections" — closed as completed by #671, which
  covers the accept moment only.
- #295 — asked specifically for the number of *active* connections (increment on connect,
  decrement on disconnect). The suggested answer was a userland `Drop` guard; the issue
  was closed by the stale bot, the reporter's last comment being "still relevant".
- #337 — asked how to track *client* connect/disconnect (for WebSocket); the answer was to
  use `Tracer`, and the issue was closed as completed.

Pingora already has exactly this shape, on the other side: `upstreams::peer::Tracing`
provides `on_connected` / `on_disconnected`, and `Tracer` is the answer people are pointed
at on #245, #295 and #337. But a `Tracer` is a field on `PeerOptions` and counts connections
*to upstreams*; there is no equivalent for connections *from* downstream. The alternatives
section below says why that is a different question rather than a smaller one.

## Describe the solution you'd like

Extend the existing `ConnectionFilter` seam rather than introduce a second one. It already
carries the plumbing (a per-endpoint field, a builder method, `Listeners::set_connection_filter`)
and is already gated behind `connection_filter`, which is off by default.

Two additions:

1. **Hand the listen address to the filter** — either as an argument to `should_accept`, or
   via an additional defaulted method that receives it. `ListenerEndpoint::accept()` already
   holds it.

2. **Add an end-of-connection notification**, e.g. a defaulted
   `async fn connection_closed(&self, ...)`.

On the second point, one implementation detail matters more than the API shape: **the
notification is best driven by a value whose lifetime is the spawned task, calling the hook
in `Drop`, rather than by explicit calls at each exit.** The task has three exits; an
explicit call has to be written three times, and the failure mode of missing one is silent —
a live-connection number that never comes back down, sitting next to a total that looks
perfectly healthy. A guard makes the notification a structural fact instead of a caller's
discipline.

**Backwards compatibility**: adding defaulted methods is source-compatible for existing
implementors; changing `should_accept`'s signature is not. We would suggest defaulted
additions, but either works — the feature is opt-in.

**A scope question we would rather ask than assume**: `connection_filter` is named for
*filtering*, and observing a connection's lifetime is a different concern. If you would
prefer these to live under a differently named feature, or on a sibling trait, we are happy
to shape a PR that way. What we would like to avoid is a second, parallel wiring path
alongside the one `ConnectionFilter` already has.

## Describe alternatives you've considered

- **`upstreams::peer::Tracer`** — the standing answer on #245, #295 and #337, and the one we
  expect to be offered here, so we would rather address it up front. A `Tracer` is placed in
  `PeerOptions` and its `on_connected` / `on_disconnected` fire for connections Pingora opens
  *to an upstream*, counting active plus pooled ones. That is a different population from the
  connections a listener is currently holding: a downstream connection that never opens an
  upstream connection produces no tracer events at all — one rejected by a filter, one that
  fails or times out in the TLS handshake, a request served from cache or by a local handler,
  or any L4 application that never dials out. A tracer also carries no notion of which
  listener a connection arrived on. Nothing in `listeners/` or `services/listening.rs`
  references it.
- **Counting in userland with a `Drop` guard** (also suggested on #295). It cannot cover the
  window this is about: connections that time out or fail during `io.handshake()` never reach
  the application, because `handle_event` is only called on the successful branch. For an
  entry point those are a population you specifically want to see. A userland guard also has
  no way to learn which listener a connection arrived on.
- **Polling the operating system** (`netstat`-style, also considered on #295): it is a
  sampling answer to a question about state transitions, and attributing sockets back to a
  particular listener of a particular process is awkward at best.
- **A separate observer trait alongside `ConnectionFilter`**: this duplicates wiring that
  already exists (per-endpoint field, builder method, `Listeners::set_*`), which is why we
  are proposing to extend the existing seam. See the scope question above if you disagree.
- **Carrying it in a fork** — what we do today. It works, but it is a permanent rebase cost
  for something that looks generally useful rather than specific to us, which is why we are
  asking here instead of keeping it.

## Additional context

- Related: #671 (the `ConnectionFilter` trait), #118, #295, #337. #941 (open) proposes
  attaching filters per address, which is adjacent to point 1 above.
- We maintain a fork carrying this capability and would be glad to send a PR in whatever
  shape you prefer — including one that only does point 1, if an end-of-connection hook is
  not something you want in this trait.
- Nothing in what we are describing knows about metrics: the hook would receive the listen
  address as a string, and everything else (counter versus gauge, label names, naming) stays
  in the caller.

**Pingora version**: `main` @ `09696b5`
