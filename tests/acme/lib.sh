#!/usr/bin/env bash
# shellcheck shell=bash
#
# ACME 两个端到端场景共用的那一半：[`run.sh`](run.sh)（签发）与 [`renew.sh`](renew.sh)（续期）。
#
# ★ ★ **为什么抽出来，而不是抄一份。**
#   两个场景都要「起一个真 CA（pebble）+ 一台假权威 NS（challtestsrv）+ 一个 exec hook」，
#   那是一百多行、且每一行都带着一条踩坑记录（v6 回环、两级证书、SIGTERM vs SIGINT…）。
#   本仓库在「同一个判定抄了两份」上反复栽过——D18 那次的处置就是**让分家在结构上做不到**
#   （两侧共用 `wildcard_covers` 一份实现）。抄一份 pebble 起法，等于给下一个人埋一处必然分叉：
#   改了一边、另一边继续绿，而绿的那边测的已经不是同一个东西了。
#
# 调用方在 source 之前必须设好：`WORK`（工作目录）、`HOST`、`TRUST_ANCHOR`（信任根落点）。

# 失败计数与进程表。★ 枢衡的进程收 SIGINT，别的（pebble / challtestsrv）收 SIGTERM——
# 见 `acme_cleanup` 里那段注释，那是量出来的不是洁癖。
FAILS=0
PIDS=()
AUX_PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

# 收尸：先招呼、再等、最后补刀；然后把信任根摘掉。
#
# ★ 不是洁癖：Pingora 把 SIGTERM 当**优雅停机**，会等完整的排空窗口（每次跑白等几秒）；
#   而 pebble 与 challtestsrv 是 Go 程序，**根本不理 SIGINT 之外的期待**——
#   对所有进程一律 SIGINT 的话，pebble 每次都要挨一发 SIGKILL，
#   bash 就在收工时打一行 `Killed`，读起来像是出了事。
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
  # ★ 把信任库改回去。容器是 --rm 的，但这些脚本也可能被人在别处跑，
  #   而「往系统信任库里塞了一张自签根然后不收拾」是一件不该留下的事。
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

# 等某个文件里出现某段文本。★ 用「日志里出现了什么」当同步点，而不是 `sleep N`：
#   固定 sleep 要么慢、要么在慢一点的机器上变成偶发红。
wait_log() {
  local file=$1 pattern=$2 limit=${3:-60} waited=0
  while [ "$waited" -lt "$((limit * 4))" ]; do
    # ★ ★ `-F`（固定字符串）**不是可选的**：要等的那行里有 `*.wild.example`，
    #   而 `*` 在正则里是元字符 —— `成功：*.wild.example` 会被读成
    #   「零个或多个『：』+ 任意字符 + wild…」，于是**永远匹配不上一行确实存在的日志**。
    #   ⚠ 症状极具误导性：门报「120s 内没等到签发成功」，而日志里那一行明明就在那儿。
    if [ -f "$file" ] && grep -qF "$pattern" "$file"; then return 0; fi
    sleep 0.25
    waited=$((waited + 1))
  done
  return 1
}

# 等某个文件里某段文本出现**第 N 次**。续期场景要的判据是「又签了一张」，
# 而「签发成功」这句在第一次签发时就已经出现过一遍。
# ⚠ 只等「出现过」会立刻命中第一次那条，于是一道本该验续期的门变成恒绿。
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

# ⚠ `|| true` 是必需的：`set -e` 之下，赋值里的命令替换失败会让整个脚本退出。
run_curl() { curl "$@" 2>/dev/null || true; }

# ★ ★ 要**退出码**时不能走命令替换（子 shell 里设的变量传不回来）。
CURL_RC=0
# shellcheck disable=SC2034  # CURL_RC 由 source 本文件的场景脚本读，跨文件 shellcheck 看不见
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

# 文件权限判据。私钥必须是 0600（G55）。
expect_mode() {
  local what=$1 want=$2 path=$3 got
  if [ ! -e "$path" ]; then
    fail "$what：$path 不存在"
    return
  fi
  got=$(stat -c '%a' "$path")
  if [ "$got" = "$want" ]; then ok "$what 权限 $got"; else fail "$what 权限期望 $want，实际 $got"; fi
}

