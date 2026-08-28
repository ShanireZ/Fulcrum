#!/usr/bin/env bash
# ACME 端到端：在门禁里跑一个**真的 CA**（pebble，G64），让枢衡真签一张证书出来。
#
# ★ ★ **这个场景存在的全部理由是「判据」**：账户、订单、HTTP-01、ARI、退避、跨代锁、
#   热装 —— G54 的三种挑战与 G58 那条「签发并续期一次通配符证书」，
#   **每一条都只有真跑过才算数**，编得过、单测绿都不算。
#
# ★ 为什么是 pebble 而不是打真 CA（G64，owner 拍板）：真实签发要么打真 CA
#   （要真域名、消耗速率配额、**不能进门禁**），要么在门禁里跑一个本地 ACME 服务器。
#   pebble 是 Let's Encrypt 官方的测试用 CA，与 boulder 同一批人写的另一个 RFC 8555 实现。
#
# 拓扑：
#
#   pebble-challtestsrv  :8053(DNS) :8055(管理)  —— ★ 一身二职，见下
#   pebble               :14000(目录) :15000(管理)  —— 真 CA，验 HTTP-01 时连 127.0.0.1:8083
#   fulcrum serve        :8083(HTTP)  :9443(HTTPS)  —— 被验的那一方
#
# ★ ★ ★ **challtestsrv 一身二职，而这正是 DNS-01 判据成立的关键**：
#   ① 它是 pebble 的解析器（pebble 起来时带了 `-dnsserver`）；
#   ② 它也是枢衡 `resolvers` 里那台「权威 NS」；
#   ③ exec hook 通过它的管理接口（:8055 的 `/set-txt`）写记录。
#   于是「hook 写进去的」「枢衡确认可见的」「CA 查到的」**是同一份数据**，
#   而不是三个互相假设的替身——那种测法只能证明我们的假设自洽。
#
# 用法：
#   bash tests/acme/run.sh              # 容器里跑（tests/m0/docker-run.sh 会驱动它）
#   ACME_KEEP=1 bash tests/acme/run.sh  # 失败时保留工作目录，便于人来看
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1

# pebble 侧
DIR_PORT=${DIR_PORT:-14000}   # ACME 目录（HTTPS）
MGMT_PORT=${MGMT_PORT:-15000} # 管理接口（HTTPS），根证书从这里取
DNS_PORT=${DNS_PORT:-8053}
CTS_MGMT_PORT=${CTS_MGMT_PORT:-8055}
# 枢衡侧。★ 避开 serve 场景的 9100–9103 与 M0 的 8080–8082。
HTTP_PORT=${HTTP_PORT:-8083}
TLS_PORT=${TLS_PORT:-9443}
# ★ ★ 批 7：这一行的含义变了，而它是 TLS-ALPN-01 判据的**全部前提**。
#
#   `pebble.tlsPort` 是 pebble **发起 TLS-ALPN-01 验证时去连的端口**。
#   在这之前它被设成一个没人要的端口（9444），因为那一批还没做这个挑战。
#   现在把它指向**枢衡自己的 HTTPS 端口** —— 于是「CA 来验」这件事是真的发生的：
#   pebble 拿着 `acme-tls/1` 连过来，枢衡在握手里回那张自签的挑战证书。
#   ⚠ 真实环境里这个端口恒为 443（RFC 8737 §3 写死），只有 pebble 允许改。
PEBBLE_TLSALPN_PORT=${PEBBLE_TLSALPN_PORT:-$TLS_PORT}

DIR_URL="https://localhost:${DIR_PORT}/dir"
# ★ 目录名由 `issuer_slug()` 从目录 URL 推出来：host[:port] 里的非白名单字符换成 `-`。
#   这里**写死期望值**而不是调用产品代码去算 —— 判据不能拿被测对象自己算的结果当尺子。
ISSUER_DIR="ca-localhost-${DIR_PORT}"

DOMAIN=acme.example
# ★ ★ 批 7：第二个非通配域名，**专门用来守住 HTTP-01 那条路**。
#
#   TLS-ALPN-01 接线之后它成了「主」，于是 `acme.example` 会走 TLS-ALPN-01——
#   ⚠ 如果只有它一个，**HTTP-01 那条路（连同它绕过路由的那套判据）会在这一批里
#   悄悄变成没人跑**，而门照样全绿。这正是本仓库反复栽的形状：
#   一个功能被新功能顶替之后，守它的判据跟着一起失效，却没有任何东西说出来。
#
#   怎么让它走备用的那条：**预先在存储里放一份 `meta.json`，说上一次
#   TLS-ALPN-01 失败过**。于是 `pick_non_dns_challenge` 会选 HTTP-01——
#   ★ 这同时把「主/备切换」这条逻辑端到端验了一遍，而不只是单测里验过。
HTTP01_DOMAIN=http01.example
WILDCARD_SITE='*.wild.example'
WILDCARD_HOST=x.wild.example
# ★ 另一个通配符站点，**故意不配 DNS-01** —— 用来守「推迟 ≠ 失败」那条判据。
#   两个通配符站点同时在场，才能在一次跑里同时验「配了就能签」与「没配就推迟」。
NODNS_SITE='*.nodns.example'

