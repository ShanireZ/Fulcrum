#!/usr/bin/env bash
# 回應壓縮端到端（**M2 批 I**，G99–G102）。
#
# ★ 本場景驗四件事，而其中只有第三件是**新出現的危險**：
#   ① 現壓：反代與靜態檔案兩條路都真的壓了，而且**解出來等於原文**；
#   ② 預壓縮旁檔案：發的是旁檔案那幾個位元組，且強 ETag / Range **一樣都不掉**；
#   ③ ★★★ **壓縮 × 快取**（G101「壓完再存」）—— 快取裡存的是壓縮後的位元組，
#      於是「誰能拿到這一份」由次級鍵說了算，而**次級鍵寫錯一次就是給錯內容**；
#   ④ 不該壓的不壓（圖片、太小的、已經壓過的）。
#
# ★ ★ **判據不看「有沒有 Content-Encoding 頭」就算數** —— 那個頭是我們自己寫的，
#   寫死它也能騙過。凡是說「壓了」的地方都**真的解壓一遍再比**。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 連接埠表見 AGENTS.md。**9600–9601 是本場景的**（批 I 新增）。
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
# ★ ★ **解壓工具必須先自證**：本場景大半判據建在「解出來等於原文」上，
#   而一個不存在（或行為不同）的 `gzip -dc` 會讓那些斷言以另一種方式失敗，
#   排查方向整個跑偏。⇒ 拿一份已知答案的樣本當場量一次。
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

# ── 夾具 ────────────────────────────────────────────────────────────────────
#
# ★ 體要**夠長**：壓縮層對 `Content-Length < 20` 的回應一概不壓（那是它的
#   `MIN_COMPRESS_LEN`）。★ 也要**可壓** —— 一段重複的文字壓縮比很高，
#   於是「壓過的比原文短」這條附帶判據也成立。
BODY="AAAAAAAAAABBBBBBBBBBCCCCCCCCCCDDDDDDDDDDEEEEEEEEEEFFFFFFFFFFGGGGGGGGGGHHHHHHHHHH"
BODY="$BODY$BODY"

mkdir -p "$ROOT/static"
printf '%s' "$BODY" > "$ROOT/static/plain.txt"
# ★ 旁檔案裡放**一眼認得出**的內容 —— 於是「這幾個位元組來自旁檔案」是可以逐字斷言的，
#   而不是「它看起來像壓過的」。⚠ 它不必是真的 brotli：本格驗的是**選誰**，
#   而「選中的表示就原樣發出去」恰恰是被驗的那件事。
printf 'SIDECAR-BYTES-NOT-REAL-BROTLI' > "$ROOT/static/plain.txt.br"
# 陳舊的那一份：內容與上面那條同形，但 mtime 會被調到過去。
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
    # ⚠ 下面两条指着**同一台**上游，而且**有意都不写 id**（M2 批 N 任务 2.9 / G125，
    #   裁决 R6 ③ 第二轮）⇒ 它们的键「站点名 + id + 上游地址」完全相同，
    #   共享同一个覆盖格子，一次 disable 两条一起摘掉。
    #   ★ 这是「一个后端挂在几组 handle 路由后面」那个最常见的形状，它一个字节
    #     都不用改就装得上。写了 id 才分得开的那一路由 tests/serve 那份夹具覆盖。
    #   ⚠ 任务 2.8 曾照第一轮口径把这个形状在装载期拒掉，本场景当场装不上。
    # ⚠ ⚠ 这个 heredoc **不带引号**（因为要展开端口变量）⇒ 本行里的反引号
    #   会被当成命令替换。在这种 heredoc 里写注释一律用「」，别用反引号。
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
  # ⚠ **不加 `--compressed`**：那會讓 curl 自己解壓，於是「體到底是不是壓過的」
  #   這件事被它悄悄抹平了 —— 判據要看的正是線上那幾個位元組。
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

