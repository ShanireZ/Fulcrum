---
type: 驗證記錄
title: rustls 接縫 · 打開 G6 的那個 feature 會發生什麼
description: 三道門結構上都夠不到它；打開後一次進來 42 個從未受審的 crate，其中 aws-lc-rs 被無謂編進產物。
resource: ../../vendor/pingora/pingora-rustls/Cargo.toml
tags: [驗證, 已通過, 必讀, 易錯, 供應鏈, TLS]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-14T00:00:00Z
sources:
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 1 條硬約束（TLS 後端鎖死 rustls）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G6（統一 rustls）、G13（musl 靜態二進位）、G30（fork 的由來）、G41（本輪）
---

★ **本頁記的是實際跑出來的東西，屬歷史事實。** 結論可以被後續實驗推翻，但「那幾次跑出了什麼」不會變。

> **起因是一條早就寫在文件裡、卻沒人動的待辦。** [`TLS 与自动 HTTPS`](/architecture/tls.md)
> 自己登記著：「`pingora-core` 的 `default = []`，rustls 後端要顯式開 `features = ["rustls"]`……
> **G6 這條決策目前在 `Cargo.toml` 裡還沒有任何表達**——M1 第一天就要補上。」
>
> 去補的時候發現，它不是補一行 feature 的事。

# 一、★ ★ ★ 這條接縫從來沒有被編譯、測試或稽核過

**三道門各自都夠不到它，而且都不是疏忽，是結構。**

| 門 | 為什麼夠不到 | 判據 |
|---|---|---|
| G30 的上界稽核 | 跑在**根 workspace** 上 | 根 `Cargo.lock` **不含** `pingora-rustls`／`rustls`／`ring`／`rustls-native-certs` |
| `tests/vendor/run.sh` 迴歸網 | `cargo test` **不帶任何 `--features`** → `default = []` | rustls 那一半程式碼一行都不跑 |
| `tools/supply-audit.py` | 同第一條 | 同第一條 |

★ **「根 lock 不含它」不是特例**：實測 `sentry`／`x509-parser`／`ouroboros` 同樣缺席——
**未啟用的可選相依一個都不在根鎖裡**。也就是說這個盲區的形狀是通用的，不只針對 rustls。

**實測代價**：

```
cargo tree -p pingora-core -e normal            → 133 个 crate
cargo tree -p pingora-core --features rustls    → 175 个 crate
```

> ★ ★ **M1 第一天打開那個 feature，產品相依圖會一次進來 42 個從未過任何一道門的 crate。**

# 二、一個具體缺陷：明明用 ring，卻把 aws-lc-rs 一起編了進去

`pingora-rustls` 的清單兩處都沒寫 `default-features = false`：

| 相依 | 上游寫法 | 那個 crate 的 `default` |
|---|---|---|
| `rustls` | `{ version = "0.23.12", features = ["ring"] }` | `["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]` |
| `tokio-rustls` | `"0.26.0"` | `["logging", "tls12", "aws_lc_rs"]` |

★ **兩扇門，都通向 aws-lc-rs**——只關一扇沒有效果。

而這個 crate 自己裝的是 **ring**：

```rust
pub fn install_default_crypto_provider() {
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}
```

crate 裡另一處密碼學呼叫是 `ring::digest`。**aws-lc-rs 編進去了，一行都不會被呼叫。**

## 代價不是「多兩個 crate」

| | 原始碼體積 | 怎麼編 |
|---|---|---|
| `ring` | 8.5 MB | Rust + 少量彙編 |
| **`aws-lc-sys`** | ★ **69 MB** | **C（AWS-LC，BoringSSL 分支），走 cmake** |

這直接頂到 G13 的 **musl 靜態二進位**散佈，也是白送的攻擊面。

## 修法與實測

```toml
rustls       = { version = "0.23.12", default-features = false,
                 features = ["ring", "logging", "std", "tls12"] }
tokio-rustls = { version = "0.26.0",  default-features = false,
                 features = ["ring", "logging", "tls12"] }
```

| 判據 | 結果 |
|---|---|
| 編譯 | ✅ 通過 |
| 相依圖 | 175 → **173**，★ **只少了 `aws-lc-rs` 與 `aws-lc-sys`**，別的一個沒動 |
| 迴歸網 | stock 與改後**逐項相同**（跑了對照組，見下）|

★ **唯一被丟掉的預設項是 `prefer-post-quantum`，而它是安全的**——
`prefer-post-quantum = ["aws_lc_rs"]`，**它自己就是 aws-lc-rs 的一扇門**；
rustls 原始碼裡每一處 `#[cfg(feature = "prefer-post-quantum")]` 都在 `src/crypto/aws_lc_rs/` 下。
**ring provider 之下它是空操作。** ★ 這是讀原始碼確認的，不是推斷。

