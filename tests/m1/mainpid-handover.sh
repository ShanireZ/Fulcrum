#!/usr/bin/env bash
# M1 第三个场景：**把被否掉的那条路钉住**。
#
# G31 拍板时的形状是「新进程落在同一个 cgroup 内**并抢过 MainPID**」。本 spike 实测下来，
# 这条路**一半对一半错**，而错的那一半没有任何症状会在升级当时暴露：
#
#   · 对的一半：MainPID 交接确实被 systemd 接受，unit 确实活过了老代退出。
#   · ★ ★ ★ 错的一半：交接过去的 pid 不是 systemd 亲生的（老进程 fork 的），
#     systemd 把它标成 alien，**此后每一次 systemctl stop 都不再等排空**——
#     SIGTERM 与 SIGKILL 几乎同时发出，unit 以 failed(signal) 收场。
#     也就是说：**第一次升级之后，这台机器就再也没有优雅停机了**，
#     而升级本身看起来一切正常。重启、`systemctl restart`、机器关机，全部变成硬杀。
#
# 本场景跑的就是 G31 的原始形状（ExitType=main + 交接），把这两件事都断言下来。
# ★ 它的价值不在「发现」，而在**防止有人照着 G31 的原文再实现一遍**：
#   那段话还留在 PLAN.md 的决策日志里（决策日志不改写历史），下一个读到它的人
#   完全可能照做。这个脚本是留给他的那条红线。
#
# ⚠ 若它红了：说明 systemd 对 alien MainPID 的停机行为变了。那是好消息，
#   但要**重新评估**是否回到交接方案，而不是把这里改绿。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/m1/lib.sh
. "$HERE/lib.sh"

DROPIN_DIR="/etc/systemd/system/${UNIT}.d"
# conf/m1.yaml 的 grace_period_seconds。正常停机实测 10 秒（grace 5 + graceful 5，见 run.sh [8/9]），
# 所以「少于 5 秒」就是「一秒排空都没等」。
GRACE_FLOOR=5

cleanup_all() {
  m1_cleanup
  rm -rf "$DROPIN_DIR"
  systemctl daemon-reload >/dev/null 2>&1 || true
}
trap cleanup_all EXIT

echo "=== [0/5] 装 G31 原始形状的 drop-in（ExitType=main + M1_MAINPID=claim）==="
mkdir -p "$DROPIN_DIR"
# NotifyAccess 必须放宽到 all：发 MAINPID= 的是**下一代**，那一刻它既不是 MainPID，
# 也不是任何 Exec* 拉起的进程。★ 这本身就是交接方案的一项代价——
# cgroup 内任何进程都能给 systemd 发通知。
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

# ★ 把 systemd 自己那句警告也钉下来。它是「错的那一半」的直接来源，
#   而且它**说得并不准**——原话是「多半察觉不到它退出」，实测 systemd 1 秒内就察觉了
#   （新一代在老代退出后会被 reparent 给 PID 1，也就是 systemd 自己）。
#   真正的后果不是「察觉不到」，而是**停机不再等它**。
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

# ── 判据只挂在两个**稳定**的量上 ─────────────────────────────────────────────
#
# ⚠ ★ ★ **`Result` 不能当判据，它会抖。** 实测：同一份代码、同一个容器口径，
#   空闲时连跑三次都是 `failed/signal`，而在跑完整套验证（机器忙）的那一次是
#   `inactive/dead` + `Result=success` —— 两次的**坏行为完全一样**（0 秒、主进程被 SIGKILL），
#   差别只在 systemd 有没有来得及把 reparent 过来的主进程 wait 掉并读到 status=9/KILL。
#   ★ **时快时慢的反证等于没有反证**。把判据挂在 `Result` 上的话，
#   于是它在全量跑里红了一次——红的不是被测对象，是判据本身。
#
# 稳定的是这两条（四次观测全部一致）：
[ "$STOP_SECS" -lt "$GRACE_FLOOR" ] \
  || fail "★ 交接之后停机竟然用了 ${STOP_SECS}s（≥ grace_period_seconds=${GRACE_FLOOR}）——
       说明 systemd 现在会等 alien MainPID 排空了。行为变了，MainPID 交接方案值得重新评估。"

# systemd 必须是**动手杀**的，而不是等它自己退干净的。
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
