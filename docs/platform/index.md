---
type: 技术基线
title: 技术与运维
description: 选了什么、依赖怎么管、供应链现状如何、怎么构建、安全底线在哪。
resource: ../../Cargo.toml
tags: [技术, 索引]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md §5 技术底盘、§10 G26 与 G29
---

* [技术栈](/platform/tech-stack.md) - Rust + Pingora 0.8.1 + **quiche** + **BoringSSL**（G103/G104）；★ 两处决策与清单之间的差距
* [依赖策略](/platform/dependency-policy.md) - G29；★ **绝不裸跑 `cargo update`**；★ dep-check 看不见被上游上界卡住的东西
* [供应链现状](/platform/supply-chain.md) - ★ **必读**：脱字号需求自带上界、pingora 的十条天花板、4 条公告与可达性分析
* [HTTP/3 库选型事实表](/platform/http3-libraries.md) - D11 拍板前摆出来的事实，**不给推荐项**；★ quiche 用 BoringSSL 不是 rustls、s2n-quic 根本不带 HTTP/3、`h3` 是「没发版」不是「没人开发」
* [向上游提 PR 的流程清单](/platform/upstream-pr.md) - G32；★ **必须先开 issue**；★ MSRV 1.85.0 正好卡住 `lru 0.18.2`；★ 上游改面比枢衡 fork 大得多
* [构建与验证](/platform/build-and-test.md) - 一切在 Docker 里跑；★ 三个 Windows 宿主机上的坑
* [宿主机陷阱、门禁纪律、场景开关与端口表](/platform/host-and-gate-traps.md) - 从 `AGENTS.md` 拆出来的细则，**仍然逐条是硬规矩**；★ 绿了也可能什么都不说明的五种形态；★ 加场景要同时改三处；★ 端口分配表
* [部署](/platform/deploy.md) - G31/G33/G37/G78 的落地形状；★ **`TimeoutStopSec` 小于停机预算＝硬杀连接**；★ 换二进制必须 rename
* [安全基线](/platform/security-baseline.md) - 七条；★ 第 5 条（不手写协议栈）是整个安全论证的支点
