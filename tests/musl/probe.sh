#!/usr/bin/env bash
# musl + BoringSSL 靜態連結探針的驅動（G103/G104 的未驗前置，PLAN.md §10）。
#
#   bash tests/musl/probe.sh                 # 兩個架構 + 反證
#   ARCHES="amd64" bash tests/musl/probe.sh  # 只跑一個架構
#   REVERSE=0     bash tests/musl/probe.sh   # 跳過反證（★ 平時不要跳）
#
# ★ ★ **它不在門禁裡，這是決定不是遺漏**（G108）：本指令碼編的是 **spike**，答不了
#   「**產物**是不是單靜態二進位」（G13 的散佈口徑）。常設的那一格是
#   `tests/musl/product.sh`，它編的是**產品本體**；本指令碼留在門外當歷史記錄，需要時手動跑。
#
# ⚠ **要重跑它的人先知道**：它偶發紅在 `apk add … clang-dev` 上，重跑即過。
#   根因是**基礎鏡像按 digest 釘死、而那幾個 apk 包一個沒釘** —— 它拉的是 Alpine 的活儲存庫。
#
# ── 這份判據要分開的兩種情況 ────────────────────────────────────────────────
# ★ ★ 「連結成功」與「跑得起來」是兩件事，而它們在 `cargo build` 那一層長得一樣。
#   所以產物**不是**拿 `file(1)` 看一眼就算數的：它被塞進一個 `FROM scratch`
#   鏡像裡真的跑一遍 —— 那個鏡像裡**除了這個二進位什麼都沒有**，
#   動態連結的東西在那裡會死在「找不到解釋器」上。
#   ⇒ 這同時是 §6.2 「不做官方容器鏡像，文件給一份 FROM scratch Dockerfile 代替」
#     那句話的字面檢驗。
# ★ 反證走同一份 Dockerfile，只把 `+crt-static` 翻成 `-crt-static`：
#   那一趟**必須**紅在 scratch 裡跑不起來。分不開好壞兩種情況的尺子不是尺子。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# ★ MSYS 會把 `/probe`、`/evidence.txt` 這類**容器內**路徑改寫成 Windows 路徑，必須關掉。
export MSYS_NO_PATHCONV=1
# ⚠ ⚠ 關掉之後**宿主機側的路徑就沒人替你轉了**：docker 是 Windows 處理程序，
#   MSYS 那種 `/d/…` 形式的路徑它認不得（實測報的是 `unable to prepare context: path not found`）。
#   ⇒ 與 tests/m0/docker-run.sh 同一處理：宿主機路徑統一走 cygpath 轉成 `D:/…`。
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

# 架構 → 期望的 ELF Machine 欄位與目標三元組。
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

# 從一個 `FROM scratch` 鏡像裡取檔案。
# ★ 不能 `docker run cat` —— 那個鏡像裡沒有 cat，沒有 sh，什麼都沒有。
#   `docker create` + `docker cp` 走的是鏡像層，不需要鏡像裡有任何處理程序。
extract() {
  local image=$1 src=$2 dst=$3 cid host_dst
  # ⚠ `docker cp` 的**目的地**是宿主機路徑，同樣得轉 —— 不轉的話 `/tmp/tmp.XXXX`
  #   會被 Windows 版 docker 解成 `<当前盘>:\tmp\…`（實測報 `directory "D:\tmp"
  #   does not exist`）。★ 這與上面 build context 那條是同一個坑的兩處發作。
  if command -v cygpath >/dev/null 2>&1; then
    host_dst="$(cygpath -m "$dst")"
  else
    host_dst="$dst"
  fi
  cid=$(docker create "$image")
  docker cp "$cid:$src" "$host_dst" >/dev/null
  docker rm -f "$cid" >/dev/null
}

# 讀 `键=值` 證據檔案裡的一格。
field() { sed -n "s/^$2=//p" "$1"; }

build_one() {
  local arch=$1 crt=$2 tag=$3
  echo "[probe] docker build --platform linux/$arch（$crt）…"
  # ⚠ `--pull=false`：基礎鏡像已按 digest 釘死，不需要每次去問 registry；
  #   而在這臺機器上那一問經常要走代理，失敗起來像是建置壞了。
  docker build \
    --platform "linux/$arch" \
    --pull=false \
    --build-arg "CRT_STATIC=$crt" \
    -f "$REPO_HOST/$DOCKERFILE" \
    -t "$tag" \
    "$REPO_HOST/$CONTEXT"
}

# ── 正面：每個架構一格 ──────────────────────────────────────────────────────
for arch in $ARCHES; do
  echo
  echo "=== [$arch] 静态链接 + 在 FROM scratch 里真跑 ==="
  tag="fulcrum-musl-probe:$arch"

  # ⚠ 這句話**不許寫成「BoringSSL 編不出來」**：`docker build` 會因為很多與
  #   BoringSSL 無關的原因失敗（上下文路徑、拉不到基礎鏡像、磁碟滿）。
  #   ★ 一句把所有失敗都歸到同一個原因上的回報錯誤，比沒有回報錯誤更能誤導下一個人。
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

  # ★ 主判據。完全靜態 ⇒ 沒有 INTERP 段、沒有 NEEDED 條目。
  #   兩個都看：只看 INTERP 會漏掉「有動態段但沒解釋器」這種畸形產物。
  interp=$(field "$ev" INTERP)
  needed=$(field "$ev" NEEDED)
  if [ "$interp" = "0" ] && [ "$needed" = "0" ]; then
    ok "[$arch] 完全静态：INTERP=0、NEEDED=0（$(field "$ev" FILE)）"
  else
    fail "[$arch] 不是完全静态：INTERP=$interp、NEEDED=$needed"
  fi

  echo "[probe] 大小 $(field "$ev" BYTES) 字节"
  rm -f "$ev"

  # ── 真跑。★ 鏡像裡除了這個二進位什麼都沒有。 ──────────────────────────
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

  # ★ 讓二進位自己說出它是給誰編的，與 ELF 頭讀出來的對一遍 ——
  #   交叉編譯時「以為在編 A、實際編的是 B」兩邊都不會回報錯誤。
  want_triple=$(expect_triple "$arch")
  if grep -q "目标三元组：$want_triple" "$out"; then
    ok "[$arch] 二进制自报的目标三元组 = $want_triple"
  else
    fail "[$arch] 二进制自报的三元组不是 $want_triple"
  fi
  rm -f "$out"
done

# ── 反證：同一份 Dockerfile，把靜態翻成動態 ────────────────────────────────
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
