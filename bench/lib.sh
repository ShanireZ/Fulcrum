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
# ⚠ ⚠ ★ **这两个数仍然不是实测得出的阈值。** 2026-09-06 第一次在合格宿主上跑完之后
#   已经回核过一轮，逐条结论在 `bench/README.md`「这两个阈值的回核」一节：
#
#   · `0.05` 那一趟**没有误报**（全场 spread = 0.8599，远高于它）。
#     ⚠ ⚠ ★ **但同一组数暴露了 B 的一个真实缺陷**：最快两家（haproxy / nginx）只差
#     **0.0071**，正是 B 声称要怀疑的形状，而 B 一声没吭 —— 因为它取的是**全场极差**，
#     被枢衡的慢整个撑开了。⇒ **一个慢的被测会掩盖掉快的那几家之间的收敛。**
#     ✅ 已由 **`G146`** 结案（原 `D34`）：新增判据 **④C**（`bench_top_convergence`），
#     判「最强者 vs 第二强」，**只作用于 PASS**。⛔ 但 `0.05` 这个数**本身**
#     仍然没有实测支撑 —— G146 改的是「拿它去比什么」，不是「它该取多少」。
#   · `0.90` **一次都没被执行过**（那一趟 A 没打开，没有 ceiling.txt）⇒ 仍然验不了。
#
# ★ 取 0.05 的理由写在判据 ④ 的注释里（四套结构完全不同的实现挤进 5% 更像是
#   共享了外部上限）—— 是一条可以被真实数据推翻的假设，⛔ 不是结论。
BENCH_MIN_SPREAD=${BENCH_MIN_SPREAD:-0.05}
BENCH_GEN_HEADROOM=${BENCH_GEN_HEADROOM:-0.90}

# ── 判据 ⑤ 的阈值：每千请求新建多少条上游连接才算「没在复用」──────────────────
#
# ★ ★ **与上面那两个不同，这一个有实测支撑**（2026-09-09 `ShanirePCX`，
#   全部在**门禁那组参数**下量的：`BENCH_GATE_DURATION=2s` / `..._CONNECTIONS=10`）：
#
#   · 对齐口径配好，**三趟**：3.20/3.16/2.40/2.39 · 2.92/—/1.72/— · 0.84/1.31/0.68/1.39
#     ⇒ ★ ★ **同一形态波动约 5 倍**（0.68–3.20），⛔ 这不是噪声可以忽略的量级。
#   · 把 nginx 那三行撤掉 ⇒ nginx **1005.65**（每请求恰好新建一条）
#   · 另在 10s/50 连接下量过：把 caddy 的 `transport` 块撤掉 ⇒ **100.30**
#
# ⇒ 取 50：离对齐那侧的**最大观测值** 3.20 有 15 倍余量，离两种未对齐形态
#   分别有 20 倍与 2 倍。
#
# ⛔ **不取 10**：① 对齐那侧实测已经到过 3.20，而波动是 5 倍量级 ⇒ 余量只剩 3 倍；
#   ② 门禁跑 2s × 10 连接，若某趟 rps 掉到 500，oha 自己那 10 条连接就是 10/千。
#   两条都指向同一件事 —— 那会变成一道**随机翻面**的门。
#   ⚠ 这个观测量是**整个 netns 的**累计值，本来就含 oha 那一份
#   （见 `bench/case/reverse-proxy-throughput.sh` 里 `passive_opens` 那段）。
#
# ⚠ ⚠ ★ **它的能力边界写在自测里**（`bench_self_check` 判据 ⑤ 那一段）：
#   门禁参数下它只抓得住「每请求新建」那一族；「部分不复用」那一族的表现依赖
#   并发数，要到窗口参数（连接数大一个量级）才暴露。⛔ 别把它读成「四家都验过了」。
BENCH_UPSTREAM_REUSE_MAX=${BENCH_UPSTREAM_REUSE_MAX:-50}

