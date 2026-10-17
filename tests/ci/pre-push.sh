#!/usr/bin/env bash
# `pre-push` 門：**別讓一棵靜態上就站不住的樹離開這臺機器。**
#
# 起因（2026-09-04 實付代價）：`9c0634f` 是一次**手動保盤提交** —— 測試裡引用一個
# 還沒定義的常數，三處 `E0599`，整個 test crate 編譯不過，**而它已經被推上去了**。
# ⚠ ⚠ 它躲過了每一個便宜的讀數：`git log` 乾淨、`git status` 乾淨、兩道文件門全綠、
# `origin/main..main` 是 0 —— **全都說「一切正常」**。⇒ 唯一判得動它的是**真去編一次**。
#
# ## ★ 為什麼是 `pre-push` 而不是 `pre-commit`
#
# 那種半成品提交**本來就是要的**：它的用途是防資料丟失，攔它等於攔掉它的全部價值。
# 要攔的是**離開這臺機器**那一步。
#
# ## ★ 它跑 `shellcheck` + 編譯，⛔ 一條測試都不跑 —— 這是權衡，不是偷懶
#
# 本機常年跑著 20+ 個工作階段，並行實測比空載慢 3.75 倍 ⇒ 按空載餘量設的超時會**週期性
# 假紅**。而一道假紅過幾次的 hook 會被 `--no-verify` 永久繞過 —— 那時它連編譯都不看了。
# ⇒ 這兩件事答的正是那個缺陷問的那句話，且**都不可能因機器負載而紅**。
# ⚠ 代價寫在明處：**它不攔「編得過但測試紅」**。那一格歸完整門禁，本門不冒充它。
#
# ## ⚠ ⚠ 2026-09-05：`shellcheck` 是補上來的，而它此前**不在**這道門裡
#
# ⛔ **有意留下這段而不是把上面那句改乾淨**：它是這道門被觀察到的一次真實盲區。
# 那天改了 `tests/lib/vol-lock.sh` 與 `tests/m0/docker-run.sh` 兩個 `.sh`，
# 跑了**四趟** `COMPILE_ONLY` 全綠 —— 而 `COMPILE_ONLY` 那一格當時是
# `CMD='cargo test …'` **直接替換**，不是 `CMD="$CMD && …"` 追加 ⇒ 它不含 `LINT_CMD`。
# ★ ★ ★ 於是**一次純 shell 的改動可以完整地穿過這道門**，而那四個綠對它沒有任何判別力。
# ⇒ owner 拍板把 `shellcheck-all.sh` 掛進那一格（⛔ 不掛 `fmt`/`clippy`：clippy 要先編依賴，
#   會把這道門從秒級拖成分鐘級，而**門變慢正是它被 `--no-verify` 繞過的起點**）。
# ⚠ 仍然不攔的：`fmt` 差異、clippy 告警、以及所有測試。那三格歸完整門禁。
#
# ## ⚠ ⚠ 2026-10-06（G158）：`shellcheck` 改成**看被推的提交**再決定跑不跑
#
# 起因是一次實測：熱快取、乾淨工作樹，三趟 33–61s，而網路本身只佔約 2s。
# 最慢的一項正是 `shellcheck`（54 個指令碼一次全掃，15–33s），而它每一趟都跑，
# 不管被推的提交碰沒碰 `.sh`；容器外的宿主機自測與遺留卷報告又佔 13–20s
# （Git Bash 每次 fork 幾十毫秒，約 2000 條命令）。⇒ owner 拍板兩條：
#   ① 被推的提交**淨改動**碰到 `*.sh` · `*shellcheckrc` · `docker/Dockerfile.build`
#      （釘著 shellcheck 版本）之一才跑 shellcheck。判不出來的一律照跑：遠端還沒有這個引用、
#      遠端那一端本機沒有、`git diff` 本身失敗 —— 「沒能判斷」不算「不需要」。
#   ② 調 `docker-run.sh` 時關掉宿主機自測與遺留卷報告。它們驗的是門自己的機器與磁碟賬，
#      完整門禁每趟照跑。
# ⚠ 代價寫在明處：
#   · shellcheck 掃的仍是**工作樹**，跳不跳由被推的提交決定 ⇒ 被推的提交沒碰 `.sh`、
#     而工作樹裡躺著一個未提交的壞 `.sh` 時，本門放行（那個 `.sh` 不在這次推送裡）。
#   · 門自己的機器壞了（卷名推導、鎖、位元組探針）時，本門不再當場發現，要等下一趟完整門禁。
#     ⇒ **已由 G159 收窄**（同日）：宿主機自測改成按指紋跑 —— 門的指令碼（`docker-run.sh`、
#       `tests/lib/*.sh`）或宿主工具（bash / MSYS 執行環境 / Git for Windows）一變，本門當場照跑；
#       都沒變且本工作樹有同一指紋的通過記錄才跳過。判據與盲區寫在 `docker-run.sh` 那一段。
# ★ 判據釘在 `--self-check`（合成儲存庫，兩個方向都有），掛在完整門禁的 lint 那一格；
#   ⛔ 不在 pre-push 路徑上跑 —— 那正是本條要省的那一類開銷。
# ⛔ 三個開關**不由呼叫方給**：本指令碼按判據顯式賦值後傳下去，環境裡帶進來的同名變數蓋不過它們。
#
# ## ⚠ ⚠ 已知盲區：它量的是**工作樹**，不是被推的那幾筆
#
# `pre-push` 跑在工作樹上。工作樹幹淨時工作樹 ≡ `HEAD`，而 owner 批量推時正是這種情形
# （2026-09-04 那次也是：樹幹淨、HEAD 就是那筆壞的）⇒ 那時它守得住。
# **工作樹髒時它守不住**：未提交的改動可能正好補上了被推那一筆的窟窿，而本門看不見。
# ⇒ 髒樹時下面會**明說自己量的是什麼**，⛔ 不假裝它守住了被推的狀態。
#
# ## 裝它（⚠ hook 不在版本控制裡 —— 換機器 / 重新 clone 都要再跑一次）
#
#     printf '#!/usr/bin/env bash\nexec bash "$(git rev-parse --show-toplevel)/tests/ci/pre-push.sh" "$@"\n' \
#       > .git/hooks/pre-push && chmod +x .git/hooks/pre-push
#
# ⛔ **本指令碼不提供任何自己的繞過開關**：一個隨手能設的開關會把門變成建議
#    （與本儲存庫「不留逐行豁免記號」同一條紀律）。真要繞過只有 `git push --no-verify` ——
#    那是一次**顯式**的、看得見的動作。
set -euo pipefail

