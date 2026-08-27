#!/usr/bin/env bash
# HTTP 面的 PROXY protocol「收」半边端到端（**M2 批 L 第 ① 步**）。
#
# ★ ★ 为什么它是**独立一格**而不是塞进 `tests/serve/run.sh`：那一格验的是
#   「七层路由决策被执行对了没有」，而这一格验的是**连接开头的那几十个字节** ——
#   它发生在 HTTP 之前、甚至在 TLS 之前。⚠ 两者混在一起时，一条断言红了，
#   读日志的人会先去看路由，而问题其实在监听器那一层。
#
# ★ **判据挂在 `remote_ip` 匹配器上，而不是挂在某个自造的调试头上**：
#   `remote_ip` 是**用户看得见的那一面** —— 它决定一条 `handle @internal` 命不命中。
#   ⇒ 这一格量的是「用户写的规则会不会按真实客户端生效」，不是「我们内部记对了没有」。
#
# 端口（★ 与其余场景全都错开，见 AGENTS.md 那张端口表）：
#   9800 实例 A 的 HTTP 站点（**配了** `proxy_protocol_from`）
#   9801 实例 A 的 HTTPS 站点（同一个进程、同一份全局清单）
#   9802 实例 B 的 HTTP 站点（**没配** —— 反向判据要它）
#   9803 **永远没人在听**（客户端自证要它）
#
# ★ 客户端是 `client.py`（裸 socket），理由见它的文件头：
#   curl 没有任何办法让我们在连接开头塞自己的字节。

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
CLIENT="$REPO/tests/proxyproto/client.py"
WORK=$(mktemp -d)
HOST=127.0.0.1
A_HTTP=${A_HTTP:-9800}
A_TLS=${A_TLS:-9801}
B_HTTP=${B_HTTP:-9802}
DEAD_PORT=${DEAD_PORT:-9803}
ADMIN_SOCK="$WORK/admin.sock"

FAILS=0
PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid waited
  # ★ SIGINT 而不是 SIGTERM：Pingora 把 SIGTERM 当优雅停机，会等完整排空窗口。
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

# `$1` = 说明，`$2` = 期望的整行，`$3` = 实测的整行。
expect_line() {
  if [ "$3" = "$2" ]; then
    ok "$1"
  else
    fail "$1：期望「$2」实际「$3」"
  fi
}

# ── [0/5] 基线：端口未被占 + **客户端自己会不会说「不」** ────────────────────
#
# ★ ★ 两条自证缺一不可（这条纪律是 `tests/h3/run.sh` 立的）：
#   ① 端口没被占 —— 否则后面每一条断言测的都是别人的服务；
#   ② **客户端对着一个没人听的端口必须失败** —— 否则它「永远返回点什么」这件事
#      会让下面所有反向判据变成空转，而它们看起来全是绿的。
echo "=== [0/5] 基线：端口未被占用，且客户端自己会说「不」 ==="
for p in "$A_HTTP" "$A_TLS" "$B_HTTP" "$DEAD_PORT"; do
  if port_listening "$p"; then
    echo "PROXYPROTO TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
ok "四个端口都空着"

BASELINE=$(python3 "$CLIENT" --port "$DEAD_PORT" --timeout 2 2>&1 || true)
case "$BASELINE" in
  ERROR=*) ok "客户端对着没人听的端口会失败（$BASELINE）—— 它说得出「不」" ;;
  *) echo "PROXYPROTO TESTS FAILED: 客户端对着空端口居然给出了「$BASELINE」。" >&2
     echo "  ⇒ 下面所有反向判据都会变成空转，本次结果不可采信。" >&2
     exit 1 ;;
esac

# ── 自签证书（HTTPS 那一格要）──────────────────────────────────────────────
#
# ⚠ SAN 必须有 `a.example`：枢衡按**证书自己的 SAN** 决定这张证书装在哪些 SNI 上。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" \
  -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "PROXYPROTO TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ── 两份配置 ────────────────────────────────────────────────────────────────
