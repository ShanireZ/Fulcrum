#!/usr/bin/env bash
# HTTP 面的 PROXY protocol「收」半邊端到端（**M2 批 L 第 ① 步**）。
#
# ★ ★ 為什麼它是**獨立一格**而不是塞進 `tests/serve/run.sh`：那一格驗的是
#   「七層路由決策被執行對了沒有」，而這一格驗的是**連接開頭的那幾十個位元組** ——
#   它發生在 HTTP 之前、甚至在 TLS 之前。⚠ 兩者混在一起時，一條斷言紅了，
#   讀紀錄的人會先去看路由，而問題其實在監聽器那一層。
#
# ★ **判據掛在 `remote_ip` 匹配器上，而不是掛在某個自造的偵錯頭上**：
#   `remote_ip` 是**使用者看得見的那一面** —— 它決定一條 `handle @internal` 命不命中。
#   ⇒ 這一格量的是「使用者寫的規則會不會按真實用戶端生效」，不是「我們內部記對了沒有」。
#
# 連接埠（★ 與其餘場景全都錯開，見 AGENTS.md 那張連接埠表）：
#   9800 執行個體 A 的 HTTP 站台（**配了** `proxy_protocol_from`）
#   9801 執行個體 A 的 HTTPS 站台（同一個處理程序、同一份全域清單）
#   9802 執行個體 B 的 HTTP 站台（**沒配** —— 反向判據要它）
#   9803 **永遠沒人在聽**（用戶端自證要它）
#
# ★ 用戶端是 `client.py`（裸 socket），理由見它的檔案頭：
#   curl 沒有任何辦法讓我們在連接開頭塞自己的位元組。

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

# `$1` = 說明，`$2` = 期望的整行，`$3` = 實測的整行。
expect_line() {
  if [ "$3" = "$2" ]; then
    ok "$1"
  else
    fail "$1：期望「$2」实际「$3」"
  fi
}

# ── [0/5] 基線：連接埠未被佔 + **用戶端自己會不會說「不」** ────────────────────
#
# ★ ★ 兩條自證缺一不可（這條紀律是 `tests/h3/run.sh` 立的）：
#   ① 連接埠沒被佔 —— 否則後面每一條斷言測的都是別人的服務；
#   ② **用戶端對著一個沒人聽的連接埠必須失敗** —— 否則它「永遠回傳點什麼」這件事
#      會讓下面所有反向判據變成空轉，而它們看起來全是綠的。
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

# ── 自簽證書（HTTPS 那一格要）──────────────────────────────────────────────
#
# ⚠ SAN 必須有 `a.example`：樞衡按**證書自己的 SAN** 決定這張證書裝在哪些 SNI 上。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" \
  -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "PROXYPROTO TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ── 兩份設定 ────────────────────────────────────────────────────────────────
#
# ★ ★ 站台裡那條 `@real remote_ip 192.0.2.0/24` 就是整格的判據核心：
#   `192.0.2.0/24` 是 RFC 5737 的**文件專用網段**，不可能是任何真實對端 ——
#   ⇒ 它命中，當且僅當「PROXY 頭裡那個位址真的被用上了」。
#   ⚠ 若換成一個可能真出現的網段，一條「壓根沒解析頭」的實作也可能碰巧綠。
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

# ⚠ 執行個體 B **有意不寫** `proxy_protocol_from` —— 它是第 [4/5] 節那條反向的對照物。
cat > "$WORK/b.Fulcrumfile" <<CONF
http://a.example:$B_HTTP {
    @real remote_ip 192.0.2.0/24
    handle @real {
        respond 200 "real"
    }
    respond 200 "peer"
}
CONF

# ── [1/5] 起服務 ────────────────────────────────────────────────────────────
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

