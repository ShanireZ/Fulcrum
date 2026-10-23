#!/usr/bin/env bash
# 案例類「TLS 握手」（M3，§8 七類之一）—— 「HTTPS 版的高並行短連接」（owner 2026-09-24 原話）：
# 四家各從**記憶體**回同一份正文，**每個請求一條新連接、每條連接一次完整 TLS 1.3 握手**。
#
#   bash bench/case/tls-handshake.sh <輸出目錄>
#
# ⛔ **本指令碼不判定、不比較、不列印任何「誰更快」。** 它只產出原始資料。判定在 `bench/verdict.sh`。
#
# ── 口徑（owner 2026-09-24 拍、2026-10-23 補拍組；★ 每一條都被 bench/README.md 引用，改這裡要一起改）──────
#
#   ① 每條連接都**完整握手**：四家**伺服器端統一關掉**工作階段重複使用 —— ⛔ 不相依 oha 的預設行為
#      （oha 1.15.0 實測從不帶 pre_shared_key，但那是它今天的行為，不是口徑）。
#      「關掉」= **一張 session ticket 都不發**：只做到「發了也用不上」的那一家，每次握手照樣多做簽發 ticket 的工作。
#   ② 證書 **ECDSA P-256**，每趟現造一張自簽葉證書（無中間證書），四家同一對檔案。
#   ③ 協定 **TLS 1.3**：⛔ 不在伺服器端限版本（樞衡沒有那個旋鈕，只限另外三家就不對等），由探針核協商出來的是 1.3。
#   ④ 金鑰交換組四家**顯式寫成同一個**：`BENCH_TLS_GROUP`（`X25519MLKEM768`，見 bench/lib.sh）。
#   ⑤ 按**每秒握手數**判：判據 ⑥ ① 證了「一請求一條新連接」⇒ oha 的每秒請求數就是每秒握手數，
#      交給既有的 `verdict.sh`（越高越好，⛔ 不加方向參數）。
#   ⑥ 其餘照「高並行短連接」那一類：同一份 payload、訪問紀錄全關、worker 數一致、伺服器端先關（判據 ⑥ 全套）。
#   ⑦ URL 走主機名 `bench-tls.test`（oha `--connect-to` 連到 127.0.0.1）⇒ 用戶端帶 SNI，四家都走正常的按 SNI 挑證書那條路。
#
# ── 開跑前的握手探針（★ 承重）────────────────────────────────────────────────────
#
# 每家開跑前用鏡像裡的 `openssl s_client -trace` 連兩次（第二次帶第一次的 session），摘要落 `raw/<類>/probe-<名字>.txt`，
# 判據 `bench_tls_probe_violations`：TLSv1.3 · 協商出 `BENCH_TLS_GROUP` · 沒有 HelloRetryRequest · **0 張 ticket** · 第二次是新握手。
# ★ owner 2026-10-23 拍：**探針不過 ⇒ 只作廢這一類** —— 那一家不跑負載、⛔ 不計入失敗（本指令碼照樣退 0，
#   `bench/run.sh` 的 `set -e` 不會把後面的類一起停掉），`verdict.sh` 讀到不過的探針就把這一類判成 VOID 並寫明原因。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/case/tls-handshake.sh <输出目录>}
CASE=tls-handshake
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1
NAME=bench-tls.test

# ⛔ 這幾個**不帶 `:-`**：預設由上面那次 `source` 供（理由與 static-throughput.sh 逐字相同）。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
TLS_GROUP=$BENCH_TLS_GROUP
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 連接埠：9961–9965（連接埠表在 docs/platform/host-and-gate-traps.md）。9965 = 抓請求頭的合成伺服器端。
PORT_FULCRUM=${BENCH_PORT_TLS_FULCRUM:-9961}
PORT_CADDY=${BENCH_PORT_TLS_CADDY:-9962}
PORT_HAPROXY=${BENCH_PORT_TLS_HAPROXY:-9963}
PORT_NGINX=${BENCH_PORT_TLS_NGINX:-9964}
PORT_CAPTURE=${BENCH_PORT_TLS_CAPTURE:-9965}

# ★ 短連接的兩個旗標**只在這裡寫一次**，負載與抓請求頭用同一個陣列（判據 ⑥ ②）。
#   TLS 那兩個（`--insecure` 自簽證書、`--connect-to` 帶 SNI）與請求頭無關，按連接埠在負載那一行補。
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

# 本 netns 裡被動打開的 TCP 連接累計數（判據 ⑥ ① 的觀測量，與短連接那一類同一個取法）。
passive_opens() {
  awk '/^Tcp:/ {
    if (h == "") { for (i = 1; i <= NF; i++) k[i] = $i; h = 1; next }
    for (i = 1; i <= NF; i++) if (k[i] == "PassiveOpens") print $i
  }' /proc/net/snmp
}

pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

mkdir -p "$RAW_DIR"

# ── 探針先自證（★ 承重）──────────────────────────────────────────────────────
if ! python3 "$BENCH_DIR/close-probe.py" --self-check; then
  echo "TLS-HANDSHAKE FAILED: 行为探针的自测没过 —— 它判不动，这一类不跑" >&2
  exit 1
