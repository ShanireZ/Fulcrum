---
type: 架构基线
title: 观测
description: Prometheus 指标 + 结构化访问日志 + Runtime 实时 stats；首版不做 OpenTelemetry tracing。
resource: ../../PLAN.md
tags: [架构, 观测]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G16（观测基线）、G8（Runtime 通道）、G23（首版不做 Web UI）
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 D7（结构化访问日志的字段清单与格式，最晚 M2）
---

★ 本页属**技术基线**，带真内容。它**服从** [`PLAN.md`](../../PLAN.md)；冲突时以 `PLAN.md` 为准。

# 三件套（G16）

| 面 | 形态 |
|---|---|
| **指标** | Prometheus 端点；覆盖连接、请求、上游、TLS、缓存、证书签发。⏳ **方案已定（批 M，G116–G118/G121）**，见下面「指标」一节 —— ⚠ 方案定了不等于做完了 |
| **日志** | 结构化访问日志与错误日志；**字段清单与格式已由 G113 + G114 定稿**（D7 结案）—— 见下面「访问日志的字段契约」|
| **实时** | Runtime 通道上的**只读 stats**：每上游的实时连接数、队列、错误、健康态，以及**临时覆盖层清单**。⏳ **方案已定（批 N，G119/G120）**，见下面「Runtime 实时 stats」一节 |

# 访问日志的字段契约（**G113 + G114**，由 D7 结案）

✅ ✅ ✅ **整份契约已接线完毕**：固定集是**批 L 第 ② 步**
（`log` 从 [`fulcrum_runtime::UNWIRED`](../../crates/fulcrum-runtime/src/lib.rs) 里**整条删掉**），
**白名单头与 TLS 信息是第 ③ 步**。判据都长在**第二十三个场景**
[`tests/log/run.sh`](../../tests/log/run.sh)（第 ② 步 36 条断言，第 ③ 步 +26 条 = **62 条**，
两批各做过注入反证：第 ② 步四次、第 ③ 步六次）。

⚠ ★ **TLS 那几格在 HTTP/3 上是 3/4** —— `tls_cipher` 取不到，
见下面「h3 上有三格，第四格拿不到」。★ 那不是遗漏，是 quiche 的公开 API 到此为止。

★ 分开说，是因为「契约定了」「一半接了」「全接了」是三件事，
而本页此前那条「日志：字段清单是 D7（待定）」躺过一段 ——
**一句被读成「已经有了」的契约比没有契约更贵。**

## 形状

**一行一条 JSON**（`\n` 结尾），**字段扁平**——没有任何嵌套对象、没有数组。

★ ★ 扁平不是风格：它让「以后想加 logfmt」只需要换一个序列化器，**字段清单一个字不动**。
取 Caddy 那种嵌套（`request.headers` 是一个 map）的话，加 logfmt 就得再定一套拍扁规则，
而**那套规则自己又是一份要钉住的契约**。

⚠ ⚠ **取不到的字段不出现，而不是给 `null`。** `null` 在 logfmt 里没有对应物，
而 `upstream=` 与「这条请求没有上游」是两个意思。

## 固定字段（每一条都有）

| 字段 | 类型 | 语义 |
|---|---|---|
| `ts` | string | RFC 3339、**UTC**、毫秒精度（`T03:14:15.926Z`）|
| `level` | string | 按状态码派生：`< 400` → `info`，`4xx` → `warn`，`5xx` → `error` |
| `proto` | string | `HTTP/1.1` · `HTTP/2.0` · `HTTP/3.0`。★ **取自入口而不是推断**（与 `Alt-Svc` 那处同一条纪律）|
| `method` | string | 请求方法，原样 |
| `host` | string | `Host` / `:authority`，**去掉端口** |
| `uri` | string | **原始**请求目标（path + query），`rewrite` **之前** |
| `status` | number | 下游看到的状态码。⚠ **一个响应头都没写出去时是 `0`** —— 那不是「未知」，是「这条连接上什么都没发生」（此时 `outcome` 是 `aborted`）|
| `resp_size` | number | 响应**体**字节数（**不含头**）。⚠ ⚠ 这一句需要 fork **第 13 处**改动才成立：上游 h1 的 `body_bytes_sent` 把序列化后的响应头也加了进去，而 **h2 与 h3 只数体** ⇒ 同一个请求走两种协议会给出两个数。★ 那处改动连同一条长在上游测试模块里的回归守卫，见 [`FORK.md`](../../vendor/pingora/FORK.md) |
| `duration_ms` | number | 从读到请求头到响应写完，毫秒，三位小数 |
| `outcome` | string | 闭集，见下 |
| `remote_ip` | string | ★ ★ ★ **见下面「`remote_ip` 是这份契约里最贵的一格」** |
| `remote_port` | number | 同上 |

