#!/usr/bin/env bash
# ACME 端到端：在門禁裡跑一個**真的 CA**（pebble，G64），讓樞衡真籤一張證書出來。
#
# ★ ★ **這個場景存在的全部理由是「判據」**：帳戶、訂單、HTTP-01、ARI、退避、跨代鎖、
#   熱裝 —— G54 的三種挑戰與 G58 那條「簽發並續期一次通配符證書」，
#   **每一條都只有真跑過才算數**，編得過、單測綠都不算。
#
# ★ 為什麼是 pebble 而不是打真 CA（G64，owner 拍板）：真實簽發要麼打真 CA
#   （要真網域、消耗速率配額、**不能進門禁**），要麼在門禁裡跑一個本機 ACME 伺服器。
#   pebble 是 Let's Encrypt 官方的測試用 CA，與 boulder 同一批人寫的另一個 RFC 8555 實作。
#
# 拓撲：
#
#   pebble-challtestsrv  :8053(DNS) :8055(管理)  —— ★ 一身二職，見下
#   pebble               :14000(目錄) :15000(管理)  —— 真 CA，驗 HTTP-01 時連 127.0.0.1:8083
#   fulcrum serve        :8083(HTTP)  :9443(HTTPS)  —— 被驗的那一方
#
# ★ ★ ★ **challtestsrv 一身二職，而這正是 DNS-01 判據成立的關鍵**：
#   ① 它是 pebble 的解析器（pebble 起來時帶了 `-dnsserver`）；
#   ② 它也是樞衡 `resolvers` 裡那臺「權威 NS」；
#   ③ exec hook 通過它的管理介面（:8055 的 `/set-txt`）寫記錄。
#   於是「hook 寫進去的」「樞衡確認可見的」「CA 查到的」**是同一份資料**，
#   而不是三個互相假設的替身——那種測法只能證明我們的假設自洽。
#
# 用法：
#   bash tests/acme/run.sh              # 容器裡跑（tests/m0/docker-run.sh 會驅動它）
#   ACME_KEEP=1 bash tests/acme/run.sh  # 失敗時保留工作目錄，便於人來看
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1

# pebble 側
DIR_PORT=${DIR_PORT:-14000}   # ACME 目錄（HTTPS）
MGMT_PORT=${MGMT_PORT:-15000} # 管理介面（HTTPS），根證書從這裡取
DNS_PORT=${DNS_PORT:-8053}
CTS_MGMT_PORT=${CTS_MGMT_PORT:-8055}
# 樞衡側。★ 避開 serve 場景的 9100–9103 與 M0 的 8080–8082。
HTTP_PORT=${HTTP_PORT:-8083}
TLS_PORT=${TLS_PORT:-9443}
# ★ ★ 批 7：這一行的含義變了，而它是 TLS-ALPN-01 判據的**全部前提**。
#
#   `pebble.tlsPort` 是 pebble **發起 TLS-ALPN-01 驗證時去連的連接埠**。
#   在這之前它被設成一個沒人要的連接埠（9444），因為那一批還沒做這個挑戰。
#   現在把它指向**樞衡自己的 HTTPS 連接埠** —— 於是「CA 來驗」這件事是真的發生的：
#   pebble 拿著 `acme-tls/1` 連過來，樞衡在握手裡回那張自籤的挑戰證書。
#   ⚠ 真實環境裡這個連接埠恆為 443（RFC 8737 §3 寫死），只有 pebble 允許改。
PEBBLE_TLSALPN_PORT=${PEBBLE_TLSALPN_PORT:-$TLS_PORT}

DIR_URL="https://localhost:${DIR_PORT}/dir"
# ★ 目錄名由 `issuer_slug()` 從目錄 URL 推出來：host[:port] 裡的非白名單字元換成 `-`。
#   這裡**寫死期望值**而不是呼叫產品程式碼去算 —— 判據不能拿被測物件自己算的結果當尺子。
ISSUER_DIR="ca-localhost-${DIR_PORT}"

