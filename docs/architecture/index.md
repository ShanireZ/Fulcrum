---
type: 架構基線
title: 架構
description: 樞衡的技術基線——原 docs/architecture.md 拆分而成；帶真內容，服從 PLAN.md。
resource: ../../PLAN.md
tags: [架構, 索引, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md §5 技術底盤、§5.1 三條硬約束、§10 決策清單
---

★ ★ **本節是本 bundle 唯一帶真內容的部分。**

下面這些是**技術基線**，不是 `PLAN.md` 的複述 —— 那些內容 `PLAN.md` 裡沒有第二份。定位仍然是：**服從 [`PLAN.md`](../../PLAN.md)，衝突時以 `PLAN.md` 為準**。詳見 [唯一權威與本 bundle 的定位](/governance/source-of-truth.md)。

括號中的 `G<n>` 指向 `PLAN.md` §10 決策清單。

* [處理程序與元件邊界](/architecture/process-model.md) - 樞衡是**一個處理程序**，既是控制面也是資料面（G2 / G9）
* [設定分層](/architecture/configuration.md) - DSL → 結構化設定 → 執行環境；★ 不做反向生成（G4 / G11 / G20）
* [DSL 指令集參考](/architecture/dsl-reference.md) - ★ M1 的完整指令清單、匹配器、佔位符、預設值；**執行順序表是公開契約**（G49 / G60–G63）
* [管理面](/architecture/control-plane.md) - 雙通道；★ 生效狀態 = 期望狀態 ⊕ 臨時覆蓋層（G8 / G14 / G18）
* [TLS 與自動 HTTPS](/architecture/tls.md) - ⚠ ★ **此後後端是 BoringSSL 不是 rustls**（G104 推翻 §5.1 第 1 條），動態證書走 `select_certificate_callback`；不留雙路徑、On-Demand 強制存取資格（G6 / G12 / G15）
* [資料路徑](/architecture/data-path.md) - 三個入口一個路由層；靜態與快取；上游發現（G6 / G7 / G17）
* [回落層](/architecture/fallback.md) - 過渡期頂班，1.0 時歸零；四條約束（G2 / G9 / G25）
* [觀測](/architecture/observability.md) - 指標 + 結構化紀錄 + Runtime 即時 stats（G16）

# 原檔案另外兩節去了哪

| 原 `architecture.md` | 現在在 |
|---|---|
| §8 安全基線 | [安全基線](/platform/security-baseline.md) |
| §9 待驗證的接縫 | [尚未驗證的接縫](/verification/open-seams.md) - ★ 其中一條已由 M0 解除 |
