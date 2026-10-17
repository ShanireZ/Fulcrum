#!/usr/bin/env bash
# 換代視窗內的 **QUIC 連接跨處理程序轉交**端到端（**M2 批 K**，G109 ②③④⑤）。
#
# ★ ★ ★ **它驗的是一句產品承諾**：「換代時長連接不斷」—— TCP 那側 M0 早就驗過，
#   而 h3 直到本批之前都**不成立**（`Ownership::Relay` 的處置是丟棄）。
#
# ⚠ ⚠ ★ **實測形態比 G109 ⑤ 寫的更強，而這一條要寫在明處。**
#   G109 ⑤ 把它寫成概率性的（「核心把資料報任意分給兩代之一，N=20 時漏檢約 1e-6」）。
#   實測**不是**：老一代收到 SIGQUIT 之後就**不再 `recv_from`**
#   （`quic/listener.rs` 那條與 L4 UDP 逐字相同的紀律）⇒ 換代之後屬於它的
#   **每一個**資料報都落到新一代手裡，**必須**經轉交才回得去。
#   ⚠ ⚠ **但那一段是從「老一代廣播停機」算起，不是從 SIGQUIT 算起** ——
#   中間有約 5 秒兩代都在收，核心隨便分。⇒ 請求序列必須**跨過**那 5 秒，
#   見下面 `N_REQ` 那一段。
#
# 連接埠（★ 與其餘場景全都錯開，見 AGENTS.md 那張連接埠表）：
#   9910 被測站台 —— **TLS**，於是 G110 讓同一個連接埠號上自動也聽 UDP（h3）
#   9911 **永遠沒人在聽**（`--http3-only` 那把尺子的自證要它）

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
PORT=${PORT:-9910}
DEAD_PORT=${DEAD_PORT:-9911}

# 同一條連接上要打幾個請求。
#
# ⚠ ⚠ ★ **這個數不是隨手定的。**
#
#   老一代收到 SIGQUIT 之後就**不再 `recv_from`** ⇒ **在那之後**，屬於它的
#   每一個資料報都落到新一代手裡，**必須**經轉交才回得去 ——
#   這比 G109 ⑤ 寫的那個概率性判據強得多。
#
#   ⚠ ⚠ **但「那之後」不是 SIGQUIT 那一刻** —— pingora 要先送 fd、
#   再等約 **5 秒**才廣播停機。這 5 秒裡**兩代都在收**，核心隨便分，
#   一串請求全落在裡面的話，轉交壞掉也可能碰巧通過。
#   ⇒ 取 N=20 @ 2/s（整串約 10s）、換代在第 2 秒 ⇒ **約有 6 秒的請求
#     落在老一代已經停止收包之後**，那一段是確定性的。
N_REQ=${N_REQ:-20}
# ⚠ ⚠ ★ ★ ★ **單位不能省。** `curl --rate N` 不帶單位時預設是**每小時**，不是每秒 ——
#   寫成 `--rate 4` 的話，一串請求之間會隔整整 **15 分鐘**，
#   判據掛到超時而**它紅的理由與轉交毫無關係**。
#   ★ ★ **一把沒自證過的尺子，會給出一個與被測物件無關的紅。**
RATE=${RATE:-2/s}
# 整串的硬時限（秒）。★ 正常路徑下 20 個請求 @ 2/s ≈ 10s；
#   ⚠ 轉交壞掉時每個失敗的請求要燒掉 `--max-time 5`，所以餘量要留夠，
#   否則**判據會先撞上自己的時限**，而那時它紅的理由說的是「超時」不是「轉交」。
SEQ_DEADLINE=${SEQ_DEADLINE:-75}

