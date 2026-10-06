#!/usr/bin/env bash
# `pre-push` 门：**别让一棵静态上就站不住的树离开这台机器。**
#
# 起因（2026-09-04 实付代价）：`9c0634f` 是一次**手工保盘提交** —— 测试里引用一个
# 还没定义的常量，三处 `E0599`，整个 test crate 编译不过，**而它已经被推上去了**。
# ⚠ ⚠ 它躲过了每一个便宜的读数：`git log` 干净、`git status` 干净、两道文档门全绿、
# `origin/main..main` 是 0 —— **全都说「一切正常」**。⇒ 唯一判得动它的是**真去编一次**。
#
# ## ★ 为什么是 `pre-push` 而不是 `pre-commit`
#
# 那种半成品提交**本来就是要的**：它的用途是防数据丢失，拦它等于拦掉它的全部价值。
# 要拦的是**离开这台机器**那一步。
#
# ## ★ 它跑 `shellcheck` + 编译，⛔ 一条测试都不跑 —— 这是权衡，不是偷懒
#
# 本机常年跑着 20+ 个会话，并发实测比空载慢 3.75 倍 ⇒ 按空载余量设的超时会**周期性
# 假红**。而一道假红过几次的 hook 会被 `--no-verify` 永久绕过 —— 那时它连编译都不看了。
# ⇒ 这两件事答的正是那个缺陷问的那句话，且**都不可能因机器负载而红**。
# ⚠ 代价写在明处：**它不拦「编得过但测试红」**。那一格归完整门禁，本门不冒充它。
#
# ## ⚠ ⚠ 2026-09-05：`shellcheck` 是补上来的，而它此前**不在**这道门里
#
# ⛔ **有意留下这段而不是把上面那句改干净**：它是这道门被观察到的一次真实盲区。
# 那天改了 `tests/lib/vol-lock.sh` 与 `tests/m0/docker-run.sh` 两个 `.sh`，
# 跑了**四趟** `COMPILE_ONLY` 全绿 —— 而 `COMPILE_ONLY` 那一格当时是
# `CMD='cargo test …'` **直接替换**，不是 `CMD="$CMD && …"` 追加 ⇒ 它不含 `LINT_CMD`。
# ★ ★ ★ 于是**一次纯 shell 的改动可以完整地穿过这道门**，而那四个绿对它没有任何判别力。
# ⇒ owner 拍板把 `shellcheck-all.sh` 挂进那一格（⛔ 不挂 `fmt`/`clippy`：clippy 要先编依赖，
#   会把这道门从秒级拖成分钟级，而**门变慢正是它被 `--no-verify` 绕过的起点**）。
# ⚠ 仍然不拦的：`fmt` 差异、clippy 告警、以及所有测试。那三格归完整门禁。
#
# ## ⚠ ⚠ 2026-10-06（G158）：`shellcheck` 改成**看被推的提交**再决定跑不跑
#
# 起因是一次实测：热缓存、干净工作树，三趟 33–61s，而网络本身只占约 2s。
# 最慢的一项正是 `shellcheck`（54 个脚本一次全扫，15–33s），而它每一趟都跑，
# 不管被推的提交碰没碰 `.sh`；容器外的宿主机自测与遗留卷报告又占 13–20s
# （Git Bash 每次 fork 几十毫秒，约 2000 条命令）。⇒ owner 拍板两条：
#   ① 被推的提交**净改动**碰到 `*.sh` · `*shellcheckrc` · `docker/Dockerfile.build`
#      （钉着 shellcheck 版本）之一才跑 shellcheck。判不出来的一律照跑：远端还没有这个引用、
#      远端那一端本地没有、`git diff` 本身失败 —— 「没能判断」不算「不需要」。
#   ② 调 `docker-run.sh` 时关掉宿主机自测与遗留卷报告。它们验的是门自己的机器与磁盘账，
#      完整门禁每趟照跑。
# ⚠ 代价写在明处：
#   · shellcheck 扫的仍是**工作树**，跳不跳由被推的提交决定 ⇒ 被推的提交没碰 `.sh`、
#     而工作树里躺着一个未提交的坏 `.sh` 时，本门放行（那个 `.sh` 不在这次推送里）。
#   · 门自己的机器坏了（卷名推导、锁、字节探针）时，本门不再当场发现，要等下一趟完整门禁。
#     ⇒ **已由 G159 收窄**（同日）：宿主机自测改成按指纹跑 —— 门的脚本（`docker-run.sh`、
#       `tests/lib/*.sh`）或宿主工具（bash / MSYS 运行时 / Git for Windows）一变，本门当场照跑；
#       都没变且本工作树有同一指纹的通过记录才跳过。判据与盲区写在 `docker-run.sh` 那一段。
# ★ 判据钉在 `--self-check`（合成仓库，两个方向都有），挂在完整门禁的 lint 那一格；
#   ⛔ 不在 pre-push 路径上跑 —— 那正是本条要省的那一类开销。
# ⛔ 三个开关**不由调用方给**：本脚本按判据显式赋值后传下去，环境里带进来的同名变量盖不过它们。
#
# ## ⚠ ⚠ 已知盲区：它量的是**工作树**，不是被推的那几笔
#
# `pre-push` 跑在工作树上。工作树干净时工作树 ≡ `HEAD`，而 owner 批量推时正是这种情形
# （2026-09-04 那次也是：树干净、HEAD 就是那笔坏的）⇒ 那时它守得住。
# **工作树脏时它守不住**：未提交的改动可能正好补上了被推那一笔的窟窿，而本门看不见。
# ⇒ 脏树时下面会**明说自己量的是什么**，⛔ 不假装它守住了被推的状态。
#
# ## 装它（⚠ hook 不在版本控制里 —— 换机器 / 重新 clone 都要再跑一次）
#
#     printf '#!/usr/bin/env bash\nexec bash "$(git rev-parse --show-toplevel)/tests/ci/pre-push.sh" "$@"\n' \
#       > .git/hooks/pre-push && chmod +x .git/hooks/pre-push
#
# ⛔ **本脚本不提供任何自己的绕过开关**：一个随手能设的开关会把门变成建议
#    （与本仓「不留逐行豁免记号」同一条纪律）。真要绕过只有 `git push --no-verify` ——
#    那是一次**显式**的、看得见的动作。
set -euo pipefail

