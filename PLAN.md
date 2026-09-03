# 枢衡 Fulcrum · 产品与实施计划

> 本文件是枢衡（Fulcrum）项目的**唯一权威**。架构细节见 [`docs/architecture/`](docs/architecture/index.md)，
> 它服从本文件；两者冲突时以本文件为准。
>
> 全部文档按 [Open Knowledge Format v0.2](docs/references/okf-spec.md) 组织在 [`docs/`](docs/index.md) 里。

## 1. 当前状态

| 里程碑 | 状态 |
|---|---|
| **M0** 接缝验证 | ✅ **通过** |
| **M1** 接管一台机 | ✅ **通过** |
| **M2** 接管两台 | ⏳ **进行中** —— 自研批次全部做完，观测三件套 **3/3**；⛔ 只剩**退出条件**（两台机器由枢衡承载，那是**运维动作**不是工程任务）|
| **M3** 对拍达标 | ⏳ 未开工 |
| **M4** 发布 | ⏳ 未开工 |

### 产品 crate

| crate | 职责 |
|---|---|
| [`fulcrum-config`](crates/fulcrum-config) | DSL → 诊断 → 结构化配置 |
| [`fulcrum-runtime`](crates/fulcrum-runtime) | 运行时对象图（站点索引／匹配器／执行链）。★ **纯逻辑，不引用 pingora** |
| [`fulcrum-tls`](crates/fulcrum-tls) | 按 SNI 挑证书、每域一目录的 PEM 存储、续期判定 |
| [`fulcrum-acme`](crates/fulcrum-acme) | ACME 客户端：三种挑战 + 原生 Cloudflare / DNSPod |
| [`fulcrum-server`](crates/fulcrum-server) | 数据面：挂到 Pingora 上；L4、静态文件、缓存、压缩、HTTP/3 |
| [`fulcrum`](crates/fulcrum) | 命令行：`validate` / `compile` / `plan` / `serve` |

`vendor/pingora/` 是 Pingora 0.8 的 fork，改动逐条记在
[`vendor/pingora/FORK.md`](vendor/pingora/FORK.md)。`spikes/` 是验证台，不是产品。

### 能力面

✅ **已落地** —— DSL 全链路（诊断 → 结构化 → 运行时图 → TLS 装载）· 反向代理 + 四种自研
`lb_policy` · DNS 定期重解析 · 主动健康检查 · 自动 HTTPS（TLS-ALPN-01 / HTTP-01 / DNS-01）·
全量原子 load（Unix socket）· systemd `Type=notify` 托管下的**换代零中断** ·
L4 面（TCP/UDP 透传、SNI/ALPN 分流、PROXY protocol 收）· 静态文件 · HTTP 缓存（内存 + 磁盘 +
防惊群 + `POST /purge`）· 响应压缩 · **HTTP/3**（quiche，含换代时的跨进程连接转交）·
结构化访问日志（JSON）· **Prometheus 指标**（站点块里的终结指令 `metrics`，
text exposition 自研零依赖）。

⏳ **未落地** —— PROXY protocol 的「发」那半边 · 被动熔断（`passive_fail`）。

### 判据

一条命令跑完全部门禁：

```bash
bash tests/m0/docker-run.sh
```

⛔ **本机不跑 Rust（G107）** —— 构建与测试一律在容器里。逐格说明见
[`docs/platform/build-and-test.md`](docs/platform/build-and-test.md)。

## 2. 项目定位

一句话：

> **一个 Rust 单静态二进制，同时是 Web 服务器、反向代理与负载均衡器；
> 简单时像 Caddy，复杂时不必换软件。**

它要做的事，是把今天必须靠三个软件拼起来才能做到的事，收进一个进程：

```text
今天：Caddy（自动 HTTPS）+ HAProxy（L4 与调度）+ Nginx（静态与缓存）
枢衡：一个进程、一份配置、一套观测
```

## 3. 要解决的问题

三家各有所长也各有硬伤。枢衡的产品假设是：**这些长处不冲突，可以收进一个实现；而这些短处大多源自各自的历史包袱，不是必然代价。**

| | 长处（要吸收） | 短处（要避开） |
|---|---|---|
| **Nginx** | 静态文件吞吐、`proxy_cache` 磁盘缓存、内存占用低、生态与文档最厚 | 自创 DSL 且语义有坑（`if` 不可靠、`location` 匹配顺序反直觉）；无原生自动 HTTPS；OSS 版无管理 API，只能整体 reload；**上游域名只在启动时解析一次**；`stream` 的 L4 能力弱于 HAProxy；模块要编译进二进制 |
| **HAProxy** | L4/L7 调度深度（stick-table、慢启动、queue）、健康检查体系最完整、Runtime API 可热改、stats 原生 | **不是 Web 服务器**（不能服务静态文件）、无缓存层、配置最晦涩、ACME 支持到 3.x 才成型、Data Plane API 是另一个独立进程、**runtime 改动 reload 后无声消失** |
| **Caddy** | 自动 HTTPS/ACME 是标杆、默认即安全、Admin API 全量热加载且失败自动回退、配置最易上手 | Go GC 导致高并发下 P99 尾延迟与内存不如 C 系；调度深度远不及 HAProxy；缓存要装插件；插件生态要 `xcaddy` 重编译；**Admin API 默认绑回环且无认证**，同机任意进程可改配置 |

### 3.1 竞争位置（必须诚实记录）

这一格并不空：Traefik、APISIX、Kong、Nginx Proxy Manager、Zoraxy，以及 Kubernetes Gateway API 及其多个实现，都在附近。
2024 年后还多了 Cloudflare 的 **Pingora**（Rust 代理框架）和 ISRG 基于它的 **River**。

枢衡的空位是真实但狭窄的：**非 K8s 环境下，一个进程同时覆盖 Web 服务器 + 反代 + 负载均衡，且带自动 HTTPS 与原生管理 API。**
没有任何一款现有软件同时做到这三件事——Pingora 是库不是产品，River 不做静态与缓存。

## 4. 设计原则

1. **一个进程，一份配置，一套观测。** 这是产品的全部理由；任何让用户"再装一个东西"的设计都要被质疑。
2. **吸收长处，不继承短处。** 每采纳一个三家已有的设计，必须能说清它避开了对应的哪个坑。
3. **期望状态是唯一权威，例外必须可见。** 临时覆盖不持久化，但永远显示在 stats 与 API 里。
4. **先验证，再发布；失败即回滚。** 配置变更是事务，不是文件写入。
5. **默认即安全。** 管理面默认不出机器；危险能力（On-Demand TLS）默认要求显式准入配置。
6. **性能以端到端实测为准，且数据公开。** 不发布无法复现的性能声明。
7. **回落是脚手架不是架构。** 它的成功标志是被删除。

## 5. 技术底盘

| 项 | 选择 | 依据 |
|---|---|---|
| 语言 | **Rust** | 性能可与 nginx/haproxy 同级（Go 做不到，而那正是 Caddy 的短处）；内存安全避开 nginx/haproxy CVE 史上的缓冲区类问题 |
| 数据面底座 | **Pingora 0.8.1**（Apache-2.0，可被 GPL-3.0 吸收）。★ 实际走 [`vendor/pingora/`](vendor/pingora/FORK.md) 的 fork（G30） | 用到的是**零停机优雅升级**与**上游连接池**（外加 h1/h2 那套协议实现）。⚠ LoadBalancer、健康检查与缓存框架**都是自研的**，不要以为底座送了 |
| HTTP/3 | **`quiche`**（G103），以自建 Service 挂 QUIC 入口 | Pingora 0.8 不带 HTTP/3。取 quiche 的三条：**传输层与 HTTP/3 语义层在同一个 crate 里**（`quiche::h3`，无 feature 门控）· **sans-IO + 公开的 `Header::from_slice`** ⇒ 按 DCID 分流写在我们自己代码里 · ⚠ 而 `quinn` 对认不出的 DCID **回 stateless reset**（会杀掉升级窗口里的在飞连接）。事实表见 [`docs/platform/http3-libraries.md`](docs/platform/http3-libraries.md) |
| TLS | **统一 BoringSSL**（G104） | quiche 用 BoringSSL；继续锁 rustls 就必然两套并存。两个入口共用同一个 `select_certificate_callback`。详见 §5.1 第 1 条 |
| 配置 | **自研 DSL（Caddyfile 式）编译到结构化配置** | 人写 DSL、机器写结构化格式；结构化那份是唯一内部事实 |
| 分发 | **Linux x86_64 + aarch64 单静态二进制（musl）+ systemd 单元 + deb/rpm** | Rust 优势直接兑现：一个文件、零依赖 |

