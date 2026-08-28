#!/usr/bin/env bash
# ACME 续期端到端：让**跑着的**枢衡自己走到续期，真重签一张**通配符**证书。
#
# ★ **它补的是 G58 的另一半。** G58 写的是「至少签发**并续期**一次通配符证书」，
#   而 `tests/acme/run.sh` 只验到「第二轮判**已是最新**」—— 那恰恰是「**没有**续期」的判据。
#   ⚠ 「暂时不用续」与「该续时会续」是两件相反的事，前者绿一万次也证不出后者。
#
# ★ ★ **判据挂在方法上，不只挂在结果上**：一张新证书出现在盘上，可能来自「按 ARI 续期」，
#   也可能来自「旧的没读出来于是重签」。所以同时钉三样：① 日志里那句
#   `该续期了：CA 的 ARI 建议窗口已经开始`（**谁下的决定**）、② 证书**序列号变了**、
#   ③ 续期那一趟**又走了一遍 DNS-01**。
#
# 单独一个场景 + 单独一套端口 + 单独一个 pebble 实例：短寿命证书会污染同一个 CA 下的其它
#   断言（run.sh 有好几条依赖「刚签完的证书还早得很」）。共用一个 CA 等于让两个场景互相设定前提。
#
# 拓扑（端口全部与 run.sh 错开，见 AGENTS.md 的端口表）：
#
#   pebble-challtestsrv  :8054(DNS) :8056(管理)
#   pebble               :14001(目录) :15001(管理)   —— profile 把证书寿命压到 ~1.5 天
#   fulcrum serve        :8084(HTTP)  :9445(HTTPS)
#
# ★ ★ ★ **三条反证（实做，每条都改产品代码、跑一遍、再改回去）**：
#
#   | 扰动 | 结果 |
#   |---|---|
#   | ARI 那一支 `renew_now: false`（假装分支被改坏）| 红在「没等到该续期」+「没等到签发成功」；日志里它每 60s 判一次「已是最新」——**正是这道门要抓的那种无声地永不续期** |
#   | 签完不 `install`（假装忘了热装）| **只红一条**：线上握手拿到的是**旧**序列号。⚠ 盘上换了、HTTPS 仍然 200 —— 一道只验「还能访问」或「盘上变了」的门在这里全绿 |
#   | 判定改成恒真 | 红在 [3/5] 那两条反向断言。其余全绿 —— 也就是说**一道只断言「续期发生了」的门，对「每 60s 重签一次」是瞎的**，而那在真 CA 上直接烧掉速率配额 |
#
#   ★ 第三条还量出一件事：判「该续」时 `next_check` 是 `MIN_CHECK`(60s)，
#   与签发后那条 3600s 取 min 之后仍是 60s。真实证书上这只是「续完 60s 后多看一眼」，
#   无害；但它说明「续期后多久再看」这条路是**由续期判定而不是签发结果**决定的。
#
# 用法：
#   bash tests/acme/renew.sh              # 容器里跑（tests/m0/docker-run.sh 会驱动它）
#   ACME_KEEP=1 bash tests/acme/renew.sh  # 失败时保留工作目录
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1

DIR_PORT=${DIR_PORT:-14001}
MGMT_PORT=${MGMT_PORT:-15001}
DNS_PORT=${DNS_PORT:-8054}
CTS_MGMT_PORT=${CTS_MGMT_PORT:-8056}
HTTP_PORT=${HTTP_PORT:-8084}
TLS_PORT=${TLS_PORT:-9445}
PEBBLE_TLSALPN_PORT=${PEBBLE_TLSALPN_PORT:-9446}