# ── 负载参数：**缺省只在这里定义一次**（2026-09-06）───────────────────────────
#
# ⚠ ⚠ ★ ★ ★ **这四行为什么必须在这个文件里，而不是在用例脚本里。**
#
# 在它们搬过来之前，缺省写在 `bench/case/static-throughput.sh`（`:-4096` 那一族），
# 而 `bench/env-snapshot.sh` 记的是**声明值** `${BENCH_PAYLOAD_BYTES:-}`
# ⇒ 没人显式设的那些参数，**用例真的用了缺省，而快照写 `null`**。
# 2026-09-06 那组读数就带着这个缺陷：`payload_bytes: null`，而那一趟真的用了 4096。
# ★ 门禁里同时露出的还有 `workers: null` ⇒ ⛔ **别把它记成「payload 的毛病」**，
#   四个参数用的是同一个写法，只是那一趟 owner 恰好显式设了另外三个。
#
# ★ ★ G19 要的是**原始数据可被第三方复现**，而一份说不出自己用了多大 payload、
#   几个 worker 的静态吞吐读数**复现不出来** ⇒ 这不是元数据，是口径本身。
#
# ⛔ **别把缺省「顺手也写进 `env-snapshot.sh` 一份」** —— 那才是更坏的形态：
#   两处缺省一旦飘掉，快照会**理直气壮地报一个错的数**，而 `null` 至少不骗人。
#   ⇒ 一处定义、两边都 `source` 本文件，于是这一族缺陷**结构性地不存在**。
# ★ 与上面那几个阈值同一个写法（`${X:-缺省}`）⇒ 宿主侧设了就一路带进容器
#   （`bench/docker-run.sh` 的 `-e BENCH_*`），没设则两边各自算出**同一个**缺省。
#
# ⚠ 这四个值本身是**口径**，改它们要连 `bench/README.md` 一起改。
#   ⛔ 真要出数那天必须显式设 `BENCH_WORKERS`（理由见用例脚本头部第 ⑤ 条）。
BENCH_DURATION=${BENCH_DURATION:-10s}
BENCH_CONNECTIONS=${BENCH_CONNECTIONS:-50}
BENCH_WORKERS=${BENCH_WORKERS:-1}
BENCH_PAYLOAD_BYTES=${BENCH_PAYLOAD_BYTES:-4096}

# ── 判据 ①：宿主合格性 ─────────────────────────────────────────────────────
#
#   bench_disqualifiers <kernel_release> <nproc> <loadavg1> <attest> <affinity> [kparam_mismatch]
#
# 每行打一条「不合格的理由」；**一行都不打 = 合格**。
#
# ★ ★ 六条分成性质完全不同的三半，⛔ 别把它们读成一张清单：
#
#   四条（kernel / cpus / load / affinity）是**容器自己看得见**的，机器判。
#
#   一条（kernel-params）是 **G145 从 attest 里拆出来的** —— 它此前是那句声明的第三件，
#   而实测证明「已固化」这句话可以完全诚实、同时对被测行为零影响（per-netns，
#   传不进容器）。⇒ 现在它由真实比较喂进来，比较本身在 `bench_kparam_mismatches`。
#   ★ 本参数**有默认值** ⇒ G145 之前写的每一条调用都还能跑，⛔ 但那意味着漏传时
#     它安静地不判 —— 所以 `env-snapshot.sh` 那唯一一处真实调用必须传满六个参数，
#     而自测里专门有一条钉住「传了不符的比较结果就必须红」。
#
#   一条（attest）是**容器原理上看不见、也没有任何门看得见**的 ——
#   「这台机器没在承载真业务」与「网络路径上没有 TUN 代理」这两件，
#   在容器里问不出来（netns 是容器自己的）。
#   ⇒ 它要求一句**人写下来的**声明。⚠ 声明不是证明，`README.md` 把这一格
#     该核什么逐条写明；这里能做到的只有「谁都没声明过就一定不算合格」。
#
# ⚠ ⚠ **`kernel` 那一条不是在嫌弃 Windows**：G132 记着的实测理由是这台开发机上
#   那个 TUN 代理会干扰网络（容器出不去 UDP/443），而 Docker Desktop 的 Linux 侧
#   跑在 WSL2 里 ⇒ 内核串是本轮**唯一**一条不需要人配合就判得死的证据。
bench_disqualifiers() {
  local kernel=$1 cpus=$2 load1=$3 attest=$4 affinity=${5:-} kparam_mismatch=${6:-}

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

  # 人写下来的那两件（容器看不见，**也没有任何门看得见**）。
  # ★ ★ G145 之前这里是三件，第三件是「内核参数已固化」。它现在**不在这句声明里了** ——
  #   六个键各自有了门（四个容器侧 + 一个宿主侧 + nofile），见本文件 `bench_container_sysctls`
  #   的注释。⛔ 别把它加回来：一件既有门又要人声明的事，会让人以为声明是那道门。
  if [ -z "$attest" ]; then
    echo "attest: 没有任何人声明过「专机 + 无 TUN 代理」（见 bench/README.md）"
  fi

  # 内核参数（G145）。★ 比较本身在 `bench_kparam_mismatches` 里，本函数只把它的输出
  #   转成不合格理由 —— ⇒ 「怎么比」与「比出来算不算不合格」各自可被单独反证。
  # ⚠ 逐行加前缀，⛔ 不是 `printf '前缀 %s\n' "$整串"` —— 后者在多条不符时只给
  #   第一行加前缀，而本函数对外的契约是「**每行**一条不合格理由」。
  if [ -n "$kparam_mismatch" ]; then
    printf '%s\n' "$kparam_mismatch" | sed 's/^/kernel-params: /'
  fi
}

