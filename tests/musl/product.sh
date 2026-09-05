#!/usr/bin/env bash
# **产品**的 musl 静态产物自证（D22，owner 拍板）。
#
#   bash tests/musl/product.sh                 # 正向 + 反证
#   ARCHES="amd64" bash tests/musl/product.sh  # 只跑一个架构（默认就是它）
#   REVERSE=0     bash tests/musl/product.sh   # 跳过反证（★ 平时不要跳）
#
# ── 它与 `tests/musl/probe.sh` 的分工（★ 这一条是全部意义所在）───────────────
#
# `probe.sh` 编的是 **spike**（`spikes/musl-boringssl`），本脚本编的是 **产品**
# （`crates/fulcrum` 的那个 bin）。D22 想守的那句话是 **G13 的分发口径**
# ——「Linux x86_64 + aarch64 单静态二进制」——**而探针答不了它**：
# 它自己的验证记录第 5 节第 1 条就写着「『探针编得出来』证不了『枢衡编得出来』」。
# ⇒ owner 拍板把判据换成产品自证；探针**留在门外**当历史记录。
#
# ── 两个方向，缺一不可 ──────────────────────────────────────────────────────
#
# 正向：`+crt-static` 的产物 `INTERP=0 / NEEDED=0`，且在 `FROM scratch` 里
#       真的跑完一次 `fulcrum validate`（四层：诊断 → 结构化 → 运行时图 → TLS 装载）。
# 反向①（**同一份 Dockerfile，翻成 `-crt-static`**）：动态版必须在 scratch 里跑不起来。
#       ★ 分不开好坏两种情况的尺子不是尺子。
# 反向②（**同一个镜像，喂一份必须被拒的配置**）：退出码非 0，且说得出那句专门的诊断。
#       ★ ★ 它守的是「退出码 0」这件事本身 —— 一个**启动就 exit(0)**、根本没读配置的
#       二进制，在正向那一格里与真的跑完四层**完全一样**。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# ★ MSYS 会把容器内路径改写成 Windows 路径，必须关掉；理由与 probe.sh 逐字相同。
export MSYS_NO_PATHCONV=1
# ⚠ 关掉之后宿主机侧的路径就没人替你转了 —— docker 是 Windows 程序。
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO")"
else
  REPO_HOST="$REPO"
fi

DOCKERFILE="docker/Dockerfile.musl-product"
# ⚠ ⚠ **上下文是仓库根**：根 `Cargo.toml` 的 `[patch.crates-io]` 指着 `vendor/pingora`。
#   仓库根那份 `.dockerignore` 负责把 `target/` 挡在上下文之外。
CONTEXT="."
# ★ 默认只跑 amd64。aarch64 要在 qemu 上编**整个产品**（不只是探针那点代码），
#   ⏳ 何时把它也挂上今天挂在 **D24** 名下（D22 本身已由 G108 结案）—— 写在这里而不是留白。
ARCHES=${ARCHES:-"amd64"}
REVERSE=${REVERSE:-1}

FAILS=0
ok() { echo "  ✓ $*"; }
fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}

expect_machine() {
  case "$1" in
  amd64) echo "Advanced Micro Devices X86-64" ;;
  arm64) echo "AArch64" ;;
  *) echo "?" ;;
  esac
}

# 从一个 `FROM scratch` 镜像里取文件。
# ★ 不能 `docker run cat` —— 那个镜像里没有 cat、没有 sh，什么都没有。
extract() {
  local image=$1 src=$2 dst=$3 cid host_dst
  if command -v cygpath >/dev/null 2>&1; then
    host_dst="$(cygpath -m "$dst")"
  else
    host_dst="$dst"
  fi
  cid=$(docker create "$image")
  docker cp "$cid:$src" "$host_dst" >/dev/null
  docker rm -f "$cid" >/dev/null
}

field() { sed -n "s/^$2=//p" "$1"; }

build_one() {
  local arch=$1 crt=$2 tag=$3
  echo "[product] docker build --platform linux/$arch（$crt）…"
  # ⚠ `--pull=false`：基础镜像已按 digest 钉死，不必每次去问 registry。
  docker build \
    --platform "linux/$arch" \
    --pull=false \
    --build-arg "CRT_STATIC=$crt" \
    -f "$REPO_HOST/$DOCKERFILE" \
    -t "$tag" \
    "$REPO_HOST/$CONTEXT"
}

