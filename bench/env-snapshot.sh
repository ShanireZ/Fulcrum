#!/usr/bin/env bash
# 環境快照 + **宿主合格性判定**（M3 第一刀，G132 的交付物之二與之三的接縫）。
#
#   bash bench/env-snapshot.sh <输出文件.json>
#
# ★ ★ ★ 它是整條流水線上**唯一**決定「這一趟出不出得了數」的地方。判據本體在
#   `bench/lib.sh`（純函式、合成輸入自測），本檔案只負責**採集讀數**並把結果寫入磁碟。
#   ⇒ 採集錯了會被判據看見（讀不出來一律判紅，⛔「沒能檢查」不算「檢查通過」），
#     判據錯了會被 `bench/lib.sh --self-check` 看見。兩邊各有各的門。
#
# ⚠ 它跑在**容器裡**。容器看得見什麼、看不見什麼，見 `bench/README.md`
#   「合格宿主」一節 —— 那一節是這份快照的口徑說明，⛔ 別隻讀欄位名。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT=${1:?用法：bash bench/env-snapshot.sh <输出文件.json>}

# ── 採集 ───────────────────────────────────────────────────────────────────
#
# ★ 每個讀數都寫成「讀得到就用，讀不到留空」——⛔ 不給預設值。
#   一個讀不出來的值被預設成「看起來正常」的那一刻，判據就瞎了。
read_or_empty() { cat "$1" 2>/dev/null || true; }

KERNEL=$(uname -r 2>/dev/null || true)
NPROC=$(nproc 2>/dev/null || true)
LOAD1=$(read_or_empty /proc/loadavg | awk '{print $1}')
CPU_MODEL=$(awk -F': ' '/^model name/ { print $2; exit }' /proc/cpuinfo 2>/dev/null || true)
MEM_KB=$(awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo 2>/dev/null || true)
CONTAINER_HOST=$(hostname 2>/dev/null || true)

# 人寫下來的那一句（容器原理上看不見的三件事）。
ATTEST=${BENCH_HOST_ATTEST:-}

# 被測物件的版本。★ 三家競品的版本由鏡像自己在建置時寫死（`/etc/fulcrum-bench-subjects`），
#   ⛔ 不在這裡去問它們 —— 「問出來的版本」與「鏡像裡釘的版本」不一致時，
#   不一致本身才是要暴露的東西，而那由 `tests/bench/run.sh` 的版本門管。
SUBJECTS=$(read_or_empty /etc/fulcrum-bench-subjects)
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}
# ⚠ ⚠ ★ **樞衡沒有 `--version` 這個參數**（它退 2 並打用法）⇒ 這一行**恆得到空串**，
#   而空串在快照裡與「問過了，它沒有版本」長得一模一樣。⛔ 留著它只是為了
#   將來真加了這個參數時能自動接上，**它不是身分的來源**。
FULCRUM_VER_RC=0
FULCRUM_VER=$("$FULCRUM_BIN" --version 2>/dev/null) || FULCRUM_VER_RC=$?
if [ "$FULCRUM_VER_RC" -ne 0 ]; then
  # ⚠ ⚠ ★ 快照裡**一個欄位都不改**（口徑歸 G19 / `G147` 管，⛔ 不在這一筆裡動）——
  #   這裡只是把上面那句自認的毛病「空串與『問過了，它沒有版本』長得一模一樣」
  #   在**紀錄裡**分開：真因由樞衡自己的原話說。
  # ★ 於是「為什麼這一格是空的」不再需要事後猜；⛔ 它仍然不是身分的來源。
  fulcrum_ver_err=$("$FULCRUM_BIN" --version 2>&1 >/dev/null) || true
  echo "★ \`$FULCRUM_BIN --version\` 退了 $FULCRUM_VER_RC ⇒ 快照里 fulcrum_version 会是空的。" >&2
  echo "  ⛔ 这个空**不**表示「问过了，它没有版本」。它自己的原话：" >&2
  printf '%s\n' "$fulcrum_ver_err" | sed 's/^/      /' >&2
  echo "  ★ 这一趟量的到底是哪个二进制，看 fulcrum_sha256 与 fulcrum_build_id —— 那两项才是身份。" >&2
