#!/usr/bin/env bash
# shellcheck shell=bash
#
# ACME 兩個端到端場景共用的那一半：[`run.sh`](run.sh)（簽發）與 [`renew.sh`](renew.sh)（續期）。
#
# ★ ★ **為什麼抽出來，而不是抄一份。**
#   兩個場景都要「起一個真 CA（pebble）+ 一臺假權威 NS（challtestsrv）+ 一個 exec hook」，
#   那是一百多行、且每一行都帶著一條踩坑記錄（v6 迴環、兩級證書、SIGTERM vs SIGINT…）。
#   本儲存庫在「同一個判定抄了兩份」上反覆栽過——D18 那次的處置就是**讓分家在結構上做不到**
#   （兩側共用 `wildcard_covers` 一份實作）。抄一份 pebble 起法，等於給下一個人埋一處必然分叉：
#   改了一邊、另一邊繼續綠，而綠的那邊測的已經不是同一個東西了。
#
# 呼叫方在 source 之前必須設好：`WORK`（工作目錄）、`HOST`、`TRUST_ANCHOR`（信任根落點）。

# 失敗計數與處理程序表。★ 樞衡的處理程序收 SIGINT，別的（pebble / challtestsrv）收 SIGTERM——
# 見 `acme_cleanup` 裡那段註解，那是量出來的不是潔癖。
FAILS=0
PIDS=()
AUX_PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

# 收屍：先招呼、再等、最後補刀；然後把信任根摘掉。
#
# ★ 不是潔癖：Pingora 把 SIGTERM 當**優雅停機**，會等完整的排空視窗（每次跑白等幾秒）；
#   而 pebble 與 challtestsrv 是 Go 處理程序，**根本不理 SIGINT 之外的期待**——
#   對所有處理程序一律 SIGINT 的話，pebble 每次都要挨一發 SIGKILL，
#   bash 就在收工時打一行 `Killed`，讀起來像是出了事。
acme_cleanup() {
  local pid waited
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  for pid in "${AUX_PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -TERM "$pid" 2>/dev/null || true
  done
  for pid in "${PIDS[@]:-}" "${AUX_PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -9 "$pid" 2>/dev/null || true
  done
  # ★ 把信任庫改回去。容器是 --rm 的，但這些指令碼也可能被人在別處跑，
  #   而「往系統信任庫裡塞了一張自簽根然後不收拾」是一件不該留下的事。
  if [ -f "$TRUST_ANCHOR" ]; then
    rm -f "$TRUST_ANCHOR"
    update-ca-certificates --fresh >/dev/null 2>&1 || true
  fi
}

port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}

wait_port() {
  local port=$1 tries=0
  while [ "$tries" -lt 200 ]; do
    if port_listening "$port"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

# 等某個檔案裡出現某段文字。★ 用「紀錄裡出現了什麼」當同步點，而不是 `sleep N`：
#   固定 sleep 要麼慢、要麼在慢一點的機器上變成偶發紅。
wait_log() {
  local file=$1 pattern=$2 limit=${3:-60} waited=0
  while [ "$waited" -lt "$((limit * 4))" ]; do
    # ★ ★ `-F`（固定字串）**不是可選的**：要等的那行裡有 `*.wild.example`，
    #   而 `*` 在正則裡是元字元 —— `成功：*.wild.example` 會被讀成
    #   「零個或多個『：』+ 任意字元 + wild…」，於是**永遠匹配不上一行確實存在的紀錄**。
    #   ⚠ 症狀極具誤導性：門報「120s 內沒等到簽發成功」，而紀錄裡那一行明明就在那兒。
    if [ -f "$file" ] && grep -qF "$pattern" "$file"; then return 0; fi
    sleep 0.25
    waited=$((waited + 1))
  done
  return 1
}

# 等某個檔案裡某段文字出現**第 N 次**。續期場景要的判據是「又簽了一張」，
# 而「簽發成功」這句在第一次簽發時就已經出現過一遍。
# ⚠ 只等「出現過」會立刻命中第一次那條，於是一道本該驗續期的門變成恆綠。
wait_log_count() {
  local file=$1 pattern=$2 want=$3 limit=${4:-60} waited=0 got
  while [ "$waited" -lt "$((limit * 4))" ]; do
    if [ -f "$file" ]; then
      got=$(grep -cF "$pattern" "$file" || true)
      if [ "${got:-0}" -ge "$want" ]; then return 0; fi
    fi
    sleep 0.25
    waited=$((waited + 1))
  done
  return 1
}

# ⚠ `|| true` 是必需的：`set -e` 之下，賦值裡的命令替換失敗會讓整個指令碼退出。
run_curl() { curl "$@" 2>/dev/null || true; }

# ★ ★ 要**結束代碼**時不能走命令替換（子 shell 裡設的變數傳不回來）。
CURL_RC=0
# shellcheck disable=SC2034  # CURL_RC 由 source 本檔案的場景指令碼讀，跨檔案 shellcheck 看不見
curl_capture() {
  set +e
  curl "$@" > "$WORK/curlw" 2>/dev/null
  CURL_RC=$?
  set -e
}

expect_status() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望 $want，实际 $got"; fi
}

