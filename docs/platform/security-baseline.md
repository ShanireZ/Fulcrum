---
type: 技術基線
title: 安全基線
description: 管理面預設不出機器、危險能力預設要求顯式存取資格、協定棧一律交給經過稽核的庫。
resource: ../../PLAN.md
tags: [安全, 承重牆, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G14（管理面安全）、G15（On-Demand 存取資格強制）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 風險表（單人 + AI 承擔網路關鍵路徑軟體的安全責任）
  - id: plan-4
    resource: /references/plan.md
    title: PLAN.md §4 設計原則第 5 條（預設即安全）
---

★ 本頁承接原 `docs/architecture.md` §8，屬**技術基線**。它**服從** [`PLAN.md`](../../PLAN.md)。

# 基線七條

1. **管理面預設不出機器** —— 只綁 Unix domain socket，權限交給檔案系統 ACL；遠程管理需顯式開啟且只能走 mTLS（G14）→ [管理面](/architecture/control-plane.md)
2. **危險能力預設要求顯式存取資格設定** —— On-Demand TLS 未配存取資格來源則**啟動失敗**（G15）→ [TLS](/architecture/tls.md)
3. **私鑰、ACME 憑證、上游認證資訊不進普通紀錄**；★ **設定預覽必須脫敏**
4. **最小檔案權限與處理程序權限** —— 不需要長期 root（僅綁定特權連接埠時用 capability）
5. ★ ★ **不自己實作 TLS、HTTP/2 狀態機、HPACK、QUIC** —— 全部交給經過稽核的成熟庫
6. **變更稽核**：身分、版本、時間、結果
7. **回落層不靜態連結任何一家** —— 僅通過處理程序邊界與 API 交互（也是授權條款考慮）→ [回落層](/architecture/fallback.md)

# ★ 第 5 條不是風格偏好

`PLAN.md` §9 把「**單人 + AI 承擔網路關鍵路徑軟體的安全責任**」列為主要風險，而緩解手段只有三條：

- 記憶體安全交給 **Rust**
- 協定棧交給**成熟庫**
- ★ **不自己實作上面那四樣**

這是整個專案安全論證的支點。任何「自己寫一個小的 HPACK 實作就夠了」的提議都直接違反它。

# ★ 第 3 條在 On-Demand 場景下尤其重要

G15 之外還有三道保險：**簽發速率上限、失敗退避、指標告警**。指標會暴露網域——★ 設計指標標籤時要想清楚**哪些網域資訊該進 Prometheus**，多租戶場景下這本身就是資訊洩露面。

# ★  複審：M0 spike 上的三條，兩條已修、一條留給 M1

spike 是驗證臺不是產品，但它是 M1 的範本——**在範本上留下的壞習慣會被照抄**。

| | 情況 | 處置 |
|---|---|---|
| **UDP 回聲是反射面** | 源位址可偽造 → 把流量反射給第三方（放大比 1:1，但反射是真的）。★ 當前**不可達**：`docker run` 沒有 `-p`，連接埠只在容器網路內——但那是**部署方式**在備援，不是服務本身安全 | ✅ 已改：預設綁 `127.0.0.1`，要放開必須顯式 `M0_BIND_HOST=0.0.0.0`。**顯式比預設危險要好**（第 5 條「預設即安全」）。★ M1 的 L4 UDP 要從第一天帶源限制與速率上限 |
| **沒有讀超時** | 連上來什麼都不發（或每 29 秒滴一個位元組）的對端會永久佔住一個任務——慢速連接耗盡最基本的形態 | ✅ 已改：TCP 回聲 30 秒空閒超時；HTTP 讀頭 15 秒**總預算**（★ 不是每次 `read` 的預算，後者擋不住「慢慢滴」） |
| **fd 耗盡時 accept 循環滿速空轉** | 實測：無退避時 **369,156 條紀錄/秒**，3 秒寫了一百多萬行；有退避時 1 條/秒 | ✅ 已改：EMFILE/ENFILE 退避 1 秒，與 pingora 自己 accept 循環的做法一致。★ **這是可用性事故，不是噪音**——它把「fd 快用完了」放大成「機器廢了」，且恰好發生在資源最緊張的時候 |
| **容器以 root 跑** | Windows 上無所謂；Linux CI 上會在工作樹裡留下 root 屬主的 `run/` 等產物 | ⏳ 留了口子未啟用：`DOCKER_USER="$(id -u):$(id -g)"`。★ **沒有在 Linux 上實測過**，所以是口子不是預設——在 Windows 上傳宿主機 uid 反而會讓命名卷不可寫、把建置搞壞 |

# 供應鏈

依賴是安全基線的一部分，但它有自己的策略與現狀：

- **策略**（G29：追新 + 24 小時懷疑期）→ [依賴策略](/platform/dependency-policy.md)
- **現狀**（44 項陳舊、4 條公告及可達性分析）→ ★ [供應鏈現狀](/platform/supply-chain.md)

# 相關

[管理面](/architecture/control-plane.md) · [TLS](/architecture/tls.md) · [觀測](/architecture/observability.md) · [供應鏈現狀](/platform/supply-chain.md)