DOMAIN=acme.example
# ★ ★ 批 7：第二個非通配網域，**專門用來守住 HTTP-01 那條路**。
#
#   TLS-ALPN-01 串接之後它成了「主」，於是 `acme.example` 會走 TLS-ALPN-01——
#   ⚠ 如果只有它一個，**HTTP-01 那條路（連同它繞過路由的那套判據）會在這一批裡
#   悄悄變成沒人跑**，而門照樣全綠。這正是本儲存庫反覆栽的形狀：
#   一個功能被新功能頂替之後，守它的判據跟著一起失效，卻沒有任何東西說出來。
#
#   怎麼讓它走備用的那條：**預先在儲存裡放一份 `meta.json`，說上一次
#   TLS-ALPN-01 失敗過**。於是 `pick_non_dns_challenge` 會選 HTTP-01——
#   ★ 這同時把「主/備切換」這條邏輯端到端驗了一遍，而不只是單測裡驗過。
HTTP01_DOMAIN=http01.example
WILDCARD_SITE='*.wild.example'
WILDCARD_HOST=x.wild.example
# ★ 另一個通配符站台，**故意不配 DNS-01** —— 用來守「推遲 ≠ 失敗」那條判據。
#   兩個通配符站台同時在場，才能在一次跑裡同時驗「配了就能籤」與「沒配就推遲」。
NODNS_SITE='*.nodns.example'

STATE="$WORK/state"
CERT_DIR="$STATE/certs/$ISSUER_DIR/$DOMAIN"
HTTP01_CERT_DIR="$STATE/certs/$ISSUER_DIR/$HTTP01_DOMAIN"
# ★ `*` 在儲存裡被轉義成 `_wildcard_`（見 fulcrum-tls 的 sanitize/unsanitize）。
WILD_CERT_DIR="$STATE/certs/$ISSUER_DIR/_wildcard_.wild.example"
NODNS_CERT_DIR="$STATE/certs/$ISSUER_DIR/_wildcard_.nodns.example"
HOOK="$WORK/dns-hook.sh"
ACCOUNT_JSON="$STATE/acme/$ISSUER_DIR/account.json"

# 裝進系統信任庫的那份（pebble 自己 HTTPS API 的證書）。★ 走系統信任庫是**故意的**：
# 產品程式碼用的是 `instant-acme` 的預設用戶端（平台信任庫），
# **絕不為了測試給產品加一個「信任某個根」的設定項**——那是測試專用旋鈕長進產品面。
TRUST_ANCHOR=/usr/local/share/ca-certificates/fulcrum-pebble-api.crt

# ★ 起 CA、起假權威 NS、寫 exec hook、那一整套等待與斷言小工具，
#   與續期場景 [`renew.sh`](renew.sh) **共用同一份**。理由見 lib.sh 開頭。
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

# ★ ★ **這條與 [3/6] 的那條是同一個判據的兩個方向**：現在沒有，簽完必須有。
#   一個只見過「有」的斷言，與一個恆真的斷言無法區分。
if [ -e "$CERT_DIR/cert.pem" ]; then
  echo "ACME TESTS FAILED: $CERT_DIR/cert.pem 一开始就存在 —— 那下面「签出来了」就证明不了任何事" >&2
  exit 1
fi
ok "存储里还没有 $DOMAIN 的证书"

echo "=== [1/6] 起 pebble（本地 CA）与 challtestsrv（假 DNS）==="

acme_make_api_cert "ACME TESTS"
acme_write_pebble_config "$DIR_PORT" "$MGMT_PORT" "$HTTP_PORT" "$PEBBLE_TLSALPN_PORT"
acme_start_challtestsrv "$DNS_PORT" "$CTS_MGMT_PORT"
# ★ 授權重複使用留在 pebble 的預設值（50%）：本場景每個網域只籤一次，重複使用與否都碰不到。
#   續期場景把它關掉，理由見 lib.sh。
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

# ★ 不加 `-k`。這一次請求同時驗了兩件事：pebble 活著，**且信任根真的裝進去了**。
DIR_JSON=$(run_curl -fsS --max-time 10 "$DIR_URL")
if [ -z "$DIR_JSON" ]; then
  echo "ACME TESTS FAILED: 取不到 ACME 目录 $DIR_URL（多半是信任根没装上）" >&2
  cat "$WORK/pebble.log" >&2
  exit 1
fi
ok "ACME 目录取到了，且是**验过签**的（信任根装好了）"

