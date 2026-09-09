#!/usr/bin/env bash
# 诊断读数：**磁盘命中**与**内存命中**的 p99 差（`G150` ②）。
#
#   bash bench/diag/cache-hit-p99.sh <输出目录>
#
# ★ ★ ★ **这不是 §8 七类里的一类，本步也不判定、不比较、不打印任何「谁更快」。**
#   §8 那七类是「与 Caddy / HAProxy / Nginx 三家对拍」；这一步只有枢衡自己，
#   量的是**同一个进程的两种缓存后端**之间的差 —— 磁盘命中对内存命中，
#   根本没有竞品。它回答 `PLAN.md` `G150` ② 那条判据：
#   「M3 对拍**必须记录**『磁盘命中的 p99 − 内存命中的 p99』这一格，落进原始数据」。
#
# ⚠ ⚠ ⛔ **输出落 `diag/`，不落 `raw/`。** `bench/verdict.sh` 是
#   `for raw_case in "$OUT_DIR"/raw/*/` —— `raw/` 下每个子目录都会被当成一类去判，
#   而 `bench/read-raw.py` 把该目录下每个 `*.json` 当成一家被测。只有枢衡一家
#   ⇒ 判据 ④ 会按「有效读数不足两条」判 `VOID`。两个方向由 `tests/bench/gate.sh`
#   的 C18 钉着（与 `respond-ceiling` 的 C17 同形）。
#
# ⚠ ⚠ **有意不定阈值**（`G150` ② 明写）：定一个没有实测支撑的数字正是 `G148` ③
#   点名的那种错。本步只把数量出来。
#
# ── 拓扑 ───────────────────────────────────────────────────────────────────
#
#   源站（file_server） ← 反代 + cache（内存 / 磁盘两个变体，**一次只起一个**）
#
# ★ 为什么必须有源站：`crates/fulcrum-server/src/lib.rs:654` 写着「缓存**只裹
#   `reverse_proxy`**」⇒ ⛔ 不能把 `cache` 挂在 `file_server` 上省掉这一层。
# ⚠ 而**命中之后源站根本不被访问** ⇒ 它在测量窗口里的 CPU 成本 ≈ 0。
#
# ── 对齐口径（两个变体之间）────────────────────────────────────────────────
#
#   ① **一次只起一个变体。** 同时跑会互相抢 CPU，量到的是「两个一起跑」这件事。
#   ② 同一个源站进程、同一份 payload 字节、同一个 URL 路径。
#   ③ 访问日志两边全关；明文 HTTP/1.1 + keep-alive。
#   ④ worker 数三个进程一致（`BENCH_WORKERS`）。
#   ⑤ 内存那份是**从磁盘那份删行派生**的，⛔ 不是另写一份（单变量由生成方式保证）。
#      ⚠ **两处差别，如实写在这里**：① `disk` 那一行被删掉（这是被测的那个变量）；
#      ② 监听端口不同 —— 两个变体虽然一次只起一个，但沿用 `respond-ceiling.sh` 的
#      两端口做法，⛔ 不共用一个端口去赌 TIME_WAIT 与释放时机。
#      ⇒ 「唯一差别是 disk 那一行」这句话**不成立**，别那么写；成立的是
#      「**语义上**唯一的变量是缓存后端，端口那处是机械必需」。
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ⚠ 只为拿那四个负载参数的缺省（`bench/lib.sh` 的「负载参数」一节）。
#   ⛔ 不许在本文件里再写一份 `:-` 缺省 —— 那正是 G147 治掉的那一族。
# shellcheck source=bench/lib.sh
. "$BENCH_DIR/lib.sh"

OUT_DIR=${1:?用法：bash bench/diag/cache-hit-p99.sh <输出目录>}
NAME=cache-hit-p99
DIAG_DIR="$OUT_DIR/diag/$NAME"
WORK=$(mktemp -d)
HOST=127.0.0.1

# ⛔ 这四个不带 `:-`：缺省由上面那次 `source` 供（G147 一处定义）。
DURATION=$BENCH_DURATION
CONNECTIONS=$BENCH_CONNECTIONS
WORKERS=$BENCH_WORKERS
PAYLOAD_BYTES=$BENCH_PAYLOAD_BYTES
FULCRUM_BIN=${FULCRUM_BIN:-/w/target/release/fulcrum}

