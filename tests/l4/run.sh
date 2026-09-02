#!/usr/bin/env bash
# L4 面端到端（**M2 批 A**）：`l4 { tcp … }` 真的由枢衡自己转发。
#
# ★ ★ 为什么它是**独立一格**而不是塞进 `tests/serve/run.sh`：那一格验的是
#   「七层路由决策被执行对了没有」，而这一格验的是**另一个入口** ——
#   自建监听器、没有 Host、没有路径、字节原样搬。⚠ 两者混在一起时，
#   一条 L4 断言红了，读日志的人会先去看路由。
#
# ★ **上游还是枢衡自己**（两个 `fulcrum serve` 的 HTTP 实例），理由与数据面那一格
#   逐字相同：零外部依赖，而且上游把自己的名字与收到的路径回显出来 ——
#   于是「透传有没有原样送过去」「轮询有没有真的轮」可以被**看见**，不是靠推断。
#
# 端口（★ 与其余场景全都错开，见 AGENTS.md 那张端口表）：
#   9300 L4 TCP 监听（被测）· 9301 上游 A · 9302 上游 B · 9303 被测实例的 HTTP 站点
#   9304 **永远没人在听**（「上游全挂」那条判据要它）
#   9305 L4 **UDP** 监听（被测）· 9306 UDP 上游 A · 9307 UDP 上游 B
#   9308 L4 **SNI 分流**监听 · 9309/9310/9311 三条规则各自的上游 · 9312 兜底上游
#   （★ 分流那一格用**另一个实例**：前面几格已经换过三代，而分流要一份干净的监听器）
#
# ★ UDP 上游是一个 **12 行的 python3 回声脚本**（镜像里有 python3，见 Dockerfile.build）。
#   ⚠ 为什么不像 TCP 那样「上游就是枢衡自己」：枢衡没有 UDP 回声这种东西，
#   而为了测试给产品加一个，正是本仓库反复拒绝的那类做法。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
L4_PORT=${L4_PORT:-9300}
UP_A=${UP_A:-9301}
UP_B=${UP_B:-9302}
HTTP_PORT=${HTTP_PORT:-9303}
DEAD_PORT=${DEAD_PORT:-9304}
UDP_PORT=${UDP_PORT:-9305}
UDP_A=${UDP_A:-9306}
UDP_B=${UDP_B:-9307}
SNI_PORT=${SNI_PORT:-9308}
SNI_A=${SNI_A:-9309}
SNI_B=${SNI_B:-9310}
SNI_C=${SNI_C:-9311}
SNI_D=${SNI_D:-9312}
ADMIN_SOCK="$WORK/admin.sock"
UPGRADE_SOCK="$WORK/upgrade.sock"

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

# ★ ★ **忽略 SIGPIPE**：[6/11] 会往一条**可能已经断掉**的连接上写字节，
#   而那正是它要测的东西。⚠ 默认行为下这一写会把脚本自己打死（退出码 141），
#   于是门是红的、但**红得看不出原因** —— 屏幕上没有 `L4 TESTS FAILED`，
#   只有一个信号码。实测过：反证 ② 第一次跑出来就是 141。
#   ⇒ 忽略它，让写失败变成一个**返回值**，由下面的判据说出人话。
trap '' PIPE

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

wait_gone() {
  local port=$1 tries=0
  while [ "$tries" -lt 100 ]; do
    if ! port_listening "$port"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

# ── [0/11] 基线：端口必须都还没被占 ──────────────────────────────────────────
#
# ★ 本仓库栽过一次：一个「基线探针」对着上一个场景遗留的进程报了绿。
echo "=== [0/11] 基线：端口未被占用 ==="
for p in "$L4_PORT" "$UP_A" "$UP_B" "$HTTP_PORT" "$DEAD_PORT" \
  "$SNI_PORT" "$SNI_A" "$SNI_B" "$SNI_C" "$SNI_D"; do
  if port_listening "$p"; then
    echo "L4 TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑，否则下面测的是别人的服务。" >&2
    exit 1
  fi
done
ok "$L4_PORT / $UP_A / $UP_B / $HTTP_PORT / $DEAD_PORT / $SNI_PORT+四个分流上游都是空的"
# ⚠ UDP 端口占没占用**探不出来**（UDP 没有连接，connect 不会失败）——
#   所以这里不装样子去探，而是让下面 bind 失败时**自己说清楚**。
#   ★ 一个探不准的基线判据比没有更糟：它会让人以为查过了。
command -v python3 > /dev/null 2>&1 || {
  echo "L4 TESTS FAILED: 镜像里没有 python3 —— UDP 那两格的回声上游要它（见 docker/Dockerfile.build）。" >&2
  exit 1
}

[ -x "$BIN" ] || {
  echo "L4 TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 配置 ────────────────────────────────────────────────────────────────────
cat > "$WORK/upa.Fulcrumfile" <<CONF
:$UP_A {
    respond 200 "up-a path={path}"
}
CONF
cat > "$WORK/upb.Fulcrumfile" <<CONF
:$UP_B {
    respond 200 "up-b path={path}"
}
CONF

# 被测实例：一个 HTTP 站点（证明进程活着）+ 一个 L4 监听器（本场景的主角）+ 管理面。
cat > "$WORK/l4.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

:$HTTP_PORT {
    respond 200 "l4-host alive"
}

l4 {
    tcp :$L4_PORT {
        proxy $HOST:$UP_A $HOST:$UP_B
    }
    udp :$UDP_PORT {
        proxy $HOST:$UDP_A
    }
}
CONF

# ── [1/11] 起服务 ────────────────────────────────────────────────────────────
echo "=== [1/11] 起两个上游与被测实例 ==="
start() {
  local name=$1 conf=$2
  shift 2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    "$@" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
# ★ UDP 回声上游：收到什么就带上自己的标签回什么。
#   标签是这一节全部判据的抓手 —— 它让「回包是谁发的」可以被**看见**。
cat > "$WORK/udp-echo.py" <<'PYEOF'
import socket
import sys

port = int(sys.argv[1])
tag = sys.argv[2].encode()
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", port))
while True:
    data, addr = s.recvfrom(65535)
    # ★ ★ **把来源端口也回显出去**：它是「枢衡有没有复用同一条会话」的**唯一**抓手。
    #   ⚠ 少了它，一个「每个数据报都新建一条会话」的实现在客户端看来完全正常 ——
    #   而那种实现会把 fd 烧光，且只在压力下才显形。
    s.sendto(b"%s:%d:%s\n" % (tag, addr[1], data), addr)
PYEOF
udp_echo() {
  python3 "$WORK/udp-echo.py" "$1" "$2" > "$WORK/udp-$2.log" 2>&1 &
  PIDS+=($!)
}
udp_echo "$UDP_A" up-udp-a
udp_echo "$UDP_B" up-udp-b

start upa "$WORK/upa.Fulcrumfile"
UPA_PID=${PIDS[-1]}
start upb "$WORK/upb.Fulcrumfile"
UPB_PID=${PIDS[-1]}
# ★ 被测实例用**共享的** upgrade sock：[6/11] 要靠它把监听 fd 交给下一代。
RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/l4.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/gen1.pid" \
  --upgrade-sock "$UPGRADE_SOCK" \
  > "$WORK/gen1.log" 2>&1 &
GEN1=$!
PIDS+=("$GEN1")

for p in "$UP_A" "$UP_B" "$HTTP_PORT" "$L4_PORT"; do
  wait_port "$p" || {
    echo "L4 TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "两个上游、HTTP 站点与 **L4 监听 $L4_PORT** 都起来了"

# ★ 反向自证：$DEAD_PORT 必须真的没人在听 —— [5/11] 的全部价值就在它是死的。
if port_listening "$DEAD_PORT"; then
  fail "$DEAD_PORT 上居然有人在听 —— [5/11] 那条判据会失去意义"
else
  ok "$DEAD_PORT 确认没人在听（[5/11] 要用它）"
fi

# ── [2/11] 透传与轮询 ────────────────────────────────────────────────────────
#
# ★ 判据不是「连得上」，是**上游收到的东西与客户端发的一模一样**：
#   路径回显出来了，说明字节是原样搬过去的，而不是被谁重组过。
echo "=== [2/11] 透传：字节原样送到上游，多个 proxy 轮询 ==="
BODIES=""
for _ in 1 2 3 4; do
  B=$(curl -sS --max-time 5 "http://$HOST:$L4_PORT/hello" 2>/dev/null || echo "curl-failed")
  BODIES="$BODIES$B|"
done
case "$BODIES" in
  *"path=/hello"*) ok "路径原样透传到上游（body 里回显的是 /hello）" ;;
  *) fail "透传后上游看到的路径不对：$BODIES" ;;
esac
HAS_A=0
HAS_B=0
case "$BODIES" in *up-a*) HAS_A=1 ;; esac
case "$BODIES" in *up-b*) HAS_B=1 ;; esac
if [ "$HAS_A" = 1 ] && [ "$HAS_B" = 1 ]; then
  ok "四次连接落在**两个**上游上（轮询真的在轮）"