fi
# ★ ★ ★ 身分靠這兩個讀數，⛔ 不靠上面那一行：
#   ① **sha256** —— 一個檔案總有一個，它是「量的到底是哪個二進位」的**唯一**可靠答案；
#   ② **內嵌的建置身分**（G141 的 `FULCRUM_BUILD_VERSION`，semver 建置中繼資料語法）——
#      它把那個 sha256 映射回一次提交。⚠ G141 自己寫明它**不判工作樹髒不髒**
#      ⇒ 同一提交上帶著不同未提交改動的兩次建置，這一項相同而 sha256 不同。
#      ★ 正因如此兩個都要記：①分得開的它分得開，②說得出出處的它說得出出處。
# ⚠ `fulcrum_build_info` 那個指標要服務跑起來且開了 metrics 才拿得到 ⇒ 這裡夠不著。
FULCRUM_SHA=$(sha256sum "$FULCRUM_BIN" 2>/dev/null | cut -d' ' -f1 || true)
# ★ 取法在 `bench/lib.sh` 的 `bench_build_id_from_bytes`（帶自測）：2026-09-10 視窗二那份快照裡
#   這一格粘上了產物裡緊挨著的下一個字串字面值，而原先這裡那條正則**沒有收尾邊界**。
# ⚠ `{ …; } 2>/dev/null` 包住重定向：產物不存在時 `<` 那一步的回報錯誤也進 `/dev/null`。
FULCRUM_BUILD_ID=$( { bench_build_id_from_bytes < "$FULCRUM_BIN"; } 2>/dev/null || true)

# ── 核心參數與資源上限（G145）───────────────────────────────────────────────
#
# ★ ★ ★ G145 之前這一段只是**記錄**，明寫著「⛔ 不作為合格性判據的輸入」，
#   而「核心參數已固化」是 `attest` 那句人寫的宣告裡的第三件。
#   2026-09-06 的實測把那條路整個否掉了：四個鍵是 **per-netns** 的，
#   宿主上 `sysctl --system` 過的值**一個都傳不進這個容器** ——
#   ⇒ 一句完全誠實的「已固化」宣告，對被測行為零影響。
#   ⇒ 現在它們由 `docker run --sysctl` 在容器裡設，而這裡**逐條斷言實測==宣告**。
#
# ⛔ 讀法用 `/proc/sys/...` 直讀，**不調 `sysctl(8)`**：那個工具不在鏡像裡時
#   `sysctl -n x` 會走到「命令不存在」那條路，而一個恆失敗的原語與「值不符」
#   在輸出上分不開（`tests/stress/run.sh` 栽過同形狀的：寫了 `ss` 而鏡像裡沒有）。
# ⛔ **讀不到就不打這一行** —— 補一個空值等於把「這個鍵不存在」偽裝成「值是空」，
#   而 `bench_kparam_mismatches` 恰恰要把前者判紅。
read_sysctl_kv() {
  local key path val
  for key in "$@"; do
    path=/proc/sys/$(printf '%s' "$key" | tr '.' '/')
    if val=$(cat "$path" 2>/dev/null); then
      printf '%s=%s\n' "$key" "$val"
    fi
  done
}

# 記進快照的那張清單**比要判的寬**：多記 `netdev_max_backlog` 與 `fs.file-max`
# 是為了讓第三方看到「容器裡到底讀得到什麼」——
# ★ 實測這兩條在容器裡的表現完全相反：前者**讀不到**（非 netns 化，不出現在容器的
#   /proc/sys/net 視圖裡），後者讀得到而且就是**宿主的**值。
SYSCTLS="net.core.somaxconn net.ipv4.tcp_max_syn_backlog net.ipv4.ip_local_port_range net.ipv4.tcp_tw_reuse net.core.netdev_max_backlog fs.file-max"

DECLARED=$(bench_container_sysctls)
# ⚠ 唯讀宣告裡那幾個鍵來做比較，⛔ 不拿上面那張更寬的清單 ——
#   多記的兩條本來就讀不到 / 不歸容器管，拿它們去比會得到兩條必然的假紅。
DECLARED_KEYS=$(printf '%s\n' "$DECLARED" | cut -d= -f1)
# shellcheck disable=SC2086  # 有意分詞：DECLARED_KEYS 是一串以空白分隔的鍵名
OBSERVED=$(read_sysctl_kv $DECLARED_KEYS)
KPARAM=$(bench_kparam_mismatches "$DECLARED" "$OBSERVED")

# nofile：★ 實測容器預設只有 **1024**，而 `fs.file-max` 是 9.2e18 ——
#   會咬的從來不是後者。⇒ 由 `docker run --ulimit` 設，在這裡核。
NOFILE=$(ulimit -n 2>/dev/null || true)
if [ "$NOFILE" != "$BENCH_NOFILE" ]; then
  KPARAM=$(printf '%s\n%s' "$KPARAM" "nofile 实测 '${NOFILE}' ≠ 声明 '${BENCH_NOFILE}'")
