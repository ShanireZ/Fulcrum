---
type: 技术基线
title: HTTP/3 库选型事实表（D11）
description: 三条候选路线在「按 DCID 路由」「rustls 整合面」「维护活跃度」上的实测事实；★ quiche 用 BoringSSL 而不是 rustls，s2n-quic 根本不带 HTTP/3。
resource: ../../PLAN.md
tags: [依赖, 待定, 必读, 易错]
status: draft
generated:
  by: claude-code/opus-5
  at: 2026-08-25T00:00:00Z
sources:
  - id: plan-10-d11
    resource: /references/plan.md
    title: PLAN.md §10 G103（D11 结案：HTTP/3 取 quiche）与它的两条前置
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 G6 三条硬约束（rustls 锁死 / tower 用不上 / QUIC 监听器必须参与 socket 移交）
  - id: open-seams
    resource: /verification/open-seams.md
    title: 升级窗口内的 QUIC 连接归属；`ResolvesServerCert` 能否同时服务两个入口
  - id: crates-io
    resource: https://crates.io/api/v1/crates
    title: 各 crate 在 crates.io 上**自己声明**的版本、依赖与 feature 表
  - id: docs-rs
    resource: https://docs.rs
    title: quiche / quinn-proto / s2n-quic 的公开 API 文档
---

# 这份表是干什么的

✅ ★★★ **D11 2026-08-25 由 G103–G105 结案：取 `quiche`**（§10）。
**本页原样保留** —— 它是拍板时看的那张表，不是拍板之后的追述。

> ★ 拍板同时定了另外两条，都由本页的事实直接推出来：
> **G104** TLS 栈**整体换到 BoringSSL**（⚠ 推翻 §5.1 第 1 条 —— 因为第 2 节那条
> 「quiche 用 BoringSSL、依赖表里没有 rustls」使得「继续锁 rustls」等价于「两套 TLS 并存」）；
> **G105** HTTP/3 语义层用 `quiche::h3` **不自研**（第 0 节 (b) 与安全基线第 5 条）。
> ✅ ★ **那条未验前置已验完，通过**：
> 记录 [`musl + BoringSSL 静态链接`](/verification/musl-boringssl.md) —— **G103 不重议**。
> ⚠ ⚠ ★ 而下面第 3 条那句「没有实测，只是把两句话放在一起」，实测之后的答案与它的**方向**
> 一致（能做到）而**卡点完全不同**：卡的不是 BoringSSL 与 musl，是 **`boring-sys` 的
> bindgen 要 `dlopen`，而 Alpine 上 build script 默认是静态的**。
> ★ **一条「把两句话放在一起」的担心，实测之后往往不是被证实或证伪，而是被换成另一件事**
> —— 而那件事没有人预先写在任何清单上。

以下是拍板前的原文。它**只记查到了什么，不给推荐项** —— 推荐项与拍板走 §10 的常规流程。

★ 口径照 **G100 那次**：读的是源码与各 crate 自己声明的元数据，不是印象；
★ 也照 [尚未验证的接缝](/verification/open-seams.md) 结尾那条方法论：**推断不是证据** ——
下面凡是推断，都单独标着，并写明要拿什么把它坐实。

⚠ **全部数字的采集日期是 2026-08-25。** 这一页的每次复查都要换日期 ——
「上游停更了」与「我们上次看是三个月前」在句子里长得一样。

---

# 0. ★★★ 先纠正 D11 那行里的一处比较错位

D11 当初把候选写成 **`h3` / `quiche` / `s2n-quic` 三选一**（⚠ **它已由 G103 结案取
`quiche`**，不在 §11 里了 —— 本节记的是那次拍板前的比较）。
而实测出来的分层是：

| | QUIC 传输层 | HTTP/3 语义层 | 合起来要几个 crate |
|---|---|---|---|
| 路线 A | **quinn** | **h3**（靠 `h3-quinn` 适配） | 3 |
| 路线 B | **quiche** | **`quiche::h3`（同一个 crate 里）** | 1 |
| 路线 C | **s2n-quic** | ⚠ **没有** —— 它是 QUIC-only | 3（仍要 `h3` + 自写 backend）|

⇒ 两条结论直接从这张表出来：

