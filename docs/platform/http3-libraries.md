---
type: 技術基線
title: HTTP/3 庫選型事實表（D11）
description: 三條候選路線在「按 DCID 路由」「rustls 整合面」「維護活躍度」上的實測事實；★ quiche 用 BoringSSL 而不是 rustls，s2n-quic 根本不帶 HTTP/3。
resource: ../../PLAN.md
tags: [依賴, 待定, 必讀, 易錯]
status: draft
generated:
  by: claude-code/opus-5
  at: 2026-08-25T00:00:00Z
sources:
  - id: plan-10-d11
    resource: /references/plan.md
    title: PLAN.md §10 G103（D11 結案：HTTP/3 取 quiche）與它的兩條前置
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 G6 三條硬約束（① TLS 後端 —— 原文鎖 rustls，已由 G104 改成統一 BoringSSL；② tower 用不上；③ QUIC 監聽器必須參與 socket 移交 —— 已由 M0 解除）
  - id: open-seams
    resource: /verification/open-seams.md
    title: 升級視窗內的 QUIC 連接歸屬；`ResolvesServerCert` 能否同時服務兩個入口
  - id: crates-io
    resource: https://crates.io/api/v1/crates
    title: 各 crate 在 crates.io 上**自己宣告**的版本、依賴與 feature 表
  - id: docs-rs
    resource: https://docs.rs
    title: quiche / quinn-proto / s2n-quic 的公開 API 文件
---

# 這份表是幹什麼的

✅ ★★★ **D11 2026-08-25 由 G103–G105 結案：取 `quiche`**（§10）。
**本頁原樣保留** —— 它是拍板時看的那張表，不是拍板之後的追述。

> ★ ★ **「原樣保留」保的是哪一部分**（owner 2026-09-03 拍板，寫在這裡免得下一個人再猜）：
> **保的是當時查到的事實與比較** —— 版本號、feature 表、依賴關係、各條路線的取捨，
> ⛔ **一個字都不許改**，錯了也留著（本頁第 3 節那條「沒有實測、只是把兩句話放在一起」
> 就是這麼留下來的，而它後來被實測換成了另一件事）。
> ⚠ **不保路標**：指向 `§` 號、`D` 號、`G` 號、檔案路徑這一類**引用**允許後補更正 ——
> 一個指錯地方的路標不是歷史，是壞鏈。★ 更正要**當場說明它是後補的**。
> ⇒ 於是本頁既不必對著一個已經結案的 `§11` 說話，也不必給引用門開豁免。

> ★ 拍板同時定了另外兩條，都由本頁的事實直接推出來：
> **G104** TLS 棧**整體換到 BoringSSL**（⚠ 推翻 §5.1 第 1 條 —— 因為第 2 節那條
> 「quiche 用 BoringSSL、依賴表裡沒有 rustls」使得「繼續鎖 rustls」等價於「兩套 TLS 並存」）；
> **G105** HTTP/3 語義層用 `quiche::h3` **不自研**（第 0 節 (b) 與安全基線第 5 條）。
> ✅ ★ **那條未驗前置已驗完，通過**：
> 記錄 [`musl + BoringSSL 静态链接`](/verification/musl-boringssl.md) —— **G103 不重議**。
> ⚠ ⚠ ★ 而下面第 3 條那句「沒有實測，只是把兩句話放在一起」，實測之後的答案與它的**方向**
> 一致（能做到）而**卡點完全不同**：卡的不是 BoringSSL 與 musl，是 **`boring-sys` 的
> bindgen 要 `dlopen`，而 Alpine 上 build script 預設是靜態的**。
> ★ **一條「把兩句話放在一起」的擔心，實測之後往往不是被證實或證偽，而是被換成另一件事**
> —— 而那件事沒有人預先寫在任何清單上。

以下是拍板前的原文。它**只記查到了什麼，不給推薦項** —— 推薦項與拍板走 §10 的常規流程。

