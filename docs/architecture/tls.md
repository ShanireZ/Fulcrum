---
type: 架構基線
title: TLS 與自動 HTTPS
description: ⚠ 2026-08-25 起後端是 BoringSSL 不是 rustls（G104 推翻 §5.1 第 1 條）；h1/h2 與 QUIC 共用一套，動態證書走 select_certificate_callback；On-Demand 未配存取資格則啟動失敗。
resource: ../../PLAN.md
tags: [架構, 承重牆, 必讀, 易錯, TLS]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 1 條硬約束（TLS 後端統一 BoringSSL —— ⚠ 原文鎖的是 rustls，已由 G104 推翻）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G104（TLS 棧統一 BoringSSL，推翻 G6 原來的 rustls 口徑）、G12（自動 HTTPS）、G15（On-Demand 存取資格強制）
  - id: plan-10-d4
    resource: /references/plan.md
    title: PLAN.md §10 G53–G57（D4 結案：ACME 用戶端、挑戰類型、證書儲存、續期與失敗退避、DNS-01 的落法）
---

★ 本頁屬**技術基線**，帶真內容。它**服從** [`PLAN.md`](../../PLAN.md)；衝突時以 `PLAN.md` 為準。

> ⚠ ⚠ ⚠ ★ ★ ★ **本頁的 TLS 後端前提整個變了 —— 請先讀這一段再往下看。**
>
> **G104**（§10）**推翻了 §5.1 第 1 條**（「TLS 後端鎖死 rustls……不可回頭」），
> 後端改為**統一 BoringSSL**。成因是 **G103 取 `quiche` 做 HTTP/3**，
> 而 quiche 用 BoringSSL、依賴表裡沒有 rustls ⇒ 繼續鎖 rustls 就必然「兩套 TLS 並存」，
> 而那正是本頁「最容易在哪做錯」**第 3 條**點名的形狀。
>
> | 這一條 | 現在是什麼 |
> |---|---|
> | 動態挑證書的介面 | ~~`ResolvesServerCert`~~ → **`boring::ssl::SslContextBuilder::set_select_certificate_callback`**（直接拿到 `ClientHello`）|
> | 兩個入口的關係 | **仍然共用一套** —— 目標沒變，介質從 rustls 換成 BoringSSL |
> | On-Demand 握手期現籤 | ★ **變容易了**：`set_async_select_certificate_callback` 是**原生非同步**的，而 `UNWIRED` 裡 `on_demand` 的登記理由正是「`resolve()` 是同步的，需要一座橋」⇒ **那條理由到期了** |
>
> ✅ ★ ★ **產品的監聽器側已經換完了。**`crates/fulcrum-tls` 現在是 `CertKey`（`X509` + `PKey`）+ `set_select_certificate_callback`，`pingora-core` 掛 `boringssl` feature。
> ✅ ★ ★ **第 ② 處 —— L4 的 ClientHello 預讀也換完了。**
> `peek_client_hello` 現在起**一臺真的握手狀態機 + 一條記憶體傳輸**，在早回呼裡抄走 SNI 與 ALPN
> 之後**當場把握手掐掉**；⚠ 記憶體傳輸的**寫那一半一律丟掉** —— 我們不是這條 TLS 連接的對端，
> 往真用戶端寫一個位元組（哪怕只是一個 alert）都等於冒充上游說話。
> ⇒ `pingora-rustls` 退出根 workspace 的依賴圖，**fork 改動 8 / 8b / 10 三條全部刪除**。
> ★ ★ **一處行為變化，而它是變好的那一側**：早回呼發生在幾乎所有 ClientHello 檢核**之前**，
> ⇒ 一個完整 TLS 棧會當場拒掉的 hello（缺 `signature_algorithms`）照樣按 SNI 分流 ——
> **「能不能分流」終於只取決於「能不能讀出 SNI」**，而不再取決於某個 TLS 棧的嚴格程度。
> ✅ ✅ ★ ★ ★ **第 ③ 處 —— 出站 HTTPS 也換完了，三處全換到此收口。**
> `crates/fulcrum-acme/src/https.rs` 自己寫了一個 `tower_service::Service<Uri>` 連接器
> （底下是 fork 裡已有的 `pingora_boringssl::tokio_ssl::SslStream`，**新增 0 個包**），
> **ACME 協定本身**與**原生 DNS 供應商的 API** 從此共用它 —— 一套信任庫、一套檢核、一份 ALPN。
> ⚠ 檢核一行都不是自己寫的：鏈走 `SslConnector::builder` 裡的 `set_default_verify_paths()`，
> 名字走 `configure()?.verify_hostname(true).into_ssl(domain)`。
> ⚠ ⚠ **「產物裡只有一套 TLS」現在成立了，而證明它的不是門 4** ——
> **`Cargo.lock` 是依賴圖的超集**：`hyper-rustls` / `rustls` / `schannel` **仍然寫在鎖裡**
> （`instant-acme` 的 `ring` feature 裡那句 `hyper-rustls?/ring` 把它留住了），
> 而 `cargo tree -e all --target all -i rustls` 已經是 **nothing to print**。
> ⇒ 新立 **門 5**（讀 `cargo tree`，`supply_gates.rs`）；門 4 的口徑收窄成「鎖裡寫著哪些」。
> ★ ★ ★ **門 4 曾經在註解裡預言「③ 做完時我會先紅一次提醒你改」—— 它沒紅。**
> 一道門的註解寫「本門守 X」不等於它判得動 X，這一次連**它會不會紅**都預言錯了。
> ★ 順帶兩條：fork 改動 **8 / 8b** 在 BoringSSL 那側**不需要對應物**（`TlsSettings`
> 對 `SslAcceptorBuilder` 有 `DerefMut`）；`pingora-boringssl/src/ext.rs` 自帶
> `suspend_when_need_ssl_cert` —— **On-Demand 要的那座橋不用自己造**。⇒ ★ **本頁下面凡是描述 rustls 的段落，
> 現在都是「當前實作」而不是「目標形態」** —— 兩者第一次分開了，讀的時候要分清。
> ✅ ★ **那條未驗前置已驗過，通過**：完全靜態的 musl 二進位在 `FROM scratch`
> 裡跑完真的 QUIC 握手，且 `set_select_certificate_callback` **真的被呼叫**、讀得到 SNI
> ⇒ **G103 不重議**。記錄 [`musl + BoringSSL 静态链接`](/verification/musl-boringssl.md)。
> ⚠ ⚠ 但它換來一條新的：**儲存庫現有的建置鏡像編不出 musl 產物**（Debian 沒有 `musl-g++`），
> 已立 **D21**，✅ **已由 G131 結案**（口徑 = Alpine 原生 + qemu 跑 aarch64）。
> ★ 這一條當初就不擋本頁描述的遷移開工。

