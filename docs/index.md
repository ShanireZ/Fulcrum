---
okf_version: "0.2"
---

# 枢衡 Fulcrum · 知识 bundle

按 [Open Knowledge Format v0.2](/references/okf-spec.md) 组织的知识 bundle。

★ **[`PLAN.md`](../PLAN.md) 是唯一权威。** 本 bundle 的绝大部分是指向它的**导航图**——每篇概念用 `sources` 声明出处（章节号 + 决策编号），正文只做定位与串联，不复述结论。与 `PLAN.md` 冲突时一律以 `PLAN.md` 为准，并且要把这里改掉。

★ **一处例外必须说清**：[架构](/architecture/index.md) 一节**带真内容**，它是原 `docs/architecture.md` 拆分而来的**技术基线**，不是 `PLAN.md` 的复述。它与原文件一样**服从 `PLAN.md`**。详见 [唯一权威与本 bundle 的定位](/governance/source-of-truth.md)。

## [治理](/governance/index.md)

* [唯一权威与本 bundle 的定位](/governance/source-of-truth.md) - 谁说了算、架构一节为什么是例外、发现冲突怎么办
* [决策日志](/governance/decision-log.md) - §10 的导航：哪条管哪件事、★ owner 推翻 AI 建议的那些决定
* [待定清单](/governance/open-questions.md) - 仍然开着的那些：**D19/D20/D26（M2–M3）· D21/D23/D24/D9（M4）**
* [实施路线](/governance/roadmap.md) - M0–M4 五个里程碑与各自的退出条件；判据只有「能不能真用」
* [工作方式](/governance/working-agreement.md) - 不可回头的硬约束、不得绕过的三条、性能声明的纪律

## [产品](/product/index.md)

* [定位](/product/positioning.md) - 一个进程覆盖三件事；三家的长处与短处；★ 竞争位置必须诚实记录
* [首版范围](/product/scope.md) - 自研四块、明确不做七项、回落层两家
* [设计原则](/product/design-principles.md) - 七条；★ 第七条「回落是脚手架」的成功标志是被删除

## [架构（技术基线）](/architecture/index.md)

* [进程与组件边界](/architecture/process-model.md) - 枢衡是**一个进程**，既是控制面也是数据面
* [配置分层](/architecture/configuration.md) - DSL → 结构化配置 → 运行时对象图；★ 不做反向生成
* [DSL 指令集参考](/architecture/dsl-reference.md) - ★ M1 的完整指令清单；执行顺序表是**公开契约**
* [管理面](/architecture/control-plane.md) - 全量 load + Runtime 增量双通道；★ 基线 ⊕ 临时覆盖层
* [TLS 与自动 HTTPS](/architecture/tls.md) - ⚠ ★ **此后后端是 BoringSSL 不是 rustls**（G104 推翻 §5.1 第 1 条）；不留双路径、On-Demand 强制准入
* [数据路径](/architecture/data-path.md) - 三个入口一个路由层；静态与缓存；上游发现与健康
* [回落层](/architecture/fallback.md) - 过渡期顶班，1.0 时归零；四条防止它长成第二个产品的约束
* [观测](/architecture/observability.md) - 指标 + 结构化日志 + Runtime 实时 stats

## [技术与运维](/platform/index.md)

* [技术栈](/platform/tech-stack.md) - Rust + Pingora 0.8.1 + **quiche** + **BoringSSL**（★ 由 G103/G104 换掉 quinn/h3 与 rustls）；单静态二进制
* [依赖策略](/platform/dependency-policy.md) - G29：追新 + 不设上界 + `Cargo.lock` 入库 + **24 小时安全怀疑期**
* [供应链现状](/platform/supply-chain.md) - ★ **必读**：脱字号需求自带上界、pingora 的十条天花板、4 条公告与可达性分析
* [HTTP/3 库选型事实表](/platform/http3-libraries.md) - D11 拍板前的事实，**不给推荐项**；★ quiche 用 BoringSSL 不是 rustls、s2n-quic 根本不带 HTTP/3
* [构建与验证](/platform/build-and-test.md) - 一切在 Docker 里跑（G26）；宿主机只需 Docker
* [部署](/platform/deploy.md) - systemd unit 照抄形状；★ `TimeoutStopSec` 怎么算；变更 / 回滚 / 换二进制三条路
* [安全基线](/platform/security-baseline.md) - 管理面默认不出机器；不自己实现 TLS/HTTP2/HPACK/QUIC

## [验证](/verification/index.md)

* [M0 接缝验证](/verification/m0-seam.md) - ✅ **已通过**：自建 TCP/UDP 监听器参与了 Pingora 的 socket 移交
* [性能验收标准](/verification/performance-bar.md) - G19：三家全对拍、逐类设门、脚本与原始数据全公开
* [musl + BoringSSL 静态链接](/verification/musl-boringssl.md) - ✅ **已通过**（G103/G104 的未验前置）；★ ★ ★ 而卡住它的是**构建宿主**，不是 musl 也不是 BoringSSL
* [尚未验证的接缝](/verification/open-seams.md) - 还剩哪些没被代码证明；★ 升级窗口内 UDP 数据报分流

## [外部材料](/references/index.md)

* [OKF 规范](/references/okf-spec.md) - 本 bundle 遵循的格式
* [PLAN.md](/references/plan.md) - 唯一权威的入口与章节地图
* [AGENTS.md](/references/agents.md) - agent 的项目级约束
* [上游技术入口](/references/upstream.md) - Pingora / quiche / BoringSSL 与三家的官方文档

## Agent 约定

`agents/` 存的是**工具怎么用这个仓库**（issue 落在哪、领域文档去哪找、装了哪些工程技能、本 bundle 自己的维护合同），不是产品知识。项目级 agent 约束的权威始终是 [`AGENTS.md`](../AGENTS.md)，本目录只补它引到的那几份细则。

* [Agent 配置索引](/agents/index.md) - 五份细则的入口：issue tracker 与 Wayfinder 约定、triage 标签映射、领域文档布局（`CONTEXT.md` / ADR，不存在就静默跳过）、工程技能编排，以及本 bundle 遵循的 OKF v0.2 维护合同。
