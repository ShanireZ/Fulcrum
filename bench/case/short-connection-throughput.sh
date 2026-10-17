#!/usr/bin/env bash
# 案例類「高並行短連接」（M3，§8 七類之一）—— 四家各從**記憶體**回同一份正文，
# 而**每個請求都是一條新連接、由伺服器端先關**。
#
#   bash bench/case/short-connection-throughput.sh <输出目录>
#
# ⛔ **本指令碼不判定、不比較、不列印任何「誰更快」。** 它只產出原始資料。
#   判定在 `bench/verdict.sh`，而那一步會先問宿主合不合格。
#
# ── 對齊口徑（★ 每一條都會被 bench/README.md 引用，改這裡要一起改）─────────────
#
#   ① **一次只起一家。**
#   ② **同一份 payload 位元組、同一個 URL 路徑**，開跑前逐位元組核對過。
#   ③ **訪問紀錄四家全關。**
#   ④ **明文 HTTP/1.1。** TLS 歸「TLS 握手」那一類，⛔ 不混進來。
#   ⑤ **worker 數四家一致**（`BENCH_WORKERS`）。
#   ⑥ ★ ★ ★ **每個請求一條新連接，且由伺服器端先關**（owner 2026-09-23 拍板）。
#      oha 帶 `--disable-keepalive` 與 `-H 'Connection: close'`（收成一個陣列 `OHA_SHORT_ARGS`）。
#      ⚠ 在 oha v1.15.0 下後者是**冗餘**的：`--disable-keepalive` 在 HTTP/1.1 時自己就插
#      `Connection: close`（`src/lib.rs:537`；使用者的 `-H` 在它之後插、能覆蓋它）。
#      ⛔ 設計時以為它不發（唯讀了 `src/client.rs`），2026-09-23 的注入反證當場推翻。
#      ★ 仍然留著那個頭：口徑寫在明處，不相依 oha 的一個隱式行為（換個版本它可能就不加了）。
#      三件事各有一道門：
#        · 每請求一條新連接 —— 判據 ⑥ ①（PassiveOpens 差值，落 `new-conns.txt`）；
#        · oha 真的帶著那個頭 —— 判據 ⑥ ②（開跑前用**同一個陣列**向合成伺服器端發一次，
#          抓到的請求原文落 `oha-request.txt`）；
#        · 四家收到那個頭會先關 —— 每家開跑前的行為探針（`bench/close-probe.py`）。
#      判據本體在 `bench/lib.sh`，串接由 `tests/bench/gate.sh` 的 C21 判。
#      ⚠ `new-conns.txt` 後兩列（TIME_WAIT 兩側條數）是**診斷**，⛔ 不判：實測誰先關是競態，
#        門禁參數下還會被 netns 的 TIME_WAIT 上限截斷（理由見 `bench_tw_split` 的註解）。
#
# ── 為什麼是「從記憶體回」（owner 2026-09-23 拍板）──────────────────────────────
#
# 這一類量的只是「建連 → 解析 → 回應 → 拆連」，⛔ 不混進檔案 IO（那條路歸靜態吞吐那一類）。
# 四家各用自己的內聯寫法，範本按**形態**命名（`*-respond*`）：
#   樞衡 `respond` · Caddy `respond` · HAProxy `http-request return … file`（啟動時讀進記憶體）·
#   ⚠ nginx 是**兩個變數拼接** —— 它讀設定的緩衝區只有 4096 位元組，一個 4096 位元組的參數
#   解析不了。代價與方向寫在 `bench/conf/nginx-respond.conf` 頭部與 bench/README.md。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ ⚠ ★ **只為拿那四個負載參數的預設與判據 ⑥ 的解析函式**（`bench/lib.sh`）。
#   ⛔ **不許在本檔案裡再寫一份 `:-` 預設** —— 理由與 static-throughput.sh 逐字相同。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/case/short-connection-throughput.sh <输出目录>}
CASE=short-connection-throughput
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 這四個**不帶 `:-`**：預設由上面那次 `source` 供。少了那一行就是 `set -u` 當場回報錯誤。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 連接埠：9956–9960（連接埠表在 docs/platform/host-and-gate-traps.md）。9960 = 抓請求頭的合成伺服器端。
# ⚠ ⛔ 一個都別改成 :80 —— 那個連接埠在門禁裡是共享的（G137）。
PORT_FULCRUM=${BENCH_PORT_SC_FULCRUM:-9956}
PORT_CADDY=${BENCH_PORT_SC_CADDY:-9957}
PORT_HAPROXY=${BENCH_PORT_SC_HAPROXY:-9958}
PORT_NGINX=${BENCH_PORT_SC_NGINX:-9959}
PORT_CAPTURE=${BENCH_PORT_SC_CAPTURE:-9960}

