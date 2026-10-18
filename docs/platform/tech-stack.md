---
type: 技術基線
title: 技術棧
description: Rust + Pingora 0.8.1 打底 + quiche 掛 QUIC/HTTP-3 + 統一 BoringSSL（★ 2026-08-25 由 G103/G104 從 quinn+h3 與 rustls 換過來）；Linux 單靜態二進位散佈。
resource: ../../Cargo.toml
tags: [技術選型, 必讀]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-5
    resource: /references/plan.md
    title: PLAN.md §5 技術底盤、§5.1 G6 附帶的三條硬約束
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G1（Rust）、G6（Pingora + quinn/h3 + rustls）、G13（散佈）
---

★ 選型結論的權威在 [`PLAN.md`](../../PLAN.md) §5。本頁補**當前儲存庫裡的實際狀態**。

# 選型

| 項 | 選擇 | 依據 |
|---|---|---|
| 語言 | **Rust** | 效能可與 nginx/haproxy 同級（Go 做不到，而那正是 Caddy 的短處）；記憶體安全避開緩衝區類 CVE |
| 資料面底座 | **Pingora 0.8.1**（Apache-2.0，可被 GPL-3.0 吸收）| 白送零停機優雅升級、上游連接池、LoadBalancer 與健康檢查骨架、快取框架 |
| HTTP/3 | ⚠ ~~`quinn` + `h3`~~ → **`quiche`**（**G103**），以自建 Service 掛 QUIC 入口 | ★ Pingora 0.8 **不帶 HTTP/3**；★ quiche 把傳輸層與 HTTP/3 語義層裝在同一個 crate 裡（`quiche::h3`，無 feature 門控）⇒ 不需要那個停更 15.6 個月的 `h3` crate |
| TLS | ⚠ ⚠ ~~統一 rustls~~ → **統一 BoringSSL**（**G104**，同日）| ★ **「一套」這條目標沒變，變的是介質**：quiche 用 BoringSSL，繼續鎖 rustls 就必然兩套並存。⇒ 把 fork 當初刪掉的 `pingora-boringssl` 加回來，兩個入口共用同一個 `select_certificate_callback` |
| 設定 | **自研 DSL（Caddyfile 式）編譯到結構化設定** | 人寫 DSL、機器寫結構化；結構化那份是唯一內部事實 |
| 散佈 | **Linux x86_64 + aarch64 單靜態二進位（musl）+ systemd + deb/rpm** | 一個檔案、零相依；★ **不做官方容器鏡像**（G13）|

★ **選 Pingora 的判據值得記住**：它白送的恰是**自測最難、錯了最致命**的那部分（優雅升級、連接池、快取框架）。

# 當前儲存庫的實際狀態

★ **此後 workspace 裡有產品程式碼了**：`crates/fulcrum-config`（設定層）與 `crates/fulcrum`（產品二進位）。
`spikes/m0-seam` 與 `spikes/m1-systemd` 仍在，它們是**驗證裝置，不是產品程式碼**。
⚠ 這一句在  加進 `spikes/m1-systemd` 之後就已經過期了（寫的還是「唯一成員」），
直到  才被狀態句掃描抓到——**狀態句會腐爛，而腐爛的那一句讀起來和新鮮的一模一樣**。

```toml
[workspace]
resolver = "3"
members = ["spikes/m0-seam"]
```

| 項 | 值 |
|---|---|
| Rust edition | **2024** |
| 相依 resolver | **3** |
| 直接相依 | `pingora-core` · `tokio` · `async-trait` · `log` · `env_logger` —— ★ **全是最新**，見 [供應鏈現狀](/platform/supply-chain.md) |
| 建置鏡像 | `rust:1-trixie`（Debian 13，Rust 1.98.0）+ cmake + clang，見 [建置與驗證](/platform/build-and-test.md) |
| ★ **pingora 來源** | **不是 crates.io**，而是 [`vendor/pingora/`](../../vendor/pingora/FORK.md) 的 fork，經 `[patch.crates-io]` 接入 |

## ★ pingora 走的是本儲存庫的 fork

`Cargo.toml` 裡有一段 `[patch.crates-io]` 把 `pingora-core` 指向 `vendor/pingora/pingora-core`。

**動機**（G30）：crates.io 上的 `pingora-core 0.8.1` 用十條版本上界把 44 個傳遞相依釘在舊版，其中兩條帶真實安全公告。fork 只改各 `Cargo.toml` 的上界與隨之而來的 12 個呼叫點，**沒有任何行為變更**。

