#!/usr/bin/env bash
# 資料面端到端：起兩個 `fulcrum serve`，用真流量驗證路由決策**被執行對了**。
#
# ★ ★ 這一層與 `crates/fulcrum-runtime/tests/routing.rs` 分工明確：
#   那邊測「該做什麼」（純邏輯、脫網、紅了指到具體規則），
#   這邊測「做出來了沒有」（真 socket、真 HTTP、真上游）。
#   **兩邊都要有**：只有前者，一個把決策算對卻寫錯回應的資料面照樣全綠；
#   只有後者，一條規則錯了只會得到一個狀態碼，查起來要靠猜。
#
# ★ **上游就是樞衡自己**（另一個 `fulcrum serve` 執行個體）。這不是偷懶：
#   它讓整套測試**零外部相依**，而且上游那邊用 `{path}` / `{header.X-Up}`
#   把收到的東西回顯出來——於是「rewrite 有沒有帶到上游」「header_up 有沒有生效」
#   這兩件事可以被**看見**，而不是只能推斷。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
PROXY_PORT=${PROXY_PORT:-9100}
UP_PORT=${UP_PORT:-9101}
NAMED_PORT=${NAMED_PORT:-9102}
TLS_PORT=${TLS_PORT:-9103}
# ★ 批 11（主動健康檢查）用的兩個上游：
#   SICK_PORT —— 處理程序好好的、業務路徑回 200，**但探測路徑回 500**；
#   LATE_PORT —— 一開始**沒人在聽**，測到一半才起來（驗「摘得掉」也「回得來」）。
# ⚠ 名字有意不叫 `*UP_PORT`：下面那條 `sed "s/:UP_PORT/…/g"` 是全域替換，
#   而本檔案已經為「一條只認得一種寫法的替換」栽過一次。
SICK_PORT=${SICK_PORT:-9104}
LATE_PORT=${LATE_PORT:-9105}
# ★ ★ G136（被動熔斷）用的上游：**業務路徑就回 500**。
#   ⚠ 它與 SICK_PORT 恰好相反，而這個對照就是兩個特性的分界：
#   SICK 是「探測路徑壞、業務好」——只有**主動**健康檢查看得見；
#   BAD  是「業務就是壞的，探測路徑無所謂」——只有**被動**熔斷看得見。
BAD_PORT=${BAD_PORT:-9106}
# 執行緒模型那一節（G140）自己的三個連接埠。★ 三個執行個體**同時只起一個**，
# 但連接埠仍各佔一格：重複使用同一個連接埠時，上一個還沒完全放開就會讓下一個「起不來」，
# 而症狀落在一個與執行緒數毫無關係的地方。
THREADS_PORT_A=${THREADS_PORT_A:-9107}
THREADS_PORT_B=${THREADS_PORT_B:-9108}
THREADS_PORT_C=${THREADS_PORT_C:-9109}
# ⚠ 9107 曾經是空的（回落層刪除後退場），**2026-09-05 被上面那一組收走了**（G140）。
#   連接埠表（docs/platform/host-and-gate-traps.md）已跟著從 9100–9106 擴到 9100–9109。
# ★ 第 13 節（G163）的上游：一個 python 小服務，把收到的請求頭名**原樣**回成正文 ——
#   樞衡當上遊的 `respond` 只認得值、看不出頭名的大小寫。連接埠表跟著擴到 9100–9110。
ECHO_PORT=${ECHO_PORT:-9110}
# ★ 管理面走 Unix socket（G14），所以它**不佔連接埠**，也不進上面那張連接埠表。
ADMIN_SOCK="$WORK/admin.sock"

FAILS=0
PIDS=()

fail() { echo "  ✗ $*" >&2; FAILS=$((FAILS + 1)); }
# 既不算過也不算不過的一行現場記錄。★ 用它把「這條判據本趟分辨不出來」說出口，
# ⛔ 別靜默跳過 —— 一條被跳過的判據與一條通過的判據在輸出上長得一模一樣。
note() { echo "  · $*"; }
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid
  # ★ 先還原 /etc/hosts —— 備份就在 $WORK 裡，而本函式末尾會把 $WORK 刪掉。
  #   這臺容器後面還要跑別的場景，留下一條 rotate.test 是給後面下絆子。
  [ -f "$WORK/hosts.orig" ] && cat "$WORK/hosts.orig" > /etc/hosts 2>/dev/null
  true
  # ★ 用 SIGINT 而不是 SIGTERM：Pingora 把 SIGTERM 當**優雅停機**，會等完整的排空視窗；
  #   SIGINT 是立刻退。測試收尾要的是後者——等排空只會讓每次跑都慢幾秒，
  #   而 bash 隨後還會為被 SIGKILL 收掉的後台任務打一行 `Killed` 噪音。
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  # ★ 等它們真的走掉。不等的話，下一次跑會撞上「連接埠還被佔著」，
  #   而那看起來像是本次的新問題。
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -9 "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}

