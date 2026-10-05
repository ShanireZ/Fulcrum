# 这份数据里被改过的两处

> 本目录的原始数据与从宿主取回的原件逐字节相同，**只有两处例外**：① `env.json` 的 `attest`
> 去标识；② `oha-request.txt` 的行尾 CRLF → LF。两处都机械核过，⛔ 没有第三处。
> 规则与理由同[窗口三](../2026-10-05-window-3/REDACTION.md)（owner 2026-09-06 定、窗口二起沿用），这里只记本趟做了什么、怎么核的。

## ① `attest` 去标识

| | |
|---|---|
| 字段 | [`env.json`](env.json) → `attest` |
| 其余字段 | ⛔ **一个字都没动**（含 `qualified` / `disqualifiers` / `load_params` / `host.cpu_topology` / 全部读数） |
| 标记 | `env.json` 末尾追加了 `"attest_redacted": true` |
| 做法 | **文本替换恰好那一行**，再在末尾追加那一个键（上一行的 `}` 因此多了一个逗号）。⛔ 没有重新序列化 |

**入库的这一版**与窗口三入库版**逐字相同**（本趟运行时的声明原文与窗口三的原文相同 —— 停服前逐个核过，仍在跑的进程集合逐类相同）：

```text
专机：该机上全部应用服务与数据存储（共 6 个） 已全部停止（owner 2026-10-05 授权）· 无 TUN：网络设备只有 lo/eth0/docker0（docker0 是容器桥，非代理）· 仍在跑：一个出站隧道客户端、云厂商的两个探针、系统基础服务（chrony/cron/rsyslog/tuned/unattended-upgrades/fwupd/ModemManager/udisks2/multipathd）—— 它们仍会偶尔占用 CPU
```

与运行时原文相比，差别只有「具名 → 计数／角色」三处（被停掉的 6 个服务逐个具名 → 共 6 个 · 一个具名的隧道客户端 → 一个出站隧道客户端 ·
两个具名的云厂商探针 → 云厂商的两个探针）。

★ **机械核过，⛔ 不是自述**：对原文依次做这三处替换，结果与入库版逐字相等，且与窗口三入库版逐字相等；
解析两份 JSON，除 `attest` 与新增的 `attest_redacted` 外每个字段的值都相等；按行比，变了的只有
`attest` 那一行与末尾那三行。去标识后对整个目录扫他项目名 / IP / 主机名，零命中。
完整原文留在本地运行记录里，⛔ 不入公开仓。

⚠ 这份声明仍然只有纪律，没有门：「专机」与「无 TUN 代理」两件容器原理上看不见。⛔ 声明不是证明。

## ② `oha-request.txt` 的行尾 CRLF → LF

| | |
|---|---|
| 文件 | [`raw/short-connection-throughput/oha-request.txt`](raw/short-connection-throughput/oha-request.txt) |
| 原件 | 用例脚本把 oha 发出的请求原样落盘，HTTP 的行尾就是 CRLF —— 7 个 CR，**全部**成对出现在 CRLF 里 |
| 入库版 | 每个 CRLF 换成 LF，⛔ 别的字节一个没动：入库版 = 原件删掉全部 CR，逐字节相等（机械核过） |
| 为什么 | 本仓 `.gitattributes` 是 `* text=auto eol=lf` ⇒ 原样提交会让门禁的行尾检查当场红；owner 2026-10-05 拍的是「转 LF 并写明」 |
| 不影响判定 | 窗口清单 8d 用的 `bench_request_close_violations`（`bench/lib.sh`）自己先删 `\r` 再判 |

CRLF 的原件留在本地运行记录里。