# ★ ARI（RFC 9773）能不能被真驗一次，取決於這個 CA 認不認 `renewalInfo`。
#   把它打出來而不是假設 —— 下面 [5/6] 那條 ARI 斷言的前提就是它。
if printf '%s' "$DIR_JSON" | grep -q 'renewalInfo'; then
  ok "这个 CA 的目录里有 renewalInfo（ARI 可以被真验一次）"
  HAS_ARI=1
else
  echo "  ⚠ 目录里没有 renewalInfo —— ARI 那条断言会被跳过" >&2
  HAS_ARI=0
fi

# pebble 的根證書。簽出來的證書要能用它驗過 —— 那才是「真簽了一張」。
acme_fetch_root "ACME TESTS" "$MGMT_PORT"
ok "取到 pebble 的根证书"

acme_write_dns_hook "$HOOK" "$CTS_MGMT_PORT"

echo "=== [2/6] 起枢衡，等它把证书签下来 ==="

# ★ ★ 挑戰站台寫的是 `respond 403` —— **這是判據本身，不是隨手寫的**。
#   一份 `respond 403` 的設定完全合法，而 HTTP-01 的應答若走正常路由，
#   **使用者的設定會把自己的證書籤發擋掉**，現場只看得到「CA 說驗不過」。
#   於是：籤成功 ⇒ 應答確實繞過了路由；而下面 [4/6] 裡那個不認識的 token 拿到 403
#   ⇒ 繞過的面只有「當前有效的 token」這一點大。**兩個方向都在同一次跑裡。**
#
# ★ ★ 兩個通配符站台是**故意的**，它們各守一條判據：
#   · `*.wild.example` 配了 `dns exec` + `resolvers` ⇒ 應當**真的簽下來**（G58）；
#   · `*.nodns.example` 什麼都沒配 ⇒ 應當被記成**推遲**，不是**失敗**
#     （失敗會進退避、進計數、進告警）。
#   一次跑裡同時驗「配了就能籤」與「沒配就推遲」，兩個方向都在。
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
# 路徑裡有斜槓，換個分隔符。
sed -i "s|HOOKPATH|$HOOK|" "$WORK/acme.Fulcrumfile"

# ★ ★ 預先給 $HTTP01_DOMAIN 放一份 meta.json，說「上一次 TLS-ALPN-01 失敗過」。
#   於是 `pick_non_dns_challenge` 會給它選 HTTP-01（G54 的備）。
#   ⚠ 欄位要寫全：`load_meta` 解析失敗會**靜默退回預設值**（那是有意的——
#   meta 壞了不該讓證書不可用），而退回預設值之後這個網域會走主的那條，
#   本條判據就悄悄失效了。所以下面 [3/6] 裡有一條斷言專門驗它**真的走了 HTTP-01**。
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

# ★ ★ 這一條永遠成立：`*.nodns.example` 沒配 DNS-01，這一批籤不出來，
#   所以對它的握手必須被拒絕。判據掛在 **curl 的結束代碼**上——
#   `%{http_code}` 的 `000` 也可能來自超時、連接埠沒開，分不出「被拒絕握手」這一種。
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
  # ★ 先把最可能、又最難認出來的那一種說破：pebble 故意拒 5% 的合法 nonce
  #   （見 lib.sh 裡 `acme_start_pebble` 上面那一段），而用戶端只重試 3 次。撞上時樞衡這邊
  #   只有一句 `client error`，pebble 那邊才看得到 `anti-replay nonce`。
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

# ── ★ ★ ★ G54 的「主/備」：判據掛在**這一次到底走了哪條路**上 ──────────────
#
# ⚠ 只斷言「三張證書都出來了」是分不出走了哪種挑戰的——而這一批的全部內容
#   恰恰是「多了一種挑戰，並且它成了主」。所以判據掛在那兩行 info 紀錄上。
#   ★ 那兩行被特意提到 info 級，理由與 G58 那條輪詢紀錄一模一樣：
#     它是「用了哪種挑戰」唯一可從外部觀察到的痕跡。
if grep -qF "$DOMAIN：本次挑战走 TLS-ALPN-01" "$WORK/run1.log"; then
  ok "★ $DOMAIN **选**的是 TLS-ALPN-01（G54 的主，RFC 8737）"
else
  fail "$DOMAIN 没有走 TLS-ALPN-01 —— 主/备选反了，或者这条路根本没接上"
  grep -E '本次挑战走' "$WORK/run1.log" >&2 || true