`outcome` 的取值是**闭集**：`reverse_proxy` · `file_server` · `respond` · `redir` ·
`error` · `no_site_match` · `acme_http01`。
★ 闭集意味着**新增一种终结方式必须来改这里**，而不是悄悄多出一个没人认得的值。
（落法：数据面从 `Outcome` 那个枚举**一次算出来**，穷尽 `match` ⇒ 加一种就编不过。）

⚠ ⚠ ★ **`outcome` 说的是「怎么终结的」，不是「回了几号码」** —— 两者独立：
一条用户写的 `respond 503` 的 `outcome` 是 **`respond`**（它没走错误页那条路），
而 `level` 那一格才会是 `error`。★ `outcome=error` 专指**走了 `handle_errors` / 默认错误页**
那条路（连不上上游、静态文件 404、站点内无路由匹配）。
⇒ 想按「出了几个 5xx」统计就看 `status`；想按「哪条路径在出问题」统计才看 `outcome`。

## 条件字段（★ 取不到就不出现）

| 字段 | 何时出现 |
|---|---|
| `site` | 命中了站点（`outcome=no_site_match` 时**没有这一格**）。★ 值是**站点的名字** = 配置里**第一个地址的原文**（例 `http://a.example:9900`），装载日志用的也是它 —— ⚠ 不是主机名：一个站点可以有多个主机名，而日志要指得回配置里那一块 |
| `upstream` | `outcome=reverse_proxy` 且真的选中了一个上游 |
| `cache` | 这条请求过了缓存层；取值与 `X-Fulcrum-Cache` **同源**（`HIT` / `HIT-DISK` / `REVALIDATED-DISK` / …）。⚠ 回源**没有**这个值 —— 那是本仓库既有口径，不是遗漏 |
| `tls_version` · `tls_cipher` | 这条连接是 TLS。★ 两格都来自 `SslDigest`，⚠ 值为空串时那一格**不出现**（等同「问不出来」）—— **h3 上 `tls_cipher` 正是这种情况**，见下面那一节 |
| `tls_sni` | 客户端发了 SNI（**小写**）。⚠ 它与 `host` **是两件事**：一条按 IP 直连、Host 头写着域名的请求有 `host` 而没有 `tls_sni` |
| `tls_alpn` | 协商出了 ALPN（`h2` / `http/1.1` / `acme-tls/1`）。⚠ 客户端不发 ALPN 时**不出现**，而 `proto` 照常有值 —— ★ 那条反向判据在场景里，它证明这一格不是从 `proto` 反推的 |
| `req_hdr_<名>` · `resp_hdr_<名>` | 该头**在白名单里**且这条请求上真的有 |

**头名规范化**：小写，`-` 换成 `_`，加前缀 `req_hdr_` / `resp_hdr_`
（`User-Agent` → `req_hdr_user_agent`）。多值头按 `, ` 连接（RFC 9110 §5.3）。
⚠ 同一个头在白名单里写两遍（含大小写不同）**只留一份** —— 两个同名的 JSON 键，
哪一个会被解析器留下没有定义。
⚠ 响应头取的是**最终**那一份（`header` 那一步之后写出去的），不是上游给的那一份。

## ⚠ h3 上有三格，第四格拿不到

