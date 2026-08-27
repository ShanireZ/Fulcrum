---
type: 架构基线
title: TLS 与自动 HTTPS
description: ⚠ 2026-08-25 起后端是 BoringSSL 不是 rustls（G104 推翻 §5.1 第 1 条）；h1/h2 与 QUIC 共用一套，动态证书走 select_certificate_callback；On-Demand 未配准入则启动失败。
resource: ../../PLAN.md
tags: [架构, 承重墙, 必读, 易错, TLS]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-12T00:00:00Z
sources:
  - id: plan-51
    resource: /references/plan.md
    title: PLAN.md §5.1 第 1 条硬约束（TLS 后端锁死 rustls）
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G6（统一 rustls）、G12（自动 HTTPS）、G15（On-Demand 准入强制）
  - id: plan-11
    resource: /references/plan.md
    title: PLAN.md §11 D4（ACME 提供商、证书存储、续期与失败退避，最晚 M1）
---

★ 本页属**技术基线**，带真内容。它**服从** [`PLAN.md`](../../PLAN.md)；冲突时以 `PLAN.md` 为准。

> ⚠ ⚠ ⚠ ★ ★ ★ **本页的 TLS 后端前提整个变了 —— 请先读这一段再往下看。**
>
> **G104**（§10）**推翻了 §5.1 第 1 条**（「TLS 后端锁死 rustls……不可回头」），
> 后端改为**统一 BoringSSL**。成因是 **G103 取 `quiche` 做 HTTP/3**，
> 而 quiche 用 BoringSSL、依赖表里没有 rustls ⇒ 继续锁 rustls 就必然「两套 TLS 并存」，
> 而那正是本页「最容易在哪做错」**第 3 条**点名的形状。
>
> | 这一条 | 现在是什么 |
> |---|---|
> | 动态挑证书的接口 | ~~`ResolvesServerCert`~~ → **`boring::ssl::SslContextBuilder::set_select_certificate_callback`**（直接拿到 `ClientHello`）|
> | 两个入口的关系 | **仍然共用一套** —— 目标没变，介质从 rustls 换成 BoringSSL |
> | On-Demand 握手期现签 | ★ **变容易了**：`set_async_select_certificate_callback` 是**原生异步**的，而 `UNWIRED` 里 `on_demand` 的登记理由正是「`resolve()` 是同步的，需要一座桥」⇒ **那条理由到期了** |
>
> ✅ ★ ★ **产品的监听器侧已经换完了。**`crates/fulcrum-tls` 现在是 `CertKey`（`X509` + `PKey`）+ `set_select_certificate_callback`，`pingora-core` 挂 `boringssl` feature。
> ✅ ★ ★ **第 ② 处 —— L4 的 ClientHello 预读也换完了。**
> `peek_client_hello` 现在起**一台真的握手状态机 + 一条内存传输**，在早回调里抄走 SNI 与 ALPN
> 之后**当场把握手掐掉**；⚠ 内存传输的**写那一半一律丢掉** —— 我们不是这条 TLS 连接的对端，
> 往真客户端写一个字节（哪怕只是一个 alert）都等于冒充上游说话。
> ⇒ `pingora-rustls` 退出根 workspace 的依赖图，**fork 改动 8 / 8b / 10 三条全部删除**。
> ★ ★ **一处行为变化，而它是变好的那一侧**：早回调发生在几乎所有 ClientHello 校验**之前**，
> ⇒ 一个完整 TLS 栈会当场拒掉的 hello（缺 `signature_algorithms`）照样按 SNI 分流 ——
> **「能不能分流」终于只取决于「能不能读出 SNI」**，而不再取决于某个 TLS 栈的严格程度。
> ✅ ✅ ★ ★ ★ **第 ③ 处 —— 出站 HTTPS 也换完了，三处全换到此收口。**
> `crates/fulcrum-acme/src/https.rs` 自己写了一个 `tower_service::Service<Uri>` 连接器
> （底下是 fork 里已有的 `pingora_boringssl::tokio_ssl::SslStream`，**新增 0 个包**），
> **ACME 协议本身**与**原生 DNS 供应商的 API** 从此共用它 —— 一套信任库、一套校验、一份 ALPN。
> ⚠ 校验一行都不是自己写的：链走 `SslConnector::builder` 里的 `set_default_verify_paths()`，
> 名字走 `configure()?.verify_hostname(true).into_ssl(domain)`。
> ⚠ ⚠ **「产物里只有一套 TLS」现在成立了，而证明它的不是门 4** ——
> **`Cargo.lock` 是依赖图的超集**：`hyper-rustls` / `rustls` / `schannel` **仍然写在锁里**
> （`instant-acme` 的 `ring` feature 里那句 `hyper-rustls?/ring` 把它留住了），
> 而 `cargo tree -e all --target all -i rustls` 已经是 **nothing to print**。
> ⇒ 新立 **门 5**（读 `cargo tree`，`supply_gates.rs`）；门 4 的口径收窄成「锁里写着哪些」。
> ★ ★ ★ **门 4 曾经在注释里预言「③ 做完时我会先红一次提醒你改」—— 它没红。**
> 一道门的注释写「本门守 X」不等于它判得动 X，这一次连**它会不会红**都预言错了。
> ★ 顺带两条：fork 改动 **8 / 8b** 在 BoringSSL 那侧**不需要对应物**（`TlsSettings`
> 对 `SslAcceptorBuilder` 有 `DerefMut`）；`pingora-boringssl/src/ext.rs` 自带
> `suspend_when_need_ssl_cert` —— **On-Demand 要的那座桥不用自己造**。⇒ ★ **本页下面凡是描述 rustls 的段落，
> 现在都是「当前实现」而不是「目标形态」** —— 两者第一次分开了，读的时候要分清。
> ✅ ★ **那条未验前置已验过，通过**：完全静态的 musl 二进制在 `FROM scratch`
> 里跑完真的 QUIC 握手，且 `set_select_certificate_callback` **真的被调用**、读得到 SNI
> ⇒ **G103 不重议**。记录 [`musl + BoringSSL 静态链接`](/verification/musl-boringssl.md)。
> ⚠ ⚠ 但它换来一条新的：**仓库现有的构建镜像编不出 musl 产物**（Debian 没有 `musl-g++`），
> 已立 **D21**。★ 这一条不挡本页描述的迁移开工。

