#!/usr/bin/env bash
# 对拍那一格的**容器内**断言（M3 第一刀，G132）。由 `tests/bench/run.sh` 调起。
#
# ⛔ **这一格不判性能，它判「那条流水线跑不跑得通、判据判不判得动」。**
#   ⚠ ⚠ ★ 它要求**在这台开发机上**跑出来的那一趟结论是 `UNQUALIFIED`。
#   ⛔ 别把这条读成「对拍永远不出数字」—— 2026-09-06 合格宿主到位后已经出过一组
#   （`bench/results/`）。这一格判的是**拒绝那条路仍然管用**：只要宿主不合格，
#   判定就结构上算不出结论。★ 那正是 G132 要的东西，而不是那句话本身。
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

# ── C12 / C13：内核参数那道门（G145），两个方向都在这一趟里被走到 ───────────
#
# ★ ★ ★ 这两条是一对，**少任何一条另一条都说明不了问题**：
#   C13 只证「容器侧四个键真的被 `--sysctl` 设上了」；如果没有 C12，
#   一个把宿主侧检查整个删掉的改动会让 C13 照常绿。
#   C12 只证「宿主侧缺凭证会判红」；如果没有 C13，一个**根本没传旗标**的改动
#   会让容器侧四个键也一起判红，而 C12 仍然绿 —— 那时红的来源说不清。
echo "── C12/C13 内核参数（G145）──"

# C13 正向：容器侧那四个键，实测必须等于声明。
# ⚠ 判据取快照里那两份**独立记录**（声明 / 实测），⛔ 不重跑一次比较 ——
#   重跑等于用同一段代码给自己作证。
if python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
kp = d.get("kernel_params") or {}
declared = dict(x.split("=", 1) for x in kp.get("declared_in_container") or [])
if not declared:
    print("declared_in_container 是空的 —— 空声明会让整格判据恒绿"); sys.exit(1)
seen = d.get("sysctl_as_seen_in_container") or {}
bad = []
for k, want in declared.items():
    got = seen.get(k)
    if got is None:
        bad.append(k + " 没被记进快照"); continue
    if " ".join(got.split()) != " ".join(want.split()):
        bad.append("%s 实测 %r ≠ 声明 %r" % (k, got, want))
if bad:
    print("; ".join(bad)); sys.exit(1)
print(len(declared))
' "$OUT/env.json" > /tmp/bench-c13.txt 2>&1; then
  ok "C13 容器侧 $(cat /tmp/bench-c13.txt) 个键实测==声明 ⇒ --sysctl 旗标真的生效了"
else
  bad "C13 容器侧内核参数没按声明生效：$(cat /tmp/bench-c13.txt)"
fi

# C12 反向：本格**有意不给** `BENCH_HOST_SYSTLS` ⇒ 宿主侧那一条必须判红。
# ★ 它证明的是「那道门会咬」，⛔ 不是「这台机器不合格」——
#   后者本来就被 kernel 那一条判死了，拿它当证据等于什么都没证。
if python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
reasons = [r for r in d.get("disqualifiers") or [] if r.startswith("kernel-params:")]
host = [r for r in reasons if "netdev_max_backlog" in r]
if not host:
    print("一条 kernel-params/netdev_max_backlog 的理由都没有；实际理由=%r" % (d.get("disqualifiers"),))
    sys.exit(1)
print(host[0])
' "$OUT/env.json" > /tmp/bench-c12.txt 2>&1; then
  ok "C12 缺宿主侧凭证 ⇒ 判红并逐字点名（$(cut -c1-72 /tmp/bench-c12.txt)…）"
else
  bad "C12 没给宿主侧凭证却没判红 —— 那道门是空操作：$(cat /tmp/bench-c12.txt)"
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

# ── C8–C10：判据 ④（压测端饱和）的端到端接线 ──────────────────────────────
#
# ★ ★ ★ `bench/lib.sh --self-check` 已经把这条判据的**两个方向**用合成输入钉死了
#   （31 条）。⛔ **那些一条都证明不了 `verdict.sh` 真的去问了它** —— 纯函数判得动
#   与「判定器在流水线上真的调用它、并且真的因此不出结论」是两件事，
#   而后者只有端到端走一遍才看得见（同 G136 那三处落账点的教训）。