一条走 **HTTP/3** 的请求，`tls_version`（恒 `TLSv1.3`）· `tls_sni` · `tls_alpn` 三格都有，
**只有 `tls_cipher` 不出现**。

★ ★ **三格与 h1/h2 走的是同一段代码** —— 都读同一个 `SslDigest`，
差别只在**谁造了那份 digest**：h1/h2 是上游的 `from_ssl()`（拿 `&SslRef`），
h3 是 `H3Session`（拿 `quiche::Connection`）。
⇒ 「同一格数据两个填法」在结构上做不到，而那正是 **D27** 结案时取这条路的理由。

⚠ **`tls_cipher` 为什么拿不到**：quiche 的 `Handshake::cipher()` 在**私有** `mod tls` 里，
而 quiche 一个 TLS 出口都没有 re-export（`Connection` 的公开 API 只给得出
`server_name()` 与 `application_proto()`）。
★ 处置是**留空 ⇒ 那一格不出现**，而不是编一个值 ——
⚠ ⚠ **一个编出来的套件名读起来与真的一模一样**，而它会一路骗到读日志的人。

★ `tls_version` 恒为 `TLSv1.3` **不是猜的**：RFC 9001 §4.2 写死了 QUIC 只能用 TLS 1.3。

> ✅ **D27 与 D28 一起结案** —— 落点是 **fork 改动 14**
> （`SslDigest` 直接多两格 `sni` / `alpn`）。⚠ 顺带把 D28 那趟「每条握手多一次挂起/恢复」
> 也消掉了：那两格不再需要 `TlsAccept` 回调。

## ★ ★ ★ `remote_ip` 是这份契约里最贵的一格

**语义**：默认是 **socket 对端**；而当这条连接落在全局
`proxy_protocol_from` 的信任清单里时，它是 **PROXY 头里的真实客户端**。

⚠ ⚠ 这两句必须**同时**成立才算这一格做完 —— 先发一个「只可能是 socket 对端」的
`remote_ip`、以后再改成「可能是 PROXY 头里那个」，**是一次静默的语义变更**，
而日志的消费方不会有任何提示。
★ 这正是 §10 把「PROXY protocol 的 HTTP 半边」的时机绑在 D7 上的全部理由 ——
**理由是契约顺序，不是工期**。

## 配置面

```text
log {
    output   stderr | file <绝对路径>
    level    debug | info | warn | error
    headers       <名字…>
    resp_headers  <名字…>
}
```

| 子指令 | 语义 |
|---|---|
| `output` | `stderr`（默认，进 journal）或 `file <绝对路径>`。★ ★ 文件**在装载时就打开**，打不开是**硬错误**（`serve` 起不来 / `POST /load` 回 400 且旧配置一个字节没动）—— ⚠ 一个用来「出了事你能知道」的东西，自己坏掉时必须有人知道，而不是「起来了、服务正常、日志悄悄没有」。⚠ 必须绝对路径，理由与 `cache { disk }`（G91）逐字相同 |
| `level` | **阈值**：`warn` 只记 4xx/5xx，`error` 只记 5xx。⚠ 对访问日志 **`debug` 与 `info` 等价**（都记全部）—— 保留 `debug` 只是因为它本来就在语法里 |
| `headers` | **请求头白名单**。⚠ ⚠ 默认**一个头都不记** —— 不写这一行就是不记，没有「记几个常用的」这种中间态 |
| `resp_headers` | **响应头白名单**。同上。★ 取的是**最终**响应头（`header` 那一步之后） |

⚠ 白名单里的名字**必须是合法 HTTP 头名**（RFC 9110 的 `token`），否则配置装不上。
★ 理由是「查不到」与「这条请求上没有这个头」在日志里**长得一模一样** ——
⇒ 一个拼错的名字会静静地什么都不记，而运维看到的是「我配了它怎么没有」。

### ⚠ ⚠ 白名单里的四个名字是编译期错误

`Authorization` · `Cookie` · `Set-Cookie` · `Proxy-Authorization`
写进任一白名单 ⇒ **`fulcrum validate` 直接报诊断，配置装不上**（大小写不敏感）。