> ✅ **TLS 那条脊梁已经落地并跑通**（`crates/fulcrum-tls` + `crates/fulcrum-server/tls.rs`）：
> 按 SNI 动态挑证书、每域一目录的 PEM 存储（原子写 + `flock`）、续期判定（ARI → 1/3 → 退避）、
> `tls <cert> <key>`。端到端由 `tests/serve/run.sh` 验过：`--cacert` 验签通过、
> ALPN 协商到 h2、未知 SNI 被拒绝握手。
> ✅ **ACME 本体（自动签发/续期）已全部接线**——三种挑战（TLS-ALPN-01 主、
> HTTP-01 备、DNS-01 通配符）、原生 Cloudflare / DNSPod、ARI 续期，门禁里对着真 CA（pebble）
> 跑两个端到端场景；**并且已经在真实域名上对生产 Let's Encrypt 真用着**。
> ⏳ **仍没接线的只剩 `tls internal` 与 On-Demand**——它们在 `fulcrum_runtime::UNWIRED` 里
> 逐条登记着、装载时会打出来，★ 那份登记表由 `tests/unwired_contract.rs` **双向**钉住
> （Rust 一份、`dsl-reference.md` 一句），**以它为准，不以这段散文为准**。
>
> ⚠ ⚠ **落地时撞到一件必须记住的事**：Pingora 0.8.1 的 rustls 监听器
> **根本够不到 `ResolvesServerCert`**，见下面「一条硬约束差点无路可走」。

# ~~统一 rustls（G6）~~ → 统一 BoringSSL（G104）

**目标形态（G104）**：h1/h2 入口（Pingora）与 QUIC 入口（**quiche**）**共用同一套 BoringSSL 配置**：
一套证书存储、一套 ALPN 协商、一套会话缓存。

**当前实现**：✅ **四处全是 BoringSSL，而且 h3 那一处已经在跑** ——
h1/h2 入口、L4 的 ClientHello 预读、出站 HTTPS，加上 **QUIC 入口**
（`quic::listener::build_quic_config` 把同一个 `SniResolver` `install_into` 一个
`SslContextBuilder`，再交给 `quiche::Config::with_boring_ssl_ctx_builder`）。
★ ★ 「两个入口共用同一个 `select_certificate_callback`」从此**不是目标形态，是实现**：
HTTP/3 端到端场景 [`tests/h3/run.sh`](../../tests/h3/run.sh) 全程用 `--cacert`（不是 `-k`）
⇒ **同一张证书经 BoringSSL 装进 quiche 之后在 QUIC 上真的验得过**，这一条有判据。
⚠ 这一行原本写着「**当前实现**：h1/h2 走 rustls」，而监听器侧换完的当天它就不成立了。
★ **一句被标注为「当前实现」的话，恰恰是最会烂的那一种**：它按定义就是会过期的，
而没有任何东西在检查它。

