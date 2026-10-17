#!/usr/bin/env bash
# CI 建置快取：把門禁用的兩個 docker 命名卷匯出成一個壓縮流 / 再灌回去。
#
#   bash tests/ci/cache.sh save    <缓存目录>
#   bash tests/ci/cache.sh restore <缓存目录>
#
# ★ ★ ★ **為什麼值得做，以及它的危險在哪**
#
#   實測（本機）：`fulcrum-cargo` 684MB、`fulcrum-target-*` **6.3GB**
#   （vendor 3.0G + debug 2.8G + release 580M）。冷快取下一次全量 CI 要 17–21 分鐘，
#   其中絕大部分是這兩坨東西從零編出來。
#
#   ⚠ ⚠ **而快取最壞的失效方式不是「沒命中」，是「命中了一份不該用的」** ——
#   本儲存庫為此栽過一次同族的：`fulcrum-build:local` 裡跑的一直是 bookworm，
#   而 Dockerfile 早就換成了 trixie，**此後每一次「全綠」用的基礎鏡像都不是儲存庫宣告的那個**。
#   ⇒ 這個指令碼的重點不是壓縮，是**證明灌回去的那份配得上現在這套工具鏈**：
#     · 卷名本身帶著 `Dockerfile.build` 的內容雜湊（docker-run.sh 定的規矩，這裡照抄同一套演算法）；
#     · 歸檔裡另存一份 `meta.txt`，**灌回之前逐字比對**，對不上就**跳過並說明**，絕不硬灌。
#
# ★ 不自己再造一套卷名演算法：從 `docker-run.sh` 那條規則抄同一份雜湊來源（同一個檔案、同一段雜湊）。
#   ⚠ 兩處各算各的話，它們會在某次改動後安靜地指向兩個不同的卷 —— 那時快取永遠不命中，
#     而現象只是「CI 怎麼還是這麼慢」。
set -euo pipefail

MODE=${1:-}
DIR=${2:-}
if [ -z "$MODE" ] || [ -z "$DIR" ]; then
  echo "用法：bash tests/ci/cache.sh {save|restore} <缓存目录>" >&2
  exit 2
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DOCKERFILE="$REPO/docker/Dockerfile.build"
[ -f "$DOCKERFILE" ] || { echo "找不到 $DOCKERFILE" >&2; exit 1; }

# shellcheck source=tests/lib/vol-lock.sh
. "$REPO/tests/lib/vol-lock.sh"

DOCKERFILE_SHA=$(sha256sum "$DOCKERFILE" | cut -d' ' -f1)
# ★ 卷名從共用的那一份推導來（`fulcrum_target_vol`），本檔案一個字元都不拼 ——
#   頂上那句「不自己再造一套卷名演算法」現在是結構上做不到，而不是靠人記得。
# ⚠ 名字裡除了 Dockerfile 雜湊還有**工作樹路徑**的短雜湊：CI 上一棵樹一個 checkout 路徑，
#   所以命中與否不受影響；本機上多棵工作樹才是它存在的理由。
TARGET_VOL="$(fulcrum_target_vol "$DOCKERFILE_SHA" "$REPO")"
CARGO_VOL=fulcrum-cargo

# ★ 搬運用的鏡像**不另外釘一個**：直接從 `docker/Dockerfile.systemd` 的 FROM 行讀回來。
#   那一行本來就是按 digest 釘死的，而且那個鏡像後面跑 M1 場景時無論如何都要拉。
#   ⇒ 同一個事實只有一個來源，也不多拉一個鏡像層。
HELPER=$(sed -n 's/^FROM \(debian:[^ ]*\).*/\1/p' "$REPO/docker/Dockerfile.systemd" | head -1)
[ -n "$HELPER" ] || { echo "从 Dockerfile.systemd 里读不出 FROM 行 —— 搬运镜像无从确定" >&2; exit 1; }

# ★ 副檔名有意**不寫死成 `.zst`**：壓縮器是按「這臺機器上有什麼」選的，
#   而一個叫 `.zst` 的 gzip 檔案是一句會誤導人的謊。
ARCHIVE="$DIR/fulcrum-build-cache.tar.z"
META="$DIR/meta.txt"

# 壓縮器：有 zstd 就用（多執行緒、快），沒有就退回 gzip，並**說出來**（不是靜默降級）。
if command -v zstd >/dev/null 2>&1; then
  CODEC=zstd
  COMPRESS=(zstd -3 -T0 -q)
