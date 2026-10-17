---
type: 技術基線
title: 建置與驗證
description: 一切在 Docker 裡跑（G26）；宿主機只需 Docker；SIGQUIT + fd 移交是 Linux 獨有的，Windows 上根本沒法驗。
resource: ../../tests/m0/docker-run.sh
tags: [建置, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G26（建置與驗證環境 = Docker Desktop）
  - id: agents
    resource: /references/agents.md
    title: AGENTS.md「Building and testing」
---

# 怎麼跑

```bash
bash tests/m0/docker-run.sh                  # 建置 + 下面全部場景
BUILD_ONLY=1     bash tests/m0/docker-run.sh # 只建置（⛔ 不含測試目標）
COMPILE_ONLY=1   bash tests/m0/docker-run.sh # 只編譯，含**全部測試目標**，一條測試都不跑
LINT_ONLY=1      bash tests/m0/docker-run.sh # 只跑 fmt + clippy + shellcheck
LINT=0           bash tests/m0/docker-run.sh # 跳過 lint
UNIT_ONLY=1      bash tests/m0/docker-run.sh # 只跑樞衡自己那幾個 crate 的測試
UNIT_TESTS=0     bash tests/m0/docker-run.sh # 跳過它們
SERVE_ONLY=1     bash tests/m0/docker-run.sh # 只跑資料面端到端（真流量）
SERVE_TESTS=0    bash tests/m0/docker-run.sh # 跳過它
L4_ONLY=1        bash tests/m0/docker-run.sh # 只跑 L4 面端到端（M2 批 A–D）
L4_TESTS=0       bash tests/m0/docker-run.sh # 跳過它
FILES_ONLY=1     bash tests/m0/docker-run.sh # 只跑自研靜態檔案端到端（M2 批 F）
FILES_TESTS=0    bash tests/m0/docker-run.sh # 跳過它
CACHE_ONLY=1     bash tests/m0/docker-run.sh # 只跑自研 HTTP 快取端到端（M2 批 G）
CACHE_TESTS=0    bash tests/m0/docker-run.sh # 跳過它
CACHEDISK_ONLY=1  bash tests/m0/docker-run.sh # 只跑快取磁碟後端端到端（M2 批 H）
CACHEDISK_TESTS=0 bash tests/m0/docker-run.sh # 跳過它
ENCODE_ONLY=1    bash tests/m0/docker-run.sh # 只跑回應壓縮端到端（M2 批 I）
ENCODE_TESTS=0   bash tests/m0/docker-run.sh # 跳過它
H3_ONLY=1        bash tests/m0/docker-run.sh # 只跑 HTTP/3 端到端（M2 批 J）
H3_TESTS=0       bash tests/m0/docker-run.sh # 跳過它
PP_ONLY=1        bash tests/m0/docker-run.sh # 只跑 HTTP 面的 PROXY protocol（M2 批 L ①）
PP_TESTS=0       bash tests/m0/docker-run.sh # 跳過它
LOG_ONLY=1       bash tests/m0/docker-run.sh # 只跑結構化訪問紀錄（M2 批 L ②③）
LOG_TESTS=0      bash tests/m0/docker-run.sh # 跳過它
METRICS_ONLY=1   bash tests/m0/docker-run.sh # 只跑 Prometheus 指標端到端（M2 批 M）
METRICS_TESTS=0  bash tests/m0/docker-run.sh # 跳過它
RELAY_ONLY=1     bash tests/m0/docker-run.sh # 只跑換代時的 QUIC 跨處理程序轉交（M2 批 K）
RELAY_TESTS=0    bash tests/m0/docker-run.sh # 跳過它
MUSL_ONLY=1      bash tests/m0/docker-run.sh # 只跑產品 musl 靜態產物那一格
MUSL_TESTS=0     bash tests/m0/docker-run.sh # 跳過它
SMOKE_ONLY=1     bash tests/m0/docker-run.sh # 只跑冒煙自證
SMOKE_TESTS=0    bash tests/m0/docker-run.sh # 跳過它
STRESS_ONLY=1    bash tests/m0/docker-run.sh # 只跑壓力
STRESS_TESTS=0   bash tests/m0/docker-run.sh # 跳過它
BENCH_ONLY=1     bash tests/m0/docker-run.sh # 建置後只跑對拍流水線與判據，不產出效能數字
BENCH_TESTS=0    bash tests/m0/docker-run.sh # 跳過對拍那一格
ACME_ONLY=1      bash tests/m0/docker-run.sh # 只跑 ACME 簽發端到端（pebble 當本機 CA，G64）
ACME_TESTS=0     bash tests/m0/docker-run.sh # 跳過它
RENEW_ONLY=1     bash tests/m0/docker-run.sh # 只跑 ACME 續期端到端（G58 的「續期」半邊）
RENEW_TESTS=0    bash tests/m0/docker-run.sh # 跳過它
VENDOR_ONLY=1    bash tests/m0/docker-run.sh # 只跑 fork 迴歸網（rebase 後的第一道門）
VENDOR_TESTS=0   bash tests/m0/docker-run.sh # 跳過 fork 迴歸網
UNCLAIMED_ONLY=1 bash tests/m0/docker-run.sh # 只跑「未被認領的繼承 fd」場景
M1_TESTS=0       bash tests/m0/docker-run.sh # 跳過 systemd 場景

bash tests/m1/systemd-run.sh                 # 單獨跑 systemd 場景
M1_ONLY=main     bash tests/m1/systemd-run.sh
M1_KEEP=1        bash tests/m1/systemd-run.sh # 失敗後保留容器，進去看現場
```

## ★ 兩道宿主機側的前置檢查（進容器之前，`DOCS_GATE=0` 一起關掉）

它們**不是場景**：純文字、幾十毫秒、各 `*_ONLY` 模式下照跑，排在最前是因為紅了指得最準。
⚠ 留在宿主機上**不是因為容器裡跑不了**（鏡像裡有 `python3`），而是它們不需要 Linux。

| 門 | 它答的問題 | ⛔ 它答不了的 |
|---|---|---|
| [`tools/docs-check.py`](../../tools/docs-check.py) | 每一份文件有沒有 frontmatter，以及**從 `index.md` 走不走得到**（傳遞閉包）| 內容對不對 |
| [`tools/plan-refs.py`](../../tools/plan-refs.py) | ① `open-questions.md` 那張表與 `PLAN.md` §11 是否**逐字相等**；② 有沒有哪一處**聲稱某個 D 號還在 §11 裡**而它已經不在了 | 不帶 `§11` 字樣的散文、跨行的句子、以及「把還開著的問題掛在已結案號下」這類**語義**錯 —— 三條都寫在它自己的文件裡 |

★ 後者立於 2026-09-03：一次體檢在**一批**抄件裡讀到「它還開著」這句假話，
而在它出現之前，**本儲存庫沒有任何一道門看得見一句過期的註解**。
⛔ **這裡有意不寫那批有幾處** —— 一個寫在散文裡的計數沒有任何門守著，
而本頁開頭那張場景清單已經為同一件事漂過好幾次。
⚠ 它的兩條判據性質不同 —— ① 完全機械、永不誤報；② 是句式判據，**天生有漏網**。

## ★ ★ `pre-push` 門：編譯不過的樹不許離開這臺機器

**起因（2026-09-04 實付代價）**：`9c0634f` 是一次**手動保盤提交** —— 測試裡引用一個還沒
定義的常數，三處 `E0599`，整個 test crate 編譯不過，**而它已經被推上去了**。
⚠ ⚠ 它躲過了每一個便宜的讀數：`git log` 乾淨、`git status` 乾淨、兩道文件門全綠、
`origin/main..main` 是 0 —— **全都說「一切正常」**。⇒ 唯一判得動它的是**真去編一次**。

- **是 `pre-push` 不是 `pre-commit`**：那種半成品提交本來就是要的（防資料丟失），
  攔它等於攔掉它的全部價值。要攔的是**離開這臺機器**那一步。
- **`shellcheck` + 編譯，⛔ 一條測試都不跑**：本機常年 20+ 工作階段並行，按空載餘量設的超時會
  週期性假紅，而一道假紅過幾次的 hook 會被 `--no-verify` 永久繞過 —— 那時它連編譯都不看了。
  ⇒ 這兩件事**都不會因機器負載而紅**。
  ⚠ 代價寫在明處：**它不攔「編得過但測試紅」**，也不攔 `fmt` 差異與 clippy 告警，
  那三格歸完整門禁。
- ⚠ ⚠ ★ **`shellcheck` 是 2026-09-05 補進來的（G134），而它此前不在這道門裡** —— 起因是一次
  實測：那天改了兩個 `.sh`，跑了**四趟** `COMPILE_ONLY` 全綠，而那一格當時是
  `CMD='cargo test …'` **直接替換**、不是 `CMD="$CMD && …"` 追加 ⇒ 它不含 `LINT_CMD`。
  ★★★ **一次純 shell 的改動可以完整穿過這道門**，而那四個綠對它零判別力。
  ⛔ **不掛 `clippy`**：它要先把相依（含 BoringSSL）編出來，會把這道門從秒級拖成分鐘級，
  而**門變慢正是它被 `--no-verify` 繞過的起點**；`shellcheck` 唯讀原始碼、在同一個已起的容器裡跑。
  ★ 體檢法：讀 `*_ONLY` 那串 `elif` 時先看它是 `CMD="$CMD && …"` 還是 `CMD='…'` ——
  一個字元之差，決定 lint 那一整格在不在。
- ★ ★ **shellcheck 按被推的提交決定跑不跑（G158，2026-10-06）**：實測這道門三趟 33–61s，
  網路只佔約 2s；shellcheck 一項 15–33s，宿主機側自測與遺留卷報告 13–20s。
  ⇒ 被推那一段兩端的樹在 `*.sh` · `*shellcheckrc` · `docker/Dockerfile.build` 上有淨改動才跑；
  判不出來（新分支、遠端那一端本機沒有、`git diff` 失敗）一律照跑。遺留卷報告在這條路徑上
  不跑，歸完整門禁。改後實測：不碰 `.sh` 的推送 10.2s，碰了的 43.8s。
  ⚠ 代價：被推的提交沒碰 `.sh` 時，工作樹裡未提交的壞 `.sh` 不攔。
  判據是 `tests/ci/pre-push.sh --self-check`，掛在 lint 那一格。
- ★ ★ **宿主機自測按指紋跑（G159，同日）**：G158 原本在這條路徑上無條件跳過它們，代價是
  「改壞門的那一刻」恰好不驗。現在指紋 = `docker-run.sh` + `tests/lib/*.sh` 的內容 +
  宿主工具身分（bash · MSYS 執行環境 · Git for Windows 版本）；本工作樹有同一指紋的通過記錄
  （`.git/fulcrum-host-selftest.ok`，worktree 裡是各自的 git 目錄）才跳過，否則當場跑十條（約 7s）。
  記錄只由真跑通過的一趟寫下，要跑就先撤掉舊的；完整門禁不問記錄、每趟照跑並刷新。
  ⚠ 剩下的盲區：指紋沒覆蓋到的環境變化；判據函式壞成「恆答對得上」只有完整門禁看得見。
  實測：同指紋 7.9s 跳過；沒有記錄或指紋不同時照跑。
- **走 `COMPILE_ONLY` 那一格**，⛔ 不自己拼 `docker run` —— 建置鏡像、`target` 卷名、
  那把「同一棵樹只許跑一次」的鎖、行尾位元組探針、兩道宿主機側的門，全部只有一份推導。
  它給那一格傳 `COMPILE_SHELLCHECK=<判据结果> HOST_SELFTESTS=auto VOL_REPORT=0`，三個都是
  顯式賦值，環境裡帶進來的同名變數蓋不過它們；⛔ 完整門禁不帶這三個開關跑。
  ⛔ 宿主機自測沒有「無條件跳過」這一檔：除 `auto` 以外的任何值都照跑。
- ⚠ ⚠ **盲區**：它量的是**工作樹**，不是被推的那幾筆。工作樹幹淨時兩者等同（`9c0634f`
  那次正是），髒樹時指令碼會**明說自己量的是什麼**，⛔ 不假裝它守住了被推的狀態。

⚠ **`.git/hooks/` 不在版本控制裡** ⇒ 換機器、重新 clone 都會**靜默地沒有這道門**。裝一次：

```bash
printf '#!/usr/bin/env bash\nexec bash "$(git rev-parse --show-toplevel)/tests/ci/pre-push.sh" "$@"\n' > .git/hooks/pre-push && chmod +x .git/hooks/pre-push
```

⛔ 指令碼**不提供自己的繞過開關**（一個隨手能設的開關會把門變成建議，與「不留逐行豁免記號」
同一條紀律）；真要繞過只有 `git push --no-verify`，那是一次顯式的、看得見的動作。
★ 2026-09-04 實測：**壞樹被攔住且本機裸倉收到 0 個 ref**（一個位元組都沒出去）· 好樹放行 ·
只有刪除時跳過且一次 docker 都不起 · 髒樹時那句「本次量的是工作樹」逐字出現。
★ 2026-10-06 實測（G158）：SC2086 探針在「新分支 + 環境裡帶 `COMPILE_SHELLCHECK=0`」下
被攔住並點名那個檔案 · 同一探針在不碰 `.sh` 的範圍下放行（即上面那條代價）·
不帶開關直接跑 `COMPILE_ONLY` 時九條宿主機自測、遺留卷報告與 shellcheck 照舊都在。
★ 2026-10-06 實測（G159）：五種注入（指紋恆定 · 不看內容 · 不看工具 · 讀不到也給指紋 ·
恆答對得上）在副本上各只紅在該紅的那幾條 · 預設模式下讓 `mktemp` 失敗 ⇒ 記錄被撤掉，
下一次 pre-push 照跑 · 記錄有效時直接跑 `COMPILE_ONLY` 十條自測全部執行。
反證做法（注入寫在位元組副本上、`sha256` 核還原、⛔ 不用 `git checkout`）見
[`tests/ci/pre-push.sh`](../../tests/ci/pre-push.sh) 的檔案頭。

## 場景清單

⚠ ⚠ **這張表沒有門看著**，而它已經漂過多次 ——
**一張自己不會變紅的清單只會越落越遠，落後時讀起來還完全正常。**
★ 加場景時，**加進這張表就是加場景的一部分**。

> ★ [`tests/musl/probe.sh`](../../tests/musl/probe.sh)（musl + BoringSSL 靜態連結探針）
> **故意不是**一個常設場景 —— 它編的是 spike，答不了「產物是不是單靜態二進位」（G108）。
> ⚠ 但它照樣被 lint 那一格的 `shellcheck` 掃到 —— 「不在門禁裡跑」與「不被 lint 看」是
> 兩件事。★ 現在這是**推導的結果**（掃的是 `tests/` 整棵樹），不再需要誰記得往清單里加一項。



| 場景 | 入口 | 判據 |
|---|---|---|
| ★ **lint** | `cargo fmt --all -- --check`（另對兩個獨立 workspace 各補一條 `--manifest-path`：`spikes/musl-boringssl`、`vendor/pingora` —— 根那條 `--all` 夠不到它們）+ `cargo clippy --workspace --all-targets -- -D warnings` + `shellcheck` | 見下 |
| ★ **樞衡自己的 crate 測試** | [`tests/unit/run.sh`](../../tests/unit/run.sh) | `cargo test --workspace --locked --no-fail-fast` 全綠，**且條數不低於下界**，見下 |
| **fork 迴歸網** | [`tests/vendor/run.sh`](../../tests/vendor/run.sh) | ★ 不是「零失敗」，而是**失敗集合與官方原版 0.8.1 逐項相同**（現為 2 條）。⚠ 它**相依容器能上公用網路**，見下 |
| ★ **資料面端到端** | [`tests/serve/run.sh`](../../tests/serve/run.sh) | 真流量走完路由 → 轉發 → `rewrite` → `header_up` → 重定向 → 421 → **回落三向**（501 / 轉發 / 502）→ keep-alive → 一個約 4 KiB 的回應只許一個資料段（寫緩衝，G155）→ 錯誤頁裡的 `{header.X}` 取原始請求頭（第 11 節）|
| ★ **L4 面端到端** | [`tests/l4/run.sh`](../../tests/l4/run.sh) | TCP/UDP 透傳、SNI/ALPN 分流（**不終止 TLS**，位元組原樣重放）、PROXY protocol 的收與發、★ **換代時 L4 長連接不斷**（自建監聽器參與 socket 移交）|
| ★ **靜態檔案端到端** | [`tests/files/run.sh`](../../tests/files/run.sh) | 索引 / 尾斜槓 301 / browse / ETag+304 / 單段 Range / MIME；★ 它有一條別處都沒有的判據：**路徑穿越與 `hide` 清單** —— 那兩樣壞掉時服務完全正常，只是多發了幾個檔案 |
| ★ **HTTP 快取端到端** | [`tests/cache/run.sh`](../../tests/cache/run.sh) | RFC 9111 語義：命中/回源、`no-store`/`private`/`Set-Cookie` 不存、備援 `ttl`、`Vary` 兩分支共存、請求側 CC、`purge`、防驚群。★ 最貴的一條：**帶 `Authorization` 的回應不許發給別人** |
| ★ ★ **快取磁碟後端端到端** | [`tests/cache/disk.sh`](../../tests/cache/disk.sh) | ★ ★ ★ 決定性的那條是**把處理程序殺掉再起來東西還在**（同一次跑裡用去掉 `disk` 的同一份設定反證：記憶體後端必然回源）。另有：啟動不掃盤 / 優雅停機存索引 / **重驗證只改 meta 不動 body** / 索引冷時 `purge` 照樣清得掉 / 壞條目即丟即刪 / 目錄用不了則關快取但照常轉發 |
| ★ ★ **回應壓縮端到端** | [`tests/encode/run.sh`](../../tests/encode/run.sh) | 反代與靜態檔案兩條路真的壓了（★ **解出來逐字等於原文**，不是「有那個頭」）/ 不該壓的三種都沒壓 / ★ 預壓縮旁檔案逐字發出且**強 ETag 與 Range 都在** / ★ 陳舊旁檔案被當成不存在 / ★★★ **壓縮 × 快取**：寫法不同但首選相同的命中同一條、不認壓縮的拿不到壓縮位元組、兩種表示共存 |
| ★ ★ ★ **HTTP/3 端到端** | [`tests/h3/run.sh`](../../tests/h3/run.sh) | ★ 它把批 J 從「零件通了」變成「入口在跑」：在它之前**沒有任何設定能打開那個監聽器**。★ ★ **用戶端是 curl 的 OpenSSL-QUIC 棧**，與被測的 quiche 沒有一行共同程式碼 ⇒ 這是本儲存庫對 h3 唯一的**互操作**判據（`quic` 模組裡那兩條走真 UDP 的端到端用的是 quiche 自己的用戶端，兩邊一起理解錯時會一起綠）。⚠ 全場 `--http3-only`，**不是** `--http3`（後者會回落到 h1/h2）；開跑前拿一個空 UDP 連接埠自證這把尺子真的在走 QUIC。五件事：h3 真的通 / **h3 與 h1 跑同一條執行鏈**（三種 outcome 逐字相同）/ ★★★ **逐跳頭在 h3 那側消失而 h1 上仍在**（RFC 9114 §4.2，自帶對照）/ ★★★ **`Alt-Svc` 出現在六條互不相同的回應路徑上** / **兩條反向**（明文連接埠不發、h3 自己不發）|
| ★ ★ **PROXY protocol（HTTP 面）** | [`tests/proxyproto/run.sh`](../../tests/proxyproto/run.sh) | **M2 批 L ①**。它是唯一一格量**連接開頭那幾個位元組**的 —— 在 TLS 握手**之前**。v1/v2 兩種頭 · 與首包合並行送 · `LOCAL` 不覆蓋對端 · ★ **兩條反向**（不在信任清單裡就一個位元組都不讀；另一個執行個體有意不配它當對照物）· 換設定立刻生效 |
| ★ ★ **結構化訪問紀錄** | [`tests/log/run.sh`](../../tests/log/run.sh) | **M2 批 L ②③**。★ ★ ★ **判據掛在那一行的內容上，不是「檔案非空」** —— 契約定的是欄位清單，而「有輸出」與「輸出對」之間隔著整張表。欄位用 `python3` 讀，**絕不 `grep '"status":200'`**（欄位順序/空格/轉義一變就靜默漏判）。一條請求正好一行 · `uri` 是 `rewrite` **之前**那個 · 打不開的紀錄路徑是**裝載期硬錯誤** · `outcome` 閉集逐個走一遍 · 白名單頭（★ 一條帶 `Authorization`/`Cookie` 的請求，整行**原始位元組**裡一個都找不到）· TLS 四格（★ **`--no-alpn` 那條反向是唯一分得出「量出來的」與「猜出來的」的判據**）· 敏感頭**兩條路各攔一次** |
| ★ ★ **Prometheus 指標** | [`tests/metrics/run.sh`](../../tests/metrics/run.sh) | **M2 批 M**（G116–G121）。★ ★ ★ **判據掛在「增量」與「交叉對照」上，不掛在「抓到了東西」** —— 一個把指標硬編碼成常數的實作，能通過每一條只問「正文裡有沒有那幾個字」的判據。⚠ 計數發生在 `Record::finish`（回應寫完**之後**）⇒ 第 N 次抓取看到的是前 N−1 次的量，所以每條判據都**抓兩次做差**。正文用一個自帶自證的 `python3` 解析器讀（它先證「命中得了、也落空得了」）。格式（每個族的 HELP/TYPE，族的清單從 `FAMILIES` 派生 · `Content-Type` 逐字 · 標籤鍵集合**逐字相等**，多一個 `uri` 就紅 · ★ 上游兩族**族在、樣本無**，分得開「沒接上」與「沒資料」）· 訪問控制兩個方向（站台 C 把 `metrics` 圈在 `10.0.0.0/8` 裡 ⇒ 我們夠不著，拿到備援 403）· ★★★ **三條反向**：① 沒寫 `metrics` 的站台上同一路徑**不是**指標；② 50 個未知 `Host` 之後 series 條數不增長**且**那一格正好 +50（⚠ **兩句都要**：實測把那句 `inc` 刪掉之後，前半句是**綠的**）；③ 一致性門 —— 指標在站台 A 上的增量 = 訪問紀錄新增行數 + 1（那個 +1 是左端那次抓取自己）· ★★★ G121：同一條請求上指標的 `site="a2.example"` 與紀錄的 `site="http://a.example:9920"` 給出**不同的值**，且 `site` 標籤的取值是一個**閉集** = 四條位址字面值 + `<none>` · ★★★ **TLS 族**（G122 / G127）：明文請求一筆都不記 · h1 與 h2 **分開打分開斷**（合成一條的話，一個只在 h2 上記的實作照樣全綠）· 漲的那一條的 `{version,cipher}` 與**訪問紀錄剛報出來的那一對值**逐字相同（⛔ 不寫死套件名）· h3 落在 `cipher="<unknown>"` 而**同一條請求的紀錄行裡 `tls_cipher` 不出現** · **這個族裡一個空標籤值都不許有** —— ⚠ 實測：改成記空串之後「TLS 族正好 +1」**仍然是綠的**，逮住它的是空標籤那條 · ★★★ **`status_class="none"`**（G124）：兩邊一起斷（紀錄多一行 `status=0` + 指標那一格正好 +1，且兩邊落在同一個 `site` 上），⚠ 夾具是「不帶 `Content-Length` 的慢上游 + 發完立刻 RST」——**帶 `Content-Length` 的話 `status` 是 200**，因為回應頭 `write_all` 只進了使用者態緩衝、根本沒有 syscall |
| ★ ★ ★ **換代時的 QUIC 跨處理程序轉交** | [`tests/quic-relay/run.sh`](../../tests/quic-relay/run.sh) | **M2 批 K**（G109 ②③④⑤）。★ 它驗的是一句**產品承諾**：「換代時長連接不斷」——TCP 那側 M0 早就驗過，而 h3 在批 K 之前**不成立**。⚠ 它是唯一一格會在一次跑裡**起兩代產品處理程序**的場景（`SIGQUIT` + `-u`）。兩代給**不同的回應體** ⇒「這一條是誰服務的」由**回應本身**說，不用去紀錄考古。★ ★ 三條自證缺一不可：空 UDP 連接埠上 `--http3-only` 必須失敗 · **不換代**時同一條連接能連打 3 個請求（否則「換代後失敗了」與「這條連線路徑本來就不行」分不開）· `num_connects` 之和 = 1（否則一個每次重連的 curl 會讓整格全綠，而它證明的是別的事）|
| ★ **ACME 端到端（簽發）** | [`tests/acme/run.sh`](../../tests/acme/run.sh) | 門禁裡跑**真的 CA**（pebble，G64），一次簽兩張：HTTP-01 一張 + DNS-01 通配符一張（33 條）|
| ★ ★ **ACME 端到端（續期）** | [`tests/acme/renew.sh`](../../tests/acme/renew.sh) | ★ G58 的**另一半**：證書壽命壓到 ~1.5 天，**跑著的**樞衡自己的巡檢循環按 ARI 重簽一張（21 條）。判據不是「出現了新證書」，見 [TLS 與自動 HTTPS](/architecture/tls.md) |
| ★ ★ **冒煙（自證）** | [`tests/smoke/self-check.sh`](../../tests/smoke/self-check.sh) | ★ 它跑的是 [`tests/smoke/run.sh`](../../tests/smoke/run.sh)——**那個指令碼的正式用法是對著真網域跑**，門禁裡拿一個健康執行個體 + 一個死連接埠 + 一張自簽證書，證明它對好目標給綠、對壞目標給紅 |
| ★ ★ **壓力** | [`tests/stress/run.sh`](../../tests/stress/run.sh) | 持續負載下**零錯誤、fd 不漲、記憶體不漲**，且負載中途走一次全量 load 仍然零失敗。⚠ **不產出效能宣告**，效能口徑見 [G19](/verification/performance-bar.md)（M3）|
| ★ ★ **對拍流水線與判據**（M3 第一刀，G132）| [`tests/bench/run.sh`](../../tests/bench/run.sh) | ⛔ **它不判效能，也不產出任何效能數字** —— 判的是那條流水線還活著、判據還判得動。★ **2026-09-06 那句承諾兌現了**：合格宿主到位，第一組真實讀數在 [`bench/results/`](../../bench/results/2026-09-06-static-throughput/README.md)（結論是 `FAIL`）。⛔ 本格自身仍然出不了數字 —— 它跑在這臺開發機上，而這臺機器仍被判成不合格。它跑在**第三張鏡像**裡（樞衡 + Caddy + HAProxy + nginx + oha 同處一室，全部按 digest / sha256 釘死）。三組：① 釘死的版本與 [`bench/README.md`](../../bench/README.md) 那張表逐條對上，且兩張鏡像裡的 `oha` 是同一個（★ §8 要的「同一個負載生成器」）；② 四家真的起得來、真的回那個資源、原始資料真的寫入磁碟，**而這臺開發機被判成不合格宿主 ⇒ 判定拒絕出結論**；③ ★ ★ ★ **反證七條**（承重）—— 喂合成的**合格**快照，判定必須真的打出 PASS 與 FAIL。⚠ 沒有 ③，② 的判別力是零：**一個永遠拒絕出結論的判定器，與一個壞掉的判定器給出完全相同的輸出** |
| **M0 接縫驗證** | [`tests/m0/run.sh`](../../tests/m0/run.sh) | 優雅升級中三類流量零中斷 |
| ★ **未被認領的繼承 fd** | [`tests/m0/unclaimed.sh`](../../tests/m0/unclaimed.sh) | ★ 2026-09-24 起驗的是**修復**（pingora 0.9.0 + 每個自建服務都宣告 `listen_addresses()`），見下 |
| ★ ★ **M1 產品二進位** | [`tests/m1/product.sh`](../../tests/m1/product.sh) | ★ **它跑的是 `fulcrum serve`，不是 spike**（G78）：`Type=notify` 就緒、pid 檔案、三次 `systemctl reload`（改設定 / 回復 / **換二進位**）、停機走完排空 |
| **M1 systemd 主場景** | [`tests/m1/run.sh`](../../tests/m1/run.sh) | 兩次 `systemctl reload` 零停機換代 + 停機仍走完排空。★ 跑的是 **spike**，驗的是機制那一層（fd 移交、`CLOEXEC`）|
| ★ **M1 `ExitType=main`** | [`tests/m1/exit-type-main.sh`](../../tests/m1/exit-type-main.sh) | ★ 重現壞行為：去掉 `ExitType=cgroup` 會怎樣 |
| ★ **M1 MainPID 交接** | [`tests/m1/mainpid-handover.sh`](../../tests/m1/mainpid-handover.sh) | ★ 重現壞行為：釘住 G31 推斷的那條被否掉的路 |

## CI

[`.github/workflows/gate.yml`](../../.github/workflows/gate.yml)，觸發是 **push 到 `main`** + 手動。

★ ★ ★ **它裡面只有一條命令，而且與上面那條逐字相同**：`bash tests/m0/docker-run.sh`。
這不是省事，是判據紀律 —— CI 若自己拼一套「等價的」步驟，就會長出**第二套判據**，
而兩套判據遲早分家，那時「CI 綠」與「門禁綠」是兩件事，卻沒有任何東西會說出來。

| 決定 | 理由 |
|---|---|
| `runs-on: ubuntu-24.04`（不是 `ubuntu-latest`）| M1 場景相依宿主機是 **cgroup v2**；`latest` 換代時不會有任何一行輸出說環境變了 |
| **沒有 `paths-ignore`** | ⚠ 「只改文件就跳過」是錯的：`doc_contract.rs` 用 `include_str!` 讀 `dsl-reference.md`，另一道門還**反向**斷言已串接的能力不許出現在裡面 ⇒ **純文件改動真的能把門禁弄紅** |
| `concurrency` + `cancel-in-progress` | 連推幾次時只有最後一次跑到底 |
| `timeout-minutes: 120` | ⚠ **止血閥**，不是預期耗時 |
| 先「騰出磁碟」一步 | runner 上 `/` 只有二十幾 GB，而這裡要放兩個鏡像 + cargo registry + release 模式的整個 `target`。⚠ 磁碟滿的回報錯誤形態五花八門，**沒有一條會說「磁碟滿了」** |

⚠ **CI 是備援，不是第一眼**：本機先跑一遍仍然是規矩。

## 建置快取

搬運在 [`tests/ci/cache.sh`](../../tests/ci/cache.sh)：`save` / `restore` 兩個子命令，
把兩個 docker 命名卷**流式**打成一個歸檔（tar 在容器裡跑、壓縮在宿主機上跑，
不落幾個 GB 的中間檔案 —— runner 的磁碟裝不下）。量級：`fulcrum-cargo` 約 700 MB、
`fulcrum-target-*` 約 6 GB、歸檔約 2 GB（裝得進 GitHub 每倉 10 GB 的快取預算）。

★ ★ ★ **快取最壞的失效方式不是「沒命中」，是「命中了一份不該用的」。** 五道防線：

| # | 防線 |
|---|---|
| 1 | 快取鍵裡帶 `Dockerfile.build` 的雜湊 —— **工具鏈身分**（連 C 工具鏈一起，cargo 的 fingerprint 覆蓋不到那半邊）|
| 2 | 歸檔裡另存 `meta.txt`，**灌回前逐字比對**那個雜湊；對不上就跳過並說明，**絕不硬灌** |
| 3 | 同一份 `meta.txt` 裡的 `target-volume=` 也逐字比對 —— 它把「哪個鏡像 × 哪一棵工作樹」兩個座標都編在裡面，少了這一條，**另一棵樹產出的歸檔照樣灌得進這棵樹的卷** |
| 4 | 解壓器按**歸檔自己的魔數**選，不按「這臺機器上裝了什麼」選 —— 判據掛產物不掛環境 |
| 5 | 灌完**當場自證**卷裡真的有東西；空了就喊（不判紅，但絕不讓人以為它生效了）|

⚠ 第 3 條不能用「反正容器裡都是 `/w`，內容通用」跳過：**路徑通用，`mtime` 不通用**。另一棵樹編出來的產物比這棵樹的原始碼新時，cargo 會直接當成最新的重複使用——就是下面那兩次實測的形狀。★ 而它落在最看不見的地方：CI 上沒人盯著卷名，只會看到一輪很快的綠。

⚠ **懷疑快取的時候**：把 workflow 裡快取鍵的 `v1` 改成 `v2`，整片快取立即作廢。

### ⚠ 三件說在明處

1. **命中快取能省下的是門禁那一步的編譯時間**，一輪從二十幾分鍾降到十幾分鍾。
   ⚠ 每次實測都會給出一個區間而不是一個數（兩次實測差 27%，而**每一步都同比變慢**）——
   那是 runner 快慢的差別。★ 拿其中一次當「那個數字」會把抖動說成確定性。
2. **命中即跳過匯出**：曾經有一輪命中之後仍然花了六分鐘壓出一份歸檔，
   而下一步因為命中被跳過 ⇒ 那六分鐘原樣扔掉。
   > ★ **一步只在「它的產物會被用到」時才該跑**，而判據要往前挪到那一步自己身上。
3. **命中時快取不刷新**（匯出整步跳過）。於是隨提交累積，快取裡那份 target 相對 HEAD
   越來越舊，cargo 每輪補的差量慢慢變大。哪天兩把鎖之一變了，鍵跟著變、自然重做一份新的。

## ⚠ 關於 fork 迴歸網的兩件事

<a id="一已查實它依賴容器能上公用網路"></a>

### 一、已查實：它**相依容器能上公用網路**

`pingora-core` 的 `connectors::*` 裡有一批測試**連的是真的 `1.1.1.1:443` / `:80`**
（`one.one.one.one`，見 `pingora-core/src/connectors/http/{mod,v1,v2}.rs`）。
這道門一直悄悄掛著一個外部前提，而 [§8 的效能對拍](/verification/performance-bar.md)
要求環境可重現。真要處置，方向是把這批測試指向**容器內的假上游**。

### 二、✅ 已查實並修掉：那條偶發是**短讀**，不是環境

上游 `test_connect_uds` 自己寫錯了：

```rust
let mut buf = [0; 9];
let _ = stream.read(&mut buf).await.unwrap();   // ← 回傳值就是「讀到了多少」
assert_eq!(&buf, b"it works!");
```

`read()` 讀到 1 個位元組也算成功，而流式 socket 允許把 `write_all(b"it works!")` **分段送達** ——
此時 `buf` 裡是半條訊息加一串 0。**回傳值被 `let _ =` 丟掉，於是「唯讀到幾個位元組」
沒有任何地方會說出來。** ✅ fork 裡已改成 `read_exact`（見 `vendor/pingora/FORK.md` §7）。

#### ★ ★ ★ 判據：擾動，不是重跑

「又跑了幾次沒再見到」對間歇性缺陷**等於沒有證據**。做法是**把它變成確定性的**：
讓 mock server 按「1 位元組 + 20ms + 剩下 8 位元組」送達（**流式 socket 的合法行為，不是破壞**），
未修的版本每次都失敗，改成 `read_exact` 之後有無擾動都通過。

> ★ ★ **間歇性缺陷的出路是「把它變成確定性的」，不是「多跑幾次看看」。**
> ★ 而在查實之前，**先寫下「我不知道」比寫下一個講得通但沒驗過的原因更有用** ——
> 後者會讓下一個人跳過實驗。

## ★ ★ 「樞衡自己的 crate 測試」堵的是一個結構性缺口

在它出現之前，**沒有任何一個場景會跑本儲存庫自己寫的 Rust 測試**：

- fork 迴歸網跑的是 `vendor/pingora` **自帶**的測試；
- M0 / 未認領 fd / M1 三個都是 shell 寫的端到端；
- lint 只看得見「編不過 / 有 warning」。

也就是說，M1 產品程式碼裡的每一條 `#[test]` 都會**一次都不跑，而整條鏈照樣報綠**。
★ 這與本儲存庫反覆抓到的「判據覆蓋面小於它自稱回答的範圍」是同一個形狀。

★ **判據裡最要緊的是測試條數的下界**（`MIN_TESTS`）。理由是迴歸網上吃過的那一次：
缺 `--no-fail-fast` 讓 `cargo test` 每次都停在第一個測試二進位，六個 crate 的單測從沒跑過
而門一直是綠的 —— **「新加的測試沒讓計數變化」是這類缺陷唯一會露頭的地方**。

三條反證都做過：有測試失敗 → 紅；`MIN_TESTS=999` → 紅；還原 → 綠。

## ★ ★ M1 的場景跑在**另一個容器**裡

它們要驗的東西只存在於 systemd 裡（MainPID、cgroup 生命週期、`KillMode`），所以用
[`docker/Dockerfile.systemd`](../../docker/Dockerfile.systemd)（Debian 13 + **systemd 257**，
釘到 digest）另起一個 `--privileged --cgroupns=private`、以 systemd 為 PID 1 的容器。
`docker-run.sh` 在最後呼叫 [`tests/m1/systemd-run.sh`](../../tests/m1/systemd-run.sh) 驅動它們——
**一個「要另外記得跑」的場景，與不存在的場景沒有區別**。

⚠ ★ ★ **千萬不要再把宿主機的 `/sys/fs/cgroup` 掛進那個容器**（網上很多做法這麼寫）。
容器用的是私有 cgroup namespace，再掛一次宿主機的樹兩邊就對不上：journald 起不來，
**而且 `MAINPID=` 通知會被靜默丟棄**—— 差點因此寫下一個完全錯誤的結論。
cgroup v2 下 `--privileged` 自己就會掛成 rw。詳見
[M1 spike #1](/verification/m1-systemd.md) §6。

★ **測試宿主鏡像也按內容雜湊重建**（與建置鏡像同一條紀律）：systemd 的版本是本 spike
結論的一部分，浮動 tag 會讓結論某天悄悄失效。⚠ 它**何時拔釘子尚未定案**——
G36 的 rustc 口徑只管建置鏡像（`G152` 把它的 M3 凍結擴到了對拍鏡像），不管本鏡像，見 [待定清單](/governance/open-questions.md)。

★ ★ **重現類場景的綠意味著「壞行為已重現」，不是「功能正常」**（M1 的 `exit-type-main.sh` 等）。
`unclaimed.sh` 原先也是這一類；2026-09-24 rebase 到 pingora 0.9.0（含上游 `listen_addresses()`）之後，
它按預告**反寫**成驗修復：第二代關掉沒人認領的 fd、老一代退出後連接埠拒連、不再傳給第三代。
⚠ 它現在守的最要緊的一條是「任一服務回傳 `None` ⇒ 整個處理程序的清理靜默關閉」（反證實測過）。
背景見 [尚未驗證的接縫](/verification/open-seams.md)。

★ M0 那幾個場景共用 8080–8082 連接埠，**每一個都必須不留殘餘處理程序**（`tests/m0/run.sh` 末尾
等最後一代真正退出正是為此）。⚠ 實際踩過：處理程序沒走乾淨，下一個場景綁不上連接埠，
而它的基線探測**照樣變綠** —— 對著殘留處理程序。

## ★ ★ 產品的 musl 靜態產物也跑在**容器之外**（G108）

[`tests/musl/product.sh`](../../tests/musl/product.sh) 自己 `docker build` 一個 **Alpine** 鏡像
把**產品本體**編成 musl 靜態產物，再塞進 `FROM scratch` 裡跑一次 `fulcrum validate`。
⇒ 它與那次 `docker run` 是兩回事，與 M1 那四格同理，由 `docker-run.sh` 在最後驅動。

> ★ **這一格的來歷**：D22 登記的本是「把 `tests/musl/probe.sh` 掛成常設的門」，
> 而 owner 換掉了**判據本身**（**G108**）—— 探針編的是 spike，答不了「產物是不是單靜態二進位」。
> ⇒ D22 **已結案**，探針留在門外當歷史記錄。
> ⚠ **這一格的預設是 x86_64**（`ARCHES="amd64"`），而 G13 承諾兩個架構 —— 另外那半邊
> 曾掛號 **D24**，✅ **已由 `G144` 結案 = 候選 ②**：aarch64 只在 `Cargo.lock` 或那三張釘
> rustc 的 Dockerfile 變化時才跑，其餘時候讀 [`tests/musl/arm64-verified.txt`](../../tests/musl/arm64-verified.txt)
> 那份**驗證記錄**（判據與觸發集在 [`tests/musl/arm64-trigger.sh`](../../tests/musl/arm64-trigger.sh)）。
> ⛔ 別把「這一格是綠的」讀成「G13 的散佈口徑全都有門守著」：它**不看**產品功能對不對
> （那是容器裡那些場景的事，而它們跑的是 glibc 產物）。
> ⚠ ⚠ **binfmt 是機器本機狀態**：沒裝的機器上，一旦觸發集變了這一格會**判紅而不是靜默跳過**
> （「沒能檢查」不算「檢查通過」），訊息裡給了兩條出路。

⚠ ⚠ **它的上下文是儲存庫根**（根 `Cargo.toml` 的 `[patch.crates-io]` 指著 `vendor/pingora`），
所以儲存庫根有一份 [`.dockerignore`](../../.dockerignore) —— ★ 而 `.dockerignore` 是**按上下文根讀的**，
`Dockerfile.build`（上下文 `docker/`）與 `Dockerfile.musl-probe`（上下文 `spikes/musl-boringssl/`）
**都讀不到它**。「加一條 `.dockerignore`」聽起來是全域動作，實際上不是。

★ **兩條 BuildKit 快取掛載是它掛得起門禁的前提**：`COPY . .` 意味著儲存庫裡任何一個檔案變了
這一層就作廢，而這是一道每次提交都要跑的門。實測 **6m49s（冷）→ 50s（改了 Rust 原始碼）
→ 12s（只改別的檔案）**。
⚠ ⚠ **CI 上沒有這份快取** —— `fulcrum-cache` 覆蓋的是 docker 命名卷，覆蓋不到 buildkit 的
cache mount ⇒ **CI 每次都付冷價**。這一條寫在明處。

⚠ **`id` 裡必須帶 `$CRT_STATIC`**：反證那一趟的 RUSTFLAGS 與正向不同，
共用一份 target 只會讓兩邊互相把對方的產物擠掉 —— cargo 不會算錯，只會一直重編。

## ★ 收尾（重做）

兩個指令碼現在用**同一套規則**，寫在各自的 `cleanup` 裡，並掛在 `EXIT` trap 上（**失敗路徑也要收**，否則 `fail` 一 exit，兩代原樣留下）：

> **最後起的那一代是活的，用 `SIGINT`；更早的都在排空，直接 `SIGKILL`。**

這條規則來自一次**實測**，不是推演：

| 那一代的狀態 | 發 `SIGINT` |
|---|---|
| 還在 `main_loop` 裡等信號 | **0 秒退出**（`ShutdownType::Quick`，兩個超時都是 0）|
| 已收過 `SIGQUIT`、正在排空 | ★ **被吞掉**，40 秒後照樣活著 |

後一條的原因：處理程序已離開 `main_loop`、沒人再從信號管道里讀，而 tokio 的處理器仍然裝著、頂掉了 `SIGINT` 預設的 terminate 行為。**排空中的那一代收不到任何可捕獲的信號**——它只能自己走完 `CLOSE_TIMEOUT`(5) + `grace_period_seconds`(30) + `graceful_shutdown_timeout_seconds`(≤30)，最快 35 秒、最慢 65 秒。所以對更早那些用 `SIGKILL` 是**有意的選擇，不是等待失敗後的備援**：判據此刻已全部做完，它們的 fd 也早已交接出去，只剩「把連接埠空出來」。

★ 這條同樣適用於 M1 的 systemd 設計：**一旦一代開始排空，就叫不動了**，`TimeoutStopSec` 必須按 35–65 秒這個量級來配。
⚠ 35–65 秒是按原始碼公式推的，**在 pingora 0.8.1 下它其實偏小**：等完各 runtime 之後還有一次重複的
`sleep(graceful_shutdown_timeout_seconds)`，真實區間是 65–95 秒。2026-09-24 rebase 到 0.9.0（上游 `6f15714` 刪了那次 sleep）
之後公式才與實際相符 —— 最快 35（runtime 一收到關停就退）、最慢 65（拖滿 graceful）。
M1 實測佐證：同一個 grace 5 + graceful 5 的場景，`systemctl stop` 在 0.8.1 下 10 秒、0.9.0 下 5 秒。

### 三條從收尾這件事帶走的

- **閾值和被檢查的物件不能是同一個數** —— 收尾原先「發 `SIGTERM` 等 30 秒」，而設定裡的
  `graceful_shutdown_timeout_seconds` 也是 30，於是它每次都落進 `SIGKILL` 分支、
  在每次全綠的執行裡打一行 ⚠。★ **每次都亮的告警等於沒有告警。**
- **調數字之前先讀它在等什麼**：把上限從 30 提到 45 照樣每次都紅 —— 那 30 秒是無條件
  `thread::sleep`，根本不是「等連接排空」。
- ★ **一個只是碰巧成立的清理，和一個不存在的清理，區別只在運氣**：收尾改快之後，
  `unclaimed.sh` 的 `[0/6]` 當場報「開跑前 8081 上就已經有人在 LISTEN」——
  原來第一代一直是被那 30 秒必然超時的等待**順帶**收掉的。
  ⇒ 兩個指令碼現在都逐代登記 pid、逐代收。

★ **宿主機除 Docker 外什麼都不需要** —— 不用裝 Rust 工具鏈。

## ★ lint 門

- 排在最前跑：它最快，紅了指得最準（一行程式碼），不必等幾分鐘的端到端。
- 用 `--workspace` 而不是寫死包名：★ **寫死包名的作用域會在加新 crate 時悄悄漏掉它們**。
- `clippy` / `rustfmt` **裝在鏡像裡**（官方 rust 鏡像不帶它們），不在跑測試時臨時
  `rustup component add` —— 後者要聯網且版本會漂，而它們是判據本身。

★ ★ **`shellcheck` 那半邊掃哪些檔案是推匯出來的，不再是一張手寫清單**
（[`tests/ci/shellcheck-all.sh`](../../tests/ci/shellcheck-all.sh)）。

那張手寫清單栽過兩次，兩次都是**漏一項而沒有任何東西會說**：`tests/m1/lib.sh` 帶著一處
SC2045 躺進來；後來漏掉的是整個 `tests/quic-relay/`（18 項 vs 儲存庫裡 19 個含 `.sh` 的目錄），
底下壓著 4 條真實告警，**從來沒人看見過**。
⇒ 照 `docker-run.sh` 對 `*_ONLY` 的同一條路子改：**不列舉，問一句結構性的問題**
（「這幾棵樹下有哪些 `.sh`」），**並且每次執行都自證**。加目錄的人不需要記得改任何清單。

⚠ ★ ★ ★ **但 2026-09-05 它自己又栽了一次，而且是同一個形狀往上抬了一層。**
「推導」從來只在**它被給定的那幾棵根**之內成立 —— 當時那棵根只有 `tests`，
於是新建的頂層 `bench/` 帶著**五個 `.sh` 一次靜態檢查都沒被跑過**就進了儲存庫。
⇒ 這與它當初取代的那張手寫目錄清單是同一個缺陷，只是清單從「目錄」變成了「根」。
⇒ 處置**不是**把 `bench` 加進根就完事（那還是一張清單，下一個頂層目錄照樣漏），
而是照 §「自查的篩子必須比門禁的寬」那條辦：門禁掃 `$ROOTS`，
另有一道**更寬**的普查問「有沒有 `.sh` 落在根之外」——有就紅。
★ **於是「加了新目錄忘了改」從靜默漏掃變成當場紅。**

★ ★ 普查問的是 `git ls-files --cached --others --exclude-standard`，
**⛔ 不是 `find` 掃工作樹，而這是判據本身不是實作口味**：

- 光 `--cached` 只看**已跟蹤**的 ⇒ 一個剛寫出來、還沒 `git add` 的指令碼整個溜過去 ——
  **而那正是那五個 `bench/*.sh` 當時的狀態**，判據會在最該說話的那一刻沉默；
- `find` 掃工作樹則相反，它會把 `handoff/`（gitignored、按 G115 本機留存）裡的草稿也判紅，
  而它給出的修法（「把那個頂層目錄加進 ROOTS」）對 `handoff/` 恰恰是**錯的建議**。

⇒ 「已經在儲存庫裡、或正在進儲存庫的」才是要問的那個集合。⚠ 手動排除只剩 `vendor/` 一項
（上游的程式碼，不歸本儲存庫 lint 管），⛔ **不許再加**：每加一項就是一次消音。
⚠ 另有一條自證：**普查一個被跟蹤的 `.sh` 都沒數到時判紅** —— 一個恆回傳空的普查
對每一棵樹都說「沒問題」。

- 用 `find` 而不是 glob：`tests/**/*.sh` 在**非交互 bash 裡沒有 globstar**，`**` 退化成 `*`
  ⇒ 它逐字等價於 `tests/*/*.sh`，**兩層以下的指令碼一個都不匹配**，而寫著 `**` 的人以為覆蓋到了。
- 枚舉器每次拿一棵**答案已知**的假樹自證（兩層深的指令碼 · 帶空格的路徑 · 名字像指令碼的目錄 ·
  `notes.md` 與 `run.sh.bak`），**逐字比對** ⇒ 少認一個紅，多認一個也紅。
- **「一個都沒找到」是紅，不是「掃完了」** —— 所以那裡不許寫成 `find … | xargs -r shellcheck`：
  `-r` 的語義就是「沒有輸入就一條都不跑」，掃空時結束代碼 0，安靜地全綠。

> ★ 四條反證都實測過：① 新建一個**不在任何清單裡**的 `tests/<x>/`，塞一條 SC2045 ⇒ 當場紅；
> ② 把枚舉器限成一層深 ⇒ 自測報「實得少了 `two/deep/nested.sh`」；③ 把枚舉器弄成恆回傳空
> ⇒ 自測報「實得為空」；④（2026-09-05 加的普查，**兩個方向各一條**）在 `tools/` 下放一個
> **未跟蹤**的 `.sh` ⇒ 普查當場點名「有 1 個 .sh 落在被掃的樹之外」，刪掉後重跑回綠；
> 而在 `handoff/`（gitignored）下放一個 `.sh` ⇒ **不報**，因為它不屬於本儲存庫。
> **一條從未被證明會紅的掃描判據，與沒有這條判據是一回事** ——
> 而一條只證明過會紅、沒證明過會綠的判據，同樣分不清「它在判」與「它恆判紅」。

⚠ ⚠ **別把這道門當成能攔住那處子 shell 缺陷的東西 —— 實測它攔不住。**
shellcheck 0.10 的 SC2030/SC2031 只認**寫在子 shell 那幾行裡**的賦值（`( B=1 )`、`X=$(D=9; …)`
都報），而 `tests/quic-relay/run.sh` 當年那處是「函式體裡 `PIDS+=`、呼叫方 `$(f)`」——
逐字重現過，**一聲不吭**。攔住它的是場景收尾時那條「用過的連接埠要還回去」的自證，不是 lint。
> ★ 這條要寫在明處：**一把沒量過就被當成能量的尺子，比沒有尺子更貴。**

★ **lint 那一格還掛著兩條與 lint 無關的自檢**，都是因為它們守的程式碼
**在一趟綠的門禁裡從來不會被執行**：

| 自檢 | 它守的東西 | 為什麼掛在這裡 |
|---|---|---|
| `tests/ci/dump-cache.sh --self-check` | CI 那段搬運程式碼的結束代碼取法（G94） | 本體只在 `cache-hit != 'true'` 時才跑，往後基本不會再跑 |
| `tests/acme/self-check.sh` | ACME 兩個場景的**失敗現場取證**（`lib.sh` 的 `acme_dump_ports` 一族） | 取證只在「已經要紅」的路徑上執行 ⇒ 場景一綠就碰不到它 |

> ★ ★ 兩條是同一個形狀：**一段只在出事那天才第一次執行的程式碼，等於一段沒人驗過的程式碼**，
> 而出事那天發現它自己是壞的，就把那一次現場也一起賠進去了。
> 後者自己開一個監聽 socket 當靶子，要求取證**指名道姓**說出持有者是哪個 pid，
> 再關掉靶子要求它改口 —— 兩個方向都釘，只釘「看得見」的話一個恆說「看得見」的實作照樣全綠。

★ **vendor 的 `unexpected cfg condition value` 不會把這道門帶紅**（實測）：`--` 之後的
`-D warnings` 只作用於 clippy 直接 lint 的那個 crate，`vendor/pingora` 作為相依被普通 rustc
編譯，它的 warning 就只是 warning。

# ⛔ 不要在宿主機上裝 Rust（G107）

**理由是一次實測**：整套 C++ 工具鏈裝齊之後，`cargo check -p fulcrum-tls` **仍然編不過** ——
BoringSSL 編出來了，**編不過的是我們自己的程式碼**。六個產品 crate 裡有四個用 `std::os::unix`
（證書儲存的 `flock` 與 `0600`、L4 面的 fd 移交），`cargo check --workspace` 還會另外倒在
`sd-notify` 的 `F_SETFD` / `clock_gettime` 上。
⇒ 本機能覆蓋的從頭到尾只有 `fulcrum-config` 與 `fulcrum-runtime` 兩個 crate ——
**與什麼都不裝的時候是同樣的兩個。**

> ★ ★ 那段彎路只有一條教訓：**「卡住它的從來不是 X」** —— 不是 musl、不是 BoringSSL，
> 是建置宿主；不是連結器參數，是 PATH 上那個 `link.exe` 到底是誰；也不是 C++ 工具鏈，
> 是我們自己的程式碼。**每一步都真的修掉了一個攔路的東西，然後才發現它從來不是決定性的那個。**

⇒ 不要在宿主機上跑 cargo；不要往任何 PATH 里加 `CARGO_TARGET_DIR`、`LIBCLANG_PATH`
或工具鏈目錄；不要把任何本機輸出當判據、也不要當雷達。想看編譯錯誤就跑門禁。

# 為什麼全在容器裡（G26）

兩條理由，第二條是硬的：

1. **零主機汙染、完全可重現**，且**將來直接就是 CI 與對拍環境的底座**
2. ★ ★ **`SIGQUIT` + fd 移交是 Linux 獨有的**——這套東西**在 Windows 上根本沒法驗證**

未採納 WSL2 Ubuntu 26.04：★ 它的 `sudo` 要密碼，工具鏈安裝無法自動化；且主機會被裝東西、環境不可重現給別人。

# 建置鏡像

[`docker/Dockerfile.build`](../../docker/Dockerfile.build) → `fulcrum-build:local`，**不存在、或 Dockerfile 內容變了**，都由指令碼自動重建。

★ **基礎鏡像單獨是不夠的**：pingora 拉 `libz-ng-sys`，它要 `cmake` 編 zlib-ng。鏡像因此額外裝了：

| 包 | 為什麼 |
|---|---|
| `cmake` | ★ **必須** —— `libz-ng-sys` 用它編 zlib-ng |
| `clang` | 備著 —— 將來引入 `aws-lc-rs` 或任何走 bindgen 的 `*-sys` 包會用到 |

## ★ 基礎鏡像釘死到 digest

```
FROM rust:1.98.0-trixie@sha256:271849e998ffce5776454bbf98c5dc21baafc854ff8e566197908d3aca9a81e8
```

★ 換釘之前先 `docker manifest inspect` 驗過新 digest 真的解得開 ——
**別把一次全量冷編花在一個取不到的鏡像上**。
鏡像裡實際是什麼，每次跑驗證時由 `docker-run.sh` 列印（取自 `/etc/fulcrum-toolchain`）。

**為什麼必須釘**：`Cargo.lock` 入庫、vendor 的鎖也入庫、G26 明寫「完全可重現」、
[§8 的效能對拍](/verification/performance-bar.md)要求環境可重現 —— 而編譯器本身原先跟著
`rust:1-trixie` 這個浮動 tag 走。

★ ★ **這不是假設，是已經發生過的事**：Dockerfile 早就從 `rust:1-bookworm` 換成了
`rust:1-trixie`，而本機鏡像裡跑的**一直還是 bookworm**（指令碼只判「鏡像在不在」）。
此後每一次「全綠」，實際用的基礎鏡像都不是儲存庫宣告的那個，而且沒有任何輸出會說出來。
釘死並重建之後才看見真實漂移面：**cmake 3.25.1 → 3.31.6、clang 14.0.6 → 19.1.7**。

**修法有兩半，缺一不可**：

1. **這裡釘死**（tag + digest）
2. **`docker-run.sh` 按 Dockerfile 的內容雜湊決定重建** —— 雜湊寫進鏡像的 `cool.cnb.fulcrum.dockerfile-sha256` label，每次比對。這樣**任何**改動（digest、apt 裝什麼、加一層）都會觸發重建，不只是 `FROM` 那一行。★ 拿不到雜湊時**強制重建**，不是跳過檢查。

## 怎麼升（口徑 = G36）

G29 的精神是追新，所以這個釘子是**要定期拔的**，不是拿來爛在舊版本上的。**什麼時候拔由 `dep-check.py` 告訴你**，不用自己記：

```bash
python tools/dep-check.py     # 第三項就是建置鏡像檢查
```

★ 判據只有 **rustc 版本**。建置鏡像不是執行環境鏡像（G13 散佈的是 musl 靜態二進位，[首版範圍](/product/scope.md)不做官方容器鏡像），Debian 包一個都不進產物，所以 digest 漂移只打一行提示、**不判紅**——否則這道門會幾乎常年亮著。

結束代碼 **40** 表示要人管，含三種情況：rustc 有新版 / `@sha256` 釘子掉了 / ★ **本次沒能查證**（「沒能檢查」不算「檢查通過」）。

★ **它只報告，拒絕自動應用**（`--apply` 也不動），與 fork rebase 同樣處理——換編譯器必須重跑三場景，不該無人值守。報紅之後手動做：

```bash
docker pull rust:1-trixie
docker image inspect rust:1-trixie --format '{{range .RepoDigests}}{{.}}{{end}}'
# 把 Dockerfile.build 的 tag 與 digest 一起換成新值（tag 寫精確小版本，別寫 1-trixie）
bash tests/m0/docker-run.sh          # 鏡像會因內容雜湊變化自動重建，三場景必須全綠
# 然後把新的 rustc 版本更新到本頁
```

⚠ ★ **同一個 rustc 版本釘在三張 Dockerfile 裡，必須一起換**：
[`Dockerfile.build`](../../docker/Dockerfile.build) ·
[`Dockerfile.musl-probe`](../../docker/Dockerfile.musl-probe) ·
[`Dockerfile.musl-product`](../../docker/Dockerfile.musl-product)。
musl 那兩份的結論裡寫著「rustc 與 `Dockerfile.build` 是同一個版本」——
只換一邊那句話就變成假的，**而沒有任何東西會說出來**（`dep-check.py` 只看 `Dockerfile.build`）。
> ⚠ 這一行曾經寫著「兩個地方」，而那時已經是三個 ——
> **一句用數字說事的話，會在數字變了的那一刻悄悄變假。**
實測（x86_64）：產品編成 musl 靜態產物 **16,191,800 位元組**、`INTERP=0 / NEEDED=0`、
在 `FROM scratch` 裡 `fulcrum validate` 跑通；冷編 **6m49s**、改 Rust 原始碼後 **50s**、
只改別的檔案 **12s**（靠 BuildKit 快取掛載）。⚠ **CI 上沒有那份快取，每次都是冷的。**

⚠ ★ **M3 對拍期間凍結。** 開始採集資料時記下鏡像 digest，整個對拍期不動——否則[§8](/verification/performance-bar.md) 的原始資料在自己內部就不可比。

★ `apt` 的 `cmake` / `clang` **沒有**釘版本——真釘就得連 `snapshot.debian.org` 一起釘，代價與收益不成比例。折中是把實際裝到的版本落進 `/etc/fulcrum-toolchain` 並每次列印，**讓漂移可見**。查不出來的漂移才是問題。

# 快取與產物

| 東西 | 放哪 | 為什麼 |
|---|---|---|
| `cargo` registry | 命名卷 `fulcrum-cargo` | ★ 避開 Windows 檔案系統的慢 I/O，也不汙染宿主機。**與工具鏈無關，可跨鏡像共享** |
| `target` | 命名卷 `fulcrum-target-<Dockerfile 哈希前 12 位>-<工作树路径哈希前 12 位>` | ★ **必須同時跟著鏡像和工作樹走**，見下 |
| 執行期產物（pid / sock / 紀錄 / 探針輸出）| `run/m0/` | **已 gitignore** |

★ **首次建置慢（要拉基礎鏡像 + 編譯全部相依），之後增量建置約 25 秒。**

## ★ ★ 為什麼 `target` 卷的名字裡帶 Dockerfile 雜湊與工作樹標籤

cargo 的 fingerprint 覆蓋 rustc 版本與 flags，**但不覆蓋 C 工具鏈**——build script 產出的東西（`libz-ng-sys` 用 cmake 編的 zlib-ng、ring 的 `.o`）只在自身宣告的輸入變化時才重建。於是「**換基礎鏡像但 rustc 不變**」（正是  bookworm→trixie 那一次的形狀）會沿用舊鏡像編出來的 C 目標檔案。

★ 那正是剛釘死的可重現性**在低一層漏掉**：鏡像釘住了，但「用這個鏡像產出的東西」沒釘住。那次是靠**手動** `docker volume rm` 才敢說「全綠歸屬於釘死後的鏡像」——**相依人記得做的事，等於沒做**。

現在卷名帶上 Dockerfile 的內容雜湊：換鏡像自動換卷，舊卷留著還能退回，指令碼會把殘留的舊卷列出來提示刪除。

## ★ ★ ★ 以及為什麼還要帶工作樹標籤

上面那條理由一個字都沒變，只是它當年成立的前提是「一臺機器上只有一棵原始碼樹」——而那個前提已經不成立。本機現在同時有主樹和 `.claude/worktrees/` 下的若干棵：容器把各自的 `/w` 掛成**不同的原始碼樹**，卻曾經把**同一個** `/w/target` 掛給了所有人。

⚠ ⚠ 後果不是「編得慢」，是**門給出別人家的讀數，而兩邊都不紅**。實測撞見兩次：一次編譯錯誤指向一個只存在於另一棵樹的符號；另一次報「696 條全綠、結束代碼 0」，跑的卻是**沒有本次新判據**的舊測試二進位。後一種是致命的那一種——它不回報錯誤，它給一個像樣的、綠的、錯的答案。

⇒ 卷名再拼上工作樹路徑的短雜湊，兩條性質同時成立。**推導只有一份**，在 [`tests/lib/vol-lock.sh`](../../tests/lib/vol-lock.sh) 裡，由 `tests/m0/docker-run.sh` · `tests/m1/systemd-run.sh` · `tests/ci/cache.sh` 三處共用——此前那三處各寫了一遍同一個表達式，而各寫各的失效形態是安靜地指向兩個不同的卷（M1 掛上一個自動新建的空卷、CI 快取永遠不命中）。

## ★ ★ 樹標籤的第二筆賬：誰來回收沒有主人的卷

按樹分開是拿磁碟換量具：**每棵工作樹各佔一個約 6 GB 的卷**。而工作樹是會消失的——`.claude/worktrees/` 下那些用完就刪——**它們的卷不會跟著走**。

⚠ 卷名後綴是雜湊，**反推不迴路徑** ⇒ 光看 `docker volume ls` 說不出哪些已經沒有主人。而「別的樹的卷一律不給刪除命令」這條規矩（它防的是誤刪隔壁正在用的快取）恰好把這些也一起擋在了外面。⇒ 磁碟只漲不落，且沒有一行字指得出該刪哪個。

★ 修法是讓卷**自己記住主人**：新建時把歸一後的工作樹路徑寫進 label `cool.cnb.fulcrum.tree`（歸一用的是 `fulcrum_tree_norm`，與算卷名雜湊的是同一份，兩處各歸一各的話會把一棵活著的樹報成「已經不在」）。於是「不屬於本樹」再分成兩半：

| label 記著的樹 | 提示 |
|---|---|
| **已經不在了** | 給出 `docker volume rm` —— 刪了碰不到任何人 |
| **還在** | 照舊只報數、**不給命令**（刪掉別人正在用的那個，只會讓那棵樹莫名其妙地全量重編）|
| **讀不到 label** | 歸後一半。加這條規則之前留下的卷、以及手動建的卷（[supply-chain.md](supply-chain.md) 裡那條 `supply-audit` 命令就手動掛著一個）都讀不出主人，而**「沒能檢查」不算「檢查通過」** |

⚠ label **只有新建那一次寫得進去**：對已存在的卷 `docker volume create` 是**靜默的 no-op**，不回報錯誤也不更新 label（實測）。~~這不構成問題——樹變了卷名就變，名字與 label 永遠是同一次新建寫下的。~~ ★ 新建的那一次**當場自證**讀得回來且指向本樹；讀不回來就喊一句「這條回收提示這一輪等於沒有」，**不判紅**——它是提示，不是量具本身。

### ⚠ ⚠ ⚠ 2026-09-04：上面那句劃掉的話被實測推翻了

⛔ **有意不刪原話**（與 §10 裡 G122 那半句同一手法）：它當時是真心話，留著才看得出錯在哪。

它成立的前提是「**卷都是這條規則建的**」。而實測本機 **11 個 `fulcrum-target-*` 卷、10 個無主、約 92 GB，回收提示看得見 1 個**。三個成因彼此無關：

| 看不見的成因 | 個數 | 體積 |
|---|---|---|
| 舊命名代（無雜湊 / 只有一段雜湊，加樹標籤之前建的）| 8 | 83.8 GB |
| label 鍵還沒統一那一代（寫成了 `cool.cnb.fulcrum.checkout`）| 1 | 4.1 GB |

⇒ **9 個看不見，共 87.9 GB**；第 10 個（label 鍵正確、樹確實沒了，4.1 GB）是唯一被點出來的那個。

★ 順帶一條同源的：**本樹自己那個卷（9.6 GB）也沒有 label** —— 它比 label 機制早生 17 分鐘。今天它靠「後綴是本樹標籤」那一步被排除在清單外，沒出事；但它說明這條機制對**任何先於它存在的卷**都是瞎的，而那正是劃掉那句話的理由。

★★★ 那兩個 4.085 GB 的卷是**同一棵工作樹拿了兩份** —— `sha256("/d/WorkSpace/…")` 與 `sha256("D:/WorkSpace/…")` 是兩個標籤（兩個雜湊都逐字算過對上）。⇒ [`vol-lock.sh`](../../tests/lib/vol-lock.sh) 頂上警告的那種「兩處各歸一各」**真的發生過**，8.17 GB 就是它留在盤上的物證。

⇒ 缺的不是「多讀一個 label 鍵」（那張歷史鍵清單自己就是一份會過期的抄件），是一條**不相依建卷那一刻記下了什麼**的判據：

**規則：一個今天的 `fulcrum_target_vol` 造不出來的卷名，沒有任何現行呼叫點夠得著它。** ⛔ 這不是「看起來很舊」那種年齡啟發 —— 年齡是啟發，「現行程式碼造不出這個名字」是關於**今天這份程式碼**的陳述，讀一份推導就能證（三處呼叫點全走 `fulcrum_target_vol`）。

⚠ **殘餘風險寫在明處**：若有人在另一棵工作樹上 checkout 著舊提交跑舊指令碼，它的卷是舊代形狀 ⇒ 會被列進清單。後果是**那棵樹下次全量重編**，不是資料丟失；而 `docker volume rm` 對佔用中的卷本來就會拒絕。

⚠ ⚠ **這條新規則絕不許吞掉舊規則保守的那一半**：名字是**當代形狀**、但讀不到 label 的，照舊只報數。放寬一次的症狀不是回報錯誤，是有一天它給出一條刪錯卷的命令。

★ **第三件：把體積說出來，不只報個數。**「另有 10 個卷」讀起來無關痛癢，「另有 10 個卷，17.6GB / 16.7GB / 11.1GB …」不是 —— 而 92 GB 那次，缺的正是這一行。⛔ **有意不求和**：`docker system df` 給的是已經格式化過的字串（`9.628GB`），在 bash 裡解析單位再相加是一臺會給出「像樣的錯答案」的一次性量具。⚠ 它比 `docker volume ls` 貴得多（實測 **1.4–1.9s vs 0.34s**）⇒ 只在真有東西要報時問一次，兩個桶都空就一次都不問；量不到時打「未量到」，⛔ 絕不打 0。

★ 形狀判據每次跑還**當場自證**一次：拿本樹自己那個真名字去問它。`fulcrum_tree_tag` 在既沒有 `sha256sum` 也沒有 `git` 的宿主上會退到備援標籤，那時當代的名字會被判成「舊代」—— ⛔ 那正是它最容易刪錯的一輪。⇒ 自證不過就把整條規則**關掉**並說出來（不判紅，與上面 label 那條同一紀律）。

★ **六個方向各一條自測**（`selftest_tree_state` 與 `selftest_vol_verdict`，與其它 `selftest_*` 同處）：樹還在 ⇒ `live`、樹沒了 ⇒ `gone`、讀不到 ⇒ `unknown`；label 說還在 ⇒ 一律留（★★ 它必須**短路在最前** —— 否則「舊代形狀 + 樹還活著」會掉進新規則，那就是勸人刪隔壁正在編譯的快取）· 當代形狀 + 無 label ⇒ 仍然留 · 形狀判據宣告不可信 ⇒ 新規則整條不生效。

⚠ 第六條是**推導自證**：`selftest_vol_verdict` 拿 `fulcrum_target_vol` **真的生成一個名字**來比，⛔ 不是手寫一個範例名 —— 手寫的那份釘不住形狀，改了推導它照樣綠。反證做過：把推導改成三段名 ⇒ 自測當場紅。

### ⚠ ⚠ ⚠ 2026-09-05：上面這一整套判據，**只在掃描集內說話**

前面兩個桶（無主 / 判不出）都建立在同一個前提上：**要判的那個卷在掃描集裡**。而掃描集是

```bash
docker volume ls -q --filter name=fulcrum-target      # 子串匹配
```

⇒ 名字裡沒有 `fulcrum-target` 這個**子串**的卷，上面每一條判據、每一條自測都夠不著它。兩天裡逃出去的有三族，⚠ **三族全部是人工發現的，門禁一次都沒幫上忙**：

| 逃逸的卷 | 含 `fulcrum-target`？ | 含 `fulcrum`？ | 怎麼被發現的 |
|---|---|---|---|
| `fulcrum-upstream-target`（投稿六那棵上游克隆臨時建的）| ❌ | ✅ | 人工「儲存庫裡零引用」 |
| `fulcrum-musl-{probe,alpine}-{target,cargo}` ×4（musl 探針那場）| ❌ | ✅ | 人工 |
| `pingora-up994-target`（10.51 GB）| ❌ | ❌ | 人工，且**無 filter 列一遍**才看見 |

★ ★ ★ 共同成因**不是「篩子太窄」**：篩子按**名字**判，而要找的東西是「本儲存庫的活造出來的」—— **名字只是屬主的代理**。三族全是一次性實驗順手建的卷（上游克隆、musl 探針、投稿六 CI），它們本來就不走 `fulcrum_target_vol` 那條推導，⇒ **怎麼調名字模式都夠不著**。

⛔ 因此 2026-09-04 擺出來的那條候選（「放寬到 `name=fulcrum-`，再按後綴分流」）**被第三族證偽了** —— 它連 `fulcrum` 都不含。

⇒ 判據換個問法：**屬主判不了，能判的只有「報得多寬」。** 加第三個桶，[`vol-lock.sh`](../../tests/lib/vol-lock.sh) 的 `fulcrum_vol_attribution` / `fulcrum_unattributed_vols`：

| 這個桶 | 落法 |
|---|---|
| 掃描集 | **無 filter** 的 `docker volume ls -q`，全機 |
| 減掉 | 本門管轄內的（本樹活卷 + `fulcrum-cargo` + 掃描集裡的全部）· 說得出屬主的（`FULCRUM_FOREIGN_VOL_PREFIXES` 前綴白名單）· docker 自己建的匿名卷（**恰好 64 位十六進制**）|
| 剩下的 | 報名字與體積，⛔ **一律不給刪除命令** —— 本門對它們的屬主一無所知，給命令等於拿一句「我不知道這是誰的」去勸人刪，那比不報更壞 |

★ ★ 選它而不是繼續放寬 filter 的**全部理由**：它的失效方向是**噪音**（白名單過期 ⇒ 多報一行），⛔ 不是**沉默**（漏報一個卷）。

⚠ ⚠ 那份白名單**本身就是一個新篩子**，有變成消音手段的風險。三條紀律寫在清單頭上：① 只放**別的專案**的卷、逐條註明屬主；② 前綴**錨在開頭**，⛔ 不是子串——子串語義正是本輪要修的那個缺陷；③ 自測釘住它不許吞掉任何 `fulcrum` 開頭的名字。★ ★ **誠實說出它守不住什麼**：它攔不住有人把一個真正屬於本儲存庫、名字裡卻沒有 `fulcrum` 的卷（`pingora-up994-target` 就是這個形狀）寫進白名單。⛔ 那一格沒有門，只有紀律。

★ **噪音賬 —— 這是這個數字在本儲存庫的唯一一處**（2026-09-05 在 `ShanirePCX` 上實測，⛔ 換臺機器自己數）：全機 **19** 個卷 − 管轄內 **2**（本樹 target 卷 + `fulcrum-cargo`）− 別人家 **10**（betai ×2 · BetaPass ×1 · nxtstrict ×2 · Plumbline ×2 · WenTian ×3）− docker 匿名 **7** = **0** ⇒ 穩態下這個桶**一行都不打**；真出現一個 `pingora-up994-target` 才打一行。那個貴的量具沿用同一條紀律：只在真有東西要報時問一次。

⚠ ⚠ ★ **這一行本身栽過一次，值得當反面教材留著。** 它的第一版寫的是「別人家 **11** − 匿名 **6**」，兩個數**都是錯的**（實際 10 與 7），而且被抄進了 `PLAN.md` 與 `docker-run.sh` 共**三處**。★ ★ ★ 它沒被任何東西抓住，原因不是門不夠多，而是 **`19 − 2 − 11 − 6` 與 `19 − 2 − 10 − 7` 的和都是 0** —— 那個「= 0」才是被引用的結論，中間項錯了**不與任何東西矛盾**。⇒ 修法不是「再加一道核數的門」（沒有門夠得著一臺機器上此刻有幾個卷），是**讓這個值只存在一處、其餘只留結論與指針**；`PLAN.md` 與 `docker-run.sh` 現在都不再複述中間項。★ 同族見 §10 裡對「抄件」的其它幾處處置。

★ **九條自測**（`selftest_unattributed_vols`），⚠ **全部用合成輸入**、⛔ 不相依宿主上此刻恰好有什麼卷 —— 因為在一臺已經清乾淨的機器上這個桶永遠打不出一行，而「桶是空的」與「這段程式碼根本沒跑」長得一模一樣（2026-09-04 那次正是為此不得不造四個探針卷）。七條釘判據（別人家 ⇒ `foreign` · 匿名 ⇒ `anonymous` · ★★★ **承重**：逃逸過的真名字 ⇒ `unattributed` · 白名單不許吞 `fulcrum*` 兩條 · 匿名判據釘在**長度**上，63 位不算 · 白名單是前綴不是子串），兩條釘集合層（管不著的原樣報出來 / 全在管轄內時一個字都不打）。

⚠ 集合層那兩條是**必須的**：判據函式對了、而循環裡一個 `continue` 放錯位置，症狀同樣是「一行都不打」。

## ★ ★ 同一棵樹上同一時刻只許跑一次門禁

卷分開只解決「別的樹」。同一棵樹上並行跑兩次門禁，兩次仍然共用那一個卷——回到同一種壞法。⇒ **拿卷之前**先取一把與卷同名的鎖，拿不到就當場說清楚並退出，**不排隊、不靜默繼續**。

★ 鎖用 `mkdir` 而不是 `flock`：宿主是 Windows + Git Bash（MSYS），**這臺機器上沒有 `flock`**（`command -v flock` 空手而歸）。寫一把在這臺宿主上其實不生效的鎖，比沒有鎖更壞——它會讓人以為並行已經攔住了。`mkdir` 在目錄已存在時必然失敗且不留副作用，NTFS 與 ext4 上都成立。⚠ 絕不可以退化成 `[ -d "$dir" ] || mkdir "$dir"`：那是兩步，中間正好是競態視窗。

★ 鎖放在宿主機暫存目錄（`${TMPDIR:-/tmp}/fulcrum-gate-locks/<卷名>.lock`），不放儲存庫裡——儲存庫裡那份會被「`target/` 必須為空」和行尾兩道門看見。持有者寫進 `pid`，處理程序不在了就**接管並說出來**（否則一次 Ctrl-C 會把這棵樹永久鎖死）。`docker-run.sh` 持鎖期間調 `systemd-run.sh` 靠匯出的 `FULCRUM_GATE_LOCK_HELD` 放行，而後者單獨跑時自己上鎖。

★ 這兩組性質各有**兩個方向**的自測，掛在 `docker-run.sh` 每次執行都跑的那批 `selftest_*` 裡（與位元組探針、`*_ONLY` 判據同一處）：兩棵樹名字不同 / 同一棵樹名字穩定 / 換鏡像仍然換卷 / 名字是合法卷名；鎖被佔用時明確失敗並說出原因 / 放開後拿得到且退出時還得回來 / 同一處理程序樹內可重入 / 陳舊鎖接管時喊出來。

# 兩個 Windows 宿主機上的坑

這兩條已經寫進指令碼，改指令碼時不要碰掉：

1. ★ **`export MSYS_NO_PATHCONV=1`** —— git-bash 會把容器內的 `/w` 之類路徑改寫成 Windows 路徑
2. ★ **掛載源必須是 Windows 形式的絕對路徑** —— 指令碼用 `cygpath -m` 轉換

還有第三條不在指令碼裡，在 [`.gitattributes`](../../.gitattributes)：

★ ★ **行尾必須無條件是 LF。** 開發機上 `core.autocrlf=true` 時，checkout 會把 `tests/m0/*.sh` 轉成 CRLF，容器裡的 bash 會當場報 `\r: command not found`——**而且是克隆之後才發作，寫程式碼時看不出來**。行尾轉換發生在 checkout 不在提交，所以 `git status` 乾淨**並不能證明什麼**。

# 判定

★ **`tests/m0/run.sh` 的結束代碼即結論。** 它非零即失敗，並列印為什麼。

它驗兩類證據：

- **直接證據**：第二代必須在紀錄裡報告繼承了兩個自建監聽器的 fd（`[raw-tcp] INHERITED fd=` / `[raw-udp] INHERITED fd=`）
- **間接證據**：三類流量零中斷，且**每類至少成功過一次**（★ 否則「零失敗」可能只是因為探針根本沒跑起來）

★ 第二條裡的「至少成功過一次」值得注意——**它防的是「測試本身無效但看起來全綠」**。

# 提交前

跑 `bash tests/m0/docker-run.sh`；★ **新加或改動了門，就先證明它能紅**
（翻轉條件、破壞輸入、或刪掉修復，看著它失敗，然後放回去）。

★ 一道**只被觀測到綠過**的門，與一道**根本沒在跑**的門，是無法區分的。

# 相關

[M0 接縫驗證](/verification/m0-seam.md) · [相依策略](/platform/dependency-policy.md) · [工作方式](/governance/working-agreement.md)
