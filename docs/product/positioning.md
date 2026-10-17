---
type: 產品定位
title: 定位
description: 一個 Rust 單靜態二進位同時是 Web 伺服器、反向代理與負載均衡器；空位真實但狹窄。
resource: ../../PLAN.md
tags: [定位, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-2
    resource: /references/plan.md
    title: PLAN.md §2 專案定位、§3 要解決的問題、§3.1 競爭位置
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G21（產品名）、G22（使用者分層）
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §2–§3。**

# 一句話

> **一個 Rust 單靜態二進位，同時是 Web 伺服器、反向代理與負載均衡器；簡單時像 Caddy，複雜時不必換軟體。**

```text
今天：Caddy（自动 HTTPS）+ HAProxy（L4 与调度）+ Nginx（静态与缓存）
枢衡：一个进程、一份配置、一套观测
```

# 產品假設

三家各有所長也各有硬傷。樞衡賭的是兩件事：

1. **這些長處不衝突，可以收進一個實作。**
2. **這些短處大多源自各自的歷史包袱，不是必然代價。**

★ **每採納一個三家已有的設計，必須能說清它避開了對應的哪個坑**（[設計原則](/product/design-principles.md) 第 2 條）。`PLAN.md` §3 的表格逐項列了長處與短處，是這條紀律的檢查表。

幾個被點名要消滅的坑：

| 坑 | 出自 | 樞衡怎麼避 |
|---|---|---|
| 上游網域**預設**只在啟動時解析一次（1.27.3 起開放原始碼版可顯式寫 `resolve`，要另配 `resolver` 與 `zone`） | Nginx OSS | DNS 定期重解析（G17）→ [資料路徑](/architecture/data-path.md) |
| runtime 改動 **reload 後無聲消失** | HAProxy | 不持久化，但**強制可見**（G18）→ [管理面](/architecture/control-plane.md) |
| Admin API **預設綁回環且無認證** | Caddy | 預設只綁 Unix socket（G14）→ [安全基線](/platform/security-baseline.md) |
| 語義有坑的自創 DSL（`if` 不可靠、`location` 順序反直覺） | Nginx | Caddyfile 式 DSL（G20）→ [設定分層](/architecture/configuration.md) |
| Go GC 導致高並行下 P99 尾延遲與記憶體劣化 | Caddy | Rust（G1）|

# ★ 競爭位置必須誠實記錄

`PLAN.md` §3.1 專門立了一節，因為**這一格並不空**：Traefik、APISIX、Kong、Nginx Proxy Manager、Zoraxy，以及 Kubernetes Gateway API 及其多個實作都在附近。2024 年後還多了 Cloudflare 的 **Pingora**（樞衡自己的底座）和 ISRG 基於它的 **River**。

⚠ **「三合一」本身不是空位**（2026-09-24 按一手來源更正，舊版這裡寫的「沒有任何一款現有軟體同時做到這三件事」⛔ 不成立）：Zoraxy 同時有反代、負載均衡、ACME、TCP/UDP 轉發與靜態目錄；River 0.5.0 起能服務靜態檔案（開發已暫停）；nginx 1.27.3 起開放原始碼版有上游動態重解析，另有官方 ACME 動態模組；HAProxy 有記憶體小物件快取。來源連結見 `PLAN.md` §3.1。

樞衡的空位是**真實但狹窄**的，它在**整合的深度與治理方式**，⛔ 不在「別家沒有這項功能」：

> **預設交付的整合程度**（一個靜態二進位、一份設定裡同時有 L4、靜態檔案、磁碟快取、HTTP/3 與自動 HTTPS，不靠外掛或定製建置）· **設定與執行態的治理**（全量原子 load、臨時覆蓋不持久化但永遠可見）· **維運成本**（非 K8s 環境，一個處理程序、一套觀測）。

⛔ 這些是比較維度，不是「別家都做不到」的斷言；功能深度與效能是否等同只能按 `PLAN.md` §8 實測。

★ **這一節的存在本身就是紀律**：它擋住的是「我們沒有競爭者」這種在立項書裡極常見、且幾乎總是錯的說法。

# 給誰用（G22）

| | 使用者 | 待遇 |
|---|---|---|
| **第一類** | 自己的機隊 **與** 自託管 / homelab 站長（★ **並列**主打） | 文件、預設值與遷移工具**優先服務這一類** |
| 第二類 | nginx 存量遷移使用者、需要 L4+L7 混合的小團隊 | |

# 名字（G21）

**樞衡 / Fulcrum。**

- 中文：「樞」是門軸與中樞（閘道），「衡」是平衡（負載均衡）；古語「位居樞衡」指中樞要職。
- 英文：`Fulcrum` 是槓桿與天平的支點——**既是轉動的中心（樞），又是平衡的基準（衡）**，是候選裡唯一同時具備兩層含義的詞。
- ★ 已搜過：代理與負載均衡領域無同名專案。

# 一條已作廢的說法

★ **凡把本專案描述為「統一編排 Caddy/HAProxy/Nginx 的控制面」的說法，一律作廢。**

定位先前的六輪頭腦風暴中徹底重定：**樞衡是一款自研資料面產品，不是多引擎控制面。** Caddy 與 Nginx 只在過渡期作為回落後端，1.0 時回落程式碼歸零。
