---
type: 验证记录
title: rustls 接缝 · 打开 G6 的那个 feature 会发生什么
description: 三道门结构上都够不到它；打开后一次进来 42 个从未受审的 crate，其中 aws-lc-rs 被无谓编进产物。
resource: ../../vendor/pingora/pingora-rustls/Cargo.toml
tags: [验证, 已通过, 必读, 易错, 供应链, TLS]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-14T00:00:00Z
sources:
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 1 条硬约束（TLS 后端锁死 rustls）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G6（统一 rustls）、G13（musl 静态二进制）、G30（fork 的由来）、G41（本轮）
---

★ **本页记的是实际跑出来的东西，属历史事实。** 结论可以被后续实验推翻，但「那几次跑出了什么」不会变。

> **起因是一条早就写在文档里、却没人动的待办。** [`TLS 与自动 HTTPS`](/architecture/tls.md)
> 自己登记着：「`pingora-core` 的 `default = []`，rustls 后端要显式开 `features = ["rustls"]`……
> **G6 这条决策目前在 `Cargo.toml` 里还没有任何表达**——M1 第一天就要补上。」
>
> 去补的时候发现，它不是补一行 feature 的事。

# 一、★ ★ ★ 这条接缝从来没有被编译、测试或审计过

**三道门各自都够不到它，而且都不是疏忽，是结构。**

| 门 | 为什么够不到 | 判据 |
|---|---|---|
| G30 的上界审计 | 跑在**根 workspace** 上 | 根 `Cargo.lock` **不含** `pingora-rustls`／`rustls`／`ring`／`rustls-native-certs` |
| `tests/vendor/run.sh` 回归网 | `cargo test` **不带任何 `--features`** → `default = []` | rustls 那一半代码一行都不跑 |
| `tools/supply-audit.py` | 同第一条 | 同第一条 |

★ **「根 lock 不含它」不是特例**：实测 `sentry`／`x509-parser`／`ouroboros` 同样缺席——
**未启用的可选依赖一个都不在根锁里**。也就是说这个盲区的形状是通用的，不只针对 rustls。

**实测代价**：

```
cargo tree -p pingora-core -e normal            → 133 个 crate
cargo tree -p pingora-core --features rustls    → 175 个 crate
```

> ★ ★ **M1 第一天打开那个 feature，产品依赖图会一次进来 42 个从未过任何一道门的 crate。**

# 二、一个具体缺陷：明明用 ring，却把 aws-lc-rs 一起编了进去

`pingora-rustls` 的清单两处都没写 `default-features = false`：

| 依赖 | 上游写法 | 那个 crate 的 `default` |
|---|---|---|
| `rustls` | `{ version = "0.23.12", features = ["ring"] }` | `["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]` |
| `tokio-rustls` | `"0.26.0"` | `["logging", "tls12", "aws_lc_rs"]` |

★ **两扇门，都通向 aws-lc-rs**——只关一扇没有效果。

而这个 crate 自己装的是 **ring**：

```rust
pub fn install_default_crypto_provider() {
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}
```

crate 里另一处密码学调用是 `ring::digest`。**aws-lc-rs 编进去了，一行都不会被调用。**

## 代价不是「多两个 crate」

| | 源码体积 | 怎么编 |
|---|---|---|
| `ring` | 8.5 MB | Rust + 少量汇编 |
| **`aws-lc-sys`** | ★ **69 MB** | **C（AWS-LC，BoringSSL 分支），走 cmake** |

这直接顶到 G13 的 **musl 静态二进制**分发，也是白送的攻击面。

## 修法与实测

```toml
rustls       = { version = "0.23.12", default-features = false,
                 features = ["ring", "logging", "std", "tls12"] }
tokio-rustls = { version = "0.26.0",  default-features = false,
                 features = ["ring", "logging", "tls12"] }
```

| 判据 | 结果 |
|---|---|
| 编译 | ✅ 通过 |
| 依赖图 | 175 → **173**，★ **只少了 `aws-lc-rs` 与 `aws-lc-sys`**，别的一个没动 |
| 回归网 | stock 与改后**逐项相同**（跑了对照组，见下）|

★ **唯一被丢掉的默认项是 `prefer-post-quantum`，而它是安全的**——
`prefer-post-quantum = ["aws_lc_rs"]`，**它自己就是 aws-lc-rs 的一扇门**；
rustls 源码里每一处 `#[cfg(feature = "prefer-post-quantum")]` 都在 `src/crypto/aws_lc_rs/` 下。
**ring provider 之下它是空操作。** ★ 这是读源码确认的，不是推断。