# ── 判据 ①bis：内核参数真的按声明生效了吗（G145）─────────────────────────────
#
# ★ ★ ★ **为什么这一格必须存在**：G145 之前，「内核参数已固化」是 `attest` 那句话里的
#   第三件，⇒ 它没有门。而 2026-09-06 的实测把问题挖得更深 —— 就算真的在宿主上
#   `sysctl --system` 了，**四个键里没有一个传得进对拍容器**：
#
#     宿主 net.ipv4.ip_local_port_range = 20000 60000 ⇒ 新开的默认网络容器仍读到 32768 60999
#     宿主 net.core.somaxconn           = 12345       ⇒ 新开的默认网络容器仍读到 4096
#
#   它们是 **per-netns** 的，而对拍整个跑在自己的 netns 里。
#   ⇒ 一份「已固化」的声明可以完全诚实，同时对被测行为**零影响**。
#   ⚠ 这正是本仓最在意的那一族：一道**恒绿**的门与一道不存在的门，输出完全一样。
#
# ⇒ G145 的落法：**四个 per-netns 键改由 `docker run --sysctl` 在容器里设**，
#   而本函数在容器内断言「实测值 == 声明值」。⛔ 声明与实测分家时判红。

# 容器侧那四个键 —— ★ ★ **唯一**那份声明。
#   `bench/docker-run.sh` 用它拼 `--sysctl` 旗标，`bench/env-snapshot.sh` 用它当期望值。
#   ⛔ 别在任何别处再抄一份：两份一旦分家，容器会按 A 跑而快照按 B 判，**两边都不红**。
#
# ⚠ ⚠ `tcp_tw_reuse` 声明成 **2 而不是 1**（G145 改的，此前是 1）。
#   `2` 的语义是「**仅对环回**启用」，而对拍的四家与 oha 同处一个容器、只走环回
#   ⇒ 2 恰好覆盖本场景，而 1（全局启用）比它更宽却**一点也不更贴切**。
#   ★ 顺带：2 也正是 Linux 4.12 起的内核缺省 ⇒ 这一条声明的是「缺省没有被改坏」。
bench_container_sysctls() {
  printf '%s\n' \
    'net.core.somaxconn=65535' \
    'net.ipv4.tcp_max_syn_backlog=65535' \
    'net.ipv4.ip_local_port_range=10240 65535' \
    'net.ipv4.tcp_tw_reuse=2'
}

# 宿主侧那一条 —— ⛔ 它**设不进也读不到**容器里。
#
# ★ 实测（2026-09-06，那台候选对拍宿主，Linux 6.8）：
#     docker run --sysctl net.core.netdev_max_backlog=16384 …
#       ⇒ runc 当场拒绝：`open sysctl … : no such file or directory`
#     容器内 `cat /proc/sys/net/core/netdev_max_backlog`
#       ⇒ `No such file or directory`（它非 netns 化 ⇒ 不出现在容器的 /proc/sys/net 视图里）
#
# ⇒ 它只能在**宿主上**设、在**宿主上**判 —— 那道门在 `bench/docker-run.sh` 里，
#   而启动器把宿主实测值经环境变量传进容器，本文件据此判「启动器到底跑没跑过」。
#
# ★ 为什么单独留它而不是一并丢掉：**环回包也走 per-CPU `input_pkt_queue`**，
#   正是这个键管的队列。那台机器的出厂值是 **1000** ⇒ 它是六个键里**唯一**
#   既有效、又真有可能在对拍里成为瓶颈的。
bench_host_sysctls() {
  printf '%s\n' 'net.core.netdev_max_backlog=16384'
}

