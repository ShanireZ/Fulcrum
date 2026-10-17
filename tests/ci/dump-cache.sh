#!/usr/bin/env bash
# 把建置快取匯出成一個歸檔，並**如實報告匯出成功沒有**（G94）。
#
# ★ ★ **為什麼它不寫在 workflow 的 `run:` 塊裡**（G94）：`docker-run.sh` 的 `LINT_CMD`
#   掃的是 `tests/**/*.sh`，寫在 workflow 裡就**一行都沒人 lint** —— 而那個「結束代碼恆為 0」
#   的 bug 正是活在那種地方。搬出來還收掉第二樣：它**能在開發機上跑**。
#   ⚠ 那條修法（`SAVE_RC=${PIPESTATUS[0]}` 緊跟管道）在 CI 上沒有判據（它住在一個
#   `cache-hit != 'true'` 才執行的步驟裡）⇒ 本檔案底部的 `--self-check` 就是替它補的那道。
#
# 用法：
#   bash tests/ci/dump-cache.sh <缓存目录> [診斷檔案]
#   bash tests/ci/dump-cache.sh --self-check       # 不碰 docker，只驗結束代碼取法
#
# 結束代碼：0 = 跑完了（**不代表匯出成功**）。匯出成功與否看 stdout 的 `archived=`。
# ★ 這條口徑是有意的，與本儲存庫那條「結束代碼只判命令跑沒跑起來，結果綠不綠看報告正文」一致。
set -euo pipefail

# ── --self-check：不碰 docker，只釘住「結束代碼是怎麼取的」──────────────────────
#
# ★ ★ 這一段就是那條修法的判據。它重現 Actions 的 shell（`bash -e`，**不帶 pipefail**），
#   把一個必然失敗的命令接上 `| tee`，然後用**兩種寫法**去取結束代碼：
#     · 舊寫法（`if 管道; then …; fi` 之後取 PIPESTATUS）—— 必須讀到 0（錯的）
#     · 新寫法（管道之後緊接著取）           —— 必須讀到 70（對的）
#   ⚠ 兩個方向都釘：只釘新寫法的話，一個恆回傳 70 的實作也會綠。
if [ "${1:-}" = "--self-check" ]; then
  fails=0
  tmp=$(mktemp)
  trap 'rm -f "$tmp"' EXIT

  # 舊寫法：`fi` 之前最後執行的是 `RC=0` 這條**簡單命令**，
  # 於是 `fi` 之後的 PIPESTATUS 已經是 `(0)` —— 恆把結束代碼覆寫成 0。
  old_way() {
    local RC
    if (exit 70) 2>&1 | tee -a "$tmp" >/dev/null; then
      RC=0
    else
      RC=1
    fi
    RC=${PIPESTATUS[0]:-$RC}
    echo "$RC"
  }
  # 新寫法：管道之後**緊接著**取，中間一條命令都沒有。
  new_way() {
    local RC
    set +e
    (exit 70) 2>&1 | tee -a "$tmp" >/dev/null
    RC=${PIPESTATUS[0]}
    set -e
    echo "$RC"
  }

  got_old=$(old_way)
  got_new=$(new_way)
  if [ "$got_old" = "0" ]; then
    echo "  ✓ 旧写法确实读到 0（这就是那个 bug 的形状）"
  else
    echo "  ✗ 旧写法读到 $got_old —— 本该是 0。反证不成立了，这条自证已经证不到东西" >&2
    fails=$((fails + 1))
  fi
  if [ "$got_new" = "70" ]; then
    echo "  ✓ 新写法读到 70（真实退出码传得出来）"
  else
    echo "  ✗ 新写法读到 $got_new —— 本该是 70" >&2
    fails=$((fails + 1))
  fi
  # ★ 第三條：兩種寫法**必須給出不同的答案**。少了它，一個把兩個函式寫成
  #   同一件事的"重構"會讓上面兩條同時綠或同時紅，而看不出它們已經不再對照。
  if [ "$got_old" != "$got_new" ]; then
    echo "  ✓ 两种写法给出不同答案（$got_old ≠ $got_new）—— 这条对照仍然是活的"
  else
    echo "  ✗ 两种写法给出了同一个答案（$got_old）—— 对照已经失效" >&2
    fails=$((fails + 1))
  fi

  echo
  if [ "$fails" -ne 0 ]; then
    echo "DUMP-CACHE SELF-CHECK FAILED: $fails 条不通过" >&2
    exit 1
  fi
  echo "DUMP-CACHE SELF-CHECK PASSED —— 退出码取法两个方向各钉一条，且对照仍然有效。"
  exit 0
fi

CACHE_DIR=${1:?用法：dump-cache.sh <缓存目录> [诊断文件]}
DIAG=${2:-$HOME/cache-diag.txt}
REPO=${REPO:-$(pwd)}

: > "$DIAG"
say() { echo "$@" | tee -a "$DIAG"; }

# ⚠ ⚠ 只量**毫秒級**的那幾樣。那版診斷（`docker system df -v` +
#   逐卷統計）把匯出從 5m30s 拖到 24m54s、整輪 CI 到 40m02s —— 一個為了查清
#   問題而把問題變貴的診斷，當天就收窄成了下面這三行。
say "── 导出前的磁盘 ──"
df -h / 2>&1 | tee -a "$DIAG"
say "── docker 占了多少 ──"
docker system df 2>&1 | tee -a "$DIAG" || true

