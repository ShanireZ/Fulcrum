#!/usr/bin/env bash
# 對拍那一格的**宿主機側**驅動（M3 第一刀，G132）。
#
#   bash tests/bench/run.sh
#
# ★ ★ 它與 `tests/m0/docker-run.sh` 裡那次 `docker run` 是**兩回事**，理由與
#   `tests/musl/product.sh` 逐字相同：這一格要的是**另一張鏡像**
#   （`docker/Dockerfile.bench`，裡面有三家競品），掛不進那個容器。
#
# ⛔ **本格不產出、也不許產出任何效能數字**（G132）。它判三件事：
#   ① `Dockerfile.bench` 裡釘的版本與 `bench/README.md` 那張表對得上；
#   ② `oha` 在兩張鏡像裡是同一個版本（§8 的「同一個負載生成器」）；
#   ③ 那條流水線跑得通，而判據兩個方向都判得動（容器內，`tests/bench/gate.sh`）。
#
# ★ 它需要 `target/release/fulcrum` —— 由主建置那一格產出。⇒ 在完整門禁裡
#   它排在那次 `docker run` **之後**（同 musl 那一格）。

set -euo pipefail

REPO_UNIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# ★ MSYS 會把容器內路徑改寫成 Windows 路徑，必須關掉；理由同 tests/musl/product.sh。
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

# ── 門 ①：釘死的 digest 必須在 README 那張表裡 ──────────────────────────────
#
# ★ ★ 判據掛在 **digest** 上而不是版本號：`nginx:1.29-bookworm` 這種 tag 是浮動的，
#   而 digest 是那個鏡像的身分本身。⇒ 「表裡寫的」與「鏡像裡釘的」不一致時，
#   不一致本身才是要暴露的東西。
# ⚠ ⚠ 這道門存在的直接原因：本儲存庫 2026-09-05 當天栽過一次抄件不同步
#   （一個分解式抄了三處，改一處另兩處不會跟著紅）。⇒ 抄件必須有門。
echo "── ① 钉死的 digest 与 README 那张表 ──"
scan_digests() { grep -oE 'sha256:[0-9a-f]{64}' "$1" | sort -u; }

# ★ 掃描器自證：它必須能**命中**也能**落空**。
#   ⚠ 一個恆回傳空的掃描器，會讓下面每一條斷言都「通過」，而輸出一模一樣。
#   ⚠ ⚠ 兩處 `|| true` 是**必需的**，不是保險：`set -o pipefail` 下 grep 落空回傳 1
#   ⇒ 整條管道非 0 ⇒ `set -e` 當場殺掉指令碼，而那正是**落空自證**要走的那條路。
#   （實測：不寫它時本指令碼在這一行靜默退出，只打了一個小標題就 RC=1。）
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

# ── 門 ②：oha 在兩張鏡像裡必須是同一個 ─────────────────────────────────────
#
# ★ §8 的方法學寫著「同一個負載生成器」。兩張鏡像各釘一個版本，會讓
#   「壓力那一格看到的」與「對拍看到的」悄悄分家 —— 而兩邊都不會紅。
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

# ── 建鏡像：內容變了就重做（同 docker-run.sh 的手法）────────────────────────
#
# ★ 只判「鏡像在不在」是不夠的：Dockerfile 改了也會照樣命中舊鏡像，
#   此後每一次「全綠」用的都不是儲存庫宣告的那幾個 digest，而沒有任何東西會說。
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

# ── 產物：對拍要的是主建置那一格產出的 release 二進位 ───────────────────────
#
# ⚠ 它住在**目標卷**裡，不在工作樹上 ⇒ 卷名要按與 `docker-run.sh` 完全相同的
#   推導算（`tests/lib/vol-lock.sh` 是唯一那份推導）。⛔ 別在這裡另抄一份：
#   兩份推導一旦分家，這一格會掛上**另一棵樹的**產物，而兩邊都不紅。
BUILD_DF_SHA=$(sha256sum "$DF_BUILD" | cut -d' ' -f1)
TARGET_VOL="$(fulcrum_target_vol "$BUILD_DF_SHA" "$REPO_UNIX")"
if ! docker volume inspect "$TARGET_VOL" >/dev/null 2>&1; then
  echo "BENCH GATE FAILED: 找不到目标卷 $TARGET_VOL —— 先跑一次构建那一格" >&2
  exit 1
fi

echo "── ③ 容器内：流水线与判据 ──"
# ★ ★ 目標卷**唯讀掛載**。兩條理由，第二條是承重的：
#   ① 這一格只要讀 `target/release/fulcrum` 一個檔案，寫權限用不上；
#   ② ⚠ 於是它**不必去搶 `fulcrum_lock_acquire` 那把鎖** —— 而那把鎖在完整門禁裡
#      已經被 `docker-run.sh` 攥著（本格排在那次 `docker run` 之後）。
#      寫掛載會讓「單獨跑本格」與「跟著完整門禁跑」需要兩套不同的加鎖邏輯，
#      而唯讀掛載讓兩條路一模一樣。
#
# ── 核心參數旗標（G145）───────────────────────────────────────────────────
#
# ★ ★ 從 `bench/lib.sh` 那**一份**宣告推導，⛔ 不在這裡另抄一遍：旗標與宣告一旦
#   分家，容器會按 A 跑而快照按 B 判，兩邊都不紅。
#
# ⚠ ⚠ **有意不設 `BENCH_HOST_SYSCTLS`**。那是宿主側那個鍵的憑證，而這臺開發機
#   給不出來（WSL2 核心里根本沒有 `net.core.netdev_max_backlog` 這個鍵）。
#   ⇒ 這一趟因此必然多出一條 `kernel-params: 宿主侧 …` 的不合格理由 —— **那是對的**：
#   ★ ★ ★ 於是這道門在每一次門禁裡**兩個方向都被走到**：容器側那四個鍵因為旗標
#   真的設上了而**相符**（正向），宿主側那一條因為沒有憑證而**判紅**（反向）。
#   ⛔ 別為了「讓輸出好看」把它補上 —— 補上就等於把反向那一半永久關掉。
#   兩個方向各由 `tests/bench/gate.sh` 的 C12 / C13 釘著。
# shellcheck source=bench/lib.sh
. "$REPO_UNIX/bench/lib.sh"
SYSCTL_ARGS=()
while IFS= read -r tok; do
  [ -n "$tok" ] || continue
  SYSCTL_ARGS+=("$tok")
done <<EOF
$(bench_docker_sysctl_flags)
EOF

docker run --rm \
  -v "${REPO_HOST}:/w" \
  -v "${TARGET_VOL}:/w/target:ro" \
  -w /w \
  "${SYSCTL_ARGS[@]}" \
  -e BENCH_GATE_DURATION \
  -e BENCH_GATE_CONNECTIONS \
  "$BENCH_IMAGE" \
  bash tests/bench/gate.sh
