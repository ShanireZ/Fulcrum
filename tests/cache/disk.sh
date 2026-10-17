#!/usr/bin/env bash
# 自研 HTTP 快取的**磁碟後端**端到端（**M2 批 H**，形狀由 G83/G84 定死）。
#
# ★ 與 `tests/cache/run.sh`（批 G）的分工說死：
#   · 那一格驗的是**語義**（可快取性 / 新鮮度 / Vary / 防驚群 / purge），跑的是記憶體後端；
#   · 這一格驗的是**介質**——同一套語義換到盤上之後，多出來的那幾件事：
#     重新啟動之後還在、啟動不掃盤、壞條目即丟即刪、目錄用不了怎麼辦。
#
# ★ ★ ★ **本場景最要緊的一條判據，以及為什麼是它。**
#   判據必須能分辨「從記憶體來的」與「從磁碟來的」，而那件事靠狀態碼看不出來。
#   `X-Fulcrum-Cache: HIT-DISK` 這個頭**只是方便的那一半**
#   —— 它信的是「本處理程序挑中了哪個後端」，一個把後綴寫死的實作照樣能騙過它。
#   ⇒ 真正的判據是**把處理程序殺掉再起來，東西還在**：
#     · 磁碟後端：命中，體裡是**重新啟動之前**那個 nonce；
#     · 記憶體後端：必然回源。
#   ★ 這兩半在**同一次跑裡**都做（第 2 格與它下面那條反證），
#   於是這把尺子在好情況與壞情況下讀數不同 —— 那正是 AGENTS.md 那條
#   「a ruler that reads the same in both cases cannot tell them apart」要的東西。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 連接埠表見 AGENTS.md。**9502–9503 是本場景的**（批 H 新增；批 G 那一格是 9500–9501）。
#   ★ 選之前查過那張表 —— 而批 F 正是因為**表不全**才撞上壓力那一格。
PORT=${CACHEDISK_PORT:-9502}
UP_PORT=${CACHEDISK_UP_PORT:-9503}
ADMIN_SOCK="$WORK/admin.sock"
DISK="$WORK/cachedir"

FAILS=0
PIDS=()
CACHE_PID=""

fail() { echo "  ✗ $*" >&2; FAILS=$((FAILS + 1)); }
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

echo "=== [0/8] 基线：端口未被占用 ==="
for p in "$PORT" "$UP_PORT"; do
  if port_listening "$p"; then
    echo "CACHE-DISK TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑。" >&2
    exit 1
  fi
done
ok "$PORT / $UP_PORT 都是空的"