# ★ `Vary` 的值是一串**欄位名**，而欄位名大小寫不敏感（RFC 9110 §5.1）。
#   ⚠ 實測兩條路給的大小寫不同：壓縮層用 `http::header::ACCEPT_ENCODING`
#   （小寫），而旁檔案那條是我們自己寫的 `Accept-Encoding`。
#   ★ 兩者都合法 —— 所以判據必須**按語義比**，不能按位元組比：
#   一條按位元組比的斷言會在這裡紅，而它紅的不是產品的問題。
# ★ 頭名的**大小寫**逐字釘（G162）：壓縮層用 `http::header` 常數改寫 `ETag`、追加 `Vary`，
#   頭名照 pingora 的標題化表寫 —— 那兩個名是 fork 改動 17 補進表的，補之前它們以**小寫**上線
#   （`etag: W/…` · `vary: accept-encoding`）。上面的 `hdr` 不分大小寫，判不出這一族。
expect_header_name_exact() {
  local what=$1 name=$2
  if grep -q "^$name: " "$WORK/hdr"; then ok "$what 的头名原样是 $name"; else
    fail "$what 的头名期望原样「$name」，实际「$(grep -i "^$name:" "$WORK/hdr" | head -1 | cut -d: -f1)」"; fi
}

expect_vary_accept_encoding() {
  local what=$1 got
  got=$(hdr Vary | tr '[:upper:]' '[:lower:]')
  if [ "$got" = "accept-encoding" ]; then ok "$what 的 Vary = $(hdr Vary)"; else
    fail "$what 的 Vary 期望 Accept-Encoding（不分大小写），实际「$(hdr Vary)」"; fi
}

# ★ ★ **體可能是二進位**（壓過的），所以逐字比要走檔案，不能走 `$(…)` ——
#   命令替換會**丟掉 NUL 位元組**並打一行 warning，於是「相等」這件事被悄悄改寫了。
body_is() {
  local what=$1 want=$2
  printf '%s' "$want" > "$WORK/want"
  if cmp -s "$WORK/body" "$WORK/want"; then
    ok "$what"
    return 0
  fi
  return 1
}

# ── [1/6] 反代路上的現壓 ────────────────────────────────────────────────────
echo
echo "=== [1/6] 反代：真的压了，而且解出来等于原文 ==="
CODE=$(probe -H "Accept-Encoding: gzip" "$BASE/text")
expect_status "GET /text（带 Accept-Encoding: gzip）" 200 "$CODE"
expect_header "GET /text" "Content-Encoding" "gzip"
# ★ ★ ★ **判據在這裡**：不是「有那個頭」，是**解出來等於原文**。
#   ⚠ 一個只寫頭不壓的實作、或者一個漏掉壓縮收尾（gzip footer）的實作，
#   頭上一模一樣，而這一條會紅。
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
# ★ RFC 9110：內容隨 `Accept-Encoding` 變 ⇒ 必須有 `Vary`。
#   ⚠ 少了它，下游任何一層快取都會把這份 gzip 發給不認 gzip 的用戶端。
expect_vary_accept_encoding "GET /text"
expect_header_name_exact "GET /text（反代现压）" "Vary"
# ⚠ 現壓是流式的 ⇒ 長度事先不知道 ⇒ `Content-Length` 必然沒有。
expect_no_header "被压的响应" "Content-Length"

# ★ 反向那一半：**不帶** `Accept-Encoding` ⇒ 原樣。
#   ⚠ 少了它，一個「無論如何都壓」的實作會讓上面每一條都綠。
CODE=$(probe "$BASE/text")
expect_status "GET /text（不带 Accept-Encoding）" 200 "$CODE"
expect_no_header "不带 Accept-Encoding 的响应" "Content-Encoding"
if [ "$(raw_body)" = "$BODY" ]; then ok "★ 不带 Accept-Encoding 时拿到的是原文"; else
  fail "不带 Accept-Encoding 却拿到了别的东西"; fi

