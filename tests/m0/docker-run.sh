#!/usr/bin/env bash
# 在容器里构建并跑验证（G26）。宿主机什么都不用装，只要有 Docker。
#
#   bash tests/m0/docker-run.sh    # 全量：构建 + lint + 自研 crate 测试 + fork 回归网 + 全部端到端场景
#
# 一次只开一个 `<X>_ONLY=1` 只跑那一格；`<X>_TESTS=0` 跳过那一格：
#
#   BUILD · LINT · UNIT · VENDOR             构建 · fmt+clippy+shellcheck · 自研 crate 测试 · fork 回归网
#   SERVE · L4 · FILES · CACHE · CACHEDISK   数据面 · L4 面 · 静态文件 · 缓存 · 缓存磁盘后端
#   ENCODE · H3 · PP · LOG                   压缩 · HTTP/3 · PROXY protocol（HTTP 面）· 访问日志
#   METRICS · RELAY                          Prometheus 指标 · QUIC 跨进程转交
#   ACME · RENEW · SMOKE · STRESS · MUSL     签发 · 续期 · 冒烟 · 压力 · musl 静态产物
#   UNCLAIMED                                未被认领的继承 fd
#
#   LINT=0 跳过 lint；M1_TESTS=0 跳过 M1 的 systemd 场景。
#
# ★ M1 的 systemd 场景跑在**另一个容器**里（systemd 当 PID 1），由本脚本在最后调用
#   tests/m1/systemd-run.sh 驱动；单独跑用 `bash tests/m1/systemd-run.sh`。
#
#   DOCKER_USER="$(id -u):$(id -g)"  bash tests/m0/docker-run.sh
#     ★ Linux 宿主机／CI 上用，免得容器以 root 在挂进去的工作树里留下 root 属主的产物。
#       Windows 上**默认不开** —— 传宿主机 uid 会让命名卷（root 属主）不可写。
#       ⚠ 这条没有在 Linux 上实测过：它是留好的口子，不是已验证的路径。
#
# 构建缓存放在 docker 命名卷里，既不污染宿主机，也避开 Windows 文件系统的慢 I/O。
set -euo pipefail

IMAGE=${IMAGE:-fulcrum-build:local}
REPO_UNIX="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# target 卷叫什么、以及「同一棵树同一时刻只许跑一次门禁」那把锁，都在这里。
# ★ 三处（本文件 · tests/m1/systemd-run.sh · tests/ci/cache.sh）共用同一份推导，
#   不各写一遍 —— 各写一遍的失效形态是安静地指向两个不同的卷。
VOL_LOCK_LIB="$REPO_UNIX/tests/lib/vol-lock.sh"
# shellcheck source=tests/lib/vol-lock.sh
. "$VOL_LOCK_LIB"

# ★ 行尾前置检查 —— **必须放在 `export MSYS_NO_PATHCONV=1` 之前**。
#
#   `.gitattributes` 声明「行尾无条件 LF」，但它只在 checkout / commit 时生效——
#   **容器 bind-mount 的是工作树，不是索引**。宿主机上任何工具（Windows 版 Python 的
#   `write_text()` 默认就把 \n 翻成 \r\n）写过的文件都会在工作树里留下 CRLF，
#   而 `git status` 依然干净。.sh 一旦沾上 CRLF，容器里的 bash 当场报 `\r: command not found`。
#
#   ★ ★ 位置是硬要求，不是风格：`MSYS_NO_PATHCONV=1` 一旦生效，原生 git.exe 收到的
#     就是未经转换的 `/d/...`，`rev-parse` 会失败，这道门会被**静默跳过**。
#
# ★ ★ **拿不到 git 时不「跳过」，而是换一条不依赖 git 的路走完** ——
#   「没能检查」不算「检查通过」，而这道门真实的失效模式恰恰就是 `git rev-parse` 失败。

# 逐字节数 CR。★ **不要用 grep**：MSYS 的 grep 在读取阶段就把行尾规整掉了，
#   CRLF 文件里 `grep -c $'\r'` 返回 0，于是会得到一份**很像真结论的全 0 报告**。
#   `tr -dc | wc -c` 在宿主机与容器里都是对的。
cr_count()  { tr -dc '\r'   < "$1" | wc -c; }
nul_count() { tr -dc '\000' < "$1" | wc -c; }   # 含 NUL 即二进制，同 grep -I 的判据

# ★ ★ **这两个原语必须自证没瞎，每次都证。** 它们的失效方式是安静的：换个平台、
#   换个实现（比如有人「顺手优化」成 grep），它们就恒返回 0，这道门永远绿。
#   代价是三个临时文件、两次 `tr` —— 比一份假绿便宜太多。
selftest_byte_probes() {
  local d rc=0
  d=$(mktemp -d)
  printf 'a\r\nb\r\n' > "$d/crlf"      # 2 个 CR
  printf 'a\nb\n'     > "$d/lf"        # 0 个 CR
  printf 'x\000y'     > "$d/bin"       # 含 NUL
  [ "$(cr_count  "$d/crlf")" -eq 2 ] || { echo "★ cr_count 认不出 CRLF 文件里的 CR" >&2; rc=1; }
  [ "$(cr_count  "$d/lf")"   -eq 0 ] || { echo "★ cr_count 在纯 LF 文件上误报"      >&2; rc=1; }
  [ "$(nul_count "$d/bin")"  -gt 0 ] || { echo "★ nul_count 认不出二进制文件"        >&2; rc=1; }
  [ "$(nul_count "$d/lf")"   -eq 0 ] || { echo "★ nul_count 在文本文件上误报"        >&2; rc=1; }
  rm -rf "$d"
  [ "$rc" -eq 0 ] || {
    echo "  行尾检查的字节原语自测未通过——**本次 CRLF 检查的结论一律不可信**。" >&2
    echo "  多半是 tr 的实现变了，或有人把它换成了看不见 CR 的 grep。" >&2
    exit 1
  }
}
selftest_byte_probes

# ── 「现在是不是某个 `*_ONLY` 模式」──────────────────────────────────────────
#
# ★ ★ **判据是结构性的**：不逐个列举，而是问一句「有没有任何一个 `*_ONLY` 被设成 1」。
#   ⇒ 下一个人加场景时不需要记得改这里。⚠ 换成手工清单的话，漏掉一项就会让
#   「只跑这一格」连带跑 M1 那几个 systemd 场景，多出几十条 `  ✓` ——
#   **拿整份日志去数断言的人会得到一个偏大而很像样的数字**。
# ⚠ `M1_ONLY` 是 systemd 场景**自己的**选择器（`M1_ONLY=main`），显式排除。
only_mode_in() {
  awk -F= '$1 ~ /_ONLY$/ && $1 != "M1_ONLY" && $2 == "1" { found = 1 }
           END { exit(found ? 0 : 1) }'
}

