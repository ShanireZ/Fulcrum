#!/usr/bin/env bash
# Prometheus 指標端到端（**M2 批 M**；G116–G121，裁決 R1–R8）。
#
# ★ ★ 為什麼它是**獨立一格**：與 `tests/log/` 同一個理由 —— 它驗的不是「回應對不對」，
#   而是**回應之外的那份計數**。它有自己的格式契約（text exposition 0.0.4）、
#   自己的訪問控制（`metrics` 是站台塊裡的終結指令，序號 75）、自己的基數上界。
#   ⚠ 混進 `tests/serve/run.sh` 的話，一條指標斷言紅了，讀紀錄的人會先去看路由。
#
# ★ ★ ★ **判據掛在「誰在數、數出來的和別處對不對得上」，不掛在「抓到了東西」**：
#   一個把整份指標硬編碼成字串的實作，能通過每一條只問「正文裡有沒有那幾個字」的判據。
#   ⇒ 本格的重量全在**增量**與**交叉對照**上：
#     · 增量：抓兩次做差（⚠ 計數發生在 `Record::finish`，即**回應寫完之後**
#       ⇒ 第 N 次抓取看到的是前 N−1 次的量）；
#     · 交叉對照：同一批請求，指標那一側的增量必須與**訪問紀錄**那一側的新增行數對得上。
#
# 連接埠（★ 與其餘場景全都錯開，見 AGENTS.md 那張連接埠表）：
#   9920 站台 A —— **兩條位址** `a.example` / `a2.example`（★ G121 的正面判據要第二條）；
#        配 `log { output file … level info }`（一致性門要它記全部）；
#        `@internal { remote_ip 127.0.0.0/8 · path /metrics }` + `handle @internal { metrics }`
#        + 備援 `respond 403`。
#   9921 站台 B —— `b.example`，**整塊裡一個 `metrics` 都沒有**（反向判據 ① 要它）。
#   9921 站台 C —— `c.example`，`@outsider remote_ip 10.0.0.0/8` + `handle @outsider { metrics }`
#        + 備援 `respond 403`。★ 我們打不進 10.0.0.0/8 ⇒ 拿到的是 403 而不是指標
#        （訪問控制的**反向**那一半，在容器裡真的造得出來的形狀）。
#   9921 站台 W —— `*.wild.example`，**一條通配位址**（G121 的摺疊判據要它，判據 6）。
#   9922 站台 T —— `a.example:9922` + `tls`（**M2 批 M′ 任務 3**，`fulcrum_tls_requests_total`）。
#        ★ G110 讓有 TLS 的連接埠在**同一個連接埠號**上自動聽 UDP ⇒ h1 / h2 / h3 三條路
#          都在這一個連接埠上量，不另起服務。
#   9923 **不是樞衡的監聽器**，是一個 python 慢上游（**M2 批 M′ 任務 4**，G124）：
#        睡 1.5s 才回話，而且**有意不帶 `Content-Length`**。理由見本檔案末尾那一節。
#
# ⚠ ⚠ ★ **站台 W 補的是夾具形狀上的洞，不是多一條覆蓋。** 另外四條位址全是**精確**
#   字面值 ⇒ 對每一條命中站台的請求，`ctx.host` 與命中的那條位址**逐字相等**。
#   於是一個把 `site` 標籤取成 `ctx.host` 的實作（G121 明令禁止的正是這一個）
#   在本格別的每一條判據上都是綠的 —— 連那條「取值閉集」也綠，因為兩者給出同一個集合。
#   ★ ★ **盲區在夾具裡，而判據的條數說明不了這件事**（AGENTS.md「Gate discipline」第一條）。
#
# ⚠ ⚠ **`@internal` 裡那條 `path /metrics` 是有意加的，不是從 brief 抄漏的**：
#   只寫 `remote_ip 127.0.0.0/8` 的話，本格所有請求都來自 127.0.0.1 ⇒ 站台 A 上
#   **任何路徑都會命中 `metrics`**，備援的 `respond 403` 一次也走不到 ——
#   而一致性門要求同一個站台上至少出現 `metrics` 與 `respond` 兩種 `outcome`。
#   ★ 順帶：它讓「站台 B 上打**同一路徑**」這句話有了確定的所指。
#
# ⚠ ⚠ **本場景仍然不碰共享的 `:80`，而現在它需要一句顯式的話才成立。**
#   前四個站台全寫 `http://` ⇒ 天然不合成重定向站台；而站台 T 寫的是
#   `a.example:9922`（沒寫 scheme ⇒ 按 `compile.rs` 的規則推成 `https` ⇒ `auto_https`）
#   ——那本來**會**讓 `synthesize_http_redirect` 給它合成一個 `:80` 的 308 站台，
#   於是本格變成 AGENTS.md 那張表上第八個隱式綁 `:80` 的場景。
#   ⇒ 三份設定的全域塊都寫 `auto_http_redirect false` 把它按住。
#   ★ 那一行今天**不改變任何既有判據**（前四個站台本來就不合成）——它守的正是
#     站台 T 這一條，⛔ 別當成可以順手刪掉的樣板。
#   ★ cleanup 照舊斷言 9920/9921/9922 都已經還回去
#   （照 `tests/quic-relay/run.sh`：判據掛在「連接埠還回去了沒有」，不掛在「處理程序還在不在」）。

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
A_PORT=${A_PORT:-9920}
# ★ 站台 B 與站台 C **共用**這一個連接埠：站台按 `(host, port)` 匹配，同連接埠不同主機名
#   是合法的兩個站台。⇒ 反向判據 ① 與訪問控制的反向半邊在同一個監聽器上量，
#   「監聽器不一樣」這條解釋被排除掉了。
BC_PORT=${BC_PORT:-9921}
# 站台 T：TLS 那一格（M2 批 M′ 任務 3）。★ 同一個連接埠號上同時有 TCP 與 UDP（G110）
#   ⇒ h1 / h2 / h3 三條路共用它。
TLS_PORT=${TLS_PORT:-9922}
# status_class="none" 那條路（M2 批 M′ 任務 4，G124）要的**慢上游**。
# ⚠ 它不是樞衡的監聽器，但它照樣佔著一個連接埠 ⇒ 進 AGENTS.md 指的那張連接埠表，
#   也進 cleanup 那份「走的時候還回去了沒有」的單子。
ABORT_UP_PORT=${ABORT_UP_PORT:-9923}
# ── 連接指標（M2 批 O，G122 的連接那半）用的 L4 兩格 ────────────────────────
# ★ ★ 為什麼 l4 也要擺進本場景：這個族的核心斷言是「**四個互不相干的 accept 循環
#   都記進了同一個族，而 entrypoint 分得開**」—— 那句話只有在同一個處理程序裡同時有
#   五種入口（http / admin / quic / l4_tcp / l4_udp）時才驗得了。拆到 tests/l4 去，
#   它就永遠驗不到。
# ★ L4 TCP 的上游重複使用 $ABORT_UP_PORT（那個慢上游是個 TCP 監聽器，透傳夠用）；
#   UDP 另起一個回顯上游。
L4T_PORT=${L4T_PORT:-9924}
L4U_PORT=${L4U_PORT:-9925}
UDPUP_PORT=${UDPUP_PORT:-9926}
LOGFILE="$WORK/access.json"
EXPO="$WORK/expo.py"
# 抓取路徑。★ 它同時是「站台 B 上打的同一路徑」。
MPATH=/metrics
# ── fulcrum_overrides_active（M2 批 N 任務 6.5，G126）用：管理面 Unix socket ──
#   ★ 不需要新連接埠（G14：admin 只走 unix socket）——不佔用 AGENTS.md 那張連接埠表。
ADMIN_SOCK="$WORK/admin.sock"

FAILS=0
PIDS=()

fail() {
  echo "  ✗ $*" >&2
  FAILS=$((FAILS + 1))
}
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid waited
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

  # ── ★ ★ 收尾自證：本場景用過的連接埠，走的時候必須全部還回去 ──────────────────
  #
  # 照 `tests/quic-relay/run.sh` 那段。⚠ 它守的是**上面那個 `PIDS` 收全了沒有**，
  # 而判據掛在「連接埠還回去了沒有」而不是「處理程序還在不在」：前者才是下一個場景真會
  # 被絆到的東西，後者要先知道該找哪個 pid —— 而一個沒被登記的 pid 恰恰最找不到。
  # ⚠ 本格不碰 `:80`：前四個站台都是 `http://`，而 `auto_https` 的站台 T 由全域
  #   `auto_http_redirect false` 按住（見檔案頭）⇒ 單子裡沒有它。
  local p leaked=""
  for p in "$A_PORT" "$BC_PORT" "$TLS_PORT" "$ABORT_UP_PORT" "$L4T_PORT" "$L4U_PORT" "$UDPUP_PORT"; do
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
    echo "METRICS TESTS FAILED: 收尾没干净 —— 退出时这些端口还有人在听：$leaked" >&2
    echo "  ⇒ 多半是某个 pid 没进 \$PIDS（经典写法：\`X=\$(start …)\`，\$(…) 是子 shell，" >&2
    echo "     数组改的是副本）。⚠ 后果不在本场景：泄漏的进程活到下一个场景去。" >&2
    # ★ 用 `pgrep -af` 而不是 `ps | grep`：一個 defunct 處理程序攥不住監聽 socket，
    #   所以這裡要的只是「還活著的是哪幾個」——而那正好是 pgrep 答得了的。
    pgrep -af "fulcrum serve" >&2 || true
    exit 1
  fi
}
trap cleanup EXIT

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

# 訪問紀錄那一側的兩個讀數。★ 與 `tests/log/run.sh` 逐字同一套寫法。
lines() { wc -l < "$LOGFILE" | tr -d ' '; }

# 取訪問紀錄**最後一行**的某個欄位。⚠ 用 python3 而不是 grep：一個
# `grep '"site":"http://a.example:9920"'` 會在欄位順序、空格、轉義任一處變化時靜默漏判。
field() {
  python3 -c '
import json, sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
o = json.loads(line)
k = sys.argv[2]
print(o[k] if k in o else "<缺>")
' "$LOGFILE" "$1"
}

# 一條普通請求，只回 HTTP 碼。$1 連接埠 · $2 Host · $3 路徑。
req() {
  curl -sS -o /dev/null -w '%{http_code}' --max-time 5 -H "Host: $2" \
    "http://$HOST:$1$3" 2>/dev/null || echo 000
}

# 一條請求，正文落到 $4、回應頭落到 $4.hdr，回 HTTP 碼。
get() {
  curl -sS -o "$4" -D "$4.hdr" -w '%{http_code}' --max-time 5 -H "Host: $2" \
    "http://$HOST:$1$3" 2>/dev/null || echo 000
}

# 抓一次指標（Host: a.example，來源自然是 127.0.0.1 ⇒ `@internal` 命中）。
scrape() { get "$A_PORT" a.example "$MPATH" "$1"; }

# 回應頭裡 `Content-Type` 的**原文**。★ 不走 curl 的 `%{content_type}`：那是 curl
# 規整過的，而這一格判的恰恰是**我們寫出去的那一串逐字對不對**（抓取端按它挑解析器）。
content_type() {
  python3 - "$1" <<'PY'
import sys

val = ""
with open(sys.argv[1], "r", encoding="utf-8", errors="replace") as f:
    for line in f:
        k, sep, v = line.partition(":")
        if sep and k.strip().lower() == "content-type":
            val = v.strip()
print(val)
PY
}

expo() { python3 "$EXPO" "$@"; }

# ★ ★ ★ 「捕獲一個可能失敗的命令」的唯一寫法，連同它為什麼存在，都在
#   `tests/lib/capture.sh` 裡（任務 7 從本檔案與 `tests/serve/run.sh` 各一份收斂而來）。
# shellcheck source=tests/lib/capture.sh
. "$REPO/tests/lib/capture.sh"

# 一個數值斷言。$1 說明 · $2 期望 · $3 實際。
eq() {
  if [ "$2" = "$3" ]; then
    ok "$1（$3）"
  else
    fail "$1：期望「$2」实际「$3」"
  fi
}

# ── exposition 的讀取端 ─────────────────────────────────────────────────────
#
# ★ ★ **判據要問的是「這個 series 的值漲了多少」，而不是「正文裡有沒有那幾個字」**：
#   後者對「把指標硬編碼成一段常數」的實作完全無效。⇒ 需要一個真的解析器。
# ⚠ 它自己也可能壞。壞了的表現是**恆答 0 / 恆答有**，而那會讓下面每一條判據變成空轉
#   ⇒ 它帶一個 `selftest`，每次開跑先證「它命中得了，也落空得了」。
cat > "$EXPO" <<'PY'
#!/usr/bin/env python3
"""Prometheus text exposition（0.0.4）的读取端 —— 只服务于 tests/metrics/run.sh。

★ 只做一件事：把正文解析成 `(族名, 标签表, 值)` 的清单，然后按标签筛。
⚠ 读不懂的行**当场抛**，不跳过 —— 「跳过读不懂的」等于让一份撕坏的 exposition
  在每一条判据上都表现得像一份好的。
"""