★ 按 G32 查过上游：`main`（`0046038`）的清单**一字未改**，全仓也没有别处关掉过 rustls 的默认
（唯一一处 `default-features = false` 是 `pingora/Cargo.toml` 里的 `reqwest`，无关）。
→ **投稿三**2026-08-19 发出：issue [#965](https://github.com/cloudflare/pingora/issues/965)
→ PR [#966](https://github.com/cloudflare/pingora/pull/966)。

## ★ ★ ★ 2026-08-19 补：其实是**三扇门**，而第三扇是判据结构上够不到的

发出去当天，上游 Codex 复审提了一条 P2：`pingora-core` 自己的 `[dev-dependencies]` 里
还有一行裸的 `rustls = "0.23"`。实测属实，**而且比它说的更宽**——那条 dev-dependency
**是无条件的**，所以不只影响 rustls feature 构建：

| `cargo tree -p pingora-core`（唯一 crate 数）| 上游 main | 只关前两扇 | 三扇都关 |
|---|---|---|---|
| `--features rustls -e normal` | 178，aws-lc 在 | **176，没了** | 176，没了 |
| `--features rustls -e normal,dev` | 260，在 | **260，仍在** | **258，没了** |
| 无 feature，`-e normal,dev` | 228，在 | **228，仍在** | **226，没了** |

> ★ ★ ★ **它此前查不出来，不是因为没人想到，是因为判据结构上够不到**：
> 本页上下所有测量都用 `cargo tree -e normal`，而 **`-e normal` 按定义排除 dev-dependencies**。
>
> **这是本项目第三次撞上同一个形状**——
> 第一次是本页第一节（G30 上界审计／回归网／`supply-audit.py` 三道门都够不到 `pingora-rustls`），
> 第二次是 G43（`FORK.md` 的核对命令只 diff 了 `pingora-core` 一个目录），
> 第三次就是这里。**三次的共同点是：那道门当时都是绿的。**

★ **落法上刻意不给 dev-dependency 指定任何 provider**（只留 `logging`/`std`/`tls12`）：
它只被 `connectors/http` 的测试拿去用类型，运行时的 provider 本来就经 `pingora-rustls` 进来。
带 `ring` 的写法也量过——**依赖图一模一样**，所以不指定不损失什么，却能不和上游
[#630](https://github.com/cloudflare/pingora/pull/630) /
[#887](https://github.com/cloudflare/pingora/pull/887) 的 provider 选型工作打架。

⚠ **代价已经付了 5 天**：fork 的 `vendor/pingora/Cargo.lock` 里 `aws-lc-rs`/`aws-lc-sys`
此前一直是 2 条，修完变 **0** 条——**G41 之后每一次回归网都在白编那 69 MB 的 C 源码**。

# 三、★ 对照组：失败集合确实变了，但不是这次改动造成的

打开 rustls 会**新增 18 条通过、5 条失败**。判断这 5 条的归属必须有对照组，否则
「是我改坏的」与「是这个 feature 本来就带的」分不开。

| | passed | failed | ignored |
|---|---|---|---|
| 不开 rustls（原基线）| 335 | 3 | 2 |
| 开 rustls · **stock 清单** | 353 | **8** | 3 |
| 开 rustls · **改过的清单** | 353 | **8** | 3 |

**5 条 rustls 特有的失败在两组里完全一致** → 与清单改动无关。

★ 两次跑之间失败集合有 ±1 的抖动，成员在 `connectors::l4::tests::test_conn_timeout` 与
`protocols::l4::stream::tests::test_rx_timestamp` 之间变——**恰好就是仓库里已登记／已注明
不稳定的那两条**，符合 `tests/vendor/run.sh` 早就写下的「重跑一次再定性」纪律。

# 四、★ ★ ★ 根因查到了：`192.0.2.1` 在 Docker 网络里**有人应答**

5 条新失败全是同一句 `should throw an error`，全部走同一个常量：

```rust
const BLACK_HOLE: &str = "192.0.2.1:79";   // pingora-core/src/connectors/mod.rs:492
```

它们设 1ms 连接超时，等 `ConnectTimedout`。**而这个地址在容器里连得上**：

```
192.0.2.1:79  CONNECTED in 1.7ms   ← RFC 5737 TEST-NET-1，本该不可达
```

★ ★ **已登记的那条 `connectors::l4::tests::test_conn_timeout` 是同一个根因。**
它当初被登记成「环境性失败」是对的，但**根因一直没人查**——
而一旦查了，它是**可以被修掉**的。

## 修法：把环境修对，而不是把名单加长

```bash
iptables -A OUTPUT -d 192.0.2.0/24 -j DROP     # 需要 --cap-add=NET_ADMIN
```

| | passed | failed | 已知失败名单 |
|---|---|---|---|
| 修环境前 | 352 | 9 | 3 条 |
| **修环境后** | ★ **359** | ★ **2** | ★ **2 条** |

★ **只有 `DROP` 有用。** `ip route add blackhole` 给的是立刻 `EHOSTUNREACH`——
那是 `ConnectError` 不是 `ConnectTimedout`，测试照样红。**必须让 SYN 被静默丢弃。**

⚠ **代价**：那批测试现在真的会等满自己的超时，回归网从 ~30s 变成 ~136s。
**这是判据变真实的价钱**——此前它们「快」，是因为它们根本没在测超时。

## 这道门自己要能证明前提成立

`tests/vendor/run.sh` [3/4] 会在跑测试前**自证黑洞**，三个分支都反证过：

| 情形 | 实测 | 结果 |
|---|---|---|
| 没装规则 | 6ms 连上 | ✅ 判红，并给出装规则的命令 |
| 用 `REJECT`（立刻失败）| 1008ms 退出码 1 | ✅ 判红，并说明为什么 `blackhole` 路由不行 |
| 用 `DROP` | 2004ms 超时 | ✅ 通过 |

★ ★ **判据挂在行为上，不挂在「规则在不在」上**：`iptables -L` 列得出规则，不等于它生效
（容器可能没 NET_ADMIN、内核模块可能缺、可能被前面的链抢先 ACCEPT）。
**能证明它生效的只有「连过去真的会超时」这一件事。**

# 五、`rustls-native-certs` 卡在两年前

| | 值 |
|---|---|
| 上游需求 | `^0.7.1` |
| 实际锁定 | `0.7.3`（**2024-08-28**）|
| 最新 | `0.8.4`（2026-06-01）|
| `0.8.0` 发布于 | ★ **2024-08-29——锁定版本的第二天** |

**正是 G30 存在的那个「脱字号自带上界」形状，长在审计够不到的 crate 里。**

## ★ ★ 调用点适配差点写错，这条最值得记

0.8 的 `load_native_certs()` 不再返回 `Result`，改为返回 `CertificateResult { certs, errors }`——
**部分成功从此是可表达的状态**。直觉写法是「`errors` 非空即失败」。

**但那比 0.7.3 更严。** 0.7.3 的 `CertPaths::load()` 结尾写着：

```rust
// promote first error if we have no certs to return
if let (Some(error), []) = (first_error, certs.as_slice()) {
    return Err(error);
}
```

> ★ **它只在一张都没读到时才返回 `Err`**；只要读到了哪怕一张，错误就被咽掉。

照直觉写，`/etc/ssl/certs` 里一个读不动的文件就能让反代**起不来**。
**那是行为变更，不是适配**——而 fork 的纪律是「意图严格限定为放宽上界 + 随之而来的适配」。

落法：`certs.is_empty() && !errors.is_empty()` 才判失败；`errors` 里的内容改为 `warn!` 一行——
**控制流与 0.7.3 完全一致，只是不再无声**（0.7.3 是直接丢掉）。

连带：`0.8.4` 自带 `openssl-probe 0.2.1`，所以最终依赖图是 **174** 而不是 173。

# 六、可以带走的几条

1. ★ ★ ★ **「登记为环境性失败」是把问题挂起，不是把问题解决。**
   名单里每一条都欠着一次根因调查——不查，就永远不知道它其实能被修掉。
   这次查了一条，名单就短了一条，而且门变严了。
2. ★ ★ **一个从没被编译过的 feature，等于一片没有任何判据覆盖的代码**，
   哪怕它在决策日志里被锁死了两个月（G6）。**决策不会自己变成构建配置。**
3. ★ ★ **上游 crate 的 `default` feature 是一条静默的供应链入口。**
   `features = ["ring"]` 读起来像「我选了 ring」，实际是「我在默认之外**又加了** ring」。
4. ★ **迁移到「更好的 API」时，要先把旧 API 的真实语义读出来**，
   而不是照着新 API 的形状写。新 API 把错误暴露出来，不等于旧行为就是「有错即失败」。
5. ★ **判据的覆盖面必须等于被问的范围**：`FORK.md` 问「fork 改了哪些文件」，
   而它的核对命令只 diff 了 `pingora-core` 一个目录——于是连它自己结论段里列着的
   两个 `pingora-runtime` 文件都看不见，workspace 清单更是从建立 fork 起就没被算进去过。

# 相关

[TLS 与自动 HTTPS](/architecture/tls.md) · [供应链现状](/platform/supply-chain.md) ·
[fork 说明](../../vendor/pingora/FORK.md) · [上游投稿](../../upstream-pr/README.md) ·
[尚未验证的接缝](/verification/open-seams.md)