fi

# 宿主側那一條（`net.core.netdev_max_backlog`）：⛔ 容器裡讀不到，只能由啟動器
# 在宿主上讀了再傳進來。
# ★ ★ 這一格判的是**啟動器到底跑沒跑過** —— 不用 `bench/docker-run.sh` 起容器，
#   這個變數就不存在，於是判紅。⛔ 它不是「相信這個變數」，它是「沒有它就不算數」。
HOST_KV=${BENCH_HOST_SYSCTLS:-}
DECLARED_HOST=$(bench_host_sysctls)
HOST_MISMATCH=$(bench_kparam_mismatches "$DECLARED_HOST" "$HOST_KV")
if [ -n "$HOST_MISMATCH" ]; then
  KPARAM=$(printf '%s\n%s' "$KPARAM" "$(printf '%s\n' "$HOST_MISMATCH" | sed 's/^/宿主侧 /')")
fi

# ⚠ 上面三次拼接都可能在開頭留下空行（KPARAM 一開始可能是空串）⇒ 去掉空行，
#   ⛔ 否則一個只含換行的字串會讓 `[ -n ]` 成立，判出一條內容為空的「不合格理由」。
KPARAM=$(printf '%s\n' "$KPARAM" | grep -v '^[[:space:]]*$' || true)

# CPU 親和。★ ★ 判據要的是「兩組核**都**指定了」這一件事，⇒ 只設一個等於沒設：
#   那樣負載生成器仍然會跑到被測那批核上，而讀數看起來完全正常。
AFFINITY=""
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  AFFINITY="server=${BENCH_SERVER_CPUS} load=${BENCH_LOAD_CPUS}"
fi

# CPU 拓撲：每個邏輯 CPU 的超執行緒兄弟（2026-10-05 加；診斷，⛔ 不判合格性）。
# ★ 上面那條親和只說「兩組核不相交」，說不出兩組是否**共用物理核**（超執行緒兄弟互搶執行單元）——
#   開發機上 `0-1` 就是同一個物理核的兩個超執行緒，而合格宿主的拓撲在此之前一次都沒記過
#   ⇒ 兩臺機器上同一個親和量到的比例不同時，少了這一格就說不清是不是口徑不同。
# ⚠ 讀不到就一行都不寫（快照裡記 null = 「沒記到」，⛔ 不是「沒有超執行緒」）。
CPU_SIBLINGS=""
for f in /sys/devices/system/cpu/cpu[0-9]*/topology/thread_siblings_list; do
  [ -r "$f" ] || continue
  cpu=${f#/sys/devices/system/cpu/cpu}
  CPU_SIBLINGS="${CPU_SIBLINGS}${cpu%%/*}=$(cat "$f")
"
done

# ── 站台根落在哪種檔案系統上（口徑的一部分，⛔ 不是中繼資料）─────────────────
#
# ★ ★ ★ 樞衡的靜態檔案路徑先在本執行緒上試一次 `preadv2(RWF_NOWAIT)`，而
#   **認不認由檔案系統各自決定**（實測 ext4 認、overlayfs 與 tmpfs 回 `EOPNOTSUPP`）
#   ⇒ 同一個二進位、同一臺機器，站台根換一種檔案系統，靜態吞吐差 45%。
#   ⛔ 一份不說出這一格的讀數是重現不出來的（G19）。
# ⚠ 本步只**採讀數**，⛔ 不判紅：跑在 overlayfs 上仍然是一次有效的測量，
#   量的只是另一種部署形狀 —— 而那件事必須**寫在資料裡**，不能靠誰記得。
# ⚠ 用例還沒跑 ⇒ 探的是站台根**將要落在**的那個目錄，不是站台根本身。
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

# ★ ★ 磁碟快取的根落在哪種檔案系統上，與站台根**同等重要**（`G150` ②）：
#   那一格量的是「磁碟命中的 p99 − 記憶體命中的 p99」，而磁碟那一半直接由檔案系統決定。
#   ⛔ 不記它就是 `G147` 那個缺陷原樣重演 —— 一份說不出自己快取落在什麼檔案系統上的
#   p99 讀數**重現不出來**（2026-09-06 實測過：站台根換一種檔案系統，靜態吞吐差 45%）。
# ⚠ 與 site_root 同一條紀律：本步只**採讀數**，⛔ 不判紅。
# ⚠ 用例還沒跑 ⇒ 探的是快取根**將要落在**的那個目錄（`bench/diag/cache-hit-p99.sh`
#   在 `$BENCH_WWW_ROOT` 下按 pid 開子目錄），不是快取根本身。
CACHE_ROOT=${BENCH_WWW_ROOT:-${TMPDIR:-/tmp}}
CACHE_FS=unknown
CACHE_NOWAIT=unknown
if [ -d "$CACHE_ROOT" ]; then
  while IFS='=' read -r k v; do
    case "$k" in
      fs) CACHE_FS=$v ;;
      rwf_nowait) CACHE_NOWAIT=$v ;;
      *) ;;
    esac
  done <<EOF
