#!/usr/bin/env bash
# 对拍的**判据本体**（M3 第一刀，G132）。
#
# ★ ★ ★ 本文件里的每一个判据都写成**纯函数**：读数由调用方作参数传进来，
#   函数自己不去问宿主。⛔ 这不是风格洁癖，是判据能不能被反证的分水岭 ——
#   一个直接读 `/proc` 的判据，只能在**它恰好跑着的那台机器上**被观察到一种结果，
#   而那正是「只被观察过绿的门与不存在的门无法区分」那句话的形状。
#   ⇒ 参数化之后，两个方向都能用**合成输入**当场证给人看（见文件末尾的自测）。
#
# 被 `bench/env-snapshot.sh`（采集 → 判合格性）与 `bench/verdict.sh`（判定）共用。
#
#   bash bench/lib.sh --self-check     # 只跑自测，不需要 docker、不需要任何被测对象
#
# ⚠ 本文件**不产出任何性能数字**，也不读原始数据以外的东西。

set -euo pipefail

# ── 合格宿主的阈值 ─────────────────────────────────────────────────────────
#
# ★ 它们是**判据的一部分**，不是调试旋钮 —— 改动等于改口径，要连 `bench/README.md`
#   那张表一起改（`tests/bench/run.sh` 有一道门把两处对上）。
BENCH_MIN_CPUS=${BENCH_MIN_CPUS:-4}
BENCH_MAX_IDLE_LOAD=${BENCH_MAX_IDLE_LOAD:-0.50}

# ── 判据 ①：宿主合格性 ─────────────────────────────────────────────────────
#
#   bench_disqualifiers <kernel_release> <nproc> <loadavg1> <attest> <affinity>
#
# 每行打一条「不合格的理由」；**一行都不打 = 合格**。
#
# ★ ★ 五条分成性质完全不同的两半，⛔ 别把它们读成一张清单：
#
#   四条（kernel / cpus / load / affinity）是**容器自己看得见**的，机器判。
#   第五条（attest）是**容器原理上看不见**的 —— 「这台机器没在承载真业务」
#   「网络路径上没有 TUN 代理」「内核参数已按 bench/sysctl.conf 固化」这三件事，
#   在容器里问不出来（netns 是容器自己的，sysctl 读到的也是容器自己的）。
#   ⇒ 它要求一句**人写下来的**声明。⚠ 声明不是证明，`README.md` 把这一格
#     该核什么逐条写明；这里能做到的只有「谁都没声明过就一定不算合格」。
#
# ⚠ ⚠ **`kernel` 那一条不是在嫌弃 Windows**：G132 记着的实测理由是这台开发机上
#   那个 TUN 代理会干扰网络（容器出不去 UDP/443），而 Docker Desktop 的 Linux 侧
#   跑在 WSL2 里 ⇒ 内核串是本轮**唯一**一条不需要人配合就判得死的证据。
bench_disqualifiers() {
  local kernel=$1 cpus=$2 load1=$3 attest=$4 affinity=${5:-}

  # WSL2 / Docker Desktop：内核串里带 microsoft 或 WSL。
  # ⚠ 大小写两种都见过（`5.15.0-microsoft-standard-WSL2`）⇒ 先折成小写再比，
  #   ⛔ 别写两条 case 分支去枚举，枚举漏一种时它安静地判成合格。
  case "$(printf '%s' "$kernel" | tr '[:upper:]' '[:lower:]')" in
    *microsoft* | *wsl*)
      echo "kernel: 内核串是 '${kernel}' ⇒ 这是 WSL2 / Docker Desktop，不是专用对拍宿主"
      ;;
  esac

  # 核数：负载生成器与被测**不许共享 CPU**（§9 的固化项之一是 CPU 亲和）。
  # ⚠ 判据写成「分得出两组」而不是某个具体机型，⇒ 它不随机器换代过期。
  if ! printf '%s' "$cpus" | grep -qE '^[0-9]+$'; then
    echo "cpus: 核数读不出来（读到的是 '${cpus}'）——「没能检查」不算「检查通过」"
  elif [ "$cpus" -lt "$BENCH_MIN_CPUS" ]; then
    echo "cpus: 只有 ${cpus} 个核，少于 ${BENCH_MIN_CPUS} ⇒ 负载生成器与被测分不开"
  fi

  # 空载：机器在干别的活，就等于被测在与别人共享 CPU（G132 点名的第二条）。
  # ★ 用 awk 比浮点：bash 的 `[ ]` 只会整数比较，`0.9 -gt 0.5` 是语法错而不是 false。
  if ! printf '%s' "$load1" | grep -qE '^[0-9]+([.][0-9]+)?$'; then
    echo "load: 1 分钟负载读不出来（读到的是 '${load1}'）——「没能检查」不算「检查通过」"
  elif awk -v a="$load1" -v b="$BENCH_MAX_IDLE_LOAD" 'BEGIN { exit !(a > b) }'; then
    echo "load: 1 分钟负载 ${load1} 高于 ${BENCH_MAX_IDLE_LOAD} ⇒ 这台机器不空闲"
  fi

  # CPU 亲和：§9 的缓解项逐字写着「基准环境用脚本固化，含**内核参数与 CPU 亲和**」。
  # ★ ★ 它判的是「有没有把被测与负载生成器钉到不相交的核上」，⛔ 不是「钉得对不对」——
  #   钉得对不对要看具体机器的拓扑，那台机器今天还不存在。
  # ⚠ 这一条**容器自己看得见**（就是两个环境变量在不在），所以归机器判那一半；
  #   ⛔ 别把它挪进 attest 那一句里 —— 那等于把一件能自动判的事降格成一句声明。
  if [ -z "$affinity" ]; then
    echo "affinity: 没有设置 BENCH_SERVER_CPUS / BENCH_LOAD_CPUS ⇒ 被测与负载生成器抢同一批核"
  fi

  # 人写下来的那三件（容器看不见）。
  if [ -z "$attest" ]; then
    echo "attest: 没有任何人声明过「专机 + 无 TUN 代理 + 内核参数已固化」（见 bench/README.md）"
  fi
}