else
  fail "轮询没在轮：四次全落在同一个上游上（$BODIES）"
fi

# ── [3/11] 管理面：上游换得了，监听器换不了 ──────────────────────────────────
#
# ★ ★ 这两条是**一对**：换上游不经过 bind，所以要放行；换监听器要重新 bind，
#   所以要 409。⚠ 只验后者的话，一个「什么都拒绝」的实现照样绿，
#   而那会让 `POST /load` 对任何带 L4 的配置整个不能用。
echo "=== [3/11] 管理面：上游换得了、L4 监听器换不了（409）==="
admin_post() {
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

cat > "$WORK/only-a.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

:$HTTP_PORT {
    respond 200 "l4-host alive"
}

l4 {
    tcp :$L4_PORT {
        proxy $HOST:$UP_A
    }
    udp :$UDP_PORT {
        proxy $HOST:$UDP_A
    }
}
CONF
"$BIN" compile "$WORK/only-a.Fulcrumfile" > "$WORK/only-a.json" 2>"$WORK/only-a.err" || {
  echo "L4 TESTS FAILED: compile only-a 失败：" >&2
  cat "$WORK/only-a.err" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/only-a.json")")
if [ "$CODE" = "200" ]; then
  ok "换上游（两个 → 一个）被接受：$CODE"
else
  fail "换上游应当是 200，实际 $CODE：$(cat "$WORK/admin.out")"
fi
ONLY_A=1
for _ in 1 2 3 4; do
  B=$(curl -sS --max-time 5 "http://$HOST:$L4_PORT/after-load" 2>/dev/null || echo "curl-failed")
  case "$B" in *up-a*) ;; *) ONLY_A=0 ;; esac
done
if [ "$ONLY_A" = 1 ]; then
  ok "换完之后四次连接**全部**落在 up-a（新连接按新配置走）"
else
  fail "换完之后还有连接落在旧上游上 —— L4 那条路没读当前运行时"
fi

# 监听端口变了 ⇒ 409，且旧的照常服务。
cat > "$WORK/moved.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

:$HTTP_PORT {
    respond 200 "l4-host alive"
}

l4 {
    tcp :$((L4_PORT + 40)) {
        proxy $HOST:$UP_A
    }
    udp :$UDP_PORT {
        proxy $HOST:$UDP_A
    }
}
CONF
"$BIN" compile "$WORK/moved.Fulcrumfile" > "$WORK/moved.json" 2>/dev/null || {
  echo "L4 TESTS FAILED: compile moved 失败" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/moved.json")")
if [ "$CODE" = "409" ]; then
  ok "换 L4 监听端口被拒（409）—— 端口在启动时绑定，要走 systemctl reload"
else
  fail "换 L4 监听端口应当是 409，实际 $CODE：$(cat "$WORK/admin.out")"
fi
B=$(curl -sS --max-time 5 "http://$HOST:$L4_PORT/still" 2>/dev/null || echo "curl-failed")
case "$B" in
  *up-a*) ok "被拒之后旧配置照常服务（一个字节都没换）" ;;
  *) fail "被拒之后 L4 那条路坏了：$B" ;;
esac

# 把两个上游load 回来，后面两节要用。
"$BIN" compile "$WORK/l4.Fulcrumfile" > "$WORK/both.json" 2>/dev/null
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/both.json")")
if [ "$CODE" = "200" ]; then
  ok "两个上游 load 回来了"
else
  fail "load 回两个上游失败：$CODE"
fi

# ── [4/11] 一个上游挂掉：客户端不该有感觉 ────────────────────────────────────
#
# ★ ★ 这一条是「L4 面能不能真用」的分界线。两个上游挂掉一个时：
#   · 只在**候选地址**之间回退的实现 ⇒ **每两条连接坏一条**（轮询照样轮到死的那个）
#   · 在**上游**之间回退的实现 ⇒ 客户端全程无感
#   ⚠ 建连之前换上游是安全的：一个字节都还没走。（HTTP 那边有意不这么做，
#     因为那里换上游会滑向重试语义——两边的边界写在 l4.rs 与 pick_index_by 上。）
echo "=== [4/11] 挂掉一个上游 —— 客户端应当全程无感 ==="
kill -INT "$UPB_PID" 2>/dev/null || true
wait_gone "$UP_B" || fail "up-b 没停下来，下面这条判据会失去意义"
OKS=0
for _ in 1 2 3 4 5 6; do
  B=$(curl -sS --max-time 5 "http://$HOST:$L4_PORT/one-down" 2>/dev/null || echo "curl-failed")
  case "$B" in *up-a*) OKS=$((OKS + 1)) ;; esac
done
if [ "$OKS" = 6 ]; then
  ok "up-b 挂了之后 6 次连接**全部**成功并落在 up-a（建连阶段换上游）"
else
  fail "up-b 挂了之后只有 $OKS/6 次成功 —— 轮询把连接送给了一个死上游"
fi

# ── [5/11] 上游全挂：干净地关掉，并且说出来 ──────────────────────────────────
#
# ★ L4 上没有状态码可回，能做的只有关掉连接。⚠ 但**必须留一行日志**：
#   现场表现是「连上就断」，日志里没有那一行的话，运维分不清是上游全挂
#   还是端口配错，甚至会怀疑防火墙。
echo "=== [5/11] 上游全挂：连接被干净关掉，且日志说得出原因 ==="
kill -INT "$UPA_PID" 2>/dev/null || true
wait_gone "$UP_A" || fail "up-a 没停下来"
if curl -sS --max-time 5 "http://$HOST:$L4_PORT/all-down" > "$WORK/alldown.out" 2>&1; then
  fail "上游全挂时这条连接居然拿到了响应：$(cat "$WORK/alldown.out")"
else
  ok "上游全挂 ⇒ 连接被关掉（curl 失败，不是挂住）"
fi
if grep -q "全都连不上" "$WORK/gen1.log"; then
  ok "日志里说清了原因（「N 个上游全都连不上」）"
else
  fail "日志里没有那一行 —— 现场只会看到「连上就断」"
fi
# ★ 反向那一半：HTTP 那一侧**不受影响**。⚠ 少了它，一个「L4 出问题就把进程搞死」
#   的实现在上面那条判据上表现完全相同。
CODE=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$HTTP_PORT/" 2>/dev/null || echo 000)
if [ "$CODE" = "200" ]; then
  ok "L4 全挂不影响同进程的 HTTP 站点（进程活得好好的）"
else
  fail "HTTP 站点也不行了（$CODE）—— L4 的问题不该外溢"
fi

