---
type: 架构基线
title: 数据路径
description: 三个入口归一到同一个路由层与同一个上游池；静态文件自研，缓存磁盘后端自研；上游支持 DNS 定期重解析。
resource: ../../PLAN.md
tags: [架构, 承重墙, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G6（Pingora 底座 + quinn/h3）、G7（自研四块）、G17（上游发现）
  - id: plan-6
    resource: /references/plan.md
    title: PLAN.md §6.1 首版自研范围
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 D8（磁盘缓存后端设计，最晚 M2）、D11（HTTP/3 库选型）
---

★ 本页属**技术基线**，带真内容。它**服从** [`PLAN.md`](../../PLAN.md)；冲突时以 `PLAN.md` 为准。

> ✅ **h1/h2 那条入口已经真的在跑了。** 下面 §「M1 批 2 落地了什么」是实测记录，
> 其余各节仍然是目标形态。

# M1 批 2 落地了什么

```text
结构化配置 ──▶ crates/fulcrum-runtime（运行时对象图，纯逻辑）
                     │   站点索引 (Host, 端口) · 匹配器 · 执行链求值
                     ▼
              crates/fulcrum-server（数据面）
                     │   HttpServerApp + connectors::http
                     ▼
                  Pingora
```

| 已经在跑 | 说明 |
|---|---|
| HTTP/1.1 + h2c 入口、keep-alive | `HttpServerApp`；**每个监听端口一个 app 实例** |
| `respond` / `redir` / `header` / `rewrite` / `handle` / `route` | 按执行顺序表（G49）求值 |
| `reverse_proxy` | h1/h2 上游、四种 `lb_policy`、`header_up` / `header_down` |
| 421 / 404 / 502（G63）与 `handle_errors` | 无站点匹配真的回 421 |
| 回落指令（`file_server` / `cache`）| ✅ **薄转发给 nginx**（G79）；没配 `fallback_nginx` 才回 501，见下 |

★ ★ **路由语义与数据面被故意切开**：`fulcrum-runtime` **一行 pingora 都不引用**。
代价是数据面要做一次 `&RequestHeader → RequestCtx` 的转换，
换来的是整套路由语义**可以脱网单测**——而那正是本项目最需要被测的逻辑。
⚠ 两层测试都要有：只有单测，一个「决策算对却写错响应」的数据面照样全绿；
只有端到端，一条规则错了只会得到一个状态码。

## ✅ 回落指令怎么走（G79）

`file_server` / `cache` 该回落给 nginx（§6.3），`l4` 该回落给 caddy。
全局块里写 `fallback_nginx <主机:端口>` 就**薄转发**过去。

| 全局块里 | 该走回落的请求 |
|---|---|
| 没写 | **501** —— 诚实的中间态：你没告诉我后端在哪 |
| 写了，后端连得上 | **转发过去**（+1 跳，装载日志逐条列出）|
| 写了，后端连不上 | **502**（`all_upstreams_down`）|

★ ★ **501 与 502 有意不合并**：合并之后，一次配置遗漏与一次后端故障
在现场长得一模一样。三个方向各有一条端到端判据。

★ ★ ★ **「薄转发」是用「共用同一段代码」落实的**：回落目标就是一个普通的
`ProxyTarget`，走的是与 `reverse_proxy` **完全同一段**转发函数 ——
于是能力翻译、配置语义映射、统一两家行为差异这三件事**做不到**，
而不是靠自觉不去做。见[回落层](/architecture/fallback.md)。

⚠ `l4` 整条仍未接线，所以 caddy 那一侧目前**没有请求走得到**。

## ⚠ 每个监听端口一个 app 实例

站点索引按 `(Host, 本地端口)` **两维**查，而 Pingora 的 `ServerSession`
**问不出「这条连接落在哪个监听器上」**。把端口写进 app 里是唯一不靠猜的办法。

## ★ ★ 客户端 IP 只取 socket 对端

`remote_ip` 匹配器拿到的是 `session.client_addr()`，**不是 `X-Forwarded-For`**——
后者的最左项客户端可以随便写，拿它做内网放行等于没有放行控制。
口径见[安全基线](/platform/security-baseline.md)。
~~⚠ 将来支持「信任前置代理」时，判据是**最近一跳**且必须显式配置。~~

★ ★ **（M2 批 D）：上面那句「将来」到了一半。**
`proxy_protocol_from <网段…>` 就是那个「必须显式配置」的清单，
而它落实「最近一跳」的方式比 XFF 更彻底：**PROXY 头本来就只描述一跳**
（发它的那台代理说「我这条连接的客户端是谁」），⇒ 不存在「取最左还是最右」这个问题。

| 面 | 现在怎样 |
|---|---|
| **L4** | ✅ **收与发都接线了**，见 [DSL 参考 §4.5](/architecture/dsl-reference.md) |
| **HTTP** | ⏳ 清单收得进配置、建得进运行时图，**运行时不做** —— 已登记进 `UNWIRED`。Pingora 的 accept 与上游连接器都够不到，要给 fork 加接缝（`PLAN.md` §10）|

⚠ ⚠ **在 HTTP 那半边接线之前，`remote_ip` 仍然只取 socket 对端** ——
前面挂了 LB 的话，它拿到的是**那台 LB 的地址**。★ 这一条写在这里是因为
「配置里写了 `proxy_protocol_from`」与「`remote_ip` 会用它」是两件事，
而它们看起来完全一样。

# 三个入口，一个路由层

| 入口 | 实现 | 说明 |
|---|---|---|
| h1 / h2（TCP + TLS） | Pingora `http_proxy_service` | 含 WebSocket 升级与 gRPC（本质是 h2）|
| **HTTP/3（UDP + QUIC）** | **自建 `Service`**，⚠ ~~`quinn` + `h3`~~ → **`quiche`**（**G103**）| ✅ **（批 J）已落地并接线**：有 `tls` 的端口自动在**同端口**听 UDP（G110），h1/h2 响应带 `Alt-Svc`。★ Pingora 0.8 **不带 HTTP/3**；★ 自建的是入口/连接归属/路由汇合，**协议栈本体用 `quiche::h3`**（G105）|
| **L4（TCP / UDP 透传）** | **自建 `Service`** | ✅ **整个 L4 面已落地**：TCP（批 A）+ UDP（批 B）+ SNI/ALPN 分流（批 C）+ **PROXY protocol 收发（批 D）**。自建监听器 + 参与 socket 移交、与 `reverse_proxy` 共用同一套挑上游的实现；UDP 另有会话表（空闲 60s 回收 + 1024 条上限）与「停机即停 `recv_from`」|

★ 三个入口把请求归一到**同一个路由层**与**同一个上游池**，因此配置里**不存在**「这条规则只对 HTTP/3 生效」这类分裂。
✅ ★ ★ **这句话有判据**：h3 入口由 `FulcrumApp` 自己实现
`H3RequestHandler`（**同一个 app 实例**，`H1Entry` 那层 newtype 存在的唯一理由就是别建出第二个
—— 第二个意味着**第二个上游连接池**），而
[`tests/h3/run.sh`](../../tests/h3/run.sh) 拿 `reverse_proxy` / `file_server` / 错误页
三种 outcome 在 h3 与 h1 上比**逐字相同的响应体**。
⚠ ⚠ 只验「h3 上 GET / 拿到 200 与正确的体」是不够的：注入一个不走执行链、直接回首页那个串的桩，
**那条判据仍然是绿的**。

两个自建入口能与 Pingora 原生入口挂在同一个 `Server` 上而不牺牲优雅升级——这一点已由 M0 证明，见 [M0 接缝验证](/verification/m0-seam.md) 与 [进程与组件边界](/architecture/process-model.md)。

★ **HTTP/3 那条前置由 G109 解决**：升级窗口内两代进程共享同一个 UDP socket，
数据报会被分流，而**连接状态只存在于某一代** ⇒ 代标识编进服务端自选的 CID 前缀（批 J），
不属于本代的数据报经一条 unix datagram 通道**跨进程转交**给它那一代（批 K）。
⇒ **换代零中断对 h3 也成立。** ★ 转交**只走一跳**，而这一点是结构性的：
`relay::decode` 的返回值里没有 `gen_id`，处置它的那一层也拿不到判归属所需的任何东西。

# 静态文件与缓存

- **静态文件由枢衡自身实现**：range、ETag、压缩与预压缩、目录索引。
- ~~**HTTP 缓存复用 `pingora-cache` 的 `Storage` 与 `EvictionManager` 抽象，磁盘后端由枢衡实现**~~ —— ⚠ ★ **由 G82 改口径：缓存层完全自研，`pingora-cache` 不进 fork。** 原文保留在这里，因为它记录了这个决定是从哪个形状改过来的。

## ✅ D8 已由 G82–G84 结案

| | 定下来的 |
|---|---|
| **缓存层来源** | **完全自研**，不 vendor `pingora-cache`（★ 因此 D17 的「只补 `pingora-cache`」作废，fork 当时维持 7 个 crate；⚠ ** 因 G104 加回 `pingora-boringssl`，现为 8 个** —— 与缓存无关）|
| **磁盘布局** | 缓存键 hash 的**两级分片目录** + 每条目 **meta 与 body 两个文件** + `tmp` 后 `rename` 原子落地 |
| **崩溃恢复** | 启动**不扫盘**：读时校验、坏条目即丢即删；后台任务渐进重建索引 |
| **`purge`** | 走管理面 `POST /purge`（按 key 或前缀），与 `/load`、`/renew` **同一个** Unix socket、同一套 0600 权限（G14）|

★ **meta 与 body 分开**不是风格：重验证（304）**只改 meta 不动 body**，而那是缓存最常见的写操作之一；合成一个文件就得整体重写。
★ **启动不扫盘**是因为它与 G78 的 `sd_notify(READY=1)` 直接冲突 —— 零停机换代要求新一代**快速就绪**，而全盘扫描的时间随缓存大小线性增长。⚠ 代价是索引与磁盘之间有个**最终一致**的窗口：刚重启时可能少算占用，于是淘汰比稳态晚一点触发。

⚠ ⚠ **自研的代价写在明处**：RFC 9111 的新鲜度与可缓存性、`Vary`、防惊群、元数据序列化全部自己写（对照 `pingora-cache` 0.8.1 的实测行数 ≈4000 行），**而缓存的错表现为「偶尔给错内容」** —— 不像转发的错那样当场可见。⇒ 判据要按这个形状设计，不能只测「命中率对不对」。

★ 磁盘缓存的崩溃恢复是经典难题（`PLAN.md` §9 已列为风险）。**缓解顺序仍然有效**：先做内存层与只读缓存，磁盘后端最后做 —— 它说的是**做的顺序**，上面那三条说的是**做成什么形状**，两者不冲突。

## ✅ 已经做出来了：语义层与内存后端＝**批 G**，磁盘后端＝**批 H**

配置面是 `cache { disk <目录> }`；不写 `disk` 就是内存后端。逐条见
[DSL 参考 §4.2](/architecture/dsl-reference.md)。⚠ **缓存后端是进程级的** ——
多个 `cache` 块写了不同的 `disk` 是编译期错误（`FUL-DSL-0035`）。

落地时值得记住的三件事：

1. ★ ★ ★ **读路径不看淘汰索引。** 按缓存键算出文件名直接开 ⇒ 「启动不扫盘」与
   「重启之后第一个请求就能命中」**同时**成立。⚠ 把读挂到索引上的写法会让一次重启
   等于一次全量回源 —— 而那恰恰是磁盘缓存存在的理由。索引只管淘汰。
2. ★ **meta 是一族（一个主键）一个，body 是一条一个。** 请求到达时还不知道 `Vary`
   （那是响应告诉我们的），所以变体的文件名算不出来 —— 一族一个 meta 让一次查找
   只要两次 `open`。没有 `Vary` 时它**就是** G83 说的「两个文件」，而 G83 给出的
   理由（重验证只改 meta 不动 body）一字不差地成立。
3. ★ **目录用不了 ⇒ 关掉缓存、照常转发。** 不退回内存（按磁盘写的 `capacity`
   会变成内存预算 ⇒ OOM），也不拒绝启动（换代时那是服务整体中断）。
   ⚠ 而「关掉」必须说得出来：装载日志一行 `error` + 运行时 `X-Fulcrum-Cache`
   那个头不再出现。

# 上游发现与健康（G17）

- **上游地址**：静态列表 **或** 域名 + **定期重解析**
- **主动健康检查**：可配频率与并发上限
- **被动熔断**：按错误率与连续失败计数摘除，恢复后按**慢启动**回灌

> ★ **DNS 定期重解析直接消灭 nginx OSS 的经典事故源**：上游域名只在启动时解析一次，后端 IP 变了必须 reload。

# 最容易在哪做错

1. ★ **给 HTTP/3 单开一套路由规则。** 三个入口共用一个路由层是本页的核心；一旦分裂，「一份配置」这个产品理由就破了。
2. ~~★ **假设 `pingora-cache` 开源版带磁盘后端。**~~ ✅ ** 已亲手核实**（读 0.8.1 源码）：`memory.rs` 的 `MemCache` **是它唯一的 `Storage` 实现**，而 `EvictionManager` 有两个（`lru` / `simple_lru`，自带 save/load）。★ 这条核实**照样有价值**，尽管 G82 之后我们不用它了：它是拍板时看的那张表上的一行。
3. **把慢启动漏掉。** 熔断恢复后直接满权重回灌，会把刚恢复的节点二次打死。

# 相关

[TLS](/architecture/tls.md)（两个入口共用同一个 `select_certificate_callback`） · [观测](/architecture/observability.md)