FAILS=0
PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
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
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 60 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -9 "$pid" 2>/dev/null || true
  done

  # ── ★ ★ ★ 收尾自證：本場景用過的連接埠，走的時候必須全部還回去 ──────────────
  #
  # ⚠ 它守的是**上面那個 `PIDS` 到底收全了沒有** —— 而那正是本場景栽過的地方：
  #   `GEN2=$(start_gen …)` 的 `$(…)` 是子 shell，`PIDS+=` 改的是副本，
  #   於是 kill 循環一個處理程序都沒收到，**而本場景照樣 PASSED**。
  #
  # ★ ★ **判據掛在「連接埠還回去了沒有」，不掛在「處理程序還在不在」**：
  #   前者才是下一個場景真正會被絆到的東西，而後者要先知道該找哪個 pid ——
  #   一個沒被登記的 pid，恰恰是這裡最找不到的。
  #
  # ★ ★ ★ **`:80` 必須在這張單子裡。** 它是自動 HTTPS 合成出來的重定向站台
  #   （見 AGENTS.md 連接埠表），本場景從來沒有顯式寫過它，也正因如此，
  #   洩漏的時候**沒有任何一處會提到它** —— 上一次的現場是「ACME 莫名其妙起不來」。
  # ⚠ `:80` 只有在**開跑時是空的**才算本場景漏的 —— 否則這道門會替別人受過，
  #   而一條指錯人的判據比沒有判據更費時間。
  local p leaked="" ports="$PORT"
  if [ "${PORT80_AT_START:-free}" = "free" ]; then ports="$ports 80"; fi
  for p in $ports; do
    waited=0
    while port_listening "$p" && [ "$waited" -lt 30 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    if port_listening "$p"; then leaked="$leaked $p"; fi
  done
  rm -rf "$WORK"

  if [ -n "$leaked" ]; then
    echo >&2
    echo "QUIC-RELAY TESTS FAILED: 收尾没干净 —— 退出时这些端口还有人在听：$leaked" >&2
    echo "  ⇒ 多半是某一代的 pid 没进 \$PIDS（经典写法：\`X=\$(start_gen …)\`，" >&2
    echo "     \$(…) 是子 shell，数组改了个副本）。" >&2
    echo "  ⚠ 这件事的后果**不在本场景**：泄漏的进程攥着 :80 活到下一个场景，" >&2
    echo "     而那边的基线不看 :80 ⇒ 现场会是「ACME 莫名其妙起不来」。" >&2
    # ★ 用 `pgrep -af` 而不是 `ps | grep`（同 tests/metrics/run.sh）：後者靠「把自己從
    #   結果裡濾掉」成立，那一步一旦被改動（比如換個匹配詞）就會安靜地把自己算進去；
    #   而這裡問的是「還活著的是哪幾個」—— 一個 defunct 處理程序攥不住監聽 socket。
    pgrep -af "fulcrum serve" >&2 || true
    exit 1
  fi
}
trap cleanup EXIT

