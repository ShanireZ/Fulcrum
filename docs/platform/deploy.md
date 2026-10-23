---
type: 技術基線
title: 部署
description: 怎麼把樞衡裝到一臺真機器上並交給 systemd 託管；停機時長怎麼算；變更與回復走哪條路。
resource: ../../tests/m1/fulcrum-prod.service
tags: [維運, 部署, systemd]
status: stable
generated:
  by: claude-code/opus-5
  at: 2026-08-21T00:00:00Z
sources:
  - id: plan-10
    resource: /references/plan.md
    title: PLAN.md §10 G31 / G33 / G34 / G37（處理程序模型與託管）、G78（產品側串接）
  - id: process-model
    resource: /architecture/process-model.md
    title: 處理程序與元件邊界 —— 託管形狀與升級視窗的實測結論
---

★ 本頁屬**技術基線**，帶真內容。它**服從** [`PLAN.md`](../../PLAN.md)；衝突時以 `PLAN.md` 為準。

# 一句話

**一個靜態二進位 + 一份 `Fulcrumfile` + 一個 systemd unit。** 沒有別的元件。

# unit 檔案

下面這份是**可以照抄的形狀**，與門禁裡跑的那份
（[`tests/m1/fulcrum-prod.service`](../../tests/m1/fulcrum-prod.service)）逐項相同，
**只差一處，而那一處是有意的**：這裡多一個 `--bind-host [::]`，理由見下面那段 ⚠ ⚠。
★ 其餘逐項相同不是巧合：門禁裡跑的形狀與文件裡寫的形狀分家的話，
文件這一份沒有任何東西在看著它。

```ini
[Unit]
Description=Fulcrum
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
# ★ 讓 unit 活過換代的是這一行，不是 MainPID 交接（G37；交接會毀掉優雅停機）。
ExitType=cgroup
NotifyAccess=main

User=fulcrum
Group=fulcrum
# 綁 80/443 需要它；不要給 CAP_NET_ADMIN 之類用不到的能力。
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

ExecStart=/usr/local/bin/fulcrum serve /etc/fulcrum/Fulcrumfile --bind-host [::]
# ⚠ **不能用 $MAINPID**：ExitType=cgroup 下它在首次換代後歸零。
ExecReload=/bin/sh -c 'kill -USR2 "$(cat /run/fulcrum/fulcrum.pid)"'

ConfigurationDirectory=fulcrum
StateDirectory=fulcrum
RuntimeDirectory=fulcrum
RuntimeDirectoryPreserve=yes

KillMode=control-group
# ★ 見下面「停機要花多久」——這個數不是拍腦袋的。
TimeoutStopSec=60
Restart=always

Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

| 目錄 | 放什麼 | 誰建 |
|---|---|---|
| `/etc/fulcrum` | `Fulcrumfile` | `ConfigurationDirectory=` |
| `/var/lib/fulcrum` | 證書儲存（`certs/`）、ACME 帳號 | `StateDirectory=` |
| `/run/fulcrum` | pid 檔案、升級 socket、管理面 socket | `RuntimeDirectory=` |

⚠ ⚠ **`--bind-host [::]` 不是可選項 —— 只要網域有 AAAA 記錄。**

`fulcrum serve` 的預設是 `0.0.0.0`，**只監聽 IPv4**。而 **Let's Encrypt 在網域有 AAAA 時
優先走 IPv6 驗證** ⇒ 少了這一行，現場表現是「**證書籤不下來**」，而它看起來像 ACME 的問題。

> ★  上線演練實測（ubuntu:24.04）：預設值下 `curl http://[::1]/` 回傳 `000`（連不上）；
> 換成 `--bind-host '[::]'` 之後 `ss` 顯示 `*:80`，IPv4 與 IPv6 都回 200 ——
> Linux `bindv6only=0` 的預設行為，pingora 側沒有設 `IPV6_V6ONLY`，所以一個 `[::]` 就是雙棧。
>
> ★ ★ **門禁抓不到它**：那裡所有夾具都在 `127.0.0.1` 上說話，
> **從來沒有一條判據問過「IPv6 上有沒有人接」**。⇒ 門禁那份 unit 也就沒有這一行，
> 兩份因此差這一處；這是**記錄在案的有意差異**，不是漂移。

