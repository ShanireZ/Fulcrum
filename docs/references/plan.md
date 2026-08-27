---
type: 权威文档
title: PLAN.md · 唯一权威
description: 枢衡的定位、范围、里程碑、验收标准、风险、§10 决策清单与 §11 待定清单。
resource: ../../PLAN.md
tags: [权威, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: ../../PLAN.md
    title: PLAN.md 全文
---

★ ★ **[`PLAN.md`](../../PLAN.md) 是枢衡的唯一权威。** 本 bundle 与它冲突时，一律以它为准，并且要把 bundle 那一处改掉。

# 章节地图

| 章节 | 内容 | bundle 里的导航 |
|---|---|---|
| §1 | 当前状态 | [实施路线](/governance/roadmap.md) |
| §2 | 项目定位 | [定位](/product/positioning.md) |
| §3 | 要解决的问题；§3.1 竞争位置 | [定位](/product/positioning.md) |
| §4 | 设计原则（七条）| [设计原则](/product/design-principles.md) |
| §5 | 技术底盘；**§5.1 三条硬约束** | [技术栈](/platform/tech-stack.md) · [决策日志](/governance/decision-log.md) |
| §6 | 首版范围（§6.1 自研 / §6.2 不做 / §6.3 回落）| [首版范围](/product/scope.md) · [回落层](/architecture/fallback.md) |
| §7 | 里程碑 M0–M4 与退出条件 | [实施路线](/governance/roadmap.md) |
| §8 | 性能验收标准 | [性能验收标准](/verification/performance-bar.md) |
| §9 | 主要风险 | [尚未验证的接缝](/verification/open-seams.md) · [供应链现状](/platform/supply-chain.md) |
| **§10** | ★ **决策日志** | [决策日志](/governance/decision-log.md) |
| **§11** | ★ **待定清单 D2–D12** | [待定清单](/governance/open-questions.md) |
| §12 | 官方技术入口 | [上游技术入口](/references/upstream.md) |

# 引用它的时候

- **引 G 编号与章节号，不要引本 bundle。** bundle 是导航，会跟着重构变；`PLAN.md` 的编号是稳定标识。
- ★ **正文与决策日志冲突时，以决策日志为准。**
- ★ **§11 待定项不是需求。** 没拍板之前不得据以实现。

# 两条容易读错的地方

1. ★ **G5 的「全量」指对外能力面，不是自研代码面。** 不先读这条，§6 整章都会读错。见 [首版范围](/product/scope.md)。
2. ★ **G29 同时是 D1 的结案。** 读到「依赖版本策略待定」这类旧表述时，它是过期的。见 [依赖策略](/platform/dependency-policy.md)。