# ★ ★ ★ **这个数字是量出来的，不是猜的。改它之前先读完这一段。**
#
# 实测（pebble v2.10.1）：ARI 建议窗口 =
#   `[notAfter − 寿命/3 − 1 天, notAfter − 寿命/3 + 1 天]`
# —— **那个 ±1 天是绝对值，不随寿命缩放**。两个量级各验过一次：
#   · 90 天证书：窗口在寿命的 65%–67%，宽度正好 2 天；
#   · 129700s 证书：预测 `2L/3 − 86400 = 66s`，实测 **65s**（差 1s 来自证书只精确到秒）。
#
# 于是「ARI 窗口开始的时刻」= notBefore + (2L/3 − 86400)。要让它落在**签发之后几十秒**，
# L 就得在 129600s（1.5 天）出头一点点。取 129700 ⇒ 窗口在签发后约 65s 开始。
#
# ⚠ ⚠ 为什么不干脆取几分钟的寿命：那样 `2L/3 − 86400` 是**负数**，
#   ARI 窗口在证书签发之前就已经开始了，于是第二代**第一轮巡检就直接续**——
#   跑得更快，但**证不到「它自己等到了时候才动」**，而那正是续期的全部内容。
#   ★ 同一族的教训：一个恒真的判据与一个正确的判据无法区分。
#
# ⚠ 另一个下界：`fulcrum_tls::renewal::MIN_CHECK` 是 60s，巡检不会睡得比它更短。
#   窗口若在 65s 处开始，实际就是「睡 ~63s 再回来」——本场景的用时基本就是这 65s。
VALIDITY=${VALIDITY:-129700}
# 等续期最多等多久。★ 比 65s 宽出一大截：这里宁可慢，也不要在慢一点的机器上偶发红。
RENEW_WAIT=${RENEW_WAIT:-180}

DIR_URL="https://localhost:${DIR_PORT}/dir"
ISSUER_DIR="ca-localhost-${DIR_PORT}"

# ★ 被续的是**通配符**，因为 G58 要的就是通配符那一张。顺带：通配符只能走 DNS-01。
WILDCARD_SITE='*.renew.example'
WILDCARD_HOST=a.renew.example
STATE="$WORK/state"
# ★ `*` 在存储里被转义成 `_wildcard_`（见 fulcrum-tls 的 sanitize/unsanitize）。
CERT_DIR="$STATE/certs/$ISSUER_DIR/_wildcard_.renew.example"
META="$CERT_DIR/meta.json"
HOOK="$WORK/dns-hook.sh"
TRUST_ANCHOR=/usr/local/share/ca-certificates/fulcrum-pebble-renew.crt

# shellcheck source=tests/acme/lib.sh
. "$(dirname "$0")/lib.sh"

