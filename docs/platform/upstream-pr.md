---
type: 技术基线
title: 向上游 pingora 提 PR 的流程清单
description: 两份投稿（G32 的 lru 升级、G38 的 fd 泄漏修复）共用一套上游规范；本页把它逐条落成可核对项。
resource: ../../vendor/pingora/FORK.md
tags: [依赖, 上游, 流程, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-13T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G32（只推两条 + owner 本人提交）
  - id: contributing
    resource: https://github.com/cloudflare/pingora/blob/main/.github/CONTRIBUTING.md
    title: 上游 .github/CONTRIBUTING.md
  - id: build-yml
    resource: https://github.com/cloudflare/pingora/blob/main/.github/workflows/build.yml
    title: 上游 .github/workflows/build.yml（PR 必过的 CI 门）
---

★ **本页的每一条都来自实际读取上游仓库，不是惯例推测。** 采集日期 2026-08-13，采集方式见末节。

# 为什么只推两条（G32）

G32 拍板时写的是「推 `lru` 与 `prometheus` 两条」。★ ★ **实测之后修正为：只推 `lru` 一条。**

| | 上游 **main** 现值 | 结论 |
|---|---|---|
| `lru` | `0.16.3`（解析到 **0.16.4**，即 RUSTSEC-2026-0253 影响的版本）| ★ **推**，目标 `0.18.2` |
| `prometheus` | ★ ★ **已经是 `0.14`**，`protobuf` 已是 **3.7.2** | ★ **不用推——上游自己早做了**，只是尚未发版（`0.8.1` 是最后一个 release，main 已领先 167+ 提交）|

★ **这条要连带记住**：枢衡 fork 里的 `prometheus` 与 `nix` 两条改动，**下一次上游发版就会白捡消失**。rebase 时先查上游有没有自己做了，别闷头重做。

`brotli` / `rand` / `sfv` **不推**——纯版本号、无公告，`sfv` 那条还是枢衡自己迁的（上游 main 至今是 `0.10.4`），一起推容易把有说服力的那条拖黄。

# ★ 硬性流程（漏一条就等于没遵守）

## 1. ★ ★ 必须先开 issue，再开 PR

> CONTRIBUTING：**"More often than not, start by filing an issue on GitHub… Non-trivial PRs will also require a GitHub issue."**

它列的「不需要 issue 的小修」是：**改错别字、小重构、文档或注释编辑**。

★ **依赖的大版本升级 + 跨 crate 的调用点适配不在其中，属于 non-trivial** → **必须先开 issue**。直接开 PR 就是违反流程。

issue 的作用是先谈设计，避免「做完了才发现方向不被接受」。

## 2. PR 正文引用该 issue 号

## 3. ★ 不要期待自己的 PR 被直接 merge

上游的合并方式是**批量重放**，不是合你的分支：

```text
Needs Review → （可能）Changes Requested → Accepted
   → 维护者把你的 commit rebase 到 main 的**另一个批量 PR** 里（同步内部仓库）
   → 你的 PR 被 close
```

★ **「PR 被 close」在这里是成功而不是拒绝。** 判断是否落地要看改动有没有出现在 `main`，别看 PR 状态。

## 4. 时限：没有承诺

> "internal contributions will take priority… We can't promise we will review or address all PRs or issues in a timely manner."

★ **所以 G32 的「其余常年 rebase」不是备选而是必须**——fork 的维护不能建立在「PR 会被及时接受」这个假设上。

## 5. 行为准则

适用 [Contributor Covenant Code of Conduct]（Cloudflare 组织级）。联系邮箱 `opensource@cloudflare.com`。

[Contributor Covenant Code of Conduct]: https://github.com/cloudflare/.github/blob/26b37ca2ba7ab3d91050ead9f2c0e30674d3b91e/CODE_OF_CONDUCT.md

## 6. DCO / CLA

★ **仓库内没有任何 DCO 或 CLA 的 workflow 与文件**（`.github/` 下只有 CONTRIBUTING、两个 issue 模板、五个 workflow；**没有 PR 模板**）。

★ 但机器人可能不体现在仓库里。**保险做法是提交时就带 `Signed-off-by:`**——多写无害，缺了要返工。

# ★ CI 门（`build.yml`，每个 PR 必过）

toolchain 矩阵：**`nightly` / `1.85.0`（MSRV）/ `1.97.1`（latest stable）**，`fail-fast: false`。

| 步骤 | 命令 | 跑在哪些 toolchain |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | 全部 |
| 检查 | `cargo check --workspace`（`1.85.0` 时加 `--exclude pingora-foundations`）| 全部 |
| 测试 | `cargo test --verbose --lib --bins --tests --no-fail-fast` | 除 `1.85.0` |
| 文档测试 | `cargo test --verbose --doc` | 除 `1.85.0` |
| clippy | `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | **仅 `1.97.1`** |
| 审计 | `cargo audit` | **仅 `1.97.1`** |
| 未用依赖 | `cargo machete`（0.7.0）| **仅 `1.97.1`** |

★ 测试环境装了 **openresty**（当作测试用的服务端），本地复跑要一并准备。

## ★ ★ MSRV 是硬底线，且这次正好卡住

上游 MSRV = **1.85.0**。已逐个核过候选版本的 `rust-version`：

| crate | 目标版本 | 声明的 MSRV | 结论 |
|---|---|---|---|
| `lru` | **0.18.2** | **1.85.0** | ★ **正好等于上游底线，不需要抬 MSRV** |
| `prometheus` | 0.14.0 | 1.81 | ✅ 低于底线 |
| `protobuf` | 3.7.2 | 未声明 | ✅ 无约束 |

★ **`lru` 0.17.0 起 MSRV 就是 1.85.0**（0.16.x 是 1.70.0）。这是整个 PR 可行性的前提——**若上游哪天把 MSRV 降回 1.85 以下，这条 PR 立刻不成立**。

★ 上游 `.cargo/config.toml` 设了 `[resolver] incompatible-rust-versions = "fallback"`（MSRV 感知解析）。

## ★ ★ 他们的 audit 忽略名单里**没有**这两条

`.cargo/audit.toml` 只忽略三条，全是 `rustls-webpki` 经 `aws-sdk-s3-transfer-manager` 引入的：`RUSTSEC-2026-0098` / `RUSTSEC-2026-0099` / `RUSTSEC-2026-0104`。

`RUSTSEC-2026-0253`（lru）**不在**忽略名单里。

## ★ ★ ★ 但实跑结果推翻了「所以他们 CI 是红的」这个推断

在 main（`0046038`）上实跑 `cargo audit`：

```text
Scanning Cargo.lock for vulnerabilities (677 crate dependencies)
warning: 4 allowed warnings found
  derivative 2.2.0     unmaintained  RUSTSEC-2024-0388
  paste 1.0.15         unmaintained  RUSTSEC-2024-0436
  rustls-pemfile 2.2.0 unmaintained  RUSTSEC-2025-0134
  lru 0.16.4           unsound       RUSTSEC-2026-0253
```

★ **退出码 0——它是绿的。** 原因：RustSec 把这条归类为 **`unsound`（warning）而不是 vulnerability**，而 `build.yml` 跑的是**裸 `cargo audit`**，warning 不会让它失败（除非加 `--deny warnings`）。

★ ★ **所以 PR 的立论不能写成「你们的 CI 红了」——那是假的，写进去会当场失去可信度。** 正确的立论是：

> `cargo audit` 在 main 上报告 `lru 0.16.4` 的 RUSTSEC-2026-0253（unsound，`LruCache::pop()` 缺乏 panic 安全导致潜在 UAF）；`lru 0.18.2` 已修复，且其 MSRV 正好是 1.85.0，与本仓库的 MSRV 底线一致，无需抬 MSRV。

★ **这是本页最该记住的一条方法论**：**先跑，再写立论**。基于「忽略名单里没有它」推出「CI 必红」在这里是错的。

# ★ 范围提醒：上游的改面比枢衡 fork 大得多

`lru` 在上游是 `[workspace.dependencies]` 里的一行，被 **五个 crate** 引用：

```text
pingora-cache   lru = { workspace = true }
pingora-core    lru = { workspace = true, optional = true }   ← 仅 s2n 后端
pingora-lru     lru = { workspace = true }
pingora-pool    lru = { workspace = true }
tinyufo         lru = { workspace = true }
```

★ ★ **枢衡的 fork 只 vendor 了 `pingora-core` 与 `pingora-pool`，而且 `pingora-core` 里用 `lru` 的那个文件在 s2n 后端下——rustls-only 构建根本不编译它。**

**结论：fork 里现成的改动不足以直接变成上游 PR。** 上游 PR 必须覆盖 `pingora-cache` / `pingora-lru` / `tinyufo` 以及 **s2n 后端**的调用点，并过 `cargo check --workspace`。

# 提交信息惯例

上游 main 近期提交（无 conventional-commits 强制）：

```text
ci: bump latest-stable toolchain from 1.91.1 to 1.97.1
Bump h2 dependency from 0.4.11 to 0.4.14      ← ★ 同类先例
Close unclaimed inherited listening sockets on graceful upgrade
compression: add can_decompress and will_decompress predicates
```

**句首大写的祈使句**，可选 `ci:` / `compression:` 这类领域前缀。

★ **`Bump h2 dependency from 0.4.11 to 0.4.14` 是最好的模板**——同类改动已经被接受过，照它的形状写：

```text
Bump lru dependency from 0.16.3 to 0.18.2
```

# 谁来提交

⚠ **口径2026-08-14 变更（G40）**：owner 在自己登录的前提下**授权代发**。

| | 原口径（G32 附加要求）| 现口径（G40）|
|---|---|---|
| 谁点「提交」| ★ owner 本人 | ★ **owner 授权后可代发** |
| 用谁的身份 | owner 的 GitHub 账号 | **不变**——仍是 owner 的账号与 `Signed-off-by` |
| 发之前 | 复核并优化草稿 | **不变**，且**每次都要重查上游是否已修/已有人报** |

★ **没变的那一条才是要紧的**：对外发布用的始终是 owner 的身份，所以
**「发之前把每条立论都跑一遍」这条纪律不因为谁点按钮而放松**。

# ✅ 材料已备好（2026-08-13）

投稿材料在 [`upstream-pr/`](../../upstream-pr/README.md)：issue 草稿、PR 正文草稿、可 `git am` 的补丁（**一行**，已带 `Signed-off-by`）。

## 七个门的实测结果（上游 main `0046038` + 本补丁）

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo check --workspace`（1.97.1）| ✅ **零源码改动** |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | ✅ |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ |
| `cargo machete` | ✅ |
| `cargo test --lib --bins --tests --no-fail-fast` | 116 失败，**与未改动的 main 逐项相同**（缺 openresty；双向差集为空）|
| `cargo audit` | ★ **不变**，见下 |

## ★ ★ 两条必须写进投稿的诚实说明

1. **这一行不会让 `cargo audit` 那条消失。** 实测：改后 lock 里 `pingora-cache` / `pingora-core` / `pingora-lru` / `pingora-pool` / `TinyUFO` **全部是 0.18.2** ✅，但 **`aws-sdk-s3` 独立要求受影响版本的 `lru`**（`--ignore-rust-version` 下 0.16.4，普通解析下 0.12.5）。它不在默认 feature 图里，但在 lock 里，而 audit 扫的是 lock。**改动的真实价值是「pingora 自己的五个 crate 离开了受影响版本」，不是「修好了公告」。**

2. **不要写「你们 CI 红了」**——上文已实测那是假的。

## ★ 一条实验设计的教训

第一轮跑门时 MSRV 1.85.0 报红（`s2n-tls requires rustc 1.91`），一度被当成 `lru` 引起的回归。2×2 对称实验后确认：**红完全由锁文件生成方式决定，与 `lru` 无关**——是拿 `cargo audit` 步骤的 `--ignore-rust-version` 锁文件去跑 `cargo check` 造成的，而上游 CI 的 MSRV 档**不预先生成 lock**。

| | 普通 lock | `--ignore-rust-version` |
|---|---|---|
| 基线 | ✅ rc=0 | ✗ rc=101 |
| lru 0.18.2 | ✅ **rc=0** | ✗ rc=101 |

★ **对照实验要把变量控住**，否则会把自己的实验污染当成被测对象的缺陷报上去。

# 怎么重新采集本页的事实

```bash
git clone --depth 1 --branch 0.8.1 https://github.com/cloudflare/pingora /tmp/pingora
cd /tmp/pingora && git fetch --depth 200 origin main
git show FETCH_HEAD:.github/CONTRIBUTING.md
git show FETCH_HEAD:.github/workflows/build.yml
git show FETCH_HEAD:.cargo/audit.toml
git log --format=%s FETCH_HEAD -12

# 立论要用实跑结果，不要只凭忽略名单推断
git checkout -B prmain FETCH_HEAD
cargo install --locked cargo-audit
cargo generate-lockfile --ignore-rust-version && cargo audit
```

MSRV 用 crates.io API 查 `rust_version`：`https://crates.io/api/v1/crates/<name>`。

# ★ 第二份投稿：`get_fds_from()` 的两处 fd 泄漏（G38，2026-08-14）

本页上面那套流程约束**逐条同样适用**（先开 issue、引用 issue 号、别指望直接 merge、
带 `Signed-off-by`、由 owner 本人提交）。这里只记与投稿一不同的地方。

**它是行为修复，不是版本号升级**，所以立论方式不同：

| | 投稿一（`lru`）| 投稿二（fd 泄漏）|
|---|---|---|
| 立论 | 引用一条安全公告 | ★ **可复现的缺陷 + 实测数据 + 回归测试** |
| 源码改动 | 零 | 两处，都在 `get_fds_from()` 内 |
| 测试 | 无（不需要）| ★ 新增一个，**分别**能抓住两处缺陷 |
| MSRV | 卡在 1.85.0（`lru` 的底线正好等于上游）| 不涉及，实跑 `+1.85.0 check` 通过 |

★ ★ **动手前先查了上游做没做**（G32 留下的纪律，`prometheus`/`nix` 两条就是没查而白改的）：
上游 `main` 的同一函数仍是 `MsgFlags::empty()`、accept 的连接仍然没人关，
且仓库里**没有任何 issue/PR 提过 CLOEXEC**。

★ **PR 正文里主动写了一段「范围说明」**：同一函数里 `SOCK_CLOEXEC` / `accept4()` 那两处
也缺，但它们的 fd 在函数返回前就关掉了，只暴露一个很窄的 fork 竞态窗口，
故**有意不动**并说明理由。★ 审阅者一定会问「为什么只修这两处」——
**与其等他问，不如先答**；而答案必须是真实的取舍，不是遗漏。

材料与实测结果见 [`upstream-pr/README.md`](../../upstream-pr/README.md)。

# 相关

[fork 说明](../../vendor/pingora/FORK.md) · [依赖策略](/platform/dependency-policy.md) · [供应链现状](/platform/supply-chain.md)