★ 口徑照 **G100 那次**：讀的是原始碼與各 crate 自己宣告的中繼資料，不是印象；
★ 也照 [尚未驗證的接縫](/verification/open-seams.md) 結尾那條方法論：**推斷不是證據** ——
下面凡是推斷，都單獨標著，並寫明要拿什麼把它坐實。

⚠ **全部數字的採集日期是 2026-08-25。** 這一頁的每次複查都要換日期 ——
「上游停更了」與「我們上次看是三個月前」在句子里長得一樣。

---

# 0. ★★★ 先糾正 D11 那行裡的一處比較錯位

D11 當初把候選寫成 **`h3` / `quiche` / `s2n-quic` 三選一**（⚠ **它已由 G103 結案取
`quiche`**，不在 §11 裡了 —— 本節記的是那次拍板前的比較）。
而實測出來的分層是：

| | QUIC 傳輸層 | HTTP/3 語義層 | 合起來要幾個 crate |
|---|---|---|---|
| 路線 A | **quinn** | **h3**（靠 `h3-quinn` 適配） | 3 |
| 路線 B | **quiche** | **`quiche::h3`（同一個 crate 裡）** | 1 |
| 路線 C | **s2n-quic** | ⚠ **沒有** —— 它是 QUIC-only | 3（仍要 `h3` + 自寫 backend）|

⇒ 兩條結論直接從這張表出來：

1. ★ **D11 那行是拿一層去比兩層。** 「`h3` 自 2025-05-06 起十五個月未更新」說的只是**上面那層**；
   下面那層 quinn，`quinn-proto 0.11.17` 是 **2026-08-17 發的（8 天前）**。
   ⇒ 「這條路線停滯了」這句話，對 h3 成立、對 quinn **不成立**，而原文把兩者寫成了一件事。
2. ★★ **路線 C 繞不開 h3。** s2n-quic 的 crate 描述原話是「An implementation of the IETF QUIC protocol」，
   feature 表 22 條裡沒有任何一條通向 HTTP/3。⇒ 選 s2n-quic **仍然要把 `h3` 摞在上面**，
   而且要走它那個名字就是警告的 feature：`i-implement-a-third-party-backend-and-opt-into-breaking-changes`。
   ⇒ **「換掉 h3」不是選 s2n-quic 能換來的東西。**

---

# 1. 維護活躍度

## crates.io（各 crate 自己宣告的版本中繼資料）

| crate | 最新未 yank 版本 | 發佈日期 | 距 2026-08-25 | 總下載 |
|---|---|---|---|---|
| `h3` | 0.0.8 | 2025-05-06 | **15.6 個月** | 4,024,685 |
| `h3-quinn` | 0.0.10 | 2025-05-06 | **15.6 個月** | 3,482,179 |
| `quinn` | 0.11.11 | 2026-06-22 | 2.1 個月 | 275,272,724 |
| `quinn-proto` | 0.11.17 | **2026-08-17** | **8 天** | 282,292,550 |
| `quiche` | 0.29.3 | 2026-07-14 | 1.3 個月 | 2,467,607 |
| `s2n-quic` | 1.88.0 | **2026-08-21** | **4 天** | 672,917 |

★ `h3` 與 `h3-quinn` **同一天發的**（2025-05-06）—— 適配器與本體一起停在那裡，不是隻停了一個。

## GitHub（2026-08-25 查）

| 儲存庫 | 最後一次提交 | 近半年提交數 | stars | open issues |
|---|---|---|---|---|
| `hyperium/h3` | 2026-08-13 | **7** | 884 | 48 |
| `quinn-rs/quinn` | 2026-08-23 | **≥100** | 5,233 | 199 |
| `cloudflare/quiche` | 2026-08-13 | **≥100** | 11,793 | 337 |
| `aws/s2n-quic` | 2026-08-24 | **≥100** | 1,363 | 272 |

