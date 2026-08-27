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
| **指标** | Prometheus 端点；覆盖连接、请求、上游、TLS、缓存、证书签发 |
| **日志** | 结构化访问日志与错误日志；**字段清单与格式已由 G113 + G114 定稿**（D7 结案）—— 见下面「访问日志的字段契约」|
| **实时** | Runtime 通道上的**只读 stats**：每上游的实时连接数、队列、错误、健康态，以及**临时覆盖层清单** |

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

# 三件套的进度要分开数

⚠ ⚠ **批 L 做完之后观测三件套是 1/3，不是 ✅。** 指标（Prometheus）与
Runtime stats 都不在批 L 里，全仓 `prometheus` 至今零命中。
⚠ **一格里装三件事，最容易被读成一件。**

# ★ 为什么 Runtime stats 是边际成本最低的高价值功能

Runtime stats 是 HAProxy `show stat` 的对应物。它**复用 G8 已经要做的 Runtime 通道**——通道本来就要建，在其上加一个只读端点几乎不增加工作量。

★ 而它承载的是 G18 的强制要求：**临时覆盖层必须永远可见**。这不是一个可选的观测特性，是状态模型的一部分。见 [管理面](/architecture/control-plane.md)。

# 回落层要计数

[回落层](/architecture/fallback.md) 的第 2 条约束要求：**哪些请求走了回落，必须在 stats 中计数**。

★ 这条有实际用途：**回落计数归零是拆除该块回落的信号**，也是 M2 退出条件「回落层已无常态使用」的判据。

# 首版不做

**OpenTelemetry tracing**（G23 范围之外）。

Web UI 也不做——★ **G16 的 Prometheus 指标接 Grafana 已覆盖可视化需求**，而 nginx、HAProxy、Caddy 三家都没有官方 UI。

# 最容易在哪做错

1. ★ **把临时覆盖层清单做成一个需要单独查询的端点。** G18 的原话是「stats 与 API 的**每一次响应**都携带」。见 [管理面](/architecture/control-plane.md) 的易错点 2。
2. **私钥、ACME 凭据、上游认证信息进了普通日志。** [安全基线](/platform/security-baseline.md) 明令禁止，配置预览也必须脱敏。

# 相关

[管理面](/architecture/control-plane.md) · [回落层](/architecture/fallback.md) · [安全基线](/platform/security-baseline.md)