$(python3 "$BENCH_DIR/site-root-probe.py" "$CACHE_ROOT" 2>/dev/null || true)
EOF
fi

# ── 判合格性（判據在 lib.sh）────────────────────────────────────────────────
# ⚠ ⚠ 第六個參數**必須傳** —— `bench_disqualifiers` 給了它預設值（為了不推翻
#   G145 之前寫的每一條呼叫），⇒ 漏傳時它會安靜地不判核心參數那一格。
#   ★ 這裡是它在真實路徑上**唯一**的一次呼叫。
DISQ=$(bench_disqualifiers "$KERNEL" "$NPROC" "$LOAD1" "$ATTEST" "$AFFINITY" "$KPARAM")
if [ -z "$DISQ" ]; then QUALIFIED=true; else QUALIFIED=false; fi

# ── 寫入磁碟 ───────────────────────────────────────────────────────────────────
#
# ★ 用 python3 序列化，⛔ 不手動拼 JSON：手拼的那一刻，任何一個帶引號或反斜槓的
#   讀數（CPU 型號裡就有）都會產出一份**語法上壞掉、但看起來很像 JSON** 的檔案。
export SNAP_KERNEL="$KERNEL" SNAP_NPROC="$NPROC" SNAP_LOAD1="$LOAD1" \
  SNAP_CPU="$CPU_MODEL" SNAP_MEM_KB="$MEM_KB" SNAP_HOST="$CONTAINER_HOST" \
  SNAP_ATTEST="$ATTEST" SNAP_SUBJECTS="$SUBJECTS" SNAP_FULCRUM="$FULCRUM_VER" \
  SNAP_FULCRUM_SHA="$FULCRUM_SHA" SNAP_FULCRUM_BUILD_ID="$FULCRUM_BUILD_ID" \
  SNAP_DISQ="$DISQ" SNAP_QUALIFIED="$QUALIFIED" SNAP_SYSCTLS="$SYSCTLS" \
  SNAP_AFFINITY="$AFFINITY" SNAP_CPU_SIBLINGS="$CPU_SIBLINGS" \
  SNAP_SERVER_CPUS="${BENCH_SERVER_CPUS:-}" SNAP_LOAD_CPUS="${BENCH_LOAD_CPUS:-}" \
  SNAP_DECLARED_CONTAINER="$DECLARED" SNAP_DECLARED_HOST="$DECLARED_HOST" \
  SNAP_HOST_KV="$HOST_KV" SNAP_NOFILE="$NOFILE" SNAP_NOFILE_DECLARED="$BENCH_NOFILE" \
  SNAP_OUT="$OUT" \
  SNAP_MIN_CPUS="$BENCH_MIN_CPUS" SNAP_MAX_LOAD="$BENCH_MAX_IDLE_LOAD" \
  SNAP_DURATION="$BENCH_DURATION" SNAP_CONNECTIONS="$BENCH_CONNECTIONS" \
  SNAP_WORKERS="$BENCH_WORKERS" SNAP_PAYLOAD_BYTES="$BENCH_PAYLOAD_BYTES" \
  SNAP_SITE_ROOT="$SITE_ROOT" SNAP_SITE_FS="$SITE_FS" SNAP_SITE_NOWAIT="$SITE_NOWAIT" \
  SNAP_CACHE_ROOT="$CACHE_ROOT" SNAP_CACHE_FS="$CACHE_FS" SNAP_CACHE_NOWAIT="$CACHE_NOWAIT"

python3 "$BENCH_DIR/snapshot-json.py"

echo "[bench/env] 快照已写入 $OUT"
if [ "$QUALIFIED" = true ]; then
  echo "[bench/env] 宿主判定：**合格**"
else
  echo "[bench/env] 宿主判定：**不合格** —— 本趟不会产出任何性能结论："
  printf '%s\n' "$DISQ" | sed 's/^/           · /'
fi