# 端口：9946–9948，在 `docs/platform/host-and-gate-traps.md` 那张表已保留的
# 9940–9949 段之内（9940–9943 归静态吞吐，9944–9945 归 respond-ceiling）。
# ⛔ 一个都别改成 :80 —— 那个端口在门禁里是共享的（G137）。
PORT_ORIGIN=${BENCH_PORT_CACHE_ORIGIN:-9946}
PORT_MEM=${BENCH_PORT_CACHE_MEM:-9947}
PORT_DISK=${BENCH_PORT_CACHE_DISK:-9948}

FAILS=0
CHILD=
ORIGIN_PID=
ok() { echo "  ✓ $*"; }
bad() {
  FAILS=$((FAILS + 1))
  echo "  ✗ $*" >&2
}

# ── ttl 由 BENCH_DURATION 算出来，⛔ 不写字面量 ────────────────────────────
#
# ★ ★ 一个写死的 `ttl` 与一个可改的 `BENCH_DURATION` 是**两处会飘的值**，
#   而飘掉的那天没有任何东西会说 —— 症状是「量到的其实是重验证」，
#   而 oha 的 JSON 里看不出这件事，输出看着完全正常。⇒ 一处定义（G147 那条形状）。
# ⚠ 认不出的写法**当场死**，⛔ 不回落到某个缺省：一个安静回落的解析器
#   会让 ttl 变成一个只有它自己知道的数。
duration_secs() {
  local d=$1 n unit
  n=${d%[smh]}
  unit=${d#"$n"}
  case "$n" in
    '' | *[!0-9]*)
      echo "CACHE-HIT-P99 FAILED: BENCH_DURATION 的数值部分不是整数：'$d'" >&2
      exit 1
      ;;
  esac
  case "$unit" in
    s) echo "$n" ;;
    m) echo "$((n * 60))" ;;
    h) echo "$((n * 3600))" ;;
    *)
      echo "CACHE-HIT-P99 FAILED: 认不出 BENCH_DURATION 的写法：'$d'（只认 <整数>s|m|h）" >&2
      exit 1
      ;;
  esac
}
DURATION_SECS=$(duration_secs "$DURATION")
TTL="$((DURATION_SECS * 10))s"