1. ★ **D11 那行是拿一层去比两层。** 「`h3` 自 2025-05-06 起十五个月未更新」说的只是**上面那层**；
   下面那层 quinn，`quinn-proto 0.11.17` 是 **2026-08-17 发的（8 天前）**。
   ⇒ 「这条路线停滞了」这句话，对 h3 成立、对 quinn **不成立**，而原文把两者写成了一件事。
2. ★★ **路线 C 绕不开 h3。** s2n-quic 的 crate 描述原话是「An implementation of the IETF QUIC protocol」，
   feature 表 22 条里没有任何一条通向 HTTP/3。⇒ 选 s2n-quic **仍然要把 `h3` 摞在上面**，
   而且要走它那个名字就是警告的 feature：`i-implement-a-third-party-backend-and-opt-into-breaking-changes`。
   ⇒ **「换掉 h3」不是选 s2n-quic 能换来的东西。**

---

# 1. 维护活跃度

## crates.io（各 crate 自己声明的版本元数据）

| crate | 最新未 yank 版本 | 发布日期 | 距 2026-08-25 | 总下载 |
|---|---|---|---|---|
| `h3` | 0.0.8 | 2025-05-06 | **15.6 个月** | 4,024,685 |
| `h3-quinn` | 0.0.10 | 2025-05-06 | **15.6 个月** | 3,482,179 |
| `quinn` | 0.11.11 | 2026-06-22 | 2.1 个月 | 275,272,724 |
| `quinn-proto` | 0.11.17 | **2026-08-17** | **8 天** | 282,292,550 |
| `quiche` | 0.29.3 | 2026-07-14 | 1.3 个月 | 2,467,607 |
| `s2n-quic` | 1.88.0 | **2026-08-21** | **4 天** | 672,917 |

★ `h3` 与 `h3-quinn` **同一天发的**（2025-05-06）—— 适配器与本体一起停在那里，不是只停了一个。

## GitHub（2026-08-25 查）

| 仓库 | 最后一次提交 | 近半年提交数 | stars | open issues |
|---|---|---|---|---|
| `hyperium/h3` | 2026-08-13 | **7** | 884 | 48 |
| `quinn-rs/quinn` | 2026-08-23 | **≥100** | 5,233 | 199 |
| `cloudflare/quiche` | 2026-08-13 | **≥100** | 11,793 | 337 |
| `aws/s2n-quic` | 2026-08-24 | **≥100** | 1,363 | 272 |

⚠ **那三个 `≥100` 是查询上限不是计数**：`gh api commits?since=…&per_page=100` 一页取满就没再翻。
★ 而 `h3` 的 **7** 是真数出来的（一页没取满）。**一个取满了上限的数字与一个真实计数放在同一列里，
必须标出来** —— 否则下一个人会拿 100 去跟 7 比。

## ★★ 一条最容易读错的：`h3` **没停止开发，它是没发版**

| 事实 | 值 |
|---|---|
| 最后一次 **发版** | 2025-05-06（15.6 个月前）|
| 最后一次 **提交** | 2026-08-13（12 天前）|
| 近半年提交 | 7 |
| 仓库 archived？ | 否 |

⇒ 「十五个月未更新」这句话，按**发版**读是对的，按**开发**读是错的。
★ 两者的差别在选型上是实的：一个不发版的库意味着**修好的东西你拿不到**（除非钉 git rev），
而一个停止开发的库意味着**根本没人修**。⇒ D11 拍板时要说清楚记的是哪一个。

---

# 2. rustls 整合面

> ⚠ 这一栏挂着 §5.1 的**第 1 条硬约束**（TLS 后端锁死 rustls）与 G6 的原话
> 「一套证书、一套 ALPN、一套会话缓存」。★ 我们现在锁的是 **rustls 0.23.43**（`Cargo.lock`）。

