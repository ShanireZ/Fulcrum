#!/usr/bin/env bash
# 對拍那一格的**容器內**斷言（M3 第一刀，G132）。由 `tests/bench/run.sh` 調起。
#
# ⛔ **這一格不判效能，它判「那條流水線跑不跑得通、判據判不判得動」。**
#   ⚠ ⚠ ★ 它要求**在這臺開發機上**跑出來的那一趟結論是 `UNQUALIFIED`。
#   ⛔ 別把這條讀成「對拍永遠不出數字」—— 2026-09-06 合格宿主到位後已經出過一組
#   （`bench/results/`）。這一格判的是**拒絕那條路仍然管用**：只要宿主不合格，
#   判定就結構上算不出結論。★ 那正是 G132 要的東西，而不是那句話本身。
#
# 三組斷言：
#
#   A 正向 —— 四家真的起得來、真的回那個資源、原始資料真的落了盤
#   B 拒絕 —— 在**這臺**（不合格的）宿主上，判定必須拒絕出結論
#   C 反證 —— 喂一份**合成的合格**快照，判定必須真的打出 PASS 與 FAIL
#
# ★ ★ ★ **C 是承重的那一組。** A 與 B 加起來只證明「它今天說了不」，
#   而一個永遠說不的判定器與一個壞掉的判定器給出完全相同的輸出。
#   ⇒ 沒有 C，B 的判別力是零。

set -euo pipefail

REPO=${REPO:-/w}
OUT=/tmp/bench-gate-out
FIX=/tmp/bench-gate-fixture

# ★ 判據是**純函式**，本檔案與 `bench/verdict.sh` 調的是**同一個**，
#   ⛔ 不是兩份手寫的平行邏輯（與 `bench/docker-run.sh` 重複使用
#   `bench_kparam_mismatches` 同一條理由：兩份「差不多的」判據一定會分家，
#   而分家的現場是「設定照過、門沒生效」）。
# ★ `lib.sh` 被 `source` 時什麼都不做（它自己那道 `BASH_SOURCE` 判斷）⇒ 無副作用。
# shellcheck source=bench/lib.sh
. "$REPO/bench/lib.sh"

FAILS=0
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

# ── A 正向：整條流水線真的跑一趟 ────────────────────────────────────────────
#
# ⚠ 時長壓到幾秒：這一格問的是「跑不跑得通」，⛔ 不是「跑多快」——
#   而**正因為它不問快慢，把時長調短不會讓它變得不誠實**。
echo "── A 整条流水线 ──"
BENCH_DURATION=${BENCH_GATE_DURATION:-2s} \
  BENCH_CONNECTIONS=${BENCH_GATE_CONNECTIONS:-10} \
  bash "$REPO/bench/run.sh" "$OUT" > /tmp/bench-gate.log 2>&1 || {
  bad "bench/run.sh 整趟没跑完"
  sed 's/^/      /' /tmp/bench-gate.log >&2
  exit 1
}

# 四家逐個點名。★ **名單在這裡是寫死的，而這是有意的**：這一格問的正是
#   「G19 要的那四家一個都不少」，⇒ 它必須獨立於 `bench/case/` 自己推匯出來的集合。
#   ⚠ 從被測目錄反推名單會讓「少跑一家」變得看不見 —— 少的那一家兩邊一起消失。
for subject in fulcrum caddy haproxy nginx; do
  f="$OUT/raw/static-throughput/$subject.json"
  if [ -s "$f" ]; then
    ok "$subject 的原始数据落盘了"
  else
    bad "$subject 的原始数据没落盘（$f 不存在或为空）"
  fi
done

# 原始資料要真的能被判據讀出來（⛔ 不是「檔案在」就算）。
readings=$(python3 "$REPO/bench/read-raw.py" "$OUT/raw/static-throughput" || true)
if printf '%s\n' "$readings" | grep -q 'INVALID'; then
  bad "有被测的读数无效：$(printf '%s' "$readings" | tr '\n' ' ')"
else
  ok "四家的读数都通过了有效性校验（成功率 1.0、只有 200、无传输层错误）"
fi
# ⚠ 承重的一條：檢核器必須真的讀到了四行，⛔ 不是「沒有 INVALID」——
#   一個讀到 0 行的檢核器同樣打不出 INVALID。
n_read=$(printf '%s\n' "$readings" | grep -c . || true)
if [ "$n_read" = 4 ]; then
  ok "校验器读到了 4 行读数"
else
  bad "校验器读到 $n_read 行，该是 4 —— 「没有 INVALID」可能只是因为它一行都没读到"
fi

# ── B 拒絕：這臺宿主不合格 ⇒ 結構性地不出結論 ───────────────────────────────
echo "── B 在这台宿主上必须拒绝出结论 ──"
if grep -q '"qualified": false' "$OUT/env.json"; then
  ok "环境快照把这台宿主判成了不合格"
