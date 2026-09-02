---
type: 项目治理
title: 唯一权威与本 bundle 的定位
description: PLAN.md 是枢衡的唯一权威；本 bundle 主体是导航图，架构一节是唯一带真内容的例外。
resource: ../../PLAN.md
tags: [治理, 防漂移, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md 卷首「本文件是唯一权威」
  - id: agents
    resource: /references/agents.md
    title: AGENTS.md「Project status」与「Working rules」
---

# 谁说了算

[`PLAN.md`](../../PLAN.md) 是枢衡的产品定位、范围、里程碑、验收标准与决策记录的**唯一权威**。

- **§10 决策清单是拍板结果的权威记录。** 正文与决策日志冲突时，以决策日志为准。
- **§11 待定清单不是需求。** 没拍板之前不得据以实现，也不得在别处当成已定事实复述。
- ★ **§10 的每一条都由 owner 逐项拍板，不得静默重开。** 若某条看起来不对，**说出来并问**，不要绕过它实现（[`AGENTS.md`](../../AGENTS.md) Working rules 第一条）。

# 本 bundle 是什么

**主体是导航图。** 每篇概念用 `sources` 声明出处，正文只写「这是什么、和谁相连、最容易在哪做错」，**结论本身去读 `PLAN.md`**。

理由与成均项目相同：一份把权威的结论抄一遍的 `docs/` 迟早与权威分岔，而分岔时没人知道该信哪个。

## ★ 一处例外：架构一节带真内容

[架构](/architecture/index.md) 的七篇是原 `docs/architecture.md`（197 行）拆分而来。**那份文件从来不是 `PLAN.md` 的复述，它是技术基线本身**——进程结构、配置分层、双通道语义、TLS 路径、数据路径、回落层约束、观测形态，这些内容 `PLAN.md` 里没有第二份。

所以这一节的定位与原文件**完全一致**：

> 它**服从** `PLAN.md`；两者冲突时以 `PLAN.md` 为准。

拆分只改变了它的组织方式（一个文件 → 七篇带 `sources` 的概念），**没有改变它的权威层级，也没有引入任何 `PLAN.md` 里没有的新判断**。

# 发现冲突怎么办

**一律以 `PLAN.md` 为准，并且把本 bundle 那一处改掉。**

不要「两边都留着让读者自己判断」——那正是防漂移要消灭的状态。

# 文档职责划分

| 文件 | 职责 | 是否权威 |
|---|---|---|
| [`PLAN.md`](../../PLAN.md) | 定位、范围、里程碑、验收标准、风险、§10 决策清单、§11 待定清单 | ★ **是** |
| [`AGENTS.md`](../../AGENTS.md) | agent 的项目级约束与最易踩错的硬约束 | 指针，不含独立结论 |
| [`README.md`](../../README.md) | 稳定的对外介绍与权威导航 | 指针，不复制当前里程碑状态 |
| `docs/architecture/`（本 bundle） | ★ **技术基线** | 服从 `PLAN.md` |
| `docs/` 其余部分（本 bundle） | 概念导航、关系图、易错点索引 | **否** |
| `docs/verification/` | 已跑过的验证及其原始证据 | ★ **是**（历史事实） |

★ **`docs/verification/` 与其他章节不同**：它记的是**实际发生过的测试与观测到的输出**，那是事实不是转述。结论可以被后续实验推翻，但「那一次跑出了什么」不会变。

# 一条易错点

★ **状态类句子最容易漂。** 当前里程碑、已完成/进行中/未开工与剩余项只在 `PLAN.md` §1 维护。README、AGENTS、roadmap 与工作区项目索引只保留稳定身份、约束和到 `PLAN.md` 的导航，不建立需要同步的状态副本。

本项目曾在 M0 状态变化后留下过过期 README 徽章与正文。该历史说明了为什么要消除副本，而不是继续要求跨文件同步；带日期和上下文的实测结果继续留在 `docs/verification/`。
