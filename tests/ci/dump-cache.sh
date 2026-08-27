#!/usr/bin/env bash
# 把构建缓存导出成一个归档，并**如实报告导出成功没有**（G94）。
#
# ★ ★ **为什么它不写在 workflow 的 `run:` 块里**（G94）：`docker-run.sh` 的 `LINT_CMD`
#   扫的是 `tests/**/*.sh`，写在 workflow 里就**一行都没人 lint** —— 而那个「退出码恒为 0」
#   的 bug 正是活在那种地方。搬出来还收掉第二样：它**能在开发机上跑**。
#   ⚠ 那条修法（`SAVE_RC=${PIPESTATUS[0]}` 紧跟管道）在 CI 上没有判据（它住在一个
#   `cache-hit != 'true'` 才执行的步骤里）⇒ 本文件底部的 `--self-check` 就是替它补的那道。
#
# 用法：
#   bash tests/ci/dump-cache.sh <缓存目录> [诊断文件]
#   bash tests/ci/dump-cache.sh --self-check       # 不碰 docker，只验退出码取法
#
# 退出码：0 = 跑完了（**不代表导出成功**）。导出成功与否看 stdout 的 `archived=`。
# ★ 这条口径是有意的，与本仓库那条「退出码只判命令跑没跑起来，结果绿不绿看报告正文」一致。
set -euo pipefail

# ── --self-check：不碰 docker，只钉住「退出码是怎么取的」──────────────────────
#
# ★ ★ 这一段就是那条修法的判据。它复现 Actions 的 shell（`bash -e`，**不带 pipefail**），
#   把一个必然失败的命令接上 `| tee`，然后用**两种写法**去取退出码：
#     · 旧写法（`if 管道; then …; fi` 之后取 PIPESTATUS）—— 必须读到 0（错的）
#     · 新写法（管道之后紧接着取）           —— 必须读到 70（对的）
#   ⚠ 两个方向都钉：只钉新写法的话，一个恒返回 70 的实现也会绿。
if [ "${1:-}" = "--self-check" ]; then
  fails=0
  tmp=$(mktemp)
  trap 'rm -f "$tmp"' EXIT

  # 旧写法：`fi` 之前最后执行的是 `RC=0` 这条**简单命令**，
  # 于是 `fi` 之后的 PIPESTATUS 已经是 `(0)` —— 恒把退出码覆写成 0。
  old_way() {
    local RC
    if (exit 70) 2>&1 | tee -a "$tmp" >/dev/null; then
      RC=0
    else
      RC=1
    fi
    RC=${PIPESTATUS[0]:-$RC}
    echo "$RC"
  }
  # 新写法：管道之后**紧接着**取，中间一条命令都没有。
  new_way() {
    local RC
    set +e
    (exit 70) 2>&1 | tee -a "$tmp" >/dev/null
    RC=${PIPESTATUS[0]}
    set -e
    echo "$RC"
  }

  got_old=$(old_way)
  got_new=$(new_way)
  if [ "$got_old" = "0" ]; then
    echo "  ✓ 旧写法确实读到 0（这就是那个 bug 的形状）"
  else
    echo "  ✗ 旧写法读到 $got_old —— 本该是 0。反证不成立了，这条自证已经证不到东西" >&2
    fails=$((fails + 1))
  fi
  if [ "$got_new" = "70" ]; then
    echo "  ✓ 新写法读到 70（真实退出码传得出来）"
  else
    echo "  ✗ 新写法读到 $got_new —— 本该是 70" >&2
    fails=$((fails + 1))
  fi
  # ★ 第三条：两种写法**必须给出不同的答案**。少了它，一个把两个函数写成
  #   同一件事的"重构"会让上面两条同时绿或同时红，而看不出它们已经不再对照。
  if [ "$got_old" != "$got_new" ]; then
    echo "  ✓ 两种写法给出不同答案（$got_old ≠ $got_new）—— 这条对照仍然是活的"
  else
    echo "  ✗ 两种写法给出了同一个答案（$got_old）—— 对照已经失效" >&2
    fails=$((fails + 1))
  fi

  echo
  if [ "$fails" -ne 0 ]; then
    echo "DUMP-CACHE SELF-CHECK FAILED: $fails 条不通过" >&2
    exit 1
  fi
  echo "DUMP-CACHE SELF-CHECK PASSED —— 退出码取法两个方向各钉一条，且对照仍然有效。"
  exit 0
fi

CACHE_DIR=${1:?用法：dump-cache.sh <缓存目录> [诊断文件]}
DIAG=${2:-$HOME/cache-diag.txt}
REPO=${REPO:-$(pwd)}

: > "$DIAG"
say() { echo "$@" | tee -a "$DIAG"; }

# ⚠ ⚠ 只量**毫秒级**的那几样。那版诊断（`docker system df -v` +
#   逐卷统计）把导出从 5m30s 拖到 24m54s、整轮 CI 到 40m02s —— 一个为了查清
#   问题而把问题变贵的诊断，当天就收窄成了下面这三行。
say "── 导出前的磁盘 ──"
df -h / 2>&1 | tee -a "$DIAG"
say "── docker 占了多少 ──"
docker system df 2>&1 | tee -a "$DIAG" || true

