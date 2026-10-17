#!/usr/bin/env bash
# 冒煙指令碼自己的門：起一個本機執行個體，**用同一份 run.sh 量四次**，
# 要求它一次給綠、三次給紅 —— 而且每次都紅在該紅的那一行上。
#
# ★ ★ ★ 為什麼必須有這一層：`tests/smoke/run.sh` 的正式用法是**對著真網域跑**，
#   而真網域不在門禁裡。少了這一層，那個指令碼就是一段**從沒被證明能紅**的程式碼，
#   等它第一次對著 `example.com` 跑的時候，沒人知道「全過」到底意味著什麼。
#   ⇒ 門禁裡對著本機執行個體跑，**故意喂兩個壞目標**，再**要求一次「機器外的觀察點」**，
#     看它三次認不認得出來。
set -euo pipefail

REPO=${REPO:-/w}
BIN="$REPO/target/release/fulcrum"
SMOKE="$REPO/tests/smoke/run.sh"
WORK=$(mktemp -d)
HOST=127.0.0.1
PORT=${SMOKE_PORT:-9210}
TLS_PORT=${SMOKE_TLS_PORT:-9211}
DEAD_PORT=${SMOKE_DEAD_PORT:-9212}

FAILS=0
PIDS=()
ok() { echo "  ✓ $*"; }
bad() { FAILS=$((FAILS + 1)); echo "  ✗ $*" >&2; }

