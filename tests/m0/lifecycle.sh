#!/usr/bin/env bash
# 两个 M0 场景共用的「起了几代 / 怎么把它们收干净」。
#
# 用法（在场景脚本里）：
#     . "$(dirname "${BASH_SOURCE[0]}")/lifecycle.sh"
#     lifecycle_init "$RUN"
#     trap lifecycle_cleanup EXIT
#     ...   每起一代就   record_gen "$PID"
#
# ★ 收尾规则足够微妙（见下面那张表），所以只留一份 —— 两份复制粘贴迟早分头长歪。
#
# ══ 为什么必须逐代收 ═════════════════════════════════════════════════════════
#
# 三个 M0 场景共用 8080–8082，每一个都必须不留残余进程；留下的话，下一个场景绑不上端口，
# 而它的基线探测**照样会绿**（对着这个残留进程）—— 实际发生过。
#
# ★ `$RUN/m0.pid` 里**永远只有最后一代**。而任何发生在「下一代已起、上一代未退」窗口里的
#   断言失败，都会让上一代活着离开并继续持有端口。
#   ⚠ 一个只是碰巧成立的清理（比如靠收尾时那 30 秒死等把上一代耗走），
#   与一个不存在的清理，区别只在运气。
#
# ══ 为什么末代与更早的用不同的信号 ═══════════════════════════════════════════
#
# 信号收不收得动，取决于那一代处在哪个阶段 —— **这是量出来的，不是从文档推的**：
#
#   | 那一代的状态                  | 发 SIGINT                                    |
#   |------------------------------|----------------------------------------------|
#   | 还在 `main_loop` 里等信号     | **0 秒退出**（Quick，两个超时都是 0）          |
#   | 已收过 SIGQUIT、正在排空      | ★ **被吞掉**，40 秒后照样活着                  |
#
# 后一条的原因：进程已离开 `main_loop`，没人再从信号管道里读；而 tokio 的处理器仍然装着，
# 顶掉了 SIGINT 默认的 terminate 行为。**排空中的那一代收不到任何可捕获的信号**——
# 它只能自己走完 `CLOSE_TIMEOUT`(5) + `grace_period_seconds` + `graceful_shutdown_timeout_seconds`
# （`vendor/pingora/pingora-core/src/server/mod.rs`），在 m0.yaml 的配置下是 35–65 秒。
#
# ★ 所以：**最后起的那一代是活的，用 SIGINT；更早的都在排空，直接 SIGKILL。**
#   对更早那些用 SIGKILL 是**有意的选择，不是等待失败后的兜底**——收尾时判据已全部做完，
#   它们的 fd 也早已交接出去，只剩「把端口空出来」这一件事。
#
# ★ 这条同样适用于 M1 的 systemd 设计：一旦一代开始排空就叫不动了，
#   `TimeoutStopSec` 必须按 35–65 秒这个量级配。

# 未 init 时保持为空，`lifecycle_cleanup` 会安全地什么都不做（脚本可能在起第一代之前就失败）。
LIFECYCLE_PIDS=""
# ★ 「附属进程」——不是 m0-seam 的某一代，而是脚本自己拉起的辅助进程（目前只有探针）。
#   它们不持有监听端口，所以留下来不会让下一个场景绑不上；但 `lifecycle_cleanup`
#   声称收的是「本脚本起的每一个进程」，**不含它们就是名不副实**——
#   而名不副实的清理，正是本文件开头那段教训的来源。
LIFECYCLE_AUX=""
# 末代收不掉时等多久才强杀。SIGINT 走 fast shutdown，正常应当秒退，所以走到超时就是真异常。
LIFECYCLE_WAIT=${LIFECYCLE_WAIT:-30}

lifecycle_init() {
  LIFECYCLE_PIDS="$1/generations.pids"
  : > "$LIFECYCLE_PIDS"
}

record_gen() {
  [ -n "$LIFECYCLE_PIDS" ] || return 0
  echo "$1" >> "$LIFECYCLE_PIDS"
}

# 登记一个附属进程（探针之类）。收尾时直接收掉，不走「末代/更早」那套。
record_aux() { LIFECYCLE_AUX="$LIFECYCLE_AUX $1"; }

lifecycle_cleanup() {
  local aux
  for aux in $LIFECYCLE_AUX; do
    kill -TERM "$aux" 2>/dev/null || true
  done

  [ -n "$LIFECYCLE_PIDS" ] && [ -f "$LIFECYCLE_PIDS" ] || return 0
  local live earlier pid
  live=$(tail -n 1 "$LIFECYCLE_PIDS")
  earlier=$(sed '$d' "$LIFECYCLE_PIDS")

  # 更早的那些都在排空，信号叫不动，直接 SIGKILL（见上）。
  for pid in $earlier; do
    kill -0 "$pid" 2>/dev/null || continue
    kill -KILL "$pid" 2>/dev/null || true
  done

  [ -n "$live" ] || return 0
  kill -0 "$live" 2>/dev/null || return 0
  kill -INT "$live" 2>/dev/null || true
  for _ in $(seq 1 "$LIFECYCLE_WAIT"); do
    kill -0 "$live" 2>/dev/null || return 0
    sleep 1
  done
  # ★ 走到这里才是真异常：活着的那一代对 fast shutdown 没反应。
  echo "⚠ 末代 $live 收到 SIGINT 后 ${LIFECYCLE_WAIT} 秒仍没退出，强杀。
   ★ 这不该发生：SIGINT 走 fast shutdown，两个超时都是 0，正常应当秒退。
   若它反复出现，是退出真的卡住了，要去查，而不是把这个数字调大。" >&2
  kill -KILL "$live" 2>/dev/null || true
  sleep 1
}
