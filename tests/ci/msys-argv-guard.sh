#!/usr/bin/env bash
# 門：`MSYS_NO_PATHCONV=1` 生效的那片區域裡，**git 的 argv 裡不許出現路徑**。
#
# ## 它攔的是什麼（2026-09-08 實付一次，代價是一份說不出出處的驗證記錄）
#
#   `tests/m0/docker-run.sh` 為了讓 docker 收到未轉換的容器路徑，`export MSYS_NO_PATHCONV=1`。
#   ⚠ 那是 **export** ⇒ 此後由它調起／source 進來的一切都繼承。
#   而 MSYS 一旦停止翻譯 argv，原生 `git.exe` 收到的就是字面值 `/d/Workspace/Fulcrum`：
#
#       fatal: cannot change to '/d/Workspace/Fulcrum': No such file or directory   (rc=128)
#
#   `61b867a` 那份 `tests/musl/arm64-verified.txt` 裡的 `verified_commit=unknown` 就是這麼來的。
#
# ★ ★ ★ **本儲存庫早就寫下過這條**（`tests/m0/docker-run.sh` 頂部那段「位置是硬要求，不是風格」），
#   ⚠ 然而只有**一個**消費者被排到 export 前面，其餘三處 git 呼叫留在後面沒人回來掃。
#   ⇒ 這道門存在的理由就是那句自己寫下的話：
#     「修完一個形狀要當場把同形的全掃一遍，靠人眼掃是掃不乾淨的，得有工具。」
#
# ## 判據形狀：**預設拒絕**，⛔ 不維護一張「禁止寫法」清單
#
#   一張手寫的禁止清單，漏一項就靜默放行，而且沒人會發現漏了（本儲存庫在別處栽過）。
#   ⇒ 這裡反過來：**區域內 git 的 argv 裡只要出現「路徑形狀」的東西就判紅** ——
#     `/` 開頭的字面值、`$VAR` / `"$VAR"` / `"${VAR}"`、以及 `-C` / `--git-dir` / `--work-tree`
#     這三個**按定義就吃路徑**的選項。⇒ 將來有人發明新寫法，失敗方向是**新增即受禁**。
#
#   ⛔ **重定向不是 argv**：`git hash-object --stdin < "$f"` 是對的（重定向由 bash 做），
#     所以掃描到 `<` / `>` 就停止取 argv。這條**有專門的自測釘著**。
#   ⛔ 本門只管 `git`。`docker` 正相反 —— 它**需要**未轉換的路徑，那正是那句 export 的目的。
#
# ## 區域怎麼來：**推導**，⛔ 不手寫清單
#
#   ① `tests/m0/docker-run.sh` 裡 `export MSYS_NO_PATHCONV=1` 那一行**之後**的部分；
#   ② 該檔案裡以 `$REPO_UNIX/….sh` 形式引用到的**每個**指令碼，整份都算
#      （調起的、source 的都算 —— source 進來的函式體是在同一個 shell 裡跑的，
#       它寫在 export 之前不代表它**執行**在 export 之前）。
#   ⚠ **用相對路徑調起的那些有意不算**（`bash tests/ci/…`、`bash tests/serve/…`）：
#     它們跑在**容器裡**（`$CMD` / `$LINT_CMD` 是餵給 `docker run` 的），那兒是 Linux、
#     沒有 MSYS，這個危害原理上不存在。⇒ 區分的判據是「宿主側還是容器側」，
#     而 `$REPO_UNIX/` 這個前綴恰好就是宿主側路徑的標記。
#     ⛔ 別把它讀成「相對路徑比較安全」——它只是跑在另一個世界裡。
#   ⚠ ② 是**有意放寬**的：寧可多管，也不要因為「這個指令碼只在 export 前跑」這種
#     需要逐個論證的判斷而漏掉。多管的代價是寫法受限，漏掉的代價是又一份 `unknown`。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DRUN="$REPO/tests/m0/docker-run.sh"

fail_n=0
say_bad() { printf '  ✗ %s\n' "$*" >&2; fail_n=$((fail_n + 1)); }