# C8：**B 承重** —— 四家挤在一起 ⇒ VOID，且退出码非 0。
# ⚠ 注意这组数在判据 ③ 眼里是**合格的 PASS**（枢衡 1000 对最强者 1020，门槛 918）
#   ⇒ 若 ④ 没有被接上，这一格会打出 PASS 而不是 VOID。**这正是它守的那个失效。**
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=1000 nginx=1010 caddy=1005 haproxy=1020 > /dev/null
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c8.log 2>&1; then
  bad "C8 四家挤在 2% 以内，verdict.sh 却退出 0 —— 判据 ④ 没有被接上"
else
  if grep -q '^VERDICT: VOID' "$FIX/verdict.txt" &&
    grep -q 'spread:' "$FIX/verdict.txt"; then
    ok "C8 四家挤在 2% 以内 ⇒ VOID（且理由是 spread），⛔ 没有变成一个 PASS"
  else
    bad "C8 没报 VOID：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C9：**A 那一半的接线** —— 同一组分得开的数，只多一个 `ceiling.txt` ⇒ VOID。
# ★ 一个变量的翻面：C1 用的就是这组数且打的是 PASS ⇒ 红的来源只可能是那个文件。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
echo 320 > "$FIX/raw/synthetic/ceiling.txt"
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c9.log 2>&1; then
  bad "C9 最强者 300 越过了上限 320×0.9=288，verdict.sh 却退出 0"
else
  if grep -q '^VERDICT: VOID' "$FIX/verdict.txt" &&
    grep -q 'ceiling:' "$FIX/verdict.txt"; then
    ok "C9 raw/<类>/ceiling.txt=320 ⇒ VOID（且理由是 ceiling）"
  else
    bad "C9 没报 VOID：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C10：**正向** —— 上限文件在、但最强者远低于它 ⇒ 照常出结论。
# ⚠ ⚠ 没有这一条，C9 与「只要存在 ceiling.txt 就一律作废」**无法区分** ——
#   而那种实现会让 A 这一半变成一个恒作废的空操作。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
echo 10000 > "$FIX/raw/synthetic/ceiling.txt"
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c10.log 2>&1 &&
  grep -q '^VERDICT: PASS' "$FIX/verdict.txt"; then
  ok "C10 上限 10000 远高于最强者 300 ⇒ 照常出结论（A 不是恒作废）"
else
  bad "C10 上限远高于最强者却没出结论：$(tr '\n' ' ' < "$FIX/verdict.txt")"
fi

# C11：⛔ `ceiling.txt` **不许**被 read-raw.py 当成第五家被测。
# ★ 判据取「最强者仍然是 nginx=300」：若那个文件被当成一份读数排进去，
#   最强者会变成它，而 C10 那一格照样是绿的 —— 两者分不开就等于没判。
if grep -q 'nginx = 300' "$FIX/verdict.txt"; then
  ok "C11 ceiling.txt 没有被当成第五家被测（最强者仍是 nginx=300）"
else
  bad "C11 最强者不是 nginx=300 —— ceiling.txt 可能被当成一份读数排进去了"
fi

# ── C14 / C15：判据 ④C（最强者附近的收敛，G146 结案 D34）────────────────────
#
# ★ ★ ★ **这两条是一对，而 C15 才是分水岭。** C14 只证「顶部收敛时那个 PASS 被扣下」；
#   ⚠ 没有 C15，一个把这条判据做成「对所有结论都作废」的实现会让 C14 照常绿 ——
#   而那正是 owner 否掉的那三条原候选，它们会把 2026-09-06 那个结实的 FAIL 扣掉。
echo "── C14/C15 最强者附近的收敛（G146）──"

