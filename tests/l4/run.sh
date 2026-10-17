#!/usr/bin/env bash
# L4 面端到端（**M2 批 A**）：`l4 { tcp … }` 真的由樞衡自己轉發。
#
# ★ ★ 為什麼它是**獨立一格**而不是塞進 `tests/serve/run.sh`：那一格驗的是
#   「七層路由決策被執行對了沒有」，而這一格驗的是**另一個入口** ——
#   自建監聽器、沒有 Host、沒有路徑、位元組原樣搬。⚠ 兩者混在一起時，
#   一條 L4 斷言紅了，讀紀錄的人會先去看路由。
#
# ★ **上游還是樞衡自己**（兩個 `fulcrum serve` 的 HTTP 執行個體），理由與資料面那一格
#   逐字相同：零外部依賴，而且上游把自己的名字與收到的路徑回顯出來 ——
#   於是「透傳有沒有原樣送過去」「輪詢有沒有真的輪」可以被**看見**，不是靠推斷。
#
# 連接埠（★ 與其餘場景全都錯開，見 AGENTS.md 那張連接埠表）：
#   9300 L4 TCP 監聽（被測）· 9301 上游 A · 9302 上游 B · 9303 被測執行個體的 HTTP 站台
#   9304 **永遠沒人在聽**（「上游全掛」那條判據要它）
#   9305 L4 **UDP** 監聽（被測）· 9306 UDP 上游 A · 9307 UDP 上游 B
#   9308 L4 **SNI 分流**監聽 · 9309/9310/9311 三條規則各自的上游 · 9312 備援上游
#   （★ 分流那一格用**另一個執行個體**：前面幾格已經換過三代，而分流要一份乾淨的監聽器）
#
# ★ UDP 上游是一個 **12 行的 python3 回聲指令碼**（鏡像裡有 python3，見 Dockerfile.build）。
#   ⚠ 為什麼不像 TCP 那樣「上游就是樞衡自己」：樞衡沒有 UDP 回聲這種東西，
#   而為了測試給產品加一個，正是本儲存庫反覆拒絕的那類做法。
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
  # ★ SIGINT 而不是 SIGTERM：Pingora 把 SIGTERM 當優雅停機，會等完整排空視窗。
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

# ★ ★ **忽略 SIGPIPE**：[6/11] 會往一條**可能已經斷掉**的連接上寫位元組，
#   而那正是它要測的東西。⚠ 預設行為下這一寫會把指令碼自己打死（結束代碼 141），
#   於是門是紅的、但**紅得看不出原因** —— 螢幕上沒有 `L4 TESTS FAILED`，
#   只有一個信號碼。實測過：反證 ② 第一次跑出來就是 141。
#   ⇒ 忽略它，讓寫失敗變成一個**回傳值**，由下面的判據說出人話。
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

# ── [0/11] 基線：連接埠必須都還沒被佔 ──────────────────────────────────────────
#
# ★ 本儲存庫栽過一次：一個「基線探針」對著上一個場景遺留的處理程序報了綠。
echo "=== [0/11] 基线：端口未被占用 ==="
for p in "$L4_PORT" "$UP_A" "$UP_B" "$HTTP_PORT" "$DEAD_PORT" \
  "$SNI_PORT" "$SNI_A" "$SNI_B" "$SNI_C" "$SNI_D"; do
  if port_listening "$p"; then
    echo "L4 TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑，否则下面测的是别人的服务。" >&2
    exit 1
  fi
done
ok "$L4_PORT / $UP_A / $UP_B / $HTTP_PORT / $DEAD_PORT / $SNI_PORT+四个分流上游都是空的"
# ⚠ UDP 連接埠占沒佔用**探不出來**（UDP 沒有連接，connect 不會失敗）——
#   所以這裡不裝樣子去探，而是讓下面 bind 失敗時**自己說清楚**。
#   ★ 一個探不準的基線判據比沒有更糟：它會讓人以為查過了。
command -v python3 > /dev/null 2>&1 || {
  echo "L4 TESTS FAILED: 镜像里没有 python3 —— UDP 那两格的回声上游要它（见 docker/Dockerfile.build）。" >&2
  exit 1
}

