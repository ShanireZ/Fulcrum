#!/usr/bin/env bash
# 對 tests/ 下**每一個** `.sh` 跑靜態檢查 —— lint 那一格的 shell 那半邊。
#
# ★ ★ ★ **為什麼它存在**：這件事原來是 `docker-run.sh` 的 `LINT_CMD` 上一張
#   **手寫的目錄清單**（`tests/acme/*.sh` … `tests/vendor/*.sh`，共 18 項）。
#   ⚠ **清單漏一項時沒有任何東西會說** —— 它只是安靜地少掃一個目錄，而 lint 照常全綠。
#
#   實測漏掉的是 `tests/quic-relay/`，而那**恰好**是本儲存庫栽過一次安靜 shell 缺陷的那一格：
#   `GEN2=$(start_gen …)` 裡 `$(…)` 是子 shell ⇒ `PIDS+=` 改的是副本、`cleanup` 一個處理程序
#   都沒收到，**而該場景照常報 PASSED**；洩漏的處理程序攥著合成出來的 `:80` 活到下一個場景，
#   讓 CI 間歇性紅了三個月（PLAN.md §9）。
#
# ⚠ ⚠ ★ **但別把這道門當成「本來能攔住那個缺陷」的東西 —— 實測它攔不住。**
#   0.10 上的 SC2030/SC2031 只認**寫在子 shell 那幾行裡**的賦值
#   （`( B=1 )` 與 `X=$(D=9; …)` 都報），而當年那處是「函式體裡 `PIDS+=`、呼叫方 `$(f)`」——
#   逐字重現過，一聲不吭。（順帶：管道右側 `while read` 裡改變數，0.10 也不報。）
#   ⇒ 這次補的是**覆蓋面**：那一格從來沒被掃過，底下壓著 4 條真實告警。
#     真正攔住那個缺陷的是場景收尾時那條「用過的連接埠要還回去」的自證，不是這道門。
#   ★ 這條要寫在明處 —— 一把沒量過就被當成能量的尺子，比沒有尺子更貴。
#
# ⚠ 順帶一條本檔案自己踩到過兩次的：註解行以「# 空格 shellcheck」開頭會被當成**指令**
#   解析（SC1073）—— 寫這個詞的時候別讓它落在行首。
#
# ★ ★ 改法照 `docker-run.sh` 對 `*_ONLY` 的那條路子（`only_mode_in` / `selftest_only_mode`）：
#   **不列舉，改問一句結構性的問題**（「這棵樹下有哪些 `.sh`」），**並且每次執行都自證**。
#   ⇒ 下一個人加場景時不需要記得改這裡；而「問句本身瞎了」有判據看著。
#
# ★ ★ ★ **2026-09-05：加了第二棵樹 `bench/`，並加了一道「有沒有第三棵樹」的普查。**
#   起因是 M3 第一刀新建了頂層 `bench/`，裡面五個 `.sh` —— 而本檔案當時只掃 `tests`
#   ⇒ **一整個目錄的 shell 從出生起就沒被靜態看過**，與它當初要修的那個缺陷
#   （手寫清單漏掉 `tests/quic-relay/`）**是同一個形狀，只是換了一層**。
#   ⚠ ⚠ 只把 `bench` 加進清單是不夠的：那還是一張清單，下一個頂層目錄照樣會漏。
#   ⇒ 照 §3.1 那條教訓辦 —— **普查那一步的篩子必須比門禁的寬**：
#     門禁掃 `$ROOTS`，而 `selftest_no_orphans` 用一個**更寬**的篩子（全倉，
#     只排除 target/vendor/.git）去問「有沒有 `.sh` 落在 `$ROOTS` 之外」，有就紅。
#   ★ 於是「加了新目錄忘了改這裡」從**靜默漏掃**變成**當場紅**。
#
# 用法：
#   bash tests/ci/shellcheck-all.sh          # 掃 <仓库根>/tests 與 <仓库根>/bench，並跑普查
#   bash tests/ci/shellcheck-all.sh <目录>   # 只掃指定的那棵樹（做反證時用），⛔ 不跑普查
#
# 結束代碼：0 = 全過；非 0 = 靜態檢查有話說，或枚舉器自測 / 普查沒過。
set -euo pipefail