# ── [2/6] 不該壓的不壓 ──────────────────────────────────────────────────────
echo
echo "=== [2/6] 不该压的不压 ==="
probe -H "Accept-Encoding: gzip" "$BASE/png" > /dev/null
expect_no_header "image/png" "Content-Encoding"
probe -H "Accept-Encoding: gzip" "$BASE/tiny" > /dev/null
expect_no_header "太短的响应（5 字节）" "Content-Encoding"
# ★ ★ 上游已經壓過（自稱 gzip）而用戶端也認 gzip ⇒ **原樣轉發，不再壓一遍**。
#   ⚠ 雙重壓縮的現場是「用戶端解一次還是二進位」，而兩邊的頭都完全正常。
probe -H "Accept-Encoding: gzip" "$BASE/already" > /dev/null
expect_header "上游已压过的响应" "Content-Encoding" "gzip"
if [ "$(raw_body)" = "$BODY" ]; then
  ok "★ ★ 上游已压过的原样转发（没有被再压一遍）"
else
  fail "★ ★ 上游已压过的被动过了：收到 $(raw_body | wc -c) 字节，期望 ${#BODY}"
fi

# ── [3/6] 靜態檔案的現壓 ────────────────────────────────────────────────────
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
# ⚠ ⚠ **被現壓的回應沒有 Range** —— 這是正確的（區間說的是未壓縮的位元組），
#   但它是使用者看得見的行為，所以釘住它，別讓它在某天悄悄變回去。
expect_no_header "被现压的静态文件" "Accept-Ranges"
# ★ 被弱化的 ETag 與追加的 Vary 都是壓縮層用常數插的（G162）。
expect_header_name_exact "被现压的静态文件" "ETag"
expect_header_name_exact "被现压的静态文件" "Vary"

# ── [4/6] 預壓縮旁檔案 ──────────────────────────────────────────────────────
echo
echo "=== [4/6] 预压缩旁文件：发的是旁文件，且强 ETag 与 Range 都还在 ==="
CODE=$(probe -H "Accept-Encoding: br" "$BASE/static/plain.txt")
expect_status "GET /static/plain.txt（br）" 200 "$CODE"
expect_header "旁文件" "Content-Encoding" "br"
# ★ ★ ★ 逐字斷言：發出去的就是**旁檔案那幾個位元組**。
#   ⚠ 只看 `Content-Encoding: br` 的話，一個「現壓成 br 卻標成旁檔案」的實作也能過。
if [ "$(raw_body)" = "SIDECAR-BYTES-NOT-REAL-BROTLI" ]; then
  ok "★ ★ ★ 发出去的就是旁文件那几个字节"
else
  fail "★ ★ ★ 发的不是旁文件：「$(raw_body)」"
fi
expect_vary_accept_encoding "旁文件"
# ★ ★ **旁檔案比現壓強的地方，全在這三條上**：
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

# ⚠ ⚠ ⚠ **陳舊的旁檔案必須被當成不存在** —— 這一條 nginx 與 Caddy 預設都不做。
#   一個改了原檔案卻忘了重新生成旁檔案的部署，只會讓**支援 br 的那部分使用者**
#   拿到舊內容，不報任何錯、紀錄裡一行都沒有。
CODE=$(probe -H "Accept-Encoding: br" "$BASE/static/stale.txt")
expect_status "GET /static/stale.txt（旁文件是陈的）" 200 "$CODE"
if body_is "（占位）" "STALE-SIDECAR-MUST-NOT-BE-SERVED" > /dev/null; then
  fail "★ ★ ★ 陈旧的旁文件被发出去了 —— 用户拿到的是旧内容，而没有任何东西会说"
else
  ok "★ ★ ★ 陈旧的旁文件被当成不存在（发的不是它）"
fi

# ★ 反向那一半：用戶端**不認 br** 時，旁檔案一個位元組都不該出現。
CODE=$(probe -H "Accept-Encoding: gzip" "$BASE/static/plain.txt")
if body_is "（占位）" "SIDECAR-BYTES-NOT-REAL-BROTLI" > /dev/null; then
  fail "★ ★ ★ 把 br 旁文件发给了只认 gzip 的客户端 —— 它解不开"
else
  ok "★ 只认 gzip 的客户端拿不到 br 旁文件"
fi

# ── [5/6] ★★★ 壓縮 × 快取（G101）───────────────────────────────────────────
echo
echo "=== [5/6] ★★★ 压缩 × 缓存：存的是压缩后的字节，谁能拿到由次级键说了算 ==="
curl -s --unix-socket "$ADMIN_SOCK" -X POST --data-binary '{"all":true}' \
  --max-time 10 "http://localhost/purge" > /dev/null