## ★ ★ ~~这是一条不可回头的硬约束~~ → 它已被推翻了

> ~~**rustls 后端不支持 `certificate_callback`。动态证书选择必须实现 `ResolvesServerCert`。**~~
>
> ~~自动 HTTPS 与 On-Demand TLS 都建在这个接口上，因此 **TLS 后端从第一天起锁死，不留双路径**。~~

★ ★ ★ **原文保留，因为那句理由到今天依然为真** —— rustls 确实没有 `certificate_callback`。
**变的不是这句话，是我们不再站在 rustls 那一侧了。**

> ★ ★ **留一句方法论：「不可回头」约束的是我们自己不要反复，不是约束事实不许变。**
> ⚠ 一条**理由仍然成立、而结论已经不适用**的约束，比一条明显过时的约束**更难发现** ——
> 因为读到它的人会先验证那个理由，而理由是对的。

**现行（G104）**：后端统一 **BoringSSL**，动态证书选择走
`SslContextBuilder::set_select_certificate_callback(|ClientHello| …)`。
★ **「不留双路径」这条目标一个字没变** —— 恰恰是为了守住它，才取「统一到 BoringSSL」
而不是改动面更小的「两套并存」。

★ **一处现实差距，查了一半**：`pingora-core` 的 `default = []`，
**rustls 后端要显式开 `features = ["rustls"]`**。M0 不碰 TLS，所以这个 feature
**从来没有被打开过**——G6 这条决策至今在 `Cargo.toml` 里没有任何表达。

★ ★ ★ **去补的时候发现它不是补一行 feature 的事**，全部证据见
[rustls 接缝验证](/verification/m1-rustls-seam.md)：

- **打开它，产品依赖图 133 → 175**——一次进来 **42 个从未受审的 crate**；
- **三道门（G30 上界审计／回归网／`supply-audit.py`）结构上都够不到 `pingora-rustls`**，
  因为未启用的可选依赖**根本不在根 `Cargo.lock` 里**；
- 里面有一条真缺陷：明明装的是 ring，`aws-lc-rs`（69 MB C 源码）却被一起编进产物。

**已办**（G41／G42）：fork 里两处清单修掉、`rustls-native-certs` 抬到 0.8；
**回归网现在带 `--features pingora-core/rustls` 跑**，rustls 那一半代码第一次进了判据。

✅ 产品侧那一半也补上了：`crates/fulcrum-server` 带着那个 feature 之后多解析出 **46 个包**，
而 **`aws-lc-rs` 不在其中**（`supply_gates.rs` 门 1 逐把锁验过）。

⚠ ★ **以上整节按历史读**：那个 feature
**今天叫 `boringssl`**（G104），而门 3（`supply_gates.rs`）守的正是这一条。
★ 原文不改写，因为它记录的是「打开一个此前没人打开过的 feature，会一次进来 42 个未受审的 crate」——
**那件事与后端是谁无关**，换成 BoringSSL 之后同样成立。

# ~~★ ★ ★ 一条硬约束差点无路可走：`ResolvesServerCert` 在 Pingora 0.8.1 里够不到~~

> ⚠ ⚠ ★ **本节整段是先前的历史，而它描述的那处 fork 改动已归零。**
> 逐字保留，因为它记录的是「上游没有一扇门通向那个接口」这件事**是怎么被查清的**
> （四条路各撞一次墙，不是读了一句文档）。
>
> **今天的事实**：TLS 后端是 BoringSSL，动态证书走 `set_select_certificate_callback`
> —— 而那是上游 `pingora-boringssl` **本来就有**的能力，不需要任何 fork 改动。
> **fork 改动 8 / 8b / 10 在 §10 全部删除。**
>
> ★ ★ ★ **而这里有一条比结论更值钱的东西**：下面那句写着这处改动的归零条件是
> 「**等 #632 或 #908 落地**」—— 而它归零的时候，上游那两条**一条都没落地**。
> 三处 fork 改动的归零条件全都挂在「上游做点什么」上，**结果三处都是被第三件事释放的**
> （G103 取 quiche ⇒ G104 换后端）。
> ⇒ **一条把自己绑在别人身上的到期条件，多半不是它真正的到期方式** ——
> 而没有任何东西会在它真的到期时通知你。

