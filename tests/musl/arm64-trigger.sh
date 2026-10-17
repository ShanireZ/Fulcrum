#!/usr/bin/env bash
# aarch64 那一格什麼時候跑（**D24 = 候選 ②**，owner 2026-09-06 拍板）。
#
#   . tests/musl/arm64-trigger.sh          # 被 source，拿三個函式
#   bash tests/musl/arm64-trigger.sh --self-check
#   bash tests/musl/arm64-trigger.sh --hash        # 打當前觸發集的雜湊
#
# ★ ★ ★ 判據只回答一個問題：**「當前這個相依狀態，有沒有被 arm64 真的驗證過。」**
#   ⛔ 它不回答「這臺機器能不能跑 arm64」（那是 binfmt，機器本機狀態，由呼叫方探）。
#
# ── 觸發集為什麼是這幾個檔案 ────────────────────────────────────────────────
#
# `docker/Dockerfile.musl-product` 第 23 行逐字寫著「rustc 版本必須與
# `docker/Dockerfile.build`、`docker/Dockerfile.musl-probe` **三者一致**」
# ⇒ `G144`（結案 D24）裡那句「三張 Dockerfile」指的就是它們。
#
# ★ 這裡用**推導**取它們（`^FROM rust:`），⛔ 不寫死清單：寫死的清單在出現第四張
#   釘 rustc 的 Dockerfile 時會靜靜漏掉它，而**漏掉的那一刻不紅**
#   —— `tests/ci/shellcheck-all.sh` 為同一條教訓改過一次（bench/ 帶著五個沒掃過的指令碼進來）。
#
# ⚠ `Dockerfile.bench` / `Dockerfile.systemd` **有意不在集合裡**：它們不參與 musl
#   產物的建置，算進去會讓一次 bench 鏡像調整白白觸發一趟 18 分鐘的 arm64 冷建。
#   ★ 這條「有意排除」由自測釘著，⇒ 它是一條判據不是一句註解。
#
# ⚠ ⚠ ★ **原始碼改動有意不觸發，而這是一條判斷、不是一條讀數。**
#   `Dockerfile.musl-product` 確實是 `COPY . .`，但我們自己那幾個 crate 是架構中立的、
#   amd64 那一格每趟都編；aarch64 會壞在**相依 / 工具鏈 / 基礎鏡像**上，那正是這個集合。
#   ⛔ 它錯的方向是「漏掉一次原始碼引起的 arm64 迴歸」。真出現那麼一次時，
#   要改的是**這個集合**，而不是給它加一條豁免。

set -euo pipefail

# ── 觸發集：推導，⛔ 不是清單 ───────────────────────────────────────────────
fulcrum_arm64_trigger_files() {
  local root=${1:-.} p
  {
    if [ -f "$root/Cargo.lock" ]; then
      echo "Cargo.lock"
    fi
    # ⚠ `grep -l` 在一個都沒匹配上時結束代碼非 0，而本函式在 `set -e` 下被呼叫
    #   ⇒ 顯式吞掉它的結束代碼，把「一個都沒有」交給自測去判（空集是一種**症狀**，
    #     ⛔ 不是一個可以靜靜通過的答案）。
    for p in $(grep -l '^FROM rust:' "$root"/docker/Dockerfile.* 2>/dev/null || true); do
      printf '%s\n' "${p#"$root"/}"
    done
  } | LC_ALL=C sort
}

# ── 觸發集的雜湊 ───────────────────────────────────────────────────────────
#
# ★ 路徑名與內容**一起**進雜湊：只雜湊內容的話，把一張 Dockerfile 改名
#   （或刪掉一張、加進另一張同內容的）不會讓雜湊變。
fulcrum_arm64_trigger_hash() {
  local root=${1:-.} f
  fulcrum_arm64_trigger_files "$root" | while IFS= read -r f; do
    printf '%s\n' "$f"
    cat "$root/$f"
  done | sha256sum | cut -d' ' -f1
}

