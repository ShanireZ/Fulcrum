---
type: 验证记录
title: M1 spike #1 · systemd 下的零停机升级
description: 通过，但推翻了 G31 的一半——撑住 unit 的是 ExitType=cgroup，而「抢 MainPID」会毁掉优雅停机。
resource: ../../tests/m1/run.sh
tags: [验证, 已通过, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-14T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G31（进程模型 = Type=notify 前台运行；附带强制做本 spike）
  - id: plan-10-g33
    resource: /references/plan.md
    title: PLAN.md §10 G33（路径约定）、G34（回落层 systemd 依赖）
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 D14（G31 需要 owner 修订）
---

★ **本页记的是实际跑出来的东西，属历史事实。** 结论可以被后续实验推翻，但「那几次跑出了什么」不会变。

> **结论：通过，但形状与 G31 拍板时假定的不一样。**
>
> systemd `Type=notify` + **前台运行** + **`ExitType=cgroup`** 之下，
> 一次 `systemctl reload` 能完成零停机换代：unit 全程不离开 `active`，三类流量零中断，
> 停机仍然走完排空。
>
> ✅ **口径已由 G37 拍板**（2026-08-14），两条 fd 缺陷已由 **G38** 在 fork 里修掉。
>
> ★ ★ ★ 而 G31 写的「新进程必须落在同一个 cgroup 内**并抢过 MainPID**」——
> 前半句对，**后半句实测是错的，而且有害**。详见下面第 2 节。
>
> 日期：2026-08-14 ｜ 环境：`fulcrum-systemd:local`（Debian 13 trixie / **systemd 257.13-1~deb13u1**，
> 钉到 digest）＋ 产物由 `fulcrum-build:local`（Rust 1.97.1）构建
> ｜ 复跑：`bash tests/m1/systemd-run.sh`

# 0. 它要回答什么

G31 把进程模型定成 systemd `Type=notify` + 前台运行时，同时记下了一条**纯靠读文档与代码推出来、
没有跑过**的冲突：

> pingora 的零停机升级是外部拉起新进程（`-u`），而 `Type=notify` 下老进程一退出，unit 即被
> 判定结束、默认 `KillMode=control-group` 会杀掉整个 cgroup。

owner 拍板时因此附了一条强制条件：**这件事必须先做 spike，判据照 M0 的形式写死。**
理由是「★ 推断不是证据」——M0 那次正是靠 spike 额外捞出了 UDP 分流那条风险。

**这一次它又捞出来了两条**，而且其中一条恰恰是那条推断本身。

# 1. 三个场景，三种口径

| 场景 | 口径 | 它证明什么 |
|---|---|---|
| [`run.sh`](../../tests/m1/run.sh) | `ExitType=cgroup`，**不交接 MainPID** | 产品要保证的行为 |
| [`exit-type-main.sh`](../../tests/m1/exit-type-main.sh) | 去掉 `ExitType=cgroup` | 那一行确实在干活；`run.sh` 的断言分得清好坏 |
| [`mainpid-handover.sh`](../../tests/m1/mainpid-handover.sh) | G31 推断的形状（`ExitType=main` + 交接） | 它**为什么被否掉** |

★ 后两个脚本复现的是**坏行为**，所以它们的绿 = 坏行为照常发生 —— 与
[`tests/m0/unclaimed.sh`](../../tests/m0/unclaimed.sh) 同类。它们红了的含义是「行为变了」，
不是「测试坏了」。

# 2. ★ ★ ★ 最值钱的一条：交接 MainPID 会**悄悄弄丢优雅停机**

同一份二进制、同一份配置，只改 unit 文件里的两行，实测三种组合：

| 口径 | 老代退出后 unit | `systemctl stop` |
|---|---|---|
| 交接 MainPID（**G31 推断的形状**）| `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |
| **`ExitType=cgroup`，不交接** | `active` ✅ | **10 秒走完排空，`success`** ✅ |
| 两者都用 | `active` ✅ | ★ **0 秒，`failed (signal)`** ❌ |

**机制**：交接过去的 pid 不是 systemd 亲生的（它是老进程 fork 出来的），systemd 会打一句

```
fulcrum-m1.service: Supervising process 174 which is not our child.
We'll most likely not notice when it exits.
```

并把它标成 alien。此后停机**不再等它排空**——SIGTERM 与 SIGKILL 几乎同时发出。

★ **为什么这条特别危险**：整个失效**在升级当时没有任何症状**。换代成功了、流量零中断、
`systemctl status` 一切正常。代价要到**下一次停机**才兑现，而那时它已经和「升级」这个动作
完全脱钩了——重启、`systemctl restart`、机器关机，全部变成硬杀连接。
（这与 [open-seams.md](/verification/open-seams.md) 里「黑洞不是立刻出现的」是同一类陷阱：
**判据取早了就什么都看不见**。）

★ 顺带更正 systemd 那句警告：它说「多半察觉不到它退出」，**实测不对**——新一代在老代退出后会被
reparent 给 PID 1（也就是 systemd 自己），SIGKILL 掉它之后 systemd **1 秒内**就报了
`Main process exited, code=killed`。真正的后果不是「察觉不到」，而是**停机不再等它**。

## 换成 `ExitType=cgroup`

`ExitType=`（systemd ≥ 250）换掉了「unit 什么时候算结束」的判据：

- `main`（默认）：主进程退出 = unit 结束
- **`cgroup`：cgroup 里还有进程就算活着**

于是**根本不需要交接**：老代退出时 cgroup 里还有新代，unit 照常 `active`；
停机时 systemd 照常等整组排空。

⚠ **已知代价**：第一次升级之后 `MainPID` 变成 **0**。直接后果是
`ExecReload=/bin/kill -USR2 $MAINPID` **第一次能成、第二次失败**（`$MAINPID` 展开成空）。
★ **失败方式是实测的，不是推的**：`systemctl reload` 退出码 **1**，journal 里留下 `kill` 的 Usage 与
`Reload failed`，而 **unit 仍是 `active/success`**——也就是说**升级没发生，服务照常跑着老一代**。
⚠ 本页初稿把它写成「静默失败」，实测推翻了那个词：它吵得很。**升级没发生**才是真正的后果。
★ 所以本 spike 让每一代自己写 pid 文件，`ExecReload` 从文件里找当前这一代——
**而「只升一次」的测试看不见这个洞**，`run.sh` 因此连升两次。

# 3. 主场景实测（`run.sh`，九步）

```
[1/9] start          unit active/running，MainPID=gen1，三端口在听，pid 文件 = gen1
[4/9] reload → gen2  pid 文件换代，全程 active/running，两代重叠窗口成立
[5/9] gen1 退出      unit 仍 active，gen2 活着，MainPID 归零
[6/9] 探针           http 376/0   tcp 377/0   udp 377/0   （成功/失败）
[7/9] reload → gen3  连升两次都成立
[8/9] stop           10 秒，Result=success，cgroup 已空
```

**三类流量零中断**：HTTP 请求 0 失败、跨升级的 TCP 长连接 0 断开、UDP 回声 0 丢失——
判据与探针与 M0 是同一份（[`m0-probe`](../../spikes/m0-seam/src/bin/probe.rs)），
所以两组数字可以直接对读。

★ **停机耗时的下界也是判据**：`stop` 若秒回，说明它压根没等排空，那「`TimeoutStopSec` 要按
排空时长配」这条结论就是假的——而秒回恰恰就是交接方案的失败形状。
实测 10 秒 = `grace_period_seconds`(5) + `graceful_shutdown_timeout_seconds`(5)，与公式吻合。

> ⚠ 产品默认配置（M0 用的 30 + 30）对应的量级是 **35–65 秒**，`TimeoutStopSec` 必须按它配。
> 依据见 [`tests/m0/lifecycle.sh`](../../tests/m0/lifecycle.sh) 里实测出的那张表：
> **一旦某一代开始排空，它就收不到任何可捕获的信号了**，`systemctl stop` 之后再补一刀是没用的。

# 4. ★ ★ 另外捞到的两条 fd 缺陷（M0 结构上不可能覆盖）—— ✅ 已修

M0 的第二代是**从 shell 起的**，与第一代没有 fork 关系。M1 改成「老进程自己 fork 下一代」
（这是「新进程落在同一个 cgroup 内」最省事的做法）之后，子进程会继承父进程
**所有没设 `CLOEXEC` 的 fd**——而经 `SCM_RIGHTS` 收来的 fd 恰恰没有：

- `recvmsg` 调用时没带 `MSG_CMSG_CLOEXEC`（`vendor/pingora/pingora-core/src/server/transfer_fd/mod.rs`）
- `pingora-core` 全仓搜不到任何一处 CLOEXEC 设置
- spike 自己的 `TcpListener::from_raw_fd` 同样不会补上

实测（每一代都在「前一代已退出」的安静时刻量的，数的是 `ss -p` 列出的 `fd=` 个数）：

| | 8080 | 8081 | 8082 | 为什么 |
|---|---|---|---|---|
| gen1 | 1 | 1 | 1 | 自己 `bind`，std 建的 fd 带 CLOEXEC |
| gen2 | 1 | 1 | 1 | fork 自 gen1，但 gen1 的 fd 有 CLOEXEC，继承不过来 |
| **gen3** | **2** | **2** | **2** | ★ 继承 gen2 的一份（无 CLOEXEC）+ `SCM_RIGHTS` 再收一份 |

**原因也已实测**，不是只读源码推的：`/proc/<gen3>/fdinfo/<fd>` 的 `flags`（八进制）里
`O_CLOEXEC`(02000000) 位是 0，两个 fd 都是。

**后果**（按机制推，量级未实测）：

1. **逐代累加**——gen4 会有 3 份、gen5 有 4 份。升级次数多的长命进程会慢慢吃 fd。
2. ★ 更要紧的一条：**上游那个还没发版的清理机制会被绕过。**
   `f82478ae` 的 `listen_addresses()` 关的是 **fd 表里**那些没人认领的 fd，
   而**继承进来的那一份根本不在 fd 表里**。也就是说
   [open-seams.md](/verification/open-seams.md) 里那个「未被认领的 fd 黑洞化」问题，
   在 fork 式升级下**即使上游修好了也可能仍然存在**。

## 4.1 ★ 还有一条：accept 出来的传输 socket 从来不 close

读上游代码准备投稿时发现的**第二处**，在同一个函数里：`get_fds_from()` 结尾只
`close(listen_fd)`，而 `accept_with_retry_timeout()` 返回的那个连接是**裸 `RawFd`、没有
`Drop`**，此后既不 close 也不转移——**每完成一次优雅升级就永久泄漏一个已连接的 unix socket**。

实测（按 `/proc/<pid>/fd` → `/proc/net/unix` 反查，数的是路径为 upgrade.sock 的那些）：

| | 攥着的 upgrade.sock fd |
|---|---|
| gen1（从未经历过移交）| 0 |
| gen2（一次移交）| **1**（`St=03` CONNECTED，即 accept 出来的那一端）|
| gen3（移交 + 继承）| **2**（自己漏一个 + 从 gen2 继承一个）|

★ ★ **两个缺陷会叠乘**：泄漏的那个 socket 自己也没有 CLOEXEC，于是逐代累加。
★ `St=03`（CONNECTED）是关键判据——它证明泄漏的是**accept 出来的连接**而不是监听 socket
（后者确实被正确关掉了）。

## 4.2 ✅ 修法（G38）与它守在哪

两处都在 fork 里修掉了（`vendor/pingora` 的 [`FORK.md`](../../vendor/pingora/FORK.md) §4）：

| | 修法 |
|---|---|
| ① accept 的连接不 close | 包成 `OwnedFd` 交给 `Drop`，顺带覆盖 `cmsgs()?` 的提前返回路径 |
| ② 收来的 fd 无 CLOEXEC | `MsgFlags::empty()` → **`MsgFlags::MSG_CMSG_CLOEXEC`**（原子，避免收完再 `fcntl` 与 fork 之间的竞态窗口）|

修完复测：三代的监听 fd 重数**全部是 1**，攥着的 upgrade.sock **全部是 0**。

**守门人**是 `run.sh` [7/9] 的三条断言（fd 重数、CLOEXEC 位、upgrade.sock 计数）。
★ 它们同时是 **rebase 的守门人**：上游若未接受这两条，而 rebase 时漏了重做，它们会红。

★ **上游 0.8.1 与当前 main 都有这两处**（2026-08-14 核对过 main 的同一函数），
且上游仓库**没有任何 issue/PR 提过 CLOEXEC**。投稿材料见
[`upstream-pr/`](../../upstream-pr/README.md)，按 G32 的流程由 owner 本人提交。

# 5. 顺带坐实与更正的几条

| | 结论 | 怎么来的 |
|---|---|---|
| G33「`PIDFile=` 不需要」 | ✅ 坐实。前台模式下 pingora **不写 pid 文件**（`pid_file` 只在 `daemonize()` 里被读），`/tmp/pingora.pid` 不存在即证明前台模式生效 | `run.sh` [1/9] 的双向断言 |
| ⚠ `error_log` / `pid_file` 会被**安静忽略** | 前台模式下这两个配置项 pingora 一个都不读。配置里写了路径、文件永远不出现，且没有任何提示 | 读 `server/daemon.rs` + 实测 |
| G31「前台运行保住了 `run()` 之前的线程」 | ✅ 坐实。升级触发器就住在那样一个线程里，它活着 | `systemctl reload` 能工作即是证据 |
| `NotifyAccess` 可以收紧到 `main` | ✅ 不交接 MainPID 之后，发通知的永远是主进程自己，不再需要 `all` | `run.sh` 用的就是 `main` |
| systemd 那句 "not our child" 警告 | ⚠ 措辞不准，见第 2 节 | 实测 1 秒内即察觉 |

# 6. ★ 一条把结论差点带反的环境坑

第一次搭测试宿主时，容器是按「网上通行做法」起的：`--privileged` **加上**
`-v /sys/fs/cgroup:/sys/fs/cgroup:rw`。结果：

1. `journald` 起不来（`Failed to acquire cgroup root path: Protocol driver not attached`），于是**没有任何日志**；
2. ★ ★ **`MAINPID=` 交接被静默丢弃**——systemd 无法把发信进程解析到某个 unit 上。

于是第一轮实验的现象是「交接没生效、unit 照常死掉」，**差一点就写成
「systemd 不接受来自非亲生进程的 MainPID 交接」这个完全错误的结论。**

根因是那个 bind mount：容器用的是私有 cgroup namespace，再把宿主机的 cgroup 树挂进来，
两边对不上。cgroup v2 下 `--privileged` 已经会把正确的视图挂成 rw，**不需要也不应该再挂一次**。

★ 教训与本仓库已有的那几条同形：**先让工具自己证明它没瞎，再信它给的结论**。
这里救场的是「journald 也挂了」这个旁证——**两个症状同源，才让人去查环境而不是去查 systemd**。

# 7. 判据在哪

| | 文件 |
|---|---|
| 主场景 | [`tests/m1/run.sh`](../../tests/m1/run.sh) |
| 反证（去掉 `ExitType=cgroup`）| [`tests/m1/exit-type-main.sh`](../../tests/m1/exit-type-main.sh) |
| 反证（G31 推断的形状）| [`tests/m1/mainpid-handover.sh`](../../tests/m1/mainpid-handover.sh) |
| 共用判据原语 | [`tests/m1/lib.sh`](../../tests/m1/lib.sh) |
| 宿主机驱动 | [`tests/m1/systemd-run.sh`](../../tests/m1/systemd-run.sh) |
| unit 文件 | [`tests/m1/fulcrum-m1.service`](../../tests/m1/fulcrum-m1.service) |
| 配置 | [`conf/m1.yaml`](../../conf/m1.yaml) |
| spike 本体 | [`spikes/m1-systemd/src/main.rs`](../../spikes/m1-systemd/src/main.rs) |
| 测试宿主镜像 | [`docker/Dockerfile.systemd`](../../docker/Dockerfile.systemd) |

★ 流量服务与探针**复用 M0 的那一份**（`spikes/m0-seam` 现在多了个 `lib.rs`），
不是复制。理由：三类流量是「零停机」的判据本身，**判据有两份就等于没有判据**。

# 相关

[M0 接缝验证](/verification/m0-seam.md) · [尚未验证的接缝](/verification/open-seams.md) ·
[进程与组件边界](/architecture/process-model.md) · [待定清单](/governance/open-questions.md) ·
[构建与验证](/platform/build-and-test.md)
