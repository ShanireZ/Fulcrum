#!/usr/bin/env bash
# M0 接縫驗證：一次 SIGQUIT + `-u` 優雅升級，三類流量零中斷。
# 這個指令碼在容器裡跑（G26），結束代碼即結論。
set -euo pipefail

RUN=${RUN:-/w/run/m0}
BIN=${BIN:-/w/target/release}
CONF=${CONF:-/w/conf/m0.yaml}
# ★ 與 spike 的 `bind_host()` 預設保持一致（迴環）。位址寫死在判據裡遲早失配。
BIND_HOST=${M0_BIND_HOST:-127.0.0.1}
export M0_BIND_HOST="$BIND_HOST"
DURATION_MS=${DURATION_MS:-6000}
INTERVAL_MS=${INTERVAL_MS:-20}

export RUST_LOG=${RUST_LOG:-info}

mkdir -p "$RUN"
rm -f "$RUN"/*.pid "$RUN"/*.sock "$RUN"/*.log "$RUN"/*.json "$RUN"/*.pids

fail() { echo; echo "M0 FAILED: $*"; echo "--- server log ---"; cat "$RUN/error.log" 2>/dev/null || true; exit 1; }

# ── 收尾 ────────────────────────────────────────────────────────────────────
# 規則與全部理由在 lifecycle.sh 裡（一句話：**每一代都要收，末代 SIGINT、更早的 SIGKILL**）。
# ★ 找不到就必須炸，不能默默不收——兩個場景共用 8080–8082，收尾沒跑等於給下一個場景埋雷。
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
# ★ 探針也要納入收尾。它是用戶端、不佔監聽連接埠，所以留下來不會讓下一個場景綁不上；
#   但 [3/5]~[4/5] 之間任何失敗都會讓它一直跑到 deadline，而 lifecycle_cleanup
#   聲稱收的是「本指令碼起的每一個處理程序」——**不含它就是名不副實**。
record_aux "$PROBE"

sleep 2

echo "=== [3/5] 触发优雅升级：SIGQUIT 给第一代，再以 -u 起第二代 ==="
# ★ 記下升級前的紀錄行數。**它不再是 INHERITED 斷言的切分依據**（那條已改成按 pid 過濾），
#   只用來在 [5/5] 證明一件事：升級視窗內第一代確實還在往同一份 error.log 裡寫。
#   ——那正是「按行號切分」不夠用的原因，也是「按 pid 過濾」這個修法的必要性所在。
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

# ── 直接證據：第二代必須在紀錄裡報告繼承了兩個自建監聽器的 fd
#
# ★ 按 **pid** 過濾，不按行號。兩代在升級視窗內**同時**往同一個 error.log 寫，
#   `tail -n +N` 取的是「快照之後的所有行」，也就是「時間點之後的所有代」——
#   而斷言聲稱的是「只看第二代」。今天不出錯，只是因為第一代走的是 bind fresh、
#   不打 INHERITED；將來一旦讓第一代也走繼承路徑，這條斷言會重新變得不精確。
#   pid 由 spike 打進每行前綴（見 spikes/m0-seam/src/main.rs），與 pid 檔案同源。
GEN2_TAG="pid=$GEN2 "   # ★ 帶尾隨空格：否則 pid=123 會順帶匹配上 pid=1234
grep -F "$GEN2_TAG" "$RUN/error.log" > "$RUN/gen2.log" || true
[ -s "$RUN/gen2.log" ] \
  || fail "按 '$GEN2_TAG' 过滤后一行都没有。第二代要么没写日志，要么日志前缀的格式变了——
       无论哪种，下面两条断言都失去了'只看第二代'的依据，不能放行。"
grep -q "\[raw-tcp\] INHERITED fd=" "$RUN/gen2.log" || fail "第二代没有继承自建 TCP 监听器的 fd"
grep -q "\[raw-udp\] INHERITED fd=" "$RUN/gen2.log" || fail "第二代没有继承自建 UDP 监听器的 fd（★ 这是 M0 的核心风险点）"

# ★ 證明上面那道過濾**不是空操作**：第一代在升級快照之後確實還在寫同一份紀錄。
#   若這條紅了，說明第一代不再在視窗內輸出——那時「按行號切分」和「按 pid 過濾」
#   等價，這條斷言就該連同上面的註解一起重估，而不是刪掉了事。
GEN1_AFTER=$(tail -n "+$((LOG_LINES_BEFORE_UPGRADE + 1))" "$RUN/error.log" | grep -c -F "pid=$GEN1 " || true)
[ "$GEN1_AFTER" -gt 0 ] \
  || fail "升级快照之后第一代一行都没写。按 pid 过滤本是为了把它排除掉，
       现在它不写了——判据的前提变了，需要人工重估 [5/5] 的注释与断言。"
echo "  ✓ 第二代继承了自建 TCP 与 UDP 监听器的 fd（判据按 pid=$GEN2 过滤）"
echo "    ★ 同一窗口内第一代还写了 $GEN1_AFTER 行——按行号切分会把它们一起算进来"

# ── 間接證據：三類流量零中斷
[ "${HTTP_OK:-0}" -gt 0 ]  || fail "HTTP 探针一次都没成功过，测试本身无效"
[ "${TCP_OK:-0}" -gt 0 ]   || fail "TCP 探针一次都没成功过，测试本身无效"
[ "${UDP_OK:-0}" -gt 0 ]   || fail "UDP 探针一次都没成功过，测试本身无效"
[ "${HTTP_FAIL:-1}" -eq 0 ] || fail "HTTP 请求失败 $HTTP_FAIL 次"
[ "${TCP_DISC:-1}" -eq 0 ]  || fail "跨升级的长连接断开 $TCP_DISC 次"
[ "${UDP_LOSS:-1}" -eq 0 ]  || fail "UDP 回声丢失 $UDP_LOSS 次"
echo "  ✓ http $HTTP_OK/0  tcp $TCP_OK/0  udp $UDP_OK/0（成功/失败）"

echo
echo "M0 PASSED —— 自建 TCP 与 UDP 监听器参与了 Pingora 的 socket 移交，升级窗口内三类流量零中断。"

# ★ 兩代的收尾都交給檔案開頭那個 EXIT trap（`cleanup`），這裡不再各寫一遍。
#   放在 trap 裡而不是寫在末尾，是因為它同樣要覆蓋**失敗路徑**：
#   `fail` 直接 exit，此前那些路徑上兩代都會原樣留下。
#   連接埠是否真的空出來，由下一個場景的 [0/6] 前置檢查獨立盯著。