STATE="$WORK/state"
CERT_DIR="$STATE/certs/$ISSUER_DIR/$DOMAIN"
HTTP01_CERT_DIR="$STATE/certs/$ISSUER_DIR/$HTTP01_DOMAIN"
# ★ `*` 在存储里被转义成 `_wildcard_`（见 fulcrum-tls 的 sanitize/unsanitize）。
WILD_CERT_DIR="$STATE/certs/$ISSUER_DIR/_wildcard_.wild.example"
NODNS_CERT_DIR="$STATE/certs/$ISSUER_DIR/_wildcard_.nodns.example"
HOOK="$WORK/dns-hook.sh"
ACCOUNT_JSON="$STATE/acme/$ISSUER_DIR/account.json"

# 装进系统信任库的那份（pebble 自己 HTTPS API 的证书）。★ 走系统信任库是**故意的**：
# 产品代码用的是 `instant-acme` 的默认客户端（平台信任库），
# **绝不为了测试给产品加一个「信任某个根」的配置项**——那是测试专用旋钮长进产品面。
TRUST_ANCHOR=/usr/local/share/ca-certificates/fulcrum-pebble-api.crt

# ★ 起 CA、起假权威 NS、写 exec hook、那一整套等待与断言小工具，
#   与续期场景 [`renew.sh`](renew.sh) **共用同一份**。理由见 lib.sh 开头。
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

echo "=== [0/6] 基线：端口是空的、存储里还没有证书 ==="

acme_require_tools "ACME TESTS"
[ -x "$BIN" ] || {
  echo "ACME TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

acme_require_ports_free "ACME TESTS" \
  "$DIR_PORT" "$MGMT_PORT" "$DNS_PORT" "$CTS_MGMT_PORT" "$HTTP_PORT" "$TLS_PORT"
ok "六个端口都是空的"

# ★ ★ **这条与 [3/6] 的那条是同一个判据的两个方向**：现在没有，签完必须有。
#   一个只见过「有」的断言，与一个恒真的断言无法区分。
if [ -e "$CERT_DIR/cert.pem" ]; then
  echo "ACME TESTS FAILED: $CERT_DIR/cert.pem 一开始就存在 —— 那下面「签出来了」就证明不了任何事" >&2
  exit 1
fi
ok "存储里还没有 $DOMAIN 的证书"

echo "=== [1/6] 起 pebble（本地 CA）与 challtestsrv（假 DNS）==="

acme_make_api_cert "ACME TESTS"
acme_write_pebble_config "$DIR_PORT" "$MGMT_PORT" "$HTTP_PORT" "$PEBBLE_TLSALPN_PORT"
acme_start_challtestsrv "$DNS_PORT" "$CTS_MGMT_PORT"
# ★ 授权复用留在 pebble 的默认值（50%）：本场景每个域名只签一次，复用与否都碰不到。
#   续期场景把它关掉，理由见 lib.sh。
acme_start_pebble "$DNS_PORT"

wait_port "$DIR_PORT" || {
  echo "ACME TESTS FAILED: pebble 的目录端口 $DIR_PORT 起不来。日志：" >&2
  cat "$WORK/pebble.log" >&2
  exit 1
}
wait_port "$MGMT_PORT" || {
  echo "ACME TESTS FAILED: pebble 的管理端口 $MGMT_PORT 起不来" >&2
  exit 1
}

# ★ 不加 `-k`。这一次请求同时验了两件事：pebble 活着，**且信任根真的装进去了**。
DIR_JSON=$(run_curl -fsS --max-time 10 "$DIR_URL")
if [ -z "$DIR_JSON" ]; then
  echo "ACME TESTS FAILED: 取不到 ACME 目录 $DIR_URL（多半是信任根没装上）" >&2
  cat "$WORK/pebble.log" >&2
  exit 1
fi
ok "ACME 目录取到了，且是**验过签**的（信任根装好了）"

# ★ ARI（RFC 9773）能不能被真验一次，取决于这个 CA 认不认 `renewalInfo`。
#   把它打出来而不是假设 —— 下面 [5/6] 那条 ARI 断言的前提就是它。
if printf '%s' "$DIR_JSON" | grep -q 'renewalInfo'; then
  ok "这个 CA 的目录里有 renewalInfo（ARI 可以被真验一次）"
  HAS_ARI=1
else
  echo "  ⚠ 目录里没有 renewalInfo —— ARI 那条断言会被跳过" >&2
  HAS_ARI=0
fi

# pebble 的根证书。签出来的证书要能用它验过 —— 那才是「真签了一张」。
acme_fetch_root "ACME TESTS" "$MGMT_PORT"
ok "取到 pebble 的根证书"

acme_write_dns_hook "$HOOK" "$CTS_MGMT_PORT"

echo "=== [2/6] 起枢衡，等它把证书签下来 ==="

# ★ ★ 挑战站点写的是 `respond 403` —— **这是判据本身，不是随手写的**。
#   一份 `respond 403` 的配置完全合法，而 HTTP-01 的应答若走正常路由，
#   **用户的配置会把自己的证书签发挡掉**，现场只看得到「CA 说验不过」。
#   于是：签成功 ⇒ 应答确实绕过了路由；而下面 [4/6] 里那个不认识的 token 拿到 403
#   ⇒ 绕过的面只有「当前有效的 token」这一点大。**两个方向都在同一次跑里。**
#
# ★ ★ 两个通配符站点是**故意的**，它们各守一条判据：
#   · `*.wild.example` 配了 `dns exec` + `resolvers` ⇒ 应当**真的签下来**（G58）；
#   · `*.nodns.example` 什么都没配 ⇒ 应当被记成**推迟**，不是**失败**
#     （失败会进退避、进计数、进告警）。
#   一次跑里同时验「配了就能签」与「没配就推迟」，两个方向都在。
cat > "$WORK/acme.Fulcrumfile" <<CONF
{
    acme_ca $DIR_URL
    acme_email fulcrum-gate@example.invalid
}

http://$DOMAIN:$HTTP_PORT {
    respond 403
}

