#!/usr/bin/env bash
# 用例类「反代吞吐」（M3，§8 七类之二）——四家各作反向代理，转发到**同一个常驻源站**。
#
#   bash bench/case/reverse-proxy-throughput.sh <输出目录>
#
# ⛔ **本脚本不判定、不比较、不打印任何「谁更快」。** 它只产出原始数据。
#   判定在 `bench/verdict.sh`，而那一步会先问宿主合不合格。
#
# ── 对齐口径（★ 每一条都会被 bench/README.md 引用，改这里要一起改）─────────────
#
#   ① **一次只起一家**（源站除外，它常驻 —— 见下）。
#   ② **同一个源站、同一个 URL 路径、同一份 payload 字节**，开跑前逐字节核对过。
#   ③ **访问日志四家全关。**
#   ④ **明文 HTTP/1.1 + keep-alive。** TLS 归「TLS 握手」那一类，⛔ 不混进来。
#   ⑤ **worker 数四家一致**（`BENCH_WORKERS`）——★ **源站也吃同一个值**。
#   ⑥ ★ ★ ★ **上游连接复用四家一致。** 四家的**默认**行为不同，而 oha 的 JSON 里
#      **看不出这件事** —— 输出一切正常，只是某一家的数被系统性压低。
#      ⇒ 四家全部显式配置（逐家的理由写在各自的配置文件里），并由本脚本产出的
#      `upstream-conns.txt` 交给 `tests/bench/gate.sh` 的 C20 第 ② 半判着。
#
# ── 源站为什么是枢衡自己的 `respond` ─────────────────────────────────────────
#
# ⚠ 与 `diag/cache-hit-p99.sh` 那个源站**性质完全不同**：那里命中之后源站根本
#   不被访问（测量窗口里的 CPU 成本 ≈ 0），而**这里每一个请求都要穿过它**。
#
# ★ 镜像里能当源站的只有四家自己与 python3（`docker/Dockerfile.bench`）
#   ⇒ 加第五个实现要改镜像，**撞 G36**（对拍期间冻结构建镜像升级）。
#   ⇒ **无论选谁，都必然有一趟是「同二进制两实例」**。选枢衡 ⇒ 受损的是
#   枢衡自己那一趟：**误差方向对我们不利** ⇒ PASS 更可信，⛔ FAIL 不许拿它辩解。
#   （选 haproxy 则受损的是很可能的最强者 ⇒ 门槛被低估 ⇒ PASS 不可信 ——
#    那正是判据 ④C／`G146` 在治的那个失效。）
#
# ⛔ **枢衡 `file_server` 当场出局**：静态吞吐实测 7638.6 rps，四家会一起被它压住
#   ⇒ 判据 ④B（收敛）作废整类 ⇒ 白开一次窗口，而窗口要 owner 停一次服。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ ⚠ ★ **只为拿那四个负载参数的缺省**（`bench/lib.sh` 的「负载参数」一节）。
#   ⛔ **不许在本文件里再写一份 `:-` 缺省** —— 理由与 static-throughput.sh 逐字相同：
#   用例用「声明值或缺省」而快照记「声明值」，会让没人显式设的参数在快照里成 `null`。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/case/reverse-proxy-throughput.sh <输出目录>}
CASE=reverse-proxy-throughput
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 这四个**不带 `:-`**：缺省由上面那次 `source` 供。少了那一行就是 `set -u`
#   当场报错，⛔ 而不是安静地回落到一个只有本文件知道的数。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 端口：9949–9953（端口表在 docs/platform/host-and-gate-traps.md）。
# ⚠ ⛔ 一个都别改成 :80 —— 那个端口在门禁里是共享的（G137），占住它会让一个
#   与本用例毫无关系的场景起不来，而报错落在那个无辜的端口上。
PORT_ORIGIN=${BENCH_PORT_RP_ORIGIN:-9949}
PORT_FULCRUM=${BENCH_PORT_RP_FULCRUM:-9950}
PORT_CADDY=${BENCH_PORT_RP_CADDY:-9951}
PORT_HAPROXY=${BENCH_PORT_RP_HAPROXY:-9952}
PORT_NGINX=${BENCH_PORT_RP_NGINX:-9953}

FAILS=0
CHILD=
ORIGIN_PID=
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

