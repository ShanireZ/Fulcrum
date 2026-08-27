#!/usr/bin/env bash
# 未被认领的继承 fd 会怎样 —— 补上 M0 只验了一面的那个缺口。
# 这个脚本在容器里跑（G26），退出码即结论。
#
# ★ 背景：M0 证明的是**认领成功**那条路（自建监听器能取到 fd、升级零中断）。
#   上游 `f82478ae`（尚未发版）指出另一面：**没被认领的继承 fd 会黑洞化连接**。
#   见 docs/verification/open-seams.md。
#
# ★ ★ **本脚本验的是「当前（未修）行为」，因此它的绿是「坏行为已复现」。**
#   等上游发布含 `listen_addresses()` 清理机制的版本、fork rebase 上去之后，
#   这里的断言要**反过来写**（届时应当验「老 fd 确实被关掉了」）。
#   到那天这个脚本会红——**那是它在正确地报告口径变了，不是它坏了。**
#
# 造法：gen2 用 `M0_DROP_RAW_TCP=1` 启动，模拟「配置里删掉了 raw-tcp 这个监听器」，
# 于是 gen1 传来的 8081 那个 fd 没有任何服务去认领。
set -euo pipefail

RUN=${RUN:-/w/run/m0-unclaimed}
BIN=${BIN:-/w/target/release}
CONF=${CONF:-/w/conf/m0-unclaimed.yaml}
PORT=8081
# ★ 与 spike 的 `bind_host()` 默认保持一致（回环）。判据里出现的地址不能写死——
#   spike 从 0.0.0.0 收紧到 127.0.0.1，写死的 grep 会当场全部失配，
#   而失配的方向是「找不到 = 判红」，看起来像功能坏了，其实只是地址变了。
BIND_HOST=${M0_BIND_HOST:-127.0.0.1}
export M0_BIND_HOST="$BIND_HOST"

export RUST_LOG=${RUST_LOG:-info}