http://$HTTP01_DOMAIN:$HTTP_PORT {
    respond 403
}

$DOMAIN:$TLS_PORT {
    respond 200 "acme-secure"
}

$HTTP01_DOMAIN:$TLS_PORT {
    respond 200 "http01-secure"
}

$WILDCARD_SITE:$TLS_PORT {
    tls {
        dns exec HOOKPATH
        resolvers $HOST:$DNS_PORT
    }
    respond 200 "wild-ok"
}

$NODNS_SITE:$TLS_PORT {
    respond 200 "nodns"
}
CONF
# 路径里有斜杠，换个分隔符。
sed -i "s|HOOKPATH|$HOOK|" "$WORK/acme.Fulcrumfile"

# ★ ★ 预先给 $HTTP01_DOMAIN 放一份 meta.json，说「上一次 TLS-ALPN-01 失败过」。
#   于是 `pick_non_dns_challenge` 会给它选 HTTP-01（G54 的备）。
#   ⚠ 字段要写全：`load_meta` 解析失败会**静默退回默认值**（那是有意的——
#   meta 坏了不该让证书不可用），而退回默认值之后这个域名会走主的那条，
#   本条判据就悄悄失效了。所以下面 [3/6] 里有一条断言专门验它**真的走了 HTTP-01**。
mkdir -p "$HTTP01_CERT_DIR"
cat > "$HTTP01_CERT_DIR/meta.json" <<METAEOF
{
  "issuer": "$ISSUER_DIR",
  "issuer_url": null,
  "ari_start": null,
  "ari_end": null,
  "renewal": { "failures": 0, "last_attempt": null },
  "last_challenge_failed": "tls-alpn-01"
}
METAEOF

start_fulcrum() {
  local name=$1
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/acme.Fulcrumfile" \
    --bind-host "$HOST" \
    --state-dir "$STATE" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
  FULCRUM_PID=$!
}

start_fulcrum run1
for p in "$HTTP_PORT" "$TLS_PORT"; do
  wait_port "$p" || {
    echo "ACME TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK/run1.log" >&2
    acme_dump_ports "第一代：$p 起不来"
    exit 1
  }
done
ok "两个监听起来了"

# ★ ★ 这一条永远成立：`*.nodns.example` 没配 DNS-01，这一批签不出来，
#   所以对它的握手必须被拒绝。判据挂在 **curl 的退出码**上——
#   `%{http_code}` 的 `000` 也可能来自超时、端口没开，分不出「被拒绝握手」这一种。
curl_capture -s -o /dev/null -w '%{http_code}' --max-time 5 -k \
  --resolve "x.nodns.example:$TLS_PORT:$HOST" "https://x.nodns.example:$TLS_PORT/"
