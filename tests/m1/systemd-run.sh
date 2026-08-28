#!/usr/bin/env bash
# M1 的 systemd 场景驱动：把产物放进一个**以 systemd 为 PID 1** 的容器里跑。
#
#   bash tests/m1/systemd-run.sh                   # 构建 + 四个场景
#   M1_SKIP_BUILD=1  bash tests/m1/systemd-run.sh  # 跳过构建（由 docker-run.sh 调用时用）
#   M1_ONLY=product  bash tests/m1/systemd-run.sh  # 只跑**产品二进制**场景
#   M1_ONLY=main     bash tests/m1/systemd-run.sh  # 只跑 spike 主场景（机制那一层）
#   M1_ONLY=rev      bash tests/m1/systemd-run.sh  # 只跑 ExitType=main 反证场景
#   M1_ONLY=handover bash tests/m1/systemd-run.sh  # 只跑「被否掉的 MainPID 交接」场景
#   M1_KEEP=1        bash tests/m1/systemd-run.sh  # 失败后**不删容器**，留着进去看现场
#
# 四个场景的分工：
#   product.sh          ★ **产品二进制**在 systemd 下起得来、换得了代、停得干净（G78）
#   run.sh              spike：机制那一层（fd 移交、CLOEXEC、升级 socket 不泄漏）
#   exit-type-main.sh   去掉 ExitType=cgroup 会怎样 —— 证明那一行确实在干活
#   mainpid-handover.sh G31 推断的那条路会怎样 —— 证明它为什么被否掉
#
# ★ ★ ★ **为什么产品那一个是才有的**：在它之前，这里三个场景跑的全是
#   spike 二进制，而 spike 把 sd_notify / pid 文件 / SIGUSR2 换代**自己实现了一遍**。
#   于是三个场景全绿，产品二进制却 `systemctl start` 超时失败（实测）。
#   > **夹具喂给门的那个二进制，本身也是夹具的一部分。**
#
# ★ 为什么必须是另一个容器：M0 那套跑在构建镜像里（rust 工具链，PID 1 是 bash）。
#   本场景要验的东西**只存在于 systemd 里**——MainPID、cgroup 生命周期、KillMode。
#   同一个容器同时做这两件事，会让每次改测试宿主都触发一次全量重编译。
set -euo pipefail

REPO_UNIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# 卷名的推导与门禁互斥锁：与 tests/m0/docker-run.sh 共用同一份，不在这里重算。
VOL_LOCK_LIB="$REPO_UNIX/tests/lib/vol-lock.sh"
# shellcheck source=tests/lib/vol-lock.sh
. "$VOL_LOCK_LIB"

# ── 构建 ────────────────────────────────────────────────────────────────────
# ★ 不自己写一份构建逻辑：行尾检查、构建镜像的内容哈希重建、缓存卷，全都在 docker-run.sh 里，
#   抄一份过来必然分头长歪（本仓库已经在 lifecycle.sh 上吃过一次）。
if [ "${M1_SKIP_BUILD:-0}" != "1" ]; then
  echo "[m1] 先构建（复用 tests/m0/docker-run.sh 的构建路径）"
  BUILD_ONLY=1 bash "$REPO_UNIX/tests/m0/docker-run.sh"
fi

export MSYS_NO_PATHCONV=1
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO_UNIX")"
else
  REPO_HOST="$REPO_UNIX"
fi
DOCKER_CTX="${REPO_HOST}/docker"

# ── 产物在哪个 target 卷里 ───────────────────────────────────────────────────
#
# ★ ★ **不自己重算一遍卷名。** docker-run.sh 用 `docker/Dockerfile.build` 的内容哈希
#   做卷名后缀，这里改成**从构建镜像的 label 上读回来**——同一个事实只有一个来源，
#   于是不可能算出两个不同的值。
#
# ★ 并且当场核对它与**当前** Dockerfile 是否一致：不一致说明构建没跑或跑的是旧镜像，
#   此时若照旧挂上去，跑的就是**上一个工具链编出来的二进制**，而且会报绿。
#   ——「拿不到就强制重建」在 docker-run.sh 里；这里拿不到就**判红**，因为这里不负责构建。
BUILD_IMAGE=${IMAGE:-fulcrum-build:local}
LABEL_KEY="cool.cnb.fulcrum.dockerfile-sha256"
IMAGE_SHA=$(docker image inspect "$BUILD_IMAGE" --format "{{index .Config.Labels \"$LABEL_KEY\"}}" 2>/dev/null || true)
[ -n "$IMAGE_SHA" ] || {
  echo "★ 读不到构建镜像 $BUILD_IMAGE 的 $LABEL_KEY 标签。" >&2
  echo "  先跑一次 \`bash tests/m0/docker-run.sh\`（或去掉 M1_SKIP_BUILD）。" >&2
  exit 1
}
if command -v sha256sum >/dev/null 2>&1; then
  NOW_SHA=$(sha256sum "${REPO_UNIX}/docker/Dockerfile.build" | cut -d' ' -f1)
  [ "$NOW_SHA" = "$IMAGE_SHA" ] || {
    echo "★ 构建镜像里记的 Dockerfile 哈希是 ${IMAGE_SHA:0:12}…，而当前文件是 ${NOW_SHA:0:12}…" >&2
    echo "  也就是说本机的产物是**旧工具链**编的。先跑 \`bash tests/m0/docker-run.sh\`。" >&2
    exit 1
  }
