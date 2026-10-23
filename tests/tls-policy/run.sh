#!/usr/bin/env bash
# 兩個全域 TLS 設定（G168）的端到端：`tls_groups` 與 `tls_session_resumption`，真處理程序、真握手。
#
#   bash tests/tls-policy/run.sh      # 在建置鏡像裡，`target/release/fulcrum` 已建好
#
# ★ 用戶端是建置鏡像裡的 `openssl s_client`（OpenSSL 3.5.x）與 curl 8.14.1（h3 走 OpenSSL 的 QUIC 棧）
#   —— 與被測的 BoringSSL / quiche 沒有一行共同程式碼，判的是互操作，不是自己對自己的理解。
#
# 連接埠（表在 docs/platform/host-and-gate-traps.md）：
#   9450 不寫鍵的對照組 · 9451 `tls_groups X25519MLKEM768` + `tls_session_resumption false`（含 h3）·
#   9452 換代那一組 · 9453 只寫 `tls_session_resumption false`（TLS 1.2 那一條要它：
#   後量子組在 1.2 裡不存在，只釘 `X25519MLKEM768` 的那一組 1.2 用戶端握不上）。
#
# ⚠ 每份設定都寫 `auto_http_redirect false`：本場景同時起好幾個處理程序，而 TLS 站台預設會合成一個 `:80`
#   重定向站台（見連接埠表）—— 第二個處理程序會綁不上 `:80`，紅的理由與本場景無關。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
NAME=tls-policy.test
P_DEFAULT=${P_DEFAULT:-9450}
P_POLICY=${P_POLICY:-9451}
P_GEN=${P_GEN:-9452}
P_OFF=${P_OFF:-9453}
ALL_PORTS="$P_DEFAULT $P_POLICY $P_GEN $P_OFF"

