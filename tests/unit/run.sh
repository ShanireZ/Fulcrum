#!/usr/bin/env bash
# 枢衡自己那几个 crate 的测试（单元 + 集成 + 文档测试）。
#
# ★ ★ **为什么要新加一个场景**：在它之前，`docker-run.sh` 的七个场景里
#   **没有任何一个会跑本仓库自己写的 Rust 测试**——
#     · 场景 1 跑的是 `vendor/pingora` 自带的测试（fork 回归网）；
#     · 场景 2–6 是 shell 写的端到端；
#     · 场景 0 是 lint，它只看得见「编不过 / 有 warning」。
#   也就是说，M1 产品代码里的每一条 `#[test]` 都会**一次都不跑**，
#   而整条链照样报绿。这与本仓库反复抓到的「判据覆盖面小于它自称回答的范围」同形。
#
# ★ 判据里最要紧的一条是**测试条数的下界**。理由是在 fork 回归网上
#   同族的一条：缺 `--no-fail-fast` 时 `cargo test` **每次都停在第一个二进制**，
#   六个 crate 的单测从没跑过，而门一直是绿的。它是被
#   「新加了 18 条测试，总数却没变」逼出来的——
#   **「新加的测试没让计数变化」是这类缺陷唯一会露头的地方。**
#   所以这里既带 `--no-fail-fast`，也把总数打出来并钉一个下界。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
OUTFILE=${UNIT_TEST_OUT:-/tmp/unit-test.out}

# 跑到的测试少于这个数，就认为「本次根本没测到该测的东西」，判红。
# ★ 它是**下界**不是**等于**：写新测试不该顺手改门。
#   只有当它明显落后于现状（比如现在有 200 条了）时才往上提。
MIN_TESTS=${MIN_TESTS:-60}

fail() { echo; echo "UNIT TESTS FAILED: $*"; exit 1; }

echo "=== [1/2] cargo test --workspace（枢衡自己的 crate）==="
echo "  ★ --locked：锁文件已入库，测试不许顺手改它（G29 第 2 条）"
echo "  ★ --no-fail-fast：否则一个二进制红了，后面的**一条都不会跑**，而总数看不出来"

set +e
cargo test --workspace --locked --no-fail-fast 2>&1 | tee "$OUTFILE"
CARGO_RC=${PIPESTATUS[0]}
set -e

# 「编译不过 / 跑不起来」与「跑完有失败」是两件事，不能混着判。
if ! grep -q "^test result:" "$OUTFILE"; then
  fail "cargo test 没能跑到出结果（退出码 $CARGO_RC）——多半是编译错误，看上面的输出"
fi

echo
echo "=== [2/2] 汇总 ==="

# `test result: ok. 38 passed; 0 failed; …` —— 用字段位置取，不用正则。
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
