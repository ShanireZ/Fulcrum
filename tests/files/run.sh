#!/usr/bin/env bash
# 自研靜態檔案服務的端到端（**M2 批 F**，G87–G91）。
#
# ★ ★ 為什麼**新開一個場景**而不是塞進 `tests/serve/run.sh`：
#   它要自己的夾具樹 —— 已知大小與時間戳的檔案、符號連結、點開頭的檔案、深目錄。
#   與 serve 的上游夾具混在一起，兩邊都會變得難讀，而難讀的夾具是本儲存庫
#   反覆栽的那一類（「判據在守什麼」看不出來，於是改的時候被順手搬走）。
#
# ★ 與 `crates/fulcrum-server/src/files/*` 裡那 49 條單測的分工：
#   那邊測**純函式**（解碼、歸一、hide、MIME、日期、Range 邊界），脫網、可離線；
#   這邊測**真 socket 上真的發出去了什麼** —— 頭、狀態碼、位元組數。
#   ⚠ 兩邊都要有：只有單測，一個算對了卻寫錯回應頭的實作會全綠；
#   只有端到端，一條邊界錯了只能看到一個狀態碼，查起來要靠猜。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ ⚠ **9400–9401，不是 9200–9201** —— 後者是**壓力場景**在用的
#   （`tests/stress/run.sh` 的 `STRESS_PORT` / `STRESS_UP_PORT`）——
#   ★ 我是照著 `AGENTS.md` 那張連接埠表選的，而**那張表上壓根沒有壓力那一格**。
#   兩個場景串行跑，所以這一輪沒真撞上；但這正是那張表存在的理由，
#   而一張不全的表比沒有表更危險：它讓人以為自己查過了。
#   ⇒ 換到 9400–9401，**並把壓力那兩格補進那張表**。
PORT=${FILES_PORT:-9400}
# ★ 第二個執行個體：`follow_symlinks false` 那一側。兩種行為**不能擠在一個站台裡**
#   —— 那條開關是站台級的，而兩側都有真實使用者（G87 的代價那一段）。
STRICT_PORT=${FILES_STRICT_PORT:-9401}

FAILS=0
PIDS=()

fail() { echo "  ✗ $*" >&2; FAILS=$((FAILS + 1)); }
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid waited
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    # SIGINT 而不是 SIGTERM：Pingora 把 SIGTERM 當優雅停機，會等完整排空視窗。
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

# ── [0/6] 基線 ──────────────────────────────────────────────────────────────
#
# ★ 連接埠沒清乾淨時，後面每一條斷言測的都是別人的服務。本儲存庫栽過一次。
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

