#!/usr/bin/env bash
# 对拍那一格的**容器内**断言（M3 第一刀，G132）。由 `tests/bench/run.sh` 调起。
#
# ⛔ **这一格不判性能，它判「那条流水线跑不跑得通、判据判不判得动」。**
#   本轮（G132）不产出任何性能数字，而这一格恰恰是那句话的**判据**：
#   它要求真实那一趟的结论是 `UNQUALIFIED`。
#
# 三组断言：
#
#   A 正向 —— 四家真的起得来、真的回那个资源、原始数据真的落了盘
#   B 拒绝 —— 在**这台**（不合格的）宿主上，判定必须拒绝出结论
#   C 反证 —— 喂一份**合成的合格**快照，判定必须真的打出 PASS 与 FAIL
#
# ★ ★ ★ **C 是承重的那一组。** A 与 B 加起来只证明「它今天说了不」，
#   而一个永远说不的判定器与一个坏掉的判定器给出完全相同的输出。
#   ⇒ 没有 C，B 的判别力是零。

set -euo pipefail

REPO=${REPO:-/w}
OUT=/tmp/bench-gate-out
FIX=/tmp/bench-gate-fixture

FAILS=0
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

# ── A 正向：整条流水线真的跑一趟 ────────────────────────────────────────────
#
# ⚠ 时长压到几秒：这一格问的是「跑不跑得通」，⛔ 不是「跑多快」——
#   而**正因为它不问快慢，把时长调短不会让它变得不诚实**。
echo "── A 整条流水线 ──"
BENCH_DURATION=${BENCH_GATE_DURATION:-2s} \
  BENCH_CONNECTIONS=${BENCH_GATE_CONNECTIONS:-10} \
  bash "$REPO/bench/run.sh" "$OUT" > /tmp/bench-gate.log 2>&1 || {
  bad "bench/run.sh 整趟没跑完"
  sed 's/^/      /' /tmp/bench-gate.log >&2
  exit 1
}

# 四家逐个点名。★ **名单在这里是写死的，而这是有意的**：这一格问的正是
#   「G19 要的那四家一个都不少」，⇒ 它必须独立于 `bench/case/` 自己推导出来的集合。
#   ⚠ 从被测目录反推名单会让「少跑一家」变得看不见 —— 少的那一家两边一起消失。
for subject in fulcrum caddy haproxy nginx; do
  f="$OUT/raw/static-throughput/$subject.json"
  if [ -s "$f" ]; then
    ok "$subject 的原始数据落盘了"
  else
    bad "$subject 的原始数据没落盘（$f 不存在或为空）"
  fi
done

# 原始数据要真的能被判据读出来（⛔ 不是「文件在」就算）。
readings=$(python3 "$REPO/bench/read-raw.py" "$OUT/raw/static-throughput" || true)
if printf '%s\n' "$readings" | grep -q 'INVALID'; then
  bad "有被测的读数无效：$(printf '%s' "$readings" | tr '\n' ' ')"
else
  ok "四家的读数都通过了有效性校验（成功率 1.0、只有 200、无传输层错误）"
fi
# ⚠ 承重的一条：校验器必须真的读到了四行，⛔ 不是「没有 INVALID」——
#   一个读到 0 行的校验器同样打不出 INVALID。
n_read=$(printf '%s\n' "$readings" | grep -c . || true)
if [ "$n_read" = 4 ]; then
  ok "校验器读到了 4 行读数"
else
  bad "校验器读到 $n_read 行，该是 4 —— 「没有 INVALID」可能只是因为它一行都没读到"
fi

# ── B 拒绝：这台宿主不合格 ⇒ 结构性地不出结论 ───────────────────────────────
echo "── B 在这台宿主上必须拒绝出结论 ──"
if grep -q '"qualified": false' "$OUT/env.json"; then
  ok "环境快照把这台宿主判成了不合格"
else
  bad "环境快照没把这台宿主判成不合格 —— 这台开发机不该是合格宿主（G132）"
fi
# ⚠ 理由必须非空：`qualified: false` 配一张空理由清单是一份坏快照。
if python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if d.get("disqualifiers") else 1)
' "$OUT/env.json"; then
  ok "不合格的理由逐条写出来了"
else
  bad "判成不合格却一条理由都没有"
fi
if grep -q '^VERDICT: UNQUALIFIED' "$OUT/verdict.txt"; then
  ok "判定结论是 UNQUALIFIED"
else
  bad "判定没报 UNQUALIFIED —— 实际内容：$(head -5 "$OUT/verdict.txt" | tr '\n' ' ')"
fi
# ★ ★ 「不出数字」要判的是**没有出**，而不是「有一行写着不出」。
if grep -qE '^VERDICT: (PASS|FAIL)' "$OUT/verdict.txt"; then
  bad "不合格的宿主上竟然打出了 PASS / FAIL —— G132 说本轮不产出任何性能结论"
else
  ok "整份判定里一个 PASS / FAIL 都没有"
fi

# ── C 反证：合成的合格宿主上，判定必须真的出得来 ─────────────────────────────
#
# ★ ★ ★ 没有这一组，B 的判别力是零。
echo "── C 反证：合成合格快照 ──"

# C1：枢衡 280 对最强者 nginx 300（门槛 270）⇒ 该 PASS。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c1.log 2>&1; then
  if grep -q '^VERDICT: PASS' "$FIX/verdict.txt" &&
    grep -q 'nginx = 300' "$FIX/verdict.txt"; then
    ok "C1 合格快照 + 枢衡 280/最强者 300 ⇒ 真的打出了 PASS，且认出最强者是 nginx"
  else
    bad "C1 没打出预期的 PASS：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