### 5.1 G6 附带的三条硬约束

1. **TLS 后端统一 BoringSSL。** 两个入口（h1/h2 与 h3/QUIC）共用**同一个**
   `boring::ssl::SslContextBuilder::set_select_certificate_callback`，
   动态证书选择走它而不是 `ResolvesServerCert`。

   ★ 取「统一到 BoringSSL」而不是「两套并存」，理由与 G66 同源：
   **让分家在结构上做不到，比让两份互相钉着更可靠。**

   **范围是三处，全部换完**：① 监听器侧（挑证书 / ALPN）· ② L4 的 ClientHello 预读
   （一台真的握手状态机 + 一条内存传输，早回调抄完就掐掉握手，只看不终止）·
   ③ 出站 HTTPS（[`crates/fulcrum-acme/src/https.rs`](crates/fulcrum-acme/src/https.rs)
   自写 `tower_service::Service<Uri>` 连接器喂 `instant_acme::HttpClient`，**新增 0 个包**）。

   ⇒ **依赖图里只有一套 TLS**：`cargo tree -e all --workspace --target all -i rustls`
   → nothing to print。

   ⚠ ⚠ **`Cargo.lock` 是依赖图的超集** —— `hyper-rustls` / `rustls` / `tokio-rustls` /
   `schannel` 仍然写在锁里（`instant-acme` 的 `ring` feature 里那句 `"hyper-rustls?/ring"`
   把它留在了包级解析中）。隔离实验：一个只依赖 `instant-acme` 的空 crate，
   锁里 **129** 个包而 `cargo tree` 只有 **72** 个。
   ⇒ 门 4 的口径是「**锁里写着哪些**」，答「依赖图里真有哪些」的是**门 5**（读 `cargo tree`）。
   ⏳ **「产物里真的链接了哪几套」仍未有判据 —— D23。**

   > ★ ★ 一条方法论：**「不可回头」约束的是我们自己不要反复，不是约束事实不许变。**
   > 本条原本锁的是 rustls，理由是「rustls 没有 `certificate_callback`」——
   > 那句话今天依然为真，只是**我们不再站在 rustls 那一侧了**。
   > ⚠ **一条理由仍然成立、而结论已经不适用的约束，比一条明显过时的约束更难发现。**

2. ⚠ **tower 中间件生态用不上。** Pingora 有自己的 `ProxyHttp` 阶段/过滤器模型，
   tower 的 `Service`/`Layer` 插不进去（同名不同物）。

   ⚠ ⚠ ★ **但第一句要连着一条附注读，否则它会误导下一个人**：
   「Pingora 有自己的 `ProxyHttp` 模型」这句**关于上游为真、关于我们为假** ——
   **我们从来没有用过 `ProxyHttp`**，`pingora-proxy` 连依赖都不是。
   真正的执行链挂在 `HttpServerApp` / `ServerSession` 上。
   ★ **一句关于上游为真、而关于我们为假的描述，比一句明显过时的描述更难发现。**

   ★ 唯一真的用上 tower 的地方与本条**不是一回事**：第 1 条 ③ 那个出站连接器
   **是给别人的接口实现一个 trait，不是把 tower 中间件栈拼进数据面**。

3. ⚠ **自建 QUIC(UDP) 与 L4 监听器必须接入 Pingora 的 socket 移交**，
   否则优雅升级时这两条会断连。这是整个方案里唯一需要真花力气验证的接缝，
   已列为 M0 的退出条件（✅ 通过）。

## 6. 首版范围

### 6.1 自研（四块全部）

- **L7 反代核心**（产品本体）：HTTP/1.1、HTTP/2、TLS 终止、WebSocket、gRPC —— 后三者 Pingora 已覆盖
- **L4 面**：TCP/UDP 透传、SNI/ALPN 分流、PROXY protocol
- **静态文件服务**：range、ETag、压缩与预压缩、目录索引
- **HTTP 缓存**：**整层自研**（G82），`pingora-cache` 不进 fork
- **HTTP/3 / QUIC**：**`quiche`**（G103）独立入口，在枢衡自己的路由层与 h1/h2 汇合。
  ★ **「自研」在这一条里指的是哪一半，由 G105 说死**：入口、连接归属、
  路由汇合与那条中间件链是自研；**协议栈本体（QUIC 状态机 + HTTP/3 帧层 + QPACK）不是** ——
  用 `quiche::h3`。⚠ 手写 QPACK 会直接违反[安全基线](docs/platform/security-baseline.md)第 5 条
  （QPACK 就是 HPACK 在 HTTP/3 里的对应物），而那一条是整个安全论证的支点。
  > ★ 此前这两件事一直靠「HTTP/3 是自研的一块」同一个词指代，读起来像是连协议栈也自己写。

### 6.2 首版明确不做

- Web UI（G23）；管理面只有 DSL + CLI + HTTP API
- 服务发现集成（Consul / etcd / Docker labels）
- Kubernetes Endpoint 监听与 Gateway API 实现
- OpenTelemetry tracing
- Windows / macOS 支持
- 多节点控制平面、RBAC、审批流
- 官方容器镜像（文档给现成 `FROM scratch` Dockerfile 代替）

### 6.3 回落层 ⛔ **已整块删除**（G98）

原本的设计是：自研某块能力尚未完成时，该类流量回落给 Nginx（静态与缓存）或 Caddy（其余），
1.0 时归零。**它的三个用户（`l4` / `file_server` / `cache`）在 M2 批 B/F/G 里全部自研完成**，
于是整层比原计划提前删掉。

⚠ 写了 `fallback_nginx` / `fallback_caddy` 的配置现在**编译不过**，报的是一条专门的诊断
（`FUL-DSL-0034`），不是「未知的全局选项」。留下的三条结论见
[`docs/architecture/fallback.md`](docs/architecture/fallback.md)。

## 7. 里程碑（按「能自用」切分）

> ★ 「某一整块做完了」与「这个里程碑做完了」是两件事。**退出条件是唯一能把两者分开的东西**，
> 所以每个里程碑都写清楚它，并且**不因为功能清单看起来做完了就提前打勾**。

### M0 · 接缝验证（范围按 G27 收窄）✅ **通过**

**范围** —— 一个 Pingora `Server` 同时挂三类服务：内建 HTTP 代理（h1/h2）、自建裸 TCP 服务、
自建裸 UDP 服务；两个自建服务的监听 fd 接入 Pingora 的 socket 移交。

**退出条件** —— 一次 `SIGQUIT` + `-u` 优雅升级过程中，三类连接**零中断**。

> ★ QUIC **有意**不在 M0 内（G27/G28）。风险本体是「非 Pingora 托管的监听器能否参与 fd 移交」，
> 与监听器上面跑什么协议无关。把 QUIC 排除在外，失败时才能立刻分清是接缝的问题还是 QUIC 库的问题。

✅ 已达成：第二代进程继承了两个自建监听器的 fd（fd 编号在两代间不同，正说明是经 `SCM_RIGHTS` 传过来的）；
七次独立运行全绿。⇒ §5.1 第 3 条的风险就此解除。

### M1 · 接管一台机 ✅ **通过**

**范围** —— DSL → 结构化配置 → 运行时全链路 · 反向代理 + 基础负载均衡 · 自动 HTTPS ·
DNS 定期重解析 · 主动健康检查 · 全量原子 load · 未实现能力回落 · 产品二进制由 systemd 托管。

**退出条件**（G73）—— 一台真实服务器由枢衡承载，上线跑压力冒烟开测。

✅ 全部满足。★ 附带推翻了 G31 的一半：撑住 systemd unit 的是 `ExitType=cgroup`，
而「抢过 MainPID」会悄悄弄丢优雅停机（口径已由 **G37** 修订）。证据见
[`docs/verification/m1-systemd.md`](docs/verification/m1-systemd.md)。