# C14：枢衡 295 对最强者 300（门槛 270）⇒ 本该 PASS；
#      而 nginx=300 与 haproxy=299 只差 0.0033 < 0.05 ⇒ 门槛可能被低估 ⇒ 必须 VOID。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=295 nginx=300 haproxy=299 caddy=100 > /dev/null
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c14.log 2>&1; then
  bad "C14 顶部收敛时那个 PASS 没被扣下，verdict.sh 退出 0"
else
  # ⚠ ⚠ ★ 三个方向一起判，缺一不可：报了 VOID · 说得出理由 · **⛔ 整份判定里
  #   一个 `VERDICT: PASS` 都不许有** —— G142 那条「先打出来的 PASS 已经会被引用」
  #   在这里一字不改地成立。
  if grep -q '^VERDICT: VOID' "$FIX/verdict.txt" &&
    grep -q 'top-convergence' "$FIX/verdict.txt" &&
    ! grep -q '^VERDICT: PASS' "$FIX/verdict.txt"; then
    ok "C14 最强者 300 与第二强 299 只差 0.0033 ⇒ VOID，且整份判定里没有 PASS"
  else
    bad "C14 没走 VOID 那条路，或把 PASS 打了出来：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C15 ★★★ 分水岭：**同样顶部收敛，但结论是 FAIL ⇒ ⛔ 不许作废**。
#   枢衡 100 远低于门槛 270，而 nginx=300 / haproxy=299 仍然收敛。
#   ★ 理由：门槛被低估时真实门槛只会**更高** ⇒ FAIL 更成立，作废它等于扣掉一个
#   正确结论，而那并不比给出一个错结论便宜。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=100 nginx=300 haproxy=299 caddy=100 > /dev/null
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c15.log 2>&1; then
  bad "C15 枢衡低于门槛，verdict.sh 却退出 0"
else
  if grep -q '^VERDICT: FAIL' "$FIX/verdict.txt" &&
    ! grep -q '^VERDICT: VOID' "$FIX/verdict.txt"; then
    ok "C15 同样顶部收敛但结论是 FAIL ⇒ 照常打 FAIL，⛔ 没有被作废"
  else
    bad "C15 一个结实的 FAIL 被顶部收敛判据扣掉了：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# ── C16：**原始数据必须说得出自己是在什么口径下量的** ───────────────────────
#
# ⚠ ⚠ ★ 判的是 **A 那一趟真跑出来的** `env.json`（⛔ 不是合成夹具）——
#   这一条问的正是「那份快照诚不诚实」，拿合成的去问等于问了个假问题。
#
# **起因是一个真实缺陷**（2026-09-06 那组读数里就带着）：`env-snapshot.sh` 记的是
# **声明值** `${BENCH_PAYLOAD_BYTES:-}`，而用例跑的是 **声明值或缺省** `:-4096`
# ⇒ 没人显式设它时，快照写 `null`，而那一趟**真的**用的是 4096。
# ★ ★ ★ 那不是「少了一格元数据」：G19 要的是**原始数据可被第三方复现**，
# 而一份说不出自己用了多大 payload 的静态吞吐读数**复现不出来**。
# ⚠ ⚠ 它**不止 payload 一格** —— 四个参数用的是同一个写法，只是那一趟 owner
#   恰好显式设了另外三个 ⇒ 只有 payload 露了头。⛔ 别把它记成「payload 的毛病」。
#
# ★ 修法是**把缺省收进 `bench/lib.sh` 一处定义**，两边（快照与用例）都从那里取
#   ⇒ 「两处缺省飘掉」这类缺陷结构性地不存在，而不是被修好一次。
#
# ⛔ 本条**不判这四个值取得对不对** —— 那是口径本身，归 `bench/README.md`。
#   它只判「快照说不说得出话」。⚠ 失效方向是噪音（多一条红），不是沉默。
echo "── C16 口径必须落在原始数据里 ──"
if python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
lp = d.get("load_params")
if not lp:
    print("快照里根本没有 load_params —— 空的会让这一格恒绿"); sys.exit(1)
