#!/usr/bin/env bash
# 响应压缩端到端（**M2 批 I**，G99–G102）。
#
# ★ 本场景验四件事，而其中只有第三件是**新出现的危险**：
#   ① 现压：反代与静态文件两条路都真的压了，而且**解出来等于原文**；
#   ② 预压缩旁文件：发的是旁文件那几个字节，且强 ETag / Range **一样都不掉**；
#   ③ ★★★ **压缩 × 缓存**（G101「压完再存」）—— 缓存里存的是压缩后的字节，
#      于是「谁能拿到这一份」由次级键说了算，而**次级键写错一次就是给错内容**；
#   ④ 不该压的不压（图片、太小的、已经压过的）。
#
# ★ ★ **判据不看「有没有 Content-Encoding 头」就算数** —— 那个头是我们自己写的，
#   写死它也能骗过。凡是说「压了」的地方都**真的解压一遍再比**。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 端口表见 AGENTS.md。**9600–9601 是本场景的**（批 I 新增）。
PORT=${ENCODE_PORT:-9600}
UP_PORT=${ENCODE_UP_PORT:-9601}
ADMIN_SOCK="$WORK/admin.sock"
ROOT="$WORK/www"

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

echo "=== [0/6] 基线：端口未被占用、工具在 ==="
for p in "$PORT" "$UP_PORT"; do
  if port_listening "$p"; then
    echo "ENCODE TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑。" >&2
    exit 1
  fi
