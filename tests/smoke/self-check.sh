#!/usr/bin/env bash
# 冒烟脚本自己的门：起一个本地实例，**用同一份 run.sh 量四次**，
# 要求它一次给绿、三次给红 —— 而且每次都红在该红的那一行上。
#
# ★ ★ ★ 为什么必须有这一层：`tests/smoke/run.sh` 的正式用法是**对着真域名跑**，
#   而真域名不在门禁里。少了这一层，那个脚本就是一段**从没被证明能红**的代码，
#   等它第一次对着 `example.com` 跑的时候，没人知道「全过」到底意味着什么。
#   ⇒ 门禁里对着本地实例跑，**故意喂两个坏目标**，再**要求一次「机器外的观察点」**，
#     看它三次认不认得出来。
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
  # ★ 先还原 /etc/hosts —— 备份在 $WORK 里，而本函数末尾要删掉 $WORK。
  #   ⚠ 这台容器后面还要跑别的场景，留下一条 secure.smoke 是给后面下绊子。
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

# ★ ★ **用 /dev/tcp 真连一下，不用 `ss`。** 在这里栽过一次：
#   写 `ss -lnt "sport = :$1"` 是不行的：**构建镜像里根本没有 ss**
#   （iproute2 只装在 systemd 测试镜像里）—— 于是这个原语**恒返回 false**：
#   步骤 0 那句「端口都空着」永远成立（危险方向），而 `wait_port` 永远超时（安全方向）。
#   ⚠ 它是靠后者才暴露的：如果脚本里只有前者，这道检查会安静地什么都不查。
#   ★ 判据挂在**行为**上（真连一次），不挂在某个工具在不在上 —— 同 tests/serve/run.sh。
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

# ── 自签证书：给「证书链验不过要被认出来」那一条当靶子 ─────────────────────
#
# ★ 现签而不是提交进仓库：仓库里的测试证书迟早过期，而过期那天红的是「TLS 坏了」。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=secure.smoke" -addext "subjectAltName=DNS:secure.smoke" \
  -addext "basicConstraints=critical,CA:TRUE" >/dev/null 2>&1 \
  || { echo "SMOKE SELF-CHECK FAILED: 自签证书生成失败" >&2; exit 1; }

# ⚠ Docker 把 /etc/hosts 做成 bind-mount 的**文件**：只能就地截断重写（`cat >`），
#   `sed -i` / `mv` 要换 inode，在 bind-mount 的文件上做不到。
cp /etc/hosts "$WORK/hosts.orig"
{ cat "$WORK/hosts.orig"; printf '%s secure.smoke\n' "$HOST"; } > /tmp/hosts.new
cat /tmp/hosts.new > /etc/hosts || {
  echo "SMOKE SELF-CHECK FAILED: 改不了 /etc/hosts —— HTTPS 那一档没法验。" >&2
  exit 1
}
ok "secure.smoke 指向 $HOST；自签证书已生成"

# ── [1/5] 起实例 ───────────────────────────────────────────────────────────
echo "=== [1/5] 起被冒烟的实例 ==="
# ★ ★ 明文站点必须**具名**（`http://127.0.0.1:9210`），不能写成 `:9210`。
#   `:9210` 是不带主机名的 catch-all，**任何 Host 都命中它** —— 于是
#   run.sh 里「未知 Host → 421」那一条在本夹具上永远走不到，
#   而它会以「期望 421、实际 200」的形式红，看起来像是产品的问题。
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

# ── [2/5] 正方向：对着好目标必须全过 ───────────────────────────────────────
echo "=== [2/5] 正方向：对着一个健康的实例 ==="
if bash "$SMOKE" "http://$HOST:$PORT" > "$WORK/good.out" 2>&1; then
  ok "冒烟对健康实例给绿（$(grep -c '✓' "$WORK/good.out") 项）"
else
  bad "冒烟对健康实例给了红 —— 要么实例真有问题，要么判据太严"
  sed 's/^/      /' "$WORK/good.out" >&2
fi

# ── [3/5] 反方向之一：目标根本连不上 ───────────────────────────────────────
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

# ── [4/5] 反方向之二：HTTPS 那一档不是摆设 ─────────────────────────────────
#
# ★ ★ 这一条是本文件存在的主要理由：`run.sh` 的 HTTPS 档**只有对着真域名才会走到**，
#   而真域名不在门禁里。拿一张自签证书当靶子，就能在门禁里证明那几行代码
#   **真的会跑、而且认得出坏证书**。
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
  # ★ 顺带确认「未知 SNI 被拒绝握手」那一条也真的跑到了：它与证书链是两件事，
  #   而它恰好是本仓库有意为之的一个行为（回落会让服务端日志看起来一切正常）。
  if grep -q "未知 SNI 被拒绝握手" "$WORK/tls.out"; then
    ok "「未知 SNI 被拒绝握手」这一项也走到了，并且是过的"
  else
    bad "「未知 SNI 被拒绝握手」这一项没出现在输出里 —— 它多半根本没跑"
  fi
fi

# ── [5/5] 反方向之三：观察点不是一句装饰 ───────────────────────────────────
#
# ★ ★ ★ 这一步钉的是那条缺陷本身（§10 第 48 / 50 / 53 轮）：
#   `run.sh` 从机器内跑与从机器外跑**证明的不是同一件事**，而它的结语容易对两者
#   说同一句话。⇒ 两条判据缺一不可：
#     ① 要求机器外观察点时，对着 127.0.0.1 必须**红**，且红在观察点那一行；
#     ② 正方向那一轮的结语里必须**指名**观察点 —— 少了这条，把结语里的观察点删掉
#        不会有任何东西变红，那正是原缺陷能活三个月的形状。
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
# ⚠ 判据必须钉在**结语那一行**上：开头那行 note 里也有「观察点：」，
#   只 grep 这四个字的话，把结语里的观察点删掉它照样绿 —— 那种判据等于没有。
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
