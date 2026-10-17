#!/usr/bin/env bash
# 對拍的**判據本體**（M3 第一刀，G132）。
#
# ★ ★ ★ 本檔案裡的每一個判據都寫成**純函式**：讀數由呼叫方作參數傳進來，
#   函式自己不去問宿主。⛔ 這不是風格潔癖，是判據能不能被反證的分水嶺 ——
#   一個直接讀 `/proc` 的判據，只能在**它恰好跑著的那臺機器上**被觀察到一種結果，
#   而那正是「只被觀察過綠的門與不存在的門無法區分」那句話的形狀。
#   ⇒ 參數化之後，兩個方向都能用**合成輸入**當場證給人看（見檔案末尾的自測）。
#
# 被 `bench/env-snapshot.sh`（採集 → 判合格性）與 `bench/verdict.sh`（判定）共用。
#
#   bash bench/lib.sh --self-check     # 只跑自測，不需要 docker、不需要任何被測物件
#
# ⚠ 本檔案**不產出任何效能數字**，也不讀原始資料以外的東西。

set -euo pipefail

# ── 合格宿主的閾值 ─────────────────────────────────────────────────────────
#
# ★ 它們是**判據的一部分**，不是偵錯旋鈕 —— 改動等於改口徑，要連 `bench/README.md`
#   那張表一起改（`tests/bench/run.sh` 有一道門把兩處對上）。
BENCH_MIN_CPUS=${BENCH_MIN_CPUS:-4}
BENCH_MAX_IDLE_LOAD=${BENCH_MAX_IDLE_LOAD:-0.50}

# ── 壓測端飽和的兩個閾值（判據 ④）─────────────────────────────────────────
#
# ⚠ ⚠ ★ **這兩個數仍然不是實測得出的閾值。** 2026-09-06 第一次在合格宿主上跑完之後
#   已經回核過一輪，逐條結論在 `bench/README.md`「這兩個閾值的回核」一節：
#
#   · `0.05` 那一趟**沒有誤報**（全場 spread = 0.8599，遠高於它）。
#     ⚠ ⚠ ★ **但同一組數暴露了 B 的一個真實缺陷**：最快兩家（haproxy / nginx）只差
#     **0.0071**，正是 B 聲稱要懷疑的形狀，而 B 一聲沒吭 —— 因為它取的是**全場極差**，
#     被樞衡的慢整個撐開了。⇒ **一個慢的被測會掩蓋掉快的那幾家之間的收斂。**
#     ✅ 已由 **`G146`** 結案（原 `D34`）：新增判據 **④C**（`bench_top_convergence`），
#     判「最強者 vs 第二強」，**只作用於 PASS**。⛔ 但 `0.05` 這個數**本身**
#     仍然沒有實測支撐 —— G146 改的是「拿它去比什麼」，不是「它該取多少」。
#   · `0.90` **一次都沒被執行過**（那一趟 A 沒打開，沒有 ceiling.txt）⇒ 仍然驗不了。
#
# ★ 取 0.05 的理由寫在判據 ④ 的註解裡（四套結構完全不同的實作擠進 5% 更像是
#   共享了外部上限）—— 是一條可以被真實資料推翻的假設，⛔ 不是結論。
BENCH_MIN_SPREAD=${BENCH_MIN_SPREAD:-0.05}
BENCH_GEN_HEADROOM=${BENCH_GEN_HEADROOM:-0.90}

# ── 判據 ⑤ 的閾值：每千請求新建多少條上游連接才算「沒在重複使用」──────────────────
#
# ★ ★ **與上面那兩個不同，這一個有實測支撐**（2026-09-09 `ShanirePCX`，
#   全部在**門禁那組參數**下量的：`BENCH_GATE_DURATION=2s` / `..._CONNECTIONS=10`）：
#
#   · 對齊口徑配好，**三趟**：3.20/3.16/2.40/2.39 · 2.92/—/1.72/— · 0.84/1.31/0.68/1.39
#     ⇒ ★ ★ **同一形態波動約 5 倍**（0.68–3.20），⛔ 這不是噪聲可以忽略的量級。
#   · 把 nginx 那三行撤掉 ⇒ nginx **1005.65**（每請求恰好新建一條）
#   · 另在 10s/50 連接下量過：把 caddy 的 `transport` 塊撤掉 ⇒ **100.30**
#
# ⇒ 取 50：離對齊那側的**最大觀測值** 3.20 有 15 倍餘量，離兩種未對齊形態
#   分別有 20 倍與 2 倍。
#
# ⛔ **不取 10**：① 對齊那側實測已經到過 3.20，而波動是 5 倍量級 ⇒ 餘量只剩 3 倍；
#   ② 門禁跑 2s × 10 連接，若某趟 rps 掉到 500，oha 自己那 10 條連接就是 10/千。
#   兩條都指向同一件事 —— 那會變成一道**隨機翻面**的門。
#   ⚠ 這個觀測量是**整個 netns 的**累計值，本來就含 oha 那一份
#   （見 `bench/case/reverse-proxy-throughput.sh` 裡 `passive_opens` 那段）。
#
# ⚠ ⚠ ★ **它的能力邊界寫在自測裡**（`bench_self_check` 判據 ⑤ 那一段）：
#   門禁參數下它只抓得住「每請求新建」那一族；「部分不復用」那一族的表現相依
#   並行數，要到視窗參數（連接數大一個量級）才暴露。⛔ 別把它讀成「四家都驗過了」。
BENCH_UPSTREAM_REUSE_MAX=${BENCH_UPSTREAM_REUSE_MAX:-50}

# ── 判據 ⑥ 的閾值：「高並行短連接」那一類真的是每請求一條新連接 ─────────────────
#
# ★ **口徑自證**，⛔ 不是效能門檻（與判據 ⑤ 同性質）。觀測量與判法見
#   `bench_short_conn_violations` 的註解。
#
#   **新建下限 990**：對的時候每個完成的請求**恰好**對應一次被動建連，再加上 deadline
#   砍掉的那幾條 ⇒ **建構上 ≥ 1000**；2026-09-23 `ShanirePCX` 容器裡門禁參數（2s × 10）
#   實測四家 1000.15–1000.51。兩個旗標都去掉時 ≈ 0.x。⇒ 取 1000 × 0.99，那 1% 只留給
#   「判不出的怪事」，⛔ 不是給「部分重複使用」留的口子（帶著 `Connection: close` 時那一族不存在）。
# ⚠ 「伺服器端先關」那一半**沒有閾值**：它判的是 oha 發出的請求裡有沒有 `Connection: close`
#   （`bench_request_close_violations`），⛔ 不是 TIME_WAIT 佔比 —— 後者實測是競態、
#   而且門禁參數下會被 netns 的 TIME_WAIT 上限截斷，見那個函式的註解。
BENCH_SHORT_NEW_CONNS_MIN=${BENCH_SHORT_NEW_CONNS_MIN:-990}

# ── 負載參數：**預設只在這裡定義一次**（2026-09-06）───────────────────────────
#
# ⚠ ⚠ ★ ★ ★ **這四行為什麼必須在這個檔案裡，而不是在案例指令碼裡。**
#
# 在它們搬過來之前，預設寫在 `bench/case/static-throughput.sh`（`:-4096` 那一族），
# 而 `bench/env-snapshot.sh` 記的是**宣告值** `${BENCH_PAYLOAD_BYTES:-}`
# ⇒ 沒人顯式設的那些參數，**案例真的用了預設，而快照寫 `null`**。
# 2026-09-06 那組讀數就帶著這個缺陷：`payload_bytes: null`，而那一趟真的用了 4096。
# ★ 門禁裡同時露出的還有 `workers: null` ⇒ ⛔ **別把它記成「payload 的毛病」**，
#   四個參數用的是同一個寫法，只是那一趟 owner 恰好顯式設了另外三個。
#
# ★ ★ G19 要的是**原始資料可被第三方重現**，而一份說不出自己用了多大 payload、
#   幾個 worker 的靜態吞吐讀數**重現不出來** ⇒ 這不是中繼資料，是口徑本身。
#
# ⛔ **別把預設「順手也寫進 `env-snapshot.sh` 一份」** —— 那才是更壞的形態：
#   兩處預設一旦飄掉，快照會**理直氣壯地報一個錯的數**，而 `null` 至少不騙人。
#   ⇒ 一處定義、兩邊都 `source` 本檔案，於是這一族缺陷**結構性地不存在**。
# ★ 與上面那幾個閾值同一個寫法（`${X:-缺省}`）⇒ 宿主側設了就一路帶進容器
#   （`bench/docker-run.sh` 的 `-e BENCH_*`），沒設則兩邊各自算出**同一個**預設。
#
# ⚠ 這四個值本身是**口徑**，改它們要連 `bench/README.md` 一起改。
#   ⛔ 真要出數那天必須顯式設 `BENCH_WORKERS`（理由見案例指令碼頭部第 ⑤ 條）。
BENCH_DURATION=${BENCH_DURATION:-10s}
BENCH_CONNECTIONS=${BENCH_CONNECTIONS:-50}
BENCH_WORKERS=${BENCH_WORKERS:-1}
BENCH_PAYLOAD_BYTES=${BENCH_PAYLOAD_BYTES:-4096}

