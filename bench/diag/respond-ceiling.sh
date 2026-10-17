#!/usr/bin/env bash
# 診斷讀數：`respond` 的上界，與 `file_server` **背靠背**比。
#
#   bash bench/diag/respond-ceiling.sh <输出目录>
#
# ★ ★ ★ **這不是 §8 七類裡的一類，本步也不判定、不比較、不列印任何「誰更快」。**
#   §8 那七類是「與 Caddy / HAProxy / Nginx 三家對拍」；這一步只有樞衡自己，
#   量的是「把回應推出去」這條路的上界 —— 用來回答
#   `handoff/design-static-file-cache.md` §7 那個問題：
#   「把檔案那條路修到極限能到哪」，⇒ 方案 ③ 做不做由它決定。
#
# ⚠ ⚠ ⛔ **輸出落 `diag/`，不落 `raw/`。** `bench/verdict.sh` 是
#   `for raw_case in "$OUT_DIR"/raw/*/` —— **`raw/` 下每個子目錄都會被當成一類去判**，
#   而 `bench/read-raw.py` 把該目錄下每個 `*.json` 當成一家被測。只有樞衡一家
#   ⇒ 判據 ④ 會按「有效讀數不足兩條」判 `VOID`：那不是錯結論，但它把一個
#   **診斷上界**擺成了一個 §8 的類別，而七類裡沒有它。
#   ★ 與 G142 記的那條同族（`ceiling.txt` 不許叫 `.json`，否則被當成第五家被測）。
#   兩個方向都由 `tests/bench/gate.sh` 的 C17 釘著。
#
# ── 對齊口徑（兩個變體之間）────────────────────────────────────────────────
#
#   ① **一次只起一個。** 同時跑會互相搶 CPU，量到的是「兩個一起跑」這件事。
#   ② **同一份 payload 位元組、同一個 URL 路徑。** ★ 兩邊都用 `/payload.bin`：
#      `respond` 對任何路徑都回同一份正文，取同一個路徑是為了讓**請求位元組數**也一致。
#   ③ **訪問紀錄兩邊全關；明文 HTTP/1.1 + keep-alive。**
#   ④ **worker 數兩邊一致**（`BENCH_WORKERS`，兩份設定吃同一個 `threads_l7`）。
#   ⑤ ⚠ **一處有意留著的不對等**：`respond` 與 `file_server` 寫回應走的是不同的路
#      （`dsl-reference.md`：`encode` 今天只接在 `file_server` 與轉發兩處）。
#      ⇒ 兩邊**都不配 `encode`**，且 payload 不可壓縮 ⇒ 這一格對兩邊都不起作用。
#      ⛔ 引用差值時別把這句話丟掉。
#
# ⚠ ⚠ **payload 必須是可列印的**：`respond` 的正文**只有內聯形態**
#   （⛔ 沒有 file 形式）⇒ 用 base64 字母表（不含 `|`、`&`、反斜槓與引號
#   ⇒ `sed` 替換與 DSL 的引號都不會被它撐破）。
#   ⛔ 這與靜態吞吐那一類的 `/dev/urandom` **不是同一份位元組** ——
#   ★ 但那不影響本步：本步比的是**同一趟裡的兩個變體**，兩邊用的是同一份可列印位元組。
#   ⛔ 也**因此不許**把本步的讀數與 `raw/static-throughput/fulcrum.json` 並排比。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ 只為拿那四個負載參數的預設（`bench/lib.sh` 的「負載參數」一節）。
#   ⛔ 不許在本檔案裡再寫一份 `:-` 預設 —— 那正是 G147 治掉的那一族。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/diag/respond-ceiling.sh <输出目录>}
NAME=respond-ceiling
DIAG_DIR="$OUT_DIR/diag/$NAME"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 這四個不帶 `:-`：預設由上面那次 `source` 供（G147 一處定義）。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 連接埠：9944–9945，在 `docs/platform/host-and-gate-traps.md` 那張表已保留的
# 9940–9949 段之內（9940–9943 歸靜態吞吐那一類）。
# ⛔ 一個都別改成 :80 —— 那個連接埠在門禁裡是共享的（G137）。
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

# ── 原語（與 bench/case/static-throughput.sh 同一套，理由見那裡的註解）──────
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

# ── payload：可列印的 N 位元組 ───────────────────────────────────────────────
#
# ⚠ ⚠ ⛔ **有意一步都不走管道**：`head -c N` 讀夠就關，上游會吃到 SIGPIPE，
#   而本檔案是 `set -o pipefail` ⇒ 寫成 `… | base64 | head -c N` 會讓整指令碼
#   在一次**完全正常**的截斷上退出。⇒ 落三個中間檔案，一步一個命令。
if [ -n "${BENCH_WWW_ROOT:-}" ]; then
  WWW="$BENCH_WWW_ROOT/$NAME-$$"
  rm -rf "$WWW"
else
  WWW="$WORK/www"
fi
mkdir -p "$WWW" "$DIAG_DIR"

# base64 把 3 位元組漲成 4 ⇒ 取 PAYLOAD_BYTES 位元組的隨機源足夠裁出 PAYLOAD_BYTES 個字元。
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

# ── 繪製兩份設定 ───────────────────────────────────────────────────────────
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

# ── 一個變體的完整一趟 ─────────────────────────────────────────────────────
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

  # ★ ★ ★ 開跑前先證明它回的**就是那份位元組**。
  #   ⚠ 判據比 static-throughput 那條更強：那邊比狀態碼與**長度**，這裡直接
  #   `cmp` 整份正文 —— 本步比的是兩個變體的差，⇒ 「兩邊回的是同一份位元組」
  #   是這個差有意義的前提，而長度相同的兩份不同正文照樣能騙過長度判據。
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

# ⚠ 順序有意固定：結果與順序無關才對，⇒ 固定它是為了讓兩趟之間可比。
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