| | TLS 后端 | 与我们这份 rustls 的关系 |
|---|---|---|
| **quinn 0.11.11** | `rustls ^0.23.5`（可选，但 `default` 里含 `rustls-ring` ⇒ 默认就开）| ✅ **同一个大版本**，不产生第二份 rustls |
| **quiche 0.29.3** | `boring ^4.3`（BoringSSL），`default = ["boringssl-boring-crate"]` | ❌ **依赖表里没有 rustls，9 个 feature 没有一条通向它** |
| **s2n-quic 1.88.0** | 默认 `provider-tls-default` → `s2n-quic-tls-default`（平台探测，即 s2n-tls）；`provider-tls-rustls` → `s2n-quic-rustls` | ✅ 拿得到，但**要显式关掉 default**；`s2n-quic-rustls 0.88.0` 声明的是 `rustls ^0.23`（**非可选**）⇒ 同大版本 |

## ★★★ quiche 那一格的后果比「不整合」严重

选 quiche 不是「rustls 那套用不上」，是**三件事同时发生**：

1. **产物里两套 TLS**：rustls 给 h1/h2，BoringSSL 给 h3 ——
   而 G6 的原话是一套证书、一套 ALPN、一套会话缓存。
2. ★★★ **`ResolvesServerCert` 在 BoringSSL 那侧根本不存在。**
   ⇒ D11 的第二条前置（[「`ResolvesServerCert` 能否同时服务两个入口」](/verification/open-seams.md)）
   对 quiche **不是「要验」，是「结构上做不到」** —— 两个入口会有两套完全独立的挑证书机制，
   而自动 HTTPS / On-Demand 全建在 rustls 那一条路上（§5.1 第 1 条，**选定后不能改主意**）。
3. **BoringSSL 是 C 依赖**，而 §5 的分发口径是 **musl 单静态二进制**。
   ⚠ 这一条**没有实测**，只是把两句话放在一起；要判它得真去 musl 上编一次。
   > ✅ **2026-08-25 补记（本页其余部分维持拍板时的原文）**：真去编过了，**通过** ——
   > 见 [`musl + BoringSSL 静态链接`](/verification/musl-boringssl.md)。

## ⚠ 一条会当场作废既有成果的坑：`aws-lc-rs`

quinn 的三个 rustls feature：

| feature | 后果 |
|---|---|
| `rustls-ring` | ring（**我们现在就是 ring**，`Cargo.lock` 里 `ring 0.17.14`）|
| `rustls-aws-lc-rs` | ⚠ **把 `aws-lc-rs` 拖回依赖图** |
| `rustls-aws-lc-rs-fips` | 同上 |

★ 本仓库为把 `aws-lc-rs` 挡在依赖图外面付过**三次**代价：G41/G45 那轮
（`pingora-core/rustls` 实测多解析 46 个包、**而 aws-lc-rs 不在其中**，`PLAN.md` §1）、
`instant-acme` 的 default feature、`hyper-rustls` 的 default feature。

✅ **好消息**：quinn 的 `default` 用的正是 `rustls-ring`，顺路。
⚠ **但别照抄 `default`**：它是 `['log', 'platform-verifier', 'runtime-tokio', 'rustls-ring', 'bloom']`，
其中 `platform-verifier` 是**客户端**验服务端证书用的 —— 我们这一侧是服务端。
⇒ 该写 `default-features = false` + 显式挑。

---

# 3. 「按 DCID 路由」的原语

> 这一栏挂着 D11 的**第一条前置**：升级窗口内两代进程持有同一个 UDP socket，
> 数据报会被分流，而 QUIC 的连接状态只在某一代的内存里 ——
> 被分到另一代的包**不是「换条会话」，是解不开**。见 [尚未验证的接缝](/verification/open-seams.md)。

## quiche：✅ 有，而且在连接存在之前就能用

```rust
pub fn from_slice<'b>(buf: &'b mut [u8], dcid_len: usize) -> Result<Header<'a>>
```

公开函数，**不解密、不需要先有连接**，返回的 `Header` 公开字段里就有 `dcid` 与 `scid`。
★ 再加上 quiche 是 **sans-IO**（crate 文档原话：应用负责提供 I/O 与带定时器的事件循环），
socket 与事件循环本来就在我们手里 ⇒ **「先看 DCID 再决定交给哪一代」这件事写在我们自己的代码里**，
不需要库同意。

## quinn：⚠ 路由在库内部，我们只有两个钩子

`Endpoint::handle(...) -> Option<DatagramEvent>`，而 `DatagramEvent` **只有三个变体**：

