#!/usr/bin/env bash
# 压力：持续负载下**不出错、不漏 fd、不涨内存**，并且负载中途换一次配置仍然零失败。
# （M1 退出条件第 2 条的另一半）
#
#   bash tests/stress/run.sh                       # 门禁里：自己起实例，对着它打
#   STRESS_TARGET=https://ai.example.com \
#     bash tests/stress/run.sh                     # 对着**已经部署好的**目标打
#
# ★ ★ ★ **它不是性能对拍，也绝不产出性能声明。**
#   §8 / G19 的口径是：三家全部对拍、同机同用例同负载生成器、逐类设门、
#   脚本与原始数据全部公开 —— 那是 **M3** 的事。
#   本脚本只回答一个是非题：**这台机器扛得住持续负载吗**（错误数、fd、内存、换配置）。
#   ⚠ 输出里的 RPS / 延迟数字**只作现场记录**，不许被引用成性能结论。
#
# ★ 负载生成器用 oha（镜像里按 sha256 钉死）。理由：**一个自己写的负载生成器
#   少报了错误，看起来和「没有错误」一模一样**，而「零错误」正是这里的主判据。
set -euo pipefail

REPO=${REPO:-/w}
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
PORT=${STRESS_PORT:-9200}
UP_PORT=${STRESS_UP_PORT:-9201}

# 负载参数。★ 默认压得住门禁的时间预算；真域名上想跑久一点就调它们。
DURATION=${STRESS_DURATION:-20s}
CONNECTIONS=${STRESS_CONNECTIONS:-50}
# 允许的错误率上限。★ **默认是 0**：这一条的全部价值就在于零。
MAX_ERRORS=${STRESS_MAX_ERRORS:-0}
# fd 允许的净增长条数（负载结束、连接排空之后量）。
MAX_FD_GROWTH=${STRESS_MAX_FD_GROWTH:-16}
# RSS 允许的净增长（KB）。
MAX_RSS_GROWTH_KB=${STRESS_MAX_RSS_GROWTH_KB:-40960}

FAILS=0
PIDS=()
ok() { echo "  ✓ $*"; }
bad() { FAILS=$((FAILS + 1)); echo "  ✗ $*" >&2; }
note() { echo "  · $*"; }

