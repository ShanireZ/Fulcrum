#!/usr/bin/env bash
# 换代窗口内的 **QUIC 连接跨进程转交**端到端（**M2 批 K**，G109 ②③④⑤）。
#
# ★ ★ ★ **它验的是一句产品承诺**：「换代时长连接不断」—— TCP 那侧 M0 早就验过，
#   而 h3 直到本批之前都**不成立**（`Ownership::Relay` 的处置是丢弃）。
#
# ⚠ ⚠ ★ **实测形态比 G109 ⑤ 写的更强，而这一条要写在明处。**
#   G109 ⑤ 把它写成概率性的（「内核把数据报任意分给两代之一，N=20 时漏检约 1e-6」）。
#   实测**不是**：老一代收到 SIGQUIT 之后就**不再 `recv_from`**
#   （`quic/listener.rs` 那条与 L4 UDP 逐字相同的纪律）⇒ 换代之后属于它的
#   **每一个**数据报都落到新一代手里，**必须**经转交才回得去。
#   ⚠ ⚠ **但那一段是从「老一代广播停机」算起，不是从 SIGQUIT 算起** ——
#   中间有约 5 秒两代都在收，内核随便分。⇒ 请求序列必须**跨过**那 5 秒，
#   见下面 `N_REQ` 那一段。
#
# 端口（★ 与其余场景全都错开，见 AGENTS.md 那张端口表）：
#   9910 被测站点 —— **TLS**，于是 G110 让同一个端口号上自动也听 UDP（h3）
#   9911 **永远没人在听**（`--http3-only` 那把尺子的自证要它）

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
PORT=${PORT:-9910}
DEAD_PORT=${DEAD_PORT:-9911}

# 同一条连接上要打几个请求。
#
# ⚠ ⚠ ★ **这个数不是随手定的。**
#
#   老一代收到 SIGQUIT 之后就**不再 `recv_from`** ⇒ **在那之后**，属于它的
#   每一个数据报都落到新一代手里，**必须**经转交才回得去 ——
#   这比 G109 ⑤ 写的那个概率性判据强得多。
#
#   ⚠ ⚠ **但「那之后」不是 SIGQUIT 那一刻** —— pingora 要先送 fd、
#   再等约 **5 秒**才广播停机。这 5 秒里**两代都在收**，内核随便分，
#   一串请求全落在里面的话，转交坏掉也可能碰巧通过。
#   ⇒ 取 N=20 @ 2/s（整串约 10s）、换代在第 2 秒 ⇒ **约有 6 秒的请求
#     落在老一代已经停止收包之后**，那一段是确定性的。
N_REQ=${N_REQ:-20}
# ⚠ ⚠ ★ ★ ★ **单位不能省。** `curl --rate N` 不带单位时默认是**每小时**，不是每秒 ——
#   写成 `--rate 4` 的话，一串请求之间会隔整整 **15 分钟**，
#   判据挂到超时而**它红的理由与转交毫无关系**。
#   ★ ★ **一把没自证过的尺子，会给出一个与被测对象无关的红。**
RATE=${RATE:-2/s}
# 整串的硬时限（秒）。★ 正常路径下 20 个请求 @ 2/s ≈ 10s；
#   ⚠ 转交坏掉时每个失败的请求要烧掉 `--max-time 5`，所以余量要留够，
#   否则**判据会先撞上自己的时限**，而那时它红的理由说的是「超时」不是「转交」。
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

  # ── ★ ★ ★ 收尾自证：本场景用过的端口，走的时候必须全部还回去 ──────────────
  #
  # ⚠ 它守的是**上面那个 `PIDS` 到底收全了没有** —— 而那正是本场景栽过的地方：
  #   `GEN2=$(start_gen …)` 的 `$(…)` 是子 shell，`PIDS+=` 改的是副本，
  #   于是 kill 循环一个进程都没收到，**而本场景照样 PASSED**。
  #
  # ★ ★ **判据挂在「端口还回去了没有」，不挂在「进程还在不在」**：
  #   前者才是下一个场景真正会被绊到的东西，而后者要先知道该找哪个 pid ——
  #   一个没被登记的 pid，恰恰是这里最找不到的。
  #
  # ★ ★ ★ **`:80` 必须在这张单子里。** 它是自动 HTTPS 合成出来的重定向站点
  #   （见 AGENTS.md 端口表），本场景从来没有显式写过它，也正因如此，
  #   泄漏的时候**没有任何一处会提到它** —— 上一次的现场是「ACME 莫名其妙起不来」。
  # ⚠ `:80` 只有在**开跑时是空的**才算本场景漏的 —— 否则这道门会替别人受过，
  #   而一条指错人的判据比没有判据更费时间。
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
    # ★ 用 `pgrep -af` 而不是 `ps | grep`（同 tests/metrics/run.sh）：后者靠「把自己从
    #   结果里滤掉」成立，那一步一旦被改动（比如换个匹配词）就会安静地把自己算进去；
    #   而这里问的是「还活着的是哪几个」—— 一个 defunct 进程攥不住监听 socket。
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
  # $1 = 路径。回「HTTP 码体」两段。
  curl -sS -k --http3-only --max-time 8 \
    --resolve "a.example:$PORT:$HOST" \
    -w ' %{http_code}' "https://a.example:$PORT$1" 2>/dev/null || echo " 000"
}

