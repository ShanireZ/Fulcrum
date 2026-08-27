---
type: 项目治理
title: 决策日志导航
description: 哪条决策管哪件事、各轮拍板的脉络、owner 推翻 AI 建议的那些决定，以及三条不可回头的硬约束。
resource: ../../PLAN.md
tags: [治理, 必读, 决策]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 决策清单（全文）
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 G6 附带的三条硬约束
---

★ **完整表述在 [`PLAN.md`](../../PLAN.md) §10，本页只做导航。** 引用时引 G 编号与 `PLAN.md`，不要引本页。

每一条都由 owner 拍板，按顺序追加在 `PLAN.md` §10 末尾。**本页不复述条数** —— ★ 一个没人维护的计数迟早会变成一句假话。

# 按「我想知道什么」查

| 我想知道 | 去看 |
|---|---|
| 为什么是 Rust | G1 |
| 三家产品在这里扮演什么角色 | G2 · G9 · G25 → [回落层](/architecture/fallback.md) |
| 首版到底做多少 | G3 · G5 · G7 → [首版范围](/product/scope.md) |
| 配置长什么样 | G4 · G11 · G20 → [配置分层](/architecture/configuration.md) |
| 底座为什么选 Pingora | G6 → [技术栈](/platform/tech-stack.md) |
| 配置怎么热更新 | G8 · G18 → [管理面](/architecture/control-plane.md) |
| 什么时候开源 | G10 |
| 自动 HTTPS 怎么做 | G12 · G15 → [TLS](/architecture/tls.md) |
| 怎么分发 | G13 → [技术栈](/platform/tech-stack.md) |
| 管理面怎么保证安全 | G14 → [安全基线](/platform/security-baseline.md) |
| 能看到什么指标 | G16 → [观测](/architecture/observability.md) |
| 上游节点怎么发现与摘除 | G17 → [数据路径](/architecture/data-path.md) |
| 性能怎么算达标 | G19 → [性能验收标准](/verification/performance-bar.md) |
| 名字的来历 | G21 |
| 给谁用 | G22 → [定位](/product/positioning.md) |
| 为什么没有 Web UI | G23 |
| 里程碑怎么切 | G24 → [实施路线](/governance/roadmap.md) |
| 为什么全在 Docker 里跑 | G26 → [构建与验证](/platform/build-and-test.md) |
| M0 为什么不含 QUIC | G27 · G28 → [M0 接缝验证](/verification/m0-seam.md) |
| 依赖怎么升级 | G29 → [依赖策略](/platform/dependency-policy.md) |
| ★ pingora 为什么走 fork | **G30** → [供应链现状](/platform/supply-chain.md) · [`FORK.md`](../../vendor/pingora/FORK.md) |
| 静态文件怎么发（符号链接 / 隐藏文件 / 范围请求 / MIME）| **G87 · G88 · G89 · G90**（§10）|

# ★ owner 推翻了 AI 推荐项的决定

这些条目值得单独记住 —— 它们是项目里 AI 判断与 owner 判断分岔的地方，**引用时不要回头去找
AI 当初的建议**。

