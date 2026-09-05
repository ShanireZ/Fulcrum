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

REPO="$(git rev-parse --show-toplevel)"

# ── 这一次到底有没有代码往外送 ─────────────────────────────────────────────
#
# `pre-push` 从 stdin 收若干行 `<local_ref> <local_sha> <remote_ref> <remote_sha>`。
# ⚠ 删除远端分支那种，`local_sha` **全是 0**：那一次一个字节的代码都没往外送，编它没意义。
# ⇒ 只有**全部**都是删除（或一行都没有）才跳过；只要有一行是真推送就照编。
# ★ 判「全 0」不比长度：sha1 是 40 位、sha256 是 64 位，写死长度的写法会在换算法那天
#   **静默地把删除当成推送**（多编一次，不致命）或反过来 —— 用「有没有非 0 字符」判。
HAS_CONTENT=0
while read -r _local_ref local_sha _remote_ref _remote_sha; do
  case "$local_sha" in
    *[!0]*) HAS_CONTENT=1 ;;
  esac
done
if [ "$HAS_CONTENT" = 0 ]; then
  echo "[pre-push] 这一次只有删除、没有代码往外送 —— 跳过本门。"
  exit 0
fi

# ── 说清楚这一趟量的是什么 ─────────────────────────────────────────────────
if [ -n "$(git status --porcelain)" ]; then
  echo "⚠ 工作树不干净 ⇒ **本次量的是工作树，不是你正在推的那几笔。**" >&2
  echo "  未提交的改动可能正好补上了被推那一笔的窟窿，而这道门看不见这件事。" >&2
  echo "  要它守得住被推的状态，就在工作树干净时推。" >&2
fi

# ⚠ ⚠ 这两行**必须单引号**：双引号里的反引号是**命令替换** —— 写成双引号的话，
#   bash 会真的去跑那条 cargo 命令，而 Rust 在这台宿主上不许跑（G107）。
#   ★ 写这个文件时当场踩了一次；`shellcheck` 的 SC2006 也守得住，但别指望它兜底。
echo '[pre-push] shellcheck + 只编译、不跑测试（shellcheck-all.sh && cargo test --no-run --workspace --all-targets）——'
echo '           判据是一句话：**静态上就站不住的树不许离开这台机器**。'

# ⚠ 走的是 `docker-run.sh` 的 `COMPILE_ONLY` 那一格，⛔ 不自己拼 `docker run`：
#   构建镜像、target 卷名、那把「同一棵树只许跑一次」的锁、行尾字节探针、两道文档门 ——
#   全部只有一份推导（`tests/lib/vol-lock.sh` + `docker-run.sh`）。
#   ★ 各写一遍的失效形态是**安静地指向另一个卷**，而那时门测的是别人家的读数。
if COMPILE_ONLY=1 bash "$REPO/tests/m0/docker-run.sh"; then
  echo "[pre-push] ✓ shellcheck 与编译（含全部测试目标）都过了 —— 放行。"
  exit 0
fi

echo "" >&2
echo "⛔ **push 已中止**：这棵树 shellcheck 没过、编译不过，或者这道门根本没能跑起来。" >&2
echo "   ★ 「没能检查」不算「检查通过」——docker 起不来、或本树上另有一次门禁正在跑" >&2
echo "     （那把锁不排队，它会点名持锁的 pid），都会走到这里。上面那段输出说的就是原因。" >&2
echo '   ⚠ 修好再推。⛔ 别顺手 --no-verify：这道门此刻拦下的，正是它被加出来的那个东西。' >&2
exit 1