# ★ 门禁已经跑完了，镜像这时候一个都不需要留：它们本来就不跨轮缓存，
#   每一轮都要重建。搬运用的 helper 会被 cache.sh 重新拉（几十 MB）。
#   ⚠ 这一步不影响任何判据，纯粹是腾地方。
say "── 清掉 builder 缓存与所有镜像 ──"
docker builder prune -af >/dev/null 2>&1 || true
docker image prune -af >/dev/null 2>&1 || true
df -h / 2>&1 | tee -a "$DIAG"

# ⚠ ⚠ **判据取「命令成功了没有」，不取「文件在不在」。**
#   实测：一次 cancel-in-progress 把门禁掐了，导出随之失败，
#   **而半截归档已经落盘**（370MB，完整的是 2.2GB）——
#   按「文件在不在」判会把这份残档存进缓存键，而缓存条目不可变，
#   此后每一轮都精确命中它。**「文件存在」不是「导出成功」。**
say "── cache.sh save ──"
# ⚠ ⚠ ⚠ **不许写回 `if 管道; then …; fi` 再在 `fi` 之后取 PIPESTATUS**
#   （之前的写法，两处都是坏的，而且互相掩盖）：
#   ① Actions 的默认 shell 是 `bash -e {0}`，**不带 pipefail** ⇒ 管道的退出码是
#      `tee` 的，`if` 永远走 then 分支，`cache.sh` 失败根本传不出来；
#   ② 想补救 ① 的那行 `SAVE_RC=${PIPESTATUS[0]:-$SAVE_RC}` 写在 `fi` **之后**，
#      而 `fi` 之前最后执行的是 `SAVE_RC=0` 这条**简单命令** ⇒ 那时 PIPESTATUS
#      已经是 `(0)`，这一行**恒把退出码覆写成 0**。
#   ⇒ 唯一可靠的写法：**管道之后紧接着取 PIPESTATUS，中间不许有任何命令。**
#   ★ 这条由本文件的 `--self-check` 两个方向各钉一条（G94）。
set +e
bash "$REPO/tests/ci/cache.sh" save "$CACHE_DIR" 2>&1 | tee -a "$DIAG"
SAVE_RC=${PIPESTATUS[0]}
set -e
say "cache.sh save 退出码：$SAVE_RC"
# ⚠ 不要写成 `ls -la`（shellcheck SC2012）—— 这段代码当初在 workflow 的 `run:` 块里时
#   一次都没被扫过，G94 要收的正是这个。
# ⚠ 顺带一条：注释行以「# 空格 shellcheck」开头会被当成**指令**解析（SC1073）。
#   这条注释自己就踩过一次。
find "$CACHE_DIR" -maxdepth 1 -type f -printf '%10s  %f\n' 2>&1 | tee -a "$DIAG" || true

# ★ ★ **两道判据分开量，打架就说出来。**
#   写成「退出码为 0 **且** 归档文件在」一条与关系的话，坏掉的那道
#   （恒 0 的退出码）会被好的那道（`cache.sh` 失败时自己删掉半截归档）接住，
#   **连着好几轮 CI 都不显形**。⇒ 把「两道判据不一致」本身打成一行诊断：
#   它一旦出现，就说明其中一道坏了 —— 而不是等下一次侥幸用完。
ARCHIVED_FILE=no
[ -f "$CACHE_DIR/fulcrum-build-cache.tar.z" ] && ARCHIVED_FILE=yes
if [ "$SAVE_RC" = "0" ] && [ "$ARCHIVED_FILE" = "no" ]; then
  say "★ ★ 两道判据打架：退出码说成功，而归档文件不在 —— 先看这一步的退出码是怎么取的"
fi
if [ "$SAVE_RC" != "0" ] && [ "$ARCHIVED_FILE" = "yes" ]; then
  say "★ ★ 两道判据打架：退出码说失败，而半截归档还在 —— cache.sh 的清理路径可能变了"
fi

if [ "$SAVE_RC" = "0" ] && [ "$ARCHIVED_FILE" = "yes" ]; then
  ARCHIVED=true
  say "archived=true"
else
  ARCHIVED=false
  say "★ archived=false —— 不存缓存（宁可下一轮冷跑，也不占住那个键）"
fi
say "── 导出后的磁盘 ──"
df -h / 2>&1 | tee -a "$DIAG"

# ── 报给调用方 ──────────────────────────────────────────────────────────────
#
# ★ 在 Actions 里写 step output；在开发机上只打到 stdout。
#   ⚠ 两条路都走，是为了让这个脚本在两边**行为一样** —— 一个只在 CI 上
#   才有输出的脚本，在开发机上跑一遍等于什么都没验。
echo "archived=$ARCHIVED"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "archived=$ARCHIVED" >> "$GITHUB_OUTPUT"
fi
# 同一份诊断也塞进 job summary，两条路都留着。
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  { echo '```text'; cat "$DIAG"; echo '```'; } >> "$GITHUB_STEP_SUMMARY" || true
fi
