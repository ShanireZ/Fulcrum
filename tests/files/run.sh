#!/usr/bin/env bash
# 自研静态文件服务的端到端（**M2 批 F**，G87–G91）。
#
# ★ ★ 为什么**新开一个场景**而不是塞进 `tests/serve/run.sh`：
#   它要自己的夹具树 —— 已知大小与时间戳的文件、符号链接、点开头的文件、深目录。
#   与 serve 的上游夹具混在一起，两边都会变得难读，而难读的夹具是本仓库
#   反复栽的那一类（「判据在守什么」看不出来，于是改的时候被顺手搬走）。
#
# ★ 与 `crates/fulcrum-server/src/files/*` 里那 49 条单测的分工：
#   那边测**纯函数**（解码、归一、hide、MIME、日期、Range 边界），脱网、可离线；
#   这边测**真 socket 上真的发出去了什么** —— 头、状态码、字节数。
#   ⚠ 两边都要有：只有单测，一个算对了却写错响应头的实现会全绿；
#   只有端到端，一条边界错了只能看到一个状态码，查起来要靠猜。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ ⚠ **9400–9401，不是 9200–9201** —— 后者是**压力场景**在用的
#   （`tests/stress/run.sh` 的 `STRESS_PORT` / `STRESS_UP_PORT`）——
#   ★ 我是照着 `AGENTS.md` 那张端口表选的，而**那张表上压根没有压力那一格**。
#   两个场景串行跑，所以这一轮没真撞上；但这正是那张表存在的理由，
#   而一张不全的表比没有表更危险：它让人以为自己查过了。
#   ⇒ 换到 9400–9401，**并把压力那两格补进那张表**。
PORT=${FILES_PORT:-9400}
# ★ 第二个实例：`follow_symlinks false` 那一侧。两种行为**不能挤在一个站点里**
#   —— 那条开关是站点级的，而两侧都有真实用户（G87 的代价那一段）。
STRICT_PORT=${FILES_STRICT_PORT:-9401}

FAILS=0
PIDS=()

fail() { echo "  ✗ $*" >&2; FAILS=$((FAILS + 1)); }
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid waited
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    # SIGINT 而不是 SIGTERM：Pingora 把 SIGTERM 当优雅停机，会等完整排空窗口。
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

# ── [0/6] 基线 ──────────────────────────────────────────────────────────────
#
# ★ 端口没清干净时，后面每一条断言测的都是别人的服务。本仓库栽过一次。
echo "=== [0/6] 基线：端口未被占用 ==="
for p in "$PORT" "$STRICT_PORT"; do
  if port_listening "$p"; then
    echo "FILES TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑。" >&2
    exit 1
  fi
done
ok "$PORT / $STRICT_PORT 都是空的"

