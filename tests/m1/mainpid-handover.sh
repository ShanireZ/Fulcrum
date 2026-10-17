#!/usr/bin/env bash
# M1 第三個場景：**把被否掉的那條路釘住**。
#
# G31 拍板時的形狀是「新處理程序落在同一個 cgroup 內**並搶過 MainPID**」。本 spike 實測下來，
# 這條路**一半對一半錯**，而錯的那一半沒有任何症狀會在升級當時暴露：
#
#   · 對的一半：MainPID 交接確實被 systemd 接受，unit 確實活過了老代退出。
#   · ★ ★ ★ 錯的一半：交接過去的 pid 不是 systemd 親生的（老處理程序 fork 的），
#     systemd 把它標成 alien，**此後每一次 systemctl stop 都不再等排空**——
#     SIGTERM 與 SIGKILL 幾乎同時發出，unit 以 failed(signal) 收場。
#     也就是說：**第一次升級之後，這臺機器就再也沒有優雅停機了**，
#     而升級本身看起來一切正常。重新啟動、`systemctl restart`、機器關機，全部變成硬殺。
#
# 本場景跑的就是 G31 的原始形狀（ExitType=main + 交接），把這兩件事都斷言下來。
# ★ 它的價值不在「發現」，而在**防止有人照著 G31 的原文再實作一遍**：
#   那段話還留在 PLAN.md 的決策紀錄裡（決策紀錄不改寫歷史），下一個讀到它的人
#   完全可能照做。這個指令碼是留給他的那條紅線。
#
# ⚠ 若它紅了：說明 systemd 對 alien MainPID 的停機行為變了。那是好訊息，
#   但要**重新評估**是否回到交接方案，而不是把這裡改綠。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/m1/lib.sh
. "$HERE/lib.sh"

DROPIN_DIR="/etc/systemd/system/${UNIT}.d"
# conf/m1.yaml 的 grace_period_seconds。正常停機實測 10 秒（grace 5 + graceful 5，見 run.sh [8/9]），
# 所以「少於 5 秒」就是「一秒排空都沒等」。
GRACE_FLOOR=5

cleanup_all() {
  m1_cleanup
  rm -rf "$DROPIN_DIR"
  systemctl daemon-reload >/dev/null 2>&1 || true
}
trap cleanup_all EXIT

echo "=== [0/5] 装 G31 原始形状的 drop-in（ExitType=main + M1_MAINPID=claim）==="
mkdir -p "$DROPIN_DIR"
# NotifyAccess 必須放寬到 all：發 MAINPID= 的是**下一代**，那一刻它既不是 MainPID，
# 也不是任何 Exec* 拉起的處理程序。★ 這本身就是交接方案的一項代價——
# cgroup 內任何處理程序都能給 systemd 發通知。
printf '[Service]\nExitType=main\nNotifyAccess=all\nEnvironment=M1_MAINPID=claim\n' \
  > "$DROPIN_DIR/mainpid-handover.conf"
systemctl daemon-reload
[ "$(unit_prop ExitType)" = "main" ] || fail "drop-in 没生效：ExitType=$(unit_prop ExitType)"
[ "$(unit_prop NotifyAccess)" = "all" ] || fail "drop-in 没生效：NotifyAccess=$(unit_prop NotifyAccess)"
unit_prop Environment | grep -q 'M1_MAINPID=claim' || fail "drop-in 没生效：Environment 里没有 M1_MAINPID=claim"
rm -f "$PID_FILE"
echo "  ✓ drop-in 已生效"

echo "=== [1/5] start ==="
systemctl start "$UNIT" || fail "systemctl start 失败"
assert_active "刚 start 完"
GEN1=$(main_pid)
echo "  gen1 = $GEN1"

echo "=== [2/5] reload —— 这一次新一代会去抢 MainPID ==="
systemctl reload "$UNIT" || fail "systemctl reload 失败"
GEN2=""
for _ in $(seq 1 100); do
  mp=$(main_pid)
  if [ "$mp" != "$GEN1" ] && [ "$mp" -gt 0 ] 2>/dev/null; then GEN2=$mp; break; fi
  sleep 0.2
done
[ -n "$GEN2" ] || fail "20 秒内 MainPID 一直是 $GEN1 —— 交接没有发生，本场景的前提不成立"
echo "  ✓ 对的那一半成立：MainPID $GEN1 → $GEN2（systemd 接受了来自同 cgroup 非亲生进程的交接）"

