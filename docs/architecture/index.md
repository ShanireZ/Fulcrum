---
type: 架构基线
title: 架构
description: 枢衡的技术基线——原 docs/architecture.md 拆分而成；带真内容，服从 PLAN.md。
resource: ../../PLAN.md
tags: [架构, 索引, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md §5 技术底盘、§5.1 三条硬约束、§10 决策清单
---

★ ★ **本节是本 bundle 唯一带真内容的部分。**

下面这些是**技术基线**，不是 `PLAN.md` 的复述 —— 那些内容 `PLAN.md` 里没有第二份。定位仍然是：**服从 [`PLAN.md`](../../PLAN.md)，冲突时以 `PLAN.md` 为准**。详见 [唯一权威与本 bundle 的定位](/governance/source-of-truth.md)。

括号中的 `G<n>` 指向 `PLAN.md` §10 决策清单。

* [进程与组件边界](/architecture/process-model.md) - 枢衡是**一个进程**，既是控制面也是数据面（G2 / G9）
* [配置分层](/architecture/configuration.md) - DSL → 结构化配置 → 运行时；★ 不做反向生成（G4 / G11 / G20）
* [DSL 指令集参考](/architecture/dsl-reference.md) - ★ M1 的完整指令清单、匹配器、占位符、默认值；**执行顺序表是公开契约**（G49 / G60–G63）
* [管理面](/architecture/control-plane.md) - 双通道；★ 生效状态 = 期望状态 ⊕ 临时覆盖层（G8 / G14 / G18）
* [TLS 与自动 HTTPS](/architecture/tls.md) - ⚠ ★ **此后后端是 BoringSSL 不是 rustls**（G104 推翻 §5.1 第 1 条），动态证书走 `select_certificate_callback`；不留双路径、On-Demand 强制准入（G6 / G12 / G15）
* [数据路径](/architecture/data-path.md) - 三个入口一个路由层；静态与缓存；上游发现（G6 / G7 / G17）
* [回落层](/architecture/fallback.md) - 过渡期顶班，1.0 时归零；四条约束（G2 / G9 / G25）
* [观测](/architecture/observability.md) - 指标 + 结构化日志 + Runtime 实时 stats（G16）

# 原文件另外两节去了哪

| 原 `architecture.md` | 现在在 |
|---|---|
| §8 安全基线 | [安全基线](/platform/security-baseline.md) |
| §9 待验证的接缝 | [尚未验证的接缝](/verification/open-seams.md) - ★ 其中一条已由 M0 解除 |