missing = sorted(k for k, v in lp.items() if v is None or v == "")
if missing:
    print("这几格没被记进去：%s（整份 load_params=%r）" % (", ".join(missing), lp)); sys.exit(1)
print("%d 格全部记到：%s" % (len(lp), lp))
' "$OUT/env.json" > /tmp/bench-c16.txt 2>&1; then
  ok "C16 $(cat /tmp/bench-c16.txt)"
else
  bad "C16 快照说不出自己是在什么口径下量的：$(cat /tmp/bench-c16.txt)"
fi

# ── C17：`respond` 上界那一步真的产出了，且**没有**混成 §8 的一类 ────────────
#
# `respond` 量的是「把响应推出去」的上界，与 `file_server` 背靠背比
# （handoff/design-static-file-cache.md §7：先量再决定方案 ③ 做不做）。
# ★ ★ 它**不是 §8 七类里的一类** —— §8 那七类是「与三家对拍」，而这一步只有枢衡。
#
# ⚠ ⚠ ⛔ **所以它的输出不许落 `raw/`**：`bench/verdict.sh` 是
#   `for raw_case in "$OUT_DIR"/raw/*/` —— **`raw/` 下每个子目录都会被当成一类去判**，
#   而 `bench/read-raw.py` 把该目录下每个 `*.json` 当成一家被测。
#   只有枢衡一家 ⇒ 判据 ④ 会按「有效读数不足两条」判 `VOID`：那不是错结论，
#   但它把一个**诊断上界**摆成了 §8 的一个类别，而七类里没有它。
#   ★ 与 G142 记的那条同族（`ceiling.txt` 不许叫 `.json`，否则被当成第五家被测）。
#
# 本条**两半都判**，缺一半都不够：
#   ① 两份读数真的产出了（少了它，diag 那一步整个不跑也没人会说）
#   ② `raw/` 下的类别数**没有因为它而变多**（少了它，输出挪回 `raw/` 也没人会说）
echo "── C17 respond 上界：产出了，且没混成 §8 的一类 ──"
# ⚠ ⚠ 计数必须**报得出来**，⛔ 不许把脚本掐掉：`find` 在不存在的目录上退出码非 0，
#   而本文件是 `set -euo pipefail` ⇒ 初稿写成 `$(find … | wc -l)` 时，
#   目录不存在那一趟**整个门在这里静默中止**（35 条 ✓、0 条 ✗、连收尾行都没打）。
#   ★ 那正是判据最该说话的那一刻，而它当时哑了。⇒ 先判目录在不在。
DIAG_DIR="$OUT/diag/respond-ceiling"
if [ -d "$DIAG_DIR" ]; then
  DIAG_N=$(find "$DIAG_DIR" -maxdepth 1 -name '*.json' | wc -l | tr -d ' ')
else
  DIAG_N=0
fi
if [ -d "$OUT/raw" ]; then
  RAW_N=$(find "$OUT/raw" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')
  RAW_NAMES=$(find "$OUT/raw" -mindepth 1 -maxdepth 1 -type d -printf '%f ' )
else
  RAW_N=0
  RAW_NAMES="(raw/ 不存在)"
fi
if [ "$DIAG_N" != 2 ]; then
  bad "C17 diag/respond-ceiling/ 下该有 2 份读数，实际 $DIAG_N（目录里有：$(find "$DIAG_DIR" -maxdepth 1 -mindepth 1 -printf '%f ' 2>/dev/null || echo '（目录不存在）')）"
elif [ "$RAW_N" != 1 ]; then
  bad "C17 raw/ 下该只有 static-throughput 一类，实际 $RAW_N 类：$RAW_NAMES ⇒ 诊断读数混进 §8 的类别里了"
else
  ok "C17 diag 两份读数在（file / respond），而 raw/ 仍只有 $RAW_N 类：$RAW_NAMES"
fi

echo
if [ "$FAILS" = 0 ]; then
  echo "BENCH GATE PASSED"
else
  echo "BENCH GATE FAILED：$FAILS 处" >&2
  exit 1
fi