# 容器里的 `nofile` 软/硬上限。
# ⚠ ⚠ ★ **实测容器缺省只有 1024**（2026-09-06，docker 29.1.3）—— 而 `fs.file-max`
#   在那台机器上是 9223372036854775807。⇒ 会咬的从来不是 `fs.file-max`，是这个。
#   ★ G145 之前的 `bench/sysctl.conf` 只管前者，而且把它**从 9.2e18 降到 2097152**。
BENCH_NOFILE=${BENCH_NOFILE:-1048576}

# `docker run` 要加的旗标 —— ★ ★ **从上面那两份声明推导，⛔ 不另写一份清单**。
#   两个调用方（`bench/docker-run.sh` 真跑、`tests/bench/run.sh` 门禁）都用它。
#   ⚠ 旗标与清单一旦分家，容器会按 A 跑而快照按 B 判，**两边都不会红**。
#
# ⚠ **一行一个 token**，⛔ 不是一整行空格分隔的字符串：
#   `net.ipv4.ip_local_port_range=10240 65535` 这个值**自己带空格**，
#   拼成一行再让调用方分词，会把它劈成两个参数而 docker 只报一句语焉不详的用法错。
#   ⇒ 调用方用 `while IFS= read -r` 逐行读进数组。
bench_docker_sysctl_flags() {
  local line
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    printf '%s\n%s\n' '--sysctl' "$line"
  done <<EOF
$(bench_container_sysctls)
EOF
  printf '%s\n%s\n' '--ulimit' "nofile=${BENCH_NOFILE}:${BENCH_NOFILE}"
}

# 把任意空白（含 tab）压成单个空格并去掉首尾。
# ⚠ ⚠ 这一步是承重的：`sysctl` 打多值时用 **tab** 分隔（`10240<TAB>65535`），
#   而声明里写的是空格 ⇒ 不归一化的话，一次完全正确的固化会被判成不符。
_bench_norm_ws() {
  printf '%s' "$1" | tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//'
}

# 在一份 `key=value` 多行文本里查一个键；查不到返回非 0（⛔ 不返回空串当成查到了）。
_bench_sysctl_lookup() {
  local key=$1 observed=$2 line
  while IFS= read -r line; do
    case "$line" in
      "$key="*)
        printf '%s' "${line#*=}"
        return 0
        ;;
    esac
  done <<EOF
$observed
EOF
  return 1
}

