#!/usr/bin/env bash
# 自研 HTTP 快取的端到端（**M2 批 G**，G82–G84 + G95–G98）。
#
# ★ ★ ★ **判據為什麼寫成這個樣子。** G82 拍板時把代價寫在了明處：
#   > RFC 9111 語義、Vary、防驚群、中繼資料序列化 ≈4000 行全自己寫，
#   > **而快取的錯表現為「偶爾給錯內容」** —— 不像轉發的錯那樣當場可見。
#   ⇒ 這裡每一條斷言都要能區分「**從快取來的**」與「**回源來的**」，
#   而不只是「回了 200」。做法：**用戶端每次帶一個不同的 `X-Nonce`，上游把它回顯出來** ——
#   於是「體裡是上一次那個 nonce」＝命中、「是這一次的」＝回源。
#   ★ 這一點靠狀態碼是永遠看不出來的：兩者都是 200。
#
# ★ 與 `crates/fulcrum-server/src/cache/*` 裡那 50 條單測的分工：
#   那邊測**純函式**（Cache-Control 解析、新鮮度、快取鍵、Vary、LRU、防驚群閘），
#   這邊測**真 socket 上真的發生了什麼**。⚠ 兩邊都要有。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 連接埠表見 AGENTS.md。**9500–9501 是本場景的**（批 G 新增）。
#   ★ 選之前查過那張表 —— 而批 F 正是因為**表不全**才撞上壓力那一格。
# ⚠ 本場景只用 9500–9501。⚠ 連接埠範圍寫寬了的話，AGENTS.md 那張表
#   寫的也是 9500–9501 ⇒ 兩處口徑不一致，而多佔的那個號會讓下一個人繞開它。
#   ★ （批 H）改成與那張表一致，磁碟後端那一格因此正當地取了 9502–9503。
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

# ── 上游：一個**把請求帶來的 nonce 回顯出去**的執行個體 ───────────────────────────
#
# ★ 上游是**樞衡自己**（與 serve 場景同一個做法，零外部相依）：
#   `respond` 配 `header` 就能造出任意回應頭。
#
# ★ ★ ★ **「每次不同」由請求帶進來的 `X-Nonce` 提供，而不是由時間提供。**
#   ⚠ 寫一個**根本不存在**的佔位符（如 `time.now.unix_ms`）編譯會當場紅，
#   那是好事：DSL 的佔位符表是窮盡的，打錯字不會被當成字面值放過去。
#   ⚠ ⚠ 而就算寫成真的那個 `{time}`，它也是**秒級**精度：兩次連著的請求會撞出
#   同一個值，於是「體沒變 ⇒ 命中」這條判據在快機器上**恆真**。
#   ⚠ ⚠ `{remote_port}` 同樣不行：上游看到的是**樞衡→上游**那條連接的源連接埠，
#   而樞衡有**連接池** —— 重複使用池裡的連接會給出同一個連接埠，
#   於是一次真的回源會被判成命中。★ 三種寫法都是同一個形狀：
#   **判據在最該報警的那一刻給一個像樣的錯答案。**
#   ⇒ nonce 由**用戶端**每次現給：命中 ⇒ 體裡是**上一次**的 nonce；回源 ⇒ 是這一次的。
#   ★ 它與連接池、時鐘精度、機器快慢全都無關。
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
  # ⚠ 先清空 body 與 hdr：curl 的 `-o` 在零位元組時不動那個檔案（批 F 栽過）。
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

# ★ ★ ★ **本場景最要緊的兩個 helper**：它們分辨「命中」與「回源」，
#   而那件事**靠狀態碼永遠看不出來** —— 兩者都是 200。
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

# 打兩次同一條路徑，**每次給一個不同的 nonce**，回顯兩次的體。
# ★ 命中 ⇒ 第二次的體裡仍是第一次那個 nonce；回源 ⇒ 是第二次那個。
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