⚠ `RuntimeDirectoryPreserve=yes` 是**保險**：`RuntimeDirectory=` 預設在 unit 停止時
被刪掉，而 `ExitType=cgroup` 之下換代時 unit 並不停止。
★ **沒有實測過「去掉它會怎樣」**——門禁裡那份 unit 一直帶著它。留著它成本為零。

⚠ ⚠ **`User=` / `AmbientCapabilities` 這兩行門禁沒有跑過** —— ✅ **owner 拍板：
就這麼辦，第一次真機部署時當成待驗證項**（不為它單造一個門）。

容器裡的場景是以 root、不帶 `User=` 跑的，所以這兩行只是按 G33（特權丟棄交給 systemd）
寫下的形狀，**沒有任何一次執行證明過它**。第一次真機部署時**逐條驗這三件**：

1. `systemctl start` 之後處理程序真的是以 `fulcrum` 身分跑的（`ps -o user= -p $(cat /run/fulcrum/fulcrum.pid)`）；
2. 80 / 443 真的綁得上（`AmbientCapabilities=CAP_NET_BIND_SERVICE` 生效）；
3. ★ **`/var/lib/fulcrum` 下證書的屬主與權限** —— ACME 要能寫進去，
   而 `StateDirectory=` 建目錄時用的是 `User=`/`Group=`。
   ⚠ 這一條最容易出事，而它的現場表現是「證書籤不下來」，看起來像 ACME 的問題。

> ★ 為什麼不為它造一個門：門禁容器裡沒有 `User=` 的對端（要建使用者、要真綁 80/443、
> 還要一套證書目錄的屬主斷言），而這三件在**真機上一條命令就驗完了**。
> 這與 G57 那條「我們對 CF/DNSPod 的理解是對的只能靠真網域證」是同一類取捨：
> **有些判據的對端只存在於生產環境，硬在門禁裡造一個替身等於沒有判據。**

# ⚠ ⚠ ⚠ 在中國大陸的機器上：未備案的網域，**明文 HTTP 會被營運商截走**

在 `example.com`（騰訊雲，網域尚未備案）實測到的形狀，逐條記下來 ——
★ 它不是樞衡的缺陷，但**它決定了這臺機器上哪條 ACME 挑戰路能用**：

| 打法 | 結果 | 說明 |
|---|---|---|
| `http://<公网 IP>/`（不帶網域）| **421** | ★ 這是**樞衡自己**回的（無站台匹配，G63）—— 80 連接埠通、請求到得了機器 |
| 同一個 IP，但帶 `Host: example.com` | **302 → 雲廠商的 webblock 頁** | 攔截掛在**明文 Host 頭**上 |
| 機器上回環打自己 | **308** | 樞衡的跳轉本身完全正常 |

⇒ 三條後果：

1. **自動 HTTP 重定向對公用網路使用者不生效** —— 機器上是對的，中間被截了。
2. ⚠ ⚠ **HTTP-01（G54 的「備」）在這種機器上是死的**：CA 的驗證請求同樣帶 Host 頭，
   同樣會撞上攔截頁。**能簽下來全靠 TLS-ALPN-01（主）與 DNS-01（通配符）**。
   ★ 換句話說：在未備案的大陸機器上，G54 那個「主/備」實際只剩主。
3. 443 不受影響（SNI 沒被攔），所以在此之前一切看起來都正常 ——
   ★ **這正是它難查的地方**：證書籤得下來、站台跑得好，只有明文那一跳是壞的。

> ★ 排查口徑：**先用 IP 直連打一次 80**。回 421 就說明樞衡在正常工作，
> 問題在連線路徑上；回別的（尤其是跳到某個 `webblock` / `notice` 頁）就是被截了。

# ⚠ ⚠ ⚠ `Restart=` 為什麼必須是 `always`，不能是 `on-failure`

**因為換代時死掉的是新一代，而老一代是正常退出的。**

在 `example.com` 上真的踩了一次：一次 `systemctl reload` 之後，
新一代在**裝載階段** panic 了，而老一代此時已經交出監聽 fd、排空、正常退出（exit 0）。
systemd 看到的整件事因此是——

