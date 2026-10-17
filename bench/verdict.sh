#!/usr/bin/env bash
# 判定（M3 第一刀，G132 的交付物之三）。
#
#   bash bench/verdict.sh <输出目录>
#
# ★ ★ ★ **它是唯一會說出「達標 / 不達標」的地方，而它先問宿主合不合格。**
#   宿主不合格 ⇒ 逐條說出為什麼，然後**結構性地不算任何結論**：
#   不取最強者、不算門檻、不打 PASS / FAIL。⛔ 這不是紀律，是這段程式碼的形狀。
#
#   ⚠ ⚠ 它**不是**一個開關：`bench/env-snapshot.sh` 寫出來的那份 env.json 是
#   本指令碼唯一的輸入來源，而那份檔案裡的 `qualified` 由 `bench/lib.sh` 的
#   純函式判據算出來。⇒ 想讓它出結論，只有一條路：**換一臺真的合格的宿主**。
#
# ★ 反證怎麼做：本指令碼吃的是 `<输出目录>` 裡的檔案 ⇒ 喂一份**合成的**
#   合格 env.json 加一組合成原始資料，它必須真的打出 PASS / FAIL。
#   ⛔ 一個永遠拒絕出結論的判定器，與一個壞掉的判定器給出完全相同的輸出。
#   那條反證在 `tests/bench/run.sh` 裡，每趟都跑。

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

# ★ 用 python3 讀 JSON，⛔ 不用 grep：`"qualified": false` 與 `"qualified": true`
#   之間只差幾個位元組，而一個寫歪的 grep 在兩種情況下給出同一個答案。
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

# ── 宿主不合格：說清楚為什麼，然後停在這裡 ──────────────────────────────────
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

# ── 宿主合格：逐類判定 ─────────────────────────────────────────────────────
#
# ⚠ 走到這裡說明五條判據全部成立。★ 下面每一類各判一次，⛔ 不跨類取最強者。
RC=0
CASES=0
for raw_case in "$OUT_DIR"/raw/*/; do
  [ -d "$raw_case" ] || continue
  CASES=$((CASES + 1))
  case_name=$(basename "$raw_case")
  readings=$(python3 "$BENCH_DIR/read-raw.py" "$raw_case")

  ours=$(printf '%s\n' "$readings" | awk '$1 == "fulcrum" { print $2 }')
  # ⚠ **競品集合是「除樞衡以外的全部」，⛔ 不是一張寫死的名單**：
  #   寫死的名單在加第四家時沒人會想起來去改，而它安靜地少比一家。
  rivals=$(printf '%s\n' "$readings" | awk '$1 != "fulcrum" { print $1, $2 }')

  # 無效讀數一律先攤開說，⛔ 不許它們悄悄退場。
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

  # ── 判據 ④：壓測端自己先飽和 ────────────────────────────────────────────
  #
  # ★ ★ ★ **位置是承重的：它必須排在 `bench_verdict_one` 之前。** 它守的恰恰是
  #   「判據 ③ 會照常打 PASS」的那個失效 —— 排在後面等於先出一個結論、再說
  #   那組數其實不可比，而先打出來的那個 PASS 已經會被人引用了。
  #
  # ★ 生成器上限是**可選輸入**（判據 A）：`raw/<类>/ceiling.txt` 裡一個數，
  #   沒有這個檔案時 A 完全不參與、B 照常判。
  # ⚠ ⚠ ⛔ **有意不叫 `ceiling.json`** —— `read-raw.py` 對這個目錄做的是
  #   `glob("*.json")`，一個 `.json` 會被它當成**第五家被測**混進競品集合，
  #   於是「最強者」可能變成那個參照服務，而**不會有任何東西說**。
  ceiling=
  if [ -f "$raw_case/ceiling.txt" ]; then
    ceiling=$(tr -d ' \n\r' < "$raw_case/ceiling.txt")
  fi
  # ⚠ 喂進去的是**全場**讀數（含樞衡自己），⛔ 不是 `rivals`：飽和是整組數的性質，
  #   而樞衡自己頂到天花板同樣讓這一類不可比。
  saturation=$(bench_saturation "$ceiling" "$readings")
  if [ -n "$saturation" ]; then
    {
      echo "## $case_name"
      echo "VERDICT: VOID（这一类的读数不可比 ⇒ 结构性地不出结论）"
      printf '%s\n' "$saturation" | sed 's/^/  · /'
      echo "  全部读数："
      printf '%s\n' "$readings" | sed 's/^/    /'
      echo
    } >> "$VERDICT_TXT"
    echo "[bench/verdict] $case_name：**VOID** —— 压测端饱和判据不通过 ⇒ 不判定"
    printf '%s\n' "$saturation" | sed 's/^/               · /'
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
  # ★ 顯式拆成具名變數，⛔ 不用 `set -- $line` —— 那要靠詞分割，而位置參數
  #   在下面那段裡讀起來像是命令行參數，改動時極易接錯一格。
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
  # ── 判據 ④C：最強者附近的收斂（`G146`，結案 `D34`）──────────────────────
  #
  # ★ ★ ★ **位置也是承重的，而它與 B 恰好相反：它必須排在 `bench_verdict_one`
  #   之後**，因為它只威脅 PASS、永遠不威脅 FAIL ——
  #     門檻 = max(競品) × 0.9；那個 max 被天花板壓住時**門檻是被低估的**。
  #     · PASS ⇒ 樞衡可能只是壓過了一個被低估的門檻 ⇒ 不可信，作廢。
  #     · FAIL ⇒ 真實門檻只會更高 ⇒ FAIL 更成立 ⇒ ⛔ 不該作廢。
  #   起因是 2026-09-06 第一趟真實資料：全場極差 0.8599（B 一聲沒吭），而最快兩家
  #   只差 0.0071 —— **一個慢的被測會掩蓋掉快的那幾家之間的收斂**（原 D34）。
  #
  # ⚠ ⚠ ⛔ **「排在後面」絕不等於「可以先把 PASS 打出來」。** 上面那次
  #   `bench_verdict_one` 只算不打；到這裡才決定 `verdict.txt` 與標準輸出上
  #   寫的是 PASS 還是 VOID。★ `G142` 當初把 B 排在前面，理由正是
  #   「先打出來的那個 PASS 已經會被人引用了」—— 那條理由在這裡一字不改地成立。
  if [ "$v_status" = "PASS" ]; then
    top_conv=$(bench_top_convergence "$readings")
    if [ -n "$top_conv" ]; then
      {
        echo "## $case_name"
        echo "VERDICT: VOID（门槛可能被低估 ⇒ 这个 PASS 不可信）"
        printf '%s\n' "$top_conv" | sed 's/^/  · /'
        echo "  ⛔ 被扣下的那个结论是：PASS（最强者 $v_best_name = $v_best_val，门槛 $v_floor，枢衡 $v_ours）"
        echo "  全部读数："
        printf '%s\n' "$readings" | sed 's/^/    /'
        echo
      } >> "$VERDICT_TXT"
      echo "[bench/verdict] $case_name：**VOID** —— 最强者附近收敛 ⇒ 那个 PASS 不可信"
      printf '%s\n' "$top_conv" | sed 's/^/               · /'
      RC=1
      continue
    fi
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