# ── [1/7] 命中與回源 ────────────────────────────────────────────────────────
echo
echo "=== [1/7] 命中与回源 ==="
OUT=$(twice "$BASE/maxage"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /maxage（上游给了 max-age=60）" "$A" "$B"
expect_header "GET /maxage 第二次" "X-Fulcrum-Cache" "HIT"
# ★ ★ `Age` 是 RFC 9111 §5.1 要求的 —— 下游若還有一層快取，它靠這個頭算自己的新鮮度。
#   ⚠ 不發的話，同一份內容會被一路放大到兩倍壽命。
AGE=$(hdr Age)
if [ -n "$AGE" ]; then ok "命中带 Age 头（$AGE）"; else fail "命中却没有 Age 头（RFC 9111 §5.1）"; fi

# ★ ★ 反向那一半：`no-store` 的回應**不許**被快取。
#   ⚠ 少了它，一個「什麼都存」的實作會讓上面那條全綠。
OUT=$(twice "$BASE/nostore"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /nostore（no-store）" "$A" "$B"

# ★ ★ 我們是**共享**快取 ⇒ `private` 是禁令。
OUT=$(twice "$BASE/private"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /private（共享缓存不许存 private）" "$A" "$B"

# ★ ★ G96 的備援：上游一個字都沒說 ⇒ 用設定裡的 `ttl 30s`。
OUT=$(twice "$BASE/nofresh"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /nofresh（上游没说 ⇒ 兜底 ttl 生效）" "$A" "$B"

# 錯誤回應不快取（500 不在可快取狀態碼錶裡）。
OUT=$(twice "$BASE/err"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /err（500 不可缓存）" "$A" "$B"

# ★ 超過單條目上限（配的是 50B，這條體 70B）⇒ 不快取，但**照常發給用戶端**。
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
# 同一個 flavor 兩次 ⇒ 命中
OUT=$(twice "$BASE/vary" -H "X-Flavor: a")
A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /vary（X-Flavor: a 两次）" "$A" "$B"
# ★ ★ 換一個 flavor ⇒ **不該**命中那一份，而且內容要對得上自己的 flavor。
#   ⚠ 這一條是 `Vary` 唯一真正會出錯的地方：一個不看 Vary 的實作會把
#   flavor=a 的回應發給 flavor=b 的用戶端 —— **狀態碼、頭、長度全都正常**。
CODE=$(probe -H "X-Flavor: b" "$BASE/vary")
expect_status "GET /vary（X-Flavor: b）" 200 "$CODE"
if body | grep -q "flavor=b"; then ok "★ 换了 Vary 头拿到的是自己那一份"; else
  fail "★ ★ 拿到了别人那一份：$(body)"; fi
# 再來一次 b ⇒ 現在該命中 b 那一份了
OUT=$(twice "$BASE/vary" -H "X-Flavor: b")
A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_hit "GET /vary（X-Flavor: b 两次）" "$A" "$B"
# 而 a 那一份**還在**（兩份共存，不是互相擠掉）。
probe -H "X-Flavor: a" "$BASE/vary" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ] && body | grep -q "flavor=a"; then
  ok "★ 两个 Vary 分支共存，互不挤掉"
else
  fail "a 那一份被挤掉了：X-Fulcrum-Cache=$(hdr X-Fulcrum-Cache) 体=$(body)"
fi

# `Vary: *` ⇒ 永遠不可快取。
OUT=$(twice "$BASE/varystar"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /varystar（Vary: *）" "$A" "$B"

# ── [3/7] 私有內容不許串號 ─────────────────────────────────────────────────
echo
echo "=== [3/7] 私有内容不许串号 ==="
# ★ ★ ★ **本場景最貴的一條**（RFC 9111 §3.5）：帶 `Authorization` 的請求，
#   其回應對共享快取預設不可存。⚠ 漏了它就是把一個人的私有頁面發給下一個人 ——
#   而那件事**不會有任何回報錯誤**，只會在兩個使用者之間偶爾串一次內容。
# ⚠ ⚠ **兩次請求要各帶各的 nonce** —— 少了它，兩個體都是 `secret-`（空 nonce），
#   於是「alice ≠ bob」這條斷言**恆假**，而它紅的時候看起來像產品出了問題。
#   ★ 實測栽過：報的是「bob 拿到了 alice 的回應」，而其實是判據沒帶值。
probe -H "Authorization: Bearer alice" -H "X-Nonce: alice1" "$BASE/authonly" > /dev/null
ALICE=$(body)
probe -H "Authorization: Bearer bob" -H "X-Nonce: bob1" "$BASE/authonly" > /dev/null
BOB=$(body)
if [ "$ALICE" != "$BOB" ] && [ -z "$(hdr X-Fulcrum-Cache)" ]; then
  ok "★ ★ 带 Authorization 的响应没被共享（alice 与 bob 拿到的不是同一份）"
else
  fail "★ ★ ★ bob 拿到了 alice 的响应：alice「$ALICE」bob「$BOB」cache=「$(hdr X-Fulcrum-Cache)」"
fi
# 連**沒帶**憑證的第三個人也不該撿到它。
probe -H "X-Nonce: anon1" "$BASE/authonly" > /dev/null
if [ -z "$(hdr X-Fulcrum-Cache)" ]; then ok "★ 匿名请求也没捡到那份私有响应"; else
  fail "★ ★ ★ 匿名请求命中了带 Authorization 那份：$(body)"; fi

# `Set-Cookie` 且沒寫 public ⇒ 不存（存了就要剝頭，而剝錯一次就是串號）。
OUT=$(twice "$BASE/setcookie"); A=$(printf '%s' "$OUT" | head -1); B=$(printf '%s' "$OUT" | tail -1)
expect_miss "GET /setcookie（带 Set-Cookie）" "$A" "$B"

# ── [4/7] 請求側的 Cache-Control ───────────────────────────────────────────
echo
echo "=== [4/7] 请求侧 Cache-Control ==="
probe -H "X-Nonce: cc0" "$BASE/maxage" > /dev/null   # 先確保快取裡有
FIRST=$(body)
# `no-cache` ⇒ 用戶端要求回源。
probe -H "Cache-Control: no-cache" -H "X-Nonce: cc1" "$BASE/maxage" > /dev/null
if [ "$(body)" != "$FIRST" ]; then ok "请求带 no-cache ⇒ 回源"; else
  fail "请求带 no-cache 却给了缓存的：$(body)"; fi
# `only-if-cached` 命中時正常給。
probe -H "Cache-Control: only-if-cached" -H "X-Nonce: cc2" "$BASE/maxage" > /dev/null
if [ "$(hdr X-Fulcrum-Cache)" = "HIT" ]; then ok "only-if-cached 命中时正常给"; else
  fail "only-if-cached 命中时没给：$(hdr X-Fulcrum-Cache)"; fi
# ★ ★ `only-if-cached` **沒命中 ⇒ 504**（RFC 9111 §5.2.1.7），不是回源。
#   ⚠ 回源的話，一個「離線模式」的用戶端會在它以為自己不聯網時把上游打醒。
CODE=$(probe -H "Cache-Control: only-if-cached" "$BASE/never-cached-path")
expect_status "★ only-if-cached 没命中 ⇒ 504" 504 "$CODE"
# `max-age=0` ⇒ 用戶端不接受任何陳的 ⇒ 回源。
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

# ★ 清一個從沒被快取過的 URL ⇒ **200 而不是 404**：purge 的語義是「讓它不在」，
#   而它本來就不在同樣滿足這個語義。⚠ 回 404 會讓指令碼裡到處是重試。
RESP=$(admin /purge '{"key":"从来没有过的键"}')
expect_status "purge 一个不存在的键" 200 "$(printf '%s' "$RESP" | tail -1)"

RESP=$(admin /purge '{"all":true}')
expect_status "POST /purge（全清）" 200 "$(printf '%s' "$RESP" | tail -1)"

# 管理面不認識的路徑仍然是 404（新加一個端點不該讓別的變鬆）。
RESP=$(admin /nope '{}')
expect_status "POST /nope（管理面认识的那几个之外仍然是 404）" 404 "$(printf '%s' "$RESP" | tail -1)"

# ── ★ ★ ★ G123：被清掉的**條目**數自成一族，不再擠進 cache_events_total ────
#
# 這一格驗的是**串接**：`admin.rs` 的 purge 收尾把真實條目數記進了新族。
# ⚠ 單測只證得了「這個族渲得出來」（它調的是 `record_purged_entries(0)`），
#   證不到「n 真的從 purge 那條路上傳過來了」—— 那一段只有在這裡走得到。
#
# 讀一個**無標籤** counter 的當前讀數。
# ⛔ 它不是第二個 exposition 解析器（那一份是 `tests/metrics/run.sh` 的 `expo`）——
#   它只取一行已知形狀的樣本的第二個欄位。⚠ 若第三個場景也要讀 exposition，
#   正確做法是把 `expo` 抽進 `tests/lib/`，⛔ 不是再抄一份。
bare_counter() {
  awk -v n="$2" '$1 == n { print $2 }' "$1"
}
scrape_metrics() {
  curl -sS -o "$1" --max-time 5 "$BASE/_metrics" 2>/dev/null
}

# ★ 造 **3** 條快取條目 —— ⚠ 有意不是 1 條：「記條目數」與「記 purge 調了幾次」
#   在只清掉 1 條時給出**同一個讀數**，那樣的夾具兩種實作都全綠。
for p in /maxage /nofresh /authonly; do
  probe -H "X-Nonce: g123" "$BASE$p" > /dev/null
done

scrape_metrics "$WORK/m-before.txt"
PURGED_BEFORE=$(bare_counter "$WORK/m-before.txt" fulcrum_cache_purged_entries_total)

# ★ ★ 取數端先自證：命中得了，也落空得了 —— 一個恆答空的讀法會讓下面每一條空轉。
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
# 浮點：exposition 裡 counter 渲成 `3` 還是 `3.0` 由繪製器定 ⇒ 用 awk 算差，不做字串比。
DELTA=$(awk -v a="$PURGED_AFTER" -v b="$PURGED_BEFORE" 'BEGIN { printf "%d", a - b }')
if [ "$DELTA" = "$CLEARED" ]; then
  ok "★★★ fulcrum_cache_purged_entries_total 正好涨了 $CLEARED —— 与 purge 回话里那个数逐一对得上"
else
  fail "★★★ 新族涨了 $DELTA，而 purge 说清掉了 $CLEARED 条（before=$PURGED_BEFORE after=$PURGED_AFTER）"
fi

# ★ ★ ★ 反向那半：**它不許再以 `event="purge"` 的形式出現**。
#   ⚠ 少了這一條，一個「兩邊都記一筆」的實作在上面每一條上都是綠的 ——
#     而那正是 G123 要消掉的形狀（兩個分母共用一個族）。
if grep -q 'fulcrum_cache_events_total{event="purge"}' "$WORK/m-after.txt"; then
  fail "★★★ purge 又回到 cache_events_total 里了 —— 它数的是条目，那个族数的是请求（G123）"
else
  ok "★★★ cache_events_total 里没有 event=\"purge\" —— 两个分母不再共用一个族（G123）"
fi
# ★ 而那個族本身**還在**，且真的有樣本 —— 否則上面那條反向斷言在「整個族都沒渲出來」
#   的情況下也會綠，那時它證明的不是 G123，只是指標壞了。
if grep -q '^fulcrum_cache_events_total{event="' "$WORK/m-after.txt"; then
  ok "★ 而 cache_events_total 本身还在且有样本（上一条不是在对着一个空族说话）"
else
  fail "★ cache_events_total 一条样本都没有 —— 上面那条反向断言因此什么都没证明"
fi

# ── [6/7] 防驚群 ───────────────────────────────────────────────────────────
echo
echo "=== [6/7] 防惊群 ==="
# ★ ★ 同時打 20 個**同一條**沒快取過的 URL。它們要麼拿到同一份（leader 存下、
#   follower 重新查到），要麼各自回源 —— 而**正確的實作只回源一次**。
#   ⚠ 這裡不數上游被打了幾次（上游是樞衡、沒有計數端點），
#   ★ 改為數**回來的不同內容有幾種**：只回源一次 ⇒ 全都一樣。
# ⚠ ⚠ ⚠ **每個並行請求必須帶一個不同的 nonce** —— 少了它，20 個體全都是
#   `up-`（空 nonce），於是「只有 1 種內容」這條斷言**恆成立**：
#   ★ 驚群擋沒擋住，它給出的答案一模一樣。**那是一條永遠給綠的判據。**
#   ⇒ 帶上 nonce 之後：擋住了 ⇒ 大家拿到 leader 那一個 nonce（1 種）；
#   沒擋住 ⇒ 各自回源、各拿各的（20 種）。
admin /purge '{"all":true}' > /dev/null
rm -f "$WORK/herd."*
# ⚠ ⚠ ⚠ **只等這 20 個，不能用光禿禿的 `wait`。**
#   `wait` 不帶參數會等**本 shell 的全部後台作業** —— 而 `start()` 把兩個
#   `fulcrum serve` 也放在了後台，它們**永遠不退出**。
#   ★ 實測：整個場景在這一行掛到外層超時，而螢幕上最後一行是 `=== [6/7] 防惊群 ===`，
#   看起來像是產品死鎖了。
#   > **一個會掛住的判據比一條紅的判據更貴**：紅的指著問題，掛住的指著錯誤的方向。
HERD_PIDS=()
for i in $(seq 1 20); do
  curl -s --max-time 20 -H "X-Nonce: herd$i" "$BASE/maxage" -o "$WORK/herd.$i" &
  HERD_PIDS+=($!)
done
for pid in "${HERD_PIDS[@]}"; do wait "$pid" || true; done
# ⚠ ⚠ ⚠ **每份之間要補一個換行**：回應體**沒有行尾**，直接 `cat` 會把 20 份
#   拼成**一行** ⇒ `sort -u | wc -l` 恆等於 1。★ 也就是說，不補這個換行的話，
#   「只有 1 種內容」這條斷言**不管驚群擋沒擋住都成立** —— 又一條永遠給綠的判據，
#   而且它與上面那條（忘了帶 nonce）是**同一批裡的第二個**同形錯誤。
#   > **判據失效時它不沉默，而是給一個像樣的錯答案。**
DISTINCT=$(for f in "$WORK/herd."*; do cat "$f"; echo; done | sort -u | wc -l)
if [ "$DISTINCT" = "1" ]; then
  ok "★ ★ 20 个并发请求只回源了一次（收到的内容只有 1 种）"
else
  fail "★ ★ 惊群没挡住：20 个并发请求收到了 $DISTINCT 种不同的内容"
  for f in "$WORK/herd."*; do cat "$f"; echo; done | sort -u | head -5 >&2
fi
# ★ 反證那一半：**關掉並行**時每次都該拿到自己的 nonce（快取已被上面填上，所以
#   這裡改用一條沒快取過的路徑）。⚠ 少了它，一個「把所有並行請求都回同一份」的
#   實作 —— 比如把 nonce 忽略掉 —— 會讓上面那條全綠。
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

# ── [7/7] 裝載紀錄 ─────────────────────────────────────────────────────────
echo
echo "=== [7/7] 装载日志 ==="
# ★ ★ `ttl` 是**備援**不是覆蓋（G96），而從 nginx 遷過來的人預設以為它是覆蓋。
#   ⇒ 裝載時必須說出來，與 G88 的 hide 清單同一條紀律。
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
# ★ 回落層已經刪掉 ⇒ 裝載紀錄裡**不該**再出現回落那幾個字。
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