# ── 判據 ①：宿主合格性 ─────────────────────────────────────────────────────
#
#   bench_disqualifiers <kernel_release> <nproc> <loadavg1> <attest> <affinity> [kparam_mismatch]
#
# 每行打一條「不合格的理由」；**一行都不打 = 合格**。
#
# ★ ★ 六條分成性質完全不同的三半，⛔ 別把它們讀成一張清單：
#
#   四條（kernel / cpus / load / affinity）是**容器自己看得見**的，機器判。
#
#   一條（kernel-params）是 **G145 從 attest 裡拆出來的** —— 它此前是那句宣告的第三件，
#   而實測證明「已固化」這句話可以完全誠實、同時對被測行為零影響（per-netns，
#   傳不進容器）。⇒ 現在它由真實比較喂進來，比較本身在 `bench_kparam_mismatches`。
#   ★ 本參數**有預設值** ⇒ G145 之前寫的每一條呼叫都還能跑，⛔ 但那意味著漏傳時
#     它安靜地不判 —— 所以 `env-snapshot.sh` 那唯一一處真實呼叫必須傳滿六個參數，
#     而自測裡專門有一條釘住「傳了不符的比較結果就必須紅」。
#
#   一條（attest）是**容器原理上看不見、也沒有任何門看得見**的 ——
#   「這臺機器沒在承載真業務」與「網路路徑上沒有 TUN 代理」這兩件，
#   在容器裡問不出來（netns 是容器自己的）。
#   ⇒ 它要求一句**人寫下來的**宣告。⚠ 宣告不是證明，`README.md` 把這一格
#     該核什麼逐條寫明；這裡能做到的只有「誰都沒宣告過就一定不算合格」。
#
# ⚠ ⚠ **`kernel` 那一條不是在嫌棄 Windows**：G132 記著的實測理由是這臺開發機上
#   那個 TUN 代理會干擾網路（容器出不去 UDP/443），而 Docker Desktop 的 Linux 側
#   跑在 WSL2 裡 ⇒ 核心串是本輪**唯一**一條不需要人配合就判得死的證據。
bench_disqualifiers() {
  local kernel=$1 cpus=$2 load1=$3 attest=$4 affinity=${5:-} kparam_mismatch=${6:-}

  # WSL2 / Docker Desktop：核心串裡帶 microsoft 或 WSL。
  # ⚠ 大小寫兩種都見過（`5.15.0-microsoft-standard-WSL2`）⇒ 先折成小寫再比，
  #   ⛔ 別寫兩條 case 分支去枚舉，枚舉漏一種時它安靜地判成合格。
  case "$(printf '%s' "$kernel" | tr '[:upper:]' '[:lower:]')" in
    *microsoft* | *wsl*)
      echo "kernel: 内核串是 '${kernel}' ⇒ 这是 WSL2 / Docker Desktop，不是专用对拍宿主"
      ;;
  esac

  # 核數：負載生成器與被測**不許共享 CPU**（§9 的固化項之一是 CPU 親和）。
  # ⚠ 判據寫成「分得出兩組」而不是某個具體機型，⇒ 它不隨機器換代過期。
  if ! printf '%s' "$cpus" | grep -qE '^[0-9]+$'; then
    echo "cpus: 核数读不出来（读到的是 '${cpus}'）——「没能检查」不算「检查通过」"
  elif [ "$cpus" -lt "$BENCH_MIN_CPUS" ]; then
    echo "cpus: 只有 ${cpus} 个核，少于 ${BENCH_MIN_CPUS} ⇒ 负载生成器与被测分不开"
  fi

  # 空載：機器在幹別的活，就等於被測在與別人共享 CPU（G132 點名的第二條）。
  # ★ 用 awk 比浮點：bash 的 `[ ]` 只會整數比較，`0.9 -gt 0.5` 是語法錯而不是 false。
  if ! printf '%s' "$load1" | grep -qE '^[0-9]+([.][0-9]+)?$'; then
    echo "load: 1 分钟负载读不出来（读到的是 '${load1}'）——「没能检查」不算「检查通过」"
  elif awk -v a="$load1" -v b="$BENCH_MAX_IDLE_LOAD" 'BEGIN { exit !(a > b) }'; then
    echo "load: 1 分钟负载 ${load1} 高于 ${BENCH_MAX_IDLE_LOAD} ⇒ 这台机器不空闲"
  fi

  # CPU 親和：§9 的緩解項逐字寫著「基準環境用指令碼固化，含**核心參數與 CPU 親和**」。
  # ★ ★ 它判的是「有沒有把被測與負載生成器釘到不相交的核上」，⛔ 不是「釘得對不對」——
  #   釘得對不對要看具體機器的拓撲，那臺機器今天還不存在。
  # ⚠ 這一條**容器自己看得見**（就是兩個環境變數在不在），所以歸機器判那一半；
  #   ⛔ 別把它挪進 attest 那一句裡 —— 那等於把一件能自動判的事降格成一句宣告。
  if [ -z "$affinity" ]; then
    echo "affinity: 没有设置 BENCH_SERVER_CPUS / BENCH_LOAD_CPUS ⇒ 被测与负载生成器抢同一批核"
  fi

  # 人寫下來的那兩件（容器看不見，**也沒有任何門看得見**）。
  # ★ ★ G145 之前這裡是三件，第三件是「核心參數已固化」。它現在**不在這句宣告裡了** ——
  #   六個鍵各自有了門（四個容器側 + 一個宿主側 + nofile），見本檔案 `bench_container_sysctls`
  #   的註解。⛔ 別把它加回來：一件既有門又要人宣告的事，會讓人以為宣告是那道門。
  if [ -z "$attest" ]; then
    echo "attest: 没有任何人声明过「专机 + 无 TUN 代理」（见 bench/README.md）"
  fi

  # 核心參數（G145）。★ 比較本身在 `bench_kparam_mismatches` 裡，本函式只把它的輸出
  #   轉成不合格理由 —— ⇒ 「怎麼比」與「比出來算不算不合格」各自可被單獨反證。
  # ⚠ 逐行加前綴，⛔ 不是 `printf '前缀 %s\n' "$整串"` —— 後者在多條不符時只給
  #   第一行加前綴，而本函式對外的契約是「**每行**一條不合格理由」。
  if [ -n "$kparam_mismatch" ]; then
    printf '%s\n' "$kparam_mismatch" | sed 's/^/kernel-params: /'
  fi
}

# ── 判據 ①bis：核心參數真的按宣告生效了嗎（G145）─────────────────────────────
#
# ★ ★ ★ **為什麼這一格必須存在**：G145 之前，「核心參數已固化」是 `attest` 那句話裡的
#   第三件，⇒ 它沒有門。而 2026-09-06 的實測把問題挖得更深 —— 就算真的在宿主上
#   `sysctl --system` 了，**四個鍵裡沒有一個傳得進對拍容器**：
#
#     宿主 net.ipv4.ip_local_port_range = 20000 60000 ⇒ 新開的預設網路容器仍讀到 32768 60999
#     宿主 net.core.somaxconn           = 12345       ⇒ 新開的預設網路容器仍讀到 4096
#
#   它們是 **per-netns** 的，而對拍整個跑在自己的 netns 裡。
#   ⇒ 一份「已固化」的宣告可以完全誠實，同時對被測行為**零影響**。
#   ⚠ 這正是本儲存庫最在意的那一族：一道**恆綠**的門與一道不存在的門，輸出完全一樣。
#
# ⇒ G145 的落法：**四個 per-netns 鍵改由 `docker run --sysctl` 在容器裡設**，
#   而本函式在容器內斷言「實測值 == 宣告值」。⛔ 宣告與實測分家時判紅。

# 容器側那四個鍵 —— ★ ★ **唯一**那份宣告。
#   `bench/docker-run.sh` 用它拼 `--sysctl` 旗標，`bench/env-snapshot.sh` 用它當期望值。
#   ⛔ 別在任何別處再抄一份：兩份一旦分家，容器會按 A 跑而快照按 B 判，**兩邊都不紅**。
#
# ⚠ ⚠ `tcp_tw_reuse` 宣告成 **2 而不是 1**（G145 改的，此前是 1）。
#   `2` 的語義是「**僅對環回**啟用」，而對拍的四家與 oha 同處一個容器、只走環回
#   ⇒ 2 恰好覆蓋本場景，而 1（全域啟用）比它更寬卻**一點也不更貼切**。
#   ★ 順帶：2 也正是 Linux 4.12 起的核心預設 ⇒ 這一條宣告的是「預設沒有被改壞」。
bench_container_sysctls() {
  printf '%s\n' \
    'net.core.somaxconn=65535' \
    'net.ipv4.tcp_max_syn_backlog=65535' \
    'net.ipv4.ip_local_port_range=10240 65535' \
    'net.ipv4.tcp_tw_reuse=2'
}

# 宿主側那一條 —— ⛔ 它**設不進也讀不到**容器裡。
#
# ★ 實測（2026-09-06，那臺候選對拍宿主，Linux 6.8）：
#     docker run --sysctl net.core.netdev_max_backlog=16384 …
#       ⇒ runc 當場拒絕：`open sysctl … : no such file or directory`
#     容器內 `cat /proc/sys/net/core/netdev_max_backlog`
#       ⇒ `No such file or directory`（它非 netns 化 ⇒ 不出現在容器的 /proc/sys/net 視圖裡）
#
# ⇒ 它只能在**宿主上**設、在**宿主上**判 —— 那道門在 `bench/docker-run.sh` 裡，
#   而啟動器把宿主實測值經環境變數傳進容器，本檔案據此判「啟動器到底跑沒跑過」。
#
# ★ 為什麼單獨留它而不是一併丟掉：**環回包也走 per-CPU `input_pkt_queue`**，
#   正是這個鍵管的佇列。那臺機器的出廠值是 **1000** ⇒ 它是六個鍵裡**唯一**
#   既有效、又真有可能在對拍裡成為瓶頸的。
bench_host_sysctls() {
  printf '%s\n' 'net.core.netdev_max_backlog=16384'
}

