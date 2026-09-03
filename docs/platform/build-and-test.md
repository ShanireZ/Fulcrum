---
type: 技术基线
title: 构建与验证
description: 一切在 Docker 里跑（G26）；宿主机只需 Docker；SIGQUIT + fd 移交是 Linux 独有的，Windows 上根本没法验。
resource: ../../tests/m0/docker-run.sh
tags: [构建, 必读, 易错]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G26（构建与验证环境 = Docker Desktop）
  - id: agents
    resource: /references/agents.md
    title: AGENTS.md「Building and testing」
---

# 怎么跑

```bash
bash tests/m0/docker-run.sh                  # 构建 + 下面全部场景
BUILD_ONLY=1     bash tests/m0/docker-run.sh # 只构建
LINT_ONLY=1      bash tests/m0/docker-run.sh # 只跑 fmt + clippy + shellcheck
LINT=0           bash tests/m0/docker-run.sh # 跳过 lint
UNIT_ONLY=1      bash tests/m0/docker-run.sh # 只跑枢衡自己那几个 crate 的测试
UNIT_TESTS=0     bash tests/m0/docker-run.sh # 跳过它们
SERVE_ONLY=1     bash tests/m0/docker-run.sh # 只跑数据面端到端（真流量）
SERVE_TESTS=0    bash tests/m0/docker-run.sh # 跳过它
L4_ONLY=1        bash tests/m0/docker-run.sh # 只跑 L4 面端到端（M2 批 A–D）
L4_TESTS=0       bash tests/m0/docker-run.sh # 跳过它
FILES_ONLY=1     bash tests/m0/docker-run.sh # 只跑自研静态文件端到端（M2 批 F）
FILES_TESTS=0    bash tests/m0/docker-run.sh # 跳过它
CACHE_ONLY=1     bash tests/m0/docker-run.sh # 只跑自研 HTTP 缓存端到端（M2 批 G）
CACHE_TESTS=0    bash tests/m0/docker-run.sh # 跳过它
CACHEDISK_ONLY=1  bash tests/m0/docker-run.sh # 只跑缓存磁盘后端端到端（M2 批 H）
CACHEDISK_TESTS=0 bash tests/m0/docker-run.sh # 跳过它
ENCODE_ONLY=1    bash tests/m0/docker-run.sh # 只跑响应压缩端到端（M2 批 I）
ENCODE_TESTS=0   bash tests/m0/docker-run.sh # 跳过它
H3_ONLY=1        bash tests/m0/docker-run.sh # 只跑 HTTP/3 端到端（M2 批 J）
H3_TESTS=0       bash tests/m0/docker-run.sh # 跳过它
PP_ONLY=1        bash tests/m0/docker-run.sh # 只跑 HTTP 面的 PROXY protocol（M2 批 L ①）
PP_TESTS=0       bash tests/m0/docker-run.sh # 跳过它
LOG_ONLY=1       bash tests/m0/docker-run.sh # 只跑结构化访问日志（M2 批 L ②③）
LOG_TESTS=0      bash tests/m0/docker-run.sh # 跳过它
METRICS_ONLY=1   bash tests/m0/docker-run.sh # 只跑 Prometheus 指标端到端（M2 批 M）
METRICS_TESTS=0  bash tests/m0/docker-run.sh # 跳过它
RELAY_ONLY=1     bash tests/m0/docker-run.sh # 只跑换代时的 QUIC 跨进程转交（M2 批 K）
RELAY_TESTS=0    bash tests/m0/docker-run.sh # 跳过它
MUSL_ONLY=1      bash tests/m0/docker-run.sh # 只跑产品 musl 静态产物那一格
MUSL_TESTS=0     bash tests/m0/docker-run.sh # 跳过它
SMOKE_ONLY=1     bash tests/m0/docker-run.sh # 只跑冒烟自证
SMOKE_TESTS=0    bash tests/m0/docker-run.sh # 跳过它
STRESS_ONLY=1    bash tests/m0/docker-run.sh # 只跑压力
STRESS_TESTS=0   bash tests/m0/docker-run.sh # 跳过它
ACME_ONLY=1      bash tests/m0/docker-run.sh # 只跑 ACME 签发端到端（pebble 当本地 CA，G64）
ACME_TESTS=0     bash tests/m0/docker-run.sh # 跳过它
RENEW_ONLY=1     bash tests/m0/docker-run.sh # 只跑 ACME 续期端到端（G58 的「续期」半边）
RENEW_TESTS=0    bash tests/m0/docker-run.sh # 跳过它
VENDOR_ONLY=1    bash tests/m0/docker-run.sh # 只跑 fork 回归网（rebase 后的第一道门）
VENDOR_TESTS=0   bash tests/m0/docker-run.sh # 跳过 fork 回归网
UNCLAIMED_ONLY=1 bash tests/m0/docker-run.sh # 只跑「未被认领的继承 fd」场景
M1_TESTS=0       bash tests/m0/docker-run.sh # 跳过 systemd 场景

bash tests/m1/systemd-run.sh                 # 单独跑 systemd 场景
M1_ONLY=main     bash tests/m1/systemd-run.sh
M1_KEEP=1        bash tests/m1/systemd-run.sh # 失败后保留容器，进去看现场
```

## 场景清单

⚠ ⚠ **这张表没有门看着**，而它已经漂过多次 ——
**一张自己不会变红的清单只会越落越远，落后时读起来还完全正常。**
★ 加场景时，**加进这张表就是加场景的一部分**。

> ★ [`tests/musl/probe.sh`](../../tests/musl/probe.sh)（musl + BoringSSL 静态链接探针）
> **故意不是**一个常设场景 —— 它编的是 spike，答不了「产物是不是单静态二进制」（G108）。
> ⚠ 但它照样被 lint 那一格的 `shellcheck` 扫到 —— 「不在门禁里跑」与「不被 lint 看」是
> 两件事。★ 现在这是**推导的结果**（扫的是 `tests/` 整棵树），不再需要谁记得往清单里加一项。



