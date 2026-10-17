---
type: 技術基線
title: 供應鏈現狀
description: fork 後只剩 1 條失維公告、0 個真漏洞；剩餘陳舊包全部被「已是最新版」的第三方 crate 卡住。
resource: ../../Cargo.lock
tags: [依賴, 安全, 必讀, 易錯]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G29（追新 + 24 小時懷疑期）、G30（fork pingora 放寬上界）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 風險表（Pingora 上游演進與樞衡定製的合併成本）
  - id: crates-io
    resource: https://crates.io/api/v1/crates/pingora-core/0.8.1/dependencies
    title: pingora-core 0.8.1 在 crates.io 上宣告的依賴需求
  - id: osv
    resource: https://api.osv.dev/v1/querybatch
    title: OSV.dev 對 Cargo.lock 全量 176 包的公告查詢
---

★ **本頁記的是先前的實測快照。** 資料來自對 `Cargo.lock` 全部 176 個 registry 包逐個查 crates.io 與 OSV.dev。**重跑方式在末節。**

# 結論先行

★ **G30 的 fork 已執行完畢。** 下表是前後對照：

| | fork 前 | fork 後 |
|---|---|---|
| 鎖定包總數 | 176 | **162** |
| 陳舊包 | **44** | **17**（排除非 Linux 目標後 **12**，其中 2 項還不參與編譯）|
| 安全公告 | 4 | ★ **1** |
| 其中**真漏洞** | 2（`lru` UAF、`protobuf` DoS）| ★ ★ **0** |
| 5 個直接依賴 | 全是最新 | 全是最新 |
| Docker 基鏡像 | `rust:1-bookworm`（Debian 12 oldstable）| **`rust:1-trixie`**（Debian 13，Rust 1.97.1）|

