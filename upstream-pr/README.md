# upstream-pr —— 给 cloudflare/pingora 的投稿材料

> 依据 [`PLAN.md`](../PLAN.md) §10 **G32**。流程约束见 [`docs/platform/upstream-pr.md`](../docs/platform/upstream-pr.md)。

★ **这个目录是过渡性的**，和回落层一样——**改动被上游接受（或明确拒绝）之后就删掉**。

## 里面是三份投稿，彼此独立

**投稿一 · `lru` 版本升级**（G32，2026-08-14 **已发出**，✅ **已落地**）

> ✅ ✅ **改动已经在上游 `main` 里**（2026-09-04 实测，这才是本目录自己的落地判据）：
> [`6463ad6`](https://github.com/cloudflare/pingora/commit/6463ad6)，2026-08-14，
> author `Shanire <shanire86@gmail.com>`、committer 是维护者、`Signed-off-by` 原样保留。
> ⇒ ★ 「我们投的东西会不会被重放进 `main`」有了**第一个正例**。
> ⏳ 按本目录开头那条纪律（「被接受或明确拒绝之后就删掉」），**这一份材料可以清了** ——
> ⛔ 留给 owner 决定，我没删。
> ⚠ 而投稿二与投稿三**尚未落地**：它们的主题在 `main` 的提交里零命中
> （带正对照：同一条检索搜得到上面那笔 lru）。
>
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

## ✅ 投稿六（**issue 与 PR 都已发，2026-09-04**）· 监听器上的连接生命周期

> ✅ PR [cloudflare/pingora#995](https://github.com/cloudflare/pingora/pull/995)
> （base `main` · head `ShanireZ:listener-tracer` · 一个文件 `+135/-1` · 一个提交 `0dda18b`）。
> 正文草稿留档在 [`pr-6-listener-tracer.md`](pr-6-listener-tracer.md)，发后逐行核过。
>
> ✅ **上游 CI 四项全绿**：`pingora (1.85.0)` 3m39s · `(1.97.1)` 10m58s · `(nightly)` 8m7s ·
> `semgrep-oss` 33s。★★★ 它补上了本地验证的两处缺口 —— **MSRV** 那道本地没有工具链，
> 而 `1.97.1` 跑的是**带 openresty 的完整套件**（本地那 99 条集成失败是环境造成的，就此坐实）。
> ⏳ 等回话。★ 判断是否落地看**改动有没有出现在 `main`**，⛔ 不看 PR 状态。

> ✅ issue [cloudflare/pingora#994](https://github.com/cloudflare/pingora/issues/994)
> （经 owner 逐次授权，G40）。发后核过：归一行尾后与批准的那份**逐字符相等**。
> ⚠ ⚠ ⚠ **发出去之后自己查出一处错，已公开更正**（owner 单独批准）：
> [issue-comment-5536326108](https://github.com/cloudflare/pingora/issues/994#issuecomment-5536326108)。
> `Stream` 上本来就有 `pub tracer`，且 `Stream::drop` 会调 `on_disconnected()` ——
> **机制是通的，缺的只是监听器那侧没人赋值**。⇒ 要求因此**变小**：不要新方法，
> 只要监听器侧能挂 `Tracer`。全文见材料开头那一节。
>
> ✅ **补丁已备好、⛔ 未发**：
> [`0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch`](0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch)
> —— 基于 `09696b5`，只改一个文件，`fmt` / `clippy -D warnings` 均 `RC=0`，
> `git am` 回放干净且树逐字节一致，**失败集合与基线逐项相同**（多出两条通过的新测试）。
> ⛔ 它是在**干净的上游克隆**上做的，`vendor/pingora/` 一个字节没动。验证表见材料 §2.2。

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

✅ **形状已定（owner 2026-09-04 拍板：形状 ②）** —— 给已有的 `ConnectionFilter` 补上
「连接结束」钩子与「监听地址」，⛔ **不提新接缝**（提了就正面撞上上游 2026-08-25
那句「#671 已经加了连接级过滤器」）。**issue 正文草稿在
[`issue-6-connection-counter.md`](issue-6-connection-counter.md) §5 与其后的「正文」一节**，
⛔ **未发** —— 发要 owner 按 G40 单独授权。

⛔ **仍然没有生成 `.patch`**：基线已从 `0046038` 移到 `09696b5`，而本目录的纪律是
**先发 issue、等回应、再发 PR** —— 补丁的形状取决于维护者对「挂在 `ConnectionFilter` 上
还是别处」的回话。一份基线过期、形状可能作废的补丁摆在这里比没有更糟。
✅ **G46 那一项已补上（2026-09-04）**，结果在 [`issue-6-connection-counter.md`](issue-6-connection-counter.md) §3.1：
⛔ **不构成否决**（没有 open 的 issue/PR 在做这件事本身，与投稿五那次的 #632 完全不同），
★★★ **但立论必须改写** —— 上游 2026-08-25 刚以「#671 已加连接级过滤器」为由把
[#118](https://github.com/cloudflare/pingora/issues/118)（「on connect 阶段」）判成 `COMPLETED`。
★ 同时需求侧的证据变强了：#118 / #295 / #337 三个互不相干的使用者两年里各要过一次，三条都没拿到实现。
⚠ ⚠ 方法上踩到一条：**`gh search issues` 隐含 `is:issue`，一条 PR 都不返回** ——
只跑那一条通道会答出「没人做过」而其实一个 PR 都没看过。

## ⏳ 投稿七（材料已备，⏳ 未发，owner 2026-10-08 拍「先备材料、暂不发」）· `read_request` 每请求白做的两件事

> 对应 fork **改动 16**（PLAN **G157**）的 ② ③（C1 / C2）。① （B2，l4 流不再记等待计时）**不投** ——
> 上游有读者，在上游不是语义不变，见下「B2 为什么不投」。
> ⛔ 没开 issue、没开 PR、没 push、没 fork，GitHub 上零写入（查重只读、没评论）。
> 补丁做在 scratchpad 里的**干净上游克隆**上，`vendor/pingora/` 一个字节没动。
>
> 上游 `main` = **`4487f7b`**（committer date 2026-09-11，「Abort tls offload tasks when dropped」）。
> 三处代码在它上面与 fork 改动前（`0c232f7^`）**逐字节相同**（`diff --strip-trailing-cr`，三个文件皆空），
> `702f690..4487f7b` 没有提交碰过 `l4/stream.rs` / `v1/server.rs` / `v1/common.rs` —— 读的是克隆下来的代码，不是网页。

| 文件 | 用途 |
|---|---|
| [`issue-7-read-request.md`](issue-7-read-request.md) | issue 草稿（`feature_request.md` 模板的四个小标题）。★ **必须先发它** |
| [`pr-7-read-request.md`](pr-7-read-request.md) | PR 正文草稿；发前换 `#<ISSUE_NUMBER>` |
| [`0007-Parse-request-headers-into-uninitialized-header-slots.patch`](0007-Parse-request-headers-into-uninitialized-header-slots.patch) | C1（`[PATCH 1/2]`）：改走 `parse_with_uninit_headers`；回归测试 `tests_stream::read_max_headers` |
| [`0008-Keep-request-header-refs-on-the-stack-instead-of-in-a-Vec.patch`](0008-Keep-request-header-refs-on-the-stack-instead-of-in-a-Vec.patch) | C2（`[PATCH 2/2]`）：`populate_header_refs` + 栈上槽位；两条单测 + `tests/read_request_allocations.rs` |

两份都是 `git format-patch` 原样输出：author 与 `Signed-off-by` 都是 `Shanire <shanire86@gmail.com>`，0 个 CR。

### 怎么切：一份 issue、一份 PR、两个提交（⛔ 没有「投稿八」）

- **合成一份**：C1 与 C2 在同一个函数里，立论相同（每条请求白做的功），验证形状也相同 ——
  与前几份「立论与验证方式完全不同 ⇒ 各自开」的情形相反。拆成两份 PR 反而会在同一函数上互相排队。
- **但分两个提交，且各自都能单独落地**：0008 不依赖 0007，单独 `git am` 到 `main` 上干净应用、门全过
  （下表「C1 单独 / C2 单独」两行）。上游走**批量重放、按提交取**，维护者可以只拿一个。
- **为什么不压成一个提交**：风险形状不同 —— C1 在 pingora 里**零 `unsafe`**（只是换 httparse 的入口），
  C2 新加**一个 `unsafe` 块**（把已写的前缀当 `&[KVRef]`）。维护者若对后者有意见，不该拖住前者。
- **为什么仍要先开 issue**：CONTRIBUTING 的免 issue 清单是错别字、小重构、文档；
  新加 `unsafe` + 新加一个带 `#[global_allocator]` 的测试二进制，不算 trivial。
- B2 不在里面（见下），所以没有第三个提交。

### ★ ★ B2 为什么不投：上游有读者（⛔ 没做删除补丁）

fork 侧「8 个 crate 里没有读者」**在 fork 里仍然成立**（枢衡不 vendor `pingora-proxy`），但上游 22 个 crate 里有：

| 位置（上游 `4487f7b`） | 是什么 |
|---|---|
| `pingora-core/src/protocols/digest.rs:235-244` | 公开 trait `GetTimingDigest` 的 `get_read_pending_time` / `get_write_pending_time`（缺省回 0）；`protocols/mod.rs:94` 把它并进 `IO` 约束 ⇒ 任何持有 `Stream`（`Box<dyn IO>`）的使用者都能调。自 **0.3.0** 起公开（`ea1db2f`，2024-05-21；CHANGELOG：「Add the API to track socket read and write pending time」）|
| `pingora-core/src/protocols/l4/stream.rs:677-683` | l4 `Stream` 的实现；TLS 包装层逐层转发：`tls/boringssl_openssl/client.rs:97-102` · `tls/rustls/stream.rs:118-123` · `tls/s2n/stream.rs:270-275` |
| ★ **`pingora-proxy/src/proxy_h1.rs:213`、`:242-244`** | **树内读者**：代理 H1 上游时，在 `proxy_1to1` 前后各读一次**上游连接**的 `get_write_pending_time()`，差值写进 `Session` |
| `pingora-proxy/src/lib.rs:619-620`、`:945-947` | 公开 getter `Session::upstream_write_pending_time()`；由 `900ec23`（2026-01-21，Cloudflare 的作者，「Add upstream_write_pending_time to Session for upload diagnostics」）加入 |

⇒ 在上游删掉计时 = 上面那个公开 getter 与 trait 方法**恒为 0**：是行为变更，⛔ 不能按「语义不变」投。

★ 顺带核清一处，影响取舍：上游 `AccumulatedDuration` **只在 Pending↔Ready 转换时读钟**
（`start()` 只在第一次 Pending 时读，`stop()` 只在随后 Ready 时读；立刻就绪的读写一次钟都不读）。
fork 侧量到的 `clock_gettime −2/请求` 与此一致（keep-alive 上每条请求的那次读先 Pending 一次）。
⚠ `vendor/pingora/FORK.md` 改动 16 ① 那格写「每次各读一两次时钟」，措辞偏宽 —— ⛔ 本次范围外，我没改，交 owner。

**可选做法（owner 定）**：

| 做法 | 改什么 | 取舍 |
|---|---|---|
| **A. 不投**（我的推荐）| 无 | fork 那 4 处删除照旧随 rebase 重放（守卫测试会在被合掉时变红）。上游 8 个月前刚为它加了读者，说明他们在用这份数据；省下的只是每次挂起→就绪的两次 vDSO 读钟 |
| B. 运行期开关，缺省开 | 例如 `Stream::set_track_pending_time(bool)`，关掉时不读钟、getter 回 0 | 对现有用户语义不变；但为很小的收益加公开 API。枢衡拿到的是 `ServerSession` 里的 `Box<dyn IO>`，要用还得在 accept 处接线（形状类似投稿六的监听器 `Tracer`）⇒ 改面不小 |
| C. cargo feature 关掉 | `no-pending-time` 之类 | ⛔ 不推荐：「关能力」的 feature 违反 feature 可加性 —— 同一依赖图里的 `pingora-proxy` 会静默拿到 0 |
| D. 只关读侧 | 读侧没有树内读者 | 仍是公开 trait 方法的行为变更，只省一半 |

若选 B：按本目录纪律**先开 issue 问要不要**，⛔ 别直接做补丁。

### G46 查重（2026-10-08，只读）

**方法**：`gh search issues` 与 `gh search prs` **两条通道**（不加 `--state` ⇒ 开着的与关了的都在），22 个查询串；
另把上游 **124 个 open PR 的改动文件**逐个过一遍（不靠关键词）；再在克隆里用 `git log -S` 查历史。

| 查什么 | 结果 |
|---|---|
| `parse_with_uninit_headers` · `uninit headers` · `uninit_headers` · `MaybeUninit header` · `populate_headers` · `KVRef` · `header_refs` · `read_pending_time` · `write_pending_time` · `pending time` · `AccumulatedDuration` · `upstream_write_pending_time` · `header allocation` · `read_request allocation` · `EMPTY_HEADER` · `httparse allocation` · `httparse performance` · `clock_gettime` · `Instant::now overhead` · `per-request allocation` | **两条通道全 0** |
| `MAX_HEADERS` | issue [#993](https://github.com/cloudflare/pingora/issues/993)（open）· PR [#1000](https://github.com/cloudflare/pingora/pull/1000)（open）· #83（无关，hyper 升级）|
| `read_request` | #993 / #1000；其余无关：#447（超时）、#844 / #969（shutdown Notify，已进 `main`）、#876（pipelining，已进 `main`）|
| 动同几个文件的 open PR | 6 个：#1000（下）· [#677](https://github.com/cloudflare/pingora/pull/677)（改 `for header in header_refs` 循环里那行注释）· #956 / #732（`once_cell`→`LazyLock`，别处）· #902（SNI，别处）· #971（l4 虚拟流池化，只与 B2 相关）|
| 上游历史 | 从未用过 `parse_with_uninit_headers`；`[EMPTY_HEADER; MAX_HEADERS]` 与 `populate_headers` 自 0.1.0 未动 ⇒ **没有「试过又退回」** |
| ★ 正对照（同一脚本同一通道）| `listener Tracer` / `connection lifetime` → PR **#995**；`CLOEXEC` → issue **#959**、PR **#960** |

⇒ **零命中「有人在做同一件事」**。⚠ 但 **#1000 改的正是 C1 那两行**（它要把头数上限做成可配：
`Request::new(&mut headers[..max_headers])`）。实测（`git merge-tree`，先扣掉 #1000 与 `main` 本来就有的
`apps/mod.rs` 冲突）：本投稿只多出**那两行**一处冲突，一行可解；**C2 与它不冲突**。
#1000 至今**没有维护者评审**（只有 issue 作者的 5 条 COMMENTED），最后活动 2026-09-25。已在 issue / PR 草稿里主动写明。
★ #677 那一处是设计时就避开的：C2 让 `header_refs` 变成 `&[KVRef]`，**循环那一行一字不改** ⇒ 实测不冲突。
⚠ 方法上又见一次：`connection lifetime` 在 issue 通道是 0、PR 通道才命中 —— **两条通道都要跑**。

### 验证（2026-10-08，容器内，⛔ Rust 不在宿主机跑）

**环境**：镜像 `upstream-g157-rust:1.97.1` = 官方 `rust:1.97.1@sha256:b1b3c9c0d921d7fa0a6d1f9ec7e4eab87f8c8ec97644c3d791450f131dec813f`
（Debian 13，rustc 1.97.1）+ 上游 `build.yml` 那组 `cmake libclang-dev` + `iptables` + `rustup toolchain install 1.85.0 --profile minimal`；
容器 `--cap-add NET_ADMIN` 后先 `iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`；`CARGO_BUILD_JOBS=4`；
宿主 Windows 11 + Docker Desktop 29.8.2。两侧共用**同一份 `Cargo.lock`**（cargo 1.97.1 生成，httparse 解析到 1.10.1）。
两侧共用一个 target 卷，所以**每个编译步骤先在容器里 touch `pingora-core`**，这些日志里都有 `Compiling`/`Checking pingora-core`。
「打补丁后」用的是**拿两份 `.patch` `git am` 出来的回放树**（与开发分支树逐字节一致）—— 测的就是要交出去的东西。

| 门（命令取自上游 `build.yml`） | `main` `4487f7b` | 打补丁后 |
|---|---|---|
| `git am` 0007 + 0008 | — | ✅ 干净；回放树 == 分支树；★ 0008 单独 `git am` 到 `main` 也干净 |
| `cargo fmt --all -- --check` | ✅ rc=0 | ✅ rc=0（⚠ 第一次 rc=1：我的测试里一行没按 rustfmt 折行，已改进 C1、重新生成补丁后重跑）|
| `cargo check --workspace` | ✅ rc=0 | ✅ rc=0 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ rc=0 | ✅ rc=0；warning 与基线逐行相同（只有 `pingora-foundations` 那条 clippy.toml MSRV 提示）|
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ rc=0 | ✅ rc=0（lock 由 cargo 1.85.0 现生成，与 1.97.1 那份逐字节相同）|
| `cargo test -p pingora-core --lib --tests --no-fail-fast` | 574 passed / 3 failed / 2 ignored | **579 / 3 / 2**；失败集合**双向差集为空**，多出的 5 条全是新测试，没有一条「改前过、改后不过」|
| 同上，**C1 单独** / **C2 单独** | — | 575 / 3 / 2 · 578 / 3 / 2；失败集合同上（clippy 也各自 rc=0）|
| `cargo test --doc -p pingora-core` | ✅ 1 passed / 10 ignored | ✅ 同 |
| `cargo test --verbose --lib --bins --tests --no-fail-fast`（CI 原命令，全 workspace）| 1155 / 129 / 6 | **1160 / 129 / 6**；失败集合**按测试名**比双向差集为空，多出的 5 条全是新测试。失败的 4 个 target 两侧相同：`pingora-core --lib`（上面那 3 条）· `pingora-load-balancing --lib`（`health_check::test::test_tcp_check`）· `pingora-proxy` 的 `test_basic`（18）/ `test_upstream`（107）—— 后两者要 openresty |
| `cargo check -p pingora-core --features patched_http1` | ✗ rc=101，唯一错误 E0599 `parse_unchecked` 不存在 | ✗ 同一个错误、只此一个 ⇒ 补丁里 patched 那条路的其余部分过了类型检查（借用检查没走到）|
| `cargo audit` / `cargo machete` / nightly 档 / openresty 集成测试 | 未跑 | 未跑（本地没装 audit/machete、没装 openresty）|

那 3 条两侧都红：`connectors::l4::tests::test_bind_to_port_range_on_connect`（老面孔，宿主机相关）与
`http::v2::server` 的 `test_req_conflicting_content_length_rejected`、`test_req_malformed_stream_budget_exhausted`
（这次新见，⛔ 没查根因；判它与本改动无关靠的是**基线同样红**，且它们走 h2、不经过 `v1::server::read_request`）。

⚠ 方法上踩到一条：容器的 stdout 与 stderr 经 `docker run` 分两路转出，cargo 的 `Running <binary>`（stderr）
会与测试输出（stdout）**错序** ⇒ 按「哪个二进制」归属测试名会张冠李戴（全 workspace 那次一度算出 3 条假的
「改前过、改后不过」）。★ 判据改为**只按测试名（多重集）比**，上表都是这样比的。

**探针读数**（一次性测试文件，⛔ 不在补丁里；两侧同一镜像、同一 lock）：

| | `main` | 打补丁后 |
|---|---|---|
| `read_request` 分配次数，3 个头（热身后连读 5 次）| **9**（5/5）| **8**（5/5）|
| 同，0 个头 | 4 | 4（空 `Vec` 本来就不分配 ⇒ 符合预期）|
| `size_of_val(&read_request())`（future 大小）| 216 | **216**（两个数组不跨 `.await`，没进 future）|
| `size_of::<httparse::Header>()` / `size_of::<KVRef>()` | 32 / 32 | — |

### 注入反证（都在副本 `pingora-inject` 上，每次都有 `Compiling pingora-core`；判据：与「打补丁后」的失败集合比）

| 注入 | 新增的红 |
|---|---|
| I1 · C1 给 httparse **少一个槽位** | 只红 `read_max_headers`（256 个头被 `TooManyHeaders` 拒）|
| I2 · C1 的代码**退回 `main` 原样**，测试留着 | **全绿** ⇒ 它守的是**等价**，不是优化本身。C1 省的是栈上的写，不是分配，没有测试能抓「被退回」—— PR 里照实写了 |
| I3 · C2 的调用处**退回 `Vec`**，测试留着 | 只红 `read_request_allocation_budget`：「made 9 allocations, more than 8」|
| I4 · C2 的栈上槽位**少一个** | 只红 `read_max_headers`（`index out of bounds: the len is 255 but the index is 255`）|
| I5 · `populate_header_refs` **不再跳过空名** | 只红那两条 helper 单测（`attempt to subtract with overflow`）|

### ★ 数字的口径

⛔ fork 侧诊断台的读数（用户指令 −4.7%、malloc −1 等）**一个都没进** issue / PR。进去的数都能在上游复现：
`8 KiB` = `256 × size_of::<httparse::Header>()`；分配 9 → 8 由补丁自带的测试给出（退回 C2 它就报 9）；
future 216 不变是探针读数，审阅者要自己写一行 `size_of_val` 才能复现 —— 所以 PR 里写的是「不变」并给了数。

★ **计时没进 PR，理由是实测太抖**：上游树内就有 criterion 基准 `pingora-core/benches/h1_pipelining.rs`
（`h1_single_request` 与 `h1_pipelining/bodyless/*` 正好走 `read_request`），在同一容器里做了
两侧**交替跑了 6 段、得到 3 组对比**（`--save-baseline` / `--baseline` 交替，每段都先 touch `pingora-core`）：

| 对比 | `bodyless/1` | `/16` | `/64` | `/256` | `/1024` | `h1_single_request` |
|---|---|---|---|---|---|---|
| 补丁 vs main（第 1 对）| −12.1% | −16.7% | −14.0% | −20.8% | −24.8% | −24.0%（CI −41.9%…−8.5%）|
| main vs 补丁（反向）| +12.1% | +25.7% | +24.6% | +19.5% | +15.3% | 无显著（p=0.82）|
| 补丁 vs main（第 2 对）| −5.2% | −20.3% | −24.5% | 噪声内 | ⚠ **+10.8%** | 无显著（p=0.18）|

方向大体一致，⚠ 但第 2 对的 `/1024` 反了号，`h1_single_request` 的置信区间宽到没意义；
这台宿主同时跑着别的容器（BetaPass 的 Playwright、WenTian 的 PG/Redis）。
⇒ ⛔ 这些百分比**不写进 issue / PR**；PR 里只提一句「树内这个 bench 走的就是这条路径，我机器太吵、没引数」，
把复现方法交给审阅者。

### 发前要重查

1. **上游 `main` 动没动**：`git fetch` 后重新 `git am` 两份补丁。若 `read_request` 一带有人改过，
   **重量分配次数**（`MAX_ALLOCATIONS = 8` 是对 `4487f7b` 量的，⛔ 别为了绿直接改数）并重跑整张门表，
   再改 PR 正文里的 574 / 579 等数。
2. **#1000**：若已合，先 rebase（那一行改成 `parse_req_buffer(&mut req, &buf, &mut headers[..max_headers])`）；
   `read_max_headers` 测的是缺省上限，仍成立。若还开着，草稿里那句「谁后落谁 rebase」不变。
3. **#677**：仍应不冲突；重跑 `git merge-tree` 确认。
4. **G46 重跑**：同一脚本（两条通道）+ open PR 文件维度扫一遍；带正对照。
5. 外部写入（都要 owner 按 G40 单独授权）：发 issue → 拿号 → 推 `ShanireZ/pingora` 分支 → 开 PR。
6. 若 owner 要 B2：走上表方案 B，**先开 issue 问**。

**留下的本地资源**（⛔ 没删，留给 owner 决定）：镜像 `rust:1.97.1`（拉取）、`upstream-g157-rust:1.97.1`（自建，约 3.9 GB）；
卷 `upstream-g157-cargo`（约 425 MB）/ `upstream-g157-target`（约 14 GB）/ `upstream-g157-target-msrv`（约 1 GB）；⛔ 没碰任何 `fulcrum-*` 卷。

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