# ── 判据 ②：逐类取「该类最强者」 ───────────────────────────────────────────
#
#   bench_best_of <三家竞品的读数，每行 `<名字> <数值>`>
#
# 打一行 `<名字> <数值>`。
#
# ★ ★ G19 的门槛是「不劣于**该类最强者** 10%」，而 §8 明写「最强者逐类不同 ——
#   静态那一类比的是 nginx，L4 那一类比的是 HAProxy」。⇒ 这里**必须逐类现算**，
#   ⛔ 不许把某一家钉成基准，也不许取三家平均。
# ⚠ 本函数只吃**竞品**的读数：把枢衡自己也喂进来会让它在自己领先时拿自己当门槛，
#   那道门就恒真了。调用方负责别喂错，`bench_verdict_one` 是唯一的调用点。
bench_best_of() {
  awk '
    NF >= 2 && $2 + 0 == $2 { if (!seen || $2 + 0 > best + 0) { best = $2; name = $1; seen = 1 } }
    END { if (seen) printf "%s %s\n", name, best }
  '
}

# ── 判据 ③：一类用例的判定 ─────────────────────────────────────────────────
#
#   bench_verdict_one <枢衡的读数> <三家竞品的读数，每行 `<名字> <数值>`>
#
# 打一行 `PASS|FAIL <最强者名> <最强者数值> <门槛值> <枢衡数值>`。
#
# ★ 方向：吞吐类**越大越好** ⇒ 门槛是 `最强者 × 0.9`，枢衡要 `>=` 它。
# ⚠ ⚠ 延迟类是**反过来**的（越小越好），而本轮只有「静态吞吐」一类 ⇒
#   ⛔ 本函数**有意只处理越大越好那一种**，加第二类时要显式加方向参数，
#   而不是让它去猜。留一个会猜的判据，比留一个只做一半的判据坏得多。
bench_verdict_one() {
  local ours=$1 rivals=$2
  local best_line best_name best_val floor
  best_line=$(printf '%s\n' "$rivals" | bench_best_of)
  [ -n "$best_line" ] || { echo "NODATA"; return 0; }
  best_name=${best_line%% *}
  best_val=${best_line##* }
  floor=$(awk -v b="$best_val" 'BEGIN { printf "%.4f", b * 0.9 }')
  if awk -v o="$ours" -v f="$floor" 'BEGIN { exit !(o + 0 >= f + 0) }'; then
    printf 'PASS %s %s %s %s\n' "$best_name" "$best_val" "$floor" "$ours"
  else
    printf 'FAIL %s %s %s %s\n' "$best_name" "$best_val" "$floor" "$ours"
  fi
}

# ── 自测：全部用**合成输入** ───────────────────────────────────────────────
#
# ★ ★ ★ 不依赖宿主上此刻恰好是什么样（同 G133 的九条自测）。这一点是承重的：
#   本轮唯一跑得到的宿主是**不合格**的那一台 ⇒ 若自测也从宿主取读数，
#   「合格」那条分支就一次都执行不到，而**一个永远返回不合格的判据，
#   与一个坏掉的判据给出完全相同的输出**。
bench_self_check() {
  local rc=0
  local out
  # ⚠ ⚠ 条数**从计数器派生，⛔ 不写死**：一个写在消息里的计数没有任何门守着，
  #   加了一条断言却忘了改那个数，两边都不会红 —— 本仓 2026-09-05 当天栽过一次
  #   同形状的（`19−2−11−6` 与 `19−2−10−7` 都等于 0，错的中间项与谁都不矛盾）。
  local n=0
  # 三个断言原语。★ 计数在这里**只发生一处** ⇒ 加断言时数字自己会跟上。
  want_empty() { n=$((n + 1)); [ -z "$2" ] || { echo "✗ $1（实得：$2）" >&2; rc=1; }; }
  want_eq() { n=$((n + 1)); [ "$3" = "$2" ] || { echo "✗ $1（该是 '$2'，实得 '$3'）" >&2; rc=1; }; }
  # shellcheck disable=SC2254  # 模式**有意**不加引号：第 2 个参数就是一个 glob
  want_match() { n=$((n + 1)); case "$3" in $2) ;; *) echo "✗ $1（实得 '$3'）" >&2; rc=1 ;; esac; }

  # ★ 一台**合成的**合格宿主：五个参数全部处在合格那一侧。
  #   下面每条负向用例都从它出发，**只翻一个变量** —— 这样红的来源说得清。
  local OK_KERNEL="6.12.0-generic" OK_CPUS=16 OK_LOAD="0.03"
  local OK_ATTEST="专机 · 无代理 · sysctl 已固化" OK_AFF="server=0-3 load=4-7"

  # —— 合格性：合格那一侧必须真的存在（★ ★ ★ 承重的就是这一条）——
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_empty "一台合成的合格宿主被判成了不合格" "$out"

  # —— 五条各自要判得动（每条单独翻一个变量，其余保持合格）——
  out=$(bench_disqualifiers "5.15.0-microsoft-standard-WSL2" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_match "WSL2 内核串没被判出来" '*kernel:*' "$out"
  # ⚠ 大写那一种单独钉一条：`tr` 折小写那一步删掉时，只有这条会红。
  out=$(bench_disqualifiers "6.6.0-WSL2" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_match "大写的 WSL 没被判出来（折小写那一步失效了）" '*kernel:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" 2 "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_match "核数不足没被判出来" '*cpus:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "3.20" "$OK_ATTEST" "$OK_AFF")
  want_match "高负载没被判出来" '*load:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "")
  want_match "没设 CPU 亲和没被判出来" '*affinity:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "" "$OK_AFF")
  want_match "缺声明没被判出来" '*attest:*' "$out"

  # —— 「读不出来」必须判红，⛔ 不许当成通过 ——
  out=$(bench_disqualifiers "$OK_KERNEL" "" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_match "核数读空被当成了合格" '*cpus:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "unknown" "$OK_ATTEST" "$OK_AFF")
  want_match "负载读不出来被当成了合格" '*load:*' "$out"

  # —— 边界：恰好等于阈值的那一侧算合格（⛔ 别让门在边界上随机翻面）——
  out=$(bench_disqualifiers "$OK_KERNEL" "$BENCH_MIN_CPUS" "$BENCH_MAX_IDLE_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_empty "恰好压在阈值上的宿主被判成了不合格" "$out"

  # —— 最强者：逐类现算，⛔ 不是第一行、不是平均 ——
  out=$(printf 'caddy 100\nnginx 300\nhaproxy 200\n' | bench_best_of)
  want_eq "最强者算错了" "nginx 300" "$out"
  # ⚠ 把最大值放在**第一行**：一个「取第一行」的错实现在上一条里也会绿。
  out=$(printf 'nginx 300\ncaddy 100\nhaproxy 200\n' | bench_best_of)
  want_eq "最大值在首行时算错了" "nginx 300" "$out"
  # ⚠ 非数字行必须被跳过，⛔ 不许把它当成 0 参与比较后仍宣称有数据。
  out=$(printf 'caddy n/a\nnginx 300\n' | bench_best_of)
  want_eq "非数字行没被跳过" "nginx 300" "$out"
  out=$(printf 'caddy n/a\n' | bench_best_of)
  want_empty "一行有效数据都没有时还报出了最强者" "$out"

  # —— 判定：两个方向都要出得来 ——
  out=$(bench_verdict_one 280 "$(printf 'caddy 100\nnginx 300\nhaproxy 200\n')")
  want_match "280 对最强者 300（门槛 270）该 PASS" "PASS *" "$out"
  out=$(bench_verdict_one 260 "$(printf 'caddy 100\nnginx 300\nhaproxy 200\n')")
  want_match "260 对最强者 300（门槛 270）该 FAIL" "FAIL *" "$out"
  # ⚠ 边界：恰好 90% 算过（10% 是「不劣于」，含等号）。
  out=$(bench_verdict_one 270 "$(printf 'nginx 300\n')")
  want_match "恰好 90% 该 PASS" "PASS *" "$out"
  out=$(bench_verdict_one 999 "")
  want_eq "没有竞品读数时该报 NODATA" "NODATA" "$out"

  if [ "$rc" = 0 ]; then
    echo "[bench/lib] 判据自测通过（合成输入，${n} 条）"
  else
    echo "[bench/lib] ★ 判据自测未通过 —— **本次对拍的任何结论都不可信**" >&2
  fi
  return "$rc"
}

# 被 `source` 时什么都不做；直接跑且带 --self-check 时跑自测。
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  case "${1:-}" in
    --self-check) bench_self_check ;;
    *) echo "用法：bash bench/lib.sh --self-check" >&2; exit 2 ;;
  esac
fi