⚠ **`passive_fail`（被动熔断）不在 M1 清单上，至今没做。** 主动健康检查也**不是**它：
一个「`/health` 回 200 而真实业务在 500」的上游，主动检查看不出来。

### M2 · 接管两台（★ 由 owner 从「全部五台」改为两台，**G81**）⏳ **进行中**

**范围** —— L4 面、静态文件、磁盘缓存、HTTP/3 依次自研上线并逐块拆除回落 ·
Runtime 增量通道 + 临时覆盖层 · 观测三件套（Prometheus 指标、结构化访问日志、Runtime 实时 stats）。

**退出条件**（G81）—— **两台**服务器全部由枢衡承载；回落层已无常态使用。

✅ **自研批次全部做完**

| 批 | 内容 |
|---|---|
| A / B / C | L4 的 TCP 透传 · UDP 透传（按客户端地址维护会话）· SNI/ALPN 分流 |
| D | PROXY protocol 的 **L4 那半边**（`tcp` 块里「收」与「发」两条独立指令） |
| F | 静态文件：`file_server` 从「回落 nginx」变成自研 |
| G / H | HTTP 缓存：语义层 + 内存后端 + 防惊群 + `POST /purge`（G）· 磁盘后端（H）。⇒ **回落层在批 G 里整块归零**，比 §6.3 原本写的「1.0 时」提前 |
| I | 响应压缩（G99–G102） |
| J | HTTP/3 入口本体：quiche + Retry 地址验证 + `quiche::h3` 事件循环 + QUIC 监听器 `Service` 与 socket 移交。★ **SCID 从第一天就带 `gen_id` 前缀** —— CID 的形状是对外可见的，等批 K 再改会让批 J 发出去的连接在换代时全部认不出来 |
| K | 换代时的 **QUIC 跨进程转交**（G109）。⇒ **换代零中断从此对 h3 也成立** |
| L | 观测 ①：**结构化访问日志**（JSON，G113/G114）+ PROXY protocol 的 **HTTP 半边（只做「收」）** + 白名单头 + TLS 信息 |
| M | ✅ 观测 ②：**Prometheus 指标**。站点块里的终结指令 `metrics`（G116）· 文本格式自研零依赖（G117）· `no_site_match` 计数器结掉 D26（G118）· 站点标签取「实际匹配到的那条地址字面量」（G121）。取数点只有 `Record::finish` 一处，门在 [`tests/metrics/run.sh`](tests/metrics/run.sh) |
| N | ✅ 观测 ③ + G8/G18 的另一半：**增量通道 → 临时覆盖层 → 只读 stats**，按这个顺序（G119）。`POST /load` 的 `overrides` 是必填参数（G120）· `reverse_proxy` 的选填 `id` 与同键共享格子（G125）· 指标族 `fulcrum_overrides_active`（G126）。门在 [`tests/stats/run.sh`](tests/stats/run.sh) 与 `tests/serve/` `tests/metrics/` 三处 |
| M′ | ✅ **批 M 的收尾**（G122 的 TLS 那半 · G123 拆 `purge` · G124 的 `none` 判据 · 基数表那道门扩到读全行）。★ 三件都落在**已有的取数点**上，不动 fork、不新增监听器钩子 —— 它们结掉的是批 M 自己留下的口径问题。⚠ **新族与 `observability.md` 那张基数表必须同一笔改**：那道把表钉在 `FAMILIES` 上的门会把「表里有、代码里没有」判红，而它现在**逐字读到类型/来处/标签**（不只是族名）。★ G124 那一格顺带量出一条与直觉相反的机制，写在 [`observability.md`](docs/architecture/observability.md) 的 `status_class` 一节。 |
| O | ✅ **连接指标**（G122 的连接那半，D32 就此两半都落地）：`fulcrum_connections_total` / `_active{listen,entrypoint}`。**fork 第 15 处**（`ConnectionCounter` 接缝 + `ConnGuard`）覆盖 h1/h2 **与 admin**，L4 TCP / L4 UDP / QUIC 各自那条循环再接三处 —— 四处记进**同一个**族，抓取时问活体。★ 减一只有 `ConnGuard` 的 `Drop` 一个调用点；⚠ `l4_udp` 那一格数的是**会话**，从 `sessions.len()` 派生而**不是** `+1/-1`。★ 有意排在批 N 之后：批 N 挡在 M2 打勾的路上，而这一格不挡。 |

> ⚠ **批的字母不代表顺序**：批 K 的号早于批 L 写定，而 owner 后来拍 **G112** 把观测插到了
> 批 K 之前。**不改号** —— 改号会让已有引用一起变成错的。

✅ **批 M（Prometheus 指标）已落地** —— `metrics` 终结指令、text exposition 自研零依赖，
门在 [`tests/metrics/run.sh`](tests/metrics/run.sh)。
✅ **批 N（Runtime 增量通道 → 临时覆盖层 → 只读 stats）已落地**（方案见
[观测](docs/architecture/observability.md) 与 [管理面](docs/architecture/control-plane.md)，
决定见 §10 的 G119/G120/G125/G126），门在 [`tests/stats/run.sh`](tests/stats/run.sh)
与 [`tests/serve/run.sh`](tests/serve/run.sh)、[`tests/metrics/run.sh`](tests/metrics/run.sh)。
⚠ **一格里装三件事，最容易被读成一件**；观测这一格今天是 **3/3**。
⛔ **但 M2 仍然不能打勾** —— 剩的那半句是**运维动作**（两台机器由枢衡承载），不是工程任务。

⏳ **退出条件本身要拆成两半数**，因为它们的性质完全不同：

| 半句 | 状态 |
|---|---|
| 「回落层已无常态使用」 | ✅ **G98 把整层删掉的那天就恒真了** —— 层不存在，就不可能有常态使用。⚠ 它此前一直被连着前半句读成「还欠着」|
| 「**两台**服务器全部由枢衡承载」 | ⏳ **这是运维动作，不是工程任务**。它没挡住批 M，也不该挡住批 N，但它是 M2 打勾的**唯一**硬条件 |

⏳ 另外欠着：PROXY protocol 的「发」那半边（至今没有任何 DSL 面）。
★ **Runtime 增量通道 + 临时覆盖层**不再单列 —— 它们就是批 N 的前两步。

### M3 · 对拍达标 ⏳ 未开工

**范围** —— 与 Caddy、HAProxy、Nginx 三家同机同用例对拍。

**退出条件** —— 每类用例不劣于该类最强者 10%；基准脚本与原始数据可被第三方复现。

### M4 · 发布 ⏳ 未开工

**范围** —— 文档、`deb`/`rpm` 包、DSL 参考、`nginx.conf` 与 `Caddyfile` 迁移指南、安全披露流程。

**退出条件** —— 已在生产连续承载 ≥ 90 天，且 M3 达标。

> ⚠ **仓库已经公开了（G115），而这个里程碑没有因此提前。** 「源码看得见」与「可以拿去用」
> 是两件事：现在没有版本号、没有包、没有兼容性承诺。

## 8. 性能验收标准（G19）

- **对象**：Caddy、HAProxy、Nginx 三家全部对拍，不挑软柿子
- **方法**：同一台机、同一组用例、同一负载生成器；用例按类划分（静态吞吐、反代吞吐、TLS 握手、L4 转发、缓存命中、长连接、高并发短连接）
- **门槛**：每类用例，枢衡不劣于**该类最强者** 10%
- **公开**：基准脚本与**原始数据**全部公开。不发布无法复现的性能声明，不使用"快 N 倍"式表述

## 9. 主要风险