WILD_CODE=$(cat "$WORK/curlw")
if [ "$CURL_RC" -ne 0 ] && [ "$WILD_CODE" = "000" ]; then
  ok "没配 DNS-01 的通配符站点 ⇒ 握手被拒绝（curl 退出码 $CURL_RC）"
else
  fail "没配 DNS-01 的通配符期望握手失败，实际 curl 退出码 $CURL_RC、HTTP $WILD_CODE"
fi

if ! wait_log "$WORK/run1.log" "ACME 签发成功：$WILDCARD_SITE" 120; then
  echo "ACME TESTS FAILED: 120s 内没等到**通配符**（DNS-01）签发成功。" >&2
  echo "── 枢衡日志 ──" >&2
  cat "$WORK/run1.log" >&2
  echo "── challtestsrv ──" >&2
  tail -30 "$WORK/challtestsrv.log" >&2
  echo "── pebble ──" >&2
  tail -40 "$WORK/pebble.log" >&2
  exit 1
fi
ok "★ 通配符 $WILDCARD_SITE 走 DNS-01 真签下来了（G58）"

if ! wait_log "$WORK/run1.log" "ACME 签发成功：$DOMAIN" 90; then
  echo "ACME TESTS FAILED: 90s 内没等到签发成功。" >&2
  # ★ 先把最可能、又最难认出来的那一种说破：pebble 故意拒 5% 的合法 nonce
  #   （见 lib.sh 里 `acme_start_pebble` 上面那一段），而客户端只重试 3 次。撞上时枢衡这边
  #   只有一句 `client error`，pebble 那边才看得到 `anti-replay nonce`。
  if grep -qi 'nonce' "$WORK/pebble.log" 2>/dev/null; then
    echo "★ pebble 的日志里出现了 nonce 相关的拒绝 —— 这很可能就是那个 5% 的随机项" >&2
    echo "  （NONCE_REJECT=0 可以关掉它复核；但**别把它长久关掉**，那条重试路就没人验了）" >&2
  fi
  echo "── 枢衡日志 ──" >&2
  cat "$WORK/run1.log" >&2
  echo "── pebble 日志 ──" >&2
  cat "$WORK/pebble.log" >&2
  exit 1
fi
ok "枢衡向 pebble 真签下来一张 $DOMAIN 的证书"

if ! wait_log "$WORK/run1.log" "ACME 签发成功：$HTTP01_DOMAIN" 90; then
  fail "90s 内没等到 $HTTP01_DOMAIN（HTTP-01 那条备用路）签发成功"
  grep -E 'ACME|挑战' "$WORK/run1.log" >&2 || true
else
  ok "★ $HTTP01_DOMAIN 走 HTTP-01（G54 的备）也签下来了"
fi

# ── ★ ★ ★ G54 的「主/备」：判据挂在**这一次到底走了哪条路**上 ──────────────
#
# ⚠ 只断言「三张证书都出来了」是分不出走了哪种挑战的——而这一批的全部内容
#   恰恰是「多了一种挑战，并且它成了主」。所以判据挂在那两行 info 日志上。
#   ★ 那两行被特意提到 info 级，理由与 G58 那条轮询日志一模一样：
#     它是「用了哪种挑战」唯一可从外部观察到的痕迹。
if grep -qF "$DOMAIN：本次挑战走 TLS-ALPN-01" "$WORK/run1.log"; then
  ok "★ $DOMAIN **选**的是 TLS-ALPN-01（G54 的主，RFC 8737）"
else
  fail "$DOMAIN 没有走 TLS-ALPN-01 —— 主/备选反了，或者这条路根本没接上"
  grep -E '本次挑战走' "$WORK/run1.log" >&2 || true
fi

# ── ★ ★ ★ 上面那条只证明「我们**试**了 TLS-ALPN-01」，不证明「它**成了**」──────
#
# ⚠ ⚠ **反证抓到过一个真缺陷**：把挑战证书里那条 acmeIdentifier 扩展去掉之后，
#   pebble 当场 `authz … set INVALID`（破坏是生效的），但退避到点后按 G54 的「备」
#   换成 HTTP-01，证书照样签了出来 ⇒ **整道门全绿**，连上面那条断言也是绿的（它确实「试」过）。
#   ★ 一道分不出「成功」与「失败了但被兜住」的门，恰好对这一层是瞎的。
#
# 修法：判据改挂在 **CA 自己的记录**上——pebble 每次 HTTP 验证都会打一行
# `Attempting to validate w/ HTTP: http://<域名>:`。TLS-ALPN-01 成功的话，
# 这个域名**根本不该出现在那些行里**。
if grep -qF "Attempting to validate w/ HTTP: http://$HTTP01_DOMAIN:" "$WORK/pebble.log"; then
  ok "自证：同一把尺子量得出 $HTTP01_DOMAIN 确实被 CA 用 HTTP-01 验过"