else
  bad "环境快照没把这台宿主判成不合格 —— 这台开发机不该是合格宿主（G132）"
fi
# ⚠ 理由必須非空：`qualified: false` 配一張空理由清單是一份壞快照。
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
# ★ ★ 「不出數字」要判的是**沒有出**，而不是「有一行寫著不出」。
if grep -qE '^VERDICT: (PASS|FAIL)' "$OUT/verdict.txt"; then
  bad "不合格的宿主上竟然打出了 PASS / FAIL —— G132 说本轮不产出任何性能结论"
else
  ok "整份判定里一个 PASS / FAIL 都没有"
fi

# ── C12 / C13：核心參數那道門（G145），兩個方向都在這一趟裡被走到 ───────────
#
# ★ ★ ★ 這兩條是一對，**少任何一條另一條都說明不了問題**：
#   C13 只證「容器側四個鍵真的被 `--sysctl` 設上了」；如果沒有 C12，
#   一個把宿主側檢查整個刪掉的改動會讓 C13 照常綠。
#   C12 只證「宿主側缺憑證會判紅」；如果沒有 C13，一個**根本沒傳旗標**的改動
#   會讓容器側四個鍵也一起判紅，而 C12 仍然綠 —— 那時紅的來源說不清。
echo "── C12/C13 内核参数（G145）──"

# C13 正向：容器側那四個鍵，實測必須等於宣告。
# ⚠ 判據取快照裡那兩份**獨立記錄**（宣告 / 實測），⛔ 不重跑一次比較 ——
#   重跑等於用同一段程式碼給自己作證。
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

# C12 反向：本格**有意不給** `BENCH_HOST_SYSTLS` ⇒ 宿主側那一條必須判紅。
# ★ 它證明的是「那道門會咬」，⛔ 不是「這臺機器不合格」——
#   後者本來就被 kernel 那一條判死了，拿它當證據等於什麼都沒證。
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

# ── C 反證：合成的合格宿主上，判定必須真的出得來 ─────────────────────────────
#
# ★ ★ ★ 沒有這一組，B 的判別力是零。
echo "── C 反证：合成合格快照 ──"

# C1：樞衡 280 對最強者 nginx 300（門檻 270）⇒ 該 PASS。
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

# C2：樞衡 260 對最強者 300（門檻 270）⇒ 該 FAIL，**且結束代碼非 0**。
# ⚠ 兩件事都要判：一個只打字不改結束代碼的判定器，在流水線裡等於沒判。
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

# C3：讀數無效時**不許**變成一個數參與比較。
# ⚠ 這一條守的是 read-raw.py 那半邊：把一份 successRate 不為 1 的資料喂進去，
#   判定必須報 NO-VERDICT，⛔ 不許它照常排名。
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

# C4：合成的**不合格**快照必須也走拒絕那條路（⇒ 拒絕不是靠「跑在這臺機器上」）。
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

# C5：**真**的傳輸層錯誤必須仍然判得動。
# ★ ★ ★ 這一條是 C3 的姊妹條，而它存在的理由要寫在明處：`read-raw.py` 裡那條
#   errorDistribution 判據在 2026-09-05 被**收窄**過（放行 `aborted due to deadline`，
#   那是 oha `-z` 到點砍在飛請求留下的，每條並行連接恰好一條）。
#   ⚠ ⚠ **一次收窄與一次刪除，在「真實資料全綠」這件事上長得一模一樣。**
#   ⇒ 必須有一條喂真故障的用例，證明收窄之後它沒變成空操作。
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

# C6：良性那一條**量太大**時也要判紅（它那時就不是「到點收尾」了）。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=280 nginx=300 caddy=100 haproxy=200 > /dev/null
# 夾具裡 200 是 12345 條 ⇒ 1% 是 123.45；給 5000 條，遠超那道界。
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

# C7：**正向** —— 良性且量小的那一條必須被放行。
# ⚠ 沒有這一條，C5/C6 與「把整個欄位判成永遠有錯」無法區分。
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

# ── C8–C10：判據 ④（壓測端飽和）的端到端串接 ──────────────────────────────
#
# ★ ★ ★ `bench/lib.sh --self-check` 已經把這條判據的**兩個方向**用合成輸入釘死了
#   （31 條）。⛔ **那些一條都證明不了 `verdict.sh` 真的去問了它** —— 純函式判得動
#   與「判定器在流水線上真的呼叫它、並且真的因此不出結論」是兩件事，
#   而後者只有端到端走一遍才看得見（同 G136 那三處落賬點的教訓）。

# C8：**B 承重** —— 四家擠在一起 ⇒ VOID，且結束代碼非 0。
# ⚠ 注意這組數在判據 ③ 眼裡是**合格的 PASS**（樞衡 1000 對最強者 1020，門檻 918）
#   ⇒ 若 ④ 沒有被接上，這一格會打出 PASS 而不是 VOID。**這正是它守的那個失效。**
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