上面那条「动态证书必须实现 `ResolvesServerCert`」在落地时撞了墙——
**上游的 rustls 监听器没有任何一扇门通向那个接口**：

| 门 | 实际情况 |
|---|---|
| `TlsSettings::intermediate(cert, key)` | 只接受证书与私钥的**文件路径**，`build()` 里写死 `with_single_cert` |
| `TlsSettings::with_callbacks()` | **直接返回错误**（"Certificate callbacks are not supported with feature rustls"）|
| 自己构造 `Acceptor` | 字段私有，crate 外构造不出来 |
| `add_address(ServerAddress)` | 枚举只有 `Tcp` / `Uds`，带不上 acceptor |

后果：**一个端口只能一张证书、换证书要重载、握手期按 SNI 现签无从实现**——
而自动 HTTPS（G12）与 On-Demand（G15）都建在那条路上。

**处置：fork 里加了 `TlsSettings::with_cert_resolver`**（见
[`vendor/pingora/FORK.md`](../../vendor/pingora/FORK.md) §8）。
⚠ 这是 fork 的**第一处「加能力」改动**——加能力比抬版本上界更贵，
上游一旦改动 `TlsSettings` 的形状就要手工重贴。★ 但它没有替代方案：
不加就等于放弃两条已经拍板的决策。
❌ **投稿五已撤销（G68）**：同一诉求上游已有 **9 条** issue/PR，
其中 #632 与我们逐点相同且更完整 —— **不发第 7 份**。★ 这处改动的归零条件因此变成
「等 #632 或 #908 落地」，而不是「等我们那份被合」。

# 证书获取的三条路

| 来源 | 触发时机 | 说明 |
|---|---|---|
| 配置中声明的域名 | 启动 / 配置装载时 | 默认行为，提前签发与续期 |
| **On-Demand** | TLS 握手时按 SNI | 面向多租户与用户自定义域名 |
| 外部提供的 PEM | 配置中显式指定 | 企业 CA、已购证书、内网自签 |

# ✅ ACME 落法（D4 已由 G53–G56 结案）

| 项 | 决定 |
|---|---|
| 客户端库 | **`instant-acme`**，上层自己搭（G53）|
| 挑战 | ★ **TLS-ALPN-01 主 + HTTP-01 备 + DNS-01**（G54）|
| 存储 | 每域一目录的 PEM（G55）|
| 续期 | **ARI 优先**，回退「剩余寿命 1/3」（G56）|

## ★ ★ 选 `instant-acme` 的理由是接口形状，不是「更轻」

~~§5.1 第 1 条锁死 rustls，动态证书必须走 `ResolvesServerCert`~~（⚠ **由 G104 推翻**，
现在走 BoringSSL 的 `select_certificate_callback` —— ★ **但选 `instant-acme` 的理由不受影响**：
它讲的是「ACME 协议库不该替我们管 acceptor 与准入」，与后端是谁无关）；而 On-Demand TLS 要在
**握手期按 SNI 触发签发并做准入控制**。这套逻辑必须长在枢衡自己的证书管理器里——
`rustls-acme` 那类高层库的模型是「给你一个 acceptor」，与 Pingora 的监听器不是一套，
准入控制也塞不进去。`instant-acme` 只做 RFC 8555 协议本身，挑战求解与存储调度归我们。

⚠ ⚠ **写依赖时必须 `default-features = false` 再显式开 `ring`。**
`instant-acme` 的 default feature 里**就含 `aws-lc-rs`**——照默认写一行，
[rustls 接缝那一轮](/verification/m1-rustls-seam.md)把 aws-lc-rs 赶出依赖图的成果**当场作废**。
★ 这是「上游 crate 的 `default` 是一条静默的供应链入口」的**第四次**现身，
**落地时要有一道门守住它**，别只写在注释里。

## ★ ★ 通配符只吃一层（RFC 6125），而站点索引比它宽

`*.example.com` 的证书覆盖 `a.example.com`，**不覆盖** `example.com`，
也**不覆盖** `a.b.example.com`。这不是我们的偏好，是 RFC 6125 与浏览器的实际行为。

⚠ 放宽成「后缀匹配」的后果很难查：**服务端挑了一张证书、客户端拒绝它**，
而服务端日志里只有一次成功的握手。