# ── 純函式：當前雜湊 + 驗證記錄正文 ⇒ 三選一 ─────────────────────────────────
#
#   verified —— 這個相依狀態被 arm64 真的驗證過，本趟不必再跑
#   stale    —— 驗證過，但驗證的是**另一個**相依狀態
#   never    —— 從來沒有過一份驗證記錄
#
# ⚠ `never` 與 `stale` 有意分開：給人的話不一樣（一個是「還沒有過」，
#   一個是「你剛改了相依」），而**合成同一個「要跑」並不會讓判據更簡單**。
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

# ── 這臺機器跑得了 aarch64 嗎（binfmt，**機器本機狀態**）────────────────────
#
#   fulcrum_arm64_platform_ok "<docker buildx inspect --bootstrap 的输出>"
#
# ★ 寫成**純函式**（讀數由呼叫方喂進來），理由與 `bench/lib.sh` 那幾條逐字相同：
#   直接去問 docker 的判據，只能在**它恰好跑著的那臺機器上**被觀察到一種結果。
#
# ⚠ ⚠ **誠實交代一條**：容器那一層今天只觀察到了**一個方向** —— 2026-09-06 在
#   `ShanirePCX` 上裝 binfmt **之前** `docker run --platform linux/arm64` 回
#   `exec format error`、裝**之後**回 `aarch64`，兩個方向都實測過；但
#   `buildx inspect` 的輸出**只在裝好之後看過**。⇒ 下面這個函式的**解析**兩個方向
#   都由合成輸入釘著，而「裝之前 buildx 到底怎麼打」本機沒有觀察過。
#
# ★ ★ ★ **缺的那個方向 2026-09-08 在 `ShanireHomePC` 上量到了，結論比預想的壞：**
#   binfmt **沒有**註冊時（`docker run --platform linux/arm64 alpine:3 uname -m`
#   當場回 `exec /bin/uname: exec format error`），`docker buildx inspect` **照樣列著**
#     Platforms: linux/amd64, …, linux/arm64, linux/riscv64, linux/ppc64le, linux/s390x, linux/arm/v7, linux/arm/v6
#   ⇒ 它列的是**buildx 願意接受的目標**，⛔ 不是「這臺機器現在跑得動的」。
#   ⇒ **假陽不是邊角情況，它是 Docker Desktop 上的常態** ——
#     這個探針在本機實際上**攔不住任何東西**。
# ⚠ 同一天還量到：binfmt 的註冊**不跨 Docker Desktop / WSL2 VM 重新啟動存活** ——
#   09-08 10:28 那趟 aarch64 編得好好的，同一臺機器同一天下午就 `exec format error` 了。
# ★ 誤判方向仍然是軟的（假陽 ⇒ 那趟建置自己回報錯誤，本次實測正是如此：
#   `[build 5/7]` 第一條真正要執行的指令上 `exec /bin/sh: exec format error`），
#   ⛔ 但別再把這個探針讀成一道門。真要判「這臺機器現在跑不跑得了 arm64」，
#   判據得是**真執行一次**（`docker run --platform linux/arm64 … true`），不是問 buildx。
fulcrum_arm64_platform_ok() {
  case "$1" in
    *linux/arm64*) return 0 ;;
    *) return 1 ;;
  esac
}