# ── 被推的這一段會不會改變 shellcheck 的結論（G158）──────────────────────────
#
# 回傳 0 = 要跑，stdout 說出理由（被碰到的路徑，或為什麼判不出來）；回傳 1 = 不必跑。
# ★ 比的是兩端的**樹**（`git diff <远端> <本地>`），⛔ 不是逐筆看提交：中間加了又刪掉的 `.sh`
#   不改變被推那一端的內容，而 shellcheck 只看內容。遠端不是祖先（強推）時這樣比照樣對。
# ★ 三類路徑都承重：`*.sh` 是被掃的物件；`*shellcheckrc` 改規則；`docker/Dockerfile.build`
#   釘著 shellcheck 的版本 —— 後兩者一個 `.sh` 都不碰，也能讓同一批指令碼的結論變紅。
SC_PATHSPEC=('*.sh' '*shellcheckrc' 'docker/Dockerfile.build')
shellcheck_trigger() {
  local remote_sha=$1 local_sha=$2 changed
  case "$remote_sha" in
    *[!0]*) ;;
    *) echo "远端还没有这个引用，没有可比的基线"; return 0 ;;
  esac
  if ! git cat-file -e "${remote_sha}^{commit}" 2>/dev/null; then
    echo "远端那一端（${remote_sha:0:12}）本地没有，比不了 —— fetch 之后才判得出来"
    return 0
  fi
  if ! changed=$(git diff --name-only --no-renames "$remote_sha" "$local_sha" -- "${SC_PATHSPEC[@]}"); then
    echo "git diff 没能比出结果"
    return 0
  fi
  [ -n "$changed" ] || return 1
  printf '%s\n' "$changed"
}