# 檔案權限判據。私鑰必須是 0600（G55）。
expect_mode() {
  local what=$1 want=$2 path=$3 got
  if [ ! -e "$path" ]; then
    fail "$what：$path 不存在"
    return
  fi
  got=$(stat -c '%a' "$path")
  if [ "$got" = "$want" ]; then ok "$what 权限 $got"; else fail "$what 权限期望 $want，实际 $got"; fi
}

# 鏡像裡那幾個外部處理程序在不在（G64 裝的那套）。第一個參數是場景名，只進回報錯誤文字。
acme_require_tools() {
  local scenario=$1 one
  for one in pebble pebble-challtestsrv openssl curl update-ca-certificates; do
    command -v "$one" >/dev/null 2>&1 || {
      echo "$scenario FAILED: 镜像里没有 $one —— 这个场景要 docker/Dockerfile.build 里那套（G64）" >&2
      exit 1
    }
  done
}

# ★ ★ 這一步不是形式：本儲存庫栽過一次「基線探針對著上一個場景遺留的處理程序報綠」。
acme_require_ports_free() {
  local scenario=$1 p
  shift
  for p in "$@"; do
    if port_listening "$p"; then
      echo "$scenario FAILED: 端口 $p 已经被占用了 —— 先清掉再跑，否则下面测的是别人的服务。" >&2
      exit 1
    fi
  done
}

# ── 失敗現場取證：那一刻誰佔著哪些連接埠 ──────────────────────────────────
#
# ⛔ **只在已經要紅的路徑上呼叫**：它一行都不參與判定，
#   因此不可能把一趟本來能過的跑判紅。
#
# ★ ★ ★ **它第一次跑就把那次間歇紅的根因指出來了，而現有紀錄一個字都答不出**：
#   上一個場景（quic-relay）漏了一個處理程序，它攥著**合成出來的 `:80`**（見 AGENTS.md 連接埠表）；
#   pingora 在**持有全域 `ListenFds` 鎖**的狀態下做那個會重試 30 秒的 bind
#   ⇒ 排在後面的 `:8083` 跟著起不來，而紀錄裡只有 `:80` 的錯。
#   ⇒ **報出來的連接埠不是出事的連接埠**，靠讀紀錄永遠繞不出來。
#   ★ 源頭已經在 quic-relay 自己的 `cleanup` 裡堵上（它走時要把用過的連接埠還回去）。
#
# ⚠ 鏡像裡**沒有 `ss` / `lsof` / `fuser`**（iproute2 只裝在 systemd 測試鏡像裡）
#   ⇒ 只能讀 `/proc/net/tcp{,6}` 與 `/proc/<pid>/fd`。
#
# ★ ★ 打的是**全部狀態**，不只是 LISTEN：`bind()` 報 `EADDRINUSE` 那一刻，
#   「另一個處理程序正在 LISTEN」與「同一個 addr:port 上掛著一條殘留連接」是兩個不同的答案，
#   而只打 LISTEN 的話後者會表現成「什麼都沒有」—— 一句最會把人帶偏的結論。
#
# ★ ★ ★ 同一個 inode 的**全部**持有者都要打：一個被子處理程序繼承走的監聽 fd，
#   現場就是「兩個 pid 指著同一個 socket」，而處理程序表裡那個 fulcrum 已經不在了。
#   這個形狀本儲存庫栽過一次，常設判據是 [`tests/m0/unclaimed.sh`](../m0/unclaimed.sh)。