stop_one() {
  local pid=$1 waited=0
  [ -n "$pid" ] || return 0
  kill -INT "$pid" 2>/dev/null || true
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 80 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  kill -KILL "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

cleanup() {
  stop_one "$CHILD"
  CHILD=
  stop_one "$ORIGIN_PID"
  ORIGIN_PID=
  rm -rf "$WORK"
}
trap cleanup EXIT

# ── 原语 ───────────────────────────────────────────────────────────────────
#
# ★ 「起来了没有」判在**行为**上（真连一次），⛔ 不判某个工具在不在 ——
#   `tests/stress/run.sh` 在这条上栽过：写 `ss -lnt …` 而镜像里根本没有 `ss`，
#   于是那个原语**恒返回 false**。
port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}
wait_port() {
  local tries=0
  while [ "$tries" -lt 150 ]; do
    port_listening "$1" && return 0
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}
wait_port_gone() {
  local tries=0
  while [ "$tries" -lt 150 ]; do
    port_listening "$1" || return 0
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

# 本 netns 里被动打开的 TCP 连接累计数（对齐口径第 ⑥ 条的观测量）。
#
# ⚠ ⚠ 它是**整个 netns 的**，⛔ 不是只有「反代→源站」那一段（还含 oha→反代 那部分）。
#   ★ 它够用**不是因为它精确，而是因为对齐与不对齐之间隔着两个数量级**：
#     开发机 10s/50 连接实测，对齐后 0.4–2.1，未对齐 100.3（caddy）与 1001.5（nginx）。
# ⚠ 逐列按**表头**找 `PassiveOpens`，⛔ 不写死列号：不同内核的 `/proc/net/snmp`
#   列数不同，而一个写死的列号会安静地读出另一个计数器。
passive_opens() {
  awk '/^Tcp:/ {
    if (h == "") { for (i = 1; i <= NF; i++) k[i] = $i; h = 1; next }
    for (i = 1; i <= NF; i++) if (k[i] == "PassiveOpens") print $i
  }' /proc/net/snmp
}

# CPU 亲和。★ 两组核都给了才用 `taskset`；只给一个等于没给。
# ⚠ ⚠ **源站与被测共享 `BENCH_SERVER_CPUS`**（owner 2026-09-09 拍板）：合格宿主
#   只有 4 核，分三组会让 oha 只剩 1 核去打四万多 rps ⇒ 很可能**压测端先饱和**，
#   而那正是判据 ④B 会作废整类的情形。
#   ⇒ 代价是源站吃掉的 CPU 混进四家读数，★ 但它对四家一视同仁 ⇒ 类内的相对
#   比较（G19 那 10%）仍然成立。这一条写在 bench/README.md 的明处。
pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

mkdir -p "$RAW_DIR"

# ── payload：可打印的 N 字节 ───────────────────────────────────────────────
#
# ⚠ ⚠ `respond` 的正文**只有内联形态**（⛔ 没有 file 形式，不像 HAProxy 那条
#   `http-request return … file <路径>`）⇒ payload 必须**可打印**。取 base64
#   字母表（不含 `|`、`&`、反斜杠与引号）—— 与 diag/respond-ceiling.sh 同一手法。
# ⛔ 这与静态吞吐那一类的 `/dev/urandom` **不是同一份字节** ⇒ 两类的绝对读数
#   不可并排比较（又一条，写在 bench/README.md）。
# ⚠ base64 把 3 字节涨成 4 ⇒ 取 PAYLOAD_BYTES 字节的随机源足够裁出 PAYLOAD_BYTES 个字符。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WORK/rand.bin"
base64 -w0 < "$WORK/rand.bin" > "$WORK/b64.txt"
head -c "$PAYLOAD_BYTES" "$WORK/b64.txt" > "$WORK/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WORK/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "REVERSE-PROXY-THROUGHPUT FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
BODY=$(cat "$WORK/payload.bin")
URL_PATH=/payload.bin