[ -x "$BIN" ] || {
  echo "L4 TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 設定 ────────────────────────────────────────────────────────────────────
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

# 被測執行個體：一個 HTTP 站台（證明處理程序活著）+ 一個 L4 監聽器（本場景的主角）+ 管理面。
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

# ── [1/11] 起服務 ────────────────────────────────────────────────────────────
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
# ★ UDP 回聲上游：收到什麼就帶上自己的標籤回什麼。
#   標籤是這一節全部判據的抓手 —— 它讓「回包是誰發的」可以被**看見**。
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
# ★ 被測執行個體用**共享的** upgrade sock：[6/11] 要靠它把監聽 fd 交給下一代。
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

# ★ 反向自證：$DEAD_PORT 必須真的沒人在聽 —— [5/11] 的全部價值就在它是死的。
if port_listening "$DEAD_PORT"; then
  fail "$DEAD_PORT 上居然有人在听 —— [5/11] 那条判据会失去意义"
else
  ok "$DEAD_PORT 确认没人在听（[5/11] 要用它）"
fi

# ── [2/11] 透傳與輪詢 ────────────────────────────────────────────────────────
#
# ★ 判據不是「連得上」，是**上游收到的東西與用戶端發的一模一樣**：
#   路徑回顯出來了，說明位元組是原樣搬過去的，而不是被誰重組過。
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

# ── [3/11] 管理面：上游換得了，監聽器換不了 ──────────────────────────────────
#
# ★ ★ 這兩條是**一對**：換上游不經過 bind，所以要放行；換監聽器要重新 bind，
#   所以要 409。⚠ 只驗後者的話，一個「什麼都拒絕」的實作照樣綠，
#   而那會讓 `POST /load` 對任何帶 L4 的設定整個不能用。
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

# 監聽連接埠變了 ⇒ 409，且舊的照常服務。
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

# 把兩個上游load 回來，後面兩節要用。
"$BIN" compile "$WORK/l4.Fulcrumfile" > "$WORK/both.json" 2>/dev/null
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/both.json")")
if [ "$CODE" = "200" ]; then
  ok "两个上游 load 回来了"
else
  fail "load 回两个上游失败：$CODE"
fi

# ── [4/11] 一個上游掛掉：用戶端不該有感覺 ────────────────────────────────────
#
# ★ ★ 這一條是「L4 面能不能真用」的分界線。兩個上游掛掉一個時：
#   · 只在**候選位址**之間退回的實作 ⇒ **每兩條連接壞一條**（輪詢照樣輪到死的那個）
#   · 在**上游**之間退回的實作 ⇒ 用戶端全程無感
#   ⚠ 建連之前換上游是安全的：一個位元組都還沒走。（HTTP 那邊有意不這麼做，
#     因為那裡換上游會滑向重試語義——兩邊的邊界寫在 l4.rs 與 pick_index_by 上。）
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

# ── [5/11] 上游全掛：乾淨地關掉，並且說出來 ──────────────────────────────────
#
# ★ L4 上沒有狀態碼可回，能做的只有關掉連接。⚠ 但**必須留一行紀錄**：
#   現場表現是「連上就斷」，紀錄裡沒有那一行的話，維運分不清是上游全掛
#   還是連接埠配錯，甚至會懷疑防火牆。
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
# ★ 反向那一半：HTTP 那一側**不受影響**。⚠ 少了它，一個「L4 出問題就把處理程序搞死」
#   的實作在上面那條判據上表現完全相同。
CODE=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$HTTP_PORT/" 2>/dev/null || echo 000)
if [ "$CODE" = "200" ]; then
  ok "L4 全挂不影响同进程的 HTTP 站点（进程活得好好的）"