★ **讀 pingora 原始碼時看 `vendor/pingora/`，不要看 crates.io 上那份。** 完整改動清單、維護方式與已知的輕微行為差異（含改動 17 有意改的頭名大小寫）見 [`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md)。

# ★ 兩處決策與清單之間的差距

## ~~1. rustls 後端還沒被打開~~ ⇒ ✅ **已補上，而補上的是 `boringssl`**

> ⚠ **本節整段是（M0 時）的歷史，逐字保留，因為它記錄的是一個仍然成立的機制**
> ——「`pingora-core` 的 `default = []`，不開 feature 就一個 TLS 後端都沒有，而**未啟用的
> 可選相依根本不在鎖裡** ⇒ 常規稽核看不見它」。★ 那正是後來 `supply_gates.rs` 門 3 存在的理由。
>
> **今天的事實**：`crates/fulcrum-server` 帶的是 **`features = ["boringssl"]`**（G104，
> 推翻了 §5.1 第 1 條），門 3 守著這一條。⚠ 下面那句「當前 `Cargo.lock` 裡沒有 `rustls`、
> 沒有 `ring`」**已經不成立**：`ring` 是 `instant-acme` / `rcgen` 的正常相依；
> 而 `rustls` **仍然寫在鎖裡卻已不在相依圖裡**（`Cargo.lock` 是相依圖的超集，§10 實測）。

`pingora-core` 的 **`default = []`**——不開 feature 就**一個 TLS 後端都沒有**。當前 `Cargo.lock` 裡確實沒有 `pingora-rustls`、沒有 `rustls`、沒有 `ring`。

M0 不碰 TLS，所以這沒有暴露成問題。但 **G6「TLS 統一 rustls」目前在 `Cargo.toml` 裡沒有任何表達**，M1 第一天就要補：

```toml
pingora-core = { version = ">=0.8.1", features = ["rustls"] }
```

★ 該 feature 會連帶引入 `pingora-rustls` · `x509-parser` · `ouroboros`。

pingora-core 0.8.1 的 feature 全表：

| feature | 引入 |
|---|---|
| `default` | ★ **空** |
| `rustls` | `pingora-rustls` · `any_tls` · `x509-parser` · `ouroboros` |
| `openssl` / `boringssl` | 對應後端 + `openssl_derived` |
| `s2n` | `pingora-s2n` · … · ★ `lru` |
| `sentry` / `patched_http1` / `connection_filter` | |

## 2. ~~`quinn` 與 `h3` 還沒進清單~~ ⇒ ✅ 進清單的是 `quiche`

**庫選型（D11）由 G103–G105 結案：取 `quiche`**，事實表見
[HTTP/3 庫選型事實表](/platform/http3-libraries.md)。
✅ 兩條前置都已結案：「升級期 QUIC 連接歸屬」按 DCID 跨處理程序轉交（G109），
「BoringSSL 與 musl 靜態連結」驗過通過（見
[musl + BoringSSL 靜態連結](/verification/musl-boringssl.md)）。
⚠ ⚠ 後者的卡點不在 musl 也不在 BoringSSL，**在建置宿主**：`boring-sys` 的 bindgen 要
`dlopen`，而 Alpine 上 build script 預設是靜態的 ⇒ 由此立過 **D21**（建置宿主口徑）——
**儲存庫現有的這張建置鏡像編不出 musl 產物**（Debian 沒有 `musl-g++`）。
✅ **D21 已由 G131 結案**：口徑取 **Alpine 原生 + qemu 跑 aarch64**，⛔ 決定性理由是
另一條候選會給**供應鏈**新增一整套 C/C++ 交叉編譯器，而不是「Alpine 更方便」。

# 三條不可回頭的硬約束（G6 附帶）

完整表述見 `PLAN.md` §5.1，逐條落點見 [決策紀錄](/governance/decision-log.md)：

1. ⚠ ~~**TLS 後端鎖死 rustls**，動態證書走 `ResolvesServerCert`~~ → ★ **由 G104 推翻**：後端統一 **BoringSSL**，動態證書走 `select_certificate_callback` → [TLS](/architecture/tls.md)
2. **tower 中介軟體用不上**（與 `ProxyHttp` 階段模型同名不同物）
3. ~~自建 QUIC/L4 監聽器必須接入 socket 移交~~ → ✅ 已由 M0 解除

# 相關

[供應鏈現狀](/platform/supply-chain.md) · [相依策略](/platform/dependency-policy.md) · [建置與驗證](/platform/build-and-test.md)