# ── [6/11] ★ ★ socket 移交：换代时 L4 长连接不断 ─────────────────────────────
#
# ★ ★ ★ 这是本场景**最贵**的一条，也是 L4 面唯一真正难的地方。
#   自建监听器如果不参与 fd 移交，第一次 `systemctl reload` 会让这个端口重新 bind，
#   而那一刻**所有 L4 长连接一起断** —— 与此同时 HTTP 一切正常，
#   于是没有人会想到去看 L4。⇒ 判据钉三件事：
#     ① 新一代日志里出现「继承了监听 fd」（而不是「监听 … 已登记」）
#     ② 换代**期间建立的那条连接**还能继续说话
#     ③ 换代之后**新**连接照常（新一代真的在 accept）
echo "=== [6/11] socket 移交：换代时长连接不断 ==="
start upa2 "$WORK/upa.Fulcrumfile"
wait_port "$UP_A" || {
  echo "L4 TESTS FAILED: up-a 起不回来" >&2
  exit 1
}

# 在**换代之前**建一条连接，并在上面完成一次请求-响应。
exec 3<>"/dev/tcp/$HOST/$L4_PORT"
printf 'GET /before HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n' >&3

# 读一整个响应：状态行 + 头（到空行为止）+ 按 Content-Length 精确读 body。
# ⚠ 用 `read -N` 而不是 `head -c`：前者是 bash 内建、按字节读，
#   后者可能一次多读一块，把**下一个**响应的开头吃掉。
read_response() {
  local status line len=0 body
  IFS= read -r -t 5 status <&3 || return 1
  while IFS= read -r -t 5 line <&3; do
    line=${line%$'\r'}
    [ -z "$line" ] && break
    case "$line" in
      [Cc]ontent-[Ll]ength:*)
        len=${line#*:}
        len=${len// /}
        ;;
    esac
  done
  if [ "$len" -gt 0 ]; then
    # ★ 读 body 的唯一目的是**把这条响应从流里读干净**：不读完的话，
    #   下一次读会拿到上一条的尾巴，而现场表现是「换代之后响应乱了」。
    IFS= read -r -N "$len" -t 5 body <&3 || return 1
    [ -n "$body" ] || true
  fi
  printf '%s' "${status%$'\r'}"
}
BEFORE=$(read_response) || BEFORE="读不到响应"
case "$BEFORE" in
  *200*) ok "换代前：长连接上完成了一次请求（$BEFORE）" ;;
  *) fail "换代前那条长连接就不通：$BEFORE" ;;
esac

# 触发换代：SIGQUIT 给第一代，再以 -u 起第二代（与 M0 那一格同一个形状）。
kill -QUIT "$GEN1" 2>/dev/null || true
RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/l4.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/gen2.pid" \
  --upgrade-sock "$UPGRADE_SOCK" \
  -u \
  > "$WORK/gen2.log" 2>&1 &
GEN2=$!
PIDS+=("$GEN2")

# 等第二代真的接手（它会在日志里说自己继承了 fd）。
tries=0
while [ "$tries" -lt 100 ]; do
  if grep -q "继承了监听 fd" "$WORK/gen2.log" 2>/dev/null; then break; fi
  sleep 0.1
  tries=$((tries + 1))
done
if grep -q "继承了监听 fd" "$WORK/gen2.log" 2>/dev/null; then
  ok "第二代**继承**了 L4 的监听 fd（没有重新 bind）"
else
  fail "第二代没有继承监听 fd —— 它自己重新 bind 了，长连接会在下一次换代时全断"
  sed -n '1,40p' "$WORK/gen2.log" >&2
fi
# ★ 反向那一半：第二代**不该**打出「新 bind」那一行。
#   ⚠ 少了它，一个「先继承、再顺手 bind 一个」的实现照样能让上面那条绿。
if grep -q "已登记为 fulcrum-l4-tcp" "$WORK/gen2.log" 2>/dev/null; then
  fail "第二代还是自己 bind 了一个新的监听器（日志里有「已登记为」）"
else
  ok "第二代没有新 bind 任何 L4 监听器（继承的那一个就是全部）"
fi

# ② 换代之前建立的那条连接**还能继续说话**。
if printf 'GET /after HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n' >&3 2>/dev/null; then
  AFTER=$(read_response) || AFTER="读不到响应"
else
  # ★ 连写都写不进去 = 那条连接在换代那一刻就被断了，而这正是本节要抓的坏行为。
  AFTER="那条连接已经断了（写都写不进去）"
fi
case "$AFTER" in
  *200*) ok "换代之后**那条老连接**仍然通（$AFTER）—— fd 移交没有打断它" ;;
  *) fail "换代把已经建立的 L4 连接打断了：$AFTER" ;;
esac
exec 3<&-
exec 3>&-

# ③ 换代之后新连接照常。
B=$(curl -sS --max-time 5 "http://$HOST:$L4_PORT/new-conn" 2>/dev/null || echo "curl-failed")
case "$B" in
  *up-a*) ok "换代之后新连接也照常（第二代真的在 accept）" ;;
  *) fail "换代之后新连接不通：$B" ;;
esac

# ★ 老一代收到的是 SIGQUIT（优雅停机），它会**等完整的排空窗口**才走。
#   这里明确用 SIGINT 收掉它并等它真的没了 —— 否则 cleanup 里的 `kill -9`
#   会让 bash 在收尾时打一行 `Killed` 噪音，而那行读起来像是本场景出了事。
kill -INT "$GEN1" 2>/dev/null || true
waited=0
while kill -0 "$GEN1" 2>/dev/null && [ "$waited" -lt 50 ]; do
  sleep 0.1
  waited=$((waited + 1))
done

# ── [7/11] UDP 透传与会话 ────────────────────────────────────────────────────
#
# ★ 判据不是「有回包」，是**回包带着上游的标签**：那说明数据报真的走到了上游，
#   而不是被谁在中间应付掉。⚠ 第二个包走的是**另一条代码路径**（老会话，`touch`），
#   所以两个包要发在**同一个 socket** 上 —— 换个 socket 就换了源端口，那还是新会话。
#
# ⚠ ⚠ **客户端不能用 bash 的 `/dev/udp`**，这一条是本批实测撞出来的：
#   bash 的 `read` 内建对不可 seek 的 fd **一次读一个字节**，而在**数据报** socket 上
#   一次 `read()` 会**消费掉整个数据报**并只返回第一个字节 —— 于是它拿到 `u`，
#   然后一直等下一个数据报，最后超时。
#   ★ ★ 现场表现是「UDP 透传不通」，而**被测方完全是好的**（上游日志里那个包收到了）。
#   ⇒ 客户端换成 python（整包 `recvfrom`），它顺带让「回包的源地址」可以被**显式**断言。
echo "=== [7/11] UDP：数据报透传 + 同一客户端复用会话 ==="
cat > "$WORK/udp-send.py" <<'PYEOF'
"""在**同一个** socket 上依次发若干数据报，每个都等一次回包。

输出每行一条：`<源地址>\t<收到的内容>`；超时写 `-\tudp-timeout`。
★ 把源地址也打出来，是为了让「回包来自监听地址」成为一条**看得见**的判据 ——
  一个从会话 socket 直接回包的实现，在 connected 客户端上表现为「收不到」，
  而在这里表现为**源地址不对**，后者指得准得多。
"""
import socket
import sys

port = int(sys.argv[1])
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(3)
for payload in sys.argv[2:]:
    s.sendto(payload.encode(), ("127.0.0.1", port))
    try:
        data, src = s.recvfrom(65535)
        print("%s:%d\t%s" % (src[0], src[1], data.decode("utf-8", "replace").strip()))
    except OSError:
        print("-\tudp-timeout")
PYEOF

udp_send() {
  # $1 = 端口，其余 = 要依次发的载荷。每行一条 `<源地址><TAB><内容>`。
  local port=$1
  shift
  python3 "$WORK/udp-send.py" "$port" "$@"
}