```text
fulcrum.service: Deactivated successfully.
```

⇒ **`Restart=on-failure` 一動不動**，服務就那麼停著，直到有人手動 `systemctl start`。
★ 現場最難受的一點：`systemctl status` 說的是「成功」，而站台是死的。

| 設定 | 換代時新一代崩掉 | 手動 `systemctl stop` |
|---|---|---|
| `Restart=on-failure` | ❌ **不拉起**（unit 以成功收場）| 不拉起 ✅ |
| `Restart=always` | ✅ 拉起 | 不拉起 ✅（顯式 stop 從不觸發 restart）|

⚠ `always` 不會讓「停機」變得拉不住：systemd 對**顯式 stop** 從不重新啟動。
它救的是「沒人打算讓它停，而它停了」這一類 —— 而換代恰恰是這一類裡最常見的那種。

★ 這一條與 G37（`ExitType=cgroup`）是綁在一起的：正因為 unit 的生死看的是整個 cgroup，
「老的走了、新的沒起來」才會被算成一次**乾淨的收場**。

# ★ ★ 停機要花多久（`TimeoutStopSec` 怎麼定）

```text
systemctl stop 的耗时 ≈ grace_period + graceful_shutdown_timeout
                       ↑ 排空窗口        ↑ 等各 runtime 退出
```

| 項 | 預設 | 怎麼改 |
|---|---|---|
| 排空視窗 | **30s** | DSL 全域塊裡寫 `grace_period <时长>` |
| 收尾 | **5s** | 不可配（常數，見 `process.rs`）|
| **合計** | **35s** | ⇒ `TimeoutStopSec` 取 **60** 即可 |

★ **不必自己算**：處理程序啟動時會把這個數打進紀錄 ——

```text
停机预算约 35s（排空 + 收尾）—— systemd unit 的 TimeoutStopSec 要大于它；换代时老一代另需最多 5s 送 fd
```

⇒ **配了 `grace_period 120s` 就照著紀錄裡的新數字把 `TimeoutStopSec` 提上去。**

⚠ 這個和**在 2026-09-24 之前是少算的**：pingora 0.8.1 等完各 runtime 之後還要再 `sleep` 一次收尾時長，
真實最長是 40 秒。rebase 到 0.9.0 之後（上游 `6f15714` 刪了那次 sleep）和才與實際相符 ——
M1 產品場景實測 `systemctl stop` **30 秒**（預算 35）。`TimeoutStopSec=60` 在兩種情況下都夠。

> ⚠ ⚠ **`TimeoutStopSec` 小於停機預算的後果不是「慢一點」，是硬殺連接。**
> systemd 到點就 SIGTERM → SIGKILL，而**一旦某一代開始排空，它就收不到任何
> 可捕獲的信號了** —— 補一刀沒用，只能等它自己走完。現場形態是
> `State 'stop-sigterm' timed out. Killing.` → `code=killed, status=9/KILL`，
> unit 以 `failed (signal)` 收場。

# 改設定：兩條路，各管一半

| 路 | 命令 | 能改什麼 | 代價 |
|---|---|---|---|
| 全量原子 load（G8）| `curl --unix-socket /run/fulcrum/admin.sock -X POST --data-binary @cfg.json "http://localhost/load?overrides=clear"` | 路由、上游、頭、證書來源…… | 處理程序不換代，**改不了監聽連接埠集**（連接埠變了回 **409**）、也改不了全域的 `tls_groups` / `tls_session_resumption`（G168，變了同樣 **409**）；`overrides` **必填**、走查詢串（G120）：發佈流水線用 `clear`（回到期望狀態），事故處理中想保留剛打的補丁就用 `?overrides=keep` |
| 換代（G37）| `systemctl reload fulcrum` | **一切**，含監聽連接埠集 | 起一個新處理程序，老的排空後退出；零停機 |

★ `systemctl reload` 會讓下一代**重新讀一遍 `/etc/fulcrum/Fulcrumfile`**——
所以「改檔案 + reload」就是標準的變更流程，**回復就是把檔案改回去再 reload 一次**。
兩個方向都由 [`tests/m1/product.sh`](../../tests/m1/product.sh) 的 `[4/9]`/`[6/9]` 釘著。