> ✅ **TLS 那條脊樑已經完成實作並跑通**（`crates/fulcrum-tls` + `crates/fulcrum-server/tls.rs`）：
> 按 SNI 動態挑證書、每域一目錄的 PEM 儲存（原子寫 + `flock`）、續期判定（ARI → 1/3 → 退避）、
> `tls <cert> <key>`。端到端由 `tests/serve/run.sh` 驗過：`--cacert` 驗籤通過、
> ALPN 協商到 h2、未知 SNI 被拒絕握手。
> ✅ ★ **全域 `default_sni`（不帶 SNI 的用戶端）也接上了** —— 裝載時把**名字**交給
> `SniResolver`（不是當時那張證書：存證書的話，自動簽發那張會因為「裝載時還沒簽下來」
> 而永遠取不到，續期換掉之後那份副本還會一直髮舊的）。⚠ 它此前是本儲存庫
> 「DSL 認得、編譯得過、執行環境零呼叫方」的活樣本，**而且不在 `UNWIRED` 裡**
> ⇒ 裝載紀錄一個字都不說。判據：`tests/serve/run.sh` 9d（真流量，不帶 SNI 要拿到
> 那張證書）＋ 9c 的反向那半（配著它，未知 SNI 仍須被拒 —— 它不是 `fallback_sni`）。
> ✅ **ACME 本體（自動簽發/續期）已全部串接**——三種挑戰（TLS-ALPN-01 主、
> HTTP-01 備、DNS-01 通配符）、原生 Cloudflare / DNSPod、ARI 續期，門禁裡對著真 CA（pebble）
> 跑兩個端到端場景；**並且已經在真實網域上對生產 Let's Encrypt 真用著**。
> ⏳ **仍沒串接的只剩 `tls internal` 與 On-Demand**——它們在 `fulcrum_runtime::UNWIRED` 裡
> 逐條登記著、裝載時會打出來，★ 那份登記表由 `tests/unwired_contract.rs` **雙向**釘住
> （Rust 一份、`dsl-reference.md` 一句），**以它為準，不以這段散文為準**。
>
> ⚠ ⚠ **完成實作時撞到一件必須記住的事**：Pingora 0.8.1 的 rustls 監聽器
> **根本夠不到 `ResolvesServerCert`**，見下面「一條硬約束差點無路可走」。

# ~~統一 rustls（G6）~~ → 統一 BoringSSL（G104）

**目標形態（G104）**：h1/h2 入口（Pingora）與 QUIC 入口（**quiche**）**共用同一套 BoringSSL 設定**：
一套證書儲存、一套 ALPN 協商、一套工作階段快取。

**當前實作**：✅ **四處全是 BoringSSL，而且 h3 那一處已經在跑** ——
h1/h2 入口、L4 的 ClientHello 預讀、出站 HTTPS，加上 **QUIC 入口**
（`quic::listener::build_quic_config` 把同一個 `SniResolver` `install_into` 一個
`SslContextBuilder`，再交給 `quiche::Config::with_boring_ssl_ctx_builder`）。
★ ★ 「兩個入口共用同一個 `select_certificate_callback`」從此**不是目標形態，是實作**：
HTTP/3 端到端場景 [`tests/h3/run.sh`](../../tests/h3/run.sh) 全程用 `--cacert`（不是 `-k`）
⇒ **同一張證書經 BoringSSL 裝進 quiche 之後在 QUIC 上真的驗得過**，這一條有判據。
⚠ 這一行原本寫著「**當前實作**：h1/h2 走 rustls」，而監聽器側換完的當天它就不成立了。
★ **一句被標註為「當前實作」的話，恰恰是最會爛的那一種**：它按定義就是會過期的，
而沒有任何東西在檢查它。

