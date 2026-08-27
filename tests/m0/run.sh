#!/usr/bin/env bash
# M0 接缝验证：一次 SIGQUIT + `-u` 优雅升级，三类流量零中断。
# 这个脚本在容器里跑（G26），退出码即结论。
set -euo pipefail

RUN=${RUN:-/w/run/m0}
BIN=${BIN:-/w/target/release}
CONF=${CONF:-/w/conf/m0.yaml}
# ★ 与 spike 的 `bind_host()` 默认保持一致（回环）。地址写死在判据里迟早失配。
BIND_HOST=${M0_BIND_HOST:-127.0.0.1}
export M0_BIND_HOST="$BIND_HOST"
DURATION_MS=${DURATION_MS:-6000}
INTERVAL_MS=${INTERVAL_MS:-20}

export RUST_LOG=${RUST_LOG:-info}

mkdir -p "$RUN"
rm -f "$RUN"/*.pid "$RUN"/*.sock "$RUN"/*.log "$RUN"/*.json "$RUN"/*.pids

fail() { echo; echo "M0 FAILED: $*"; echo "--- server log ---"; cat "$RUN/error.log" 2>/dev/null || true; exit 1; }

# ── 收尾 ────────────────────────────────────────────────────────────────────
# 规则与全部理由在 lifecycle.sh 里（一句话：**每一代都要收，末代 SIGINT、更早的 SIGKILL**）。
# ★ 找不到就必须炸，不能默默不收——两个场景共用 8080–8082，收尾没跑等于给下一个场景埋雷。
LIFECYCLE_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lifecycle.sh"
[ -f "$LIFECYCLE_LIB" ] || { echo "找不到 $LIFECYCLE_LIB —— 收尾逻辑缺失，拒绝继续。" >&2; exit 1; }
# shellcheck source=tests/m0/lifecycle.sh
. "$LIFECYCLE_LIB"
lifecycle_init "$RUN"
trap lifecycle_cleanup EXIT

echo "=== [1/5] 启动第一代 ==="
"$BIN/m0-seam" -c "$CONF" -d
sleep 1
[ -f "$RUN/m0.pid" ] || fail "第一代没有写出 pid 文件"
GEN1=$(cat "$RUN/m0.pid")
record_gen "$GEN1"
echo "gen1 pid = $GEN1"

echo "=== [2/5] 起探针（持续 ${DURATION_MS}ms）==="
"$BIN/m0-probe" "$DURATION_MS" "$INTERVAL_MS" "$BIND_HOST:8080" "$BIND_HOST:8081" "$BIND_HOST:8082" > "$RUN/probe.json" &
PROBE=$!
# ★ 探针也要纳入收尾。它是客户端、不占监听端口，所以留下来不会让下一个场景绑不上；
#   但 [3/5]~[4/5] 之间任何失败都会让它一直跑到 deadline，而 lifecycle_cleanup
#   声称收的是「本脚本起的每一个进程」——**不含它就是名不副实**。
record_aux "$PROBE"

sleep 2

echo "=== [3/5] 触发优雅升级：SIGQUIT 给第一代，再以 -u 起第二代 ==="
# ★ 记下升级前的日志行数。**它不再是 INHERITED 断言的切分依据**（那条已改成按 pid 过滤），
#   只用来在 [5/5] 证明一件事：升级窗口内第一代确实还在往同一份 error.log 里写。
#   ——那正是「按行号切分」不够用的原因，也是「按 pid 过滤」这个修法的必要性所在。
LOG_LINES_BEFORE_UPGRADE=$(wc -l < "$RUN/error.log")
kill -QUIT "$GEN1"
"$BIN/m0-seam" -c "$CONF" -d -u
sleep 1
GEN2=$(cat "$RUN/m0.pid")
record_gen "$GEN2"
echo "gen2 pid = $GEN2"
[ "$GEN1" != "$GEN2" ] || fail "pid 没变，说明根本没换进程（$GEN1）"

echo "=== [4/5] 等探针跑完 ==="
wait "$PROBE" || true
RESULT=$(cat "$RUN/probe.json")
echo "probe: $RESULT"

num() { echo "$RESULT" | sed -n "s/.*\"$1\":\([0-9]*\).*/\1/p"; }

