# 這份資料裡被改過的兩處

> 本目錄的原始資料與從宿主取回的原件逐位元組相同，**只有兩處例外**：① `env.json` 的 `attest`
> 去標識；② `oha-request.txt` 的行尾 CRLF → LF。兩處都機械核過，⛔ 沒有第三處。
> 規則與理由同[視窗三](../2026-10-05-window-3/REDACTION.md)（owner 2026-09-06 定、視窗二起沿用），這裡只記本趟做了什麼、怎麼核的。

## ① `attest` 去標識

| | |
|---|---|
| 欄位 | [`env.json`](env.json) → `attest` |
| 其餘欄位 | ⛔ **一個字都沒動**（含 `qualified` / `disqualifiers` / `load_params` / `host.cpu_topology` / 全部讀數） |
| 標記 | `env.json` 末尾追加了 `"attest_redacted": true` |
| 做法 | **文字替換恰好那一行**，再在末尾追加那一個鍵（上一行的 `}` 因此多了一個逗號）。⛔ 沒有重新序列化 |

**入庫的這一版**與視窗三入庫版**逐字相同**（本趟執行環境的宣告原文與視窗三的原文相同 —— 停服前逐個核過，仍在跑的處理程序集合逐類相同）：

```text
专机：该机上全部应用服务与数据存储（共 6 个） 已全部停止（owner 2026-10-05 授权）· 无 TUN：网络设备只有 lo/eth0/docker0（docker0 是容器桥，非代理）· 仍在跑：一个出站隧道客户端、云厂商的两个探针、系统基础服务（chrony/cron/rsyslog/tuned/unattended-upgrades/fwupd/ModemManager/udisks2/multipathd）—— 它们仍会偶尔占用 CPU
```

與執行環境原文相比，差別只有「具名 → 計數／角色」三處（被停掉的 6 個服務逐個具名 → 共 6 個 · 一個具名的隧道用戶端 → 一個出站隧道用戶端 ·
兩個具名的雲廠商探針 → 雲廠商的兩個探針）。

★ **機械核過，⛔ 不是自述**：對原文依次做這三處替換，結果與入庫版逐字相等，且與視窗三入庫版逐字相等；
解析兩份 JSON，除 `attest` 與新增的 `attest_redacted` 外每個欄位的值都相等；按行比，變了的只有
`attest` 那一行與末尾那三行。去標識後對整個目錄掃他專案名 / IP / 主機名，零命中。
完整原文留在本機執行記錄裡，⛔ 不入公開倉。

⚠ 這份宣告仍然只有紀律，沒有門：「專機」與「無 TUN 代理」兩件容器原理上看不見。⛔ 宣告不是證明。

## ② `oha-request.txt` 的行尾 CRLF → LF

| | |
|---|---|
| 檔案 | [`raw/short-connection-throughput/oha-request.txt`](raw/short-connection-throughput/oha-request.txt) |
| 原件 | 用例指令碼把 oha 發出的請求原樣寫入磁碟，HTTP 的行尾就是 CRLF —— 7 個 CR，**全部**成對出現在 CRLF 裡 |
| 入庫版 | 每個 CRLF 換成 LF，⛔ 別的位元組一個沒動：入庫版 = 原件刪掉全部 CR，逐位元組相等（機械核過） |
| 為什麼 | 本儲存庫 `.gitattributes` 是 `* text=auto eol=lf` ⇒ 原樣提交會讓門禁的行尾檢查當場紅；owner 2026-10-05 拍的是「轉 LF 並寫明」 |
| 不影響判定 | 視窗清單 8d 用的 `bench_request_close_violations`（`bench/lib.sh`）自己先刪 `\r` 再判 |

CRLF 的原件留在本機執行記錄裡。
