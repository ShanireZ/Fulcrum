#!/usr/bin/env bash
# 对拍那一格的**宿主机侧**驱动（M3 第一刀，G132）。
#
#   bash tests/bench/run.sh
#
# ★ ★ 它与 `tests/m0/docker-run.sh` 里那次 `docker run` 是**两回事**，理由与
#   `tests/musl/product.sh` 逐字相同：这一格要的是**另一张镜像**
#   （`docker/Dockerfile.bench`，里面有三家竞品），挂不进那个容器。
#
# ⛔ **本格不产出、也不许产出任何性能数字**（G132）。它判三件事：
#   ① `Dockerfile.bench` 里钉的版本与 `bench/README.md` 那张表对得上；
#   ② `oha` 在两张镜像里是同一个版本（§8 的「同一个负载生成器」）；
#   ③ 那条流水线跑得通，而判据两个方向都判得动（容器内，`tests/bench/gate.sh`）。
#
# ★ 它需要 `target/release/fulcrum` —— 由主构建那一格产出。⇒ 在完整门禁里
#   它排在那次 `docker run` **之后**（同 musl 那一格）。

set -euo pipefail

REPO_UNIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# ★ MSYS 会把容器内路径改写成 Windows 路径，必须关掉；理由同 tests/musl/product.sh。
export MSYS_NO_PATHCONV=1
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO_UNIX")"
else
  REPO_HOST="$REPO_UNIX"
fi

# shellcheck source=tests/lib/vol-lock.sh
. "$REPO_UNIX/tests/lib/vol-lock.sh"

BENCH_IMAGE=${BENCH_IMAGE:-fulcrum-bench:local}
DF_BENCH="$REPO_UNIX/docker/Dockerfile.bench"
DF_BUILD="$REPO_UNIX/docker/Dockerfile.build"
README="$REPO_UNIX/bench/README.md"

FAILS=0
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

# ── 门 ①：钉死的 digest 必须在 README 那张表里 ──────────────────────────────
#
# ★ ★ 判据挂在 **digest** 上而不是版本号：`nginx:1.29-bookworm` 这种 tag 是浮动的，
#   而 digest 是那个镜像的身份本身。⇒ 「表里写的」与「镜像里钉的」不一致时，
#   不一致本身才是要暴露的东西。
# ⚠ ⚠ 这道门存在的直接原因：本仓 2026-09-05 当天栽过一次抄件不同步
#   （一个分解式抄了三处，改一处另两处不会跟着红）。⇒ 抄件必须有门。
echo "── ① 钉死的 digest 与 README 那张表 ──"
scan_digests() { grep -oE 'sha256:[0-9a-f]{64}' "$1" | sort -u; }

# ★ 扫描器自证：它必须能**命中**也能**落空**。
#   ⚠ 一个恒返回空的扫描器，会让下面每一条断言都「通过」，而输出一模一样。
#   ⚠ ⚠ 两处 `|| true` 是**必需的**，不是保险：`set -o pipefail` 下 grep 落空返回 1
#   ⇒ 整条管道非 0 ⇒ `set -e` 当场杀掉脚本，而那正是**落空自证**要走的那条路。
#   （实测：不写它时本脚本在这一行静默退出，只打了一个小标题就 RC=1。）
SELFTEST_HIT=$( { printf 'FROM x@sha256:%064d\n' 0 | grep -oE 'sha256:[0-9a-f]{64}' || true; } | wc -l | tr -d ' ')
SELFTEST_MISS=$( { printf 'FROM x@sha256:tooshort\n' | grep -oE 'sha256:[0-9a-f]{64}' || true; } | wc -l | tr -d ' ')
if [ "$SELFTEST_HIT" = 1 ] && [ "$SELFTEST_MISS" = 0 ]; then
  ok "digest 扫描器自证：命中 1 / 落空 0"
else
  bad "digest 扫描器自证失败（命中 $SELFTEST_HIT，落空 $SELFTEST_MISS）——下面的结论一律不可信"
fi

DIGESTS=$(scan_digests "$DF_BENCH")
N_DIGESTS=$(printf '%s\n' "$DIGESTS" | grep -c . || true)
if [ "$N_DIGESTS" -ge 4 ]; then
  ok "Dockerfile.bench 里钉了 $N_DIGESTS 个 digest"
else
  bad "Dockerfile.bench 里只找到 $N_DIGESTS 个 digest —— 四个来源（三家 + 基础镜像）都该钉"
