#!/usr/bin/env bash
# aarch64 那一格什么时候跑（**D24 = 候选 ②**，owner 2026-09-06 拍板）。
#
#   . tests/musl/arm64-trigger.sh          # 被 source，拿三个函数
#   bash tests/musl/arm64-trigger.sh --self-check
#   bash tests/musl/arm64-trigger.sh --hash        # 打当前触发集的哈希
#
# ★ ★ ★ 判据只回答一个问题：**「当前这个依赖状态，有没有被 arm64 真的验证过。」**
#   ⛔ 它不回答「这台机器能不能跑 arm64」（那是 binfmt，机器本地状态，由调用方探）。
#
# ── 触发集为什么是这几个文件 ────────────────────────────────────────────────
#
# `docker/Dockerfile.musl-product` 第 23 行逐字写着「rustc 版本必须与
# `docker/Dockerfile.build`、`docker/Dockerfile.musl-probe` **三者一致**」
# ⇒ `G144`（结案 D24）里那句「三张 Dockerfile」指的就是它们。
#
# ★ 这里用**推导**取它们（`^FROM rust:`），⛔ 不写死清单：写死的清单在出现第四张
#   钉 rustc 的 Dockerfile 时会静静漏掉它，而**漏掉的那一刻不红**
#   —— `tests/ci/shellcheck-all.sh` 为同一条教训改过一次（bench/ 带着五个没扫过的脚本进来）。
#
# ⚠ `Dockerfile.bench` / `Dockerfile.systemd` **有意不在集合里**：它们不参与 musl
#   产物的构建，算进去会让一次 bench 镜像调整白白触发一趟 18 分钟的 arm64 冷建。
#   ★ 这条「有意排除」由自测钉着，⇒ 它是一条判据不是一句注释。
#
# ⚠ ⚠ ★ **源码改动有意不触发，而这是一条判断、不是一条读数。**
#   `Dockerfile.musl-product` 确实是 `COPY . .`，但我们自己那几个 crate 是架构中立的、
#   amd64 那一格每趟都编；aarch64 会坏在**依赖 / 工具链 / 基础镜像**上，那正是这个集合。
#   ⛔ 它错的方向是「漏掉一次源码引起的 arm64 回归」。真出现那么一次时，
#   要改的是**这个集合**，而不是给它加一条豁免。

set -euo pipefail

# ── 触发集：推导，⛔ 不是清单 ───────────────────────────────────────────────
fulcrum_arm64_trigger_files() {
  local root=${1:-.} p
  {
    if [ -f "$root/Cargo.lock" ]; then
      echo "Cargo.lock"
    fi
    # ⚠ `grep -l` 在一个都没匹配上时退出码非 0，而本函数在 `set -e` 下被调用
    #   ⇒ 显式吞掉它的退出码，把「一个都没有」交给自测去判（空集是一种**症状**，
    #     ⛔ 不是一个可以静静通过的答案）。
    for p in $(grep -l '^FROM rust:' "$root"/docker/Dockerfile.* 2>/dev/null || true); do
      printf '%s\n' "${p#"$root"/}"
    done
  } | LC_ALL=C sort
}

# ── 触发集的哈希 ───────────────────────────────────────────────────────────
#
# ★ 路径名与内容**一起**进哈希：只哈希内容的话，把一张 Dockerfile 改名
#   （或删掉一张、加进另一张同内容的）不会让哈希变。
fulcrum_arm64_trigger_hash() {
  local root=${1:-.} f
  fulcrum_arm64_trigger_files "$root" | while IFS= read -r f; do
    printf '%s\n' "$f"
    cat "$root/$f"
  done | sha256sum | cut -d' ' -f1
}