fi
# ★ ★ 卷名还带着**这棵工作树**的短哈希（理由写在 tests/lib/vol-lock.sh 顶部：
#   一台机器上有好几棵工作树，而它们曾经共用同一个 /w/target）。
#   ⚠ 这里同样**不自己拼字符串** —— 拼一遍就是给下一次改名埋一处安静的分叉，
#     而分叉的表现是本格挂上一个自动新建的**空卷**，然后抱怨找不到产品二进制。
TARGET_VOL="$(fulcrum_target_vol "$IMAGE_SHA" "$REPO_UNIX")"

# ★ 与 M0 同一把锁（锁名＝卷名）：本格与 M0 挂的是同一个卷，并发跑一样会互相踩。
# ⚠ 由 docker-run.sh 调进来时它已经持锁，靠导出的 FULCRUM_GATE_LOCK_HELD 直接放行；
#   上面那次 `BUILD_ONLY=1 docker-run.sh` 也是它自己取自己放，所以取锁必须排在构建之后。
fulcrum_lock_acquire "$TARGET_VOL" "$REPO_UNIX" || exit 1

# ── 测试宿主镜像（systemd）─────────────────────────────────────────────────
#
# 与构建镜像同一条纪律：**内容变了就重做**，而不是「在就用」。
# 理由见 docker/Dockerfile.systemd 顶部——systemd 的版本是本 spike 结论的一部分。
SYSTEMD_IMAGE=${SYSTEMD_IMAGE:-fulcrum-systemd:local}
SYSTEMD_DOCKERFILE="${REPO_UNIX}/docker/Dockerfile.systemd"
if command -v sha256sum >/dev/null 2>&1; then
  SYSTEMD_SHA=$(sha256sum "$SYSTEMD_DOCKERFILE" | cut -d' ' -f1)
elif command -v git >/dev/null 2>&1; then
  SYSTEMD_SHA="githash-$(git hash-object "$SYSTEMD_DOCKERFILE")"
else
  SYSTEMD_SHA=""
  echo "⚠ 既没有 sha256sum 也没有 git，无法判断测试宿主镜像是否过期——本次强制重建" >&2
fi
CUR_SHA=$(docker image inspect "$SYSTEMD_IMAGE" --format "{{index .Config.Labels \"$LABEL_KEY\"}}" 2>/dev/null || true)
if [ -z "$SYSTEMD_SHA" ] || [ "$CUR_SHA" != "$SYSTEMD_SHA" ]; then
  echo "[m1] building $SYSTEMD_IMAGE …"
  docker build -t "$SYSTEMD_IMAGE" --label "$LABEL_KEY=$SYSTEMD_SHA" \
    -f "${DOCKER_CTX}/Dockerfile.systemd" "$DOCKER_CTX"
fi