# C9：**A 那一半的串接** —— 同一組分得開的數，只多一個 `ceiling.txt` ⇒ VOID。
# ★ 一個變數的翻面：C1 用的就是這組數且打的是 PASS ⇒ 紅的來源只可能是那個檔案。
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

# C10：**正向** —— 上限檔案在、但最強者遠低於它 ⇒ 照常出結論。
# ⚠ ⚠ 沒有這一條，C9 與「只要存在 ceiling.txt 就一律作廢」**無法區分** ——
#   而那種實作會讓 A 這一半變成一個恆作廢的空操作。
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

# C11：⛔ `ceiling.txt` **不許**被 read-raw.py 當成第五家被測。
# ★ 判據取「最強者仍然是 nginx=300」：若那個檔案被當成一份讀數排進去，
#   最強者會變成它，而 C10 那一格照樣是綠的 —— 兩者分不開就等於沒判。
if grep -q 'nginx = 300' "$FIX/verdict.txt"; then
  ok "C11 ceiling.txt 没有被当成第五家被测（最强者仍是 nginx=300）"
else
  bad "C11 最强者不是 nginx=300 —— ceiling.txt 可能被当成一份读数排进去了"
fi

# ── C14 / C15：判據 ④C（最強者附近的收斂，G146 結案 D34）────────────────────
#
# ★ ★ ★ **這兩條是一對，而 C15 才是分水嶺。** C14 只證「頂部收斂時那個 PASS 被扣下」；
#   ⚠ 沒有 C15，一個把這條判據做成「對所有結論都作廢」的實作會讓 C14 照常綠 ——
#   而那正是 owner 否掉的那三條原候選，它們會把 2026-09-06 那個結實的 FAIL 扣掉。
echo "── C14/C15 最强者附近的收敛（G146）──"

# C14：樞衡 295 對最強者 300（門檻 270）⇒ 本該 PASS；
#      而 nginx=300 與 haproxy=299 只差 0.0033 < 0.05 ⇒ 門檻可能被低估 ⇒ 必須 VOID。
rm -rf "$FIX"
python3 "$REPO/tests/bench/mkfixture.py" "$FIX" true \
  fulcrum=295 nginx=300 haproxy=299 caddy=100 > /dev/null
if bash "$REPO/bench/verdict.sh" "$FIX" > /tmp/bench-c14.log 2>&1; then
  bad "C14 顶部收敛时那个 PASS 没被扣下，verdict.sh 退出 0"
else
  # ⚠ ⚠ ★ 三個方向一起判，缺一不可：報了 VOID · 說得出理由 · **⛔ 整份判定裡
  #   一個 `VERDICT: PASS` 都不許有** —— G142 那條「先打出來的 PASS 已經會被引用」
  #   在這裡一字不改地成立。
  if grep -q '^VERDICT: VOID' "$FIX/verdict.txt" &&
    grep -q 'top-convergence' "$FIX/verdict.txt" &&
    ! grep -q '^VERDICT: PASS' "$FIX/verdict.txt"; then
    ok "C14 最强者 300 与第二强 299 只差 0.0033 ⇒ VOID，且整份判定里没有 PASS"
  else
    bad "C14 没走 VOID 那条路，或把 PASS 打了出来：$(tr '\n' ' ' < "$FIX/verdict.txt")"
  fi
fi

# C15 ★★★ 分水嶺：**同樣頂部收斂，但結論是 FAIL ⇒ ⛔ 不許作廢**。
#   樞衡 100 遠低於門檻 270，而 nginx=300 / haproxy=299 仍然收斂。
#   ★ 理由：門檻被低估時真實門檻只會**更高** ⇒ FAIL 更成立，作廢它等於扣掉一個
#   正確結論，而那並不比給出一個錯結論便宜。
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

# ── C16：**原始資料必須說得出自己是在什麼口徑下量的** ───────────────────────
#
# ⚠ ⚠ ★ 判的是 **A 那一趟真跑出來的** `env.json`（⛔ 不是合成夾具）——
#   這一條問的正是「那份快照誠不誠實」，拿合成的去問等於問了個假問題。
#
# **起因是一個真實缺陷**（2026-09-06 那組讀數里就帶著）：`env-snapshot.sh` 記的是
# **宣告值** `${BENCH_PAYLOAD_BYTES:-}`，而用例跑的是 **宣告值或預設** `:-4096`
# ⇒ 沒人顯式設它時，快照寫 `null`，而那一趟**真的**用的是 4096。
# ★ ★ ★ 那不是「少了一格中繼資料」：G19 要的是**原始資料可被第三方重現**，
# 而一份說不出自己用了多大 payload 的靜態吞吐讀數**重現不出來**。
# ⚠ ⚠ 它**不止 payload 一格** —— 四個參數用的是同一個寫法，只是那一趟 owner
#   恰好顯式設了另外三個 ⇒ 只有 payload 露了頭。⛔ 別把它記成「payload 的毛病」。
#
# ★ 修法是**把預設收進 `bench/lib.sh` 一處定義**，兩邊（快照與用例）都從那裡取
#   ⇒ 「兩處預設飄掉」這類缺陷結構性地不存在，而不是被修好一次。
#
# ⛔ 本條**不判這四個值取得對不對** —— 那是口徑本身，歸 `bench/README.md`。
#   它只判「快照說不說得出話」。⚠ 失效方向是噪音（多一條紅），不是沉默。
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

