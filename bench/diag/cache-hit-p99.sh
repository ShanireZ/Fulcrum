#!/usr/bin/env bash
# 診斷讀數：**磁碟命中**與**記憶體命中**的 p99 差（`G150` ②）。
#
#   bash bench/diag/cache-hit-p99.sh <输出目录>
#
# ★ ★ ★ **這不是 §8 七類裡的一類，本步也不判定、不比較、不列印任何「誰更快」。**
#   §8 那七類是「與 Caddy / HAProxy / Nginx 三家對拍」；這一步只有樞衡自己，
#   量的是**同一個處理程序的兩種快取後端**之間的差 —— 磁碟命中對記憶體命中，
#   根本沒有競品。它回答 `PLAN.md` `G150` ② 那條判據：
#   「M3 對拍**必須記錄**『磁碟命中的 p99 − 記憶體命中的 p99』這一格，落進原始資料」。
#
# ⚠ ⚠ ⛔ **輸出落 `diag/`，不落 `raw/`。** `bench/verdict.sh` 是
#   `for raw_case in "$OUT_DIR"/raw/*/` —— `raw/` 下每個子目錄都會被當成一類去判，
#   而 `bench/read-raw.py` 把該目錄下每個 `*.json` 當成一家被測。只有樞衡一家
#   ⇒ 判據 ④ 會按「有效讀數不足兩條」判 `VOID`。兩個方向由 `tests/bench/gate.sh`
#   的 C18 釘著（與 `respond-ceiling` 的 C17 同形）。
#
# ⚠ ⚠ **有意不定閾值**（`G150` ② 明寫）：定一個沒有實測支撐的數字正是 `G148` ③
#   點名的那種錯。本步只把數量出來。
#
# ── 拓撲 ───────────────────────────────────────────────────────────────────
#
#   源站（file_server） ← 反代 + cache（記憶體 / 磁碟兩個變體，**一次只起一個**）
#
# ★ 為什麼必須有源站：`crates/fulcrum-server/src/lib.rs:654` 寫著「快取**只裹
#   `reverse_proxy`**」⇒ ⛔ 不能把 `cache` 掛在 `file_server` 上省掉這一層。
# ⚠ 而**命中之後源站根本不被訪問** ⇒ 它在測量視窗裡的 CPU 成本 ≈ 0。
#
# ── 對齊口徑（兩個變體之間）────────────────────────────────────────────────
#
#   ① **一次只起一個變體。** 同時跑會互相搶 CPU，量到的是「兩個一起跑」這件事。
#   ② 同一個源站處理程序、同一份 payload 位元組、同一個 URL 路徑。
#   ③ 訪問紀錄兩邊全關；明文 HTTP/1.1 + keep-alive。
#   ④ worker 數三個處理程序一致（`BENCH_WORKERS`）。
#   ⑤ 記憶體那份是**從磁碟那份刪行派生**的，⛔ 不是另寫一份（單變數由生成方式保證）。
#      ⚠ **兩處差別，如實寫在這裡**：① `disk` 那一行被刪掉（這是被測的那個變數）；
#      ② 監聽連接埠不同 —— 兩個變體雖然一次只起一個，但沿用 `respond-ceiling.sh` 的
#      兩連接埠做法，⛔ 不共用一個連接埠去賭 TIME_WAIT 與釋放時機。
#      ⇒ 「唯一差別是 disk 那一行」這句話**不成立**，別那麼寫；成立的是
#      「**語義上**唯一的變數是快取後端，連接埠那處是機械必需」。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ 只為拿那四個負載參數的預設（`bench/lib.sh` 的「負載參數」一節）。
#   ⛔ 不許在本檔案裡再寫一份 `:-` 預設 —— 那正是 G147 治掉的那一族。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/diag/cache-hit-p99.sh <输出目录>}
NAME=cache-hit-p99
DIAG_DIR="$OUT_DIR/diag/$NAME"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 這四個不帶 `:-`：預設由上面那次 `source` 供（G147 一處定義）。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 連接埠：9946–9948，在 `docs/platform/host-and-gate-traps.md` 那張表已保留的
# 9940–9949 段之內（9940–9943 歸靜態吞吐，9944–9945 歸 respond-ceiling）。
# ⛔ 一個都別改成 :80 —— 那個連接埠在門禁裡是共享的（G137）。
PORT_ORIGIN=${BENCH_PORT_CACHE_ORIGIN:-9946}
PORT_MEM=${BENCH_PORT_CACHE_MEM:-9947}
PORT_DISK=${BENCH_PORT_CACHE_DISK:-9948}