| 场景 | 入口 | 判据 |
|---|---|---|
| ★ **lint** | `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `shellcheck` | 见下 |
| ★ **枢衡自己的 crate 测试** | [`tests/unit/run.sh`](../../tests/unit/run.sh) | `cargo test --workspace --locked --no-fail-fast` 全绿，**且条数不低于下界**，见下 |
| **fork 回归网** | [`tests/vendor/run.sh`](../../tests/vendor/run.sh) | ★ 不是「零失败」，而是**失败集合与官方原版 0.8.1 逐项相同**（现为 2 条）。⚠ 它**依赖容器能上公网**，见下 |
| ★ **数据面端到端** | [`tests/serve/run.sh`](../../tests/serve/run.sh) | 真流量走完路由 → 转发 → `rewrite` → `header_up` → 重定向 → 421 → **回落三向**（501 / 转发 / 502）→ keep-alive |
| ★ **L4 面端到端** | [`tests/l4/run.sh`](../../tests/l4/run.sh) | TCP/UDP 透传、SNI/ALPN 分流（**不终止 TLS**，字节原样重放）、PROXY protocol 的收与发、★ **换代时 L4 长连接不断**（自建监听器参与 socket 移交）|
| ★ **静态文件端到端** | [`tests/files/run.sh`](../../tests/files/run.sh) | 索引 / 尾斜杠 301 / browse / ETag+304 / 单段 Range / MIME；★ 它有一条别处都没有的判据：**路径穿越与 `hide` 清单** —— 那两样坏掉时服务完全正常，只是多发了几个文件 |
| ★ **HTTP 缓存端到端** | [`tests/cache/run.sh`](../../tests/cache/run.sh) | RFC 9111 语义：命中/回源、`no-store`/`private`/`Set-Cookie` 不存、兜底 `ttl`、`Vary` 两分支共存、请求侧 CC、`purge`、防惊群。★ 最贵的一条：**带 `Authorization` 的响应不许发给别人** |
| ★ ★ **缓存磁盘后端端到端** | [`tests/cache/disk.sh`](../../tests/cache/disk.sh) | ★ ★ ★ 决定性的那条是**把进程杀掉再起来东西还在**（同一次跑里用去掉 `disk` 的同一份配置反证：内存后端必然回源）。另有：启动不扫盘 / 优雅停机存索引 / **重验证只改 meta 不动 body** / 索引冷时 `purge` 照样清得掉 / 坏条目即丢即删 / 目录用不了则关缓存但照常转发 |
| ★ ★ **响应压缩端到端** | [`tests/encode/run.sh`](../../tests/encode/run.sh) | 反代与静态文件两条路真的压了（★ **解出来逐字等于原文**，不是「有那个头」）/ 不该压的三种都没压 / ★ 预压缩旁文件逐字发出且**强 ETag 与 Range 都在** / ★ 陈旧旁文件被当成不存在 / ★★★ **压缩 × 缓存**：写法不同但首选相同的命中同一条、不认压缩的拿不到压缩字节、两种表示共存 |
| ★ ★ ★ **HTTP/3 端到端** | [`tests/h3/run.sh`](../../tests/h3/run.sh) | ★ 它把批 J 从「零件通了」变成「入口在跑」：在它之前**没有任何配置能打开那个监听器**。★ ★ **客户端是 curl 的 OpenSSL-QUIC 栈**，与被测的 quiche 没有一行共同代码 ⇒ 这是本仓库对 h3 唯一的**互操作**判据（`quic` 模块里那两条走真 UDP 的端到端用的是 quiche 自己的客户端，两边一起理解错时会一起绿）。⚠ 全场 `--http3-only`，**不是** `--http3`（后者会回落到 h1/h2）；开跑前拿一个空 UDP 端口自证这把尺子真的在走 QUIC。五件事：h3 真的通 / **h3 与 h1 跑同一条执行链**（三种 outcome 逐字相同）/ ★★★ **逐跳头在 h3 那侧消失而 h1 上仍在**（RFC 9114 §4.2，自带对照）/ ★★★ **`Alt-Svc` 出现在六条互不相同的响应路径上** / **两条反向**（明文端口不发、h3 自己不发）|
| ★ ★ **PROXY protocol（HTTP 面）** | [`tests/proxyproto/run.sh`](../../tests/proxyproto/run.sh) | **M2 批 L ①**。它是唯一一格量**连接开头那几个字节**的 —— 在 TLS 握手**之前**。v1/v2 两种头 · 与首包合并发送 · `LOCAL` 不覆盖对端 · ★ **两条反向**（不在信任清单里就一个字节都不读；另一个实例有意不配它当对照物）· 换配置立刻生效 |
| ★ ★ **结构化访问日志** | [`tests/log/run.sh`](../../tests/log/run.sh) | **M2 批 L ②③**。★ ★ ★ **判据挂在那一行的内容上，不是「文件非空」** —— 契约定的是字段清单，而「有输出」与「输出对」之间隔着整张表。字段用 `python3` 读，**绝不 `grep '"status":200'`**（字段顺序/空格/转义一变就静默漏判）。一条请求正好一行 · `uri` 是 `rewrite` **之前**那个 · 打不开的日志路径是**装载期硬错误** · `outcome` 闭集逐个走一遍 · 白名单头（★ 一条带 `Authorization`/`Cookie` 的请求，整行**原始字节**里一个都找不到）· TLS 四格（★ **`--no-alpn` 那条反向是唯一分得出「量出来的」与「猜出来的」的判据**）· 敏感头**两条路各拦一次** |
| ★ ★ **Prometheus 指标** | [`tests/metrics/run.sh`](../../tests/metrics/run.sh) | **M2 批 M**（G116–G121）。★ ★ ★ **判据挂在「增量」与「交叉对照」上，不挂在「抓到了东西」** —— 一个把指标硬编码成常量的实现，能通过每一条只问「正文里有没有那几个字」的判据。⚠ 计数发生在 `Record::finish`（响应写完**之后**）⇒ 第 N 次抓取看到的是前 N−1 次的量，所以每条判据都**抓两次做差**。正文用一个自带自证的 `python3` 解析器读（它先证「命中得了、也落空得了」）。格式（每个族的 HELP/TYPE，族的清单从 `FAMILIES` 派生 · `Content-Type` 逐字 · 标签键集合**逐字相等**，多一个 `uri` 就红 · ★ 上游两族**族在、样本无**，分得开「没接上」与「没数据」）· 访问控制两个方向（站点 C 把 `metrics` 圈在 `10.0.0.0/8` 里 ⇒ 我们够不着，拿到兜底 403）· ★★★ **三条反向**：① 没写 `metrics` 的站点上同一路径**不是**指标；② 50 个未知 `Host` 之后 series 条数不增长**且**那一格正好 +50（⚠ **两句都要**：实测把那句 `inc` 删掉之后，前半句是**绿的**）；③ 一致性门 —— 指标在站点 A 上的增量 = 访问日志新增行数 + 1（那个 +1 是左端那次抓取自己）· ★★★ G121：同一条请求上指标的 `site="a2.example"` 与日志的 `site="http://a.example:9920"` 给出**不同的值**，且 `site` 标签的取值是一个**闭集** = 四条地址字面量 + `<none>` · ★★★ **TLS 族**（G122 / G127）：明文请求一笔都不记 · h1 与 h2 **分开打分开断**（合成一条的话，一个只在 h2 上记的实现照样全绿）· 涨的那一条的 `{version,cipher}` 与**访问日志刚报出来的那一对值**逐字相同（⛔ 不写死套件名）· h3 落在 `cipher="<unknown>"` 而**同一条请求的日志行里 `tls_cipher` 不出现** · **这个族里一个空标签值都不许有** —— ⚠ 实测：改成记空串之后「TLS 族正好 +1」**仍然是绿的**，逮住它的是空标签那条 · ★★★ **`status_class="none"`**（G124）：两边一起断（日志多一行 `status=0` + 指标那一格正好 +1，且两边落在同一个 `site` 上），⚠ 夹具是「不带 `Content-Length` 的慢上游 + 发完立刻 RST」——**带 `Content-Length` 的话 `status` 是 200**，因为响应头 `write_all` 只进了用户态缓冲、根本没有 syscall |
| ★ ★ ★ **换代时的 QUIC 跨进程转交** | [`tests/quic-relay/run.sh`](../../tests/quic-relay/run.sh) | **M2 批 K**（G109 ②③④⑤）。★ 它验的是一句**产品承诺**：「换代时长连接不断」——TCP 那侧 M0 早就验过，而 h3 在批 K 之前**不成立**。⚠ 它是唯一一格会在一次跑里**起两代产品进程**的场景（`SIGQUIT` + `-u`）。两代给**不同的响应体** ⇒「这一条是谁服务的」由**响应本身**说，不用去日志考古。★ ★ 三条自证缺一不可：空 UDP 端口上 `--http3-only` 必须失败 · **不换代**时同一条连接能连打 3 个请求（否则「换代后失败了」与「这条链路本来就不行」分不开）· `num_connects` 之和 = 1（否则一个每次重连的 curl 会让整格全绿，而它证明的是别的事）|
| ★ **ACME 端到端（签发）** | [`tests/acme/run.sh`](../../tests/acme/run.sh) | 门禁里跑**真的 CA**（pebble，G64），一次签两张：HTTP-01 一张 + DNS-01 通配符一张（33 条）|
| ★ ★ **ACME 端到端（续期）** | [`tests/acme/renew.sh`](../../tests/acme/renew.sh) | ★ G58 的**另一半**：证书寿命压到 ~1.5 天，**跑着的**枢衡自己的巡检循环按 ARI 重签一张（16 条）。判据不是「出现了新证书」，见 [TLS 与自动 HTTPS](/architecture/tls.md) |
| ★ ★ **冒烟（自证）** | [`tests/smoke/self-check.sh`](../../tests/smoke/self-check.sh) | ★ 它跑的是 [`tests/smoke/run.sh`](../../tests/smoke/run.sh)——**那个脚本的正式用法是对着真域名跑**，门禁里拿一个健康实例 + 一个死端口 + 一张自签证书，证明它对好目标给绿、对坏目标给红 |
| ★ ★ **压力** | [`tests/stress/run.sh`](../../tests/stress/run.sh) | 持续负载下**零错误、fd 不涨、内存不涨**，且负载中途走一次全量 load 仍然零失败。⚠ **不产出性能声明**，性能口径见 [G19](/verification/performance-bar.md)（M3）|
| **M0 接缝验证** | [`tests/m0/run.sh`](../../tests/m0/run.sh) | 优雅升级中三类流量零中断 |
| ★ **未被认领的继承 fd** | [`tests/m0/unclaimed.sh`](../../tests/m0/unclaimed.sh) | ★ **它复现的是已知的坏行为**，见下 |
| ★ ★ **M1 产品二进制** | [`tests/m1/product.sh`](../../tests/m1/product.sh) | ★ **它跑的是 `fulcrum serve`，不是 spike**（G78）：`Type=notify` 就绪、pid 文件、三次 `systemctl reload`（改配置 / 回滚 / **换二进制**）、停机走完排空 |
| **M1 systemd 主场景** | [`tests/m1/run.sh`](../../tests/m1/run.sh) | 两次 `systemctl reload` 零停机换代 + 停机仍走完排空。★ 跑的是 **spike**，验的是机制那一层（fd 移交、`CLOEXEC`）|
| ★ **M1 `ExitType=main`** | [`tests/m1/exit-type-main.sh`](../../tests/m1/exit-type-main.sh) | ★ 复现坏行为：去掉 `ExitType=cgroup` 会怎样 |
| ★ **M1 MainPID 交接** | [`tests/m1/mainpid-handover.sh`](../../tests/m1/mainpid-handover.sh) | ★ 复现坏行为：钉住 G31 推断的那条被否掉的路 |

## CI

[`.github/workflows/gate.yml`](../../.github/workflows/gate.yml)，触发是 **push 到 `main`** + 手工。

★ ★ ★ **它里面只有一条命令，而且与上面那条逐字相同**：`bash tests/m0/docker-run.sh`。
这不是省事，是判据纪律 —— CI 若自己拼一套「等价的」步骤，就会长出**第二套判据**，
而两套判据迟早分家，那时「CI 绿」与「门禁绿」是两件事，却没有任何东西会说出来。

| 决定 | 理由 |
|---|---|
| `runs-on: ubuntu-24.04`（不是 `ubuntu-latest`）| M1 场景依赖宿主机是 **cgroup v2**；`latest` 换代时不会有任何一行输出说环境变了 |
| **没有 `paths-ignore`** | ⚠ 「只改文档就跳过」是错的：`doc_contract.rs` 用 `include_str!` 读 `dsl-reference.md`，另一道门还**反向**断言已接线的能力不许出现在里面 ⇒ **纯文档改动真的能把门禁弄红** |
| `concurrency` + `cancel-in-progress` | 连推几次时只有最后一次跑到底 |
| `timeout-minutes: 120` | ⚠ **止血阀**，不是预期耗时 |
| 先「腾出磁盘」一步 | runner 上 `/` 只有二十几 GB，而这里要放两个镜像 + cargo registry + release 模式的整个 `target`。⚠ 磁盘满的报错形态五花八门，**没有一条会说「磁盘满了」** |

⚠ **CI 是兜底，不是第一眼**：本机先跑一遍仍然是规矩。

## 构建缓存

搬运在 [`tests/ci/cache.sh`](../../tests/ci/cache.sh)：`save` / `restore` 两个子命令，
把两个 docker 命名卷**流式**打成一个归档（tar 在容器里跑、压缩在宿主机上跑，
不落几个 GB 的中间文件 —— runner 的磁盘装不下）。量级：`fulcrum-cargo` 约 700 MB、
`fulcrum-target-*` 约 6 GB、归档约 2 GB（装得进 GitHub 每仓 10 GB 的缓存预算）。

★ ★ ★ **缓存最坏的失效方式不是「没命中」，是「命中了一份不该用的」。** 五道防线：

| # | 防线 |
|---|---|
| 1 | 缓存键里带 `Dockerfile.build` 的哈希 —— **工具链身份**（连 C 工具链一起，cargo 的 fingerprint 覆盖不到那半边）|
| 2 | 归档里另存 `meta.txt`，**灌回前逐字比对**那个哈希；对不上就跳过并说明，**绝不硬灌** |
| 3 | 同一份 `meta.txt` 里的 `target-volume=` 也逐字比对 —— 它把「哪个镜像 × 哪一棵工作树」两个坐标都编在里面，少了这一条，**另一棵树产出的归档照样灌得进这棵树的卷** |
| 4 | 解压器按**归档自己的魔数**选，不按「这台机器上装了什么」选 —— 判据挂产物不挂环境 |
| 5 | 灌完**当场自证**卷里真的有东西；空了就喊（不判红，但绝不让人以为它生效了）|

⚠ 第 3 条不能用「反正容器里都是 `/w`，内容通用」跳过：**路径通用，`mtime` 不通用**。另一棵树编出来的产物比这棵树的源码新时，cargo 会直接当成最新的复用——就是下面那两次实测的形状。★ 而它落在最看不见的地方：CI 上没人盯着卷名，只会看到一轮很快的绿。

⚠ **怀疑缓存的时候**：把 workflow 里缓存键的 `v1` 改成 `v2`，整片缓存立即作废。

### ⚠ 三件说在明处

1. **命中缓存能省下的是门禁那一步的编译时间**，一轮从二十几分钟降到十几分钟。
   ⚠ 每次实测都会给出一个区间而不是一个数（两次实测差 27%，而**每一步都同比变慢**）——
   那是 runner 快慢的差别。★ 拿其中一次当「那个数字」会把抖动说成确定性。
2. **命中即跳过导出**：曾经有一轮命中之后仍然花了六分钟压出一份归档，
   而下一步因为命中被跳过 ⇒ 那六分钟原样扔掉。
   > ★ **一步只在「它的产物会被用到」时才该跑**，而判据要往前挪到那一步自己身上。
3. **命中时缓存不刷新**（导出整步跳过）。于是随提交累积，缓存里那份 target 相对 HEAD
   越来越旧，cargo 每轮补的差量慢慢变大。哪天两把锁之一变了，键跟着变、自然重做一份新的。

## ⚠ 关于 fork 回归网的两件事

### 一、已查实：它**依赖容器能上公网**

`pingora-core` 的 `connectors::*` 里有一批测试**连的是真的 `1.1.1.1:443` / `:80`**
（`one.one.one.one`，见 `pingora-core/src/connectors/http/{mod,v1,v2}.rs`）。
这道门一直悄悄挂着一个外部前提，而 [§8 的性能对拍](/verification/performance-bar.md)
要求环境可复现。真要处置，方向是把这批测试指向**容器内的假上游**。

### 二、✅ 已查实并修掉：那条偶发是**短读**，不是环境

上游 `test_connect_uds` 自己写错了：

```rust
let mut buf = [0; 9];
let _ = stream.read(&mut buf).await.unwrap();   // ← 返回值就是「读到了多少」
assert_eq!(&buf, b"it works!");
```

`read()` 读到 1 个字节也算成功，而流式 socket 允许把 `write_all(b"it works!")` **分段送达** ——
此时 `buf` 里是半条消息加一串 0。**返回值被 `let _ =` 丢掉，于是「只读到几个字节」
没有任何地方会说出来。** ✅ fork 里已改成 `read_exact`（见 `vendor/pingora/FORK.md` §7）。

#### ★ ★ ★ 判据：扰动，不是重跑

「又跑了几次没再见到」对间歇性缺陷**等于没有证据**。做法是**把它变成确定性的**：
让 mock server 按「1 字节 + 20ms + 剩下 8 字节」送达（**流式 socket 的合法行为，不是破坏**），
未修的版本每次都失败，改成 `read_exact` 之后有无扰动都通过。

> ★ ★ **间歇性缺陷的出路是「把它变成确定性的」，不是「多跑几次看看」。**
> ★ 而在查实之前，**先写下「我不知道」比写下一个讲得通但没验过的原因更有用** ——
> 后者会让下一个人跳过实验。

## ★ ★ 「枢衡自己的 crate 测试」堵的是一个结构性缺口

在它出现之前，**没有任何一个场景会跑本仓库自己写的 Rust 测试**：

- fork 回归网跑的是 `vendor/pingora` **自带**的测试；
- M0 / 未认领 fd / M1 三个都是 shell 写的端到端；
- lint 只看得见「编不过 / 有 warning」。

也就是说，M1 产品代码里的每一条 `#[test]` 都会**一次都不跑，而整条链照样报绿**。
★ 这与本仓库反复抓到的「判据覆盖面小于它自称回答的范围」是同一个形状。

