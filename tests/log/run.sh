#!/usr/bin/env bash
# 結構化訪問紀錄端到端（**M2 批 L 第 ② + ③ 步**；D7 由 G113 + G114 結案）。
#
# ★ ★ 為什麼它是**獨立一格**：別的場景驗的都是「回應對不對」，而這一格驗的是
#   **回應之外的那一行** —— 它寫在別的地方、有自己的格式契約、有自己的開關。
#   ⚠ 混進 `tests/serve/run.sh` 的話，一條紀錄斷言紅了，讀紀錄的人會先去看路由。
#
# ★ ★ ★ **判據掛在那一行的內容上，不是掛在「檔案非空」上。**
#   契約（`docs/architecture/observability.md`）定的是**欄位清單**，
#   而「有輸出」與「輸出對」之間隔著整個欄位表。
#
# 連接埠（★ 與其餘場景全都錯開，見 AGENTS.md 那張連接埠表）：
#   9900 站台 A —— `log { output file … }`，本格主要在它上面量
#   9901 站台 B —— `log { output stderr }`
#   9902 站台 C —— **沒配 `log`**（反向判據要它）
#   9903 站台 D —— `log { level warn }`（閾值那一格）★ 它同時是「**沒配 headers**」的對照物
#   9904 上游（**另一個執行個體**，免得自環警告汙染裝載紀錄）
#   9905 **永遠沒人在聽**（「連不上上游 ⇒ outcome=error」那條要它）
#   9906 站台 E —— **HTTPS**（批 L 第 ③ 步的 TLS 四格全在它上面量）
#          ★ 它同時是 **h3** 那一半的落點：G110 讓有 TLS 的連接埠**自動在同一個連接埠號上聽 UDP**
#          ⇒ 不用另起服務，同一個 9906 上就有 HTTP/3（D27 結案）

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
A_PORT=${A_PORT:-9900}
B_PORT=${B_PORT:-9901}
C_PORT=${C_PORT:-9902}
D_PORT=${D_PORT:-9903}
UP_PORT=${UP_PORT:-9904}
# ★ 永遠沒人在聽 —— 「連不上上游 ⇒ 錯誤頁」那條判據要它。
DEAD_PORT=${DEAD_PORT:-9905}
# ★ HTTPS 那一格（批 L 第 ③ 步）：TLS 四格只有在真的握過手之後才有值。
E_PORT=${E_PORT:-9906}
ADMIN_SOCK="$WORK/admin.sock"
LOGFILE="$WORK/access.json"

FAILS=0
PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid waited
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    waited=0
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

lines() { wc -l < "$LOGFILE" | tr -d ' '; }

# 取紀錄檔案最後一行的某個欄位。★ 用 python3 而不是 grep：
# ⚠ 一個 `grep '"status":200'` 會在欄位順序、空格、轉義任一處變化時**靜默漏判**，
#   而那正是「判據看起來在守契約、其實在守一個字串」的形狀。
field() {
  python3 -c '
import json, sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
o = json.loads(line)
k = sys.argv[2]
print(o[k] if k in o else "<缺>")
' "$LOGFILE" "$1"
}

req() { curl -sS -o /dev/null -w '%{http_code}' --max-time 5 -H "Host: $2" "http://$HOST:$1$3" 2>/dev/null || echo 000; }

# 這一行裡到底有哪些鍵。★ 給「一個 `req_hdr_*` 都不許有」那幾條反向判據用。
keys() {
  python3 -c '
import json, sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
print(" ".join(json.loads(line).keys()))
' "$LOGFILE"
}

# 這一行的**原始位元組**裡有沒有出現某個串。
# ★ ★ 與 `field` 不同：那個按鍵取值，而這一條問的是「它在這一行裡出現過沒有」——
#   ⚠ 一個把 `Authorization` 寫進了**某個別的鍵**的實作，按鍵取值是看不見的。
line_has() {
  python3 -c '
import sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
sys.exit(0 if sys.argv[2].lower() in line.lower() else 1)
' "$LOGFILE" "$1"
}

# ── [0/6] 基線 ──────────────────────────────────────────────────────────────
#
# ★ ★ 兩條自證：連接埠沒被佔（否則測的是別人的服務）；**紀錄檔案此刻不存在**
#   （否則「多了一行」這種判據分不出「剛寫的」與「本來就有的」）。
echo "=== [0/9] 基线：端口空着，日志文件还不存在 ==="
for p in "$A_PORT" "$B_PORT" "$C_PORT" "$D_PORT" "$UP_PORT" "$DEAD_PORT" "$E_PORT"; do
  if port_listening "$p"; then
    echo "LOG TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