[ -x "$BIN" ] || {
  echo "CACHE-DISK TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 上游 ────────────────────────────────────────────────────────────────────
#
# ★ 上游是**樞衡自己**（與批 G / serve 同一個做法，零外部依賴）。
# ★ ★ 「每次不同」由用戶端現給的 `X-Nonce` 提供，**不是**由時間或連接埠提供 ——
#   批 G 在這上面一口氣栽了三次（`{time}` 秒級精度、`{remote_port}` 撞上連接池、
#   忘了帶 nonce），三次都是「判據在最該報警的那一刻給一個像樣的錯答案」。
cat > "$WORK/up.Fulcrumfile" <<'CONF'
:UP_PORT {
    # ⚠ 匹配器只能定义在**站点块的顶层**（`FUL-DSL-0010`）——写进 `handle` 里
    #   编译当场就红。★ 那是好事：它意味着「这个 @名字指什么」只有一个地方能查。
    @inm header If-None-Match

    handle /maxage {
        header Cache-Control "max-age=60"
        respond 200 "up-{header.X-Nonce}"
    }
    handle /reval {
        # ★ 重验证那一格要的上游：给 ETag，而**带了 `If-None-Match` 的请求回 304**
        #   —— 于是枢衡走的是「只改 meta 不动 body」那条路（G83 定这个形状的理由本身）。
        # ⚠ ⚠ **寿命给足 60s，靠请求侧 `Cache-Control: max-age=0` 逼出重验证**，
        #   而不是给一个 1s 的寿命再 `sleep 2` 等它过期。★ 差别是判据的性质：
        #   等时钟 = 在机器快慢上赌一把（慢机器上第二次请求就变成了重验证，
        #   而那条本该验「命中」的断言会红得毫无道理）；请求侧收紧新鲜度
        #   （RFC 9111 §5.2.1.1）是**当场生效**的，与机器无关。
        header Cache-Control "max-age=60"
        header ETag "\"v1\""
        respond @inm 304
        respond 200 "up-{header.X-Nonce}"
    }
    respond 404 "no-such-path"
}
CONF
sed -i "s/:UP_PORT/:$UP_PORT/" "$WORK/up.Fulcrumfile"

# ── 兩份設定：磁碟後端 / 記憶體後端。★ 除了那一行 `disk`，**其餘一模一樣** ────
#
# ⚠ 這一點是判據的一部分：兩次跑之間只差一行，於是第 2 格那對結果的差別
#   只可能來自後端本身，不可能來自別的什麼。
cat > "$WORK/disk.Fulcrumfile" <<'CONF'
{
    admin unix/ADMIN_SOCK
    # ★ 优雅停机的排空窗口压到 1s：第 3 格要真的走一次 SIGTERM，
    #   而产品默认是 30s —— 那会让这个场景白等半分钟。
    grace_period 1s
}

:PORT {
    cache {
        ttl 30s
        capacity 1MB
        disk DISK_DIR
    }
    reverse_proxy 127.0.0.1:UP_PORT
}
CONF
sed -i "s/:PORT/:$PORT/; s/:UP_PORT/:$UP_PORT/; s|ADMIN_SOCK|$ADMIN_SOCK|; s|DISK_DIR|$DISK|" \
  "$WORK/disk.Fulcrumfile"

# 記憶體後端那一份：把 `disk` 那行刪掉，別的一個字不改。
grep -v '^        disk ' "$WORK/disk.Fulcrumfile" > "$WORK/mem.Fulcrumfile"
if grep -q 'disk ' "$WORK/mem.Fulcrumfile"; then
  echo "CACHE-DISK TESTS FAILED: 内存那份配置里还留着 disk —— 第 2 格的反证会证不到东西。" >&2
  exit 1
fi

start_up() {
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/up.Fulcrumfile" \
    --bind-host "$HOST" \
    --pid-file "$WORK/up.pid" \
    --upgrade-sock "$WORK/up.sock" \
    > "$WORK/up.log" 2>&1 &
  PIDS+=($!)
}

# $1 = 設定檔案，$2 = 紀錄檔案名（**追加**，重新啟動之後還能讀到上一代說過什麼）
start_cache() {
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$1" \
    --bind-host "$HOST" \
    --pid-file "$WORK/cache.pid" \
    --upgrade-sock "$WORK/cache.sock" \
    >> "$WORK/$2" 2>&1 &
  CACHE_PID=$!
  PIDS+=("$CACHE_PID")
}

# $1 = 信號（INT = 快停，TERM = 優雅）
stop_cache() {
  local sig=$1 waited=0
  [ -n "$CACHE_PID" ] || return 0
  kill -"$sig" "$CACHE_PID" 2>/dev/null || true
  # ⚠ ⚠ **只等這一個 pid，不能用光禿禿的 `wait`** —— 它會等本 shell 的全部後台作業，
  #   而上游那個 `fulcrum serve` **永遠不退出**。批 G 就是這麼在螢幕上掛了 9 分多鐘，
  #   最後一行停在「=== [6/7] 防驚群 ===」，讀起來像產品死鎖。
  #   > ★ 一個會掛住的判據比一條紅的判據更貴：紅的指著問題，掛住的指著錯誤的方向。
  while kill -0 "$CACHE_PID" 2>/dev/null && [ "$waited" -lt 200 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  kill -9 "$CACHE_PID" 2>/dev/null || true
  # ★ 處理程序沒了不等於連接埠放開了：下一次 bind 會撞 EADDRINUSE，
  #   而回報錯誤讀起來像「連接埠被別人佔了」。
  waited=0
  while port_listening "$PORT" && [ "$waited" -lt 200 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  CACHE_PID=""
}

start_up
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$UP_PORT" || { echo "CACHE-DISK TESTS FAILED: 上游 $UP_PORT 没起来" >&2; cat "$WORK/up.log" >&2; exit 1; }
wait_port "$PORT"    || { echo "CACHE-DISK TESTS FAILED: $PORT 没起来" >&2; cat "$WORK/disk.log" >&2; exit 1; }

BASE="http://$HOST:$PORT"

# ── helper ─────────────────────────────────────────────────────────────────
NONCE=0
probe() {
  # ⚠ 先清空 body 與 hdr：curl 的 `-o` 在零位元組時**不動**那個檔案（批 F 栽過），
  #   於是上一次的內容會被當成這一次的。
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 10 "$@"
}

# 帶一個**新的** nonce 打一次。★ 命中 ⇒ 體裡是**上一次**那個 nonce。
probe_n() {
  NONCE=$((NONCE + 1))
  probe -H "X-Nonce: n$NONCE" "$@"
}

hdr() { { grep -i "^$1:" "$WORK/hdr" || true; } | head -1 | cut -d' ' -f2- | tr -d '\r'; }
body() { cat "$WORK/body"; }

admin() {
  curl -s --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    -w '\n%{http_code}' --max-time 10 "http://localhost$1"
}

# ★ ★ ★ **「東西已經落到盤上了」這句話，只能由產品自己說。**
#
# ⚠ ⚠ 資料面是**先把回應發完、再存快取**的（`store_if_allowed` 排在
#   `write_response_body(…, true)` 之後，而且 body 寫入磁碟還帶一次 `fsync`）——
#   那個順序是對的：用戶端不該等一次磁碟寫。
#   ⇒ 於是 `curl` 回傳的那一刻，盤上**可能還什麼都沒有**。
#
# ★ 這裡容易栽：`purge` 之後打一發再 `find` 檔案，
#   兩次跑紅在**兩個不同的格子**（一次說「meta 有 body 沒有」、一次說「沒找到 body」）——
#   ⚠ 而它讀起來完全像產品缺陷，我據此推出過一個很像真的錯結論
#   （以為是後台清理把剛寫下的 body 當成孤兒收走了；開 debug 紀錄一看，**一次都沒收過**）。
#   > ★ 一條會隨機紅的判據，紅與綠都不說明任何事；而它先教人重跑，再教人忽略。
#
# ⇒ 修法**不是** sleep 一下再看，而是：**再打一發，等它說命中**。
#   命中這件事只有在快取裡真的有那條時才成立 —— 用產品自己的可觀測事實
#   把「存好了」這一刻釘死，與機器快慢無關。
settle_stored() {
  local what=$1 url=$2 tries=0 state
  while [ "$tries" -lt 50 ]; do
    probe_n "$url" > /dev/null
    state=$(hdr X-Fulcrum-Cache)
    if [ -n "$state" ]; then
      ok "$what 已经在缓存里（$state）"
      return 0
    fi
    sleep 0.1
    tries=$((tries + 1))
  done
  fail "$what 打了 $tries 发都没进缓存 —— 这不是抖动，是它真的没被存下来"
  return 1
}

expect_status() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望 $want，实际 $got"; fi
}

# ── [1/8] 盤上真的有東西，而且是 G83 那個形狀 ──────────────────────────────
echo
echo "=== [1/8] 磁盘后端：命中头带 -DISK，盘上是两级分片 + meta/body 两文件 ==="
probe_n "$BASE/maxage" > /dev/null
FIRST=$(body)
CODE=$(probe_n "$BASE/maxage")
expect_status "GET /maxage（第二次）" 200 "$CODE"
SECOND=$(body)
if [ "$FIRST" = "$SECOND" ]; then ok "命中（体没变）"; else
  fail "本该命中：第一次「$FIRST」第二次「$SECOND」"; fi
# ★ 後綴說的是「這個處理程序**真的**挑中了哪個後端」——配了 `disk` 而目錄用不了時
#   快取是關掉的，那時這個頭一個都不會出現（第 7 格驗它）。
if [ "$(hdr X-Fulcrum-Cache)" = "HIT-DISK" ]; then
  ok "X-Fulcrum-Cache = HIT-DISK（分得出内存与磁盘）"
else
  fail "X-Fulcrum-Cache 期望 HIT-DISK，实际「$(hdr X-Fulcrum-Cache)」"
fi

# G83 的佈局：兩級分片目錄 · meta 與 body 兩類檔案。
METAS=$(find "$DISK" -name '*.meta' | wc -l)
BODIES=$(find "$DISK" -name '*.body' | wc -l)
if [ "$METAS" -ge 1 ] && [ "$BODIES" -ge 1 ]; then
  ok "盘上有 meta（$METAS）与 body（$BODIES）两类文件（G83）"
else
  fail "盘上没落下东西：meta=$METAS body=$BODIES"
  find "$DISK" -type f >&2 || true
fi
# ⚠ 分片深度**要數出來**，不是看一眼像不像：`<root>/<aa>/<bb>/<文件>` ⇒ 相對根有 3 段。
#   ★ 單層目錄在幾十萬檔案時會把清理任務本身拖垮，而那件事在門禁裡永遠不會發生
#   （門禁只存幾條）—— 所以只能釘形狀。
ONE_META=$(find "$DISK" -name '*.meta' | head -1)
REL=${ONE_META#"$DISK"/}
DEPTH=$(echo "$REL" | awk -F/ '{print NF}')
if [ "$DEPTH" = "3" ]; then ok "两级分片目录（$REL）"; else
  fail "分片深度期望 3 段，实际 $DEPTH 段：$REL"; fi

# ── [2/8] ★★★ 重新啟動之後還在（快停：索引沒來得及存）───────────────────────
echo
echo "=== [2/8] ★★★ 重启之后东西还在，而且启动不扫盘 ==="
KEEP=$SECOND   # 重新啟動之前快取裡那一份（體裡是第一次那個 nonce）
stop_cache INT
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: 重启之后 $PORT 没起来" >&2; cat "$WORK/disk.log" >&2; exit 1; }

# ⚠ ⚠ **這一步必須排在任何 GET 之前**：讀路徑會順手把命中的那條記進索引，
#   於是先 GET 再問「索引裡有幾條」，問到的是被自己改過的數。
RESP=$(admin /purge '{"key":"一个从来没有过的键"}')
LEFT=$(printf '%s' "$RESP" | grep -o '还剩 [0-9]* 条' | grep -o '[0-9]*' || true)
if [ "$LEFT" = "0" ]; then
  ok "★ 启动不扫盘：索引里 0 条（G84 —— 全盘扫描与 G78 的快速就绪冲突）"
else
  fail "启动时扫盘了？索引里已经有 $LEFT 条：$RESP"
fi

CODE=$(probe_n "$BASE/maxage")
expect_status "重启之后 GET /maxage" 200 "$CODE"
if [ "$(body)" = "$KEEP" ] && [ "$(hdr X-Fulcrum-Cache)" = "HIT-DISK" ]; then
  ok "★ ★ ★ 重启之后**命中的还是重启之前那一份**（索引是空的，读路径照样找得到）"
else
  fail "★ ★ ★ 重启之后没命中：期望体「$KEEP」实际「$(body)」cache=「$(hdr X-Fulcrum-Cache)」"
fi

# ★ ★ 反證的另一半 —— **同一把尺子在記憶體後端上必須讀出相反的結果**。
#   ⚠ 少了它，上面那條證明不了「是磁碟讓它活下來的」：一個把快取掛在
#   某處處理程序外狀態上的實作、甚至一個根本沒重新啟動成功的指令碼，都能讓它綠。
echo '  —— 反证：同一份配置去掉 disk 那一行 ——'
stop_cache INT
start_cache "$WORK/mem.Fulcrumfile" "mem.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: 内存实例没起来" >&2; cat "$WORK/mem.log" >&2; exit 1; }
probe_n "$BASE/maxage" > /dev/null
MEM_BEFORE=$(body)
if [ "$(hdr X-Fulcrum-Cache)" = "" ]; then ok "内存实例第一次是回源（意料之中）"; else
  fail "内存实例第一次就命中了？cache=$(hdr X-Fulcrum-Cache) —— 那说明它读到了盘上的东西"; fi
stop_cache INT
start_cache "$WORK/mem.Fulcrumfile" "mem.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: 内存实例第二次没起来" >&2; exit 1; }
probe_n "$BASE/maxage" > /dev/null
if [ "$(body)" != "$MEM_BEFORE" ] && [ -z "$(hdr X-Fulcrum-Cache)" ]; then
  ok "★ 内存后端重启之后**必然回源** —— 这把尺子在两种情况下读数不同"
else
  fail "★ 内存后端居然活过了重启：before「$MEM_BEFORE」after「$(body)」cache=「$(hdr X-Fulcrum-Cache)」"
fi
stop_cache INT

# ── [3/8] 優雅停機把淘汰索引存下去（G84 的 save/load）─────────────────────
echo
echo "=== [3/8] 优雅停机存索引，下一代读得回来 ==="
rm -f "$DISK/index.json"
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: $PORT 没起来" >&2; exit 1; }
probe_n "$BASE/maxage" > /dev/null   # 讓索引裡至少有一條
stop_cache TERM
if [ -s "$DISK/index.json" ]; then
  ok "优雅停机把淘汰索引写下去了（index.json）"
else
  fail "优雅停机之后没有 index.json —— G84 的 save 那一半没发生"
  grep -i '缓存' "$WORK/disk.log" | tail -5 >&2 || true
fi
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: $PORT 没起来" >&2; exit 1; }
RESP=$(admin /purge '{"key":"一个从来没有过的键"}')
LEFT=$(printf '%s' "$RESP" | grep -o '还剩 [0-9]* 条' | grep -o '[0-9]*' || true)
if [ -n "$LEFT" ] && [ "$LEFT" -ge 1 ]; then
  ok "★ 下一代把索引读回来了（$LEFT 条）—— 与第 2 格那个 0 是同一把尺子的两个读数"
else
  fail "索引没被读回来：$RESP"
fi

# ── [4/8] 重驗證只改 meta，不動 body ───────────────────────────────────────
echo
echo "=== [4/8] 重验证（304）只改 meta，不动 body ==="
admin /purge '{"all":true}' > /dev/null
probe_n "$BASE/reval" > /dev/null
settle_stored "/reval" "$BASE/reval" || true
# ⚠ ⚠ 「快取裡那份」要從**命中的那一發**取，不是從第一發取：第一發的回應
#   有沒有被存下來，正是 `settle_stored` 在確認的那件事 —— 拿它當基準就是
#   把待證的結論當成了前提。★ 而命中回來的體，按定義就是快取裡那一份。
REVAL_BODY=$(body)
BODY_FILE=$(find "$DISK" -name '*.body' | head -1)
META_FILE=$(find "$DISK" -name '*.meta' | head -1)
if [ -n "$BODY_FILE" ] && [ -n "$META_FILE" ]; then
  ok "/reval 落盘了（meta 与 body 各一个）"
else
  fail "/reval 没落盘：body=「$BODY_FILE」meta=「$META_FILE」"
  find "$DISK" -type f >&2 || true
fi
if [ -n "$BODY_FILE" ] && [ -n "$META_FILE" ]; then
  # ★ 取**納秒**精度的 mtime：秒級的話，一次落在同一秒裡的改寫會讀不出差別，
  #   於是「meta 確實被改寫了」那條反向判據會隨機不成立。
  BODY_MTIME_BEFORE=$(stat -c %.9Y "$BODY_FILE")
  META_MTIME_BEFORE=$(stat -c %.9Y "$META_FILE")
  # ★ 用**請求側** `max-age=0` 逼出重驗證（當場生效），不等時鐘。
  CODE=$(probe_n -H "Cache-Control: max-age=0" "$BASE/reval")
  expect_status "请求侧 max-age=0 ⇒ GET /reval" 200 "$CODE"
  # ★ 回給用戶端的必須是**快取裡那份完整回應**，不是上游那個 304。
  if [ "$(body)" = "$REVAL_BODY" ]; then
    ok "重验证之后回的是缓存里那份完整内容（不是把 304 转下去）"
  else
    fail "重验证之后内容变了：期望「$REVAL_BODY」实际「$(body)」"
  fi
  if [ "$(hdr X-Fulcrum-Cache)" = "REVALIDATED-DISK" ]; then
    ok "X-Fulcrum-Cache = REVALIDATED-DISK"
  else
    fail "X-Fulcrum-Cache 期望 REVALIDATED-DISK，实际「$(hdr X-Fulcrum-Cache)」"
  fi
  BODY_MTIME_AFTER=$(stat -c %.9Y "$BODY_FILE")
  META_MTIME_AFTER=$(stat -c %.9Y "$META_FILE")
  # ★ ★ ★ **G83 把 meta 與 body 分開，理由就是這一條**：重驗證是最常見的寫操作之一，
  #   而它不該動 body（那可能是幾 MB）。⚠ 一個「整條重寫」的實作在**別的每一條判據上
  #   都是綠的** —— 內容對、頭對、狀態碼對，只是每次 304 都把整個 body 重寫一遍。
  if [ "$BODY_MTIME_BEFORE" = "$BODY_MTIME_AFTER" ]; then
    ok "★ ★ 重验证没动 body 文件（G83 分开存的理由本身）"
  else
    fail "★ ★ 重验证把 body 整个重写了：$BODY_MTIME_BEFORE → $BODY_MTIME_AFTER"
  fi
  # 反向那一半：meta **必須**變 —— 否則「只改 meta」這句話的前半截也沒發生。
  if [ "$META_MTIME_BEFORE" != "$META_MTIME_AFTER" ]; then
    ok "★ 而 meta 确实被改写了（新鲜期续上了）"
  else
    fail "★ meta 没变 —— 那重验证根本没落地，上面那条 body 没变也就证明不了什么"
  fi
fi

# ── [5/8] purge 以盤為準，不是以索引為準 ──────────────────────────────────
echo
echo "=== [5/8] 索引是冷的时候，purge 照样清得掉 ==="
probe_n "$BASE/maxage" > /dev/null
stop_cache INT                       # 快停 ⇒ 索引不存盤
rm -f "$DISK/index.json"             # 連上一輪存的那份也拿掉
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: $PORT 没起来" >&2; exit 1; }
# ⚠ ⚠ 一個**只看索引**的 purge 會在這一刻清掉 0 條，還回一句「清掉 0 條」——
#   而這正是有人會去按 purge 的那個時刻（剛重新啟動、快取裡全是舊內容）。
#   ★ purge 的語義是「讓它不在」，所以它必須以盤為準。
RESP=$(admin /purge '{"prefix":"GET"}')
CLEARED=$(printf '%s' "$RESP" | grep -o '清掉 [0-9]* 条' | grep -o '[0-9]*' || true)
if [ -n "$CLEARED" ] && [ "$CLEARED" -ge 1 ]; then
  ok "★ ★ 索引是冷的，purge 前缀仍清掉了 $CLEARED 条（它走盘，不走索引）"
else
  fail "★ ★ 索引冷的时候 purge 清不掉东西：$RESP"
fi
CODE=$(probe_n "$BASE/maxage")
expect_status "purge 之后 GET /maxage" 200 "$CODE"
if [ -z "$(hdr X-Fulcrum-Cache)" ]; then
  ok "purge 之后真的回源了（没有命中头）"
else
  fail "purge 说清了，下次请求还是命中：$(hdr X-Fulcrum-Cache)"
fi

# ── [6/8] 壞條目即丟即刪（讀時檢核，G84）──────────────────────────────────
echo
echo "=== [6/8] body 被截断 ⇒ 即丢即删，不是把半个页面发出去 ==="
admin /purge '{"all":true}' > /dev/null
probe_n "$BASE/maxage" > /dev/null
# ⚠ 同上：`curl` 回傳不等於盤上有東西。等它自己說「命中」再去看檔案。
settle_stored "/maxage" "$BASE/maxage" || true
GOOD=$(body)
BODY_FILE=$(find "$DISK" -name '*.body' | head -1)
if [ -z "$BODY_FILE" ]; then
  fail "没找到 body 文件，这一格证不到东西"
else
  printf 'xx' > "$BODY_FILE"        # 截斷成兩個位元組
  CODE=$(probe_n "$BASE/maxage")
  expect_status "body 被截断之后 GET /maxage" 200 "$CODE"
  # ★ ★ 判據看的是**體**，不是狀態碼：一個不做讀時檢核的實作會把 `xx` 原樣
  #   發出去，而它照樣回 200、照樣有 Content-Length —— 用戶端拿到半個頁面。
  if [ "$(body)" != "xx" ] && [ "$(body)" != "$GOOD" ]; then
    ok "★ ★ 坏条目被当成未命中、回源取了新的（体是「$(body)」）"
  elif [ "$(body)" = "xx" ]; then
    fail "★ ★ ★ 把截断的 body 原样发出去了 —— 读时校验（G84）没做"
  else
    fail "体没变（「$(body)」）—— 坏条目似乎还在被当成好的用"
  fi
  if [ ! -f "$BODY_FILE" ] || [ "$(cat "$BODY_FILE")" != "xx" ]; then
    ok "★ 坏条目被删掉了（「即丢即删」的后半句）"
  else
    fail "★ 坏条目还在盘上，下次还会被撞见：$BODY_FILE"
  fi
fi

# ── [7/8] 目錄用不了 ⇒ 關掉快取，但照常轉發 ───────────────────────────────
echo
echo "=== [7/8] 缓存目录用不了：关掉缓存，服务照常 ==="
stop_cache INT
# 用一個**檔案**當快取根 —— `create_dir_all` 會失敗。
BAD_ROOT="$WORK/iam-a-file"
: > "$BAD_ROOT"
sed "s|$DISK|$BAD_ROOT|" "$WORK/disk.Fulcrumfile" > "$WORK/bad.Fulcrumfile"
start_cache "$WORK/bad.Fulcrumfile" "bad.log"
if wait_port "$PORT"; then
  ok "★ ★ 目录用不了，进程**照样起来了**（拒绝启动 = 换代时服务整体中断）"
else
  fail "★ ★ 缓存目录用不了就起不来了 —— 那等于一次 reload 打死整个服务"
  cat "$WORK/bad.log" >&2
fi
if port_listening "$PORT"; then
  CODE=$(probe_n "$BASE/maxage")
  expect_status "缓存关掉之后 GET /maxage" 200 "$CODE"
  probe_n "$BASE/maxage" > /dev/null
  if [ -z "$(hdr X-Fulcrum-Cache)" ]; then
    ok '★ 缓存确实关掉了（X-Fulcrum-Cache 一次都没出现）—— 运行时看得见'
  else
    fail "★ 目录用不了却还在缓存：$(hdr X-Fulcrum-Cache)"
  fi
fi
# ★ ★ **兩條獨立的信號，各自查一遍**：一條是「目錄為什麼用不了」（error），
#   另一條是「於是快取關掉了」（裝載結論）。⚠ 合成一條 grep 的話，
#   兩行裡少了任何一行都不會紅 —— 而少了後面那行，維運只會看到一個紅字，
#   看不到「所以現在到底是什麼狀態」。
if grep -q '缓存磁盘目录' "$WORK/bad.log"; then
  ok "★ 日志说了目录为什么用不了"
else
  fail "日志里没说目录用不了 —— 那就是一次静默失能"
  grep -i '缓存' "$WORK/bad.log" >&2 || head -30 "$WORK/bad.log" >&2
fi
if grep -q '缓存后端：已关闭' "$WORK/bad.log"; then
  ok "★ 装载结论也说了「缓存后端：已关闭」"
else
  fail "装载结论没说缓存被关掉了"
  grep -i '缓存' "$WORK/bad.log" >&2 || head -30 "$WORK/bad.log" >&2
fi
stop_cache INT

# ── [8/8] 裝載紀錄 + 編譯期那條診斷 ───────────────────────────────────────
echo
echo "=== [8/8] 装载日志说得出后端；两个不同的 disk 编译不过 ==="
if grep -q '磁盘后端' "$WORK/disk.log"; then
  ok "装载日志说了磁盘后端"
else
  fail "装载日志没说后端"
  grep -i '缓存' "$WORK/disk.log" >&2 || true
fi
if grep '磁盘后端' "$WORK/disk.log" | grep -q '启动不扫盘'; then
  ok "★ 而且说了它的形状（两级分片 / meta-body 两文件 / 启动不扫盘）"
else
  fail "装载日志没说清磁盘后端的形状"
fi
# ⚠ 批 G 留下的那條紀律：`ttl` 是**備援**不是覆蓋，裝載時必須說出來。
if grep '缓存：站点' "$WORK/disk.log" | grep -q '兜底'; then
  ok "ttl 是兜底这件事仍然说得出来（G96）"
else
  fail "装载日志不再说 ttl 是兜底了 —— 批 G 那条可见性被本批弄丢了"
fi

# ★ ★ 快取後端是**處理程序級**的：兩個 `cache` 塊寫不同的 `disk` 必須編譯不過，
#   而且要給那條**專門的**診斷（`FUL-DSL-0035`），不是一句「未知的子指令」。
cat > "$WORK/conflict.Fulcrumfile" <<'CONF'
:18080 {
    cache {
        disk /tmp/one
    }
    reverse_proxy 127.0.0.1:1
}
:18081 {
    cache {
        disk /tmp/two
    }
    reverse_proxy 127.0.0.1:2
}
CONF
set +e
VOUT=$("$BIN" validate "$WORK/conflict.Fulcrumfile" 2>&1)
VRC=$?
set -e
if [ "$VRC" -ne 0 ] && printf '%s' "$VOUT" | grep -q 'FUL-DSL-0035'; then
  ok "★ 两个不同的 disk 被 FUL-DSL-0035 拦下（validate 退出码 $VRC）"
else
  fail "两个不同的 disk 没被拦下（退出码 $VRC）：$VOUT"
fi
# 反向那一半：一致的設定必須過。⚠ 少了它，一條「只要有兩個 cache 就回報錯誤」的實作全綠。
sed 's|/tmp/two|/tmp/one|' "$WORK/conflict.Fulcrumfile" > "$WORK/agree.Fulcrumfile"
if "$BIN" validate "$WORK/agree.Fulcrumfile" > /dev/null 2>&1; then
  ok "★ 反向：两个 cache 写同一个 disk 照常通过"
else
  fail "★ 一致的配置也被拦了 —— 那条检查拦得太宽"
  "$BIN" validate "$WORK/agree.Fulcrumfile" >&2 2>&1 || true
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "CACHE-DISK TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 磁盘实例日志 ──" >&2
  cat "$WORK/disk.log" >&2
  echo "── 上游日志 ──" >&2
  cat "$WORK/up.log" >&2
  exit 1
fi
echo "CACHE-DISK TESTS PASSED —— 磁盘后端：两级分片 + meta/body 两文件 / ★ 重启之后还在（内存后端反证必然回源）/ 启动不扫盘 / 优雅停机存索引 / ★ 重验证只改 meta 不动 body / 索引冷时 purge 照样清得掉 / 坏条目即丢即删 / 目录用不了则关缓存但照常转发 / 装载日志 + FUL-DSL-0035。"