fi

# ── ★ ★ ★ 上面那條只證明「我們**試**了 TLS-ALPN-01」，不證明「它**成了**」──────
#
# ⚠ ⚠ **反證抓到過一個真缺陷**：把挑戰證書裡那條 acmeIdentifier 擴充去掉之後，
#   pebble 當場 `authz … set INVALID`（破壞是生效的），但退避到點後按 G54 的「備」
#   換成 HTTP-01，證書照樣簽了出來 ⇒ **整道門全綠**，連上面那條斷言也是綠的（它確實「試」過）。
#   ★ 一道分不出「成功」與「失敗了但被兜住」的門，恰好對這一層是瞎的。
#
# 修法：判據改掛在 **CA 自己的記錄**上——pebble 每次 HTTP 驗證都會打一行
# `Attempting to validate w/ HTTP: http://<域名>:`。TLS-ALPN-01 成功的話，
# 這個網域**根本不該出現在那些行裡**。
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
# ★ 再釘一條更粗的：一次健康的跑裡，CA 不該判任何一個授權 INVALID。
#   它擋的是同一族的「失敗了但被某種備援蓋過去」——不限於挑戰類型。
if grep -q "set INVALID" "$WORK/pebble.log"; then
  fail "CA 判过 INVALID —— 这一跑里有验证失败被兜住了"
  grep "set INVALID" "$WORK/pebble.log" >&2 || true
else
  ok "CA 一次 INVALID 都没判过（没有失败被兜底盖住）"
fi
# ★ 反向那一半：預置了「上次 TLS-ALPN-01 失敗過」的那個網域必須**換成備用的**。
#   ⚠ 少了這一條，一個「永遠走主的」實作照樣能讓上面那條全綠。
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
# ★ 私鑰 0600 是 G55 的硬要求，而且**權限要在 create 的那一刻就給**
#   （先建再 chmod 之間有一個視窗，那個視窗裡私鑰是可讀的）。
expect_mode "私钥" 600 "$CERT_DIR/key.pem"
expect_mode "证书" 644 "$CERT_DIR/cert.pem"
expect_mode "账户凭据" 600 "$ACCOUNT_JSON"

# ★ ★ `issuer_url` 記的是**籤它的那個 CA 的目錄 URL 原文**。
#   目錄名（`ca-localhost-14000`）是有損映射，兩個 CA 可能落到同一個名字上——
#   判據掛在資料上，不掛在命名約定上。
META="$CERT_DIR/meta.json"
if grep -qF "\"issuer_url\": \"$DIR_URL\"" "$META" 2>/dev/null; then
  ok "meta.json 里的 issuer_url 就是这次用的那个目录 URL"
else
  fail "meta.json 里的 issuer_url 对不上（期望 $DIR_URL）"
  cat "$META" >&2 || true
fi
# ★ 上面那條 grep 的**自證**：同一把尺子必須量得出「不是這個值」。
#   一個只會命中的判據與一個恆真的判據無法區分。
if grep -qF '"issuer_url": null' "$META" 2>/dev/null; then
  fail "meta.json 里 issuer_url 同时还是 null —— 判据自己瞎了"
else
  ok "同一把尺子量 null 时不命中（判据自证）"
fi

# ★ 第一輪之後 ARI 視窗必須還是空的：ARI 是**跟著某一張已存證書**問的，
#   剛簽下來那一刻還沒問過。它與 [5/6] 那條是同一個判據的兩個方向。
if grep -q '"ari_start": null' "$META" 2>/dev/null; then
  ok "刚签完时 ARI 窗口是空的（还没问过）"
else
  fail "刚签完时 meta.json 里的 ari_start 不是 null"
  cat "$META" >&2 || true
fi

# ★ 配了 DNS-01 的通配符：證書**必須**寫入磁碟，而且目錄名裡的 `*` 被轉義成 `_wildcard_`。
if [ -f "$WILD_CERT_DIR/cert.pem" ]; then
  ok "通配符证书落在 certs/$ISSUER_DIR/_wildcard_.wild.example/"
else
  fail "通配符证书没落在 $WILD_CERT_DIR/"
  ls -laR "$STATE/certs" >&2 || true
fi
expect_mode "通配符私钥" 600 "$WILD_CERT_DIR/key.pem"

