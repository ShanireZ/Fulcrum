#!/usr/bin/env bash
# HTTP/3 端到端（G109 / G110）。
#
# ★ ★ ★ **在这一格之前，批 J 的五步全部是「零件通了」，而没有任何配置能把入口打开。**
# 前五步的判据都住在 `crates/fulcrum-server/src/quic/` 的 `#[cfg(test)]` 里：
# 它们证得了「收包循环 + h3 事件循环 + 那座桥」各自对，⚠ **证不了产品会用它们** ——
# 那些判据挂的处理器是判据自己写的 `EchoHandler`，而产品的执行链一行都没接进去。
#
# # ★ ★ 本格的客户端是 curl，而**「不自己写客户端」本身是判据的一部分**
#
# 前五步的端到端用的是 `quiche` 自己的客户端 —— 与被测的服务端**同一个库**。
# 那测得了「我们用对了这个库」，⚠ **测不了互操作**：一处双方共有的理解偏差
# （ALPN、传输参数、QPACK、逐跳头）会让两边一起错，而判据全绿。
# 容器里的 curl 8.14 走的是 **OpenSSL 3.5 的 QUIC 栈 + nghttp3**，
# 与 quiche 没有一行共同代码 ⇒ 它红的时候，红的是真的协议问题。
# ★ 下面第 0 步既不信「镜像里有没有 h3 客户端」这类说法，也不信 `curl -V`：
#   它拿一个**已知没有 h3 的端口**去量，两个方向都走一遍。
#
# # 本格验的五件事
#
#   ① **h3 真的能用**：`--http3-only` 打过来，拿到 200 与 `http_version=3`；
#   ② ★ ★ ★ **`Alt-Svc` 出现在 h1/h2 的每一条响应上**（G110 的另一半）——
#      六条互不相同的响应路径（respond / redir / 错误页 / file_server /
#      reverse_proxy / 缓存命中）各查一次。⚠ 这正是 `Downstream` 那个漏斗
#      存在的唯一理由：**少一条路，浏览器就有一半的页面看不到这个广播**；
#   ③ **两条反向**：明文端口**不发**、h3 自己那侧**也不发**。
#      ⚠ 少了它们，一个「无条件加这个头」的实现在 ② 下面是全绿的；
#   ④ ★ ★ ★ **h3 走的是同一条执行链**：路由、`reverse_proxy`、`file_server`
#      在 h3 上给出与 h1 **逐字相同**的响应体；
#   ⑤ ★ ★ **逐跳头在 h3 上被滤掉**（RFC 9114 §4.2）——
#      ⚠ 这一条**只有接线之后才测得到**：在此之前执行链根本喂不到 h3，
#      而现在用户的一句 `header Connection …` 就能把一条 h3 流打掉。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
# ⚠ 端口表见 AGENTS.md。**9700–9702 是本场景的**（批 J 第六步新增）。
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

# ── ★ ★ ★ 尺子自证，两步，缺一不可 ────────────────────────────────────────
#
# ⚠ 本格**每一条**断言都建在「curl 真的走了 h3」上，而 curl 有一个安静的失败模式：
#   `--http3`（不带 `-only`）**会回落到 h1/h2**。⇒ 全场一律用 `--http3-only`。
# ★ 但「命令行里写了 `--http3-only`」仍然不等于「这个 curl 编进了 QUIC 后端」——
#   一个没有 h3 的 curl 会直接报错，而那种红看起来像**产品**没起来。
# ⇒ 先问它自己（`-V`），再拿一个**已知没有任何东西在听 UDP** 的端口去量：
#   它必须**失败**。★ 只做前者的话，判据信的是工具的自报家门；
#   只做后者的话，分不出「curl 不会 h3」与「那个端口确实没人听」。
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

# ── 自签证书 ───────────────────────────────────────────────────────────────
#
# ★ 现签而不是提交进仓库：仓库里的测试证书迟早过期，而过期那天红的是
#   「TLS 坏了」。⚠ SAN 必须有 `$SNI` —— 枢衡按证书自己的 SAN 决定它装在哪些 SNI 上。
# ★ ★ 全场用 `--cacert` 而**不是** `-k`：这一格顺带证明了
#   **同一张证书经 BoringSSL 装进 quiche 之后，在 QUIC 上也真的验得过**（G104）。
#   ⚠ 用 `-k` 的话，「证书没装上」与「装上了」这两种情况长得一模一样。
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

# ⚠ `handle_errors` 那条给的是**错误页**那条写响应的路 —— 它与 `respond` 走的
#   是同一个函数，但走到它的路不同（路由没命中 → `write_error`）。
# ★ `/hopbyhop` 那条 `header Connection …` 是**故意**的：h1 上它完全合法，
#   而 h3 上发出去对端会按协议错误重置这条流（RFC 9114 §4.2）。
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
# ⚠ ⚠ **`--http3-only` 不许省成 `--http3`**：后者会在 QUIC 不通时**回落到 TCP**，
#   于是「h3 坏了」与「h3 好了」拿到同一个 200。★ 全场只有 `h1` / `h3` 两个入口。
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