# ★ `LC_ALL=C.UTF-8` 不是可選項，而且要設在**這裡**而不是呼叫方：本儲存庫的指令碼註解是中文，
#   而 shellcheck 在 POSIX locale 下會在**輸出報告時**炸掉
#   （`commitBuffer: invalid argument (cannot encode character …)`），結束代碼 2 ——
#   那樣它紅得像是發現了問題，實際是它自己說不出話，排查方向會整個跑偏。
#   ⇒ 設在指令碼裡，誰來調都逃不掉這一條。
export LC_ALL=C.UTF-8

# ★ 先回到儲存庫根再用**相對**路徑掃：報告裡的路徑要能直接粘進編輯器，
#   而 `/w/tests/…` 只在容器裡成立。
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO"
# ★ 給了參數就只掃那一棵（反證用）；沒給就掃儲存庫自己那兩棵，並跑普查。
if [ "$#" -ge 1 ]; then
  ROOTS=("$1")
  ORPHAN_CHECK=0
else
  ROOTS=(tests bench)
  ORPHAN_CHECK=1
fi

# ── 枚舉器：結構性判據 ──────────────────────────────────────────────────────
#
# ★ 用 `find` 而不是 glob，兩條理由都不是風格：
#   ① `tests/**/*.sh` 在**非交互 bash 裡沒有 globstar**，`**` 退化成 `*` ⇒ 它逐字等價於
#      `tests/*/*.sh`，**兩層以下的指令碼一個都不匹配**，而寫著 `**` 的人以為覆蓋到了。
#      （實測：容器裡這兩個模式展開出來的檔案**逐個相同** —— 因為本儲存庫的指令碼眼下正好
#       都只有一層深。⇒ 寫 `**` 的人今天看不出差別，明天放一個兩層深的指令碼才會。）
#   ② glob 匹配不到東西時 bash 把**模式本身**當參數傳下去 —— 交給 find 壓根不產生這問題。
# ★ 全程 NUL 分隔（`-print0` / `read -d ''`）：含空格或換行的路徑按行分隔會被劈成
#   兩個不存在的路徑，與 `docker-run.sh` 裡那條行尾掃描是同一件事。
sh_files_under() {
  find "$1" -type f -name '*.sh' -print0 | sort -z
}

# ★ ★ **它必須自證沒瞎，每次都證** —— 一個恆回傳空的枚舉器會讓這道門永遠綠，而
#   「一個字都沒掃」與「全掃過且都沒問題」在結束代碼上長得一模一樣。
#   ⇒ 拿一棵**答案已知**的假樹問它，逐字比對（同 `selftest_only_mode` 的形狀）。
# ⚠ 逐字比對是有意的：它一次釘住兩個方向 —— 少認一個紅，多認一個也紅。
selftest_sh_files() {
  local d f got want
  d=$(mktemp -d)
  # `dir.sh` 是個**目錄**：判據是「檔案」，不是「名字像指令碼」。
  mkdir -p "$d/one" "$d/two/deep" "$d/a b" "$d/one/dir.sh"
  : > "$d/one/run.sh"
  : > "$d/two/deep/nested.sh"   # ★ 兩層深 —— 一層的 glob 看不見它，這正是最該釘的一條
  : > "$d/a b/spaced.sh"        # ★ 路徑帶空格 —— 按行分隔會把它劈成兩半
  : > "$d/one/notes.md"         # 不是 .sh
  : > "$d/one/run.sh.bak"       # 不**以** .sh 結尾

  got=""
  while IFS= read -r -d '' f; do
    got="$got|${f#"$d"/}"
  done < <(sh_files_under "$d")
  rm -rf "$d"

  want='|a b/spaced.sh|one/run.sh|two/deep/nested.sh'
  if [ "$got" = "$want" ]; then
    return 0
  fi
  echo "★ 枚举器自测未通过——**本次静态检查的覆盖面一律不可信**。" >&2
  echo "    期望：$want" >&2
  echo "    实得：$got" >&2
  echo "  多半是判据被改窄了（限了深度、换回 glob），或 sort 的顺序变了。" >&2
  return 1
}
selftest_sh_files

