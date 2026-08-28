#!/usr/bin/env bash
# shellcheck shell=bash
#
# 门禁那个 `target` 卷的两件事：**它叫什么**，以及**同一时刻只许一次门禁用它**。
#
# 三处 source 本文件：`tests/m0/docker-run.sh` · `tests/m1/systemd-run.sh` · `tests/ci/cache.sh`。
#
# ★ ★ **为什么是一个文件而不是三份表达式。** 卷名此前在那三处各写了一遍
#   `fulcrum-target-${SHA:0:12}`。三处各算各的，规则一改就会安静地指向两个不同的卷 ——
#   而失效形态不是报错：M1 会挂上一个**自动新建的空卷**，CI 缓存会**永远不命中**，
#   现象只是「怎么找不到二进制」和「怎么还是这么慢」。`tests/ci/cache.sh` 顶部
#   那句「不自己再造一套卷名算法」说的就是这件事，本文件是它的实现。

# ── 这棵工作树的短标签 ────────────────────────────────────────────────────────
#
# ★ ★ ★ **卷名必须同时跟着「哪个镜像」与「哪一棵工作树」走。**
#
#   「跟着镜像走」的理由在 `docker-run.sh` 里写着，仍然成立（cargo 的 fingerprint
#   不覆盖 C 工具链）。今天多出来的是第二条：**一台机器上现在有好几棵工作树**
#   （主树加 `.claude/worktrees/` 下若干），而容器把各自的 `/w` 挂成不同的源码树、
#   却把同一个 `/w/target` 挂给了所有人。
#
#   ⚠ ⚠ 后果不是「编得慢」，是**门给出别人家的读数，而两边都不红**。实测两种：
#     ① 一次编译错误指向一个只存在于另一棵树的符号；
#     ② 一次报「696 条全绿、退出码 0」，跑的却是**没有本次新判据**的旧测试二进制。
#   第二种是致命的那一种 —— 它不报错，它给一个像样的、绿的、错的答案。
#
# ★ 传进来的路径请一律是 `cd "$(dirname …)/../.." && pwd` 的结果（三处调用点都是这么来的）。
#   本函数内部再按平台归一一次，免得同一棵树在两个调用点算出两个标签。
fulcrum_tree_tag() {
  local path=$1 norm
  if command -v cygpath >/dev/null 2>&1; then
    norm="$(cygpath -m "$path")"
  else
    norm="$path"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$norm" | sha256sum | cut -c1-12
  elif command -v git >/dev/null 2>&1; then
    printf '%s' "$norm" | git hash-object --stdin | cut -c1-12
  else
    # ★ ★ 兜底**不许退化成一个常量** —— 那正好把「两棵树共用一个卷」这件事重新造出来。
    #   退而求其次：把路径本身洗成合法的卷名字符，它照样是一树一个，只是不好看。
    printf '%s' "$norm" | tr -c 'a-zA-Z0-9' '_' | tail -c 40
  fi
}

# `$1` = Dockerfile 的内容哈希（可为空）；`$2` = 工作树路径。
#
# ★ 顺序是「镜像哈希在前、树标签在后」，因为 `docker-run.sh` 要按后缀
#   `-<树标签>` 把「本树的旧卷」与「别的树的卷」分开 —— 前者可以劝人删，后者绝不可以。
fulcrum_target_vol() {
  printf 'fulcrum-target-%s-%s' "${1:0:12}" "$(fulcrum_tree_tag "$2")"
}

# ── 门禁互斥 ────────────────────────────────────────────────────────────────
#
# ★ ★ ★ **卷分开了还不够**：同一棵树上并发跑两次门禁，两次仍然共用那一个卷，
#   于是回到同一种失效 —— 一次跑读到另一次跑写了一半的产物，而两边都不红。
#
# ★ ★ **用 `mkdir` 而不是 `flock`，因为这台宿主上没有 `flock`。**
#   宿主是 Windows + Git Bash（MSYS），`command -v flock` 空手而归（实测）。
#   写一把在这台机器上其实不生效的锁，比没有锁更坏 —— 它会让人以为并发已经拦住了。
#   `mkdir` 在目录已存在时必然失败、且不留副作用，这一点在 NTFS 与 ext4 上都成立。
#   ⚠ 绝不可以退化成 `[ -d "$dir" ] || mkdir "$dir"` —— 那是两步，中间正好是竞态窗口。
#
# 锁放在宿主机的临时目录里，不放仓库里：仓库里那份会被「target/ 必须为空」和行尾两道门看见。
FULCRUM_LOCK_ROOT=${FULCRUM_LOCK_ROOT:-${TMPDIR:-/tmp}/fulcrum-gate-locks}
FULCRUM_LOCK_DIR=${FULCRUM_LOCK_DIR:-}   # 本进程真正持有的那一把（空 = 没持有）