⚠ **那三個 `≥100` 是查詢上限不是計數**：`gh api commits?since=…&per_page=100` 一頁取滿就沒再翻。
★ 而 `h3` 的 **7** 是真數出來的（一頁沒取滿）。**一個取滿了上限的數字與一個真實計數放在同一列裡，
必須標出來** —— 否則下一個人會拿 100 去跟 7 比。

## ★★ 一條最容易讀錯的：`h3` **沒停止開發，它是沒發版**

| 事實 | 值 |
|---|---|
| 最後一次 **發版** | 2025-05-06（15.6 個月前）|
| 最後一次 **提交** | 2026-08-13（12 天前）|
| 近半年提交 | 7 |
| 儲存庫 archived？ | 否 |

⇒ 「十五個月未更新」這句話，按**發版**讀是對的，按**開發**讀是錯的。
★ 兩者的差別在選型上是實的：一個不發版的庫意味著**修好的東西你拿不到**（除非釘 git rev），
而一個停止開發的庫意味著**根本沒人修**。⇒ D11 拍板時要說清楚記的是哪一個。

---

# 2. rustls 整合面

> ⚠ 這一欄掛著 §5.1 的**第 1 條硬約束**（TLS 後端鎖死 rustls）與 G6 的原話
> 「一套證書、一套 ALPN、一套工作階段快取」。★ 我們現在鎖的是 **rustls 0.23.43**（`Cargo.lock`）。

| | TLS 後端 | 與我們這份 rustls 的關係 |
|---|---|---|
| **quinn 0.11.11** | `rustls ^0.23.5`（可選，但 `default` 裡含 `rustls-ring` ⇒ 預設就開）| ✅ **同一個大版本**，不產生第二份 rustls |
| **quiche 0.29.3** | `boring ^4.3`（BoringSSL），`default = ["boringssl-boring-crate"]` | ❌ **依賴表裡沒有 rustls，9 個 feature 沒有一條通向它** |
| **s2n-quic 1.88.0** | 預設 `provider-tls-default` → `s2n-quic-tls-default`（平台探測，即 s2n-tls）；`provider-tls-rustls` → `s2n-quic-rustls` | ✅ 拿得到，但**要顯式關掉 default**；`s2n-quic-rustls 0.88.0` 宣告的是 `rustls ^0.23`（**非可選**）⇒ 同大版本 |

## ★★★ quiche 那一格的後果比「不整合」嚴重

選 quiche 不是「rustls 那套用不上」，是**三件事同時發生**：

1. **產物裡兩套 TLS**：rustls 給 h1/h2，BoringSSL 給 h3 ——
   而 G6 的原話是一套證書、一套 ALPN、一套工作階段快取。
2. ★★★ **`ResolvesServerCert` 在 BoringSSL 那側根本不存在。**
   ⇒ D11 的第二條前置（[「`ResolvesServerCert` 能否同時服務兩個入口」](/verification/open-seams.md)）
   對 quiche **不是「要驗」，是「結構上做不到」** —— 兩個入口會有兩套完全獨立的挑證書機制，
   而自動 HTTPS / On-Demand 全建在 rustls 那一條路上（§5.1 第 1 條，**選定後不能改主意**）。
3. **BoringSSL 是 C 依賴**，而 §5 的散布口徑是 **musl 單靜態二進位**。
   ⚠ 這一條**沒有實測**，只是把兩句話放在一起；要判它得真去 musl 上編一次。
   > ✅ **2026-08-25 補記（本頁其餘部分維持拍板時的原文）**：真去編過了，**通過** ——
   > 見 [`musl + BoringSSL 静态链接`](/verification/musl-boringssl.md)。

## ⚠ 一條會當場作廢既有成果的坑：`aws-lc-rs`

quinn 的三個 rustls feature：