★ ★ 理由要连着 G114 一起读：owner 推翻的是「记不记头」这条推荐项，
**没有**推翻它背后那半条 ——「私钥、ACME 凭据、上游认证信息不得进普通日志」
（[安全基线](/platform/security-baseline.md)，也是本页「最容易在哪做错」第 2 条）。
⇒ **一个被推翻的推荐项，它的理由通常只有一半被推翻**，那半条要换一个落点继续成立。
★ 取「编译期拒绝」而不是「运行时脱敏」：后者要求脱敏表跟得上每一个新的敏感头名，
而**漏一个就是一次静默泄漏**；前者的失效形态是「配置装不上」，当场可见。

### ⏳ 没有站点匹配的那条请求，记不进访问日志

`log` 是**站点块内**的指令，而 `outcome=no_site_match`（421，G63）的请求
**不属于任何站点** ⇒ 它没有一份 `log` 配置可用。

⚠ 这不是「以后顺手补上」，**已登记为 [`PLAN.md`](../../PLAN.md) §11 的 D26** ——
它要拍的是配置面往哪儿长（全局 `log` 块 / 站点级的默认 / 就这样），
而那是一个比字段清单更大的问题。

# 指标（三件套的第一件，**批 M**）

## 端点 = 站点块里的终结指令 `metrics`（G116）

`metrics` 进执行顺序表当 **Terminal**（序号 75，夹在 `respond` 与 `reverse_proxy` 之间），
写在**普通站点块**里：

```text
metrics.example:9443 {
    tls /etc/fulcrum/m.crt /etc/fulcrum/m.key
    @internal remote_ip 10.0.0.0/8
    handle @internal { metrics }
    respond 403
}
```

★ ★ **取这条而不是「独立的 metrics 监听器」，是为了不长出第二个网络管理面。**
访问控制（`remote_ip` 匹配器）、TLS、访问日志、压缩**全部复用现有机制** ⇒
[G14「管理面只绑 Unix socket」的口径一个字不动](/platform/security-baseline.md) ——
指标面根本不属于管理面。

⛔ **不挂在 admin socket 上当唯一出口**：Prometheus 抓不了 Unix domain socket，
那样用户必须再装一个 exporter，直接撞设计原则 1（「任何让用户再装一个东西的设计都要被质疑」）。

⚠ **代价说在明处**：指标与业务共用监听器，**用户把 matcher 写错就会把指标暴露出去**。
这一条只能靠文档与诊断兜，**架构兜不住** —— 写下来是因为它不该在事后才被发现。

`outcome` 的闭集因此**多第 8 个值 `metrics`**。★ 闭集是穷尽 `match`，加一种就编不过。

## 取数点：与访问日志**同一段代码**

⚠ ⚠ ★ **计数器只能加在 [`access_log::Record`](../../crates/fulcrum-server/src/access_log.rs)
收尾的那一处。**

两处各算一遍 `outcome` / `status` 的话迟早分家，而**分家的表现是两个数字都言之凿凿、却对不上** ——
这正是 D18 / G66 那条「让分家在结构上做不到」要防的形状。

⇒ 判据是一条**一致性门**：打一串已知形状的请求，`fulcrum_requests_total` 的增量必须与访问日志
里的行数逐条对得上。★ 它同时守住两边，而任何一边**单独**的断言都守不住这件事。

## 指标清单与基数

⚠ ⚠ **Prometheus 的经典坑是标签基数。这张表里每一格都要能说出上界，
而且那个上界必须由配置定、不由访问者定。**

| 指标 | 类型 | 标签 | 上界 |
|---|---|---|---|
| `fulcrum_requests_total` | counter | `site` `outcome` `status_class` `proto` | address 数 × 8 × 5 × 3 |
| `fulcrum_request_duration_seconds` | histogram | `site` `outcome` | 桶写死 |
| `fulcrum_upstream_inflight` | gauge | `upstream` | 配置定 |
| `fulcrum_upstream_healthy` | gauge | `upstream` | 配置定 |
| `fulcrum_cache_events_total` | counter | `event` | `hit`/`miss`/`stale`/`purge` |
| `fulcrum_cert_expiry_seconds` | gauge | `domain` | 配置定 |
| `fulcrum_acme_issue_total` | counter | `result` | `ok`/`fail`/`deferred` |
| `fulcrum_no_site_match_total` | counter | `host` | 见下（G118）|
| `fulcrum_build_info` | gauge(=1) | 版本等 | 1 |