R1=$(udp_send "$UDP_PORT" "ping-1")
BODY1=${R1#*$'\t'}
SRC1=${R1%%$'\t'*}
# 回包格式：`<上游标签>:<上游看到的源端口>:<原样内容>`
case "$BODY1" in
  up-udp-a:*:ping-1) ok "UDP 数据报原样送到上游并带回了回包（$BODY1）" ;;
  *) fail "UDP 透传不通：$R1" ;;
esac
# ★ ★ 回包**必须来自监听地址**。⚠ 从会话 socket 直接回包也能让上面那条过，
#   但真实客户端多半是 connected 的，内核会把那种回包**丢掉** ——
#   现场表现是「偶尔收不到响应」，而抓包才看得出源端口不对。
if [ "$SRC1" = "$HOST:$UDP_PORT" ]; then
  ok "回包来自**监听地址** $SRC1（不是会话 socket 的临时端口）"
else
  fail "回包源地址是 $SRC1，应当是 $HOST:$UDP_PORT —— connected 的客户端会把它丢掉"
fi

# 同一个 socket 连发两个：第二个走「老会话」那条路。
SESS=$(udp_send "$UDP_PORT" "sess-1" "sess-2")
S1=$(printf '%s\n' "$SESS" | sed -n '1p' | cut -f2)
S2=$(printf '%s\n' "$SESS" | sed -n '2p' | cut -f2)
if [ "${S1##*:}" = "sess-1" ] && [ "${S2##*:}" = "sess-2" ]; then
  ok "同一个客户端地址连发两个包都通（两个回包都对得上）"
else
  fail "两个包没都通：第一个「$S1」第二个「$S2」"
fi
# ★ ★ **会话真的被复用了吗** —— 判据是**上游看到的源端口两次相同**。
#   ⚠ 少了这一条，一个「每个数据报都新建会话」的实现在客户端看来完全正常，
#   而它会把 fd 烧光。★ 这就是那个回声脚本要把来源端口回显出来的全部理由。
P1=$(printf '%s' "$S1" | cut -d: -f2)
P2=$(printf '%s' "$S2" | cut -d: -f2)
if [ -n "$P1" ] && [ "$P1" = "$P2" ]; then
  ok "两个包在上游看来来自**同一个源端口** $P1（会话被复用了，不是每包新建）"
else
  fail "上游看到两个不同的源端口（$P1 / $P2）—— 每个数据报都新建了一条会话"
fi

# ── [8/11] ★ ★ UDP 的 socket 移交：换代时老一代必须**停止收包** ──────────────
#
# ★ ★ ★ 这一格比 TCP 那格更要紧，而且**只有 UDP 有**：两代进程持有的是**同一个**
#   UDP socket（fd 移交成功的表现），双方都在 `recv_from` 时，数据报会在两代之间
#   **被分流** —— 老一代会把本该属于新一代的会话首包吃掉，
#   而客户端只看到「换代那几秒有些请求没回应」，两边日志都正常。
#   （这正是 docs/verification/open-seams.md 里那条 M0 衍生风险的第一次具体落地。）
#
# ⚠ **判据必须能分辨「谁在收包」**，而不只是「还通不通」：老一代若仍在收，
#   它照样能正确转发（配置一样），客户端根本看不出来。
#   ⇒ 让第二代**指向另一个上游**（tag 不同），于是「回包带的是谁的 tag」
#     就直接回答了「这个包是哪一代处理的」。
echo "=== [8/11] UDP socket 移交：老一代停机后不许再抢数据报 ==="
cat > "$WORK/l4-v2.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}

:$HTTP_PORT {
    respond 200 "l4-host alive"
}

l4 {
    tcp :$L4_PORT {
        proxy $HOST:$UP_A
    }
    udp :$UDP_PORT {
        proxy $HOST:$UDP_B
    }
}
CONF

kill -QUIT "$GEN2" 2>/dev/null || true
RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/l4-v2.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/gen3.pid" \
  --upgrade-sock "$UPGRADE_SOCK" \
  -u \
  > "$WORK/gen3.log" 2>&1 &
GEN3=$!
PIDS+=("$GEN3")

tries=0
while [ "$tries" -lt 100 ]; do
  if grep -q "继承了 UDP 监听 fd" "$WORK/gen3.log" 2>/dev/null; then break; fi
  sleep 0.1
  tries=$((tries + 1))
done
if grep -q "继承了 UDP 监听 fd" "$WORK/gen3.log" 2>/dev/null; then
  ok "第三代**继承**了 UDP 的监听 fd（没有重新 bind）"
else
  fail "第三代没有继承 UDP 监听 fd —— 换代时这个端口会重新 bind"
  sed -n '1,40p' "$WORK/gen3.log" >&2
fi

# ★ 老一代必须**说出**自己停止收包了 —— 这是那条设计的可观测面。
# ⚠ 要**等一下**：SIGQUIT 是异步的，老一代处理它与新一代打出「继承了 fd」之间
#   没有先后保证。★ 不等的话，它会偶尔红在一条**其实成立**的判据上 ——
#   而一条会偶尔假红的判据，最后一定会被人当成噪音。
tries=0
while [ "$tries" -lt 100 ]; do
  if grep -q "停止收包" "$WORK/gen2.log" 2>/dev/null; then break; fi
  sleep 0.1
  tries=$((tries + 1))
done
if grep -q "停止收包" "$WORK/gen2.log" 2>/dev/null; then
  ok "老一代收到停机信号后**停止收包**（日志里说了）"
else
  fail "老一代没有说自己停止收包 —— 它可能还在与新一代抢同一个 socket"
fi

# ★ ★ 真正的判据：换代之后**每一个**回包都必须带新一代那个上游的 tag。
#   ⚠ 只要老一代还在 `recv_from`，就会有一部分包带回旧 tag —— 而它**不是偶发**：
#   两个进程轮流被内核唤醒，比例接近一半。
# ⚠ ⚠ **这是一条概率性判据，必须说清楚**：两代进程谁被内核唤醒是随机的。
#   ★ 实测（本批的反证 ①，故意不 break）：**6 个数据报里被老一代抢走 1 个**。
#   ⇒ 发 20 个，把「坏实现却全绿」压到 (5/6)^20 ≈ 3%；而 CI 每次推送都跑一遍，
#     连续漏检的概率可以忽略。⚠ 不写这段的话，下一个人会以为它是确定性的。
FROM_OLD=0
FROM_NEW=0
for i in $(seq 1 20); do
  R=$(udp_send "$UDP_PORT" "after-$i" | cut -f2)
  case "$R" in
    up-udp-b:*) FROM_NEW=$((FROM_NEW + 1)) ;;
    up-udp-a:*) FROM_OLD=$((FROM_OLD + 1)) ;;
    *) : ;;
  esac
done
if [ "$FROM_NEW" = 20 ]; then
  ok "换代后 20 个数据报**全部**由新一代处理（老一代一个都没抢走）"
else
  fail "换代后有 $FROM_OLD 个数据报被老一代抢走了（新一代只处理了 $FROM_NEW/20）—— 老一代没有停止 recv_from"
fi

# ── [9/11] ★ ★ SNI / ALPN 分流（批 C）────────────────────────────────────────
#
# ★ ★ 这一格与前面几格**用的是另一个实例**（自己的端口、自己的配置）：前面那些
#   已经换过三代，而分流要的是一份**干净的、带规则的**监听器。
#   ⚠ 混进去的话，[3/8] 那条「L4 监听器集变了就 409」会红在一个无关的原因上。
#
# ★ 判据的抓手是**上游回显它收到的字节**（tag + hex）：于是
#   「分流去了谁」与「字节有没有被改」这两件事都可以被**看见**，而不是只能推断。
echo "=== [9/11] SNI / ALPN 分流 ==="

