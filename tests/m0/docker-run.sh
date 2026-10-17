#!/usr/bin/env bash
# 在容器裡建置並跑驗證（G26）。宿主機什麼都不用裝，只要有 Docker。
#
#   bash tests/m0/docker-run.sh    # 全量：建置 + lint + 自研 crate 測試 + fork 迴歸網 + 全部端到端場景
#
# 一次只開一個 `<X>_ONLY=1` 只跑那一格；`<X>_TESTS=0` 跳過那一格：
#
#   BUILD · LINT · UNIT · VENDOR             建置 · fmt+clippy+shellcheck · 自研 crate 測試 · fork 迴歸網
#   COMPILE                                  shellcheck + 編譯（含全部測試目標）、一條測試都不跑 —— pre-push 門用的那一格（G158：shellcheck 按被推的提交決定跑不跑）
#   SERVE · L4 · FILES · CACHE · CACHEDISK   資料面 · L4 面 · 靜態檔案 · 快取 · 快取磁碟後端
#   ENCODE · H3 · PP · LOG                   壓縮 · HTTP/3 · PROXY protocol（HTTP 面）· 訪問紀錄
#   METRICS · RELAY                          Prometheus 指標 · QUIC 跨處理程序轉交
#   ACME · RENEW · SMOKE · STRESS · MUSL     簽發 · 續期 · 冒煙 · 壓力 · musl 靜態產物
#   UNCLAIMED                                未被認領的繼承 fd
#   BENCH                                    對拍流水線與判據（M3 第一刀，⛔ 不出效能數字）
#
#   LINT=0 跳過 lint；M1_TESTS=0 跳過 M1 的 systemd 場景；BENCH_TESTS=0 跳過對拍那一格。
#
#   HOST_SELFTESTS=auto 宿主機側那串自測只在門的指令碼或宿主工具變了時才跑（G159）；
#   VOL_REPORT=0 跳過遺留卷報告；COMPILE_SHELLCHECK=0 讓 COMPILE 那一格不跑 shellcheck。
#     ★ 這三個是 pre-push 門用的（G158 / G159），由 tests/ci/pre-push.sh 顯式賦值；
#       ⛔ 完整門禁不該帶著它們跑 —— 帶了就不是完整門禁。
#     ⛔ 宿主機自測**沒有「無條件跳過」這一檔**：除 `auto` 以外的任何值都照跑。
#
# ★ M1 的 systemd 場景跑在**另一個容器**裡（systemd 當 PID 1），由本指令碼在最後呼叫
#   tests/m1/systemd-run.sh 驅動；單獨跑用 `bash tests/m1/systemd-run.sh`。
# ★ 對拍那一格跑在**第三張鏡像**裡（三家競品同處一室），同樣由本指令碼在最後呼叫
#   tests/bench/run.sh 驅動；單獨跑用 `bash tests/bench/run.sh`。
#
#   DOCKER_USER="$(id -u):$(id -g)"  bash tests/m0/docker-run.sh
#     ★ Linux 宿主機／CI 上用，免得容器以 root 在掛進去的工作樹裡留下 root 屬主的產物。
#       Windows 上**預設不開** —— 傳宿主機 uid 會讓命名卷（root 屬主）不可寫。
#       ⚠ 這條沒有在 Linux 上實測過：它是留好的口子，不是已驗證的路徑。
#
# 建置快取放在 docker 命名卷裡，既不汙染宿主機，也避開 Windows 檔案系統的慢 I/O。
set -euo pipefail

IMAGE=${IMAGE:-fulcrum-build:local}
REPO_UNIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# target 卷叫什麼、以及「同一棵樹同一時刻只許跑一次門禁」那把鎖，都在這裡。
# ★ 三處（本檔案 · tests/m1/systemd-run.sh · tests/ci/cache.sh）共用同一份推導，
#   不各寫一遍 —— 各寫一遍的失效形態是安靜地指向兩個不同的卷。
VOL_LOCK_LIB="$REPO_UNIX/tests/lib/vol-lock.sh"
# shellcheck source=tests/lib/vol-lock.sh
. "$VOL_LOCK_LIB"

# ── 宿主機自測：這一趟跑不跑（G158 → G159）────────────────────────────────────
#
# 下面那串自測喂的都是**合成輸入**，結論只取決於兩樣東西：門自己的指令碼，與宿主機上的
# shell 工具。⇒ 兩樣都沒變時，上一次通過的結論今天仍然成立。
# ★ G158 讓 pre-push 無條件跳過它們，代價是「門的機器壞了要等下一趟完整門禁」——
#   而**改壞它的那一刻**（改 `vol-lock.sh`、換 Git for Windows）恰恰最該當場證。
#   ⇒ G159 改成：`HOST_SELFTESTS=auto` 時，本工作樹有一份**同一指紋**的通過記錄才跳過。
#
# ★ 指紋 = 本指令碼 + `tests/lib/` 下全部 `.sh` 的內容 + 宿主工具身分
#   （`BASH_VERSION` · `uname -sr`（MSYS 執行環境）· `git --version` —— Git for Windows
#   的版本號，自測用到的 tr / sha256sum / cygpath / mkdir 都隨它一起散布）。
#   ★ 用 `tests/lib/` 整個目錄而不是點名 `vol-lock.sh`：往後多一個 lib 時不會安靜地漏掉；
#     多算進來的檔案只會讓自測多跑幾次，方向是安全的。
# ★ 記錄放在 `git rev-parse --git-path` 下：只屬於本機、本工作樹，不進儲存庫，
#   也不進工作樹（行尾檢查與 target 掃描都看不見它）。
# ★ ★ 記錄**只由一次真跑通過的自測寫下**（與 G144 的驗證記錄同一條：生成器重現不出來的
#   記錄與編造的無法區分）；**要跑就先撤掉舊記錄**，任何一條紅 ⇒ 下一趟沒有記錄可用。
# ⚠ 剩下的盲區寫在明處：① 指紋沒覆蓋到的環境變化（同一 Git for Windows 版本下單獨換了
#   某個工具）；② 判「對不對得上」的這兩個函式自己壞成「恆答對得上」時，pre-push 會一直
#   跳過 —— 那一種只有完整門禁看得見（預設模式不問記錄、每趟照跑，並跑下面那條自測）。
host_selftest_fp() {   # host_selftest_fp <文件>...：算不出（讀不到檔案、沒有 sha256sum）就回傳非 0
  local f
  for f in "$@"; do [ -r "$f" ] || return 1; done
  {
    printf 'bash=%s\nuname=%s\ngit=%s\n' "$BASH_VERSION" "$(uname -sr 2>&1)" "$(git --version 2>&1)"
    for f in "$@"; do
      printf '%s  %s\n' "$(sha256sum < "$f" | cut -d' ' -f1)" "${f#"$REPO_UNIX"/}"
    done
  } | sha256sum | cut -d' ' -f1
}
host_selftest_stamp_ok() {   # <记录文件> <指纹>：記錄裡的指紋與之逐字相同才答 0
  [ -n "$2" ] && [ -f "$1" ] && [ "$(sed -n 's/^fp=//p' "$1")" = "$2" ]
}
host_selftest_stamp_write() {   # <记录文件> <指纹>：先寫臨時檔案再 mv，讀的一方看不到半份
  local tmp="$1.tmp.$$"
  printf 'fp=%s\nat=%s\n' "$2" "$(date '+%Y-%m-%d %H:%M:%S')" > "$tmp" && mv -f "$tmp" "$1"
}

