#!/usr/bin/env bash
# 對拍的**宿主機側啟動器**（G145）。真要出數那天跑的就是它。
#
#   BENCH_HOST_ATTEST="專機 · 無 TUN 代理" \
#   BENCH_SERVER_CPUS=0-1 BENCH_LOAD_CPUS=2-3 \
#   BENCH_FULCRUM_BIN=/path/to/fulcrum \
#     bash bench/docker-run.sh [輸出目錄]
#
# ★ ★ ★ **它存在的唯一理由是那一個鍵**：`net.core.netdev_max_backlog` 非 netns 化
#   ⇒ 它**設不進容器**（runc 當場拒絕），也**讀不到容器裡**
#   （`/proc/sys/net/core/netdev_max_backlog: No such file or directory`）。
#   ⇒ 只能在宿主上設、在宿主上判。本指令碼就是那道門。
#
# ⚠ ⚠ 而它同時是「另外四個鍵真的被設上了」的**唯一**保證：那四個鍵由這裡的
#   `--sysctl` 旗標帶進容器，`bench/env-snapshot.sh` 在容器內逐條斷言。
#   ⇒ 繞過本指令碼手起容器 ⇒ 快照拿不到 `BENCH_HOST_SYSCTLS` ⇒ 判成不合格。
#   ★ 那不是「相信這個環境變數」，是「沒有它就不算數」—— 一個能被偽造的憑證，
#     與一個根本不存在的檢查，在**忘了跑**這件事上的表現完全不同。
#
# ⛔ 本指令碼不判定、不比較、不列印任何效能數字。判定在容器裡的 `bench/run.sh`。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_UNIX="$(cd "$BENCH_DIR/.." && pwd)"
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_REL=${1:-bench/out}

# ★ MSYS 會把容器內路徑改寫成 Windows 路徑，必須關掉；理由同 tests/bench/run.sh。
export MSYS_NO_PATHCONV=1
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO_UNIX")"
else
  REPO_HOST="$REPO_UNIX"
fi

BENCH_IMAGE=${BENCH_IMAGE:-fulcrum-bench:local}
FULCRUM_BIN_HOST=${BENCH_FULCRUM_BIN:-$REPO_UNIX/target/release/fulcrum}

# ── 門 ①：宿主側那個鍵 ─────────────────────────────────────────────────────
#
# ⛔ 讀法用 `/proc/sys/...` 直讀，不調 `sysctl(8)`：理由同 env-snapshot.sh ——
#   一個恆失敗的原語與「值不符」在輸出上分不開。
# ★ 比較本身重複使用 `bench_kparam_mismatches`（純函式，已被 5 條注入反證釘過）
#   ⇒ ⛔ 這裡不另寫一遍「怎麼算相符」。
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

# ── 產物必須在 ─────────────────────────────────────────────────────────────
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

# ── 旗標：從 lib.sh 那一份宣告推導 ──────────────────────────────────────────
#
# ⚠ 逐行讀進陣列，⛔ 不做無引號分詞：`10240 65535` 自己帶空格。
DOCKER_ARGS=()
while IFS= read -r tok; do
  [ -n "$tok" ] || continue
  DOCKER_ARGS+=("$tok")
done <<EOF
$(bench_docker_sysctl_flags)
EOF

echo "[bench/launch] 镜像 $BENCH_IMAGE · 产物 $FULCRUM_BIN_HOST · 输出 $OUT_REL"

# ★ 把宿主實測值經環境變數傳進去 —— 容器讀不到它，只能這樣過河。
# ── 站台根掛在一個匿名捲上（⇒ ext4），⛔ 不是容器可寫層 ────────────────────
#
# ★ ★ ★ **這是口徑不是編排細節。** 樞衡的靜態檔案路徑先在本執行緒上試一次
#   `preadv2(RWF_NOWAIT)`，而**認不認由檔案系統各自決定** —— 2026-09-06 實測
#   ext4 認、**overlayfs 與 tmpfs 回 `EOPNOTSUPP`**；同一個二進位、同一臺機器，
#   站台根換一種檔案系統，靜態吞吐差 **45%**。
# ⚠ ⚠ 而在此之前站台根來自 `mktemp -d`，落在**容器可寫層（overlayfs）**上 ——
#   那是編排的副產品，**從來沒人挑過**，卻決定了被測的哪條路徑會被走到。
#   ⇒ 而樞衡按 G13 的散布形狀（systemd + 單靜態二進位）在生產上是 ext4/xfs。
# ★ 用**匿名卷**（`-v /bench-www`，沒有源路徑）而不是命名卷：`--rm` 會連它一起刪
#   ⇒ ⛔ 不跨趟留殘留、不用管回收，也不會被門禁那道「說不出屬主的卷」判據挑出來
#   （它認得 docker 匿名卷那 64 位十六進制名字）。
# ⚠ 用 `${VAR-default}`（**沒有冒號**）⇒ 顯式傳一個空串就回到 overlayfs 那種形狀，
#   而兩種形狀都由 `bench/env-snapshot.sh` 寫進 `env.json` 的 `site_root` 裡
#   ⇒ ⛔ 不管走哪條，原始資料都說得出自己是在哪種形狀下量的。
# ⚠ 必須 `export`：下面那行 `-e BENCH_WWW_ROOT`（不帶 `=`）取的是**本處理程序環境裡**
#   的值，而一個沒匯出的普通賦值在那裡是看不見的 —— 症狀是站台根靜默落回
#   容器可寫層，而輸出看起來完全正常。
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