| feature | 後果 |
|---|---|
| `rustls-ring` | ring（**我們現在就是 ring**，`Cargo.lock` 裡 `ring 0.17.14`）|
| `rustls-aws-lc-rs` | ⚠ **把 `aws-lc-rs` 拖回依賴圖** |
| `rustls-aws-lc-rs-fips` | 同上 |

★ 本儲存庫為把 `aws-lc-rs` 擋在依賴圖外面付過**三次**代價：G41/G45 那輪
（`pingora-core/rustls` 實測多解析 46 個包、**而 aws-lc-rs 不在其中**，`PLAN.md` §1）、
`instant-acme` 的 default feature、`hyper-rustls` 的 default feature。

✅ **好訊息**：quinn 的 `default` 用的正是 `rustls-ring`，順路。
⚠ **但別照抄 `default`**：它是 `['log', 'platform-verifier', 'runtime-tokio', 'rustls-ring', 'bloom']`，
其中 `platform-verifier` 是**用戶端**驗伺服器端證書用的 —— 我們這一側是伺服器端。
⇒ 該寫 `default-features = false` + 顯式挑。

---

# 3. 「按 DCID 路由」的原語

> 這一欄掛著 D11 的**第一條前置**：升級視窗內兩代處理程序持有同一個 UDP socket，
> 資料報會被分流，而 QUIC 的連接狀態只在某一代的記憶體裡 ——
> 被分到另一代的包**不是「換條工作階段」，是解不開**。見 [尚未驗證的接縫](/verification/open-seams.md)。

## quiche：✅ 有，而且在連接存在之前就能用

```rust
pub fn from_slice<'b>(buf: &'b mut [u8], dcid_len: usize) -> Result<Header<'a>>
```

公開函式，**不解密、不需要先有連接**，回傳的 `Header` 公開欄位裡就有 `dcid` 與 `scid`。
★ 再加上 quiche 是 **sans-IO**（crate 文件原話：應用負責提供 I/O 與帶定時器的事件循環），
socket 與事件循環本來就在我們手裡 ⇒ **「先看 DCID 再決定交給哪一代」這件事寫在我們自己的程式碼裡**，
不需要庫同意。

## quinn：⚠ 路由在庫內部，我們只有兩個鉤子

`Endpoint::handle(...) -> Option<DatagramEvent>`，而 `DatagramEvent` **只有三個變體**：

| 變體 | 載荷 | 文件原話 |
|---|---|---|
| `ConnectionEvent` | `(ConnectionHandle, ConnectionEvent)` | The datagram is redirected to its `Connection` |
| `NewConnection` | `Incoming` | The datagram may result in starting a new `Connection` |
| `Response` | `Transmit` | Response generated directly by the endpoint |

⇒ **沒有一個變體表示「這個 DCID 我不認識」**，而回傳類型是 `Option`。

### ✅ ★★★ 2026-08-25 已讀原始碼坐實，而答案比推斷**嚴重得多**

讀的是 `quinn-proto-0.11.17` 這個 tag 的 `quinn-proto/src/endpoint.rs`（48648 位元組，不是 docs.rs 的摘要頁）。
`handle()` 末尾那串 `else if` 鏈的**最後一格**是：

```rust
} else {
    // If we got this far, we're receiving a seemingly valid packet for an unknown
    // connection. Send a stateless reset if possible.
    self.stateless_reset(now, datagram_len, addresses, *dst_cid, buf)
        .map(DatagramEvent::Response)
}
```

⚠ ⚠ ⚠ **認不出的資料報不是被丟掉，是被回一個 stateless reset** ——
而 stateless reset 的語義是**告訴對端「這條連接死了，拆掉它」**。

⇒ ★ ★ ★ **推斷錯在方向上**：我原以為最壞是「丟一個包」（QUIC 自己會重傳，連接扛得住），
實際是**升級視窗裡新一代會主動殺掉老一代的在飛連接**。
> ★ ★ **「拿不回那個資料報」與「替我們把連接殺了」，在 `Option<DatagramEvent>`
> 這個簽名上長得一模一樣。** 只看類型簽名推不出方向，得讀實作。