## ★ ★ ~~這是一條不可回頭的硬約束~~ → 它已被推翻了

> ~~**rustls 後端不支援 `certificate_callback`。動態證書選擇必須實作 `ResolvesServerCert`。**~~
>
> ~~自動 HTTPS 與 On-Demand TLS 都建在這個介面上，因此 **TLS 後端從第一天起鎖死，不留雙路徑**。~~

★ ★ ★ **原文保留，因為那句理由到今天依然為真** —— rustls 確實沒有 `certificate_callback`。
**變的不是這句話，是我們不再站在 rustls 那一側了。**

> ★ ★ **留一句方法論：「不可回頭」約束的是我們自己不要反覆，不是約束事實不許變。**
> ⚠ 一條**理由仍然成立、而結論已經不適用**的約束，比一條明顯過時的約束**更難發現** ——
> 因為讀到它的人會先驗證那個理由，而理由是對的。

**現行（G104）**：後端統一 **BoringSSL**，動態證書選擇走
`SslContextBuilder::set_select_certificate_callback(|ClientHello| …)`。
★ **「不留雙路徑」這條目標一個字沒變** —— 恰恰是為了守住它，才取「統一到 BoringSSL」
而不是改動面更小的「兩套並存」。

## ⚠ 2026-09-24 起預設帶後量子金鑰交換（boring 5 的預設，owner 拍了沿用）

rebase 到 pingora 0.9.0 時 `boring` 從 4 升到 5。boring 4 要靠 `pq-experimental` 給 BoringSSL
打補丁才有後量子混合組（fork 當年把它去掉了，見 [`FORK.md`](../../vendor/pingora/FORK.md) §11），
boring 5 把那個補丁**無條件**打上 ⇒ 上面四處 BoringSSL 一起變，產品側沒寫一行設定。

- ⚠ 後量子組的 key share 大得多 ⇒ 用這類組的 ClientHello 在 QUIC 上一個 Initial 包裝不下、會拆成多個。
  由 [`tests/h3`](../../tests/h3/run.sh) 與 [`tests/quic-relay`](../../tests/quic-relay/run.sh) 端到端備援：
  兩處的用戶端都是 curl 8.14.1 + OpenSSL 3.5.6；在同一張鏡像裡實測這份 libssl 的預設組
  （`openssl s_client` 對 `s_server`，curl 不指定 `--curves` 時用的就是它）協商出 `X25519MLKEM768`
  ⇒ 這個用戶端**本來就發**後量子 key share。★ 所以「多包 Initial」在 rebase 之前就在發生，變的只是伺服器端現在也會選這個組。
- ★ 壓測裡 TLS 握手那一類要把四家的組**顯式寫成同一組**，見 [`bench/README.md`](../../bench/README.md)。

★ **一處現實差距，查了一半**：`pingora-core` 的 `default = []`，
**rustls 後端要顯式開 `features = ["rustls"]`**。M0 不碰 TLS，所以這個 feature
**從來沒有被打開過**——G6 這條決策至今在 `Cargo.toml` 裡沒有任何表達。

★ ★ ★ **去補的時候發現它不是補一行 feature 的事**，全部證據見
[rustls 接縫驗證](/verification/m1-rustls-seam.md)：

- **打開它，產品依賴圖 133 → 175**——一次進來 **42 個從未受審的 crate**；
- **三道門（G30 上界稽核／迴歸網／`supply-audit.py`）結構上都夠不到 `pingora-rustls`**，
  因為未啟用的可選依賴**根本不在根 `Cargo.lock` 裡**；
- 裡面有一條真缺陷：明明裝的是 ring，`aws-lc-rs`（69 MB C 原始碼）卻被一起編進產物。

**已辦**（G41／G42）：fork 裡兩處清單修掉、`rustls-native-certs` 抬到 0.8；
**迴歸網現在帶 `--features pingora-core/rustls` 跑**，rustls 那一半程式碼第一次進了判據。

✅ 產品側那一半也補上了：`crates/fulcrum-server` 帶著那個 feature 之後多解析出 **46 個包**，
而 **`aws-lc-rs` 不在其中**（`supply_gates.rs` 門 1 逐把鎖驗過）。

⚠ ★ **以上整節按歷史讀**：那個 feature
**今天叫 `boringssl`**（G104），而門 3（`supply_gates.rs`）守的正是這一條。
★ 原文不改寫，因為它記錄的是「打開一個此前沒人打開過的 feature，會一次進來 42 個未受審的 crate」——
**那件事與後端是誰無關**，換成 BoringSSL 之後同樣成立。

# ~~★ ★ ★ 一條硬約束差點無路可走：`ResolvesServerCert` 在 Pingora 0.8.1 裡夠不到~~

