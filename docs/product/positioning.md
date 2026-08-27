---
type: 产品定位
title: 定位
description: 一个 Rust 单静态二进制同时是 Web 服务器、反向代理与负载均衡器；空位真实但狭窄。
resource: ../../PLAN.md
tags: [定位, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-2
    resource: /references/plan.md
    title: PLAN.md §2 项目定位、§3 要解决的问题、§3.1 竞争位置
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G21（产品名）、G22（用户分层）
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §2–§3。**

# 一句话

> **一个 Rust 单静态二进制，同时是 Web 服务器、反向代理与负载均衡器；简单时像 Caddy，复杂时不必换软件。**

```text
今天：Caddy（自动 HTTPS）+ HAProxy（L4 与调度）+ Nginx（静态与缓存）
枢衡：一个进程、一份配置、一套观测
```

# 产品假设

三家各有所长也各有硬伤。枢衡赌的是两件事：

1. **这些长处不冲突，可以收进一个实现。**
2. **这些短处大多源自各自的历史包袱，不是必然代价。**

★ **每采纳一个三家已有的设计，必须能说清它避开了对应的哪个坑**（[设计原则](/product/design-principles.md) 第 2 条）。`PLAN.md` §3 的表格逐项列了长处与短处，是这条纪律的检查表。

几个被点名要消灭的坑：

| 坑 | 出自 | 枢衡怎么避 |
|---|---|---|
| 上游域名**只在启动时解析一次** | Nginx OSS | DNS 定期重解析（G17）→ [数据路径](/architecture/data-path.md) |
| runtime 改动 **reload 后无声消失** | HAProxy | 不持久化，但**强制可见**（G18）→ [管理面](/architecture/control-plane.md) |
| Admin API **默认绑回环且无认证** | Caddy | 默认只绑 Unix socket（G14）→ [安全基线](/platform/security-baseline.md) |
| 语义有坑的自创 DSL（`if` 不可靠、`location` 顺序反直觉） | Nginx | Caddyfile 式 DSL（G20）→ [配置分层](/architecture/configuration.md) |
| Go GC 导致高并发下 P99 尾延迟与内存劣化 | Caddy | Rust（G1）|

# ★ 竞争位置必须诚实记录

`PLAN.md` §3.1 专门立了一节，因为**这一格并不空**：Traefik、APISIX、Kong、Nginx Proxy Manager、Zoraxy，以及 Kubernetes Gateway API 及其多个实现都在附近。2024 年后还多了 Cloudflare 的 **Pingora**（枢衡自己的底座）和 ISRG 基于它的 **River**。

枢衡的空位是**真实但狭窄**的：

> **非 K8s 环境下，一个进程同时覆盖 Web 服务器 + 反代 + 负载均衡，且带自动 HTTPS 与原生管理 API。**

没有任何一款现有软件同时做到这三件事——**Pingora 是库不是产品，River 不做静态与缓存**。

★ **这一节的存在本身就是纪律**：它挡住的是「我们没有竞争者」这种在立项书里极常见、且几乎总是错的说法。

# 给谁用（G22）

| | 用户 | 待遇 |
|---|---|---|
| **第一类** | 自己的机队 **与** 自托管 / homelab 站长（★ **并列**主打） | 文档、默认值与迁移工具**优先服务这一类** |
| 第二类 | nginx 存量迁移用户、需要 L4+L7 混合的小团队 | |

# 名字（G21）

**枢衡 / Fulcrum。**

- 中文：「枢」是门轴与中枢（网关），「衡」是平衡（负载均衡）；古语「位居枢衡」指中枢要职。
- 英文：`Fulcrum` 是杠杆与天平的支点——**既是转动的中心（枢），又是平衡的基准（衡）**，是候选里唯一同时具备两层含义的词。
- ★ 已搜过：代理与负载均衡领域无同名项目。

# 一条已作废的说法

★ **凡把本项目描述为「统一编排 Caddy/HAProxy/Nginx 的控制面」的说法，一律作废。**

定位先前的六轮头脑风暴中彻底重定：**枢衡是一款自研数据面产品，不是多引擎控制面。** Caddy 与 Nginx 只在过渡期作为回落后端，1.0 时回落代码归零。