port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}
wait_port() {
  local port=$1 tries=0
  while [ "$tries" -lt 150 ]; do
    if port_listening "$port"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

h3_get() {
  # $1 = 路徑。回「HTTP 碼體」兩段。
  curl -sS -k --http3-only --max-time 8 \
    --resolve "a.example:$PORT:$HOST" \
    -w ' %{http_code}' "https://a.example:$PORT$1" 2>/dev/null || echo " 000"
}

# ── [0/6] 基線 ──────────────────────────────────────────────────────────────
echo "=== [0/6] 基线：端口空着，而 --http3-only 这把尺子量得了东西 ==="
for p in "$PORT" "$DEAD_PORT"; do
  if port_listening "$p"; then
    echo "QUIC-RELAY TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
ok "两个端口都空着"

# ★ `:80` 在這裡**只記不判**：佔著它不是本場景的錯，也不該攔住本場景開跑
#   （它由自動 HTTPS 合成出來，好幾個場景共用 —— ⛔ 這裡有意不抄個數，
#    是哪幾個見 docs/platform/host-and-gate-traps.md 那張連接埠表）。
#   記下來是為了收尾時能分清「本場景漏了」與「進來時就有人佔著」。
PORT80_AT_START=free
if port_listening 80; then
  PORT80_AT_START=busy
  echo "  ⚠ 开跑时 :80 就有人在听 —— 收尾自证会跳过它（那不是本场景漏的）"
fi

# ⚠ ⚠ `--http3-only` 不許省成 `--http3`：後者在 QUIC 不通時**回落到 TCP**，
#   於是整格會在「h3 根本沒起來」時照樣綠。
# ★ 先拿一個沒人聽的連接埠自證這把尺子會失敗 —— 否則一個沒編進 QUIC 後端的 curl
#   會讓下面每一條都變成空轉。
if curl -sS --http3-only --max-time 5 -o /dev/null "https://$HOST:$DEAD_PORT/" >/dev/null 2>&1; then
  echo "QUIC-RELAY TESTS FAILED: 对着空端口 --http3-only 竟然成功了 —— 尺子量不了东西。" >&2
  exit 1
fi
ok "★ 空端口上 --http3-only 如期失败 ⇒ 它真的在走 QUIC，不是悄悄回落"

# ── 自簽證書 ────────────────────────────────────────────────────────────────
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1 || {
  echo "QUIC-RELAY TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ★ ★ **兩代給不同的回應體**，於是「這一條是誰服務的」是**回應本身**說的，
#   不需要去紀錄裡考古。⚠ 那也讓判據不依賴任何紀錄格式。
write_conf() {
  cat > "$WORK/fulcrum.conf" <<CONF
a.example:$PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 "$1"
}
CONF
}

# ⚠ ⚠ ⚠ **它把 pid 放進 `GEN_PID`，不 `echo` 出來 —— 而這不是風格問題。**
#
#   原來的寫法是「函式里 `PIDS+=($!)` 且 `echo $!`，呼叫方 `GEN1=$(start_gen gen1)`」。
#   `$(…)` 跑在**子 shell** 裡 ⇒ `PIDS+=` 改的是子 shell 那份副本，**父 shell 的 `PIDS`
#   永遠是空的**，於是 `cleanup` 一個處理程序都收不到：`echo` 出來的 pid 是對的，
#   陣列卻沒跟出來，兩件事只有一件成功，而失敗的那件沒有任何症狀。
#
# ★ ★ 後果不在本場景裡 —— 本場景照常 **PASSED**，洩漏的第二代處理程序活到下一個場景：
#   它攥著 `9910` 和**合成出來的 `:80`**（見 AGENTS.md 連接埠表），
#   而 ACME 場景的基線不看 `:80` ⇒ 現場是「ACME 莫名其妙起不來」。
#   ⇒ **一個場景的收尾漏了，紅的是別人。**
start_gen() {
  # $1 = 紀錄名，$2..= 額外參數；結果放在 GEN_PID 裡
  local name=$1
  shift
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/fulcrum.conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/fulcrum.pid" \
    --upgrade-sock "$WORK/upgrade.sock" \
    "$@" > "$WORK/$name.log" 2>&1 &
  GEN_PID=$!
  PIDS+=("$GEN_PID")
}

# ── [1/6] 第一代 ────────────────────────────────────────────────────────────
echo "=== [1/6] 起第一代（它的响应体是 gen1）==="
write_conf gen1
start_gen gen1
GEN1=$GEN_PID
wait_port "$PORT" || {
  echo "QUIC-RELAY TESTS FAILED: 第一代起不来。日志：" >&2
  cat "$WORK"/gen1.log >&2
  exit 1
}
ok "第一代起来了（pid=$GEN1）"

OUT=$(h3_get /)
# ⚠ 判據與報法**分開寫**，不要 `A && ok … || fail …`（SC2015，本儲存庫 tests/acme/lib.sh
#   記過同一條）：那串裡 `ok` 一旦失敗（stdout 被關掉就夠了），`fail` 會**跟著**執行
#   ⇒ 一條其實成立的判據把 FAILS 加了一。
if [ "${OUT##* }" = "200" ] && [ "${OUT%% *}" = "gen1" ]; then
  ok "h3 通了，而且是第一代在服务（$OUT）"
else
  fail "第一代的 h3 请求不对：$OUT"
fi

# ── [2/6] 轉交 socket 真的建出來了 ──────────────────────────────────────────
echo "=== [2/6] 转交 socket 建出来了，而且路径由 gen_id 推导 ==="
#
# ★ 代標識從裝載紀錄裡讀 —— 那一行是維運唯一看得見它的地方，
#   ⚠ 而「紀錄裡印的那個 gen」與「socket 路徑裡那個 gen」必須是同一個：
#     兩者分家的話，轉交會靜靜地發給一個不存在的地方。
GEN1_HEX=$(grep -aoE "gen=[0-9a-f]{16}" "$WORK/gen1.log" | head -1 | cut -d= -f2)
if [ -n "$GEN1_HEX" ]; then
  ok "装载日志里印出了本代的 gen（$GEN1_HEX）"
else
  fail "装载日志里没有 gen= —— 下面那条路径判据没有依据了"
fi
RELAY1="$WORK/quic-relay-$GEN1_HEX.sock"
if [ -S "$RELAY1" ]; then
  ok "★★ 转交 socket 在（$RELAY1），而路径完全由 gen_id 推导 —— 两代之间不需要任何协商"
else
  # ⚠ 現場清單用 `find … -printf` 而不是 `ls`（同 tests/ci/dump-cache.sh，SC2012）：
  #   `ls` 會按終端機與 locale 改主意，而這一行是排查這條判據時唯一的證據。
  fail "★★ 转交 socket 不在：$RELAY1（现有：$(find "$WORK" -mindepth 1 -maxdepth 1 -printf '%f ' 2>/dev/null))"
fi

# ── [3/6] 起一條**長跑**的 h3 請求序列 ──────────────────────────────────────
echo "=== [3/6] 后台起一串 h3 请求（同一条连接上跑，中途会换代）==="
#
# ⚠ ⚠ `--rate` 讓這一串**慢下來**，好讓換代發生在它中間。
#   ★ `%{num_connects}` 每個 URL 打一行 —— 它們的**和**必須是 1，
#     那才叫「同一條連接」。⇒ 沒有這一條的話，一個每次都重連的 curl
#     會讓整格全綠，而它證明的是「新一代能服務新連接」——**完全不是本格要驗的事**。
# ── ★ ★ ★ 先自證這把尺子：**不換代**時同一條連接能連打多個請求 ────────────
#
# ⚠ ⚠ 少了這一條，「換代之後失敗了」與「這條連線路徑本來就打不了第二個請求」
#   **在結果上長得一模一樣** —— 而後者與轉交毫無關係。
#   ★ 第一次排查正是被這一點繞了很久：以為是轉交沒接住，而實際上前幾個請求
#     在換代**之前**就已經失敗了。
# ⚠ ⚠ ★ ★ ★ **`-o` 與 `-w` 都是「按 URL 生效」的 —— 每個 URL 都要自己一份 `-o`。**
#   只給一個 `-o /dev/null` 的話，**從第二個 URL 起回應體會混進 `-w` 的輸出**：
#   實測拿到過 `gen1200 0`，而 `awk '$1=="200"'` 只認得第一行 ⇒ 判據報「9 個裡只成功 1 個」，
#   ★ 而那**完全是尺子的錯，被測的東西一直是好的**。
#   ⚠ 這條本儲存庫 §10 白紙黑字記過（當年拿到的是 `1smoke-ok0`）—— **又踩了一次**。
PRE=$(curl -sS -k --http3-only --max-time 8 --rate "$RATE"   --resolve "a.example:$PORT:$HOST"   -w '%{http_code} %{num_connects}
'   -o /dev/null "https://a.example:$PORT/p1"   -o /dev/null "https://a.example:$PORT/p2"   -o /dev/null "https://a.example:$PORT/p3"   2>/dev/null || true)
PRE_OK=$(printf '%s
' "$PRE" | awk '$1=="200"' | wc -l | tr -d ' ')
PRE_CONN=$(printf '%s
' "$PRE" | awk '{s+=$2} END {print s+0}')
if [ "$PRE_OK" = "3" ] && [ "$PRE_CONN" = "1" ]; then
  ok "★★★ 尺子自证：**不换代**时同一条 h3 连接上 3 个请求全成功（num_connects=1）"
else
  echo "QUIC-RELAY TESTS FAILED: 不换代时同一条连接就打不了 3 个请求" >&2
  echo "  （200 的个数=$PRE_OK，num_connects 之和=$PRE_CONN）" >&2
  echo "  ⇒ 这与转交无关，下面每一条都会变成空转 —— 本次结果不可采信。" >&2
  printf '%s
' "$PRE" >&2
  echo "--- gen1（后 60 行）---" >&2; tail -60 "$WORK/gen1.log" >&2 || true
  exit 1
fi

# ★ 每個 URL 前面都要有自己的 `-o /dev/null`（理由見上面那一段）。
URLS=()
for i in $(seq 1 "$N_REQ"); do
  URLS+=(-o /dev/null "https://a.example:$PORT/r$i")
done
# ⚠ ⚠ ★ **兩層時限，缺一不可。**
#   · `--max-time` 是**每一次傳輸**的上限 ⇒ N 個 URL 就是 N 份；
#   · 外面那層 `timeout` 才是**整串**的上限。
#   ★ 只有裡面那層的話，轉交沒接住時它會跑半小時 ——
#     一個要半小時才肯說「我紅了」的判據，與不報紅差不多。
(
  timeout "$SEQ_DEADLINE" curl -sS -k --http3-only --max-time 5 --rate "$RATE" \
    --resolve "a.example:$PORT:$HOST" \
    -w '%{http_code} %{num_connects}\n' \
    "${URLS[@]}" > "$WORK/seq.out" 2>"$WORK/seq.err"
  echo $? > "$WORK/seq.rc"
) &
SEQ=$!
PIDS+=("$SEQ")

# ── [4/6] 中途換代 ──────────────────────────────────────────────────────────
sleep 2
echo "=== [4/6] 换代：改配置 → SIGQUIT 第一代 → 以 -u 起第二代 ==="
write_conf gen2
kill -QUIT "$GEN1"
start_gen gen2 --upgrade
GEN2=$GEN_PID
sleep 2
if kill -0 "$GEN1" 2>/dev/null; then
  ok "换代窗口成立：第一代还活着（排空中），第二代已经起来（pid=$GEN2）"
else
  fail "第一代太早退出了 —— 转交没有对象，本格量不到东西"
fi

# ── [5/6] ★★★ 核心判據 ─────────────────────────────────────────────────────
echo "=== [5/6] ★★★ 那条连接活过了换代，而且一直由**第一代**服务 ==="
wait "$SEQ" 2>/dev/null || true
RC=$(cat "$WORK/seq.rc" 2>/dev/null || echo 99)
LINES=$(wc -l < "$WORK/seq.out" 2>/dev/null | tr -d ' ')
OK200=$(awk '$1=="200"' "$WORK/seq.out" 2>/dev/null | wc -l | tr -d ' ')
CONNECTS=$(awk '{s+=$2} END {print s+0}' "$WORK/seq.out" 2>/dev/null)

if [ "$LINES" = "$N_REQ" ]; then
  ok "$N_REQ 个请求都有结果"
else
  fail "只拿到 $LINES / $N_REQ 行结果（curl rc=$RC；124 = 整串超时）：$(head -3 "$WORK/seq.err" 2>/dev/null | tr '\n' ' ')"
fi
if [ "$OK200" = "$N_REQ" ]; then
  ok "★★★ $N_REQ 个请求**全部 200** —— 换代窗口内这条 h3 连接一次都没断"
else
  fail "★★★ 只有 $OK200 / $N_REQ 个请求成功 —— 转交没接住（这正是批 K 之前的行为）"
fi
if [ "$CONNECTS" = "1" ]; then
  ok "★★★ num_connects 之和 = 1 ⇒ $N_REQ 个请求真的在**同一条连接**上"
else
  fail "★★★ num_connects 之和 = $CONNECTS（期望 1）—— 它们不在同一条连接上，上面那条 200 证明不了转交"
fi

# ── [6/6] 反向 ──────────────────────────────────────────────────────────────
echo "=== [6/6] 反向：新连接归第二代；老一代退出后它的 socket 也没了 ==="
# ⚠ ⚠ ★ **不能斷言「換代之後新開的連接歸第二代」。**
#   重疊視窗裡**兩代都在 `recv_from` 同一個 fd**，核心把新連接的首包分給誰都行 ——
#   老一代收下它、回 Retry、建連，是**完全合法**的（它還沒收到停機信號）。
#   ⇒ 那樣一條斷言在一次正確的執行裡也會紅，而**一道只會攔住正確產出的判據，
#     和不報紅一樣壞**。★ 真正說得準的那一刻是**老一代退出之後**，即下面那條。

waited=0
while kill -0 "$GEN1" 2>/dev/null && [ "$waited" -lt 400 ]; do
  sleep 0.1
  waited=$((waited + 1))
done
if kill -0 "$GEN1" 2>/dev/null; then
  fail "第一代过了 40 秒还没退出"
else
  ok "第一代排空之后自己退出了"
  if [ -e "$RELAY1" ]; then
    fail "★★ 第一代退出了，而它的转交 socket 还留在 $RELAY1 —— 每换一次代泄漏一个文件"
  else
    ok "★★ 第一代退出时把自己的转交 socket unlink 掉了（G109 ③）"
  fi
fi

OUT=$(h3_get /)
if [ "${OUT%% *}" = "gen2" ]; then
  ok "老一代走了之后，服务照常（$OUT）"
else
  fail "老一代走了之后服务坏了：$OUT"
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "QUIC-RELAY TESTS FAILED：$FAILS 条断言没过。" >&2
  # ⚠ ⚠ ★ **兩側的轉交計數要單獨打出來** —— 排查這一格時，
  #   「發出去幾個」與「收到幾個」是**兩個不同的數**，而只看其中一個會得出反的結論。
  #   ★ 第一次排查正是栽在這裡：只 `tail -20` 看了尾巴，就斷言「老一代沒收到」，
  #     而那幾行早被截掉了。⇒ **別在半份證據上推。**
  echo "--- 转交计数 ---" >&2
  for g in gen1 gen2; do
    printf '  %s: 发出 %s · 收到 %s · 没人认领 %s · 拆不开 %s · 对方已退出 %s
'       "$g"       "$(grep -ac '的包转交给了' "$WORK/$g.log" 2>/dev/null || echo 0)"       "$(grep -ac '已送进那条连接' "$WORK/$g.log" 2>/dev/null || echo 0)"       "$(grep -ac '没人认领' "$WORK/$g.log" 2>/dev/null || echo 0)"       "$(grep -ac '拆不开' "$WORK/$g.log" 2>/dev/null || echo 0)"       "$(grep -ac '而那一代已经退出' "$WORK/$g.log" 2>/dev/null || true)" >&2
    grep -a '换代转交口\|建不出换代转交口\|HTTP/3，gen=' "$WORK/$g.log" 2>/dev/null | sed "s/^/  $g: /" >&2 || true
  done
  echo "--- gen1（后 60 行）---" >&2; tail -60 "$WORK/gen1.log" >&2 || true
  echo "--- gen2（后 60 行）---" >&2; tail -60 "$WORK/gen2.log" >&2 || true
  echo "--- curl ---" >&2; cat "$WORK/seq.out" 2>/dev/null >&2 || true
  echo "--- curl stderr ---" >&2; head -10 "$WORK/seq.err" 2>/dev/null >&2 || true
  exit 1
fi
echo "QUIC-RELAY TESTS PASSED —— 换代窗口内 h3 连接跨进程转交真的在跑（尺子自证 · 同一条连接 $N_REQ 个请求全部 200 · num_connects=1 · 老一代退出后服务照常 · 退出时 unlink）。"
