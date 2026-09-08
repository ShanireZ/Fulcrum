#!/usr/bin/env bash
# 门：`MSYS_NO_PATHCONV=1` 生效的那片区域里，**git 的 argv 里不许出现路径**。
#
# ## 它拦的是什么（2026-09-08 实付一次，代价是一份说不出出处的验证记录）
#
#   `tests/m0/docker-run.sh` 为了让 docker 收到未转换的容器路径，`export MSYS_NO_PATHCONV=1`。
#   ⚠ 那是 **export** ⇒ 此后由它调起／source 进来的一切都继承。
#   而 MSYS 一旦停止翻译 argv，原生 `git.exe` 收到的就是字面量 `/d/Workspace/Fulcrum`：
#
#       fatal: cannot change to '/d/Workspace/Fulcrum': No such file or directory   (rc=128)
#
#   `61b867a` 那份 `tests/musl/arm64-verified.txt` 里的 `verified_commit=unknown` 就是这么来的。
#
# ★ ★ ★ **本仓早就写下过这条**（`tests/m0/docker-run.sh` 顶部那段「位置是硬要求，不是风格」），
#   ⚠ 然而只有**一个**消费者被排到 export 前面，其余三处 git 调用留在后面没人回来扫。
#   ⇒ 这道门存在的理由就是那句自己写下的话：
#     「修完一个形状要当场把同形的全扫一遍，靠人眼扫是扫不干净的，得有工具。」
#
# ## 判据形状：**默认拒绝**，⛔ 不维护一张「禁止写法」清单
#
#   一张手写的禁止清单，漏一项就静默放行，而且没人会发现漏了（本仓在别处栽过）。
#   ⇒ 这里反过来：**区域内 git 的 argv 里只要出现「路径形状」的东西就判红** ——
#     `/` 开头的字面量、`$VAR` / `"$VAR"` / `"${VAR}"`、以及 `-C` / `--git-dir` / `--work-tree`
#     这三个**按定义就吃路径**的选项。⇒ 将来有人发明新写法，失败方向是**新增即受禁**。
#
#   ⛔ **重定向不是 argv**：`git hash-object --stdin < "$f"` 是对的（重定向由 bash 做），
#     所以扫描到 `<` / `>` 就停止取 argv。这条**有专门的自测钉着**。
#   ⛔ 本门只管 `git`。`docker` 正相反 —— 它**需要**未转换的路径，那正是那句 export 的目的。
#
# ## 区域怎么来：**推导**，⛔ 不手写清单
#
#   ① `tests/m0/docker-run.sh` 里 `export MSYS_NO_PATHCONV=1` 那一行**之后**的部分；
#   ② 该文件里以 `$REPO_UNIX/….sh` 形式引用到的**每个**脚本，整份都算
#      （调起的、source 的都算 —— source 进来的函数体是在同一个 shell 里跑的，
#       它写在 export 之前不代表它**执行**在 export 之前）。
#   ⚠ **用相对路径调起的那些有意不算**（`bash tests/ci/…`、`bash tests/serve/…`）：
#     它们跑在**容器里**（`$CMD` / `$LINT_CMD` 是喂给 `docker run` 的），那儿是 Linux、
#     没有 MSYS，这个危害原理上不存在。⇒ 区分的判据是「宿主侧还是容器侧」，
#     而 `$REPO_UNIX/` 这个前缀恰好就是宿主侧路径的标记。
#     ⛔ 别把它读成「相对路径比较安全」——它只是跑在另一个世界里。
#   ⚠ ② 是**有意放宽**的：宁可多管，也不要因为「这个脚本只在 export 前跑」这种
#     需要逐个论证的判断而漏掉。多管的代价是写法受限，漏掉的代价是又一份 `unknown`。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DRUN="$REPO/tests/m0/docker-run.sh"

fail_n=0
say_bad() { printf '  ✗ %s\n' "$*" >&2; fail_n=$((fail_n + 1)); }

# ── 取 export 那一行的行号 ───────────────────────────────────────────────────
# ⚠ 找不到就**判红**，⛔ 不是「那就没什么可查的」——本门的前提没了，
#   而一道前提消失后仍然报绿的门，比没有这道门更坏。
export_line_of() {
  grep -n '^[[:space:]]*export[[:space:]]\+MSYS_NO_PATHCONV=1' "$1" | head -1 | cut -d: -f1
}

# ── 从一行里取出 git 的 argv（遇到重定向/管道/分隔符就停）────────────────────
# 打出该行里每个 git 调用的参数，一行一个 token。
# ⚠ ⚠ ★ **必须跟踪引号状态**：`echo "… && git rev-parse …"` 里那个 git 是**字符串正文**，
#   不是一次调用。第一版没跟踪，于是本门上线第一次真扫就误伤了自己的报错文案。
#   ⛔ 不能改成「先剥掉所有引号内容」——那会把 `git -C "$REPO_UNIX"` 的 `"$REPO_UNIX"`
#   一起剥掉，正好把**最该拦的那一种**变成瞎的。
#   ⇒ 起作用的机制是：**只在引号外按空白切 token** ⇒ 一整个带引号的字符串是**一个** token，
#     里面的 `git` 永远不会以命令词的身份单独出现；而 `git -C "$X"` 的 `"$X"` 是引号外
#     起头的独立 token ⇒ 照样看得见。★ 这条机制由 `selftest` 的最后两行阴性样本钉着，
#     ⛔ 别以为「多加一个 token 起始引号状态的判断」还能再防一层 —— 实测那是**死代码**
#     （撤掉它自测一条都不变），而一条看起来在防守、实际恒真的判断比没有更坏。
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