# ── 編排要的那個決定（純函式）───────────────────────────────────────────────
#
#   fulcrum_arm64_decide <state> <platform_ok: yes|no>
#
# 打三選一：`amd64-only` / `both` / `cannot-verify`。
#
# ★ ★ ★ **抽成純函式是為了讓那條紅路徑可以被執行到。** 把它寫在 `docker-run.sh` 的
#   `if/elif/else` 裡，`cannot-verify` 那一支在一臺裝了 binfmt 的機器上**一次都不會跑**
#   —— 而一條從未被執行過的路徑不會自己回報錯誤：它壞掉（比如少寫一個 `exit 1`）的那天，
#   正是它本該攔住東西的那天。⇒ 這裡兩個方向都由合成輸入釘著。
fulcrum_arm64_decide() {
  local state=$1 platform_ok=$2
  case "$state" in
    verified) echo "amd64-only" ;;
    stale | never)
      if [ "$platform_ok" = yes ]; then echo "both"; else echo "cannot-verify"; fi
      ;;
    # ⚠ 認不出來的狀態**不許**落進任何一條「照常跑」的分支：
    #   一個 `*)` 備援到 amd64-only 的實作，會在 `--state` 哪天多一種取值時靜默漏跑。
    *) echo "cannot-verify" ;;
  esac
}