import re
import sys

# `name{k="v",…} value` 或 `name value`。
# ⚠ 标签段用贪婪 `.*` 再回退到最后一个 `}`：标签**值**里允许出现 `}`，
#   写成 `[^}]*` 的话那种行会被判成「读不懂」——一个只在少见输入上发作的假红。
_SAMPLE = re.compile(
    r"^(?P<name>[a-zA-Z_:][a-zA-Z0-9_:]*)"
    r"(?:\{(?P<labels>.*)\})?"
    r"[ \t]+(?P<value>[^ \t]+)[ \t]*$"
)
_LABEL = re.compile(r'([a-zA-Z_][a-zA-Z0-9_]*)="((?:[^"\\]|\\.)*)"')
_UNESC = {"n": "\n", "\\": "\\", '"': '"'}
# 直方图的三种派生名。★ 它们的族是**去掉后缀**那个名字。
_HIST_SUFFIX = ("_bucket", "_sum", "_count")


def unescape(s):
    return re.sub(r"\\(.)", lambda m: _UNESC.get(m.group(1), m.group(1)), s)


def parse(text):
    """→ (samples, meta)。

    samples = [(名字, {标签: 值}, 值字符串)]
    meta    = {族名: {"HELP": …, "TYPE": …}}
    """
    samples = []
    meta = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith("#"):
            parts = line.split(None, 3)
            if len(parts) >= 3 and parts[1] in ("HELP", "TYPE"):
                meta.setdefault(parts[2], {})[parts[1]] = parts[3] if len(parts) > 3 else ""
            continue
        m = _SAMPLE.match(line)
        if not m:
            raise SystemExit("exposition 里有一行读不懂：%r" % raw)
        labels = {}
        if m.group("labels"):
            for k, v in _LABEL.findall(m.group("labels")):
                labels[k] = unescape(v)
        samples.append((m.group("name"), labels, m.group("value")))
    return samples, meta


def read(path):
    with open(path, "r", encoding="utf-8") as f:
        return parse(f.read())


def family_of(name, meta):
    """一条样本属于哪个族。★ 直方图的 `_bucket`/`_sum`/`_count` 归到本族名下。"""
    if name in meta:
        return name
    for suf in _HIST_SUFFIX:
        if name.endswith(suf):
            base = name[: -len(suf)]
            if meta.get(base, {}).get("TYPE") == "histogram":
                return base
    return None


def matched(samples, family, filters):
    want = dict(kv.split("=", 1) for kv in filters)
    out = []
    for name, labels, value in samples:
        if name != family:
            continue
        if all(labels.get(k) == v for k, v in want.items()):
            out.append((labels, value))
    return out


def main(argv):
    cmd = argv[1]

    if cmd == "selftest":
        return selftest()

    if cmd == "lint":
        samples, meta = read(argv[2])
        bad = []
        for fam, kinds in sorted(meta.items()):
            for need in ("HELP", "TYPE"):
                if need not in kinds:
                    bad.append("族 %s 缺 # %s" % (fam, need))
            if kinds.get("TYPE") == "counter" and not fam.endswith("_total"):
                bad.append("counter 族 %s 的名字不以 _total 收尾" % fam)
        seen = set()
        for name, labels, _ in samples:
            if family_of(name, meta) is None:
                bad.append("样本 %s 没有对应的 # HELP / # TYPE 声明" % name)
            key = (name, tuple(sorted(labels.items())))
            if key in seen:
                bad.append("同一条 series 出现了两次：%s%s" % (name, labels))
            seen.add(key)
        for line in bad:
            print(line)
        return 1 if bad else 0

    if cmd == "meta":
        _, meta = read(argv[2])
        kinds = meta.get(argv[3], {})
        return 0 if ("HELP" in kinds and "TYPE" in kinds) else 1

    samples, _ = read(argv[2])
    family = argv[3]
    filters = argv[4:]

    if cmd == "series":
        print(len(matched(samples, family, filters)))
        return 0
    if cmd == "sum":
        total = 0.0
        for _, value in matched(samples, family, filters):
            total += float(value)
        # ★ 全是计数器/量表的整数值 ⇒ 打成整数，免得判据去比 "50" 和 "50.0"。
        print(int(total) if total == int(total) else total)
        return 0
    if cmd == "labelkeys":
        keys = set()
        for labels, _ in matched(samples, family, filters):
            keys.add(",".join(sorted(labels)))
        for k in sorted(keys):
            print(k)
        return 0
    if cmd == "labelvalues":
        # $4 = 族，$5 = 标签名 ⇒ 打出该标签**出现过的全部取值**，排序去重。
        vals = set()
        for name, labels, _ in samples:
            if name == family and argv[4] in labels:
                vals.add(labels[argv[4]])
        for v in sorted(vals):
            print(v)
        return 0

    raise SystemExit("不认识的子命令：%s" % cmd)


# ── ★ ★ 自证：它命中得了，也落空得了 ────────────────────────────────────────
#
# ⚠ 一个恒答 0 的 `sum` 会让「涨了 50」变成「0 == 0 + 50」当场红 —— 那还算好的；
#   而一个恒答「有」的 `meta` 会让格式那几条**永远绿**，那才是这里真正要挡的形状。
_FIXTURE = """\
# HELP t_requests_total 请求数。
# TYPE t_requests_total counter
t_requests_total{site="a.example",outcome="metrics"} 3
t_requests_total{site="a.example",outcome="respond"} 4
t_requests_total{site="<none>",outcome="no_site_match"} 5
# HELP t_empty_total 一次都没发生过。
# TYPE t_empty_total counter
# HELP t_latency_seconds 时延，秒。
# TYPE t_latency_seconds histogram
t_latency_seconds_bucket{le="+Inf"} 12
t_latency_seconds_sum 1.5
t_latency_seconds_count 12
"""


def _check(cond, what):
    if not cond:
        print("★ expo.py 自证未过：%s" % what)
        return 1
    return 0


def selftest():
    rc = 0
    samples, meta = parse(_FIXTURE)

    # 命中。
    rc |= _check(len(matched(samples, "t_requests_total", [])) == 3, "三条 series 数不对")
    rc |= _check(
        sum(float(v) for _, v in matched(samples, "t_requests_total", ['site=a.example'])) == 7.0,
        "按 site 筛出来的和不对",
    )
    rc |= _check(
        len(matched(samples, "t_requests_total", ['site=a.example'])) == 2, "按 site 筛出来的条数不对"
    )
    # 落空 —— ⚠ 这几条才是「它会不会恒答有」的判据。
    rc |= _check(len(matched(samples, "t_empty_total", [])) == 0, "空族竟然筛出了样本")
    rc |= _check(
        len(matched(samples, "t_requests_total", ["site=不存在"])) == 0, "不存在的标签值竟然命中了"
    )
    rc |= _check("t_empty_total" in meta, "空族的 HELP/TYPE 没被读到")
    rc |= _check("t_不存在_total" not in meta, "不存在的族竟然读出了声明")
    # 直方图的派生名归到本族。
    rc |= _check(family_of("t_latency_seconds_bucket", meta) == "t_latency_seconds", "直方图后缀没归族")
    rc |= _check(family_of("t_野生_total", meta) is None, "没声明过的样本名竟然归到了某个族")

    # 读不懂的行必须**抛**，不许静默跳过。
    try:
        parse("# TYPE t_x counter\nt_x 这不是一行合法样本 还多一段\n")
        rc |= _check(False, "撕坏的 exposition 竟然解析通过了")
    except SystemExit:
        pass

    if rc == 0:
        print("expo.py 自证通过（命中与落空各证一遍）")
    return rc


if __name__ == "__main__":
    sys.exit(main(sys.argv))
PY

# ── [0/8] 基線 ──────────────────────────────────────────────────────────────
#
# ★ ★ 三條自證：連接埠沒被佔（否則量的是別人的服務）；訪問紀錄此刻不存在
#   （否則「多了幾行」分不出「剛寫的」與「本來就有的」）；讀取端量得了東西。
echo "=== [0/8] 基线：端口空着 · 日志文件还不存在 · exposition 读取端自证 ==="
for p in "$A_PORT" "$BC_PORT" "$TLS_PORT" "$L4T_PORT" "$ABORT_UP_PORT" "$UDPUP_PORT"; do
  if port_listening "$p"; then
    echo "METRICS TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
# ⛔ 不在這句話裡寫個數：那個計數一道門都沒有，加一個連接埠時它當場過期而不會紅。
# ⚠ $L4U_PORT 與 $UDPUP_PORT 是 UDP，`port_listening` 是 TCP 探測 ⇒ 探不到它們，
#   基線裡有意不查（查了也恆為「空著」，那是一條讀起來成立而實際空轉的斷言）。
ok "本格用到的 TCP 端口都空着（$A_PORT $BC_PORT $TLS_PORT $L4T_PORT $ABORT_UP_PORT $UDPUP_PORT）"
if [ -e "$LOGFILE" ]; then
  echo "METRICS TESTS FAILED: $LOGFILE 已经存在，本次结果不可采信。" >&2
  exit 1
fi
ok "访问日志还不存在（「多了几行」才说得清）"
if expo selftest; then
  ok "★★ exposition 读取端自证通过 —— 下面每一条增量判据才有意义"
else
  echo "METRICS TESTS FAILED: expo.py 自证未过，本次结果一律不可采信。" >&2
  exit 1
fi

# ── 設定 ────────────────────────────────────────────────────────────────────
#
# ⚠ ⚠ **`admin unix/$ADMIN_SOCK` 從第一代就開著**（G126 那節要用它 POST /runtime
#   與 GET /stats）：admin 是啟動時才綁的監聽（與資料面監聽器同一條紀律），
#   POST /load 換不了它 —— 必須在這裡，不能等後面 [reload] 再加。
#   ★ 它是一個獨立的 Unix socket 監聽，不影響這份設定上任何一條既有判據
#   （不產生新的 `site`、不產生新的 `reverse_proxy`）。

# ── 自簽證書（站台 T 要，M2 批 M′ 任務 3）─────────────────────────────────
#
# ⚠ SAN 必須有 `a.example`：樞衡按**證書自己的 SAN** 決定這張證書裝在哪些 SNI 上。
# ★ 與 `tests/log/run.sh` 那段逐字同一套（現籤，⛔ 不入庫）。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" \
  -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "METRICS TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ⚠ ⚠ ⚠ **站台 T 必須出現在下面**三份**設定裡**（`a` / `ov` / `ov-dangling`）。
#   `POST /load` 明確拒絕監聽連接埠集發生變化（`admin.rs`：「監聽連接埠集變了，本處理程序
#   換不了」，回 409）⇒ 少寫一份，後面那兩次 reload 會 409，**而紅的地方會在
#   `fulcrum_overrides_active` 那一節**，指向一個與真正原因完全無關的方向。
cat > "$WORK/a.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
    auto_http_redirect false
}

http://a.example:$A_PORT, http://a2.example:$A_PORT {
    log {
        output file $LOGFILE
        level info
    }
    @internal {
        remote_ip 127.0.0.0/8
        path $MPATH
    }
    handle @internal {
        metrics
    }
    respond 403
}

http://b.example:$BC_PORT {
    respond 200 "b-ok"
}

http://c.example:$BC_PORT {
    @outsider remote_ip 10.0.0.0/8
    handle @outsider {
        metrics
    }
    respond 403
}

http://*.wild.example:$BC_PORT {
    respond 200 "wild-ok"
}

# ★ ★ 站点 T：TLS 那一格。⚠ 有意**不写** metrics、不写 matcher ——
#   它只负责「让请求真的走过一次 TLS」，抓取仍旧从站点 A 走明文出口。
#   ⇒ 「抓取自己不计进 tls_requests_total」这件事因此是**结构上**成立的，不靠记性。
a.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    log {
        output file $LOGFILE
        level info
    }
    respond 200 "t-ok"
}

# ★ ★ L4 两格（**M2 批 O**）：连接族要在同一个进程里同时看到五种入口。
# ⚠ ⚠ 它必须出现在**三份**配置里 —— 「POST /load」按 (协议, 监听地址原样) 比 L4
#   监听器集，少写一份就是 409，而红会落在 overrides 那一节、指向完全错误的方向。
# ⚠ 这个 heredoc 不带引号（要展开端口变量）⇒ 注释里一律用「」，别用反引号。
l4 {
    tcp :$L4T_PORT {
        proxy 127.0.0.1:$ABORT_UP_PORT
    }
    udp :$L4U_PORT {
        proxy 127.0.0.1:$UDPUP_PORT
    }
}
CONF

