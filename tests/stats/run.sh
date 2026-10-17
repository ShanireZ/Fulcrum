#!/usr/bin/env bash
# `GET /stats` 的**可選欄位與退化態**（M2 批 N 任務 7）。
#
# ★ ★ ★ **為什麼它是獨立一格，而不是往 `tests/serve/` 裡再加一節**：
#   `/stats` 的「抓得到、欄位在、與 /metrics 同源」已經有兩處真 E2E 覆蓋了
#   （`tests/serve/run.sh` 的 [3.6/4]、`tests/metrics/run.sh` 的 overrides 一節）。
#   ⇒ 再抓一遍**不構成**新場景的理由。本格只覆蓋那兩處**在結構上夠不著**的東西：
#   它們各自的執行個體形態是固定的，而本格要的恰恰是**換著形態開**。
#
# ⚠ ⚠ ★ **一個被實測推翻過的設計（寫在這裡免得下一個人再推一遍）**：
#   本格初稿打算覆蓋 `cache` / `certs` 兩個 `Option` 的四種組合。**其中兩格在
#   生產上根本到不了** —— 唯一的生產呼叫點 `crates/fulcrum-server/src/lib.rs:2146`
#   把兩個參數都寫死成 `Some(...)`（`cache` 更是在 `lib.rs:1828` 無條件建構）。
#   ⇒ `"cache": null` / `"certs": null` **只有 Rust 單測**裡那個
#   `AdminApp::new(…, None, None, None)` 造得出來，真二進位一次都不會吐。
#   ⇒ 本格判的是**「空」與「非空」**，⛔ 不是「null 與非 null」。
#   ★ 那個 `Option` 該不該留，已登記給 owner（任務 7 brief §7）——**本格不改它**。
#
# 三條主題（都是今天一條判據都沒有的）：
#   ① `certs: []`（有解析器、零證書）與 `certs: [一张]` 是**兩個不同的狀態**。
#      ⚠ `tests/serve/` 恆有 TLS 站台、`tests/metrics/` 恆無 ⇒ 各自只走得到一邊，
#        而「空陣列」這一邊**從來沒有任何判據看過**。
#   ② `cache.entries` 是**一次真讀數**，不是「沒接上所以給了個 0」——
#      判據掛在「快取了東西之後它真的動了」，⛔ 不掛在「這個欄位在不在」。
#      ★ 與 `fulcrum_overrides_active` 判據 4/5 分開寫是同一條道理：
#        **「零」與「不存在」在讀的人眼裡長得一樣**。
#   ③ `config_loaded_at_unix` 在一次 `POST /load` 之後**真的變了**。
#      今天只有「它是個數」這種判據，沒有任何東西看它會不會動 ——
#      一個把它寫成常數（比如處理程序啟動時間）的實作能通過現有全部判據。
#
# 連接埠 9930–9935（★ 與其餘場景全都錯開，見
# docs/platform/host-and-gate-traps.md 那張連接埠表）：
#   9930 站台 S —— `http://s.example:9930`，帶 `cache` + `reverse_proxy` 到 9931。
#   9931 上游 —— 一個真的 HTTP 上游（python）。
#   9932 站台 T —— **第二階段**才起，`t.example:9932` 帶 `tls` ⇒ `certs` 從空變非空。
#
# ⚠ ⚠ **本格不碰共享的 `:80`，而這件事需要兩句話才成立**：
#   ① 第一、二代的站台寫的是 `http://s.example:9930` ⇒ 天然不合成重定向站台；
#   ② 第三代寫的是 `t.example:9932`（沒寫 scheme ⇒ 推成 `https` ⇒ `auto_https`），
#      它**本來會**合成一個 `:80` 站台 —— 由那一份設定裡的 `auto_http_redirect false` 按住，
#      並由一條**編譯期**判據守著（見 gen3 那一段）。
#   ★ 這一段此前只寫了 ①，於是那句「不合成 `:80` 重定向站台」在第三代上是**假話**，
#     而它讀起來完全成立 —— 2026-09-03 實測 `fulcrum compile` 才發現（當時本格確實在綁 `:80`，
#     且不在 AGENTS.md 那張表點名的名單裡）。
# ★ cleanup 仍然斷言本格用過的連接埠都已還回去（照 tests/quic-relay/run.sh）。

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