| 变体 | 载荷 | 文档原话 |
|---|---|---|
| `ConnectionEvent` | `(ConnectionHandle, ConnectionEvent)` | The datagram is redirected to its `Connection` |
| `NewConnection` | `Incoming` | The datagram may result in starting a new `Connection` |
| `Response` | `Transmit` | Response generated directly by the endpoint |

⇒ **没有一个变体表示「这个 DCID 我不认识」**，而返回类型是 `Option`。

### ✅ ★★★ 2026-08-25 已读源码坐实，而答案比推断**严重得多**

读的是 `quinn-proto-0.11.17` 这个 tag 的 `quinn-proto/src/endpoint.rs`（48648 字节，不是 docs.rs 的摘要页）。
`handle()` 末尾那串 `else if` 链的**最后一格**是：

```rust
} else {
    // If we got this far, we're receiving a seemingly valid packet for an unknown
    // connection. Send a stateless reset if possible.
    self.stateless_reset(now, datagram_len, addresses, *dst_cid, buf)
        .map(DatagramEvent::Response)
}
```

⚠ ⚠ ⚠ **认不出的数据报不是被丢掉，是被回一个 stateless reset** ——
而 stateless reset 的语义是**告诉对端「这条连接死了，拆掉它」**。

⇒ ★ ★ ★ **推断错在方向上**：我原以为最坏是「丢一个包」（QUIC 自己会重传，连接扛得住），
实际是**升级窗口里新一代会主动杀掉老一代的在飞连接**。
> ★ ★ **「拿不回那个数据报」与「替我们把连接杀了」，在 `Option<DatagramEvent>`
> 这个签名上长得一模一样。** 只看类型签名推不出方向，得读实现。

### ✅ 而同一段源码里就有解药，是个**现成类型**

那条 `else` 链的**前一格**是：

```rust
} else if !event.first_decode.is_initial()
    && self.local_cid_generator.validate(dst_cid).is_err()
{
    debug!("dropping packet with invalid CID");
    None
}
```

⇒ **只要 `validate()` 判 `Err`，quinn 就走「丢包」而不是「stateless reset」。**

关键在于**默认实现是什么**（`quinn-proto/src/cid_generator.rs`）：

| generator | `validate` | 后果 |
|---|---|---|
| trait 默认 | `fn validate(&self, _cid) -> Result<(), InvalidCid> { Ok(()) }` —— **无条件放行** | ⚠ 落进 `else` ⇒ **stateless reset** |
| `RandomConnectionIdGenerator` | **不覆盖**，用默认 | ⚠ 同上 |
| **`HashedConnectionIdGenerator::from_key(key)`** | 按 key 对 nonce 算 FxHash 签名并比对 | ✅ **两代用不同 key ⇒ 互相拒认 ⇒ 丢包，不 reset** |

★ ★ 而 `from_key` 的文档原话就是冲着这件事写的：
**"Allows `validate` to recognize a consistent set of connection IDs across restarts"**。

⇒ **降级路线是现成的**：每一代一个自己的 key，失效形态从「杀死连接」降到「丢一个数据报」，
而后者 QUIC 自己会重传扛过去。⚠ **但它仍然只是降级，不是解决** ——
落到新一代手里的那些包**还是回不到老一代**（`handle` 回的是 `None`，
数据报和 DCID 都拿不回来）。**真正的「转交」只能在 quinn 之外做**：
`Endpoint::new` 收的是我们自己的 socket，所以路由要写在
我们自己实现的 `AsyncUdpSocket::poll_recv` 里，在喂给 quinn 之前分流。
⚠ ⚠ 而两代是**两个进程**，跨进程转交那一段与选哪个库无关 —— 三条路线都要自己写。

我们**确实有**的钩子是 `ConnectionIdGenerator`：

| 方法 | 用途 |
|---|---|
| `generate_cid(&mut self) -> ConnectionId` | ★ **CID 的字节由我们自己造** ⇒ 可以埋一个「第几代」的标记 |
| `cid_len(&self) -> usize` | 长度 |
| `cid_lifetime(&self) -> Option<Duration>` | 有效期 |
| `validate(&self, cid) -> Result<(), InvalidCid>`（有默认实现）| 快速筛掉「不像本 generator 发的」，**允许假阳性** |