> ⚠ ⚠ ★ **本節整段是先前的歷史，而它描述的那處 fork 改動已歸零。**
> 逐字保留，因為它記錄的是「上游沒有一扇門通向那個介面」這件事**是怎麼被查清的**
> （四條路各撞一次牆，不是讀了一句文件）。
>
> **今天的事實**：TLS 後端是 BoringSSL，動態證書走 `set_select_certificate_callback`
> —— 而那是上游 `pingora-boringssl` **本來就有**的能力，不需要任何 fork 改動。
> **fork 改動 8 / 8b / 10 在 §10 全部刪除。**
>
> ★ ★ ★ **而這裡有一條比結論更值錢的東西**：下面那句寫著這處改動的歸零條件是
> 「**等 #632 或 #908 完成實作**」—— 而它歸零的時候，上游那兩條**一條都沒完成實作**。
> 三處 fork 改動的歸零條件全都掛在「上游做點什麼」上，**結果三處都是被第三件事釋放的**
> （G103 取 quiche ⇒ G104 換後端）。
> ⇒ **一條把自己綁在別人身上的到期條件，多半不是它真正的到期方式** ——
> 而沒有任何東西會在它真的到期時通知你。

上面那條「動態證書必須實作 `ResolvesServerCert`」在完成實作時撞了牆——
**上游的 rustls 監聽器沒有任何一扇門通向那個介面**：

| 門 | 實際情況 |
|---|---|
| `TlsSettings::intermediate(cert, key)` | 只接受證書與私鑰的**檔案路徑**，`build()` 裡寫死 `with_single_cert` |
| `TlsSettings::with_callbacks()` | **直接回傳錯誤**（"Certificate callbacks are not supported with feature rustls"）|
| 自己建構 `Acceptor` | 欄位私有，crate 外建構不出來 |
| `add_address(ServerAddress)` | 枚舉只有 `Tcp` / `Uds`，帶不上 acceptor |

後果：**一個連接埠只能一張證書、換證書要重載、握手期按 SNI 現籤無從實作**——
而自動 HTTPS（G12）與 On-Demand（G15）都建在那條路上。