# ── [2/6] h3 走的是**同一条执行链** ───────────────────────────────────────
echo "=== [2/6] ★★★ h3 上跑的是同一条执行链（不是一个另写的桩）==="
# ★ 判据是「与 h1 逐字相同」，而不是「有内容」：后者对一个只会回 200 的桩也成立。
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
# ★ 路由没命中 → 站点的 `handle_errors`。⚠ 它与 `respond` 是**两条不同的路**
#   （`write_error`），而两条都要在 h3 上成立。
# ★ 状态码取 410 而**体里**写 `{status}`：这一条同时钉住两件事 ——
#   错误页自己的码（410）与**原始**错误码（404）**不是同一个数**。
#   ⚠ 两边都写 404 的话，一个把 `{status}` 展开成「错误页自己的码」的实现照样全绿。
R=$(h3 /nope);       E3=$(body)
expect "错误页走 h3" "410 3" "$R"
expect "错误页的体（h3）：{status} 是**原始**错误码" "error-page-404" "$E3"

# ── [3/6] ★★★ 逐跳头在 h3 上必须消失（RFC 9114 §4.2）─────────────────────
echo "=== [3/6] ★★★ 逐跳头：h1 上合法，h3 上必须被滤掉 ==="
# ⚠ ⚠ **这一条只有接线之后才测得到**：在批 J 第六步之前，执行链根本喂不到 h3。
#   ★ 而它的危险面正是「接线」本身带进来的 —— 用户写一句 `header Connection …`
#     就能让一条 h3 流被对端重置，而 h1 上那句话完全正常。
R=$(h1 /hopbyhop)
expect "h1 上带 Connection 的响应" "200 1.1" "$R"
if [ -n "$(hdr Connection)" ]; then
  ok "★ 对照：同一条配置在 h1 上**真的**发出了 Connection 头（$(hdr Connection)）"
else
  # ⚠ 没有这条对照，下面那条「h3 上没有」就与「这条配置根本没加过头」无法区分。
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

# ── [4/6] ★★★ Alt-Svc 出现在 h1/h2 的**每一条**响应上（G110）────────────
echo "=== [4/6] ★★★ Alt-Svc：六条互不相同的响应路径，一条都不能漏 ==="
# ⚠ ⚠ 不发它，浏览器**永远不会主动尝试 h3** —— 整个入口对真实用户等于不存在。
#   ★ 而「每一条」不是洁癖：漏掉的那一条如果正好是首页，广播就等于没有。
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
# ⑥ 缓存：**回源那一条与命中那一条是两段不同的代码**（后者走 `write_cached`）。
#   ⚠ 只查其中一条的话，另一条漏掉时没有任何东西会说。
check_altsvc "⑥ 缓存（回源）"     /cached      200
MISS=$(hdr X-Fulcrum-Cache)
check_altsvc "⑥ 缓存（命中）"     /cached      200
HIT=$(hdr X-Fulcrum-Cache)
# ⚠ ⚠ 本仓库的口径是：**回源没有这个头，命中是 `HIT`**（`write_cached` 才插它）。
#   ★ 不要写成「两次都非空且不相等」—— 那会红，而红得**对**：
#     回源那次本来就没有这个头。⇒ 断言按真实口径写，不按我以为的口径写。
if [ -z "$MISS" ] && [ "$HIT" = "HIT" ]; then
  ok "★ 两次真的分别是回源与命中（无头 → HIT）—— 第二条走的确实是 write_cached"
else
  # ⚠ 少了这一步，「⑥ 缓存（命中）」可能其实又回了一次源，于是 `write_cached`
  #   那条路一次都没被走到，而两条断言都是绿的。
  fail "两次 /cached 没有分出回源与命中（第一次「$MISS」第二次「$HIT」）—— ⑥ 的第二条没验到 write_cached"
fi

# ── [5/6] 两条反向 ─────────────────────────────────────────────────────────
echo "=== [5/6] ★★★ 反向：不该发的地方一个字都不许有 ==="
# ⚠ ⚠ 少了这两条，一个「无条件给每条响应加这个头」的实现在 [4/6] 下面**全绿**。
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

# ── [6/6] 装载日志 ─────────────────────────────────────────────────────────
echo "=== [6/6] 装载日志：h3 入口要在启动时说出来 ==="
# ★ 「443 上到底开没开 h3」是运维第一个会问的问题 —— 它必须在日志里，
#   而不是只能靠抓包知道。
if grep -q "\[quic\] 监听 $HOST:$PORT" "$WORK/fulcrum.log"; then
  ok "★ 启动日志说了 h3 入口：$(grep -o "\[quic\] 监听 .*" "$WORK/fulcrum.log" | head -1)"
else
  fail "启动日志里没有 [quic] 监听 $HOST:$PORT"
fi
# ★ 反向：明文端口**不该**有 h3 监听器（G110 是「跟着 tls 开」，不是「全开」）。
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
