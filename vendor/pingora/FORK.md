# vendor/pingora —— 枢衡对 Pingora 的 fork

> 基线：`cloudflare/pingora` **tag `0.8.1`**（commit `719ef6c`，Apache-2.0）
> 建立：2026-08-12，依据 [`PLAN.md`](../../PLAN.md) §10 **G30**
> 上游：https://github.com/cloudflare/pingora

## 为什么有这个 fork

`pingora-core 0.8.1` 的清单里有十条版本上界，把 44 个传递依赖钉在旧版上，其中两条带真实安全公告。

★ **成因是 Cargo 的默认行为**：`brotli = "3"` 不是「≥3」，而是脱字号需求 `^3` = `>=3.0.0, <4.0.0`——**每一个不带符号的版本字符串都自带一个上界**。

★ ★ **`[patch.crates-io]` 打不破需求**——被 patch 的 crate 仍须满足依赖方的版本需求，所以没法直接把 `brotli` patch 成 8。**唯一的杠杆是替换 `pingora-core` 本身**，这就是这个 fork 存在的理由。

完整背景见 [`docs/platform/supply-chain.md`](../../docs/platform/supply-chain.md)。

## 范围

★ **只保留枢衡实际用到的 8 个 crate**，不是整个上游 workspace（21 个）：

```
pingora-core  pingora-error  pingora-http  pingora-pool
pingora-runtime  pingora-timeout  pingora-rustls  pingora-boringssl
```

⚠ ★ ★ **第 8 个是 2026-08-25 加回来的（G104），而它此前是被特意删掉的。**
删它的理由写在 `pingora-core/Cargo.toml` 里：「`PLAN.md` §5.1 第 1 条把 TLS 后端锁死在
rustls，三者永远不会被启用」——**而 §5.1 第 1 条已被 G104 推翻**（§10 第 59 轮，
本项目第一次推翻一条写着「不可回头」的约束）。详见下面的**改动 11**。
★ `pingora-openssl` 与 `pingora-s2n` 仍然没有 vendor，它们的 `#[cfg]` 分支照旧不会被点亮。

~~`pingora-rustls` 目前未被启用，先留着——G6 要求的 `features = ["rustls"]` 在 M1 会用到。~~
（M1 起它一直是被启用的那一条；⏳ G104 之后它会退到只服务 L4 的 ClientHello 预读，
最终去留见 `PLAN.md` §10 第 61 轮。）

⚠ ★ ★ ★ **2026-08-25（G104 第 ② 处）更新：`pingora-rustls` 已经退出根 workspace 的依赖图。**
L4 的 ClientHello 预读换到 BoringSSL 早回调之后，产品 crate 里**没有任何一个**再依赖它，
根 `Cargo.toml` 的 `[patch]` 与 `[workspace.dependencies]` 两处声明都已删掉
（实测：根 `Cargo.lock` 少了 `pingora-rustls` 与 `no_debug` 两个包）。
★ **但这个 crate 本身仍然留在 fork 里，仍是 vendor workspace 的成员，回归网照跑** ——
理由不是留恋：`pingora-core` 的 `rustls` feature 是**上游原样**的可选分支，
删掉这个 crate 就等于对上游多改一处，而**留着它一行改动都不用做**。
⇒ **「范围」仍然是 8 个 crate**，变的是「其中几个进产品依赖图」（现在是 7 个）。
> ★ 顺带一条方法论：**「没人用了」与「该删了」是两件事。**
> 前者说的是依赖图，后者说的是 rebase 成本 —— 而这里两者指向相反的方向。

## 改了什么

★ **改动的意图严格限定为「放宽版本上界 + 随之而来的调用点适配」。**

⚠ ★ ★ **但「没有行为变更」这句话，2026-08-12 建立本 fork 时写下来是错的**：`nix` 那一轮实际引入了一条真回归（见下面第 2 节），它在 2026-08-13 补上回归网的当天被逮到并修好。
**教训不是「当时不够小心」，是「当时没有能发现它的东西」**——编译过、M0 七跑全绿，而那条 bug 一直在。

### 1. 版本上界（`Cargo.toml` × 3）

