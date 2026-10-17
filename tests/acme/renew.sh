#!/usr/bin/env bash
# ACME 續期端到端：讓**跑著的**樞衡自己走到續期，真重籤一張**通配符**證書。
#
# ★ **它補的是 G58 的另一半。** G58 寫的是「至少簽發**並續期**一次通配符證書」，
#   而 `tests/acme/run.sh` 只驗到「第二輪判**已是最新**」—— 那恰恰是「**沒有**續期」的判據。
#   ⚠ 「暫時不用續」與「該續時會續」是兩件相反的事，前者綠一萬次也證不出後者。
#
# ★ ★ **判據掛在方法上，不只掛在結果上**：一張新證書出現在盤上，可能來自「按 ARI 續期」，
#   也可能來自「舊的沒讀出來於是重籤」。所以同時釘三樣：① 紀錄裡那句
#   `该续期了：CA 的 ARI 建议窗口已经开始`（**誰下的決定**）、② 證書**序列號變了**、
#   ③ 續期那一趟的挑戰**按 CA 的選擇走對了**（CA 新建授權就重解 DNS-01，重複使用有效授權就不解）。
#
# ★ ★ 第一代的序列號若帶 DER 符號位元組，就在續期之前重籤一張（[2/5] 末尾，G166）：
#   pebble v2.10.1 認不出那種證書的 `replaces`，而本格要證的是 `replaces` 被接受那一條。
#
# 單獨一個場景 + 單獨一套連接埠 + 單獨一個 pebble 執行個體：短壽命證書會汙染同一個 CA 下的其它
#   斷言（run.sh 有好幾條依賴「剛簽完的證書還早得很」）。共用一個 CA 等於讓兩個場景互相設定前提。
#
# 拓撲（連接埠全部與 run.sh 錯開，見 AGENTS.md 的連接埠表）：
#
#   pebble-challtestsrv  :8054(DNS) :8056(管理)
#   pebble               :14001(目錄) :15001(管理)   —— profile 把證書壽命壓到 ~1.5 天
#   fulcrum serve        :8084(HTTP)  :9445(HTTPS)
#
# ★ ★ ★ **三條反證（實做，每條都改產品程式碼、跑一遍、再改回去）**：
#
#   | 擾動 | 結果 |
#   |---|---|
#   | ARI 那一支 `renew_now: false`（假裝分支被改壞）| 紅在「沒等到該續期」+「沒等到簽發成功」；紀錄裡它每 60s 判一次「已是最新」——**正是這道門要抓的那種無聲地永不續期** |
#   | 簽完不 `install`（假裝忘了熱裝）| **只紅一條**：線上握手拿到的是**舊**序列號。⚠ 盤上換了、HTTPS 仍然 200 —— 一道只驗「還能訪問」或「盤上變了」的門在這裡全綠 |
#   | 判定改成恆真 | 紅在 [3/5] 那兩條反向斷言。其餘全綠 —— 也就是說**一道只斷言「續期發生了」的門，對「每 60s 重籤一次」是瞎的**，而那在真 CA 上直接燒掉速率配額 |
#
#   ★ 第三條還量出一件事：判「該續」時 `next_check` 是 `MIN_CHECK`(60s)，
#   與簽發後那條 3600s 取 min 之後仍是 60s。真實證書上這只是「續完 60s 後多看一眼」，
#   無害；但它說明「續期後多久再看」這條路是**由續期判定而不是簽發結果**決定的。
#
# 用法：
#   bash tests/acme/renew.sh              # 容器裡跑（tests/m0/docker-run.sh 會驅動它）
#   ACME_KEEP=1 bash tests/acme/renew.sh  # 失敗時保留工作目錄
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

