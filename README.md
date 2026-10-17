# 樞衡 Fulcrum

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg?style=flat-square)](LICENSE)

**Web 伺服器 + 反向代理 + 負載均衡，Rust 實作。**

靜態二進位 + Caddyfile 式設定 + 零停機換代 + HTTP/3

## Features

- **反向代理與負載均衡** —— 四種 `lb_policy`、DNS 定期重解析、主動健康檢查
- **自動 HTTPS** —— ACME 三種挑戰（TLS-ALPN-01 / HTTP-01 / DNS-01），原生 Cloudflare 與 DNSPod
- **靜態檔案** —— 目錄索引、範圍請求、條件請求、預壓縮旁檔案
- **HTTP 快取** —— 記憶體 + 磁碟兩級、防驚群、`POST /purge`
- **回應壓縮** · **結構化訪問紀錄**（JSON）
- **Prometheus 指標** —— 站台塊裡的 `metrics` 指令，零新相依
- **HTTP/1.1 · HTTP/2 · HTTP/3**
- **L4 面** —— TCP / UDP 透傳、SNI / ALPN 分流、PROXY protocol
- **管理面**（Unix socket，預設不出機器）—— 全量原子 `POST /load`（`overrides` 必填：`keep` / `clear`）·
  增量 `POST /runtime` 改**臨時覆蓋層**（摘上游 / 改權重，不持久化但 `GET /stats` 裡永遠看得見）·
  `POST /renew` · `POST /purge` · 唯讀快照 `GET /stats`
- **零停機換代** —— systemd `Type=notify`，`systemctl reload` 換二進位、換設定、換監聽連接埠集

## 安裝

從原始碼建置：

```bash
git clone https://github.com/ShanireZ/Fulcrum.git && cd Fulcrum
docker build -f docker/Dockerfile.musl-product --target build -t fulcrum-build .
cid=$(docker create fulcrum-build) && docker cp "$cid:/fulcrum" ./fulcrum && docker rm "$cid"
```

`./fulcrum` 是 musl 單靜態二進位（`INTERP=0 / NEEDED=0`），複製至 `/usr/local/bin/` 即可，單檔案、零執行環境相依。


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
fulcrum validate Fulcrumfile   # 四層檢核：診斷 → 結構化設定 → 執行環境圖 → TLS 裝載
fulcrum plan     Fulcrumfile   # 列印執行計畫：每條指令實際跑在第幾步
fulcrum compile  Fulcrumfile   # DSL → 結構化設定（JSON），預設脫敏
fulcrum serve    Fulcrumfile   # 起資料面
```

`serve` 常用選項：

| 選項                 | 預設                                        |
| -------------------- | ------------------------------------------- |
| `--bind-host <H>`    | `0.0.0.0`                                   |
| `--state-dir <D>`    | `/var/lib/fulcrum`（證書存在 `<D>/certs/`） |
| `--pid-file <P>`     | `/run/fulcrum/fulcrum.pid`                  |
| `--upgrade-sock <S>` | `/run/fulcrum/upgrade.sock`                 |
| `-u, --upgrade`      | 從正在跑的舊世代接管（零停機換代）          |

設定語法逐條見 [`docs/architecture/dsl-reference.md`](docs/architecture/dsl-reference.md)；
生產部署（systemd unit、換代、證書續期）詳見 [`docs/platform/deploy.md`](docs/platform/deploy.md)。

## License

本專案採用 [GNU General Public License v3.0](LICENSE)（`GPL-3.0`）發佈。

`vendor/pingora/` 是 Cloudflare Pingora 的 fork，原專案按 Apache-2.0 授權，其授權條款與版權宣告原樣保留在該目錄下。