# ── 普查：有沒有 `.sh` 落在 `$ROOTS` 之外 ───────────────────────────────────
#
# ★ ★ ★ **這個篩子必須比門禁的寬**，否則它只會把門禁的盲區原樣複述一遍，
#   還看起來像做過一次獨立普查（本儲存庫 2026-09-05 §3.1 那條教訓的原話）。
#   ⇒ 它問的是「**這個儲存庫擁有的** `.sh` 有沒有落在根之外」。
#
# ★ ★ 用 `git ls-files` 而不是 `find`，**這是判據本身而不是實作口味**：
#   `find` 掃的是**工作樹** ⇒ `handoff/`（gitignored、本機的、按 G115 不入庫）裡
#   隨手放一個 `.sh` 就會誤報，而它給出的修法（「把那個頂層目錄加進 ROOTS」）
#   對 `handoff/` 恰恰是**錯的建議**。換個問法之後，`target/`、`.git/` 以及
#   一切 gitignored 的東西自動落在判據之外，排除表從三項縮成一項。
# ⚠ 剩下那一項是 `vendor/`：它**是**被跟蹤的，但它是 fork 進來的上游程式碼，
#   不歸本儲存庫的 lint 管（今天它下面一個 `.sh` 都沒有 ⇒ 這條排除是為將來寫的）。
#   ⛔ 別再往這張表裡加東西 —— 每加一項就是一次消音。
# ⚠ ⚠ ★ **`--cached --others --exclude-standard` 三個都是承重的。**
#   光 `--cached` 只看**已跟蹤**的 ⇒ 一個剛寫出來、還沒 `git add` 的指令碼會整個溜過去，
#   而那恰恰是本場那五個 `bench/*.sh` 當時的狀態 —— 判據在最該說話的那一刻沉默。
#   加上 `--others --exclude-standard` 之後它問的是「**已經在儲存庫裡、或正在進儲存庫的**」，
#   而被 `.gitignore` 擋掉的（`handoff/`、`target/`）仍然落在判據之外。
orphan_sh_files() {
  git ls-files -z --cached --others --exclude-standard -- '*.sh'
}