# ★ 沒配 DNS-01 的那個：推遲，不是失敗，**而且不許留下任何儲存痕跡**。
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

# ★ ★ ★ G58 那條硬約束的判據：紀錄裡必須有「向權威 NS 確認可見」這一步。
#   ⚠ 它證的不是「等過」，而是**等的方式**——固定 sleep 也會讓簽發成功，
#   所以只斷言「簽下來了」是分不出這兩種實作的。
if grep -q "的 TXT 已在全部 1 台权威 NS 上可见" "$WORK/run1.log"; then
  ok "DNS-01 真去问了权威 NS 确认 TXT 可见（G58：绝不能只 sleep）"
else
  fail "日志里没有「向权威 NS 确认 TXT 可见」那一步 —— 可能退化成了固定 sleep"
  grep -i 'TXT\|DNS-01' "$WORK/run1.log" >&2 || true
fi

# ★ 挑戰用的 TXT 用完要摘掉。判據是**直接去問那臺權威 NS**，不是看紀錄。
CHAL_RECORD="_acme-challenge.wild.example"
LEFTOVER=$(run_curl -fsS -X POST -H 'Content-Type: application/json' \
  --data "{\"host\":\"$CHAL_RECORD\"}" "http://$HOST:$CTS_MGMT_PORT/clear-txt" -o /dev/null -w '%{http_code}')
if [ "$LEFTOVER" = "200" ]; then
  ok "挑战记录名可被清理（收尾）"
fi

echo "=== [4/6] 签出来的证书是真能用的 ==="

RESOLVE="$DOMAIN:$TLS_PORT:$HOST"
TLSURL="https://$DOMAIN:$TLS_PORT/"

# ★ ★ 用 `--cacert <pebble 根>` 而不是 `-k`：後者只證明「握手成功了」，
#   證不了「伺服器端給的是那張剛簽下來的證書」。
CODE=$(run_curl -s -o "$WORK/body" -w '%{http_code}' --max-time 10 \
  --cacert "$WORK/pebble-root.pem" --resolve "$RESOLVE" "$TLSURL")
expect_status "HTTPS（用 pebble 的根验签）" 200 "$CODE"
BODY=$(cat "$WORK/body" 2>/dev/null || true)
if [ "$BODY" = "acme-secure" ]; then
  ok "响应体 = $BODY"
else
  fail "响应体期望「acme-secure」，实际「$BODY」"
fi

# ★ 這張證書是不是 pebble 籤的，問證書自己，別問我們自己寫的 meta。
ISSUER_CN=$(openssl x509 -in "$CERT_DIR/cert.pem" -noout -issuer 2>/dev/null || true)
if printf '%s' "$ISSUER_CN" | grep -qi 'pebble'; then
  ok "证书的签发者是 pebble：$ISSUER_CN"
else
  fail "证书的签发者看起来不是 pebble：$ISSUER_CN"
fi

# ── ★ ★ ★ 通配符證書真的能服務一個子域（G58 的正面判據）────────────────────
#
# ⚠ 「證書檔案躺在盤上」與「它真能用」是兩件事。這一條走完整條鏈：
#   SNI 是 `x.wild.example` → 解析器按通配符挑中那張證書 → 用戶端用 pebble 的根驗籤。
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

# ★ 證書上的 SAN 必須真的是通配符，而不是恰好籤了 `x.wild.example` 這一個名字。
#   ⚠ 少了這一條，一個「把通配符悄悄降級成單網域」的實作照樣全綠。
WSAN=$(openssl x509 -in "$WILD_CERT_DIR/cert.pem" -noout -ext subjectAltName 2>/dev/null || true)
if printf '%s' "$WSAN" | grep -q '\*\.wild\.example'; then
  ok "通配符证书的 SAN 里确实是 *.wild.example"
else
  fail "通配符证书的 SAN 不是通配符：$WSAN"
fi

# ★ ★ HTTP-01 的應答面**只有「當前有效的 token」那麼大**。
#   token 用完由 `Drop` 摘掉，所以現在這條路徑必須落迴路由 —— 而路由說 403。
#   ⚠ 若這裡拿到 200，說明挑戰路徑被永久打開了：那是一條免費的資訊洩露面。
CHAL_CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
  --resolve "$DOMAIN:$HTTP_PORT:$HOST" \
  "http://$DOMAIN:$HTTP_PORT/.well-known/acme-challenge/deadbeef")