# ── 渲染配置 ───────────────────────────────────────────────────────────────
#
# ★ 模板在 `bench/conf/` 下、是**交付物的一部分**，这里只替换占位符。
#   ⛔ 别把配置内联进本脚本：第三方要看的就是那几份配置本身。
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__RUN_DIR__|$WORK|g" \
    -e "s|__ORIGIN_HOST__|$HOST|g" \
    -e "s|__ORIGIN_PORT__|$PORT_ORIGIN|g" \
    "$src" > "$dst"
}
# 源站：与 diag/respond-ceiling.sh **共用同一份模板**（见那份文件头部的说明）。
sed -e "s|__PORT__|$PORT_ORIGIN|g" \
  -e "s|__WORKERS__|$WORKERS|g" \
  -e "s|__BODY__|$BODY|g" \
  "$BENCH_DIR/conf/fulcrum-respond.Fulcrumfile" > "$WORK/origin.Fulcrumfile"

render "$BENCH_DIR/conf/fulcrum-proxy.Fulcrumfile" "$WORK/fulcrum.Fulcrumfile" "$PORT_FULCRUM"
render "$BENCH_DIR/conf/Caddyfile-proxy" "$WORK/Caddyfile" "$PORT_CADDY"
render "$BENCH_DIR/conf/haproxy-proxy.cfg" "$WORK/haproxy.cfg" "$PORT_HAPROXY"
render "$BENCH_DIR/conf/nginx-proxy.conf" "$WORK/nginx.conf" "$PORT_NGINX"
mkdir -p "$WORK/nginx-body"

# ── 源站：常驻，跨四趟 ─────────────────────────────────────────────────────
#
# ★ 它是**环境**不是被测 ⇒ 不跟着四家一起起停：每趟重起会把源站的预热状态
#   变成一个新变量，而四家面对的就不再是同一个上游了。
echo "── 源站（枢衡 respond）──"
"${pin_server[@]}" "$FULCRUM_BIN" serve "$WORK/origin.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/origin.pid" \
  --upgrade-sock "$WORK/origin.sock" \
  --state-dir "$WORK/origin-state" > "$WORK/origin.log" 2>&1 &
ORIGIN_PID=$!
if ! wait_port "$PORT_ORIGIN"; then
  echo "REVERSE-PROXY-THROUGHPUT FAILED: 源站没在 $PORT_ORIGIN 上起来" >&2
  sed 's/^/      /' "$WORK/origin.log" >&2 || true
  exit 1
fi
ok "源站在 $PORT_ORIGIN 上起来了"

CONNS_TXT="$RAW_DIR/upstream-conns.txt"
: > "$CONNS_TXT"