# 容器裡的 `nofile` 軟/硬上限。
# ⚠ ⚠ ★ **實測容器預設只有 1024**（2026-09-06，docker 29.1.3）—— 而 `fs.file-max`
#   在那臺機器上是 9223372036854775807。⇒ 會咬的從來不是 `fs.file-max`，是這個。
#   ★ G145 之前的 `bench/sysctl.conf` 只管前者，而且把它**從 9.2e18 降到 2097152**。
BENCH_NOFILE=${BENCH_NOFILE:-1048576}

# `docker run` 要加的旗標 —— ★ ★ **從上面那兩份宣告推導，⛔ 不另寫一份清單**。
#   兩個呼叫方（`bench/docker-run.sh` 真跑、`tests/bench/run.sh` 門禁）都用它。
#   ⚠ 旗標與清單一旦分家，容器會按 A 跑而快照按 B 判，**兩邊都不會紅**。
#
# ⚠ **一行一個 token**，⛔ 不是一整行空格分隔的字串：
#   `net.ipv4.ip_local_port_range=10240 65535` 這個值**自己帶空格**，
#   拼成一行再讓呼叫方分詞，會把它劈成兩個參數而 docker 只報一句語焉不詳的用法錯。
#   ⇒ 呼叫方用 `while IFS= read -r` 逐行讀進陣列。
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

# 把任意空白（含 tab）壓成單個空格並去掉首尾。
# ⚠ ⚠ 這一步是承重的：`sysctl` 打多值時用 **tab** 分隔（`10240<TAB>65535`），
#   而宣告裡寫的是空格 ⇒ 不歸一化的話，一次完全正確的固化會被判成不符。
_bench_norm_ws() {
  printf '%s' "$1" | tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//'
}

# 在一份 `key=value` 多行文字裡查一個鍵；查不到回傳非 0（⛔ 不回傳空串當成查到了）。
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
# 每行打一條問題；**一行都不打 = 逐條相符**。
#
# ⚠ 三種「判不了」一律算問題，⛔ 不算通過：
#   ① 宣告本身是空的 —— 一個空宣告會讓本函式**恆回傳空集**，而那與「全部相符」
#      的輸出一模一樣。★ 這是本儲存庫反覆栽的那一族，所以它排在最前面。
#   ② 某個鍵在實測裡讀不到（⚠ 訊息有意不寫「在容器裡」——宿主側那道門也用這個函式） —— 「沒能檢查」不算「檢查通過」。
#   ③ 讀到了但不等於宣告值。
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

# ── 判據 ②：逐類取「該類最強者」 ───────────────────────────────────────────
#
#   bench_best_of <三家竞品的读数，每行 `<名字> <数值>`>
#
# 打一行 `<名字> <数值>`。
#
# ★ ★ G19 的門檻是「不劣於**該類最強者** 10%」，而 §8 明寫「最強者逐類不同 ——
#   靜態那一類比的是 nginx，L4 那一類比的是 HAProxy」。⇒ 這裡**必須逐類現算**，
#   ⛔ 不許把某一家釘成基準，也不許取三家平均。
# ⚠ 本函式只吃**競品**的讀數：把樞衡自己也喂進來會讓它在自己領先時拿自己當門檻，
#   那道門就恆真了。呼叫方負責別喂錯，`bench_verdict_one` 是唯一的呼叫點。
bench_best_of() {
  awk '
    NF >= 2 && $2 + 0 == $2 { if (!seen || $2 + 0 > best + 0) { best = $2; name = $1; seen = 1 } }
    END { if (seen) printf "%s %s\n", name, best }
  '
}

# ── 判據 ③：一類案例的判定 ─────────────────────────────────────────────────
#
#   bench_verdict_one <枢衡的读数> <三家竞品的读数，每行 `<名字> <数值>`>
#
# 打一行 `PASS|FAIL <最强者名> <最强者数值> <门槛值> <枢衡数值>`。
#
# ★ 方向：吞吐類**越大越好** ⇒ 門檻是 `最强者 × 0.9`，樞衡要 `>=` 它。
# ⚠ ⚠ 延遲類是**反過來**的（越小越好）⇒
#   ⛔ 本函式**有意只處理越大越好那一種**，**加第一個方向相反的類時**要顯式加
#   方向參數，而不是讓它去猜。留一個會猜的判據，比留一個只做一半的判據壞得多。
# ⚠ 2026-09-09：第二類（反代吞吐）加進來了，而它仍是 rps 越大越好 ⇒ **沒加參數**。
#   ⛔ 這條原本寫的是「加第二類時」——第二類已經加了卻沒有方向參數，那句話會讓
#   下一個人當成有人偷懶。★ 最可能逼出這個參數的是「TLS 握手」那一類
#   （若它取握手延遲口徑而不是握手速率）。
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

# ── 判據 ④：壓測端自己先飽和 ───────────────────────────────────────────────
#
#   bench_saturation <生成器上限读数或空> <全场读数，每行 `<名字> <数值>`>
#
# 每行打一條「這一類不可信的理由」；**一行都不打 = 沒測到飽和**。
# ★ 契約與判據 ① 逐字相同（打理由 / 空 = 通過）⇒ 兩個呼叫點可以同一個形狀處理。
#
# ★ ★ ★ **它守的失效不會讓任何東西變紅。** 負載生成器自己先到頂時，四家讀數會
#   一起收斂到生成器的天花板，而判據 ③（`ours >= best × 0.9`）**照常打 PASS** ——
#   輸出一組看起來完全正常、其實量的是 oha 的數。⇒ 判據 ① 那五條一條都答不了它：
#   它們判的是「這臺機器合不合格」，而這裡壞掉的是**量具本身到頂了**。
#
# 兩條判據，性質完全不同：
#
#   **B（收斂，承重）** —— 全場讀數彼此擠在一起就作廢該類。★ 它**不需要任何新測量**，
#     用的就是已經採到的那組數 ⇒ 今天就能用合成輸入把兩個方向全反證掉。
#     依據：nginx（C）· HAProxy（C）· Caddy（Go）· 樞衡（Rust/pingora）是四套結構
#     完全不同的實作，它們落進同一個百分點裡，**「共享了某個外部上限」遠比
#     「它們真的一樣快」更可能**。⚠ 它說不出是哪一個上限（生成器 / 環回 / 親和）——
#     ⛔ 別把它的訊息讀成「一定是 oha 飽和了」，它只說「這組數不可比」。
#
#   **A（絕對天花板，可選）** —— 給了生成器上限讀數時，最強者逼近它就作廢該類。
#     ⚠ ⚠ A 的正確性**整個壓在「參照服務真的比四家都快」這一條上**，而那條在合格
#     宿主到位之前**驗不了**：參照選慢了 ⇒ 恆作廢；選的是另一個真實伺服器 ⇒ 量到的
#     是 `min(oha, 那个服务器)`，天花板被低估。⇒ ⛔ **今天它是可選輸入、不是承重件**：
#     上限留空時 A 完全不參與，B 照常判。參照服務選定那天再把它打開。
#
# ⚠ ⚠ 誤判方向是**有意不對稱**的：一次誤作廢只是**扣下一個結論**，一次漏判是
#   **給出一個錯結論**。⇒ 兩條都往「作廢」那側倒，且**「判不了」一律作廢，
#   ⛔ 不算通過**（讀數不足兩條 / 上限不是正數 —— 同判據 ① 那句「『沒能檢查』
#   不算『檢查通過』」）。
#
# ★ 邊界與判據 ① 取同一個約定：**恰好壓在閾值上算好的那一側**（不作廢），
#   ⇒ 兩處都用嚴格不等號，⛔ 別讓門在邊界上隨機翻面。
bench_saturation() {
  local ceiling=$1 readings=$2
  local stats cnt max min max_name min_name spread

  # ★ 數值那一格用與 `bench_best_of` **逐字相同**的判法（`$2 + 0 == $2`）：
  #   兩處對「什麼算一條有效讀數」的口徑必須一致，否則同一組數在判據 ②
  #   與判據 ④ 裡的「全場」不是同一個集合，而那種不一致不會有任何東西說。
  stats=$(printf '%s\n' "$readings" | awk '
    NF >= 2 && $2 + 0 == $2 {
      n++
      if (n == 1 || $2 + 0 > max + 0) { max = $2; max_name = $1 }
      if (n == 1 || $2 + 0 < min + 0) { min = $2; min_name = $1 }
    }
    END { printf "%d %s %s %s %s\n", n + 0, (n ? max : "-"), (n ? min : "-"), (n ? max_name : "-"), (n ? min_name : "-") }
  ')
  read -r cnt max min max_name min_name <<< "$stats"

  # —— B：收斂（承重）——
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

  # —— A：絕對天花板（可選；⛔ 上限留空時整條不參與）——
  [ -n "$ceiling" ] || return 0
  if ! printf '%s' "$ceiling" | grep -qE '^[0-9]+([.][0-9]+)?$' ||
    awk -v c="$ceiling" 'BEGIN { exit !(c + 0 <= 0) }'; then
    echo "ceiling: 生成器上限读数是 '${ceiling}'，不是正数 ——「判不了」不算「没饱和」"
  elif [ "$cnt" -ge 1 ] &&
    awk -v m="$max" -v c="$ceiling" -v h="$BENCH_GEN_HEADROOM" 'BEGIN { exit !(m + 0 > c * h) }'; then
    echo "ceiling: 最强者 ${max_name}=${max} 已越过生成器上限 ${ceiling} 的 ${BENCH_GEN_HEADROOM} ⇒ 量到的可能是 oha 自己的天花板，不是被测的能力"
  fi
}

# ── 判據 ④C：最強者附近的收斂（`G146`，結案 `D34`）──────────────────────────
#
#   bench_top_convergence <全场读数，每行 `<名字> <数值>`>
#
# 每行打一條理由；**一行都不打 = 最強者與第二強分得開**。
#
# ★ ★ ★ **它存在的理由是一組真實資料推翻了 B 的一個前提。** 2026-09-06 第一趟
#   真實對拍：全場極差 **0.8599**（遠高於 `BENCH_MIN_SPREAD`）⇒ B 一聲沒吭；
#   而**最快兩家 haproxy 與 nginx 只差 0.0071** —— 兩套架構差異極大的實作落進
#   0.71% 以內，**那正是 B 聲稱要懷疑的形狀**。
#   ⇒ B 取的是**全場極差**，被最慢那一家（樞衡 7638）整個撐開了
#   ⇒ **一個慢的被測會掩蓋掉快的那幾家之間的收斂。**
#
# ⚠ ⚠ ★ **而它只威脅 PASS，永遠不威脅 FAIL** —— 這一條是 `G146` 與原先那三條
#   候選的分水嶺，也是它為什麼排在 `bench_verdict_one` **之後**：
#
#     門檻 = max(競品) × 0.9。若那個 max 被天花板壓住了，**門檻就是被低估的**。
#     · 結論是 PASS ⇒ 樞衡可能只是壓過了一個被低估的門檻 ⇒ **不可信，作廢**。
#     · 結論是 FAIL ⇒ 真實門檻只會**更高** ⇒ FAIL **更成立** ⇒ ⛔ 不該作廢。
#
#   ★ `G142` 那條「兩邊都往作廢側倒」的初衷是**不給出錯結論**，而天花板之下的
#   FAIL **不可能是錯的** ⇒ 這不是放鬆，是把那條初衷貫徹到底。
#   ⛔ 三條原候選（最快 k 家 / 每一對 / 極差外加一條）都會把 09-06 那個結實的
#   FAIL 扣掉，而扣掉一個正確結論並不比給出一個錯結論便宜。
#
# ⚠ ⚠ ⛔ **排在後面 ≠ 可以先把 PASS 打出來。** `verdict.sh` 內部算完再決定寫什麼，
#   一個會被作廢的 PASS **一個字都不許落進 `verdict.txt` 或標準輸出** ——
#   `G142` 當初把 B 排在前面，理由正是「先打出來的那個 PASS 已經會被人引用了」。
#
# ⚠ 喂的是**全場**讀數（含樞衡），⛔ 不是競品集合：樞衡自己與最強者一起頂在
#   天花板上時，那個 PASS 同樣不可信。
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

  # 「判不了」一律作廢，⛔ 不算「分得開」——同判據 ① 那句「『沒能檢查』不算『檢查通過』」。
  if [ "$cnt" -lt 2 ]; then
    echo "top-convergence: 只有 ${cnt} 条有效读数，判不了最强者附近的收敛（至少要 2 条）"
    return 0
  fi
  if ! awk -v m="$m1" 'BEGIN { exit !(m + 0 > 0) }'; then
    echo "top-convergence: 最强者读数是 '${m1}'，不是正数 ⇒ 判不了"
    return 0
  fi

  gap=$(awk -v a="$m1" -v b="$m2" 'BEGIN { printf "%.4f", (a - b) / a }')
  # ★ 邊界與其餘判據取同一個約定：**恰好壓在閾值上算分得開**（不作廢）。
  if awk -v g="$gap" -v t="$BENCH_MIN_SPREAD" 'BEGIN { exit !(g < t) }'; then
    echo "top-convergence: 最强者 ${n1}=${m1} 与第二强 ${n2}=${m2} 只差 ${gap}（阈值 ${BENCH_MIN_SPREAD}）⇒ 这个「最强者」可能被某个外部上限压住了，⇒ 由它算出的门槛是**被低估的**，⛔ 压过它的 PASS 不可信"
  fi
}