# ★ ★ ★ **這個數字是量出來的，不是猜的。改它之前先讀完這一段。**
#
# 實測（pebble v2.10.1）：ARI 建議視窗 =
#   `[notAfter − 寿命/3 − 1 天, notAfter − 寿命/3 + 1 天]`
# —— **那個 ±1 天是絕對值，不隨壽命縮放**。兩個量級各驗過一次：
#   · 90 天證書：視窗在壽命的 65%–67%，寬度正好 2 天；
#   · 129700s 證書：預測 `2L/3 − 86400 = 66s`，實測 **65s**（差 1s 來自證書只精確到秒）。
#
# 於是「ARI 視窗開始的時刻」= notBefore + (2L/3 − 86400)。要讓它落在**簽發之後幾十秒**，
# L 就得在 129600s（1.5 天）出頭一點點。取 129700 ⇒ 視窗在簽發後約 65s 開始。
#
# ⚠ ⚠ 為什麼不乾脆取幾分鐘的壽命：那樣 `2L/3 − 86400` 是**負數**，
#   ARI 視窗在證書籤發之前就已經開始了，於是第二代**第一輪巡檢就直接續**——
#   跑得更快，但**證不到「它自己等到了時候才動」**，而那正是續期的全部內容。
#   ★ 同一族的教訓：一個恆真的判據與一個正確的判據無法區分。
#
# ⚠ 另一個下界：`fulcrum_tls::renewal::MIN_CHECK` 是 60s，巡檢不會睡得比它更短。
#   視窗若在 65s 處開始，實際就是「睡 ~63s 再回來」——本場景的用時基本就是這 65s。
VALIDITY=${VALIDITY:-129700}
# 等續期最多等多久。★ 比 65s 寬出一大截：這裡寧可慢，也不要在慢一點的機器上偶發紅。
RENEW_WAIT=${RENEW_WAIT:-180}

DIR_URL="https://localhost:${DIR_PORT}/dir"
ISSUER_DIR="ca-localhost-${DIR_PORT}"

# ★ 被續的是**通配符**，因為 G58 要的就是通配符那一張。順帶：通配符只能走 DNS-01。
WILDCARD_SITE='*.renew.example'
WILDCARD_HOST=a.renew.example
STATE="$WORK/state"
# ★ `*` 在儲存裡被轉義成 `_wildcard_`（見 fulcrum-tls 的 sanitize/unsanitize）。
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

