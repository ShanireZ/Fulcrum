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
#
# ★ 归一单独拆出来，因为**它有第二个用户**：卷上那个记「属于哪棵树」的 label
#   （见下面 `fulcrum_target_vol_create`）。两处各归一各的话，label 里的路径与
#   卷名里的哈希会指向同一棵树的两种写法，而症状是回收提示把一棵活着的树报成「已经不在」。
fulcrum_tree_norm() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m "$1"
  else
    printf '%s' "$1"
  fi
}

fulcrum_tree_tag() {
  local path=$1 norm
  norm="$(fulcrum_tree_norm "$path")"
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

# ── 卷属于哪棵树：写在 label 上 ──────────────────────────────────────────────
#
# ★ ★ **「别的树的卷绝不劝人删」解决了一半，另一半是磁盘。** 工作树是会消失的
#   （`.claude/worktrees/` 下那些用完就删），而它们的卷不会跟着消失 —— 一个约 6GB，
#   躺在那里，**没有任何东西说得出它还有没有主人**：卷名后缀是哈希，反推不回路径。
#   ⇒ 新建时把归一后的树路径记在 label 上，回收提示据此把「主人已经不在了」的那些
#     单列出来。这一条**不放宽** `docker-run.sh` 那条规矩：还在的树，照旧不给删除命令。
#
# ⚠ label **只有新建那一次写得进去**：对一个已存在的卷，`docker volume create`
#   是**静默的 no-op** —— 不报错，也不更新 label（实测）。这不构成问题，因为树变了
#   卷名就变了 ⇒ 名字与 label 永远是同一次新建写下的，不可能各说各话。
#   ★ 反过来说，**没有 label 的卷一定不是这条规则建的**（加它之前留下的，或手工建的），
#     那一类只能报「不明」，绝不能算成「已经不在」。
FULCRUM_TREE_LABEL="cool.cnb.fulcrum.tree"

# `$1` = 卷名；`$2` = 工作树路径（原样传，内部归一）。
#
# ⚠ 记的是 `fulcrum_tree_norm` 归一之后那一份，而不是 `/d/…` 那种 MSYS 路径：
#   后者会被 Git Bash 在传给 docker.exe 时改写成 `D:/…`（实测），于是同一棵树
#   在不同调用点记出两种写法。归一之后本来就是 `D:/…`，没有可改写的东西。
fulcrum_target_vol_create() {
  docker volume create --label "${FULCRUM_TREE_LABEL}=$(fulcrum_tree_norm "$2")" "$1" >/dev/null
}

# 一个 label 值对应的树现在是什么状态：live / gone / unknown。
#
# ★ `unknown` 与 `gone` **必须分开**：前者是「没能检查」，后者是「检查过了，没有」。
#   混成一个的话，加这条规则之前留下的卷会被当成无主的报出去。
fulcrum_tree_state() {
  local tree=${1:-}
  if [ -z "$tree" ]; then
    printf 'unknown'
  elif [ -d "$tree" ]; then
    printf 'live'
  else
    printf 'gone'
  fi
}

# ── 第二条判据：这个卷名，**今天这份推导造得出来吗** ──────────────────────────
#
# ★ ★ ★ **为什么 label 那一条不够。** label 记在建卷那一刻，而对一个已存在的卷
#   `docker volume create` 是**静默 no-op** ⇒ **机制存在之前留下的卷永远补不上 label**，
#   一律落进「不明」，而「不明」这一半只进不出 —— 它没有任何出口。
#   ⚠ ⚠ 这条缺口的代价，2026-09-04 在**两台开发机上各量过一次**：
#     · 开发机 A（写下这段注释的那台）：11 个 target 卷、10 个无主、约 92GB，
#       回收提示**看得见 1 个**。三个成因彼此无关：旧命名代 8 个 · label 键还没统一
#       那一代 1 个（键写成了 `…checkout`）· 本树自己的卷比 label 机制早生 17 分钟。
#     · 开发机 B（`ShanireHomePC`）：12 个 `fulcrum*` 卷、6 个无主、约 48GB，
#       六个**全部**落在下面的规则 ③（建于 label 机制之前，`Labels` 是空的 `map[]`）。
#   ★ ★ **并排放这两行不是记流水账，是这段注释的判据本身**：卷是**机器本地状态**，
#     ⇒ 「本机」「实测」这类词在源码注释里没有机器名时，读的人无从分辨它说的是哪一台。
#     ⛔ 换台机器读到这里，别把上面任何一个数当成你面前这台的读数 —— 自己数一遍。
#   ⇒ 缺的不是「多读一个键」，是一条**不依赖建卷那一刻记下了什么**的判据。
#
# ★ 本判据不问「它是什么时候建的」，问的是 `fulcrum_target_vol` 今天**造不造得出它**。
#   造不出来 ⇒ 三个调用点（`docker-run.sh` / `systemd-run.sh` / `cache.sh` 都走这一份推导）
#   没有任何一条够得着它。⛔ 这不是「看起来很旧」那种年龄启发 —— 年龄是启发，
#   「现行代码造不出这个名字」是关于**今天这份代码**的陈述，读一份推导就能证。
#
# ⚠ 诚实说出残余风险：安全性建立在「够得着它的只有这三处」之上。若有人在另一棵工作树上
#   checkout 着旧提交跑旧脚本，它的卷会是旧代形状 ⇒ 会被列进清单。后果是**那棵树下次
#   全量重编**（一次冷编），不是数据丢失；而 `docker volume rm` 对占用中的卷本来就会拒绝。
#
# ⚠ ⚠ 下面这个正则是**手写**的，而手写的形状会随命名方案改动悄悄失效。两道东西钉住它：
#   ① `selftest_vol_shape` 拿 `fulcrum_target_vol` **真的生成一个名字**来比 ——
#      推导一改形状，那条自测当场红（⛔ 手写一个样例名去比是钉不住的）；
#   ② `docker-run.sh` 每次跑再拿**本树的真名字**自证一次 —— `fulcrum_tree_tag` 在没有
#      sha256sum 也没有 git 的宿主上会退到另一种标签，那时本判据自己失效，
#      而**失效必须说出来**，绝不能默默把当代的卷判成旧代的。
fulcrum_vol_shape() {
  if [[ "${1:-}" =~ ^fulcrum-target-[0-9a-f]{12}-[0-9a-f]{12}$ ]]; then
    printf 'current'
  else
    printf 'legacy'
  fi
}

# 一个**不属于本树**的卷该怎么处置：`orphan`（可以给删除命令）/ `keep`（只报数）。
#
# `$1` = 卷名；`$2` = label 里读出来的树路径（可空）；`$3` = 形状判据这一轮可不可信（1/0）。
#
# ★ 三条规则**有次序**，而次序本身是承重的：
#   ① label 说主人**还在** ⇒ 一律 `keep`。★ ★ 它必须**短路在最前面** —— 否则一个
#      「旧代形状 + 树还活着」的卷会掉进规则 A，而那就是「劝人删掉隔壁正在编译的缓存」。
#      ⚠ 这个洞是写反证时才发现的，⛔ 不是设计时就想到的。
#   ② label 说主人**已经不在** ⇒ `orphan`（原有的规则，一个字没改）。
#   ③ 读不到 label，**且**今天的推导造不出这个名字 ⇒ `orphan`。
#   其余一律 `keep` —— 「没能检查」不算「检查通过」。
#
# ⚠ ⚠ **规则 ③ 绝不许吞掉规则 ② 保守的那一半**：名字是**当代形状**、但读不到 label 的，
#   照旧 `keep`。放宽一次的症状不是报错，是有一天它给出一条删错卷的命令。
fulcrum_vol_verdict() {
  local name=${1:-} tree=${2:-} shape_ok=${3:-0} state
  state=$(fulcrum_tree_state "$tree")
  if [ "$state" = live ]; then
    printf 'keep'
  elif [ "$state" = gone ]; then
    printf 'orphan'
  elif [ "$shape_ok" = 1 ] && [ "$(fulcrum_vol_shape "$name")" = legacy ]; then
    printf 'orphan'
  else
    printf 'keep'
  fi
}

# 一批卷各自占多少盘，一行一个 `名字|体积`。
#
# ⛔ **有意不求和。** `docker system df` 给的是**已经格式化过的字符串**（`9.628GB`），
#   在 bash 里解析单位再相加是一台会给出「像样的错答案」的一次性量具，而本仓刚在
#   别处栽过这一跤。逐个列出体积同样达到目的：**让代价说得出口**。
#   ⇒ 「另有 10 个卷」读起来无关痛痒，「另有 10 个卷，9.6GB / 16.7GB / 11.1GB …」不是。
#
# ⚠ 量不到就一个字都不打，由调用方说「没量到」——「读不到」不许变成「没这回事」。
# ★ 它比 `docker volume ls` 贵得多（开发机 A 上实测 1.4–1.9s vs 0.34s；⚠ 这个成本随**宿主上所有卷**
#   的内容涨，不只随 fulcrum 的卷涨 ⇒ 换台机器要重新量，别用这个数）⇒ 调用方只在
#   真的有东西要报的时候才调它，桶是空的就一次都不调。
fulcrum_vol_sizes() {
  docker system df -v --format '{{range .Volumes}}{{.Name}}|{{.Size}}
{{end}}' 2>/dev/null || true
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
