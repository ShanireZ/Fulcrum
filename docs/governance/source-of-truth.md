---
type: 專案治理
title: 唯一權威與本 bundle 的定位
description: PLAN.md 是樞衡的唯一權威；本 bundle 主體是導覽圖，架構一節是唯一帶真內容的例外。
resource: ../../PLAN.md
tags: [治理, 防漂移, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md 卷首「本檔案是唯一權威」
  - id: agents
    resource: /references/agents.md
    title: AGENTS.md「Project status」與「Working rules」
---

# 誰說了算

[`PLAN.md`](../../PLAN.md) 是樞衡的產品定位、範圍、里程碑、驗收標準與決策記錄的**唯一權威**。

- **§10 決策清單是拍板結果的權威記錄。** 正文與決策紀錄衝突時，以決策紀錄為準。
- **§11 待定清單不是需求。** 沒拍板之前不得據以實作，也不得在別處當成已定事實複述。
- ★ **§10 的每一條都由 owner 逐項拍板，不得靜默重開。** 若某條看起來不對，**說出來並問**，不要繞過它實作（[`AGENTS.md`](../../AGENTS.md) Working rules 第一條）。

# 本 bundle 是什麼

**主體是導覽圖。** 每篇概念用 `sources` 宣告出處，正文只寫「這是什麼、和誰相連、最容易在哪做錯」，**結論本身去讀 `PLAN.md`**。

理由與成均專案相同：一份把權威的結論抄一遍的 `docs/` 遲早與權威分岔，而分岔時沒人知道該信哪個。

## ★ 一處例外：架構一節帶真內容

[架構](/architecture/index.md) 的七篇是原 `docs/architecture.md`（197 行）拆分而來。**那份檔案從來不是 `PLAN.md` 的複述，它是技術基線本身**——處理程序結構、設定分層、雙通道語義、TLS 路徑、資料路徑、回落層約束、觀測形態，這些內容 `PLAN.md` 裡沒有第二份。

所以這一節的定位與原檔案**完全一致**：

> 它**服從** `PLAN.md`；兩者衝突時以 `PLAN.md` 為準。

拆分只改變了它的組織方式（一個檔案 → 七篇帶 `sources` 的概念），**沒有改變它的權威層級，也沒有引入任何 `PLAN.md` 裡沒有的新判斷**。

# 發現衝突怎麼辦

**一律以 `PLAN.md` 為準，並且把本 bundle 那一處改掉。**

不要「兩邊都留著讓讀者自己判斷」——那正是防漂移要消滅的狀態。

# 文件職責劃分

| 檔案 | 職責 | 是否權威 |
|---|---|---|
| [`PLAN.md`](../../PLAN.md) | 定位、範圍、里程碑、驗收標準、風險、§10 決策清單、§11 待定清單 | ★ **是** |
| [`AGENTS.md`](../../AGENTS.md) | agent 的專案級約束與最易踩錯的硬約束 | 指針，不含獨立結論 |
| [`README.md`](../../README.md) | 穩定的對外介紹與權威導覽 | 指針，不復制當前里程碑狀態 |
| `docs/architecture/`（本 bundle） | ★ **技術基線** | 服從 `PLAN.md` |
| `docs/` 其餘部分（本 bundle） | 概念導覽、關係圖、易錯點索引 | **否** |
| `docs/verification/` | 已跑過的驗證及其原始證據 | ★ **是**（歷史事實） |

★ **`docs/verification/` 與其他章節不同**：它記的是**實際發生過的測試與觀測到的輸出**，那是事實不是轉述。結論可以被後續實驗推翻，但「那一次跑出了什麼」不會變。

# 一條易錯點

★ **狀態類句子最容易漂。** 當前里程碑、已完成/進行中/未開工與剩餘項只在 `PLAN.md` §1 維護。README、AGENTS、roadmap 與工作區專案索引只保留穩定身分、約束和到 `PLAN.md` 的導覽，不建立需要同步的狀態副本。

本專案曾在 M0 狀態變化後留下過過期 README 徽章與正文。該歷史說明了為什麼要消除副本，而不是繼續要求跨檔案同步；帶日期和上下文的實測結果繼續留在 `docs/verification/`。