# ── 原语（与 bench/diag/respond-ceiling.sh 同一套，理由见那里的注释）──────
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
kill_pid() {
  local pid=$1 waited=0
  [ -n "$pid" ] || return 0
  kill -INT "$pid" 2>/dev/null || true
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 80 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  kill -KILL "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
stop_child() {
  [ -n "$CHILD" ] || return 0
  kill_pid "$CHILD"
  CHILD=
}
stop_origin() {
  [ -n "$ORIGIN_PID" ] || return 0
  kill_pid "$ORIGIN_PID"
  ORIGIN_PID=
}

cleanup() {
  stop_child
  stop_origin
  rm -rf "$WORK"
  [ -z "${BASE:-}" ] || rm -rf "$BASE"
}
trap cleanup EXIT

pin_server=()
pin_load=()
if [ -n "${BENCH_SERVER_CPUS:-}" ] && [ -n "${BENCH_LOAD_CPUS:-}" ]; then
  pin_server=(taskset -c "$BENCH_SERVER_CPUS")
  pin_load=(taskset -c "$BENCH_LOAD_CPUS")
fi

# ── 站点根与缓存根：**各开各的子目录**，⛔ 不共用一个 ──────────────────────
#
# ★ 复用现有 ext4 匿名卷（owner 2026-09-09 拍板）：不动 bench/docker-run.sh 的卷集；
#   与生产形状一致（同一块盘）。⚠ 带 `$$` 是照 respond-ceiling.sh:128 的做法 ——
#   `BENCH_WWW_ROOT` 是跨步骤共用的挂载点，不带 pid 时两步会互相踩。
if [ -n "${BENCH_WWW_ROOT:-}" ]; then
  BASE="$BENCH_WWW_ROOT/$NAME-$$"
else
  BASE="$WORK/$NAME"
fi
rm -rf "$BASE"
WWW="$BASE/www"
CACHE_DIR="$BASE/cache"
mkdir -p "$WWW" "$CACHE_DIR" "$DIAG_DIR"

# payload：⚠ 与 respond-ceiling 不同，这里**不必可打印** —— 它由源站的
# `file_server` 从磁盘发出，不进 DSL 的内联正文。⇒ 与静态吞吐那一类同源。
head -c "$PAYLOAD_BYTES" /dev/urandom > "$WWW/payload.bin"
ACTUAL_BYTES=$(wc -c < "$WWW/payload.bin" | tr -d ' ')
[ "$ACTUAL_BYTES" = "$PAYLOAD_BYTES" ] || {
  echo "CACHE-HIT-P99 FAILED: payload 该是 $PAYLOAD_BYTES 字节，实际 $ACTUAL_BYTES" >&2
  exit 1
}
URL_PATH=/payload.bin

# ── 渲染三份配置 ───────────────────────────────────────────────────────────
render() {
  local src=$1 dst=$2 port=$3
  sed -e "s|__PORT__|$port|g" \
    -e "s|__WWW_ROOT__|$WWW|g" \
    -e "s|__WORKERS__|$WORKERS|g" \
    -e "s|__TTL__|$TTL|g" \
    -e "s|__CACHE_DIR__|$CACHE_DIR|g" \
    -e "s|__ORIGIN_PORT__|$PORT_ORIGIN|g" \
    "$src" > "$dst"
}
render "$BENCH_DIR/conf/fulcrum-cache-origin.Fulcrumfile" "$WORK/origin.Fulcrumfile" "$PORT_ORIGIN"
render "$BENCH_DIR/conf/fulcrum-cache-disk.Fulcrumfile" "$WORK/disk.Fulcrumfile" "$PORT_DISK"

# ★ ★ ★ 内存那一份**由磁盘那份删掉 `disk` 一行派生**，别的一个字节不改
#   —— 单变量由生成方式保证（照 tests/cache/disk.sh:145-146）。
#   ⚠ 端口那一格要换成内存变体的，⇒ 先删行再改端口，两步都只动一行。
grep -v '^        disk ' "$WORK/disk.Fulcrumfile" > "$WORK/mem.Fulcrumfile.tmp"
sed -e "s|:$PORT_DISK {|:$PORT_MEM {|" "$WORK/mem.Fulcrumfile.tmp" > "$WORK/mem.Fulcrumfile"
# ⚠ 派生自证：删掉的**必须恰好是一行**，且那一行**必须**是 `disk`。
#   ⛔ 少了这一步，缩进一改 grep 就什么都删不掉，而两个变体会变成同一个后端 ——
#   那种失效下 delta 恒等于「两次内存命中之差」，看着完全正常。
DISK_LINES=$(wc -l < "$WORK/disk.Fulcrumfile")
MEM_LINES=$(wc -l < "$WORK/mem.Fulcrumfile")
if [ "$((DISK_LINES - MEM_LINES))" != 1 ]; then
  echo "CACHE-HIT-P99 FAILED: 派生内存变体该只少一行，实际磁盘 $DISK_LINES 行、内存 $MEM_LINES 行" >&2
  exit 1
fi
if grep -q '^        disk ' "$WORK/mem.Fulcrumfile"; then
  echo "CACHE-HIT-P99 FAILED: 内存变体里仍然有 disk 那一行" >&2
  exit 1
fi

# ── 承重件：量之前先证明量的真是那个后端的命中 ─────────────────────────────
#
# ⚠ ⚠ ⛔ 这一格最危险的失效是「以为在量磁盘命中，其实缓存压根没开」：
#   `dsl-reference.md` 明写「配了 `disk` 而目录用不了」时**缓存是关掉的、照常转发**，
#   `X-Fulcrum-Cache` 那个头**一个都不会出现** —— 而那种情况下 oha 照样量得出
#   一组**看起来完全正常**的数，⛔ 不会有任何东西说。
# ★ 判别力来自 `dsl-reference.md`：「后缀说的是**这个进程真的挑中了哪个后端**，
#   不是配置里写了什么」。单测在 `crates/fulcrum-server/src/cache/mod.rs:587,590`。
# ⚠ ⚠ **指标那一层答不了这个问题**：`cache/mod.rs:280` 写着它「折得比响应头**粗**：
#   `HIT` 与 `HIT-DISK` 在指标上是同一格」⇒ ⛔ 别想用 `metrics` 代替本判据。
# ★ ★ ★ **观测到的值要落进原始数据，⛔ 不能只以「✓ 打印行」的形式存在。**
#   ⚠ 2026-09-09 实测撞出来的：本步在门禁里跑时，`tests/bench/gate.sh:40` 把
#   `bench/run.sh` 的输出重定向进**容器里**的 `/tmp/bench-gate.log`，`--rm` 之后就没了
#   ⇒ 那几行 `✓` **没有任何人看得见**，包括写它的人。
#   ⇒ 于是「量的是不是那个后端」这件事就只剩「相信这个脚本的控制流」这一条路，
#   而那正是 G147 治的那一族：**口径必须落在原始数据里**。
#   ⇒ 观测值写进 `delta.json`，由 `tests/bench/gate.sh` 的 C18 **独立复核**。
STATE_LOG="$WORK/cache-states.tsv"
: > "$STATE_LOG"

assert_cache_state() {
  local name=$1 port=$2 want=$3 when_key=$4 log=$5
  local hdr got age when rc=0
  case "$when_key" in
    before) when=开跑前 ;;
    after) when=跑完后 ;;
    *)
      echo "CACHE-HIT-P99 FAILED: 认不出的时点键 '$when_key'（只认 before|after）" >&2
      exit 1
      ;;
  esac
  hdr=$(curl -sS -D - -o "$WORK/$name.body" "http://$HOST:$port$URL_PATH" 2>>"$log" || true)
  got=$(printf '%s' "$hdr" | tr -d '\r' | awk -F': ' 'tolower($1) == "x-fulcrum-cache" { print $2 }')
  age=$(printf '%s' "$hdr" | tr -d '\r' | awk -F': ' 'tolower($1) == "age" { print $2 }')
  # ⚠ 无论过没过都记：一份记着「这里当时是空的」的原始数据，比一份干脆没有这一格的
  #   原始数据诚实得多。⛔ 只在成功时记 = 事后看不出失败长什么样。
  printf '%s\t%s\t%s\t%s\n' "$name" "$when_key" "${got:-}" "${age:-}" >> "$STATE_LOG"

  if [ "$got" != "$want" ]; then
    bad "$name（$when）X-Fulcrum-Cache 该是 '$want'，实际 '${got:-（这个头根本没出现）}' —— 量的不是那个后端的命中"
    return 1
  fi

  # ── 「整趟没发生重验证」由 `Age` 判，⛔ 不由「前后取值相同」判 ───────────
  #
  # ⛔ ⛔ **本条原先写的是「跑完后再核一次 X-Fulcrum-Cache，相同 ⇒ 没重验证」——
  #   那是错的，2026-09-09 用反证实测证伪：注入 `ttl=1s` < `duration=2s`
  #   之后整个门禁**照样全绿**。**
  # ★ 根因（读源码，⛔ 不是推理）：`CacheState` 只有 `Hit` / `Revalidated` 两个值
  #   （`cache/mod.rs:261`），而重验证之后条目**重新变新鲜** ⇒ 之后的请求又是 `Hit`
  #   —— 与「从未过期」在最终那一次观测上**完全相同**。
  # ★ 改用 `Age`：`Age = now − stored_at`（`lib.rs:1384`），而重验证走
  #   `refresh(…, now_unix(), …)` → `stored_at = now`（`store.rs:208`，单测
  #   `refresh_只改元数据不动体` 钉着）⇒ **重验证会把 `Age` 归零**。
  #   ⇒ 预热在负载之前 ⇒ 中途没被刷新的话，跑完时 `Age` 必然 ≥ 负载时长；
  #   只要中途重验证过一次，`Age` 就被砍回去。
  if [ "$when_key" = after ]; then
    case "$age" in
      '' | *[!0-9]*)
        bad "$name（$when）Age 头缺失或不是整数：'${age:-（没有这个头）}' —— 判不了整趟有没有重验证"
        rc=1
        ;;
      *)
        if [ "$age" -lt "$DURATION_SECS" ]; then
          bad "$name（$when）Age=$age 小于负载时长 ${DURATION_SECS}s ⇒ **整趟中途发生过重验证**，量到的不全是命中"
          rc=1
        else
          ok "$name（$when）X-Fulcrum-Cache = $got，Age = $age ≥ ${DURATION_SECS}s ⇒ 整趟没重验证"
        fi
        ;;
    esac
    return "$rc"
  fi

  ok "$name（$when）X-Fulcrum-Cache = $got（Age = ${age:-?}）"
  return 0
}

