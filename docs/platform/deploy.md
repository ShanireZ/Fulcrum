---
type: 技术基线
title: 部署
description: 怎么把枢衡装到一台真机器上并交给 systemd 托管；停机时长怎么算；变更与回滚走哪条路。
resource: ../../tests/m1/fulcrum-prod.service
tags: [运维, 部署, systemd]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-21T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G31 / G33 / G34 / G37（进程模型与托管）、G78（产品侧接线）
  - id: process-model
    resource: /architecture/process-model.md
    title: 进程与组件边界 —— 托管形状与升级窗口的实测结论
---

★ 本页属**技术基线**，带真内容。它**服从** [`PLAN.md`](../../PLAN.md)；冲突时以 `PLAN.md` 为准。

# 一句话

**一个静态二进制 + 一份 `Fulcrumfile` + 一个 systemd unit。** 没有别的组件。

# unit 文件

下面这份是**可以照抄的形状**，与门禁里跑的那份
（[`tests/m1/fulcrum-prod.service`](../../tests/m1/fulcrum-prod.service)）逐项相同，
**只差一处，而那一处是有意的**：这里多一个 `--bind-host [::]`，理由见下面那段 ⚠ ⚠。
★ 其余逐项相同不是巧合：门禁里跑的形状与文档里写的形状分家的话，
文档这一份没有任何东西在看着它。

```ini
[Unit]
Description=Fulcrum
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
# ★ 让 unit 活过换代的是这一行，不是 MainPID 交接（G37；交接会毁掉优雅停机）。
ExitType=cgroup
NotifyAccess=main

User=fulcrum
Group=fulcrum
# 绑 80/443 需要它；不要给 CAP_NET_ADMIN 之类用不到的能力。
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

ExecStart=/usr/local/bin/fulcrum serve /etc/fulcrum/Fulcrumfile --bind-host [::]
# ⚠ **不能用 $MAINPID**：ExitType=cgroup 下它在首次换代后归零。
ExecReload=/bin/sh -c 'kill -USR2 "$(cat /run/fulcrum/fulcrum.pid)"'

ConfigurationDirectory=fulcrum
StateDirectory=fulcrum
RuntimeDirectory=fulcrum
RuntimeDirectoryPreserve=yes

KillMode=control-group
# ★ 见下面「停机要花多久」——这个数不是拍脑袋的。
TimeoutStopSec=60
Restart=always

Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

| 目录 | 放什么 | 谁建 |
|---|---|---|
| `/etc/fulcrum` | `Fulcrumfile` | `ConfigurationDirectory=` |
| `/var/lib/fulcrum` | 证书存储（`certs/`）、ACME 账号 | `StateDirectory=` |
| `/run/fulcrum` | pid 文件、升级 socket、管理面 socket | `RuntimeDirectory=` |

⚠ ⚠ **`--bind-host [::]` 不是可选项 —— 只要域名有 AAAA 记录。**

`fulcrum serve` 的默认是 `0.0.0.0`，**只监听 IPv4**。而 **Let's Encrypt 在域名有 AAAA 时
优先走 IPv6 验证** ⇒ 少了这一行，现场表现是「**证书签不下来**」，而它看起来像 ACME 的问题。

> ★  上线彩排实测（ubuntu:24.04）：默认值下 `curl http://[::1]/` 返回 `000`（连不上）；
> 换成 `--bind-host '[::]'` 之后 `ss` 显示 `*:80`，IPv4 与 IPv6 都回 200 ——
> Linux `bindv6only=0` 的默认行为，pingora 侧没有设 `IPV6_V6ONLY`，所以一个 `[::]` 就是双栈。
>
> ★ ★ **门禁抓不到它**：那里所有夹具都在 `127.0.0.1` 上说话，
> **从来没有一条判据问过「IPv6 上有没有人接」**。⇒ 门禁那份 unit 也就没有这一行，
> 两份因此差这一处；这是**记录在案的有意差异**，不是漂移。

⚠ `RuntimeDirectoryPreserve=yes` 是**保险**：`RuntimeDirectory=` 默认在 unit 停止时
被删掉，而 `ExitType=cgroup` 之下换代时 unit 并不停止。
★ **没有实测过「去掉它会怎样」**——门禁里那份 unit 一直带着它。留着它成本为零。

