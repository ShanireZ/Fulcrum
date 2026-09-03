# upstream-pr —— 给 cloudflare/pingora 的投稿材料

> 依据 [`PLAN.md`](../PLAN.md) §10 **G32**。流程约束见 [`docs/platform/upstream-pr.md`](../docs/platform/upstream-pr.md)。

★ **这个目录是过渡性的**，和回落层一样——**改动被上游接受（或明确拒绝）之后就删掉**。

## 里面是三份投稿，彼此独立

**投稿一 · `lru` 版本升级**（G32，2026-08-14 **已发出**）

> ✅ issue [cloudflare/pingora#961](https://github.com/cloudflare/pingora/issues/961)
> → PR [cloudflare/pingora#962](https://github.com/cloudflare/pingora/pull/962)
>
> ⚠ **发之前重跑门，推翻了 08-13 记的一条**：当时写「失败集合逐项相同（116/116）」，
> 实测是 **116 vs 117**——多出来的是 `pingora-memory-cache` 的 `tests::test_eviction`。
> 查证后确认**与本改动无关**（详见下面「投稿一：发之前查出来的三件事」）。

| 文件 | 用途 |
|---|---|
| [`issue.md`](issue.md) | GitHub issue 草稿。★ **必须先发它** |
| [`pr.md`](pr.md) | PR 正文草稿。发 PR 时把 `#<ISSUE_NUMBER>` 换成真实编号 |
| `0001-Bump-lru-dependency-from-0.16.3-to-0.18.2.patch` | 可直接 `git am` 的补丁，基于上游 `main`（`0046038`），已带 `Signed-off-by` |

**投稿二 · `get_fds_from()` 的两处 fd 泄漏**（G38，2026-08-14 **已发出**，并按复审意见改过一版）

> ✅ issue [cloudflare/pingora#959](https://github.com/cloudflare/pingora/issues/959)
> → PR [cloudflare/pingora#960](https://github.com/cloudflare/pingora/pull/960)
> · ★ 已按 Copilot 的复审意见补一版（`f94e445`，CI 四项全绿）
> · 顺带派生出 issue [#963](https://github.com/cloudflare/pingora/issues/963)
> （2026-08-14 由 owner 授权后代发，见 PLAN.md §10 **G40**）。
>
> ✅ **上游 CI 四项全绿**：`pingora (1.85.0)` 3m55s / `pingora (1.97.1)` 13m42s /
> `pingora (nightly)` 8m19s / `semgrep-oss` 36s；`mergeable = MERGEABLE`，等待评审。
> ★ 本地预跑的那张门表因此**被上游 CI 复核过一遍**——包括本地没装、跑不了的
> `cargo machete` 与完整的 `cargo test`（上游 runner 带 openresty）。
>
> ★ **「PR 被 close」在这里是成功而不是拒绝**——上游走批量重放，判断是否落地
> 要看改动有没有出现在 `main`，别看 PR 状态。

| 文件 | 用途 |
|---|---|
| [`issue-2-fd-leaks.md`](issue-2-fd-leaks.md) | GitHub issue 草稿。★ **必须先发它** |
| [`pr-2-fd-leaks.md`](pr-2-fd-leaks.md) | PR 正文草稿 |
| `0002-Close-the-transfer-socket-and-set-CLOEXEC-on-received-listener-fds.patch` | 可直接 `git am` 的补丁，基于上游 `main`（`0046038`），已带 `Signed-off-by` |

**投稿三 · `pingora-rustls` 无谓编进 aws-lc-rs**（G41，2026-08-19 **已发出**）

> ✅ issue [cloudflare/pingora#965](https://github.com/cloudflare/pingora/issues/965)
> → PR [cloudflare/pingora#966](https://github.com/cloudflare/pingora/pull/966)
> （owner 于 2026-08-19 指示「再审核一遍、严格照上游规范、查过已修与已有人报之后再代发」）。
>
> `pingora-rustls` 向 `rustls` 与 `tokio-rustls` 要了 `ring` provider，但**两处都没写
> `default-features = false`**，而两者的 `default` **都含 `aws_lc_rs`**——于是
> **ring 与 aws-lc-rs 两个 provider 一起编进产物**，而 aws-lc-rs 一行都不会被调用
> （本 crate 自己装的是 ring，另一处密码学调用是 `ring::digest`）。
>
> ★ **代价**：`aws-lc-sys` 是 **69 MB 的 C 源码**（ring 8.5 MB）走 cmake 编。
> 对静态链接 musl 的人是实打实的障碍。

### ★ ★ ★ 发前复审推翻了这份草稿的两处（2026-08-19，G45）

**一、「查过上游做没做」只查了代码，没查未合并的 PR。**

08-14 的原话是「全仓也没有别处关掉过 rustls 的默认」——那句话本身没错，但它回答的是
**代码里有没有**，而 G32 要问的是**有没有人已经在做**。一搜就撞见两个 open PR：

| | 标题 | 状态 |
|---|---|---|
| [#630](https://github.com/cloudflare/pingora/pull/630) | Allow using ring or aws-lc-rs as rustls crypto provider | 2025-05 开，已 conflicts |
| [#887](https://github.com/cloudflare/pingora/pull/887) | Make ring an optional dependency in pingora-rustls | 2026-05 开，clean |

★ **但这不是撤稿理由，反而是本投稿最有价值的一格**：两个 PR **都只改了 `rustls` 那一行**，
而 aws-lc-rs 是从**两扇门**进来的。实测（上游 `main` `0046038`，`cargo tree -p pingora-core -e normal` 唯一 crate 数）：

| 清单形态 | crate 数 | aws-lc-rs / aws-lc-sys |
|---|---|---|
| `main` 原样，`--features rustls` | 178 | 在 |
| #630 对 `rustls` 行的写法单独应用 | 178 | **仍在** |
| #887 完整应用，`--features rustls` | 178 | **仍在** |
| ★ #887 完整应用，`--features rustls-no-provider` | 177 | **仍在**（`ring` 没了）|
| **本投稿（两扇门都关）** | **176** | **没了** |

★ ★ 倒数第二行是给 #887 作者的：它成功甩掉了 `ring`，**但一个明确选择「我自己装 provider」
的消费者，照样要编那 69 MB 的 C**——而那正是他想省掉的东西。差的就是 `tokio-rustls` 一行。

★ **这个实验自证有效**：形态②里 aws-lc 消失了，说明探针既能命中也能落空。

**二、提交信息里的依赖图数字是 fork 的，不是上游的。**

正文写「178 → 176」（上游），而 `git format-patch` 的提交信息里写的是「175 → 173」——
那是 **fork 的数字**（fork 抬过版本上界，解析出来的图不一样）。
★ **提交信息要进上游的历史，挂着一个审阅者复现不出来的数字**，比不写更糟。已改为 178 → 176。

> ★ ★ ★ **可带走的一条**：同一份材料里，**面向内部的数字与面向外部的数字必须分开记账**。
> 这两个数各自都是对的，错的是它们出现在了对方的文档里——而两边都写着「实测」。

| 文件 | 用途 |
|---|---|
| [`issue-3-aws-lc-rs.md`](issue-3-aws-lc-rs.md) | GitHub issue 草稿。★ **必须先发它** |
| [`pr-3-aws-lc-rs.md`](pr-3-aws-lc-rs.md) | PR 正文草稿 |
| `0003-Do-not-compile-aws-lc-rs-when-the-ring-provider-is-requested.patch` | 可直接 `git am` 的补丁，基于上游 `main`（`0046038`），已带 `Signed-off-by` |

### ★ 投稿三的验证（2026-08-19 **重跑**，容器内，基于上游 main `0046038`）

★ **全部逐项重跑过**，不是照抄 fork 侧、也不是沿用 08-14 那份——**PR 里引用的每个数都要审阅者能在上游复现**。
命令一律取自上游 [`.github/workflows/build.yml`](https://github.com/cloudflare/pingora/blob/main/.github/workflows/build.yml)。

| 门 | 结果 |
|---|---|
| `git am` | ✅ 干净应用；★ **回到 `0046038` 重放后与分支树逐字节一致** |
| `cargo tree -p pingora-core --features rustls -e normal` | ★ **178 → 176**；消失的**恰好**是 `aws-lc-rs` 与 `aws-lc-sys`，**且没有任何新增** |
| `cargo fmt --all -- --check` | ✅ 前后皆过 |
| `cargo check --workspace` | ✅ 前后皆过 |
| `cargo build -p pingora-core --features rustls` | ✅ 前后皆过 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ 前后皆过 |
| `cargo test -p pingora-core --lib --no-fail-fast --features rustls` | **566 / 7 / 2**，前后**逐项相同**（双向差集两侧皆空）|
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ 前后皆过，MSRV 不受影响 |
| `cargo audit` / `cargo machete` | 未跑（本地没装），留给上游 CI |

⚠ **那 7 条失败全是环境性的，且与 08-14 那次的「1 条」不矛盾**：这一次容器**没装**
`iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`，于是 G42 查清的那批连接超时测试多红 6 条
（Docker 默认网络替 `192.0.2.1` 应答）；剩下那条仍是 `test_bind_to_port_range_on_connect`。
★ **判据是「前后集合相同」，它不依赖环境**——但**数字依赖**，所以数字旁边必须写清环境。

★ **发之前按 G32 重查了三件事**（2026-08-19）：

1. **上游修了没有**——`main` 仍停在 `0046038`（2026-08-07 起未动），
   `pingora-rustls/Cargo.toml` 那两行一字未改（克隆下来实测，不是读网页）。
2. **有没有人已经报过**——搜 `aws-lc-sys` / `prefer-post-quantum` **零命中**；
   搜 `aws-lc` / `default-features` / `crypto provider` 命中的是上面那两个 PR
   与已结案的 [#446](https://github.com/cloudflare/pingora/issues/446)（另一个症状：
   provider 没显式指定导致 panic，已由 `install_default_crypto_provider()` 解决）。
3. **上游规范**——`.github/CONTRIBUTING.md` 要求非 trivial 的 PR 先开 issue（已照做）；
   issue 正文按 `.github/ISSUE_TEMPLATE/bug_report.md` 的小标题重排过；
   ★ 上游近 30 个提交里 `Signed-off-by` **零出现**，但保留它与前两份投稿一致且无害。

★ **三份各自开 issue、各自开 PR，不要合并**：一条是纯版本号升级、一条是行为修复、
一条是清单 feature 修正，立论与验证方式完全不同，合起来只会让几边都更难审。

### ★ 投稿二比投稿一更有说服力，理由值得记

投稿一是「版本号升级 + 引用一条公告」；投稿二带着**可复现的缺陷、实测数据、以及一个
能分别抓住两处缺陷的回归测试**。G32 当初定「只推 lru 一条」的顾虑是「纯版本号的改动
一起推容易把有说服力的那条拖黄」——投稿二不属于那一类，所以 G38 单独推它。

## ★ 投稿二的验证（2026-08-14，容器内，基于上游 main `0046038`）

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo check --workspace`（1.97.1）| ✅ |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ MSRV 不受影响 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ |
| `cargo test -p pingora-core --lib --no-fail-fast` | 544 passed / 2 failed —— **与未改动的 main 逐项相同**（`connectors::l4` 那两条，环境性），双向差集为空 |
| `cargo audit` | 不涉及（没动任何依赖）|
| `cargo machete` | 未跑（本地没装）；本改动不动任何清单 |

★ ★ **新测试的反证已分别做过**（一个只见过绿的测试与一个没在测的测试无法区分）：

| 撤掉哪一半 | 测试报什么 |
|---|---|
| `MSG_CMSG_CLOEXEC` | `fd received over SCM_RIGHTS is missing FD_CLOEXEC` |
| `OwnedFd` 那处接管 | `the accepted transfer socket was left open` |

两边**分别**红，说明它确实在测两件事，而不是碰巧一起绿。

## ★ 先查上游做没做，再动手

投稿二动手前核对过（2026-08-14）：上游 `main` 的同一函数**仍是 `MsgFlags::empty()`**，
accept 出来的连接**仍然没人关**，且仓库里**没有任何 issue/PR 提过 CLOEXEC**。
★ 这一步是 G32 留下的纪律——fork 里的 `prometheus` 与 `nix` 两条就是没查上游而白改的。

## ★ 投稿一：发之前查出来的三件事（2026-08-14 重审）

**① 「失败集合逐项相同」是错的。** 08-13 记的是 116/116，重跑实测 **116 vs 117**。
多出来的 `tests::test_eviction` 经两条独立证据判定与 lru 无关：

- **基线也抖**：单独重复跑那个测试二进制 20 次，基线红 1 次、打补丁红 1 次，**同一比率**；
- **结构上够不到**：`pingora-memory-cache` 不依赖 `lru`，它走 `TinyUFO`，
  而 `TinyUFO` 的 `src/` 一处都没用 `lru`——`lru` 是它的 **dev-dependency**，只给 benchmark 用。

★ 两条证据缺一不可：只有「基线也抖」会被质疑成巧合，只有「够不到」会被质疑成没跑过。

**② caveat 里有一处推理是错的（结论碰巧对）。** 原文用
`cargo tree -i lru@0.16.4 --workspace` 无匹配来论证「不在默认 feature 图里」——
而**普通解析下压根没有 0.16.4**（是 0.12.5），所以那条命令是因为**错的原因**才无匹配。
改成控住变量的 2×2 实测表，并写明真正的机制：`aws-sdk-s3` 是 `pingora-runtime` 的**可选**依赖，
只由非默认的 `dial9-worker-s3` feature 开启。

**③ 标题向上游惯例靠拢。** 上游那些 `RUSTSEC-…:` 开头的 issue **全是 `github-actions[bot]` 开的**
（带 `dependencies` 标签）；人写的依赖 issue 是「Update X …」那种形状（如 #875）。
★ 顺带查实：47 条 `dependencies` issue 里**没有** lru 这一条，机器人三个月来一次都没为它开过——
这条写进了 issue 正文，解释为什么由人来报。

## ★ 投稿二收到的复审，以及由它牵出的两条

发出后 GitHub 的 Copilot 复审留了一条行内意见：**新加的回归测试自己没有关掉收到的 fd**。

**采纳了。** 理由不是「reviewer 说了」，而是：上游同一文件里另外两个测试
（`test_send_receive_fds` / `test_serde_via_socket`）确实也不关——但**那不构成在一个
「主题就是 fd 泄漏」的 PR 里继续不关的理由**。改动已 amend 进原提交（`f94e445`），
CI 四项仍全绿。

### ★ ★ 差点把一条有效意见驳回去

采纳后 `close_unclaimed_tests::closes_only_unclaimed_fds` 红了一次，第一反应是「这条建议引入了
flakiness」。**5 次样本就下结论是错的**，做了三配置对照（每档 20 次）：

| 配置 | 该测试失败次数 |
|---|---|
| 上游未改动 | 5/20 |
| 本 PR（未采纳建议） | 2/20 |
| 本 PR（已采纳建议） | 5/20 |
| ★ **全新 pristine 上游克隆** | **7/20** |

**它在完全未改动的上游上就已经这样。** → 已单独报为
[#963](https://github.com/cloudflare/pingora/issues/963)（按上游 `bug_report.md` 模板写，
数据取自 pristine 克隆而非我打过补丁的树）。

根因：那个测试关掉 fd 之后断言这个**号**已失效（`fcntl(fd, F_GETFD) == -1` / `EBADF`），
而 fd 号是进程级、关掉即被复用——同一二进制里任何并行测试在那个窗口开一个 fd 就会拿走它。

### ★ 同一轮里还纠正了投稿一的一句话

投稿一（`lru`）正文原本写「那条 flaky 的 `test_eviction` 我没查，happy to file it separately」。
一搜才发现**上游早有 [#591](https://github.com/cloudflare/pingora/issues/591) 报过、
[#740](https://github.com/cloudflare/pingora/pull/740) 提了修复**（用 `force_put` 绕过
TinyLFU 准入）。已改成引用这两条。★ **「我可以帮你报」在别人早就报过时，等于宣告自己没搜。**

## ★ 顺序不能反

上游 `CONTRIBUTING.md` 写着 **"Non-trivial PRs will also require a GitHub issue"**，而依赖大版本升级不在它列举的
trivial 清单（错别字／小重构／文档）里。**先发 issue，等回应，再发 PR 并引用 issue 号。**

## 怎么用

```bash
# 1. 在 GitHub 上 fork cloudflare/pingora，然后
git clone git@github.com:<你的账号>/pingora.git && cd pingora
git remote add upstream https://github.com/cloudflare/pingora.git
git fetch upstream main && git checkout -b lru-0.18.2 upstream/main

# 2. 应用补丁（两份投稿各自一条分支，别混在一起）
git am /path/to/Fulcrum/upstream-pr/0001-Bump-lru-dependency-from-0.16.3-to-0.18.2.patch
#   或
git am /path/to/Fulcrum/upstream-pr/0002-Close-the-transfer-socket-and-set-CLOEXEC-on-received-listener-fds.patch

# 3. 先发 issue（issue.md 的内容），拿到编号后再 push + 开 PR（pr.md 的内容）
git push origin lru-0.18.2
```

## ★ 已经跑过的验证（2026-08-13，容器内，基于上游 main `0046038`）

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo check --workspace`（1.97.1） | ✅ 零源码改动 |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ |
| `cargo machete` | ✅ |
| `cargo test --lib --bins --tests --no-fail-fast` | 116 失败，**与未改动的 main 逐项相同**（缺 openresty，双向差集为空）|
| `cargo audit` | ★ **不变**——见下 |

## ★ ★ 两条必须诚实写进投稿的事

1. **这一行不会让 `cargo audit` 那条消失。** `aws-sdk-s3` 独立要求受影响版本的 `lru`（`--ignore-rust-version` 解析下是
   0.16.4，普通解析下是 0.12.5）。它不在默认 feature 图里，但在 lock 里，而 audit 扫的是 lock。**这条不归上游这个仓库管。**
   改动的真实价值是「**pingora 自己的五个 crate 离开了受影响版本**」，不是「修好了公告」。

2. **上游的 CI 现在不是红的。** RustSec 把这条归为 `unsound`（warning）而非 vulnerability，裸 `cargo audit` 不因 warning 失败。
   ★ **不要把立论写成「你们 CI 红了」——那是假的，写进去会当场失去可信度。**

## ★ 一条实验设计的教训

第一轮跑门时 MSRV 1.85.0 那档报红（`s2n-tls requires rustc 1.91`），我一度以为是 `lru` 引起的回归。
做 2×2 对称实验后确认：**红完全由锁文件生成方式决定，与 `lru` 无关**——我拿 `cargo audit` 步骤用的
`--ignore-rust-version` 锁文件去跑 `cargo check`，而上游 CI 的 MSRV 档**不预先生成 lock**。

| | 普通 lock | `--ignore-rust-version` |
|---|---|---|
| 基线 | ✅ rc=0 | ✗ rc=101 |
| lru 0.18.2 | ✅ **rc=0** | ✗ rc=101 |

★ **对照实验要把变量控住**，否则会把自己的实验污染当成被测对象的缺陷报上去。

## ✅ 投稿四（2026-08-20 **已发出**）· `test_connect_uds` 的短读

> ✅ issue [cloudflare/pingora#967](https://github.com/cloudflare/pingora/issues/967)
> → PR [cloudflare/pingora#968](https://github.com/cloudflare/pingora/pull/968)

- 立论：[`issue-4-short-read.md`](issue-4-short-read.md)
- 补丁：[`0004-Use-read_exact-in-test_connect_uds.patch`](0004-Use-read_exact-in-test_connect_uds.patch)
  （★ 2026-08-20 **重新生成**：原先那份不是 `git format-patch` 的输出——没有 `From` 头、
  没有 `Signed-off-by`、hunk 头只有一个裸 `@@`，**`git am` 根本吃不下**。
  现已在 `0046038` 上回放验证过，且与分支树逐字节一致。）
- fork 侧已经改掉了（见 [`../vendor/pingora/FORK.md`](../vendor/pingora/FORK.md) §7）

### ★ ★ 上游 CI：三绿一红，而**那一红与本改动无关**（已在 PR 里说明）

| 检查 | 结果 |
|---|---|
| `pingora (1.85.0)` | ✅ 3m50s |
| `pingora (nightly)` | ✅ 8m39s |
| `semgrep-oss` | ✅ 22s |
| `pingora (1.97.1)` | ❌ 13m21s —— **`cargo audit` 那一步** |

红的原因是 **RUSTSEC-2026-0258**（`h2` 0.3.27，"unbounded empty DATA frames"，
**2026-08-17 公布**，评级是 vulnerability 而非 warning，所以 `cargo audit` 退出非零）。

★ ★ **判它「与本改动无关」不能靠推理，要拿两条独立证据**：

1. **上游 `main` 最后一次 CI 是 2026-08-07**（`0046038`），**早于公告发布日**。
   `cargo audit` 是**跑的时候**去拉公告库的 ⇒ **那次绿说明不了今天**。
2. **投稿三的 PR [#966](https://github.com/cloudflare/pingora/pull/966)**（比本份早开、
   只动 `pingora-rustls/Cargo.toml`）**在同一个 job、同一个公告 ID 上同样红**。

而本改动只动一行**测试代码**、不碰任何清单或锁文件，结构上够不到那一步。
⚠ 已在 PR 里留言把这三点说清楚——**一个红勾摆在那里没人解释，审阅者第一反应就是「你把 CI 搞红了」**。

★ **Copilot 的自动复审（2026-08-20 01:52，`COMMENTED`）：无可执行意见**——
只给了一段准确的改动摘要，**零行内评论、未要求改动**。
⚠ 与投稿二那次不同（那次它留了一条「新测试自己没关掉收到的 fd」，我们**采纳了**），
这一次没有要处置的东西。记在这里是为了**下一个人不必再去点开看一遍**。

> ★ ★ 顺带一条给我们自己的：**`cargo audit` 这类「跑的时候才去拉数据库」的门，
> 它的绿是有保质期的**。上游 `main` 今天看着是绿的，只是因为它从 08-07 起就没再跑过。
> 同一件事在本仓库对应的是 `dep-check.py` 的第 160 档——它每次都真去查，所以不会有这种假绿。

★ **它与前三份的证据形状不同，值得单独说**：前三份是「实测到一个泄漏／一个多编的 crate」，
这一份是**一条间歇性失败**。而间歇性失败的证据不能是「又跑了几次没见到」——
所以立论里给的是**一个确定性的扰动**：把 mock server 的一次 `write_all` 拆成
「1 字节 + 20ms + 8 字节」（流式 socket 的合法行为），**修前 3/3 必红、修后 5/5 必绿**。

### ★ ★ ★ 发之前复审推翻了草稿里的一句话

草稿把它写成「一条会间歇性打红构建的 flaky 测试」。**实测下来那句话是错的**：
这个测试整块在 `#[cfg(feature = "any_tls")]` 里，而 `pingora-core` 与 `pingora` 的
`default` **都是 `[]`** ⇒ 上游 CI 跑的 `cargo test --workspace --lib --bins --tests`
**根本不编译它**（实测 `-- --list | grep -c test_connect_uds` → **0**）。
★ 所以立论改成「任何带 TLS backend 跑测试的人会撞上」。
⚠ 与投稿一那条教训同形：**把立论写成一件审阅者一查就知道是假的事，会当场失去可信度。**

### ★ ★ 「失败集合前后相同」第一次跑出来是**不相同**的

基线 7 条、打补丁后 6 条，差的是 `test_conn_timeout_with_offload`。
**没有把它解释成「环境性的」了事，而是把环境修对**：容器里没装
`iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`，于是那批拿 RFC 5737 地址当黑洞的
连接超时测试被 Docker 的网络替它应答而恒红且抖。装上之后两侧都是 **572/1/2**，差集为空。
> ★ 这正是 G41/G42 当初查过的同一件事——**结论没被写进跑上游门的流程里，于是又踩了一遍**。
> 已补进下面的「怎么用」。

## ⏳ 投稿六（**材料准备中，⛔ 未发**）· 监听器上的连接计数接缝

对应 fork **改动 15**。依据 G122：「投不投**等 rebase 读过上游 `main` 之后再判**」。
立论与已查清的部分：[`issue-6-connection-counter.md`](issue-6-connection-counter.md)。

⚠ ⚠ **本轮查出两件把这份材料的形状改掉的事**（2026-09-03，克隆实测，`main` = `09696b5`）：

1. ★★★ **G122 里那句「上游 `main` 已把 `prometheus` 整条删掉」在今天的 `main` 上不成立** ——
   `pingora-prometheus/src/lib.rs` 还在（131 行），`pingora` 与 `pingora-proxy` 都依赖它。
   ⚠ 而那句话是「投不投」这个判断的**全部理由**（「口味未知」）⇒ **已登记给 owner**，
   ⛔ 我没有替它改结论。
2. ★★★ **上游已经有半个同位置的接缝**：`ConnectionFilter::should_accept`
   （`listeners/l4.rs:472`，每条连接一次，TCP accept 之后、TLS 握手之前）——
   **`+1` 的位置逐字相同**，⛔ 而「连接结束」与「哪个监听地址」两样它都给不出，
   偏偏那两样正是做连接计数必须的。⇒ 立论形状从「加一个新接缝」变成「把已有的补完」。

⛔ **有意没有生成 `.patch`**：基线已从 `0046038` 移到 `09696b5`，而形状未定 ——
一份基线过期、形状可能作废的补丁摆在这里比没有更糟，下一个人会以为它可用。
⏳ **发之前还欠一项**：G46 要求的「有没有人已经开过 issue / PR（含 open 的）」本轮未查。

## ❌ 投稿五（**已撤销，不发**）· rustls 监听器用不上自定义证书解析器

> owner 2026-08-20 拍板 **「什么都不做」**：不开 issue、不开 PR、也不去别人的线程留言。

**理由不是立论错了，是这件事上游从 2025-04 就有人报了——同一主题已有 9 条 issue/PR。**
尤其 [#632](https://github.com/cloudflare/pingora/pull/632) **与我们的做法逐点相同、
而且更完整**（它顺手把 `build()` 的 panic 改成了 `Result`），已有 3 个独立使用者证实可用；
它卡住的原因是**合并冲突 + 维护者评审带宽**，不是设计分歧
（[#908](https://github.com/cloudflare/pingora/pull/908) 那条线从 6 月催到 8 月）。
⇒ 再开第 7 份只会让队列更堵。逐条证据见 [`issue-5-cert-resolver.md`](issue-5-cert-resolver.md)。

⚠ ★ **fork 里那条 `with_cert_resolver` 照旧留着，但归零条件变了**：
不再是「等我们的投稿被接受」，而是**等 #632 或 #908 落地**。
