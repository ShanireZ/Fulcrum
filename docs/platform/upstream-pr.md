---
type: 技術基線
title: 向上遊 pingora 提 PR 的流程清單
description: 兩份投稿（G32 的 lru 升級、G38 的 fd 洩漏修復）共用一套上游規範；本頁把它逐條落成可核對項。
resource: ../../vendor/pingora/FORK.md
tags: [依賴, 上游, 流程, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-13T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G32（只推兩條 + owner 本人提交）
  - id: contributing
    resource: https://github.com/cloudflare/pingora/blob/main/.github/CONTRIBUTING.md
    title: 上游 .github/CONTRIBUTING.md
  - id: build-yml
    resource: https://github.com/cloudflare/pingora/blob/main/.github/workflows/build.yml
    title: 上游 .github/workflows/build.yml（PR 必過的 CI 門）
---

★ **本頁的每一條都來自實際讀取上游儲存庫，不是慣例推測。** 採集日期 2026-08-13，採集方式見末節。

# 為什麼只推兩條（G32）

G32 拍板時寫的是「推 `lru` 與 `prometheus` 兩條」。★ ★ **實測之後修正為：只推 `lru` 一條。**

| | 上游 **main** 現值 | 結論 |
|---|---|---|
| `lru` | `0.16.3`（解析到 **0.16.4**，即 RUSTSEC-2026-0253 影響的版本）| ★ **推**，目標 `0.18.2` |
| `prometheus` | ★ ★ **已經是 `0.14`**，`protobuf` 已是 **3.7.2** | ★ **不用推——上游自己早做了**，只是尚未發版（`0.8.1` 是最後一個 release，main 已領先 167+ 提交）|

★ **這條要連帶記住**：樞衡 fork 裡的 `prometheus` 與 `nix` 兩條改動，**下一次上游發版就會白撿消失**。rebase 時先查上游有沒有自己做了，別悶頭重做。

`brotli` / `rand` / `sfv` **不推**——純版本號、無公告，`sfv` 那條還是樞衡自己遷的（上游 main 至今是 `0.10.4`），一起推容易把有說服力的那條拖黃。

# ★ 硬性流程（漏一條就等於沒遵守）

## 1. ★ ★ 必須先開 issue，再開 PR

> CONTRIBUTING：**"More often than not, start by filing an issue on GitHub… Non-trivial PRs will also require a GitHub issue."**

它列的「不需要 issue 的小修」是：**改錯別字、小重構、文件或註解編輯**。

★ **依賴的大版本升級 + 跨 crate 的呼叫點適配不在其中，屬於 non-trivial** → **必須先開 issue**。直接開 PR 就是違反流程。

issue 的作用是先談設計，避免「做完了才發現方向不被接受」。

## 2. PR 正文引用該 issue 號

## 3. ★ 不要期待自己的 PR 被直接 merge

上游的合併方式是**批量重放**，不是合你的分支：

```text
Needs Review → （可能）Changes Requested → Accepted
   → 维护者把你的 commit rebase 到 main 的**另一个批量 PR** 里（同步内部仓库）
   → 你的 PR 被 close
```

★ **「PR 被 close」在這裡是成功而不是拒絕。** 判斷是否完成實作要看改動有沒有出現在 `main`，別看 PR 狀態。

## 4. 時限：沒有承諾

> "internal contributions will take priority… We can't promise we will review or address all PRs or issues in a timely manner."

★ **所以 G32 的「其餘常年 rebase」不是備選而是必須**——fork 的維護不能建立在「PR 會被及時接受」這個假設上。

## 5. 行為準則

適用 [Contributor Covenant Code of Conduct]（Cloudflare 組織級）。聯繫電子信箱 `opensource@cloudflare.com`。

[Contributor Covenant Code of Conduct]: https://github.com/cloudflare/.github/blob/26b37ca2ba7ab3d91050ead9f2c0e30674d3b91e/CODE_OF_CONDUCT.md

## 6. DCO / CLA

★ **儲存庫內沒有任何 DCO 或 CLA 的 workflow 與檔案**（`.github/` 下只有 CONTRIBUTING、兩個 issue 範本、五個 workflow；**沒有 PR 範本**）。

★ 但機器人可能不體現在儲存庫裡。**保險做法是提交時就帶 `Signed-off-by:`**——多寫無害，缺了要返工。

# ★ CI 門（`build.yml`，每個 PR 必過）

toolchain 矩陣：**`nightly` / `1.85.0`（MSRV）/ `1.97.1`（latest stable）**，`fail-fast: false`。

| 步驟 | 命令 | 跑在哪些 toolchain |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | 全部 |
| 檢查 | `cargo check --workspace`（`1.85.0` 時加 `--exclude pingora-foundations`）| 全部 |
| 測試 | `cargo test --verbose --lib --bins --tests --no-fail-fast` | 除 `1.85.0` |
| 文件測試 | `cargo test --verbose --doc` | 除 `1.85.0` |
| clippy | `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | **僅 `1.97.1`** |
| 稽核 | `cargo audit` | **僅 `1.97.1`** |
| 未用依賴 | `cargo machete`（0.7.0）| **僅 `1.97.1`** |

★ 測試環境裝了 **openresty**（當作測試用的伺服器端），本機復跑要一併準備。

## ★ ★ MSRV 是硬底線，且這次正好卡住

上游 MSRV = **1.85.0**。已逐個核過候選版本的 `rust-version`：

| crate | 目標版本 | 宣告的 MSRV | 結論 |
|---|---|---|---|
| `lru` | **0.18.2** | **1.85.0** | ★ **正好等於上游底線，不需要抬 MSRV** |
| `prometheus` | 0.14.0 | 1.81 | ✅ 低於底線 |
| `protobuf` | 3.7.2 | 未宣告 | ✅ 無約束 |

★ **`lru` 0.17.0 起 MSRV 就是 1.85.0**（0.16.x 是 1.70.0）。這是整個 PR 可行性的前提——**若上游哪天把 MSRV 降回 1.85 以下，這條 PR 立刻不成立**。

★ 上游 `.cargo/config.toml` 設了 `[resolver] incompatible-rust-versions = "fallback"`（MSRV 感知解析）。

## ★ ★ 他們的 audit 忽略名單裡**沒有**這兩條

`.cargo/audit.toml` 只忽略三條，全是 `rustls-webpki` 經 `aws-sdk-s3-transfer-manager` 引入的：`RUSTSEC-2026-0098` / `RUSTSEC-2026-0099` / `RUSTSEC-2026-0104`。

`RUSTSEC-2026-0253`（lru）**不在**忽略名單裡。

## ★ ★ ★ 但實跑結果推翻了「所以他們 CI 是紅的」這個推斷

在 main（`0046038`）上實跑 `cargo audit`：

```text
Scanning Cargo.lock for vulnerabilities (677 crate dependencies)
warning: 4 allowed warnings found
  derivative 2.2.0     unmaintained  RUSTSEC-2024-0388
  paste 1.0.15         unmaintained  RUSTSEC-2024-0436
  rustls-pemfile 2.2.0 unmaintained  RUSTSEC-2025-0134
  lru 0.16.4           unsound       RUSTSEC-2026-0253
```

★ **結束代碼 0——它是綠的。** 原因：RustSec 把這條歸類為 **`unsound`（warning）而不是 vulnerability**，而 `build.yml` 跑的是**裸 `cargo audit`**，warning 不會讓它失敗（除非加 `--deny warnings`）。

★ ★ **所以 PR 的立論不能寫成「你們的 CI 紅了」——那是假的，寫進去會當場失去可信度。** 正確的立論是：

> `cargo audit` 在 main 上報告 `lru 0.16.4` 的 RUSTSEC-2026-0253（unsound，`LruCache::pop()` 缺乏 panic 安全導致潛在 UAF）；`lru 0.18.2` 已修復，且其 MSRV 正好是 1.85.0，與本儲存庫的 MSRV 底線一致，無需抬 MSRV。

★ **這是本頁最該記住的一條方法論**：**先跑，再寫立論**。基於「忽略名單裡沒有它」推出「CI 必紅」在這裡是錯的。

# ★ 範圍提醒：上游的改面比樞衡 fork 大得多

`lru` 在上游是 `[workspace.dependencies]` 裡的一行，被 **五個 crate** 引用：

```text
pingora-cache   lru = { workspace = true }
pingora-core    lru = { workspace = true, optional = true }   ← 仅 s2n 后端
pingora-lru     lru = { workspace = true }
pingora-pool    lru = { workspace = true }
tinyufo         lru = { workspace = true }
```

★ ★ **樞衡的 fork 只 vendor 了 `pingora-core` 與 `pingora-pool`，而且 `pingora-core` 裡用 `lru` 的那個檔案在 s2n 後端下——rustls-only 建置根本不編譯它。**

**結論：fork 裡現成的改動不足以直接變成上游 PR。** 上游 PR 必須覆蓋 `pingora-cache` / `pingora-lru` / `tinyufo` 以及 **s2n 後端**的呼叫點，並過 `cargo check --workspace`。

# 提交資訊慣例

上游 main 近期提交（無 conventional-commits 強制）：

```text
ci: bump latest-stable toolchain from 1.91.1 to 1.97.1
Bump h2 dependency from 0.4.11 to 0.4.14      ← ★ 同类先例
Close unclaimed inherited listening sockets on graceful upgrade
compression: add can_decompress and will_decompress predicates
```

**句首大寫的祈使句**，可選 `ci:` / `compression:` 這類領域前綴。

★ **`Bump h2 dependency from 0.4.11 to 0.4.14` 是最好的範本**——同類改動已經被接受過，照它的形狀寫：

```text
Bump lru dependency from 0.16.3 to 0.18.2
```

# 誰來提交

⚠ **口徑2026-08-14 變更（G40）**：owner 在自己登入的前提下**授權代發**。

| | 原口徑（G32 附加要求）| 現口徑（G40）|
|---|---|---|
| 誰點「提交」| ★ owner 本人 | ★ **owner 授權後可代發** |
| 用誰的身分 | owner 的 GitHub 帳號 | **不變**——仍是 owner 的帳號與 `Signed-off-by` |
| 發之前 | 複核並優化草稿 | **不變**，且**每次都要重查上游是否已修/已有人報** |

★ **沒變的那一條才是要緊的**：對外發布用的始終是 owner 的身分，所以
**「發之前把每條立論都跑一遍」這條紀律不因為誰點按鈕而放鬆**。

# ✅ 材料已備好（2026-08-13）

投稿材料在 [`upstream-pr/`](../../upstream-pr/README.md)：issue 草稿、PR 正文草稿、可 `git am` 的補丁（**一行**，已帶 `Signed-off-by`）。

## 七個門的實測結果（上游 main `0046038` + 本補丁）

| 門 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo check --workspace`（1.97.1）| ✅ **零原始碼改動** |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ |
| `cargo machete` | ✅ |
| `cargo test --lib --bins --tests --no-fail-fast` | 116 失敗，**與未改動的 main 逐項相同**（缺 openresty；雙向差集為空）|
| `cargo audit` | ★ **不變**，見下 |

## ★ ★ 兩條必須寫進投稿的誠實說明

1. **這一行不會讓 `cargo audit` 那條消失。** 實測：改後 lock 裡 `pingora-cache` / `pingora-core` / `pingora-lru` / `pingora-pool` / `TinyUFO` **全部是 0.18.2** ✅，但 **`aws-sdk-s3` 獨立要求受影響版本的 `lru`**（`--ignore-rust-version` 下 0.16.4，普通解析下 0.12.5）。它不在預設 feature 圖裡，但在 lock 裡，而 audit 掃的是 lock。**改動的真實價值是「pingora 自己的五個 crate 離開了受影響版本」，不是「修好了公告」。**

2. **不要寫「你們 CI 紅了」**——上文已實測那是假的。

## ★ 一條實驗設計的教訓

第一輪跑門時 MSRV 1.85.0 報紅（`s2n-tls requires rustc 1.91`），一度被當成 `lru` 引起的迴歸。2×2 對稱實驗後確認：**紅完全由鎖定檔生成方式決定，與 `lru` 無關**——是拿 `cargo audit` 步驟的 `--ignore-rust-version` 鎖定檔去跑 `cargo check` 造成的，而上游 CI 的 MSRV 檔**不預先生成 lock**。

| | 普通 lock | `--ignore-rust-version` |
|---|---|---|
| 基線 | ✅ rc=0 | ✗ rc=101 |
| lru 0.18.2 | ✅ **rc=0** | ✗ rc=101 |

★ **對照實驗要把變數控住**，否則會把自己的實驗汙染當成被測物件的缺陷報上去。

# 怎麼重新採集本頁的事實

```bash
git clone --depth 1 --branch 0.8.1 https://github.com/cloudflare/pingora /tmp/pingora
cd /tmp/pingora && git fetch --depth 200 origin main
git show FETCH_HEAD:.github/CONTRIBUTING.md
git show FETCH_HEAD:.github/workflows/build.yml
git show FETCH_HEAD:.cargo/audit.toml
git log --format=%s FETCH_HEAD -12

# 立論要用實跑結果，不要只憑忽略名單推斷
git checkout -B prmain FETCH_HEAD
cargo install --locked cargo-audit
cargo generate-lockfile --ignore-rust-version && cargo audit
```

MSRV 用 crates.io API 查 `rust_version`：`https://crates.io/api/v1/crates/<name>`。

# ★ 第二份投稿：`get_fds_from()` 的兩處 fd 洩漏（G38，2026-08-14）

本頁上面那套流程約束**逐條同樣適用**（先開 issue、引用 issue 號、別指望直接 merge、
帶 `Signed-off-by`、由 owner 本人提交）。這裡只記與投稿一不同的地方。

**它是行為修復，不是版本號升級**，所以立論方式不同：

| | 投稿一（`lru`）| 投稿二（fd 洩漏）|
|---|---|---|
| 立論 | 引用一條安全公告 | ★ **可重現的缺陷 + 實測資料 + 迴歸測試** |
| 原始碼改動 | 零 | 兩處，都在 `get_fds_from()` 內 |
| 測試 | 無（不需要）| ★ 新增一個，**分別**能抓住兩處缺陷 |
| MSRV | 卡在 1.85.0（`lru` 的底線正好等於上游）| 不涉及，實跑 `+1.85.0 check` 通過 |

★ ★ **動手前先查了上游做沒做**（G32 留下的紀律，`prometheus`/`nix` 兩條就是沒查而白改的）：
上游 `main` 的同一函式仍是 `MsgFlags::empty()`、accept 的連接仍然沒人關，
且儲存庫裡**沒有任何 issue/PR 提過 CLOEXEC**。

★ **PR 正文裡主動寫了一段「範圍說明」**：同一函式里 `SOCK_CLOEXEC` / `accept4()` 那兩處
也缺，但它們的 fd 在函式回傳前就關掉了，只暴露一個很窄的 fork 競態視窗，
故**有意不動**並說明理由。★ 審閱者一定會問「為什麼只修這兩處」——
**與其等他問，不如先答**；而答案必須是真實的取捨，不是遺漏。

材料與實測結果見 [`upstream-pr/README.md`](../../upstream-pr/README.md)。

# 相關

[fork 說明](../../vendor/pingora/FORK.md) · [依賴策略](/platform/dependency-policy.md) · [供應鏈現狀](/platform/supply-chain.md)