# ★ ★ ★ 「捕獲一個可能失敗的命令」的唯一寫法，連同它為什麼必須存在，都在
#   tests/lib/capture.sh 裡（任務 7 收斂）。⛔ 本檔案裡不許再出現裸 `VAR=$(可能失败的命令)`。
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

  # ── ★ ★ 收尾自證：本場景用過的連接埠，走的時候必須全部還回去 ────────────────
  #   照 tests/quic-relay/run.sh 那段。判據掛在「連接埠還回去了沒有」而不是
  #   「處理程序還在不在」：前者才是下一個場景真會被絆到的東西。
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

# 管理面：回狀態碼，正文落 $WORK/admin.out（與其餘場景同一個約定）。
admin_get() {
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" "http://localhost$1" 2>/dev/null || echo "000"
}
admin_post() {
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# 一條數值斷言。$1 說明 · $2 期望 · $3 實際。
eq() {
  if [ "$2" = "$3" ]; then
    ok "$1（$3）"
  else
    fail "$1：期望「$2」实际「$3」"
  fi
}

# ── /stats 的讀取端 ─────────────────────────────────────────────────────────
#
# ⚠ 用 python 而不是 grep：`grep '"certs":\[\]'` 會在欄位順序、空格、轉義任一處
#   變化時靜默漏判 —— 而「靜默漏判」正好長得像「判據通過」。
# ⚠ ★ 讀不懂就**當場非零退出**，不猜、不給預設值：一個「找不到就回 0」的讀取端
#   會讓下面每一條判據在伺服器端徹底壞掉時**依然是綠的**。
#   ⇒ 它自己帶 selftest，開跑前先證「它命中得了，也落空得了」。
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


def certs_len_of(v):
    """`certs` 的条数；`null` **当场判负**。回 (rc, 一行输出)。

    ⚠ ⚠ `null` 要说清楚，⛔ 不许靠 `len(None)` 抛 TypeError 崩掉 —— 崩出来的是一段
    traceback，而读日志的人要多花一轮才看出「哦，它是 null」。
    ★ 这一格与 `cache_is_object` 合起来钉的是「两个可选字段在真二进制上恒为非 null」
    （`admin.rs` 上那两段字段文档指的就是这里）。
    ⚠ 它只证得了**当前构型**，⛔ 证不了「所有构型」—— 而那正是这条契约今天的全部内容。
    """
    c = need_key(v, "certs")
    if c is None:
        return 1, ("/stats 的 `certs` 是 null —— 而生产装配无条件传 Some(…)，"
                   "它到不了这个状态")
    return 0, str(len(c))


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
        # ★ `certs` 的 null 判据也要两个方向 —— ⛔ 且走的是**判据本体那段代码**，
        #   不是在这里重写一遍（重写一遍的自证什么都证不了）。
        if certs_len_of({"certs": []}) != (0, "0"):
            print("selftest: 空数组竟然没被判成 0 条", file=sys.stderr)
            rc = 1
        if certs_len_of({"certs": None})[0] == 0:
            print("selftest: certs 是 null 竟然没被判负 —— 那条判据是空的", file=sys.stderr)
            rc = 1
        if rc == 0:
            print("stats_check.py 自证通过（命中与落空各证一遍）")
        return rc

    v = load(argv[2])

    if cmd == "certs_len":
        rc, out = certs_len_of(v)
        print(out, file=sys.stderr if rc else sys.stdout)
        if rc:
            return rc
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

# ── [0/5] 基線 ──────────────────────────────────────────────────────────────
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

# ── [1/5] 起上游與被測執行個體（第一代：無 TLS 站台）───────────────────────────
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
# ★ 單獨記住被測執行個體的 pid：[5/5] 要**只停它**，而 `PIDS` 裡還有上游，
#   清空整個陣列會讓 `cleanup` 丟掉上游（初稿就是這麼洩漏 9931 的）。
F1_PID=$!
PIDS+=("$F1_PID")
wait_port "$S_PORT" || {
  echo "STATS TESTS FAILED: 被测实例 $S_PORT 起不来。日志：" >&2
  cat "$WORK/f.log" >&2
  exit 1
}
ok "被测实例起来了（站点 S 在 $S_PORT）"

# ── [2/5] 主題 ①：certs 是**空陣列**，不是 null、也不是缺欄位 ───────────────
echo "=== [2/5] 主题 ①：一个 TLS 站点都没有 ⇒ certs 是空数组 ==="

capture_ok "GET /stats（第一代）" admin_get /stats
eq "GET /stats（真 unix socket）" 200 "$CAPTURE_OUT"
cp "$WORK/admin.out" "$WORK/stats1.json"

capture_ok "certs 的条数（第一代）" sc certs_len "$WORK/stats1.json"
eq "★★★ 无 TLS 站点 ⇒ certs 是**空数组**（字段在、值为空 —— 与「字段不在」是两回事）" \
  0 "$CAPTURE_OUT"

# ★ ★ cache 那一格同一刻也要看：它必須是**一個物件**（有 bytes 與 entries），
#   ⛔ 不是 null。這一條與上面那條合起來釘住「兩個可選欄位在真二進位上恆為非 null」。
capture_ok "cache 的形状（第一代）" sc cache_is_object "$WORK/stats1.json"
eq "★★★ cache 是一个真对象（bytes + entries 都在）—— 生产上它到不了 null" \
  OK "$CAPTURE_OUT"

capture_ok "cache.entries（还没请求过）" sc cache_entries "$WORK/stats1.json"
eq "★ 夹具前提：这一刻缓存里一条都没有" 0 "$CAPTURE_OUT"

# ── [3/5] 主題 ②：cache.entries 是真讀數（快取了東西之後它真的動）──────────
echo "=== [3/5] 主题 ②：cache.entries 是一次真读数，不是恒 0 ==="

# ⚠ ⚠ 判據**不能**只看「entries 是 0」——一個把 cache 那一格硬編碼成
#   `{bytes:0, entries:0}` 的實作在上面那條判據上是綠的。
#   ⇒ 唯一分得開的寫法是**讓它動一次**，比前後兩個讀數。
capture_ok "打一条会被缓存的请求" curl -sS -o /dev/null -w '%{http_code}' \
  --max-time 5 -H "Host: s.example" "http://$HOST:$S_PORT/cacheable"
eq "请求走通（回源到上游）" 200 "$CAPTURE_OUT"

# ⚠ ⚠ ★ **先輪詢，⛔ 不能 curl 一回來就死讀一次**（2026-09-10 坐實；此前完整門禁裡紅過）：
#   產品在**回應體寫完之後**才把條目存進快取 —— `crates/fulcrum-server/src/lib.rs` 裡
#   `write_response_body(…, true)` 在前、`store_if_allowed` 在後，而這是**有意的**：
#   快取寫入不該掛在用戶端的延遲路徑上。上游帶 `Content-Length` ⇒ curl 在最後一個體位元組
#   到達時就退出了，此後伺服器端還要走完「讀到 EOF → 壓縮收尾 → 寫空尾塊 → 存」。
#   ⇒ 一次死讀在機器滿載時會讀到 0（完整門禁裡紅過；單跑 `STATS_ONLY` 從沒紅過）。
#   ★ 在那兩行之間注入 300 ms 延遲 ⇒ 舊寫法 100% 紅，訊息逐字相同。
# ★ 輪詢**不削弱**這條判據要抓的東西：一個把 cache 硬編碼成 `{entries:0}` 的實作永遠
#   等不到 1 ⇒ 超時後下面那條照原樣紅。⚠ 它放棄的是「存入變慢、但慢不過 5 秒」那一族，
#   而那從來不是這條判據的目標。
# ⛔ 循環裡只取數、不判定（用 `capture` 不用 `capture_ok`，否則每輪沒等到都記一筆失敗）；
#   判定仍是下面那幾行原樣的斷言。
C_TRIES=0
while [ "$C_TRIES" -lt 50 ]; do
  capture admin_get /stats
  if [ "$CAPTURE_OUT" = 200 ]; then
    capture sc cache_entries "$WORK/admin.out"
    if [ "$CAPTURE_RC" -eq 0 ] && [ "$CAPTURE_OUT" = 1 ]; then break; fi
  fi
  sleep 0.1
  C_TRIES=$((C_TRIES + 1))
done
if [ "$C_TRIES" -lt 50 ]; then
  echo "  · 第 $((C_TRIES + 1)) 次读 /stats 看到条目落地（上限 50 次 ≈ 5 秒）"
else
  echo "  · 读了 50 次（≈ 5 秒）条目仍未落地 —— 下面那条会照原样判红"
fi

capture_ok "GET /stats（缓存之后）" admin_get /stats
eq "GET /stats（缓存之后）" 200 "$CAPTURE_OUT"
cp "$WORK/admin.out" "$WORK/stats2.json"

capture_ok "cache.entries（缓存之后）" sc cache_entries "$WORK/stats2.json"
eq "★★★ 缓存了一条之后 cache.entries 真的动了 ⇒ 它是活体读数，不是常量" \
  1 "$CAPTURE_OUT"

# ── [4/5] 主題 ③：config_loaded_at_unix 在一次 /load 之後真的變了 ───────────
echo "=== [4/5] 主题 ③：config_loaded_at_unix 会随 /load 变 ==="

capture_ok "换代之前的 config_loaded_at_unix" sc field "$WORK/stats2.json" config_loaded_at_unix
T_BEFORE="$CAPTURE_OUT"
ok "换代之前 config_loaded_at_unix = $T_BEFORE"

# ★ 睡一下，否則兩次裝載可能落在同一個秒裡 —— 那樣「變了沒有」這條判據
#   會**間歇性**地假綠，而間歇性假綠比恆假綠更難查。
sleep 1.1

# 第二代：加一個 TLS 站台（主題 ① 的另一半），並保留站台 S。
# ⚠ 監聽連接埠集變了（多了 $T_PORT）——`/load` 對連接埠集變化是 409。
#   ⇒ 本格**有意**不走 /load 換連接埠，而是：先用同一套連接埠集 load 一次
#     （證明時間戳會動），TLS 那半留給第三代重開一個執行個體。
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
# ⚠ 同上：**分流 stderr**。這一份設定今天沒有 `tls` ⇒ stderr 恰好是空的，
#   於是原來那種 `capture_ok` + `printf "$CAPTURE_OUT"` 的寫法**靠運氣**是對的 ——
#   誰給它加一行 `tls`，那句「已裝載 N 個 SNI 的證書」就會被拼進 JSON 裡。
if ! "$BIN" compile "$WORK/gen2.Fulcrumfile" > "$WORK/gen2.json" 2> "$WORK/gen2.err"; then
  fail "compile 第二代失败：$(head -3 "$WORK/gen2.err" | tr '
' ' ')"
fi

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

# ★ 順帶：換代之後 upstreams 有兩行（兩條 reverse_proxy 指同一臺）。
#   ⚠ ⚠ 判據要 ≥2 且**不等於 1**：只有一行時「按位址歸併了沒有」測不出來。
capture_ok "换代之后 upstreams 的行数" sc upstreams_len "$WORK/stats3.json"
eq "★★ 两条 reverse_proxy 指同一台 ⇒ upstreams 出两行（⛔ 不按地址归并）" \
  2 "$CAPTURE_OUT"

# ── [5/5] 主題 ① 的另一半：有 TLS 站台 ⇒ certs 非空且逐項對得上 ─────────────
echo "=== [5/5] 主题 ① 另一半：起一个带 TLS 的实例 ⇒ certs 从空变非空 ==="

# 停掉**被測執行個體**，把 $S_PORT 還回去（⚠ 連接埠集變了，`/load` 會 409 ——
# 換連接埠不是本格要測的東西，所以這裡重開一個執行個體而不是換代）。
#
# ⚠ ⚠ ★ **只停被測執行個體，⛔ 不動上游、⛔ 不清空 `PIDS`。**
#   初稿在這裡 `kill` 了 `PIDS` 裡所有 pid 然後 `PIDS=()`，結果：上游沒被 SIGINT 收掉，
#   而它的 pid 已經被從陣列裡抹掉 ⇒ `cleanup` 再也找不到它 ⇒ **連接埠 9931 洩漏到下一個場景**。
#   ★ 那次洩漏是被本檔案 `cleanup` 裡那段「連接埠還回去了沒有」自證抓住的
#   —— 全部 18 條業務判據當時都是綠的。**這就是那段自證存在的全部理由。**
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
    auto_http_redirect false
}
t.example:$T_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 t-ok
}
CONF

