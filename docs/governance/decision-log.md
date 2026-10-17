---
type: 專案治理
title: 決策紀錄導覽
description: 哪條決策管哪件事、各輪拍板的脈絡、owner 推翻 AI 建議的那些決定，以及三條不可回頭的硬約束。
resource: ../../PLAN.md
tags: [治理, 必讀, 決策]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 決策清單（全文）
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 G6 附帶的三條硬約束
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §10，本頁只做導覽。** 引用時引 G 編號與 `PLAN.md`，不要引本頁。

每一條都由 owner 拍板，按順序追加在 `PLAN.md` §10 末尾。**本頁不復述條數** —— ★ 一個沒人維護的計數遲早會變成一句假話。

# 按「我想知道什麼」查

| 我想知道 | 去看 |
|---|---|
| 為什麼是 Rust | G1 |
| 三家產品在這裡扮演什麼角色 | G2 · G9 · G25 → [回落層](/architecture/fallback.md) |
| 首版到底做多少 | G3 · G5 · G7 → [首版範圍](/product/scope.md) |
| 設定長什麼樣 | G4 · G11 · G20 → [設定分層](/architecture/configuration.md) |
| 底座為什麼選 Pingora | G6 → [技術棧](/platform/tech-stack.md) |
| 設定怎麼熱更新 | G8 · G18 → [管理面](/architecture/control-plane.md) |
| 什麼時候開放原始碼 | G10 |
| 自動 HTTPS 怎麼做 | G12 · G15 → [TLS](/architecture/tls.md) |
| 怎麼散佈 | G13 → [技術棧](/platform/tech-stack.md) |
| 管理面怎麼保證安全 | G14 → [安全基線](/platform/security-baseline.md) |
| 能看到什麼指標 | G16（基線）· **G116 · G117 · G118 · G121**（Prometheus 指標怎麼落的）· **G122 · G123 · G124**（D32 / D31 / D30 結案：TLS 與連接兩族 · `purge` 拆族 · `status_class` 的 `none`）→ [觀測](/architecture/observability.md) |
| 上游節點怎麼發現與摘除 | G17 → [資料路徑](/architecture/data-path.md) |
| 效能怎麼算達標 | G19 → [效能驗收標準](/verification/performance-bar.md) |
| 名字的來歷 | G21 |
| 給誰用 | G22 → [定位](/product/positioning.md) |
| 為什麼沒有 Web UI | G23 |
| 里程碑怎麼切 | G24 → [實施路線](/governance/roadmap.md) |
| 為什麼全在 Docker 裡跑 | G26 → [建置與驗證](/platform/build-and-test.md) |
| M0 為什麼不含 QUIC | G27 · G28 → [M0 接縫驗證](/verification/m0-seam.md) |
| 相依怎麼升級 | G29 → [相依策略](/platform/dependency-policy.md) |
| ★ pingora 為什麼走 fork | **G30** → [供應鏈現狀](/platform/supply-chain.md) · [`FORK.md`](../../vendor/pingora/FORK.md) |
| 靜態檔案怎麼發（符號連結 / 隱藏檔案 / 範圍請求 / MIME）| **G87 · G88 · G89 · G90**（§10）|

# ★ owner 推翻了 AI 推薦項的決定

這些條目值得單獨記住 —— 它們是專案裡 AI 判斷與 owner 判斷分岔的地方，**引用時不要回頭去找
AI 當初的建議**。