# ── 跑一个场景 ──────────────────────────────────────────────────────────────
#
# ★ **每个场景一个全新容器。** 比「一个容器跑两遍再清理」强得多：M0 那边正是因为
#   上一个场景留下了进程，下一个场景的基线探测对着残留进程报了绿（实际发生过）。
#   systemd 的状态（failed 计数、drop-in、journal）更难清干净，所以这里直接不复用。
run_scenario() {
  local name=$1 script=$2 rc=0
  # ★ 必须另起一句：`local` 的**所有参数在赋值发生之前就已经展开完了**，
  #   写成 `local name=$1 cname="…${name}"` 时 `${name}` 取的是赋值前的值（这里是未定义，
  #   `set -u` 当场炸）。这条第一次跑就咬了一口。
  local cname="fulcrum-m1-${name}"
  echo
  echo "════════ 场景 ${name}：${script} ════════"
  docker rm -f "$cname" >/dev/null 2>&1 || true

  # --privileged + 私有 cgroup namespace 是 systemd 在容器里跑起来的最低要求。
  # ★ ★ **千万不要再把宿主机的 /sys/fs/cgroup 挂进来。** 实测：那样做会让
  #   容器内的 cgroup 视图与它自己的 namespace 对不上，journald 直接起不来
  #   （"Failed to acquire cgroup root path"），而**更糟的是 MAINPID= 交接会被静默丢弃**
  #   ——systemd 无法把发信进程解析到某个 unit 上。⚠ 它很容易被误读成「systemd 不接受交接」
  #   这个完全错误的结论。privileged 下 docker 自己会把正确的 cgroup2 视图挂成 rw。
  docker run -d --name "$cname" \
    --privileged --cgroupns=private \
    --tmpfs /run --tmpfs /run/lock \
    -v "${REPO_HOST}:/w" \
    -v "${TARGET_VOL}:/w/target" \
    "$SYSTEMD_IMAGE" >/dev/null

  # 等 systemd 起来。★ 不用 sleep：等它自己说话。
  local booted=""
  for _ in $(seq 1 60); do
    booted=$(docker exec "$cname" systemctl is-system-running 2>/dev/null || true)
    case "$booted" in running | degraded) break ;; esac
    sleep 0.5
  done
  case "$booted" in
    running | degraded) ;;
    *)
      echo "★ 容器里的 systemd 30 秒内没起来（is-system-running=${booted:-<空>}）" >&2
      docker exec "$cname" systemctl list-units --state=failed --no-pager || true
      docker rm -f "$cname" >/dev/null 2>&1 || true
      return 1
      ;;
  esac
  if [ "$booted" = "degraded" ]; then
    # ★ degraded 要把是谁挂了打出来。容器里有几个 unit 起不来是正常的，
    #   但「正常的 degraded」和「真的有东西坏了」必须能分开看。
    echo "  systemd 状态 degraded，失败的 unit："
    docker exec "$cname" systemctl list-units --state=failed --no-pager --no-legend | sed 's/^/      /'
  fi
  docker exec "$cname" cat /etc/fulcrum-systemd-image | sed 's/^/  /'

  # ★ 装**全部** unit 文件，而不是点名一个：场景与 unit 是一对一的
  #   （spike 三个用 fulcrum-m1.service，产品那个用 fulcrum-prod.service），
  #   点名的写法会在加第四个场景时安静地少装一个 —— 而症状是「unit 没被加载」，
  #   看起来像 systemd 的问题。⚠ 装了不等于启动：没有 `systemctl enable`，
  #   `[Install]` 段不生效，每个场景只 start 自己那一个。
  docker exec "$cname" bash -c \
    'install -m 0644 /w/tests/m1/*.service /etc/systemd/system/ && systemctl daemon-reload'

  docker exec -e RUST_LOG="${RUST_LOG:-info}" "$cname" bash "/w/tests/m1/${script}" || rc=$?

  if [ "$rc" -ne 0 ] && [ "${M1_KEEP:-0}" = "1" ]; then
    echo "★ M1_KEEP=1：容器 $cname 保留着，进去看现场：" >&2
    echo "      docker exec -it $cname bash" >&2
  else
    docker rm -f "$cname" >/dev/null 2>&1 || true
  fi
  return "$rc"
}

FAILED=0
case "${M1_ONLY:-all}" in
  product) run_scenario product product.sh || FAILED=1 ;;
  main) run_scenario main run.sh || FAILED=1 ;;
  rev) run_scenario exit-type-main exit-type-main.sh || FAILED=1 ;;
  handover) run_scenario mainpid-handover mainpid-handover.sh || FAILED=1 ;;
  *)
    # ★ 四个场景全都跑完再汇总，**即使前面已经红了**：
    #   产品场景是「产品真的能被托管吗」，spike 主场景是「机制成立吗」，
    #   另两个是「不这么做会怎样」。只看一个红，分不清是机制没生效、
    #   是产品没接上、还是判据本身分不清好坏。
    # ★ 产品排第一：它坏了最要紧，也最可能是刚改出来的。
    run_scenario product product.sh || FAILED=1
    run_scenario main run.sh || FAILED=1
    run_scenario exit-type-main exit-type-main.sh || FAILED=1
    run_scenario mainpid-handover mainpid-handover.sh || FAILED=1
    ;;
esac

echo
if [ "$FAILED" -eq 0 ]; then
  echo "M1 systemd 场景：全部通过。"
else
  echo "M1 systemd 场景：有场景失败（见上）。" >&2
fi
exit "$FAILED"
