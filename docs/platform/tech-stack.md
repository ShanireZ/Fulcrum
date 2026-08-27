---
type: 技术基线
title: 技术栈
description: Rust + Pingora 0.8.1 打底 + quiche 挂 QUIC/HTTP-3 + 统一 BoringSSL（★ 2026-08-25 由 G103/G104 从 quinn+h3 与 rustls 换过来）；Linux 单静态二进制分发。
resource: ../../Cargo.toml
tags: [技术选型, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-5
    resource: /references/plan.md
    title: PLAN.md §5 技术底盘、§5.1 G6 附带的三条硬约束
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G1（Rust）、G6（Pingora + quinn/h3 + rustls）、G13（分发）
---

★ 选型结论的权威在 [`PLAN.md`](../../PLAN.md) §5。本页补**当前仓库里的实际状态**。

# 选型

| 项 | 选择 | 依据 |
|---|---|---|
| 语言 | **Rust** | 性能可与 nginx/haproxy 同级（Go 做不到，而那正是 Caddy 的短处）；内存安全避开缓冲区类 CVE |
| 数据面底座 | **Pingora 0.8.1**（Apache-2.0，可被 GPL-3.0 吸收）| 白送零停机优雅升级、上游连接池、LoadBalancer 与健康检查骨架、缓存框架 |
| HTTP/3 | ⚠ ~~`quinn` + `h3`~~ → **`quiche`**（**G103**），以自建 Service 挂 QUIC 入口 | ★ Pingora 0.8 **不带 HTTP/3**；★ quiche 把传输层与 HTTP/3 语义层装在同一个 crate 里（`quiche::h3`，无 feature 门控）⇒ 不需要那个停更 15.6 个月的 `h3` crate |
| TLS | ⚠ ⚠ ~~统一 rustls~~ → **统一 BoringSSL**（**G104**，同日）| ★ **「一套」这条目标没变，变的是介质**：quiche 用 BoringSSL，继续锁 rustls 就必然两套并存。⇒ 把 fork 当初删掉的 `pingora-boringssl` 加回来，两个入口共用同一个 `select_certificate_callback` |
| 配置 | **自研 DSL（Caddyfile 式）编译到结构化配置** | 人写 DSL、机器写结构化；结构化那份是唯一内部事实 |
| 分发 | **Linux x86_64 + aarch64 单静态二进制（musl）+ systemd + deb/rpm** | 一个文件、零依赖；★ **不做官方容器镜像**（G13）|

★ **选 Pingora 的判据值得记住**：它白送的恰是**自测最难、错了最致命**的那部分（优雅升级、连接池、缓存框架）。

# 当前仓库的实际状态

★ **此后 workspace 里有产品代码了**：`crates/fulcrum-config`（配置层）与 `crates/fulcrum`（产品二进制）。
`spikes/m0-seam` 与 `spikes/m1-systemd` 仍在，它们是**验证装置，不是产品代码**。
⚠ 这一句在  加进 `spikes/m1-systemd` 之后就已经过期了（写的还是「唯一成员」），
直到  才被状态句扫描抓到——**状态句会腐烂，而腐烂的那一句读起来和新鲜的一模一样**。

```toml
[workspace]
resolver = "3"
members = ["spikes/m0-seam"]
```

| 项 | 值 |
|---|---|
| Rust edition | **2024** |
| 依赖 resolver | **3** |
| 直接依赖 | `pingora-core` · `tokio` · `async-trait` · `log` · `env_logger` —— ★ **全是最新**，见 [供应链现状](/platform/supply-chain.md) |
| 构建镜像 | `rust:1-trixie`（Debian 13，Rust 1.97.1）+ cmake + clang，见 [构建与验证](/platform/build-and-test.md) |
| ★ **pingora 来源** | **不是 crates.io**，而是 [`vendor/pingora/`](../../vendor/pingora/FORK.md) 的 fork，经 `[patch.crates-io]` 接入 |

## ★ pingora 走的是本仓库的 fork

`Cargo.toml` 里有一段 `[patch.crates-io]` 把 `pingora-core` 指向 `vendor/pingora/pingora-core`。

**动机**（G30）：crates.io 上的 `pingora-core 0.8.1` 用十条版本上界把 44 个传递依赖钉在旧版，其中两条带真实安全公告。fork 只改各 `Cargo.toml` 的上界与随之而来的 12 个调用点，**没有任何行为变更**。

★ **读 pingora 源码时看 `vendor/pingora/`，不要看 crates.io 上那份。** 完整改动清单、维护方式与已知的一处轻微行为差异见 [`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md)。

# ★ 两处决策与清单之间的差距

## ~~1. rustls 后端还没被打开~~ ⇒ ✅ **已补上，而补上的是 `boringssl`**

> ⚠ **本节整段是（M0 时）的历史，逐字保留，因为它记录的是一个仍然成立的机制**
> ——「`pingora-core` 的 `default = []`，不开 feature 就一个 TLS 后端都没有，而**未启用的
> 可选依赖根本不在锁里** ⇒ 常规审计看不见它」。★ 那正是后来 `supply_gates.rs` 门 3 存在的理由。
>
> **今天的事实**：`crates/fulcrum-server` 带的是 **`features = ["boringssl"]`**（G104，
> 推翻了 §5.1 第 1 条），门 3 守着这一条。⚠ 下面那句「当前 `Cargo.lock` 里没有 `rustls`、
> 没有 `ring`」**已经不成立**：`ring` 是 `instant-acme` / `rcgen` 的正常依赖；
> 而 `rustls` **仍然写在锁里却已不在依赖图里**（`Cargo.lock` 是依赖图的超集，§10 实测）。

`pingora-core` 的 **`default = []`**——不开 feature 就**一个 TLS 后端都没有**。当前 `Cargo.lock` 里确实没有 `pingora-rustls`、没有 `rustls`、没有 `ring`。

M0 不碰 TLS，所以这没有暴露成问题。但 **G6「TLS 统一 rustls」目前在 `Cargo.toml` 里没有任何表达**，M1 第一天就要补：

```toml
pingora-core = { version = ">=0.8.1", features = ["rustls"] }
```

★ 该 feature 会连带引入 `pingora-rustls` · `x509-parser` · `ouroboros`。

pingora-core 0.8.1 的 feature 全表：

| feature | 引入 |
|---|---|
| `default` | ★ **空** |
| `rustls` | `pingora-rustls` · `any_tls` · `x509-parser` · `ouroboros` |
| `openssl` / `boringssl` | 对应后端 + `openssl_derived` |
| `s2n` | `pingora-s2n` · … · ★ `lru` |
| `sentry` / `patched_http1` / `connection_filter` | |

## 2. ~~`quinn` 与 `h3` 还没进清单~~ ⇒ ✅ 进清单的是 `quiche`

**库选型（D11）由 G103–G105 结案：取 `quiche`**，事实表见
[HTTP/3 库选型事实表](/platform/http3-libraries.md)。
✅ 两条前置都已结案：「升级期 QUIC 连接归属」按 DCID 跨进程转交（G109），
「BoringSSL 与 musl 静态链接」验过通过（见
[musl + BoringSSL 静态链接](/verification/musl-boringssl.md)）。
⚠ ⚠ 后者的卡点不在 musl 也不在 BoringSSL，**在构建宿主**：`boring-sys` 的 bindgen 要
`dlopen`，而 Alpine 上 build script 默认是静态的 ⇒ 由此新立 **D21**（构建宿主口径）——
**仓库现有的这张构建镜像编不出 musl 产物**（Debian 没有 `musl-g++`）。

# 三条不可回头的硬约束（G6 附带）

完整表述见 `PLAN.md` §5.1，逐条落点见 [决策日志](/governance/decision-log.md)：

1. ⚠ ~~**TLS 后端锁死 rustls**，动态证书走 `ResolvesServerCert`~~ → ★ **由 G104 推翻**：后端统一 **BoringSSL**，动态证书走 `select_certificate_callback` → [TLS](/architecture/tls.md)
2. **tower 中间件用不上**（与 `ProxyHttp` 阶段模型同名不同物）
3. ~~自建 QUIC/L4 监听器必须接入 socket 移交~~ → ✅ 已由 M0 解除

# 相关

[供应链现状](/platform/supply-chain.md) · [依赖策略](/platform/dependency-policy.md) · [构建与验证](/platform/build-and-test.md)