fi

# ── 證書（②）與 payload（與短連接那一類同一手法）──────────────────────────────
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=$NAME" -addext "subjectAltName=DNS:$NAME" >/dev/null 2>&1 || {
  echo "TLS-HANDSHAKE FAILED: openssl 生成自签证书失败" >&2
  exit 1
}
cat "$WORK/tls.crt" "$WORK/tls.key" > "$WORK/tls.pem"
# ★ HAProxy 那一個處理程序專用的 OpenSSL 設定：只多一條 `NumTickets = 0`（見 bench/conf/haproxy-tls.cfg 頭部）。
printf 'openssl_conf = oc\n[oc]\nssl_conf = sc\n[sc]\nsystem_default = sd\n[sd]\nNumTickets = 0\n' \
  > "$WORK/haproxy-openssl.cnf"

head -c "$PAYLOAD_BYTES" /dev/urandom > "$WORK/rand.bin"
base64 -w0 < "$WORK/rand.bin" > "$WORK/b64.txt"
head -c "$PAYLOAD_BYTES" "$WORK/b64.txt" > "$WORK/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WORK/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "TLS-HANDSHAKE FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
BODY=$(cat "$WORK/payload.bin")
HALF=$((PAYLOAD_BYTES / 2))
BODY_1=${BODY:0:$HALF}
BODY_2=${BODY:$HALF}
[ "${BODY_1}${BODY_2}" = "$BODY" ] || {
  echo "TLS-HANDSHAKE FAILED: 两半拼回去不等于原文" >&2
  exit 1
}
if [ $((PAYLOAD_BYTES - HALF + 2)) -ge 4096 ]; then
  echo "TLS-HANDSHAKE FAILED: payload $PAYLOAD_BYTES 字节拆成两半仍放不进 nginx 那块 4096 字节的配置缓冲区 —— 要拆成更多段" >&2
  exit 1
fi
URL_PATH=/payload.bin

render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__RUN_DIR__|$WORK|g" \
    -e "s|__PAYLOAD__|$WORK/payload.bin|g" \
    -e "s|__NAME__|$NAME|g" \
    -e "s|__CERT__|$WORK/tls.crt|g" \
    -e "s|__KEY__|$WORK/tls.key|g" \
    -e "s|__PEM__|$WORK/tls.pem|g" \
    -e "s|__TLS_GROUP_LOWER__|$(printf '%s' "$TLS_GROUP" | tr '[:upper:]' '[:lower:]')|g" \
    -e "s|__TLS_GROUP__|$TLS_GROUP|g" \
    -e "s|__BODY_1__|$BODY_1|g" \
    -e "s|__BODY_2__|$BODY_2|g" \
    -e "s|__BODY__|$BODY|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum-tls.Fulcrumfile" "$WORK/fulcrum.Fulcrumfile" "$PORT_FULCRUM"
render "$BENCH_DIR/conf/Caddyfile-tls" "$WORK/Caddyfile" "$PORT_CADDY"
render "$BENCH_DIR/conf/haproxy-tls.cfg" "$WORK/haproxy.cfg" "$PORT_HAPROXY"
render "$BENCH_DIR/conf/nginx-tls.conf" "$WORK/nginx.conf" "$PORT_NGINX"
mkdir -p "$WORK/nginx-body"

# ★ Caddy 跑 JSON：Caddyfile 關不了 session ticket（見 bench/conf/Caddyfile-tls 頭部）。
#   jq 補兩處：關 ticket；每一條連接政策（含 `caddy adapt` 自己多生的兜底 `{}`）都帶上 curves。
LOWER_GROUP=$(printf '%s' "$TLS_GROUP" | tr '[:upper:]' '[:lower:]')
if ! caddy adapt --config "$WORK/Caddyfile" --adapter caddyfile 2>"$WORK/caddy-adapt.err" \
  | jq --arg g "$LOWER_GROUP" '.apps.tls.session_tickets.disabled = true
      | .apps.http.servers |= map_values(.tls_connection_policies |= map(.curves = [$g]))' \
  > "$WORK/caddy.json"; then
  echo "TLS-HANDSHAKE FAILED: Caddyfile 转不成 JSON：$(cat "$WORK/caddy-adapt.err")" >&2
  exit 1
fi

CONNS_TXT="$RAW_DIR/new-conns.txt"
: > "$CONNS_TXT"

# ── 判據 ⑥ ② 的觀測量：oha 真正發出的那個請求（明文合成伺服器端 —— 請求頭與 TLS 無關）──────────
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
fi
CAP_PID=


