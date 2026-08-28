# 枢衡 Fulcrum

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg?style=flat-square)](LICENSE)

**Web 服务器 + 反向代理 + 负载均衡，Rust 实现。**

静态二进制 + Caddyfile 式配置 + 零停机换代 + HTTP/3

## Features

- **反向代理与负载均衡** —— 四种 `lb_policy`、DNS 定期重解析、主动健康检查
- **自动 HTTPS** —— ACME 三种挑战（TLS-ALPN-01 / HTTP-01 / DNS-01），原生 Cloudflare 与 DNSPod
- **静态文件** —— 目录索引、范围请求、条件请求、预压缩旁文件
- **HTTP 缓存** —— 内存 + 磁盘两级、防惊群、`POST /purge`
- **响应压缩** · **结构化访问日志**（JSON）
- **Prometheus 指标** —— 站点块里的 `metrics` 指令，九个族，零新依赖
- **HTTP/1.1 · HTTP/2 · HTTP/3**
- **L4 面** —— TCP / UDP 透传、SNI / ALPN 分流、PROXY protocol
- **零停机换代** —— systemd `Type=notify`，`systemctl reload` 换二进制、换配置、换监听端口集

## 安装

从源码构建：

```bash
git clone https://github.com/ShanireZ/Fulcrum.git && cd Fulcrum
docker build -f docker/Dockerfile.musl-product --target build -t fulcrum-build .
cid=$(docker create fulcrum-build) && docker cp "$cid:/fulcrum" ./fulcrum && docker rm "$cid"
```

`./fulcrum` 是 musl 单静态二进制（`INTERP=0 / NEEDED=0`），复制至 `/usr/local/bin/` 即可，单文件、零运行时依赖。


## 使用

`Fulcrumfile`：

```text
example.com {
    tls
    reverse_proxy 127.0.0.1:8080 127.0.0.1:8081 {
        lb_policy    least_conn
        health_uri   /healthz
    }
    encode gzip zstd
    log {
        output   /var/log/fulcrum/access.log
        headers  x-request-id
    }
}

static.example.com {
    tls
    file_server {
        root   /var/www
        index  index.html
    }
    cache {
        ttl        5m
        capacity   256m
        disk       /var/cache/fulcrum
    }
}
```

```bash
fulcrum validate Fulcrumfile   # 四层校验：诊断 → 结构化配置 → 运行时图 → TLS 装载
fulcrum plan     Fulcrumfile   # 打印执行计划：每条指令实际跑在第几步
fulcrum compile  Fulcrumfile   # DSL → 结构化配置（JSON），默认脱敏
fulcrum serve    Fulcrumfile   # 起数据面
```

`serve` 常用选项：

| 选项                 | 默认                                        |
| -------------------- | ------------------------------------------- |
| `--bind-host <H>`    | `0.0.0.0`                                   |
| `--state-dir <D>`    | `/var/lib/fulcrum`（证书存在 `<D>/certs/`） |
| `--pid-file <P>`     | `/run/fulcrum/fulcrum.pid`                  |
| `--upgrade-sock <S>` | `/run/fulcrum/upgrade.sock`                 |
| `-u, --upgrade`      | 从正在跑的旧世代接管（零停机换代）          |

配置语法逐条见 [`docs/architecture/dsl-reference.md`](docs/architecture/dsl-reference.md)；
生产部署（systemd unit、换代、证书续期）详见 [`docs/platform/deploy.md`](docs/platform/deploy.md)。

## License

本项目采用 [GNU General Public License v3.0](LICENSE)（`GPL-3.0`）发布。

`vendor/pingora/` 是 Cloudflare Pingora 的 fork，原项目按 Apache-2.0 授权，其许可证与版权声明原样保留在该目录下。