# ── 夾具樹 ──────────────────────────────────────────────────────────────────
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
# ★ ★ 這兩個是 G88「按段比、不是按前綴比」的反例 —— 它們**不該**被擋。
printf 'gitlab-ci-visible\n'    > "$ROOT/.gitlab-ci.yml"
printf 'environment-visible\n'  > "$ROOT/.environment"
printf 'outside-secret\n'       > "$OUTSIDE/secret.txt"
: > "$ROOT/empty.txt"
# Range 判據要一個**已知長度**的檔案。1000 位元組。
# ⚠ ⚠ **不許用 python3**：建置鏡像裡沒有它（`docs-check.py` 因此跑在宿主機上）。
#   一個「在開發機上跑得通、在門裡跑不起來」的夾具，正是本儲存庫點過名的那一類。
: > "$ROOT/range.bin"
for _ in $(seq 1 100); do printf '0123456789' >> "$ROOT/range.bin"; done
# ★ ★ ★ **跨過 `CHUNK`（64 KiB）那條界的三份夾具。**
#
# 2026-09-06 起 `file_server` 分兩條傳送路徑：長度 ≤ CHUNK 的在 `serve()` 裡就被
# 一次 `spawn_blocking` 整讀進記憶體、由 `send_bytes` 發；更大的走原來的分塊循環
# `stream_body`（`open` + 逐塊 `read`，各自一次 `spawn_blocking`）。
#
# ⚠ ⚠ ★ **在這三份夾具之前，本儲存庫沒有任何一個測試用過大於 64 KiB 的檔案。**
#   改之前分塊路徑是**唯一**路徑（每個檔案測試都在走它），改之後它只剩
#   大檔案 / 預壓縮旁檔案 / 目錄索引能走到 ⇒ `stream_body` 一旦被弄壞，
#   **不會有任何東西變紅**。這三份就是補上的那道門。
# ⛔ **不許把 `chunk-plus.bin` 改成「明顯更大」的尺寸**：它只比界大 **1 個位元組**，
#   那正是它的判別力所在 —— 界寫成 `>=` 還是 `>` 只有它分得開。
#
# ⚠ ⛔ **不用 `yes … | head -c N`**：本檔案是 `set -o pipefail`，`head` 先退會給
#   `yes` 一個 SIGPIPE ⇒ 管道狀態 141 ⇒ 整個指令碼被 `set -e` 殺掉。
#   ⇒ 先造一份填充料，再用**重定向**（⛔ 不是管道）截出三個尺寸。
FILL_LINE=""
for _ in $(seq 1 100); do FILL_LINE="${FILL_LINE}0123456789"; done
: > "$WORK/filler"
for _ in $(seq 1 300); do printf '%s' "$FILL_LINE" >> "$WORK/filler"; done
head -c 65536 < "$WORK/filler" > "$ROOT/chunk-exact.bin"
head -c 65537 < "$WORK/filler" > "$ROOT/chunk-plus.bin"
head -c 200000 < "$WORK/filler" > "$ROOT/big.bin"
# ★ 目錄列表的夾具：一個正常名字 + 一個**帶尖括號的名字**（XSS 那條判據用）。
printf 'listed\n' > "$ROOT/browsable/plain.txt"
printf 'xss\n'    > "$ROOT/browsable/<script>.txt"
mkdir -p "$ROOT/browsable/adir"
# 符號連結：一條指到 root **之內**（合法的連結農場），一條指到 root **之外**。
ln -s "$ROOT/a.txt"          "$ROOT/link-inside.txt"
ln -s "$OUTSIDE/secret.txt"  "$ROOT/link-outside.txt"

FILE_LEN=$(stat -c '%s' "$ROOT/range.bin")
[ "$FILE_LEN" = "1000" ] || {
  echo "FILES TESTS FAILED: 夹具 range.bin 应当是 1000 字节，实际 $FILE_LEN" >&2
  exit 1
}
ok "夹具树建好了（range.bin = $FILE_LEN 字节）"

# ── 設定 ────────────────────────────────────────────────────────────────────
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

# ── 斷言 helper ─────────────────────────────────────────────────────────────
#
# ⚠ ⚠ `--http0.9` 不加：本場景全部走正常 HTTP/1.1。
#   `-s -o body -D hdr -w code` 三樣一次拿全，後面的 expect_* 都讀這兩個檔案。
probe() {
  # ⚠ ⚠ **先把 body 清空**。curl 的 `-o` 在**一個位元組都沒收到**時不會去動那個檔案
  #   ⇒ 上一次請求的回應體會原樣留著。於是 `expect_bytes … 0`（304 / HEAD 那幾條）
  #   讀到的是**上一次的殘留**，判據在最該報警的那一刻反而給出一個像樣的錯答案。
  #   ★ 實測栽過：304 那條報「實際 16 位元組」，而 16 正是上一次那個檔案的長度。
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 5 "$@"
}

# HEAD 專用：`-I` 會把**回應頭**寫進 `-o` 那個檔案，所以 body 的位元組數不能用它來量。
# ★ 用 curl 自己的 `%{size_download}` —— 它數的是**回應體**，與 `-I` 怎麼寫入磁碟無關。
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

