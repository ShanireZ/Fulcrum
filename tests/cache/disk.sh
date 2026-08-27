#!/usr/bin/env bash
# 自研 HTTP 缓存的**磁盘后端**端到端（**M2 批 H**，形状由 G83/G84 定死）。
#
# ★ 与 `tests/cache/run.sh`（批 G）的分工说死：
#   · 那一格验的是**语义**（可缓存性 / 新鲜度 / Vary / 防惊群 / purge），跑的是内存后端；
#   · 这一格验的是**介质**——同一套语义换到盘上之后，多出来的那几件事：
#     重启之后还在、启动不扫盘、坏条目即丢即删、目录用不了怎么办。
#
# ★ ★ ★ **本场景最要紧的一条判据，以及为什么是它。**
#   判据必须能分辨「从内存来的」与「从磁盘来的」，而那件事靠状态码看不出来。
#   `X-Fulcrum-Cache: HIT-DISK` 这个头**只是方便的那一半**
#   —— 它信的是「本进程挑中了哪个后端」，一个把后缀写死的实现照样能骗过它。
#   ⇒ 真正的判据是**把进程杀掉再起来，东西还在**：
#     · 磁盘后端：命中，体里是**重启之前**那个 nonce；
#     · 内存后端：必然回源。
#   ★ 这两半在**同一次跑里**都做（第 2 格与它下面那条反证），
#   于是这把尺子在好情况与坏情况下读数不同 —— 那正是 AGENTS.md 那条
#   「a ruler that reads the same in both cases cannot tell them apart」要的东西。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 端口表见 AGENTS.md。**9502–9503 是本场景的**（批 H 新增；批 G 那一格是 9500–9501）。
#   ★ 选之前查过那张表 —— 而批 F 正是因为**表不全**才撞上压力那一格。
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
# ★ 上游是**枢衡自己**（与批 G / serve 同一个做法，零外部依赖）。
# ★ ★ 「每次不同」由客户端现给的 `X-Nonce` 提供，**不是**由时间或端口提供 ——
#   批 G 在这上面一口气栽了三次（`{time}` 秒级精度、`{remote_port}` 撞上连接池、
#   忘了带 nonce），三次都是「判据在最该报警的那一刻给一个像样的错答案」。
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

# ── 两份配置：磁盘后端 / 内存后端。★ 除了那一行 `disk`，**其余一模一样** ────
#
# ⚠ 这一点是判据的一部分：两次跑之间只差一行，于是第 2 格那对结果的差别
#   只可能来自后端本身，不可能来自别的什么。
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

# 内存后端那一份：把 `disk` 那行删掉，别的一个字不改。
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

# $1 = 配置文件，$2 = 日志文件名（**追加**，重启之后还能读到上一代说过什么）
start_cache() {
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$1" \
    --bind-host "$HOST" \
    --pid-file "$WORK/cache.pid" \
    --upgrade-sock "$WORK/cache.sock" \
    >> "$WORK/$2" 2>&1 &
  CACHE_PID=$!
  PIDS+=("$CACHE_PID")
}