**只剩 `daemonize` 一條**（RUSTSEC-2025-0069，失維、無 CVE、無可升版本）。★ **它是被權衡後保留的，不是漏掉的**——換掉它意味著手寫特權丟棄程式碼，理由與出路見 [`FORK.md`](../../vendor/pingora/FORK.md) 第 9 節（此處原寫「第 7 節」，不準）。
✅ **2026-09-24：這一條也沒了** —— fork rebase 到 pingora 0.9.0，上游自己把 `daemonize` 換成了 `daemonix`（`ae96f7e`），兩把鎖裡都不再有 `daemonize`，`tools/supply-audit.py` 的 `ACCEPTED` 隨之清空。
★ 同一次 rebase 新進依賴圖的包：`flurry`（`ListenFds` 按位址的鎖）、`seize`（flurry 的回收）、`dashmap` 5（連接池重寫）、`daemonix`（unix 上無條件依賴；樞衡恆前台執行、不走它）。都過了 24 小時懷疑期；vendor 鎖裡另有一整棵 `dial9*` / `aws-sdk-s3` 可選依賴被 fork 主動裁掉（它會把 aws-lc-rs 帶進鎖，見 FORK.md 文首）。
✅ **2026-09-24 緊接著的依賴追新**（`dep-check.py --apply`，一次 39 項；`thiserror` / `thiserror-impl` 2.0.21 按 **G153** 在懷疑期內破例採納）：
`rustls` 0.23.43 → 0.23.45 修掉 **RUSTSEC-2026-0285**（兩把鎖都有這條）⇒ 未登記的公告回到 0。
⚠ **授權條款變了一族**：`zstd` 0.14 / `zstd-safe` 8 / `zstd-sys` 2.1 從 MIT（或 MIT / Apache-2.0）改成 **BSD-3-Clause** ——
寬鬆授權，FSF 把它（Modified BSD）列為與 GPL 相容；發佈時的第三方宣告要收錄它的版權與免責條款。
★ 同一筆裡 vendor 迴歸網 [2/5] 的判據收窄（**G154**）：根側有、vendor 側缺的版本，由 `cargo tree -i` 答「有沒有 pingora crate 在用」，
因為 rcgen 0.14.10 → pem 4 把 `base64` 0.23.1 只帶進了根側。
✅ **2026-10-16 的依賴追新**（`dep-check.py --apply`，一次 37 項，全部過了 24 小時懷疑期，**沒有破例**）：
`rustls` 0.23.45 → 0.23.46 修掉 **RUSTSEC-2026-0346 / 0347 / 0348 / 0349**（兩把鎖都有；修復版 2026-10-14 09:54 UTC 發佈，採納時約 43 小時）。
四條都不進產物：根鎖那份是「鎖裡有、圖裡沒有」（`cargo tree --workspace -i rustls@0.23.45 -e all --target all --all-features` 什麼都打不出），
vendor 鎖那份只有迴歸網會編（`pingora-rustls` 成員、`pingora-core` 的 dev-dep `reqwest`）。owner 仍選採納、不登記 `ACCEPTED` —— 出路已過懷疑期，`ACCEPTED` 留給沒有出路的公告。
同批的 `h2` 0.4.20 自己在 codec 層拒掉值不同的重複 `content-length`，迴歸網兩條上游測試因此紅 ⇒ fork 改動 20（[`FORK.md`](../../vendor/pingora/FORK.md)）。
`--apply` 的結束代碼是 1，但那是良性的：9 項已被同族包先帶到位，`name@旧版本` 不再匹配；兩把鎖逐包核過，37 項全在目標版本。
此後根鎖那一側也改為按結果鎖判成敗（[依賴策略](/platform/dependency-policy.md)「指令碼做了什麼」第 4 條），同一形狀退 `10`。
⚠ 同一次掃出、**仍未登記**的：`unsafe-libyaml` 0.2.11（RUSTSEC-2024-0449，失維、無修復版，**`[编译]`**，`pingora-core` → `serde_yaml`，兩把鎖都有）
與 `quinn-proto` 0.11.16 × 5（RUSTSEC-2026-0338…0342，只在 vendor 鎖、`[仅 lock]`）⇒ `supply-audit.py` 根鎖 1 條、vendor 鎖 6 條，`dep-check.py` 仍亮 `160`，待 owner 另行處置。
⚠ `dep-check.py` 的候選只來自**根清單**的 `cargo update --dry-run` ⇒ 只在 vendor 鎖裡的包（如 `quinn-proto`）`--apply` 永遠夠不到。
✅ **2026-10-16 同日：`unsafe-libyaml` 離開了兩把鎖** —— fork 改動 21 把 `pingora-core` 的 `serde_yaml`（已棄用）換成公告推薦的純安全 Rust 實作 `serde-saphyr` 2.0.0（[`FORK.md`](../../vendor/pingora/FORK.md)）。
★ owner 選的這一條**推翻了 agent 推薦項**（推薦的是登記 `ACCEPTED`：產品從不解析 YAML —— 走 `Server::new_with_opt_and_conf`、`ServerConf` 在 Rust 裡構、`opt.conf` 被忽略）。
兩把鎖各新進 10 / 11 個包（`serde-saphyr` · `granit-parser` · `annotate-snippets` · `encoding_rs` 等），全部過 24 小時懷疑期、無 yank、授權寬鬆；根鎖同時少了 `ryu`，編譯圖 202 → 210 個。
⚠ `encoding_rs` 帶 **BSD-3-Clause**（WHATWG 資料），發佈時的第三方宣告要收錄。根鎖 `supply-audit.py` 由此 exit 0；vendor 鎖剩 `quinn-proto` × 5。
✅ **2026-10-16 同日：`quinn-proto` × 5 也清了** —— 只改 vendor 鎖：`quinn` 0.11.11 → 0.11.12、`quinn-proto` 0.11.16 → 0.11.19（owner 選連 `quinn` 一起升；發佈於 09-14 / 09-30，遠過懷疑期、無 yank），根鎖逐位元組不變。兩個包誰都不編（`reqwest` 的弱依賴 `quinn?/ring` 只把它們記進鎖；vendor workspace 全 feature、全 target 的 `cargo tree -i` 什麼都打不出）；`errno` / `quinn-udp` / `winapi-util` 的 `windows-sys` 邊隨之回到 0.59.0（與根鎖一致，只在 Windows 目標上）。兩把鎖 `supply-audit.py` 都 exit 0。
⚠ ⚠ **此處原先鏈到 [待定清單](/governance/open-questions.md) 的 `D12`，那是過期的**（2026-09-08 更正）——
`D12` 已結案（`G32`），而那份清單裡**一個 D 號都沒有**，鏈過去什麼都看不到。
★ 與本頁下文那句「它已經不在待定清單裡了」原本是**同一份檔案裡自相矛盾的兩句**。★ 此後 `rustls-pemfile`（RUSTSEC-2025-0134）**已由 G45 遷走**，不再在名單上。