# 頭名大小寫不敏感；只取第一次出現，去掉行尾 CR。
# ⚠ ⚠ **`grep` 那一節必須自己吞掉「沒匹配」**：本檔案是 `set -euo pipefail`，
#   而 `grep` 找不到時回傳 1 ⇒ pipefail 把整條管道判成失敗 ⇒ `set -e` 當場殺掉指令碼。
#   ★ 實測栽過：`expect_header_absent`（它的正常用法就是「查一個不該在的頭」）
#   一執行就把場景打斷，而螢幕上是 **exit 1 卻零條 ✗** —— 看起來像通過了。
#   ⇒ 一個「只有在斷言應當通過時才會崩」的 helper，比沒有這條斷言更糟。
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

# ★ ★ 頭名的**大小寫與順序**逐行釘死（2026-10-05，構回應頭不再走 String 之後加的）。
#   ⚠ 上面的 `hdr` 按大小寫不敏感取值 ⇒ 判不出「`ETag` 以 `etag` 上線」這一族迴歸。
#   ★ G162 起 `file_server` 的回應頭**不建大小寫表**（`build_no_case`），頭名全靠 pingora 的標題化表寫出：
#   `ETag` / `Last-Modified` / `Content-Range` / `Location` / `Allow` 是 fork 改動 17 補進去的 ——
#   rebase 沖掉那幾行、或有人往 `files/mod.rs` 加了一個表外的大寫頭名，它們就以小寫上線。
#   ⇒ 那一族**只有這幾格**判得到（200 / 206 / 416 / 301 / 405 各釘一條）。
#   只比名字：值由別的斷言判，`Date` 每秒都變。
expect_header_names() {
  local what=$1 want=$2 got
  got=$(tr -d '\r' < "$WORK/hdr" | sed -n '2,$p' | sed '/^$/d' | cut -d: -f1 | paste -sd' ' -)
  if [ "$got" = "$want" ]; then ok "$what 的头名逐个原样：$got"; else
    fail "$what 的头名（大小写 + 顺序）期望「$want」，实际「$got」"; fi
}

expect_bytes() {
  local what=$1 want=$2 got
  got=$(stat -c '%s' "$WORK/body")
  if [ "$got" = "$want" ]; then ok "$what 收到 $got 字节"; else
    fail "$what 期望 $want 字节，实际 $got"; fi
}

# ── [1/6] 發檔案 · 索引 · MIME ───────────────────────────────────────────────
echo
echo "=== [1/6] 发文件 · 索引 · MIME ==="
expect_status "GET /a.txt" 200 "$(probe "$BASE/a.txt")"
expect_body   "GET /a.txt" "plain-text-file"
expect_header "GET /a.txt" "Content-Type" "text/plain; charset=utf-8"
expect_header "GET /a.txt" "Accept-Ranges" "bytes"
expect_header_names "GET /a.txt" "Content-Type Content-Length Accept-Ranges ETag Last-Modified Date Connection"
# ★ G90 那張小表真的被查了：css 與無副檔名各一條。
expect_status "GET /style.css" 200 "$(probe "$BASE/style.css")"
expect_header "GET /style.css" "Content-Type" "text/css; charset=utf-8"
expect_status "GET /blob.bin" 200 "$(probe "$BASE/blob.bin")"
expect_header "GET /blob.bin" "Content-Type" "application/octet-stream"
# 根目錄 → index.html（預設索引名）。
expect_status "GET /（缺省 index.html）" 200 "$(probe "$BASE/")"
expect_body   "GET /（缺省 index.html）" "hello-index"
expect_status "GET /sub/（子目录索引）" 200 "$(probe "$BASE/sub/")"
expect_body   "GET /sub/（子目录索引）" "sub-index"
expect_status "GET /sub/deep/d.txt（深目录）" 200 "$(probe "$BASE/sub/deep/d.txt")"
expect_body   "GET /sub/deep/d.txt（深目录）" "deep-file"
expect_status "GET /nope.txt（不存在）" 404 "$(probe "$BASE/nope.txt")"
# 空檔案：Content-Length 必須是 0，而不是「沒有這個頭」。
expect_status "GET /empty.txt（空文件）" 200 "$(probe "$BASE/empty.txt")"
expect_header "GET /empty.txt（空文件）" "Content-Length" "0"