# $1 = 信号（INT = 快停，TERM = 优雅）
stop_cache() {
  local sig=$1 waited=0
  [ -n "$CACHE_PID" ] || return 0
  kill -"$sig" "$CACHE_PID" 2>/dev/null || true
  # ⚠ ⚠ **只等这一个 pid，不能用光秃秃的 `wait`** —— 它会等本 shell 的全部后台作业，
  #   而上游那个 `fulcrum serve` **永远不退出**。批 G 就是这么在屏幕上挂了 9 分多钟，
  #   最后一行停在「=== [6/7] 防惊群 ===」，读起来像产品死锁。
  #   > ★ 一个会挂住的判据比一条红的判据更贵：红的指着问题，挂住的指着错误的方向。
  while kill -0 "$CACHE_PID" 2>/dev/null && [ "$waited" -lt 200 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  kill -9 "$CACHE_PID" 2>/dev/null || true
  # ★ 进程没了不等于端口放开了：下一次 bind 会撞 EADDRINUSE，
  #   而报错读起来像「端口被别人占了」。
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
  # ⚠ 先清空 body 与 hdr：curl 的 `-o` 在零字节时**不动**那个文件（批 F 栽过），
  #   于是上一次的内容会被当成这一次的。
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 10 "$@"
}

# 带一个**新的** nonce 打一次。★ 命中 ⇒ 体里是**上一次**那个 nonce。
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

# ★ ★ ★ **「东西已经落到盘上了」这句话，只能由产品自己说。**
#
# ⚠ ⚠ 数据面是**先把响应发完、再存缓存**的（`store_if_allowed` 排在
#   `write_response_body(…, true)` 之后，而且 body 落盘还带一次 `fsync`）——
#   那个顺序是对的：客户端不该等一次磁盘写。
#   ⇒ 于是 `curl` 返回的那一刻，盘上**可能还什么都没有**。
#
# ★ 这里容易栽：`purge` 之后打一发再 `find` 文件，
#   两次跑红在**两个不同的格子**（一次说「meta 有 body 没有」、一次说「没找到 body」）——
#   ⚠ 而它读起来完全像产品缺陷，我据此推出过一个很像真的错结论
#   （以为是后台清理把刚写下的 body 当成孤儿收走了；开 debug 日志一看，**一次都没收过**）。
#   > ★ 一条会随机红的判据，红与绿都不说明任何事；而它先教人重跑，再教人忽略。
#
# ⇒ 修法**不是** sleep 一下再看，而是：**再打一发，等它说命中**。
#   命中这件事只有在缓存里真的有那条时才成立 —— 用产品自己的可观测事实
#   把「存好了」这一刻钉死，与机器快慢无关。
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

# ── [1/8] 盘上真的有东西，而且是 G83 那个形状 ──────────────────────────────
echo
echo "=== [1/8] 磁盘后端：命中头带 -DISK，盘上是两级分片 + meta/body 两文件 ==="
probe_n "$BASE/maxage" > /dev/null
FIRST=$(body)
CODE=$(probe_n "$BASE/maxage")
expect_status "GET /maxage（第二次）" 200 "$CODE"
SECOND=$(body)
if [ "$FIRST" = "$SECOND" ]; then ok "命中（体没变）"; else
  fail "本该命中：第一次「$FIRST」第二次「$SECOND」"; fi
# ★ 后缀说的是「这个进程**真的**挑中了哪个后端」——配了 `disk` 而目录用不了时
#   缓存是关掉的，那时这个头一个都不会出现（第 7 格验它）。
if [ "$(hdr X-Fulcrum-Cache)" = "HIT-DISK" ]; then
  ok "X-Fulcrum-Cache = HIT-DISK（分得出内存与磁盘）"
else
  fail "X-Fulcrum-Cache 期望 HIT-DISK，实际「$(hdr X-Fulcrum-Cache)」"
fi

# G83 的布局：两级分片目录 · meta 与 body 两类文件。
METAS=$(find "$DISK" -name '*.meta' | wc -l)
BODIES=$(find "$DISK" -name '*.body' | wc -l)
if [ "$METAS" -ge 1 ] && [ "$BODIES" -ge 1 ]; then
  ok "盘上有 meta（$METAS）与 body（$BODIES）两类文件（G83）"
else
  fail "盘上没落下东西：meta=$METAS body=$BODIES"
  find "$DISK" -type f >&2 || true
fi
# ⚠ 分片深度**要数出来**，不是看一眼像不像：`<root>/<aa>/<bb>/<文件>` ⇒ 相对根有 3 段。
#   ★ 单层目录在几十万文件时会把清理任务本身拖垮，而那件事在门禁里永远不会发生
#   （门禁只存几条）—— 所以只能钉形状。
ONE_META=$(find "$DISK" -name '*.meta' | head -1)
REL=${ONE_META#"$DISK"/}
DEPTH=$(echo "$REL" | awk -F/ '{print NF}')
if [ "$DEPTH" = "3" ]; then ok "两级分片目录（$REL）"; else
  fail "分片深度期望 3 段，实际 $DEPTH 段：$REL"; fi

# ── [2/8] ★★★ 重启之后还在（快停：索引没来得及存）───────────────────────
echo
echo "=== [2/8] ★★★ 重启之后东西还在，而且启动不扫盘 ==="
KEEP=$SECOND   # 重启之前缓存里那一份（体里是第一次那个 nonce）
stop_cache INT
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: 重启之后 $PORT 没起来" >&2; cat "$WORK/disk.log" >&2; exit 1; }

# ⚠ ⚠ **这一步必须排在任何 GET 之前**：读路径会顺手把命中的那条记进索引，
#   于是先 GET 再问「索引里有几条」，问到的是被自己改过的数。
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

# ★ ★ 反证的另一半 —— **同一把尺子在内存后端上必须读出相反的结果**。
#   ⚠ 少了它，上面那条证明不了「是磁盘让它活下来的」：一个把缓存挂在
#   某处进程外状态上的实现、甚至一个根本没重启成功的脚本，都能让它绿。
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

# ── [3/8] 优雅停机把淘汰索引存下去（G84 的 save/load）─────────────────────
echo
echo "=== [3/8] 优雅停机存索引，下一代读得回来 ==="
rm -f "$DISK/index.json"
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: $PORT 没起来" >&2; exit 1; }
probe_n "$BASE/maxage" > /dev/null   # 让索引里至少有一条
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

# ── [4/8] 重验证只改 meta，不动 body ───────────────────────────────────────
echo
echo "=== [4/8] 重验证（304）只改 meta，不动 body ==="
admin /purge '{"all":true}' > /dev/null
probe_n "$BASE/reval" > /dev/null
settle_stored "/reval" "$BASE/reval" || true
# ⚠ ⚠ 「缓存里那份」要从**命中的那一发**取，不是从第一发取：第一发的响应
#   有没有被存下来，正是 `settle_stored` 在确认的那件事 —— 拿它当基准就是
#   把待证的结论当成了前提。★ 而命中回来的体，按定义就是缓存里那一份。
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
  # ★ 取**纳秒**精度的 mtime：秒级的话，一次落在同一秒里的改写会读不出差别，
  #   于是「meta 确实被改写了」那条反向判据会随机不成立。
  BODY_MTIME_BEFORE=$(stat -c %.9Y "$BODY_FILE")
  META_MTIME_BEFORE=$(stat -c %.9Y "$META_FILE")
  # ★ 用**请求侧** `max-age=0` 逼出重验证（当场生效），不等时钟。
  CODE=$(probe_n -H "Cache-Control: max-age=0" "$BASE/reval")
  expect_status "请求侧 max-age=0 ⇒ GET /reval" 200 "$CODE"
  # ★ 回给客户端的必须是**缓存里那份完整响应**，不是上游那个 304。
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
  # ★ ★ ★ **G83 把 meta 与 body 分开，理由就是这一条**：重验证是最常见的写操作之一，
  #   而它不该动 body（那可能是几 MB）。⚠ 一个「整条重写」的实现在**别的每一条判据上
  #   都是绿的** —— 内容对、头对、状态码对，只是每次 304 都把整个 body 重写一遍。
  if [ "$BODY_MTIME_BEFORE" = "$BODY_MTIME_AFTER" ]; then
    ok "★ ★ 重验证没动 body 文件（G83 分开存的理由本身）"
  else
    fail "★ ★ 重验证把 body 整个重写了：$BODY_MTIME_BEFORE → $BODY_MTIME_AFTER"
  fi
  # 反向那一半：meta **必须**变 —— 否则「只改 meta」这句话的前半截也没发生。
  if [ "$META_MTIME_BEFORE" != "$META_MTIME_AFTER" ]; then
    ok "★ 而 meta 确实被改写了（新鲜期续上了）"
  else
    fail "★ meta 没变 —— 那重验证根本没落地，上面那条 body 没变也就证明不了什么"
  fi
fi

# ── [5/8] purge 以盘为准，不是以索引为准 ──────────────────────────────────
echo
echo "=== [5/8] 索引是冷的时候，purge 照样清得掉 ==="
probe_n "$BASE/maxage" > /dev/null
stop_cache INT                       # 快停 ⇒ 索引不存盘
rm -f "$DISK/index.json"             # 连上一轮存的那份也拿掉
start_cache "$WORK/disk.Fulcrumfile" "disk.log"
wait_port "$PORT" || { echo "CACHE-DISK TESTS FAILED: $PORT 没起来" >&2; exit 1; }
# ⚠ ⚠ 一个**只看索引**的 purge 会在这一刻清掉 0 条，还回一句「清掉 0 条」——
#   而这正是有人会去按 purge 的那个时刻（刚重启、缓存里全是旧内容）。
#   ★ purge 的语义是「让它不在」，所以它必须以盘为准。
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

# ── [6/8] 坏条目即丢即删（读时校验，G84）──────────────────────────────────
echo
echo "=== [6/8] body 被截断 ⇒ 即丢即删，不是把半个页面发出去 ==="
admin /purge '{"all":true}' > /dev/null
probe_n "$BASE/maxage" > /dev/null
# ⚠ 同上：`curl` 返回不等于盘上有东西。等它自己说「命中」再去看文件。
settle_stored "/maxage" "$BASE/maxage" || true
GOOD=$(body)
BODY_FILE=$(find "$DISK" -name '*.body' | head -1)
if [ -z "$BODY_FILE" ]; then
  fail "没找到 body 文件，这一格证不到东西"
else
  printf 'xx' > "$BODY_FILE"        # 截断成两个字节
  CODE=$(probe_n "$BASE/maxage")
  expect_status "body 被截断之后 GET /maxage" 200 "$CODE"
  # ★ ★ 判据看的是**体**，不是状态码：一个不做读时校验的实现会把 `xx` 原样
  #   发出去，而它照样回 200、照样有 Content-Length —— 客户端拿到半个页面。
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

# ── [7/8] 目录用不了 ⇒ 关掉缓存，但照常转发 ───────────────────────────────
echo
echo "=== [7/8] 缓存目录用不了：关掉缓存，服务照常 ==="
stop_cache INT
# 用一个**文件**当缓存根 —— `create_dir_all` 会失败。
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
# ★ ★ **两条独立的信号，各自查一遍**：一条是「目录为什么用不了」（error），
#   另一条是「于是缓存关掉了」（装载结论）。⚠ 合成一条 grep 的话，
#   两行里少了任何一行都不会红 —— 而少了后面那行，运维只会看到一个红字，
#   看不到「所以现在到底是什么状态」。
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

# ── [8/8] 装载日志 + 编译期那条诊断 ───────────────────────────────────────
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
# ⚠ 批 G 留下的那条纪律：`ttl` 是**兜底**不是覆盖，装载时必须说出来。
if grep '缓存：站点' "$WORK/disk.log" | grep -q '兜底'; then
  ok "ttl 是兜底这件事仍然说得出来（G96）"
else
  fail "装载日志不再说 ttl 是兜底了 —— 批 G 那条可见性被本批弄丢了"
fi

# ★ ★ 缓存后端是**进程级**的：两个 `cache` 块写不同的 `disk` 必须编译不过，
#   而且要给那条**专门的**诊断（`FUL-DSL-0035`），不是一句「未知的子指令」。
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
# 反向那一半：一致的配置必须过。⚠ 少了它，一条「只要有两个 cache 就报错」的实现全绿。
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