✅ **站点索引（路由那一侧）曾经是宽的**（`ends_with`，任意层都命中），两边语义不一致 ——
**D18 由 G66 结案**：收紧站点索引，且**两侧共用同一份实现**
（`fulcrum_config::host::wildcard_covers`）。★ 不是拿契约测试钉住两份，
而是让「两份各自漂」在结构上做不到。

## 证书存储：两条约束来自「两代进程并存」

布局：`<state>/certs/<issuer>/<domain>/{cert.pem, key.pem, meta.json}`，私钥 `0600`，
目录由 systemd `StateDirectory=` 托管（G33）且可覆盖。

★ ★ **升级窗口内两代进程共享同一个证书目录**（M0 已证两代并存），
而 On-Demand 签发是**运行期**行为，两代可能同时写同一个域名。所以：

1. 写入一律**写临时文件再 `rename`**（同目录内原子替换）；
2. 跨进程用**文件锁**串行化同一域名的签发——否则两代各签一张、互相覆盖，
   还各自消耗 CA 的速率配额。

## 续期不写死天数

先问 CA 的 **ARI**（RFC 9773，`instant-acme` 自带、Let's Encrypt 已提供）拿建议窗口；
查不到时回退「剩余寿命 1/3」。★ **Let's Encrypt 正在推 6 天短寿命证书**——
「到期前 30 天」在那种证书上等于**永远在续期**。
★ ARI 还有比例制给不了的东西：CA 因安全事件需要**提前批量撤销重签**时，它是唯一的通知渠道。

失败走指数退避（封顶）+ **随机抖动** + 指标告警。抖动不是可选项：
同机几十个域名同时到期时，不抖动会把重试打成尖峰。

# ✅ DNS-01 的落法（D15 已由 G57–G59 结案）

**原生 Cloudflare + DNSPod，其余走 exec hook。** ★ 两家是查出来的不是拍的——
实测 `example.com` 的 NS 在 **DNSPod**、`example.net` 在 **Cloudflare**，
即 M1 退出条件里的两个域名分属两家，而这两家又分别是全球第一与大陆第一。

> ★ **形状是「不先建体系」**：原生支持按真实用到的一家一家加。
> §6.2 不做服务发现集成，而**一个 DNS 供应商动物园是同一个形状**。

★ **通配符证书是 M1 的交付内容**（G58），已写进 §7 M1 的退出条件。
⚠ 代价：DNS-01 因此位于 M1 的关键路径上，它出问题会直接卡验收。

★ ★ **一条必须写死的实现约束**：要等 TXT 记录**可见**了才能通知 CA 校验，
而「可见」不等于「API 返回 200」。**判据挂在行为上**——直接向该域的**权威 NS** 轮询确认，
**绝不能只 sleep 一个固定秒数**（快时浪费、慢时直接签失败，而失败要消耗 CA 速率配额）。

## ⚠ DNS 凭据比 On-Demand 被刷爆严重得多

拿到某域的 DNS 写权限 = **能为该域签发任意证书** + 能改 MX 劫持邮件。所以（G59）：

1. **凭据绝不写进 DSL**——DSL 是要被 diff、被贴进 issue、被版本控制的东西；只从文件或环境变量读。
2. **能程序化校验权限范围的，启动时就校验并拒绝启动**（Cloudflare 有 token 校验端点）。
3. 校验不了的，**强制在配置里显式声明该凭据覆盖哪些 zone**，超范围签发一律拒绝。

★ 形状照 G15：**错误在启动时暴露，不等被滥用才发现。**

# On-Demand 的强制准入（G15）

★ **未配置准入来源时，启用 On-Demand 会导致启动失败**，而不是运行时才发现被刷爆。

准入来源二选一（或并用）：

- **域名白名单 / 通配符**：无外部依赖，握手期零额外延迟
- **ask 端点**：握手时回调外部 HTTP 服务询问该域名是否允许签发，面向域名集合动态的场景

准入之外还有三道保险：**签发速率上限、失败退避、指标告警**。

# ✅ ACME 的判据：门禁里跑一个真的 CA（G64）

真实签发只有两条路：打真 CA（要真域名、会消耗速率配额，**不能进门禁**），
或者在门禁里跑一个本地 ACME 服务器。★ **而 G58 已经把「签发并续期一次通配符证书」
写进 M1 的退出条件**——没有本地 CA 的话，那条退出条件只能靠人手工跑一次真域名来证明，
**不可复现**。