# ── ★★★ 跨 `CHUNK` 界的兩條傳送路徑 ────────────────────────────────────────
#
# ⚠ 斷言取**逐位元組相等**（`cmp`），⛔ 不只是長度：一個把偏移算錯的分塊循環
#   長度照樣對，而內容是錯位的。
expect_same_bytes() {
  local what=$1 src=$2
  if cmp -s "$src" "$WORK/body"; then
    ok "$what 体与源文件逐字节相同（$(stat -c '%s' "$src") 字节）"
  else
    fail "$what 体与源文件不同（源 $(stat -c '%s' "$src") 字节，实收 $(stat -c '%s' "$WORK/body") 字节）"
  fi
}

# ① 恰好 CHUNK ⇒ 走**記憶體**路徑（界是 `> CHUNK` 才分塊 ⇒ 等於時不分）。
expect_status "GET /chunk-exact.bin（恰好 64 KiB）" 200 "$(probe "$BASE/chunk-exact.bin")"
expect_header "GET /chunk-exact.bin" "Content-Length" "65536"
expect_same_bytes "GET /chunk-exact.bin" "$ROOT/chunk-exact.bin"
# ② CHUNK+1 ⇒ 走**分塊**路徑。★ ★ 只比 ① 大一個位元組，而**這一對確實釘得開兩條路徑**
#   —— 2026-09-06 反證實測：給 `stream_body` 注入「只發第一塊就停」之後，
#   ① 照常綠、② 與 ③ 當場紅（實收 65536 位元組）。⇒ ① 沒走分塊路徑，②③ 走了。
expect_status "GET /chunk-plus.bin（64 KiB + 1）" 200 "$(probe "$BASE/chunk-plus.bin")"
expect_header "GET /chunk-plus.bin" "Content-Length" "65537"
expect_same_bytes "GET /chunk-plus.bin" "$ROOT/chunk-plus.bin"
# ③ 200000 位元組 ⇒ 分塊循環真的轉 **4 圈**（最後一圈 3392 位元組）。
#   ★ 只有它能抓到「第二塊之後就不對了」這一類錯。
expect_status "GET /big.bin（200000 字节，4 块）" 200 "$(probe "$BASE/big.bin")"
expect_header "GET /big.bin" "Content-Length" "200000"
expect_same_bytes "GET /big.bin" "$ROOT/big.bin"

# ── [2/6] 方法 · 尾斜槓 301 · browse ────────────────────────────────────────
echo
echo "=== [2/6] 方法 · 尾斜杠 301 · browse ==="
# ★ HEAD：頭要**和 GET 一樣**，體一個位元組都不發。
#   ⚠ `Content-Length` 比的是**夾具的真實位元組數**，不是一個手寫的數字 ——
#   手寫的話，夾具改一個字這條判據就變成在守一個過時的數。
A_TXT_LEN=$(stat -c '%s' "$ROOT/a.txt")
probe "$BASE/a.txt" > /dev/null
expect_header "GET /a.txt" "Content-Length" "$A_TXT_LEN"
CODE=$(probe -I "$BASE/a.txt")
expect_status "HEAD /a.txt" 200 "$CODE"
expect_header "HEAD /a.txt（头与 GET 一致）" "Content-Length" "$A_TXT_LEN"
HEAD_BYTES=$(probe_head_body_bytes "$BASE/a.txt")
if [ "$HEAD_BYTES" = "0" ]; then ok "HEAD /a.txt 响应体 0 字节"; else
  fail "HEAD /a.txt 响应体期望 0 字节，实际 $HEAD_BYTES"; fi