selftest_no_orphans() {
  local f rel root hit orphans="" n=0 owned=0 git_err git_rc=0

  # ★ ★ 「沒能檢查」不算「檢查通過」：問不出來就紅，⛔ 不靜默跳過。
  #
  # ⚠ ⚠ ★ **訊息必須說出是「哪一種」問不出來** —— 這一條是 2026-09-08 用
  #   **三天、十一趟 CI 連紅**換來的。原先這裡寫的是
  #       `git rev-parse --is-inside-work-tree >/dev/null 2>&1`
  #   ⇒ **git 的原話被 `2>&1` 連同 stdout 一起吞掉**，只留下一句
  #   「不是 git 工作樹，或者沒有 git」。而在 CI 上**那兩條都不成立**：
  #   git 裝著（2.47.3）、`.git` 也隨 bind mount 進來了 —— git 只是**拒絕回答**：
  #       `fatal: detected dubious ownership in repository at '/w'`
  #   （容器以 root 跑，而 runner 的檢出屬主是另一個 uid）。
  #   ⇒ ★ 一個把三種死因合併成一句話的判據，**紅了三天而沒有人知道為什麼**。
  #   本機重現過（把檢出 `chown` 給 1001 再以 root 跑）：訊息與 CI 逐字相同。
  #
  # ⚠ ⛔ **捕獲失敗命令的輸出時不能讓 `set -e` 把指令碼掐掉** ——
  #   `x=$(失败的命令)` 在 `set -e` 下當場退出，而那正是判據最該說話的一刻。
  #   ⇒ 用 `|| git_rc=$?` 接住。
  if ! command -v git >/dev/null 2>&1; then
    echo "★ 普查跑不了：这个环境里**没有 git**——**「没能检查」不算「检查通过」**。" >&2
    return 1
  fi
  # `2>&1 >/dev/null` 的順序是承重的：先把 stderr 接到當前的 stdout（＝捕獲），
  # 再把 stdout 丟掉 ⇒ 只拿到 git 說的話。⛔ 反過來寫會連正常輸出一起收。
  git_err=$(git rev-parse --is-inside-work-tree 2>&1 >/dev/null) || git_rc=$?
  if [ "$git_rc" != 0 ]; then
    # ★ 措辭有意覆蓋兩種死因：「這裡不是儲存庫」與「是儲存庫但 git 不肯認」——
    #   ⛔ 本判據**不自己給死因分類**，真因由下面 git 的原話說，那比任何分類都準。
    echo "★ 普查跑不了：git **没有回答**「这是不是工作树」（退出码 $git_rc）——**「没能检查」不算「检查通过」**。" >&2
    echo "  ★ git 自己说的是（⛔ 别把这几行吞掉，它们才是真因）：" >&2
    # ⚠ 用 `sed` 逐行縮進，⛔ 不用 `printf '    %s\n'` —— 後者對多行只給第一行加前綴，
    #   而 git 這條訊息正好是多行的（第二、三行才寫著修法）。
    printf '%s\n' "$git_err" | sed 's/^/    /' >&2
    return 1
  fi

  while IFS= read -r -d '' f; do
    rel=$f
    case "$rel" in vendor/*) continue ;; esac
    owned=$((owned + 1))
    hit=0
    for root in "${ROOTS[@]}"; do
      case "$rel" in "$root"/*) hit=1; break ;; esac
    done
    [ "$hit" = 1 ] || { orphans="$orphans  $rel"$'\n'; n=$((n + 1)); }
  done < <(orphan_sh_files)

  # ★ ★ **「一個都沒數到」是紅，不是「普查通過」** —— 與下面那條
  #   「掃空了」同一條紀律：一個恆回傳空的普查，對每一棵樹都說「沒問題」。
  if [ "$owned" -eq 0 ]; then
    echo "★ 普查一个被跟踪的 .sh 都没数到 —— 这不是「没问题」，是**普查本身瞎了**。" >&2
    return 1
  fi

  if [ "$n" = 0 ]; then
    echo "[shellcheck] 普查：本仓的 $owned 个 .sh 全在 $ROOTS_LABEL 之内"
    return 0
  fi
  echo "★ 有 $n 个 .sh 落在被扫的树（$ROOTS_LABEL）之外 —— **它们从来没被静态检查看过**：" >&2
  printf '%s' "$orphans" >&2
  echo "  修法：把它所在的顶层目录加进本文件的 ROOTS，⛔ 别把它加进排除表。" >&2
  return 1
}

ROOTS_LABEL="${ROOTS[*]}"
if [ "$ORPHAN_CHECK" = 1 ]; then
  selftest_no_orphans
fi

FILES=()
for ROOT in "${ROOTS[@]}"; do
  if [ ! -d "$ROOT" ]; then
    echo "★ 要扫的树不在：$ROOT" >&2
    exit 1
  fi
  while IFS= read -r -d '' f; do
    FILES+=("$f")
  done < <(sh_files_under "$ROOT")
done

# ★ ★ **「一個都沒找到」是紅，不是「掃完了沒問題」。**
#   這是推導式清單唯一的失效方式，所以它要有自己的一句話。
# ⚠ 也正因為如此，**這裡不許寫成 `find … | xargs -r shellcheck`**：
#   `-r` 的語義就是「沒有輸入就一條都不跑」⇒ 掃空時結束代碼 0，安靜地全綠。
#   （不帶 `-r` 時 shellcheck 拿到零個檔案會報 `No files specified.` 退出 3，是紅的；
#    但那條回報錯誤說的是它自己的用法，不是「這棵樹裡沒有指令碼」。）
if [ "${#FILES[@]}" -eq 0 ]; then
  echo "★ $ROOTS_LABEL 下一个 .sh 都没找到 —— 这不是「扫完了」，是**扫空了**。" >&2
  exit 1
fi

# ★ 把覆蓋面**打出來**：一道說不出自己掃了什麼的門，綠的時候沒人判斷得了它掃得夠不夠。
#   這也是這次改動唯一讓人看得見「quic-relay 現在在裡面了」的地方。
DIRS=""
for f in "${FILES[@]}"; do
  d=${f%/*}
  case "|$DIRS|" in
    *"|$d|"*) ;;
    *) DIRS="$DIRS$d|" ;;
  esac
done
echo "[shellcheck] ${#FILES[@]} 个脚本 / $(printf '%s' "$DIRS" | tr -cd '|' | wc -c) 个目录：${DIRS//|/ }"

shellcheck "${FILES[@]}"
echo "  ✓ shellcheck：${#FILES[@]} 个脚本全过"
