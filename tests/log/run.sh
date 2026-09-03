#!/usr/bin/env bash
# 结构化访问日志端到端（**M2 批 L 第 ② + ③ 步**；D7 由 G113 + G114 结案）。
#
# ★ ★ 为什么它是**独立一格**：别的场景验的都是「响应对不对」，而这一格验的是
#   **响应之外的那一行** —— 它写在别的地方、有自己的格式契约、有自己的开关。
#   ⚠ 混进 `tests/serve/run.sh` 的话，一条日志断言红了，读日志的人会先去看路由。
#
# ★ ★ ★ **判据挂在那一行的内容上，不是挂在「文件非空」上。**
#   契约（`docs/architecture/observability.md`）定的是**字段清单**，
#   而「有输出」与「输出对」之间隔着整个字段表。
#
# 端口（★ 与其余场景全都错开，见 AGENTS.md 那张端口表）：
#   9900 站点 A —— `log { output file … }`，本格主要在它上面量
#   9901 站点 B —— `log { output stderr }`
#   9902 站点 C —— **没配 `log`**（反向判据要它）
#   9903 站点 D —— `log { level warn }`（阈值那一格）★ 它同时是「**没配 headers**」的对照物
#   9904 上游（**另一个实例**，免得自环警告污染装载日志）
#   9905 **永远没人在听**（「连不上上游 ⇒ outcome=error」那条要它）
#   9906 站点 E —— **HTTPS**（批 L 第 ③ 步的 TLS 四格全在它上面量）
#          ★ 它同时是 **h3** 那一半的落点：G110 让有 TLS 的端口**自动在同一个端口号上听 UDP**
#          ⇒ 不用另起服务，同一个 9906 上就有 HTTP/3（D27 结案）

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
# ★ 永远没人在听 —— 「连不上上游 ⇒ 错误页」那条判据要它。
DEAD_PORT=${DEAD_PORT:-9905}
# ★ HTTPS 那一格（批 L 第 ③ 步）：TLS 四格只有在真的握过手之后才有值。
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

# 取日志文件最后一行的某个字段。★ 用 python3 而不是 grep：
# ⚠ 一个 `grep '"status":200'` 会在字段顺序、空格、转义任一处变化时**静默漏判**，
#   而那正是「判据看起来在守契约、其实在守一个字符串」的形状。
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

# 这一行里到底有哪些键。★ 给「一个 `req_hdr_*` 都不许有」那几条反向判据用。
keys() {
  python3 -c '
import json, sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
print(" ".join(json.loads(line).keys()))
' "$LOGFILE"
}

# 这一行的**原始字节**里有没有出现某个串。
# ★ ★ 与 `field` 不同：那个按键取值，而这一条问的是「它在这一行里出现过没有」——
#   ⚠ 一个把 `Authorization` 写进了**某个别的键**的实现，按键取值是看不见的。
line_has() {
  python3 -c '
import sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
sys.exit(0 if sys.argv[2].lower() in line.lower() else 1)
' "$LOGFILE" "$1"
}

# ── [0/6] 基线 ──────────────────────────────────────────────────────────────
#
# ★ ★ 两条自证：端口没被占（否则测的是别人的服务）；**日志文件此刻不存在**
#   （否则「多了一行」这种判据分不出「刚写的」与「本来就有的」）。
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

# ── 配置 ────────────────────────────────────────────────────────────────────
# ⚠ 上游写成**不带主机名**的 `:PORT`：转发过去时 Host 还是 `a.example`，
#   而一个带主机名的上游站点会对它回 **421** —— 第一次跑正是栽在这里，
#   ★ 而那条 421 一路顺着日志显形（status/level/resp_size 三格同时红），
#   说明这一格量的确实是真流量，不是一个造出来的形状。
cat > "$WORK/up.Fulcrumfile" <<CONF
:$UP_PORT {
    respond 200 "from-upstream"
}
CONF

# ── 自签证书（HTTPS 那一格要，批 L 第 ③ 步）───────────────────────────────
#
# ⚠ SAN 必须有 `a.example`：枢衡按**证书自己的 SAN** 决定这张证书装在哪些 SNI 上。
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

# ── [1/6] 起服务 ────────────────────────────────────────────────────────────
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