# ★ ★ 405 必須帶 `Allow`（RFC 9110 要求）。⚠ 少了這個頭，405 是不合規的。
CODE=$(probe -X POST "$BASE/a.txt")
expect_status "POST /a.txt" 405 "$CODE"
expect_header "POST /a.txt" "Allow" "GET, HEAD"
expect_header_names "POST /a.txt（405）" "Allow Content-Length Date Connection"
CODE=$(probe -X DELETE "$BASE/a.txt")
expect_status "DELETE /a.txt" 405 "$CODE"
# ★ ★ 目錄不帶尾斜槓 → 301，**查詢串要保留**。
#   ⚠ 少了查詢串那一半，一次 `?page=2` 的翻頁會在跳轉後丟參數 ——
#   而現場只看得到「點了下一頁卻回到第一頁」。
CODE=$(probe "$BASE/sub")
expect_status "GET /sub（缺尾斜杠）" 301 "$CODE"
expect_header "GET /sub（缺尾斜杠）" "Location" "/sub/"
expect_header_names "GET /sub（301）" "Location Content-Length Date Connection"
CODE=$(probe "$BASE/sub?page=2&x=1")
expect_status "GET /sub?page=2&x=1" 301 "$CODE"
expect_header "GET /sub?page=2&x=1（查询串要保留）" "Location" "/sub/?page=2&x=1"
# browse 開著 → 目錄列表；沒開 → 404。
CODE=$(probe "$BASE/browsable/")
expect_status "GET /browsable/（browse 开着）" 200 "$CODE"
expect_header "GET /browsable/" "Content-Type" "text/html; charset=utf-8"
expect_body_has "GET /browsable/" "plain.txt"
expect_body_has "GET /browsable/（目录带尾斜杠）" "adir/"
# ★ ★ ★ 目錄列表把檔案名放進 HTML —— 一個叫 `<script>` 的檔案就是一次儲存型 XSS。
expect_body_lacks "GET /browsable/（XSS）" "<script>"
expect_body_has   "GET /browsable/（XSS 已转义）" "&lt;script&gt;"
expect_status "GET /noindex/（没索引也没开 browse）" 404 "$(probe "$BASE/noindex/")"

# ── [3/6] hide 清單（G88）───────────────────────────────────────────────────
echo
echo "=== [3/6] hide 清单 ==="
# ★ ★ 回 **404 不是 403**：403 等於確認「這個檔案在」。
CODE=$(probe "$BASE/.git/config")
expect_status "GET /.git/config（默认表挡住）" 404 "$CODE"
expect_body_lacks "GET /.git/config" "SECRET-CONFIG"
expect_status "GET /.env（默认表挡住）" 404 "$(probe "$BASE/.env")"
expect_body_lacks "GET /.env" "SECRET-ENV"
# ⚠ 編碼過的也要擋住 —— 解碼在歸一之前，所以這是免費的；釘一條是因為
#   「先查 hide 再解碼」是另一種很自然、而且是**錯**的寫法。
expect_status "GET /%2egit/config（编码过）" 404 "$(probe --path-as-is "$BASE/%2egit/config")"
# ★ ★ ★ 按**段**比不是按**前綴**比：這兩個檔案**不該**被擋。
#   ⚠ 按前綴寫完全跑得通，只是會把兩個正當檔案擋成 404 —— 沒有任何東西會紅。
expect_status "GET /.gitlab-ci.yml（不该被 .git 命中）" 200 "$(probe "$BASE/.gitlab-ci.yml")"
expect_body   "GET /.gitlab-ci.yml" "gitlab-ci-visible"
expect_status "GET /.environment（不该被 .env 命中）" 200 "$(probe "$BASE/.environment")"
expect_body   "GET /.environment" "environment-visible"
# ★ 使用者寫的 `hide` 是**追加**：strict 執行個體上預設表**仍然**生效。
expect_status "STRICT GET /.git/config（追加没把默认表挤掉）" 404 "$(probe "$STRICT/.git/config")"
# ★ ★ 裝載時把生效的清單打出來 —— 一個不說出來的非空預設就是一次靜默行為。
# ⚠ ⚠ **判據不能只 grep `hide` 這個詞**：設定檔案裡、別的紀錄行裡都可能有它。
#   批 E 剛栽過一次同形的（`grep "观察点："` 被開頭那行 note 滿足，刪掉結語照樣綠）。
#   ⇒ 釘的是「hide 那一行上**真的列著預設表裡的條目**」。
if grep 'hide' "$WORK/files.log" | grep -q '\.git' \
  && grep 'hide' "$WORK/files.log" | grep -q '\.env'; then
  ok "★ 装载日志逐字列出了生效的 hide 清单（含默认表）"