# ★ ★ **它必须自证没瞎，每次都证** —— 一个恒答「不是 only 模式」的实现不会让任何东西红。
selftest_only_mode() {
  local rc=0
  printf 'FOO=1\nBAR=2\n'     | only_mode_in && { echo "★ only_mode_in 在没有 *_ONLY 时误报" >&2; rc=1; }
  printf 'CACHE_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 CACHE_ONLY=1" >&2; rc=1; }
  printf 'CACHEDISK_ONLY=1\n' | only_mode_in || { echo "★ only_mode_in 认不出 CACHEDISK_ONLY=1" >&2; rc=1; }
  printf 'RELAY_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 RELAY_ONLY=1" >&2; rc=1; }
  printf 'CACHE_ONLY=0\n'     | only_mode_in && { echo "★ only_mode_in 把 =0 也当成了开" >&2; rc=1; }
  printf 'M1_ONLY=main\n'     | only_mode_in && { echo "★ M1_ONLY 不该让 M1 那四格被跳过" >&2; rc=1; }
  # ★ **新加一个 `*_ONLY` 就要在这里加一条**，否则这个自测覆盖的
  #   仍然是加它之前的那些 —— 与「不自省的名单」同一个形状。
  printf 'MUSL_ONLY=1\n'      | only_mode_in || { echo "★ only_mode_in 认不出 MUSL_ONLY=1" >&2; rc=1; }
  printf 'METRICS_ONLY=1\n'   | only_mode_in || { echo "★ only_mode_in 认不出 METRICS_ONLY=1" >&2; rc=1; }
  printf 'STATS_ONLY=1\n'     | only_mode_in || { echo "★ only_mode_in 认不出 STATS_ONLY=1" >&2; rc=1; }
  [ "$rc" -eq 0 ] || {
    echo "  *_ONLY 判据自测未通过——**本次跑不跑 M1 场景的结论一律不可信**。" >&2
    exit 1
  }
}
selftest_only_mode

# ── ★ ★ 卷名推导的自测 ──────────────────────────────────────────────────────
#
# 四条，**两组各自两个方向**：
#   ① 两棵不同的工作树 ⇒ 两个卷名（这是本次修的那件事）；
#   ② 同一棵工作树两次 ⇒ 同一个卷名 —— 不稳的话每次都从零编，而且没有一行字会说；
#   ③ 换构建镜像仍然换卷 —— 这条性质本来就有（cargo 的 fingerprint 不覆盖 C 工具链），
#      **不许在加树标签的时候把它丢掉**；
#   ④ 名字必须是合法的 docker 卷名。⚠ 这条不是洁癖：名字里一旦混进 `/`，
#      `docker run -v "$名字:/w/target"` 会被当成**绑定挂载**而不是命名卷 —— 照跑、不报错。
selftest_target_vol() {
  local rc=0 a b c d
  a=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-a")
  b=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-b")
  c=$(fulcrum_target_vol "deadbeefcafe1111" "/tmp/fulcrum-selftest-tree-a")
  d=$(fulcrum_target_vol "0000face0000ffff" "/tmp/fulcrum-selftest-tree-a")
  [ "$a" != "$b" ] || { echo "★ 两棵不同的工作树算出了同一个 target 卷名（$a）" >&2; rc=1; }
  [ "$a" = "$c" ]  || { echo "★ 同一棵工作树两次算出了不同的卷名（$a ≠ $c）"    >&2; rc=1; }
  [ "$a" != "$d" ] || { echo "★ 换了构建镜像却还是同一个卷名（$a）——「跟着镜像走」丢了" >&2; rc=1; }
  [[ "$a" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || { echo "★ 算出来的卷名不合法：$a" >&2; rc=1; }
  [ "$rc" -eq 0 ] || {
    echo "  卷名自测未通过——**「本次跑的是这棵树自己的产物」这个前提一律不可信**。" >&2
    exit 1
  }
}
selftest_target_vol

# ── ★ ★ 「这个卷还有没有主人」的自测 ────────────────────────────────────────
#
# 卷按树分开之后多出来一笔磁盘账：工作树是会消失的（`.claude/worktrees/` 下那些用完就删），
# 它们的卷不会。⇒ 新建时把树路径写进 label，回收提示据此把**主人已经不在**的那些单列出来。
#
# 三条，一条一个方向，缺一条都会让回收提示变成一句危险的话：
#   ① 树还在 ⇒ `live`。错成 `gone` 的话，提示会劝人删掉**隔壁正在用的**那个卷 ——
#      而受害者那边只看到一次莫名的全量重编，没有一行字指向这里。
#   ② 树没了 ⇒ `gone`。错成 `live` 的话，这条回收路等于不存在，磁盘继续无声地涨。
#   ③ 没有 label ⇒ `unknown`，**不是** `gone`。「没能检查」不算「检查通过」：
#      加这条规则之前留下的卷、以及手工建的卷（supply-chain.md 里那条 supply-audit
#      命令就手工挂着一个），都不能因为「读不到主人」就被当成无主的。
selftest_tree_state() {
  local rc=0 probe
  probe=$(mktemp -d)
  [ "$(fulcrum_tree_state "$probe")"        = live ]    || { echo "★ 存在的工作树被判成了「不在」——回收提示会去劝人删隔壁正在用的卷" >&2; rc=1; }
  [ "$(fulcrum_tree_state "$probe/gone")"   = gone ]    || { echo "★ 已经不存在的工作树没被判出来——那条回收路等于不存在" >&2; rc=1; }
  [ "$(fulcrum_tree_state "")"              = unknown ] || { echo "★ 空 label（读不到主人）被判成了别的——不明与无主必须分开" >&2; rc=1; }
  rmdir "$probe"
  [ "$rc" -eq 0 ] || {
    echo "  卷归属判定自测未通过——**「哪些卷可以删」这个结论一律不可信**。" >&2
    exit 1
  }
}
selftest_tree_state

# ── ★ ★ 门禁互斥的自测 ──────────────────────────────────────────────────────
#
# 卷分开之后还剩一种：**同一棵树上并发跑两次门禁**，两次仍然共用那一个卷。
# 这里同样两个方向都验，外加两条这把锁特有的失效形态。
#
# ★ 每条都起一个真的子 shell 去调真的 `fulcrum_lock_acquire` —— 不做桩。
#   这把锁在这台宿主上到底成不成立（`flock` 在 MSYS 上根本没有），
#   只有让它真的去 `mkdir` 一次才知道。
selftest_gate_lock() {
  local rc=0 root out dead
  root=$(mktemp -d)

  # ① 锁被一个**活着的**进程拿着 ⇒ 第二次必须失败，并且**说出为什么**。
  #   ★ 拿本进程自己的 pid 当「活着的持有者」：不必起后台进程，而 `kill -0 $$` 必然为真。
  mkdir -p "$root/probe.lock"
  printf '%s\n' "$$" > "$root/probe.lock/pid"
  if out=$(FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD='' \
             bash -c '. "$1" && fulcrum_lock_acquire probe /selftest/tree' _ "$VOL_LOCK_LIB" 2>&1); then
    echo "★ 锁已被占用，fulcrum_lock_acquire 却成功了 —— 这把锁等于不存在" >&2; rc=1
  else
    case "$out" in
      *"另一次门禁正在这棵工作树上跑"*) ;;
      *)
        echo "★ 它拒绝了，却没说出为什么——拿不到锁必须说人话，不是静默失败：" >&2
        printf '%s\n' "$out" >&2; rc=1
        ;;
    esac
  fi
  [ -d "$root/probe.lock" ] || { echo "★ 抢锁失败的那一次把**别人的**锁删掉了" >&2; rc=1; }

  # ② 锁放开之后必须拿得到；★ 并且拿到的那一次**退出时要还回来**。
  #   ⚠ trap 没生效的话，第一次跑完就把这棵树永久锁死了，而症状要到下一次才出现。
  rm -rf "$root/probe.lock"
  if ! out=$(FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD='' \
               bash -c '. "$1" && fulcrum_lock_acquire probe /selftest/tree && [ -d "$2/probe.lock" ]' \
               _ "$VOL_LOCK_LIB" "$root" 2>&1); then
    echo "★ 锁放开之后仍然拿不到（或拿到了却没有建出锁目录）：" >&2
    printf '%s\n' "$out" >&2; rc=1
  fi
  [ ! -d "$root/probe.lock" ] || { echo "★ 持锁的进程退出了、锁却没还回来 —— trap 没生效" >&2; rc=1; }

  # ③ 可重入：`docker-run.sh` 持锁期间要能调 `tests/m1/systemd-run.sh`，
  #   而后者单独跑时必须自己上锁 —— 少了这一条，M1 那一格会被自己锁死。
  mkdir -p "$root/probe.lock"
  printf '%s\n' "$$" > "$root/probe.lock/pid"
  FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD="$root/probe.lock" \
    bash -c '. "$1" && fulcrum_lock_acquire probe /selftest/tree' _ "$VOL_LOCK_LIB" >/dev/null 2>&1 \
    || { echo "★ 已经持锁的进程树里再取一次被挡住了 —— M1 那一格会被自己锁死" >&2; rc=1; }
  rm -rf "$root/probe.lock"

  # ④ 陈旧锁（上一次被 Ctrl-C 掉、没走到 trap）⇒ 接管，并且**说出来**。
  #   ⚠ 先证明这个 pid 真的已经不在——「造不出现场」不算「判据通过」。
  ( : ) &
  dead=$!
  wait "$dead" 2>/dev/null || true
  if kill -0 "$dead" 2>/dev/null; then
    echo "★ 造不出「持有者已死」的现场（pid $dead 居然还在）——本条这次等于没验" >&2; rc=1
  else
    mkdir -p "$root/stale.lock"
    printf '%s\n' "$dead" > "$root/stale.lock/pid"
    if out=$(FULCRUM_LOCK_ROOT="$root" FULCRUM_GATE_LOCK_HELD='' \
               bash -c '. "$1" && fulcrum_lock_acquire stale /selftest/tree' _ "$VOL_LOCK_LIB" 2>&1); then
      case "$out" in
        *接管*) ;;
        *) echo "★ 接管了一把陈旧锁却没说 —— 一次没人知道的接管与没有锁无异" >&2; rc=1 ;;
      esac
    else
      echo "★ 持有者已经不在，锁却接管不了 —— 一次 Ctrl-C 就能把这棵树永久锁死：" >&2
      printf '%s\n' "$out" >&2; rc=1
    fi
  fi

  rm -rf "$root"
  [ "$rc" -eq 0 ] || {
    echo "  门禁互斥自测未通过——**「这棵树上没有第二次门禁在跑」这个前提一律不可信**。" >&2
    exit 1
  }
}
selftest_gate_lock