# 從 PEM 裡取序列號 / 起止時刻。★ 判據裡的「變了沒有」全靠它們。
cert_serial() { openssl x509 -in "$1" -noout -serial | cut -d= -f2; }
cert_epoch() {
  # $2 是 -startdate 或 -enddate
  date -d "$(openssl x509 -in "$1" -noout "$2" | cut -d= -f2)" +%s
}
# `cert_serial` 打的是**逐位元組**兩位十六進制（恆為偶數位）⇒ 首位是 8–F ⇔ 首位元組 ≥ 0x80
#   ⇔ DER 要在前面補一個符號位元組 `00`，ARI 標識裡的序列號也就帶著它（RFC 9773 §4.1）。
needs_sign_byte() {
  [ $((${#1} % 2)) -eq 0 ] || return 1
  case "$1" in
    [89A-Fa-f]*) return 0 ;;
    *) return 1 ;;
  esac
}
# 發 SIGINT、最多等 10s。⚠ 等完不判活沒活 —— 呼叫的地方各有各的判法。
stop_fulcrum() {
  local waited=0
  kill -INT "$FULCRUM_PID" 2>/dev/null || true
  while kill -0 "$FULCRUM_PID" 2>/dev/null && [ "$waited" -lt 100 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
}
# pebble 每**新建**一份授權打一句 `There are now N authorizations in the db`，取最後那個 N。
# ⚠ `|| true` 不能省：一句都沒有時 grep 退 1，`pipefail` + `set -e` 會讓整個指令碼靜默退出。
authz_total() {
  grep -aoE 'There are now [0-9]+ authorizations in the db' "$WORK/pebble.log" |
    tail -1 | awk '{print $4}' || true
}

echo "=== [0/5] 基线：端口是空的、存储里还没有证书 ==="

acme_require_tools "ACME RENEW TESTS"
[ -x "$BIN" ] || {
  echo "ACME RENEW TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ★ ★ [2/5] 末尾那段「第一代重籤」只為 pebble v2.10.1 的一個缺陷而設（G166）。
#   ⚠ 版本一變就紅，⛔ 不許改成「不是 v2.10.1 就跳過」：升級的人必須回來確認新版本
#   含上游 `1fcb30c`（issue #550），再刪掉那一段和這一條 —— 留著它會讓本格少測一種序列號。
PEBBLE_VER=$(pebble -version 2>&1 | head -1 || true)
case "$PEBBLE_VER" in
  *' v2.10.1 '*) ok "pebble 是 v2.10.1（[2/5] 末尾的第一代重签只为它而设）" ;;
  *)
    echo "ACME RENEW TESTS FAILED: pebble 版本是「${PEBBLE_VER:-（取不到）}」，不再是 v2.10.1 ——" >&2
    echo "  先确认它含上游 1fcb30c（issue #550），再删掉 [2/5] 末尾的第一代重签与这一条（G166）" >&2
    exit 1
    ;;
esac
# ★ 判據函式的自證：真撞上過的那個序列號要判「帶」、去掉最高位要判「不帶」，兩頭都得對。
#   ⚠ 只驗一頭的話，一個恆真或恆假的 `needs_sign_byte` 都能過。
if needs_sign_byte B36F9903F33BC7 && needs_sign_byte 80 &&
  ! needs_sign_byte 336F9903F33BC7 && ! needs_sign_byte 7F; then
  ok "needs_sign_byte 两头都判得对（B36F9903F33BC7 带、336F9903F33BC7 不带）"
else
  echo "ACME RENEW TESTS FAILED: needs_sign_byte 自证不过 —— [2/5] 末尾的前提控制不可信" >&2
  exit 1
fi
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
# ⚠ ⚠ 生效的是 `profiles`，**不是** `certificateValidityPeriod`（後者在 v2.10.1 裡是死設定，
#   實測詳見 lib.sh 裡 `acme_write_pebble_config` 上面那一段）。
# ⚠ profile 必須叫 `default`：實測另起名字之後，目錄裡就沒有 default 了，
#   而用戶端不選 profile 時沒有備援。
acme_write_pebble_config "$DIR_PORT" "$MGMT_PORT" "$HTTP_PORT" "$PEBBLE_TLSALPN_PORT" \
  "\"profiles\": { \"default\": { \"description\": \"short-lived\", \"validityPeriod\": $VALIDITY } },"
acme_start_challtestsrv "$DNS_PORT" "$CTS_MGMT_PORT"
# ★ ★ `PEBBLE_AUTHZREUSE=0`：pebble 預設有 50% 概率重複使用上一次的授權，
#   那樣**續期那一趟根本不走挑戰**，「續期時又驗了一遍 DNS-01」那一支就只有一半的趟次判得到。
#   ⚠ ⚠ 設成 0 也**不是**「永不復用」，而是每趟 1/100（pebble 的差一，原文見下面判據四）
#   ⇒ 判據四**不靠這一行**決定走哪一支，而是從 CA 的紀錄讀出這一趟走了哪一支、再按那一支判；
#   這一行只讓「續期重解挑戰」那一支成為常態（99/100），每趟門禁幾乎都真的判到它。
#   授權重複使用是 CA 側的優化，與「樞衡在需要時會不會重新解一次挑戰」無關。
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

# ★ ★ **自證第一半：短壽命真的生效了。**
#   少了這一條，一個「profile 沒被認出來」的 pebble 會照常簽出 90 天的證書，
#   而本場景的表現是「等了 180s 沒等到續期」——那條回報錯誤會把人引向續期邏輯，
#   而真正壞掉的是這一行設定。⚠ 把「前提沒成立」誤報成「被測物件壞了」，
#   比直接紅更貴：它會讓人去改一段本來是對的程式碼。
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
# ⚠ 這裡**不能像 run.sh 那樣「沒有 renewalInfo 就跳過」**：本場景要驗的續期
#   正是由 ARI 觸發的。CA 不給 ARI，這個場景就什麼也證不了，那必須是紅，不是跳過。
#   ★ 「跳過」寫成「綠」，是把一道門悄悄拆掉。
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

# ★ ★ **自證第二半：從產物上再量一次壽命。**
#   上面量的是 pebble 說了什麼，這裡量的是它**真的簽了什麼**。
#   ⚠ 允許幾秒誤差：證書的時刻只精確到秒，編碼時會截斷。
if [ "$LIFETIME" -ge "$((VALIDITY - 5))" ] && [ "$LIFETIME" -le "$((VALIDITY + 5))" ]; then
  ok "签下来这张的寿命 = ${LIFETIME}s（≈ 配置的 ${VALIDITY}s）"
else
  fail "签下来这张的寿命是 ${LIFETIME}s，期望 ≈ ${VALIDITY}s —— 短寿命没落到证书上"
fi

# 續期那一趟要跟它比，所以這裡先釘住「第一趟確實走了 DNS-01」。
if grep -qF "的 TXT 已在全部 1 台权威 NS 上可见" "$WORK/gen1.log"; then
  ok "第一趟真去问了权威 NS 确认 TXT 可见（G58）"
else
  fail "第一趟日志里没有「向权威 NS 确认 TXT 可见」那一步"
fi

# ★ ★ ★ **前提控制：第一代的序列號不許帶 DER 符號位元組（G166）。**
#   pebble v2.10.1 存訂單的鍵是 `hex(big.Int.Bytes())`（不帶符號位元組），查 `replaces` 時
#   卻用 ARI 標識裡的原始位元組 —— 而 RFC 9773 §4.1 規定首位元組 ≥ 0x80 時要帶上那個 `00`。
#   ⇒ 這種證書帶 `replaces` 下單，pebble 一律回 `serverInternal`
#   （`could not find order resulting in the given certificate serial number`；
#   上游 issue #550，修在 main `1fcb30c`，沒發版）。pebble 的序列號 < 2^63 ⇒ 約 1/255。
#   ★ 樞衡碰上會去掉 `replaces` 再下一次、照樣續上（G166 的產品那一半，單測守著）——
#   ⚠ 但本格要證的是「`replaces` 被 CA 接受」那一條（判據二之二），所以在第二代起來之前換一張。
#   ⛔ 這**不是**「加重試」：換不換由一個確定的位元組決定，發生在被測的續期**之前**，
#   不碰續期本身 —— 續期那一趟失敗照樣紅。
REROLLS=0
while needs_sign_byte "$SERIAL_1"; do
  REROLLS=$((REROLLS + 1))
  if [ "$REROLLS" -gt 3 ]; then
    echo "ACME RENEW TESTS FAILED: 连着 4 张序列号都带符号字节（每张约 1/255）—— \
不是运气，去查 pebble 的序列号是怎么来的" >&2
    exit 1
  fi
  echo "  ⟳ 第一代序列号 $SERIAL_1 带 DER 符号字节（pebble v2.10.1 #550）⇒ 丢掉它重签（第 $REROLLS 次）"
  stop_fulcrum
  # ⚠ 舊處理程序還佔著連接埠時，下面的 `wait_port` 會量到它而立刻回傳 —— 那就不是在等新的一代了。
  if kill -0 "$FULCRUM_PID" 2>/dev/null || port_listening "$TLS_PORT" ||
    port_listening "$HTTP_PORT"; then
    echo "ACME RENEW TESTS FAILED: 重签之前第一代没退干净（pid $FULCRUM_PID）" >&2
    exit 1
  fi
  rm -rf "$CERT_DIR"
  start_fulcrum "gen1-reroll$REROLLS"
  wait_port "$TLS_PORT" || {
    echo "ACME RENEW TESTS FAILED: 重签的第一代起不来。日志：" >&2
    cat "$WORK/gen1-reroll$REROLLS.log" >&2
    exit 1
  }
  if ! wait_log "$WORK/gen1-reroll$REROLLS.log" "ACME 签发成功：$WILDCARD_SITE" 120; then
    echo "ACME RENEW TESTS FAILED: 重签的第一代 120s 内没签下来。" >&2
    cat "$WORK/gen1-reroll$REROLLS.log" >&2
    echo "── pebble ──" >&2
    tail -40 "$WORK/pebble.log" >&2
    exit 1
  fi
  SERIAL_1=$(cert_serial "$CERT_DIR/cert.pem")
  NOT_BEFORE_1=$(cert_epoch "$CERT_DIR/cert.pem" -startdate)
done
# 判據四要的是「續期那單新建了幾份授權」，基線取在第一代**最後**簽完之後。
AUTHZ_BASE=$(authz_total)

echo "=== [3/5] 第二代：先判「还不到时候」——续期的反面必须先成立 ==="

# ★ ★ 為什麼必須換一代：簽完那一輪的 `next_check` 是**寫死的 3600s**
#   （`issue.rs` 裡 `report.note_next_check(Duration::from_secs(3600))`），
#   而 ARI 是「盤上已經有一張」時才去問的。所以第一代要等一小時才會再看一眼，
#   門禁等不起。⚠ 這不是缺陷：真實證書是 90 天 / 6 天量級，一小時無關緊要。
#   ★ 順帶這一步也驗了「重新啟動之後盤上那張讀得出來、裝得上」。
stop_fulcrum
for p in "$HTTP_PORT" "$TLS_PORT"; do
  if port_listening "$p"; then
    fail "第一代退了，端口 $p 还被占着 —— 第二轮测的会是别人的服务"
  fi
done
# ★ 與 [`run.sh`](run.sh) 同一處取證，理由寫在那邊：`:80` 只記不判。
#   ⚠ 本場景的 `:80` 與 run.sh 的是**同一個**（重定向連接埠寫死，兩邊都隱式用它），
#   所以兩個場景的現場都要留下來 —— 只在 run.sh 那邊留，換成本場景紅的時候就又什麼都沒有。
PORT80_BEFORE_GEN2=$(acme_port_snapshot 80)

start_fulcrum gen2
wait_port "$TLS_PORT" || {
  echo "ACME RENEW TESTS FAILED: 第二代起不来。日志：" >&2
  cat "$WORK/gen2.log" >&2
  # ★ 與 run.sh 同一處，連同那條「不許包成函式」的理由（`wait` 在子 shell 裡恆 127）。
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
  # ⚠ 這條是**續期判據的反面**，缺了它「後面續了」就可能只是「它每輪都續」。
  ok "第二代第一轮判「已是最新」——还不到 ARI 窗口，所以不动"
fi

# ★ ★ 把「此刻簽發成功出現過幾次」記下來當基線。
#   ⚠ 下面那條判據要的是「**又**簽了一張」，而 `wait_log` 只會告訴你「出現過」。
#   上面那條斷言已經說明第一輪沒簽，但**判據不該建在另一條判據的結論上**：
#   那條一旦被改動或放寬，這條會跟著悄悄失效，而它自己看起來還是綠的。
ISSUED_BASELINE=$(grep -cF "ACME 签发成功：$WILDCARD_SITE" "$WORK/gen2.log" || true)

# ARI 視窗必須**落在將來**。它同時是兩件事的判據：
#   ① ARI 真被問到並落了盤；② 下面那次續期是「等到時候才動」，不是「一上來就動」。
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

# ★ ★ ★ 判據一：**誰下的決定**。這一句只在 `should_renew` 走 ARI 那一支時才會出現——
#   走比例制（剩餘壽命 1/3）會打另一句話。固定 sleep 或「讀不出舊證書於是重籤」
#   則一句都不會有。⚠ 只斷言「又出現了一張新證書」是分不出這幾種的。
if ! wait_log "$WORK/gen2.log" "$WILDCARD_SITE 该续期了：CA 的 ARI 建议窗口已经开始" "$RENEW_WAIT"; then
  fail "${RENEW_WAIT}s 内没等到「该续期了：CA 的 ARI 建议窗口已经开始」"
  grep -E '该续期|暂不续期|ACME' "$WORK/gen2.log" >&2 || true
else
  ok "★ 巡检循环自己醒过来，按 CA 的 ARI 判定该续期了"
fi

# ★ ★ 判據二：真的**又**簽了一張 —— 按上面那條基線計數，不按「出現過」。
if ! wait_log_count "$WORK/gen2.log" "ACME 签发成功：$WILDCARD_SITE" \
  "$((ISSUED_BASELINE + 1))" 60; then
  fail "判了该续期，却没有等到「ACME 签发成功」——续期在半路失败了"
  grep -E 'ACME|失败' "$WORK/gen2.log" >&2 || true
  echo "── pebble ──" >&2
  tail -40 "$WORK/pebble.log" >&2
fi

# ★ ★ 判據二之二（G166）：續期那單**帶著 `replaces`、而且被 CA 認了**。
#   正面證據取 CA 自己的話：pebble 只在帶 `replaces` 的訂單簽完之後打
#   `Order … has been marked as replaced in the DB` —— 那句在訂單轉 valid **之後**的
#   goroutine 裡才打（`wfe.go` 的 finalize）⇒ 這裡等一小會兒，⛔ 不是立刻 grep。
#   反面證據取樞衡的話：退回那句一次都不許出現。那條路有單測守著；
#   本格要證的是 `replaces` 被接受這一條，第一代的前提控制保證走得到它。
if wait_log "$WORK/pebble.log" "has been marked as replaced in the DB" 15; then
  ok "续期那单带的 replaces 被 CA 认了（pebble：旧订单已标成被替换）"
else
  fail "15s 内 pebble 没说旧订单被替换 —— 续期那单没带 replaces，或者带了 CA 没认"
  grep -aiE 'replace' "$WORK/pebble.log" >&2 || true
fi
if grep -qF "带 replaces 下单被 CA 拒了" "$WORK/gen2.log"; then
  fail "续期那一趟走了「replaces 被拒 ⇒ 去掉再下」的回退 —— 第一代的前提控制没起作用"
  grep -F "带 replaces 下单被 CA 拒了" "$WORK/gen2.log" >&2
else
  ok "续期那一趟没走「replaces 被拒」之后的回退"
fi

# ★ ★ ★ 判據三：**序列號變了**。這一條是 owner 點名的那條——
#   「不能只數紀錄行數」。序列號是 CA 給的，同一張證書重寫一遍不會換號。
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

# ★ ★ 判據四：**續期那一趟的挑戰按 CA 的選擇走對了** —— CA 新建了授權就必須重解 DNS-01，
#   CA 重複使用了有效授權就不許重解。
#   ⚠ 少了「必須重解」那一支，一個「續期時不去改 TXT」的實作會在真實 CA 上間歇性失敗，
#   而在授權還沒過期的視窗裡看起來完全正常。
#   ⚠ ⚠ **走哪一支由 CA 定，⛔ 不由 `PEBBLE_AUTHZREUSE=0` 保證**：pebble v2.10.1 `wfe/wfe.go`
#   新建授權的條件是 `authz == nil || rand.Intn(100) > wfe.authzReusePercent`，抽到 0 時
#   `0 > 0` 為假 ⇒ 照樣重複使用，每趟 1/100（`-1` 過不了它的值域檢查、回落到預設 50，繞不開）。
#   2026-10-05 全量門禁真碰上了一次：續期那單沒新建授權、沒有 `POST /chalZ/`，樞衡如實沒去重解，
#   而當時的判據只認「CA 不會重複使用」，於是判紅 —— 紅的是判據的前提，不是產品。
#   ★ 分支信號取 CA 自己的話：pebble 每**新建**一份授權打一句
#   `There are now N authorizations in the db`，重複使用時一句不打 ⇒ 第一代最後簽完時記下 N₀
#   （`AUTHZ_BASE`；沒重簽過就是 1），續期那單新建了就是 N₀ + 1、重複使用了就仍是 N₀。
#   ⚠ 取差、⛔ 不取絕對值：[2/5] 末尾的第一代重籤（G166）會多建授權。
#   ⛔ 不許改成「重複使用就跳過」或「重複使用就重跑」：兩支都判，每一支判的是樞衡在那一支下該做的事。
#   ★ 錯判那一支也逃不掉：CA 新建了授權而樞衡沒解，訂單到不了 ready，上面判據二先紅。
AUTHZ_COUNT=$(authz_total)
AUTHZ_NEW=
if [ -n "$AUTHZ_BASE" ] && [ -n "$AUTHZ_COUNT" ]; then
  AUTHZ_NEW=$((AUTHZ_COUNT - AUTHZ_BASE))
fi
TXT_CONFIRMED=$(grep -cF "的 TXT 已在全部 1 台权威 NS 上可见" "$WORK/gen2.log" || true)
case "$AUTHZ_NEW" in
  1)
    if [ "$TXT_CONFIRMED" -ge 1 ]; then
      ok "续期那单 CA 新建了授权，枢衡重新写了 TXT 并重新确认可见（G58 对续期同样成立）"
    else
      fail "续期那单 CA 新建了授权，枢衡却没有「向权威 NS 确认 TXT 可见」——挑战可能被跳过了"
      grep -i 'TXT\|DNS-01' "$WORK/gen2.log" >&2 || true
    fi
    ;;
  0)
    if [ "$TXT_CONFIRMED" -eq 0 ]; then
      ok "续期那单 CA 复用了有效授权（pebble 在 AUTHZREUSE=0 时仍有 1/100），枢衡如实没有重解挑战"
    else
      fail "续期那单 CA 复用了有效授权，枢衡却又确认了 ${TXT_CONFIRMED} 次 TXT —— 不需要挑战时也去解了"
      grep -i 'TXT\|DNS-01' "$WORK/gen2.log" >&2 || true
    fi
    ;;
  *)
    fail "pebble 报的授权总数：第一代签完时「${AUTHZ_BASE:-（一句都没有）}」、续期之后「${AUTHZ_COUNT:-（一句都没有）}」，\
期望续期那单新建 1 份或复用（差 0）—— 分支信号本身不成立"
    grep -aF 'authorizations in the db' "$WORK/pebble.log" >&2 || true
    ;;
