---
type: 技术基线
title: 依赖策略（G29）
description: 追新、不设上界、Cargo.lock 入库、每周检查，且任何版本必须已发布满 24 小时才允许采纳。
resource: ../../tools/dep-check.py
tags: [依赖, 安全, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G29（D1 结案，五条口径与已认下的代价）
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §10 G29**（它同时是 D1 的结案）。

# 五条口径

1. `Cargo.toml` 用 **`>=` 下界、不设上界**
2. **`Cargo.lock` 入库**，保证任何一次构建都可复现
3. **每周**跑一次依赖检查，跟进最新版本
4. ★ **任何版本必须已发布满 24 小时才允许采纳**
5. 每次升级必须**通过当前里程碑的全部验证**（M3 之后是全量对拍）

# ★ 代价已被明确告知并重申接受

Cargo 的 `>=` **真的没有上界**，`cargo update` 会跳到已发布的最大版本，**包括破坏性的大版本**。所以每周那次检查有时会是一次真的迁移工作，不是点一下就完。

AI 提示这一点后，owner 于同日回复「**确定使用最新+包括破坏性的版本更新**」——★ **这是知情后的重申，不是疏漏。** 不要拿「破坏性版本有风险」当理由回头去钉死版本。

# 24 小时怀疑期防的是什么

它挡的是**一类具体攻击**：**投毒版本发布后数小时内被发现并 yank**。盲目追新会正好撞进那个窗口；等满 24 小时再取，绝大多数这类事件已经暴露。

★ 这条**无法写进 `Cargo.toml`**，只能由脚本执行。

# 怎么跑

```bash
python tools/dep-check.py            # 只报告
python tools/dep-check.py --apply    # 采纳通过怀疑期的
python tools/dep-check.py --hours 48 # 换一个怀疑期长度
```

★ ★ **绝不要裸跑 `cargo update`。** 它会跳过怀疑期——而怀疑期正是 G29 存在的理由。

退出码即结论（★ **每项检查各占一位，可叠加**）：

| 码 | 含义 |
|---|---|
| `0` | 都没有要处理的 |
| `10` | 有可采纳的 cargo 更新（`--apply` 模式下表示已采纳）|
| `20` | ★ fork 的上游有新版，要人工 rebase |
| `40` | ★ 构建镜像要人管（G36）：rustc 有新版 / `@sha256` 钉子掉了 / **本次没能查证** |
| `80` | ★ systemd 测试宿主镜像要人管（G39）：大版本变了 / **本次没能查证** |
| `160` | ★ **有未登记的安全公告 / 本次没能查证**（新增，扫**两把锁**）|
| `1` | 出错 |

★ 它查的几件事里，**只有第一件在 `cargo update` 的视野内**。其余（被 patch 的 pingora-core、构建镜像的编译器、测试宿主的 systemd、以及**现有版本上的安全公告**）`cargo update` 结构上就看不见——这正是它们要被单独盯住的原因。

★ ★ **`160` 这一项扫的是两把锁，`vendor/pingora/Cargo.lock` 在前、根锁在后。** 在它之前，`supply-audit.py` 默认只审根锁，**而且没有任何东西自动调用它**——偏偏根锁是两者里**次要**的那把（`[patch.crates-io]` 把 `pingora-core` 指向 vendor，而根锁里连一个 rustls 相关的包都没有）。**最该被扫的那把锁，恰恰没人扫。** 代价  兑现过一次：`h2` 的 RUSTSEC-2026-0258 是手动跑一次 vendor 锁才撞见的。

★ `ACCEPTED` 豁免名单从 `supply-audit.py` **导入**而不是复制——两份名单分家的表现是「这边判红、那边判绿，而两边都自称权威」。并进本脚本而不另设节奏，理由同 G36：**没人记得住第二件事**。

# 脚本做了什么

1. 用 `cargo update --dry-run` 问出「如果现在更新，会动哪些包」——★ **只有这批需要查**，全量查几百个传递依赖是浪费
2. 对每个候选版本查 crates.io 的**发布时间**与 **yanked 状态**
3. 发布不足 N 小时、或已被 yank 的，**一律挡下并说明原因**
4. `--apply` 时只对通过的候选做 `cargo update -p <name> --precise <ver>`

# ★ 它看不见什么

这是使用这个脚本时最容易误判的一点：

> **`dep-check.py` 只报告 `cargo update` 能动的东西。**

而 `cargo update` **只在现有版本需求范围内移动**。任何被上游清单的上界卡住的依赖，它**一个都不会报**——脚本会输出「没有可用更新，依赖已是最新」，而实际上有 44 个包落后，其中两个带安全公告。

★ **「dep-check 说全绿」不等于「依赖是最新的」。** 全量审计是**另一个脚本**：

```bash
python tools/supply-audit.py --cargo "<cargo 命令>"
```

它逐包对比 crates.io 的 `max_stable_version`、对 `Cargo.lock` 全量查 OSV，并**按「参与编译 / 仅在 lock 里」分桶**（★ 不传 `--cargo` 就没有分桶，数字会偏大一倍）。退出码 `20` = 有未登记的安全公告；★ **陈旧包不判红**——那会让这道门永远红。详见 [供应链现状](/platform/supply-chain.md)。

★ **这不是假设，是实际发生过的** 全量审计前，`cargo update --dry-run` 一直报「无可用更新」，而当时有 44 个包落后、两条真漏洞在库里。

# ★ pingora 走 fork，上游由脚本单独盯着

`[patch.crates-io]` 把 `pingora-core` 指向 [`vendor/pingora/`](../../vendor/pingora/FORK.md)（G30）。

★ ★ **这制造了一个盲区**：被 patch 的 crate **彻底脱离了 crates.io 的更新流**，`cargo update` 再也不会为它查任何东西。上游发了 0.9 也不会有任何提示——**而 fork 的全部价值（抬上界、修掉两条公告）恰恰要靠跟进上游才不会随时间腐化**。

所以 `dep-check.py` **单独查一次上游最新版**，与 fork 的基线比对：

```text
── fork 上游检查 ──
fork 基线 pingora-core 0.8.1 仍是上游最新，无需 rebase。
```

上游领先时它会报出版本、发布时间、期间发过几个版本，并**明确拒绝自动采纳**：

| | |
|---|---|
| 基线从哪来 | ★ 直接读 `vendor/pingora/pingora-core/Cargo.toml` 的 `version`——**不另设一份要手工同步的记录**，那种记录迟早与事实分家 |
| 怀疑期 | 同样适用（新版不满 24 小时会标注） |
| 退出码 | **20** = fork 落后；与 cargo 那条叠加，两者都有则 **30** |
| 关掉 | `--skip-fork` |

## ★ 它还会看上游 main，不只看发版

只盯 release 是不够的：`nix` 0.24→0.31 那次迁移**先落在上游 main**，我们直接照抄省下了一整轮工作。所以脚本还会问 GitHub「main 领先基线 tag 多少提交」，并挑出标题像依赖改动的那些：

```text
  ★ 上游 main 领先 tag 0.8.1 **167 个提交**（尚未发版）
    其中 20 条看起来与依赖有关 …
```

★ **这条只报告、不影响退出码**——main 上的东西还没发版，是「值得提前捞」而不是「必须跟进」。

★ ★ **它第一次跑就有了收获**：捞出两条直接打在 fd 移交上的未发版 commit，其中一条会改变枢衡自建 `Service` 的写法。见 [尚未验证的接缝](/verification/open-seams.md)。

## ★ 第三件：构建镜像的编译器（G36 / D13）

同样在 `cargo update` 的视野之外，理由和 fork 一样——它压根不是一个 crate。

`docker/Dockerfile.build` 的基础镜像钉到了 digest（可复现），代价是不会自己跟进。脚本会查 Docker Hub 上 `rust:1-trixie` 现在指向哪个精确版本（**纯 HTTP，不需要 Docker，也不拉镜像**），与 Dockerfile 里钉的比对。

| | |
|---|---|
| 判据 | ★ **只看 rustc 版本**。Debian 侧的 digest 漂移只打一行提示——构建镜像不是运行时镜像，那些包不进产物（G13 是 musl 静态二进制）|
| 钉子从哪来 | ★ 直接读 `docker/Dockerfile.build` 的 `FROM` 行——**不另设一份要手工同步的记录** |
| 退出码 | **40**；rustc 有新版 / `@sha256` 钉子掉了 / **本次没能查证**，三种都算 |
| 自动采纳 | ★ **拒绝**，同 fork rebase——换编译器必须重跑三场景，不该无人值守 |
| 关掉 | `--skip-image` |

★ **「没能查证」判红不判绿**：Docker Hub 不可达时它返回「要人管」，而不是「无事」。只发一个请求，失败多半是真有问题。

⚠ ★ **M3 对拍期间冻结**，别在采集数据的中途换编译器。详见 [构建与验证](/platform/build-and-test.md)。

跟进步骤见 [`FORK.md`](../../vendor/pingora/FORK.md)：把上界改动在新基线上重做，修随之而来的调用点。

- ★ **先看上游有没有已经自己抬了某几条**——`nix` 那一次就是白捡的
- ★ **做完照例跑 M0**——fork 的改动里有一处正落在 `transfer_fd` 上

# 升级之后

★ **必须跑完当前里程碑的全部验证**（G29 第 5 条）：

```bash
bash tests/m0/docker-run.sh
```

M3 之后是全量对拍。见 [构建与验证](/platform/build-and-test.md) 与 [性能验收标准](/verification/performance-bar.md)。

# 相关

[供应链现状](/platform/supply-chain.md) · [技术栈](/platform/tech-stack.md) · [工作方式](/governance/working-agreement.md)