owner 已拍板：**加**。落地形态是**两个**场景——
[`tests/acme/run.sh`](../../tests/acme/run.sh)（管「签发」）与
[`tests/acme/renew.sh`](../../tests/acme/renew.sh)（管「续期」），
共用的那一半在 [`tests/acme/lib.sh`](../../tests/acme/lib.sh)：

| 角色 | 是谁 |
|---|---|
| CA | `pebble`（Let's Encrypt 官方测试 CA，与 boulder 同一批人写的另一个 RFC 8555 实现） |
| 权威 DNS | `pebble-challtestsrv`，把任何域名解析到 `127.0.0.1` |
| 被验的那一方 | `fulcrum serve` 自己 |

两个二进制按 **sha256 逐架构钉死**在 `docker/Dockerfile.build` 里，理由与基础镜像那条一样：
浮动的 `releases/latest` 会让门禁里的 CA 哪天悄悄换掉，而**没有一行输出会说出来**。

★ ★ **三条写这套判据时踩到、值得记下来的**：

1. **不能拿一张自签的 `CA:TRUE` 证书直接当 pebble 的服务端证书。**
   curl 收得下，而 **产品里的那个 TLS 客户端不收**（当时是 rustls，报 `CaUsedAsEndEntity`），
   现场表现是「建 ACME 账户失败：client error (Connect)」，看不出跟证书有关。
   ⚠ 真正要连上 CA 的是**产品里的那个客户端**，不是 curl——
   拿 curl 验一遍会得到一份全绿的假报告。必须是「根 + 叶」两级，叶还要有 `serverAuth`。
   ⚠ ★ **此后那个客户端是 BoringSSL 了**（G104 第 ③ 处）。这条坑仍然成立，
   只是报错文本会变 —— ★ 而这正是「判据别钉在某个库的错误串上」的一个现成例子：
   这一段钉的是**形状**（自签 CA 当叶子用），不是那个 `CaUsedAsEndEntity`。
2. **challtestsrv 的 `-defaultIPv6 ''` 不是可选的**：它默认同时回 A 与 AAAA（`::1`），
   而枢衡若绑在 v4 上，CA 走 v6 来验就连不上——现场同样只有一句「验不过」。
3. **pebble 默认故意拒 5% 的合法 nonce**（逼客户端实现 RFC 8555 §6.5 的重试），
   而 `instant-acme` 0.8.5 的重试上限是**三次**。留着它是对的（真 CA 就这样），
   但要先把偶发概率算清楚，见 `tests/acme/run.sh` 里那段注释。

## ★ ★ ★ TLS-ALPN-01：判据必须挂在 CA 的记录上，不能挂在我们自己的日志上

TLS-ALPN-01（RFC 8737 / G54 的「主」）已接线。它的形状是：
CA 向 `<域名>:443` 握手、ALPN 只提 `acme-tls/1`，服务器回一张**自签**证书，
带一条 critical 的 `id-pe-acmeIdentifier` 扩展（内容是 key authorization 的 SHA-256），
CA 看完就断开。**零路由占用** —— 用户的 `respond 403` 挡不住自己的证书签发。

★ 两张证书表**互不相通**是安全属性：协商到 `acme-tls/1` 只查挑战表（查不到就拒绝握手，
绝不回落到真证书）；没协商到就绝不查挑战表（否则普通访客拿到自签证书，浏览器报错，
而服务端日志里是一次**成功**的握手）。

> ⚠ ⚠ ★ **这一批的反证用一种没预料到的方式教了一课。**
>
> 把挑战证书里那条扩展去掉，本以为门会红在「签不出来」——**门全绿**。
> pebble 日志里明明写着 `authz … set INVALID`：破坏是生效的，
> 只是退避到点之后，G54 的「备」把它接住了，HTTP-01 一次就过。
> 而当时门里那条「这个域名走的是 TLS-ALPN-01」的断言**也是绿的**——
> 它证明的是「我们**试**了」，不是「它**成了**」。
>
> **一道分不出「成功」与「失败了但被兜住」的门，恰好对这一批的全部内容是瞎的。**
>
> 修法：判据改挂在 **CA 自己的记录**上——pebble 每次 HTTP 验证都会打一行
> `Attempting to validate w/ HTTP: http://<域名>:`，TLS-ALPN-01 成功的话
> 这个域名根本不该出现在那些行里；再配一条自证（同一把尺子必须量得出那个
> 确实走 HTTP-01 的域名），外加一条更粗的「一次健康的跑里 CA 不该判任何 INVALID」。

## ★ ★ ★ 「续期」那半边为什么要单独一个场景