#
# ★ ★ 站点里那条 `@real remote_ip 192.0.2.0/24` 就是整格的判据核心：
#   `192.0.2.0/24` 是 RFC 5737 的**文档专用网段**，不可能是任何真实对端 ——
#   ⇒ 它命中，当且仅当「PROXY 头里那个地址真的被用上了」。
#   ⚠ 若换成一个可能真出现的网段，一条「压根没解析头」的实现也可能碰巧绿。
cat > "$WORK/a.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
    proxy_protocol_from 127.0.0.0/8
}

http://a.example:$A_HTTP {
    @real remote_ip 192.0.2.0/24
    handle @real {
        respond 200 "real"
    }
    respond 200 "peer"
}

a.example:$A_TLS {
    tls $WORK/tls.crt $WORK/tls.key
    @real remote_ip 192.0.2.0/24
    handle @real {
        respond 200 "real-tls"
    }
    respond 200 "peer-tls"
}
CONF

# ⚠ 实例 B **有意不写** `proxy_protocol_from` —— 它是第 [4/5] 节那条反向的对照物。
cat > "$WORK/b.Fulcrumfile" <<CONF
http://a.example:$B_HTTP {
    @real remote_ip 192.0.2.0/24
    handle @real {
        respond 200 "real"
    }
    respond 200 "peer"
}
CONF

# ── [1/5] 起服务 ────────────────────────────────────────────────────────────
echo "=== [1/5] 起两个实例 ==="
start() {
  local name=$1 conf=$2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
start a "$WORK/a.Fulcrumfile"
start b "$WORK/b.Fulcrumfile"

for p in "$A_HTTP" "$A_TLS" "$B_HTTP"; do
  wait_port "$p" || {
    echo "PROXYPROTO TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "三个监听都起来了"

# ── [2/5] 正向：受信来源发头 ⇒ `remote_ip` 看到的是**头里那个** ─────────────
echo "=== [2/5] 正向：受信来源发头，remote_ip 换成真实客户端 ==="

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1:192.0.2.7:56324 --split)
expect_line "v1，头与请求**分两段**发" "STATUS=200 BODY=real" "$R"

# ★ ★ ★ 这一条是 fork 那侧 `rewind()` 存在的**全部理由**。
#   一次 read 会把 PROXY 头与它后面的 `GET` **一起**读回来；多读到的那几个字节
#   必须原样还回流里。⚠ 丢掉 rewind 的实现**只在这一条上坏**，
#   而上面那条（分两段发）照样绿 —— 两条缺一不可。
R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1:192.0.2.7:56324)
expect_line "★★★ v1，头与请求在**同一个段**里（多读的字节要还回去）" "STATUS=200 BODY=real" "$R"

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v2:192.0.2.9:41000)
expect_line "★★★ v2（二进制），头与请求在同一个段里" "STATUS=200 BODY=real" "$R"

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v2:192.0.2.9:41000 --split)
expect_line "v2，分两段发" "STATUS=200 BODY=real" "$R"

# ── [3/5] 边界：`LOCAL` / `UNKNOWN` **不覆盖**，继续用 socket 对端 ──────────
#
# ⚠ ⚠ 这两条同时是上面那四条的**对照**：它们走的是同一条代码路径、
#   同样读掉了一个合法的头，而结论相反。
#   ★ 没有它们，一个「只要看见 PROXY 前缀就把 remote_ip 塞成 192.0.2.x」的实现
#     会在 [2/5] 下面全绿。
echo "=== [3/5] 边界：LOCAL / UNKNOWN 是合法的，而它们不覆盖地址 ==="

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1unknown)
expect_line "★★ v1 \`PROXY UNKNOWN\` ⇒ 头收下了，地址**不换**" "STATUS=200 BODY=peer" "$R"

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v2local)
expect_line "★★ v2 \`LOCAL\` ⇒ 同上（健康检查就长这样）" "STATUS=200 BODY=peer" "$R"