scan_file() {                                   # $1=文件 $2=起始行（1=整份）
  local f=$1 start=${2:-1} rel=${1#"$REPO"/}
  local tmp line_no=0
  tmp=$(mktemp)
  awk -v s="$start" 'NR>=s' "$f" > "$tmp"
  # 逐行扫，报出行号（换算回原文件）
  while IFS= read -r line; do
    line_no=$((line_no + 1))
    case "$line" in *git*) ;; *) continue ;; esac
    printf '%s\n' "$line" > "$tmp.one"
    # ⚠ **一行最多报一条**。同一处写法常同时命中两条理由（`git -C "$X"` 既有 `-C`
    #   又有 `$X`）⇒ 逐 token 报会让「几处问题」和「几条理由」混成一个数，
    #   而自测正是按**处数**对的。★ 这是自测当场逮出来的，⛔ 不是事后想到的。
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

# ── 自测：先证明这把刀砍得动、也砍不错 ──────────────────────────────────────
#
# ★ 顺序与本仓其它几道门一致：**判据先自证，再去判真东西**。
#   一把恒返回「没问题」的刀，和一次真的干净扫描，输出一模一样。
selftest() {
  local d rc=0
  d=$(mktemp -d)

  # ① 阳性：该拦的三种写法，一条都不许漏
  cat > "$d/bad.sh" <<'SH'
git -C "$REPO_UNIX" rev-parse --short HEAD
git hash-object "$DOCKERFILE"
git --git-dir=/w/.git log -1
SH
  # ② 阴性：该放的写法，一条都不许误伤。
  #    ★ 最后两行是**回归钉**：字符串正文里的 git、注释里的 git 都**不是调用**。
  #      本门第一次真扫时正是误伤了自己的报错文案 ⇒ 这两行撤掉即红。
  cat > "$d/good.sh" <<'SH'
( cd "$_root" && git rev-parse --short HEAD )
git hash-object --stdin < "$DOCKERFILE"
printf '%s' "$x" | git hash-object --stdin
git -c core.quotePath=false ls-files --eol
echo "改法： git -C $d rev-parse HEAD 是不许的"
# git -C "$X" rev-parse HEAD
SH
  # ③ 重定向不是 argv：目标是 `/` 开头的字面量也不许判红
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

  # ④ 区域推导：export **之前**的 git -C 不该被管，**之后**的必须被管。
  #    ★ 这一条两个方向都钉，⛔ 否则「区域算错成整份」和「区域算对」输出一样。
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
  # ⚠ 这里断的是**行号**，⛔ 不是报文里出现哪个变量名 —— `-C` 那条理由先命中，
  #   报文里根本不会有 `AFTER` 字样。★ 第一版断错了对象，被这条自测自己逮住。
  if ! grep -q 'drun\.sh:3:' "$d/d.err" || grep -q 'drun\.sh:1:' "$d/d.err"; then
    echo "★ 自测未过：区域盖错了行（该报第 3 行、不该报第 1 行）" >&2; cat "$d/d.err" >&2; rc=1
  fi

  fail_n=$saved
  rm -rf "$d"
  [ "$rc" = 0 ] || return 1
  echo "  ✓ msys-argv-guard 自测通过（阳性 3 条 · 阴性不误伤 · 重定向不算 argv · 区域两个方向）"
}

selftest || { echo "MSYS ARGV GUARD FAILED: 判据自己没过自测" >&2; exit 1; }

# ── 真扫 ────────────────────────────────────────────────────────────────────
EXPORT_LINE=$(export_line_of "$DRUN" || true)
if [ -z "${EXPORT_LINE:-}" ]; then
  echo "MSYS ARGV GUARD FAILED: 在 $DRUN 里找不到 \`export MSYS_NO_PATHCONV=1\`。" >&2
  echo "  ⛔ 「前提没了」不算「没问题」——要么它被挪走了（那本门要跟着改），" >&2
  echo "  要么它被删了（那 docker 那些调用会开始收到被转换的路径）。两种都要人来看。" >&2
  exit 1
fi

# 区域②：docker-run.sh 里引用到的每个 $REPO_UNIX/….sh
# shellcheck disable=SC2016  # ★ 这里要的**正是字面量** `$REPO_UNIX` —— 我们在源码文本里
#   找它，⛔ 不是要展开它（展开了反而什么都找不到）。
mapfile -t REFED < <(grep -oE '\$REPO_UNIX/[A-Za-z0-9_./-]+\.sh' "$DRUN" | sed 's|^\$REPO_UNIX/||' | sort -u)

echo "[msys-argv-guard] 区域：docker-run.sh 第 $((EXPORT_LINE + 1)) 行起 + 它引用的 ${#REFED[@]} 个脚本（推导，⛔ 非手写清单）"

# ⚠ 区域必须真的包含那个**出过事**的脚本；算空了要判红，⛔ 不许「没扫到就算过」。
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