# ── 纯函数：当前哈希 + 验证记录正文 ⇒ 三选一 ─────────────────────────────────
#
#   verified —— 这个依赖状态被 arm64 真的验证过，本趟不必再跑
#   stale    —— 验证过，但验证的是**另一个**依赖状态
#   never    —— 从来没有过一份验证记录
#
# ⚠ `never` 与 `stale` 有意分开：给人的话不一样（一个是「还没有过」，
#   一个是「你刚改了依赖」），而**合成同一个「要跑」并不会让判据更简单**。
fulcrum_arm64_state() {
  local cur=$1 stamp=${2:-} recorded
  recorded=$(printf '%s\n' "$stamp" | sed -n 's/^hash=//p' | head -1)
  if [ -z "$recorded" ]; then
    echo never
  elif [ "$recorded" = "$cur" ]; then
    echo verified
  else
    echo stale
  fi
}

# ── 这台机器跑得了 aarch64 吗（binfmt，**机器本地状态**）────────────────────
#
#   fulcrum_arm64_platform_ok "<docker buildx inspect --bootstrap 的输出>"
#
# ★ 写成**纯函数**（读数由调用方喂进来），理由与 `bench/lib.sh` 那几条逐字相同：
#   直接去问 docker 的判据，只能在**它恰好跑着的那台机器上**被观察到一种结果。
#
# ⚠ ⚠ **诚实交代一条**：容器那一层今天只观察到了**一个方向** —— 2026-09-06 在
#   `ShanirePCX` 上装 binfmt **之前** `docker run --platform linux/arm64` 回
#   `exec format error`、装**之后**回 `aarch64`，两个方向都实测过；但
#   `buildx inspect` 的输出**只在装好之后看过**。⇒ 下面这个函数的**解析**两个方向
#   都由合成输入钉着，而「装之前 buildx 到底怎么打」本机没有观察过。
# ★ 好在误判方向是软的：探针**假阳**（说能跑其实不能）会退化成那趟构建自己报错，
#   ⛔ 只有**假阴**会误拦，而假阴要求 buildx 在装了 binfmt 之后仍不列 arm64。
fulcrum_arm64_platform_ok() {
  case "$1" in
    *linux/arm64*) return 0 ;;
    *) return 1 ;;
  esac
}

# ── 编排要的那个决定（纯函数）───────────────────────────────────────────────
#
#   fulcrum_arm64_decide <state> <platform_ok: yes|no>
#
# 打三选一：`amd64-only` / `both` / `cannot-verify`。
#
# ★ ★ ★ **抽成纯函数是为了让那条红路径可以被执行到。** 把它写在 `docker-run.sh` 的
#   `if/elif/else` 里，`cannot-verify` 那一支在一台装了 binfmt 的机器上**一次都不会跑**
#   —— 而一条从未被执行过的路径不会自己报错：它坏掉（比如少写一个 `exit 1`）的那天，
#   正是它本该拦住东西的那天。⇒ 这里两个方向都由合成输入钉着。
fulcrum_arm64_decide() {
  local state=$1 platform_ok=$2
  case "$state" in
    verified) echo "amd64-only" ;;
    stale | never)
      if [ "$platform_ok" = yes ]; then echo "both"; else echo "cannot-verify"; fi
      ;;
    # ⚠ 认不出来的状态**不许**落进任何一条「照常跑」的分支：
    #   一个 `*)` 兜底到 amd64-only 的实现，会在 `--state` 哪天多一种取值时静默漏跑。
    *) echo "cannot-verify" ;;
  esac
}