# ★ ★ ★ 短連接的兩個旗標**只在這裡寫一次**：負載那一行與抓請求頭那一次用的是**同一個陣列**
#   ⇒ 判據 ⑥ ② 判到的，就是負載真正帶著的。⛔ 別在負載那一行另抄一份。
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

# ── 原語 ───────────────────────────────────────────────────────────────────
#
# ★ 「起來了沒有」判在**行為**上（真連一次），⛔ 不判某個工具在不在 ——
#   `tests/stress/run.sh` 在這條上栽過：寫 `ss -lnt …` 而鏡像里根本沒有 `ss`。
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

# 本 netns 裡被動打開的 TCP 連接累計數（判據 ⑥ 第 ① 半的觀測量）。
# ★ 與反代類同一個計數器、同一個取法：逐列按**表頭**找 `PassiveOpens`，⛔ 不寫死列號。
# ★ 本類**沒有源站** ⇒ oha 跑的那段時間裡，netns 裡只有 oha→被測這一段在建連。
passive_opens() {
  awk '/^Tcp:/ {
    if (h == "") { for (i = 1; i <= NF; i++) k[i] = $i; h = 1; next }
    for (i = 1; i <= NF; i++) if (k[i] == "PassiveOpens") print $i
  }' /proc/net/snmp
}

# CPU 親和。★ 兩組核都給了才用 `taskset`；只給一個等於沒給。
pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

mkdir -p "$RAW_DIR"

# ── 探針先自證（★ 承重）──────────────────────────────────────────────────────
#
# 下面每家開跑前都要過一道「它真的會先關」的行為核對，而一個**從沒見過「不關」**的探針
# 與一個恆報「關了」的壞探針給不出任何區別 ⇒ 先用合成被測兩個方向證一遍，不過就整類不跑。
if ! python3 "$BENCH_DIR/close-probe.py" --self-check; then
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: 行为探针的自测没过 —— 它判不动，这一类不跑" >&2
  exit 1
fi

# ── payload：可列印的 N 位元組（與反代類同一手法）───────────────────────────────
#
# ⚠ 樞衡 `respond` 與 Caddy `respond` 的正文都是內聯的 ⇒ 必須**可列印**：取 base64 字母表
#   （不含 `|`、`&`、`$`、反斜槓與引號 ⇒ `sed` 替換、nginx 的變數語法、各家的引號都撐不破）。
# ⛔ 這與靜態吞吐那一類的 `/dev/urandom` **不是同一份位元組** ⇒ 兩類的絕對讀數不可並排比較。
# ⚠ base64 把 3 位元組漲成 4 ⇒ 取 PAYLOAD_BYTES 位元組的隨機源足夠裁出 PAYLOAD_BYTES 個字元。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WORK/rand.bin"
base64 -w0 < "$WORK/rand.bin" > "$WORK/b64.txt"
head -c "$PAYLOAD_BYTES" "$WORK/b64.txt" > "$WORK/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WORK/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
BODY=$(cat "$WORK/payload.bin")
# ⚠ nginx 那一家要兩半（見 bench/conf/nginx-respond.conf 頭部）。
HALF=$((PAYLOAD_BYTES / 2))
BODY_1=${BODY:0:$HALF}
BODY_2=${BODY:$HALF}
[ "${BODY_1}${BODY_2}" = "$BODY" ] || {
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: 两半拼回去不等于原文" >&2
  exit 1
}
# ⚠ 每一半加上兩個引號必須**小於** 4096（nginx 的 `NGX_CONF_BUFFER`），否則它當場起不來 ——
#   ⛔ 在這裡先說清楚，別讓它變成一句落在 nginx 紀錄裡的 `too long parameter`。
if [ $((PAYLOAD_BYTES - HALF + 2)) -ge 4096 ]; then
  echo "SHORT-CONNECTION-THROUGHPUT FAILED: payload $PAYLOAD_BYTES 字节拆成两半仍放不进 nginx 那块 4096 字节的配置缓冲区 —— 要拆成更多段" >&2
  exit 1
fi
URL_PATH=/payload.bin

# ── 繪製四家的設定 ─────────────────────────────────────────────────────────
#
# ★ 範本在 `bench/conf/` 下、是**交付物的一部分**，這裡只替換佔位符。
# ⚠ `__BODY_1__` / `__BODY_2__` 排在 `__BODY__` 之前：後者不是前兩者的前綴
#   （`__BODY__` 要求緊跟兩個下劃線），但排在前面讓「誰先被替換」不需要人去想。
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