# `/proc/net/tcp{,6}` → 每行一個 `地址:端口 状态 inode=N`（v6 寫成 `[地址]:端口`）。
# 參數是要讀的檔案 —— 自測時指向樣本，於是**同一份解碼**兩處共用。
# ⚠ `strtonum()` 是 gawk 擴充，而鏡像裡的 awk 是 mawk ⇒ 十六進制只能自己拆。
acme_decode_proc_net() {
  awk '
    function h2d(s,   i, d, n) {
      n = 0
      s = toupper(s)
      for (i = 1; i <= length(s); i++) {
        d = index("0123456789ABCDEF", substr(s, i, 1)) - 1
        if (d < 0) return -1
        n = n * 16 + d
      }
      return n
    }
    # v4 是 8 位十六进制、**按字节反序**；v6 是 32 位，按 4 个 32 位字各自反字节序。
    # ⚠ 双栈监听 `[::]:port` 只出现在 tcp6 里，tcp 一行都没有 —— 所以两个文件都要读。
    function addr(h,   i, w, b, out, grp) {
      if (length(h) == 8) {
        out = ""
        for (i = 4; i >= 1; i--) out = out (i == 4 ? "" : ".") h2d(substr(h, i * 2 - 1, 2))
        return out
      }
      out = ""
      for (w = 0; w < 4; w++)
        for (b = 4; b >= 1; b--) out = out substr(h, w * 8 + b * 2 - 1, 2)
      # 16 字节 → 8 组四位十六进制。**不做 `::` 压缩**：诊断要的是没有歧义，不是好看。
      grp = ""
      for (i = 1; i <= 8; i++) grp = grp (i == 1 ? "" : ":") substr(out, i * 4 - 3, 4)
      return "[" grp "]"
    }
    BEGIN {
      S["01"] = "ESTABLISHED"; S["02"] = "SYN_SENT";  S["03"] = "SYN_RECV"
      S["04"] = "FIN_WAIT1";   S["05"] = "FIN_WAIT2"; S["06"] = "TIME_WAIT"
      S["07"] = "CLOSE";       S["08"] = "CLOSE_WAIT"; S["09"] = "LAST_ACK"
      S["0A"] = "LISTEN";      S["0B"] = "CLOSING"
    }
    # 表头没有 `<数字>:` 这一列 —— 靠它把表头挡掉，而不是靠 NR>1（两个文件各有一行表头）。
    $1 !~ /^[0-9]+:$/ { next }
    {
      split($2, L, ":")
      st = toupper($4)
      printf "%s:%d %s inode=%s\n", addr(L[1]), h2d(L[2]), (st in S ? S[st] : "状态" st), $10
    }
  ' "$@"
}

# 解碼自測。★ 它**不是形式**：一個恆空的取證函式，在最該說話的那一刻是沉默的，
# 而沉默會被讀成「連接埠上什麼都沒有」—— 那正好是本次要查的那個問題的錯誤答案。
# ⚠ 只在取證路徑上跑（一次幾毫秒），綠的跑一個字都不多。
#
# ★ 樣本是**真實核心輸出**：v4 兩行與 v6 那行都是 `tests/m0/unclaimed.sh` 在建置鏡像裡
#   實抓下來的那一份（它同時是那處 v6 盲區的實物證據），這裡原樣引用。
acme_selftest_decode() {
  local dir out bad="" want
  dir=$(mktemp -d)
  {
    echo '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
    echo '   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0'
    echo '   1: 0100007F:1F91 0100007F:8000 01 00000000:00000000 00:00000000 00000000     0        0 12346 1 0000000000000000 20 0 0 10 -1'
  } > "$dir/tcp"
  {
    echo '  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
    echo '   0: 00000000000000000000000000000000:1FA3 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 52691 1 00000000c0a6790d 100 0 0 10 0'
  } > "$dir/tcp6"
  out=$(acme_decode_proc_net "$dir/tcp" "$dir/tcp6" 2>/dev/null || true)
  rm -rf "$dir"

  for want in \
    '127.0.0.1:8080 LISTEN inode=12345' \
    '127.0.0.1:8081 ESTABLISHED inode=12346' \
    '[0000:0000:0000:0000:0000:0000:0000:0000]:8099 LISTEN inode=52691'
  do
    case $out in
      *"$want"*) ;;
      *) bad="$bad
       缺: $want" ;;
    esac
  done
  # ★ 反方向：兩行表頭一行都不許變成結果。只對正方向自證的話，
  #   一個「把每一行都原樣吐出來」的壞實作照樣能全中。
  if [ "$(printf '%s\n' "$out" | grep -c .)" != "3" ]; then
    bad="$bad
       行数不是 3（表头没被挡掉？）"
  fi

  [ -z "$bad" ] || {
    echo "  ⚠ ⚠ /proc/net 解码自测没过 —— 下面那张表不可信：$bad" >&2
    return 1
  }
  return 0
}