FAILS=0
CHILD=
ORIGIN_PID=
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

# ── ttl 由 BENCH_DURATION 算出來，⛔ 不寫字面值 ────────────────────────────
#
# ★ ★ 一個寫死的 `ttl` 與一個可改的 `BENCH_DURATION` 是**兩處會飄的值**，
#   而飄掉的那天沒有任何東西會說 —— 症狀是「量到的其實是重驗證」，
#   而 oha 的 JSON 裡看不出這件事，輸出看著完全正常。⇒ 一處定義（G147 那條形狀）。
# ⚠ 認不出的寫法**當場死**，⛔ 不回落到某個預設：一個安靜回落的解析器
#   會讓 ttl 變成一個只有它自己知道的數。
duration_secs() {
  local d=$1 n unit
  n=${d%[smh]}
  unit=${d#"$n"}
  case "$n" in
    '' | *[!0-9]*)
      echo "CACHE-HIT-P99 FAILED: BENCH_DURATION 的数值部分不是整数：'$d'" >&2
      exit 1
      ;;
  esac
  case "$unit" in
    s) echo "$n" ;;
    m) echo "$((n * 60))" ;;
    h) echo "$((n * 3600))" ;;
    *)
      echo "CACHE-HIT-P99 FAILED: 认不出 BENCH_DURATION 的写法：'$d'（只认 <整数>s|m|h）" >&2
      exit 1
      ;;
  esac
}
DURATION_SECS=$(duration_secs "$DURATION")
TTL="$((DURATION_SECS * 10))s"

