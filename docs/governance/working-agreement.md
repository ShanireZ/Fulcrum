---
type: 專案治理
title: 工作方式與不得繞過的約束
description: 在樞衡上工作時必須遵守的紀律——決策不得靜默重開、效能不得無據宣稱、協定棧不得手寫。
resource: ../../AGENTS.md
tags: [治理, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: agents
    resource: /references/agents.md
    title: AGENTS.md「Working rules」
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G6（三條硬約束）、G19（效能驗收）、G29（相依策略）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 主要風險（單人 + AI 承擔網路關鍵路徑的安全責任）
---

★ 權威表述在 [`AGENTS.md`](../../AGENTS.md) 與 [`PLAN.md`](../../PLAN.md) §10。本頁把散落各處的紀律收在一起。

# 決策

**§10 的每一條都由 owner 逐項拍板，不得靜默重開。**

若某條看起來不對：**說出來並問**。不要繞過它實作，也不要「先按自己的想法做、回頭再解釋」。

★ **owner 推翻了 AI 推薦項的那些決定**尤其如此——它們**看起來**像是 AI 建議更優，但每一條 owner 都給了理由，而且 G29 是在 AI 明確提示代價之後**同日重申**的。逐條見 [決策紀錄](/governance/decision-log.md)。

> ★ **不要在這裡寫「一共有幾條」。** 那個數字曾經被抄進五處，而新增決定的那一天五處**同時**過期。
> **副本的數量就是它過期時要修的處數。**

# 三條鎖死的技術約束

來自 G6，完整表述見 `PLAN.md` §5.1：

1. **TLS 後端是 rustls。** 動態證書選擇走 `ResolvesServerCert`，**不是** `certificate_callback`——rustls 後端不支援它。自動 HTTPS 與 On-Demand TLS 都建在這條路上，**不留雙路徑**。
2. **tower 中介軟體不與 Pingora 的 `ProxyHttp` 階段模型複合。** 同名不同物，插不進去。
3. **自建 QUIC 與 L4 監聽器必須接入 Pingora 的 socket 移交。** ✅ 已由 M0 證明可行——★ 但那是「可以做到」，不是「自動發生」：自建 `Service` **有義務**自己取走 fd 並放回去。

# 效能

★ **絕不在沒有可重現的端到端實測時宣稱效能。**

- 三家（Caddy / HAProxy / Nginx）**全部對拍**，不挑軟柿子
- 逐類設門：不劣於**該類最強者** 10%
- 指令碼與**原始資料**全部公開
- ★ **不使用「快 N 倍」式表述**

詳見 [效能驗收標準](/verification/performance-bar.md)。

# 安全

★ **絕不手寫 TLS、HTTP/2 狀態機、HPACK 或 QUIC。** 全部交給經過稽核的成熟庫。

這條不是風格偏好。`PLAN.md` §9 把「單人 + AI 承擔網路關鍵路徑軟體的安全責任」列為主要風險，而緩解手段只有三條：記憶體安全交給 Rust、協定棧交給成熟庫、**不自己實作上面那四樣**。

# 回落層

**它是腳手架，不是架構。成功標誌是被刪除。**

- 只做**薄轉發**：不做能力翻譯、不做設定語義映射、不試圖統一兩家的行為差異
- **不靜態連結** Caddy 或 Nginx，僅通過處理程序邊界與 API 交互（也是授權條款考慮）
- 一塊自研完成，**立即拆除對應回落**

詳見 [回落層](/architecture/fallback.md)。

# 狀態模型

**期望狀態是唯一權威。** Runtime 臨時覆蓋不持久化，但**必須永遠可見**於 stats 與 API 輸出。

★ **不要新增第二條持久化寫路徑。** G18 的判據是：HAProxy 的病根不是 runtime 改動會丟，而是**丟得無聲無息**——問題的本質是「可見與否」，不是「持久化與否」。

<a id="依賴"></a>

# 相依

追新（G29），**但不得裸跑 `cargo update`** ——那會跳過 24 小時安全懷疑期，而懷疑期正是 G29 存在的理由。

```bash
python tools/dep-check.py            # 只報告
python tools/dep-check.py --apply    # 採納通過懷疑期的
```

★ **升級之後必須跑完當前里程碑的全部驗證**（M0 是 `tests/m0/run.sh`；M3 之後是全量對拍）。

詳見 [相依策略](/platform/dependency-policy.md) 與 [供應鏈現狀](/platform/supply-chain.md)。

# 建置與驗證

**一切在 Docker 裡跑（G26）。** 宿主機除 Docker 外什麼都不需要——而且 `SIGQUIT` + fd 移交是 Linux 獨有的，**在 Windows 上根本沒法驗**。

```bash
bash tests/m0/docker-run.sh              # 建置 + 跑 M0
BUILD_ONLY=1 bash tests/m0/docker-run.sh # 只建置
```

交接前要做的三件事：**檢核 Markdown 連結、跑 `git diff --check`、跑 `bash tests/m0/docker-run.sh`**。

# ★ 一條本工作區反覆出現的病

**狀態類句子最容易漂。** 詳見 [唯一權威與本 bundle 的定位](/governance/source-of-truth.md) 末節——本專案已經犯過一次（M0 通過後 README 徽章沒跟）。

**每次狀態變化後，這幾處當成一張清單整體掃一遍**：README → `PLAN.md` §1 → `PLAN.md` §7。