# 第一發：gzip 用戶端，未命中 ⇒ 回源並存下壓縮後的位元組。
probe -H "Accept-Encoding: gzip" -H "X-Nonce: n1" "$BASE/cached" > /dev/null
FIRST=$(gunzipped)
expect_header "缓存第一发（gzip）" "Content-Encoding" "gzip"
if [ "$FIRST" = "$BODY-n1" ]; then ok "第一发解出来是 up 的原文（带 n1）"; else
  fail "第一发解出来不对：「$FIRST」"; fi

# ★ ★ ★ **歸一化的判據**：另一個**寫法不同、首選仍是 gzip** 的用戶端必須命中同一條。
#   ⚠ 拿 `Accept-Encoding` 原值當次級鍵的實作，這裡會**未命中**（體裡是 n2），
#   而那正是「快取被寫法炸開成幾十份」的可觀測形態。
probe -H "Accept-Encoding: gzip, deflate" -H "X-Nonce: n2" "$BASE/cached" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && [ "$(gunzipped)" = "$BODY-n1" ]; then
  ok "★ ★ ★ 写法不同但首选相同的客户端命中了**同一条**（次级键归一化生效）"
else
  fail "★ ★ ★ 归一化没生效：cache=「$(hdr X-Fulcrum-Cache)」解出「$(gunzipped)」（期望带 n1）"
fi

# ★ ★ ★ **不許串**：一個**不接受任何壓縮**的用戶端絕不能拿到那份 gzip 位元組。
probe -H "X-Nonce: n3" "$BASE/cached" > /dev/null
if [ -n "$(hdr Content-Encoding)" ]; then
  fail "★ ★ ★ 把 gzip 的字节发给了没说自己认 gzip 的客户端 —— 它解不开"
elif body_is "★ ★ ★ 不认压缩的客户端拿到的是它自己那一份未压缩的（回源取的）" "$BODY-n3"; then
  :
else
  fail "★ ★ ★ 不认压缩的客户端拿到了别的东西（$(wc -c < "$WORK/body") 字节）"
fi
# 再打一發 identity ⇒ 這次該命中 identity 那一份。
probe -H "X-Nonce: n4" "$BASE/cached" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && body_is "★ identity 那一份也进了缓存，第二次命中" "$BODY-n3"; then
  :
else
  fail "identity 那一份没缓存住：cache=「$(hdr X-Fulcrum-Cache)」"
fi
# ★ 而 gzip 那一份**還在**（兩份共存，不是互相擠掉）。
probe -H "Accept-Encoding: gzip" -H "X-Nonce: n5" "$BASE/cached" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && [ "$(gunzipped)" = "$BODY-n1" ]; then
  ok "★ 两种表示在缓存里共存，互不挤掉"
else
  fail "gzip 那一份被挤掉了：cache=「$(hdr X-Fulcrum-Cache)」"
fi

# ── [6/6] 裝載紀錄 ──────────────────────────────────────────────────────────
echo
echo "=== [6/6] 装载日志 ==="
# ⚠ ⚠ `encode` 從 M1 起就寫得下而執行環境不做（它在 UNWIRED 裡躺了整整一段）。
#   ⇒ 它**剛剛開始真的生效**這件事必須說出來：一個從舊版本升上來的站台，
#   行為在這一刻變了，而設定一個字都沒改。
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
# ★ 反向：`encode` 已經串接 ⇒ 裝載紀錄裡**不該**再把它報成未串接。
#   ⚠ 一條過期的警告不會紅，它只會訓練人不看那張表。
# ⚠ ⚠ **`{ … || true; }` 不是裝飾**：`set -euo pipefail` 下，第一個 `grep`
#   在**一條未串接警告都沒有**時退出 1，於是整條管道失敗、指令碼當場死掉 ——
#   而那恰恰是這條斷言**應當通過**的情況。★ 批 F 為同一個形狀栽過一次
#   （「只在斷言該通過時才崩的 helper」，現場是 exit 1 而零條 ✗）。
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
