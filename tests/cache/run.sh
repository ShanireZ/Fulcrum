#!/usr/bin/env bash
# 自研 HTTP 缓存的端到端（**M2 批 G**，G82–G84 + G95–G98）。
#
# ★ ★ ★ **判据为什么写成这个样子。** G82 拍板时把代价写在了明处：
#   > RFC 9111 语义、Vary、防惊群、元数据序列化 ≈4000 行全自己写，
#   > **而缓存的错表现为「偶尔给错内容」** —— 不像转发的错那样当场可见。
#   ⇒ 这里每一条断言都要能区分「**从缓存来的**」与「**回源来的**」，
#   而不只是「回了 200」。做法：**客户端每次带一个不同的 `X-Nonce`，上游把它回显出来** ——
#   于是「体里是上一次那个 nonce」＝命中、「是这一次的」＝回源。
#   ★ 这一点靠状态码是永远看不出来的：两者都是 200。
#
# ★ 与 `crates/fulcrum-server/src/cache/*` 里那 50 条单测的分工：
#   那边测**纯函数**（Cache-Control 解析、新鲜度、缓存键、Vary、LRU、防惊群闸），
#   这边测**真 socket 上真的发生了什么**。⚠ 两边都要有。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 端口表见 AGENTS.md。**9500–9501 是本场景的**（批 G 新增）。
#   ★ 选之前查过那张表 —— 而批 F 正是因为**表不全**才撞上压力那一格。
# ⚠ 本场景只用 9500–9501。⚠ 端口范围写宽了的话，AGENTS.md 那张表
#   写的也是 9500–9501 ⇒ 两处口径不一致，而多占的那个号会让下一个人绕开它。
#   ★ （批 H）改成与那张表一致，磁盘后端那一格因此正当地取了 9502–9503。
PORT=${CACHE_PORT:-9500}
UP_PORT=${CACHE_UP_PORT:-9501}
ADMIN_SOCK="$WORK/admin.sock"

FAILS=0
PIDS=()

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

echo "=== [0/7] 基线：端口未被占用 ==="
for p in "$PORT" "$UP_PORT"; do
  if port_listening "$p"; then
    echo "CACHE TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑。" >&2
    exit 1
  fi
done
ok "$PORT / $UP_PORT 都是空的"