for arch in $ARCHES; do
  echo
  echo "=== [$arch] 产品编成 musl 静态产物，并在 FROM scratch 里真跑 ==="
  tag="fulcrum-musl-product:$arch"

  # ⚠ 这句话**不许写成「BoringSSL 编不出来」**：产品图里有一整票 `*-sys`，
  #   而 `docker build` 还会因为上下文、磁盘、网络失败。
  #   ★ 一句把所有失败都归到同一个原因上的报错，比没有报错更能误导下一个人。
  if ! build_one "$arch" "+crt-static" "$tag"; then
    fail "[$arch] docker build 没过 —— 具体原因看上面 buildkit 的输出，不要预设是哪个依赖"
    continue
  fi
  ok "[$arch] 产品在 musl 上编出来了（整个产品图，不只是 BoringSSL）"

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
  echo "[product] 大小 $(field "$ev" BYTES) 字节"

  # ── ★ ★ ★ D23：产物里真的链接了哪几套 TLS ────────────────────────────────
  #
  # ⚠ 它与供应链那两道门问的**不是同一个问题**：门 4 看 `Cargo.lock`、门 5 看依赖图，
  #   而「图里有、产物里其实没链接」是门 5 **原理上**看不见的那一半。
  # ★ 判据的推导在 `tests/ci/tls-linkage.sh` 的文件头，这里只做判定。
  tls_nm=$(field "$ev" TLS_NM)
  tls_ctrl=$(field "$ev" TLS_PROBE_CONTROL)
  # ⚠ ⚠ **先判「量到了没有」**：读不到符号表时下面每一条「必须为 0」都会凭空全绿。
  #   ⇒ 「没能检查」不算「检查通过」（与本仓其它几处同一条纪律）。
  if [ "$tls_nm" != "ok" ]; then
    fail "[$arch] 取不到符号表（TLS_NM=$tls_nm）⇒ 下面那组 TLS 链接判据一律不可信"
  elif [ -z "$tls_ctrl" ] || [ "$tls_ctrl" -eq 0 ]; then
    # ★ 扫描器自证：一个写错的模式与「真的一个都没有」给出完全相同的 0。
    fail "[$arch] 扫描器自证没过（TLS_PROBE_CONTROL=$tls_ctrl）⇒ 那组判据分不出真假"
  else
    if [ "$(field "$ev" TLS_BORINGSSL_ONLY)" -eq 3 ]; then
      ok "[$arch] 产物里有 BoringSSL 独有的 3 个符号（含 G6 那条共用回调的执行者）"
    else
      fail "[$arch] BoringSSL 独有符号只找到 $(field "$ev" TLS_BORINGSSL_ONLY)/3 —— TLS 后端不是 BoringSSL？"
    fi
    if [ "$(field "$ev" TLS_OPENSSL_ONLY)" -eq 0 ]; then
      ok "[$arch] 产物里没有 OpenSSL 独有的符号"
    else
      fail "[$arch] 产物里出现了 OpenSSL 独有符号（$(field "$ev" TLS_OPENSSL_ONLY) 个）—— G6 第 1 条被破坏"
    fi
    if [ "$(field "$ev" TLS_RUSTLS_IMPL)" -eq 0 ]; then
      ok "[$arch] rustls 本体没有被链接进来（rustls_pki_types 有 $(field "$ev" TLS_RUSTLS_PKI_TYPES) 个符号，是允许的：纯类型 crate）"
    else
      fail "[$arch] rustls **本体**被链接进产物了（$(field "$ev" TLS_RUSTLS_IMPL) 个符号）—— G6 第 1 条被破坏"
    fi
  fi
  rm -f "$ev"

  # ── 正向：在什么都没有的镜像里跑一次 validate ──────────────────────────
  out="$(mktemp)"
  if docker run --rm --platform "linux/$arch" "$tag" >"$out" 2>&1; then
    run_rc=0
  else
    run_rc=$?
  fi
  sed 's/^/    | /' "$out"
  if [ "$run_rc" -eq 0 ]; then
    ok "[$arch] 在 FROM scratch 里跑通：validate 四层全过，退出码 0"
  else
    fail "[$arch] 在 FROM scratch 里跑失败，退出码 $run_rc"
  fi
  rm -f "$out"

  # ── 反向②：同一个镜像，喂一份必须被拒的配置 ────────────────────────────
  #
  # ★ ★ 它守的是**上面那个「退出码 0」本身**：一个启动就 exit(0)、根本没读配置的
  #   二进制，在正向那一格里长得一模一样。
  # ⚠ scratch 里没有 shell，但 `--entrypoint` 走的是 execve，不需要 shell。
  bad="$(mktemp)"
  if docker run --rm --platform "linux/$arch" --entrypoint /fulcrum "$tag" \
    validate /bad.Fulcrumfile >"$bad" 2>&1; then
    bad_rc=0
  else
    bad_rc=$?
  fi
  if [ "$bad_rc" -eq 0 ]; then
    fail "[$arch] 一份该被拒的配置竟然通过了 —— 那说明正向那条「退出码 0」什么都没证明"
    sed 's/^/    | /' "$bad"
  elif grep -q '整块删除' "$bad"; then
    ok "[$arch] 坏配置被拒，且给的是那条**专门的**诊断（退出码 $bad_rc）—— 编译层真的跑了"
  else
    fail "[$arch] 坏配置是被拒了（退出码 $bad_rc），但给的不是那条专门的诊断：$(head -3 "$bad" | tr '\n' ' ')"
  fi
  rm -f "$bad"
done

# ── 反向①：同一份 Dockerfile，把静态翻成动态 ──────────────────────────────
if [ "$REVERSE" = "1" ]; then
  echo
  echo "=== [反证] 动态链接的同一份产物必须在 scratch 里跑不起来 ==="
  rtag="fulcrum-musl-product:reverse"
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
  echo "MUSL PRODUCT PASSED —— 产品本体编成了 musl 单静态二进制，并在一个什么都没有的镜像里跑完了 validate。"
  echo "  ⚠ 它**证不了**的：产品功能对不对（那是容器里那些场景的事，它们跑的是 glibc 产物）；"
  echo "    也证不了 aarch64（本格默认只跑 amd64，见脚本顶部）。"
else
  echo "MUSL PRODUCT FAILED（$FAILS 条）" >&2
  exit 1
fi