| 依赖 | 上游 0.8.1 | fork | 动机 |
|---|---|---|---|
| `lru` | `0.16.3` | **`0.18.2`** | ★ **RUSTSEC-2026-0253**（UAF）。✅ ★ **上游 `main` 已于 2026-08-25 落地这一条 —— 那是我们的 [PR #962](https://github.com/cloudflare/pingora/pull/962)**；⚠ PR 本身显示 `closed / merged=false`（Cloudflare 从内部仓库导入再推，PR 就这么收场）——★ **那个状态读起来像被拒，而它不是**。⇒ 上游发版后本条可归零 |
| `prometheus` | `0.13` | **`0.14`** | ★ 连带把 `protobuf` 2.28 → 3.7.2，修 **RUSTSEC-2024-0437**（DoS）|
| `nix` | `~0.24.3` | **`0.31`** | ★ 波浪号连小版本都钉死；上游 main 已自行抬到 `~0.31.1` |
| `brotli` | `3` | **`8`** | 无公告，纯版本号；**无源码改动**，保留成本近似为零 |
| `rand` | `0.8` | **`0.10`** | 无公告，纯版本号；有源码改动（3 个文件），已被单元测试覆盖 |
| `sfv` | `0.10.4` | **`0.15`** | 无公告，纯版本号；⚠ 见「残余风险」 |
| `x509-parser` | `0.16.0` | **`0.18.1`** | ★ **2026-08-20 加**：`instant-acme`（ARI）要 `^0.18`，不抬就是**产物里两份 x509-parser**。**零调用点适配**（用到的只有 `FromDer` / `raw_serial_as_string` / `iter_organization*` / `nom::AsBytes`，跨版本未变）|

★ ★ **`x509-parser` 那条的判据值得单说：它不是「体积大了点」。**
`tests/vendor/run.sh` 的第 [2/5] 步会比对 vendor 锁与根锁的版本，对不上就**拒绝跑测试**，
理由是「vendor 测试跑的不是产物里那套组合，结果不可采信」——**那道门是对的**。
所以引入一份重复依赖的真实代价是**整个回归网停摆**，而不是几百 KB。
⚠ 它是 2026-08-19 那批 ACME 代码引入的，当时**没跑回归网**所以没人看见；
交接单里那句「没跑不等于绿」第二天就兑现了。

### ★ 2026-08-13：还原了四条纯装饰的上界

`strum` / `strum_macros`（→ `0.26.2`）、`daggy`（→ `0.8`）、`openssl-probe`（→ `0.1.6`）、`windows-sys`（→ `0.59.0`）**已还原为上游值**。

判据是 **保留成本 vs 收益**，而不是「新就是好」：

| | 收益 | 保留成本 |
|---|---|---|
| `openssl-probe` · `windows-sys` | ★ **零**——前者只在 openssl/boringssl 后端编译（§5.1 锁死 rustls），后者只在 Windows（G13 只出 Linux）。**它们对产物一个字节都不影响** | 每次 rebase 都要重做 |
| `strum` · `daggy` | 无安全公告，纯版本号 | 同上；`daggy` 还连带把 `petgraph` 拖到 0.8 |

★ 留下来的六条里，**只有 `lru` 与 `prometheus` 有安全公告**——真要推 PR 给上游（D12 选项 1），推这两条。

★ ★ **还原不改变任何源码**：这四条升级当初就没有产生调用点适配（升级后签名未变），所以还原是纯清单编辑。fork 相对官方原版的全部改动只有 **12 个文件**（2026-08-14 实测，G41 之后），可用下面「怎么核对」一节的命令随时重算。

### 2. 调用点适配（12 处，3 个文件）

**`nix` 0.24 → 0.31**（`server/transfer_fd/mod.rs`、`protocols/l4/stream.rs`）

★ ★ **这两处按上游 main 的做法改**——上游已经做过同一次迁移（main 的 `nix` 是 `~0.31.1`），照抄它的模式比自己发明可靠。**但没有整文件覆盖**：上游 main 还带着与本次无关的行为变更，而那些没有被 M0 验证过。

| 变化 | 落法 |
|---|---|
| `socket::socket()` 返回 `OwnedFd` 而非 `RawFd` | 调用点加 `.as_raw_fd()`；★ **`unistd::close` 现在签名是 `IntoRawFd` 会消费所有权，所以原有的手动 close 语义仍然正确，没有 double-close** |
| `listen(fd, i32)` → `listen(&impl AsFd, Backlog)` | `Backlog::new(8)` |
| `fchmodat(None, ..)` → `fchmodat(impl AsFd, ..)` | `BorrowedFd::borrow_raw(libc::AT_FDCWD)`，语义与原来的 `None` 一致 |
| `RecvMsg::cmsgs()` 变为可失败 | `?` 传播（控制缓冲被截断时返回 `ENOBUFS`）|
| `setsockopt(RawFd, ..)` → `setsockopt(&impl AsFd, ..)` | 传引用 |

★ ★ ★ **上面这五条是编译器逼着改的。`nix` 这一轮还有两条编译器不会报错的语义变化，2026-08-12 全漏了**，导致 `protocols/l4/stream.rs` 里**开了 rx timestamp 的连接每一次 `read()` 都会失败**（`ENOBUFS`）。2026-08-13 照上游 main 修好（它做同一次迁移时两条都改了）：

| 变化（编译器不报错） | 漏掉的后果 | 修法（同上游 main） |
|---|---|---|
| `cmsg_space!` 从「只有 capacity、`len == 0`」变成 ★ **`len == n` 的零向量**，而 `recvmsg` 按 **len** 取控制缓冲 | 沿用旧写法 `.clear()` 把控制缓冲清成 **0 字节**，于是每次 `recvmsg` 都截断 | `.clear()` → **`.fill(0)`** |
| 配合上一行，截断后 `cmsgs()?` 现在返回 `ENOBUFS` 而不再静默忽略 | `?` 把它一路抛出 `poll_read`，**read 整个失败** | 缓冲改按 `cmsg_space!(nix::sys::socket::Timestamps)` 留（`SO_TIMESTAMPING` 给的是**三个** `TimeSpec`，原来只按一个留） |

★ **另有 4 处 `nix` 类型错误在 `transfer_fd` 的 `#[cfg(test)]` 里**（`socket()` 返回 `OwnedFd`），2026-08-12 未改——因为那些测试**从来没有被编译过**。同样在 2026-08-13 补上。

**`rand` 0.8 → 0.10**（`pingora-runtime/src/lib.rs`、`connectors/l4.rs`、`connectors/offload.rs`）

★ **两轮破坏叠在一起**：`thread_rng()` → `rng()`、`gen_range()` → `random_range()`、`seq::SliceRandom` → `seq::IndexedRandom`，
以及 ★ **0.10 特有的一条**：便捷方法从 `Rng` 移到了**新的 `RngExt` trait**（`Rng` 现在是 `rand_core` 的再导出）。
失效形态很迷惑——`use rand::Rng;` 会被报成 **unused import**，同时 `random_range` 报 **not found**。

**`sfv` 0.10 → 0.15**（`protocols/http/compression/mod.rs`）

★ 上游 main **还没做这个**，这一处是枢衡自己迁的。`Parser::parse_list(bytes)` → `Parser::new(bytes).parse_list::<sfv::List>()`；token 从 `&str` 变成 `&TokenRef`，取值走 `.as_str()`。

### 3. 换掉 `derivative`（RUSTSEC-2024-0388）

★ **`derivative` → `educe` 0.7.6**（`pingora-core/src/upstreams/peer.rs`，5 处）。

`derivative` 自 2021 年起未更新且已被标记失维；它在这里只做一件事——给 `PeerOptions` 派生 `Debug` 并忽略三个函数指针字段。`educe` 是直接对应物（2026-08-01 更新，97M 下载，OSV 干净）：

| | |
|---|---|
| `use derivative::Derivative;` | `use educe::Educe;` |
| `#[derive(Clone, Derivative)]` `#[derivative(Debug)]` | `#[derive(Clone, Educe)]` `#[educe(Debug)]` |
| `#[derivative(Debug = "ignore")]` ×3 | `#[educe(Debug(ignore))]` ×3 |

★ ★ **连带白赚一个**：`syn 1.0.109` 一起消失了——`derivative` 是整个依赖图里**唯一**还要 `syn ^1` 的包。

### ★ 4. fd 卫生：`get_fds_from()` 的两处泄漏（2026-08-14，M1 spike 实测发现）

★ **这是 fork 里唯一一处主动改变运行时行为的改动**（其余都是版本上界与随之而来的适配）。
★ 2026-08-14 补注：§5 的 rustls 那组**不属于**这一类——它是清单 feature 修正加一处
**刻意保持语义不变**的调用点适配（唯一的新增是一行 `warn!` 日志）。两处都在
`pingora-core/src/server/transfer_fd/mod.rs` 的 `get_fds_from()` 里，**上游 `0.8.1` 与当前
`main` 都有**（2026-08-14 核对过 main 的同一函数），且上游仓库**没有任何 issue/PR 提过 CLOEXEC**。

| | 缺陷 | 修法 |
|---|---|---|
| ① | `accept()` 出来的连接**从来不 close**——下面只 `close(listen_fd)`，而它是裸 `RawFd`、没有 `Drop` | 包成 `OwnedFd` 交给 `Drop`，顺带覆盖 `cmsgs()?` 的提前返回路径 |
| ② | `recvmsg` 用 `MsgFlags::empty()`，于是经 `SCM_RIGHTS` 收来的监听 fd **没有 `FD_CLOEXEC`** | 改成 `MsgFlags::MSG_CMSG_CLOEXEC` |

**实测（修复前，M1 的三代升级）**：

| | 攥着的 upgrade.sock fd | 每个监听 socket 的 fd 重数 |
|---|---|---|
| gen1（没经历过移交）| 0 | 1 |
| gen2（一次移交）| **1**（`St=03` CONNECTED）| 1 |
| gen3（移交 + 继承）| **2** | **2** |

★ ★ **两个缺陷会叠乘**：泄漏的那个 socket 自己也没有 CLOEXEC，于是逐代累加。

★ ★ ★ **② 对枢衡比对上游严重得多**，因为两边的升级形状不同：

- 上游的新进程是**从命令行另起**的，fd 只经 `SCM_RIGHTS` 过去，不存在继承路径；
- 枢衡（M1）的新进程是**老进程 fork+exec 的**（systemd 要求它落在同一个 cgroup 内），
  于是没有 CLOEXEC 的 fd 会**同时**从继承与 `SCM_RIGHTS` 两条路进来。

★ 而继承进来的那一份**不在 pingora 的 fd 表里**，所以上游那条尚未发版的
`listen_addresses()`（关掉未被认领的 fd）**够不到它**——也就是说
「未认领 fd 黑洞化」在 fork 式升级下，**即使上游修好也可能仍然存在**。

**守门人**：`tests/m1/run.sh` [7/9] 三条断言（fd 重数全 1、监听 fd 带 CLOEXEC、
攥着的 upgrade.sock 数为 0）。★ 它们同时是 **rebase 的守门人**——
上游若未接受而 rebase 时漏了重做，这三条会红。

★ **已备好上游投稿材料**，见 [`upstream-pr/`](../../upstream-pr/README.md)。

### ★ 5. rustls 后端：两处清单 + 一处调用点（2026-08-14，G41）

★ ★ ★ **先说这条最值钱的**：`pingora-rustls` 这个 crate **从来没有被编译、测试或审计过**，
而 G6 把 TLS 后端锁死在 rustls——也就是说 **M1 第一天打开 `features = ["rustls"]` 时，
产品依赖图会一次进来 42 个从未过任何一道门的 crate**（实测 133 → 175）。

三道门**各自都够不到它**，而且都不是疏忽，是结构：

| 门 | 为什么够不到 |
|---|---|
| G30 的上界审计 | 跑在**根 workspace** 上，而根 `Cargo.lock` **根本不含** `pingora-rustls`／`rustls`／`ring`／`rustls-native-certs`（实测：`sentry`／`x509-parser`／`ouroboros` 同样缺席——**未启用的可选依赖一个都不在锁里**）|
| `tests/vendor/run.sh` 回归网 | `cargo test` **不带任何 `--features`** → `default = []`，rustls 那一半代码一行都不跑 |
| `tools/supply-audit.py` | 同第一条，跑在根 workspace 上 |

✅ **已修**：回归网现在带 `--features pingora-core/rustls` 跑（本条改动的守门人）。

#### ① 两处清单：`default-features = false`（aws-lc-rs 被无谓编进产物）

上游写的是 `rustls = { version = "0.23.12", features = ["ring"] }`，**没有关掉默认 feature**。
而 `rustls 0.23` 的 `default` 是 `["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]`，
`tokio-rustls 0.26` 的 `default` 是 `["logging", "tls12", "aws_lc_rs"]`——**两扇门，都通向 aws-lc-rs**。

于是 **ring 与 aws-lc-rs 两个 crypto provider 一起编进产物**，而这个 crate 自己装的是 ring
（`install_default_crypto_provider()` → `rustls::crypto::ring::default_provider()`，`ring::digest` 也在用），
**aws-lc-rs 一行都不会被调用**。

★ **代价不是「多两个 crate」**：`aws-lc-sys` 是 **69 MB 的 C 源码**（ring 是 8.5 MB）走 cmake 编，
直接顶到 `PLAN.md` G13 的 **musl 静态二进制**分发，也是白送的攻击面。

| 变化 | 值 |
|---|---|
| 丢掉的默认 feature | 只有 `prefer-post-quantum`——★ 而它自己就是 `["aws_lc_rs"]`，rustls 源码里每一处 `#[cfg(feature = "prefer-post-quantum")]` 都在 `src/crypto/aws_lc_rs/` 下。**ring provider 下它是空操作**（读源码确认，非推断）|
| 依赖图 | 175 → **173**，★ **只少了 `aws-lc-rs` 与 `aws-lc-sys`**，别的一个没动 |
| 回归网 | stock 与改后**逐项相同**（对照组跑过）|

★ **上游 `main` 的清单一字未改**（2026-08-14 按 G32 核对），已备投稿三。

#### ② `rustls-native-certs` `^0.7.1` → `0.8.4`

`^0.7.1` 把它钉在 **0.7.3（2024-08-28）**，而 **0.8.0 第二天就发了**，现在已到 0.8.4——
**正是 G30 存在的那个「脱字号自带上界」形状，长在审计够不到的 crate 里**。

⚠ **有调用点适配**（`pingora-rustls/src/lib.rs`，`load_platform_certs_incl_env_into_store`）：
0.8 的 `load_native_certs()` 不再返回 `Result`，改为返回 `CertificateResult { certs, errors }`——
**部分成功从此是可表达的状态**。

★ ★ **这里差点写错，值得记**：直觉写法是「`errors` 非空即失败」，但那**比 0.7.3 更严**。
0.7.3 的 `CertPaths::load()` 结尾那句注释是 `promote first error if we have no certs to return`——
**它只在一张都没读到时才返回 `Err`**；只要读到了哪怕一张，错误就被咽掉。
照直觉写，`/etc/ssl/certs` 里一个读不动的文件就能让反代**起不来**。那是行为变更，不是适配。

落法：`certs.is_empty() && !errors.is_empty()` 才判失败，其余照旧；`errors` 里的内容
改为 `warn!` 一行——**控制流与 0.7.3 完全一致，只是不再无声**（0.7.3 是直接丢掉）。

连带效果：`rustls-native-certs 0.8.4` 自带 `openssl-probe 0.2.1`（0.7.3 用的是 0.1.x），
所以最终依赖图是 **174**，不是 173。

#### ★ ★ ★ ③ 第三扇门：`pingora-core` 自己的 dev-dependency（2026-08-19，G45）

**G41 只关了两扇。第三扇是上游 PR [#966](https://github.com/cloudflare/pingora/pull/966) 的
Codex 复审提出来的**（P2），实测属实，而且**比它说的更宽**。

`pingora-core/Cargo.toml` 的 `[dev-dependencies]` 里有一行裸的 `rustls = "0.23"`。
它是**无条件**的，所以不只影响 rustls feature 构建——**默认（openssl）构建跑测试时同样在编 aws-lc-rs**：

| `cargo tree -p pingora-core` | 改前 | 改后 |
|---|---|---|
| `--features rustls -e normal` | 176 | 176（这一格 G41 已经修好了）|
| `--features rustls -e normal,dev` | **260，aws-lc 在** | **258，没了** |
| 无 feature，`-e normal,dev` | **228，aws-lc 在** | **226，没了** |

> ★ ★ ★ **它为什么此前没被发现**：G41 的判据用的是 `cargo tree -e normal`，
> 而 **`-e normal` 按定义排除 dev-dependencies**。判据结构上够不到被问的范围——
> **这在本 fork 已经是第三次**（G41「三道门都够不到 pingora-rustls」、G43「核对命令只 diff 一个目录」）。
> 三次的形状完全一样：**判据的覆盖面小于它自称回答的范围，而它照样是绿的。**

★ **落法上刻意不指定 provider**：这条 dev-dependency 只被 `connectors/http` 的测试拿去用类型
（`ServerCertVerifier` / `CertificateDer` / `DigitallySignedStruct`），运行时要的 provider 本来就经
`pingora-rustls` 进来。在这里点名一个 provider，就是把本改动要修的错误再犯一遍，
也会和上游 [#630](https://github.com/cloudflare/pingora/pull/630) /
[#887](https://github.com/cloudflare/pingora/pull/887) 的 provider 选型工作打架。
★ 带 `ring` 的写法也量过——**依赖图一模一样**，所以不指定不损失任何东西。

**实测效果**：fork 的 `vendor/pingora/Cargo.lock` 里 `aws-lc-rs` / `aws-lc-sys` 条数
**由 2 变 0**（连带 `dunce`、`fs_extra` 这两个 `aws-lc-sys` 的构建依赖一起消失）。
⚠ 也就是说 **G41 之后到今天，回归网每一次都在编那 69 MB 的 C 源码**。

### ★ 6. `rustls-pemfile` 迁走（2026-08-19，G45）

RUSTSEC-2025-0134：`rustls-pemfile` 失维（仓库 2025-08 归档）、`patched = []`、**无版本可升**。
owner 于 2026-08-19 拍板迁走而不是登记豁免——理由是
[`tools/supply-audit.py`](../../tools/supply-audit.py) 的 `ACCEPTED` 要求每条写清「出路在哪」，
而这条**有干净的出路**；有出路却登记进去，等于把一个已知可解的问题挂起来。
（对照 `daemonize`：它的出路是架构层的 G31，且当时还没落地。）

★ **它不是换实现，是搬家**——公告原文：*"The latest version of rustls-pemfile is in fact a thin
wrapper around the same code used in rustls-pki-types"*。读源码确认属实：
`rustls-pemfile 2.2.0` 里就写着 `impl PemObject for Item`，`read_all` 用的是 pki-types 的同一个
`ReadIter`。这条事实把「证书与私钥解析属安全敏感」这个顾虑的分量降了下来。

| 动了什么 | 落法 |
|---|---|
| `Cargo.toml` | 删掉 `rustls-pemfile`；`rustls-pki-types` 下界 `1.7.0` → `1.9`（`PemObject` 自 1.9.0 起提供），实际解析到 1.15.1 |
| `load_pem_file()` | `Vec<Item>` → `Vec<(SectionKind, Vec<u8>)>` |
| `load_ca_file_into_store()` | `let Item::X509Certificate(..) else` → `if kind != SectionKind::Certificate` |
| `load_certs_and_key_files()` | 三分支手写 match → `PrivateKeyDer::from_pem(kind, der)`（★ 它认的**恰好**是 Rsa/Ec/Pkcs8 三种，逐项一致）|
| `load_pem_file_ca()` / `load_pem_file_private_key()` | `rustls_pemfile::certs()` / `private_key()` → 对应类型的 `pem_reader_iter()` |

依赖图 **174 → 173**，差集只有 `rustls-pemfile v2.2.0`
（口径：`cargo tree -p pingora-core --features rustls -e normal --prefix none | sed 's/ (\*)$//' | sort -u | wc -l`，**含根包自己**）。

#### ⚠ 一处必须逐字保留的语义：ECHCONFIG 会被静默跳过

`rustls-pemfile` 的 `Item::from_kind()` 只映射 7 种段类型，`SectionKind::EchConfigList` 落在
`_ => None`，而上一层 `read_one` 写的是 `None => continue`——**ECHCONFIG 段根本不会被交出来**。
pki-types 的 `(SectionKind, Vec<u8>)` 迭代器**会**把它交出来，于是
`load_ca_file_into_store()` 的「非证书即报错」会对一个含 ECHCONFIG 的 CA 文件判红。
**那是行为变更，不是适配**——与 §5 ② 的 `load_native_certs()` 是同一个形状。

落法是 `RUSTLS_PEMFILE_ITEM_KINDS` **白名单**而不是把 `EchConfigList` 单独排掉：
`SectionKind` 是 `#[non_exhaustive]`，白名单能让将来新增的段类型继续被跳过，
与 `rustls-pemfile` **冻结下来**的行为一致（它已归档，不会再长出新的 `Item`）。

#### ★ ★ 迁移之前，这个 crate 一条测试都没有

所以先补了 **18 条特征化测试**（`pingora-rustls/src/lib.rs` 末尾），
**照着 `rustls-pemfile` 的现有行为写、先跑绿，然后才动迁移**。
★ 其中两条是专门为了不让注释停留在断言上——`load_pem_file_ca` **读完整个文件**
（第一张证书之后的语法错误照样判红）而 `load_pem_file_private_key` **取到第一把就早退**，
**这两个函数在这一点上不一样**，而这种不对称最容易在重写时被「统一」掉。

**三条反证**（撤掉迁移里的某处落法，看对应测试会不会红）：

| 撤掉什么 | 结果 |
|---|---|
| 白名单改成全放行（照直迁移的写法）| ✓ `ca_store_silently_skips_echconfig` 判红，**且只有它红** |
| `load_pem_file_ca` 改成取到第一张就返回 | ✓ `pem_file_ca_still_errors_on_a_malformed_section_after_the_first_cert` 判红，且只有它红 |
| 私钥改成按类型偏好挑（而非文件顺序）| ✓ `certs_and_key_takes_the_first_key_in_file_order` 判红，且只有它红 |

★ 第一条同时证明了**照直迁移确实会改变行为**——白名单不是多余的谨慎。

### ★ 7. 一条 flaky 测试的根因：短读（2026-08-19）

`connectors::tests::test_connect_uds` 会**间歇性**失败。它不是环境问题，是测试自己写错了：

```rust
let mut buf = [0; 9];
let _ = stream.read(&mut buf).await.unwrap();   // ← 返回值就是「读到了多少」
assert_eq!(&buf, b"it works!");
```

★ **`read()` 读到 1 字节也是成功的**。流式 socket 允许把 `write_all(b"it works!")`
分成多段送达，此时 `buf` 里是半条消息加一串 0，`assert_eq!` 就挂了。
返回值被 `let _ =` 丢掉，于是「只读到几个字节」这件事**没有任何地方会说出来**。

**修法**：`stream.read_exact(&mut buf).await.unwrap()`。这是 fork 里的**第二处行为变更**
（第一处是 §4 的 fd 卫生），改的是**测试**不是产品代码。

#### ★ ★ 判据：同一个扰动，修前必红、修后必绿

不是「重跑几次没再见到」——那种证据对间歇性缺陷等于没有。做法是**让它变成确定性的**：
把 mock server 的一次 `write_all` 拆成「1 字节 + 20ms + 剩下 8 字节」两段送达
（**这是流式 socket 的合法行为，不是破坏**），然后

| | 结果 |
|---|---|
| 扰动 + 未修 | **必失败**，`left: [105, 0, 0, 0, 0, 0, 0, 0, 0]`——只有 `i` 到了，后面全是 0 |
| 扰动 + `read_exact` | **通过** |
| 无扰动 + `read_exact` | 通过 |

★ 那个 `[105, 0, 0, …]` 就是短读的签名，也正是它偶发时会打出来的东西。

⚠ **同一形状全树扫过**：另有 `listeners/mod.rs` 一处 `let _ = stream.read(&mut buf)`，
但它读完**不做内容断言**（只为把请求排空），不是同一个缺陷，**没有动**。

★ **上游也有这个问题**（fork 没改过这块），已备投稿。

### ~~★ ★ ★ 8. 加了一条能力：`TlsSettings::with_cert_resolver`（2026-08-19）~~ ✅ **已删除（2026-08-25）**

> ✅ ★ ★ ★ **2026-08-25（G104 第 ② 处）：本条已经删掉了，文件与上游 0.8.1 逐字节相同。**
> （核法：`git diff 00ebcad -- vendor/pingora/pingora-core/src/listeners/tls/rustls/mod.rs` 为空。）
> 8 / 8b / 10 三条**一起**删的，正如下面那句预告的 —— L4 的 ClientHello 预读换到
> BoringSSL 早回调之后，`pingora-rustls` 在根 workspace 里**再没有任何使用者**。
> ★ ★ **下面的原文全部保留**，因为它记录的是「当初为什么不可避免」，
> 而那段论证本身没有错 —— 错的是它的前提（§5.1 第 1 条），而前提已被 G104 换掉。
>
> ⚠ ⚠ **原「归零条件」也一并作废，而这一条值得单独说**：本条的归零条件在 2026-08-20
> 从「我们的投稿被接受」改成了「上游 #632 / #908 落地」，而**真正让它归零的是第三件事** ——
> 我们自己换掉了那一侧的 TLS 后端。
> ★ **一条挂在别人身上的归零条件，会让人只盯着别人动没动。**
>
> 以下为原文（2026-08-25 之前）：
>
> > ⚠ ⚠ ⚠ ★ **2026-08-25（G104）起，产品已经不走这条路了 —— 本条现在是死代码。**
> > 监听器侧换成 BoringSSL 之后，动态挑证书走的是
> > `SslContextBuilder::set_select_certificate_callback`，而 boringssl 那侧的
> > `TlsSettings` 对 `SslAcceptorBuilder` 有 `Deref`/`DerefMut` ⇒ **这个 setter 本来就够得到**。
> > ⏳ **本轮没有删它**，理由是**一起删更干净**：改动 **10**（`Acceptor` re-export）
> > 还被 `crates/fulcrum-server/src/l4.rs` 的 ClientHello 预读用着，`pingora-rustls`
> > 无论如何要留到那一处也换过去为止。⇒ **8 / 8b / 10 三条随同一批一起删。**
> > ★ 而「留着它」的代价只在 **rebase 时**才兑现（要手工重贴），
> > 而在那之前不会有 rebase —— **所以这不是拖，是把一次风险合并成一次。**

> ★ ★ ★ **2026-08-20 更新：这一条的「归零条件」变了，而且变得更好。**
>
> 原打算给上游投一份（投稿五）。**发之前按 G32 搜了一遍，撤稿了**：这件事上游
> 从 2025-04 起已有 **9 条** issue/PR，其中
> [#632](https://github.com/cloudflare/pingora/pull/632) **与本条逐点相同、而且更完整**
> （它顺手把 `build()` 那个 panic 改成了 `Result`），已有 3 个独立使用者证实可用。
> 它卡的是合并冲突 + 维护者评审带宽，不是设计分歧。
>
> ⇒ **本条的归零条件不再是「我们的投稿被接受」，而是「#632 或
> [#908](https://github.com/cloudflare/pingora/pull/908) 落地」。**
> 那天到了就把这一条删掉，改用上游的接口。
> ★ 顺带一条好消息：**归零不再取决于我们自己去推**——有人已经推了 15 个月。
> ⚠ 但在那之前，rebase 时它照旧要手工重贴。

**这是 fork 里第一处「加能力」的改动**，前面那些都是抬上界、修缺陷或修测试。
所以先把它为什么不可避免说清楚。

`PLAN.md` §5.1 第 1 条是**不可回头**的硬约束：

> rustls 后端不支持 `certificate_callback`。**动态证书选择必须实现 `ResolvesServerCert`。**

而上游 0.8.1 的 rustls 监听器**没有任何一扇门通向那个接口**：

| 门 | 实际情况 |
|---|---|
| `TlsSettings::intermediate(cert, key)` | 只接受**文件路径**，`build()` 里写死 `with_single_cert` |
| `TlsSettings::with_callbacks()` | **直接返回错误**（"Certificate callbacks are not supported with feature rustls"）|
| 自己构造 `Acceptor` | 字段私有，crate 外构造不出来 |
| `add_address(ServerAddress)` | 枚举只有 `Tcp` / `Uds`，带不上 acceptor |

后果是：**一个监听端口只能有一张证书、换证书要重载、握手期按 SNI 现签无从实现**——
而自动 HTTPS（G12）与 On-Demand TLS（G15）都建在那条路上。

**改了两处**：

1. `pingora-rustls/src/lib.rs` —— 再导出 `ResolvesServerCert` / `CertifiedKey` / `ClientHello`。
   ★ 为什么由这个 crate 集中导出、而不让上层自己依赖 `rustls`：
   aws-lc-rs 当初正是从 `rustls` 与 `tokio-rustls` **两扇门**一起进来的（见 §5），
   每多一处 feature 声明就多一扇门。集中在这里，provider 的选择只有一个地方能改。
2. `pingora-core/src/listeners/tls/rustls/mod.rs` —— `TlsSettings` 多一个
   `cert_resolver` 字段与一个 `with_cert_resolver()` 构造器；`build()` 里有它就走它，
   **完全不读证书文件**（于是 `intermediate()` 那个「文件读不到就 panic」与这条路无关）。

⚠ ⚠ **代价必须记下来**：加能力比抬上界更贵。上游一旦改动 `TlsSettings` 的形状，
这一条要**手工重贴**，而不是像版本上界那样「看看上游是不是自己抬了」。
★ 已备上游投稿（这是一个对所有 rustls 用户都缺的能力），若被采纳这条 fork 改动即可归零。

★ 判据：`crates/fulcrum-tls` 的 `SniResolver` 走这条路，而
`tests/serve/run.sh` 用真流量验过——`--cacert` 验签通过、ALPN 协商到 h2、
未知 SNI 被拒绝握手（curl 退出码 35）。

### ~~★ ★ 8b. 加了第二条能力：`TlsSettings::set_alpn_wire`（2026-08-20）~~ ✅ **已删除（2026-08-25）**

> ✅ **2026-08-25（G104 第 ② 处）：随 8 / 10 一起删掉了**，理由与改动 8 同一条：
> BoringSSL 那侧直接 `set_alpn_select_callback` 写个闭包即可，
> 而那个「枚举里没有变体能表达混合清单」的阻碍在那一侧根本不存在。
> ★ 原文保留在下面 —— ⚠ 那段「先更正一句在四份文档里被重复了三天的话」的记录
> **与本条的存废无关，它记的是一次判据教训**，不随代码删除而作废。

**这是 fork 里第二处「加能力」的改动**，而它比 §8 那处**小一个数量级**：
只给一个已有的私有字段开一个写入口，不新增类型、不改任何上游行为、不碰别的 TLS 后端。

⚠ ★ **先更正一句在四份文档里被重复了三天的话。** 那句话是
「TLS-ALPN-01 做不了，因为 `TlsSettings` 只能设 `ALPN::{H1,H2,H2H1}`」。
**它不准确**：上游 0.8.1 的 `ALPN` 枚举**本来就有 `Custom(CustomALPN)` 变体**
（`protocols/tls/mod.rs`），`set_alpn` 也是公开的。真正的阻碍在别处——

| 变体 | `to_wire_protocols()` 吐出什么 |
|---|---|
| `H1` | `[http/1.1]` |
| `H2` | `[h2]` |
| `H2H1` | `[h2, http/1.1]` |
| `Custom(x)` | **`[x]`** —— 只有它自己 |

⇒ **枚举里没有任何一个变体能表达「内建那两条 + 一条自定义」的混合清单**，
而 `alpn_protocols` 字段是私有的。这才是阻碍。

RFC 8737 的 TLS-ALPN-01 要的正是这种混合：**同一个 443 端口**既要正常服务
h2 / http/1.1，又要在 CA 来验证时协商出 `acme-tls/1`。挑战挪不到别的端口——
RFC 8737 §3 把端口写死成 443。

**改了一处**：`pingora-core/src/listeners/tls/rustls/mod.rs` 加一个
`set_alpn_wire(Vec<Vec<u8>>)`。枢衡把清单设成 `[h2, http/1.1, acme-tls/1]`。
★ 协商语义由 rustls 定：服务端这份清单是**偏好序**，取第一条客户端也提供的 ——
所以 `acme-tls/1` 放在末尾，**正常流量的偏好完全不受影响**，
而只提供 `acme-tls/1` 的 CA 验证连接会且只会协商到它。

★ **归零条件**：上游给 `ALPN` 加一个多协议变体，或给 `TlsSettings` 开等价入口。
⚠ 与 §8 一样，rebase 时要手工重贴——但这一处只有三行。

★ 判据：`tests/acme/run.sh` 里 pebble 的 `tlsPort` 直接指向枢衡的 TLS 端口，
证书**真的由 TLS-ALPN-01 签下来**；另有两条反向断言钉住
「普通流量拿不到挑战证书」与「acme-tls/1 拿不到真证书」。

### ~~★ 10. 一行 re-export：`rustls::server::{Accepted, Acceptor}`（2026-08-23，M2 批 C）~~ ✅ **已删除（2026-08-25）**

> ✅ ★ ★ ★ **2026-08-25（G104 第 ② 处）：换掉了，本条删除。**
> L4 的 SNI/ALPN 分流现在走 BoringSSL 的早回调（`SSL_CTX_set_select_certificate_cb`），
> 落点在 [`crates/fulcrum-server/src/l4.rs`](../../crates/fulcrum-server/src/l4.rs) 的
> `peek_client_hello`：字节喂进一条**内存传输**，ClientHello 解完的那一刻早回调触发，
> 抄走 `servername()` 与 ALPN 扩展之后**当场把握手掐掉**。
>
> ⚠ ⚠ **换过来有一处行为变化，而它是变好的那一侧**：早回调发生在几乎所有 ClientHello
> 校验**之前**，而 rustls 的 `accept()` 要握手前半程做完才给 `Accepted`。
> ⇒ 一个 rustls 会当场拒掉的 ClientHello（缺 `signature_algorithms`），
> 在这一侧照样读得出 SNI 并正常分流 —— **那正是 `tests/l4/run.sh` ⑥ 当初写下的意图**
> （「枢衡不替上游拒绝一个它自己也许能处理的客户端」），只是 rustls 那条路做不到。
> ★ 那道判据因此换了期望值，旧契约与换的理由留在它自己那里。
>
> ★ 而删掉它的**代价是零**：这一行本来就只是 re-export；
> 而收益是 `pingora-rustls` 整个退出根 workspace 的依赖图（连同 `no_debug` 共 2 个包）。

`pingora-rustls/src/lib.rs` 里多了一行 `pub use`。

**为什么需要**：L4 的 SNI/ALPN 分流要**只读 ClientHello、不完成握手**，
而 rustls 的 `Acceptor` 正是为这件事存在的（`read_tls` + `accept()` → `Accepted`）。

★ ★ **它不是「加能力」**，与 §8 / §8b 那两处性质不同：那两处改的是上游的行为面，
而这一行只是把 rustls 已有的类型**转出去**。而且这正是那个文件自己写着的做法 ——
它上面几行的注释是「⚠ 不让上层自己直接依赖 `rustls`……此处集中导出，
provider 的选择仍然只由本 crate 的清单决定」。

| | |
|---|---|
| rebase 成本 | 几乎为零（一行 `pub use`，与上游代码不冲突）|
| 新增依赖 | **0 个包** —— rustls 早就在图里 |
| 归零条件 | 上游哪天自己导出它就删掉这一行 |

⚠ **替代方案是手写 ClientHello 解析**，而那是在解析**攻击者控制的二进制** ——
安全基线点名不许手搓的那一类里最靠边的一种。⇒ 这一行换掉的是那件事。

### ★ ★ ★ 11. 把删掉的 `pingora-boringssl` 加回来（2026-08-25，G104）

**这是 fork 里第一处「把当初删掉的东西加回来」的改动**，而它的成因不在上游，在我们自己：
`PLAN.md` §5.1 第 1 条被 **G104** 推翻了。

改了三处，全在清单里，**没有一行源码改动**：

1. `Cargo.toml`（workspace）—— members 加 `pingora-boringssl`；
2. `pingora-core/Cargo.toml` —— 加回可选路径依赖，以及上游原样的
   `boringssl = ["pingora-boringssl", "openssl_derived"]`；
   ★ `openssl_derived` 才是源码里那一大片 `#[cfg]` 用的名字：**boringssl 与 openssl
   共用同一份实现**（`listeners/tls/boringssl_openssl/`）。
3. `pingora-boringssl/Cargo.toml` —— 见下面那条**唯一的枢衡改动**。

#### ★ 唯一的改动：去掉上游的 `boring/pq-experimental`

上游写的是 `boring = { version = "4.5", features = ["pq-experimental"] }`。
⚠ ⚠ **那个 feature 不是一个开关，它会让 `boring-sys` 给 BoringSSL 源码打一个补丁**
（`boring-sys-4.22.0/build/main.rs`：`apply_patch(config, "boring-pq.patch")`，
并打一行 `cargo:warning=applying experimental post quantum crypto patch to boringssl`）。

★ **而本 crate 用不上它**：`src/ext.rs` 里唯一碰后量子的地方是 `ssl_use_second_key_share`，
被本 crate **自己的** `pq_use_second_keyshare` feature 门控（默认关），关着时是个空函数。
⇒ 去掉它不改变任何行为。⚠ 将来若要打开 `pq_use_second_keyshare`，
**必须同时把 `pq-experimental` 加回来** —— `SSL_use_second_keyshare` 这个符号只有补丁打上才存在。

> ★ ★ ★ **更硬的那条理由：它决定了 `docs/verification/musl-boringssl.md` 那份验证还算不算数。**
> 那份探针编的是**原版** BoringSSL。开着 `pq-experimental`，产品编的就是另一份打过补丁的源码，
> 而「musl 上验过了」这句话**不再覆盖产品**。
> ⇒ **一次验证的适用范围，止于它真正编过的那份源码。**

#### ⚠ 一条与 G30 相反的上界纪律

`boring` 的 `"4.5"`（脱字号 ⇒ `<5.0.0`）**必须保留，不许按 G30 抬**。
G30 的口径是拆掉那些把传递依赖钉在旧版上的上界；这一条相反：**`quiche 0.29.3`（G103）
要的也是 `boring ^4`，两边必须解到同一个 `boring`**，否则 `SslContextBuilder`
同名而是两个类型。实测两者解到同一个 **`boring 4.22.0`**。
⇒ 抬成 `>=4.22` 会让 cargo 挑 5.2.0，当场把 quiche 那条路弄坏。

#### ★ 它顺手让另外三条 fork 改动**可以归零**

| 改动 | 为什么在 BoringSSL 那侧不需要 |
|---|---|
| **8**（`TlsSettings::with_cert_resolver`）| boringssl 的 `TlsSettings` 对 `SslAcceptorBuilder` 实现了 `Deref`/`DerefMut`，`set_select_certificate_callback` **本来就够得到** |
| **8b**（`set_alpn_wire`）| 同上：`set_alpn_select_callback` 也够得到，混合 ALPN 清单自己写一个闭包即可 |
| **10**（re-export `rustls::server::{Accepted, Acceptor}`）| ⏳ 要等 L4 的 ClientHello 预读也换过去；`boring::ssl::ClientHello` 有 `servername()` / `get_extension()` / `as_bytes()` |

★ ★ **FORK.md 自己写着「加能力比抬上界更贵，rebase 时要手工重贴」**——
⇒ **这一次换后端会让 fork 变小，不是变大。**
⚠ 但**现在还一条都没删**：产品仍然跑在 rustls 那条路上（本次只是把后端加回来，没有点亮）。
删除的时机与判据见 `PLAN.md` §10 第 61 轮。

#### 首次点亮的实测

`cargo check -p pingora-core --features boringssl --no-default-features`（2026-08-25）：
**0 个 error，43 条 warning，1m25s**。
★ 值得记一句：**这些 `#[cfg]` 分支在本 fork 里从来没有被编译过**
（`Cargo.toml` 原注释：「源码里的 `#[cfg(feature)]` 分支原样保留，只是永远不会被点亮」），
而 fork 期间抬了十条版本上界、换掉了 `derivative`、改过 `nix` 的调用点。
⇒ 「保留着的分支」与「还能编的分支」是两件事，而**在点亮之前没有任何东西能分开它们**。
⚠ 那 43 条 warning 集中在 `boring 4.22` 已废弃的 `SslCurve` / `set_curves`
（`connectors/tls/boringssl_openssl/mod.rs`，出站那侧），**不影响 lint 门**——
`-D warnings` 只作用于 clippy 直接 lint 的那个 crate，vendor 作为依赖被普通 rustc 编译。

### ★ ★ ★ 12. 加了一条能力：PROXY protocol 的「收」接缝（2026-08-27，M2 批 L 第 ① 步）

**上游完全不支持 PROXY protocol**（全库零命中，2026-08-24 核过）。
枢衡要在 **HTTP 面**收它，而唯一正确的位置是**拿到裸 `L4Stream`、TLS 握手之前**的那一处
—— `listeners/mod.rs` 的 `UninitializedStream::handshake()`，它是 `pub(crate)`，外部够不到。

⚠ **本条属「加能力」那一类**，G30 刻意让这一类保持稀有 —— 现存的另一处是改动 11。

#### 改了什么：**一个文件，190 行，零删除**

`pingora-core/src/listeners/mod.rs`：

| 加了什么 | 说明 |
|---|---|
| `pub trait ProxyProtocolPolicy` + `pub enum ProxyProtocolVerdict` | **接缝本身**。两个方法（`trusts` / `feed`）都由使用方实现 |
| `Listeners` / `TransportStackBuilder` / `TransportStack` / `UninitializedStream` 各一个字段 | 把策略从 `Listeners` 一路带到那条连接上 |
| `Listeners::set_proxy_protocol()` | 与上游自己的 `set_connection_filter()` **逐字同形**（含「已有的与之后加的都设上」那半） |
| `read_proxy_protocol()` + `override_peer_addr()` | 循环读 + 覆盖地址 |
| `handshake()` 里一次调用 | 在 `set_buffer()` 之后、TLS 之前 |

★ ★ ★ **解析与信任判断一行都没进 fork**：前者在
[`fulcrum_runtime::proxyproto`](../../crates/fulcrum-runtime/src/proxyproto.rs)（28 条单测），
后者是 `fulcrum_runtime::Runtime::trusts_proxy_protocol`。
⇒ **这段代码不认识 PROXY protocol 的任何一个字节**，rebase 时它不会成为负担。

★ 使用方**不需要给上游再加任何东西**：`Service::endpoints()` 本来就是 `pub`，
返回 `&mut Listeners`。

#### ⚠ ⚠ ★ ★ ★ 一处「看起来行、实际不行」的落法，写下来免得下一个人再走一遍

`PLAN.md` §10 **第 49 轮**那份勘察里有一句「好消息」：

> 「`SocketDigest.peer_addr` 是 **`pub` 的 `OnceCell`** ⇒ 抢在 pingora 自己填之前
> `set` 进去就够了，**不必改它读取客户端地址的任何一处**。」

**那句话是错的。** `services/listening.rs` 在 `io.handshake()` **之前**就调了
`io.peer_addr()`（为了握手失败时那行日志里能带上地址），而
`SocketDigest::peer_addr()` 是 **`get_or_init`** ⇒ 到我们这一步，那个 `OnceCell`
**已经被填过了**，`set()` 必然返回 `Err`。

⚠ ⚠ 而 `OnceCell::set()` 的返回值**很容易被写成 `let _ =`** ——
于是「地址没换成」会是一次**完全无声**的失效：日志正常、请求正常、
只有 `remote_ip` 一直是那台 LB 的地址。

⇒ 落法改成**换一整份 `SocketDigest`**（`GetSocketDigest::set_socket_digest`，上游本来就有）：
从同一个 fd 新造一份、先把 `peer_addr` 填好再挂上去。
`local_addr` / `original_dst` 会从**同一个 fd** 重新惰性派生，什么都不丢。

> ★ ★ **一份勘察报告里的「好消息」，与一条被跑过的路，是两件事。**
> 那句话在 2026-08-24 写下时没有任何东西撞过它，而它躺了三天。
> （同一份勘察里还有一句也错了：「它们是 fork 的第 11 / 12 处」——
> 11 号在 2026-08-25 被改动 11 占掉了。⇒ **一份勘察会同时留下事实和过期物，而它们长得一样。**）

#### 归零条件

⏳ **上游支持 PROXY protocol，或接受一个同形的接缝**。
★ 与改动 8/8b/10 那三条不同的是：**本条不指望上游做什么** ——
它们当年的归零条件全都挂在「上游动一下」上，而真正让它们归零的是第三件事（G104 换了 TLS 后端）。
⇒ 这里如实写：**没有已知的归零路径**，它就是一项要跟着 rebase 的常年成本。

### ★ ★ ★ 13. 一处缺陷修复：h1 的 `body_bytes_sent` 把响应头也数了进去（2026-08-27，M2 批 L 第 ② 步）

**这一处属「缺陷修复」那一类**（与改动 4「fd 卫生」、改动 7「短读根因」同类），
**不是「加能力」** —— 它让一个函数**符合它自己的文档**。

#### 事实

`pingora-core/src/protocols/http/v1/server.rs` 的 `write_response_header` 里有一行：

```rust
self.body_bytes_sent += write_buf.len();
```

而 `write_buf` 是**序列化之后的响应头**。同一个字段的读取函数上面写着：

> `/// Return how many response body bytes (application, not wire) already sent downstream`

⇒ **名字说 body，文档说 body，而它数了 header。**

#### ⚠ ⚠ 而决定性的不是「名不副实」，是**它与 h2 不一致**

| | 计数点 | 含响应头？ |
|---|---|---|
| **h1**（`v1/server.rs`）| 写头处 **与** 写体处各 `+=` 一次 | ✅ **含** |
| **h2**（`v2/server.rs`）| 只在写体处 `body_sent += data_len` | ❌ 不含 |
| **h3**（枢衡自己的 `H3Session`）| 只数体（`sent_n`）| ❌ 不含 |

★ 上游自己的三条 h2 测试断言「16 字节体 ⇒ `body_bytes_sent() == 16`」——
**上游对这个数的期望就是「只数体」**。

⇒ 同一个请求走 h1 与走 h2，这个数**差一个响应头的长度**，而没有任何东西说出这件事。

#### 为什么枢衡非管不可

访问日志有一格 **`resp_size`**，契约（[`docs/architecture/observability.md`](../../docs/architecture/observability.md)）
写的是「响应**体**字节数（不含头）」—— 而**那是一份公开契约**，
不能按客户端用了哪个协议给出两个不同的答案。

⚠ 实测形态：一个 4 字节的 `respond 200 "a-ok"`，日志里 `resp_size` 是 **144**。
★ 它不会让任何东西变红 —— 那个数看起来完全像一个响应的大小。

#### 改了什么

**删掉那一行**（一处），并在**上游自己的测试模块里**加一条回归守卫（一处）。

⚠ ⚠ ★ **守卫放在上游的测试模块里，不是放在枢衡那一侧** ——
那样它才会在 [`tests/vendor/run.sh`](../../tests/vendor/run.sh)（fork 回归网）里跑，
**于是一次把那行加回来的 rebase 会当场变红**。

##### ★ ★ ★ 那条守卫第一版是坏的，而逮到它的是反证的**失败消息**

第一版没给 mock IO 写 `.write(...)` 期望 ⇒ 它在**干净树上也红**，
而且红在 `tokio-test` 的 `unexpected write` 上 ——
**一条红得与它声称要守的那件事毫无关系的判据**。

⚠ 我没在干净树上跑过它就去做注入；注入之后它红了，
而**红的理由读起来不对**（注入的是计数器，红的是 mock IO）。

> ★ ★ ★ **「它红了」与「它因为我注入的那件事而红」是两回事。**
> ⇒ 反证要看的不只是**红没红**，还有**红在哪一行、消息说的是不是那件事**。
> 修好之后再注入，它红在 `left: 19, right: 0` 上 —— 19 正是那个响应头的字节数。

#### 归零条件

⏳ **上游自己修掉它**（或接受一份同形的 PR）。
★ 与改动 4 / 7 同类：这两条当年也是「上游也有这个问题，已备投稿」。
⚠ 本条**尚未投稿** —— 登记在这里，免得它变成一件只有代码记得的事。

### ★ ★ ★ 14. 加了两格数据：`SslDigest` 的 `sni` / `alpn`（2026-08-27，G128，D27 + D28 结案）

**这一处属「加能力」那一类**（与改动 12 同类），⚠ 而它加的是**数据**不是行为 ——
两个 `Option<String>` 字段 + 两行赋值，**不改任何函数签名**。

#### 改了什么

| 文件 | 改动 |
|---|---|
| `pingora-core/src/protocols/tls/digest.rs` | `SslDigest` 加 `pub sni: Option<String>` 与 `pub alpn: Option<String>`；`new()` 的**函数体**里初始化成 `None` |
| `pingora-core/src/protocols/tls/boringssl_openssl/stream.rs` | `SslDigest::from_ssl()` 在建好之后填这两格（`ssl.servername(HOST_NAME)` / `ssl.selected_alpn_protocol()`）|

★ **有意不进 `new()` 的签名**：那样 `rustls` / `s2n` 两个后端的调用点一个字都不用改，
而它们本来就填不出这两格。⇒ 这次改动**只碰 boringssl 那一支**。

#### 为什么枢衡非管不可

访问日志有 `tls_sni` / `tls_alpn` 两格（契约见
[`docs/architecture/observability.md`](../../docs/architecture/observability.md)）。

上游把这两格留给了 `TlsAccept::handshake_complete_callback` + `SslDigest.extension`
（返回值会被塞进那个扩展位，HTTP 层从 `session.digest()` 拿得到）。
⚠ ⚠ **走那条路要求监听器带回调**，而带回调时上游走的是 `handshake_with_callback()` ——
它的第一行 `start_accept()` **无条件**装一个恒回 `-1` 的 `cert_cb`
（`ext::suspend_when_need_ssl_cert` → `SSL_set_cert_cb(raw_cert_block)`）
⇒ **每条 TLS 连接都要多走一趟「挂起 → `certificate_callback` → `resume_accept`」**。

> ★ ★ ★ 那趟开销是 §10 **第 78 轮实测**出来的，而它推翻了一句写下来的推理：
> 「证书是同步挑的 ⇒ 握手不会为证书挂起 ⇒ `certificate_callback` 不该被调到」。
> **每一个词都对，只有「因此」两个字是错的** —— 挂不挂起不由「需不需要」决定，
> 由上游那一行**无条件**的 setter 决定。

⇒ 第 79 轮改成这一处 fork：`from_ssl()` 在握手结束后本来就握着 `&SslRef`，
顺手记两格**一分额外开销都没有**。

##### ★ 它还顺带解决了 h3（D27）

`SslDigest` 一旦有了这两格，**h3 也能自己造一份**
（`crates/fulcrum-server/src/quic/h3_session.rs` 的 `quic_digest`：
`version` 恒 `TLSv1.3`（RFC 9001 §4.2）、`sni`/`alpn` 取自 `quiche::Connection` 的公开 API）。
⇒ h1/h2 与 h3 在访问日志那一层**走同一段代码**，「同一格数据两个填法」在结构上做不到。

⚠ 而 `SslDigestExtension::set` 是 `pub(crate)` —— **不动 fork 的话 h3 根本塞不进那个扩展位**，
这也是候选方案「只把 `set` 放开成 `pub`」被否掉的原因：它只买到一半（D27），
而另一半（D28）的落点就在同一个文件里。

#### 守它的是谁

| 半边 | 谁守 |
|---|---|
| **那两格还在不在** | ★ 编译器（我们的代码读 `ssl.sni`）+ [`crates/fulcrum/tests/tls_digest_gate.rs`](../../crates/fulcrum/tests/tls_digest_gate.rs) 门 1 —— ⚠ 两者的**报错不一样**：编译器说「`SslDigest` 没有 `sni` 字段」（会让人去改自己的代码），门 1 说「**fork 改动 14 被 rebase 冲掉了**」|
| **`from_ssl()` 还填不填** | ⚠ ⚠ **编译器守不住这一半** —— 一次把 `from_ssl` 恢复成上游版本的 rebase 照样编得过，而日志里那两格会**静静地变成 `None`**（读起来是「客户端没发 SNI」，一句完全成立的假话）。⇒ 门 2 读那个函数体，钉住两处赋值**与它们取自哪两个 `SslRef` 方法** |
| **运行时真的填对了** | **第二十三个场景** [`tests/log/run.sh`](../../tests/log/run.sh)：真握手上量 h1/h2 四格、h3 三格，外加一条 `--no-alpn` 的反向（★ 它是唯一分得出「量出来的」与「猜出来的」那条）|

★ ★ **这一条与改动 13 的守法不同，而不同是有理由的**：改动 13 是**删一行**，
守卫能长在上游自己的测试模块里（`tests/vendor/run.sh` 会跑）；
本条是**加两格数据**，而 `from_ssl()` 要一个真的 `SslRef` 才测得到 ——
⚠ 在上游测试模块里写不出一条不假装的判据，所以守卫落在**我们这一侧**，并写明了它们各自答什么。

#### 归零条件

⏳ **上游自己把 SNI / ALPN 记进 `SslDigest`**（或接受一份同形的 PR）。
★ 上游今天的立场是「用 `extension` 自己塞」——
⚠ 那条路对**只想要 SNI/ALPN** 的人来说，代价是每条握手多一趟挂起/恢复，
而这一点大概率上游自己也没量过。**本条尚未投稿。**

### ★ ★ ★ 15. 加了一条能力：连接计数接缝（2026-09-03，M2 批 O）

**上游没有任何按监听器统计连接数的出口**：`Service::run_endpoint` 那条 accept 循环
把连接直接 spawn 出去，`TransportStack` 是 `pub(crate)`，外部够不到那一刻。
枢衡要的是 `fulcrum_connections_total` / `fulcrum_connections_active{listen,entrypoint}`
（`PLAN.md` §10 的 **G122 ②**），而它必须在 **accept 之后、握手之前**记一笔。

⚠ **本条属「加能力」那一类**，G30 刻意让这一类保持稀有 —— 现存的另外两处是改动 11 与 12。

#### 改了什么：**两个文件，一处删除（一行 import 重排）**

> ⛔ 这里**有意不写行数**：一个写在散文里的行数没有任何门守着，改一次就过期而不会红
> （本仓 2026-09-03 为这条栽过一次）。要数就跑
> `git diff <上游基线> -- pingora-core/src/listeners/mod.rs pingora-core/src/services/listening.rs`。

`pingora-core/src/listeners/mod.rs`：

| 加了什么 | 说明 |
|---|---|
| `pub trait ConnectionCounter` | **接缝本身**，两个方法（`enter` / `leave`）都由使用方实现 |
| `pub struct ConnGuard` | ★★★ `leave` 的**唯一**调用点。构造即 `enter`，`Drop` 即 `leave` |
| `Listeners` / `TransportStackBuilder` / `TransportStack` 各一个字段 | 把计数器从 `Listeners` 一路带到那条连接上 |
| `Listeners::set_connection_counter()` | 与上游自己的 `set_connection_filter()`、与改动 12 的 `set_proxy_protocol()` **逐字同形**（含「已有的与之后加的都设上」那半）|
| `TransportStack::connection_counter()` | `run_endpoint` 取句柄用 |

`pingora-core/src/services/listening.rs` 的 `run_endpoint`：循环外取一次句柄与
`Arc<str>` 地址，**spawn 之前**构造 guard 并把它移进任务。

★ ★ ★ **这段代码不认识 Prometheus 的任何一个概念**：它不知道 counter 与 gauge 的区别、
不认识标签、也不知道 `entrypoint` 是什么。它只递一个 `listen: &str`（就是
`TransportStack::as_str()` 那个监听地址原样），别的全在
[`crates/fulcrum-server/src/conn_stats.rs`](../../crates/fulcrum-server/src/conn_stats.rs)。
⇒ rebase 时它不会成为负担。

#### ⚠ ⚠ ★ 为什么 `ConnGuard` 必须长在**这一侧**

它完全可以定义在枢衡那一侧，fork 只留一个两方法的 trait —— fork 会更薄。
**但那样 `run_endpoint` 里就变成「先 `enter`、再自己构造 guard」两步**，
而**漏掉构造 guard 那一行正是 rebase 冲突里最容易被合掉的形状**。
⚠ 它失效时的表现是 **gauge 只涨不降**：正文格式合法、counter 在动、series 也都在，
只有一个数字永远只增 —— **没有任何东西会红**。
⇒ 放在这一侧，是让「守卫存在」成为**结构事实**而不是使用方的纪律。

#### ⚠ ⚠ ⚠ 那个必须绑名字的变量

`run_endpoint` 里那句 **`let _conn_guard = conn_guard;`** 写成裸 `let _ = conn_guard;`
会让它**当场 drop**，于是每条连接进来就立刻减一 ⇒ gauge 恒为 0 而 counter 照涨。
⚠ **同一个后果还有第二种形态**：把那一行**整个删掉** —— 那个值就不再被移进
`async move` 块，于是在 accept 循环这一轮末尾就 drop 了。

✅ **2026-09-04：这一段补上了自己的门。**
`listeners/mod.rs` 的 `mod test` 里多一条 **`枢衡改动15_守卫必须被移进任务且绑了名字`**：
它 `include_str!` 读 `services/listening.rs`，剥掉整行注释后要求「构造了 `ConnGuard`」
∧「有一个**具名**的移动」∧「没有裸 `let _ = conn_guard`」。
★ **两种形态都实测过会红**（注入写在字节副本上、跑完核 sha256 还原），
而同一趟里那条**行为**判据（`枢衡改动15_连接守卫在_drop_时减一`）**照旧绿** ——
⇒ 两条各守一段（一条验行为、一条验接线），⛔ 别合并。

⚠ ⚠ **它第一次跑就红在自己身上**，而原因值得记住：`listening.rs` 里那句**警告**本身就写着
「⛔ 写成裸 `let _ = conn_guard;` 会当场 drop」，判据把**注释里那句警告当成了坏代码**。
⇒ 修法是**先剥整行注释**，⚠ 且只剥整行（行内 `//` 未必是注释）；剥离器自己也带两条自证，
另有一份夹具专门守「注释里提到坏形态的正确源码必须被判为对」——
那是这个 bug 的回归测试。★ 已知边界写在门里：**行尾注释剥不掉**。

★ 端到端那一层仍然有它自己的判据（`tests/metrics/run.sh` 里
「连上 TLS 端口什么都不发 ⇒ `active` +1」）⇒ **三层各守一段**。

#### 守卫

照改动 13 的手法，**回归守卫长在上游自己的测试模块里**
（`listeners/mod.rs` 的 `mod test`）：一个假的 `ConnectionCounter`，
断言「构造即 enter 一次」「drop 即 leave 一次」「递过去的是监听地址原样」。
★ 反证实测：把 `ConnGuard::new` 里那句 `counter.enter(&listen)` 删掉 ⇒ 它当场红，
且 `tests/vendor/run.sh` 把它定向重跑一次后判为「**重跑仍失败 —— 这才是 fork 该被判红的东西**」。

#### 归零条件

⏳ **没有已知的归零路径。** 上游没有表达过要给监听器加统计出口的意思，
而这条接缝服务的是枢衡自己的指标契约。⇒ 它是一项**要跟着 rebase 的常年成本**。
★ 与改动 8/8b/10 那三条不同：它们的归零条件挂在「上游动一下」上，本条不指望上游做什么。
⏳ **投不投上游等 rebase 读过上游 `main` 之后再判**（G122 已定）——
上游 `main` 已把 `prometheus` 整条删掉，口味未知。

### 9. 没有动的一个：`daemonize`

★ **`daemonize` 原样保留，这是一个经过权衡的决定，不是遗漏。**

它的问题同样是**失维**（RUSTSEC-2025-0069）而非陈旧——**没有更新的版本可升**。但与 `derivative` 不同，它做的是**特权丢弃**（`setuid` / `setgid` / `initgroups` / `chown_pid_file`），换掉它的三条路都不划算：

| 路 | 为什么不走 |
|---|---|
| 手写替代 | ★ 特权丢弃的顺序错一步就是真实提权漏洞。与项目安全基线「不手写安全关键原语」的精神直接冲突 |
| 换 `daemonize-me` / `daemonize2` | 维护中，但下载量分别只有 `daemonize` 的 1/50 与 1/1700。**在特权丢弃这条路上，把久经使用但失维的换成新鲜但少人跑的，不是明确的安全改善** |
| 在 fork 里 vendor 它 | 只是把同一段代码换个位置，OSV 不再报是因为它不再是 registry 包——**那是骗扫描器，不是修问题** |

★ ★ **真正的出路是架构层面的**：G13 定的分发方式是 **systemd**，而 systemd 下的现代做法是 `Type=simple` / `Type=notify` **前台运行**，由 `User=` / `Group=` 做特权丢弃、日志走 journal。那样 `conf.daemon` 恒为 `false`，`daemonize()` **整条路径都是死代码**，依赖可以直接删掉——**并且特权丢弃交给了比任何 crate 都更经得起审计的 systemd**。

但这取决于「Pingora 的零停机升级在前台模式下怎么做」，那是 **M1 的设计问题**（与 D5 / D6 相邻），不是这次 fork 该顺手拍板的。当初登记为 **D12** —— ⚠ 那一条已经结案，不在 `PLAN.md` §11 里了：进程模型由 **G31** 定（`Type=notify` 前台运行）并经 **G37** 修订（`ExitType=cgroup`，不交接 MainPID），而 `daemonize` 这个依赖**是被权衡后保留的**（理由见 [`supply-chain.md`](../../docs/platform/supply-chain.md)）。

★ **在那之前，这条公告是已知且已接受的**：它是 informational（失维），**没有 CVE**。

## 效果（2026-08-12 实测）

| | fork 前 | fork 后 |
|---|---|---|
| 锁定包总数 | 176 | **162** |
| 陈旧包 | 44 | **17**（排除非 Linux 目标后 **12**）|
| 安全公告 | 4 | ★ **1** |
| 其中真漏洞 | 2（`lru` UAF、`protobuf` DoS）| ★ ★ **0** |

**只剩 `daemonize` 一条**（失维、无 CVE、无可升版本），理由见上一节。

### 剩下 12 项为什么升不动——★ 已逐条查过绑定约束

**其中 2 项根本不参与编译**（`bitflags 1.3.2`、`miniz_oxide 0.8.9` 只存在于其他 target/feature 的解析结果里）。

★ **真正在编译的 10 项，全部被「已经是最新版」的第三方 crate 卡住**——也就是说**不是我们落后，是生态链上游还没采纳新大版本**：

| 陈旧包 | 卡它的是 | 那个卡它的包是不是最新？ |
|---|---|---|
| `allocator-api2` 0.2.21 | `hashbrown 0.17.1` 要 `^0.2.9` | ★ **是最新** |
| `base64` 0.22.1 | `sfv 0.15.0` 要 `^0.22.1` | ★ **是最新** |
| `alloc-no-stdlib` / `alloc-stdlib` | `brotli 8.0.4` 要 `<3` / `~0.2` | ★ **是最新** |
| `thiserror` / `thiserror-impl` 1.0.69 | `protobuf 3.7.2` 要 `^1.0.30` | ★ **是最新** |
| `getrandom` 0.3.4 | `ahash 0.8.12` 要 `^0.3.1` | ★ **是最新** |
| `syn` 2.0.119 | `strum_macros 0.28.0` 等要 `^2.0` | ★ **是最新** |
| `hashbrown` 0.15.5 · `foldhash` 0.1.5 | ★ **`petgraph 0.8.3` 要 `^0.15.0`**（`indexmap` 与 `lru` 都已要 `^0.17`）| ★ **是最新** |

★ ★ **结论：这 10 项不可能靠我们这侧的任何操作升上去**——除非再去 fork `brotli` / `sfv` / `hashbrown` / `protobuf` / `ahash` / `petgraph` 六七个包，而这一层**已经没有任何安全公告**，纯粹是版本号。**性价比为负，明确不做。**

## ★ 迁移完整性（2026-08-13 审计）

`nix` 那条真回归暴露的问题是「迁移只做了编译器逼着做的那一半」。补完之后**逐条查过还剩什么没做**，方法与结论如下。

### 怎么核对（可随时重跑）

```bash
# 1) fork 相对官方原版到底改了哪些文件
#    ★ ★ 必须**逐个 vendored crate 都比**，外加 workspace 清单。
#    此前这里只 diff 了 pingora-core 一个目录，于是 pingora-runtime 的两个文件
#    （它自己的结论段里明明列着）与 workspace Cargo.toml 结构上都看不见。
#    「改了哪些文件」这种问题，判据的覆盖面必须等于被问的范围。
git clone --depth 1 --branch 0.8.1 https://github.com/cloudflare/pingora /tmp/stock   # 719ef6c
for c in pingora-core pingora-error pingora-http pingora-pool \
         pingora-runtime pingora-timeout pingora-rustls; do
  git --no-pager diff --no-index --stat "/tmp/stock/$c" "/w/vendor/pingora/$c"
done
git --no-pager diff --no-index --stat /tmp/stock/Cargo.toml /w/vendor/pingora/Cargo.toml

# 2) 有上游参照物的那条，逐行比 nix API 的使用
git -C /tmp/stock fetch --depth 1 origin main
git -C /tmp/stock show FETCH_HEAD:pingora-core/src/protocols/l4/stream.rs
```

### 结论

**fork 相对官方原版共改动 12 个文件**（2026-08-14 用上面的命令逐 crate 实测）：

| crate | 文件 | 出自 |
|---|---|---|
| workspace | `Cargo.toml` | 范围裁剪 21 → 7 个成员；`educe` |
| `pingora-core` | `Cargo.toml` | §1 版本上界 + ★ §5 ③（G45：dev-dependency 那扇门）|
| | `src/connectors/l4.rs`、`src/connectors/offload.rs` | §2（`rand`）|
| | `src/protocols/http/compression/mod.rs`、`src/upstreams/peer.rs` | §2（`sfv`）|
| | `src/protocols/l4/stream.rs` | §2（`nix`，含那条真回归）|
| | `src/server/transfer_fd/mod.rs` | §2（`nix`）+ ★ §4（两处 fd 泄漏）|
| `pingora-runtime` | `Cargo.toml`、`src/lib.rs` | §2（`rand`）|
| **`pingora-rustls`** | **`Cargo.toml`、`src/lib.rs`** | ★ **§5（G41）+ §6（G45：迁走 `rustls-pemfile` + 18 条特征化测试）** |

★ ★ **此前这里写的是「9 个文件」，漏了 3 个**——`pingora-rustls` 那两个是 G41 新增的，
但 **workspace 的 `Cargo.toml` 从建立 fork 那天起就一直在改动之列，却从来没被算进去**。
漏算的原因就是上面那条命令：**它只 diff `pingora-core` 一个目录**，结构上看不见另外几个。
★ 判据够不到的东西，不会因为没人提就不存在。

| 升级 | 有上游参照物吗 | 迁移完整性 |
|---|---|---|
| **`nix`** 0.24→0.31 | ★ **有**（main 已抬到 `~0.31.1`）| ✅ **已逐行核对**：两个文件里所有触碰 nix API 的行**与上游 main 完全一致**，无遗漏 |
| `rand` 0.8→0.10 | ✗ main 仍是 `0.8` | 改动 3 个文件；`pingora-runtime` 与 `connectors/l4` 的单元测试覆盖到 |
| `sfv` 0.10→0.15 | ✗ main 仍是 `0.10.4` | ⚠ 见下 |
| `brotli` 3→8 · `lru` · `prometheus` · `strum` · `daggy` | ✗ | ★ **无源码改动**（升级后签名未变），不存在「迁移的另一半」 |
| `derivative`→`educe` | ✗（main 仍用 `derivative`）| 只是 `Debug` 派生宏的等价替换，5 处 |

★ **核对时排除了两条假信号**：main 里的 `close_unclaimed()` 是**新功能**（`f82478ae`，未认领 fd 黑洞化那条修复，0.8.1 里不存在），不属于 nix 迁移，登记在 [尚未验证的接缝](../../docs/verification/open-seams.md)；另一条是 `use std::os::fd::AsRawFd;` 被写进了合并的 `use` 语句里，逐行比对会误报。

### ⚠ 残余风险：只剩 `sfv` 一条值得记名

它是唯一同时满足四条的：**① 改了源码 ② 上游没做过、无参照物 ③ 输入是客户端可控的 `Accept-Encoding` ④ 单元测试只覆盖良构输入**（`test_accept_encoding_req_header` 测的是空值、`"gzip"`、`"what, br, gzip"`，没有畸形输入、没有 q 值）。

★ **但后果面已经查清并且很浅**：`compression/mod.rs` 的解析失败分支是 `warn!` + **保持算法列表为空**——也就是说，如果 sfv 0.15 对某个畸形头比 0.10 更严格，最坏结果是**该响应不压缩**。不是崩溃，不是错误内容，不是安全问题。

**在这个后果面下，不值得为它再做什么。** 若将来 `sfv` 出现在别的、后果更重的路径上，这条要重新评估。

## 验证

```bash
bash tests/m0/docker-run.sh                 # 构建 + fork 回归网 + M0
VENDOR_ONLY=1 bash tests/m0/docker-run.sh   # 只跑 fork 回归网（rebase 后的第一道门）
```

回归网有两层，**缺一不可**：

| 层 | 验什么 | 入口 |
|---|---|---|
| **上游自带的单元测试**（340 条） | 抬过上界的那些库，在 pingora 自己的代码里还对不对 | [`tests/vendor/run.sh`](../../tests/vendor/run.sh) |
| **M0 接缝验证** | 优雅升级时三类流量零中断 | [`tests/m0/run.sh`](../../tests/m0/run.sh) |

判据不是「零失败」，而是 ★ **失败集合与官方原版 0.8.1 逐项相同**——这个容器里有 3 条测试在官方原版上同样失败（依赖容器外的网络行为）。名单写死在 `tests/vendor/run.sh` 里，**多一条少一条都判红**。

### ★ ★ 这一层是 2026-08-13 才补上的，补上当场就逮到一条真回归

**在那之前，「M0 是这次改动唯一的回归网」是字面属实的**，而且比字面更糟——**上游那 340 条测试当时根本编译不过**：

1. `pingora-core/Cargo.toml` 还指着三个没被 vendor 进来的 TLS 后端。★ 这个坑**在构建时完全不显形**：作为根 workspace 的 **patch 依赖**，cargo 不去读未启用的可选 path 依赖的清单；一旦作为 **workspace 成员**加载就必须全读。
2. `transfer_fd` 的测试代码里有 4 处 `nix` 0.31 的类型错误——lib 改了，测试没改，而测试从没被编译过。

清掉这两条之后跑出来的结果，见下节。

## 维护

★ **上游每次发版都要重做一遍这里的改动**——这正是 `PLAN.md` §9 那条「Pingora 上游演进与枢衡定制的合并成本」，G30 把它从一条风险变成了一项进行中的成本。

### 上游有没有动，由脚本盯着

★ ★ **不要靠人记得去看。** patch 过去的 crate **脱离了 `cargo update` 的视野**，上游发了新版不会有任何提示。`tools/dep-check.py` 因此**单独查一次上游** `pingora-core`，与本 fork 的基线比对：

```bash
python tools/dep-check.py
```

```text
── fork 上游检查 ──
fork 基线 pingora-core 0.8.1 仍是上游最新，无需 rebase。
```

- **基线从哪来**：★ 直接读本目录 `pingora-core/Cargo.toml` 的 `version`——**不另设一份要手工同步的记录**
- **退出码 20** = 上游领先（与 cargo 那条叠加，两者都有则 30）
- ★ **它只报告、绝不自动采纳**——rebase 一个 fork 不是脚本该无人值守去做的事

### ⚠ ⚠ ★ ★ ★ 上游 `main` 与发布 tag 是 **diverged**，别把 main 当成「tag + 更多」

2026-08-27（§10 第 75 轮）实测 `GET /compare/0.8.1...main`：

```
status : diverged        ahead_by : 190        behind_by : 8
merge_base : faac65b0c2  ← 那是 0.8.0（2026-03-02）
```

⇒ **两条线在 0.8.0 分了叉。** 那 8 个「只在 tag 上、不在 main 上」的提交里有：

- `Bound default HTTP/2 server limits to mitigate memory exhaustion`
- **`RUSTSEC-2026-0098 and RUSTSEC-2026-0099 fixes`**

⚠ ⚠ **把 fork 挪到 `main`、或按 `main` 重做 vendor，会静默丢掉这两条安全修复。**
✅ 我们从 **0.8.1** vendor，所以它们**我们有**；受威胁的只是「以后照 main 对齐」这个动作。

★ 本节此前只有下面「rebase 的步骤」里那句「先看上游有没有已经自己抬了某几条」，
而 `dep-check.py` 当时只报「main 领先 N 个提交」—— **一句会被读成「main ⊇ tag」的话**。
✅ 已修：它现在报 `diverged / 领先 / 落后`，并把只在 tag 上的那几条**逐条列出来**。

#### ⚠ 「提前捞能省一轮迁移」这句话，2026-08-27 第一次被量了 —— 量出来是零

把本文件表里每一条上界与 `main` 逐项比过（取 `raw.githubusercontent.com` 上 8 份清单）：

| | 我们 | 上游 `main` | 结论 |
|---|---|---|---|
| `brotli` | `8` | `3` | main **更低**，无可捞 |
| `rand` | `0.10` | `0.8` | 同上 |
| `sfv` | `0.15` | `0.10.4` | 同上 |
| `x509-parser` | `0.18.1` | `0.16.0` | 同上 |
| `prometheus` | `0.14` | **这条依赖不在了**（零命中）| ⚠ ⚠ ★ main 把它从 `pingora-core` **整条删了**。⇒ 上游下次发版时：本文件 §1 那条 `prometheus` 上界（**RUSTSEC-2024-0437** 的修法）会随依赖一起消失，而**上游自带的 `prometheus_http_app` 也会消失**。★ 我们的产品代码**一行都不碰它**（任何一份 `crates/*/Cargo.toml` 里都没有 `prometheus` 这个依赖），所以不挡路。✅ **那条「有保质期」的路已经不用走了**：M2 批 M 的指标端点按 **G117 自研 text exposition、零新依赖**，与上游那个 `prometheus_http_app` 无关 —— 它消不消失都碰不到我们。⚠ 判据要按依赖写，别按文本写：`crates/` 下现在**搜得到** `prometheus` 这个词（都是讲我们自己那个端点的注释与文档） |
| `lru` | `0.18.2` | `0.18.2` | ✅ 已一致（我们的 PR）|
| `boring` / `boring-sys` | `4.5` | **`5`** | ⛔ **不能捞**，见下 |

⛔ **`boring` 4 → 5 是唯一一条 main 真的领先的，而它不能捞**：`quiche` 0.29.3 要
`boring = "^4.3"` ⇒ 抬到 5 会让图里出现**两份 `boring`/`boring-sys`**，
也就是一个二进制里链两套 BoringSSL。★ 守住这条的**不是任何一道门，是编译器**
（`crates/fulcrum/tests/supply_gates.rs` 自己写着：两道门按包名去重，
答不了「同一个名字有几个版本」）。

⚠ 而 `main` 其余的依赖漂移方向是 **Cloudflare 自己的内部需要**：
`aws-sdk-s3` · `dial9-tokio-telemetry` · `daemonix`（替掉 `daemonize`）· `flurry` ·
`tikv-jemallocator`。⇒ 捞过来等于把别人的部署形态搬进我们的依赖图。

> ★ ★ **「提前捞能省一轮迁移」不是一句错话，是一句从没被量过的话。**
> 它成立的前提是「上游在往我们要去的方向走」，而 `main` 今天走的是另一个方向。

### rebase 的步骤

1. 取上游新 tag，比对本文件「改了什么」两节
2. ★ **先看上游有没有已经自己抬了某几条**——`nix` 这一次就是白捡的
3. 重做剩下的上界改动，逐个修调用点
4. ★ **把 `pingora-core/Cargo.toml` 里三个没被 vendor 的 TLS 后端（openssl / boringssl / s2n）
   连同 `[features]` 里对应三条一起删掉**——不删，任何直接对 vendor 树跑的 cargo 命令都会失败，
   而构建时完全不显形（理由见该文件内的注释）
5. ★ ★ **跑 `bash tests/m0/docker-run.sh`**（回归网 + M0，两层都要）

★ ★ ★ **第 3 步有个必踩的坑，2026-08-12 就踩了一次**：**「改到编译通过」不等于「迁移做完了」。**
`nix` 那一轮真正伤人的两条（`cmsg_space!` 的 `len` 语义、`.clear()` → `.fill(0)`）**编译器一个字都不会说**。
所以第 3 步的正确做法是 ★ **把上游 main 对同一个文件的 diff 通读一遍**，而不是只修报错的行：

```bash
git -C <上游克隆> show FETCH_HEAD:pingora-core/src/protocols/l4/stream.rs
```

★ 第 5 步是这个坑唯一的兜底——**上游那 340 条测试是白送的，它当天就逮到了这条**。

两条降低成本的路子（**属 D12，尚未拍板**）：

1. **把这些上界改动作为 PR 推给上游。** 合并后 fork 即可退役。★ 上游 main 已经自己抬了 `nix`，说明他们并不抗拒——`lru` 那条还带 RUSTSEC 编号，是最有说服力的一条。
2. **常年跟着上游 rebase。** 改动集中在几个 `Cargo.toml` 与 12 个调用点，冲突面不大。

★ **重做时先看上游有没有已经自己升了**——`nix` 这一次就是白捡的。

## 已知的一处轻微行为差异

`get_fds_from()` 里 `msg.cmsgs()?` 在控制缓冲被截断（`MSG_CTRUNC`）时会提前返回，**跳过 `unlink(path)` 的清理**，留下一个残余的 socket 文件。上游 main 的行为完全相同。这条路径在正常升级中不出现（缓冲按 `MAX_FDS = 32` 预留），记在这里只是为了不让它成为将来某次排查的意外。
