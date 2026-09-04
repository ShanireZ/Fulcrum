# 投稿六（✅ **issue 与 PR 都已发**）· 监听器上的连接生命周期

> ✅ **2026-09-04 经 owner 逐次授权（G40）发出**：
> issue [cloudflare/pingora#994](https://github.com/cloudflare/pingora/issues/994)。
> ★ 正文是 `scratchpad` 里那份提取产物**逐字发出**的，发后核过：
> 归一行尾后与批准的那份**逐字符相等**（9709 == 9709），中文字 0 个。
> ⚠ 落地口径：**只发 issue**。为什么现在不发 PR，见 §2（那一节的理由一条都没变，
> 只是又多了一条：**我们 fork 今天的形状与本 issue 提的形状不是同一个**）。

> 对应 fork **改动 15**（[`../vendor/pingora/FORK.md`](../vendor/pingora/FORK.md)）。
> 依据 `PLAN.md` §10 **G122**：「投不投上游**等 rebase 读过上游 `main` 之后再判**」。
> ⛔ **本轮只备材料、只做 G32/G46 那三项必查，不发 issue、不发 PR**（发要 owner 授权，G40）。

---

## ⚠ ⚠ ⚠ 读下面任何一节之前先读这一节：**发出去之后自己查出一处错，已公开更正**

**2026-09-04，#994 发出之后**，在备补丁时读上游代码，查出我们（我）在 issue 正文里
写错了一句 —— 已在 #994 下发**更正评论**
（[issue-comment-5536326108](https://github.com/cloudflare/pingora/issues/994#issuecomment-5536326108)，
owner 单独批准）。

**错的那句**：「a `Tracer` is a field on `PeerOptions` … **there is no equivalent for
connections from downstream**」，以及开头那句「a process … **cannot** report how many
downstream connections it currently has」。

**事实**（当天实查，上游 `main` = `09696b5`）：

| 事实 | 出处 |
|---|---|
| `Stream` 上有 `pub tracer: Option<Tracer>` | `protocols/l4/stream.rs:440` |
| **`Stream::drop` 就在调 `on_disconnected()`** | `protocols/l4/stream.rs:706-710` |
| 出站连接器给它赋值：`t.0.on_connected(); stream.tracer = Some(t);` | `connectors/l4.rs:221-225` |
| `Tracer` 实现了 `Clone`（`boxed_clone`）| `upstreams/peer.rs:83` |
| `ServerApp::process_new(self, **mut session: Stream**, …)` | `apps/mod.rs:51-56` |

⇒ ★★★ **机制本来就是通的，缺的只是「监听器那侧没人给它赋值」。**
而且因为 `process_new` 递的是 `Stream` 本体，**实现 `ServerApp` 的应用今天就能自己
`session.tracer = Some(t)`** 拿到按连接的 disconnect 通知 ⇒ 我那句「cannot」太强了。

**我错在哪 —— 这一条值得记住**：我做过的**普查是对的**（`listeners/` 与 `services/` 里
`tracer` 零命中，逐文件查实过）；⛔ **错的是解读** —— 我把「这两个目录里没有」读成了
「不存在对应机制」，而正确的结论是「**机制在共用的 `Stream` 类型上就有，只是只在出站那侧
接了线**」。★ 判据没坏，是我从判据跨到结论时多走了一步。

**仍然站得住的两条**（更正评论里保留的就是它们）：
① **accept→握手 那段窗口**应用看不见（`handle_event` 只在 `Ok` 那支被调）⇒ 那些连接数不到；
② 按监听器分格今天没有（留给 #941）。

⇒ ★ **更正之后要求变得小得多**：不要新方法，只要**监听器侧能挂 `Tracer`**。
补丁已按这个形状备好（§2.2）。

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

### 2.1 ★★★ 2026-09-04 补：**为什么 issue 发了而 PR 没发**（owner 问过）

四条理由，前两条是流程，后两条是硬事实：

1. **上游 CONTRIBUTING 要求先 issue**：「Non-trivial PRs will also require a GitHub issue」——
   给 core 的一个 trait 加方法不在它列的「错别字 / 小重构 / 文档」豁免里。
   本目录 [`../docs/platform/upstream-pr.md`](../docs/platform/upstream-pr.md) 把这条记成
   硬性流程第 1 条：**直接开 PR 就是违反流程**。
2. **PR 的形状正是这份 issue 请他们定的东西**：挂 `ConnectionFilter` 还是姐妹 trait ·
   feature 名要不要换 · 同步还是异步。⚠ 在这三条没回话之前写出来的补丁多半要作废 ——
   本目录有先例：投稿三在发前复审被推翻、投稿五被整个撤销。
3. ★★★ **我们 fork 今天的形状与本 issue 提的形状不是同一个。**
   fork 改动 15 是一条**独立的** `ConnectionCounter`（`enter` / `leave`，还带监听地址参数）；
   issue 提的是 `ConnectionFilter` 上**一条不带地址的** `fn connection_closed(&self)`。
   ⇒ **今天没有可发的补丁**，要发就得先把 fork 改成那个形状 —— 而那动的是 G122 拍过的设计
   与五个调用点。
4. ★★★ **而且那样改会让我们自己丢掉 `listen` 标签。** 我们的指标契约是
   `fulcrum_connections_active{listen,entrypoint}`（G122），而 issue 里那条钩子**有意不带地址**
   ⇒ 在 #941 落地之前它给不出按监听器分格的数。
   ⚠ ⚠ **推论要写在明处**：即便上游全盘接受，**fork 改动 15 也还得留着**（或留一个更薄的版本）
   直到 #941 落地 —— ⛔ 这份投稿不是「把 fork delta 删掉」的路径，它是给上游用户的贡献。

### 2.2 ✅ 2026-09-04 再补：**补丁已备好**（owner 拍板「现在就备」）

⚠ ⚠ **§2 与 §2.1 的理由被一件事推翻了一半**：更正评论把形状换成了「让监听器侧能挂
`Tracer`」（见 §6）—— 那个形状**不依赖维护者先回话**，因为它复用的是上游已有的机制，
没有新 trait、没有签名变更。⇒ 补丁可以现在就备。

**[`0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch`](0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch)**
（基于上游 `main` = `09696b5`，带 `Signed-off-by`，⛔ **未发**）。

⚠ ⛔ **它是在一个干净的上游克隆上做的，`vendor/pingora/` 一个字节都没动。**
★ 那是本目录的纪律：补丁是给上游的，不是我们 fork 的 diff 导出。

**改了什么**（只有 `pingora-core/src/listeners/mod.rs` 一个文件）：
`Listeners` 多一个可选 `Tracer` + `set_tracer()`（形状照抄它自己的 `set_pre_tls_callback`）·
`TransportStackBuilder` / `TransportStack` 各带一格 ·
`TransportStack::accept()` 里克隆一份、调 `on_connected()`、赋给 `stream.tracer`。
结束那一半由**已有的** `Stream::drop` 负责 ⇒ **天生成对**。

★★★ **为什么它能覆盖握手窗口**：`handshake()` 的两条支路都把那个具体的 L4 `Stream`
继续持有（非 TLS 直接 `Box::new(self.l4)`；TLS 是 `tls_handshake(self.l4)` 包一层）
⇒ accept 时挂上的 tracer 活过握手，握手失败/超时的连接也照样报 disconnect。

**验证（全部在容器里，⛔ Rust 不在宿主机跑；且用的是那棵树自己的卷，不碰我们的）**

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ `RC=0` |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ `RC=0` |
| `cargo test -p pingora-core --lib` | 打补丁 **557 passed / 2 failed**；**基线 555 / 2** |
| `cargo test --workspace --lib --bins --tests`（上游 CI 那条）| 两侧**各 123 条失败**，⭐ **逐项对比：失败的测试名完全相同**，差的只有汇总行的 `555 passed → 557 passed` |
| `git am` 回放 | ✅ 在 `09696b5` 上干净应用，且**回放树与开发树逐字节一致**（`git diff` 为空）|

⚠ 那两条基线失败是 `connectors::l4::tests::{test_conn_timeout, test_bind_to_port_range_on_connect}`；
那 123 条里的大头是 `pingora-proxy` 的集成测试，**要 openresty**，容器里没有 ——
⛔ 但这句话不是靠「显然无关」下的，是**回基线量出来的**。
⚠ MSRV 那道（`cargo +1.85.0 check`）与 `cargo audit` / `cargo machete` 本地没有，留给上游 CI。

### 2.3 ✅ 2026-09-04：**PR 已发** —— [cloudflare/pingora#995](https://github.com/cloudflare/pingora/pull/995)

owner 单独授权后发出（两次外部写入：先把补丁推成 `ShanireZ/pingora` 的
`listener-tracer` 分支，再 `gh pr create`）。落地核对：
base `main` · head `ShanireZ:listener-tracer` · **一个文件 `+135/-1`** ·
**一个提交 `0dda18b`** · 标题一致 · 正文 66 行**逐行相同**。
正文草稿留档在 [`pr-6-listener-tracer.md`](pr-6-listener-tracer.md)。

**发之前当天重做的核对**（⛔ 不引用上一轮）：#994 仍 OPEN 且**无维护者回复** ·
上游 `main` **没动**（仍 `09696b5`）· `set_tracer` / `listener tracer` /
`downstream tracer` / `connection tracer` 四个词**两条通道全 0**（⛔ 无人抢先）·
`fmt` 与 `clippy -D warnings` **各 `RC=0`** · `cargo test -p pingora-core --lib`
**557 / 2**，两条新测试 `ok`。

**✅ 上游 CI 四项全绿**（2026-09-04，`gh pr checks` 与后台监视两次一致）：

| job | 结果 |
|---|---|
| `pingora (1.85.0)` | **pass** 3m39s |
| `pingora (1.97.1)` | **pass** 10m58s |
| `pingora (nightly)` | **pass** 8m7s |
| `semgrep-oss` | **pass** 33s |

★★★ **它正好补上了本地验证的两处缺口**：
① `1.85.0` 那道是 **MSRV**（`cargo +1.85.0 check`），本地镜像里没有那个工具链；
② `1.97.1` 那道跑的是**带 openresty 的完整套件** —— 本地那 99 条 `pingora-proxy` 集成失败
是**环境**造成的，上游跑绿了就此坐实。
⇒ ⛔ 「留给上游 CI」那句话不再是欠账，它已经兑现。

⇒ ⏳ **下一步：等上游回话。** ⚠ CONTRIBUTING 不承诺及时评审，而 #941 挂了六周 0 评论
⇒ ⛔ **别把它当成会很快有下文的事**。
★ 判断是否落地看**改动有没有出现在 `main`**，⛔ 不看 PR 状态（本目录纪律；投稿一正是
PR 被 close 而改动进了 `main`）。

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

### 3.2 ✅ 第二轮：**普查**，不再是检索（owner 2026-09-04 要求「仔细检查……或已被 close 否决的」）

★★★ **方法从「搜」换成「枚举」** —— 因为「我的关键词覆盖到了吗」本身是个假设，
而今天早上已经栽过一次（`gh search issues` 一条 PR 都不返回）。⇒ 用 GraphQL 分页
**把全部 issue 与 PR 逐条列出来**，判断留给人读标题，不留给正则。

| 量到的（2026-09-04）| 数 |
|---|---|
| issue 总数 | **436**（178 open / 258 closed）|
| closed 的分布 | `COMPLETED` 196 · **`NOT_PLANNED` 61** · `DUPLICATE` 1 · 无 reason 0 |
| PR 总数 | **500**（115 open / 269 closed / **116 MERGED**）|

⚠ **自证**（少了它，一个提前停下的分页器看起来和跑完一样）：枚举结果里含
issue **#118** 与 PR **#962**（两类各一个已知存在的），且**最小 issue 号是 #1** ⇒ 走到底了。

**① 「有没有被 close 否决过」—— 61 条 `NOT_PLANNED` 逐条读完：⛔ 没有一条否决我们的 ask。**
★ 顺带一条对判断有用的观察：其中 **#582「Adjustable buffer sizes in L4 `Stream`」与
#285「reuse port option?」后来其实都做了**（`c0845a8` per-listener L4 缓冲配置 ·
reuse_port 现已支持）⇒ 在这个仓里 `NOT_PLANNED` 常常是「**现在不做／不按你说的做**」，
不是「永不」。

**② 宽网扫全部 936 条标题**（96 个 issue + 69 个 PR 命中，全部读过），四条新发现：

| | 发现 | 对草稿的影响 |
|---|---|---|
| ★★★ | **#560（2025-03）与 #822（2026-02）都还 open**，要求「把 `prometheus` 在 `pingora-core` 里变成可选」；`842ddd9` 用**整个搬走**回应了它们（另有 PR #612 / #708 是更早的尝试，均已关闭）| ⇒ **上游刻意不让指标实现留在 core 里**。草稿新增一段**主动引用这段历史**，说明我们要的东西在那条线的另一侧（一个钩子、零指标概念）|
| ★★★ | **#988（open，2026-09-01）+ PR #990 / #991（都 open）** —— torinnd 的「报告实际绑定的监听地址」。⚠ **与我们的第 1 点互补而非竞争**：他答「有哪些地址」，我们答「**这一条连接**从哪个进来」| 草稿的 Additional context 里点名，并把两者的差别写清 |
| ★★ | **PR #751（open 自 2025-11）「Add finish_downstream_session to ProxyHttp」**（fixes #647）—— 一个**按会话**的结束钩子，在 `pingora-proxy` 层 | 点名，并说明层次与粒度不同（我们的在 core、按连接，因此也覆盖 L4 与握手没走完的连接）|
| ★ | **PR #663「给 `Tracing` trait 加 `check_allowed`」是作者自己当天关的**（"wrong repository"）⇒ ⛔ **不是被否决** | 消除了「扩展那个 trait 曾被拒」的疑虑 |

⚠ ⚠ **顺带查出一条与本目录现有口径有出入的事，登记给 owner**：`README.md` 写着
「上游的合并方式是**批量重放**，不是合你的分支 ⇒ 『PR 被 close』在这里是成功而不是拒绝」。
而实测 **116 个 PR 处于 `MERGED` 状态**（例如 #516「add tweak_new_upstream_tcp_connection
hook」）—— ⇒ 两条路**都存在**，那句话作为唯一口径太强了。
★ 我们自己的投稿一恰好走的是前者（PR #962 closed，而改动进了 `main`）。

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

★ ★ **而第二轮查出的另一条同族的事**写进了正文，因为它**在**主题上：
`ConnectionFilter` 在上游**有两份定义**且签名不一致 ——
feature 开时是 `async fn should_accept(&self, _addr: Option<&SocketAddr>)`（`connection_filter.rs`），
关时是 `fn should_accept(&self, _addr: &std::net::SocketAddr)`（`listeners/mod.rs:58`，
而那份 stub 的文档写着 "for API compatibility"）。
⇒ 它**直接约束我们提的改法**（加默认方法要在两处加，而两处本来就不一致）⇒ 正文里一句话点到，
⛔ 并明说「只因为它影响这次改动才提，不是另开一份报告」。
★ 判据是同一条：**在主题上的、约束我们提案的 → 写；off-topic 的小错 → 不夹带。**

---

### 5.3 第三轮复查（owner 要求「再走一轮」，⛔ 换角度，不重复前两轮的查法）

前两轮查的是「事实对不对」与「有没有前人」。这一轮换成三面：**上游有没有动 · 当维护者会
怎么反驳 · 措辞与篇幅**。

| | 查了什么 | 结果 |
|---|---|---|
| ✅ | 上游 `main` 有没有移动（正文钉的是 `09696b5`）| **没动**，仍是 `09696b5`（2026-08-25）⇒ 钉的那个号今天仍然准 |
| ✅ | `connection_filter` 是不是只在 main 上（维护者会问版本）| **不是** —— 已发布的 `0.8.1` tag 上 `connection_filter.rs` 与那个 feature 都在 ⇒ 正文不需要版本告示 |
| ✅ | 那条「两份定义签名不一致」是不是 main 上的临时状态 | **不是**，`0.8.1` 上同样如此（real 收 `Option<&SocketAddr>` 且 `async`，stub 收 `&SocketAddr` 且同步）|

★★★ **这一轮真正逮到的一条（技术上要紧，前两轮都没想到）：`Drop` 不能 `await`。**
草稿原本把结束钩子写成 `async fn connection_closed(...)` —— 而**我们自己给出的落法**
（一个由任务持有、在 `Drop` 里调钩子的守卫）与 `async` 钩子**是矛盾的**：
`Drop` 里没法 await，于是要么每条关闭的连接 `spawn` 一次，要么架一座阻塞桥，两者都更差。
⇒ 已改成 `fn connection_closed(&self, listen: &str)`（同步），并在正文里把这条理由写明；
`should_accept` 照旧 `async`。
★ 这条是**我们自己踩过才知道的**，写进去比任何论证都有说服力。

另外三处按「当维护者会怎么反驳」改了：

1. 加了一段**具体的 API 草图**（三行 `impl`）—— 维护者评估的是人体工学，而先前正文只有散文；
   ⚠ 草图明写它用的是「不破坏兼容」那个变体。
2. 「the one we **expect to be offered** here」改成「the answer **given** on #245/#295/#337」
   —— ⛔ 去掉替对方揣测的口气。
3. 「one implementation detail **matters more than** the API shape」改成
   「two things we **learned the hard way** carrying this in a fork, offered for what they are
   worth **rather than as a prescription**」—— ⛔ 去掉教对方怎么写代码的口气。

⚠ 篇幅：约 130 行。判断是**值得**——它按上游模板分节、前人证据密集，而这个仓的维护者
带宽紧（#941 open 六周 0 评论）⇒ 一次把话说完比来回三轮便宜。

### 5.4 第四轮复查（owner 又要求「再 review 一遍」）：**我们的立论会不会被架空**

⛔ 前三轮查的是「事实 / 前人 / 措辞」。这一轮只问一件事：
**上游已有或在途的东西，会不会让我们的某一点变得不必要？**

★★★ **答案是会，而且是第 2 点（监听地址）。**

今天的机制（当天实查）：`Listeners::set_connection_filter` 把**同一个实例**盖到每个
endpoint 上，`add_endpoint` / `add_listener` 也是 clone 那一个 ⇒ 一个过滤器分不出监听器。
⚠ 而 `ListenerEndpointBuilder::connection_filter` **确实**收按 endpoint 的过滤器 ——
只是 `TransportStackBuilder` 是私有的、`TransportStack` 是 `pub(crate)`，
⇒ **手搓的 endpoint 喂不进 `Service`**，那条路走不通。★ 这正是 **#941** 要解决的事。

⇒ ★★★ **#941 一落地，「每个地址一个过滤器实例」天生就知道自己的地址，我们的第 2 点就没必要了。**
这是维护者**一定**会提的反驳。⇒ 三处照此改：

1. **两点重新排序**：不会被架空的那条（结束钩子）领头，监听地址退为第 2；
   全部 `point 1` / `point 2` 交叉引用同步核过（`grep -n "point 1\|point 2"`）。
2. **自己把这条说出来**：正文明写「#941 一落地，第 2 点就变得不必要，**而我们宁愿如此**」——
   ★ 这让我们的要求**变小**，也说明我们不是想再造一套机制。
3. **标题跟着改**：`…has no end-of-connection hook, and cannot tell listeners apart` ——
   「cannot tell listeners apart」正是 #941 会修掉的那句话，措辞与事实对齐。

⚠ 顺带补一条给「只读问题小节的人」的指针（那一条里加了「#941, open, would give this by
construction」），免得他停在那里被误导。

**同一条思路推到第 1 点：结束钩子会不会也已经有现成的？** —— 查了，**没有**：

| 候选 | 实查结论 |
|---|---|
| `ServerApp::cleanup` / `HttpServerApp::http_cleanup`（**#118 的报告者当年就提过它**）| ⛔ **按服务，不按连接**：`cleanup` 的文档注释自己写着 "called **once after the service stops listening to its endpoints**"，调用点在 `listening.rs:319`，**在 accept 循环退出之后**。它看不见单条连接 |
| PR #751 `finish_downstream_session` | 按**会话**、在 `pingora-proxy` 层 ⇒ 覆盖不到 L4，也覆盖不到握手没走完的连接 |
| 枚举出的全部 500 个 PR / 436 个 issue | 宽网里带 `disconnect\|lifecycle\|hook\|callback`，165 条命中全部读过 ⇒ **只有 #751 一个候选** |

⇒ ★ 第 1 点**没有被架空**，而它现在是领头的那一点 —— 形状对了。
★ `http_cleanup` 那条已像对 `Tracer` 那样**自己先答掉**（写进 alternatives），
因为 #118 的报告者提过它 ⇒ 维护者也可能提。

**再往外一层查了 workspace 里那个陌生成员 `pingora-foundations`**（描述是
「Foundations **telemetry** integration」）：读过它的 `lib.rs` —— 它是**遥测管道**
（把 `foundations` 的 logging / metrics / tracing 与 `/metrics` HTTP 服务接成一个
`BackgroundService`），⛔ **不提供任何连接级数据源** ⇒ 不架空我们。
★★★ 反而**从另一侧强化了立论**，这句已进正文：上游已经给了「往哪儿放这个数」
与「从哪儿吐出去」（`pingora-prometheus` + `pingora-foundations`），
**缺的正是 core 里报出这个数的东西**。

### 5.4b ✅ owner 拍板：**第 2 点整条删掉，只提结束钩子**（2026-09-04）

⚠ ⚠ **本节取代 §5.4 里「两点重新排序」与那个旧标题** —— §5.4 记的是那一轮查到了什么，
处置由 owner 改成了更狠的一刀：**不要第 2 点**。

★★★ 而这一刀逼出一处**必须一起改、否则自相矛盾**的地方：钩子原本是
`fn connection_closed(&self, listen: &str)` —— **那个 `listen` 参数本身就是第 2 点**。
删掉第 2 点，签名就得收成 **`fn connection_closed(&self)`**。

⇒ ★ 而这让故事更硬，不是更弱：

| | |
|---|---|
| **今天**（一个实例服务所有 endpoint）| 单这一条就给出**进程级的在线连接数** —— 而今天连这个都拿不到 |
| **#941 落地后** | 每个地址一个实例 ⇒ 按监听器分格**天生**就有，⛔ 不需要我们要任何东西 |

⇒ 最终诉求＝**一条带默认实现的同步方法**，零签名变更、零新数据过边界、
既有实现方零改动。★ 这是这份 issue 能有的最小形状。

同笔收尾（否则会留下互相矛盾的句子）：

1. **标题**改成 `ConnectionFilter has no end-of-connection hook, so downstream connections
   can be counted starting but never ending` —— 自足、不再提监听器身份；
2. 问题小节从「三件事挡在前面」收成**一件**（那条「拿不到 peer addr 就不调」降为
   一句**明说「我们不要求你改」**的告示，免得把计数吹过头）；
3. 兼容性那段删掉「改签名不兼容」那半句（我们不再提改签名）；
4. alternatives 里凡是拿「学不到监听器身份」当论据的都删掉 —— 那不再是我们的论点；
5. Additional context 把 #941 改成「**我们有意不碰**监听器身份，那正是它该来的地方」。

⚠ ⚠ ★ 全部 `point 1` / `point 2` / `listen: &str` / `connection_accepted` 的残留
**逐条 grep 核过**（⛔ 不靠通读：改写之后最容易留下的正是一句读起来通顺的旧引用）。

### 5.5 发布用的正文怎么取（⛔ 不手打）

`scratchpad/extract_issue_body.py`：从材料里按 `**Title**:` 与
`## What is the problem` 两个锚点切出标题与正文，写成两个文件，并打印 sha256。
★ **理由**：owner 批准的是材料里那份，真发出去的必须与它**逐字节相同** ——
手打重录正是两者分家的方式。

它带守卫：正文里**不许**出现我们自己的任何记号（`⛔ ★ ⚠ ⇒`、`**Title**:`、
`正文（GitHub issue` 等），四个模板小节缺一即拒。
⚠ ⚠ **守卫第一版漏了 `⚠`** —— 一份不全的守卫与没有守卫是同一个形状，已补。
★ 而「守卫通过」本身不算证据：另外**直接量过**一遍提取出来的正文 ——
四个记号各 0 次、**中文字符 0 个**。

---

## 正文（GitHub issue，逐字）

**Template**: `.github/ISSUE_TEMPLATE/feature_request.md`

**Title**: `ConnectionFilter has no end-of-connection hook, so downstream connections can be counted starting but never ending`

## What is the problem your feature solves, or the need it fulfills?

A process that uses Pingora as its entry point cannot report how many downstream connections
it currently has. A connection *starting* can be observed today; a connection *ending* cannot
be observed at all.

`ConnectionFilter::should_accept` (added by #671, behind the `connection_filter` feature) is
called after `accept()` and before the TLS handshake, which is the right moment to see a
connection start. **Nothing is called when one ends.** `run_endpoint` spawns one task per
connection, and that task has three exits — handshake timeout, handshake error, and
`handle_event` returning — and none of them notifies anything. So a number built on
`should_accept` can only ever go up.

This has been asked for more than once:

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

Worth noting from the other side: `pingora-prometheus` and `pingora-foundations` already give
a place to publish such a number and an endpoint to serve it from. What is missing is anything
in core that reports it.

One caveat we are *not* asking you to change, mentioned so a count is not oversold: in
`ListenerEndpoint::accept()`, if the stream has no socket digest or the digest has no peer
address, `should_accept` is not called at all and the connection is accepted by default. For a
filter that is a safe default; it does mean a count of starts is not exhaustive.

## Describe the solution you'd like

Extend the existing `ConnectionFilter` seam rather than introduce a second one. It already
carries the plumbing (a per-endpoint field, a builder method, `Listeners::set_connection_filter`)
and is already gated behind `connection_filter`, which is off by default.

**One addition**: a defaulted `fn connection_closed(&self)`, called once for each connection
`should_accept` was consulted about, when that connection is gone. No signature changes, no
new data crossing the boundary, and nothing for existing implementors to do.

The pairing is the part worth pinning down: as noted above, `should_accept` is skipped
entirely when there is no peer address. A close hook that fired for *every* accepted
connection would therefore not pair with it, and the obvious `+1` / `-1` implementation would
underflow. Firing it exactly where `should_accept` ran keeps the two in step.

```rust
#[async_trait]
impl ConnectionFilter for MyConnectionCount {
    async fn should_accept(&self, _addr: Option<&SocketAddr>) -> bool {
        self.live.fetch_add(1, Relaxed);
        true
    }

    // new
    fn connection_closed(&self) { self.live.fetch_sub(1, Relaxed); }
}
```

**On attributing connections to a particular listener**: we are deliberately *not* asking for
that here. Today `Listeners::set_connection_filter` installs one instance across every
endpoint (and `add_endpoint` clones that same one), so the hook above yields a process-wide
live count — which is more than is obtainable now. Per-listener attribution follows from #941
by construction, since a filter instance attached to one address already knows its address.
We would rather wait for that than ask for a second mechanism.

Two things we learned the hard way carrying this in a fork, offered for what they are worth
rather than as a prescription:

- **The end hook wants to be synchronous.** The only way we found to guarantee it fires is a
  value owned by the spawned task that calls the hook from `Drop` — and `Drop` cannot await.
  An `async` close hook therefore forces either a `spawn` per closing connection or a
  blocking bridge, both worse than a plain `fn`. `should_accept` can stay `async`.
- **Drive it from that guard, not from explicit calls at each exit.** The task has three
  exits; an explicit call has to be written three times, and the failure mode of missing one
  is silent — a live-connection number that never comes back down, sitting next to a total
  that looks perfectly healthy.

**Backwards compatibility**: a defaulted method is source-compatible for existing
implementors, and the feature is opt-in, so nothing changes for anyone who does not implement
it.

One thing worth knowing before shaping this: the trait exists twice. The real one is in
`listeners/connection_filter.rs` under the feature; `listeners/mod.rs` defines a stub for
when the feature is off, described as being there "for API compatibility". Their signatures
already differ — the stub's `should_accept` is synchronous and takes `&SocketAddr`, the real
one is `async` and takes `Option<&SocketAddr>` — so any addition here needs a decision about
the stub too. We mention it only because it shapes the change, not as a separate report.

**On what this would add to `pingora-core`**: we are aware that core deliberately no longer
carries metrics. #560 and #822 asked for `prometheus` to be made optional, and `842ddd9`
answered by moving the Prometheus HTTP app out into its own `pingora-prometheus` crate,
which now builds entirely on core's public API. What we are asking for sits on the other
side of that same line: one notification, with no metrics dependency, no counter/gauge
distinction and no label vocabulary. The counting stays in the caller, exactly as
`pingora-prometheus` now sits outside core.

**A scope question we would rather ask than assume**: `connection_filter` is named for
*filtering*, and observing when a connection ends is a different concern. If you would prefer
it to live under a differently named feature, or on a sibling trait, we are happy to shape a
PR that way. What we would like to avoid is a second, parallel wiring path alongside the one
`ConnectionFilter` already has.

## Describe alternatives you've considered

- **`upstreams::peer::Tracer`** — the answer given on #245, #295 and #337, so it is worth
  addressing up front rather than after a round trip. A `Tracer` is placed in
  `PeerOptions` and its `on_connected` / `on_disconnected` fire for connections Pingora opens
  *to an upstream*, counting active plus pooled ones. That is a different population from the
  connections a listener is currently holding: a downstream connection that never opens an
  upstream connection produces no tracer events at all — one rejected by a filter, one that
  fails or times out in the TLS handshake, a request served from cache or by a local handler,
  or any L4 application that never dials out. Nothing in `listeners/` or
  `services/listening.rs` references it.
- **`ServerApp::cleanup` / `HttpServerApp::http_cleanup`** — suggested on #118, so worth
  ruling out explicitly. These are per-service, not per-connection: the doc comment on
  `cleanup` says it is "called once after the service stops listening to its endpoints", and
  the call site in `run_endpoint` is after the accept loop has exited. They cannot see
  individual connections at all.
- **Counting in userland with a `Drop` guard** (also suggested on #295). It cannot cover the
  window this is about: connections that time out or fail during `io.handshake()` never reach
  the application, because `handle_event` is only called on the successful branch. For an
  entry point those are a population you specifically want to see.
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

- Related, on the seam itself: #671 (the `ConnectionFilter` trait), #118, #295, #337.
- Related, on listener identity, which we are deliberately leaving alone: #941 (open) attaches
  filters per address, which is how we would expect per-listener attribution to arrive; #988,
  with PR #991 reporting the addresses a service actually bound and PR #990 fixing the fd-table
  collision it needs (all open).
- Related, on end-of-something hooks: #751 (open since 2025-11) adds
  `finish_downstream_session` to `ProxyHttp`. That is a per-session hook in `pingora-proxy`;
  what is asked for above is a per-connection notification in `pingora-core`, so it also
  covers L4 applications and connections that never complete a handshake.
- We maintain a fork carrying this capability and would be glad to send the PR, in whatever
  shape you prefer.

**Pingora version**: `main` @ `09696b5`