run_variant() {
  local name=$1 port=$2 conf=$3 want=$4
  local log="$WORK/$name.log"

  echo "── $name ──"
  "${pin_server[@]}" "$FULCRUM_BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    --state-dir "$WORK/$name-state" > "$log" 2>&1 &
  CHILD=$!

  if ! wait_port "$port"; then
    bad "$name 没在 $port 上起来"
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    return 0
  fi

  # ⚠ 预热：第一发必然回源（MISS），把条目灌进缓存。⛔ 结果不判 ——
  #   本步要判的是**第二发之后**的稳态命中。
  curl -sS -o /dev/null "http://$HOST:$port$URL_PATH" 2>>"$log" || true

  if ! assert_cache_state "$name" "$port" "$want" before "$log"; then
    sed 's/^/      /' "$log" >&2 || true
    stop_child
    wait_port_gone "$port" || true
    return 0
  fi

  # ★ 命中的那份正文必须与 payload **逐字节相同**（同 respond-ceiling.sh:192）：
  #   两个变体之间的差要有意义，前提是两边回的是同一份字节。
  if ! cmp -s "$WORK/$name.body" "$WWW/payload.bin"; then
    bad "$name 回的正文与 payload **不是同一份字节**（$(wc -c < "$WORK/$name.body") 字节对 $PAYLOAD_BYTES）"
    stop_child
    wait_port_gone "$port" || true
    return 0
  fi
  ok "$name 命中的正文与 payload 逐字节相同（$PAYLOAD_BYTES 字节）"

  if "${pin_load[@]}" oha \
    --output-format json --no-tui \
    -z "$DURATION" -c "$CONNECTIONS" \
    -o "$DIAG_DIR/$name.json" \
    "http://$HOST:$port$URL_PATH" >> "$log" 2>&1; then
    ok "$name 的原始数据已落盘：diag/$NAME/$name.json"
  else
    bad "$name 那一趟负载没跑完"
    sed 's/^/      /' "$log" >&2 || true
  fi

  # ★ ★ 跑完之后**再核一次**：仍是同一个值 ⇒ 整趟没有发生重验证。
  #   ⚠ 中途过期的话量到的是 `REVALIDATED` 那条**要回源**的路而不是命中，
  #   而 oha 的 JSON 里看不出这件事 —— 这一次复核就是为它加的。
  assert_cache_state "$name" "$port" "$want" after "$log" || true

  stop_child
  wait_port_gone "$port" || bad "$name 收掉之后 $port 还占着 —— 下一个变体会起不来"
}