⛔ **任何形态都不加 `uri` 标签。**

### `site` 那一格 = 请求**实际匹配到的那条地址字面量**（G121）

`site="a.example"` / `site="*.wild.example"` —— 通配符**折叠成它自己的字面量**。

⚠ **不能直接用 `host`**：通配符站点下 host 由请求方决定 ⇒ 一个 `*.example` 站点就能让 series
无限增长。★ **「已命中站点的请求，host 总是有界的」是错的，而它错得很像对的。**

⚠ 也不取「站点块的第一个地址」：那会把同一个块里的 `a.example` 与 `b.example` 混成一格，
而且**改地址的书写顺序就会让时序断裂** —— 一个在配置里完全看不出来的副作用。

★ 代价：通配符站点下各子域名合并成一格，「哪个租户在打我」这一格答不了。
**那个问题留给访问日志** —— 它有真的 `host`。

## D26 由 `no_site_match` 计数器结案（G118）

`host` 那一格**来自请求方、攻击者可控** ⇒ **只有出现在配置里的 host 才带真值**，
其余一律归 `host="<other>"`。⇒ 上界 = 站点 address 数 + 1，仍由配置定。

★ 这回答了 D26 真正想问的那句「谁在拿奇怪的 Host 打我」，
而**不必回答**「全局 `log` 与站点 `log` 是覆盖还是合并」那个更贵的配置面问题。

⚠ 代价写在明处：**只知道有多少、来自哪个已知 host，不知道具体是哪个未知 host。**
这是有意的 —— 不让外人往我们的内存与时序库里写任意字符串。

## 格式自研，零新依赖（G117）

Prometheus 的 text exposition 是行式纯文本，自己写约百行。
★ 它**不是安全敏感协议栈**，不撞[安全基线](/platform/security-baseline.md)第 5 条
（那条管的是 TLS / HPACK / QUIC 这类）。零新依赖 ⇒ 供应链门不用动，musl 静态产物也不受影响。
⚠ 代价：直方图分桶、标签值转义、`_total` 后缀这些细节要自己钉住，由单测守。

## 批 M 的门

新场景 `tests/metrics/run.sh`（端口段 9920–9921）。除格式与访问控制外，**三条反向缺一不可**：

1. 没写 `metrics` 的站点上，同一路径**必须不是**指标 —— 否则「抓到了指标」证明不了是这条指令干的；
2. 用未知 `Host` 打 50 次之后 series 数**不增长** —— 守 G118 那条封顶；
3. **一致性门**：`fulcrum_requests_total` 的增量与访问日志行数逐条对得上 —— 守「两处不分家」。

# Runtime 实时 stats（三件套的第三件，**批 N**）

**批 N 的顺序 = G8 增量通道 → G18 临时覆盖层 → 只读 stats（G119）。**
⚠ owner 拍的是这个顺序（AI 推荐的是「先只读 stats、写通道留后」）⇒
stats **从第一天就带 `overrides` 一节**，不存在「先发一个没有覆盖层的 stats、以后再补」的中间形态。

## 增量通道（G8 的另一半）

admin socket 上 `POST /runtime`，收的是**动词**而不是一份配置：`set_weight` · `disable` · `enable`。
⚠ ⚠ **不写盘。** 期望状态是唯一权威，[管理面](/architecture/control-plane.md)**不许出现第二条持久写路径**。

## `POST /load` 与覆盖层：`overrides` 是**必填**参数（G120）

载荷必须带 `"overrides": "keep" | "clear"`，**缺了就拒绝**。

★ ★ **不给默认值就是这条的全部内容**，因为两种运维现实都正确且互相冲突：