# ── 原語（與 bench/diag/respond-ceiling.sh 同一套，理由見那裡的註解）──────
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
kill_pid() {
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
stop_child() {
  [ -n "$CHILD" ] || return 0
  kill_pid "$CHILD"
  CHILD=
}
stop_origin() {
  [ -n "$ORIGIN_PID" ] || return 0
  kill_pid "$ORIGIN_PID"
  ORIGIN_PID=
}

cleanup() {
  stop_child
  stop_origin
  rm -rf "$WORK"
  [ -z "${BASE:-}" ] || rm -rf "$BASE"
}
trap cleanup EXIT

pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

# ── 站台根與快取根：**各開各的子目錄**，⛔ 不共用一個 ──────────────────────
#
# ★ 重複使用現有 ext4 匿名卷（owner 2026-09-09 拍板）：不動 bench/docker-run.sh 的卷集；
#   與生產形狀一致（同一塊盤）。⚠ 帶 `$$` 是照 respond-ceiling.sh:128 的做法 ——
#   `BENCH_WWW_ROOT` 是跨步驟共用的掛載點，不帶 pid 時兩步會互相踩。
if [ -n "${BENCH_WWW_ROOT:-}" ]; then
  BASE="$BENCH_WWW_ROOT/$NAME-$$"
else
  BASE="$WORK/$NAME"
fi
rm -rf "$BASE"
WWW="$BASE/www"
CACHE_DIR="$BASE/cache"
mkdir -p "$WWW" "$CACHE_DIR" "$DIAG_DIR"

# payload：⚠ 與 respond-ceiling 不同，這裡**不必可列印** —— 它由源站的
# `file_server` 從磁碟發出，不進 DSL 的內聯正文。⇒ 與靜態吞吐那一類同源。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WWW/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WWW/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "CACHE-HIT-P99 FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
URL_PATH=/payload.bin

# ── 繪製三份設定 ───────────────────────────────────────────────────────────
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WWW_ROOT__|$WWW|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__TTL__|$TTL|g" \
    -e "s|__CACHE_DIR__|$CACHE_DIR|g" \
    -e "s|__ORIGIN_PORT__|$PORT_ORIGIN|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum-cache-origin.Fulcrumfile" "$WORK/origin.Fulcrumfile" "$PORT_ORIGIN"
render "$BENCH_DIR/conf/fulcrum-cache-disk.Fulcrumfile" "$WORK/disk.Fulcrumfile" "$PORT_DISK"

# ★ ★ ★ 記憶體那一份**由磁碟那份刪掉 `disk` 一行派生**，別的一個位元組不改
#   —— 單變數由生成方式保證（照 tests/cache/disk.sh:145-146）。
#   ⚠ 連接埠那一格要換成記憶體變體的，⇒ 先刪行再改連接埠，兩步都只動一行。
grep -v '^        disk ' "$WORK/disk.Fulcrumfile" > "$WORK/mem.Fulcrumfile.tmp"
sed -e "s|:$PORT_DISK {|:$PORT_MEM {|" "$WORK/mem.Fulcrumfile.tmp" > "$WORK/mem.Fulcrumfile"
# ⚠ 派生自證：刪掉的**必須恰好是一行**，且那一行**必須**是 `disk`。
#   ⛔ 少了這一步，縮進一改 grep 就什麼都刪不掉，而兩個變體會變成同一個後端 ——
#   那種失效下 delta 恆等於「兩次記憶體命中之差」，看著完全正常。
DISK_LINES=$(wc -l < "$WORK/disk.Fulcrumfile")
MEM_LINES=$(wc -l < "$WORK/mem.Fulcrumfile")
if [ "$((DISK_LINES - MEM_LINES))" != 1 ]; then
  echo "CACHE-HIT-P99 FAILED: 派生内存变体该只少一行，实际磁盘 $DISK_LINES 行、内存 $MEM_LINES 行" >&2
  exit 1
fi
if grep -q '^        disk ' "$WORK/mem.Fulcrumfile"; then
  echo "CACHE-HIT-P99 FAILED: 内存变体里仍然有 disk 那一行" >&2
  exit 1
fi

# ── 承重件：量之前先證明量的真是那個後端的命中 ─────────────────────────────
#
# ⚠ ⚠ ⛔ 這一格最危險的失效是「以為在量磁碟命中，其實快取壓根沒開」：
#   `dsl-reference.md` 明寫「配了 `disk` 而目錄用不了」時**快取是關掉的、照常轉發**，
#   `X-Fulcrum-Cache` 那個頭**一個都不會出現** —— 而那種情況下 oha 照樣量得出
#   一組**看起來完全正常**的數，⛔ 不會有任何東西說。
# ★ 判別力來自 `dsl-reference.md`：「後綴說的是**這個處理程序真的挑中了哪個後端**，
#   不是設定裡寫了什麼」。單測在 `crates/fulcrum-server/src/cache/mod.rs:587,590`。
# ⚠ ⚠ **指標那一層答不了這個問題**：`cache/mod.rs:280` 寫著它「折得比回應頭**粗**：
#   `HIT` 與 `HIT-DISK` 在指標上是同一格」⇒ ⛔ 別想用 `metrics` 代替本判據。
# ★ ★ ★ **觀測到的值要落進原始資料，⛔ 不能只以「✓ 列印行」的形式存在。**
#   ⚠ 2026-09-09 實測撞出來的：本步在門禁裡跑時，`tests/bench/gate.sh:40` 把
#   `bench/run.sh` 的輸出重定向進**容器裡**的 `/tmp/bench-gate.log`，`--rm` 之後就沒了
#   ⇒ 那幾行 `✓` **沒有任何人看得見**，包括寫它的人。
#   ⇒ 於是「量的是不是那個後端」這件事就只剩「相信這個指令碼的控制流」這一條路，
#   而那正是 G147 治的那一族：**口徑必須落在原始資料裡**。
#   ⇒ 觀測值寫進 `delta.json`，由 `tests/bench/gate.sh` 的 C18 **獨立複核**。
STATE_LOG="$WORK/cache-states.tsv"
: > "$STATE_LOG"

assert_cache_state() {
  local name=$1 port=$2 want=$3 when_key=$4 log=$5
  local hdr got age when rc=0
  case "$when_key" in
    before) when=开跑前 ;;
    after) when=跑完后 ;;
    *)
      echo "CACHE-HIT-P99 FAILED: 认不出的时点键 '$when_key'（只认 before|after）" >&2
      exit 1
      ;;
  esac
  hdr=$(curl -sS -D - -o "$WORK/$name.body" "http://$HOST:$port$URL_PATH" 2>>"$log" || true)
  got=$(printf '%s' "$hdr" | tr -d '\r' | awk -F': ' 'tolower($1) == "x-fulcrum-cache" { print $2 }')
  age=$(printf '%s' "$hdr" | tr -d '\r' | awk -F': ' 'tolower($1) == "age" { print $2 }')
  # ⚠ 無論過沒過都記：一份記著「這裡當時是空的」的原始資料，比一份乾脆沒有這一格的
  #   原始資料誠實得多。⛔ 只在成功時記 = 事後看不出失敗長什麼樣。
  printf '%s\t%s\t%s\t%s\n' "$name" "$when_key" "${got:-}" "${age:-}" >> "$STATE_LOG"

  if [ "$got" != "$want" ]; then
    bad "$name（$when）X-Fulcrum-Cache 该是 '$want'，实际 '${got:-（这个头根本没出现）}' —— 量的不是那个后端的命中"
    return 1
  fi

  # ── 「整趟沒發生重驗證」由 `Age` 判，⛔ 不由「前後取值相同」判 ───────────
  #
  # ⛔ ⛔ **本條原先寫的是「跑完後再核一次 X-Fulcrum-Cache，相同 ⇒ 沒重驗證」——
  #   那是錯的，2026-09-09 用反證實測證偽：注入 `ttl=1s` < `duration=2s`
  #   之後整個門禁**照樣全綠**。**
  # ★ 根因（讀原始碼，⛔ 不是推理）：`CacheState` 只有 `Hit` / `Revalidated` 兩個值
  #   （`cache/mod.rs:261`），而重驗證之後條目**重新變新鮮** ⇒ 之後的請求又是 `Hit`
  #   —— 與「從未過期」在最終那一次觀測上**完全相同**。
  # ★ 改用 `Age`：`Age = now − stored_at`（`lib.rs:1384`），而重驗證走
  #   `refresh(…, now_unix(), …)` → `stored_at = now`（`store.rs:208`，單測
  #   `refresh_只改元数据不动体` 釘著）⇒ **重驗證會把 `Age` 歸零**。
  #   ⇒ 預熱在負載之前 ⇒ 中途沒被刷新的話，跑完時 `Age` 必然 ≥ 負載時長；
  #   只要中途重驗證過一次，`Age` 就被砍回去。
  if [ "$when_key" = after ]; then
    case "$age" in
      '' | *[!0-9]*)
        bad "$name（$when）Age 头缺失或不是整数：'${age:-（没有这个头）}' —— 判不了整趟有没有重验证"
        rc=1
        ;;
      *)
        if [ "$age" -lt "$DURATION_SECS" ]; then
          bad "$name（$when）Age=$age 小于负载时长 ${DURATION_SECS}s ⇒ **整趟中途发生过重验证**，量到的不全是命中"
          rc=1
        else
          ok "$name（$when）X-Fulcrum-Cache = $got，Age = $age ≥ ${DURATION_SECS}s ⇒ 整趟没重验证"
        fi
        ;;
    esac
    return "$rc"
  fi

  ok "$name（$when）X-Fulcrum-Cache = $got（Age = ${age:-?}）"
  return 0
}