| | AI 建议 | owner 拍板 | 分歧的根子 |
|---|---|---|---|
| **G3** | 只做「反代 + 自动 HTTPS + 基础 LB」（评 3.0 分） | 三合一全量首版 | 配合 G2/G5 后成立——「全量」指**对外能力面**，不是自研代码面 |
| **G13** | 先做容器镜像，后做包 | 单静态二进制 + systemd + deb/rpm，**不做官方镜像** | 既然是单静态二进制，文档给一份 `FROM scratch` Dockerfile 就够了 |
| **G22** | （未记录反对） | 自己的机队 **与** 自托管站长**并列**主打 | 文档、默认值与迁移工具优先服务第一类 |
| **G25** | 只回落 Caddy 一家（评 8.5 vs 7.0） | 回落 **Caddy + Nginx 两家** | 代价（工程量翻倍、1.0 全删）已知并接受，换过渡期不难看的性能数字 |
| **G29** | 精确钉死 `=0.8.1` | **追新，包括破坏性大版本** + 24 小时怀疑期 | ★ AI 提示「`>=` 无上界会跳到破坏性大版本」后，owner **同日重申**「确定使用最新+包括破坏性的版本更新」——**这是知情后的重申，不是疏漏** |
| **G30** | 接受 pingora 的天花板，跟进上游 0.9（约季度节奏）| **fork `pingora-core` 放宽上界** | 代价（`PLAN.md` §9 的合并成本从风险变成进行中的成本）已认下，换来两条真漏洞立即清零；★ **实测只需改 12 处调用点，远小于预估** |
| **G49** | 书写顺序即执行顺序，零隐式重排序 | **内建顺序表**（照 Caddy）| AI 的理由是「它与 `PLAN.md` §3 点名的 nginx `location` 坑同构」；owner 换来的是「用户随便写也能跑对」与 Caddyfile 迁移零意外（G22 把这两类用户都列为目标）。★ 代价四条已一并落实 |
| **G54** | DNS-01 推到 M2（「要长出一整套 DNS 供应商插件体系」）| **DNS-01 进 M1** | 通配符证书没有别的路；★ 落法收敛为「原生两家 + exec hook 兜底」（G57），不建体系 |
| **G58** | M1 只做机制，通配符不写进验收 | **通配符进 M1 退出条件** | ⚠ 代价 AI 当时就提了：DNS-01 因此位于 M1 关键路径上，出问题直接卡验收。**这是知情后的选择** |
| **G87** | 静态文件只跟随符号链接，不给开关（少一条指令） | **默认跟随 + 加一条 `follow_symlinks false`** | AI 的理由是 §6.2 「能少一条指令就少一条」；owner 换来的是**两侧都有真实用户**——默认那侧要的是零额外系统调用与链接农场可用，关掉那侧要的是符号链接逃逸在结构上做不到 |
| **G88** | 默认 404 掉一切以 `.` 开头的路径段（`.well-known/` 例外） | **不按 `.` 一刀切**，改成一张**可配置的默认 404 清单**（`hide`）| owner 不接受「按字符形状猜危险」这个判据——`.well-known/` 要开例外这件事本身就说明这条规则**猜错过一次**；换成一张**指名道姓**的清单，代价（表要维护）换来的是「为什么 404」永远查得到 |
| **G88**（叠加方式）| `hide` 写了就**整条替换**默认表 | **追加**，另给一条 `hide_defaults false` 关掉默认表 | AI 取替换是为了让默认表能被推翻；owner 用一条独立开关达成同一件事，同时保住「再挡一样只写一行」。★ **两个目标本来就不必二选一** —— 分岔点不在取哪一侧，在于有没有想到第三种写法 |

# ★ 三条不可回头的硬约束（G6 附带）

完整表述见 `PLAN.md` §5.1。这三条**不得绕过**，[`AGENTS.md`](../../AGENTS.md) 也重复了一遍：

1. ⚠ ⚠ ⚠ ~~**TLS 后端锁死 rustls。**~~ —— ★ ★ ★ **由 G104 推翻**（§10），**这是本项目第一次推翻一条写着「不可回头」的硬约束**。现行：后端统一 **BoringSSL**，两个入口共用同一个 `select_certificate_callback`。<br>★ 原文（保留）：动态证书选择必须走 `ResolvesServerCert`，**不是** `certificate_callback`（rustls 后端不支持它）。<br>> ★ **「不可回头」约束的是我们自己不要反复，不是约束事实不许变。** 那句理由今天依然为真 —— 只是我们不再站在 rustls 那一侧了。详见 [TLS](/architecture/tls.md)。
2. **tower 中间件生态用不上。** Pingora 有自己的 `ProxyHttp` 阶段模型，tower 的 `Service`/`Layer` 插不进去——**同名不同物**。⚠ 但那句话**关于上游为真、关于我们为假**：本仓库从来没有用过 `ProxyHttp`，执行链挂在 `HttpServerApp` / `ServerSession` 上。
3. ~~**自建 QUIC/L4 监听器必须接入 Pingora 的 socket 移交**~~ → ✅ **已由 M0 解除**，见 [M0 接缝验证](/verification/m0-seam.md)。★ 但它**衍生出一条新风险**（升级窗口内 UDP 数据报在两代进程间分流），见 [尚未验证的接缝](/verification/open-seams.md)。

# 一处需要留意的编号事实

★ **G29 同时是 D1 的结案。** `PLAN.md` §11 里 D1 已划掉，指向 G29。读到「依赖版本策略待定」这类旧表述时，**它是过期的**。
