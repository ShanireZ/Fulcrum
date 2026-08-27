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
