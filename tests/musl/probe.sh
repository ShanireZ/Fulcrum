#!/usr/bin/env bash
# musl + BoringSSL 静态链接探针的驱动（G103/G104 的未验前置，PLAN.md §10）。
#
#   bash tests/musl/probe.sh                 # 两个架构 + 反证
#   ARCHES="amd64" bash tests/musl/probe.sh  # 只跑一个架构
#   REVERSE=0     bash tests/musl/probe.sh   # 跳过反证（★ 平时不要跳）
#
# ★ ★ **它不在门禁里，这是决定不是遗漏**（G108）：本脚本编的是 **spike**，答不了
#   「**产物**是不是单静态二进制」（G13 的分发口径）。常设的那一格是
#   `tests/musl/product.sh`，它编的是**产品本体**；本脚本留在门外当历史记录，需要时手工跑。
#
# ⚠ **要重跑它的人先知道**：它偶发红在 `apk add … clang-dev` 上，重跑即过。
#   根因是**基础镜像按 digest 钉死、而那几个 apk 包一个没钉** —— 它拉的是 Alpine 的活仓库。
#
# ── 这份判据要分开的两种情况 ────────────────────────────────────────────────
# ★ ★ 「链接成功」与「跑得起来」是两件事，而它们在 `cargo build` 那一层长得一样。
#   所以产物**不是**拿 `file(1)` 看一眼就算数的：它被塞进一个 `FROM scratch`
#   镜像里真的跑一遍 —— 那个镜像里**除了这个二进制什么都没有**，
#   动态链接的东西在那里会死在「找不到解释器」上。
#   ⇒ 这同时是 §6.2 「不做官方容器镜像，文档给一份 FROM scratch Dockerfile 代替」
#     那句话的字面检验。
# ★ 反证走同一份 Dockerfile，只把 `+crt-static` 翻成 `-crt-static`：
#   那一趟**必须**红在 scratch 里跑不起来。分不开好坏两种情况的尺子不是尺子。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# ★ MSYS 会把 `/probe`、`/evidence.txt` 这类**容器内**路径改写成 Windows 路径，必须关掉。
export MSYS_NO_PATHCONV=1
# ⚠ ⚠ 关掉之后**宿主机侧的路径就没人替你转了**：docker 是 Windows 程序，
#   MSYS 那种 `/d/…` 形式的路径它认不得（实测报的是 `unable to prepare context: path not found`）。
#   ⇒ 与 tests/m0/docker-run.sh 同一处理：宿主机路径统一走 cygpath 转成 `D:/…`。
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO")"
else
  REPO_HOST="$REPO"
fi

DOCKERFILE="docker/Dockerfile.musl-probe"
CONTEXT="spikes/musl-boringssl"
ARCHES=${ARCHES:-"amd64 arm64"}
REVERSE=${REVERSE:-1}

FAILS=0
ok() { echo "  ✓ $*"; }
fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}

# 架构 → 期望的 ELF Machine 字段与目标三元组。
expect_machine() {
  case "$1" in
  amd64) echo "Advanced Micro Devices X86-64" ;;
  arm64) echo "AArch64" ;;
  *) echo "?" ;;
  esac
}
expect_triple() {
  case "$1" in
  amd64) echo "x86_64-unknown-linux-musl" ;;
  arm64) echo "aarch64-unknown-linux-musl" ;;
  *) echo "?" ;;
  esac
}

# 从一个 `FROM scratch` 镜像里取文件。
# ★ 不能 `docker run cat` —— 那个镜像里没有 cat，没有 sh，什么都没有。
#   `docker create` + `docker cp` 走的是镜像层，不需要镜像里有任何程序。
extract() {
  local image=$1 src=$2 dst=$3 cid host_dst
  # ⚠ `docker cp` 的**目的地**是宿主机路径，同样得转 —— 不转的话 `/tmp/tmp.XXXX`
  #   会被 Windows 版 docker 解成 `<当前盘>:\tmp\…`（实测报 `directory "D:\tmp"
  #   does not exist`）。★ 这与上面 build context 那条是同一个坑的两处发作。
  if command -v cygpath >/dev/null 2>&1; then
    host_dst="$(cygpath -m "$dst")"
  else
    host_dst="$dst"
  fi
  cid=$(docker create "$image")
  docker cp "$cid:$src" "$host_dst" >/dev/null
  docker rm -f "$cid" >/dev/null
}

# 读 `键=值` 证据文件里的一格。
field() { sed -n "s/^$2=//p" "$1"; }

build_one() {
  local arch=$1 crt=$2 tag=$3
  echo "[probe] docker build --platform linux/$arch（$crt）…"
  # ⚠ `--pull=false`：基础镜像已按 digest 钉死，不需要每次去问 registry；
  #   而在这台机器上那一问经常要走代理，失败起来像是构建坏了。
  docker build \
    --platform "linux/$arch" \
    --pull=false \
    --build-arg "CRT_STATIC=$crt" \
    -f "$REPO_HOST/$DOCKERFILE" \
    -t "$tag" \
    "$REPO_HOST/$CONTEXT"
}