# 一份**沒有任何來源匹配器**的 metrics 設定 —— 只用來證那條診斷真的會說話。
cat > "$WORK/unguarded.Fulcrumfile" <<CONF
http://a.example:$A_PORT {
    metrics
}
CONF

# ── [1/8] 起服務 ────────────────────────────────────────────────────────────
echo "=== [1/8] 起被测实例 ==="
RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$WORK/a.Fulcrumfile" \
  --bind-host "$HOST" \
  --pid-file "$WORK/a.pid" \
  --upgrade-sock "$WORK/a.sock" \
  > "$WORK/a.log" 2>&1 &
PIDS+=($!)

for p in "$A_PORT" "$BC_PORT" "$TLS_PORT" "$L4T_PORT"; do
  wait_port "$p" || {
    echo "METRICS TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "TCP 监听都起来了（$A_PORT $BC_PORT $TLS_PORT $L4T_PORT）"

if [ -f "$LOGFILE" ] && [ "$(lines)" = "0" ]; then
  ok "访问日志装载时就开好了，此刻 0 行（一致性门的基线）"
else
  fail "访问日志此刻不是「存在且空」—— 一致性门的基线不成立"
fi

# ★ 那條診斷的**兩個方向**：本格這份設定圈住了來源 ⇒ 不該報；
#   而一份沒圈的設定**必須**報。⚠ 只驗前者的話，一條從來不會說話的診斷也全綠 ——
#   而 G116 明寫「matcher 寫錯就會把指標暴露出去，只能靠文件與診斷兜」。
if "$BIN" validate "$WORK/a.Fulcrumfile" > "$WORK/validate.out" 2>&1; then
  if grep -q "FUL-DSL-0037" "$WORK/validate.out"; then
    fail "★★ 本格的 metrics 被 remote_ip 圈住了，却仍报 FUL-DSL-0037"
  else
    ok "★★ 圈住了来源的 metrics：没有 FUL-DSL-0037"
  fi
else
  fail "被测配置连 validate 都没过：$(head -5 "$WORK/validate.out" | tr '\n' ' ')"
fi
"$BIN" validate "$WORK/unguarded.Fulcrumfile" > "$WORK/unguarded.out" 2>&1 || true
if grep -q "FUL-DSL-0037" "$WORK/unguarded.out"; then
  ok "★★★ 而没圈来源的 metrics ⇒ FUL-DSL-0037（这条诊断真的会说话）"
else
  fail "★★★ 没圈来源的 metrics 竟然一声不吭：$(head -5 "$WORK/unguarded.out" | tr '\n' ' ')"
fi

# ── [2/8] 判據 1：格式 ──────────────────────────────────────────────────────
echo "=== [2/8] 判据 1：抓到的正文是合法 exposition ==="
#
# ⚠ ⚠ **先打三條預熱請求再抓**：計數發生在 `Record::finish`（回應寫完之後）
#   ⇒ 第一次抓取的正文裡 `fulcrum_requests_total` **一條樣本都沒有**，
#   而「四個標籤都在」這條判據在沒有樣本時會退化成空轉。
CODE=$(req "$A_PORT" a.example "/")
eq "预热①：站点 A 上非 /metrics 的路径走兜底 respond" 403 "$CODE"
CODE=$(req "$A_PORT" nobody-warm.invalid "$MPATH")
eq "预热②：9920 上的未知 Host ⇒ 无站点匹配" 421 "$CODE"
# ★ 預熱③是**已知位址字面值、錯連接埠**：`b.example` 只配在 9921 上 ⇒ 9920 上它無站台匹配，
#   而它**是**一條位址字面值 ⇒ G118 給它自己一格，而不是並進 `<other>`。
#   ⚠ 少了它，判據 4 裡「series 條數不增長」在一個「把 host 恆寫成 <other>」的實作上也全綠。
CODE=$(req "$A_PORT" b.example "$MPATH")
eq "预热③：已知字面量打错端口 ⇒ 无站点匹配" 421 "$CODE"
sleep 0.2

S1="$WORK/s1.txt"
CODE=$(scrape "$S1")
eq "抓取端点回 200" 200 "$CODE"

CT=$(content_type "$S1.hdr")
eq "★★ Content-Type 逐字" "text/plain; version=0.0.4; charset=utf-8" "$CT"

# 每個族的 `# HELP` / `# TYPE` 一個都不許少。
#
# ⛔ **這裡不寫族的個數**：一個寫在註解裡的計數沒有任何門守著，加族時它當場過期
#   而不會紅（批 N 任務 8 因此把這類計數從五個檔案裡全部拿掉了）。
#   ★ 下面那條 `# TYPE` 行數比對**就是**那個計數，而它是從真實抓取裡數出來的。
FAMILIES_EXPECTED="fulcrum_requests_total fulcrum_request_duration_seconds
fulcrum_cache_events_total fulcrum_cache_purged_entries_total
fulcrum_no_site_match_total fulcrum_upstream_inflight fulcrum_upstream_healthy
fulcrum_cert_expiry_seconds fulcrum_acme_issue_total fulcrum_build_info
fulcrum_overrides_active fulcrum_tls_requests_total
fulcrum_connections_total fulcrum_connections_active
fulcrum_upstream_passive_open"
for f in $FAMILIES_EXPECTED; do
  if expo meta "$S1" "$f"; then
    ok "族 $f 的 HELP/TYPE 都在"
  else
    fail "族 $f 缺 HELP 或 TYPE"
  fi
done

# ★ ★ ★ **反向那半：抓取裡不許有這張單子之外的族。**
#
# ⚠ ⚠ 少了它，上面那個循環只證明「單子裡的都在」—— 新增一個族卻忘了寫進單子時
#   **一條都不會紅**，而這個檔案正是那種遺漏唯一會露頭的地方
#   （`FAMILIES` ↔ 基數表那道門在單測裡，它看不見這份手抄單子）。
# ★ 判據是**數出來的**：`# TYPE` 一族一行，拿它與單子的條數比。
TYPE_LINES=$(grep -c '^# TYPE ' "$S1")
WANT_FAMILIES=$(echo "$FAMILIES_EXPECTED" | wc -w)
eq "★★★ 抓取里的族数与上面这张单子逐个对得上（多一个族没写进单子就红）" \
  "$WANT_FAMILIES" "$TYPE_LINES"

# ★ 整份正文過一遍結構檢查：每條樣本都有宣告 · 沒有重複的 series · counter 名字帶 `_total`。
#   ⚠ 「重複的 series」這一條尤其值錢：抓取端會把它算成兩條，而正文讀起來完全正常。
LINT=$(expo lint "$S1" || true)
if [ -z "$LINT" ]; then
  ok "★★ 整份 exposition 结构检查通过（声明齐全 · 无重复 series · counter 名字带 _total）"
else
  fail "★★ exposition 结构检查没过：$(echo "$LINT" | tr '\n' '；')"
fi

# `fulcrum_requests_total` 的**每一條**樣本都必須正好帶那四個標籤。
# ★ 判的是**鍵的集合逐字相等**，不是「含有」——「含有」在多出一個 `uri` 標籤時照樣綠，
#   而「任何形態都不加 uri 標籤」是 G116 那張基數表寫死的一條。
KEYS=$(expo labelkeys "$S1" fulcrum_requests_total)
eq "★★★ fulcrum_requests_total 的标签键集合（每条样本都一样）" \
  "outcome,proto,site,status_class" "$KEYS"

# ★ ★ 上游那兩族：**族在、樣本無**。
#   本設定裡一個 `reverse_proxy` 都沒有 ⇒ 它們沒有任何資料源。
#   ⚠ 這一條分得開「沒接上」與「沒資料」：族都不出現才是沒接上，而那正是
#   「一個指標存在、只是還沒發生過」與「這個指標根本沒做」之間唯一的區別。
for f in fulcrum_upstream_inflight fulcrum_upstream_healthy; do
  eq "★★★ $f 有 HELP/TYPE 但零条样本（没有上游 ⇒ 没数据，不是没接上）" \
    0 "$(expo series "$S1" "$f")"
done

# ★ ★ ★ fulcrum_overrides_active（G126，批 N 任務 6.5）：與上面兩族**相反**——
#   它是**無標籤**的單值 gauge，即便這一刻登記處一項覆蓋都沒有，也必須
#   **出一條樣本**（值是 0），而不是跟著「沒資料」一起整族消失。⚠ 這是判據
#   寫法紀律的第 4 條：0 與「不存在」在這個族身上必須能分得開——族仍然出現，
#   只是恰好一項覆蓋都沒有。
#   ⚠ ⚠ 先證「樣本真的存在」再問「值是多少」：`expo sum` 在「一條樣本都沒有」
#   與「樣本存在、值恰好是 0」兩種情況下算出的都是 0（沒有匹配項時 `sum` 的
#   初值就是 0）——單獨看 sum 擋不住「count==0 就整族不出樣本」這種實作。
eq "★★★ fulcrum_overrides_active 基数恒为 1（判据 5，0 覆盖时的基线；顺带证明判据 4 的样本真的存在）" \
  1 "$(expo series "$S1" fulcrum_overrides_active)"
eq "★★★ fulcrum_overrides_active 此刻 0 覆盖，值是 0（判据 4：族仍出现，不是整族消失）" \
  0 "$(expo sum "$S1" fulcrum_overrides_active)"
eq "★★★ fulcrum_overrides_active 不带任何标签（判据 1）" \
  "" "$(expo labelkeys "$S1" fulcrum_overrides_active)"

# ── [3/8] 判據 2：訪問控制的兩個方向 ────────────────────────────────────────
echo "=== [3/8] 判据 2：够得着的抓得到，够不着的拿到 403 ==="
if grep -q '^# TYPE ' "$S1"; then
  ok "正向：127.0.0.1 打站点 A ⇒ 200 且正文真的是指标（有 # TYPE 行）"
else
  fail "正向：抓到的正文里没有 # TYPE 行 —— 那它不是一份 exposition"
fi

# 反向：站台 C 把 `metrics` 圈在 10.0.0.0/8 裡，而我們從 127.0.0.1 打過去 ——
# ⇒ `@outsider` 不命中，落到備援那條 `respond 403`。
# ★ 這是本格里**在容器裡真的造得出來**的那個反向形狀：不需要第二塊網卡，
#   只需要一個我們不可能來自的網段。
C_OUT="$WORK/c.txt"
CODE=$(get "$BC_PORT" c.example "$MPATH" "$C_OUT")
eq "★★★ 反向：来源不在 10.0.0.0/8 ⇒ 站点 C 给的是兜底 403" 403 "$CODE"
if grep -q '^# TYPE ' "$C_OUT"; then
  fail "★★★ 403 的正文里竟然有 # TYPE —— 指标从一个够不着的匹配器后面漏出来了"
else
  ok "★★★ 而那份 403 的正文里一个 # TYPE 都没有"
fi
CT=$(content_type "$C_OUT.hdr")
if [ "$CT" = "text/plain; version=0.0.4; charset=utf-8" ]; then
  fail "★★ 403 的 Content-Type 竟然是 exposition 那一串"
else
  ok "★★ 而它的 Content-Type 也不是 exposition 那一串（$CT）"
fi

# ── [4/8] 判據 3（反向 ①）：沒寫 metrics 的站台 ─────────────────────────────
echo "=== [4/8] 判据 3：站点 B 没写 metrics ⇒ 同一路径拿到的不是指标 ==="
#
# ★ ★ ★ 少了這一條，「抓到了指標」證明不了是 `metrics` 這條指令乾的 ——
#   一個「見到 /metrics 就繪製指標」的實作（把端點做成了硬編碼路徑）在上面每一條判據上全綠。
B_OUT="$WORK/b.txt"
CODE=$(get "$BC_PORT" b.example "$MPATH" "$B_OUT")
eq "站点 B 上打 $MPATH 回 200" 200 "$CODE"
if grep -q '^# TYPE ' "$B_OUT"; then
  fail "★★★ 没写 metrics 的站点上竟然抓到了指标 —— 那个端点不是这条指令给的"
else
  ok "★★★ 没写 metrics 的站点：同一路径的正文里一个 # TYPE 都没有"
fi
eq "★★ 而它给的是自己那条 respond 的正文" "b-ok" "$(cat "$B_OUT")"

# ── [5/8] 判據 4（反向 ②）：50 個互不相同的未知 Host ────────────────────────
echo "=== [5/8] 判据 4：未知 Host 打 50 次 ⇒ series 不增长，而那一格正好 +50 ==="
#
# ⚠ ⚠ **兩句都要斷言**。只斷言前半（「series 條數不增長」）的話，
#   一個「計數器根本沒在加」的實作也能過 —— 而那時 series 條數當然不增長。
S2="$WORK/s2.txt"
CODE=$(scrape "$S2")
eq "抓取回 200（取基线）" 200 "$CODE"
N1=$(expo series "$S2" fulcrum_no_site_match_total)
V1=$(expo sum "$S2" fulcrum_no_site_match_total 'host=<other>')
W1=$(expo sum "$S2" fulcrum_no_site_match_total host=b.example)
# ★ 尺子自證：兩格都得有值 —— 一個把 host 恆寫成 `<other>` 的實作在這裡當場紅，
#   而它在「series 不增長」那一句上是全綠的。
if [ "$V1" -ge 1 ] && [ "$W1" -ge 1 ]; then
  ok "★★★ 基线上两格都有值：host=<other> 是 $V1，host=b.example 是 $W1（尺子读得出两种值）"
else
  fail "★★★ 基线不成立：host=<other> 是 $V1，host=b.example 是 $W1 —— 至少一格没在数"
fi

for ((i = 0; i < 50; i++)); do
  req "$A_PORT" "nobody-$i.invalid" "$MPATH" > /dev/null
done
sleep 0.3

S3="$WORK/s3.txt"
CODE=$(scrape "$S3")
eq "抓取回 200（取增量）" 200 "$CODE"
eq "★★★ 50 个互不相同的未知 Host 之后，series 条数不增长（上界由配置定）" \
  "$N1" "$(expo series "$S3" fulcrum_no_site_match_total)"
eq "★★★ 而 host=<other> 那一格正好 +50（计数器真的在加）" \
  "$((V1 + 50))" "$(expo sum "$S3" fulcrum_no_site_match_total 'host=<other>')"
eq "★★ 已知字面量那一格没被这 50 条带动" \
  "$W1" "$(expo sum "$S3" fulcrum_no_site_match_total host=b.example)"

# ── [6/8] 判據 5：一致性門 ──────────────────────────────────────────────────
echo "=== [6/8] 判据 5：指标的增量与访问日志新增的行数逐条对得上 ==="
#
# ★ ★ ★ 它守的是「兩處不分家」：`fulcrum_requests_total` 與訪問紀錄那一行
#   由**同一個 `Record::finish`** 喂出來。⚠ 各算各的話，兩個數字都會言之鑿鑿、
#   卻對不上 —— 而那正是 D18 / G66 那個形狀。
#
# ⚠ 三件要算清楚的事：
#   ① 站台 A 配的是 `level info` ⇒ 它記全部（`LogLevel::All`）；
#   ② `no_site_match` 的請求**不屬於任何站台** ⇒ 記不進訪問紀錄，它們在指標裡是
#      `site="<none>"` —— **兩邊都不算它**；
#   ③ 抓取那一條請求自己也會被計一次、也會被記一行（`outcome="metrics"`），
#      而計數在**繪製之後** ⇒ 第 N 次抓取看到的是前 N−1 次的量。⇒ 抓兩次做差。
site_a_total() {
  local a b
  a=$(expo sum "$1" fulcrum_requests_total site=a.example)
  b=$(expo sum "$1" fulcrum_requests_total site=a2.example)
  echo "$((a + b))"
}

S4="$WORK/s4.txt"
CODE=$(scrape "$S4")
eq "抓取回 200（一致性门的左端）" 200 "$CODE"
M_A=$(site_a_total "$S4")
NONE_A=$(expo sum "$S4" fulcrum_requests_total 'site=<none>')
sleep 0.3
L_A=$(lines)

# 一串**已知形狀**的請求：站台 A 上兩種 outcome 各兩條，外加一條不屬於任何站台的。
CODE=$(req "$A_PORT" a.example "$MPATH"); eq "  k1 站点 A · metrics" 200 "$CODE"
CODE=$(req "$A_PORT" a.example "/nope"); eq "  k2 站点 A · respond" 403 "$CODE"
CODE=$(req "$A_PORT" a2.example "/nope"); eq "  k3 站点 A（第二条地址）· respond" 403 "$CODE"
CODE=$(req "$A_PORT" nobody-gate.invalid "/nope"); eq "  k4 无站点匹配（两边都不算它）" 421 "$CODE"
CODE=$(req "$A_PORT" a2.example "$MPATH"); eq "  k5 站点 A（第二条地址）· metrics" 200 "$CODE"
sleep 0.3

L_B=$(lines)
eq "★★ 访问日志新增行数 = 落在站点 A 上的那 4 条（k4 不属于任何站点 ⇒ 记不进去）" \
  4 "$((L_B - L_A))"

S5="$WORK/s5.txt"
CODE=$(scrape "$S5")
eq "抓取回 200（一致性门的右端）" 200 "$CODE"
M_B=$(site_a_total "$S5")
# ⇒ 指標那一側多出來的是：k1 k2 k3 k5 這 4 條，**加上 S4 那次抓取自己**（它在繪製之後才計）。
eq "★★★ fulcrum_requests_total 在站点 A 上的增量 = 日志新增行数 + 1（+1 是左端那次抓取自己）" \
  "$((L_B - L_A + 1))" "$((M_B - M_A))"
eq "★★ 而 site=<none> 那一格只涨了 1（k4），它一行日志都没多" \
  1 "$(($(expo sum "$S5" fulcrum_requests_total 'site=<none>') - NONE_A))"

# ── ★ ★ ★ 順帶把**兩個族**的數對起來 ────────────────────────────────────────
#
# `fulcrum_no_site_match_total`（G118，記在寫 `outcome` 的同一處）與
# `fulcrum_requests_total{site="<none>",outcome="no_site_match"}`（收尾那一處）
# 數的是**同一批事件**，只是前者按 `host` 分、後者按 `status_class` / `proto` 分
# ⇒ **兩個族的總和必須恆等**。而它們由兩個檔案裡的兩條語句各加一次。
#
# ⚠ 今天兩處一致 —— 這條斷言防的不是今天，是**將來多一條「不算命中站台」的早退路徑**
#   （批 N 的 stats 端點、第二種 421 場景……）時有人只寫了其中一句：
#   兩個數字各自都在漲、各自都言之鑿鑿，而從此差一個常數，**沒有任何東西會說**。
#   ★ 這正是 D18 / G66 那個形狀：批 M 為請求那兩族躲開了它，卻在這兩族之間留了同一個口子。
# ★ 上面兩節把兩個讀數都已經量過了（判據 4 量前者、本節量後者）——**只差這一條把它倆對起來**。
# ⚠ 判據讀的是**同一份抓取正文**，不是兩次抓取：兩句 `inc` 一前一後（一句在早退處、
#   一句在收尾），跨兩次抓取比會把一條在途請求讀成假紅。
eq "★★★ 两个族数的是同一批事件：sum(no_site_match_total) = requests_total{site=<none>,outcome=no_site_match}" \
  "$(expo sum "$S5" fulcrum_no_site_match_total)" \
  "$(expo sum "$S5" fulcrum_requests_total 'site=<none>' outcome=no_site_match)"

# ── [7/8] 判據 6：G121 的正面判據 ───────────────────────────────────────────
echo "=== [7/8] 判据 6：两个「site」给出不同的值 · 通配站点折叠成自己的字面量 ==="
#
# ★ ★ ★ 這是本批**最容易被做成同一件事**的地方：兩者共用「site」這個名字純屬巧合。
#   · 指標的 `site` 標籤（G121）= 請求**實際命中的那條位址字面值** ⇒ `a2.example`；
#   · 訪問紀錄的 `site` 欄位（R3）= **站台的名字** = 第一個位址的原文 ⇒ `http://a.example:9920`。
#   ⚠ 一個把指標的 site 直接取自紀錄那個欄位的實作，在別的每一條判據上都是綠的。
S6="$WORK/s6.txt"
CODE=$(scrape "$S6")
eq "抓取回 200（取 a2.example 那一格的基线）" 200 "$CODE"
X1=$(expo sum "$S6" fulcrum_requests_total site=a2.example)
WILD1=$(expo sum "$S6" fulcrum_requests_total 'site=*.wild.example')

CODE=$(req "$A_PORT" a2.example "/g121")
eq "用 Host: a2.example 打一条（落到兜底 respond）" 403 "$CODE"
sleep 0.3

# 同一條請求的**紀錄那一面**：`host` 是 a2.example，而 `site` 仍是第一條位址的原文。
eq "★★★ 这条请求的日志行：host" "a2.example" "$(field host)"
eq "★★★ 这条请求的日志行：site（= 站点的名字 = 第一条地址的原文）" \
  "http://a.example:$A_PORT" "$(field site)"

# ── ★ ★ ★ 通配站台：兩個不同的子網域，指標上必須摺疊成同一格 ────────────────
#
# ★ ★ ★ **這幾行守的是 `site_addr` 那個賦值點本身**（`fulcrum-server/src/lib.rs` 裡
#   `session.record.site_addr = Some(routed.site_addr.clone())` —— 整批裡 `site` 標籤
#   **唯一**的賦值點）。把它換成 `Arc::from(host.as_str())`，執行環境單測（夠不到 server 這一層）、
#   access_log 單測（夾具自己塞值，繞開了這一行）與本檔案其餘每一條判據**全都仍然綠**：
#   四條精確位址上 `host` 與位址字面值逐字相等，兩者在那份設定上是同一個字串。
# ⚠ ⚠ **必須兩個不同的子網域各打一次**：只打一個的話，「取 host」在那一次上也只產生
#   一格 —— 閉集斷言照樣綠，錯的只是那一格的名字，而名字對不對沒有第二條判據在問。
#   ⇒ 兩個子網域之後，「取 host」的實作會多出**兩格**，三條斷言一起紅。
CODE=$(req "$BC_PORT" w1.wild.example "/g121-wild")
eq "  通配站点 · 子域名 ①" 200 "$CODE"
CODE=$(req "$BC_PORT" w2.wild.example "/g121-wild")
eq "  通配站点 · 子域名 ②（★ 与 ① 不同 —— 「取 host」只有在这里才露得出来）" 200 "$CODE"
sleep 0.3

S7="$WORK/s7.txt"
CODE=$(scrape "$S7")
eq "抓取回 200（取 a2.example 那一格的增量）" 200 "$CODE"
eq "★★★ 同一条请求的**指标那一面**：site=\"a2.example\" 正好 +1" \
  1 "$(($(expo sum "$S7" fulcrum_requests_total site=a2.example) - X1))"

eq "★★★ 两个子域名折叠成通配符自己的字面量：site=\"*.wild.example\" 正好 +2" \
  2 "$(($(expo sum "$S7" fulcrum_requests_total 'site=*.wild.example') - WILD1))"
for sub in w1.wild.example w2.wild.example; do
  eq "★★★ 而 site=\"$sub\" 一条 series 都没有（请求方给的 host 没有漏进标签）" \
    0 "$(expo series "$S7" fulcrum_requests_total "site=$sub")"
done

# ★ ★ 封口：`site` 這個標籤**出現過的全部取值**是一個閉集，正好 = 五條位址字面值 + `<none>`。
#   ⇒ ① 同一個站台的兩條位址各自成格（G121，不併成一格）；
#      ② 紀錄那種帶 scheme 與連接埠的寫法**一次都沒出現**在標籤裡；
#      ③ 通配站台佔的是**它自己那條字面值**那一格，兩個子網域各自都不在集合裡；
#      ④ 上界 = 位址數 + 1，由設定定、不由訪問者定（R2）。
#   ⚠ 判的是**集合逐字相等**，不是「含有 a2.example」——「含有」在多冒出一格時照樣綠。
eq "★★★ site 标签的取值是闭集：五条地址字面量 + <none>" \
  "*.wild.example <none> a.example a2.example b.example c.example" \
  "$(expo labelvalues "$S7" fulcrum_requests_total site | tr '\n' ' ' | sed 's/ $//')"

# ── [8/8] 收尾前的一條：抓取自己也被計進去了 ────────────────────────────────
echo "=== [8/8] 抓取端点自己也在被计（它不是一条特权请求）==="
#
# ★ 它把「第 N 次抓取看到的是前 N−1 次的量」這句話釘成判據：`outcome="metrics"`
#   那一格必須隨著抓取次數增長。⚠ 一個「抓取不計數」的實作會讓上面那條一致性門
#   的 `+1` 變成 `+0`，而那條判據會紅得像是別的地方出了問題 —— 這裡說清楚它。
Y1=$(expo sum "$S7" fulcrum_requests_total site=a.example outcome=metrics)
S8="$WORK/s8.txt"
CODE=$(scrape "$S8")
eq "抓取回 200" 200 "$CODE"
eq "★★ outcome=metrics 那一格随抓取增长（上一次抓取被这一次看见了）" \
  1 "$(($(expo sum "$S8" fulcrum_requests_total site=a.example outcome=metrics) - Y1))"

# ── fulcrum_tls_requests_total：走 TLS 的請求（M2 批 M′ 任務 3，G122 / G127）──
#
# ★ ★ ★ 這一族的判據**全部掛在「與訪問紀錄那一行讀出來的值對得上」上**，
#   ⛔ 不掛在寫死的套件名上：把 `TLS_AES_256_GCM_SHA384` 寫進判據，等於讓這道門
#   在 BoringSSL 換一次預設套件時紅，而那時紅的地方指向的是一個完全正確的實作。
#   ⇒ 每一條 TLS 請求先從紀錄裡讀出這條連接真的協商出了什麼，再拿它去指標裡找那條 series。
#
# ⚠ ⚠ **⛔ 這裡沒有、也不許有「這個族的總數 = requests_total 的總數」那種判據**：
#   明文請求的 `record.tls` 是 `None` ⇒ 不計（G122 / 計畫 S5）。那條判據會在第一條
#   明文請求上紅，而那條請求什麼問題都沒有。★ 說得出口的關係只有「小於等於」。
echo "=== fulcrum_tls_requests_total（M2 批 M′ 任务 3，G122 的 TLS 半 / G127）==="

# 一條走 TLS 的請求。$1 = 路徑，其餘 = 額外的 curl 參數。回 HTTP 碼。
# ⚠ `-k`：證書是現籤自籤的。⚠ `--resolve`：SNI 要是 `a.example`（證書的 SAN 就是它）。
tls_req() {
  local path=$1
  shift
  curl -sS -o /dev/null -w '%{http_code}' -k --max-time 5 \
    --resolve "a.example:$TLS_PORT:$HOST" "$@" \
    "https://a.example:$TLS_PORT$path" 2>/dev/null || echo 000
}

# 這個族此刻的總數。★ 只有一個取數處，寫成函式免得三處各抄一遍。
tls_total() { expo sum "$1" fulcrum_tls_requests_total; }

# ── ★ 夾具自證：`--http3-only` 真的在走 QUIC，不是悄悄回落到 TCP ─────────────
#
# ⚠ ⚠ `--http3-only` 不許寫成 `--http3`：後者在 QUIC 不通時**回落到 TCP**，
#   於是下面 h3 那半會在「h3 根本沒起來」時照樣全綠（本儲存庫在 tests/h3/run.sh 裡
#   白紙黑字記過這一條）。★ 對著一個**沒有 UDP 監聽**的連接埠（站台 A 是純 HTTP）
#   打一次，它必須失敗 —— 少了這條，一個「curl 沒編進 QUIC 後端」的鏡像會讓
#   h3 那一整半變成空轉。
if curl -sS --http3-only --max-time 5 -o /dev/null \
    "https://$HOST:$A_PORT/" >/dev/null 2>&1; then
  fail "★ 对着一个没有 UDP 监听的端口 --http3-only 竟然成功了 —— 这把尺子量不了东西"
else
  ok "★ 空 UDP 端口上 --http3-only 如期失败 ⇒ 它真的在走 QUIC，不是悄悄回落"
fi

ST0="$WORK/st0.txt"
CODE=$(scrape "$ST0"); eq "抓取回 200（TLS 族的基线）" 200 "$CODE"
T0=$(tls_total "$ST0")
R0=$(expo sum "$ST0" fulcrum_requests_total)

# ── ① 明文請求**一筆都不記** ────────────────────────────────────────────────
CODE=$(req "$BC_PORT" b.example "/p1"); eq "  明文 1（站点 B）" 200 "$CODE"
CODE=$(req "$BC_PORT" b.example "/p2"); eq "  明文 2（站点 B）" 200 "$CODE"
CODE=$(req "$BC_PORT" b.example "/p3"); eq "  明文 3（站点 B）" 200 "$CODE"
sleep 0.3
ST1="$WORK/st1.txt"
CODE=$(scrape "$ST1"); eq "抓取回 200" 200 "$CODE"
# ★ 先自證這三條請求真的被數到了 —— 否則下面那條「TLS 族沒漲」在一個
#   「三條請求根本沒發出去」的世界裡也全綠。⚠ 4 = 3 條明文 + ST0 那次抓取自己
#   （計數在繪製之後 ⇒ 第 N 次抓取看到的是前 N−1 次的量）。
eq "★ 夹具自证：requests_total 涨了 4（3 条明文 + ST0 那次抓取自己）" \
  4 "$(($(expo sum "$ST1" fulcrum_requests_total) - R0))"
eq "★★★ 而 TLS 族一笔都没涨 —— 明文请求的 record.tls 是 None（G122，⛔ 别写成两族总数相等）" \
  0 "$(($(tls_total "$ST1") - T0))"

# ── ② h1 與 h2 各一條：分開斷，⛔ 不合成一條 ────────────────────────────────
#
# ⚠ ⚠ 合成一條（「打兩次、漲 2」）的話，一個**只在 h2 上記**的實作照樣全綠：
#   兩條請求走的是同一份 `{version,cipher}`，兩次 +1 與一次 +2 在總數上分不開。
#   ⇒ 一條一條打、一條一條斷。
# 打一條 TLS 請求並把它量完。$1 = 這一格的名字（也用作抓取檔案名與路徑）·
# $2 = curl 的協定開關 · $3 = 期望的 `proto` 欄位 · $4 = 上一份抓取正文（做差的左端）。
# ★ 量完把這一次的抓取正文路徑寫進 `$LAST_SCRAPE`，給下一格當左端。
LAST_SCRAPE=""
tls_case() {
  local name=$1 flag=$2 want_proto=$3 prev=$4
  local code ver cipher here before after
  code=$(tls_req "/t-$name" "$flag")
  eq "  TLS 请求（$name）回 200" 200 "$code"
  sleep 0.3
  eq "  ★ 它真的走的是 $want_proto（否则下面量的是另一条路）" "$want_proto" "$(field proto)"
  ver=$(field tls_version)
  cipher=$(field tls_cipher)
  # ★ 夾具自證：h1/h2 上套件**真的問得出來** —— 否則下面那條交叉斷言會退化成
  #   「<unknown> 那一格漲了 1」，而它在一個什麼都讀不出來的實作上也全綠。
  if [ -n "$cipher" ] && [ "$cipher" != "<缺>" ] && [ "$cipher" != "<unknown>" ]; then
    ok "  ★ 夹具自证：$name 上真的协商出了套件（$cipher），不是 <unknown>"
  else
    fail "  ★ $name 上 tls_cipher 读不出真值（拿到「$cipher」）—— 交叉断言会退化成空转"
  fi
  here="$WORK/st-$name.txt"
  code=$(scrape "$here")
  eq "  抓取回 200" 200 "$code"
  eq "★★★ $name：TLS 族正好 +1" 1 "$(($(tls_total "$here") - $(tls_total "$prev")))"
  # ★ ★ ★ 交叉對照：漲的那一筆必須落在**訪問紀錄剛剛報出來的那一對值**上，
  #   ⛔ 不是落在寫死的套件名上，也不是「隨便哪條 series 漲了」。
  before=$(expo sum "$prev" fulcrum_tls_requests_total "version=$ver" "cipher=$cipher")
  after=$(expo sum "$here" fulcrum_tls_requests_total "version=$ver" "cipher=$cipher")
  eq "★★★ $name：涨的正好是 {version=$ver, cipher=$cipher} 那一条（与访问日志同一对值）" \
    1 "$((after - before))"
  LAST_SCRAPE="$here"
}

# ⚠ ⚠ 兩格**分開打、分開斷**：合成一條（「打兩次、漲 2」）的話，一個**只在 h2 上記**
#   的實作照樣全綠 —— 兩條請求走的是同一份 `{version,cipher}`，兩次 +1 與一次 +2
#   在總數上分不開。
tls_case http1.1 --http1.1 "HTTP/1.1" "$ST1"
tls_case h2 --http2 "HTTP/2.0" "$LAST_SCRAPE"
ST2="$LAST_SCRAPE"

# ── ③ h3：取不到的那一格記 `<unknown>`，⛔ 不記空串（G127）─────────────────
CODE=$(tls_req "/t-h3" --http3-only)
eq "  TLS 请求（h3）回 200" 200 "$CODE"
sleep 0.3
eq "  ★ 它真的走的是 HTTP/3.0" "HTTP/3.0" "$(field proto)"
# RFC 9001 §4.2：QUIC 只能用 TLS 1.3。★ 這是規範，不是我們猜的。
eq "  ★ h3 的 tls_version 是 TLSv1.3（RFC 9001 §4.2）" "TLSv1.3" "$(field tls_version)"
# ⚠ ⚠ **兩側處置不同，這裡一併釘住**：訪問紀錄裡那一格**不出現**（契約：取不到的
#   欄位不出現）；而指標裡它必須是 `<unknown>`（指標沒有「那一格不出現」這回事）。
#   ★ 少了上面這半，「紀錄與指標是同一套處置」這句假話沒有任何東西擋得住。
eq "★★★ h3：访问日志里 tls_cipher **不出现**（取不到的字段不出现）" "<缺>" "$(field tls_cipher)"
ST3="$WORK/st-h3.txt"
CODE=$(scrape "$ST3"); eq "  抓取回 200" 200 "$CODE"
eq "★★★ h3：TLS 族正好 +1" 1 "$(($(tls_total "$ST3") - $(tls_total "$ST2")))"
UNK_BEFORE=$(expo sum "$ST2" fulcrum_tls_requests_total version=TLSv1.3 'cipher=<unknown>')
UNK_AFTER=$(expo sum "$ST3" fulcrum_tls_requests_total version=TLSv1.3 'cipher=<unknown>')
eq "★★★ h3：涨的正好是 {version=TLSv1.3, cipher=<unknown>} 那一条（G127）" \
  1 "$((UNK_AFTER - UNK_BEFORE))"

# ── ④ ★ ★ ★ G127 的正主：這個族裡**一個空標籤值都不許有** ──────────────────
#
# ⚠ ⚠ 在 Prometheus 資料模型裡空標籤值等同於「這個標籤不存在」⇒ 記空串的話，
#   「今天這一格取不到」與「明天把 cipher 整個刪掉」在抓取端**分不開**；
#   而 `cipher=""` 恰好是 PromQL 裡「該標籤不存在」的慣用寫法。
# ★ ★ 這一條**不看是不是 h3**：它問的是「有沒有哪條 series 的哪一格是空的」——
#   與 G127 把判據落在「值為空」上、⛔ 不落在「是不是 h3」上逐字同一個形狀。
# ⚠ 取樣前先自證樣本里真有東西：一個恆答空集的讀法會讓下面那條恆綠。
TLS_SERIES=$(expo series "$ST3" fulcrum_tls_requests_total)
if [ "$TLS_SERIES" -ge 2 ]; then
  ok "★ 取样自证：这个族此刻有 $TLS_SERIES 条 series（h1/h2 那对 + h3 的 <unknown> 那条）"
else
  fail "★ 这个族此刻只有 $TLS_SERIES 条 series —— 下面那条空标签检查会退化成空转"
fi
for lbl in version cipher; do
  if expo labelvalues "$ST3" fulcrum_tls_requests_total "$lbl" | grep -qx ''; then
    fail "★★★ G127：$lbl 那一格出现了**空标签值** —— 抓取端读起来等同于「没有这个标签」"
  else
    ok "★★★ G127：$lbl 那一格没有任何空值（取不到的记 <unknown>，不记空串）"
  fi
done

# ── ⑤ 標籤鍵逐字，⛔ 不許多出 sni / alpn ────────────────────────────────────
#
# ★ 判的是**鍵的集合逐字相等**（`labelkeys` 打的是排序後的鍵名）：寫成「含有
#   version 與 cipher」的話，多出一個 `sni` 標籤照樣綠 —— 而那正是 G122 明令
#   擋住的東西（`sni`/`alpn` 是**訪問者給的**，與 G121 擋住 host 是同一件事）。
eq "★★★ 这个族只带 {cipher, version} 两个标签（⛔ 不许有 sni / alpn）" \
  "cipher,version" "$(expo labelkeys "$ST3" fulcrum_tls_requests_total)"

# ── ⑥ 與 requests_total 的關係：**小於**，⛔ 不是相等 ───────────────────────
TLS_SUM=$(tls_total "$ST3")
REQ_SUM=$(expo sum "$ST3" fulcrum_requests_total)
if [ "$TLS_SUM" -gt 0 ] && [ "$TLS_SUM" -lt "$REQ_SUM" ]; then
  ok "★★★ sum(tls_requests_total)=$TLS_SUM 严格小于 sum(requests_total)=$REQ_SUM（明文请求不计，G122）"
else
  fail "★★★ 期望 0 < tls_requests_total($TLS_SUM) < requests_total($REQ_SUM) —— \
明文请求被计进 TLS 族了，或者 TLS 请求一条都没被计"
fi

# ── fulcrum_overrides_active：與 GET /stats 同源、懸空的照樣計入 ───────────
#   （M2 批 N 任務 6.5，G126，裁決 R13）
#
# ⚠ ⚠ ⚠ 判據 1/2/3/5 必須從真的 /metrics 出口上抓，不許只在 Rust 單測裡
#   直接調繪製函式（任務 4 的教訓：16 條判據全直接調 handler，刪掉路由那一行
#   21 條判據照樣全綠）。/stats 那一側同理必須走真 admin socket，不直接調
#   `AdminApp::stats()`——否則「路由真的接在生產會走的那條線上」這件事
#   本身就沒有判據在守。
echo "=== fulcrum_overrides_active（M2 批 N 任务 6.5，G126，R13）==="

admin_post() {
  # $1 = 路徑，$2 = body。回狀態碼，正文落 $WORK/admin.out（與 tests/serve/run.sh 同一個約定）。
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

admin_get() {
  # $1 = 路徑。回狀態碼，正文落 $WORK/admin.out。
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X GET \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# /stats 那一側只服務本節：overrides 陣列的總條目數、其中 dangling=true 的條數。
# ★ 與 tests/serve/run.sh 的 stats_check.py 是同一種分工（判據本身落在 bash 這層
# 的 eq 上，python3 只負責把 JSON 讀成一個數）。
stats_overrides_total() {
  python3 -c '
import json, sys
v = json.load(open(sys.argv[1], encoding="utf-8"))
print(len(v["overrides"]))
' "$1"
}
stats_overrides_dangling() {
  python3 -c '
import json, sys
v = json.load(open(sys.argv[1], encoding="utf-8"))
print(sum(1 for e in v["overrides"] if e["dangling"]))
' "$1"
}

OV_SITE="http://ov.example:$BC_PORT"
# ★ 沿用任務 6 報告點名過的那個「幽靈連接埠」慣例：19999 從不監聽，只用來登記
#   覆蓋鍵，不需要真的發起連接——不在 AGENTS.md 的連接埠表裡，因為它從不是
#   真正被佔用的連接埠。
OV_UP="127.0.0.1:19999"

# ★ ★ 兩代設定都在**原有四個站台原封不動**的基礎上，只新增 `ov.example` 這
#   一個站台——不碰 A_PORT/BC_PORT 已經在監聽的那兩個監聽器（`listen_ports`
#   只按「連接埠 + 是否 TLS」比較，加一個新主機名不會讓它們對不上而撞 409）。
#   ⚠ ⚠ 不能把這個站台擺進**第一代**：那樣一來 [2/8] 那條「沒有 reverse_proxy
#   ⇒ upstream_inflight/healthy 零樣本」的判據就會被這裡的真實上游破壞——
#   「兩族族在、樣本無」這條判據與本節的覆蓋夾具互斥，天然只能分兩代擺。
cat > "$WORK/ov.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
    auto_http_redirect false
}

http://a.example:$A_PORT, http://a2.example:$A_PORT {
    log {
        output file $LOGFILE
        level info
    }
    @internal {
        remote_ip 127.0.0.0/8
        path $MPATH
    }
    handle @internal {
        metrics
    }
    # ★ 「status_class=none」那条路（M2 批 M′ 任务 4，G124）。⚠ 它挂在**站点 A** 上
    #   是有意的：那一条要求「访问日志真的多了一行」，而站点 A 是本格唯一配了 log 的
    #   HTTP 站点。⚠ ⚠ 它**不能**摆进第一代配置 —— 那会让 [2/8] 那条「没有上游 ⇒
    #   upstream_inflight/healthy 零样本」当场破掉（与 ov.example 同一个理由）。
    handle /abort {
        reverse_proxy 127.0.0.1:$ABORT_UP_PORT
    }
    respond 403
}

http://b.example:$BC_PORT {
    respond 200 "b-ok"
}