> ★ ★ ★ ** 重要更正：上面這句「只剩一條」的適用範圍比它讀起來窄。**
>
> 本頁所有數字都是**根 `Cargo.lock`** 的。而根鎖**不含任何未啟用的可選依賴**——實測
> `pingora-rustls`／`rustls`／`ring`／`rustls-native-certs`／`sentry`／`x509-parser`／`ouroboros`
> **一個都不在裡面**。G6 把 TLS 後端鎖死在 rustls，所以 M1 打開 `features = ["rustls"]` 的那天，
> 產品依賴圖會 **133 → 175**，一次進來 **42 個本頁從來沒數過的 crate**。
>
> **對 vendor 鎖跑一次同一個指令碼（`--lock vendor/pingora/Cargo.lock`），就多出一條未登記公告**：
>
> | | 公告 | 類型 | 現狀 |
> |---|---|---|---|
> | `rustls-pemfile` 2.2.0 | **RUSTSEC-2025-0134** | 失維（儲存庫 2025-08 起歸檔）| ~~⏳ 待 owner 處置~~ → ✅ **由 G45 遷到 `rustls-pki-types` 的 `PemObject`**（遷移前先補了 18 條特徵化測試 + 三條反證）|
>
> ★ 它與 `daemonize` **不同**：`daemonize` 無路可走（換掉＝手寫特權丟棄），
> 而這條**有乾淨的遷移路徑**——上游建議改用 `rustls-pki-types` 的 `PemObject` trait，
> 而 **`rustls-pki-types 1.15.1` 早就在依賴圖裡**（`PemObject` 需要 ≥1.9.0）。
> fork 裡只有 4 處呼叫點，全在 `pingora-rustls/src/lib.rs`。
> ⚠ 但它動的是**證書與私鑰的解析路徑**，屬安全敏感，不是清單改一行的事。
>
> 完整經過見 [rustls 接縫驗證](/verification/m1-rustls-seam.md)。

> ★ ★ ★ **還有一條方向相反、而且更難想到的：`Cargo.lock` 也可能比依賴圖**多**。**
>
> 上面那條說的是「鎖裡**少**了未啟用的可選依賴 ⇒ 稽核看不見它們」。反過來的那一面是：
> 關掉 `instant-acme` 的 `hyper-rustls` feature 之後，`hyper-rustls` / `rustls` / `tokio-rustls` /
> `schannel` **仍然寫在鎖裡，而已經沒有任何人連結它們**。
>
> **成因**：`instant-acme` 的 `ring` feature 裡有一句 `"hyper-rustls?/ring"`。那個 `?` 的語義是
> 「只有當可選依賴 `hyper-rustls` 被別處打開時，才順帶開它的 `ring`」——
> ⚠ **feature 解析確實沒有打開它，而包級解析照樣把它寫進了鎖。**
>
> ✅ **隔離實驗**：新建一個只依賴
> `instant-acme = { default-features = false, features = ["ring", "x509-parser", "time"] }`
> 的空 crate，`cargo generate-lockfile` 之後 —— **鎖裡 129 個包，`cargo tree` 只有 72 個**。
>
> ⇒ **一條可帶走的口徑**：
>
> | 問 | 用什麼答 |
> |---|---|
> | **鎖裡寫著**哪些 | 讀 `Cargo.lock`（`supply_gates.rs` 門 1 / 門 4）。⚠ 它既可能**少**（未啟用的可選依賴）也可能**多**（本條）|
> | **依賴圖裡真有**哪些 | `cargo tree -e all --workspace --target all --locked`（門 5）|
> | **產物裡真的連結了**哪些 | ✅ **判符號表**：`tests/ci/tls-linkage.sh` 取數 + `tests/musl/product.sh` 判定（**G138** 結案 **D23**）。⛔ 不判 `strings` —— `rustls` 那 4 次全來自純類型 crate `rustls_pki_types` |
>
> ⚠ ⚠ **這三問此前被當成同一問**，而門 4 的註解裡那句「換完時我會先紅一次提醒你改」
> 正是建立在那個混同上 —— **③ 換完了，它沒紅。**

