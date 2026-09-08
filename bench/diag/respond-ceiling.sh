#!/usr/bin/env bash
# 诊断读数：`respond` 的上界，与 `file_server` **背靠背**比。
#
#   bash bench/diag/respond-ceiling.sh <输出目录>
#
# ★ ★ ★ **这不是 §8 七类里的一类，本步也不判定、不比较、不打印任何「谁更快」。**
#   §8 那七类是「与 Caddy / HAProxy / Nginx 三家对拍」；这一步只有枢衡自己，
#   量的是「把响应推出去」这条路的上界 —— 用来回答
#   `handoff/design-static-file-cache.md` §7 那个问题：
#   「把文件那条路修到极限能到哪」，⇒ 方案 ③ 做不做由它决定。
#
# ⚠ ⚠ ⛔ **输出落 `diag/`，不落 `raw/`。** `bench/verdict.sh` 是
#   `for raw_case in "$OUT_DIR"/raw/*/` —— **`raw/` 下每个子目录都会被当成一类去判**，
#   而 `bench/read-raw.py` 把该目录下每个 `*.json` 当成一家被测。只有枢衡一家
#   ⇒ 判据 ④ 会按「有效读数不足两条」判 `VOID`：那不是错结论，但它把一个
#   **诊断上界**摆成了一个 §8 的类别，而七类里没有它。
#   ★ 与 G142 记的那条同族（`ceiling.txt` 不许叫 `.json`，否则被当成第五家被测）。
#   两个方向都由 `tests/bench/gate.sh` 的 C17 钉着。
#
# ── 对齐口径（两个变体之间）────────────────────────────────────────────────
#
#   ① **一次只起一个。** 同时跑会互相抢 CPU，量到的是「两个一起跑」这件事。
#   ② **同一份 payload 字节、同一个 URL 路径。** ★ 两边都用 `/payload.bin`：
#      `respond` 对任何路径都回同一份正文，取同一个路径是为了让**请求字节数**也一致。
#   ③ **访问日志两边全关；明文 HTTP/1.1 + keep-alive。**
#   ④ **worker 数两边一致**（`BENCH_WORKERS`，两份配置吃同一个 `threads_l7`）。
#   ⑤ ⚠ **一处有意留着的不对等**：`respond` 与 `file_server` 写响应走的是不同的路
#      （`dsl-reference.md`：`encode` 今天只接在 `file_server` 与转发两处）。
#      ⇒ 两边**都不配 `encode`**，且 payload 不可压缩 ⇒ 这一格对两边都不起作用。
#      ⛔ 引用差值时别把这句话丢掉。
#
# ⚠ ⚠ **payload 必须是可打印的**：`respond` 的正文**只有内联形态**
#   （⛔ 没有 file 形式）⇒ 用 base64 字母表（不含 `|`、`&`、反斜杠与引号
#   ⇒ `sed` 替换与 DSL 的引号都不会被它撑破）。
#   ⛔ 这与静态吞吐那一类的 `/dev/urandom` **不是同一份字节** ——
#   ★ 但那不影响本步：本步比的是**同一趟里的两个变体**，两边用的是同一份可打印字节。
#   ⛔ 也**因此不许**把本步的读数与 `raw/static-throughput/fulcrum.json` 并排比。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ 只为拿那四个负载参数的缺省（`bench/lib.sh` 的「负载参数」一节）。
#   ⛔ 不许在本文件里再写一份 `:-` 缺省 —— 那正是 G147 治掉的那一族。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/diag/respond-ceiling.sh <输出目录>}
NAME=respond-ceiling
DIAG_DIR="$OUT_DIR/diag/$NAME"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 这四个不带 `:-`：缺省由上面那次 `source` 供（G147 一处定义）。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 端口：9944–9945，在 `docs/platform/host-and-gate-traps.md` 那张表已保留的
# 9940–9949 段之内（9940–9943 归静态吞吐那一类）。
# ⛔ 一个都别改成 :80 —— 那个端口在门禁里是共享的（G137）。
PORT_FILE=${BENCH_PORT_DIAG_FILE:-9944}
PORT_RESPOND=${BENCH_PORT_DIAG_RESPOND:-9945}

FAILS=0
CHILD=
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

cleanup() {
  stop_child
  rm -rf "$WORK"
  [ -z "${WWW:-}" ] || rm -rf "$WWW"
}
trap cleanup EXIT

# ── 原语（与 bench/case/static-throughput.sh 同一套，理由见那里的注释）──────
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

pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

# ── payload：可打印的 N 字节 ───────────────────────────────────────────────
#
# ⚠ ⚠ ⛔ **有意一步都不走管道**：`head -c N` 读够就关，上游会吃到 SIGPIPE，
#   而本文件是 `set -o pipefail` ⇒ 写成 `… | base64 | head -c N` 会让整脚本
#   在一次**完全正常**的截断上退出。⇒ 落三个中间文件，一步一个命令。
if [ -n "${BENCH_WWW_ROOT:-}" ]; then
  WWW="$BENCH_WWW_ROOT/$NAME-$$"
  rm -rf "$WWW"
else
  WWW="$WORK/www"
fi
mkdir -p "$WWW" "$DIAG_DIR"

# base64 把 3 字节涨成 4 ⇒ 取 PAYLOAD_BYTES 字节的随机源足够裁出 PAYLOAD_BYTES 个字符。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WORK/rand.bin"
base64 -w0 < "$WORK/rand.bin" > "$WORK/b64.txt"
head -c "$PAYLOAD_BYTES" "$WORK/b64.txt" > "$WWW/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WWW/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "RESPOND-CEILING FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
BODY=$(cat "$WWW/payload.bin")
URL_PATH=/payload.bin

# ── 渲染两份配置 ───────────────────────────────────────────────────────────
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WWW_ROOT__|$WWW|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__BODY__|$BODY|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum.Fulcrumfile" "$WORK/file.Fulcrumfile" "$PORT_FILE"
render "$BENCH_DIR/conf/fulcrum-respond.Fulcrumfile" "$WORK/respond.Fulcrumfile" "$PORT_RESPOND"

# ── 一个变体的完整一趟 ─────────────────────────────────────────────────────
run_variant() {
  local name=$1 port=$2 conf=$3
  local log="$WORK/$name.log"

  echo "── $name ──"
  "${pin_server[@]}" "$FULCRUM_BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    --state-dir "$WORK/$name-state" > "$log" 2>&1 &
  CHILD=$!

  if ! wait_port "$port"; then
    bad "$name 没在 $port 上起来"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi

  # ★ ★ ★ 开跑前先证明它回的**就是那份字节**。
  #   ⚠ 判据比 static-throughput 那条更强：那边比状态码与**长度**，这里直接
  #   `cmp` 整份正文 —— 本步比的是两个变体的差，⇒ 「两边回的是同一份字节」
  #   是这个差有意义的前提，而长度相同的两份不同正文照样能骗过长度判据。
  local code
  code=$(curl -sS -o "$WORK/$name.body" -w '%{http_code}' \
    "http://$HOST:$port$URL_PATH" 2>>"$log" || echo 000)
  if [ "$code" != "200" ]; then
    bad "$name 开跑前的核对没过：HTTP $code（该是 200）"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi
  if ! cmp -s "$WORK/$name.body" "$WWW/payload.bin"; then
    bad "$name 回的正文与 payload **不是同一份字节**（$(wc -c < "$WORK/$name.body") 字节对 $PAYLOAD_BYTES）"
    stop_child
    return 0
  fi
  ok "$name 回的正文与 payload 逐字节相同（HTTP 200，$PAYLOAD_BYTES 字节）"

  if "${pin_load[@]}" oha \
    --output-format json --no-tui \
    -z "$DURATION" -c "$CONNECTIONS" \
    -o "$DIAG_DIR/$name.json" \
    "http://$HOST:$port$URL_PATH" >> "$log" 2>&1; then
    ok "$name 的原始数据已落盘：diag/$NAME/$name.json"
  else
    bad "$name 那一趟负载没跑完"
    sed 's/^/      /' "$log" >&2 || true
  fi

  stop_child
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一个变体会起不来"
}

# ⚠ 顺序有意固定：结果与顺序无关才对，⇒ 固定它是为了让两趟之间可比。
run_variant fulcrum-file "$PORT_FILE" "$WORK/file.Fulcrumfile"
run_variant fulcrum-respond "$PORT_RESPOND" "$WORK/respond.Fulcrumfile"

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/diag/$NAME] 两个变体都跑通，原始数据在 $DIAG_DIR"
  echo "[bench/diag/$NAME] ⛔ 本步**不产出任何结论**，也**不参与** §8 的判定"
else
  echo "[bench/diag/$NAME] ★ $FAILS 处失败 —— 这一趟的诊断数据不完整" >&2
  exit 1
fi
