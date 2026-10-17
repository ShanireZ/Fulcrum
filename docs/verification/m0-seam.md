---
type: 驗證記錄
title: M0 接縫驗證 · 結果
description: 通過。自建的裸 TCP 與裸 UDP 監聽器都參與了 Pingora 的 socket 移交，優雅升級視窗內三類流量零中斷。
resource: ../../tests/m0/run.sh
tags: [驗證, 已通過, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-7
    resource: /references/plan.md
    title: PLAN.md §7 M0（範圍與退出條件）
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 3 條硬約束（這條風險的原始表述）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G26（Docker 環境）、G27（範圍收窄）、G28（QUIC 推後）
---

★ **本頁記的是實際跑出來的東西，屬歷史事實。** 結論可以被後續實驗推翻，但「那幾次跑出了什麼」不會變。

⚠ ★ ★ **這份結論的兩個前提，必須和結論一起讀**（2026-08-14 補記——原先只有結論、沒有前提）：

1. **全部證據都來自 `daemon: true`**（[`conf/m0.yaml`](../../conf/m0.yaml)），即 pingora 自己 fork 出守護處理程序的那條路。
   而 **G31 定的產品路徑是 systemd `Type=notify` 前台執行、`daemonize` 依賴刪除**——
   也就是說，**產品將要走的那條路，本頁一個字都沒驗過**。
   G31 已把「systemd 下的 MainPID 交接 + 零停機升級」列為 **M1 的第一個 spike**，判據照 M0 的形式寫死；
   ★ 但在那個 spike 跑出來之前，**不要把本頁的綠當成「前台模式也沒問題」**。
2. **綁定位址**當時是 `0.0.0.0`。2026-08-14 收緊為預設 `127.0.0.1`（可用 `M0_BIND_HOST` 覆蓋），
   理由見[安全基線](/platform/security-baseline.md)一節的回聲反射面。下面紀錄摘錄裡的 `0.0.0.0:...`
   是**當時的原樣記錄**，沒有改寫——★ 歷史證據不該為了和今天一致而被修飾。

> **結論：通過。** 自建的裸 TCP 與裸 UDP 監聽器都參與了 Pingora 的 socket 移交，
> 一次 `SIGQUIT` + `-u` 優雅升級視窗內，三類流量零中斷。
>
> 日期：2026-08-12 ｜ 環境：Docker `fulcrum-build:local`（`rust:1-trixie` + cmake + clang，Rust 1.97.1）
> ｜ pingora-core 0.8.1（★ 現為 [`vendor/pingora`](../../vendor/pingora/FORK.md) 的 fork）
> ｜ 重跑方式：`bash tests/m0/docker-run.sh`
>
> ★ **六次獨立執行全綠**，其中三次是在 G30 的依賴 fork 之後——見 §5。

# 1. 要回答的問題

`PLAN.md` §5.1 第 3 條把這件事列為整個方案**唯一需要真花力氣驗證**的接縫：

> 自建 QUIC(UDP) 與 L4 監聽器必須接入 Pingora 的 socket 移交，否則優雅升級時這兩條會斷連。

**不通過就不進 M1。**

★ QUIC 不在 M0 內（G27/G28）。風險本體是「非 Pingora 託管的監聽器能否參與 fd 移交」，**這與監聽器上跑什麼協定無關**——能移交裸 UDP 的 fd，就能移交 QUIC 的。把 QUIC 排除在外，失敗時才能立刻分清是**接縫**的問題還是 **QUIC 庫**的問題。

# 2. 先讀原始碼得到的推斷

`pingora-core 0.8.1` 的 `Service` trait 文件註解直接寫明瞭自建服務的**義務與權利**：

> `fds` (Unix only): a collection of listening file descriptors. During zero downtime restart
> the `fds` would contain the listening sockets passed from the old service, services should
> take the sockets they need to use then. If the sockets the service looks for don't appear in
> the collection, the service should create its own listening sockets and then put them into
> the collection in order for them to be passed to the next server.

★ 也就是說，**自建監聽器參與 fd 移交是 `Service` 契約裡第一等的設計，不是 hack**。

再看容器本身（`src/server/transfer_fd/mod.rs`）：

```rust
pub struct Fds { map: HashMap<String, RawFd> }
pub fn add(&mut self, bind: String, fd: RawFd)
pub fn get(&self, bind: &str) -> Option<&RawFd>
```

`HashMap<String, RawFd>` —— ★ **對協定零假設**。鍵是任意字串，值是個 `i32`；傳輸走 `SCM_RIGHTS`，本來就能傳任意 fd。所以 UDP 在結構上沒有任何障礙。

推斷到此為止。★ ★ **推斷不是證據**，所以有了這個 spike。

# 3. 實作

三個服務掛在同一個 `Server` 上（`spikes/m0-seam/src/main.rs`）：

| 服務 | 監聽 | 託管方 | 證明什麼 |
|---|---|---|---|
| `m0-http` | TCP 8080 | Pingora `listening::Service` | 原生服務照常工作，且與自建服務共用一張 fd 表不撞鍵 |
| `m0-raw-tcp` | TCP 8081 | **自建 `Service`** | 自建 TCP 監聽器能取到/放回 fd |
| `m0-raw-udp` | UDP 8082 | **自建 `Service`** | ★ 自建 **UDP** 監聽器能取到/放回 fd |

自建服務的 fd 表鍵刻意加了前綴（`m0-raw-tcp:0.0.0.0:8081`），與 Pingora 原生的裸 `addr:port` 鍵錯開——順便證明**鍵空間是自由的**。

# 4. 直接證據

第一代註冊、收到 `SIGQUIT` 送出、第二代繼承：

```text
[raw-udp] bound fresh on 0.0.0.0:8082, registered fd=10 as key=m0-raw-udp:0.0.0.0:8082
[raw-tcp] bound fresh on 0.0.0.0:8081, registered fd=23 as key=m0-raw-tcp:0.0.0.0:8081
pingora_core::server] SIGQUIT received, sending socks and gracefully exiting
[raw-udp] INHERITED fd=6 for key=m0-raw-udp:0.0.0.0:8082
[raw-tcp] INHERITED fd=5 for key=m0-raw-tcp:0.0.0.0:8081
```

★ 兩代之間 **fd 編號不同**（23→5、10→6）正是對的：`SCM_RIGHTS` 是把 fd **複製**進新處理程序的 fd 表，**編號由接收方核心分配，與傳送方無關**。編號若相同反而說明沒真的傳。

★ **不同次執行的 fd 編號也不同**（首輪是 23→6、10→7），同樣是這個原因。**不要把某一次的具體編號寫進斷言。**

# 5. 間接證據

探針在升級前後持續打三類流量（`spikes/m0-seam/src/bin/probe.rs`），**六次獨立執行**：

| | 環境 | HTTP 成功/失敗 | TCP 成功/斷開 | UDP 成功/丟失 |
|---|---|---|---|---|
| 1 | 原始（bookworm，pingora 0.8.1）| 281 / **0** | 283 / **0** | 283 / **0** |
| 2 | 同上 | 282 / **0** | 283 / **0** | 283 / **0** |
| 3 | 同上 —— ★ **改造前基線** | 282 / **0** | 283 / **0** | 283 / **0** |
| 4 | **trixie**（Debian 13 + Rust 1.97.1）| 281 / **0** | 283 / **0** | 283 / **0** |
| 5 | ★ ★ **trixie + fork 後的 pingora** | 281 / **0** | 282 / **0** | 282 / **0** |
| 6 | 同上 | 282 / **0** | 284 / **0** | 283 / **0** |
| 7 | 同上 | 282 / **0** | 284 / **0** | 284 / **0** |

判據：HTTP 每輪新建連接、打的是 accept 路徑；TCP ★ **同一條連接貫穿升級**；UDP 跑在迴環上無擁塞，**丟失即代表 socket 失守**。

處理程序確實換了：`gen1 pid=23 → gen2 pid=43`（第 1 次）、`gen1 pid=4006 → gen2 pid=4026`（第 3 次）。

★ ★ **第 5–7 次尤其重要**：G30 的 fork 把 `nix` 從 0.24.3 抬到 0.31.3，而那一輪破壞**恰好落在 `server/transfer_fd/mod.rs`——本頁要驗的那個模組**。`socket()` 改為回傳 `OwnedFd`、`listen()` 改簽名、`cmsgs()` 變為可失敗，三處都在 fd 移交路徑上。**連跑三次全綠，且兩代之間 fd 編號照舊不同**，說明移交仍然是真的。改動清單見 [`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md)。

# 6. 順帶得到的兩條結論

## 6.1 給 D11（HTTP/3 庫選型）的一條約束 ★

升級視窗內**兩代處理程序持有的是同一個 UDP socket**，雙方都在 `recv_from`，於是資料報會在兩代之間被**分流**。

對回聲服務這無所謂——誰答都一樣，所以零丟失。★ ★ **但對 QUIC 是真問題**：連接狀態只存在於某一代處理程序裡，被分到另一代的資料報無法被正確處理。

所以 M2 做 HTTP/3 時，**fd 移交本身已經不是問題，連接歸屬才是**。完整推導與可能方向見 [尚未驗證的接縫](/verification/open-seams.md)。

## 6.2 給 D2（執行緒模型）的實測依據

M0 跑在 `threads: 2` / `listener_tasks_per_fd: 1` / `work_stealing: true` 下全綠。這說明 Pingora 的預設多執行緒 work-stealing 模型**對自建服務沒有額外約束**——自建 `Service` 拿到的是 `current_handle()` 所在的那個 runtime，行為與原生服務一致。

★ **D2 此後已經不再待定** —— 執行緒模型的結論是 `PLAN.md` §10 的 **G35**（保持
`work_stealing = true`；執行緒數按 service 角色定預設、設定可覆蓋）。本節留下的是當時給它的
實測依據：★ **至少不存在「自建服務必須單執行緒」這類隱藏限制**。

# 7. 讀紀錄時不要誤判的一行

升級過程中第二代會先打一條 **ERROR**，然後才成功：

```text
ERROR pingora_core::server::transfer_fd] No incoming socket transfer, sleep 1s and try again
INFO  pingora_core::server::bootstrap_services] Bootstrap done
```

★ **這是正常的重試，不是失敗。** 第二代起得比第一代送 fd 早一點時就會出現。判據是結束代碼與後面那兩行 `INHERITED`，**不是有沒有 ERROR 字樣**。

# 8. 怎麼重跑

```bash
bash tests/m0/docker-run.sh
```

宿主機只要有 Docker。建置鏡像不存在會自動做，`cargo` 與 `target` 快取放在命名卷裡，不汙染宿主機也不進 Windows 檔案系統。執行期產物（pid / sock / 紀錄 / 探針輸出）落在 `run/m0/`（已 gitignore）。★ **結束代碼即結論。**

詳見 [建置與驗證](/platform/build-and-test.md)。