# inode → 全部持有它的 pid。★ 一個 socket 掛著兩個 pid 正是「fd 被繼承走了」的現場。
acme_socket_owners() {
  local fd link pid ino
  for fd in /proc/[0-9]*/fd/*; do
    # 這一條只擋「glob 一個都沒匹配上，`$fd` 還是那串字面值」與「處理程序剛好在這一瞬沒了」。
    # ⚠ 別照著「`socket:[N]` 指不到任何路徑，所以要用 `-L`」去理解它 —— **那句是錯的**：
    #   procfs 的這類符號連結是核心造的，`stat()` 直接落到 socket 上，
    #   實測（建置鏡像裡，對一個真的監聽 fd）`-e` 與 `-L` **都為真**。
    #   ⇒ 這裡選 `-L` 是因為要問的本來就是「它是不是一條符號連結」，不是因為 `-e` 會漏。
    [ -L "$fd" ] || continue
    link=$(readlink "$fd" 2>/dev/null) || continue
    case $link in
      'socket:['*']') ;;
      *) continue ;;
    esac
    # ★ 只留數字，不寫 `${link#socket:[}` —— `[` 在參數展開的模式裡是括號表達式的開頭，
    #   一個沒閉合的 `[` 究竟被當成字面值還是模式，靠的是實作的寬容而不是規範。
    ino=${link//[^0-9]/}
    pid=${fd#/proc/}
    pid=${pid%%/*}
    printf '%s %s\n' "$ino" "$pid"
  done
}

# 取證正文。第一個參數進標題（說明是在哪一步紅的）。
acme_dump_ports() {
  local why=$1 a st ino pid owners hold comm state cmd p
  echo "── 取证（$why）：TCP socket 表 ──" >&2
  acme_selftest_decode || true
  # ★ 「讀不到」與「上面什麼都沒有」必須分開說：一張空表被讀成後者，
  #   就正好給出本次要查的那個問題的錯誤答案。
  if [ ! -r /proc/net/tcp ] && [ ! -r /proc/net/tcp6 ]; then
    echo "  ⚠ /proc/net/tcp 与 /proc/net/tcp6 都读不到 —— 下面这张表是空的，" >&2
    echo "    但那说明的是「问不到」，不是「端口上没有东西」。" >&2
  fi
  owners=$(acme_socket_owners 2>/dev/null || true)
  while read -r a st ino; do
    [ -n "$a" ] || continue
    ino=${ino#inode=}
    hold=""
    for pid in $(printf '%s\n' "$owners" | awk -v i="$ino" '$1 == i { print $2 }'); do
      comm=$(cat "/proc/$pid/comm" 2>/dev/null || echo '?')
      hold="$hold $pid($comm)"
    done
    [ -n "$hold" ] || hold=" 无（已经没有 fd 指着它）"
    printf '  %-48s %-12s inode=%-9s 持有者:%s\n' "$a" "$st" "$ino" "$hold" >&2
  done <<EOF
$(acme_decode_proc_net /proc/net/tcp /proc/net/tcp6 2>/dev/null || true)
EOF

  echo "── 取证（$why）：进程表 ──" >&2
  for p in /proc/[0-9]*; do
    [ -r "$p/stat" ] || continue
    # ⚠ `awk '{print $3}'` 在這裡是錯的：comm 帶括號且可以含空格，
    #   狀態字母要從**最後一個** `)` 之後取。
    state=$(sed 's/.*) //' "$p/stat" 2>/dev/null | cut -d' ' -f1)
    # ⚠ 換行也要一起壓掉：一條帶換行的參數會把這張表拆成好幾行，讀起來像是多了幾個處理程序。
    cmd=$(tr '\0\n' '  ' < "$p/cmdline" 2>/dev/null | cut -c1-140)
    [ -n "$cmd" ] || cmd="[$(cat "$p/comm" 2>/dev/null || echo '?')]"
    printf '  pid=%-7s state=%-3s %s\n' "${p#/proc/}" "$state" "$cmd" >&2
  done
}

# 某一個連接埠當下的全部 socket（任意狀態）。只取證，不判定；沒有就回空串。
# ★ 與 `acme_decode_proc_net` 一樣收「要讀哪些檔案」—— 於是**過濾本身也能拿樣本驗**，
#   而不是留一條只有在真出事那天才第一次執行的分支。
# ⚠ 錨 `$` 不可省：`:80$` 與 `:8080` / `:8083` 必須分得開。
acme_port_snapshot() {
  local port=$1
  shift
  [ "$#" -gt 0 ] || set -- /proc/net/tcp /proc/net/tcp6
  acme_decode_proc_net "$@" 2>/dev/null | awk -v p="$port" '$1 ~ (":" p "$")' || true
}

# pebble 自己 HTTPS API 的證書，並把根裝進**系統**信任庫。
#
# ★ 現簽，不進儲存庫：儲存庫裡的測試證書遲早過期，
#   而過期那天紅的是「ACME 壞了」，要繞一圈才發現是證書到期。
#
# ★ ★ ★ **必須是「根 + 葉」兩級，不能拿一張自簽的 CA 證書直接當伺服器端證書。**
#   `tests/serve/run.sh` 裡那張就是自簽 + `CA:TRUE` 一張打天下，curl 收得下——
#   **而 rustls 不收**：`rustls-platform-verifier` 當場判
#   `invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))`，
#   現場表現是「建 ACME 帳戶失敗：client error (Connect)」，看不出跟證書有關。
#   ⚠ 這正是「判據只認得一種形狀」的又一次：拿 curl 驗一遍會全綠，
#   而**真正要連上 pebble 的是產品裡的 rustls 用戶端**，不是 curl。
#   ★ 順帶記一條：`extendedKeyUsage=serverAuth` 也不是裝飾，rustls 會看它。
#
# ★ ★ 裝進**系統**信任庫，因為產品程式碼走的是平台信任庫。
#   ⚠ 這一步要 root。裝不上要當場紅：一個「沒能裝信任根」卻繼續跑的指令碼，
#   最後只會得到一句語焉不詳的「CA 連不上」。
acme_make_api_cert() {
  local scenario=$1
  openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
    -keyout "$WORK/ca.key" -out "$WORK/ca.crt" \
    -subj "/CN=Fulcrum Gate Test CA" \
    -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    >/dev/null 2>&1 || {
    echo "$scenario FAILED: openssl 生成测试根证书失败" >&2
    exit 1
  }
  cat > "$WORK/leaf.ext" <<'EXT'
subjectAltName=DNS:localhost,IP:127.0.0.1
basicConstraints=critical,CA:FALSE
extendedKeyUsage=serverAuth
EXT
  # ⚠ 兩步**分開寫**，不要 `A && B || { 报错; }`。shellcheck 的 SC2015 就是衝它來的，
  #   而本儲存庫在 G44 上已經被 `&&`/`||` 同優先級坑過一次真的假綠——
  #   那次的教訓不是「注意點」，是**別再寫這個形狀**。
  openssl req -newkey rsa:2048 -nodes -sha256 \
    -keyout "$WORK/pebble.key" -out "$WORK/pebble.csr" -subj "/CN=localhost" \
    >/dev/null 2>&1 || {
    echo "$scenario FAILED: openssl 生成 pebble 的 API 私钥/CSR 失败" >&2
    exit 1
  }
  openssl x509 -req -in "$WORK/pebble.csr" -sha256 -days 2 \
    -CA "$WORK/ca.crt" -CAkey "$WORK/ca.key" -CAcreateserial \
    -extfile "$WORK/leaf.ext" -out "$WORK/pebble.crt" \
    >/dev/null 2>&1 || {
    echo "$scenario FAILED: openssl 给 pebble 的 API 签服务端证书失败" >&2
    exit 1
  }
  cp "$WORK/ca.crt" "$TRUST_ANCHOR"
  update-ca-certificates >/dev/null 2>&1 || {
    echo "$scenario FAILED: update-ca-certificates 失败（要 root）" >&2
    exit 1
  }
}

# 寫 pebble 的設定。第五個參數是**可選**的一段額外 JSON（末尾自帶逗號），
# 續期場景用它塞 `profiles`（把證書壽命調到分鐘級）。
#
# ⚠ ⚠ 別改用 `certificateValidityPeriod`：**它在 pebble v2.10.1 裡是死設定**——
#   解析進結構體了，但 CA 只認 profile。實測：配 600 之後啟動紀錄仍然是
#   `Loaded profile "default" with certificate validity period of 7776000 seconds`。
#   ★ 這一條是量出來的；照著字面意思讀設定結構體會得出相反的結論。
acme_write_pebble_config() {
  local dir_port=$1 mgmt_port=$2 http_port=$3 tlsalpn_port=$4 extra=${5:-}
  cat > "$WORK/pebble.json" <<PEBBLECONF
{
  "pebble": {
    "listenAddress": "$HOST:$dir_port",
    "managementListenAddress": "$HOST:$mgmt_port",
    "certificate": "$WORK/pebble.crt",
    "privateKey": "$WORK/pebble.key",
    "httpPort": $http_port,
    "tlsPort": $tlsalpn_port,
    "ocspResponderURL": "",
    "externalAccountBindingRequired": false,
    "domainBlocklist": ["blocked-domain.example"],
    $extra
    "retryAfter": { "authz": 1, "order": 1 }
  }
}
PEBBLECONF
}

# ★ ★ `-defaultIPv6 ''` **不是可選的**：challtestsrv 預設同時回 A 與 AAAA（`::1`），
#   而這裡的樞衡綁在 127.0.0.1 上 —— pebble 走 v6 來驗就連不上，
#   而現場只有一句「CA 說驗不過」。
# ★ 它自帶的挑戰伺服器（-http01/-https01/-tlsalpn01）**必須全部關掉**：
#   應答挑戰的人是樞衡，不是它。留著只會讓「誰答的」變得說不清。
acme_start_challtestsrv() {
  local dns_port=$1 mgmt_port=$2
  pebble-challtestsrv \
    -management "$HOST:$mgmt_port" \
    -dnsserver "$HOST:$dns_port" \
    -defaultIPv4 "$HOST" \
    -defaultIPv6 '' \
    -http01 '' -https01 '' -tlsalpn01 '' -doh '' \
    > "$WORK/challtestsrv.log" 2>&1 &
  AUX_PIDS+=($!)
}

# ★ `PEBBLE_VA_NOSLEEP=1`：pebble 預設在驗證前隨機睡幾秒（模擬真實 CA 的延遲）。
#   門禁裡那是純粹的等待。
#
# ★ ★ **`PEBBLE_WFE_NONCEREJECT` 留在 pebble 自己的預設值 5（%），是有意的，
#   而這個決定是**算出來的**，不是「跑幾次沒看見」。**
#
#   它是 pebble 用來逼用戶端實作 `badNonce` 重試的開關，而**真 CA 就是這麼幹的**
#   （RFC 8555 §6.5 要求用戶端拿一個新 nonce 重試）。關掉它，門禁就再也證不了這條路。
#   ⚠ 但留著一個隨機失敗源，就必須先算清楚它多久會把門變成一次偶發紅：
#
#   · `instant-acme` 0.8.5 的重試是**有上限的**：`lib.rs` 的 `post()` 裡
#     `let mut retries = 3`，即「首次 + 2 次重試」，三次全被拒就把 400 回傳給呼叫方。
#     ★ 這一條是**讀原始碼讀出來的**，不是從行為猜的——猜出來的重試次數會讓下面的估算
#     整個建在沙子上。
#   · 於是一次 POST 永久失敗的機率是 p³，一輪簽發大約十來次 POST。
#   · 實測（本機）：`NONCE_REJECT=50` 跑 5 次，**4 次綠、1 次紅**
#     （紅在建帳戶那一步，紀錄裡是 `POST /sign-me-up` 連著三發）。
#   · 按 p³ 縮放回預設的 5%：0.2 × (0.05/0.5)³ ≈ **兩萬分之一**量級。
#     ⚠ 這是估算不是實測——真要實測 1e-4 得跑上萬次，那個代價不值。
#   要重現那次測量：`NONCE_REJECT=50 bash tests/acme/run.sh`。
#
# 第二個參數是可選的 `PEBBLE_AUTHZREUSE`（百分比）。續期場景把它設成 0，
# ⚠ 否則 pebble 有 50% 的機率重複使用上一次的授權，於是**續期那一趟根本不走挑戰**——
#   一條「續期時 TXT 又寫了一遍」的斷言會變成擲硬幣。
# ⚠ ⚠ 0 也**不是**「永不復用」，而是 1/100（pebble 的差一，原文見 renew.sh 判據四）⇒
#   那條斷言不靠這個參數，而是按 CA 紀錄裡這一趟走了哪一支來判。
acme_start_pebble() {
  local dns_port=$1 authz_reuse=${2:-}
  local -a env_args=(PEBBLE_VA_NOSLEEP=1 "PEBBLE_WFE_NONCEREJECT=${NONCE_REJECT:-5}")
  [ -z "$authz_reuse" ] || env_args+=("PEBBLE_AUTHZREUSE=$authz_reuse")
  env "${env_args[@]}" pebble \
    -config "$WORK/pebble.json" \
    -dnsserver "$HOST:$dns_port" \
    -strict \
    > "$WORK/pebble.log" 2>&1 &
  AUX_PIDS+=($!)
}

# 取 pebble 的根證書（供 curl `--cacert` 用）。落在 `$WORK/pebble-root.pem`。
acme_fetch_root() {
  local scenario=$1 mgmt_port=$2
  run_curl -fsS --max-time 10 "https://localhost:${mgmt_port}/roots/0" -o "$WORK/pebble-root.pem"
  if ! grep -q 'BEGIN CERTIFICATE' "$WORK/pebble-root.pem" 2>/dev/null; then
    echo "$scenario FAILED: 取不到 pebble 的根证书" >&2
    exit 1
  fi
}

# ── DNS-01 的 exec hook（G57）────────────────────────────────────────────────
#
# ★ 呼叫約定是產品定的：`<程序> <set|clear> <记录名> <值>`。
#   這裡讓它去改 challtestsrv 的記錄，而 **challtestsrv 同時就是 pebble 用的那臺權威 NS**
#   （pebble 起來時帶了 `-dnsserver`）——於是「寫進去」與「CA 查得到」是同一份資料，
#   不是兩個互相假設的替身。
# ⚠ 用 `printf '%s'` 拼 JSON 而不是 echo：值裡有 base64url 的 `-` 與 `_`，
#   而 echo 對反斜槓的處理隨 shell 而變。
acme_write_dns_hook() {
  local path=$1 cts_port=$2
  cat > "$path" <<'HOOKEOF'
#!/bin/sh
set -eu
action=$1
name=$2
value=$3
case "$action" in
  set)   printf '{"host":"%s","value":"%s"}' "$name" "$value" \
           | curl -fsS -X POST -H 'Content-Type: application/json' \
                  --data @- "http://127.0.0.1:CTSPORT/set-txt" >/dev/null ;;
  clear) printf '{"host":"%s"}' "$name" \
           | curl -fsS -X POST -H 'Content-Type: application/json' \
                  --data @- "http://127.0.0.1:CTSPORT/clear-txt" >/dev/null ;;
  *) echo "unknown action $action" >&2; exit 2 ;;
esac
HOOKEOF
  sed -i "s/CTSPORT/$cts_port/g" "$path"
  chmod 0755 "$path"
}
