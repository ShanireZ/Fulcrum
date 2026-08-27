---
type: 技术基线
title: 安全基线
description: 管理面默认不出机器、危险能力默认要求显式准入、协议栈一律交给经过审计的库。
resource: ../../PLAN.md
tags: [安全, 承重墙, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G14（管理面安全）、G15（On-Demand 准入强制）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 风险表（单人 + AI 承担网络关键路径软件的安全责任）
  - id: plan-4
    resource: /references/plan.md
    title: PLAN.md §4 设计原则第 5 条（默认即安全）
---

★ 本页承接原 `docs/architecture.md` §8，属**技术基线**。它**服从** [`PLAN.md`](../../PLAN.md)。

# 基线七条

1. **管理面默认不出机器** —— 只绑 Unix domain socket，权限交给文件系统 ACL；远程管理需显式开启且只能走 mTLS（G14）→ [管理面](/architecture/control-plane.md)
2. **危险能力默认要求显式准入配置** —— On-Demand TLS 未配准入来源则**启动失败**（G15）→ [TLS](/architecture/tls.md)
3. **私钥、ACME 凭据、上游认证信息不进普通日志**；★ **配置预览必须脱敏**
4. **最小文件权限与进程权限** —— 不需要长期 root（仅绑定特权端口时用 capability）
5. ★ ★ **不自己实现 TLS、HTTP/2 状态机、HPACK、QUIC** —— 全部交给经过审计的成熟库
6. **变更审计**：身份、版本、时间、结果
7. **回落层不静态链接任何一家** —— 仅通过进程边界与 API 交互（也是许可证考虑）→ [回落层](/architecture/fallback.md)

# ★ 第 5 条不是风格偏好

`PLAN.md` §9 把「**单人 + AI 承担网络关键路径软件的安全责任**」列为主要风险，而缓解手段只有三条：

- 内存安全交给 **Rust**
- 协议栈交给**成熟库**
- ★ **不自己实现上面那四样**

这是整个项目安全论证的支点。任何「自己写一个小的 HPACK 实现就够了」的提议都直接违反它。

# ★ 第 3 条在 On-Demand 场景下尤其重要

G15 之外还有三道保险：**签发速率上限、失败退避、指标告警**。指标会暴露域名——★ 设计指标标签时要想清楚**哪些域名信息该进 Prometheus**，多租户场景下这本身就是信息泄露面。

# ★  复审：M0 spike 上的三条，两条已修、一条留给 M1

spike 是验证台不是产品，但它是 M1 的模板——**在模板上留下的坏习惯会被照抄**。

| | 情况 | 处置 |
|---|---|---|
| **UDP 回声是反射面** | 源地址可伪造 → 把流量反射给第三方（放大比 1:1，但反射是真的）。★ 当前**不可达**：`docker run` 没有 `-p`，端口只在容器网络内——但那是**部署方式**在兜底，不是服务本身安全 | ✅ 已改：默认绑 `127.0.0.1`，要放开必须显式 `M0_BIND_HOST=0.0.0.0`。**显式比默认危险要好**（第 5 条「默认即安全」）。★ M1 的 L4 UDP 要从第一天带源限制与速率上限 |
| **没有读超时** | 连上来什么都不发（或每 29 秒滴一个字节）的对端会永久占住一个任务——慢速连接耗尽最基本的形态 | ✅ 已改：TCP 回声 30 秒空闲超时；HTTP 读头 15 秒**总预算**（★ 不是每次 `read` 的预算，后者挡不住「慢慢滴」） |
| **fd 耗尽时 accept 循环满速空转** | 实测：无退避时 **369,156 条日志/秒**，3 秒写了一百多万行；有退避时 1 条/秒 | ✅ 已改：EMFILE/ENFILE 退避 1 秒，与 pingora 自己 accept 循环的做法一致。★ **这是可用性事故，不是噪音**——它把「fd 快用完了」放大成「机器废了」，且恰好发生在资源最紧张的时候 |
| **容器以 root 跑** | Windows 上无所谓；Linux CI 上会在工作树里留下 root 属主的 `run/` 等产物 | ⏳ 留了口子未启用：`DOCKER_USER="$(id -u):$(id -g)"`。★ **没有在 Linux 上实测过**，所以是口子不是默认——在 Windows 上传宿主机 uid 反而会让命名卷不可写、把构建搞坏 |

# 供应链

依赖是安全基线的一部分，但它有自己的策略与现状：

- **策略**（G29：追新 + 24 小时怀疑期）→ [依赖策略](/platform/dependency-policy.md)
- **现状**（44 项陈旧、4 条公告及可达性分析）→ ★ [供应链现状](/platform/supply-chain.md)

# 相关

[管理面](/architecture/control-plane.md) · [TLS](/architecture/tls.md) · [观测](/architecture/observability.md) · [供应链现状](/platform/supply-chain.md)
