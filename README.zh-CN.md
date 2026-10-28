# 枢衡 Fulcrum

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg?style=flat-square)](LICENSE)

<img src="assets/brand/fulcrum-logo.png" alt="Fulcrum" width="160" height="160">

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文(TW)](README.zh-TW.md) | [日本語](README.ja.md)

以 Rust 实现的 Web 服务器、反向代理与负载均衡器。

- HTTP/1.1、HTTP/2、HTTP/3 与自动 HTTPS。
- 反向代理、负载均衡及 TCP/UDP 转发。
- 静态文件、缓存、压缩、JSON 访问日志与指标。
- 一个进程、一份配置，支持 systemd 管理的升级。

## 构建

使用 Docker 从源码构建 Linux 版本：

```bash
git clone https://github.com/ShanireZ/Fulcrum.git
cd Fulcrum
docker build -f docker/Dockerfile.musl-product --target build -t fulcrum-build .
cid=$(docker create fulcrum-build) && docker cp "$cid:/fulcrum" ./fulcrum && docker rm "$cid"
```

导出的 `fulcrum` 是 musl 静态二进制。前置条件及独立的开发验证门见[构建与测试](docs/platform/build-and-test.md)。

## 使用

保存为 `Fulcrumfile`：

```text
example.com {
    tls
    reverse_proxy 127.0.0.1:8080
}
```

```bash
./fulcrum validate Fulcrumfile
./fulcrum serve Fulcrumfile
```

## 文档与许可

[配置语法](docs/architecture/dsl-reference.md) · [部署](docs/platform/deploy.md) ·
[文档](docs/index.md) · [产品范围与状态](PLAN.md)

[GNU GPL v3.0](LICENSE)。随附的 Pingora fork 保留其
[Apache-2.0 许可](vendor/pingora/LICENSE)与[修改说明](vendor/pingora/FORK.md)。
