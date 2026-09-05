#!/usr/bin/env bash
# 用例类「静态吞吐」——四家各服务**同一个固定资源**，同一个负载生成器打。
#
#   bash bench/case/static-throughput.sh <输出目录>
#
# ⛔ **本脚本不判定、不比较、不打印任何「谁更快」。** 它只产出原始数据。
#   判定在 `bench/verdict.sh`，而那一步会先问宿主合不合格。
#   ★ 这条分工不是洁癖：把「量」和「判」写在一起，就没有办法在不合格的宿主上
#     跑完整条流水线而结构性地不出结论 —— 而那正是 G132 要的东西。
#
# ── 对齐口径（★ 每一条都会被 bench/README.md 引用，改这里要一起改）─────────────
#
#   ① **一次只起一家。** 四家同时在跑会互相抢 CPU，量到的是「四家一起跑」这件事。
#   ② **同一个 payload、同一个 URL 路径、同一个字节数**，且开跑前逐字节核对过。
#   ③ **访问日志四家全关。** 日志是另一件事，开着会把它的代价算进吞吐里。
#   ④ **明文 HTTP/1.1 + keep-alive。** TLS 归「TLS 握手」那一类，⛔ 不混进来。
#   ⑤ **worker 数四家一致**（`BENCH_WORKERS`，缺省 1）。
#      ⚠ ⚠ ★ 缺省取 1 **不是保守，是今天唯一可对齐的值**：枢衡的数据面走
#      pingora `ServerConf::default()`，那里 `threads: 1`，而 DSL 与 CLI **都没有
#      调它的旋钮**（2026-09-05 实测：全仓 grep `threads` 在产品代码里一处都没有）。
#      ⇒ 把另外三家调到 N>1 而枢衡只能是 1，量到的差异里混着一条与实现无关的变量。
#      ⛔ **别把这条读成「枢衡只能单线程」** —— 它读作「今天没有旋钮」，
#      而那是 M3 出数之前必须先解决的一件事，已登记。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR=${1:?用法：bash bench/case/static-throughput.sh <输出目录>}
CASE=static-throughput
RAW_DIR="$OUT_DIR/raw/$CASE"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ── 参数（★ 它们是口径的一部分，会被写进环境快照）────────────────────────────
DURATION=${BENCH_DURATION:-10s}
CONNECTIONS=${BENCH_CONNECTIONS:-50}
WORKERS=${BENCH_WORKERS:-1}
PAYLOAD_BYTES=${BENCH_PAYLOAD_BYTES:-4096}
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 端口：9940–9943（端口表在 docs/platform/host-and-gate-traps.md）。
#
# ★ 严格说本格**不与任何场景争端口** —— 它跑在自己的容器里，netns 是独立的。
#   仍然登记进那张表、仍然挑一段没人用的，是因为**「今天不冲突」不是一个能靠的性质**：
#   哪天有人给这个容器加上 `--net=host`，冲突会立刻出现而没有任何东西会说。
#
# ⚠ ⚠ ⛔ **初稿写的是 9700–9703，那是 `tests/h3/` 已经占着的一段。**
#   写下「9700–9703 是空段」那句话时，用的是一条自己拼的枚举命令，而它的
#   字符类是 `[A-Z_]+` —— **匹配不到 `${H3_PORT:-9700}` 里的那个数字 `3`**。
#   ⇒ 它安静地少列了一整格，而输出看起来完整。★ 教训与本仓 §3.1 同族：
#   **自查用的筛子必须比被查的东西宽**；这里正确的做法是直接读那张表，⛔ 不是自己拼一条 grep。
#
# ⚠ ⛔ **一个都别改成 :80** —— 那个端口在门禁里是共享的（G137），占住它会让一个
#   与本用例毫无关系的场景起不来，而报错落在那个无辜的端口上。
PORT_FULCRUM=${BENCH_PORT_FULCRUM:-9940}
PORT_CADDY=${BENCH_PORT_CADDY:-9941}
PORT_HAPROXY=${BENCH_PORT_HAPROXY:-9942}
PORT_NGINX=${BENCH_PORT_NGINX:-9943}

FAILS=0
CHILD=
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

cleanup() {
  stop_child
  rm -rf "$WORK"
}
trap cleanup EXIT