# ⚠ 算不出指紋 / 拿不到記錄路徑 ⇒ 照跑，也不寫記錄 —— 「沒能判斷」不算「不必跑」。
# ⚠ 兩個路徑都寫成 `"$REPO_UNIX"/…`（引號收在變數後面）**不是風格**：`msys-argv-guard.sh`
#   把本檔案裡每一處「變數名緊跟斜槓再到 .sh」都推導成「本門會調起的指令碼」並整份掃它的 git argv；
#   這裡只是拿檔案算雜湊、不調起它，寫成那種形狀會讓本檔案把自己算進去（2026-10-06 實測誤報 3 處）。
HOST_SELFTEST_FP=$(host_selftest_fp "$REPO_UNIX"/tests/m0/docker-run.sh "$REPO_UNIX"/tests/lib/*.sh) \
  || HOST_SELFTEST_FP=""
HOST_SELFTEST_STAMP=$(git -C "$REPO_UNIX" rev-parse --path-format=absolute \
                        --git-path fulcrum-host-selftest.ok 2>/dev/null) || HOST_SELFTEST_STAMP=""
RUN_HOST_SELFTESTS=1
if [ "${HOST_SELFTESTS:-1}" = "auto" ]; then
  if [ -n "$HOST_SELFTEST_STAMP" ] && host_selftest_stamp_ok "$HOST_SELFTEST_STAMP" "$HOST_SELFTEST_FP"; then
    RUN_HOST_SELFTESTS=0
    echo "[docker-run] 宿主机自测：本工作树 $(sed -n 's/^at=//p' "$HOST_SELFTEST_STAMP") 对同一份输入通过过 ⇒ 本趟跳过（G159）"
  else
    echo "[docker-run] 宿主机自测：门的脚本或宿主工具变了（或本工作树还没有通过记录）⇒ 本趟照跑（G159）"
  fi
fi
if [ "$RUN_HOST_SELFTESTS" = 1 ] && [ -n "$HOST_SELFTEST_STAMP" ]; then
  rm -f "$HOST_SELFTEST_STAMP"
fi
host_selftest() { [ "$RUN_HOST_SELFTESTS" = "0" ] || "$@"; }

# ★ 上面那套判據自己的自測：兩個方向都有（該變的變、不該變的不變；該認的認、不該認的不認）。
#   ⚠ 它本身也經由 host_selftest 調 —— 改它就是改本指令碼，指紋隨之變，下一趟必跑；
#     而「判據壞成恆答對得上」那一種，由預設模式（完整門禁）每趟照跑它來兜。
selftest_host_selftest_stamp() {
  local d rc=0 a b c v
  d=$(mktemp -d)
  printf 'x\n' > "$d/one.sh"
  printf 'y\n' > "$d/two.sh"
  a=$(host_selftest_fp "$d/one.sh" "$d/two.sh") || a=""
  b=$(host_selftest_fp "$d/one.sh" "$d/two.sh") || b=""
  v=$(BASH_VERSION="selftest-other"; host_selftest_fp "$d/one.sh" "$d/two.sh") || v=""
  printf 'x \n' > "$d/one.sh"
  c=$(host_selftest_fp "$d/one.sh" "$d/two.sh") || c=""
  { [ -n "$a" ] && [ "$a" = "$b" ]; } || { echo "★ 同一份输入两次算出不同的指纹（或算不出来）" >&2; rc=1; }
  [ "$a" != "$c" ] || { echo "★ 输入文件改了一个字节，指纹没变 —— pre-push 会拿旧记录跳过自测" >&2; rc=1; }
  [ "$a" != "$v" ] || { echo "★ 宿主工具身份变了，指纹没变" >&2; rc=1; }
  ! host_selftest_fp "$d/one.sh" "$d/missing.sh" >/dev/null 2>&1 \
    || { echo "★ 输入文件读不到时仍给出了指纹" >&2; rc=1; }
  ! host_selftest_stamp_ok "$d/stamp" "$a" || { echo "★ 没有记录时答「对得上」" >&2; rc=1; }
  host_selftest_stamp_write "$d/stamp" "$a"
  host_selftest_stamp_ok "$d/stamp" "$a" || { echo "★ 刚写下的记录读回来对不上" >&2; rc=1; }
  ! host_selftest_stamp_ok "$d/stamp" "$c" || { echo "★ 指纹不同也答「对得上」" >&2; rc=1; }
  ! host_selftest_stamp_ok "$d/stamp" "" || { echo "★ 空指纹答「对得上」" >&2; rc=1; }
  rm -rf "$d"
  [ "$rc" -eq 0 ] || {
    echo "  宿主机自测记录的判据自测未通过 —— pre-push「跳过宿主机自测」一律不可信。" >&2
    exit 1
  }
}
host_selftest selftest_host_selftest_stamp

# ★ 行尾前置檢查 —— **必須放在 `export MSYS_NO_PATHCONV=1` 之前**。
#
#   `.gitattributes` 宣告「行尾無條件 LF」，但它只在 checkout / commit 時生效——
#   **容器 bind-mount 的是工作樹，不是索引**。宿主機上任何工具（Windows 版 Python 的
#   `write_text()` 預設就把 \n 翻成 \r\n）寫過的檔案都會在工作樹裡留下 CRLF，
#   而 `git status` 依然乾淨。.sh 一旦沾上 CRLF，容器裡的 bash 當場報 `\r: command not found`。
#
#   ★ ★ 位置是硬要求，不是風格：`MSYS_NO_PATHCONV=1` 一旦生效，原生 git.exe 收到的
#     就是未經轉換的 `/d/...`，`rev-parse` 會失敗，這道門會被**靜默跳過**。
#
# ★ ★ **拿不到 git 時不「跳過」，而是換一條不依賴 git 的路走完** ——
#   「沒能檢查」不算「檢查通過」，而這道門真實的失效模式恰恰就是 `git rev-parse` 失敗。

# 逐位元組數 CR。★ **不要用 grep**：MSYS 的 grep 在讀取階段就把行尾規整掉了，
#   CRLF 檔案裡 `grep -c $'\r'` 回傳 0，於是會得到一份**很像真結論的全 0 報告**。
#   `tr -dc | wc -c` 在宿主機與容器裡都是對的。
cr_count()  { tr -dc '\r'   < "$1" | wc -c; }
nul_count() { tr -dc '\000' < "$1" | wc -c; }   # 含 NUL 即二進位，同 grep -I 的判據

# ★ ★ **這兩個原語必須自證沒瞎，每次都證。** 它們的失效方式是安靜的：換個平台、
#   換個實作（比如有人「順手優化」成 grep），它們就恆回傳 0，這道門永遠綠。
#   代價是三個臨時檔案、兩次 `tr` —— 比一份假綠便宜太多。
selftest_byte_probes() {
  local d rc=0
  d=$(mktemp -d)
  printf 'a\r\nb\r\n' > "$d/crlf"      # 2 個 CR
  printf 'a\nb\n'     > "$d/lf"        # 0 個 CR
  printf 'x\000y'     > "$d/bin"       # 含 NUL
  [ "$(cr_count  "$d/crlf")" -eq 2 ] || { echo "★ cr_count 认不出 CRLF 文件里的 CR" >&2; rc=1; }
  [ "$(cr_count  "$d/lf")"   -eq 0 ] || { echo "★ cr_count 在纯 LF 文件上误报"      >&2; rc=1; }
  [ "$(nul_count "$d/bin")"  -gt 0 ] || { echo "★ nul_count 认不出二进制文件"        >&2; rc=1; }
  [ "$(nul_count "$d/lf")"   -eq 0 ] || { echo "★ nul_count 在文本文件上误报"        >&2; rc=1; }
  rm -rf "$d"
  [ "$rc" -eq 0 ] || {
    echo "  行尾检查的字节原语自测未通过——**本次 CRLF 检查的结论一律不可信**。" >&2
    echo "  多半是 tr 的实现变了，或有人把它换成了看不见 CR 的 grep。" >&2
    exit 1
  }
}
host_selftest selftest_byte_probes

# ── 「現在是不是某個 `*_ONLY` 模式」──────────────────────────────────────────
#
# ★ ★ **判據是結構性的**：不逐個列舉，而是問一句「有沒有任何一個 `*_ONLY` 被設成 1」。
#   ⇒ 下一個人加場景時不需要記得改這裡。⚠ 換成手動清單的話，漏掉一項就會讓
#   「只跑這一格」連帶跑 M1 那幾個 systemd 場景，多出幾十條 `  ✓` ——
#   **拿整份紀錄去數斷言的人會得到一個偏大而很像樣的數字**。
# ⚠ `M1_ONLY` 是 systemd 場景**自己的**選擇器（`M1_ONLY=main`），顯式排除。
only_mode_in() {
  awk -F= '$1 ~ /_ONLY$/ && $1 != "M1_ONLY" && $2 == "1" { found = 1 }
           END { exit(found ? 0 : 1) }'
}

# ★ ★ **它必須自證沒瞎，每次都證** —— 一個恆答「不是 only 模式」的實作不會讓任何東西紅。
selftest_only_mode() {
  local rc=0
  printf 'FOO=1\nBAR=2\n'     | only_mode_in && { echo "★ only_mode_in 在没有 *_ONLY 时误报" >&2; rc=1; }
  printf 'CACHE_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 CACHE_ONLY=1" >&2; rc=1; }
  printf 'CACHEDISK_ONLY=1\n' | only_mode_in || { echo "★ only_mode_in 认不出 CACHEDISK_ONLY=1" >&2; rc=1; }
  printf 'RELAY_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 RELAY_ONLY=1" >&2; rc=1; }
  printf 'CACHE_ONLY=0\n'     | only_mode_in && { echo "★ only_mode_in 把 =0 也当成了开" >&2; rc=1; }
  printf 'M1_ONLY=main\n'     | only_mode_in && { echo "★ M1_ONLY 不该让 M1 那四格被跳过" >&2; rc=1; }
  # ★ **新加一個 `*_ONLY` 就要在這裡加一條**，否則這個自測覆蓋的
  #   仍然是加它之前的那些 —— 與「不自省的名單」同一個形狀。
  printf 'MUSL_ONLY=1\n'      | only_mode_in || { echo "★ only_mode_in 认不出 MUSL_ONLY=1" >&2; rc=1; }
  printf 'METRICS_ONLY=1\n'   | only_mode_in || { echo "★ only_mode_in 认不出 METRICS_ONLY=1" >&2; rc=1; }
  printf 'STATS_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 STATS_ONLY=1" >&2; rc=1; }
  printf 'COMPILE_ONLY=1\n'   | only_mode_in || { echo "★ only_mode_in 认不出 COMPILE_ONLY=1" >&2; rc=1; }
  printf 'BENCH_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 BENCH_ONLY=1" >&2; rc=1; }
  [ "$rc" -eq 0 ] || {
    echo "  *_ONLY 判据自测未通过——**本次跑不跑 M1 场景的结论一律不可信**。" >&2
    exit 1
  }
}
host_selftest selftest_only_mode

# ── ★ ★ 卷名推導的自測 ──────────────────────────────────────────────────────
#
# 四條，**兩組各自兩個方向**：
#   ① 兩棵不同的工作樹 ⇒ 兩個卷名（這是本次修的那件事）；
#   ② 同一棵工作樹兩次 ⇒ 同一個卷名 —— 不穩的話每次都從零編，而且沒有一行字會說；
#   ③ 換建置鏡像仍然換卷 —— 這條性質本來就有（cargo 的 fingerprint 不覆蓋 C 工具鏈），
#      **不許在加樹標籤的時候把它丟掉**；
#   ④ 名字必須是合法的 docker 卷名。⚠ 這條不是潔癖：名字裡一旦混進 `/`，
#      `docker run -v "$名字:/w/target"` 會被當成**綁定掛載**而不是命名卷 —— 照跑、不回報錯誤。
selftest_target_vol() {
  local rc=0 a b c d
  a=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-a")
  b=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-b")
  c=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-a")
  d=$(fulcrum_target_vol "0000face0000ffff" "/tmp/fulcrum-selftest-tree-a")
  [ "$a" != "$b" ] || { echo "★ 两棵不同的工作树算出了同一个 target 卷名（$a）" >&2; rc=1; }
  [ "$a" = "$c" ]  || { echo "★ 同一棵工作树两次算出了不同的卷名（$a ≠ $c）"    >&2; rc=1; }
  [ "$a" != "$d" ] || { echo "★ 换了构建镜像却还是同一个卷名（$a）——「跟着镜像走」丢了" >&2; rc=1; }
  [[ "$a" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || { echo "★ 算出来的卷名不合法：$a" >&2; rc=1; }
  [ "$rc" -eq 0 ] || {
    echo "  卷名自测未通过——**「本次跑的是这棵树自己的产物」这个前提一律不可信**。" >&2
    exit 1
  }
}
host_selftest selftest_target_vol

# ── ★ ★ 「這個卷還有沒有主人」的自測 ────────────────────────────────────────
#
# 卷按樹分開之後多出來一筆磁碟賬：工作樹是會消失的（`.claude/worktrees/` 下那些用完就刪），
# 它們的卷不會。⇒ 新建時把樹路徑寫進 label，回收提示據此把**主人已經不在**的那些單列出來。
#
# 四條，一條一個方向，缺一條都會讓回收提示變成一句危險的話：
#   ① 樹還在 ⇒ `live`。錯成 `gone` 的話，提示會勸人刪掉**隔壁正在用的**那個卷 ——
#      而受害者那邊只看到一次莫名的全量重編，沒有一行字指向這裡。
#   ② 樹沒了 ⇒ `gone`。錯成 `live` 的話，這條回收路等於不存在，磁碟繼續無聲地漲。
#   ③ 沒有 label ⇒ `unknown`，**不是** `gone`。「沒能檢查」不算「檢查通過」：
#      加這條規則之前留下的卷、以及手動建的卷（supply-chain.md 裡那條 supply-audit
#      命令就手動掛著一個），都不能因為「讀不到主人」就被當成無主的。
#   ④ ★ ★ ★ **另一臺 shell 寫法的 label，判得動而且判得對。** label 是**建卷那臺
#      shell** 的寫法，而回收提示可能在另一臺 shell 裡跑：`/mnt/d/…` 在 Git Bash 裡
#      就是不存在，`D:/…` 在 WSL 裡也一樣。⇒ 判活前先經 `fulcrum_tree_local`
#      譯成本 shell 夠得到的寫法。這一條釘的是**互譯真的在工作**：
#      拿另一臺 shell 寫法的**不存在**的樹，必須答 `gone` —— 答 `unknown` 說明
#      互譯退化了（那時回收路在這一側等於不存在），答 `live` 則更糟。
#      ⚠ 與 ① 是同一個失效形態的兩個入口，而 ① 那條自測**抓不到它**：
#        它的探針是當前 shell 自己 `mktemp -d` 出來的，形式上永遠是本 shell 看得懂的。
#   ⑤ ★ **規範形往返**：真實存在的目錄經 `fulcrum_tree_norm` 之後仍判得出 `live`。
#      ⚠ 少了它，把 ④ 做成「一律 gone」也能過 —— 而那正好是①要防的那件事。
#   ⑥ ★ 夠不到的寫法 ⇒ `unknown`。Git Bash 裡 WSL 原生路徑（不在 `/mnt/<盘符>/` 下）
#      會被 cygpath 映到 MSYS 安裝根底下，那個路徑不存在 ⇒ 不攔就會答 `gone`。
selftest_tree_state() {
  local rc=0 probe canon
  probe=$(mktemp -d)
  [ "$(fulcrum_tree_state "$probe")"        = live ]    || { echo "★ 存在的工作树被判成了「不在」——回收提示会去劝人删隔壁正在用的卷" >&2; rc=1; }
  [ "$(fulcrum_tree_state "$probe/gone")"   = gone ]    || { echo "★ 已经不存在的工作树没被判出来——那条回收路等于不存在" >&2; rc=1; }
  [ "$(fulcrum_tree_state "")"              = unknown ] || { echo "★ 空 label（读不到主人）被判成了别的——不明与无主必须分开" >&2; rc=1; }
  if command -v cygpath >/dev/null 2>&1; then
    [ "$(fulcrum_tree_state /mnt/q/NoSuchTree)" = gone ] \
      || { echo "★ ★ Git Bash 判不动 WSL 写法的 label——回收路在这一侧等于不存在" >&2; rc=1; }
    [ "$(fulcrum_tree_state /nosuchroot/tree)" = unknown ] \
      || { echo "★ Git Bash 够不到的 POSIX 写法没答 unknown——被 cygpath 映到 MSYS 根底下会假装成「不在」" >&2; rc=1; }
  else
    [ "$(fulcrum_tree_state Q:/NoSuchTree)" = gone ] \
      || { echo "★ ★ WSL 判不动 Windows 写法的 label——回收路在这一侧等于不存在" >&2; rc=1; }
  fi
  canon=$(fulcrum_tree_norm "$probe")
  [ "$(fulcrum_tree_state "$canon")" = live ] \
    || { echo "★ ★ 规范形($canon)往返之后认不出这棵树还在——④ 那条会退化成「一律 gone」" >&2; rc=1; }
  rmdir "$probe"
  [ "$rc" -eq 0 ] || {
    echo "  卷归属判定自测未通过——**「哪些卷可以删」这个结论一律不可信**。" >&2
    exit 1
  }
}
host_selftest selftest_tree_state

# ── ★ ★ ★ 「同一棵樹只該有一個標籤」的自測 ──────────────────────────────────
#
# Docker Desktop 把 WSL 側的 bind 源暫存到 `<暂存根>/<发行版>/<sha256(源路径)>`，
# **且那個暫存掛載不隨容器退出釋放**（實測 `docker kill` 後仍在）。⇒ 一旦某一輪的
# `pwd` 落在暫存路徑上，標籤就換一個，而它的 sha256 又正好是下一層暫存目錄的名字 ——
# 每跑一輪生一個新卷（約 8.7GB），**快取從此永不命中**。
# ⚠ ⚠ 2026-09-05 這臺宿主上量到過一條四節的鏈：37 分鐘里長出 4 個卷、約 34.7GB，
#   而門每一輪都是綠的 —— 失效形態裡沒有任何一行錯誤，只是慢，且磁碟無聲地漲。
#   ⛔ 換臺機器讀到這裡別把這個數當成你面前這臺的讀數，自己數一遍。
#
# 四條：
#   ① 暫存路徑反查得回真實源（承重的那條）。
#   ② ★ 真實源與它的暫存路徑**必須算出同一個標籤** —— 鏈正是從這裡斷掉的。
#   ③ 反查不出來 ⇒ **原樣回傳，不猜**。猜錯就是把兩棵樹併成一個卷。
#   ④ 普通路徑不受影響。
#
# ⚠ 夾具用**虛構**的盤符/發行版/雜湊，斷言的是「暫存 → 真實源」這層**關係**：
#   抄本機真值的判據換臺機器就是一份自洽的假綠。夾具與現實是否同形，
#   靠的是它照著真實 `mountinfo` 的欄位順序寫（`$3` 設備號 `$4` 子路徑 `$5` 掛載點）。
selftest_tree_unstage() {
  local rc=0 fix saved h staged real
  fix=$(mktemp)
  h=1111111122222222333333334444444455555555666666667777777788888888
  staged="$FULCRUM_STAGING_ROOT/Distro-1.0/$h"
  real=/mnt/q/Projects/Widget
  {
    printf '101 100 0:99 / /mnt/q rw,noatime - 9p Q: rw\n'
    printf '202 100 0:99 /Projects/Widget %s rw,noatime shared:7 - 9p Q: rw\n' "$staged"
  } > "$fix"
  saved=$FULCRUM_MOUNTINFO
  FULCRUM_MOUNTINFO=$fix
  [ "$(fulcrum_tree_unstage "$staged")" = "$real" ] \
    || { echo "★ 暂存路径没被反查回真实源——标签会逐轮变，每轮多一个约 8.7GB 的卷" >&2; rc=1; }
  [ "$(fulcrum_tree_tag "$real")" = "$(fulcrum_tree_tag "$staged")" ] \
    || { echo "★ ★ 真实源与其暂存路径算出了两个标签——那条链还会继续长" >&2; rc=1; }
  [ "$(fulcrum_tree_unstage "$FULCRUM_STAGING_ROOT/Distro-1.0/nosuchmount")" = "$FULCRUM_STAGING_ROOT/Distro-1.0/nosuchmount" ] \
    || { echo "★ 反查不出来时没有原样返回——猜错就是把两棵树并进同一个卷" >&2; rc=1; }
  [ "$(fulcrum_tree_unstage /tmp/fulcrum-selftest-plain)" = /tmp/fulcrum-selftest-plain ] \
    || { echo "★ 普通路径被反查逻辑改写了" >&2; rc=1; }
  FULCRUM_MOUNTINFO=$saved
  rm -f "$fix"
  [ "$rc" -eq 0 ] || {
    echo "  树标签自测未通过——**这一轮的 target 卷可能是新生的空卷，缓存不命中**。" >&2
    exit 1
  }
}
host_selftest selftest_tree_unstage

# ── ★ ★ 「同一棵樹在兩個 shell 下只有一個標籤」的自測 ───────────────────────
#
# 同一棵樹在 WSL 與 Git Bash 裡是兩種寫法（`/mnt/d/W/F` 與 `/d/W/F`）⇒ 標籤若是
# 寫法的雜湊，一棵樹就拿兩個卷，滿載各 8–10GB。⛔ 它**不是**「門給出別人家讀數」
# 那一條（那條要求兩棵**不同**的樹共用一個卷）：這裡讀數是對的，只是雙份佔盤。
# ⇒ 規範形取 Windows 盤符式 `X:/…`（大寫盤符）。
#
# 三條：
#   ① 本 shell 的本機寫法歸到規範形。⚠ 兩個 shell 各斷言各的，**合起來**才是
#      「兩邊歸到同一個字串」—— 一個處理程序裡看不到另一邊，這是本條的固有侷限。
#   ② ★ ★ 反向映射譯得回本 shell 夠得到的寫法。**承重**：少了它，label 全變成
#      Windows 寫法之後 **WSL 那一側的回收路整條死掉**（判什麼都是 unknown），
#      而症狀只是「磁碟怎麼還在漲」，沒有一行字會說出原因。
#   ③ 盤符大小寫歸一 —— `/mnt/q` 與 `/mnt/Q` 是同一個盤，必須同一個標籤。
selftest_tree_canon() {
  local rc=0
  if command -v cygpath >/dev/null 2>&1; then
    [ "$(fulcrum_tree_norm /q/Projects/Widget)" = "Q:/Projects/Widget" ] \
      || { echo "★ Git Bash 的本地写法没归到规范形（实得 $(fulcrum_tree_norm /q/Projects/Widget)）" >&2; rc=1; }
    [ "$(fulcrum_tree_local Q:/Projects/Widget)" = "Q:/Projects/Widget" ] \
      || { echo "★ 规范形在 Git Bash 里没译成够得到的写法" >&2; rc=1; }
  else
    [ "$(fulcrum_tree_norm /mnt/q/Projects/Widget)" = "Q:/Projects/Widget" ] \
      || { echo "★ WSL 的本地写法没归到规范形（实得 $(fulcrum_tree_norm /mnt/q/Projects/Widget)）——同一棵树会拿两个卷" >&2; rc=1; }
    [ "$(fulcrum_tree_local Q:/Projects/Widget)" = "/mnt/q/Projects/Widget" ] \
      || { echo "★ ★ 规范形在 WSL 里译不回去——这一侧的回收路会整条死掉" >&2; rc=1; }
  fi
  [ "$(fulcrum_tree_tag /mnt/q/W)" = "$(fulcrum_tree_tag /mnt/Q/W)" ] \
    || { echo "★ 盘符大小写没归一——同一个盘上的同一棵树拿到两个标签" >&2; rc=1; }
  [ "$rc" -eq 0 ] || {
    echo "  规范形自测未通过——**同一棵树可能在两个 shell 下各占一个 target 卷**。" >&2
    exit 1
  }
}
host_selftest selftest_tree_canon

# ── ★ ★ 「哪些不屬於本樹的卷可以給刪除命令」的自測 ──────────────────────────
#
# 上面那條 label 判據只分得清**機制存在之後建的卷**，而對已存在的卷
# `docker volume create` 是靜默 no-op ⇒ 之前留下的永遠補不上 label，一律落進「不明」，
# 而「不明」那一半**只進不出**。⚠ ⚠ 2026-09-04 在**兩臺開發機上各量過一次**代價：
# 開發機 A（寫下這段的那臺）11 個 target 卷 / 10 個無主 / 約 92GB，回收提示看得見 **1** 個；
# 開發機 B（`ShanireHomePC`）12 個 `fulcrum*` 卷 / 6 個無主 / 約 48GB。
# ★ 兩行並排是判據本身：卷是**機器本機狀態** ⇒ ⛔ 註解裡不帶機器名的「本機 / 實測」
# 換臺機器讀就是一句會誤導人的話。到一臺新機器上先自己數，別用上面任何一個數。
# ⇒ 補一條**不依賴建卷那一刻記下了什麼**的判據
# （`fulcrum_vol_shape`：今天的 `fulcrum_target_vol` 造不造得出這個名字）。
#
# 六條，每條守一個**不同的失效形態**：
#   ① label 說樹還在 ⇒ keep。★ ★ 它必須短路在最前 —— 錯了就是「勸人刪掉隔壁正在編譯的
#      快取」，而受害者那邊只看到一次莫名的全量重編。⚠ 這個洞是寫反證時才發現的。
#   ② label 說樹沒了 ⇒ orphan（原有那條，一個字沒改）。
#   ③ 舊命名代 + 讀不到 label ⇒ orphan —— 這是新買到的那 80GB。
#   ④ ★ ★ ★ **承重**：當代形狀 + 讀不到 label ⇒ **仍然 keep**。新規則一旦吞掉舊規則
#      保守的那一半，症狀不是回報錯誤，是有一天它給出一條刪錯卷的命令。
#   ⑤ 形狀判據這一輪不可信（shape_ok=0）⇒ 新規則整條不生效，退回原來的保守行為。
#   ⑥ ★ 形狀用**真的生成出來的名字**釘住 —— 手寫一個樣例名去比是釘不住的：
#      `fulcrum_target_vol` 改了形狀，這一條必須當場紅。
selftest_vol_verdict() {
  local rc=0 probe gen
  probe=$(mktemp -d)
  [ "$(fulcrum_vol_verdict fulcrum-target-0c672eb8e734 "$probe" 1)" = keep ] \
    || { echo "★ label 说主人还在，却被判成可删——回收提示会去刨隔壁正在用的缓存" >&2; rc=1; }
  [ "$(fulcrum_vol_verdict fulcrum-target-737269d62e58-c31a8c20540d "$probe/gone" 1)" = orphan ] \
    || { echo "★ label 说主人已经不在，却没判出来——原有那条回收路丢了" >&2; rc=1; }
  [ "$(fulcrum_vol_verdict fulcrum-target-0c672eb8e734 "" 1)" = orphan ] \
    || { echo "★ 今天的推导造不出的旧代卷名没被判出来——那几十 GB 继续没有一行字说得出" >&2; rc=1; }
  [ "$(fulcrum_vol_verdict fulcrum-target-737269d62e58-c31a8c20540d "" 1)" = keep ] \
    || { echo "★ ★ 当代形状但读不到 label 的卷被判成了可删——形状规则吞掉了保守的那一半" >&2; rc=1; }
  [ "$(fulcrum_vol_verdict fulcrum-target-0c672eb8e734 "" 0)" = keep ] \
    || { echo "★ 形状判据已声明不可信，规则却照样生效了——那正好是它最容易删错的那一轮" >&2; rc=1; }
  gen=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-a")
  [ "$(fulcrum_vol_shape "$gen")" = current ] \
    || { echo "★ 本推导刚生成的卷名（$gen）被形状判据判成了「旧代」——照这个判法它会劝人删正在用的卷" >&2; rc=1; }
  rmdir "$probe" 2>/dev/null || true
  [ "$rc" -eq 0 ] || {
    echo "  卷处置判定自测未通过——**下面那份「可以删」的清单一律不可信**。" >&2
    exit 1
  }
}
host_selftest selftest_vol_verdict

# ── ★ ★ ★ 「本門管不著的那些卷」的自測 ────────────────────────────────────
#
# 這個桶 2026-09-05 加上，起因是**三族捲逃出了掃描集，而三族全部是人工發現的**
# （成因、白名單紀律、以及為什麼「放寬 filter」治不了，都在 `vol-lock.sh` 那一段）。
#
# ⚠ ⚠ 它天生難自證：在一臺已經清乾淨的機器上它**永遠打不出一行**，而
# 「桶是空的」與「這段程式碼根本沒跑」長得一模一樣 —— 這正是 2026-09-04 那次不得不
# 造四個探針卷的原因。⇒ 下面全部用**合成輸入**，兩個方向各跑一遍，
# ⛔ 不依賴這臺宿主上此刻恰好有什麼卷。
#
# 九條，每條守一個不同的失效形態：
#   ① 別人家的卷 ⇒ `foreign`。錯了就是每趟刷一堆噪音，而**一道刷噪音的門很快就沒人看**。
#   ② docker 的匿名卷 ⇒ `anonymous`。同上。
#   ③ ★ ★ ★ **承重**：既不像本儲存庫、又不在白名單裡的 ⇒ `unattributed`。這條錯了本桶
#      等於不存在 —— 而它正是為這一格加出來的。用**真的逃逸過**的那個名字釘。
#   ④⑤ ★ ★ 白名單不許吞掉 `fulcrum*`：`fulcrum-musl-alpine-target` 與
#      `fulcrum-upstream-target` 這兩族真的逃逸過，必須報得出來。
#   ⑥ ★ 匿名判據釘在**長度**上：63 位十六進制不是 docker 的匿名卷，是有人起的名字。
#   ⑦ ★ 白名單是**前綴**不是子串 —— 子串語義正是本輪要修的那個缺陷，
#      ⛔ 不許在修它的程式碼裡把它重新引進來。
#   ⑧ ★ ★ 集合層：管轄內的先摘掉、管不著的報出來，**且順序與原樣輸出一致**。
#      判據函式對了而循環裡一個 `continue` 放錯位置，症狀同樣是「一行都不打」。
#   ⑨ ★ ★ 空桶不誤報：全機卷全在管轄內時，必須一個字都不打（與 ⑧ 一紅一綠兩個方向）。
selftest_unattributed_vols() {
  local rc=0 all scope out
  local anon64=0ed04ad35290be14708aa36d1d8b9330b535ac1ed868fd6a136f1d0c061f3cb8
  [ "$(fulcrum_vol_attribution wentian_pgdata)" = foreign ] \
    || { echo "★ 别人家的卷没被认出来——本桶会每趟刷噪音，而刷噪音的门很快就没人看了" >&2; rc=1; }
  [ "$(fulcrum_vol_attribution "$anon64")" = anonymous ] \
    || { echo "★ docker 的匿名卷没被认出来——同上" >&2; rc=1; }
  [ "$(fulcrum_vol_attribution pingora-up994-target)" = unattributed ] \
    || { echo "★ ★ ★ 逃出扫描集的卷没被报出来——本桶等于不存在，而它正是为这一格加的" >&2; rc=1; }
  [ "$(fulcrum_vol_attribution fulcrum-musl-alpine-target)" = unattributed ] \
    || { echo "★ ★ 白名单吞掉了 fulcrum* ——那正好是本仓自己逃逸过的那一族" >&2; rc=1; }
  [ "$(fulcrum_vol_attribution fulcrum-upstream-target)" = unattributed ] \
    || { echo "★ ★ 白名单吞掉了 fulcrum* ——投稿六那棵克隆的卷就是这个形状" >&2; rc=1; }
  [ "$(fulcrum_vol_attribution "${anon64%?}")" != anonymous ] \
    || { echo "★ 63 位十六进制被当成了 docker 的匿名卷——匿名判据一放宽就会静默吞掉人起的名字" >&2; rc=1; }
  [ "$(fulcrum_vol_attribution x-wentian-y)" = unattributed ] \
    || { echo "★ 白名单按子串匹配了——子串语义正是本轮要修的那个缺陷，⛔ 不许再引进来" >&2; rc=1; }

  # ⑧ 集合層：一份合成的「全機卷」，其中兩個在管轄內、兩個有主、兩個管不著。
  all="fulcrum-cargo
fulcrum-target-aaaaaaaaaaaa-bbbbbbbbbbbb
wentian_pgdata
$anon64
pingora-up994-target
fulcrum-musl-alpine-target"
  scope="fulcrum-cargo
fulcrum-target-aaaaaaaaaaaa-bbbbbbbbbbbb"
  out=$(fulcrum_unattributed_vols "$all" "$scope")
  [ "$out" = "pingora-up994-target
fulcrum-musl-alpine-target" ] \
    || { echo "★ ★ 管不着的那两个没被原样报出来（实得：${out:-<空>}）——集合层错了，判据对也没用" >&2; rc=1; }

  # ⑨ 反方向：全機卷全在管轄內 ⇒ 一個字都不打。
  out=$(fulcrum_unattributed_vols "$scope" "$scope")
  [ -z "$out" ] \
    || { echo "★ ★ 管辖内的卷被报成了「管不着」（实得：$out）——空桶必须不误报" >&2; rc=1; }

  [ "$rc" -eq 0 ] || {
    echo "  「本门管不着的卷」自测未通过——**下面那份清单一律不可信**。" >&2
    exit 1
  }
}
host_selftest selftest_unattributed_vols

# ── ★ ★ 門禁互斥的自測 ──────────────────────────────────────────────────────
#
# 卷分開之後還剩一種：**同一棵樹上並行跑兩次門禁**，兩次仍然共用那一個卷。
# 這裡同樣兩個方向都驗，外加兩條這把鎖特有的失效形態。
#
# ★ 每條都起一個真的子 shell 去調真的 `fulcrum_lock_acquire` —— 不做樁。
#   這把鎖在這臺宿主上到底成不成立（`flock` 在 MSYS 上根本沒有），
#   只有讓它真的去 `mkdir` 一次才知道。
selftest_gate_lock() {
  local rc=0 root out dead
  root=$(mktemp -d)

  # ① 鎖被一個**活著的**處理程序拿著 ⇒ 第二次必須失敗，並且**說出為什麼**。
  #   ★ 拿本處理程序自己的 pid 當「活著的持有者」：不必起後台處理程序，而 `kill -0 $$` 必然為真。
  mkdir -p "$root/probe.lock"
  printf '%s\n' "$$" > "$root/probe.lock/pid"
  if out=$(FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD='' \
             bash -c '. "$1" && fulcrum_lock_acquire probe /selftest/tree' _ "$VOL_LOCK_LIB" 2>&1); then
    echo "★ 锁已被占用，fulcrum_lock_acquire 却成功了 —— 这把锁等于不存在" >&2; rc=1
  else
    case "$out" in
      *"另一次门禁正在这棵工作树上跑"*) ;;
      *)
        echo "★ 它拒绝了，却没说出为什么——拿不到锁必须说人话，不是静默失败：" >&2
        printf '%s\n' "$out" >&2; rc=1
        ;;
    esac
  fi
  [ -d "$root/probe.lock" ] || { echo "★ 抢锁失败的那一次把**别人的**锁删掉了" >&2; rc=1; }

  # ② 鎖放開之後必須拿得到；★ 並且拿到的那一次**退出時要還回來**。
  #   ⚠ trap 沒生效的話，第一次跑完就把這棵樹永久鎖死了，而症狀要到下一次才出現。
  rm -rf "$root/probe.lock"
  if ! out=$(FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD='' \
               bash -c '. "$1" && fulcrum_lock_acquire probe /selftest/tree && [ -d "$2/probe.lock" ]' \
               _ "$VOL_LOCK_LIB" "$root" 2>&1); then
    echo "★ 锁放开之后仍然拿不到（或拿到了却没有建出锁目录）：" >&2
    printf '%s\n' "$out" >&2; rc=1
  fi
  [ ! -d "$root/probe.lock" ] || { echo "★ 持锁的进程退出了、锁却没还回来 —— trap 没生效" >&2; rc=1; }

  # ③ 可重入：`docker-run.sh` 持鎖期間要能調 `tests/m1/systemd-run.sh`，
  #   而後者單獨跑時必須自己上鎖 —— 少了這一條，M1 那一格會被自己鎖死。
  mkdir -p "$root/probe.lock"
  printf '%s\n' "$$" > "$root/probe.lock/pid"
  FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD="$root/probe.lock" \
    bash -c '. "$1" && fulcrum_lock_acquire probe /selftest/tree' _ "$VOL_LOCK_LIB" >/dev/null 2>&1 \
    || { echo "★ 已经持锁的进程树里再取一次被挡住了 —— M1 那一格会被自己锁死" >&2; rc=1; }
  rm -rf "$root/probe.lock"

  # ④ 陳舊鎖（上一次被 Ctrl-C 掉、沒走到 trap）⇒ 接管，並且**說出來**。
  #   ⚠ 先證明這個 pid 真的已經不在——「造不出現場」不算「判據通過」。
  ( : ) &
  dead=$!
  wait "$dead" 2>/dev/null || true
  if kill -0 "$dead" 2>/dev/null; then
    echo "★ 造不出「持有者已死」的现场（pid $dead 居然还在）——本条这次等于没验" >&2; rc=1
  else
    mkdir -p "$root/stale.lock"
    printf '%s\n' "$dead" > "$root/stale.lock/pid"
    if out=$(FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD='' \
               bash -c '. "$1" && fulcrum_lock_acquire stale /selftest/tree' _ "$VOL_LOCK_LIB" 2>&1); then
      case "$out" in
        *接管*) ;;
        *) echo "★ 接管了一把陈旧锁却没说 —— 一次没人知道的接管与没有锁无异" >&2; rc=1 ;;
      esac
    else
      echo "★ 持有者已经不在，锁却接管不了 —— 一次 Ctrl-C 就能把这棵树永久锁死：" >&2
      printf '%s\n' "$out" >&2; rc=1
    fi
  fi

  rm -rf "$root"
  [ "$rc" -eq 0 ] || {
    echo "  门禁互斥自测未通过——**「这棵树上没有第二次门禁在跑」这个前提一律不可信**。" >&2
    exit 1
  }
}
host_selftest selftest_gate_lock

# ★ 走到這裡 ⇔ 上面十條宿主機自測這一趟真的全跑過、全過了（任何一條紅都已 `exit 1`）
#   ⇒ 這時才寫通過記錄（G159）。寫不成不判紅：下一趟 pre-push 只是照跑自測，不影響本趟判定。
if [ "$RUN_HOST_SELFTESTS" = 1 ] && [ -n "$HOST_SELFTEST_STAMP" ] && [ -n "$HOST_SELFTEST_FP" ]; then
  host_selftest_stamp_write "$HOST_SELFTEST_STAMP" "$HOST_SELFTEST_FP" \
    || echo "⚠ 宿主机自测的通过记录没写成（$HOST_SELFTEST_STAMP）—— 下一趟 pre-push 会照跑自测。" >&2
fi

# 不依賴 git 的備援掃描：先一次性判「整棵樹有沒有 CR」（一遍就夠，常見情況到此為止），
# 有才逐個找是哪些檔案（這時候慢一點無所謂，因為已經要人來看了）。
#
# ★ 全程用 NUL 分隔（`-print0` / `read -d ''`），不用換行——**含換行符的檔案名**在按行
#   分隔時會被拆成兩個不存在的路徑。這與 git 那條路上「按 TAB 取路徑」是同一件事：
#   分隔符必須是路徑裡不可能出現的位元組，而換行不是。
crlf_scan_without_git() {
  local find_args=(
    "$REPO_UNIX"
    \( -name .git -o -name target -o -name run -o -name __pycache__ -o -name node_modules \) -prune -o
    -type f ! -name '*.png' ! -name '*.jpg' ! -name '*.webp' ! -name '*.ico' ! -name '*.pdf' -print0
  )
  # 快路徑：全樹一遍，只問「有沒有 CR」。常見情況到此為止，不必逐個開子處理程序。
  if [ "$(find "${find_args[@]}" | xargs -0 cat 2>/dev/null | tr -dc '\r' | wc -c)" -eq 0 ]; then
    return 0
  fi
  # 有了才逐個找是哪些——這時候慢一點無所謂，因為已經要人來看了。
  local f
  while IFS= read -r -d '' f; do
    [ "$(nul_count "$f")" -eq 0 ] || continue        # 二進位，跳過
    [ "$(cr_count "$f")" -eq 0 ]  || printf '%s\n' "${f#"$REPO_UNIX"/}"
  done < <(find "${find_args[@]}")
}

# ── git 答不答得出「$REPO_UNIX 是不是工作樹」────────────────────────────────
#
# ★ ★ 判據**分兩段問**，⛔ 不合成一句。`command -v git && git rev-parse …` 那種寫法
#   把三種死因壓成同一句話 ——「沒裝」「不是儲存庫」「裝著、也是儲存庫、但它拒答」——
#   而 2026-09-08 那次 CI 連紅十一趟，真正發生的正是**第三種**：
#     fatal: detected dubious ownership in repository at '/w'
#   ⇒ 那句 else 裡的話（「沒裝，或不是 git 儲存庫」）當時**兩條都不成立**。
# ★ 用 `2>&1 >/dev/null` **只捕 stderr**（⛔ 順序不能反，反了是把兩股都並進變數），
#   把 git 的原話逐行縮進打出來。⛔ 本判據**不自己給死因分類** —— 真因由 git 的原話說，
#   那比任何分類都準。
# ⚠ 用 `|| git_rc=$?` 接住，⛔ 否則 `set -e` 會在判據最該說話的一刻把指令碼掐掉。
GIT_EOL_OK=0
if ! command -v git >/dev/null 2>&1; then
  echo "★ 行尾检查拿不到权威口径：这个环境里**没有 git**。" >&2
else
  git_eol_rc=0
  git_eol_err=$(git -C "$REPO_UNIX" rev-parse --git-dir 2>&1 >/dev/null) || git_eol_rc=$?
  if [ "$git_eol_rc" -eq 0 ]; then
    GIT_EOL_OK=1
  else
    echo "★ 行尾检查拿不到权威口径：git **装着**（$(command -v git)），" >&2
    echo "  \`git -C $REPO_UNIX rev-parse --git-dir\` 却退了 $git_eol_rc。它自己的原话：" >&2
    printf '%s\n' "$git_eol_err" | sed 's/^/      /' >&2
  fi
fi

if [ "$GIT_EOL_OK" = 1 ]; then
  echo "[docker-run] 行尾检查：git ls-files --eol（权威口径，尊重 .gitattributes）"
  # ★ 路徑按 **TAB** 取。`git ls-files --eol` 的每行是
  #     i/<eol><空白>w/<eol><空白>attr/<attr…><空白><TAB><PATH>
  #   —— 路徑前那個製表符是唯一可靠的分隔點。
  #   寫死列號不行（attr 段字數不定，`attr/text=auto eol=lf` 會被拆成兩列）；
  #   `$NF` 同樣不行——**含空格的路徑會被砍到只剩最後一段**，打出來像個不存在的檔案。
  #   w/ 那一列則在 $1 內部按空白再切一刀取，避免 `index()` 式的模糊匹配。
  #   ★ `core.quotePath=false`：否則非 ASCII 路徑會被 C-quote 成 "\344\275\240…"，
  #     人拿著它去 `ls` 是對不上的。
  CRLF_FILES=$(git -C "$REPO_UNIX" -c core.quotePath=false ls-files --eol \
                 | awk -F'\t' '{ split($1, a, " "); if (a[2] == "w/crlf") print $2 }')
else
  echo "  ⇒ 改用逐字节扫描。⚠ ⚠ **这是另一把尺子，不是同一道判据的备用实现**：" >&2
  echo "    它不认识 .gitattributes，也**不认识跟踪状态** —— 未跟踪与 .gitignore 掉的文件" >&2
  echo "    照样会被算进来（实测：handoff/ 下十几份 .md 会被列出来）⇒ 它比权威口径更严，" >&2
  echo "    ⛔ 下面那条「一行修复」对这些文件是**错的**，别照着敲。" >&2
  CRLF_FILES=$(crlf_scan_without_git)
fi

if [ -n "$CRLF_FILES" ]; then
  echo "★ 以下文件在**工作树**里是 CRLF，而 .gitattributes 要求无条件 LF：" >&2
  # ★ 一行一個檔案，**不靠詞拆分**。`printf '%s\n' $CRLF_FILES` 未加引號時，
  #   一個含空格的檔案名會被拆成兩行，看起來像兩個不存在的檔案——
  #   而這正是人要拿去排查的那行字。
  printf '%s\n' "$CRLF_FILES" | while IFS= read -r crlf_file; do
    printf '      %s\n' "$crlf_file" >&2
  done
  echo "  一行修复（重新按索引里的 LF 检出）：" >&2
  echo "      git -C \"$REPO_UNIX\" rm --cached -r . >/dev/null && git -C \"$REPO_UNIX\" reset --hard" >&2
  # ★ 這裡必須用 printf 而不是 echo：這行文字里帶著反斜槓，而 `echo` 展不展開轉義
  #   隨 shell 而變（bash 內建不展開，dash / sh 的 echo 會展開）。shellcheck SC2028 就是衝它來的。
  printf '  %s\n' '★ 或者只修这几个：用 LF 重写（Windows 版 Python 要按字节写或传 newline="\n"）。' >&2
  exit 1
fi

# ── 儲存庫裡**每一個** target/ 都必須是空的（G107）────────────────
#
# 攔的是這樣一件事：**有人在宿主機上跑了 cargo，幾個 GB 悄悄長進儲存庫，而沒有人發現。**
#
# ★ 掃的是**儲存庫裡所有的 `target/`**，不是寫死的一個路徑 ——
#   只看儲存庫根那一個的話，`vendor/pingora/target/` 裡躺著幾個 GB 它也不會紅。
#   > ★ ★ **反證證的是「它對我塞進去的那個位置分得開好壞」，證不了「該看的位置它都看了」** ——
#   > 覆蓋面與靈敏度是兩件事，而反證只量得到後者。
#
# ★ ★ **判據是「非空」，不是某個 MB 閾值**：容器把命名卷掛在 `/w/target` 上，
#   Docker 只會在宿主機建一個**空的掛載點目錄** ⇒ 空 = 正常，非空 = 有別的東西寫過它。
#
# ★ 放在宿主機側、`docker run` 之前：任何一次呼叫（含 `LINT_ONLY=1`）都會經過它。
# ⚠ 它**不阻止**長出來，只保證長出來之後活不過下一次門禁 —— 它不是寫保護。
# ★ `-prune` 既不下到 `.git`，也不下到已經命中的 target 裡（後者在 GB 級目錄上真的會慢）。
TARGET_DIRS=$(find "$REPO_UNIX" -maxdepth 4 \
  \( -name .git -o -name node_modules \) -prune -o \
  -type d -name target -print -prune 2>/dev/null || true)

DIRTY_TARGETS=""
while IFS= read -r d; do
  [ -n "$d" ] || continue
  # ★ 判據仍是「非空」而不是某個 MB 閾值：容器只在 /w/target 上掛命名卷，
  #   Docker 只會在宿主機建一個**空的掛載點目錄** ⇒ 空 = 正常，非空 = 有別的東西寫過它。
  if [ -n "$(ls -A "$d" 2>/dev/null)" ]; then
    DIRTY_TARGETS="${DIRTY_TARGETS}${d}
"
  fi
done <<<"$TARGET_DIRS"

if [ -n "$DIRTY_TARGETS" ]; then
  echo "★ 仓库里有非空的 target/ —— 有东西在这个仓库里构建过，而产物落在了工作树上。" >&2
  echo "  容器只在 /w/target 上挂 docker 命名卷；**别的 target/ 一律直接写在宿主机的仓库里**。" >&2
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    echo "  ── $d（$(du -sh "$d" 2>/dev/null | cut -f1)）" >&2
    du -sh "$d"/* 2>/dev/null | sed 's/^/        /' >&2
  done <<<"$DIRTY_TARGETS"
  echo "  ⚠ G107（PLAN.md §10）：**Rust 一律在 Docker 里跑，不要在宿主机上跑 cargo**；" >&2
  echo "    而在容器里跑 cargo 时也要给 --target-dir，别让它写进 bind mount。" >&2
  echo "  修复（这些目录都在 .gitignore 里，里面没有任何被跟踪的文件）：" >&2
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    echo "      rm -rf \"$d\"" >&2
  done <<<"$DIRTY_TARGETS"
  exit 1
fi

# ★ `MUSL_ONLY=1` 在這裡就分岔出去：那一格自己 `docker build` 一個 Alpine 鏡像，
#   **既不需要建置鏡像，也不需要那次 `docker run`** —— 走到下面只會白等幾分鐘。
#   ⚠ 上面兩道宿主機側的門（行尾 / target 非空）**故意留在它前面**：
#     它們守的是工作樹本身，與跑哪一格無關。
if [ "${MUSL_ONLY:-0}" = "1" ]; then
  bash "$REPO_UNIX/tests/musl/product.sh"
  exit $?
fi

# git-bash 會把容器內的 /w 之類路徑改寫成 Windows 路徑，必須關掉
export MSYS_NO_PATHCONV=1

# 宿主機側的掛載源必須是 Windows 形式的絕對路徑
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO_UNIX")"
else
  REPO_HOST="$REPO_UNIX"
fi
# docker build 的上下文也是宿主機路徑，同樣要轉換
DOCKER_CTX="${REPO_HOST}/docker"

# ── 建置鏡像：不存在就做，**內容變了也要重做** ──────────────────────────────────
#
# ★ 只判「鏡像在不在」是不夠的：Dockerfile 改了也會照樣命中舊鏡像，此後每一次「全綠」
#   用的都不是儲存庫宣告的那個基礎鏡像，而且沒有任何東西會說出來。
#   ⇒ 把 Dockerfile 的內容雜湊寫成鏡像的 label，每次比對；**任何**改動都會觸發重建。
# ★ 拿不到雜湊時**強制重建**，不是跳過檢查 —— 「沒能檢查」不算「檢查通過」。
LABEL_KEY="cool.cnb.fulcrum.dockerfile-sha256"
DOCKERFILE="${REPO_UNIX}/docker/Dockerfile.build"
if command -v sha256sum >/dev/null 2>&1; then
  DOCKERFILE_SHA=$(sha256sum "$DOCKERFILE" | cut -d' ' -f1)
elif command -v git >/dev/null 2>&1; then
  # ⚠ ⚠ ★ `--stdin` 不是風格：本行在第 615 行那句 `export MSYS_NO_PATHCONV=1` **之後**，
  #   ⇒ 原生 git.exe 會拿到未轉換的 `/d/...` 並報
  #   `fatal: could not open '/d/…' for reading`（2026-09-08 實測 rc=128）。
  #   本檔案第 42 行為行尾檢查寫下的那條位置硬要求，說的正是這件事 —— 而這一行在它之後。
  #   ⇒ 用 `--stdin` 由 bash 做重定向，**argv 裡沒有路徑**，設不設那個變數都一樣。
  #   （同源寫法：`tests/lib/vol-lock.sh:168`。實測兩種寫法雜湊逐字相同。）
  DOCKERFILE_SHA="githash-$(git hash-object --stdin < "$DOCKERFILE")"
else
  DOCKERFILE_SHA=""
  echo "⚠ 既没有 sha256sum 也没有 git，无法判断构建镜像是否过期——本次强制重建" >&2
fi

IMAGE_SHA=$(docker image inspect "$IMAGE" --format "{{index .Config.Labels \"$LABEL_KEY\"}}" 2>/dev/null || true)
if [ -z "$DOCKERFILE_SHA" ] || [ "$IMAGE_SHA" != "$DOCKERFILE_SHA" ]; then
  if [ -z "$IMAGE_SHA" ]; then
    echo "[docker-run] building $IMAGE（镜像不存在，或它是加这道检查之前做的）..."
  else
    echo "[docker-run] rebuilding $IMAGE：Dockerfile 变了（镜像里记的是 ${IMAGE_SHA:0:12}…，现在是 ${DOCKERFILE_SHA:0:12}…）"
  fi
  docker build -t "$IMAGE" --label "$LABEL_KEY=$DOCKERFILE_SHA" \
    -f "${DOCKER_CTX}/Dockerfile.build" "$DOCKER_CTX"
fi

# ── 快取卷 ─────────────────────────────────────────────────────────────────
#
# `fulcrum-cargo`（下載下來的 crate 原始碼）與工具鏈無關，**可以跨鏡像共享**。
#
# ★ ★ `target` 不行，它必須跟著鏡像走。cargo 的 fingerprint 覆蓋 rustc 版本與 flags，
#   **但不覆蓋 C 工具鏈** —— build script 產出的東西只在自身宣告的輸入變化時才重建，
#   於是「換基礎鏡像但 rustc 不變」會**沿用舊鏡像編出來的 C 目標檔案**。
#   ⇒ 卷名帶上 Dockerfile 的內容雜湊：換鏡像自動換卷，舊卷留著還能退回。
#
# ★ ★ ★ **它還必須跟著「哪一棵工作樹」走。** 上面那條理由一個字都沒變，只是它當年
#   成立的前提是「一臺機器上只有一棵原始碼樹」——而那個前提今天不成立了：本機有主樹
#   加 `.claude/worktrees/` 下若干棵，各自的 `/w` 是不同的原始碼，`/w/target` 卻是同一個卷。
#   ⇒ 卷名再拼上這棵樹路徑的短雜湊，兩條性質同時成立（換鏡像換卷、換樹也換卷）。
#   ⚠ ⚠ 失效形態不是「編得慢」，是**門給出別人家的讀數、而兩邊都不紅**：實測撞見兩次，
#     一次編譯錯誤指向一個只存在於另一棵樹的符號；另一次報「696 條全綠、結束代碼 0」，
#     跑的卻是**沒有本次新判據**的舊測試二進位 —— 它不回報錯誤，它給一個像樣的綠的錯答案。
#   推導只有一份（tests/lib/vol-lock.sh），四條自測在本檔案開頭的 selftest_target_vol。
TREE_TAG="$(fulcrum_tree_tag "$REPO_UNIX")"
TARGET_VOL="$(fulcrum_target_vol "$DOCKERFILE_SHA" "$REPO_UNIX")"

# ── ★ ★ 同一棵樹上同一時刻只許跑一次 ────────────────────────────────────────
#
# 卷分開只解決「別的樹」。同一棵樹上並行跑兩次門禁，兩次仍然共用這一個卷 —— 回到同一種壞法。
# ⇒ **拿卷之前**先取一把與卷同名的鎖；拿不到就當場說清楚並退出，不排隊、不靜默繼續。
# ★ 用 `mkdir` 而不是 `flock`：這臺宿主（Windows + Git Bash / MSYS）上沒有 flock，
#   寫一把其實不生效的鎖比沒有鎖更壞。機制與自測都在 tests/lib/vol-lock.sh。
fulcrum_lock_acquire "$TARGET_VOL" "$REPO_UNIX" || exit 1

docker volume create fulcrum-cargo  >/dev/null
# ★ 新建時把「屬於哪棵樹」寫進 label（見 tests/lib/vol-lock.sh）——
#   下面那條回收提示只有靠它才分得清「主人已經不在」與「主人還在」。
# ⚠ 已存在的卷是**靜默 no-op**，label 寫不進去；那一類下面按「不明」處理，不當無主的。
VOL_EXISTED=0
docker volume inspect "$TARGET_VOL" >/dev/null 2>&1 && VOL_EXISTED=1
fulcrum_target_vol_create "$TARGET_VOL" "$REPO_UNIX"

# ★ ★ 新建的那一次**當場自證** label 真的寫進去了、而且讀回來就是這棵樹。
#   ⚠ 少了這一條，label 悄悄沒寫成的後果是：下面那條回收路對每個卷都答「不明」，
#     於是它什麼也不做，而現象只是「磁碟怎麼還在漲」—— 沒有一行字會說出原因。
#   ★ 不判紅，但要喊（與 cache.sh「灌回之後卷還是空的」同一條紀律）：
#     這是一條回收提示，不是量具本身，為它掐掉整輪門禁不成比例。
if [ "$VOL_EXISTED" = 0 ]; then
  VOL_TREE=$(docker volume inspect --format "{{index .Labels \"$FULCRUM_TREE_LABEL\"}}" "$TARGET_VOL" 2>/dev/null || true)
  [ "$(fulcrum_tree_state "$VOL_TREE")" = live ] || {
    echo "⚠ 刚建出来的卷认不出自己属于哪棵树（label 读回来是 '${VOL_TREE:-<空>}'）——" >&2
    echo "  下面那条「主人已经不在」的回收提示这一轮等于没有（不判红，但别以为它在工作）。" >&2
  }
fi

# ── 遺留卷報告（下面三個桶）：`VOL_REPORT=0` 時整段不跑（G158）─────────────────
#
# ★ 只給 pre-push 門用：這一段答的是「磁碟上躺著哪些舊卷、能不能刪」，⛔ 不判紅，
#   與「這棵樹站不站得住」無關；而它是宿主機側最貴的一段（`docker system df -v`
#   一條就 1.4–1.9s）。完整門禁每趟照跑，預設照舊。
# ⚠ 上面那條「新建的卷當場自證 label」**有意留在這段之外**：它只在卷是本趟新建時
#   才花一次 inspect，而新建那一趟錯過了，就再沒有哪一趟會驗它。
if [ "${VOL_REPORT:-1}" = "1" ]; then
  # 舊卷不自動刪（可能還想退回），但要說出來——不然它們會無聲地佔滿磁碟。
  #
  # ⚠ ⚠ **只勸人刪「本樹的」舊卷。** 帶上樹標籤之後，`fulcrum-target-*` 裡躺著的多數是
  #   **別的工作樹正在用的卷**；照舊一股腦列成「舊卷」並附上 `docker volume rm`，
  #   那就成了一條教人去刨別人工作樹的提示 —— 而受害者那邊只會看到一次莫名的全量重編。
  #   判據是後綴：`-<本树标签>` 結尾的才是本樹的。
  ALL_VOLS=$(docker volume ls -q --filter name=fulcrum-target || true)
  MINE_STALE=$(printf '%s\n' "$ALL_VOLS" | grep -- "-${TREE_TAG}\$" | grep -v "^${TARGET_VOL}\$" || true)
  OTHER_VOLS=$(printf '%s\n' "$ALL_VOLS" | grep -v -- "-${TREE_TAG}\$" | grep -v '^$' || true)
  if [ -n "$MINE_STALE" ]; then
    echo "[docker-run] 这棵树另有 $(printf '%s\n' "$MINE_STALE" | wc -l) 个旧的 target 卷（对应更早的构建镜像），确认不再回退就删："
    printf '%s\n' "$MINE_STALE" | sed 's/^/      docker volume rm /'
  fi

  # ★ ★ **「別的樹的卷不給刪除命令」解決了誤刪，沒解決磁碟。** 工作樹用完就刪，
  #   它們的卷不會跟著走 —— 一個約 6GB，而卷名後綴是雜湊、反推不迴路徑，
  #   於是**沒有任何東西說得出哪些已經沒有主人了**。
  #   ⇒ 按 label 記著的那棵樹現在還在不在，把「不屬於本樹」再分成兩半：
  #     主人已經不在 ⇒ 給刪除命令（刪了碰不到任何人）；
  #     主人還在、或讀不到 label ⇒ 照舊只報數、不給命令。
  #   ⚠ 「讀不到 label」有意歸後一半：加這條規則之前留下的卷讀不出主人，
  #     而「沒能檢查」不算「檢查通過」。
  # ★ ★ 形狀判據（`fulcrum_vol_shape`）**這一輪可不可信，當場自證**：拿本樹自己那個
  #   真名字去問。`fulcrum_tree_tag` 在既沒有 sha256sum 也沒有 git 的宿主上會退到
  #   另一種標籤，那時當代的名字會被判成「舊代」—— ⛔ 那正是它最容易刪錯的一輪。
  #   ⇒ 自證不過就把整條規則關掉（退回原來的保守行為），**並且說出來**。
  if [ "$(fulcrum_vol_shape "$TARGET_VOL")" = current ]; then
    SHAPE_OK=1
  else
    SHAPE_OK=0
    echo "⚠ 本树的卷名（$TARGET_VOL）不符合当代形状 —— 这台宿主上 fulcrum_tree_tag 多半" >&2
    echo "  退到了兜底标签。⇒ 「今天的推导造不出这个名字」这条规则本轮**整条关掉**，" >&2
    echo "  旧命名代的卷会退回「不明」那一半（不判红，但别以为它在工作）。" >&2
  fi

  if [ -n "$OTHER_VOLS" ]; then
    mapfile -t OTHER_LIST <<< "$OTHER_VOLS"
    # 一趟 inspect 問完。★ 卷名在前、路徑在最後一段：`IFS='|' read` 把剩下的整段都給
    #   最後一個變數 ⇒ 路徑裡真有 `|` 也只是這一行顯示得難看，不會把卷名讀錯、
    #   進而給出一條刪錯卷的命令。
    OTHER_META=$(docker volume inspect --format "{{.Name}}|{{index .Labels \"$FULCRUM_TREE_LABEL\"}}" \
                   "${OTHER_LIST[@]}" 2>/dev/null || true)
    ORPHAN_VOLS=()
    ORPHAN_WHY=()
    KEEP_VOLS=()
    while IFS='|' read -r vol tree; do
      [ -n "$vol" ] || continue
      if [ "$(fulcrum_vol_verdict "$vol" "$tree" "$SHAPE_OK")" = orphan ]; then
        ORPHAN_VOLS+=("$vol")
        # ★ 把**憑哪條規則**判的寫在命令旁邊：兩條規則的證據強度不同，
        #   而人是照著這行去按刪除鍵的。
        if [ -n "$tree" ]; then
          ORPHAN_WHY+=("工作树已不在：$tree")
        else
          ORPHAN_WHY+=("读不到 label，且今天的推导造不出这个名字")
        fi
      else
        KEEP_VOLS+=("$vol")
      fi
    done <<< "$OTHER_META"
    ORPHAN_N=${#ORPHAN_VOLS[@]}
    KEEP_N=${#KEEP_VOLS[@]}
    # ⚠ ⚠ **「讀不到」不許變成「沒這回事」。** `inspect` 整批失敗時上面兩個計數都是 0，
    #   於是這一段一個字都不打 —— 而它取代的是 a4cc2b5 那句**無條件**的報數，
    #   ⇒ 「另有 N 個卷不屬於這棵樹」這句話會安靜地消失。
    #   把對不上的那些一律並進「不給命令」的那一半（保守方向），並說出發生過。
    OTHER_TOTAL=$(printf '%s\n' "$OTHER_VOLS" | wc -l)
    UNREAD_N=$((OTHER_TOTAL - ORPHAN_N - KEEP_N))
    if [ "$UNREAD_N" -gt 0 ]; then
      echo "⚠ 有 $UNREAD_N 个 target 卷的 label 没读到（docker volume inspect 没给出这几行）——" >&2
      echo "  它们一律按「还有人要」算，不进下面那份删除清单。" >&2
      KEEP_N=$((KEEP_N + UNREAD_N))
    fi

    # ★ ★ **把體積說出來，不只報個數。** 2026-09-04 實測：本機 10 個無主卷共約 92GB，
    #   而當時這裡只會說「另有 N 個卷」—— 一個個數讀起來無關痛癢，一串 GB 不是。
    #   ⚠ `docker system df -v` 比 `docker volume ls` 貴得多（實測 1.4–1.9s vs 0.34s）
    #   ⇒ 只在**真有東西要報**的時候問一次；兩個桶都空就一次都不問。
    #   ⚠ ⚠ 量不到時打「未量到」，⛔ 絕不打 0 —— 「讀不到」不許變成「沒這回事」。
    VOL_SIZES=""
    if [ "$ORPHAN_N" -gt 0 ] || [ "$KEEP_N" -gt 0 ]; then
      VOL_SIZES=$(fulcrum_vol_sizes)
    fi
    # ★ 實作搬進了 `vol-lock.sh`（`fulcrum_vol_size_of`），因為下面第三個桶也要用它 ——
    #   兩處各寫一遍的失效形態不是回報錯誤，是有一天兩邊對「沒量到」給出不同的說法。
    #   ⚠ 順帶修掉一處：原來的 `grep -F -- "$want|"` 是**子串**匹配，
    #   `x-fulcrum-cargo|…` 那一行會被 `fulcrum-cargo` 認領 ⇒ 一個卷報上另一個卷的體積。
    vol_size_of() { fulcrum_vol_size_of "$VOL_SIZES" "$1"; }

    if [ "$ORPHAN_N" -gt 0 ]; then
      echo "[docker-run] 另有 $ORPHAN_N 个 target 卷判定为**无主**，删了碰不到任何人："
      for i in "${!ORPHAN_VOLS[@]}"; do
        printf '      docker volume rm %s   # %s，%s\n' \
          "${ORPHAN_VOLS[$i]}" "$(vol_size_of "${ORPHAN_VOLS[$i]}")" "${ORPHAN_WHY[$i]}"
      done
    fi
    if [ "$KEEP_N" -gt 0 ]; then
      echo "[docker-run] 另有 $KEEP_N 个 target 卷**不属于这棵工作树**，且判不出是不是无主的："
      for vol in "${KEEP_VOLS[@]}"; do
        printf '             %s   %s\n' "$vol" "$(vol_size_of "$vol")"
      done
      echo "             有意不给删除命令：删掉别人正在用的那一个，只会让那棵树莫名其妙地全量重编。"
      echo "             ⚠ 但体积写在这里 —— 这一半只进不出，2026-09-04 它悄悄涨到过 92GB。"
    fi
  fi

  # ── ★ ★ ★ 第三個桶：本門**管不著**的卷（只報，⛔ 不給刪除命令）──────────────
  #
  # 上面兩個桶都只在**掃描集內**說話，而掃描集是個**子串**篩子；三族卷從它下面漏過去過，
  # 而**三族全部是人工發現的**，門禁一次都沒幫上忙（成因、白名單紀律、以及為什麼
  # 「放寬 filter」治不了第三族，都在 `vol-lock.sh` 的 `FULCRUM_FOREIGN_VOL_PREFIXES` 那一段）。
  # ⇒ 這裡改成**無 filter** 地列一遍全機卷，減掉本門管轄內的、以及說得出屬主的，剩下的報出來。
  #
  # ⚠ ⚠ **只報名字與體積，⛔ 一律不給刪除命令。** 本門對它們的屬主一無所知 ——
  #   給命令等於拿一句「我不知道這是誰的」去勸人刪，那比不報更壞。
  # ★ 噪音賬：穩態下這個桶是空的，**一行都不打**；真出現一個 `pingora-up994-target` 才打一行。
  #   ⚠ ⚠ **具體讀數有意不寫在這裡** —— 它逐機而異，抄在程式碼註解裡就是一份會過期又沒人核的值。
  #   唯一一處在 `docs/platform/build-and-test.md`「噪音賬」那一行。
  #   ★ 這不是潔癖：本條第一版把它抄了**三處**，而三處的數是錯的（別人家/匿名寫成 11/6，
  #     實際 10/7）—— **和恰好仍是 0**，於是它自洽、沒有任何門看得見。
  # ⚠ 那個貴的量具沿用上面同一條紀律：`fulcrum_vol_sizes`（`docker system df -v`，
  #   開發機 A 上實測 1.4–1.9s）**只在真有東西要報時問一次**，桶空就一次都不問。
  ALL_MACHINE_VOLS=$(docker volume ls -q || true)
  # 本門管轄內 = 本樹活卷 + 有意共享的 registry 卷 + 掃描集裡的全部（上面兩個桶已經處置過）。
  GATE_SCOPE=$(printf '%s\n%s\n%s\n' "$TARGET_VOL" fulcrum-cargo "$ALL_VOLS")
  UNATTRIBUTED=$(fulcrum_unattributed_vols "$ALL_MACHINE_VOLS" "$GATE_SCOPE")
  if [ -n "$UNATTRIBUTED" ]; then
    UNATTR_N=$(printf '%s\n' "$UNATTRIBUTED" | wc -l)
    UNATTR_SIZES=$(fulcrum_vol_sizes)
    echo "[docker-run] 另有 $UNATTR_N 个卷，本门**管不着、也说不出属于谁**："
    while read -r vol; do
      [ -n "$vol" ] || continue
      printf '             %s   %s\n' "$vol" "$(fulcrum_vol_size_of "$UNATTR_SIZES" "$vol")"
    done <<< "$UNATTRIBUTED"
    echo "             ⛔ 有意不给删除命令：本门不知道它们的属主。判据只能是人工的三条 ——"
    echo "                「仓库里零引用 + 没有任何容器引用 + 找不到那棵检出」（09-04/05 三次都是这么判的）。"
    echo "             ★ 若查实是别的项目的，把前缀加进 tests/lib/vol-lock.sh 的"
    echo "                FULCRUM_FOREIGN_VOL_PREFIXES —— ⛔ 只放别人家的，别拿它消音（纪律写在那份清单头上）。"
  fi
fi

# ★ ★ ★ 這裡的花括號**不是風格，是判據本身**（實測的一次假綠）。
#
#   原文是：  CMD='cargo build --release --locked || cargo build --release'
#   而後面又拼成：CMD="$LINT_CMD && $CMD"，於是整條鏈是
#
#       lint && build --locked || build && vendor && m0 && unclaimed
#
#   shell 裡 `&&` 與 `||` **同優先級、左結合**，所以它實際被解析成
#
#       ((lint && build --locked) || build) && vendor && m0 && unclaimed
#
#   ★ **lint 一紅，左邊整個為假，`|| build` 就把它接住了** —— 後面全部照跑，結束代碼 0。
#     lint 門等於不存在，而且沒有任何症狀（G44 就是這麼紅了三次提交沒人看見）。
#     第二個後果：lint 一紅就悄悄改跑不帶 `--locked` 的建置，而 `--locked` 正是
#     G29 第 2 條的執行者。
#
#   修法兩條，缺一不可：① 用 `{ ...; }` 把回落綁死在建置這一步上；
#   ② 回落真的發生時**喊出來** —— 一個沒人知道發生過的回落，與沒有回落無異。
CMD='{ cargo build --release --locked || { echo "⚠ --locked 构建失败，回落到不带 --locked 的构建（Cargo.lock 可能已过期，G29 第 2 条）" >&2; cargo build --release; }; }'

# ★ lint 門：`cargo fmt --check` + `cargo clippy -D warnings`。
#
# ★ 用 `--workspace` 而不是寫死包名：**寫死的作用域會在加新 crate 時悄悄漏掉它們**。
#
# ★ vendor 那些 `unexpected cfg condition value` 不會把這道門帶紅（實測）：
#   `--` 之後的 `-D warnings` 只作用於 clippy 直接 lint 的那個 crate，
#   `vendor/pingora` 作為依賴被普通 rustc 編譯，它的 warning 就只是 warning。
#
# ★ ★ **shellcheck 與 Rust 那兩樣同等重要**：本儲存庫的判據大半寫在 shell 裡，而 shell 的
#   失敗模式是安靜的 —— `cmd \n arg`（字面 `\n` 讓後面的參數整體錯位一個，SC1012）與
#   `printf '%s\n' $LIST`（詞拆分劈開含空格的項）兩種，`bash -n` 全部放行。
LINT_CMD='cargo fmt --all -- --check'
# ★ ★ `spikes/musl-boringssl` **在根 workspace 之外**（它自己有一個空的 `[workspace]` 表），
#   所以上面那條 `--all` 夠不到它 —— 而「夠不到」的表現是**綠**。
#   ⇒ 單獨給它一條 `--manifest-path`。
#   ⚠ **只給 fmt，不給 clippy**，而這是權衡不是遺漏：clippy 要先把依賴編出來，
#     那意味著**每一次 lint 都要編一遍 BoringSSL**；`cargo fmt` 唯讀原始碼，零建置成本。
#     ⇒ 那份探針的 Rust 程式碼目前**沒有 clippy 看著**，已登記在
#     docs/verification/musl-boringssl.md「這份探針證不了什麼」一節。
LINT_CMD="$LINT_CMD && cargo fmt --manifest-path spikes/musl-boringssl/Cargo.toml --all -- --check"
# ★ `vendor/pingora` 同理：它是**另一個 workspace**（根 `[patch]` 把它當路徑依賴，不是成員），
#   根那條 `--all` 夠不到它，夠不到的表現同樣是**綠**。2026-10-16 實測漂了兩處，全是 fork
#   自己寫的行（改動 19 的 `v1/server.rs`、改動 6 的 `pingora-rustls` 測試）；上游原碼在本鏡像的
#   rustfmt 下是乾淨的 ⇒ 這一條只攔 fork 改動。⚠ rebase 後若紅在上游原碼上，先核是不是
#   rustfmt 版本的排法差異，別順手把整份上游重排進 fork 的改動面。
LINT_CMD="$LINT_CMD && cargo fmt --manifest-path vendor/pingora/Cargo.toml --all -- --check"
LINT_CMD="$LINT_CMD && cargo clippy --workspace --all-targets --locked -- -D warnings"
# ★ ★ shell 那半邊**掃哪些檔案是推匯出來的，不是一張手寫清單**。
#
#   原文在這裡寫死了 18 個 `tests/<x>/*.sh`。⚠ **清單漏一項時沒有任何東西會說** ——
#   它只是安靜地少掃一個目錄，而 lint 照常全綠。實測漏的是 `tests/quic-relay/`，
#   那一格底下壓著 4 條真實告警，從來沒人看見過。
#   （更早還栽過一次同形狀的：tests/m1/lib.sh 帶著一處 SC2045 躺進來。）
#   ⚠ **不要順手把它說成「本來能攔住那處子 shell 缺陷」—— 實測攔不住**，
#     理由與實測記錄寫在 tests/ci/shellcheck-all.sh 頂部。
#
#   ⇒ 照本檔案對 `*_ONLY` 的同一條路子（`only_mode_in` / `selftest_only_mode`）改成
#     **結構性判據 + 每次自證**：判據在 tests/ci/shellcheck-all.sh，它自己拿一棵
#     答案已知的假樹釘住枚舉器沒瞎（含兩層深、含空格的路徑）。
#   ★ 順帶收掉一條舊的手動紀律：`tests/musl/` 那份探針跑在門禁外，但它**照樣被 lint 看**
#     —— 現在這是推導的結果，不再需要誰記得。
# ★ `LC_ALL=C.UTF-8` 那條（中文註解會讓 shellcheck 在 POSIX locale 下報不出話來）
#   搬進了那個指令碼里，誰來調都逃不掉。
LINT_CMD="$LINT_CMD && bash tests/ci/shellcheck-all.sh"
# ★ ★ 「`MSYS_NO_PATHCONV=1` 生效那片區域裡，git 的 argv 不許出現路徑」。
#   2026-09-08 實付一次：本檔案下面那句 `export` 漏給了它調起的子指令碼，
#   於是 `arm64-trigger.sh` 裡的 `git -C "$_root"` 拿到未轉換的 `/d/...` 而失敗，
#   代價是一份 `verified_commit=unknown` 的驗證記錄。
#   ⚠ 本檔案頂部**早就寫下過**這條位置硬要求，⛔ 卻只有行尾檢查那一個消費者照做了
#   ⇒ 這道門存在的意義是：這件事從此不再依賴誰記得。判據與自證在那個指令碼里。
# ⛔ 它只管 `git`。`docker` 正相反 —— 那句 export 本來就是為它加的。
LINT_CMD="$LINT_CMD && bash tests/ci/msys-argv-guard.sh"
# ★ ★ CI 那段搬運程式碼的自證（G94）。**掛在 lint 這一格而不是新開一個場景**：
#   它只花毫秒、不需要 docker、也不需要網路，而且它驗的是**門自己的管道**
#   （結束代碼是怎麼取的），與各場景驗的產品行為不是一回事。
# ⚠ 它是那條修法在本儲存庫唯一的判據 —— `dump-cache.sh` 本體只在
#   `cache-hit != 'true'` 時才在 CI 上跑，而快取鍵不認原始碼 ⇒ 往後基本不會再跑。
LINT_CMD="$LINT_CMD && bash tests/ci/dump-cache.sh --self-check"
# ★ ★ ACME 那套**失敗現場取證**的判據（`tests/acme/lib.sh` 的 `acme_dump_ports` 一族）。
#   **掛在 lint 這一格而不是 ACME 那一格**，理由與上面那條同源：取證程式碼只在
#   「已經要紅」的路徑上執行 ⇒ 一趟綠的 ACME 場景**從來不碰它**，
#   於是它壞了要等到真出事那天才發現，而那一次現場也就跟著白丟了。
#   ⚠ 它不碰 docker、不要產品二進位、只花幾百毫秒（自己開一個監聽 socket 當靶子）。
LINT_CMD="$LINT_CMD && bash tests/acme/self-check.sh"
# ★ `dep-check.py --apply` 判「兩把鎖對不對得上」的那段邏輯（2026-09-24 加）。
#   掛在 lint 這一格，理由同上兩條：`--apply` 一年跑不了幾次，而它的判定一旦與 vendor 迴歸網 [2/5]
#   分家，只會在那一次才露出來 —— 2026-09-24 就是：它比 [2/5] 更嚴，把三個合法的包誤報成對不上。
#   ⚠ 不出網、不跑 cargo，只喂合成輸入；python3 用的是建置鏡像裡那一份。
LINT_CMD="$LINT_CMD && python3 tools/dep-check.py --self-check"
# ★ pre-push 門「被推的提交碰沒碰 `.sh`」那條判據（G158）。掛在 lint 這一格，理由同上：
#   pre-push 路徑為了快**有意不跑**它，而判據一旦壞成「恆答不必跑」，那道門就安靜地
#   不再看 shell —— 只能由完整門禁替它守著。⚠ 只建一棵合成 git 儲存庫，不碰 docker、不出網。
LINT_CMD="$LINT_CMD && bash tests/ci/pre-push.sh --self-check"

if [ "${VENDOR_ONLY:-0}" = "1" ]; then
  # 只跑 fork 迴歸網。它不依賴 spike 二進位，所以連建置都跳過。
  CMD='bash tests/vendor/run.sh'
elif [ "${UNIT_ONLY:-0}" = "1" ]; then
  CMD='bash tests/unit/run.sh'
elif [ "${COMPILE_ONLY:-0}" = "1" ]; then
  # ★ **`shellcheck` + 編譯（含全部測試目標）**，⛔ 一條測試都不跑 ——
  #   `tests/ci/pre-push.sh` 用的就是這一格，⇒ 改這裡就是在改那道門的承諾。
  #   它答的是「這棵樹**靜態**上站得住嗎」，而那正是 2026-09-04 那次**被推出去**的缺陷問的話：
  #   測試裡引用一個還沒定義的常數，三處 `E0599`，整個 test crate 編譯不過。
  #
  # ⚠ ⚠ **`--all-targets` 是承重的**：那個缺陷就在**測試目標**裡，而 `BUILD_ONLY` 那條
  #   `cargo build --release` 壓根不編測試目標 ⇒ 它逮不住 —— 兩格不是同一件事。
  #
  # ★ ★ ★ **`shellcheck` 是 2026-09-05 加進來的（owner 拍板），起因是一次實測**：
  #   那天改了 `vol-lock.sh` 與 `docker-run.sh` 兩個 `.sh`，跑了**四趟** `COMPILE_ONLY`
  #   全綠 —— 而這一格當時**不含 `LINT_CMD`**（它是 `CMD='…'` 直接替換，不是 `CMD="$CMD && …"`
  #   追加）⇒ **那四個綠對那次改動沒有任何判別力**，直到單獨補跑了一次 shellcheck 才知道。
  #   ⚠ 推論更要緊：`pre-push` 門走的正是這一格 ⇒ **一次純 shell 的改動此前可以完整穿過它**。
  #   ★ 一個字元之差決定 lint 那一整格在不在：讀 `*_ONLY` 這串 `elif` 時先看
  #     它是 `CMD="$CMD && …"` 還是 `CMD='…'`。
  #
  # ⚠ **只掛 `shellcheck`，⛔ 不掛 `fmt` / `clippy`**，這是權衡不是遺漏：
  #   `clippy` 要先把依賴編出來（含 BoringSSL），那會把這道門從秒級拖成分鐘級，
  #   而**門變慢正是它被 `--no-verify` 繞過的起點**。`shellcheck` 唯讀原始碼、
  #   在本已起著的同一個容器裡跑，⇒ 邊際成本是這一格里最便宜的那一檔。
  #   ⚠ 代價寫在明處：**這一格仍然不攔 `fmt` 差異與 clippy 告警**，那兩格歸完整門禁。
  #
  # ★ **有意不跑測試**：本機常年 20+ 工作階段並行，按空載餘量設的超時會週期性假紅，
  #   而一道假紅過幾次的 pre-push 門會被 `--no-verify` 永久繞過。
  #   ⇒ 這一格里兩件事**都不會因機器負載而紅**（編譯不會，`shellcheck` 也不會）。
  #   ⚠ 代價寫在明處：**它不攔「編得過但測試紅」** —— 那一格歸完整門禁。
  #
  # ⚠ `spikes/musl-boringssl` 在根 workspace 之外（同 `cargo fmt` 那條的理由），本格夠不到它。
  #
  # ★ ★ `shellcheck` 放在**前面**：它幾秒就出結果，而編譯要十幾秒起 ⇒ shell 寫錯時立刻說。
  # ⚠ ⚠ 那對花括號**不是風格**：見本檔案下方那段「`&&` 與 `||` 同優先級左結合」的實測假綠。
  #   本格今天沒有 `||`，但把回落綁死在鏈上是這裡的既定紀律，⛔ 別順手拆掉。
  #
  # ★ ★ 2026-10-06（G158）：`COMPILE_SHELLCHECK=0` 時本格只編譯。pre-push 門按被推的提交
  #   判這個值（碰到 `*.sh` / `*shellcheckrc` / `docker/Dockerfile.build` 才是 1）——
  #   起因是實測 shellcheck 一項 15–33s，是這道門裡最慢的，而多數推送一個 `.sh` 都不碰。
  #   判據與自測在 `tests/ci/pre-push.sh`；預設（不給這個變數）照舊跑 shellcheck。
  if [ "${COMPILE_SHELLCHECK:-1}" = "0" ]; then
    CMD='{ cargo test --no-run --workspace --all-targets; }'
  else
    CMD='{ bash tests/ci/shellcheck-all.sh && cargo test --no-run --workspace --all-targets; }'
  fi
elif [ "${SERVE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/serve/run.sh"
elif [ "${L4_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/l4/run.sh"
elif [ "${FILES_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/files/run.sh"
elif [ "${CACHE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/cache/run.sh"
elif [ "${CACHEDISK_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/cache/disk.sh"
elif [ "${ENCODE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/encode/run.sh"
elif [ "${H3_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/h3/run.sh"
elif [ "${PP_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/proxyproto/run.sh"
elif [ "${LOG_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/log/run.sh"
elif [ "${METRICS_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/metrics/run.sh"
elif [ "${STATS_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/stats/run.sh"
elif [ "${RELAY_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/quic-relay/run.sh"
elif [ "${ACME_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/acme/run.sh"
elif [ "${RENEW_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/acme/renew.sh"
elif [ "${LINT_ONLY:-0}" = "1" ]; then
  CMD="$LINT_CMD"
elif [ "${UNCLAIMED_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/m0/unclaimed.sh"
elif [ "${SMOKE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/smoke/self-check.sh"
elif [ "${STRESS_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/stress/run.sh"
elif [ "${BENCH_ONLY:-0}" = "1" ]; then
  # ★ ★ **有意什麼都不追加** —— `CMD` 保持預設（`cargo build --release`）。
  #   對拍那一格跑在**另一張鏡像**裡（`docker/Dockerfile.bench`，裝著三家競品），
  #   掛不進下面那次 `docker run`；它由本檔案末尾那段宿主機側的分支調起，
  #   而它要的正是這次建置產出的 `target/release/fulcrum`。
  # ⚠ ⛔ 別把它寫成 `CMD="$CMD && bash tests/bench/run.sh"`：那會讓它在
  #   `fulcrum-build` 鏡像裡跑，而那張鏡像裡沒有 caddy / haproxy / nginx ——
  #   症狀是三家「起不來」，而真正的原因是跑錯了鏡像。
  :
elif [ "${BUILD_ONLY:-0}" != "1" ]; then
  # ── 順序的三條原則 ────────────────────────────────────────────────────
  #   ① 粒度細、跑得快的在前（lint → 單測 → fork 迴歸網），紅了指得最準；
  #   ② **驗產品的排在驗接縫的前面**（資料面 / L4 / 檔案 / 快取 / … 在 M0、M1 之前）——
  #      產品壞了先知道，免得對著一個接縫紀錄找一個其實在路由層的錯；
  #   ③ 依賴別的格成立才有意義的排在後面（QUIC 跨處理程序轉交要 h3 與紀錄都先成立）。
  [ "${LINT:-1}" = "0" ] || CMD="$LINT_CMD && $CMD"
  [ "${UNIT_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/unit/run.sh"
  [ "${VENDOR_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/vendor/run.sh"
  [ "${SERVE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/serve/run.sh"
  # L4：另一個入口（自建監聽器、沒有 Host、位元組原樣搬）。★ 獨有判據：換代時長連接不斷。
  [ "${L4_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/l4/run.sh"
  # 靜態檔案。★ 獨有判據：路徑穿越與 hide 清單 —— 壞掉時服務完全正常，只是多發了幾個檔案。
  [ "${FILES_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/files/run.sh"
  # 快取。★ 獨有判據：私有內容不許在兩個用戶端之間串號 —— 壞掉時沒有任何紀錄會說。
  [ "${CACHE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/cache/run.sh"
  # 快取磁碟後端。★ 獨有判據：**把處理程序殺掉再起來東西還在** ——
  #   記憶體後端不可能通過它，而它是「從記憶體來的」與「從磁碟來的」之間唯一不靠自報家門的判據。
  [ "${CACHEDISK_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/cache/disk.sh"
  # 壓縮：與快取耦合（G101 壓完再存）⇒ 排在快取兩格之後。★ 獨有判據：
  #   次級鍵寫錯就是把 gzip 的位元組發給不認 gzip 的用戶端，而兩邊的頭都完全正常。
  [ "${ENCODE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/encode/run.sh"
  # HTTP/3：同一條執行鏈換成 QUIC 出去。★ 獨有判據：逐跳頭在 h3 上禁止（RFC 9114 §4.2）·
  #   `Alt-Svc` 要出現在 h1/h2 的每一條回應上（G110）。
  # ★ 用戶端是 curl 的 OpenSSL-QUIC 棧，與被測的 quiche 沒有一行共同程式碼
  #   ⇒ 這是本儲存庫對 h3 唯一的**互操作**判據。
  [ "${H3_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/h3/run.sh"
  # PROXY protocol（HTTP 面）：連接開頭那幾十個位元組，在 TLS 之前、在 HTTP 之前。★ 獨有判據：
  #   ① PROXY 頭與請求在同一個 TCP 段裡到達（丟掉回還那一步的實作只在合並行送時才壞）；
  #   ② 沒配 `proxy_protocol_from` 的執行個體一個位元組都不讀 —— 少了它，任何用戶端都能自稱任意 IP。
  [ "${PP_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/proxyproto/run.sh"
  # 結構化訪問紀錄：回應之外的那一行。★ 獨有判據：① 一條請求正好一行；
  #   ② `uri` 是 `rewrite` **之前**那個（取改寫後的也編得過、也「有值」，
  #      只是它說出的是一個用戶端從沒請求過的位址）；③ 紀錄路徑打不開 ⇒ 裝載時就紅。
  [ "${LOG_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/log/run.sh"
  # Prometheus 指標：回應之外的那份**計數**。★ 排在訪問紀錄**之後**不是風格 ——
  #   它的一致性門拿訪問紀錄當對照物（同一個 `Record::finish` 喂出來的兩側），
  #   紀錄那一格先成立，這一格紅了才指得準。★ 獨有判據：
  #   ① 沒寫 `metrics` 的站台上同一路徑**不是**指標（否則「抓到了」證明不了是這條指令乾的）；
  #   ② 50 個未知 Host 之後 series 條數不增長**且**那一格正好 +50（只判前半的話，
  #      「計數器根本沒在加」也能過）；③ 兩個「site」在同一條請求上給出不同的值（G121 / R3）。
  [ "${METRICS_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/metrics/run.sh"
  # `GET /stats` 的**可選欄位與退化態**（M2 批 N 任務 7）。★ 有意排在 metrics 之後：
  #   「/stats 抓得到、與 /metrics 同源」由 serve 與 metrics 兩格守著，那兩格先成立，
  #   這一格紅了才指得準。★ 獨有判據：① `certs: []`（有解析器零證書）與 `certs: [一张]`
  #   是兩個不同的狀態——其餘兩格的執行個體形態固定，各自只走得到一邊；② `cache.entries`
  #   是活體讀數（快取一條之後它真的動），⛔ 不是「這個欄位在不在」；
  #   ③ `config_loaded_at_unix` 在一次 `/load` 之後真的變（今天沒有任何東西看它會不會動）。
  [ "${STATS_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/stats/run.sh"
  # 換代時的 QUIC 跨處理程序轉交。⚠ 唯一一格會在一次跑裡起兩代產品處理程序（SIGQUIT + `-u`）。
  [ "${RELAY_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/quic-relay/run.sh"
  # ACME：產品裡唯一一條「要有一個真的對端才算數」的路（G64 加 pebble 的全部理由）。
  [ "${ACME_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/acme/run.sh"
  # 續期是同一條路的另一半（G58「簽發**並續期**」）。⚠ 自帶 ~65s 等待（要等 ARI 視窗開），
  #   這是本條鏈裡唯一一處「非等不可」的時間，理由寫在 tests/acme/renew.sh 頂部。
  [ "${RENEW_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/acme/renew.sh"
  # ★ 冒煙與壓力**分工不同**：其餘場景驗「程式碼寫對了沒有」，這兩個驗「一臺跑著的樞衡
  #   現在是不是好的」。門禁裡對著自己起的本機執行個體跑，上線時換個目標 URL 對著真網域跑。
  [ "${SMOKE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/smoke/self-check.sh"
  [ "${STRESS_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/stress/run.sh"
  CMD="$CMD && bash tests/m0/run.sh"
  # 未認領 fd：驗的是**當前未修行為的重現**，而 M0 驗的是產品要保證的行為。
  CMD="$CMD && bash tests/m0/unclaimed.sh"
fi

# ★ 每次都把鏡像裡**實際**的工具鏈打出來。§8 要求效能對拍的環境可重現，
#   而可重現的第一步是看得見跑的到底是什麼——上面那條 bookworm/trixie 的教訓
#   之所以能悄悄躺三天，正是因為沒有任何一行輸出說過它是哪個基礎鏡像。
TOOLCHAIN='echo "── 构建镜像工具链 ──"; cat /etc/fulcrum-toolchain 2>/dev/null || echo "（无 /etc/fulcrum-toolchain：这个镜像是加钉子之前做的）"; echo'

# ── ★ 把 192.0.2.0/24 做成真丟包（G41）────────────────────────────────────
#
# 上游的連接超時測試用 `192.0.2.1:79`（RFC 5737 TEST-NET-1）當黑洞，設 1ms 超時
# 等 `ConnectTimedout`。**Docker 預設網路會替它應答**——實測 1.7ms 就 CONNECTED，
# 於是超時永遠等不到，6 條測試恆紅（其中一條此前被登記成「環境性失敗」，根因沒查）。
#
# ★ 裝上這條 DROP 之後實測：359 passed / 2 failed，已知失敗名單 3 → 2 條。
#   **把環境修對，比把名單加長強**——名單越長，門越鈍。
#
# ⚠ 需要 NET_ADMIN。這個容器本來就把儲存庫讀寫掛進來、以 root 跑，多這一個能力
#   不改變信任邊界；換來的是一道真能紅的門。
# ⚠ 裝不上要**當場紅**，不許繼續跑——「沒能檢查」當成「檢查通過」是本專案栽過的形狀。
#   真正的判據在 tests/vendor/run.sh 的自證步驟裡（那裡連 `-j DROP` 有沒有生效都驗）。
BLACKHOLE_CMD='iptables -A OUTPUT -d 192.0.2.0/24 -j DROP'

DOCKER_USER_ARGS=()
[ -z "${DOCKER_USER:-}" ] || DOCKER_USER_ARGS=(--user "$DOCKER_USER")

# ── 把 SMOKE_* / STRESS_* 這些旋鈕**透進容器**───────────────────────────────
#
# ★ ★ 批 15 補的，而它是被一次反證逼出來的：
#   `tests/stress/run.sh` 頂上寫著一排 `STRESS_DURATION` / `STRESS_MAX_ERRORS` …
#   的用法說明，而**從宿主機這條路進來時它們一個都到不了容器裡** ——
#   於是那些旋鈕擰了等於沒擰，而指令碼照樣跑、照樣報綠。
#   ⚠ 這與「DSL 裡認得、卻沒有任何人讀」的那類缺陷是同一個形狀：
#     **一個寫在文件裡、卻接不上的開關，比沒有這個開關更糟。**
#   （發現它的是一次反證：`STRESS_MAX_FD_GROWTH=-1` 本該讓門當場紅，結果照樣綠。）
#
# ★ 按**前綴**轉發，不寫死名單：寫死的名單在加第 N 個旋鈕時沒人會想起來去改，
#   於是它會安靜地少轉發一個。`docker run -e VAR`（不帶 =值）取的就是宿主機當前的值。
PASS_ENV_ARGS=()
while IFS='=' read -r k _; do
  case "$k" in
    SMOKE_* | STRESS_*) PASS_ENV_ARGS+=(-e "$k") ;;
  esac
done < <(env)

# -- docs/ bundle 門（宿主機，進容器之前）------------------------------------
#
# ★ 排在最前、且不進容器：它是純文字檢查，幾十毫秒，紅了指得最準 ——
#   而 Docker 存在的理由是 Linux 特有的 fd 移交與 systemd，跟這道門沒有關係。
# ⚠ 鏡像裡**是有** `/usr/bin/python3` 的（基礎鏡像帶的，並已在 Dockerfile.build 裡
#   顯式宣告）—— 這道門留在宿主機上不是因為容器裡跑不了它。
#
# ★★ 它攔的是「新增文件沒進 index.md」—— 那類文件對從入口讀目錄的人
#   等於不存在，而在這道門出現之前沒有任何一場看文件。
#
# ★ 它**不是一個場景**，是前置檢查——所以各 *_ONLY 模式下照跑（幾十毫秒，且紅了
#   指得最準，與 lint 排最前是同一個理由）。真不想跑就 DOCS_GATE=0。
#
# ⚠ ⚠ **路徑必須用 `$REPO_HOST` 而不是 `$REPO_UNIX`**，而這不是風格問題：
#   本指令碼前面已經 `export MSYS_NO_PATHCONV=1`（那是為了讓 docker 收到未轉換的路徑），
#   於是在 Windows + Git Bash 上，原生 `python.exe` 拿到的會是字面的 `/d/Workspace/…`，
#   它按當前盤解析成 `D:\d\Workspace\…` —— 報的是「找不到檔案」，
#   而那個路徑看起來又很像對的，排查方向會整個跑偏。
#   ★ `REPO_HOST` 是上面用 `cygpath -m` 轉過的那一份；**在 Linux 上它與 `REPO_UNIX` 逐字相同**。
#   ⚠ 它與本檔案開頭那條「`MSYS_NO_PATHCONV=1` 一旦生效，`git rev-parse` 會失敗」
#     是**同一個根因**。
# ★ 同一個開關下的第二道：`PLAN.md` §11 的**引用門**（`tools/plan-refs.py`）。
#   它答的是另一個問題 —— ⛔ 不許把兩道混著說：
#     · `docs-check.py` 答「新增的文件到不到得了」（bundle 的結構）；
#     · `plan-refs.py` 答「有沒有哪一處聲稱某個 D 號還在 §11 待定清單裡，而它已經不在了」。
#   ⚠ 它自己寫明瞭它**答不了**什麼（不帶 `§11` 字樣的散文、跨行的句子、語義錯），
#     ⛔ 別把它讀成「D 號引用全都有門守著」。
#   ★ 立它的直接原因：2026-09-03 一次體檢**成批**讀到這樣的假話，
#     而在它出現之前，本儲存庫沒有任何門看得見一句過期的註解。
#   ⛔ **這裡有意不寫那批有幾處**：一個寫在註解裡的計數沒有任何門守著，錯了不會紅 ——
#     ⚠ 而這一句的初稿正是這麼壞的（寫下的數與實測對不上）。**要數就跑這道門。**
if [ "${DOCS_GATE:-1}" = "1" ]; then
  python3 "$REPO_HOST/tools/docs-check.py"
  python3 "$REPO_HOST/tools/plan-refs.py"
fi

# ── ★ ★ ★ 認 `RWF_NOWAIT` 的那個夾具根（2026-09-06）────────────────────────
#
# `crates/fulcrum-server/src/files/probe.rs` 的快路徑（`preadv2(RWF_NOWAIT)` 整讀、
# 零跨執行緒）**能不能生效由檔案系統決定**：實測 ext4 認，**overlayfs 與 tmpfs 回
# `EOPNOTSUPP`**。而這個容器裡 `std::env::temp_dir()` 就在**鏡像可寫層**上，即
# overlayfs ⇒ 在這一行出現之前，那族判據裡「認 ⇒ 必須命中快路徑」那一支
# **除空檔案那一格外一次都走不到**。
#
# ⚠ ⚠ ★ **代價是實測出來的，⛔ 不是推理**：把 `read_full_nowait` 改成
#   「非空檔案一律回 `false`」（＝快路徑對每一個真實檔案都失效），
#   整趟 `UNIT_ONLY` **RC=0、881 條全綠，連那條契約測試自己都是 `ok`**。
#   ⇒ 一個把本次改動整個廢掉的迴歸，當時沒有任何東西會說。
#
# ★ ★ **必須是匿名卷（`-v /路径`，沒有源），⛔ 不許換成命名卷**，兩條理由：
#   ① 上面那道「說不出屬主的卷」判據按名字判，而匿名卷那 64 位十六進制名字
#      被 `fulcrum_vol_attribution` 認成 `anonymous` ⇒ 不會被點名；
#      一個命名的診斷卷 2026-09-06 當場被它挑出來過（`bench/docker-run.sh` 挑
#      匿名卷是同一個理由）。
#   ② `--rm` 會**連它一起刪** ⇒ 不跨趟留殘留、不用管回收。
# ⚠ 路徑逐字進 docker，靠的是上面那句 `export MSYS_NO_PATHCONV=1`；
#   ⛔ 別把它挪到那句之前，否則 Git Bash 會把它改寫成 `D:/Program Files/Git/...`。
#
# ⛔ **這裡不斷言它落在 ext4 上。** 承重的性質是「本趟認與不認兩種都在場」，
#   而那條斷言在 Rust 那邊（`每趟必须两个方向都真的走到`）—— 它對 xfs 之類
#   同樣認 `RWF_NOWAIT` 的檔案系統照樣成立，⛔ 不寫死檔案系統名字。
FS_FIXTURE=/fulcrum-fs-fixture

# ── 讓容器裡的 git 肯回答「/w 是不是工作樹」（2026-09-08）──────────────────
#
# ⚠ ⚠ ★ **這三個環境變數修的是一次三天、十一趟的 CI 連紅。**
#   `tests/ci/shellcheck-all.sh` 的普查要問 `git ls-files`，而在 **CI** 上
#   git 會拒絕：容器以 **root** 跑（除非設了 `DOCKER_USER`），而 runner 的檢出
#   屬主是另一個 uid ⇒ `fatal: detected dubious ownership in repository at '/w'`。
#   ★ 本機不會（Docker Desktop 的 bind mount 屬主不衝突）⇒ **「本機綠」對這件事
#   沒有判別力**，這也是它三天沒被發現的原因之一。
#
# ★ 用 `GIT_CONFIG_*` 而不是往容器裡寫 `~/.gitconfig`：**不落任何檔案**、
#   作用域就是這一個容器、且在這裡一處可見。
#   ⛔ 它**不削弱**那道普查 —— 普查照樣真跑；它只是讓 git 肯開口。
# ⚠ `actions/checkout` 加的 `safe.directory` 記的是 **runner 宿主**上的路徑
#   （`/home/runner/work/...`），而容器裡儲存庫掛在 `/w` ⇒ 那條**對不上**，
#   ⛔ 別指望它順帶管住這裡。
docker run --rm \
  "${DOCKER_USER_ARGS[@]}" \
  "${PASS_ENV_ARGS[@]}" \
  --cap-add=NET_ADMIN \
  -v "${REPO_HOST}:/w" \
  -v fulcrum-cargo:/usr/local/cargo/registry \
  -v "${TARGET_VOL}:/w/target" \
  -v "$FS_FIXTURE" \
  -w /w \
  -e RUST_LOG="${RUST_LOG:-info}" \
  -e FULCRUM_TEST_FS_ROOTS="$FS_FIXTURE" \
  -e GIT_CONFIG_COUNT=1 \
  -e GIT_CONFIG_KEY_0=safe.directory \
  -e GIT_CONFIG_VALUE_0=/w \
  "$IMAGE" \
  bash -c "$TOOLCHAIN; $BLACKHOLE_CMD; $CMD"

# ── 第二十格：產品的 musl 靜態產物（D22，owner 拍板）────────────
#
# ★ ★ 它與上面那次 `docker run` 是**兩回事**：這一格自己 `docker build` 一個
#   **Alpine** 鏡像把產品編成 musl 靜態產物，再塞進 `FROM scratch` 裡跑一次
#   `fulcrum validate`。⇒ 跑在宿主機側，掛不進上面那個容器 —— 與 M1 那四格同理。
#
# ⚠ ⚠ **它守的是 G13 的散布口徑（「Linux 單靜態二進位」），而在它之前那句話沒有任何門。**
#   D22 原本登記的是「把 `tests/musl/probe.sh` 掛成常設的門」，而 owner
#   拍板換掉了判據本身：**那份探針編的是 spike，答不了「產物是不是單靜態二進位」**
#   （它自己的驗證記錄第 5 節第 1 條就寫著這句話）。⇒ 探針**留在門外**當歷史記錄。
#
# ★ 實測：冷編一趟 **6m49s**；加了 BuildKit 快取掛載之後，
#   原始碼沒變的重跑 **7.7s**。⇒ 它掛得起每一次門禁。
if [ "${MUSL_TESTS:-1}" = "1" ] && ! env | only_mode_in; then
  # ── aarch64 那一格什麼時候跑：**D24 = 候選 ②**（owner 2026-09-06 拍板）────────
  #
  #   只在 `Cargo.lock` 或那三張釘 rustc 的 Dockerfile 變化時才連 aarch64 一起跑；
  #   其餘時候讀 `tests/musl/arm64-verified.txt` 那份**驗證記錄**。
  #
  # ★ 判據與觸發集都在 `tests/musl/arm64-trigger.sh`，這裡只做編排。
  # ⚠ 走**子處理程序**而不是 `source`：那個檔案的自測會定義 `want_*` 這幾個名字，
  #   source 進來會和本指令碼里的同名函式打架，而那種打架**不回報錯誤**，
  #   只是讓某一邊的斷言悄悄換了一個實作。
  # ★ 自測排在最前，理由與 `bench/run.sh` 那條逐字相同：判據自己壞了的時候，
  #   它最典型的形態就是**恆回傳同一個答案**，而那種壞法在下面這個 case 裡
  #   長得和「一切正常」一模一樣。
  # ★ 決定本身是 `arm64-trigger.sh` 裡的一個**純函式**（三條分支各有合成輸入釘著，
  #   含那條在裝了 binfmt 的機器上平時執行不到的紅路徑）⇒ 本處只按它的答案分派，
  #   ⛔ 這裡不再有第二份判斷邏輯。
  bash "$REPO_UNIX/tests/musl/arm64-trigger.sh" --self-check
  ARM64_STAMP="$REPO_UNIX/tests/musl/arm64-verified.txt"
  ARM64_DECISION=$(bash "$REPO_UNIX/tests/musl/arm64-trigger.sh" --decide "$ARM64_STAMP")

  case "$ARM64_DECISION" in
    amd64-only)
      echo "[docker-run] aarch64：这个依赖状态已经被真的验证过（tests/musl/arm64-verified.txt）"
      echo "[docker-run]            ⇒ 本趟只跑 amd64（D24 = 候选 ②）"
      bash "$REPO_UNIX/tests/musl/product.sh"
      ;;
    both)
      echo "[docker-run] aarch64：Cargo.lock 或那三张钉 rustc 的 Dockerfile 变了 ⇒ 本趟连 aarch64 一起跑。"
      echo "[docker-run]            ⚠ 冷建以十分钟计（实测 ShanirePCX 34m05s / ShanireHomePC 18m07s）。"
      ARCHES="amd64 arm64" bash "$REPO_UNIX/tests/musl/product.sh"
      # ★ 走到這裡說明上面那趟**真的成功了**（`set -e`：失敗根本到不了這一行）
      #   ⇒ 這時候寫驗證記錄才是如實的。
      bash "$REPO_UNIX/tests/musl/arm64-trigger.sh" --record "$ARM64_STAMP"
      echo "[docker-run] ★ 已刷新 tests/musl/arm64-verified.txt —— 它是一份**验证记录**，"
      echo "[docker-run]   ⚠ 记得随本次改动一起提交，否则下一趟会再跑一次这十几分钟。"
      ;;
    *)
      # ⚠ ⚠ ⛔ **「沒能檢查」不算「檢查通過」** —— 這裡有意判紅而不是靜默跳過。
      #   本儲存庫栽過同一物種：`348 passed / 0 failed`，而整個執行環境根本沒載入起來。
      echo "MUSL FAILED: Cargo.lock 或那三张钉 rustc 的 Dockerfile 变了，而这台机器" >&2
      echo "  跑不了 aarch64 ⇒ G13 承诺的第二个架构在这一趟里**没有任何判据**。" >&2
      echo "  （判据答的是 '$ARM64_DECISION'）两条路，选一条：" >&2
      echo "    ① 在本机装 qemu binfmt，然后重跑：" >&2
      echo "         docker run --privileged --rm tonistiigi/binfmt --install arm64" >&2
      echo "       ⚠ 特权 + 机器级改动 ⇒ 由 owner 定；装完 docker buildx inspect 会列出 linux/arm64。" >&2
      echo "    ② 在一台已经装了 binfmt 的机器上跑一次完整门禁，把刷新后的" >&2
      echo "       tests/musl/arm64-verified.txt 一起提交过来。" >&2
      echo "  ⛔ 别用「本机跑不了」当豁免 —— 那正是这道门存在的那一天要拦的东西。" >&2
      exit 1
      ;;
  esac
fi

# ── M1 場景 ────────────────────────────────────────────────────────────────
#
# ★ 它跑在**另一個容器**裡（systemd 當 PID 1），所以掛不進上面那次 docker run。
#   驅動在 tests/m1/systemd-run.sh，這裡只負責把它接進**唯一那條命令**——
#   一個「要另外記得跑」的場景，與不存在的場景沒有區別。
#
# ⚠ 上面那行不能寫成 `exec docker run` —— 那樣本段永遠執行不到；
#   `set -e` 保證前面紅了就不會走到這裡。
# ★ 「是不是某個 `*_ONLY` 模式」由 `only_mode_in` 結構性地判（定義與自測在本檔案開頭）。
if [ "${M1_TESTS:-1}" = "1" ] && ! env | only_mode_in; then
  # M1_SKIP_BUILD=1：產物剛剛才在上面建置過，不必再走一遍。
  M1_SKIP_BUILD=1 bash "$REPO_UNIX/tests/m1/systemd-run.sh"
fi

# ── 對拍那一格（M3 第一刀，G132）────────────────────────────────────────────
#
# ★ 它跑在**第三張鏡像**裡（`docker/Dockerfile.bench`：樞衡 + Caddy + HAProxy +
#   nginx + oha 同處一室），所以和 musl、M1 一樣掛不進上面那次 `docker run`。
#   它要上面那次建置產出的 `target/release/fulcrum`，⇒ 排在它之後。
#
# ⛔ **它不產出、也不許產出任何效能數字**（G132）。它判的是三件事：釘死的版本與
#   `bench/README.md` 那張表對得上 · 兩張鏡像裡的 oha 是同一個 · 那條流水線跑得通
#   **而判據兩個方向都判得動**。⇒ 它綠不等於對拍達標，那是 M3 退出條件的事。
#
# ★ ★ 掛進常設門禁的理由與 musl 那一格逐字相同：**一個「要另外記得跑」的場景，
#   與不存在的場景沒有區別** —— 而這一格守著的恰恰是「合格宿主那天跑一遍就能出數」
#   這句承諾，它一旦悄悄爛掉，要到那天才發現。
#
# ⚠ `BENCH_ONLY=1` 時上面那次 `docker run` 只做建置（見 `*_ONLY` 那串 elif），
#   所以這裡不能用 `! env | only_mode_in` 一刀切 —— 那樣 `BENCH_ONLY` 會把自己跳過。
if [ "${BENCH_TESTS:-1}" = "1" ] &&
  { [ "${BENCH_ONLY:-0}" = "1" ] || ! env | only_mode_in; }; then
  bash "$REPO_UNIX/tests/bench/run.sh"
fi
