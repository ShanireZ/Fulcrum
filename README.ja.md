# Fulcrum

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg?style=flat-square)](LICENSE)

<img src="assets/brand/fulcrum-logo.png" alt="Fulcrum" width="160" height="160">

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文(TW)](README.zh-TW.md) | [日本語](README.ja.md)

Rust 製の Web サーバー、リバースプロキシ、ロードバランサーです。

- HTTP/1.1、HTTP/2、HTTP/3 と自動 HTTPS。
- リバースプロキシ、負荷分散、TCP/UDP 転送。
- 静的ファイル、キャッシュ、圧縮、JSON アクセスログ、メトリクス。
- 単一プロセス、共通の設定、systemd によるアップグレード。

## ビルド

Docker で Linux 向けにソースからビルドします。

```bash
git clone https://github.com/ShanireZ/Fulcrum.git
cd Fulcrum
docker build -f docker/Dockerfile.musl-product --target build -t fulcrum-build .
cid=$(docker create fulcrum-build) && docker cp "$cid:/fulcrum" ./fulcrum && docker rm "$cid"
```

取り出した `fulcrum` は musl の静的バイナリです。前提条件と開発用の検証手順は[ビルドとテスト](docs/platform/build-and-test.md)を参照してください。

## 使い方

次を `Fulcrumfile` として保存します。

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

## ドキュメントとライセンス

[設定構文](docs/architecture/dsl-reference.md) · [デプロイ](docs/platform/deploy.md) ·
[ドキュメント](docs/index.md) · [製品の範囲と状況](PLAN.md)

[GNU GPL v3.0](LICENSE)。同梱する Pingora fork の
[Apache-2.0 ライセンス](vendor/pingora/LICENSE)と[変更記録](vendor/pingora/FORK.md)は維持しています。
