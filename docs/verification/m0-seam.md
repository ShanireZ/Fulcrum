---
type: 验证记录
title: M0 接缝验证 · 结果
description: 通过。自建的裸 TCP 与裸 UDP 监听器都参与了 Pingora 的 socket 移交，优雅升级窗口内三类流量零中断。
resource: ../../tests/m0/run.sh
tags: [验证, 已通过, 必读]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-7
    resource: /references/plan.md
    title: PLAN.md §7 M0（范围与退出条件）
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 3 条硬约束（这条风险的原始表述）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G26（Docker 环境）、G27（范围收窄）、G28（QUIC 推后）
---

★ **本页记的是实际跑出来的东西，属历史事实。** 结论可以被后续实验推翻，但「那几次跑出了什么」不会变。

⚠ ★ ★ **这份结论的两个前提，必须和结论一起读**（2026-08-14 补记——原先只有结论、没有前提）：

1. **全部证据都来自 `daemon: true`**（[`conf/m0.yaml`](../../conf/m0.yaml)），即 pingora 自己 fork 出守护进程的那条路。
   而 **G31 定的产品路径是 systemd `Type=notify` 前台运行、`daemonize` 依赖删除**——
   也就是说，**产品将要走的那条路，本页一个字都没验过**。
   G31 已把「systemd 下的 MainPID 交接 + 零停机升级」列为 **M1 的第一个 spike**，判据照 M0 的形式写死；
   ★ 但在那个 spike 跑出来之前，**不要把本页的绿当成「前台模式也没问题」**。
2. **绑定地址**当时是 `0.0.0.0`。2026-08-14 收紧为默认 `127.0.0.1`（可用 `M0_BIND_HOST` 覆盖），
   理由见[安全基线](/platform/security-baseline.md)一节的回声反射面。下面日志摘录里的 `0.0.0.0:...`
   是**当时的原样记录**，没有改写——★ 历史证据不该为了和今天一致而被修饰。

> **结论：通过。** 自建的裸 TCP 与裸 UDP 监听器都参与了 Pingora 的 socket 移交，
> 一次 `SIGQUIT` + `-u` 优雅升级窗口内，三类流量零中断。
>
> 日期：2026-08-12 ｜ 环境：Docker `fulcrum-build:local`（`rust:1-trixie` + cmake + clang，Rust 1.97.1）
> ｜ pingora-core 0.8.1（★ 现为 [`vendor/pingora`](../../vendor/pingora/FORK.md) 的 fork）
> ｜ 重跑方式：`bash tests/m0/docker-run.sh`
>
> ★ **六次独立运行全绿**，其中三次是在 G30 的依赖 fork 之后——见 §5。

# 1. 要回答的问题

`PLAN.md` §5.1 第 3 条把这件事列为整个方案**唯一需要真花力气验证**的接缝：

> 自建 QUIC(UDP) 与 L4 监听器必须接入 Pingora 的 socket 移交，否则优雅升级时这两条会断连。

**不通过就不进 M1。**

★ QUIC 不在 M0 内（G27/G28）。风险本体是「非 Pingora 托管的监听器能否参与 fd 移交」，**这与监听器上跑什么协议无关**——能移交裸 UDP 的 fd，就能移交 QUIC 的。把 QUIC 排除在外，失败时才能立刻分清是**接缝**的问题还是 **QUIC 库**的问题。

# 2. 先读源码得到的推断

`pingora-core 0.8.1` 的 `Service` trait 文档注释直接写明了自建服务的**义务与权利**：

> `fds` (Unix only): a collection of listening file descriptors. During zero downtime restart
> the `fds` would contain the listening sockets passed from the old service, services should
> take the sockets they need to use then. If the sockets the service looks for don't appear in
> the collection, the service should create its own listening sockets and then put them into
> the collection in order for them to be passed to the next server.

★ 也就是说，**自建监听器参与 fd 移交是 `Service` 契约里第一等的设计，不是 hack**。

再看容器本身（`src/server/transfer_fd/mod.rs`）：

```rust
pub struct Fds { map: HashMap<String, RawFd> }
pub fn add(&mut self, bind: String, fd: RawFd)
pub fn get(&self, bind: &str) -> Option<&RawFd>
```

`HashMap<String, RawFd>` —— ★ **对协议零假设**。键是任意字符串，值是个 `i32`；传输走 `SCM_RIGHTS`，本来就能传任意 fd。所以 UDP 在结构上没有任何障碍。

推断到此为止。★ ★ **推断不是证据**，所以有了这个 spike。

# 3. 实现

三个服务挂在同一个 `Server` 上（`spikes/m0-seam/src/main.rs`）：

| 服务 | 监听 | 托管方 | 证明什么 |
|---|---|---|---|
| `m0-http` | TCP 8080 | Pingora `listening::Service` | 原生服务照常工作，且与自建服务共用一张 fd 表不撞键 |
| `m0-raw-tcp` | TCP 8081 | **自建 `Service`** | 自建 TCP 监听器能取到/放回 fd |
| `m0-raw-udp` | UDP 8082 | **自建 `Service`** | ★ 自建 **UDP** 监听器能取到/放回 fd |

自建服务的 fd 表键刻意加了前缀（`m0-raw-tcp:0.0.0.0:8081`），与 Pingora 原生的裸 `addr:port` 键错开——顺便证明**键空间是自由的**。