# ★ ★ 装载时就该把日志文件建出来 —— 那是「打不开要在装载时红」的正面一半。
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

# ── [2/6] 一条请求 ⇒ 正好一行，且每个字段都按契约 ───────────────────────────
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
# ★ `site` 是站点的**名字** = 配置里第一个地址的原文（装载日志用的也是它）——
#   而不是主机名。⚠ 我第一次按主机名写，实测告诉我不是。
check site "http://a.example:$A_PORT"
check level "info"
check remote_ip "127.0.0.1"

# ★ ★ ★ `uri` 取的是**原始**请求目标，`rewrite` **之前**。
#   ⚠ 这条最容易写反：数据面手里一直有 `effective_path`，随手取它就编得过、
#   跑得通、日志也「有值」—— 而它说出的是一个**客户端从没请求过**的地址。
check uri "/rw/thing?k=v"

# 上游那一格：★ 记的是**真的连上的那一个**。
UP=$(field upstream)
if [ "$UP" = "127.0.0.1:$UP_PORT" ]; then
  ok "字段 upstream = $UP（真的连上的那一个）"
else
  fail "字段 upstream：期望「127.0.0.1:$UP_PORT」实际「$UP」"
fi

# 数值那两格：只断言「是数字且合理」，不钉死具体值。
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

# ts：★ 断言的是**格式**与**新鲜度**，不是某个字面量。
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

# ★ ★ 扁平：一个嵌套对象/数组都不许有。
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

# ── [3/6] outcome 是个闭集，逐个走一遍 ──────────────────────────────────────
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
# ★ ★ ★ 这两条放在一起，是因为它们分得开**两件很容易被合起来看的事**：
#   `outcome` 说的是「这一次是**怎么**终结的」，`status` 说的是「回了几号码」。
#   ⚠ 我第一次把 `respond 503` 的 outcome 写成了 `error`，而实测纠正了它：
#     一条**用户显式写的** `respond 503` 就是 `respond` —— 它没走错误页那条路。
outcome_is "/boom" respond
# 而真的走错误页那条路的是**连不上上游**：
outcome_is "/dead" error

# ── [4/6] 阈值：`level warn` 的站点只记 4xx/5xx ─────────────────────────────
echo "=== [4/9] level 是阈值（而不是这一行的级别）==="
BEFORE=$(lines)
req "$D_PORT" d.example "/" > /dev/null
sleep 0.15
if [ "$(lines)" = "$BEFORE" ]; then
  ok "★★ level warn 的站点：200 **一行都不写**"
else
  fail "★★ level warn 的站点写了一条 200（行数 $BEFORE → $(lines)）"
fi
# ⚠ ⚠ **重新取一次基数**，而不是接着上面那个用。
#   反证时实测过：上一条若因缺陷多写了一行，这一条会红在
#   「没记下 404」上 —— 而那个诊断**与事实相反**（它记下了，只是基数被带偏了）。
#   ★ 一条判据的基数取自另一条判据的前置状态时，前一条的失败会伪装成后一条的失败。
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
# ⑦ 没配 `log` 的站点：一行都不写。
#    ⚠ 少了这一条，一个「不看配置、见请求就写」的实现在上面全绿。
BEFORE=$(lines)
req "$C_PORT" c.example "/" > /dev/null
sleep 0.15
if [ "$(lines)" = "$BEFORE" ]; then
  ok "★★★ 没配 log 的站点：一行都不写"
else
  fail "★★★ 没配 log 的站点也被记了（行数 $BEFORE → $(lines)）"
fi

# ⑧ `output stderr` 的站点：写进了**进程的 stderr**，而不是那个文件。
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

# ── [6/6] 打不开的日志路径必须在**装载时**红 ────────────────────────────────
echo "=== [6/9] 打不开的日志路径 ⇒ 装载时就红 ==="
#
# ★ ★ ★ 一个用来「出了事你能知道」的东西，自己坏掉时必须有人知道。
#   ⚠ 若它降级成「起来了、服务正常、日志悄悄没有」，那是这一整批要防的失效本身。
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

# 而 `POST /load` 那条路要**原子地**拒绝，旧配置一个字节都不动。
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