cleanup() {
  acme_cleanup
  if [ "${ACME_KEEP:-0}" = "1" ] && [ "$FAILS" -ne 0 ]; then
    echo "ACME_KEEP=1：工作目录留在 $WORK" >&2
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

# 从 PEM 里取序列号 / 起止时刻。★ 判据里的「变了没有」全靠它们。
cert_serial() { openssl x509 -in "$1" -noout -serial | cut -d= -f2; }
cert_epoch() {
  # $2 是 -startdate 或 -enddate
  date -d "$(openssl x509 -in "$1" -noout "$2" | cut -d= -f2)" +%s
}

echo "=== [0/5] 基线：端口是空的、存储里还没有证书 ==="

acme_require_tools "ACME RENEW TESTS"
[ -x "$BIN" ] || {
  echo "ACME RENEW TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}
acme_require_ports_free "ACME RENEW TESTS" \
  "$DIR_PORT" "$MGMT_PORT" "$DNS_PORT" "$CTS_MGMT_PORT" "$HTTP_PORT" "$TLS_PORT"
ok "六个端口都是空的"

if [ -e "$CERT_DIR/cert.pem" ]; then
  echo "ACME RENEW TESTS FAILED: $CERT_DIR/cert.pem 一开始就存在" >&2
  exit 1
fi
ok "存储里还没有 $WILDCARD_SITE 的证书"

echo "=== [1/5] 起一个**证书寿命被压短**的 pebble ==="

acme_make_api_cert "ACME RENEW TESTS"
# ⚠ ⚠ 生效的是 `profiles`，**不是** `certificateValidityPeriod`（后者在 v2.10.1 里是死配置，
#   实测详见 lib.sh 里 `acme_write_pebble_config` 上面那一段）。
# ⚠ profile 必须叫 `default`：实测另起名字之后，目录里就没有 default 了，
#   而客户端不选 profile 时没有兜底。
acme_write_pebble_config "$DIR_PORT" "$MGMT_PORT" "$HTTP_PORT" "$PEBBLE_TLSALPN_PORT" \
  "\"profiles\": { \"default\": { \"description\": \"short-lived\", \"validityPeriod\": $VALIDITY } },"
acme_start_challtestsrv "$DNS_PORT" "$CTS_MGMT_PORT"
# ★ ★ `PEBBLE_AUTHZREUSE=0`：pebble 默认有 50% 概率复用上一次的授权，
#   那样**续期那一趟根本不走挑战**，下面那条「续期时又验了一遍 DNS-01」会变成掷硬币。
#   ⚠ 关掉它换来的是判据确定，而不是把一条真实路径藏起来——授权复用是 CA 侧的优化，
#   与「枢衡会不会重新解一次挑战」无关。
acme_start_pebble "$DNS_PORT" 0

wait_port "$DIR_PORT" || {
  echo "ACME RENEW TESTS FAILED: pebble 的目录端口 $DIR_PORT 起不来。日志：" >&2
  cat "$WORK/pebble.log" >&2
  exit 1
}
wait_port "$MGMT_PORT" || {
  echo "ACME RENEW TESTS FAILED: pebble 的管理端口 $MGMT_PORT 起不来" >&2
  exit 1
}

# ★ ★ **自证第一半：短寿命真的生效了。**
#   少了这一条，一个「profile 没被认出来」的 pebble 会照常签出 90 天的证书，
#   而本场景的表现是「等了 180s 没等到续期」——那条报错会把人引向续期逻辑，
#   而真正坏掉的是这一行配置。⚠ 把「前提没成立」误报成「被测对象坏了」，
#   比直接红更贵：它会让人去改一段本来是对的代码。
if grep -qF "certificate validity period of $VALIDITY seconds" "$WORK/pebble.log"; then
  ok "pebble 的 default profile 寿命 = ${VALIDITY}s（短寿命配置真的生效了）"
else
  echo "ACME RENEW TESTS FAILED: pebble 没有按 ${VALIDITY}s 装载 profile —— 短寿命配置没生效。" >&2
  grep -F 'Loaded profile' "$WORK/pebble.log" >&2 || true
  exit 1
fi

DIR_JSON=$(run_curl -fsS --max-time 10 "$DIR_URL")
if [ -z "$DIR_JSON" ]; then
  echo "ACME RENEW TESTS FAILED: 取不到 ACME 目录 $DIR_URL（多半是信任根没装上）" >&2
  cat "$WORK/pebble.log" >&2
  exit 1
fi
# ⚠ 这里**不能像 run.sh 那样「没有 renewalInfo 就跳过」**：本场景要验的续期
#   正是由 ARI 触发的。CA 不给 ARI，这个场景就什么也证不了，那必须是红，不是跳过。
#   ★ 「跳过」写成「绿」，是把一道门悄悄拆掉。
if printf '%s' "$DIR_JSON" | grep -q 'renewalInfo'; then
  ok "CA 目录里有 renewalInfo（ARI 是本场景的触发机制，缺了它就白跑）"
else
  echo "ACME RENEW TESTS FAILED: 这个 CA 的目录里没有 renewalInfo —— 续期场景无法成立" >&2
  exit 1
fi

acme_fetch_root "ACME RENEW TESTS" "$MGMT_PORT"
ok "取到 pebble 的根证书"
acme_write_dns_hook "$HOOK" "$CTS_MGMT_PORT"

echo "=== [2/5] 第一代：把通配符签下来 ==="

cat > "$WORK/renew.Fulcrumfile" <<CONF
{
    acme_ca $DIR_URL
    acme_email fulcrum-renew-gate@example.invalid
}

http://$WILDCARD_HOST:$HTTP_PORT {
    respond 403
}

$WILDCARD_SITE:$TLS_PORT {
    tls {
        dns exec HOOKPATH
        resolvers $HOST:$DNS_PORT
    }
    respond 200 "renew-ok"
}
CONF
sed -i "s|HOOKPATH|$HOOK|" "$WORK/renew.Fulcrumfile"

start_fulcrum() {
  local name=$1
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/renew.Fulcrumfile" \
    --bind-host "$HOST" \
    --state-dir "$STATE" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
  FULCRUM_PID=$!
}

start_fulcrum gen1
wait_port "$TLS_PORT" || {
  echo "ACME RENEW TESTS FAILED: 第一代起不来。日志：" >&2
  cat "$WORK/gen1.log" >&2
  acme_dump_ports "第一代起不来"
  exit 1
}

if ! wait_log "$WORK/gen1.log" "ACME 签发成功：$WILDCARD_SITE" 120; then
  echo "ACME RENEW TESTS FAILED: 120s 内没等到通配符签发成功。" >&2
  echo "── 枢衡 ──" >&2
  cat "$WORK/gen1.log" >&2
  echo "── challtestsrv ──" >&2
  tail -30 "$WORK/challtestsrv.log" >&2
  echo "── pebble ──" >&2
  tail -40 "$WORK/pebble.log" >&2
  exit 1
fi
ok "通配符 $WILDCARD_SITE 走 DNS-01 签下来了"

SERIAL_1=$(cert_serial "$CERT_DIR/cert.pem")
NOT_BEFORE_1=$(cert_epoch "$CERT_DIR/cert.pem" -startdate)
NOT_AFTER_1=$(cert_epoch "$CERT_DIR/cert.pem" -enddate)
LIFETIME=$((NOT_AFTER_1 - NOT_BEFORE_1))

# ★ ★ **自证第二半：从产物上再量一次寿命。**
#   上面量的是 pebble 说了什么，这里量的是它**真的签了什么**。
#   ⚠ 允许几秒误差：证书的时刻只精确到秒，编码时会截断。
if [ "$LIFETIME" -ge "$((VALIDITY - 5))" ] && [ "$LIFETIME" -le "$((VALIDITY + 5))" ]; then
  ok "签下来这张的寿命 = ${LIFETIME}s（≈ 配置的 ${VALIDITY}s）"
else
  fail "签下来这张的寿命是 ${LIFETIME}s，期望 ≈ ${VALIDITY}s —— 短寿命没落到证书上"
fi

# 续期那一趟要跟它比，所以这里先钉住「第一趟确实走了 DNS-01」。
if grep -qF "的 TXT 已在全部 1 台权威 NS 上可见" "$WORK/gen1.log"; then
  ok "第一趟真去问了权威 NS 确认 TXT 可见（G58）"
else
  fail "第一趟日志里没有「向权威 NS 确认 TXT 可见」那一步"
fi

echo "=== [3/5] 第二代：先判「还不到时候」——续期的反面必须先成立 ==="

# ★ ★ 为什么必须换一代：签完那一轮的 `next_check` 是**写死的 3600s**
#   （`issue.rs` 里 `report.note_next_check(Duration::from_secs(3600))`），
#   而 ARI 是「盘上已经有一张」时才去问的。所以第一代要等一小时才会再看一眼，
#   门禁等不起。⚠ 这不是缺陷：真实证书是 90 天 / 6 天量级，一小时无关紧要。
#   ★ 顺带这一步也验了「重启之后盘上那张读得出来、装得上」。
kill -INT "$FULCRUM_PID" 2>/dev/null || true
waited=0
while kill -0 "$FULCRUM_PID" 2>/dev/null && [ "$waited" -lt 100 ]; do
  sleep 0.1
  waited=$((waited + 1))
done
for p in "$HTTP_PORT" "$TLS_PORT"; do
  if port_listening "$p"; then
    fail "第一代退了，端口 $p 还被占着 —— 第二轮测的会是别人的服务"
  fi
done
# ★ 与 [`run.sh`](run.sh) 同一处取证，理由写在那边：`:80` 只记不判。
#   ⚠ 本场景的 `:80` 与 run.sh 的是**同一个**（重定向端口写死，两边都隐式用它），
#   所以两个场景的现场都要留下来 —— 只在 run.sh 那边留，换成本场景红的时候就又什么都没有。
PORT80_BEFORE_GEN2=$(acme_port_snapshot 80)

start_fulcrum gen2
wait_port "$TLS_PORT" || {
  echo "ACME RENEW TESTS FAILED: 第二代起不来。日志：" >&2
  cat "$WORK/gen2.log" >&2
  # ★ 与 run.sh 同一处，连同那条「不许包成函数」的理由（`wait` 在子 shell 里恒 127）。
  if kill -0 "$FULCRUM_PID" 2>/dev/null; then
    echo "  ⇒ 第二代 pid $FULCRUM_PID **还活着** —— 它是没在 $TLS_PORT 上听，不是死了。" >&2
  else
    GEN2_RC=0
    wait "$FULCRUM_PID" 2>/dev/null || GEN2_RC=$?
    echo "  ⇒ 第二代 pid $FULCRUM_PID **已经退了**（退出码 $GEN2_RC）—— 它是死了，不是慢。" >&2
  fi
  echo "── 取证：第二代起来之前 :80 上的 socket ──" >&2
  if [ -n "$PORT80_BEFORE_GEN2" ]; then
    printf '%s\n' "$PORT80_BEFORE_GEN2" >&2
    echo "  ⇒ 第二代还没起就已经有人占着 :80 —— 不是第二代自己跟自己抢。" >&2
  else
    echo "  （空：第一代退干净之后 :80 上一个 socket 都没有" >&2
    echo "   ⇒ 占用是在第二代启动之后才出现的，看下面的表是谁。）" >&2
  fi
  acme_dump_ports "第二代起不来"
  exit 1
}
if ! wait_log "$WORK/gen2.log" 'ACME 本轮：签发 0，已是最新 1，' 60; then
  fail "第二代第一轮没有判成「签发 0，已是最新 1」"
  grep 'ACME' "$WORK/gen2.log" >&2 || true
else
  # ⚠ 这条是**续期判据的反面**，缺了它「后面续了」就可能只是「它每轮都续」。
  ok "第二代第一轮判「已是最新」——还不到 ARI 窗口，所以不动"
fi

# ★ ★ 把「此刻签发成功出现过几次」记下来当基线。
#   ⚠ 下面那条判据要的是「**又**签了一张」，而 `wait_log` 只会告诉你「出现过」。
#   上面那条断言已经说明第一轮没签，但**判据不该建在另一条判据的结论上**：
#   那条一旦被改动或放宽，这条会跟着悄悄失效，而它自己看起来还是绿的。
ISSUED_BASELINE=$(grep -cF "ACME 签发成功：$WILDCARD_SITE" "$WORK/gen2.log" || true)

# ARI 窗口必须**落在将来**。它同时是两件事的判据：
#   ① ARI 真被问到并落了盘；② 下面那次续期是「等到时候才动」，不是「一上来就动」。
ARI_START=$(sed -n 's/.*"ari_start": \([0-9]*\).*/\1/p' "$META")
if [ -z "$ARI_START" ]; then
  fail "第二代第一轮之后 meta.json 里没有 ARI 窗口"
  cat "$META" >&2 || true
else
  NOW=$(date +%s)
  if [ "$ARI_START" -gt "$NOW" ]; then
    ok "ARI 窗口在 $((ARI_START - NOW))s 之后才开始（= notBefore + $((ARI_START - NOT_BEFORE_1))s）"
  else
    fail "ARI 窗口在第二代起来时就已经开始了（早了 $((NOW - ARI_START))s）—— \
本场景要的是「它自己等到时候」。把 VALIDITY 调大一些（当前 ${VALIDITY}s）。"
  fi
fi

echo "=== [4/5] 不碰它，等它自己走到续期 ==="

# ★ ★ ★ 判据一：**谁下的决定**。这一句只在 `should_renew` 走 ARI 那一支时才会出现——
#   走比例制（剩余寿命 1/3）会打另一句话。固定 sleep 或「读不出旧证书于是重签」
#   则一句都不会有。⚠ 只断言「又出现了一张新证书」是分不出这几种的。
if ! wait_log "$WORK/gen2.log" "$WILDCARD_SITE 该续期了：CA 的 ARI 建议窗口已经开始" "$RENEW_WAIT"; then
  fail "${RENEW_WAIT}s 内没等到「该续期了：CA 的 ARI 建议窗口已经开始」"
  grep -E '该续期|暂不续期|ACME' "$WORK/gen2.log" >&2 || true
else
  ok "★ 巡检循环自己醒过来，按 CA 的 ARI 判定该续期了"
fi

# ★ ★ 判据二：真的**又**签了一张 —— 按上面那条基线计数，不按「出现过」。
if ! wait_log_count "$WORK/gen2.log" "ACME 签发成功：$WILDCARD_SITE" \
  "$((ISSUED_BASELINE + 1))" 60; then
  fail "判了该续期，却没有等到「ACME 签发成功」——续期在半路失败了"
  grep -E 'ACME|失败' "$WORK/gen2.log" >&2 || true
  echo "── pebble ──" >&2
  tail -40 "$WORK/pebble.log" >&2
fi

# ★ ★ ★ 判据三：**序列号变了**。这一条是 owner 点名的那条——
#   「不能只数日志行数」。序列号是 CA 给的，同一张证书重写一遍不会换号。
SERIAL_2=$(cert_serial "$CERT_DIR/cert.pem")
if [ "$SERIAL_1" != "$SERIAL_2" ]; then
  ok "★ 盘上的证书换了一张：serial $SERIAL_1 → $SERIAL_2"
else
  fail "序列号没变（$SERIAL_1）—— 没有真的重签"
fi

NOT_BEFORE_2=$(cert_epoch "$CERT_DIR/cert.pem" -startdate)
if [ "$NOT_BEFORE_2" -gt "$NOT_BEFORE_1" ]; then
  ok "新证书的 notBefore 比旧的晚 $((NOT_BEFORE_2 - NOT_BEFORE_1))s"
else
  fail "新证书的 notBefore 没有前进（$NOT_BEFORE_1 → $NOT_BEFORE_2）"
fi

# ★ ★ 判据四：**续期那一趟又走了一遍 DNS-01**。
#   `PEBBLE_AUTHZREUSE=0` 之后 CA 不会复用授权，所以挑战必须重解一次。
#   ⚠ 少了这一条，一个「续期时不去改 TXT」的实现会在真实 CA 上间歇性失败，
#   而在授权还没过期的窗口里看起来完全正常。
if grep -qF "的 TXT 已在全部 1 台权威 NS 上可见" "$WORK/gen2.log"; then
  ok "续期那一趟重新写了 TXT 并重新确认可见（G58 对续期同样成立）"
else
  fail "续期那一趟没有「向权威 NS 确认 TXT 可见」——挑战可能被跳过了"
  grep -i 'TXT\|DNS-01' "$WORK/gen2.log" >&2 || true
fi

# ★ ★ 判据五：**装上去了**，不只是写到盘上。走真的 TLS 握手，从线上取序列号。
#   ⚠ 「盘上换了一张」与「客户端拿到的是新的那张」是两件事：
#   少了热装那一步，服务端会拿着一张已经被换掉的证书一直用到重启。
WIRE_SERIAL=$(echo | openssl s_client -connect "$HOST:$TLS_PORT" \
  -servername "$WILDCARD_HOST" -CAfile "$WORK/pebble-root.pem" 2>/dev/null |
  openssl x509 -noout -serial 2>/dev/null | cut -d= -f2 || true)
if [ "$WIRE_SERIAL" = "$SERIAL_2" ]; then
  ok "★ 线上握手拿到的就是新那张（serial $WIRE_SERIAL）—— 续完热装进去了"
else
  fail "线上握手拿到的序列号是「$WIRE_SERIAL」，盘上是「$SERIAL_2」—— 续完没热装"
fi

# 通配符照旧能服务子域，而且用 pebble 的根验得过签。
WCODE=$(run_curl -s -o "$WORK/wbody" -w '%{http_code}' --max-time 10 \
  --cacert "$WORK/pebble-root.pem" \
  --resolve "$WILDCARD_HOST:$TLS_PORT:$HOST" "https://$WILDCARD_HOST:$TLS_PORT/")
expect_status "续期之后通配符 HTTPS（$WILDCARD_HOST，用 pebble 的根验签）" 200 "$WCODE"

# ★ ARI 窗口是**跟着某一张证书**的：换了一张，旧窗口必须作废，
#   否则新证书一上来就会被判成「该续了」——那是一个无限续期循环。
if grep -q '"ari_start": null' "$META" 2>/dev/null; then
  ok "续完之后 ARI 窗口被清空了（窗口跟着证书走，不跟着域名走）"
else
  fail "续完之后 meta.json 里的 ari_start 还留着旧值"
  cat "$META" >&2 || true
fi

echo "=== [5/5] 收工 ==="

if [ "$FAILS" -ne 0 ]; then
  echo >&2
  echo "ACME RENEW TESTS FAILED: $FAILS 条断言不通过" >&2
  # ★ ★ 加的取证。这一格**会偶发红**（PLAN §10）：
  #   连跑第四次全量门禁时红了 5 条，单独复跑 17/17 绿。
  #   错误是 pebble 的原话：`could not find order resulting in the given
  #   certificate serial number`（ARI 续期下单时带的 `replaces` 查不到订单）——
  #   ⚠ **而 pebble 自己的日志里明明有那笔订单**。
  #   ★ 下面这几行把**两侧的序列号放在一起**，下次再碰就能当场判断：
  #   是我们发错了序列号，还是 pebble 自己丢了订单。
  #   ⚠ ⚠ **没有这一段时，上一次真实失败只留下了一句「续期在半路失败了」。**
  echo "── ★ 两侧的序列号对不对得上 ──" >&2
  if [ -f "$CERT_DIR/cert.pem" ]; then
    echo "盘上这张证书的序列号（我们会拿它去做 ARI 的 replaces）：$(cert_serial "$CERT_DIR/cert.pem")" >&2
  else
    echo "（盘上没有 cert.pem）" >&2
  fi
  echo "pebble 说它签过哪几张：" >&2
  grep -a "Issued certificate serial" "$WORK/pebble.log" >&2 || echo "（一行都没有）" >&2
  echo "pebble 建过哪几单：" >&2
  grep -a "Added order" "$WORK/pebble.log" >&2 || echo "（一行都没有）" >&2
  echo "枚举到的 ARI / replaces 相关报错：" >&2
  grep -aiE "renewalInfo|replaces|could not find order" "$WORK/pebble.log" | tail -20 >&2 \
    || echo "（一行都没有）" >&2
  echo "── 枢衡（第一代）──" >&2
  cat "$WORK/gen1.log" >&2
  echo "── 枢衡（第二代）──" >&2
  cat "$WORK/gen2.log" 2>/dev/null >&2 || true
  echo "── pebble ──" >&2
  tail -60 "$WORK/pebble.log" >&2
  echo >&2
  echo "★ 先单独复跑这一格（它会偶发红）：bash tests/acme/renew.sh" >&2
  echo "  ⚠ 不许用「加重试」修它 —— 那会同时掩盖真实的续期缺陷。" >&2
  exit 1
fi
echo "ACME RENEW TESTS PASSED —— 通配符证书由**跑着的**枢衡按 CA 的 ARI 自己续了一次："
echo "  等到窗口才动、序列号真的变了、续期那一趟重新解了一遍 DNS-01、续完热装进了握手。"