[ -x "$BIN" ] || {
  echo "CACHE TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 上游：一个**把请求带来的 nonce 回显出去**的实例 ───────────────────────────
#
# ★ 上游是**枢衡自己**（与 serve 场景同一个做法，零外部依赖）：
#   `respond` 配 `header` 就能造出任意响应头。
#
# ★ ★ ★ **「每次不同」由请求带进来的 `X-Nonce` 提供，而不是由时间提供。**
#   ⚠ 写一个**根本不存在**的占位符（如 `time.now.unix_ms`）编译会当场红，
#   那是好事：DSL 的占位符表是穷尽的，打错字不会被当成字面量放过去。
#   ⚠ ⚠ 而就算写成真的那个 `{time}`，它也是**秒级**精度：两次连着的请求会撞出
#   同一个值，于是「体没变 ⇒ 命中」这条判据在快机器上**恒真**。
#   ⚠ ⚠ `{remote_port}` 同样不行：上游看到的是**枢衡→上游**那条连接的源端口，
#   而枢衡有**连接池** —— 复用池里的连接会给出同一个端口，
#   于是一次真的回源会被判成命中。★ 三种写法都是同一个形状：
#   **判据在最该报警的那一刻给一个像样的错答案。**
#   ⇒ nonce 由**客户端**每次现给：命中 ⇒ 体里是**上一次**的 nonce；回源 ⇒ 是这一次的。
#   ★ 它与连接池、时钟精度、机器快慢全都无关。
cat > "$WORK/up.Fulcrumfile" <<'CONF'
:UP_PORT {
    # ★ 每一条路径回一种 Cache-Control 组合，体里带一个**会变的值** ——
    #   于是「体没变」＝这次是从缓存来的，「体变了」＝回源了。
    handle /maxage {
        header Cache-Control "max-age=60"
        respond 200 "up-{header.X-Nonce}"
    }
    handle /nostore {
        header Cache-Control "no-store"
        respond 200 "up-{header.X-Nonce}"
    }
    handle /private {
        header Cache-Control "private, max-age=60"
        respond 200 "up-{header.X-Nonce}"
    }
    handle /nofresh {
        # 上游一个字都不说 —— 兜底 ttl 该在这里生效（G96）。
        respond 200 "up-{header.X-Nonce}"
    }
    handle /vary {
        header Cache-Control "max-age=60"
        header Vary "X-Flavor"
        respond 200 "flavor={header.X-Flavor}-{header.X-Nonce}"
    }
    handle /varystar {
        header Cache-Control "max-age=60"
        header Vary "*"
        respond 200 "up-{header.X-Nonce}"
    }
    handle /setcookie {
        header Cache-Control "max-age=60"
        header Set-Cookie "sid=abc"
        respond 200 "up-{header.X-Nonce}"
    }
    handle /authonly {
        header Cache-Control "max-age=60"
        respond 200 "secret-{header.X-Nonce}"
    }
    handle /err {
        header Cache-Control "max-age=60"
        respond 500 "boom-{header.X-Nonce}"
    }
    handle /big {
        header Cache-Control "max-age=60"
        respond 200 "0123456789012345678901234567890123456789012345678901234567890123456789"
    }
    respond 404 "no-such-path"
}
CONF
sed -i "s/:UP_PORT/:$UP_PORT/" "$WORK/up.Fulcrumfile"

cat > "$WORK/cache.Fulcrumfile" <<'CONF'
{
    admin unix/ADMIN_SOCK
}

:PORT {
    # ★ ★ 这个端点只给 [5/7] 的 G123 判据用：**被清掉的条目数自成一族**这件事，
    #   只有在一个真的缓存了东西、又真的被 purge 过的进程上才验得到。
    #   ⚠ 路径与上游那几条 handle 互不重叠。
    @m {
        remote_ip 127.0.0.0/8
        path /_metrics
    }
    handle @m {
        metrics
    }
    # ⚠ `max_size` 有意配得很小（50 字节）：/big 那条要验「超过单条目上限 ⇒ 不缓存」。
    cache {
        ttl 30s
        max_size 50B
        capacity 1MB
    }
    reverse_proxy 127.0.0.1:UP_PORT
}
CONF
sed -i "s/:PORT/:$PORT/; s/:UP_PORT/:$UP_PORT/; s|ADMIN_SOCK|$ADMIN_SOCK|" "$WORK/cache.Fulcrumfile"

start() {
  local name=$1 conf=$2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
start up "$WORK/up.Fulcrumfile"
start cache "$WORK/cache.Fulcrumfile"

wait_port "$UP_PORT" || { echo "CACHE TESTS FAILED: 上游 $UP_PORT 没起来" >&2; cat "$WORK/up.log" >&2; exit 1; }
wait_port "$PORT"    || { echo "CACHE TESTS FAILED: $PORT 没起来" >&2; cat "$WORK/cache.log" >&2; exit 1; }

BASE="http://$HOST:$PORT"

# ── helper ─────────────────────────────────────────────────────────────────
probe() {
  # ⚠ 先清空 body 与 hdr：curl 的 `-o` 在零字节时不动那个文件（批 F 栽过）。
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 5 "$@"
}

hdr() { { grep -i "^$1:" "$WORK/hdr" || true; } | head -1 | cut -d' ' -f2- | tr -d '\r'; }
body() { cat "$WORK/body"; }

expect_status() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望 $want，实际 $got"; fi
}

expect_header() {
  local what=$1 name=$2 want=$3 got
  got=$(hdr "$name")
  if [ "$got" = "$want" ]; then ok "$what 的 $name = $got"; else
    fail "$what 的 $name 期望「$want」，实际「$got」"; fi
}

# ★ ★ ★ **本场景最要紧的两个 helper**：它们分辨「命中」与「回源」，
#   而那件事**靠状态码永远看不出来** —— 两者都是 200。
expect_hit() {
  local what=$1 first=$2 second=$3 state
  state=$(hdr X-Fulcrum-Cache)
  if [ "$first" = "$second" ] && [ -n "$state" ]; then
    ok "$what 命中（体没变，X-Fulcrum-Cache=$state）"
  else
    fail "$what 本该命中：第一次「$first」第二次「$second」X-Fulcrum-Cache=「$state」"
  fi
}

expect_miss() {
  local what=$1 first=$2 second=$3 state
  state=$(hdr X-Fulcrum-Cache)
  if [ "$first" != "$second" ] && [ -z "$state" ]; then
    ok "$what 回源（体变了，没有 X-Fulcrum-Cache 头）"
  else
    fail "$what 本该回源：第一次「$first」第二次「$second」X-Fulcrum-Cache=「$state」"
  fi
}

# 打两次同一条路径，**每次给一个不同的 nonce**，回显两次的体。
# ★ 命中 ⇒ 第二次的体里仍是第一次那个 nonce；回源 ⇒ 是第二次那个。
NONCE=0
twice() {
  local url=$1
  shift
  NONCE=$((NONCE + 1))
  probe -H "X-Nonce: n$NONCE" "$@" "$url" > /dev/null
  local a
  a=$(body)
  NONCE=$((NONCE + 1))
  probe -H "X-Nonce: n$NONCE" "$@" "$url" > /dev/null
  local b
  b=$(body)
  printf '%s\n%s\n' "$a" "$b"
}

# ── [1/7] 命中与回源 ────────────────────────────────────────────────────────
echo
echo "=== [1/7] 命中与回源 ==="
OUT=$(twice "$BASE/maxage"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /maxage（上游给了 max-age=60）" "$A" "$B"
expect_header "GET /maxage 第二次" "X-Fulcrum-Cache" "HIT"
# ★ ★ `Age` 是 RFC 9111 §5.1 要求的 —— 下游若还有一层缓存，它靠这个头算自己的新鲜度。
#   ⚠ 不发的话，同一份内容会被一路放大到两倍寿命。
AGE=$(hdr Age)
if [ -n "$AGE" ]; then ok "命中带 Age 头（$AGE）"; else fail "命中却没有 Age 头（RFC 9111 §5.1）"; fi

# ★ ★ 反向那一半：`no-store` 的响应**不许**被缓存。
#   ⚠ 少了它，一个「什么都存」的实现会让上面那条全绿。
OUT=$(twice "$BASE/nostore"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /nostore（no-store）" "$A" "$B"

# ★ ★ 我们是**共享**缓存 ⇒ `private` 是禁令。
OUT=$(twice "$BASE/private"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /private（共享缓存不许存 private）" "$A" "$B"

# ★ ★ G96 的兜底：上游一个字都没说 ⇒ 用配置里的 `ttl 30s`。
OUT=$(twice "$BASE/nofresh"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /nofresh（上游没说 ⇒ 兜底 ttl 生效）" "$A" "$B"

# 错误响应不缓存（500 不在可缓存状态码表里）。
OUT=$(twice "$BASE/err"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /err（500 不可缓存）" "$A" "$B"

# ★ 超过单条目上限（配的是 50B，这条体 70B）⇒ 不缓存，但**照常发给客户端**。
CODE=$(probe "$BASE/big")
expect_status "GET /big（超过 max_size）" 200 "$CODE"
if [ "$(body | wc -c)" -ge 70 ]; then ok "超上限的响应照常完整发出去了"; else
  fail "超上限的响应被截断了：$(body | wc -c) 字节"; fi
probe "$BASE/big" > /dev/null
if [ -z "$(hdr X-Fulcrum-Cache)" ]; then ok "超上限的响应没进缓存"; else
  fail "超上限的响应被存进去了"; fi

# ── [2/7] Vary ─────────────────────────────────────────────────────────────
echo
echo "=== [2/7] Vary ==="
# 同一个 flavor 两次 ⇒ 命中
OUT=$(twice "$BASE/vary" -H "X-Flavor: a")
A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /vary（X-Flavor: a 两次）" "$A" "$B"
# ★ ★ 换一个 flavor ⇒ **不该**命中那一份，而且内容要对得上自己的 flavor。
#   ⚠ 这一条是 `Vary` 唯一真正会出错的地方：一个不看 Vary 的实现会把
#   flavor=a 的响应发给 flavor=b 的客户端 —— **状态码、头、长度全都正常**。
CODE=$(probe -H "X-Flavor: b" "$BASE/vary")
expect_status "GET /vary（X-Flavor: b）" 200 "$CODE"
if body | grep -q "flavor=b"; then ok "★ 换了 Vary 头拿到的是自己那一份"; else
  fail "★ ★ 拿到了别人那一份：$(body)"; fi
# 再来一次 b ⇒ 现在该命中 b 那一份了
OUT=$(twice "$BASE/vary" -H "X-Flavor: b")
A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /vary（X-Flavor: b 两次）" "$A" "$B"
# 而 a 那一份**还在**（两份共存，不是互相挤掉）。
probe -H "X-Flavor: a" "$BASE/vary" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && body | grep -q "flavor=a"; then
  ok "★ 两个 Vary 分支共存，互不挤掉"
else
  fail "a 那一份被挤掉了：X-Fulcrum-Cache=$(hdr X-Fulcrum-Cache) 体=$(body)"
fi

# `Vary: *` ⇒ 永远不可缓存。
OUT=$(twice "$BASE/varystar"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /varystar（Vary: *）" "$A" "$B"

# ── [3/7] 私有内容不许串号 ─────────────────────────────────────────────────
echo
echo "=== [3/7] 私有内容不许串号 ==="
# ★ ★ ★ **本场景最贵的一条**（RFC 9111 §3.5）：带 `Authorization` 的请求，
#   其响应对共享缓存默认不可存。⚠ 漏了它就是把一个人的私有页面发给下一个人 ——
#   而那件事**不会有任何报错**，只会在两个用户之间偶尔串一次内容。
# ⚠ ⚠ **两次请求要各带各的 nonce** —— 少了它，两个体都是 `secret-`（空 nonce），
#   于是「alice ≠ bob」这条断言**恒假**，而它红的时候看起来像产品出了问题。
#   ★ 实测栽过：报的是「bob 拿到了 alice 的响应」，而其实是判据没带值。
probe -H "Authorization: Bearer alice" -H "X-Nonce: alice1" "$BASE/authonly" > /dev/null
ALICE=$(body)
probe -H "Authorization: Bearer bob" -H "X-Nonce: bob1" "$BASE/authonly" > /dev/null
BOB=$(body)
if [ "$ALICE" != "$BOB" ] && [ -z "$(hdr X-Fulcrum-Cache)" ]; then
  ok "★ ★ 带 Authorization 的响应没被共享（alice 与 bob 拿到的不是同一份）"
else
  fail "★ ★ ★ bob 拿到了 alice 的响应：alice「$ALICE」bob「$BOB」cache=「$(hdr X-Fulcrum-Cache)」"
fi
# 连**没带**凭据的第三个人也不该捡到它。
probe -H "X-Nonce: anon1" "$BASE/authonly" > /dev/null
if [ -z "$(hdr X-Fulcrum-Cache)" ]; then ok "★ 匿名请求也没捡到那份私有响应"; else
  fail "★ ★ ★ 匿名请求命中了带 Authorization 那份：$(body)"; fi

# `Set-Cookie` 且没写 public ⇒ 不存（存了就要剥头，而剥错一次就是串号）。
OUT=$(twice "$BASE/setcookie"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /setcookie（带 Set-Cookie）" "$A" "$B"

# ── [4/7] 请求侧的 Cache-Control ───────────────────────────────────────────
echo
echo "=== [4/7] 请求侧 Cache-Control ==="
probe -H "X-Nonce: cc0" "$BASE/maxage" > /dev/null   # 先确保缓存里有
FIRST=$(body)
# `no-cache` ⇒ 客户端要求回源。
probe -H "Cache-Control: no-cache" -H "X-Nonce: cc1" "$BASE/maxage" > /dev/null
if [ "$(body)" != "$FIRST" ]; then ok "请求带 no-cache ⇒ 回源"; else
  fail "请求带 no-cache 却给了缓存的：$(body)"; fi
# `only-if-cached` 命中时正常给。
probe -H "Cache-Control: only-if-cached" -H "X-Nonce: cc2" "$BASE/maxage" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ]; then ok "only-if-cached 命中时正常给"; else
  fail "only-if-cached 命中时没给：$(hdr X-Fulcrum-Cache)"; fi
# ★ ★ `only-if-cached` **没命中 ⇒ 504**（RFC 9111 §5.2.1.7），不是回源。
#   ⚠ 回源的话，一个「离线模式」的客户端会在它以为自己不联网时把上游打醒。
CODE=$(probe -H "Cache-Control: only-if-cached" "$BASE/never-cached-path")
expect_status "★ only-if-cached 没命中 ⇒ 504" 504 "$CODE"
# `max-age=0` ⇒ 客户端不接受任何陈的 ⇒ 回源。
probe -H "X-Nonce: cc3" "$BASE/maxage" > /dev/null
BEFORE=$(body)
probe -H "Cache-Control: max-age=0" -H "X-Nonce: cc4" "$BASE/maxage" > /dev/null
if [ "$(body)" != "$BEFORE" ]; then ok "请求 max-age=0 ⇒ 回源"; else
  fail "请求 max-age=0 却给了缓存的"; fi

# ── [5/7] 管理面 purge ─────────────────────────────────────────────────────
echo
echo "=== [5/7] POST /purge ==="
admin() {
  curl -s --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    -w '\n%{http_code}' --max-time 5 "http://localhost$1"
}
probe -H "X-Nonce: pg1" "$BASE/maxage" > /dev/null
CACHED=$(body)
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ]; then ok "purge 之前是命中的"; else
  fail "purge 之前就没命中，后面那条证不到东西"; fi

RESP=$(admin /purge '{"url":{"method":"GET","scheme":"http","host":"127.0.0.1","path":"/maxage"}}')
CODE=$(printf '%s' "$RESP" | tail -1)
expect_status "POST /purge（按 url）" 200 "$CODE"
if printf '%s' "$RESP" | grep -q "清掉 1 条"; then ok "purge 说清掉了 1 条"; else
  fail "purge 的回话不对：$RESP"; fi

probe -H "X-Nonce: pg2" "$BASE/maxage" > /dev/null
if [ -z "$(hdr X-Fulcrum-Cache)" ] && [ "$(body)" != "$CACHED" ]; then
  ok "★ purge 之后真的回源了（体变了、没有命中头）"
else
  fail "★ purge 说清了，但下次请求还是命中：cache=$(hdr X-Fulcrum-Cache)"
fi

# ★ 清一个从没被缓存过的 URL ⇒ **200 而不是 404**：purge 的语义是「让它不在」，
#   而它本来就不在同样满足这个语义。⚠ 回 404 会让脚本里到处是重试。
RESP=$(admin /purge '{"key":"从来没有过的键"}')
expect_status "purge 一个不存在的键" 200 "$(printf '%s' "$RESP" | tail -1)"

RESP=$(admin /purge '{"all":true}')
expect_status "POST /purge（全清）" 200 "$(printf '%s' "$RESP" | tail -1)"

# 管理面不认识的路径仍然是 404（新加一个端点不该让别的变松）。
RESP=$(admin /nope '{}')
expect_status "POST /nope（管理面认识的那几个之外仍然是 404）" 404 "$(printf '%s' "$RESP" | tail -1)"

# ── ★ ★ ★ G123：被清掉的**条目**数自成一族，不再挤进 cache_events_total ────
#
# 这一格验的是**接线**：`admin.rs` 的 purge 收尾把真实条目数记进了新族。
# ⚠ 单测只证得了「这个族渲得出来」（它调的是 `record_purged_entries(0)`），
#   证不到「n 真的从 purge 那条路上传过来了」—— 那一段只有在这里走得到。
#
# 读一个**无标签** counter 的当前读数。
# ⛔ 它不是第二个 exposition 解析器（那一份是 `tests/metrics/run.sh` 的 `expo`）——
#   它只取一行已知形状的样本的第二个字段。⚠ 若第三个场景也要读 exposition，
#   正确做法是把 `expo` 抽进 `tests/lib/`，⛔ 不是再抄一份。
bare_counter() {
  awk -v n="$2" '$1 == n { print $2 }' "$1"
}
scrape_metrics() {
  curl -sS -o "$1" --max-time 5 "$BASE/_metrics" 2>/dev/null
}

# ★ 造 **3** 条缓存条目 —— ⚠ 有意不是 1 条：「记条目数」与「记 purge 调了几次」
#   在只清掉 1 条时给出**同一个读数**，那样的夹具两种实现都全绿。
for p in /maxage /nofresh /authonly; do
  probe -H "X-Nonce: g123" "$BASE$p" > /dev/null
done

scrape_metrics "$WORK/m-before.txt"
PURGED_BEFORE=$(bare_counter "$WORK/m-before.txt" fulcrum_cache_purged_entries_total)

# ★ ★ 取数端先自证：命中得了，也落空得了 —— 一个恒答空的读法会让下面每一条空转。
if [ -n "$PURGED_BEFORE" ]; then
  ok "取数端自证①：读得到 fulcrum_cache_purged_entries_total（$PURGED_BEFORE）"
else
  fail "取数端自证①：读不到 fulcrum_cache_purged_entries_total —— 下面几条证不到东西"
fi
if [ -z "$(bare_counter "$WORK/m-before.txt" fulcrum_不存在的族)" ]; then
  ok "取数端自证②：对不存在的族落空（不是恒答一个值）"
else
  fail "取数端自证②：对一个不存在的族也读出了东西 —— 这个读法是坏的"
fi

RESP=$(admin /purge '{"all":true}')
expect_status "POST /purge（全清，G123 判据用）" 200 "$(printf '%s' "$RESP" | tail -1)"
CLEARED=$(printf '%s' "$RESP" | sed -n 's/^清掉 \([0-9]\+\) 条.*/\1/p')

scrape_metrics "$WORK/m-after.txt"
PURGED_AFTER=$(bare_counter "$WORK/m-after.txt" fulcrum_cache_purged_entries_total)

if [ "$CLEARED" = "1" ] || [ -z "$CLEARED" ]; then
  fail "夹具纪律没守住：这一次只清掉了「$CLEARED」条 —— 要 ≥2 条，否则「记条目数」与「记调用次数」读数相同"
else
  ok "★ 夹具纪律：这一次清掉了 $CLEARED 条（不是 1 条）"
fi
# 浮点：exposition 里 counter 渲成 `3` 还是 `3.0` 由渲染器定 ⇒ 用 awk 算差，不做字符串比。
DELTA=$(awk -v a="$PURGED_AFTER" -v b="$PURGED_BEFORE" 'BEGIN { printf "%d", a - b }')
if [ "$DELTA" = "$CLEARED" ]; then
  ok "★★★ fulcrum_cache_purged_entries_total 正好涨了 $CLEARED —— 与 purge 回话里那个数逐一对得上"
else
  fail "★★★ 新族涨了 $DELTA，而 purge 说清掉了 $CLEARED 条（before=$PURGED_BEFORE after=$PURGED_AFTER）"
fi

# ★ ★ ★ 反向那半：**它不许再以 `event="purge"` 的形式出现**。
#   ⚠ 少了这一条，一个「两边都记一笔」的实现在上面每一条上都是绿的 ——
#     而那正是 G123 要消掉的形状（两个分母共用一个族）。
if grep -q 'fulcrum_cache_events_total{event="purge"}' "$WORK/m-after.txt"; then
  fail "★★★ purge 又回到 cache_events_total 里了 —— 它数的是条目，那个族数的是请求（G123）"
else
  ok "★★★ cache_events_total 里没有 event=\"purge\" —— 两个分母不再共用一个族（G123）"
fi
# ★ 而那个族本身**还在**，且真的有样本 —— 否则上面那条反向断言在「整个族都没渲出来」
#   的情况下也会绿，那时它证明的不是 G123，只是指标坏了。
if grep -q '^fulcrum_cache_events_total{event="' "$WORK/m-after.txt"; then
  ok "★ 而 cache_events_total 本身还在且有样本（上一条不是在对着一个空族说话）"
else
  fail "★ cache_events_total 一条样本都没有 —— 上面那条反向断言因此什么都没证明"
fi

# ── [6/7] 防惊群 ───────────────────────────────────────────────────────────
echo
echo "=== [6/7] 防惊群 ==="
# ★ ★ 同时打 20 个**同一条**没缓存过的 URL。它们要么拿到同一份（leader 存下、
#   follower 重新查到），要么各自回源 —— 而**正确的实现只回源一次**。
#   ⚠ 这里不数上游被打了几次（上游是枢衡、没有计数端点），
#   ★ 改为数**回来的不同内容有几种**：只回源一次 ⇒ 全都一样。
# ⚠ ⚠ ⚠ **每个并发请求必须带一个不同的 nonce** —— 少了它，20 个体全都是
#   `up-`（空 nonce），于是「只有 1 种内容」这条断言**恒成立**：
#   ★ 惊群挡没挡住，它给出的答案一模一样。**那是一条永远给绿的判据。**
#   ⇒ 带上 nonce 之后：挡住了 ⇒ 大家拿到 leader 那一个 nonce（1 种）；
#   没挡住 ⇒ 各自回源、各拿各的（20 种）。
admin /purge '{"all":true}' > /dev/null
rm -f "$WORK/herd."*
# ⚠ ⚠ ⚠ **只等这 20 个，不能用光秃秃的 `wait`。**
#   `wait` 不带参数会等**本 shell 的全部后台作业** —— 而 `start()` 把两个
#   `fulcrum serve` 也放在了后台，它们**永远不退出**。
#   ★ 实测：整个场景在这一行挂到外层超时，而屏幕上最后一行是 `=== [6/7] 防惊群 ===`，
#   看起来像是产品死锁了。
#   > **一个会挂住的判据比一条红的判据更贵**：红的指着问题，挂住的指着错误的方向。
HERD_PIDS=()
for i in $(seq 1 20); do
  curl -s --max-time 20 -H "X-Nonce: herd$i" "$BASE/maxage" -o "$WORK/herd.$i" &
  HERD_PIDS+=($!)
done
for pid in "${HERD_PIDS[@]}"; do wait "$pid" || true; done
# ⚠ ⚠ ⚠ **每份之间要补一个换行**：响应体**没有行尾**，直接 `cat` 会把 20 份
#   拼成**一行** ⇒ `sort -u | wc -l` 恒等于 1。★ 也就是说，不补这个换行的话，
#   「只有 1 种内容」这条断言**不管惊群挡没挡住都成立** —— 又一条永远给绿的判据，
#   而且它与上面那条（忘了带 nonce）是**同一批里的第二个**同形错误。
#   > **判据失效时它不沉默，而是给一个像样的错答案。**
DISTINCT=$(for f in "$WORK/herd."*; do cat "$f"; echo; done | sort -u | wc -l)
if [ "$DISTINCT" = "1" ]; then
  ok "★ ★ 20 个并发请求只回源了一次（收到的内容只有 1 种）"
else
  fail "★ ★ 惊群没挡住：20 个并发请求收到了 $DISTINCT 种不同的内容"
  for f in "$WORK/herd."*; do cat "$f"; echo; done | sort -u | head -5 >&2
fi
# ★ 反证那一半：**关掉并发**时每次都该拿到自己的 nonce（缓存已被上面填上，所以
#   这里改用一条没缓存过的路径）。⚠ 少了它，一个「把所有并发请求都回同一份」的
#   实现 —— 比如把 nonce 忽略掉 —— 会让上面那条全绿。
rm -f "$WORK/seq."*
for i in 1 2 3; do
  curl -s --max-time 10 -H "X-Nonce: seq$i" "$BASE/nostore" -o "$WORK/seq.$i"
done
SEQ_DISTINCT=$(for f in "$WORK/seq."*; do cat "$f"; echo; done | sort -u | wc -l)
if [ "$SEQ_DISTINCT" = "3" ]; then
  ok "★ 反证：不可缓存的路径上三次串行请求拿到三份不同的内容"
else
  fail "★ 反证不成立 —— nonce 根本没被回显？三次只有 $SEQ_DISTINCT 种"
fi

# ── [7/7] 装载日志 ─────────────────────────────────────────────────────────
echo
echo "=== [7/7] 装载日志 ==="
# ★ ★ `ttl` 是**兜底**不是覆盖（G96），而从 nginx 迁过来的人默认以为它是覆盖。
#   ⇒ 装载时必须说出来，与 G88 的 hide 清单同一条纪律。
if grep '缓存：站点' "$WORK/cache.log" | grep -q '兜底'; then
  ok "★ 装载日志说出了 ttl 是兜底"
else
  fail "装载日志没说 ttl 是兜底（G96 的可见性）"
  grep -i '缓存' "$WORK/cache.log" >&2 || head -30 "$WORK/cache.log" >&2
fi
if grep -q '内存后端' "$WORK/cache.log"; then
  ok "装载日志说了后端与容量上限"
else
  fail "装载日志没说后端"
fi
# ★ 回落层已经删掉 ⇒ 装载日志里**不该**再出现回落那几个字。
if grep -q '回落' "$WORK/cache.log"; then
  fail "装载日志里还有「回落」—— 那一层已于批 G 删除"
  grep '回落' "$WORK/cache.log" >&2
else
  ok "★ 装载日志里再没有回落（那一层已归零）"
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "CACHE TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 缓存实例日志 ──" >&2
  cat "$WORK/cache.log" >&2
  echo "── 上游日志 ──" >&2
  cat "$WORK/up.log" >&2
  exit 1
fi
echo "CACHE TESTS PASSED —— 自研缓存：命中/回源 / no-store·private·Set-Cookie 不存 / 兜底 ttl / Vary 两分支共存 / ★ Authorization 不串号 / 请求侧 CC 四条 / purge 三种 / 防惊群 / 装载日志。"