# ── 判據 ⑤：上游連接重複使用口徑對齊 ───────────────────────────────────────────
#
#   bench_upstream_reuse_violations <阈值> <每行 `<名字> <每千请求新建连接数>`>
#
# 每行打一條「這一家沒在重複使用上游連接」的理由；**一行都不打 = 對齊了**。
# ★ 契約與判據 ① / ④ 逐字相同（打理由 / 空 = 通過）⇒ 呼叫點可以同一個形狀處理。
#
# ★ ★ ★ **它守的失效不會讓任何東西變紅。** 四家對上游連接的**預設**行為不同
#   —— 鏡像按 digest 釘的 nginx 1.29.1 每請求新建一條（upstream keepalive 是
#   1.29.7 起才預設開的），caddy 走 Go 的 transport 只留少量空閒連接 ——
#   而 **oha 的 JSON 裡看不出這件事**：成功率、狀態碼、錯誤分佈全都正常，
#   只是那一家的 rps 被系統性壓低。⇒ 判據 ①–④ 一條都答不了它。
#   ⚠ 而 nginx 在靜態吞吐裡是**第二強** ⇒ 它被低估會直接汙染「該類最強者」與判據 ④C。
#
# ⚠ ⚠ **「判不了」一律算違規，⛔ 不算通過**（同判據 ① 那句「『沒能檢查』不算
#   『檢查通過』」）：讀數不是數字（含 `NaN`）、或那一行根本沒有第二列時，打違規。
# ★ 邊界與判據 ①/④ 取同一個約定：**恰好壓在閾值上算好的那一側** ⇒ 嚴格不等號。
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

# ── 上游空閒連接上限：跟著並行數走（2026-09-10）──────────────────────────────
#
#   bench_upstream_idle_cap <并发连接数>   ⇒ 列印 max(64, 並行連接數)
#
# ★ ★ ★ caddy 的 `keepalive_idle_conns_per_host` 與 nginx upstream 的 `keepalive`
#   都是「**每個 host / 每個 worker** 最多留幾條**空閒**上游連接」—— 超出的在用完之後
#   被關掉（nginx 官方文件：按 LRU 淘汰）。並行一旦大過它，多出來的連接每一輪都要新建
#   ⇒ 「部分不復用」，而 oha 的 JSON 裡看不出來，只是那一家的 rps 被壓低。
#   2026-09-10 開發機 200 並行、單變數實測（上限 64 → 256，haproxy 作對照不動）：
#   caddy 179.32 → 1.52、nginx 130.16 → 1.35 條/千；rps +16.7% / +20.7%。
#   視窗二（合格宿主）裡同一處缺陷報成 caddy 69.62、nginx 21.26。
# ⇒ 上限**跟著並行數**繪製（`bench/case/reverse-proxy-throughput.sh` 的 `render()`），
#   「上限沒跟上並行」這一族就結構性地不存在，⛔ 不靠人記得去改一個寫死的數。
# ★ 下限 64：門禁只跑 10 條連接，繪製出來仍是原來那個 64 ⇒ 門禁行為一字不變。
# ⚠ nginx 按 worker 算：給**每個** worker 都留夠並行數 ⇒ 連接全落在一個 worker 上也夠；
#   多出來的只是空閒槽位，不改變被測的工作量。
# ⛔ 讀不出正整數就當場非零退出 —— 一個安靜回落到某個數的上限，與寫死沒有區別。
bench_upstream_idle_cap() {
  local c=${1:-}
  case "$c" in
    '' | *[!0-9]*)
      echo "bench_upstream_idle_cap：并发连接数不是正整数（'$c'）" >&2
      return 1
      ;;
  esac
  if [ "$c" -lt 1 ]; then
    echo "bench_upstream_idle_cap：并发连接数不是正整数（'$c'）" >&2
    return 1
  fi
  if [ "$c" -gt 64 ]; then echo "$c"; else echo 64; fi
}

# ── 建置身分：從產物位元組裡取 `FULCRUM_BUILD_VERSION`（`G141`）─────────────────
#
#   bench_build_id_from_bytes < <产物>   ⇒ 列印建置身分；取不到就什麼都不打（恆回傳 0）
#
# ★ 只認 `crates/fulcrum-server/build.rs` 真會產出的兩種形狀：
#   `<x.y.z>+g<12 位小写十六进制>`（讀 `.git` 那一檔）與 `<x.y.z>+unknown`（第三檔）。
# ⚠ ⚠ ★ **收尾邊界是承重的**：Rust 的字串字面值在產物裡**首尾相接、不帶 NUL**，
#   身分串後面緊跟著別的字面值 ⇒ 一條「後面還能接字母數字就一直吃」的正則會把鄰居吃進來。
#   2026-09-10 視窗二的 `env.json` 正是這樣：`0.1.0+g6bf1f3fe0618` 後面粘上了
#   `HeadersResetPriority…`。⇒ 兩種形狀都以**定長**或**定字面值**收尾，鄰居是什麼都吃不進來。
# ⛔ 第一檔（`FULCRUM_BUILD_ID` 環境變數喂進來的任意串）在位元組裡**原理上劃不出邊界**
#   ⇒ 那種產物這裡取不到、列印空串 —— ★ 空串不騙人，粘了鄰居的串會。
# ⚠ **開頭那一側同理劃不出**：前一個字面值若以數字結尾，會被當成主版本號的一部分吃進來。
#   今天沒觀察到過；⛔ 身分的權威本來就是 sha256（`env-snapshot.sh` 那一段註解），不是這一格。
bench_build_id_from_bytes() {
  { grep -aoE '[0-9]+\.[0-9]+\.[0-9]+\+(g[0-9a-f]{12}|unknown)' || true; } | sort -u | head -1
}

