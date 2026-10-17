#!/usr/bin/env bash
# 用例類「靜態吞吐」——四家各服務**同一個固定資源**，同一個負載生成器打。
#
#   bash bench/case/static-throughput.sh <输出目录>
#
# ⛔ **本指令碼不判定、不比較、不列印任何「誰更快」。** 它只產出原始資料。
#   判定在 `bench/verdict.sh`，而那一步會先問宿主合不合格。
#   ★ 這條分工不是潔癖：把「量」和「判」寫在一起，就沒有辦法在不合格的宿主上
#     跑完整條流水線而結構性地不出結論 —— 而那正是 G132 要的東西。
#
# ── 對齊口徑（★ 每一條都會被 bench/README.md 引用，改這裡要一起改）─────────────
#
#   ① **一次只起一家。** 四家同時在跑會互相搶 CPU，量到的是「四家一起跑」這件事。
#   ② **同一個 payload、同一個 URL 路徑、同一個位元組數**，且開跑前逐位元組核對過。
#   ③ **訪問紀錄四家全關。** 紀錄是另一件事，開著會把它的代價算進吞吐裡。
#   ④ **明文 HTTP/1.1 + keep-alive。** TLS 歸「TLS 握手」那一類，⛔ 不混進來。
#   ⑤ **worker 數四家一致**（`BENCH_WORKERS`，預設 1）。四家各自的旋鈕：
#      樞衡 `threads_l7` · nginx `worker_processes` · HAProxy `nbthread` · Caddy `GOMAXPROCS`。
#      ⚠ ⚠ ★ **樞衡那個旋鈕是這一刀跑通之後才有的** —— 2026-09-05 之前它走
#      pingora `ServerConf::default()`（`threads: 1`），DSL 與 CLI 都沒有調它的口子
#      ⇒ 四家當時唯一可對齊的值只有 1。那是 D33，已由 G140 結案。
#      ★ 這條正是「第一刀的價值在於跑通一遍」的活樣本：缺的旋鈕不是想出來的，是撞出來的。
#      ⚠ 預設仍取 1：它與機器無關 ⇒ 門禁裡那一趟在任何機器上都一樣。
#      ⛔ 真要出數那天**必須顯式設它**（按那臺合格宿主的 CPU 分配），
#      因為「四家都跑各自的預設」與「四家跑同一個 worker 數」是兩個不同的實驗。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ ⚠ ★ **只為拿那四個負載參數的預設**（`bench/lib.sh` 的「負載參數」一節）。
#   ⛔ **不許在本檔案裡再寫一份 `:-` 預設** —— 2026-09-06 之前正是那樣：
#   用例用「宣告值或預設」，而 `env-snapshot.sh` 記「宣告值」
#   ⇒ 沒人顯式設的參數，用例真的用了預設而快照寫 `null`
#   （那一趟的 `payload_bytes` 與門禁裡的 `workers` 都是這麼來的）。
#   ⇒ 預設一處定義、兩邊都從這裡取，那一族缺陷就**結構性地不存在**。
# ★ `lib.sh` 被 `source` 時什麼都不做（它自己那道 `BASH_SOURCE` 判斷），
#   ⇒ 這裡拿到的只有變數與函式，⛔ 沒有任何副作用。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/case/static-throughput.sh <输出目录>}
CASE=static-throughput
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ── 參數（★ 它們是口徑的一部分，會被寫進環境快照）────────────────────────────
# ⛔ 這四個**不帶 `:-`**：預設由上面那次 `source` 供。少了那一行就是 `set -u`
#   當場回報錯誤，⛔ 而不是安靜地回落到一個只有本檔案知道的數。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 連接埠：9940–9943（連接埠表在 docs/platform/host-and-gate-traps.md）。
#
# ★ 嚴格說本格**不與任何場景爭連接埠** —— 它跑在自己的容器裡，netns 是獨立的。
#   仍然登記進那張表、仍然挑一段沒人用的，是因為**「今天不衝突」不是一個能靠的性質**：
#   哪天有人給這個容器加上 `--net=host`，衝突會立刻出現而沒有任何東西會說。
#
# ⚠ ⚠ ⛔ **初稿寫的是 9700–9703，那是 `tests/h3/` 已經佔著的一段。**
#   寫下「9700–9703 是空段」那句話時，用的是一條自己拼的枚舉命令，而它的
#   字元類是 `[A-Z_]+` —— **匹配不到 `${H3_PORT:-9700}` 裡的那個數字 `3`**。
#   ⇒ 它安靜地少列了一整格，而輸出看起來完整。★ 教訓與本儲存庫 §3.1 同族：
#   **自查用的篩子必須比被查的東西寬**；這裡正確的做法是直接讀那張表，⛔ 不是自己拼一條 grep。
#
# ⚠ ⛔ **一個都別改成 :80** —— 那個連接埠在門禁裡是共享的（G137），佔住它會讓一個
#   與本用例毫無關係的場景起不來，而回報錯誤落在那個無辜的連接埠上。
PORT_FULCRUM=${BENCH_PORT_FULCRUM:-9940}
PORT_CADDY=${BENCH_PORT_CADDY:-9941}
PORT_HAPROXY=${BENCH_PORT_HAPROXY:-9942}
PORT_NGINX=${BENCH_PORT_NGINX:-9943}

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
  # ⚠ `$WWW` 可能落在一個**跨趟存活**的命名捲上（`BENCH_WWW_ROOT`）⇒ 單獨刪。
  #   ⛔ `${WWW:-}` 那層守衛是承重的：`set -u` 下，在 `WWW` 賦值之前退出（比如
  #   payload 位元組數對不上）會讓這裡 `unbound variable`，而那會把一條清楚的
  #   失敗訊息換成一句 bash 的錯。
  [ -z "${WWW:-}" ] || rm -rf "$WWW"
}
trap cleanup EXIT