cat > "$WORK/tcp-echo.py" <<'PYEOF'
"""收什么回什么：`<tag>:<收到字节的 hex>`。

用法：tcp-echo.py <端口> <tag> [eof]

★ 回 hex 而不是原文，是因为 ClientHello 是二进制 —— 而这一格最要紧的一条判据
  正是「**枢衡看过 ClientHello，但一个字节都没吃掉**」。

★ ★ ★ 第三个参数 `eof`：**读到发送方半关闭为止**，而不是「一次 recv 就算收完」。
  ⚠ ⚠ 它修的是一条**在量别的东西**的判据 —— 详见 `serve()` 里那段。
  ⛔ 不给它设成默认：curl 与 `hello.py` 那些客户端**不半关闭**，
  对它们「读到 EOF」等于每条连接白等一个超时。
"""
import socket
import sys
import threading

port = int(sys.argv[1])
tag = sys.argv[2]
# 读到发送方半关闭为止（见模块注释）。⚠ 逐字比较，不做前缀匹配。
until_eof = len(sys.argv) > 3 and sys.argv[3] == "eof"
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port))
srv.listen(16)


def serve(conn):
    # ⚠ 这个超时是**兜底**，不是判据：`eof` 模式的确定性来自半关闭，不来自它。
    #   留着它只为「万一 EOF 没透过来」时别把整格挂死。
    conn.settimeout(1.0)
    data = b""
    try:
        while len(data) < 4096:
            chunk = conn.recv(4096)
            if not chunk:
                break
            data += chunk
            # ★ ★ ★ **「一次 recv」量的是 TCP 分段边界，不是「收到了什么」。**
            #   实测（2026-09-02）：枢衡在 `l4.rs` 里分三次写给上游 ——
            #   ① 建连时写 PROXY 头 · ② 重放 peek 走的字节 · ③ `copy_bidirectional`
            #   搬其余流量。「只发不收」那条路上 ② 是空的，于是 ① 与 ③ 之间隔着
            #   一个客户端往返 ⇒ **落不落进同一个段是赛跑**。
            #   ⇒ 现场表现：`只发不收那条路上载荷丢了`，而载荷根本没丢，
            #     它在下一段里；同一棵树重跑就绿 —— 一条会随机红的判据，
            #     会把真正的回归伪装成「又是那条老 flaky」。
            #   ★ `eof` 模式改成**读到发送方半关闭为止**：由发送方说「我说完了」，
            #     与分段无关。半关闭能透过来是 `copy_bidirectional` 的性质，
            #     `l4.rs` 那段注释正是靠它（「一端读到 EOF 就把另一端 shutdown 写方向」）。
            if not until_eof:
                break
    except OSError:
        pass
    try:
        conn.sendall(("%s:%s\n" % (tag, data.hex())).encode())
    except OSError:
        pass
    conn.close()


while True:
    c, _ = srv.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
PYEOF

cat > "$WORK/hello.py" <<'PYEOF'
"""造一个**最小但合法**的 TLS ClientHello 并发出去，然后读一行回应。

用法：hello.py <端口> [sni] [alpn1,alpn2...] [nosigalgs]
  · sni 写 `-` 表示**不带** SNI 扩展
  · 第三个参数省略/留空表示不带 ALPN 扩展
  · 第四个参数写 `nosigalgs` 则**故意不带** signature_algorithms

⚠ ⚠ **`signature_algorithms` 是一条实测出来的边界**：夹具不带它的话，
  于是 rustls 直接 `SignatureAlgorithmsExtensionRequired` 拒绝 —— 而现场表现是
  「SNI 分流一条都不命中、全部走兜底」，看起来像匹配写错了。真实客户端都会带它。
输出：`<回应行>\\t<我们发出去的字节的 hex>`

★ ★ 为什么手工造而不用 python 的 ssl 库：ssl 库造不出「不带 SNI」与
  「根本不是 TLS」这两种输入，而它们恰恰是这一格要验的两条兜底路径。
⚠ 这是**测试夹具**里的手写 TLS 字节，不是产品代码 —— 产品那边看 ClientHello
  用的是 rustls 的 `Acceptor`（安全基线：不手搓解析攻击者控制的二进制）。
"""
import os
import socket
import struct
import sys

port = int(sys.argv[1])
sni = sys.argv[2] if len(sys.argv) > 2 else "-"
alpns = sys.argv[3].split(",") if len(sys.argv) > 3 and sys.argv[3] else []
with_sigalgs = not (len(sys.argv) > 4 and sys.argv[4] == "nosigalgs")

exts = b""
if sni != "-":
    host = sni.encode()
    entry = b"\x00" + struct.pack("!H", len(host)) + host  # name_type=host_name + 名字
    data = struct.pack("!H", len(entry)) + entry  # server_name_list
    exts += struct.pack("!HH", 0x0000, len(data)) + data
if alpns:
    protos = b"".join(bytes([len(p)]) + p.encode() for p in alpns)
    data = struct.pack("!H", len(protos)) + protos
    exts += struct.pack("!HH", 0x0010, len(data)) + data
# supported_versions：只留 TLS 1.3
sv = b"\x02\x03\x04"
exts += struct.pack("!HH", 0x002B, len(sv)) + sv
# supported_groups：x25519
sg = b"\x00\x02\x00\x1d"
exts += struct.pack("!HH", 0x000A, len(sg)) + sg
if with_sigalgs:
    # signature_algorithms：ecdsa_secp256r1_sha256 / rsa_pss_rsae_sha256 / rsa_pkcs1_sha256
    # ⚠ rustls **要求**这一条，少了它整个 ClientHello 会被拒（TLS 1.3 强制）。
    sa = b"\x00\x06\x04\x03\x08\x04\x04\x01"
    exts += struct.pack("!HH", 0x000D, len(sa)) + sa

body = b"\x03\x03" + os.urandom(32) + b"\x00"  # legacy_version + random + 空 session_id
ciphers = b"\x13\x01"  # TLS_AES_128_GCM_SHA256
body += struct.pack("!H", len(ciphers)) + ciphers
body += b"\x01\x00"  # compression_methods = [null]
body += struct.pack("!H", len(exts)) + exts
hs = b"\x01" + struct.pack("!I", len(body))[1:] + body  # handshake(client_hello) + 24 位长度
rec = b"\x16\x03\x01" + struct.pack("!H", len(hs)) + hs  # TLS record

s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(5)
s.connect(("127.0.0.1", port))
s.sendall(rec)
try:
    line = s.recv(65535).decode("utf-8", "replace").strip()
except OSError as e:
    line = "recv-failed:%s" % type(e).__name__
print("%s\t%s" % (line, rec.hex()))
PYEOF

cat > "$WORK/plain.py" <<'PYEOF'
"""发一段**根本不是 TLS** 的字节，验兜底那条路。"""
import socket
import sys

port = int(sys.argv[1])
payload = b"GET / HTTP/1.0\r\n\r\n"
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(5)
s.connect(("127.0.0.1", port))
s.sendall(payload)
try:
    line = s.recv(65535).decode("utf-8", "replace").strip()
except OSError as e:
    line = "recv-failed:%s" % type(e).__name__
print("%s\t%s" % (line, payload.hex()))
PYEOF

tcp_echo() {
  python3 "$WORK/tcp-echo.py" "$1" "$2" > "$WORK/tcp-$2.log" 2>&1 &
  PIDS+=($!)
}
tcp_echo "$SNI_A" up-sni-a
tcp_echo "$SNI_B" up-wild-b
tcp_echo "$SNI_C" up-alpn-c
tcp_echo "$SNI_D" up-default

cat > "$WORK/sni.Fulcrumfile" <<CONF
:$((SNI_PORT + 100)) {
    respond 200 "sni-host alive"
}

l4 {
    tcp :$SNI_PORT {
        sni api.example.com {
            proxy $HOST:$SNI_A
        }
        sni *.internal.example.com {
            proxy $HOST:$SNI_B
        }
        alpn h2 {
            proxy $HOST:$SNI_C
        }
        proxy $HOST:$SNI_D
    }
}
CONF
RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/sni.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/sni.pid" \
  --upgrade-sock "$WORK/sni.sock" \
  > "$WORK/sni.log" 2>&1 &