# ── 判據 ⑥ ② 的觀測量：oha 真正發出的那個請求 ──────────────────────────────────
#
# ★ 用負載那一組參數（`OHA_SHORT_ARGS`，同一個陣列）向一個只記請求頭的合成伺服器端發**一次**，
#   把它收到的請求頭原文寫入磁碟；判定在 `bench/lib.sh` 的 `bench_request_close_violations`。
# ⚠ 合成伺服器端會跳過 `wait_port` 那一下空連接、只存第一條帶完整請求頭的連接。
# ⚠ ⚠ ⛔ 寫入磁碟檔案**有意不叫 `.json`**（`read-raw.py` 會把它當成第五家被測）。
# ⛔ 這一步只採讀數、不判定：沒抓到時檔案為空，由判據當「判不了」算違規 —— 與另外幾格同一個分工。
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
# 起 → 等連接埠 → **核對它回的是不是那份 payload** → **核對它真的會先關** →
# 打負載（前後各讀一次 PassiveOpens，跑完讀 TIME_WAIT）→ 收 → 等連接埠消失。
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

  # ★ ★ ★ **開跑前先證明樣本里真有東西**：狀態碼、位元組數、**逐位元組**都要對上。
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

  # ★ ★ ★ **再證明它真的會先關**：判在行為上（限時內讀到 EOF），⛔ 不判回應頭。
  local close_rc=0
  python3 "$BENCH_DIR/close-probe.py" "$HOST" "$port" "$URL_PATH" || close_rc=$?
  if [ "$close_rc" != 0 ]; then
    bad "$name 收到 Connection: close 之后没有先关连接（close-probe 退 $close_rc；3 = 限时内没关，4 = 不是 200，1 = 连不上）"
    stop_child
    return 0
  fi
  ok "$name 收到 Connection: close 之后先关了连接（行为核对，⛔ 不是看响应头）"

  # ── 負載 + 判據 ⑥ 的兩個觀測量 ───────────────────────────────────────────
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
  # ★ 診斷列（⛔ 不判）：跑完那一刻這一家連接埠上的 TIME_WAIT 落在哪一側。每家連接埠不同
  #   ⇒ 前一家留下的不會算到這一家頭上；⚠ 但 netns 的 TIME_WAIT 表是**共享**的，前幾家把它
  #   佔滿時後幾家會讀到 `0 0`（門禁參數下實測就是這樣）⇒ 讀它之前先看 `bench_tw_split` 的註解。
  tw=$(bench_tw_split "$port" "$(cat /proc/net/tcp /proc/net/tcp6 2>/dev/null || true)")

  # ⚠ ⚠ ⛔ 觀測檔案**有意不叫 `.json`** —— `bench/read-raw.py` 對本目錄做 `glob("*.json")`，
  #   一個 `.json` 會被當成**第五家被測**。★ 與 `upstream-conns.txt` / `ceiling.txt` 同一條教訓。
  if [ -s "$RAW_DIR/$name.json" ]; then
    reqs=$(python3 -c 'import json,sys;print(sum(json.load(open(sys.argv[1]))["statusCodeDistribution"].values()))' \
      "$RAW_DIR/$name.json" 2>/dev/null || echo 0)
  else
    reqs=0
  fi
  if [ "$reqs" -gt 0 ] 2>/dev/null; then
    per1k=$(awk -v o="$opened" -v r="$reqs" 'BEGIN { printf "%.2f", 1000.0 * o / r }')
  else
    # ⛔ 「算不出來」一律記成判據認得的壞值，⛔ 而不是省略這一行 —— 少判與判過在輸出上分不開。
    per1k=NaN
  fi
  printf '%s %s %s\n' "$name" "$per1k" "$tw" >> "$CONNS_TXT"
  echo "  · $name 本趟新建连接 $opened / 请求 $reqs ⇒ 每千请求 $per1k 条；TIME_WAIT 服务端 / 客户端 = $tw（诊断）"

  stop_child
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一家会起不来"
}

# ⚠ 順序有意固定：結果與順序無關才對，⇒ 固定它是為了讓兩趟之間可比。
run_subject fulcrum "$PORT_FULCRUM" \
  "$FULCRUM_BIN" serve "$WORK/fulcrum.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/fulcrum.pid" \
  --upgrade-sock "$WORK/fulcrum.sock" \
  --state-dir "$WORK/state"

# ★ `GOMAXPROCS` 是 Caddy 唯一的並行度旋鈕（它是 Go 寫的，沒有 worker 處理程序的概念）。
GOMAXPROCS="$WORKERS" \
  run_subject caddy "$PORT_CADDY" \
  caddy run --config "$WORK/Caddyfile" --adapter caddyfile

# ★ `-db` = 不 daemon 化、不做後台化，編排指令碼才拿得到它的 pid。
run_subject haproxy "$PORT_HAPROXY" \
  haproxy -f "$WORK/haproxy.cfg" -db

# ★ nginx 的 `daemon off` 寫在設定裡（見 bench/conf/nginx-respond.conf）。
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