# ── 一家的完整一趟 ─────────────────────────────────────────────────────────
#
# 核源站还活着 → 起 → 等端口 → **核对它转发的到底是不是那份 payload** →
# 打负载（前后各读一次 PassiveOpens）→ 收 → 等端口消失。
run_subject() {
  local name=$1 port=$2
  shift 2
  local log="$WORK/$name.log"

  echo "── $name ──"

  # ★ ★ 每趟开跑前先确认**源站还活着** —— 它常驻跨四趟，中途死掉会让后面几家
  #   一起变成 502，而 `read-raw.py` 会把它们判成 INVALID（那是对的），
  #   但现场看不出真因在源站身上。
  if ! kill -0 "$ORIGIN_PID" 2>/dev/null; then
    bad "$name 开跑前源站已经不在了 —— 后面几家的读数都不算数"
    return 0
  fi

  "${pin_server[@]}" "$@" > "$log" 2>&1 &
  CHILD=$!

  if ! wait_port "$port"; then
    bad "$name 没在 $port 上起来"
    sed 's/^/      /' "$log" >&2 || true
    stop_one "$CHILD"
    CHILD=
    return 0
  fi

  # ★ ★ ★ **开跑前先证明样本里真有东西，且它真的穿过了源站。**
  #   一个稳定回 404、或回 200 空 body 的被测会给出非常漂亮的 RPS，
  #   而那个数与「转发那个资源」毫无关系。⇒ 状态码、字节数、**逐字节**都要对上。
  local probe code size
  probe=$(curl -sS -o "$WORK/$name.body" -w '%{http_code} %{size_download}' \
    "http://$HOST:$port$URL_PATH" 2>>"$log" || echo "000 0")
  code=${probe%% *}
  size=${probe##* }
  if [ "$code" != "200" ] || [ "$size" != "$PAYLOAD_BYTES" ]; then
    bad "$name 开跑前的核对没过：HTTP $code，body $size 字节（该是 200 / $PAYLOAD_BYTES）"
    sed 's/^/      /' "$log" >&2 || true
    stop_one "$CHILD"
    CHILD=
    return 0
  fi
  if ! cmp -s "$WORK/$name.body" "$WORK/payload.bin"; then
    bad "$name 回的正文与源站那份 payload **不是同一份字节**"
    stop_one "$CHILD"
    CHILD=
    return 0
  fi
  ok "$name 转发的是源站那份 payload（HTTP 200，$size 字节，逐字节相同）"

  # ── 负载 + 上游连接复用的观测量 ─────────────────────────────────────────
  local before after opened reqs per1k
  before=$(passive_opens)
  if "${pin_load[@]}" oha \
    --output-format json --no-tui \
    -z "$DURATION" -c "$CONNECTIONS" \
    -o "$RAW_DIR/$name.json" \
    "http://$HOST:$port$URL_PATH" >> "$log" 2>&1; then
    ok "$name 的原始数据已落盘：raw/$CASE/$name.json"
  else
    bad "$name 那一趟负载没跑完"
    sed 's/^/      /' "$log" >&2 || true
  fi
  after=$(passive_opens)
  opened=$((after - before))

  # ⚠ ⚠ ⛔ **有意不叫 `.json`** —— `bench/read-raw.py` 对本目录做 `glob("*.json")`，
  #   一个 `.json` 会被它当成**第五家被测**混进竞品集合，于是「最强者」可能变成它，
  #   而**不会有任何东西说**。★ 与 `ceiling.txt` 逐字同一条教训（bench/verdict.sh）。
  if [ -s "$RAW_DIR/$name.json" ]; then
    reqs=$(python3 -c 'import json,sys;print(sum(json.load(open(sys.argv[1]))["statusCodeDistribution"].values()))' \
      "$RAW_DIR/$name.json" 2>/dev/null || echo 0)
  else
    reqs=0
  fi
  if [ "$reqs" -gt 0 ] 2>/dev/null; then
    per1k=$(awk -v o="$opened" -v r="$reqs" 'BEGIN { printf "%.2f", 1000.0 * o / r }')
  else
    # ⛔ 「算不出来」一律记成一个判据认得的坏值，⛔ 而不是省略这一行 ——
    #   缺一行会让判据少判一家，而**少判与判过在输出上分不开**。
    per1k=NaN
  fi
  printf '%s %s\n' "$name" "$per1k" >> "$CONNS_TXT"
  echo "  · $name 本趟新建连接 $opened / 请求 $reqs ⇒ 每千请求 $per1k 条"

  stop_one "$CHILD"
  CHILD=
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一家会起不来"
}

# ⚠ 顺序有意固定：结果与顺序无关才对，⇒ 固定它是为了让两趟之间可比。
run_subject fulcrum "$PORT_FULCRUM" \
  "$FULCRUM_BIN" serve "$WORK/fulcrum.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/fulcrum.pid" \
  --upgrade-sock "$WORK/fulcrum.sock" \
  --state-dir "$WORK/state"

# ★ `GOMAXPROCS` 是 Caddy 唯一的并行度旋钮（它是 Go 写的，没有 worker 进程的概念）。
GOMAXPROCS="$WORKERS" \
  run_subject caddy "$PORT_CADDY" \
  caddy run --config "$WORK/Caddyfile" --adapter caddyfile

# ★ `-db` = 不 daemon 化、不做后台化，编排脚本才拿得到它的 pid。
run_subject haproxy "$PORT_HAPROXY" \
  haproxy -f "$WORK/haproxy.cfg" -db

# ★ nginx 的 `daemon off` 写在配置里（见 bench/conf/nginx-proxy.conf）。
run_subject nginx "$PORT_NGINX" \
  nginx -c "$WORK/nginx.conf"

stop_one "$ORIGIN_PID"
ORIGIN_PID=
wait_port_gone "$PORT_ORIGIN" || bad "源站收掉之后 $PORT_ORIGIN 还占着"

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/$CASE] 四家全部跑通，原始数据在 $RAW_DIR"
  echo "[bench/$CASE] ⛔ 本步**不产出任何结论** —— 判定见 bench/verdict.sh"
else
  echo "[bench/$CASE] ★ $FAILS 处失败 —— 这一趟的原始数据不完整，⛔ 不许拿去判定" >&2
  exit 1
fi