# ── 源站：整趟只起一次，两个变体共用 ───────────────────────────────────────
echo "── 源站（file_server，:$PORT_ORIGIN）──"
"${pin_server[@]}" "$FULCRUM_BIN" serve "$WORK/origin.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/origin.pid" \
  --upgrade-sock "$WORK/origin.sock" \
  --state-dir "$WORK/origin-state" > "$WORK/origin.log" 2>&1 &
ORIGIN_PID=$!
if ! wait_port "$PORT_ORIGIN"; then
  echo "CACHE-HIT-P99 FAILED: 源站没在 $PORT_ORIGIN 上起来" >&2
  sed 's/^/      /' "$WORK/origin.log" >&2 || true
  exit 1
fi
ok "源站起来了（ttl=$TTL，⇒ 远大于一趟负载的 $DURATION）"

# ⚠ 顺序有意固定：结果与顺序无关才对，⇒ 固定它是为了让两趟之间可比。
run_variant cache-mem "$PORT_MEM" "$WORK/mem.Fulcrumfile" HIT
run_variant cache-disk "$PORT_DISK" "$WORK/disk.Fulcrumfile" HIT-DISK

stop_origin
wait_port_gone "$PORT_ORIGIN" || bad "源站收掉之后 $PORT_ORIGIN 还占着"

