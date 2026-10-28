# 樞衡 Fulcrum

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg?style=flat-square)](LICENSE)

<img src="assets/brand/fulcrum-logo.png" alt="Fulcrum" width="160" height="160">

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文(TW)](README.zh-TW.md) | [日本語](README.ja.md)

以 Rust 實作的 Web 伺服器、反向代理與負載均衡器。

- HTTP/1.1、HTTP/2、HTTP/3 與自動 HTTPS。
- 反向代理、負載均衡及 TCP/UDP 轉送。
- 靜態檔案、快取、壓縮、JSON 存取紀錄與指標。
- 一個處理程序、一份設定，支援 systemd 管理的升級。

## 建置

使用 Docker 從原始碼建置 Linux 版本：

```bash
git clone https://github.com/ShanireZ/Fulcrum.git
cd Fulcrum
docker build -f docker/Dockerfile.musl-product --target build -t fulcrum-build .
cid=$(docker create fulcrum-build) && docker cp "$cid:/fulcrum" ./fulcrum && docker rm "$cid"
```

匯出的 `fulcrum` 是 musl 靜態二進位。前置條件及獨立的開發驗證門見[建置與測試](docs/platform/build-and-test.md)。

## 使用

儲存為 `Fulcrumfile`：

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

## 文件與授權

[設定語法](docs/architecture/dsl-reference.md) · [部署](docs/platform/deploy.md) ·
[文件](docs/index.md) · [產品範圍與狀態](PLAN.md)

[GNU GPL v3.0](LICENSE)。隨附的 Pingora fork 保留其
[Apache-2.0 授權](vendor/pingora/LICENSE)與[修改說明](vendor/pingora/FORK.md)。