# ── [0/6] 基线 ──────────────────────────────────────────────────────────────
echo "=== [0/6] 基线：端口空着，而 --http3-only 这把尺子量得了东西 ==="
for p in "$PORT" "$DEAD_PORT"; do
  if port_listening "$p"; then
    echo "QUIC-RELAY TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
ok "两个端口都空着"

# ★ `:80` 在这里**只记不判**：占着它不是本场景的错，也不该拦住本场景开跑
#   （它由自动 HTTPS 合成出来，七个场景共用，见 AGENTS.md 端口表）。
#   记下来是为了收尾时能分清「本场景漏了」与「进来时就有人占着」。
PORT80_AT_START=free
if port_listening 80; then
  PORT80_AT_START=busy
  echo "  ⚠ 开跑时 :80 就有人在听 —— 收尾自证会跳过它（那不是本场景漏的）"
fi

# ⚠ ⚠ `--http3-only` 不许省成 `--http3`：后者在 QUIC 不通时**回落到 TCP**，
#   于是整格会在「h3 根本没起来」时照样绿。
# ★ 先拿一个没人听的端口自证这把尺子会失败 —— 否则一个没编进 QUIC 后端的 curl
#   会让下面每一条都变成空转。
if curl -sS --http3-only --max-time 5 -o /dev/null "https://$HOST:$DEAD_PORT/" >/dev/null 2>&1; then
  echo "QUIC-RELAY TESTS FAILED: 对着空端口 --http3-only 竟然成功了 —— 尺子量不了东西。" >&2
  exit 1
fi
ok "★ 空端口上 --http3-only 如期失败 ⇒ 它真的在走 QUIC，不是悄悄回落"

# ── 自签证书 ────────────────────────────────────────────────────────────────
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1 || {
  echo "QUIC-RELAY TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ★ ★ **两代给不同的响应体**，于是「这一条是谁服务的」是**响应本身**说的，
#   不需要去日志里考古。⚠ 那也让判据不依赖任何日志格式。
write_conf() {
  cat > "$WORK/fulcrum.conf" <<CONF
a.example:$PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 "$1"
}
CONF
}

