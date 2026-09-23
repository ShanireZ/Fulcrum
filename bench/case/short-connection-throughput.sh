#!/usr/bin/env bash
# 用例类「高并发短连接」（M3，§8 七类之一）—— 四家各从**内存**回同一份正文，
# 而**每个请求都是一条新连接、由服务端先关**。
#
#   bash bench/case/short-connection-throughput.sh <输出目录>
#
# ⛔ **本脚本不判定、不比较、不打印任何「谁更快」。** 它只产出原始数据。
#   判定在 `bench/verdict.sh`，而那一步会先问宿主合不合格。
#
# ── 对齐口径（★ 每一条都会被 bench/README.md 引用，改这里要一起改）─────────────
#
#   ① **一次只起一家。**
#   ② **同一份 payload 字节、同一个 URL 路径**，开跑前逐字节核对过。
#   ③ **访问日志四家全关。**
#   ④ **明文 HTTP/1.1。** TLS 归「TLS 握手」那一类，⛔ 不混进来。
#   ⑤ **worker 数四家一致**（`BENCH_WORKERS`）。
#   ⑥ ★ ★ ★ **每个请求一条新连接，且由服务端先关**（owner 2026-09-23 拍板）。
#      oha 带 `--disable-keepalive` 与 `-H 'Connection: close'`（收成一个数组 `OHA_SHORT_ARGS`）。
#      ⚠ 在 oha v1.15.0 下后者是**冗余**的：`--disable-keepalive` 在 HTTP/1.1 时自己就插
#      `Connection: close`（`src/lib.rs:537`；用户的 `-H` 在它之后插、能覆盖它）。
#      ⛔ 设计时以为它不发（只读了 `src/client.rs`），2026-09-23 的注入反证当场推翻。
#      ★ 仍然留着那个头：口径写在明处，不依赖 oha 的一个隐式行为（换个版本它可能就不加了）。
#      三件事各有一道门：
#        · 每请求一条新连接 —— 判据 ⑥ ①（PassiveOpens 差值，落 `new-conns.txt`）；
#        · oha 真的带着那个头 —— 判据 ⑥ ②（开跑前用**同一个数组**向合成服务端发一次，
#          抓到的请求原文落 `oha-request.txt`）；
#        · 四家收到那个头会先关 —— 每家开跑前的行为探针（`bench/close-probe.py`）。
#      判据本体在 `bench/lib.sh`，接线由 `tests/bench/gate.sh` 的 C21 判。
#      ⚠ `new-conns.txt` 后两列（TIME_WAIT 两侧条数）是**诊断**，⛔ 不判：实测谁先关是竞态，
#        门禁参数下还会被 netns 的 TIME_WAIT 上限截断（理由见 `bench_tw_split` 的注释）。
#
# ── 为什么是「从内存回」（owner 2026-09-23 拍板）──────────────────────────────
#
# 这一类量的只是「建连 → 解析 → 回应 → 拆连」，⛔ 不混进文件 IO（那条路归静态吞吐那一类）。
# 四家各用自己的内联写法，模板按**形态**命名（`*-respond*`）：
#   枢衡 `respond` · Caddy `respond` · HAProxy `http-request return … file`（启动时读进内存）·
#   ⚠ nginx 是**两个变量拼接** —— 它读配置的缓冲区只有 4096 字节，一个 4096 字节的参数
#   解析不了。代价与方向写在 `bench/conf/nginx-respond.conf` 头部与 bench/README.md。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ ⚠ ★ **只为拿那四个负载参数的缺省与判据 ⑥ 的解析函数**（`bench/lib.sh`）。
#   ⛔ **不许在本文件里再写一份 `:-` 缺省** —— 理由与 static-throughput.sh 逐字相同。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/case/short-connection-throughput.sh <输出目录>}
CASE=short-connection-throughput
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 这四个**不带 `:-`**：缺省由上面那次 `source` 供。少了那一行就是 `set -u` 当场报错。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 端口：9956–9960（端口表在 docs/platform/host-and-gate-traps.md）。9960 = 抓请求头的合成服务端。
# ⚠ ⛔ 一个都别改成 :80 —— 那个端口在门禁里是共享的（G137）。
PORT_FULCRUM=${BENCH_PORT_SC_FULCRUM:-9956}
PORT_CADDY=${BENCH_PORT_SC_CADDY:-9957}
PORT_HAPROXY=${BENCH_PORT_SC_HAPROXY:-9958}
PORT_NGINX=${BENCH_PORT_SC_NGINX:-9959}
PORT_CAPTURE=${BENCH_PORT_SC_CAPTURE:-9960}

