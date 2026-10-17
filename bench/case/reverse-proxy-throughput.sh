#!/usr/bin/env bash
# 案例類「反代吞吐」（M3，§8 七類之二）——四家各作反向代理，轉發到**同一個常駐源站**。
#
#   bash bench/case/reverse-proxy-throughput.sh <输出目录>
#
# ⛔ **本指令碼不判定、不比較、不列印任何「誰更快」。** 它只產出原始資料。
#   判定在 `bench/verdict.sh`，而那一步會先問宿主合不合格。
#
# ── 對齊口徑（★ 每一條都會被 bench/README.md 引用，改這裡要一起改）─────────────
#
#   ① **一次只起一家**（源站除外，它常駐 —— 見下）。
#   ② **同一個源站、同一個 URL 路徑、同一份 payload 位元組**，開跑前逐位元組核對過。
#   ③ **訪問紀錄四家全關。**
#   ④ **明文 HTTP/1.1 + keep-alive。** TLS 歸「TLS 握手」那一類，⛔ 不混進來。
#   ⑤ **worker 數四家一致**（`BENCH_WORKERS`）——★ **源站也吃同一個值**。
#   ⑥ ★ ★ ★ **上游連接重複使用四家一致。** 四家的**預設**行為不同，而 oha 的 JSON 裡
#      **看不出這件事** —— 輸出一切正常，只是某一家的數被系統性壓低。
#      ⇒ 四家全部顯式設定（逐家的理由寫在各自的設定檔案裡），並由本指令碼產出的
#      `upstream-conns.txt` 交給 `tests/bench/gate.sh` 的 C20 第 ② 半判著。
#
# ── 源站為什麼是樞衡自己的 `respond` ─────────────────────────────────────────
#
# ⚠ 與 `diag/cache-hit-p99.sh` 那個源站**性質完全不同**：那裡命中之後源站根本
#   不被訪問（測量視窗裡的 CPU 成本 ≈ 0），而**這裡每一個請求都要穿過它**。
#
# ★ 鏡像裡能當源站的只有四家自己與 python3（`docker/Dockerfile.bench`）
#   ⇒ 加第五個實作要改鏡像，**撞 G36**（`G152` 把它的 M3 凍結擴到了對拍鏡像）。
#   ⇒ **無論選誰，都必然有一趟是「同二進位兩執行個體」**。選樞衡 ⇒ 受損的是
#   樞衡自己那一趟：**誤差方向對我們不利** ⇒ PASS 更可信，⛔ FAIL 不許拿它辯解。
#   （選 haproxy 則受損的是很可能的最強者 ⇒ 門檻被低估 ⇒ PASS 不可信 ——
#    那正是判據 ④C／`G146` 在治的那個失效。）
#
# ⚠ **樞衡 `file_server` 當時出局**的理由（2026-09-06 靜態吞吐 7638.6 rps ⇒ 四家會一起被它壓住
#   ⇒ 判據 ④B 收斂作廢整類）按視窗二的數（19811.2，已高於這一類的最強者）**已不成立**；
#   選 `respond` 今天只靠上面那條「誤差方向」站著 —— 見 bench/README.md「反代吞吐」一節。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ ⚠ ★ **只為拿那四個負載參數的預設**（`bench/lib.sh` 的「負載參數」一節）。
#   ⛔ **不許在本檔案裡再寫一份 `:-` 預設** —— 理由與 static-throughput.sh 逐字相同：
#   案例用「宣告值或預設」而快照記「宣告值」，會讓沒人顯式設的參數在快照裡成 `null`。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/case/reverse-proxy-throughput.sh <输出目录>}
CASE=reverse-proxy-throughput
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 這四個**不帶 `:-`**：預設由上面那次 `source` 供。少了那一行就是 `set -u`
#   當場回報錯誤，⛔ 而不是安靜地回落到一個只有本檔案知道的數。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 連接埠：9949–9953（連接埠表在 docs/platform/host-and-gate-traps.md）。
# ⚠ ⛔ 一個都別改成 :80 —— 那個連接埠在門禁裡是共享的（G137），佔住它會讓一個
#   與本案例毫無關係的場景起不來，而回報錯誤落在那個無辜的連接埠上。
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