# ── `--self-check`：拿一棵答案已知的合成儲存庫釘住上面那條判據 ──────────────────
#
# ★ 兩個方向都要有：恆答「要跑」的壞判據會讓「不必跑」那幾條紅，恆答「不必跑」的
#   會讓「要跑」那幾條紅 —— 只有一個方向的自測，對另一種壞法零判別力。
# ★ 掛在完整門禁 lint 那一格（容器裡跑）；⛔ 不在 pre-push 路徑上跑。
selftest_shellcheck_trigger() (
  d=$(mktemp -d)
  trap 'rm -rf "$d"' EXIT
  cd "$d"
  export GIT_AUTHOR_NAME=selftest GIT_AUTHOR_EMAIL=selftest@invalid \
         GIT_COMMITTER_NAME=selftest GIT_COMMITTER_EMAIL=selftest@invalid
  git -c init.defaultBranch=main init -q .
  git config core.autocrlf false
  git config commit.gpgsign false
  snap() { git add -A && git commit -q -m "$1" && git rev-parse HEAD; }

  mkdir -p src tests/x docker docs
  printf 'a\n' > src/a.rs
  printf '#!/bin/sh\n' > tests/x/run.sh
  printf 'FROM x\n' > docker/Dockerfile.build
  base=$(snap base)
  printf 'b\n' > src/a.rs
  rs_only=$(snap rs-only)
  printf 'echo\n' >> tests/x/run.sh
  sh_mod=$(snap sh-mod)
  printf 'x\n' > docs/x.sh.md
  printf 'x\n' > tests/x/run.sh.bak
  lookalike=$(snap lookalike)
  git rm -q tests/x/run.sh
  sh_del=$(snap sh-del)
  printf 'FROM y\n' > docker/Dockerfile.build
  dockerfile=$(snap dockerfile)
  printf 'disable=SC2086\n' > .shellcheckrc
  rcfile=$(snap rcfile)
  mkdir -p tests/y/deep
  printf '#!/bin/sh\n' > tests/y/deep/n.sh
  added=$(snap added)
  git rm -q tests/y/deep/n.sh
  net_zero=$(snap net-zero)
  zero=$(printf '%040d' 0)
  unknown=$(printf 'de%038d' 0)   # 一個本機沒有的 sha

  n=0
  rc=0
  # expect <yes|no> <说明> <远端> <本地> [理由裡必須整行出現的路徑]
  expect() {
    local want=$1 label=$2 out got
    n=$((n + 1))
    if out=$(shellcheck_trigger "$3" "$4" </dev/null); then got=yes; else got=no; fi
    if [ "$got" != "$want" ]; then
      echo "★ $label：期望 $want，实得 $got（${out:-无输出}）" >&2
      rc=1
      return 0
    fi
    if [ -n "${5:-}" ] && ! printf '%s\n' "$out" | grep -qxF -- "$5"; then
      echo "★ $label：判对了，但理由里没点名 $5（实得：$out）" >&2
      rc=1
    fi
  }
  expect no  "只改 .rs"                            "$base"       "$rs_only"
  expect yes "改了一个 .sh"                        "$rs_only"    "$sh_mod"     tests/x/run.sh
  expect yes "多笔里有一笔改了 .sh"                "$base"       "$sh_mod"     tests/x/run.sh
  expect no  "名字里带 .sh 但不以它结尾"           "$sh_mod"     "$lookalike"
  expect yes "删掉一个 .sh"                        "$lookalike"  "$sh_del"     tests/x/run.sh
  expect yes "改了钉 shellcheck 版本的 Dockerfile" "$sh_del"     "$dockerfile" docker/Dockerfile.build
  expect yes "加了 .shellcheckrc"                  "$dockerfile" "$rcfile"     .shellcheckrc
  expect yes "两层深的新 .sh"                      "$rcfile"     "$added"      tests/y/deep/n.sh
  expect no  "加了又删掉（净改动为零）"            "$rcfile"     "$net_zero"
  expect yes "远端还没有这个引用"                  "$zero"       "$net_zero"
  expect yes "远端那一端本地没有"                  "$unknown"    "$net_zero"
  if [ "$rc" != 0 ]; then
    echo "  ⇒ 判据自测未通过 —— pre-push 门「跳不跳 shellcheck」一律不可信。" >&2
    exit 1
  fi
  echo "[pre-push --self-check] ✓ $n 条全过（「要跑」与「不必跑」两个方向都有）"
)

if [ "${1:-}" = "--self-check" ]; then
  selftest_shellcheck_trigger
  exit 0
fi

REPO="$(git rev-parse --show-toplevel)"