| 风险 | 缓解 |
|---|---|
| ~~升级窗口内两代进程共享同一个 UDP socket，数据报会被分流~~ | ✅ **已解除**（M2 批 K）：按 DCID 判归属，不属于本代的数据报经一条 unix datagram 通道转交给它那一代（G109） |
| ⏳ ★ **仓库现有的构建镜像编不出 musl 产物**（实测：Debian trixie 没有 `musl-g++`，也没有任何 musl 交叉包）| 探针用的是**第二张钉死的基础镜像**（Alpine，原生 musl），aarch64 那一趟走 qemu。★ **不挡 HTTP/3 开工**——产品的发布流水线本身还不存在（打包是 M4）。⏳ 待拍：（a）Alpine 原生 + qemu 跑 aarch64，（b）glibc 宿主 + musl 交叉工具链（zig / musl-cross-make），两个架构都快但给供应链新增一整套 C/C++ 交叉编译器 |
| ~~回落层代码 1.0 时全删，G25 使其工程量翻倍~~ | ✅ **已解除**：三个用户全部自研完成，整层于 M2 批 G 删除（G98）|
| ~~门禁在 GitHub Actions 上间歇性红（ACME 场景「起不来」）~~ | ✅ **已定位并修掉**（2026-08-28）。根因**两层**：① `tests/quic-relay/run.sh` 写成 `GEN2=$(start_gen …)`，`$(…)` 是子 shell ⇒ `PIDS+=` 改的是副本、`cleanup` 一个进程都没收到，**而该场景照常 PASSED**；泄漏的第二代攥着**合成出来的 `:80`**（见 AGENTS.md 端口表）活到下一个场景。② pingora 的 `ListenerEndpoint::listen` **攥着全局 `ListenFds` 锁**去做 `bind()`，而 `bind_tcp` 要重试 30 次 × 1 秒 ⇒ **一个被占的端口把整把锁停住 30 秒，所有还没拿到锁的监听器一起起不来** —— 于是现场报的是 `:8083` 起不来，日志里却只有 `:80` 的错。⇒ **报出来的端口不是出事的端口**，而这正是它三个月读不出来的原因。★ 处置：源头修好（pid 登记 + `cleanup` 里加一条「用过的端口要还回去」的自证，两个方向都实测过），取证留在两个 ACME 场景的失败路径上。★ ★ **③ 顺带查出来的第三层**：lint 那一格的 `shellcheck` 扫的是一张**手写目录清单**，而它漏掉的正好是 `tests/quic-relay/`（18 项 vs 仓库里 19 个含 `.sh` 的目录）⇒ **本仓库唯一起两代产品进程的那一格，从来没被静态检查看过**，底下压着 4 条真实告警。已改成**推导 + 每次自证**（[`tests/ci/shellcheck-all.sh`](tests/ci/shellcheck-all.sh)，三条反证都实测过）。⚠ **但不要把它记成「本来能拦住 ①」** —— 实测 shellcheck 0.10 的 SC2030/SC2031 只认**写在子 shell 那几行里**的赋值（`( B=1 )`、`X=$(D=9; …)`），对「函数体里 `PIDS+=`、调用方 `$(f)`」**一声不吭**。⚠ 由此作废两条旧结论：**「占着 `:80` 无害」**（那是一次落在良性锁序上的采样）与**「每个 Service 各有 runtime ⇒ 互不拖累」**（各有 runtime，但共用那把锁）。★★ ② **上游 `main` 已经修掉了，不必投**：提交 `1d9371191`（2026-03-25）把 `ListenFds` 换成 `parking_lot::Mutex`，因为同步锁不能跨 `bind().await` 持有，顺势引入**按地址**的异步锁 —— 我们那行 `// consider make this mutex std::sync::Mutex or OnceCell` 正是被它删掉的。⚠ **但它只在 `main` 上**：上游最新 release 仍是 **0.8.1**（＝我们 vendor 的那个），`0.8.1 → main` 相差 190 个提交 ⇒ **本仓要等下一次 rebase 才拿得到**，已登记在 [尚未验证的接缝](docs/verification/open-seams.md)。⚠ 上游的修法**新增了 `flurry` 依赖**，所以「现在就 backport 进 fork」不是顺手的事（供应链门要过），而触发者已经修掉 ⇒ 放大器目前是休眠的。 |
| 单人 + AI 承担网络关键路径软件的安全责任 | 内存安全交给 Rust；协议栈交给成熟库；不自己实现 TLS、HTTP/2 状态机、HPACK、QUIC |
| 磁盘缓存的崩溃恢复是经典难题 | 先做内存层与只读缓存，磁盘后端最后做；原子写 + 启动时校验 + 校验失败即丢弃该条目 |
| On-Demand TLS 即使有准入控制仍可能被内部误用刷爆 | 准入配置强制（G15）+ 速率上限 + 签发失败进入退避 + 指标告警 |
| 性能对拍环境不可复现，数字失去意义 | 基准环境用脚本固化，含内核参数与 CPU 亲和；每次对拍记录完整环境快照 |
| Pingora 上游演进与枢衡定制的合并成本 | ★ **这已经不是「风险」而是「进行中的成本」**：枢衡确实 fork 了 `pingora-core`（`vendor/pingora/`）。缓解是——改动**意图上限定为版本上界 + 随之而来的调用点适配**；`tools/dep-check.py` **自动盯上游的发版与 main 分支**（patch 过去的 crate 脱离了 `cargo update` 的视野）；每次 rebase 后跑**回归网 + M0**，M3 之后跑全量对拍。<br>★ ★ 「不引入行为变更」这条当初是**断言**而不是**判据**，而它已经失败过一次（详见 `vendor/pingora/FORK.md`）。现在由 [`tests/vendor/run.sh`](tests/vendor/run.sh)（上游自带的 340 条单元测试）看着 |
| 三家的许可证义务与打包方式 | 产物不链接三家中的任何一家（回落层已删除，见 §6.3）。发布前需正式审查 |

## 10. 决策清单

> 本节是**结论表**：每一行是一条**仍然生效**的决定。★ 标的是 owner 推翻了 AI 推荐项。
> ⚠ **编号只增不减** —— 代码注释、文档与判据里有上千处按号引用。
> ⛔ 被撤销的条目保留原号并标注，因为「当初为什么那样定」本身仍是有效信息。