### ✅ 而同一段原始碼裡就有解藥，是個**現成類型**

那條 `else` 鏈的**前一格**是：

```rust
} else if !event.first_decode.is_initial()
    && self.local_cid_generator.validate(dst_cid).is_err()
{
    debug!("dropping packet with invalid CID");
    None
}
```

⇒ **只要 `validate()` 判 `Err`，quinn 就走「丟包」而不是「stateless reset」。**

關鍵在於**預設實作是什麼**（`quinn-proto/src/cid_generator.rs`）：

| generator | `validate` | 後果 |
|---|---|---|
| trait 預設 | `fn validate(&self, _cid) -> Result<(), InvalidCid> { Ok(()) }` —— **無條件放行** | ⚠ 落進 `else` ⇒ **stateless reset** |
| `RandomConnectionIdGenerator` | **不覆蓋**，用預設 | ⚠ 同上 |
| **`HashedConnectionIdGenerator::from_key(key)`** | 按 key 對 nonce 算 FxHash 簽名並比對 | ✅ **兩代用不同 key ⇒ 互相拒認 ⇒ 丟包，不 reset** |

★ ★ 而 `from_key` 的文件原話就是衝著這件事寫的：
**"Allows `validate` to recognize a consistent set of connection IDs across restarts"**。

⇒ **降級路線是現成的**：每一代一個自己的 key，失效形態從「殺死連接」降到「丟一個資料報」，
而後者 QUIC 自己會重傳扛過去。⚠ **但它仍然只是降級，不是解決** ——
落到新一代手裡的那些包**還是回不到老一代**（`handle` 回的是 `None`，
資料報和 DCID 都拿不回來）。**真正的「轉交」只能在 quinn 之外做**：
`Endpoint::new` 收的是我們自己的 socket，所以路由要寫在
我們自己實作的 `AsyncUdpSocket::poll_recv` 裡，在餵給 quinn 之前分流。
⚠ ⚠ 而兩代是**兩個處理程序**，跨處理程序轉交那一段與選哪個庫無關 —— 三條路線都要自己寫。

我們**確實有**的鉤子是 `ConnectionIdGenerator`：

| 方法 | 用途 |
|---|---|
| `generate_cid(&mut self) -> ConnectionId` | ★ **CID 的位元組由我們自己造** ⇒ 可以埋一個「第幾代」的標記 |
| `cid_len(&self) -> usize` | 長度 |
| `cid_lifetime(&self) -> Option<Duration>` | 有效期 |
| `validate(&self, cid) -> Result<(), InvalidCid>`（有預設實作）| 快速篩掉「不像本 generator 發的」，**允許假陽性** |

⚠ ⚠ **但「篩掉」與「轉交給上一代」是兩件事。** 前者 quinn 給了，後者正是這條前置要答的問題，
而上面那個推斷說的恰恰是：**後者可能在這個 API 面上做不到。**

## s2n-quic：形狀與 quinn 同族，但 `Validator` 能**從原始訊息裡解析長度**

`provider::connection_id` 裡三個 trait：**`Generator`** / **`Validator`** / **`Format`**。
✅ **2026-08-25 已讀原始碼拿到簽名**（`quic/s2n-quic/src/provider/connection_id.rs`，v1.88.0 那個 tag）：

```rust
fn generate(&mut self, connection_info: &ConnectionInfo) -> connection::LocalId;
fn lifetime(&self) -> Option<Duration>;
fn validate(&self, connection_info: &ConnectionInfo, buffer: &[u8]) -> Option<usize>;
```

