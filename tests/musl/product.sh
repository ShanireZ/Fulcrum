#!/usr/bin/env bash
# **產品**的 musl 靜態產物自證（D22，owner 拍板）。
#
#   bash tests/musl/product.sh                 # 正向 + 反證
#   ARCHES="amd64" bash tests/musl/product.sh  # 只跑一個架構（預設就是它）
#   REVERSE=0     bash tests/musl/product.sh   # 跳過反證（★ 平時不要跳）
#
# ── 它與 `tests/musl/probe.sh` 的分工（★ 這一條是全部意義所在）───────────────
#
# `probe.sh` 編的是 **spike**（`spikes/musl-boringssl`），本指令碼編的是 **產品**
# （`crates/fulcrum` 的那個 bin）。D22 想守的那句話是 **G13 的散布口徑**
# ——「Linux x86_64 + aarch64 單靜態二進位」——**而探針答不了它**：
# 它自己的驗證記錄第 5 節第 1 條就寫著「『探針編得出來』證不了『樞衡編得出來』」。
# ⇒ owner 拍板把判據換成產品自證；探針**留在門外**當歷史記錄。
#
# ── 兩個方向，缺一不可 ──────────────────────────────────────────────────────
#
# 正向：`+crt-static` 的產物 `INTERP=0 / NEEDED=0`，且在 `FROM scratch` 裡
#       真的跑完一次 `fulcrum validate`（四層：診斷 → 結構化 → 執行環境圖 → TLS 裝載）。
# 反向①（**同一份 Dockerfile，翻成 `-crt-static`**）：動態版必須在 scratch 裡跑不起來。
#       ★ 分不開好壞兩種情況的尺子不是尺子。
# 反向②（**同一個鏡像，喂一份必須被拒的設定**）：結束代碼非 0，且說得出那句專門的診斷。
#       ★ ★ 它守的是「結束代碼 0」這件事本身 —— 一個**啟動就 exit(0)**、根本沒讀設定的
#       二進位，在正向那一格里與真的跑完四層**完全一樣**。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# ★ MSYS 會把容器內路徑改寫成 Windows 路徑，必須關掉；理由與 probe.sh 逐字相同。
export MSYS_NO_PATHCONV=1
# ⚠ 關掉之後宿主機側的路徑就沒人替你轉了 —— docker 是 Windows 處理程序。
if command -v cygpath >/dev/null 2>&1; then
  REPO_HOST="$(cygpath -m "$REPO")"
else
  REPO_HOST="$REPO"
fi

DOCKERFILE="docker/Dockerfile.musl-product"
# ⚠ ⚠ **上下文是儲存庫根**：根 `Cargo.toml` 的 `[patch.crates-io]` 指著 `vendor/pingora`。
#   儲存庫根那份 `.dockerignore` 負責把 `target/` 擋在上下文之外。
CONTEXT="."
# ★ 預設只跑 amd64。aarch64 要在 qemu 上編**整個產品**（不只是探針那點程式碼）。
# ✅ 「何時也跑 aarch64」曾掛號 **D24**，已由 **G144** 結案 = 候選 ②：
#   只在 `Cargo.lock` 或那三張釘 rustc 的 Dockerfile 變化時跑。
#   ⚠ ⚠ **那個決定不在本指令碼里**（本指令碼只認 `ARCHES`）—— 編排在 `tests/m0/docker-run.sh`，
#   判據與觸發集在 `tests/musl/arm64-trigger.sh`。⇒ 直接手跑本指令碼時它**不會**替你判。
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