| 编号 | 决策 |
|---|---|
| **G1** | **实现语言 = Rust。** 性能可与 nginx/haproxy 同级（Go 做不到，而那正是 Caddy 的短处）；内存安全避开 nginx/haproxy CVE 史上的缓冲区类问题；`tokio` / `hyper` / `rustls` / `quinn` / `instant-acme` 生态齐全。 |
| **G2** | **三家产品的角色 = 混合数据面。** 枢衡自研进程做入口，未实现的能力回落给成熟引擎，实现一块接管一块。第一天就是完整可用产品，回落路径天然是灰度开关与 A/B 对拍台。 |
| **G3** | **首版切片 = 三合一全量首版。**（AI 曾评 3.0 分并建议只做「反代 + 自动 HTTPS + 基础 LB」。）配合 G2 与 G5 后成立——"全量"指对外能力面，不是自研代码面。 |
| **G4** | **配置形态 = 自研 DSL（Caddyfile 式）。** |
| **G5** | **「三合一全量」口径 = 对外能力面全量，自研逐块吞并。** 用户看到完整三合一，内部自研哪块算哪块，其余回落。 |
| **G6** | **数据面底座 = Pingora 0.8 打底 + 独立 QUIC 入口。** 附带三条硬约束见 §5.1。★ 原文写的是 `quinn`/`h3` + rustls，已由 **G103 / G104** 改成 `quiche` + BoringSSL。 |
| **G7** | **首版自研范围 = L4 面 + 静态文件 + 磁盘缓存后端 + HTTP/3，四块全自研。** |
| **G8** | **热重载模型 = 全量原子 load + 增量 Runtime 双通道。** 直取两家长处：全量 load 保证"整份配置即唯一事实"与原子回退（Caddy），增量 runtime 用于改权重、摘节点、清缓存且零 reload（HAProxy）。 |
| **G9** | **回落定位 = 时间轴上的临时顶班。** 某块自研未完成时回落，完成即拆除，**1.0 时回落代码归零**。 |
| **G10** | **发布模式 = GPL-3.0 开源**，判据在 M4 退出条件中写死。★ 「私有开发到首个能跑生产的版本」那一半已由 **G115** 改写：仓库提前公开，而**发布**仍按 M4 的退出条件走。 |
| **G11** | **配置分层 = DSL 编译到结构化配置，二者都是合法入口；结构化那份是唯一内部事实。** diff、版本化、原子回滚全建在它上面；DSL 可随时演进而不动内核。 |
| **G12** | **自动 HTTPS = 默认全自动 + 支持 On-Demand TLS。** |
| **G13** | **平台分发 = Linux x86_64 + aarch64 单静态二进制（musl）+ systemd 单元 + 官方 deb/rpm；不做官方容器镜像。**（AI 建议先做容器镜像后做包。）因是单静态二进制，文档给一份 `FROM scratch` Dockerfile 即可替代官方镜像。 |
| **G14** | **管理面安全 = 默认只绑 Unix socket（文件权限控制）；远程管理需显式开 mTLS。** 直接堵掉 Caddy「Admin API 默认绑回环且无认证、同机任意进程可改配置」这个短处。 |
| **G15** | **On-Demand TLS 准入 = 强制。** 未配 ask 端点或域名白名单则**拒绝启用**——错误在启动时暴露，而不是在被刷爆时才发现。 |
| **G16** | **观测基线 = Prometheus 指标 + 结构化访问日志 + Runtime 实时 stats。** Runtime 通道 G8 反正要做，在其上加只读 stats 是边际成本最低的高价值功能。 |
| **G17** | **上游发现 = 静态列表 + DNS 定期重解析 + 主动健康检查 + 被动熔断。** DNS 重解析直接消灭 nginx OSS「上游域名只在启动时解析一次」这个经典事故源。 |
| **G18** | **Runtime 改动归宿 = 显式临时覆盖层。** 不持久化，但 stats 与 API 永远显示「当前有 N 项临时覆盖生效中」。 |
| **G19** | **性能验收 = 三家全对拍，逐类设门（不劣于该类最强者 10%），脚本与原始数据全公开。** 详见 §8。 |
| **G20** | **DSL 风格 = Caddyfile 式（站点地址起头 + 大括号块）。** |
| **G21** | **产品名 = 枢衡 / Fulcrum。** |
| **G22** | **用户分层。** 第一类：**自己的机队 + 自托管/homelab 站长**（并列主打）。 |
| **G23** | **首版不做 Web UI**，只有 DSL + CLI + API。G16 的 Prometheus 指标接 Grafana 已覆盖可视化需求；nginx、HAProxy、Caddy 三家都没有官方 UI。 |
| **G24** | **里程碑按「能自用」切分。** 见 §7。 |
| **G25** | **回落范围 = Caddy + Nginx 两家**（静态与缓存交给 Nginx，其余交给 Caddy）。 |
| **G26** | **M0 构建与验证环境 = Docker Desktop（官方 rust 镜像）。** |
| **G27** | **M0 验证范围收窄 = 裸 UDP + 裸 TCP 服务验 fd 移交接缝，QUIC 推后。** 理由见 §7 M0 下方引文。 |
| **G28** | **HTTP/3 库选型推迟到 M2**（登记为 D11）。★ 已由 **G103** 结案。 |
| **G29** | **（D1 结案）依赖版本策略 = 追新 + 24 小时安全怀疑期。** |
| **G30** | **依赖天花板的处置 = fork `pingora-core` 放宽版本上界。** |
| **G31** | **进程模型 = systemd `Type=notify` 前台运行；`daemonize` 依赖删除。★ 但先做 spike 验证。** |
| **G32** | **fork 长期维护 = 把有公告支撑的上界推 PR 给上游，其余常年 rebase。** |
| **G33** | **（D5 结案）路径约定 = systemd 托管为主，但每一项都可被配置覆盖。** |
| **G34** | **（D6 结案）回落层进程管理 = 复用既有 systemd 单元 + 在枢衡的 unit 里声明依赖。** |
| **G35** | **并发与线程模型 = 保持 `work_stealing = true`；线程数按 service 角色定默认、配置可覆盖；QUIC 那部分随 D11 推到 M2。** |
| **G36** | **构建镜像的升级口径 = 并入 `dep-check.py` 的每周检查；只在 rustc 版本变了时判红；只报告、拒绝自动应用；M3 对拍期间冻结。** |
| **G37** | **（D14 结案）进程模型 = `Type=notify` + 前台 + `ExitType=cgroup`，★ 不交接 MainPID。** |
| **G38** | **`get_fds_from()` 的两处 fd 泄漏 = 先在 fork 里修，并备好上游 PR 材料由 owner 提交。** |
| **G39** | **systemd 测试宿主镜像的升级口径 = 并入 `dep-check.py` 的每周检查，判 systemd 大版本。** |
| **G40** | **上游投稿改为「owner 授权后可代发」，身份与复核要求不变。** |
| **G41** | **`pingora-rustls` 的两处清单缺陷在 fork 里修掉，并备上游投稿三。** |
| **G42** | **回归网覆盖 rustls，且把测试环境修对而不是把已知失败名单加长。** |
| **G43** | **`FORK.md` 的核对命令覆盖面补齐（顺手逮到的第三条）。** |
| **G44** | **拼出来的门禁命令要加分组。** `lint && build --locked || build && …` 的运算符优先级让 lint 的退出码被吞掉，门红着而退出码一直是 0。 |
| **G45** | **（承 G41）`rustls-pemfile` 迁到 `rustls-pki-types` 的 `PemObject`。** |
| **G46** | **（修订 G32／G40 的发前检查）「查过上游做没做」必须包含未合并的 issue 与 PR。** |
| **G47** | **（D3 第一刀）DSL 里遇到本版尚未自研的能力 = 隐式回落，不写 `fallback` 指令。** |
| **G48** | **（D3 第二刀，补一处规格真空）结构化配置的落地格式 = JSON。** |
| **G49** | **站点块内的执行顺序 = 内建顺序表（照 Caddy），不是书写顺序。** |
| **G50** | **matcher = 命名 matcher（`@name`）+ 仅路径的行内简写。** |
| **G51** | **报错策略 = 一次报全 + 稳定错误码。** |
| **G52** | **隐式回落的可见性 = 启动/装载日志逐条列出。** |
| **G53** | **ACME 客户端 = `instant-acme`，上层自己搭。** |
| **G54** | **挑战类型 = TLS-ALPN-01 主 + HTTP-01 备 + DNS-01。** |
| **G55** | **证书存储 = 每域一目录的 PEM 文件。** |
| **G56** | **续期 = ARI 优先，回退到「剩余寿命 1/3」；失败走指数退避。** |
| **G57** | **DNS-01 = 原生 Cloudflare + DNSPod，其余走 exec hook 兜底。** |
| **G58** | **通配符证书是 M1 的交付内容，不推到 M2。** ★ 附带一条硬约束：DNS-01 必须轮询权威 NS 确认 TXT 已可见，**不许固定 sleep**。 |
| **G59** | **DNS 凭据 = 只从文件或环境变量读；能程序化校验权限范围的，在启动时校验并拒绝启动。** 校验不了的供应商，强制在配置里声明该凭据覆盖哪些 zone。 |
| **G60** | **M1 的指令集边界 = 首版对外能力面全覆盖。** |
| **G61** | **占位符 = 小而固定的一组，无表达式、无函数、无条件。** |
| **G62** | **M1 只认一份配置文件，不做 `import` / `conf.d`。** |
| **G63** | **默认错误响应 = 内置默认 + `handle_errors` 可覆盖。** |
| **G64** | **门禁里跑 pebble 当本地 ACME CA。** |
| **G65** | **续期状态与证书本身解耦：没有证书的域名也要记得自己失败过几次。** |
| **G66** | **站点索引的通配符收紧到「只吃一层」，与证书侧一致；且两侧共用同一份实现。** |
| **G67** | **投稿四（`test_connect_uds` 的短读）已发出。** |
| **G68** | **投稿五（`with_cert_resolver`）撤销，不发。** |
| **G69** | **DNS 客户端自己写，不拉 crate —— 而这个决定是量出来的。** |
| **G70** | **续期单独立一个验证场景**（[`tests/acme/renew.sh`](tests/acme/renew.sh)）。 |
| **G71** | **TLS-ALPN-01 接线（G54 的「主」，RFC 8737）。** |
| **G72** | **原生两家接线，而 HTTP 客户端新增 0 个包。** |
| **G73** | **M1 退出条件去掉「连续运行 7 天无事故」，改为「上线跑压力冒烟开测」。** |
| **G74** | **加一个强制续期的口子**（`POST /renew`）。★ 它不越过退避，只越过「还不到时候」这条判定。 |
| **G75** | **管理面落地**：`admin unix/<路径>`，权限 0600，两条命令 —— `POST /load`（G8 的全量原子 load）与 `POST /renew`（G74）。没配 `admin` 就一个 socket 都不建；写成端口形态直接拒绝启动。 |
| **G76** | **`dns_refresh` 落地**：上游域名的解析结果存进 `Upstream` 的槽里并定期重解析。 |
| **G77** | **`health_uri` 落地**（主动健康检查）。 |
| **G78** | **产品二进制由 systemd 托管落地**（G31/G33/G37 的产品侧）。 |
| **G79** | **回落层的转发侧接线**：`file_server` / `cache` 真的转发给 nginx（此前一律回 501）。⛔ 整层已由 **G98** 删除。 |
| **G80** | **M1 退出条件的真域名改为生产主域名**（含 `www`）。★ 后续被 G85 / G86 两次改写。 |
| **G81** | **M2 的退出条件从「五台服务器全部由枢衡承载」改为「两台」。** |
| **G82** | **HTTP 缓存整层自研**，`pingora-cache` 不进 fork（用现成要把 8707 行 + 7 个新包吃进 fork）。 |
| **G83** | **磁盘缓存布局：两级分片目录 + 每条目 meta/body 两文件 + 写 `tmp` 后 `rename` 落地。** ★ meta 是一族（一个主键）一个，body 是一条一个。 |
| **G84** | **磁盘缓存崩溃恢复：启动不扫盘 —— 读时校验 + 后台渐进重建；`purge` 走管理面。** ⚠ 代价写在明处：刚重启时少算占用、淘汰偏晚 —— **只影响淘汰**。 |
| **G85** | **生产域名第一次更换**（owner 拍板）。⛔ 已被 **G86** 取代。 |
| **G86** | **生产域名第二次更换**（owner 拍板，同日）。 |
| **G87** | **静态文件 · 符号链接：默认跟随，另加一条可关的子指令。**（★ **推翻 AI 推荐项**「只跟随、不给开关」） |
| **G88** | **隐藏文件不按 `.` 一刀切**，改成一张可配置的默认 404 清单。 |
| **G89** | **静态文件这一批做到哪：全做。** 发文件 · 目录索引（`index`，缺省 `index.html`）· 目录不带尾斜杠 **301** 到带斜杠 · 范围请求 · 条件请求 · 预压缩旁文件。 |
| **G90** | **静态文件 · Content-Type：自带一张小表，新增 0 个包。** 约 60 条常见扩展名 + 缺省 `application/octet-stream`。 |
| **G91** | **`root` 必填，且必须是绝对路径。** |
| **G92** | **`Owner` 加一档 `SelfBuiltM2`**（印「M2 自研」），文档列头 `M1 归属` 改为 `归属`。 |
| **G93** | **`If-Modified-Since` 三种格式都解析**，RFC 850 的两位年按 RFC 9110 附录规则处理，并由单测钉住。★ 推荐项只到「三种都做」。 |
| **G94** | **CI workflow 里的 shell 搬进脚本**：`dumpcache` 那段搬成 [`tests/ci/dump-cache.sh`](tests/ci/dump-cache.sh)，与本批一起做。 |
| **G95** | **缓存切成两批**：语义层 + 内存后端 + `purge` + 防惊群，然后磁盘后端。 |
| **G96** | **`ttl` 是兜底不是覆盖**：只有上游没给新鲜度时才用它。 |
| **G97** | **缓存的 RFC 覆盖面 = 最小集 + 上游响应头里的全套 `Cache-Control` 指令。** ★ 推荐项只到「最小集」。 |
| **G98** | **回落层这一批整块删掉。** ★ 推荐项是「保留指令、1.0 时才删」。 |
| **G99** | **下一批 = `encode` 压缩 + 预压缩旁文件。** |
| **G100** | **压缩用现成的**（fork 里已有那份），自研换不来更小的依赖图，新增 0 个包。 |
| **G101** | **压缩与缓存的先后 = 压完再存 + 次级键归一化。** |
| **G102** | **压缩后的 ETag 弱化成 `W/"…"`**（照 nginx gzip filter）。 |
| **G103** | **D11 结案：HTTP/3 取 `quiche`。** ★ AI 推荐的是 `quinn` + `h3`。 |
| **G104** | **TLS 栈统一到 BoringSSL**：把 fork 当初删掉的 `pingora-boringssl` 加回来，h1/h2 与 h3 共用同一套。★ AI 摆的两条里，「两套并存」才是改动面更小的那条。⇒ 推翻 §5.1 第 1 条原来的 rustls 口径。 |
| **G105** | **HTTP/3 语义层用 `quiche::h3`，不自研。** |
| **G106** | ~~owner 在宿主机装 Rust。~~ ⛔ **已被 G107 整条撤销**。 |
| **G107** | **Rust 一律在 Docker 里跑。** ⛔ 整条撤销 G106；本机跑 Rust 的决定、指引与全部构建产物一并清除。判据不变：`bash tests/m0/docker-run.sh`。 |
| **G108** | **D22 的判据换成产品自证**：新增场景 [`tests/musl/product.sh`](tests/musl/product.sh)，用产品本体编 musl 静态产物并在 `FROM scratch` 里跑一次 `validate`；`tests/musl/probe.sh` 留在门外当历史记录。 |
| **G109** | **升级窗口内 QUIC 连接归属：按 DCID 跨进程转交**（★ **推翻 AI 推荐项**「停收 + GOAWAY」）。理由与 §7 「换代时长连接不断」一致 —— TCP 那侧做得到，h3 就不该做不到。★ 形状：`<run_dir>/quic-relay-<gen_id_hex>.sock`（`SOCK_DGRAM`）· 报文 `[from][to][原始数据报]` · 单向 · 对方已退出即丢弃 · **只走一跳**。 |
| **G110** | **HTTP/3 跟着 `tls` 自动开，并发 `Alt-Svc`**（D25）：有 TLS 的站点自动在同端口听 UDP，h1/h2 响应加 `Alt-Svc: h3=":443"`。★ 推翻 AI 推荐项。 |
| **G111** | **HMAC / AEAD 由 `fulcrum-server` 直接依赖 `boring`**，破例于「BoringSSL 类型只经 `pingora-boringssl` 拿」那条纪律。★ 推翻 AI 推荐项。 |
| **G112** | **观测先于换代转交**：批 L（访问日志 + PROXY protocol 的 HTTP 半边）排在批 K 之前。 |
| **G113** | 访问日志格式：**JSON 单一格式，字段扁平**（不取 logfmt，也不取「两种可选」）。 |
| **G114** | **访问日志的字段清单**（★ **推翻 AI 推荐项**）。 |
| **G115** | **仓库提前转为公开**，并把当前树作为**第一个提交**（此前的提交历史移除）。⚠ **「源码公开」不是「发布」** —— M4 的范围与退出条件不变（还没有版本号、没有包、没有兼容性承诺）。★ 连带：`handoff/` 进 `.gitignore` 不再入库；文档与注释只留当前结论，不留过程记录。 |
| **G116** | **Prometheus 端点 = 站点块里的终结指令 `metrics`**（执行顺序表 Terminal，序号 75）。⇒ 访问控制（`remote_ip` 匹配器）、TLS、访问日志全部复用现有机制，**不新增监听器、不新增认证体系，G14「管理面只绑 Unix socket」的口径一个字不动** —— 指标面根本不属于管理面。⛔ 不挂 admin socket 当唯一出口：Prometheus 抓不了 Unix socket，那样用户必须再装一个 exporter，直接撞设计原则 1。⚠ 代价：指标与业务共用监听器，**matcher 写错就会把指标暴露出去**，只能靠文档与诊断兜。 |
| **G117** | **指标的 text exposition 格式自研，零新依赖。** 它是行式纯文本、**不是安全敏感协议栈**，不撞安全基线第 5 条（那条管 TLS/HPACK/QUIC）。⇒ 供应链门不用动，musl 静态产物不受影响。⚠ 代价：直方图分桶、标签值转义、`_total` 后缀要自己钉住，由单测守。 |
| **G118** | **D26 结案 = 给 `no_site_match` 一个计数器**，而不是回答「全局 `log` 与站点 `log` 是覆盖还是合并」。`host` 标签**只有出现在配置里的才带真值**，其余归 `host="<other>"` ⇒ 上界由配置定、不由访问者定。⚠ 代价写在明处：**只知道有多少、来自哪个已知 host，不知道具体是哪个未知 host** —— 有意如此，不让外人往我们的时序库里写任意字符串。 |
| **G119** | **批 N 的顺序 = G8 增量通道 → G18 临时覆盖层 → 只读 stats。** ★ owner 拍的是这个顺序（AI 推荐的是「先只读 stats、写通道留后」）⇒ stats **从第一天就带 `overrides` 一节**，不存在「先发一个没有覆盖层的 stats」这种中间形态。 |
| **G120** | **`POST /load` 的 `overrides` 是必填参数**（`keep` / `clear`），**缺了就拒绝**。★ 不给默认值是这条的全部内容：发布流水线要 `clear`（发布＝回到期望状态），事故处理中的人要 `keep`（一次无关的发布不该把刚摘掉的坏节点放回去）—— 两种现实都正确且互相冲突，**任何默认值都会在另一种场景里悄悄做错事**。与「改了监听端口就显式拒绝」同一条纪律：判据在拒绝上，不在尽力而为上。⚠ `clear` 那一档必须在回话里**逐项列出**被清掉的覆盖 —— §3 点名要避开 HAProxy 那个「runtime 改动 reload 后无声消失」。 |
| **G121** | **`fulcrum_requests_total` 的站点标签 = 请求实际匹配到的那条地址字面量**（`a.example` / `*.wild.example`，通配符折叠成自己的字面量）。⚠ **不能用 `host`**：通配符站点下 host 由请求方决定，一个 `*.example` 就能让 series 无限增长 —— ★「已命中站点的请求 host 总是有界的」是错的，而它错得很像对的。⚠ 也不取「站点块的第一个地址」：那会把同一块里的 `a.example` 与 `b.example` 混成一格，且**改地址书写顺序会让时序断裂**。★ 代价：通配符站点下各子域名合并成一格，「哪个租户在打我」留给访问日志答（它有真 host）。 |
| **G122** | **D32 结案 = 两半分别落地，且分别命名。** ① **TLS 那半**：新增 `fulcrum_tls_requests_total{version,cipher}`，取数点仍是 `Record::finish` **那一处**（`Record` 已经带着 `TlsFields`）。★ 两个标签都由**服务端协商**产生 ⇒ 上界由我们编进去的套件表定、不由访问者定，正好过 G118/G121 那条纪律；⛔ `sni` / `alpn` **不当标签**（访问者给的，与 G121 挡的是同一件事）。⚠ 名字里是 `requests` 不是 `handshakes` —— keep-alive 下一次握手对应多条请求，叫 `handshakes` 会是一句读起来完全成立的假话。② **连接那半**：`fulcrum_connections_total`（counter）+ `fulcrum_connections_active`（gauge），标签 `listen`（监听地址，上界由配置定）。⚠ ⚠ **它是四处，不是一处**：fork 第 15 处改动（`Service::run_endpoint`）覆盖 h1/h2 与 admin，而 L4 TCP · L4 UDP · QUIC **各有各的 accept 循环**，pingora 那个一次都不经过。★★★ gauge 的减一**必须用 `Drop` 守卫** —— 那个连接任务有三条退出路径（握手超时 / 握手失败 / 正常结束），手写三处 `fetch_sub` 正是 D18/G66 那个分家形状。★ 四处只负责 `+1/-1` 到**同一个** `ConnStats`，而它与标签定义只有一份。⚠ fork 够不到我们的 `metrics` 模块 ⇒ 只能由 fork 暴露计数、我们**抓取时问活体**（`upstream_inflight` 那条路子）。★ 顺序：**现在就在 fork 里做，不等 rebase**（改动是孤立新增，冲突面小；而投递或等待都不改变我们什么时候用得上 —— `ListenFds` 那条已证）；**投不投上游等 rebase 读过 `main` 之后再判** —— 上游 `main` 已把 `prometheus` 整条删掉，口味未知。 |
| **G123** | **D31 结案 = 把 `purge` 拆成自己的族。** 新增 `fulcrum_cache_purged_entries_total`（单位「条目」），`fulcrum_cache_events_total{event}` 只剩 `hit` / `miss` / `stale` **三个同一分母的值** ⇒ `sum(cache_events_total)` 恢复成「查过缓存的请求数」，命中率直接可写。★ 这是**把一个靠用户记住的陷阱，换成一个结构上不存在的陷阱**。⚠ 「是哪一种命中」（`HIT` / `HIT-DISK`）**不进指标**：`CacheHandle` 全仓只建一次，`Backend` 是每进程一个、由配置定死的 ⇒ 那个区分在一个运行中的进程里**是常量**，做成标签携带的信息量恰好为零，只剩基数成本；要知道后端是哪个，装载日志那一行就说了。⚠ 另一半边界**保持现状并写在明处**：`miss` 记在**回源**那一处（有意不记在「查缓存没命中」那一处 —— 那里还会拐弯：`only-if-cached` 回 504 根本没回源、防惊群的 follower 等完重查会**命中**）⇒ 上游连不上的请求三格都不在，故障期间 `hit/(hit+miss)` 会偏高；★ 那时 `upstream_healthy` 与 `requests_total{outcome="error"}` 是两个更直接的信号。 |
| **G124** | **D30 结案 = `status_class` 保持六个值，第六个是 `none`，并补一条判据。** `status == 0`（一个响应头都没写出去 —— 那不是「未知」，是**什么都没发生**）与 1–99 / 600+ 一起归 `none`。★★ 它**可达**这件事现在有实据：`response_written` 只在 `write_all` **成功之后**才被置上（`v1/server.rs`）⇒ 下游在我们写响应头之前断开时，站点已命中、`outcome` 是 `reverse_proxy` 之类，而 `status` 是 0，**且 `LogLevel::All.records(0)` 为真 ⇒ 这一行照样会被写进访问日志**。⛔ 因此「这类请求不计进 `requests_total`」当场破掉一致性门（日志有那一行、指标没那一笔）；⛔ 也不取 `0xx` —— HTTP 里没有这个类，它会被当成真的状态类去查规范。⚠ 而这条路**今天没有任何判据走过** ⇒ 本条同时要求在 `tests/metrics/run.sh` 里造一次「发完请求立刻关连接」，两边一起断言：访问日志真的多了一行 `status=0`，且指标里 `status_class="none"` 正好 +1。★ 否则这一格只存在于代码与文档里。 |
| **G125** | **`reverse_proxy` 的选填 `id`，与覆盖层的三格键 `(站点名, id, 上游地址)`。**★ owner **两轮**拍板：先拍「发明 `id` + 同站点歧义就拒绝」，实测后**重拍为「保留 `id`，但撞键不拒绝 ——同键共享同一个覆盖格子」**。⇒ 一次 `disable` 把共享那一格的几条 `reverse_proxy` 一起摘掉，⛔ **不是错误、也不给告警**：「一个后端挂在几组 `handle` 路由后面」是反代最常见的写法，**现有配置一个字节都不用改**；而「一起摘掉」多半正是要的语义（那台机器坏了，它不该从任何路由收流量）。**想分开就给其中一条写一行 `id`**。⚠ ⚠ **键里的上游地址是归一化之后的那个串**（`backend` ⇒ `backend:80`）⇒ 两条写法不同、归一化后相同的 `reverse_proxy` **共享同一格** —— 这不是缺陷而是边界：管理面对着的是运行时。⚠ 「这一格管着几条」由 `GET /stats` 显示（G18），那是「以为 `disable` 只影响一条」唯一的提醒。⛔ **不自动派生 `id`**：内容哈希会让「发布时加一台机器」把刚摘掉的坏节点顶悬空，站点内序号会让「换一下书写顺序」静默改掉寻址 —— 与 `weight` 拒绝位置式是同一条理由。|
| **G126** | **新增指标族 `fulcrum_overrides_active`（无标签单值 gauge，基数恒为 1）。**★ owner **推翻**了 AI 的建议（AI 选的是「不加，只从 `/stats` 出」）。值是**当前生效中的临时覆盖总数**，⚠ ⚠ **悬空的照样计入** —— 它确实还在登记处占着一格，且 `/load` 已经逐条点过名。⛔ **不按 `(站点, 上游)` 打标签**：那等于把 `/stats` 的 `overrides` 一节整个搬进指标（两份实现同一件事），而「是哪几项」本来就该去 `/stats` 看。★ 取数与 `/stats` **同源**（同一个 `OverrideLayer::entries()`），⛔ 不新增第二处计数；抓取路径上只许用**已持快照**的那一版（`override_entries_of`），因为 `override_counts()` 内部会再取一次快照，而一次抓取取两份快照会让两个族落在两份不同的配置上。⚠ **新族与 `observability.md` 那张基数表必须同一笔改**：把表钉在 `FAMILIES` 上的那道门会判红 ——★ 但它**只比族名**，`gauge / 活体 / 无标签 / 恒为 1` 那四格今天没有任何门守着。|
| **G127** | **指标的标签值取不到时记 `<unknown>`，⛔ 不记空串。** ★ 判据落在「**取到的值为空**」上，⛔ **不落在「是不是 h3」上** —— 后者把一个今天成立的巧合钉进代码：哪天别的传输也读不出套件，它会走到分支外面记一个空串，**而那时不会有任何东西红**。`fulcrum_tls_requests_total` 的 `version` 与 `cipher` 两格**同一条规则**，⛔ 不给 `cipher` 开特例。**决定性依据在 Prometheus 数据模型**：空标签值与「这个标签不存在」被视为等价 ⇒ ⚠ ⚠ 「今天记 `cipher=""`」与「明天把 `cipher` 这个标签整个删掉」在抓取端**分不开**；且 `cipher=""` 正是 PromQL 里「该标签不存在」的惯用写法，运维照着写出来的过滤器会命中一批他没打算命中的 series。★ 记号形状随 G118 的 `<other>`（尖括号在真实套件名 `TLS_AES_256_GCM_SHA384` 里不可能出现，撞不上）。⚠ **但两者动机不同，别当成同一条**：`<other>` 挡的是**访问者可控的无界基数**，`<unknown>` 标的是**我们问不出来**；共用的只是那个防撞记号。⚠ 它把「这条传输问不出套件」与「我们没读到」并成一格 —— 今天两者恰好是同一个集合（h3 走 quiche，`Handshake::cipher()` 锁在私有 `mod tls` 里），**而判据有意不依赖这个巧合**。基数代价：h3 恒为 `TLSv1.3` × `<unknown>` ⇒ 多一条 series。|