# ── [4/5] ★★★ 两条反向 ──────────────────────────────────────────────────────
echo "=== [4/5] 两条反向 ==="

# ⑦ 清单内的来源**不发头** ⇒ 关连接（§10 拍板的语义）。
#    ★ 取「必须发」而不是「发了就用、没发就算了」的理由是**安全上的**：
#      若允许清单内的来源选择性地不发头，它就能让枢衡改用 socket 对端 ——
#      而那个地址正是 LB 自己，于是一条 `remote_ip 10.0.0.0/8` 规则会命中它。
R=$(python3 "$CLIENT" --port "$A_HTTP")
expect_line "★★★ 清单内的来源不发头 ⇒ 连接被关掉" "CLOSED" "$R"

# ⑧ ★ ★ ★ **没配 `proxy_protocol_from` 的实例：一个字节都不读。**
#    ⇒ 那段 PROXY 文本被当成 HTTP 请求行，于是是一个**坏请求**。
#    ⚠ ⚠ 少了这一条，一个「无条件解析 PROXY 头」的实现会在 [2/5] 下面全绿 ——
#      而它意味着**任何客户端都能自称是任意 IP**，`remote_ip` 规则随之失守。
R=$(python3 "$CLIENT" --port "$B_HTTP" --header v1:192.0.2.7:56324)
case "$R" in
  "STATUS=200 BODY=real")
    fail "★★★ 没配信任清单的实例居然认了 PROXY 头 —— 任何人都能自称任意 IP" ;;
  "STATUS=200 BODY=peer")
    fail "★★★ 没配信任清单的实例把 PROXY 头**读掉丢弃**了 —— 口径是「一个字节都不读」" ;;
  STATUS=4*|CLOSED)
    ok "★★★ 没配的实例一个字节都不读（那段文本被当成请求行 ⇒ $R）" ;;
  *)
    fail "★★★ 没配的实例给出了没预料到的结果：$R" ;;
esac

# 对照：同一个实例、不发头 ⇒ 一切正常。★ 它证明上面那一条的成因是**那个头**，
# 而不是「实例 B 根本没起来」。
R=$(python3 "$CLIENT" --port "$B_HTTP")
expect_line "对照：同一个实例不发头 ⇒ 正常服务" "STATUS=200 BODY=peer" "$R"

# ── [5/5] TLS + 换配置立刻生效 ──────────────────────────────────────────────
echo "=== [5/5] PROXY 头在 ClientHello 之前；以及换配置立刻生效 ==="

# ⑨ ★ ★ ★ **位置判据**：PROXY 头按规格在 **TLS 之前**。
#    ⚠ 若那段读取被放在 TLS 握手**之后**，这一条会红在「握手失败」上 ——
#      因为 BoringSSL 会把 `PROXY TCP4 …` 当成一个畸形的 ClientHello。
#    ★ 它是唯一一条能分辨「读对了地方」与「读对了内容」的断言。
R=$(python3 "$CLIENT" --port "$A_TLS" --tls --header v1:192.0.2.7:56324)
expect_line "★★★ HTTPS：PROXY 头在 ClientHello 之前，握手成功且地址是真客户端" \
  "STATUS=200 BODY=real-tls" "$R"

# ⚠ ⚠ **同一个产品行为，在两条客户端上长得不一样**：明文那侧读到 0 字节（`CLOSED`），
#   而 TLS 那侧是 Python 的 ssl 在握手中途撞上 EOF（`UNEXPECTED_EOF_WHILE_READING`）。
#   ★ 那是**客户端库**的性质，不是产品的 —— 所以判据不能照抄明文那条的期望值。
#   ⇒ 判的是**那件真正要守的事：握手绝不能成功**；两种「没成功」的形状都收，
#     而任何一个 `STATUS=` 都判红（它意味着连接活下来了）。
R=$(python3 "$CLIENT" --port "$A_TLS" --tls)
case "$R" in
  STATUS=*)
    fail "★★ HTTPS：清单内不发头，而握手居然成功了（$R）—— 那条「必须发」的语义没生效" ;;
  CLOSED|*EOF*)
    ok "★★ HTTPS：清单内不发头 ⇒ 连接被关（TLS 也不例外，$R）" ;;
  *)
    fail "★★ HTTPS：清单内不发头，给出了没预料到的结果：$R" ;;