# 不依赖 git 的兜底扫描：先一次性判「整棵树有没有 CR」（一遍就够，常见情况到此为止），
# 有才逐个找是哪些文件（这时候慢一点无所谓，因为已经要人来看了）。
#
# ★ 全程用 NUL 分隔（`-print0` / `read -d ''`），不用换行——**含换行符的文件名**在按行
#   分隔时会被拆成两个不存在的路径。这与 git 那条路上「按 TAB 取路径」是同一件事：
#   分隔符必须是路径里不可能出现的字节，而换行不是。
crlf_scan_without_git() {
  local find_args=(
    "$REPO_UNIX"
    \( -name .git -o -name target -o -name run -o -name __pycache__ -o -name node_modules \) -prune -o
    -type f ! -name '*.png' ! -name '*.jpg' ! -name '*.webp' ! -name '*.ico' ! -name '*.pdf' -print0
  )
  # 快路径：全树一遍，只问「有没有 CR」。常见情况到此为止，不必逐个开子进程。
  if [ "$(find "${find_args[@]}" | xargs -0 cat 2>/dev/null | tr -dc '\r' | wc -c)" -eq 0 ]; then
    return 0
  fi
  # 有了才逐个找是哪些——这时候慢一点无所谓，因为已经要人来看了。
  local f
  while IFS= read -r -d '' f; do
    [ "$(nul_count "$f")" -eq 0 ] || continue        # 二进制，跳过
    [ "$(cr_count "$f")" -eq 0 ]  || printf '%s\n' "${f#"$REPO_UNIX"/}"
  done < <(find "${find_args[@]}")
}

if command -v git >/dev/null 2>&1 && git -C "$REPO_UNIX" rev-parse --git-dir >/dev/null 2>&1; then
  echo "[docker-run] 行尾检查：git ls-files --eol（权威口径，尊重 .gitattributes）"
  # ★ 路径按 **TAB** 取。`git ls-files --eol` 的每行是
  #     i/<eol><空白>w/<eol><空白>attr/<attr…><空白><TAB><PATH>
  #   —— 路径前那个制表符是唯一可靠的分隔点。
  #   写死列号不行（attr 段字数不定，`attr/text=auto eol=lf` 会被拆成两列）；
  #   `$NF` 同样不行——**含空格的路径会被砍到只剩最后一段**，打出来像个不存在的文件。
  #   w/ 那一列则在 $1 内部按空白再切一刀取，避免 `index()` 式的模糊匹配。
  #   ★ `core.quotePath=false`：否则非 ASCII 路径会被 C-quote 成 "\344\275\240…"，
  #     人拿着它去 `ls` 是对不上的。
  CRLF_FILES=$(git -C "$REPO_UNIX" -c core.quotePath=false ls-files --eol \
                 | awk -F'\t' '{ split($1, a, " "); if (a[2] == "w/crlf") print $2 }')
else
  echo "★ 拿不到 git（没装，或 $REPO_UNIX 不是 git 仓库），改用逐字节扫描。" >&2
  echo "  这条路不认识 .gitattributes，只认 CR 字节；跟踪状态与属性一概不知，可能比权威口径更严。" >&2
  CRLF_FILES=$(crlf_scan_without_git)
fi

if [ -n "$CRLF_FILES" ]; then
  echo "★ 以下文件在**工作树**里是 CRLF，而 .gitattributes 要求无条件 LF：" >&2
  # ★ 一行一个文件，**不靠词拆分**。`printf '%s\n' $CRLF_FILES` 未加引号时，
  #   一个含空格的文件名会被拆成两行，看起来像两个不存在的文件——
  #   而这正是人要拿去排查的那行字。
  printf '%s\n' "$CRLF_FILES" | while IFS= read -r crlf_file; do
    printf '      %s\n' "$crlf_file" >&2
  done
  echo "  一行修复（重新按索引里的 LF 检出）：" >&2
  echo "      git -C \"$REPO_UNIX\" rm --cached -r . >/dev/null && git -C \"$REPO_UNIX\" reset --hard" >&2
  # ★ 这里必须用 printf 而不是 echo：这行文本里带着反斜杠，而 `echo` 展不展开转义
  #   随 shell 而变（bash 内建不展开，dash / sh 的 echo 会展开）。shellcheck SC2028 就是冲它来的。
  printf '  %s\n' '★ 或者只修这几个：用 LF 重写（Windows 版 Python 要按字节写或传 newline="\n"）。' >&2
  exit 1
fi