⚠ ⚠ **但「筛掉」与「转交给上一代」是两件事。** 前者 quinn 给了，后者正是这条前置要答的问题，
而上面那个推断说的恰恰是：**后者可能在这个 API 面上做不到。**

## s2n-quic：形状与 quinn 同族，但 `Validator` 能**从原始报文里解析长度**

`provider::connection_id` 里三个 trait：**`Generator`** / **`Validator`** / **`Format`**。
✅ **2026-08-25 已读源码拿到签名**（`quic/s2n-quic/src/provider/connection_id.rs`，v1.88.0 那个 tag）：

```rust
fn generate(&mut self, connection_info: &ConnectionInfo) -> connection::LocalId;
fn lifetime(&self) -> Option<Duration>;
fn validate(&self, connection_info: &ConnectionInfo, buffer: &[u8]) -> Option<usize>;
```

★ **注意 `validate` 的形状与 quinn 的不是一回事**：quinn 那个收一个已经解析好的
`&ConnectionId`、回 `Result<(), InvalidCid>`（"这个像不像我发的"）；
而这个收的是**原始字节缓冲**、回 `Option<usize>` —— **它回的是「CID 有多长」**，
也就是说它承担的是**从报文里把 CID 解析出来**这件事，比 quinn 那个钩子靠前一层。

⚠ **但仍然没有证据说明 s2n-quic 对认不出的 CID 做什么**（丢包？stateless reset？）——
★ 这正是 quinn 那条栽过的地方：**钩子的形状说明不了失效形态。**
⇒ 要判它得读 `s2n-quic-transport` 的 endpoint 分发路径，与读 quinn 那次同样的做法。

## 一句话对比

| | 能在「连接存在之前」拿到 DCID？ | 能控制自己发出去的 CID 字节？ | 认不出的数据报能拿回来吗？ | ★ **认不出时的失效形态** |
|---|---|---|---|---|
| quiche | ✅ `Header::from_slice` | ✅ `accept(scid, odcid, local, peer, config)` —— 服务端 CID 由调用方传进去 | ✅ 数据报从头到尾都在我们手里 | ✅ **由我们决定** —— 库根本没机会自作主张 |
| quinn | ❌ 没有公开的部分解码，`handle` 回 `None` | ✅ `generate_cid` | ❌ **不能**（已读源码坐实）| ⚠ ⚠ **默认是 stateless reset ⇒ 杀掉对端连接**；换 `HashedConnectionIdGenerator::from_key` 后降级为丢包 |
| s2n-quic | ✅ `Validator::validate(_, buffer) -> Option<usize>` 收原始字节 | ✅ `Generator::generate` | ⚠ 未核实 | ⚠ **未核实** —— 而 quinn 那条正说明**钩子的形状说明不了失效形态** |

---

# 4. ★ 第四条维度：socket 移交（§5.1 第 3 条要它）

> 这一条不在最初点名的三条里，但它是**硬约束**：自建 QUIC(UDP) 监听器
> **必须接入 Pingora 的 socket 移交**，否则优雅升级时这条会断连。

| | 能不能喂一个已经 bind 好的 socket |
|---|---|
| **quinn** | ✅ **能**。`Endpoint::new(config, server_config, socket: std::net::UdpSocket, runtime: Arc<dyn Runtime>)` 直接收一个已 bind 的 `UdpSocket`；另有 `Endpoint::new_with_abstract_socket(..., socket: Arc<dyn AsyncUdpSocket>, ...)` |
| **quiche** | ✅ **天然如此**。sans-IO ⇒ 它根本不碰 socket |
| **s2n-quic** | ✅ **能**（2026-08-25 读源码核实，`quic/s2n-quic-platform/src/io/tokio/builder.rs`，v1.88.0）：`Builder::with_rx_socket(socket: std::net::UdpSocket)` 与 `with_tx_socket(…)` 各收一个**已 bind 的** `std::net::UdpSocket`（另有 `with_prioritized_socket`）|

★ **三条路线在这一栏上没有区别** —— §5.1 第 3 条对选库**不构成筛选条件**。

---

# 5. ★ 仓库里已有两道门在管这件事，选型要连它们一起看

`crates/fulcrum/tests/supply_gates.rs` 里：