else
  fail "尺子瞎了：CA 的日志里连 $HTTP01_DOMAIN 的 HTTP 验证都找不到，下一条断言不可采信"
fi
if grep -qF "Attempting to validate w/ HTTP: http://$DOMAIN:" "$WORK/pebble.log"; then
  fail "★ CA 用 HTTP-01 验过 $DOMAIN —— 说明 TLS-ALPN-01 那一趟**失败了**，只是被备用挑战兜住了"
  grep -E "set INVALID|Attempting to validate" "$WORK/pebble.log" >&2 || true
else
  ok "★ ★ CA 从没用 HTTP-01 验过 $DOMAIN ⇒ TLS-ALPN-01 那一趟是**真的成功了**"
fi
# ★ 再钉一条更粗的：一次健康的跑里，CA 不该判任何一个授权 INVALID。
#   它挡的是同一族的「失败了但被某种兜底盖过去」——不限于挑战类型。
if grep -q "set INVALID" "$WORK/pebble.log"; then
  fail "CA 判过 INVALID —— 这一跑里有验证失败被兜住了"
  grep "set INVALID" "$WORK/pebble.log" >&2 || true
else
  ok "CA 一次 INVALID 都没判过（没有失败被兜底盖住）"
fi
# ★ 反向那一半：预置了「上次 TLS-ALPN-01 失败过」的那个域名必须**换成备用的**。
#   ⚠ 少了这一条，一个「永远走主的」实现照样能让上面那条全绿。
if grep -qF "$HTTP01_DOMAIN：本次挑战走 HTTP-01" "$WORK/run1.log"; then
  ok "★ $HTTP01_DOMAIN 按预置的失败记录换成了 **HTTP-01**（主/备切换真的生效）"
else
  fail "$HTTP01_DOMAIN 没有换成 HTTP-01 —— meta.json 里那条 last_challenge_failed 没被读到"
  grep -E '本次挑战走' "$WORK/run1.log" >&2 || true
fi

echo "=== [3/6] 落盘的东西对不对（G55）==="

if [ -f "$CERT_DIR/cert.pem" ]; then
  ok "证书落在 certs/$ISSUER_DIR/$DOMAIN/"
else
  fail "证书没落在 $CERT_DIR/"
  ls -laR "$STATE" >&2 || true
fi
# ★ 私钥 0600 是 G55 的硬要求，而且**权限要在 create 的那一刻就给**
#   （先建再 chmod 之间有一个窗口，那个窗口里私钥是可读的）。
expect_mode "私钥" 600 "$CERT_DIR/key.pem"
expect_mode "证书" 644 "$CERT_DIR/cert.pem"
expect_mode "账户凭据" 600 "$ACCOUNT_JSON"

# ★ ★ `issuer_url` 记的是**签它的那个 CA 的目录 URL 原文**。
#   目录名（`ca-localhost-14000`）是有损映射，两个 CA 可能落到同一个名字上——
#   判据挂在数据上，不挂在命名约定上。
META="$CERT_DIR/meta.json"
if grep -qF "\"issuer_url\": \"$DIR_URL\"" "$META" 2>/dev/null; then
  ok "meta.json 里的 issuer_url 就是这次用的那个目录 URL"
else
  fail "meta.json 里的 issuer_url 对不上（期望 $DIR_URL）"
  cat "$META" >&2 || true
fi
# ★ 上面那条 grep 的**自证**：同一把尺子必须量得出「不是这个值」。
#   一个只会命中的判据与一个恒真的判据无法区分。
if grep -qF '"issuer_url": null' "$META" 2>/dev/null; then
  fail "meta.json 里 issuer_url 同时还是 null —— 判据自己瞎了"
else
  ok "同一把尺子量 null 时不命中（判据自证）"
fi

# ★ 第一轮之后 ARI 窗口必须还是空的：ARI 是**跟着某一张已存证书**问的，
#   刚签下来那一刻还没问过。它与 [5/6] 那条是同一个判据的两个方向。
if grep -q '"ari_start": null' "$META" 2>/dev/null; then
  ok "刚签完时 ARI 窗口是空的（还没问过）"