run_variant() {
  local name=$1 port=$2 conf=$3 want=$4
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

  # ⚠ 預熱：第一發必然回源（MISS），把條目灌進快取。⛔ 結果不判 ——
  #   本步要判的是**第二發之後**的穩態命中。
  curl -sS -o /dev/null "http://$HOST:$port$URL_PATH" 2>>"$log" || true

  if ! assert_cache_state "$name" "$port" "$want" before "$log"; then
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    wait_port_gone "$port" || true
    return 0
  fi

  # ★ 命中的那份正文必須與 payload **逐位元組相同**（同 respond-ceiling.sh:192）：
  #   兩個變體之間的差要有意義，前提是兩邊回的是同一份位元組。
  if ! cmp -s "$WORK/$name.body" "$WWW/payload.bin"; then
    bad "$name 回的正文与 payload **不是同一份字节**（$(wc -c < "$WORK/$name.body") 字节对 $PAYLOAD_BYTES）"
    stop_child
    wait_port_gone "$port" || true
    return 0
  fi
  ok "$name 命中的正文与 payload 逐字节相同（$PAYLOAD_BYTES 字节）"

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

  # ★ ★ 跑完之後**再核一次**：仍是同一個值 ⇒ 整趟沒有發生重驗證。
  #   ⚠ 中途過期的話量到的是 `REVALIDATED` 那條**要回源**的路而不是命中，
  #   而 oha 的 JSON 裡看不出這件事 —— 這一次複核就是為它加的。
  assert_cache_state "$name" "$port" "$want" after "$log" || true

  stop_child
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一个变体会起不来"
}

# ── 源站：整趟只起一次，兩個變體共用 ───────────────────────────────────────
echo "── 源站（file_server，:$PORT_ORIGIN）──"
"${pin_server[@]}" "$FULCRUM_BIN" serve "$WORK/origin.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/origin.pid" \
  --upgrade-sock "$WORK/origin.sock" \
  --state-dir "$WORK/origin-state" > "$WORK/origin.log" 2>&1 &
ORIGIN_PID=$!
if ! wait_port "$PORT_ORIGIN"; then
  echo "CACHE-HIT-P99 FAILED: 源站没在 $PORT_ORIGIN 上起来" >&2
  sed 's/^/      /' "$WORK/origin.log" >&2 || true
  exit 1
fi
ok "源站起来了（ttl=$TTL，⇒ 远大于一趟负载的 $DURATION）"