⚠ **換代不是免費的**：老一代要先等 `CLOSE_TIMEOUT`（5s，pingora 寫死）送 fd，
再走滿 `grace_period`。所以兩次 reload 之間至少留出這麼久，否則會積起好幾代同時排空。

> ★ ★ ★ **`systemctl reload` 之後的幾秒內，兩份設定都在服務** —— 這一條要知道。
>
> 老一代把監聽 fd 交給新一代之後，**還要再 accept `CLOSE_TIMEOUT`（5 秒）**：
> 那段時間裡新舊兩代**在同一個監聽 socket 上一起 accept**，核心在兩者之間散佈連接。
> ⇒ reload 剛回傳就發一個請求，它可能落在老一代身上、拿到**舊設定**的回應。
>
> ⚠ 這不是缺陷，是優雅換代的固有性質（nginx 同）。但它意味著：
> **驗證一次變更是否生效，要「等它收斂」，不能只打一發**。
>  CI 上真的因此紅過一次 —— 本機十次都落在新一代，CI 第一次就落在了老一代。
> 判據形態見 [`tests/m1/product.sh`](../../tests/m1/product.sh) 的 `wait_body`。

# 強制續期一張證書

```bash
# 越過「還不到時候」，但**不越過退避**
curl --unix-socket /run/fulcrum/admin.sock -X POST      --data '{"domain":"*.example.com"}' http://localhost/renew

# ★ 根因已經修好（憑證補上了 / DNS 通了 / 防火牆開了），想立刻重試：
curl --unix-socket /run/fulcrum/admin.sock -X POST      --data '{"domain":"*.example.com","force":true}' http://localhost/renew
```

| | 越過「還不到時候」 | 越過退避 |
|---|---|---|
| 預設 | ✅ | ❌ |
| `"force": true` | ✅ | ✅ **並把失敗計數清零** |

⚠ ⚠ **預設那一檔不越過退避是有意的**：CA 的失敗驗證配額按**帳戶**算，
一個簽不出來的網域反覆重試能把整個帳戶的配額耗光，**連累同一臺機器上別的網域**。
⇒ `force` 是給「根因已經修好」用的，不是重試按鈕。

> ★ 在 `example.com` 上實測出這一檔的必要性：DNSPod 憑證缺失讓通配符
> 連失敗 9 次、退避漲到 3 小時；**憑證補好之後**想立刻重試，在它之前唯一的辦法是
> 去刪 `meta.json` 裡的失敗計數 —— 而讓維運手改盤上的狀態檔案，
> 等於把「標準動作」定義成危險動作（刪錯一個檔案就是刪掉證書）。
> ⚠ 每一次清掉非零計數都會打一條 **warn**：它是維運動作，不該悄悄發生。

# 升級二進位

換二進位走的是**同一條路**：把新檔案 `mv` 到 `/usr/local/bin/fulcrum`，然後 `systemctl reload`。

```bash
install -m 0755 fulcrum.new /usr/local/bin/fulcrum.tmp
mv /usr/local/bin/fulcrum.tmp /usr/local/bin/fulcrum
systemctl reload fulcrum
```

⚠ **必須是 `mv`（rename），不能直接往原檔案裡寫**：往一個正在跑的執行檔裡寫會
`ETXTBSY`。而 rename **換的是 inode**，於是老處理程序的 `/proc/self/exe` 立刻變成
`/usr/local/bin/fulcrum (deleted)`。

> ★ ★ ★ **這條曾經是一個真缺陷**（實測出並當天修掉）：
> `std::env::current_exe()` **原樣回傳那個帶 `(deleted)` 後綴的路徑**，
> 拿它去 exec 必然 `ENOENT`。現場是
> `拉起下一代失败，本次换代放弃，本代继续服务：No such file or directory`——
> 服務沒斷（那條保險起了作用），但**升級沒發生**，
> ⚠ 而 `systemctl reload` **回傳的是成功**。
> ⇒ 現在的實作在 `current_exe()` 指向的檔案已不存在時回落到 `argv[0]`（nginx 也走這條），
> 由 [`tests/m1/product.sh`](../../tests/m1/product.sh) 的 `[7/9]` 按 **inode** 釘住。