# ── 仓库里**每一个** target/ 都必须是空的（G107）────────────────
#
# 拦的是这样一件事：**有人在宿主机上跑了 cargo，几个 GB 悄悄长进仓库，而没有人发现。**
#
# ★ 扫的是**仓库里所有的 `target/`**，不是写死的一个路径 ——
#   只看仓库根那一个的话，`vendor/pingora/target/` 里躺着几个 GB 它也不会红。
#   > ★ ★ **反证证的是「它对我塞进去的那个位置分得开好坏」，证不了「该看的位置它都看了」** ——
#   > 覆盖面与灵敏度是两件事，而反证只量得到后者。
#
# ★ ★ **判据是「非空」，不是某个 MB 阈值**：容器把命名卷挂在 `/w/target` 上，
#   Docker 只会在宿主机建一个**空的挂载点目录** ⇒ 空 = 正常，非空 = 有别的东西写过它。
#
# ★ 放在宿主机侧、`docker run` 之前：任何一次调用（含 `LINT_ONLY=1`）都会经过它。
# ⚠ 它**不阻止**长出来，只保证长出来之后活不过下一次门禁 —— 它不是写保护。
# ★ `-prune` 既不下到 `.git`，也不下到已经命中的 target 里（后者在 GB 级目录上真的会慢）。
TARGET_DIRS=$(find "$REPO_UNIX" -maxdepth 4 \
  \( -name .git -o -name node_modules \) -prune -o \
  -type d -name target -print -prune 2>/dev/null || true)

DIRTY_TARGETS=""
while IFS= read -r d; do
  [ -n "$d" ] || continue
  # ★ 判据仍是「非空」而不是某个 MB 阈值：容器只在 /w/target 上挂命名卷，
  #   Docker 只会在宿主机建一个**空的挂载点目录** ⇒ 空 = 正常，非空 = 有别的东西写过它。
  if [ -n "$(ls -A "$d" 2>/dev/null)" ]; then
    DIRTY_TARGETS="${DIRTY_TARGETS}${d}
"
  fi
done <<<"$TARGET_DIRS"

