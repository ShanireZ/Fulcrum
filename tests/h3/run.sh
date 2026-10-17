#!/usr/bin/env bash
# HTTP/3 端到端（G109 / G110）。
#
# ★ ★ ★ **在這一格之前，批 J 的五步全部是「零件通了」，而沒有任何設定能把入口打開。**
# 前五步的判據都住在 `crates/fulcrum-server/src/quic/` 的 `#[cfg(test)]` 裡：
# 它們證得了「收包循環 + h3 事件循環 + 那座橋」各自對，⚠ **證不了產品會用它們** ——
# 那些判據掛的處理器是判據自己寫的 `EchoHandler`，而產品的執行鏈一行都沒接進去。
#
# # ★ ★ 本格的用戶端是 curl，而**「不自己寫用戶端」本身是判據的一部分**
#
# 前五步的端到端用的是 `quiche` 自己的用戶端 —— 與被測的伺服器端**同一個庫**。
# 那測得了「我們用對了這個庫」，⚠ **測不了互操作**：一處雙方共有的理解偏差
# （ALPN、傳輸參數、QPACK、逐跳頭）會讓兩邊一起錯，而判據全綠。
# 容器裡的 curl 8.14 走的是 **OpenSSL 3.5 的 QUIC 棧 + nghttp3**，
# 與 quiche 沒有一行共同程式碼 ⇒ 它紅的時候，紅的是真的協定問題。
# ★ 下面第 0 步既不信「鏡像裡有沒有 h3 用戶端」這類說法，也不信 `curl -V`：
#   它拿一個**已知沒有 h3 的連接埠**去量，兩個方向都走一遍。
#
# # 本格驗的五件事
#
#   ① **h3 真的能用**：`--http3-only` 打過來，拿到 200 與 `http_version=3`；
#   ② ★ ★ ★ **`Alt-Svc` 出現在 h1/h2 的每一條回應上**（G110 的另一半）——
#      六條互不相同的回應路徑（respond / redir / 錯誤頁 / file_server /
#      reverse_proxy / 快取命中）各查一次。⚠ 這正是 `Downstream` 那個漏斗
#      存在的唯一理由：**少一條路，瀏覽器就有一半的頁面看不到這個廣播**；
#   ③ **兩條反向**：明文連接埠**不發**、h3 自己那側**也不發**。
#      ⚠ 少了它們，一個「無條件加這個頭」的實作在 ② 下面是全綠的；
#   ④ ★ ★ ★ **h3 走的是同一條執行鏈**：路由、`reverse_proxy`、`file_server`
#      在 h3 上給出與 h1 **逐字相同**的回應體；
#   ⑤ ★ ★ **逐跳頭在 h3 上被濾掉**（RFC 9114 §4.2）——
#      ⚠ 這一條**只有串接之後才測得到**：在此之前執行鏈根本喂不到 h3，
#      而現在使用者的一句 `header Connection …` 就能把一條 h3 流打掉。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 連接埠表見 AGENTS.md。**9700–9702 是本場景的**（批 J 第六步新增）。
PORT=${H3_PORT:-9700}
UP_PORT=${H3_UP_PORT:-9701}
PLAIN_PORT=${H3_PLAIN_PORT:-9702}
ADMIN_SOCK="$WORK/admin.sock"
ROOT="$WORK/www"
SNI=h3.example
RESOLVE="$SNI:$PORT:$HOST"

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

echo "=== [0/6] 基线：端口空着、二进制在、★ 而且这把尺子真的会说 h3 ==="
for p in "$PORT" "$UP_PORT" "$PLAIN_PORT"; do
  if port_listening "$p"; then
    echo "H3 TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑。" >&2
    exit 1
  fi