PIDS+=($!)
for p in "$SNI_A" "$SNI_B" "$SNI_C" "$SNI_D" "$SNI_PORT"; do
  wait_port "$p" || {
    echo "L4 TESTS FAILED: 分流那一格的端口 $p 起不来。日志：" >&2
    tail -30 "$WORK/sni.log" >&2
    exit 1
  }
done
ok "分流实例与四个上游都起来了（$SNI_PORT → a/b/c/兜底）"

hello_to() { python3 "$WORK/hello.py" "$SNI_PORT" "$@"; }

# ① 精确 SNI
OUT=$(hello_to api.example.com)
GOT=${OUT%%$'\t'*}
SENT=${OUT##*$'\t'}
case "$GOT" in
  up-sni-a:*) ok "SNI=api.example.com → 精确规则那个上游" ;;
  *) fail "精确 SNI 分流不对：$GOT" ;;
esac
# ★ ★ **ClientHello 必须原样到达上游**：我们看过它，但一个字节都不许吃掉。
#   ⚠ 少了这条判据，一个「读走 ClientHello 就不管了」的实现会让上面那条照常绿，
#   而真实客户端会卡在握手上 —— 现场表现是「TLS 偶尔连不上」。
if [ "${GOT#up-sni-a:}" = "$SENT" ]; then
  ok "上游收到的字节与客户端发出的**逐字节相同**（ClientHello 被原样重放）"
else
  fail "ClientHello 没有被原样送到上游：发出 ${#SENT} 个 hex 字符，上游收到 ${#GOT}"
fi

# ② 通配一层
OUT=$(hello_to x.internal.example.com)
case "${OUT%%$'\t'*}" in
  up-wild-b:*) ok "SNI=x.internal.example.com → 通配规则那个上游（只吃一层）" ;;
  *) fail "通配 SNI 分流不对：${OUT%%$'\t'*}" ;;
esac

# ③ 两层子域**不该**命中通配 —— 这是 G66 那条语义的判据
OUT=$(hello_to a.b.internal.example.com)
case "${OUT%%$'\t'*}" in
  up-default:*) ok "SNI=a.b.internal.example.com **不命中**通配（G66：只吃一层）→ 兜底" ;;
  *) fail "两层子域不该命中通配，却去了：${OUT%%$'\t'*}" ;;
esac

# ④ 不带 SNI、只带 ALPN
OUT=$(hello_to - h2)
case "${OUT%%$'\t'*}" in
  up-alpn-c:*) ok "无 SNI + ALPN=h2 → ALPN 规则那个上游" ;;
  *) fail "ALPN 分流不对：${OUT%%$'\t'*}" ;;
esac

# ⑤ ALPN 是逐字节相等：h2c 不该命中 h2
OUT=$(hello_to - h2c)
case "${OUT%%$'\t'*}" in
  up-default:*) ok "ALPN=h2c **不命中** h2（逐字节相等，不是前缀）→ 兜底" ;;
  *) fail "h2c 不该命中 h2 规则，却去了：${OUT%%$'\t'*}" ;;
esac

# ⑥ ★ ★ ★ **一个「完整 TLS 栈会当场拒掉」的 ClientHello，照样按它的 SNI 分流。**
#
#   ⚠ ⚠ **（G104 第 ② 处）：这条判据换了期望值，而红的不是新写的门。**
#   旧契约（M2 批 C，原文保留）：
#     > ★ ★ **rustls 不认的 ClientHello 也走兜底，而不是被关掉**。
#     > 这条判据是本批实测撞出来的：缺 `signature_algorithms` 时 rustls 直接拒
#     > （`SignatureAlgorithmsExtensionRequired`）—— 也就是说，**「能不能分流」取决于
#     > rustls 认不认这个 hello，而那比「能读出 SNI」严格**。
#     > ⇒ 取舍写在明处：这种连接**交给兜底上游**，让真正的对端去决定怎么答复；
#     >   枢衡不替上游拒绝一个它自己也许能处理的客户端。
#
#   ★ 预读换成 BoringSSL 的早回调之后，那句「取舍」**兑现得比当初更彻底**：
#   早回调发生在几乎所有 ClientHello 校验**之前**，所以这里读得出 SNI 就按 SNI 分流 ——
#   **「能不能分流」终于只取决于「能不能读出 SNI」**，而不再取决于某个 TLS 栈的严格程度。
#   ⇒ 期望值从「兜底」换成「精确规则那个上游」。
#   ⚠ ⚠ **这不是判据变松**：变松的写法是「去了哪儿都行，别被关掉就成」，
#     而这里钉死的是一个**更窄**的结论。旧契约保留在上面，换的理由就在这几行。
#
#   ⚠ 顺带说清一件事：我们**没有**校验这个 hello 就把它转走了 ——
#   所以「字节原样重放」在这条路上不是锦上添花，而是这个取舍成立的**前提**：
#   上游必须拿到一份未经我们改动的 hello，才谈得上「让它自己决定」。
OUT=$(hello_to api.example.com "" nosigalgs)
GOT=${OUT%%$'\t'*}
SENT=${OUT##*$'\t'}
case "$GOT" in
  up-sni-a:*) ok "完整 TLS 栈会拒掉的 hello（缺 signature_algorithms）照样按 SNI 分流" ;;
  *) fail "缺 signature_algorithms 的 hello 去了：$GOT" ;;
esac
if [ "${GOT#up-sni-a:}" = "$SENT" ]; then
  ok "★ 这条没被我们校验过的 hello，**逐字节**原样交给了上游去决定"
else
  fail "没校验就转走的 hello 还被改了：发出 ${#SENT} 个 hex 字符，上游收到 ${#GOT}"
fi

# ⑦ 根本不是 TLS ⇒ 兜底，且**读走的字节要原样重放**
OUT=$(python3 "$WORK/plain.py" "$SNI_PORT")
GOT=${OUT%%$'\t'*}
SENT=${OUT##*$'\t'}
case "$GOT" in
  up-default:*) ok "不是 TLS 的流量 → 兜底（没有被当成错误关掉）" ;;
  *) fail "非 TLS 流量没走兜底：$GOT" ;;
esac
if [ "${GOT#up-default:}" = "$SENT" ]; then
  ok "非 TLS 那条路上，**读走的字节也被原样重放**（开头没被吃掉）"
else
  fail "非 TLS 流量的开头字节被吃掉了：期望 $SENT，上游收到 ${GOT#up-default:}"
fi

# ── [10/11] ★ ★ ★ PROXY protocol：收与发（批 D）──────────────────────────────
#
# ★ ★ 又是**另一个实例**（自己的端口、自己的配置），理由与分流那一格逐字相同。
#
# ★ ★ ★ **这一格最要紧的那条判据，验的是「什么都不做」**：
#   来源不在信任清单里时，枢衡**一个字节都不许读** —— 那 12/16 字节要原样流给上游。
#   ⚠ 它与「读掉丢弃」在功能上都「能用」，而两者给上游的是**完全不同的两条流**。
#   ⇒ 判据只能落在**上游收到的字节**上，落在别处（日志、状态码）都分不出这两种。
#
# 四个监听器，四种组合 —— 因为**收与发是两件独立的事，不是一个开关的两半**：
#   9313 收 + 发（链式）· 9315 只收 · 9316 不信任（清单里没有 127/8）· 9317 只发
echo "=== [10/11] PROXY protocol：收与发 ==="

PP_UP=${PP_UP:-9314}
PP_CHAIN=${PP_CHAIN:-9313}
PP_RECV=${PP_RECV:-9315}
PP_UNTRUSTED=${PP_UNTRUSTED:-9316}
PP_SEND=${PP_SEND:-9317}