G58 的原话是「至少签发**并续期**一次通配符证书」。签发那半边由 `tests/acme/run.sh` 钉死了，
而**续期那半边此前一条端到端判据都没有** —— 签发那一格验到的是
「第二轮巡检判**已是最新**」，那恰恰是**没有**续期时的判据。

> ⚠ **「暂时不用续」与「该续时会续」是两件相反的事。** 前者绿一万次也证不出后者。
> 这一族的形状在本仓库反复出现：一个只见过「有」的断言，与一个恒真的断言无法区分。

做法：**另起一个 pebble 实例 + 独立 state dir**，
用 profile 把证书寿命压到 **~1.5 天**，让**跑着的**枢衡自己的巡检循环走到续期。
短寿命证书会污染同一个 CA 下的其它断言，所以两个场景**绝不共用一个 CA**。

★ ★ **那个「1.5 天」是量出来的**（pebble v2.10.1）：

| 量到的 | 结论 |
|---|---|
| ARI 建议窗口 | `[notAfter − 寿命/3 − 1 天, notAfter − 寿命/3 + 1 天]` |
| 那个 ±1 天 | **绝对值，不随寿命缩放**（90 天与 129700s 两个量级各验一次：预测 66s、实测 65s）|
| `certificateValidityPeriod` | **死配置**：解析进结构体了，CA 只认 profile，实测配 600 之后仍然装载 7776000 |
| profile 名字 | 必须是 `default`，否则目录里没有 default，客户端不选 profile 时没有兜底 |

⚠ 所以**不能**把寿命调到分钟级：那样 `2L/3 − 86400` 是负数，ARI 窗口在证书出生之前
就已经开始，第二代第一轮巡检直接续——跑得更快，却**证不到「它自己等到了时候才动」**，
而那正是续期的全部内容。取 129700s ⇒ 窗口在签发后约 65s 开始。

判据刻意**不是**「出现了一张新证书」——那分不出三种情形：按 ARI 续期、
旧的读不出来于是重签、每一轮都在重签。所以它钉的是**谁下的决定**（日志里 ARI 那一句，
只有 `should_renew` 的 ARI 分支会打）、**序列号变了**、续期那趟**重新解了一遍 DNS-01**、
新证书**上了线**（真握手取序列号，不是看盘上的文件），以及反向的那一半
（第一轮必须判「已是最新」，且 ARI 窗口还在将来）。

★ 三条反证各自只红该红的那几条，其中一条最说明问题：**把「签完热装」那一步摘掉，
盘上换了、HTTPS 仍然 200、只有「线上握手拿到的序列号」这一条红**——
一道只验「还能访问」或「盘上变了」的门在那里是全绿的。

## ★ ★ 原生 Cloudflare / DNSPod 与 G59（落地）

三条约束各自落在能被门守住的地方：

| G59 | 落点 | 谁守着 |
|---|---|---|
| 第 1 条：凭据绝不写进 DSL | `dns <provider> <来源>` 只认 `env:名字` / `file:路径` | **编译期错误** |
> ⚠ ⚠ ⚠ **第 1 条由 owner 改口径**：凭据**可以**写进 DSL 了（Caddy 形状 ——
> 一份配置文件就能跑完）。⇒ 代价挪到了三处、而不是消失：
> ① 装载期**权限门**（配置对 other 可读就拒绝启动）；
> ② `compile` **默认脱敏**、`Secret` 的 `Debug`/`Display` 默认脱敏；
> ③ 脱敏产物**不许被 load**（否则 `«已脱敏»` 会被当凭据发给 CA，
> 而现场表现与「凭据真的写错」一模一样）。详见 [DSL 参考](/architecture/dsl-reference.md) §4.4。

| 第 2 条：能校验的启动时校验并拒绝启动 | Cloudflare **对声明的每个 zone 查一次 id**（`GET /zones?name=…`）、DNSPod `Info.Version`，跑在 `run_forever()` **之前** | 启动路径 |