★ **注意 `validate` 的形狀與 quinn 的不是一回事**：quinn 那個收一個已經解析好的
`&ConnectionId`、回 `Result<(), InvalidCid>`（"這個像不像我發的"）；
而這個收的是**原始位元組緩衝**、回 `Option<usize>` —— **它回的是「CID 有多長」**，
也就是說它承擔的是**從訊息裡把 CID 解析出來**這件事，比 quinn 那個鉤子靠前一層。

⚠ **但仍然沒有證據說明 s2n-quic 對認不出的 CID 做什麼**（丟包？stateless reset？）——
★ 這正是 quinn 那條栽過的地方：**鉤子的形狀說明不了失效形態。**
⇒ 要判它得讀 `s2n-quic-transport` 的 endpoint 散布路徑，與讀 quinn 那次同樣的做法。

## 一句話對比

| | 能在「連接存在之前」拿到 DCID？ | 能控制自己發出去的 CID 位元組？ | 認不出的資料報能拿回來嗎？ | ★ **認不出時的失效形態** |
|---|---|---|---|---|
| quiche | ✅ `Header::from_slice` | ✅ `accept(scid, odcid, local, peer, config)` —— 伺服器端 CID 由呼叫方傳進去 | ✅ 資料報從頭到尾都在我們手裡 | ✅ **由我們決定** —— 庫根本沒機會自作主張 |
| quinn | ❌ 沒有公開的部分解碼，`handle` 回 `None` | ✅ `generate_cid` | ❌ **不能**（已讀原始碼坐實）| ⚠ ⚠ **預設是 stateless reset ⇒ 殺掉對端連接**；換 `HashedConnectionIdGenerator::from_key` 後降級為丟包 |
| s2n-quic | ✅ `Validator::validate(_, buffer) -> Option<usize>` 收原始位元組 | ✅ `Generator::generate` | ⚠ 未核實 | ⚠ **未核實** —— 而 quinn 那條正說明**鉤子的形狀說明不了失效形態** |

---

# 4. ★ 第四條維度：socket 移交（§5.1 第 3 條要它）

> 這一條不在最初點名的三條裡，但它是**硬約束**：自建 QUIC(UDP) 監聽器
> **必須接入 Pingora 的 socket 移交**，否則優雅升級時這條會斷連。

| | 能不能喂一個已經 bind 好的 socket |
|---|---|
| **quinn** | ✅ **能**。`Endpoint::new(config, server_config, socket: std::net::UdpSocket, runtime: Arc<dyn Runtime>)` 直接收一個已 bind 的 `UdpSocket`；另有 `Endpoint::new_with_abstract_socket(..., socket: Arc<dyn AsyncUdpSocket>, ...)` |
| **quiche** | ✅ **天然如此**。sans-IO ⇒ 它根本不碰 socket |
| **s2n-quic** | ✅ **能**（2026-08-25 讀原始碼核實，`quic/s2n-quic-platform/src/io/tokio/builder.rs`，v1.88.0）：`Builder::with_rx_socket(socket: std::net::UdpSocket)` 與 `with_tx_socket(…)` 各收一個**已 bind 的** `std::net::UdpSocket`（另有 `with_prioritized_socket`）|

★ **三條路線在這一欄上沒有區別** —— §5.1 第 3 條對選庫**不構成篩選條件**。

---

# 5. ★ 儲存庫裡已有兩道門在管這件事，選型要連它們一起看

`crates/fulcrum/tests/supply_gates.rs` 裡：

| | 門 | 它對三條路線各說什麼 |
|---|---|---|
| 門 1 | `两把锁里都不许出现_aws_lc_rs()` —— 掃根鎖與 `vendor/pingora` 的鎖，任何 `aws-lc*` 開頭的包都判紅 | ✅ quinn 走 `rustls-ring` 不觸發；⚠ 走 `rustls-aws-lc-rs` **當場紅** |
| 門 2 | `会自带_crypto_provider_的依赖必须写_default_features_false()` —— 按一張**手寫名單** `MUST_DISABLE_DEFAULT` 查 | ✅ ★ **`quinn` 已經在那張名單裡**（`supply_gates.rs:133`，早就有人替它留了位）|

