#!/usr/bin/env bash
# M1 的 systemd 場景驅動：把產物放進一個**以 systemd 為 PID 1** 的容器裡跑。
#
#   bash tests/m1/systemd-run.sh                   # 建置 + 四個場景
#   M1_SKIP_BUILD=1  bash tests/m1/systemd-run.sh  # 跳過建置（由 docker-run.sh 呼叫時用）
#   M1_ONLY=product  bash tests/m1/systemd-run.sh  # 只跑**產品二進位**場景
#   M1_ONLY=main     bash tests/m1/systemd-run.sh  # 只跑 spike 主場景（機制那一層）
#   M1_ONLY=rev      bash tests/m1/systemd-run.sh  # 只跑 ExitType=main 反證場景
#   M1_ONLY=handover bash tests/m1/systemd-run.sh  # 只跑「被否掉的 MainPID 交接」場景
#   M1_KEEP=1        bash tests/m1/systemd-run.sh  # 失敗後**不刪容器**，留著進去看現場
#
# 四個場景的分工：
#   product.sh          ★ **產品二進位**在 systemd 下起得來、換得了代、停得乾淨（G78）
#   run.sh              spike：機制那一層（fd 移交、CLOEXEC、升級 socket 不洩漏）
#   exit-type-main.sh   去掉 ExitType=cgroup 會怎樣 —— 證明那一行確實在幹活
#   mainpid-handover.sh G31 推斷的那條路會怎樣 —— 證明它為什麼被否掉
#
# ★ ★ ★ **為什麼產品那一個是才有的**：在它之前，這裡三個場景跑的全是
#   spike 二進位，而 spike 把 sd_notify / pid 檔案 / SIGUSR2 換代**自己實作了一遍**。
#   於是三個場景全綠，產品二進位卻 `systemctl start` 超時失敗（實測）。
#   > **夾具餵給門的那個二進位，本身也是夾具的一部分。**
#
# ★ 為什麼必須是另一個容器：M0 那套跑在建置鏡像裡（rust 工具鏈，PID 1 是 bash）。
#   本場景要驗的東西**只存在於 systemd 裡**——MainPID、cgroup 生命週期、KillMode。
#   同一個容器同時做這兩件事，會讓每次改測試宿主都觸發一次全量重編譯。
set -euo pipefail

REPO_UNIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# 卷名的推導與門禁互斥鎖：與 tests/m0/docker-run.sh 共用同一份，不在這裡重算。
VOL_LOCK_LIB="$REPO_UNIX/tests/lib/vol-lock.sh"
# shellcheck source=tests/lib/vol-lock.sh
. "$VOL_LOCK_LIB"

# ── 建置 ────────────────────────────────────────────────────────────────────
# ★ 不自己寫一份建置邏輯：行尾檢查、建置鏡像的內容雜湊重建、快取卷，全都在 docker-run.sh 裡，
#   抄一份過來必然分頭長歪（本儲存庫已經在 lifecycle.sh 上吃過一次）。
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

# ── 產物在哪個 target 卷裡 ───────────────────────────────────────────────────
#
# ★ ★ **不自己重算一遍卷名。** docker-run.sh 用 `docker/Dockerfile.build` 的內容雜湊
#   做卷名後綴，這裡改成**從建置鏡像的 label 上讀回來**——同一個事實只有一個來源，
#   於是不可能算出兩個不同的值。
#
# ★ 並且當場核對它與**當前** Dockerfile 是否一致：不一致說明建置沒跑或跑的是舊鏡像，
#   此時若照舊掛上去，跑的就是**上一個工具鏈編出來的二進位**，而且會報綠。
#   ——「拿不到就強制重建」在 docker-run.sh 裡；這裡拿不到就**判紅**，因為這裡不負責建置。
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
# ★ ★ 卷名還帶著**這棵工作樹**的短雜湊（理由寫在 tests/lib/vol-lock.sh 頂部：
#   一臺機器上有好幾棵工作樹，而它們曾經共用同一個 /w/target）。
#   ⚠ 這裡同樣**不自己拼字串** —— 拼一遍就是給下一次改名埋一處安靜的分叉，
#     而分叉的表現是本格掛上一個自動新建的**空卷**，然後抱怨找不到產品二進位。
TARGET_VOL="$(fulcrum_target_vol "$IMAGE_SHA" "$REPO_UNIX")"

# ★ ★ ★ **卷必須已經在了，不在就當場紅。** 上面那段註解說「分叉的表現是本格掛上一個
#   自動新建的空卷」——它防住了「自己拼字串」那條成因，卻沒防住**卷壓根不存在**：
#   `docker run -v <名字>:<路径>` 對不存在的命名卷是**靜默新建一個空卷**，不回報錯誤。
#   ⇒ 症狀是本格掛上空的 `/w/target`、抱怨找不到產品二進位，而真正的原因
#     （誰也沒建置過）要繞一大圈才看得出來；更壞的是那個空卷**沒有 label**，
#     於是它此後在回收提示裡永遠落進「不明」那一半，只進不出。
# ⛔ **不改成「不在就自己建」**：空卷意味著沒有產品二進位，建了也只是把失敗推到更晚，
#   而那時錯誤資訊指向的是別的東西。⇒ 與 `tests/bench/run.sh` 同形：說清楚，然後退出。
# ★ 這裡不負責建置（同上面 IMAGE_SHA 那兩條的理由），所以給的是「先跑哪一格」而不是代跑。
if ! docker volume inspect "$TARGET_VOL" >/dev/null 2>&1; then
  echo "M1 GATE FAILED: 找不到目标卷 $TARGET_VOL —— 先跑一次构建那一格" >&2
  echo "  \`bash tests/m0/docker-run.sh\`（或去掉 M1_SKIP_BUILD 让本脚本自己构建）。" >&2
  exit 1