| 谁在调用 | 想要什么 |
|---|---|
| 发布流水线 | `clear` —— 发布就是回到期望状态 |
| 事故处理中的人 | `keep` —— 一次无关的配置发布不该把刚摘掉的坏节点放回去 |

⇒ **任何一个默认值都会在另一种场景里悄悄做错事。**
★ 与「`POST /load` 遇到监听端口变化时**显式拒绝**」同一条纪律：
**判据在拒绝上，不在尽力而为上。**

⚠ `clear` 那一档必须在回话里**逐项列出**被清掉的覆盖 ——
[§3](../../PLAN.md) 点名要避开 HAProxy 那个「runtime 改动 reload 后无声消失」的短处。

## 只读 stats

`GET /stats`（admin socket），JSON：世代标识与配置装载时间 · 每上游的 `inflight` 与 `healthy` ·
证书到期 · 缓存占用 · **`overrides` 清单**。

★ `inflight` 与 `healthy` 这两个量在
[`Upstream`](../../crates/fulcrum-runtime/src/lib.rs) 上**已经有了**（`AtomicUsize` 与 `AtomicBool`）——
这正是 G16 那句「边际成本最低」的实处，不是修辞。

⚠ ⚠ **与 `/metrics` 同源**：两个出口从**同一组原子量**读，不各算一遍 ——
否则就是本页上面刚点过名的那个形状。

⚠ G18 的原话是「stats 与 API 的**每一次响应**都携带」覆盖层清单
⇒ 它**不是一个单独的端点**，见下面「最容易在哪做错」第 1 条。

# 三件套的进度要分开数

⚠ ⚠ **批 L 做完之后观测三件套是 1/3，不是 ✅。** 指标（Prometheus）与
Runtime stats 都不在批 L 里，全仓 `prometheus` 至今零命中。
⚠ **一格里装三件事，最容易被读成一件。**

★ 另外两件现在**各有一批**：指标＝**批 M**，Runtime stats＝**批 N**（见上面两节）。
⚠ **「方案定了」与「做完了」仍然是两件事** —— 本页上面写的是方案，`prometheus` 依然零命中。

# ★ 为什么 Runtime stats 是边际成本最低的高价值功能

Runtime stats 是 HAProxy `show stat` 的对应物。它**复用 G8 已经要做的 Runtime 通道**——通道本来就要建，在其上加一个只读端点几乎不增加工作量。

★ 而它承载的是 G18 的强制要求：**临时覆盖层必须永远可见**。这不是一个可选的观测特性，是状态模型的一部分。见 [管理面](/architecture/control-plane.md)。

# ~~回落层要计数~~ ⛔ **这一节已经没有对象了**

原文要求「哪些请求走了回落，必须在 stats 中计数」，并把**回落计数归零**当作
M2 退出条件「回落层已无常态使用」的判据。

⚠ **G98 把[回落层](/architecture/fallback.md)整块删掉之后，这条判据连被测对象都不存在了** ——
而「回落层已无常态使用」也因此**恒真**（层不在，就不可能有常态使用），见 `PLAN.md` §7 里
把退出条件拆成两半的那张表。
★ 留着这一节而不是删掉，是因为**一条读起来仍然成立、而对象已经消失的要求，比一条明显过时的要求更难发现**。

# 首版不做

**OpenTelemetry tracing**（G23 范围之外）。

Web UI 也不做——★ **G16 的 Prometheus 指标接 Grafana 已覆盖可视化需求**，而 nginx、HAProxy、Caddy 三家都没有官方 UI。

# 最容易在哪做错

1. ★ **把临时覆盖层清单做成一个需要单独查询的端点。** G18 的原话是「stats 与 API 的**每一次响应**都携带」。见 [管理面](/architecture/control-plane.md) 的易错点 2。
2. **私钥、ACME 凭据、上游认证信息进了普通日志。** [安全基线](/platform/security-baseline.md) 明令禁止，配置预览也必须脱敏。

# 相关

[管理面](/architecture/control-plane.md) · [回落层](/architecture/fallback.md) · [安全基线](/platform/security-baseline.md)