else
  fail "HTTP 站点也不行了（$CODE）—— L4 的问题不该外溢"
fi

# ── [6/11] ★ ★ socket 移交：換代時 L4 長連接不斷 ─────────────────────────────
#
# ★ ★ ★ 這是本場景**最貴**的一條，也是 L4 面唯一真正難的地方。
#   自建監聽器如果不參與 fd 移交，第一次 `systemctl reload` 會讓這個連接埠重新 bind，
#   而那一刻**所有 L4 長連接一起斷** —— 與此同時 HTTP 一切正常，
#   於是沒有人會想到去看 L4。⇒ 判據釘三件事：
#     ① 新一代紀錄裡出現「繼承了監聽 fd」（而不是「監聽 … 已登記」）
#     ② 換代**期間建立的那條連接**還能繼續說話
#     ③ 換代之後**新**連接照常（新一代真的在 accept）
echo "=== [6/11] socket 移交：换代时长连接不断 ==="
start upa2 "$WORK/upa.Fulcrumfile"
wait_port "$UP_A" || {
  echo "L4 TESTS FAILED: up-a 起不回来" >&2
  exit 1
}

# 在**換代之前**建一條連接，並在上面完成一次請求-回應。
exec 3<>"/dev/tcp/$HOST/$L4_PORT"
printf 'GET /before HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n' >&3

# 讀一整個回應：狀態行 + 頭（到空行為止）+ 按 Content-Length 精確讀 body。
# ⚠ 用 `read -N` 而不是 `head -c`：前者是 bash 內建、按位元組讀，
#   後者可能一次多讀一塊，把**下一個**回應的開頭吃掉。
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
    # ★ 讀 body 的唯一目的是**把這條回應從流裡讀乾淨**：不讀完的話，
    #   下一次讀會拿到上一條的尾巴，而現場表現是「換代之後回應亂了」。
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

# 觸發換代：SIGQUIT 給第一代，再以 -u 起第二代（與 M0 那一格同一個形狀）。
kill -QUIT "$GEN1" 2>/dev/null || true
RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/l4.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/gen2.pid" \
  --upgrade-sock "$UPGRADE_SOCK" \
  -u \
  > "$WORK/gen2.log" 2>&1 &
GEN2=$!
PIDS+=("$GEN2")

# 等第二代真的接手（它會在紀錄裡說自己繼承了 fd）。
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
# ★ 反向那一半：第二代**不該**打出「新 bind」那一行。
#   ⚠ 少了它，一個「先繼承、再順手 bind 一個」的實作照樣能讓上面那條綠。
if grep -q "已登记为 fulcrum-l4-tcp" "$WORK/gen2.log" 2>/dev/null; then
  fail "第二代还是自己 bind 了一个新的监听器（日志里有「已登记为」）"
else
  ok "第二代没有新 bind 任何 L4 监听器（继承的那一个就是全部）"
fi

# ② 換代之前建立的那條連接**還能繼續說話**。
if printf 'GET /after HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n' >&3 2>/dev/null; then
  AFTER=$(read_response) || AFTER="读不到响应"
else
  # ★ 連寫都寫不進去 = 那條連接在換代那一刻就被斷了，而這正是本節要抓的壞行為。
  AFTER="那条连接已经断了（写都写不进去）"
fi
case "$AFTER" in
  *200*) ok "换代之后**那条老连接**仍然通（$AFTER）—— fd 移交没有打断它" ;;
  *) fail "换代把已经建立的 L4 连接打断了：$AFTER" ;;
esac
exec 3<&-
exec 3>&-

# ③ 換代之後新連接照常。
B=$(curl -sS --max-time 5 "http://$HOST:$L4_PORT/new-conn" 2>/dev/null || echo "curl-failed")
case "$B" in
  *up-a*) ok "换代之后新连接也照常（第二代真的在 accept）" ;;
  *) fail "换代之后新连接不通：$B" ;;
