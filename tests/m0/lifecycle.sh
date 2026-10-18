#!/usr/bin/env bash
# 兩個 M0 場景共用的「起了幾代 / 怎麼把它們收乾淨」。
#
# 用法（在場景指令碼裡）：
#     . "$(dirname "${BASH_SOURCE[0]}")/lifecycle.sh"
#     lifecycle_init "$RUN"
#     trap lifecycle_cleanup EXIT
#     ...   每起一代就   record_gen "$PID"
#
# ★ 收尾規則足夠微妙（見下面那張表），所以只留一份 —— 兩份複製粘貼遲早分頭長歪。
#
# ══ 為什麼必須逐代收 ═════════════════════════════════════════════════════════
#
# 三個 M0 場景共用 8080–8082，每一個都必須不留殘餘處理程序；留下的話，下一個場景綁不上連接埠，
# 而它的基線探測**照樣會綠**（對著這個殘留處理程序）—— 實際發生過。
#
# ★ `$RUN/m0.pid` 裡**永遠只有最後一代**。而任何發生在「下一代已起、上一代未退」視窗裡的
#   斷言失敗，都會讓上一代活著離開並繼續持有連接埠。
#   ⚠ 一個只是碰巧成立的清理（比如靠收尾時那 30 秒死等把上一代耗走），
#   與一個不存在的清理，區別只在運氣。
#
# ══ 為什麼末代與更早的用不同的信號 ═══════════════════════════════════════════
#
# 信號收不收得動，取決於那一代處在哪個階段 —— **這是量出來的，不是從文件推的**：
#
#   | 那一代的狀態                  | 發 SIGINT                                    |
#   |------------------------------|----------------------------------------------|
#   | 還在 `main_loop` 裡等信號     | **0 秒退出**（Quick，兩個超時都是 0）          |
#   | 已收過 SIGQUIT、正在排空      | ★ **被吞掉**，40 秒後照樣活著                  |
#
# 後一條的原因：處理程序已離開 `main_loop`，沒人再從信號管道裡讀；而 tokio 的處理器仍然裝著，
# 頂掉了 SIGINT 預設的 terminate 行為。**排空中的那一代收不到任何可捕獲的信號**——
# 它只能自己走完 `CLOSE_TIMEOUT`(5) + `grace_period_seconds` + `graceful_shutdown_timeout_seconds`
# （`vendor/pingora/pingora-core/src/server/mod.rs`），在 m0.yaml 的設定下是 35–65 秒。
#
# ★ 所以：**最後起的那一代是活的，用 SIGINT；更早的都在排空，直接 SIGKILL。**
#   對更早那些用 SIGKILL 是**有意的選擇，不是等待失敗後的備援**——收尾時判據已全部做完，
#   它們的 fd 也早已交接出去，只剩「把連接埠空出來」這一件事。
#
# ★ 這條同樣適用於 M1 的 systemd 設計：一旦一代開始排空就叫不動了，
#   `TimeoutStopSec` 必須按 35–65 秒這個量級配。

# 未 init 時保持為空，`lifecycle_cleanup` 會安全地什麼都不做（指令碼可能在起第一代之前就失敗）。
LIFECYCLE_PIDS=""
# ★ 「附屬處理程序」——不是 m0-seam 的某一代，而是指令碼自己拉起的輔助處理程序（目前只有探針）。
#   它們不持有監聽連接埠，所以留下來不會讓下一個場景綁不上；但 `lifecycle_cleanup`
#   聲稱收的是「本指令碼起的每一個處理程序」，**不含它們就是名不副實**——
#   而名不副實的清理，正是本檔案開頭那段教訓的來源。
LIFECYCLE_AUX=""
# 末代收不掉時等多久才強殺。SIGINT 走 fast shutdown，正常應當秒退，所以走到超時就是真異常。
LIFECYCLE_WAIT=${LIFECYCLE_WAIT:-30}

lifecycle_init() {
  LIFECYCLE_PIDS="$1/generations.pids"
  : > "$LIFECYCLE_PIDS"
}

record_gen() {
  [ -n "$LIFECYCLE_PIDS" ] || return 0
  echo "$1" >> "$LIFECYCLE_PIDS"
}

# 登記一個附屬處理程序（探針之類）。收尾時直接收掉，不走「末代/更早」那套。
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

  # 更早的那些都在排空，信號叫不動，直接 SIGKILL（見上）。
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
  # ★ 走到這裡才是真異常：活著的那一代對 fast shutdown 沒反應。
  echo "⚠ 末代 $live 收到 SIGINT 后 ${LIFECYCLE_WAIT} 秒仍没退出，强杀。
   ★ 这不该发生：SIGINT 走 fast shutdown，两个超时都是 0，正常应当秒退。
   若它反复出现，是退出真的卡住了，要去查，而不是把这个数字调大。" >&2
  kill -KILL "$live" 2>/dev/null || true
  sleep 1
}