HTTP_OK=$(num http_ok);          HTTP_FAIL=$(num http_failures)
TCP_OK=$(num tcp_ok);            TCP_DISC=$(num tcp_disconnects)
UDP_OK=$(num udp_ok);            UDP_LOSS=$(num udp_losses)

echo "=== [5/5] 判定 ==="

# ── 直接证据：第二代必须在日志里报告继承了两个自建监听器的 fd
#
# ★ 按 **pid** 过滤，不按行号。两代在升级窗口内**同时**往同一个 error.log 写，
#   `tail -n +N` 取的是「快照之后的所有行」，也就是「时间点之后的所有代」——
#   而断言声称的是「只看第二代」。今天不出错，只是因为第一代走的是 bind fresh、
#   不打 INHERITED；将来一旦让第一代也走继承路径，这条断言会重新变得不精确。
#   pid 由 spike 打进每行前缀（见 spikes/m0-seam/src/main.rs），与 pid 文件同源。
GEN2_TAG="pid=$GEN2 "   # ★ 带尾随空格：否则 pid=123 会顺带匹配上 pid=1234
grep -F "$GEN2_TAG" "$RUN/error.log" > "$RUN/gen2.log" || true
[ -s "$RUN/gen2.log" ] \
  || fail "按 '$GEN2_TAG' 过滤后一行都没有。第二代要么没写日志，要么日志前缀的格式变了——
       无论哪种，下面两条断言都失去了'只看第二代'的依据，不能放行。"
grep -q "\[raw-tcp\] INHERITED fd=" "$RUN/gen2.log" || fail "第二代没有继承自建 TCP 监听器的 fd"
grep -q "\[raw-udp\] INHERITED fd=" "$RUN/gen2.log" || fail "第二代没有继承自建 UDP 监听器的 fd（★ 这是 M0 的核心风险点）"

# ★ 证明上面那道过滤**不是空操作**：第一代在升级快照之后确实还在写同一份日志。
#   若这条红了，说明第一代不再在窗口内输出——那时「按行号切分」和「按 pid 过滤」
#   等价，这条断言就该连同上面的注释一起重估，而不是删掉了事。
GEN1_AFTER=$(tail -n "+$((LOG_LINES_BEFORE_UPGRADE + 1))" "$RUN/error.log" | grep -c -F "pid=$GEN1 " || true)
[ "$GEN1_AFTER" -gt 0 ] \
  || fail "升级快照之后第一代一行都没写。按 pid 过滤本是为了把它排除掉，
       现在它不写了——判据的前提变了，需要人工重估 [5/5] 的注释与断言。"
echo "  ✓ 第二代继承了自建 TCP 与 UDP 监听器的 fd（判据按 pid=$GEN2 过滤）"
echo "    ★ 同一窗口内第一代还写了 $GEN1_AFTER 行——按行号切分会把它们一起算进来"

# ── 间接证据：三类流量零中断
[ "${HTTP_OK:-0}" -gt 0 ]  || fail "HTTP 探针一次都没成功过，测试本身无效"
[ "${TCP_OK:-0}" -gt 0 ]   || fail "TCP 探针一次都没成功过，测试本身无效"
[ "${UDP_OK:-0}" -gt 0 ]   || fail "UDP 探针一次都没成功过，测试本身无效"
[ "${HTTP_FAIL:-1}" -eq 0 ] || fail "HTTP 请求失败 $HTTP_FAIL 次"
[ "${TCP_DISC:-1}" -eq 0 ]  || fail "跨升级的长连接断开 $TCP_DISC 次"
[ "${UDP_LOSS:-1}" -eq 0 ]  || fail "UDP 回声丢失 $UDP_LOSS 次"
echo "  ✓ http $HTTP_OK/0  tcp $TCP_OK/0  udp $UDP_OK/0（成功/失败）"

echo
echo "M0 PASSED —— 自建 TCP 与 UDP 监听器参与了 Pingora 的 socket 移交，升级窗口内三类流量零中断。"

# ★ 两代的收尾都交给文件开头那个 EXIT trap（`cleanup`），这里不再各写一遍。
#   放在 trap 里而不是写在末尾，是因为它同样要覆盖**失败路径**：
#   `fail` 直接 exit，此前那些路径上两代都会原样留下。
#   端口是否真的空出来，由下一个场景的 [0/6] 前置检查独立盯着。