esac

# ★ 老一代收到的是 SIGQUIT（優雅停機），它會**等完整的排空視窗**才走。
#   這裡明確用 SIGINT 收掉它並等它真的沒了 —— 否則 cleanup 裡的 `kill -9`
#   會讓 bash 在收尾時打一行 `Killed` 噪音，而那行讀起來像是本場景出了事。
kill -INT "$GEN1" 2>/dev/null || true
waited=0
while kill -0 "$GEN1" 2>/dev/null && [ "$waited" -lt 50 ]; do
  sleep 0.1
  waited=$((waited + 1))
done

# ── [7/11] UDP 透傳與工作階段 ────────────────────────────────────────────────────
#
# ★ 判據不是「有回包」，是**回包帶著上游的標籤**：那說明資料報真的走到了上游，
#   而不是被誰在中間應付掉。⚠ 第二個包走的是**另一條程式碼路徑**（老工作階段，`touch`），
#   所以兩個包要發在**同一個 socket** 上 —— 換個 socket 就換了源連接埠，那還是新工作階段。
#
# ⚠ ⚠ **用戶端不能用 bash 的 `/dev/udp`**，這一條是本批實測撞出來的：
#   bash 的 `read` 內建對不可 seek 的 fd **一次讀一個位元組**，而在**資料報** socket 上
#   一次 `read()` 會**消費掉整個資料報**並只回傳第一個位元組 —— 於是它拿到 `u`，
#   然後一直等下一個資料報，最後超時。
#   ★ ★ 現場表現是「UDP 透傳不通」，而**被測方完全是好的**（上游紀錄裡那個包收到了）。
#   ⇒ 用戶端換成 python（整包 `recvfrom`），它順帶讓「回包的源位址」可以被**顯式**斷言。
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
  # $1 = 連接埠，其餘 = 要依次發的載荷。每行一條 `<源地址><TAB><内容>`。
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
# ★ ★ 回包**必須來自監聽位址**。⚠ 從工作階段 socket 直接回包也能讓上面那條過，
#   但真實用戶端多半是 connected 的，核心會把那種回包**丟掉** ——
#   現場表現是「偶爾收不到回應」，而抓包才看得出源連接埠不對。
if [ "$SRC1" = "$HOST:$UDP_PORT" ]; then
  ok "回包来自**监听地址** $SRC1（不是会话 socket 的临时端口）"
else
  fail "回包源地址是 $SRC1，应当是 $HOST:$UDP_PORT —— connected 的客户端会把它丢掉"
fi

# 同一個 socket 連發兩個：第二個走「老工作階段」那條路。
SESS=$(udp_send "$UDP_PORT" "sess-1" "sess-2")
S1=$(printf '%s\n' "$SESS" | sed -n '1p' | cut -f2)
S2=$(printf '%s\n' "$SESS" | sed -n '2p' | cut -f2)
if [ "${S1##*:}" = "sess-1" ] && [ "${S2##*:}" = "sess-2" ]; then
  ok "同一个客户端地址连发两个包都通（两个回包都对得上）"
else
  fail "两个包没都通：第一个「$S1」第二个「$S2」"
fi
# ★ ★ **工作階段真的被重複使用了嗎** —— 判據是**上游看到的源連接埠兩次相同**。
#   ⚠ 少了這一條，一個「每個資料報都新建工作階段」的實作在用戶端看來完全正常，
#   而它會把 fd 燒光。★ 這就是那個回聲指令碼要把來源連接埠回顯出來的全部理由。
P1=$(printf '%s' "$S1" | cut -d: -f2)
P2=$(printf '%s' "$S2" | cut -d: -f2)
if [ -n "$P1" ] && [ "$P1" = "$P2" ]; then
  ok "两个包在上游看来来自**同一个源端口** $P1（会话被复用了，不是每包新建）"
else
  fail "上游看到两个不同的源端口（$P1 / $P2）—— 每个数据报都新建了一条会话"
