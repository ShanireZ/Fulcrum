---
type: 项目治理
title: 待定清单
description: 不阻塞当前进度、但必须在对应里程碑开工前定下来的问题；只列仍然开着的那些。
resource: ../../PLAN.md
tags: [治理, 待定, 尚未落地]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 待定清单
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 决策清单（已结案的那些的结论）
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §11，本页只做导航。**
**待定项不是需求** —— 没拍板之前不得据以实现，也不得在别处当成已定事实复述。

⚠ **编号只增不减。** 已结案的 D 号不在本页，它们的结论在
[决策日志](/governance/decision-log.md) 指向的 `PLAN.md` §10 里；
一条被删掉的待定项与一条从来没有过的待定项，事后看起来一模一样。

# 仍然开着的

| | 待定项 | 最晚需要在 | 一句话 |
|---|---|---|---|
| **D19** | `cache { capacity … }` 改了之后 `POST /load` 不生效 | M2 收尾前 | 后端容量在打开时定死，而同一个块里的 `ttl` / `max_size` 换配置立刻生效 —— 两条子指令行为不同而配置文件上看不出来。批 H 只堵了更贵的那半（`disk` 变了回 409）→ [数据路径](/architecture/data-path.md) |
| **D30** | `status_class` 的**第 6 个值** | M2 收尾前 | `status == 0`（一个响应头都没写出去 —— 那不是「未知」，是**什么都没发生**）与 1–99 / 600+ 一起归 `none`，于是闭集是六个而不是五个。三条候选：维持现状／这类请求干脆不计（⚠ 那样一致性门就不成立了）／改用 `0xx` → [观测](/architecture/observability.md) |
| **D20** | 磁盘缓存的 I/O 在请求路径上是同步的 | M3 | 改成 async 等于把 `Storage` 接口劈成两份，数据面就要为「内存还是磁盘」分出两条路。⏳ **先量再定** —— M3 对拍的命中率与 p99 会给出这个数有多大 → [数据路径](/architecture/data-path.md) |
| **D31** | 缓存事件的**折叠口径与边界** | M3 | `hit`/`miss`/`stale`/`purge` 四个值比 `X-Fulcrum-Cache` 粗；`miss` 记在**回源**那一处而不是「查缓存没命中」那一处。⚠ **`purge` 与另外三个不在同一个分母里**（条目 vs 请求）⇒ `sum(cache_events_total)` 不是任何一个有意义的量；⚠ 上游连不上的请求**三格都不在**，上游故障时 `hit/(hit+miss)` 会偏高 → [观测](/architecture/observability.md) |
| **D21** | musl 产物的**构建宿主口径** | M4 打包前 | 仓库现有的构建镜像（Debian trixie）编不出 musl 产物：只有 `musl-gcc`（C），没有 `musl-g++`，而 BoringSSL 的 `ssl/` 是 C++。两条候选：Alpine 原生 + qemu 跑 aarch64／glibc 宿主 + musl 交叉工具链 → [musl + BoringSSL 静态链接](/verification/musl-boringssl.md) |
| **D23** | **产物里真的链接了哪几套 TLS** | M4 打包前 | 三个问题不是一个：**锁里写着哪些**（供应链门 4）·**依赖图里真有哪些**（门 5，`cargo tree -e all --target all`）·**产物里链接了哪些**（本条）。★ 门 5 是本条的超集，「多了一套」这个方向已经守住；欠的是「图里有、产物里其实没链接」那一半 → [供应链现状](/platform/supply-chain.md) |
| **D24** | musl 静态产物那一格**只覆盖 x86_64** | M4 打包前 | G13 承诺两个架构；aarch64 要在 qemu 上编整个产品。★ 不紧迫的理由：aarch64 产物一次都没发布过，缺的是首次验证不是回归 → [构建与验证](/platform/build-and-test.md) |
| **D29** | 自动 HTTPS 的**重定向端口写死 `:80`** | M4 发布前 | 合成出来的 308 站点端口是个字面量，配置面上说不出它。⚠ 不只是个默认值：HTTP-01 规定 CA 只连 80 端口，「让它可配」与「HTTP-01 还能用」有张力。★ 门禁里七个场景因此隐式共用 `127.0.0.1:80`，这件事已写进 `AGENTS.md` 的端口表。⚠ **占着它并非无害**（旧的「无害」结论只是一次落在良性锁序上的采样）—— 现在由**各场景在自己的收尾里把 `:80` 还回去**来守 → [DSL 参考](/architecture/dsl-reference.md) |
| **D9** | 版本与兼容性策略 | M4 | 何时算 breaking change；DSL 与结构化配置各自的稳定性承诺 |

# 两条读法

**「等 X 发生就到期」的登记，没有任何东西会在 X 发生时通知它。**
D22 的到期条件满足了四轮才被发现，而这四轮里每一轮都读过这张表。
⇒ 到期条件要么挂进一道门，要么写成「每轮都要看一眼」的那种检查。

**一条待定项被拍板时，答案未必是它问的那个问题。**
D22 登记的是「探针何时变成常设的门」，而 owner 换掉的是**判据本身** ——
探针编的是 spike，答不了「产物是不是单静态二进制」。
⇒ 结案时先回头看这一条问的到底是什么。