#   bench_kparam_mismatches <声明 key=value 多行> <实测 key=value 多行>
#
# 每行打一条问题；**一行都不打 = 逐条相符**。
#
# ⚠ 三种「判不了」一律算问题，⛔ 不算通过：
#   ① 声明本身是空的 —— 一个空声明会让本函数**恒返回空集**，而那与「全部相符」
#      的输出一模一样。★ 这是本仓反复栽的那一族，所以它排在最前面。
#   ② 某个键在实测里读不到（⚠ 报文有意不写「在容器里」——宿主侧那道门也用这个函数） —— 「没能检查」不算「检查通过」。
#   ③ 读到了但不等于声明值。
bench_kparam_mismatches() {
  local declared=$1 observed=$2
  local line key want got

  if [ -z "$(printf '%s' "$declared" | tr -d '[:space:]')" ]; then
    echo "声明为空 —— 一个空声明会让本判据恒返回「全部相符」，⛔ 那不算通过"
    return 0
  fi

  while IFS= read -r line; do
    [ -n "$line" ] || continue
    key=${line%%=*}
    want=$(_bench_norm_ws "${line#*=}")
    if ! got=$(_bench_sysctl_lookup "$key" "$observed"); then
      echo "${key} 读不到 —— 「没能检查」不算「检查通过」"
      continue
    fi
    got=$(_bench_norm_ws "$got")
    [ "$got" = "$want" ] || echo "${key} 实测 '${got}' ≠ 声明 '${want}'"
  done <<EOF
$declared
EOF
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
# ⚠ ⚠ 延迟类是**反过来**的（越小越好）⇒
#   ⛔ 本函数**有意只处理越大越好那一种**，**加第一个方向相反的类时**要显式加
#   方向参数，而不是让它去猜。留一个会猜的判据，比留一个只做一半的判据坏得多。
# ⚠ 2026-09-09：第二类（反代吞吐）加进来了，而它仍是 rps 越大越好 ⇒ **没加参数**。
#   ⛔ 这条原本写的是「加第二类时」——第二类已经加了却没有方向参数，那句话会让
#   下一个人当成有人偷懒。★ 最可能逼出这个参数的是「TLS 握手」那一类
#   （若它取握手延迟口径而不是握手速率）。
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

# ── 判据 ④C：最强者附近的收敛（`G146`，结案 `D34`）──────────────────────────
#
#   bench_top_convergence <全场读数，每行 `<名字> <数值>`>
#
# 每行打一条理由；**一行都不打 = 最强者与第二强分得开**。
#
# ★ ★ ★ **它存在的理由是一组真实数据推翻了 B 的一个前提。** 2026-09-06 第一趟
#   真实对拍：全场极差 **0.8599**（远高于 `BENCH_MIN_SPREAD`）⇒ B 一声没吭；
#   而**最快两家 haproxy 与 nginx 只差 0.0071** —— 两套架构差异极大的实现落进
#   0.71% 以内，**那正是 B 声称要怀疑的形状**。
#   ⇒ B 取的是**全场极差**，被最慢那一家（枢衡 7638）整个撑开了
#   ⇒ **一个慢的被测会掩盖掉快的那几家之间的收敛。**
#
# ⚠ ⚠ ★ **而它只威胁 PASS，永远不威胁 FAIL** —— 这一条是 `G146` 与原先那三条
#   候选的分水岭，也是它为什么排在 `bench_verdict_one` **之后**：
#
#     门槛 = max(竞品) × 0.9。若那个 max 被天花板压住了，**门槛就是被低估的**。
#     · 结论是 PASS ⇒ 枢衡可能只是压过了一个被低估的门槛 ⇒ **不可信，作废**。
#     · 结论是 FAIL ⇒ 真实门槛只会**更高** ⇒ FAIL **更成立** ⇒ ⛔ 不该作废。
#
#   ★ `G142` 那条「两边都往作废侧倒」的初衷是**不给出错结论**，而天花板之下的
#   FAIL **不可能是错的** ⇒ 这不是放松，是把那条初衷贯彻到底。
#   ⛔ 三条原候选（最快 k 家 / 每一对 / 极差外加一条）都会把 09-06 那个结实的
#   FAIL 扣掉，而扣掉一个正确结论并不比给出一个错结论便宜。
#
# ⚠ ⚠ ⛔ **排在后面 ≠ 可以先把 PASS 打出来。** `verdict.sh` 内部算完再决定写什么，
#   一个会被作废的 PASS **一个字都不许落进 `verdict.txt` 或标准输出** ——
#   `G142` 当初把 B 排在前面，理由正是「先打出来的那个 PASS 已经会被人引用了」。
#
# ⚠ 喂的是**全场**读数（含枢衡），⛔ 不是竞品集合：枢衡自己与最强者一起顶在
#   天花板上时，那个 PASS 同样不可信。
bench_top_convergence() {
  local readings=$1
  local stats cnt m1 m2 n1 n2 gap

  stats=$(printf '%s\n' "$readings" | awk '
    NF >= 2 && $2 + 0 == $2 {
      n++
      v = $2 + 0
      if (n == 1) { m1 = v; n1 = $1 }
      else if (v > m1) { m2 = m1; n2 = n1; m1 = v; n1 = $1 }
      else if (n == 2 || v > m2) { m2 = v; n2 = $1 }
    }
    END {
      printf "%d %s %s %s %s\n", n + 0, (n ? m1 : "-"), (n >= 2 ? m2 : "-"), (n ? n1 : "-"), (n >= 2 ? n2 : "-")
    }
  ')
  read -r cnt m1 m2 n1 n2 <<< "$stats"

  # 「判不了」一律作废，⛔ 不算「分得开」——同判据 ① 那句「『没能检查』不算『检查通过』」。
  if [ "$cnt" -lt 2 ]; then
    echo "top-convergence: 只有 ${cnt} 条有效读数，判不了最强者附近的收敛（至少要 2 条）"
    return 0
  fi
  if ! awk -v m="$m1" 'BEGIN { exit !(m + 0 > 0) }'; then
    echo "top-convergence: 最强者读数是 '${m1}'，不是正数 ⇒ 判不了"
    return 0
  fi

  gap=$(awk -v a="$m1" -v b="$m2" 'BEGIN { printf "%.4f", (a - b) / a }')
  # ★ 边界与其余判据取同一个约定：**恰好压在阈值上算分得开**（不作废）。
  if awk -v g="$gap" -v t="$BENCH_MIN_SPREAD" 'BEGIN { exit !(g < t) }'; then
    echo "top-convergence: 最强者 ${n1}=${m1} 与第二强 ${n2}=${m2} 只差 ${gap}（阈值 ${BENCH_MIN_SPREAD}）⇒ 这个「最强者」可能被某个外部上限压住了，⇒ 由它算出的门槛是**被低估的**，⛔ 压过它的 PASS 不可信"
  fi
}

# ── 判据 ⑤：上游连接复用口径对齐 ───────────────────────────────────────────
#
#   bench_upstream_reuse_violations <阈值> <每行 `<名字> <每千请求新建连接数>`>
#
# 每行打一条「这一家没在复用上游连接」的理由；**一行都不打 = 对齐了**。
# ★ 契约与判据 ① / ④ 逐字相同（打理由 / 空 = 通过）⇒ 调用点可以同一个形状处理。
#
# ★ ★ ★ **它守的失效不会让任何东西变红。** 四家对上游连接的**默认**行为不同
#   —— 镜像按 digest 钉的 nginx 1.29.1 每请求新建一条（upstream keepalive 是
#   1.29.7 起才默认开的），caddy 走 Go 的 transport 只留少量空闲连接 ——
#   而 **oha 的 JSON 里看不出这件事**：成功率、状态码、错误分布全都正常，
#   只是那一家的 rps 被系统性压低。⇒ 判据 ①–④ 一条都答不了它。
#   ⚠ 而 nginx 在静态吞吐里是**第二强** ⇒ 它被低估会直接污染「该类最强者」与判据 ④C。
#
# ⚠ ⚠ **「判不了」一律算违规，⛔ 不算通过**（同判据 ① 那句「『没能检查』不算
#   『检查通过』」）：读数不是数字（含 `NaN`）、或那一行根本没有第二列时，打违规。
# ★ 边界与判据 ①/④ 取同一个约定：**恰好压在阈值上算好的那一侧** ⇒ 严格不等号。
bench_upstream_reuse_violations() {
  local limit=$1 readings=$2
  printf '%s\n' "$readings" | awk -v lim="$limit" '
    NF == 0 { next }
    NF < 2 {
      printf "%s：这一行读不出「每千请求新建连接数」（原文 %s）—— 「判不了」不算「判过了」\n", $1, $0
      next
    }
    $2 + 0 != $2 {
      printf "%s：每千请求新建连接数不是数字（%s）—— 「判不了」不算「判过了」\n", $1, $2
      next
    }
    $2 + 0 > lim + 0 {
      printf "%s：每千请求新建了 %s 条上游连接（上限 %s）⇒ 它没在复用上游连接，这一家的读数被系统性压低\n", $1, $2, lim
    }
  '
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

  # —— 内核参数（G145）——
  #
  # ★ 先钉「声明本身不是空的」：`bench_kparam_mismatches` 拿一份空声明会**恒返回空集**，
  #   而那与「逐条相符」的输出一模一样 ⇒ 声明一旦被谁清空，下面每一条都会照常绿。
  local DECL OBS
  DECL=$(bench_container_sysctls)
  want_eq "容器侧声明的条数不对（空声明会让整格判据恒绿）" 4 "$(printf '%s\n' "$DECL" | grep -c .)"
  want_eq "宿主侧声明的条数不对" 1 "$(bench_host_sysctls | grep -c .)"

  # 第六个参数：传进不符的比较结果就必须红，传空就不该红。
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF" "net.core.somaxconn 实测 '4096' ≠ 声明 '65535'")
  want_match "内核参数不符没被判成不合格" '*kernel-params:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF" "")
  want_empty "内核参数逐条相符时被判成了不合格" "$out"

  # 比较本身：逐条相符 ⇒ 一行都不打。
  OBS=$(printf '%s\n' 'net.core.somaxconn=65535' 'net.ipv4.tcp_max_syn_backlog=65535' \
    'net.ipv4.ip_local_port_range=10240 65535' 'net.ipv4.tcp_tw_reuse=2')
  want_empty "逐条相符却打出了问题" "$(bench_kparam_mismatches "$DECL" "$OBS")"

  # ★ ★ 承重：`sysctl` 打多值用 **tab**，声明里写的是空格 ⇒ 少了归一化，
  #   一次完全正确的固化会被判成不符，而那种门最后会被人绕过去而不是被人满足。
  want_empty "tab 与空格的差别被当成了不符（归一化那一步失效了）" \
    "$(bench_kparam_mismatches 'net.ipv4.ip_local_port_range=10240 65535' "$(printf 'net.ipv4.ip_local_port_range=10240\t65535')")"

  # 值不等 ⇒ 逐字点名那个键。
  want_match "值不等没被判出来" '*somaxconn*' \
    "$(bench_kparam_mismatches 'net.core.somaxconn=65535' 'net.core.somaxconn=4096')"
  # ⚠ 只点该点的那一个：另一条相符的键不许被顺带报出来。
  want_eq "值不等时报出的条数不对（相符的那条被顺带报了）" 1 \
    "$(bench_kparam_mismatches "$(printf 'net.core.somaxconn=65535\nnet.ipv4.tcp_tw_reuse=2\n')" \
       "$(printf 'net.core.somaxconn=4096\nnet.ipv4.tcp_tw_reuse=2\n')" | grep -c .)"

  # 「读不到」必须判红，⛔ 不许当成通过 —— 一个查不到就返回空串的实现，会把
  # 「这个键不存在」和「这个键的值是空」混成同一件事。
  want_match "键在实测里读不到却被当成了相符" '*读不到*' \
    "$(bench_kparam_mismatches 'net.core.somaxconn=65535' 'net.ipv4.tcp_tw_reuse=2')"
  want_eq "实测整份为空时没有逐条报出来" 4 \
    "$(bench_kparam_mismatches "$DECL" "" | grep -c .)"

  # ★ ★ ★ 空声明：⛔ 不许静默通过。
  want_match "空声明被当成了「全部相符」" '*声明为空*' "$(bench_kparam_mismatches "" "$OBS")"
  want_match "只有空白的声明被当成了「全部相符」" '*声明为空*' "$(bench_kparam_mismatches "$(printf '  \n\n')" "$OBS")"

  # —— docker 旗标：从声明推导出来的那一份 ——
  want_eq "docker 旗标行数不对（4 个 --sysctl + 1 个 --ulimit ⇒ 10 行）" 10 \
    "$(bench_docker_sysctl_flags | grep -c .)"
  # ★ ★ 承重：`10240 65535` 自己带空格，它必须是**一整行一个 token**。
  #   ⛔ 被劈成两个参数时 docker 只报一句语焉不详的用法错，而不会提到那个空格。
  want_eq "带空格的那个值被劈开了" 1 \
    "$(bench_docker_sysctl_flags | grep -cF 'net.ipv4.ip_local_port_range=10240 65535')"
  want_eq "nofile 旗标没跟着 BENCH_NOFILE 走" 1 \
    "$(bench_docker_sysctl_flags | grep -cF "nofile=${BENCH_NOFILE}:${BENCH_NOFILE}")"

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

  # —— 判据 ④C：最强者附近的收敛（G146，结案 D34）——
  #
  # ★ ★ ★ 这一组的**承重对照**是第一条：2026-09-06 那组**真实数据**喂给 B 时
  #   它一声没吭（全场极差 0.8599），而喂给 C 时它响 ——
  #   ⇒ 证明 C 抓的正是 B **结构上**抓不到的那件事。
  #   ⛔ 少了这条对照，「C 判得动」与「C 在重复 B 已经会做的事」分不开。
  local REAL_0906
  REAL_0906=$(printf 'haproxy 54531.2335\nnginx 54144.1168\ncaddy 10834.7315\nfulcrum 7638.6264\n')
  want_empty "2026-09-06 那组真实数据被 B 判成了收敛（它的全场极差是 0.8599）" \
    "$(bench_saturation "" "$REAL_0906")"
  want_match "同一组真实数据里最快两家只差 0.0071，C 却没判出来" '*top-convergence:*' \
    "$(bench_top_convergence "$REAL_0906")"

  # 分得开 ⇒ 一行都不打。
  want_empty "最强者与第二强差 0.30 却被判成了收敛" \
    "$(bench_top_convergence "$(printf 'a 100\nb 70\nc 50\n')")"
  # ⚠ 边界：恰好等于阈值算**分得开**（与其余判据同一个约定）。
  want_empty "恰好压在阈值上被判成了收敛" \
    "$(bench_top_convergence "$(printf 'a 100\nb 95\n')")"
  # ★ 刚跨过去那一侧必须响 —— 少了这条，上一条与「恒不响」分不开。
  want_match "刚跨进阈值内却没判出来" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 100\nb 96\n')")"
  # ⚠ 它看的是**最强者与第二强**，⛔ 不是最强与最弱：下面这组极差 0.9 而顶部收敛。
  want_match "顶部收敛被底部的离散掩盖了（这正是 D34 那个缺陷）" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 100\nb 99\nc 10\n')")"
  # 「判不了」一律作废。
  want_match "只有一条读数时被当成了分得开" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 100\n')")"
  want_match "读数全不是数字时被当成了分得开" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a x\nb y\n')")"
  want_match "最强者不是正数时被当成了分得开" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 0\nb 0\n')")"

  # —— 判据 ⑤：上游连接复用（**口径对齐**，⛔ 不是性能门槛）——
  #
  # ★ ★ 合成输入用的是**真实实测值**（2026-09-09 `ShanirePCX`），⛔ 不是编的
  #   —— 与上面那组 `REAL_0906` 同一个做法。
  local REUSE_ALIGNED REUSE_NGINX_OFF
  REUSE_ALIGNED=$(printf 'fulcrum 3.20\ncaddy 3.16\nhaproxy 2.40\nnginx 2.39\n')
  REUSE_NGINX_OFF=$(printf 'fulcrum 2.92\ncaddy 4.12\nhaproxy 1.72\nnginx 1005.65\n')
  want_empty "四家对齐后的真实读数被判成了违规 —— 那道判据恒红" \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$REUSE_ALIGNED")"
  want_match "nginx 每千请求新建 1005.65 条却没被判成违规 —— 那道判据是空操作" '*nginx*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$REUSE_NGINX_OFF")"

  # ⚠ ⚠ ★ ★ ★ **已知的能力边界，钉在这里免得被误读。**
  #   上面那组 `REUSE_NGINX_OFF` 里 caddy 的 `transport` 块**也被撤掉了**，
  #   而它的读数只有 4.12 ⇒ **判不出来**。「部分不复用」那一族的表现**依赖并发数**
  #   （门禁只跑 10 条连接），⇒ 这道判据在门禁参数下只抓得住「每请求新建」那一族。
  #   ⛔ 别据此把阈值压到 4 附近：门禁 2s×10 连接下若 rps 掉到 500，
  #   oha 自己那 10 条就是 10/千 —— 那会变成一道随机翻面的门。
  want_empty "门禁参数下的 caddy 4.12 被判成了违规（这条红了说明阈值紧到会误伤）" \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy 4.12\n')")"
  # ★ 而窗口参数（连接数大一个量级）下同一处缺陷会暴露成 100.3 ⇒ 那时必须抓住。
  #   ⇒ **同一道判据在两种参数下各抓一族**，而窗口那一趟的读数会进原始数据。
  want_match "窗口参数下 caddy 未对齐的 100.30 没被判成违规" '*caddy*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy 100.30\n')")"

  # 「判不了」一律算违规（同判据 ①/④ 那句「『没能检查』不算『检查通过』」）。
  want_match "NaN 被当成了通过" '*caddy*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy NaN\n')")"
  want_match "少一列的行被当成了通过" '*caddy*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy\n')")"
  # ⚠ 边界：恰好压在阈值上算**好**的那一侧（与其余判据同一个约定）。
  want_empty "恰好压在阈值上被判成了违规" \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'a %s\n' "$BENCH_UPSTREAM_REUSE_MAX")")"

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