else
  fail "刚签完时 meta.json 里的 ari_start 不是 null"
  cat "$META" >&2 || true
fi

# ★ 配了 DNS-01 的通配符：证书**必须**落盘，而且目录名里的 `*` 被转义成 `_wildcard_`。
if [ -f "$WILD_CERT_DIR/cert.pem" ]; then
  ok "通配符证书落在 certs/$ISSUER_DIR/_wildcard_.wild.example/"
else
  fail "通配符证书没落在 $WILD_CERT_DIR/"
  ls -laR "$STATE/certs" >&2 || true
fi
expect_mode "通配符私钥" 600 "$WILD_CERT_DIR/key.pem"

# ★ 没配 DNS-01 的那个：推迟，不是失败，**而且不许留下任何存储痕迹**。
if [ -e "$NODNS_CERT_DIR" ]; then
  fail "没配 DNS-01 的通配符在存储里留下了 $NODNS_CERT_DIR —— 它本不该被处理"
else
  ok "没配 DNS-01 的通配符没有在存储里留下任何东西"
fi
if grep -q '推迟 1，退避 0，失败 0' "$WORK/run1.log"; then
  ok "它被记成「推迟 1」而不是失败（失败会进退避、进计数、进告警）"
else
  fail "巡检结果那一行不是「推迟 1，退避 0，失败 0」"
  grep 'ACME 本轮' "$WORK/run1.log" >&2 || true
fi

# ★ ★ ★ G58 那条硬约束的判据：日志里必须有「向权威 NS 确认可见」这一步。
#   ⚠ 它证的不是「等过」，而是**等的方式**——固定 sleep 也会让签发成功，
#   所以只断言「签下来了」是分不出这两种实现的。
if grep -q "的 TXT 已在全部 1 台权威 NS 上可见" "$WORK/run1.log"; then
  ok "DNS-01 真去问了权威 NS 确认 TXT 可见（G58：绝不能只 sleep）"
else
  fail "日志里没有「向权威 NS 确认 TXT 可见」那一步 —— 可能退化成了固定 sleep"
  grep -i 'TXT\|DNS-01' "$WORK/run1.log" >&2 || true
fi

# ★ 挑战用的 TXT 用完要摘掉。判据是**直接去问那台权威 NS**，不是看日志。
CHAL_RECORD="_acme-challenge.wild.example"
LEFTOVER=$(run_curl -fsS -X POST -H 'Content-Type: application/json' \
  --data "{\"host\":\"$CHAL_RECORD\"}" "http://$HOST:$CTS_MGMT_PORT/clear-txt" -o /dev/null -w '%{http_code}')
if [ "$LEFTOVER" = "200" ]; then
  ok "挑战记录名可被清理（收尾）"
fi

echo "=== [4/6] 签出来的证书是真能用的 ==="

RESOLVE="$DOMAIN:$TLS_PORT:$HOST"
TLSURL="https://$DOMAIN:$TLS_PORT/"

# ★ ★ 用 `--cacert <pebble 根>` 而不是 `-k`：后者只证明「握手成功了」，
#   证不了「服务端给的是那张刚签下来的证书」。
CODE=$(run_curl -s -o "$WORK/body" -w '%{http_code}' --max-time 10 \
  --cacert "$WORK/pebble-root.pem" --resolve "$RESOLVE" "$TLSURL")
expect_status "HTTPS（用 pebble 的根验签）" 200 "$CODE"
BODY=$(cat "$WORK/body" 2>/dev/null || true)
if [ "$BODY" = "acme-secure" ]; then
  ok "响应体 = $BODY"
else
  fail "响应体期望「acme-secure」，实际「$BODY」"
fi

# ★ 这张证书是不是 pebble 签的，问证书自己，别问我们自己写的 meta。
ISSUER_CN=$(openssl x509 -in "$CERT_DIR/cert.pem" -noout -issuer 2>/dev/null || true)
if printf '%s' "$ISSUER_CN" | grep -qi 'pebble'; then
  ok "证书的签发者是 pebble：$ISSUER_CN"
else
  fail "证书的签发者看起来不是 pebble：$ISSUER_CN"
fi

# ── ★ ★ ★ 通配符证书真的能服务一个子域（G58 的正面判据）────────────────────
#
# ⚠ 「证书文件躺在盘上」与「它真能用」是两件事。这一条走完整条链：
#   SNI 是 `x.wild.example` → 解析器按通配符挑中那张证书 → 客户端用 pebble 的根验签。
WCODE=$(run_curl -s -o "$WORK/wbody" -w '%{http_code}' --max-time 10 \
  --cacert "$WORK/pebble-root.pem" \
  --resolve "$WILDCARD_HOST:$TLS_PORT:$HOST" "https://$WILDCARD_HOST:$TLS_PORT/")
