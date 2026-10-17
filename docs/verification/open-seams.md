---
type: 驗證記錄
title: 尚未驗證的接縫
description: 還有哪些技術假設沒被程式碼證明；已解除的那些只留結論。
resource: ../../PLAN.md
tags: [驗證, 風險, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 主要風險
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 G6 附帶的三條硬約束
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 待定清單
---

**本頁記的是尚未被程式碼證明的技術假設**，每一條的目標都是變成「已驗證」或「此路不通」。

# ⏳ 還開著的

| 接縫 | 已知事實 | 待驗證 |
|---|---|---|
| **建置鏡像編不出 musl 產物** | 實測：Debian trixie 只有 `musl-gcc`（C），沒有 `musl-g++`，而 BoringSSL 的 `ssl/` 是 C++ | ✅ **已由 G131 結案**（曾掛號 D21）：口徑 = Alpine 原生 + qemu 跑 aarch64。✅ qemu 那一趟的耗時已量到：`ShanireHomePC` 2026-09-05 冷建 **18m07s**、`ShanirePCX` 2026-09-06 冷建 **34m05s**（⚠ 近兩倍，**機器本機**，⛔ 別互相照抄）。✅ **曾掛號 D24 的「何時跑 aarch64」已由 `G144` 結案 = 候選 ②**：只在 `Cargo.lock` 或那三張釘 rustc 的 Dockerfile 變化時跑。⚠ binfmt 是**機器本機狀態**，兩臺開發機現在都裝了 |
| **產物裡真的連結了哪幾套 TLS** | 相依圖裡只有一套（`cargo tree -e all --target all` 為空） | ✅ **已由 G138 結案**（曾掛號 D23）：判據掛在 musl 靜態產物上，`tests/ci/tls-linkage.sh` 取數 + `tests/musl/product.sh` 判**符號表**。⛔ 不判 `strings` —— `rustls` 那 4 次全來自純類型 crate `rustls_pki_types`，靠 Rust v0 名字修飾的長度前綴（`_6rustls` ≠ `_16rustls_pki_types`）才分得開 |

# ✅ 已解除的

| 接縫 | 結論 |
|---|---|
| 自建 Service 掛載 · socket 移交（認領路徑） | ✅ M0 實測：三類服務並掛，自建 TCP/UDP 監聽器的 fd 經 `SCM_RIGHTS` 傳到第二代 → [M0 接縫驗證](/verification/m0-seam.md) |
| 未被認領的繼承 fd 會怎樣 | ✅ **已修**（2026-09-24，pingora 0.9.0 的 `f82478a` + 樞衡與 spike 的每個自建服務都實作了 `listen_addresses()`）。常設判據 [`tests/m0/unclaimed.sh`](../../tests/m0/unclaimed.sh) 從「重現壞行為」反寫成「驗修復」：第二代關掉那個 fd、老一代退出後連接埠拒連、不再傳給第三代；反證實測：任一服務回傳 `None` ⇒ 它當場紅。見下 |
| **`bind()` 攥著全域 `ListenFds` 鎖**（vendor 0.8.1 時） | ✅ **2026-09-24 隨 rebase 到 0.9.0 解除**：上游 `1d9371191` 把 `ListenFds` 換成 `parking_lot::Mutex` + 按位址的非同步鎖（`flurry` 隨之進相依圖，過了供應鏈門）。⚠ 鎖換成同步鎖之後守衛不許跨 `.await` ⇒ 產品三處 + spike 三處取用改成短持鎖 |
| **`listen_addresses()` 的串接** | ✅ **2026-09-24 接上**：`fulcrum-server` 的 L4 TCP / L4 UDP / QUIC 三個自建服務回傳各自的 **fd 表鍵**（⛔ 不是 bind 位址 —— 那會讓新一代先關掉自己要繼承的 fd），由單測 `*_服务声明的正是它的_fd_表键` 釘住（先紅後綠）。⚠ 它帶預設實作（回傳 `None` = 關掉**整個處理程序**的清理），**漏實作不會有任何編譯錯誤** —— 這正是要單測的原因 |
| 移交來的監聽 fd 沒有 `FD_CLOEXEC` | ✅ 由 **G38** 在 fork 裡修掉（包 `OwnedFd` + `MSG_CMSG_CLOEXEC`），並已投上游（[pingora#959](https://github.com/cloudflare/pingora/issues/959) → [#960](https://github.com/cloudflare/pingora/pull/960)）。★ 上游走批量重放，**「PR 被 close」是成功不是拒絕** —— 看改動有沒有進 `main`。✅ **上游 0.9.0 已含（`b2b35fd`），fork 裡那處改動 2026-09-24 歸零**；M1 `[7/9]` 的三條斷言照舊留著（上游單測看不到 fork+exec 的逐代累加）|
| 兩個入口能否共用同一份挑證書實作 | ✅ **不是被驗證通過，是被取消了前提**：G104 換到 BoringSSL 之後共用的不再是 `ResolvesServerCert`，而是同一個 `select_certificate_callback` → [TLS](/architecture/tls.md) |
| BoringSSL 與 musl 靜態連結 | ✅ 已通過 → [musl + BoringSSL 靜態連結](/verification/musl-boringssl.md)。⚠ 卡點不在 musl 也不在 BoringSSL，在建置宿主 —— 換來了上面那條建置宿主口徑（曾掛號 D21，✅ 已由 G131 結案）|
| systemd 下的零停機升級 | ✅ 由 M1 通過，★ 但推翻了 G31 的一半 → [M1 spike #1](/verification/m1-systemd.md) |
| 升級視窗內 QUIC 連接歸屬 | ✅ 由 **G109** 解除：按 DCID 跨處理程序轉交，判據 [`tests/quic-relay/run.sh`](../../tests/quic-relay/run.sh)，見下 |
| L4 UDP 在升級視窗內的資料報分流 | ✅ 收到停機信號就不再 `recv_from`；⚠ **視窗被縮小不是被消滅** —— 在那之前兩代都在收 |
| `pingora-cache` 的抽象邊界 | ✅ 已核實（`MemCache` 是唯一的 `Storage` 實作），⚠ 而同日 **G82** 決定快取層完全自研 ⇒ 這條不再通向任何東西 |

# 三條要帶走的結論

## 未被認領的 fd 會保持 LISTEN 並把連接吞掉，而它只在老一代退出之後才顯形

> ✅ **2026-09-24 起這是已修掉的行為**（pingora 0.9.0 + 各服務宣告 `listen_addresses()`）：新一代會關掉它，
> 老一代退出後連接埠拒連。下面是 0.8.1 時的現象與教訓，留作出處 —— ★「先等第一代退出再探」這條判據寫法照舊有效。

第二代不掛某個監聽器時，那個 fd 仍被繼承、仍在 LISTEN：TCP 三次握手**照常成功**，請求
發出去之後**超時無回應**，並且逐代傳遞。

★ ★ ★ **連接層看起來完全健康** —— 任何只探「連接埠通不通」的健康檢查都會說它是好的。

★ 而 pingora 把 fd 發給新一代之後會硬等 `CLOSE_TIMEOUT` 才廣播停機，這段時間裡兩代都持有
同一個監聽 socket、老一代照常 accept ⇒ **升級後立刻做的健康檢查會是綠的**，問題要等老處理程序
走乾淨才暴露，而那時告警已經和「升級」這個動作脫鉤了。
⇒ 判據因此寫成「先等第一代退出，再探」，而不是「升級後 sleep 幾秒再探」。

## 「把資料報丟了」與「替我們把對端連接殺了」可能在同一個函式簽名上長得一模一樣

quinn 的 `Endpoint::handle()` 對認不出的 DCID **回 stateless reset** —— 語義是告訴對端這條
連接已經不存在。換代視窗裡，老一代的在飛連接只要有一個資料報落到新一代手裡，用戶端就會
**真的把它拆掉**，不是丟包重傳能扛過去的那種。而這個函式回傳的是 `Option<DatagramEvent>`：
唯讀類型簽名推出來的方向是錯的。

⇒ 這條事實是 G103 取 `quiche` 的依據之一，完整對照見
[HTTP/3 庫選型事實表](/platform/http3-libraries.md)。

## 一條只寫在驗證記錄裡的前置，不會在開工時被讀到

讀待辦的人看不見它，而讀這一頁的人已經在做別的事了。
⇒ 本頁往後新增的每一條「最晚要在 X 之前驗」，都要同時在 `PLAN.md` §11 掛一個 D 號。

同族的還有另外兩條：**「被驗證通過」與「被取消了前提」在結論裡長得一模一樣，而它們對判據的
要求相反** —— 後者意味著沒有任何判據需要為它存在，再寫一條門那條門會永遠綠；以及
**一個 spike 證明的是「這條路走得通」，不是「產品走在這條路上」**，而這個差別在門裡看不見，
因為門測的就是 spike。

# 一條方法論

**先讀原始碼得到推斷，再用真流量把推斷坐實 —— 推斷不是證據。**
M0 從類型簽名推出「UDP 在結構上沒有障礙」，仍然寫了 spike；而 spike 除了確認推斷，
還額外撈出了升級視窗內資料報分流這條風險，那是純讀原始碼看不出來的。

# 相關

[M0 接縫驗證](/verification/m0-seam.md) · [待定清單](/governance/open-questions.md) ·
[資料路徑](/architecture/data-path.md) · [TLS](/architecture/tls.md)