# 從一個 `FROM scratch` 鏡像裡取檔案。
# ★ 不能 `docker run cat` —— 那個鏡像裡沒有 cat、沒有 sh，什麼都沒有。
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

  # ── 建置身分（`G141` 的第 ① 檔）─────────────────────────────────────────────
  #
  # ⚠ ★ **只在呼叫方已經設了 `FULCRUM_BUILD_ID` 時才往下傳**，⛔ 不在這裡自己去問 git。
  #   兩條理由，都不是風格問題：
  #   · 這個上下文的 `.dockerignore` 排掉了 `.git/` ⇒ 容器裡**讀不到**，第 ② 檔在這一趟
  #     必然落空 —— 身分只能從外面喂進來；
  #   · ⚠ ⚠ 而**無條件喂**會讓這一層的雜湊**每次提交都變**，於是這道每次提交都要跑的門
  #     每次都重編我們那幾個 crate。門禁跑出來的是**測試產物不是發佈物**，它報
  #     `+unknown` 是如實的。⇒ 發佈時由建置環境設這個變數，門禁不設。
  local id_arg=()
  if [ -n "${FULCRUM_BUILD_ID:-}" ]; then
    id_arg=(--build-arg "FULCRUM_BUILD_ID=$FULCRUM_BUILD_ID")
  fi

  echo "[product] docker build --platform linux/$arch（$crt）…"
  # ⚠ `--pull=false`：基礎鏡像已按 digest 釘死，不必每次去問 registry。
  docker build \
    --platform "linux/$arch" \
    --pull=false \
    --build-arg "CRT_STATIC=$crt" \
    "${id_arg[@]}" \
    -f "$REPO_HOST/$DOCKERFILE" \
    -t "$tag" \
    "$REPO_HOST/$CONTEXT"
}

for arch in $ARCHES; do
  echo
  echo "=== [$arch] 产品编成 musl 静态产物，并在 FROM scratch 里真跑 ==="
  tag="fulcrum-musl-product:$arch"

  # ⚠ 這句話**不許寫成「BoringSSL 編不出來」**：產品圖裡有一整票 `*-sys`，
  #   而 `docker build` 還會因為上下文、磁碟、網路失敗。
  #   ★ 一句把所有失敗都歸到同一個原因上的回報錯誤，比沒有回報錯誤更能誤導下一個人。
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

  # ★ 主判據。完全靜態 ⇒ 沒有 INTERP 段、沒有 NEEDED 條目。
  #   兩個都看：只看 INTERP 會漏掉「有動態段但沒解釋器」這種畸形產物。
  interp=$(field "$ev" INTERP)
  needed=$(field "$ev" NEEDED)
  if [ "$interp" = "0" ] && [ "$needed" = "0" ]; then
    ok "[$arch] 完全静态：INTERP=0、NEEDED=0（$(field "$ev" FILE)）"
  else
    fail "[$arch] 不是完全静态：INTERP=$interp、NEEDED=$needed"
  fi
  echo "[product] 大小 $(field "$ev" BYTES) 字节"

  # ── ★ ★ ★ D23：產物裡真的連結了哪幾套 TLS ────────────────────────────────
  #
  # ⚠ 它與供應鏈那兩道門問的**不是同一個問題**：門 4 看 `Cargo.lock`、門 5 看依賴圖，
  #   而「圖裡有、產物裡其實沒連結」是門 5 **原理上**看不見的那一半。
  # ★ 判據的推導在 `tests/ci/tls-linkage.sh` 的檔案頭，這裡只做判定。
  tls_nm=$(field "$ev" TLS_NM)
  tls_ctrl=$(field "$ev" TLS_PROBE_CONTROL)
  # ⚠ ⚠ **先判「量到了沒有」**：讀不到符號表時下面每一條「必須為 0」都會憑空全綠。
  #   ⇒ 「沒能檢查」不算「檢查通過」（與本儲存庫其它幾處同一條紀律）。
  if [ "$tls_nm" != "ok" ]; then
    fail "[$arch] 取不到符号表（TLS_NM=$tls_nm）⇒ 下面那组 TLS 链接判据一律不可信"
  elif [ -z "$tls_ctrl" ] || [ "$tls_ctrl" -eq 0 ]; then
    # ★ 掃描器自證：一個寫錯的模式與「真的一個都沒有」給出完全相同的 0。
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

  # ── 正向：在什麼都沒有的鏡像裡跑一次 validate ──────────────────────────
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

  # ── 反向②：同一個鏡像，喂一份必須被拒的設定 ────────────────────────────
  #
  # ★ ★ 它守的是**上面那個「結束代碼 0」本身**：一個啟動就 exit(0)、根本沒讀設定的
  #   二進位，在正向那一格里長得一模一樣。
  # ⚠ scratch 裡沒有 shell，但 `--entrypoint` 走的是 execve，不需要 shell。
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

# ── 反向①：同一份 Dockerfile，把靜態翻成動態 ──────────────────────────────
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
  echo "    ⚠ 它只证了刚才真跑过的那几个架构（本趟：$ARCHES）—— ⛔ 别读成两个架构都验过了。"
else
  echo "MUSL PRODUCT FAILED（$FAILS 条）" >&2
  exit 1
fi
