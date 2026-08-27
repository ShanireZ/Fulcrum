---
type: 架构基线
title: 进程与组件边界
description: 枢衡是一个进程，它既是控制面也是数据面——这与旧版架构（控制面编排三个外部数据面）相反。
resource: ../../PLAN.md
tags: [架构, 承重墙, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-2
    resource: /references/plan.md
    title: PLAN.md §2 项目定位（一个进程、一份配置、一套观测）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G2（三家角色）、G9（回落定位）
---

★ 本页属**技术基线**，带真内容。它**服从** [`PLAN.md`](../../PLAN.md)；冲突时以 `PLAN.md` 为准。

# 枢衡是一个进程

它既是控制面也是数据面——★ **这与旧版架构（控制面编排三个外部数据面）相反**。

```text
                    ┌──────────────── fulcrum（单进程）────────────────┐
  DSL 文件 ─────────▶│  解析器 → 结构化配置（唯一内部事实）→ 校验器      │
  CLI / HTTP API ───▶│                     │                          │
                    │              事务式装载（原子生效 / 失败回退）    │
                    │                     │                          │
                    │   ┌─────────────────┴─────────────────┐        │
                    │   │        运行时（Pingora Server）      │        │
                    │   │  h1/h2 Service · L4 Service ·      │        │
                    │   │  QUIC/h3 Service · 后台服务         │        │
                    │   └─────────────────┬─────────────────┘        │
                    └─────────────────────┼──────────────────────────┘
                                          │
                       ┌──────────────────┼──────────────────┐
                       ▼                  ▼                  ▼
                    上游服务          本地静态文件        回落层（过渡期）
                                                        Caddy / Nginx
```

**流量路径默认只经过枢衡一跳。** 回落层是过渡期的例外，且在计划输出中**必须显式标出额外跳数**（G9）。

# 组件之间的边界

| 组件 | 职责 | 相关 |
|---|---|---|
| 解析器 | DSL → 结构化配置 | [配置分层](/architecture/configuration.md) |
| 校验器 + 事务式装载 | 原子生效 / 失败回退 | [管理面](/architecture/control-plane.md) |
| 运行时 | Pingora `Server` 上挂的一组 `Service` | [数据路径](/architecture/data-path.md) |
| 回落层 | 过渡期薄转发 | [回落层](/architecture/fallback.md) |

# ✅ 它怎么被托管（G31 / G33 / G34，结案）

**systemd `Type=notify`，前台运行。** `conf.daemon` 恒 `false`。

| 项 | 落法 | 换来什么 |
|---|---|---|
| 特权丢弃 | systemd 的 `User=` / `Group=` | ★ `daemonize` 依赖整条删除，**RUSTSEC-2025-0069 归零**；特权丢弃交给比任何 crate 都更经得起审计的 systemd |
| 日志 | stderr → journal | 不需要自己开日志文件、不需要 logrotate |
| 目录 | `ConfigurationDirectory=` `/etc/fulcrum`<br>`StateDirectory=` `/var/lib/fulcrum`<br>`RuntimeDirectory=` `/run/fulcrum`<br>`CacheDirectory=` `/var/cache/fulcrum` | systemd 负责创建并按 `User=`/`Group=` 设权限。★ **但每一项都可被配置覆盖**——D8 的磁盘缓存要能挂到别的盘 |
| PID 文件 | ⚠ **这一格已被 M1 spike #1 的实测部分推翻** —— 见下一节 | 原话是「`Type=notify` 下 systemd 自己跟踪 MainPID」，说的是 systemd 的 **`PIDFile=` 指令**不需要（那是对的，至今如此）。但它**不等于**「没有任何东西需要知道当前是哪一代」：`ExitType=cgroup` 之下 MainPID 在首次换代后归零，`ExecReload` 必须从一个**每代自写**的 pid 文件里找当前这一代 |
| 回落层 | `Wants=` / `After=` 声明依赖，**不监管** | 见 [回落层](/architecture/fallback.md) |

★ **一条独立于安全考虑的收益**：`daemonize()` 会 fork，而 pingora 自己的文档注释写着「`run()` 之前创建的任何线程都会丢失」。前台运行**解除了这条约束**——配置解析、ACME、回落层相关的东西可以在 `run()` 之前起来。

## ✅ 升级窗口内怎么活下来（M1 spike #1，实测）—— ★ 结论与原推断不同

原来写在这里的推断是：

> 新进程必须**落在同一个 cgroup 内**并**抢过 MainPID**——即 nginx `USR2` 那套。

★ ★ ★ **前半句对，后半句实测是错的，而且有害。**

| 口径 | 老代退出后 unit | `systemctl stop` |
|---|---|---|
| 交接 MainPID（原推断）| `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |
| **`ExitType=cgroup`，不交接** | `active` ✅ | **10 秒走完排空，`success`** ✅ |
| 两者都用 | `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |

交接过去的 pid 不是 systemd 亲生的（老进程 fork 的），systemd 会把它标成 alien，
此后停机**不再等它排空**。★ **整个失效在升级当时零症状**，代价要到下一次停机才兑现——
重启、`systemctl restart`、关机，全部变成硬杀连接。

**采纳的形状**（✅ **已由 G37 拍板**；D14 结案）：

| 项 | 落法 |
|---|---|
| unit 活过换代 | **`ExitType=cgroup`**（systemd ≥ 250）：cgroup 里还有进程就算活着 |
| 新进程进 cgroup | 老进程收 `SIGUSR2` 后自己 fork+exec（`-u`），然后给自己发 `SIGQUIT` 送 fd |
| MainPID | ★ **不交接。** 首次升级后 `MainPID` 归零，这是 `ExitType=cgroup` 的已知代价 |
| `ExecReload` | ⚠ **不能用 `$MAINPID`**（归零后展开成空，**第一次能成、第二次报错**：`systemctl reload` 退出码 1、journal 里是 `kill` 的 Usage，而 unit 仍 `active`——升级没发生，老一代照常跑着）。改从每代自写的 pid 文件里找 |
| `NotifyAccess` | 可收紧到 `main`——不交接之后，发通知的永远是主进程自己 |
| `TimeoutStopSec` | ≥ `grace_period_seconds` + `graceful_shutdown_timeout_seconds`。★ **产品默认是 30 + 5 = 35 秒**（批 12 定的，见下一节），所以 `TimeoutStopSec=60` 够用 |

> ★ **推断不是证据。** M0 那次靠 spike 额外捞出了 UDP 分流那条风险；这一次 spike 直接
> **推翻了推断本身**，并另外捞出一条新缺陷（移交来的监听 fd 没有 `CLOEXEC`）。
> 全部数据见 [M1 spike #1](/verification/m1-systemd.md)。

## ✅ 产品二进制真的被托管起来（G78）

⚠ ⚠ **上一节描述的形状，产品二进制一度并不满足。**  收官那天实测：
按上表写一个 unit 去起 `fulcrum serve`，`systemctl start` **超时失败**
（`MainPID=0`、`Result=timeout`、`/run/fulcrum/` 空），而 journal 里数据面
本身好好地在 8080 上听着。

> ★ ★ ★ **为什么当时的门一条都没抓到**：M1 那三个 systemd 场景跑的是
> **spike 二进制**，而 spike 把这些自己实现了一遍。
> **一个 spike 证明的是「这条路走得通」，不是「产品走在这条路上」。**

批 12 把四件事接进 [`crates/fulcrum-server/src/process.rs`](../../crates/fulcrum-server/src/process.rs)：

| # | 落法 | 不做会怎样（都实测过） |
|---|---|---|
| ① | 等 `ExecutionPhase::Running` 后发 `sd_notify(READY=1)` | `systemctl start` 超时失败 |
| ② | 同一时刻写 pid 文件（write + rename，**先落文件再报就绪**）| `ExecReload` 无处可查当前这一代 |
| ③ | 装 `SIGUSR2` 处理器：fork+exec 自己（带 `-u`），成功后才给自己发 `SIGQUIT` | ⚠ **比「reload 无事发生」更糟**：SIGUSR2 的默认动作是**终止进程**，实测 `code=killed, status=12/USR2`，而 `systemctl reload` 自己**返回成功** |
| ④ | `grace_period_seconds` 默认 **30s**、`graceful_shutdown_timeout_seconds` 显式 **5s** | 留 `None` 就落到 pingora 的 `EXIT_TIMEOUT=300`，`TimeoutStopSec=60` 会在排空到 1/5 时 SIGKILL |

★ **停机预算写在启动日志里**（`停机预算约 35s（排空 + 收尾）`），因为
`TimeoutStopSec` 必须按它设，而它由配置决定 —— 只存在于源码里的话运维只能靠猜。

★ ★ **顺带查出并修掉一条**：换代（`-u`）时监听 fd 是**从上一代继承**的
（按地址字符串查 fd 表，UDS 的键就是路径），新一代不会重新 `bind()`。
此时若把管理 socket 的路径 unlink 掉，两代都在一个**没有名字的** inode 上 accept，
客户端按路径连过去只有 ENOENT —— **一次 `systemctl reload` 之后管理面永久失联**，
而日志里一切正常、socket 也确实在 listen。⇒ 只有**非换代**那一趟才清陈旧文件。

判据：[`tests/m1/product.sh`](../../tests/m1/product.sh)（M1 的第四个 systemd 场景，
**跑产品二进制**）。五条反证各自只红在它该红的那一步。
部署形状见[部署](/platform/deploy.md)。

# ✅ 线程模型（G35，结案）

★ ★ **最要紧的一条机制事实：线程不跨 service 共享。** `ServerConf` 的注释原话是 "The threads are not shared across services"——**每个 service 各起一套 runtime**，所以**总线程数 ≈ Σ(各 service 的 threads)**。枢衡注定有 4+ 个 service，**全局把 `threads` 设成核数会直接超订**（4 核 × 4 service = 16 个 worker 线程）。

| 机制 | 事实 | 出处 |
|---|---|---|
| 每 service 线程数 | `Service::threads()` 可逐 service 覆盖全局 `conf.threads`（默认 **1**）| `server/mod.rs:705` |
| 窃取开关 | ★ **`work_stealing` 是全局的**，不能逐 service 混用 | `server/mod.rs:742` |
| NoSteal 的坑 | ★ `current_handle()` 返回**随机一个线程**的 handle——「把工作交回本线程」的假设不成立 | `pingora-runtime/src/lib.rs` |

**决策**：

- **`work_stealing = true`（Steal）**，per-core 推到 **M3 用对拍数据定**。M0 实测 Steal 下自建服务全绿，是目前唯一有证据的配置。
- **线程数按 service 角色定默认，全部可被配置覆盖**。初值：L7 反代 = CPU 核数、L4 = 2、管理面/后台 = 1。★ **初值不是结论**，判据在 M3 的对拍数据里。
- **QUIC 的线程分配不在本轮**，~~随 [D11](/governance/open-questions.md) 在 M2 选库时一并定~~ ⇒ ⚠ ★ **D11 已已结案（取 `quiche`），而线程分配这一条并没有跟着定** —— quiche 是 **sans-IO**，事件循环整个由我们自己写 ⇒ 它现在是**实施批次自己的设计题**，不再挂在 D11 上。

# ★ 一条已被 M0 证实的运行时性质

Pingora 的 `Server` 上可以**同时挂原生服务与自建服务**，两者共用一张 fd 表且键空间自由：

- 原生 `listening::Service` 用裸 `addr:port` 作键
- 自建 `Service` 可以自己定键（M0 里用了 `m0-raw-tcp:0.0.0.0:8081` 这样的前缀形式）

★ **自建 `Service` 有义务自己取走 fd 并放回去**——这不是绕过设计，`Service` trait 的文档注释本就把它写成自建服务的义务。证据见 [M0 接缝验证](/verification/m0-seam.md)。

这条直接决定了 [数据路径](/architecture/data-path.md) 里三个入口能挂在同一个 `Server` 上而不牺牲优雅升级。