⚠ 不要用 `systemctl restart` 換二進位：那是一次真正的停機（排空 + 重新綁連接埠）。

## ⚠ ⚠ ★ 跨過 `G140` 那一次升級：執行緒數預設會變，而且**順序是單向的**

`G140`（2026-09-05）給 DSL 全域塊加了 `threads_l7` / `threads_l4` / `threads_admin`，
並讓 `G35` 的角色預設**真的生效** ⇒ **L7 那一格從寫死的 1 變成 CPU 核數**。
⚠ 這是一次**真實的行為變更**：資源佔用與延遲分佈都會變。

⛔ ⛔ **它不是「重新啟動就會變」，是「換了新二進位才會變」** —— 舊二進位**根本不認識**
這三個鍵。⇒ 由此得出一條**順序不能反**的紀律：

| 什麼時候寫 `threads_l7 1` | 結果 |
|---|---|
| **在換二進位之前**寫進設定 | ⛔ **舊二進位當場判錯、設定載入失敗** |
| 與新二進位**同時**或**之後**寫 | ✅ 正常 |

★ 這不是推斷，是 2026-09-06 在生產機上用 `fulcrum validate` 拿**一份獨立的最小設定**
實測出來的（⛔ 沒有碰生產設定本身）：舊二進位回
`threads_l7 ^^^^^^^^^^ 全局选项块里没有这一项` / `1 个错误`，
而同一份設定去掉那一行則正常通過。

⇒ **換二進位那一次，要麼接受新預設，要麼在同一步裡把 `threads_l7` 顯式寫死。**
⚠ 而「接受新預設」意味著執行緒數跟著那臺機器的核數走 —— 2026-09-06 實測生產機是
**2 核**，⇒ 那一跳是 1 → 2。⛔ 別把這個數抄到別的機器上，它按核數走。

⚠ ⚠ ★ **那三個角色初值今天還不是結論** —— 定它們的判據正是 M3 對拍要產出的資料
（`PLAN.md` §7 M3 逐字寫著這一條）。⇒ 在有對拍資料之前，**把它顯式釘成 1 是保守選擇**，
它讓變更時機受控，而不是由「誰下一次換二進位」決定。

# 上線前的檢查清單

1. `fulcrum validate /etc/fulcrum/Fulcrumfile` —— 四層檢核一次跑完（診斷 / 結構化設定 / 執行環境圖 / TLS 裝載）。
2. `fulcrum plan /etc/fulcrum/Fulcrumfile` —— 看每條指令實際跑在第幾步、哪些路由走了回落。
   ⚠ 它還會列出**設定裡用到了但執行環境還沒串接**的能力，那些是真的不生效。
3. ★ `ss -ltnp | grep fulcrum` —— 網域有 AAAA 時，80/443 必須顯示成 `*:80` / `*:443`
   而不是 `0.0.0.0:80`。**是 `0.0.0.0` 就說明 unit 少了 `--bind-host [::]`**，
   而它的症狀會晚幾分鐘才出現，且長得像 ACME 的問題。
4. `systemctl start fulcrum && systemctl status fulcrum` —— 應當在一秒內 `active (running)`。
   ⚠ 若它卡住直到超時失敗，看 journal 裡有沒有 `sd_notify 已发出：READY=1`。
5. `cat /run/fulcrum/fulcrum.pid` —— 應當等於 `MainPID`。**`ExecReload` 整條路建在它上面。**
6. `systemctl reload fulcrum` 一次，確認 pid 檔案裡的數變了、流量沒斷。
7. ★ **只在換二進位那一次**：先確認這臺機器上現在跑的那版**認不認識 `threads_l7`**
   （`fulcrum validate` 一份只含該鍵的最小設定即可，⛔ 別拿生產設定試）。
   認不識 ⇒ 那一步會跨過 `G140`，執行緒數預設要變 —— 順序與取捨見上面「跨過 G140 那一次升級」。

# 相關

- [處理程序與元件邊界](/architecture/process-model.md) —— 託管形狀是怎麼定下來的，以及兩次被實測推翻的推斷
- [管理面](/architecture/control-plane.md) —— `POST /load` 的契約與 409 的理由
- [建置與驗證](/platform/build-and-test.md) —— 怎麼在本機跑出這個二進位
