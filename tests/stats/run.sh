#!/usr/bin/env bash
# `GET /stats` 的**可选字段与退化态**（M2 批 N 任务 7）。
#
# ★ ★ ★ **为什么它是独立一格，而不是往 `tests/serve/` 里再加一节**：
#   `/stats` 的「抓得到、字段在、与 /metrics 同源」已经有两处真 E2E 覆盖了
#   （`tests/serve/run.sh` 的 [3.6/4]、`tests/metrics/run.sh` 的 overrides 一节）。
#   ⇒ 再抓一遍**不构成**新场景的理由。本格只覆盖那两处**在结构上够不着**的东西：
#   它们各自的实例形态是固定的，而本格要的恰恰是**换着形态开**。
#
# ⚠ ⚠ ★ **一个被实测推翻过的设计（写在这里免得下一个人再推一遍）**：
#   本格初稿打算覆盖 `cache` / `certs` 两个 `Option` 的四种组合。**其中两格在
#   生产上根本到不了** —— 唯一的生产调用点 `crates/fulcrum-server/src/lib.rs:2146`
#   把两个参数都写死成 `Some(...)`（`cache` 更是在 `lib.rs:1828` 无条件构造）。
#   ⇒ `"cache": null` / `"certs": null` **只有 Rust 单测**里那个
#   `AdminApp::new(…, None, None, None)` 造得出来，真二进制一次都不会吐。
#   ⇒ 本格判的是**「空」与「非空」**，⛔ 不是「null 与非 null」。
#   ★ 那个 `Option` 该不该留，已登记给 owner（任务 7 brief §7）——**本格不改它**。
#
# 三条主题（都是今天一条判据都没有的）：
#   ① `certs: []`（有解析器、零证书）与 `certs: [一张]` 是**两个不同的状态**。
#      ⚠ `tests/serve/` 恒有 TLS 站点、`tests/metrics/` 恒无 ⇒ 各自只走得到一边，
#        而「空数组」这一边**从来没有任何判据看过**。
#   ② `cache.entries` 是**一次真读数**，不是「没接上所以给了个 0」——
#      判据挂在「缓存了东西之后它真的动了」，⛔ 不挂在「这个字段在不在」。
#      ★ 与 `fulcrum_overrides_active` 判据 4/5 分开写是同一条道理：
#        **「零」与「不存在」在读的人眼里长得一样**。
#   ③ `config_loaded_at_unix` 在一次 `POST /load` 之后**真的变了**。
#      今天只有「它是个数」这种判据，没有任何东西看它会不会动 ——
#      一个把它写成常量（比如进程启动时间）的实现能通过现有全部判据。
#
# 端口 9930–9935（★ 与其余场景全都错开，见
# docs/platform/host-and-gate-traps.md 那张端口表）：
#   9930 站点 S —— `http://s.example:9930`，带 `cache` + `reverse_proxy` 到 9931。
#   9931 上游 —— 一个真的 HTTP 上游（python）。
#   9932 站点 T —— **第二阶段**才起，`t.example:9932` 带 `tls` ⇒ `certs` 从空变非空。
#
# ⚠ 本格三个站点全部显式写端口且第一阶段全是 `http://` ⇒ **不合成 `:80` 重定向站点**，
#   与那个共享端口无关。★ 但 cleanup 仍然断言端口已经还回去（照 tests/quic-relay/run.sh）。

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
S_PORT=${S_PORT:-9930}
UP_PORT=${UP_PORT:-9931}
T_PORT=${T_PORT:-9932}
ADMIN_SOCK="$WORK/admin.sock"

FAILS=0
PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

# ★ ★ ★ 「捕获一个可能失败的命令」的唯一写法，连同它为什么必须存在，都在
#   tests/lib/capture.sh 里（任务 7 收敛）。⛔ 本文件里不许再出现裸 `VAR=$(可能失败的命令)`。
# shellcheck source=tests/lib/capture.sh
. "$REPO/tests/lib/capture.sh"

port_listening() {
  timeout 1 bash -c "exec 3<>/dev/tcp/$HOST/$1" 2>/dev/null
}

wait_port() {
  local port=$1 tries=0
  while [ "$tries" -lt 100 ]; do
    if port_listening "$port"; then return 0; fi
    sleep 0.1
    tries=$((tries + 1))
  done
  return 1
}

