# 投稿六（✅ **issue 與 PR 都已發**）· 監聽器上的連接生命週期

> ✅ **2026-09-04 經 owner 逐次授權（G40）發出**：
> issue [cloudflare/pingora#994](https://github.com/cloudflare/pingora/issues/994)。
> ★ 正文是 `scratchpad` 裡那份提取產物**逐字發出**的，發後核過：
> 歸一行尾後與批准的那份**逐字元相等**（9709 == 9709），中文字 0 個。
> ⚠ 完成實作口徑：**只發 issue**。為什麼現在不發 PR，見 §2（那一節的理由一條都沒變，
> 只是又多了一條：**我們 fork 今天的形狀與本 issue 提的形狀不是同一個**）。

> 對應 fork **改動 15**（[`../vendor/pingora/FORK.md`](../vendor/pingora/FORK.md)）。
> 依據 `PLAN.md` §10 **G122**：「投不投上游**等 rebase 讀過上游 `main` 之後再判**」。
> ⛔ **本輪只備材料、只做 G32/G46 那三項必查，不發 issue、不發 PR**（發要 owner 授權，G40）。

---

## ⚠ ⚠ ⚠ 讀下面任何一節之前先讀這一節：**發出去之後自己查出一處錯，已公開更正**

**2026-09-04，#994 發出之後**，在備補丁時讀上游程式碼，查出我們（我）在 issue 正文裡
寫錯了一句 —— 已在 #994 下發**更正評論**
（[issue-comment-5536326108](https://github.com/cloudflare/pingora/issues/994#issuecomment-5536326108)，
owner 單獨批准）。

**錯的那句**：「a `Tracer` is a field on `PeerOptions` … **there is no equivalent for
connections from downstream**」，以及開頭那句「a process … **cannot** report how many
downstream connections it currently has」。

**事實**（當天實查，上游 `main` = `09696b5`）：

| 事實 | 出處 |
|---|---|
| `Stream` 上有 `pub tracer: Option<Tracer>` | `protocols/l4/stream.rs:440` |
| **`Stream::drop` 就在調 `on_disconnected()`** | `protocols/l4/stream.rs:706-710` |
| 出站連接器給它賦值：`t.0.on_connected(); stream.tracer = Some(t);` | `connectors/l4.rs:221-225` |
| `Tracer` 實作了 `Clone`（`boxed_clone`）| `upstreams/peer.rs:83` |
| `ServerApp::process_new(self, **mut session: Stream**, …)` | `apps/mod.rs:51-56` |

⇒ ★★★ **機制本來就是通的，缺的只是「監聽器那側沒人給它賦值」。**
而且因為 `process_new` 遞的是 `Stream` 本體，**實作 `ServerApp` 的應用今天就能自己
`session.tracer = Some(t)`** 拿到按連接的 disconnect 通知 ⇒ 我那句「cannot」太強了。

**我錯在哪 —— 這一條值得記住**：我做過的**普查是對的**（`listeners/` 與 `services/` 裡
`tracer` 零命中，逐檔案查實過）；⛔ **錯的是解讀** —— 我把「這兩個目錄裡沒有」讀成了
「不存在對應機制」，而正確的結論是「**機制在共用的 `Stream` 類型上就有，只是只在出站那側
接了線**」。★ 判據沒壞，是我從判據跨到結論時多走了一步。

**仍然站得住的兩條**（更正評論裡保留的就是它們）：
① **accept→握手 那段視窗**應用看不見（`handle_event` 只在 `Ok` 那支被調）⇒ 那些連接數不到；
② 按監聽器分格今天沒有（留給 #941）。

⇒ ★ **更正之後要求變得小得多**：不要新方法，只要**監聽器側能掛 `Tracer`**。
補丁已按這個形狀備好（§2.2）。

---

## 0. ⚠ ⚠ 先說兩件把這份材料的形狀改掉的事（2026-09-03 實測）

### ① ★★★ G122 裡那句「上游 `main` 已把 `prometheus` 整條刪掉」，在今天的 `main` 上**不成立**

實測（克隆 `https://github.com/cloudflare/pingora.git`，`main` = **`09696b5`**，2026-08-24）：

| 查什麼 | 實測 |
|---|---|
| `pingora-prometheus/src/lib.rs` | **在**，131 行 |
| 誰依賴它 | `pingora/Cargo.toml:48` 與 `pingora-proxy/Cargo.toml:53`（`pingora-prometheus = "0.8.0"`），外加 `prometheus = "0.14"` |

⚠ ⚠ **這不是「G122 錯了」，而是「G122 的一條前提今天需要 owner 重新看一眼」**：
那句話是**投不投上游**這個判斷的**全部理由**（「口味未知」）。前提變了，判斷的依據就變了。
⛔ **我沒有替它改結論** —— 投不投仍然是 owner 的決定。
★ 順帶：本儲存庫那條老紀律在這裡第 N 次兌現 ——
**一條待辦躺著的理由，常常不是它難，而是它被綁在一個沒人回頭核的前提上。**

#### ①a ★★★ 重判材料（owner 2026-09-04 點名要「重新看一眼」）：**那句話是一次重構被讀成了刪除**

`pingora-prometheus` 這條路徑上**只有一個提交** ——
[`842ddd9`](https://github.com/cloudflare/pingora/commit/842ddd9)（2026-04-01）
**"Split out pingora-prometheus into a separate crate"**。它**從來沒被刪過，它是那天被拆出來新建的**。

那一筆的檔案清單（`gh api repos/.../commits/842ddd9`）：

| 動作 | 檔案 |
|---|---|
| **removed** | `pingora-core/src/apps/prometheus_http_app.rs`（−66）|
| modified | `pingora-core/src/services/listening.rs`（**−16**）· `pingora-core/Cargo.toml`（−2）· `apps/mod.rs`（−2）|
| **added** | `pingora-prometheus/Cargo.toml`（+22）· `pingora-prometheus/src/lib.rs`（+131）|

⇒ ★★★ **只看 `pingora-core` 的話，prometheus 確實「整條消失」了** —— 那句前提幾乎必定是這麼寫下的。
而在 workspace 層面它是**被搬走**：`pingora-prometheus` 是 workspace 成員，
`pingora` 與 `pingora-proxy` 今天都依賴它，而 `pingora-core` 對它**零引用**。
⚠ 那 −16 行還正好在 `services/listening.rs` —— **fork 改動 15 動的就是這個檔案**。

**⇒ 「口味未知」這個推斷今天有了精確的答案，而且方向對我們有利**

那次拆分把上游的口味說得很清楚：**core 只暴露公開接縫，指標實作住在一個消費它的獨立 crate 裡。**
`pingora-prometheus/src/lib.rs` 整個建在 core 的公開 API 上
（`apps::http_app::{HttpServer, ServeHttp}` · `modules::http::compression` ·
`protocols::http::ServerSession` · `services::listening::Service`）。
★ 而形狀 ② 向 core 要的正是**一個鉤子**，counter/gauge、標籤、命名全留在呼叫方
（草稿裡那句 "Nothing in what we are describing knows about metrics" 就是這個意思）——
**我們站在那條線的同一側。**

**另外三條不依賴 prometheus 的口味證據**（全部當天實測）：

| | 證據 |
|---|---|
| ★★ | `ConnectionFilter`（#671）本身就是**外部貢獻者**加進 core 的監聽器級接縫，走 opt-in feature |
| ★★ | `upstreams::peer::Tracing { on_connected, on_disconnected }` **已經在 core 裡** ⇒ 連接生命週期鉤子在 core 裡是既有形態 |
| ★★★ | **`listeners/` 2026 年一直在動，而且加的正是「接縫」與「按監聽器設定」**：`600c5c0` pre-TLS 回呼（04-03）· `c0845a8` per-listener L4 緩衝設定（05-05）· `79771f5` rustls 的 TlsAcceptCallbacks（06-12）· `8aeef34` 下行 TLS 握手卸載（06-30）· `f82478a` 關掉未被認領的繼承 socket（07-23）|

★★★ **最硬的一條：我們自己的投稿一已經落進 `main`** ——
[`6463ad6`](https://github.com/cloudflare/pingora/commit/6463ad6)（2026-08-14），
author `Shanire <shanire86@gmail.com>`、committer 是維護者、我們的 `Signed-off-by` 原樣保留。
⇒ 「我們投的東西會不會被重放進 main」這件事**已經有一個正例，不再是推測**。

**⚠ 誠實的另一側（⛔ 不給單邊材料）**

1. ⚠ 那次拆分是上游在**讓 core 變小**，而我們要往 core 的一個 trait 上**加兩樣**。
   ★ 反駁是：搬走的是一個 **Prometheus 專用的 HTTP app**，不是接縫 —— 那一刀切在
   「實作」與「接縫」之間，而我們要的是接縫。但這個讀法是真實存在的，⛔ 別裝作沒有。
2. ⚠ ⚠ **真正的風險不是口味，是評審頻寬**：#941（per-address 過濾器）open 六週、**0 條評論**；
   #295 是被 stale 機器人關的；CONTRIBUTING 明說不承諾及時評審。
   ★ 而**我們已發的三份裡只有一份完成實作**：投稿二（fd 洩漏）與投稿三（aws-lc-rs）的主題
   在 `main` 的提交裡**零命中**（⚠ 帶正對照：同一條檢索能搜到已完成實作的那筆 lru）。
   ⇒ 發出去的預期結果是「**它可能就躺著**」。那是時間成本問題，不是口味問題。

**⇒ 要 owner 拍的兩件**（⛔ §10 只有 owner 能改，我一個字沒動）

1. **那句前提本身**：它作為陳述是**假的**。三條路 ——
   (a) 劃掉那句、換成今天的證據說得出的話；
   (b) 劃掉那句，「投不投」改由別的依據來判；
   (c) 行不動，把糾正寫在別處 —— ⚠ 那會讓 §10 留著一句假話，
   **而那正是 G128 / G129 兩次補記要治的毛病。**
2. **投不投** —— 那句前提造出來的那道閘門已經不存在了。

### ② ★★★ 上游**已經有**一個位置幾乎相同的接縫：`ConnectionFilter`

`pingora-core/src/listeners/connection_filter.rs`（feature `connection_filter`，
`pingora-core/Cargo.toml:122` 寫著 `connection_filter = []` ⇒ **預設關**）：

```rust
pub trait ConnectionFilter: Debug + Send + Sync {
    /// This method is called after a TCP connection is accepted but before
    /// any further processing (including TLS handshake).
    async fn should_accept(&self, addr: Option<&SocketAddr>) -> bool;
}
```

呼叫點在 `pingora-core/src/listeners/l4.rs:472–486`，**每條連接一次**，
裝配入口是 `Listeners::set_connection_filter` / `ListeningService::set_connection_filter`。

⇒ **那正是我們 fork 改動 15 裡 `+1` 的那一刻**。

#### ⚠ 但它只覆蓋一半，而缺的那一半正是最貴的

| 我們要的 | `ConnectionFilter` 給不給得了 |
|---|---|
| `+1`（接進來一條）| ✅ 位置逐字相同 |
| **`-1`（連接結束）** | ❌ **沒有對應的鉤子** —— 而 `_active` 那一格的減一**必須**由 `Drop` 守衛做（G122：那個連接任務有三條退出路徑，手寫三處 `fetch_sub` 正是 D18/G66 那個分家形狀）|
| **哪個監聽位址**（`listen` 標籤）| ❌ 只給 peer addr；而一個 `Listeners` 可以有多個監聽位址（`conn_stats.rs` 頂部那條註解正是為此寫的）|

⇒ 立論的形狀因此**不是**「上游缺一個接縫」，而是
**「上游有半個，而另外半個（結束時機 + 監聽位址）恰好是做連接計數必須的那半」**。
★ 這比原來那份「我們加了一個新 trait」的說法**更有說服力**，也更容易被接受 ——
它變成了「把已有的 `ConnectionFilter` 補完」而不是「再開一個平行的擴充點」。

---

## 1. ⏳ 因此這份材料**現在停在這裡**，等 owner 拍三件事

| | 要拍的 | 為什麼不能由我代拍 |
|---|---|---|
| ★★★ | **投不投**（G122 那句前提已經不成立，見 §0①）| 那是 §10 的決定 |
| ✅ | ~~**投什麼形狀**~~ —— **owner 2026-09-04 拍板：形狀 ②**（給 `ConnectionFilter` 補上結束鉤子與監聽位址）。正文草稿在 §5 | 已決 |
| ★ | **要不要順帶提 feature 預設關**：`connection_filter` 預設關 ⇒ 想用的人要顯式開 feature。若我們的形狀掛在它上面，這一點必須在 issue 裡說清 | 涉及要不要建議上游改預設，那是口味 |

## 2. ⛔ 為什麼這一輪**沒有**生成補丁

按本目錄 [`README.md`](README.md) 的體例，`.patch` 必須是**在上游 `main` 上重播驗證過**的
`git format-patch` 輸出，並逐項跑過上游 `.github/workflows/build.yml` 裡那幾道門。

⚠ 而今天：

1. **基線動了** —— 前四份投稿全基於 `0046038`（2026-08-07），今天 `main` 是 `09696b5`（08-24）。
   ★ 投稿四那次就因為基線/格式問題**重新生成過一遍**，教訓記在 README 裡。
2. **形狀未定**（§1 第二行）—— 在「新接縫」與「補完 `ConnectionFilter`」之間沒拍板之前，
   生成出來的補丁多半是要作廢的那一份。
3. ★ 本目錄的紀律是 **「先發 issue，等回應，再發 PR」**，而 issue 的正文取決於 §1。

⇒ ★ **先把「查過了什麼」釘下來（本檔案），補丁等形狀定了再生成** ——
⛔ 一份基線過期、形狀可能作廢的 `.patch` 擺在這裡，比沒有更糟：
下一個人會以為它是可用的。

### 2.1 ★★★ 2026-09-04 補：**為什麼 issue 發了而 PR 沒發**（owner 問過）

四條理由，前兩條是流程，後兩條是硬事實：

1. **上游 CONTRIBUTING 要求先 issue**：「Non-trivial PRs will also require a GitHub issue」——
   給 core 的一個 trait 加方法不在它列的「錯別字 / 小重構 / 文件」豁免裡。
   本目錄 [`../docs/platform/upstream-pr.md`](../docs/platform/upstream-pr.md) 把這條記成
   硬性流程第 1 條：**直接開 PR 就是違反流程**。
2. **PR 的形狀正是這份 issue 請他們定的東西**：掛 `ConnectionFilter` 還是姐妹 trait ·
   feature 名要不要換 · 同步還是非同步。⚠ 在這三條沒回話之前寫出來的補丁多半要作廢 ——
   本目錄有先例：投稿三在發前複審被推翻、投稿五被整個撤銷。
3. ★★★ **我們 fork 今天的形狀與本 issue 提的形狀不是同一個。**
   fork 改動 15 是一條**獨立的** `ConnectionCounter`（`enter` / `leave`，還帶監聽位址參數）；
   issue 提的是 `ConnectionFilter` 上**一條不帶位址的** `fn connection_closed(&self)`。
   ⇒ **今天沒有可發的補丁**，要發就得先把 fork 改成那個形狀 —— 而那動的是 G122 拍過的設計
   與五個呼叫點。
4. ★★★ **而且那樣改會讓我們自己丟掉 `listen` 標籤。** 我們的指標契約是
   `fulcrum_connections_active{listen,entrypoint}`（G122），而 issue 裡那條鉤子**有意不帶位址**
   ⇒ 在 #941 完成實作之前它給不出按監聽器分格的數。
   ⚠ ⚠ **推論要寫在明處**：即便上游全盤接受，**fork 改動 15 也還得留著**（或留一個更薄的版本）
   直到 #941 完成實作 —— ⛔ 這份投稿不是「把 fork delta 刪掉」的路徑，它是給上游使用者的貢獻。

### 2.2 ✅ 2026-09-04 再補：**補丁已備好**（owner 拍板「現在就備」）

⚠ ⚠ **§2 與 §2.1 的理由被一件事推翻了一半**：更正評論把形狀換成了「讓監聽器側能掛
`Tracer`」（見 §6）—— 那個形狀**不依賴維護者先回話**，因為它重複使用的是上游已有的機制，
沒有新 trait、沒有簽名變更。⇒ 補丁可以現在就備。

**[`0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch`](0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch)**
（基於上游 `main` = `09696b5`，帶 `Signed-off-by`，⛔ **未發**）。

⚠ ⛔ **它是在一個乾淨的上游克隆上做的，`vendor/pingora/` 一個位元組都沒動。**
★ 那是本目錄的紀律：補丁是給上游的，不是我們 fork 的 diff 匯出。

**改了什麼**（只有 `pingora-core/src/listeners/mod.rs` 一個檔案）：
`Listeners` 多一個可選 `Tracer` + `set_tracer()`（形狀照抄它自己的 `set_pre_tls_callback`）·
`TransportStackBuilder` / `TransportStack` 各帶一格 ·
`TransportStack::accept()` 裡克隆一份、調 `on_connected()`、賦給 `stream.tracer`。
結束那一半由**已有的** `Stream::drop` 負責 ⇒ **天生成對**。

★★★ **為什麼它能覆蓋握手視窗**：`handshake()` 的兩條支路都把那個具體的 L4 `Stream`
繼續持有（非 TLS 直接 `Box::new(self.l4)`；TLS 是 `tls_handshake(self.l4)` 包一層）
⇒ accept 時掛上的 tracer 活過握手，握手失敗/超時的連接也照樣報 disconnect。

**驗證（全部在容器裡，⛔ Rust 不在宿主機跑；且用的是那棵樹自己的卷，不碰我們的）**

| 門 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ `RC=0` |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ `RC=0` |
| `cargo test -p pingora-core --lib` | 打補丁 **557 passed / 2 failed**；**基線 555 / 2** |
| `cargo test --workspace --lib --bins --tests`（上游 CI 那條）| 兩側**各 123 條失敗**，⭐ **逐項對比：失敗的測試名完全相同**，差的只有彙總行的 `555 passed → 557 passed` |
| `git am` 重播 | ✅ 在 `09696b5` 上乾淨應用，且**重播樹與開發樹逐位元組一致**（`git diff` 為空）|

⚠ 那兩條基線失敗是 `connectors::l4::tests::{test_conn_timeout, test_bind_to_port_range_on_connect}`；
那 123 條裡的大頭是 `pingora-proxy` 的整合測試，**要 openresty**，容器裡沒有 ——
⛔ 但這句話不是靠「顯然無關」下的，是**回基線量出來的**。
⚠ MSRV 那道（`cargo +1.85.0 check`）與 `cargo audit` / `cargo machete` 本機沒有，留給上游 CI。

### 2.3 ✅ 2026-09-04：**PR 已發** —— [cloudflare/pingora#995](https://github.com/cloudflare/pingora/pull/995)

owner 單獨授權後發出（兩次外部寫入：先把補丁推成 `ShanireZ/pingora` 的
`listener-tracer` 分支，再 `gh pr create`）。完成實作核對：
base `main` · head `ShanireZ:listener-tracer` · **一個檔案 `+135/-1`** ·
**一個提交 `0dda18b`** · 標題一致 · 正文 66 行**逐行相同**。
正文草稿留檔在 [`pr-6-listener-tracer.md`](pr-6-listener-tracer.md)。

**發之前當天重做的核對**（⛔ 不引用上一輪）：#994 仍 OPEN 且**無維護者回復** ·
上游 `main` **沒動**（仍 `09696b5`）· `set_tracer` / `listener tracer` /
`downstream tracer` / `connection tracer` 四個詞**兩條通道全 0**（⛔ 無人搶先）·
`fmt` 與 `clippy -D warnings` **各 `RC=0`** · `cargo test -p pingora-core --lib`
**557 / 2**，兩條新測試 `ok`。

**✅ 上游 CI 四項全綠**（2026-09-04，`gh pr checks` 與後台監視兩次一致）：

| job | 結果 |
|---|---|
| `pingora (1.85.0)` | **pass** 3m39s |
| `pingora (1.97.1)` | **pass** 10m58s |
| `pingora (nightly)` | **pass** 8m7s |
| `semgrep-oss` | **pass** 33s |

★★★ **它正好補上了本機驗證的兩處缺口**：
① `1.85.0` 那道是 **MSRV**（`cargo +1.85.0 check`），本機鏡像裡沒有那個工具鏈；
② `1.97.1` 那道跑的是**帶 openresty 的完整套件** —— 本機那 99 條 `pingora-proxy` 整合失敗
是**環境**造成的，上游跑綠了就此坐實。
⇒ ⛔ 「留給上游 CI」那句話不再是欠賬，它已經兌現。

⇒ ⏳ **下一步：等上游回話。** ⚠ CONTRIBUTING 不承諾及時評審，而 #941 掛了六週 0 評論
⇒ ⛔ **別把它當成會很快有下文的事**。
★ 判斷是否完成實作看**改動有沒有出現在 `main`**，⛔ 不看 PR 狀態（本目錄紀律；投稿一正是
PR 被 close 而改動進了 `main`）。

## 3. ✅ G32/G46 要求的「先查上游做沒做」——已查的部分

> ⚠ G46 修訂過口徑：**「查過上游做沒做」必須包含未合併的 issue 與 PR**，⛔ 不只是查程式碼。

| 查什麼 | 實測（2026-09-03） |
|---|---|
| 上游有沒有同名接縫 | **有半個**：`ConnectionFilter`（見 §0②）；⛔ **沒有**任何 `ConnectionCounter` / 連接結束鉤子 |
| 上游有沒有別處在數連接 | `pingora-core/src/listeners/` 與 `services/` 裡 grep `accepted_?conn` / `conn_count` / `active_conn` —— **零命中** |
| `prometheus` 還在不在 | **在**（§0①）|
| ✅ **有沒有人已經開過 issue / PR** | **已查（2026-09-04）** —— 結果與方法在下面 §3.1。★ 它確實改寫了 §1 第二行。|

### 3.1 ✅ G46 的那一項：查過了（2026-09-04）

**方法（寫下來是為了可重現，⛔ 不是流水賬）**：`gh search issues` 與 `gh search prs`，
20 個查詢串 × **兩條通道** → 34 條去重結果。

⚠ ⚠ **兩條通道是承重的，不是穩妥起見**：**`gh search issues` 一條 PR 都不回傳。**
★ 用正對照測的，⛔ 不是推斷：拿 PR **#962 的原題**（`Bump lru dependency from`）去搜，
`issues` 通道 **0 條**、`prs` 通道 **1 條**（就是它）。
⇒ 只跑 `issues` 的一輪掃描**會答出「沒有前人做過」，而它一個 PR 都沒看過**，
偏偏 G46 要的正是「含未合併的 PR」。
★ 另一條前提也自證過：`--state` 只收 `open|closed`，**省略它才是全狀態**——
自證用 `lru`：issues 10 條中 5 條已關閉、prs 9 條中 7 條已關閉。

| 命中 | 狀態 | 是什麼 |
|---|---|---|
| ★★★ [#118](https://github.com/cloudflare/pingora/issues/118) | **closed，`COMPLETED`，2026-08-25** | 「`"on connect"` phase for incoming connections」—— **逐字就是本接縫**。★★★ 維護者的結案留言是一句話：**「A connection level filter was added with #671.」** |
| ★★ [#295](https://github.com/cloudflare/pingora/issues/295) | closed（**被 stale 機器人關的**，⛔ 不是被解決）| 「想知道此刻有多少 **active** 連接：新連接 +1、結束 −1」。維護者給的是**使用者態 `Drop` 守衛**的寫法，報告者最後一句是「still relevant」|
| ★★★ [#337](https://github.com/cloudflare/pingora/issues/337) | closed，**`COMPLETED`** | 「怎麼跟蹤**用戶端** connect/disconnect（尤其 websocket）」。★★★ 維護者的回話是 **「Disconnect is tracked via the drop of the `Tracer` object」** ⇒ **上游對「怎麼數連接」的既有答案就是 `Tracer`** —— 而它是 `PeerOptions` 上的欄位、數的是**到後端**的連接（`peer.rs:555`，下行三個檔案對它零引用）|
| ★ [#671](https://github.com/cloudflare/pingora/pull/671) | closed（改動已在上游）| `ConnectionFilter` trait 本體，即 §0② 那半個接縫 |
| ★★ [#941](https://github.com/cloudflare/pingora/pull/941) | **open · 0 條評論 · 2026-07-26** | `add_endpoint_with_filter`：把過濾器掛到**單個監聽位址**上 —— 正是「哪個監聽位址」那一半 |
| ⚠ [#897](https://github.com/cloudflare/pingora/pull/897) | open | 標題寫著「resolve #295」，⛔ **看著相關其實是純文件**（只改 `docs/user_guide/index.md`），不是競爭實作 |

**這一輪改變了什麼（三條，⛔ 都不由我拍）**

1. ⛔ **G46 不構成否決**：沒有任何 open 的 issue/PR 在做「監聽器上的連接計數」本身。
   ★ 與投稿五**不同** —— 那次桌上擺著 [#632](https://github.com/cloudflare/pingora/pull/632)
   這樣逐點相同、而且更完整的實作，所以 owner 拍了「什麼都不做」。這次沒有那種東西。
2. ★★★ **但立論必須改寫**：上游在 **2026-08-25**（十天前）剛把 #118 以 `COMPLETED` 關掉，
   理由就是「#671 加了連接級過濾器」。⇒ 再開一份，等於對著維護者說「你們認為已經解決的
   這件事其實沒解決」。**那句話必須精確到 `should_accept` 給不出的那兩樣 ——
   「連接結束」與「哪個監聽位址」**，⛔ 泛泛說「缺一個連接級接縫」會被正當地當成噪音。
3. ★★ **需求側的證據變強了，而它此前不在這份材料裡**：三個互不相干的使用者在兩年裡
   各自要過同一樣東西（#118 / #295 / #337）。
   ⚠ ⚠ **糾正我 2026-09-04 早些時候寫下的一句**：當時寫的是「三條都沒拿到實作」——
   **那不準確**（`5c4676f` 的提交資訊裡留著那句錯話，⛔ 絕不 amend，就地在這裡改準）。
   實際是**三條都被結掉了，而給出的答案各自只覆蓋問題的一部分**：
   #118 → #671（真實作，只覆蓋 **accept 那一刻**）· #337 → `Tracer`（真實作，
   只覆蓋**出站那一側**）· #295 → 只有它是被 stale 機器人關的。
   ★ 這個更準的形狀**對我們更有利**：不是「上游不理」，而是「每個答案都答的是相鄰的問題」。

4. ★★★ **本輪第二序的發現：上游對「怎麼數連接」的標準答案是 `Tracer`。**
   同一條評論（#245 的 `2129977269`）被 #295 與 #337 反覆引用，而它明說是
   「when you try to connect to your **upstream** with `HttpPeer`, you can put a `Tracer`
   object into it」。⇒ ⚠ **正文若不正面處理這一點，第一條回覆就會是「用 Tracer」** ——
   已在草稿的 alternatives 一節裡放在**第一條**，並寫明它數的是哪一群連接、
   以及哪幾類下行連接它一次事件都不會產生。

⚠ **#941 會影響形狀**：per-address 那一半若被合併，我們就只剩「連接結束」要提；
而它今天 open、0 評論、掛了六週 —— ⛔ **別假設它會完成實作**，也別假設它不會。

### 3.2 ✅ 第二輪：**普查**，不再是檢索（owner 2026-09-04 要求「仔細檢查……或已被 close 否決的」）

★★★ **方法從「搜」換成「枚舉」** —— 因為「我的關鍵詞覆蓋到了嗎」本身是個假設，
而今天早上已經栽過一次（`gh search issues` 一條 PR 都不回傳）。⇒ 用 GraphQL 分頁
**把全部 issue 與 PR 逐條列出來**，判斷留給人讀標題，不留給正則。

| 量到的（2026-09-04）| 數 |
|---|---|
| issue 總數 | **436**（178 open / 258 closed）|
| closed 的分佈 | `COMPLETED` 196 · **`NOT_PLANNED` 61** · `DUPLICATE` 1 · 無 reason 0 |
| PR 總數 | **500**（115 open / 269 closed / **116 MERGED**）|

⚠ **自證**（少了它，一個提前停下的分頁器看起來和跑完一樣）：枚舉結果裡含
issue **#118** 與 PR **#962**（兩類各一個已知存在的），且**最小 issue 號是 #1** ⇒ 走到底了。

**① 「有沒有被 close 否決過」—— 61 條 `NOT_PLANNED` 逐條讀完：⛔ 沒有一條否決我們的 ask。**
★ 順帶一條對判斷有用的觀察：其中 **#582「Adjustable buffer sizes in L4 `Stream`」與
#285「reuse port option?」後來其實都做了**（`c0845a8` per-listener L4 緩衝設定 ·
reuse_port 現已支援）⇒ 在這個倉裡 `NOT_PLANNED` 常常是「**現在不做／不按你說的做**」，
不是「永不」。

**② 寬網掃全部 936 條標題**（96 個 issue + 69 個 PR 命中，全部讀過），四條新發現：

| | 發現 | 對草稿的影響 |
|---|---|---|
| ★★★ | **#560（2025-03）與 #822（2026-02）都還 open**，要求「把 `prometheus` 在 `pingora-core` 裡變成可選」；`842ddd9` 用**整個搬走**回應了它們（另有 PR #612 / #708 是更早的嘗試，均已關閉）| ⇒ **上游刻意不讓指標實作留在 core 裡**。草稿新增一段**主動引用這段歷史**，說明我們要的東西在那條線的另一側（一個鉤子、零指標概念）|
| ★★★ | **#988（open，2026-09-01）+ PR #990 / #991（都 open）** —— torinnd 的「報告實際綁定的監聽位址」。⚠ **與我們的第 1 點互補而非競爭**：他答「有哪些位址」，我們答「**這一條連接**從哪個進來」| 草稿的 Additional context 裡點名，並把兩者的差別寫清 |
| ★★ | **PR #751（open 自 2025-11）「Add finish_downstream_session to ProxyHttp」**（fixes #647）—— 一個**按工作階段**的結束鉤子，在 `pingora-proxy` 層 | 點名，並說明層次與粒度不同（我們的在 core、按連接，因此也覆蓋 L4 與握手沒走完的連接）|
| ★ | **PR #663「給 `Tracing` trait 加 `check_allowed`」是作者自己當天關的**（"wrong repository"）⇒ ⛔ **不是被否決** | 消除了「擴充那個 trait 曾被拒」的疑慮 |

⚠ ⚠ **順帶查出一條與本目錄現有口徑有出入的事，登記給 owner**：`README.md` 寫著
「上游的合併方式是**批量重放**，不是合你的分支 ⇒ 『PR 被 close』在這裡是成功而不是拒絕」。
而實測 **116 個 PR 處於 `MERGED` 狀態**（例如 #516「add tweak_new_upstream_tcp_connection
hook」）—— ⇒ 兩條路**都存在**，那句話作為唯一口徑太強了。
★ 我們自己的投稿一恰好走的是前者（PR #962 closed，而改動進了 `main`）。

## 4. 立論骨架（形狀定了之後按它寫 issue）

**症狀**：一個用 pingora 做入口的處理程序，**說不出自己有多少條連接**。
`ConnectionFilter` 能數「接進來過幾條」，但數不出「此刻還有幾條」——
後者要一個「連接結束」的時機，而上游今天沒有任何地方給得出它。

**為什麼它不該由使用者自己在外面數**：連接任務有三條退出路徑（握手超時 / 握手失敗 /
正常結束）。在外面數就要在三處各寫一次減一，⚠ 而那正是「兩處各算一遍遲早分家」的形狀 ——
分家的表現是**一個恆不歸零的 gauge**，看起來像連接洩漏，而其實是計數洩漏。

**我們的落法**（fork 改動 15）：一個 `+1` 的接縫 + 一個 `Drop` 守衛做 `-1`，
⛔ **減一隻有守衛那一個呼叫點**。那段程式碼不認識 counter/gauge、不認識標籤、
**不知道 Prometheus 存在** —— ★ 這一點要寫進 issue：它不是在給上游塞一個指標體系。

**代價與邊界**：⚠ 上游若接受，`connection_filter` 那個 feature 的名字與預設值要一起想 ——
「過濾」與「計數」是兩件事，掛在同一個 feature 上會讓只想計數的人被迫編進過濾邏輯。

---

## 5. 正文草稿（**形狀 ②**，owner 2026-09-04 拍板）

> ⛔ **未發。** 發要 owner 按 **G40** 單獨授權 —— 本節只是草稿。
> ★ 下面「正文」一節是**逐字要發出去的英文**，它上面的中文都是給我們自己看的賬。

### 5.1 寫之前按上游 `main` = `09696b5` 逐條核過的事實

⚠ ⛔ **一條都不是從 09-03 那份快照抄的**，全部當天重取（`gh api .../contents/...?ref=main`）。

| 事實 | 出處 |
|---|---|
| `async fn should_accept(&self, _addr: Option<&SocketAddr>) -> bool { true }` | `listeners/connection_filter.rs` |
| 呼叫點在 `ListenerEndpoint::accept()` 裡，**而 `self.listen_addr: ServerAddress` 就在同一個結構體上**，且 `impl AsRef<str> for ServerAddress` | `listeners/l4.rs:464-494` · `l4.rs:75` |
| ★★★ **沒有 socket digest、或拿不到 peer addr 時，過濾器根本不被呼叫**（兩個 `else` 都直接 `true`）| `l4.rs:471-484` |
| `run_endpoint` 每條連接 spawn 一個任務，**三條退出路徑**（握手超時 / 握手失敗 / `handle_event` 回傳），**沒有任何結束通知** | `services/listening.rs:202-252` |
| 應用只在握手**成功**那一支才拿到連接（`Ok(io) => Self::handle_event(...)`）| `services/listening.rs:237` |
| `connection_filter` 是 **opt-in**：`default = []` | `pingora-core/Cargo.toml` |
| 上游自己已有同形狀的 trait，但只在**出站**那一側：`upstreams::peer::Tracing { on_connected, on_disconnected }` | `upstreams/peer.rs:70` ⚠ 我們 fork 裡是 `:69`，**別抄 fork 的行號** |
| `Listeners::set_connection_filter` 與 `ListenerEndpointBuilder::connection_filter` 都在 | `listeners/mod.rs:439` · `l4.rs:329` |
| `Tracer` 是 `PeerOptions` 上的欄位（出站側）| `upstreams/peer.rs:555` |
| ★ 下行路徑**一次都沒引用**它 —— ⚠ 這是**普查**不是抽查：`listeners/` 下全部四個檔案（`connection_filter.rs` · `l4.rs` · `mod.rs` · `tls/mod.rs`）加 `services/listening.rs`，`tracer\|Tracing` **全部 0 命中** | 逐檔案取 `?ref=main` 後 grep |
| 那條被 #295 與 #337 反覆引用的標準答案 | issue comment `2129977269`（#245 下）|

★★★ **第三行是本輪新查到的，而且它是正文裡最有說服力的一條**：
`should_accept` 作為**過濾器**，「拿不到位址就放行」是穩妥的預設；
而作為**計數接縫**，那等於**一部分連接永遠不進賬，且沒有任何東西會說**。
⚠ 更耐人尋味的是簽名已經是 `Option<&SocketAddr>`（表達得出「沒有位址」），
**呼叫點卻在此之前就短路了**。

### 5.2 ⛔ 有意**沒有**寫進正文的三件

1. ⛔ **不提 `ConnectionCounter` 這個名字，也不提「新接縫」** —— 形狀 ② 的全部要點就是
   「補完已有的」。提了就正面撞上上游 2026-08-25 那句「#671 已經加了連接級過濾器」。
2. ⛔ **一個效能數字都沒有**（`PLAN.md` §8：沒有可重現的端到端測量就不寫）。
3. ⚠ 順帶看到 trait 文件裡那段示例的簽名是**舊的**（寫著 `&SocketAddr`，實際是
   `Option<&SocketAddr>`）—— 是個真的小錯，但**與本 issue 無關，⛔ 不夾帶**：
   夾帶會稀釋立論，而這正是投稿一栽過的形狀。

★ ★ **而第二輪查出的另一條同族的事**寫進了正文，因為它**在**主題上：
`ConnectionFilter` 在上游**有兩份定義**且簽名不一致 ——
feature 開時是 `async fn should_accept(&self, _addr: Option<&SocketAddr>)`（`connection_filter.rs`），
關時是 `fn should_accept(&self, _addr: &std::net::SocketAddr)`（`listeners/mod.rs:58`，
而那份 stub 的文件寫著 "for API compatibility"）。
⇒ 它**直接約束我們提的改法**（加預設方法要在兩處加，而兩處本來就不一致）⇒ 正文裡一句話點到，
⛔ 並明說「只因為它影響這次改動才提，不是另開一份報告」。
★ 判據是同一條：**在主題上的、約束我們提案的 → 寫；off-topic 的小錯 → 不夾帶。**

---

### 5.3 第三輪複查（owner 要求「再走一輪」，⛔ 換角度，不重複前兩輪的查法）

前兩輪查的是「事實對不對」與「有沒有前人」。這一輪換成三面：**上游有沒有動 · 當維護者會
怎麼反駁 · 措辭與篇幅**。

| | 查了什麼 | 結果 |
|---|---|---|
| ✅ | 上游 `main` 有沒有移動（正文釘的是 `09696b5`）| **沒動**，仍是 `09696b5`（2026-08-25）⇒ 釘的那個號今天仍然準 |
| ✅ | `connection_filter` 是不是隻在 main 上（維護者會問版本）| **不是** —— 已發佈的 `0.8.1` tag 上 `connection_filter.rs` 與那個 feature 都在 ⇒ 正文不需要版本告示 |
| ✅ | 那條「兩份定義簽名不一致」是不是 main 上的臨時狀態 | **不是**，`0.8.1` 上同樣如此（real 收 `Option<&SocketAddr>` 且 `async`，stub 收 `&SocketAddr` 且同步）|

★★★ **這一輪真正逮到的一條（技術上要緊，前兩輪都沒想到）：`Drop` 不能 `await`。**
草稿原本把結束鉤子寫成 `async fn connection_closed(...)` —— 而**我們自己給出的落法**
（一個由任務持有、在 `Drop` 裡調鉤子的守衛）與 `async` 鉤子**是矛盾的**：
`Drop` 裡沒法 await，於是要麼每條關閉的連接 `spawn` 一次，要麼架一座阻塞橋，兩者都更差。
⇒ 已改成 `fn connection_closed(&self, listen: &str)`（同步），並在正文裡把這條理由寫明；
`should_accept` 照舊 `async`。
★ 這條是**我們自己踩過才知道的**，寫進去比任何論證都有說服力。

另外三處按「當維護者會怎麼反駁」改了：

1. 加了一段**具體的 API 草圖**（三行 `impl`）—— 維護者評估的是人體工學，而先前正文只有散文；
   ⚠ 草圖明寫它用的是「不破壞相容」那個變體。
2. 「the one we **expect to be offered** here」改成「the answer **given** on #245/#295/#337」
   —— ⛔ 去掉替對方揣測的口氣。
3. 「one implementation detail **matters more than** the API shape」改成
   「two things we **learned the hard way** carrying this in a fork, offered for what they are
   worth **rather than as a prescription**」—— ⛔ 去掉教對方怎麼寫程式碼的口氣。

⚠ 篇幅：約 130 行。判斷是**值得**——它按上游範本分節、前人證據密集，而這個倉的維護者
頻寬緊（#941 open 六週 0 評論）⇒ 一次把話說完比來回三輪便宜。

### 5.4 第四輪複查（owner 又要求「再 review 一遍」）：**我們的立論會不會被架空**

⛔ 前三輪查的是「事實 / 前人 / 措辭」。這一輪只問一件事：
**上游已有或在途的東西，會不會讓我們的某一點變得不必要？**

★★★ **答案是會，而且是第 2 點（監聽位址）。**

今天的機制（當天實查）：`Listeners::set_connection_filter` 把**同一個執行個體**蓋到每個
endpoint 上，`add_endpoint` / `add_listener` 也是 clone 那一個 ⇒ 一個過濾器分不出監聽器。
⚠ 而 `ListenerEndpointBuilder::connection_filter` **確實**收按 endpoint 的過濾器 ——
只是 `TransportStackBuilder` 是私有的、`TransportStack` 是 `pub(crate)`，
⇒ **手搓的 endpoint 喂不進 `Service`**，那條路走不通。★ 這正是 **#941** 要解決的事。

⇒ ★★★ **#941 一完成實作，「每個位址一個過濾器執行個體」天生就知道自己的位址，我們的第 2 點就沒必要了。**
這是維護者**一定**會提的反駁。⇒ 三處照此改：

1. **兩點重新排序**：不會被架空的那條（結束鉤子）領頭，監聽位址退為第 2；
   全部 `point 1` / `point 2` 交叉引用同步核過（`grep -n "point 1\|point 2"`）。
2. **自己把這條說出來**：正文明寫「#941 一完成實作，第 2 點就變得不必要，**而我們寧願如此**」——
   ★ 這讓我們的要求**變小**，也說明我們不是想再造一套機制。
3. **標題跟著改**：`…has no end-of-connection hook, and cannot tell listeners apart` ——
   「cannot tell listeners apart」正是 #941 會修掉的那句話，措辭與事實對齊。

⚠ 順帶補一條給「唯讀問題小節的人」的指針（那一條里加了「#941, open, would give this by
construction」），免得他停在那裡被誤導。

**同一條思路推到第 1 點：結束鉤子會不會也已經有現成的？** —— 查了，**沒有**：

| 候選 | 實查結論 |
|---|---|
| `ServerApp::cleanup` / `HttpServerApp::http_cleanup`（**#118 的報告者當年就提過它**）| ⛔ **按服務，不按連接**：`cleanup` 的文件註解自己寫著 "called **once after the service stops listening to its endpoints**"，呼叫點在 `listening.rs:319`，**在 accept 循環退出之後**。它看不見單條連接 |
| PR #751 `finish_downstream_session` | 按**工作階段**、在 `pingora-proxy` 層 ⇒ 覆蓋不到 L4，也覆蓋不到握手沒走完的連接 |
| 枚舉出的全部 500 個 PR / 436 個 issue | 寬網裡帶 `disconnect\|lifecycle\|hook\|callback`，165 條命中全部讀過 ⇒ **只有 #751 一個候選** |

⇒ ★ 第 1 點**沒有被架空**，而它現在是領頭的那一點 —— 形狀對了。
★ `http_cleanup` 那條已像對 `Tracer` 那樣**自己先答掉**（寫進 alternatives），
因為 #118 的報告者提過它 ⇒ 維護者也可能提。

**再往外一層查了 workspace 裡那個陌生成員 `pingora-foundations`**（描述是
「Foundations **telemetry** integration」）：讀過它的 `lib.rs` —— 它是**遙測管道**
（把 `foundations` 的 logging / metrics / tracing 與 `/metrics` HTTP 服務接成一個
`BackgroundService`），⛔ **不提供任何連接級資料源** ⇒ 不架空我們。
★★★ 反而**從另一側強化了立論**，這句已進正文：上游已經給了「往哪兒放這個數」
與「從哪兒吐出去」（`pingora-prometheus` + `pingora-foundations`），
**缺的正是 core 裡報出這個數的東西**。

### 5.4b ✅ owner 拍板：**第 2 點整條刪掉，只提結束鉤子**（2026-09-04）

⚠ ⚠ **本節取代 §5.4 裡「兩點重新排序」與那個舊標題** —— §5.4 記的是那一輪查到了什麼，
處置由 owner 改成了更狠的一刀：**不要第 2 點**。

★★★ 而這一刀逼出一處**必須一起改、否則自相矛盾**的地方：鉤子原本是
`fn connection_closed(&self, listen: &str)` —— **那個 `listen` 參數本身就是第 2 點**。
刪掉第 2 點，簽名就得收成 **`fn connection_closed(&self)`**。

⇒ ★ 而這讓故事更硬，不是更弱：

| | |
|---|---|
| **今天**（一個執行個體服務所有 endpoint）| 單這一條就給出**處理程序級的線上連接數** —— 而今天連這個都拿不到 |
| **#941 完成實作後** | 每個位址一個執行個體 ⇒ 按監聽器分格**天生**就有，⛔ 不需要我們要任何東西 |

⇒ 最終訴求＝**一條帶預設實作的同步方法**，零簽名變更、零新資料過邊界、
既有實作方零改動。★ 這是這份 issue 能有的最小形狀。

同筆收尾（否則會留下互相矛盾的句子）：

1. **標題**改成 `ConnectionFilter has no end-of-connection hook, so downstream connections
   can be counted starting but never ending` —— 自足、不再提監聽器身分；
2. 問題小節從「三件事擋在前面」收成**一件**（那條「拿不到 peer addr 就不調」降為
   一句**明說「我們不要求你改」**的告示，免得把計數吹過頭）；
3. 相容性那段刪掉「改簽名不相容」那半句（我們不再提改簽名）；
4. alternatives 裡凡是拿「學不到監聽器身分」當論據的都刪掉 —— 那不再是我們的論點；
5. Additional context 把 #941 改成「**我們有意不碰**監聽器身分，那正是它該來的地方」。

⚠ ⚠ ★ 全部 `point 1` / `point 2` / `listen: &str` / `connection_accepted` 的殘留
**逐條 grep 核過**（⛔ 不靠通讀：改寫之後最容易留下的正是一句讀起來通順的舊引用）。

### 5.5 發佈用的正文怎麼取（⛔ 不手打）

`scratchpad/extract_issue_body.py`：從材料裡按 `**Title**:` 與
`## What is the problem` 兩個錨點切出標題與正文，寫成兩個檔案，並列印 sha256。
★ **理由**：owner 批准的是材料裡那份，真發出去的必須與它**逐位元組相同** ——
手打重錄正是兩者分家的方式。

它帶守衛：正文裡**不許**出現我們自己的任何記號（`⛔ ★ ⚠ ⇒`、`**Title**:`、
`正文（GitHub issue` 等），四個範本小節缺一即拒。
⚠ ⚠ **守衛第一版漏了 `⚠`** —— 一份不全的守衛與沒有守衛是同一個形狀，已補。
★ 而「守衛通過」本身不算證據：另外**直接量過**一遍提取出來的正文 ——
四個記號各 0 次、**中文字元 0 個**。

---

## 正文（GitHub issue，逐字）

**Template**: `.github/ISSUE_TEMPLATE/feature_request.md`

**Title**: `ConnectionFilter has no end-of-connection hook, so downstream connections can be counted starting but never ending`

## What is the problem your feature solves, or the need it fulfills?

A process that uses Pingora as its entry point cannot report how many downstream connections
it currently has. A connection *starting* can be observed today; a connection *ending* cannot
be observed at all.

`ConnectionFilter::should_accept` (added by #671, behind the `connection_filter` feature) is
called after `accept()` and before the TLS handshake, which is the right moment to see a
connection start. **Nothing is called when one ends.** `run_endpoint` spawns one task per
connection, and that task has three exits — handshake timeout, handshake error, and
`handle_event` returning — and none of them notifies anything. So a number built on
`should_accept` can only ever go up.

This has been asked for more than once:

- #118, "on connect phase for incoming connections" — closed as completed by #671, which
  covers the accept moment only.
- #295 — asked specifically for the number of *active* connections (increment on connect,
  decrement on disconnect). The suggested answer was a userland `Drop` guard; the issue
  was closed by the stale bot, the reporter's last comment being "still relevant".
- #337 — asked how to track *client* connect/disconnect (for WebSocket); the answer was to
  use `Tracer`, and the issue was closed as completed.

Pingora already has exactly this shape, on the other side: `upstreams::peer::Tracing`
provides `on_connected` / `on_disconnected`, and `Tracer` is the answer people are pointed
at on #245, #295 and #337. But a `Tracer` is a field on `PeerOptions` and counts connections
*to upstreams*; there is no equivalent for connections *from* downstream. The alternatives
section below says why that is a different question rather than a smaller one.

Worth noting from the other side: `pingora-prometheus` and `pingora-foundations` already give
a place to publish such a number and an endpoint to serve it from. What is missing is anything
in core that reports it.

One caveat we are *not* asking you to change, mentioned so a count is not oversold: in
`ListenerEndpoint::accept()`, if the stream has no socket digest or the digest has no peer
address, `should_accept` is not called at all and the connection is accepted by default. For a
filter that is a safe default; it does mean a count of starts is not exhaustive.

## Describe the solution you'd like

Extend the existing `ConnectionFilter` seam rather than introduce a second one. It already
carries the plumbing (a per-endpoint field, a builder method, `Listeners::set_connection_filter`)
and is already gated behind `connection_filter`, which is off by default.

**One addition**: a defaulted `fn connection_closed(&self)`, called once for each connection
`should_accept` was consulted about, when that connection is gone. No signature changes, no
new data crossing the boundary, and nothing for existing implementors to do.

The pairing is the part worth pinning down: as noted above, `should_accept` is skipped
entirely when there is no peer address. A close hook that fired for *every* accepted
connection would therefore not pair with it, and the obvious `+1` / `-1` implementation would
underflow. Firing it exactly where `should_accept` ran keeps the two in step.

```rust
#[async_trait]
impl ConnectionFilter for MyConnectionCount {
    async fn should_accept(&self, _addr: Option<&SocketAddr>) -> bool {
        self.live.fetch_add(1, Relaxed);
        true
    }

    // new
    fn connection_closed(&self) { self.live.fetch_sub(1, Relaxed); }
}
```

**On attributing connections to a particular listener**: we are deliberately *not* asking for
that here. Today `Listeners::set_connection_filter` installs one instance across every
endpoint (and `add_endpoint` clones that same one), so the hook above yields a process-wide
live count — which is more than is obtainable now. Per-listener attribution follows from #941
by construction, since a filter instance attached to one address already knows its address.
We would rather wait for that than ask for a second mechanism.

Two things we learned the hard way carrying this in a fork, offered for what they are worth
rather than as a prescription:

- **The end hook wants to be synchronous.** The only way we found to guarantee it fires is a
  value owned by the spawned task that calls the hook from `Drop` — and `Drop` cannot await.
  An `async` close hook therefore forces either a `spawn` per closing connection or a
  blocking bridge, both worse than a plain `fn`. `should_accept` can stay `async`.
- **Drive it from that guard, not from explicit calls at each exit.** The task has three
  exits; an explicit call has to be written three times, and the failure mode of missing one
  is silent — a live-connection number that never comes back down, sitting next to a total
  that looks perfectly healthy.

**Backwards compatibility**: a defaulted method is source-compatible for existing
implementors, and the feature is opt-in, so nothing changes for anyone who does not implement
it.

One thing worth knowing before shaping this: the trait exists twice. The real one is in
`listeners/connection_filter.rs` under the feature; `listeners/mod.rs` defines a stub for
when the feature is off, described as being there "for API compatibility". Their signatures
already differ — the stub's `should_accept` is synchronous and takes `&SocketAddr`, the real
one is `async` and takes `Option<&SocketAddr>` — so any addition here needs a decision about
the stub too. We mention it only because it shapes the change, not as a separate report.

**On what this would add to `pingora-core`**: we are aware that core deliberately no longer
carries metrics. #560 and #822 asked for `prometheus` to be made optional, and `842ddd9`
answered by moving the Prometheus HTTP app out into its own `pingora-prometheus` crate,
which now builds entirely on core's public API. What we are asking for sits on the other
side of that same line: one notification, with no metrics dependency, no counter/gauge
distinction and no label vocabulary. The counting stays in the caller, exactly as
`pingora-prometheus` now sits outside core.

**A scope question we would rather ask than assume**: `connection_filter` is named for
*filtering*, and observing when a connection ends is a different concern. If you would prefer
it to live under a differently named feature, or on a sibling trait, we are happy to shape a
PR that way. What we would like to avoid is a second, parallel wiring path alongside the one
`ConnectionFilter` already has.

## Describe alternatives you've considered

- **`upstreams::peer::Tracer`** — the answer given on #245, #295 and #337, so it is worth
  addressing up front rather than after a round trip. A `Tracer` is placed in
  `PeerOptions` and its `on_connected` / `on_disconnected` fire for connections Pingora opens
  *to an upstream*, counting active plus pooled ones. That is a different population from the
  connections a listener is currently holding: a downstream connection that never opens an
  upstream connection produces no tracer events at all — one rejected by a filter, one that
  fails or times out in the TLS handshake, a request served from cache or by a local handler,
  or any L4 application that never dials out. Nothing in `listeners/` or
  `services/listening.rs` references it.
- **`ServerApp::cleanup` / `HttpServerApp::http_cleanup`** — suggested on #118, so worth
  ruling out explicitly. These are per-service, not per-connection: the doc comment on
  `cleanup` says it is "called once after the service stops listening to its endpoints", and
  the call site in `run_endpoint` is after the accept loop has exited. They cannot see
  individual connections at all.
- **Counting in userland with a `Drop` guard** (also suggested on #295). It cannot cover the
  window this is about: connections that time out or fail during `io.handshake()` never reach
  the application, because `handle_event` is only called on the successful branch. For an
  entry point those are a population you specifically want to see.
- **Polling the operating system** (`netstat`-style, also considered on #295): it is a
  sampling answer to a question about state transitions, and attributing sockets back to a
  particular listener of a particular process is awkward at best.
- **A separate observer trait alongside `ConnectionFilter`**: this duplicates wiring that
  already exists (per-endpoint field, builder method, `Listeners::set_*`), which is why we
  are proposing to extend the existing seam. See the scope question above if you disagree.
- **Carrying it in a fork** — what we do today. It works, but it is a permanent rebase cost
  for something that looks generally useful rather than specific to us, which is why we are
  asking here instead of keeping it.

## Additional context

- Related, on the seam itself: #671 (the `ConnectionFilter` trait), #118, #295, #337.
- Related, on listener identity, which we are deliberately leaving alone: #941 (open) attaches
  filters per address, which is how we would expect per-listener attribution to arrive; #988,
  with PR #991 reporting the addresses a service actually bound and PR #990 fixing the fd-table
  collision it needs (all open).
- Related, on end-of-something hooks: #751 (open since 2025-11) adds
  `finish_downstream_session` to `ProxyHttp`. That is a per-session hook in `pingora-proxy`;
  what is asked for above is a per-connection notification in `pingora-core`, so it also
  covers L4 applications and connections that never complete a handshake.
- We maintain a fork carrying this capability and would be glad to send the PR, in whatever
  shape you prefer.

**Pingora version**: `main` @ `09696b5`