**處置：fork 里加了 `TlsSettings::with_cert_resolver`**（見
[`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md) §8）。
⚠ 這是 fork 的**第一處「加能力」改動**——加能力比抬版本上界更貴，
上游一旦改動 `TlsSettings` 的形狀就要手動重貼。★ 但它沒有替代方案：
不加就等於放棄兩條已經拍板的決策。
❌ **投稿五已撤銷（G68）**：同一訴求上游已有 **9 條** issue/PR，
其中 #632 與我們逐點相同且更完整 —— **不發第 7 份**。★ 這處改動的歸零條件因此變成
「等 #632 或 #908 完成實作」，而不是「等我們那份被合」。

# 證書獲取的三條路

| 來源 | 觸發時機 | 說明 |
|---|---|---|
| 設定中宣告的網域 | 啟動 / 設定裝載時 | 預設行為，提前簽發與續期 |
| **On-Demand** | TLS 握手時按 SNI | 面向多租戶與使用者自定義網域 |
| 外部提供的 PEM | 設定中顯式指定 | 企業 CA、已購證書、內網自籤 |

# ✅ ACME 落法（D4 已由 G53–G56 結案）

| 項 | 決定 |
|---|---|
| 用戶端庫 | **`instant-acme`**，上層自己搭（G53）|
| 挑戰 | ★ **TLS-ALPN-01 主 + HTTP-01 備 + DNS-01**（G54）|
| 儲存 | 每域一目錄的 PEM（G55）|
| 續期 | **ARI 優先**，退回「剩餘壽命 1/3」（G56）|

## ★ ★ 選 `instant-acme` 的理由是介面形狀，不是「更輕」

~~§5.1 第 1 條鎖死 rustls，動態證書必須走 `ResolvesServerCert`~~（⚠ **由 G104 推翻**，
現在走 BoringSSL 的 `select_certificate_callback` —— ★ **但選 `instant-acme` 的理由不受影響**：
它講的是「ACME 協定庫不該替我們管 acceptor 與存取資格」，與後端是誰無關）；而 On-Demand TLS 要在
**握手期按 SNI 觸發簽發並做存取資格控制**。這套邏輯必須長在樞衡自己的證書管理器裡——
`rustls-acme` 那類高層庫的模型是「給你一個 acceptor」，與 Pingora 的監聽器不是一套，
存取資格控制也塞不進去。`instant-acme` 只做 RFC 8555 協定本身，挑戰求解與儲存排程歸我們。

⚠ ⚠ **寫依賴時必須 `default-features = false` 再顯式開 `ring`。**
`instant-acme` 的 default feature 裡**就含 `aws-lc-rs`**——照預設寫一行，
[rustls 接縫那一輪](/verification/m1-rustls-seam.md)把 aws-lc-rs 趕出依賴圖的成果**當場作廢**。
★ 這是「上游 crate 的 `default` 是一條靜默的供應鏈入口」的**第四次**現身，
**完成實作時要有一道門守住它**，別只寫在註解裡。

## ★ ★ 通配符只吃一層（RFC 6125），而站台索引比它寬

`*.example.com` 的證書覆蓋 `a.example.com`，**不覆蓋** `example.com`，
也**不覆蓋** `a.b.example.com`。這不是我們的偏好，是 RFC 6125 與瀏覽器的實際行為。

⚠ 放寬成「後綴匹配」的後果很難查：**伺服器端挑了一張證書、用戶端拒絕它**，
而伺服器端紀錄裡只有一次成功的握手。

✅ **站台索引（路由那一側）曾經是寬的**（`ends_with`，任意層都命中），兩邊語義不一致 ——
**D18 由 G66 結案**：收緊站台索引，且**兩側共用同一份實作**
（`fulcrum_config::host::wildcard_covers`）。★ 不是拿契約測試釘住兩份，
而是讓「兩份各自漂」在結構上做不到。

## 證書儲存：兩條約束來自「兩代處理程序並存」

佈局：`<state>/certs/<issuer>/<domain>/{cert.pem, key.pem, meta.json}`，私鑰 `0600`，
目錄由 systemd `StateDirectory=` 託管（G33）且可覆蓋。

★ ★ **升級視窗內兩代處理程序共享同一個證書目錄**（M0 已證兩代並存），
而 On-Demand 簽發是**執行期**行為，兩代可能同時寫同一個網域。所以：

1. 寫入一律**寫臨時檔案再 `rename`**（同目錄內原子替換）；
2. 跨處理程序用**檔案鎖**串行化同一網域的簽發——否則兩代各簽一張、互相覆蓋，
   還各自消耗 CA 的速率配額。

## 續期不寫死天數

先問 CA 的 **ARI**（RFC 9773，`instant-acme` 自帶、Let's Encrypt 已提供）拿建議視窗；
查不到時退回「剩餘壽命 1/3」。★ **Let's Encrypt 正在推 6 天短壽命證書**——
「到期前 30 天」在那種證書上等於**永遠在續期**。
★ ARI 還有比例制給不了的東西：CA 因安全事件需要**提前批量撤銷重籤**時，它是唯一的通知渠道。

失敗走指數退避（封頂）+ **隨機抖動** + 指標告警。抖動不是可選項：
同機幾十個網域同時到期時，不抖動會把重試打成尖峰。

★ **帶 `replaces` 下單被 CA 拒了，就去掉它再下一次**（G166）。不退回的話：失敗進退避，
下一輪拿同一張證書算出同一個 `replaces`，CA 再拒 —— 這個網域一直到過期都續不上，
而 `replaces` 換來的只是速率上的優惠。只認 CA 回的 problem 文件，連不上之類不退回。
⚠ ARI 標識裡的序列號照 RFC 9773 §4.1 **帶著** DER 符號位元組（首位元組 ≥ 0x80 時補的那個 `00`），
⛔ 不為遷就哪個 CA 剝掉它：pebble v2.10.1 就認不出帶它的標識（issue #550，約 1/255 的證書），
那正是這條退回兜住的情形。

# ✅ DNS-01 的落法（D15 已由 G57–G59 結案）

**原生 Cloudflare + DNSPod，其餘走 exec hook。** ★ 兩家是查出來的不是拍的——
實測 `example.com` 的 NS 在 **DNSPod**、`example.net` 在 **Cloudflare**，
即 M1 退出條件裡的兩個網域分屬兩家，而這兩家又分別是全球第一與大陸第一。

> ★ **形狀是「不先建體系」**：原生支援按真實用到的一家一家加。
> §6.2 不做服務發現整合，而**一個 DNS 供應商動物園是同一個形狀**。

★ **通配符證書是 M1 的交付內容**（G58），已寫進 §7 M1 的退出條件。
⚠ 代價：DNS-01 因此位於 M1 的關鍵路徑上，它出問題會直接卡驗收。

★ ★ **一條必須寫死的實作約束**：要等 TXT 記錄**可見**了才能通知 CA 檢核，
而「可見」不等於「API 回傳 200」。**判據掛在行為上**——直接向該域的**權威 NS** 輪詢確認，
**絕不能只 sleep 一個固定秒數**（快時浪費、慢時直接籤失敗，而失敗要消耗 CA 速率配額）。

## ⚠ DNS 憑證比 On-Demand 被刷爆嚴重得多

拿到某域的 DNS 寫權限 = **能為該域簽發任意證書** + 能改 MX 劫持電子郵件。所以（G59）：

1. **憑證絕不寫進 DSL**——DSL 是要被 diff、被貼進 issue、被版本控制的東西；只從檔案或環境變數讀。
2. **能處理程序化檢核權限範圍的，啟動時就檢核並拒絕啟動**（Cloudflare 有 token 檢核端點）。
3. 檢核不了的，**強制在設定裡顯式宣告該憑證覆蓋哪些 zone**，超範圍簽發一律拒絕。

★ 形狀照 G15：**錯誤在啟動時暴露，不等被濫用才發現。**

# On-Demand 的強制存取資格（G15）

★ **未設定存取資格來源時，啟用 On-Demand 會導致啟動失敗**，而不是執行環境才發現被刷爆。

存取資格來源二選一（或並用）：

- **網域白名單 / 通配符**：無外部依賴，握手期零額外延遲
- **ask 端點**：握手時回呼外部 HTTP 服務詢問該網域是否允許簽發，面向網域集合動態的場景

存取資格之外還有三道保險：**簽發速率上限、失敗退避、指標告警**。

# ✅ ACME 的判據：門禁裡跑一個真的 CA（G64）

真實簽發只有兩條路：打真 CA（要真網域、會消耗速率配額，**不能進門禁**），
或者在門禁裡跑一個本機 ACME 伺服器。★ **而 G58 已經把「簽發並續期一次通配符證書」
寫進 M1 的退出條件**——沒有本機 CA 的話，那條退出條件只能靠人手動跑一次真網域來證明，
**不可重現**。

owner 已拍板：**加**。完成實作形態是**兩個**場景——
[`tests/acme/run.sh`](../../tests/acme/run.sh)（管「簽發」）與
[`tests/acme/renew.sh`](../../tests/acme/renew.sh)（管「續期」），
共用的那一半在 [`tests/acme/lib.sh`](../../tests/acme/lib.sh)：

| 角色 | 是誰 |
|---|---|
| CA | `pebble`（Let's Encrypt 官方測試 CA，與 boulder 同一批人寫的另一個 RFC 8555 實作） |
| 權威 DNS | `pebble-challtestsrv`，把任何網域解析到 `127.0.0.1` |
| 被驗的那一方 | `fulcrum serve` 自己 |

兩個二進位按 **sha256 逐架構釘死**在 `docker/Dockerfile.build` 裡，理由與基礎鏡像那條一樣：
浮動的 `releases/latest` 會讓門禁裡的 CA 哪天悄悄換掉，而**沒有一行輸出會說出來**。

★ ★ **三條寫這套判據時踩到、值得記下來的**：

1. **不能拿一張自籤的 `CA:TRUE` 證書直接當 pebble 的伺服器端證書。**
   curl 收得下，而 **產品裡的那個 TLS 用戶端不收**（當時是 rustls，報 `CaUsedAsEndEntity`），
   現場表現是「建 ACME 帳戶失敗：client error (Connect)」，看不出跟證書有關。
   ⚠ 真正要連上 CA 的是**產品裡的那個用戶端**，不是 curl——
   拿 curl 驗一遍會得到一份全綠的假報告。必須是「根 + 葉」兩級，葉還要有 `serverAuth`。
   ⚠ ★ **此後那個用戶端是 BoringSSL 了**（G104 第 ③ 處）。這條坑仍然成立，
   只是回報錯誤文字會變 —— ★ 而這正是「判據別釘在某個庫的錯誤串上」的一個現成例子：
   這一段釘的是**形狀**（自籤 CA 當葉子用），不是那個 `CaUsedAsEndEntity`。
2. **challtestsrv 的 `-defaultIPv6 ''` 不是可選的**：它預設同時回 A 與 AAAA（`::1`），
   而樞衡若綁在 v4 上，CA 走 v6 來驗就連不上——現場同樣只有一句「驗不過」。
3. **pebble 預設故意拒 5% 的合法 nonce**（逼用戶端實作 RFC 8555 §6.5 的重試），
   而 `instant-acme` 0.8.5 的重試上限是**三次**。留著它是對的（真 CA 就這樣），
   但要先把偶發概率算清楚，見 `tests/acme/run.sh` 裡那段註解。

## ★ ★ ★ TLS-ALPN-01：判據必須掛在 CA 的記錄上，不能掛在我們自己的紀錄上

TLS-ALPN-01（RFC 8737 / G54 的「主」）已串接。它的形狀是：
CA 向 `<域名>:443` 握手、ALPN 只提 `acme-tls/1`，伺服器回一張**自籤**證書，
帶一條 critical 的 `id-pe-acmeIdentifier` 擴充（內容是 key authorization 的 SHA-256），
CA 看完就斷開。**零路由佔用** —— 使用者的 `respond 403` 擋不住自己的證書籤發。

★ 兩張證書表**互不相通**是安全屬性：協商到 `acme-tls/1` 只查挑戰表（查不到就拒絕握手，
絕不回落到真證書）；沒協商到就絕不查挑戰表（否則普通訪客拿到自簽證書，瀏覽器回報錯誤，
而伺服器端紀錄裡是一次**成功**的握手）。

> ⚠ ⚠ ★ **這一批的反證用一種沒預料到的方式教了一課。**
>
> 把挑戰證書裡那條擴充去掉，本以為門會紅在「籤不出來」——**門全綠**。
> pebble 紀錄裡明明寫著 `authz … set INVALID`：破壞是生效的，
> 只是退避到點之後，G54 的「備」把它接住了，HTTP-01 一次就過。
> 而當時門裡那條「這個網域走的是 TLS-ALPN-01」的斷言**也是綠的**——
> 它證明的是「我們**試**了」，不是「它**成了**」。
>
> **一道分不出「成功」與「失敗了但被兜住」的門，恰好對這一批的全部內容是瞎的。**
>
> 修法：判據改掛在 **CA 自己的記錄**上——pebble 每次 HTTP 驗證都會打一行
> `Attempting to validate w/ HTTP: http://<域名>:`，TLS-ALPN-01 成功的話
> 這個網域根本不該出現在那些行裡；再配一條自證（同一把尺子必須量得出那個
> 確實走 HTTP-01 的網域），外加一條更粗的「一次健康的跑裡 CA 不該判任何 INVALID」。

## ★ ★ ★ 「續期」那半邊為什麼要單獨一個場景

G58 的原話是「至少簽發**並續期**一次通配符證書」。簽發那半邊由 `tests/acme/run.sh` 釘死了，
而**續期那半邊此前一條端到端判據都沒有** —— 簽發那一格驗到的是
「第二輪巡檢判**已是最新**」，那恰恰是**沒有**續期時的判據。

> ⚠ **「暫時不用續」與「該續時會續」是兩件相反的事。** 前者綠一萬次也證不出後者。
> 這一族的形狀在本儲存庫反覆出現：一個只見過「有」的斷言，與一個恆真的斷言無法區分。

做法：**另起一個 pebble 執行個體 + 獨立 state dir**，
用 profile 把證書壽命壓到 **~1.5 天**，讓**跑著的**樞衡自己的巡檢循環走到續期。
短壽命證書會汙染同一個 CA 下的其它斷言，所以兩個場景**絕不共用一個 CA**。

★ ★ **那個「1.5 天」是量出來的**（pebble v2.10.1）：

| 量到的 | 結論 |
|---|---|
| ARI 建議視窗 | `[notAfter − 寿命/3 − 1 天, notAfter − 寿命/3 + 1 天]` |
| 那個 ±1 天 | **絕對值，不隨壽命縮放**（90 天與 129700s 兩個量級各驗一次：預測 66s、實測 65s）|
| `certificateValidityPeriod` | **死設定**：解析進結構體了，CA 只認 profile，實測配 600 之後仍然裝載 7776000 |
| profile 名字 | 必須是 `default`，否則目錄裡沒有 default，用戶端不選 profile 時沒有備援 |

⚠ 所以**不能**把壽命調到分鐘級：那樣 `2L/3 − 86400` 是負數，ARI 視窗在證書出生之前
就已經開始，第二代第一輪巡檢直接續——跑得更快，卻**證不到「它自己等到了時候才動」**，
而那正是續期的全部內容。取 129700s ⇒ 視窗在簽發後約 65s 開始。

判據刻意**不是**「出現了一張新證書」——那分不出三種情形：按 ARI 續期、
舊的讀不出來於是重籤、每一輪都在重籤。所以它釘的是**誰下的決定**（紀錄裡 ARI 那一句，
只有 `should_renew` 的 ARI 分支會打）、**序列號變了**、續期那趟的挑戰**按 CA 的選擇走對了**
（CA 新建授權就必須重解 DNS-01、重複使用有效授權就不許重解；走哪一支從 CA 紀錄讀，
⚠ pebble 的 `PEBBLE_AUTHZREUSE=0` 仍有 1/100 重複使用，⛔ 不能當成「永不復用」）、
新證書**上了線**（真握手取序列號，不是看盤上的檔案），以及反向的那一半
（第一輪必須判「已是最新」，且 ARI 視窗還在將來）。

★ 續期那單的 `replaces` 必須**被 CA 接受**（pebble 說舊訂單已標成被替換、樞衡沒走退回）。
為此第一代的序列號若帶 DER 符號位元組，就在續期之前重籤一張（G166）：pebble v2.10.1
認不出那種證書的 `replaces`。⚠ 這段只對 v2.10.1 成立，版本一變本格就紅，逼升級的人回來刪掉它。

★ 三條反證各自只紅該紅的那幾條，其中一條最說明問題：**把「簽完熱裝」那一步摘掉，
盤上換了、HTTPS 仍然 200、只有「線上握手拿到的序列號」這一條紅**——
一道只驗「還能訪問」或「盤上變了」的門在那裡是全綠的。

## ★ ★ 原生 Cloudflare / DNSPod 與 G59（完成實作）

三條約束各自落在能被門守住的地方：

| G59 | 落點 | 誰守著 |
|---|---|---|
| 第 1 條：憑證絕不寫進 DSL | `dns <provider> <来源>` 只認 `env:名字` / `file:路径` | **編譯期錯誤** |
> ⚠ ⚠ ⚠ **第 1 條由 owner 改口徑**：憑證**可以**寫進 DSL 了（Caddy 形狀 ——
> 一份設定檔案就能跑完）。⇒ 代價挪到了三處、而不是消失：
> ① 裝載期**權限門**（設定對 other 可讀就拒絕啟動）；
> ② `compile` **預設脫敏**、`Secret` 的 `Debug`/`Display` 預設脫敏；
> ③ 脫敏產物**不許被 load**（否則 `«已脱敏»` 會被當憑證發給 CA，
> 而現場表現與「憑證真的寫錯」一模一樣）。詳見 [DSL 參考](/architecture/dsl-reference.md) §4.4。

| 第 2 條：能檢核的啟動時檢核並拒絕啟動 | Cloudflare **對宣告的每個 zone 查一次 id**（`GET /zones?name=…`）、DNSPod `Info.Version`，跑在 `run_forever()` **之前** | 啟動路徑 |

> ★ ★ ★ **Cloudflare 那一條  換過判據，而換的理由值得記住。**
> 原先打的是 `/user/tokens/verify`。Cloudflare 2026 年把權杖分成**使用者級 `cfut_`** 與
> **帳號級 `cfat_`**，而後者在使用者級端點上**必然**回 `1000 Invalid API Token` ——
> G59 把「對端說不行」判成 Fatal，於是**一把完全好用的帳號級權杖會讓樞衡拒絕啟動**，
> 錯誤資訊還理直氣壯地說「憑證不可用」。
> ⇒ 換成查 zone id 之後，判據嚴格更強：它同時證明了**權杖能用**、**夠得著這個 zone**、
> 以及 **`zones` 裡的名字真的存在**（打錯一個字母立刻現形，而不是等到第一次簽發）。
> ★ 這也是「**驗我們接下來真的要做的那件事**，而不是驗有沒有某個我們其實不用的介面認它」
> 這條口徑的第一次完成實作。
| 第 3 條：顯式宣告 zone，超範圍拒絕 | `tls { zones … }`，原生供應商**必填**；判定按**標籤邊界** | **編譯期錯誤** + 執行環境拒絕 |

> ★ ★ ★ **第 1 條必須是白名單。** 一個「看起來像 token 就回報錯誤」的黑名單實作，
> 要去猜什麼樣子算 token —— 而**猜錯的那一次，恰恰就是真 token 被放行的那一次**，
> 且沒有任何症狀。

> ★ ★ **第 3 條不等於「把權限做小了」。** DNSPod 的 token 是**帳號級**的，
> 覆蓋該帳號下的全部網域，也**沒有可問出範圍的端點**。宣告的價值在於：
> **越權那一刻是樞衡自己拒絕的，而且拒絕的理由在設定裡看得見**。

★ ★ **第 2 條有一處知情的取捨。** 「對端回話說不行」與「壓根沒連上」處置**相反**，
而分法掛在**類型**上（`VerifyError::{Fatal, Inconclusive}`），不是讓呼叫方去 match 錯誤串：

- `Fatal`（對端說不行 / 憑證根本讀不出來）⇒ **拒絕啟動**；
- `Inconclusive`（網路不通、超時）⇒ 打一條 **error** 繼續跑。

⚠ 後者與「『沒能檢查』當成『檢查通過』是栽過的形狀」**是有張力的**。
取捨理由：一次網路抖動不該讓整臺機器上**所有**站台都起不來，包括那些用靜態證書的。
★ 所以它**不是悄悄跳過**，是打 error 明確說出「這份憑證沒被驗過」。

### ⚠ ⚠ 這一批的判據邊界，與前幾批不同

單測能證明的是**內部自洽**：我們發出去的 URL / 方法 / 頭 / body 正是我們以為的那些，
給定一段文件裡那種形狀的回應、解析出來的東西正是我們以為的那些。

> **它證不了「我們對這兩家 API 的理解是對的」。** 拿一個我們自己寫的假服務去證後者，
> 就是「判據掛在替身上等於沒有判據」——假服務的行為也是我們自己想出來的，
> **兩邊同時錯的時候它照樣全綠**。
>
> ★ 那一條的判據只能是 **M1 退出時那兩個真網域**（`example.com` 在 DNSPod、
> `example.net` 在 Cloudflare）。這是 G57 當初把原生供應商單獨分一批時就寫下的話，
> 不是這一批的新說法。

★ 兩處容易想當然、各由一條測試釘住的地方：**dnsapi.cn 永遠回 HTTP 200**
（真正的結果在 body 的 `status.code` 裡，只看狀態碼會把每次失敗都當成功）；
**DNSPod 的憑證在 body 裡而 Cloudflare 的在頭裡**（一個只擋 `Authorization` 頭的
脫敏實作在 DNSPod 上完全失效，而失效的表現是憑證出現在紀錄裡）。

# 最容易在哪做錯

1. ★ ★ **照著 openssl 後端的例子寫 `certificate_callback`。** rustls 後端**沒有**這個東西。這是 G6 三條硬約束裡的第一條，[`AGENTS.md`](../../AGENTS.md) 也單獨重複了一遍——因為它是「看起來能用、實際編不過」的形態，而網上大量 Pingora 示例用的是 openssl 後端。
2. ★ **把「啟動失敗」實作成「執行環境告警」。** G15 的整個價值在於**錯誤在啟動時暴露**。降級成告警等於把它變回「被刷爆時才發現」——那正是它要防的。
3. **給兩個入口各配一套。** 一套證書、一套 ALPN、一套工作階段快取是 G6 的原話；兩套會在 On-Demand 簽發時出現「h2 拿到了新證書而 h3 還是舊的」這類只在特定路徑出現的問題。<br>★ ★ ★ **這一條正是 G104 取「統一到 BoringSSL」而不是「兩套並存」的理由。** 當時它針對的是「兩套 rustls」，而它對「一套 rustls + 一套 BoringSSL」**同樣成立、甚至更成立** —— ⇒ 一條寫在「最容易做錯」清單裡的話，在一次選型裡**直接決定了拍板方向**。

# 相關

[資料路徑](/architecture/data-path.md)（三個入口） · [安全基線](/platform/security-baseline.md) · [技術棧](/platform/tech-stack.md)
