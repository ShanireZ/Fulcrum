---
type: 外部規範
title: 上游技術入口
description: Pingora、QUIC 生態、rustls 與三家參照物的官方文件入口。
resource: ../../PLAN.md
tags: [參考, 外部]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-12
    resource: /references/plan.md
    title: PLAN.md §12 官方技術入口
---

<a id="底座與依賴"></a>

# 底座與相依

| | 連結 | 用途 |
|---|---|---|
| **Pingora** | https://github.com/cloudflare/pingora | 資料面底座（G6）；★ 也是 [供應鏈現狀](/platform/supply-chain.md) 裡那些版本上界的來源 |
| **`quiche`** | https://github.com/cloudflare/quiche | ★ **QUIC + HTTP/3，D11 於 2026-08-25 取的就是它**（G103）。⚠ 它用 **BoringSSL** 不是 rustls —— G104 因此把 TLS 棧整體換了 |
| ~~`quinn`~~ | https://github.com/quinn-rs/quinn | ~~QUIC（D11 候選之一的組成部分）~~ —— **未取**；★ 落選的決定性一條：`Endpoint::handle()` 對認不出的 DCID **回 stateless reset**，會殺掉升級視窗裡的在飛連接 |
| `h3` | https://github.com/hyperium/h3 | HTTP/3；★ 停在 0.0.8，**自 2025-05-06 未再發版** |
| `rustls` | https://github.com/rustls/rustls | ★ 鎖死的 TLS 後端（G6）|

★ **查 Pingora 的 API 時釘住版本。** [`AGENTS.md`](../../AGENTS.md) 要求：不要跨版本推斷 Pingora / rustls / quinn 的 API 相容性，**對著釘住的版本跑 `cargo doc` 核實**。

# 三家參照物

這三家既是 [定位](/product/positioning.md) 裡要吸收長處、避開短處的物件，也是 [效能驗收標準](/verification/performance-bar.md) 的對拍物件，其中兩家還是過渡期的 [回落後端](/architecture/fallback.md)。

| | 連結 | 為什麼要讀 |
|---|---|---|
| Caddy API | https://caddyserver.com/docs/api | 全量熱載入 + 失敗自動退回的參照（G8）；★ 也是 G14 要修正的短處的出處 |
| HAProxy Runtime API | https://www.haproxy.com/documentation/haproxy-runtime-api/ | 增量 runtime 通道的參照（G8）；★ G18 判據的出處 |
| Nginx graceful control | https://nginx.org/en/docs/control.html | 優雅控制的參照 |

# 供應鏈查詢入口

[供應鏈現狀](/platform/supply-chain.md) 那份快照用的是這兩個：

| | 連結 |
|---|---|
| crates.io API（版本與發佈時間）| `https://crates.io/api/v1/crates/<name>` |
| OSV.dev 批量查詢（安全公告）| `POST https://api.osv.dev/v1/querybatch`，ecosystem 填 `crates.io` |

`tools/dep-check.py` 用的是前者。
