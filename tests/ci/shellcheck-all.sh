#!/usr/bin/env bash
# 对 tests/ 下**每一个** `.sh` 跑静态检查 —— lint 那一格的 shell 那半边。
#
# ★ ★ ★ **为什么它存在**：这件事原来是 `docker-run.sh` 的 `LINT_CMD` 上一张
#   **手写的目录清单**（`tests/acme/*.sh` … `tests/vendor/*.sh`，共 18 项）。
#   ⚠ **清单漏一项时没有任何东西会说** —— 它只是安静地少扫一个目录，而 lint 照常全绿。
#
#   实测漏掉的是 `tests/quic-relay/`，而那**恰好**是本仓库栽过一次安静 shell 缺陷的那一格：
#   `GEN2=$(start_gen …)` 里 `$(…)` 是子 shell ⇒ `PIDS+=` 改的是副本、`cleanup` 一个进程
#   都没收到，**而该场景照常报 PASSED**；泄漏的进程攥着合成出来的 `:80` 活到下一个场景，
#   让 CI 间歇性红了三个月（PLAN.md §9）。
#
# ⚠ ⚠ ★ **但别把这道门当成「本来能拦住那个缺陷」的东西 —— 实测它拦不住。**
#   0.10 上的 SC2030/SC2031 只认**写在子 shell 那几行里**的赋值
#   （`( B=1 )` 与 `X=$(D=9; …)` 都报），而当年那处是「函数体里 `PIDS+=`、调用方 `$(f)`」——
#   逐字复现过，一声不吭。（顺带：管道右侧 `while read` 里改变量，0.10 也不报。）
#   ⇒ 这次补的是**覆盖面**：那一格从来没被扫过，底下压着 4 条真实告警。
#     真正拦住那个缺陷的是场景收尾时那条「用过的端口要还回去」的自证，不是这道门。
#   ★ 这条要写在明处 —— 一把没量过就被当成能量的尺子，比没有尺子更贵。
#
# ⚠ 顺带一条本文件自己踩到过两次的：注释行以「# 空格 shellcheck」开头会被当成**指令**
#   解析（SC1073）—— 写这个词的时候别让它落在行首。
#
# ★ ★ 改法照 `docker-run.sh` 对 `*_ONLY` 的那条路子（`only_mode_in` / `selftest_only_mode`）：
#   **不列举，改问一句结构性的问题**（「这棵树下有哪些 `.sh`」），**并且每次运行都自证**。
#   ⇒ 下一个人加场景时不需要记得改这里；而「问句本身瞎了」有判据看着。
#
# ★ ★ ★ **2026-09-05：加了第二棵树 `bench/`，并加了一道「有没有第三棵树」的普查。**
#   起因是 M3 第一刀新建了顶层 `bench/`，里面五个 `.sh` —— 而本文件当时只扫 `tests`
#   ⇒ **一整个目录的 shell 从出生起就没被静态看过**，与它当初要修的那个缺陷
#   （手写清单漏掉 `tests/quic-relay/`）**是同一个形状，只是换了一层**。
#   ⚠ ⚠ 只把 `bench` 加进清单是不够的：那还是一张清单，下一个顶层目录照样会漏。
#   ⇒ 照 §3.1 那条教训办 —— **普查那一步的筛子必须比门禁的宽**：
#     门禁扫 `$ROOTS`，而 `selftest_no_orphans` 用一个**更宽**的筛子（全仓，
#     只排除 target/vendor/.git）去问「有没有 `.sh` 落在 `$ROOTS` 之外」，有就红。
#   ★ 于是「加了新目录忘了改这里」从**静默漏扫**变成**当场红**。
#
# 用法：
#   bash tests/ci/shellcheck-all.sh          # 扫 <仓库根>/tests 与 <仓库根>/bench，并跑普查
#   bash tests/ci/shellcheck-all.sh <目录>   # 只扫指定的那棵树（做反证时用），⛔ 不跑普查
#
# 退出码：0 = 全过；非 0 = 静态检查有话说，或枚举器自测 / 普查没过。
set -euo pipefail

# ★ `LC_ALL=C.UTF-8` 不是可选项，而且要设在**这里**而不是调用方：本仓库的脚本注释是中文，
#   而 shellcheck 在 POSIX locale 下会在**输出报告时**炸掉
#   （`commitBuffer: invalid argument (cannot encode character …)`），退出码 2 ——
#   那样它红得像是发现了问题，实际是它自己说不出话，排查方向会整个跑偏。
#   ⇒ 设在脚本里，谁来调都逃不掉这一条。
export LC_ALL=C.UTF-8

# ★ 先回到仓库根再用**相对**路径扫：报告里的路径要能直接粘进编辑器，
#   而 `/w/tests/…` 只在容器里成立。
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO"
# ★ 给了参数就只扫那一棵（反证用）；没给就扫仓库自己那两棵，并跑普查。
if [ "$#" -ge 1 ]; then
  ROOTS=("$1")
  ORPHAN_CHECK=0
else
  ROOTS=(tests bench)
  ORPHAN_CHECK=1
fi