# ── 差值落盘 ───────────────────────────────────────────────────────────────
#
# ★ 用 python3 序列化，⛔ 不手工拼 JSON。
# ★ **指名来源**：差值可被第三方按那两份原始读数重算，⛔ 不是一个孤零零的数。
if [ "$FAILS" = 0 ]; then
  DIAG_DIR="$DIAG_DIR" STATE_LOG="$STATE_LOG" TTL="$TTL" DURATION="$DURATION" \
    DURATION_SECS="$DURATION_SECS" python3 - <<'PY'
import json, os, pathlib
d = pathlib.Path(os.environ["DIAG_DIR"])

def p99(name):
    # ⚠ 键路径是 `latencyPercentiles.p99`（2026-09-09 对着
    #   bench/results/2026-09-06-static-throughput/ 那组真实读数实测）。
    # ⛔ **不是** summary.p99 —— summary 里根本没有 p99。
    # ⛔ 也不是 metrics.latency_ms.p99 —— 那份单位是**毫秒**，取错差 1000 倍
    #    而看着像个正常的数。
    return json.loads((d / name).read_text(encoding="utf-8"))["latencyPercentiles"]["p99"]

mem, disk = p99("cache-mem.json"), p99("cache-disk.json")

# ★ ★ ★ 观测到的 `X-Fulcrum-Cache` 落进原始数据 —— 这是「量的是不是那个后端的命中」
#   唯一留得下来的证据（打印行进了容器里的日志，`--rm` 之后就没了）。
#   ⇒ `tests/bench/gate.sh` 的 C18 会**独立复核**这四格，⛔ 不靠信任本脚本的控制流。
states = {}
for line in pathlib.Path(os.environ["STATE_LOG"]).read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    name, when, got, age = line.split("\t")
    # ★ `age` 与 `state` 一起记：只有 `age` 分得开「整趟都是命中」与
    #   「中途过期又被重验证回来」—— 后者最终那一次观测同样是 HIT。
    states.setdefault(name, {})[when] = {"state": got, "age": age}

(d / "delta.json").write_text(json.dumps({
    "what": "磁盘命中的 p99 − 内存命中的 p99（G150 ②）",
    "unit": "seconds",
    "p99_mem": mem,
    "p99_disk": disk,
    "delta": disk - mem,
    "source": {"mem": "cache-mem.json", "disk": "cache-disk.json"},
    # ⚠ ⚠ `state` 说的是「这个进程**真的**挑中了哪个后端」，⛔ 不是配置里写了什么
    #   （`dsl-reference.md` 的原话）。
    # ⛔ ⛔ **`after.state` 与 `before.state` 相同**这件事**证不了**「整趟没重验证」
    #   —— 2026-09-09 用反证实测证伪过（`ttl=1s` < `duration=2s` 时它照样全绿）：
    #   重验证之后条目重新变新鲜，之后的请求又是 `Hit`。
    # ★ 分得开的是 **`after.age`**：重验证会把 `stored_at` 置为当前时刻
    #   （`store.rs:208`）⇒ `Age` 归零。`after.age >= duration_secs`
    #   才代表这一条从预热起就没被刷新过。
    "cache_state": states,
    # ★ 口径：ttl 是从 duration 算出来的（10 倍），三个值一起记才说得清。
    "ttl": os.environ["TTL"],
    "duration": os.environ["DURATION"],
    "duration_secs": int(os.environ["DURATION_SECS"]),
    # ⚠ 有意不定阈值（G150 ② 明写）—— 定一个没有实测支撑的数字
    #   正是 G148 ③ 点名的那种错。阈值等有数之后再定。
    "threshold": None,
}, ensure_ascii=False, indent=2))
PY
  ok "差值已落盘：diag/$NAME/delta.json"
fi

echo
if [ "$FAILS" = 0 ]; then
  echo "[bench/diag/$NAME] 两个后端都跑通，原始数据与差值在 $DIAG_DIR"
  echo "[bench/diag/$NAME] ⛔ 本步**不产出任何结论**，也**不参与** §8 的判定"
else
  echo "[bench/diag/$NAME] ★ $FAILS 处失败 —— 这一趟的诊断数据不完整" >&2
  exit 1
fi
