---
type: 外部规范
title: 上游技术入口
description: Pingora、QUIC 生态、rustls 与三家参照物的官方文档入口。
resource: ../../PLAN.md
tags: [参考, 外部]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-12
    resource: /references/plan.md
    title: PLAN.md §12 官方技术入口
---

# 底座与依赖

| | 链接 | 用途 |
|---|---|---|
| **Pingora** | https://github.com/cloudflare/pingora | 数据面底座（G6）；★ 也是 [供应链现状](/platform/supply-chain.md) 里那些版本上界的来源 |
| **`quiche`** | https://github.com/cloudflare/quiche | ★ **QUIC + HTTP/3，D11 于 2026-08-25 取的就是它**（G103）。⚠ 它用 **BoringSSL** 不是 rustls —— G104 因此把 TLS 栈整体换了 |
| ~~`quinn`~~ | https://github.com/quinn-rs/quinn | ~~QUIC（D11 候选之一的组成部分）~~ —— **未取**；★ 落选的决定性一条：`Endpoint::handle()` 对认不出的 DCID **回 stateless reset**，会杀掉升级窗口里的在飞连接 |
| `h3` | https://github.com/hyperium/h3 | HTTP/3；★ 停在 0.0.8，**自 2025-05-06 未再发版** |
| `rustls` | https://github.com/rustls/rustls | ★ 锁死的 TLS 后端（G6）|

★ **查 Pingora 的 API 时钉住版本。** [`AGENTS.md`](../../AGENTS.md) 要求：不要跨版本推断 Pingora / rustls / quinn 的 API 兼容性，**对着钉住的版本跑 `cargo doc` 核实**。

# 三家参照物

这三家既是 [定位](/product/positioning.md) 里要吸收长处、避开短处的对象，也是 [性能验收标准](/verification/performance-bar.md) 的对拍对象，其中两家还是过渡期的 [回落后端](/architecture/fallback.md)。

| | 链接 | 为什么要读 |
|---|---|---|
| Caddy API | https://caddyserver.com/docs/api | 全量热加载 + 失败自动回退的参照（G8）；★ 也是 G14 要修正的短处的出处 |
| HAProxy Runtime API | https://www.haproxy.com/documentation/haproxy-runtime-api/ | 增量 runtime 通道的参照（G8）；★ G18 判据的出处 |
| Nginx graceful control | https://nginx.org/en/docs/control.html | 优雅控制的参照 |

# 供应链查询入口

[供应链现状](/platform/supply-chain.md) 那份快照用的是这两个：

| | 链接 |
|---|---|
| crates.io API（版本与发布时间）| `https://crates.io/api/v1/crates/<name>` |
| OSV.dev 批量查询（安全公告）| `POST https://api.osv.dev/v1/querybatch`，ecosystem 填 `crates.io` |

`tools/dep-check.py` 用的是前者。