# ── [7/9] 白名单头：只记写进去的那几个，一个不多 ────────────────────────────
echo "=== [7/9] 白名单头（默认一个都不记）==="
#
# ★ ★ ★ 这一格里最值钱的是**反向**那两条：一条带 Authorization 与 Cookie 的请求
#   打过去，整行日志里**一个字节都不许有它们**。
#   ⚠ 而其中一条必须按「这一行的原始字节里出现过没有」来判，不能只按键取值 ——
#     一个把凭据写进了**别的键**的实现，按键取值原理上看不见。
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
# ⚠ 多值头按 `, ` 连接（RFC 9110 §5.3）。★ 判据钉的是**连起来那一串**而不是「有值」——
#   一个只取第一个值的实现在「有值」下是绿的。
check req_hdr_x_trace_id "aaa, bbb"
# 响应头那一半：一个协议自己的头 + 一个我们在执行链上加的。
# ★ 后者证明取的是**最终**响应头（`header` 那一步之后），不是上游给的那一份。
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

# ★ ★ 默认那一半只能**反向**证：站点 D 配了 log 但**没写** headers。
#   ⚠ 少了这一条，一个「把整个头映射倒进日志」的实现在上面全绿 ——
#     而那恰恰是 G114 那半条理由要防的东西。
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
# ⚠ `--resolve` 而不是改 hosts：SNI 必须真的是 `a.example`，
#   而直连 `https://127.0.0.1:PORT` 发出去的 SNI 是空的（IP 不做 SNI）。
tls_req() {
  local path=$1
  shift
  curl -sS -o /dev/null -k --max-time 5 \
    --resolve "a.example:$E_PORT:$HOST" "$@" "https://a.example:$E_PORT$path" 2>/dev/null
}

tls_req / || fail "HTTPS 请求发不出去"
sleep 0.2
# ⚠ 站点名是配置里**第一个地址的原文** —— HTTPS 那个块写的就是 `a.example:9906`
#   （没有 scheme 前缀），所以这里**没有** `https://`。★ 我第一次照着 A 站点的样子
#   写成 `https://…`，实测纠正了它 —— 与批 L 第 ② 步那次「site 不是主机名」同族。
check site "a.example:$E_PORT"
# curl 默认 ALPN 里带 h2 ⇒ 协商出 h2，而 `proto` 也该跟着是 HTTP/2.0。
check proto "HTTP/2.0"
check tls_alpn "h2"
# ★ ★ SNI 与 `host` 是**两件事**：这里它们碰巧相等，而下面那条 `--no-alpn` 的
#   反向判据证明这一格不是从 `host` 抄来的（那条里 host 还在，tls_alpn 没了）。
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

# 同一个端口，强制 http/1.1 ⇒ 两格一起变。
tls_req / --http1.1 || fail "HTTPS（http/1.1）请求发不出去"
sleep 0.2
check proto "HTTP/1.1"
check tls_alpn "http/1.1"

# ★ ★ ★ 反向：客户端**不发 ALPN**。⇒ tls_alpn 那一格必须消失，而其余三格还在。
#   ⚠ 少了这一条，一个「按 proto 反推 ALPN」的实现在上面两条里完全绿 ——
#     而它说出的是一个**从没协商过**的值。
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

# ★ ★ 反向：明文那条路上四格一个都不出现。
req "$A_PORT" a.example "/" > /dev/null
sleep 0.15
KEYS=$(keys)
for k in tls_version tls_cipher tls_sni tls_alpn; do
  case " $KEYS " in
    *" $k "*) fail "★★★ 明文请求的日志里出现了 $k" ;;
    *) ok "★★★ 明文请求：$k 不出现" ;;
  esac
done

# ── ★ ★ ★ h3 那一半（**D27 结案，G128**）────────────────────────
#
# 站点 E 是 TLS 站点，而 G110 让**有 TLS 的端口自动在同一个端口号上听 UDP**
# ⇒ 这里不用另起服务，同一个 9906 上就有 h3。
#
# ⚠ ⚠ **`--http3-only` 不许省成 `--http3`**：后者在 QUIC 不通时会**回落到 TCP**，
#   于是下面几条会在「h3 根本没起来」时照样全绿 —— 那是本仓库在
#   `tests/h3/run.sh` 里白纸黑字记过的一条。
#
# ★ 先自证这把尺子量得了东西：对着一个**没有 UDP 监听**的端口（站点 C 是纯 HTTP）
#   `--http3-only` 必须失败。⚠ 少了它，一个「curl 没编进 QUIC 后端」的镜像会让
#   下面每一条都变成空转。
if curl -sS --http3-only --max-time 5 -o /dev/null \
    "https://$HOST:$C_PORT/" >/dev/null 2>&1; then
  fail "★ 对着一个没有 UDP 监听的端口 --http3-only 竟然成功了 —— 这把尺子量不了东西"