| | 门 | 它对三条路线各说什么 |
|---|---|---|
| 门 1 | `两把锁里都不许出现_aws_lc_rs()` —— 扫根锁与 `vendor/pingora` 的锁，任何 `aws-lc*` 开头的包都判红 | ✅ quinn 走 `rustls-ring` 不触发；⚠ 走 `rustls-aws-lc-rs` **当场红** |
| 门 2 | `会自带_crypto_provider_的依赖必须写_default_features_false()` —— 按一张**手写名单** `MUST_DISABLE_DEFAULT` 查 | ✅ ★ **`quinn` 已经在那张名单里**（`supply_gates.rs:133`，早就有人替它留了位）|

## ⚠ ⚠ 而门 2 那张名单里**没有** `s2n-quic`

s2n-quic 的 `default` 是 `['provider-address-token-default', 'provider-tls-default']`，
后者通向 `s2n-quic-tls-default`（平台探测 ⇒ **s2n-tls**）。
⇒ 一份照默认写的 `s2n-quic = "1"` 会把**另一套 TLS 栈**编进产物，
而**门 1 不会红**（s2n-tls 不叫 `aws-lc*`）、**门 2 也不会红**（名字不在名单上）。

★ ★ 这正是 `unwired_contract.rs` 顶上引的那条纪律说的事：**手写清单在清单本身漏了一项时照样绿。**
⇒ **若 D11 选 s2n-quic，把 `s2n-quic` 加进 `MUST_DISABLE_DEFAULT` 是同一批里必须做的一步**，
不是「记得也可以」。

⚠ `quiche` 不在名单上**不是同一个洞**：它拖进来的是 `boring`（BoringSSL），
既不是 rustls 的 crypto provider，也不叫 `aws-lc*` —— 两道门对它**结构上就说不出话**。
⇒ 选 quiche 的话，「产物里有几套 TLS」这件事**目前没有任何门看着**。

---

# 6. ✅ 原先欠的三条**2026-08-25 全部补上**（读的是各自 release tag 的源码）

| | 欠什么 | 结果 |
|---|---|---|
| ① | quinn 对认不出的 DCID 怎么处理 | ✅ **查清了，而且推断是错的**：默认**回 stateless reset**（杀连接），不是丢包；换 `HashedConnectionIdGenerator::from_key` 可降级为丢包。见第 3 节 |
| ② | s2n-quic `connection_id` 三个 trait 的签名 | ✅ 拿到了，且 `validate` 收**原始字节**回 `Option<usize>`，比 quinn 的钩子靠前一层 |
| ③ | s2n-quic 能不能接一个继承来的 UDP fd | ✅ **能**（`with_rx_socket` / `with_tx_socket`）⇒ 三条路线在这一栏无差别 |

## ⏳ 而补完之后新长出来的两条

| | 欠什么 | 怎么补 |
|---|---|---|
| ④ | **s2n-quic 对认不出的 CID 做什么**（丢包？stateless reset？）| 读 `s2n-quic-transport` 的 endpoint 分发路径 —— ★ ① 那条教训正是：**钩子的形状说明不了失效形态** |
| ⑤ | **quiche 对认不出的 CID 做什么** | ★ 严格说这一条不存在：quiche 是 sans-IO，报文交给谁由我们决定，库没有机会自作主张。⇒ 登记在这里只是为了让这一栏三家都有话说 |

> ★ ★ ★ **补 ① 的过程本身值得记一句**：三条欠账里，只有 ① 的答案**改变了结论的方向** ——
> ②③ 只是把「未核实」换成「能」，而 ① 把「最坏是丢一个包」换成了
> **「新一代会主动杀掉老一代的在飞连接」**。
> ⚠ 而这两种结局在 `Option<DatagramEvent>` 这个类型签名上**长得一模一样**。

---

# 相关

[尚未验证的接缝](/verification/open-seams.md)（两条前置的完整推导） ·
[决策日志](/governance/decision-log.md)（D11 由 **G103** 结案：取 `quiche`） ·
[技术栈](/platform/tech-stack.md) ·
[供应链现状](/platform/supply-chain.md)（脱字号需求自带上界；重复依赖让整张回归网停摆过一次） ·
[TLS 与自动 HTTPS](/architecture/tls.md)
