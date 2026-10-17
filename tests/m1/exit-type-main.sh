#!/usr/bin/env bash
# M1 反證場景：把 `ExitType` 換回 systemd 的預設值 `main`，其餘一模一樣。
#
# ★ 它重現的是**壞行為**，所以它的綠 = 壞行為照常發生 —— 與 `tests/m0/unclaimed.sh` 原先同類
#   （後者 2026-09-24 rebase 到 pingora 0.9.0 後已反寫成驗修復）。
#   存在的理由有兩條，都很硬：
#
#   1. **證明 `ExitType=cgroup` 那一行確實在幹活。** 少了它，unit 會在老代退出時被判定結束，
#      並把已經接手了監聽 fd 的新一代一起殺掉 —— 這正是 G31 擔心的那個失敗。
#   2. ★ ★ **證明 run.sh 的判據分得清好壞。** 一道只見過綠的門，與一道根本沒在跑的門，
#      從輸出上無法區分（AGENTS.md「Gate discipline」）。run.sh 裡那句
#      「gen1 退出把 gen2 一起帶走了」的斷言，只有在這裡真的紅過，才算數。
#
# ⚠ 若有一天這個指令碼**紅了**，含義是「systemd 的行為變了」，不是「測試壞了」——
#   那時要回頭重估 run.sh 的判據，而不是把這裡改綠。
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
# ★ 證明 drop-in 真的生效了，而不是寫了個沒人讀的檔案。
#   少了這一條，下面整場戲可能是在跑**正常口徑**——而正常口徑下 unit 不死，
#   於是這個反證指令碼會紅，我們就會去查一個根本不存在的問題。
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

# ★ 判據取「不再 active」的**第一時刻**，而不是「等 2 秒再看」。
#   寫死 sleep 2 然後同時斷言「unit 不 active」與「gen2 已死」的話，
#   結果卡在一個自相矛盾的中間態上：unit 已經是 deactivating，而 gen2 還活著。
#   ——因為 systemd 判定 unit 結束之後是**發 SIGTERM 讓整組優雅退出**，
#   而 gen2 是 pingora，它會老老實實排空 grace+graceful 秒。兩件事本來就不同時發生。
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
# ★ ★ 這裡的失效形態比預想的更陰險：新一代**沒有被立刻殺掉**，
#   而是收到 SIGTERM 後照常排空——於是**升級看起來成功了，流量也確實還在通**，
#   直到排空視窗走完，整臺機器上的服務才一起消失。
#   若判據寫成「升級後立刻探一下連接埠通不通」，這個場景會報綠。
#   （與 open-seams.md 裡「黑洞不是立刻出現的」是同一類陷阱。）
alive "$GEN2" && echo "  · 此刻 gen2=$GEN2 仍活着，正在排空 —— 端口还通，但它已经被判了死刑"
for _ in $(seq 1 200); do   # 40s：夠 grace(5)+graceful(5) 走完，也夠 systemd 備援 SIGKILL
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