⚠ ⚠ **`User=` / `AmbientCapabilities` 这两行门禁没有跑过** —— ✅ **owner 拍板：
就这么办，第一次真机部署时当成待验证项**（不为它单造一个门）。

容器里的场景是以 root、不带 `User=` 跑的，所以这两行只是按 G33（特权丢弃交给 systemd）
写下的形状，**没有任何一次运行证明过它**。第一次真机部署时**逐条验这三件**：

1. `systemctl start` 之后进程真的是以 `fulcrum` 身份跑的（`ps -o user= -p $(cat /run/fulcrum/fulcrum.pid)`）；
2. 80 / 443 真的绑得上（`AmbientCapabilities=CAP_NET_BIND_SERVICE` 生效）；
3. ★ **`/var/lib/fulcrum` 下证书的属主与权限** —— ACME 要能写进去，
   而 `StateDirectory=` 建目录时用的是 `User=`/`Group=`。
   ⚠ 这一条最容易出事，而它的现场表现是「证书签不下来」，看起来像 ACME 的问题。

> ★ 为什么不为它造一个门：门禁容器里没有 `User=` 的对端（要建用户、要真绑 80/443、
> 还要一套证书目录的属主断言），而这三件在**真机上一条命令就验完了**。
> 这与 G57 那条「我们对 CF/DNSPod 的理解是对的只能靠真域名证」是同一类取舍：
> **有些判据的对端只存在于生产环境，硬在门禁里造一个替身等于没有判据。**

# ⚠ ⚠ ⚠ 在中国大陆的机器上：未备案的域名，**明文 HTTP 会被运营商截走**

在 `example.com`（腾讯云，域名尚未备案）实测到的形状，逐条记下来 ——
★ 它不是枢衡的缺陷，但**它决定了这台机器上哪条 ACME 挑战路能用**：

| 打法 | 结果 | 说明 |
|---|---|---|
| `http://<公网 IP>/`（不带域名）| **421** | ★ 这是**枢衡自己**回的（无站点匹配，G63）—— 80 端口通、请求到得了机器 |
| 同一个 IP，但带 `Host: example.com` | **302 → 云厂商的 webblock 页** | 拦截挂在**明文 Host 头**上 |
| 机器上回环打自己 | **308** | 枢衡的跳转本身完全正常 |

⇒ 三条后果：

1. **自动 HTTP 重定向对公网用户不生效** —— 机器上是对的，中间被截了。
2. ⚠ ⚠ **HTTP-01（G54 的「备」）在这种机器上是死的**：CA 的验证请求同样带 Host 头，
   同样会撞上拦截页。**能签下来全靠 TLS-ALPN-01（主）与 DNS-01（通配符）**。
   ★ 换句话说：在未备案的大陆机器上，G54 那个「主/备」实际只剩主。
3. 443 不受影响（SNI 没被拦），所以在此之前一切看起来都正常 ——
   ★ **这正是它难查的地方**：证书签得下来、站点跑得好，只有明文那一跳是坏的。

> ★ 排查口径：**先用 IP 直连打一次 80**。回 421 就说明枢衡在正常工作，
> 问题在链路上；回别的（尤其是跳到某个 `webblock` / `notice` 页）就是被截了。

# ⚠ ⚠ ⚠ `Restart=` 为什么必须是 `always`，不能是 `on-failure`

**因为换代时死掉的是新一代，而老一代是正常退出的。**

在 `example.com` 上真的踩了一次：一次 `systemctl reload` 之后，
新一代在**装载阶段** panic 了，而老一代此时已经交出监听 fd、排空、正常退出（exit 0）。
systemd 看到的整件事因此是——

```text
fulcrum.service: Deactivated successfully.
```

⇒ **`Restart=on-failure` 一动不动**，服务就那么停着，直到有人手动 `systemctl start`。
★ 现场最难受的一点：`systemctl status` 说的是「成功」，而站点是死的。

| 配置 | 换代时新一代崩掉 | 手动 `systemctl stop` |
|---|---|---|
| `Restart=on-failure` | ❌ **不拉起**（unit 以成功收场）| 不拉起 ✅ |
| `Restart=always` | ✅ 拉起 | 不拉起 ✅（显式 stop 从不触发 restart）|