if [ -n "$DIRTY_TARGETS" ]; then
  echo "★ 仓库里有非空的 target/ —— 有东西在这个仓库里构建过，而产物落在了工作树上。" >&2
  echo "  容器只在 /w/target 上挂 docker 命名卷；**别的 target/ 一律直接写在宿主机的仓库里**。" >&2
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    echo "  ── $d（$(du -sh "$d" 2>/dev/null | cut -f1)）" >&2
    du -sh "$d"/* 2>/dev/null | sed 's/^/        /' >&2
  done <<<"$DIRTY_TARGETS"
  echo "  ⚠ G107（PLAN.md §10）：**Rust 一律在 Docker 里跑，不要在宿主机上跑 cargo**；" >&2
  echo "    而在容器里跑 cargo 时也要给 --target-dir，别让它写进 bind mount。" >&2
  echo "  修复（这些目录都在 .gitignore 里，里面没有任何被跟踪的文件）：" >&2
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    echo "      rm -rf \"$d\"" >&2
  done <<<"$DIRTY_TARGETS"
  exit 1
fi

# ★ `MUSL_ONLY=1` 在这里就分岔出去：那一格自己 `docker build` 一个 Alpine 镜像，
#   **既不需要构建镜像，也不需要那次 `docker run`** —— 走到下面只会白等几分钟。
#   ⚠ 上面两道宿主机侧的门（行尾 / target 非空）**故意留在它前面**：
#     它们守的是工作树本身，与跑哪一格无关。
if [ "${MUSL_ONLY:-0}" = "1" ]; then
  bash "$REPO_UNIX/tests/musl/product.sh"
  exit $?
fi

# git-bash 会把容器内的 /w 之类路径改写成 Windows 路径，必须关掉
export MSYS_NO_PATHCONV=1

# 宿主机侧的挂载源必须是 Windows 形式的绝对路径
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO_UNIX")"
else
  REPO_HOST="$REPO_UNIX"
fi
# docker build 的上下文也是宿主机路径，同样要转换
DOCKER_CTX="${REPO_HOST}/docker"

# ── 构建镜像：不存在就做，**内容变了也要重做** ──────────────────────────────────
#
# ★ 只判「镜像在不在」是不够的：Dockerfile 改了也会照样命中旧镜像，此后每一次「全绿」
#   用的都不是仓库声明的那个基础镜像，而且没有任何东西会说出来。
#   ⇒ 把 Dockerfile 的内容哈希写成镜像的 label，每次比对；**任何**改动都会触发重建。
# ★ 拿不到哈希时**强制重建**，不是跳过检查 —— 「没能检查」不算「检查通过」。
LABEL_KEY="cool.cnb.fulcrum.dockerfile-sha256"
DOCKERFILE="${REPO_UNIX}/docker/Dockerfile.build"
if command -v sha256sum >/dev/null 2>&1; then
  DOCKERFILE_SHA=$(sha256sum "$DOCKERFILE" | cut -d' ' -f1)
elif command -v git >/dev/null 2>&1; then
  DOCKERFILE_SHA="githash-$(git hash-object "$DOCKERFILE")"
else
  DOCKERFILE_SHA=""
  echo "⚠ 既没有 sha256sum 也没有 git，无法判断构建镜像是否过期——本次强制重建" >&2
fi

IMAGE_SHA=$(docker image inspect "$IMAGE" --format "{{index .Config.Labels \"$LABEL_KEY\"}}" 2>/dev/null || true)
if [ -z "$DOCKERFILE_SHA" ] || [ "$IMAGE_SHA" != "$DOCKERFILE_SHA" ]; then
  if [ -z "$IMAGE_SHA" ]; then
    echo "[docker-run] building $IMAGE（镜像不存在，或它是加这道检查之前做的）..."
  else
    echo "[docker-run] rebuilding $IMAGE：Dockerfile 变了（镜像里记的是 ${IMAGE_SHA:0:12}…，现在是 ${DOCKERFILE_SHA:0:12}…）"
  fi
  docker build -t "$IMAGE" --label "$LABEL_KEY=$DOCKERFILE_SHA" \
    -f "${DOCKER_CTX}/Dockerfile.build" "$DOCKER_CTX"
fi

# ── 缓存卷 ─────────────────────────────────────────────────────────────────
#
# `fulcrum-cargo`（下载下来的 crate 源码）与工具链无关，**可以跨镜像共享**。
#
# ★ ★ `target` 不行，它必须跟着镜像走。cargo 的 fingerprint 覆盖 rustc 版本与 flags，
#   **但不覆盖 C 工具链** —— build script 产出的东西只在自身声明的输入变化时才重建，
#   于是「换基础镜像但 rustc 不变」会**沿用旧镜像编出来的 C 目标文件**。
#   ⇒ 卷名带上 Dockerfile 的内容哈希：换镜像自动换卷，旧卷留着还能回退。
#
# ★ ★ ★ **它还必须跟着「哪一棵工作树」走。** 上面那条理由一个字都没变，只是它当年
#   成立的前提是「一台机器上只有一棵源码树」——而那个前提今天不成立了：本机有主树
#   加 `.claude/worktrees/` 下若干棵，各自的 `/w` 是不同的源码，`/w/target` 却是同一个卷。
#   ⇒ 卷名再拼上这棵树路径的短哈希，两条性质同时成立（换镜像换卷、换树也换卷）。
#   ⚠ ⚠ 失效形态不是「编得慢」，是**门给出别人家的读数、而两边都不红**：实测撞见两次，
#     一次编译错误指向一个只存在于另一棵树的符号；另一次报「696 条全绿、退出码 0」，
#     跑的却是**没有本次新判据**的旧测试二进制 —— 它不报错，它给一个像样的绿的错答案。
#   推导只有一份（tests/lib/vol-lock.sh），四条自测在本文件开头的 selftest_target_vol。
TREE_TAG="$(fulcrum_tree_tag "$REPO_UNIX")"
TARGET_VOL="$(fulcrum_target_vol "$DOCKERFILE_SHA" "$REPO_UNIX")"

# ── ★ ★ 同一棵树上同一时刻只许跑一次 ────────────────────────────────────────
#
# 卷分开只解决「别的树」。同一棵树上并发跑两次门禁，两次仍然共用这一个卷 —— 回到同一种坏法。
# ⇒ **拿卷之前**先取一把与卷同名的锁；拿不到就当场说清楚并退出，不排队、不静默继续。
# ★ 用 `mkdir` 而不是 `flock`：这台宿主（Windows + Git Bash / MSYS）上没有 flock，
#   写一把其实不生效的锁比没有锁更坏。机制与自测都在 tests/lib/vol-lock.sh。
fulcrum_lock_acquire "$TARGET_VOL" "$REPO_UNIX" || exit 1

docker volume create fulcrum-cargo  >/dev/null
# ★ 新建时把「属于哪棵树」写进 label（见 tests/lib/vol-lock.sh）——
#   下面那条回收提示只有靠它才分得清「主人已经不在」与「主人还在」。
# ⚠ 已存在的卷是**静默 no-op**，label 写不进去；那一类下面按「不明」处理，不当无主的。
VOL_EXISTED=0
docker volume inspect "$TARGET_VOL" >/dev/null 2>&1 && VOL_EXISTED=1
fulcrum_target_vol_create "$TARGET_VOL" "$REPO_UNIX"

# 旧卷不自动删（可能还想回退），但要说出来——不然它们会无声地占满磁盘。
#
# ⚠ ⚠ **只劝人删「本树的」旧卷。** 带上树标签之后，`fulcrum-target-*` 里躺着的多数是
#   **别的工作树正在用的卷**；照旧一股脑列成「旧卷」并附上 `docker volume rm`，
#   那就成了一条教人去刨别人工作树的提示 —— 而受害者那边只会看到一次莫名的全量重编。
#   判据是后缀：`-<本树标签>` 结尾的才是本树的。
ALL_VOLS=$(docker volume ls -q --filter name=fulcrum-target || true)
MINE_STALE=$(printf '%s\n' "$ALL_VOLS" | grep -- "-${TREE_TAG}\$" | grep -v "^${TARGET_VOL}\$" || true)
OTHER_VOLS=$(printf '%s\n' "$ALL_VOLS" | grep -v -- "-${TREE_TAG}\$" | grep -v '^$' || true)
if [ -n "$MINE_STALE" ]; then
  echo "[docker-run] 这棵树另有 $(printf '%s\n' "$MINE_STALE" | wc -l) 个旧的 target 卷（对应更早的构建镜像），确认不再回退就删："
  printf '%s\n' "$MINE_STALE" | sed 's/^/      docker volume rm /'
fi

# ★ ★ 新建的那一次**当场自证** label 真的写进去了、而且读回来就是这棵树。
#   ⚠ 少了这一条，label 悄悄没写成的后果是：下面那条回收路对每个卷都答「不明」，
#     于是它什么也不做，而现象只是「磁盘怎么还在涨」—— 没有一行字会说出原因。
#   ★ 不判红，但要喊（与 cache.sh「灌回之后卷还是空的」同一条纪律）：
#     这是一条回收提示，不是量具本身，为它掐掉整轮门禁不成比例。
if [ "$VOL_EXISTED" = 0 ]; then
  VOL_TREE=$(docker volume inspect --format "{{index .Labels \"$FULCRUM_TREE_LABEL\"}}" "$TARGET_VOL" 2>/dev/null || true)
  [ "$(fulcrum_tree_state "$VOL_TREE")" = live ] || {
    echo "⚠ 刚建出来的卷认不出自己属于哪棵树（label 读回来是 '${VOL_TREE:-<空>}'）——" >&2
    echo "  下面那条「主人已经不在」的回收提示这一轮等于没有（不判红，但别以为它在工作）。" >&2
  }
fi

# ★ ★ **「别的树的卷不给删除命令」解决了误删，没解决磁盘。** 工作树用完就删，
#   它们的卷不会跟着走 —— 一个约 6GB，而卷名后缀是哈希、反推不回路径，
#   于是**没有任何东西说得出哪些已经没有主人了**。
#   ⇒ 按 label 记着的那棵树现在还在不在，把「不属于本树」再分成两半：
#     主人已经不在 ⇒ 给删除命令（删了碰不到任何人）；
#     主人还在、或读不到 label ⇒ 照旧只报数、不给命令。
#   ⚠ 「读不到 label」有意归后一半：加这条规则之前留下的卷读不出主人，
#     而「没能检查」不算「检查通过」。
if [ -n "$OTHER_VOLS" ]; then
  mapfile -t OTHER_LIST <<< "$OTHER_VOLS"
  # 一趟 inspect 问完。★ 卷名在前、路径在最后一段：`IFS='|' read` 把剩下的整段都给
  #   最后一个变量 ⇒ 路径里真有 `|` 也只是这一行显示得难看，不会把卷名读错、
  #   进而给出一条删错卷的命令。
  OTHER_META=$(docker volume inspect --format "{{.Name}}|{{index .Labels \"$FULCRUM_TREE_LABEL\"}}" \
                 "${OTHER_LIST[@]}" 2>/dev/null || true)
  ORPHAN_HINT=""
  ORPHAN_N=0
  KEEP_N=0
  while IFS='|' read -r vol tree; do
    [ -n "$vol" ] || continue
    if [ "$(fulcrum_tree_state "$tree")" = gone ]; then
      ORPHAN_HINT="${ORPHAN_HINT}      docker volume rm ${vol}   # ${tree}"$'\n'
      ORPHAN_N=$((ORPHAN_N + 1))
    else
      KEEP_N=$((KEEP_N + 1))
    fi
  done <<< "$OTHER_META"
  # ⚠ ⚠ **「读不到」不许变成「没这回事」。** `inspect` 整批失败时上面两个计数都是 0，
  #   于是这一段一个字都不打 —— 而它取代的是 a4cc2b5 那句**无条件**的报数，
  #   ⇒ 「另有 N 个卷不属于这棵树」这句话会安静地消失。
  #   把对不上的那些一律并进「不给命令」的那一半（保守方向），并说出发生过。
  OTHER_TOTAL=$(printf '%s\n' "$OTHER_VOLS" | wc -l)
  UNREAD_N=$((OTHER_TOTAL - ORPHAN_N - KEEP_N))
  if [ "$UNREAD_N" -gt 0 ]; then
    echo "⚠ 有 $UNREAD_N 个 target 卷的 label 没读到（docker volume inspect 没给出这几行）——" >&2
    echo "  它们一律按「还有人要」算，不进下面那份删除清单。" >&2
    KEEP_N=$((KEEP_N + UNREAD_N))
  fi
  if [ "$ORPHAN_N" -gt 0 ]; then
    echo "[docker-run] 另有 $ORPHAN_N 个 target 卷，它们的工作树**已经不在了**（一个约 6GB），删了碰不到任何人："
    printf '%s' "$ORPHAN_HINT"
  fi
  if [ "$KEEP_N" -gt 0 ]; then
    echo "[docker-run] 另有 $KEEP_N 个 target 卷**不属于这棵工作树**（别的工作树在用，或是加树标签之前留下的）。"
    echo "             有意不给删除命令：删掉别人正在用的那一个，只会让那棵树莫名其妙地全量重编。"
  fi
fi

# ★ ★ ★ 这里的花括号**不是风格，是判据本身**（实测的一次假绿）。
#
#   原文是：  CMD='cargo build --release --locked || cargo build --release'
#   而后面又拼成：CMD="$LINT_CMD && $CMD"，于是整条链是
#
#       lint && build --locked || build && vendor && m0 && unclaimed
#
#   shell 里 `&&` 与 `||` **同优先级、左结合**，所以它实际被解析成
#
#       ((lint && build --locked) || build) && vendor && m0 && unclaimed
#
#   ★ **lint 一红，左边整个为假，`|| build` 就把它接住了** —— 后面全部照跑，退出码 0。
#     lint 门等于不存在，而且没有任何症状（G44 就是这么红了三次提交没人看见）。
#     第二个后果：lint 一红就悄悄改跑不带 `--locked` 的构建，而 `--locked` 正是
#     G29 第 2 条的执行者。
#
#   修法两条，缺一不可：① 用 `{ ...; }` 把回落绑死在构建这一步上；
#   ② 回落真的发生时**喊出来** —— 一个没人知道发生过的回落，与没有回落无异。
CMD='{ cargo build --release --locked || { echo "⚠ --locked 构建失败，回落到不带 --locked 的构建（Cargo.lock 可能已过期，G29 第 2 条）" >&2; cargo build --release; }; }'

# ★ lint 门：`cargo fmt --check` + `cargo clippy -D warnings`。
#
# ★ 用 `--workspace` 而不是写死包名：**写死的作用域会在加新 crate 时悄悄漏掉它们**。
#
# ★ vendor 那些 `unexpected cfg condition value` 不会把这道门带红（实测）：
#   `--` 之后的 `-D warnings` 只作用于 clippy 直接 lint 的那个 crate，
#   `vendor/pingora` 作为依赖被普通 rustc 编译，它的 warning 就只是 warning。
#
# ★ ★ **shellcheck 与 Rust 那两样同等重要**：本仓库的判据大半写在 shell 里，而 shell 的
#   失败模式是安静的 —— `cmd \n arg`（字面 `\n` 让后面的参数整体错位一个，SC1012）与
#   `printf '%s\n' $LIST`（词拆分劈开含空格的项）两种，`bash -n` 全部放行。
LINT_CMD='cargo fmt --all -- --check'
# ★ ★ `spikes/musl-boringssl` **在根 workspace 之外**（它自己有一个空的 `[workspace]` 表），
#   所以上面那条 `--all` 够不到它 —— 而「够不到」的表现是**绿**。
#   ⇒ 单独给它一条 `--manifest-path`。
#   ⚠ **只给 fmt，不给 clippy**，而这是权衡不是遗漏：clippy 要先把依赖编出来，
#     那意味着**每一次 lint 都要编一遍 BoringSSL**；`cargo fmt` 只读源码，零构建成本。
#     ⇒ 那份探针的 Rust 代码目前**没有 clippy 看着**，已登记在
#     docs/verification/musl-boringssl.md「这份探针证不了什么」一节。
LINT_CMD="$LINT_CMD && cargo fmt --manifest-path spikes/musl-boringssl/Cargo.toml --all -- --check"
LINT_CMD="$LINT_CMD && cargo clippy --workspace --all-targets --locked -- -D warnings"
# ★ ★ shell 那半边**扫哪些文件是推导出来的，不是一张手写清单**。
#
#   原文在这里写死了 18 个 `tests/<x>/*.sh`。⚠ **清单漏一项时没有任何东西会说** ——
#   它只是安静地少扫一个目录，而 lint 照常全绿。实测漏的是 `tests/quic-relay/`，
#   那一格底下压着 4 条真实告警，从来没人看见过。
#   （更早还栽过一次同形状的：tests/m1/lib.sh 带着一处 SC2045 躺进来。）
#   ⚠ **不要顺手把它说成「本来能拦住那处子 shell 缺陷」—— 实测拦不住**，
#     理由与实测记录写在 tests/ci/shellcheck-all.sh 顶部。
#
#   ⇒ 照本文件对 `*_ONLY` 的同一条路子（`only_mode_in` / `selftest_only_mode`）改成
#     **结构性判据 + 每次自证**：判据在 tests/ci/shellcheck-all.sh，它自己拿一棵
#     答案已知的假树钉住枚举器没瞎（含两层深、含空格的路径）。
#   ★ 顺带收掉一条旧的手工纪律：`tests/musl/` 那份探针跑在门禁外，但它**照样被 lint 看**
#     —— 现在这是推导的结果，不再需要谁记得。
# ★ `LC_ALL=C.UTF-8` 那条（中文注释会让 shellcheck 在 POSIX locale 下报不出话来）
#   搬进了那个脚本里，谁来调都逃不掉。
LINT_CMD="$LINT_CMD && bash tests/ci/shellcheck-all.sh"
# ★ ★ CI 那段搬运代码的自证（G94）。**挂在 lint 这一格而不是新开一个场景**：
#   它只花毫秒、不需要 docker、也不需要网络，而且它验的是**门自己的管道**
#   （退出码是怎么取的），与各场景验的产品行为不是一回事。
# ⚠ 它是那条修法在本仓库唯一的判据 —— `dump-cache.sh` 本体只在
#   `cache-hit != 'true'` 时才在 CI 上跑，而缓存键不认源码 ⇒ 往后基本不会再跑。
LINT_CMD="$LINT_CMD && bash tests/ci/dump-cache.sh --self-check"
# ★ ★ ACME 那套**失败现场取证**的判据（`tests/acme/lib.sh` 的 `acme_dump_ports` 一族）。
#   **挂在 lint 这一格而不是 ACME 那一格**，理由与上面那条同源：取证代码只在
#   「已经要红」的路径上执行 ⇒ 一趟绿的 ACME 场景**从来不碰它**，
#   于是它坏了要等到真出事那天才发现，而那一次现场也就跟着白丢了。
#   ⚠ 它不碰 docker、不要产品二进制、只花几百毫秒（自己开一个监听 socket 当靶子）。
LINT_CMD="$LINT_CMD && bash tests/acme/self-check.sh"

if [ "${VENDOR_ONLY:-0}" = "1" ]; then
  # 只跑 fork 回归网。它不依赖 spike 二进制，所以连构建都跳过。
  CMD='bash tests/vendor/run.sh'
elif [ "${UNIT_ONLY:-0}" = "1" ]; then
  CMD='bash tests/unit/run.sh'
elif [ "${SERVE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/serve/run.sh"
elif [ "${L4_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/l4/run.sh"
elif [ "${FILES_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/files/run.sh"
elif [ "${CACHE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/cache/run.sh"
elif [ "${CACHEDISK_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/cache/disk.sh"
elif [ "${ENCODE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/encode/run.sh"
elif [ "${H3_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/h3/run.sh"
elif [ "${PP_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/proxyproto/run.sh"
elif [ "${LOG_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/log/run.sh"
elif [ "${METRICS_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/metrics/run.sh"
elif [ "${STATS_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/stats/run.sh"
elif [ "${RELAY_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/quic-relay/run.sh"
elif [ "${ACME_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/acme/run.sh"
elif [ "${RENEW_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/acme/renew.sh"
elif [ "${LINT_ONLY:-0}" = "1" ]; then
  CMD="$LINT_CMD"
elif [ "${UNCLAIMED_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/m0/unclaimed.sh"
elif [ "${SMOKE_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/smoke/self-check.sh"
elif [ "${STRESS_ONLY:-0}" = "1" ]; then
  CMD="$CMD && bash tests/stress/run.sh"
elif [ "${BUILD_ONLY:-0}" != "1" ]; then
  # ── 顺序的三条原则 ────────────────────────────────────────────────────
  #   ① 粒度细、跑得快的在前（lint → 单测 → fork 回归网），红了指得最准；
  #   ② **验产品的排在验接缝的前面**（数据面 / L4 / 文件 / 缓存 / … 在 M0、M1 之前）——
  #      产品坏了先知道，免得对着一个接缝日志找一个其实在路由层的错；
  #   ③ 依赖别的格成立才有意义的排在后面（QUIC 跨进程转交要 h3 与日志都先成立）。
  [ "${LINT:-1}" = "0" ] || CMD="$LINT_CMD && $CMD"
  [ "${UNIT_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/unit/run.sh"
  [ "${VENDOR_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/vendor/run.sh"
  [ "${SERVE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/serve/run.sh"
  # L4：另一个入口（自建监听器、没有 Host、字节原样搬）。★ 独有判据：换代时长连接不断。
  [ "${L4_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/l4/run.sh"
  # 静态文件。★ 独有判据：路径穿越与 hide 清单 —— 坏掉时服务完全正常，只是多发了几个文件。
  [ "${FILES_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/files/run.sh"
  # 缓存。★ 独有判据：私有内容不许在两个客户端之间串号 —— 坏掉时没有任何日志会说。
  [ "${CACHE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/cache/run.sh"
  # 缓存磁盘后端。★ 独有判据：**把进程杀掉再起来东西还在** ——
  #   内存后端不可能通过它，而它是「从内存来的」与「从磁盘来的」之间唯一不靠自报家门的判据。
  [ "${CACHEDISK_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/cache/disk.sh"
  # 压缩：与缓存耦合（G101 压完再存）⇒ 排在缓存两格之后。★ 独有判据：
  #   次级键写错就是把 gzip 的字节发给不认 gzip 的客户端，而两边的头都完全正常。
  [ "${ENCODE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/encode/run.sh"
  # HTTP/3：同一条执行链换成 QUIC 出去。★ 独有判据：逐跳头在 h3 上禁止（RFC 9114 §4.2）·
  #   `Alt-Svc` 要出现在 h1/h2 的每一条响应上（G110）。
  # ★ 客户端是 curl 的 OpenSSL-QUIC 栈，与被测的 quiche 没有一行共同代码
  #   ⇒ 这是本仓库对 h3 唯一的**互操作**判据。
  [ "${H3_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/h3/run.sh"
  # PROXY protocol（HTTP 面）：连接开头那几十个字节，在 TLS 之前、在 HTTP 之前。★ 独有判据：
  #   ① PROXY 头与请求在同一个 TCP 段里到达（丢掉回还那一步的实现只在合并发送时才坏）；
  #   ② 没配 `proxy_protocol_from` 的实例一个字节都不读 —— 少了它，任何客户端都能自称任意 IP。
  [ "${PP_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/proxyproto/run.sh"
  # 结构化访问日志：响应之外的那一行。★ 独有判据：① 一条请求正好一行；
  #   ② `uri` 是 `rewrite` **之前**那个（取改写后的也编得过、也「有值」，
  #      只是它说出的是一个客户端从没请求过的地址）；③ 日志路径打不开 ⇒ 装载时就红。
  [ "${LOG_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/log/run.sh"
  # Prometheus 指标：响应之外的那份**计数**。★ 排在访问日志**之后**不是风格 ——
  #   它的一致性门拿访问日志当对照物（同一个 `Record::finish` 喂出来的两侧），
  #   日志那一格先成立，这一格红了才指得准。★ 独有判据：
  #   ① 没写 `metrics` 的站点上同一路径**不是**指标（否则「抓到了」证明不了是这条指令干的）；
  #   ② 50 个未知 Host 之后 series 条数不增长**且**那一格正好 +50（只判前半的话，
  #      「计数器根本没在加」也能过）；③ 两个「site」在同一条请求上给出不同的值（G121 / R3）。
  [ "${METRICS_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/metrics/run.sh"
  # `GET /stats` 的**可选字段与退化态**（M2 批 N 任务 7）。★ 有意排在 metrics 之后：
  #   「/stats 抓得到、与 /metrics 同源」由 serve 与 metrics 两格守着，那两格先成立，
  #   这一格红了才指得准。★ 独有判据：① `certs: []`（有解析器零证书）与 `certs: [一张]`
  #   是两个不同的状态——其余两格的实例形态固定，各自只走得到一边；② `cache.entries`
  #   是活体读数（缓存一条之后它真的动），⛔ 不是「这个字段在不在」；
  #   ③ `config_loaded_at_unix` 在一次 `/load` 之后真的变（今天没有任何东西看它会不会动）。
  [ "${STATS_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/stats/run.sh"
  # 换代时的 QUIC 跨进程转交。⚠ 唯一一格会在一次跑里起两代产品进程（SIGQUIT + `-u`）。
  [ "${RELAY_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/quic-relay/run.sh"
  # ACME：产品里唯一一条「要有一个真的对端才算数」的路（G64 加 pebble 的全部理由）。
  [ "${ACME_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/acme/run.sh"
  # 续期是同一条路的另一半（G58「签发**并续期**」）。⚠ 自带 ~65s 等待（要等 ARI 窗口开），
  #   这是本条链里唯一一处「非等不可」的时间，理由写在 tests/acme/renew.sh 顶部。
  [ "${RENEW_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/acme/renew.sh"
  # ★ 冒烟与压力**分工不同**：其余场景验「代码写对了没有」，这两个验「一台跑着的枢衡
  #   现在是不是好的」。门禁里对着自己起的本地实例跑，上线时换个目标 URL 对着真域名跑。
  [ "${SMOKE_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/smoke/self-check.sh"
  [ "${STRESS_TESTS:-1}" = "0" ] || CMD="$CMD && bash tests/stress/run.sh"
  CMD="$CMD && bash tests/m0/run.sh"
  # 未认领 fd：验的是**当前未修行为的复现**，而 M0 验的是产品要保证的行为。
  CMD="$CMD && bash tests/m0/unclaimed.sh"
fi

# ★ 每次都把镜像里**实际**的工具链打出来。§8 要求性能对拍的环境可复现，
#   而可复现的第一步是看得见跑的到底是什么——上面那条 bookworm/trixie 的教训
#   之所以能悄悄躺三天，正是因为没有任何一行输出说过它是哪个基础镜像。
TOOLCHAIN='echo "── 构建镜像工具链 ──"; cat /etc/fulcrum-toolchain 2>/dev/null || echo "（无 /etc/fulcrum-toolchain：这个镜像是加钉子之前做的）"; echo'

# ── ★ 把 192.0.2.0/24 做成真丢包（G41）────────────────────────────────────
#
# 上游的连接超时测试用 `192.0.2.1:79`（RFC 5737 TEST-NET-1）当黑洞，设 1ms 超时
# 等 `ConnectTimedout`。**Docker 默认网络会替它应答**——实测 1.7ms 就 CONNECTED，
# 于是超时永远等不到，6 条测试恒红（其中一条此前被登记成「环境性失败」，根因没查）。
#
# ★ 装上这条 DROP 之后实测：359 passed / 2 failed，已知失败名单 3 → 2 条。
#   **把环境修对，比把名单加长强**——名单越长，门越钝。
#
# ⚠ 需要 NET_ADMIN。这个容器本来就把仓库读写挂进来、以 root 跑，多这一个能力
#   不改变信任边界；换来的是一道真能红的门。
# ⚠ 装不上要**当场红**，不许继续跑——「没能检查」当成「检查通过」是本项目栽过的形状。
#   真正的判据在 tests/vendor/run.sh 的自证步骤里（那里连 `-j DROP` 有没有生效都验）。
BLACKHOLE_CMD='iptables -A OUTPUT -d 192.0.2.0/24 -j DROP'

DOCKER_USER_ARGS=()
[ -z "${DOCKER_USER:-}" ] || DOCKER_USER_ARGS=(--user "$DOCKER_USER")

# ── 把 SMOKE_* / STRESS_* 这些旋钮**透进容器**───────────────────────────────
#
# ★ ★ 批 15 补的，而它是被一次反证逼出来的：
#   `tests/stress/run.sh` 顶上写着一排 `STRESS_DURATION` / `STRESS_MAX_ERRORS` …
#   的用法说明，而**从宿主机这条路进来时它们一个都到不了容器里** ——
#   于是那些旋钮拧了等于没拧，而脚本照样跑、照样报绿。
#   ⚠ 这与「DSL 里认得、却没有任何人读」的那类缺陷是同一个形状：
#     **一个写在文档里、却接不上的开关，比没有这个开关更糟。**
#   （发现它的是一次反证：`STRESS_MAX_FD_GROWTH=-1` 本该让门当场红，结果照样绿。）
#
# ★ 按**前缀**转发，不写死名单：写死的名单在加第 N 个旋钮时没人会想起来去改，
#   于是它会安静地少转发一个。`docker run -e VAR`（不带 =值）取的就是宿主机当前的值。
PASS_ENV_ARGS=()
while IFS='=' read -r k _; do
  case "$k" in
    SMOKE_* | STRESS_*) PASS_ENV_ARGS+=(-e "$k") ;;
  esac
done < <(env)

# -- docs/ bundle 门（宿主机，进容器之前）------------------------------------
#
# ★ 排在最前、且不进容器：它是纯文本检查，几十毫秒，红了指得最准 ——
#   而 Docker 存在的理由是 Linux 特有的 fd 移交与 systemd，跟这道门没有关系。
# ⚠ 镜像里**是有** `/usr/bin/python3` 的（基础镜像带的，并已在 Dockerfile.build 里
#   显式声明）—— 这道门留在宿主机上不是因为容器里跑不了它。
#
# ★★ 它拦的是「新增文档没进 index.md」—— 那类文档对从入口读目录的人
#   等于不存在，而在这道门出现之前没有任何一场看文档。
#
# ★ 它**不是一个场景**，是前置检查——所以各 *_ONLY 模式下照跑（几十毫秒，且红了
#   指得最准，与 lint 排最前是同一个理由）。真不想跑就 DOCS_GATE=0。
#
# ⚠ ⚠ **路径必须用 `$REPO_HOST` 而不是 `$REPO_UNIX`**，而这不是风格问题：
#   本脚本前面已经 `export MSYS_NO_PATHCONV=1`（那是为了让 docker 收到未转换的路径），
#   于是在 Windows + Git Bash 上，原生 `python.exe` 拿到的会是字面的 `/d/Workspace/…`，
#   它按当前盘解析成 `D:\d\Workspace\…` —— 报的是「找不到文件」，
#   而那个路径看起来又很像对的，排查方向会整个跑偏。
#   ★ `REPO_HOST` 是上面用 `cygpath -m` 转过的那一份；**在 Linux 上它与 `REPO_UNIX` 逐字相同**。
#   ⚠ 它与本文件开头那条「`MSYS_NO_PATHCONV=1` 一旦生效，`git rev-parse` 会失败」
#     是**同一个根因**。
# ★ 同一个开关下的第二道：`PLAN.md` §11 的**引用门**（`tools/plan-refs.py`）。
#   它答的是另一个问题 —— ⛔ 不许把两道混着说：
#     · `docs-check.py` 答「新增的文档到不到得了」（bundle 的结构）；
#     · `plan-refs.py` 答「有没有哪一处声称某个 D 号还在 §11 待定清单里，而它已经不在了」。
#   ⚠ 它自己写明了它**答不了**什么（不带 `§11` 字样的散文、跨行的句子、语义错），
#     ⛔ 别把它读成「D 号引用全都有门守着」。
#   ★ 立它的直接原因：2026-09-03 一次体检**成批**读到这样的假话，
#     而在它出现之前，本仓没有任何门看得见一句过期的注释。
#   ⛔ **这里有意不写那批有几处**：一个写在注释里的计数没有任何门守着，错了不会红 ——
#     ⚠ 而这一句的初稿正是这么坏的（写下的数与实测对不上）。**要数就跑这道门。**
if [ "${DOCS_GATE:-1}" = "1" ]; then
  python3 "$REPO_HOST/tools/docs-check.py"
  python3 "$REPO_HOST/tools/plan-refs.py"
fi

docker run --rm \
  "${DOCKER_USER_ARGS[@]}" \
  "${PASS_ENV_ARGS[@]}" \
  --cap-add=NET_ADMIN \
  -v "${REPO_HOST}:/w" \
  -v fulcrum-cargo:/usr/local/cargo/registry \
  -v "${TARGET_VOL}:/w/target" \
  -w /w \
  -e RUST_LOG="${RUST_LOG:-info}" \
  "$IMAGE" \
  bash -c "$TOOLCHAIN; $BLACKHOLE_CMD; $CMD"

# ── 第二十格：产品的 musl 静态产物（D22，owner 拍板）────────────
#
# ★ ★ 它与上面那次 `docker run` 是**两回事**：这一格自己 `docker build` 一个
#   **Alpine** 镜像把产品编成 musl 静态产物，再塞进 `FROM scratch` 里跑一次
#   `fulcrum validate`。⇒ 跑在宿主机侧，挂不进上面那个容器 —— 与 M1 那四格同理。
#
# ⚠ ⚠ **它守的是 G13 的分发口径（「Linux 单静态二进制」），而在它之前那句话没有任何门。**
#   D22 原本登记的是「把 `tests/musl/probe.sh` 挂成常设的门」，而 owner
#   拍板换掉了判据本身：**那份探针编的是 spike，答不了「产物是不是单静态二进制」**
#   （它自己的验证记录第 5 节第 1 条就写着这句话）。⇒ 探针**留在门外**当历史记录。
#
# ★ 实测：冷编一趟 **6m49s**；加了 BuildKit 缓存挂载之后，
#   源码没变的重跑 **7.7s**。⇒ 它挂得起每一次门禁。
if [ "${MUSL_TESTS:-1}" = "1" ] && ! env | only_mode_in; then
  bash "$REPO_UNIX/tests/musl/product.sh"
fi

# ── M1 场景 ────────────────────────────────────────────────────────────────
#
# ★ 它跑在**另一个容器**里（systemd 当 PID 1），所以挂不进上面那次 docker run。
#   驱动在 tests/m1/systemd-run.sh，这里只负责把它接进**唯一那条命令**——
#   一个「要另外记得跑」的场景，与不存在的场景没有区别。
#
# ⚠ 上面那行不能写成 `exec docker run` —— 那样本段永远执行不到；
#   `set -e` 保证前面红了就不会走到这里。
# ★ 「是不是某个 `*_ONLY` 模式」由 `only_mode_in` 结构性地判（定义与自测在本文件开头）。
if [ "${M1_TESTS:-1}" = "1" ] && ! env | only_mode_in; then
  # M1_SKIP_BUILD=1：产物刚刚才在上面构建过，不必再走一遍。
  M1_SKIP_BUILD=1 bash "$REPO_UNIX/tests/m1/systemd-run.sh"
fi
