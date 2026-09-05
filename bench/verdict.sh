#!/usr/bin/env bash
# 判定（M3 第一刀，G132 的交付物之三）。
#
#   bash bench/verdict.sh <输出目录>
#
# ★ ★ ★ **它是唯一会说出「达标 / 不达标」的地方，而它先问宿主合不合格。**
#   宿主不合格 ⇒ 逐条说出为什么，然后**结构性地不算任何结论**：
#   不取最强者、不算门槛、不打 PASS / FAIL。⛔ 这不是纪律，是这段代码的形状。
#
#   ⚠ ⚠ 它**不是**一个开关：`bench/env-snapshot.sh` 写出来的那份 env.json 是
#   本脚本唯一的输入来源，而那份文件里的 `qualified` 由 `bench/lib.sh` 的
#   纯函数判据算出来。⇒ 想让它出结论，只有一条路：**换一台真的合格的宿主**。
#
# ★ 反证怎么做：本脚本吃的是 `<输出目录>` 里的文件 ⇒ 喂一份**合成的**
#   合格 env.json 加一组合成原始数据，它必须真的打出 PASS / FAIL。
#   ⛔ 一个永远拒绝出结论的判定器，与一个坏掉的判定器给出完全相同的输出。
#   那条反证在 `tests/bench/run.sh` 里，每趟都跑。

set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/verdict.sh <输出目录>}
ENV_JSON="$OUT_DIR/env.json"
VERDICT_TXT="$OUT_DIR/verdict.txt"

[ -f "$ENV_JSON" ] || {
  echo "VERDICT FAILED: 找不到 $ENV_JSON —— 没有环境快照就没有判定" >&2
  exit 1
}

# ★ 用 python3 读 JSON，⛔ 不用 grep：`"qualified": false` 与 `"qualified": true`
#   之间只差几个字节，而一个写歪的 grep 在两种情况下给出同一个答案。
read_json_field() {
  OUT_DIR="$OUT_DIR" python3 -c '
import json, os, sys, pathlib
d = json.loads((pathlib.Path(os.environ["OUT_DIR"]) / "env.json").read_text())
key = sys.argv[1]
v = d.get(key)
if isinstance(v, list):
    print("\n".join(str(x) for x in v))
elif isinstance(v, bool):
    print("true" if v else "false")
elif v is None:
    print("")
else:
    print(v)
' "$1"
}

QUALIFIED=$(read_json_field qualified)
DISQ=$(read_json_field disqualifiers)

{
  echo "# 枢衡对拍判定"
  echo "# 环境快照：env.json"
  echo
} > "$VERDICT_TXT"

# ── 宿主不合格：说清楚为什么，然后停在这里 ──────────────────────────────────
if [ "$QUALIFIED" != "true" ]; then
  {
    echo "VERDICT: UNQUALIFIED"
    echo
    echo "这台宿主不满足对拍的可复现性要求，⇒ 本轮**不产出任何性能结论**。"
    echo "原始数据仍然完整落盘（raw/ 下），它证明的是**流水线跑得通**，"
    echo "⛔ 它不是、也不许被引用成性能读数。"
    echo
    echo "不合格的理由："
    printf '%s\n' "$DISQ" | sed 's/^/  · /'
    echo
    echo "怎样才算合格：见 bench/README.md「合格宿主」一节。"
  } >> "$VERDICT_TXT"
  echo "[bench/verdict] **UNQUALIFIED** —— 不出结论。理由："
  printf '%s\n' "$DISQ" | sed 's/^/               · /'
  echo "[bench/verdict] 已写入 $VERDICT_TXT"
  exit 0
fi

# ── 宿主合格：逐类判定 ─────────────────────────────────────────────────────
#
# ⚠ 走到这里说明五条判据全部成立。★ 下面每一类各判一次，⛔ 不跨类取最强者。
RC=0
CASES=0
for raw_case in "$OUT_DIR"/raw/*/; do
  [ -d "$raw_case" ] || continue
  CASES=$((CASES + 1))
  case_name=$(basename "$raw_case")
  readings=$(python3 "$BENCH_DIR/read-raw.py" "$raw_case")

  ours=$(printf '%s\n' "$readings" | awk '$1 == "fulcrum" { print $2 }')
  # ⚠ **竞品集合是「除枢衡以外的全部」，⛔ 不是一张写死的名单**：
  #   写死的名单在加第四家时没人会想起来去改，而它安静地少比一家。
  rivals=$(printf '%s\n' "$readings" | awk '$1 != "fulcrum" { print $1, $2 }')

  # 无效读数一律先摊开说，⛔ 不许它们悄悄退场。
  invalid=$(printf '%s\n' "$readings" | awk '$2 == "INVALID" { print }')
  if [ -n "$invalid" ]; then
    {
      echo "## $case_name"
      echo "VERDICT: NO-VERDICT（有读数无效，比较无意义）"
      printf '%s\n' "$invalid" | sed 's/^/  · /'
      echo
    } >> "$VERDICT_TXT"
    echo "[bench/verdict] $case_name：有读数无效 ⇒ 不判定"
    RC=1
    continue
  fi

  if [ -z "$ours" ]; then
    {
      echo "## $case_name"
      echo "VERDICT: NO-VERDICT（这一类里没有枢衡自己的读数）"
      echo
    } >> "$VERDICT_TXT"
    RC=1
    continue
  fi

  line=$(bench_verdict_one "$ours" "$rivals")
  # ★ 显式拆成具名变量，⛔ 不用 `set -- $line` —— 那要靠词分割，而位置参数
  #   在下面那段里读起来像是命令行参数，改动时极易接错一格。
  read -r v_status v_best_name v_best_val v_floor v_ours <<< "$line"
  if [ "$v_status" = "NODATA" ]; then
    {
      echo "## $case_name"
      echo "VERDICT: NO-VERDICT（一家竞品的读数都没有）"
      echo
    } >> "$VERDICT_TXT"
    RC=1
    continue
  fi
  {
    echo "## $case_name"
    echo "VERDICT: $v_status"
    echo "  该类最强者：$v_best_name = $v_best_val"
    echo "  门槛（最强者 × 0.9，G19「不劣于 10%」）：$v_floor"
    echo "  枢衡：$v_ours"
    echo "  全部读数："
    printf '%s\n' "$readings" | sed 's/^/    /'
    echo
  } >> "$VERDICT_TXT"
  echo "[bench/verdict] $case_name：$v_status（最强者 $v_best_name=$v_best_val，门槛 $v_floor，枢衡 $v_ours）"
  [ "$v_status" = "PASS" ] || RC=1
done

if [ "$CASES" = 0 ]; then
  echo "VERDICT FAILED: $OUT_DIR/raw/ 下一类用例都没有" >&2
  exit 1
fi

echo "[bench/verdict] 已写入 $VERDICT_TXT"
exit "$RC"