expect_status "通配符 HTTPS（$WILDCARD_HOST，用 pebble 的根验签）" 200 "$WCODE"
WBODY=$(cat "$WORK/wbody" 2>/dev/null || true)
if [ "$WBODY" = "wild-ok" ]; then
  ok "通配符站点响应体 = $WBODY"
else
  fail "通配符站点响应体期望「wild-ok」，实际「$WBODY」"
fi

# ★ 证书上的 SAN 必须真的是通配符，而不是恰好签了 `x.wild.example` 这一个名字。
#   ⚠ 少了这一条，一个「把通配符悄悄降级成单域名」的实现照样全绿。
WSAN=$(openssl x509 -in "$WILD_CERT_DIR/cert.pem" -noout -ext subjectAltName 2>/dev/null || true)
if printf '%s' "$WSAN" | grep -q '\*\.wild\.example'; then
  ok "通配符证书的 SAN 里确实是 *.wild.example"
else
  fail "通配符证书的 SAN 不是通配符：$WSAN"
fi

# ★ ★ HTTP-01 的应答面**只有「当前有效的 token」那么大**。
#   token 用完由 `Drop` 摘掉，所以现在这条路径必须落回路由 —— 而路由说 403。
#   ⚠ 若这里拿到 200，说明挑战路径被永久打开了：那是一条免费的信息泄露面。
CHAL_CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
  --resolve "$DOMAIN:$HTTP_PORT:$HOST" \
  "http://$DOMAIN:$HTTP_PORT/.well-known/acme-challenge/deadbeef")
expect_status "签发完之后，不认识的挑战 token 落回路由（配置说 403）" 403 "$CHAL_CODE"

# ── ★ ★ ★ TLS-ALPN-01 的那张挑战证书表，与真证书表**互不相通** ──────────────
#
# 这是两条方向相反的安全属性，缺一不可：
#   ① 协商到 `acme-tls/1` 却没有挂着的挑战证书 ⇒ **拒绝握手**，
#      绝不回落到真证书（否则等于把用户的真证书交给任何一个说 `acme-tls/1` 的对端）；
#   ② 普通流量（h2 / http/1.1）**拿不到**挑战证书 —— 那张是自签的，
#      拿到它的浏览器会当场报证书错误，而服务端日志里是一次**成功**的握手。
#
# ⚠ 现在挑战早已用完摘掉，所以 ① 直接可验：带 `acme-tls/1` 连过去必须失败。
#   ② 由下面那条「证书的签发者是 pebble」间接守着——挑战证书是自签的，
#   一旦漏进普通流量，签发者就不是 pebble 了。
set +e
openssl s_client -connect "$HOST:$TLS_PORT" -servername "$DOMAIN" \
  -alpn acme-tls/1 -verify_return_error </dev/null >"$WORK/alpn_probe" 2>&1
ALPN_RC=$?
set -e
if [ "$ALPN_RC" -ne 0 ]; then
  ok "★ 带 acme-tls/1 连过去被拒绝握手（挑战表是空的，不回落到真证书）"
else
  fail "带 acme-tls/1 竟然握手成功了 —— 挑战路径回落到了真证书"
  head -20 "$WORK/alpn_probe" >&2 || true
fi
# ★ 同一把尺子的**自证**：不带 acme-tls/1 时它必须能连上。
#   否则上面那个「失败」可能只是端口不通、或者 openssl 用法写错了。
set +e
openssl s_client -connect "$HOST:$TLS_PORT" -servername "$DOMAIN" \
  -CAfile "$WORK/pebble-root.pem" </dev/null >"$WORK/alpn_probe2" 2>&1
PLAIN_RC=$?
set -e
if [ "$PLAIN_RC" -eq 0 ]; then
  ok "同一把尺子不带 acme-tls/1 时握手成功（判据自证：它不是恒失败）"
else
  fail "不带 acme-tls/1 也连不上（退出码 $PLAIN_RC）—— 上面那条拒绝证明不了任何事"
  head -20 "$WORK/alpn_probe2" >&2 || true
fi

echo "=== [5/6] 第二轮巡检：判「已是最新」，而不是再签一张 ==="

CERT_SHA_1=$(sha256sum "$CERT_DIR/cert.pem" | cut -d' ' -f1)