cleanup() {
  local pid waited p leaked=""
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -9 "$pid" 2>/dev/null || true
  done

  # ── ★ ★ 收尾自证：本场景用过的端口，走的时候必须全部还回去 ────────────────
  #   照 tests/quic-relay/run.sh 那段。判据挂在「端口还回去了没有」而不是
  #   「进程还在不在」：前者才是下一个场景真会被绊到的东西。
  for p in "$S_PORT" "$UP_PORT" "$T_PORT"; do
    waited=0
    while port_listening "$p" && [ "$waited" -lt 30 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    if port_listening "$p"; then leaked="$leaked $p"; fi
  done
  rm -rf "$WORK"

  if [ -n "$leaked" ]; then
    echo >&2
    echo "STATS TESTS FAILED: 收尾没干净 —— 退出时这些端口还有人在听：$leaked" >&2
    echo "  ⇒ 多半是某个 pid 没进 \$PIDS（经典写法：\`X=\$(start …)\`，\$(…) 是子 shell，" >&2
    echo "     数组改的是副本）。⚠ 后果不在本场景：泄漏的进程活到下一个场景去。" >&2
    pgrep -af "fulcrum serve" >&2 || true
    exit 1
  fi
}
trap cleanup EXIT

# 管理面：回状态码，正文落 $WORK/admin.out（与其余场景同一个约定）。
admin_get() {
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" "http://localhost$1" 2>/dev/null || echo "000"
}
admin_post() {
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# 一条数值断言。$1 说明 · $2 期望 · $3 实际。
eq() {
  if [ "$2" = "$3" ]; then
    ok "$1（$3）"
  else
    fail "$1：期望「$2」实际「$3」"
  fi
}

# ── /stats 的读取端 ─────────────────────────────────────────────────────────
#
# ⚠ 用 python 而不是 grep：`grep '"certs":\[\]'` 会在字段顺序、空格、转义任一处
#   变化时静默漏判 —— 而「静默漏判」正好长得像「判据通过」。
# ⚠ ★ 读不懂就**当场非零退出**，不猜、不给默认值：一个「找不到就回 0」的读取端
#   会让下面每一条判据在服务端彻底坏掉时**依然是绿的**。
#   ⇒ 它自己带 selftest，开跑前先证「它命中得了，也落空得了」。
STATS_PY="$WORK/stats_check.py"
cat > "$STATS_PY" <<'PY'
#!/usr/bin/env python3
"""只服务于 tests/stats/run.sh：读 /stats 的 JSON，按子命令回答一个具体问题。
⚠ 读不懂 / 找不到就非零退出——不猜、不吞、不给默认值。"""
import json
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def need_key(v, k):
    if k not in v:
        print(f"/stats 里没有顶层字段 `{k}`", file=sys.stderr)
        raise SystemExit(1)
    return v[k]


def main(argv):
    cmd = argv[1]

    # ★ 自证：命中与落空各证一遍。⛔ 一个恒答「有」或恒答 0 的读取端，
    #   会让本场景每一条判据都变成空转，而门依然全绿。
    if cmd == "selftest":
        rc = 0
        sample = {"certs": [], "cache": {"bytes": 0, "entries": 0},
                  "config_loaded_at_unix": 1.5, "upstreams": []}
        if len(sample["certs"]) != 0:
            print("selftest: 空数组竟然不是 0 长", file=sys.stderr)
            rc = 1
        # 落空那一边：不存在的键必须抛，⛔ 不许回一个默认值。
        try:
            need_key(sample, "根本没有这个键")
            print("selftest: 缺字段竟然没抛", file=sys.stderr)
            rc = 1
        except SystemExit:
            pass
        # 命中那一边：存在的键必须取得到，且值原样。
        if need_key(sample, "config_loaded_at_unix") != 1.5:
            print("selftest: 取到的值不是原样", file=sys.stderr)
            rc = 1
        if rc == 0:
            print("stats_check.py 自证通过（命中与落空各证一遍）")
        return rc

    v = load(argv[2])

    if cmd == "certs_len":
        print(len(need_key(v, "certs")))
    elif cmd == "certs_domains":
        # ★ 逐项列出，⛔ 不只回条数：一个只渲染第一项的实现，`len()` 仍然是对的。
        print(",".join(sorted(c["domain"] for c in need_key(v, "certs"))))
    elif cmd == "cert_not_after_positive":
        # `not_after_unix` 必须是**绝对 Unix 秒**（裁决 R5）⇒ 远大于 0。
        # ⚠ 判「> 10 亿」而不是「> 0」：一个把「还剩几秒」写进去的实现
        #   （相对秒，几百万）也 > 0，那种错正是 R5 挡的。
        certs = need_key(v, "certs")
        # ⚠ ⚠ ★ **空列表要当场判负，⛔ 不许「没有坏项 ⇒ OK」。**
        #   实测教训：注入「certs 恒空」时本条**照样是绿的** —— 一条对空集
        #   恒真的判据，在最该说话的时候正好沉默。★ 这与本仓
        #   「取样前先确认样本里真有东西」是同一条。
        if not certs:
            print("certs 是空的——本条判据对空集恒真，⇒ 当场判负而不是放行")
            return 0
        bad = [c for c in certs if not c["not_after_unix"] > 1_000_000_000]
        print("OK" if not bad else f"不是绝对 Unix 秒：{bad}")
    elif cmd == "cache_entries":
        print(need_key(v, "cache")["entries"])
    elif cmd == "cache_is_object":
        c = need_key(v, "cache")
        print("OK" if isinstance(c, dict) and "bytes" in c and "entries" in c else f"形状不对：{c}")
    elif cmd == "field":
        print(need_key(v, argv[3]))
    elif cmd == "upstreams_len":
        print(len(need_key(v, "upstreams")))
    else:
        print(f"不认识的子命令：{cmd}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
PY

sc() { python3 "$STATS_PY" "$@"; }

# ── [0/5] 基线 ──────────────────────────────────────────────────────────────
echo "=== [0/5] 基线：三个端口空着 · /stats 读取端自证 ==="
for p in "$S_PORT" "$UP_PORT" "$T_PORT"; do
  if port_listening "$p"; then
    echo "STATS TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
ok "三个端口都空着"
if sc selftest; then
  ok "★★ /stats 读取端自证通过 —— 下面每一条判据才有意义"
else
  echo "STATS TESTS FAILED: stats_check.py 自证未过，本次结果一律不可采信。" >&2
  exit 1
fi

# ── [1/5] 起上游与被测实例（第一代：无 TLS 站点）───────────────────────────
echo "=== [1/5] 起上游与被测实例（第一代：一个 TLS 站点都没有）==="

python3 -c "
import http.server, socketserver
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'up-ok'
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.send_header('Content-Length', str(len(body)))
        # ★ 明确可缓存：本格的 cache.entries 判据要它真的进得了缓存。
        self.send_header('Cache-Control', 'max-age=60')
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
socketserver.TCPServer.allow_reuse_address = True
socketserver.TCPServer(('$HOST', $UP_PORT), H).serve_forever()
" > "$WORK/up.log" 2>&1 &
PIDS+=($!)
wait_port "$UP_PORT" || {
  echo "STATS TESTS FAILED: 上游 $UP_PORT 起不来" >&2
  cat "$WORK/up.log" >&2
  exit 1
}
ok "上游 $UP_PORT 起来了"

cat > "$WORK/gen1.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
http://s.example:$S_PORT {
    cache {
        ttl 60s
        capacity 1MB
    }
    reverse_proxy $HOST:$UP_PORT
}
CONF

RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/gen1.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/f.pid" \
  --upgrade-sock "$WORK/f.sock" \
  > "$WORK/f.log" 2>&1 &
# ★ 单独记住被测实例的 pid：[5/5] 要**只停它**，而 `PIDS` 里还有上游，
#   清空整个数组会让 `cleanup` 丢掉上游（初稿就是这么泄漏 9931 的）。
F1_PID=$!
PIDS+=("$F1_PID")
wait_port "$S_PORT" || {
  echo "STATS TESTS FAILED: 被测实例 $S_PORT 起不来。日志：" >&2
  cat "$WORK/f.log" >&2
  exit 1
}
ok "被测实例起来了（站点 S 在 $S_PORT）"

# ── [2/5] 主题 ①：certs 是**空数组**，不是 null、也不是缺字段 ───────────────
echo "=== [2/5] 主题 ①：一个 TLS 站点都没有 ⇒ certs 是空数组 ==="

capture_ok "GET /stats（第一代）" admin_get /stats
eq "GET /stats（真 unix socket）" 200 "$CAPTURE_OUT"
cp "$WORK/admin.out" "$WORK/stats1.json"

capture_ok "certs 的条数（第一代）" sc certs_len "$WORK/stats1.json"
eq "★★★ 无 TLS 站点 ⇒ certs 是**空数组**（字段在、值为空 —— 与「字段不在」是两回事）" \
  0 "$CAPTURE_OUT"

# ★ ★ cache 那一格同一刻也要看：它必须是**一个对象**（有 bytes 与 entries），
#   ⛔ 不是 null。这一条与上面那条合起来钉住「两个可选字段在真二进制上恒为非 null」。
capture_ok "cache 的形状（第一代）" sc cache_is_object "$WORK/stats1.json"
eq "★★★ cache 是一个真对象（bytes + entries 都在）—— 生产上它到不了 null" \
  OK "$CAPTURE_OUT"

capture_ok "cache.entries（还没请求过）" sc cache_entries "$WORK/stats1.json"
eq "★ 夹具前提：这一刻缓存里一条都没有" 0 "$CAPTURE_OUT"

# ── [3/5] 主题 ②：cache.entries 是真读数（缓存了东西之后它真的动）──────────
echo "=== [3/5] 主题 ②：cache.entries 是一次真读数，不是恒 0 ==="

# ⚠ ⚠ 判据**不能**只看「entries 是 0」——一个把 cache 那一格硬编码成
#   `{bytes:0, entries:0}` 的实现在上面那条判据上是绿的。
#   ⇒ 唯一分得开的写法是**让它动一次**，比前后两个读数。
capture_ok "打一条会被缓存的请求" curl -sS -o /dev/null -w '%{http_code}' \
  --max-time 5 -H "Host: s.example" "http://$HOST:$S_PORT/cacheable"
eq "请求走通（回源到上游）" 200 "$CAPTURE_OUT"

capture_ok "GET /stats（缓存之后）" admin_get /stats
eq "GET /stats（缓存之后）" 200 "$CAPTURE_OUT"
cp "$WORK/admin.out" "$WORK/stats2.json"

capture_ok "cache.entries（缓存之后）" sc cache_entries "$WORK/stats2.json"
eq "★★★ 缓存了一条之后 cache.entries 真的动了 ⇒ 它是活体读数，不是常量" \
  1 "$CAPTURE_OUT"

# ── [4/5] 主题 ③：config_loaded_at_unix 在一次 /load 之后真的变了 ───────────
echo "=== [4/5] 主题 ③：config_loaded_at_unix 会随 /load 变 ==="

capture_ok "换代之前的 config_loaded_at_unix" sc field "$WORK/stats2.json" config_loaded_at_unix
T_BEFORE="$CAPTURE_OUT"
ok "换代之前 config_loaded_at_unix = $T_BEFORE"

# ★ 睡一下，否则两次装载可能落在同一个秒里 —— 那样「变了没有」这条判据
#   会**间歇性**地假绿，而间歇性假绿比恒假绿更难查。
sleep 1.1

# 第二代：加一个 TLS 站点（主题 ① 的另一半），并保留站点 S。
# ⚠ 监听端口集变了（多了 $T_PORT）——`/load` 对端口集变化是 409。
#   ⇒ 本格**有意**不走 /load 换端口，而是：先用同一套端口集 load 一次
#     （证明时间戳会动），TLS 那半留给第三代重开一个实例。
cat > "$WORK/gen2.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
http://s.example:$S_PORT {
    cache {
        ttl 60s
        capacity 1MB
    }
    handle /second/* {
        reverse_proxy $HOST:$UP_PORT
    }
    handle {
        reverse_proxy $HOST:$UP_PORT
    }
}
CONF
capture_ok "compile 第二代" "$BIN" compile "$WORK/gen2.Fulcrumfile"
printf '%s' "$CAPTURE_OUT" > "$WORK/gen2.json"

capture_ok "POST /load（overrides 必填，G120）" \
  admin_post "/load?overrides=clear" "$(cat "$WORK/gen2.json")"
eq "换代成功" 200 "$CAPTURE_OUT"

capture_ok "GET /stats（换代之后）" admin_get /stats
eq "GET /stats（换代之后）" 200 "$CAPTURE_OUT"
cp "$WORK/admin.out" "$WORK/stats3.json"

capture_ok "换代之后的 config_loaded_at_unix" sc field "$WORK/stats3.json" config_loaded_at_unix
T_AFTER="$CAPTURE_OUT"
if [ "$T_AFTER" = "$T_BEFORE" ]; then
  fail "★★★ config_loaded_at_unix 在一次 /load 之后没变（$T_AFTER）—— 它是个常量，不是装载时间"
else
  ok "★★★ config_loaded_at_unix 随 /load 变了（$T_BEFORE → $T_AFTER）"
fi

# ★ 顺带：换代之后 upstreams 有两行（两条 reverse_proxy 指同一台）。
#   ⚠ ⚠ 判据要 ≥2 且**不等于 1**：只有一行时「按地址归并了没有」测不出来。
capture_ok "换代之后 upstreams 的行数" sc upstreams_len "$WORK/stats3.json"
eq "★★ 两条 reverse_proxy 指同一台 ⇒ upstreams 出两行（⛔ 不按地址归并）" \
  2 "$CAPTURE_OUT"

# ── [5/5] 主题 ① 的另一半：有 TLS 站点 ⇒ certs 非空且逐项对得上 ─────────────
echo "=== [5/5] 主题 ① 另一半：起一个带 TLS 的实例 ⇒ certs 从空变非空 ==="

# 停掉**被测实例**，把 $S_PORT 还回去（⚠ 端口集变了，`/load` 会 409 ——
# 换端口不是本格要测的东西，所以这里重开一个实例而不是换代）。
#
# ⚠ ⚠ ★ **只停被测实例，⛔ 不动上游、⛔ 不清空 `PIDS`。**
#   初稿在这里 `kill` 了 `PIDS` 里所有 pid 然后 `PIDS=()`，结果：上游没被 SIGINT 收掉，
#   而它的 pid 已经被从数组里抹掉 ⇒ `cleanup` 再也找不到它 ⇒ **端口 9931 泄漏到下一个场景**。
#   ★ 那次泄漏是被本文件 `cleanup` 里那段「端口还回去了没有」自证抓住的
#   —— 全部 18 条业务判据当时都是绿的。**这就是那段自证存在的全部理由。**
kill -INT "$F1_PID" 2>/dev/null || true
waited=0
while port_listening "$S_PORT" && [ "$waited" -lt 50 ]; do sleep 0.1; waited=$((waited + 1)); done
if port_listening "$S_PORT"; then
  echo "STATS TESTS FAILED: 第一代停了之后 $S_PORT 还有人在听，后面的判据不可采信。" >&2
  exit 1
fi
ok "第一代已停，$S_PORT 还回去了（上游仍在跑，由 cleanup 收）"

openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=t.example" -addext "subjectAltName=DNS:t.example" \
  > "$WORK/openssl.log" 2>&1 || {
  echo "STATS TESTS FAILED: openssl 生成自签证书失败" >&2
  cat "$WORK/openssl.log" >&2
  exit 1
}
ok "自签证书生成好了（现签，⛔ 不入库）"

cat > "$WORK/gen3.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
t.example:$T_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 t-ok
}
CONF

RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/gen3.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/f3.pid" \
  --upgrade-sock "$WORK/f3.sock" \
  > "$WORK/f3.log" 2>&1 &
PIDS+=($!)
wait_port "$T_PORT" || {
  echo "STATS TESTS FAILED: 带 TLS 的实例 $T_PORT 起不来。日志：" >&2
  cat "$WORK/f3.log" >&2
  exit 1
}
ok "带 TLS 的实例起来了（站点 T 在 $T_PORT）"

capture_ok "GET /stats（带 TLS 的实例）" admin_get /stats
eq "GET /stats（带 TLS 的实例）" 200 "$CAPTURE_OUT"
cp "$WORK/admin.out" "$WORK/stats4.json"

capture_ok "certs 的条数（带 TLS）" sc certs_len "$WORK/stats4.json"
eq "★★★ 有一个 TLS 站点 ⇒ certs 非空 —— 与 [2/5] 那次的空数组是**两个不同的状态**" \
  1 "$CAPTURE_OUT"

# ★ 逐项断到，⛔ 不只断条数（表头 .len() 在只渲染第一项时仍然是对的）。
capture_ok "certs 的域名逐项" sc certs_domains "$WORK/stats4.json"
eq "★★ certs 逐项对得上（域名就是配置里那个）" "t.example" "$CAPTURE_OUT"

capture_ok "not_after_unix 是绝对 Unix 秒" sc cert_not_after_positive "$WORK/stats4.json"
eq "★★★ not_after_unix 是**绝对 Unix 秒**（裁决 R5）⛔ 不是「还剩几秒」" \
  OK "$CAPTURE_OUT"

# ── 收尾 ────────────────────────────────────────────────────────────────────
echo
if [ "$FAILS" -eq 0 ]; then
  echo "STATS TESTS PASSED —— /stats 的可选字段与退化态真的在跑（certs 空数组与非空是两个状态且逐项对得上 · not_after_unix 是绝对 Unix 秒 · cache 是真对象且 entries 会随缓存动 · config_loaded_at_unix 会随 /load 变 · 两条 reverse_proxy 指同一台时 upstreams 出两行不归并）。"
else
  echo "STATS TESTS FAILED：$FAILS 条断言没过。" >&2
  echo "── 被测实例日志 ──" >&2
  cat "$WORK"/f*.log >&2 2>/dev/null || true
  echo "── 最后一次 /stats 正文 ──" >&2
  cat "$WORK/admin.out" >&2 2>/dev/null || true
  exit 1
fi