else
  CODEC=gzip
  [ "$MODE" = "save" ] && echo "⚠ 没有 zstd，退回 gzip（慢，但结果一样）"
  COMPRESS=(gzip -1 -c)
fi

# ★ ★ **解壓按歸檔自己的魔數選，不按「這臺機器上裝了什麼」選。**
#   ⚠ 存的那臺有 zstd、取的那臺沒有（或反過來）是完全可能的，
#   而那時按環境猜會拿 `zstd -d` 去解一個 gzip 流 —— 回報錯誤跟真正的原因毫無關係。
#   ★ 判據掛在**產物**上，不掛在環境上 —— 與本儲存庫那幾道「自證」是同一條紀律。
sniff_codec() {
  local magic
  # ⚠ `tr -d '[:space:]'` 而不是 `tr -d ' \n'`：後者裡那個反斜槓在本儲存庫的
  #   工具鏈上被吃過好幾次（heredoc 會剝掉一層），而剝掉之後它刪的是字母 n ——
  #   十六進制裡沒有 n，所以**它照樣能跑**，只是理由變了。能不寫反斜槓就不寫。
  magic=$(head -c 4 "$1" | od -An -tx1 | tr -d '[:space:]')
  case "$magic" in
    28b52ffd*) printf 'zstd' ;;
    1f8b*) printf 'gzip' ;;
    *) printf 'unknown' ;;
  esac
}

