#!/usr/bin/env bash
# ACME 兩個場景那套**失敗現場取證**（`lib.sh` 的 `acme_dump_ports` 一族）的判據。
#
# ★ ★ **為什麼它必須存在**：取證程式碼只在「已經要紅」的那條路徑上執行 ⇒ 一趟綠的門禁
#   從來不碰它。而本儲存庫反覆栽的正是這個形狀 —— 一段只在出事那天才第一次執行的程式碼，
#   出事那天才發現它自己是壞的，於是那一次現場也白丟了。
#   ⇒ 這裡把它拆成兩段各自可判的：
#     ① 解碼（`/proc/net/tcp{,6}` → 位址:連接埠 狀態 inode）—— 拿固定樣本，`lib.sh` 自帶；
#     ② **串接**（inode → 是哪個處理程序持有）—— 本檔案：自己開一個監聽 socket，
#        要求取證能指名道姓地把**本處理程序**認出來，然後關掉它、要求它不再被認出來。
#
# ★ ② 的兩個方向缺一不可：只驗「開著的時候看得見」，一個恆說「看得見」的實作照樣全綠。
#
# ⚠ 它**不碰 docker、不需要產品二進位**，所以掛在 lint 那一格
#   （同 `tests/ci/dump-cache.sh --self-check` 的理由）。
#
# 用法：
#   bash tests/acme/self-check.sh
#
# 結束代碼：0 = 全部通過；1 = 有判據不通過（現場打在 stderr 上）。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"

# `lib.sh` 在 source 之前要求呼叫方設好這三個。取證那幾個函式一個都不讀它們，
# 但**不設就等於賭它以後也不讀** —— 按它自己寫的約定設。
WORK=$(mktemp -d)
HOST=127.0.0.1
TRUST_ANCHOR="$WORK/unused.crt"
# ⚠ 靶子處理程序也要收：中途任何一條判據把指令碼帶走時，它還睡著 120 秒。
cleanup() {
  [ -z "${PY:-}" ] || kill "$PY" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# shellcheck source=tests/acme/lib.sh
. "$(dirname "$0")/lib.sh"

BAD=0
bad() { echo "  ✗ $*" >&2; BAD=$((BAD + 1)); }

echo "=== [1/2] 解码：拿固定样本两个方向各走一遍 ==="
if acme_selftest_decode; then
  echo "  ✓ /proc/net 解码自测通过（v4 / v6 / 状态名 / 表头挡得掉）"
else
  bad "解码自测没过 —— 取证那张表在真出事那天会是错的"
fi

echo "=== [2/2] 接线：取证必须能把持有 socket 的进程指出来 ==="
# ⚠ 這一格只在 Linux 上成立（要 /proc/net 與 /proc/<pid>/fd）。門禁全在容器裡跑，
#   所以**讀不到就是紅**，不是「跳過」—— 一個悄悄跳過的判據等於沒有判據。
if [ ! -r /proc/net/tcp ]; then
  bad "读不到 /proc/net/tcp —— 取证在这台机器上根本问不出东西"
else
  # 自己開一個監聽 socket。★ 連接埠交給核心挑（bind 到 0），
  #   不寫死一個「應該沒人用」的連接埠 —— 那種假設遲早會撞上別人。
  # ⚠ 只監聽、不接受連接 ⇒ 關掉之後不會留 TIME_WAIT，下面的反方向才幹淨。
  python3 -u -c 'import socket, time
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
print(s.getsockname()[1], flush=True)
time.sleep(120)' > "$WORK/port" &
  PY=$!

  PORT=""
  for _ in $(seq 1 100); do
    PORT=$(head -1 "$WORK/port" 2>/dev/null || true)
    [ -z "$PORT" ] || break
    sleep 0.1
  done

  if [ -z "$PORT" ]; then
    bad "起不来那个用来当靶子的监听 socket（python3 没吐出端口）"
  else
    SNAP=$(acme_port_snapshot "$PORT")
    case $SNAP in
      *LISTEN*) echo "  ✓ 端口 $PORT 上那条 LISTEN 被看见了" ;;
      *) bad "端口 $PORT 明明在 LISTEN，取证却看不见它（拿到的是：${SNAP:-空})" ;;
    esac

    # ★ ★ 這一條才是本檔案的理由：不是「看得見有個 socket」，
    #   而是**能說出是誰**。CI 上那次紅缺的正是這一句。
    DUMP=$(acme_dump_ports "自检" 2>&1 || true)
    if printf '%s' "$DUMP" | grep -qE "^  127\.0\.0\.1:$PORT +LISTEN .*持有者:.* $PY\("; then
      echo "  ✓ 取证把持有者指成了 pid $PY（本进程开的那个 socket）"
    else
      bad "取证没能把端口 $PORT 的持有者指成 pid $PY —— 下一次 CI 红照样问不出「是谁」。现场："
      printf '%s\n' "$DUMP" | grep -E ":$PORT |持有者" >&2 || true
    fi
  fi

  # ── 反方向：靶子關掉之後，同一把尺子必須說「沒有了」───────────────
  kill "$PY" 2>/dev/null || true
  wait "$PY" 2>/dev/null || true
  if [ -n "$PORT" ]; then
    GONE=""
    for _ in $(seq 1 50); do
      GONE=$(acme_port_snapshot "$PORT")
      [ -n "$GONE" ] || break
      sleep 0.1
    done
    if [ -z "$GONE" ]; then
      echo "  ✓ 靶子关掉之后端口 $PORT 上就什么都没有了（判据不是恒真）"
    else
      bad "靶子已经关掉，取证却还说端口 $PORT 上有东西：$GONE"
    fi
  fi
fi

if [ "$BAD" -ne 0 ]; then
  echo "ACME SELF-CHECK FAILED: $BAD 条判据不通过" >&2
  exit 1
fi
echo "ACME SELF-CHECK PASSED —— 失败现场取证的解码与接线两段都验过，且各自有反方向。"