# ── [2/5] 正向：受信來源發頭 ⇒ `remote_ip` 看到的是**頭裡那個** ─────────────
echo "=== [2/5] 正向：受信来源发头，remote_ip 换成真实客户端 ==="

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1:192.0.2.7:56324 --split)
expect_line "v1，头与请求**分两段**发" "STATUS=200 BODY=real" "$R"

# ★ ★ ★ 這一條是 fork 那側 `rewind()` 存在的**全部理由**。
#   一次 read 會把 PROXY 頭與它後面的 `GET` **一起**讀回來；多讀到的那幾個位元組
#   必須原樣還回流裡。⚠ 丟掉 rewind 的實作**只在這一條上壞**，
#   而上面那條（分兩段發）照樣綠 —— 兩條缺一不可。
R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1:192.0.2.7:56324)
expect_line "★★★ v1，头与请求在**同一个段**里（多读的字节要还回去）" "STATUS=200 BODY=real" "$R"

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v2:192.0.2.9:41000)
expect_line "★★★ v2（二进制），头与请求在同一个段里" "STATUS=200 BODY=real" "$R"

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v2:192.0.2.9:41000 --split)
expect_line "v2，分两段发" "STATUS=200 BODY=real" "$R"

# ── [3/5] 邊界：`LOCAL` / `UNKNOWN` **不覆蓋**，繼續用 socket 對端 ──────────
#
# ⚠ ⚠ 這兩條同時是上面那四條的**對照**：它們走的是同一條程式碼路徑、
#   同樣讀掉了一個合法的頭，而結論相反。
#   ★ 沒有它們，一個「只要看見 PROXY 前綴就把 remote_ip 塞成 192.0.2.x」的實作
#     會在 [2/5] 下面全綠。
echo "=== [3/5] 边界：LOCAL / UNKNOWN 是合法的，而它们不覆盖地址 ==="

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1unknown)
expect_line "★★ v1 \`PROXY UNKNOWN\` ⇒ 头收下了，地址**不换**" "STATUS=200 BODY=peer" "$R"

R=$(python3 "$CLIENT" --port "$A_HTTP" --header v2local)
expect_line "★★ v2 \`LOCAL\` ⇒ 同上（健康检查就长这样）" "STATUS=200 BODY=peer" "$R"

# ── [4/5] ★★★ 兩條反向 ──────────────────────────────────────────────────────
echo "=== [4/5] 两条反向 ==="

# ⑦ 清單內的來源**不發頭** ⇒ 關連接（§10 拍板的語義）。
#    ★ 取「必須發」而不是「發了就用、沒發就算了」的理由是**安全上的**：
#      若允許清單內的來源選擇性地不發頭，它就能讓樞衡改用 socket 對端 ——
#      而那個位址正是 LB 自己，於是一條 `remote_ip 10.0.0.0/8` 規則會命中它。
R=$(python3 "$CLIENT" --port "$A_HTTP")
expect_line "★★★ 清单内的来源不发头 ⇒ 连接被关掉" "CLOSED" "$R"

# ⑧ ★ ★ ★ **沒配 `proxy_protocol_from` 的執行個體：一個位元組都不讀。**
#    ⇒ 那段 PROXY 文字被當成 HTTP 請求行，於是是一個**壞請求**。
#    ⚠ ⚠ 少了這一條，一個「無條件解析 PROXY 頭」的實作會在 [2/5] 下面全綠 ——
#      而它意味著**任何用戶端都能自稱是任意 IP**，`remote_ip` 規則隨之失守。
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

# 對照：同一個執行個體、不發頭 ⇒ 一切正常。★ 它證明上面那一條的成因是**那個頭**，
# 而不是「執行個體 B 根本沒起來」。
R=$(python3 "$CLIENT" --port "$B_HTTP")
expect_line "对照：同一个实例不发头 ⇒ 正常服务" "STATUS=200 BODY=peer" "$R"

# ── [5/5] TLS + 換設定立刻生效 ──────────────────────────────────────────────
echo "=== [5/5] PROXY 头在 ClientHello 之前；以及换配置立刻生效 ==="

