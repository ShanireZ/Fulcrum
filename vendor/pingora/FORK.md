# vendor/pingora —— 樞衡對 Pingora 的 fork

> 基線：`cloudflare/pingora` **tag `0.9.0`**（commit `702f690`，Apache-2.0）
> 建立：2026-08-12（基線 `0.8.1` / `719ef6c`），依據 [`PLAN.md`](../../PLAN.md) §10 **G30**
> ★ rebase：**2026-09-24 從 0.8.1 換到 0.9.0**（owner 當場批）—— 這次撤掉了什麼、重做了什麼，見下面「★ 2026-09-24：rebase 到 0.9.0」
> 上游：https://github.com/cloudflare/pingora

## 為什麼有這個 fork

`pingora-core 0.8.1` 的清單裡有十條版本上界，把 44 個傳遞相依釘在舊版上，其中兩條帶真實安全公告。

★ **成因是 Cargo 的預設行為**：`brotli = "3"` 不是「≥3」，而是脫字號需求 `^3` = `>=3.0.0, <4.0.0`——**每一個不帶符號的版本字串都自帶一個上界**。

★ ★ **`[patch.crates-io]` 打不破需求**——被 patch 的 crate 仍須滿足相依方的版本需求，所以沒法直接把 `brotli` patch 成 8。**唯一的槓桿是替換 `pingora-core` 本身**，這就是這個 fork 存在的理由。

完整背景見 [`docs/platform/supply-chain.md`](../../docs/platform/supply-chain.md)。

## 範圍

★ **只保留樞衡實際用到的 8 個 crate**，不是整個上游 workspace（0.9.0 是 22 個成員；0.8.1 是 20 個 —— 此處原寫的「21」不準）：

```
pingora-core  pingora-error  pingora-http  pingora-pool
pingora-runtime  pingora-timeout  pingora-rustls  pingora-boringssl
```

⚠ ★ ★ **第 8 個是 2026-08-25 加回來的（G104），而它此前是被特意刪掉的。**
刪它的理由寫在 `pingora-core/Cargo.toml` 裡：「`PLAN.md` §5.1 第 1 條把 TLS 後端鎖死在
rustls，三者永遠不會被啟用」——**而 §5.1 第 1 條已被 G104 推翻**（§10 第 59 輪，
本專案第一次推翻一條寫著「不可回頭」的約束）。詳見下面的**改動 11**。
★ `pingora-openssl` 與 `pingora-s2n` 仍然沒有 vendor，它們的 `#[cfg]` 分支照舊不會被點亮。

~~`pingora-rustls` 目前未被啟用，先留著——G6 要求的 `features = ["rustls"]` 在 M1 會用到。~~
（M1 起它一直是被啟用的那一條；⏳ G104 之後它會退到只服務 L4 的 ClientHello 預讀，
最終去留見 `PLAN.md` §10 第 61 輪。）

⚠ ★ ★ ★ **2026-08-25（G104 第 ② 處）更新：`pingora-rustls` 已經退出根 workspace 的相依圖。**
L4 的 ClientHello 預讀換到 BoringSSL 早回呼之後，產品 crate 裡**沒有任何一個**再相依它，
根 `Cargo.toml` 的 `[patch]` 與 `[workspace.dependencies]` 兩處宣告都已刪掉
（實測：根 `Cargo.lock` 少了 `pingora-rustls` 與 `no_debug` 兩個包）。
★ **但這個 crate 本身仍然留在 fork 裡，仍是 vendor workspace 的成員，迴歸網照跑** ——
理由不是留戀：`pingora-core` 的 `rustls` feature 是**上游原樣**的可選分支，
刪掉這個 crate 就等於對上游多改一處，而**留著它一行改動都不用做**。
⇒ **「範圍」仍然是 8 個 crate**，變的是「其中幾個進產品相依圖」（現在是 7 個）。
> ★ 順帶一條方法論：**「沒人用了」與「該刪了」是兩件事。**
> 前者說的是相依圖，後者說的是 rebase 成本 —— 而這裡兩者指向相反的方向。

## ★ 2026-09-24：rebase 到 0.9.0

**做法**：8 個 crate 整體換成官方 tag `0.9.0`（`git -c core.autocrlf=false archive` ——
⚠ 本機全域 `core.autocrlf=true`，不加 `-c` 會匯出 CRLF，每個檔案都像被改過），
再按本檔案的節號**逐條**重做仍需要的改動（沒有走三方合併：被上游吸收的改動會以衝突的形式回來，
而 `offload.rs` 被上游挪了位置，補丁跟不過去）。

| 節 | 結果 | 依據 |
|---|---|---|
| §1 `lru` / `prometheus` / `nix` 上界 | ✅ 歸零 | 0.9.0：`lru = "0.18.2"`（owner 的提交 `6463ad6`）· prometheus 從 pingora-core 整條刪掉（`842ddd9`）· `nix = "~0.31.1"`（`7e29246`）|
| §1 `brotli` / `rand` / `sfv` / `x509-parser` 上界 | 重做 | 0.9.0 仍是 `3` / `0.8` / `0.10.4` / `0.16.0` |
| §2 `nix` 呼叫點 | ✅ 歸零 | 上游同一次遷移（`7e29246`），逐條一致 |
| §2 `rand` / `sfv` 呼叫點 | 重做 | ⚠ `connectors/offload.rs` 被上游挪成 `pingora-core/src/offload.rs`（`8aeef34`），在新位置重做 |
| §3 `derivative` → `educe` | 重做 | 0.9.0 仍用 derivative 2.2.0 |
| §4 fd 衛生（① accept 出來的連接不關 · ② 沒有 CLOEXEC）| ✅ 歸零 | 上游 `b2b35fd`（owner 的 PR #960），並自帶測試 `test_receive_does_not_leak_fds` |
| §5 ① rustls / tokio-rustls 關 default · ③ pingora-core 的 dev-dep | ✅ 歸零 | 上游 `5396a1f`（owner 的 PR #966）|
| §5 ② `rustls-native-certs` 0.8.4 | 重做 | 0.9.0 仍是 `0.7.1` |
| §6 遷走 `rustls-pemfile` + 18 條特徵化測試 | 重做 | 三方合併零衝突 |
| §7 `test_connect_uds` 的 `read_exact` | ✅ 歸零 | 上游 `91b880d`（owner 的 PR #968）|
| §11 `pingora-boringssl` 去掉 `pq-experimental` | ✅ 歸零 | 0.9.0 已是 `boring = "5"` 且不帶特性（boring 5 裡 `pq-experimental` 是空特性）；quiche 0.30 放寬到 `>=4.19, <6` ⇒ 圖裡只有一個 boring 5.2.0 |
| §12 PROXY protocol「收」 | **重做，換了形狀（B）** | 上游新增 `PreTlsProcess`，但只在 TLS 分支裡調（`600c5c0` 有意收窄）⇒ fork 只剩「把呼叫挪到 TLS 分支之前」幾行；讀取循環搬進 `crates/fulcrum-server/src/proxyproto.rs`（owner 2026-09-24 拍板）|
| §13 h1 `body_bytes_sent` | ✅ 歸零 | 上游 `e7de90a`，語義與我們一致，還自帶更強的測試 `body_bytes_sent_excludes_response_header` |
| §14 `SslDigest` 的 `sni` / `alpn` | 重做 | 兩個檔案三方合併零衝突 |
| §15 連接計數接縫 | 重做 | ⚠ 0.9.0 新增建構點 `add_listener()`（`add_tcp` / `add_uds` / `add_tls_with_settings` 都走它）⇒ 那裡也複製了 `connection_counter`；⛔ 填 `None` 能編過，但之後加的連接埠會靜默不計數 |
| **新增：裁掉 dial9** | 新 | 刪 pingora-core 的 `dial9*` 三個特性與 pingora-runtime 的兩個可選相依（`dial9-tokio-telemetry`、`aws-sdk-s3`）。vendor 鎖會記下成員 crate 的**全部**可選相依，而 aws-sdk-s3 的預設特性拉進 aws-lc-rs ⇒ `crates/fulcrum/tests/supply_gates.rs` 門 1 紅。★ 反證實測：保留 dial9 時 vendor 鎖多出 aws-lc-rs / aws-lc-sys / aws-sdk-s3 等，387 → 544 個包。★ 刪掉的特性名（連同第 4 步刪的 openssl / s2n）凡原始碼裡仍有 `cfg` 引用的，登記進 workspace 清單 `[workspace.lints.rust]` 的 `check-cfg`，`pingora-core` 接上 `[lints] workspace = true` —— 不登記時每次編譯 58 條 `unexpected_cfgs` 警告（實測：`LINT_ONLY` 58 → 0；`--features s2n` 仍報「沒有這個特性」）|

**結果**：fork 相對官方 0.9.0 改動 **14 個檔案**（相對 0.8.1 時是 19 個），清單見文末「怎麼核對」。
**判據**：vendor 迴歸網的失敗集合與官方 0.9.0 在同款容器裡逐項相同（官方 0.9.0：19 個二進位 735 過 / 1 敗，
敗的只有宿主機相關那條 ⇒ `tests/vendor/run.sh` 的 `EXPECTED_FAILURES` 清空）；fork 多出的 20 條是本 fork 自己的測試
（§6 的 18 條 + §15 的 2 條）。

**0.9.0 帶來的、本 fork 之外的變化**（產品側已適配或接受）：`ListenFds` 換成 parking_lot 鎖（產品三處 + spike 三處
fd 表取用改成短持鎖）· 新一代關掉未認領的繼承 fd（`f82478a`；產品與 spike 的自建服務都實作了 `listen_addresses()`，
`tests/m0/unclaimed.sh` 反寫成「修好了」）· 上游空閒連接池改成全域 LRU（反代 `Connector` 顯式給 `128 × threads_l7`）·
boring 5 預設開後量子金鑰交換 · quiche 0.30 · HTTP/1 / HTTP/2 入口若干新拒絕 · h2 入口預設上限收緊到每連接 100 條流 /
頭部表 64 KiB（`90890bb`；樞衡沒覆寫 `h2_options()`，owner 2026-09-24 拍了接受）· 停機時 h2 已收到的流會處理完（`aece993`）
（後三條見 `docs/architecture/data-path.md`，後量子那條見 `docs/architecture/tls.md`）。

## 改了什麼

★ **改動的意圖嚴格限定為「放寬版本上界 + 隨之而來的呼叫點適配」。**
（這句是建 fork 時的口徑；此後由 owner 逐條拍進來的另外幾類 —— 加能力（含 `G163` 那個預設不變的開關）、缺陷修復、`G157` 的語義不變的效能改動、`G162` 改了線上頭名大小寫的效能改動 —— 各自寫在本節對應的條目裡。）

⚠ ★ ★ **但「沒有行為變更」這句話，2026-08-12 建立本 fork 時寫下來是錯的**：`nix` 那一輪實際引入了一條真迴歸（見下面第 2 節），它在 2026-08-13 補上回歸網的當天被逮到並修好。
**教訓不是「當時不夠小心」，是「當時沒有能發現它的東西」**——編譯過、M0 七跑全綠，而那條 bug 一直在。

### 1. 版本上界（`Cargo.toml` × 3）

> ★ 2026-09-24（0.9.0）：`lru` / `prometheus` / `nix` 三行**已歸零**（0.9.0 自己就是這幾個值，或把相依整條刪了）；
> `brotli` / `rand` / `sfv` / `x509-parser` 四行照舊重做。下表是 0.8.1 時的原樣，留作出處。