else
  bad "C1 verdict.sh 退出码非 0（该是 0）：$(tail -3 /tmp/bench-c1.log | tr '\n' ' ')"
fi

# C2：枢衡 260 对最强者 300（门槛 270）⇒ 该 FAIL，**且退出码非 0**。
# ⚠ 两件事都要判：一个只打字不改退出码的判定器，在流水线里等于没判。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=260 nginx=300 caddy=100 haproxy=200 > /dev/null
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c2.log 2>&1; then
  bad "C2 枢衡低于门槛，verdict.sh 却退出 0"
else
  if grep -q '^VERDICT: FAIL' "$FIX/verdict.txt"; then
    ok "C2 枢衡 260/门槛 270 ⇒ 打出 FAIL 且退出码非 0"
  else
    bad "C2 退出码对了但没打出 FAIL：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C3：读数无效时**不许**变成一个数参与比较。
# ⚠ 这一条守的是 read-raw.py 那半边：把一份 successRate 不为 1 的数据喂进去，
#   判定必须报 NO-VERDICT，⛔ 不许它照常排名。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
python3 -c '
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
d = json.loads(p.read_text())
d["summary"]["successRate"] = 0.87
p.write_text(json.dumps(d))
' "$FIX/raw/synthetic/nginx.json"
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c3.log 2>&1; then
  bad "C3 有读数无效，verdict.sh 却退出 0"
else
  if grep -q '^VERDICT: NO-VERDICT' "$FIX/verdict.txt"; then
    ok "C3 一份读数 successRate=0.87 ⇒ NO-VERDICT，没有被当成一个数排进去"
  else
    bad "C3 没报 NO-VERDICT：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C4：合成的**不合格**快照必须也走拒绝那条路（⇒ 拒绝不是靠「跑在这台机器上」）。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" false \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c4.log 2>&1 || true
if grep -q '^VERDICT: UNQUALIFIED' "$FIX/verdict.txt" &&
  ! grep -qE '^VERDICT: (PASS|FAIL)' "$FIX/verdict.txt"; then
  ok "C4 合成的不合格快照同样拿不到结论 ⇒ 拒绝挂在快照上，不挂在「跑在哪台机器上」"
else
  bad "C4 不合格快照没走拒绝那条路：$(tr '\n' ' ' < "$FIX/verdict.txt")"
fi

# C5：**真**的传输层错误必须仍然判得动。
# ★ ★ ★ 这一条是 C3 的姊妹条，而它存在的理由要写在明处：`read-raw.py` 里那条
#   errorDistribution 判据在 2026-09-05 被**收窄**过（放行 `aborted due to deadline`，
#   那是 oha `-z` 到点砍在飞请求留下的，每条并发连接恰好一条）。
#   ⚠ ⚠ **一次收窄与一次删除，在「真实数据全绿」这件事上长得一模一样。**
#   ⇒ 必须有一条喂真故障的用例，证明收窄之后它没变成空操作。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
python3 -c '
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
d = json.loads(p.read_text())
d["errorDistribution"] = {"connection refused": 7}
p.write_text(json.dumps(d))
' "$FIX/raw/synthetic/nginx.json"
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c5.log 2>&1; then
  bad "C5 有真传输层错误，verdict.sh 却退出 0 —— 收窄那一步把判据变成了空操作"
else
  if grep -q '^VERDICT: NO-VERDICT' "$FIX/verdict.txt"; then
    ok "C5 connection refused ⇒ NO-VERDICT（收窄之后仍判得动真故障）"
  else
    bad "C5 没报 NO-VERDICT：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C6：良性那一条**量太大**时也要判红（它那时就不是「到点收尾」了）。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
# 夹具里 200 是 12345 条 ⇒ 1% 是 123.45；给 5000 条，远超那道界。
python3 -c '
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
d = json.loads(p.read_text())
d["errorDistribution"] = {"aborted due to deadline": 5000}
p.write_text(json.dumps(d))
' "$FIX/raw/synthetic/nginx.json"
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c6.log 2>&1; then
  bad "C6 deadline 砍掉 5000/12345 却照常出了结论 —— 那道 1% 的界没生效"
else
  if grep -q '^VERDICT: NO-VERDICT' "$FIX/verdict.txt"; then
    ok "C6 deadline 砍掉的量超过 1% ⇒ NO-VERDICT"
  else
    bad "C6 没报 NO-VERDICT：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C7：**正向** —— 良性且量小的那一条必须被放行。
# ⚠ 没有这一条，C5/C6 与「把整个字段判成永远有错」无法区分。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
python3 -c '
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
d = json.loads(p.read_text())
d["errorDistribution"] = {"aborted due to deadline": 50}
p.write_text(json.dumps(d))
' "$FIX/raw/synthetic/nginx.json"
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c7.log 2>&1 &&
  grep -q '^VERDICT: PASS' "$FIX/verdict.txt"; then
  ok "C7 deadline 砍掉 50/12345（0.4%）⇒ 照常出结论"
else
  bad "C7 良性且量小的 deadline 记录被判成了无效：$(tr '\n' ' ' < "$FIX/verdict.txt")"
fi

echo
if [ "$FAILS" = 0 ]; then
  echo "BENCH GATE PASSED"
else
  echo "BENCH GATE FAILED：$FAILS 处" >&2
  exit 1
fi