fi
while IFS= read -r d; do
  [ -n "$d" ] || continue
  if grep -qF "$d" "$README"; then
    ok "README 里有 ${d:0:19}…"
  else
    bad "README 那张表里没有 ${d:0:19}… —— 改了 Dockerfile 却没改表"
  fi
done <<< "$DIGESTS"

# ── 门 ②：oha 在两张镜像里必须是同一个 ─────────────────────────────────────
#
# ★ §8 的方法学写着「同一个负载生成器」。两张镜像各钉一个版本，会让
#   「压力那一格看到的」与「对拍看到的」悄悄分家 —— 而两边都不会红。
echo "── ② oha 在两张镜像里是不是同一个 ──"
oha_arg() { grep -oE "^ARG $1=.*" "$2" | head -1 | cut -d= -f2- ; }
for key in OHA_VERSION OHA_SHA256_AMD64 OHA_SHA256_ARM64; do
  a=$(oha_arg "$key" "$DF_BUILD")
  b=$(oha_arg "$key" "$DF_BENCH")
  if [ -z "$a" ] || [ -z "$b" ]; then
    bad "$key 在某一张 Dockerfile 里读不出来（build='$a' bench='$b'）——「没能检查」不算「检查通过」"
  elif [ "$a" = "$b" ]; then
    ok "$key 两张一致"
  else
    bad "$key 两张不一致：build='$a' bench='$b'"
  fi
done

[ "$FAILS" = 0 ] || {
  echo "BENCH GATE FAILED（宿主机侧）：$FAILS 处" >&2
  exit 1
}

# ── 建镜像：内容变了就重做（同 docker-run.sh 的手法）────────────────────────
#
# ★ 只判「镜像在不在」是不够的：Dockerfile 改了也会照样命中旧镜像，
#   此后每一次「全绿」用的都不是仓库声明的那几个 digest，而没有任何东西会说。
LABEL_KEY="cool.cnb.fulcrum.dockerfile-sha256"
if command -v sha256sum >/dev/null 2>&1; then
  DF_SHA=$(sha256sum "$DF_BENCH" | cut -d' ' -f1)
else
  DF_SHA=""
  echo "⚠ 没有 sha256sum，无法判断对拍镜像是否过期——本次强制重建" >&2
fi
IMG_SHA=$(docker image inspect "$BENCH_IMAGE" --format "{{index .Config.Labels \"$LABEL_KEY\"}}" 2>/dev/null || true)
if [ -z "$DF_SHA" ] || [ "$IMG_SHA" != "$DF_SHA" ]; then
  echo "[bench] building $BENCH_IMAGE …"
  docker build -t "$BENCH_IMAGE" --label "$LABEL_KEY=$DF_SHA" \
    -f "${REPO_HOST}/docker/Dockerfile.bench" "${REPO_HOST}/docker"
fi

# ── 产物：对拍要的是主构建那一格产出的 release 二进制 ───────────────────────
#
# ⚠ 它住在**目标卷**里，不在工作树上 ⇒ 卷名要按与 `docker-run.sh` 完全相同的
#   推导算（`tests/lib/vol-lock.sh` 是唯一那份推导）。⛔ 别在这里另抄一份：
#   两份推导一旦分家，这一格会挂上**另一棵树的**产物，而两边都不红。
BUILD_DF_SHA=$(sha256sum "$DF_BUILD" | cut -d' ' -f1)
TARGET_VOL="$(fulcrum_target_vol "$BUILD_DF_SHA" "$REPO_UNIX")"
if ! docker volume inspect "$TARGET_VOL" >/dev/null 2>&1; then
  echo "BENCH GATE FAILED: 找不到目标卷 $TARGET_VOL —— 先跑一次构建那一格" >&2
  exit 1
fi

echo "── ③ 容器内：流水线与判据 ──"
# ★ ★ 目标卷**只读挂载**。两条理由，第二条是承重的：
#   ① 这一格只要读 `target/release/fulcrum` 一个文件，写权限用不上；
#   ② ⚠ 于是它**不必去抢 `fulcrum_lock_acquire` 那把锁** —— 而那把锁在完整门禁里
#      已经被 `docker-run.sh` 攥着（本格排在那次 `docker run` 之后）。
#      写挂载会让「单独跑本格」与「跟着完整门禁跑」需要两套不同的加锁逻辑，
#      而只读挂载让两条路一模一样。
docker run --rm \
  -v "${REPO_HOST}:/w" \
  -v "${TARGET_VOL}:/w/target:ro" \
  -w /w \
  -e BENCH_GATE_DURATION \
  -e BENCH_GATE_CONNECTIONS \
  "$BENCH_IMAGE" \
  bash tests/bench/gate.sh