| 相依 | 上游 0.8.1 | fork | 動機 |
|---|---|---|---|
| `lru` | `0.16.3` | **`0.18.2`** | ★ **RUSTSEC-2026-0253**（UAF）。✅ ★ **上游 `main` 已於 2026-08-25 完成實作這一條 —— 那是我們的 [PR #962](https://github.com/cloudflare/pingora/pull/962)**；⚠ PR 本身顯示 `closed / merged=false`（Cloudflare 從內部儲存庫匯入再推，PR 就這麼收場）——★ **那個狀態讀起來像被拒，而它不是**。⇒ 上游發版後本條可歸零 |
| `prometheus` | `0.13` | **`0.14`** | ★ 連帶把 `protobuf` 2.28 → 3.7.2，修 **RUSTSEC-2024-0437**（DoS）|
| `nix` | `~0.24.3` | **`0.31`** | ★ 波浪號連小版本都釘死；上游 main 已自行抬到 `~0.31.1` |
| `brotli` | `3` | **`8`** | 無公告，純版本號；**無原始碼改動**，保留成本近似為零 |
| `rand` | `0.8` | **`0.10`** | 無公告，純版本號；有原始碼改動（3 個檔案），已被單元測試覆蓋 |
| `sfv` | `0.10.4` | **`0.15`** | 無公告，純版本號；⚠ 見「殘餘風險」 |
| `x509-parser` | `0.16.0` | **`0.18.1`** | ★ **2026-08-20 加**：`instant-acme`（ARI）要 `^0.18`，不抬就是**產物裡兩份 x509-parser**。**零呼叫點適配**（用到的只有 `FromDer` / `raw_serial_as_string` / `iter_organization*` / `nom::AsBytes`，跨版本未變）|

★ ★ **`x509-parser` 那條的判據值得單說：它不是「體積大了點」。**
`tests/vendor/run.sh` 的第 [2/5] 步會比對 vendor 鎖與根鎖的版本，對不上就**拒絕跑測試**，
理由是「vendor 測試跑的不是產物裡那套組合，結果不可採信」——**那道門是對的**。
所以引入一份重複相依的真實代價是**整個迴歸網停擺**，而不是幾百 KB。
⚠ 它是 2026-08-19 那批 ACME 程式碼引入的，當時**沒跑回歸網**所以沒人看見；
交接單裡那句「沒跑不等於綠」第二天就兌現了。

### ★ 2026-08-13：還原了四條純裝飾的上界

`strum` / `strum_macros`（→ `0.26.2`）、`daggy`（→ `0.8`）、`openssl-probe`（→ `0.1.6`）、`windows-sys`（→ `0.59.0`）**已還原為上游值**。

判據是 **保留成本 vs 收益**，而不是「新就是好」：

| | 收益 | 保留成本 |
|---|---|---|
| `openssl-probe` · `windows-sys` | ★ **零**——前者只在 openssl/boringssl 後端編譯（§5.1 鎖死 rustls），後者只在 Windows（G13 只出 Linux）。**它們對產物一個位元組都不影響** | 每次 rebase 都要重做 |
| `strum` · `daggy` | 無安全公告，純版本號 | 同上；`daggy` 還連帶把 `petgraph` 拖到 0.8 |

★ 留下來的六條裡，**只有 `lru` 與 `prometheus` 有安全公告** —— 按 `G32` 該推 PR 的就是這兩條。
⚠ ⚠ **2026-09-08 實測：上游 `main` 已經自己把這兩條抬到位了**（`lru 0.18.2` / `prometheus 0.14`）
⇒ **沒有 PR 可推了**，詳見文末「rebase 的步驟」那一節。
（⛔ 此處原寫「D12 選項 1」—— `D12` 已由 `G32` 結案，⛔ 不是待辦。）

★ ★ **還原不改變任何原始碼**：這四條升級當初就沒有產生呼叫點適配（升級後簽名未變），所以還原是純清單編輯。fork 相對官方原版的全部改動只有 **12 個檔案**（2026-08-14 實測，G41 之後），可用下面「怎麼核對」一節的命令隨時重算。

### 2. 呼叫點適配（12 處，3 個檔案）

> ★ 2026-09-24（0.9.0）：`nix` 那部分**已歸零**（上游同一次遷移，`7e29246`）；`rand` / `sfv` 部分重做 ——
> ⚠ `connectors/offload.rs` 被上游挪成了 `pingora-core/src/offload.rs`（`8aeef34`），改動在新位置。

**`nix` 0.24 → 0.31**（`server/transfer_fd/mod.rs`、`protocols/l4/stream.rs`）

★ ★ **這兩處按上游 main 的做法改**——上游已經做過同一次遷移（main 的 `nix` 是 `~0.31.1`），照抄它的模式比自己發明可靠。**但沒有整檔案覆蓋**：上游 main 還帶著與本次無關的行為變更，而那些沒有被 M0 驗證過。

| 變化 | 落法 |
|---|---|
| `socket::socket()` 回傳 `OwnedFd` 而非 `RawFd` | 呼叫點加 `.as_raw_fd()`；★ **`unistd::close` 現在簽名是 `IntoRawFd` 會消費所有權，所以原有的手動 close 語義仍然正確，沒有 double-close** |
| `listen(fd, i32)` → `listen(&impl AsFd, Backlog)` | `Backlog::new(8)` |
| `fchmodat(None, ..)` → `fchmodat(impl AsFd, ..)` | `BorrowedFd::borrow_raw(libc::AT_FDCWD)`，語義與原來的 `None` 一致 |
| `RecvMsg::cmsgs()` 變為可失敗 | `?` 傳播（控制緩衝被截斷時回傳 `ENOBUFS`）|
| `setsockopt(RawFd, ..)` → `setsockopt(&impl AsFd, ..)` | 傳引用 |

★ ★ ★ **上面這五條是編譯器逼著改的。`nix` 這一輪還有兩條編譯器不會回報錯誤的語義變化，2026-08-12 全漏了**，導致 `protocols/l4/stream.rs` 裡**開了 rx timestamp 的連接每一次 `read()` 都會失敗**（`ENOBUFS`）。2026-08-13 照上游 main 修好（它做同一次遷移時兩條都改了）：

| 變化（編譯器不回報錯誤） | 漏掉的後果 | 修法（同上遊 main） |
|---|---|---|
| `cmsg_space!` 從「只有 capacity、`len == 0`」變成 ★ **`len == n` 的零向量**，而 `recvmsg` 按 **len** 取控制緩衝 | 沿用舊寫法 `.clear()` 把控制緩衝清成 **0 位元組**，於是每次 `recvmsg` 都截斷 | `.clear()` → **`.fill(0)`** |
| 配合上一行，截斷後 `cmsgs()?` 現在回傳 `ENOBUFS` 而不再靜默忽略 | `?` 把它一路拋出 `poll_read`，**read 整個失敗** | 緩衝改按 `cmsg_space!(nix::sys::socket::Timestamps)` 留（`SO_TIMESTAMPING` 給的是**三個** `TimeSpec`，原來只按一個留） |

★ **另有 4 處 `nix` 類型錯誤在 `transfer_fd` 的 `#[cfg(test)]` 裡**（`socket()` 回傳 `OwnedFd`），2026-08-12 未改——因為那些測試**從來沒有被編譯過**。同樣在 2026-08-13 補上。

**`rand` 0.8 → 0.10**（`pingora-runtime/src/lib.rs`、`connectors/l4.rs`、`connectors/offload.rs`）

★ **兩輪破壞疊在一起**：`thread_rng()` → `rng()`、`gen_range()` → `random_range()`、`seq::SliceRandom` → `seq::IndexedRandom`，
以及 ★ **0.10 特有的一條**：便捷方法從 `Rng` 移到了**新的 `RngExt` trait**（`Rng` 現在是 `rand_core` 的再匯出）。
失效形態很迷惑——`use rand::Rng;` 會被報成 **unused import**，同時 `random_range` 報 **not found**。

**`sfv` 0.10 → 0.15**（`protocols/http/compression/mod.rs`）

★ 上游 main **還沒做這個**，這一處是樞衡自己遷的。`Parser::parse_list(bytes)` → `Parser::new(bytes).parse_list::<sfv::List>()`；token 從 `&str` 變成 `&TokenRef`，取值走 `.as_str()`。

### 3. 換掉 `derivative`（RUSTSEC-2024-0388）

★ **`derivative` → `educe` 0.7.6**（`pingora-core/src/upstreams/peer.rs`，5 處）。

`derivative` 自 2021 年起未更新且已被標記失維；它在這裡只做一件事——給 `PeerOptions` 派生 `Debug` 並忽略三個函式指針欄位。`educe` 是直接對應物（2026-08-01 更新，97M 下載，OSV 乾淨）：

| | |
|---|---|
| `use derivative::Derivative;` | `use educe::Educe;` |
| `#[derive(Clone, Derivative)]` `#[derivative(Debug)]` | `#[derive(Clone, Educe)]` `#[educe(Debug)]` |
| `#[derivative(Debug = "ignore")]` ×3 | `#[educe(Debug(ignore))]` ×3 |

★ ★ **連帶白賺一個**：`syn 1.0.109` 一起消失了——`derivative` 是整個相依圖裡**唯一**還要 `syn ^1` 的包。

### ★ 4. fd 衛生：`get_fds_from()` 的兩處洩漏（2026-08-14，M1 spike 實測發現）

> ✅ **2026-09-24 已歸零**：上游 0.9.0 含 `b2b35fd`（owner 的 PR #960），`transfer_fd/mod.rs` 整份取上游，
> 上游還自帶 `test_receive_does_not_leak_fds`。⚠ `tests/m1/run.sh` [7/9] 那三條斷言**保留**：上游單測只看單個處理程序，
> 看不到 fork+exec 下的逐代累加 —— 只是提示文字改指上游。

★ **這是 fork 裡唯一一處主動改變執行環境行為的改動**（其餘都是版本上界與隨之而來的適配）。
★ 2026-08-14 補註：§5 的 rustls 那組**不屬於**這一類——它是清單 feature 修正加一處
**刻意保持語義不變**的呼叫點適配（唯一的新增是一行 `warn!` 紀錄）。兩處都在
`pingora-core/src/server/transfer_fd/mod.rs` 的 `get_fds_from()` 裡，**上游 `0.8.1` 與當前
`main` 都有**（2026-08-14 核對過 main 的同一函式），且上游儲存庫**沒有任何 issue/PR 提過 CLOEXEC**。

| | 缺陷 | 修法 |
|---|---|---|
| ① | `accept()` 出來的連接**從來不 close**——下面只 `close(listen_fd)`，而它是裸 `RawFd`、沒有 `Drop` | 包成 `OwnedFd` 交給 `Drop`，順帶覆蓋 `cmsgs()?` 的提前回傳路徑 |
| ② | `recvmsg` 用 `MsgFlags::empty()`，於是經 `SCM_RIGHTS` 收來的監聽 fd **沒有 `FD_CLOEXEC`** | 改成 `MsgFlags::MSG_CMSG_CLOEXEC` |

**實測（修復前，M1 的三代升級）**：

| | 攥著的 upgrade.sock fd | 每個監聽 socket 的 fd 重數 |
|---|---|---|
| gen1（沒經歷過移交）| 0 | 1 |
| gen2（一次移交）| **1**（`St=03` CONNECTED）| 1 |
| gen3（移交 + 繼承）| **2** | **2** |

★ ★ **兩個缺陷會疊乘**：洩漏的那個 socket 自己也沒有 CLOEXEC，於是逐代累加。

★ ★ ★ **② 對樞衡比對上游嚴重得多**，因為兩邊的升級形狀不同：

- 上游的新處理程序是**從命令行另起**的，fd 只經 `SCM_RIGHTS` 過去，不存在繼承路徑；
- 樞衡（M1）的新處理程序是**老處理程序 fork+exec 的**（systemd 要求它落在同一個 cgroup 內），
  於是沒有 CLOEXEC 的 fd 會**同時**從繼承與 `SCM_RIGHTS` 兩條路進來。

★ 而繼承進來的那一份**不在 pingora 的 fd 表裡**，所以上游那條尚未發版的
`listen_addresses()`（關掉未被認領的 fd）**夠不到它**——也就是說
「未認領 fd 黑洞化」在 fork 式升級下，**即使上游修好也可能仍然存在**。

**守門人**：`tests/m1/run.sh` [7/9] 三條斷言（fd 重數全 1、監聽 fd 帶 CLOEXEC、
攥著的 upgrade.sock 數為 0）。★ 它們同時是 **rebase 的守門人**——
上游若未接受而 rebase 時漏了重做，這三條會紅。

★ **已備好上游投稿材料**，見 [`upstream-pr/`](../../upstream-pr/README.md)。

### ★ 5. rustls 後端：兩處清單 + 一處呼叫點（2026-08-14，G41）

★ ★ ★ **先說這條最值錢的**：`pingora-rustls` 這個 crate **從來沒有被編譯、測試或稽核過**，
而 G6 把 TLS 後端鎖死在 rustls——也就是說 **M1 第一天打開 `features = ["rustls"]` 時，
產品相依圖會一次進來 42 個從未過任何一道門的 crate**（實測 133 → 175）。

三道門**各自都夠不到它**，而且都不是疏忽，是結構：

| 門 | 為什麼夠不到 |
|---|---|
| G30 的上界稽核 | 跑在**根 workspace** 上，而根 `Cargo.lock` **根本不含** `pingora-rustls`／`rustls`／`ring`／`rustls-native-certs`（實測：`sentry`／`x509-parser`／`ouroboros` 同樣缺席——**未啟用的可選相依一個都不在鎖裡**）|
| `tests/vendor/run.sh` 迴歸網 | `cargo test` **不帶任何 `--features`** → `default = []`，rustls 那一半程式碼一行都不跑 |
| `tools/supply-audit.py` | 同第一條，跑在根 workspace 上 |

✅ **已修**：迴歸網現在帶 `--features pingora-core/rustls` 跑（本條改動的守門人）。

#### ① 兩處清單：`default-features = false`（aws-lc-rs 被無謂編進產物）

> ✅ 2026-09-24 已歸零（上游 `5396a1f`，owner 的 PR #966）。

上游寫的是 `rustls = { version = "0.23.12", features = ["ring"] }`，**沒有關掉預設 feature**。
而 `rustls 0.23` 的 `default` 是 `["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]`，
`tokio-rustls 0.26` 的 `default` 是 `["logging", "tls12", "aws_lc_rs"]`——**兩扇門，都通向 aws-lc-rs**。

於是 **ring 與 aws-lc-rs 兩個 crypto provider 一起編進產物**，而這個 crate 自己裝的是 ring
（`install_default_crypto_provider()` → `rustls::crypto::ring::default_provider()`，`ring::digest` 也在用），
**aws-lc-rs 一行都不會被呼叫**。

★ **代價不是「多兩個 crate」**：`aws-lc-sys` 是 **69 MB 的 C 原始碼**（ring 是 8.5 MB）走 cmake 編，
直接頂到 `PLAN.md` G13 的 **musl 靜態二進位**散佈，也是白送的攻擊面。

| 變化 | 值 |
|---|---|
| 丟掉的預設 feature | 只有 `prefer-post-quantum`——★ 而它自己就是 `["aws_lc_rs"]`，rustls 原始碼裡每一處 `#[cfg(feature = "prefer-post-quantum")]` 都在 `src/crypto/aws_lc_rs/` 下。**ring provider 下它是空操作**（讀原始碼確認，非推斷）|
| 相依圖 | 175 → **173**，★ **只少了 `aws-lc-rs` 與 `aws-lc-sys`**，別的一個沒動 |
| 迴歸網 | stock 與改後**逐項相同**（對照組跑過）|

★ **上游 `main` 的清單一字未改**（2026-08-14 按 G32 核對），已備投稿三。

#### ② `rustls-native-certs` `^0.7.1` → `0.8.4`

`^0.7.1` 把它釘在 **0.7.3（2024-08-28）**，而 **0.8.0 第二天就發了**，現在已到 0.8.4——
**正是 G30 存在的那個「脫字號自帶上界」形狀，長在稽核夠不到的 crate 裡**。

⚠ **有呼叫點適配**（`pingora-rustls/src/lib.rs`，`load_platform_certs_incl_env_into_store`）：
0.8 的 `load_native_certs()` 不再回傳 `Result`，改為回傳 `CertificateResult { certs, errors }`——
**部分成功從此是可表達的狀態**。

★ ★ **這裡差點寫錯，值得記**：直覺寫法是「`errors` 非空即失敗」，但那**比 0.7.3 更嚴**。
0.7.3 的 `CertPaths::load()` 結尾那句註解是 `promote first error if we have no certs to return`——
**它只在一張都沒讀到時才回傳 `Err`**；只要讀到了哪怕一張，錯誤就被咽掉。
照直覺寫，`/etc/ssl/certs` 裡一個讀不動的檔案就能讓反代**起不來**。那是行為變更，不是適配。

落法：`certs.is_empty() && !errors.is_empty()` 才判失敗，其餘照舊；`errors` 裡的內容
改為 `warn!` 一行——**控制流與 0.7.3 完全一致，只是不再無聲**（0.7.3 是直接丟掉）。

連帶效果：`rustls-native-certs 0.8.4` 自帶 `openssl-probe 0.2.1`（0.7.3 用的是 0.1.x），
所以最終相依圖是 **174**，不是 173。

#### ★ ★ ★ ③ 第三扇門：`pingora-core` 自己的 dev-dependency（2026-08-19，G45）

> ✅ 2026-09-24 已歸零（同 `5396a1f`）。

**G41 只關了兩扇。第三扇是上游 PR [#966](https://github.com/cloudflare/pingora/pull/966) 的
Codex 複審提出來的**（P2），實測屬實，而且**比它說的更寬**。

`pingora-core/Cargo.toml` 的 `[dev-dependencies]` 裡有一行裸的 `rustls = "0.23"`。
它是**無條件**的，所以不只影響 rustls feature 建置——**預設（openssl）建置跑測試時同樣在編 aws-lc-rs**：

| `cargo tree -p pingora-core` | 改前 | 改後 |
|---|---|---|
| `--features rustls -e normal` | 176 | 176（這一格 G41 已經修好了）|
| `--features rustls -e normal,dev` | **260，aws-lc 在** | **258，沒了** |
| 無 feature，`-e normal,dev` | **228，aws-lc 在** | **226，沒了** |

> ★ ★ ★ **它為什麼此前沒被發現**：G41 的判據用的是 `cargo tree -e normal`，
> 而 **`-e normal` 按定義排除 dev-dependencies**。判據結構上夠不到被問的範圍——
> **這在本 fork 已經是第三次**（G41「三道門都夠不到 pingora-rustls」、G43「核對命令只 diff 一個目錄」）。
> 三次的形狀完全一樣：**判據的覆蓋面小於它自稱回答的範圍，而它照樣是綠的。**

★ **落法上刻意不指定 provider**：這條 dev-dependency 只被 `connectors/http` 的測試拿去用類型
（`ServerCertVerifier` / `CertificateDer` / `DigitallySignedStruct`），執行環境要的 provider 本來就經
`pingora-rustls` 進來。在這裡點名一個 provider，就是把本改動要修的錯誤再犯一遍，
也會和上游 [#630](https://github.com/cloudflare/pingora/pull/630) /
[#887](https://github.com/cloudflare/pingora/pull/887) 的 provider 選型工作打架。
★ 帶 `ring` 的寫法也量過——**相依圖一模一樣**，所以不指定不損失任何東西。

**實測效果**：fork 的 `vendor/pingora/Cargo.lock` 裡 `aws-lc-rs` / `aws-lc-sys` 條數
**由 2 變 0**（連帶 `dunce`、`fs_extra` 這兩個 `aws-lc-sys` 的建置相依一起消失）。
⚠ 也就是說 **G41 之後到今天，迴歸網每一次都在編那 69 MB 的 C 原始碼**。

### ★ 6. `rustls-pemfile` 遷走（2026-08-19，G45）

RUSTSEC-2025-0134：`rustls-pemfile` 失維（儲存庫 2025-08 歸檔）、`patched = []`、**無版本可升**。
owner 於 2026-08-19 拍板遷走而不是登記豁免——理由是
[`tools/supply-audit.py`](../../tools/supply-audit.py) 的 `ACCEPTED` 要求每條寫清「出路在哪」，
而這條**有乾淨的出路**；有出路卻登記進去，等於把一個已知可解的問題掛起來。
（對照 `daemonize`：它的出路是架構層的 G31，且當時還沒完成實作。）

★ **它不是換實作，是搬家**——公告原文：*"The latest version of rustls-pemfile is in fact a thin
wrapper around the same code used in rustls-pki-types"*。讀原始碼確認屬實：
`rustls-pemfile 2.2.0` 裡就寫著 `impl PemObject for Item`，`read_all` 用的是 pki-types 的同一個
`ReadIter`。這條事實把「證書與私鑰解析屬安全敏感」這個顧慮的分量降了下來。

| 動了什麼 | 落法 |
|---|---|
| `Cargo.toml` | 刪掉 `rustls-pemfile`；`rustls-pki-types` 下界 `1.7.0` → `1.9`（`PemObject` 自 1.9.0 起提供），實際解析到 1.15.1 |
| `load_pem_file()` | `Vec<Item>` → `Vec<(SectionKind, Vec<u8>)>` |
| `load_ca_file_into_store()` | `let Item::X509Certificate(..) else` → `if kind != SectionKind::Certificate` |
| `load_certs_and_key_files()` | 三分支手寫 match → `PrivateKeyDer::from_pem(kind, der)`（★ 它認的**恰好**是 Rsa/Ec/Pkcs8 三種，逐項一致）|
| `load_pem_file_ca()` / `load_pem_file_private_key()` | `rustls_pemfile::certs()` / `private_key()` → 對應類型的 `pem_reader_iter()` |

相依圖 **174 → 173**，差集只有 `rustls-pemfile v2.2.0`
（口徑：`cargo tree -p pingora-core --features rustls -e normal --prefix none | sed 's/ (\*)$//' | sort -u | wc -l`，**含根包自己**）。

#### ⚠ 一處必須逐字保留的語義：ECHCONFIG 會被靜默跳過

`rustls-pemfile` 的 `Item::from_kind()` 只映射 7 種段類型，`SectionKind::EchConfigList` 落在
`_ => None`，而上一層 `read_one` 寫的是 `None => continue`——**ECHCONFIG 段根本不會被交出來**。
pki-types 的 `(SectionKind, Vec<u8>)` 迭代器**會**把它交出來，於是
`load_ca_file_into_store()` 的「非證書即回報錯誤」會對一個含 ECHCONFIG 的 CA 檔案判紅。
**那是行為變更，不是適配**——與 §5 ② 的 `load_native_certs()` 是同一個形狀。

落法是 `RUSTLS_PEMFILE_ITEM_KINDS` **白名單**而不是把 `EchConfigList` 單獨排掉：
`SectionKind` 是 `#[non_exhaustive]`，白名單能讓將來新增的段類型繼續被跳過，
與 `rustls-pemfile` **凍結下來**的行為一致（它已歸檔，不會再長出新的 `Item`）。

#### ★ ★ 遷移之前，這個 crate 一條測試都沒有

所以先補了 **18 條特徵化測試**（`pingora-rustls/src/lib.rs` 末尾），
**照著 `rustls-pemfile` 的現有行為寫、先跑綠，然後才動遷移**。
★ 其中兩條是專門為了不讓註解停留在斷言上——`load_pem_file_ca` **讀完整個檔案**
（第一張證書之後的語法錯誤照樣判紅）而 `load_pem_file_private_key` **取到第一把就早退**，
**這兩個函式在這一點上不一樣**，而這種不對稱最容易在重寫時被「統一」掉。

**三條反證**（撤掉遷移裡的某處落法，看對應測試會不會紅）：

| 撤掉什麼 | 結果 |
|---|---|
| 白名單改成全放行（照直遷移的寫法）| ✓ `ca_store_silently_skips_echconfig` 判紅，**且只有它紅** |
| `load_pem_file_ca` 改成取到第一張就回傳 | ✓ `pem_file_ca_still_errors_on_a_malformed_section_after_the_first_cert` 判紅，且只有它紅 |
| 私鑰改成按類型偏好挑（而非檔案順序）| ✓ `certs_and_key_takes_the_first_key_in_file_order` 判紅，且只有它紅 |

★ 第一條同時證明了**照直遷移確實會改變行為**——白名單不是多餘的謹慎。

### ★ 7. 一條 flaky 測試的根因：短讀（2026-08-19）

> ✅ 2026-09-24 已歸零：上游 0.9.0 含 `91b880d`（owner 的 PR #968）。⚠ 同一條測試的另一個原因（#998 的 TOCTOU）
> 在 0.9.0 裡形狀沒變，照舊由 `tests/vendor/run.sh` 的重跑邏輯管。

`connectors::tests::test_connect_uds` 會**間歇性**失敗。它不是環境問題，是測試自己寫錯了：

```rust
let mut buf = [0; 9];
let _ = stream.read(&mut buf).await.unwrap();   // ← 回傳值就是「讀到了多少」
assert_eq!(&buf, b"it works!");
```

★ **`read()` 讀到 1 位元組也是成功的**。流式 socket 允許把 `write_all(b"it works!")`
分成多段送達，此時 `buf` 裡是半條訊息加一串 0，`assert_eq!` 就掛了。
回傳值被 `let _ =` 丟掉，於是「唯讀到幾個位元組」這件事**沒有任何地方會說出來**。

**修法**：`stream.read_exact(&mut buf).await.unwrap()`。這是 fork 裡的**第二處行為變更**
（第一處是 §4 的 fd 衛生），改的是**測試**不是產品程式碼。

#### ★ ★ 判據：同一個擾動，修前必紅、修後必綠

不是「重跑幾次沒再見到」——那種證據對間歇性缺陷等於沒有。做法是**讓它變成確定性的**：
把 mock server 的一次 `write_all` 拆成「1 位元組 + 20ms + 剩下 8 位元組」兩段送達
（**這是流式 socket 的合法行為，不是破壞**），然後

| | 結果 |
|---|---|
| 擾動 + 未修 | **必失敗**，`left: [105, 0, 0, 0, 0, 0, 0, 0, 0]`——只有 `i` 到了，後面全是 0 |
| 擾動 + `read_exact` | **通過** |
| 無擾動 + `read_exact` | 通過 |

★ 那個 `[105, 0, 0, …]` 就是短讀的簽名，也正是它偶發時會打出來的東西。

⚠ **同一形狀全樹掃過**：另有 `listeners/mod.rs` 一處 `let _ = stream.read(&mut buf)`，
但它讀完**不做內容斷言**（只為把請求排空），不是同一個缺陷，**沒有動**。

★ **上游也有這個問題**（fork 沒改過這塊），已備投稿。

### ~~★ ★ ★ 8. 加了一條能力：`TlsSettings::with_cert_resolver`（2026-08-19）~~ ✅ **已刪除（2026-08-25）**

> ✅ ★ ★ ★ **2026-08-25（G104 第 ② 處）：本條已經刪掉了，檔案與上游 0.8.1 逐位元組相同。**
> （核法：`git diff 00ebcad -- vendor/pingora/pingora-core/src/listeners/tls/rustls/mod.rs` 為空。）
> 8 / 8b / 10 三條**一起**刪的，正如下面那句預告的 —— L4 的 ClientHello 預讀換到
> BoringSSL 早回呼之後，`pingora-rustls` 在根 workspace 裡**再沒有任何使用者**。
> ★ ★ **下面的原文全部保留**，因為它記錄的是「當初為什麼不可避免」，
> 而那段論證本身沒有錯 —— 錯的是它的前提（§5.1 第 1 條），而前提已被 G104 換掉。
>
> ⚠ ⚠ **原「歸零條件」也一併作廢，而這一條值得單獨說**：本條的歸零條件在 2026-08-20
> 從「我們的投稿被接受」改成了「上游 #632 / #908 完成實作」，而**真正讓它歸零的是第三件事** ——
> 我們自己換掉了那一側的 TLS 後端。
> ★ **一條掛在別人身上的歸零條件，會讓人只盯著別人動沒動。**
>
> 以下為原文（2026-08-25 之前）：
>
> > ⚠ ⚠ ⚠ ★ **2026-08-25（G104）起，產品已經不走這條路了 —— 本條現在是死程式碼。**
> > 監聽器側換成 BoringSSL 之後，動態挑證書走的是
> > `SslContextBuilder::set_select_certificate_callback`，而 boringssl 那側的
> > `TlsSettings` 對 `SslAcceptorBuilder` 有 `Deref`/`DerefMut` ⇒ **這個 setter 本來就夠得到**。
> > ⏳ **本輪沒有刪它**，理由是**一起刪更乾淨**：改動 **10**（`Acceptor` re-export）
> > 還被 `crates/fulcrum-server/src/l4.rs` 的 ClientHello 預讀用著，`pingora-rustls`
> > 無論如何要留到那一處也換過去為止。⇒ **8 / 8b / 10 三條隨同一批一起刪。**
> > ★ 而「留著它」的代價只在 **rebase 時**才兌現（要手動重貼），
> > 而在那之前不會有 rebase —— **所以這不是拖，是把一次風險合併成一次。**

> ★ ★ ★ **2026-08-20 更新：這一條的「歸零條件」變了，而且變得更好。**
>
> 原打算給上游投一份（投稿五）。**發之前按 G32 搜了一遍，撤稿了**：這件事上游
> 從 2025-04 起已有 **9 條** issue/PR，其中
> [#632](https://github.com/cloudflare/pingora/pull/632) **與本條逐點相同、而且更完整**
> （它順手把 `build()` 那個 panic 改成了 `Result`），已有 3 個獨立使用者證實可用。
> 它卡的是合併衝突 + 維護者評審頻寬，不是設計分歧。
>
> ⇒ **本條的歸零條件不再是「我們的投稿被接受」，而是「#632 或
> [#908](https://github.com/cloudflare/pingora/pull/908) 完成實作」。**
> 那天到了就把這一條刪掉，改用上游的介面。
> ★ 順帶一條好訊息：**歸零不再取決於我們自己去推**——有人已經推了 15 個月。
> ⚠ 但在那之前，rebase 時它照舊要手動重貼。

**這是 fork 裡第一處「加能力」的改動**，前面那些都是抬上界、修缺陷或修測試。
所以先把它為什麼不可避免說清楚。

`PLAN.md` §5.1 第 1 條是**不可回頭**的硬約束：

> rustls 後端不支援 `certificate_callback`。**動態證書選擇必須實作 `ResolvesServerCert`。**

而上游 0.8.1 的 rustls 監聽器**沒有任何一扇門通向那個介面**：

| 門 | 實際情況 |
|---|---|
| `TlsSettings::intermediate(cert, key)` | 只接受**檔案路徑**，`build()` 裡寫死 `with_single_cert` |
| `TlsSettings::with_callbacks()` | **直接回傳錯誤**（"Certificate callbacks are not supported with feature rustls"）|
| 自己建構 `Acceptor` | 欄位私有，crate 外建構不出來 |
| `add_address(ServerAddress)` | 枚舉只有 `Tcp` / `Uds`，帶不上 acceptor |

後果是：**一個監聽連接埠只能有一張證書、換證書要重載、握手期按 SNI 現簽無從實作**——
而自動 HTTPS（G12）與 On-Demand TLS（G15）都建在那條路上。

**改了兩處**：

1. `pingora-rustls/src/lib.rs` —— 再匯出 `ResolvesServerCert` / `CertifiedKey` / `ClientHello`。
   ★ 為什麼由這個 crate 集中匯出、而不讓上層自己相依 `rustls`：
   aws-lc-rs 當初正是從 `rustls` 與 `tokio-rustls` **兩扇門**一起進來的（見 §5），
   每多一處 feature 宣告就多一扇門。集中在這裡，provider 的選擇只有一個地方能改。
2. `pingora-core/src/listeners/tls/rustls/mod.rs` —— `TlsSettings` 多一個
   `cert_resolver` 欄位與一個 `with_cert_resolver()` 建構器；`build()` 裡有它就走它，
   **完全不讀證書檔案**（於是 `intermediate()` 那個「檔案讀不到就 panic」與這條路無關）。

⚠ ⚠ **代價必須記下來**：加能力比抬上界更貴。上游一旦改動 `TlsSettings` 的形狀，
這一條要**手動重貼**，而不是像版本上界那樣「看看上游是不是自己抬了」。
★ 已備上游投稿（這是一個對所有 rustls 使用者都缺的能力），若被採納這條 fork 改動即可歸零。

★ 判據：`crates/fulcrum-tls` 的 `SniResolver` 走這條路，而
`tests/serve/run.sh` 用真流量驗過——`--cacert` 驗簽通過、ALPN 協商到 h2、
未知 SNI 被拒絕握手（curl 結束代碼 35）。

### ~~★ ★ 8b. 加了第二條能力：`TlsSettings::set_alpn_wire`（2026-08-20）~~ ✅ **已刪除（2026-08-25）**

> ✅ **2026-08-25（G104 第 ② 處）：隨 8 / 10 一起刪掉了**，理由與改動 8 同一條：
> BoringSSL 那側直接 `set_alpn_select_callback` 寫個閉包即可，
> 而那個「枚舉裡沒有變體能表達混合清單」的阻礙在那一側根本不存在。
> ★ 原文保留在下面 —— ⚠ 那段「先更正一句在四份文件裡被重複了三天的話」的記錄
> **與本條的存廢無關，它記的是一次判據教訓**，不隨程式碼刪除而作廢。

**這是 fork 裡第二處「加能力」的改動**，而它比 §8 那處**小一個數量級**：
只給一個已有的私有欄位開一個寫入口，不新增類型、不改任何上游行為、不碰別的 TLS 後端。

⚠ ★ **先更正一句在四份文件裡被重複了三天的話。** 那句話是
「TLS-ALPN-01 做不了，因為 `TlsSettings` 只能設 `ALPN::{H1,H2,H2H1}`」。
**它不準確**：上游 0.8.1 的 `ALPN` 枚舉**本來就有 `Custom(CustomALPN)` 變體**
（`protocols/tls/mod.rs`），`set_alpn` 也是公開的。真正的阻礙在別處——

| 變體 | `to_wire_protocols()` 吐出什麼 |
|---|---|
| `H1` | `[http/1.1]` |
| `H2` | `[h2]` |
| `H2H1` | `[h2, http/1.1]` |
| `Custom(x)` | **`[x]`** —— 只有它自己 |

⇒ **枚舉裡沒有任何一個變體能表達「內建那兩條 + 一條自訂」的混合清單**，
而 `alpn_protocols` 欄位是私有的。這才是阻礙。

RFC 8737 的 TLS-ALPN-01 要的正是這種混合：**同一個 443 連接埠**既要正常服務
h2 / http/1.1，又要在 CA 來驗證時協商出 `acme-tls/1`。挑戰挪不到別的連接埠——
RFC 8737 §3 把連接埠寫死成 443。

**改了一處**：`pingora-core/src/listeners/tls/rustls/mod.rs` 加一個
`set_alpn_wire(Vec<Vec<u8>>)`。樞衡把清單設成 `[h2, http/1.1, acme-tls/1]`。
★ 協商語義由 rustls 定：伺服器端這份清單是**偏好序**，取第一條用戶端也提供的 ——
所以 `acme-tls/1` 放在末尾，**正常流量的偏好完全不受影響**，
而只提供 `acme-tls/1` 的 CA 驗證連接會且只會協商到它。

★ **歸零條件**：上游給 `ALPN` 加一個多協定變體，或給 `TlsSettings` 開等價入口。
⚠ 與 §8 一樣，rebase 時要手動重貼——但這一處只有三行。

★ 判據：`tests/acme/run.sh` 裡 pebble 的 `tlsPort` 直接指向樞衡的 TLS 連接埠，
證書**真的由 TLS-ALPN-01 簽下來**；另有兩條反向斷言釘住
「普通流量拿不到挑戰證書」與「acme-tls/1 拿不到真證書」。

### ~~★ 10. 一行 re-export：`rustls::server::{Accepted, Acceptor}`（2026-08-23，M2 批 C）~~ ✅ **已刪除（2026-08-25）**

> ✅ ★ ★ ★ **2026-08-25（G104 第 ② 處）：換掉了，本條刪除。**
> L4 的 SNI/ALPN 分流現在走 BoringSSL 的早回呼（`SSL_CTX_set_select_certificate_cb`），
> 落點在 [`crates/fulcrum-server/src/l4.rs`](../../crates/fulcrum-server/src/l4.rs) 的
> `peek_client_hello`：位元組喂進一條**記憶體傳輸**，ClientHello 解完的那一刻早回呼觸發，
> 抄走 `servername()` 與 ALPN 擴充之後**當場把握手掐掉**。
>
> ⚠ ⚠ **換過來有一處行為變化，而它是變好的那一側**：早回呼發生在幾乎所有 ClientHello
> 檢核**之前**，而 rustls 的 `accept()` 要握手前半程做完才給 `Accepted`。
> ⇒ 一個 rustls 會當場拒掉的 ClientHello（缺 `signature_algorithms`），
> 在這一側照樣讀得出 SNI 並正常分流 —— **那正是 `tests/l4/run.sh` ⑥ 當初寫下的意圖**
> （「樞衡不替上游拒絕一個它自己也許能處理的用戶端」），只是 rustls 那條路做不到。
> ★ 那道判據因此換了期望值，舊契約與換的理由留在它自己那裡。
>
> ★ 而刪掉它的**代價是零**：這一行本來就只是 re-export；
> 而收益是 `pingora-rustls` 整個退出根 workspace 的相依圖（連同 `no_debug` 共 2 個包）。

`pingora-rustls/src/lib.rs` 裡多了一行 `pub use`。

**為什麼需要**：L4 的 SNI/ALPN 分流要**唯讀 ClientHello、不完成握手**，
而 rustls 的 `Acceptor` 正是為這件事存在的（`read_tls` + `accept()` → `Accepted`）。

★ ★ **它不是「加能力」**，與 §8 / §8b 那兩處性質不同：那兩處改的是上游的行為面，
而這一行只是把 rustls 已有的類型**轉出去**。而且這正是那個檔案自己寫著的做法 ——
它上面幾行的註解是「⚠ 不讓上層自己直接相依 `rustls`……此處集中匯出，
provider 的選擇仍然只由本 crate 的清單決定」。

| | |
|---|---|
| rebase 成本 | 幾乎為零（一行 `pub use`，與上游程式碼不衝突）|
| 新增相依 | **0 個包** —— rustls 早就在圖裡 |
| 歸零條件 | 上游哪天自己匯出它就刪掉這一行 |

⚠ **替代方案是手寫 ClientHello 解析**，而那是在解析**攻擊者控制的二進位** ——
安全基線點名不許手搓的那一類裡最靠邊的一種。⇒ 這一行換掉的是那件事。

### ★ ★ ★ 11. 把刪掉的 `pingora-boringssl` 加回來（2026-08-25，G104）

**這是 fork 裡第一處「把當初刪掉的東西加回來」的改動**，而它的成因不在上游，在我們自己：
`PLAN.md` §5.1 第 1 條被 **G104** 推翻了。

改了三處，全在清單裡，**沒有一行原始碼改動**：

1. `Cargo.toml`（workspace）—— members 加 `pingora-boringssl`；
2. `pingora-core/Cargo.toml` —— 加回可選路徑相依，以及上游原樣的
   `boringssl = ["pingora-boringssl", "openssl_derived"]`；
   ★ `openssl_derived` 才是原始碼裡那一大片 `#[cfg]` 用的名字：**boringssl 與 openssl
   共用同一份實作**（`listeners/tls/boringssl_openssl/`）。
3. `pingora-boringssl/Cargo.toml` —— 見下面那條**唯一的樞衡改動**。

#### ★ 唯一的改動：去掉上游的 `boring/pq-experimental`

> ✅ **2026-09-24 已歸零**：0.9.0 的清單是 `boring = "5"` 且不帶特性（boring 5 裡 `pq-experimental` 是空特性，
> PQ 補丁無條件打上）⇒ 這個 crate 在 fork 裡**零改動**。⚠ 下面「與 G30 相反的上界紀律」（釘在 `^4`）一併作廢：
> quiche 0.30 接 `>=4.19, <6`，三個消費者統一到 boring 5.2.0。⚠ 下文「`SSL_use_second_keyshare` 只有打補丁才存在」
> 一句存疑：2026-09-24 的調研在本機 boring-sys 4.22.0 原始碼（含補丁與 deps/boringssl）裡沒搜到這個符號。

上游寫的是 `boring = { version = "4.5", features = ["pq-experimental"] }`。
⚠ ⚠ **那個 feature 不是一個開關，它會讓 `boring-sys` 給 BoringSSL 原始碼打一個補丁**
（`boring-sys-4.22.0/build/main.rs`：`apply_patch(config, "boring-pq.patch")`，
並打一行 `cargo:warning=applying experimental post quantum crypto patch to boringssl`）。

★ **而本 crate 用不上它**：`src/ext.rs` 裡唯一碰後量子的地方是 `ssl_use_second_key_share`，
被本 crate **自己的** `pq_use_second_keyshare` feature 門控（預設關），關著時是個空函式。
⇒ 去掉它不改變任何行為。⚠ 將來若要打開 `pq_use_second_keyshare`，
**必須同時把 `pq-experimental` 加回來** —— `SSL_use_second_keyshare` 這個符號只有補丁打上才存在。

> ★ ★ ★ **更硬的那條理由：它決定了 `docs/verification/musl-boringssl.md` 那份驗證還算不算數。**
> 那份探針編的是**原版** BoringSSL。開著 `pq-experimental`，產品編的就是另一份打過補丁的原始碼，
> 而「musl 上驗過了」這句話**不再覆蓋產品**。
> ⇒ **一次驗證的適用範圍，止於它真正編過的那份原始碼。**

#### ⚠ 一條與 G30 相反的上界紀律

`boring` 的 `"4.5"`（脫字號 ⇒ `<5.0.0`）**必須保留，不許按 G30 抬**。
G30 的口徑是拆掉那些把傳遞相依釘在舊版上的上界；這一條相反：**`quiche 0.29.3`（G103）
要的也是 `boring ^4`，兩邊必須解到同一個 `boring`**，否則 `SslContextBuilder`
同名而是兩個類型。實測兩者解到同一個 **`boring 4.22.0`**。
⇒ 抬成 `>=4.22` 會讓 cargo 挑 5.2.0，當場把 quiche 那條路弄壞。

#### ★ 它順手讓另外三條 fork 改動**可以歸零**

| 改動 | 為什麼在 BoringSSL 那側不需要 |
|---|---|
| **8**（`TlsSettings::with_cert_resolver`）| boringssl 的 `TlsSettings` 對 `SslAcceptorBuilder` 實作了 `Deref`/`DerefMut`，`set_select_certificate_callback` **本來就夠得到** |
| **8b**（`set_alpn_wire`）| 同上：`set_alpn_select_callback` 也夠得到，混合 ALPN 清單自己寫一個閉包即可 |
| **10**（re-export `rustls::server::{Accepted, Acceptor}`）| ⏳ 要等 L4 的 ClientHello 預讀也換過去；`boring::ssl::ClientHello` 有 `servername()` / `get_extension()` / `as_bytes()` |

★ ★ **FORK.md 自己寫著「加能力比抬上界更貴，rebase 時要手動重貼」**——
⇒ **這一次換後端會讓 fork 變小，不是變大。**
⚠ 但**現在還一條都沒刪**：產品仍然跑在 rustls 那條路上（本次只是把後端加回來，沒有點亮）。
刪除的時機與判據見 `PLAN.md` §10 第 61 輪。

#### 首次點亮的實測

`cargo check -p pingora-core --features boringssl --no-default-features`（2026-08-25）：
**0 個 error，43 條 warning，1m25s**。
★ 值得記一句：**這些 `#[cfg]` 分支在本 fork 裡從來沒有被編譯過**
（`Cargo.toml` 原註解：「原始碼裡的 `#[cfg(feature)]` 分支原樣保留，只是永遠不會被點亮」），
而 fork 期間抬了十條版本上界、換掉了 `derivative`、改過 `nix` 的呼叫點。
⇒ 「保留著的分支」與「還能編的分支」是兩件事，而**在點亮之前沒有任何東西能分開它們**。
⚠ 那 43 條 warning 集中在 `boring 4.22` 已廢棄的 `SslCurve` / `set_curves`
（`connectors/tls/boringssl_openssl/mod.rs`，出站那側），**不影響 lint 門**——
`-D warnings` 只作用於 clippy 直接 lint 的那個 crate，vendor 作為相依被普通 rustc 編譯。

### ★ ★ ★ 12. 加了一條能力：PROXY protocol 的「收」接縫（2026-08-27，M2 批 L 第 ① 步）

> ★ ★ **2026-09-24（0.9.0）換了形狀（B，owner 拍板）**：上游 0.9.0 自己加了 `PreTlsProcess`（`600c5c0`），
> 但有意只在 TLS 分支裡調 ⇒ fork 只剩 `listeners/mod.rs` 的 `handshake()` 裡「把回呼挪到 TLS 分支之前」幾行；
> trait、讀循環、換 digest、setter、四個欄位**都不在 fork 裡了**。讀取邏輯在
> `crates/fulcrum-server/src/proxyproto.rs`（`PreTlsProcess` 實作，6 條真 socket 單測）。
> 下文描述的是 0.8.1 時的形狀，留作出處。**歸零條件改為：上游讓 pre-TLS 回呼在明文連接埠也調。**

**上游完全不支援 PROXY protocol**（全庫零命中，2026-08-24 核過）。
樞衡要在 **HTTP 面**收它，而唯一正確的位置是**拿到裸 `L4Stream`、TLS 握手之前**的那一處
—— `listeners/mod.rs` 的 `UninitializedStream::handshake()`，它是 `pub(crate)`，外部夠不到。

⚠ **本條屬「加能力」那一類**，G30 刻意讓這一類保持稀有 —— 現存的另一處是改動 11。

#### 改了什麼：**一個檔案，190 行，零刪除**

`pingora-core/src/listeners/mod.rs`：

| 加了什麼 | 說明 |
|---|---|
| `pub trait ProxyProtocolPolicy` + `pub enum ProxyProtocolVerdict` | **接縫本身**。兩個方法（`trusts` / `feed`）都由使用方實作 |
| `Listeners` / `TransportStackBuilder` / `TransportStack` / `UninitializedStream` 各一個欄位 | 把策略從 `Listeners` 一路帶到那條連接上 |
| `Listeners::set_proxy_protocol()` | 與上游自己的 `set_connection_filter()` **逐字同形**（含「已有的與之後加的都設上」那半） |
| `read_proxy_protocol()` + `override_peer_addr()` | 循環讀 + 覆蓋位址 |
| `handshake()` 裡一次呼叫 | 在 `set_buffer()` 之後、TLS 之前 |

★ ★ ★ **解析與信任判斷一行都沒進 fork**：前者在
[`fulcrum_runtime::proxyproto`](../../crates/fulcrum-runtime/src/proxyproto.rs)（28 條單測），
後者是 `fulcrum_runtime::Runtime::trusts_proxy_protocol`。
⇒ **這段程式碼不認識 PROXY protocol 的任何一個位元組**，rebase 時它不會成為負擔。

★ 使用方**不需要給上游再加任何東西**：`Service::endpoints()` 本來就是 `pub`，
回傳 `&mut Listeners`。

#### ⚠ ⚠ ★ ★ ★ 一處「看起來行、實際不行」的落法，寫下來免得下一個人再走一遍

`PLAN.md` §10 **第 49 輪**那份勘察裡有一句「好訊息」：

> 「`SocketDigest.peer_addr` 是 **`pub` 的 `OnceCell`** ⇒ 搶在 pingora 自己填之前
> `set` 進去就夠了，**不必改它讀取用戶端位址的任何一處**。」

**那句話是錯的。** `services/listening.rs` 在 `io.handshake()` **之前**就調了
`io.peer_addr()`（為了握手失敗時那行紀錄裡能帶上位址），而
`SocketDigest::peer_addr()` 是 **`get_or_init`** ⇒ 到我們這一步，那個 `OnceCell`
**已經被填過了**，`set()` 必然回傳 `Err`。

⚠ ⚠ 而 `OnceCell::set()` 的回傳值**很容易被寫成 `let _ =`** ——
於是「位址沒換成」會是一次**完全無聲**的失效：紀錄正常、請求正常、
只有 `remote_ip` 一直是那臺 LB 的位址。

⇒ 落法改成**換一整份 `SocketDigest`**（`GetSocketDigest::set_socket_digest`，上游本來就有）：
從同一個 fd 新造一份、先把 `peer_addr` 填好再掛上去。
`local_addr` / `original_dst` 會從**同一個 fd** 重新惰性派生，什麼都不丟。

> ★ ★ **一份勘察報告裡的「好訊息」，與一條被跑過的路，是兩件事。**
> 那句話在 2026-08-24 寫下時沒有任何東西撞過它，而它躺了三天。
> （同一份勘察裡還有一句也錯了：「它們是 fork 的第 11 / 12 處」——
> 11 號在 2026-08-25 被改動 11 佔掉了。⇒ **一份勘察會同時留下事實和過期物，而它們長得一樣。**）

#### 歸零條件

⏳ **上游支援 PROXY protocol，或接受一個同形的接縫**。
★ 與改動 8/8b/10 那三條不同的是：**本條不指望上游做什麼** ——
它們當年的歸零條件全都掛在「上游動一下」上，而真正讓它們歸零的是第三件事（G104 換了 TLS 後端）。
⇒ 這裡如實寫：**沒有已知的歸零路徑**，它就是一項要跟著 rebase 的常年成本。

### ★ ★ ★ 13. 一處缺陷修復：h1 的 `body_bytes_sent` 把回應頭也數了進去（2026-08-27，M2 批 L 第 ② 步）

> ✅ 2026-09-24 已歸零：上游 `e7de90a` 修了同一個缺陷，語義與我們一致（h1 與 h2 都不計回應頭），
> 並自帶 `body_bytes_sent_excludes_response_header`。

**這一處屬「缺陷修復」那一類**（與改動 4「fd 衛生」、改動 7「短讀根因」同類），
**不是「加能力」** —— 它讓一個函式**符合它自己的文件**。

#### 事實

`pingora-core/src/protocols/http/v1/server.rs` 的 `write_response_header` 裡有一行：

```rust
self.body_bytes_sent += write_buf.len();
```

而 `write_buf` 是**序列化之後的回應頭**。同一個欄位的讀取函式上面寫著：

> `/// Return how many response body bytes (application, not wire) already sent downstream`

⇒ **名字說 body，文件說 body，而它數了 header。**

#### ⚠ ⚠ 而決定性的不是「名不副實」，是**它與 h2 不一致**

| | 計數點 | 含回應頭？ |
|---|---|---|
| **h1**（`v1/server.rs`）| 寫頭處 **與** 寫體處各 `+=` 一次 | ✅ **含** |
| **h2**（`v2/server.rs`）| 只在寫體處 `body_sent += data_len` | ❌ 不含 |
| **h3**（樞衡自己的 `H3Session`）| 只數體（`sent_n`）| ❌ 不含 |

★ 上游自己的三條 h2 測試斷言「16 位元組體 ⇒ `body_bytes_sent() == 16`」——
**上游對這個數的期望就是「只數體」**。

⇒ 同一個請求走 h1 與走 h2，這個數**差一個回應頭的長度**，而沒有任何東西說出這件事。

#### 為什麼樞衡非管不可

訪問紀錄有一格 **`resp_size`**，契約（[`docs/architecture/observability.md`](../../docs/architecture/observability.md)）
寫的是「回應**體**位元組數（不含頭）」—— 而**那是一份公開契約**，
不能按用戶端用了哪個協定給出兩個不同的答案。

⚠ 實測形態：一個 4 位元組的 `respond 200 "a-ok"`，紀錄裡 `resp_size` 是 **144**。
★ 它不會讓任何東西變紅 —— 那個數看起來完全像一個回應的大小。

#### 改了什麼

**刪掉那一行**（一處），並在**上游自己的測試模組裡**加一條迴歸守衛（一處）。

⚠ ⚠ ★ **守衛放在上游的測試模組裡，不是放在樞衡那一側** ——
那樣它才會在 [`tests/vendor/run.sh`](../../tests/vendor/run.sh)（fork 迴歸網）裡跑，
**於是一次把那行加回來的 rebase 會當場變紅**。

##### ★ ★ ★ 那條守衛第一版是壞的，而逮到它的是反證的**失敗訊息**

第一版沒給 mock IO 寫 `.write(...)` 期望 ⇒ 它在**乾淨樹上也紅**，
而且紅在 `tokio-test` 的 `unexpected write` 上 ——
**一條紅得與它聲稱要守的那件事毫無關係的判據**。

⚠ 我沒在乾淨樹上跑過它就去做注入；注入之後它紅了，
而**紅的理由讀起來不對**（注入的是計數器，紅的是 mock IO）。

> ★ ★ ★ **「它紅了」與「它因為我注入的那件事而紅」是兩回事。**
> ⇒ 反證要看的不只是**紅沒紅**，還有**紅在哪一行、訊息說的是不是那件事**。
> 修好之後再注入，它紅在 `left: 19, right: 0` 上 —— 19 正是那個回應頭的位元組數。

#### 歸零條件

⏳ **上游自己修掉它**（或接受一份同形的 PR）。
★ 與改動 4 / 7 同類：這兩條當年也是「上游也有這個問題，已備投稿」。
⚠ 本條**尚未投稿** —— 登記在這裡，免得它變成一件只有程式碼記得的事。

### ★ ★ ★ 14. 加了兩格資料：`SslDigest` 的 `sni` / `alpn`（2026-08-27，G128，D27 + D28 結案）

**這一處屬「加能力」那一類**（與改動 12 同類），⚠ 而它加的是**資料**不是行為 ——
兩個 `Option<String>` 欄位 + 兩行賦值，**不改任何函式簽名**。

#### 改了什麼

| 檔案 | 改動 |
|---|---|
| `pingora-core/src/protocols/tls/digest.rs` | `SslDigest` 加 `pub sni: Option<String>` 與 `pub alpn: Option<String>`；`new()` 的**函式體**裡初始化成 `None` |
| `pingora-core/src/protocols/tls/boringssl_openssl/stream.rs` | `SslDigest::from_ssl()` 在建好之後填這兩格（`ssl.servername(HOST_NAME)` / `ssl.selected_alpn_protocol()`）|

★ **有意不進 `new()` 的簽名**：那樣 `rustls` / `s2n` 兩個後端的呼叫點一個字都不用改，
而它們本來就填不出這兩格。⇒ 這次改動**只碰 boringssl 那一支**。

#### 為什麼樞衡非管不可

訪問紀錄有 `tls_sni` / `tls_alpn` 兩格（契約見
[`docs/architecture/observability.md`](../../docs/architecture/observability.md)）。

上游把這兩格留給了 `TlsAccept::handshake_complete_callback` + `SslDigest.extension`
（回傳值會被塞進那個擴充位，HTTP 層從 `session.digest()` 拿得到）。
⚠ ⚠ **走那條路要求監聽器帶回呼**，而帶回呼時上游走的是 `handshake_with_callback()` ——
它的第一行 `start_accept()` **無條件**裝一個恆回 `-1` 的 `cert_cb`
（`ext::suspend_when_need_ssl_cert` → `SSL_set_cert_cb(raw_cert_block)`）
⇒ **每條 TLS 連接都要多走一趟「掛起 → `certificate_callback` → `resume_accept`」**。

> ★ ★ ★ 那趟開銷是 §10 **第 78 輪實測**出來的，而它推翻了一句寫下來的推理：
> 「證書是同步挑的 ⇒ 握手不會為證書掛起 ⇒ `certificate_callback` 不該被調到」。
> **每一個詞都對，只有「因此」兩個字是錯的** —— 掛不掛起不由「需不需要」決定，
> 由上游那一行**無條件**的 setter 決定。

⇒ 第 79 輪改成這一處 fork：`from_ssl()` 在握手結束後本來就握著 `&SslRef`，
順手記兩格**一分額外開銷都沒有**。

##### ★ 它還順帶解決了 h3（D27）

`SslDigest` 一旦有了這兩格，**h3 也能自己造一份**
（`crates/fulcrum-server/src/quic/h3_session.rs` 的 `quic_digest`：
`version` 恆 `TLSv1.3`（RFC 9001 §4.2）、`sni`/`alpn` 取自 `quiche::Connection` 的公開 API）。
⇒ h1/h2 與 h3 在訪問紀錄那一層**走同一段程式碼**，「同一格資料兩個填法」在結構上做不到。

⚠ 而 `SslDigestExtension::set` 是 `pub(crate)` —— **不動 fork 的話 h3 根本塞不進那個擴充位**，
這也是候選方案「只把 `set` 放開成 `pub`」被否掉的原因：它只買到一半（D27），
而另一半（D28）的落點就在同一個檔案裡。

#### 守它的是誰

| 半邊 | 誰守 |
|---|---|
| **那兩格還在不在** | ★ 編譯器（我們的程式碼讀 `ssl.sni`）+ [`crates/fulcrum/tests/tls_digest_gate.rs`](../../crates/fulcrum/tests/tls_digest_gate.rs) 門 1 —— ⚠ 兩者的**回報錯誤不一樣**：編譯器說「`SslDigest` 沒有 `sni` 欄位」（會讓人去改自己的程式碼），門 1 說「**fork 改動 14 被 rebase 沖掉了**」|
| **`from_ssl()` 還填不填** | ⚠ ⚠ **編譯器守不住這一半** —— 一次把 `from_ssl` 恢復成上游版本的 rebase 照樣編得過，而紀錄裡那兩格會**靜靜地變成 `None`**（讀起來是「用戶端沒發 SNI」，一句完全成立的假話）。⇒ 門 2 讀那個函式體，釘住兩處賦值**與它們取自哪兩個 `SslRef` 方法** |
| **執行環境真的填對了** | **第二十三個場景** [`tests/log/run.sh`](../../tests/log/run.sh)：真握手上量 h1/h2 四格、h3 三格，外加一條 `--no-alpn` 的反向（★ 它是唯一分得出「量出來的」與「猜出來的」那條）|

★ ★ **這一條與改動 13 的守法不同，而不同是有理由的**：改動 13 是**刪一行**，
守衛能長在上游自己的測試模組裡（`tests/vendor/run.sh` 會跑）；
本條是**加兩格資料**，而 `from_ssl()` 要一個真的 `SslRef` 才測得到 ——
⚠ 在上游測試模組裡寫不出一條不假裝的判據，所以守衛落在**我們這一側**，並寫明瞭它們各自答什麼。

#### 歸零條件

⏳ **上游自己把 SNI / ALPN 記進 `SslDigest`**（或接受一份同形的 PR）。
★ 上游今天的立場是「用 `extension` 自己塞」——
⚠ 那條路對**只想要 SNI/ALPN** 的人來說，代價是每條握手多一趟掛起/恢復，
而這一點大機率上游自己也沒量過。**本條尚未投稿。**

### ★ ★ ★ 15. 加了一條能力：連接計數接縫（2026-09-03，M2 批 O）

> ★ 2026-09-24（0.9.0）重做：⚠ 0.9.0 新增建構點 `add_listener()`，`add_tcp` / `add_uds` / `add_tls_with_settings`
> 都走它 ⇒ 那裡也複製 `connection_counter`（填 `None` 能編過，但之後加的連接埠靜默不計數）；
> `services/listening.rs` 的 import 與 0.9.0 新加的 `ListenerConfig` 並列。

**上游沒有任何按監聽器統計連接數的出口**：`Service::run_endpoint` 那條 accept 循環
把連接直接 spawn 出去，`TransportStack` 是 `pub(crate)`，外部夠不到那一刻。
樞衡要的是 `fulcrum_connections_total` / `fulcrum_connections_active{listen,entrypoint}`
（`PLAN.md` §10 的 **G122 ②**），而它必須在 **accept 之後、握手之前**記一筆。

⚠ **本條屬「加能力」那一類**，G30 刻意讓這一類保持稀有 —— 現存的另外兩處是改動 11 與 12。

#### 改了什麼：**兩個檔案，一處刪除（一行 import 重排）**

> ⛔ 這裡**有意不寫行數**：一個寫在散文裡的行數沒有任何門守著，改一次就過期而不會紅
> （本儲存庫 2026-09-03 為這條栽過一次）。要數就跑
> `git diff <上游基线> -- pingora-core/src/listeners/mod.rs pingora-core/src/services/listening.rs`。

`pingora-core/src/listeners/mod.rs`：

| 加了什麼 | 說明 |
|---|---|
| `pub trait ConnectionCounter` | **接縫本身**，兩個方法（`enter` / `leave`）都由使用方實作 |
| `pub struct ConnGuard` | ★★★ `leave` 的**唯一**呼叫點。建構即 `enter`，`Drop` 即 `leave` |
| `Listeners` / `TransportStackBuilder` / `TransportStack` 各一個欄位 | 把計數器從 `Listeners` 一路帶到那條連接上 |
| `Listeners::set_connection_counter()` | 與上游自己的 `set_connection_filter()`、與改動 12 的 `set_proxy_protocol()` **逐字同形**（含「已有的與之後加的都設上」那半）|
| `TransportStack::connection_counter()` | `run_endpoint` 取句柄用 |

`pingora-core/src/services/listening.rs` 的 `run_endpoint`：循環外取一次句柄與
`Arc<str>` 位址，**spawn 之前**建構 guard 並把它移進任務。

★ ★ ★ **這段程式碼不認識 Prometheus 的任何一個概念**：它不知道 counter 與 gauge 的區別、
不認識標籤、也不知道 `entrypoint` 是什麼。它只遞一個 `listen: &str`（就是
`TransportStack::as_str()` 那個監聽位址原樣），別的全在
[`crates/fulcrum-server/src/conn_stats.rs`](../../crates/fulcrum-server/src/conn_stats.rs)。
⇒ rebase 時它不會成為負擔。

#### ⚠ ⚠ ★ 為什麼 `ConnGuard` 必須長在**這一側**

它完全可以定義在樞衡那一側，fork 只留一個兩方法的 trait —— fork 會更薄。
**但那樣 `run_endpoint` 裡就變成「先 `enter`、再自己建構 guard」兩步**，
而**漏掉建構 guard 那一行正是 rebase 衝突裡最容易被合掉的形狀**。
⚠ 它失效時的表現是 **gauge 只漲不降**：正文格式合法、counter 在動、series 也都在，
只有一個數字永遠只增 —— **沒有任何東西會紅**。
⇒ 放在這一側，是讓「守衛存在」成為**結構事實**而不是使用方的紀律。

#### ⚠ ⚠ ⚠ 那個必須綁名字的變數

`run_endpoint` 裡那句 **`let _conn_guard = conn_guard;`** 寫成裸 `let _ = conn_guard;`
會讓它**當場 drop**，於是每條連接進來就立刻減一 ⇒ gauge 恆為 0 而 counter 照漲。
⚠ **同一個後果還有第二種形態**：把那一行**整個刪掉** —— 那個值就不再被移進
`async move` 塊，於是在 accept 循環這一輪末尾就 drop 了。

✅ **2026-09-04：這一段補上了自己的門。**
`listeners/mod.rs` 的 `mod test` 裡多一條 **`枢衡改动15_守卫必须被移进任务且绑了名字`**：
它 `include_str!` 讀 `services/listening.rs`，剝掉整行註解後要求「建構了 `ConnGuard`」
∧「有一個**具名**的移動」∧「沒有裸 `let _ = conn_guard`」。
★ **兩種形態都實測過會紅**（注入寫在位元組副本上、跑完核 sha256 還原），
而同一趟裡那條**行為**判據（`枢衡改动15_连接守卫在_drop_时减一`）**照舊綠** ——
⇒ 兩條各守一段（一條驗行為、一條驗串接），⛔ 別合併。

⚠ ⚠ **它第一次跑就紅在自己身上**，而原因值得記住：`listening.rs` 裡那句**警告**本身就寫著
「⛔ 寫成裸 `let _ = conn_guard;` 會當場 drop」，判據把**註解裡那句警告當成了壞程式碼**。
⇒ 修法是**先剝整行註解**，⚠ 且只剝整行（行內 `//` 未必是註解）；剝離器自己也帶兩條自證，
另有一份夾具專門守「註解裡提到壞形態的正確原始碼必須被判為對」——
那是這個 bug 的迴歸測試。★ 已知邊界寫在門裡：**行尾註解剝不掉**。

★ 端到端那一層仍然有它自己的判據（`tests/metrics/run.sh` 裡
「連上 TLS 連接埠什麼都不發 ⇒ `active` +1」）⇒ **三層各守一段**。

#### 守衛

照改動 13 的手法，**迴歸守衛長在上游自己的測試模組裡**
（`listeners/mod.rs` 的 `mod test`）：一個假的 `ConnectionCounter`，
斷言「建構即 enter 一次」「drop 即 leave 一次」「遞過去的是監聽位址原樣」。
★ 反證實測：把 `ConnGuard::new` 裡那句 `counter.enter(&listen)` 刪掉 ⇒ 它當場紅，
且 `tests/vendor/run.sh` 把它定向重跑一次後判為「**重跑仍失敗 —— 這才是 fork 該被判紅的東西**」。

#### 歸零條件

⏳ **沒有已知的歸零路徑。** 上游沒有表達過要給監聽器加統計出口的意思，
而這條接縫服務的是樞衡自己的指標契約。⇒ 它是一項**要跟著 rebase 的常年成本**。
★ 與改動 8/8b/10 那三條不同：它們的歸零條件掛在「上游動一下」上，本條不指望上游做什麼。
⏳ **投不投上游等 rebase 讀過上游 `main` 之後再判**（G122 已定）——
上游 `main` 已把 `prometheus` 整條刪掉，口味未知。

### ★ ★ ★ 16. 一類新改動：請求路徑上三處語義不變的開銷（2026-10-06，G157）

⚠ **本條開了一個新類別：只為效能、對樞衡看得見的行為一個不變。** 此前的類別是版本上界與呼叫點適配（§1–§7）、
加能力（11 / 12 / 14 / 15）、缺陷修復（13）。本類與它們一樣要**隨 rebase 重放**；收進來的門檻是「語義不變」——
改了行為的（請求頭 / 回應頭保留大小寫的表、keep-alive 等下一條請求時的計時器）⛔ 不在本條，各要一處設計、另拍。

**起因**：M3 的 HTTP 層診斷（`PLAN.md` §7 M3）。開發機上的裸 pingora 診斷臺對 fork 裡的候選逐個做單變數試驗
（每個變體一份副本、只差一處補丁；計數口徑 = 每請求的使用者指令數與分配次數，⛔ 不是效能宣告，讀數不入庫，`G139`），
owner 2026-10-06 按推薦先拍這三處。

#### 改了什麼：**兩個檔案，加一個測試檔案**

| | 檔案 | 改了什麼 | 為什麼語義不變 |
|---|---|---|---|
| ① | `src/protocols/l4/stream.rs` | `poll_read` / `poll_write` / `poll_flush` / `poll_write_vectored` 之後**不再記等待計時**（原來每次都調一次計時，讀鍾只在掛起的起止：轉為掛起時一次、掛起後就緒時一次）。`AccumulatedDuration` 與兩個 getter 原樣留著（少改上游的行），`total` 恆為零 | 那兩個 getter（`get_read_pending_time` / `get_write_pending_time`）在樞衡與本 fork 的 8 個 crate 裡**沒有任何讀者**，只有 trait 實作；恆為零也正是 trait 預設值 |
| ② | `src/protocols/http/v1/server.rs` | `read_request` 的 256 個頭槽位**不先填**，改走 `httparse::Request::parse_with_uninit_headers` | 讀過 httparse 1.10.1 的原始碼：`parse` = `parse_with_config(缺省配置)` → `parse_with_config_and_uninit_headers`，與 uninit 入口是**同一個解析器、同一份預設設定**；差別只在沒解析完時 `req.headers` 留什麼，而 `read_request` 在那兩種情況下不讀它 |
| ③ | 同上 | 頭偏移寫進**棧上**的槽位（新函式 `populate_header_refs`，規則與 `common.rs` 的 `populate_headers` 相同），不再每條請求分配一個 `Vec<KVRef>` | 同一組偏移，只是放的地方不同；`populate_headers` 原樣留給 `client.rs` |
| — | `tests/fulcrum_read_request_alloc.rs`（新檔案）| ③ 的分配次數判據，見下 | |

⚠ ② 只改預設那條路：`patched_http1` 那條照上游（先填後 `parse_unchecked`）。**該特性在本 fork 本來就編不過** ——
crates.io 的 httparse 1.10.1 沒有 `parse_unchecked`（那是 Cloudflare 自己那份 httparse 的方法），改前改後一樣。

**量了多少**（開發機，200 連接 × 200000 條，三輪中位；⛔ 不是效能宣告）：裸 pingora 每請求使用者指令 ① −374 · ② −770 · ③ −250，
三處合計約 −1.4k（約 −5%）；malloc ③ −1（25 → 24）；`clock_gettime` ① −2。用這份 fork 本身重編的複核（同一臺診斷臺、同一口徑）：
使用者指令 −1315（200 連接）/ −1388（1 條連接），都是 −4.7%；malloc −1、`clock_gettime` −2 逐位吻合；核心指令在噪聲裡。

#### 守衛（都長在 fork 自己的測試裡，`tests/vendor/run.sh` 照跑）

| 判據 | 守什麼 | 反證（注入寫在 vendor 的副本上、掛到同一路徑編，⛔ 沒碰工作樹）|
|---|---|---|
| `l4/stream.rs` 的 `枢衡改动16_读等待不再计时` | 一次真的掛起過約 50 ms 的讀之後，`get_read_pending_time()` 仍是零；自帶「這次讀確實等過」的自證 | `poll_read` 裡那一行等待計時加回去 ⇒ **只紅它** |
| `v1/server.rs` 的 `枢衡改动16_头数正好到上限时逐个对得上_多一个就拒` | 頭數恰好 `MAX_HEADERS` 時 256 個頭的名與值逐個對得上（② ③ 兩處棧上槽位都用滿）；多一個照上游拒（`InvalidHTTPHeader`）| 給 httparse 少遞一個槽位 ⇒ **只紅它**（256 個被拒）；`populate_header_refs` 漏掉最後一個 ⇒ 紅它（另一條的「3 個頭」斷言也跟著紅）|
| `tests/fulcrum_read_request_alloc.rs` 的 `枢衡改动16_读请求头不再为偏移单独分配` | 一條普通請求一次讀完時，`read_request` 一共分配 **8** 次（三次獨立編譯都是 8；裡面有讀緩衝、請求頭、頭表與保留大小寫的表等，⚠ 組成沒有逐項拆）；單獨一個測試二進位、只數本執行緒；自帶「分配器真的在數」的自證 | ③ 那兩處換回上游原樣 ⇒ **只紅它**，「分配了 9 次（期望 8）」|

四次注入每次都有 `Compiling pingora-core` 為證（副本在容器裡先 touch，繞開 mtime 陷阱）。

⚠ 第三條數的是**整個** `read_request`：rebase 後上遊自己增減了分配也會紅 —— 那時先核是誰動的，再改期望值，⛔ 別為了綠直接改數。
跨兩次讀才完整的請求頭、URI 轉義後重解析（②③ 都要重新進那個循環），上游自己的測試（`read_2_buf` 等、`escaped_uri_*`）已覆蓋。

#### 歸零條件

② ③ 不相依樞衡；owner 2026-10-08 拍「先備材料、暫不發」⇒ 投稿材料在 [`upstream-pr/`](../../upstream-pr/README.md) 投稿七
（一份 issue、一份 PR、兩個提交，基於上游 `main` `4487f7b`）。⏳ 發不發是對外動作，另拍。
⚠ ① **在上游不是語義不變**：上游的 `pingora-proxy` 讀上游連接的 `get_write_pending_time()`（`proxy_h1.rs`，`Session::upstream_write_pending_time()`），
兩個 getter 還是公開 trait 的方法 ⇒ 原樣投不了，可選做法記在投稿七那一節。在本 fork 裡它照樣語義不變（那個 crate 不在 fork 裡）。
在歸零之前三處都是**要跟著 rebase 的常年成本**：都是小補丁，① 的四處刪除最容易在衝突裡被合掉 —— 那時第一條判據會紅。

### ★ ★ 17. 標題化表補七個名，並公開它（2026-10-14，G162）

**起因**：§7 M3 底座單變數試驗的 A2 —— 回應頭保留大小寫的那張表（`ResponseHeader::build` 另建的第二個 `HeaderMap`）
每個回應兩次分配，裸 pingora 上界約 −6% 使用者指令。樞衡自己構的回應要換 `build_no_case` 而線上頭名不變，
前提是沒表時 pingora 寫得出同樣的大小寫；而上游這張表（`pingora-http/src/case_header_name.rs` 的 `titled_header_name_str`，
12 個名，上游自己留著 `TODO: add more const header here`）裡沒有樞衡會發的 `ETag` 等幾個。owner 2026-10-14 拍（彈出視窗兩次）。

#### 改了什麼：**兩個檔案**

| | 檔案 | 改了什麼 |
|---|---|---|
| ① | `pingora-http/src/case_header_name.rs` | 表裡補 `Allow` · `Alt-Svc` · `Content-Range` · `ETag` · `Last-Modified` · `Location` · `Vary`；函式由 `pub(crate)` 改 `pub` |
| ② | `pingora-http/src/lib.rs` | 在 crate 根再匯出它（一行），樞衡用它判「不建表時這個名會不會照原樣寫出」|

⚠ ⚠ **這不是「語義不變」那一類（改動 16）**：這張表同時決定**用 `http::header` 常數插入的名**怎麼寫，有沒有大小寫表都一樣。
⇒ 壓縮層用常數改寫的 `ETag`（弱化）與追加的 `Vary`，從小寫 `etag:` / `vary:` 變成 `ETag:` / `Vary:`。
開了 `encode` 的站台上，`Vary` 是隻要壓縮層認為回應隨 `Accept-Encoding` 變就追加（壓沒壓都追加）⇒ 那類站台的大部分回應都看得見。
owner 10-14 認了這一處（與不壓時、與預壓縮旁檔案、與 nginx 的寫法一致）。別的路徑上這七個名幾乎只出現在回應裡：
h2 / h3 用戶端的請求轉發到 h1 上游時若帶了它們，也從小寫變成這裡的寫法。

#### 量了多少（開發機診斷臺，計數口徑，⛔ 不是效能宣告）

連同樞衡那一側（`file_server` 一律、`respond` / 跳轉 / 錯誤頁在使用者 `header` 指令的名都照原樣時不建表）：
每請求 malloc / free 各 −2（`respond` 與 `file_server`，1 條連接與 200 條連接四格都是）；使用者指令 −5% 到 −6%（`file_server` 200 條連接 −9.6%，改前區間寬）。

#### 守衛

| 判據 | 守什麼 | 反證（注入寫在臨時 worktree 裡，⛔ 沒碰工作樹）|
|---|---|---|
| 本檔案的 `枢衡改动17_补进标题化表的名照原样写出` | 七個名照這裡寫（查表、常數轉名、不建表寫 h1 三條路）；表外的名照舊小寫 | 表裡刪掉 `ETag` 與 `Vary` ⇒ 紅（`None` 對 `Some("ETag")`），同 crate 另外 56 條照綠 |
| 樞衡的 `tests/files/run.sh` 逐行釘頭名（200 / 206 / 416 / 301 / 405）與 `tests/encode/run.sh` 釘 `ETag` / `Vary` 大小寫 | rebase 沖掉補的那幾行時，線上的樣子 | 同一個注入 ⇒ files 紅在帶 `ETag` 的三條（301 / 405 照綠）、encode 三條全紅 |

#### 歸零條件

上游那條 TODO 正是這件事 ⇒ ① 是給上游提 PR 的候選（對外動作，owner 定）；② 上游未必收，收不了就留一行。
在歸零之前是**要跟著 rebase 的常年成本**：表是七行、衝突時最容易被合掉 —— 那時上表兩條判據都會紅。

### ★ ★ 18. 讀 h1 請求頭時可以不建保留大小寫的表（2026-10-14，G163）

**起因**：§7 M3 底座單變數試驗的 A1 —— 讀請求頭時另建的那張表（`RequestHeader::build` 的第二個 `HeaderMap`）每條請求兩次分配，
裸 pingora 上界約 −4.5% 使用者指令。它只在把請求頭**原樣轉發給 h1 上游**時有用；`respond` / `file_server` 一類終結不讀頭名的大小寫。
上游 pingora 一律建（`read_request` 寫死 `build`）。owner 2026-10-14 拍「按連接埠開關」（推薦項；另一個選項是一律不建，那會改反代時上游看到的頭名）。

#### 改了什麼：**兩個檔案，加一條測試**

| | 檔案 | 改了什麼 |
|---|---|---|
| ① | `pingora-core/src/protocols/http/v1/server.rs` | `HttpSession` 加一格 `preserve_header_case`（預設 `true`，與上游相同）與它的 setter；`read_request` 按它選 `build` 或 `build_no_case` |
| ② | `pingora-core/src/protocols/http/server.rs` | `Session::set_preserve_header_case` 轉給 h1；h2 / 子請求 / 自訂 no-op（與 `set_keepalive` 同形）|
| — | `pingora-core/tests/fulcrum_read_request_alloc.rs` | 多一條：關掉時比開著少 2 次分配 |

★ **預設不變** ⇒ 不調 setter 的呼叫方（含上游自己的全部測試）行為一字不差；這一類與改動 15 一樣屬「加能力」，G30 刻意讓它稀有。
每條請求的工作階段都是新建的 ⇒ 呼叫方要每條設一次；樞衡在 `keepalive::read_request` 的參數裡設（編譯期逼著呼叫方表態）。

#### 量了多少（開發機診斷臺，計數口徑，⛔ 不是效能宣告）

樞衡在沒有反代站台的連接埠上關掉它：`respond` 與 `file_server`、1 / 200 條連接四格每請求 malloc 各 −2，使用者指令 −2% 到 −5%；
反代連接埠照舊建表，讀的是同一條老路。

#### 守衛

| 判據 | 守什麼 | 反證（注入寫在臨時 worktree 裡，⛔ 沒碰工作樹）|
|---|---|---|
| 本 fork `v1/server.rs` 的 `枢衡改动18_关掉保留大小写_名值照旧_写回按缺省写法` | 預設 / 顯式開 / 關三種：名與值照舊可查；開著時有表、寫回原樣，關掉時沒表、寫回按標題化表或小寫 | setter 被忽略（恆建表）⇒ 紅它，並紅下一條與樞衡 `keepalive` 的單測 |
| `tests/fulcrum_read_request_alloc.rs` 的 `枢衡改动18_关掉保留大小写少两次分配` | 關掉時恰好少 2 次（同一份請求，開與關各數一次）| 同一個注入 ⇒ 紅它，而改動 16 那條照綠 |

#### 歸零條件

要上游接一個「讀請求頭時不保留大小寫」的選項（或把那張表做成按需建）⇒ 是給上游提 PR 的候選（對外動作，owner 定）。
在那之前是**要跟著 rebase 的常年成本**：一格、一個 setter、一處 `if`，衝突時最容易丟的是那個 `if` —— 上表兩條判據都會紅。

### ★ ★ 19. 寫 h1 回應頭時少兩次分配（2026-10-16，G165）

**類別**：與改動 16 同一類 —— 只為效能、對樞衡看得見的行為一個不變（線上位元組逐個相同）。
**起因**：`PLAN.md` §7 M3 記著 `respond` 每請求 18 次 malloc 裡有三次在 pingora 寫回應頭那一步；開發機診斷臺的歸因
（`handoff/` 下，⛔ 不入庫）把其中兩次落在 `prepare_response_header`。owner 2026-10-16 按簡報推薦拍（兩處都改 · 判據並進現有計數測試二進位 · 投稿材料先備不發）。

#### 改了什麼：**一個檔案（改動 16 已計入），判據並進已有的測試檔案**

| | 位置（`src/protocols/http/v1/server.rs`）| 改了什麼 | 為什麼語義不變 |
|---|---|---|---|
| ① | `prepare_response_header` 裡補 `Connection` 那幾行 | `"keep-alive"` / `"close"` 改成 `HeaderValue::from_static(…)`；上游那條 `TODO: make these lazy static` 就是這件事 | `&str` 走 `HeaderValue::from_str` → `Bytes::copy_from_slice`，每次拷 5 / 10 位元組；`from_static` 不分配。兩者都是 `is_sensitive = false`，寫出去的位元組相同 |
| ② | `prepare_response_header` 的回傳值，與它的兩個呼叫方 | 回傳 `BytesMut` 而不是 `freeze` 過的 `Bytes`；proxy task 那條路（`send_header_task` 要 `Bytes`、會存起來）在呼叫處自己 `freeze` | 緩衝按 4 KiB 開、頭只有幾百位元組 ⇒ 長度 ≠ 容量 ⇒ `freeze` 走 `Vec → Bytes` 要另分配一塊 `Shared`（bytes 1.12.1）。直寫那條路只 `write_all(&buf)` 借用、用完就丟 ⇒ 那塊是白分配的。同一份位元組，只是不凍結 |

⚠ 樞衡的 h1 不走 proxy task（`proxy_tasks_enabled` 預設 false，`crates/` 裡沒有啟用的）；那條路照舊 `freeze` 一次，行為不變。
檔案數不變：兩處都在改動 16 已計入的 `v1/server.rs`，判據在改動 16 已計入的 `tests/fulcrum_read_request_alloc.rs`（2026-10-16 用下文命令對 `0.9.0` 實測仍是 20 個）。

#### 量了多少（開發機診斷臺，計數口徑，⛔ 不是效能宣告）

改前 `9b43f01`、改後同一棵樹加本改動，同一個容器同一個目標卷編：每請求 malloc / free 各 −2（`respond` 18 → 16、`file_server` 21 → 19，1 與 200 條連接四格都是）；
使用者指令 1 條連接 `respond` −1.9%、`file_server` −2.5%，200 條連接時在三輪區間內分不出；
32 種請求（含四條續連接）除 `Date` 外逐位元組相同（指標端點正文是活計數，只比頭），比較器負對照（G162 之前的二進位）紅 9 條。

#### 守衛（都在 `tests/fulcrum_read_request_alloc.rs`，`tests/vendor/run.sh` 照跑）

| 判據 | 守什麼 | 反證（注入寫在臨時 worktree 裡，⛔ 沒碰工作樹；每次都有 `Compiling pingora-core`）|
|---|---|---|
| `枢衡改动19_写响应头不再拷连接头的值也不再冻结缓冲` | 讀完一條請求後 `write_response_header` 一共分配 **1** 次（只剩序列化用的那塊暫存緩衝；上游原樣是 3 次，改前在本 fork 上實測 3）；`keep-alive` 與 `close` 兩種都數。連接用預留了容量的 `Cursor<Vec<u8>>`，寫的時候連接自己不分配 | ① 換回 `&str` ⇒ **只紅它**「分配了 2 次（期望 1）」；② 換回 `freeze` ⇒ **只紅它**，同樣是 2 |
| `枢衡改动19_写响应头的字节照旧` | 續與不續各寫一次：除 `Date` 的值（核它是 HTTP 日期）外逐位元組等於 `HTTP/1.1 200 OK` · `Content-Length: 0` · `Date` · `Connection: keep-alive` / `close`。本 fork 8 個 crate 裡寫回應頭的測試幾乎都關了 `update_resp_headers`，沒有一條釘這兩行（上游只在沒進 fork 的 `pingora-proxy` 整合測試 `test_basic` / `test_upstream` 裡經 OpenResty 查 `Connection` 的值）| 值寫成 `Keep-Alive` ⇒ **只紅它**（迴歸網裡上游自己的測試沒有一條跟著紅 —— 正是要補它的理由）|

proxy task 那處漏了 `freeze` ⇒ 編不過（E0308，類型逼著），不需要測試守。
⚠ `Date` 是執行緒本機快取、每秒換一次，換的那一秒裡第一次寫會多分配幾次 ⇒ 分配那條先熱身、再取三次裡最少的。
⚠ 與改動 16 那條一樣，數的是**整個**呼叫 ⇒ rebase 後上遊自己增減分配也會紅：先核是誰動的，再改期望值。

#### 歸零條件

① 是上游自己的 TODO、② 動的是私有函式 ⇒ 兩處在上游也語義不變，一起做成一份投稿（[`upstream-pr/`](../../upstream-pr/README.md) 投稿八，基於上游 `main` `4487f7b`；⏳ 材料已備、未發，發不發另拍）。
上游 `main`（2026-10-16 克隆）這兩處與 0.9.0 一字不差。在歸零之前是**要跟著 rebase 的常年成本**：兩處都小，衝突時最容易丟的是呼叫處那個 `freeze` —— 丟了編不過；另兩處被合掉時分配那條會紅。

### ★ 20. 兩條上游 h2 測試改成「h2 在 codec 層就拒掉」（2026-10-16，h2 0.4.20）

**類別**：改測試、不改產品程式碼（與改動 7 同一類）。
**起因**：2026-10-16 的相依追新（`dep-check.py --apply`，37 項，為修 rustls 四條公告）把 `h2` 0.4.19 → 0.4.20。
0.4.20 的 `recv_headers` 自己對**值不同的重複 `content-length`** 發 `PROTOCOL_ERROR` 重置流（CHANGELOG「Fix rejecting duplicate `content-length` headers with different values」；0.4.19 只看第一個值）
⇒ 這樣的請求到不了 `accept()`，pingora 自己那道重複 CL 檢核與每連接壞流預算都碰不到它。上游兩條測試斷言的正是 pingora 那一層 ⇒ 迴歸網紅兩條、定向重跑仍紅。
上游 `main`（2026-10-16 讀）這兩條與 0.9.0 一字不差；投稿七、八在上游 `main` 上跑全 workspace 時它們兩側都紅、當時沒查根因（[`upstream-pr/`](../../upstream-pr/README.md)）——
就是這一條：同日在上游 `main` `4487f7b` 上實測，h2 0.4.20 兩條紅、換 0.4.19 兩條綠；投稿八那兩份現生成的鎖都是 0.4.20（投稿七的鎖沒留下，按日期推斷同樣是）。
上游已有人在 PR #816 的評論裡提過（2026-10-06），沒有 issue、沒有修它的 PR。
owner 2026-10-16 拍（彈出視窗）：採 0.4.20、改測試（另兩個選項：h2 暫留 0.4.19；整批撤回、只升 rustls）。

#### 改了什麼：**一個檔案**（`pingora-core/src/protocols/http/v2/server.rs`，此前與 0.9.0 逐字相同 ⇒ fork 檔案數 20 → 21）

| 上游測試 | 上游斷言 | 改成 |
|---|---|---|
| `test_req_conflicting_content_length_rejected` | 伺服器端拿到 `Some(H2Accept::Rejected)`，下一次 `from_h2_conn` 才是 `None` | 第一次 `from_h2_conn`（1 s 超時）就是 `None` —— 流在 codec 裡被重置、pingora 沒見過它；用戶端照舊斷言 `PROTOCOL_ERROR` |
| `test_req_malformed_stream_budget_exhausted` → **改名** `test_req_conflicting_content_length_not_charged_to_malformed_budget` | 預算 31 時這條請求把預算耗盡（`H2Error`）| 預算 31 時回傳 `None`、預算仍是 31 |

線上行為：用戶端看到的仍是 `RST_STREAM PROTOCOL_ERROR`，只是發的人從 pingora 換成了 h2。
⚠ 唯一的差別是計數：這類流不再計入 pingora 每連接 32 條的壞流預算，改計入 h2 自己的本機錯誤重置上限（`DEFAULT_LOCAL_RESET_COUNT_MAX` = 1024）。
預算耗盡本身仍由上游另兩條測試守（`test_authority_mismatch_exhausts_malformed_stream_budget`、`test_forbidden_target_exhausts_malformed_stream_budget`），不靠本節這條。

#### 反證（臨時副本，⛔ 沒碰工作樹；每次都重編了 `pingora-core`）

副本里把 h2 退回到 0.4.19、只跑這兩條 ⇒ **兩條都紅**：「malformed request must not reach Pingora's acceptance checks」與「malformed request was charged to Pingora's budget」；
工作樹（0.4.20）裡兩條都綠 ⇒ 它們釘的是 h2 這一處行為，h2 哪天退回去會當場紅。

#### 歸零條件

上游遲早要為 h2 0.4.20 改這兩條；rebase 時照上游的改法、刪掉本節。在那之前衝突只會落在這兩條測試上（產品程式碼一行沒動）。
⏳ 投稿材料已備、未發：[`upstream-pr/`](../../upstream-pr/README.md) 投稿九（同樣兩條測試，另把上游 workspace 的 `h2` 下界抬到 `>=0.4.20`；fork 這邊沒抬）。

### ★ 21. `serde_yaml` 換成 `serde-saphyr`（2026-10-16）

**類別**：換相依（與改動 3 `derivative` → `educe` 同一類）。
**起因**：RUSTSEC-2024-0449 —— `serde_yaml`（`0.9.34+deprecated`）底下的 `unsafe-libyaml` 失維（儲存庫 2024-03 歸檔，libyaml 的 c2rust 機器翻譯），無修復版；
公告 2026-10-12 改過之後才出現在掃描裡，而且它在產品的**編譯圖**裡（`pingora-core` 的非可選相依）。
owner 2026-10-16 拍（彈出視窗），★ **推翻了 agent 推薦項**：推薦的是登記 `ACCEPTED`（產品從不解析 YAML：走 `Server::new_with_opt_and_conf`、`ServerConf` 在 Rust 裡構、`opt.conf` 被忽略；
只有兩個 spike 經 `Server::new` 解析常數 `"---\nversion: 1"`），另一個選項是照上游 issue #779 把 `serde_yaml` 做成可選 feature。

#### 改了什麼：**兩個檔案**（`Cargo.toml` 已計入 ⇒ fork 檔案數 21 → 22）

| | 檔案 | 改了什麼 |
|---|---|---|
| ① | `pingora-core/Cargo.toml` | `serde_yaml = "0.9"` → `serde-saphyr = { version = "2", default-features = false, features = ["deserialize", "serialize"] }`（不要 `comments`）|
| ② | `pingora-core/src/server/configuration/mod.rs` | `from_yaml` / `to_yaml` 兩處換成 `serde_saphyr::from_str` / `to_string`；測試 `test_working_directory_deserializes_from_yaml_string` 原用 `serde_yaml::to_value` 取樹，`serde-saphyr` 沒有 `Value` ⇒ 改成核那一行 + 往返一次 |

兩把鎖：`serde_yaml` / `unsafe-libyaml` 都走了（根鎖連帶少了 `ryu`）；新進 `serde-saphyr` 2.0.0 · `granit-parser` 2.0.0 · `annotate-snippets` · `encoding_rs` · `encoding_rs_io` ·
`arraydeque` · `core_detect` · `multiversion_no_op` · `nohash-hasher` · `unicode-width`（vendor 另有 `simdutf8`，根鎖裡早有），**全部過 24 小時懷疑期**，現有包一個版本都沒動。
⚠ 這些包**進了產品的編譯圖**（202 → 210），而產品照舊一行 YAML 都不解析 —— 這是換掉 `unsafe-libyaml` 的代價，寫在明處。
⚠ 看得到的行為差別：序列化時列表裡的字串帶引號（`- "1.2.3.4"`），往返不變；兩個解析器在邊角 YAML 上的差異**沒有逐項比**。
對樞衡無影響（不讀 YAML 設定），只影響直接用 pingora YAML 設定的人。

#### 判據

`pingora-core` 的 `server::configuration` 全部 14 條測試照跑照綠（含讀真 YAML 檔案的 `test_load_file`、拿常數構預設值的 `test_default`）；兩個 spike 的 `Server::new` 由 M0 / M1 場景覆蓋。

#### 歸零條件

上游 issue #779（「Make serde_yaml optional or configurable」，2025-12 起 open）完成實作，或上游自己換掉 `serde_yaml` ⇒ rebase 時照上游、刪掉本節。
⛔ 原樣投給上游不合適：`serde-saphyr` 2 要 Rust 1.89，上游 MSRV 是 1.85。在那之前是**要跟著 rebase 的常年成本**：一行清單、兩處呼叫、一條測試。

### 9. 沒有動的一個：`daemonize`

> ✅ 2026-09-24：上游 0.9.0 自己把 `daemonize` 換成了 `daemonix`（`ae96f7e`）⇒ RUSTSEC-2025-0069 隨 daemonize
> 一起離開兩把鎖，`tools/supply-audit.py` 裡那條豁免已刪。下文是當時的權衡，留作出處。

★ **`daemonize` 原樣保留，這是一個經過權衡的決定，不是遺漏。**

它的問題同樣是**失維**（RUSTSEC-2025-0069）而非陳舊——**沒有更新的版本可升**。但與 `derivative` 不同，它做的是**特權丟棄**（`setuid` / `setgid` / `initgroups` / `chown_pid_file`），換掉它的三條路都不划算：

| 路 | 為什麼不走 |
|---|---|
| 手寫替代 | ★ 特權丟棄的順序錯一步就是真實提權漏洞。與專案安全基線「不手寫安全關鍵原語」的精神直接衝突 |
| 換 `daemonize-me` / `daemonize2` | 維護中，但下載量分別只有 `daemonize` 的 1/50 與 1/1700。**在特權丟棄這條路上，把久經使用但失維的換成新鮮但少人跑的，不是明確的安全改善** |
| 在 fork 裡 vendor 它 | 只是把同一段程式碼換個位置，OSV 不再報是因為它不再是 registry 包——**那是騙掃描器，不是修問題** |

★ ★ **真正的出路是架構層面的**：G13 定的散佈方式是 **systemd**，而 systemd 下的現代做法是 `Type=simple` / `Type=notify` **前台執行**，由 `User=` / `Group=` 做特權丟棄、紀錄走 journal。那樣 `conf.daemon` 恆為 `false`，`daemonize()` **整條路徑都是死程式碼**，相依可以直接刪掉——**並且特權丟棄交給了比任何 crate 都更經得起稽核的 systemd**。

但這取決於「Pingora 的零停機升級在前台模式下怎麼做」，那是 **M1 的設計問題**（與 D5 / D6 相鄰），不是這次 fork 該順手拍板的。當初登記為 **D12** —— ⚠ 那一條已經結案，不在 `PLAN.md` §11 裡了：處理程序模型由 **G31** 定（`Type=notify` 前台執行）並經 **G37** 修訂（`ExitType=cgroup`，不交接 MainPID），而 `daemonize` 這個相依**是被權衡後保留的**（理由見 [`supply-chain.md`](../../docs/platform/supply-chain.md)）。

★ **在那之前，這條公告是已知且已接受的**：它是 informational（失維），**沒有 CVE**。

## 效果（2026-08-12 實測）

| | fork 前 | fork 後 |
|---|---|---|
| 鎖定包總數 | 176 | **162** |
| 陳舊包 | 44 | **17**（排除非 Linux 目標後 **12**）|
| 安全公告 | 4 | ★ **1** |
| 其中真漏洞 | 2（`lru` UAF、`protobuf` DoS）| ★ ★ **0** |

**只剩 `daemonize` 一條**（失維、無 CVE、無可升版本），理由見上一節。

### 剩下 12 項為什麼升不動——★ 已逐條查過綁定約束

**其中 2 項根本不參與編譯**（`bitflags 1.3.2`、`miniz_oxide 0.8.9` 只存在於其他 target/feature 的解析結果裡）。

★ **真正在編譯的 10 項，全部被「已經是最新版」的第三方 crate 卡住**——也就是說**不是我們落後，是生態鏈上游還沒採納新大版本**：

| 陳舊包 | 卡它的是 | 那個卡它的包是不是最新？ |
|---|---|---|
| `allocator-api2` 0.2.21 | `hashbrown 0.17.1` 要 `^0.2.9` | ★ **是最新** |
| `base64` 0.22.1 | `sfv 0.15.0` 要 `^0.22.1` | ★ **是最新** |
| `alloc-no-stdlib` / `alloc-stdlib` | `brotli 8.0.4` 要 `<3` / `~0.2` | ★ **是最新** |
| `thiserror` / `thiserror-impl` 1.0.69 | `protobuf 3.7.2` 要 `^1.0.30` | ★ **是最新** |
| `getrandom` 0.3.4 | `ahash 0.8.12` 要 `^0.3.1` | ★ **是最新** |
| `syn` 2.0.119 | `strum_macros 0.28.0` 等要 `^2.0` | ★ **是最新** |
| `hashbrown` 0.15.5 · `foldhash` 0.1.5 | ★ **`petgraph 0.8.3` 要 `^0.15.0`**（`indexmap` 與 `lru` 都已要 `^0.17`）| ★ **是最新** |

★ ★ **結論：這 10 項不可能靠我們這側的任何操作升上去**——除非再去 fork `brotli` / `sfv` / `hashbrown` / `protobuf` / `ahash` / `petgraph` 六七個包，而這一層**已經沒有任何安全公告**，純粹是版本號。**性價比為負，明確不做。**

## ★ 遷移完整性（2026-08-13 稽核）

`nix` 那條真迴歸暴露的問題是「遷移只做了編譯器逼著做的那一半」。補完之後**逐條查過還剩什麼沒做**，方法與結論如下。

### 怎麼核對（可隨時重跑）

```bash
# 1) fork 相對官方原版到底改了哪些檔案
#    ★ ★ 必須**逐個 vendored crate 都比**，外加 workspace 清單。
#    此前這裡只 diff 了 pingora-core 一個目錄，於是 pingora-runtime 的兩個檔案
#    （它自己的結論段裡明明列著）與 workspace Cargo.toml 結構上都看不見。
#    「改了哪些檔案」這種問題，判據的覆蓋面必須等於被問的範圍。
#    ★ 2026-09-24 更正：這裡原先只列了 7 個 crate，**漏了 G104 加回來的 pingora-boringssl** ——
#      正是上面那句話說的形狀，於是 §11 的改動檔案結構上看不見。現在是 8 個。
#    ⚠ 本機全域 core.autocrlf=true：在 Windows 上匯出上游樹必須 `-c core.autocrlf=false`，
#      否則匯出的是 CRLF，每個檔案都會被比成「有改動」。
git clone --depth 1 --branch 0.9.0 https://github.com/cloudflare/pingora /tmp/stock   # 702f690
for c in pingora-core pingora-error pingora-http pingora-pool \
         pingora-runtime pingora-timeout pingora-rustls pingora-boringssl; do
  git --no-pager diff --no-index --stat "/tmp/stock/$c" "/w/vendor/pingora/$c"
done
git --no-pager diff --no-index --stat /tmp/stock/Cargo.toml /w/vendor/pingora/Cargo.toml

# 2) 有上游參照物的那條，逐行比 nix API 的使用
git -C /tmp/stock fetch --depth 1 origin main
git -C /tmp/stock show FETCH_HEAD:pingora-core/src/protocols/l4/stream.rs
```

### 結論

> ★ **2026-10-16（改動 21）：22 個檔案** —— 改動 20 那 21 個，加 `pingora-core` 的 `src/server/configuration/mod.rs`
> （改前它與 `0.9.0` 逐字相同；`pingora-core/Cargo.toml` 早已計入）。同日對 vendor 跑 `cargo fmt` 只重排了改動 19 與改動 6 自己寫的三處（已計入的檔案），檔案數不變。
>
> ★ **2026-10-16（改動 20）：21 個檔案** —— 改動 19 那 20 個，加 `pingora-core` 的 `src/protocols/http/v2/server.rs`
> （改前它與 `0.9.0` 逐字相同：拿 GitHub 上 tag `0.9.0` 的原檔案比，0 行差異；⚠ 沒有重跑上面那整條命令）。
>
> ★ **2026-10-16（改動 19）：仍是 20 個檔案** —— 改動 19 兩處都落在已計入的 `protocols/http/v1/server.rs`，
> 判據在已計入的 `tests/fulcrum_read_request_alloc.rs`（同一條命令對 `0.9.0` 實測，`702f690`）。
>
> ★ **2026-10-14（改動 18）：20 個檔案** —— 改動 17 那 19 個，加 `pingora-core` 的 `src/protocols/http/server.rs`
> （改動 18 另兩處落在已計入的 `protocols/http/v1/server.rs` 與 `tests/fulcrum_read_request_alloc.rs`；同一條命令實測）。
>
> ★ **2026-10-14（改動 17）：19 個檔案** —— 下面那 17 個，加 `pingora-http` 的 `src/case_header_name.rs` 與 `src/lib.rs`
> （用上面的命令對 `0.9.0` 實測，`702f690`；只差權限位、內容為 0 行的不算）。`pingora-http` 自此不再是零改動。
>
> ★ **2026-10-06（改動 16）：17 個檔案** —— 下面那 14 個，加 `pingora-core` 的 `protocols/http/v1/server.rs`、
> `protocols/l4/stream.rs` 與新增的 `tests/fulcrum_read_request_alloc.rs`（用上面的命令對 `0.9.0` 實測，`702f690`）。
>
> ★ **2026-09-24（相對 0.9.0）：14 個檔案** —— workspace `Cargo.toml` · `pingora-core` 的 `Cargo.toml`、
> `connectors/l4.rs`、`offload.rs`、`listeners/mod.rs`、`protocols/http/compression/mod.rs`、
> `protocols/tls/boringssl_openssl/stream.rs`、`protocols/tls/digest.rs`、`services/listening.rs`、`upstreams/peer.rs` ·
> `pingora-runtime` 的 `Cargo.toml`、`src/lib.rs` · `pingora-rustls` 的 `Cargo.toml`、`src/lib.rs`；
> `pingora-error` / `pingora-http` / `pingora-pool` / `pingora-timeout` / `pingora-boringssl` 零改動。
> 下面是 2026-08-14 相對 0.8.1 的那一版，留作出處（⚠ 那張表把 `upstreams/peer.rs` 記在 §2 下，應屬 §3；
> 到 0.8.1 後期它已經漲到 19 個檔案）。

**fork 相對官方原版共改動 12 個檔案**（2026-08-14 用上面的命令逐 crate 實測）：

| crate | 檔案 | 出自 |
|---|---|---|
| workspace | `Cargo.toml` | 範圍裁剪 21 → 7 個成員；`educe` |
| `pingora-core` | `Cargo.toml` | §1 版本上界 + ★ §5 ③（G45：dev-dependency 那扇門）|
| | `src/connectors/l4.rs`、`src/connectors/offload.rs` | §2（`rand`）|
| | `src/protocols/http/compression/mod.rs`、`src/upstreams/peer.rs` | §2（`sfv`）|
| | `src/protocols/l4/stream.rs` | §2（`nix`，含那條真迴歸）|
| | `src/server/transfer_fd/mod.rs` | §2（`nix`）+ ★ §4（兩處 fd 洩漏）|
| `pingora-runtime` | `Cargo.toml`、`src/lib.rs` | §2（`rand`）|
| **`pingora-rustls`** | **`Cargo.toml`、`src/lib.rs`** | ★ **§5（G41）+ §6（G45：遷走 `rustls-pemfile` + 18 條特徵化測試）** |

★ ★ **此前這裡寫的是「9 個檔案」，漏了 3 個**——`pingora-rustls` 那兩個是 G41 新增的，
但 **workspace 的 `Cargo.toml` 從建立 fork 那天起就一直在改動之列，卻從來沒被算進去**。
漏算的原因就是上面那條命令：**它只 diff `pingora-core` 一個目錄**，結構上看不見另外幾個。
★ 判據夠不到的東西，不會因為沒人提就不存在。

| 升級 | 有上游參照物嗎 | 遷移完整性 |
|---|---|---|
| **`nix`** 0.24→0.31 | ★ **有**（main 已抬到 `~0.31.1`）| ✅ **已逐行核對**：兩個檔案裡所有觸碰 nix API 的行**與上游 main 完全一致**，無遺漏 |
| `rand` 0.8→0.10 | ✗ main 仍是 `0.8` | 改動 3 個檔案；`pingora-runtime` 與 `connectors/l4` 的單元測試覆蓋到 |
| `sfv` 0.10→0.15 | ✗ main 仍是 `0.10.4` | ⚠ 見下 |
| `brotli` 3→8 · `lru` · `prometheus` · `strum` · `daggy` | ✗ | ★ **無原始碼改動**（升級後簽名未變），不存在「遷移的另一半」 |
| `derivative`→`educe` | ✗（main 仍用 `derivative`）| 只是 `Debug` 派生宏的等價替換，5 處 |

★ **核對時排除了兩條假信號**：main 裡的 `close_unclaimed()` 是**新功能**（`f82478ae`，未認領 fd 黑洞化那條修復，0.8.1 裡不存在），不屬於 nix 遷移，登記在 [尚未驗證的接縫](../../docs/verification/open-seams.md)；另一條是 `use std::os::fd::AsRawFd;` 被寫進了合併的 `use` 語句裡，逐行比對會誤報。

### ⚠ 殘餘風險：只剩 `sfv` 一條值得記名

它是唯一同時滿足四條的：**① 改了原始碼 ② 上游沒做過、無參照物 ③ 輸入是用戶端可控的 `Accept-Encoding` ④ 單元測試只覆蓋良構輸入**（`test_accept_encoding_req_header` 測的是空值、`"gzip"`、`"what, br, gzip"`，沒有畸形輸入、沒有 q 值）。

★ **但後果面已經查清並且很淺**：`compression/mod.rs` 的解析失敗分支是 `warn!` + **保持演算法列表為空**——也就是說，如果 sfv 0.15 對某個畸形頭比 0.10 更嚴格，最壞結果是**該回應不壓縮**。不是崩潰，不是錯誤內容，不是安全問題。

**在這個後果面下，不值得為它再做什麼。** 若將來 `sfv` 出現在別的、後果更重的路徑上，這條要重新評估。

## 驗證

```bash
bash tests/m0/docker-run.sh                 # 建置 + fork 迴歸網 + M0
VENDOR_ONLY=1 bash tests/m0/docker-run.sh   # 只跑 fork 迴歸網（rebase 後的第一道門）
```

迴歸網有兩層，**缺一不可**：

| 層 | 驗什麼 | 入口 |
|---|---|---|
| **上游自帶的單元測試**（340 條） | 抬過上界的那些庫，在 pingora 自己的程式碼裡還對不對 | [`tests/vendor/run.sh`](../../tests/vendor/run.sh) |
| **M0 接縫驗證** | 優雅升級時三類流量零中斷 | [`tests/m0/run.sh`](../../tests/m0/run.sh) |

判據不是「零失敗」，而是 ★ **失敗集合與官方原版 0.8.1 逐項相同**——這個容器裡有 3 條測試在官方原版上同樣失敗（相依容器外的網路行為）。名單寫死在 `tests/vendor/run.sh` 裡，**多一條少一條都判紅**。

### ★ ★ 這一層是 2026-08-13 才補上的，補上當場就逮到一條真迴歸

**在那之前，「M0 是這次改動唯一的迴歸網」是字面屬實的**，而且比字面更糟——**上游那 340 條測試當時根本編譯不過**：

1. `pingora-core/Cargo.toml` 還指著三個沒被 vendor 進來的 TLS 後端。★ 這個坑**在建置時完全不顯形**：作為根 workspace 的 **patch 相依**，cargo 不去讀未啟用的可選 path 相依的清單；一旦作為 **workspace 成員**載入就必須全讀。
2. `transfer_fd` 的測試程式碼裡有 4 處 `nix` 0.31 的類型錯誤——lib 改了，測試沒改，而測試從沒被編譯過。

清掉這兩條之後跑出來的結果，見下節。

## 維護

★ **上游每次發版都要重做一遍這裡的改動**——這正是 `PLAN.md` §9 那條「Pingora 上游演進與樞衡客製的合併成本」，G30 把它從一條風險變成了一項進行中的成本。

### 上游有沒有動，由指令碼盯著

★ ★ **不要靠人記得去看。** patch 過去的 crate **脫離了 `cargo update` 的視野**，上游發了新版不會有任何提示。`tools/dep-check.py` 因此**單獨查一次上游** `pingora-core`，與本 fork 的基線比對：

```bash
python tools/dep-check.py
```

```text
── fork 上游检查 ──
fork 基线 pingora-core 0.8.1 仍是上游最新，无需 rebase。
```

- **基線從哪來**：★ 直接讀本目錄 `pingora-core/Cargo.toml` 的 `version`——**不另設一份要手動同步的記錄**
- **結束代碼 20** = 上游領先（與 cargo 那條疊加，兩者都有則 30）
- ★ **它只報告、絕不自動採納**——rebase 一個 fork 不是指令碼該無人值守去做的事

### ⚠ ⚠ ★ ★ ★ 上游 `main` 與發佈 tag 是 **diverged**，別把 main 當成「tag + 更多」

2026-08-27（§10 第 75 輪）實測 `GET /compare/0.8.1...main`：

```
status : diverged        ahead_by : 190        behind_by : 8
merge_base : faac65b0c2  ← 那是 0.8.0（2026-03-02）
```

⇒ **兩條線在 0.8.0 分了叉。** 那 8 個「只在 tag 上、不在 main 上」的提交裡有：

- `Bound default HTTP/2 server limits to mitigate memory exhaustion`
- **`RUSTSEC-2026-0098 and RUSTSEC-2026-0099 fixes`**

⚠ ⚠ **把 fork 挪到 `main`、或按 `main` 重做 vendor，會靜默丟掉這兩條安全修復。**
✅ 我們從 **0.8.1** vendor，所以它們**我們有**；受威脅的只是「以後照 main 對齊」這個動作。

★ 本節此前只有下面「rebase 的步驟」裡那句「先看上游有沒有已經自己抬了某幾條」，
而 `dep-check.py` 當時只報「main 領先 N 個提交」—— **一句會被讀成「main ⊇ tag」的話**。
✅ 已修：它現在報 `diverged / 领先 / 落后`，並把只在 tag 上的那幾條**逐條列出來**。

#### ⚠ 「提前撈能省一輪遷移」這句話，2026-08-27 第一次被量了 —— 量出來是零

把本檔案表裡每一條上界與 `main` 逐項比過（取 `raw.githubusercontent.com` 上 8 份清單）：

| | 我們 | 上游 `main` | 結論 |
|---|---|---|---|
| `brotli` | `8` | `3` | main **更低**，無可撈 |
| `rand` | `0.10` | `0.8` | 同上 |
| `sfv` | `0.15` | `0.10.4` | 同上 |
| `x509-parser` | `0.18.1` | `0.16.0` | 同上 |
| `prometheus` | `0.14` | **這條相依不在了**（零命中）| ⚠ ⚠ ★ main 把它從 `pingora-core` **整條刪了**。⇒ 上游下次發版時：本檔案 §1 那條 `prometheus` 上界（**RUSTSEC-2024-0437** 的修法）會隨相依一起消失，而**上游自帶的 `prometheus_http_app` 也會消失**。★ 我們的產品程式碼**一行都不碰它**（任何一份 `crates/*/Cargo.toml` 裡都沒有 `prometheus` 這個相依），所以不擋路。✅ **那條「有保質期」的路已經不用走了**：M2 批 M 的指標端點按 **G117 自研 text exposition、零新相依**，與上游那個 `prometheus_http_app` 無關 —— 它消不消失都碰不到我們。⚠ 判據要按相依寫，別按文字寫：`crates/` 下現在**搜得到** `prometheus` 這個詞（都是講我們自己那個端點的註解與文件） |
| `lru` | `0.18.2` | `0.18.2` | ✅ 已一致（我們的 PR）|
| `boring` / `boring-sys` | `4.5` | **`5`** | ⛔ **不能撈**，見下 |

⛔ **`boring` 4 → 5 是唯一一條 main 真的領先的，而它不能撈**：`quiche` 0.29.3 要
`boring = "^4.3"` ⇒ 抬到 5 會讓圖裡出現**兩份 `boring`/`boring-sys`**，
也就是一個二進位裡鏈兩套 BoringSSL。★ 守住這條的**不是任何一道門，是編譯器**
（`crates/fulcrum/tests/supply_gates.rs` 自己寫著：兩道門按包名去重，
答不了「同一個名字有幾個版本」）。

⚠ 而 `main` 其餘的相依漂移方向是 **Cloudflare 自己的內部需要**：
`aws-sdk-s3` · `dial9-tokio-telemetry` · `daemonix`（替掉 `daemonize`）· `flurry` ·
`tikv-jemallocator`。⇒ 撈過來等於把別人的部署形態搬進我們的相依圖。

> ★ ★ **「提前撈能省一輪遷移」不是一句錯話，是一句從沒被量過的話。**
> 它成立的前提是「上游在往我們要去的方向走」，而 `main` 今天走的是另一個方向。

### rebase 的步驟

1. 取上游新 tag，比對本檔案「改了什麼」兩節
2. ★ **先看上游有沒有已經自己抬了某幾條**——`nix` 這一次就是白撿的
3. 重做剩下的上界改動，逐個修呼叫點
4. ★ **把 `pingora-core/Cargo.toml` 裡沒被 vendor 的 TLS 後端（G104 之後只剩 openssl / s2n 兩個）
   連同 `[features]` 裡對應兩條一起刪掉**——不刪，任何直接對 vendor 樹跑的 cargo 命令都會失敗，
   而建置時完全不顯形（理由見該檔案內的註解）。★ 0.9.0 起再加一組：`dial9*` 三個特性與
   pingora-runtime 的兩個可選相依（理由見文首「rebase 到 0.9.0」一節）。
   ★ 刪完把原始碼裡仍有 `cfg(feature = …)` 引用的那幾個名字登記進 workspace 清單的 `check-cfg`
   （現為 openssl / s2n / dial9 / dial9-worker-s3）—— 否則編得過，但每次編譯刷 58 條警告，真的警告被淹掉
5. ★ ★ **跑 `bash tests/m0/docker-run.sh`**（迴歸網 + M0，兩層都要）

★ ★ **2026-09-24 那次 rebase 補的四條**（每條都是當場踩出來的）：

- **匯出上遊樹用 `git -c core.autocrlf=false archive <tag>`**：本機全域 `autocrlf=true`，不加 `-c` 匯出的是 CRLF。
- **官方失敗名單要重測**：在門禁同款容器（`fulcrum-build` 鏡像 · `NET_ADMIN` · 192.0.2.0/24 DROP）裡對官方新 tag 跑
  `cargo test -p <8 个 crate> --features pingora-core/rustls --no-fail-fast`，按結果改 `tests/vendor/run.sh` 的
  `EXPECTED_FAILURES`（兩個方向都嚴：名單外的失敗判紅，名單裡的「現在過了」也判紅）。
- **鎖定檔只增量補、逐個過懷疑期**：⛔ 不刪鎖重生成（那會把所有包挑到最新、繞過 G29 的 24 小時）。
  ⚠ 根鎖**不能**只跑一次 `cargo metadata` 做最小重解析：鎖裡釘著 path 版舊 pingora 時，cargo 會轉去 crates.io
  取**官方原版**舊版本（本次實測：官方 pingora-core 0.8.1 連同 prometheus 被拉回了相依圖）⇒ 要點名
  `cargo update -p pingora-core -p pingora-http -p pingora-boringssl …`，再逐包核新增版本的發佈時間。
- **鎖操作一律在建置鏡像的一次性容器裡跑**（G107）：宿主機跑 cargo 會留下 `target/.rustc_info.json`，
  G107 那道「儲存庫裡的 target/ 必須為空」當場攔。

★ ★ ★ **第 3 步有個必踩的坑，2026-08-12 就踩了一次**：**「改到編譯通過」不等於「遷移做完了」。**
`nix` 那一輪真正傷人的兩條（`cmsg_space!` 的 `len` 語義、`.clear()` → `.fill(0)`）**編譯器一個字都不會說**。
所以第 3 步的正確做法是 ★ **把上游 main 對同一個檔案的 diff 通讀一遍**，而不是隻修回報錯誤的行：

```bash
git -C <上游克隆> show FETCH_HEAD:pingora-core/src/protocols/l4/stream.rs
```

★ 第 5 步是這個坑唯一的備援——**上游那 340 條測試是白送的，它當天就逮到了這條**。

降低成本的路子 —— ⚠ ⚠ **這裡原先寫著「兩條路子，屬 D12，尚未拍板」，那是過期的**
（2026-09-08 更正）。**`D12` 早已結案，結論是 `G32`**，而且它不是「二選一」，是**按判據分流**：

> **G32：fork 長期維護 = 把有公告支撐的上界推 PR 給上游，其餘常年 rebase。**

⚠ `PLAN.md` §11 自 2026-09-07 起**整張表是空的**，`D12` 不在裡面。
⛔ 別再把這裡讀成一個待辦。（⚠ `tools/plan-refs.py` 判據 ② 只認「§11 的 Dnn」那種寫法，
本處這種「屬 D12」它**射程外**——它自己的頭注寫明瞭「不許讀成 D 號引用全都有門守著」。）

★ ★ ★ **而 `G32` 的「推 PR」那一半，2026-09-08 一查發現已經沒有目標了**：

| 有公告支撐的兩條 | 我們 | 上游 `main` |
|---|---|---|
| `lru` | `0.18.2` | **`0.18.2`**（根清單，已 workspace 化）|
| `prometheus` | `0.14` | **`0.14`**（已拆成獨立的 `pingora-prometheus` crate）|

⇒ 兩條**上游都自己抬到位了**，與 `nix` 那次一樣是白撿的。**沒有 PR 可推**，
`G32` 剩下的就只有「常年 rebase」那一半 —— 而那正是現狀。
★ 這同時意味著：**下一個 tag 一發布，這幾條上界改動就能歸零**。
⚠ ⛔ 但別為此去 rebase 到 `main`：`0.8.1` 仍是最新 tag，而 `main` 與它**是 diverged 的**
（2026-09-08 實測 ahead 190 / behind 8），本檔案上文那條「別把 main 當成 tag + 更多」照舊成立。
✅ **2026-09-24：兌現了** —— 0.9.0 發佈（2026-09-09），本 fork 已 rebase 上去，那幾條上界隨之歸零，見文首。
⚠ 「別對齊 main」照舊成立：0.9.0 之後 main 又多了 5 個提交，本次一個都沒撈。

★ **重做時先看上游有沒有已經自己升了**——`nix` 與這兩條都是白撿的。

## 已知的輕微行為差異

★ 有意的一處（2026-10-14，改動 17 / G162）：`ETag` · `Vary` 等七個頭名在 h1 上照標題化表寫 —— 壓縮層用常數改寫 / 追加的
`etag` / `vary` 因此從小寫變成 `ETag` / `Vary`，與上游不同。詳見改動 17。

下面這一處與上游**相同**，只是記下來：`get_fds_from()` 裡 `msg.cmsgs()?` 在控制緩衝被截斷（`MSG_CTRUNC`）時會提前回傳，**跳過 `unlink(path)` 的清理**，留下一個殘餘的 socket 檔案。上游 main 的行為完全相同。這條路徑在正常升級中不出現（緩衝按 `MAX_FDS = 32` 預留），記在這裡只是為了不讓它成為將來某次排查的意外。
