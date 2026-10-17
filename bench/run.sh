#!/usr/bin/env bash
# 對拍編排（M3 第一刀，G132）。**跑在對拍容器裡**（docker/Dockerfile.bench）。
#
#   bash bench/run.sh [輸出目錄]        # 預設 bench/out
#
# 四步，順序是承重的：
#
#   ① 判據自測   —— 判據自己先證明它兩個方向都判得動（合成輸入）
#   ② 環境快照   —— 採集讀數並**判宿主合格性**
#   ③ 逐類跑     —— 只產出原始資料，⛔ 不判定
#   ④ 判定       —— 宿主不合格就結構性地不出結論
#
# ★ ★ ★ ① 排在最前不是為了好看：如果判據本身壞了，後面三步產出的一切都不可信，
#   而**一個壞掉的判據最典型的形態就是它恆回傳同一個答案** —— 那種壞法在
#   ②③④ 裡長得和「一切正常」一模一樣。
#
# ★ ★ **G132 的「本輪不產出任何效能數字」從來不是靠那句話守著的，是靠 ② 與 ④ 的
#   形狀守著的。** 2026-09-06 合格宿主到位、第一組讀數出來了 —— ⛔ 那不是它被違反，
#   恰恰是它按設計工作：不合格的宿主在 ② 裡就被判死，④ 於是結構上不算結論；
#   數字只在真的過了那六條判據的機器上才出得來。
#   ⇒ 在開發機上跑本指令碼，今天仍然一個數字都出不來。
#
# ⚠ 第三方重現請讀 `bench/README.md` —— 它才是口徑，本檔案只是編排。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR=${1:-$BENCH_DIR/out}

# ★ 每一趟都從乾淨的輸出目錄開始：上一趟留下的 raw/ 會被這一趟的判定讀進去，
#   而那份資料可能來自另一套參數、另一臺機器 —— 混在一起沒有任何東西會說。
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

echo "── ① 判据自测 ──"
bash "$BENCH_DIR/lib.sh" --self-check

echo
echo "── ② 环境快照与宿主合格性 ──"
bash "$BENCH_DIR/env-snapshot.sh" "$OUT_DIR/env.json"

echo
echo "── ③ 逐类跑（只产出原始数据）──"
# ★ 案例集**推匯出來，⛔ 不寫死清單**：寫死的清單在加第二類時沒人會想起來去改，
#   而它安靜地少跑一類，輸出看起來完全正常（同 tests/ci/shellcheck-all.sh 的教訓）。
CASE_N=0
for case_sh in "$BENCH_DIR"/case/*.sh; do
  [ -f "$case_sh" ] || continue
  CASE_N=$((CASE_N + 1))
  bash "$case_sh" "$OUT_DIR"
done
[ "$CASE_N" -gt 0 ] || {
  echo "BENCH FAILED: bench/case/ 下一个用例都没有 —— 这一趟什么都没量" >&2
  exit 1
}

echo
echo "── ③′ 诊断读数（⛔ 不是 §8 的类别，不参与判定）──"
# ★ 與 ③ 同一條紀律：**推匯出來，⛔ 不寫死清單**。
# ⚠ ⚠ 輸出落 `diag/`，⛔ 不落 `raw/` —— `verdict.sh` 把 `raw/` 下每個子目錄都當成
#   一類去判，而這些步驟只有樞衡自己。兩個方向由 `tests/bench/gate.sh` 的 C17 釘著。
#
# ⚠ ⚠ ★ **失敗不在這裡中止**：診斷是附加讀數，而 ④ 是 §8 的結論。
#   讓一個診斷步驟的失敗把整組對拍結論扣掉，在合格宿主的視窗裡意味著白停一次服。
#   ⇒ 記下來，先把 ④ 跑完，最後再據它退非 0。
DIAG_N=0
DIAG_RC=0
for diag_sh in "$BENCH_DIR"/diag/*.sh; do
  [ -f "$diag_sh" ] || continue
  DIAG_N=$((DIAG_N + 1))
  bash "$diag_sh" "$OUT_DIR" || DIAG_RC=1
done
[ "$DIAG_N" -gt 0 ] || {
  echo "BENCH FAILED: bench/diag/ 下一个诊断步骤都没有 —— 它们是被谁删掉了吗" >&2
  exit 1
}

echo
echo "── ④ 判定 ──"
# ⚠ `verdict.sh` 在「宿主合格且某一類沒達標」時回傳非 0。**那是它該做的事**，
#   而本輪宿主必然不合格 ⇒ 它必然回傳 0。⛔ 別在這裡把它的結束代碼吞掉：
#   吞掉之後，將來真的在合格宿主上跑出一個 FAIL 時，本指令碼會照常報綠。
bash "$BENCH_DIR/verdict.sh" "$OUT_DIR"

# ★ ③′ 那一步的失敗留到這裡才報：判定已經出過結論了，⛔ 而它不該被診斷扣掉。
#   ⚠ 走到這裡說明 `verdict.sh` 是 0；它非 0 時 `set -e` 已經在上一行退出了。
[ "$DIAG_RC" = 0 ] || {
  echo "BENCH FAILED: ③′ 诊断读数那一步有失败（见上）—— §8 的判定已经照常出过结论" >&2
  exit 1
}