> ★ ★ ★ **Cloudflare 那一条  换过判据，而换的理由值得记住。**
> 原先打的是 `/user/tokens/verify`。Cloudflare 2026 年把令牌分成**用户级 `cfut_`** 与
> **账号级 `cfat_`**，而后者在用户级端点上**必然**回 `1000 Invalid API Token` ——
> G59 把「对端说不行」判成 Fatal，于是**一把完全好用的账号级令牌会让枢衡拒绝启动**，
> 错误信息还理直气壮地说「凭据不可用」。
> ⇒ 换成查 zone id 之后，判据严格更强：它同时证明了**令牌能用**、**够得着这个 zone**、
> 以及 **`zones` 里的名字真的存在**（打错一个字母立刻现形，而不是等到第一次签发）。
> ★ 这也是「**验我们接下来真的要做的那件事**，而不是验有没有某个我们其实不用的接口认它」
> 这条口径的第一次落地。
| 第 3 条：显式声明 zone，超范围拒绝 | `tls { zones … }`，原生供应商**必填**；判定按**标签边界** | **编译期错误** + 运行时拒绝 |

> ★ ★ ★ **第 1 条必须是白名单。** 一个「看起来像 token 就报错」的黑名单实现，
> 要去猜什么样子算 token —— 而**猜错的那一次，恰恰就是真 token 被放行的那一次**，
> 且没有任何症状。

> ★ ★ **第 3 条不等于「把权限做小了」。** DNSPod 的 token 是**账号级**的，
> 覆盖该账号下的全部域名，也**没有可问出范围的端点**。声明的价值在于：
> **越权那一刻是枢衡自己拒绝的，而且拒绝的理由在配置里看得见**。

★ ★ **第 2 条有一处知情的取舍。** 「对端回话说不行」与「压根没连上」处置**相反**，
而分法挂在**类型**上（`VerifyError::{Fatal, Inconclusive}`），不是让调用方去 match 错误串：

- `Fatal`（对端说不行 / 凭据根本读不出来）⇒ **拒绝启动**；
- `Inconclusive`（网络不通、超时）⇒ 打一条 **error** 继续跑。

⚠ 后者与「『没能检查』当成『检查通过』是栽过的形状」**是有张力的**。
取舍理由：一次网络抖动不该让整台机器上**所有**站点都起不来，包括那些用静态证书的。
★ 所以它**不是悄悄跳过**，是打 error 明确说出「这份凭据没被验过」。

### ⚠ ⚠ 这一批的判据边界，与前几批不同

单测能证明的是**内部自洽**：我们发出去的 URL / 方法 / 头 / body 正是我们以为的那些，
给定一段文档里那种形状的响应、解析出来的东西正是我们以为的那些。

> **它证不了「我们对这两家 API 的理解是对的」。** 拿一个我们自己写的假服务去证后者，
> 就是「判据挂在替身上等于没有判据」——假服务的行为也是我们自己想出来的，
> **两边同时错的时候它照样全绿**。
>
> ★ 那一条的判据只能是 **M1 退出时那两个真域名**（`example.com` 在 DNSPod、
> `example.net` 在 Cloudflare）。这是 G57 当初把原生供应商单独分一批时就写下的话，
> 不是这一批的新说法。

★ 两处容易想当然、各由一条测试钉住的地方：**dnsapi.cn 永远回 HTTP 200**
（真正的结果在 body 的 `status.code` 里，只看状态码会把每次失败都当成功）；
**DNSPod 的凭据在 body 里而 Cloudflare 的在头里**（一个只挡 `Authorization` 头的
脱敏实现在 DNSPod 上完全失效，而失效的表现是凭据出现在日志里）。

# 最容易在哪做错

1. ★ ★ **照着 openssl 后端的例子写 `certificate_callback`。** rustls 后端**没有**这个东西。这是 G6 三条硬约束里的第一条，[`AGENTS.md`](../../AGENTS.md) 也单独重复了一遍——因为它是「看起来能用、实际编不过」的形态，而网上大量 Pingora 示例用的是 openssl 后端。
2. ★ **把「启动失败」实现成「运行时告警」。** G15 的整个价值在于**错误在启动时暴露**。降级成告警等于把它变回「被刷爆时才发现」——那正是它要防的。
3. **给两个入口各配一套。** 一套证书、一套 ALPN、一套会话缓存是 G6 的原话；两套会在 On-Demand 签发时出现「h2 拿到了新证书而 h3 还是旧的」这类只在特定路径出现的问题。<br>★ ★ ★ **这一条正是 G104 取「统一到 BoringSSL」而不是「两套并存」的理由。** 当时它针对的是「两套 rustls」，而它对「一套 rustls + 一套 BoringSSL」**同样成立、甚至更成立** —— ⇒ 一条写在「最容易做错」清单里的话，在一次选型里**直接决定了拍板方向**。

# 相关

[数据路径](/architecture/data-path.md)（三个入口） · [安全基线](/platform/security-baseline.md) · [技术栈](/platform/tech-stack.md)