fi

# ── [8/11] ★ ★ UDP 的 socket 移交：換代時老一代必須**停止收包** ──────────────
#
# ★ ★ ★ 這一格比 TCP 那格更要緊，而且**只有 UDP 有**：兩代處理程序持有的是**同一個**
#   UDP socket（fd 移交成功的表現），雙方都在 `recv_from` 時，資料報會在兩代之間
#   **被分流** —— 老一代會把本該屬於新一代的工作階段首包吃掉，
#   而用戶端只看到「換代那幾秒有些請求沒回應」，兩邊紀錄都正常。
#   （這正是 docs/verification/open-seams.md 裡那條 M0 衍生風險的第一次具體完成實作。）
#
# ⚠ **判據必須能分辨「誰在收包」**，而不只是「還通不通」：老一代若仍在收，
#   它照樣能正確轉發（設定一樣），用戶端根本看不出來。
#   ⇒ 讓第二代**指向另一個上游**（tag 不同），於是「回包帶的是誰的 tag」
#     就直接回答了「這個包是哪一代處理的」。
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

# ★ 老一代必須**說出**自己停止收包了 —— 這是那條設計的可觀測面。
# ⚠ 要**等一下**：SIGQUIT 是非同步的，老一代處理它與新一代打出「繼承了 fd」之間
#   沒有先後保證。★ 不等的話，它會偶爾紅在一條**其實成立**的判據上 ——
#   而一條會偶爾假紅的判據，最後一定會被人當成噪音。
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

# ★ ★ 真正的判據：換代之後**每一個**回包都必須帶新一代那個上游的 tag。
#   ⚠ 只要老一代還在 `recv_from`，就會有一部分包帶回舊 tag —— 而它**不是偶發**：
#   兩個處理程序輪流被核心喚醒，比例接近一半。
# ⚠ ⚠ **這是一條概率性判據，必須說清楚**：兩代處理程序誰被核心喚醒是隨機的。
#   ★ 實測（本批的反證 ①，故意不 break）：**6 個資料報裡被老一代搶走 1 個**。
#   ⇒ 發 20 個，把「壞實作卻全綠」壓到 (5/6)^20 ≈ 3%；而 CI 每次推送都跑一遍，
#     連續漏檢的概率可以忽略。⚠ 不寫這段的話，下一個人會以為它是確定性的。
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
# ★ ★ 這一格與前面幾格**用的是另一個執行個體**（自己的連接埠、自己的設定）：前面那些
#   已經換過三代，而分流要的是一份**乾淨的、帶規則的**監聽器。
#   ⚠ 混進去的話，[3/8] 那條「L4 監聽器集變了就 409」會紅在一個無關的原因上。
#
# ★ 判據的抓手是**上游回顯它收到的位元組**（tag + hex）：於是
#   「分流去了誰」與「位元組有沒有被改」這兩件事都可以被**看見**，而不是隻能推斷。
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

# ① 精確 SNI
OUT=$(hello_to api.example.com)
GOT=${OUT%%$'\t'*}
SENT=${OUT##*$'\t'}
case "$GOT" in
  up-sni-a:*) ok "SNI=api.example.com → 精确规则那个上游" ;;
  *) fail "精确 SNI 分流不对：$GOT" ;;
esac
# ★ ★ **ClientHello 必須原樣到達上游**：我們看過它，但一個位元組都不許吃掉。
#   ⚠ 少了這條判據，一個「讀走 ClientHello 就不管了」的實作會讓上面那條照常綠，
#   而真實用戶端會卡在握手上 —— 現場表現是「TLS 偶爾連不上」。
if [ "${GOT#up-sni-a:}" = "$SENT" ]; then
  ok "上游收到的字节与客户端发出的**逐字节相同**（ClientHello 被原样重放）"
else
  fail "ClientHello 没有被原样送到上游：发出 ${#SENT} 个 hex 字符，上游收到 ${#GOT}"
