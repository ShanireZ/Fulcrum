---
type: 产品定位
title: 首版范围
description: 自研四块全部做、明确不做七项、未完成的能力过渡期回落给两家引擎。
resource: ../../PLAN.md
tags: [范围, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-6
    resource: /references/plan.md
    title: PLAN.md §6 首版范围（§6.1 自研 / §6.2 不做 / §6.3 回落层）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G3、G5、G7、G23、G25
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §6。**

# ★ 先分清「全量」指的是什么

G5 定的口径，理解错了整章都会读错：

> **「三合一全量」= 对外能力面全量，自研逐块吞并。**

用户看到的是完整的三合一；**内部自研哪块算哪块，其余回落**。所以 G3 的「全量首版」**不是**说第一版就要把所有代码自己写完。

# 自研（四块全部，G7）

- **L7 反代核心**（产品本体）：HTTP/1.1、HTTP/2、TLS 终止、WebSocket、gRPC —— 后三者 Pingora 已覆盖
- **L4 面**：TCP/UDP 透传、SNI/ALPN 分流、PROXY protocol
- **静态文件服务**：range、ETag、压缩与预压缩、目录索引
- **HTTP 缓存磁盘后端**：实现 Pingora 的 `Storage` 与 `EvictionManager` —— ★ **开源版只带内存实现**
- **HTTP/3 / QUIC**：⚠ ~~`quinn` + `h3`~~ → **`quiche`**（**G103**）独立入口，在枢衡自己的路由层与 h1/h2 汇合。★ **「自研」指入口、连接归属、路由汇合与中间件链；协议栈本体不自研**（G105 —— 手写 QPACK 违反[安全基线](/platform/security-baseline.md)第 5 条）

技术落点见 [数据路径](/architecture/data-path.md)。

# 明确不做（七项）

| 不做 | 依据 |
|---|---|
| Web UI | G23——管理面只有 DSL + CLI + HTTP API；Prometheus 接 Grafana 已覆盖可视化，且三家都没有官方 UI |
| 服务发现集成（Consul / etcd / Docker labels） | |
| Kubernetes Endpoint 监听与 Gateway API 实现 | ★ 与 [定位](/product/positioning.md) 里「非 K8s 环境」的空位一致 |
| OpenTelemetry tracing | |
| Windows / macOS 支持 | G13——目标平台只有 Linux |
| 多节点控制平面、RBAC、审批流 | |
| 官方容器镜像 | ★ G13——单静态二进制，文档给一份 `FROM scratch` Dockerfile 代替 |

# 回落层（过渡期，1.0 时归零）

| 能力 | 过渡期回落给 | 理由 |
|---|---|---|
| 静态文件、HTTP 缓存 | **Nginx** | 这两块 nginx 最强，过渡期性能表现更体面 |
| 其余未完成能力 | **Caddy** | 能力最全，Admin API 最好对接，机队现状本就是 Caddy |

★ **代价已知并接受（G25）**：回落两家而非一家，回落层工程量**约翻倍**，而这些代码 1.0 时**全部删除**。换来的是过渡期不必向自己和早期用户解释难看的性能数字。

设计约束与拆除节奏见 [回落层](/architecture/fallback.md)。

# ★ 范围与里程碑不是一回事

本页说的是**首版对外能力面**。什么时候真正做出来、按什么顺序拆掉回落，在 [实施路线](/governance/roadmap.md)：M1 只接管一台机，L4 / 静态 / 磁盘缓存 / HTTP-3 要到 M2 才依次上线并逐块拆除回落。
