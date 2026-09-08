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
# ⚠ ⚠ ★ **枢衡没有 `--version` 这个参数**（它退 2 并打用法）⇒ 这一行**恒得到空串**，
#   而空串在快照里与「问过了，它没有版本」长得一模一样。⛔ 留着它只是为了
#   将来真加了这个参数时能自动接上，**它不是身份的来源**。
FULCRUM_VER_RC=0
FULCRUM_VER=$("$FULCRUM_BIN" --version 2>/dev/null) || FULCRUM_VER_RC=$?
if [ "$FULCRUM_VER_RC" -ne 0 ]; then
  # ⚠ ⚠ ★ 快照里**一个字段都不改**（口径归 G19 / `G147` 管，⛔ 不在这一笔里动）——
  #   这里只是把上面那句自认的毛病「空串与『问过了，它没有版本』长得一模一样」
  #   在**日志里**分开：真因由枢衡自己的原话说。
  # ★ 于是「为什么这一格是空的」不再需要事后猜；⛔ 它仍然不是身份的来源。
  fulcrum_ver_err=$("$FULCRUM_BIN" --version 2>&1 >/dev/null) || true
  echo "★ \`$FULCRUM_BIN --version\` 退了 $FULCRUM_VER_RC ⇒ 快照里 fulcrum_version 会是空的。" >&2
  echo "  ⛔ 这个空**不**表示「问过了，它没有版本」。它自己的原话：" >&2
  printf '%s\n' "$fulcrum_ver_err" | sed 's/^/      /' >&2
  echo "  ★ 这一趟量的到底是哪个二进制，看 fulcrum_sha256 与 fulcrum_build_id —— 那两项才是身份。" >&2
fi
# ★ ★ ★ 身份靠这两个读数，⛔ 不靠上面那一行：
#   ① **sha256** —— 一个文件总有一个，它是「量的到底是哪个二进制」的**唯一**可靠答案；
#   ② **内嵌的构建身份**（G141 的 `FULCRUM_BUILD_VERSION`，semver 构建元数据语法）——
#      它把那个 sha256 映射回一次提交。⚠ G141 自己写明它**不判工作树脏不脏**
#      ⇒ 同一提交上带着不同未提交改动的两次构建，这一项相同而 sha256 不同。
#      ★ 正因如此两个都要记：①分得开的它分得开，②说得出出处的它说得出出处。
# ⚠ `fulcrum_build_info` 那个指标要服务跑起来且开了 metrics 才拿得到 ⇒ 这里够不着。
FULCRUM_SHA=$(sha256sum "$FULCRUM_BIN" 2>/dev/null | cut -d' ' -f1 || true)
FULCRUM_BUILD_ID=$(grep -aoE '[0-9]+\.[0-9]+\.[0-9]+\+[0-9a-zA-Z._-]+' "$FULCRUM_BIN" 2>/dev/null | sort -u | head -1 || true)

# ── 内核参数与资源上限（G145）───────────────────────────────────────────────
#
# ★ ★ ★ G145 之前这一段只是**记录**，明写着「⛔ 不作为合格性判据的输入」，
#   而「内核参数已固化」是 `attest` 那句人写的声明里的第三件。
#   2026-09-06 的实测把那条路整个否掉了：四个键是 **per-netns** 的，
#   宿主上 `sysctl --system` 过的值**一个都传不进这个容器** ——
#   ⇒ 一句完全诚实的「已固化」声明，对被测行为零影响。
#   ⇒ 现在它们由 `docker run --sysctl` 在容器里设，而这里**逐条断言实测==声明**。
#
# ⛔ 读法用 `/proc/sys/...` 直读，**不调 `sysctl(8)`**：那个工具不在镜像里时
#   `sysctl -n x` 会走到「命令不存在」那条路，而一个恒失败的原语与「值不符」
#   在输出上分不开（`tests/stress/run.sh` 栽过同形状的：写了 `ss` 而镜像里没有）。
# ⛔ **读不到就不打这一行** —— 补一个空值等于把「这个键不存在」伪装成「值是空」，
#   而 `bench_kparam_mismatches` 恰恰要把前者判红。
read_sysctl_kv() {
  local key path val
  for key in "$@"; do
    path=/proc/sys/$(printf '%s' "$key" | tr '.' '/')
    if val=$(cat "$path" 2>/dev/null); then
      printf '%s=%s\n' "$key" "$val"
    fi
  done
}

