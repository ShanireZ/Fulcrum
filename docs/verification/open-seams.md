---
type: 验证记录
title: 尚未验证的接缝
description: 还有哪些技术假设没被代码证明；已解除的那些只留结论。
resource: ../../PLAN.md
tags: [验证, 风险, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 主要风险
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 G6 附带的三条硬约束
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 待定清单
---

**本页记的是尚未被代码证明的技术假设**，每一条的目标都是变成「已验证」或「此路不通」。

# ⏳ 还开着的

| 接缝 | 已知事实 | 待验证 |
|---|---|---|
| **构建镜像编不出 musl 产物** | 实测：Debian trixie 只有 `musl-gcc`（C），没有 `musl-g++`，而 BoringSSL 的 `ssl/` 是 C++ | ⏳ 挂号 **D21**（构建宿主口径）。★ 不挡开工 —— 发布流水线本身还不存在 |
| **产物里真的链接了哪几套 TLS** | 依赖图里只有一套（`cargo tree -e all --target all` 为空） | ⏳ 挂号 **D23**。「图里有 ≠ 产物里链接了」，而这一步没有判据 |
| **`bind()` 攥着全局 `ListenFds` 锁**（本仓 vendor 的 0.8.1 里） | `ListenerEndpoint::listen` 先 `fds_table.lock().await` 再在**持锁状态下** `bind()`，而 `bind_tcp` 重试 30 次 × 1 秒 ⇒ 一个被占端口把整把锁停住 30 秒，**所有还没拿到锁的监听器一起起不来**（2026-08-28 实测：现场报的是另一个端口起不来）| ✅ **上游 `main` 已修**（`1d9371191`，2026-03-25：`ListenFds` 换 `parking_lot::Mutex` + 按地址的异步锁）。⏳ 但上游最新 release 仍是 **0.8.1**（＝本仓 vendor 的那个）⇒ **下一次 rebase 时随之解除**。⚠ 上游修法新增 `flurry` 依赖 ⇒ 「现在就 backport」要先过供应链门，不是顺手的事 |
| **上游 `listen_addresses()` 落地后的接线** | 上游 main 上有一条尚未发版的改动：给 `Service` 加 `listen_addresses()`，用来在换代后关掉没人认领的 fd | ⏳ rebase 上去之后，**枢衡的每一个自建 `Service` 都要显式实现它**。⚠ 它带默认实现（返回 `None` = 关掉清理）⇒ **漏实现不会有任何编译错误**，而一个服务没实现就会关掉整个进程的清理 |

# ✅ 已解除的

| 接缝 | 结论 |
|---|---|
| 自建 Service 挂载 · socket 移交（认领路径） | ✅ M0 实测：三类服务并挂，自建 TCP/UDP 监听器的 fd 经 `SCM_RIGHTS` 传到第二代 → [M0 接缝验证](/verification/m0-seam.md) |
| 未被认领的继承 fd 会怎样 | ✅ 已复现并有常设判据 → [`tests/m0/unclaimed.sh`](../../tests/m0/unclaimed.sh)，见下 |
| 移交来的监听 fd 没有 `FD_CLOEXEC` | ✅ 由 **G38** 在 fork 里修掉（包 `OwnedFd` + `MSG_CMSG_CLOEXEC`），并已投上游（[pingora#959](https://github.com/cloudflare/pingora/issues/959) → [#960](https://github.com/cloudflare/pingora/pull/960)）。★ 上游走批量重放，**「PR 被 close」是成功不是拒绝** —— 看改动有没有进 `main` |
| 两个入口能否共用同一份挑证书实现 | ✅ **不是被验证通过，是被取消了前提**：G104 换到 BoringSSL 之后共用的不再是 `ResolvesServerCert`，而是同一个 `select_certificate_callback` → [TLS](/architecture/tls.md) |
| BoringSSL 与 musl 静态链接 | ✅ 已通过 → [musl + BoringSSL 静态链接](/verification/musl-boringssl.md)。⚠ 卡点不在 musl 也不在 BoringSSL，在构建宿主 —— 换来了上面 D21 那条 |
| systemd 下的零停机升级 | ✅ 由 M1 通过，★ 但推翻了 G31 的一半 → [M1 spike #1](/verification/m1-systemd.md) |
| 升级窗口内 QUIC 连接归属 | ✅ 由 **G109** 解除：按 DCID 跨进程转交，判据 [`tests/quic-relay/run.sh`](../../tests/quic-relay/run.sh)，见下 |
| L4 UDP 在升级窗口内的数据报分流 | ✅ 收到停机信号就不再 `recv_from`；⚠ **窗口被缩小不是被消灭** —— 在那之前两代都在收 |
| `pingora-cache` 的抽象边界 | ✅ 已核实（`MemCache` 是唯一的 `Storage` 实现），⚠ 而同日 **G82** 决定缓存层完全自研 ⇒ 这条不再通向任何东西 |

# 三条要带走的结论

## 未被认领的 fd 会保持 LISTEN 并把连接吞掉，而它只在老一代退出之后才显形

第二代不挂某个监听器时，那个 fd 仍被继承、仍在 LISTEN：TCP 三次握手**照常成功**，请求
发出去之后**超时无回应**，并且逐代传递。

★ ★ ★ **连接层看起来完全健康** —— 任何只探「端口通不通」的健康检查都会说它是好的。

★ 而 pingora 把 fd 发给新一代之后会硬等 `CLOSE_TIMEOUT` 才广播停机，这段时间里两代都持有
同一个监听 socket、老一代照常 accept ⇒ **升级后立刻做的健康检查会是绿的**，问题要等老进程
走干净才暴露，而那时告警已经和「升级」这个动作脱钩了。
⇒ 判据因此写成「先等第一代退出，再探」，而不是「升级后 sleep 几秒再探」。

## 「把数据报丢了」与「替我们把对端连接杀了」可能在同一个函数签名上长得一模一样

quinn 的 `Endpoint::handle()` 对认不出的 DCID **回 stateless reset** —— 语义是告诉对端这条
连接已经不存在。换代窗口里，老一代的在飞连接只要有一个数据报落到新一代手里，客户端就会
**真的把它拆掉**，不是丢包重传能扛过去的那种。而这个函数返回的是 `Option<DatagramEvent>`：
只读类型签名推出来的方向是错的。

⇒ 这条事实是 G103 取 `quiche` 的依据之一，完整对照见
[HTTP/3 库选型事实表](/platform/http3-libraries.md)。

## 一条只写在验证记录里的前置，不会在开工时被读到

读待办的人看不见它，而读这一页的人已经在做别的事了。
⇒ 本页往后新增的每一条「最晚要在 X 之前验」，都要同时在 `PLAN.md` §11 挂一个 D 号。

同族的还有另外两条：**「被验证通过」与「被取消了前提」在结论里长得一模一样，而它们对判据的
要求相反** —— 后者意味着没有任何判据需要为它存在，再写一条门那条门会永远绿；以及
**一个 spike 证明的是「这条路走得通」，不是「产品走在这条路上」**，而这个差别在门里看不见，
因为门测的就是 spike。

# 一条方法论

**先读源码得到推断，再用真流量把推断坐实 —— 推断不是证据。**
M0 从类型签名推出「UDP 在结构上没有障碍」，仍然写了 spike；而 spike 除了确认推断，
还额外捞出了升级窗口内数据报分流这条风险，那是纯读源码看不出来的。

# 相关

[M0 接缝验证](/verification/m0-seam.md) · [待定清单](/governance/open-questions.md) ·
[数据路径](/architecture/data-path.md) · [TLS](/architecture/tls.md)