# 4. 直接证据

第一代注册、收到 `SIGQUIT` 送出、第二代继承：

```text
[raw-udp] bound fresh on 0.0.0.0:8082, registered fd=10 as key=m0-raw-udp:0.0.0.0:8082
[raw-tcp] bound fresh on 0.0.0.0:8081, registered fd=23 as key=m0-raw-tcp:0.0.0.0:8081
pingora_core::server] SIGQUIT received, sending socks and gracefully exiting
[raw-udp] INHERITED fd=6 for key=m0-raw-udp:0.0.0.0:8082
[raw-tcp] INHERITED fd=5 for key=m0-raw-tcp:0.0.0.0:8081
```

★ 两代之间 **fd 编号不同**（23→5、10→6）正是对的：`SCM_RIGHTS` 是把 fd **复制**进新进程的 fd 表，**编号由接收方内核分配，与发送方无关**。编号若相同反而说明没真的传。

★ **不同次运行的 fd 编号也不同**（首轮是 23→6、10→7），同样是这个原因。**不要把某一次的具体编号写进断言。**

# 5. 间接证据

探针在升级前后持续打三类流量（`spikes/m0-seam/src/bin/probe.rs`），**六次独立运行**：

| | 环境 | HTTP 成功/失败 | TCP 成功/断开 | UDP 成功/丢失 |
|---|---|---|---|---|
| 1 | 原始（bookworm，pingora 0.8.1）| 281 / **0** | 283 / **0** | 283 / **0** |
| 2 | 同上 | 282 / **0** | 283 / **0** | 283 / **0** |
| 3 | 同上 —— ★ **改造前基线** | 282 / **0** | 283 / **0** | 283 / **0** |
| 4 | **trixie**（Debian 13 + Rust 1.97.1）| 281 / **0** | 283 / **0** | 283 / **0** |
| 5 | ★ ★ **trixie + fork 后的 pingora** | 281 / **0** | 282 / **0** | 282 / **0** |
| 6 | 同上 | 282 / **0** | 284 / **0** | 283 / **0** |
| 7 | 同上 | 282 / **0** | 284 / **0** | 284 / **0** |

判据：HTTP 每轮新建连接、打的是 accept 路径；TCP ★ **同一条连接贯穿升级**；UDP 跑在回环上无拥塞，**丢失即代表 socket 失守**。

进程确实换了：`gen1 pid=23 → gen2 pid=43`（第 1 次）、`gen1 pid=4006 → gen2 pid=4026`（第 3 次）。

★ ★ **第 5–7 次尤其重要**：G30 的 fork 把 `nix` 从 0.24.3 抬到 0.31.3，而那一轮破坏**恰好落在 `server/transfer_fd/mod.rs`——本页要验的那个模块**。`socket()` 改为返回 `OwnedFd`、`listen()` 改签名、`cmsgs()` 变为可失败，三处都在 fd 移交路径上。**连跑三次全绿，且两代之间 fd 编号照旧不同**，说明移交仍然是真的。改动清单见 [`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md)。

# 6. 顺带得到的两条结论

## 6.1 给 D11（HTTP/3 库选型）的一条约束 ★

升级窗口内**两代进程持有的是同一个 UDP socket**，双方都在 `recv_from`，于是数据报会在两代之间被**分流**。

对回声服务这无所谓——谁答都一样，所以零丢失。★ ★ **但对 QUIC 是真问题**：连接状态只存在于某一代进程里，被分到另一代的数据报无法被正确处理。

所以 M2 做 HTTP/3 时，**fd 移交本身已经不是问题，连接归属才是**。完整推导与可能方向见 [尚未验证的接缝](/verification/open-seams.md)。

## 6.2 给 D2（线程模型）的实测依据

M0 跑在 `threads: 2` / `listener_tasks_per_fd: 1` / `work_stealing: true` 下全绿。这说明 Pingora 的默认多线程 work-stealing 模型**对自建服务没有额外约束**——自建 `Service` 拿到的是 `current_handle()` 所在的那个 runtime，行为与原生服务一致。

★ **D2 此后已经不再待定** —— 线程模型的结论是 `PLAN.md` §10 的 **G35**（保持
`work_stealing = true`；线程数按 service 角色定默认、配置可覆盖）。本节留下的是当时给它的
实测依据：★ **至少不存在「自建服务必须单线程」这类隐藏限制**。

# 7. 读日志时不要误判的一行

升级过程中第二代会先打一条 **ERROR**，然后才成功：

```text
ERROR pingora_core::server::transfer_fd] No incoming socket transfer, sleep 1s and try again
INFO  pingora_core::server::bootstrap_services] Bootstrap done
```

★ **这是正常的重试，不是失败。** 第二代起得比第一代送 fd 早一点时就会出现。判据是退出码与后面那两行 `INHERITED`，**不是有没有 ERROR 字样**。

# 8. 怎么重跑

```bash
bash tests/m0/docker-run.sh
```

宿主机只要有 Docker。构建镜像不存在会自动做，`cargo` 与 `target` 缓存放在命名卷里，不污染宿主机也不进 Windows 文件系统。运行期产物（pid / sock / 日志 / 探针输出）落在 `run/m0/`（已 gitignore）。★ **退出码即结论。**

详见 [构建与验证](/platform/build-and-test.md)。