# 记进快照的那张清单**比要判的宽**：多记 `netdev_max_backlog` 与 `fs.file-max`
# 是为了让第三方看到「容器里到底读得到什么」——
# ★ 实测这两条在容器里的表现完全相反：前者**读不到**（非 netns 化，不出现在容器的
#   /proc/sys/net 视图里），后者读得到而且就是**宿主的**值。
SYSCTLS="net.core.somaxconn net.ipv4.tcp_max_syn_backlog net.ipv4.ip_local_port_range net.ipv4.tcp_tw_reuse net.core.netdev_max_backlog fs.file-max"

DECLARED=$(bench_container_sysctls)
# ⚠ 只读声明里那几个键来做比较，⛔ 不拿上面那张更宽的清单 ——
#   多记的两条本来就读不到 / 不归容器管，拿它们去比会得到两条必然的假红。
DECLARED_KEYS=$(printf '%s\n' "$DECLARED" | cut -d= -f1)
# shellcheck disable=SC2086  # 有意分词：DECLARED_KEYS 是一串以空白分隔的键名
OBSERVED=$(read_sysctl_kv $DECLARED_KEYS)
KPARAM=$(bench_kparam_mismatches "$DECLARED" "$OBSERVED")

# nofile：★ 实测容器缺省只有 **1024**，而 `fs.file-max` 是 9.2e18 ——
#   会咬的从来不是后者。⇒ 由 `docker run --ulimit` 设，在这里核。
NOFILE=$(ulimit -n 2>/dev/null || true)
if [ "$NOFILE" != "$BENCH_NOFILE" ]; then
  KPARAM=$(printf '%s\n%s' "$KPARAM" "nofile 实测 '${NOFILE}' ≠ 声明 '${BENCH_NOFILE}'")
fi

# 宿主侧那一条（`net.core.netdev_max_backlog`）：⛔ 容器里读不到，只能由启动器
# 在宿主上读了再传进来。
# ★ ★ 这一格判的是**启动器到底跑没跑过** —— 不用 `bench/docker-run.sh` 起容器，
#   这个变量就不存在，于是判红。⛔ 它不是「相信这个变量」，它是「没有它就不算数」。
HOST_KV=${BENCH_HOST_SYSCTLS:-}
DECLARED_HOST=$(bench_host_sysctls)
HOST_MISMATCH=$(bench_kparam_mismatches "$DECLARED_HOST" "$HOST_KV")
if [ -n "$HOST_MISMATCH" ]; then
  KPARAM=$(printf '%s\n%s' "$KPARAM" "$(printf '%s\n' "$HOST_MISMATCH" | sed 's/^/宿主侧 /')")
fi

# ⚠ 上面三次拼接都可能在开头留下空行（KPARAM 一开始可能是空串）⇒ 去掉空行，
#   ⛔ 否则一个只含换行的字符串会让 `[ -n ]` 成立，判出一条内容为空的「不合格理由」。
KPARAM=$(printf '%s\n' "$KPARAM" | grep -v '^[[:space:]]*$' || true)

# CPU 亲和。★ ★ 判据要的是「两组核**都**指定了」这一件事，⇒ 只设一个等于没设：
#   那样负载生成器仍然会跑到被测那批核上，而读数看起来完全正常。
AFFINITY=""
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  AFFINITY="server=${BENCH_SERVER_CPUS} load=${BENCH_LOAD_CPUS}"
fi