else
  fail "装载日志里没有把 hide 清单列出来（G88 的可见性那一格）"
  grep -i 'hide\|静态文件' "$WORK/files.log" >&2 || head -30 "$WORK/files.log" >&2
fi
# ★ strict 執行個體：使用者追加的兩段也要在紀錄裡看得見。
if grep 'hide' "$WORK/strict.log" | grep -q 'extra-secret' \
  && grep 'hide' "$WORK/strict.log" | grep -q 'another'; then
  ok "★ 装载日志也列出了用户追加的那两段"
else
  fail "strict 实例的装载日志里没有用户追加的 hide 段"
  grep -i 'hide\|静态文件' "$WORK/strict.log" >&2 || true
fi

# ── [4/6] 路徑穿越 · 符號連結（G87）────────────────────────────────────────
echo
echo "=== [4/6] 路径穿越 · 符号链接 ==="
# ⚠ ⚠ `--path-as-is` 是**必須的**：不加的話 curl 自己就把 `..` 歸一掉了，
#   於是打到伺服器的根本不是穿越請求 —— 這條判據會變成恆真。
expect_status "GET /../outside/secret.txt（穿越）" 400 "$(probe --path-as-is "$BASE/../outside/secret.txt")"
expect_body_lacks "GET /..（穿越）" "outside-secret"
expect_status "GET /%2e%2e/outside/secret.txt（编码穿越）" 400 "$(probe --path-as-is "$BASE/%2e%2e/outside/secret.txt")"
expect_status "GET /sub/%2e%2e%2f%2e%2e/outside（多级编码穿越）" 400 \
  "$(probe --path-as-is "$BASE/sub/%2e%2e%2f%2e%2e/outside/secret.txt")"
# 歸一之後仍在 root 內的 `..` 是**正當**的，必須放行。
# ★ 少了這條反向判據，一個「見到 .. 就拒」的實作會讓上面三條全綠。
expect_status "GET /sub/../a.txt（归一后仍在 root 内）" 200 "$(probe --path-as-is "$BASE/sub/../a.txt")"
expect_body   "GET /sub/../a.txt" "plain-text-file"
# ── 符號連結：預設跟隨 ──
expect_status "GET /link-inside.txt（默认跟随）" 200 "$(probe "$BASE/link-inside.txt")"
expect_body   "GET /link-inside.txt" "plain-text-file"
# ⚠ 預設那一側，指向外面的連結**就是會把外面的檔案發出去** ——
#   G87 把這個代價寫在明處，這條判據釘住它，免得哪天有人以為預設是安全的。
expect_status "GET /link-outside.txt（默认跟随 ⇒ 发得出去）" 200 "$(probe "$BASE/link-outside.txt")"
expect_body   "GET /link-outside.txt（代价已认下）" "outside-secret"
# ── follow_symlinks false 那一側 ──
expect_status "STRICT GET /link-inside.txt（链接农场仍可用）" 200 "$(probe "$STRICT/link-inside.txt")"
expect_body   "STRICT GET /link-inside.txt" "plain-text-file"
# ★ ★ 這裡回 **403 不是 404**，與 hide 有意不同：那是部署方自己放的一條連結，
#   是一條要讓維運看見的設定事實。
CODE=$(probe "$STRICT/link-outside.txt")
expect_status "STRICT GET /link-outside.txt（指到 root 之外）" 403 "$CODE"
expect_body_lacks "STRICT GET /link-outside.txt" "outside-secret"