for p in "$PP_UP" "$PP_CHAIN" "$PP_RECV" "$PP_UNTRUSTED" "$PP_SEND"; do
  if port_listening "$p"; then
    fail "$p 已经被占用 —— 这一格的判据会失去意义"
  fi
done

# 上游：复用分流那一格的回显脚本（收什么回什么，回的是 hex）。
#
# ★ ★ ★ **这一格是唯一开 `eof` 的**：它的客户端是 `ppclient.py`（发完就半关闭），
#   而这一格的判据要断「头**之后**跟着载荷」—— 那是两次独立的 write，
#   「一次 recv」判不动它。⇒ 其余几格的上游仍走默认（curl / `hello.py` 不半关闭）。
python3 "$WORK/tcp-echo.py" "$PP_UP" up-pp eof >"$WORK/up-pp.log" 2>&1 &
PIDS+=($!)

cat > "$WORK/pp.Fulcrumfile" <<PPCONF
{
    admin unix/$WORK/pp-admin.sock
}

:$((PP_CHAIN + 100)) {
    respond 200 "pp alive"
}

l4 {
    # [A] 收 + 发：信任本机，转发时**重新造一个 v1 头** —— 链式传递。
    tcp :$PP_CHAIN {
        proxy_protocol_from 127.0.0.0/8
        proxy_protocol v1
        proxy 127.0.0.1:$PP_UP
    }
    # [B] 只收不发：头被吃掉，上游只看到应用数据。
    tcp :$PP_RECV {
        proxy_protocol_from 127.0.0.0/8
        proxy 127.0.0.1:$PP_UP
    }
    # [C] **不信任**本机（清单里只有 10/8）⇒ 一个字节都不读。
    tcp :$PP_UNTRUSTED {
        proxy_protocol_from 10.0.0.0/8
        proxy 127.0.0.1:$PP_UP
    }
    # [D] 只发不收：枢衡是第一跳，报的是 socket 对端。
    tcp :$PP_SEND {
        proxy_protocol v2
        proxy 127.0.0.1:$PP_UP
    }
}
PPCONF

"$BIN" serve "$WORK/pp.Fulcrumfile" --bind-host "$HOST" \
  --pid-file "$WORK/pp.pid" --upgrade-sock "$WORK/pp-upgrade.sock" \
  --state-dir "$WORK/pp-state" >"$WORK/pp.log" 2>&1 &
PIDS+=($!)
wait_port "$PP_CHAIN" || fail "PROXY protocol 实例没起来"
wait_port "$PP_UP" || fail "回显上游没起来"

# 客户端夹具：精确控制**发出去的第一批字节**。
#
# ★ 有意让它接受一段 hex 而不是「造一个头给我」：这一格要发的东西里有
#   **坏头**与 **LOCAL 头**，而一个「会造合法头」的夹具造不出它们。
cat > "$WORK/ppclient.py" <<'PYEOF'
"""发 `<头的 hex>` + `<载荷>`，读一行回应。

用法：ppclient.py <端口> <头的hex，'none' 表示不发头> <载荷>
输出：`<回应行>`
"""
import socket
import sys

port = int(sys.argv[1])
head_hex = sys.argv[2]
payload = sys.argv[3].encode()

head = b"" if head_hex == "none" else bytes.fromhex(head_hex)
s = socket.create_connection(("127.0.0.1", port), timeout=3)
try:
    s.sendall(head + payload)
    # ★ ★ ★ **半关闭 = 「我说完了」**，上游那侧靠它知道该停止读了。
    #   ⚠ 少了它，上游只能靠「一次 recv」或超时来断，而**前者量的是 TCP 分段边界**
    #     （见 tcp-echo.py 里那段）。
    #   ⚠ 坏头 / 信任来源不发头那两条路上枢衡会**直接关连接** ⇒ 这里可能抛，
    #     那不是错误，是那两条判据要的结果 —— 所以单独吞掉，让下面照常去读回应。
    try:
        s.shutdown(socket.SHUT_WR)
    except OSError:
        pass
    s.settimeout(3)
    data = b""
    while b"\n" not in data:
        chunk = s.recv(4096)
        if not chunk:
            break
        data += chunk
    sys.stdout.write(data.decode(errors="replace").strip())
except OSError as e:
    # ★ 连接被枢衡关掉时走这里 —— 这是**坏头那条判据要的结果**，不是脚本出错。
    sys.stdout.write("CLOSED:%s" % e.__class__.__name__)
finally:
    s.close()
PYEOF

# 一个 v1 头：客户端自称 203.0.113.7:40000。
PP_V1_HEX=$(printf 'PROXY TCP4 203.0.113.7 10.0.0.9 40000 443\r\n' | od -An -tx1 | tr -d ' \n')
PAYLOAD='hello-l4'
PAYLOAD_HEX=$(printf '%s' "$PAYLOAD" | od -An -tx1 | tr -d ' \n')

# ① ★ ★ ★ **不在信任清单里 ⇒ 一个字节都不读。**
#
#   上游收到的必须是 **头 + 载荷**，逐字节相同。
#   ⚠ 这一条红了而其余全绿，说明实现取的是「读掉丢弃」那条路 ——
#     两者在功能上都「能用」，而这条判据是唯一能分开它们的东西。
GOT=$(python3 "$WORK/ppclient.py" "$PP_UNTRUSTED" "$PP_V1_HEX" "$PAYLOAD")
if [ "${GOT#up-pp:}" = "${PP_V1_HEX}${PAYLOAD_HEX}" ]; then
  ok "★ 不在信任清单里：PROXY 头**一个字节都没被读走**，原样流给了上游"
else
  fail "不信任的来源，头没有被原样透传：期望 ${PP_V1_HEX}${PAYLOAD_HEX}，上游收到 ${GOT#up-pp:}"
fi

# ② 在清单里、只收不发 ⇒ 头被吃掉，上游只看到载荷。
GOT=$(python3 "$WORK/ppclient.py" "$PP_RECV" "$PP_V1_HEX" "$PAYLOAD")
if [ "${GOT#up-pp:}" = "$PAYLOAD_HEX" ]; then
  ok "在信任清单里：头被吃掉，上游只收到应用数据"
else
  fail "只收不发那条路上，上游收到的不是纯载荷：${GOT#up-pp:}（期望 $PAYLOAD_HEX）"
fi

# ③ ★ ★ **链式传递**：收了一个头，发出去的那个头里必须还是**最初那个客户端**。
#
#   ⚠ 这一条同时证明了「收对了」与「发对了」—— 而且它是**唯一**能证明
#     「收到的地址真的被用上了」的判据：L4 是透传，枢衡不会把它看到的 IP 说给任何人听。
GOT=$(python3 "$WORK/ppclient.py" "$PP_CHAIN" "$PP_V1_HEX" "$PAYLOAD")
GOT_HEX=${GOT#up-pp:}
# ★ hex → 文本用 python，**有意不用 `sed` + `printf`**：后者要写反斜杠，
#   而 `AGENTS.md` 那节第 1 条记着「heredoc 会吃掉一层反斜杠」——
#   ⚠ 本批在写这一格时又踩了一次，而它的表现是给出一份**看起来很像真结论**的空串。
GOT_TEXT=$(python3 -c 'import sys;sys.stdout.write(bytes.fromhex(sys.argv[1]).decode("latin-1"))' "$GOT_HEX")
case "$GOT_TEXT" in
  "PROXY TCP4 203.0.113.7 "*)
    ok "★ ★ 链式传递：发给上游的头里是**最初那个客户端** 203.0.113.7"
    ;;
  *)
    fail "链式传递没生效，上游收到的头是：$GOT_TEXT"
    ;;
esac
case "$GOT_TEXT" in
  *"$PAYLOAD") ok "链式那条路上，载荷跟在新头后面，一个字节没丢" ;;
  *) fail "链式那条路上载荷丢了：$GOT_TEXT" ;;
esac