wait_port() {
  local port=$1 tries=0
  while [ "$tries" -lt 100 ]; do
    if port_listening "$port"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

# 等一個連接埠**放開**。★ 執行緒模型那一節要連起三個執行個體，
# 上一個還攥著連接埠時下一個會「起不來」，而症狀落在一個與執行緒數無關的地方。
wait_port_gone() {
  local port=$1 tries=0
  while [ "$tries" -lt 100 ]; do
    if ! port_listening "$port"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

# ── [0/4] 基線：四個連接埠必須**都還沒被佔** ──────────────────────────────────
#
# ★ ★ 這一步不是形式。本儲存庫栽過一次：一個「基線探針」對著**上一個場景遺留的處理程序**
#   報了綠。連接埠沒清乾淨時，後面每一條斷言測的都是別人的服務。
echo "=== [0/4] 基线：端口未被占用 ==="
for p in "$PROXY_PORT" "$UP_PORT" "$NAMED_PORT" "$TLS_PORT" "$SICK_PORT" "$LATE_PORT" "$BAD_PORT" "$ECHO_PORT"; do
  if port_listening "$p"; then
    echo "SERVE TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑，否则下面测的是别人的服务。" >&2
    exit 1
  fi
done
ok "$PROXY_PORT / $UP_PORT / $NAMED_PORT / $TLS_PORT / $SICK_PORT / $LATE_PORT / $BAD_PORT / $ECHO_PORT 都是空的"

[ -x "$BIN" ] || {
  echo "SERVE TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 設定 ────────────────────────────────────────────────────────────────────
#
# 上游把收到的**路徑**與 `X-Up` 頭回顯出來 —— 於是 rewrite 與 header_up
# 是否真的生效，可以直接從回應體裡讀出來。
# ★ 回落層已整層刪除（G98），它留下的那句話仍然寫在資料面的模組文件裡：
#   **501 與 502 是兩個不同的事實，不許合併** —— 合併之後，一次設定遺漏
#   與一次後端故障在現場長得一模一樣。
cat > "$WORK/upstream.Fulcrumfile" <<'CONF'
:UP_PORT {
    respond 200 "up path={path} xup={header.X-Up}"
}
CONF
sed -i "s/:UP_PORT/:$UP_PORT/" "$WORK/upstream.Fulcrumfile"

# ★ ★ ★ 「病號」上游：**業務路徑完全正常，只有探測路徑回 500**。
#
#   它是本批最重要的那個夾具。⚠ 少了它，「健康檢查」與「連得上嗎」
#   在判據上分不開 —— 而一個只會 TCP 連一下的實作會全綠。
cat > "$WORK/sick.Fulcrumfile" <<'CONF'
:SICK_PORT {
    # ⚠ 回落层整层删除（G98）之后，这里那条守「没配回落 ⇒ 501」的判据没了对象。
    #   ★ **改判据的时候要看清楚原来那条在守什么，别把它一起搬走。**
    handle /health {
        respond 500 sick-health
    }
    handle {
        respond 200 "sick but serving path={path}"
    }
}
CONF
sed -i "s/:SICK_PORT/:$SICK_PORT/" "$WORK/sick.Fulcrumfile"

# ★ ★ ★ G136 的「壞」上游：**業務路徑就回 500**，處理程序活得好好的。
#
#   ⚠ 它是被動熔斷這一節唯一的夾具，而它的價值在於**主動健康檢查對它無能為力**：
#   一個只探 `/health` 的實作會認為它健康（這裡連 `health_uri` 都沒配），
#   而真實流量每一次都拿到 500。⇒ PLAN 給 `passive_fail` 的立身理由就是這一格。
cat > "$WORK/bad.Fulcrumfile" <<'CONF'
:BAD_PORT {
    respond 500 "bad upstream"
}
CONF
sed -i "s/:BAD_PORT/:$BAD_PORT/" "$WORK/bad.Fulcrumfile"

# 「遲到」的上游：這份設定先寫好，但**處理程序要到本場景中途才起**。
# ★ ★ 一條值得繼承的取值方法（快取那一格用的也是它）：
#   讓上游回顯一個用戶端現給的值，把「這份東西是誰給的」變成**看得見**的。

cat > "$WORK/late.Fulcrumfile" <<'CONF'
:LATE_PORT {
    respond 200 "late path={path}"
}
CONF
sed -i "s/:LATE_PORT/:$LATE_PORT/" "$WORK/late.Fulcrumfile"

cat > "$WORK/proxy.Fulcrumfile" <<'CONF'
{
    admin unix/ADMIN_SOCK
    # ★ 全局 `default_sni`：客户端不带 SNI 时当作它报了这个名字（判据 9d）。
    # ⚠ 它同时让 9c 那条**变得有意义** —— 配着 default_sni 还要拒掉未知 SNI，
    #   才说明这条指令没有顺手变成「谁来都发一张证书」的兜底。
    default_sni secure.example
}

# ⚠ ⚠ ⚠ **这个站点里「写了 `id`」与「没写 `id`」两种形状都是有意的**
#   （M2 批 N 任务 2.9 / G125，裁决 R6 ③ **第二轮**）。
#   覆盖层的键是 `(站点名, id, 归一化后的上游地址)`，而代码按「键相同 / 键不同」分支
#   ⇒ 夹具必须**两种形状都有**，否则其中一条路在端到端这一层一次都没走过
#   （AGENTS.md 门禁纪律第一条：盲区在夹具里）。
#   ① **没写 `id`**：`/api` · `/rw` · `/cached` · `/hc` 四条都指着 `127.0.0.1:UP_PORT`
#      ⇒ 四条的键**完全相同**，共享同一个覆盖格子，一次 `disable` 四条一起摘掉。
#      ★ 这正是反代最常见的写法（一个后端挂在几组 `handle` 路由后面），而它
#      **一个字节都不用改就装得上** —— 这份夹具就是那句话的实测。
#      ⚠ 任务 2.8 曾照第一轮口径把这个形状在装载期拒掉，四个端到端场景当场全红。
#   ② **写了 `id`**：`/sick` 与 `/sickok` 都指着 `127.0.0.1:SICK_PORT`，而两条的
#      `health_status` 口径不同 ⇒ 写 `id` 才能把它们分成两格、各摘各的。
:PROXY_PORT {
    header X-Fulcrum test
    @api path /api/*
    handle @api {
        reverse_proxy 127.0.0.1:UP_PORT {
            header_up X-Up 1
            passive_fail 3
        }
    }
    handle /rw/* {
        rewrite * /rewritten
        reverse_proxy 127.0.0.1:UP_PORT
    }
    handle /redir {
        redir * https://example.com/moved 301
    }
    # ⚠ ⚠ **这一格（M2 批 F）换了方向。**
    #   旧契约：`file_server` 回落给 nginx ⇒ `/static/x` 应当回 200 且**响应体来自
    #   回落后端**（`fallback-nginx path=/static/x`）。
    #   新契约：`file_server` **自研** ⇒ `/static/x` 应当回 200 且响应体是**磁盘上那个文件**。
    #   ★ 换的不是「测不测回落」，是「file_server 归谁」——
    #   「回落已接线 ⇒ 转发」那条判据由下面的 `/cached/*` 全额接住，一条都没少。
    handle /static/* {
        file_server {
            root WWW_ROOT
        }
    }
    # ⚠ ⚠ **这一格（M2 批 G）换了方向。**
    #   旧契约：`cache` 回落给 nginx ⇒ `/cached/x` 回 200 且**响应体来自回落后端**。
    #   新契约：`cache` **自研** ⇒ 它是**中间件**，裹住下面那条 `reverse_proxy`。
    #   ★ 这里只验「配了 `cache` 不会把转发弄坏」—— 缓存语义本身由
    #   **`tests/cache/run.sh`** 管。⚠ 两处各测各的，别在这里重复一遍：
    #   一份夹具同时背两套判据，改的时候两套都会被顺手改坏。
    handle /cached/* {
        cache {
            ttl 30s
        }
        reverse_proxy 127.0.0.1:UP_PORT
    }
    handle /err {
        respond 418 teapot
    }
    handle /byname/* {
        reverse_proxy localhost:UP_PORT
    }
    handle /deadname/* {
        reverse_proxy no-such-host-zzz.invalid:80
    }
    # ★ 第 13 节（G163）：上游把收到的头名原样回成正文。
    handle /echo-case/* {
        reverse_proxy 127.0.0.1:ECHO_PORT
    }
    handle /rotate/* {
        reverse_proxy rotate.test:UP_PORT {
            dns_refresh 5s
        }
    }
    handle /hc/* {
        reverse_proxy 127.0.0.1:UP_PORT 127.0.0.1:LATE_PORT {
            health_uri /health
            health_interval 1s
            health_timeout 1s
        }
    }
    handle /sick/* {
        reverse_proxy 127.0.0.1:SICK_PORT {
            id sick
            health_uri /health
            health_interval 1s
            health_timeout 1s
        }
    }
    handle /sickok/* {
        reverse_proxy 127.0.0.1:SICK_PORT {
            id sickok
            health_uri /health
            health_status 5xx
            health_interval 1s
            health_timeout 1s
        }
    }
    # ── ★ ★ ★ G136：被动熔断。**两条路指着同一对上游，只差一行 `passive_fail`。**
    #
    #   ⚠ ⚠ 这个对照本身就是反证，而且是**一趟里同时验真假两侧**那一种
    #   （与 `tests/m0/unclaimed.sh` 同一手法）：
    #   两条 `handle` 各有自己的 `ProxyTarget` ⇒ 各有自己的一组 `Upstream` 与熔断状态，
    #   于是「打 10 个请求拿到几个 500」这一个读数在两条路上必须给出**不同**的答案。
    #   ⛔ 少了 `/nopf`，一个「熔断根本没生效、只是恰好后面几个请求轮到好上游」的实现
    #   照样能让 `/pf` 那一侧看起来对。
    #   ★ 坏上游写在**前面**：轮询游标从 0 起，于是第 1 个请求就落在它身上。
    handle /pf/* {
        reverse_proxy 127.0.0.1:BAD_PORT 127.0.0.1:UP_PORT {
            id pf
            passive_fail 2
            # ⚠ ⚠ 两个旋钮**显式写死 60s**，⛔ 不吃缺省（10s / 30s）：
            #   本机并发实测比空载慢 3.75 倍，而缺省窗口只有 10s ——
            #   若窗口在两次失败之间过期，计数会重来、这条判据就**周期性假红**。
            #   ★ 一道假红过几次的门会被人忽略，那比它不存在更坏。
            passive_window 60s
            passive_cooldown 60s
        }
    }
    handle /nopf/* {
        # ★ ★ `tracing` 是**这份夹具里仅存的未接线能力**（G60：预留指令，不产生行为）。
        #   ⚠ 它在这里的唯一用途是给下面那条「未接线公告」断言当锚点 ——
        #   G136 把 `passive_fail` 接线之后，原来的锚点当场失效并**红给我看了**，
        #   那是它在正常工作。⛔ 别因为「这行看起来没用」就删掉它。
        tracing
        reverse_proxy 127.0.0.1:BAD_PORT 127.0.0.1:UP_PORT {
            id nopf
        }
    }
    # ★ ★ ★ **自环：一个装在容器里的 `reverse_proxy` 指回本进程自己的监听口。**
    #   ⚠ 有意**嵌在 `handle` arm 里**：`crates/fulcrum-runtime/tests/fallback.rs`
    #   那条单测用的是**扁平**站点（`:9999 { reverse_proxy 127.0.0.1:9999 }`），
    #   于是「容器要下钻才找得到」这一半在任何一层都没有判据。
    #   ⛔ **不许给这条路径打真流量** —— 真打进去就是一个无限转发循环。
    handle /selfloop/* {
        reverse_proxy 127.0.0.1:PROXY_PORT
    }
    # ★ 第 10 节（写缓冲，G155）的两条：正文是下面 sed 现填的 4096 / 32768 个字母。
    handle /wbuf/4k {
        respond 200 "WBUF_4K"
    }
    handle /wbuf/32k {
        respond 200 "WBUF_32K"
    }
    handle {
        respond 200 root
    }
}

http://only.example:NAMED_PORT {
    respond 200 named-only
}

# ★ 第 11 节（HTTP 层诊断发现 2 · 请求头不整份克隆）：错误页里的 `{header.X}` 必须取**原始**请求头。
#   页里带 `{path}`，是为了让两条错误的正文互不相同（两者的错误码恰好都是 404）。
http://errs.example:NAMED_PORT {
    handle /static/* {
        file_server {
            root WWW_ROOT
        }
    }
    handle_errors {
        respond 418 "{status} {path} probe={header.X-Probe}"
    }
}

secure.example:TLS_PORT {
    tls TLS_CRT TLS_KEY
    # ★ 第 10 节（写缓冲，G155）：TLS 记录在写缓冲之上 ⇒ 这条量的是「头与正文两个记录也一次发出」。
    #   ⚠ 原来那句 `respond 200 secure-ok` 挪进了兜底 `handle`，`/` 的行为一个字节没变（9a 守着）。
    handle /wbuf/4k {
        respond 200 "WBUF_4K"
    }
    handle {
        respond 200 secure-ok
    }
}
CONF

# ── 自簽證書（給 `tls <cert> <key>` 那條路用）────────────────────────────────
#
# ★ 用 openssl 現簽一張，而**不是**把一張證書提交進儲存庫：
#   儲存庫裡的測試證書遲早會過期，而過期那天紅的是「TLS 壞了」，
#   查起來要繞一圈才發現是證書到期。現簽的永遠新鮮。
# ⚠ SAN 必須有 `secure.example`：樞衡是**按證書自己的 SAN** 決定這張證書裝在哪些
#   SNI 上的（設定裡寫的站台名只用來核對），只給 CN 是裝不上的。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=secure.example" \
  -addext "subjectAltName=DNS:secure.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "SERVE TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ⚠ `UP_PORT` 在兩種前綴下都出現（`127.0.0.1:` 與 `localhost:`），兩邊都要替。
#   ★ **一條只認得一種寫法的替換，在另一種寫法上等於沒有替換。**
sed -i "s/:PROXY_PORT/:$PROXY_PORT/; s/:UP_PORT/:$UP_PORT/g; s/:NAMED_PORT/:$NAMED_PORT/; s/:TLS_PORT/:$TLS_PORT/; s/:SICK_PORT/:$SICK_PORT/g; s/:LATE_PORT/:$LATE_PORT/g; s/:BAD_PORT/:$BAD_PORT/g; s/:ECHO_PORT/:$ECHO_PORT/g" \
  "$WORK/proxy.Fulcrumfile"
# ★ 自研 file_server 的根 —— 內容在這裡現造（M2 批 F）。
#   ⚠ 必須是**絕對路徑**（G91），`$WORK` 本來就是。
mkdir -p "$WORK/www/static"
printf 'self-built-file-server\n' > "$WORK/www/static/x"
# 路徑裡有斜槓，用 | 當分隔符
sed -i "s|TLS_CRT|$WORK/tls.crt|; s|TLS_KEY|$WORK/tls.key|; s|ADMIN_SOCK|$ADMIN_SOCK|; s|WWW_ROOT|$WORK/www|" \
  "$WORK/proxy.Fulcrumfile"
# ★ 第 10 節（寫緩衝，G155）的兩份正文：4096 與 32768 個字母，現造。
#   ⚠ 只用字母：它們要原樣塞進 `respond` 的引號裡，還要過一遍 sed。
BODY_4K=$(head -c 4096 /dev/zero | tr '\0' 'w')
BODY_32K=$(head -c 32768 /dev/zero | tr '\0' 'W')
sed -i "s/WBUF_4K/$BODY_4K/g; s/WBUF_32K/$BODY_32K/" "$WORK/proxy.Fulcrumfile"

# ── ★ ★ ★ 會變的網域：給「後台重解析」準備夾具（批 10）──────────────────────
#
# ⚠ `/byname/` 與 `/deadname/` 只驗到**啟動那一次**解析對了。
#   「每 N 秒重解析一次」是另一件事，而它壞掉的樣子**完全無聲**：
#   啟動那次照常成功，此後位址永遠停在啟動時那一份 —— 這正是 nginx OSS
#   那個經典事故（`dns_refresh` 這條指令存在的全部理由）。
#   ⇒ 判據必須讓**同一個網域在處理程序活著的時候換 IP**，再看它跟不跟得上。
#
# ★ 手法：直接改容器的 `/etc/hosts`。glibc 的 nss_files 每次查詢都重讀它，
#   處理程序內沒有快取 —— 所以這是一次真的「DNS 答案變了」。
# ⚠ Docker 把 /etc/hosts 做成 bind-mount 的**檔案**：只能**就地截斷重寫**
#   （`cat >`）。`sed -i` / `mv` 要換 inode，在 bind-mount 的檔案上做不到。
cp /etc/hosts "$WORK/hosts.orig"
set_rotate_ip() {
  { cat "$WORK/hosts.orig"; echo "$1 rotate.test"; } > /etc/hosts 2>/dev/null
}

# ⚠ 寫不進去就**當場退出**，不「跳過」。
#   ★ 紀律：「沒能檢查」不許當成「檢查通過」。
if ! set_rotate_ip 127.0.0.1; then
  echo "SERVE TESTS FAILED: 改不了 /etc/hosts —— 后台重解析那一节没法验。" >&2
  echo "  容器默认以 root 跑；传了 DOCKER_USER 就会撞到这里。" >&2
  exit 1
fi
ok "rotate.test 先指向真上游（127.0.0.1）"

# ── [1/4] 起服務 ────────────────────────────────────────────────────────────
echo "=== [1/4] 起上游与代理 ==="
start() {
  local name=$1 conf=$2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
start upstream "$WORK/upstream.Fulcrumfile"
start sick "$WORK/sick.Fulcrumfile"
start bad "$WORK/bad.Fulcrumfile"
# ⚠ `late` **故意不起** —— 它要到 [2c/4] 那一節才上場。
start proxy "$WORK/proxy.Fulcrumfile"

for p in "$UP_PORT" "$SICK_PORT" "$BAD_PORT" "$PROXY_PORT" "$NAMED_PORT" "$TLS_PORT"; do
  wait_port "$p" || {
    echo "SERVE TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "七个监听都起来了（迟到那个还没起，那是有意的）"
# ★ ★ 一條「靠某個前提成立」的判據，要**把那個前提也測一遍** ——
#   否則前提悄悄不成立的那天，判據仍然是綠的。

# ── [2/4] 斷言 ──────────────────────────────────────────────────────────────
echo "=== [2/4] 断言 ==="

# ★ ★ 統一的 curl 包裝。**結束代碼與 `-w` 的輸出必須分開拿。**
#
# ⚠ 不要寫成 `curl … -w '%{http_code}' || echo "000"`：curl 連不上時 `-w`
#   **已經輸出了 `000`**，`|| echo` 再補一個就成了 `000000`。
#   ★ **先弄清工具已經做了什麼，再決定補什麼。**
# 只取 `-w` 的輸出（不關心結束代碼）。
# ⚠ `|| true` 是必需的：`set -e` 之下，賦值裡的命令替換失敗會讓整個指令碼退出。
run_curl() {
  curl "$@" 2>/dev/null || true
}

# ★ ★ 要**結束代碼**時不能走命令替換。
#
# ⚠ 不要寫成「函式裡 `CURL_RC=$?`、呼叫方 `X=$(run_curl …)`」：
#   **命令替換開的是子 shell，裡面設的變數不會傳回父 shell**，
#   於是 `CURL_RC` 永遠是初值 0，讀起來像「curl 成功了」。
# 結果寫檔案，結束代碼留在當前 shell 的 `CURL_RC` 裡。
CURL_RC=0
curl_capture() {
  set +e
  curl "$@" > "$WORK/curlw" 2>/dev/null
  CURL_RC=$?
  set -e
}

# ★ ★ ★ 通用版（任意命令可能非零退出、又想要它的輸出時，用它代替裸的
#   `VAR=$(cmd)`；修復輪 2，評審 N1）連同「它為什麼必須存在」都搬進了
#   `tests/lib/capture.sh`（任務 7 收斂：本檔案與 `tests/metrics/run.sh` 曾各有一份）。
#   ⚠ 上面那個 `curl_capture()` **有意留在本檔案**：它只服務 curl，且把正文落到
#   `$WORK/curlw` 而不是變數裡，與通用版不是同一件事。
# shellcheck source=tests/lib/capture.sh
. "$REPO/tests/lib/capture.sh"

# 取狀態碼 + 回應體 + 某個回應頭，一次請求全拿到。
probe() {
  local url=$1 host_hdr=${2:-}
  local args=(-s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 5)
  [ -z "$host_hdr" ] || args+=(-H "Host: $host_hdr")
  run_curl "${args[@]}" "$url"
}

expect_status() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望 $want，实际 $got"; fi
}

expect_body() {
  local what=$1 want=$2
  local got
  got=$(cat "$WORK/body")
  if [ "$got" = "$want" ]; then ok "$what 体 = $got"; else fail "$what 体期望「$want」，实际「$got」"; fi
}

expect_header() {
  local what=$1 name=$2 want=$3
  local got
  # 頭名大小寫不敏感；只取第一次出現，去掉行尾的 CR。
  got=$(grep -i "^$name:" "$WORK/hdr" | head -1 | cut -d' ' -f2- | tr -d '\r')
  if [ "$got" = "$want" ]; then ok "$what 的 $name = $got"; else fail "$what 的 $name 期望「$want」，实际「$got」"; fi
}

# ★ 頭名的**大小寫**逐字釘（G162）：上面的 `expect_header` 不分大小寫 ⇒ 判不出「使用者寫的 `X-Fulcrum` 以 `x-fulcrum` 上線」。
#   樞衡自建的回應只在要插的名都照原樣寫得出時才不建大小寫表；`X-Fulcrum` 既不在 pingora 的標題化表裡、
#   也不是小寫 ⇒ 這一條必須照舊建表、照使用者寫的發。判「什麼時候不建表」那個函式壞成恆不建時，紅在這裡。
expect_header_name_exact() {
  local what=$1 name=$2
  if grep -q "^$name: " "$WORK/hdr"; then ok "$what 的头名原样是 $name"; else
    fail "$what 的头名期望原样「$name」，实际「$(grep -i "^$name:" "$WORK/hdr" | head -1 | cut -d: -f1)」"; fi
}

BASE="http://$HOST:$PROXY_PORT"

# 1) 備援 handle + 站台級 header（G49：header 排在 handle 之前，所以兩者都生效）
expect_status "GET /" 200 "$(probe "$BASE/")"
expect_body "GET /" "root"
expect_header "GET /" "X-Fulcrum" "test"
expect_header_name_exact "GET /" "X-Fulcrum"

# 2) ★ 轉發：命名匹配器 + header_up 都要真的生效
expect_status "GET /api/x（转发）" 200 "$(probe "$BASE/api/x")"
expect_body "GET /api/x（转发）" "up path=/api/x xup=1"

# 3) ★ rewrite 要帶到上游去
expect_status "GET /rw/y（改写后转发）" 200 "$(probe "$BASE/rw/y")"
expect_body "GET /rw/y（改写后转发）" "up path=/rewritten xup="

# 4) 重定向
expect_status "GET /redir" 301 "$(probe "$BASE/redir")"
expect_header "GET /redir" "Location" "https://example.com/moved"

# 5) respond 帶狀態碼與體
expect_status "GET /err" 418 "$(probe "$BASE/err")"
expect_body "GET /err" "teapot"

# 6) ★ 自研的兩塊（`file_server` 批 F、`cache` 批 G）
#
# ★ 判據取**回應體**而不只是狀態碼：只看 200 的話，一個仍然在回落的實作照樣綠
#   （回落後端也回 200）。回應體是唯一能把「自研發的」與「別人發的」分開的東西。
expect_status "GET /static/x（file_server 自研 → 直接发文件）" 200 "$(probe "$BASE/static/x")"
#   ★ `expect_body` 是**精確**比對，所以它同時也是反向那一半：一個仍然在回落的
#   實作會得到 `fallback-nginx path=/static/x …`，當場不等。
expect_body "GET /static/x（file_server 自研 → 直接发文件）" "self-built-file-server"
# ★ 自研之後 Content-Type 由 G90 那張小表決定。`x` 沒有副檔名 ⇒ 預設值。
#   ⚠ 這一條是「表真的被查了」的唯一痕跡 —— 回落後端給的會是它自己的判斷。
expect_header "GET /static/x" "Content-Type" "application/octet-stream"
# ★ `cache` 現在是**中介軟體**（批 G），裹住 `/cached/*` 那條 `reverse_proxy`。
#   這裡只驗「配了 `cache` 不會把轉發弄壞」—— 判據取**回顯出來的路徑**，
#   而不只是狀態碼：它證明請求是原樣被帶到上游的。
#   ⚠ 快取語義本身（命中/回源/Vary/Authorization 不串號/防驚群）由 `tests/cache/run.sh` 管。
expect_status "GET /cached/x（cache 裹着 reverse_proxy）" 200 "$(probe "$BASE/cached/x")"
expect_body "GET /cached/x（cache 裹着 reverse_proxy）" "up path=/cached/x xup="
# ★ ★ 第二次必須**命中**，而命中這件事靠狀態碼看不出來（兩次都是 200）
#   ⇒ 看 `X-Fulcrum-Cache`。⚠ 少了這一條，一個「裝上快取卻從不命中」的實作
#   在本場景裡與正確實作完全一樣。
probe "$BASE/cached/x" > /dev/null
expect_header "GET /cached/x 第二次" "X-Fulcrum-Cache" "HIT"

# 7) ★ ★ 無站台匹配 → 421（G63）。`:9102` 上只有一個具名站台。
NAMED="http://$HOST:$NAMED_PORT"
expect_status "Host=only.example" 200 "$(probe "$NAMED/" only.example)"
expect_body "Host=only.example" "named-only"
expect_status "Host=nope（无站点匹配）" 421 "$(probe "$NAMED/" nope.example)"

# 8) keep-alive：一次連接上兩個請求
# ⚠ `-o` 是**按 URL** 生效的，不是全域的：兩個 URL 只給一個 `-o /dev/null`，
#   第二個回應體會漏到 stdout 裡，把 `-w` 的輸出攪成 `200,teapot418,`。
#   ★ 這不是 curl 的坑，是「以為選項是全域的」——每個 URL 配一個 `-o`。
KA=$(curl -s -o /dev/null -o /dev/null -w '%{http_code},' --max-time 5 "$BASE/" "$BASE/err" 2>/dev/null || echo "000,")
if [ "$KA" = "200,418," ]; then ok "同一连接上两个请求：$KA"; else fail "keep-alive 期望 200,418, 实际 $KA"; fi

# ── 9) ★ ★ TLS：按 SNI 動態挑證書（§5.1 第 1 條鎖死的那條路）────────────────
#
# ★ 動態挑證書走的是 BoringSSL 的 `set_select_certificate_callback`（G104），
#   兩個入口（h1/h2 與 h3）共用同一個回呼。
TLSURL="https://secure.example:$TLS_PORT/"
RESOLVE="secure.example:$TLS_PORT:$HOST"

# 9a) 證書鏈要**真的能驗過**——用 `--cacert` 而不是 `-k`。
#     ★ `-k` 只能證明「握手成功了」，證不了「伺服器端給的是我們那張證書」。
TLS_CODE=$(curl -s -o "$WORK/body" -w '%{http_code}' --max-time 5 \
  --cacert "$WORK/tls.crt" --resolve "$RESOLVE" "$TLSURL" 2>/dev/null || echo "000")
expect_status "HTTPS（--cacert 验签）" 200 "$TLS_CODE"
expect_body "HTTPS（--cacert 验签）" "secure-ok"

# 9b) ★ ALPN 要能協商到 h2。`enable_h2()` 少調一次，這裡會掉回 1.1。
H2V=$(run_curl -s -o /dev/null -w '%{http_version}' --max-time 5 --http2 \
  --cacert "$WORK/tls.crt" --resolve "$RESOLVE" "$TLSURL")
if [ "$H2V" = "2" ]; then ok "HTTPS 上 ALPN 协商到 HTTP/2"; else fail "HTTPS 期望 http_version=2，实际 $H2V"; fi

# 9c) ★ ★ 沒有證書的 SNI 必須**握手失敗**，而不是拿一張不匹配的證書去應答。
#     後者會讓用戶端看到證書錯誤，而維運在伺服器端只看到一次成功的握手。
#     ★ 判據掛在 **curl 的結束代碼**上，不只掛在 `%{http_code}` 上：後者連不上時是 `000`，
#     而 `000` 也可能來自超時、連接埠沒開、DNS 失敗——分不出「被拒絕握手」這一種。
curl_capture -s -o /dev/null -w '%{http_code}' --max-time 5 -k \
  --resolve "nocert.example:$TLS_PORT:$HOST" "https://nocert.example:$TLS_PORT/"
NOCERT=$(cat "$WORK/curlw")
if [ "$CURL_RC" -ne 0 ] && [ "$NOCERT" = "000" ]; then
  ok "未知 SNI 被拒绝握手（curl 退出码 $CURL_RC），没有拿别人的证书应答"
else
  fail "未知 SNI 期望握手失败，实际 curl 退出码 $CURL_RC、HTTP $NOCERT"
fi

# 9d) ★ ★ 不帶 SNI 的用戶端（老用戶端、或直接按 IP 訪問）→ 全域 `default_sni`。
#
#     ⚠ ⚠ 這條指令曾經是**「DSL 認得、編譯得過、執行環境零呼叫方」**的活樣本：
#     `SniResolver::set_default` 全倉唯一的呼叫方在一條 `#[cfg(test)]` 裡，
#     而它又**不在 `UNWIRED` 裡** ⇒ 裝載紀錄一個字都不說，配了它的人只會看到
#     不帶 SNI 的用戶端照樣被拒絕握手。
#     ★ 判據掛在**拿到的是哪一張證書**上，不掛在「握手成功了嗎」上：後者在伺服器端
#     隨便發一張證書時同樣成立，而那正是 9c 要防的那種「成功」。
#     ★ 與它成對的反向那半就是上面的 9c —— 本場景現在**配著** default_sni，
#     所以 9c 從此還多守一件事：這條指令沒有順手變成未知 SNI 的備援
#     （Caddy 把那件事分成另一個選項 `fallback_sni`，我們沒有它）。
NOSNI_SUBJ=$(echo | openssl s_client -connect "$HOST:$TLS_PORT" -noservername 2>/dev/null \
             | openssl x509 -noout -subject 2>/dev/null || true)
case "$NOSNI_SUBJ" in
  *secure.example*) ok "不带 SNI 的握手拿到 default_sni 那张证书（$NOSNI_SUBJ）" ;;
  *) fail "不带 SNI 期望拿到 secure.example 那张证书，实际：${NOSNI_SUBJ:-（一张都没拿到，多半是握手被拒）}" ;;
esac

# ── 10) ★ ★ ★ 一個約 4 KiB 的回應只許一個資料段：L7 寫緩衝（G155）──────────────
#
# ★ 起因（2026-09-24 HTTP 層診斷的發現 1）：pingora 給接入連接的寫緩衝預設只有 1460 位元組
#   ⇒ 頭 + 約 4 KiB 正文的回應被拆成**兩次 send、兩個 TCP 段**（頭先被衝出去、正文再寫穿），
#   而 nginx / haproxy 都是一次。修法在 `crates/fulcrum-server/src/lib.rs` 的 L7 監聽那段。
# ★ 讀法：用戶端讀**自己那條** socket 的 `TCP_INFO`，數它收到了幾個帶資料的段
#   （`tcpi_data_segs_in`）。環回上一次 send 就是一個段（MSS 遠大於這裡的正文）
#   ⇒ 讀數是確定的，不用壓測。
# ★ 同一條連接先發一次預熱，只量第二個回應：TLS 的握手與工作階段票據都落在預熱那一次裡。
# ★ ★ 內建反證：32 KiB 的正文比寫緩衝大 ⇒ 修好之後照樣「先衝頭、再寫穿」，兩個段。
#   ⇒ 它證明這把尺子在修好之後**仍然看得見兩個段** —— 一個恆報 1 的讀法會讓它紅。
#   ⚠ 寫緩衝哪天調到 32 KiB 以上，這一條會紅：那時把它的正文調到比緩衝大，⛔ 別刪。
cat > "$WORK/segs.py" <<'PY'
#!/usr/bin/env python3
"""只服务于 tests/serve/run.sh 的第 10 节：同一条 keep-alive 连接上发两次同一个请求，
打印「第二个响应让客户端收到了几个带数据的 TCP 段」与「它的正文字节数」。
⚠ 读不到、读不懂就当场非零退出——不猜、不吞。"""
import socket
import ssl
import struct
import sys

# `struct tcp_info`（include/uapi/linux/tcp.h）里要用的两格：
#   tcpi_bytes_received  u64 @ 128 —— 只用来自证偏移没读错（明文时必须等于实际读到的字节数）
#   tcpi_data_segs_in    u32 @ 152 —— 收到的带数据的段（Linux 4.6 起才有）
OFF_BYTES_RECEIVED = 128
OFF_DATA_SEGS_IN = 152


def tcp_info(sock):
    raw = sock.getsockopt(socket.IPPROTO_TCP, socket.TCP_INFO, 256)
    if len(raw) < OFF_DATA_SEGS_IN + 4:
        sys.exit(f"TCP_INFO 只有 {len(raw)} 字节，读不到 tcpi_data_segs_in")
    (received,) = struct.unpack_from("<Q", raw, OFF_BYTES_RECEIVED)
    (data_segs,) = struct.unpack_from("<I", raw, OFF_DATA_SEGS_IN)
    return received, data_segs


def read_response(sock):
    """读完一个带 Content-Length 的响应：返回（状态码，正文字节数，从 socket 读到的总字节数）。"""
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(65536)
        if not chunk:
            sys.exit("响应头还没读完，连接就断了")
        buf += chunk
    head, _, body = buf.partition(b"\r\n\r\n")
    lines = head.split(b"\r\n")
    status = int(lines[0].split()[1])
    length = None
    for line in lines[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
    if length is None:
        sys.exit("响应没有 Content-Length —— 这一节只量带 Content-Length 的响应")
    total = len(buf)
    while len(body) < length:
        chunk = sock.recv(65536)
        if not chunk:
            sys.exit(f"正文只读到 {len(body)}/{length} 字节，连接就断了")
        body += chunk
        total += len(chunk)
    if len(body) != length:
        sys.exit(f"读到的正文比 Content-Length 多：{len(body)} > {length}")
    return status, length, total


def main():
    host, port, path = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    sni = sys.argv[4] if len(sys.argv) > 4 else None
    sock = socket.create_connection((host, port), timeout=5)
    if sni:
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        # ⚠ wrap 之后原来那个 socket 对象就脱手了；TCP_INFO 从 SSLSocket 上取（同一个 fd）。
        sock = ctx.wrap_socket(sock, server_hostname=sni)
    request = f"GET {path} HTTP/1.1\r\nHost: {sni or host}\r\n\r\n".encode()

    # 预热：TLS 的握手与会话票据都落在这一次里，⛔ 不计。
    sock.sendall(request)
    status, _, _ = read_response(sock)
    if status != 200:
        sys.exit(f"预热那一次拿到 {status}，期望 200")

    before_bytes, before_segs = tcp_info(sock)
    sock.sendall(request)
    status, length, total = read_response(sock)
    after_bytes, after_segs = tcp_info(sock)
    if status != 200:
        sys.exit(f"第二次拿到 {status}，期望 200")
    # ★ 自证：明文时 bytes_received 的增量必须正好等于这次从 socket 读到的字节数
    #   ⇒ 128 那个偏移没读错、struct 布局对得上。⚠ 152（data_segs_in）没有单独自证，
    #   写错时主要靠 32 KiB 那条反证兜。TLS 时线上多了记录头与认证标签，对不上是正常的，不判。
    if not sni and after_bytes - before_bytes != total:
        sys.exit(
            f"TCP_INFO 自证失败：bytes_received 增量 {after_bytes - before_bytes}"
            f" ≠ 实际读到 {total}（struct tcp_info 的偏移读错了？）"
        )
    print(after_segs - before_segs, length)


try:
    main()
except (OSError, ssl.SSLError) as e:
    sys.exit(f"{type(e).__name__}: {e}")
PY

# 量一次、判一次。★ 先核打到的是不是那條路由：落到備援 `handle` 的話正文只有幾個位元組，
#   而小回應本來就是一個段 ⇒ 不核這一條，路由寫錯時這一節照樣全綠。
# ⚠ 量不出來（python3 非零退出）就算紅，⛔ 不跳過。
expect_segs() {
  local what=$1 want_len=$2 min=$3 max=$4
  shift 4
  local out segs len want_txt
  if ! out=$(python3 "$WORK/segs.py" "$@" 2>"$WORK/segs.err"); then
    fail "$what：量不出来 —— $(cat "$WORK/segs.err")"
    return 0
  fi
  read -r segs len <<<"$out"
  if [ "$len" != "$want_len" ]; then
    fail "$what：正文 $len 字节，期望 $want_len —— 没打到那条路由"
    return 0
  fi
  want_txt="恰好 $min 个"
  [ -n "$max" ] || want_txt="至少 $min 个"
  if [ "$segs" -ge "$min" ] && { [ -z "$max" ] || [ "$segs" -le "$max" ]; }; then
    ok "$what：$len 字节的正文 → $segs 个带数据的段"
  else
    fail "$what：$len 字节的正文 → $segs 个带数据的段，期望$want_txt"
  fi
}

expect_segs "明文 GET /wbuf/4k" 4096 1 1 "$HOST" "$PROXY_PORT" /wbuf/4k
expect_segs "HTTPS GET /wbuf/4k（TLS 在写缓冲之上）" 4096 1 1 "$HOST" "$TLS_PORT" /wbuf/4k secure.example
expect_segs "明文 GET /wbuf/32k（内置反证：正文比写缓冲大）" 32768 2 "" "$HOST" "$PROXY_PORT" /wbuf/32k

# ── 11) ★ ★ 錯誤頁裡的 `{header.X}` 取的是**原始**請求頭（HTTP 層診斷發現 2 · 請求頭不整份克隆）──────
#
# ★ 起因：錯誤頁那一路的請求頭改成「借 session 裡那一份」之後（`NoRouteMatch` 直接借，
#   `file_server` 出錯後重新借一次），一個「錯誤頁拿到空頭 / 拿錯了頭」的迴歸在別的門裡全綠 ——
#   在它之前沒有任何一道門在錯誤頁裡用 `{header.X}`。
# ★ ① 先證明這個站台的 `/static/*` 真的落到 `file_server` —— ② 才能說是「file_server 出錯之後」那一路。
ERRS="http://$HOST:$NAMED_PORT"
expect_status "errs.example GET /static/x（/static/* 落到 file_server）" 200 "$(probe "$ERRS/static/x" errs.example)"
expect_body "errs.example GET /static/x" "self-built-file-server"
probe_errs() {
  run_curl -s -o "$WORK/body" -w '%{http_code}' --max-time 5 \
    -H "Host: errs.example" -H "X-Probe: p1" "$ERRS$1"
}
expect_status "errs.example GET /static/missing（file_server 出错 → 错误页）" 418 "$(probe_errs /static/missing)"
expect_body "errs.example GET /static/missing" "404 /static/missing probe=p1"
expect_status "errs.example GET /nothing（NoRouteMatch → 错误页）" 418 "$(probe_errs /nothing)"
expect_body "errs.example GET /nothing" "404 /nothing probe=p1"

# ── 12) ★ ★ 續不續聽 pingora 的判定，樞衡只管時長（G161）──────────────────────────
#
# ★ 起因：`process_new_http` 讀完請求頭後曾無條件設 60 s keep-alive，蓋掉了 pingora 判出的「不續」：
#   ① HTTP/1.0 沒要求 keep-alive 也續（RFC 9112 §9.3：回完就關）；
#   ② TE 與 CL 同在的請求在讀完請求體的站台上也續（§6.1：回完必須關，防請求走私）。
# ★ 走 `/api/*`（反代，把請求體讀完才回）：`respond` 不讀請求體，會被 pingora
#   「請求體沒讀完就先回 ⇒ 關」那條另行關掉 ⇒ ② 在那裡看不出來（2026-10-08 實測）。
# ★ 兩個方向都判：該關的關、該續的續 —— 一個「什麼都關」的實作會讓前一半全綠。
cat > "$WORK/conn.py" <<'PY'
#!/usr/bin/env python3
"""只服务于 tests/serve/run.sh 的第 12 节：发一条原始请求、读完它的响应，
打印「响应的 Connection 头」与「回完之后对端关没关（closed / open）」。
⚠ 读不到、读不懂就当场非零退出——不猜、不吞。"""
import re
import socket
import sys

REQS = {
    "h10": b"GET /api/conn HTTP/1.0\r\nHost: a.example\r\n\r\n",
    "h10-ka": b"GET /api/conn HTTP/1.0\r\nHost: a.example\r\nConnection: keep-alive\r\n\r\n",
    "h11": b"GET /api/conn HTTP/1.1\r\nHost: a.example\r\n\r\n",
    "h11-close": b"GET /api/conn HTTP/1.1\r\nHost: a.example\r\nConnection: close\r\n\r\n",
    "te-cl": (
        b"POST /api/conn HTTP/1.1\r\nHost: a.example\r\n"
        b"Transfer-Encoding: chunked\r\nContent-Length: 5\r\n\r\n0\r\n\r\n"
    ),
}

host, port, name = sys.argv[1], int(sys.argv[2]), sys.argv[3]
s = socket.create_connection((host, port), timeout=5)
s.sendall(REQS[name])
buf = b""
while b"\r\n\r\n" not in buf:
    chunk = s.recv(65536)
    if not chunk:
        sys.exit(f"响应头没读完对端就关了：{buf[:200]!r}")
    buf += chunk
head, _, body = buf.partition(b"\r\n\r\n")
if not head.startswith(b"HTTP/1.1 200 "):
    sys.exit(f"不是 200：{head.splitlines()[0]!r}")
m = re.search(rb"(?im)^content-length:\s*(\d+)\s*$", head)
if not m:
    sys.exit("响应没有 Content-Length，判不出正文在哪里结束")
n = int(m.group(1))
while len(body) < n:
    chunk = s.recv(65536)
    if not chunk:
        sys.exit("正文没读完对端就关了")
    body += chunk
if len(body) > n:
    sys.exit(f"正文之后多出 {len(body) - n} 字节")
conn = re.search(rb"(?im)^connection:\s*([^\r\n]*)", head)
s.settimeout(1)
try:
    state = "closed" if s.recv(1) == b"" else "extra-bytes"
except socket.timeout:
    state = "open"
print(conn.group(1).decode().strip().lower() if conn else "-", state)
PY

# ⚠ 量不出來（python3 非零退出）就算紅，⛔ 不跳過。
expect_conn() {
  local what=$1 name=$2 want=$3 out
  if ! out=$(python3 "$WORK/conn.py" "$HOST" "$PROXY_PORT" "$name" 2>"$WORK/conn.err"); then
    fail "$what：量不出来 —— $(cat "$WORK/conn.err")"
    return 0
  fi
  if [ "$out" = "$want" ]; then
    ok "$what → Connection: ${out% *} · 回完后 ${out#* }"
  else
    fail "$what → 得到「$out」，期望「$want」"
  fi
}
expect_conn "HTTP/1.0 没要求 keep-alive ⇒ 回完就关" h10 "close closed"
expect_conn "HTTP/1.0 带 Connection: keep-alive ⇒ 照续" h10-ka "keep-alive open"
expect_conn "HTTP/1.1 不带 Connection ⇒ 照续" h11 "keep-alive open"
expect_conn "HTTP/1.1 Connection: close ⇒ 关" h11-close "close closed"
expect_conn "TE 与 CL 同在、反代读完请求体 ⇒ 关（RFC 9112 §6.1）" te-cl "close closed"

# ── 13) ★ ★ 有反代站台的連接埠上，h1 用戶端的頭名原樣到上游（G163）──────────────────────
#
# ★ 起因：讀 h1 請求頭時保留頭名原樣大小寫的那張表，G163 起**按連接埠**建 —— 連接埠上沒有反代站台就不建
#   （每條請求省兩次分配）。讀請求頭之前還不知道 Host，只能按連接埠定。
#   ⇒ 本連接埠有反代（`/api/*` 等）⇒ 必須照舊建表 ⇒ 轉發給上游的頭名一個字都不變。
#   判定壞成「一律不建」（或連接埠判反了）時紅在這裡：上游收到的會是 `x-mixed-case` 一類小寫。
# ⚠ 反方向（沒有反代的連接埠不建表）線上上**看不出任何差別**，只少兩次分配 ——
#   那一半由 `fulcrum-runtime` 的 `proxy_ports` 單測、`keepalive` 的單測與開發機診斷臺的計數判。
cat > "$WORK/echo_case.py" <<'PY'
#!/usr/bin/env python3
"""只服务于 tests/serve/run.sh 的第 13 节：把收到的请求头名原样（逗号分隔）回成正文。"""
import signal
import socket
import sys
import threading


def handle(c):
    try:
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = c.recv(65536)
            if not chunk:
                return  # `wait_port` 的探测连接：收到 EOF 就走。
            data += chunk
        lines = data.split(b"\r\n\r\n", 1)[0].split(b"\r\n")[1:]
        names = b",".join(ln.split(b":", 1)[0] for ln in lines)
        c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s" % (len(names), names))
    except OSError:
        pass
    finally:
        c.close()


srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(16)
# 收尾先发 SIGINT、5 秒后 SIGKILL。⚠ 非交互 bash 里 `&` 起的进程 SIGINT 是被忽略的，python 那时不装
#   KeyboardInterrupt ⇒ 显式接住 SIGINT（SIGTERM 也接），干净退出、当场还端口。
#   `settimeout` 让主线程回得到 Python、信号才处理得了。
signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
srv.settimeout(0.5)
try:
    while True:
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            continue
        conn.settimeout(5)
        threading.Thread(target=handle, args=(conn,), daemon=True).start()
except (KeyboardInterrupt, SystemExit):
    pass
finally:
    srv.close()
PY
python3 "$WORK/echo_case.py" "$ECHO_PORT" > "$WORK/echo_case.log" 2>&1 &
PIDS+=($!)
if wait_port "$ECHO_PORT"; then
  ECHO_CODE=$(run_curl -s -o "$WORK/body" -w '%{http_code}' --max-time 5 \
    -H "X-Mixed-Case: 1" -H "x-lower-case: 2" -H "X-ALL-UPPER: 3" "$BASE/echo-case/x")
  expect_status "GET /echo-case/x（反代到回头名的上游）" 200 "$ECHO_CODE"
  ECHO_NAMES=",$(cat "$WORK/body"),"
  for n in X-Mixed-Case x-lower-case X-ALL-UPPER; do
    case "$ECHO_NAMES" in
      *",$n,"*) ok "上游收到的头名原样是 $n" ;;
      *) fail "上游收到的头名里没有原样的「$n」：$(cat "$WORK/body")" ;;
    esac
  done
else
  fail "回头名的上游（$ECHO_PORT）没起来：$(cat "$WORK/echo_case.log")"
fi

# ── ★ ★ ★ 網域上游（批 10）──────────────────────────────────────────────────
#
# ⚠ ⚠ **這兩條補的是一個具體的夾具缺口**：在它們之前，本場景裡**所有上游
#   都寫的是 `127.0.0.1:端口`** —— 而 IP 字面值**不走 DNS**。
#   於是「網域上游」這條路從來沒有被任何一道門餵過，而它上面躺著一個真缺陷：
#   `HttpPeer::new` 在**每個請求**上做一次阻塞 `getaddrinfo`，失敗還 panic
#   （實測：每請求一次 panic，用戶端只看到連接被丟棄，不是乾淨的 502）。
#
# ★ 判據兩個方向都要：**解析得出來的照常轉發**，**解析不出來的回乾淨的 502**。
expect_status "GET /byname/x（域名上游，能解析）" 200 "$(probe "$BASE/byname/x")"
expect_body "GET /byname/x（域名上游，能解析）" "up path=/byname/x xup="

# ★ ★ 解析不出來的那個：**502，而不是連接被丟棄**。
#   ⚠ 判據必須同時看狀態碼與 curl 的結束代碼 —— 改之前這裡拿到的是
#   `000`（連接斷了）而不是 502，而只斷言「不是 200」兩者都滿足。
curl_capture -s -o /dev/null -w '%{http_code}' --max-time 10 "$BASE/deadname/x"
DEAD_CODE=$(cat "$WORK/curlw")
if [ "$CURL_RC" -eq 0 ] && [ "$DEAD_CODE" = "502" ]; then
  ok "★ 解析不出来的域名上游 → 干净的 502（curl 退出码 0）"
else
  fail "解析不出来的域名上游期望 502 且 curl 成功，实际 curl 退出码 $CURL_RC、HTTP $DEAD_CODE"
fi

# ★ ★ ★ 而且**處理程序沒有 panic**。這一條是本批的頭條判據：
#   改之前那裡是每請求一次 `called Result::unwrap() on an Err value`。
if grep -qiE "panicked at|Result::unwrap" "$WORK/proxy.log"; then
  fail "日志里出现了 panic —— 请求路径上还在做 DNS"
  grep -iE "panicked at|Result::unwrap" "$WORK/proxy.log" | head -3 >&2
else
  ok "★ 请求路径上没有任何 panic（改之前这里是每请求一次）"
fi

# ★ 裝載時那條 error 必須說出來「哪個上游解析不出來」——
#   一個靜靜跳過的實作會讓維運對著 502 查半天。
if grep -q "no-such-host-zzz.invalid:80" "$WORK/proxy.log"; then
  ok "装载日志点名了解析不出来的那个上游"
else
  fail "装载日志没说哪个上游解析不出来"
fi

# ── ★ ★ ★ 後台重解析：網域在處理程序活著的時候換了 IP（批 10）────────────────────
#
# ★ 三步，兩個方向都要走：**跟得上變壞** → **也跟得上變好**。
#   ⚠ 只驗前者，一個「解析失敗就把上游永久摘掉」的實作照樣全綠；
#     只驗後者，一個「壓根沒重解析、一直連著啟動那一份」的實作在
#     127.0.0.1 → 127.0.0.1 上也照樣全綠。

# ★ 先驗一件更基本的事：**那個後台任務真的起來了，而且用的是設定裡那個間隔**。
#   ⚠ 少了這條，下面兩步一旦超時，分不出「沒重解析」還是「間隔取錯成 30s」。
if grep -qF "上游 DNS 定期重解析已启动，每 5s 一次" "$WORK/proxy.log"; then
  ok "后台重解析任务起来了，间隔取自配置里的 dns_refresh 5s"
else
  fail "日志里没有后台重解析的启动行（或者间隔不是 5s）"
  grep -i '重解析' "$WORK/proxy.log" >&2 || true
fi

# 輪詢直到狀態碼等於期望值，或超時。回顯最後一次拿到的碼。
POLL_TIMEOUT=30
poll_status() {
  local url=$1 want=$2 tries=0 got=
  while [ "$tries" -lt "$POLL_TIMEOUT" ]; do
    got=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$url")
    if [ "$got" = "$want" ]; then break; fi
    sleep 1
    tries=$((tries + 1))
  done
  echo "$got"
}

# 1) 啟動時它指向真上游 —— 照常轉發。
expect_status "GET /rotate/x（会变的域名，起初指向真上游）" 200 "$(probe "$BASE/rotate/x")"

# 2) 把它指到一個沒人在聽的位址，等後台那一輪把新答案取回來。
#    ⚠ 127.0.0.9 在 Linux 上屬於本機的 lo，連它會**立刻**被拒 —— 乾淨、快、可判。
set_rotate_ip 127.0.0.9 || fail "改不了 /etc/hosts"
GOT=$(poll_status "$BASE/rotate/x" 502)
if [ "$GOT" = "502" ]; then
  ok "★ 后台重解析跟上了「域名换到一个连不上的 IP」→ 干净的 502"
else
  fail "域名换 IP 之后期望 502，$POLL_TIMEOUT 秒内一直是 $GOT —— 后台重解析没跑？"
fi

# ★ 而且要說出來「它變了、變成了什麼」。
#   ⚠ 只看狀態碼分不出「重解析了」與「上游恰好掛了」——
#     紀錄裡那一行是**誰做的決定**的唯一憑證。
if grep -F '的地址变了' "$WORK/proxy.log" | grep -qF '127.0.0.9'; then
  ok "★ 日志记下了新解析出来的地址（谁做的决定看得见）"
else
  fail "日志里没有「地址变了 → 127.0.0.9」那一行 —— 重解析没留痕"
  grep -i '重解析\|地址变了' "$WORK/proxy.log" >&2 || true
fi

# 3) 再指回來 —— **它得自己恢復**。
#    ★ ★ 這一步才是 `dns_refresh` 存在的全部理由：nginx OSS 那個事故就是
#      「網域換了 IP，而處理程序一直連著啟動時解析到的那一份」。
set_rotate_ip 127.0.0.1 || fail "改不了 /etc/hosts"
GOT=$(poll_status "$BASE/rotate/x" 200)
if [ "$GOT" = "200" ]; then
  ok "★ ★ 域名指回来之后它自己恢复了（dns_refresh 的全部理由）"
else
  fail "域名指回真上游之后期望 200，$POLL_TIMEOUT 秒内一直是 $GOT"
fi

# ★ ★ 失敗紀錄**不許每輪刷一行**。
#   `no-such-host-zzz.invalid` 從頭到尾都解析不出來，而上面這幾步跑了十幾秒
#   ＝ 後台任務轉了好幾輪。一個「每輪都 warn」的實作在這裡會留下好幾行。
#   ⚠ 判據用 `-le 1` 不用 `-eq 1`：本節耗時短時可能一輪都還沒轉到，
#     那時 0 行是對的；而**刷屏的實作給不出 ≤1**。
SPAM=$(grep -cF '开始解析不出来' "$WORK/proxy.log" || true)
if [ "$SPAM" -le 1 ]; then
  ok "★ 一个永久坏掉的上游只报一次，不是每轮刷一行（实际 $SPAM 行）"
else
  fail "后台重解析把同一个失败刷了 $SPAM 行 —— 日志会被淹掉"
fi


# ── ★ ★ ★ 主動健康檢查（批 11）──────────────────────────────────────────────
echo "=== [2c/4] 主动健康检查（health_uri）==="

# ★ 先釘住「後台任務真的起來了」。少了它，下面每一條超時都分不出
#   「判定沒生效」還是「任務壓根沒起」。
if grep -qF "主动健康检查已启动" "$WORK/proxy.log"; then
  ok "主动健康检查任务起来了"
else
  fail "日志里没有主动健康检查的启动行"
  grep -i '健康' "$WORK/proxy.log" >&2 || true
fi

# 連打 n 次，回顯命中 want 的次數。
burst_status() {
  local url=$1 want=$2 n=$3 hit=0 i=0 got=
  while [ "$i" -lt "$n" ]; do
    got=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$url")
    if [ "$got" = "$want" ]; then hit=$((hit + 1)); fi
    i=$((i + 1))
  done
  echo "$hit"
}

# 輪詢直到「連打 n 次全中」，或超時。回顯最後一輪的命中數。
poll_burst() {
  local url=$1 want=$2 n=$3 tries=0 hit=0
  while [ "$tries" -lt "$POLL_TIMEOUT" ]; do
    hit=$(burst_status "$url" "$want" "$n")
    if [ "$hit" = "$n" ]; then break; fi
    sleep 1
    tries=$((tries + 1))
  done
  echo "$hit"
}

# ── ★ ★ ★ 本節的錨：那臺「病號」的服務能力**完全正常** ─────────────────────
#
# ⚠ 少了這一條，下面那個 502 說明不了任何事情 —— 它可能只是「連不上」。
#   有了它，502 唯一可能的來源就是**探測路徑的判定**。
expect_status "直连病号上游（业务路径）" 200 "$(probe "http://$HOST:$SICK_PORT/whatever")"
expect_body "直连病号上游（业务路径）" "sick but serving path=/whatever"
expect_status "直连病号上游的探测路径" 500 "$(probe "http://$HOST:$SICK_PORT/health")"

# 1) 探測路徑回 500 ⇒ 它被摘掉，那條路由只剩 502。
HIT=$(poll_burst "$BASE/sick/x" 502 5)
if [ "$HIT" = "5" ]; then
  ok "★ ★ 探测路径回 500 的上游被摘掉了（而它的业务路径明明是 200）"
else
  fail "病号上游期望被摘掉（连 5 次 502），$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/5"
fi

# 2) ★ ★ ★ 同一個處理程序、同一條 health_uri，只把 `health_status` 寫成 5xx
#    ⇒ 判定**相反**。這一條證明的是「狀態碼模式真的被讀了」，
#    而不是「凡是 500 就摘」這種寫死的實作。
HIT=$(poll_burst "$BASE/sickok/x" 200 5)
if [ "$HIT" = "5" ]; then
  ok "★ ★ ★ 同一个上游、health_status 写 5xx ⇒ 判它健康（状态码模式真的被读了）"
else
  fail "health_status 5xx 那条期望一直 200，$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/5"
fi

# 3) 紀錄要說清楚**誰**被摘了、**為什麼**。
#    ⚠ 只看狀態碼的話，維運面對 502 分不出「上游掛了」「被健康檢查摘了」
#      「DNS 沒解析出來」——這三種的處置完全不同。
if grep -F '被健康检查摘掉' "$WORK/proxy.log" | grep -qF "$SICK_PORT"; then
  ok "日志点名了被摘掉的那个上游与原因"
else
  fail "日志里没有「被健康检查摘掉 … $SICK_PORT」那一行"
  grep -i '健康检查' "$WORK/proxy.log" | head -5 >&2 || true
fi

# 4) 兩個上游、其中一個**根本沒人在聽** ⇒ 探測把它摘掉之後，輪詢必須一直落在活的那個上。
#    ⚠ 沒有健康檢查時這裡是「每兩個請求壞一個」——而那正是 round_robin 的固有行為，
#      不是缺陷；本批要的就是讓它不再發生。
HIT=$(poll_burst "$BASE/hc/x" 200 10)
if [ "$HIT" = "10" ]; then
  ok "★ 挂掉的那个上游被摘掉之后，连打 10 次全是 200"
else
  fail "期望连打 10 次全 200，$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/10"
fi

# 5) ★ ★ 恢復那一半：把遲到的上游起起來，它必須**自己回到輪詢裡**。
#    ⚠ 只驗「摘得掉」的話，一個單向的實作照樣全綠 —— 而那意味著
#      上游修好之後**永遠回不來**，維運只能重新啟動樞衡。
echo "  · 现在把迟到的那个上游起起来"
start late "$WORK/late.Fulcrumfile"
wait_port "$LATE_PORT" || fail "迟到的上游起不来"

seen_late() {
  local tries=0 i=0
  local body=''
  while [ "$tries" -lt "$POLL_TIMEOUT" ]; do
    i=0
    while [ "$i" -lt 8 ]; do
      body=$(run_curl -s --max-time 5 "$BASE/hc/x")
      case "$body" in
        *"late path="*)
          echo yes
          return
          ;;
      esac
      i=$((i + 1))
    done
    sleep 1
    tries=$((tries + 1))
  done
  echo no
}
if [ "$(seen_late)" = "yes" ]; then
  ok "★ ★ 上游修好之后它自己回到了轮询里（恢复方向也通）"
else
  fail "迟到的上游起来了 $POLL_TIMEOUT 秒，仍然没有一个请求落到它身上"
fi
# ★ 而且恢復也要留痕。
if grep -qF '健康检查恢复了' "$WORK/proxy.log"; then
  ok "日志记下了恢复"
else
  fail "日志里没有「健康检查恢复了」"
fi

# 6) ★ 沒配 `health_uri` 的路由**完全不受影響**（`/api/` 上有 health_uri，
#    但 `/rw/` 那條沒有）—— 一個「對所有上游都探一遍」的實作會去打
#    一個使用者從沒說過的路徑，而那在很多後端上是 404 ⇒ 全部被判死。
expect_status "没配 health_uri 的路由照常转发" 200 "$(probe "$BASE/rw/anything")"

# ── [2d/4] ★ ★ ★ 被動熔斷（passive_fail，G136）────────────────────────────
#
# ⚠ ⚠ **這一節是 `passive_fail` 在真實請求路徑上唯一的判據。**
#   單測只證明了狀態機與篩子對，⛔ 證明不了「服務層真的會去落賬」——
#   而落賬那三處全在 `proxy_with` 裡，是一條**只有真流量才走得到**的路。
#   ★ 那正是本儲存庫反覆抓到的形狀：一條從沒被執行過的路徑不會自己回報錯誤。
#
# ★ ★ 判據是**兩條路的差值**，⛔ 不是任何一條的絕對值：
#   `/pf` 與 `/nopf` 指著**同一對上游**（壞的在前、好的在後），只差一行 `passive_fail`。
#   兩條各有自己的 `ProxyTarget` ⇒ 各有自己的熔斷狀態，於是同一個讀數
#   （「10 個請求裡幾個 500」）在兩條路上必須給出不同的答案。
echo "=== [2d/4] 被动熔断（passive_fail）==="

# 1) 對照組：沒配 `passive_fail` ⇒ 輪詢照常把一半請求送給壞上游。
#    ⚠ 這一條是**反證**：它要是也只中 2 個，說明「差值」根本不來自被動熔斷，
#      而來自別的什麼（比如壞上游其實沒在回 500）。
NOPF_500=$(burst_status "$BASE/nopf/x" 500 10)
if [ "$NOPF_500" = "5" ]; then
  ok "没配 passive_fail ⇒ 10 个请求里 5 个落到坏上游（轮询原样）"
else
  fail "对照组期望 5 个 500，实得 $NOPF_500 —— 坏上游没在回 500？还是轮询不是交替？"
fi

# 2) ★ ★ ★ 承重：配了 `passive_fail 2` ⇒ **只賠掉閾值那麼多個請求**，之後全綠。
#    ★ 這個數字本身就是這個特性的全部價值：樞衡**不換上游重試**，
#      所以每一個打到壞上游的請求都是使用者可見的錯。
#      「2」說明熔斷在第 2 次失敗之後真的把它摘出了排程。
PF_500=$(burst_status "$BASE/pf/x" 500 10)
if [ "$PF_500" = "2" ]; then
  ok "★ ★ ★ 配了 passive_fail 2 ⇒ 10 个请求只赔掉 2 个，坏上游被摘出调度"
else
  fail "期望恰好 2 个 500（阈值），实得 $PF_500"
  grep -iF 'passive_fail' "$WORK/proxy.log" | tail -5 >&2 || true
fi

# 3) ★ 兩條路必須**真的不同** —— 把差值本身寫成一條斷言。
#    ⚠ 少了它，一個「兩邊都只中 2 個」的巧合（比如壞上游中途死了）會讓上面兩條
#      各自看起來都對，而它們其實在描述同一件與熔斷無關的事。
if [ "$NOPF_500" != "$PF_500" ]; then
  ok "★ 同一对上游，只差一行 passive_fail ⇒ 读数不同（$NOPF_500 vs $PF_500）"
else
  fail "两条路读数相同（都是 $NOPF_500）—— 那一行 passive_fail 没起任何作用"
fi

# 4) 紀錄要說清楚**誰**被熔斷了。
#    ⚠ 與健康檢查那條同一理由：只看狀態碼的話，維運分不出「上游掛了」
#      「被健康檢查摘了」「被被動熔斷摘了」——三種的處置完全不同。
if grep -F '被动熔断' "$WORK/proxy.log" | grep -qF "$BAD_PORT"; then
  ok "日志点名了被熔断的那个上游"
else
  fail "日志里没有「被动熔断 … $BAD_PORT」那一行"
  grep -iF 'passive' "$WORK/proxy.log" | head -5 >&2 || true
fi

# 5) ⚠ 反方向：**沒配 passive_fail 的那條路一個字都不許變**。
#    ★ 它守的是 G136 那條「不寫就是完全關閉」——一個「順手對所有 reverse_proxy
#      都開一遍」的實作會讓這條也只中 2 個，而上面第 1 條已經會紅；
#      這裡再單獨確認它此刻**仍然**把請求送給壞上游（熔斷沒有外溢到別的目標）。
NOPF_AGAIN=$(burst_status "$BASE/nopf/x" 500 4)
if [ "$NOPF_AGAIN" = "2" ]; then
  ok "★ 熔断不外溢：没配的那条路仍然照常轮询（4 个里 2 个 500）"
else
  fail "没配 passive_fail 的那条路读数变了（4 个里 $NOPF_AGAIN 个 500）—— 熔断外溢到别的 ProxyTarget 了？"
fi

# ── [3/4] 管理面：全量原子 load（G8）與訪問控制（G14）──────────────────────
#
# ⚠ ⚠ **本節必須排在上面所有資料面斷言之後**：它會真的把設定換掉。
echo "=== [3/4] 管理面（Unix socket，G14）==="

# ★ ★ 權限就是這個管理面的**全部**訪問控制（G14：交給檔案系統 ACL）。
#   一個 0666 的 socket 等於「同機任意處理程序可改設定」—— 正是 G14 要堵的那個短處。
if [ -S "$ADMIN_SOCK" ]; then
  ok "管理 socket 建起来了：$ADMIN_SOCK"
  SOCK_MODE=$(stat -c '%a' "$ADMIN_SOCK")
  if [ "$SOCK_MODE" = "600" ]; then
    ok "socket 权限 600（G14：这是管理面的全部访问控制）"
  else
    fail "socket 权限是 $SOCK_MODE，期望 600 —— 同机任意进程都能改配置了"
  fi
else
  fail "管理 socket 没建起来：$ADMIN_SOCK"
fi

# ★ 反向那一半：**沒配 `admin` 的那個執行個體不該有任何 socket**。
#   ⚠ 少了這一條，一個「不管配沒配都開一個」的實作照樣全綠 ——
#   而 G14 的第一句話是「管理面預設不出機器」。
if [ -e "$WORK/upstream.sock" ] && [ -S "$WORK/upstream.sock" ]; then
  fail "上游实例没配 admin，却出现了一个 socket"
else
  ok "没配 admin 的实例没有建任何 socket（G14：默认不出机器）"
fi

admin_post() {
  # $1 = 路徑，$2 = body。回 "狀態碼<TAB>正文"
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# 不認識的路徑 → 404。⚠ 一個「什麼都收下」的管理面會讓打錯的命令看起來成功了。
CODE=$(admin_post /nope '{}')
expect_status "管理面：不认识的路径" 404 "$CODE"

# 壞 JSON → 400，**且舊設定一個位元組都沒動**。
CODE=$(admin_post "/load?overrides=clear" '{ not json')
expect_status "管理面：坏载荷" 400 "$CODE"
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$PROXY_PORT/")
expect_status "坏载荷之后旧配置还在服务" 200 "$CODE"

# ── ★ ★ ★ 增量通道：POST /runtime（M2 批 N 任務 4，裁決 R10；修復輪 1，評審 I1）──
#
# ⚠ ⚠ ⚠ 少了這一段，`/runtime` 這條路由**在生產上可以整個不存在**而沒有任何
#   門會紅：全部 21 條 admin.rs 判據都是直接調 `self.runtime(&body)`，一條都沒有
#   經過 `process_new_http` 裡的路由散佈（那個 `match (method, path)`）。
#   把路由表裡 `("POST","/runtime") => self.runtime(&body)` 那一行刪掉，
#   21 條判據照樣全綠，而這裡回 404——`admin.rs` 自己的註解寫著「一個判據漏掉
#   一個入口，等於在那個入口上沒有判據」。
#   ★ ★ 判據掛在**資料面**上，不是掛在「`/runtime` 回了 200」上：
#   `disable` 之後要走真流量確認那臺上游**真的不再收流量**，`enable` 之後
#   要走真流量確認它**真的收回來了**。
echo "  · 增量通道：POST /runtime（disable 一个上游 ⇒ 数据面真的不再收流量）"

# 基線：/api 現在應該是健康的（它指著 UP_PORT，沒配 health_uri，沒被摘過）。
expect_status "/runtime 基线：/api 现在是健康的" 200 "$(probe "$BASE/api/x")"

# ⚠ `site` 取的是這個站台塊在 DSL 裡的**原文**（`SiteRt::name`），不是拼出來的——
#   本場景的主站台塊頭就是裸連接埠 `:PROXY_PORT`（沒有 host、沒有 scheme），
#   替換之後就是 `:$PROXY_PORT`。上游位址是 IP 字面值，不需要歸一化，
#   與 `reverse_proxy` 那一行逐字相同。
RUNTIME_SITE=":$PROXY_PORT"
RUNTIME_UP="127.0.0.1:$UP_PORT"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "POST /runtime disable" 200 "$CODE"

# ★ ★ ★ 真正的判據：資料面上它必須真的不再收流量（不是「回了 200」）。
#   `/rw`、`/cached` 與 `/api` 共享同一個覆蓋格子（三條都沒寫 `id`，鍵相同）——
#   這裡只驗 `/api`，上面那條 200 斷言已經確認過它原本是健康的。
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/api/x")
expect_status "★ ★ disable 之后 /api 在数据面上真的不再收流量" 502 "$CODE"

# ★ 反向那一半：`enable` 把它撤回來，資料面也要真的恢復——只驗「摘得掉」
#   而不驗「回得來」，一個單向的實作會讓維運在真正需要撤銷的時候束手無策。
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"enable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "POST /runtime enable" 200 "$CODE"
expect_status "★ enable 之后 /api 在数据面上恢复了" 200 "$(probe "$BASE/api/x")"
expect_body "enable 之后 /api" "up path=/api/x xup=1"

# ★ ★ 連接埠集變了 → 409，**且舊設定還在服務**。
#   這一條是「原子」的判據：一個「先換後檢核」的實作在「換得動」那條上表現完全相同。
printf '%s\n' ":$((PROXY_PORT + 40)) {" "    respond 200 \"moved-port\"" "}" \
  > "$WORK/otherport.Fulcrumfile"
"$BIN" compile "$WORK/otherport.Fulcrumfile" > "$WORK/otherport.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile 生成不出结构化配置" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/otherport.json")")
expect_status "管理面：端口集变了" 409 "$CODE"
if grep -q 'systemctl reload' "$WORK/admin.out"; then
  ok "409 的正文说清了该怎么办（走 systemctl reload）"
else
  fail "409 没说该怎么办：$(cat "$WORK/admin.out")"
fi
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$PROXY_PORT/")
expect_status "被拒之后旧配置还在服务（原子）" 200 "$CODE"

# ★ ★ ★ 真的換一份：**判據掛在資料面上，不是掛在「回傳了 200」上**。
#   ⚠ 只斷言 200 的話，一個「收下但什麼都不做」的實作照樣全綠。
printf '%s\n' "{" "    admin unix/$ADMIN_SOCK" "}" "" ":$PROXY_PORT {" \
  "    respond 200 \"after-load\"" "}" "" "http://only.example:$NAMED_PORT {" \
  "    respond 200 named-only" "}" "" "secure.example:$TLS_PORT {" \
  "    tls $WORK/tls.crt $WORK/tls.key" "    respond 200 secure-ok" "}" \
  > "$WORK/next.Fulcrumfile"
"$BIN" compile "$WORK/next.Fulcrumfile" > "$WORK/next.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile 生成不出新配置" >&2
  cat "$WORK/next.Fulcrumfile" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/next.json")")
expect_status "管理面：全量 load" 200 "$CODE"
BODY=$(run_curl -s --max-time 5 "http://$HOST:$PROXY_PORT/")
if [ "$BODY" = "after-load" ]; then
  ok "★ 换配置**真的落到数据面上了**（响应体 = after-load）"
else
  fail "load 返回 200，但数据面还是老的：拿到「$BODY」"
fi
# ★ 同一次 load 裡沒動的那個站台必須**照舊能用** —— 換的是整份，不是隻換命中的那個。
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
  --resolve "only.example:$NAMED_PORT:$HOST" "http://only.example:$NAMED_PORT/")
expect_status "同一份里没改动的站点照旧" 200 "$CODE"

# ── [3.5/4] `overrides` 必填 + R11 覆蓋計數（M2 批 N 任務 5，G120 / R9 / R11）──
echo "=== [3.5/4] POST /load?overrides= 与每次响应的覆盖计数 ==="

# 上一節末尾把 :$PROXY_PORT 換成了裸 respond（after-load）——這裡重新換出一份
# 帶 reverse_proxy 的，好讓 overrides 的 keep/clear 有個真上游可以 disable。
# ⚠ 監聽連接埠集必須與當前完全相同（:$PROXY_PORT 裸 HTTP、only.example:$NAMED_PORT、
#   secure.example:$TLS_PORT 走 tls），否則會撞 409，而那不是本節要測的東西。
#   ★ ★ ★ 修復輪 1，評審 F1：`:$PROXY_PORT` 下面掛**兩個**互不相同的覆蓋鍵
#   （沒寫 `id` 的備援 handle + 寫了 `id up2` 的 `/second/*`，兩者共享同一臺
#   真機 `$UP_PORT`，只是 `id` 不同）——R11 那一節要擺「兩項生效中、其中一項
#   懸空」（N≠M），否則 N 與 M 永遠是同一個數，`{n}`/`{m}` 寫反也測不出來。
cat > "$WORK/ov.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    handle /second/* {
        reverse_proxy 127.0.0.1:$UP_PORT {
            id up2
        }
    }
    handle {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/ov.Fulcrumfile" > "$WORK/ov.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile ov.Fulcrumfile 失败" >&2
  exit 1
}
# 同樣的骨架，但備援 handle 換成裸 respond——沒寫 id 的那把鍵沒了上游，拿來驗
# 懸空（判據 5）；`/second/*`（id up2）原樣留著，R11 那一節要靠它撐住「另一項
# 仍然生效中、但不懸空」。
cat > "$WORK/ov-dangling.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    handle /second/* {
        reverse_proxy 127.0.0.1:$UP_PORT {
            id up2
        }
    }
    handle {
        respond 200 "ov-dangling"
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/ov-dangling.Fulcrumfile" > "$WORK/ov-dangling.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile ov-dangling.Fulcrumfile 失败" >&2
  exit 1
}

# 重新建立帶上游的那份基線——`overrides` 必填，即使這次只是「重建夾具」也不例外。
# ⚠ `RUNTIME_SITE` / `RUNTIME_UP` 是上面 [3/4] 那一節留下的（`:$PROXY_PORT` /
#   `127.0.0.1:$UP_PORT`），這份 ov.Fulcrumfile 用的是同一個站台、同一個上游。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "重建 overrides 夹具（首次也要带 overrides）" 200 "$CODE"
expect_status "重建之后 / 走真上游" 200 "$(probe "$BASE/")"

# ── 判據 1：缺 overrides ⇒ 400，且舊設定一個位元組沒動 ─────────────────────
#   ⚠ ⚠ 判據寫法紀律：body 是完全合法、會真的生效的 ov.json——唯一的毛病
#   是沒帶 `?overrides=`，否則 400 可能來自別的原因，判據就 confound 了。
CODE=$(admin_post /load "$(cat "$WORK/ov.json")")
expect_status "缺 overrides ⇒ 400" 400 "$CODE"
if grep -q 'overrides' "$WORK/admin.out"; then
  ok "400 的正文点了 overrides 的名"
else
  fail "400 没说清是 overrides 缺了：$(cat "$WORK/admin.out")"
fi
expect_status "缺 overrides 被拒之后旧配置还在服务" 200 "$(probe "$BASE/")"

# ── 判據 2：overrides 寫了別的值 ⇒ 400，同上；夾具「除了這一處，其餘完全合法」──
CODE=$(admin_post "/load?overrides=bogus" "$(cat "$WORK/ov.json")")
expect_status "overrides 写了不认识的值 ⇒ 400" 400 "$CODE"
expect_status "写错值被拒之后旧配置还在服务" 200 "$(probe "$BASE/")"

# ── 修復輪 1，評審 F3（提級 Minor）：overrides= 重複且衝突 ⇒ 400，不是靜默取
#   第一個 ────────────────────────────────────────────────────────────────
#   G120 的立身之本是「兩種現實互相沖突 ⇒ 不猜」，同一個請求裡寫了兩個 overrides=
#   正是這個形狀的教科書執行個體。⚠ ⚠ 判據寫法紀律：body 依舊是完全合法的 ov.json，
#   唯一的毛病是查詢串裡 `overrides` 出現了兩次。
CODE=$(admin_post "/load?overrides=keep&overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "overrides 重复且冲突 ⇒ 400" 400 "$CODE"
if grep -q '重复' "$WORK/admin.out"; then
  ok "400 的正文说清是「重复」而不是「值不认识」"
else
  fail "400 没说清是「重复」：$(cat "$WORK/admin.out")"
fi
expect_status "重复被拒之后旧配置还在服务" 200 "$(probe "$BASE/")"

# ── 判據 3：overrides=keep ⇒ 覆蓋還在，且仍作用在資料面上 ───────────────────
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "先摘掉这个上游（走 /runtime）" 200 "$CODE"
expect_status "摘掉之后数据面确实不通了" 502 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"

CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov.json")")
expect_status "overrides=keep 的 load" 200 "$CODE"
expect_status "★ ★ keep 之后覆盖还在数据面上生效（继续 502）" 502 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"

# ── 判據 5：懸空——keep 之後鍵落不到任何上游的覆蓋仍在、標了 dangling、回話點名 ──
#   ⚠ 判據寫法紀律：斷言的位址（$RUNTIME_UP）是這份夾具自己的真實上游位址，
#   不會出現在任何錯誤範本的固定字面值裡。
CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov-dangling.json")")
expect_status "keep：换成不带这个上游的配置" 200 "$CODE"
if grep -qF "$RUNTIME_UP" "$WORK/admin.out"; then
  ok "★ 悬空覆盖在回话里被逐条点名（地址 $RUNTIME_UP 出现在正文里）"
else
  fail "回话没点名悬空覆盖：$(cat "$WORK/admin.out")"
fi
if grep -q '悬空' "$WORK/admin.out"; then
  ok "回话说清了这是「悬空」"
else
  fail "回话没说这是悬空：$(cat "$WORK/admin.out")"
fi

# ── 判據 4：overrides=clear ⇒ 覆蓋沒了，且回話裡逐項列出被清掉的 ──────────────
#   現在登記處裡那一項是懸空的（上面 keep 換設定之後）。帶回真有上游的那份，
#   走 clear——clear 不分懸空不懸空，全清。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "overrides=clear 的 load" 200 "$CODE"
if grep -qF "$RUNTIME_UP" "$WORK/admin.out"; then
  ok "★ clear 的回话逐项列出了被清掉的那一项（地址 $RUNTIME_UP 出现在正文里）"
else
  fail "clear 的回话没有逐项列出被清掉的：$(cat "$WORK/admin.out")"
fi
expect_status "★ clear 之后数据面恢复（不再是 502）" 200 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"

# ── 判據 10：clear 之後再設一次覆蓋，資料面當場生效（身分沒斷的行為側對照）──
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "clear 之后再 disable 一次" 200 "$CODE"
expect_status "★ 身份没断：再 disable 立刻又生效" 502 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"enable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "收尾：enable 撤回来" 200 "$CODE"
expect_status "撤回之后数据面恢复" 200 "$(probe "$BASE/")"

# ── 判據 6 / 7：R11 —— 每一次回應都帶「當前有 N 項臨時覆蓋生效中（其中 M 項懸空）」──
#   ⚠ ⚠ 「每一次」就是每一次：200/400/404/413 都要帶；413 那條在 process_new_http
#   的另一個 reply() 呼叫點上（讀 body 超上界），主路徑的判據測不到它。
#   ★ ★ ★ 修復輪 1，評審 F1（Critical）：夾具必須是**兩項覆蓋、其中一項懸空**
#   （N≠M）。原先只擺一項、且那一項就是要懸空的那個 ⇒ N 與 M 永遠是同一個數
#   （都是 1），`reply()` 裡把 `{n}`/`{m}` 寫反、或兩格印同一個值，都測不出來——
#   這正是判據寫法紀律第 2 條點名的「好壞兩種情況下讀數相同的尺」。
#   ⇒ 這裡擺兩把不同的鍵：沒寫 id 的備援（$RUNTIME_UP）與寫了 id 的 up2——
#   下面這次 load 用 ov-dangling.json（只留 up2 那條路），沒寫 id 的那把因此
#   懸空，up2 那把仍然活著。
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "为 R11 摆第一项生效中的覆盖（没写 id 的那把，之后会悬空）" 200 "$CODE"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"id\":\"up2\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "为 R11 摆第二项生效中的覆盖（id=up2，之后仍然活着）" 200 "$CODE"
CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov-dangling.json")")
expect_status "为 R11 换成只留 up2 那条路的配置（没写 id 的那把悬空）" 200 "$CODE"
# 現在：登記處應該有 2 項生效中的覆蓋，其中 1 項（沒寫 id 的那把）懸空 ⇒ N=2、M=1，
# 兩個數不同——一把真正能分得清「讀對了」與「讀串了」的尺。
COUNT_LINE="当前有 2 项临时覆盖生效中（其中 1 项悬空）"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ 200 响应带着正确的计数行：$COUNT_LINE"
else
  fail "200 响应没带正确的计数行，期望包含「$COUNT_LINE」：$(cat "$WORK/admin.out")"
fi

CODE=$(admin_post /nope '{}')
expect_status "R11／404：不认识的路径" 404 "$CODE"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ 404 响应也带着计数行"
else
  fail "404 响应没带计数行：$(cat "$WORK/admin.out")"
fi

CODE=$(admin_post "/load?overrides=clear" '{ not json')
expect_status "R11／400：坏 JSON" 400 "$CODE"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ 400 响应也带着计数行"
else
  fail "400 响应没带计数行：$(cat "$WORK/admin.out")"
fi

# ★ ★ ★ 413——在 process_new_http 的另一個 reply() 呼叫點上，主路徑測不到它。
#   真發一個超過 4 MiB 上界的載荷；寫檔案再用 @file，避免把 ~5MB 塞進單個
#   shell 參數（ARG_MAX 會炸）。
head -c 5000000 /dev/zero | tr '\0' 'a' > "$WORK/big.bin"
CODE=$(curl -s -o "$WORK/admin.out" -w '%{http_code}' \
  --unix-socket "$ADMIN_SOCK" -X POST --data-binary "@$WORK/big.bin" \
  "http://localhost/load" 2>/dev/null || echo "000")
expect_status "R11／413：载荷太大" 413 "$CODE"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ ★ 413 响应也带着计数行——它在主路径之外，最容易被漏"
else
  fail "★ ★ ★ 413 响应没带计数行（brief 点名最容易漏的那条）：$(cat "$WORK/admin.out")"
fi

# 收尾：clear 掉，恢復資料面，別影響後面 [4/4] 的判據。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "overrides 小节收尾：clear 恢复" 200 "$CODE"
expect_status "收尾之后数据面恢复" 200 "$(probe "$BASE/")"

# ── [3.6/4] GET /stats（M2 批 N 任務 6，R12「與 /metrics 同源」）─────────────
#
# ⚠ ⚠ ⚠ 這一節是 brief §7 點名「R12 的同源判據本來就必須走真路徑」的完成實作：
#   下面每一條判據都從**真 socket**上打——`/stats` 走 admin unix socket，
#   `/metrics` 走資料面 HTTP 連接埠，兩者在**同一時刻**各抓一次。admin.rs 裡
#   逐欄位的深度判據已經在 Rust 單測裡做過（同源、fanout、overrides、
#   證書/快取預設、裝載時間），這裡**不重複**那些細節，只證明「路由真的接
#   在生產會走的那條線上」——任務 4 那次「16 條判據全部直接調 handler，
#   把 `("GET","/stats") => …` 那一行刪掉全部照樣綠」的教訓點名過這件事。
echo "=== [3.6/4] GET /stats 与 GET /metrics 同源 ==="

admin_get() {
  # $1 = 路徑。回狀態碼，正文寫進 $WORK/admin.out（與 admin_post 同一個約定）。
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X GET \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# ★ ★ 一把共享鍵（兩條 reverse_proxy 都沒寫 id，指同一臺 $UP_PORT）+ 一把獨立
#   鍵（id=solo，同樣指 $UP_PORT——與共享鍵**同位址不同 id**，R12 的位址級
#   歸併因此要把三條 reverse_proxy 的 upstream 行併成 /metrics 上的一行）+
#   一把指不存在位址的獨立鍵（id=third，純 IP 字面值，不需要能連上——只用來
#   把 fanout 總鍵數拉到 3，與共享鍵的 proxies=2 錯開，避免「總數與 proxies
#   恰好相等」的判據寫法陷阱）。`/__metrics` 掛在資料面，與 admin socket 上的
#   `/stats` 是兩個不同的出口。
#
# ★ ★ ★ 修復輪 1，評審 I2：`solo` 那條 `reverse_proxy` 掛了 `health_uri`，
#   而 `health_status` 故意寫成 `500`——$UP_PORT 那臺上游對**任何**路徑都回
#   200（它的設定就是裸 `respond 200 "up path=… xup=…"`，沒有一條路徑會回
#   500），於是這條探測**恆被判失敗**，不需要真的去改 $UP_PORT 自己的設定
#   （那個設定被本檔案幾十處別的判據共用，不該為了這一節去動它）。
#   ⇒ `solo` 那一行的 `healthy` 會穩定落在 `false`，`shared-a`/`shared-b`
#   沒配 `health_uri`、恆 `true`——三行合取 = `false`，R12 的 healthy 歸併
#   因此走的是**非退化值**，不再是「一刻都沒配 health_uri、恆 true」那種
#   兩邊都讀到同一個平凡值的退化場景。
cat > "$WORK/stats.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    handle /__metrics {
        metrics
    }
    handle /shared-a/* {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /shared-b/* {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /solo/* {
        reverse_proxy 127.0.0.1:$UP_PORT {
            id solo
            health_uri /solo-health-probe
            health_status 500
            health_interval 1s
        }
    }
    handle /third/* {
        reverse_proxy 127.0.0.1:19999 {
            id third
        }
    }
    handle {
        respond 200 "stats-default"
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/stats.Fulcrumfile" > "$WORK/stats.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile stats.Fulcrumfile 失败" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/stats.json")")
expect_status "装载 /stats 夹具（含撞键 + /__metrics + 一条恒不健康的探测）" 200 "$CODE"
expect_status "夹具装好之后 /shared-a 走真上游" 200 "$(probe "$BASE/shared-a/x")"

# ★ ★ ★ 等主動健康檢查真的把 solo 判成不健康——`poll_burst`/`POLL_TIMEOUT`
#   是 [2c/4] 那一節留下的現成 helper（同一份指令碼，函式在前面定義過就能在
#   這裡用）。判據掛在**資料面行為**上（502），不是「配了 health_uri 就當它
#   已經生效」——剛裝載完的那一小段視窗裡 healthy 恆為初值 true（同 [2c/4]
#   的教訓）。
HIT=$(poll_burst "$BASE/solo/x" 502 5)
if [ "$HIT" = "5" ]; then
  ok "★ solo 探测路径恒 500 ⇒ 它被判不健康（数据面上 /solo/* 变成 502）"
else
  fail "solo 期望被判不健康（连 5 次 502），$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/5"
fi

# 只做一件事的小工具：解析 /stats 的 JSON 與 /metrics 的 exposition 文字，
# 按位址做歸併再互相比對。★ 判據本身（同源相等）落在 bash 這一層的
# `expect_status`/`ok`/`fail` 上，python3 只負責把兩份格式都不同的正文
# 讀成同一種形狀——這與 tests/metrics/run.sh 的 EXPO 是同一個分工。
cat > "$WORK/stats_check.py" <<'PY'
#!/usr/bin/env python3
"""只服务于 tests/serve/run.sh 的 [3.6/4] 一节：读 /stats 的 JSON 与
/metrics 的 Prometheus exposition，按子命令回答一个具体问题。
⚠ 读不懂就当场非零退出——不猜、不吞。"""
import json
import re
import sys


def load_stats(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def metrics_gauge(path, family, addr):
    """/metrics 那一侧：Prometheus text exposition 里某个 gauge 的读数（float）。
    找不到就抛——调用方不该拿到一个假装存在的默认值。"""
    pat = re.compile(
        r"^" + re.escape(family) + r"\{upstream=\"" + re.escape(addr) + r"\"\}\s+(\S+)$"
    )
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = pat.match(line.rstrip("\n"))
            if m:
                return float(m.group(1))
    raise SystemExit(f"没找到 {family}{{upstream=\"{addr}\"}} 这一行（{path}）")


def merge_stats_upstreams(v, addr):
    """/stats 那一侧按地址现算：inflight 求和、healthy 取合取。
    返回 (行数, inflight 和, healthy 是否全真)。"""
    rows = [r for r in v["upstreams"] if r["addr"] == addr]
    if not rows:
        raise SystemExit(f"upstreams 里没有 addr={addr}")
    return len(rows), sum(r["inflight"] for r in rows), all(r["healthy"] for r in rows)


cmd = sys.argv[1]

if cmd == "shape":
    (stats_path,) = sys.argv[2:]
    v = load_stats(stats_path)
    need = [
        "pid",
        "config_loaded_at_unix",
        "upstreams",
        "overrides",
        "fanout",
        "fanout_shared",
        "cache",
        "certs",
    ]
    missing = [k for k in need if k not in v]
    if missing:
        print(f"缺顶层字段：{missing}", file=sys.stderr)
        sys.exit(1)
    for k in ("upstreams", "overrides", "fanout", "fanout_shared"):
        if not isinstance(v[k], list):
            print(f"`{k}` 应该是数组，实际是 {type(v[k])}", file=sys.stderr)
            sys.exit(1)
    print("OK")

elif cmd == "fanout_proxies":
    # 打印 fanout_shared 里某个 addr 的 proxies；找不到、或混进了
    # proxies<=1 的项，就非零退出。
    stats_path, addr = sys.argv[2:]
    v = load_stats(stats_path)
    rows = [r for r in v["fanout_shared"] if r["addr"] == addr]
    if not rows:
        print(f"fanout_shared 里没有 addr={addr}", file=sys.stderr)
        sys.exit(1)
    if any(r["proxies"] <= 1 for r in rows):
        print(f"fanout_shared 混进了 proxies<=1 的项：{rows}", file=sys.stderr)
        sys.exit(1)
    print(rows[0]["proxies"])

elif cmd == "fanout_total":
    (stats_path,) = sys.argv[2:]
    print(len(load_stats(stats_path)["fanout"]))

elif cmd == "rows_for_addr":
    stats_path, addr = sys.argv[2:]
    n, _, _ = merge_stats_upstreams(load_stats(stats_path), addr)
    print(n)

elif cmd == "field":
    # 打印一个顶层标量字段（比如 pid）——只给不需要专门子命令的简单读取用。
    stats_path, key = sys.argv[2:]
    v = load_stats(stats_path)
    if key not in v:
        print(f"顶层没有字段 `{key}`", file=sys.stderr)
        sys.exit(1)
    print(v[key])

elif cmd == "addrs":
    (stats_path,) = sys.argv[2:]
    for r in load_stats(stats_path)["upstreams"]:
        print(r["addr"])

elif cmd == "same_source":
    # ★ ★ ★ R12 的核心判据：把 /stats 那一份按地址求和/取合取之后，与
    # /metrics 那一份逐项相等。⛔ ⛔ 不是「两边都不是 0」——这里对**每一个
    # 家族**都用真的数值相等比较，哪怕两边这一刻都读到 0 / true，比较本身
    # 仍然是在问「这两个数是不是同一个源」，不是在问「这两个数是不是非零」。
    stats_path, metrics_path, addr = sys.argv[2:]
    v = load_stats(stats_path)
    _, stats_inflight, stats_healthy = merge_stats_upstreams(v, addr)
    metrics_inflight = metrics_gauge(metrics_path, "fulcrum_upstream_inflight", addr)
    metrics_healthy = metrics_gauge(metrics_path, "fulcrum_upstream_healthy", addr)
    bad = []
    if float(stats_inflight) != metrics_inflight:
        bad.append(f"inflight：/stats 归并={stats_inflight}，/metrics={metrics_inflight}")
    want_healthy = 1.0 if stats_healthy else 0.0
    if want_healthy != metrics_healthy:
        bad.append(f"healthy：/stats 归并={stats_healthy}，/metrics={metrics_healthy}")
    if bad:
        print("；".join(bad), file=sys.stderr)
        sys.exit(1)
    print(f"OK inflight={stats_inflight} healthy={stats_healthy}")

else:
    print(f"不认识的子命令：{cmd}", file=sys.stderr)
    sys.exit(1)
PY

# ── 判據 1：GET /stats 從真 socket 上打，回 200 + 合法 JSON ──────────────────
CODE=$(admin_get /stats)
expect_status "GET /stats（真 socket）" 200 "$CODE"
cp "$WORK/admin.out" "$WORK/stats1.json"
if python3 "$WORK/stats_check.py" shape "$WORK/stats1.json"; then
  ok "/stats 的正文是合法 JSON，且带齐 8 个顶层字段"
else
  fail "/stats 的正文形状不对（见上面 python3 的报错）"
fi

# ★ ★ 修復輪 1，評審 M1：單測裡 handler 與斷言同處理程序，`pid` 那條判據近乎
#   恆真（好壞兩種情況讀數相同）。這裡換一個**獨立**的對照物——
#   `--pid-file "$WORK/proxy.pid"` 是 `proxy` 那個 `fulcrum serve` 處理程序
#   自己寫入磁碟的、裝的正是它自己的 OS pid（見 `process.rs::write_pid_file`），
#   與 JSON 裡的 `pid` 欄位是兩條完全不同的路徑各自量出來的同一件事。
#   ★ 分辨力由這個建構保證（不同原始碼位置 + 跨真實處理程序邊界），可注入的
#   錯法是存在的——比如把 `stats()` 的 `pid` 接成 `std::process::id() + 1`
#   或任何寫死值，這條判據會抓住。
#
# ★ ★ ★ 修復輪 2，評審 N1：這兩行原來是裸的 `VAR=$(...)`——`field` 子命令
#   在欄位不存在時 `sys.exit(1)`，`cat` 在檔案不存在時也非零退出，兩者在
#   set -e 下都會讓指令碼半路硬中止而不是優雅報紅（與判據 3 那條
#   `SAME_SOURCE_OUT` 是同一類坑，這兩行當時漏包了）。⇒ 改用上面新寫的
#   `capture()`——本檔案現在「捕獲一個可能失敗的命令」只有這一種寫法。
capture python3 "$WORK/stats_check.py" field "$WORK/stats1.json" pid
JSON_PID=$CAPTURE_OUT
JSON_PID_RC=$CAPTURE_RC
capture cat "$WORK/proxy.pid"
PROXY_PID=$CAPTURE_OUT
PROXY_PID_RC=$CAPTURE_RC
if [ "$JSON_PID_RC" -eq 0 ] && [ "$PROXY_PID_RC" -eq 0 ] && [ -n "$JSON_PID" ] && [ "$JSON_PID" = "$PROXY_PID" ]; then
  ok "★ ★ /stats 的 pid（$JSON_PID）与 proxy 进程自己的 pid 文件（$PROXY_PID）一致"
else
  fail "pid 判据没通过：JSON_PID=「$JSON_PID」(rc=$JSON_PID_RC)，PROXY_PID=「$PROXY_PID」(rc=$PROXY_PID_RC)"
fi

# ── 判據 2：fanout_shared 裡那把撞鍵的 proxies == 2，走真 HTTP ──────────────
PROXIES=$(python3 "$WORK/stats_check.py" fanout_proxies "$WORK/stats1.json" "127.0.0.1:$UP_PORT" || echo "ERR")
if [ "$PROXIES" = "2" ]; then
  ok "★ ★ fanout_shared 里 127.0.0.1:$UP_PORT 显示 proxies=2"
else
  fail "fanout_shared 的 proxies 应该是 2，实际「$PROXIES」"
fi
# ★ ★ ★ 修復輪 1，評審 I1：這裡要的是**等值**斷言，不是「不等於 2」——
#   夾具的真實總鍵數是**已知的字面值 3**（共享位址一把 + solo 一把 + third
#   一把）。寫成 `!= 2` 的話，`python3` 硬失敗時 `TOTAL="ERR"`，
#   `"ERR" != "2"` 為真 ⇒ 會打出一個**綠的** ✓（`fail` 只累加計數不退出，
#   上游 `shape` 先紅也擋不住這一條靜默通過）；即便 `python3` 沒壞，`0`/`1`/
#   `99` 這類錯誤值同樣會被 `!= 2` 放行——那是一把「不等值」判據，只能擋住
#   `TOTAL` 恰好等於 2 的那一種錯法。改成 `= 3` 之後，`ERR` 與任何非 3 的值
#   都會被正確判紅。
TOTAL=$(python3 "$WORK/stats_check.py" fanout_total "$WORK/stats1.json" || echo "ERR")
if [ "$TOTAL" = "3" ]; then
  ok "★ fanout 总键数 = 3（共享地址 + solo + third），与 proxies（2）不相等——判据写法纪律"
else
  fail "fanout 总键数应该是 3，实际「$TOTAL」"
fi

# ── 判據 3：R12 同源——GET /metrics 與 GET /stats 同一時刻各抓一次，
#   /stats 按位址歸併之後與 /metrics 逐項相等 ────────────────────────────────
CODE=$(probe "$BASE/__metrics")
expect_status "GET /metrics（数据面，真 HTTP）" 200 "$CODE"
cp "$WORK/body" "$WORK/metrics1.txt"

ROWS=$(python3 "$WORK/stats_check.py" rows_for_addr "$WORK/stats1.json" "127.0.0.1:$UP_PORT" || echo "ERR")
if [ "$ROWS" = "3" ]; then
  ok "127.0.0.1:$UP_PORT 在 /stats 里出了 3 行（shared-a/shared-b/solo 各一行，不归并）"
else
  fail "应该有 3 行未归并的 upstream 记录，实际 $ROWS 行"
fi

# ⚠ ⚠ 判據寫法紀律：不許「兩邊都不是 0」——`same_source` 比的是**逐項相等**
#   （數值比較，不是字串比較）。inflight 這一格這一刻大機率仍然是退化的
#   0（真實並行流量在這段指令碼裡沒有製造出來，與 admin.rs 裡 Rust 單測那條
#   分開覆蓋）；★ ★ ★ 但 healthy 這一格**不是**——上面剛等到 solo 被判
#   不健康（502 已經證實），三行 healthy 取合取的結果是**非退化的 false**，
#   不再是「沒配 health_uri、恆 true」那種兩邊巧合讀到同一個平凡值的場景。
#   把 /stats 那一側改成「自己再數一遍」而不讀同一組原子量，這條判據必須紅
#   （反證見任務報告——本輪修復要求做實這一條，不許只在報告裡寫一句了事）。
# ⚠ ⚠ 用 `capture()`（`same_source` 判定「不同源」時會非零退出，裸的
#   `$(...)` 賦值在 set -e 下會讓指令碼半路硬中止——那正是修復輪 1 順手
#   加固過的那類坑，見 §4.5 反證；修復輪 2 又在 M1 那兩行新栽了一次，
#   見 §11——現在全檔案這一族統一走 `capture()`，不再各自手包一次）。
capture python3 "$WORK/stats_check.py" same_source \
     "$WORK/stats1.json" "$WORK/metrics1.txt" "127.0.0.1:$UP_PORT"
SAME_SOURCE_OUT=$CAPTURE_OUT
SAME_SOURCE_RC=$CAPTURE_RC
if [ "$SAME_SOURCE_RC" -eq 0 ]; then
  ok "★ ★ ★ R12：/stats 按地址归并（求和 inflight / 取合取 healthy）之后与 /metrics 逐项相等（$SAME_SOURCE_OUT）"
else
  fail "★ ★ ★ R12：/stats 与 /metrics 不同源：$SAME_SOURCE_OUT"
fi
# ★ ★ 非退化確認（呼應 admin.rs 那條 Rust 判據收尾的「順帶確認」）：光
#   「逐項相等」擋不住兩邊**都**讀到同一個平凡值——這裡再核一次 healthy
#   落的確實是 False，不是巧合的 True。
case "$SAME_SOURCE_OUT" in
  *"healthy=False"*) ok "★ 确认 healthy 归并结果是非退化的 False（不是两边巧合都读到 True）" ;;
  *) fail "healthy 应该归并成 False（solo 那一行不健康），实际：$SAME_SOURCE_OUT" ;;
esac

# ── 判據 4：不快取 Runtime 快照——POST /load 換掉上游之後，/stats 立刻反映
#   新的那份（不是繼續舉著換設定之前那份） ───────────────────────────────────
cat > "$WORK/stats-swapped.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    reverse_proxy 127.0.0.1:$UP_PORT {
        id swapped
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/stats-swapped.Fulcrumfile" > "$WORK/stats-swapped.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile stats-swapped.Fulcrumfile 失败" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/stats-swapped.json")")
expect_status "换掉 /stats 夹具的上游（拆掉撞键，只留 id=swapped 一条）" 200 "$CODE"

CODE=$(admin_get /stats)
expect_status "换配置之后再打一次 GET /stats" 200 "$CODE"
if python3 "$WORK/stats_check.py" shape "$WORK/admin.out"; then
  ok "换配置之后 /stats 仍然是合法 JSON"
else
  fail "换配置之后 /stats 的形状坏了"
fi
if python3 "$WORK/stats_check.py" addrs "$WORK/admin.out" | grep -qx "127.0.0.1:19999"; then
  fail "★ ★ ★ R12：/stats 还看得到换配置之前的老上游 127.0.0.1:19999 —— 像是缓存了一份旧 Runtime"
else
  ok "★ ★ ★ 换配置之后老上游 127.0.0.1:19999（third 那条）不在 /stats 里了"
fi
FANOUT_TOTAL_AFTER=$(python3 "$WORK/stats_check.py" fanout_total "$WORK/admin.out" || echo "ERR")
if [ "$FANOUT_TOTAL_AFTER" = "1" ]; then
  ok "★ ★ ★ 换配置之后 fanout 只剩新配置里那一把键（不再是撞键的 2）——/stats 没举着旧的 Runtime 快照"
else
  fail "换配置之后 fanout 应该只有 1 把键（新配置只有一条 reverse_proxy），实际 $FANOUT_TOTAL_AFTER"
fi

# ── 判據 5：404 的「可用」清單裡有 GET /stats（真 socket）───────────────────
CODE=$(admin_post /nope '{}')
expect_status "GET /stats 判据同一节里顺带核一次：/nope 仍然 404" 404 "$CODE"
if grep -q 'GET /stats' "$WORK/admin.out"; then
  ok "404 的可用清单里有 GET /stats"
else
  fail "404 的可用清单没有提到 GET /stats：$(cat "$WORK/admin.out")"
fi

# 收尾：clear 掉、換回 [4/4] 需要的乾淨基線，避免影響後面的裝載紀錄判據。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "[3.6/4] 收尾：clear 恢复" 200 "$CODE"
expect_status "收尾之后数据面恢复" 200 "$(probe "$BASE/")"

# ── [4/4] 裝載紀錄該說的話 ──────────────────────────────────────────────────
echo "=== [4/4] 装载日志 ==="
# ★ ★ G52（隱式的、使用者沒寫出來的東西必須可見）沒有作廢，它換了物件：
#   回落層刪完之後，承擔它的是快取與靜態檔案那兩處「把生效的預設值打出來」。
#   ⇒ 這條判據因此釘兩件事：**回落那幾個字不許再出現**，
#   而**快取的生效設定必須出現**。
if grep -q '回落' "$WORK/proxy.log"; then
  fail "装载日志里还有「回落」—— 那一层已整层删除（G98）"
  grep '回落' "$WORK/proxy.log" >&2
else
  ok "★ 装载日志里再没有回落（那一层已归零）"
fi
# ★ G52 的接班人：`cache` 的生效設定必須被打出來，且要說清 `ttl` 是**備援**。
#   ⚠ 少了後半句，從 nginx 遷過來的人會以為它是覆蓋 —— 而那是一個
#   與使用者預期相反、卻不說出來的預設。
if grep '缓存：站点' "$WORK/proxy.log" | grep -q '兜底'; then
  ok "★ 装载日志说出了缓存的生效设置（含「ttl 是兜底」这一句）"
else
  fail "装载日志没把缓存的生效设置说出来（G52 的接班人）"
  grep -i '缓存' "$WORK/proxy.log" >&2 || sed -n '1,20p' "$WORK/proxy.log" >&2
fi
# ── ⏳ 未串接的能力也要說出來，**而已經串接的不許再被說** ──────────────────
#
# ⚠ ⚠ 這條斷言的錨點在兩天裡被迫換了兩次（批 10 `dns_refresh` 串接、
#   批 11 `health_uri` 串接），兩次都是**當場紅給我看的** —— 那是它在正常工作。
#   ★ 但它暴露了一個真問題：**錨點是個會移動的靶子**。等最後一條未串接能力
#   也做完，這條斷言會無處可錨，而那一天最省事的做法是把它刪掉。
#
# ⇒ 改成兩半，各自守一個方向：
#   ① **正向**：這行公告的**固定前綴**必須出現（它不隨任何一條能力的死活而變），
#      而且這一份夾具確實用到了一條仍未串接的能力（`passive_fail`）。
#   ② ★ ★ **反向**：**已經串接的能力不許出現在這行公告裡**。
#      這一半才是真正會**無聲**的那種爛法 —— 一條假警告不會紅，
#      它只會讓人漸漸不看這張表。批 8 串接完 ACME 之後，
#      `dsl-reference.md` 那句清單掛著「自動簽發」躺了四天沒人發現，
#      就是因為**沒有任何東西在看反向那一半**。
UNWIRED_LINES=$(grep -F '这一批还没接线' "$WORK/proxy.log" || true)
if [ -n "$UNWIRED_LINES" ]; then
  ok "装载日志有未接线能力的公告"
else
  fail "装载日志里一条未接线公告都没有（而夹具里写了 tracing）"
fi
# ⚠ ⚠ **錨點在 G136 又換了一次**（這是第三次：批 10 `dns_refresh`、批 11 `health_uri`、
#   本次 `passive_fail`），三次都是當場紅給我看的。⇒ 現在錨在 `tracing` 上，
#   而夾具裡那一行 `tracing` 就是為它寫的。
if printf '%s' "$UNWIRED_LINES" | grep -qF 'tracing'; then
  ok "公告点名了 tracing（它确实还没接线）"
else
  fail "公告里没有 tracing：$UNWIRED_LINES"
fi
# ★ ★ 反向：這幾條**已經串接**了，公告裡再出現就是假警告。
STALE=""
# ★ G136：`passive_fail` 進這張「已串接」清單 —— 它此後再出現在公告裡就是假警告。
for cap in health_uri dns_refresh admin acme passive_fail; do
  if printf '%s' "$UNWIRED_LINES" | grep -qF "$cap"; then
    STALE="$STALE $cap"
  fi
done
if [ -z "$STALE" ]; then
  ok "★ 已经接线的能力没有出现在未接线公告里（没有假警告）"
else
  fail "未接线公告里还挂着已经接线的能力：$STALE"
fi
# ★ 證書裝載也要在紀錄裡留痕：裝了幾個 SNI、哪些。
if grep -q '已装载 1 个 SNI' "$WORK/proxy.log"; then
  ok "装载日志说出了装了几张证书"
else
  fail "装载日志里没有证书装载那一行"
  grep -i 'SNI\|证书' "$WORK/proxy.log" >&2 || true
fi
# ★ ★ 裝載紀錄是 `default_sni` 在執行中**唯一**看得見的地方。
#   ⚠ 它此前正是卡在「配了」與「生效了」分不開這一點上：編譯得過、執行環境零呼叫方，
#   而紀錄一個字都不說 ⇒ 配了它的人沒有任何辦法在現場發現它沒接。
#   ★ 這一條與 9d 分工：9d 問行為，這一條問**維運看不看得見**。
if grep -qF 'default_sni secure.example' "$WORK/proxy.log"; then
  ok "装载日志说出了不带 SNI 的握手会用哪张证书"
else
  fail "装载日志没说 default_sni 生效到了哪个名字"
  grep -i 'SNI' "$WORK/proxy.log" >&2 || true
fi
# ── ★ ★ ★ 自環：容器裡的 `reverse_proxy` 指回自己，裝載紀錄必須說出來 ────────
#
# ⚠ ⚠ 這條警告在 `fulcrum-runtime` 裡算了很久，而**一個消費者都沒有**：
#   `Runtime::self_loop_warnings` 全倉唯一的讀者是單測，`log_load_summary`
#   一個字都不說。⇒ 端到端這一層此前的讀數是「算得對」，而維運現場讀到的是
#   **一片安靜** —— 正是本儲存庫那條老形狀：*一個用來「出了事你能知道」的東西，
#   自己不說話時沒人知道。*
# ★ 與單測分工：單測釘**扁平**站台的判定，這一格釘**容器下鑽**（夾具裡那條
#   `/selfloop/*` 嵌在 `handle` arm 裡）+ **它真的被列印出來了**。
SELFLOOP_LINES=$(grep -F '转发回自己' "$WORK/proxy.log" || true)
if [ -n "$SELFLOOP_LINES" ]; then
  ok "★ 装载日志说出了自环（容器里的 reverse_proxy 指回本进程的监听口）"
else
  fail "装载日志一个字都没提自环 —— 而夹具里 /selfloop/* 指着 127.0.0.1:$PROXY_PORT"
fi
# ★ 點得出是**哪個上游**：一句「有自環」而不說是誰，維運得自己翻整份設定。
if printf '%s' "$SELFLOOP_LINES" | grep -qF "127.0.0.1:$PROXY_PORT"; then
  ok "★ 自环警告点名了那个上游地址"
else
  fail "自环警告没说是哪个上游：$SELFLOOP_LINES"
fi
# ★ ★ 反向那一半：**不自環的上游不許出現在這條警告裡。**
#   ⚠ 少了它，一個「對每條 reverse_proxy 都報一句」的實作在上面兩條上全綠，
#   而現場是每份正常設定都挨一堆假警告 —— 而假警告會把真的那條一起埋掉
#   （這一節上面那條「已串接的能力不許出現在未串接公告裡」守的是同一件事）。
if printf '%s' "$SELFLOOP_LINES" | grep -qF "127.0.0.1:$UP_PORT"; then
  fail "自环警告把不自环的上游 127.0.0.1:$UP_PORT 也说进去了：$SELFLOOP_LINES"
else
  ok "★ 不自环的上游没有被说成自环（没有假警告）"
fi
# ★ 恰好一條：夾具裡只有一處自環，多出來的每一條都是噪音。
SELFLOOP_N=$(printf '%s' "$SELFLOOP_LINES" | grep -cF '转发回自己' || true)
if [ "$SELFLOOP_N" = "1" ]; then
  ok "★ 自环警告恰好一条（夹具里就一处自环）"
else
  fail "自环警告有 $SELFLOOP_N 条，夹具里只有一处自环：$SELFLOOP_LINES"
fi

# ── 執行緒模型：那個旋鈕真的落到處理程序上了嗎（G35 結案 · G140 完成實作）────────────
#
# ★ ★ ★ **這一節存在的全部理由**：`serve()` 裡那三行 `s.threads = Some(...)`
#   在此之前**沒有任何判據執行過** —— 單測夠不到 `serve()`（它回傳 `!`、還要綁連接埠），
#   而 `process.rs` 那幾條純函式測試只證明`算出来的数对`，⛔ 不證明`它被用上了`。
#   一條從未被執行過的路徑，`它生效了`就是一句沒有判據的斷言。
#
# ★ 判據掛在**處理程序真的有幾個 OS 執行緒**上（`/proc/<pid>/status` 的 `Threads:`），
#   ⛔ 不掛在啟動紀錄那一行上：紀錄打的是**剛算出來的那個數**，
#   對`它有没有真的传给 pingora`零判別力（同 G135 那條`回显自己刚写进去的值`）。
#
# ⚠ 2026-09-05 實測（12 核）：`Threads` = **4 + threads_l7**，嚴格線性 ——
#   `threads_l7 1`→5 · `8`→12 · `16`→20 · 不寫（12 核）→16。
#   下面三條界都是按這組實測數定的，⛔ 不是猜的。
echo "=== [4/4] 线程模型（G140）==="

threads_of() {
  # 起一個只帶一行全域設定的最小執行個體，量它的 OS 執行緒數，收掉，列印那個數。
  local label=$1 line=$2 port=$3
  {
    echo "{"
    [ -n "$line" ] && echo "  $line"
    echo "}"
    echo "http://127.0.0.1:$port {"
    echo "  respond 200"
    echo "}"
  } > "$WORK/$label.Fulcrumfile"

  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/$label.Fulcrumfile" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$label.pid" \
    --upgrade-sock "$WORK/$label.sock" \
    > "$WORK/$label.log" 2>&1 &
  local pid=$!
  if ! wait_port "$port"; then
    echo "0"
    return
  fi
  # ⚠ 讓 runtime 把 worker 都拉起來再量：tokio 的 worker 是建 runtime 時就 spawn 的，
  #   但`端口在听`比那一步早一點點。
  sleep 1
  awk '/^Threads:/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo "0"
  kill -INT "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  wait_port_gone "$port" 2>/dev/null || true
}

T_ONE=$(threads_of threads_one "threads_l7 1" "$THREADS_PORT_A")
T_EIGHT=$(threads_of threads_eight "threads_l7 8" "$THREADS_PORT_B")
T_DEFAULT=$(threads_of threads_default "" "$THREADS_PORT_C")
NPROC=$(nproc 2>/dev/null || echo 0)
note "实测线程数：threads_l7=1 → $T_ONE · threads_l7=8 → $T_EIGHT · 不写 → $T_DEFAULT（nproc=$NPROC）"

# ★ `读不出来`判紅，⛔ 不當成通過 —— 三個數裡任何一個是 0 都說明量具沒工作。
if [ "$T_ONE" -gt 0 ] && [ "$T_EIGHT" -gt 0 ] && [ "$T_DEFAULT" -gt 0 ]; then
  ok "三个实例的线程数都量到了"
else
  fail "线程数没量到（$T_ONE / $T_EIGHT / $T_DEFAULT）——「没能检查」不算「检查通过」"
fi

# ① 旋鈕真的會動它。★ 實測差值恰好是 7，界取 6 留一格餘量
#   （⛔ 不寫死 7：tokio 的阻塞池可能臨時多出一條，而那與本條判據無關）。
if [ "$((T_EIGHT - T_ONE))" -ge 6 ]; then
  ok "★ 「threads_l7」 从 1 调到 8，线程数多了 $((T_EIGHT - T_ONE)) 条 ⇒ 那个旋钮真的落到进程上了"
else
  fail "「threads_l7」 从 1 调到 8 只多了 $((T_EIGHT - T_ONE)) 条线程 —— 那三行 s.threads 多半没接上"
fi

# ② 反方向的界：`threads_l7 1` 那一趟必須**真的少**。
#   ⚠ 沒有這一條，一個`无论配什么都起一百条线程`的實作在 ① 上照樣過不了 ——
#   但一個`无论配什么都按核数起`的實作會讓 ① 的差值恰好是 0，而 ② 才說得出它錯在哪。
if [ "$T_ONE" -le 8 ]; then
  ok "「threads_l7 1」 那一趟只有 $T_ONE 条线程 ⇒ 它没有偷偷按核数起"
else
  fail "「threads_l7 1」 却起了 $T_ONE 条线程 —— 配置多半根本没被读"
fi

# ③ ★ ★ **不寫時的預設真的是核數**（G140 ②：owner 拍板讓 G35 的初值這次真的生效）。
#   ⚠ 判成差值而不是絕對值：那 4 條固定執行緒（管理面 + 後台）與本條無關。
#   ⚠ 核太少時這條判據沒有分辨力，跳過並說出來 —— ⛔ 不靜默跳過。
if [ "$NPROC" -ge 4 ]; then
  if [ "$((T_DEFAULT - T_ONE))" -ge "$((NPROC - 2))" ]; then
    ok "★ 不写 「threads_l7」 时比写 1 多了 $((T_DEFAULT - T_ONE)) 条（nproc=$NPROC）⇒ 角色缺省真的是核数"
  else
    fail "不写 「threads_l7」 时只比写 1 多了 $((T_DEFAULT - T_ONE)) 条（nproc=$NPROC）—— L7 的角色缺省多半没生效"
  fi
else
  note "nproc=$NPROC < 4，「缺省取核数」这条判据在这台机器上分辨不出来，本趟跳过"
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "SERVE TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 代理日志 ──" >&2
  cat "$WORK/proxy.log" >&2
  echo "── 上游日志 ──" >&2
  cat "$WORK/upstream.log" >&2
  exit 1
fi
echo "SERVE TESTS PASSED —— 路由决策被真流量执行对了（转发 / 改写 / header_up / 重定向 / 421 / file_server 自研 / cache 裹转发 / keep-alive / HTTPS+SNI+h2 / 4 KiB 响应一个数据段 / 错误页取原始请求头 / 续不续听 pingora 的判定 / 反代端口的头名原样到上游）。"