FAILS=0
PIDS=()
fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}
wait_port() {
  local tries=0
  while [ "$tries" -lt 150 ]; do
    if port_listening "$1"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

cleanup() {
  local pid waited p leaked=""
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 60 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -9 "$pid" 2>/dev/null || true
  done
  # ★ 收尾自證（照 tests/quic-relay/run.sh）：判在「連接埠還回去了沒有」，⛔ 不判「處理程序還在不在」——
  #   沒進 `PIDS` 的那個 pid 恰恰是這裡最找不到的。
  for p in $ALL_PORTS; do
    waited=0
    while port_listening "$p" && [ "$waited" -lt 30 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    if port_listening "$p"; then leaked="$leaked $p"; fi
  done
  rm -rf "$WORK"
  if [ -n "$leaked" ]; then
    echo "TLS-POLICY TESTS FAILED: 收尾没干净 —— 退出时这些端口还有人在听：$leaked" >&2
    pgrep -af "fulcrum serve" >&2 || true
    exit 1
  fi
}
trap cleanup EXIT

# 起一個處理程序。$1 = 名字，$2.. = 額外參數；pid 放在 START_PID（⛔ 不 `echo` —— `$(…)` 是子 shell，
# `PIDS+=` 會改到副本上，見 quic-relay 那段）。
start() {
  local name=$1
  shift
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/$name.conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.upgrade.sock" \
    --state-dir "$WORK/$name.state" \
    "$@" >> "$WORK/$name.log" 2>&1 &
  START_PID=$!
  PIDS+=("$START_PID")
}

# 寫一份設定。$1 = 名字，$2 = 連接埠，$3 = 回應體，$4 = 全域塊裡額外的那幾行（可空）。
write_conf() {
  cat > "$WORK/$1.conf" <<CONF
{
    auto_http_redirect false
    grace_period 1s
    admin unix/$WORK/$1.admin.sock
$4
}
$NAME:$2 {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 "$3"
}
CONF
}

# 一次握手，原文落 `$WORK/$1.txt`。$2 = 連接埠，其餘參數照給 s_client。
# ★ 連上之後**等 1 秒**再斷：TLS 1.3 的 NewSessionTicket 在握手之後才到，斷得太快會數成 0 張。
probe() {
  local out=$1 port=$2
  shift 2
  { printf 'GET / HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n' "$NAME"; sleep 1; } \
    | timeout 10 openssl s_client -connect "$HOST:$port" -servername "$NAME" -msg "$@" \
      > "$WORK/$out.txt" 2>&1 || true
}
handshook() { grep -Eq '^(New|Reused), TLSv1\.[23]' "$WORK/$1.txt"; }
# ⚠ OpenSSL 3.5 的 s_client 只對 KEM 組（`X25519MLKEM768`）印 `Negotiated TLS1.3 group:`；
#   ECDH 組印的是 `Peer Temp Key: X25519, 253 bits` / `Peer Temp Key: ECDH, prime256v1, 256 bits`（10-23 實測）
#   ⇒ 兩種都要認，只認前一種的話「協商出 X25519」會被讀成「沒協商出組」。
group_of() {
  local f="$WORK/$1.txt" g
  g=$(sed -n 's/^Negotiated TLS1.3 group: *//p' "$f" | grep -v '<NULL>' | head -1)
  if [ -z "$g" ]; then
    g=$(sed -n 's/^Peer Temp Key: *\([^,]*\),.*/\1/p' "$f" | head -1)
    [ "$g" != ECDH ] || g=$(sed -n 's/^Peer Temp Key: *ECDH, *\([^,]*\),.*/\1/p' "$f" | head -1)
  fi
  printf '%s\n' "$g"
}
tickets_of() { grep -c 'NewSessionTicket' "$WORK/$1.txt" || true; }
# ⚠ `|| true`：`pipefail` 下沒匹配時 grep 退 1，而它多半在 `$(…)` 裡 —— `set -e` 會讓整個場景當場退出，
#   紅的理由變成「指令碼死了」而不是「沒握上」。
reuse_of() { grep -Eo '^(New|Reused), TLSv1\.[23]' "$WORK/$1.txt" | head -1 || true; }

# 兩次連接：第一次存 session，第二次帶上它。結果（`New, TLSv1.3` / `Reused, TLSv1.3`）印出來。
# ⚠ 第一次沒拿到可存的 session（0 張 ticket 的 TLS 1.3）時 s_client 不寫檔案 ⇒ 第二次照常連、必然是新握手。
second_of() {
  local tag=$1 port=$2
  shift 2
  rm -f "$WORK/$tag.sess"
  probe "$tag-1" "$port" -sess_out "$WORK/$tag.sess" "$@"
  if [ -s "$WORK/$tag.sess" ]; then
    probe "$tag-2" "$port" -sess_in "$WORK/$tag.sess" "$@"
  else
    probe "$tag-2" "$port" "$@"
  fi
  reuse_of "$tag-2"
}

h3_status() {
  # $1 = 連接埠，其餘給 curl。回 HTTP 碼（握不上 = 000）。
  local port=$1 code
  shift
  # ⚠ ⛔ 不寫 `|| echo 000`：curl 失敗時 `-w` 照樣印出 `000`，再補一個就成了 `000000`，
  #   而判據比的是 `000` —— 一條「握不上」會被判成「握上了」。
  code=$(curl -sS -k --http3-only --max-time 8 "$@" --resolve "$NAME:$port:$HOST" \
    -o /dev/null -w '%{http_code}' "https://$NAME:$port/" 2>/dev/null) || true
  echo "${code:-000}"
}

admin_load() {
  # $1 = 名字（用它的 admin socket），$2 = 結構化設定檔。回狀態碼，正文落 $WORK/admin.out。
  local code
  code=$(curl -s -o "$WORK/admin.out" -w '%{http_code}' --unix-socket "$WORK/$1.admin.sock" \
    -X POST --data-binary "@$2" "http://localhost/load?overrides=clear" 2>/dev/null) || true
  echo "${code:-000}"
}

compile_to() {
  # $1 = 設定檔，$2 = 輸出的結構化 JSON。
  "$BIN" compile "$1" > "$2" 2>"$2.err" || {
    echo "TLS-POLICY TESTS FAILED: compile 不过：$(cat "$2.err")" >&2
    exit 1
  }
}

# ── [0/5] 基線 ──────────────────────────────────────────────────────────────
echo "=== [0/5] 基线：端口空着，证书造好了 ==="
for p in $ALL_PORTS; do
  if port_listening "$p"; then
    echo "TLS-POLICY TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
ok "四个端口都空着"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=$NAME" -addext "subjectAltName=DNS:$NAME" >/dev/null 2>&1 || {
  echo "TLS-POLICY TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}
ok "ECDSA P-256 自签证书（与对拍那一类同一种）"

write_conf default "$P_DEFAULT" default ""
write_conf policy "$P_POLICY" policy "    tls_groups X25519MLKEM768
    tls_session_resumption false"
write_conf off "$P_OFF" off "    tls_session_resumption false"
for n in default policy off; do start "$n"; done
for p in "$P_DEFAULT" "$P_POLICY" "$P_OFF"; do
  wait_port "$p" || {
    echo "TLS-POLICY TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "三个实例都起来了（对照组 $P_DEFAULT · 政策组 $P_POLICY · 只关恢复 $P_OFF）"

# ── [1/5] ★ 尺子自證：對照組上，這幾把尺子**量得到**恢復、ticket 與 X25519 ───────────
echo "=== [1/5] ★ 尺子自证：不写键的那个实例上，恢复、ticket、X25519 都量得到 ==="
# ⚠ 少了這一節，下面「0 張 ticket」「新握手」「X25519 握不上」三條在一把壞尺子下也是綠的。
probe d-x25519 "$P_DEFAULT" -groups X25519
if handshook d-x25519 && [ "$(group_of d-x25519)" = X25519 ]; then
  ok "对照组：只给 X25519 照样握得上，协商出 X25519"
else
  fail "对照组只给 X25519 都握不上 —— 尺子坏了：$(tail -3 "$WORK/d-x25519.txt" | tr '\n' ' ')"
fi
probe d-both "$P_DEFAULT" -groups X25519MLKEM768:X25519
if [ "$(group_of d-both)" = X25519MLKEM768 ]; then
  ok "对照组：两者都给时协商出 X25519MLKEM768（BoringSSL 默认就开着后量子组）"
else
  fail "对照组协商出的组是 '$(group_of d-both)'"
fi
T=$(tickets_of d-both)
if [ "$T" -gt 0 ] 2>/dev/null; then ok "对照组：数到 $T 张 NewSessionTicket（⇒ 这把尺子数得到 ticket）"; else fail "对照组一张 ticket 都没数到 —— 等得不够久，或者 -msg 的输出格式变了"; fi
R=$(second_of d-r13 "$P_DEFAULT" -groups X25519MLKEM768:X25519)
if [ "$R" = "Reused, TLSv1.3" ]; then ok "对照组：带 session 的第二次是恢复的（TLS 1.3）"; else fail "对照组第二次不是恢复的：'$R' —— 尺子量不到恢复"; fi
R=$(second_of d-r12 "$P_DEFAULT" -tls1_2)
if [ "$R" = "Reused, TLSv1.2" ]; then ok "对照组：TLS 1.2 第二次也是恢复的"; else fail "对照组 TLS 1.2 第二次不是恢复的：'$R'"; fi
if [ "$(h3_status "$P_DEFAULT" --curves X25519)" = 200 ]; then
  ok "对照组：h3 上只给 X25519 照样 200（⇒ curl 的 --curves 管得到 h3）"
else
  fail "对照组 h3 上只给 X25519 就不通了 —— 下面 h3 那一条会变成空转"
fi

# ── [2/5] 政策組：組釘死、一張 ticket 都不發、不恢復 ─────────────────────────────
echo "=== [2/5] 政策组：tls_groups X25519MLKEM768 + tls_session_resumption false ==="
probe p-x25519 "$P_POLICY" -groups X25519
if handshook p-x25519; then fail "组钉死了，只给 X25519 的客户端却握上了"; else ok "只给 X25519 的客户端握不上"; fi
probe p-both "$P_POLICY" -groups X25519MLKEM768:X25519
if [ "$(group_of p-both)" = X25519MLKEM768 ] && grep -q 'TLSv1.3' "$WORK/p-both.txt"; then
  ok "两者都给时协商出 X25519MLKEM768、TLS 1.3"
else
  fail "协商出的组是 '$(group_of p-both)'"
fi
T=$(tickets_of p-both)
if [ "$T" = 0 ]; then ok "一张 NewSessionTicket 都没发"; else fail "关了恢复却发了 $T 张 ticket"; fi
R=$(second_of p-r13 "$P_POLICY" -groups X25519MLKEM768:X25519)
if [ "$R" = "New, TLSv1.3" ]; then ok "带 session 的第二次是新握手"; else fail "关了恢复，第二次却是 '$R'"; fi
if [ "$(h3_status "$P_POLICY" --curves X25519)" = 000 ]; then ok "h3：只给 X25519 握不上"; else fail "h3 上组没钉住"; fi
if [ "$(h3_status "$P_POLICY" --curves X25519MLKEM768)" = 200 ]; then ok "h3：给 X25519MLKEM768 就 200"; else fail "h3 上给了钉的那个组也不通"; fi

# 只關恢復的那一個：TLS 1.2 的 session ID / 1.2 ticket 那一半（cache OFF）。
R=$(second_of o-r12 "$P_OFF" -tls1_2)
if [ "$R" = "New, TLSv1.2" ]; then ok "只关恢复：TLS 1.2 第二次也是新握手"; else fail "TLS 1.2 下关了恢复却是 '$R'"; fi

# 裝載紀錄：HTTPS 與 h3 兩行都說出了政策；不寫的那個說「BoringSSL 默认」。
if grep -q "监听 .*$P_POLICY.*X25519MLKEM768.*会话恢复 关" "$WORK/policy.log" \
  && grep -q "\[quic\] 监听 .*$P_POLICY.*X25519MLKEM768.*会话恢复 关" "$WORK/policy.log"; then
  ok "启动日志里 HTTPS 与 h3 两行都说出了生效的政策"
else
  fail "启动日志没说出政策：$(grep '监听' "$WORK/policy.log" | tr '\n' ' ')"
fi
if grep -q "监听 .*$P_DEFAULT.*BoringSSL 默认" "$WORK/default.log"; then ok "不写的那个说「BoringSSL 默认」"; else fail "不写的那个日志没说缺省"; fi

# ── [3/5] `POST /load`：兩個值變了就 409，沒變就照常換 ───────────────────────────
echo "=== [3/5] POST /load：TLS 设置变了 ⇒ 409，且什么都没换 ==="
write_conf policy-g "$P_POLICY" policy "    tls_groups X25519MLKEM768 X25519
    tls_session_resumption false"
write_conf policy-r "$P_POLICY" policy "    tls_groups X25519MLKEM768"
write_conf policy-same "$P_POLICY" reloaded "    tls_groups X25519MLKEM768
    tls_session_resumption false"
for v in g r same; do
  # ⚠ admin 那一行要指著**正在跑的**那個 socket —— 三份設定共用 policy 的。
  sed -i "s|unix/$WORK/policy-$v.admin.sock|unix/$WORK/policy.admin.sock|" "$WORK/policy-$v.conf"
  compile_to "$WORK/policy-$v.conf" "$WORK/policy-$v.json"
done
CODE=$(admin_load policy "$WORK/policy-g.json")
if [ "$CODE" = 409 ] && grep -q 'tls_groups' "$WORK/admin.out" && grep -q 'systemctl reload' "$WORK/admin.out"; then
  ok "只改 tls_groups ⇒ 409，点了名、指向 systemctl reload"
else
  fail "改了 tls_groups 却是 $CODE：$(cat "$WORK/admin.out")"
fi
probe p-after-g "$P_POLICY" -groups X25519
if handshook p-after-g; then fail "409 之后组却变了（X25519 握得上了）"; else ok "409 之后握手照旧（X25519 仍握不上）"; fi
CODE=$(admin_load policy "$WORK/policy-r.json")
if [ "$CODE" = 409 ] && grep -q 'tls_session_resumption' "$WORK/admin.out"; then
  ok "只改 tls_session_resumption ⇒ 409，点了名"
else
  fail "改了 tls_session_resumption 却是 $CODE：$(cat "$WORK/admin.out")"
fi
probe p-after-r "$P_POLICY" -groups X25519MLKEM768:X25519
if [ "$(tickets_of p-after-r)" = 0 ]; then ok "409 之后照样一张 ticket 都不发"; else fail "409 之后开始发 ticket 了"; fi
CODE=$(admin_load policy "$WORK/policy-same.json")
BODY=$(curl -sk --max-time 5 --resolve "$NAME:$P_POLICY:$HOST" "https://$NAME:$P_POLICY/" 2>/dev/null || true)
if [ "$CODE" = 200 ] && [ "$BODY" = reloaded ]; then
  ok "两个 TLS 设置都没变 ⇒ 200，而且真的换上了（响应体成了 reloaded）"
else
  fail "两个 TLS 设置都没变却是 $CODE / 响应体 '$BODY'：$(cat "$WORK/admin.out")"
fi

# ── [4/5] 換代：改了恢復開關 → SIGQUIT + `-u` 起新一代 ⇒ 新值生效 ─────────────────
echo "=== [4/5] 换代之后新值生效（systemctl reload 走的就是这条）==="
write_conf gen "$P_GEN" gen ""
start gen
GEN1=$START_PID
wait_port "$P_GEN" || {
  echo "TLS-POLICY TESTS FAILED: 换代那一组的第一代起不来：$(cat "$WORK/gen.log")" >&2
  exit 1
}
R=$(second_of g1 "$P_GEN" -groups X25519MLKEM768:X25519)
if [ "$R" = "Reused, TLSv1.3" ]; then ok "第一代（不写键）：第二次是恢复的"; else fail "第一代第二次是 '$R'"; fi
write_conf gen "$P_GEN" gen "    tls_session_resumption false"
kill -QUIT "$GEN1"
start gen --upgrade
waited=0
while kill -0 "$GEN1" 2>/dev/null && [ "$waited" -lt 150 ]; do
  sleep 0.1
  waited=$((waited + 1))
done
if kill -0 "$GEN1" 2>/dev/null; then fail "第一代 15 秒还没退（grace_period 是 1s）"; fi
wait_port "$P_GEN" || fail "换代之后 $P_GEN 没人听了"
R=$(second_of g2 "$P_GEN" -groups X25519MLKEM768:X25519)
T=$(tickets_of g2-1)
if [ "$R" = "New, TLSv1.3" ] && [ "$T" = 0 ]; then
  ok "★ 第二代：0 张 ticket、第二次是新握手 ⇒ 改了的那一行换代之后真的生效了"
else
  fail "第二代：ticket $T 张、第二次 '$R'"
fi

# ── [5/5] 結論 ──────────────────────────────────────────────────────────────
echo
if [ "$FAILS" = 0 ]; then
  echo "TLS-POLICY TESTS PASSED —— 全局 TLS 设置真的在跑（尺子自证 · 组钉死两向 · 0 张 ticket · 不恢复（1.3 与 1.2）· h3 同一份政策 · /load 两个值变了就 409 · 换代之后新值生效 · 启动日志）。"
else
  echo "TLS-POLICY TESTS FAILED: $FAILS 处失败" >&2
  exit 1
fi