# ── 枚举器：结构性判据 ──────────────────────────────────────────────────────
#
# ★ 用 `find` 而不是 glob，两条理由都不是风格：
#   ① `tests/**/*.sh` 在**非交互 bash 里没有 globstar**，`**` 退化成 `*` ⇒ 它逐字等价于
#      `tests/*/*.sh`，**两层以下的脚本一个都不匹配**，而写着 `**` 的人以为覆盖到了。
#      （实测：容器里这两个模式展开出来的文件**逐个相同** —— 因为本仓库的脚本眼下正好
#       都只有一层深。⇒ 写 `**` 的人今天看不出差别，明天放一个两层深的脚本才会。）
#   ② glob 匹配不到东西时 bash 把**模式本身**当参数传下去 —— 交给 find 压根不产生这问题。
# ★ 全程 NUL 分隔（`-print0` / `read -d ''`）：含空格或换行的路径按行分隔会被劈成
#   两个不存在的路径，与 `docker-run.sh` 里那条行尾扫描是同一件事。
sh_files_under() {
  find "$1" -type f -name '*.sh' -print0 | sort -z
}

# ★ ★ **它必须自证没瞎，每次都证** —— 一个恒返回空的枚举器会让这道门永远绿，而
#   「一个字都没扫」与「全扫过且都没问题」在退出码上长得一模一样。
#   ⇒ 拿一棵**答案已知**的假树问它，逐字比对（同 `selftest_only_mode` 的形状）。
# ⚠ 逐字比对是有意的：它一次钉住两个方向 —— 少认一个红，多认一个也红。
selftest_sh_files() {
  local d f got want
  d=$(mktemp -d)
  # `dir.sh` 是个**目录**：判据是「文件」，不是「名字像脚本」。
  mkdir -p "$d/one" "$d/two/deep" "$d/a b" "$d/one/dir.sh"
  : > "$d/one/run.sh"
  : > "$d/two/deep/nested.sh"   # ★ 两层深 —— 一层的 glob 看不见它，这正是最该钉的一条
  : > "$d/a b/spaced.sh"        # ★ 路径带空格 —— 按行分隔会把它劈成两半
  : > "$d/one/notes.md"         # 不是 .sh
  : > "$d/one/run.sh.bak"       # 不**以** .sh 结尾

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

# ── 普查：有没有 `.sh` 落在 `$ROOTS` 之外 ───────────────────────────────────
#
# ★ ★ ★ **这个筛子必须比门禁的宽**，否则它只会把门禁的盲区原样复述一遍，
#   还看起来像做过一次独立普查（本仓 2026-09-05 §3.1 那条教训的原话）。
#   ⇒ 它问的是「**这个仓库拥有的** `.sh` 有没有落在根之外」。
#
# ★ ★ 用 `git ls-files` 而不是 `find`，**这是判据本身而不是实现口味**：
#   `find` 扫的是**工作树** ⇒ `handoff/`（gitignored、本地的、按 G115 不入库）里
#   随手放一个 `.sh` 就会误报，而它给出的修法（「把那个顶层目录加进 ROOTS」）
#   对 `handoff/` 恰恰是**错的建议**。换个问法之后，`target/`、`.git/` 以及
#   一切 gitignored 的东西自动落在判据之外，排除表从三项缩成一项。
# ⚠ 剩下那一项是 `vendor/`：它**是**被跟踪的，但它是 fork 进来的上游代码，
#   不归本仓的 lint 管（今天它下面一个 `.sh` 都没有 ⇒ 这条排除是为将来写的）。
#   ⛔ 别再往这张表里加东西 —— 每加一项就是一次消音。
# ⚠ ⚠ ★ **`--cached --others --exclude-standard` 三个都是承重的。**
#   光 `--cached` 只看**已跟踪**的 ⇒ 一个刚写出来、还没 `git add` 的脚本会整个溜过去，
#   而那恰恰是本场那五个 `bench/*.sh` 当时的状态 —— 判据在最该说话的那一刻沉默。
#   加上 `--others --exclude-standard` 之后它问的是「**已经在仓库里、或正在进仓库的**」，
#   而被 `.gitignore` 挡掉的（`handoff/`、`target/`）仍然落在判据之外。
orphan_sh_files() {
  git ls-files -z --cached --others --exclude-standard -- '*.sh'
}

selftest_no_orphans() {
  local f rel root hit orphans="" n=0 owned=0

  # ★ ★ 「没能检查」不算「检查通过」：问不出来就红，⛔ 不静默跳过。
  if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "★ 普查跑不了：这里不是 git 工作树，或者没有 git——**「没能检查」不算「检查通过」**。" >&2
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

  # ★ ★ **「一个都没数到」是红，不是「普查通过」** —— 与下面那条
  #   「扫空了」同一条纪律：一个恒返回空的普查，对每一棵树都说「没问题」。
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

# ★ ★ **「一个都没找到」是红，不是「扫完了没问题」。**
#   这是推导式清单唯一的失效方式，所以它要有自己的一句话。
# ⚠ 也正因为如此，**这里不许写成 `find … | xargs -r shellcheck`**：
#   `-r` 的语义就是「没有输入就一条都不跑」⇒ 扫空时退出码 0，安静地全绿。
#   （不带 `-r` 时 shellcheck 拿到零个文件会报 `No files specified.` 退出 3，是红的；
#    但那条报错说的是它自己的用法，不是「这棵树里没有脚本」。）
if [ "${#FILES[@]}" -eq 0 ]; then
  echo "★ $ROOTS_LABEL 下一个 .sh 都没找到 —— 这不是「扫完了」，是**扫空了**。" >&2
  exit 1
fi

# ★ 把覆盖面**打出来**：一道说不出自己扫了什么的门，绿的时候没人判断得了它扫得够不够。
#   这也是这次改动唯一让人看得见「quic-relay 现在在里面了」的地方。
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