# ── ★ ★ ★ 那行 `auto_http_redirect false` 是**承重的，不是樣板** ──────────────
#
# 「t.example:$T_PORT」沒寫 scheme ⇒ `parse_address` 按主機名非空推成 `https`
# ⇒ `auto_https` 為真 ⇒ 而 `auto_http_redirect` **預設是開的**
# ⇒ `synthesize_http_redirect` 會給它合成一個 **`http://t.example:80`** 站台。
# ⚠ ⚠ 那會讓本場景**隱式綁住共享的 `:80`** —— 而本儲存庫實測過：佔著 `:80` 能讓
#   **別的連接埠**起不來（`ListenerEndpoint::listen` 持著處理程序級 `ListenFds` 鎖去 bind，
#   一個佔用把那把鎖停住最多 30 秒），症狀落在一個無辜的連接埠上。
# ⚠ 寫 `tls <crt> <key>` **摘不掉**它：那一行只決定 TLS 開不開，與 `auto_https` 無關。
#
# ★ ★ 這條判據**落在編譯產物上而不是執行環境**：它是確定性的（不拼時序、不拼誰先起），
#   而且刪掉上面那一行就當場紅。
# ⚠ ⚠ **compile 的輸出不能用 `capture_ok` 接**：`capture` 是 `2>&1` 合併的，而
#   `fulcrum compile` 會把「已裝載 N 個 SNI 的證書」這類話寫到 **stderr**
#   ⇒ 拿到的是「一行中文 + JSON」，`json.load` 當場炸。★ 實測過一次。
#   ⇒ 要解析的輸出一律**分流到檔案**，只讓判據去接 python 那一步。
if ! "$BIN" compile "$WORK/gen3.Fulcrumfile" > "$WORK/gen3.json" 2> "$WORK/gen3.err"; then
  fail "compile 第三代失败：$(head -3 "$WORK/gen3.err" | tr '
' ' ')"
fi
capture_ok "第三代的端口集" python3 -c '
import json, sys
cfg = json.load(open(sys.argv[1], encoding="utf-8"))
print(" ".join(str(p) for p in sorted({a["port"] for s in cfg["sites"] for a in s["addresses"]})))
' "$WORK/gen3.json"
GEN3_PORTS="$CAPTURE_OUT"
eq "★★★ 第三代的端口集里没有 80 —— 本场景不碰那个共享端口（守着上面那行 auto_http_redirect false）" \
  "$T_PORT" "$GEN3_PORTS"

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

# ★ 逐項斷到，⛔ 不只斷條數（表頭 .len() 在只繪製第一項時仍然是對的）。
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