# ── 「高並行短連接」那一類的診斷列：TIME_WAIT 落在哪一側（⛔ 不是判據）──────────
#
#   bench_tw_split <被测端口> <`/proc/net/tcp` 与 `/proc/net/tcp6` 的原文>
#     ⇒ 列印一行 `<服务端条数> <客户端条数>`
#
# ★ 只數 state `06`（TIME_WAIT）。**本機**連接埠 = 被測連接埠 ⇒ 那一條是伺服器端先關留下的；
#   **遠端**連接埠 = 被測連接埠 ⇒ 是用戶端先關留下的。
# ⚠ ⚠ **它原本是判據 ⑥ 的第二半，實測站不住，owner 2026-09-23 改判**（見
#   `bench_request_close_violations`）。兩條理由都是實測：
#   ① 門禁參數下 netns 的 TIME_WAIT 表（開發機容器 65536 條）被前兩家佔滿，後兩家讀到 `0 0`
#     （`TcpExt: TCPTimeWaitOverflow` 一趟漲到 112042）；
#   ② 表是空的時候它也只是**競態**：oha 讀完回應自己也立刻關 —— 每家之間等 65 秒時伺服器端佔比
#     nginx 0.946 · HAProxy 0.975 · 樞衡 0.581 · Caddy 0.569。
#   ⇒ 它照記進 `new-conns.txt` 當**診斷**，⛔ 不據它判任何事。
# ⚠ /proc 裡的連接埠是**大寫四位十六進制**（9956 ⇒ `26E4`）；位址列是 `IP:PORT`，
#   IPv6（含 IPv4 映射位址，Go 寫的 Caddy 就落在這裡）那一段是 32 位十六進制、不含冒號
#   ⇒ 取冒號後最後一段。
# ⚠ 兩份檔案各有一行表頭（首列 `sl`）⇒ 按首列跳，⛔ 不按 `NR == 1`：拼起來之後第二份的表頭在中間。
# ★ 本函式只解析**傳進來的原文**，⛔ 不自己去讀 /proc —— 與本檔案其餘判據同一條紀律：
#   解析是最容易寫錯的那一段，⇒ 它必須能用合成輸入兩個方向都證給人看。
bench_tw_split() {
  local port=$1 text=$2 hex
  case "$port" in
    '' | *[!0-9]*)
      echo "bench_tw_split：端口不是正整数（'$port'）" >&2
      return 1
      ;;
  esac
  hex=$(printf '%04X' "$port")
  printf '%s\n' "$text" | awk -v p="$hex" '
    $1 == "sl" || NF < 4 { next }
    $4 != "06" { next }
    {
      n = split($2, l, ":")
      m = split($3, r, ":")
      if (l[n] == p) s++
      else if (r[m] == p) c++
    }
    END { printf "%d %d\n", s, c }
  '
}

# ── 判據 ⑥：「高並行短連接」那一類真的是「每請求一條新連接、伺服器端先關」────────────
#
# ★ ★ ★ **它守的失效同樣不會讓任何東西變紅**：去掉 oha 的兩個旗標，這一類就安靜地
#   變回「keep-alive 吞吐」—— 成功率、狀態碼全都正常，量的卻是另一件事；只去掉
#   `Connection: close`，就安靜地從「伺服器端先關」變成「用戶端先關」。
#
# 兩半，各一個純函式；再加案例指令碼裡那道行為探針（`bench/close-probe.py`，四家收到頭會不會關），
# 三件事各有一道門：
#   ① `bench_short_conn_violations` —— 每請求一條新連接（PassiveOpens 差值）；
#   ② `bench_request_close_violations` —— oha 發出的請求裡**真的帶著** `Connection: close`
#      （開跑前用負載那一組 oha 參數，向一個只記請求頭的合成伺服器端發一次，抓到的原文）。
# ★ 單點變化各自落在哪道門上（2026-09-23 逐個注入、各跑一趟 `BENCH_ONLY` 實測）：
#   兩個旗標都刪 ⇒ ① ② 都紅；只刪 `-H 'Connection: close'` ⇒ ⛔ **不紅，而且這是對的**：
#   oha v1.15.0 的 `--disable-keepalive` 自己帶上那個頭（`src/lib.rs:537`），線上口徑沒變；
#   加 `-H 'Connection: keep-alive'`（使用者頭覆蓋 oha 插的 close）⇒ ① 仍綠、只有 ② 紅 ——
#   ★ **② 單獨守著的就是這一種**。⚠ 設計時寫的是「只刪頭 ⇒ ② 紅」，那是錯的（以為 oha 不發）。
#
#   bench_short_conn_violations <新建下限> <每行 `<名字> <每千请求新建> [診斷列…]`>
#
# 每行打一條違規理由；**一行都不打 = 通過**。契約與判據 ⑤ 逐字相同。
# ⚠ 第 3 列起（TIME_WAIT 兩側條數）是**診斷**，⛔ 不判（理由見 `bench_tw_split`）。
# ⚠ ⚠ **「判不了」一律算違規**：非數字（含 `NaN`）、少列。
# ★ 邊界：恰好壓在下限上算好的那一側 ⇒ 嚴格不等號（與其餘判據同一個約定）。
bench_short_conn_violations() {
  local new_min=$1 readings=$2
  printf '%s\n' "$readings" | awk -v nm="$new_min" '
    NF == 0 { next }
    NF < 2 {
      printf "%s：这一行读不全（原文 %s）—— 「判不了」不算「判过了」\n", $1, $0
      next
    }
    $2 + 0 != $2 {
      printf "%s：每千请求新建连接数不是数字（%s）—— 「判不了」不算「判过了」\n", $1, $2
      next
    }
    $2 + 0 < nm + 0 {
      printf "%s：每千请求只新建了 %s 条连接（下限 %s）⇒ 有请求复用了连接，这一类量的就不是短连接\n", $1, $2, nm
    }
  '
}

#   bench_request_close_violations <抓到的请求原文>
#
# 打一條違規理由；**什麼都不打 = 請求裡帶著 `Connection: close`**。
# ★ 只看**請求頭**（請求行之後、第一個空行之前），頭名**行首錨定、整名相等、大小寫不敏感**，
#   值按逗號分詞、去空白後逐個比 `close`（大小寫不敏感）—— `x-connection: close` 不是那個頭，
#   `Connection: te, close` 是。
# ⚠ 原文是 CRLF 分行的（合成伺服器端原樣寫入磁碟）⇒ 先去掉 `\r`。
# ⚠ ⚠ **什麼都沒抓到算違規**（「判不了」不算「判過了」）。
bench_request_close_violations() {
  local req=$1
  if [ -z "$req" ]; then
    echo "没抓到 oha 发出的请求 —— 「判不了」不算「判过了」"
    return 0
  fi
  printf '%s\n' "$req" | tr -d '\r' | awk '
    NR == 1 { next }
    $0 == "" { exit }
    {
      line = tolower($0)
      if (line ~ /^connection[[:space:]]*:/) {
        sub(/^connection[[:space:]]*:/, "", line)
        n = split(line, toks, ",")
        for (i = 1; i <= n; i++) {
          t = toks[i]
          gsub(/^[[:space:]]+|[[:space:]]+$/, "", t)
          if (t == "close") found = 1
        }
      }
    }
    END {
      if (!found) print "oha 发出的请求里没有 `Connection: close` ⇒ 服务端不会先关，这一类就悄悄变成了客户端先关"
    }
  '
}