# ★ 把 systemd 自己那句警告也釘下來。它是「錯的那一半」的直接來源，
#   而且它**說得並不準**——原話是「多半察覺不到它退出」，實測 systemd 1 秒內就察覺了
#   （新一代在老代退出後會被 reparent 給 PID 1，也就是 systemd 自己）。
#   真正的後果不是「察覺不到」，而是**停機不再等它**。
journalctl -u "$UNIT" --no-pager -o cat --since "-3min" \
  | grep -q "which is not our child" \
  || fail "journal 里找不到 systemd 那句 'not our child' 警告 —— 交接的实现方式可能变了，
       而下面关于停机的断言全都建立在「MainPID 是 alien」这个前提上。"
echo "  ✓ systemd 已把它标成 alien（journal 里有 'not our child'）"

echo "=== [3/5] 等 gen1 退出 ==="
for _ in $(seq 1 150); do alive "$GEN1" || break; sleep 0.2; done
alive "$GEN1" && fail "30 秒后 gen1 还没退出"
sleep 1
assert_active "第一代退出之后"
alive "$GEN2" || fail "gen2 被带走了 —— 交接连「对的那一半」都没成立"
echo "  ✓ unit 活过了老代退出，gen2 仍是 MainPID —— 到这里为止，G31 的推断都是对的"

echo "=== [4/5] ★ 现在停机 —— 错的那一半在这里显形 ==="
STOP_T0=$(date +%s)
systemctl stop "$UNIT" || true
STOP_T1=$(date +%s)
STOP_SECS=$((STOP_T1 - STOP_T0))
STATE=$(unit_state)
RESULT_PROP=$(unit_prop Result)
echo "  stop 耗时 ${STOP_SECS}s，最终 $STATE，Result=$RESULT_PROP"

# ── 判據只掛在兩個**穩定**的量上 ─────────────────────────────────────────────
#
# ⚠ ★ ★ **`Result` 不能當判據，它會抖。** 實測：同一份程式碼、同一個容器口徑，
#   空閒時連跑三次都是 `failed/signal`，而在跑完整套驗證（機器忙）的那一次是
#   `inactive/dead` + `Result=success` —— 兩次的**壞行為完全一樣**（0 秒、主處理程序被 SIGKILL），
#   差別只在 systemd 有沒有來得及把 reparent 過來的主處理程序 wait 掉並讀到 status=9/KILL。
#   ★ **時快時慢的反證等於沒有反證**。把判據掛在 `Result` 上的話，
#   於是它在全量跑裡紅了一次——紅的不是被測物件，是判據本身。
#
# 穩定的是這兩條（四次觀測全部一致）：
[ "$STOP_SECS" -lt "$GRACE_FLOOR" ] \
  || fail "★ 交接之后停机竟然用了 ${STOP_SECS}s（≥ grace_period_seconds=${GRACE_FLOOR}）——
       说明 systemd 现在会等 alien MainPID 排空了。行为变了，MainPID 交接方案值得重新评估。"

# systemd 必須是**動手殺**的，而不是等它自己退乾淨的。
journalctl -u "$UNIT" --no-pager -o cat --since "-3min" \
  | grep -q "Killing process ${GEN2} .*with signal SIGKILL" \
  || fail "★ journal 里找不到「systemd 用 SIGKILL 杀掉主进程 ${GEN2}」这一句。
       坏行为的形态变了：也许它现在走的是正常停机流程，那 MainPID 交接方案值得重新评估。
       （注意 Result=${RESULT_PROP} 这个量是会抖的，不要拿它当判据，理由见本节注释。）"
echo "  ★ 坏行为复现：停机 ${STOP_SECS} 秒就结束，且 journal 里 systemd 是用 SIGKILL 收的主进程"
echo "    —— 排空一秒都没等。（Result=${RESULT_PROP}，★ 这个量会抖，仅作参考不作判据）"

echo "=== [5/5] 结论 ==="
echo
echo "M1-MAINPID-HANDOVER 复现成功 —— G31 推断的那条路："
echo "  · 对的一半：交接被接受，unit 活过老代退出（$GEN1 → $GEN2）"
echo "  · ★ 错的一半：此后 systemctl stop 只用 ${STOP_SECS}s，且主进程是被 systemd SIGKILL 掉的 ——"
echo "    优雅停机没了，而升级当时没有任何症状。"
echo "★ 这就是 run.sh 改用 ExitType=cgroup、不做 MainPID 交接的实测依据。"