# ⚠ ⚠ ⚠ **它把 pid 放进 `GEN_PID`，不 `echo` 出来 —— 而这不是风格问题。**
#
#   原来的写法是「函数里 `PIDS+=($!)` 且 `echo $!`，调用方 `GEN1=$(start_gen gen1)`」。
#   `$(…)` 跑在**子 shell** 里 ⇒ `PIDS+=` 改的是子 shell 那份副本，**父 shell 的 `PIDS`
#   永远是空的**，于是 `cleanup` 一个进程都收不到：`echo` 出来的 pid 是对的，
#   数组却没跟出来，两件事只有一件成功，而失败的那件没有任何症状。
#
# ★ ★ 后果不在本场景里 —— 本场景照常 **PASSED**，泄漏的第二代进程活到下一个场景：
#   它攥着 `9910` 和**合成出来的 `:80`**（见 AGENTS.md 端口表），
#   而 ACME 场景的基线不看 `:80` ⇒ 现场是「ACME 莫名其妙起不来」。
#   ⇒ **一个场景的收尾漏了，红的是别人。**
start_gen() {
  # $1 = 日志名，$2..= 额外参数；结果放在 GEN_PID 里
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
# ⚠ 判据与报法**分开写**，不要 `A && ok … || fail …`（SC2015，本仓库 tests/acme/lib.sh
#   记过同一条）：那串里 `ok` 一旦失败（stdout 被关掉就够了），`fail` 会**跟着**执行
#   ⇒ 一条其实成立的判据把 FAILS 加了一。
if [ "${OUT##* }" = "200" ] && [ "${OUT%% *}" = "gen1" ]; then
  ok "h3 通了，而且是第一代在服务（$OUT）"
else
  fail "第一代的 h3 请求不对：$OUT"
fi

# ── [2/6] 转交 socket 真的建出来了 ──────────────────────────────────────────
echo "=== [2/6] 转交 socket 建出来了，而且路径由 gen_id 推导 ==="
#
# ★ 代标识从装载日志里读 —— 那一行是运维唯一看得见它的地方，
#   ⚠ 而「日志里印的那个 gen」与「socket 路径里那个 gen」必须是同一个：
#     两者分家的话，转交会静静地发给一个不存在的地方。
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
  # ⚠ 现场清单用 `find … -printf` 而不是 `ls`（同 tests/ci/dump-cache.sh，SC2012）：
  #   `ls` 会按终端与 locale 改主意，而这一行是排查这条判据时唯一的证据。
  fail "★★ 转交 socket 不在：$RELAY1（现有：$(find "$WORK" -mindepth 1 -maxdepth 1 -printf '%f ' 2>/dev/null))"
fi

# ── [3/6] 起一条**长跑**的 h3 请求序列 ──────────────────────────────────────
echo "=== [3/6] 后台起一串 h3 请求（同一条连接上跑，中途会换代）==="
#
# ⚠ ⚠ `--rate` 让这一串**慢下来**，好让换代发生在它中间。
#   ★ `%{num_connects}` 每个 URL 打一行 —— 它们的**和**必须是 1，
#     那才叫「同一条连接」。⇒ 没有这一条的话，一个每次都重连的 curl
#     会让整格全绿，而它证明的是「新一代能服务新连接」——**完全不是本格要验的事**。
# ── ★ ★ ★ 先自证这把尺子：**不换代**时同一条连接能连打多个请求 ────────────
#
# ⚠ ⚠ 少了这一条，「换代之后失败了」与「这条链路本来就打不了第二个请求」
#   **在结果上长得一模一样** —— 而后者与转交毫无关系。
#   ★ 第一次排查正是被这一点绕了很久：以为是转交没接住，而实际上前几个请求
#     在换代**之前**就已经失败了。
# ⚠ ⚠ ★ ★ ★ **`-o` 与 `-w` 都是「按 URL 生效」的 —— 每个 URL 都要自己一份 `-o`。**
#   只给一个 `-o /dev/null` 的话，**从第二个 URL 起响应体会混进 `-w` 的输出**：
#   实测拿到过 `gen1200 0`，而 `awk '$1=="200"'` 只认得第一行 ⇒ 判据报「9 个里只成功 1 个」，
#   ★ 而那**完全是尺子的错，被测的东西一直是好的**。
#   ⚠ 这条本仓库 §10 白纸黑字记过（当年拿到的是 `1smoke-ok0`）—— **又踩了一次**。
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

# ★ 每个 URL 前面都要有自己的 `-o /dev/null`（理由见上面那一段）。
URLS=()
for i in $(seq 1 "$N_REQ"); do
  URLS+=(-o /dev/null "https://a.example:$PORT/r$i")
done
# ⚠ ⚠ ★ **两层时限，缺一不可。**
#   · `--max-time` 是**每一次传输**的上限 ⇒ N 个 URL 就是 N 份；
#   · 外面那层 `timeout` 才是**整串**的上限。
#   ★ 只有里面那层的话，转交没接住时它会跑半小时 ——
#     一个要半小时才肯说「我红了」的判据，与不报红差不多。
(
  timeout "$SEQ_DEADLINE" curl -sS -k --http3-only --max-time 5 --rate "$RATE" \
    --resolve "a.example:$PORT:$HOST" \
    -w '%{http_code} %{num_connects}\n' \
    "${URLS[@]}" > "$WORK/seq.out" 2>"$WORK/seq.err"
  echo $? > "$WORK/seq.rc"
) &
SEQ=$!
PIDS+=("$SEQ")

# ── [4/6] 中途换代 ──────────────────────────────────────────────────────────
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

# ── [5/6] ★★★ 核心判据 ─────────────────────────────────────────────────────
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
# ⚠ ⚠ ★ **不能断言「换代之后新开的连接归第二代」。**
#   重叠窗口里**两代都在 `recv_from` 同一个 fd**，内核把新连接的首包分给谁都行 ——
#   老一代收下它、回 Retry、建连，是**完全合法**的（它还没收到停机信号）。
#   ⇒ 那样一条断言在一次正确的运行里也会红，而**一道只会拦住正确产出的判据，
#     和不报红一样坏**。★ 真正说得准的那一刻是**老一代退出之后**，即下面那条。

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
  # ⚠ ⚠ ★ **两侧的转交计数要单独打出来** —— 排查这一格时，
  #   「发出去几个」与「收到几个」是**两个不同的数**，而只看其中一个会得出反的结论。
  #   ★ 第一次排查正是栽在这里：只 `tail -20` 看了尾巴，就断言「老一代没收到」，
  #     而那几行早被截掉了。⇒ **别在半份证据上推。**
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