| | AI 建議 | owner 拍板 | 分歧的根子 |
|---|---|---|---|
| **G3** | 只做「反代 + 自動 HTTPS + 基礎 LB」（評 3.0 分） | 三合一全量首版 | 配合 G2/G5 後成立——「全量」指**對外能力面**，不是自研程式碼面 |
| **G13** | 先做容器鏡像，後做包 | 單靜態二進位 + systemd + deb/rpm，**不做官方鏡像** | 既然是單靜態二進位，文件給一份 `FROM scratch` Dockerfile 就夠了 |
| **G22** | （未記錄反對） | 自己的機隊 **與** 自託管站長**並列**主打 | 文件、預設值與遷移工具優先服務第一類 |
| **G25** | 只回落 Caddy 一家（評 8.5 vs 7.0） | 回落 **Caddy + Nginx 兩家** | 代價（工程量翻倍、1.0 全刪）已知並接受，換過渡期不難看的效能數字 |
| **G29** | 精確釘死 `=0.8.1` | **追新，包括破壞性大版本** + 24 小時懷疑期 | ★ AI 提示「`>=` 無上界會跳到破壞性大版本」後，owner **同日重申**「確定使用最新+包括破壞性的版本更新」——**這是知情後的重申，不是疏漏** |
| **G30** | 接受 pingora 的天花板，跟進上游 0.9（約季度節奏）| **fork `pingora-core` 放寬上界** | 代價（`PLAN.md` §9 的合併成本從風險變成進行中的成本）已認下，換來兩條真漏洞立即清零；★ **實測只需改 12 處呼叫點，遠小於預估** |
| **G49** | 書寫順序即執行順序，零隱式重排序 | **內建順序表**（照 Caddy）| AI 的理由是「它與 `PLAN.md` §3 點名的 nginx `location` 坑同構」；owner 換來的是「使用者隨便寫也能跑對」與 Caddyfile 遷移零意外（G22 把這兩類使用者都列為目標）。★ 代價四條已一併落實 |
| **G54** | DNS-01 推到 M2（「要長出一整套 DNS 供應商外掛體系」）| **DNS-01 進 M1** | 通配符證書沒有別的路；★ 落法收斂為「原生兩家 + exec hook 備援」（G57），不建體系 |
| **G58** | M1 只做機制，通配符不寫進驗收 | **通配符進 M1 退出條件** | ⚠ 代價 AI 當時就提了：DNS-01 因此位於 M1 關鍵路徑上，出問題直接卡驗收。**這是知情後的選擇** |
| **G87** | 靜態檔案只跟隨符號連結，不給開關（少一條指令） | **預設跟隨 + 加一條 `follow_symlinks false`** | AI 的理由是 §6.2 「能少一條指令就少一條」；owner 換來的是**兩側都有真實使用者**——預設那側要的是零額外系統呼叫與連結農場可用，關掉那側要的是符號連結逃逸在結構上做不到 |
| **G88** | 預設 404 掉一切以 `.` 開頭的路徑段（`.well-known/` 例外） | **不按 `.` 一刀切**，改成一張**可設定的預設 404 清單**（`hide`）| owner 不接受「按字元形狀猜危險」這個判據——`.well-known/` 要開例外這件事本身就說明這條規則**猜錯過一次**；換成一張**指名道姓**的清單，代價（表要維護）換來的是「為什麼 404」永遠查得到 |
| **G88**（疊加方式）| `hide` 寫了就**整條替換**預設表 | **追加**，另給一條 `hide_defaults false` 關掉預設表 | AI 取替換是為了讓預設表能被推翻；owner 用一條獨立開關達成同一件事，同時保住「再擋一樣只寫一行」。★ **兩個目標本來就不必二選一** —— 分岔點不在取哪一側，在於有沒有想到第三種寫法 |

# ★ 三條不可回頭的硬約束（G6 附帶）

完整表述見 `PLAN.md` §5.1。這三條**不得繞過**，[`AGENTS.md`](../../AGENTS.md) 也重複了一遍：

1. ⚠ ⚠ ⚠ ~~**TLS 後端鎖死 rustls。**~~ —— ★ ★ ★ **由 G104 推翻**（§10），**這是本專案第一次推翻一條寫著「不可回頭」的硬約束**。現行：後端統一 **BoringSSL**，兩個入口共用同一個 `select_certificate_callback`。<br>★ 原文（保留）：動態證書選擇必須走 `ResolvesServerCert`，**不是** `certificate_callback`（rustls 後端不支援它）。<br>> ★ **「不可回頭」約束的是我們自己不要反覆，不是約束事實不許變。** 那句理由今天依然為真 —— 只是我們不再站在 rustls 那一側了。詳見 [TLS](/architecture/tls.md)。
2. **tower 中介軟體生態用不上。** Pingora 有自己的 `ProxyHttp` 階段模型，tower 的 `Service`/`Layer` 插不進去——**同名不同物**。⚠ 但那句話**關於上游為真、關於我們為假**：本儲存庫從來沒有用過 `ProxyHttp`，執行鏈掛在 `HttpServerApp` / `ServerSession` 上。
3. ~~**自建 QUIC/L4 監聽器必須接入 Pingora 的 socket 移交**~~ → ✅ **已由 M0 解除**，見 [M0 接縫驗證](/verification/m0-seam.md)。★ 但它**衍生出一條新風險**（升級視窗內 UDP 資料報在兩代處理程序間分流），見 [尚未驗證的接縫](/verification/open-seams.md)。

# 一處需要留意的編號事實

★ **G29 同時是 D1 的結案。** `PLAN.md` §11 **只列仍然開著的**，D1 早已不在裡面 —— 結論在 §10 的 G29。讀到「相依版本策略待定」這類舊錶述時，**它是過期的**。