# ⚠ 順序有意固定：結果與順序無關才對，⇒ 固定它是為了讓兩趟之間可比。
run_variant cache-mem "$PORT_MEM" "$WORK/mem.Fulcrumfile" HIT
run_variant cache-disk "$PORT_DISK" "$WORK/disk.Fulcrumfile" HIT-DISK

stop_origin
wait_port_gone "$PORT_ORIGIN" || bad "源站收掉之后 $PORT_ORIGIN 还占着"

# ── 差值寫入磁碟 ───────────────────────────────────────────────────────────────
#
# ★ 用 python3 序列化，⛔ 不手動拼 JSON。
# ★ **指名來源**：差值可被第三方按那兩份原始讀數重算，⛔ 不是一個孤零零的數。
if [ "$FAILS" = 0 ]; then
  DIAG_DIR="$DIAG_DIR" STATE_LOG="$STATE_LOG" TTL="$TTL" DURATION="$DURATION" \
    DURATION_SECS="$DURATION_SECS" python3 - <<'PY'
import json, os, pathlib
d = pathlib.Path(os.environ["DIAG_DIR"])

def p99(name):
    # ⚠ 键路径是 `latencyPercentiles.p99`（2026-09-09 对着
    #   bench/results/2026-09-06-static-throughput/ 那组真实读数实测）。
    # ⛔ **不是** summary.p99 —— summary 里根本没有 p99。
    # ⛔ 也不是 metrics.latency_ms.p99 —— 那份单位是**毫秒**，取错差 1000 倍
    #    而看着像个正常的数。
    return json.loads((d / name).read_text(encoding="utf-8"))["latencyPercentiles"]["p99"]

mem, disk = p99("cache-mem.json"), p99("cache-disk.json")

# ★ ★ ★ 观测到的 `X-Fulcrum-Cache` 落进原始数据 —— 这是「量的是不是那个后端的命中」
#   唯一留得下来的证据（打印行进了容器里的日志，`--rm` 之后就没了）。
#   ⇒ `tests/bench/gate.sh` 的 C18 会**独立复核**这四格，⛔ 不靠信任本脚本的控制流。
states = {}
for line in pathlib.Path(os.environ["STATE_LOG"]).read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    name, when, got, age = line.split("\t")
    # ★ `age` 与 `state` 一起记：只有 `age` 分得开「整趟都是命中」与
    #   「中途过期又被重验证回来」—— 后者最终那一次观测同样是 HIT。
    states.setdefault(name, {})[when] = {"state": got, "age": age}

(d / "delta.json").write_text(json.dumps({
    "what": "磁盘命中的 p99 − 内存命中的 p99（G150 ②）",
    "unit": "seconds",
    "p99_mem": mem,
    "p99_disk": disk,
    "delta": disk - mem,
    "source": {"mem": "cache-mem.json", "disk": "cache-disk.json"},
    # ⚠ ⚠ `state` 说的是「这个进程**真的**挑中了哪个后端」，⛔ 不是配置里写了什么
    #   （`dsl-reference.md` 的原话）。
    # ⛔ ⛔ **`after.state` 与 `before.state` 相同**这件事**证不了**「整趟没重验证」
    #   —— 2026-09-09 用反证实测证伪过（`ttl=1s` < `duration=2s` 时它照样全绿）：
    #   重验证之后条目重新变新鲜，之后的请求又是 `Hit`。
    # ★ 分得开的是 **`after.age`**：重验证会把 `stored_at` 置为当前时刻
    #   （`store.rs:208`）⇒ `Age` 归零。`after.age >= duration_secs`
    #   才代表这一条从预热起就没被刷新过。
    "cache_state": states,
    # ★ 口径：ttl 是从 duration 算出来的（10 倍），三个值一起记才说得清。
    "ttl": os.environ["TTL"],
    "duration": os.environ["DURATION"],
    "duration_secs": int(os.environ["DURATION_SECS"]),
    # ⚠ 有意不定阈值（G150 ② 明写）—— 定一个没有实测支撑的数字
    #   正是 G148 ③ 点名的那种错。阈值等有数之后再定。
    "threshold": None,
}, ensure_ascii=False, indent=2))
PY
  ok "差值已落盘：diag/$NAME/delta.json"
fi

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/diag/$NAME] 两个后端都跑通，原始数据与差值在 $DIAG_DIR"
  echo "[bench/diag/$NAME] ⛔ 本步**不产出任何结论**，也**不参与** §8 的判定"
else
  echo "[bench/diag/$NAME] ★ $FAILS 处失败 —— 这一趟的诊断数据不完整" >&2
  exit 1
fi
