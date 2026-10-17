#!/usr/bin/env bash
# fork 迴歸網（G30）：跑 vendor/pingora 自帶的單元測試。
# 這個指令碼在容器裡跑（G26），結束代碼即結論。
#
# ★ 為什麼需要它：`vendor/pingora` 自帶 `[workspace]` 且被根 `Cargo.toml` `exclude`，
#   所以根目錄的 cargo **一個字都跑不到它**——而 fork 改過的每一處上界，
#   上游都自帶正對著的單元測試：
#
#     sfv    0.10→0.15  →  compression/mod.rs   test_accept_encoding_req_header
#                                               test_decide_on_accept_encoding
#     brotli 3→8        →  compression/brotli.rs 壓縮/解壓往返
#     nix    0.24→0.31  →  server/transfer_fd/mod.rs、protocols/l4/stream.rs
#     lru    0.16→0.18  →  pingora-pool/src/lru.rs、connection.rs
#
#   在這個指令碼存在之前，FORK.md 裡那句「M0 接縫驗證是這次改動唯一的迴歸網」
#   是**字面屬實**的：M0 一個位元組的壓縮協商都不測。
set -euo pipefail

REPO=${REPO:-/w}
MANIFEST="$REPO/vendor/pingora/Cargo.toml"
LOCK="$REPO/vendor/pingora/Cargo.lock"
ROOT_LOCK="$REPO/Cargo.lock"
TARGET=${VENDOR_TARGET:-$REPO/target/vendor}

fail() { echo; echo "VENDOR TESTS FAILED: $*"; exit 1; }

# ── [1/5] 鎖定檔 ────────────────────────────────────────────────────────────
# vendor 是獨立 workspace，有自己的一份解析結果。沒有鎖定檔，每次跑用的版本都可能不同。
if [ ! -f "$LOCK" ]; then
  echo "=== [1/5] 生成 vendor/pingora/Cargo.lock ==="
  cargo generate-lockfile --manifest-path "$MANIFEST"
else
  echo "=== [1/5] 已有 vendor/pingora/Cargo.lock ==="
  # ★ 鎖定檔存在 ≠ 鎖定檔還對得上清單。
  #   FORK.md 的 rebase 步驟第 3、4 步改的正是 vendor 裡的 Cargo.toml，
  #   改完之後這份鎖就過期了，而下面用的是 `--locked`——cargo 拋的原始錯誤
  #   離「刪鎖重生成」這條修法**隔著一層**。**每次 rebase 必踩**，
  #   所以這裡提前用一次不編譯的 `cargo metadata --locked` 把它問出來。
  # ⚠ ⚠ ★ 但**隔著一層 ≠ 該丟掉**：它是唯一分得開「鎖過期」與「網路／鑑權／取不到 crate」
  #   的東西。⇒ 下面把它原樣打出來，再補那條最常見的解讀，⛔ 不是拿解讀替掉它。
  # ⚠ ⚠ ★ 原先這裡把 cargo 的 stderr 丟進 `/dev/null`，然後**斷言**死因是「鎖過期」
  #   並教人 `rm vendor/pingora/Cargo.lock`。實測（2026-09-08，把 cargo 換成一個
  #   報網路錯的樁）：一次**取不到 crate** 的失敗，得到的也是那一整段「刪掉鎖」——
  #   ⇒ 照著敲會**刪掉一份入庫的解析結果**，而且刪了也修不好。
  # ★ 現在先把 cargo 的原話原樣打出來，再把「最常見的那一種」降級成**建議**，
  #   並寫明什麼情況下**不該**刪鎖。⛔ 判據不替 cargo 說它沒說過的話。
  cargo_rc=0
  cargo_err=$(cargo metadata --manifest-path "$MANIFEST" --format-version 1 --locked 2>&1 >/dev/null) || cargo_rc=$?
  if [ "$cargo_rc" -ne 0 ]; then
    echo
    echo "★ \`cargo metadata --locked\` 没过（退出码 $cargo_rc）。⛔ 先看它自己怎么说："
    printf '%s\n' "$cargo_err" | sed 's/^/      /'
    echo
    echo "  ★ **最常见**的一种是锁与清单对不上（刚 rebase 过、改了某个 Cargo.toml）——"
    echo "    上面那段里出现 lock file / --locked / needs to be updated 之类字样时才是它。"
    echo "    那一种的修法就一步——删掉它，重跑本脚本会自动重新生成："
    echo "        rm vendor/pingora/Cargo.lock"
    echo "    ★ 重新生成之后记得**连同新锁一起提交**（G29 第 2 条：Cargo.lock 入库）。"
    echo "  ⛔ ⛔ 上面说的若是网络、鉴权、或某个 crate 取不到，**别删锁**：删了修不好，"
    echo "    还会把一份入库的解析结果弄丢。"
    fail "cargo metadata --locked 没过（退出码 $cargo_rc）——死因见上面 cargo 的原话"
  fi
  echo "  ✓ 锁文件与清单一致"
fi

# ── [2/5] ★ 證明這次跑的是「二進位裡那套版本」 ─────────────────────────────
# 這一步不能省。vendor 獨立解析，如果它解出來的 brotli 跟根 Cargo.lock 裡的
# 不是同一個，那測試全綠也證明不了任何關於實際產物的事——只證明了另一套組合能跑。
echo "=== [2/5] 比对 vendor 与根 Cargo.lock 的版本一致性 ==="