http://c.example:$BC_PORT {
    @outsider remote_ip 10.0.0.0/8
    handle @outsider {
        metrics
    }
    respond 403
}

http://*.wild.example:$BC_PORT {
    respond 200 "wild-ok"
}

http://ov.example:$BC_PORT {
    handle /a/* {
        reverse_proxy $OV_UP {
            id ova
        }
    }
    handle {
        reverse_proxy $OV_UP
    }
}

# ★ 站点 T 原样带过来：「POST /load」按 (端口, 是否 TLS) 比监听器集，
#   这一份少了它就是「端口集变了」⇒ 409（理由见第一份配置那段注释）。
# ⚠ 这个 heredoc 不带引号（要展开端口变量）⇒ 本行里写反引号会被当成命令替换，
#   所以这里一律用「」—— 与 tests/log/run.sh 里那条同一个陷阱。
a.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    log {
        output file $LOGFILE
        level info
    }
    respond 200 "t-ok"
}

# ★ ★ L4 两格（**M2 批 O**）：连接族要在同一个进程里同时看到五种入口。
# ⚠ ⚠ 它必须出现在**三份**配置里 —— 「POST /load」按 (协议, 监听地址原样) 比 L4
#   监听器集，少写一份就是 409，而红会落在 overrides 那一节、指向完全错误的方向。
# ⚠ 这个 heredoc 不带引号（要展开端口变量）⇒ 注释里一律用「」，别用反引号。
l4 {
    tcp :$L4T_PORT {
        proxy 127.0.0.1:$ABORT_UP_PORT
    }
    udp :$L4U_PORT {
        proxy 127.0.0.1:$UDPUP_PORT
    }
}
CONF
"$BIN" compile "$WORK/ov.Fulcrumfile" > "$WORK/ov.json" 2>/dev/null || {
  echo "METRICS TESTS FAILED: compile ov.Fulcrumfile 失败" >&2
  exit 1
}

# 同樣的骨架，但 `ov.example` 只留預設 handle 那一條路——寫了 `id ova` 的那把
# 鍵因此懸空，沒寫 id 的那把仍然活著（與 tests/serve/run.sh 的 ov-dangling
# 手法同一個形狀）。
cat > "$WORK/ov-dangling.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
    auto_http_redirect false
}

http://a.example:$A_PORT, http://a2.example:$A_PORT {
    log {
        output file $LOGFILE
        level info
    }
    @internal {
        remote_ip 127.0.0.0/8
        path $MPATH
    }
    handle @internal {
        metrics
    }
    # ★ 与上一份逐字相同（G124 那条路；判据跑在这一代上）。
    handle /abort {
        reverse_proxy 127.0.0.1:$ABORT_UP_PORT
    }
    respond 403
}

http://b.example:$BC_PORT {
    respond 200 "b-ok"
}

http://c.example:$BC_PORT {
    @outsider remote_ip 10.0.0.0/8
    handle @outsider {
        metrics
    }
    respond 403
}

http://*.wild.example:$BC_PORT {
    respond 200 "wild-ok"
}

http://ov.example:$BC_PORT {
    handle {
        reverse_proxy $OV_UP
    }
}

# ★ 站点 T 原样带过来（同上：少了它这一份就撞 409）。
a.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    log {
        output file $LOGFILE
        level info
    }
    respond 200 "t-ok"
}

# ★ ★ L4 两格（**M2 批 O**）：连接族要在同一个进程里同时看到五种入口。
# ⚠ ⚠ 它必须出现在**三份**配置里 —— 「POST /load」按 (协议, 监听地址原样) 比 L4
#   监听器集，少写一份就是 409，而红会落在 overrides 那一节、指向完全错误的方向。
# ⚠ 这个 heredoc 不带引号（要展开端口变量）⇒ 注释里一律用「」，别用反引号。
l4 {
    tcp :$L4T_PORT {
        proxy 127.0.0.1:$ABORT_UP_PORT
    }
    udp :$L4U_PORT {
        proxy 127.0.0.1:$UDPUP_PORT
    }
}
CONF
"$BIN" compile "$WORK/ov-dangling.Fulcrumfile" > "$WORK/ov-dangling.json" 2>/dev/null || {
  echo "METRICS TESTS FAILED: compile ov-dangling.Fulcrumfile 失败" >&2
  exit 1
}

CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
eq "装载 ov.Fulcrumfile（新增 ov.example，两把可覆盖的键）" 200 "$CODE"

# ── 判據 4（更強的一版）：這一刻真的有 reverse_proxy 了，但一項覆蓋都還沒設 ──
#   ⇒ fulcrum_overrides_active 仍然是 0——證明它數的是「覆蓋」，不是「隨便什麼
#   跟 reverse_proxy 沾邊的東西」。
S9A="$WORK/s9a.txt"
CODE=$(scrape "$S9A")
eq "重新抓取回 200" 200 "$CODE"
# ⚠ ⚠ `expo sum` 在「一條樣本都沒有」與「樣本存在、值是 0」兩種情況下算出
#   的都是 0（沒有匹配項時 `sum` 的初值就是 0）——光看 sum 擋不住「count==0
#   就整族不出樣本」這種實作。⇒ 先用 `series` 證「樣本真的存在」，sum 那條
#   才談得上是在量「值」而不是在量一個巧合。
eq "★★★ fulcrum_overrides_active 此刻仍出一条样本（不是被 0 值悄悄吞掉）" \
  1 "$(expo series "$S9A" fulcrum_overrides_active)"
eq "★★★ 有上游了，但还没设覆盖 ⇒ fulcrum_overrides_active 仍是 0" \
  0 "$(expo sum "$S9A" fulcrum_overrides_active)"

# ── 擺兩把覆蓋鍵：沒寫 id 的那把（之後仍然活著）+ id=ova 的那把（之後會懸空）──
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$OV_SITE\",\"upstream\":\"$OV_UP\"}]}")
eq "POST /runtime：摘掉默认 handle 那条路（没写 id）" 200 "$CODE"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$OV_SITE\",\"id\":\"ova\",\"upstream\":\"$OV_UP\"}]}")
eq "POST /runtime：摘掉 id=ova 那条路" 200 "$CODE"

# ── 判據 2（第一次，0 懸空）：GET /stats 與 GET /metrics 同一時刻各抓一次 ──
CODE=$(admin_get /stats)
eq "GET /stats（真 socket）" 200 "$CODE"
cp "$WORK/admin.out" "$WORK/ov_stats1.json"
S9B="$WORK/s9b.txt"
CODE=$(scrape "$S9B")
eq "GET /metrics（数据面，真 HTTP）" 200 "$CODE"

capture_ok "/stats 总条目数（0 悬空这一刻）" stats_overrides_total    "$WORK/ov_stats1.json"; ST1_TOTAL="$CAPTURE_OUT"
capture_ok "/stats 悬空条目数（0 悬空这一刻）" stats_overrides_dangling "$WORK/ov_stats1.json"; ST1_DANGLING="$CAPTURE_OUT"
capture_ok "/metrics 的 overrides_active（0 悬空这一刻）" expo sum "$S9B" fulcrum_overrides_active; M1_VALUE="$CAPTURE_OUT"
eq "★ 夹具前提：/stats 里两项覆盖都还没悬空" 0 "$ST1_DANGLING"
eq "★ 夹具前提：/stats 里总数是 2" 2 "$ST1_TOTAL"
eq "★★★ 判据 2：fulcrum_overrides_active 与 /stats 的 overrides 总条目数同源（0 悬空这一刻）" \
  "$ST1_TOTAL" "$M1_VALUE"

# ── 換成只留預設路那條的設定 ⇒ id=ova 那把鍵懸空，沒寫 id 的那把仍然活著 ──
CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov-dangling.json")")
eq "overrides=keep：换成不带 id=ova 那条路的配置" 200 "$CODE"

# ── 判據 2（第二次，1 懸空）+ 判據 3：懸空的照樣計入 ─────────────────────
CODE=$(admin_get /stats)
eq "GET /stats（换代之后，真 socket）" 200 "$CODE"
cp "$WORK/admin.out" "$WORK/ov_stats2.json"
S9C="$WORK/s9c.txt"
CODE=$(scrape "$S9C")
eq "GET /metrics（换代之后，真 HTTP）" 200 "$CODE"

capture_ok "/stats 总条目数（1 悬空这一刻）" stats_overrides_total    "$WORK/ov_stats2.json"; ST2_TOTAL="$CAPTURE_OUT"
capture_ok "/stats 悬空条目数（1 悬空这一刻）" stats_overrides_dangling "$WORK/ov_stats2.json"; ST2_DANGLING="$CAPTURE_OUT"
capture_ok "/metrics 的 overrides_active（1 悬空这一刻）" expo sum "$S9C" fulcrum_overrides_active; M2_VALUE="$CAPTURE_OUT"
eq "★ 夹具前提：/stats 里总数还是 2（R8：设过覆盖的悬空了也不删）" 2 "$ST2_TOTAL"
eq "★ 夹具前提：/stats 里悬空数是 1（id=ova 那把）" 1 "$ST2_DANGLING"
# ⚠ ⚠ 判據寫法紀律：總數（2）與懸空數（1）必須不相等，否則一個把兩者讀串了
#   的實作（比如把 fulcrum_overrides_active 接成懸空數、或接成「非懸空數」）
#   在這份夾具上也可能蒙對——上面這條斷言先把「$ST2_TOTAL != $ST2_DANGLING」
#   釘死，下面兩條才真的分得清讀對了還是讀串了。
if [ "$ST2_TOTAL" = "$ST2_DANGLING" ]; then
  fail "夹具写法纪律没守住：总数与悬空数相等（$ST2_TOTAL），两者会被读串也测不出来"
else
  ok "★ 夹具写法纪律：总数（$ST2_TOTAL）与悬空数（$ST2_DANGLING）不相等"
fi
eq "★★★ 判据 3：悬空的照样计入——fulcrum_overrides_active 是总数 2，不是悬空数 1" \
  2 "$M2_VALUE"
eq "★★★ 判据 2：换代之后仍与 /stats 的 overrides 总条目数同源（1 悬空这一刻）" \
  "$ST2_TOTAL" "$M2_VALUE"

# ── 判據 5（非退化版）：登記處裡明明有 2 項覆蓋，正文裡仍然只有一行 ──────────
eq "★★★ 判据 5：即便有 2 项覆盖，fulcrum_overrides_active 也只出一条 series（基数恒为 1）" \
  1 "$(expo series "$S9C" fulcrum_overrides_active)"
eq "★ 那一行不带任何标签" "" "$(expo labelkeys "$S9C" fulcrum_overrides_active)"

# ── status_class="none"：那條路真的走得到（M2 批 M′ 任務 4，G124 / D30 結案）──
#
# ★ ★ ★ G124 明寫這一格要**兩邊一起斷**：訪問紀錄真的多了一行 `status=0`，
#   且 `fulcrum_requests_total{status_class="none"}` 正好 +1。
#   ⇒ 在這之前，閉集裡的第六個值只存在於程式碼與文件裡，**一條端到端判據都沒走過**。
#
# ⚠ ⚠ ⚠ **「下游斷開」本身不足以讓 status 變成 0**，而那句話讀起來像是夠的。
#   開工前的探針實測（三種構型，同一棵樹同一天）：
#     · `respond` 站台 + 發完請求立刻 RST                      ⇒ status **200**
#     · 慢上游（**帶** Content-Length）+ RST 之後 1.2s 才寫     ⇒ status **200**（resp_size 是 0）
#     · 慢上游（**不帶** Content-Length）+ RST                  ⇒ status **0** ✅
#
#   機制在 pingora 的 `write_response_header`（`v1/server.rs`）：回應頭 `write_all`
#   進的是**帶緩衝的流**，而 flush 只在「1xx 或**沒有** Content-Length」時才發生
#   ⇒ 帶 CL 的回應頭**根本不產生 syscall**，`write_all` 回傳 Ok，`response_written`
#   照樣被置上，於是 status 是 200。★ 只有會 flush 的那一支才真的碰 socket，
#   也只有它會在一條被 RST 掉的連接上失敗 —— 失敗了 `response_written` 才留在 None。
#
# ⇒ 夾具用的是**不帶 Content-Length 的慢上游**：它把「伺服器端此刻還一個位元組都沒寫」
#   與「寫的時候真的會有一次 syscall」兩件事同時釘住 —— ⛔ **這不是一場競賽**。
# ★ 對照組（同一條路、用戶端正常收完）拿到 200，那證明這把尺子量得了兩邊。
echo "=== status_class=\"none\"（M2 批 M′ 任务 4，G124 / D30 结案）==="

# 慢上游：睡 1.5s，再回一個**不帶 Content-Length** 的回應（關閉定界）。
# ⚠ 每條連接一個執行緒：`wait_port` 的探測連接會被 accept 一次，單執行緒的話它會把
#   accept 循環佔住 1.5s，真正那條請求就排在後面了。
cat > "$WORK/slow_up.py" <<'PY'
import signal
import socket
import sys
import threading
import time


def handle(conn):
    try:
        if not conn.recv(65536):
            return          # `wait_port` 的探测连接：收到 EOF 就走，别睡。
        time.sleep(1.5)
        # ⚠ ⚠ **有意不带 Content-Length**（关闭定界）——它让枢衡在写响应头那一步
        #   真的 flush 一次，而那一次 syscall 正是本节判据要的东西。
        conn.sendall(b"HTTP/1.1 200 OK\r\nX-Slow-Upstream: 1\r\n\r\nslow-body")
    except OSError:
        pass
    finally:
        try:
            conn.close()
        except OSError:
            pass


srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(16)
# ⚠ 收尾那段先发 SIGINT、等 5s 再 SIGKILL。⛔ 让它走到 SIGKILL 的代价不是「慢 5 秒」，
#   是门禁日志末尾多出一行 `… Killed …` —— 而下一个读日志的人会把它读成「有东西崩了」。
#   ⇒ 两个信号都接住，干净退出。★ `settimeout` 让主线程每 0.5s 回一次 Python，
#     信号才有机会被处理（阻塞在 accept() 里的时候它不一定回得来）。
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
srv.settimeout(0.5)
try:
    while True:
        try:
            c, _ = srv.accept()
        except socket.timeout:
            continue
        c.settimeout(None)
        threading.Thread(target=handle, args=(c,), daemon=True).start()
except (KeyboardInterrupt, SystemExit):
    pass
finally:
    srv.close()
PY

# 用戶端。$1 = read | rst · $2 連接埠 · $3 路徑 · $4 Host。
# ⚠ curl 做不了這件事：要的是 **SO_LINGER(1,0) + close ⇒ 立刻發 RST**，
#   而不是正常的 FIN —— 半關之下伺服器端第一次寫照樣成功（探針實測過）。
cat > "$WORK/abort_req.py" <<'PY'
import socket
import struct
import sys

mode, port, path, host = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
s = socket.create_connection(("127.0.0.1", port), timeout=10)
s.sendall(
    ("GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n" % (path, host)).encode()
)
if mode == "read":
    s.settimeout(10)
    buf = b""
    try:
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
    except OSError:
        pass
    s.close()
    print(buf.split(b"\r\n", 1)[0].decode("latin-1") if buf else "<空>")
else:
    # linger=0 ⇒ close() 立刻发 RST，而不是走 FIN 那条正常路。
    s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    s.close()
    print("RST")
PY

python3 "$WORK/slow_up.py" "$ABORT_UP_PORT" > "$WORK/slow_up.log" 2>&1 &
PIDS+=($!)
wait_port "$ABORT_UP_PORT" || {
  echo "METRICS TESTS FAILED: 慢上游端口 $ABORT_UP_PORT 起不来。" >&2
  cat "$WORK/slow_up.log" >&2 || true
  exit 1
}
# ⛔ 這句話有意**只說連接埠起來了**：「它睡 1.5s 且不帶 Content-Length」是夾具的性質，
#   寫進這一行就成了一句**沒有任何東西驗證的散文** —— 實測過：給上游加回
#   `Content-Length` 之後這一行照樣列印「且不帶 Content-Length」，而下面三條判據紅。
# ★ 那兩個性質**有門守著，只是門在下面**：帶了 CL 的話 status 就是 200，
#   「那一行的 status 是 0」當場紅。⇒ 屬性由判據說，不由這一行說。
ok "慢上游起来了（$ABORT_UP_PORT）"

SN0="$WORK/sn0.txt"
CODE=$(scrape "$SN0"); eq "抓取回 200（none 那一格的基线）" 200 "$CODE"
NONE0=$(expo sum "$SN0" fulcrum_requests_total status_class=none)
sleep 0.3
LN0=$(lines)

# ── ★ 對照組先跑：同一條路、用戶端**正常收完** ⇒ 200，而 none 那一格不漲 ──────
#   ⚠ 少了它，下面那兩條在一個「status 恆為 0」的實作上也全綠。
capture_ok "对照组请求（正常收完）" python3 "$WORK/abort_req.py" read "$A_PORT" /abort a.example
CTRL_LINE="$CAPTURE_OUT"
case "$CTRL_LINE" in
  "HTTP/1.1 200"*) ok "★ 对照：客户端自己收到了 $CTRL_LINE（这条路本身是通的）" ;;
  *) fail "★ 对照：客户端没收到 200 状态行，拿到的是「$CTRL_LINE」" ;;
esac
sleep 0.5
LN1=$(lines)
eq "★ 对照：访问日志多了一行" 1 "$((LN1 - LN0))"
eq "★ 对照：那一行 status=200" 200 "$(field status)"
eq "★ 对照：那一行 outcome=reverse_proxy" reverse_proxy "$(field outcome)"
SN1="$WORK/sn1.txt"
CODE=$(scrape "$SN1"); eq "抓取回 200" 200 "$CODE"
NONE1=$(expo sum "$SN1" fulcrum_requests_total status_class=none)
eq "★★ 对照：status_class=none 一格没涨（这把尺子不是恒答有）" 0 "$((NONE1 - NONE0))"
sleep 0.3
# ⚠ 基線要在**這次抓取自己那一行**寫進去之後再取：抓取也是一條被記的請求，
#   拿 LN1 當左端的話，下面那條「多了一行」會算成 2。
LN1B=$(lines)

# ── ★ ★ ★ 正主：發完請求立刻 RST，而上游還要 1.5s 才回話 ────────────────────
capture_ok "abort 请求（发完立刻 RST）" python3 "$WORK/abort_req.py" rst "$A_PORT" /abort a.example
sleep 2.5
LN2=$(lines)
eq "★★★ G124 ①：访问日志真的多了一行（站点匹配上了 ⇒ 它记得进去）" 1 "$((LN2 - LN1B))"
eq "★★★ G124 ①：那一行的 status 是 0 —— 一个响应头都没写出去" 0 "$(field status)"
# ⚠ ⚠ 這一條守的是契約裡那句「此時 outcome 仍是執行鏈給的那個值」。
# ★ 它立起來時防的是 `Record` 上那個 `aborted` 預設值；那個欄位今天已經刪掉
#   （outcome 改成 serve_one 的回傳值）⇒ 防的東西在類型上沒有了，而**判據留著**：
#   它守的是「鏈給的值不被收尾那一步覆蓋掉」，那件事與欄位在不在無關。
eq "★★★ 而 outcome 仍是执行链给的那个值" reverse_proxy "$(field outcome)"
eq "★ 那一行也有 site（它属于站点 A，所以才记得进访问日志）" \
  "http://a.example:$A_PORT" "$(field site)"
SN2="$WORK/sn2.txt"
CODE=$(scrape "$SN2"); eq "抓取回 200" 200 "$CODE"
eq "★★★ G124 ②：fulcrum_requests_total{status_class=\"none\"} 正好 +1" \
  1 "$(($(expo sum "$SN2" fulcrum_requests_total status_class=none) - NONE1))"
# ★ 兩邊一起斷的最後一句：那一筆落在**站台 A** 上，而不是 site=<none> ——
#   ⚠ 它證明「紀錄有那一行、指標有那一筆」說的是**同一條請求**，不是兩件碰巧同時發生的事。
NONE_A_BEFORE=$(expo sum "$SN1" fulcrum_requests_total site=a.example status_class=none)
NONE_A_AFTER=$(expo sum "$SN2" fulcrum_requests_total site=a.example status_class=none)
eq "★★★ 而且那一笔落在 site=a.example 上（与日志那一行是同一条请求）" \
  1 "$((NONE_A_AFTER - NONE_A_BEFORE))"

# ── 連接那兩個族（M2 批 O，G122 的連接那半）─────────────────────────────────
#
# ★ ★ ★ 這一節的核心斷言只有一句：**四個互不相干的 accept 循環都記進了同一個族，
#   而 entrypoint 分得開。** 五種入口（http / admin / quic / l4_tcp / l4_udp）
#   必須在**同一個處理程序**裡同時存在，那句話才驗得了 —— 這就是 l4 塊擺進本場景的理由。
#
# ⚠ 判據一律**從夾具派生** listen 的值，⛔ 不寫死：它隨 --bind-host 變，
#   而 admin 那條是每次跑都不同的 $WORK/admin.sock。
echo "=== 连接那两个族（M2 批 O，G122 的连接那半）==="

# UDP 回顯上游（L4 UDP 那一格要）。★ 接住 SIGINT/SIGTERM 乾淨退出，
# 否則收尾那段要走到 kill -9，紀錄末尾會多出一行「Killed」被讀成「有東西崩了」。
cat > "$WORK/udp_echo.py" <<'PY'
import signal
import socket
import sys

signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.settimeout(0.5)
try:
    while True:
        try:
            data, peer = s.recvfrom(65536)
        except socket.timeout:
            continue
        s.sendto(data, peer)
except (KeyboardInterrupt, SystemExit):
    pass
finally:
    s.close()
PY
python3 "$WORK/udp_echo.py" "$UDPUP_PORT" > "$WORK/udp_echo.log" 2>&1 &
PIDS+=($!)
ok "UDP 回显上游起来了（$UDPUP_PORT）"

# 這個族此刻的某一格。$1 抓取檔案 · $2 族名 · $3 entrypoint · $4 listen。
conn_val() { expo sum "$1" "$2" "entrypoint=$3" "listen=$4"; }
# 這個族此刻的全部 (entrypoint,listen) 對，排序後一行一個。
conn_keys() {
  python3 - "$1" <<'PY'
import re
import sys

out = set()
pat = re.compile(r'^fulcrum_connections_active\{(.*)\}\s')
for line in open(sys.argv[1], encoding="utf-8"):
    m = pat.match(line)
    if not m:
        continue
    kv = dict(re.findall(r'(\w+)="((?:[^"\\]|\\.)*)"', m.group(1)))
    out.add("%s %s" % (kv.get("entrypoint"), kv.get("listen")))
for k in sorted(out):
    print(k)
PY
}

SC0="$WORK/sc0.txt"
CODE=$(scrape "$SC0"); eq "抓取回 200（连接族的基线）" 200 "$CODE"

# ── 判據 1：series 集合 == 本處理程序的監聽器集合（逐字，從夾具派生）──────────────
#
# ★ 一條斷言同時驗掉兩件事：「建成就出樣本」（不是有連接才出現）與
#   「**四處都記進了同一個族**」—— 少接一處，這一條當場少一行。
EXPECT_KEYS=$(printf '%s\n' \
  "admin $ADMIN_SOCK" \
  "http $HOST:$A_PORT" \
  "http $HOST:$BC_PORT" \
  "http $HOST:$TLS_PORT" \
  "l4_tcp $HOST:$L4T_PORT" \
  "l4_udp $HOST:$L4U_PORT" \
  "quic $HOST:$TLS_PORT" | sort)
GOT_KEYS=$(conn_keys "$SC0")
if [ "$EXPECT_KEYS" = "$GOT_KEYS" ]; then
  ok "★★★ 判据 1：series 集合与本进程的监听器集合逐字相同（五个 entrypoint 都在）"
else
  fail "★★★ 判据 1：series 集合对不上
    期望：$(echo "$EXPECT_KEYS" | tr '\n' '|')
    实际：$(echo "$GOT_KEYS" | tr '\n' '|')"
fi

# ── 判據 2：五個 entrypoint **各打一條連接 ⇒ 那一格 total 正好 +1** ───────────
#
# ⚠ ⚠ **分開打、分開斷**：合成一條（「打五次、漲 5」）的話，一個只在 http 上記的
#   實作照樣全綠 —— 與批 M′ 任務 3 裡 h1/h2 那條同一形狀。
conn_case() {
  # $1 說明 · $2 entrypoint · $3 listen · $4 上一份抓取 · 其餘 = 打一條連接的命令
  local what=$1 ep=$2 listen=$3 prev=$4
  shift 4
  local before after here
  before=$(conn_val "$prev" fulcrum_connections_total "$ep" "$listen")
  "$@" >/dev/null 2>&1 || true
  sleep 0.4
  here="$WORK/sc-$ep.txt"
  scrape "$here" >/dev/null
  after=$(conn_val "$here" fulcrum_connections_total "$ep" "$listen")
  eq "★★★ 判据 2（$what）：$ep 那一格 total 正好 +1" 1 "$((after - before))"
  LAST_CONN_SCRAPE="$here"
}

# ⚠ http 那一格：抓取自己也走 http ⇒ 用一條**別的** http 請求量它會多算抓取那一筆。
#   ⇒ 這一格改成量 BC_PORT（抓取走的是 A_PORT），兩者是不同的監聽器、不同的 series。
conn_case "l4_tcp：连一下 L4 TCP 端口" l4_tcp "$HOST:$L4T_PORT" "$SC0" \
  timeout 3 bash -c "exec 3<>/dev/tcp/$HOST/$L4T_PORT"
conn_case "http：打一条到站点 B 的请求" http "$HOST:$BC_PORT" "$LAST_CONN_SCRAPE" \
  curl -sS --max-time 5 -H "Host: b.example" -o /dev/null "http://$HOST:$BC_PORT/conn"
conn_case "admin：一次 GET /stats" admin "$ADMIN_SOCK" "$LAST_CONN_SCRAPE" \
  curl -sS --max-time 5 --unix-socket "$ADMIN_SOCK" -o /dev/null "http://localhost/stats"
conn_case "quic：一次 --http3-only" quic "$HOST:$TLS_PORT" "$LAST_CONN_SCRAPE" \
  curl -sS -o /dev/null -k --max-time 5 --http3-only \
    --resolve "a.example:$TLS_PORT:$HOST" "https://a.example:$TLS_PORT/conn-h3"
# ⚠ L4 UDP 那一格的 total 是**事件點**（新建工作階段那一處），active 才是派生的。
conn_case "l4_udp：发一个数据报" l4_udp "$HOST:$L4U_PORT" "$LAST_CONN_SCRAPE" \
  python3 -c "
import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2)
s.sendto(b'ping', ('127.0.0.1', int(sys.argv[1])))
try: s.recvfrom(65536)
except OSError: pass
s.close()
" "$L4U_PORT"

SC1="$LAST_CONN_SCRAPE"

# ── 判據 6：L4 UDP 那一格的 active 是從**工作階段表**派生的 ─────────────────────
#
# ★ ★ UDP 上沒有連接，只有工作階段。上面那個資料報剛建了一條工作階段（空閒超時 60s，
#   此刻它還在表裡）⇒ active 必須是 1。
# ⚠ ⚠ 這一條是**唯一**碰得到 `BoundConn::set_active` 的判據 —— 判據 2 走的是
#   `bump_total`，兩者有意是兩條不同的路。少了這一條，把循環開頭那行 `set_active`
#   整個刪掉**不會有任何東西紅**。
eq "★★★ 判据 6：发过一个数据报之后，l4_udp 那一格 active = 1（从 sessions.len() 派生）" \
  1 "$(conn_val "$SC1" fulcrum_connections_active l4_udp "$HOST:$L4U_PORT")"

# ── 判據 3：★★★ **握手中也算** —— Drop 守衛在非正常退出路徑上真的在守 ─────────
#
# 連上 TLS 連接埠**什麼都不發**並保持 ⇒ 那條連接卡在握手裡，而 enter 在握手之前
# ⇒ active 必須 +1。★ 這一條是「enter 在 spawn 之前」那條口徑的唯一實據，
#   也是「run_endpoint 裡那句 let _conn_guard 寫成裸 let _」唯一抓得住的地方。
BEFORE_ACTIVE=$(conn_val "$SC1" fulcrum_connections_active http "$HOST:$TLS_PORT")
python3 - "$HOST" "$TLS_PORT" "$WORK" <<'PY' &
import socket
import sys
import time

host, port, work = sys.argv[1], int(sys.argv[2]), sys.argv[3]
s = socket.create_connection((host, port), timeout=5)
# ⚠ 一个字节都不发：TLS 握手因此永远开始不了，那条连接停在握手里。
open(work + "/halfopen.ready", "w").close()
time.sleep(4)
s.close()
PY
HALF_PID=$!
for _ in $(seq 1 50); do [ -f "$WORK/halfopen.ready" ] && break; sleep 0.1; done
sleep 0.5
SC2="$WORK/sc2.txt"
scrape "$SC2" >/dev/null
DURING_ACTIVE=$(conn_val "$SC2" fulcrum_connections_active http "$HOST:$TLS_PORT")
eq "★★★ 判据 3：连上 TLS 端口什么都不发 ⇒ active 正好 +1（enter 在握手之前）" \
  1 "$((DURING_ACTIVE - BEFORE_ACTIVE))"
wait "$HALF_PID" 2>/dev/null || true
sleep 0.8
SC3="$WORK/sc3.txt"
scrape "$SC3" >/dev/null
eq "★★★ 判据 3（另一半）：那条连接断掉之后 active 回到基线（Drop 守卫收了它）" \
  "$BEFORE_ACTIVE" "$(conn_val "$SC3" fulcrum_connections_active http "$HOST:$TLS_PORT")"

# ── 判據 4：握手失敗 ⇒ total +1 而 active 回到基線 ──────────────────────────
BEFORE_T=$(conn_val "$SC3" fulcrum_connections_total http "$HOST:$TLS_PORT")
BEFORE_A=$(conn_val "$SC3" fulcrum_connections_active http "$HOST:$TLS_PORT")
python3 - "$HOST" "$TLS_PORT" <<'PY' || true
import socket
import sys

s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=5)
# ⚠ 一串不是 ClientHello 的垃圾 ⇒ TLS 握手当场失败。
s.sendall(b"\x00" * 64 + b"not-a-clienthello\r\n\r\n")
try:
    s.recv(1024)
except OSError:
    pass
s.close()
PY
sleep 0.8
SC4="$WORK/sc4.txt"
scrape "$SC4" >/dev/null
eq "★★★ 判据 4：往 TLS 端口发垃圾字节 ⇒ total +1（握手失败的连接也算接进来过）" \
  1 "$(($(conn_val "$SC4" fulcrum_connections_total http "$HOST:$TLS_PORT") - BEFORE_T))"
eq "★★★ 判据 4（另一半）：而 active 回到基线 —— 握手失败那条退出路径也被 Drop 守卫收了" \
  "$BEFORE_A" "$(conn_val "$SC4" fulcrum_connections_active http "$HOST:$TLS_PORT")"

# ── 判據 5：active ≤ total 逐格成立 ─────────────────────────────────────────
BAD=$(python3 - "$SC4" <<'PY'
import re
import sys

tot, act = {}, {}
pat = re.compile(r'^fulcrum_connections_(total|active)\{(.*?)\}\s+(\S+)')
for line in open(sys.argv[1], encoding="utf-8"):
    m = pat.match(line)
    if not m:
        continue
    kind, labels, val = m.groups()
    (tot if kind == "total" else act)[labels] = float(val)
bad = [k for k in act if act[k] > tot.get(k, -1)]
print(" ".join(bad))
PY
)
if [ -z "$BAD" ]; then
  ok "★★ 判据 5：active ≤ total 在**每一格**上都成立"
else
  fail "★★ 判据 5：这些格子上 active > total —— $BAD"
fi

# ⚠ ⚠ **兩條路徑有意不驗，理由寫在這裡**，免得下一個人當成漏了：
#   ① fork 的握手超時（60s，寫死）：它與握手失敗是**同一個 future 的兩種結束方式**，
#      而判據 4 已經走過那個 future 的非正常結束。等 60 秒買不到新資訊。
#   ② L4 UDP 的工作階段空閒超時（60s）：那一格是 sessions.len() 的**恆等式派生**
#      （一處寫入、一個表達式），且 UdpSessionTable::sweep 自己有注入時鐘的單測
#      「到點才回收而且回收的是空閒的那條」。
# ⛔ 另外不寫「sum(fulcrum_connections_total) 等於某個數」：抓取自己走 http，
#   每抓一次就 +1，那種判據會在自己身上紅。

echo
if [ "$FAILS" -ne 0 ]; then
  echo "METRICS TESTS FAILED：$FAILS 条断言没过。" >&2
  echo "--- 被测实例日志 ---" >&2
  tail -30 "$WORK/a.log" >&2 || true
  echo "--- 最后一次抓到的正文 ---" >&2
  tail -40 "$WORK/s9c.txt" >&2 || true
  echo "--- 最后一次 /stats 正文 ---" >&2
  cat "$WORK/ov_stats2.json" >&2 2>/dev/null || true
  echo "--- 访问日志 ---" >&2
  tail -10 "$LOGFILE" >&2 || true
  exit 1
fi
echo "METRICS TESTS PASSED —— Prometheus 指标真的在跑（格式合法 · 访问控制两个方向 · 没写 metrics 的站点拿不到 · 未知 Host 封顶且真的在数 · 两个族的总和对得上 · 指标与访问日志逐条对得上 · 两个「site」在同一条请求上给出不同的值 · 通配站点的两个子域名折叠成一格 · TLS 族只数 TLS 请求且 h1/h2/h3 各自与访问日志对得上、h3 那一格是 <unknown> 不是空串 · fulcrum_overrides_active 与 /stats 同源且悬空的照样计入 · status_class=none 那条路真的走得到且两边一起对得上 · 连接族在五个入口上都记到了同一个族里、握手中与握手失败两条路都被 Drop 守卫收住）。"