fi

# ② 通配一層
OUT=$(hello_to x.internal.example.com)
case "${OUT%%$'\t'*}" in
  up-wild-b:*) ok "SNI=x.internal.example.com → 通配规则那个上游（只吃一层）" ;;
  *) fail "通配 SNI 分流不对：${OUT%%$'\t'*}" ;;
esac

# ③ 兩層子域**不該**命中通配 —— 這是 G66 那條語義的判據
OUT=$(hello_to a.b.internal.example.com)
case "${OUT%%$'\t'*}" in
  up-default:*) ok "SNI=a.b.internal.example.com **不命中**通配（G66：只吃一层）→ 兜底" ;;
  *) fail "两层子域不该命中通配，却去了：${OUT%%$'\t'*}" ;;
esac

# ④ 不帶 SNI、只帶 ALPN
OUT=$(hello_to - h2)
case "${OUT%%$'\t'*}" in
  up-alpn-c:*) ok "无 SNI + ALPN=h2 → ALPN 规则那个上游" ;;
  *) fail "ALPN 分流不对：${OUT%%$'\t'*}" ;;
esac

# ⑤ ALPN 是逐位元組相等：h2c 不該命中 h2
OUT=$(hello_to - h2c)
case "${OUT%%$'\t'*}" in
  up-default:*) ok "ALPN=h2c **不命中** h2（逐字节相等，不是前缀）→ 兜底" ;;
  *) fail "h2c 不该命中 h2 规则，却去了：${OUT%%$'\t'*}" ;;
esac

# ⑥ ★ ★ ★ **一個「完整 TLS 棧會當場拒掉」的 ClientHello，照樣按它的 SNI 分流。**
#
#   ⚠ ⚠ **（G104 第 ② 處）：這條判據換了期望值，而紅的不是新寫的門。**
#   舊契約（M2 批 C，原文保留）：
#     > ★ ★ **rustls 不認的 ClientHello 也走備援，而不是被關掉**。
#     > 這條判據是本批實測撞出來的：缺 `signature_algorithms` 時 rustls 直接拒
#     > （`SignatureAlgorithmsExtensionRequired`）—— 也就是說，**「能不能分流」取決於
#     > rustls 認不認這個 hello，而那比「能讀出 SNI」嚴格**。
#     > ⇒ 取捨寫在明處：這種連接**交給備援上游**，讓真正的對端去決定怎麼答覆；
#     >   樞衡不替上游拒絕一個它自己也許能處理的用戶端。
#
#   ★ 預讀換成 BoringSSL 的早回呼之後，那句「取捨」**兌現得比當初更徹底**：
#   早回呼發生在幾乎所有 ClientHello 檢核**之前**，所以這裡讀得出 SNI 就按 SNI 分流 ——
#   **「能不能分流」終於只取決於「能不能讀出 SNI」**，而不再取決於某個 TLS 棧的嚴格程度。
#   ⇒ 期望值從「備援」換成「精確規則那個上游」。
#   ⚠ ⚠ **這不是判據變鬆**：變鬆的寫法是「去了哪兒都行，別被關掉就成」，
#     而這裡釘死的是一個**更窄**的結論。舊契約保留在上面，換的理由就在這幾行。
#
#   ⚠ 順帶說清一件事：我們**沒有**檢核這個 hello 就把它轉走了 ——
#   所以「位元組原樣重放」在這條路上不是錦上添花，而是這個取捨成立的**前提**：
#   上游必須拿到一份未經我們改動的 hello，才談得上「讓它自己決定」。
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

# ⑦ 根本不是 TLS ⇒ 備援，且**讀走的位元組要原樣重放**
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