# ── [5/6] 條件請求：ETag / Last-Modified / 304 ─────────────────────────────
echo
echo "=== [5/6] 条件请求 ==="
probe "$BASE/a.txt" >/dev/null
ETAG=$(hdr ETag)
LASTMOD=$(hdr Last-Modified)
if [ -n "$ETAG" ]; then ok "GET /a.txt 带 ETag：$ETAG"; else fail "GET /a.txt 没有 ETag"; fi
if [ -n "$LASTMOD" ]; then ok "GET /a.txt 带 Last-Modified：$LASTMOD"; else fail "没有 Last-Modified"; fi
# ★ Last-Modified 必須是 IMF-fixdate：`Sun, 06 Nov 1994 08:49:37 GMT`。
#   ⚠ 發別的格式是不合規的（RFC 9110 對**發**那一側只允許這一種）。
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
# ★ 弱比較：`W/"x"` 與 `"x"` 在 If-None-Match 上算同一個。
CODE=$(probe -H "If-None-Match: W/$ETAG" "$BASE/a.txt")
expect_status "If-None-Match 弱前缀（弱比较）" 304 "$CODE"
CODE=$(probe -H "If-Modified-Since: $LASTMOD" "$BASE/a.txt")
expect_status "If-Modified-Since 命中" 304 "$CODE"
CODE=$(probe -H "If-Modified-Since: Sun, 06 Nov 1994 08:49:37 GMT" "$BASE/a.txt")
expect_status "If-Modified-Since 很旧" 200 "$CODE"
# ★ ★ ★ G93 那半句：RFC 850 與 asctime **也**要認。
#   ⚠ 這兩條是最容易被"只做一種"悄悄跳過的 —— 而跳過之後老用戶端只是
#   多下載一次，不回報錯誤、不留痕。
#   ⚠ 兩位年那條規則由單測釘著（離線、確定的"現在"），這裡只驗**認得**。
CODE=$(probe -H "If-Modified-Since: Sunday, 06-Nov-94 08:49:37 GMT" "$BASE/a.txt")
expect_status "If-Modified-Since RFC 850 格式（认得 ⇒ 判成旧 ⇒ 200）" 200 "$CODE"
CODE=$(probe -H "If-Modified-Since: Sun Nov  6 08:49:37 1994" "$BASE/a.txt")
expect_status "If-Modified-Since asctime 格式（认得 ⇒ 判成旧 ⇒ 200）" 200 "$CODE"
# ★ ★ 反向自證：上面兩條只驗到「沒崩」。真正區分「認得」與「沒認出來當成沒有」的，
#   是拿一個**很新**的日期去問 —— 認得就 304，沒認出來就 200。
FUTURE_850=$(date -u -d '+1 day' '+%A, %d-%b-%y %H:%M:%S GMT')
CODE=$(probe -H "If-Modified-Since: $FUTURE_850" "$BASE/a.txt")
expect_status "★ RFC 850 的未来日期 → 304（证明真的解析了，不是当成没有）" 304 "$CODE"
FUTURE_ASC=$(date -u -d '+1 day' '+%a %b %e %H:%M:%S %Y')
CODE=$(probe -H "If-Modified-Since: $FUTURE_ASC" "$BASE/a.txt")
expect_status "★ asctime 的未来日期 → 304（同上）" 304 "$CODE"
# 壞日期 ⇒ 忽略這個頭、回 200（不是 400）。
CODE=$(probe -H "If-Modified-Since: not a date at all" "$BASE/a.txt")
expect_status "If-Modified-Since 写坏了 ⇒ 忽略" 200 "$CODE"
# ★ If-None-Match 在場時 If-Modified-Since 一個字都不看（RFC 9110 §13.2.2）。
CODE=$(probe -H "If-None-Match: \"nope\"" -H "If-Modified-Since: $LASTMOD" "$BASE/a.txt")
expect_status "★ If-None-Match 优先于 If-Modified-Since" 200 "$CODE"