★ **判据里最要紧的是测试条数的下界**（`MIN_TESTS`）。理由是回归网上吃过的那一次：
缺 `--no-fail-fast` 让 `cargo test` 每次都停在第一个测试二进制，六个 crate 的单测从没跑过
而门一直是绿的 —— **「新加的测试没让计数变化」是这类缺陷唯一会露头的地方**。

三条反证都做过：有测试失败 → 红；`MIN_TESTS=999` → 红；还原 → 绿。

## ★ ★ M1 的场景跑在**另一个容器**里

它们要验的东西只存在于 systemd 里（MainPID、cgroup 生命周期、`KillMode`），所以用
[`docker/Dockerfile.systemd`](../../docker/Dockerfile.systemd)（Debian 13 + **systemd 257**，
钉到 digest）另起一个 `--privileged --cgroupns=private`、以 systemd 为 PID 1 的容器。
`docker-run.sh` 在最后调用 [`tests/m1/systemd-run.sh`](../../tests/m1/systemd-run.sh) 驱动它们——
**一个「要另外记得跑」的场景，与不存在的场景没有区别**。

⚠ ★ ★ **千万不要再把宿主机的 `/sys/fs/cgroup` 挂进那个容器**（网上很多做法这么写）。
容器用的是私有 cgroup namespace，再挂一次宿主机的树两边就对不上：journald 起不来，
**而且 `MAINPID=` 通知会被静默丢弃**—— 差点因此写下一个完全错误的结论。
cgroup v2 下 `--privileged` 自己就会挂成 rw。详见
[M1 spike #1](/verification/m1-systemd.md) §6。

★ **测试宿主镜像也按内容哈希重建**（与构建镜像同一条纪律）：systemd 的版本是本 spike
结论的一部分，浮动 tag 会让结论某天悄悄失效。⚠ 它**何时拔钉子尚未定案**——
G36 只管构建镜像的 rustc，见 [待定清单](/governance/open-questions.md)。

★ ★ **复现类场景的绿意味着「坏行为已复现」，不是「功能正常」。** 上游
`listen_addresses()` 发版、fork rebase 上去之后，那条断言要**反过来写**；届时它变红是口径
变了，不是它坏了。背景见 [尚未验证的接缝](/verification/open-seams.md)。

★ M0 那几个场景共用 8080–8082 端口，**每一个都必须不留残余进程**（`tests/m0/run.sh` 末尾
等最后一代真正退出正是为此）。⚠ 实际踩过：进程没走干净，下一个场景绑不上端口，
而它的基线探测**照样变绿** —— 对着残留进程。

## ★ ★ 产品的 musl 静态产物也跑在**容器之外**（D22）

[`tests/musl/product.sh`](../../tests/musl/product.sh) 自己 `docker build` 一个 **Alpine** 镜像
把**产品本体**编成 musl 静态产物，再塞进 `FROM scratch` 里跑一次 `fulcrum validate`。
⇒ 它与那次 `docker run` 是两回事，与 M1 那四格同理，由 `docker-run.sh` 在最后驱动。

⚠ ⚠ **它的上下文是仓库根**（根 `Cargo.toml` 的 `[patch.crates-io]` 指着 `vendor/pingora`），
所以仓库根有一份 [`.dockerignore`](../../.dockerignore) —— ★ 而 `.dockerignore` 是**按上下文根读的**，
`Dockerfile.build`（上下文 `docker/`）与 `Dockerfile.musl-probe`（上下文 `spikes/musl-boringssl/`）
**都读不到它**。「加一条 `.dockerignore`」听起来是全局动作，实际上不是。

★ **两条 BuildKit 缓存挂载是它挂得起门禁的前提**：`COPY . .` 意味着仓库里任何一个文件变了
这一层就作废，而这是一道每次提交都要跑的门。实测 **6m49s（冷）→ 50s（改了 Rust 源码）
→ 12s（只改别的文件）**。
⚠ ⚠ **CI 上没有这份缓存** —— `fulcrum-cache` 覆盖的是 docker 命名卷，覆盖不到 buildkit 的
cache mount ⇒ **CI 每次都付冷价**。这一条写在明处。

⚠ **`id` 里必须带 `$CRT_STATIC`**：反证那一趟的 RUSTFLAGS 与正向不同，
共用一份 target 只会让两边互相把对方的产物挤掉 —— cargo 不会算错，只会一直重编。

## ★ 收尾（重做）

两个脚本现在用**同一套规则**，写在各自的 `cleanup` 里，并挂在 `EXIT` trap 上（**失败路径也要收**，否则 `fail` 一 exit，两代原样留下）：

> **最后起的那一代是活的，用 `SIGINT`；更早的都在排空，直接 `SIGKILL`。**

这条规则来自一次**实测**，不是推演：

| 那一代的状态 | 发 `SIGINT` |
|---|---|
| 还在 `main_loop` 里等信号 | **0 秒退出**（`ShutdownType::Quick`，两个超时都是 0）|
| 已收过 `SIGQUIT`、正在排空 | ★ **被吞掉**，40 秒后照样活着 |

后一条的原因：进程已离开 `main_loop`、没人再从信号管道里读，而 tokio 的处理器仍然装着、顶掉了 `SIGINT` 默认的 terminate 行为。**排空中的那一代收不到任何可捕获的信号**——它只能自己走完 `CLOSE_TIMEOUT`(5) + `grace_period_seconds`(30) + `graceful_shutdown_timeout_seconds`(≤30)，最快 35 秒、最慢 65 秒。所以对更早那些用 `SIGKILL` 是**有意的选择，不是等待失败后的兜底**：判据此刻已全部做完，它们的 fd 也早已交接出去，只剩「把端口空出来」。

★ 这条同样适用于 M1 的 systemd 设计：**一旦一代开始排空，就叫不动了**，`TimeoutStopSec` 必须按 35–65 秒这个量级来配。

### 三条从收尾这件事带走的

- **阈值和被检查的对象不能是同一个数** —— 收尾原先「发 `SIGTERM` 等 30 秒」，而配置里的
  `graceful_shutdown_timeout_seconds` 也是 30，于是它每次都落进 `SIGKILL` 分支、
  在每次全绿的运行里打一行 ⚠。★ **每次都亮的告警等于没有告警。**
- **调数字之前先读它在等什么**：把上限从 30 提到 45 照样每次都红 —— 那 30 秒是无条件
  `thread::sleep`，根本不是「等连接排空」。
- ★ **一个只是碰巧成立的清理，和一个不存在的清理，区别只在运气**：收尾改快之后，
  `unclaimed.sh` 的 `[0/6]` 当场报「开跑前 8081 上就已经有人在 LISTEN」——
  原来第一代一直是被那 30 秒必然超时的等待**顺带**收掉的。
  ⇒ 两个脚本现在都逐代登记 pid、逐代收。

★ **宿主机除 Docker 外什么都不需要** —— 不用装 Rust 工具链。

## ★ lint 门

- 排在最前跑：它最快，红了指得最准（一行代码），不必等几分钟的端到端。
- 用 `--workspace` 而不是写死包名：★ **写死包名的作用域会在加新 crate 时悄悄漏掉它们**。
- `clippy` / `rustfmt` **装在镜像里**（官方 rust 镜像不带它们），不在跑测试时临时
  `rustup component add` —— 后者要联网且版本会漂，而它们是判据本身。

★ ★ **`shellcheck` 那半边扫哪些文件是推导出来的，不再是一张手写清单**
（[`tests/ci/shellcheck-all.sh`](../../tests/ci/shellcheck-all.sh)）。

那张手写清单栽过两次，两次都是**漏一项而没有任何东西会说**：`tests/m1/lib.sh` 带着一处
SC2045 躺进来；后来漏掉的是整个 `tests/quic-relay/`（18 项 vs 仓库里 19 个含 `.sh` 的目录），
底下压着 4 条真实告警，**从来没人看见过**。
⇒ 照 `docker-run.sh` 对 `*_ONLY` 的同一条路子改：**不列举，问一句结构性的问题**
（「`tests/` 这棵树下有哪些 `.sh`」），**并且每次运行都自证**。加目录的人不需要记得改任何清单。

- 用 `find` 而不是 glob：`tests/**/*.sh` 在**非交互 bash 里没有 globstar**，`**` 退化成 `*`
  ⇒ 它逐字等价于 `tests/*/*.sh`，**两层以下的脚本一个都不匹配**，而写着 `**` 的人以为覆盖到了。
- 枚举器每次拿一棵**答案已知**的假树自证（两层深的脚本 · 带空格的路径 · 名字像脚本的目录 ·
  `notes.md` 与 `run.sh.bak`），**逐字比对** ⇒ 少认一个红，多认一个也红。
- **「一个都没找到」是红，不是「扫完了」** —— 所以那里不许写成 `find … | xargs -r shellcheck`：
  `-r` 的语义就是「没有输入就一条都不跑」，扫空时退出码 0，安静地全绿。

> ★ 三条反证都实测过：① 新建一个**不在任何清单里**的 `tests/<x>/`，塞一条 SC2045 ⇒ 当场红；
> ② 把枚举器限成一层深 ⇒ 自测报「实得少了 `two/deep/nested.sh`」；③ 把枚举器弄成恒返回空
> ⇒ 自测报「实得为空」。**一条从未被证明会红的扫描判据，与没有这条判据是一回事。**

⚠ ⚠ **别把这道门当成能拦住那处子 shell 缺陷的东西 —— 实测它拦不住。**
shellcheck 0.10 的 SC2030/SC2031 只认**写在子 shell 那几行里**的赋值（`( B=1 )`、`X=$(D=9; …)`
都报），而 `tests/quic-relay/run.sh` 当年那处是「函数体里 `PIDS+=`、调用方 `$(f)`」——
逐字复现过，**一声不吭**。拦住它的是场景收尾时那条「用过的端口要还回去」的自证，不是 lint。
> ★ 这条要写在明处：**一把没量过就被当成能量的尺子，比没有尺子更贵。**

★ **lint 那一格还挂着两条与 lint 无关的自检**，都是因为它们守的代码
**在一趟绿的门禁里从来不会被执行**：

| 自检 | 它守的东西 | 为什么挂在这里 |
|---|---|---|
| `tests/ci/dump-cache.sh --self-check` | CI 那段搬运代码的退出码取法（G94） | 本体只在 `cache-hit != 'true'` 时才跑，往后基本不会再跑 |
| `tests/acme/self-check.sh` | ACME 两个场景的**失败现场取证**（`lib.sh` 的 `acme_dump_ports` 一族） | 取证只在「已经要红」的路径上执行 ⇒ 场景一绿就碰不到它 |

> ★ ★ 两条是同一个形状：**一段只在出事那天才第一次运行的代码，等于一段没人验过的代码**，
> 而出事那天发现它自己是坏的，就把那一次现场也一起赔进去了。
> 后者自己开一个监听 socket 当靶子，要求取证**指名道姓**说出持有者是哪个 pid，
> 再关掉靶子要求它改口 —— 两个方向都钉，只钉「看得见」的话一个恒说「看得见」的实现照样全绿。

★ **vendor 的 `unexpected cfg condition value` 不会把这道门带红**（实测）：`--` 之后的
`-D warnings` 只作用于 clippy 直接 lint 的那个 crate，`vendor/pingora` 作为依赖被普通 rustc
编译，它的 warning 就只是 warning。

# ⛔ 不要在宿主机上装 Rust（G107）

**理由是一次实测**：整套 C++ 工具链装齐之后，`cargo check -p fulcrum-tls` **仍然编不过** ——
BoringSSL 编出来了，**编不过的是我们自己的代码**。六个产品 crate 里有四个用 `std::os::unix`
（证书存储的 `flock` 与 `0600`、L4 面的 fd 移交），`cargo check --workspace` 还会另外倒在
`sd-notify` 的 `F_SETFD` / `clock_gettime` 上。
⇒ 本机能覆盖的从头到尾只有 `fulcrum-config` 与 `fulcrum-runtime` 两个 crate ——
**与什么都不装的时候是同样的两个。**

> ★ ★ 那段弯路只有一条教训：**「卡住它的从来不是 X」** —— 不是 musl、不是 BoringSSL，
> 是构建宿主；不是链接器参数，是 PATH 上那个 `link.exe` 到底是谁；也不是 C++ 工具链，
> 是我们自己的代码。**每一步都真的修掉了一个拦路的东西，然后才发现它从来不是决定性的那个。**

⇒ 不要在宿主机上跑 cargo；不要往任何 PATH 里加 `CARGO_TARGET_DIR`、`LIBCLANG_PATH`
或工具链目录；不要把任何本机输出当判据、也不要当雷达。想看编译错误就跑门禁。

# 为什么全在容器里（G26）

两条理由，第二条是硬的：

1. **零主机污染、完全可复现**，且**将来直接就是 CI 与对拍环境的底座**
2. ★ ★ **`SIGQUIT` + fd 移交是 Linux 独有的**——这套东西**在 Windows 上根本没法验证**

未采纳 WSL2 Ubuntu 26.04：★ 它的 `sudo` 要密码，工具链安装无法自动化；且主机会被装东西、环境不可复现给别人。

# 构建镜像

[`docker/Dockerfile.build`](../../docker/Dockerfile.build) → `fulcrum-build:local`，**不存在、或 Dockerfile 内容变了**，都由脚本自动重建。

★ **基础镜像单独是不够的**：pingora 拉 `libz-ng-sys`，它要 `cmake` 编 zlib-ng。镜像因此额外装了：

| 包 | 为什么 |
|---|---|
| `cmake` | ★ **必须** —— `libz-ng-sys` 用它编 zlib-ng |
| `clang` | 备着 —— 将来引入 `aws-lc-rs` 或任何走 bindgen 的 `*-sys` 包会用到 |

## ★ 基础镜像钉死到 digest

```
FROM rust:1.98.0-trixie@sha256:271849e998ffce5776454bbf98c5dc21baafc854ff8e566197908d3aca9a81e8
```

★ 换钉之前先 `docker manifest inspect` 验过新 digest 真的解得开 ——
**别把一次全量冷编花在一个取不到的镜像上**。
镜像里实际是什么，每次跑验证时由 `docker-run.sh` 打印（取自 `/etc/fulcrum-toolchain`）。

**为什么必须钉**：`Cargo.lock` 入库、vendor 的锁也入库、G26 明写「完全可复现」、
[§8 的性能对拍](/verification/performance-bar.md)要求环境可复现 —— 而编译器本身原先跟着
`rust:1-trixie` 这个浮动 tag 走。

★ ★ **这不是假设，是已经发生过的事**：Dockerfile 早就从 `rust:1-bookworm` 换成了
`rust:1-trixie`，而本机镜像里跑的**一直还是 bookworm**（脚本只判「镜像在不在」）。
此后每一次「全绿」，实际用的基础镜像都不是仓库声明的那个，而且没有任何输出会说出来。
钉死并重建之后才看见真实漂移面：**cmake 3.25.1 → 3.31.6、clang 14.0.6 → 19.1.7**。

**修法有两半，缺一不可**：

1. **这里钉死**（tag + digest）
2. **`docker-run.sh` 按 Dockerfile 的内容哈希决定重建** —— 哈希写进镜像的 `cool.cnb.fulcrum.dockerfile-sha256` label，每次比对。这样**任何**改动（digest、apt 装什么、加一层）都会触发重建，不只是 `FROM` 那一行。★ 拿不到哈希时**强制重建**，不是跳过检查。

## 怎么升（口径 = G36）

G29 的精神是追新，所以这个钉子是**要定期拔的**，不是拿来烂在旧版本上的。**什么时候拔由 `dep-check.py` 告诉你**，不用自己记：

```bash
python tools/dep-check.py     # 第三项就是构建镜像检查
```

★ 判据只有 **rustc 版本**。构建镜像不是运行时镜像（G13 分发的是 musl 静态二进制，[首版范围](/product/scope.md)不做官方容器镜像），Debian 包一个都不进产物，所以 digest 漂移只打一行提示、**不判红**——否则这道门会几乎常年亮着。

退出码 **40** 表示要人管，含三种情况：rustc 有新版 / `@sha256` 钉子掉了 / ★ **本次没能查证**（「没能检查」不算「检查通过」）。

★ **它只报告，拒绝自动应用**（`--apply` 也不动），与 fork rebase 同样处理——换编译器必须重跑三场景，不该无人值守。报红之后手工做：

```bash
docker pull rust:1-trixie
docker image inspect rust:1-trixie --format '{{range .RepoDigests}}{{.}}{{end}}'
# 把 Dockerfile.build 的 tag 与 digest 一起换成新值（tag 写精确小版本，别写 1-trixie）
bash tests/m0/docker-run.sh          # 镜像会因内容哈希变化自动重建，三场景必须全绿
# 然后把新的 rustc 版本更新到本页
```

⚠ ★ **同一个 rustc 版本钉在三张 Dockerfile 里，必须一起换**：
[`Dockerfile.build`](../../docker/Dockerfile.build) ·
[`Dockerfile.musl-probe`](../../docker/Dockerfile.musl-probe) ·
[`Dockerfile.musl-product`](../../docker/Dockerfile.musl-product)。
musl 那两份的结论里写着「rustc 与 `Dockerfile.build` 是同一个版本」——
只换一边那句话就变成假的，**而没有任何东西会说出来**（`dep-check.py` 只看 `Dockerfile.build`）。
> ⚠ 这一行曾经写着「两个地方」，而那时已经是三个 ——
> **一句用数字说事的话，会在数字变了的那一刻悄悄变假。**
实测（x86_64）：产品编成 musl 静态产物 **16,191,800 字节**、`INTERP=0 / NEEDED=0`、
在 `FROM scratch` 里 `fulcrum validate` 跑通；冷编 **6m49s**、改 Rust 源码后 **50s**、
只改别的文件 **12s**（靠 BuildKit 缓存挂载）。⚠ **CI 上没有那份缓存，每次都是冷的。**

⚠ ★ **M3 对拍期间冻结。** 开始采集数据时记下镜像 digest，整个对拍期不动——否则[§8](/verification/performance-bar.md) 的原始数据在自己内部就不可比。

★ `apt` 的 `cmake` / `clang` **没有**钉版本——真钉就得连 `snapshot.debian.org` 一起钉，代价与收益不成比例。折中是把实际装到的版本落进 `/etc/fulcrum-toolchain` 并每次打印，**让漂移可见**。查不出来的漂移才是问题。

# 缓存与产物

| 东西 | 放哪 | 为什么 |
|---|---|---|
| `cargo` registry | 命名卷 `fulcrum-cargo` | ★ 避开 Windows 文件系统的慢 I/O，也不污染宿主机。**与工具链无关，可跨镜像共享** |
| `target` | 命名卷 `fulcrum-target-<Dockerfile 哈希前 12 位>-<工作树路径哈希前 12 位>` | ★ **必须同时跟着镜像和工作树走**，见下 |
| 运行期产物（pid / sock / 日志 / 探针输出）| `run/m0/` | **已 gitignore** |

★ **首次构建慢（要拉基础镜像 + 编译全部依赖），之后增量构建约 25 秒。**

## ★ ★ 为什么 `target` 卷的名字里带 Dockerfile 哈希与工作树标签

cargo 的 fingerprint 覆盖 rustc 版本与 flags，**但不覆盖 C 工具链**——build script 产出的东西（`libz-ng-sys` 用 cmake 编的 zlib-ng、ring 的 `.o`）只在自身声明的输入变化时才重建。于是「**换基础镜像但 rustc 不变**」（正是  bookworm→trixie 那一次的形状）会沿用旧镜像编出来的 C 目标文件。

★ 那正是刚钉死的可复现性**在低一层漏掉**：镜像钉住了，但「用这个镜像产出的东西」没钉住。那次是靠**手工** `docker volume rm` 才敢说「全绿归属于钉死后的镜像」——**依赖人记得做的事，等于没做**。

现在卷名带上 Dockerfile 的内容哈希：换镜像自动换卷，旧卷留着还能回退，脚本会把残留的旧卷列出来提示删除。

## ★ ★ ★ 以及为什么还要带工作树标签

上面那条理由一个字都没变，只是它当年成立的前提是「一台机器上只有一棵源码树」——而那个前提已经不成立。本机现在同时有主树和 `.claude/worktrees/` 下的若干棵：容器把各自的 `/w` 挂成**不同的源码树**，却曾经把**同一个** `/w/target` 挂给了所有人。

⚠ ⚠ 后果不是「编得慢」，是**门给出别人家的读数，而两边都不红**。实测撞见两次：一次编译错误指向一个只存在于另一棵树的符号；另一次报「696 条全绿、退出码 0」，跑的却是**没有本次新判据**的旧测试二进制。后一种是致命的那一种——它不报错，它给一个像样的、绿的、错的答案。

⇒ 卷名再拼上工作树路径的短哈希，两条性质同时成立。**推导只有一份**，在 [`tests/lib/vol-lock.sh`](../../tests/lib/vol-lock.sh) 里，由 `tests/m0/docker-run.sh` · `tests/m1/systemd-run.sh` · `tests/ci/cache.sh` 三处共用——此前那三处各写了一遍同一个表达式，而各写各的失效形态是安静地指向两个不同的卷（M1 挂上一个自动新建的空卷、CI 缓存永远不命中）。

## ★ ★ 树标签的第二笔账：谁来回收没有主人的卷

按树分开是拿磁盘换量具：**每棵工作树各占一个约 6 GB 的卷**。而工作树是会消失的——`.claude/worktrees/` 下那些用完就删——**它们的卷不会跟着走**。

⚠ 卷名后缀是哈希，**反推不回路径** ⇒ 光看 `docker volume ls` 说不出哪些已经没有主人。而「别的树的卷一律不给删除命令」这条规矩（它防的是误删隔壁正在用的缓存）恰好把这些也一起挡在了外面。⇒ 磁盘只涨不落，且没有一行字指得出该删哪个。

★ 修法是让卷**自己记住主人**：新建时把归一后的工作树路径写进 label `cool.cnb.fulcrum.tree`（归一用的是 `fulcrum_tree_norm`，与算卷名哈希的是同一份，两处各归一各的话会把一棵活着的树报成「已经不在」）。于是「不属于本树」再分成两半：

| label 记着的树 | 提示 |
|---|---|
| **已经不在了** | 给出 `docker volume rm` —— 删了碰不到任何人 |
| **还在** | 照旧只报数、**不给命令**（删掉别人正在用的那个，只会让那棵树莫名其妙地全量重编）|
| **读不到 label** | 归后一半。加这条规则之前留下的卷、以及手工建的卷（[supply-chain.md](supply-chain.md) 里那条 `supply-audit` 命令就手工挂着一个）都读不出主人，而**「没能检查」不算「检查通过」** |

⚠ label **只有新建那一次写得进去**：对已存在的卷 `docker volume create` 是**静默的 no-op**，不报错也不更新 label（实测）。这不构成问题——树变了卷名就变，名字与 label 永远是同一次新建写下的。★ 新建的那一次**当场自证**读得回来且指向本树；读不回来就喊一句「这条回收提示这一轮等于没有」，**不判红**——它是提示，不是量具本身。

★ 三个方向各一条自测（`selftest_tree_state`，与其它 `selftest_*` 同处）：树还在 ⇒ `live`、树没了 ⇒ `gone`、读不到 ⇒ `unknown`。⚠ 第一条错成 `gone` 会让提示去劝人删隔壁正在用的卷；第二条错成 `live` 会让这条回收路等于不存在。

## ★ ★ 同一棵树上同一时刻只许跑一次门禁

卷分开只解决「别的树」。同一棵树上并发跑两次门禁，两次仍然共用那一个卷——回到同一种坏法。⇒ **拿卷之前**先取一把与卷同名的锁，拿不到就当场说清楚并退出，**不排队、不静默继续**。

★ 锁用 `mkdir` 而不是 `flock`：宿主是 Windows + Git Bash（MSYS），**这台机器上没有 `flock`**（`command -v flock` 空手而归）。写一把在这台宿主上其实不生效的锁，比没有锁更坏——它会让人以为并发已经拦住了。`mkdir` 在目录已存在时必然失败且不留副作用，NTFS 与 ext4 上都成立。⚠ 绝不可以退化成 `[ -d "$dir" ] || mkdir "$dir"`：那是两步，中间正好是竞态窗口。

★ 锁放在宿主机临时目录（`${TMPDIR:-/tmp}/fulcrum-gate-locks/<卷名>.lock`），不放仓库里——仓库里那份会被「`target/` 必须为空」和行尾两道门看见。持有者写进 `pid`，进程不在了就**接管并说出来**（否则一次 Ctrl-C 会把这棵树永久锁死）。`docker-run.sh` 持锁期间调 `systemd-run.sh` 靠导出的 `FULCRUM_GATE_LOCK_HELD` 放行，而后者单独跑时自己上锁。

★ 这两组性质各有**两个方向**的自测，挂在 `docker-run.sh` 每次运行都跑的那批 `selftest_*` 里（与字节探针、`*_ONLY` 判据同一处）：两棵树名字不同 / 同一棵树名字稳定 / 换镜像仍然换卷 / 名字是合法卷名；锁被占用时明确失败并说出原因 / 放开后拿得到且退出时还得回来 / 同一进程树内可重入 / 陈旧锁接管时喊出来。

# 两个 Windows 宿主机上的坑

这两条已经写进脚本，改脚本时不要碰掉：

1. ★ **`export MSYS_NO_PATHCONV=1`** —— git-bash 会把容器内的 `/w` 之类路径改写成 Windows 路径
2. ★ **挂载源必须是 Windows 形式的绝对路径** —— 脚本用 `cygpath -m` 转换

还有第三条不在脚本里，在 [`.gitattributes`](../../.gitattributes)：

★ ★ **行尾必须无条件是 LF。** 开发机上 `core.autocrlf=true` 时，checkout 会把 `tests/m0/*.sh` 转成 CRLF，容器里的 bash 会当场报 `\r: command not found`——**而且是克隆之后才发作，写代码时看不出来**。行尾转换发生在 checkout 不在提交，所以 `git status` 干净**并不能证明什么**。

# 判定

★ **`tests/m0/run.sh` 的退出码即结论。** 它非零即失败，并打印为什么。

它验两类证据：

- **直接证据**：第二代必须在日志里报告继承了两个自建监听器的 fd（`[raw-tcp] INHERITED fd=` / `[raw-udp] INHERITED fd=`）
- **间接证据**：三类流量零中断，且**每类至少成功过一次**（★ 否则「零失败」可能只是因为探针根本没跑起来）

★ 第二条里的「至少成功过一次」值得注意——**它防的是「测试本身无效但看起来全绿」**。

# 提交前

跑 `bash tests/m0/docker-run.sh`；★ **新加或改动了门，就先证明它能红**
（翻转条件、破坏输入、或删掉修复，看着它失败，然后放回去）。

★ 一道**只被观测到绿过**的门，与一道**根本没在跑**的门，是无法区分的。

# 相关

[M0 接缝验证](/verification/m0-seam.md) · [依赖策略](/platform/dependency-policy.md) · [工作方式](/governance/working-agreement.md)