# ── 原語 ───────────────────────────────────────────────────────────────────
#
# ★ 「起來了沒有」判在**行為**上（真連一次），⛔ 不判某個工具在不在 ——
#   `tests/stress/run.sh` 在這條上栽過：寫 `ss -lnt …` 而鏡像里根本沒有 `ss`，
#   於是那個原語**恆回傳 false**。
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

# 本 netns 裡被動打開的 TCP 連接累計數（對齊口徑第 ⑥ 條的觀測量）。
#
# ⚠ ⚠ 它是**整個 netns 的**，⛔ 不是只有「反代→源站」那一段（還含 oha→反代 那部分）。
#   ★ 它夠用**不是因為它精確，而是因為對齊與不對齊之間隔著兩個數量級**：
#     開發機 10s/50 連接實測，對齊後 0.4–2.1，未對齊 100.3（caddy）與 1001.5（nginx）。
# ⚠ 逐列按**表頭**找 `PassiveOpens`，⛔ 不寫死列號：不同核心的 `/proc/net/snmp`
#   列數不同，而一個寫死的列號會安靜地讀出另一個計數器。
passive_opens() {
  awk '/^Tcp:/ {
    if (h == "") { for (i = 1; i <= NF; i++) k[i] = $i; h = 1; next }
    for (i = 1; i <= NF; i++) if (k[i] == "PassiveOpens") print $i
  }' /proc/net/snmp
}

# CPU 親和。★ 兩組核都給了才用 `taskset`；只給一個等於沒給。
# ⚠ ⚠ **源站與被測共享 `BENCH_SERVER_CPUS`**（owner 2026-09-09 拍板）：合格宿主
#   只有 4 核，分三組會讓 oha 只剩 1 核去打四萬多 rps ⇒ 很可能**壓測端先飽和**，
#   而那正是判據 ④B 會作廢整類的情形。
#   ⇒ 代價是源站吃掉的 CPU 混進四家讀數，★ 但它對四家一視同仁 ⇒ 類內的相對
#   比較（G19 那 10%）仍然成立。這一條寫在 bench/README.md 的明處。
pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

mkdir -p "$RAW_DIR"

# ── payload：可列印的 N 位元組 ───────────────────────────────────────────────
#
# ⚠ ⚠ `respond` 的正文**只有內聯形態**（⛔ 沒有 file 形式，不像 HAProxy 那條
#   `http-request return … file <路径>`）⇒ payload 必須**可列印**。取 base64
#   字母表（不含 `|`、`&`、反斜槓與引號）—— 與 diag/respond-ceiling.sh 同一手法。
# ⛔ 這與靜態吞吐那一類的 `/dev/urandom` **不是同一份位元組** ⇒ 兩類的絕對讀數
#   不可並排比較（又一條，寫在 bench/README.md）。
# ⚠ base64 把 3 位元組漲成 4 ⇒ 取 PAYLOAD_BYTES 位元組的隨機源足夠裁出 PAYLOAD_BYTES 個字元。
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

# ── 繪製設定 ───────────────────────────────────────────────────────────────
#
# ★ 範本在 `bench/conf/` 下、是**交付物的一部分**，這裡只替換佔位符。
#   ⛔ 別把設定內聯進本指令碼：第三方要看的就是那幾份設定本身。
# ★ ★ caddy / nginx 的上游空閒連接上限跟著並行數走（`bench/lib.sh` 的
#   `bench_upstream_idle_cap`，2026-09-10）—— 寫死 64 時視窗的 200 並行下兩家部分不復用。
#   算不出來就在 `set -e` 下當場失敗，⛔ 不回落到一個只有本檔案知道的數。
IDLE_CAP=$(bench_upstream_idle_cap "$CONNECTIONS")
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__RUN_DIR__|$WORK|g" \
    -e "s|__ORIGIN_HOST__|$HOST|g" \
    -e "s|__ORIGIN_PORT__|$PORT_ORIGIN|g" \
    -e "s|__IDLE_CAP__|$IDLE_CAP|g" \
    "$src" > "$dst"
}
# 源站：與 diag/respond-ceiling.sh **共用同一份範本**（見那份檔案頭部的說明）。
sed -e "s|__PORT__|$PORT_ORIGIN|g" \
  -e "s|__WORKERS__|$WORKERS|g" \
  -e "s|__BODY__|$BODY|g" \
  "$BENCH_DIR/conf/fulcrum-respond.Fulcrumfile" > "$WORK/origin.Fulcrumfile"