⚠ `always` 不会让「停机」变得拉不住：systemd 对**显式 stop** 从不重启。
它救的是「没人打算让它停，而它停了」这一类 —— 而换代恰恰是这一类里最常见的那种。

★ 这一条与 G37（`ExitType=cgroup`）是绑在一起的：正因为 unit 的生死看的是整个 cgroup，
「老的走了、新的没起来」才会被算成一次**干净的收场**。

# ★ ★ 停机要花多久（`TimeoutStopSec` 怎么定）

```text
systemctl stop 的耗时 ≈ grace_period + graceful_shutdown_timeout
                       ↑ 排空窗口        ↑ 等各 runtime 退出
```

| 项 | 默认 | 怎么改 |
|---|---|---|
| 排空窗口 | **30s** | DSL 全局块里写 `grace_period <时长>` |
| 收尾 | **5s** | 不可配（常量，见 `process.rs`）|
| **合计** | **35s** | ⇒ `TimeoutStopSec` 取 **60** 即可 |

★ **不必自己算**：进程启动时会把这个数打进日志 ——

```text
停机预算约 35s（排空 + 收尾）—— systemd unit 的 TimeoutStopSec 要大于它；换代时老一代另需最多 5s 送 fd
```

⇒ **配了 `grace_period 120s` 就照着日志里的新数字把 `TimeoutStopSec` 提上去。**

> ⚠ ⚠ **`TimeoutStopSec` 小于停机预算的后果不是「慢一点」，是硬杀连接。**
> systemd 到点就 SIGTERM → SIGKILL，而**一旦某一代开始排空，它就收不到任何
> 可捕获的信号了** —— 补一刀没用，只能等它自己走完。现场形态是
> `State 'stop-sigterm' timed out. Killing.` → `code=killed, status=9/KILL`，
> unit 以 `failed (signal)` 收场。

# 改配置：两条路，各管一半

| 路 | 命令 | 能改什么 | 代价 |
|---|---|---|---|
| 全量原子 load（G8）| `curl --unix-socket /run/fulcrum/admin.sock -X POST --data-binary @cfg.json "http://localhost/load?overrides=clear"` | 路由、上游、头、证书来源…… | 进程不换代，**改不了监听端口集**（端口变了回 **409**）；`overrides` **必填**、走查询串（G120）：发布流水线用 `clear`（回到期望状态），事故处理中想保留刚打的补丁就用 `?overrides=keep` |
| 换代（G37）| `systemctl reload fulcrum` | **一切**，含监听端口集 | 起一个新进程，老的排空后退出；零停机 |

★ `systemctl reload` 会让下一代**重新读一遍 `/etc/fulcrum/Fulcrumfile`**——
所以「改文件 + reload」就是标准的变更流程，**回滚就是把文件改回去再 reload 一次**。
两个方向都由 [`tests/m1/product.sh`](../../tests/m1/product.sh) 的 `[4/9]`/`[6/9]` 钉着。

⚠ **换代不是免费的**：老一代要先等 `CLOSE_TIMEOUT`（5s，pingora 写死）送 fd，
再走满 `grace_period`。所以两次 reload 之间至少留出这么久，否则会积起好几代同时排空。

> ★ ★ ★ **`systemctl reload` 之后的几秒内，两份配置都在服务** —— 这一条要知道。
>
> 老一代把监听 fd 交给新一代之后，**还要再 accept `CLOSE_TIMEOUT`（5 秒）**：
> 那段时间里新旧两代**在同一个监听 socket 上一起 accept**，内核在两者之间分发连接。
> ⇒ reload 刚返回就发一个请求，它可能落在老一代身上、拿到**旧配置**的响应。
>
> ⚠ 这不是缺陷，是优雅换代的固有性质（nginx 同）。但它意味着：
> **验证一次变更是否生效，要「等它收敛」，不能只打一发**。
>  CI 上真的因此红过一次 —— 本机十次都落在新一代，CI 第一次就落在了老一代。
> 判据形态见 [`tests/m1/product.sh`](../../tests/m1/product.sh) 的 `wait_body`。

# 强制续期一张证书

```bash
# 越过「还不到时候」，但**不越过退避**
curl --unix-socket /run/fulcrum/admin.sock -X POST      --data '{"domain":"*.example.com"}' http://localhost/renew

# ★ 根因已经修好（凭据补上了 / DNS 通了 / 防火墙开了），想立刻重试：
curl --unix-socket /run/fulcrum/admin.sock -X POST      --data '{"domain":"*.example.com","force":true}' http://localhost/renew
```

