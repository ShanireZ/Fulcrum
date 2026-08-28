#!/usr/bin/env bash
# ACME 两个场景那套**失败现场取证**（`lib.sh` 的 `acme_dump_ports` 一族）的判据。
#
# ★ ★ **为什么它必须存在**：取证代码只在「已经要红」的那条路径上执行 ⇒ 一趟绿的门禁
#   从来不碰它。而本仓库反复栽的正是这个形状 —— 一段只在出事那天才第一次运行的代码，
#   出事那天才发现它自己是坏的，于是那一次现场也白丢了。
#   ⇒ 这里把它拆成两段各自可判的：
#     ① 解码（`/proc/net/tcp{,6}` → 地址:端口 状态 inode）—— 拿固定样本，`lib.sh` 自带；
#     ② **接线**（inode → 是哪个进程持有）—— 本文件：自己开一个监听 socket，
#        要求取证能指名道姓地把**本进程**认出来，然后关掉它、要求它不再被认出来。
#
# ★ ② 的两个方向缺一不可：只验「开着的时候看得见」，一个恒说「看得见」的实现照样全绿。
#
# ⚠ 它**不碰 docker、不需要产品二进制**，所以挂在 lint 那一格
#   （同 `tests/ci/dump-cache.sh --self-check` 的理由）。
#
# 用法：
#   bash tests/acme/self-check.sh
#
# 退出码：0 = 全部通过；1 = 有判据不通过（现场打在 stderr 上）。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"

# `lib.sh` 在 source 之前要求调用方设好这三个。取证那几个函数一个都不读它们，
# 但**不设就等于赌它以后也不读** —— 按它自己写的约定设。
WORK=$(mktemp -d)
HOST=127.0.0.1
TRUST_ANCHOR="$WORK/unused.crt"
# ⚠ 靶子进程也要收：中途任何一条判据把脚本带走时，它还睡着 120 秒。
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
# ⚠ 这一格只在 Linux 上成立（要 /proc/net 与 /proc/<pid>/fd）。门禁全在容器里跑，
#   所以**读不到就是红**，不是「跳过」—— 一个悄悄跳过的判据等于没有判据。
if [ ! -r /proc/net/tcp ]; then
  bad "读不到 /proc/net/tcp —— 取证在这台机器上根本问不出东西"
else
  # 自己开一个监听 socket。★ 端口交给内核挑（bind 到 0），
  #   不写死一个「应该没人用」的端口 —— 那种假设迟早会撞上别人。
  # ⚠ 只监听、不接受连接 ⇒ 关掉之后不会留 TIME_WAIT，下面的反方向才干净。
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

    # ★ ★ 这一条才是本文件的理由：不是「看得见有个 socket」，
    #   而是**能说出是谁**。CI 上那次红缺的正是这一句。
    DUMP=$(acme_dump_ports "自检" 2>&1 || true)
    if printf '%s' "$DUMP" | grep -qE "^  127\.0\.0\.1:$PORT +LISTEN .*持有者:.* $PY\("; then
      echo "  ✓ 取证把持有者指成了 pid $PY（本进程开的那个 socket）"
    else
      bad "取证没能把端口 $PORT 的持有者指成 pid $PY —— 下一次 CI 红照样问不出「是谁」。现场："
      printf '%s\n' "$DUMP" | grep -E ":$PORT |持有者" >&2 || true
    fi
  fi

  # ── 反方向：靶子关掉之后，同一把尺子必须说「没有了」───────────────
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