dump_lock() {
  awk '
    $1 == "name"    { n = $3; gsub(/"/, "", n); next }
    $1 == "version" && n != "" { v = $3; gsub(/"/, "", v); print n, v; n = "" }
  ' "$1"
}

dump_lock "$ROOT_LOCK" > /tmp/root.pkgs
dump_lock "$LOCK"      > /tmp/vendor.pkgs

# ★ 不變數是「根 lock 裡出現的每個版本，vendor lock 裡都必須有」，**不是**「兩邊逐字相等」。
#   （2026-09-24 起收窄成「……除非沒有任何 pingora crate 用它」，由 cargo 答，見下面 `who_uses` 那段。）
#   Cargo.lock 允許同名包多版本共存：vendor 側多解析出 dev-dependencies 與未啟用的可選相依
#   （sentry、reqwest、rstest 這些），它們會合法地拖進老版本——例如 rand 0.8.7 與 0.10.2 並存。
#   那些不進產物，不該判紅。**只有根側那個版本在 vendor 側缺席才是真問題。**
#
# ★ ★ **查的是兩邊都有的全部包，不是一張手寫名單。**
#   原先這裡是 `BUMPED="lru prometheus protobuf nix brotli ..."` —— 一張 16 項的手寫清單，
#   是 FORK.md §1 那張上界表的**人工鏡像**。它的危險方向不是「多寫了幾項」（無害），
#   而是**下一次 rebase 抬了一條新上界、沒人往名單里加**：這道門會安靜地少覆蓋一個包，
#   而它看起來照樣是綠的。這與 `supply-audit.py` 的 `ACCEPTED`、`unclaimed.sh` 的忽略名單
#   是同一個形狀——**不自省的名單**。
#
#   改成全量之後沒有名單可維護，覆蓋面從 16 個變成 170 個。
#   實測：root 171 包 / vendor 332 包 / 共有 170，**零不一致**；
#   唯一只在 root 側的是 `m0-seam` 自己（工作區成員，vendor 本就測不到，按規則自動跳過）。
versions_of() { awk -v k="$2" '$1 == k { print $2 }' "$1" | sort -u; }

# 把多行／多空格的版本列表壓成一行、單空格分隔。
# ★ 原先寫的是 `$(echo $rvs)`——靠**未加引號的詞拆分**再重新拼起來。它能工作，
#   但和 CRLF 門裡那個 `printf '...' $CRLF_FILES` 是同一個形狀：把「分詞」當字串處理手段。
#   SC2116 / SC2086 正是衝這個來的。
#   ⚠ 注意別讓註解裡某一行以 `#` + 空白 + 那個工具名開頭——它會被當成 **directive** 解析，
#     報 SC1072/SC1073。這一條本身就是寫這段註解時踩出來的。
squash() { printf '%s' "$1" | tr -s '[:space:]' ' ' | sed 's/^ *//; s/ *$//'; }

# 兩邊都有的包 = 本次真正要比的物件。只在 root 一側的（工作區成員）vendor 本就測不到；
# 只在 vendor 一側的是 dev-dep，不進產物。
cut -d' ' -f1 /tmp/root.pkgs   | sort -u > /tmp/root.names
cut -d' ' -f1 /tmp/vendor.pkgs | sort -u > /tmp/vendor.names
comm -12 /tmp/root.names /tmp/vendor.names > /tmp/shared.names
comm -23 /tmp/root.names /tmp/vendor.names > /tmp/rootonly.names

SHARED=$(wc -l < /tmp/shared.names)
ROOTONLY=$(wc -l < /tmp/rootonly.names)
MISMATCH=0
DRAGGED=0
ROOT_ONLY_PATH=0

# ★ ★ 根側有、vendor 側缺的那個版本，**產物裡是誰在用它** —— 問 cargo，不猜（2026-09-24 加）。
#   起因：rcgen 0.14.10 → pem 4 把 base64 0.23.1 帶進了根鎖，而 pingora 用的仍是 0.22.1（兩把鎖都有）。
#   按包名比，這與「pingora 自己在產物裡用著一個 vendor 沒解析出來的版本」長得一模一樣 ——
#   而本步要攔的只有後者：vendor 測試跑的不是產物裡 **pingora 那一側** 的組合。
#   ⇒ 反向樹上有 `pingora-*` = 真問題（紅）；沒有 = 只經根側獨有的包進產物，vendor 測不到也不必測（資訊）。
#   ⛔ 不用手寫名單：將來若 pingora 自己也升到那個版本而 vendor 滯後，名單會把真問題蓋住，而這裡會紅。
# ★ `--target all`：寧可多算路徑（多算只會更容易紅），不讓「只在某個 target 上出現的相依」漏掉。
# ★ `-e normal,build`：問的是產物裡的圖；dev 邊只來自工作區成員自己，碰不到 pingora 那一側。
# ★ `</dev/null`：它跑在 `while read` 循環裡，不許碰循環的 stdin。
# ⚠ 反證（2026-09-24 實測）：只在根鎖裡把 pingora 用的 lru 退一個補丁版本 ⇒ 本步紅、點名 pingora-pool。
ROOT_MANIFEST="$REPO/Cargo.toml"
who_uses() {
  cargo tree --locked --manifest-path "$ROOT_MANIFEST" -i "$1@$2" \
    -e normal,build --target all --prefix none </dev/null 2>&1
}

while IFS= read -r p; do
  rvs=$(versions_of /tmp/root.pkgs   "$p")
  vvs=$(versions_of /tmp/vendor.pkgs "$p")

  missing=
  for v in $rvs; do
    printf '%s\n' "$vvs" | grep -qx "$v" || missing="$missing $v"
  done

  if [ -z "$missing" ]; then
    if [ "$(printf '%s\n' "$vvs" | wc -l)" -gt "$(printf '%s\n' "$rvs" | wc -l)" ]; then
      # vendor 側多解析出別的版本——合法（dev-dep／未啟用的可選相依），只作資訊
      DRAGGED=$((DRAGGED + 1))
    fi
    continue
  fi

  for v in $missing; do
    if ! tree=$(who_uses "$p" "$v"); then
      # ⛔ 問不出來 ≠ 沒人用：失敗一律判紅。
      printf '  ✗  %-24s 根侧的 %s 在 vendor 侧缺席，且问不出产物里谁在用它（cargo tree 失败）：\n' "$p" "$v"
      printf '%s\n' "$tree" | sed -n '1,5s/^/        /p'
      MISMATCH=$((MISMATCH + 1))
    elif via=$(printf '%s\n' "$tree" | grep -o '^pingora-[a-z-]*' | sort -u) && [ -n "$via" ]; then
      printf '  ✗  %-24s 根侧的 %s 在 vendor 侧缺席，而 %s 在用它（vendor 有：%s）\n' \
             "$p" "$v" "$(squash "$via")" "$(squash "$vvs")"
      MISMATCH=$((MISMATCH + 1))
    else
      printf '  ·  %-24s 根侧的 %s 只经根侧独有的包进产物（%s），没有 pingora crate 在路上\n' \
             "$p" "$v" "$(printf '%s\n' "$tree" | sed -n '2,4p' | awk '{print $1}' | paste -sd'>' - | sed 's/>/ ← /g')"
      ROOT_ONLY_PATH=$((ROOT_ONLY_PATH + 1))
    fi
  done
done < /tmp/shared.names

if [ "$MISMATCH" -ne 0 ]; then
  echo
  echo "★ $MISMATCH 个包：产物里用的版本在 vendor 侧根本没解析出来。"
  echo "  这意味着 vendor 测试跑的不是产物里那套组合，结果不可采信。"
  echo "  对齐方式（对每个不一致的包）："
  echo "    cargo update -p <name> --precise <root 侧版本> --manifest-path vendor/pingora/Cargo.toml"
  fail "版本不一致，拒绝在不可采信的组合上跑测试"
fi
if [ "$ROOT_ONLY_PATH" -eq 0 ]; then
  printf '  ✓  %s 个共有包，root 侧的版本在 vendor 侧全部在场' "$SHARED"
else
  printf '  ✓  %s 个共有包，root 侧的版本在 vendor 侧全部在场 —— 除了上面 · 行那 %s 个（没有 pingora crate 用它们）' \
         "$SHARED" "$ROOT_ONLY_PATH"
fi
printf '（其中 %s 个 vendor 另有旧版本，来自 dev-dep／未启用的可选依赖，不进产物）\n' "$DRAGGED"
[ "$ROOTONLY" -eq 0 ] || printf '  ·  另有 %s 个只在 root 侧（工作区成员，vendor 本就测不到）：%s\n' \
     "$ROOTONLY" "$(squash "$(cat /tmp/rootonly.names)")"

# ★ 這道門必須能紅。它唯一的失效方式是「共有包數量掉到 0」——那時循環一次都不跑，
#   `MISMATCH` 恆為 0，於是它在**什麼都沒比**的情況下報綠。給個下界釘住。
[ "$SHARED" -ge 100 ] || fail "只比出 $SHARED 个共有包（正常应有 170 上下）——
       两份 lock 的解析结果之一多半是空的或格式变了，本次比对没有意义。"
echo "  ✓ 产物里 pingora 那一侧用到的版本在 vendor 侧全部在场——下面的测试结果对实际产物有效"

# ── [3/5] ★ ★ 自證「黑洞」真的是黑洞 ────────────────────────────────────────
#
# 下面那批連接超時測試全部相依一個前提：`192.0.2.1:79` 發出去的 SYN 被**靜默丟棄**。
# 這個前提**不是天然成立的**——Docker 預設網路會替這個位址應答（實測 1.7ms 就
# CONNECTED），要靠 docker-run.sh 裝的那條 iptables DROP 規則才成立。
#
# ★ ★ 判據掛在**行為**上，不掛在「規則在不在」上。`iptables -L` 能列出規則，
#   不等於它生效了：容器可能沒有 NET_ADMIN、核心模組可能缺、也可能被前面的鏈
#   搶先 ACCEPT。**能證明它生效的只有「連過去真的會超時」這一件事。**
#
# 三種結果必須分清，它們對應完全不同的修法：
#   超時      → 真丟包，前提成立
#   連上      → Docker 網路在替它應答，規則沒裝或沒生效
#   立刻失敗  → EHOSTUNREACH/ECONNREFUSED 一類。★ 這**也不對**——測試等的是
#               ConnectTimedout，立刻失敗給的是 ConnectError，照樣紅。
#               （所以不能用 `ip route add blackhole` 代替 DROP。）
echo "=== [3/5] 自证 192.0.2.1:79 是真黑洞 ==="

BH_RC=0
BH_T0=$(date +%s%N)
timeout 2 bash -c 'exec 3<>/dev/tcp/192.0.2.1/79' 2>/dev/null || BH_RC=$?
BH_MS=$(( ($(date +%s%N) - BH_T0) / 1000000 ))

case "$BH_RC" in
  124)
    echo "  ✓ ${BH_MS}ms 后超时 —— SYN 确实被丢弃，下面的超时测试前提成立"
    ;;
  0)
    fail "192.0.2.1:79 在 ${BH_MS}ms 内**连上了** —— 它不是黑洞。
       上游用它当不可达地址（BLACK_HOLE 常量），连得上的话那批连接超时测试会恒红。
       修法：容器要带 --cap-add=NET_ADMIN 起，并装上
           iptables -A OUTPUT -d 192.0.2.0/24 -j DROP
       正常走 tests/m0/docker-run.sh 会自动做这件事；直接跑本脚本则要自己装。"
    ;;
  *)
    fail "192.0.2.1:79 在 ${BH_MS}ms 内**立刻失败**（退出码 $BH_RC），不是超时。
       立刻失败给出的是 ConnectError，而测试等的是 ConnectTimedout —— 照样会红。
       多半是用了路由黑洞（EHOSTUNREACH）而不是 DROP。必须让 SYN 被**静默丢弃**：
           iptables -A OUTPUT -d 192.0.2.0/24 -j DROP"
    ;;
esac
echo

# ── [4/5] ★ ★ 自證公用網路可達 ─────────────────────────────────────────────────
#
# `connectors::http::*` 裡有一批測試**連的是真的 1.1.1.1:443 / :80**
# （`one.one.one.one`，見 pingora-core/src/connectors/http/{mod,v1,v2}.rs）。
# 也就是說這道門**一直掛著一個沒人寫下來的外部前提**，而 §8 要求環境可重現。
#
# ★ ★ 這一步不修那個前提（改上游測試的目標位址是另一件事，見 FORK.md §3 的討論），
#   它做的是**把隱式前提變成顯式前提**：連不上就當場說清楚，
#   而不是讓人對著 6 條名字古怪的失敗去猜是不是自己剛改壞了什麼。
#   ★ 判據同 [3/5]：掛在**行為**上（真連一次），不掛在「有沒有網卡」上。
#
# ── 2026-09-08 owner 拍板：**本機維持現狀，只留這道自證** ──────────────────
#
#   量了一遍才拍的（單變數：只用 iptables REJECT 掉 1.1.1.1，網路棧完整，
#   兩趟都照舊 DROP 192.0.2.0/24 免得 `test_conn_timeout` 在兩趟之間變數不一致）：
#     · 預設 feature：通網 2 紅 → 封掉 7 紅 ⇒ **差集正好 5 條**
#     · `--features rustls`（本門用的）：通網 2 紅 → 封掉 20 紅 ⇒ **差集正好 18 條**
#     （那 2 條基線是 `test_bind_to_port_range_on_connect` 與
#       `test_req_header_no_eos_empty_data_with_eos`，與公用網路無關）
#   ⚠ ⚠ 其中 5 條在**預設 feature** 下就跑 ⇒ 連 `cargo test -p pingora-core` 都要出網。
#
#   ⛔ **沒有改成「容器內假上游 + DNAT」**，雖然基礎設施是現成的（NET_ADMIN + iptables
#   本檔案上下都在用）。兩條理由：① 迄今**沒有一次紅是公用網路抖動引起的**，而這道自證
#   已經把「說不清的失敗」變成「說得清的失敗」；② 劫持到本機假上游會**改變那批測試
#   測的東西**（真實網際網路 TLS 握手 → 本機假上游），拿信號換環境純淨度。
#   ⛔ 也沒有把它們從迴歸網裡摘掉：`FORK.md` §5.3 那條 rustls dev-dependency
#   **就是為這批測試存在的**，摘掉等於把守門人拆了。
#
#   ⇒ 已報上游：`cloudflare/pingora#999`（帶上面這組單變數讀數）。
#     ⚠ 即使被接受也要等發版 + rebase 才完成實作，**解決不了當下** —— 所以本機維持現狀。
echo "=== [4/5] 自证 1.1.1.1 可达（那批 connectors 测试的前提）==="

net_probe() {
  local port=$1 rc=0 t0 ms
  t0=$(date +%s%N)
  timeout 5 bash -c "exec 3<>/dev/tcp/1.1.1.1/$port" 2>/dev/null || rc=$?
  ms=$(( ($(date +%s%N) - t0) / 1000000 ))
  printf '%s %s' "$rc" "$ms"
}

NET_BAD=0
for p in 443 80; do
  read -r rc ms <<EOF
$(net_probe "$p")
EOF
  if [ "$rc" -eq 0 ]; then
    printf '  ✓ 1.1.1.1:%s %sms 连通\n' "$p" "$ms"
  else
    printf '  ✗ 1.1.1.1:%s 连不上（退出码 %s，%sms）\n' "$p" "$rc" "$ms"
    NET_BAD=1
  fi
done

if [ "$NET_BAD" -ne 0 ]; then
  fail "容器上不了公网，而 connectors::http::* 那一批测试连的是真的 1.1.1.1。
       它们会一起失败，而失败信息与真正的原因毫无关系。
       ★ 这不是本仓库引入的依赖，是上游测试自带的；这里只负责**把它说清楚**。
       处置：把网络接通再跑；或 VENDOR_TESTS=0 跳过整个回归网（但那样就没有回归网了）。"
fi
echo

# ── [5/5] 跑測試 ───────────────────────────────────────────────────────────
#
# ★ 判據不是「零失敗」，是「與官方原版 0.9.0 的失敗集合逐項相同」。
#
# 這個容器裡會有測試**在官方原版上同樣失敗**（相依容器外的行為，與 fork 無關）——
# 0.8.1 時是 2 條；0.9.0 在同款容器裡只剩下面「宿主機相關」那 1 條（見「名單的來歷」）。
# 硬要求零失敗會讓這道門**永遠紅**，而永遠亮著的告警等於沒有告警。
#
# ★ ★ 但也不能簡單 --skip 掉：那樣「某條環境性失敗變成了真迴歸」和
#   「環境修好了、該把它從名單裡刪掉」這兩件事都會被吞掉。所以**雙向比對**。
#
# 名單的來歷（可重跑的對照實驗，2026-09-24 rebase 到 0.9.0 時重做）：
#   git -c core.autocrlf=false archive 0.9.0 | tar -x      # 702f690；⚠ 本機全域 autocrlf=true，
#                                                          #   不加 -c 會匯出 CRLF、憑空多出失敗
#   在**本門同款容器**裡（fulcrum-build 鏡像 · NET_ADMIN · 192.0.2.0/24 DROP）：
#   cargo test -p pingora-core -p pingora-error -p pingora-http -p pingora-pool \
#     -p pingora-runtime -p pingora-timeout -p pingora-rustls -p pingora-boringssl \
#     --features pingora-core/rustls --no-fail-fast
#   ⇒ 19 個二進位，735 過 / 1 敗 / 12 忽略；敗的只有 `test_bind_to_port_range_on_connect`
#     （下面 HOST_DEPENDENT_FAILURES 那條）。0.8.1 時名單裡的
#     `protocols::http::v2::server::test::test_req_header_no_eos_empty_data_with_eos`
#     在 0.9.0 上**已通過** ⇒ 本名單清空。
#
# ★ ★ **「登記為環境性失敗」是把問題掛起，不是把問題解決。** 名單裡的每一條都欠著一次
#   根因調查 —— `test_conn_timeout` 掛了很久，查出來是 Docker 預設網路會替
#   `192.0.2.1`（RFC 5737 TEST-NET-1）應答，於是 1ms 的連接超時永遠等不到；
#   docker-run.sh 裝一條 `iptables … -j DROP` 把它變成真丟包之後，這條測試就真的過了。
EXPECTED_FAILURES=""

# ★ ★ ★ 第三類：**宿主機相關**的失敗（CI 上線當天量出來的）。
#
# 它既不是「在官方原版上也失敗」那一類（那是 EXPECTED_FAILURES，兩個方向都嚴），
# 也不是「偶發」那一類（那是下面那段重跑邏輯）。它是**在這臺機器上穩定失敗、
# 在另一臺機器上穩定通過** —— 而兩邊跑的是同一份 fork。
#
# ⚠ ⚠ **一條在不同宿主機上給出相反結論的測試，不能充當迴歸信號**：
#   把它留在 EXPECTED_FAILURES 裡，CI 上會因為「名單裡的這條現在過了」判紅；
#   把它刪掉，本機會因為「出現了名單外的失敗」判紅。**兩邊都對，而門兩邊都紅。**
#
# ── `connectors::l4::tests::test_bind_to_port_range_on_connect` 的實測記錄 ──
#
#   · Docker Desktop / WSL2：**10/10 失敗**　· GitHub Actions（ubuntu-24.04）：**通過**
#
#   根因在斷言本身：測試把源連接埠夾到 **2 個**，順序連 10 次、每次連完就 drop session，
#   然後斷言「至少有一次 AddrNotAvailable」—— 成立與否取決於**宿主機回收源連接埠有多快**。
#   ★ 不在 fork 裡改這條斷言：它是上游測試**要測的那件事本身**（源連接埠夾緊到耗盡），
#     改斷言＝改這條測試的含義，那不叫修，叫讓它閉嘴。
#   ★ 代價說在明處：這一條**不再充當 fork 的迴歸信號**；可接受的理由是 fork 從沒動過
#     `BindTo` / 連接埠範圍那條路徑（見 FORK.md 的改動清單）。
#
# ⚠ **這張表要短，而且每加一條都要帶上「在哪臺機器上量的、錯在哪一行」**。
#   沒有實測記錄的條目就是一句「我猜它是環境問題」—— 那正是
#   `test_conn_timeout` 那條被掛了一年多的形狀。
HOST_DEPENDENT_FAILURES="connectors::l4::tests::test_bind_to_port_range_on_connect"

# ★ ★ ★ （G45 那一輪順手撞出來的）：這裡此前**沒有 `--no-fail-fast`**。
#   `pingora-core` 的 lib 測試恆有 2 條已知失敗，於是 cargo **每一次都停在第一個二進位**——
#   後面 6 個 crate 的單測、pingora-core 自己的 3 個整合測試二進位、以及全部文件測試
#   **一次都沒跑過**，而這道門一直是綠的、指令碼自己 echo 的是「7 個 crate」。
#   實測補上 `--no-fail-fast` 之後多跑出 **64 條單測 + 3 個文件測試**：
#     server_phase_fastshutdown 1 · gracefulshutdown 1 · test_basic 3 ·
#     pingora-error 8 · pingora-http 8 · pingora-pool 9 · pingora-runtime 3 ·
#     pingora-rustls 18 · pingora-timeout 13
#   ★ 是被 G45 逼出來的：那一輪給 pingora-rustls 新寫了 18 條測試，跑完總數卻還是 359——
#     **「新加的測試沒讓計數變化」是這類缺陷唯一會露頭的地方**。
#   ★ 判據本身不用改：失敗集合是掃全部 `test ... FAILED` 行得來的，跨二進位天然成立。
echo "=== [5/5] cargo test（vendor/pingora，8 个 crate + 集成测试 + 文档测试）==="
echo "  ★ 已知环境性失败（在官方 0.9.0 上同样失败，见脚本注释；0.9.0 起为空）："
# ★ 一行一條，**不靠詞拆分**。這與 tests/m0/docker-run.sh 的 CRLF 門是**同一個缺陷**，
#   那邊上午修掉了，這邊直到同日下午 shellcheck 上線才被揪出來——
#   ★ 「修完一個形狀要當場把同形的全掃一遍」，靠人眼掃是掃不乾淨的，得有工具。
# ⚠ ⚠ 循環體必須寫成 `if`，⛔ 不能寫 `[ -n "$ef" ] && printf …`（2026-09-24 實付一次）：
#   名單為空時 `printf '%s\n' ""` 仍給出一行空行 ⇒ 最後一次迭代的 `&&` 列表回傳 1 ⇒ 整個循環回傳 1
#   ⇒ `set -e` 下指令碼當場退出，門禁 RC=1 而一條測試都沒跑。名單非空時它恰好不露頭 ——
#   與本檔案末尾「這裡必須用 if」是同一個形狀。
printf '%s\n' "$EXPECTED_FAILURES" | while IFS= read -r ef; do
  if [ -n "$ef" ]; then printf '      %s\n' "$ef"; fi
done

OUTFILE=${VENDOR_TEST_OUT:-/tmp/vendor-test.out}
# --locked：鎖定檔已入庫，測試不許順手改它（同 G29 第 2 條的精神）
# --target-dir：落進 docker 命名卷，不寫 Windows 掛載點，也不汙染 vendor 目錄
set +e
cargo test --manifest-path "$MANIFEST" --target-dir "$TARGET" --locked \
  --features pingora-core/rustls --no-fail-fast 2>&1 | tee "$OUTFILE"
CARGO_RC=${PIPESTATUS[0]}
set -e

# 編譯不過 / 跑不起來，和「測試跑完有失敗」是兩回事，不能混在一起判
if ! grep -q "^test result:" "$OUTFILE"; then
  fail "cargo test 没能跑到出结果（退出码 $CARGO_RC）——多半是编译错误，看上面的输出"
fi

# 用欄位而不是正則，避開 `test result: FAILED.` 那一行
awk '$1 == "test" && $(NF-1) == "..." && $NF == "FAILED" { print $2 }' "$OUTFILE" \
  | sort -u > /tmp/vendor-actual.f
printf '%s\n' "$EXPECTED_FAILURES" | sed '/^$/d' | sort -u > /tmp/vendor-expected.f
printf '%s\n' "$HOST_DEPENDENT_FAILURES" | sed '/^$/d' | sort -u > /tmp/vendor-hostdep.f

# ★ ★ ★ **自證：宿主機相關名單裡的每一條都必須真的跑過。**
#
# 少了這一條，一個**打錯字的**條目會安靜地什麼都不做：它永遠不匹配任何東西，
# 於是「兩個方向都豁免」變成「豁免了一個不存在的測試」，而門照樣全綠。
# ⚠ 這與本儲存庫反覆抓到的「不自省的名單」是同一個形狀
#   （vendor 那張 16 項手寫清單、`unclaimed.sh` 的忽略名單、`supply-audit.py` 的 ACCEPTED）。
# 判據取「這個名字在測試輸出裡出現過」——出現成 ok 還是 FAILED 都行，那正是本類的定義。
while IFS= read -r hd; do
  [ -n "$hd" ] || continue
  if ! awk -v t="$hd" '$1 == "test" && $2 == t { found = 1 } END { exit !found }' "$OUTFILE"; then
    fail "宿主机相关名单里的这一条 **在本次测试输出里根本没出现过**：$hd
       名字写错了？还是上游把这条测试删了/改名了？
       ★ 一个永远不匹配的豁免条目，与没有这条豁免在门上看不出区别，
         但它会让人以为那条测试还在被看着。"
  fi
done < /tmp/vendor-hostdep.f

# ★ 摘出去之後再比對。⚠ grep 無匹配時回傳 1，set -e 下要接住。
grep -vxF -f /tmp/vendor-hostdep.f /tmp/vendor-actual.f > /tmp/vendor-actual2.f || true
mv /tmp/vendor-actual2.f /tmp/vendor-actual.f

comm -23 /tmp/vendor-actual.f /tmp/vendor-expected.f > /tmp/vendor-new.f
comm -13 /tmp/vendor-actual.f /tmp/vendor-expected.f > /tmp/vendor-gone.f

echo
# ★ 名單外的失敗先**定向重跑一次**再定性。偶發有**兩個已知來源**：
#
#   ① 時間競態。上游的 `protocols::l4::stream::tests::test_rx_timestamp` 自己的註解寫著
#      「setsockopt for SO_TIMESTAMPING is asynchronous so sleep a little bit」，而那個 sleep
#      只有 100µs。空載連跑 12 次全過，機器有負載時會掛在 `assert!(rx_ts.is_some())`。
#
#   ② `connectors::*` 那一批，偶發過幾次而**歸因沒有查實**（「公用網路抖動」解釋不了其中那條
#      純本機的 UDS 測試）。⚠ **在查實之前不許把它們登記進 EXPECTED_FAILURES。**
#
#      ── `connectors::tests::test_connect_uds` 的實測記錄（2026-09-08 查了一層）──
#
#      **它紅在哪一行**：`pingora-core/src/connectors/mod.rs:545` 的 `assert!(reused)`
#      （panic 原話就是 `assertion failed: reused`）⇒ 連接**沒能從池裡重複使用**，
#      `get_stream` 新建了一條。⛔ 與「讀到的位元組不對」無關，那是另一個缺陷、已由
#      FORK.md §3 的 `read_exact` 改動治掉了，⛔ 別把兩者混起來。
#
#      **兩邊各跑了多少趟**（這是本類條目要求的「在哪臺機器上量的」）：
#        · GitHub runner：09-03 紅 · 09-04 過×3 · 09-05 過×2 · 09-08 紅×2 ⇒ **約 3/8，間歇**
#        · `ShanireHomePC`：完整門禁 2 趟 · 單跑該條 `--cpus=2` 3 趟 ·
#          整套 lib 並行 `--cpus=2` 3 趟 ⇒ **0/8，一次都沒紅**
#        ⚠ 兩趟之間 `vendor/` **一個位元組沒動**，runner 鏡像也**逐字相同**
#          （`ubuntu-24.04` / Provisioner `20260828.587` / Image `20260831.293.1`）
#          ⇒ ⛔ 不是程式碼變了，也不是鏡像換了。
#
#      **機制：`reused_stream()` 裡的 TOCTOU —— 2026-09-08 重現並坐實。**
#      `release_stream()` 把**第二個 Arc** 交了出去 —— 它藏在 `OwnedMutexGuard`
#      （`stream.clone().try_lock_owned()`）裡，隨 `rt.spawn(idle_poll(...))` 去了別的執行緒。
#      而 `reused_stream()` 等的是**互斥鎖**（`s.lock().await`），`Arc::try_unwrap(s)`
#      要的卻是**引用計數 == 1**。`OwnedMutexGuard` 析構時**先放鎖、再掉 Arc**，
#      兩步之間另一個執行緒可以醒過來跑到 `try_unwrap` ⇒ 計數仍是 2 ⇒ 走
#      `Err(_) => error!("failed to acquire reusable stream"); None` ⇒ `reused == false`。
#      ★ 這與觀察到的每一條都對得上：`flavor = "multi_thread"`（兩個執行緒才有視窗）、
#        只在整套並行時紅、**單獨重跑必過**、機器越閒越不出現。
#
#      **怎麼坐實的**（⛔ 沒有改 vendor 一個位元組）：寫一個一次性 crate，用 path 相依
#      引 `pingora-core`，**自帶一個計數 `log::Log`** 把那四條路的原話各數一個計數器
#      （vendor 單測不初始化 logger，所以 CI 紀錄裡什麼都沒有 —— 這是它一直查不下去的原因）。
#      ★ 翻轉它的**單一變數是「每次迭代新建一個 `multi_thread` runtime」** ——
#        那正是 `#[tokio::test(flavor = "multi_thread")]` 的形狀：
#          · 共用一個 runtime 死循環：**0 / 18400**（0.5～24 核都試過，一次都不出）
#          · 每次新建 runtime：**出得來**，且隨超訂單調升高（下方劑量-反應）
#      **讀數**（容器內 `--cpus=2`，48 執行緒 × 100 次 = 4800 次）：
#          reused==false 264 · **try_unwrap 失敗 264** ·
#          test_reusable 三支全 0 · matches_fd 不匹配 0 ·
#          入池（find reusable）4800/4800 · matches_fd 匹配 4536
#        ⇒ **4536 + 264 = 4800，每一次嘗試都有著落**（儀器沒漏賬），
#          而 `reused==false` **100% 來自 `Arc::try_unwrap` 失敗**，其餘三條路一次都沒走。
#      **劑量-反應**（`--cpus=2`，越超訂越高）：4 執行緒 0.75% · 16 執行緒 1.50% · 48 執行緒 4.69%
#        ⇒ 與「CI 上 365 條測試並行、runtime 反覆建立銷燬時高發，單獨重跑必過」完全一致。
#      ⚠ 那個一次性 crate **有意不入庫**（G115），留在 `handoff/spike-uds-toctou/`。
#        要重建它只需三樣：path 相依 `pingora-core` · 計數 logger · 每次迭代新建 runtime。
#
#      ⛔ ⛔ **有意沒有把它登記進下面的 `HOST_DEPENDENT_FAILURES`**（owner 2026-09-08 拍板）。
#      兩條理由：① 那張表的判據是「在這臺機器上**穩定**失敗」，而這條是**間歇**（CI 4/9），
#      對不上；② 那張表**兩個方向都不判紅**（`grep -vxF` 直接從 actual 裡摘掉）⇒ 登記進去
#      就**放棄了「它哪天變成真迴歸時判紅」的能力**。
#      ⚠ 歸因現在**已經查實**（見上），⛔ 但那**不**改變結論：機制是並行視窗，
#        ⇒ 它平時是過的 ⇒ 按本節開頭那條，**也不能塞進 `EXPECTED_FAILURES`**
#        （「它平時是過的，塞進去會讓『少一條也紅』每次都觸發」）。
#      ⇒ 仍然留給下面那段重跑邏輯管：重跑仍失敗才判紅，且每次都打「它確實紅過一次」。
#      ★ 真要根治，方向在**上游**：`release_stream()` 不該把第二個 Arc 交給 spawn 出去的任務，
#        或 `reused_stream()` 不該用 `Arc::try_unwrap` 這種對引用計數敏感的取法。
#        ⛔ 本儲存庫不改 vendor 去修它 —— 那是上游的缺陷，改了要自己背 rebase 成本。
#      ⇒ **已報上游：`cloudflare/pingora#998`**（2026-09-08，含兩份重現與逐檔讀數）。
#        ⚠ 上游 `main`（`09696b5`）與 0.8.1 在這三個函式上**逐字相同** ⇒ 升 vendor 不會自動解決。
#        ★ 2026-09-24 升到 0.9.0 時重核：形狀**沒變** —— `reused_stream` 仍是 `Arc::try_unwrap`，
#          `release_stream` 仍把 `try_lock_owned()` 出來的第二個 Arc 交給 spawn 出去的 `idle_poll`；
#          變的只有 `test_reusable_stream` 多了一個計數參數與一個 keepalive 回呼 ⇒ #998 照舊成立。
#        ★ 它與 `#967`（短讀，本儲存庫已用 `read_exact` 治掉）是**同一條測試的兩個獨立原因**，
#          ⛔ 別把兩者混成一件事。
#
#   ★ 一條相關事實：`connectors::*` 裡那批測試**連的是真的 1.1.1.1:443 / :80**
#     （見 `pingora-core/src/connectors/http/{mod,v1,v2}.rs`）——**這道門一直悄悄相依容器
#     能上公用網路**，而 §8 要求環境可重現。真要處置，方向是把它們指向容器內的假上游。
#
#   ★ ★ **不能把偶發項塞進 EXPECTED_FAILURES**——它平時是過的，塞進去會讓「少一條也紅」
#     那條規則每次都觸發。也不能放任它間歇性紅：那會訓練人「重跑一次就好」，
#     等於廢掉這道門。所以：重跑一次，仍失敗才判紅，**而且不穩定要打出來**。
if [ -s /tmp/vendor-new.f ]; then
  echo "★ 名单外的失败，定向重跑一次以区分「真回归」与「偶发（时间竞态 / 公网抖动）」："
  sed 's/^/      /' /tmp/vendor-new.f
  RETRY_ARGS=()
  while IFS= read -r t; do RETRY_ARGS+=("$t"); done < /tmp/vendor-new.f
  set +e
  cargo test --manifest-path "$MANIFEST" --target-dir "$TARGET" --locked \
    --features pingora-core/rustls --no-fail-fast \
    -- --exact "${RETRY_ARGS[@]}" > /tmp/vendor-retry.out 2>&1
  set -e
  awk '$1 == "test" && $(NF-1) == "..." && $NF == "FAILED" { print $2 }' /tmp/vendor-retry.out \
    | sort -u > /tmp/vendor-still.f
  comm -23 /tmp/vendor-new.f /tmp/vendor-still.f > /tmp/vendor-flaky.f
  if [ -s /tmp/vendor-flaky.f ]; then
    echo "  ⚠ 重跑通过 —— 判定为**不稳定**，不判红（但请注意它确实红过一次）："
    sed 's/^/      /' /tmp/vendor-flaky.f
  fi
  cp /tmp/vendor-still.f /tmp/vendor-new.f
  if [ -s /tmp/vendor-new.f ]; then
    echo "  ★ 重跑仍失败 —— 这才是 fork 该被判红的东西："
    sed 's/^/      /' /tmp/vendor-new.f
  fi
fi
if [ -s /tmp/vendor-gone.f ]; then
  echo "★ 名单里的这几条**现在过了**——环境变了，把它们从 EXPECTED_FAILURES 删掉："
  sed 's/^/      /' /tmp/vendor-gone.f
fi
# ★ ★ 宿主機相關的那幾條：兩個方向都不判紅，但**必須說出這一輪是哪個方向**。
#   一條被豁免又不被報告的測試，等於從這道門上消失了。
if [ -s /tmp/vendor-hostdep.f ]; then
  echo "★ 宿主机相关（两个方向都不判红，见脚本里的实测记录）："
  while IFS= read -r hd; do
    [ -n "$hd" ] || continue
    if awk -v t="$hd" '$1 == "test" && $2 == t && $NF == "FAILED" { f = 1 } END { exit !f }' "$OUTFILE"; then
      printf '      本轮**失败**：%s\n' "$hd"
    else
      printf '      本轮**通过**：%s\n' "$hd"
    fi
  done < /tmp/vendor-hostdep.f
fi
# ★ 這裡必須用 if，不能寫 `[ -s f ] && fail ...`——set -e 下條件為假時
#   整條 && 列表回傳 1，指令碼會當場靜默退出，紅綠全亂。
if [ -s /tmp/vendor-new.f ]; then fail "出现了官方原版上没有的失败"; fi
if [ -s /tmp/vendor-gone.f ]; then fail "已知失败名单已过期，请更新"; fi

echo "VENDOR TESTS PASSED —— 失败集合与官方原版 0.9.0 逐项相同（宿主机相关的那几条已单列），fork 没有引入新的回归。"