# 镜像里那几个外部程序在不在（G64 装的那套）。第一个参数是场景名，只进报错文本。
acme_require_tools() {
  local scenario=$1 one
  for one in pebble pebble-challtestsrv openssl curl update-ca-certificates; do
    command -v "$one" >/dev/null 2>&1 || {
      echo "$scenario FAILED: 镜像里没有 $one —— 这个场景要 docker/Dockerfile.build 里那套（G64）" >&2
      exit 1
    }
  done
}

# ★ ★ 这一步不是形式：本仓库栽过一次「基线探针对着上一个场景遗留的进程报绿」。
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

# ── 失败现场取证：那一刻谁占着哪些端口 ──────────────────────────────────
#
# ⛔ **只在已经要红的路径上调用**：它一行都不参与判定，
#   因此不可能把一趟本来能过的跑判红。
#
# ★ ★ ★ **它第一次跑就把那次间歇红的根因指出来了，而现有日志一个字都答不出**：
#   上一个场景（quic-relay）漏了一个进程，它攥着**合成出来的 `:80`**（见 AGENTS.md 端口表）；
#   pingora 在**持有全局 `ListenFds` 锁**的状态下做那个会重试 30 秒的 bind
#   ⇒ 排在后面的 `:8083` 跟着起不来，而日志里只有 `:80` 的错。
#   ⇒ **报出来的端口不是出事的端口**，靠读日志永远绕不出来。
#   ★ 源头已经在 quic-relay 自己的 `cleanup` 里堵上（它走时要把用过的端口还回去）。
#
# ⚠ 镜像里**没有 `ss` / `lsof` / `fuser`**（iproute2 只装在 systemd 测试镜像里）
#   ⇒ 只能读 `/proc/net/tcp{,6}` 与 `/proc/<pid>/fd`。
#
# ★ ★ 打的是**全部状态**，不只是 LISTEN：`bind()` 报 `EADDRINUSE` 那一刻，
#   「另一个进程正在 LISTEN」与「同一个 addr:port 上挂着一条残留连接」是两个不同的答案，
#   而只打 LISTEN 的话后者会表现成「什么都没有」—— 一句最会把人带偏的结论。
#
# ★ ★ ★ 同一个 inode 的**全部**持有者都要打：一个被子进程继承走的监听 fd，
#   现场就是「两个 pid 指着同一个 socket」，而进程表里那个 fulcrum 已经不在了。
#   这个形状本仓库栽过一次，常设判据是 [`tests/m0/unclaimed.sh`](../m0/unclaimed.sh)。

# `/proc/net/tcp{,6}` → 每行一个 `地址:端口 状态 inode=N`（v6 写成 `[地址]:端口`）。
# 参数是要读的文件 —— 自测时指向样本，于是**同一份解码**两处共用。
# ⚠ `strtonum()` 是 gawk 扩展，而镜像里的 awk 是 mawk ⇒ 十六进制只能自己拆。
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

# 解码自测。★ 它**不是形式**：一个恒空的取证函数，在最该说话的那一刻是沉默的，
# 而沉默会被读成「端口上什么都没有」—— 那正好是本次要查的那个问题的错误答案。
# ⚠ 只在取证路径上跑（一次几毫秒），绿的跑一个字都不多。
#
# ★ 样本是**真实内核输出**：v4 两行与 v6 那行都是 `tests/m0/unclaimed.sh` 在构建镜像里
#   实抓下来的那一份（它同时是那处 v6 盲区的实物证据），这里原样引用。
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
  # ★ 反方向：两行表头一行都不许变成结果。只对正方向自证的话，
  #   一个「把每一行都原样吐出来」的坏实现照样能全中。
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