# ⑨ ★ ★ ★ **位置判據**：PROXY 頭按規格在 **TLS 之前**。
#    ⚠ 若那段讀取被放在 TLS 握手**之後**，這一條會紅在「握手失敗」上 ——
#      因為 BoringSSL 會把 `PROXY TCP4 …` 當成一個畸形的 ClientHello。
#    ★ 它是唯一一條能分辨「讀對了地方」與「讀對了內容」的斷言。
R=$(python3 "$CLIENT" --port "$A_TLS" --tls --header v1:192.0.2.7:56324)
expect_line "★★★ HTTPS：PROXY 头在 ClientHello 之前，握手成功且地址是真客户端" \
  "STATUS=200 BODY=real-tls" "$R"

# ⚠ ⚠ **同一個產品行為，在兩條用戶端上長得不一樣**：明文那側讀到 0 位元組（`CLOSED`），
#   而 TLS 那側是 Python 的 ssl 在握手中途撞上 EOF（`UNEXPECTED_EOF_WHILE_READING`）。
#   ★ 那是**用戶端庫**的性質，不是產品的 —— 所以判據不能照抄明文那條的期望值。
#   ⇒ 判的是**那件真正要守的事：握手絕不能成功**；兩種「沒成功」的形狀都收，
#     而任何一個 `STATUS=` 都判紅（它意味著連接活下來了）。
R=$(python3 "$CLIENT" --port "$A_TLS" --tls)
case "$R" in
  STATUS=*)
    fail "★★ HTTPS：清单内不发头，而握手居然成功了（$R）—— 那条「必须发」的语义没生效" ;;
  CLOSED|*EOF*)
    ok "★★ HTTPS：清单内不发头 ⇒ 连接被关（TLS 也不例外，$R）" ;;
  *)
    fail "★★ HTTPS：清单内不发头，给出了没预料到的结果：$R" ;;
esac

# ⑩ ★ ★ 換設定立刻生效 —— 守的是 **D19** 那個形狀
#    （「改了設定、`POST /load` 回 200、而那條指令什麼都沒做」）。
#    ⚠ D19 本身已由 **G135** 結案（快取容量現在改得動），但**本條守的是形狀不是那個執行個體**：
#    它擋的是 `proxy_protocol_from`，與快取無關。⛔ 別因為 D19 結了就把這一格刪掉。
#    ⚠ 判據掛在**資料面**上，不是掛在「/load 回傳了 200」上。
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
  "http://localhost/load?overrides=clear" 2>/dev/null || echo "000")
if [ "$CODE" = "200" ]; then
  ok "管理面：把信任清单换成 10.0.0.0/8（不再含 127.0.0.1）"
else
  fail "全量 load 没成功（$CODE）：$(cat "$WORK/admin.out" 2>/dev/null)"
fi

# ★ ★ ★ 換完之後 127.0.0.1 **不再受信** ⇒ 它發的頭一個位元組都不該被讀，
#   於是那段文字變成請求行 ⇒ 壞請求。
#   ⚠ 若這裡仍然是 `BODY=real`，說明策略拿的是一份**裝載時的快照** ——
#     那就是 D19 那個形狀：設定改了、load 回了 200、而執行環境沒跟。
R=$(python3 "$CLIENT" --port "$A_HTTP" --header v1:192.0.2.7:56324)
case "$R" in
  "STATUS=200 BODY=real")
    # ⚠ ⚠ **這條訊息不許只說一個成因**：反證時實測過，把 fork 裡那道信任門整個繞開
    #   也會讓本條紅，而那時「拿的是快照」是一個**與事實不符的診斷**。
    #   ★ ⇒ 說出**觀測到的事實**，再列出兩條候選，讓讀的人自己去分 ——
    #     一條只名指一個成因的失敗訊息，會在它猜錯的那次把人帶到反方向去。
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