## ★  修正：還原了四條純裝飾的上界，安全態**實測未變**

`strum` / `strum_macros` / `daggy` / `openssl-probe` / `windows-sys` 已還原為上游值（判據與理由見 [`FORK.md`](../../vendor/pingora/FORK.md)「還原了四條純裝飾的上界」）。**對全量 `Cargo.lock` 重查了一次 OSV**：

| | 還原前 | 還原後 |
|---|---|---|
| 鎖定包總數 | 169 | 178 |
| 安全公告 | 1（`daemonize`）| ★ **1（`daemonize`）——沒變** |
| 其中真漏洞 | 0 | ★ **0——沒變** |

★ **「+9 個包」全是假象**：新增的 10 個 `windows_*` / `windows-targets` 是 `windows-sys 0.59` 的依賴展開方式與 `0.61` 不同造成的，**在 Linux 目標上一行都不編譯**。這正是本頁反覆強調的 **報告陳舊依賴必須分桶**。

★ ★ **反而少了兩個真正在編譯的陳舊包**：`hashbrown 0.15.5` 與 `foldhash 0.1.5` **從依賴圖裡消失了**——本頁下文那張表說它們被 `petgraph 0.8.3` 卡住，而 `petgraph 0.8.3` 正是 `daggy 0.9` 拖進來的。還原 `daggy` 把這條成因一併帶走了。**下文那張「10 項」的表因此要讀作 8 項。**

## ★ 剩下 10 個真正在編譯的陳舊包，一個都升不動——已逐條查過綁定約束

**它們全部被「本身已經是最新版」的第三方 crate 卡住**，也就是說 **不是我們落後，是生態鏈上游還沒采納新大版本**：

| 陳舊包 | 卡它的 requirement | 卡它的包是最新版嗎 |
|---|---|---|
| `allocator-api2` 0.2.21 | `hashbrown 0.17.1` → `^0.2.9` | ★ 是 |
| `base64` 0.22.1 | `sfv 0.15.0` → `^0.22.1` | ★ 是 |
| `alloc-no-stdlib` · `alloc-stdlib` | `brotli 8.0.4` → `<3` / `~0.2` | ★ 是 |
| `thiserror` · `thiserror-impl` 1.0.69 | `protobuf 3.7.2` → `^1.0.30` | ★ 是 |
| `getrandom` 0.3.4 | `ahash 0.8.12` → `^0.3.1` | ★ 是 |
| `syn` 2.0.119 | `strum_macros 0.28.0` 等 → `^2.0` | ★ 是 |
| `hashbrown` 0.15.5 · `foldhash` 0.1.5 | ★ **`petgraph 0.8.3` → `^0.15.0`**（`indexmap` / `lru` 都已要 `^0.17`）| ★ 是 |

★ ★ **結論：要再往下追，就得同時 fork `brotli` / `sfv` / `hashbrown` / `protobuf` / `ahash` / `petgraph` 六七個包——而這一層已經沒有任何安全公告，純粹是版本號。性價比為負，明確不做。**

★ **另外 2 項（`bitflags 1.3.2`、`miniz_oxide 0.8.9`）根本不參與編譯**：用 `cargo tree --target x86_64-unknown-linux-gnu -e normal` 核過，它們只存在於其他 target/feature 的解析結果裡。