# inode → 全部持有它的 pid。★ 一个 socket 挂着两个 pid 正是「fd 被继承走了」的现场。
acme_socket_owners() {
  local fd link pid ino
  for fd in /proc/[0-9]*/fd/*; do
    # 这一条只挡「glob 一个都没匹配上，`$fd` 还是那串字面量」与「进程刚好在这一瞬没了」。
    # ⚠ 别照着「`socket:[N]` 指不到任何路径，所以要用 `-L`」去理解它 —— **那句是错的**：
    #   procfs 的这类符号链接是内核造的，`stat()` 直接落到 socket 上，
    #   实测（构建镜像里，对一个真的监听 fd）`-e` 与 `-L` **都为真**。
    #   ⇒ 这里选 `-L` 是因为要问的本来就是「它是不是一条符号链接」，不是因为 `-e` 会漏。
    [ -L "$fd" ] || continue
    link=$(readlink "$fd" 2>/dev/null) || continue
    case $link in
      'socket:['*']') ;;
      *) continue ;;
    esac
    # ★ 只留数字，不写 `${link#socket:[}` —— `[` 在参数展开的模式里是括号表达式的开头，
    #   一个没闭合的 `[` 究竟被当成字面量还是模式，靠的是实现的宽容而不是规范。
    ino=${link//[^0-9]/}
    pid=${fd#/proc/}
    pid=${pid%%/*}
    printf '%s %s\n' "$ino" "$pid"
  done
}

# 取证正文。第一个参数进标题（说明是在哪一步红的）。
acme_dump_ports() {
  local why=$1 a st ino pid owners hold comm state cmd p
  echo "── 取证（$why）：TCP socket 表 ──" >&2
  acme_selftest_decode || true
  # ★ 「读不到」与「上面什么都没有」必须分开说：一张空表被读成后者，
  #   就正好给出本次要查的那个问题的错误答案。
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
    # ⚠ `awk '{print $3}'` 在这里是错的：comm 带括号且可以含空格，
    #   状态字母要从**最后一个** `)` 之后取。
    state=$(sed 's/.*) //' "$p/stat" 2>/dev/null | cut -d' ' -f1)
    # ⚠ 换行也要一起压掉：一条带换行的参数会把这张表拆成好几行，读起来像是多了几个进程。
    cmd=$(tr '\0\n' '  ' < "$p/cmdline" 2>/dev/null | cut -c1-140)
    [ -n "$cmd" ] || cmd="[$(cat "$p/comm" 2>/dev/null || echo '?')]"
    printf '  pid=%-7s state=%-3s %s\n' "${p#/proc/}" "$state" "$cmd" >&2
  done
}

# 某一个端口当下的全部 socket（任意状态）。只取证，不判定；没有就回空串。
# ★ 与 `acme_decode_proc_net` 一样收「要读哪些文件」—— 于是**过滤本身也能拿样本验**，
#   而不是留一条只有在真出事那天才第一次执行的分支。
# ⚠ 锚 `$` 不可省：`:80$` 与 `:8080` / `:8083` 必须分得开。
acme_port_snapshot() {
  local port=$1
  shift
  [ "$#" -gt 0 ] || set -- /proc/net/tcp /proc/net/tcp6
  acme_decode_proc_net "$@" 2>/dev/null | awk -v p="$port" '$1 ~ (":" p "$")' || true
}

# pebble 自己 HTTPS API 的证书，并把根装进**系统**信任库。
#
# ★ 现签，不进仓库：仓库里的测试证书迟早过期，
#   而过期那天红的是「ACME 坏了」，要绕一圈才发现是证书到期。
#
# ★ ★ ★ **必须是「根 + 叶」两级，不能拿一张自签的 CA 证书直接当服务端证书。**
#   `tests/serve/run.sh` 里那张就是自签 + `CA:TRUE` 一张打天下，curl 收得下——
#   **而 rustls 不收**：`rustls-platform-verifier` 当场判
#   `invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))`，
#   现场表现是「建 ACME 账户失败：client error (Connect)」，看不出跟证书有关。
#   ⚠ 这正是「判据只认得一种形状」的又一次：拿 curl 验一遍会全绿，
#   而**真正要连上 pebble 的是产品里的 rustls 客户端**，不是 curl。
#   ★ 顺带记一条：`extendedKeyUsage=serverAuth` 也不是装饰，rustls 会看它。
#
# ★ ★ 装进**系统**信任库，因为产品代码走的是平台信任库。
#   ⚠ 这一步要 root。装不上要当场红：一个「没能装信任根」却继续跑的脚本，
#   最后只会得到一句语焉不详的「CA 连不上」。
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
  # ⚠ 两步**分开写**，不要 `A && B || { 报错; }`。shellcheck 的 SC2015 就是冲它来的，
  #   而本仓库在 G44 上已经被 `&&`/`||` 同优先级坑过一次真的假绿——
  #   那次的教训不是「注意点」，是**别再写这个形状**。
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

# 写 pebble 的配置。第五个参数是**可选**的一段额外 JSON（末尾自带逗号），
# 续期场景用它塞 `profiles`（把证书寿命调到分钟级）。
#
# ⚠ ⚠ 别改用 `certificateValidityPeriod`：**它在 pebble v2.10.1 里是死配置**——
#   解析进结构体了，但 CA 只认 profile。实测：配 600 之后启动日志仍然是
#   `Loaded profile "default" with certificate validity period of 7776000 seconds`。
#   ★ 这一条是量出来的；照着字面意思读配置结构体会得出相反的结论。
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

# ★ ★ `-defaultIPv6 ''` **不是可选的**：challtestsrv 默认同时回 A 与 AAAA（`::1`），
#   而这里的枢衡绑在 127.0.0.1 上 —— pebble 走 v6 来验就连不上，
#   而现场只有一句「CA 说验不过」。
# ★ 它自带的挑战服务器（-http01/-https01/-tlsalpn01）**必须全部关掉**：
#   应答挑战的人是枢衡，不是它。留着只会让「谁答的」变得说不清。
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

# ★ `PEBBLE_VA_NOSLEEP=1`：pebble 默认在验证前随机睡几秒（模拟真实 CA 的延迟）。
#   门禁里那是纯粹的等待。
#
# ★ ★ **`PEBBLE_WFE_NONCEREJECT` 留在 pebble 自己的默认值 5（%），是有意的，
#   而这个决定是**算出来的**，不是「跑几次没看见」。**
#
#   它是 pebble 用来逼客户端实现 `badNonce` 重试的开关，而**真 CA 就是这么干的**
#   （RFC 8555 §6.5 要求客户端拿一个新 nonce 重试）。关掉它，门禁就再也证不了这条路。
#   ⚠ 但留着一个随机失败源，就必须先算清楚它多久会把门变成一次偶发红：
#
#   · `instant-acme` 0.8.5 的重试是**有上限的**：`lib.rs` 的 `post()` 里
#     `let mut retries = 3`，即「首次 + 2 次重试」，三次全被拒就把 400 返回给调用方。
#     ★ 这一条是**读源码读出来的**，不是从行为猜的——猜出来的重试次数会让下面的估算
#     整个建在沙子上。
#   · 于是一次 POST 永久失败的概率是 p³，一轮签发大约十来次 POST。
#   · 实测（本机）：`NONCE_REJECT=50` 跑 5 次，**4 次绿、1 次红**
#     （红在建账户那一步，日志里是 `POST /sign-me-up` 连着三发）。
#   · 按 p³ 缩放回默认的 5%：0.2 × (0.05/0.5)³ ≈ **两万分之一**量级。
#     ⚠ 这是估算不是实测——真要实测 1e-4 得跑上万次，那个代价不值。
#   要复现那次测量：`NONCE_REJECT=50 bash tests/acme/run.sh`。
#
# 第二个参数是可选的 `PEBBLE_AUTHZREUSE`（百分比）。续期场景把它设成 0，
# ⚠ 否则 pebble 有 50% 的概率复用上一次的授权，于是**续期那一趟根本不走挑战**——
#   一条「续期时 TXT 又写了一遍」的断言会变成掷硬币。
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

# 取 pebble 的根证书（供 curl `--cacert` 用）。落在 `$WORK/pebble-root.pem`。
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
# ★ 调用约定是产品定的：`<程序> <set|clear> <记录名> <值>`。
#   这里让它去改 challtestsrv 的记录，而 **challtestsrv 同时就是 pebble 用的那台权威 NS**
#   （pebble 起来时带了 `-dnsserver`）——于是「写进去」与「CA 查得到」是同一份数据，
#   不是两个互相假设的替身。
# ⚠ 用 `printf '%s'` 拼 JSON 而不是 echo：值里有 base64url 的 `-` 与 `_`，
#   而 echo 对反斜杠的处理随 shell 而变。
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