esac

# ★ ★ 判據五：**裝上去了**，不只是寫到盤上。走真的 TLS 握手，從線上取序列號。
#   ⚠ 「盤上換了一張」與「用戶端拿到的是新的那張」是兩件事：
#   少了熱裝那一步，伺服器端會拿著一張已經被換掉的證書一直用到重新啟動。
WIRE_SERIAL=$(echo | openssl s_client -connect "$HOST:$TLS_PORT" \
  -servername "$WILDCARD_HOST" -CAfile "$WORK/pebble-root.pem" 2>/dev/null |
  openssl x509 -noout -serial 2>/dev/null | cut -d= -f2 || true)
if [ "$WIRE_SERIAL" = "$SERIAL_2" ]; then
  ok "★ 线上握手拿到的就是新那张（serial $WIRE_SERIAL）—— 续完热装进去了"
else
  fail "线上握手拿到的序列号是「$WIRE_SERIAL」，盘上是「$SERIAL_2」—— 续完没热装"
fi

# 通配符照舊能服務子域，而且用 pebble 的根驗得過籤。
WCODE=$(run_curl -s -o "$WORK/wbody" -w '%{http_code}' --max-time 10 \
  --cacert "$WORK/pebble-root.pem" \
  --resolve "$WILDCARD_HOST:$TLS_PORT:$HOST" "https://$WILDCARD_HOST:$TLS_PORT/")