# ── 自測：全部用**合成輸入** ───────────────────────────────────────────────
#
# ★ ★ ★ 不相依宿主上此刻恰好是什麼樣（同 G133 的九條自測）。這一點是承重的：
#   本輪唯一跑得到的宿主是**不合格**的那一臺 ⇒ 若自測也從宿主取讀數，
#   「合格」那條分支就一次都執行不到，而**一個永遠回傳不合格的判據，
#   與一個壞掉的判據給出完全相同的輸出**。
bench_self_check() {
  local rc=0
  local out
  # ⚠ ⚠ 條數**從計數器派生，⛔ 不寫死**：一個寫在訊息裡的計數沒有任何門守著，
  #   加了一條斷言卻忘了改那個數，兩邊都不會紅 —— 本儲存庫 2026-09-05 當天栽過一次
  #   同形狀的（`19−2−11−6` 與 `19−2−10−7` 都等於 0，錯的中間項與誰都不矛盾）。
  local n=0
  # 三個斷言原語。★ 計數在這裡**只發生一處** ⇒ 加斷言時數字自己會跟上。
  want_empty() { n=$((n + 1)); [ -z "$2" ] || { echo "✗ $1（实得：$2）" >&2; rc=1; }; }
  want_eq() { n=$((n + 1)); [ "$3" = "$2" ] || { echo "✗ $1（该是 '$2'，实得 '$3'）" >&2; rc=1; }; }
  # shellcheck disable=SC2254  # 模式**有意**不加引號：第 2 個參數就是一個 glob
  want_match() { n=$((n + 1)); case "$3" in $2) ;; *) echo "✗ $1（实得 '$3'）" >&2; rc=1 ;; esac; }

  # ★ 一臺**合成的**合格宿主：五個參數全部處在合格那一側。
  #   下面每條負向案例都從它出發，**只翻一個變數** —— 這樣紅的來源說得清。
  local OK_KERNEL="6.12.0-generic" OK_CPUS=16 OK_LOAD="0.03"
  local OK_ATTEST="专机 · 无代理 · sysctl 已固化" OK_AFF="server=0-3 load=4-7"

  # —— 合格性：合格那一側必須真的存在（★ ★ ★ 承重的就是這一條）——
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_empty "一台合成的合格宿主被判成了不合格" "$out"

  # —— 五條各自要判得動（每條單獨翻一個變數，其餘保持合格）——
  out=$(bench_disqualifiers "5.15.0-microsoft-standard-WSL2" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_match "WSL2 内核串没被判出来" '*kernel:*' "$out"
  # ⚠ 大寫那一種單獨釘一條：`tr` 折小寫那一步刪掉時，只有這條會紅。
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

  # —— 「讀不出來」必須判紅，⛔ 不許當成通過 ——
  out=$(bench_disqualifiers "$OK_KERNEL" "" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_match "核数读空被当成了合格" '*cpus:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "unknown" "$OK_ATTEST" "$OK_AFF")
  want_match "负载读不出来被当成了合格" '*load:*' "$out"

  # —— 邊界：恰好等於閾值的那一側算合格（⛔ 別讓門在邊界上隨機翻面）——
  out=$(bench_disqualifiers "$OK_KERNEL" "$BENCH_MIN_CPUS" "$BENCH_MAX_IDLE_LOAD" "$OK_ATTEST" "$OK_AFF")
  want_empty "恰好压在阈值上的宿主被判成了不合格" "$out"

  # —— 核心參數（G145）——
  #
  # ★ 先釘「宣告本身不是空的」：`bench_kparam_mismatches` 拿一份空宣告會**恆回傳空集**，
  #   而那與「逐條相符」的輸出一模一樣 ⇒ 宣告一旦被誰清空，下面每一條都會照常綠。
  local DECL OBS
  DECL=$(bench_container_sysctls)
  want_eq "容器侧声明的条数不对（空声明会让整格判据恒绿）" 4 "$(printf '%s\n' "$DECL" | grep -c .)"
  want_eq "宿主侧声明的条数不对" 1 "$(bench_host_sysctls | grep -c .)"

  # 第六個參數：傳進不符的比較結果就必須紅，傳空就不該紅。
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF" "net.core.somaxconn 实测 '4096' ≠ 声明 '65535'")
  want_match "内核参数不符没被判成不合格" '*kernel-params:*' "$out"
  out=$(bench_disqualifiers "$OK_KERNEL" "$OK_CPUS" "$OK_LOAD" "$OK_ATTEST" "$OK_AFF" "")
  want_empty "内核参数逐条相符时被判成了不合格" "$out"

  # 比較本身：逐條相符 ⇒ 一行都不打。
  OBS=$(printf '%s\n' 'net.core.somaxconn=65535' 'net.ipv4.tcp_max_syn_backlog=65535' \
    'net.ipv4.ip_local_port_range=10240 65535' 'net.ipv4.tcp_tw_reuse=2')
  want_empty "逐条相符却打出了问题" "$(bench_kparam_mismatches "$DECL" "$OBS")"

  # ★ ★ 承重：`sysctl` 打多值用 **tab**，宣告裡寫的是空格 ⇒ 少了歸一化，
  #   一次完全正確的固化會被判成不符，而那種門最後會被人繞過去而不是被人滿足。
  want_empty "tab 与空格的差别被当成了不符（归一化那一步失效了）" \
    "$(bench_kparam_mismatches 'net.ipv4.ip_local_port_range=10240 65535' "$(printf 'net.ipv4.ip_local_port_range=10240\t65535')")"

  # 值不等 ⇒ 逐字點名那個鍵。
  want_match "值不等没被判出来" '*somaxconn*' \
    "$(bench_kparam_mismatches 'net.core.somaxconn=65535' 'net.core.somaxconn=4096')"
  # ⚠ 只點該點的那一個：另一條相符的鍵不許被順帶報出來。
  want_eq "值不等时报出的条数不对（相符的那条被顺带报了）" 1 \
    "$(bench_kparam_mismatches "$(printf 'net.core.somaxconn=65535\nnet.ipv4.tcp_tw_reuse=2\n')" \
       "$(printf 'net.core.somaxconn=4096\nnet.ipv4.tcp_tw_reuse=2\n')" | grep -c .)"

  # 「讀不到」必須判紅，⛔ 不許當成通過 —— 一個查不到就回傳空串的實作，會把
  # 「這個鍵不存在」和「這個鍵的值是空」混成同一件事。
  want_match "键在实测里读不到却被当成了相符" '*读不到*' \
    "$(bench_kparam_mismatches 'net.core.somaxconn=65535' 'net.ipv4.tcp_tw_reuse=2')"
  want_eq "实测整份为空时没有逐条报出来" 4 \
    "$(bench_kparam_mismatches "$DECL" "" | grep -c .)"

  # ★ ★ ★ 空宣告：⛔ 不許靜默通過。
  want_match "空声明被当成了「全部相符」" '*声明为空*' "$(bench_kparam_mismatches "" "$OBS")"
  want_match "只有空白的声明被当成了「全部相符」" '*声明为空*' "$(bench_kparam_mismatches "$(printf '  \n\n')" "$OBS")"

  # —— docker 旗標：從宣告推匯出來的那一份 ——
  want_eq "docker 旗标行数不对（4 个 --sysctl + 1 个 --ulimit ⇒ 10 行）" 10 \
    "$(bench_docker_sysctl_flags | grep -c .)"
  # ★ ★ 承重：`10240 65535` 自己帶空格，它必須是**一整行一個 token**。
  #   ⛔ 被劈成兩個參數時 docker 只報一句語焉不詳的用法錯，而不會提到那個空格。
  want_eq "带空格的那个值被劈开了" 1 \
    "$(bench_docker_sysctl_flags | grep -cF 'net.ipv4.ip_local_port_range=10240 65535')"
  want_eq "nofile 旗标没跟着 BENCH_NOFILE 走" 1 \
    "$(bench_docker_sysctl_flags | grep -cF "nofile=${BENCH_NOFILE}:${BENCH_NOFILE}")"

  # —— 最強者：逐類現算，⛔ 不是第一行、不是平均 ——
  out=$(printf 'caddy 100\nnginx 300\nhaproxy 200\n' | bench_best_of)
  want_eq "最强者算错了" "nginx 300" "$out"
  # ⚠ 把最大值放在**第一行**：一個「取第一行」的錯實作在上一條裡也會綠。
  out=$(printf 'nginx 300\ncaddy 100\nhaproxy 200\n' | bench_best_of)
  want_eq "最大值在首行时算错了" "nginx 300" "$out"
  # ⚠ 非數字行必須被跳過，⛔ 不許把它當成 0 參與比較後仍宣稱有資料。
  out=$(printf 'caddy n/a\nnginx 300\n' | bench_best_of)
  want_eq "非数字行没被跳过" "nginx 300" "$out"
  out=$(printf 'caddy n/a\n' | bench_best_of)
  want_empty "一行有效数据都没有时还报出了最强者" "$out"

  # —— 判定：兩個方向都要出得來 ——
  out=$(bench_verdict_one 280 "$(printf 'caddy 100\nnginx 300\nhaproxy 200\n')")
  want_match "280 对最强者 300（门槛 270）该 PASS" "PASS *" "$out"
  out=$(bench_verdict_one 260 "$(printf 'caddy 100\nnginx 300\nhaproxy 200\n')")
  want_match "260 对最强者 300（门槛 270）该 FAIL" "FAIL *" "$out"
  # ⚠ 邊界：恰好 90% 算過（10% 是「不劣於」，含等號）。
  out=$(bench_verdict_one 270 "$(printf 'nginx 300\n')")
  want_match "恰好 90% 该 PASS" "PASS *" "$out"
  out=$(bench_verdict_one 999 "")
  want_eq "没有竞品读数时该报 NODATA" "NODATA" "$out"

  # —— 判據 ④：壓測端飽和 ————————————————————————————————————————
  #
  # ★ 一組**分得開**的全場讀數：下面每條負向案例都從它出發，只翻一個變數。
  local SPREAD_OK CEIL_SET
  SPREAD_OK=$(printf 'fulcrum 300\nnginx 280\nhaproxy 200\ncaddy 100\n')
  # ★ ★ 合格那一側必須真的存在（同判據 ① 那條承重的自測）：
  #   **一個恆作廢的判據，與一個壞掉的判據給出完全相同的輸出。**
  out=$(bench_saturation "" "$SPREAD_OK")
  want_empty "一组分得开的读数被判成了饱和" "$out"

  # —— B（承重）：四家擠在一起 ——
  local SPREAD_TIGHT
  SPREAD_TIGHT=$(printf 'fulcrum 1000\nnginx 1010\nhaproxy 1020\ncaddy 1005\n')
  out=$(bench_saturation "" "$SPREAD_TIGHT")
  want_match "四家挤在 2% 以内没被判成饱和" '*spread:*' "$out"

  # ★ ★ ★ **承重的是這一條對照**：同一組飽和資料餵給判據 ③，它照常打 PASS
  #   （樞衡 1000 對最強者 1020，門檻 918）。⇒ ④ 抓的正是 ③ **結構上**抓不到的
  #   那件事。⛔ 少了這條，「④ 判得動」與「④ 在重複 ③ 已經會做的事」分不開。
  out=$(bench_verdict_one 1000 "$(printf 'nginx 1010\nhaproxy 1020\ncaddy 1005\n')")
  want_match "同一组饱和数据判据 ③ 本来就该照常 PASS（这正是 ④ 存在的理由）" "PASS *" "$out"

  # —— 邊界：離散度恰好等於閾值算**不**飽和（⛔ 別在邊界上隨機翻面）——
  #   (100 − 95) / 100 = 0.0500，恰好是 BENCH_MIN_SPREAD。
  out=$(bench_saturation "" "$(printf 'a 100\nb 95\n')")
  want_empty "离散度恰好等于阈值时被判成了饱和" "$out"

  # —— 「判不了」必須作廢，⛔ 不許當成「沒飽和」——
  out=$(bench_saturation "" "$(printf 'fulcrum 300\n')")
  want_match "只有一条读数时被当成了没饱和" '*spread:*' "$out"
  out=$(bench_saturation "" "")
  want_match "一条读数都没有时被当成了没饱和" '*spread:*' "$out"
  # ⚠ 非數字行不算有效讀數（與 bench_best_of 同一口徑）⇒ 只剩一條 ⇒ 判不了。
  out=$(bench_saturation "" "$(printf 'fulcrum 300\nnginx n/a\n')")
  want_match "非数字行被当成了有效读数" '*spread:*' "$out"

  # —— A：上限那一半。★ CEIL_SET 的離散度是 0.7778，B 一定不響 ⇒
  #   下面幾條紅或不紅**只可能來自 A**，紅的來源說得清。
  CEIL_SET=$(printf 'fulcrum 900\nnginx 600\nhaproxy 400\ncaddy 200\n')
  # ★ ★ 一個變數的翻面：同一組讀數，上限留空 vs 給一個逼近的上限。
  out=$(bench_saturation "" "$CEIL_SET")
  want_empty "上限留空时 A 不该参与（它是可选输入，不是承重件）" "$out"
  out=$(bench_saturation 950 "$CEIL_SET")
  want_match "最强者逼近上限没被判出来" '*ceiling:*' "$out"
  out=$(bench_saturation 10000 "$CEIL_SET")
  want_empty "最强者远低于上限时被判成了饱和" "$out"
  # ⚠ 邊界：900 恰好等於 1000 × 0.90 ⇒ 算**不**飽和。
  #   ★ 數值有意取整除得盡的一對，⛔ 別用 300/0.9 那種 —— 浮點尾數會讓邊界隨機翻面，
  #     而那樣的話這條自測本身就成了一個不可靠的判據。
  out=$(bench_saturation 1000 "$CEIL_SET")
  want_empty "最强者恰好压在 上限×headroom 上时被判成了饱和" "$out"
  # —— 上限讀不出來必須作廢 ——
  out=$(bench_saturation "n/a" "$CEIL_SET")
  want_match "上限不是数字时被当成了没饱和" '*ceiling:*' "$out"
  out=$(bench_saturation 0 "$CEIL_SET")
  want_match "上限是 0 时被当成了没饱和" '*ceiling:*' "$out"

  # —— 判據 ④C：最強者附近的收斂（G146，結案 D34）——
  #
  # ★ ★ ★ 這一組的**承重對照**是第一條：2026-09-06 那組**真實資料**餵給 B 時
  #   它一聲沒吭（全場極差 0.8599），而餵給 C 時它響 ——
  #   ⇒ 證明 C 抓的正是 B **結構上**抓不到的那件事。
  #   ⛔ 少了這條對照，「C 判得動」與「C 在重複 B 已經會做的事」分不開。
  local REAL_0906
  REAL_0906=$(printf 'haproxy 54531.2335\nnginx 54144.1168\ncaddy 10834.7315\nfulcrum 7638.6264\n')
  want_empty "2026-09-06 那组真实数据被 B 判成了收敛（它的全场极差是 0.8599）" \
    "$(bench_saturation "" "$REAL_0906")"
  want_match "同一组真实数据里最快两家只差 0.0071，C 却没判出来" '*top-convergence:*' \
    "$(bench_top_convergence "$REAL_0906")"

  # 分得開 ⇒ 一行都不打。
  want_empty "最强者与第二强差 0.30 却被判成了收敛" \
    "$(bench_top_convergence "$(printf 'a 100\nb 70\nc 50\n')")"
  # ⚠ 邊界：恰好等於閾值算**分得開**（與其餘判據同一個約定）。
  want_empty "恰好压在阈值上被判成了收敛" \
    "$(bench_top_convergence "$(printf 'a 100\nb 95\n')")"
  # ★ 剛跨過去那一側必須響 —— 少了這條，上一條與「恆不響」分不開。
  want_match "刚跨进阈值内却没判出来" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 100\nb 96\n')")"
  # ⚠ 它看的是**最強者與第二強**，⛔ 不是最強與最弱：下面這組極差 0.9 而頂部收斂。
  want_match "顶部收敛被底部的离散掩盖了（这正是 D34 那个缺陷）" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 100\nb 99\nc 10\n')")"
  # 「判不了」一律作廢。
  want_match "只有一条读数时被当成了分得开" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 100\n')")"
  want_match "读数全不是数字时被当成了分得开" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a x\nb y\n')")"
  want_match "最强者不是正数时被当成了分得开" '*top-convergence:*' \
    "$(bench_top_convergence "$(printf 'a 0\nb 0\n')")"

  # —— 判據 ⑤：上游連接重複使用（**口徑對齊**，⛔ 不是效能門檻）——
  #
  # ★ ★ 合成輸入用的是**真實實測值**（2026-09-09 `ShanirePCX`），⛔ 不是編的
  #   —— 與上面那組 `REAL_0906` 同一個做法。
  local REUSE_ALIGNED REUSE_NGINX_OFF
  REUSE_ALIGNED=$(printf 'fulcrum 3.20\ncaddy 3.16\nhaproxy 2.40\nnginx 2.39\n')
  REUSE_NGINX_OFF=$(printf 'fulcrum 2.92\ncaddy 4.12\nhaproxy 1.72\nnginx 1005.65\n')
  want_empty "四家对齐后的真实读数被判成了违规 —— 那道判据恒红" \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$REUSE_ALIGNED")"
  want_match "nginx 每千请求新建 1005.65 条却没被判成违规 —— 那道判据是空操作" '*nginx*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$REUSE_NGINX_OFF")"

  # ⚠ ⚠ ★ ★ ★ **已知的能力邊界，釘在這裡免得被誤讀。**
  #   上面那組 `REUSE_NGINX_OFF` 裡 caddy 的 `transport` 塊**也被撤掉了**，
  #   而它的讀數只有 4.12 ⇒ **判不出來**。「部分不復用」那一族的表現**相依並行數**
  #   （門禁只跑 10 條連接），⇒ 這道判據在門禁參數下只抓得住「每請求新建」那一族。
  #   ⛔ 別據此把閾值壓到 4 附近：門禁 2s×10 連接下若 rps 掉到 500，
  #   oha 自己那 10 條就是 10/千 —— 那會變成一道隨機翻面的門。
  want_empty "门禁参数下的 caddy 4.12 被判成了违规（这条红了说明阈值紧到会误伤）" \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy 4.12\n')")"
  # ★ 而視窗參數（連接數大一個量級）下同一處缺陷會暴露成 100.3 ⇒ 那時必須抓住。
  #   ⇒ **同一道判據在兩種參數下各抓一族**，而視窗那一趟的讀數會進原始資料。
  want_match "窗口参数下 caddy 未对齐的 100.30 没被判成违规" '*caddy*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy 100.30\n')")"

  # 「判不了」一律算違規（同判據 ①/④ 那句「『沒能檢查』不算『檢查通過』」）。
  want_match "NaN 被当成了通过" '*caddy*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy NaN\n')")"
  want_match "少一列的行被当成了通过" '*caddy*' \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'caddy\n')")"
  # ⚠ 邊界：恰好壓在閾值上算**好**的那一側（與其餘判據同一個約定）。
  want_empty "恰好压在阈值上被判成了违规" \
    "$(bench_upstream_reuse_violations "$BENCH_UPSTREAM_REUSE_MAX" "$(printf 'a %s\n' "$BENCH_UPSTREAM_REUSE_MAX")")"

  # —— 上游空閒連接上限：跟著並行數走（2026-09-10）——
  #
  # ★ 寫死的 64 在視窗的 200 並行下讓 caddy / nginx 部分不復用（開發機單變數實測：
  #   179.32 → 1.52、130.16 → 1.35 條/千）⇒ 上限 = max(64, 並行連接數)，由案例指令碼繪製。
  # ⚠ 下限 64 是承重的：門禁只跑 10 條連接，繪製出來必須**仍是 64** ⇒ 門禁行為一字不變。
  want_eq "门禁的 10 条连接没有渲染成 64（门禁行为会变）" 64 "$(bench_upstream_idle_cap 10)"
  want_eq "恰好 64 条连接没有渲染成 64" 64 "$(bench_upstream_idle_cap 64)"
  want_eq "65 条连接没有渲染成 65（多一条也得跟上）" 65 "$(bench_upstream_idle_cap 65)"
  want_eq "窗口的 200 条连接没有渲染成 200（上限没跟上并发 ⇒ 部分不复用）" 200 "$(bench_upstream_idle_cap 200)"
  # 「算不出來」必須當場回報錯誤，⛔ 不許安靜回落到某個數 —— 那與寫死沒有區別。
  want_eq "并发数不是数字时没有报错" ERR "$(bench_upstream_idle_cap abc 2>/dev/null || echo ERR)"
  want_eq "并发数为 0 时没有报错" ERR "$(bench_upstream_idle_cap 0 2>/dev/null || echo ERR)"
  want_eq "并发数为空时没有报错" ERR "$(bench_upstream_idle_cap '' 2>/dev/null || echo ERR)"

  # —— 建置身分：從產物位元組裡取（`G141`；2026-09-10 視窗二那次粘鄰居的缺陷）——
  #
  # ★ 第一條用的是**真實的失效形狀**（視窗二 `env.json` 的 `fulcrum_build_id`），⛔ 不是編的。
  want_eq "身份串后面紧跟字母数字时把邻居吃了进来（窗口二那次的缺陷）" \
    "0.1.0+g6bf1f3fe0618" \
    "$(printf 'xx\000yyPriority0.1.0+g6bf1f3fe0618HeadersResetPriority\000zz' | bench_build_id_from_bytes)"
  want_eq "第三档 unknown 后面紧跟字母数字时把邻居吃了进来" \
    "0.1.0+unknown" \
    "$(printf 'ab\000cd0.1.0+unknownFooBar\000ef' | bench_build_id_from_bytes)"
  # ⚠ 提交號那一檔是**定長** 12 位：多出來的十六進制就是鄰居。
  want_eq "12 位之后的十六进制邻居被吃了进来" \
    "0.1.0+g6bf1f3fe0618" \
    "$(printf 'x0.1.0+g6bf1f3fe0618abc' | bench_build_id_from_bytes)"
  want_empty "没有身份串的字节里取出了东西" \
    "$(printf 'no version here 1.2.3 and 1.2.3+ nothing\n' | bench_build_id_from_bytes)"

  # —— 判據 ⑥：高並行短連接（**口徑自證**，⛔ 不是效能門檻）——
  #
  # 解析：一份合成的 /proc/net/tcp + tcp6 原文。★ 連接埠 9956 = 0x26E4、9957 = 0x26E5。
  local TCP_TEXT
  TCP_TEXT=$(printf '%s\n' \
    '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode' \
    '   0: 0100007F:26E4 0100007F:A001 06 00000000:00000000 03:00000F00 00000000     0        0 0 3 0000000000000000' \
    '   1: 0100007F:26E4 0100007F:A002 06 00000000:00000000 03:00000F00 00000000     0        0 0 3 0000000000000000' \
    '   2: 0100007F:A003 0100007F:26E4 06 00000000:00000000 03:00000F00 00000000     0        0 0 3 0000000000000000' \
    '   3: 0100007F:26E4 0100007F:A004 01 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000' \
    '   4: 0100007F:26E5 0100007F:A005 06 00000000:00000000 03:00000F00 00000000     0        0 0 3 0000000000000000' \
    '  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode' \
    '   0: 0000000000000000FFFF00000100007F:26E4 0000000000000000FFFF00000100007F:A006 06 00000000:00000000 03:00000F00 00000000     0        0 0 3 0000000000000000')
  # ★ 伺服器端 3 = v4 兩條 + v6（IPv4 映射位址）一條；用戶端 1；ESTABLISHED 與別的連接埠都不算。
  want_eq "TIME_WAIT 两侧数错了" "3 1" "$(bench_tw_split 9956 "$TCP_TEXT")"
  # ★ 反向：換一個連接埠，同一份原文裡只剩 26E5 那一條 —— 少了這條，「數對了」與「恆打 3 1」分不開。
  want_eq "换了端口还数出了别人的 TIME_WAIT" "1 0" "$(bench_tw_split 9957 "$TCP_TEXT")"
  want_eq "端口不是数字时没有报错" ERR "$(bench_tw_split abc "$TCP_TEXT" 2>/dev/null || echo ERR)"

  # 判定 ① 半：每請求一條新連接。★ 對齊那一組是**真實實測值**（2026-09-23 `ShanirePCX` 容器裡、
  #   門禁參數 2s × 10，與 REUSE_ALIGNED 同一個做法）。⚠ 後兩家 TIME_WAIT 是 `0 0`：那一趟 netns 的
  #   TIME_WAIT 表被前兩家佔滿了（`TCPTimeWaitOverflow` 112042）⇒ ★ 它正好釘住「第 3、4 列不許被判」。
  local SHORT_ALIGNED SHORT_KEEPALIVE
  SHORT_ALIGNED=$(printf 'fulcrum 1000.31 21708 15425\ncaddy 1000.41 16189 12213\nhaproxy 1000.15 0 0\nnginx 1000.17 0 0\n')
  # ★ keep-alive 那一行也是真實值：NC-1 注入（兩個旗標都刪），同一天同一臺機器。
  SHORT_KEEPALIVE=$(printf 'fulcrum 0.19 1 3\ncaddy 0.25 1 4\nhaproxy 0.12 1 3\nnginx 0.11 1 5\n')
  want_empty "四家对齐的真实读数被判成了违规（TIME_WAIT 那两列是诊断，⛔ 不许判）" \
    "$(bench_short_conn_violations "$BENCH_SHORT_NEW_CONNS_MIN" "$SHORT_ALIGNED")"
  want_match "每千请求只新建 0.19 条却没判出来 —— 这一类悄悄变回了 keep-alive 吞吐" '*fulcrum*只新建了*' \
    "$(bench_short_conn_violations "$BENCH_SHORT_NEW_CONNS_MIN" "$SHORT_KEEPALIVE")"
  # 「判不了」一律算違規。
  want_match "NaN 被当成了通过" '*caddy*新建连接数不是数字*' \
    "$(bench_short_conn_violations "$BENCH_SHORT_NEW_CONNS_MIN" "$(printf 'caddy NaN 10 0\n')")"
  want_match "少列的行被当成了通过" '*caddy*读不全*' \
    "$(bench_short_conn_violations "$BENCH_SHORT_NEW_CONNS_MIN" "$(printf 'caddy\n')")"
  # ⚠ 邊界：恰好壓在下限上算好的那一側（與其餘判據同一個約定）；剛跨過去那一側必須響。
  want_empty "恰好压在新建下限上被判成了违规" \
    "$(bench_short_conn_violations "$BENCH_SHORT_NEW_CONNS_MIN" "$(printf 'a %s 0 0\n' "$BENCH_SHORT_NEW_CONNS_MIN")")"
  want_match "刚低于新建下限却没判出来" '*a*只新建了*' \
    "$(bench_short_conn_violations 990 "$(printf 'a 989.99 0 0\n')")"

  # 判定 ② 半：oha 真的發了 `Connection: close`（抓到的請求原文，CRLF 分行）。
  # ★ 頭兩條是 oha v1.15.0 **真實發出**的請求頭（2026-09-23 抓到的原文，逐字）：
  #   一條是負載那一組參數，一條是 NC-2′ 注入的 `-H 'Connection: keep-alive'`（使用者頭覆蓋了 oha 自己插的 close）。
  want_empty "oha 真实发出的 connection: close 被判成了没带" \
    "$(bench_request_close_violations "$(printf 'GET /payload.bin HTTP/1.1\r\naccept: */*\r\naccept-encoding: gzip, compress, deflate, br\r\nuser-agent: oha/1.15.0\r\nconnection: close\r\nhost: 127.0.0.1:9960\r\n\r\n')")"
  want_match "oha 真实发出的 connection: keep-alive 被当成了 close（NC-2′ 那一种）" '*没有*Connection: close*' \
    "$(bench_request_close_violations "$(printf 'GET /payload.bin HTTP/1.1\r\naccept: */*\r\naccept-encoding: gzip, compress, deflate, br\r\nuser-agent: oha/1.15.0\r\nconnection: keep-alive\r\nhost: 127.0.0.1:9960\r\n\r\n')")"
  want_empty "首字母大写的 Connection: Close 被判成了没带" \
    "$(bench_request_close_violations "$(printf 'GET / HTTP/1.1\r\nConnection: Close\r\n\r\n')")"
  want_empty "逗号分隔的 Connection 值里有 close 却被判成了没带" \
    "$(bench_request_close_violations "$(printf 'GET / HTTP/1.1\r\nConnection: te, close\r\n\r\n')")"
  want_match "请求里没有 Connection 头却没判出来 —— 这一类悄悄变成了客户端先关" '*没有*Connection: close*' \
    "$(bench_request_close_violations "$(printf 'GET / HTTP/1.1\r\nhost: x\r\naccept: */*\r\n\r\n')")"
  want_match "Connection: keep-alive 被当成了 close" '*没有*Connection: close*' \
    "$(bench_request_close_violations "$(printf 'GET / HTTP/1.1\r\nConnection: keep-alive\r\n\r\n')")"
  # ⚠ 頭名必須**行首錨定、整名相等**：`x-connection: close` 不是那個頭。
  want_match "别的头名里含 connection 被当成了那个头" '*没有*Connection: close*' \
    "$(bench_request_close_violations "$(printf 'GET / HTTP/1.1\r\nx-connection: close\r\n\r\n')")"
  # ⚠ 只看請求頭：空行之後出現同樣的字樣不算（抓頭伺服器端只存頭，這一條防的是有人改成存整段）。
  want_match "空行之后的字样被当成了请求头" '*没有*Connection: close*' \
    "$(bench_request_close_violations "$(printf 'GET / HTTP/1.1\r\nhost: x\r\n\r\nConnection: close\r\n')")"
  want_match "什么都没抓到被当成了通过" '*没抓到*' \
    "$(bench_request_close_violations '')"

  if [ "$rc" = 0 ]; then
    echo "[bench/lib] 判据自测通过（合成输入，${n} 条）"
  else
    echo "[bench/lib] ★ 判据自测未通过 —— **本次对拍的任何结论都不可信**" >&2
  fi
  return "$rc"
}

# 被 `source` 時什麼都不做；直接跑且帶 --self-check 時跑自測。
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  case "${1:-}" in
    --self-check) bench_self_check ;;
    *) echo "用法：bash bench/lib.sh --self-check" >&2; exit 2 ;;
  esac
fi