case "$MODE" in
  save)
    mkdir -p "$DIR"
    # ── 卷在不在：⛔ 別把「docker 答不出話」也算成「卷不存在」───────────────
    #
    # ⚠ ⚠ ★ 原先這裡是 `docker volume inspect … >/dev/null 2>&1 || { 说「不存在」; exit 0; }`。
    #   實測（2026-09-08，把 docker 換成一個恆失敗的樁）：守護處理程序連不上時，
    #   這一格照樣打「卷不存在 —— 沒什麼可存的，跳過」並**退 0**
    #   ⇒ 一次壞掉的 CI 被記成一次「本來就沒東西可存」，而現象只是「怎麼老是冷快取」。
    # ★ 分辨的辦法**不是**去猜 stderr 的文字，而是**另問一個能分開兩者的問題**：
    #   守護處理程序答不答得出話。答得出 ⇒「沒有這個卷」這句話可信，跳過是對的；
    #   答不出 ⇒ 這一格什麼都沒證明，⛔ 不許靜默退 0。
    vol_rc=0
    vol_err=$(docker volume inspect "$TARGET_VOL" 2>&1 >/dev/null) || vol_rc=$?
    if [ "$vol_rc" -ne 0 ]; then
      echo "  \`docker volume inspect $TARGET_VOL\` 退了 $vol_rc，它自己的原话：" >&2
      printf '%s\n' "$vol_err" | sed 's/^/      /' >&2
      daemon_rc=0
      daemon_err=$(docker version --format '{{.Server.Version}}' 2>&1 >/dev/null) || daemon_rc=$?
      if [ "$daemon_rc" -ne 0 ]; then
        echo "CACHE SAVE FAILED: docker 守护进程也答不出话（\`docker version\` 退 $daemon_rc）" >&2
        echo "  ⇒ 上面那句「卷怎么样」什么都没证明。⛔ 「没能检查」不算「没什么可存的」。" >&2
        printf '%s\n' "$daemon_err" | sed 's/^/      /' >&2
        exit 1
      fi
      echo "★ 卷 $TARGET_VOL 不存在 —— 这一轮没什么可存的，跳过"
      echo "  （守护进程答得出话 ⇒ 上面那句「没有这个卷」是可信的）"
      exit 0
    fi
    # meta 先寫，再打包 —— 灌回時它是唯一的憑證。
    {
      echo "dockerfile-sha256=$DOCKERFILE_SHA"
      echo "target-volume=$TARGET_VOL"
      echo "codec=$CODEC"
      echo "saved-by=tests/ci/cache.sh"
    } > "$META"
    # ★ ★ ★ **半截歸檔必須自己清掉。** 實測（真發生了）：
    #   一次 `cancel-in-progress` 把門禁那一步掐了，匯出這一步隨之失敗，
    #   **但半截的歸檔檔案已經寫入磁碟**（370MB，完整的是 2.2GB），
    #   而 workflow 裡那個「檔案在不在」的判斷照樣成立 ⇒ 它把這份殘檔
    #   **存進了快取鍵**。快取條目不可變 ⇒ 此後每一輪都精確命中這個殘檔，
    #   而灌回時它會在解壓到一半時炸掉。
    #   ⚠ ⚠ 也就是說：**「檔案存在」不是「匯出成功」** —— 判據要取動作的結果，
    #   不是取動作留下的痕跡。（這與本儲存庫那條「一行說它發生了的紀錄不是證據」同形。）
    # ★ 寫成函式而不是內聯 trap 字串：內聯那種寫法裡 `rc=$?` 在單引號裡，
    #   那個掃查器看不見這次賦值（SC2154），而**為了讓它閉嘴去加 disable 註解
    #   ⚠ 上一行有意不以工具名開頭：`#` + 空白 + 那個名字會被當成 **directive** 解析，
    #     報 SC1072/SC1073。這條 `tests/vendor/run.sh` 裡白紙黑字記著，我還是踩了。
    #   是最不該走的一條路** —— 換個它讀得懂的寫法就行。
    drop_partial_archive() {
      local rc=$?
      if [ "$rc" -ne 0 ]; then
        # ★ ★ ★ **先記大小，再刪** —— 「它寫到多大時爆的」是查這件事唯一的硬數字，
        #   而刪掉之後就再也拿不到了。⚠ **這兩行的前後順序是取證的一部分，不是排版。**
        #   就是因為沒有這一行，一次真實的 No-space 失敗只留下了「失敗了」三個字。
        if [ -f "$ARCHIVE" ]; then
          echo "★ 半截归档：$(du -b "$ARCHIVE" 2>/dev/null | cut -f1) 字节（$(du -h "$ARCHIVE" 2>/dev/null | cut -f1)）" >&2
        fi
        echo "★ 导出失败（退出码 $rc），清掉半截归档，绝不让它去认领缓存键" >&2
        rm -f "$ARCHIVE" "$META"
        # ★ ★ ★ **在這一刻量磁碟，而不是等回到 workflow 再量。**
        #   ⚠ ⚠ 的教訓：workflow 裡那次「匯出後的磁碟」量到 `1.8G 可用 98%`，
        #   而同一輪換個時機量是 `32G 可用 56%` —— **`rm -f` 之後空間不一定立即釋放**
        #   （寫它的那個處理程序可能還持著 fd）。⇒ 那個「匯出吃掉 28GB」的結論是一個
        #   **測量假象**，而它把整件事的方向帶偏了好幾輪。
        #   ★ 下面這幾行在**失敗的那一刻**量（`rm -f` 之前就已經取過歸檔大小了），
        #   問的才是「爆的時候到底什麼樣」。⚠ 它們只花毫秒。
        echo "★ 爆的那一刻 —— 归档所在的文件系统（★ 此前的盲区：以前只测过 /）：" >&2
        df -h "$DIR" >&2 2>&1 || true
        echo "★ 爆的那一刻 —— 根文件系统：" >&2
        df -h / >&2 2>&1 || true
        echo "★ 爆的那一刻 —— 内存与 swap（-T0 的 zstd 会吃内存，而 swap 在磁盘上）：" >&2
        # ⚠ Git Bash 上沒有 `free`（那是 Linux 命令）。不測存在性的話，
        #   開發機上會冒一行 `command not found` —— 那看起來像指令碼壞了。
        if command -v free >/dev/null 2>&1; then
          free -h >&2 2>&1 || true
        else
          echo "  （本机没有 free，这一项只在 Linux 上量得到）" >&2
        fi
        echo "★ 已知事实（CI 实测，不必每轮再量）：" >&2
        echo "  tar 流 9087109120 字节 ≈ 9.09GB；两个卷 352M + 8.2G —— 与 docker system df 对得上。" >&2
        echo "  ⇒ 卷与 tar 流都不是异常。异常在「31GB 可用，而只写了 1.8GB 就 No space」这一句上。" >&2
      fi
    }
    trap drop_partial_archive EXIT
    # ★ ★ **取證只留零代價的那幾行。** 兩個卷多大、tar 流多大，都已經量過且對得上
    #   （約 9GB，與 `docker system df` 一致）⇒ **卷與 tar 流都沒有異常**。
    #   ⚠ ⚠ 而重新量一遍的代價是量出來的：兩次各讀 9GB，把匯出這一步從
    #   **5m30s 拉到 24m54s**，整輪 CI 從 ~25 分鐘漲到 **40m2s**。
    #   > **一個已經回答完的問題，不該每輪再問一遍。**
    #   ⚠ 而「只在失敗路徑上才跑」要真的做到 —— `du` 那種行寫在外面就會每輪都跑。
    #
    # ★ ★ ★ **真正的缺口反倒被那一版量出來了，而它幾乎不花錢**：
    #   `df -h /` 報**還有 31GB 可用**，而 zstd **只寫了 1.8GB 就報 No space left**。
    #   ⇒ 說明 `df /` 與歸檔**實際寫入的那個檔案系統可能不是一個**
    #   （`$ARCHIVE` 在 `$DIR`，而此前只測過 `/`），也可能是 `-T0` 的 zstd 吃記憶體
    #   把系統推進 swap、或者某個 cgroup 限額。**沒有證據之前不寫結論。**
    echo "[cache] 归档要写到哪个文件系统上（★ 此前的盲区）："
    df -h "$DIR" 2>&1 || true
    echo "[cache] 导出 $CARGO_VOL + $TARGET_VOL → $ARCHIVE"
    # ★ tar 在容器裡跑（卷只有容器夠得著），壓縮在**宿主機**上跑（runner 的 zstd 是多執行緒的）。
    #   ⚠ 一趟流式，不落 7GB 的中間檔案 —— runner 的磁碟裝不下那個中間檔案。
    #
    # ★ ★ ★ **`--log-driver=none` 不是優化，它是這一整件事的根因。**
    #   查清：docker 預設的 **json-file 紀錄驅動**會把容器的 stdout
    #   **轉義後寫進 `/var/lib/docker/containers/<id>/<id>-json.log`** ——
    #   而這裡的 stdout 恰好是**整個 9GB 的 tar 流**。
    #   ★ 本機實測（不帶 `--rm` 跑一次再量）：7.83GB 的 tar 流 ⇒ 紀錄 **34,016,172,298 位元組（31.7GB）**，
    #   膨脹約 **4 倍**。CI 上 tar 流 9.09GB ⇒ 紀錄 ≈ 26–36GB —— **正好就是那 28GB**。
    #   ⚠ ⚠ **`--rm` 救不了**：它只在容器**退出之後**刪紀錄，
    #   而磁碟在它**還在跑**的時候就滿了。
    #   ★ 這也解釋了為什麼「半截歸檔只有 1.8GB」與「磁碟漲了 28GB」同時成立 ——
    #   那 28GB 裡只有 1.8GB 是歸檔，剩下的是一份沒人要看的紀錄。
    # ⚠ ⚠ `MSYS_NO_PATHCONV=1` 在這一行上**不是可選的**：沒有它時，Git Bash 會把
    #   `-C /` 裡那個 `/` 改寫成 `C:/Program Files/Git/` —— tar 當場報
    #   `Cannot open: No such file or directory`。★ CI 是 Linux，那裡這個變數無害。
    #   ★ ★ 才發現：**在此之前 `save` 在開發機上根本跑不起來**，
    #   於是改動它只能靠一輪 CI（~20 分鐘）去驗 ——
    #   **而一個只能在 CI 上驗的指令碼，改它的人會候得不耐煩。**
    MSYS_NO_PATHCONV=1 docker run --rm --log-driver=none \
      -v "$CARGO_VOL:/c:ro" -v "$TARGET_VOL:/t:ro" "$HELPER" \
      tar -cf - -C / c t | "${COMPRESS[@]}" > "$ARCHIVE"
    echo "[cache] 归档大小：$(du -h "$ARCHIVE" | cut -f1)"
    ;;

  restore)
    if [ ! -f "$ARCHIVE" ]; then
      echo "[cache] 没有归档（$ARCHIVE）—— 冷启动，正常"
      exit 0
    fi
    # ★ ★ **灌回之前先驗憑證。** 對不上就跳過，而且**說清楚為什麼** ——
    #   靜默硬灌正是「悄悄喂回陳舊產物」那條路。
    if [ ! -f "$META" ]; then
      echo "★ 有归档但没有 meta.txt —— 来路不明，**不灌**（这一轮按冷启动跑）"
      exit 0
    fi
    WANT="dockerfile-sha256=$DOCKERFILE_SHA"
    if ! grep -qxF "$WANT" "$META"; then
      echo "★ 归档不是这套工具链下产出的，**不灌**（这一轮按冷启动跑）："
      echo "    归档里：$(grep '^dockerfile-sha256=' "$META" || echo '（没有这一行）')"
      echo "    现在是：$WANT"
      exit 0
    fi
    # ★ ★ **第二個座標也要對上。** `save` 早就把 `target-volume=` 寫進 meta 了，
    #   而它把「哪個建置鏡像 × 哪一棵工作樹」兩個座標都編在裡面 —— 灌回卻只認前一半，
    #   於是**另一棵樹產出的歸檔照樣灌得進這棵樹的卷**。
    #   ⚠ 別覺得「反正容器裡都是 /w，內容通用」就可以硬灌：路徑通用，**mtime 不通用**。
    #   另一棵樹編出來的產物比這棵樹的原始碼新時，cargo 會直接當成最新的重複使用 ——
    #   而這正是 tests/lib/vol-lock.sh 頂部記著的那兩次實測（一次假紅、一次假綠）。
    #   ★ 它落在**最看不見**的地方：CI 上沒人盯著卷名，只會看到一輪很快的綠。
    WANT_VOL="target-volume=$TARGET_VOL"
    if ! grep -qxF "$WANT_VOL" "$META"; then
      echo "★ 归档不是这棵工作树产出的，**不灌**（这一轮按冷启动跑）："
      echo "    归档里：$(grep '^target-volume=' "$META" || echo '（没有这一行）')"
      echo "    现在是：$WANT_VOL"
      echo "    工作树：$REPO"
      exit 0
    fi
    FOUND=$(sniff_codec "$ARCHIVE")
    case "$FOUND" in
      zstd)
        command -v zstd >/dev/null 2>&1 || {
          echo "★ 归档是 zstd 压的，而这台机器上没有 zstd —— **不灌**（按冷启动跑）"
          exit 0
        }
        DECOMPRESS=(zstd -d -q -c)
        ;;
      gzip) DECOMPRESS=(gzip -d -c) ;;
      *)
        echo "★ 归档头四个字节既不是 zstd 也不是 gzip —— 来路不明，**不灌**（按冷启动跑）"
        exit 0
        ;;
    esac
    # ★ ★ ★ **碰卷之前先把歸檔從頭到尾讀一遍。**
    #   ⚠ 少了這一步，一份被截斷的歸檔會**先往真卷裡解出一半**再回報錯誤 ——
    #   而那一半是殘缺的 .rlib，比「沒有快取」壞得多。
    #   真的出現過一份 370MB 的殘檔（完整的是 2.2GB）。
    #   ★ 兩層都驗：解壓流驗到底（`-t`），tar 也整個列一遍（`-tf`）——
    #     壓縮流完整不代表裡面那個 tar 完整。代價是多讀一遍，換的是「絕不半灌」。
    echo "[cache] 灌回前先验一遍归档完整性（两层：压缩流 + tar）…"
    if ! "${DECOMPRESS[@]}" "$ARCHIVE" | tar -tf - > /dev/null 2>&1; then
      echo "★ 归档读不完整（截断／损坏）—— **不灌**，这一轮按冷启动跑。"
      echo "  ⚠ 一份残档比没有缓存坏得多：它会往卷里解出一半残缺的产物。"
      exit 0
    fi
    echo "[cache] 归档完整。灌回 $CARGO_VOL + $TARGET_VOL（$FOUND 压的）"
    docker volume create "$CARGO_VOL" >/dev/null
    # ★ 走 fulcrum_target_vol_create：新建時要把「屬於哪棵樹」寫進 label。
    #   ⚠ 這裡是 CI 上**第一個**建出這個卷的人（`docker-run.sh` 隨後拿到的是已存在的卷，
    #     而對已存在的卷 `docker volume create` 是靜默 no-op）⇒ label 寫不寫得上，取決於這一行。
    fulcrum_target_vol_create "$TARGET_VOL" "$REPO"
    "${DECOMPRESS[@]}" "$ARCHIVE" \
      | MSYS_NO_PATHCONV=1 docker run --rm -i --log-driver=none \
          -v "$CARGO_VOL:/c" -v "$TARGET_VOL:/t" "$HELPER" \
          tar -xf - -C /
    # ★ 灌完當場自證：卷裡真的有東西了。
    #   ⚠ 少了這一條，一次「解包失敗但結束代碼是 0」的灌回會讓人以為快取生效了，
    #     而實際上後面照樣從零編 —— 現象只是「怎麼還是這麼慢」，沒有任何一行會說出來。
    N=$(docker run --rm -v "$TARGET_VOL:/t:ro" "$HELPER" \
          sh -c 'find /t -maxdepth 1 -mindepth 1 | wc -l')
    if [ "${N:-0}" -gt 0 ]; then
      echo "[cache] 灌回后 $TARGET_VOL 顶层有 $N 项"
    else
      echo "★ 灌回之后卷还是空的 —— 缓存这一轮等于没有（不判红，但别以为它生效了）" >&2
    fi
    ;;

  *)
    echo "未知模式 $MODE（只认 save / restore）" >&2
    exit 2
    ;;
esac