# ── 自测：真实仓库 + 合成输入 ───────────────────────────────────────────────
fulcrum_arm64_selfcheck() {
  local rc=0 n=0 out root
  want_eq() { n=$((n + 1)); [ "$3" = "$2" ] || { echo "✗ $1（该是 '$2'，实得 '$3'）" >&2; rc=1; }; }
  want_ne() { n=$((n + 1)); [ "$3" != "$2" ] || { echo "✗ $1（不该等于 '$2'）" >&2; rc=1; }; }
  want_has() { n=$((n + 1)); case "$3" in *"$2"*) ;; *) echo "✗ $1（'$2' 不在里面：$3）" >&2; rc=1 ;; esac; }
  want_hasnt() { n=$((n + 1)); case "$3" in *"$2"*) echo "✗ $1（'$2' 竟然在里面）" >&2; rc=1 ;; esac; }

  # ★ 仓库根可被 `FULCRUM_REPO_ROOT` 覆盖。⚠ 这**不是**调试旋钮，是判据能不能被反证的
  #   前提：反证要把本文件**复制到别处**再注入（⛔ 不改工作树上的原件 —— 这棵树上常年
  #   有别的会话在动），而副本按 `BASH_SOURCE` 推出来的「仓库根」会指向那个临时目录，
  #   于是**基线本身就是红的**，后面每条注入都零判别力。
  local REPO
  REPO=${FULCRUM_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}

  # —— 真实仓库上：★★★ 先证明样本里真有东西 ——
  #   ⚠ 一个静静返回空集的枚举器，会让哈希恒为「空输入的 sha256」⇒ 状态永远 verified，
  #     而那与「一切正常」长得一模一样。
  out=$(fulcrum_arm64_trigger_files "$REPO" | tr '\n' ' ')
  want_has "触发集里没有 Cargo.lock" "Cargo.lock" "$out"
  want_has "触发集里没有 musl-product" "docker/Dockerfile.musl-product" "$out"
  want_has "触发集里没有 musl-probe" "docker/Dockerfile.musl-probe" "$out"
  want_has "触发集里没有 Dockerfile.build" "docker/Dockerfile.build" "$out"
  # ★ 「有意排除」那两张：这条让排除成为**判据**而不是一句注释。
  want_hasnt "Dockerfile.bench 不该在触发集里" "Dockerfile.bench" "$out"
  want_hasnt "Dockerfile.systemd 不该在触发集里" "Dockerfile.systemd" "$out"

  # —— 纯函数三个分支 ——
  want_eq "哈希相同该判 verified" "verified" "$(fulcrum_arm64_state abc "hash=abc")"
  want_eq "哈希不同该判 stale" "stale" "$(fulcrum_arm64_state abc "hash=zzz")"
  want_eq "没有记录该判 never" "never" "$(fulcrum_arm64_state abc "")"
  want_eq "只有注释的记录该判 never" "never" "$(fulcrum_arm64_state abc "# 什么都没有")"
  # ⚠ 记录里混着别的行时也要取得到 hash=（⛔ 别假设它在第一行）。
  want_eq "hash= 不在首行时没取到" "verified" \
    "$(fulcrum_arm64_state abc "$(printf '# 注释\nverified_on=x\nhash=abc\n')")"

  # —— ★★★ 承重：哈希必须对**内容**和**成员集合**都敏感 ——
  #   ⛔ 用合成目录，不动真树。
  root=$(mktemp -d)
  mkdir -p "$root/docker"
  printf 'lock v1\n' > "$root/Cargo.lock"
  printf 'FROM rust:1.0\nRUN a\n' > "$root/docker/Dockerfile.build"
  printf 'FROM rust:1.0\nRUN b\n' > "$root/docker/Dockerfile.musl-product"
  printf 'FROM alpine\n' > "$root/docker/Dockerfile.bench"
  local h0 h1 h2 h3
  h0=$(fulcrum_arm64_trigger_hash "$root")
  printf 'lock v2\n' > "$root/Cargo.lock"
  h1=$(fulcrum_arm64_trigger_hash "$root")
  want_ne "改了 Cargo.lock 哈希却没变" "$h0" "$h1"
  printf 'FROM rust:1.0\nRUN b2\n' > "$root/docker/Dockerfile.musl-product"
  h2=$(fulcrum_arm64_trigger_hash "$root")
  want_ne "改了 Dockerfile 哈希却没变" "$h1" "$h2"
  # ★ 改一张**不在集合里**的 Dockerfile ⇒ 哈希必须**不变**（否则排除是假的）。
  printf 'FROM alpine\nRUN noise\n' > "$root/docker/Dockerfile.bench"
  h3=$(fulcrum_arm64_trigger_hash "$root")
  want_eq "改了 Dockerfile.bench 哈希竟然变了 —— 排除没生效" "$h2" "$h3"
  # ★ 成员集合变了（删掉一张）也必须变哈希。
  rm -f "$root/docker/Dockerfile.musl-product"
  want_ne "删掉一张 Dockerfile 哈希却没变" "$h3" "$(fulcrum_arm64_trigger_hash "$root")"
  rm -rf "$root"

  # —— binfmt 探针的**解析**，两个方向都钉（容器那一层见函数头的交代）——
  n=$((n + 1))
  fulcrum_arm64_platform_ok "Platforms: linux/amd64, linux/amd64/v2, linux/arm64" ||
    { echo "✗ 列了 linux/arm64 却判成跑不了" >&2; rc=1; }
  n=$((n + 1))
  if fulcrum_arm64_platform_ok "Platforms: linux/amd64, linux/amd64/v2, linux/386"; then
    echo "✗ 没列 linux/arm64 却判成跑得了 —— 这个探针恒真，等于没有" >&2
    rc=1
  fi
  # ⚠ 空输入（`docker buildx` 压根没跑起来）必须判**跑不了**，
  #   ⛔ 不许当成「跑得了」——「没能检查」不算「检查通过」。
  n=$((n + 1))
  if fulcrum_arm64_platform_ok ""; then
    echo "✗ 空输入被当成了跑得了" >&2
    rc=1
  fi

  # —— 编排的那个决定：**三条分支各钉一次**（含那条平时跑不到的红路径）——
  want_eq "已验证时不该跑 aarch64" "amd64-only" "$(fulcrum_arm64_decide verified yes)"
  # ★ 已验证 ⇒ 跑不跑得了 aarch64 **不该影响**这个决定（否则没装 binfmt 的机器会被误拦）。
  want_eq "已验证时不该因为跑不了 aarch64 就变卦" "amd64-only" "$(fulcrum_arm64_decide verified no)"
  want_eq "依赖变了且跑得了 ⇒ 该两个架构一起跑" "both" "$(fulcrum_arm64_decide stale yes)"
  want_eq "从没验证过且跑得了 ⇒ 该两个架构一起跑" "both" "$(fulcrum_arm64_decide never yes)"
  # ★ ★ ★ **这两条就是那条平时执行不到的红路径**：依赖变了、而这台机器跑不了 ⇒
  #   必须判「验不了」，⛔ 不许退化成 amd64-only 悄悄放行。
  want_eq "依赖变了但跑不了 ⇒ 必须判验不了" "cannot-verify" "$(fulcrum_arm64_decide stale no)"
  want_eq "从没验证过且跑不了 ⇒ 必须判验不了" "cannot-verify" "$(fulcrum_arm64_decide never no)"
  # ⚠ 认不出来的状态也必须落到验不了，⛔ 不许兜底成「照常跑」。
  want_eq "认不出的状态被兜底成了照常跑" "cannot-verify" "$(fulcrum_arm64_decide 天知道 yes)"

  if [ "$rc" = 0 ]; then
    echo "[arm64-trigger] 自测通过（${n} 条）"
  else
    echo "[arm64-trigger] ★ 自测未通过 —— arm64 触发判据不可信" >&2
  fi
  return "$rc"
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  case "${1:-}" in
    --self-check) fulcrum_arm64_selfcheck ;;
    --hash) fulcrum_arm64_trigger_hash "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" ;;
    --files) fulcrum_arm64_trigger_files "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" ;;
    # `--state <验证记录文件>` ⇒ 打 verified / stale / never。
    # ★ 给 `docker-run.sh` 用的是**子进程**这条路，⛔ 不是 source ——
    #   本文件的自测会定义 `want_*` 这几个名字，source 进去会与调用方的同名函数打架，
    #   而那种打架**不报错**，只是让某一边的断言换了一个实现。
    --state)
      _root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
      _stamp=""
      if [ -n "${2:-}" ] && [ -f "$2" ]; then _stamp=$(cat "$2"); fi
      fulcrum_arm64_state "$(fulcrum_arm64_trigger_hash "$_root")" "$_stamp"
      ;;
    # `--platform-ok` ⇒ 退出码回答「这台机器跑得了 aarch64 吗」。
    --platform-ok)
      fulcrum_arm64_platform_ok "$(docker buildx inspect --bootstrap 2>/dev/null || true)"
      ;;
    # `--decide <验证记录文件>` ⇒ 打 amd64-only / both / cannot-verify。
    # ★ 编排（`tests/m0/docker-run.sh`）只认这三个词，⇒ 那边不再有自己的判断逻辑。
    --decide)
      _root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
      _stamp=""
      if [ -n "${2:-}" ] && [ -f "$2" ]; then _stamp=$(cat "$2"); fi
      _state=$(fulcrum_arm64_state "$(fulcrum_arm64_trigger_hash "$_root")" "$_stamp")
      if fulcrum_arm64_platform_ok "$(docker buildx inspect --bootstrap 2>/dev/null || true)"; then
        _ok=yes
      else
        _ok=no
      fi
      fulcrum_arm64_decide "$_state" "$_ok"
      ;;
    # `--record <文件>` ⇒ 写一份验证记录。
    # ⛔ **只许在一次真的成功的 aarch64 验证之后调用**（调用点在 `tests/m0/docker-run.sh`）。
    # ★ 格式只存在这一处 ⇒ 那个文件是**生成出来的**，⛔ 不是手写的；
    #   一份生成器复现不出来的记录，与一份编造的记录无法区分。
    --record)
      _out=${2:?用法：--record <文件>}
      _root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

      # ── 这份记录是哪一次提交上验的 ────────────────────────────────────────
      #
      # ⚠ ⚠ ★ 原先这里是一句 `git … 2>/dev/null || echo unknown`，于是 git 拒答时
      #   记录里落一个 `verified_commit=unknown`，而**屏幕上一个字都不会有**。
      #   ⇒ 2026-09-08 `61b867a` 那份记录正是这么来的：一份**验证记录**说不出
      #   自己验的是哪一次提交，而当时没有任何东西说过为什么。
      # ★ 现在真因由 git 的原话说，并且**连同原话一起写进记录** —— 一份记录
      #   自己说得出「这一格为什么是空的」，比事后猜强。
      # ⚠ `2>&1 >/dev/null` 只捕 stderr（⛔ 顺序不能反）；`|| _rc=$?` 接住，
      #   ⛔ 否则 `set -e` 会在这里把整个 --record 掐掉，连成功的那趟也写不成记录。
      #
      # ★ ★ ★ **`61b867a` 那个 `unknown` 的真因（2026-09-08 查实并复现）：**
      #   `tests/m0/docker-run.sh:615` 有一句 `export MSYS_NO_PATHCONV=1`（docker 那些
      #   调用要它），而本脚本由该文件在**第 1217 行**调起 ⇒ **继承了这个 export**。
      #   于是 MSYS 不再翻译 argv 里的路径，原生 `git.exe` 拿到字面量 `/d/Workspace/Fulcrum`：
      #     fatal: cannot change to '/d/Workspace/Fulcrum': No such file or directory   (rc=128)
      #   ⚠ 而 `docker-run.sh:495` 那道行尾检查**排在 615 之前**，git 在那里是好的
      #   ⇒ 同一趟门禁里「git 一会儿行一会儿不行」，看起来毫无道理。
      # ⛔ **所以这里不用 `git -C <路径>`** —— 那正是把 MSYS 路径喂给原生 exe 的写法。
      #   改成在子 shell 里 `cd` 过去再问：`cd` 是 bash 内建，⛔ 不经过 argv 转换，
      #   于是这一句在 `MSYS_NO_PATHCONV` 设与不设时**行为相同**。
      #   （同源写法：`tests/lib/vol-lock.sh:168` 的 `git hash-object --stdin` —— 没有路径参数。）
      _git_head() { ( cd "$_root" 2>/dev/null && git rev-parse --short HEAD ); }
      _commit_rc=0
      _commit=$(_git_head 2>/dev/null) || _commit_rc=$?
      _commit_err=''
      if [ "$_commit_rc" -ne 0 ]; then
        _commit_err=$(_git_head 2>&1 >/dev/null) || true
        _commit=unknown
        echo "⚠ ⚠ 写 aarch64 验证记录时**问不出当前提交** —— 这一格会是 \`unknown\`。" >&2
        echo "  \`(cd $_root && git rev-parse --short HEAD)\` 退了 $_commit_rc，它自己的原话：" >&2
        printf '%s\n' "$_commit_err" | sed 's/^/      /' >&2
        echo "  ⛔ 这**不**使这份记录作废（它的主键是 hash=），但补记时别去猜那个提交号。" >&2
      fi

      {
        echo "# aarch64 已验证记录（\`D24\` = 候选 ②）"
        echo "#"
        echo "# ⛔ ⛔ **别手写这个文件。** 它由 \`tests/m0/docker-run.sh\` 在一次**真的成功的**"
        echo "#   aarch64 验证之后、经 \`arm64-trigger.sh --record\` 写入。手写等于伪造一份验证"
        echo "#   记录 —— 而本仓判据的全部意义就在那句「判据不是有没有人说做过了」。"
        echo "#"
        echo "# 它回答的问题只有一个：**当前这个依赖状态，有没有被 aarch64 真的验证过。**"
        echo "# \`hash=\` 是 \`bash tests/musl/arm64-trigger.sh --hash\` 对**触发集**算出来的值；"
        echo "# 触发集 = \`Cargo.lock\` + 那三张钉 rustc 的 Dockerfile（**推导**得出，⛔ 不是手写"
        echo "# 清单，见 \`tests/musl/arm64-trigger.sh\` 的文件头）。"
        echo "#"
        echo "# ⇒ 这个值变了 = 依赖 / 工具链 / 基础镜像变了 = aarch64 那一格要重跑。"
        echo "#   ⚠ 它**不**随源码提交变：我们自己那几个 crate 是架构中立的，而 amd64 那一格每趟都编。"
        echo "#"
        echo "# ⚠ ⚠ 墙钟是**机器本地**的：2026-09-06 \`ShanirePCX\` 冷建 34m05s，而 \`ShanireHomePC\`"
        echo "#   2026-09-05 同样一趟 18m07s —— 近两倍差距，⛔ 别互相照抄、也别当成「这道门要多久」。"
        echo
        echo "hash=$(fulcrum_arm64_trigger_hash "$_root")"
        echo "verified_at=$(date '+%Y-%m-%dT%H:%M:%S%z')"
        echo "verified_on=$(hostname)"
        echo "verified_commit=$_commit"
        # ⚠ 只有问不出来的时候才有这一段：把 git 的原话原样留在记录里。
        #   ⛔ 别把它写成 `verified_commit=` 的值 —— 解析 `hash=` 的是 sed，
        #   而这几行以 `#` 开头，对解析器天然透明（`--self-check` 里那条
        #   「hash= 不在首行时也取得到」正是钉这件事的）。
        if [ -n "$_commit_err" ]; then
          echo "#"
          echo "# ⚠ verified_commit 问不出来（git 退 $_commit_rc），它自己的原话："
          printf '%s\n' "$_commit_err" | sed 's/^/#     /'
        fi
      } > "$_out"
      ;;
    *)
      echo "用法：bash tests/musl/arm64-trigger.sh --self-check | --hash | --files | --state <文件> | --platform-ok" >&2
      exit 2
      ;;
  esac
fi