# ── C17：`respond` 上界那一步真的產出了，且**沒有**混成 §8 的一類 ────────────
#
# `respond` 量的是「把回應推出去」的上界，與 `file_server` 背靠背比
# （handoff/design-static-file-cache.md §7：先量再決定方案 ③ 做不做）。
# ★ ★ 它**不是 §8 七類裡的一類** —— §8 那七類是「與三家對拍」，而這一步只有樞衡。
#
# ⚠ ⚠ ⛔ **所以它的輸出不許落 `raw/`**：`bench/verdict.sh` 是
#   `for raw_case in "$OUT_DIR"/raw/*/` —— **`raw/` 下每個子目錄都會被當成一類去判**，
#   而 `bench/read-raw.py` 把該目錄下每個 `*.json` 當成一家被測。
#   只有樞衡一家 ⇒ 判據 ④ 會按「有效讀數不足兩條」判 `VOID`：那不是錯結論，
#   但它把一個**診斷上界**擺成了 §8 的一個類別，而七類裡沒有它。
#   ★ 與 G142 記的那條同族（`ceiling.txt` 不許叫 `.json`，否則被當成第五家被測）。
#
# 本條**兩半都判**，缺一半都不夠：
#   ① 兩份讀數真的產出了（少了它，diag 那一步整個不跑也沒人會說）
#   ② `raw/` 下的類別數**沒有因為它而變多**（少了它，輸出挪回 `raw/` 也沒人會說）
echo "── C17 respond 上界：产出了，且没混成 §8 的一类 ──"
# ⚠ ⚠ 計數必須**報得出來**，⛔ 不許把指令碼掐掉：`find` 在不存在的目錄上結束代碼非 0，
#   而本檔案是 `set -euo pipefail` ⇒ 初稿寫成 `$(find … | wc -l)` 時，
#   目錄不存在那一趟**整個門在這裡靜默中止**（35 條 ✓、0 條 ✗、連收尾行都沒打）。
#   ★ 那正是判據最該說話的那一刻，而它當時啞了。⇒ 先判目錄在不在。
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
# ★ ★ **診斷步驟的名字一個都不許在 `raw/` 下出現** —— 名單**推匯出來**，
#   ⛔ 不數類別總數：§8 有七類，補齊路上那個數要改 5 次，而漏改一次要麼誤紅、
#   要麼被人順手放寬成不判。★ 與 `bench/run.sh` 裡「用例集推匯出來，
#   ⛔ 不寫死清單」同一條紀律。
# ⚠ `diag/<名>.sh` 與它的產出目錄 `diag/<名>/` 同名，這一條由本判據自己依賴著 ——
#   哪天有指令碼不這麼命名，它會**漏判**而不是誤報 ⇒ 新增 diag 指令碼時照此命名。
DIAG_IN_RAW=
for diag_sh in "$REPO/bench/diag"/*.sh; do
  [ -f "$diag_sh" ] || continue
  dname=$(basename "$diag_sh" .sh)
  if [ -d "$OUT/raw/$dname" ]; then
    DIAG_IN_RAW="$DIAG_IN_RAW $dname"
  fi
done
if [ "$DIAG_N" != 2 ]; then
  bad "C17 diag/respond-ceiling/ 下该有 2 份读数，实际 $DIAG_N（目录里有：$(find "$DIAG_DIR" -maxdepth 1 -mindepth 1 -printf '%f ' 2>/dev/null || echo '（目录不存在）')）"
elif [ -n "$DIAG_IN_RAW" ]; then
  bad "C17 诊断读数混进 §8 的类别里了：raw/ 下出现了$DIAG_IN_RAW（raw/ 现有 $RAW_N 类：$RAW_NAMES）"
else
  ok "C17 diag 两份读数在（file / respond），而 raw/ 的 $RAW_N 类里没有任何诊断步骤：$RAW_NAMES"
fi

# ── C18：快取命中 p99 那一步真的產出了，且那個差值不是編的 ────────────────────
#
# `G150` ② 要的是「磁碟命中的 p99 − 記憶體命中的 p99」這一格落進原始資料。
# ★ ★ 它**不是 §8 七類裡的一類** —— 那七類是「與三家對拍」，而這一步只有樞衡自己
#   （磁碟命中對記憶體命中是**樞衡內部**對比，根本沒有競品）。
#   ⇒ 輸出落 `diag/`，理由與 C17 逐字相同，⛔ 不在這裡重述。
#   ⚠ 「沒混進 §8」那一半**已經由 C17 那條 `RAW_N != 1` 判著了**（它判的是 raw/ 的
#     類別總數，任何一個診斷步驟挪進去都會讓它紅）⇒ 本條不再重複判一遍，
#     只判本步自己的三份產物。
#
# ⚠ ⚠ **計數前先判目錄在不在**：`find` 在不存在的目錄上結束代碼非 0，而本檔案是
#   `set -euo pipefail` ⇒ 寫成 `$(find … | wc -l)` 會讓整個門在這裡**靜默中止**
#   （C17 那段的註解記著這個坑，它當初真的這麼壞過一次）。
# ── C19：快取根的檔案系統必須落在原始資料裡 ──────────────────────────────────
#
# ★ ★ 與 C16 同一條口徑，判的同樣是 **A 那一趟真跑出來的** `env.json`。
#   `diag/cache-hit-p99` 量的是「磁碟命中 p99 − 記憶體命中 p99」，而磁碟那一半
#   **直接由檔案系統決定** —— 2026-09-06 實測過：站台根換一種檔案系統，
#   靜態吞吐差 **45%**。⇒ 一份說不出自己快取落在哪種檔案系統上的差值
#   **重現不出來**（G19 / G147）。
# ⚠ ⚠ **兩個欄位要一起讀**：一個恆回傳 `unknown` 的探測與「真的探不了」輸出完全相同
#   ⇒ 本條要求 `fs` 與 `rwf_nowait` **都不是空**，⛔ 不只判 `path`（那一格恆有值）。
echo "── C19 缓存根的文件系统必须落在原始数据里 ──"
if python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
cr = d.get("cache_root")
if not cr:
    print("快照里根本没有 cache_root —— 空的会让这一格恒绿"); sys.exit(1)
missing = sorted(k for k, v in cr.items() if v is None or v == "")
if missing:
    print("这几格没被记进去：%s（整份 cache_root=%r）" % (", ".join(missing), cr)); sys.exit(1)
print("三格全部记到：%s" % (cr,))
' "$OUT/env.json" > /tmp/bench-c19.txt 2>&1; then
  ok "C19 $(cat /tmp/bench-c19.txt)"
else
  bad "C19 $(cat /tmp/bench-c19.txt)"
fi

echo "── C18 缓存命中 p99：三份产物在，且 delta 与两份读数对得上 ──"
C18_DIR="$OUT/diag/cache-hit-p99"
if [ -d "$C18_DIR" ]; then
  C18_N=$(find "$C18_DIR" -maxdepth 1 -name '*.json' | wc -l | tr -d ' ')
else
  C18_N=0
fi
if [ "$C18_N" != 3 ]; then
  bad "C18 diag/cache-hit-p99/ 下该有 3 份产物（cache-mem / cache-disk / delta），实际 $C18_N（目录里有：$(find "$C18_DIR" -maxdepth 1 -mindepth 1 -printf '%f ' 2>/dev/null || echo '（目录不存在）')）"
elif [ ! -f "$C18_DIR/delta.json" ]; then
  bad "C18 delta.json 不在 —— 差值没被记下来，而那正是 G150 ② 要的那一格"
else
  # ★ ★ ★ **判 delta 真的等於那兩份讀數之差，⛔ 不只判檔案在**：
  #   一個恆寫 0 的 delta.json 與一個算對了的 delta.json，在「檔案在不在」
  #   這條判據下**完全相同** —— 而本條要守的正是那個數本身。
  # ⚠ p99 的鍵路徑是 `latencyPercentiles.p99`（2026-09-09 對著
  #   `bench/results/2026-09-06-static-throughput/` 那組真實讀數實測）。
  #   ⛔ **不是** `summary.p99`（summary 里根本沒有 p99），
  #   ⛔ 也不是 `metrics.latency_ms.p99`（那份單位是**毫秒**，取錯差 1000 倍
  #      而看著像個正常的數）。
  if C18_SUMMARY=$(python3 - "$C18_DIR" <<'PY'
import json, pathlib, sys
d = pathlib.Path(sys.argv[1])
delta = json.loads((d / "delta.json").read_text(encoding="utf-8"))
src = delta["source"]
p = {}
for side in ("mem", "disk"):
    raw = json.loads((d / src[side]).read_text(encoding="utf-8"))
    p[side] = raw["latencyPercentiles"]["p99"]
ok = (abs(delta["p99_mem"] - p["mem"]) < 1e-12
      and abs(delta["p99_disk"] - p["disk"]) < 1e-12
      and abs(delta["delta"] - (p["disk"] - p["mem"])) < 1e-12)

# ★ ★ ★ **独立复核那四格后端状态**，⛔ 不靠信任 diag 脚本自己的控制流。
#   `X-Fulcrum-Cache` 的后缀说的是「这个进程**真的**挑中了哪个后端」
#   （`dsl-reference.md` 原话）⇒ 这是分得开「真磁盘命中」与
#   「缓存压根没开、照常转发」的**唯一**判据 —— 后者下 oha 照样量得出一组
#   看着完全正常的数。⚠ `metrics` 那层答不了（`cache/mod.rs:280`：
#   它「折得比响应头粗」，`HIT` 与 `HIT-DISK` 在指标上是同一格）。
# ⛔ ⛔ **「after 与 before 取值相同」证不了「整趟没重验证」** —— 2026-09-09
#   用反证实测证伪过（注入 `ttl=1s` < `duration=2s`，整个门禁照样全绿）：
#   重验证之后条目重新变新鲜，之后的请求又是 `Hit`，与从未过期完全相同。
# ★ 分得开的是 `after.age`：重验证把 `stored_at` 置为当前时刻（`store.rs:208`）
#   ⇒ `Age` 归零。⇒ `after.age >= duration_secs` 才代表从预热起没被刷新过。
want = {"cache-mem": "HIT", "cache-disk": "HIT-DISK"}
st = delta.get("cache_state", {})
dur = delta.get("duration_secs")
if not isinstance(dur, int):
    ok = False
for variant, expect in want.items():
    got = st.get(variant, {})
    before, after = got.get("before", {}), got.get("after", {})
    if before.get("state") != expect or after.get("state") != expect:
        ok = False
    age = after.get("age", "")
    if not str(age).isdigit() or not isinstance(dur, int) or int(age) < dur:
        ok = False

# ★ 把观测到的 Age 打出来：否则这条证据又变成「只有脚本自己知道」的东西，
#   而本条判据存在的全部理由就是让它看得见（见上面那段注释）。
print("mem Age={} / disk Age={} / 负载 {}s".format(
    st.get("cache-mem", {}).get("after", {}).get("age", "?"),
    st.get("cache-disk", {}).get("after", {}).get("age", "?"),
    dur))
sys.exit(0 if ok else 1)
PY
  ); then
    ok "C18 三份产物在；delta 与两份读数对得上；四格后端真命中（HIT / HIT-DISK），且跑完后 Age ≥ 负载时长 ⇒ 整趟没重验证 —— $C18_SUMMARY"
  else
    bad "C18 delta.json 通不过复核 —— 三种可能：差值与它指名的那两份读数对不上、四格后端状态不是真命中、或跑完后 Age < 负载时长（中途重验证过）。读数：${C18_SUMMARY:-（取不到）}"
  fi
fi

# ── C20：§8 第二類「反代吞吐」真的產出了 ────────────────────────────────────
#
# ★ ★ **四家逐個點名，名單寫死，而這是有意的** —— 與 A 組那句逐字同源：
#   這一格問的正是「G19 要的那四家一個都不少」，⇒ 它必須獨立於
#   `bench/case/` 自己推匯出來的集合。從產出目錄反推名單會讓「少跑一家」
#   變得看不見：少的那一家兩邊一起消失。
# ⚠ ⚠ 與 C17 / C18 方向**相反**：本類**就是** §8 的一類 ⇒ 它必須落 `raw/`，
#   ⛔ 不是 `diag/`。
echo "── C20 反代吞吐：四家的原始数据都落盘了 ──"
C20_DIR="$OUT/raw/reverse-proxy-throughput"
C20_MISSING=
for subject in fulcrum caddy haproxy nginx; do
  if [ ! -s "$C20_DIR/$subject.json" ]; then
    C20_MISSING="$C20_MISSING $subject"
  fi
done
if [ -n "$C20_MISSING" ]; then
  bad "C20 这几家的原始数据没落盘：$C20_MISSING（看的是 $C20_DIR/<家>.json）"
else
  # ⚠ ⚠ ★ **把上游重複使用那一格的讀數打進訊息裡** —— 本檔案第 40 行把
  #   `bench/run.sh` 的輸出重定向進**容器裡**的 /tmp，而容器是 `--rm` 的
  #   ⇒ 用例指令碼自己列印的那幾行**沒有任何人看得見**，包括寫它的人。
  #   ⇒ 定那道重複使用判據的閾值時，要讀的正是這一行。
  ok "C20 四家的原始数据都落盘了；上游连接读数：$(tr '\n' ' ' < "$C20_DIR/upstream-conns.txt" 2>/dev/null || echo '(upstream-conns.txt 还不在)')"
fi

# ── C20 第 ② 半：上游連接重複使用那一格真的被記下來並判過了 ──────────────────────
#
# ★ ★ 判據本體是 `bench/lib.sh` 的純函式，它自己的兩個方向由
#   `bench_self_check` 用合成輸入釘著（`bench/run.sh` 第 ① 步就跑）。
#   ⇒ **本條判的是「串接通不通」**：用例真的產出了那個檔案、gate 真的讀到了、
#   判據真的被餵了進去。⚠ 兩者缺一不可 —— 一個把路徑寫錯的 C20 會讓那幾條
#   合成反證照常全綠。
#
# ⚠ ⚠ ★ **措辭有意不寫「四家都在重複使用」。** 門禁跑的是 10 條連接，而
#   「部分不復用」那一族的表現依賴並行數（實測：同一處缺陷在 10 連接下是 4.12，
#   在 50 連接下是 100.30）⇒ 這一格在門禁參數下**只抓得住「每請求新建」那一族**。
#   ⛔ 一句「四家都驗過了」會讓人以為 caddy 那一格也被守著了 —— 它沒有。
echo "── C20 上游连接复用（口径对齐）──"
C20_CONNS="$C20_DIR/upstream-conns.txt"
if [ ! -s "$C20_CONNS" ]; then
  bad "C20 $C20_CONNS 不在或为空 —— 上游复用那一格没被记下来，而它缺席不会让任何东西变红"
else
  C20_VIOL=$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(cat "$C20_CONNS")")
  if [ -n "$C20_VIOL" ]; then
    bad "C20 有被测没在复用上游连接（上限 $BENCH_UPSTREAM_REUSE_MAX）："
    printf '%s\n' "$C20_VIOL" | sed 's/^/        · /' >&2
  else
    ok "C20 没有一家落进「每请求新建」那一族（上限 $BENCH_UPSTREAM_REUSE_MAX，实得 $(tr '\n' ' ' < "$C20_CONNS")）"
  fi
fi

# ── C21：§8 第三類「高並行短連接」真的產出了，而且原始資料真的算數 ─────────────
#
# ★ ★ 四家逐個點名，名單寫死 —— 理由與 A 組、C20 逐字同源。
# ⚠ 與 C20 不同，這裡**還要**把原始資料喂一遍 `read-raw.py`：短連接比另外兩類更容易
#   在負載中途出傳輸層錯誤（重置、臨時連接埠），而開發機上 `verdict.sh` 因為宿主不合格
#   **根本不讀** raw/ ⇒ 一份 INVALID 的讀數在門禁裡會一直沒人看見。
# ⛔ 訊息裡不打任何讀數（G132）：只說有沒有、算不算數。
echo "── C21 高并发短连接：四家的原始数据都落盘了，而且都算数 ──"
C21_DIR="$OUT/raw/short-connection-throughput"
C21_MISSING=
for subject in fulcrum caddy haproxy nginx; do
  if [ ! -s "$C21_DIR/$subject.json" ]; then
    C21_MISSING="$C21_MISSING $subject"
  fi
done
if [ -n "$C21_MISSING" ]; then
  bad "C21 这几家的原始数据没落盘：$C21_MISSING（看的是 $C21_DIR/<家>.json）"
else
  C21_READ=$(python3 "$REPO/bench/read-raw.py" "$C21_DIR" || true)
  C21_N=$(printf '%s\n' "$C21_READ" | grep -c . || true)
  if printf '%s\n' "$C21_READ" | grep -q 'INVALID'; then
    bad "C21 有被测的读数无效：$(printf '%s\n' "$C21_READ" | grep 'INVALID' | tr '\n' ' ')"
  elif [ "$C21_N" != 4 ]; then
    bad "C21 校验器读到 $C21_N 行，该是 4 —— 「没有 INVALID」可能只是因为它一行都没读到"
  else
    ok "C21 四家的原始数据都落盘了，且都通过了有效性校验（成功率 1.0、只有 200、无传输层错误）"
  fi
fi

# ── C21 第 ② 半：判據 ⑥ 兩半真的被餵過，而且都過了 ────────────────────────────
#
# ★ 判據本體是 `bench/lib.sh` 的純函式（`bench_short_conn_violations` /
#   `bench_request_close_violations`），兩個方向由 `bench_self_check` 用合成輸入釘著
#   ⇒ **本條判的是串接**：用例真的產出了那兩個檔案、這裡真的讀到了、判據真的被餵了進去。
# ⚠ ★ 把實得讀數打進訊息：容器是 `--rm` 的，不打出來就沒人看得見（與 C20 同一條）。
#   那幾列是連接計數與 TIME_WAIT 計數（後兩列是診斷、不判），⛔ 不是效能數字。
echo "── C21 短连接口径（判据 ⑥：每请求一条新连接 · oha 真带着 Connection: close）──"
C21_CONNS="$C21_DIR/new-conns.txt"
C21_REQ="$C21_DIR/oha-request.txt"
if [ ! -s "$C21_CONNS" ]; then
  bad "C21 $C21_CONNS 不在或为空 —— 判据 ⑥ ① 没被喂过，而它缺席不会让任何东西变红"
else
  C21_LINES=$(grep -c . "$C21_CONNS" || true)
  C21_VIOL=$(bench_short_conn_violations "$BENCH_SHORT_NEW_CONNS_MIN" "$(cat "$C21_CONNS")")
  if [ "$C21_LINES" != 4 ]; then
    bad "C21 new-conns.txt 有 $C21_LINES 行，该是 4 —— 少判一家与判过在输出上分不开（实得 $(tr '\n' ' ' < "$C21_CONNS")）"
  elif [ -n "$C21_VIOL" ]; then
    bad "C21 判据 ⑥ ① 有违规（新建下限 $BENCH_SHORT_NEW_CONNS_MIN，实得 $(tr '\n' ' ' < "$C21_CONNS")）："
    printf '%s\n' "$C21_VIOL" | sed 's/^/        · /' >&2
  else
    ok "C21 四家都是每请求一条新连接（下限 $BENCH_SHORT_NEW_CONNS_MIN；实得「名字 每千请求 服务端TW 客户端TW」$(tr '\n' ' ' < "$C21_CONNS")）"
  fi
fi
# ⚠ 檔案不在時喂進去的是空串 ⇒ 判據按「沒抓到」判違規 —— ⛔ 不在這裡另寫一條「在不在」。
C21_REQ_VIOL=$(bench_request_close_violations "$(cat "$C21_REQ" 2>/dev/null || true)")
if [ -n "$C21_REQ_VIOL" ]; then
  bad "C21 判据 ⑥ ② 有违规：$C21_REQ_VIOL"
  sed 's/^/        | /' "$C21_REQ" >&2 2>/dev/null || true
else
  ok "C21 oha 发出的请求里带着 Connection: close（抓到的原文在 raw/short-connection-throughput/oha-request.txt）"
fi

# ── C22：CPU 拓撲必須落在原始資料裡（2026-10-05；診斷欄位，⛔ 不進合格性判定）──────
#
# ★ 起因：合格宿主與開發機診斷臺上，同一份二進位、同一個親和量到的比例差得很遠，
#   而兩臺機器上被測與壓測端各落在哪幾個**物理核**上從來沒記過（開發機上 `0-1` 是
#   同一個物理核的兩個超執行緒）⇒ 跨機器讀數時少了一條口徑，⛔ 別靠誰記得。
# 本條兩半，缺一半都不夠：
#   ① A 那一趟真跑出來的 `env.json` 裡 `thread_siblings` 記到了（採集真的接上了）；
#   ② 判斷本身判得動：兩份合成快照必須各紅一次 —— A 那一趟**沒設親和**，
#      ⇒ 「設了親和卻沒算出兩組核」那一半隻有合成的喂得到。
c22_check() {
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
host = d.get("host") or {}
t = host.get("cpu_topology")
if not t or not t.get("thread_siblings"):
    print("快照里没有 host.cpu_topology.thread_siblings —— 被测与压测端落在哪几个物理核上说不出来"); sys.exit(1)
if host.get("cpu_affinity") and (t.get("server_cores") is None or t.get("load_cores") is None):
    print("设了亲和（%s）却没算出两组核：%r" % (host["cpu_affinity"], t)); sys.exit(1)
print("%d 个逻辑 CPU 的超线程兄弟都记到；被测核 %s · 压测核 %s · 共用 %s" % (
    len(t["thread_siblings"]), t.get("server_cores"), t.get("load_cores"), t.get("shared_cores")))
' "$1"
}
echo "── C22 CPU 拓扑必须落在原始数据里 ──"
if c22_check "$OUT/env.json" > /tmp/bench-c22.txt 2>&1; then
  ok "C22 $(cat /tmp/bench-c22.txt)"
else
  bad "C22 $(cat /tmp/bench-c22.txt)"
fi
python3 -c '
import json
json.dump({"host": {"cpu_affinity": None}}, open("/tmp/bench-c22-neg-none.json", "w"))
json.dump({"host": {"cpu_affinity": "server=0-1 load=2-3", "cpu_topology": {
    "thread_siblings": {"0": "0-1", "1": "0-1"}, "server_cores": None,
    "load_cores": ["2-3"], "shared_cores": None}}}, open("/tmp/bench-c22-neg-cores.json", "w"))
'
for neg in /tmp/bench-c22-neg-none.json /tmp/bench-c22-neg-cores.json; do
  if c22_check "$neg" > /dev/null 2>&1; then
    bad "C22 判断判不动：$(basename "$neg") 是缺了拓扑的合成快照，它却判绿"
  else
    ok "C22 反向：$(basename "$neg") 如期判红"
  fi
done

echo
if [ "$FAILS" = 0 ]; then
  echo "BENCH GATE PASSED"
else
  echo "BENCH GATE FAILED：$FAILS 处" >&2
  exit 1
fi
