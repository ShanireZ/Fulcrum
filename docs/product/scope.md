---
type: 產品定位
title: 首版範圍
description: 自研四塊全部做、明確不做七項、未完成的能力過渡期回落給兩家引擎。
resource: ../../PLAN.md
tags: [範圍, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-6
    resource: /references/plan.md
    title: PLAN.md §6 首版範圍（§6.1 自研 / §6.2 不做 / §6.3 回落層）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G3、G5、G7、G23、G25
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §6。**

# ★ 先分清「全量」指的是什麼

G5 定的口徑，理解錯了整章都會讀錯：

> **「三合一全量」= 對外能力面全量，自研逐塊吞併。**

使用者看到的是完整的三合一；**內部自研哪塊算哪塊，其餘回落**。所以 G3 的「全量首版」**不是**說第一版就要把所有程式碼自己寫完。

# 自研（四塊全部，G7）

- **L7 反代核心**（產品本體）：HTTP/1.1、HTTP/2、TLS 終止、WebSocket、gRPC —— 後三者 Pingora 已覆蓋
- **L4 面**：TCP/UDP 透傳、SNI/ALPN 分流、PROXY protocol
- **靜態檔案服務**：range、ETag、壓縮與預壓縮、目錄索引
- **HTTP 快取磁碟後端**：實作 Pingora 的 `Storage` 與 `EvictionManager` —— ★ **開放原始碼版只帶記憶體實作**
- **HTTP/3 / QUIC**：⚠ ~~`quinn` + `h3`~~ → **`quiche`**（**G103**）獨立入口，在樞衡自己的路由層與 h1/h2 匯合。★ **「自研」指入口、連接歸屬、路由匯合與中介軟體鏈；協定棧本體不自研**（G105 —— 手寫 QPACK 違反[安全基線](/platform/security-baseline.md)第 5 條）

技術落點見 [資料路徑](/architecture/data-path.md)。

# 明確不做（七項）

| 不做 | 依據 |
|---|---|
| Web UI | G23——管理面只有 DSL + CLI + HTTP API；Prometheus 接 Grafana 已覆蓋視覺化，且三家都沒有官方 UI |
| 服務發現整合（Consul / etcd / Docker labels） | |
| Kubernetes Endpoint 監聽與 Gateway API 實作 | ★ 與 [定位](/product/positioning.md) 裡「非 K8s 環境」的空位一致 |
| OpenTelemetry tracing | |
| Windows / macOS 支援 | G13——目標平台只有 Linux |
| 多節點控制平面、RBAC、審批流 | |
| 官方容器鏡像 | ★ G13——單靜態二進位，文件給一份 `FROM scratch` Dockerfile 代替 |

# 回落層（過渡期，1.0 時歸零）

| 能力 | 過渡期回落給 | 理由 |
|---|---|---|
| 靜態檔案、HTTP 快取 | **Nginx** | 這兩塊 nginx 最強，過渡期效能表現更體面 |
| 其餘未完成能力 | **Caddy** | 能力最全，Admin API 最好對接，機隊現狀本就是 Caddy |

★ **代價已知並接受（G25）**：回落兩家而非一家，回落層工程量**約翻倍**，而這些程式碼 1.0 時**全部刪除**。換來的是過渡期不必向自己和早期使用者解釋難看的效能數字。

設計約束與拆除節奏見 [回落層](/architecture/fallback.md)。

# ★ 範圍與里程碑不是一回事

本頁說的是**首版對外能力面**。什麼時候真正做出來、按什麼順序拆掉回落，在 [實施路線](/governance/roadmap.md)：M1 只接管一臺機，L4 / 靜態 / 磁碟快取 / HTTP-3 要到 M2 才依次上線並逐塊拆除回落。