# ── [6/6] Range（G89：只做單段）─────────────────────────────────────────────
echo
echo "=== [6/6] Range ==="
CODE=$(probe -H "Range: bytes=0-499" "$BASE/range.bin")
expect_status "Range: bytes=0-499" 206 "$CODE"
expect_header "Range: bytes=0-499" "Content-Range" "bytes 0-499/1000"
expect_header "Range: bytes=0-499" "Content-Length" "500"
expect_header_names "Range: bytes=0-499（206）" "Content-Type Content-Length Accept-Ranges ETag Last-Modified Content-Range Date Connection"
expect_bytes  "Range: bytes=0-499" 500
CODE=$(probe -H "Range: bytes=500-" "$BASE/range.bin")
expect_status "Range: bytes=500-" 206 "$CODE"
expect_bytes  "Range: bytes=500-" 500
CODE=$(probe -H "Range: bytes=-100" "$BASE/range.bin")
expect_status "Range: bytes=-100（末尾 100 字节）" 206 "$CODE"
expect_header "Range: bytes=-100" "Content-Range" "bytes 900-999/1000"
expect_bytes  "Range: bytes=-100" 100
# ★ 閉區間：`0-0` 是**一個**位元組。差一錯就在這裡。
CODE=$(probe -H "Range: bytes=0-0" "$BASE/range.bin")
expect_status "Range: bytes=0-0（闭区间，1 字节）" 206 "$CODE"
expect_bytes  "Range: bytes=0-0" 1
# ★ ★ 不可滿足 → 416 且**必須帶** `Content-Range: bytes */len`。
CODE=$(probe -H "Range: bytes=5000-6000" "$BASE/range.bin")
expect_status "Range: bytes=5000-6000（越界）" 416 "$CODE"
expect_header "Range 越界" "Content-Range" "bytes */1000"
expect_header_names "Range 越界（416）" "Content-Range Content-Length Accept-Ranges ETag Last-Modified Date Connection"
# ★ 末端超過檔案尾要**截住**，不是拒。
CODE=$(probe -H "Range: bytes=990-99999" "$BASE/range.bin")
expect_status "Range: bytes=990-99999（末端截住）" 206 "$CODE"
expect_bytes  "Range: bytes=990-99999" 10
# ★ ★ 多段**不做**：回 200 全量（G89），不是 206、更不是 416。
CODE=$(probe -H "Range: bytes=0-99,200-299" "$BASE/range.bin")
expect_status "Range 多段 ⇒ 回 200 全量" 200 "$CODE"
expect_bytes  "Range 多段 ⇒ 回 200 全量" 1000
expect_header_absent "Range 多段" "Content-Range"
# 不認識的單位 ⇒ 忽略、200 全量。
CODE=$(probe -H "Range: items=0-10" "$BASE/range.bin")
expect_status "Range 单位不是 bytes ⇒ 忽略" 200 "$CODE"
# ★ ★ ★ **一個跨越多塊的 Range**：1000–100000 共 **99001** 位元組 ⇒ 分塊循環要轉 2 圈。
#
# ⚠ ⚠ ★ **初稿寫的是 `bytes=65530-65545`，說它「壓在第 1 塊與第 2 塊的接縫上」——
#   那句話是錯的。** `stream_body` 先 `seek(start)`，分塊是**從 range 的起點重新數的**
#   ⇒ 一個 16 位元組的 range 永遠只有一塊，它跨不過任何邊界。
#   反證當場戳穿了它：給 `stream_body` 注入「只發第一塊就停」，那一條**照常綠**。
#   ⇒ 判據必須讓 range 自己**長過 CHUNK**，否則它只測到 `seek`，測不到跨塊拼接。
CODE=$(probe -H "Range: bytes=1000-100000" "$BASE/big.bin")
expect_status "Range 跨多块（99001 字节）" 206 "$CODE"
expect_header "Range 跨多块" "Content-Range" "bytes 1000-100000/200000"
expect_bytes  "Range 跨多块" 99001
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
# ★ ★ ★ If-Range 用**強**比較：弱 ETag 一律不匹配。
#   ⚠ 放行的話用戶端會把兩個版本的位元組拼在一起 —— 拼出來的檔案既不回報錯誤、
#   也不是任何一個版本。
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
