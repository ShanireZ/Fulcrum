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

# ── 压测端饱和的两个阈值（判据 ④）─────────────────────────────────────────
#
# ⚠ ⚠ ★ **这两个数今天没有实测支撑，它们是判断，不是读数。** 合格宿主还不存在
#   ⇒ 「四套不同实现之间正常该差多少」这件事本仓一次都没量过。⛔ 别把它们
#   读成「实测得出的阈值」；★ 第一次在合格宿主上跑完之后**要回头核这两个数**，
#   那一趟的四家真实离散度就是定它们的判据。
# ★ 取 0.05 的理由写在判据 ④ 的注释里（四套结构完全不同的实现挤进 5% 更像是
#   共享了外部上限）—— 是一条可以被第一组真实数据推翻的假设，⛔ 不是结论。
BENCH_MIN_SPREAD=${BENCH_MIN_SPREAD:-0.05}
BENCH_GEN_HEADROOM=${BENCH_GEN_HEADROOM:-0.90}

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

# ── 判据 ④：压测端自己先饱和 ───────────────────────────────────────────────
#
#   bench_saturation <生成器上限读数或空> <全场读数，每行 `<名字> <数值>`>
#
# 每行打一条「这一类不可信的理由」；**一行都不打 = 没测到饱和**。
# ★ 契约与判据 ① 逐字相同（打理由 / 空 = 通过）⇒ 两个调用点可以同一个形状处理。
#
# ★ ★ ★ **它守的失效不会让任何东西变红。** 负载生成器自己先到顶时，四家读数会
#   一起收敛到生成器的天花板，而判据 ③（`ours >= best × 0.9`）**照常打 PASS** ——
#   输出一组看起来完全正常、其实量的是 oha 的数。⇒ 判据 ① 那五条一条都答不了它：
#   它们判的是「这台机器合不合格」，而这里坏掉的是**量具本身到顶了**。
#
# 两条判据，性质完全不同：
#
#   **B（收敛，承重）** —— 全场读数彼此挤在一起就作废该类。★ 它**不需要任何新测量**，
#     用的就是已经采到的那组数 ⇒ 今天就能用合成输入把两个方向全反证掉。
#     依据：nginx（C）· HAProxy（C）· Caddy（Go）· 枢衡（Rust/pingora）是四套结构
#     完全不同的实现，它们落进同一个百分点里，**「共享了某个外部上限」远比
#     「它们真的一样快」更可能**。⚠ 它说不出是哪一个上限（生成器 / 环回 / 亲和）——
#     ⛔ 别把它的报文读成「一定是 oha 饱和了」，它只说「这组数不可比」。
#
#   **A（绝对天花板，可选）** —— 给了生成器上限读数时，最强者逼近它就作废该类。
#     ⚠ ⚠ A 的正确性**整个压在「参照服务真的比四家都快」这一条上**，而那条在合格
#     宿主到位之前**验不了**：参照选慢了 ⇒ 恒作废；选的是另一个真实服务器 ⇒ 量到的
#     是 `min(oha, 那个服务器)`，天花板被低估。⇒ ⛔ **今天它是可选输入、不是承重件**：
#     上限留空时 A 完全不参与，B 照常判。参照服务选定那天再把它打开。
#
# ⚠ ⚠ 误判方向是**有意不对称**的：一次误作废只是**扣下一个结论**，一次漏判是
#   **给出一个错结论**。⇒ 两条都往「作废」那侧倒，且**「判不了」一律作废，
#   ⛔ 不算通过**（读数不足两条 / 上限不是正数 —— 同判据 ① 那句「『没能检查』
#   不算『检查通过』」）。
#
# ★ 边界与判据 ① 取同一个约定：**恰好压在阈值上算好的那一侧**（不作废），
#   ⇒ 两处都用严格不等号，⛔ 别让门在边界上随机翻面。
bench_saturation() {
  local ceiling=$1 readings=$2
  local stats cnt max min max_name min_name spread

  # ★ 数值那一格用与 `bench_best_of` **逐字相同**的判法（`$2 + 0 == $2`）：
  #   两处对「什么算一条有效读数」的口径必须一致，否则同一组数在判据 ②
  #   与判据 ④ 里的「全场」不是同一个集合，而那种不一致不会有任何东西说。
  stats=$(printf '%s\n' "$readings" | awk '
    NF >= 2 && $2 + 0 == $2 {
      n++
      if (n == 1 || $2 + 0 > max + 0) { max = $2; max_name = $1 }
      if (n == 1 || $2 + 0 < min + 0) { min = $2; min_name = $1 }
    }
    END { printf "%d %s %s %s %s\n", n + 0, (n ? max : "-"), (n ? min : "-"), (n ? max_name : "-"), (n ? min_name : "-") }
  ')
  read -r cnt max min max_name min_name <<< "$stats"

  # —— B：收敛（承重）——
  if [ "$cnt" -lt 2 ]; then
    echo "spread: 只有 ${cnt} 条有效读数，判不了收敛（至少要 2 条）——「判不了」不算「没饱和」"
  elif ! awk -v m="$max" 'BEGIN { exit !(m + 0 > 0) }'; then
    echo "spread: 最强者读数是 '${max}'，不是正数 ⇒ 判不了收敛"
  else
    spread=$(awk -v hi="$max" -v lo="$min" 'BEGIN { printf "%.4f", (hi - lo) / hi }')
    if awk -v s="$spread" -v t="$BENCH_MIN_SPREAD" 'BEGIN { exit !(s < t) }'; then
      echo "spread: 全场 ${cnt} 家挤在 ${spread} 以内（阈值 ${BENCH_MIN_SPREAD}）——最高 ${max_name}=${max}，最低 ${min_name}=${min} ⇒ 四套不同实现这么齐，更像是共享了某个外部上限（生成器 / 环回 / 亲和），⛔ 这组数不可比"
    fi
  fi

  # —— A：绝对天花板（可选；⛔ 上限留空时整条不参与）——
  [ -n "$ceiling" ] || return 0
  if ! printf '%s' "$ceiling" | grep -qE '^[0-9]+([.][0-9]+)?$' ||
    awk -v c="$ceiling" 'BEGIN { exit !(c + 0 <= 0) }'; then
    echo "ceiling: 生成器上限读数是 '${ceiling}'，不是正数 ——「判不了」不算「没饱和」"
  elif [ "$cnt" -ge 1 ] &&
    awk -v m="$max" -v c="$ceiling" -v h="$BENCH_GEN_HEADROOM" 'BEGIN { exit !(m + 0 > c * h) }'; then
    echo "ceiling: 最强者 ${max_name}=${max} 已越过生成器上限 ${ceiling} 的 ${BENCH_GEN_HEADROOM} ⇒ 量到的可能是 oha 自己的天花板，不是被测的能力"
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

  # —— 判据 ④：压测端饱和 ————————————————————————————————————————
  #
  # ★ 一组**分得开**的全场读数：下面每条负向用例都从它出发，只翻一个变量。
  local SPREAD_OK CEIL_SET
  SPREAD_OK=$(printf 'fulcrum 300\nnginx 280\nhaproxy 200\ncaddy 100\n')
  # ★ ★ 合格那一侧必须真的存在（同判据 ① 那条承重的自测）：
  #   **一个恒作废的判据，与一个坏掉的判据给出完全相同的输出。**
  out=$(bench_saturation "" "$SPREAD_OK")
  want_empty "一组分得开的读数被判成了饱和" "$out"

  # —— B（承重）：四家挤在一起 ——
  local SPREAD_TIGHT
  SPREAD_TIGHT=$(printf 'fulcrum 1000\nnginx 1010\nhaproxy 1020\ncaddy 1005\n')
  out=$(bench_saturation "" "$SPREAD_TIGHT")
  want_match "四家挤在 2% 以内没被判成饱和" '*spread:*' "$out"

  # ★ ★ ★ **承重的是这一条对照**：同一组饱和数据喂给判据 ③，它照常打 PASS
  #   （枢衡 1000 对最强者 1020，门槛 918）。⇒ ④ 抓的正是 ③ **结构上**抓不到的
  #   那件事。⛔ 少了这条，「④ 判得动」与「④ 在重复 ③ 已经会做的事」分不开。
  out=$(bench_verdict_one 1000 "$(printf 'nginx 1010\nhaproxy 1020\ncaddy 1005\n')")
  want_match "同一组饱和数据判据 ③ 本来就该照常 PASS（这正是 ④ 存在的理由）" "PASS *" "$out"

  # —— 边界：离散度恰好等于阈值算**不**饱和（⛔ 别在边界上随机翻面）——
  #   (100 − 95) / 100 = 0.0500，恰好是 BENCH_MIN_SPREAD。
  out=$(bench_saturation "" "$(printf 'a 100\nb 95\n')")
  want_empty "离散度恰好等于阈值时被判成了饱和" "$out"

  # —— 「判不了」必须作废，⛔ 不许当成「没饱和」——
  out=$(bench_saturation "" "$(printf 'fulcrum 300\n')")
  want_match "只有一条读数时被当成了没饱和" '*spread:*' "$out"
  out=$(bench_saturation "" "")
  want_match "一条读数都没有时被当成了没饱和" '*spread:*' "$out"
  # ⚠ 非数字行不算有效读数（与 bench_best_of 同一口径）⇒ 只剩一条 ⇒ 判不了。
  out=$(bench_saturation "" "$(printf 'fulcrum 300\nnginx n/a\n')")
  want_match "非数字行被当成了有效读数" '*spread:*' "$out"

  # —— A：上限那一半。★ CEIL_SET 的离散度是 0.7778，B 一定不响 ⇒
  #   下面几条红或不红**只可能来自 A**，红的来源说得清。
  CEIL_SET=$(printf 'fulcrum 900\nnginx 600\nhaproxy 400\ncaddy 200\n')
  # ★ ★ 一个变量的翻面：同一组读数，上限留空 vs 给一个逼近的上限。
  out=$(bench_saturation "" "$CEIL_SET")
  want_empty "上限留空时 A 不该参与（它是可选输入，不是承重件）" "$out"
  out=$(bench_saturation 950 "$CEIL_SET")
  want_match "最强者逼近上限没被判出来" '*ceiling:*' "$out"
  out=$(bench_saturation 10000 "$CEIL_SET")
  want_empty "最强者远低于上限时被判成了饱和" "$out"
  # ⚠ 边界：900 恰好等于 1000 × 0.90 ⇒ 算**不**饱和。
  #   ★ 数值有意取整除得尽的一对，⛔ 别用 300/0.9 那种 —— 浮点尾数会让边界随机翻面，
  #     而那样的话这条自测本身就成了一个不可靠的判据。
  out=$(bench_saturation 1000 "$CEIL_SET")
  want_empty "最强者恰好压在 上限×headroom 上时被判成了饱和" "$out"
  # —— 上限读不出来必须作废 ——
  out=$(bench_saturation "n/a" "$CEIL_SET")
  want_match "上限不是数字时被当成了没饱和" '*ceiling:*' "$out"
  out=$(bench_saturation 0 "$CEIL_SET")
  want_match "上限是 0 时被当成了没饱和" '*ceiling:*' "$out"

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
