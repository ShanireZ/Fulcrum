---
type: 权威文档
title: AGENTS.md · 项目级 agent 约束
description: 在枢衡上工作的 agent 必须先读的文件；不含独立结论，是指向 PLAN.md 的指针加最易踩错的硬约束。
resource: ../../AGENTS.md
tags: [权威, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: agents
    resource: ../../AGENTS.md
    title: AGENTS.md 全文
---

[`AGENTS.md`](../../AGENTS.md) 是 agent 在本项目工作前要读的第一份文件。★ **它不含独立结论**——是指向 [`PLAN.md`](../../PLAN.md) 的指针，加上最容易踩错的几条硬约束。

按工作区约定（[`../AGENTS.md`](../../../AGENTS.md)），`AGENTS.md` 是**唯一**的 agent 指南文件名：**不要新增 `CLAUDE.md` / `GEMINI.md` / `.github/copilot-instructions.md`**，需要别的默认名的工具改配置去读 `AGENTS.md`。

# 它覆盖什么

| 一节 | 内容 | bundle 里的展开 |
|---|---|---|
| Project status | 项目定位、重定与改名 | [实施路线](/governance/roadmap.md) · [定位](/product/positioning.md) |
| Working rules | ★ 决策不得静默重开、三条硬约束、性能纪律、回落层约束、状态模型、依赖 | [工作方式](/governance/working-agreement.md) |
| Building and testing | Docker、M0 命令、dep-check | [构建与验证](/platform/build-and-test.md) · [依赖策略](/platform/dependency-policy.md) |
| Current validation | 交接前要做的三件事 | [工作方式](/governance/working-agreement.md) |

# 与本 bundle 的关系

`AGENTS.md` 是**入口**，本 bundle 是**展开**。两者都不是权威——权威是 `PLAN.md`。

★ **改了本 bundle 里的硬约束表述，要回头看 `AGENTS.md` 有没有说着相反的话**，反之亦然。这是工作区反复出现的一类漂移。
