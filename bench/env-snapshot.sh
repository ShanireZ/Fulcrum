#!/usr/bin/env bash
# 环境快照 + **宿主合格性判定**（M3 第一刀，G132 的交付物之二与之三的接缝）。
#
#   bash bench/env-snapshot.sh <输出文件.json>
#
# ★ ★ ★ 它是整条流水线上**唯一**决定「这一趟出不出得了数」的地方。判据本体在
#   `bench/lib.sh`（纯函数、合成输入自测），本文件只负责**采集读数**并把结果落盘。
#   ⇒ 采集错了会被判据看见（读不出来一律判红，⛔「没能检查」不算「检查通过」），
#     判据错了会被 `bench/lib.sh --self-check` 看见。两边各有各的门。
#
# ⚠ 它跑在**容器里**。容器看得见什么、看不见什么，见 `bench/README.md`
#   「合格宿主」一节 —— 那一节是这份快照的口径说明，⛔ 别只读字段名。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT=${1:?用法：bash bench/env-snapshot.sh <输出文件.json>}

# ── 采集 ───────────────────────────────────────────────────────────────────
#
# ★ 每个读数都写成「读得到就用，读不到留空」——⛔ 不给默认值。
#   一个读不出来的值被默认成「看起来正常」的那一刻，判据就瞎了。
read_or_empty() { cat "$1" 2>/dev/null || true; }

KERNEL=$(uname -r 2>/dev/null || true)
NPROC=$(nproc 2>/dev/null || true)
LOAD1=$(read_or_empty /proc/loadavg | awk '{print $1}')
CPU_MODEL=$(awk -F': ' '/^model name/ { print $2; exit }' /proc/cpuinfo 2>/dev/null || true)
MEM_KB=$(awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo 2>/dev/null || true)
CONTAINER_HOST=$(hostname 2>/dev/null || true)

# 人写下来的那一句（容器原理上看不见的三件事）。
ATTEST=${BENCH_HOST_ATTEST:-}

# 被测对象的版本。★ 三家竞品的版本由镜像自己在构建时写死（`/etc/fulcrum-bench-subjects`），
#   ⛔ 不在这里去问它们 —— 「问出来的版本」与「镜像里钉的版本」不一致时，
#   不一致本身才是要暴露的东西，而那由 `tests/bench/run.sh` 的版本门管。
SUBJECTS=$(read_or_empty /etc/fulcrum-bench-subjects)
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}
FULCRUM_VER=$("$FULCRUM_BIN" --version 2>/dev/null || true)

# 内核参数：一张**固定**清单，逐条读。
# ⚠ ⚠ 容器有自己的 netns ⇒ 这里读到的多半是**容器的**值而不是宿主的。
#   ⇒ 它们记进快照是为了「这一趟到底跑在什么参数下」可被第三方看到，
#   ⛔ **不**作为合格性判据的输入 —— 那三件容器看不见的事归 `attest` 那一条管。
SYSCTLS="net.core.somaxconn net.ipv4.tcp_max_syn_backlog net.ipv4.ip_local_port_range net.ipv4.tcp_tw_reuse net.core.netdev_max_backlog fs.file-max"

# CPU 亲和。★ ★ 判据要的是「两组核**都**指定了」这一件事，⇒ 只设一个等于没设：
#   那样负载生成器仍然会跑到被测那批核上，而读数看起来完全正常。
AFFINITY=""
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  AFFINITY="server=${BENCH_SERVER_CPUS} load=${BENCH_LOAD_CPUS}"
fi

# ── 判合格性（判据在 lib.sh）────────────────────────────────────────────────
DISQ=$(bench_disqualifiers "$KERNEL" "$NPROC" "$LOAD1" "$ATTEST" "$AFFINITY")
if [ -z "$DISQ" ]; then QUALIFIED=true; else QUALIFIED=false; fi

# ── 落盘 ───────────────────────────────────────────────────────────────────
#
# ★ 用 python3 序列化，⛔ 不手工拼 JSON：手拼的那一刻，任何一个带引号或反斜杠的
#   读数（CPU 型号里就有）都会产出一份**语法上坏掉、但看起来很像 JSON** 的文件。
export SNAP_KERNEL="$KERNEL" SNAP_NPROC="$NPROC" SNAP_LOAD1="$LOAD1" \
  SNAP_CPU="$CPU_MODEL" SNAP_MEM_KB="$MEM_KB" SNAP_HOST="$CONTAINER_HOST" \
  SNAP_ATTEST="$ATTEST" SNAP_SUBJECTS="$SUBJECTS" SNAP_FULCRUM="$FULCRUM_VER" \
  SNAP_DISQ="$DISQ" SNAP_QUALIFIED="$QUALIFIED" SNAP_SYSCTLS="$SYSCTLS" \
  SNAP_AFFINITY="$AFFINITY" \
  SNAP_OUT="$OUT" \
  SNAP_MIN_CPUS="$BENCH_MIN_CPUS" SNAP_MAX_LOAD="$BENCH_MAX_IDLE_LOAD" \
  SNAP_DURATION="${BENCH_DURATION:-}" SNAP_CONNECTIONS="${BENCH_CONNECTIONS:-}" \
  SNAP_WORKERS="${BENCH_WORKERS:-}" SNAP_PAYLOAD_BYTES="${BENCH_PAYLOAD_BYTES:-}"

python3 "$BENCH_DIR/snapshot-json.py"

echo "[bench/env] 快照已写入 $OUT"
if [ "$QUALIFIED" = true ]; then
  echo "[bench/env] 宿主判定：**合格**"
else
  echo "[bench/env] 宿主判定：**不合格** —— 本趟不会产出任何性能结论："
  printf '%s\n' "$DISQ" | sed 's/^/           · /'
fi