## ★ 先前的最新一次實測（`tools/supply-audit.py`，帶分桶）

| | 數值 |
|---|---|
| 鎖定包總數（registry） | **171** |
| `x86_64-unknown-linux-gnu` 上真正參與編譯 | **138** |
| 陳舊包（參與編譯） | **13** |
| 陳舊包（僅在 lock 裡，不進產物） | **17** |
| 安全公告 | **1**（`daemonize`，已登記並接受）|
| ★ 其中**未登記** | ★ **0** |

參與編譯的那 13 個：`alloc-no-stdlib` · `alloc-stdlib` · `allocator-api2` · `base64` · `daggy` · `getrandom` · `openssl-probe` · `petgraph` · `strum` · `strum_macros` · `syn` · `thiserror` · `thiserror-impl`。

★ 其中 `daggy` / `petgraph` / `strum` / `strum_macros` / `openssl-probe` 是 ** 主動還原為上游值的**（見上一節），不是升不動；其餘仍被第三方 crate 的上界卡住。

fork 本身見 [`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md)。**下面幾節記的是 fork 之前的診斷**，保留是因為它解釋了天花板的成因——那套機制沒有因為 fork 而消失，下次遇到同類問題還要用。

# 直接依賴：已經是最新

| crate | `Cargo.toml` 下界 | 鎖定版本 | crates.io 最新 |
|---|---|---|---|
| `pingora-core` | `>=0.8.1` | 0.8.1 | **0.8.1** ✅ |
| `tokio` | `>=1.53.1` | 1.53.1 | **1.53.1** ✅ |
| `async-trait` | `>=0.1.89` | 0.1.92 | **0.1.92** ✅ |
| `log` | `>=0.4.28` | 0.4.33 | **0.4.33** ✅ |
| `env_logger` | `>=0.11.8` | 0.11.11 | **0.11.11** ✅ |

G29 的 `>=` 無上界寫法**是對的，而且已經在生效**——三個下界落後於鎖定版本，正說明 Cargo 一直在往上跳。

# ★ 為什麼另外 44 個升不動

## 機制：脫字號需求自帶上界

`Cargo.toml` 裡寫 `brotli = "3"` **不是**「≥3」。它是**脫字號需求** `^3` = `>=3.0.0, <4.0.0`。

★ **每一個不帶符號的版本字串都自帶一個上界**，落在下一個 semver 破壞性邊界上。所以一份從沒寫過 `<` 的清單，可以佈滿上界。

Cargo 必須**同時滿足依賴圖裡的每一條需求**。樞衡只點名了 5 個 crate，另外 171 個是 pingora 帶進來的——**帶著 pingora 的需求一起進來**。對一個自己沒點名的包，樞衡沒有投票權。

## pingora-core 0.8.1 實際宣告的上界

| pingora 寫的 | 真實含義 | crates.io 最新 | 連帶卡住 |
|---|---|---|---|
| `nix = "~0.24.3"` | `>=0.24.3, <0.25.0` ← ★ **波浪號，連小版本都釘死** | 0.31.3 | `bitflags` 1.x · `memoffset` 0.6 |
| `brotli = "3"` | `<4.0.0` | 8.0.4 | `brotli-decompressor` · `alloc-*` |
| `prometheus = "0.13"` | `<0.14.0` | 0.14.0 | ★ **`protobuf` 鎖在 2.x** |
| `lru = "0.16.3"` | `<0.17.0` | 0.18.2 | ★ **UAF 那條** |
| `rand = "0.8"` | `<0.9.0` | 0.10.2 | `rand_chacha` · `rand_core` |
| `sfv = "0.10.4"` | `<0.11.0` | 0.15.0 | `base64` 0.22 |
| `daggy = "0.8"` | `<0.9.0` | 0.9.0 | `petgraph` 0.7 |
| `strum` / `strum_macros = "0.26.2"` | `<0.27.0` | 0.28.0 | |
| `openssl-probe = "0.1.6"` | `<0.2.0` | 0.2.1 | ★ 僅在 openssl/boringssl 後端，rustls 下**不編譯** |
| `derivative = "2.2.0"` | ★ **非可選**，且已失維 | 無新版 | `syn` 1.0.109 |
| `daemonize = "0.5.0"` | ★ **非可選**，且已失維 | 無新版 | |

★ **上游 main 分支也一樣。** 核過 `cloudflare/pingora` 的 main：`brotli 3` / `rand 0.8` / `strum 0.26` / `daggy 0.8` / `lru 0.16.3` / `derivative` 一個都沒動（只有 `nix` 抬到了 `~0.31.1`，但尚未發版）。

## ★ `[patch.crates-io]` 打不破需求

這一點極易誤判：**被 patch 的 crate 仍然必須滿足依賴方的版本需求**。把 `brotli` patch 成 8 再指望 `^3` 通過是不行的，Cargo 會直接回報錯誤。

**唯一的槓桿是替換 `pingora-core` 本身**（fork 並改它的清單），不是 patch 那些葉子包。

## 44 項的三類分桶

| 類 | 數量 | 說明 |
|---|---|---|
| **pingora 卡住** | ~22 | 上表及其連帶 |
| **中層 crate 卡住** | ~5 | `hashbrown` / `allocator-api2`（via `indexmap`+`lru`）、`getrandom`（via `ahash`+`rand_core`）、`miniz_oxide`（via `flate2`）|
| ★ **目標平台上根本不編譯** | ~17 | `windows-*` 11 個 · `redox_syscall` · `wasi` · `wit-bindgen` · `r-efi` ×2 —— G13 的目標只有 Linux x86_64 / aarch64，**這些只是 Cargo 記錄的完整圖，不進二進位** |

★ **報告陳舊依賴數量時必須分桶。** 「44 個包過時」聽起來嚇人，但其中 17 個在目標平台上一行程式碼都不會被編譯。

# 4 條公告與可達性

OSV.dev 對全部 176 包的查詢結果：

| 公告 | 包 | 類型 | 可達性 |
|---|---|---|---|
| **RUSTSEC-2026-0253** | `lru` 0.16.4 | memory-corruption（UAF）| ★ 當時**不可達**，見下；✅ **現已修掉** |
| **RUSTSEC-2024-0437** | `protobuf` 2.28.0 | DoS（棧溢出）| ★ 當時**不可達**，見下；✅ **現已修掉** |
| RUSTSEC-2025-0069 | `daemonize` 0.5.0 | 失維 | 無 CVE；★ 「追最新」不適用——**沒有更新的版本**。✅ **2026-09-24 已離開依賴圖**（pingora 0.9.0 換成 `daemonix`）|
| RUSTSEC-2024-0388 | `derivative` 2.2.0 | 失維 | ✅ **已修**——換成 `educe`，見上 |

## ★ 兩條真漏洞為什麼按當前執行個體化不可達

**`lru` 的 UAF** 觸發條件是「**儲存的鍵的 `Drop` 實作在 `pop()` 中 panic**」。pingora-pool 執行個體化的是 `Lru<ID, ConnectionMeta>`（`pingora-pool/src/connection.rs`），**鍵是整數類型——沒有 `Drop` impl 可以 panic**。

**`protobuf` 的 DoS** 要求**解析**不可信輸入。`prometheus` 只在 protobuf 曝露格式上走**編碼**路徑，樞衡不解析任何外部 protobuf。

★ ★ **但這兩條判斷依賴 pingora 的內部實作，pingora 改了它們就可能失效。** 所以它們**照樣登記、照樣跟蹤**，只是不構成「今天就危險」。**不要把「當前不可達」記成「已修復」。**

# owner 的處置（G30）——已執行

 拍板並當日執行：**fork `pingora-core` 放寬依賴上界**，而不是接受天花板等上游。

★ **代價已被明確告知並接受**：這直接把 `PLAN.md` §9 那條「Pingora 上游演進與樞衡定製的合併成本」**從一條已登記的風險變成一項進行中的成本**。

## 結果

| 修掉的 | 怎麼修的 |
|---|---|
| ★ **RUSTSEC-2026-0253**（`lru` UAF）| `lru` 0.16.4 → **0.18.2** |
| ★ **RUSTSEC-2024-0437**（`protobuf` DoS）| `prometheus` 0.13 → **0.14**，連帶 `protobuf` 2.28 → **3.7.2** |
| ★ **RUSTSEC-2024-0388**（`derivative` 失維）| 換成 **`educe` 0.7.6**（5 處，1 個檔案）；★ **連帶白賺 `syn 1.0.109` 一起消失**——`derivative` 是全圖唯一還要 `syn ^1` 的包 |

同批抬起來的還有 `nix` 0.24.3 → **0.31.3**、`brotli` 3 → **8**、`rand` 0.8 → **0.10.2**、`sfv` 0.10 → **0.15**、`strum` 0.26 → **0.28**、`daggy` 0.8 → **0.9**（連帶 `petgraph` 0.7 → 0.8）、`openssl-probe` 0.1 → **0.2**、`bitflags`（nix 側）1 → **2**、`thiserror`（prometheus 側）1 → **2**。

**實際改動量：版本上界 12 個呼叫點 / 3 個檔案，加上 `derivative`→`educe` 的 5 處 / 1 個檔案**——比預估的 76 個少得多，因為大部分呼叫點在破壞性升級後**簽名沒變**。

★ ★ **`nix` 那一輪恰好落在 `server/transfer_fd/mod.rs` 上，也就是 M0 要驗的那個模組。** 改法照抄上游 main（它已自行抬到 `~0.31.1`），**但沒有整檔案覆蓋**——上游 main 還帶著與本次無關、且未被 M0 驗證過的行為變更。

**改動後 M0 連跑三次全綠**，兩個自建監聽器都報告了 `INHERITED`。見 [M0 接縫驗證](/verification/m0-seam.md)。

## 破壞面實測

在樞衡實際用到的 **6 個 crate 閉包**內統計（`pingora-core` / `-error` / `-http` / `-pool` / `-runtime` / `-timeout`，共 38,876 行，`src/` 下不含測試與示例）：

| 依賴 | 呼叫點 | 檔案 | 備註 |
|---|---|---|---|
| `nix` 0.24→0.31 | 17 | 5 | 最大的一塊 |
| `daemonize` | 18 | 4 | ★ 失維，需替換或 vendor |
| `brotli` 3→8 | 10 | 2 | 集中在 `protocols/http/compression/` |
| `rand` 0.8→0.10 | 10 | 3 | |
| `lru` 0.16→0.18 | 9 | 3 | ★ 其中 1 個檔案在 s2n 後端，**rustls 下不編譯** |
| `daggy` 0.8→0.9 | 5 | 2 | |
| `derivative` | 2 | 1 | ★ 失維，僅 `upstreams/peer.rs` |
| `prometheus` 0.13→0.14 | 2 | 1 | |
| `sfv` 0.10→0.15 | 2 | 1 | |
| `strum` 0.26→0.28 | 1 | 1 | |
| `openssl-probe` | 1 | 1 | ★ boringssl/openssl 後端，**rustls 下不編譯** |

**合計 ~76 個呼叫點。** ★ 兩個最初看起來最麻煩的（`openssl-probe` 與 `lru` 在 s2n 後端的用法）**在 rustls-only 建置下根本不參與編譯**。

兩個失維包的處置與 fork 的長期維護方式當初登記為 **D12**，⚠ **它已經不在**
[待定清單](/governance/open-questions.md) **裡了**：fork 的長期維護方式由 **G32** 定
（有公告支撐的上界推 PR 給上游，其餘常年 rebase），兩個失維包各自的現狀見本頁上文
（`derivative` 已換成 `educe`；`daemonize` 是權衡後保留的）。

# 怎麼重跑這份快照

依賴檢查的日常入口是 [依賴策略](/platform/dependency-policy.md) 裡的 `tools/dep-check.py`。★ **但它只報告 `cargo update` 能動的東西**——本頁那 44 項它一個都不會報，因為 `cargo update` 對它們是**空操作**。

✅ **已經有指令碼了**（補上，此前是手動做的）：

```bash
python tools/supply-audit.py                       # 只查 lock 層面
python tools/supply-audit.py --cargo "<cargo 命令>" # ★ 帶分桶，見下
python tools/supply-audit.py --cargo "..." --markdown  # 額外輸出可直接貼回本頁的表
```

它做的正是 `dep-check.py` 結構上做不到的那件事：**逐包對比 crates.io 的 `max_stable_version`**（陳舊度）。

★ ★ **公告那一半已並進 `dep-check.py` 的第五項**（結束代碼 `160`），**每週自動掃兩把鎖**（`vendor/pingora/Cargo.lock` + 根鎖），`ACCEPTED` 名單兩邊共用同一份。此前它只在有人記得手動跑的時候才發生，而**預設掃的還是次要的那把鎖**——`h2` 的 RUSTSEC-2026-0258 就是這麼撞見的。本頁的手動深審現在主要用於**陳舊度**與分桶。

★ ★ **一定要傳 `--cargo`**，否則拿不到編譯圖，數字會偏大一倍（30 vs 13）。在這臺機器上：

```bash
python tools/supply-audit.py --cargo "docker run --rm -v D:/Workspace/Fulcrum:/w -v fulcrum-cargo:/usr/local/cargo/registry -v fulcrum-target:/w/target -w /w fulcrum-build:local cargo"
```

**結束代碼**（★ 會疊加，風格同 `dep-check.py`）：

| 碼 | 含義 |
|---|---|
| `0` | 沒有未登記的公告，且查證覆蓋率達標 |
| `20` | ★ 有未登記的公告 |
| `40` | ★ **查證覆蓋率不足**——未能查證的包超過 `--max-unresolved-pct`（預設 10%）|
| `60` | 兩者都有 |
| `1` | 出錯 |

**陳舊包不判紅**——目標建置里長期有十來個升不動的陳舊包，把它算作失敗會讓這道門永遠紅，而永遠亮著的告警等於沒有告警。已登記並接受的公告寫在指令碼的 `ACCEPTED` 裡，**每條都必須帶「為什麼接受」和「出路在哪」**。

★ ★ **`40` 這一檔補的是一個 fail-open 的洞**：`unresolved`（未能查證）此前**隻影響列印、不影響結束代碼**。於是 crates.io 整體不可達時（公司網路、DNS、限流），164 個包會全部落進 `unresolved`，指令碼打一行警告後照常輸出「沒有未登記的安全公告」並 **exit 0**——而每週的 cron / CI 只看結束代碼，會把一次**什麼都沒查到**的執行記成一次通過。

★ 閾值卡的是**比例**不是絕對值：被限流是常態，零星幾個查不到不該攔人；**整體查不到**必須攔。這是「不能讓門永遠紅」與「不能讓門假裝綠」之間的那條線。

## ★ 它第一次跑就糾正了本頁的一處措辭

`openssl-probe` 被分桶為 **「編譯」**，而此前文件寫的是「rustls 下不編譯」。核實後：它是 `pingora-core` 的**非可選**依賴，**crate 本身照樣被編譯**；rustls-only 下不編譯的是它的**唯一呼叫點**（`connectors/tls/boringssl_openssl/mod.rs`，被 `cfg` 擋掉）。

★ **所以分桶其實有四層，不是三層**：

> 「在 lock 裡」→「在編譯圖裡」→「**程式碼被用到**」→「能不能升」

前兩層指令碼能分，第三層要讀 `cfg`。**混用這四層就會得出既嚇人又無意義的數字。**

# 相關

[依賴策略](/platform/dependency-policy.md) · [技術棧](/platform/tech-stack.md) · [待定清單](/governance/open-questions.md) · [決策紀錄](/governance/decision-log.md)