# `$1` = 锁名（＝卷名）；`$2` = 说明用的工作树路径。拿不到返回 1，**不排队、不等待**。
fulcrum_lock_acquire() {
  local name=$1 tree=${2:-<未知>} dir owner
  dir="$FULCRUM_LOCK_ROOT/$name.lock"

  # ★ 可重入，靠一个导出的环境变量而**不是**比 pid：`docker-run.sh` 在自己持锁期间会调
  #   `tests/m1/systemd-run.sh`，而后者单独跑时必须自己上锁。比 pid 的话，
  #   「同一台机器上另一次门禁」也会被认成自己人。
  if [ "${FULCRUM_GATE_LOCK_HELD:-}" = "$dir" ]; then
    return 0
  fi

  mkdir -p "$FULCRUM_LOCK_ROOT"

  if ! mkdir "$dir" 2>/dev/null; then
    owner=$(cat "$dir/pid" 2>/dev/null || true)
    if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
      echo "★ 另一次门禁正在这棵工作树上跑，本次不启动。" >&2
      echo "    树   ：$tree" >&2
      echo "    卷   ：$name" >&2
      echo "    持有 ：pid $owner，自 $(cat "$dir/since" 2>/dev/null || echo '未知时间')" >&2
      echo "  ⚠ 两次门禁共用同一个 target 卷，一起跑会互相读到对方写了一半的产物 ——" >&2
      echo "    而那种坏法不报错，它给一个像样的、绿的、错的答案。" >&2
      echo "  等那一次跑完再来；确认它其实已经死了就删掉锁目录：" >&2
      echo "      rm -rf \"$dir\"" >&2
      return 1
    fi
    # ★ 持有者已经不在了（上一次被 Ctrl-C / 被 kill，没走到 trap）——**接管，并且说出来**。
    #   一次没人知道发生过的接管，与没有锁无异。
    # ⚠ 残留风险说在明处：pid 会被系统复用，理论上可能把一把活锁误判成陈旧锁。
    #   代价对比很清楚 —— 不接管的话，一次 Ctrl-C 就让这棵树上的门禁**永久**起不来。
    echo "⚠ 这棵树上留着一把没人认领的门禁锁（pid ${owner:-<未知>} 已不在），本次接管。" >&2
    echo "    $dir" >&2
    rm -rf "$dir"
    mkdir "$dir" 2>/dev/null || {
      echo "★ 接管失败：同一瞬间有第三方也在抢这把锁。重跑一次即可。" >&2
      return 1
    }
  fi

  printf '%s\n' "$$" > "$dir/pid"
  printf '%s\n' "$tree" > "$dir/tree"
  date '+%Y-%m-%dT%H:%M:%S%z' > "$dir/since" 2>/dev/null || true
  FULCRUM_LOCK_DIR="$dir"
  export FULCRUM_GATE_LOCK_HELD="$dir"
  # ★ trap 在这里设，不留给调用方：这类锁最常见的坏法就是「忘了设 trap」，
  #   而那种坏法要等到下一次跑不起来才被发现。
  #   ⚠ 两个调用点原本都没有 EXIT trap（已核对），所以这里不会覆盖掉别人的。
  trap fulcrum_lock_release EXIT
  return 0
}

# EXIT trap 调它。★ **只删自己那一把**：接管别人的锁是上面那条明确的路，
#   而在这里误删别人的锁会让并发保护整个失效，且没有任何症状。
fulcrum_lock_release() {
  local held=${FULCRUM_LOCK_DIR:-}
  [ -n "$held" ] || return 0
  if [ "$(cat "$held/pid" 2>/dev/null || true)" = "$$" ]; then
    rm -rf "$held"
  fi
  FULCRUM_LOCK_DIR=""
  unset FULCRUM_GATE_LOCK_HELD
  return 0
}
