#!/usr/bin/env bash
# 对拍编排（M3 第一刀，G132）。**跑在对拍容器里**（docker/Dockerfile.bench）。
#
#   bash bench/run.sh [输出目录]        # 缺省 bench/out
#
# 四步，顺序是承重的：
#
#   ① 判据自测   —— 判据自己先证明它两个方向都判得动（合成输入）
#   ② 环境快照   —— 采集读数并**判宿主合格性**
#   ③ 逐类跑     —— 只产出原始数据，⛔ 不判定
#   ④ 判定       —— 宿主不合格就结构性地不出结论
#
# ★ ★ ★ ① 排在最前不是为了好看：如果判据本身坏了，后面三步产出的一切都不可信，
#   而**一个坏掉的判据最典型的形态就是它恒返回同一个答案** —— 那种坏法在
#   ②③④ 里长得和「一切正常」一模一样。
#
# ★ ★ **G132 的「本轮不产出任何性能数字」从来不是靠那句话守着的，是靠 ② 与 ④ 的
#   形状守着的。** 2026-09-06 合格宿主到位、第一组读数出来了 —— ⛔ 那不是它被违反，
#   恰恰是它按设计工作：不合格的宿主在 ② 里就被判死，④ 于是结构上不算结论；
#   数字只在真的过了那六条判据的机器上才出得来。
#   ⇒ 在开发机上跑本脚本，今天仍然一个数字都出不来。
#
# ⚠ 第三方复现请读 `bench/README.md` —— 它才是口径，本文件只是编排。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR=${1:-$BENCH_DIR/out}

# ★ 每一趟都从干净的输出目录开始：上一趟留下的 raw/ 会被这一趟的判定读进去，
#   而那份数据可能来自另一套参数、另一台机器 —— 混在一起没有任何东西会说。
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

echo "── ① 判据自测 ──"
bash "$BENCH_DIR/lib.sh" --self-check

echo
echo "── ② 环境快照与宿主合格性 ──"
bash "$BENCH_DIR/env-snapshot.sh" "$OUT_DIR/env.json"

echo
echo "── ③ 逐类跑（只产出原始数据）──"
# ★ 用例集**推导出来，⛔ 不写死清单**：写死的清单在加第二类时没人会想起来去改，
#   而它安静地少跑一类，输出看起来完全正常（同 tests/ci/shellcheck-all.sh 的教训）。
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
# ★ 与 ③ 同一条纪律：**推导出来，⛔ 不写死清单**。
# ⚠ ⚠ 输出落 `diag/`，⛔ 不落 `raw/` —— `verdict.sh` 把 `raw/` 下每个子目录都当成
#   一类去判，而这些步骤只有枢衡自己。两个方向由 `tests/bench/gate.sh` 的 C17 钉着。
#
# ⚠ ⚠ ★ **失败不在这里中止**：诊断是附加读数，而 ④ 是 §8 的结论。
#   让一个诊断步骤的失败把整组对拍结论扣掉，在合格宿主的窗口里意味着白停一次服。
#   ⇒ 记下来，先把 ④ 跑完，最后再据它退非 0。
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
# ⚠ `verdict.sh` 在「宿主合格且某一类没达标」时返回非 0。**那是它该做的事**，
#   而本轮宿主必然不合格 ⇒ 它必然返回 0。⛔ 别在这里把它的退出码吞掉：
#   吞掉之后，将来真的在合格宿主上跑出一个 FAIL 时，本脚本会照常报绿。
bash "$BENCH_DIR/verdict.sh" "$OUT_DIR"

# ★ ③′ 那一步的失败留到这里才报：判定已经出过结论了，⛔ 而它不该被诊断扣掉。
#   ⚠ 走到这里说明 `verdict.sh` 是 0；它非 0 时 `set -e` 已经在上一行退出了。
[ "$DIAG_RC" = 0 ] || {
  echo "BENCH FAILED: ③′ 诊断读数那一步有失败（见上）—— §8 的判定已经照常出过结论" >&2
  exit 1
}