done
ok "$PORT / $UP_PORT / $PLAIN_PORT 都是空的"
[ -x "$BIN" ] || {
  echo "H3 TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── ★ ★ ★ 尺子自證，兩步，缺一不可 ────────────────────────────────────────
#
# ⚠ 本格**每一條**斷言都建在「curl 真的走了 h3」上，而 curl 有一個安靜的失敗模式：
#   `--http3`（不帶 `-only`）**會回落到 h1/h2**。⇒ 全場一律用 `--http3-only`。
# ★ 但「命令行裡寫了 `--http3-only`」仍然不等於「這個 curl 編進了 QUIC 後端」——
#   一個沒有 h3 的 curl 會直接回報錯誤，而那種紅看起來像**產品**沒起來。
# ⇒ 先問它自己（`-V`），再拿一個**已知沒有任何東西在聽 UDP** 的連接埠去量：
#   它必須**失敗**。★ 只做前者的話，判據信的是工具的自報家門；
#   只做後者的話，分不出「curl 不會 h3」與「那個連接埠確實沒人聽」。
if curl -V | grep -qi 'HTTP3'; then
  ok "curl 自报会 h3：$(curl -V | head -1 | tr -d '\r')"
else
  echo "H3 TESTS FAILED: 这个 curl 没有 HTTP3 —— 本格的每一条断言都要靠它。" >&2
  curl -V >&2
  exit 1
fi
if curl -sS --http3-only --max-time 5 "https://$HOST:$PLAIN_PORT/" >/dev/null 2>&1; then
  echo "H3 TESTS FAILED: 对着一个空端口 --http3-only 竟然成功了 —— 这把尺子量不了东西。" >&2
  exit 1
fi
ok "★ 空端口上 --http3-only 如期失败 ⇒ 它真的在走 QUIC，而不是悄悄回落"

# ── 自簽證書 ───────────────────────────────────────────────────────────────
#
# ★ 現簽而不是提交進儲存庫：儲存庫裡的測試證書遲早過期，而過期那天紅的是
#   「TLS 壞了」。⚠ SAN 必須有 `$SNI` —— 樞衡按證書自己的 SAN 決定它裝在哪些 SNI 上。
# ★ ★ 全場用 `--cacert` 而**不是** `-k`：這一格順帶證明了
#   **同一張證書經 BoringSSL 裝進 quiche 之後，在 QUIC 上也真的驗得過**（G104）。
#   ⚠ 用 `-k` 的話，「證書沒裝上」與「裝上了」這兩種情況長得一模一樣。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=$SNI" \
  -addext "subjectAltName=DNS:$SNI" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "H3 TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

mkdir -p "$ROOT/static"
printf 'from-disk-over-h3' > "$ROOT/static/x"

cat > "$WORK/up.Fulcrumfile" <<CONF
:$UP_PORT {
    handle /cached {
        header Cache-Control "max-age=60"
        respond 200 "upstream-cached"
    }
    respond 200 "from-upstream"
}
CONF

# ⚠ `handle_errors` 那條給的是**錯誤頁**那條寫回應的路 —— 它與 `respond` 走的
#   是同一個函式，但走到它的路不同（路由沒命中 → `write_error`）。
# ★ `/hopbyhop` 那條 `header Connection …` 是**故意**的：h1 上它完全合法，
#   而 h3 上發出去對端會按協定錯誤重置這條流（RFC 9114 §4.2）。
cat > "$WORK/fulcrum.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

$SNI:$PORT {
    tls $WORK/tls.crt $WORK/tls.key

    handle /static/* {
        file_server {
            root $ROOT
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
    handle /proxy {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /hopbyhop {
        header Connection "keep-alive"
        respond 200 "hop-by-hop-probe"
    }
    handle /redir {
        redir * https://$SNI/moved 302
    }
    handle / {
        respond 200 "h3-ok"
    }
    handle_errors {
        respond 410 "error-page-{status}"
    }
}

http://plain.example:$PLAIN_PORT {
    respond 200 "plain-ok"
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
wait_port "$UP_PORT" || { echo "H3 TESTS FAILED: 上游没起来" >&2; cat "$WORK/up.log" >&2; exit 1; }
wait_port "$PORT"    || { echo "H3 TESTS FAILED: $PORT（TCP）没起来" >&2; cat "$WORK/fulcrum.log" >&2; exit 1; }
wait_port "$PLAIN_PORT" || { echo "H3 TESTS FAILED: $PLAIN_PORT 没起来" >&2; cat "$WORK/fulcrum.log" >&2; exit 1; }

# ── helper ─────────────────────────────────────────────────────────────────
#
# ⚠ ⚠ **`--http3-only` 不許省成 `--http3`**：後者會在 QUIC 不通時**回落到 TCP**，
#   於是「h3 壞了」與「h3 好了」拿到同一個 200。★ 全場只有 `h1` / `h3` 兩個入口。
h3() {
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s --http3-only -o "$WORK/body" -D "$WORK/hdr" \
    -w '%{http_code} %{http_version}' --max-time 20 \
    --cacert "$WORK/tls.crt" --resolve "$RESOLVE" "https://$SNI:$PORT$1" 2>/dev/null || echo "000 0"
}
h1() {
  : > "$WORK/body"
  : > "$WORK/hdr"
  curl -s --http1.1 -o "$WORK/body" -D "$WORK/hdr" \
    -w '%{http_code} %{http_version}' --max-time 10 \
    --cacert "$WORK/tls.crt" --resolve "$RESOLVE" "https://$SNI:$PORT$1" 2>/dev/null || echo "000 0"
}
hdr() { { grep -i "^$1:" "$WORK/hdr" || true; } | head -1 | cut -d' ' -f2- | tr -d '\r'; }
body() { cat "$WORK/body"; }

expect() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望「$want」，实际「$got」"; fi
}

# ── [1/6] h3 真的通了 ──────────────────────────────────────────────────────
echo "=== [1/6] ★★★ h3 端到端：一条真的 HTTP/3 请求 ==="
R=$(h3 /)
expect "GET / 走 h3" "200 3" "$R"
expect "GET / 的响应体" "h3-ok" "$(body)"

# ── [2/6] h3 走的是**同一條執行鏈** ───────────────────────────────────────
echo "=== [2/6] ★★★ h3 上跑的是同一条执行链（不是一个另写的桩）==="
# ★ 判據是「與 h1 逐字相同」，而不是「有內容」：後者對一個只會回 200 的樁也成立。
R=$(h3 /proxy);      P3=$(body); expect "reverse_proxy 走 h3" "200 3" "$R"
R=$(h1 /proxy);      P1=$(body); expect "reverse_proxy 走 h1" "200 1.1" "$R"
if [ "$P3" = "$P1" ] && [ "$P3" = "from-upstream" ]; then
  ok "★ 同一条 reverse_proxy 路由在 h3 与 h1 上给出逐字相同的体（$P3）"
else
  fail "reverse_proxy 两条路不一致：h3=「$P3」 h1=「$P1」"
fi
R=$(h3 /static/x);   S3=$(body); expect "file_server 走 h3" "200 3" "$R"
if [ "$S3" = "from-disk-over-h3" ]; then
  ok "★ file_server 的字节经 h3 原样到达"
else
  fail "file_server 经 h3 拿到的不是磁盘上那份：「$S3」"
fi
# ★ 路由沒命中 → 站台的 `handle_errors`。⚠ 它與 `respond` 是**兩條不同的路**
#   （`write_error`），而兩條都要在 h3 上成立。
# ★ 狀態碼取 410 而**體裡**寫 `{status}`：這一條同時釘住兩件事 ——
#   錯誤頁自己的碼（410）與**原始**錯誤碼（404）**不是同一個數**。
#   ⚠ 兩邊都寫 404 的話，一個把 `{status}` 展開成「錯誤頁自己的碼」的實作照樣全綠。
R=$(h3 /nope);       E3=$(body)
expect "错误页走 h3" "410 3" "$R"
expect "错误页的体（h3）：{status} 是**原始**错误码" "error-page-404" "$E3"

# ── [3/6] ★★★ 逐跳頭在 h3 上必須消失（RFC 9114 §4.2）─────────────────────
echo "=== [3/6] ★★★ 逐跳头：h1 上合法，h3 上必须被滤掉 ==="
# ⚠ ⚠ **這一條只有串接之後才測得到**：在批 J 第六步之前，執行鏈根本喂不到 h3。
#   ★ 而它的危險面正是「串接」本身帶進來的 —— 使用者寫一句 `header Connection …`
#     就能讓一條 h3 流被對端重置，而 h1 上那句話完全正常。
R=$(h1 /hopbyhop)
expect "h1 上带 Connection 的响应" "200 1.1" "$R"
if [ -n "$(hdr Connection)" ]; then
  ok "★ 对照：同一条配置在 h1 上**真的**发出了 Connection 头（$(hdr Connection)）"
else
  # ⚠ 沒有這條對照，下面那條「h3 上沒有」就與「這條設定根本沒加過頭」無法區分。
  fail "对照不成立：h1 上也没看到 Connection 头 —— 那么 h3 上没有它证明不了任何事"
fi
R=$(h3 /hopbyhop)
expect "h3 上同一条请求" "200 3" "$R"
expect "h3 上的响应体（流没有被重置）" "hop-by-hop-probe" "$(body)"
if [ -z "$(hdr Connection)" ]; then
  ok "★★★ 逐跳头在 h3 那一侧消失了"
else
  fail "h3 上仍然发出了 Connection: $(hdr Connection) —— RFC 9114 §4.2 禁止"
fi

# ── [4/6] ★★★ Alt-Svc 出現在 h1/h2 的**每一條**回應上（G110）────────────
echo "=== [4/6] ★★★ Alt-Svc：六条互不相同的响应路径，一条都不能漏 ==="
# ⚠ ⚠ 不發它，瀏覽器**永遠不會主動嘗試 h3** —— 整個入口對真實使用者等於不存在。
#   ★ 而「每一條」不是潔癖：漏掉的那一條如果正好是首頁，廣播就等於沒有。
WANT="h3=\":$PORT\"; ma=86400"
check_altsvc() {
  local what=$1 path=$2 want_code=$3
  local r
  r=$(h1 "$path")
  if [ "${r%% *}" != "$want_code" ]; then
    fail "$what 期望 $want_code，实际 ${r%% *}"
    return
  fi
  expect "$what 的 Alt-Svc" "$WANT" "$(hdr Alt-Svc)"
}
check_altsvc "① respond"        /            200
check_altsvc "② redir"          /redir       302
check_altsvc "③ 错误页"          /nope        410
check_altsvc "④ file_server"     /static/x    200
check_altsvc "⑤ reverse_proxy"   /proxy       200
# ⑥ 快取：**回源那一條與命中那一條是兩段不同的程式碼**（後者走 `write_cached`）。
#   ⚠ 只查其中一條的話，另一條漏掉時沒有任何東西會說。
check_altsvc "⑥ 缓存（回源）"     /cached      200
MISS=$(hdr X-Fulcrum-Cache)
check_altsvc "⑥ 缓存（命中）"     /cached      200
HIT=$(hdr X-Fulcrum-Cache)
# ⚠ ⚠ 本儲存庫的口徑是：**回源沒有這個頭，命中是 `HIT`**（`write_cached` 才插它）。
#   ★ 不要寫成「兩次都非空且不相等」—— 那會紅，而紅得**對**：
#     回源那次本來就沒有這個頭。⇒ 斷言按真實口徑寫，不按我以為的口徑寫。
if [ -z "$MISS" ] && [ "$HIT" = "HIT" ]; then
  ok "★ 两次真的分别是回源与命中（无头 → HIT）—— 第二条走的确实是 write_cached"
else
  # ⚠ 少了這一步，「⑥ 快取（命中）」可能其實又回了一次源，於是 `write_cached`
  #   那條路一次都沒被走到，而兩條斷言都是綠的。
  fail "两次 /cached 没有分出回源与命中（第一次「$MISS」第二次「$HIT」）—— ⑥ 的第二条没验到 write_cached"
fi

# ── [5/6] 兩條反向 ─────────────────────────────────────────────────────────
echo "=== [5/6] ★★★ 反向：不该发的地方一个字都不许有 ==="
# ⚠ ⚠ 少了這兩條，一個「無條件給每條回應加這個頭」的實作在 [4/6] 下面**全綠**。
: > "$WORK/hdr"
CODE=$(curl -s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 10 \
  --resolve "plain.example:$PLAIN_PORT:$HOST" "http://plain.example:$PLAIN_PORT/" 2>/dev/null || echo 000)
expect "明文站点还在服务" "200" "$CODE"
if [ -z "$(hdr Alt-Svc)" ]; then
  ok "★★★ 反向①：明文端口 $PLAIN_PORT **不发** Alt-Svc（那里没有 h3 可去）"
else
  fail "明文端口上发了 Alt-Svc: $(hdr Alt-Svc) —— 那个端口上没有任何东西在听 UDP"
fi
R=$(h3 /)
expect "h3 请求（反向②的前提）" "200 3" "$R"
if [ -z "$(hdr Alt-Svc)" ]; then
  ok "★★★ 反向②：h3 自己那一侧**不发** Alt-Svc（客户端已经在 h3 上了）"
else
  fail "h3 的响应里也带了 Alt-Svc: $(hdr Alt-Svc)"
fi

# ── [6/6] 裝載紀錄 ─────────────────────────────────────────────────────────
echo "=== [6/6] 装载日志：h3 入口要在启动时说出来 ==="
# ★ 「443 上到底開沒開 h3」是維運第一個會問的問題 —— 它必須在紀錄裡，
#   而不是只能靠抓包知道。
if grep -q "\[quic\] 监听 $HOST:$PORT" "$WORK/fulcrum.log"; then
  ok "★ 启动日志说了 h3 入口：$(grep -o "\[quic\] 监听 .*" "$WORK/fulcrum.log" | head -1)"
else
  fail "启动日志里没有 [quic] 监听 $HOST:$PORT"
fi
# ★ 反向：明文連接埠**不該**有 h3 監聽器（G110 是「跟著 tls 開」，不是「全開」）。
if grep -q "\[quic\] 监听 $HOST:$PLAIN_PORT" "$WORK/fulcrum.log"; then
  fail "明文端口 $PLAIN_PORT 上也起了 h3 监听器 —— G110 是跟着 tls 开"
else
  ok "★ 反向：明文端口上没有起 h3 监听器"
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "H3 TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 枢衡日志 ──" >&2
  cat "$WORK/fulcrum.log" >&2
  echo "── 上游日志 ──" >&2
  cat "$WORK/up.log" >&2
  exit 1
fi
echo "H3 TESTS PASSED —— ★★★ 一条真的 HTTP/3 请求端到端走通（客户端是 curl 的 OpenSSL-QUIC 栈，与被测的 quiche 没有一行共同代码）/ ★★★ h3 与 h1 跑的是同一条执行链（reverse_proxy · file_server · 错误页逐字相同）/ ★★★ 逐跳头在 h3 那侧消失而 h1 上仍在（对照成立）/ ★★★ Alt-Svc 出现在六条互不相同的响应路径上（含缓存回源与命中两段代码）/ ★★★ 两条反向：明文端口不发、h3 自己不发 / 装载日志两向。"