cleanup() {
  local pid
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

command -v oha >/dev/null 2>&1 || {
  echo "STRESS FAILED: 镜像里没有 oha —— 负载一条都打不出去。见 docker/Dockerfile.build" >&2
  exit 1
}

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
note "oha $(oha --version | awk '{print $2}')"

EXTERNAL=0
if [ -n "${STRESS_TARGET:-}" ]; then
  EXTERNAL=1
  TARGET=${STRESS_TARGET%/}
  note "对着外部目标打：$TARGET（★ fd／内存那两项**量不了**，会点名跳过）"
else
  TARGET="http://$HOST:$PORT"
  [ -x "$BIN" ] || {
    echo "STRESS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
    exit 1
  }
  for p in "$PORT" "$UP_PORT"; do
    ! port_listening "$p" || {
      echo "STRESS FAILED: 端口 $p 已经被占用 —— 否则下面压的是别人的服务。" >&2
      exit 1
    }
  done
  ok "$PORT / $UP_PORT 都是空的；产品二进制在"
fi

# ── [1/5] 起被压的实例 ─────────────────────────────────────────────────────
if [ "$EXTERNAL" = 0 ]; then
  echo "=== [1/5] 起上游与被压实例 ==="
  # 上游：一个静态应答。★ 压的是枢衡的转发路径，不是上游的处理能力。
  printf '%s\n' ":$UP_PORT {" '    respond 200 "upstream-ok"' '}' > "$WORK/up.Fulcrumfile"
  # 被压实例：一条转发路由 + 一条本地应答路由。
  # ★ 两条都要有：只压 respond 的话，连接池、上游复用这些最容易在压力下出问题的
  #   东西一个都没被碰到。
  # ★ 带上管理面：下面要在**负载正打着的时候**走 `POST /load` 换一次配置。
  #   G8 说全量 load 是原子的，而「原子」最该被压力检验 —— 平静时换配置人人都能对。
  ADMIN_SOCK="$WORK/admin.sock"
  write_px() {
    {
      printf '%s\n' '{'
      printf '    admin unix/%s\n' "$ADMIN_SOCK"
      printf '%s\n' '}' ''
      printf '%s\n' ":$PORT {"
      printf '%s\n' '    handle /up/* {'
      printf '        reverse_proxy %s:%s\n' "$HOST" "$UP_PORT"
      printf '%s\n' '    }'
      printf '    respond 200 "%s"\n' "$1"
      printf '%s\n' '}'
    } > "$WORK/px.Fulcrumfile"
  }
  write_px local-ok

  start() {
    RUST_LOG=${RUST_LOG:-warn} "$BIN" serve "$2" --bind-host "$HOST" \
      --pid-file "$WORK/$1.pid" --upgrade-sock "$WORK/$1.sock" \
      > "$WORK/$1.log" 2>&1 &
    PIDS+=($!)
  }
  start up "$WORK/up.Fulcrumfile"
  start px "$WORK/px.Fulcrumfile"
  wait_port "$UP_PORT" || { echo "STRESS FAILED: 上游起不来" >&2; cat "$WORK"/*.log >&2; exit 1; }
  wait_port "$PORT" || { echo "STRESS FAILED: 被压实例起不来" >&2; cat "$WORK"/*.log >&2; exit 1; }
  PX_PID=${PIDS[1]}
  ok "上游与被压实例都起来了（被压实例 pid=$PX_PID）"
else
  echo "=== [1/5] 用外部目标，不起实例 ==="
  PX_PID=""
fi

# ── [2/5] 基线：fd 与 RSS ──────────────────────────────────────────────────
echo "=== [2/5] 负载前的基线 ==="
# ★ 用 find 不用 ls（SC2012）。⚠ 进程已经没了的话 find 报错走 stderr，
#   计数为 0 —— 而「基线是 0」会被下面那条自证当场判红，不会悄悄变成漂亮结论。
count_fd() { find "/proc/$1/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l; }
rss_kb() { awk '/^VmRSS:/ {print $2}' "/proc/$1/status" 2>/dev/null || echo 0; }

if [ -n "$PX_PID" ]; then
  FD0=$(count_fd "$PX_PID")
  RSS0=$(rss_kb "$PX_PID")
  # ★ 基线自证：读不到就当场说，别让 0 悄悄变成一个「涨了 0」的漂亮结论。
  if [ "${FD0:-0}" -le 0 ] || [ "${RSS0:-0}" -le 0 ]; then
    bad "读不到基线（fd=$FD0 rss=${RSS0}KB）—— fd／内存那两项本轮无效"
    FD0=""; RSS0=""
  else
    ok "基线：fd=$FD0，RSS=${RSS0}KB"
  fi
else
  FD0=""; RSS0=""
  note "外部目标，读不到它的 /proc —— fd／内存两项**跳过**（不是通过）"
fi

# ── [3/5] 打负载，中途换一次配置 ───────────────────────────────────────────
echo "=== [3/5] 打负载 ${DURATION}／并发 ${CONNECTIONS}，中途换一次配置 ==="
OUT="$WORK/oha.json"
# ⚠ 是 `--output-format json`，**不是 `-j`** —— oha 1.15 没有 `-j`，
#   而它对未知参数的报错走 stderr，stdout 是空的：现象是「没有留下结果」。
oha -z "$DURATION" -c "$CONNECTIONS" --no-tui --output-format json "$TARGET/"   > "$OUT" 2>"$WORK/oha.err" &
OHA_PID=$!

# ★ ★ ★ 负载跑到一半时**真的换一次配置**（走管理面的全量 load，G8）。
#   ⚠ 换代那条路（`systemctl reload`）在 tests/m1/product.sh 的真 systemd 下验，
#   这里验的是另一条：**进程不换、配置整体替换**，而且是在有真流量打着的时候。
#   ★ 判据仍然是「零错误」——换配置期间掉一个请求，就说明它不是原子的。
RELOADED=0
if [ "$EXTERNAL" = 0 ]; then
  sleep 5
  write_px local-ok-v2
  if "$BIN" compile "$WORK/px.Fulcrumfile" > "$WORK/px.json" 2>"$WORK/compile.err"; then
    LCODE=$(curl -s -o "$WORK/load.out" -w '%{http_code}' --unix-socket "$ADMIN_SOCK" \
      -X POST --data-binary "@$WORK/px.json" http://localhost/load 2>/dev/null || echo "000")
    if [ "$LCODE" = "200" ]; then
      RELOADED=1
      note "负载进行中完成了一次全量 load（HTTP $LCODE）"
    else
      bad "负载进行中的全量 load 回了 $LCODE，期望 200：$(head -2 "$WORK/load.out" 2>/dev/null)"
    fi
  else
    bad "负载进行中重新编译配置失败：$(head -3 "$WORK/compile.err")"
  fi
fi

wait "$OHA_PID" || true
if [ ! -s "$OUT" ]; then
  echo "STRESS FAILED: oha 没有留下结果。stderr：" >&2
  cat "$WORK/oha.err" >&2
  exit 1
fi

# ── [4/5] 判据 ────────────────────────────────────────────────────────────
echo "=== [4/5] 判据 ==="
# ★ 用 python 解析 oha 的 JSON：awk/sed 抠 JSON 是本仓库明令避免的形状。
read -r TOTAL OK2XX ERRS DEADLINE RPS P99 <<EOF
$(python3 - "$OUT" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
codes = d.get("statusCodeDistribution", {}) or {}
total = sum(codes.values())
ok2xx = sum(v for k, v in codes.items() if str(k).startswith("2"))
# oha 把连接层面的失败放在 errorDistribution 里，它与状态码是两回事：
# 一个连都没连上的请求根本不会有状态码。两者都要算进「错误」。
#
# ★ ★ ★ **但 "aborted due to deadline" 不是错误，它是 oha 自己收摊时的记账。**
#   `-z 20s` 到点时还在飞的那批请求会被它记成这一类 —— 数量大约就是并发数
#   （实测 -c 50 → 47~49 条）。把它算进「服务端出错」是**把负载生成器的收尾
#   当成了被测对象的缺陷**：那么算的话，一次完全健康的压力跑会报出
#   「错误 47 个」。⚠ 而当时最省事的处置是把上限从 0 抬到 100 —— 那会同时
#   把真正的连接错误一起放行，**判据就此废掉**。
DEADLINE_KEY = "aborted due to deadline"
edist = d.get("errorDistribution", {}) or {}
deadline = sum(v for k, v in edist.items() if DEADLINE_KEY in str(k))
err_conn = sum(v for k, v in edist.items() if DEADLINE_KEY not in str(k))
errs = (total - ok2xx) + err_conn
rps = d.get("summary", {}).get("requestsPerSec", 0)
p99 = (d.get("latencyPercentiles", {}) or {}).get("p99", 0)
print(total, ok2xx, errs, deadline, round(rps, 1), round(p99 * 1000, 2))
PY
)
EOF

note "记录（**不是性能声明**）：$RPS req/s，p99 ${P99}ms"
if [ "${TOTAL:-0}" -le 0 ]; then
  bad "一个请求都没打出去 —— 本轮压力判据全部无效"
else
  ok "打了 $TOTAL 个请求，其中 2xx $OK2XX 个"
fi
if [ "${ERRS:-1}" -le "$MAX_ERRORS" ]; then
  ok "错误 $ERRS 个（上限 $MAX_ERRORS；不含收摊时被截断的那批）"
else
  bad "错误 $ERRS 个，超过上限 $MAX_ERRORS —— 持续负载下出错是这一条要挡的东西"
  python3 -c 'import json,sys; print("errorDistribution:", json.load(open(sys.argv[1])).get("errorDistribution"))' "$OUT" >&2
fi
# ★ 收摊时被截断的那批**单独报**，并且带一条上界：它最多就是「还在飞的那些」，
#   也就是并发数量级。⚠ 远超并发数说明它不是收摊造成的，那就是真的有问题了 ——
#   这一条保证「把 deadline 那类摘出去」不会顺手把真缺陷也摘掉。
DEADLINE_MAX=$((CONNECTIONS * 2))
if [ "${DEADLINE:-0}" -le "$DEADLINE_MAX" ]; then
  note "收摊时被截断 ${DEADLINE} 个（并发 ${CONNECTIONS}，上界 ${DEADLINE_MAX}）—— 这是 oha 的记账，不是服务端错误"
else
  bad "收摊时被截断 ${DEADLINE} 个，远超并发数 ${CONNECTIONS} —— 这就不是收尾造成的了"
fi

# ★ ★ 换配置**真的落到数据面上了**——否则「负载中换配置零错误」可能只是
#   因为那次 load 压根没生效。⚠ 这与批 14 那条「一行说它接线了的日志不是证据」同形。
if [ "${RELOADED:-0}" = "1" ]; then
  BODY=$(curl -sS --max-time 5 "$TARGET/" 2>/dev/null || echo "<失败>")
  if [ "$BODY" = "local-ok-v2" ]; then
    ok "换上去的那份配置真的在服务（响应体 = $BODY）"
  else
    bad "load 回了 200，但数据面还在按旧配置回「$BODY」—— 那次换配置没生效"
  fi
fi

# ── [5/5] 负载后：fd 与内存不许持续涨 ──────────────────────────────────────
echo "=== [5/5] 负载后（等连接排空）==="
if [ -n "$FD0" ]; then
  # ★ 等一会儿再量：负载刚停时连接还在 TIME_WAIT／排空，那时量到的必然偏高，
  #   而那不是泄漏。⚠ 不等就量，会得到一个每次都红的判据 —— 永远红的门等于没有门。
  sleep 3
  FD1=$(count_fd "$PX_PID")
  RSS1=$(rss_kb "$PX_PID")
  FD_D=$((FD1 - FD0))
  RSS_D=$((RSS1 - RSS0))
  if [ "$FD_D" -le "$MAX_FD_GROWTH" ]; then
    ok "fd $FD0 → $FD1（净增 $FD_D，上限 $MAX_FD_GROWTH）"
  else
    bad "fd $FD0 → $FD1（净增 $FD_D，上限 $MAX_FD_GROWTH）—— 像是每个连接漏一个 fd"
  fi
  if [ "$RSS_D" -le "$MAX_RSS_GROWTH_KB" ]; then
    ok "RSS ${RSS0} → ${RSS1}KB（净增 ${RSS_D}KB，上限 ${MAX_RSS_GROWTH_KB}KB）"
  else
    bad "RSS ${RSS0} → ${RSS1}KB（净增 ${RSS_D}KB，上限 ${MAX_RSS_GROWTH_KB}KB）"
  fi
  # ★ 进程必须还活着。⚠ 少了这一条，一个在负载中途崩掉的实例会让上面每一项
  #   都「过」（fd 与 RSS 读不到就是 0，差值为负）。
  kill -0 "$PX_PID" 2>/dev/null || bad "★ 被压实例在负载之后已经不在了 —— 上面那些数字都不作数"
  ok "被压实例仍然活着"
else
  note "fd／内存两项本轮**跳过**（外部目标或基线没读到）"
  # ★ ★ ★ 把「跳过」记在一个变量里，**结语要照着它说话**。
  #   真机实测撞到的：跳过之后结语照样印「fd 与内存没有持续增长」——
  #   而这一轮**根本没量过**。⚠ 一句声称了没验过的事的结语，
  #   比没有结语更糟：它正是本仓库反复点名的「把跳过算成通过」。
  RES_SKIPPED=1
fi

echo
if [ "$FAILS" -eq 0 ]; then
  if [ "${RES_SKIPPED:-0}" = 1 ]; then
    echo "STRESS PASSED —— 持续负载下零错误（$TARGET）"
    echo "  ⚠ ★ **fd 与内存这一轮没有量**（外部目标读不到对方的 /proc）——"
    echo "     这两项是**跳过**，不是通过。要量它们就在被压的那台机器上跑，"
    echo "     或者不设 STRESS_TARGET 让本脚本自己起一个实例。"
  else
    echo "STRESS PASSED —— 持续负载下零错误、fd 与内存没有持续增长（$TARGET）"
  fi
  echo "  ⚠ 本脚本**不产出性能声明**：RPS／延迟只作现场记录，性能口径见 §8 / G19（M3）。"
else
  echo "STRESS FAILED: $FAILS 项不通过（$TARGET）" >&2
  exit 1
fi