# ── 原语 ───────────────────────────────────────────────────────────────────
#
# ★ 「起来了没有」判在**行为**上（真连一次），⛔ 不判某个工具在不在 ——
#   `tests/stress/run.sh` 在这条上栽过：写 `ss -lnt …` 而镜像里根本没有 `ss`，
#   于是那个原语**恒返回 false**，一个方向永远成立、另一个方向永远超时。
port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}
wait_port() {
  local tries=0
  while [ "$tries" -lt 150 ]; do
    port_listening "$1" && return 0
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}
wait_port_gone() {
  local tries=0
  while [ "$tries" -lt 150 ]; do
    port_listening "$1" || return 0
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

stop_child() {
  [ -n "$CHILD" ] || return 0
  kill -INT "$CHILD" 2>/dev/null || true
  local waited=0
  while kill -0 "$CHILD" 2>/dev/null && [ "$waited" -lt 80 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  kill -KILL "$CHILD" 2>/dev/null || true
  wait "$CHILD" 2>/dev/null || true
  CHILD=
}

# CPU 亲和。★ 两组核都给了才用 `taskset`；只给一个等于没给（见 env-snapshot.sh）。
# ⚠ 用**数组**展开，⛔ 不要拼一个字符串再让它被词分割 —— 那条路在没设亲和时
#   会展开成一个空词，`command` 拿到一个空 argv[0] 而报「command not found」。
pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

# ── 准备 payload ───────────────────────────────────────────────────────────
WWW="$WORK/www"
mkdir -p "$WWW" "$RAW_DIR"
# ★ 用 `/dev/urandom` 而不是可压缩的重复字节：四家的默认压缩策略不同，
#   一个高度可压缩的 payload 会把「压不压缩」的差异混进吞吐里。本类**不比压缩**。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WWW/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WWW/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "STATIC-THROUGHPUT FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
URL_PATH=/payload.bin

# ── 渲染四家的配置 ─────────────────────────────────────────────────────────
#
# ★ 模板在 `bench/conf/` 下、是**交付物的一部分**，这里只替换占位符。
#   ⛔ 别把配置内联进本脚本：第三方要看的就是那四份配置本身。
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WWW_ROOT__|$WWW|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__RUN_DIR__|$WORK|g" \
    -e "s|__PAYLOAD__|$WWW/payload.bin|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum.Fulcrumfile" "$WORK/fulcrum.Fulcrumfile" "$PORT_FULCRUM"
render "$BENCH_DIR/conf/Caddyfile" "$WORK/Caddyfile" "$PORT_CADDY"
render "$BENCH_DIR/conf/haproxy.cfg" "$WORK/haproxy.cfg" "$PORT_HAPROXY"
render "$BENCH_DIR/conf/nginx.conf" "$WORK/nginx.conf" "$PORT_NGINX"
mkdir -p "$WORK/nginx-body"

# ── 一家的完整一趟 ─────────────────────────────────────────────────────────
#
# 起 → 等端口 → **核对它回的到底是不是那个资源** → 打负载 → 收 → 等端口消失。
run_subject() {
  local name=$1 port=$2
  shift 2
  local log="$WORK/$name.log"

  echo "── $name ──"
  "${pin_server[@]}" "$@" > "$log" 2>&1 &
  CHILD=$!

  if ! wait_port "$port"; then
    bad "$name 没在 $port 上起来"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi

  # ★ ★ ★ **开跑前先证明样本里真有东西。** 一个稳定回 404 的被测会给出一个
  #   非常漂亮的 RPS，而那个数与「服务静态资源」毫无关系 —— 它量的是回 404 有多快。
  #   ⇒ 状态码与**字节数**都要对上，⛔ 只判状态码不够（回 200 空 body 同样很快）。
  local probe code size
  probe=$(curl -sS -o /dev/null -w '%{http_code} %{size_download}' \
    "http://$HOST:$port$URL_PATH" 2>>"$log" || echo "000 0")
  code=${probe%% *}
  size=${probe##* }
  if [ "$code" != "200" ] || [ "$size" != "$PAYLOAD_BYTES" ]; then
    bad "$name 开跑前的核对没过：HTTP $code，body $size 字节（该是 200 / $PAYLOAD_BYTES）"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi
  ok "$name 回的是那个资源（HTTP 200，$size 字节）"

  # 打负载。⚠ `--no-tui`：没有 TTY 时 oha 的交互界面会把输出弄成一团。
  if "${pin_load[@]}" oha \
    --output-format json --no-tui \
    -z "$DURATION" -c "$CONNECTIONS" \
    -o "$RAW_DIR/$name.json" \
    "http://$HOST:$port$URL_PATH" >> "$log" 2>&1; then
    ok "$name 的原始数据已落盘：raw/$CASE/$name.json"
  else
    bad "$name 那一趟负载没跑完"
    sed 's/^/      /' "$log" >&2 || true
  fi

  stop_child
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一家会起不来"
}

# ⚠ 顺序有意固定：结果与顺序无关才对，⇒ 固定它是为了让两趟之间可比。
run_subject fulcrum "$PORT_FULCRUM" \
  "$FULCRUM_BIN" serve "$WORK/fulcrum.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/fulcrum.pid" \
  --upgrade-sock "$WORK/fulcrum.sock" \
  --state-dir "$WORK/state"

# ★ `GOMAXPROCS` 是 Caddy 唯一的并行度旋钮（它是 Go 写的，没有 worker 进程的概念）。
GOMAXPROCS="$WORKERS" \
  run_subject caddy "$PORT_CADDY" \
  caddy run --config "$WORK/Caddyfile" --adapter caddyfile

# ★ `-db` = 不 daemon 化、不做后台化，编排脚本才拿得到它的 pid。
run_subject haproxy "$PORT_HAPROXY" \
  haproxy -f "$WORK/haproxy.cfg" -db

# ★ nginx 的 `daemon off` 写在配置里（见 bench/conf/nginx.conf）。
run_subject nginx "$PORT_NGINX" \
  nginx -c "$WORK/nginx.conf"

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/$CASE] 四家全部跑通，原始数据在 $RAW_DIR"
  echo "[bench/$CASE] ⛔ 本步**不产出任何结论** —— 判定见 bench/verdict.sh"
else
  echo "[bench/$CASE] ★ $FAILS 处失败 —— 这一趟的原始数据不完整，⛔ 不许拿去判定" >&2
  exit 1
fi