## 11. ⏳ 待定清单

> 只列**仍然开着**的。已结案的那些，结论在 §10 的决策表里。

| | 待定项 | 最晚需要在 |
|---|---|---|
| **D19** | **`cache { capacity … }` 改了之后 `POST /load` 不生效。** 后端容量在 `Backend::open` 时定死，而同一个块里的 `ttl` / `max_size` 是每请求现读的、换配置立刻生效 —— 两条子指令行为不同，而配置文件上看不出来。批 H 只堵了更贵的那半：`disk` 变了 `POST /load` 回 409。三条候选：① 后端支持在线改容量；② 与 `disk` 一样回 409（代价是正当的 `ttl` 调整也被拒）；③ 维持现状但在 `load` 的回话里说出来。 | M2 收尾前 |
| **D20** | **磁盘缓存的 I/O 在请求路径上是同步的。** 七个操作的签名与 `MemStore` 逐字相同，而那组签名是同步的；改成 async 等于把接口劈成两份，数据面就要为「内存还是磁盘」分出两条路。代价：一次冷盘读会占住一条 tokio 工作线程（管理面那两条走遍目录树的操作已包 `block_in_place`，请求路径上没有）。⏳ **先量再定** —— M3 对拍时的命中率与 p99 会给出这个数有多大。 | M3 |
| **D21** | **musl 产物的构建宿主口径。** 仓库现有的 `docker/Dockerfile.build`（Debian trixie）编不出 musl 产物：`musl-tools` 只给 `musl-gcc`，没有 `musl-g++`，而 BoringSSL 的 `ssl/` 是 C++；apt 里也没有 musl 交叉包。两条候选：**（a）Alpine 原生 + qemu 跑 aarch64**（现状，不新增工具链，aarch64 那趟慢一个量级）；**（b）glibc 宿主 + musl 交叉工具链**（两个架构都快，代价是新增一整套 C/C++ 交叉编译器）。⚠ 与选哪条无关、两条都要面对：`boring-sys` 的 bindgen 走 `dlopen`，静态 build script 不能 dlopen ⇒ 最终产物那一步必须用 `cargo rustc -- -C target-feature=+crt-static` 这种 per-crate 的口子。 | M4 打包前 |
| **D23** | **「产物里真的链接了哪几套 TLS」仍然没有判据。** 三个问题已经分开：锁里写着哪些（门 4）· 依赖图里真有哪些（门 5，`cargo tree -e all --target all`）· **产物里真的链接了哪些**（⏳ 本条）。⚠ `Cargo.lock` 是依赖图的超集，所以「锁 ≠ 图」已被实测抓到一次，「图 ≠ 产物」就不能再靠推理当成同一件事。★ 不紧迫：门 5 是本条的超集，「多了一套」这个方向已经守住，本条欠的是「图里有、产物里其实没链接」那一半。两条候选：① 按 musl target 再比一次 `cargo tree`（便宜，仍是图不是产物）；② 直接看产物符号。⚠ 与 D21 不是同一档：D21 卡的是能不能造出 musl 产物，本条卡的是造出来之后怎么问它。 | M4 打包前 |
| **D24** | **musl 静态产物那一格只覆盖 x86_64，而 G13 承诺两个架构。** `tests/musl/product.sh` 默认 `ARCHES="amd64"`；aarch64 要在 qemu 上编整个产品。三条候选：① 也挂进每次门禁（要先量 qemu 上编整个产品要多久）；② 只在 `Cargo.lock` / 三张 Dockerfile 变化时跑；③ 留给 M4 打包流水线。★ 不紧迫：aarch64 产物一次都没发布过，缺的不是回归而是首次验证。 | M4 打包前 |
| **D29** | **自动 HTTPS 的重定向端口写死 `:80`。** `synthesize_http_redirect`（`crates/fulcrum-config/src/compile.rs`）给每个自动 HTTPS 站点合成的 308 站点，端口是个字面量，配置面上没有任何一处说得出它。⚠ **它不只是个默认值**：HTTP-01（RFC 8555 §8.3）规定 CA 只连 80 端口，所以「让它可配」与「HTTP-01 还能用」是有张力的 —— 那条挑战正是靠这个站点才有落脚点（G54 把 HTTP-01 定为备）。两条候选：① 全局块加一条 `auto_http_redirect_port`，并写明改了它就等于关掉 HTTP-01；② 维持写死（现状），只在文档里说清楚。★ 顺带的后果已经处理掉了：门禁里**好几个场景**因此隐式共用 `127.0.0.1:80`，**是哪几个由 [`docs/platform/host-and-gate-traps.md`](docs/platform/host-and-gate-traps.md) 那张端口表说**。⛔ 这里有意**不抄那个个数**：它一道门都没有，而一个场景长出一条 TLS 站点就会悄悄加进那份名单（2026-09-03 实测抓到过一次：`tests/stats/` 早就在里面而名单上没有）。⚠ **占着它并非无害**（那个「无害」的旧结论只是一次落在良性锁序上的采样）—— 实测它能让**别的端口**起不来，所以名单上的场景各自有责任在退出时把 `:80` 还回去。★ 不紧迫：`:80` 是所有 CA 的事实默认，改它只服务于非标准部署。 | M4 发布前 |
| **D9** | **版本与兼容性策略**：何时算 breaking change，DSL 与结构化配置各自的稳定性承诺。⚠ 连带：`fulcrum_build_info{version}` 今天是 `0.0.0`（六个 crate 都是它）⇒ **这一族区分不出任何两次构建** —— 那不是缺陷，是本条没结案的直接后果。 | M4 |

## 12. 官方技术入口

- [Pingora（GitHub）](https://github.com/cloudflare/pingora)
- [Caddy API](https://caddyserver.com/docs/api)
- [HAProxy Runtime API](https://www.haproxy.com/documentation/haproxy-runtime-api/)
- [Nginx graceful control](https://nginx.org/en/docs/control.html)
- [quinn（QUIC）](https://github.com/quinn-rs/quinn)、[h3](https://github.com/hyperium/h3)、[rustls](https://github.com/rustls/rustls)