ok "七个端口都空着"
if [ -e "$LOGFILE" ]; then
  echo "LOG TESTS FAILED: $LOGFILE 已经存在，本次结果不可采信。" >&2
  exit 1
fi
ok "日志文件还不存在（「多了一行」才说得清）"

# ── 設定 ────────────────────────────────────────────────────────────────────
# ⚠ 上游寫成**不帶主機名**的 `:PORT`：轉發過去時 Host 還是 `a.example`，
#   而一個帶主機名的上游站台會對它回 **421** —— 第一次跑正是栽在這裡，
#   ★ 而那條 421 一路順著紀錄顯形（status/level/resp_size 三格同時紅），
#   說明這一格量的確實是真流量，不是一個造出來的形狀。
cat > "$WORK/up.Fulcrumfile" <<CONF
:$UP_PORT {
    respond 200 "from-upstream"
}
CONF

# ── 自簽證書（HTTPS 那一格要，批 L 第 ③ 步）───────────────────────────────
#
# ⚠ SAN 必須有 `a.example`：樞衡按**證書自己的 SAN** 決定這張證書裝在哪些 SNI 上。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" \
  -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "LOG TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

cat > "$WORK/a.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

http://a.example:$A_PORT {
    log {
        output file $LOGFILE
        headers User-Agent X-Trace-Id
        resp_headers Content-Length X-Cell
    }
    header X-Cell log3
    # ⚠ 下面两条指着**同一台**上游，而且**有意都不写 id**（M2 批 N 任务 2.9 / G125，
    #   裁决 R6 ③ 第二轮）⇒ 它们的键「站点名 + id + 上游地址」完全相同，
    #   共享同一个覆盖格子，一次 disable 两条一起摘掉。
    #   ★ 这是「一个后端挂在几组 handle 路由后面」那个最常见的形状，它一个字节
    #     都不用改就装得上。写了 id 才分得开的那一路由 tests/serve 那份夹具覆盖。
    #   ⚠ 任务 2.8 曾照第一轮口径把这个形状在装载期拒掉，本场景当场装不上。
    # ⚠ ⚠ 这个 heredoc **不带引号**（因为要展开端口变量）⇒ 本行里的反引号
    #   会被当成命令替换。在这种 heredoc 里写注释一律用「」，别用反引号。
    handle /rw/* {
        rewrite * /rewritten
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /up {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /go {
        redir * https://example.com/moved 301
    }
    handle /files/* {
        file_server {
            root $WORK/www
        }
    }
    handle /boom {
        respond 503
    }
    handle /dead {
        reverse_proxy 127.0.0.1:$DEAD_PORT
    }
    respond 200 "a-ok"
}

http://b.example:$B_PORT {
    log {
        output stderr
    }
    respond 200 "b-ok"
}

http://c.example:$C_PORT {
    respond 200 "c-ok"
}

http://d.example:$D_PORT {
    log {
        output file $LOGFILE
        level warn
    }
    handle /bad {
        respond 404
    }
    respond 200 "d-ok"
}

# ★ ★ HTTPS 那一格（**批 L 第 ③ 步**）。⚠ 有意**不写** headers/resp_headers ——
#   它量的是 TLS 四格，而「白名单」在站点 A 上量；一格测一件事，红了指得准。
a.example:$E_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    log {
        output file $LOGFILE
    }
    respond 200 "e-ok"
}
CONF

mkdir -p "$WORK/www/files"
printf 'file-body\n' > "$WORK/www/files/x"

# ── [1/6] 起服務 ────────────────────────────────────────────────────────────
echo "=== [1/9] 起上游与被测实例 ==="
start() {
  local name=$1 conf=$2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
start up "$WORK/up.Fulcrumfile"
start a "$WORK/a.Fulcrumfile"

for p in "$UP_PORT" "$A_PORT" "$B_PORT" "$C_PORT" "$D_PORT" "$E_PORT"; do
  wait_port "$p" || {
    echo "LOG TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "六个监听都起来了（含 HTTPS 那一个）"

# ★ ★ 裝載時就該把紀錄檔案建出來 —— 那是「打不開要在裝載時紅」的正面一半。
if [ -f "$LOGFILE" ]; then
  ok "★★ 装载时就把日志文件打开了（还没有任何请求）"
else
  fail "★★ 装载完成而日志文件不存在 —— 那意味着它要等第一个请求才开，打不开时就晚了"
fi
if [ "$(lines)" = "0" ]; then
  ok "而它此刻是空的（0 行）"
else
  fail "日志文件里已经有 $(lines) 行，可还没有任何请求"
fi

# ── [2/6] 一條請求 ⇒ 正好一行，且每個欄位都按契約 ───────────────────────────
echo "=== [2/9] 一条请求 ⇒ 正好一行，字段逐个对 ==="
CODE=$(req "$A_PORT" a.example "/rw/thing?k=v")
[ "$CODE" = "200" ] || fail "预热请求没回 200（$CODE）"
sleep 0.2

if [ "$(lines)" = "1" ]; then
  ok "★★★ 一条请求正好记了**一行**（不是 0 行，也不是 2 行）"
else
  fail "★★★ 一条请求记了 $(lines) 行 —— 契约是一条请求一行"
fi

check() {
  local what=$1 want=$2 got
  got=$(field "$what")
  if [ "$got" = "$want" ]; then
    ok "字段 $what = $want"
  else
    fail "字段 $what：期望「$want」实际「$got」"
  fi
}
check proto "HTTP/1.1"
check method "GET"
check host "a.example"
check status "200"
check outcome "reverse_proxy"
# ★ `site` 是站台的**名字** = 設定裡第一個位址的原文（裝載紀錄用的也是它）——
#   而不是主機名。⚠ 我第一次按主機名寫，實測告訴我不是。
check site "http://a.example:$A_PORT"
check level "info"
check remote_ip "127.0.0.1"

# ★ ★ ★ `uri` 取的是**原始**請求目標，`rewrite` **之前**。
#   ⚠ 這條最容易寫反：資料面手裡一直有 `effective_path`，隨手取它就編得過、
#   跑得通、紀錄也「有值」—— 而它說出的是一個**用戶端從沒請求過**的位址。
check uri "/rw/thing?k=v"

# 上游那一格：★ 記的是**真的連上的那一個**。
UP=$(field upstream)
if [ "$UP" = "127.0.0.1:$UP_PORT" ]; then
  ok "字段 upstream = $UP（真的连上的那一个）"
else
  fail "字段 upstream：期望「127.0.0.1:$UP_PORT」实际「$UP」"
fi

# 數值那兩格：只斷言「是數字且合理」，不釘死具體值。
SIZE=$(field resp_size)
DUR=$(field duration_ms)
if [ "$SIZE" = "13" ]; then
  ok "字段 resp_size = 13（from-upstream 的字节数）"
else
  fail "字段 resp_size：期望 13，实际「$SIZE」"
fi
if python3 -c "import sys; sys.exit(0 if 0 <= float('$DUR') < 5000 else 1)" 2>/dev/null; then
  ok "字段 duration_ms = $DUR（是个合理的毫秒数）"
else
  fail "字段 duration_ms 不像一个毫秒数：「$DUR」"
fi

# ts：★ 斷言的是**格式**與**新鮮度**，不是某個字面值。
TS=$(field ts)
if python3 - "$TS" <<'PY'
import datetime, sys
s = sys.argv[1]
t = datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%S.%f%z")
now = datetime.datetime.now(datetime.timezone.utc)
# ⚠ 只判「是 UTC、带三位毫秒、而且就在刚才」——
#   钉死一个时刻的判据要么恒红要么什么都不守。
sys.exit(0 if s.endswith("Z") and abs((now - t).total_seconds()) < 300 else 1)
PY
then
  ok "字段 ts = $TS（UTC、带毫秒、就在刚才）"
else
  fail "字段 ts 不合契约：「$TS」"
fi

# ★ ★ 扁平：一個嵌套物件/陣列都不許有。
if python3 -c '
import json, sys
o = json.loads(open("'"$LOGFILE"'", encoding="utf-8").read().strip().split("\n")[-1])
bad = [k for k, v in o.items() if isinstance(v, (dict, list))]
sys.exit(1 if bad else 0)
'; then
  ok "★★ 整行是**扁平**的（没有嵌套对象或数组）"
else
  fail "★★ 日志行里出现了嵌套结构 —— G113 取「扁平」的全部意义就没了"
fi

# ── [3/6] outcome 是個閉集，逐個走一遍 ──────────────────────────────────────
echo "=== [3/9] outcome 的每一种都真的会出现 ==="
outcome_is() {
  local path=$1 want=$2 got
  req "$A_PORT" a.example "$path" > /dev/null
  sleep 0.15
  got=$(field outcome)
  if [ "$got" = "$want" ]; then
    ok "$path ⇒ outcome = $want"
  else
    fail "$path ⇒ outcome：期望「$want」实际「$got」"
  fi
}
outcome_is "/" respond
outcome_is "/go" redir
outcome_is "/files/x" file_server
outcome_is "/up" reverse_proxy
# ★ ★ ★ 這兩條放在一起，是因為它們分得開**兩件很容易被合起來看的事**：
#   `outcome` 說的是「這一次是**怎麼**終結的」，`status` 說的是「回了幾號碼」。
#   ⚠ 我第一次把 `respond 503` 的 outcome 寫成了 `error`，而實測糾正了它：
#     一條**使用者顯式寫的** `respond 503` 就是 `respond` —— 它沒走錯誤頁那條路。
outcome_is "/boom" respond
# 而真的走錯誤頁那條路的是**連不上上游**：
outcome_is "/dead" error

# ── [4/6] 閾值：`level warn` 的站台只記 4xx/5xx ─────────────────────────────
echo "=== [4/9] level 是阈值（而不是这一行的级别）==="
BEFORE=$(lines)
req "$D_PORT" d.example "/" > /dev/null
sleep 0.15
if [ "$(lines)" = "$BEFORE" ]; then
  ok "★★ level warn 的站点：200 **一行都不写**"
else
  fail "★★ level warn 的站点写了一条 200（行数 $BEFORE → $(lines)）"
fi
# ⚠ ⚠ **重新取一次基數**，而不是接著上面那個用。
#   反證時實測過：上一條若因缺陷多寫了一行，這一條會紅在
#   「沒記下 404」上 —— 而那個診斷**與事實相反**（它記下了，只是基數被帶偏了）。
#   ★ 一條判據的基數取自另一條判據的前置狀態時，前一條的失敗會偽裝成後一條的失敗。
BEFORE=$(lines)
req "$D_PORT" d.example "/bad" > /dev/null
sleep 0.15
if [ "$(lines)" = "$((BEFORE + 1))" ]; then
  ok "★★ 同一个站点：404 **记下来了**（阈值真的在判，不是恒不写）"
  check level "warn"
  check site "http://d.example:$D_PORT"
else
  fail "★★ level warn 的站点没记下 404（行数 $BEFORE → $(lines)）"
fi

# ── [5/6] 反向 ──────────────────────────────────────────────────────────────
echo "=== [5/9] 反向 ==="
# ⑦ 沒配 `log` 的站台：一行都不寫。
#    ⚠ 少了這一條，一個「不看設定、見請求就寫」的實作在上面全綠。
BEFORE=$(lines)
req "$C_PORT" c.example "/" > /dev/null
sleep 0.15
if [ "$(lines)" = "$BEFORE" ]; then
  ok "★★★ 没配 log 的站点：一行都不写"
else
  fail "★★★ 没配 log 的站点也被记了（行数 $BEFORE → $(lines)）"
fi

# ⑧ `output stderr` 的站台：寫進了**處理程序的 stderr**，而不是那個檔案。
BEFORE=$(lines)
req "$B_PORT" b.example "/" > /dev/null
sleep 0.15
if [ "$(lines)" = "$BEFORE" ]; then
  ok "★★ output stderr 的站点没写进文件（两个出口真的分得开）"
else
  fail "★★ output stderr 的站点写进了文件（行数 $BEFORE → $(lines)）"
fi
if grep -q '"host":"b.example"' "$WORK/a.log"; then
  ok "★★ 而它确实出现在进程的 stderr 里"
else
  fail "★★ output stderr 的站点在 stderr 里也找不到 —— 那它到底写去哪了"
fi

# ── [6/6] 打不開的紀錄路徑必須在**裝載時**紅 ────────────────────────────────
echo "=== [6/9] 打不开的日志路径 ⇒ 装载时就红 ==="
#
# ★ ★ ★ 一個用來「出了事你能知道」的東西，自己壞掉時必須有人知道。
#   ⚠ 若它降級成「起來了、服務正常、紀錄悄悄沒有」，那是這一整批要防的失效本身。
cat > "$WORK/bad.Fulcrumfile" <<CONF
http://a.example:$A_PORT {
    log {
        output file /proc/这个目录不存在/x.json
    }
    respond 200 "x"
}
CONF
if "$BIN" serve "$WORK/bad.Fulcrumfile" --bind-host "$HOST" \
    --pid-file "$WORK/bad.pid" --upgrade-sock "$WORK/bad.sock" \
    > "$WORK/bad.log" 2>&1; then
  fail "★★★ 日志路径打不开而进程照样起来了 —— 那正是「起来了但没日志」那种失效"
else
  if grep -q "访问日志装载失败" "$WORK/bad.log"; then
    ok "★★★ 日志路径打不开 ⇒ 起不来，而且说出了为什么"
  else
    fail "★★★ 起不来了，但没说是日志的事：$(tail -3 "$WORK/bad.log" | tr '\n' ' ')"
  fi
fi

# 而 `POST /load` 那條路要**原子地**拒絕，舊設定一個位元組都不動。
"$BIN" compile "$WORK/bad.Fulcrumfile" > "$WORK/bad.json" 2>/dev/null || {
  echo "LOG TESTS FAILED: compile 生成不出那份坏配置" >&2
  exit 1
}
CODE=$(curl -s -o "$WORK/admin.out" -w '%{http_code}' \
  --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$(cat "$WORK/bad.json")" \
  "http://localhost/load?overrides=clear" 2>/dev/null || echo 000)
if [ "$CODE" = "400" ]; then
  ok "★★ 管理面：日志路径打不开的配置被拒（400）"
else
  fail "★★ 管理面：期望 400，实际 $CODE（$(head -1 "$WORK/admin.out" 2>/dev/null)）"
fi
CODE=$(req "$A_PORT" a.example "/")
if [ "$CODE" = "200" ]; then
  ok "★★ 被拒之后旧配置还在服务（原子）"
else
  fail "★★ 被拒之后服务坏了（$CODE）—— 那次拒绝不是原子的"
fi

# ── [7/9] 白名單頭：只記寫進去的那幾個，一個不多 ────────────────────────────
echo "=== [7/9] 白名单头（默认一个都不记）==="
#
# ★ ★ ★ 這一格裡最值錢的是**反向**那兩條：一條帶 Authorization 與 Cookie 的請求
#   打過去，整行紀錄裡**一個位元組都不許有它們**。
#   ⚠ 而其中一條必須按「這一行的原始位元組裡出現過沒有」來判，不能只按鍵取值 ——
#     一個把憑證寫進了**別的鍵**的實作，按鍵取值原理上看不見。
curl -sS -o /dev/null --max-time 5 \
  -H "Host: a.example" \
  -H "User-Agent: probe/1" \
  -H "X-Trace-Id: aaa" \
  -H "X-Trace-Id: bbb" \
  -H "Authorization: Bearer 这串凭据不该出现" \
  -H "Cookie: sid=这串也不该出现" \
  "http://$HOST:$A_PORT/" 2>/dev/null || fail "白名单那条请求发不出去"
sleep 0.2

check req_hdr_user_agent "probe/1"
# ⚠ 多值頭按 `, ` 連接（RFC 9110 §5.3）。★ 判據釘的是**連起來那一串**而不是「有值」——
#   一個只取第一個值的實作在「有值」下是綠的。
check req_hdr_x_trace_id "aaa, bbb"
# 回應頭那一半：一個協定自己的頭 + 一個我們在執行鏈上加的。
# ★ 後者證明取的是**最終**回應頭（`header` 那一步之後），不是上游給的那一份。
check resp_hdr_x_cell "log3"
check resp_hdr_content_length "4"

KEYS=$(keys)
for bad in req_hdr_authorization req_hdr_cookie; do
  case " $KEYS " in
    *" $bad "*) fail "★★★ 日志里出现了 $bad —— 那正是编译期拒绝要防的东西" ;;
    *) ok "★★★ 白名单外的 $bad 没有出现" ;;
  esac
done
if line_has "Bearer" || line_has "sid="; then
  fail "★★★ 凭据的**值**出现在了这一行的原始字节里：$(tail -1 "$LOGFILE")"
else
  ok "★★★ 那两个凭据在整行的原始字节里一个都找不到"
fi

# ★ ★ 預設那一半只能**反向**證：站台 D 配了 log 但**沒寫** headers。
#   ⚠ 少了這一條，一個「把整個頭映射倒進紀錄」的實作在上面全綠 ——
#     而那恰恰是 G114 那半條理由要防的東西。
BEFORE=$(lines)
curl -sS -o /dev/null --max-time 5 -H "Host: d.example" -H "User-Agent: probe/2" \
  "http://$HOST:$D_PORT/bad" 2>/dev/null || true
sleep 0.15
if [ "$(lines)" = "$((BEFORE + 1))" ]; then
  KEYS=$(keys)
  case "$KEYS" in
    *req_hdr_* | *resp_hdr_*) fail "★★★ 没配 headers 的站点也记了头：$KEYS" ;;
    *) ok "★★★ 没配 headers 的站点：一个 req_hdr_/resp_hdr_ 都没有（默认就是不记）" ;;
  esac
else
  fail "站点 D 那条 404 没记下来（行数 $BEFORE → $(lines)），这一条的对照物没成立"
fi

# ── [8/9] TLS 四格 ──────────────────────────────────────────────────────────
echo "=== [8/9] TLS 四格（tls_version / tls_cipher / tls_sni / tls_alpn），h1/h2 与 h3 各一遍 ==="
#
# ⚠ `--resolve` 而不是改 hosts：SNI 必須真的是 `a.example`，
#   而直連 `https://127.0.0.1:PORT` 發出去的 SNI 是空的（IP 不做 SNI）。
tls_req() {
  local path=$1
  shift
  curl -sS -o /dev/null -k --max-time 5 \
    --resolve "a.example:$E_PORT:$HOST" "$@" "https://a.example:$E_PORT$path" 2>/dev/null
}

tls_req / || fail "HTTPS 请求发不出去"
sleep 0.2
# ⚠ 站台名是設定裡**第一個位址的原文** —— HTTPS 那個塊寫的就是 `a.example:9906`
#   （沒有 scheme 前綴），所以這裡**沒有** `https://`。★ 我第一次照著 A 站台的樣子
#   寫成 `https://…`，實測糾正了它 —— 與批 L 第 ② 步那次「site 不是主機名」同族。
check site "a.example:$E_PORT"
# curl 預設 ALPN 裡帶 h2 ⇒ 協商出 h2，而 `proto` 也該跟著是 HTTP/2.0。
check proto "HTTP/2.0"
check tls_alpn "h2"
# ★ ★ SNI 與 `host` 是**兩件事**：這裡它們碰巧相等，而下面那條 `--no-alpn` 的
#   反向判據證明這一格不是從 `host` 抄來的（那條裡 host 還在，tls_alpn 沒了）。
check tls_sni "a.example"
VER=$(field tls_version)
case "$VER" in
  TLSv1.3 | TLSv1.2) ok "字段 tls_version = $VER" ;;
  *) fail "字段 tls_version 不像一个 TLS 版本：「$VER」" ;;
esac
CIPHER=$(field tls_cipher)
case "$CIPHER" in
  "" | "<缺>") fail "字段 tls_cipher 没有值 —— 一条握完手的连接必然协商出了套件" ;;
  *) ok "字段 tls_cipher = $CIPHER" ;;
esac

# 同一個連接埠，強制 http/1.1 ⇒ 兩格一起變。
tls_req / --http1.1 || fail "HTTPS（http/1.1）请求发不出去"
sleep 0.2
check proto "HTTP/1.1"
check tls_alpn "http/1.1"

# ★ ★ ★ 反向：用戶端**不發 ALPN**。⇒ tls_alpn 那一格必須消失，而其餘三格還在。
#   ⚠ 少了這一條，一個「按 proto 反推 ALPN」的實作在上面兩條裡完全綠 ——
#     而它說出的是一個**從沒協商過**的值。
tls_req / --no-alpn --http1.1 || fail "HTTPS（不发 ALPN）请求发不出去"
sleep 0.2
KEYS=$(keys)
case " $KEYS " in
  *" tls_alpn "*) fail "★★★ 客户端没发 ALPN，日志里却有 tls_alpn —— 那一格不是量出来的" ;;
  *) ok "★★★ 客户端没发 ALPN ⇒ tls_alpn 不出现" ;;
esac
case " $KEYS " in
  *" tls_version "*) ok "★★ 而 tls_version 还在 —— 这条连接仍然是 TLS" ;;
  *) fail "★★ tls_version 也没了 —— 那说明上一条其实是「整块 TLS 信息都没取到」" ;;
esac

# ★ ★ 反向：明文那條路上四格一個都不出現。
req "$A_PORT" a.example "/" > /dev/null
sleep 0.15
KEYS=$(keys)
for k in tls_version tls_cipher tls_sni tls_alpn; do
  case " $KEYS " in
    *" $k "*) fail "★★★ 明文请求的日志里出现了 $k" ;;
    *) ok "★★★ 明文请求：$k 不出现" ;;
  esac
done

# ── ★ ★ ★ h3 那一半（**D27 結案，G128**）────────────────────────
#
# 站台 E 是 TLS 站台，而 G110 讓**有 TLS 的連接埠自動在同一個連接埠號上聽 UDP**
# ⇒ 這裡不用另起服務，同一個 9906 上就有 h3。
#
# ⚠ ⚠ **`--http3-only` 不許省成 `--http3`**：後者在 QUIC 不通時會**回落到 TCP**，
#   於是下面幾條會在「h3 根本沒起來」時照樣全綠 —— 那是本儲存庫在
#   `tests/h3/run.sh` 裡白紙黑字記過的一條。
#
# ★ 先自證這把尺子量得了東西：對著一個**沒有 UDP 監聽**的連接埠（站台 C 是純 HTTP）
#   `--http3-only` 必須失敗。⚠ 少了它，一個「curl 沒編進 QUIC 後端」的鏡像會讓
#   下面每一條都變成空轉。
if curl -sS --http3-only --max-time 5 -o /dev/null \
    "https://$HOST:$C_PORT/" >/dev/null 2>&1; then
  fail "★ 对着一个没有 UDP 监听的端口 --http3-only 竟然成功了 —— 这把尺子量不了东西"
else
  ok "★ 空 UDP 端口上 --http3-only 如期失败 ⇒ 它真的在走 QUIC，不是悄悄回落"
fi

if curl -sS -o /dev/null -k --max-time 5 --http3-only \
    --resolve "a.example:$E_PORT:$HOST" "https://a.example:$E_PORT/" 2>/dev/null; then
  sleep 0.3
  # ★ ★ ★ 這四條一起說明一件事：**h3 與 h1/h2 在訪問紀錄這一層走的是同一段程式碼**
  #   （同一個 `SslDigest`），差別只在誰造了那份 digest。
  check proto "HTTP/3.0"
  check tls_sni "a.example"
  check tls_alpn "h3"
  # RFC 9001 §4.2：QUIC 只能用 TLS 1.3。★ 這是規範，不是我們猜的。
  check tls_version "TLSv1.3"
  # ⚠ ⚠ 而 `tls_cipher` 在 h3 上**有意沒有** —— quiche 的 `Handshake::cipher()`
  #   鎖在私有 `mod tls` 裡，公開 API 一個 TLS 出口都沒有。
  # ★ ★ 判據釘的是「**它不出現**」而不是「隨便什麼值」：
  #   一個編出來的套件名讀起來與真的一模一樣，而它是假的。
  KEYS=$(keys)
  case " $KEYS " in
    *" tls_cipher "*) fail "★★★ h3 的日志行里出现了 tls_cipher —— quiche 给不出它，那这个值是编的" ;;
    *) ok "★★★ h3 上 tls_cipher 有意不出现（quiche 拿不到，D27 结案时写进了契约）" ;;
  esac
else
  fail "★★★ h3 请求发不出去 —— 有 TLS 的端口应当自动在同端口听 UDP（G110）"
fi

# ⚠ ★ **這裡曾經有一條探針判據，它為什麼走了要留一句。**
#   拿 SNI/ALPN 一度要給監聽器掛 `TlsAccept` 回呼，而上游 `start_accept()` **無條件**
#   裝一個恆回 -1 的 `cert_cb`，於是每條 TLS 連接都要多走一趟「掛起 → 回呼 → resume_accept」。
#   ✅ D27 + D28 一起結案：那份回呼整條刪掉，SNI/ALPN 改由 **fork 改動 14** 直接記進
#   `SslDigest`，開銷歸零 ⇒ 探針沒有物件了，這條判據跟著走。
#
#   ★ **而它守的那件事沒有消失，只是換了個更強的守法**：
#   「別再去掛那份回呼」現在由 `crates/fulcrum/tests/tls_digest_gate.rs` 門 3 判 ——
#   ⚠ 一條結構門比一條「紀錄裡有沒有那句話」的 grep 穩得多。
#
#   > ★ ★ 可帶走的一條：**一條為「撞一個推理」而寫的判據，撞完之後要麼升級成結構門，
#   > 要麼跟著那個推理一起退場** —— 留著它只會在下一次重構時紅得莫名其妙。

# ── [9/9] 敏感頭：兩條路各攔一次 ────────────────────────────────────────────
echo "=== [9/9] Authorization / Cookie / Set-Cookie / Proxy-Authorization 写进白名单 ⇒ 装不上 ==="
#
# ★ ★ **兩條路都要攔**：DSL 那條走 `fulcrum validate`，結構化那條走 POST /load。
#   ⚠ 只攔第一條的話，一份手寫的結構化設定（G11 的公開入口）就能把憑證放進紀錄，
#     而那條路上編譯期診斷一次都不會跑。
cat > "$WORK/leak.Fulcrumfile" <<CONF
http://a.example:$A_PORT {
    log {
        output stderr
        headers Cookie
    }
    respond 200 "x"
}
CONF
if "$BIN" validate "$WORK/leak.Fulcrumfile" > "$WORK/leak.out" 2>&1; then
  fail "★★★ 白名单里写 Cookie 居然通过了 validate"
else
  if grep -q "FUL-DSL-0036" "$WORK/leak.out"; then
    ok "★★★ DSL 那条路：白名单里写 Cookie ⇒ FUL-DSL-0036，配置装不上"
  else
    fail "★★★ validate 红了，但不是因为敏感头：$(head -3 "$WORK/leak.out" | tr '\n' ' ')"
  fi
fi

# 結構化那條路：拿一份**合法**的設定，只把 headers 那一格改掉再喂進去。
# ★ 這樣改的意義是：除了這一格之外整份設定都是好的 ⇒ 400 只可能是它。
"$BIN" compile "$WORK/a.Fulcrumfile" > "$WORK/good.json" 2>/dev/null || {
  echo "LOG TESTS FAILED: compile 生成不出那份好配置" >&2
  exit 1
}
python3 - "$WORK/good.json" "$WORK/leak.json" <<'PY'
import json, sys

cfg = json.load(open(sys.argv[1], encoding="utf-8"))
patched = 0
for s in cfg["sites"]:
    if s.get("log"):
        s["log"]["headers"] = ["Cookie"]
        patched += 1
        break
# ⚠ 自证：改不到东西的话，下面那条 400 判据会变成空转 —— 那比红更坏。
assert patched == 1, "没找到带 log 的站点，这条判据不可采信"
json.dump(cfg, open(sys.argv[2], "w", encoding="utf-8"))
PY
CODE=$(curl -s -o "$WORK/leak.admin" -w '%{http_code}' \
  --unix-socket "$ADMIN_SOCK" -X POST --data-binary "@$WORK/leak.json" \
  "http://localhost/load?overrides=clear" 2>/dev/null || echo 000)
if [ "$CODE" = "400" ]; then
  ok "★★★ 结构化那条路（POST /load）：白名单里写 Cookie ⇒ 400，整份不生效"
else
  fail "★★★ 结构化那条路期望 400，实际 $CODE（$(head -1 "$WORK/leak.admin" 2>/dev/null)）"
fi
CODE=$(req "$A_PORT" a.example "/")
if [ "$CODE" = "200" ]; then
  ok "★★ 被拒之后旧配置还在服务（原子）"
else
  fail "★★ 被拒之后服务坏了（$CODE）—— 那次拒绝不是原子的"
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "LOG TESTS FAILED：$FAILS 条断言没过。" >&2
  echo "--- 被测实例日志 ---" >&2
  tail -30 "$WORK/a.log" >&2 || true
  echo "--- 访问日志 ---" >&2
  tail -10 "$LOGFILE" >&2 || true
  exit 1
fi
echo "LOG TESTS PASSED —— 结构化访问日志真的在跑（一条请求一行 · 字段按契约 · outcome 闭集 · 阈值 · 两个出口 · 白名单头 · TLS 四格 · 敏感头两条路都拦 · 反向若干）。"
