---
type: 架構基線
title: 處理程序與元件邊界
description: 樞衡是一個處理程序，它既是控制面也是資料面——這與舊版架構（控制面編排三個外部資料面）相反。
resource: ../../PLAN.md
tags: [架構, 承重牆, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-2
    resource: /references/plan.md
    title: PLAN.md §2 專案定位（一個處理程序、一份設定、一套觀測）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G2（三家角色）、G9（回落定位）
---

★ 本頁屬**技術基線**，帶真內容。它**服從** [`PLAN.md`](../../PLAN.md)；衝突時以 `PLAN.md` 為準。

# 樞衡是一個處理程序

它既是控制面也是資料面——★ **這與舊版架構（控制面編排三個外部資料面）相反**。

```text
                    ┌──────────────── fulcrum（单进程）────────────────┐
  DSL 文件 ─────────▶│  解析器 → 结构化配置（唯一内部事实）→ 校验器      │
  CLI / HTTP API ───▶│                     │                          │
                    │              事务式装载（原子生效 / 失败回退）    │
                    │                     │                          │
                    │   ┌─────────────────┴─────────────────┐        │
                    │   │        运行时（Pingora Server）      │        │
                    │   │  h1/h2 Service · L4 Service ·      │        │
                    │   │  QUIC/h3 Service · 后台服务         │        │
                    │   └─────────────────┬─────────────────┘        │
                    └─────────────────────┼──────────────────────────┘
                                          │
                       ┌──────────────────┼──────────────────┐
                       ▼                  ▼                  ▼
                    上游服务          本地静态文件        回落层（过渡期）
                                                        Caddy / Nginx
```

**流量路徑預設只經過樞衡一跳。** 回落層是過渡期的例外，且在計畫輸出中**必須顯式標出額外跳數**（G9）。

# 元件之間的邊界

| 元件 | 職責 | 相關 |
|---|---|---|
| 解析器 | DSL → 結構化設定 | [設定分層](/architecture/configuration.md) |
| 檢核器 + 交易式裝載 | 原子生效 / 失敗退回 | [管理面](/architecture/control-plane.md) |
| 執行環境 | Pingora `Server` 上掛的一組 `Service` | [資料路徑](/architecture/data-path.md) |
| 回落層 | 過渡期薄轉發 | [回落層](/architecture/fallback.md) |

# ✅ 它怎麼被託管（G31 / G33 / G34，結案）

**systemd `Type=notify`，前台執行。** `conf.daemon` 恆 `false`。

| 項 | 落法 | 換來什麼 |
|---|---|---|
| 特權丟棄 | systemd 的 `User=` / `Group=` | ★ `daemonize` 依賴整條刪除，**RUSTSEC-2025-0069 歸零**（✅ 2026-09-24 兌現：pingora 0.9.0 把它換成了 `daemonix`；後者仍在依賴圖裡，而樞衡恆 `daemon: false`、不走它）；特權丟棄交給比任何 crate 都更經得起稽核的 systemd |
| 紀錄 | stderr → journal | 不需要自己開紀錄檔案、不需要 logrotate |
| 目錄 | `ConfigurationDirectory=` `/etc/fulcrum`<br>`StateDirectory=` `/var/lib/fulcrum`<br>`RuntimeDirectory=` `/run/fulcrum`<br>`CacheDirectory=` `/var/cache/fulcrum` | systemd 負責建立並按 `User=`/`Group=` 設權限。★ **但每一項都可被設定覆蓋**——D8 的磁碟快取要能掛到別的盤 |
| PID 檔案 | ⚠ **這一格已被 M1 spike #1 的實測部分推翻** —— 見下一節 | 原話是「`Type=notify` 下 systemd 自己跟蹤 MainPID」，說的是 systemd 的 **`PIDFile=` 指令**不需要（那是對的，至今如此）。但它**不等於**「沒有任何東西需要知道當前是哪一代」：`ExitType=cgroup` 之下 MainPID 在首次換代後歸零，`ExecReload` 必須從一個**每代自寫**的 pid 檔案裡找當前這一代 |
| 回落層 | `Wants=` / `After=` 宣告依賴，**不監管** | 見 [回落層](/architecture/fallback.md) |

★ **一條獨立於安全考慮的收益**：`daemonize()` 會 fork，而 pingora 自己的文件註解寫著「`run()` 之前建立的任何執行緒都會丟失」。前台執行**解除了這條約束**——設定解析、ACME、回落層相關的東西可以在 `run()` 之前起來。

## ✅ 升級視窗內怎麼活下來（M1 spike #1，實測）—— ★ 結論與原推斷不同

原來寫在這裡的推斷是：

> 新處理程序必須**落在同一個 cgroup 內**並**搶過 MainPID**——即 nginx `USR2` 那套。

★ ★ ★ **前半句對，後半句實測是錯的，而且有害。**

| 口徑 | 老代退出後 unit | `systemctl stop` |
|---|---|---|
| 交接 MainPID（原推斷）| `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |
| **`ExitType=cgroup`，不交接** | `active` ✅ | **10 秒走完排空，`success`** ✅ |
| 兩者都用 | `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |

交接過去的 pid 不是 systemd 親生的（老處理程序 fork 的），systemd 會把它標成 alien，
此後停機**不再等它排空**。★ **整個失效在升級當時零症狀**，代價要到下一次停機才兌現——
重新啟動、`systemctl restart`、關機，全部變成硬殺連接。

**採納的形狀**（✅ **已由 G37 拍板**；D14 結案）：

| 項 | 落法 |
|---|---|
| unit 活過換代 | **`ExitType=cgroup`**（systemd ≥ 250）：cgroup 裡還有處理程序就算活著 |
| 新處理程序進 cgroup | 老處理程序收 `SIGUSR2` 後自己 fork+exec（`-u`），然後給自己發 `SIGQUIT` 送 fd |
| MainPID | ★ **不交接。** 首次升級後 `MainPID` 歸零，這是 `ExitType=cgroup` 的已知代價 |
| `ExecReload` | ⚠ **不能用 `$MAINPID`**（歸零後展開成空，**第一次能成、第二次回報錯誤**：`systemctl reload` 結束代碼 1、journal 裡是 `kill` 的 Usage，而 unit 仍 `active`——升級沒發生，老一代照常跑著）。改從每代自寫的 pid 檔案裡找 |
| `NotifyAccess` | 可收緊到 `main`——不交接之後，發通知的永遠是主處理程序自己 |
| `TimeoutStopSec` | ≥ `grace_period_seconds` + `graceful_shutdown_timeout_seconds`。★ **產品預設是 30 + 5 = 35 秒**（批 12 定的，見下一節），所以 `TimeoutStopSec=60` 夠用 |

> ★ **推斷不是證據。** M0 那次靠 spike 額外撈出了 UDP 分流那條風險；這一次 spike 直接
> **推翻了推斷本身**，並另外撈出一條新缺陷（移交來的監聽 fd 沒有 `CLOEXEC`）。
> 全部資料見 [M1 spike #1](/verification/m1-systemd.md)。

## ✅ 產品二進位真的被託管起來（G78）

⚠ ⚠ **上一節描述的形狀，產品二進位一度並不滿足。**  收官那天實測：
按上表寫一個 unit 去起 `fulcrum serve`，`systemctl start` **超時失敗**
（`MainPID=0`、`Result=timeout`、`/run/fulcrum/` 空），而 journal 裡資料面
本身好好地在 8080 上聽著。

> ★ ★ ★ **為什麼當時的門一條都沒抓到**：M1 那三個 systemd 場景跑的是
> **spike 二進位**，而 spike 把這些自己實作了一遍。
> **一個 spike 證明的是「這條路走得通」，不是「產品走在這條路上」。**

批 12 把四件事接進 [`crates/fulcrum-server/src/process.rs`](../../crates/fulcrum-server/src/process.rs)：

| # | 落法 | 不做會怎樣（都實測過） |
|---|---|---|
| ① | 等 `ExecutionPhase::Running` 後發 `sd_notify(READY=1)` | `systemctl start` 超時失敗 |
| ② | 同一時刻寫 pid 檔案（write + rename，**先落檔案再報就緒**）| `ExecReload` 無處可查當前這一代 |
| ③ | 裝 `SIGUSR2` 處理器：fork+exec 自己（帶 `-u`），成功後才給自己發 `SIGQUIT` | ⚠ **比「reload 無事發生」更糟**：SIGUSR2 的預設動作是**終止處理程序**，實測 `code=killed, status=12/USR2`，而 `systemctl reload` 自己**回傳成功** |
| ④ | `grace_period_seconds` 預設 **30s**、`graceful_shutdown_timeout_seconds` 顯式 **5s** | 留 `None` 就落到 pingora 的 `EXIT_TIMEOUT=300`，`TimeoutStopSec=60` 會在排空到 1/5 時 SIGKILL |

★ **停機預算寫在啟動紀錄裡**（`停机预算约 35s（排空 + 收尾）`），因為
`TimeoutStopSec` 必須按它設，而它由設定決定 —— 只存在於原始碼裡的話維運只能靠猜。

★ ★ **順帶查出並修掉一條**：換代（`-u`）時監聽 fd 是**從上一代繼承**的
（按位址字串查 fd 表，UDS 的鍵就是路徑），新一代不會重新 `bind()`。
此時若把管理 socket 的路徑 unlink 掉，兩代都在一個**沒有名字的** inode 上 accept，
用戶端按路徑連過去只有 ENOENT —— **一次 `systemctl reload` 之後管理面永久失聯**，
而紀錄裡一切正常、socket 也確實在 listen。⇒ 只有**非換代**那一趟才清陳舊檔案。

判據：[`tests/m1/product.sh`](../../tests/m1/product.sh)（M1 的第四個 systemd 場景，
**跑產品二進位**）。五條反證各自只紅在它該紅的那一步。
部署形狀見[部署](/platform/deploy.md)。

# ✅ 執行緒模型（G35，結案）

★ ★ **最要緊的一條機制事實：執行緒不跨 service 共享。** `ServerConf` 的註解原話是 "The threads are not shared across services"——**每個 service 各起一套 runtime**，所以**總執行緒數 ≈ Σ(各 service 的 threads)**。樞衡註定有 4+ 個 service，**全域把 `threads` 設成核數會直接超訂**（4 核 × 4 service = 16 個 worker 執行緒）。

| 機制 | 事實 | 出處 |
|---|---|---|
| 每 service 執行緒數 | `Service::threads()` 可逐 service 覆蓋全域 `conf.threads`（預設 **1**）| `server/mod.rs:705` |
| 竊取開關 | ★ **`work_stealing` 是全域的**，不能逐 service 混用 | `server/mod.rs:742` |
| NoSteal 的坑 | ★ `current_handle()` 回傳**隨機一個執行緒**的 handle——「把工作交回本執行緒」的假設不成立 | `pingora-runtime/src/lib.rs` |

**決策**：

- **`work_stealing = true`（Steal）**，per-core 推到 **M3 用對拍資料定**。M0 實測 Steal 下自建服務全綠，是目前唯一有證據的設定。
- **執行緒數按 service 角色定預設，全部可被設定覆蓋**。初值：L7 反代 = CPU 核數、L4 = 2、管理面/後台 = 1。★ **初值不是結論**，判據在 M3 的對拍資料裡。
  ✅ **已完成實作（G140，2026-09-05）** —— 在此之前這條決定**只寫在這一頁上，程式碼裡一處都沒有**：
  `ServerConf` 走 `..Default::default()`（`threads: 1`），DSL 與 CLI 都沒有旋鈕，
  ⇒ **L7 熱路徑線上上一直只有 1 個執行緒**。那不是誰做的決定，是這條決定沒被實作（D33）。
  落法：DSL 全域塊三個鍵 `threads_l7` / `threads_l4` / `threads_admin`（[DSL 參考](/architecture/dsl-reference.md) §一）·
  角色預設只住一處（`fulcrum_server::process::default_threads`）·
  `threads_admin` 同時是 `ServerConf.threads` ⇒ 後台任務跟著它 ·
  L7 / L4 各自逐 service 覆蓋（`Service::threads()`）。
  ★ h3 **與 h1/h2 同一個角色**（G140 ③）：它就是資料面入口，與 h1/h2 服務同一批流量、走同一條執行鏈。
  ⚠ ⚠ **升級一次就會變行為**：L7 那一格從 1 變成 CPU 核數，資源佔用與延遲分佈都會變。
  ★ ★ 判據掛在**處理程序真的有幾個 OS 執行緒**上（`tests/serve/run.sh` 的 [4/4] 一節），
  ⛔ 不掛在啟動紀錄那一行 —— 紀錄打的是剛算出來的數，對「它有沒有真的傳給 pingora」零判別力。
  2026-09-05 實測（12 核）：`Threads` = **4 + `threads_l7`**，嚴格線性
  （`1`→5 · `8`→12 · `16`→20 · 不寫→16）；撤掉那一行串接之後三個讀數**一起塌成 5**，兩條斷言同時紅。
- **QUIC 的執行緒分配不在本輪**，~~隨 [D11](/governance/open-questions.md) 在 M2 選庫時一併定~~ ⇒ ⚠ ★ **D11 已已結案（取 `quiche`），而執行緒分配這一條並沒有跟著定** —— quiche 是 **sans-IO**，事件循環整個由我們自己寫 ⇒ 它現在是**實施批次自己的設計題**，不再掛在 D11 上。

# ★ 一條已被 M0 證實的執行環境性質

Pingora 的 `Server` 上可以**同時掛原生服務與自建服務**，兩者共用一張 fd 表且鍵空間自由：

- 原生 `listening::Service` 用裸 `addr:port` 作鍵
- 自建 `Service` 可以自己定鍵（M0 裡用了 `m0-raw-tcp:0.0.0.0:8081` 這樣的前綴形式）

★ **自建 `Service` 有義務自己取走 fd 並放回去**——這不是繞過設計，`Service` trait 的文件註解本就把它寫成自建服務的義務。證據見 [M0 接縫驗證](/verification/m0-seam.md)。

這條直接決定了 [資料路徑](/architecture/data-path.md) 裡三個入口能掛在同一個 `Server` 上而不犧牲優雅升級。