# ── 這一次到底有沒有程式碼往外送 ─────────────────────────────────────────────
#
# `pre-push` 從 stdin 收若干行 `<local_ref> <local_sha> <remote_ref> <remote_sha>`。
# ⚠ 刪除遠端分支那種，`local_sha` **全是 0**：那一次一個位元組的程式碼都沒往外送，編它沒意義。
# ⇒ 只有**全部**都是刪除（或一行都沒有）才跳過；只要有一行是真推送就照編。
# ★ 判「全 0」不比長度：sha1 是 40 位、sha256 是 64 位，寫死長度的寫法會在換演算法那天
#   **靜默地把刪除當成推送**（多編一次，不致命）或反過來 —— 用「有沒有非 0 字元」判。
# ★ 同一趟順手按 `shellcheck_trigger` 判跑不跑 shellcheck：任何一行要跑，這一趟就跑。
HAS_CONTENT=0
RUN_SC=0
SC_WHY=""
while read -r local_ref local_sha _remote_ref remote_sha; do
  case "$local_sha" in
    *[!0]*) HAS_CONTENT=1 ;;
    *) continue ;;
  esac
  if why=$(shellcheck_trigger "$remote_sha" "$local_sha" </dev/null); then
    RUN_SC=1
    SC_WHY+="    ${local_ref}："$'\n'"$(printf '%s\n' "$why" | sed 's/^/      /')"$'\n'
  fi
done
if [ "$HAS_CONTENT" = 0 ]; then
  echo "[pre-push] 这一次只有删除、没有代码往外送 —— 跳过本门。"
  exit 0
fi

# ── 說清楚這一趟量的是什麼 ─────────────────────────────────────────────────
if [ -n "$(git status --porcelain)" ]; then
  echo "⚠ 工作树不干净 ⇒ **本次编译与 shellcheck 量的是工作树，不是你正在推的那几笔。**" >&2
  echo "  未提交的改动可能正好补上了被推那一笔的窟窿，而这道门看不见这件事。" >&2
  echo "  （跑不跑 shellcheck 倒是按被推的提交判的。）要它守得住被推的状态，就在工作树干净时推。" >&2
fi

# ⚠ ⚠ 這幾行**必須單引號**：雙引號裡的反引號是**命令替換** —— 寫成雙引號的話，
#   bash 會真的去跑那條 cargo 命令，而 Rust 在這臺宿主上不許跑（G107）。
#   ★ 寫這個檔案時當場踩了一次；`shellcheck` 的 SC2006 也守得住，但別指望它備援。
echo '[pre-push] 只编译、不跑测试（cargo test --no-run --workspace --all-targets）；被推的提交动了 .sh 才先跑 shellcheck（G158）——'
echo '           判据是一句话：**静态上就站不住的树不许离开这台机器**。'
if [ "$RUN_SC" = 1 ]; then
  echo "[pre-push] 本次跑 shellcheck —— 被推的提交碰到了它判得到的东西："
  printf '%s' "$SC_WHY"
else
  echo "[pre-push] 本次跳过 shellcheck —— 被推的提交没碰 *.sh / *shellcheckrc / docker/Dockerfile.build。"
fi
echo '[pre-push] 宿主机自测按指纹决定跑不跑（HOST_SELFTESTS=auto）；遗留卷报告留给完整门禁（VOL_REPORT=0）。'

# ⚠ 走的是 `docker-run.sh` 的 `COMPILE_ONLY` 那一格，⛔ 不自己拼 `docker run`：
#   建置鏡像、target 卷名、那把「同一棵樹只許跑一次」的鎖、行尾位元組探針、兩道文件門 ——
#   全部只有一份推導（`tests/lib/vol-lock.sh` + `docker-run.sh`）。
#   ★ 各寫一遍的失效形態是**安靜地指向另一個卷**，而那時門測的是別人家的讀數。
# ★ 三個開關在這裡**顯式賦值**，環境裡帶進來的同名變數蓋不過它們（G158 / G159）。
if COMPILE_ONLY=1 COMPILE_SHELLCHECK="$RUN_SC" HOST_SELFTESTS=auto VOL_REPORT=0 \
     bash "$REPO/tests/m0/docker-run.sh"; then
  if [ "$RUN_SC" = 1 ]; then
    echo "[pre-push] ✓ shellcheck 与编译（含全部测试目标）都过了 —— 放行。"
  else
    echo "[pre-push] ✓ 编译（含全部测试目标）过了 —— 放行（本次没跑 shellcheck，理由见上）。"
  fi
  exit 0
fi

echo "" >&2
echo "⛔ **push 已中止**：这棵树 shellcheck 没过、编译不过，或者这道门根本没能跑起来。" >&2
echo "   ★ 「没能检查」不算「检查通过」——docker 起不来、或本树上另有一次门禁正在跑" >&2
echo "     （那把锁不排队，它会点名持锁的 pid），都会走到这里。上面那段输出说的就是原因。" >&2
echo '   ⚠ 修好再推。⛔ 别顺手 --no-verify：这道门此刻拦下的，正是它被加出来的那个东西。' >&2
exit 1