# ── 站点根落在哪种文件系统上（口径的一部分，⛔ 不是元数据）─────────────────
#
# ★ ★ ★ 枢衡的静态文件路径先在本线程上试一次 `preadv2(RWF_NOWAIT)`，而
#   **认不认由文件系统各自决定**（实测 ext4 认、overlayfs 与 tmpfs 回 `EOPNOTSUPP`）
#   ⇒ 同一个二进制、同一台机器，站点根换一种文件系统，静态吞吐差 45%。
#   ⛔ 一份不说出这一格的读数是复现不出来的（G19）。
# ⚠ 本步只**采读数**，⛔ 不判红：跑在 overlayfs 上仍然是一次有效的测量，
#   量的只是另一种部署形状 —— 而那件事必须**写在数据里**，不能靠谁记得。
# ⚠ 用例还没跑 ⇒ 探的是站点根**将要落在**的那个目录，不是站点根本身。
SITE_ROOT=${BENCH_WWW_ROOT:-${TMPDIR:-/tmp}}
SITE_FS=unknown
SITE_NOWAIT=unknown
if [ -d "$SITE_ROOT" ]; then
  while IFS='=' read -r k v; do
    case "$k" in
      fs) SITE_FS=$v ;;
      rwf_nowait) SITE_NOWAIT=$v ;;
      *) ;;
    esac
  done <<EOF
$(python3 "$BENCH_DIR/site-root-probe.py" "$SITE_ROOT" 2>/dev/null || true)
EOF
fi

# ── 判合格性（判据在 lib.sh）────────────────────────────────────────────────
# ⚠ ⚠ 第六个参数**必须传** —— `bench_disqualifiers` 给了它默认值（为了不推翻
#   G145 之前写的每一条调用），⇒ 漏传时它会安静地不判内核参数那一格。
#   ★ 这里是它在真实路径上**唯一**的一次调用。
DISQ=$(bench_disqualifiers "$KERNEL" "$NPROC" "$LOAD1" "$ATTEST" "$AFFINITY" "$KPARAM")
if [ -z "$DISQ" ]; then QUALIFIED=true; else QUALIFIED=false; fi

# ── 落盘 ───────────────────────────────────────────────────────────────────
#
# ★ 用 python3 序列化，⛔ 不手工拼 JSON：手拼的那一刻，任何一个带引号或反斜杠的
#   读数（CPU 型号里就有）都会产出一份**语法上坏掉、但看起来很像 JSON** 的文件。
export SNAP_KERNEL="$KERNEL" SNAP_NPROC="$NPROC" SNAP_LOAD1="$LOAD1" \
  SNAP_CPU="$CPU_MODEL" SNAP_MEM_KB="$MEM_KB" SNAP_HOST="$CONTAINER_HOST" \
  SNAP_ATTEST="$ATTEST" SNAP_SUBJECTS="$SUBJECTS" SNAP_FULCRUM="$FULCRUM_VER" \
  SNAP_FULCRUM_SHA="$FULCRUM_SHA" SNAP_FULCRUM_BUILD_ID="$FULCRUM_BUILD_ID" \
  SNAP_DISQ="$DISQ" SNAP_QUALIFIED="$QUALIFIED" SNAP_SYSCTLS="$SYSCTLS" \
  SNAP_AFFINITY="$AFFINITY" \
  SNAP_DECLARED_CONTAINER="$DECLARED" SNAP_DECLARED_HOST="$DECLARED_HOST" \
  SNAP_HOST_KV="$HOST_KV" SNAP_NOFILE="$NOFILE" SNAP_NOFILE_DECLARED="$BENCH_NOFILE" \
  SNAP_OUT="$OUT" \
  SNAP_MIN_CPUS="$BENCH_MIN_CPUS" SNAP_MAX_LOAD="$BENCH_MAX_IDLE_LOAD" \
  SNAP_DURATION="$BENCH_DURATION" SNAP_CONNECTIONS="$BENCH_CONNECTIONS" \
  SNAP_WORKERS="$BENCH_WORKERS" SNAP_PAYLOAD_BYTES="$BENCH_PAYLOAD_BYTES" \
  SNAP_SITE_ROOT="$SITE_ROOT" SNAP_SITE_FS="$SITE_FS" SNAP_SITE_NOWAIT="$SITE_NOWAIT"

python3 "$BENCH_DIR/snapshot-json.py"

echo "[bench/env] 快照已写入 $OUT"
if [ "$QUALIFIED" = true ]; then
  echo "[bench/env] 宿主判定：**合格**"
else
  echo "[bench/env] 宿主判定：**不合格** —— 本趟不会产出任何性能结论："
  printf '%s\n' "$DISQ" | sed 's/^/           · /'
fi