[ -x "$BIN" ] || {
  echo "FILES TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 夹具树 ──────────────────────────────────────────────────────────────────
ROOT="$WORK/www"
OUTSIDE="$WORK/outside"
mkdir -p "$ROOT/sub/deep" "$ROOT/.git" "$ROOT/browsable" "$ROOT/noindex" "$OUTSIDE"

printf 'hello-index\n'          > "$ROOT/index.html"
printf 'plain-text-file\n'      > "$ROOT/a.txt"
printf 'body{color:red}\n'      > "$ROOT/style.css"
printf 'binary-ish\n'           > "$ROOT/blob.bin"
printf 'sub-index\n'            > "$ROOT/sub/index.html"
printf 'deep-file\n'            > "$ROOT/sub/deep/d.txt"
printf 'SECRET-CONFIG\n'        > "$ROOT/.git/config"
printf 'SECRET-ENV\n'           > "$ROOT/.env"
# ★ ★ 这两个是 G88「按段比、不是按前缀比」的反例 —— 它们**不该**被挡。
printf 'gitlab-ci-visible\n'    > "$ROOT/.gitlab-ci.yml"
printf 'environment-visible\n'  > "$ROOT/.environment"
printf 'outside-secret\n'       > "$OUTSIDE/secret.txt"
: > "$ROOT/empty.txt"
# Range 判据要一个**已知长度**的文件。1000 字节。
# ⚠ ⚠ **不许用 python3**：构建镜像里没有它（`docs-check.py` 因此跑在宿主机上）。
#   一个「在开发机上跑得通、在门里跑不起来」的夹具，正是本仓库点过名的那一类。
: > "$ROOT/range.bin"
for _ in $(seq 1 100); do printf '0123456789' >> "$ROOT/range.bin"; done
# ★ 目录列表的夹具：一个正常名字 + 一个**带尖括号的名字**（XSS 那条判据用）。
printf 'listed\n' > "$ROOT/browsable/plain.txt"
printf 'xss\n'    > "$ROOT/browsable/<script>.txt"
mkdir -p "$ROOT/browsable/adir"
# 符号链接：一条指到 root **之内**（合法的链接农场），一条指到 root **之外**。
ln -s "$ROOT/a.txt"          "$ROOT/link-inside.txt"
ln -s "$OUTSIDE/secret.txt"  "$ROOT/link-outside.txt"

FILE_LEN=$(stat -c '%s' "$ROOT/range.bin")
[ "$FILE_LEN" = "1000" ] || {
  echo "FILES TESTS FAILED: 夹具 range.bin 应当是 1000 字节，实际 $FILE_LEN" >&2
  exit 1
}
ok "夹具树建好了（range.bin = $FILE_LEN 字节）"

# ── 配置 ────────────────────────────────────────────────────────────────────
cat > "$WORK/files.Fulcrumfile" <<'CONF'
:PORT {
    handle /browsable/* {
        file_server browse {
            root WWW_ROOT
        }
    }
    handle /noindex/* {
        file_server {
            root WWW_ROOT
        }
    }
    # ★ 默认那一侧：`follow_symlinks` 不写 ⇒ 缺省 true（跟随）。
    #   `hide` 也不写 ⇒ 默认表生效（.git / .env / …）。
    file_server {
        root WWW_ROOT
    }
}
CONF
sed -i "s/:PORT/:$PORT/; s|WWW_ROOT|$ROOT|g" "$WORK/files.Fulcrumfile"

cat > "$WORK/strict.Fulcrumfile" <<'CONF'
:STRICT_PORT {
    file_server {
        root WWW_ROOT
        follow_symlinks false
        hide extra-secret
        hide another
    }
}
CONF
sed -i "s/:STRICT_PORT/:$STRICT_PORT/; s|WWW_ROOT|$ROOT|g" "$WORK/strict.Fulcrumfile"

start() {
  local name=$1 conf=$2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
start files "$WORK/files.Fulcrumfile"
start strict "$WORK/strict.Fulcrumfile"

wait_port "$PORT"        || { echo "FILES TESTS FAILED: $PORT 没起来" >&2; cat "$WORK/files.log" >&2; exit 1; }
wait_port "$STRICT_PORT" || { echo "FILES TESTS FAILED: $STRICT_PORT 没起来" >&2; cat "$WORK/strict.log" >&2; exit 1; }

BASE="http://$HOST:$PORT"
STRICT="http://$HOST:$STRICT_PORT"

# ── 断言 helper ─────────────────────────────────────────────────────────────
#
# ⚠ ⚠ `--http0.9` 不加：本场景全部走正常 HTTP/1.1。
#   `-s -o body -D hdr -w code` 三样一次拿全，后面的 expect_* 都读这两个文件。
probe() {
  # ⚠ ⚠ **先把 body 清空**。curl 的 `-o` 在**一个字节都没收到**时不会去动那个文件
  #   ⇒ 上一次请求的响应体会原样留着。于是 `expect_bytes … 0`（304 / HEAD 那几条）
  #   读到的是**上一次的残留**，判据在最该报警的那一刻反而给出一个像样的错答案。
  #   ★ 实测栽过：304 那条报「实际 16 字节」，而 16 正是上一次那个文件的长度。
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 5 "$@"
}

# HEAD 专用：`-I` 会把**响应头**写进 `-o` 那个文件，所以 body 的字节数不能用它来量。
# ★ 用 curl 自己的 `%{size_download}` —— 它数的是**响应体**，与 `-I` 怎么落盘无关。
probe_head_body_bytes() {
  curl -s -I -o /dev/null -D "$WORK/hdr" -w '%{size_download}' --max-time 5 "$@"
}

expect_status() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望 $want，实际 $got"; fi
}

expect_body() {
  local what=$1 want=$2 got
  got=$(cat "$WORK/body")
  if [ "$got" = "$want" ]; then ok "$what 体 = $got"; else fail "$what 体期望「$want」，实际「$got」"; fi
}

expect_body_has() {
  local what=$1 want=$2
  if grep -qF "$want" "$WORK/body"; then ok "$what 体里有「$want」"; else
    fail "$what 体里没有「$want」，实际：$(head -c 300 "$WORK/body")"
  fi
}

expect_body_lacks() {
  local what=$1 bad=$2
  if grep -qF "$bad" "$WORK/body"; then
    fail "$what 体里**不该**有「$bad」，实际：$(head -c 300 "$WORK/body")"
  else ok "$what 体里没有「$bad」"; fi
}

# 头名大小写不敏感；只取第一次出现，去掉行尾 CR。
# ⚠ ⚠ **`grep` 那一节必须自己吞掉「没匹配」**：本文件是 `set -euo pipefail`，
#   而 `grep` 找不到时返回 1 ⇒ pipefail 把整条管道判成失败 ⇒ `set -e` 当场杀掉脚本。
#   ★ 实测栽过：`expect_header_absent`（它的正常用法就是「查一个不该在的头」）
#   一执行就把场景打断，而屏幕上是 **exit 1 却零条 ✗** —— 看起来像通过了。
#   ⇒ 一个「只有在断言应当通过时才会崩」的 helper，比没有这条断言更糟。
hdr() { { grep -i "^$1:" "$WORK/hdr" || true; } | head -1 | cut -d' ' -f2- | tr -d '\r'; }

expect_header() {
  local what=$1 name=$2 want=$3 got
  got=$(hdr "$name")
  if [ "$got" = "$want" ]; then ok "$what 的 $name = $got"; else
    fail "$what 的 $name 期望「$want」，实际「$got」"; fi
}

expect_header_absent() {
  local what=$1 name=$2 got
  got=$(hdr "$name")
  if [ -z "$got" ]; then ok "$what 没有 $name 头"; else
    fail "$what 不该有 $name 头，实际「$got」"; fi
}

expect_bytes() {
  local what=$1 want=$2 got
  got=$(stat -c '%s' "$WORK/body")
  if [ "$got" = "$want" ]; then ok "$what 收到 $got 字节"; else
    fail "$what 期望 $want 字节，实际 $got"; fi
}

# ── [1/6] 发文件 · 索引 · MIME ───────────────────────────────────────────────
echo
echo "=== [1/6] 发文件 · 索引 · MIME ==="
expect_status "GET /a.txt" 200 "$(probe "$BASE/a.txt")"
expect_body   "GET /a.txt" "plain-text-file"
expect_header "GET /a.txt" "Content-Type" "text/plain; charset=utf-8"
expect_header "GET /a.txt" "Accept-Ranges" "bytes"
# ★ G90 那张小表真的被查了：css 与无扩展名各一条。
expect_status "GET /style.css" 200 "$(probe "$BASE/style.css")"
expect_header "GET /style.css" "Content-Type" "text/css; charset=utf-8"
expect_status "GET /blob.bin" 200 "$(probe "$BASE/blob.bin")"
expect_header "GET /blob.bin" "Content-Type" "application/octet-stream"
# 根目录 → index.html（缺省索引名）。
expect_status "GET /（缺省 index.html）" 200 "$(probe "$BASE/")"
expect_body   "GET /（缺省 index.html）" "hello-index"
expect_status "GET /sub/（子目录索引）" 200 "$(probe "$BASE/sub/")"
expect_body   "GET /sub/（子目录索引）" "sub-index"
expect_status "GET /sub/deep/d.txt（深目录）" 200 "$(probe "$BASE/sub/deep/d.txt")"
expect_body   "GET /sub/deep/d.txt（深目录）" "deep-file"
expect_status "GET /nope.txt（不存在）" 404 "$(probe "$BASE/nope.txt")"
# 空文件：Content-Length 必须是 0，而不是「没有这个头」。
expect_status "GET /empty.txt（空文件）" 200 "$(probe "$BASE/empty.txt")"
expect_header "GET /empty.txt（空文件）" "Content-Length" "0"

# ── [2/6] 方法 · 尾斜杠 301 · browse ────────────────────────────────────────
echo
echo "=== [2/6] 方法 · 尾斜杠 301 · browse ==="
# ★ HEAD：头要**和 GET 一样**，体一个字节都不发。
#   ⚠ `Content-Length` 比的是**夹具的真实字节数**，不是一个手写的数字 ——
#   手写的话，夹具改一个字这条判据就变成在守一个过时的数。
A_TXT_LEN=$(stat -c '%s' "$ROOT/a.txt")
probe "$BASE/a.txt" > /dev/null
expect_header "GET /a.txt" "Content-Length" "$A_TXT_LEN"
CODE=$(probe -I "$BASE/a.txt")
expect_status "HEAD /a.txt" 200 "$CODE"
expect_header "HEAD /a.txt（头与 GET 一致）" "Content-Length" "$A_TXT_LEN"
HEAD_BYTES=$(probe_head_body_bytes "$BASE/a.txt")
if [ "$HEAD_BYTES" = "0" ]; then ok "HEAD /a.txt 响应体 0 字节"; else
  fail "HEAD /a.txt 响应体期望 0 字节，实际 $HEAD_BYTES"; fi
# ★ ★ 405 必须带 `Allow`（RFC 9110 要求）。⚠ 少了这个头，405 是不合规的。
CODE=$(probe -X POST "$BASE/a.txt")
expect_status "POST /a.txt" 405 "$CODE"
expect_header "POST /a.txt" "Allow" "GET, HEAD"
CODE=$(probe -X DELETE "$BASE/a.txt")
expect_status "DELETE /a.txt" 405 "$CODE"
# ★ ★ 目录不带尾斜杠 → 301，**查询串要保留**。
#   ⚠ 少了查询串那一半，一次 `?page=2` 的翻页会在跳转后丢参数 ——
#   而现场只看得到「点了下一页却回到第一页」。
CODE=$(probe "$BASE/sub")
expect_status "GET /sub（缺尾斜杠）" 301 "$CODE"
expect_header "GET /sub（缺尾斜杠）" "Location" "/sub/"
CODE=$(probe "$BASE/sub?page=2&x=1")
expect_status "GET /sub?page=2&x=1" 301 "$CODE"
expect_header "GET /sub?page=2&x=1（查询串要保留）" "Location" "/sub/?page=2&x=1"
# browse 开着 → 目录列表；没开 → 404。
CODE=$(probe "$BASE/browsable/")
expect_status "GET /browsable/（browse 开着）" 200 "$CODE"
expect_header "GET /browsable/" "Content-Type" "text/html; charset=utf-8"
expect_body_has "GET /browsable/" "plain.txt"
expect_body_has "GET /browsable/（目录带尾斜杠）" "adir/"
# ★ ★ ★ 目录列表把文件名放进 HTML —— 一个叫 `<script>` 的文件就是一次存储型 XSS。
expect_body_lacks "GET /browsable/（XSS）" "<script>"
expect_body_has   "GET /browsable/（XSS 已转义）" "&lt;script&gt;"
expect_status "GET /noindex/（没索引也没开 browse）" 404 "$(probe "$BASE/noindex/")"

# ── [3/6] hide 清单（G88）───────────────────────────────────────────────────
echo
echo "=== [3/6] hide 清单 ==="
# ★ ★ 回 **404 不是 403**：403 等于确认「这个文件在」。
CODE=$(probe "$BASE/.git/config")
expect_status "GET /.git/config（默认表挡住）" 404 "$CODE"
expect_body_lacks "GET /.git/config" "SECRET-CONFIG"
expect_status "GET /.env（默认表挡住）" 404 "$(probe "$BASE/.env")"
expect_body_lacks "GET /.env" "SECRET-ENV"
# ⚠ 编码过的也要挡住 —— 解码在归一之前，所以这是免费的；钉一条是因为
#   「先查 hide 再解码」是另一种很自然、而且是**错**的写法。
expect_status "GET /%2egit/config（编码过）" 404 "$(probe --path-as-is "$BASE/%2egit/config")"
# ★ ★ ★ 按**段**比不是按**前缀**比：这两个文件**不该**被挡。
#   ⚠ 按前缀写完全跑得通，只是会把两个正当文件挡成 404 —— 没有任何东西会红。
expect_status "GET /.gitlab-ci.yml（不该被 .git 命中）" 200 "$(probe "$BASE/.gitlab-ci.yml")"
expect_body   "GET /.gitlab-ci.yml" "gitlab-ci-visible"
expect_status "GET /.environment（不该被 .env 命中）" 200 "$(probe "$BASE/.environment")"
expect_body   "GET /.environment" "environment-visible"
# ★ 用户写的 `hide` 是**追加**：strict 实例上默认表**仍然**生效。
expect_status "STRICT GET /.git/config（追加没把默认表挤掉）" 404 "$(probe "$STRICT/.git/config")"
# ★ ★ 装载时把生效的清单打出来 —— 一个不说出来的非空默认就是一次静默行为。
# ⚠ ⚠ **判据不能只 grep `hide` 这个词**：配置文件里、别的日志行里都可能有它。
#   批 E 刚栽过一次同形的（`grep "观察点："` 被开头那行 note 满足，删掉结语照样绿）。
#   ⇒ 钉的是「hide 那一行上**真的列着默认表里的条目**」。
if grep 'hide' "$WORK/files.log" | grep -q '\.git' \
  && grep 'hide' "$WORK/files.log" | grep -q '\.env'; then
  ok "★ 装载日志逐字列出了生效的 hide 清单（含默认表）"
else
  fail "装载日志里没有把 hide 清单列出来（G88 的可见性那一格）"
  grep -i 'hide\|静态文件' "$WORK/files.log" >&2 || head -30 "$WORK/files.log" >&2
fi
# ★ strict 实例：用户追加的两段也要在日志里看得见。
if grep 'hide' "$WORK/strict.log" | grep -q 'extra-secret' \
  && grep 'hide' "$WORK/strict.log" | grep -q 'another'; then
  ok "★ 装载日志也列出了用户追加的那两段"
else
  fail "strict 实例的装载日志里没有用户追加的 hide 段"
  grep -i 'hide\|静态文件' "$WORK/strict.log" >&2 || true
fi

# ── [4/6] 路径穿越 · 符号链接（G87）────────────────────────────────────────
echo
echo "=== [4/6] 路径穿越 · 符号链接 ==="
# ⚠ ⚠ `--path-as-is` 是**必须的**：不加的话 curl 自己就把 `..` 归一掉了，
#   于是打到服务器的根本不是穿越请求 —— 这条判据会变成恒真。
expect_status "GET /../outside/secret.txt（穿越）" 400 "$(probe --path-as-is "$BASE/../outside/secret.txt")"
expect_body_lacks "GET /..（穿越）" "outside-secret"
expect_status "GET /%2e%2e/outside/secret.txt（编码穿越）" 400 "$(probe --path-as-is "$BASE/%2e%2e/outside/secret.txt")"
expect_status "GET /sub/%2e%2e%2f%2e%2e/outside（多级编码穿越）" 400 \
  "$(probe --path-as-is "$BASE/sub/%2e%2e%2f%2e%2e/outside/secret.txt")"
# 归一之后仍在 root 内的 `..` 是**正当**的，必须放行。
# ★ 少了这条反向判据，一个「见到 .. 就拒」的实现会让上面三条全绿。
expect_status "GET /sub/../a.txt（归一后仍在 root 内）" 200 "$(probe --path-as-is "$BASE/sub/../a.txt")"
expect_body   "GET /sub/../a.txt" "plain-text-file"
# ── 符号链接：默认跟随 ──
expect_status "GET /link-inside.txt（默认跟随）" 200 "$(probe "$BASE/link-inside.txt")"
expect_body   "GET /link-inside.txt" "plain-text-file"
# ⚠ 默认那一侧，指向外面的链接**就是会把外面的文件发出去** ——
#   G87 把这个代价写在明处，这条判据钉住它，免得哪天有人以为默认是安全的。
expect_status "GET /link-outside.txt（默认跟随 ⇒ 发得出去）" 200 "$(probe "$BASE/link-outside.txt")"
expect_body   "GET /link-outside.txt（代价已认下）" "outside-secret"
# ── follow_symlinks false 那一侧 ──
expect_status "STRICT GET /link-inside.txt（链接农场仍可用）" 200 "$(probe "$STRICT/link-inside.txt")"
expect_body   "STRICT GET /link-inside.txt" "plain-text-file"
# ★ ★ 这里回 **403 不是 404**，与 hide 有意不同：那是部署方自己放的一条链接，
#   是一条要让运维看见的配置事实。
CODE=$(probe "$STRICT/link-outside.txt")
expect_status "STRICT GET /link-outside.txt（指到 root 之外）" 403 "$CODE"
expect_body_lacks "STRICT GET /link-outside.txt" "outside-secret"

# ── [5/6] 条件请求：ETag / Last-Modified / 304 ─────────────────────────────
echo
echo "=== [5/6] 条件请求 ==="
probe "$BASE/a.txt" >/dev/null
ETAG=$(hdr ETag)
LASTMOD=$(hdr Last-Modified)
if [ -n "$ETAG" ]; then ok "GET /a.txt 带 ETag：$ETAG"; else fail "GET /a.txt 没有 ETag"; fi
if [ -n "$LASTMOD" ]; then ok "GET /a.txt 带 Last-Modified：$LASTMOD"; else fail "没有 Last-Modified"; fi
# ★ Last-Modified 必须是 IMF-fixdate：`Sun, 06 Nov 1994 08:49:37 GMT`。
#   ⚠ 发别的格式是不合规的（RFC 9110 对**发**那一侧只允许这一种）。
if printf '%s' "$LASTMOD" | grep -qE '^[A-Z][a-z]{2}, [0-9]{2} [A-Z][a-z]{2} [0-9]{4} [0-9]{2}:[0-9]{2}:[0-9]{2} GMT$'; then
  ok "★ Last-Modified 是 IMF-fixdate 格式"
else
  fail "Last-Modified 不是 IMF-fixdate：「$LASTMOD」"
fi
CODE=$(probe -H "If-None-Match: $ETAG" "$BASE/a.txt")
expect_status "If-None-Match 命中" 304 "$CODE"
expect_bytes  "If-None-Match 命中（304 不带体）" 0
CODE=$(probe -H "If-None-Match: \"nope\"" "$BASE/a.txt")
expect_status "If-None-Match 不匹配" 200 "$CODE"
CODE=$(probe -H "If-None-Match: *" "$BASE/a.txt")
expect_status "If-None-Match: *" 304 "$CODE"
# ★ 弱比较：`W/"x"` 与 `"x"` 在 If-None-Match 上算同一个。
CODE=$(probe -H "If-None-Match: W/$ETAG" "$BASE/a.txt")
expect_status "If-None-Match 弱前缀（弱比较）" 304 "$CODE"
CODE=$(probe -H "If-Modified-Since: $LASTMOD" "$BASE/a.txt")
expect_status "If-Modified-Since 命中" 304 "$CODE"
CODE=$(probe -H "If-Modified-Since: Sun, 06 Nov 1994 08:49:37 GMT" "$BASE/a.txt")
expect_status "If-Modified-Since 很旧" 200 "$CODE"
# ★ ★ ★ G93 那半句：RFC 850 与 asctime **也**要认。
#   ⚠ 这两条是最容易被"只做一种"悄悄跳过的 —— 而跳过之后老客户端只是
#   多下载一次，不报错、不留痕。
#   ⚠ 两位年那条规则由单测钉着（离线、确定的"现在"），这里只验**认得**。
CODE=$(probe -H "If-Modified-Since: Sunday, 06-Nov-94 08:49:37 GMT" "$BASE/a.txt")
expect_status "If-Modified-Since RFC 850 格式（认得 ⇒ 判成旧 ⇒ 200）" 200 "$CODE"
CODE=$(probe -H "If-Modified-Since: Sun Nov  6 08:49:37 1994" "$BASE/a.txt")
expect_status "If-Modified-Since asctime 格式（认得 ⇒ 判成旧 ⇒ 200）" 200 "$CODE"
# ★ ★ 反向自证：上面两条只验到「没崩」。真正区分「认得」与「没认出来当成没有」的，
#   是拿一个**很新**的日期去问 —— 认得就 304，没认出来就 200。
FUTURE_850=$(date -u -d '+1 day' '+%A, %d-%b-%y %H:%M:%S GMT')
CODE=$(probe -H "If-Modified-Since: $FUTURE_850" "$BASE/a.txt")
expect_status "★ RFC 850 的未来日期 → 304（证明真的解析了，不是当成没有）" 304 "$CODE"
FUTURE_ASC=$(date -u -d '+1 day' '+%a %b %e %H:%M:%S %Y')
CODE=$(probe -H "If-Modified-Since: $FUTURE_ASC" "$BASE/a.txt")
expect_status "★ asctime 的未来日期 → 304（同上）" 304 "$CODE"
# 坏日期 ⇒ 忽略这个头、回 200（不是 400）。
CODE=$(probe -H "If-Modified-Since: not a date at all" "$BASE/a.txt")
expect_status "If-Modified-Since 写坏了 ⇒ 忽略" 200 "$CODE"
# ★ If-None-Match 在场时 If-Modified-Since 一个字都不看（RFC 9110 §13.2.2）。
CODE=$(probe -H "If-None-Match: \"nope\"" -H "If-Modified-Since: $LASTMOD" "$BASE/a.txt")
expect_status "★ If-None-Match 优先于 If-Modified-Since" 200 "$CODE"

# ── [6/6] Range（G89：只做单段）─────────────────────────────────────────────
echo
echo "=== [6/6] Range ==="
CODE=$(probe -H "Range: bytes=0-499" "$BASE/range.bin")
expect_status "Range: bytes=0-499" 206 "$CODE"
expect_header "Range: bytes=0-499" "Content-Range" "bytes 0-499/1000"
expect_header "Range: bytes=0-499" "Content-Length" "500"
expect_bytes  "Range: bytes=0-499" 500
CODE=$(probe -H "Range: bytes=500-" "$BASE/range.bin")
expect_status "Range: bytes=500-" 206 "$CODE"
expect_bytes  "Range: bytes=500-" 500
CODE=$(probe -H "Range: bytes=-100" "$BASE/range.bin")
expect_status "Range: bytes=-100（末尾 100 字节）" 206 "$CODE"
expect_header "Range: bytes=-100" "Content-Range" "bytes 900-999/1000"
expect_bytes  "Range: bytes=-100" 100
# ★ 闭区间：`0-0` 是**一个**字节。差一错就在这里。
CODE=$(probe -H "Range: bytes=0-0" "$BASE/range.bin")
expect_status "Range: bytes=0-0（闭区间，1 字节）" 206 "$CODE"
expect_bytes  "Range: bytes=0-0" 1
# ★ ★ 不可满足 → 416 且**必须带** `Content-Range: bytes */len`。
CODE=$(probe -H "Range: bytes=5000-6000" "$BASE/range.bin")
expect_status "Range: bytes=5000-6000（越界）" 416 "$CODE"
expect_header "Range 越界" "Content-Range" "bytes */1000"
# ★ 末端超过文件尾要**截住**，不是拒。
CODE=$(probe -H "Range: bytes=990-99999" "$BASE/range.bin")
expect_status "Range: bytes=990-99999（末端截住）" 206 "$CODE"
expect_bytes  "Range: bytes=990-99999" 10
# ★ ★ 多段**不做**：回 200 全量（G89），不是 206、更不是 416。
CODE=$(probe -H "Range: bytes=0-99,200-299" "$BASE/range.bin")
expect_status "Range 多段 ⇒ 回 200 全量" 200 "$CODE"
expect_bytes  "Range 多段 ⇒ 回 200 全量" 1000
expect_header_absent "Range 多段" "Content-Range"
# 不认识的单位 ⇒ 忽略、200 全量。
CODE=$(probe -H "Range: items=0-10" "$BASE/range.bin")
expect_status "Range 单位不是 bytes ⇒ 忽略" 200 "$CODE"
# ── If-Range ──
probe "$BASE/range.bin" >/dev/null
RETAG=$(hdr ETag)
CODE=$(probe -H "If-Range: $RETAG" -H "Range: bytes=0-9" "$BASE/range.bin")
expect_status "If-Range 匹配 ⇒ 206" 206 "$CODE"
expect_bytes  "If-Range 匹配" 10
# ★ ★ 不匹配 ⇒ **忽略 Range**、回 200 全量（不是 412、不是 416）。
CODE=$(probe -H "If-Range: \"stale\"" -H "Range: bytes=0-9" "$BASE/range.bin")
expect_status "If-Range 不匹配 ⇒ 忽略 Range、200 全量" 200 "$CODE"
expect_bytes  "If-Range 不匹配" 1000
# ★ ★ ★ If-Range 用**强**比较：弱 ETag 一律不匹配。
#   ⚠ 放行的话客户端会把两个版本的字节拼在一起 —— 拼出来的文件既不报错、
#   也不是任何一个版本。
CODE=$(probe -H "If-Range: W/$RETAG" -H "Range: bytes=0-9" "$BASE/range.bin")
expect_status "★ If-Range 弱 ETag ⇒ 强比较不匹配 ⇒ 200 全量" 200 "$CODE"
expect_bytes  "If-Range 弱 ETag" 1000

echo
if [ "$FAILS" -ne 0 ]; then
  echo "FILES TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 默认实例日志 ──" >&2
  cat "$WORK/files.log" >&2
  echo "── strict 实例日志 ──" >&2
  cat "$WORK/strict.log" >&2
  exit 1
fi
echo "FILES TESTS PASSED —— 自研静态文件：发文件 / MIME / 索引 / 尾斜杠 301 / browse+转义 / hide 按段 / 穿越四向 / 符号链接两侧 / 条件请求三种日期 / Range 单段与边界。"