esac

# ⑩ ★ ★ 换配置立刻生效 —— 守的是 §11 **D19** 那个形状
#    （「改了配置、`POST /load` 回 200、而那条指令什么都没做」）。
#    ⚠ 判据挂在**数据面**上，不是挂在「/load 返回了 200」上。
cat > "$WORK/a2.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
    proxy_protocol_from 10.0.0.0/8
}

http://a.example:$A_HTTP {
    @real remote_ip 192.0.2.0/24
    handle @real {
        respond 200 "real"
    }
    respond 200 "peer"
}

a.example:$A_TLS {
    tls $WORK/tls.crt $WORK/tls.key
    @real remote_ip 192.0.2.0/24
    handle @real {
        respond 200 "real-tls"
    }
    respond 200 "peer-tls"
}
CONF
"$BIN" compile "$WORK/a2.Fulcrumfile" > "$WORK/a2.json" 2>/dev/null || {
  echo "PROXYPROTO TESTS FAILED: compile 生成不出新配置" >&2
  exit 1
}
CODE=$(curl -s -o "$WORK/admin.out" -w '%{http_code}' \
  --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$(cat "$WORK/a2.json")" \
  "http://localhost/load" 2>/dev/null || echo "000")
if [ "$CODE" = "200" ]; then
  ok "管理面：把信任清单换成 10.0.0.0/8（不再含 127.0.0.1）"
else
  fail "全量 load 没成功（$CODE）：$(cat "$WORK/admin.out" 2>/dev/null)"
fi

# ★ ★ ★ 换完之后 127.0.0.1 **不再受信** ⇒ 它发的头一个字节都不该被读，
#   于是那段文本变成请求行 ⇒ 坏请求。
#   ⚠ 若这里仍然是 `BODY=real`，说明策略拿的是一份**装载时的快照** ——
#     那就是 D19 那个形状：配置改了、load 回了 200、而运行时没跟。
R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1:192.0.2.7:56324)
case "$R" in
  "STATUS=200 BODY=real")
    # ⚠ ⚠ **这条消息不许只说一个成因**：反证时实测过，把 fork 里那道信任门整个绕开
    #   也会让本条红，而那时「拿的是快照」是一个**与事实不符的诊断**。
    #   ★ ⇒ 说出**观测到的事实**，再列出两条候选，让读的人自己去分 ——
    #     一条只名指一个成因的失败消息，会在它猜错的那次把人带到反方向去。
    fail "★★★ 换过配置之后 127.0.0.1 仍被信任（$R）。两条候选：\
① 策略拿的是装载时快照而不是当前配置（D19 形状）；\
② fork 那侧的信任门根本没起作用（那样上面第 [4/5] 节也会红 —— 先去看那里）" ;;
  STATUS=4*|CLOSED)
    ok "★★★ 换配置**立刻生效**：127.0.0.1 不再受信（$R）" ;;
  *)
    fail "★★★ 换配置之后给出了没预料到的结果：$R" ;;
esac

echo
if [ "$FAILS" -ne 0 ]; then
  echo "PROXYPROTO TESTS FAILED：$FAILS 条断言没过。" >&2
  echo "--- 实例 A 日志 ---" >&2
  tail -40 "$WORK/a.log" >&2 || true
  echo "--- 实例 B 日志 ---" >&2
  tail -20 "$WORK/b.log" >&2 || true
  exit 1
fi
echo "PROXYPROTO TESTS PASSED —— HTTP 面的 PROXY protocol「收」半边真的在跑（v1/v2 · 合并发送 · LOCAL 不覆盖 · 两条反向 · TLS 之前 · 换配置立刻生效）。"