# ── 原語 ───────────────────────────────────────────────────────────────────
#
# ★ 「起來了沒有」判在**行為**上（真連一次），⛔ 不判某個工具在不在 ——
#   `tests/stress/run.sh` 在這條上栽過：寫 `ss -lnt …` 而鏡像里根本沒有 `ss`，
#   於是那個原語**恆回傳 false**，一個方向永遠成立、另一個方向永遠超時。
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

# CPU 親和。★ 兩組核都給了才用 `taskset`；只給一個等於沒給（見 env-snapshot.sh）。
# ⚠ 用**陣列**展開，⛔ 不要拼一個字串再讓它被詞分割 —— 那條路在沒設親和時
#   會展開成一個空詞，`command` 拿到一個空 argv[0] 而報「command not found」。
pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

# ── 準備 payload ───────────────────────────────────────────────────────────
#
# ★ ★ ★ **站台根落在哪種檔案系統上是口徑的一部分**，⛔ 不是實作細節：
#   樞衡的靜態檔案路徑先在本執行緒上試一次 `preadv2(RWF_NOWAIT)`，而**認不認由
#   檔案系統各自決定** —— 2026-09-06 實測 ext4 認、**overlayfs 與 tmpfs 回
#   `EOPNOTSUPP`**，同一個二進位、同一臺機器差 **45%**。
#   ⚠ ⚠ 而 `mktemp -d` 落在容器可寫層（overlayfs）上 —— 那是**編排的副產品，
#   從來沒人挑過**，卻決定了被測的哪條路徑會被走到。
# ⇒ `BENCH_WWW_ROOT` 由 `bench/docker-run.sh` 指到一個命名卷（ext4）。
# ⚠ ⛔ **預設仍然回落到 `$WORK`，而這不是「悄悄回落」** —— 實際落在哪種檔案系統上
#   由 `bench/env-snapshot.sh` 探出來寫進 `env.json` 的 `site_root` 裡
#   ⇒ 每一份原始資料都說得出自己是在哪種形狀下量的。
if [ -n "${BENCH_WWW_ROOT:-}" ]; then
  # ⚠ 命名卷**跨趟存活** ⇒ 每趟用自己的子目錄並在收尾時刪掉，
  #   ⛔ 否則上一趟的 payload 會留在那裡，而它與這一趟的可能不是同一份位元組。
  WWW="$BENCH_WWW_ROOT/static-throughput-$$"
  rm -rf "$WWW"
else
  WWW="$WORK/www"
fi
mkdir -p "$WWW" "$RAW_DIR"
# ★ 用 `/dev/urandom` 而不是可壓縮的重複位元組：四家的預設壓縮策略不同，
#   一個高度可壓縮的 payload 會把「壓不壓縮」的差異混進吞吐裡。本類**不比壓縮**。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WWW/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WWW/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "STATIC-THROUGHPUT FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
URL_PATH=/payload.bin

# ── 繪製四家的設定 ─────────────────────────────────────────────────────────
#
# ★ 範本在 `bench/conf/` 下、是**交付物的一部分**，這裡只替換佔位符。
#   ⛔ 別把設定內聯進本指令碼：第三方要看的就是那四份設定本身。
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WWW_ROOT__|$WWW|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__RUN_DIR__|$WORK|g" \
    -e "s|__PAYLOAD__|$WWW/payload.bin|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum.Fulcrumfile" "$WORK/fulcrum.Fulcrumfile" "$PORT_FULCRUM"
render "$BENCH_DIR/conf/Caddyfile" "$WORK/Caddyfile" "$PORT_CADDY"
render "$BENCH_DIR/conf/haproxy.cfg" "$WORK/haproxy.cfg" "$PORT_HAPROXY"
render "$BENCH_DIR/conf/nginx.conf" "$WORK/nginx.conf" "$PORT_NGINX"
mkdir -p "$WORK/nginx-body"

# ── 一家的完整一趟 ─────────────────────────────────────────────────────────
#
# 起 → 等連接埠 → **核對它回的到底是不是那個資源** → 打負載 → 收 → 等連接埠消失。
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

  # ★ ★ ★ **開跑前先證明樣本里真有東西。** 一個穩定回 404 的被測會給出一個
  #   非常漂亮的 RPS，而那個數與「服務靜態資源」毫無關係 —— 它量的是回 404 有多快。
  #   ⇒ 狀態碼與**位元組數**都要對上，⛔ 只判狀態碼不夠（回 200 空 body 同樣很快）。
  local probe code size
  probe=$(curl -sS -o /dev/null -w '%{http_code} %{size_download}' \
    "http://$HOST:$port$URL_PATH" 2>>"$log" || echo "000 0")
  code=${probe%% *}
  size=${probe##* }
  if [ "$code" != "200" ] || [ "$size" != "$PAYLOAD_BYTES" ]; then
    bad "$name 开跑前的核对没过：HTTP $code，body $size 字节（该是 200 / $PAYLOAD_BYTES）"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi
  ok "$name 回的是那个资源（HTTP 200，$size 字节）"

  # 打負載。⚠ `--no-tui`：沒有 TTY 時 oha 的交互介面會把輸出弄成一團。
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

# ★ nginx 的 `daemon off` 寫在設定裡（見 bench/conf/nginx.conf）。
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
