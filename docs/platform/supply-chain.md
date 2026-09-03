---
type: 技术基线
title: 供应链现状
description: fork 后只剩 1 条失维公告、0 个真漏洞；剩余陈旧包全部被「已是最新版」的第三方 crate 卡住。
resource: ../../Cargo.lock
tags: [依赖, 安全, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G29（追新 + 24 小时怀疑期）、G30（fork pingora 放宽上界）
  - id: plan-9
    resource: /references/plan.md
    title: PLAN.md §9 风险表（Pingora 上游演进与枢衡定制的合并成本）
  - id: crates-io
    resource: https://crates.io/api/v1/crates/pingora-core/0.8.1/dependencies
    title: pingora-core 0.8.1 在 crates.io 上声明的依赖需求
  - id: osv
    resource: https://api.osv.dev/v1/querybatch
    title: OSV.dev 对 Cargo.lock 全量 176 包的公告查询
---

★ **本页记的是先前的实测快照。** 数据来自对 `Cargo.lock` 全部 176 个 registry 包逐个查 crates.io 与 OSV.dev。**重跑方式在末节。**

# 结论先行

★ **G30 的 fork 已执行完毕。** 下表是前后对照：

| | fork 前 | fork 后 |
|---|---|---|
| 锁定包总数 | 176 | **162** |
| 陈旧包 | **44** | **17**（排除非 Linux 目标后 **12**，其中 2 项还不参与编译）|
| 安全公告 | 4 | ★ **1** |
| 其中**真漏洞** | 2（`lru` UAF、`protobuf` DoS）| ★ ★ **0** |
| 5 个直接依赖 | 全是最新 | 全是最新 |
| Docker 基镜像 | `rust:1-bookworm`（Debian 12 oldstable）| **`rust:1-trixie`**（Debian 13，Rust 1.97.1）|

**只剩 `daemonize` 一条**（RUSTSEC-2025-0069，失维、无 CVE、无可升版本）。★ **它是被权衡后保留的，不是漏掉的**——换掉它意味着手写特权丢弃代码，理由与出路见 [`FORK.md`](../../vendor/pingora/FORK.md) 第 7 节与 [D12](/governance/open-questions.md)。★ 此后 `rustls-pemfile`（RUSTSEC-2025-0134）**已由 G45 迁走**，不再在名单上。

> ★ ★ ★ ** 重要更正：上面这句「只剩一条」的适用范围比它读起来窄。**
>
> 本页所有数字都是**根 `Cargo.lock`** 的。而根锁**不含任何未启用的可选依赖**——实测
> `pingora-rustls`／`rustls`／`ring`／`rustls-native-certs`／`sentry`／`x509-parser`／`ouroboros`
> **一个都不在里面**。G6 把 TLS 后端锁死在 rustls，所以 M1 打开 `features = ["rustls"]` 的那天，
> 产品依赖图会 **133 → 175**，一次进来 **42 个本页从来没数过的 crate**。
>
> **对 vendor 锁跑一次同一个脚本（`--lock vendor/pingora/Cargo.lock`），就多出一条未登记公告**：
>
> | | 公告 | 类型 | 现状 |
> |---|---|---|---|
> | `rustls-pemfile` 2.2.0 | **RUSTSEC-2025-0134** | 失维（仓库 2025-08 起归档）| ~~⏳ 待 owner 处置~~ → ✅ **由 G45 迁到 `rustls-pki-types` 的 `PemObject`**（迁移前先补了 18 条特征化测试 + 三条反证）|
>
> ★ 它与 `daemonize` **不同**：`daemonize` 无路可走（换掉＝手写特权丢弃），
> 而这条**有干净的迁移路径**——上游建议改用 `rustls-pki-types` 的 `PemObject` trait，
> 而 **`rustls-pki-types 1.15.1` 早就在依赖图里**（`PemObject` 需要 ≥1.9.0）。
> fork 里只有 4 处调用点，全在 `pingora-rustls/src/lib.rs`。
> ⚠ 但它动的是**证书与私钥的解析路径**，属安全敏感，不是清单改一行的事。
>
> 完整经过见 [rustls 接缝验证](/verification/m1-rustls-seam.md)。

> ★ ★ ★ **还有一条方向相反、而且更难想到的：`Cargo.lock` 也可能比依赖图**多**。**
>
> 上面那条说的是「锁里**少**了未启用的可选依赖 ⇒ 审计看不见它们」。反过来的那一面是：
> 关掉 `instant-acme` 的 `hyper-rustls` feature 之后，`hyper-rustls` / `rustls` / `tokio-rustls` /
> `schannel` **仍然写在锁里，而已经没有任何人链接它们**。
>
> **成因**：`instant-acme` 的 `ring` feature 里有一句 `"hyper-rustls?/ring"`。那个 `?` 的语义是
> 「只有当可选依赖 `hyper-rustls` 被别处打开时，才顺带开它的 `ring`」——
> ⚠ **feature 解析确实没有打开它，而包级解析照样把它写进了锁。**
>
> ✅ **隔离实验**：新建一个只依赖
> `instant-acme = { default-features = false, features = ["ring", "x509-parser", "time"] }`
> 的空 crate，`cargo generate-lockfile` 之后 —— **锁里 129 个包，`cargo tree` 只有 72 个**。
>
> ⇒ **一条可带走的口径**：
>
> | 问 | 用什么答 |
> |---|---|
> | **锁里写着**哪些 | 读 `Cargo.lock`（`supply_gates.rs` 门 1 / 门 4）。⚠ 它既可能**少**（未启用的可选依赖）也可能**多**（本条）|
> | **依赖图里真有**哪些 | `cargo tree -e all --workspace --target all --locked`（门 5）|
> | **产物里真的链接了**哪些 | ⏳ 仍无判据 —— **D23** |
>
> ⚠ ⚠ **这三问此前被当成同一问**，而门 4 的注释里那句「换完时我会先红一次提醒你改」
> 正是建立在那个混同上 —— **③ 换完了，它没红。**

## ★  修正：还原了四条纯装饰的上界，安全态**实测未变**

`strum` / `strum_macros` / `daggy` / `openssl-probe` / `windows-sys` 已还原为上游值（判据与理由见 [`FORK.md`](../../vendor/pingora/FORK.md)「还原了四条纯装饰的上界」）。**对全量 `Cargo.lock` 重查了一次 OSV**：

| | 还原前 | 还原后 |
|---|---|---|
| 锁定包总数 | 169 | 178 |
| 安全公告 | 1（`daemonize`）| ★ **1（`daemonize`）——没变** |
| 其中真漏洞 | 0 | ★ **0——没变** |

★ **「+9 个包」全是假象**：新增的 10 个 `windows_*` / `windows-targets` 是 `windows-sys 0.59` 的依赖展开方式与 `0.61` 不同造成的，**在 Linux 目标上一行都不编译**。这正是本页反复强调的 **报告陈旧依赖必须分桶**。

★ ★ **反而少了两个真正在编译的陈旧包**：`hashbrown 0.15.5` 与 `foldhash 0.1.5` **从依赖图里消失了**——本页下文那张表说它们被 `petgraph 0.8.3` 卡住，而 `petgraph 0.8.3` 正是 `daggy 0.9` 拖进来的。还原 `daggy` 把这条成因一并带走了。**下文那张「10 项」的表因此要读作 8 项。**

## ★ 剩下 10 个真正在编译的陈旧包，一个都升不动——已逐条查过绑定约束

**它们全部被「本身已经是最新版」的第三方 crate 卡住**，也就是说 **不是我们落后，是生态链上游还没采纳新大版本**：

| 陈旧包 | 卡它的 requirement | 卡它的包是最新版吗 |
|---|---|---|
| `allocator-api2` 0.2.21 | `hashbrown 0.17.1` → `^0.2.9` | ★ 是 |
| `base64` 0.22.1 | `sfv 0.15.0` → `^0.22.1` | ★ 是 |
| `alloc-no-stdlib` · `alloc-stdlib` | `brotli 8.0.4` → `<3` / `~0.2` | ★ 是 |
| `thiserror` · `thiserror-impl` 1.0.69 | `protobuf 3.7.2` → `^1.0.30` | ★ 是 |
| `getrandom` 0.3.4 | `ahash 0.8.12` → `^0.3.1` | ★ 是 |
| `syn` 2.0.119 | `strum_macros 0.28.0` 等 → `^2.0` | ★ 是 |
| `hashbrown` 0.15.5 · `foldhash` 0.1.5 | ★ **`petgraph 0.8.3` → `^0.15.0`**（`indexmap` / `lru` 都已要 `^0.17`）| ★ 是 |

★ ★ **结论：要再往下追，就得同时 fork `brotli` / `sfv` / `hashbrown` / `protobuf` / `ahash` / `petgraph` 六七个包——而这一层已经没有任何安全公告，纯粹是版本号。性价比为负，明确不做。**

★ **另外 2 项（`bitflags 1.3.2`、`miniz_oxide 0.8.9`）根本不参与编译**：用 `cargo tree --target x86_64-unknown-linux-gnu -e normal` 核过，它们只存在于其他 target/feature 的解析结果里。

## ★ 先前的最新一次实测（`tools/supply-audit.py`，带分桶）

| | 数值 |
|---|---|
| 锁定包总数（registry） | **171** |
| `x86_64-unknown-linux-gnu` 上真正参与编译 | **138** |
| 陈旧包（参与编译） | **13** |
| 陈旧包（仅在 lock 里，不进产物） | **17** |
| 安全公告 | **1**（`daemonize`，已登记并接受）|
| ★ 其中**未登记** | ★ **0** |

参与编译的那 13 个：`alloc-no-stdlib` · `alloc-stdlib` · `allocator-api2` · `base64` · `daggy` · `getrandom` · `openssl-probe` · `petgraph` · `strum` · `strum_macros` · `syn` · `thiserror` · `thiserror-impl`。

★ 其中 `daggy` / `petgraph` / `strum` / `strum_macros` / `openssl-probe` 是 ** 主动还原为上游值的**（见上一节），不是升不动；其余仍被第三方 crate 的上界卡住。

fork 本身见 [`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md)。**下面几节记的是 fork 之前的诊断**，保留是因为它解释了天花板的成因——那套机制没有因为 fork 而消失，下次遇到同类问题还要用。

# 直接依赖：已经是最新

| crate | `Cargo.toml` 下界 | 锁定版本 | crates.io 最新 |
|---|---|---|---|
| `pingora-core` | `>=0.8.1` | 0.8.1 | **0.8.1** ✅ |
| `tokio` | `>=1.53.1` | 1.53.1 | **1.53.1** ✅ |
| `async-trait` | `>=0.1.89` | 0.1.92 | **0.1.92** ✅ |
| `log` | `>=0.4.28` | 0.4.33 | **0.4.33** ✅ |
| `env_logger` | `>=0.11.8` | 0.11.11 | **0.11.11** ✅ |

G29 的 `>=` 无上界写法**是对的，而且已经在生效**——三个下界落后于锁定版本，正说明 Cargo 一直在往上跳。

# ★ 为什么另外 44 个升不动

## 机制：脱字号需求自带上界

`Cargo.toml` 里写 `brotli = "3"` **不是**「≥3」。它是**脱字号需求** `^3` = `>=3.0.0, <4.0.0`。

★ **每一个不带符号的版本字符串都自带一个上界**，落在下一个 semver 破坏性边界上。所以一份从没写过 `<` 的清单，可以布满上界。

Cargo 必须**同时满足依赖图里的每一条需求**。枢衡只点名了 5 个 crate，另外 171 个是 pingora 带进来的——**带着 pingora 的需求一起进来**。对一个自己没点名的包，枢衡没有投票权。

## pingora-core 0.8.1 实际声明的上界

| pingora 写的 | 真实含义 | crates.io 最新 | 连带卡住 |
|---|---|---|---|
| `nix = "~0.24.3"` | `>=0.24.3, <0.25.0` ← ★ **波浪号，连小版本都钉死** | 0.31.3 | `bitflags` 1.x · `memoffset` 0.6 |
| `brotli = "3"` | `<4.0.0` | 8.0.4 | `brotli-decompressor` · `alloc-*` |
| `prometheus = "0.13"` | `<0.14.0` | 0.14.0 | ★ **`protobuf` 锁在 2.x** |
| `lru = "0.16.3"` | `<0.17.0` | 0.18.2 | ★ **UAF 那条** |
| `rand = "0.8"` | `<0.9.0` | 0.10.2 | `rand_chacha` · `rand_core` |
| `sfv = "0.10.4"` | `<0.11.0` | 0.15.0 | `base64` 0.22 |
| `daggy = "0.8"` | `<0.9.0` | 0.9.0 | `petgraph` 0.7 |
| `strum` / `strum_macros = "0.26.2"` | `<0.27.0` | 0.28.0 | |
| `openssl-probe = "0.1.6"` | `<0.2.0` | 0.2.1 | ★ 仅在 openssl/boringssl 后端，rustls 下**不编译** |
| `derivative = "2.2.0"` | ★ **非可选**，且已失维 | 无新版 | `syn` 1.0.109 |
| `daemonize = "0.5.0"` | ★ **非可选**，且已失维 | 无新版 | |

★ **上游 main 分支也一样。** 核过 `cloudflare/pingora` 的 main：`brotli 3` / `rand 0.8` / `strum 0.26` / `daggy 0.8` / `lru 0.16.3` / `derivative` 一个都没动（只有 `nix` 抬到了 `~0.31.1`，但尚未发版）。

## ★ `[patch.crates-io]` 打不破需求

这一点极易误判：**被 patch 的 crate 仍然必须满足依赖方的版本需求**。把 `brotli` patch 成 8 再指望 `^3` 通过是不行的，Cargo 会直接报错。

**唯一的杠杆是替换 `pingora-core` 本身**（fork 并改它的清单），不是 patch 那些叶子包。

## 44 项的三类分桶

| 类 | 数量 | 说明 |
|---|---|---|
| **pingora 卡住** | ~22 | 上表及其连带 |
| **中层 crate 卡住** | ~5 | `hashbrown` / `allocator-api2`（via `indexmap`+`lru`）、`getrandom`（via `ahash`+`rand_core`）、`miniz_oxide`（via `flate2`）|
| ★ **目标平台上根本不编译** | ~17 | `windows-*` 11 个 · `redox_syscall` · `wasi` · `wit-bindgen` · `r-efi` ×2 —— G13 的目标只有 Linux x86_64 / aarch64，**这些只是 Cargo 记录的完整图，不进二进制** |

★ **报告陈旧依赖数量时必须分桶。** 「44 个包过时」听起来吓人，但其中 17 个在目标平台上一行代码都不会被编译。

# 4 条公告与可达性

OSV.dev 对全部 176 包的查询结果：

| 公告 | 包 | 类型 | 可达性 |
|---|---|---|---|
| **RUSTSEC-2026-0253** | `lru` 0.16.4 | memory-corruption（UAF）| ★ 当时**不可达**，见下；✅ **现已修掉** |
| **RUSTSEC-2024-0437** | `protobuf` 2.28.0 | DoS（栈溢出）| ★ 当时**不可达**，见下；✅ **现已修掉** |
| RUSTSEC-2025-0069 | `daemonize` 0.5.0 | 失维 | 无 CVE；★ 「追最新」不适用——**没有更新的版本** |
| RUSTSEC-2024-0388 | `derivative` 2.2.0 | 失维 | ✅ **已修**——换成 `educe`，见上 |

## ★ 两条真漏洞为什么按当前实例化不可达

**`lru` 的 UAF** 触发条件是「**存储的键的 `Drop` 实现在 `pop()` 中 panic**」。pingora-pool 实例化的是 `Lru<ID, ConnectionMeta>`（`pingora-pool/src/connection.rs`），**键是整数类型——没有 `Drop` impl 可以 panic**。

**`protobuf` 的 DoS** 要求**解析**不可信输入。`prometheus` 只在 protobuf 曝露格式上走**编码**路径，枢衡不解析任何外部 protobuf。

★ ★ **但这两条判断依赖 pingora 的内部实现，pingora 改了它们就可能失效。** 所以它们**照样登记、照样跟踪**，只是不构成「今天就危险」。**不要把「当前不可达」记成「已修复」。**

# owner 的处置（G30）——已执行

 拍板并当日执行：**fork `pingora-core` 放宽依赖上界**，而不是接受天花板等上游。

★ **代价已被明确告知并接受**：这直接把 `PLAN.md` §9 那条「Pingora 上游演进与枢衡定制的合并成本」**从一条已登记的风险变成一项进行中的成本**。

## 结果

| 修掉的 | 怎么修的 |
|---|---|
| ★ **RUSTSEC-2026-0253**（`lru` UAF）| `lru` 0.16.4 → **0.18.2** |
| ★ **RUSTSEC-2024-0437**（`protobuf` DoS）| `prometheus` 0.13 → **0.14**，连带 `protobuf` 2.28 → **3.7.2** |
| ★ **RUSTSEC-2024-0388**（`derivative` 失维）| 换成 **`educe` 0.7.6**（5 处，1 个文件）；★ **连带白赚 `syn 1.0.109` 一起消失**——`derivative` 是全图唯一还要 `syn ^1` 的包 |

同批抬起来的还有 `nix` 0.24.3 → **0.31.3**、`brotli` 3 → **8**、`rand` 0.8 → **0.10.2**、`sfv` 0.10 → **0.15**、`strum` 0.26 → **0.28**、`daggy` 0.8 → **0.9**（连带 `petgraph` 0.7 → 0.8）、`openssl-probe` 0.1 → **0.2**、`bitflags`（nix 侧）1 → **2**、`thiserror`（prometheus 侧）1 → **2**。

**实际改动量：版本上界 12 个调用点 / 3 个文件，加上 `derivative`→`educe` 的 5 处 / 1 个文件**——比预估的 76 个少得多，因为大部分调用点在破坏性升级后**签名没变**。

★ ★ **`nix` 那一轮恰好落在 `server/transfer_fd/mod.rs` 上，也就是 M0 要验的那个模块。** 改法照抄上游 main（它已自行抬到 `~0.31.1`），**但没有整文件覆盖**——上游 main 还带着与本次无关、且未被 M0 验证过的行为变更。

**改动后 M0 连跑三次全绿**，两个自建监听器都报告了 `INHERITED`。见 [M0 接缝验证](/verification/m0-seam.md)。

## 破坏面实测

在枢衡实际用到的 **6 个 crate 闭包**内统计（`pingora-core` / `-error` / `-http` / `-pool` / `-runtime` / `-timeout`，共 38,876 行，`src/` 下不含测试与示例）：

| 依赖 | 调用点 | 文件 | 备注 |
|---|---|---|---|
| `nix` 0.24→0.31 | 17 | 5 | 最大的一块 |
| `daemonize` | 18 | 4 | ★ 失维，需替换或 vendor |
| `brotli` 3→8 | 10 | 2 | 集中在 `protocols/http/compression/` |
| `rand` 0.8→0.10 | 10 | 3 | |
| `lru` 0.16→0.18 | 9 | 3 | ★ 其中 1 个文件在 s2n 后端，**rustls 下不编译** |
| `daggy` 0.8→0.9 | 5 | 2 | |
| `derivative` | 2 | 1 | ★ 失维，仅 `upstreams/peer.rs` |
| `prometheus` 0.13→0.14 | 2 | 1 | |
| `sfv` 0.10→0.15 | 2 | 1 | |
| `strum` 0.26→0.28 | 1 | 1 | |
| `openssl-probe` | 1 | 1 | ★ boringssl/openssl 后端，**rustls 下不编译** |

**合计 ~76 个调用点。** ★ 两个最初看起来最麻烦的（`openssl-probe` 与 `lru` 在 s2n 后端的用法）**在 rustls-only 构建下根本不参与编译**。

两个失维包的处置与 fork 的长期维护方式当初登记为 **D12**，⚠ **它已经不在**
[待定清单](/governance/open-questions.md) **里了**：fork 的长期维护方式由 **G32** 定
（有公告支撑的上界推 PR 给上游，其余常年 rebase），两个失维包各自的现状见本页上文
（`derivative` 已换成 `educe`；`daemonize` 是权衡后保留的）。

# 怎么重跑这份快照

依赖检查的日常入口是 [依赖策略](/platform/dependency-policy.md) 里的 `tools/dep-check.py`。★ **但它只报告 `cargo update` 能动的东西**——本页那 44 项它一个都不会报，因为 `cargo update` 对它们是**空操作**。

✅ **已经有脚本了**（补上，此前是手工做的）：

```bash
python tools/supply-audit.py                       # 只查 lock 层面
python tools/supply-audit.py --cargo "<cargo 命令>" # ★ 带分桶，见下
python tools/supply-audit.py --cargo "..." --markdown  # 额外输出可直接贴回本页的表
```

它做的正是 `dep-check.py` 结构上做不到的那件事：**逐包对比 crates.io 的 `max_stable_version`**（陈旧度）。

★ ★ **公告那一半已并进 `dep-check.py` 的第五项**（退出码 `160`），**每周自动扫两把锁**（`vendor/pingora/Cargo.lock` + 根锁），`ACCEPTED` 名单两边共用同一份。此前它只在有人记得手动跑的时候才发生，而**默认扫的还是次要的那把锁**——`h2` 的 RUSTSEC-2026-0258 就是这么撞见的。本页的手动深审现在主要用于**陈旧度**与分桶。

★ ★ **一定要传 `--cargo`**，否则拿不到编译图，数字会偏大一倍（30 vs 13）。在这台机器上：

```bash
python tools/supply-audit.py --cargo "docker run --rm -v D:/Workspace/Fulcrum:/w -v fulcrum-cargo:/usr/local/cargo/registry -v fulcrum-target:/w/target -w /w fulcrum-build:local cargo"
```

**退出码**（★ 会叠加，风格同 `dep-check.py`）：

| 码 | 含义 |
|---|---|
| `0` | 没有未登记的公告，且查证覆盖率达标 |
| `20` | ★ 有未登记的公告 |
| `40` | ★ **查证覆盖率不足**——未能查证的包超过 `--max-unresolved-pct`（默认 10%）|
| `60` | 两者都有 |
| `1` | 出错 |

**陈旧包不判红**——目标构建里长期有十来个升不动的陈旧包，把它算作失败会让这道门永远红，而永远亮着的告警等于没有告警。已登记并接受的公告写在脚本的 `ACCEPTED` 里，**每条都必须带「为什么接受」和「出路在哪」**。

★ ★ **`40` 这一档补的是一个 fail-open 的洞**：`unresolved`（未能查证）此前**只影响打印、不影响退出码**。于是 crates.io 整体不可达时（公司网络、DNS、限流），164 个包会全部落进 `unresolved`，脚本打一行警告后照常输出「没有未登记的安全公告」并 **exit 0**——而每周的 cron / CI 只看退出码，会把一次**什么都没查到**的运行记成一次通过。

★ 阈值卡的是**比例**不是绝对值：被限流是常态，零星几个查不到不该拦人；**整体查不到**必须拦。这是「不能让门永远红」与「不能让门假装绿」之间的那条线。

## ★ 它第一次跑就纠正了本页的一处措辞

`openssl-probe` 被分桶为 **「编译」**，而此前文档写的是「rustls 下不编译」。核实后：它是 `pingora-core` 的**非可选**依赖，**crate 本身照样被编译**；rustls-only 下不编译的是它的**唯一调用点**（`connectors/tls/boringssl_openssl/mod.rs`，被 `cfg` 挡掉）。

★ **所以分桶其实有四层，不是三层**：

> 「在 lock 里」→「在编译图里」→「**代码被用到**」→「能不能升」

前两层脚本能分，第三层要读 `cfg`。**混用这四层就会得出既吓人又无意义的数字。**

# 相关

[依赖策略](/platform/dependency-policy.md) · [技术栈](/platform/tech-stack.md) · [待定清单](/governance/open-questions.md) · [决策日志](/governance/decision-log.md)
