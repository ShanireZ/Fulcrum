#!/usr/bin/env bash
# 樞衡自己那幾個 crate 的測試（單元 + 整合 + 文件測試）。
#
# ★ ★ **為什麼要新加一個場景**：在它之前，`docker-run.sh` 的七個場景裡
#   **沒有任何一個會跑本儲存庫自己寫的 Rust 測試**——
#     · 場景 1 跑的是 `vendor/pingora` 自帶的測試（fork 迴歸網）；
#     · 場景 2–6 是 shell 寫的端到端；
#     · 場景 0 是 lint，它只看得見「編不過 / 有 warning」。
#   也就是說，M1 產品程式碼裡的每一條 `#[test]` 都會**一次都不跑**，
#   而整條鏈照樣報綠。這與本儲存庫反覆抓到的「判據覆蓋面小於它自稱回答的範圍」同形。
#
# ★ 判據裡最要緊的一條是**測試條數的下界**。理由是在 fork 迴歸網上
#   同族的一條：缺 `--no-fail-fast` 時 `cargo test` **每次都停在第一個二進位**，
#   六個 crate 的單測從沒跑過，而門一直是綠的。它是被
#   「新加了 18 條測試，總數卻沒變」逼出來的——
#   **「新加的測試沒讓計數變化」是這類缺陷唯一會露頭的地方。**
#   所以這裡既帶 `--no-fail-fast`，也把總數打出來並釘一個下界。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
OUTFILE=${UNIT_TEST_OUT:-/tmp/unit-test.out}

# 跑到的測試少於這個數，就認為「本次根本沒測到該測的東西」，判紅。
# ★ 它是**下界**不是**等於**：寫新測試不該順手改門。
#   只有當它明顯落後於現狀（比如現在有 200 條了）時才往上提。
MIN_TESTS=${MIN_TESTS:-60}

fail() { echo; echo "UNIT TESTS FAILED: $*"; exit 1; }

echo "=== [1/2] cargo test --workspace（枢衡自己的 crate）==="
echo "  ★ --locked：锁文件已入库，测试不许顺手改它（G29 第 2 条）"
echo "  ★ --no-fail-fast：否则一个二进制红了，后面的**一条都不会跑**，而总数看不出来"

set +e
cargo test --workspace --locked --no-fail-fast 2>&1 | tee "$OUTFILE"
CARGO_RC=${PIPESTATUS[0]}
set -e

# 「編譯不過 / 跑不起來」與「跑完有失敗」是兩件事，不能混著判。
if ! grep -q "^test result:" "$OUTFILE"; then
  fail "cargo test 没能跑到出结果（退出码 $CARGO_RC）——多半是编译错误，看上面的输出"
fi

echo
echo "=== [2/2] 汇总 ==="

# `test result: ok. 38 passed; 0 failed; …` —— 用欄位位置取，不用正則。
PASSED=$(awk '/^test result:/ { s += $4 } END { print s + 0 }' "$OUTFILE")
FAILED=$(awk '/^test result:/ { s += $6 } END { print s + 0 }' "$OUTFILE")
BINARIES=$(grep -c '^test result:' "$OUTFILE")

printf '  %s 个测试二进制 · %s 条通过 · %s 条失败\n' "$BINARIES" "$PASSED" "$FAILED"

[ "$FAILED" -eq 0 ] || fail "$FAILED 条测试失败（上面有逐条输出）"
[ "$CARGO_RC" -eq 0 ] || fail "cargo 退出码 $CARGO_RC，但没有一条测试报失败——去看编译或链接阶段"

if [ "$PASSED" -lt "$MIN_TESTS" ]; then
  fail "只跑到 $PASSED 条测试（下界是 $MIN_TESTS）。
       这不是「测试变少了」那么简单——更可能是**某个测试二进制根本没被跑**，
       而那种失效是安静的：剩下的照样全绿。
       先看上面 $BINARIES 个二进制是不是都在，再决定要不要动 MIN_TESTS。"
fi

echo "UNIT TESTS PASSED —— $BINARIES 个二进制、$PASSED 条测试全绿。"