# ④ 只发不收：枢衡是第一跳 ⇒ 报的是 socket 对端（127.0.0.1），且写的是 **v2**。
GOT=$(python3 "$WORK/ppclient.py" "$PP_SEND" none "$PAYLOAD")
GOT_HEX=${GOT#up-pp:}
case "$GOT_HEX" in
  0d0a0d0a000d0a515549540a*)
    ok "只发不收：上游收到的是一个 **v2** 头（12 字节签名对上了）"
    ;;
  *)
    fail "只发不收那条路上，上游收到的不是 v2 头：$GOT_HEX"
    ;;
esac
case "$GOT_HEX" in
  *"$PAYLOAD_HEX") ok "只发不收那条路上，载荷跟在头后面" ;;
  *) fail "只发不收那条路上载荷丢了：$GOT_HEX" ;;
esac

# ⑤ ★ **`LOCAL` 不是坏头**：上游 LB 的健康检查就长这样。
#
#   ⚠ 把它判成坏头的实现，现场表现是**每一次健康检查都断连** ——
#     而那时业务流量看起来完全正常。
PP_LOCAL_HEX="0d0a0d0a000d0a515549540a20000000"
GOT=$(python3 "$WORK/ppclient.py" "$PP_RECV" "$PP_LOCAL_HEX" "$PAYLOAD")
if [ "${GOT#up-pp:}" = "$PAYLOAD_HEX" ]; then
  ok '★ v2 的 LOCAL 头被正常吃掉，连接没有被断（健康检查那条路）'
else
  fail "LOCAL 头没被正确处理：$GOT"
fi

# ⑥ `PROXY UNKNOWN` 同理（v1 的那一半）。
PP_UNKNOWN_HEX=$(printf 'PROXY UNKNOWN\r\n' | od -An -tx1 | tr -d ' \n')
GOT=$(python3 "$WORK/ppclient.py" "$PP_RECV" "$PP_UNKNOWN_HEX" "$PAYLOAD")
if [ "${GOT#up-pp:}" = "$PAYLOAD_HEX" ]; then
  ok "v1 的 \`PROXY UNKNOWN\` 也被正常吃掉，连接没有被断"
else
  fail "PROXY UNKNOWN 没被正确处理：$GOT"
fi

# ⑦ ★ **在清单里却发来坏头 ⇒ 关连接**（与「不在清单里」有意相反）。
#
#   理由：此时我们**已经吃掉了一部分字节、还原不回去**，把残缺的流转给上游
#   只会把问题推到一个更难查的地方。
PP_BAD_HEX=$(printf 'PROXY TCP4 not-an-ip 1.2.3.4 1 2\r\n' | od -An -tx1 | tr -d ' \n')
GOT=$(python3 "$WORK/ppclient.py" "$PP_RECV" "$PP_BAD_HEX" "$PAYLOAD")
case "$GOT" in
  up-pp:*)
    fail "信任来源发来的坏头被放行了，上游收到：$GOT"
    ;;
  *)
    ok "★ 信任来源发来的**坏头** ⇒ 连接被关掉（上游一个字节都没收到）"
    ;;
esac

# ⑧ ★ ★ **不发头时，那条路上一个字节都不多加**（发那侧的反向判据）。
#
#   ⚠ 少了它，一个「无条件都发一个头」的实现在前面每一条上都是绿的。
#   ⚠ ⚠ 用的是**不信任**那个监听器而不是 $PP_RECV —— 理由见下面 ⑨：
#     $PP_RECV 信任本机，而信任的来源**必须**发头。
GOT=$(python3 "$WORK/ppclient.py" "$PP_UNTRUSTED" none "$PAYLOAD")
if [ "${GOT#up-pp:}" = "$PAYLOAD_HEX" ]; then
  ok "没配 proxy_protocol 的监听器**不给上游多发任何字节**"
else
  fail "没配发送却多发了东西：${GOT#up-pp:}（期望 $PAYLOAD_HEX）"
fi

# ⑨ ★ ★ ★ **在信任清单里却不发头 ⇒ 连接被关掉。**
#
#   ⚠ ⚠ **这是一条会让人意外的产品行为，所以它必须有判据，而不是只写在文档里。**
#   它是本批写这一格时**被一条写错的判据撞出来的**：⑧ 最初用的是 $PP_RECV，
#   而它红在「上游一个字节都没收到」上 —— 查下去发现那不是缺陷，是语义。
#
#   ★ 取「必须发」而不是「发了就用、没发就算了」的理由：
#     `proxy_protocol_from 10.0.0.0/8` 这句话的意思是**「这个网段的流量是经代理来的」**。
#     若允许清单内的来源选择性地不发头，那它就能让枢衡改用 socket 对端 ——
#     而那个地址正是 LB 自己，于是一条 `remote_ip 10.0.0.0/8` 规则会**命中它**。
#     ⇒ 「可选」把一个显式的信任声明变成了一个可以被对端单方面关掉的开关。
#   ⚠ 代价写在明处：清单配宽了（比如整个 10/8），那个网段里**所有不发头的正常连接
#     都会被拒**。这与 HAProxy 的 `accept-proxy`、nginx 的 `listen … proxy_protocol`
#     是同一条语义，只是我们多了一层来源限定。
GOT=$(python3 "$WORK/ppclient.py" "$PP_RECV" none "$PAYLOAD")
case "$GOT" in
  up-pp:*)
    fail "信任来源没发 PROXY 头却被放行了，上游收到：$GOT"
    ;;
  *)
    ok "★ ★ 在信任清单里却**不发头** ⇒ 连接被关掉（不是悄悄退回用 socket 对端）"
    ;;
esac

# ── 判定 ────────────────────────────────────────────────────────────────────
#
# ⚠ **成功那一支有意不写 `exit 0`**：整脚本每条路都显式 exit 时，shellcheck 0.10
#   会把 `trap cleanup EXIT` 认成够不到的代码，于是 cleanup 整个函数被报 SC2317 ——
#   ★ 而那是一条**关于本脚本自己**的假警报：cleanup 每次都真的跑了。
#   实测过（本批当场撞到）：把末尾的 `exit 0` 去掉，同一份脚本就干净了。
echo
if [ "$FAILS" -ne 0 ]; then
  echo "L4 TESTS FAILED —— $FAILS 条断言没过" >&2
  echo "── 被测实例（第一代）日志 ──" >&2
  cat "$WORK/gen1.log" >&2 2>/dev/null || true
  if [ -f "$WORK/gen2.log" ]; then
    echo "── 第二代日志 ──" >&2
    cat "$WORK/gen2.log" >&2
  fi
  exit 1
fi
cat <<'EOF'
L4 TESTS PASSED —— `l4 { tcp … }` 与 `l4 { udp … }` 都由枢衡自己转发：
  · TCP：字节原样透传、多个 `proxy` 轮询
  · `POST /load` 换得了上游、换不了监听器（409），被拒之后旧配置一个字节没动
  · 挂掉一个上游时客户端全程无感（建连阶段换上游）；全挂时干净关闭并说明原因
  · ★ TCP 换代时**继承**监听 fd，老连接不断、新连接照常
  · UDP：数据报带 tag 原样往返、同一客户端复用会话
  · ★ ★ UDP 换代时继承 fd，且**老一代停止收包** —— 换代后每一个数据报都由新一代处理
  · SNI / ALPN 分流：精确 / 通配只吃一层 / 两层不命中 / ALPN 逐字节 / 非 TLS 走兜底
  · ★ ★ 两条路上的**字节都被原样重放**给上游（看过 ClientHello，但一个字节都没吃掉）
  · ★ ★ ★ PROXY protocol：**不在信任清单里就一个字节都不读**（头原样流给上游）；
    在清单里则吃掉头；★ 链式传递（发出去的头里是最初那个客户端）；只发不收写 v2；
    `LOCAL` / `PROXY UNKNOWN` 不断连；信任来源的**坏头**关连接；没配就不多发一个字节
EOF
