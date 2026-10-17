---
type: 驗證記錄
title: M1 spike #1 · systemd 下的零停機升級
description: 通過，但推翻了 G31 的一半——撐住 unit 的是 ExitType=cgroup，而「搶 MainPID」會毀掉優雅停機。
resource: ../../tests/m1/run.sh
tags: [驗證, 已通過, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-14T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G31（處理程序模型 = Type=notify 前台執行；附帶強制做本 spike）
  - id: plan-10-g33
    resource: /references/plan.md
    title: PLAN.md §10 G33（路徑約定）、G34（回落層 systemd 依賴）
  - id: plan-10-d14
    resource: /references/plan.md
    title: PLAN.md §10 G37（D14 結案：Type=notify + 前台 + ExitType=cgroup，★ 不交接 MainPID）
---

★ **本頁記的是實際跑出來的東西，屬歷史事實。** 結論可以被後續實驗推翻，但「那幾次跑出了什麼」不會變。

> **結論：通過，但形狀與 G31 拍板時假定的不一樣。**
>
> systemd `Type=notify` + **前台執行** + **`ExitType=cgroup`** 之下，
> 一次 `systemctl reload` 能完成零停機換代：unit 全程不離開 `active`，三類流量零中斷，
> 停機仍然走完排空。
>
> ✅ **口徑已由 G37 拍板**（2026-08-14），兩條 fd 缺陷已由 **G38** 在 fork 裡修掉。
>
> ★ ★ ★ 而 G31 寫的「新處理程序必須落在同一個 cgroup 內**並搶過 MainPID**」——
> 前半句對，**後半句實測是錯的，而且有害**。詳見下面第 2 節。
>
> 日期：2026-08-14 ｜ 環境：`fulcrum-systemd:local`（Debian 13 trixie / **systemd 257.13-1~deb13u1**，
> 釘到 digest）＋ 產物由 `fulcrum-build:local`（Rust 1.97.1）建置
> ｜ 復跑：`bash tests/m1/systemd-run.sh`

# 0. 它要回答什麼

G31 把處理程序模型定成 systemd `Type=notify` + 前台執行環境，同時記下了一條**純靠讀文件與程式碼推出來、
沒有跑過**的衝突：

> pingora 的零停機升級是外部拉起新處理程序（`-u`），而 `Type=notify` 下老處理程序一退出，unit 即被
> 判定結束、預設 `KillMode=control-group` 會殺掉整個 cgroup。

owner 拍板時因此附了一條強制條件：**這件事必須先做 spike，判據照 M0 的形式寫死。**
理由是「★ 推斷不是證據」——M0 那次正是靠 spike 額外撈出了 UDP 分流那條風險。

**這一次它又撈出來了兩條**，而且其中一條恰恰是那條推斷本身。

# 1. 三個場景，三種口徑

| 場景 | 口徑 | 它證明什麼 |
|---|---|---|
| [`run.sh`](../../tests/m1/run.sh) | `ExitType=cgroup`，**不交接 MainPID** | 產品要保證的行為 |
| [`exit-type-main.sh`](../../tests/m1/exit-type-main.sh) | 去掉 `ExitType=cgroup` | 那一行確實在幹活；`run.sh` 的斷言分得清好壞 |
| [`mainpid-handover.sh`](../../tests/m1/mainpid-handover.sh) | G31 推斷的形狀（`ExitType=main` + 交接） | 它**為什麼被否掉** |

★ 後兩個指令碼重現的是**壞行為**，所以它們的綠 = 壞行為照常發生 —— 與
[`tests/m0/unclaimed.sh`](../../tests/m0/unclaimed.sh) 原先同類（它 2026-09-24 rebase 到 pingora 0.9.0 後已反寫成驗修復）。
它們紅了的含義是「行為變了」，不是「測試壞了」。

# 2. ★ ★ ★ 最值錢的一條：交接 MainPID 會**悄悄弄丟優雅停機**

同一份二進位、同一份設定，只改 unit 檔案裡的兩行，實測三種組合：

| 口徑 | 老代退出後 unit | `systemctl stop` |
|---|---|---|
| 交接 MainPID（**G31 推斷的形狀**）| `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |
| **`ExitType=cgroup`，不交接** | `active` ✅ | **10 秒走完排空，`success`** ✅（0.8.1 時；0.9.0 下 5 秒，見 §3）|
| 兩者都用 | `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |

**機制**：交接過去的 pid 不是 systemd 親生的（它是老處理程序 fork 出來的），systemd 會打一句

```
fulcrum-m1.service: Supervising process 174 which is not our child.
We'll most likely not notice when it exits.
```

並把它標成 alien。此後停機**不再等它排空**——SIGTERM 與 SIGKILL 幾乎同時發出。

★ **為什麼這條特別危險**：整個失效**在升級當時沒有任何症狀**。換代成功了、流量零中斷、
`systemctl status` 一切正常。代價要到**下一次停機**才兌現，而那時它已經和「升級」這個動作
完全脫鉤了——重新啟動、`systemctl restart`、機器關機，全部變成硬殺連接。
（這與 [open-seams.md](/verification/open-seams.md) 裡「黑洞不是立刻出現的」是同一類陷阱：
**判據取早了就什麼都看不見**。）

★ 順帶更正 systemd 那句警告：它說「多半察覺不到它退出」，**實測不對**——新一代在老代退出後會被
reparent 給 PID 1（也就是 systemd 自己），SIGKILL 掉它之後 systemd **1 秒內**就報了
`Main process exited, code=killed`。真正的後果不是「察覺不到」，而是**停機不再等它**。

## 換成 `ExitType=cgroup`

`ExitType=`（systemd ≥ 250）換掉了「unit 什麼時候算結束」的判據：

- `main`（預設）：主處理程序退出 = unit 結束
- **`cgroup`：cgroup 裡還有處理程序就算活著**

於是**根本不需要交接**：老代退出時 cgroup 裡還有新代，unit 照常 `active`；
停機時 systemd 照常等整組排空。

⚠ **已知代價**：第一次升級之後 `MainPID` 變成 **0**。直接後果是
`ExecReload=/bin/kill -USR2 $MAINPID` **第一次能成、第二次失敗**（`$MAINPID` 展開成空）。
★ **失敗方式是實測的，不是推的**：`systemctl reload` 結束代碼 **1**，journal 裡留下 `kill` 的 Usage 與
`Reload failed`，而 **unit 仍是 `active/success`**——也就是說**升級沒發生，服務照常跑著老一代**。
⚠ 本頁初稿把它寫成「靜默失敗」，實測推翻了那個詞：它吵得很。**升級沒發生**才是真正的後果。
★ 所以本 spike 讓每一代自己寫 pid 檔案，`ExecReload` 從檔案裡找當前這一代——
**而「只升一次」的測試看不見這個洞**，`run.sh` 因此連升兩次。

# 3. 主場景實測（`run.sh`，九步）

```
[1/9] start          unit active/running，MainPID=gen1，三端口在听，pid 文件 = gen1
[4/9] reload → gen2  pid 文件换代，全程 active/running，两代重叠窗口成立
[5/9] gen1 退出      unit 仍 active，gen2 活着，MainPID 归零
[6/9] 探针           http 376/0   tcp 377/0   udp 377/0   （成功/失败）
[7/9] reload → gen3  连升两次都成立
[8/9] stop           10 秒，Result=success，cgroup 已空
```

**三類流量零中斷**：HTTP 請求 0 失敗、跨升級的 TCP 長連接 0 斷開、UDP 回聲 0 丟失——
判據與探針與 M0 是同一份（[`m0-probe`](../../spikes/m0-seam/src/bin/probe.rs)），
所以兩組數字可以直接對讀。

★ **停機耗時的下界也是判據**：`stop` 若秒回，說明它壓根沒等排空，那「`TimeoutStopSec` 要按
排空時長配」這條結論就是假的——而秒回恰恰就是交接方案的失敗形狀。
0.8.1 時實測 10 秒 = `grace_period_seconds`(5) + `graceful_shutdown_timeout_seconds`(5)，當時記成「與公式吻合」——
⚠ **那是碰巧吻合**：0.8.1 等完各 runtime 之後還要再 `sleep(graceful)` 一次，而 runtime 本身幾乎是立刻退出的
⇒ 10 秒裡的後 5 秒全是那次多出來的 sleep，`graceful` 超時本身沒用上。
✅ 2026-09-24 rebase 到 0.9.0（上游 `6f15714` 刪了那次 sleep）後實測：本場景 **5 秒** = grace(5) + runtime 退出（≈ 0）；
產品場景（`M1_ONLY=product`，grace 30 + graceful 5）**30 秒**，預算 35。
`run.sh` 判的是下界（≥ grace），兩版下都成立。

> ⚠ 產品預設設定（M0 用的 30 + 30）對應的量級是 **35–65 秒**，`TimeoutStopSec` 必須按它配。
> ⚠ 這組數是按原始碼公式推的：0.8.1 下還要再加一次 graceful（真實 65–95 秒），0.9.0 起公式才與實際相符。
> 依據見 [`tests/m0/lifecycle.sh`](../../tests/m0/lifecycle.sh) 裡實測出的那張表：
> **一旦某一代開始排空，它就收不到任何可捕獲的信號了**，`systemctl stop` 之後再補一刀是沒用的。

# 4. ★ ★ 另外撈到的兩條 fd 缺陷（M0 結構上不可能覆蓋）—— ✅ 已修

M0 的第二代是**從 shell 起的**，與第一代沒有 fork 關係。M1 改成「老處理程序自己 fork 下一代」
（這是「新處理程序落在同一個 cgroup 內」最省事的做法）之後，子處理程序會繼承父處理程序
**所有沒設 `CLOEXEC` 的 fd**——而經 `SCM_RIGHTS` 收來的 fd 恰恰沒有：

- `recvmsg` 呼叫時沒帶 `MSG_CMSG_CLOEXEC`（`vendor/pingora/pingora-core/src/server/transfer_fd/mod.rs`）
- `pingora-core` 全倉搜不到任何一處 CLOEXEC 設定
- spike 自己的 `TcpListener::from_raw_fd` 同樣不會補上

實測（每一代都在「前一代已退出」的安靜時刻量的，數的是 `ss -p` 列出的 `fd=` 個數）：

| | 8080 | 8081 | 8082 | 為什麼 |
|---|---|---|---|---|
| gen1 | 1 | 1 | 1 | 自己 `bind`，std 建的 fd 帶 CLOEXEC |
| gen2 | 1 | 1 | 1 | fork 自 gen1，但 gen1 的 fd 有 CLOEXEC，繼承不過來 |
| **gen3** | **2** | **2** | **2** | ★ 繼承 gen2 的一份（無 CLOEXEC）+ `SCM_RIGHTS` 再收一份 |

**原因也已實測**，不是隻讀原始碼推的：`/proc/<gen3>/fdinfo/<fd>` 的 `flags`（八進制）裡
`O_CLOEXEC`(02000000) 位是 0，兩個 fd 都是。

**後果**（按機制推，量級未實測）：

1. **逐代累加**——gen4 會有 3 份、gen5 有 4 份。升級次數多的長命處理程序會慢慢吃 fd。
2. ★ 更要緊的一條：**上游那個還沒發版的清理機制會被繞過。**
   `f82478ae` 的 `listen_addresses()` 關的是 **fd 表裡**那些沒人認領的 fd，
   而**繼承進來的那一份根本不在 fd 表裡**。也就是說
   [open-seams.md](/verification/open-seams.md) 裡那個「未被認領的 fd 黑洞化」問題，
   在 fork 式升級下**即使上游修好了也可能仍然存在**。

## 4.1 ★ 還有一條：accept 出來的傳輸 socket 從來不 close

讀上游程式碼準備投稿時發現的**第二處**，在同一個函式里：`get_fds_from()` 結尾只
`close(listen_fd)`，而 `accept_with_retry_timeout()` 回傳的那個連接是**裸 `RawFd`、沒有
`Drop`**，此後既不 close 也不轉移——**每完成一次優雅升級就永久洩漏一個已連接的 unix socket**。

實測（按 `/proc/<pid>/fd` → `/proc/net/unix` 反查，數的是路徑為 upgrade.sock 的那些）：

| | 攥著的 upgrade.sock fd |
|---|---|
| gen1（從未經歷過移交）| 0 |
| gen2（一次移交）| **1**（`St=03` CONNECTED，即 accept 出來的那一端）|
| gen3（移交 + 繼承）| **2**（自己漏一個 + 從 gen2 繼承一個）|

★ ★ **兩個缺陷會疊乘**：洩漏的那個 socket 自己也沒有 CLOEXEC，於是逐代累加。
★ `St=03`（CONNECTED）是關鍵判據——它證明洩漏的是**accept 出來的連接**而不是監聽 socket
（後者確實被正確關掉了）。

## 4.2 ✅ 修法（G38）與它守在哪

兩處都在 fork 裡修掉了（`vendor/pingora` 的 [`FORK.md`](../../vendor/pingora/FORK.md) §4）。
✅ **2026-09-24：上游 0.9.0 已含這兩處修復**（`b2b35fd`，owner 的 PR #960）⇒ fork 那處改動歸零，檔案整份取上游。
`[7/9]` 三條斷言**保留**（上游單測只看單個處理程序，看不到 fork+exec 下的逐代累加），失敗提示改指上游。
★ 反證同日重做：把 `MSG_CMSG_CLOEXEC` 改回 `empty()` ⇒ 第三代的監聽 fd 重數變成 `[8080:2 8081:2 8082:2]`，`[7/9]` 當場紅
（第二代仍是 1 —— 與當年一樣，累加要到第三代才顯形）。

| | 修法 |
|---|---|
| ① accept 的連接不 close | 包成 `OwnedFd` 交給 `Drop`，順帶覆蓋 `cmsgs()?` 的提前回傳路徑 |
| ② 收來的 fd 無 CLOEXEC | `MsgFlags::empty()` → **`MsgFlags::MSG_CMSG_CLOEXEC`**（原子，避免收完再 `fcntl` 與 fork 之間的競態視窗）|

修完複測：三代的監聽 fd 重數**全部是 1**，攥著的 upgrade.sock **全部是 0**。

**守門人**是 `run.sh` [7/9] 的三條斷言（fd 重數、CLOEXEC 位、upgrade.sock 計數）。
★ 它們同時是 **rebase 的守門人**：上游若未接受這兩條，而 rebase 時漏了重做，它們會紅。

★ **上游 0.8.1 與當前 main 都有這兩處**（2026-08-14 核對過 main 的同一函式），
且上游儲存庫**沒有任何 issue/PR 提過 CLOEXEC**（後來 owner 投了 #959 / #960，已進 0.9.0）。投稿材料見
[`upstream-pr/`](../../upstream-pr/README.md)，按 G32 的流程由 owner 本人提交。

# 5. 順帶坐實與更正的幾條

| | 結論 | 怎麼來的 |
|---|---|---|
| G33「`PIDFile=` 不需要」 | ✅ 坐實。前台模式下 pingora **不寫 pid 檔案**（`pid_file` 只在 `daemonize()` 裡被讀），`/tmp/pingora.pid` 不存在即證明前台模式生效 | `run.sh` [1/9] 的雙向斷言 |
| ⚠ `error_log` / `pid_file` 會被**安靜忽略** | 前台模式下這兩個設定項 pingora 一個都不讀。設定裡寫了路徑、檔案永遠不出現，且沒有任何提示 | 讀 `server/daemon.rs` + 實測 |
| G31「前台執行保住了 `run()` 之前的執行緒」 | ✅ 坐實。升級觸發器就住在那樣一個執行緒裡，它活著 | `systemctl reload` 能工作即是證據 |
| `NotifyAccess` 可以收緊到 `main` | ✅ 不交接 MainPID 之後，發通知的永遠是主處理程序自己，不再需要 `all` | `run.sh` 用的就是 `main` |
| systemd 那句 "not our child" 警告 | ⚠ 措辭不準，見第 2 節 | 實測 1 秒內即察覺 |

# 6. ★ 一條把結論差點帶反的環境坑

第一次搭測試宿主時，容器是按「網上通行做法」起的：`--privileged` **加上**
`-v /sys/fs/cgroup:/sys/fs/cgroup:rw`。結果：

1. `journald` 起不來（`Failed to acquire cgroup root path: Protocol driver not attached`），於是**沒有任何紀錄**；
2. ★ ★ **`MAINPID=` 交接被靜默丟棄**——systemd 無法把發信處理程序解析到某個 unit 上。

於是第一輪實驗的現象是「交接沒生效、unit 照常死掉」，**差一點就寫成
「systemd 不接受來自非親生處理程序的 MainPID 交接」這個完全錯誤的結論。**

根因是那個 bind mount：容器用的是私有 cgroup namespace，再把宿主機的 cgroup 樹掛進來，
兩邊對不上。cgroup v2 下 `--privileged` 已經會把正確的視圖掛成 rw，**不需要也不應該再掛一次**。

★ 教訓與本儲存庫已有的那幾條同形：**先讓工具自己證明它沒瞎，再信它給的結論**。
這裡救場的是「journald 也掛了」這個旁證——**兩個症狀同源，才讓人去查環境而不是去查 systemd**。

# 7. 判據在哪

| | 檔案 |
|---|---|
| 主場景 | [`tests/m1/run.sh`](../../tests/m1/run.sh) |
| 反證（去掉 `ExitType=cgroup`）| [`tests/m1/exit-type-main.sh`](../../tests/m1/exit-type-main.sh) |
| 反證（G31 推斷的形狀）| [`tests/m1/mainpid-handover.sh`](../../tests/m1/mainpid-handover.sh) |
| 共用判據原語 | [`tests/m1/lib.sh`](../../tests/m1/lib.sh) |
| 宿主機驅動 | [`tests/m1/systemd-run.sh`](../../tests/m1/systemd-run.sh) |
| unit 檔案 | [`tests/m1/fulcrum-m1.service`](../../tests/m1/fulcrum-m1.service) |
| 設定 | [`conf/m1.yaml`](../../conf/m1.yaml) |
| spike 本體 | [`spikes/m1-systemd/src/main.rs`](../../spikes/m1-systemd/src/main.rs) |
| 測試宿主鏡像 | [`docker/Dockerfile.systemd`](../../docker/Dockerfile.systemd) |

★ 流量服務與探針**重複使用 M0 的那一份**（`spikes/m0-seam` 現在多了個 `lib.rs`），
不是複製。理由：三類流量是「零停機」的判據本身，**判據有兩份就等於沒有判據**。

# 相關

[M0 接縫驗證](/verification/m0-seam.md) · [尚未驗證的接縫](/verification/open-seams.md) ·
[處理程序與元件邊界](/architecture/process-model.md) · [待定清單](/governance/open-questions.md) ·
[建置與驗證](/platform/build-and-test.md)
