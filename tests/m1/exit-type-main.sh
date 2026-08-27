#!/usr/bin/env bash
# M1 反证场景：把 `ExitType` 换回 systemd 的默认值 `main`，其余一模一样。
#
# ★ 它复现的是**坏行为**，所以它的绿 = 坏行为照常发生 —— 与 `tests/m0/unclaimed.sh` 同类。
#   存在的理由有两条，都很硬：
#
#   1. **证明 `ExitType=cgroup` 那一行确实在干活。** 少了它，unit 会在老代退出时被判定结束，
#      并把已经接手了监听 fd 的新一代一起杀掉 —— 这正是 G31 担心的那个失败。
#   2. ★ ★ **证明 run.sh 的判据分得清好坏。** 一道只见过绿的门，与一道根本没在跑的门，
#      从输出上无法区分（AGENTS.md「Gate discipline」）。run.sh 里那句
#      「gen1 退出把 gen2 一起带走了」的断言，只有在这里真的红过，才算数。
#
# ⚠ 若有一天这个脚本**红了**，含义是「systemd 的行为变了」，不是「测试坏了」——
#   那时要回头重估 run.sh 的判据，而不是把这里改绿。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/m1/lib.sh
. "$HERE/lib.sh"

DROPIN_DIR="/etc/systemd/system/${UNIT}.d"

cleanup_all() {
  m1_cleanup
  rm -rf "$DROPIN_DIR"
  systemctl daemon-reload >/dev/null 2>&1 || true
}
trap cleanup_all EXIT

echo "=== [0/5] 装反证用的 drop-in（ExitType=main）==="
mkdir -p "$DROPIN_DIR"
printf '[Service]\nExitType=main\n' > "$DROPIN_DIR/exit-type-main.conf"
systemctl daemon-reload
# ★ 证明 drop-in 真的生效了，而不是写了个没人读的文件。
#   少了这一条，下面整场戏可能是在跑**正常口径**——而正常口径下 unit 不死，
#   于是这个反证脚本会红，我们就会去查一个根本不存在的问题。
[ "$(unit_prop ExitType)" = "main" ] \
  || fail "drop-in 没生效：unit 的 ExitType 仍是 $(unit_prop ExitType)"
rm -f "$PID_FILE"
echo "  ✓ drop-in 已生效（ExitType=main）"

echo "=== [1/5] start ==="
systemctl start "$UNIT" || fail "systemctl start 失败"
assert_active "刚 start 完"
GEN1=$(main_pid)
echo "  gen1 = $GEN1"

echo "=== [2/5] reload —— 新一代会起来并接手 fd，但没有任何东西替它撑住 unit ==="
systemctl reload "$UNIT" || fail "systemctl reload 失败"
GEN2=""
for _ in $(seq 1 100); do
  cur=$(cat "$PID_FILE" 2>/dev/null || true)
  if [ -n "$cur" ] && [ "$cur" != "$GEN1" ]; then GEN2=$cur; break; fi
  sleep 0.2
done
[ -n "$GEN2" ] || fail "20 秒内没看到第二代 —— 连坏行为都没复现出来，
       多半是升级触发器本身坏了，这时 run.sh 的绿也不可信。"
echo "  gen2 = $GEN2（已接手，但 MainPID 仍是 gen1）"
[ "$(main_pid)" = "$GEN1" ] || fail "MainPID 变成了 $(main_pid) —— 本场景不该有任何交接"

echo "=== [3/5] 等 gen1 退出 —— 坏行为应当在这一刻开始 ==="
for _ in $(seq 1 150); do alive "$GEN1" || break; sleep 0.2; done
alive "$GEN1" && fail "30 秒后 gen1 还没退出"

# ★ 判据取「不再 active」的**第一时刻**，而不是「等 2 秒再看」。
#   写死 sleep 2 然后同时断言「unit 不 active」与「gen2 已死」的话，
#   结果卡在一个自相矛盾的中间态上：unit 已经是 deactivating，而 gen2 还活着。
#   ——因为 systemd 判定 unit 结束之后是**发 SIGTERM 让整组优雅退出**，
#   而 gen2 是 pingora，它会老老实实排空 grace+graceful 秒。两件事本来就不同时发生。
FIRST_STATE=""
for _ in $(seq 1 100); do
  st=$(unit_state)
  if [ "$st" != "active/running" ]; then FIRST_STATE=$st; break; fi
  sleep 0.2
done
[ -n "$FIRST_STATE" ] \
  || fail "★ ExitType=main 之下 unit **依然**活过了老代退出（20 秒内一直 active/running）——
       说明 run.sh 那条「gen1 退出把 gen2 一起带走了」的断言根本分不清好坏，它的绿不能作数。
       systemd 的行为可能变了，需要人工重估两个脚本的判据。"
echo "  ✓ gen1 一退出，unit 立刻转入 $FIRST_STATE（不再 active）"

echo "=== [4/5] ★ 关键：新一代是「被判了缓刑」，不是「安然无恙」 ==="
# ★ ★ 这里的失效形态比预想的更阴险：新一代**没有被立刻杀掉**，
#   而是收到 SIGTERM 后照常排空——于是**升级看起来成功了，流量也确实还在通**，
#   直到排空窗口走完，整台机器上的服务才一起消失。
#   若判据写成「升级后立刻探一下端口通不通」，这个场景会报绿。
#   （与 open-seams.md 里「黑洞不是立刻出现的」是同一类陷阱。）
alive "$GEN2" && echo "  · 此刻 gen2=$GEN2 仍活着，正在排空 —— 端口还通，但它已经被判了死刑"
for _ in $(seq 1 200); do   # 40s：够 grace(5)+graceful(5) 走完，也够 systemd 兜底 SIGKILL
  alive "$GEN2" || break
  sleep 0.2
done
alive "$GEN2" \
  && fail "★ 40 秒后 gen2=$GEN2 还活着，而 unit 早已不 active —— 这是第三种情况：
       进程逃出了 cgroup 生命周期管理，比预期的坏行为更糟，必须查。"
FINAL_STATE=$(unit_state)
echo "  ✓ gen2 最终还是没了（unit 现在是 $FINAL_STATE）—— 它接手的 fd 与连接一起消失"
[ -z "$(cgroup_pids)" ] || fail "cgroup 里仍有残留进程：$(cgroup_pids | tr '\n' ' ')"

echo "=== [5/5] 结论 ==="
echo
echo "M1-EXIT-TYPE-MAIN 复现成功 —— 把 ExitType 换回默认的 main 之后："
echo "  · 第一代退出，systemd 立刻判定 unit 结束（转入 $FIRST_STATE）"
echo "  · ★ 第二代不是被秒杀，而是收到 SIGTERM 后**照常排空**：升级看起来成功了、"
echo "    流量也还通，直到排空窗口走完，服务才整个消失（最终 $FINAL_STATE）"
echo "★ 于是 G31 的那条推断由推断变成实测，而 run.sh 的对应断言也证明了它能红。"
