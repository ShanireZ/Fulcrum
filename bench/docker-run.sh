#!/usr/bin/env bash
# 对拍的**宿主机侧启动器**（G145）。真要出数那天跑的就是它。
#
#   BENCH_HOST_ATTEST="专机 · 无 TUN 代理" \
#   BENCH_SERVER_CPUS=0-1 BENCH_LOAD_CPUS=2-3 \
#   BENCH_FULCRUM_BIN=/path/to/fulcrum \
#     bash bench/docker-run.sh [输出目录]
#
# ★ ★ ★ **它存在的唯一理由是那一个键**：`net.core.netdev_max_backlog` 非 netns 化
#   ⇒ 它**设不进容器**（runc 当场拒绝），也**读不到容器里**
#   （`/proc/sys/net/core/netdev_max_backlog: No such file or directory`）。
#   ⇒ 只能在宿主上设、在宿主上判。本脚本就是那道门。
#
# ⚠ ⚠ 而它同时是「另外四个键真的被设上了」的**唯一**保证：那四个键由这里的
#   `--sysctl` 旗标带进容器，`bench/env-snapshot.sh` 在容器内逐条断言。
#   ⇒ 绕过本脚本手起容器 ⇒ 快照拿不到 `BENCH_HOST_SYSCTLS` ⇒ 判成不合格。
#   ★ 那不是「相信这个环境变量」，是「没有它就不算数」—— 一个能被伪造的凭证，
#     与一个根本不存在的检查，在**忘了跑**这件事上的表现完全不同。
#
# ⛔ 本脚本不判定、不比较、不打印任何性能数字。判定在容器里的 `bench/run.sh`。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_UNIX="$(cd "$BENCH_DIR/.." && pwd)"
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_REL=${1:-bench/out}

# ★ MSYS 会把容器内路径改写成 Windows 路径，必须关掉；理由同 tests/bench/run.sh。
export MSYS_NO_PATHCONV=1
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO_UNIX")"
else
  REPO_HOST="$REPO_UNIX"
fi

BENCH_IMAGE=${BENCH_IMAGE:-fulcrum-bench:local}
FULCRUM_BIN_HOST=${BENCH_FULCRUM_BIN:-$REPO_UNIX/target/release/fulcrum}

# ── 门 ①：宿主侧那个键 ─────────────────────────────────────────────────────
#
# ⛔ 读法用 `/proc/sys/...` 直读，不调 `sysctl(8)`：理由同 env-snapshot.sh ——
#   一个恒失败的原语与「值不符」在输出上分不开。
# ★ 比较本身复用 `bench_kparam_mismatches`（纯函数，已被 5 条注入反证钉过）
#   ⇒ ⛔ 这里不另写一遍「怎么算相符」。
read_host_sysctl_kv() {
  local key path val
  while IFS= read -r key; do
    key=${key%%=*}
    [ -n "$key" ] || continue
    path=/proc/sys/$(printf '%s' "$key" | tr '.' '/')
    if val=$(cat "$path" 2>/dev/null); then
      printf '%s=%s\n' "$key" "$val"
    fi
  done <<EOF
$(bench_host_sysctls)
EOF
}

HOST_KV=$(read_host_sysctl_kv)
HOST_MISMATCH=$(bench_kparam_mismatches "$(bench_host_sysctls)" "$HOST_KV")
if [ -n "$HOST_MISMATCH" ]; then
  echo "BENCH LAUNCH REFUSED：宿主内核参数没有按 bench/sysctl.conf 固化" >&2
  printf '%s\n' "$HOST_MISMATCH" | sed 's/^/  · /' >&2
  cat >&2 <<'FIXME'

  修法（★ 机器级改动，与 owner 另行批）：
      cp bench/sysctl.conf /etc/sysctl.d/99-fulcrum-bench.conf
      sysctl --system
      cat /proc/sys/net/core/netdev_max_backlog     # 核一遍，⛔ 别只看 sysctl --system 的回显

  ⚠ ⚠ ⛔ **不要**改成「读不到就跳过」。这个键读不到只有两种可能：
     ① 你在容器里跑这个脚本（它是宿主机侧的，⛔ 跑错地方了）；
     ② 这个内核没有这个键 —— 那这台机器的环回队列深度就是**不可知**的，
        而「不可知」不算「够用」。
FIXME
  exit 1