# ── [10/11] ★ ★ ★ PROXY protocol：收與發（批 D）──────────────────────────────
#
# ★ ★ 又是**另一個執行個體**（自己的連接埠、自己的設定），理由與分流那一格逐字相同。
#
# ★ ★ ★ **這一格最要緊的那條判據，驗的是「什麼都不做」**：
#   來源不在信任清單裡時，樞衡**一個位元組都不許讀** —— 那 12/16 位元組要原樣流給上游。
#   ⚠ 它與「讀掉丟棄」在功能上都「能用」，而兩者給上游的是**完全不同的兩條流**。
#   ⇒ 判據只能落在**上游收到的位元組**上，落在別處（紀錄、狀態碼）都分不出這兩種。
#
# 四個監聽器，四種組合 —— 因為**收與發是兩件獨立的事，不是一個開關的兩半**：
#   9313 收 + 發（鏈式）· 9315 只收 · 9316 不信任（清單裡沒有 127/8）· 9317 只發
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

# 上游：重複使用分流那一格的回顯指令碼（收什麼回什麼，回的是 hex）。
#
# ★ ★ ★ **這一格是唯一開 `eof` 的**：它的用戶端是 `ppclient.py`（發完就半關閉），
#   而這一格的判據要斷「頭**之後**跟著載荷」—— 那是兩次獨立的 write，
#   「一次 recv」判不動它。⇒ 其餘幾格的上游仍走預設（curl / `hello.py` 不半關閉）。
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

# 用戶端夾具：精確控制**發出去的第一批位元組**。
#
# ★ 有意讓它接受一段 hex 而不是「造一個頭給我」：這一格要發的東西里有
#   **壞頭**與 **LOCAL 頭**，而一個「會造合法頭」的夾具造不出它們。
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

# 一個 v1 頭：用戶端自稱 203.0.113.7:40000。
PP_V1_HEX=$(printf 'PROXY TCP4 203.0.113.7 10.0.0.9 40000 443\r\n' | od -An -tx1 | tr -d ' \n')
PAYLOAD='hello-l4'
PAYLOAD_HEX=$(printf '%s' "$PAYLOAD" | od -An -tx1 | tr -d ' \n')

# ① ★ ★ ★ **不在信任清單裡 ⇒ 一個位元組都不讀。**
#
#   上游收到的必須是 **頭 + 載荷**，逐位元組相同。
#   ⚠ 這一條紅了而其餘全綠，說明實作取的是「讀掉丟棄」那條路 ——
#     兩者在功能上都「能用」，而這條判據是唯一能分開它們的東西。
GOT=$(python3 "$WORK/ppclient.py" "$PP_UNTRUSTED" "$PP_V1_HEX" "$PAYLOAD")
if [ "${GOT#up-pp:}" = "${PP_V1_HEX}${PAYLOAD_HEX}" ]; then
  ok "★ 不在信任清单里：PROXY 头**一个字节都没被读走**，原样流给了上游"
else
  fail "不信任的来源，头没有被原样透传：期望 ${PP_V1_HEX}${PAYLOAD_HEX}，上游收到 ${GOT#up-pp:}"
fi

# ② 在清單裡、只收不發 ⇒ 頭被吃掉，上游只看到載荷。
GOT=$(python3 "$WORK/ppclient.py" "$PP_RECV" "$PP_V1_HEX" "$PAYLOAD")
if [ "${GOT#up-pp:}" = "$PAYLOAD_HEX" ]; then
  ok "在信任清单里：头被吃掉，上游只收到应用数据"
else
  fail "只收不发那条路上，上游收到的不是纯载荷：${GOT#up-pp:}（期望 $PAYLOAD_HEX）"
fi