fi

# ★ 與 M0 同一把鎖（鎖名＝卷名）：本格與 M0 掛的是同一個卷，並行跑一樣會互相踩。
# ⚠ 由 docker-run.sh 調進來時它已經持鎖，靠匯出的 FULCRUM_GATE_LOCK_HELD 直接放行；
#   上面那次 `BUILD_ONLY=1 docker-run.sh` 也是它自己取自己放，所以取鎖必須排在建置之後。
fulcrum_lock_acquire "$TARGET_VOL" "$REPO_UNIX" || exit 1

# ── 測試宿主鏡像（systemd）─────────────────────────────────────────────────
#
# 與建置鏡像同一條紀律：**內容變了就重做**，而不是「在就用」。
# 理由見 docker/Dockerfile.systemd 頂部——systemd 的版本是本 spike 結論的一部分。
SYSTEMD_IMAGE=${SYSTEMD_IMAGE:-fulcrum-systemd:local}
SYSTEMD_DOCKERFILE="${REPO_UNIX}/docker/Dockerfile.systemd"
if command -v sha256sum >/dev/null 2>&1; then
  SYSTEMD_SHA=$(sha256sum "$SYSTEMD_DOCKERFILE" | cut -d' ' -f1)
elif command -v git >/dev/null 2>&1; then
  # ⚠ ⚠ ★ `--stdin`：本指令碼由 `tests/m0/docker-run.sh` 在它 `export MSYS_NO_PATHCONV=1`
  #   **之後**調起 ⇒ 繼承那個變數 ⇒ 原生 git.exe 拿到未轉換的 `/d/...` 會報
  #   `fatal: could not open '/d/…' for reading`。argv 裡不放路徑就沒這回事。
  #   （`docker-run.sh` 第 42 行那條位置硬要求說的是同一件事；同源寫法見 `tests/lib/vol-lock.sh:168`。）
  SYSTEMD_SHA="githash-$(git hash-object --stdin < "$SYSTEMD_DOCKERFILE")"
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

# ── 跑一個場景 ──────────────────────────────────────────────────────────────
#
# ★ **每個場景一個全新容器。** 比「一個容器跑兩遍再清理」強得多：M0 那邊正是因為
#   上一個場景留下了處理程序，下一個場景的基線探測對著殘留處理程序報了綠（實際發生過）。
#   systemd 的狀態（failed 計數、drop-in、journal）更難清乾淨，所以這裡直接不復用。
run_scenario() {
  local name=$1 script=$2 rc=0
  # ★ 必須另起一句：`local` 的**所有參數在賦值發生之前就已經展開完了**，
  #   寫成 `local name=$1 cname="…${name}"` 時 `${name}` 取的是賦值前的值（這裡是未定義，
  #   `set -u` 當場炸）。這條第一次跑就咬了一口。
  local cname="fulcrum-m1-${name}"
  echo
  echo "════════ 场景 ${name}：${script} ════════"
  docker rm -f "$cname" >/dev/null 2>&1 || true

  # --privileged + 私有 cgroup namespace 是 systemd 在容器裡跑起來的最低要求。
  # ★ ★ **千萬不要再把宿主機的 /sys/fs/cgroup 掛進來。** 實測：那樣做會讓
  #   容器內的 cgroup 視圖與它自己的 namespace 對不上，journald 直接起不來
  #   （"Failed to acquire cgroup root path"），而**更糟的是 MAINPID= 交接會被靜默丟棄**
  #   ——systemd 無法把發信處理程序解析到某個 unit 上。⚠ 它很容易被誤讀成「systemd 不接受交接」
  #   這個完全錯誤的結論。privileged 下 docker 自己會把正確的 cgroup2 視圖掛成 rw。
  docker run -d --name "$cname" \
    --privileged --cgroupns=private \
    --tmpfs /run --tmpfs /run/lock \
    -v "${REPO_HOST}:/w" \
    -v "${TARGET_VOL}:/w/target" \
    "$SYSTEMD_IMAGE" >/dev/null

  # 等 systemd 起來。★ 不用 sleep：等它自己說話。
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
    # ★ degraded 要把是誰掛了打出來。容器裡有幾個 unit 起不來是正常的，
    #   但「正常的 degraded」和「真的有東西壞了」必須能分開看。
    echo "  systemd 状态 degraded，失败的 unit："
    docker exec "$cname" systemctl list-units --state=failed --no-pager --no-legend | sed 's/^/      /'
  fi
  docker exec "$cname" cat /etc/fulcrum-systemd-image | sed 's/^/  /'

  # ★ 裝**全部** unit 檔案，而不是點名一個：場景與 unit 是一對一的
  #   （spike 三個用 fulcrum-m1.service，產品那個用 fulcrum-prod.service），
  #   點名的寫法會在加第四個場景時安靜地少裝一個 —— 而症狀是「unit 沒被載入」，
  #   看起來像 systemd 的問題。⚠ 裝了不等於啟動：沒有 `systemctl enable`，
  #   `[Install]` 段不生效，每個場景只 start 自己那一個。
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
    # ★ 四個場景全都跑完再彙總，**即使前面已經紅了**：
    #   產品場景是「產品真的能被託管嗎」，spike 主場景是「機制成立嗎」，
    #   另兩個是「不這麼做會怎樣」。只看一個紅，分不清是機制沒生效、
    #   是產品沒接上、還是判據本身分不清好壞。
    # ★ 產品排第一：它壞了最要緊，也最可能是剛改出來的。
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
