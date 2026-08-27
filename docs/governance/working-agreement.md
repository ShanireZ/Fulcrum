---
type: 项目治理
title: 工作方式与不得绕过的约束
description: 在枢衡上工作时必须遵守的纪律——决策不得静默重开、性能不得无据宣称、协议栈不得手写。
resource: ../../AGENTS.md
tags: [治理, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: agents
    resource: /references/agents.md
    title: AGENTS.md「Working rules」
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G6（三条硬约束）、G19（性能验收）、G29（依赖策略）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 主要风险（单人 + AI 承担网络关键路径的安全责任）
---

★ 权威表述在 [`AGENTS.md`](../../AGENTS.md) 与 [`PLAN.md`](../../PLAN.md) §10。本页把散落各处的纪律收在一起。

# 决策

**§10 的每一条都由 owner 逐项拍板，不得静默重开。**

若某条看起来不对：**说出来并问**。不要绕过它实现，也不要「先按自己的想法做、回头再解释」。

★ **owner 推翻了 AI 推荐项的那些决定**尤其如此——它们**看起来**像是 AI 建议更优，但每一条 owner 都给了理由，而且 G29 是在 AI 明确提示代价之后**同日重申**的。逐条见 [决策日志](/governance/decision-log.md)。

> ★ **不要在这里写「一共有几条」。** 那个数字曾经被抄进五处，而新增决定的那一天五处**同时**过期。
> **副本的数量就是它过期时要修的处数。**

# 三条锁死的技术约束

来自 G6，完整表述见 `PLAN.md` §5.1：

1. **TLS 后端是 rustls。** 动态证书选择走 `ResolvesServerCert`，**不是** `certificate_callback`——rustls 后端不支持它。自动 HTTPS 与 On-Demand TLS 都建在这条路上，**不留双路径**。
2. **tower 中间件不与 Pingora 的 `ProxyHttp` 阶段模型复合。** 同名不同物，插不进去。
3. **自建 QUIC 与 L4 监听器必须接入 Pingora 的 socket 移交。** ✅ 已由 M0 证明可行——★ 但那是「可以做到」，不是「自动发生」：自建 `Service` **有义务**自己取走 fd 并放回去。

# 性能

★ **绝不在没有可复现的端到端实测时宣称性能。**

- 三家（Caddy / HAProxy / Nginx）**全部对拍**，不挑软柿子
- 逐类设门：不劣于**该类最强者** 10%
- 脚本与**原始数据**全部公开
- ★ **不使用「快 N 倍」式表述**

详见 [性能验收标准](/verification/performance-bar.md)。

# 安全

★ **绝不手写 TLS、HTTP/2 状态机、HPACK 或 QUIC。** 全部交给经过审计的成熟库。

这条不是风格偏好。`PLAN.md` §9 把「单人 + AI 承担网络关键路径软件的安全责任」列为主要风险，而缓解手段只有三条：内存安全交给 Rust、协议栈交给成熟库、**不自己实现上面那四样**。

# 回落层

**它是脚手架，不是架构。成功标志是被删除。**

- 只做**薄转发**：不做能力翻译、不做配置语义映射、不试图统一两家的行为差异
- **不静态链接** Caddy 或 Nginx，仅通过进程边界与 API 交互（也是许可证考虑）
- 一块自研完成，**立即拆除对应回落**

详见 [回落层](/architecture/fallback.md)。

# 状态模型

**期望状态是唯一权威。** Runtime 临时覆盖不持久化，但**必须永远可见**于 stats 与 API 输出。

★ **不要新增第二条持久化写路径。** G18 的判据是：HAProxy 的病根不是 runtime 改动会丢，而是**丢得无声无息**——问题的本质是「可见与否」，不是「持久化与否」。

# 依赖

追新（G29），**但不得裸跑 `cargo update`** ——那会跳过 24 小时安全怀疑期，而怀疑期正是 G29 存在的理由。

```bash
python tools/dep-check.py            # 只报告
python tools/dep-check.py --apply    # 采纳通过怀疑期的
```

★ **升级之后必须跑完当前里程碑的全部验证**（M0 是 `tests/m0/run.sh`；M3 之后是全量对拍）。

详见 [依赖策略](/platform/dependency-policy.md) 与 [供应链现状](/platform/supply-chain.md)。

# 构建与验证

**一切在 Docker 里跑（G26）。** 宿主机除 Docker 外什么都不需要——而且 `SIGQUIT` + fd 移交是 Linux 独有的，**在 Windows 上根本没法验**。

```bash
bash tests/m0/docker-run.sh              # 构建 + 跑 M0
BUILD_ONLY=1 bash tests/m0/docker-run.sh # 只构建
```

交接前要做的三件事：**校验 Markdown 链接、跑 `git diff --check`、跑 `bash tests/m0/docker-run.sh`**。

# ★ 一条本工作区反复出现的病

**状态类句子最容易漂。** 详见 [唯一权威与本 bundle 的定位](/governance/source-of-truth.md) 末节——本项目已经犯过一次（M0 通过后 README 徽章没跟）。

**每次状态变化后，这几处当成一张清单整体扫一遍**：README → `PLAN.md` §1 → `PLAN.md` §7。