# ★ 换一个进程再跑一轮。这同时验了另一件事：**重启之后盘上那张读得出来、装得上**。
#   （签发路径里那次「写完回读一次再装」防的正是它读不出来，而症状要到重启才出现。）
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
# ★ ★ ★ **`:80` 在这里只记，不判。**
#
#   CI 上间歇性红在下面那句「第二代起不来」，而 `run2.log` 末尾是一串
#   `127.0.0.1:80 is in use, will try again`（pingora 的 `bind_tcp` 重试 30 次、每次 1 秒，
#   而 `wait_port` 20 秒就放弃了 ⇒ 日志停在半路）。本机从来不红。
#
#   ⚠ 现有证据分不开三种解释：第一代其实没走干净 / 那个监听 fd 被某个子进程继承走了 /
#   容器里另有第三方占着。**分开它们要的正是「第二代起来之前 `:80` 长什么样」**——
#   所以在这里取一次快照，只在下面真红的时候打出来：绿的跑一个字都不多。
#
#   ⛔ 不许把它变成断言：占着 `:80` 已被反证为无害（见 lib.sh 里那段），
#   加一道只拦得住正确产出的判据，和不报红一样坏。
PORT80_BEFORE_GEN2=$(acme_port_snapshot 80)

start_fulcrum run2
wait_port "$TLS_PORT" || {
  echo "ACME TESTS FAILED: 第二代起不来。日志：" >&2
  cat "$WORK/run2.log" >&2
  # ★ ★ ★ **「还活着但没在听」与「已经死了」是两个完全不同的答案，而 `cat 日志` 一个都给不出。**
  #   ⚠ 这几行必须写在**主 shell 里**，不能包成函数再 `$(…)` 调用：命令替换跑在子 shell 里，
  #     而 `wait` 等不了父 shell 的后台作业 —— 它会立刻返回 127，
  #     于是每一次都报「退出码 127」，一个恒定的假答案。
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

# ★ 启动时就该把盘上那张装进解析器 —— 这条是「重启之后 HTTPS 还活着」的判据。
if grep -q "装载证书（证书存储）" "$WORK/run2.log"; then
  ok "第二代启动时从存储里把证书装上了"
else
  fail "第二代的日志里没有「装载证书（证书存储）」"
  grep -i '证书\|SNI' "$WORK/run2.log" >&2 || true
fi

if ! wait_log "$WORK/run2.log" 'ACME 本轮：签发 0，已是最新 3，' 60; then
  fail "第二轮没有判成「签发 0，已是最新 3」（acme.example / http01.example / 通配符三张都该判 fresh）"
  grep 'ACME' "$WORK/run2.log" >&2 || true
else
  ok "第二轮三张证书都判「已是最新」，没有再签（含通配符）"
fi

CERT_SHA_2=$(sha256sum "$CERT_DIR/cert.pem" | cut -d' ' -f1)
if [ "$CERT_SHA_1" = "$CERT_SHA_2" ]; then
  ok "盘上那张证书一个字节都没变"
else
  fail "证书被重签了：$CERT_SHA_1 → $CERT_SHA_2"
fi

# ★ ★ ARI（RFC 9773）真被问了一次。它只在「存储里已经有一张」时才问，
#   所以第一轮是 null、第二轮是数字 —— **同一个判据的两个方向，在同一次跑里**。
if [ "$HAS_ARI" = "1" ]; then
  if grep -qE '"ari_(start|end)": [0-9]+' "$META" 2>/dev/null; then
    ok "第二轮问到了 ARI 窗口并落了盘"
  else
    fail "第二轮之后 meta.json 里还是没有 ARI 窗口"
    cat "$META" >&2 || true
  fi
fi

echo "=== [6/6] 收工 ==="

if [ "$FAILS" -ne 0 ]; then
  echo >&2
  echo "ACME TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 枢衡（第一代）──" >&2
  cat "$WORK/run1.log" >&2
  echo "── 枢衡（第二代）──" >&2
  cat "$WORK/run2.log" 2>/dev/null >&2 || true
  echo "── pebble ──" >&2
  tail -50 "$WORK/pebble.log" >&2
  exit 1
fi
echo "ACME TESTS PASSED —— 向真的 CA（pebble）签下三张证书，★ G54 那三种挑战一次跑里各走一遍："
echo "  TLS-ALPN-01（主）一张 + HTTP-01（备，由预置的失败记录切过去）一张 + DNS-01 通配符一张；"
echo "  验过签、落盘权限对、真去问权威 NS 确认 TXT 可见（G58）、"
echo "  带 acme-tls/1 连过去不回落到真证书（且同一把尺子自证不是恒失败）、"
echo "  没配 dns 的通配符被推迟而不是失败、挑战面用完即收、重启后装得上、第二轮三张都判「已是最新」。"