cleanup() {
  local pid
  # ★ 先還原 /etc/hosts —— 備份在 $WORK 裡，而本函式末尾要刪掉 $WORK。
  #   ⚠ 這臺容器後面還要跑別的場景，留下一條 secure.smoke 是給後面下絆子。
  [ -f "$WORK/hosts.orig" ] && cat "$WORK/hosts.orig" > /etc/hosts 2>/dev/null
  true
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -KILL "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

# ★ ★ **用 /dev/tcp 真連一下，不用 `ss`。** 在這裡栽過一次：
#   寫 `ss -lnt "sport = :$1"` 是不行的：**建置鏡像里根本沒有 ss**
#   （iproute2 只裝在 systemd 測試鏡像裡）—— 於是這個原語**恆回傳 false**：
#   步驟 0 那句「連接埠都空著」永遠成立（危險方向），而 `wait_port` 永遠超時（安全方向）。
#   ⚠ 它是靠後者才暴露的：如果指令碼裡只有前者，這道檢查會安靜地什麼都不查。
#   ★ 判據掛在**行為**上（真連一次），不掛在某個工具在不在上 —— 同 tests/serve/run.sh。
port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}
wait_port() {
  local tries=0
  while [ "$tries" -lt 100 ]; do
    port_listening "$1" && return 0
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

echo "=== [0/5] 前置 ==="
[ -x "$BIN" ] || { echo "SMOKE SELF-CHECK FAILED: 找不到 $BIN" >&2; exit 1; }
for p in "$PORT" "$TLS_PORT" "$DEAD_PORT"; do
  ! port_listening "$p" || {
    echo "SMOKE SELF-CHECK FAILED: 端口 $p 已被占用" >&2
    exit 1
  }
done
ok "$PORT / $TLS_PORT / $DEAD_PORT 都是空的"

# ── 自簽證書：給「證書鏈驗不過要被認出來」那一條當靶子 ─────────────────────
#
# ★ 現簽而不是提交進儲存庫：儲存庫裡的測試證書遲早過期，而過期那天紅的是「TLS 壞了」。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=secure.smoke" -addext "subjectAltName=DNS:secure.smoke" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1 \
  || { echo "SMOKE SELF-CHECK FAILED: 自签证书生成失败" >&2; exit 1; }

# ⚠ Docker 把 /etc/hosts 做成 bind-mount 的**檔案**：只能就地截斷重寫（`cat >`），
#   `sed -i` / `mv` 要換 inode，在 bind-mount 的檔案上做不到。
cp /etc/hosts "$WORK/hosts.orig"
{ cat "$WORK/hosts.orig"; printf '%s secure.smoke\n' "$HOST"; } > /tmp/hosts.new
cat /tmp/hosts.new > /etc/hosts || {
  echo "SMOKE SELF-CHECK FAILED: 改不了 /etc/hosts —— HTTPS 那一档没法验。" >&2
  exit 1
}
ok "secure.smoke 指向 $HOST；自签证书已生成"

# ── [1/5] 起執行個體 ───────────────────────────────────────────────────────────
echo "=== [1/5] 起被冒烟的实例 ==="
# ★ ★ 明文站台必須**具名**（`http://127.0.0.1:9210`），不能寫成 `:9210`。
#   `:9210` 是不帶主機名的 catch-all，**任何 Host 都命中它** —— 於是
#   run.sh 裡「未知 Host → 421」那一條在本夾具上永遠走不到，
#   而它會以「期望 421、實際 200」的形式紅，看起來像是產品的問題。
{
  printf 'http://%s:%s {\n' "$HOST" "$PORT"
  printf '%s\n' '    respond 200 "smoke-ok"'
  printf '%s\n' '}' ''
  printf 'secure.smoke:%s {\n' "$TLS_PORT"
  printf '    tls %s %s\n' "$WORK/tls.crt" "$WORK/tls.key"
  printf '%s\n' '    respond 200 "smoke-tls-ok"'
  printf '%s\n' '}'
} > "$WORK/smoke.Fulcrumfile"

RUST_LOG=${RUST_LOG:-warn} "$BIN" serve "$WORK/smoke.Fulcrumfile" --bind-host "$HOST" \
  --pid-file "$WORK/smoke.pid" --upgrade-sock "$WORK/smoke.sock" \
  > "$WORK/smoke.log" 2>&1 &
PIDS+=($!)
wait_port "$PORT" || { echo "SMOKE SELF-CHECK FAILED: 实例起不来" >&2; cat "$WORK/smoke.log" >&2; exit 1; }
wait_port "$TLS_PORT" || { echo "SMOKE SELF-CHECK FAILED: TLS 端口起不来" >&2; cat "$WORK/smoke.log" >&2; exit 1; }
ok "实例起来了（$PORT 明文、$TLS_PORT TLS）"

# ── [2/5] 正方向：對著好目標必須全過 ───────────────────────────────────────
echo "=== [2/5] 正方向：对着一个健康的实例 ==="
if bash "$SMOKE" "http://$HOST:$PORT" > "$WORK/good.out" 2>&1; then
  ok "冒烟对健康实例给绿（$(grep -c '✓' "$WORK/good.out") 项）"
else
  bad "冒烟对健康实例给了红 —— 要么实例真有问题，要么判据太严"
  sed 's/^/      /' "$WORK/good.out" >&2
fi

# ── [3/5] 反方向之一：目標根本連不上 ───────────────────────────────────────
echo "=== [3/5] 反方向：一个没人在听的端口 ==="
if bash "$SMOKE" "http://$HOST:$DEAD_PORT" > "$WORK/dead.out" 2>&1; then
  bad "★ 冒烟对着一个死端口居然给了绿 —— 它是瞎的"
else
  if grep -q "目标根本连不上" "$WORK/dead.out"; then
    ok "冒烟认出了「连不上」，而且红在该红的那一行"
  else
    bad "冒烟红了，但不是红在「连不上」上 —— 红对了地方才算数"
    sed 's/^/      /' "$WORK/dead.out" >&2
  fi
fi

# ── [4/5] 反方向之二：HTTPS 那一檔不是擺設 ─────────────────────────────────
#
# ★ ★ 這一條是本檔案存在的主要理由：`run.sh` 的 HTTPS 檔**只有對著真網域才會走到**，
#   而真網域不在門禁裡。拿一張自簽證書當靶子，就能在門禁裡證明那幾行程式碼
#   **真的會跑、而且認得出壞證書**。
echo "=== [4/5] 反方向：自签证书（HTTPS 档必须认出来）==="
if bash "$SMOKE" "https://secure.smoke:$TLS_PORT" > "$WORK/tls.out" 2>&1; then
  bad "★ 冒烟对着一张自签证书给了绿 —— HTTPS 那一档是瞎的"
else
  if grep -q "证书链验不过" "$WORK/tls.out"; then
    ok "冒烟认出了「证书链验不过」（HTTPS 档真的在跑）"
  else
    bad "冒烟红了，但没红在证书链上 —— HTTPS 档可能压根没走到"
    sed 's/^/      /' "$WORK/tls.out" >&2
  fi
  # ★ 順帶確認「未知 SNI 被拒絕握手」那一條也真的跑到了：它與證書鏈是兩件事，
  #   而它恰好是本儲存庫有意為之的一個行為（回落會讓伺服器端紀錄看起來一切正常）。
  if grep -q "未知 SNI 被拒绝握手" "$WORK/tls.out"; then
    ok "「未知 SNI 被拒绝握手」这一项也走到了，并且是过的"
  else
    bad "「未知 SNI 被拒绝握手」这一项没出现在输出里 —— 它多半根本没跑"
  fi
fi

# ── [5/5] 反方向之三：觀察點不是一句裝飾 ───────────────────────────────────
#
# ★ ★ ★ 這一步釘的是那條缺陷本身（§10 第 48 / 50 / 53 輪）：
#   `run.sh` 從機器內跑與從機器外跑**證明的不是同一件事**，而它的結語容易對兩者
#   說同一句話。⇒ 兩條判據缺一不可：
#     ① 要求機器外觀察點時，對著 127.0.0.1 必須**紅**，且紅在觀察點那一行；
#     ② 正方向那一輪的結語裡必須**指名**觀察點 —— 少了這條，把結語裡的觀察點刪掉
#        不會有任何東西變紅，那正是原缺陷能活三個月的形狀。
echo "=== [5/5] 反方向：观察点（机内 + 要求机外 ⇒ 必须红）==="
if SMOKE_REQUIRE_OFFBOX=1 bash "$SMOKE" "http://$HOST:$PORT" > "$WORK/obs.out" 2>&1; then
  bad "★ 要求机器外观察点时，对着 $HOST 居然给了绿 —— 观察点那条判据是摆设"
else
  if grep -q "观察点是「机内」" "$WORK/obs.out"; then
    ok "认出了「机内观察」，红在观察点那一行（同一个目标不加这个开关时是全绿的）"
  else
    bad "红了，但不是红在观察点上 —— 红对了地方才算数"
    sed 's/^/      /' "$WORK/obs.out" >&2
  fi
fi
# ⚠ 判據必須釘在**結語那一行**上：開頭那行 note 裡也有「觀察點：」，
#   只 grep 這四個字的話，把結語裡的觀察點刪掉它照樣綠 —— 那種判據等於沒有。
if grep -q "SMOKE PASSED.*观察点：" "$WORK/good.out"; then
  ok "正方向那一轮的**结语行**里指名了观察点 —— 机内跑与机外跑不再说同一句话"
else
  bad "结语里没有观察点 —— 那正是栽的那一条"
fi

echo
if [ "$FAILS" -eq 0 ]; then
  echo "SMOKE SELF-CHECK PASSED —— 冒烟脚本对好目标给绿、对死端口与坏证书给红、对「机内观察」在被要求机外时给红，且都红在该红的地方。"
  echo "  ★ 正式用法是对着真域名跑：bash tests/smoke/run.sh https://example.com"
else
  echo "SMOKE SELF-CHECK FAILED: $FAILS 项不通过" >&2
  exit 1
fi