★ 按 G32 查過上游：`main`（`0046038`）的清單**一字未改**，全倉也沒有別處關掉過 rustls 的預設
（唯一一處 `default-features = false` 是 `pingora/Cargo.toml` 裡的 `reqwest`，無關）。
→ **投稿三**2026-08-19 發出：issue [#965](https://github.com/cloudflare/pingora/issues/965)
→ PR [#966](https://github.com/cloudflare/pingora/pull/966)。

## ★ ★ ★ 2026-08-19 補：其實是**三扇門**，而第三扇是判據結構上夠不到的

發出去當天，上游 Codex 複審提了一條 P2：`pingora-core` 自己的 `[dev-dependencies]` 裡
還有一行裸的 `rustls = "0.23"`。實測屬實，**而且比它說的更寬**——那條 dev-dependency
**是無條件的**，所以不只影響 rustls feature 建置：

| `cargo tree -p pingora-core`（唯一 crate 數）| 上游 main | 只關前兩扇 | 三扇都關 |
|---|---|---|---|
| `--features rustls -e normal` | 178，aws-lc 在 | **176，沒了** | 176，沒了 |
| `--features rustls -e normal,dev` | 260，在 | **260，仍在** | **258，沒了** |
| 無 feature，`-e normal,dev` | 228，在 | **228，仍在** | **226，沒了** |

> ★ ★ ★ **它此前查不出來，不是因為沒人想到，是因為判據結構上夠不到**：
> 本頁上下所有測量都用 `cargo tree -e normal`，而 **`-e normal` 按定義排除 dev-dependencies**。
>
> **這是本專案第三次撞上同一個形狀**——
> 第一次是本頁第一節（G30 上界稽核／迴歸網／`supply-audit.py` 三道門都夠不到 `pingora-rustls`），
> 第二次是 G43（`FORK.md` 的核對命令只 diff 了 `pingora-core` 一個目錄），
> 第三次就是這裡。**三次的共同點是：那道門當時都是綠的。**

★ **落法上刻意不給 dev-dependency 指定任何 provider**（只留 `logging`/`std`/`tls12`）：
它只被 `connectors/http` 的測試拿去用類型，執行環境的 provider 本來就經 `pingora-rustls` 進來。
帶 `ring` 的寫法也量過——**相依圖一模一樣**，所以不指定不損失什麼，卻能不和上游
[#630](https://github.com/cloudflare/pingora/pull/630) /
[#887](https://github.com/cloudflare/pingora/pull/887) 的 provider 選型工作打架。

⚠ **代價已經付了 5 天**：fork 的 `vendor/pingora/Cargo.lock` 裡 `aws-lc-rs`/`aws-lc-sys`
此前一直是 2 條，修完變 **0** 條——**G41 之後每一次迴歸網都在白編那 69 MB 的 C 原始碼**。

# 三、★ 對照組：失敗集合確實變了，但不是這次改動造成的

打開 rustls 會**新增 18 條通過、5 條失敗**。判斷這 5 條的歸屬必須有對照組，否則
「是我改壞的」與「是這個 feature 本來就帶的」分不開。

| | passed | failed | ignored |
|---|---|---|---|
| 不開 rustls（原基線）| 335 | 3 | 2 |
| 開 rustls · **stock 清單** | 353 | **8** | 3 |
| 開 rustls · **改過的清單** | 353 | **8** | 3 |

**5 條 rustls 特有的失敗在兩組裡完全一致** → 與清單改動無關。

★ 兩次跑之間失敗集合有 ±1 的抖動，成員在 `connectors::l4::tests::test_conn_timeout` 與
`protocols::l4::stream::tests::test_rx_timestamp` 之間變——**恰好就是儲存庫裡已登記／已註明
不穩定的那兩條**，符合 `tests/vendor/run.sh` 早就寫下的「重跑一次再定性」紀律。

# 四、★ ★ ★ 根因查到了：`192.0.2.1` 在 Docker 網路裡**有人應答**

5 條新失敗全是同一句 `should throw an error`，全部走同一個常數：

```rust
const BLACK_HOLE: &str = "192.0.2.1:79";   // pingora-core/src/connectors/mod.rs:492
```

它們設 1ms 連接超時，等 `ConnectTimedout`。**而這個位址在容器裡連得上**：

```
192.0.2.1:79  CONNECTED in 1.7ms   ← RFC 5737 TEST-NET-1，本该不可达
```

★ ★ **已登記的那條 `connectors::l4::tests::test_conn_timeout` 是同一個根因。**
它當初被登記成「環境性失敗」是對的，但**根因一直沒人查**——
而一旦查了，它是**可以被修掉**的。

## 修法：把環境修對，而不是把名單加長

```bash
iptables -A OUTPUT -d 192.0.2.0/24 -j DROP     # 需要 --cap-add=NET_ADMIN
```

| | passed | failed | 已知失敗名單 |
|---|---|---|---|
| 修環境前 | 352 | 9 | 3 條 |
| **修環境後** | ★ **359** | ★ **2** | ★ **2 條** |

★ **只有 `DROP` 有用。** `ip route add blackhole` 給的是立刻 `EHOSTUNREACH`——
那是 `ConnectError` 不是 `ConnectTimedout`，測試照樣紅。**必須讓 SYN 被靜默丟棄。**

⚠ **代價**：那批測試現在真的會等滿自己的超時，迴歸網從 ~30s 變成 ~136s。
**這是判據變真實的價錢**——此前它們「快」，是因為它們根本沒在測超時。

## 這道門自己要能證明前提成立

`tests/vendor/run.sh` [3/4] 會在跑測試前**自證黑洞**，三個分支都反證過：

| 情形 | 實測 | 結果 |
|---|---|---|
| 沒裝規則 | 6ms 連上 | ✅ 判紅，並給出裝規則的命令 |
| 用 `REJECT`（立刻失敗）| 1008ms 結束代碼 1 | ✅ 判紅，並說明為什麼 `blackhole` 路由不行 |
| 用 `DROP` | 2004ms 超時 | ✅ 通過 |

★ ★ **判據掛在行為上，不掛在「規則在不在」上**：`iptables -L` 列得出規則，不等於它生效
（容器可能沒 NET_ADMIN、核心模組可能缺、可能被前面的鏈搶先 ACCEPT）。
**能證明它生效的只有「連過去真的會超時」這一件事。**

# 五、`rustls-native-certs` 卡在兩年前

| | 值 |
|---|---|
| 上游需求 | `^0.7.1` |
| 實際鎖定 | `0.7.3`（**2024-08-28**）|
| 最新 | `0.8.4`（2026-06-01）|
| `0.8.0` 發佈於 | ★ **2024-08-29——鎖定版本的第二天** |

**正是 G30 存在的那個「脫字號自帶上界」形狀，長在稽核夠不到的 crate 裡。**

## ★ ★ 呼叫點適配差點寫錯，這條最值得記

0.8 的 `load_native_certs()` 不再回傳 `Result`，改為回傳 `CertificateResult { certs, errors }`——
**部分成功從此是可表達的狀態**。直覺寫法是「`errors` 非空即失敗」。

**但那比 0.7.3 更嚴。** 0.7.3 的 `CertPaths::load()` 結尾寫著：

```rust
// promote first error if we have no certs to return
if let (Some(error), []) = (first_error, certs.as_slice()) {
    return Err(error);
}
```

> ★ **它只在一張都沒讀到時才回傳 `Err`**；只要讀到了哪怕一張，錯誤就被咽掉。

照直覺寫，`/etc/ssl/certs` 裡一個讀不動的檔案就能讓反代**起不來**。
**那是行為變更，不是適配**——而 fork 的紀律是「意圖嚴格限定為放寬上界 + 隨之而來的適配」。

落法：`certs.is_empty() && !errors.is_empty()` 才判失敗；`errors` 裡的內容改為 `warn!` 一行——
**控制流與 0.7.3 完全一致，只是不再無聲**（0.7.3 是直接丟掉）。

連帶：`0.8.4` 自帶 `openssl-probe 0.2.1`，所以最終相依圖是 **174** 而不是 173。

# 六、可以帶走的幾條

1. ★ ★ ★ **「登記為環境性失敗」是把問題掛起，不是把問題解決。**
   名單裡每一條都欠著一次根因調查——不查，就永遠不知道它其實能被修掉。
   這次查了一條，名單就短了一條，而且門變嚴了。
2. ★ ★ **一個從沒被編譯過的 feature，等於一片沒有任何判據覆蓋的程式碼**，
   哪怕它在決策紀錄裡被鎖死了兩個月（G6）。**決策不會自己變成建置設定。**
3. ★ ★ **上游 crate 的 `default` feature 是一條靜默的供應鏈入口。**
   `features = ["ring"]` 讀起來像「我選了 ring」，實際是「我在預設之外**又加了** ring」。
4. ★ **遷移到「更好的 API」時，要先把舊 API 的真實語義讀出來**，
   而不是照著新 API 的形狀寫。新 API 把錯誤暴露出來，不等於舊行為就是「有錯即失敗」。
5. ★ **判據的覆蓋面必須等於被問的範圍**：`FORK.md` 問「fork 改了哪些檔案」，
   而它的核對命令只 diff 了 `pingora-core` 一個目錄——於是連它自己結論段裡列著的
   兩個 `pingora-runtime` 檔案都看不見，workspace 清單更是從建立 fork 起就沒被算進去過。

# 相關

[TLS 與自動 HTTPS](/architecture/tls.md) · [供應鏈現狀](/platform/supply-chain.md) ·
[fork 說明](../../vendor/pingora/FORK.md) · [上游投稿](../../upstream-pr/README.md) ·
[尚未驗證的接縫](/verification/open-seams.md)