# ── 一家的完整一趟 ─────────────────────────────────────────────────────────
#
# 起 → 等連接埠 → **核對 payload** → **握手探針** → **核對它真的會先關** → 打負載 → 收 → 等連接埠消失。
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

  local probe code size
  probe=$(curl -sS -k --resolve "$NAME:$port:$HOST" -o "$WORK/$name.body" -w '%{http_code} %{size_download}' \
    "https://$NAME:$port$URL_PATH" 2>>"$log" || echo "000 0")
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
  ok "$name 回的是那份 payload（HTTPS 200，$size 字节，逐字节相同）"

  # ★ ★ ★ 握手口徑：不過 ⇒ 這一家不跑負載、⛔ 不計入 FAILS（owner 2026-10-23：只作廢這一類，由 verdict.sh 判）。
  #   探針本體是 `bench_tls_probe`（bench/lib.sh），與門禁 C23 的真伺服器反證是同一個函式。
  bench_tls_probe "$HOST" "$port" "$NAME" "$URL_PATH" "$RAW_DIR/probe-$name" > "$RAW_DIR/probe-$name.txt"
  local pv
  pv=$(bench_tls_probe_violations "$TLS_GROUP" "$(cat "$RAW_DIR/probe-$name.txt")")
  if [ -n "$pv" ]; then
    echo "  ✗ $name 开跑前的握手探针不过（⇒ 这一类会被判作废，⛔ 不算这一趟失败）：" >&2
    printf '%s\n' "$pv" | sed 's/^/      · /' >&2
    stop_child
    wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一家会起不来"
    return 0
  fi
  ok "$name 的握手口径对：$(tr '\n' ' ' < "$RAW_DIR/probe-$name.txt")"

  local close_rc=0
  python3 "$BENCH_DIR/close-probe.py" --tls "$NAME" "$HOST" "$port" "$URL_PATH" || close_rc=$?
  if [ "$close_rc" != 0 ]; then
    bad "$name 收到 Connection: close 之后没有先关连接（close-probe 退 $close_rc；3 = 限时内没关，4 = 不是 200，1 = 连不上或握不上）"
    stop_child
    return 0
  fi
  ok "$name 收到 Connection: close 之后先关了连接（行为核对，走 TLS）"

  local before after opened reqs per1k tw
  before=$(passive_opens)
  if "${pin_load[@]}" oha \
    --output-format json --no-tui \
    "${OHA_SHORT_ARGS[@]}" \
    --insecure --connect-to "$NAME:$port:$HOST:$port" \
    -z "$DURATION" -c "$CONNECTIONS" \
    -o "$RAW_DIR/$name.json" \
    "https://$NAME:$port$URL_PATH" >> "$log" 2>&1; then
    ok "$name 的原始数据已落盘：raw/$CASE/$name.json"
  else
    bad "$name 那一趟负载没跑完"
    sed 's/^/      /' "$log" >&2 || true
  fi
  after=$(passive_opens)
  opened=$((after - before))
  tw=$(bench_tw_split "$port" "$(cat /proc/net/tcp /proc/net/tcp6 2>/dev/null || true)")

  if [ -s "$RAW_DIR/$name.json" ]; then
    reqs=$(python3 -c 'import json,sys;print(sum(json.load(open(sys.argv[1]))["statusCodeDistribution"].values()))' \
      "$RAW_DIR/$name.json" 2>/dev/null || echo 0)
  else
    reqs=0
  fi
  if [ "$reqs" -gt 0 ] 2>/dev/null; then
    per1k=$(awk -v o="$opened" -v r="$reqs" 'BEGIN { printf "%.2f", 1000.0 * o / r }')
  else
    per1k=NaN
  fi
  printf '%s %s %s\n' "$name" "$per1k" "$tw" >> "$CONNS_TXT"
  echo "  · $name 本趟新建连接 $opened / 请求 $reqs ⇒ 每千请求 $per1k 条（= 每千次握手）；TIME_WAIT 服务端 / 客户端 = $tw（诊断）"

  stop_child
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一家会起不来"
}

# ⚠ 順序有意固定（與另外幾類相同）。
run_subject fulcrum "$PORT_FULCRUM" \
  "$FULCRUM_BIN" serve "$WORK/fulcrum.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/fulcrum.pid" \
  --upgrade-sock "$WORK/fulcrum.sock" \
  --state-dir "$WORK/state"

GOMAXPROCS="$WORKERS" \
  run_subject caddy "$PORT_CADDY" \
  caddy run --config "$WORK/caddy.json"

# ★ `OPENSSL_CONF` 只給 HAProxy 這一個處理程序（見 bench/conf/haproxy-tls.cfg 頭部）。
OPENSSL_CONF="$WORK/haproxy-openssl.cnf" \
  run_subject haproxy "$PORT_HAPROXY" \
  haproxy -f "$WORK/haproxy.cfg" -db

run_subject nginx "$PORT_NGINX" \
  nginx -c "$WORK/nginx.conf"

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/$CASE] 跑完了，原始数据在 $RAW_DIR（握手探针不过的那几家只有 probe-*.txt，见 verdict.sh）"
  echo "[bench/$CASE] ⛔ 本步**不产出任何结论** —— 判定见 bench/verdict.sh"
else
  echo "[bench/$CASE] ★ $FAILS 处失败 —— 这一趟的原始数据不完整，⛔ 不许拿去判定" >&2
  exit 1
fi