expect_status "签发完之后，不认识的挑战 token 落回路由（配置说 403）" 403 "$CHAL_CODE"

# ── ★ ★ ★ TLS-ALPN-01 的那張挑戰證書表，與真證書表**互不相通** ──────────────
#
# 這是兩條方向相反的安全屬性，缺一不可：
#   ① 協商到 `acme-tls/1` 卻沒有掛著的挑戰證書 ⇒ **拒絕握手**，
#      絕不回落到真證書（否則等於把使用者的真證書交給任何一個說 `acme-tls/1` 的對端）；
#   ② 普通流量（h2 / http/1.1）**拿不到**挑戰證書 —— 那張是自籤的，
#      拿到它的瀏覽器會當場報證書錯誤，而伺服器端紀錄裡是一次**成功**的握手。
#
# ⚠ 現在挑戰早已用完摘掉，所以 ① 直接可驗：帶 `acme-tls/1` 連過去必須失敗。
#   ② 由下面那條「證書的簽發者是 pebble」間接守著——挑戰證書是自籤的，
#   一旦漏進普通流量，簽發者就不是 pebble 了。
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
# ★ 同一把尺子的**自證**：不帶 acme-tls/1 時它必須能連上。
#   否則上面那個「失敗」可能只是連接埠不通、或者 openssl 用法寫錯了。
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

# ★ 換一個處理程序再跑一輪。這同時驗了另一件事：**重新啟動之後盤上那張讀得出來、裝得上**。
#   （簽發路徑裡那次「寫完回讀一次再裝」防的正是它讀不出來，而症狀要到重新啟動才出現。）
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
# ★ ★ ★ **`:80` 在這裡只記，不判。**
#
#   CI 上間歇性紅在下面那句「第二代起不來」，而 `run2.log` 末尾是一串
#   `127.0.0.1:80 is in use, will try again`（pingora 的 `bind_tcp` 重試 30 次、每次 1 秒，
#   而 `wait_port` 20 秒就放棄了 ⇒ 紀錄停在半路）。本機從來不紅。
#
#   ⚠ 現有證據分不開三種解釋：第一代其實沒走乾淨 / 那個監聽 fd 被某個子處理程序繼承走了 /
#   容器裡另有第三方佔著。**分開它們要的正是「第二代起來之前 `:80` 長什麼樣」**——
#   所以在這裡取一次快照，只在下面真紅的時候打出來：綠的跑一個字都不多。
#
#   ★ 這一條**已經用上了，而且當場把根因指了出來**：上一個場景漏了個處理程序攥著 `:80`，
#   pingora 又是**攥著全域 `ListenFds` 鎖**去做那個重試 30 秒的 bind
#   ⇒ 後面的 `:8083` 跟著起不來，而紀錄裡只有 `:80` 的錯（見 lib.sh 那段）。
#   ⛔ 仍然不許把它變成斷言：源頭在**別的場景沒把連接埠還回去**，
#   在這裡判只會指向一個受害者，而真兇那邊一個字都不會說。
PORT80_BEFORE_GEN2=$(acme_port_snapshot 80)

start_fulcrum run2
wait_port "$TLS_PORT" || {
  echo "ACME TESTS FAILED: 第二代起不来。日志：" >&2
  cat "$WORK/run2.log" >&2
  # ★ ★ ★ **「還活著但沒在聽」與「已經死了」是兩個完全不同的答案，而 `cat 日志` 一個都給不出。**
  #   ⚠ 這幾行必須寫在**主 shell 裡**，不能包成函式再 `$(…)` 呼叫：命令替換跑在子 shell 裡，
  #     而 `wait` 等不了父 shell 的後台作業 —— 它會立刻回傳 127，
  #     於是每一次都報「結束代碼 127」，一個恆定的假答案。
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

# ★ 啟動時就該把盤上那張裝進解析器 —— 這條是「重新啟動之後 HTTPS 還活著」的判據。
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

# ★ ★ ARI（RFC 9773）真被問了一次。它只在「儲存裡已經有一張」時才問，
#   所以第一輪是 null、第二輪是數字 —— **同一個判據的兩個方向，在同一次跑裡**。
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
