---
type: 验证记录
title: 验证
description: 已经跑过什么、达标线在哪、还有哪些技术假设没被代码证明。
resource: ../../tests
tags: [验证, 索引]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan
    resource: /references/plan.md
    title: PLAN.md §7 M0、§8 性能验收标准、§9 主要风险
---

★ **本节与 bundle 其他部分不同：它记的是实际发生过的测试与观测到的输出，那是事实不是转述。** 结论可以被后续实验推翻，但「那一次跑出了什么」不会变。

* [M0 接缝验证](/verification/m0-seam.md) - ✅ **已通过**（三次独立运行，三类流量零中断）；★ 顺带给 D2 与 D11 各留下一条结论
* [M1 spike #1 · systemd 下的零停机升级](/verification/m1-systemd.md) - ✅ **已通过，但推翻了 G31 的一半**：撑住 unit 的是 `ExitType=cgroup`，而「抢过 MainPID」会**悄悄弄丢优雅停机**；★ 顺带捞到一条 M0 不可能覆盖的新缺陷（移交来的监听 fd 没有 `CLOEXEC`）
* [musl + BoringSSL 静态链接](/verification/musl-boringssl.md) - ✅ **已通过**（G103/G104 的未验前置，2026-08-25）：完全静态的 musl 二进制在 `FROM scratch` 里跑完真的 QUIC 握手，`set_select_certificate_callback` 真的被调用；★ ★ ★ **而卡住它的从来不是 musl 也不是 BoringSSL，是构建宿主** —— bindgen 要 `dlopen`，而 Alpine 上 build script 默认是静态的，报错文本里三个词却全指向 BoringSSL
* [rustls 接缝 · 打开 G6 的那个 feature 会发生什么](/verification/m1-rustls-seam.md) - ✅ **已通过**（G41/G42）；★ ★ ★ **这条接缝从来没被编译、测试或审计过**——三道门结构上都够不到它，打开后一次进来 **42 个从未受审的 crate**，其中 `aws-lc-rs` 被无谓编进产物；★ 顺带把回归网的已知失败名单从 3 条**缩到** 2 条（查根因，而不是再登记一条）
* [性能验收标准](/verification/performance-bar.md) - ★ **尚未落地**，是 M3 的内容。逐类设门、不劣于该类最强者 10%
* [尚未验证的接缝](/verification/open-seams.md) - ★ M0 解除了最大的一条，但**衍生出一条新的**：升级窗口内 QUIC 连接归属