| | 越过「还不到时候」 | 越过退避 |
|---|---|---|
| 默认 | ✅ | ❌ |
| `"force": true` | ✅ | ✅ **并把失败计数清零** |

⚠ ⚠ **默认那一档不越过退避是有意的**：CA 的失败验证配额按**账户**算，
一个签不出来的域名反复重试能把整个账户的配额耗光，**连累同一台机器上别的域名**。
⇒ `force` 是给「根因已经修好」用的，不是重试按钮。

> ★ 在 `example.com` 上实测出这一档的必要性：DNSPod 凭据缺失让通配符
> 连失败 9 次、退避涨到 3 小时；**凭据补好之后**想立刻重试，在它之前唯一的办法是
> 去删 `meta.json` 里的失败计数 —— 而让运维手改盘上的状态文件，
> 等于把「标准动作」定义成危险动作（删错一个文件就是删掉证书）。
> ⚠ 每一次清掉非零计数都会打一条 **warn**：它是运维动作，不该悄悄发生。

# 升级二进制

换二进制走的是**同一条路**：把新文件 `mv` 到 `/usr/local/bin/fulcrum`，然后 `systemctl reload`。

```bash
install -m 0755 fulcrum.new /usr/local/bin/fulcrum.tmp
mv /usr/local/bin/fulcrum.tmp /usr/local/bin/fulcrum
systemctl reload fulcrum
```

⚠ **必须是 `mv`（rename），不能直接往原文件里写**：往一个正在跑的可执行文件里写会
`ETXTBSY`。而 rename **换的是 inode**，于是老进程的 `/proc/self/exe` 立刻变成
`/usr/local/bin/fulcrum (deleted)`。

> ★ ★ ★ **这条曾经是一个真缺陷**（实测出并当天修掉）：
> `std::env::current_exe()` **原样返回那个带 `(deleted)` 后缀的路径**，
> 拿它去 exec 必然 `ENOENT`。现场是
> `拉起下一代失败，本次换代放弃，本代继续服务：No such file or directory`——
> 服务没断（那条保险起了作用），但**升级没发生**，
> ⚠ 而 `systemctl reload` **返回的是成功**。
> ⇒ 现在的实现在 `current_exe()` 指向的文件已不存在时回落到 `argv[0]`（nginx 也走这条），
> 由 [`tests/m1/product.sh`](../../tests/m1/product.sh) 的 `[7/9]` 按 **inode** 钉住。

⚠ 不要用 `systemctl restart` 换二进制：那是一次真正的停机（排空 + 重新绑端口）。

# 上线前的检查清单

1. `fulcrum validate /etc/fulcrum/Fulcrumfile` —— 四层校验一次跑完（诊断 / 结构化配置 / 运行时图 / TLS 装载）。
2. `fulcrum plan /etc/fulcrum/Fulcrumfile` —— 看每条指令实际跑在第几步、哪些路由走了回落。
   ⚠ 它还会列出**配置里用到了但运行时还没接线**的能力，那些是真的不生效。
3. ★ `ss -ltnp | grep fulcrum` —— 域名有 AAAA 时，80/443 必须显示成 `*:80` / `*:443`
   而不是 `0.0.0.0:80`。**是 `0.0.0.0` 就说明 unit 少了 `--bind-host [::]`**，
   而它的症状会晚几分钟才出现，且长得像 ACME 的问题。
4. `systemctl start fulcrum && systemctl status fulcrum` —— 应当在一秒内 `active (running)`。
   ⚠ 若它卡住直到超时失败，看 journal 里有没有 `sd_notify 已发出：READY=1`。
5. `cat /run/fulcrum/fulcrum.pid` —— 应当等于 `MainPID`。**`ExecReload` 整条路建在它上面。**
6. `systemctl reload fulcrum` 一次，确认 pid 文件里的数变了、流量没断。

# 相关

- [进程与组件边界](/architecture/process-model.md) —— 托管形状是怎么定下来的，以及两次被实测推翻的推断
- [管理面](/architecture/control-plane.md) —— `POST /load` 的契约与 409 的理由
- [构建与验证](/platform/build-and-test.md) —— 怎么在本机跑出这个二进制