# ── 正面：每个架构一格 ──────────────────────────────────────────────────────
for arch in $ARCHES; do
  echo
  echo "=== [$arch] 静态链接 + 在 FROM scratch 里真跑 ==="
  tag="fulcrum-musl-probe:$arch"

  # ⚠ 这句话**不许写成「BoringSSL 编不出来」**：`docker build` 会因为很多与
  #   BoringSSL 无关的原因失败（上下文路径、拉不到基础镜像、磁盘满）。
  #   ★ 一句把所有失败都归到同一个原因上的报错，比没有报错更能误导下一个人。
  if ! build_one "$arch" "+crt-static" "$tag"; then
    fail "[$arch] docker build 没过 —— 具体原因看上面 buildkit 的输出，不要预设是 BoringSSL"
    continue
  fi
  ok "[$arch] 构建通过：BoringSSL 在 musl 上编出来了，且 Rust 侧链得上"

  ev="$(mktemp)"
  extract "$tag" /evidence.txt "$ev"

  got_machine=$(field "$ev" MACHINE)
  want_machine=$(expect_machine "$arch")
  if [ "$got_machine" = "$want_machine" ]; then
    ok "[$arch] ELF Machine = $got_machine"
  else
    fail "[$arch] ELF Machine 期望「$want_machine」，实际「$got_machine」"
  fi

  # ★ 主判据。完全静态 ⇒ 没有 INTERP 段、没有 NEEDED 条目。
  #   两个都看：只看 INTERP 会漏掉「有动态段但没解释器」这种畸形产物。
  interp=$(field "$ev" INTERP)
  needed=$(field "$ev" NEEDED)
  if [ "$interp" = "0" ] && [ "$needed" = "0" ]; then
    ok "[$arch] 完全静态：INTERP=0、NEEDED=0（$(field "$ev" FILE)）"
  else
    fail "[$arch] 不是完全静态：INTERP=$interp、NEEDED=$needed"
  fi

  echo "[probe] 大小 $(field "$ev" BYTES) 字节"
  rm -f "$ev"

  # ── 真跑。★ 镜像里除了这个二进制什么都没有。 ──────────────────────────
  out="$(mktemp)"
  if docker run --rm --platform "linux/$arch" "$tag" >"$out" 2>&1; then
    run_rc=0
  else
    run_rc=$?
  fi
  sed 's/^/    | /' "$out"

  if [ "$run_rc" -ne 0 ]; then
    fail "[$arch] 在 FROM scratch 里跑失败，退出码 $run_rc"
  elif ! grep -q '^PROBE OK$' "$out"; then
    fail "[$arch] 跑完了但没有打出 PROBE OK"
  else
    ok "[$arch] 在 FROM scratch 里跑通：QUIC 握手 + 1-RTT 数据 + 证书回调"
  fi

  # ★ 让二进制自己说出它是给谁编的，与 ELF 头读出来的对一遍 ——
  #   交叉编译时「以为在编 A、实际编的是 B」两边都不会报错。
  want_triple=$(expect_triple "$arch")
  if grep -q "目标三元组：$want_triple" "$out"; then
    ok "[$arch] 二进制自报的目标三元组 = $want_triple"
  else
    fail "[$arch] 二进制自报的三元组不是 $want_triple"
  fi
  rm -f "$out"
done

# ── 反证：同一份 Dockerfile，把静态翻成动态 ────────────────────────────────
if [ "$REVERSE" = "1" ]; then
  echo
  echo "=== [反证] 动态链接的同一份产物必须在 scratch 里跑不起来 ==="
  rtag="fulcrum-musl-probe:reverse"
  if ! build_one amd64 "-crt-static" "$rtag"; then
    fail "[反证] 连动态版都构建不出来 —— 这道反证本身失效了，别把它当通过"
  else
    ev="$(mktemp)"
    extract "$rtag" /evidence.txt "$ev"
    interp=$(field "$ev" INTERP)
    if [ "$interp" = "1" ]; then
      ok "[反证] 动态版确实带着 INTERP 段（尺子分得开这两种情况）"
    else
      fail "[反证] 动态版的 INTERP=$interp —— 这把尺子在两种情况下读数相同"
    fi
    rm -f "$ev"

    out="$(mktemp)"
    if docker run --rm --platform linux/amd64 "$rtag" >"$out" 2>&1; then
      fail "[反证] 动态版竟然在 FROM scratch 里跑通了 —— 那说明这一格根本没在验静态"
      sed 's/^/    | /' "$out"
    else
      ok "[反证] 动态版在 FROM scratch 里跑不起来（$(head -1 "$out" | cut -c1-120)）"
    fi
    rm -f "$out"
  fi
fi

echo
if [ "$FAILS" -eq 0 ]; then
  echo "MUSL PROBE PASSED"
else
  echo "MUSL PROBE FAILED（$FAILS 条）" >&2
  exit 1
fi