fi
echo "[bench/launch] 宿主内核参数逐条相符：$(printf '%s' "$HOST_KV" | tr '\n' ' ')"

# ── 产物必须在 ─────────────────────────────────────────────────────────────
if [ ! -x "$FULCRUM_BIN_HOST" ]; then
  echo "BENCH LAUNCH REFUSED：找不到可执行的枢衡产物 $FULCRUM_BIN_HOST" >&2
  echo "  ⇒ 用 BENCH_FULCRUM_BIN=<路径> 指过去，或先在构建那一格产出一个。" >&2
  exit 1
fi
if command -v cygpath >/dev/null 2>&1; then
  FULCRUM_BIN_MOUNT="$(cygpath -m "$FULCRUM_BIN_HOST")"
else
  FULCRUM_BIN_MOUNT="$FULCRUM_BIN_HOST"
fi

# ── 旗标：从 lib.sh 那一份声明推导 ──────────────────────────────────────────
#
# ⚠ 逐行读进数组，⛔ 不做无引号分词：`10240 65535` 自己带空格。
DOCKER_ARGS=()
while IFS= read -r tok; do
  [ -n "$tok" ] || continue
  DOCKER_ARGS+=("$tok")
done <<EOF
$(bench_docker_sysctl_flags)
EOF

echo "[bench/launch] 镜像 $BENCH_IMAGE · 产物 $FULCRUM_BIN_HOST · 输出 $OUT_REL"

# ★ 把宿主实测值经环境变量传进去 —— 容器读不到它，只能这样过河。
# ── 站点根挂在一个匿名卷上（⇒ ext4），⛔ 不是容器可写层 ────────────────────
#
# ★ ★ ★ **这是口径不是编排细节。** 枢衡的静态文件路径先在本线程上试一次
#   `preadv2(RWF_NOWAIT)`，而**认不认由文件系统各自决定** —— 2026-09-06 实测
#   ext4 认、**overlayfs 与 tmpfs 回 `EOPNOTSUPP`**；同一个二进制、同一台机器，
#   站点根换一种文件系统，静态吞吐差 **45%**。
# ⚠ ⚠ 而在此之前站点根来自 `mktemp -d`，落在**容器可写层（overlayfs）**上 ——
#   那是编排的副产品，**从来没人挑过**，却决定了被测的哪条路径会被走到。
#   ⇒ 而枢衡按 G13 的分发形状（systemd + 单静态二进制）在生产上是 ext4/xfs。
# ★ 用**匿名卷**（`-v /bench-www`，没有源路径）而不是命名卷：`--rm` 会连它一起删
#   ⇒ ⛔ 不跨趟留残留、不用管回收，也不会被门禁那道「说不出属主的卷」判据挑出来
#   （它认得 docker 匿名卷那 64 位十六进制名字）。
# ⚠ 用 `${VAR-default}`（**没有冒号**）⇒ 显式传一个空串就回到 overlayfs 那种形状，
#   而两种形状都由 `bench/env-snapshot.sh` 写进 `env.json` 的 `site_root` 里
#   ⇒ ⛔ 不管走哪条，原始数据都说得出自己是在哪种形状下量的。
# ⚠ 必须 `export`：下面那行 `-e BENCH_WWW_ROOT`（不带 `=`）取的是**本进程环境里**
#   的值，而一个没导出的普通赋值在那里是看不见的 —— 症状是站点根静默落回
#   容器可写层，而输出看起来完全正常。
export BENCH_WWW_ROOT=${BENCH_WWW_ROOT-/bench-www}

docker run --rm \
  -v "${REPO_HOST}:/w" \
  -v "${FULCRUM_BIN_MOUNT}:/w/target/release/fulcrum:ro" \
  -v /bench-www \
  -w /w \
  "${DOCKER_ARGS[@]}" \
  -e BENCH_HOST_SYSCTLS="$HOST_KV" \
  -e BENCH_HOST_ATTEST \
  -e BENCH_SERVER_CPUS \
  -e BENCH_LOAD_CPUS \
  -e BENCH_DURATION \
  -e BENCH_CONNECTIONS \
  -e BENCH_WORKERS \
  -e BENCH_PAYLOAD_BYTES \
  -e BENCH_MIN_SPREAD \
  -e BENCH_GEN_HEADROOM \
  -e BENCH_WWW_ROOT \
  "$BENCH_IMAGE" \
  bash bench/run.sh "$OUT_REL"