# ★ 門禁已經跑完了，鏡像這時候一個都不需要留：它們本來就不跨輪快取，
#   每一輪都要重建。搬運用的 helper 會被 cache.sh 重新拉（幾十 MB）。
#   ⚠ 這一步不影響任何判據，純粹是騰地方。
say "── 清掉 builder 缓存与所有镜像 ──"
docker builder prune -af >/dev/null 2>&1 || true
docker image prune -af >/dev/null 2>&1 || true
df -h / 2>&1 | tee -a "$DIAG"

# ⚠ ⚠ **判據取「命令成功了沒有」，不取「檔案在不在」。**
#   實測：一次 cancel-in-progress 把門禁掐了，匯出隨之失敗，
#   **而半截歸檔已經寫入磁碟**（370MB，完整的是 2.2GB）——
#   按「檔案在不在」判會把這份殘檔存進快取鍵，而快取條目不可變，
#   此後每一輪都精確命中它。**「檔案存在」不是「匯出成功」。**
say "── cache.sh save ──"
# ⚠ ⚠ ⚠ **不許寫回 `if 管道; then …; fi` 再在 `fi` 之後取 PIPESTATUS**
#   （之前的寫法，兩處都是壞的，而且互相掩蓋）：
#   ① Actions 的預設 shell 是 `bash -e {0}`，**不帶 pipefail** ⇒ 管道的結束代碼是
#      `tee` 的，`if` 永遠走 then 分支，`cache.sh` 失敗根本傳不出來；
#   ② 想補救 ① 的那行 `SAVE_RC=${PIPESTATUS[0]:-$SAVE_RC}` 寫在 `fi` **之後**，
#      而 `fi` 之前最後執行的是 `SAVE_RC=0` 這條**簡單命令** ⇒ 那時 PIPESTATUS
#      已經是 `(0)`，這一行**恆把結束代碼覆寫成 0**。
#   ⇒ 唯一可靠的寫法：**管道之後緊接著取 PIPESTATUS，中間不許有任何命令。**
#   ★ 這條由本檔案的 `--self-check` 兩個方向各釘一條（G94）。
set +e
bash "$REPO/tests/ci/cache.sh" save "$CACHE_DIR" 2>&1 | tee -a "$DIAG"
SAVE_RC=${PIPESTATUS[0]}
set -e
say "cache.sh save 退出码：$SAVE_RC"
# ⚠ 不要寫成 `ls -la`（shellcheck SC2012）—— 這段程式碼當初在 workflow 的 `run:` 塊裡時
#   一次都沒被掃過，G94 要收的正是這個。
# ⚠ 順帶一條：註解行以「# 空格 shellcheck」開頭會被當成**指令**解析（SC1073）。
#   這條註解自己就踩過一次。
find "$CACHE_DIR" -maxdepth 1 -type f -printf '%10s  %f\n' 2>&1 | tee -a "$DIAG" || true

# ★ ★ **兩道判據分開量，打架就說出來。**
#   寫成「結束代碼為 0 **且** 歸檔檔案在」一條與關係的話，壞掉的那道
#   （恆 0 的結束代碼）會被好的那道（`cache.sh` 失敗時自己刪掉半截歸檔）接住，
#   **連著好幾輪 CI 都不顯形**。⇒ 把「兩道判據不一致」本身打成一行診斷：
#   它一旦出現，就說明其中一道壞了 —— 而不是等下一次僥倖用完。
ARCHIVED_FILE=no
[ -f "$CACHE_DIR/fulcrum-build-cache.tar.z" ] && ARCHIVED_FILE=yes
if [ "$SAVE_RC" = "0" ] && [ "$ARCHIVED_FILE" = "no" ]; then
  say "★ ★ 两道判据打架：退出码说成功，而归档文件不在 —— 先看这一步的退出码是怎么取的"
fi
if [ "$SAVE_RC" != "0" ] && [ "$ARCHIVED_FILE" = "yes" ]; then
  say "★ ★ 两道判据打架：退出码说失败，而半截归档还在 —— cache.sh 的清理路径可能变了"
fi

if [ "$SAVE_RC" = "0" ] && [ "$ARCHIVED_FILE" = "yes" ]; then
  ARCHIVED=true
  say "archived=true"
else
  ARCHIVED=false
  say "★ archived=false —— 不存缓存（宁可下一轮冷跑，也不占住那个键）"
fi
say "── 导出后的磁盘 ──"
df -h / 2>&1 | tee -a "$DIAG"

# ── 報給呼叫方 ──────────────────────────────────────────────────────────────
#
# ★ 在 Actions 裡寫 step output；在開發機上只打到 stdout。
#   ⚠ 兩條路都走，是為了讓這個指令碼在兩邊**行為一樣** —— 一個只在 CI 上
#   才有輸出的指令碼，在開發機上跑一遍等於什麼都沒驗。
echo "archived=$ARCHIVED"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "archived=$ARCHIVED" >> "$GITHUB_OUTPUT"
fi
# 同一份診斷也塞進 job summary，兩條路都留著。
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  { echo '```text'; cat "$DIAG"; echo '```'; } >> "$GITHUB_STEP_SUMMARY" || true
fi