## ⚠ ⚠ 而門 2 那張名單裡**沒有** `s2n-quic`

s2n-quic 的 `default` 是 `['provider-address-token-default', 'provider-tls-default']`，
後者通向 `s2n-quic-tls-default`（平台探測 ⇒ **s2n-tls**）。
⇒ 一份照預設寫的 `s2n-quic = "1"` 會把**另一套 TLS 棧**編進產物，
而**門 1 不會紅**（s2n-tls 不叫 `aws-lc*`）、**門 2 也不會紅**（名字不在名單上）。

★ ★ 這正是 `unwired_contract.rs` 頂上引的那條紀律說的事：**手寫清單在清單本身漏了一項時照樣綠。**
⇒ **若 D11 選 s2n-quic，把 `s2n-quic` 加進 `MUST_DISABLE_DEFAULT` 是同一批裡必須做的一步**，
不是「記得也可以」。

⚠ `quiche` 不在名單上**不是同一個洞**：它拖進來的是 `boring`（BoringSSL），
既不是 rustls 的 crypto provider，也不叫 `aws-lc*` —— 兩道門對它**結構上就說不出話**。
⇒ 選 quiche 的話，「產物裡有幾套 TLS」這件事**目前沒有任何門看著**。

---

# 6. ✅ 原先欠的三條**2026-08-25 全部補上**（讀的是各自 release tag 的原始碼）

| | 欠什麼 | 結果 |
|---|---|---|
| ① | quinn 對認不出的 DCID 怎麼處理 | ✅ **查清了，而且推斷是錯的**：預設**回 stateless reset**（殺連接），不是丟包；換 `HashedConnectionIdGenerator::from_key` 可降級為丟包。見第 3 節 |
| ② | s2n-quic `connection_id` 三個 trait 的簽名 | ✅ 拿到了，且 `validate` 收**原始位元組**回 `Option<usize>`，比 quinn 的鉤子靠前一層 |
| ③ | s2n-quic 能不能接一個繼承來的 UDP fd | ✅ **能**（`with_rx_socket` / `with_tx_socket`）⇒ 三條路線在這一欄無差別 |

## ⏳ 而補完之後新長出來的兩條

| | 欠什麼 | 怎麼補 |
|---|---|---|
| ④ | **s2n-quic 對認不出的 CID 做什麼**（丟包？stateless reset？）| 讀 `s2n-quic-transport` 的 endpoint 散布路徑 —— ★ ① 那條教訓正是：**鉤子的形狀說明不了失效形態** |
| ⑤ | **quiche 對認不出的 CID 做什麼** | ★ 嚴格說這一條不存在：quiche 是 sans-IO，訊息交給誰由我們決定，庫沒有機會自作主張。⇒ 登記在這裡只是為了讓這一欄三家都有話說 |

> ★ ★ ★ **補 ① 的過程本身值得記一句**：三條欠賬裡，只有 ① 的答案**改變了結論的方向** ——
> ②③ 只是把「未核實」換成「能」，而 ① 把「最壞是丟一個包」換成了
> **「新一代會主動殺掉老一代的在飛連接」**。
> ⚠ 而這兩種結局在 `Option<DatagramEvent>` 這個類型簽名上**長得一模一樣**。

---

# 相關

[尚未驗證的接縫](/verification/open-seams.md)（兩條前置的完整推導） ·
[決策紀錄](/governance/decision-log.md)（D11 由 **G103** 結案：取 `quiche`） ·
[技術棧](/platform/tech-stack.md) ·
[供應鏈現狀](/platform/supply-chain.md)（脫字號需求自帶上界；重複依賴讓整張迴歸網停擺過一次） ·
[TLS 與自動 HTTPS](/architecture/tls.md)