else
  ok "★ 空 UDP 端口上 --http3-only 如期失败 ⇒ 它真的在走 QUIC，不是悄悄回落"
fi

if curl -sS -o /dev/null -k --max-time 5 --http3-only \
    --resolve "a.example:$E_PORT:$HOST" "https://a.example:$E_PORT/" 2>/dev/null; then
  sleep 0.3
  # ★ ★ ★ 这四条一起说明一件事：**h3 与 h1/h2 在访问日志这一层走的是同一段代码**
  #   （同一个 `SslDigest`），差别只在谁造了那份 digest。
  check proto "HTTP/3.0"
  check tls_sni "a.example"
  check tls_alpn "h3"
  # RFC 9001 §4.2：QUIC 只能用 TLS 1.3。★ 这是规范，不是我们猜的。
  check tls_version "TLSv1.3"
  # ⚠ ⚠ 而 `tls_cipher` 在 h3 上**有意没有** —— quiche 的 `Handshake::cipher()`
  #   锁在私有 `mod tls` 里，公开 API 一个 TLS 出口都没有。
  # ★ ★ 判据钉的是「**它不出现**」而不是「随便什么值」：
  #   一个编出来的套件名读起来与真的一模一样，而它是假的。
  KEYS=$(keys)
  case " $KEYS " in
    *" tls_cipher "*) fail "★★★ h3 的日志行里出现了 tls_cipher —— quiche 给不出它，那这个值是编的" ;;
    *) ok "★★★ h3 上 tls_cipher 有意不出现（quiche 拿不到，D27 结案时写进了契约）" ;;
  esac
else
  fail "★★★ h3 请求发不出去 —— 有 TLS 的端口应当自动在同端口听 UDP（G110）"
fi

# ⚠ ★ **这里曾经有一条探针判据，它为什么走了要留一句。**
#   拿 SNI/ALPN 一度要给监听器挂 `TlsAccept` 回调，而上游 `start_accept()` **无条件**
#   装一个恒回 -1 的 `cert_cb`，于是每条 TLS 连接都要多走一趟「挂起 → 回调 → resume_accept」。
#   ✅ D27 + D28 一起结案：那份回调整条删掉，SNI/ALPN 改由 **fork 改动 14** 直接记进
#   `SslDigest`，开销归零 ⇒ 探针没有对象了，这条判据跟着走。
#
#   ★ **而它守的那件事没有消失，只是换了个更强的守法**：
#   「别再去挂那份回调」现在由 `crates/fulcrum/tests/tls_digest_gate.rs` 门 3 判 ——
#   ⚠ 一条结构门比一条「日志里有没有那句话」的 grep 稳得多。
#
#   > ★ ★ 可带走的一条：**一条为「撞一个推理」而写的判据，撞完之后要么升级成结构门，
#   > 要么跟着那个推理一起退场** —— 留着它只会在下一次重构时红得莫名其妙。

# ── [9/9] 敏感头：两条路各拦一次 ────────────────────────────────────────────
echo "=== [9/9] Authorization / Cookie / Set-Cookie / Proxy-Authorization 写进白名单 ⇒ 装不上 ==="
#
# ★ ★ **两条路都要拦**：DSL 那条走 `fulcrum validate`，结构化那条走 POST /load。
#   ⚠ 只拦第一条的话，一份手写的结构化配置（G11 的公开入口）就能把凭据放进日志，
#     而那条路上编译期诊断一次都不会跑。
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

# 结构化那条路：拿一份**合法**的配置，只把 headers 那一格改掉再喂进去。
# ★ 这样改的意义是：除了这一格之外整份配置都是好的 ⇒ 400 只可能是它。
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