render "$BENCH_DIR/conf/fulcrum-proxy.Fulcrumfile" "$WORK/fulcrum.Fulcrumfile" "$PORT_FULCRUM"
render "$BENCH_DIR/conf/Caddyfile-proxy" "$WORK/Caddyfile" "$PORT_CADDY"
render "$BENCH_DIR/conf/haproxy-proxy.cfg" "$WORK/haproxy.cfg" "$PORT_HAPROXY"
render "$BENCH_DIR/conf/nginx-proxy.conf" "$WORK/nginx.conf" "$PORT_NGINX"
mkdir -p "$WORK/nginx-body"

# ── 源站：常駐，跨四趟 ─────────────────────────────────────────────────────
#
# ★ 它是**環境**不是被測 ⇒ 不跟著四家一起起停：每趟重起會把源站的預熱狀態
#   變成一個新變數，而四家面對的就不再是同一個上游了。
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
# 核源站還活著 → 起 → 等連接埠 → **核對它轉發的到底是不是那份 payload** →
# 打負載（前後各讀一次 PassiveOpens）→ 收 → 等連接埠消失。
run_subject() {
  local name=$1 port=$2
  shift 2
  local log="$WORK/$name.log"

  echo "── $name ──"

  # ★ ★ 每趟開跑前先確認**源站還活著** —— 它常駐跨四趟，中途死掉會讓後面幾家
  #   一起變成 502，而 `read-raw.py` 會把它們判成 INVALID（那是對的），
  #   但現場看不出真因在源站身上。
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

  # ★ ★ ★ **開跑前先證明樣本里真有東西，且它真的穿過了源站。**
  #   一個穩定回 404、或回 200 空 body 的被測會給出非常漂亮的 RPS，
  #   而那個數與「轉發那個資源」毫無關係。⇒ 狀態碼、位元組數、**逐位元組**都要對上。
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

  # ── 負載 + 上游連接重複使用的觀測量 ─────────────────────────────────────────
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

  # ⚠ ⚠ ⛔ **有意不叫 `.json`** —— `bench/read-raw.py` 對本目錄做 `glob("*.json")`，
  #   一個 `.json` 會被它當成**第五家被測**混進競品集合，於是「最強者」可能變成它，
  #   而**不會有任何東西說**。★ 與 `ceiling.txt` 逐字同一條教訓（bench/verdict.sh）。
  if [ -s "$RAW_DIR/$name.json" ]; then
    reqs=$(python3 -c 'import json,sys;print(sum(json.load(open(sys.argv[1]))["statusCodeDistribution"].values()))' \
      "$RAW_DIR/$name.json" 2>/dev/null || echo 0)
  else
    reqs=0
  fi
  if [ "$reqs" -gt 0 ] 2>/dev/null; then
    per1k=$(awk -v o="$opened" -v r="$reqs" 'BEGIN { printf "%.2f", 1000.0 * o / r }')
  else
    # ⛔ 「算不出來」一律記成一個判據認得的壞值，⛔ 而不是省略這一行 ——
    #   缺一行會讓判據少判一家，而**少判與判過在輸出上分不開**。
    per1k=NaN
  fi
  printf '%s %s\n' "$name" "$per1k" >> "$CONNS_TXT"
  echo "  · $name 本趟新建连接 $opened / 请求 $reqs ⇒ 每千请求 $per1k 条"

  stop_one "$CHILD"
  CHILD=
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

# ★ nginx 的 `daemon off` 寫在設定裡（見 bench/conf/nginx-proxy.conf）。
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