# ── 取 export 那一行的行號 ───────────────────────────────────────────────────
# ⚠ 找不到就**判紅**，⛔ 不是「那就沒什麼可查的」——本門的前提沒了，
#   而一道前提消失後仍然報綠的門，比沒有這道門更壞。
export_line_of() {
  grep -n '^[[:space:]]*export[[:space:]]\+MSYS_NO_PATHCONV=1' "$1" | head -1 | cut -d: -f1
}

# ── 從一行裡取出 git 的 argv（遇到重定向/管道/分隔符就停）────────────────────
# 打出該行裡每個 git 呼叫的參數，一行一個 token。
# ⚠ ⚠ ★ **必須跟蹤引號狀態**：`echo "… && git rev-parse …"` 裡那個 git 是**字串正文**，
#   不是一次呼叫。第一版沒跟蹤，於是本門上線第一次真掃就誤傷了自己的回報錯誤文案。
#   ⛔ 不能改成「先剝掉所有引號內容」——那會把 `git -C "$REPO_UNIX"` 的 `"$REPO_UNIX"`
#   一起剝掉，正好把**最該攔的那一種**變成瞎的。
#   ⇒ 起作用的機制是：**只在引號外按空白切 token** ⇒ 一整個帶引號的字串是**一個** token，
#     裡面的 `git` 永遠不會以命令詞的身分單獨出現；而 `git -C "$X"` 的 `"$X"` 是引號外
#     起頭的獨立 token ⇒ 照樣看得見。★ 這條機制由 `selftest` 的最後兩行陰性樣本釘著，
#     ⛔ 別以為「多加一個 token 起始引號狀態的判斷」還能再防一層 —— 實測那是**死程式碼**
#     （撤掉它自測一條都不變），而一條看起來在防守、實際恆真的判斷比沒有更壞。
git_argv_tokens() {
  awk '
    {
      n = length($0); q = 0; esc = 0; tok = ""; ingit = 0
      for (i = 1; i <= n + 1; i++) {
        c = (i <= n) ? substr($0, i, 1) : " "
        if (esc)                { tok = tok c; esc = 0; continue }
        if (c == "\\")          { tok = tok c; esc = 1; continue }
        if (q == 0 && c == "\"") { q = 2; tok = tok c; continue }
        if (q == 0 && c == "'"'"'")  { q = 1; tok = tok c; continue }
        if (q == 2 && c == "\"") { q = 0; tok = tok c; continue }
        if (q == 1 && c == "'"'"'")  { q = 0; tok = tok c; continue }
        if (q == 0 && c == "#" && tok == "") break        # 行尾注释
        if (q == 0 && (c == " " || c == "\t")) {
          if (tok != "") {
            if (ingit) {
              if (tok ~ /^[<>|;&)]/ || tok == "&&" || tok == "||") ingit = 0
              else print tok
            } else if (tok == "git" || tok ~ /\$\(git$/ || tok ~ /`git$/) ingit = 1
            tok = ""
          }
          continue
        }
        tok = tok c
      }
    }
  ' "$1"
}

scan_file() {                                   # $1=檔案 $2=起始行（1=整份）
  local f=$1 start=${2:-1} rel=${1#"$REPO"/}
  local tmp line_no=0
  tmp=$(mktemp)
  awk -v s="$start" 'NR>=s' "$f" > "$tmp"
  # 逐行掃，報出行號（換算回原檔案）
  while IFS= read -r line; do
    line_no=$((line_no + 1))
    case "$line" in *git*) ;; *) continue ;; esac
    printf '%s\n' "$line" > "$tmp.one"
    # ⚠ **一行最多報一條**。同一處寫法常同時命中兩條理由（`git -C "$X"` 既有 `-C`
    #   又有 `$X`）⇒ 逐 token 報會讓「幾處問題」和「幾條理由」混成一個數，
    #   而自測正是按**處數**對的。★ 這是自測當場逮出來的，⛔ 不是事後想到的。
    local tok reason=''
    while IFS= read -r tok; do
      [ -n "$tok" ] || continue
      case "$tok" in
        -C|--git-dir|--git-dir=*|--work-tree|--work-tree=*)
          reason="git 带 \`$tok\` —— 它按定义吃路径" ;;
        /*|'"/'*)
          reason="git 的 argv 里有 \`/\` 开头的字面量 \`$tok\`" ;;
        *'$'*)
          reason="git 的 argv 里有变量 \`$tok\`（可能是路径）" ;;
      esac
      [ -n "$reason" ] && break
    done < <(git_argv_tokens "$tmp.one")
    [ -n "$reason" ] && say_bad "$rel:$((start + line_no - 1)): $reason"
    rm -f "$tmp.one"
  done < "$tmp"
  rm -f "$tmp"
}

# ── 自測：先證明這把刀砍得動、也砍不錯 ──────────────────────────────────────
#
# ★ 順序與本儲存庫其它幾道門一致：**判據先自證，再去判真東西**。
#   一把恆回傳「沒問題」的刀，和一次真的乾淨掃描，輸出一模一樣。
selftest() {
  local d rc=0
  d=$(mktemp -d)

  # ① 陽性：該攔的三種寫法，一條都不許漏
  cat > "$d/bad.sh" <<'SH'
git -C "$REPO_UNIX" rev-parse --short HEAD
git hash-object "$DOCKERFILE"
git --git-dir=/w/.git log -1
SH
  # ② 陰性：該放的寫法，一條都不許誤傷。
  #    ★ 最後兩行是**迴歸釘**：字串正文裡的 git、註解裡的 git 都**不是呼叫**。
  #      本門第一次真掃時正是誤傷了自己的回報錯誤文案 ⇒ 這兩行撤掉即紅。
  cat > "$d/good.sh" <<'SH'
( cd "$_root" && git rev-parse --short HEAD )
git hash-object --stdin < "$DOCKERFILE"
printf '%s' "$x" | git hash-object --stdin
git -c core.quotePath=false ls-files --eol
echo "改法： git -C $d rev-parse HEAD 是不许的"
# git -C "$X" rev-parse HEAD
SH
  # ③ 重定向不是 argv：目標是 `/` 開頭的字面值也不許判紅
  cat > "$d/redir.sh" <<'SH'
git hash-object --stdin < /w/docker/Dockerfile.build
SH

  local saved=$fail_n
  fail_n=0; scan_file "$d/bad.sh" 1 2>/dev/null
  if [ "$fail_n" -ne 3 ]; then
    echo "★ 自测未过：阳性样本该报 3 条，实得 $fail_n —— **本次扫描一律不可信**" >&2; rc=1
  fi
  fail_n=0; scan_file "$d/good.sh" 1 2>"$d/g.err"
  if [ "$fail_n" -ne 0 ]; then
    echo "★ 自测未过：阴性样本被误伤 $fail_n 条 —— 刀太宽" >&2; cat "$d/g.err" >&2; rc=1
  fi
  fail_n=0; scan_file "$d/redir.sh" 1 2>"$d/r.err"
  if [ "$fail_n" -ne 0 ]; then
    echo "★ 自测未过：把**重定向**当成了 argv —— \`< /path\` 是 bash 做的，不经过 MSYS" >&2
    cat "$d/r.err" >&2; rc=1
  fi

  # ④ 區域推導：export **之前**的 git -C 不該被管，**之後**的必須被管。
  #    ★ 這一條兩個方向都釘，⛔ 否則「區域算錯成整份」和「區域算對」輸出一樣。
  cat > "$d/drun.sh" <<'SH'
git -C "$BEFORE" rev-parse HEAD
export MSYS_NO_PATHCONV=1
git -C "$AFTER" rev-parse HEAD
SH
  local el; el=$(export_line_of "$d/drun.sh")
  if [ "$el" != 2 ]; then
    echo "★ 自测未过：export 行号该是 2，实得 '$el'" >&2; rc=1
  fi
  fail_n=0; scan_file "$d/drun.sh" $((el + 1)) 2>"$d/d.err"
  if [ "$fail_n" -ne 1 ]; then
    echo "★ 自测未过：区域该只盖住 export 之后那一条，实得 $fail_n 条" >&2; cat "$d/d.err" >&2; rc=1
  fi
  # ⚠ 這裡斷的是**行號**，⛔ 不是訊息裡出現哪個變數名 —— `-C` 那條理由先命中，
  #   訊息裡根本不會有 `AFTER` 字樣。★ 第一版斷錯了物件，被這條自測自己逮住。
  if ! grep -q 'drun\.sh:3:' "$d/d.err" || grep -q 'drun\.sh:1:' "$d/d.err"; then
    echo "★ 自测未过：区域盖错了行（该报第 3 行、不该报第 1 行）" >&2; cat "$d/d.err" >&2; rc=1
  fi

  fail_n=$saved
  rm -rf "$d"
  [ "$rc" = 0 ] || return 1
  echo "  ✓ msys-argv-guard 自测通过（阳性 3 条 · 阴性不误伤 · 重定向不算 argv · 区域两个方向）"
}

selftest || { echo "MSYS ARGV GUARD FAILED: 判据自己没过自测" >&2; exit 1; }

# ── 真掃 ────────────────────────────────────────────────────────────────────
EXPORT_LINE=$(export_line_of "$DRUN" || true)
if [ -z "${EXPORT_LINE:-}" ]; then
  echo "MSYS ARGV GUARD FAILED: 在 $DRUN 里找不到 \`export MSYS_NO_PATHCONV=1\`。" >&2
  echo "  ⛔ 「前提没了」不算「没问题」——要么它被挪走了（那本门要跟着改），" >&2
  echo "  要么它被删了（那 docker 那些调用会开始收到被转换的路径）。两种都要人来看。" >&2
  exit 1
fi

# 區域②：docker-run.sh 裡引用到的每個 $REPO_UNIX/….sh
# shellcheck disable=SC2016  # ★ 這裡要的**正是字面值** `$REPO_UNIX` —— 我們在原始碼文字裡
#   找它，⛔ 不是要展開它（展開了反而什麼都找不到）。
mapfile -t REFED < <(grep -oE '\$REPO_UNIX/[A-Za-z0-9_./-]+\.sh' "$DRUN" | sed 's|^\$REPO_UNIX/||' | sort -u)

echo "[msys-argv-guard] 区域：docker-run.sh 第 $((EXPORT_LINE + 1)) 行起 + 它引用的 ${#REFED[@]} 个脚本（推导，⛔ 非手写清单）"

# ⚠ 區域必須真的包含那個**出過事**的指令碼；算空了要判紅，⛔ 不許「沒掃到就算過」。
case " ${REFED[*]} " in
  *" tests/musl/arm64-trigger.sh "*) ;;
  *) echo "MSYS ARGV GUARD FAILED: 区域推导没算到 tests/musl/arm64-trigger.sh —— 推导坏了" >&2; exit 1 ;;
esac

scan_file "$DRUN" $((EXPORT_LINE + 1))
for r in "${REFED[@]}"; do
  [ -f "$REPO/$r" ] || continue
  scan_file "$REPO/$r" 1
done

if [ "$fail_n" -ne 0 ]; then
  echo >&2
  echo "MSYS ARGV GUARD FAILED: 上面 $fail_n 处把路径放进了 git 的 argv。" >&2
  echo "  ⚠ 它们跑在 \`export MSYS_NO_PATHCONV=1\` 之后 ⇒ 原生 git.exe 会收到未转换的 \`/d/...\`，" >&2
  echo "    报 \`fatal: cannot change to '/d/…'\` 或 \`could not open '/d/…' for reading\`。" >&2
  echo "  ★ 改法（两种都不经过 argv 转换）：" >&2
  echo "      · 要在某个目录里问     ⇒  ( cd \"\$d\" && git … )      # cd 是 bash 内建" >&2
  echo "      · 要把某个文件喂给 git ⇒  git hash-object --stdin < \"\$f\"   # 重定向由 bash 做" >&2
  exit 1
fi

echo "  ✓ msys-argv-guard：区域内没有把路径放进 git argv 的写法"