mkdir -p "$RUN"
rm -f "$RUN"/*.pid "$RUN"/*.sock "$RUN"/*.log "$RUN"/*.pids

LOG="$RUN/error.log"

fail() { echo; echo "UNCLAIMED FAILED: $*"; echo "--- server log ---"; cat "$LOG" 2>/dev/null || true; exit 1; }

# ── 收尾 ────────────────────────────────────────────────────────────────────
# 规则与全部理由在 lifecycle.sh 里。对本场景尤其要紧：结束时最后一代手里正持着 8081 上
# 那个**孤儿 LISTEN socket**，留着它，同一容器里后续用到这批端口的场景都会撞 EADDRINUSE。
# ★ 找不到库就必须炸，不能默默不收。
LIFECYCLE_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lifecycle.sh"
[ -f "$LIFECYCLE_LIB" ] || { echo "找不到 $LIFECYCLE_LIB —— 收尾逻辑缺失，拒绝继续。" >&2; exit 1; }
# shellcheck source=tests/m0/lifecycle.sh
. "$LIFECYCLE_LIB"
lifecycle_init "$RUN"
trap lifecycle_cleanup EXIT

# ★ 端口是否有人 LISTEN —— 读 /proc/net/tcp{,6}，**不依赖 bash 的 /dev/tcp**。
#   /dev/tcp 是编译期特性（--enable-net-redirections），精简镜像里可能没有；
#   之前这里写过一句「自证它可用」的检查，实际测的是 /dev/null 且结果被丢弃——
#   是个假的守卫。改成两套互不依赖的机制，还能互相交叉验证（见 [2/6]）。
#
# ★ ★ **两个文件都要扫。** tcp6 里放的是全部 AF_INET6 socket，双栈监听 `[::]:port`
#   **只**出现在那里，/proc/net/tcp 一行都没有（在本镜像里实测过）。
#   当前 spike 绑的是 127.0.0.1（纯 IPv4），所以只读 tcp 侥幸没错；而双栈是绝大多数
#   网络库的默认绑法，M1 的真监听器一旦绑 `[::]`，这个函数就会**恒假**——
#   于是 [0/6] 的前置检查静默通过，回到它本来就是为了防的那件事：
#   **基线对着别人的进程变绿**。
PROC_NET_FILES=(/proc/net/tcp /proc/net/tcp6)
port_listening() {
  local hex f seen=0
  hex=$(printf '%04X' "$1")
  for f in "${PROC_NET_FILES[@]}"; do
    [ -r "$f" ] || continue
    seen=1
    # local_address 是 `0100007F:1F90`（v4，8 位十六进制）或
    # `00000000000000000000000000000000:1FA3`（v6，32 位）。端口段的取法一样，
    # 所以只锚「冒号之后就是端口且到词尾」，不关心前面多长。$4 == 0A 即 TCP_LISTEN。
    if awk -v want="$hex" \
         '$4 == "0A" && $2 ~ ("^[0-9A-Fa-f]+:" want "$") { found = 1 } END { exit !found }' "$f"
    then
      return 0
    fi
  done
  # ★ 读不到就必须炸，不能当成「没人监听」。fail-open 的守卫是本项目已经栽过的形状。
  [ "$seen" = 1 ] || fail "${PROC_NET_FILES[*]} 一个都读不到——无法判断端口占用，本脚本的判据全部失效"
  return 1
}

# ★ port_listening 在 [0/6] 只被用来**放行**，所以一个恒假的实现会让那道门静默通过。
#   开跑前先拿固定样本证明它两个方向都认得——这就是 AGENTS.md 说的「门自带反向测试」。
#
#   ★ 样本是**真实内核输出**，不是照着文档编的：在构建镜像里让 python 绑
#     `[::]:8099`（=0x1FA3）之后，从 /proc/net/tcp6 原样抄下来的；同一时刻 /proc/net/tcp
#     除表头外**一行都没有**。这份样本本身就是本次盲区的实物证据。
selftest_port_listening() {
  local dir saved=("${PROC_NET_FILES[@]}")
  dir=$(mktemp -d)
  {
    echo '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
    echo '   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0'
    echo '   1: 0100007F:1F91 0100007F:8000 01 00000000:00000000 00:00000000 00000000     0        0 12346 1 0000000000000000 20 0 0 10 -1'
  } > "$dir/tcp"
  {
    echo '  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
    echo '   0: 00000000000000000000000000000000:1FA3 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 52691 1 00000000c0a6790d 100 0 0 10 0'
  } > "$dir/tcp6"

  PROC_NET_FILES=("$dir/tcp" "$dir/tcp6")
  local bad=""
  port_listening 8080 || bad="$bad 8080(v4-LISTEN 应为真)"
  # ★ 这一条就是本次要修的那个盲区：8099 只在 tcp6 里。旧实现在这里必红。
  port_listening 8099 || bad="$bad 8099(v6-LISTEN 应为真)"
  ! port_listening 8081 || bad="$bad 8081(状态是 ESTABLISHED 不是 LISTEN，应为假)"
  ! port_listening 9000 || bad="$bad 9000(压根不在样本里，应为假)"
  PROC_NET_FILES=("${saved[@]}")
  rm -rf "$dir"

  [ -z "$bad" ] || fail "port_listening 自测未通过：$bad
       它是 [0/6] 唯一的判据，且只用于放行——先修它，后面所有结论都不可信。"
  echo "  ✓ port_listening 自测通过（v4/v6 各一条 LISTEN 认得出，非 LISTEN 与不存在的端口都判假）"
}

probe_connect() { timeout 2 bash -c "exec 3<>/dev/tcp/$BIND_HOST/$1" 2>/dev/null; }
# 发一个字节并等回声。回来了 → 0；连得上但没回应 → 124（timeout）；连不上 → 非 0 且非 124
probe_echo() {
  timeout 3 bash -c "
    exec 3<>/dev/tcp/$BIND_HOST/$1 || exit 7
    printf 'ping' >&3
    head -c 4 <&3
  " 2>/dev/null
}

# ★ 开跑前端口必须是干净的。
#   第一次合并进全量跑时就栽在这里：M0 场景的进程还没走干净、占着 8081，
#   本场景的 gen1 绑不上（EADDRINUSE），而**第 2 步的基线回声照样是绿的**——
#   因为它对着的是别人的进程。判据必须能把「是我们自己的进程在服务」钉死。
echo "=== [0/6] 前置：判据先自证，然后端口必须干净 ==="
selftest_port_listening
if port_listening "$PORT"; then
  fail "开跑前 $PORT 上就已经有人在 LISTEN（据 ${PROC_NET_FILES[*]}）——多半是上一个场景的进程没退干净。
       本场景要求端口干净，否则基线会对着别人的进程变绿。"
fi
echo "  ✓ $PORT 无人监听"

echo "=== [1/6] 起第一代（raw-tcp 正常挂着）==="
"$BIN/m0-seam" -c "$CONF" -d
sleep 1
[ -f "$RUN/m0.pid" ] || fail "第一代没有写出 pid 文件"
GEN1=$(cat "$RUN/m0.pid")
record_gen "$GEN1"
echo "gen1 pid = $GEN1"

echo "=== [2/6] 基线：raw-tcp 确实在工作，且**是我们这一代在服务** ==="
# ★ 两条判据缺一不可：
#   ① 我们自己的 gen1 确实 bind 成功并把 fd 注册进了表（看**我们的** error.log）
#   ② 回声通
#   只有 ② 的话，别人的进程也能让它变绿——那正是第一次合并跑时踩的坑。
grep -q "\[raw-tcp\] bound fresh on $BIND_HOST:$PORT, registered fd=" "$LOG" \
  || fail "第一代的 raw-tcp 没有成功 bind 并注册 fd（看 $LOG）。
       端口八成被别的进程占着——那样后面所有判据都是假的。"

# ★ 两套机制在这里交叉验证：/proc 说它在 LISTEN，而 /dev/tcp 连不上，
#   那就是本 shell 不支持 /dev/tcp，而不是服务的问题。把这两种情形分开报，
#   免得把缺失的 shell 特性误读成「行为变了」。
port_listening "$PORT" || fail "/proc/net/tcp 里看不到 $PORT 在 LISTEN，但日志说已经 bind 了——自相矛盾，需要人工看 $LOG"
OUT=$(probe_echo "$PORT" || true)
if [ "$OUT" != "ping" ]; then
  fail "基线不通（收到 '$OUT'）。★ 注意 /proc 显示 $PORT 确实在 LISTEN，
       所以这更像是**本 shell 不支持 /dev/tcp**（编译期特性），而不是服务出问题。"
fi
echo "  ✓ 我们这一代已注册 fd、端口在 LISTEN、回声正常（收到 '$OUT'）"

echo "=== [3/6] 升级到第二代，但**丢掉** raw-tcp 服务 ==="
kill -QUIT "$GEN1"
M0_DROP_RAW_TCP=1 "$BIN/m0-seam" -c "$CONF" -d -u
sleep 2
GEN2=$(cat "$RUN/m0.pid")
record_gen "$GEN2"
echo "gen2 pid = $GEN2"
[ "$GEN1" != "$GEN2" ] || fail "pid 没变，根本没换进程"

echo "=== [4/6] 那个 fd 还在不在表里 ==="
grep -q "\[fd-inspect\] entry key=m0-raw-tcp:$BIND_HOST:$PORT" "$LOG" \
  || fail "第二代的 fd 表里没有 m0-raw-tcp 的条目——本轮的前提不成立"

# ★ 判据锚在 pingora 自己打的启动顺序行上，而不是 spike 自证的那句 WARN——
#   那句在 daemonize **之前**打，进的是启动 shell 的 stdout，不在 error.log 里。
LAST_ORDER=$(grep "Starting services in dependency order" "$LOG" | tail -1)
echo "  第二代启动的服务：$LAST_ORDER"
case "$LAST_ORDER" in
  *m0-raw-tcp*) fail "第二代并没有真的丢掉 raw-tcp 服务" ;;
esac
echo "  ✓ fd 仍在表里，而**没有任何服务认领它**"

echo "=== [5/6] ★ 核心判据：连得上，但永远没有回应 ==="
# ★ ★ 必须先等第一代真正退出，否则测的是它而不是孤儿 socket。
#   pingora 在发完 fd 后要硬等 CLOSE_TIMEOUT（5 秒，server/mod.rs:59）才广播停机，
#   这段时间**两代都持有同一个监听 socket，而老一代照常 accept**。
#   第一次写这个脚本时就是在这个窗口里探的，结果收到了正常回声——
#   不是行为不对，是判据取早了。
echo "  等第一代退出（pid=$GEN1）..."
for _ in $(seq 1 40); do
  kill -0 "$GEN1" 2>/dev/null || break
  sleep 1
done
if kill -0 "$GEN1" 2>/dev/null; then
  fail "第一代 40 秒还没退出，无法在干净状态下判断黑洞化"
fi
echo "  ✓ 第一代已退出，现在 $PORT 上只剩第二代那个**没人认领**的 fd"
if probe_connect "$PORT"; then
  echo "  ✓ TCP 连接**成功建立**——孤儿 socket 仍在 LISTEN，内核照常完成三次握手"
else
  fail "连不上 $PORT。孤儿 fd 本应还持着这个端口；若这里连不上，说明行为已变（可能上游修复已生效），断言口径需要重写"
fi

set +e
probe_echo "$PORT" > "$RUN/echo.out" 2>/dev/null
RC=$?
set -e
if [ "$RC" -eq 124 ]; then
  echo "  ✓ ★ 发出请求后**超时无回应**（exit 124）——这就是黑洞化"
elif [ "$RC" -eq 0 ]; then
  fail "居然收到了回应（'$(cat "$RUN/echo.out")'）。说明有人在 accept——与'未被认领'的前提矛盾"
else
  fail "既不是超时也不是回应，exit=$RC。行为与预期不同，需要人工看 $LOG"
fi

echo "=== [6/6] 它会不会继续传给第三代 ==="
# ★ 用「起第三代前后的计数增量」判，不用绝对值：
#   探查服务在别的服务注册 fd **之前**启动，所以第一代那次它看到的是空表——
#   绝对计数会随服务启动顺序变化，增量不会。
BEFORE=$(grep -c "\[fd-inspect\] entry key=m0-raw-tcp:$BIND_HOST:$PORT" "$LOG" || true)
kill -QUIT "$GEN2"
M0_DROP_RAW_TCP=1 "$BIN/m0-seam" -c "$CONF" -d -u
sleep 2
GEN3=$(cat "$RUN/m0.pid")
record_gen "$GEN3"
echo "gen3 pid = $GEN3"
[ "$GEN2" != "$GEN3" ] || fail "pid 没变，第三代没起来"
AFTER=$(grep -c "\[fd-inspect\] entry key=m0-raw-tcp:$BIND_HOST:$PORT" "$LOG" || true)
[ "$AFTER" -gt "$BEFORE" ] \
  || fail "第三代的 fd 表里没有那个孤儿条目（计数 $BEFORE → $AFTER，没有增长）"
echo "  ✓ 孤儿 fd **被原样传给了第三代**（计数 $BEFORE → $AFTER）——它不会自己消失"

echo
echo "UNCLAIMED REPRODUCED —— 未被认领的继承 fd 保持 LISTEN、吞掉连接、并逐代传递。"
echo "★ 这是**当前未修行为的复现**，不是回归。上游修复（listen_addresses()）发版后，"
echo "  本脚本的断言要反过来写；届时它变红是口径变了，不是它坏了。见 docs/verification/open-seams.md。"