done
ok "$PORT / $UP_PORT 都是空的"
[ -x "$BIN" ] || {
  echo "ENCODE TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}
# ★ ★ **解压工具必须先自证**：本场景大半判据建在「解出来等于原文」上，
#   而一个不存在（或行为不同）的 `gzip -dc` 会让那些断言以另一种方式失败，
#   排查方向整个跑偏。⇒ 拿一份已知答案的样本当场量一次。
printf 'self-check-sample' > "$WORK/probe.txt"
gzip -c "$WORK/probe.txt" > "$WORK/probe.gz" 2>/dev/null || {
  echo "ENCODE TESTS FAILED: 容器里没有可用的 gzip —— 本场景的判据无从建立。" >&2
  exit 1
}
if [ "$(gzip -dc "$WORK/probe.gz")" = "self-check-sample" ]; then
  ok "gzip 压/解自证通过（判据建得起来）"
else
  echo "ENCODE TESTS FAILED: gzip 自证不通过 —— 下面每一条结论都不可信。" >&2
  exit 1
fi

# ── 夹具 ────────────────────────────────────────────────────────────────────
#
# ★ 体要**够长**：压缩层对 `Content-Length < 20` 的响应一概不压（那是它的
#   `MIN_COMPRESS_LEN`）。★ 也要**可压** —— 一段重复的文本压缩比很高，
#   于是「压过的比原文短」这条附带判据也成立。
BODY="AAAAAAAAAABBBBBBBBBBCCCCCCCCCCDDDDDDDDDDEEEEEEEEEEFFFFFFFFFFGGGGGGGGGGHHHHHHHHHH"
BODY="$BODY$BODY"

mkdir -p "$ROOT/static"
printf '%s' "$BODY" > "$ROOT/static/plain.txt"
# ★ 旁文件里放**一眼认得出**的内容 —— 于是「这几个字节来自旁文件」是可以逐字断言的，
#   而不是「它看起来像压过的」。⚠ 它不必是真的 brotli：本格验的是**选谁**，
#   而「选中的表示就原样发出去」恰恰是被验的那件事。
printf 'SIDECAR-BYTES-NOT-REAL-BROTLI' > "$ROOT/static/plain.txt.br"
# 陈旧的那一份：内容与上面那条同形，但 mtime 会被调到过去。
printf '%s' "$BODY" > "$ROOT/static/stale.txt"
printf 'STALE-SIDECAR-MUST-NOT-BE-SERVED' > "$ROOT/static/stale.txt.br"
touch -d '2020-01-01 00:00:00' "$ROOT/static/stale.txt.br"

cat > "$WORK/up.Fulcrumfile" <<CONF
:$UP_PORT {
    handle /text {
        header Content-Type "text/plain"
        respond 200 "$BODY"
    }
    handle /cached {
        header Content-Type "text/plain"
        header Cache-Control "max-age=60"
        respond 200 "$BODY-{header.X-Nonce}"
    }
    handle /png {
        header Content-Type "image/png"
        respond 200 "$BODY"
    }
    handle /already {
        header Content-Type "text/plain"
        header Content-Encoding "gzip"
        respond 200 "$BODY"
    }
    handle /tiny {
        header Content-Type "text/plain"
        respond 200 "short"
    }
    respond 404 "no-such-path"
}
CONF

cat > "$WORK/fulcrum.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

:$PORT {
    encode gzip br

    handle /static/* {
        file_server {
            root $ROOT
            precompressed br
        }
    }
    handle /cached {
        cache {
            ttl 60s
            capacity 1MB
        }
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
}
CONF

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
start fulcrum "$WORK/fulcrum.Fulcrumfile"
wait_port "$UP_PORT" || { echo "ENCODE TESTS FAILED: 上游没起来" >&2; cat "$WORK/up.log" >&2; exit 1; }
wait_port "$PORT"    || { echo "ENCODE TESTS FAILED: $PORT 没起来" >&2; cat "$WORK/fulcrum.log" >&2; exit 1; }

BASE="http://$HOST:$PORT"

# ── helper ─────────────────────────────────────────────────────────────────
probe() {
  : > "$WORK/body"
  : > "$WORK/hdr"
  # ⚠ **不加 `--compressed`**：那会让 curl 自己解压，于是「体到底是不是压过的」
  #   这件事被它悄悄抹平了 —— 判据要看的正是线上那几个字节。
  curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 10 "$@"
}
hdr() { { grep -i "^$1:" "$WORK/hdr" || true; } | head -1 | cut -d' ' -f2- | tr -d '\r'; }
raw_body() { cat "$WORK/body"; }
gunzipped() { gzip -dc "$WORK/body" 2>/dev/null || true; }

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

expect_no_header() {
  local what=$1 name=$2 got
  got=$(hdr "$name")
  if [ -z "$got" ]; then ok "$what 没有 $name（意料之中）"; else
    fail "$what 不该有 $name，实际「$got」"; fi
}

# ★ `Vary` 的值是一串**字段名**，而字段名大小写不敏感（RFC 9110 §5.1）。
#   ⚠ 实测两条路给的大小写不同：压缩层用 `http::header::ACCEPT_ENCODING`
#   （小写），而旁文件那条是我们自己写的 `Accept-Encoding`。
#   ★ 两者都合法 —— 所以判据必须**按语义比**，不能按字节比：
#   一条按字节比的断言会在这里红，而它红的不是产品的问题。
expect_vary_accept_encoding() {
  local what=$1 got
  got=$(hdr Vary | tr '[:upper:]' '[:lower:]')
  if [ "$got" = "accept-encoding" ]; then ok "$what 的 Vary = $(hdr Vary)"; else
    fail "$what 的 Vary 期望 Accept-Encoding（不分大小写），实际「$(hdr Vary)」"; fi
}

# ★ ★ **体可能是二进制**（压过的），所以逐字比要走文件，不能走 `$(…)` ——
#   命令替换会**丢掉 NUL 字节**并打一行 warning，于是「相等」这件事被悄悄改写了。
body_is() {
  local what=$1 want=$2
  printf '%s' "$want" > "$WORK/want"
  if cmp -s "$WORK/body" "$WORK/want"; then
    ok "$what"
    return 0
  fi
  return 1
}

# ── [1/6] 反代路上的现压 ────────────────────────────────────────────────────
echo
echo "=== [1/6] 反代：真的压了，而且解出来等于原文 ==="
CODE=$(probe -H "Accept-Encoding: gzip" "$BASE/text")
expect_status "GET /text（带 Accept-Encoding: gzip）" 200 "$CODE"
expect_header "GET /text" "Content-Encoding" "gzip"
# ★ ★ ★ **判据在这里**：不是「有那个头」，是**解出来等于原文**。
#   ⚠ 一个只写头不压的实现、或者一个漏掉压缩收尾（gzip footer）的实现，
#   头上一模一样，而这一条会红。
if [ "$(gunzipped)" = "$BODY" ]; then
  ok "★ ★ 解压出来与原文逐字相同（收尾那一块也在）"
else
  fail "★ ★ 解压出来与原文不同：收到 $(raw_body | wc -c) 字节，解出 $(gunzipped | wc -c) 字节"
fi
RAW_LEN=$(raw_body | wc -c)
if [ "$RAW_LEN" -lt "${#BODY}" ]; then
  ok "压过的确实更短（$RAW_LEN < ${#BODY}）"
else
  fail "压过的没有更短（$RAW_LEN vs ${#BODY}）—— 那多半根本没压"
fi
# ★ RFC 9110：内容随 `Accept-Encoding` 变 ⇒ 必须有 `Vary`。
#   ⚠ 少了它，下游任何一层缓存都会把这份 gzip 发给不认 gzip 的客户端。
expect_vary_accept_encoding "GET /text"
# ⚠ 现压是流式的 ⇒ 长度事先不知道 ⇒ `Content-Length` 必然没有。
expect_no_header "被压的响应" "Content-Length"

# ★ 反向那一半：**不带** `Accept-Encoding` ⇒ 原样。
#   ⚠ 少了它，一个「无论如何都压」的实现会让上面每一条都绿。
CODE=$(probe "$BASE/text")
expect_status "GET /text（不带 Accept-Encoding）" 200 "$CODE"
expect_no_header "不带 Accept-Encoding 的响应" "Content-Encoding"
if [ "$(raw_body)" = "$BODY" ]; then ok "★ 不带 Accept-Encoding 时拿到的是原文"; else
  fail "不带 Accept-Encoding 却拿到了别的东西"; fi

# ── [2/6] 不该压的不压 ──────────────────────────────────────────────────────
echo
echo "=== [2/6] 不该压的不压 ==="
probe -H "Accept-Encoding: gzip" "$BASE/png" > /dev/null
expect_no_header "image/png" "Content-Encoding"
probe -H "Accept-Encoding: gzip" "$BASE/tiny" > /dev/null
expect_no_header "太短的响应（5 字节）" "Content-Encoding"
# ★ ★ 上游已经压过（自称 gzip）而客户端也认 gzip ⇒ **原样转发，不再压一遍**。
#   ⚠ 双重压缩的现场是「客户端解一次还是二进制」，而两边的头都完全正常。
probe -H "Accept-Encoding: gzip" "$BASE/already" > /dev/null
expect_header "上游已压过的响应" "Content-Encoding" "gzip"
if [ "$(raw_body)" = "$BODY" ]; then
  ok "★ ★ 上游已压过的原样转发（没有被再压一遍）"
else
  fail "★ ★ 上游已压过的被动过了：收到 $(raw_body | wc -c) 字节，期望 ${#BODY}"
fi

# ── [3/6] 静态文件的现压 ────────────────────────────────────────────────────
echo
echo "=== [3/6] 静态文件：现压 ==="
CODE=$(probe -H "Accept-Encoding: gzip" "$BASE/static/plain.txt")
expect_status "GET /static/plain.txt（gzip）" 200 "$CODE"
expect_header "静态文件（gzip）" "Content-Encoding" "gzip"
if [ "$(gunzipped)" = "$BODY" ]; then
  ok "★ 静态文件解压出来与原文逐字相同"
else
  fail "★ 静态文件解压出来不对：解出 $(gunzipped | wc -c) 字节，期望 ${#BODY}"
fi
# ⚠ ⚠ **被现压的响应没有 Range** —— 这是正确的（区间说的是未压缩的字节），
#   但它是用户看得见的行为，所以钉住它，别让它在某天悄悄变回去。
expect_no_header "被现压的静态文件" "Accept-Ranges"

# ── [4/6] 预压缩旁文件 ──────────────────────────────────────────────────────
echo
echo "=== [4/6] 预压缩旁文件：发的是旁文件，且强 ETag 与 Range 都还在 ==="
CODE=$(probe -H "Accept-Encoding: br" "$BASE/static/plain.txt")
expect_status "GET /static/plain.txt（br）" 200 "$CODE"
expect_header "旁文件" "Content-Encoding" "br"
# ★ ★ ★ 逐字断言：发出去的就是**旁文件那几个字节**。
#   ⚠ 只看 `Content-Encoding: br` 的话，一个「现压成 br 却标成旁文件」的实现也能过。
if [ "$(raw_body)" = "SIDECAR-BYTES-NOT-REAL-BROTLI" ]; then
  ok "★ ★ ★ 发出去的就是旁文件那几个字节"
else
  fail "★ ★ ★ 发的不是旁文件：「$(raw_body)」"
fi
expect_vary_accept_encoding "旁文件"
# ★ ★ **旁文件比现压强的地方，全在这三条上**：
ETAG=$(hdr ETag)
case "$ETAG" in
  'W/'*) fail "★ 旁文件的 ETag 被弱化了（$ETAG）—— 它是一个真实文件，该是强 ETag" ;;
  '"'*) ok "★ 旁文件保住了**强** ETag（$ETAG）" ;;
  *) fail "★ 旁文件没有 ETag（实际「$ETAG」）" ;;
esac
expect_header "旁文件" "Accept-Ranges" "bytes"
CODE=$(probe -H "Accept-Encoding: br" -H "Range: bytes=0-6" "$BASE/static/plain.txt")
expect_status "★ 旁文件上的 Range" 206 "$CODE"
if [ "$(raw_body)" = "SIDECAR" ]; then
  ok "★ ★ 旁文件的 Range 取到的是旁文件的前 7 个字节"
else
  fail "★ ★ 旁文件的 Range 不对：「$(raw_body)」"
fi

# ⚠ ⚠ ⚠ **陈旧的旁文件必须被当成不存在** —— 这一条 nginx 与 Caddy 默认都不做。
#   一个改了原文件却忘了重新生成旁文件的部署，只会让**支持 br 的那部分用户**
#   拿到旧内容，不报任何错、日志里一行都没有。
CODE=$(probe -H "Accept-Encoding: br" "$BASE/static/stale.txt")
expect_status "GET /static/stale.txt（旁文件是陈的）" 200 "$CODE"
if body_is "（占位）" "STALE-SIDECAR-MUST-NOT-BE-SERVED" > /dev/null; then
  fail "★ ★ ★ 陈旧的旁文件被发出去了 —— 用户拿到的是旧内容，而没有任何东西会说"
else
  ok "★ ★ ★ 陈旧的旁文件被当成不存在（发的不是它）"
fi

# ★ 反向那一半：客户端**不认 br** 时，旁文件一个字节都不该出现。
CODE=$(probe -H "Accept-Encoding: gzip" "$BASE/static/plain.txt")
if body_is "（占位）" "SIDECAR-BYTES-NOT-REAL-BROTLI" > /dev/null; then
  fail "★ ★ ★ 把 br 旁文件发给了只认 gzip 的客户端 —— 它解不开"
else
  ok "★ 只认 gzip 的客户端拿不到 br 旁文件"
fi

# ── [5/6] ★★★ 压缩 × 缓存（G101）───────────────────────────────────────────
echo
echo "=== [5/6] ★★★ 压缩 × 缓存：存的是压缩后的字节，谁能拿到由次级键说了算 ==="
curl -s --unix-socket "$ADMIN_SOCK" -X POST --data-binary '{"all":true}' \
  --max-time 10 "http://localhost/purge" > /dev/null

# 第一发：gzip 客户端，未命中 ⇒ 回源并存下压缩后的字节。
probe -H "Accept-Encoding: gzip" -H "X-Nonce: n1" "$BASE/cached" > /dev/null
FIRST=$(gunzipped)
expect_header "缓存第一发（gzip）" "Content-Encoding" "gzip"
if [ "$FIRST" = "$BODY-n1" ]; then ok "第一发解出来是 up 的原文（带 n1）"; else
  fail "第一发解出来不对：「$FIRST」"; fi

# ★ ★ ★ **归一化的判据**：另一个**写法不同、首选仍是 gzip** 的客户端必须命中同一条。
#   ⚠ 拿 `Accept-Encoding` 原值当次级键的实现，这里会**未命中**（体里是 n2），
#   而那正是「缓存被写法炸开成几十份」的可观测形态。
probe -H "Accept-Encoding: gzip, deflate" -H "X-Nonce: n2" "$BASE/cached" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && [ "$(gunzipped)" = "$BODY-n1" ]; then
  ok "★ ★ ★ 写法不同但首选相同的客户端命中了**同一条**（次级键归一化生效）"
else
  fail "★ ★ ★ 归一化没生效：cache=「$(hdr X-Fulcrum-Cache)」解出「$(gunzipped)」（期望带 n1）"
fi

# ★ ★ ★ **不许串**：一个**不接受任何压缩**的客户端绝不能拿到那份 gzip 字节。
probe -H "X-Nonce: n3" "$BASE/cached" > /dev/null
if [ -n "$(hdr Content-Encoding)" ]; then
  fail "★ ★ ★ 把 gzip 的字节发给了没说自己认 gzip 的客户端 —— 它解不开"
elif body_is "★ ★ ★ 不认压缩的客户端拿到的是它自己那一份未压缩的（回源取的）" "$BODY-n3"; then
  :
else
  fail "★ ★ ★ 不认压缩的客户端拿到了别的东西（$(wc -c < "$WORK/body") 字节）"
fi
# 再打一发 identity ⇒ 这次该命中 identity 那一份。
probe -H "X-Nonce: n4" "$BASE/cached" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && body_is "★ identity 那一份也进了缓存，第二次命中" "$BODY-n3"; then
  :
else
  fail "identity 那一份没缓存住：cache=「$(hdr X-Fulcrum-Cache)」"
fi
# ★ 而 gzip 那一份**还在**（两份共存，不是互相挤掉）。
probe -H "Accept-Encoding: gzip" -H "X-Nonce: n5" "$BASE/cached" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && [ "$(gunzipped)" = "$BODY-n1" ]; then
  ok "★ 两种表示在缓存里共存，互不挤掉"
else
  fail "gzip 那一份被挤掉了：cache=「$(hdr X-Fulcrum-Cache)」"
fi

# ── [6/6] 装载日志 ──────────────────────────────────────────────────────────
echo
echo "=== [6/6] 装载日志 ==="
# ⚠ ⚠ `encode` 从 M1 起就写得下而运行时不做（它在 UNWIRED 里躺了整整一段）。
#   ⇒ 它**刚刚开始真的生效**这件事必须说出来：一个从旧版本升上来的站点，
#   行为在这一刻变了，而配置一个字都没改。
if grep -q '压缩：站点' "$WORK/fulcrum.log"; then
  ok "装载日志说了哪些站点在压、压哪几种"
else
  fail "装载日志没说压缩"
  grep -i '压缩\|encode' "$WORK/fulcrum.log" >&2 || true
fi
if grep -q '预压缩旁文件' "$WORK/fulcrum.log"; then
  ok "装载日志说了预压缩旁文件"
else
  fail "装载日志没说预压缩旁文件"
fi
# ★ 反向：`encode` 已经接线 ⇒ 装载日志里**不该**再把它报成未接线。
#   ⚠ 一条过期的警告不会红，它只会训练人不看那张表。
# ⚠ ⚠ **`{ … || true; }` 不是装饰**：`set -euo pipefail` 下，第一个 `grep`
#   在**一条未接线警告都没有**时退出 1，于是整条管道失败、脚本当场死掉 ——
#   而那恰恰是这条断言**应当通过**的情况。★ 批 F 为同一个形状栽过一次
#   （「只在断言该通过时才崩的 helper」，现场是 exit 1 而零条 ✗）。
if { grep '这一批还没接线' "$WORK/fulcrum.log" || true; } | grep -q 'encode'; then
  fail "装载日志还把 encode 报成未接线 —— 那张表过期了"
else
  ok "★ 装载日志不再把 encode 报成未接线"
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "ENCODE TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 枢衡日志 ──" >&2
  cat "$WORK/fulcrum.log" >&2
  echo "── 上游日志 ──" >&2
  cat "$WORK/up.log" >&2
  exit 1
fi
echo "ENCODE TESTS PASSED —— 压缩：反代与静态文件两条路真的压了（解出来逐字等于原文）/ 不该压的三种都没压 / ★ 预压缩旁文件逐字发出且强 ETag 与 Range 都在 / ★ 陈旧旁文件被当成不存在 / ★★★ 压缩 × 缓存：写法不同但首选相同的命中同一条、不认压缩的拿不到压缩字节、两种表示共存 / 装载日志。"