# ── 被推的这一段会不会改变 shellcheck 的结论（G158）──────────────────────────
#
# 返回 0 = 要跑，stdout 说出理由（被碰到的路径，或为什么判不出来）；返回 1 = 不必跑。
# ★ 比的是两端的**树**（`git diff <远端> <本地>`），⛔ 不是逐笔看提交：中间加了又删掉的 `.sh`
#   不改变被推那一端的内容，而 shellcheck 只看内容。远端不是祖先（强推）时这样比照样对。
# ★ 三类路径都承重：`*.sh` 是被扫的对象；`*shellcheckrc` 改规则；`docker/Dockerfile.build`
#   钉着 shellcheck 的版本 —— 后两者一个 `.sh` 都不碰，也能让同一批脚本的结论变红。
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

# ── `--self-check`：拿一棵答案已知的合成仓库钉住上面那条判据 ──────────────────
#
# ★ 两个方向都要有：恒答「要跑」的坏判据会让「不必跑」那几条红，恒答「不必跑」的
#   会让「要跑」那几条红 —— 只有一个方向的自测，对另一种坏法零判别力。
# ★ 挂在完整门禁 lint 那一格（容器里跑）；⛔ 不在 pre-push 路径上跑。
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
  unknown=$(printf 'de%038d' 0)   # 一个本地没有的 sha

  n=0
  rc=0
  # expect <yes|no> <说明> <远端> <本地> [理由里必须整行出现的路径]
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

# ── 这一次到底有没有代码往外送 ─────────────────────────────────────────────
#
# `pre-push` 从 stdin 收若干行 `<local_ref> <local_sha> <remote_ref> <remote_sha>`。
# ⚠ 删除远端分支那种，`local_sha` **全是 0**：那一次一个字节的代码都没往外送，编它没意义。
# ⇒ 只有**全部**都是删除（或一行都没有）才跳过；只要有一行是真推送就照编。
# ★ 判「全 0」不比长度：sha1 是 40 位、sha256 是 64 位，写死长度的写法会在换算法那天
#   **静默地把删除当成推送**（多编一次，不致命）或反过来 —— 用「有没有非 0 字符」判。
# ★ 同一趟顺手按 `shellcheck_trigger` 判跑不跑 shellcheck：任何一行要跑，这一趟就跑。
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

# ── 说清楚这一趟量的是什么 ─────────────────────────────────────────────────
if [ -n "$(git status --porcelain)" ]; then
  echo "⚠ 工作树不干净 ⇒ **本次编译与 shellcheck 量的是工作树，不是你正在推的那几笔。**" >&2
  echo "  未提交的改动可能正好补上了被推那一笔的窟窿，而这道门看不见这件事。" >&2
  echo "  （跑不跑 shellcheck 倒是按被推的提交判的。）要它守得住被推的状态，就在工作树干净时推。" >&2
fi

# ⚠ ⚠ 这几行**必须单引号**：双引号里的反引号是**命令替换** —— 写成双引号的话，
#   bash 会真的去跑那条 cargo 命令，而 Rust 在这台宿主上不许跑（G107）。
#   ★ 写这个文件时当场踩了一次；`shellcheck` 的 SC2006 也守得住，但别指望它兜底。
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
#   构建镜像、target 卷名、那把「同一棵树只许跑一次」的锁、行尾字节探针、两道文档门 ——
#   全部只有一份推导（`tests/lib/vol-lock.sh` + `docker-run.sh`）。
#   ★ 各写一遍的失效形态是**安静地指向另一个卷**，而那时门测的是别人家的读数。
# ★ 三个开关在这里**显式赋值**，环境里带进来的同名变量盖不过它们（G158 / G159）。
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
