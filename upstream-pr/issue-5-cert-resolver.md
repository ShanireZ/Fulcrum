# 投稿五（**已撤銷，不發**）· rustls 監聽器無法使用自定義證書解析器

> ❌ **狀態：2026-08-20 撤銷，owner 拍板「什麼都不做」。**
> **不開 issue、不開 PR、也不去別人的執行緒裡留言。** 下面的立論**沒有錯**，
> 但它**不該由我們再報一遍**——理由見下。fork 裡那條改動照舊自己養著。
>
> ## ★ ★ ★ 撤銷的理由：這件事上游早就有人報了，而且比我們做得多
>
> 按 G32「先查上游做沒做、有沒有人報過」搜了一遍（2026-08-20，`gh` 實測，不是讀網頁），
> **同一主題已有 9 條** issue/PR，最早的是 **2025-04-27**：
>
> | | 類型 | 狀態 | 說的是什麼 |
> |---|---|---|---|
> | [#594](https://github.com/cloudflare/pingora/issues/594) | issue | open，10 條評論 | 最早那條訴求：SNI based resolver |
> | ★ [#632](https://github.com/cloudflare/pingora/pull/632) | PR | open，`mergeable=false` | **就是我們這個寫法**，見下 |
> | [#599](https://github.com/cloudflare/pingora/pull/599) | PR | open | 另一條路（cert bundle） |
> | [#726](https://github.com/cloudflare/pingora/pull/726) | PR | open | 收 `ServerConfig` |
> | [#832](https://github.com/cloudflare/pingora/issues/832) | issue | open | 同一訴求，換個說法 |
> | [#916](https://github.com/cloudflare/pingora/issues/916) | issue | open | 暴露 `Acceptor` 建構器 |
> | [#833](https://github.com/cloudflare/pingora/pull/833) | PR | **closed** | TlsAcceptCallbacks，作者轉投 openssl |
> | ★ [#908](https://github.com/cloudflare/pingora/pull/908) | PR | open，8 條評論 | 接棒 #833，**當前最活躍**，一直在催評審 |
> | [#877](https://github.com/cloudflare/pingora/pull/877) | PR | **closed** | resolver support |
>
> ★ ★ **#632 與我們的做法逐點相同，而且更完整**：同樣是給 `TlsSettings` 加
> `Option<Arc<dyn ResolvesServerCert>>`、`build()` 裡二選一、`pingora-rustls` 再匯出那幾個類型；
> **它還順手把 `build()` 那個 panic 改成了 `Result`**——那是我們 fork 裡沒動的。
> 而且已有 **3 個獨立使用者**（JockeTF / tarka / sebadob 線上的專案）證實可用、各自維著 rebase。
>
> ★ ★ **它卡住的原因不是設計分歧，是合併衝突 + 維護者評審頻寬**：
> #908 那條線裡 `dpfeifer2` 從 6 月催到 8 月（`nojima` 說過「週末看一下」，之後沒下文）。
> ⇒ **再開第 7 份只會讓那個佇列更堵**，而我們自己的 `README.md` 裡就記著那條教訓：
> **「我可以幫你報」在別人早就報過時，等於宣告自己沒搜。**
>
> ## ⚠ 這條撤銷**不影響** fork 裡那條改動
>
> `TlsSettings::with_cert_resolver`（`FORK.md` §8）照舊留著——它是 M1 的承重牆。
> ★ 但要記住它的**歸零條件變了**：不再是「等我們的投稿被接受」，而是
> **等 #632 或 #908 完成實作**。到那天把 fork 那條刪掉，換成上游的介面。
>
> ---
>
> 以下是當初的草稿，**保留僅供歸檔**（如果哪天 owner 改主意要參與那幾條線，立論現成）。

---

## 標題

rustls listener cannot use a custom `ResolvesServerCert` (no SNI-based certificate selection)

## 描述（正文草稿）

用 `feature = "rustls"` 時，`listeners::tls::TlsSettings` 沒有任何辦法提供
rustls 的 `ResolvesServerCert`：

| 入口 | 現狀 |
|---|---|
| `TlsSettings::intermediate(cert_path, key_path)` | 只接受**檔案路徑**，`build()` 裡寫死 `with_single_cert` |
| `TlsSettings::with_callbacks()` | 在 rustls 後端**直接回傳錯誤** |
| 自己建構 `Acceptor` | 欄位私有 |
| `add_address(ServerAddress)` | 枚舉只有 `Tcp` / `Uds` |

後果：

- 一個監聽連接埠只能有**一張**證書；
- 換證書要重新啟動或重載設定；
- **按 SNI 在握手期選證書 / 現籤**（ACME 的 on-demand 形態）無從實作。

而 rustls 提供的正是這條路（`ResolvesServerCert`），且它是**唯一**一條——
openssl / boringssl 後端的 `certificate_callback` 在 rustls 下不存在，
`with_callbacks()` 自己也這麼說。

## 建議的改法（最小）

給 `TlsSettings` 加一個可選的 resolver，並在 `build()` 裡優先用它：

```rust
pub fn with_cert_resolver(resolver: Arc<dyn ResolvesServerCert>) -> Result<Self>;
```

有 resolver 時**完全不讀證書檔案**（於是 `intermediate()` 那個「檔案讀不到就 panic」
的行為與這條路無關）。

配套還要在 `pingora-rustls` 裡再匯出 `ResolvesServerCert` / `CertifiedKey` / `ClientHello`——
否則依賴方拿不到那幾個類型，只能自己再依賴一份 `rustls`，
而那會**多一處 feature 宣告**（`rustls` 的 default 裡含 `aws_lc_rs`，
參見投稿三 #965/#966 裡那條「兩個 provider 一起編進產物」）。

## 為什麼這不是「用別的後端就好」

`certificate_callback` 只在 openssl / boringssl 後端存在。選 rustls 的專案
（純 Rust、可 musl 靜態連結）目前**沒有任何**做 SNI 動態證書的辦法。

## 環境

- 上游 tag `0.8.1`
- 我們在 fork 裡按上面的形狀加了這條並跑通：`--cacert` 驗籤通過、
  ALPN 協商到 h2、未知 SNI 被拒絕握手（rustls 報
  `no server certificate chain resolved`，正是期望的行為）。