# ★ ★ ★ 短连接的两个旗标**只在这里写一次**：负载那一行与抓请求头那一次用的是**同一个数组**
#   ⇒ 判据 ⑥ ② 判到的，就是负载真正带着的。⛔ 别在负载那一行另抄一份。
OHA_SHORT_ARGS=(--disable-keepalive -H 'Connection: close')

FAILS=0
CHILD=
CAP_PID=
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

stop_child() {
  [ -n "$CHILD" ] || return 0
  kill -INT "$CHILD" 2>/dev/null || true
  local waited=0
  while kill -0 "$CHILD" 2>/dev/null && [ "$waited" -lt 80 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  kill -KILL "$CHILD" 2>/dev/null || true
  wait "$CHILD" 2>/dev/null || true
  CHILD=
}

cleanup() {
  stop_child
  if [ -n "$CAP_PID" ]; then
    kill "$CAP_PID" 2>/dev/null || true
    wait "$CAP_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# ── 原语 ───────────────────────────────────────────────────────────────────
#
# ★ 「起来了没有」判在**行为**上（真连一次），⛔ 不判某个工具在不在 ——
#   `tests/stress/run.sh` 在这条上栽过：写 `ss -lnt …` 而镜像里根本没有 `ss`。
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

# 本 netns 里被动打开的 TCP 连接累计数（判据 ⑥ 第 ① 半的观测量）。
# ★ 与反代类同一个计数器、同一个取法：逐列按**表头**找 `PassiveOpens`，⛔ 不写死列号。
# ★ 本类**没有源站** ⇒ oha 跑的那段时间里，netns 里只有 oha→被测这一段在建连。
passive_opens() {
  awk '/^Tcp:/ {
    if (h == "") { for (i = 1; i <= NF; i++) k[i] = $i; h = 1; next }
    for (i = 1; i <= NF; i++) if (k[i] == "PassiveOpens") print $i
  }' /proc/net/snmp
}

# CPU 亲和。★ 两组核都给了才用 `taskset`；只给一个等于没给。
pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

mkdir -p "$RAW_DIR"

# ── 探针先自证（★ 承重）──────────────────────────────────────────────────────
#
# 下面每家开跑前都要过一道「它真的会先关」的行为核对，而一个**从没见过「不关」**的探针
# 与一个恒报「关了」的坏探针给不出任何区别 ⇒ 先用合成被测两个方向证一遍，不过就整类不跑。
if ! python3 "$BENCH_DIR/close-probe.py" --self-check; then
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: 行为探针的自测没过 —— 它判不动，这一类不跑" >&2
  exit 1
fi

# ── payload：可打印的 N 字节（与反代类同一手法）───────────────────────────────
#
# ⚠ 枢衡 `respond` 与 Caddy `respond` 的正文都是内联的 ⇒ 必须**可打印**：取 base64 字母表
#   （不含 `|`、`&`、`$`、反斜杠与引号 ⇒ `sed` 替换、nginx 的变量语法、各家的引号都撑不破）。
# ⛔ 这与静态吞吐那一类的 `/dev/urandom` **不是同一份字节** ⇒ 两类的绝对读数不可并排比较。
# ⚠ base64 把 3 字节涨成 4 ⇒ 取 PAYLOAD_BYTES 字节的随机源足够裁出 PAYLOAD_BYTES 个字符。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WORK/rand.bin"
base64 -w0 < "$WORK/rand.bin" > "$WORK/b64.txt"
head -c "$PAYLOAD_BYTES" "$WORK/b64.txt" > "$WORK/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WORK/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
BODY=$(cat "$WORK/payload.bin")
# ⚠ nginx 那一家要两半（见 bench/conf/nginx-respond.conf 头部）。
HALF=$((PAYLOAD_BYTES / 2))
BODY_1=${BODY:0:$HALF}
BODY_2=${BODY:$HALF}
[ "${BODY_1}${BODY_2}" = "$BODY" ] || {
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: 两半拼回去不等于原文" >&2
  exit 1
}
# ⚠ 每一半加上两个引号必须**小于** 4096（nginx 的 `NGX_CONF_BUFFER`），否则它当场起不来 ——
#   ⛔ 在这里先说清楚，别让它变成一句落在 nginx 日志里的 `too long parameter`。
if [ $((PAYLOAD_BYTES - HALF + 2)) -ge 4096 ]; then
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: payload $PAYLOAD_BYTES 字节拆成两半仍放不进 nginx 那块 4096 字节的配置缓冲区 —— 要拆成更多段" >&2
  exit 1
fi
URL_PATH=/payload.bin

# ── 渲染四家的配置 ─────────────────────────────────────────────────────────
#
# ★ 模板在 `bench/conf/` 下、是**交付物的一部分**，这里只替换占位符。
# ⚠ `__BODY_1__` / `__BODY_2__` 排在 `__BODY__` 之前：后者不是前两者的前缀
#   （`__BODY__` 要求紧跟两个下划线），但排在前面让「谁先被替换」不需要人去想。
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__RUN_DIR__|$WORK|g" \
    -e "s|__PAYLOAD__|$WORK/payload.bin|g" \
    -e "s|__BODY_1__|$BODY_1|g" \
    -e "s|__BODY_2__|$BODY_2|g" \
    -e "s|__BODY__|$BODY|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum-respond.Fulcrumfile" "$WORK/fulcrum.Fulcrumfile" "$PORT_FULCRUM"
render "$BENCH_DIR/conf/Caddyfile-respond" "$WORK/Caddyfile" "$PORT_CADDY"
render "$BENCH_DIR/conf/haproxy-respond.cfg" "$WORK/haproxy.cfg" "$PORT_HAPROXY"
render "$BENCH_DIR/conf/nginx-respond.conf" "$WORK/nginx.conf" "$PORT_NGINX"
mkdir -p "$WORK/nginx-body"

CONNS_TXT="$RAW_DIR/new-conns.txt"
: > "$CONNS_TXT"

# ── 判据 ⑥ ② 的观测量：oha 真正发出的那个请求 ──────────────────────────────────
#
# ★ 用负载那一组参数（`OHA_SHORT_ARGS`，同一个数组）向一个只记请求头的合成服务端发**一次**，
#   把它收到的请求头原文落盘；判定在 `bench/lib.sh` 的 `bench_request_close_violations`。
# ⚠ 合成服务端会跳过 `wait_port` 那一下空连接、只存第一条带完整请求头的连接。
# ⚠ ⚠ ⛔ 落盘文件**有意不叫 `.json`**（`read-raw.py` 会把它当成第五家被测）。
# ⛔ 这一步只采读数、不判定：没抓到时文件为空，由判据当「判不了」算违规 —— 与另外几格同一个分工。
REQ_TXT="$RAW_DIR/oha-request.txt"
: > "$REQ_TXT"
python3 "$BENCH_DIR/close-probe.py" --capture-request "$PORT_CAPTURE" "$REQ_TXT" &
CAP_PID=$!
if wait_port "$PORT_CAPTURE"; then
  "${pin_load[@]}" oha --no-tui -n 1 -c 1 "${OHA_SHORT_ARGS[@]}" \
    "http://$HOST:$PORT_CAPTURE$URL_PATH" > "$WORK/oha-capture.log" 2>&1 || true
else
  bad "抓请求头的合成服务端没在 $PORT_CAPTURE 上起来"
fi
if wait "$CAP_PID"; then
  ok "抓到了 oha 发出的请求头：raw/$CASE/oha-request.txt"
else
  bad "没抓到 oha 发出的请求头（合成服务端 20 秒内没等到一条完整请求）"
  sed 's/^/      /' "$WORK/oha-capture.log" >&2 || true
fi
CAP_PID=

# ── 一家的完整一趟 ─────────────────────────────────────────────────────────
#
# 起 → 等端口 → **核对它回的是不是那份 payload** → **核对它真的会先关** →
# 打负载（前后各读一次 PassiveOpens，跑完读 TIME_WAIT）→ 收 → 等端口消失。
run_subject() {
  local name=$1 port=$2
  shift 2
  local log="$WORK/$name.log"

  echo "── $name ──"
  "${pin_server[@]}" "$@" > "$log" 2>&1 &
  CHILD=$!

  if ! wait_port "$port"; then
    bad "$name 没在 $port 上起来"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi

  # ★ ★ ★ **开跑前先证明样本里真有东西**：状态码、字节数、**逐字节**都要对上。
  local probe code size
  probe=$(curl -sS -o "$WORK/$name.body" -w '%{http_code} %{size_download}' \
    "http://$HOST:$port$URL_PATH" 2>>"$log" || echo "000 0")
  code=${probe%% *}
  size=${probe##* }
  if [ "$code" != "200" ] || [ "$size" != "$PAYLOAD_BYTES" ]; then
    bad "$name 开跑前的核对没过：HTTP $code，body $size 字节（该是 200 / $PAYLOAD_BYTES）"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi
  if ! cmp -s "$WORK/$name.body" "$WORK/payload.bin"; then
    bad "$name 回的正文与那份 payload **不是同一份字节**"
    stop_child
    return 0
  fi
  ok "$name 回的是那份 payload（HTTP 200，$size 字节，逐字节相同）"

  # ★ ★ ★ **再证明它真的会先关**：判在行为上（限时内读到 EOF），⛔ 不判响应头。
  local close_rc=0
  python3 "$BENCH_DIR/close-probe.py" "$HOST" "$port" "$URL_PATH" || close_rc=$?
  if [ "$close_rc" != 0 ]; then
    bad "$name 收到 Connection: close 之后没有先关连接（close-probe 退 $close_rc；3 = 限时内没关，4 = 不是 200，1 = 连不上）"
    stop_child
    return 0
  fi
  ok "$name 收到 Connection: close 之后先关了连接（行为核对，⛔ 不是看响应头）"

  # ── 负载 + 判据 ⑥ 的两个观测量 ───────────────────────────────────────────
  local before after opened reqs per1k tw
  before=$(passive_opens)
  if "${pin_load[@]}" oha \
    --output-format json --no-tui \
    "${OHA_SHORT_ARGS[@]}" \
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
  # ★ 诊断列（⛔ 不判）：跑完那一刻这一家端口上的 TIME_WAIT 落在哪一侧。每家端口不同
  #   ⇒ 前一家留下的不会算到这一家头上；⚠ 但 netns 的 TIME_WAIT 表是**共享**的，前几家把它
  #   占满时后几家会读到 `0 0`（门禁参数下实测就是这样）⇒ 读它之前先看 `bench_tw_split` 的注释。
  tw=$(bench_tw_split "$port" "$(cat /proc/net/tcp /proc/net/tcp6 2>/dev/null || true)")

  # ⚠ ⚠ ⛔ 观测文件**有意不叫 `.json`** —— `bench/read-raw.py` 对本目录做 `glob("*.json")`，
  #   一个 `.json` 会被当成**第五家被测**。★ 与 `upstream-conns.txt` / `ceiling.txt` 同一条教训。
  if [ -s "$RAW_DIR/$name.json" ]; then
    reqs=$(python3 -c 'import json,sys;print(sum(json.load(open(sys.argv[1]))["statusCodeDistribution"].values()))' \
      "$RAW_DIR/$name.json" 2>/dev/null || echo 0)
  else
    reqs=0
  fi
  if [ "$reqs" -gt 0 ] 2>/dev/null; then
    per1k=$(awk -v o="$opened" -v r="$reqs" 'BEGIN { printf "%.2f", 1000.0 * o / r }')
  else
    # ⛔ 「算不出来」一律记成判据认得的坏值，⛔ 而不是省略这一行 —— 少判与判过在输出上分不开。
    per1k=NaN
  fi
  printf '%s %s %s\n' "$name" "$per1k" "$tw" >> "$CONNS_TXT"
  echo "  · $name 本趟新建连接 $opened / 请求 $reqs ⇒ 每千请求 $per1k 条；TIME_WAIT 服务端 / 客户端 = $tw（诊断）"

  stop_child
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

# ★ nginx 的 `daemon off` 写在配置里（见 bench/conf/nginx-respond.conf）。
run_subject nginx "$PORT_NGINX" \
  nginx -c "$WORK/nginx.conf"

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/$CASE] 四家全部跑通，原始数据在 $RAW_DIR"
  echo "[bench/$CASE] ⛔ 本步**不产出任何结论** —— 判定见 bench/verdict.sh"
else
  echo "[bench/$CASE] ★ $FAILS 处失败 —— 这一趟的原始数据不完整，⛔ 不许拿去判定" >&2
  exit 1
fi
