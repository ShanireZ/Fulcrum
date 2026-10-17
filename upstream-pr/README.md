# upstream-pr —— 給 cloudflare/pingora 的投稿材料

> 依據 [`PLAN.md`](../PLAN.md) §10 **G32**。流程約束見 [`docs/platform/upstream-pr.md`](../docs/platform/upstream-pr.md)。

★ **這個目錄是過渡性的**，和回落層一樣——**改動被上游接受（或明確拒絕）之後就刪掉**。

## 裡面是三份投稿，彼此獨立

**投稿一 · `lru` 版本升級**（G32，2026-08-14 **已發出**，✅ **已完成實作**）

> ✅ ✅ **改動已經在上游 `main` 裡**（2026-09-04 實測，這才是本目錄自己的完成實作判據）：
> [`6463ad6`](https://github.com/cloudflare/pingora/commit/6463ad6)，2026-08-14，
> author `Shanire <shanire86@gmail.com>`、committer 是維護者、`Signed-off-by` 原樣保留。
> ⇒ ★ 「我們投的東西會不會被重放進 `main`」有了**第一個正例**。
> ⏳ 按本目錄開頭那條紀律（「被接受或明確拒絕之後就刪掉」），**這一份材料可以清了** ——
> ⛔ 留給 owner 決定，我沒刪。
> ⚠ 而投稿二與投稿三**尚未完成實作**：它們的主題在 `main` 的提交裡零命中
> （帶正對照：同一條檢索搜得到上面那筆 lru）。
>
> ✅ issue [cloudflare/pingora#961](https://github.com/cloudflare/pingora/issues/961)
> → PR [cloudflare/pingora#962](https://github.com/cloudflare/pingora/pull/962)
>
> ⚠ **發之前重跑門，推翻了 08-13 記的一條**：當時寫「失敗集合逐項相同（116/116）」，
> 實測是 **116 vs 117**——多出來的是 `pingora-memory-cache` 的 `tests::test_eviction`。
> 查證後確認**與本改動無關**（詳見下面「投稿一：發之前查出來的三件事」）。

| 檔案 | 用途 |
|---|---|
| [`issue.md`](issue.md) | GitHub issue 草稿。★ **必須先發它** |
| [`pr.md`](pr.md) | PR 正文草稿。發 PR 時把 `#<ISSUE_NUMBER>` 換成真實編號 |
| `0001-Bump-lru-dependency-from-0.16.3-to-0.18.2.patch` | 可直接 `git am` 的補丁，基於上游 `main`（`0046038`），已帶 `Signed-off-by` |

**投稿二 · `get_fds_from()` 的兩處 fd 洩漏**（G38，2026-08-14 **已發出**，並按複審意見改過一版）

> ✅ issue [cloudflare/pingora#959](https://github.com/cloudflare/pingora/issues/959)
> → PR [cloudflare/pingora#960](https://github.com/cloudflare/pingora/pull/960)
> · ★ 已按 Copilot 的複審意見補一版（`f94e445`，CI 四項全綠）
> · 順帶派生出 issue [#963](https://github.com/cloudflare/pingora/issues/963)
> （2026-08-14 由 owner 授權後代發，見 PLAN.md §10 **G40**）。
>
> ✅ **上游 CI 四項全綠**：`pingora (1.85.0)` 3m55s / `pingora (1.97.1)` 13m42s /
> `pingora (nightly)` 8m19s / `semgrep-oss` 36s；`mergeable = MERGEABLE`，等待評審。
> ★ 本機預跑的那張門表因此**被上游 CI 複核過一遍**——包括本機沒裝、跑不了的
> `cargo machete` 與完整的 `cargo test`（上游 runner 帶 openresty）。
>
> ★ **「PR 被 close」在這裡是成功而不是拒絕**——上游走批量重放，判斷是否完成實作
> 要看改動有沒有出現在 `main`，別看 PR 狀態。

| 檔案 | 用途 |
|---|---|
| [`issue-2-fd-leaks.md`](issue-2-fd-leaks.md) | GitHub issue 草稿。★ **必須先發它** |
| [`pr-2-fd-leaks.md`](pr-2-fd-leaks.md) | PR 正文草稿 |
| `0002-Close-the-transfer-socket-and-set-CLOEXEC-on-received-listener-fds.patch` | 可直接 `git am` 的補丁，基於上游 `main`（`0046038`），已帶 `Signed-off-by` |

**投稿三 · `pingora-rustls` 無謂編進 aws-lc-rs**（G41，2026-08-19 **已發出**）

> ✅ issue [cloudflare/pingora#965](https://github.com/cloudflare/pingora/issues/965)
> → PR [cloudflare/pingora#966](https://github.com/cloudflare/pingora/pull/966)
> （owner 於 2026-08-19 指示「再審核一遍、嚴格照上游規範、查過已修與已有人報之後再代發」）。
>
> `pingora-rustls` 向 `rustls` 與 `tokio-rustls` 要了 `ring` provider，但**兩處都沒寫
> `default-features = false`**，而兩者的 `default` **都含 `aws_lc_rs`**——於是
> **ring 與 aws-lc-rs 兩個 provider 一起編進產物**，而 aws-lc-rs 一行都不會被呼叫
> （本 crate 自己裝的是 ring，另一處密碼學呼叫是 `ring::digest`）。
>
> ★ **代價**：`aws-lc-sys` 是 **69 MB 的 C 原始碼**（ring 8.5 MB）走 cmake 編。
> 對靜態連結 musl 的人是實打實的障礙。

### ★ ★ ★ 發前複審推翻了這份草稿的兩處（2026-08-19，G45）

**一、「查過上游做沒做」只查了程式碼，沒查未合併的 PR。**

08-14 的原話是「全倉也沒有別處關掉過 rustls 的預設」——那句話本身沒錯，但它回答的是
**程式碼裡有沒有**，而 G32 要問的是**有沒有人已經在做**。一搜就撞見兩個 open PR：

| | 標題 | 狀態 |
|---|---|---|
| [#630](https://github.com/cloudflare/pingora/pull/630) | Allow using ring or aws-lc-rs as rustls crypto provider | 2025-05 開，已 conflicts |
| [#887](https://github.com/cloudflare/pingora/pull/887) | Make ring an optional dependency in pingora-rustls | 2026-05 開，clean |

★ **但這不是撤稿理由，反而是本投稿最有價值的一格**：兩個 PR **都只改了 `rustls` 那一行**，
而 aws-lc-rs 是從**兩扇門**進來的。實測（上游 `main` `0046038`，`cargo tree -p pingora-core -e normal` 唯一 crate 數）：

| 清單形態 | crate 數 | aws-lc-rs / aws-lc-sys |
|---|---|---|
| `main` 原樣，`--features rustls` | 178 | 在 |
| #630 對 `rustls` 行的寫法單獨應用 | 178 | **仍在** |
| #887 完整應用，`--features rustls` | 178 | **仍在** |
| ★ #887 完整應用，`--features rustls-no-provider` | 177 | **仍在**（`ring` 沒了）|
| **本投稿（兩扇門都關）** | **176** | **沒了** |

★ ★ 倒數第二行是給 #887 作者的：它成功甩掉了 `ring`，**但一個明確選擇「我自己裝 provider」
的消費者，照樣要編那 69 MB 的 C**——而那正是他想省掉的東西。差的就是 `tokio-rustls` 一行。

★ **這個實驗自證有效**：形態②裡 aws-lc 消失了，說明探針既能命中也能落空。

**二、提交資訊裡的依賴圖數字是 fork 的，不是上游的。**

正文寫「178 → 176」（上游），而 `git format-patch` 的提交資訊裡寫的是「175 → 173」——
那是 **fork 的數字**（fork 抬過版本上界，解析出來的圖不一樣）。
★ **提交資訊要進上游的歷史，掛著一個審閱者重現不出來的數字**，比不寫更糟。已改為 178 → 176。

> ★ ★ ★ **可帶走的一條**：同一份材料裡，**面向內部的數字與面向外部的數字必須分開記賬**。
> 這兩個數各自都是對的，錯的是它們出現在了對方的文件裡——而兩邊都寫著「實測」。

| 檔案 | 用途 |
|---|---|
| [`issue-3-aws-lc-rs.md`](issue-3-aws-lc-rs.md) | GitHub issue 草稿。★ **必須先發它** |
| [`pr-3-aws-lc-rs.md`](pr-3-aws-lc-rs.md) | PR 正文草稿 |
| `0003-Do-not-compile-aws-lc-rs-when-the-ring-provider-is-requested.patch` | 可直接 `git am` 的補丁，基於上游 `main`（`0046038`），已帶 `Signed-off-by` |

### ★ 投稿三的驗證（2026-08-19 **重跑**，容器內，基於上游 main `0046038`）

★ **全部逐項重跑過**，不是照抄 fork 側、也不是沿用 08-14 那份——**PR 裡引用的每個數都要審閱者能在上游重現**。
命令一律取自上游 [`.github/workflows/build.yml`](https://github.com/cloudflare/pingora/blob/main/.github/workflows/build.yml)。

| 門 | 結果 |
|---|---|
| `git am` | ✅ 乾淨應用；★ **回到 `0046038` 重放後與分支樹逐位元組一致** |
| `cargo tree -p pingora-core --features rustls -e normal` | ★ **178 → 176**；消失的**恰好**是 `aws-lc-rs` 與 `aws-lc-sys`，**且沒有任何新增** |
| `cargo fmt --all -- --check` | ✅ 前後皆過 |
| `cargo check --workspace` | ✅ 前後皆過 |
| `cargo build -p pingora-core --features rustls` | ✅ 前後皆過 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ 前後皆過 |
| `cargo test -p pingora-core --lib --no-fail-fast --features rustls` | **566 / 7 / 2**，前後**逐項相同**（雙向差集兩側皆空）|
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ 前後皆過，MSRV 不受影響 |
| `cargo audit` / `cargo machete` | 未跑（本機沒裝），留給上游 CI |

⚠ **那 7 條失敗全是環境性的，且與 08-14 那次的「1 條」不矛盾**：這一次容器**沒裝**
`iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`，於是 G42 查清的那批連接超時測試多紅 6 條
（Docker 預設網路替 `192.0.2.1` 應答）；剩下那條仍是 `test_bind_to_port_range_on_connect`。
★ **判據是「前後集合相同」，它不依賴環境**——但**數字依賴**，所以數字旁邊必須寫清環境。

★ **發之前按 G32 重查了三件事**（2026-08-19）：

1. **上游修了沒有**——`main` 仍停在 `0046038`（2026-08-07 起未動），
   `pingora-rustls/Cargo.toml` 那兩行一字未改（克隆下來實測，不是讀網頁）。
2. **有沒有人已經報過**——搜 `aws-lc-sys` / `prefer-post-quantum` **零命中**；
   搜 `aws-lc` / `default-features` / `crypto provider` 命中的是上面那兩個 PR
   與已結案的 [#446](https://github.com/cloudflare/pingora/issues/446)（另一個症狀：
   provider 沒顯式指定導致 panic，已由 `install_default_crypto_provider()` 解決）。
3. **上游規範**——`.github/CONTRIBUTING.md` 要求非 trivial 的 PR 先開 issue（已照做）；
   issue 正文按 `.github/ISSUE_TEMPLATE/bug_report.md` 的小標題重排過；
   ★ 上游近 30 個提交裡 `Signed-off-by` **零出現**，但保留它與前兩份投稿一致且無害。

★ **三份各自開 issue、各自開 PR，不要合併**：一條是純版本號升級、一條是行為修復、
一條是清單 feature 修正，立論與驗證方式完全不同，合起來只會讓幾邊都更難審。

### ★ 投稿二比投稿一更有說服力，理由值得記

投稿一是「版本號升級 + 引用一條公告」；投稿二帶著**可重現的缺陷、實測資料、以及一個
能分別抓住兩處缺陷的迴歸測試**。G32 當初定「只推 lru 一條」的顧慮是「純版本號的改動
一起推容易把有說服力的那條拖黃」——投稿二不屬於那一類，所以 G38 單獨推它。

## ★ 投稿二的驗證（2026-08-14，容器內，基於上游 main `0046038`）

| 門 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo check --workspace`（1.97.1）| ✅ |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ MSRV 不受影響 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ |
| `cargo test -p pingora-core --lib --no-fail-fast` | 544 passed / 2 failed —— **與未改動的 main 逐項相同**（`connectors::l4` 那兩條，環境性），雙向差集為空 |
| `cargo audit` | 不涉及（沒動任何依賴）|
| `cargo machete` | 未跑（本機沒裝）；本改動不動任何清單 |

★ ★ **新測試的反證已分別做過**（一個只見過綠的測試與一個沒在測的測試無法區分）：

| 撤掉哪一半 | 測試報什麼 |
|---|---|
| `MSG_CMSG_CLOEXEC` | `fd received over SCM_RIGHTS is missing FD_CLOEXEC` |
| `OwnedFd` 那處接管 | `the accepted transfer socket was left open` |

兩邊**分別**紅，說明它確實在測兩件事，而不是碰巧一起綠。

## ★ 先查上游做沒做，再動手

投稿二動手前核對過（2026-08-14）：上游 `main` 的同一函式**仍是 `MsgFlags::empty()`**，
accept 出來的連接**仍然沒人關**，且儲存庫裡**沒有任何 issue/PR 提過 CLOEXEC**。
★ 這一步是 G32 留下的紀律——fork 裡的 `prometheus` 與 `nix` 兩條就是沒查上游而白改的。

## ★ 投稿一：發之前查出來的三件事（2026-08-14 重審）

**① 「失敗集合逐項相同」是錯的。** 08-13 記的是 116/116，重跑實測 **116 vs 117**。
多出來的 `tests::test_eviction` 經兩條獨立證據判定與 lru 無關：

- **基線也抖**：單獨重複跑那個測試二進位 20 次，基線紅 1 次、打補丁紅 1 次，**同一比率**；
- **結構上夠不到**：`pingora-memory-cache` 不依賴 `lru`，它走 `TinyUFO`，
  而 `TinyUFO` 的 `src/` 一處都沒用 `lru`——`lru` 是它的 **dev-dependency**，只給 benchmark 用。

★ 兩條證據缺一不可：只有「基線也抖」會被質疑成巧合，只有「夠不到」會被質疑成沒跑過。

**② caveat 裡有一處推理是錯的（結論碰巧對）。** 原文用
`cargo tree -i lru@0.16.4 --workspace` 無匹配來論證「不在預設 feature 圖裡」——
而**普通解析下壓根沒有 0.16.4**（是 0.12.5），所以那條命令是因為**錯的原因**才無匹配。
改成控住變數的 2×2 實測表，並寫明真正的機制：`aws-sdk-s3` 是 `pingora-runtime` 的**可選**依賴，
只由非預設的 `dial9-worker-s3` feature 開啟。

**③ 標題向上遊慣例靠攏。** 上游那些 `RUSTSEC-…:` 開頭的 issue **全是 `github-actions[bot]` 開的**
（帶 `dependencies` 標籤）；人寫的依賴 issue 是「Update X …」那種形狀（如 #875）。
★ 順帶查實：47 條 `dependencies` issue 裡**沒有** lru 這一條，機器人三個月來一次都沒為它開過——
這條寫進了 issue 正文，解釋為什麼由人來報。

## ★ 投稿二收到的複審，以及由它牽出的兩條

發出後 GitHub 的 Copilot 複審留了一條行內意見：**新加的迴歸測試自己沒有關掉收到的 fd**。

**採納了。** 理由不是「reviewer 說了」，而是：上游同一檔案裡另外兩個測試
（`test_send_receive_fds` / `test_serde_via_socket`）確實也不關——但**那不構成在一個
「主題就是 fd 洩漏」的 PR 裡繼續不關的理由**。改動已 amend 進原提交（`f94e445`），
CI 四項仍全綠。

### ★ ★ 差點把一條有效意見駁回去

採納後 `close_unclaimed_tests::closes_only_unclaimed_fds` 紅了一次，第一反應是「這條建議引入了
flakiness」。**5 次樣本就下結論是錯的**，做了三設定對照（每檔 20 次）：

| 設定 | 該測試失敗次數 |
|---|---|
| 上游未改動 | 5/20 |
| 本 PR（未採納建議） | 2/20 |
| 本 PR（已採納建議） | 5/20 |
| ★ **全新 pristine 上游克隆** | **7/20** |

**它在完全未改動的上游上就已經這樣。** → 已單獨報為
[#963](https://github.com/cloudflare/pingora/issues/963)（按上游 `bug_report.md` 範本寫，
資料取自 pristine 克隆而非我打過補丁的樹）。

根因：那個測試關掉 fd 之後斷言這個**號**已失效（`fcntl(fd, F_GETFD) == -1` / `EBADF`），
而 fd 號是處理程序級、關掉即被重複使用——同一二進位裡任何並行測試在那個視窗開一個 fd 就會拿走它。

### ★ 同一輪裡還糾正了投稿一的一句話

投稿一（`lru`）正文原本寫「那條 flaky 的 `test_eviction` 我沒查，happy to file it separately」。
一搜才發現**上游早有 [#591](https://github.com/cloudflare/pingora/issues/591) 報過、
[#740](https://github.com/cloudflare/pingora/pull/740) 提了修復**（用 `force_put` 繞過
TinyLFU 存取資格）。已改成引用這兩條。★ **「我可以幫你報」在別人早就報過時，等於宣告自己沒搜。**

## ★ 順序不能反

上游 `CONTRIBUTING.md` 寫著 **"Non-trivial PRs will also require a GitHub issue"**，而依賴大版本升級不在它列舉的
trivial 清單（錯別字／小重構／文件）裡。**先發 issue，等回應，再發 PR 並引用 issue 號。**

## 怎麼用

```bash
# 1. 在 GitHub 上 fork cloudflare/pingora，然後
git clone git@github.com:<你的账号>/pingora.git && cd pingora
git remote add upstream https://github.com/cloudflare/pingora.git
git fetch upstream main && git checkout -b lru-0.18.2 upstream/main

# 2. 應用補丁（兩份投稿各自一條分支，別混在一起）
git am /path/to/Fulcrum/upstream-pr/0001-Bump-lru-dependency-from-0.16.3-to-0.18.2.patch
#   或
git am /path/to/Fulcrum/upstream-pr/0002-Close-the-transfer-socket-and-set-CLOEXEC-on-received-listener-fds.patch

# 3. 先發 issue（issue.md 的內容），拿到編號後再 push + 開 PR（pr.md 的內容）
git push origin lru-0.18.2
```

## ★ 已經跑過的驗證（2026-08-13，容器內，基於上游 main `0046038`）

| 門 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo check --workspace`（1.97.1） | ✅ 零原始碼改動 |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ |
| `cargo machete` | ✅ |
| `cargo test --lib --bins --tests --no-fail-fast` | 116 失敗，**與未改動的 main 逐項相同**（缺 openresty，雙向差集為空）|
| `cargo audit` | ★ **不變**——見下 |

## ★ ★ 兩條必須誠實寫進投稿的事

1. **這一行不會讓 `cargo audit` 那條消失。** `aws-sdk-s3` 獨立要求受影響版本的 `lru`（`--ignore-rust-version` 解析下是
   0.16.4，普通解析下是 0.12.5）。它不在預設 feature 圖裡，但在 lock 裡，而 audit 掃的是 lock。**這條不歸上游這個儲存庫管。**
   改動的真實價值是「**pingora 自己的五個 crate 離開了受影響版本**」，不是「修好了公告」。

2. **上游的 CI 現在不是紅的。** RustSec 把這條歸為 `unsound`（warning）而非 vulnerability，裸 `cargo audit` 不因 warning 失敗。
   ★ **不要把立論寫成「你們 CI 紅了」——那是假的，寫進去會當場失去可信度。**

## ★ 一條實驗設計的教訓

第一輪跑門時 MSRV 1.85.0 那檔報紅（`s2n-tls requires rustc 1.91`），我一度以為是 `lru` 引起的迴歸。
做 2×2 對稱實驗後確認：**紅完全由鎖定檔生成方式決定，與 `lru` 無關**——我拿 `cargo audit` 步驟用的
`--ignore-rust-version` 鎖定檔去跑 `cargo check`，而上游 CI 的 MSRV 檔**不預先生成 lock**。

| | 普通 lock | `--ignore-rust-version` |
|---|---|---|
| 基線 | ✅ rc=0 | ✗ rc=101 |
| lru 0.18.2 | ✅ **rc=0** | ✗ rc=101 |

★ **對照實驗要把變數控住**，否則會把自己的實驗汙染當成被測物件的缺陷報上去。

## ✅ 投稿四（2026-08-20 **已發出**）· `test_connect_uds` 的短讀

> ✅ issue [cloudflare/pingora#967](https://github.com/cloudflare/pingora/issues/967)
> → PR [cloudflare/pingora#968](https://github.com/cloudflare/pingora/pull/968)

- 立論：[`issue-4-short-read.md`](issue-4-short-read.md)
- 補丁：[`0004-Use-read_exact-in-test_connect_uds.patch`](0004-Use-read_exact-in-test_connect_uds.patch)
  （★ 2026-08-20 **重新生成**：原先那份不是 `git format-patch` 的輸出——沒有 `From` 頭、
  沒有 `Signed-off-by`、hunk 頭只有一個裸 `@@`，**`git am` 根本吃不下**。
  現已在 `0046038` 上重播驗證過，且與分支樹逐位元組一致。）
- fork 側已經改掉了（見 [`../vendor/pingora/FORK.md`](../vendor/pingora/FORK.md) §7）

### ★ ★ 上游 CI：三綠一紅，而**那一紅與本改動無關**（已在 PR 裡說明）

| 檢查 | 結果 |
|---|---|
| `pingora (1.85.0)` | ✅ 3m50s |
| `pingora (nightly)` | ✅ 8m39s |
| `semgrep-oss` | ✅ 22s |
| `pingora (1.97.1)` | ❌ 13m21s —— **`cargo audit` 那一步** |

紅的原因是 **RUSTSEC-2026-0258**（`h2` 0.3.27，"unbounded empty DATA frames"，
**2026-08-17 公佈**，評級是 vulnerability 而非 warning，所以 `cargo audit` 退出非零）。

★ ★ **判它「與本改動無關」不能靠推理，要拿兩條獨立證據**：

1. **上游 `main` 最後一次 CI 是 2026-08-07**（`0046038`），**早於公告發布日**。
   `cargo audit` 是**跑的時候**去拉公告庫的 ⇒ **那次綠說明不了今天**。
2. **投稿三的 PR [#966](https://github.com/cloudflare/pingora/pull/966)**（比本份早開、
   只動 `pingora-rustls/Cargo.toml`）**在同一個 job、同一個公告 ID 上同樣紅**。

而本改動只動一行**測試程式碼**、不碰任何清單或鎖定檔，結構上夠不到那一步。
⚠ 已在 PR 裡留言把這三點說清楚——**一個紅勾擺在那裡沒人解釋，審閱者第一反應就是「你把 CI 搞紅了」**。

★ **Copilot 的自動複審（2026-08-20 01:52，`COMMENTED`）：無可執行意見**——
只給了一段準確的改動摘要，**零行內評論、未要求改動**。
⚠ 與投稿二那次不同（那次它留了一條「新測試自己沒關掉收到的 fd」，我們**採納了**），
這一次沒有要處置的東西。記在這裡是為了**下一個人不必再去點開看一遍**。

> ★ ★ 順帶一條給我們自己的：**`cargo audit` 這類「跑的時候才去拉資料庫」的門，
> 它的綠是有保質期的**。上游 `main` 今天看著是綠的，只是因為它從 08-07 起就沒再跑過。
> 同一件事在本儲存庫對應的是 `dep-check.py` 的第 160 檔——它每次都真去查，所以不會有這種假綠。

★ **它與前三份的證據形狀不同，值得單獨說**：前三份是「實測到一個洩漏／一個多編的 crate」，
這一份是**一條間歇性失敗**。而間歇性失敗的證據不能是「又跑了幾次沒見到」——
所以立論裡給的是**一個確定性的擾動**：把 mock server 的一次 `write_all` 拆成
「1 位元組 + 20ms + 8 位元組」（流式 socket 的合法行為），**修前 3/3 必紅、修後 5/5 必綠**。

### ★ ★ ★ 發之前複審推翻了草稿裡的一句話

草稿把它寫成「一條會間歇性打紅建置的 flaky 測試」。**實測下來那句話是錯的**：
這個測試整塊在 `#[cfg(feature = "any_tls")]` 裡，而 `pingora-core` 與 `pingora` 的
`default` **都是 `[]`** ⇒ 上游 CI 跑的 `cargo test --workspace --lib --bins --tests`
**根本不編譯它**（實測 `-- --list | grep -c test_connect_uds` → **0**）。
★ 所以立論改成「任何帶 TLS backend 跑測試的人會撞上」。
⚠ 與投稿一那條教訓同形：**把立論寫成一件審閱者一查就知道是假的事，會當場失去可信度。**

### ★ ★ 「失敗集合前後相同」第一次跑出來是**不相同**的

基線 7 條、打補丁後 6 條，差的是 `test_conn_timeout_with_offload`。
**沒有把它解釋成「環境性的」了事，而是把環境修對**：容器裡沒裝
`iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`，於是那批拿 RFC 5737 位址當黑洞的
連接超時測試被 Docker 的網路替它應答而恆紅且抖。裝上之後兩側都是 **572/1/2**，差集為空。
> ★ 這正是 G41/G42 當初查過的同一件事——**結論沒被寫進跑上游門的流程裡，於是又踩了一遍**。
> 已補進下面的「怎麼用」。

## ✅ 投稿六（**issue 與 PR 都已發，2026-09-04**）· 監聽器上的連接生命週期

> ✅ PR [cloudflare/pingora#995](https://github.com/cloudflare/pingora/pull/995)
> （base `main` · head `ShanireZ:listener-tracer` · 一個檔案 `+135/-1` · 一個提交 `0dda18b`）。
> 正文草稿留檔在 [`pr-6-listener-tracer.md`](pr-6-listener-tracer.md)，發後逐行核過。
>
> ✅ **上游 CI 四項全綠**：`pingora (1.85.0)` 3m39s · `(1.97.1)` 10m58s · `(nightly)` 8m7s ·
> `semgrep-oss` 33s。★★★ 它補上了本機驗證的兩處缺口 —— **MSRV** 那道本機沒有工具鏈，
> 而 `1.97.1` 跑的是**帶 openresty 的完整套件**（本機那 99 條整合失敗是環境造成的，就此坐實）。
> ⏳ 等回話。★ 判斷是否完成實作看**改動有沒有出現在 `main`**，⛔ 不看 PR 狀態。

> ✅ issue [cloudflare/pingora#994](https://github.com/cloudflare/pingora/issues/994)
> （經 owner 逐次授權，G40）。發後核過：歸一行尾後與批准的那份**逐字元相等**。
> ⚠ ⚠ ⚠ **發出去之後自己查出一處錯，已公開更正**（owner 單獨批准）：
> [issue-comment-5536326108](https://github.com/cloudflare/pingora/issues/994#issuecomment-5536326108)。
> `Stream` 上本來就有 `pub tracer`，且 `Stream::drop` 會調 `on_disconnected()` ——
> **機制是通的，缺的只是監聽器那側沒人賦值**。⇒ 要求因此**變小**：不要新方法，
> 只要監聽器側能掛 `Tracer`。全文見材料開頭那一節。
>
> ✅ **補丁已備好、⛔ 未發**：
> [`0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch`](0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch)
> —— 基於 `09696b5`，只改一個檔案，`fmt` / `clippy -D warnings` 均 `RC=0`，
> `git am` 重播乾淨且樹逐位元組一致，**失敗集合與基線逐項相同**（多出兩條通過的新測試）。
> ⛔ 它是在**乾淨的上游克隆**上做的，`vendor/pingora/` 一個位元組沒動。驗證表見材料 §2.2。

對應 fork **改動 15**。依據 G122：「投不投**等 rebase 讀過上游 `main` 之後再判**」。
立論與已查清的部分：[`issue-6-connection-counter.md`](issue-6-connection-counter.md)。

⚠ ⚠ **本輪查出兩件把這份材料的形狀改掉的事**（2026-09-03，克隆實測，`main` = `09696b5`）：

1. ★★★ **G122 裡那句「上游 `main` 已把 `prometheus` 整條刪掉」在今天的 `main` 上不成立** ——
   `pingora-prometheus/src/lib.rs` 還在（131 行），`pingora` 與 `pingora-proxy` 都依賴它。
   ⚠ 而那句話是「投不投」這個判斷的**全部理由**（「口味未知」）⇒ **已登記給 owner**，
   ⛔ 我沒有替它改結論。
2. ★★★ **上游已經有半個同位置的接縫**：`ConnectionFilter::should_accept`
   （`listeners/l4.rs:472`，每條連接一次，TCP accept 之後、TLS 握手之前）——
   **`+1` 的位置逐字相同**，⛔ 而「連接結束」與「哪個監聽位址」兩樣它都給不出，
   偏偏那兩樣正是做連接計數必須的。⇒ 立論形狀從「加一個新接縫」變成「把已有的補完」。

✅ **形狀已定（owner 2026-09-04 拍板：形狀 ②）** —— 給已有的 `ConnectionFilter` 補上
「連接結束」鉤子與「監聽位址」，⛔ **不提新接縫**（提了就正面撞上上游 2026-08-25
那句「#671 已經加了連接級過濾器」）。**issue 正文草稿在
[`issue-6-connection-counter.md`](issue-6-connection-counter.md) §5 與其後的「正文」一節**，
⛔ **未發** —— 發要 owner 按 G40 單獨授權。

⛔ **仍然沒有生成 `.patch`**：基線已從 `0046038` 移到 `09696b5`，而本目錄的紀律是
**先發 issue、等回應、再發 PR** —— 補丁的形狀取決於維護者對「掛在 `ConnectionFilter` 上
還是別處」的回話。一份基線過期、形狀可能作廢的補丁擺在這裡比沒有更糟。
✅ **G46 那一項已補上（2026-09-04）**，結果在 [`issue-6-connection-counter.md`](issue-6-connection-counter.md) §3.1：
⛔ **不構成否決**（沒有 open 的 issue/PR 在做這件事本身，與投稿五那次的 #632 完全不同），
★★★ **但立論必須改寫** —— 上游 2026-08-25 剛以「#671 已加連接級過濾器」為由把
[#118](https://github.com/cloudflare/pingora/issues/118)（「on connect 階段」）判成 `COMPLETED`。
★ 同時需求側的證據變強了：#118 / #295 / #337 三個互不相干的使用者兩年裡各要過一次，三條都沒拿到實作。
⚠ ⚠ 方法上踩到一條：**`gh search issues` 隱含 `is:issue`，一條 PR 都不回傳** ——
只跑那一條通道會答出「沒人做過」而其實一個 PR 都沒看過。

## ⏳ 投稿七（材料已備，⏳ 未發，owner 2026-10-08 拍「先備材料、暫不發」）· `read_request` 每請求白做的兩件事

> 對應 fork **改動 16**（PLAN **G157**）的 ② ③（C1 / C2）。① （B2，l4 流不再記等待計時）**不投** ——
> 上游有讀者，在上游不是語義不變，見下「B2 為什麼不投」。
> ⛔ 沒開 issue、沒開 PR、沒 push、沒 fork，GitHub 上零寫入（查重唯讀、沒評論）。
> 補丁做在 scratchpad 裡的**乾淨上游克隆**上，`vendor/pingora/` 一個位元組沒動。
>
> 上游 `main` = **`4487f7b`**（committer date 2026-09-11，「Abort tls offload tasks when dropped」）。
> 三處程式碼在它上面與 fork 改動前（`0c232f7^`）**逐位元組相同**（`diff --strip-trailing-cr`，三個檔案皆空），
> `702f690..4487f7b` 沒有提交碰過 `l4/stream.rs` / `v1/server.rs` / `v1/common.rs` —— 讀的是克隆下來的程式碼，不是網頁。

| 檔案 | 用途 |
|---|---|
| [`issue-7-read-request.md`](issue-7-read-request.md) | issue 草稿（`feature_request.md` 範本的四個小標題）。★ **必須先發它** |
| [`pr-7-read-request.md`](pr-7-read-request.md) | PR 正文草稿；發前換 `#<ISSUE_NUMBER>` |
| [`0007-Parse-request-headers-into-uninitialized-header-slots.patch`](0007-Parse-request-headers-into-uninitialized-header-slots.patch) | C1（`[PATCH 1/2]`）：改走 `parse_with_uninit_headers`；迴歸測試 `tests_stream::read_max_headers` |
| [`0008-Keep-request-header-refs-on-the-stack-instead-of-in-a-Vec.patch`](0008-Keep-request-header-refs-on-the-stack-instead-of-in-a-Vec.patch) | C2（`[PATCH 2/2]`）：`populate_header_refs` + 棧上槽位；兩條單測 + `tests/read_request_allocations.rs` |

兩份都是 `git format-patch` 原樣輸出：author 與 `Signed-off-by` 都是 `Shanire <shanire86@gmail.com>`，0 個 CR。

### 怎麼切：一份 issue、一份 PR、兩個提交（⛔ 沒有「投稿八」）

- **合成一份**：C1 與 C2 在同一個函式里，立論相同（每條請求白做的功），驗證形狀也相同 ——
  與前幾份「立論與驗證方式完全不同 ⇒ 各自開」的情形相反。拆成兩份 PR 反而會在同一函式上互相排隊。
- **但分兩個提交，且各自都能單獨完成實作**：0008 不依賴 0007，單獨 `git am` 到 `main` 上乾淨應用、門全過
  （下表「C1 單獨 / C2 單獨」兩行）。上游走**批量重放、按提交取**，維護者可以只拿一個。
- **為什麼不壓成一個提交**：風險形狀不同 —— C1 在 pingora 裡**零 `unsafe`**（只是換 httparse 的入口），
  C2 新加**一個 `unsafe` 塊**（把已寫的前綴當 `&[KVRef]`）。維護者若對後者有意見，不該拖住前者。
- **為什麼仍要先開 issue**：CONTRIBUTING 的免 issue 清單是錯別字、小重構、文件；
  新加 `unsafe` + 新加一個帶 `#[global_allocator]` 的測試二進位，不算 trivial。
- B2 不在裡面（見下），所以沒有第三個提交。

### ★ ★ B2 為什麼不投：上游有讀者（⛔ 沒做刪除補丁）

fork 側「8 個 crate 裡沒有讀者」**在 fork 裡仍然成立**（樞衡不 vendor `pingora-proxy`），但上游 22 個 crate 裡有：

| 位置（上游 `4487f7b`） | 是什麼 |
|---|---|
| `pingora-core/src/protocols/digest.rs:235-244` | 公開 trait `GetTimingDigest` 的 `get_read_pending_time` / `get_write_pending_time`（預設回 0）；`protocols/mod.rs:94` 把它並進 `IO` 約束 ⇒ 任何持有 `Stream`（`Box<dyn IO>`）的使用者都能調。自 **0.3.0** 起公開（`ea1db2f`，2024-05-21；CHANGELOG：「Add the API to track socket read and write pending time」）|
| `pingora-core/src/protocols/l4/stream.rs:677-683` | l4 `Stream` 的實作；TLS 包裝層逐層轉發：`tls/boringssl_openssl/client.rs:97-102` · `tls/rustls/stream.rs:118-123` · `tls/s2n/stream.rs:270-275` |
| ★ **`pingora-proxy/src/proxy_h1.rs:213`、`:242-244`** | **樹內讀者**：代理 H1 上游時，在 `proxy_1to1` 前後各讀一次**上游連接**的 `get_write_pending_time()`，差值寫進 `Session` |
| `pingora-proxy/src/lib.rs:619-620`、`:945-947` | 公開 getter `Session::upstream_write_pending_time()`；由 `900ec23`（2026-01-21，Cloudflare 的作者，「Add upstream_write_pending_time to Session for upload diagnostics」）加入 |

⇒ 在上游刪掉計時 = 上面那個公開 getter 與 trait 方法**恆為 0**：是行為變更，⛔ 不能按「語義不變」投。

★ 順帶核清一處，影響取捨：上游 `AccumulatedDuration` **只在 Pending↔Ready 轉換時讀鍾**
（`start()` 只在第一次 Pending 時讀，`stop()` 只在隨後 Ready 時讀；立刻就緒的讀寫一次鍾都不讀）。
fork 側量到的 `clock_gettime −2/请求` 與此一致（keep-alive 上每條請求的那次讀先 Pending 一次）。
⚠ `vendor/pingora/FORK.md` 改動 16 ① 那格寫「每次各讀一兩次時鐘」，措辭偏寬 —— ⛔ 本次範圍外，我沒改，交 owner。

**可選做法（owner 定）**：

| 做法 | 改什麼 | 取捨 |
|---|---|---|
| **A. 不投**（我的推薦）| 無 | fork 那 4 處刪除照舊隨 rebase 重放（守衛測試會在被合掉時變紅）。上游 8 個月前剛為它加了讀者，說明他們在用這份資料；省下的只是每次掛起→就緒的兩次 vDSO 讀鍾 |
| B. 執行期開關，預設開 | 例如 `Stream::set_track_pending_time(bool)`，關掉時不讀鍾、getter 回 0 | 對現有使用者語義不變；但為很小的收益加公開 API。樞衡拿到的是 `ServerSession` 裡的 `Box<dyn IO>`，要用還得在 accept 處串接（形狀類似投稿六的監聽器 `Tracer`）⇒ 改面不小 |
| C. cargo feature 關掉 | `no-pending-time` 之類 | ⛔ 不推薦：「關能力」的 feature 違反 feature 可加性 —— 同一依賴圖裡的 `pingora-proxy` 會靜默拿到 0 |
| D. 只關讀側 | 讀側沒有樹內讀者 | 仍是公開 trait 方法的行為變更，只省一半 |

若選 B：按本目錄紀律**先開 issue 問要不要**，⛔ 別直接做補丁。

### G46 查重（2026-10-08，唯讀）

**方法**：`gh search issues` 與 `gh search prs` **兩條通道**（不加 `--state` ⇒ 開著的與關了的都在），22 個查詢串；
另把上游 **124 個 open PR 的改動檔案**逐個過一遍（不靠關鍵詞）；再在克隆裡用 `git log -S` 查歷史。

| 查什麼 | 結果 |
|---|---|
| `parse_with_uninit_headers` · `uninit headers` · `uninit_headers` · `MaybeUninit header` · `populate_headers` · `KVRef` · `header_refs` · `read_pending_time` · `write_pending_time` · `pending time` · `AccumulatedDuration` · `upstream_write_pending_time` · `header allocation` · `read_request allocation` · `EMPTY_HEADER` · `httparse allocation` · `httparse performance` · `clock_gettime` · `Instant::now overhead` · `per-request allocation` | **兩條通道全 0** |
| `MAX_HEADERS` | issue [#993](https://github.com/cloudflare/pingora/issues/993)（open）· PR [#1000](https://github.com/cloudflare/pingora/pull/1000)（open）· #83（無關，hyper 升級）|
| `read_request` | #993 / #1000；其餘無關：#447（超時）、#844 / #969（shutdown Notify，已進 `main`）、#876（pipelining，已進 `main`）|
| 動同幾個檔案的 open PR | 6 個：#1000（下）· [#677](https://github.com/cloudflare/pingora/pull/677)（改 `for header in header_refs` 循環裡那行註解）· #956 / #732（`once_cell`→`LazyLock`，別處）· #902（SNI，別處）· #971（l4 虛擬流池化，只與 B2 相關）|
| 上游歷史 | 從未用過 `parse_with_uninit_headers`；`[EMPTY_HEADER; MAX_HEADERS]` 與 `populate_headers` 自 0.1.0 未動 ⇒ **沒有「試過又退回」** |
| ★ 正對照（同一指令碼同一通道）| `listener Tracer` / `connection lifetime` → PR **#995**；`CLOEXEC` → issue **#959**、PR **#960** |

⇒ **零命中「有人在做同一件事」**。⚠ 但 **#1000 改的正是 C1 那兩行**（它要把頭數上限做成可配：
`Request::new(&mut headers[..max_headers])`）。實測（`git merge-tree`，先扣掉 #1000 與 `main` 本來就有的
`apps/mod.rs` 衝突）：本投稿只多出**那兩行**一處衝突，一行可解；**C2 與它不衝突**。
#1000 至今**沒有維護者評審**（只有 issue 作者的 5 條 COMMENTED），最後活動 2026-09-25。已在 issue / PR 草稿裡主動寫明。
★ #677 那一處是設計時就避開的：C2 讓 `header_refs` 變成 `&[KVRef]`，**循環那一行一字不改** ⇒ 實測不衝突。
⚠ 方法上又見一次：`connection lifetime` 在 issue 通道是 0、PR 通道才命中 —— **兩條通道都要跑**。

### 驗證（2026-10-08，容器內，⛔ Rust 不在宿主機跑）

**環境**：鏡像 `upstream-g157-rust:1.97.1` = 官方 `rust:1.97.1@sha256:b1b3c9c0d921d7fa0a6d1f9ec7e4eab87f8c8ec97644c3d791450f131dec813f`
（Debian 13，rustc 1.97.1）+ 上游 `build.yml` 那組 `cmake libclang-dev` + `iptables` + `rustup toolchain install 1.85.0 --profile minimal`；
容器 `--cap-add NET_ADMIN` 後先 `iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`；`CARGO_BUILD_JOBS=4`；
宿主 Windows 11 + Docker Desktop 29.8.2。兩側共用**同一份 `Cargo.lock`**（cargo 1.97.1 生成，httparse 解析到 1.10.1）。
兩側共用一個 target 卷，所以**每個編譯步驟先在容器裡 touch `pingora-core`**，這些紀錄裡都有 `Compiling`/`Checking pingora-core`。
「打補丁後」用的是**拿兩份 `.patch` `git am` 出來的重播樹**（與開發分支樹逐位元組一致）—— 測的就是要交出去的東西。

| 門（命令取自上游 `build.yml`） | `main` `4487f7b` | 打補丁後 |
|---|---|---|
| `git am` 0007 + 0008 | — | ✅ 乾淨；重播樹 == 分支樹；★ 0008 單獨 `git am` 到 `main` 也乾淨 |
| `cargo fmt --all -- --check` | ✅ rc=0 | ✅ rc=0（⚠ 第一次 rc=1：我的測試裡一行沒按 rustfmt 折行，已改進 C1、重新生成補丁後重跑）|
| `cargo check --workspace` | ✅ rc=0 | ✅ rc=0 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ rc=0 | ✅ rc=0；warning 與基線逐行相同（只有 `pingora-foundations` 那條 clippy.toml MSRV 提示）|
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ rc=0 | ✅ rc=0（lock 由 cargo 1.85.0 現生成，與 1.97.1 那份逐位元組相同）|
| `cargo test -p pingora-core --lib --tests --no-fail-fast` | 574 passed / 3 failed / 2 ignored | **579 / 3 / 2**；失敗集合**雙向差集為空**，多出的 5 條全是新測試，沒有一條「改前過、改後不過」|
| 同上，**C1 單獨** / **C2 單獨** | — | 575 / 3 / 2 · 578 / 3 / 2；失敗集合同上（clippy 也各自 rc=0）|
| `cargo test --doc -p pingora-core` | ✅ 1 passed / 10 ignored | ✅ 同 |
| `cargo test --verbose --lib --bins --tests --no-fail-fast`（CI 原命令，全 workspace）| 1155 / 129 / 6 | **1160 / 129 / 6**；失敗集合**按測試名**比雙向差集為空，多出的 5 條全是新測試。失敗的 4 個 target 兩側相同：`pingora-core --lib`（上面那 3 條）· `pingora-load-balancing --lib`（`health_check::test::test_tcp_check`）· `pingora-proxy` 的 `test_basic`（18）/ `test_upstream`（107）—— 後兩者要 openresty |
| `cargo check -p pingora-core --features patched_http1` | ✗ rc=101，唯一錯誤 E0599 `parse_unchecked` 不存在 | ✗ 同一個錯誤、只此一個 ⇒ 補丁裡 patched 那條路的其餘部分過了類型檢查（借用檢查沒走到）|
| `cargo audit` / `cargo machete` / nightly 檔 / openresty 整合測試 | 未跑 | 未跑（本機沒裝 audit/machete、沒裝 openresty）|

那 3 條兩側都紅：`connectors::l4::tests::test_bind_to_port_range_on_connect`（老面孔，宿主機相關）與
`http::v2::server` 的 `test_req_conflicting_content_length_rejected`、`test_req_malformed_stream_budget_exhausted`
（這次新見，⛔ 沒查根因；判它與本改動無關靠的是**基線同樣紅**，且它們走 h2、不經過 `v1::server::read_request`）。
✅ **根因 2026-10-16 查到了：`h2` 0.4.20**（10-06 發佈，自己在 codec 裡拒掉值不同的重複 `content-length`）。
在 `4487f7b` 上實測：h2 0.4.20 兩條紅、換 0.4.19 兩條綠。⚠ 投稿七那次的鎖沒留下來，沒直接看到它解析到 0.4.20；
但它是 10-08 現生成的鎖、上游要求 `h2 >=0.4.16` ⇒ 只能是 0.4.20。修法見投稿九。

⚠ 方法上踩到一條：容器的 stdout 與 stderr 經 `docker run` 分兩路轉出，cargo 的 `Running <binary>`（stderr）
會與測試輸出（stdout）**錯序** ⇒ 按「哪個二進位」歸屬測試名會張冠李戴（全 workspace 那次一度算出 3 條假的
「改前過、改後不過」）。★ 判據改為**只按測試名（多重集）比**，上表都是這樣比的。

**探針讀數**（一次性測試檔案，⛔ 不在補丁裡；兩側同一鏡像、同一 lock）：

| | `main` | 打補丁後 |
|---|---|---|
| `read_request` 分配次數，3 個頭（熱身後連讀 5 次）| **9**（5/5）| **8**（5/5）|
| 同，0 個頭 | 4 | 4（空 `Vec` 本來就不分配 ⇒ 符合預期）|
| `size_of_val(&read_request())`（future 大小）| 216 | **216**（兩個陣列不跨 `.await`，沒進 future）|
| `size_of::<httparse::Header>()` / `size_of::<KVRef>()` | 32 / 32 | — |

### 注入反證（都在副本 `pingora-inject` 上，每次都有 `Compiling pingora-core`；判據：與「打補丁後」的失敗集合比）

| 注入 | 新增的紅 |
|---|---|
| I1 · C1 給 httparse **少一個槽位** | 只紅 `read_max_headers`（256 個頭被 `TooManyHeaders` 拒）|
| I2 · C1 的程式碼**退回 `main` 原樣**，測試留著 | **全綠** ⇒ 它守的是**等價**，不是優化本身。C1 省的是棧上的寫，不是分配，沒有測試能抓「被退回」—— PR 裡照實寫了 |
| I3 · C2 的呼叫處**退回 `Vec`**，測試留著 | 只紅 `read_request_allocation_budget`：「made 9 allocations, more than 8」|
| I4 · C2 的棧上槽位**少一個** | 只紅 `read_max_headers`（`index out of bounds: the len is 255 but the index is 255`）|
| I5 · `populate_header_refs` **不再跳過空名** | 只紅那兩條 helper 單測（`attempt to subtract with overflow`）|

### ★ 數字的口徑

⛔ fork 側診斷臺的讀數（使用者指令 −4.7%、malloc −1 等）**一個都沒進** issue / PR。進去的數都能在上游重現：
`8 KiB` = `256 × size_of::<httparse::Header>()`；分配 9 → 8 由補丁自帶的測試給出（退回 C2 它就報 9）；
future 216 不變是探針讀數，審閱者要自己寫一行 `size_of_val` 才能重現 —— 所以 PR 裡寫的是「不變」並給了數。

★ **計時沒進 PR，理由是實測太抖**：上游樹內就有 criterion 基準 `pingora-core/benches/h1_pipelining.rs`
（`h1_single_request` 與 `h1_pipelining/bodyless/*` 正好走 `read_request`），在同一容器裡做了
兩側**交替跑了 6 段、得到 3 組對比**（`--save-baseline` / `--baseline` 交替，每段都先 touch `pingora-core`）：

| 對比 | `bodyless/1` | `/16` | `/64` | `/256` | `/1024` | `h1_single_request` |
|---|---|---|---|---|---|---|
| 補丁 vs main（第 1 對）| −12.1% | −16.7% | −14.0% | −20.8% | −24.8% | −24.0%（CI −41.9%…−8.5%）|
| main vs 補丁（反向）| +12.1% | +25.7% | +24.6% | +19.5% | +15.3% | 無顯著（p=0.82）|
| 補丁 vs main（第 2 對）| −5.2% | −20.3% | −24.5% | 噪聲內 | ⚠ **+10.8%** | 無顯著（p=0.18）|

方向大體一致，⚠ 但第 2 對的 `/1024` 反了號，`h1_single_request` 的置信區間寬到沒意義；
這臺宿主同時跑著別的容器（BetaPass 的 Playwright、WenTian 的 PG/Redis）。
⇒ ⛔ 這些百分比**不寫進 issue / PR**；PR 裡只提一句「樹內這個 bench 走的就是這條路徑，我機器太吵、沒引數」，
把重現方法交給審閱者。

### 發前要重查

1. **上游 `main` 動沒動**：`git fetch` 後重新 `git am` 兩份補丁。若 `read_request` 一帶有人改過，
   **重量分配次數**（`MAX_ALLOCATIONS = 8` 是對 `4487f7b` 量的，⛔ 別為了綠直接改數）並重跑整張門表，
   再改 PR 正文裡的 574 / 579 等數。
2. **#1000**：若已合，先 rebase（那一行改成 `parse_req_buffer(&mut req, &buf, &mut headers[..max_headers])`）；
   `read_max_headers` 測的是預設上限，仍成立。若還開著，草稿裡那句「誰後落誰 rebase」不變。
3. **#677**：仍應不衝突；重跑 `git merge-tree` 確認。
4. **G46 重跑**：同一指令碼（兩條通道）+ open PR 檔案維度掃一遍；帶正對照。
5. 外部寫入（都要 owner 按 G40 單獨授權）：發 issue → 拿號 → 推 `ShanireZ/pingora` 分支 → 開 PR。
6. 若 owner 要 B2：走上表方案 B，**先開 issue 問**。

**留下的本機資源**（⛔ 沒刪，留給 owner 決定）：鏡像 `rust:1.97.1`（拉取）、`upstream-g157-rust:1.97.1`（自建，約 3.9 GB）；
卷 `upstream-g157-cargo`（約 425 MB）/ `upstream-g157-target`（約 14 GB）/ `upstream-g157-target-msrv`（約 1 GB）；⛔ 沒碰任何 `fulcrum-*` 卷。
⚠ 2026-10-16 這臺開發機上這些鏡像與卷都已不在（10-15 Docker 狀態被本工作階段之外的東西清過）。

## ⏳ 投稿八（材料已備，⏳ 未發，owner 2026-10-16 拍「投稿材料先備不發」）· 寫 h1 回應頭時兩次白分配

> 對應 fork **改動 19**（PLAN **G165**）的 ① ②，兩處一起投。
> ⛔ 沒開 issue、沒開 PR、沒 push、沒 fork，GitHub 上零寫入（查重唯讀、沒評論）。
> 補丁做在 scratchpad 裡的**乾淨上游克隆**上，`vendor/pingora/` 一個位元組沒動。
>
> 上游 `main` = **`4487f7b`**（2026-10-16 克隆時仍是它，與投稿七同一個基底）。
> `prepare_response_header` 整個函式與 proxy task 那處呼叫，在它上面與 fork 改動前（`9b43f01`）**逐行相同**。

| 檔案 | 用途 |
|---|---|
| [`issue-8-write-response-header.md`](issue-8-write-response-header.md) | issue 草稿（`feature_request.md` 範本的四個小標題）。★ **必須先發它** |
| [`pr-8-write-response-header.md`](pr-8-write-response-header.md) | PR 正文草稿；發前換 `#<ISSUE_NUMBER>` |
| [`0009-Avoid-two-allocations-when-writing-an-HTTP-1-response-header.patch`](0009-Avoid-two-allocations-when-writing-an-HTTP-1-response-header.patch) | 一個提交：兩處改動 + 新測試二進位 `tests/write_response_header_allocations.rs`（分配上限 1 · 續與不續的位元組 · 分配器自證）|

`git format-patch` 原樣輸出：author 與 `Signed-off-by` 都是 `Shanire <shanire86@gmail.com>`，0 個 CR；`git apply --check` 對 `4487f7b` 通過。

### 怎麼切：一份 issue、一份 PR、一個提交

- **不拆**：兩處在同一個函式里、立論相同（每條回應白做的分配）、風險形狀相同（都零 `unsafe`、都改私有函式或常數），驗證也是同一個測試 —— 與投稿七「C2 新加 `unsafe`」那種值得分開的情形不同。
- **仍先開 issue**：CONTRIBUTING 的免 issue 清單是錯別字、小重構、文件；新加一個帶 `#[global_allocator]` 的測試二進位不算 trivial。

### G46 查重（2026-10-16，唯讀）

**方法**：`gh search issues` 與 `gh search prs` **兩條通道**（不加 `--state`），12 個查詢串；
另把上游 **123 個 open PR** 裡改到 `pingora-core/src/protocols/http/v1/server.rs` 的 5 個拉下來逐個看 diff，並用 `git merge-tree` 試合。

| 查什麼 | 結果 |
|---|---|
| `make these lazy static` · `prepare_response_header` · `write_response_header allocation` · `response header allocation` · `connection_value` · `freeze write_buf` · `BytesMut freeze` · `Connection header keep-alive static` · `per-response allocation` · `lazy static connection` | **兩條通道全 0** |
| `HeaderValue::from_static` | issue #229（已關，壓縮時的 `Accept-Ranges`，無關）|
| `write_response_header` | 8 個 issue（#852 / #381 / #1031 等，都是階段或語義問題，無關）· PR #289（已關，`write_response_header_ref` 繞過下游模組，無關）|
| 動同一個檔案的 open PR | #1000 · #956 · #902 · #732 · #677：diff 裡沒有一行碰 `connection_value` / `freeze` / `prepare_response_header` / `send_header_task`；`git merge-tree` 下本補丁**一處新衝突都不添**（它們與 `main` 本來各有 1 / 5 / 3 / 15 / 1 個衝突檔案，疊上本補丁後數目與名單不變）|
| ★ 正對照（同一指令碼同一通道）| `listener Tracer` → PR **#995**；`CLOEXEC` → issue **#959** |

⇒ **零命中「有人在做同一件事」**。

### 驗證（2026-10-16，容器內，⛔ Rust 不在宿主機跑）· ★ 整張上游 CI 矩陣重放過

**環境**：自建鏡像 `upstream-g165-rust:1.97.1` = 官方 `rust:1.97.1@sha256:b1b3c9c0…`（與投稿七同一個 digest）+ 上游 `build.yml` 那組 `cmake libclang-dev`
+ **OpenResty 1.27.1.2**（照 CI 的 sha256 核過、原始碼編，`pingora-proxy` 的整合測試要它）+ `rustup` 裝 `nightly`（當日 1.101.0-nightly `4ebd25825`）與 `1.85.0`
+ `cargo install --locked cargo-audit`（0.22.2）與 `cargo-machete 0.7.0`；`--cap-add NET_ADMIN` 後先 `iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`。
兩棵樹：`main` `4487f7b` 與「打補丁後」= 補丁 `git am` 到 `main` 上（`0262f87`，樹與分支逐位元組一致）；★ 都在容器裡用 `git archive` 現展開（見下「方法上踩到兩條」②）。
每個（工具鏈 × 樹）一份新展開的副本、**一個獨立的 `CARGO_TARGET_DIR`**，原始碼全部 touch，每一格都核了 `Compiling` / `Checking pingora-core (/work/…)`。
鎖定檔按 CI 的做法各自現生成（CI 不帶鎖）：六份 sha256 都是 `cb9ccc6d…`（bytes 1.12.1 · http 1.5.0 · httparse 1.10.1，與 fork 相同）。

| 工具鏈 · 門（命令照 `build.yml` 原樣）| `main` `4487f7b` | 打補丁後 |
|---|---|---|
| 1.97.1 · `cargo fmt --all -- --check` | ✅ | ✅ |
| 1.97.1 · `cargo check --workspace` | ✅ | ✅ |
| 1.97.1 · `cargo test --verbose --lib --bins --tests --no-fail-fast` | 1280 passed / 4 failed / 6 ignored | **1283 / 4 / 6**；失敗集合按測試名相同，多出的 3 條全是新測試 |
| 1.97.1 · `cargo test --verbose --doc` | 7 / 0 / 20 | 7 / 0 / 20 |
| 1.97.1 · `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ rc=0（3 條 warning，都是 `pingora-foundations` 的「clippy.toml 與 Cargo.toml 的 MSRV 不同」）| ✅ 同（逐行相同）|
| 1.97.1 · `cargo generate-lockfile --ignore-rust-version && cargo audit` | ✗ rc=1：2 條公告 + 4 條 warning | ✗ 同（逐條相同）—— 與補丁無關，見下 |
| 1.97.1 · `cargo machete` | ✅ | ✅ |
| nightly · fmt | ✗ 只差 `pingora-cache/src/key.rs` 一處 | ✗ 同一處（補丁的檔案都沒被點名）|
| nightly · check / test / doc test | ✅ / 1280 / 4 / 6 / 7 / 0 / 20 | ✅ / **1283 / 4 / 6** / 7 / 0 / 20；失敗集合相同 |
| 1.85.0 · fmt / `cargo check --workspace --exclude pingora-foundations` | ✅ / ✅ | ✅ / ✅ |
| 新測試檔案放到**沒打補丁的 `main`** 上（另跑）| 預算那條紅：「made 3 allocations, more than 1」；位元組那條綠（位元組沒變）| — |

兩側都紅的 4 條，與投稿七全 workspace 那次是同幾條：`connectors::l4::tests::test_bind_to_port_range_on_connect`、
`http::v2::server` 的 `test_req_conflicting_content_length_rejected` / `test_req_malformed_stream_budget_exhausted`（走 h2、不經過本函式；✅ 根因是 h2 0.4.20 —— 本次兩份現生成的鎖都解析到它，見投稿九）、
`pingora-load-balancing` 的 `health_check::test::test_tcp_check`。★ `pingora-proxy` 的 `test_basic` / `test_upstream` 這次**全過**（投稿七那次 125 條全紅，原因見下 ②）——
其中真查 `Connection` 值的 `test_connect_close`（keep-alive 與 close 兩處）· `test_duplex`（keep-alive）· `test_close_on_response_before_downstream_finish`（close）在打補丁的樹上都過了。
`cargo audit` 那 2 條公告是 `rustls` 0.21.12 的 RUSTSEC-2026-0346 / -0349（2026-10-14 發佈，修復版 ≥ 0.23.46），4 條 warning 是 `derivative` · `paste` · `rustls-pemfile` · `unsafe-libyaml` 失維 —— 依賴圖兩側相同，本補丁一個依賴都沒動。

**注入反證**（副本上，每次都有 `Compiling pingora-core`）：只退回 `Connection` 那處 ⇒ 只紅預算那條「made 2 allocations」；只退回 `freeze` 那處 ⇒ 同樣只紅它、同樣 2。

**補丁 v2**：重放之後把測試裡一句註解的範圍改準（原句「現有測試幾乎都關了 `update_resp_headers`」沒提 `pingora-proxy` 那幾條要 OpenResty 的整合測試）。
新舊兩份補丁按行比，只差那一句註解與 diffstat；v2 重新 `git apply --check` / `git am`（樹與分支一致）、1.97.1 的 fmt 與新測試二進位 3 條都過。⛔ 整張矩陣沒對 v2 重跑（註解不進編譯產物）。

⚠ ⚠ **方法上踩到兩條（都已改正）**：
① 幾棵樹共用一個 `CARGO_TARGET_DIR` 時，cargo 對 workspace 裡的 path 包按**相對路徑**算中繼資料 ⇒ `cp -a` 出來的副本（保留了舊 mtime）被當成「已經編過」、直接用上一棵樹的產物。
第一趟「新測試放到沒打補丁的 `main` 上」因此**跑的其實是打補丁的庫**、預算那條是綠的（編譯 1.45 s、沒重編庫就是破綻）；同一趟 MSRV 對重播樹那次也是假的（0.85 s、沒有 `Checking pingora-core`）。
⇒ 改成每格獨立目標目錄 + 全部 touch + 逐格核 `Compiling` / `Checking`。
② 上游儲存庫裡有兩個**符號連結**（`pingora-proxy/tests/utils/conf/origin/conf/keys` · `pingora/tests/keys`），在 Windows 上克隆（`core.symlinks=false`）就成了普通檔案 ⇒ OpenResty 讀不到證書起不來，
`pingora-proxy` 兩個整合測試二進位 125 條兩側全紅（「mock origin (openresty) failed to start within 10s」，之後全是「Lazy instance has previously been poisoned」）。投稿七那次沒裝 OpenResty（「後兩者要 openresty」）；就算裝了，它的克隆若也是在 Windows 上做的，也會卡在這一條。
⇒ 改成在容器裡 `git archive | tar -x` 展開（符號連結原樣），先手動起一次 mock origin 確認 `:8000` 回 200，再跑矩陣。

### ★ 數字的口徑

⛔ fork 側診斷臺的計數（每請求 malloc 18 → 16、使用者指令 −1.9% 等）**一個都沒進** issue / PR。進去的數都能在上游重現：
3 → 1 次分配由補丁自帶的測試給出（退回任一處它就報 2），4 KiB 是 `INIT_HEADER_BUF_SIZE`。計時沒測、也沒寫。

### 發前要重查

1. **上游 `main` 動沒動**：`git fetch` 後重新 `git am`。若 `prepare_response_header` 一帶有人改過，**重量分配次數**（上限 1 是對 `4487f7b` 量的），並整張矩陣重跑、再改 PR 正文裡的 1280 / 1283 等數。
2. **G46 重跑**：同一組查詢（兩條通道）+ open PR 檔案維度掃一遍 + `merge-tree`；帶正對照。
3. 外部寫入（都要 owner 按 G40 單獨授權）：發 issue → 拿號 → 推 `ShanireZ/pingora` 分支 → 開 PR。

**重做環境**：鏡像的 Dockerfile 與矩陣指令碼留在 `handoff/spike-http-ceiling/g165/upstream-ci/`（不入庫）；克隆必須在 Linux 裡展開（上面 ②）。
本機資源 2026-10-16 owner 說「不用就刪」⇒ 卷 `upstream-g165-cargo` / `upstream-g165-target` 與鏡像 `upstream-g165-rust:1.97.1` 都已刪；⛔ 沒碰任何 `fulcrum-*` 卷。

## ⏳ 投稿九（材料已備，⏳ 未發，owner 2026-10-16 拍「改準材料＋備投稿九」）· 兩條 h2 測試在 h2 0.4.20 下紅

> 對應 fork **改動 20**（`vendor/pingora/FORK.md`）。⛔ 沒開 issue、沒開 PR、沒 push、沒 fork，GitHub 上零寫入（查重唯讀、沒評論）。
> 補丁做在 scratchpad 裡的**乾淨上游克隆**上（`git clone` 自投稿八那棵 `4487f7b`，`core.autocrlf=false`），`vendor/pingora/` 一個位元組沒動。
> ★ **它最好先於投稿七、八發**：在它完成實作之前，`main` 上任何新 PR 的 CI 都會紅在這兩條上（投稿七、八的 PR 草稿已補一句指向它）。

| 檔案 | 用途 |
|---|---|
| [`issue-9-h2-content-length-tests.md`](issue-9-h2-content-length-tests.md) | issue 草稿（`bug_report.md` 的小標題）。★ **必須先發它** |
| [`pr-9-h2-content-length-tests.md`](pr-9-h2-content-length-tests.md) | PR 正文草稿；發前換 `#<ISSUE_NUMBER>` |
| [`0010-Adapt-two-h2-server-tests-to-h2-0.4.20.patch`](0010-Adapt-two-h2-server-tests-to-h2-0.4.20.patch) | 一個提交：兩條測試改斷言（第二條改名）+ workspace `h2 = ">=0.4.20"`。author 與 `Signed-off-by` 都是 `Shanire <shanire86@gmail.com>`，0 個 CR，`git apply --check` 對 `4487f7b` 通過 |

與 fork 改動 20 的差別只有兩處：註解裡不提樞衡；補丁多抬了 workspace 的 `h2` 下界（兩條測試從此依賴 0.4.20 的行為，寫進清單才不靠鎖碰運氣）。⛔ fork 那邊沒抬，理由是不為它再多一處 fork 改動、鎖已釘 0.4.20。

### G46 查重（2026-10-16，唯讀）

`gh search issues` 與 `gh search prs` 兩條通道，查詢串：`h2 0.4.20` · `conflicting content-length` · `duplicate content-length` ·
`test_req_conflicting_content_length_rejected` · `test_req_malformed_stream_budget_exhausted` · `MAX_MALFORMED_STREAMS_PER_CONN` · `malformed stream budget`。

| 結果 | |
|---|---|
| issue 通道 | **全 0** |
| PR 通道 | 只命中 **#816**（open，「early request body buffering」，與本題無關的功能 PR）。★ 命中的是它作者 **2026-10-06 17:05 UTC 的一條評論**：同樣兩條、同樣「0.4.20 紅 / 0.4.19 綠」、預言 `main` 下次跑也會紅 |
| ★ 正對照（同一指令碼同一通道）| `listener Tracer` → PR **#995** |

⇒ **有人報過、沒人在修**：沒有 issue、沒有修它的 PR、`main` 自 09-11 未動。issue 與 PR 草稿都引用了那條評論，⛔ 不把它說成我們先發現的。

### 驗證（2026-10-16，容器內 `fulcrum-build:local`，rustc 1.98.0；⛔ Rust 不在宿主機跑）

鎖按 CI 的做法現生成（`cargo generate-lockfile`，解析到 h2 0.4.20）。

| 門 | `main` `4487f7b` | 打補丁後 |
|---|---|---|
| `cargo test -p pingora-core --lib -- protocols::http::v2::server::test::test_req_`（h2 0.4.20）| ✗ 那兩條紅（1860:9「malformed request must not surface as a session」· 1917:25「connection ended before malformed budget was exhausted」）| — |
| 同上，`--precise 0.4.19` | ✅ 4/4 | ⛔ 解析失敗：新下界不許 0.4.19（這正是要的）|
| `cargo test -p pingora-core --lib -- protocols::http::v2::server`（h2 0.4.20）| — | ✅ 14/14 |
| `cargo fmt --all -- --check` | — | ✅ |
| `cargo clippy -p pingora-core --all-targets -- --allow=unknown-lints --deny=warnings` | — | ✅ |

**反證**：改過的兩條測試在 h2 0.4.19 上兩條都紅（「must not reach Pingora's acceptance checks」·「was charged to Pingora's budget」）——
⚠ 這一格是在 **fork 的副本**上量的（測試程式碼與補丁逐行相同、只差註解），上游樹上被新下界擋住、跑不到 0.4.19。

⛔ **沒做的**（與投稿八那張整矩陣相比）：全 workspace 測試、doc test、nightly / 1.85.0（MSRV）、`cargo audit` / `machete`、OpenResty 整合測試。
補丁只動測試與一行版本下界，MSRV 風險看不出來，但沒實測。

### 發前要重查

1. 上游 `main` 動沒動、那兩條測試有沒有人先改了（#816 那條評論下面有沒有後續）。
2. G46 重跑（兩條通道、帶正對照）。
3. 外部寫入（都要 owner 按 G40 單獨授權）：發 issue → 拿號 → 推 `ShanireZ/pingora` 分支 → 開 PR；再回頭把投稿七、八草稿裡的 `#<H2_ISSUE_NUMBER>` 換掉。

## ❌ 投稿五（**已撤銷，不發**）· rustls 監聽器用不上自定義證書解析器

> owner 2026-08-20 拍板 **「什麼都不做」**：不開 issue、不開 PR、也不去別人的執行緒留言。

**理由不是立論錯了，是這件事上游從 2025-04 就有人報了——同一主題已有 9 條 issue/PR。**
尤其 [#632](https://github.com/cloudflare/pingora/pull/632) **與我們的做法逐點相同、
而且更完整**（它順手把 `build()` 的 panic 改成了 `Result`），已有 3 個獨立使用者證實可用；
它卡住的原因是**合併衝突 + 維護者評審頻寬**，不是設計分歧
（[#908](https://github.com/cloudflare/pingora/pull/908) 那條線從 6 月催到 8 月）。
⇒ 再開第 7 份只會讓佇列更堵。逐條證據見 [`issue-5-cert-resolver.md`](issue-5-cert-resolver.md)。

⚠ ★ **fork 裡那條 `with_cert_resolver` 照舊留著，但歸零條件變了**：
不再是「等我們的投稿被接受」，而是**等 #632 或 #908 完成實作**。
