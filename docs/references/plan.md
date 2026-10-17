---
type: 權威文件
title: PLAN.md · 唯一權威
description: 樞衡的定位、範圍、里程碑、驗收標準、風險、§10 決策清單與 §11 待定清單。
resource: ../../PLAN.md
tags: [權威, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: ../../PLAN.md
    title: PLAN.md 全文
---

★ ★ **[`PLAN.md`](../../PLAN.md) 是樞衡的唯一權威。** 本 bundle 與它衝突時，一律以它為準，並且要把 bundle 那一處改掉。

# 章節地圖

| 章節 | 內容 | bundle 裡的導覽 |
|---|---|---|
| §1 | 當前狀態 | [直接閱讀唯一權威](../../PLAN.md#1-当前状态)；bundle 不複製當前狀態 |
| §2 | 專案定位 | [定位](/product/positioning.md) |
| §3 | 要解決的問題；§3.1 競爭位置 | [定位](/product/positioning.md) |
| §4 | 設計原則（七條）| [設計原則](/product/design-principles.md) |
| §5 | 技術底盤；**§5.1 三條硬約束** | [技術棧](/platform/tech-stack.md) · [決策紀錄](/governance/decision-log.md) |
| §6 | 首版範圍（§6.1 自研 / §6.2 不做 / §6.3 回落）| [首版範圍](/product/scope.md) · [回落層](/architecture/fallback.md) |
| §7 | 里程碑 M0–M4 與退出條件 | [實施路線](/governance/roadmap.md) |
| §8 | 效能驗收標準 | [效能驗收標準](/verification/performance-bar.md) |
| §9 | 主要風險 | [尚未驗證的接縫](/verification/open-seams.md) · [供應鏈現狀](/platform/supply-chain.md) |
| **§10** | ★ **決策紀錄** | [決策紀錄](/governance/decision-log.md) |
| **§11** | ★ **待定清單**（⛔ 這裡有意不抄 D 號範圍：它只列仍然開著的，抄一個範圍過來就會隨著結案悄悄變成假話）| [待定清單](/governance/open-questions.md) |
| §12 | 官方技術入口 | [上游技術入口](/references/upstream.md) |

# 引用它的時候

- **引 G 編號與章節號，不要引本 bundle。** bundle 是導覽，會跟著重構變；`PLAN.md` 的編號是穩定標識。
- ★ **正文與決策紀錄衝突時，以決策紀錄為準。**
- ★ **§11 待定項不是需求。** 沒拍板之前不得據以實作。

# 兩條容易讀錯的地方

1. ★ **G5 的「全量」指對外能力面，不是自研程式碼面。** 不先讀這條，§6 整章都會讀錯。見 [首版範圍](/product/scope.md)。
2. ★ **G29 同時是 D1 的結案。** 讀到「相依版本策略待定」這類舊表述時，它是過期的。見 [相依策略](/platform/dependency-policy.md)。