# ③ ★ ★ **鏈式傳遞**：收了一個頭，發出去的那個頭裡必須還是**最初那個用戶端**。
#
#   ⚠ 這一條同時證明了「收對了」與「發對了」—— 而且它是**唯一**能證明
#     「收到的位址真的被用上了」的判據：L4 是透傳，樞衡不會把它看到的 IP 說給任何人聽。
GOT=$(python3 "$WORK/ppclient.py" "$PP_CHAIN" "$PP_V1_HEX" "$PAYLOAD")
GOT_HEX=${GOT#up-pp:}
# ★ hex → 文字用 python，**有意不用 `sed` + `printf`**：後者要寫反斜槓，
#   而 `AGENTS.md` 那節第 1 條記著「heredoc 會吃掉一層反斜槓」——
#   ⚠ 本批在寫這一格時又踩了一次，而它的表現是給出一份**看起來很像真結論**的空串。
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

# ④ 只發不收：樞衡是第一跳 ⇒ 報的是 socket 對端（127.0.0.1），且寫的是 **v2**。
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

# ⑤ ★ **`LOCAL` 不是壞頭**：上游 LB 的健康檢查就長這樣。
#
#   ⚠ 把它判成壞頭的實作，現場表現是**每一次健康檢查都斷連** ——
#     而那時業務流量看起來完全正常。
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

# ⑦ ★ **在清單裡卻發來壞頭 ⇒ 關連接**（與「不在清單裡」有意相反）。
#
#   理由：此時我們**已經吃掉了一部分位元組、還原不回去**，把殘缺的流轉給上游
#   只會把問題推到一個更難查的地方。
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

# ⑧ ★ ★ **不發頭時，那條路上一個位元組都不多加**（發那側的反向判據）。
#
#   ⚠ 少了它，一個「無條件都發一個頭」的實作在前面每一條上都是綠的。
#   ⚠ ⚠ 用的是**不信任**那個監聽器而不是 $PP_RECV —— 理由見下面 ⑨：
#     $PP_RECV 信任本機，而信任的來源**必須**發頭。
GOT=$(python3 "$WORK/ppclient.py" "$PP_UNTRUSTED" none "$PAYLOAD")
if [ "${GOT#up-pp:}" = "$PAYLOAD_HEX" ]; then
  ok "没配 proxy_protocol 的监听器**不给上游多发任何字节**"
else
  fail "没配发送却多发了东西：${GOT#up-pp:}（期望 $PAYLOAD_HEX）"
fi

# ⑨ ★ ★ ★ **在信任清單裡卻不發頭 ⇒ 連接被關掉。**
#
#   ⚠ ⚠ **這是一條會讓人意外的產品行為，所以它必須有判據，而不是只寫在文件裡。**
#   它是本批寫這一格時**被一條寫錯的判據撞出來的**：⑧ 最初用的是 $PP_RECV，
#   而它紅在「上游一個位元組都沒收到」上 —— 查下去發現那不是缺陷，是語義。
#
#   ★ 取「必須發」而不是「發了就用、沒發就算了」的理由：
#     `proxy_protocol_from 10.0.0.0/8` 這句話的意思是**「這個網段的流量是經代理來的」**。
#     若允許清單內的來源選擇性地不發頭，那它就能讓樞衡改用 socket 對端 ——
#     而那個位址正是 LB 自己，於是一條 `remote_ip 10.0.0.0/8` 規則會**命中它**。
#     ⇒ 「可選」把一個顯式的信任宣告變成了一個可以被對端單方面關掉的開關。
#   ⚠ 代價寫在明處：清單配寬了（比如整個 10/8），那個網段裡**所有不發頭的正常連接
#     都會被拒**。這與 HAProxy 的 `accept-proxy`、nginx 的 `listen … proxy_protocol`
#     是同一條語義，只是我們多了一層來源限定。
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
# ⚠ **成功那一支有意不寫 `exit 0`**：整指令碼每條路都顯式 exit 時，shellcheck 0.10
#   會把 `trap cleanup EXIT` 認成夠不到的程式碼，於是 cleanup 整個函式被報 SC2317 ——
#   ★ 而那是一條**關於本指令碼自己**的假警報：cleanup 每次都真的跑了。
#   實測過（本批當場撞到）：把末尾的 `exit 0` 去掉，同一份指令碼就乾淨了。
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