# ── 自測：真實儲存庫 + 合成輸入 ───────────────────────────────────────────────
fulcrum_arm64_selfcheck() {
  local rc=0 n=0 out root
  want_eq() { n=$((n + 1)); [ "$3" = "$2" ] || { echo "✗ $1（该是 '$2'，实得 '$3'）" >&2; rc=1; }; }
  want_ne() { n=$((n + 1)); [ "$3" != "$2" ] || { echo "✗ $1（不该等于 '$2'）" >&2; rc=1; }; }
  want_has() { n=$((n + 1)); case "$3" in *"$2"*) ;; *) echo "✗ $1（'$2' 不在里面：$3）" >&2; rc=1 ;; esac; }
  want_hasnt() { n=$((n + 1)); case "$3" in *"$2"*) echo "✗ $1（'$2' 竟然在里面）" >&2; rc=1 ;; esac; }

  # ★ 儲存庫根可被 `FULCRUM_REPO_ROOT` 覆蓋。⚠ 這**不是**偵錯旋鈕，是判據能不能被反證的
  #   前提：反證要把本檔案**複製到別處**再注入（⛔ 不改工作樹上的原件 —— 這棵樹上常年
  #   有別的工作階段在動），而副本按 `BASH_SOURCE` 推出來的「儲存庫根」會指向那個暫存目錄，
  #   於是**基線本身就是紅的**，後面每條注入都零判別力。
  local REPO
  REPO=${FULCRUM_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}

  # —— 真實儲存庫上：★★★ 先證明樣本里真有東西 ——
  #   ⚠ 一個靜靜回傳空集的枚舉器，會讓雜湊恆為「空輸入的 sha256」⇒ 狀態永遠 verified，
  #     而那與「一切正常」長得一模一樣。
  out=$(fulcrum_arm64_trigger_files "$REPO" | tr '\n' ' ')
  want_has "触发集里没有 Cargo.lock" "Cargo.lock" "$out"
  want_has "触发集里没有 musl-product" "docker/Dockerfile.musl-product" "$out"
  want_has "触发集里没有 musl-probe" "docker/Dockerfile.musl-probe" "$out"
  want_has "触发集里没有 Dockerfile.build" "docker/Dockerfile.build" "$out"
  # ★ 「有意排除」那兩張：這條讓排除成為**判據**而不是一句註解。
  want_hasnt "Dockerfile.bench 不该在触发集里" "Dockerfile.bench" "$out"
  want_hasnt "Dockerfile.systemd 不该在触发集里" "Dockerfile.systemd" "$out"

  # —— 純函式三個分支 ——
  want_eq "哈希相同该判 verified" "verified" "$(fulcrum_arm64_state abc "hash=abc")"
  want_eq "哈希不同该判 stale" "stale" "$(fulcrum_arm64_state abc "hash=zzz")"
  want_eq "没有记录该判 never" "never" "$(fulcrum_arm64_state abc "")"
  want_eq "只有注释的记录该判 never" "never" "$(fulcrum_arm64_state abc "# 什么都没有")"
  # ⚠ 記錄裡混著別的行時也要取得到 hash=（⛔ 別假設它在第一行）。
  want_eq "hash= 不在首行时没取到" "verified" \
    "$(fulcrum_arm64_state abc "$(printf '# 注释\nverified_on=x\nhash=abc\n')")"

  # —— ★★★ 承重：雜湊必須對**內容**和**成員集合**都敏感 ——
  #   ⛔ 用合成目錄，不動真樹。
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
  # ★ 改一張**不在集合裡**的 Dockerfile ⇒ 雜湊必須**不變**（否則排除是假的）。
  printf 'FROM alpine\nRUN noise\n' > "$root/docker/Dockerfile.bench"
  h3=$(fulcrum_arm64_trigger_hash "$root")
  want_eq "改了 Dockerfile.bench 哈希竟然变了 —— 排除没生效" "$h2" "$h3"
  # ★ 成員集合變了（刪掉一張）也必須變雜湊。
  rm -f "$root/docker/Dockerfile.musl-product"
  want_ne "删掉一张 Dockerfile 哈希却没变" "$h3" "$(fulcrum_arm64_trigger_hash "$root")"
  rm -rf "$root"

  # —— binfmt 探針的**解析**，兩個方向都釘（容器那一層見函式頭的交代）——
  n=$((n + 1))
  fulcrum_arm64_platform_ok "Platforms: linux/amd64, linux/amd64/v2, linux/arm64" ||
    { echo "✗ 列了 linux/arm64 却判成跑不了" >&2; rc=1; }
  n=$((n + 1))
  if fulcrum_arm64_platform_ok "Platforms: linux/amd64, linux/amd64/v2, linux/386"; then
    echo "✗ 没列 linux/arm64 却判成跑得了 —— 这个探针恒真，等于没有" >&2
    rc=1
  fi
  # ⚠ 空輸入（`docker buildx` 壓根沒跑起來）必須判**跑不了**，
  #   ⛔ 不許當成「跑得了」——「沒能檢查」不算「檢查通過」。
  n=$((n + 1))
  if fulcrum_arm64_platform_ok ""; then
    echo "✗ 空输入被当成了跑得了" >&2
    rc=1
  fi

  # —— 編排的那個決定：**三條分支各釘一次**（含那條平時跑不到的紅路徑）——
  want_eq "已验证时不该跑 aarch64" "amd64-only" "$(fulcrum_arm64_decide verified yes)"
  # ★ 已驗證 ⇒ 跑不跑得了 aarch64 **不該影響**這個決定（否則沒裝 binfmt 的機器會被誤攔）。
  want_eq "已验证时不该因为跑不了 aarch64 就变卦" "amd64-only" "$(fulcrum_arm64_decide verified no)"
  want_eq "依赖变了且跑得了 ⇒ 该两个架构一起跑" "both" "$(fulcrum_arm64_decide stale yes)"
  want_eq "从没验证过且跑得了 ⇒ 该两个架构一起跑" "both" "$(fulcrum_arm64_decide never yes)"
  # ★ ★ ★ **這兩條就是那條平時執行不到的紅路徑**：相依變了、而這臺機器跑不了 ⇒
  #   必須判「驗不了」，⛔ 不許退化成 amd64-only 悄悄放行。
  want_eq "依赖变了但跑不了 ⇒ 必须判验不了" "cannot-verify" "$(fulcrum_arm64_decide stale no)"
  want_eq "从没验证过且跑不了 ⇒ 必须判验不了" "cannot-verify" "$(fulcrum_arm64_decide never no)"
  # ⚠ 認不出來的狀態也必須落到驗不了，⛔ 不許備援成「照常跑」。
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
    # ★ 給 `docker-run.sh` 用的是**子處理程序**這條路，⛔ 不是 source ——
    #   本檔案的自測會定義 `want_*` 這幾個名字，source 進去會與呼叫方的同名函式打架，
    #   而那種打架**不回報錯誤**，只是讓某一邊的斷言換了一個實作。
    --state)
      _root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
      _stamp=""
      if [ -n "${2:-}" ] && [ -f "$2" ]; then _stamp=$(cat "$2"); fi
      fulcrum_arm64_state "$(fulcrum_arm64_trigger_hash "$_root")" "$_stamp"
      ;;
    # `--platform-ok` ⇒ 結束代碼回答「這臺機器跑得了 aarch64 嗎」。
    --platform-ok)
      fulcrum_arm64_platform_ok "$(docker buildx inspect --bootstrap 2>/dev/null || true)"
      ;;
    # `--decide <验证记录文件>` ⇒ 打 amd64-only / both / cannot-verify。
    # ★ 編排（`tests/m0/docker-run.sh`）只認這三個詞，⇒ 那邊不再有自己的判斷邏輯。
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
    # `--record <文件>` ⇒ 寫一份驗證記錄。
    # ⛔ **只許在一次真的成功的 aarch64 驗證之後呼叫**（呼叫點在 `tests/m0/docker-run.sh`）。
    # ★ 格式只存在這一處 ⇒ 那個檔案是**生成出來的**，⛔ 不是手寫的；
    #   一份生成器重現不出來的記錄，與一份編造的記錄無法區分。
    --record)
      _out=${2:?用法：--record <文件>}
      _root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

      # ── 這份記錄是哪一次提交上驗的 ────────────────────────────────────────
      #
      # ⚠ ⚠ ★ 原先這裡是一句 `git … 2>/dev/null || echo unknown`，於是 git 拒答時
      #   記錄裡落一個 `verified_commit=unknown`，而**螢幕上一個字都不會有**。
      #   ⇒ 2026-09-08 `61b867a` 那份記錄正是這麼來的：一份**驗證記錄**說不出
      #   自己驗的是哪一次提交，而當時沒有任何東西說過為什麼。
      # ★ 現在真因由 git 的原話說，並且**連同原話一起寫進記錄** —— 一份記錄
      #   自己說得出「這一格為什麼是空的」，比事後猜強。
      # ⚠ `2>&1 >/dev/null` 只捕 stderr（⛔ 順序不能反）；`|| _rc=$?` 接住，
      #   ⛔ 否則 `set -e` 會在這裡把整個 --record 掐掉，連成功的那趟也寫不成記錄。
      #
      # ★ ★ ★ **`61b867a` 那個 `unknown` 的真因（2026-09-08 查實並重現）：**
      #   `tests/m0/docker-run.sh:615` 有一句 `export MSYS_NO_PATHCONV=1`（docker 那些
      #   呼叫要它），而本指令碼由該檔案在**第 1217 行**調起 ⇒ **繼承了這個 export**。
      #   於是 MSYS 不再翻譯 argv 裡的路徑，原生 `git.exe` 拿到字面值 `/d/Workspace/Fulcrum`：
      #     fatal: cannot change to '/d/Workspace/Fulcrum': No such file or directory   (rc=128)
      #   ⚠ 而 `docker-run.sh:495` 那道行尾檢查**排在 615 之前**，git 在那裡是好的
      #   ⇒ 同一趟門禁裡「git 一會兒行一會兒不行」，看起來毫無道理。
      # ⛔ **所以這裡不用 `git -C <路径>`** —— 那正是把 MSYS 路徑餵給原生 exe 的寫法。
      #   改成在子 shell 裡 `cd` 過去再問：`cd` 是 bash 內建，⛔ 不經過 argv 轉換，
      #   於是這一句在 `MSYS_NO_PATHCONV` 設與不設時**行為相同**。
      #   （同源寫法：`tests/lib/vol-lock.sh:168` 的 `git hash-object --stdin` —— 沒有路徑參數。）
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
        # ⚠ 只有問不出來的時候才有這一段：把 git 的原話原樣留在記錄裡。
        #   ⛔ 別把它寫成 `verified_commit=` 的值 —— 解析 `hash=` 的是 sed，
        #   而這幾行以 `#` 開頭，對解析器天然透明（`--self-check` 裡那條
        #   「hash= 不在首行時也取得到」正是釘這件事的）。
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
