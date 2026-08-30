---
type: 架构基线
title: 管理面：双通道与状态模型
description: 全量原子 load + Runtime 增量双通道；生效状态 = 期望状态 ⊕ 临时覆盖层，覆盖层不持久化但永远可见。
resource: ../../PLAN.md
tags: [架构, 承重墙, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G8（双通道）、G14（管理面安全）、G18（Runtime 归宿与判据）
  - id: plan-4
    resource: /references/plan.md
    title: PLAN.md §4 设计原则第 3 条与第 4 条
---

★ 本页属**技术基线**，带真内容。它**服从** [`PLAN.md`](../../PLAN.md)；冲突时以 `PLAN.md` 为准。

# 两条通道（G8）

直取两家长处：全量 load 保证「整份配置即唯一事实」与原子回退（Caddy），增量 runtime 用于改权重、摘节点、清缓存且零 reload（HAProxy）。

| 通道 | 用途 | 语义 |
|---|---|---|
| **全量 load** | 提交完整配置 | **原子生效**；任一校验或健康检查失败则**整体回退**到上一版本 |
| **Runtime 增量** | 改权重、摘/加节点、清缓存、导出 stats | 立即生效，零 reload，零连接中断 |

# 状态模型：基线 ⊕ 临时覆盖层（G18）

这是 G8 埋下的一致性问题的解法，也是枢衡与 HAProxy 的**关键分野**。

```text
生效状态 = 期望状态（基线，来自全量 load）
         ⊕ 临时覆盖层（来自 Runtime 通道）
```

- **期望状态是唯一权威。** 由全量 load 写入，可持久化、可 diff、可回滚。
- **临时覆盖层不持久化。** 进程重启之后它被清空；全量 load 之后清不清空由
  `overrides` 参数定（**必填**、走查询串，`POST /load?overrides=keep|clear`，
  G120，**批 N 任务 5**）：`clear` 才清空且逐项列出被清掉的，`keep` 原样保留——
  键落不到任何上游的那些标成**悬空**（仍算生效中）并逐条点名，不是无声无息
  地消失。
- ★ **但它永远可见。** stats 与 API 的**每一次响应**都携带「当前有 N 项临时覆盖生效中」，并可逐项列出（谁、什么时候、改了什么）。

> ★ **判据**：HAProxy 的病根**不是** runtime 改动会在 reload 后消失，而是**它消失得无声无息**。
> 问题的本质是「可见与否」，不是「持久化与否」。枢衡选择**不持久化 + 强制可见**。

# 暴露面（G14）

- **默认只绑 Unix domain socket**，权限交给文件系统 ACL。★ **零网络暴露，不需要发明 token 体系。**
- 远程管理需**显式开启**，且只能走 **mTLS**。
- ★ 这是对 Caddy 的直接修正：**Caddy 的 Admin API 默认绑回环且无认证，同机任意进程可改配置。**

# 最容易在哪做错

1. ★ ★ **新增第二条持久化写路径。** 「Runtime 改了权重，顺手存一下下次也生效」看起来体贴，实际直接摧毁「期望状态是唯一权威」——两份权威分岔时没人知道该信哪个。[设计原则](/product/design-principles.md) 第 3 条就是拿来否决它的。
2. ★ **把覆盖层做成「可见」但不是每次响应都带。** 判据是**无声无息**才是病根；一个要另外调接口才能查到的覆盖清单，实际效果与 HAProxy 相同——**运维不会去查他不知道存在的东西**。
3. **全量 load 做成「尽力而为」。** 部分生效 + 部分失败是最坏的形态：它既不是旧状态也不是新状态。第 4 条原则「配置变更是事务，不是文件写入」就是拿来否决它的。

# 相关

[配置分层](/architecture/configuration.md)（装载的是什么） · [观测](/architecture/observability.md)（Runtime stats 复用同一条通道） · [安全基线](/platform/security-baseline.md)