expect_status "续期之后通配符 HTTPS（$WILDCARD_HOST，用 pebble 的根验签）" 200 "$WCODE"

# ★ ARI 視窗是**跟著某一張證書**的：換了一張，舊視窗必須作廢，
#   否則新證書一上來就會被判成「該續了」——那是一個無限續期循環。
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
  # ★ ★ 取證。這一格曾經偶發紅，靠下面這幾行查實了（G166）：pebble 的原話
  #   `could not find order resulting in the given certificate serial number` 來自
  #   pebble v2.10.1 自己的缺陷 —— 序列號首位元組 ≥ 0x80 時，它查 `replaces` 的鍵帶著
  #   DER 符號位元組、存訂單的鍵不帶（issue #550）。⇒ [2/5] 末尾的第一代重籤之後它不該再出現。
  #   ★ 兩側的序列號仍放在一起打：`openssl -serial` 打的是數值，**看不出符號位元組**，
  #   所以「兩側一樣」不等於「標識一樣」—— 首位是 8–F 時就是那個缺陷。
  #   ⚠ ⚠ **沒有這一段時，上一次真實失敗只留下了一句「續期在半路失敗了」。**
  echo "── ★ 两侧的序列号对不对得上 ──" >&2
  echo "第一代那张的序列号（续期时拿它做 replaces）：${SERIAL_1:-（没取到）}，前面重签过 ${REROLLS:-0} 次" >&2
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
  for f in "$WORK"/gen1-reroll*.log; do
    [ -f "$f" ] || continue
    echo "── 枢衡（第一代重签：$(basename "$f" .log)）──" >&2
    cat "$f" >&2
  done
  echo "── 枢衡（第二代）──" >&2
  cat "$WORK/gen2.log" 2>/dev/null >&2 || true
  echo "── pebble ──" >&2
  tail -60 "$WORK/pebble.log" >&2
  echo >&2
  echo "★ 单独复跑这一格：bash tests/acme/renew.sh" >&2
  echo "  ⚠ 不许用「加重试」修它 —— 那会同时掩盖真实的续期缺陷。" >&2
  exit 1
fi
echo "ACME RENEW TESTS PASSED —— 通配符证书由**跑着的**枢衡按 CA 的 ARI 自己续了一次："
echo "  等到窗口才动、序列号真的变了、续期那一趟的挑战按 CA 的选择走对了、续完热装进了握手。"
