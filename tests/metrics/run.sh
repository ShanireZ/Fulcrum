#!/usr/bin/env bash
# Prometheus 指标端到端（**M2 批 M**；G116–G121，裁决 R1–R8）。
#
# ★ ★ 为什么它是**独立一格**：与 `tests/log/` 同一个理由 —— 它验的不是「响应对不对」，
#   而是**响应之外的那份计数**。它有自己的格式契约（text exposition 0.0.4）、
#   自己的访问控制（`metrics` 是站点块里的终结指令，序号 75）、自己的基数上界。
#   ⚠ 混进 `tests/serve/run.sh` 的话，一条指标断言红了，读日志的人会先去看路由。
#
# ★ ★ ★ **判据挂在「谁在数、数出来的和别处对不对得上」，不挂在「抓到了东西」**：
#   一个把整份指标硬编码成字符串的实现，能通过每一条只问「正文里有没有那几个字」的判据。
#   ⇒ 本格的重量全在**增量**与**交叉对照**上：
#     · 增量：抓两次做差（⚠ 计数发生在 `Record::finish`，即**响应写完之后**
#       ⇒ 第 N 次抓取看到的是前 N−1 次的量）；
#     · 交叉对照：同一批请求，指标那一侧的增量必须与**访问日志**那一侧的新增行数对得上。
#
# 端口（★ 与其余场景全都错开，见 AGENTS.md 那张端口表）：
#   9920 站点 A —— **两条地址** `a.example` / `a2.example`（★ G121 的正面判据要第二条）；
#        配 `log { output file … level info }`（一致性门要它记全部）；
#        `@internal { remote_ip 127.0.0.0/8 · path /metrics }` + `handle @internal { metrics }`
#        + 兜底 `respond 403`。
#   9921 站点 B —— `b.example`，**整块里一个 `metrics` 都没有**（反向判据 ① 要它）。
#   9921 站点 C —— `c.example`，`@outsider remote_ip 10.0.0.0/8` + `handle @outsider { metrics }`
#        + 兜底 `respond 403`。★ 我们打不进 10.0.0.0/8 ⇒ 拿到的是 403 而不是指标
#        （访问控制的**反向**那一半，在容器里真的造得出来的形状）。
#   9921 站点 W —— `*.wild.example`，**一条通配地址**（G121 的折叠判据要它，判据 6）。
#   9922 站点 T —— `a.example:9922` + `tls`（**M2 批 M′ 任务 3**，`fulcrum_tls_requests_total`）。
#        ★ G110 让有 TLS 的端口在**同一个端口号**上自动听 UDP ⇒ h1 / h2 / h3 三条路
#          都在这一个端口上量，不另起服务。
#   9923 **不是枢衡的监听器**，是一个 python 慢上游（**M2 批 M′ 任务 4**，G124）：
#        睡 1.5s 才回话，而且**有意不带 `Content-Length`**。理由见本文件末尾那一节。
#
# ⚠ ⚠ ★ **站点 W 补的是夹具形状上的洞，不是多一条覆盖。** 另外四条地址全是**精确**
#   字面量 ⇒ 对每一条命中站点的请求，`ctx.host` 与命中的那条地址**逐字相等**。
#   于是一个把 `site` 标签取成 `ctx.host` 的实现（G121 明令禁止的正是这一个）
#   在本格别的每一条判据上都是绿的 —— 连那条「取值闭集」也绿，因为两者给出同一个集合。
#   ★ ★ **盲区在夹具里，而判据的条数说明不了这件事**（AGENTS.md「Gate discipline」第一条）。
#
# ⚠ ⚠ **`@internal` 里那条 `path /metrics` 是有意加的，不是从 brief 抄漏的**：
#   只写 `remote_ip 127.0.0.0/8` 的话，本格所有请求都来自 127.0.0.1 ⇒ 站点 A 上
#   **任何路径都会命中 `metrics`**，兜底的 `respond 403` 一次也走不到 ——
#   而一致性门要求同一个站点上至少出现 `metrics` 与 `respond` 两种 `outcome`。
#   ★ 顺带：它让「站点 B 上打**同一路径**」这句话有了确定的所指。
#
# ⚠ ⚠ **本场景仍然不碰共享的 `:80`，而现在它需要一句显式的话才成立。**
#   前四个站点全写 `http://` ⇒ 天然不合成重定向站点；而站点 T 写的是
#   `a.example:9922`（没写 scheme ⇒ 按 `compile.rs` 的规则推成 `https` ⇒ `auto_https`）
#   ——那本来**会**让 `synthesize_http_redirect` 给它合成一个 `:80` 的 308 站点，
#   于是本格变成 AGENTS.md 那张表上第八个隐式绑 `:80` 的场景。
#   ⇒ 三份配置的全局块都写 `auto_http_redirect false` 把它按住。
#   ★ 那一行今天**不改变任何既有判据**（前四个站点本来就不合成）——它守的正是
#     站点 T 这一条，⛔ 别当成可以顺手删掉的样板。
#   ★ cleanup 照旧断言 9920/9921/9922 都已经还回去
#   （照 `tests/quic-relay/run.sh`：判据挂在「端口还回去了没有」，不挂在「进程还在不在」）。

set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
A_PORT=${A_PORT:-9920}
# ★ 站点 B 与站点 C **共用**这一个端口：站点按 `(host, port)` 匹配，同端口不同主机名
#   是合法的两个站点。⇒ 反向判据 ① 与访问控制的反向半边在同一个监听器上量，
#   「监听器不一样」这条解释被排除掉了。
BC_PORT=${BC_PORT:-9921}
# 站点 T：TLS 那一格（M2 批 M′ 任务 3）。★ 同一个端口号上同时有 TCP 与 UDP（G110）
#   ⇒ h1 / h2 / h3 三条路共用它。
TLS_PORT=${TLS_PORT:-9922}
# status_class="none" 那条路（M2 批 M′ 任务 4，G124）要的**慢上游**。
# ⚠ 它不是枢衡的监听器，但它照样占着一个端口 ⇒ 进 AGENTS.md 指的那张端口表，
#   也进 cleanup 那份「走的时候还回去了没有」的单子。
ABORT_UP_PORT=${ABORT_UP_PORT:-9923}
# ── 连接指标（M2 批 O，G122 的连接那半）用的 L4 两格 ────────────────────────
# ★ ★ 为什么 l4 也要摆进本场景：这个族的核心断言是「**四个互不相干的 accept 循环
#   都记进了同一个族，而 entrypoint 分得开**」—— 那句话只有在同一个进程里同时有
#   五种入口（http / admin / quic / l4_tcp / l4_udp）时才验得了。拆到 tests/l4 去，
#   它就永远验不到。
# ★ L4 TCP 的上游复用 $ABORT_UP_PORT（那个慢上游是个 TCP 监听器，透传够用）；
#   UDP 另起一个回显上游。
L4T_PORT=${L4T_PORT:-9924}
L4U_PORT=${L4U_PORT:-9925}
UDPUP_PORT=${UDPUP_PORT:-9926}
LOGFILE="$WORK/access.json"
EXPO="$WORK/expo.py"
# 抓取路径。★ 它同时是「站点 B 上打的同一路径」。
MPATH=/metrics
# ── fulcrum_overrides_active（M2 批 N 任务 6.5，G126）用：管理面 Unix socket ──
#   ★ 不需要新端口（G14：admin 只走 unix socket）——不占用 AGENTS.md 那张端口表。
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

  # ── ★ ★ 收尾自证：本场景用过的端口，走的时候必须全部还回去 ──────────────────
  #
  # 照 `tests/quic-relay/run.sh` 那段。⚠ 它守的是**上面那个 `PIDS` 收全了没有**，
  # 而判据挂在「端口还回去了没有」而不是「进程还在不在」：前者才是下一个场景真会
  # 被绊到的东西，后者要先知道该找哪个 pid —— 而一个没被登记的 pid 恰恰最找不到。
  # ⚠ 本格不碰 `:80`：前四个站点都是 `http://`，而 `auto_https` 的站点 T 由全局
  #   `auto_http_redirect false` 按住（见文件头）⇒ 单子里没有它。
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
    # ★ 用 `pgrep -af` 而不是 `ps | grep`：一个 defunct 进程攥不住监听 socket，
    #   所以这里要的只是「还活着的是哪几个」——而那正好是 pgrep 答得了的。
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

# 访问日志那一侧的两个读数。★ 与 `tests/log/run.sh` 逐字同一套写法。
lines() { wc -l < "$LOGFILE" | tr -d ' '; }

# 取访问日志**最后一行**的某个字段。⚠ 用 python3 而不是 grep：一个
# `grep '"site":"http://a.example:9920"'` 会在字段顺序、空格、转义任一处变化时静默漏判。
field() {
  python3 -c '
import json, sys
line = open(sys.argv[1], encoding="utf-8").read().strip().split("\n")[-1]
o = json.loads(line)
k = sys.argv[2]
print(o[k] if k in o else "<缺>")
' "$LOGFILE" "$1"
}

# 一条普通请求，只回 HTTP 码。$1 端口 · $2 Host · $3 路径。
req() {
  curl -sS -o /dev/null -w '%{http_code}' --max-time 5 -H "Host: $2" \
    "http://$HOST:$1$3" 2>/dev/null || echo 000
}

# 一条请求，正文落到 $4、响应头落到 $4.hdr，回 HTTP 码。
get() {
  curl -sS -o "$4" -D "$4.hdr" -w '%{http_code}' --max-time 5 -H "Host: $2" \
    "http://$HOST:$1$3" 2>/dev/null || echo 000
}

# 抓一次指标（Host: a.example，来源自然是 127.0.0.1 ⇒ `@internal` 命中）。
scrape() { get "$A_PORT" a.example "$MPATH" "$1"; }

# 响应头里 `Content-Type` 的**原文**。★ 不走 curl 的 `%{content_type}`：那是 curl
# 规整过的，而这一格判的恰恰是**我们写出去的那一串逐字对不对**（抓取端按它挑解析器）。
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

# ★ ★ ★ 「捕获一个可能失败的命令」的唯一写法，连同它为什么存在，都在
#   `tests/lib/capture.sh` 里（任务 7 从本文件与 `tests/serve/run.sh` 各一份收敛而来）。
# shellcheck source=tests/lib/capture.sh
. "$REPO/tests/lib/capture.sh"

# 一个数值断言。$1 说明 · $2 期望 · $3 实际。
eq() {
  if [ "$2" = "$3" ]; then
    ok "$1（$3）"
  else
    fail "$1：期望「$2」实际「$3」"
  fi
}

# ── exposition 的读取端 ─────────────────────────────────────────────────────
#
# ★ ★ **判据要问的是「这个 series 的值涨了多少」，而不是「正文里有没有那几个字」**：
#   后者对「把指标硬编码成一段常量」的实现完全无效。⇒ 需要一个真的解析器。
# ⚠ 它自己也可能坏。坏了的表现是**恒答 0 / 恒答有**，而那会让下面每一条判据变成空转
#   ⇒ 它带一个 `selftest`，每次开跑先证「它命中得了，也落空得了」。
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

# ── [0/8] 基线 ──────────────────────────────────────────────────────────────
#
# ★ ★ 三条自证：端口没被占（否则量的是别人的服务）；访问日志此刻不存在
#   （否则「多了几行」分不出「刚写的」与「本来就有的」）；读取端量得了东西。
echo "=== [0/8] 基线：端口空着 · 日志文件还不存在 · exposition 读取端自证 ==="
for p in "$A_PORT" "$BC_PORT" "$TLS_PORT" "$L4T_PORT" "$ABORT_UP_PORT" "$UDPUP_PORT"; do
  if port_listening "$p"; then
    echo "METRICS TESTS FAILED: 端口 $p 已经被占，本次结果不可采信。" >&2
    exit 1
  fi
done
# ⛔ 不在这句话里写个数：那个计数一道门都没有，加一个端口时它当场过期而不会红。
# ⚠ $L4U_PORT 与 $UDPUP_PORT 是 UDP，`port_listening` 是 TCP 探测 ⇒ 探不到它们，
#   基线里有意不查（查了也恒为「空着」，那是一条读起来成立而实际空转的断言）。
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

# ── 配置 ────────────────────────────────────────────────────────────────────
#
# ⚠ ⚠ **`admin unix/$ADMIN_SOCK` 从第一代就开着**（G126 那节要用它 POST /runtime
#   与 GET /stats）：admin 是启动时才绑的监听（与数据面监听器同一条纪律），
#   POST /load 换不了它 —— 必须在这里，不能等后面 [reload] 再加。
#   ★ 它是一个独立的 Unix socket 监听，不影响这份配置上任何一条既有判据
#   （不产生新的 `site`、不产生新的 `reverse_proxy`）。

# ── 自签证书（站点 T 要，M2 批 M′ 任务 3）─────────────────────────────────
#
# ⚠ SAN 必须有 `a.example`：枢衡按**证书自己的 SAN** 决定这张证书装在哪些 SNI 上。
# ★ 与 `tests/log/run.sh` 那段逐字同一套（现签，⛔ 不入库）。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=a.example" \
  -addext "subjectAltName=DNS:a.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "METRICS TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ⚠ ⚠ ⚠ **站点 T 必须出现在下面**三份**配置里**（`a` / `ov` / `ov-dangling`）。
#   `POST /load` 明确拒绝监听端口集发生变化（`admin.rs`：「监听端口集变了，本进程
#   换不了」，回 409）⇒ 少写一份，后面那两次 reload 会 409，**而红的地方会在
#   `fulcrum_overrides_active` 那一节**，指向一个与真正原因完全无关的方向。
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

# 一份**没有任何来源匹配器**的 metrics 配置 —— 只用来证那条诊断真的会说话。
cat > "$WORK/unguarded.Fulcrumfile" <<CONF
http://a.example:$A_PORT {
    metrics
}
CONF

# ── [1/8] 起服务 ────────────────────────────────────────────────────────────
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

# ★ 那条诊断的**两个方向**：本格这份配置圈住了来源 ⇒ 不该报；
#   而一份没圈的配置**必须**报。⚠ 只验前者的话，一条从来不会说话的诊断也全绿 ——
#   而 G116 明写「matcher 写错就会把指标暴露出去，只能靠文档与诊断兜」。
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

# ── [2/8] 判据 1：格式 ──────────────────────────────────────────────────────
echo "=== [2/8] 判据 1：抓到的正文是合法 exposition ==="
#
# ⚠ ⚠ **先打三条预热请求再抓**：计数发生在 `Record::finish`（响应写完之后）
#   ⇒ 第一次抓取的正文里 `fulcrum_requests_total` **一条样本都没有**，
#   而「四个标签都在」这条判据在没有样本时会退化成空转。
CODE=$(req "$A_PORT" a.example "/")
eq "预热①：站点 A 上非 /metrics 的路径走兜底 respond" 403 "$CODE"
CODE=$(req "$A_PORT" nobody-warm.invalid "$MPATH")
eq "预热②：9920 上的未知 Host ⇒ 无站点匹配" 421 "$CODE"
# ★ 预热③是**已知地址字面量、错端口**：`b.example` 只配在 9921 上 ⇒ 9920 上它无站点匹配，
#   而它**是**一条地址字面量 ⇒ G118 给它自己一格，而不是并进 `<other>`。
#   ⚠ 少了它，判据 4 里「series 条数不增长」在一个「把 host 恒写成 <other>」的实现上也全绿。
CODE=$(req "$A_PORT" b.example "$MPATH")
eq "预热③：已知字面量打错端口 ⇒ 无站点匹配" 421 "$CODE"
sleep 0.2

S1="$WORK/s1.txt"
CODE=$(scrape "$S1")
eq "抓取端点回 200" 200 "$CODE"

CT=$(content_type "$S1.hdr")
eq "★★ Content-Type 逐字" "text/plain; version=0.0.4; charset=utf-8" "$CT"

# 每个族的 `# HELP` / `# TYPE` 一个都不许少。
#
# ⛔ **这里不写族的个数**：一个写在注释里的计数没有任何门守着，加族时它当场过期
#   而不会红（批 N 任务 8 因此把这类计数从五个文件里全部拿掉了）。
#   ★ 下面那条 `# TYPE` 行数比对**就是**那个计数，而它是从真实抓取里数出来的。
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

# ★ ★ ★ **反向那半：抓取里不许有这张单子之外的族。**
#
# ⚠ ⚠ 少了它，上面那个循环只证明「单子里的都在」—— 新增一个族却忘了写进单子时
#   **一条都不会红**，而这个文件正是那种遗漏唯一会露头的地方
#   （`FAMILIES` ↔ 基数表那道门在单测里，它看不见这份手抄单子）。
# ★ 判据是**数出来的**：`# TYPE` 一族一行，拿它与单子的条数比。
TYPE_LINES=$(grep -c '^# TYPE ' "$S1")
WANT_FAMILIES=$(echo "$FAMILIES_EXPECTED" | wc -w)
eq "★★★ 抓取里的族数与上面这张单子逐个对得上（多一个族没写进单子就红）" \
  "$WANT_FAMILIES" "$TYPE_LINES"

# ★ 整份正文过一遍结构检查：每条样本都有声明 · 没有重复的 series · counter 名字带 `_total`。
#   ⚠ 「重复的 series」这一条尤其值钱：抓取端会把它算成两条，而正文读起来完全正常。
LINT=$(expo lint "$S1" || true)
if [ -z "$LINT" ]; then
  ok "★★ 整份 exposition 结构检查通过（声明齐全 · 无重复 series · counter 名字带 _total）"
else
  fail "★★ exposition 结构检查没过：$(echo "$LINT" | tr '\n' '；')"
fi

# `fulcrum_requests_total` 的**每一条**样本都必须正好带那四个标签。
# ★ 判的是**键的集合逐字相等**，不是「含有」——「含有」在多出一个 `uri` 标签时照样绿，
#   而「任何形态都不加 uri 标签」是 G116 那张基数表写死的一条。
KEYS=$(expo labelkeys "$S1" fulcrum_requests_total)
eq "★★★ fulcrum_requests_total 的标签键集合（每条样本都一样）" \
  "outcome,proto,site,status_class" "$KEYS"

# ★ ★ 上游那两族：**族在、样本无**。
#   本配置里一个 `reverse_proxy` 都没有 ⇒ 它们没有任何数据源。
#   ⚠ 这一条分得开「没接上」与「没数据」：族都不出现才是没接上，而那正是
#   「一个指标存在、只是还没发生过」与「这个指标根本没做」之间唯一的区别。
for f in fulcrum_upstream_inflight fulcrum_upstream_healthy; do
  eq "★★★ $f 有 HELP/TYPE 但零条样本（没有上游 ⇒ 没数据，不是没接上）" \
    0 "$(expo series "$S1" "$f")"
done

# ★ ★ ★ fulcrum_overrides_active（G126，批 N 任务 6.5）：与上面两族**相反**——
#   它是**无标签**的单值 gauge，即便这一刻登记处一项覆盖都没有，也必须
#   **出一条样本**（值是 0），而不是跟着「没数据」一起整族消失。⚠ 这是判据
#   写法纪律的第 4 条：0 与「不存在」在这个族身上必须能分得开——族仍然出现，
#   只是恰好一项覆盖都没有。
#   ⚠ ⚠ 先证「样本真的存在」再问「值是多少」：`expo sum` 在「一条样本都没有」
#   与「样本存在、值恰好是 0」两种情况下算出的都是 0（没有匹配项时 `sum` 的
#   初值就是 0）——单独看 sum 挡不住「count==0 就整族不出样本」这种实现。
eq "★★★ fulcrum_overrides_active 基数恒为 1（判据 5，0 覆盖时的基线；顺带证明判据 4 的样本真的存在）" \
  1 "$(expo series "$S1" fulcrum_overrides_active)"
eq "★★★ fulcrum_overrides_active 此刻 0 覆盖，值是 0（判据 4：族仍出现，不是整族消失）" \
  0 "$(expo sum "$S1" fulcrum_overrides_active)"
eq "★★★ fulcrum_overrides_active 不带任何标签（判据 1）" \
  "" "$(expo labelkeys "$S1" fulcrum_overrides_active)"

# ── [3/8] 判据 2：访问控制的两个方向 ────────────────────────────────────────
echo "=== [3/8] 判据 2：够得着的抓得到，够不着的拿到 403 ==="
if grep -q '^# TYPE ' "$S1"; then
  ok "正向：127.0.0.1 打站点 A ⇒ 200 且正文真的是指标（有 # TYPE 行）"
else
  fail "正向：抓到的正文里没有 # TYPE 行 —— 那它不是一份 exposition"
fi

# 反向：站点 C 把 `metrics` 圈在 10.0.0.0/8 里，而我们从 127.0.0.1 打过去 ——
# ⇒ `@outsider` 不命中，落到兜底那条 `respond 403`。
# ★ 这是本格里**在容器里真的造得出来**的那个反向形状：不需要第二块网卡，
#   只需要一个我们不可能来自的网段。
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

# ── [4/8] 判据 3（反向 ①）：没写 metrics 的站点 ─────────────────────────────
echo "=== [4/8] 判据 3：站点 B 没写 metrics ⇒ 同一路径拿到的不是指标 ==="
#
# ★ ★ ★ 少了这一条，「抓到了指标」证明不了是 `metrics` 这条指令干的 ——
#   一个「见到 /metrics 就渲染指标」的实现（把端点做成了硬编码路径）在上面每一条判据上全绿。
B_OUT="$WORK/b.txt"
CODE=$(get "$BC_PORT" b.example "$MPATH" "$B_OUT")
eq "站点 B 上打 $MPATH 回 200" 200 "$CODE"
if grep -q '^# TYPE ' "$B_OUT"; then
  fail "★★★ 没写 metrics 的站点上竟然抓到了指标 —— 那个端点不是这条指令给的"
else
  ok "★★★ 没写 metrics 的站点：同一路径的正文里一个 # TYPE 都没有"
fi
eq "★★ 而它给的是自己那条 respond 的正文" "b-ok" "$(cat "$B_OUT")"

# ── [5/8] 判据 4（反向 ②）：50 个互不相同的未知 Host ────────────────────────
echo "=== [5/8] 判据 4：未知 Host 打 50 次 ⇒ series 不增长，而那一格正好 +50 ==="
#
# ⚠ ⚠ **两句都要断言**。只断言前半（「series 条数不增长」）的话，
#   一个「计数器根本没在加」的实现也能过 —— 而那时 series 条数当然不增长。
S2="$WORK/s2.txt"
CODE=$(scrape "$S2")
eq "抓取回 200（取基线）" 200 "$CODE"
N1=$(expo series "$S2" fulcrum_no_site_match_total)
V1=$(expo sum "$S2" fulcrum_no_site_match_total 'host=<other>')
W1=$(expo sum "$S2" fulcrum_no_site_match_total host=b.example)
# ★ 尺子自证：两格都得有值 —— 一个把 host 恒写成 `<other>` 的实现在这里当场红，
#   而它在「series 不增长」那一句上是全绿的。
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

# ── [6/8] 判据 5：一致性门 ──────────────────────────────────────────────────
echo "=== [6/8] 判据 5：指标的增量与访问日志新增的行数逐条对得上 ==="
#
# ★ ★ ★ 它守的是「两处不分家」：`fulcrum_requests_total` 与访问日志那一行
#   由**同一个 `Record::finish`** 喂出来。⚠ 各算各的话，两个数字都会言之凿凿、
#   却对不上 —— 而那正是 D18 / G66 那个形状。
#
# ⚠ 三件要算清楚的事：
#   ① 站点 A 配的是 `level info` ⇒ 它记全部（`LogLevel::All`）；
#   ② `no_site_match` 的请求**不属于任何站点** ⇒ 记不进访问日志，它们在指标里是
#      `site="<none>"` —— **两边都不算它**；
#   ③ 抓取那一条请求自己也会被计一次、也会被记一行（`outcome="metrics"`），
#      而计数在**渲染之后** ⇒ 第 N 次抓取看到的是前 N−1 次的量。⇒ 抓两次做差。
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

# 一串**已知形状**的请求：站点 A 上两种 outcome 各两条，外加一条不属于任何站点的。
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
# ⇒ 指标那一侧多出来的是：k1 k2 k3 k5 这 4 条，**加上 S4 那次抓取自己**（它在渲染之后才计）。
eq "★★★ fulcrum_requests_total 在站点 A 上的增量 = 日志新增行数 + 1（+1 是左端那次抓取自己）" \
  "$((L_B - L_A + 1))" "$((M_B - M_A))"
eq "★★ 而 site=<none> 那一格只涨了 1（k4），它一行日志都没多" \
  1 "$(($(expo sum "$S5" fulcrum_requests_total 'site=<none>') - NONE_A))"

# ── ★ ★ ★ 顺带把**两个族**的数对起来 ────────────────────────────────────────
#
# `fulcrum_no_site_match_total`（G118，记在写 `outcome` 的同一处）与
# `fulcrum_requests_total{site="<none>",outcome="no_site_match"}`（收尾那一处）
# 数的是**同一批事件**，只是前者按 `host` 分、后者按 `status_class` / `proto` 分
# ⇒ **两个族的总和必须恒等**。而它们由两个文件里的两条语句各加一次。
#
# ⚠ 今天两处一致 —— 这条断言防的不是今天，是**将来多一条「不算命中站点」的早退路径**
#   （批 N 的 stats 端点、第二种 421 场景……）时有人只写了其中一句：
#   两个数字各自都在涨、各自都言之凿凿，而从此差一个常数，**没有任何东西会说**。
#   ★ 这正是 D18 / G66 那个形状：批 M 为请求那两族躲开了它，却在这两族之间留了同一个口子。
# ★ 上面两节把两个读数都已经量过了（判据 4 量前者、本节量后者）——**只差这一条把它俩对起来**。
# ⚠ 判据读的是**同一份抓取正文**，不是两次抓取：两句 `inc` 一前一后（一句在早退处、
#   一句在收尾），跨两次抓取比会把一条在途请求读成假红。
eq "★★★ 两个族数的是同一批事件：sum(no_site_match_total) = requests_total{site=<none>,outcome=no_site_match}" \
  "$(expo sum "$S5" fulcrum_no_site_match_total)" \
  "$(expo sum "$S5" fulcrum_requests_total 'site=<none>' outcome=no_site_match)"

# ── [7/8] 判据 6：G121 的正面判据 ───────────────────────────────────────────
echo "=== [7/8] 判据 6：两个「site」给出不同的值 · 通配站点折叠成自己的字面量 ==="
#
# ★ ★ ★ 这是本批**最容易被做成同一件事**的地方：两者共用「site」这个名字纯属巧合。
#   · 指标的 `site` 标签（G121）= 请求**实际命中的那条地址字面量** ⇒ `a2.example`；
#   · 访问日志的 `site` 字段（R3）= **站点的名字** = 第一个地址的原文 ⇒ `http://a.example:9920`。
#   ⚠ 一个把指标的 site 直接取自日志那个字段的实现，在别的每一条判据上都是绿的。
S6="$WORK/s6.txt"
CODE=$(scrape "$S6")
eq "抓取回 200（取 a2.example 那一格的基线）" 200 "$CODE"
X1=$(expo sum "$S6" fulcrum_requests_total site=a2.example)
WILD1=$(expo sum "$S6" fulcrum_requests_total 'site=*.wild.example')

CODE=$(req "$A_PORT" a2.example "/g121")
eq "用 Host: a2.example 打一条（落到兜底 respond）" 403 "$CODE"
sleep 0.3

# 同一条请求的**日志那一面**：`host` 是 a2.example，而 `site` 仍是第一条地址的原文。
eq "★★★ 这条请求的日志行：host" "a2.example" "$(field host)"
eq "★★★ 这条请求的日志行：site（= 站点的名字 = 第一条地址的原文）" \
  "http://a.example:$A_PORT" "$(field site)"

# ── ★ ★ ★ 通配站点：两个不同的子域名，指标上必须折叠成同一格 ────────────────
#
# ★ ★ ★ **这几行守的是 `site_addr` 那个赋值点本身**（`fulcrum-server/src/lib.rs` 里
#   `session.record.site_addr = Some(routed.site_addr.clone())` —— 整批里 `site` 标签
#   **唯一**的赋值点）。把它换成 `Arc::from(host.as_str())`，运行时单测（够不到 server 这一层）、
#   access_log 单测（夹具自己塞值，绕开了这一行）与本文件其余每一条判据**全都仍然绿**：
#   四条精确地址上 `host` 与地址字面量逐字相等，两者在那份配置上是同一个字符串。
# ⚠ ⚠ **必须两个不同的子域名各打一次**：只打一个的话，「取 host」在那一次上也只产生
#   一格 —— 闭集断言照样绿，错的只是那一格的名字，而名字对不对没有第二条判据在问。
#   ⇒ 两个子域名之后，「取 host」的实现会多出**两格**，三条断言一起红。
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

# ★ ★ 封口：`site` 这个标签**出现过的全部取值**是一个闭集，正好 = 五条地址字面量 + `<none>`。
#   ⇒ ① 同一个站点的两条地址各自成格（G121，不并成一格）；
#      ② 日志那种带 scheme 与端口的写法**一次都没出现**在标签里；
#      ③ 通配站点占的是**它自己那条字面量**那一格，两个子域名各自都不在集合里；
#      ④ 上界 = 地址数 + 1，由配置定、不由访问者定（R2）。
#   ⚠ 判的是**集合逐字相等**，不是「含有 a2.example」——「含有」在多冒出一格时照样绿。
eq "★★★ site 标签的取值是闭集：五条地址字面量 + <none>" \
  "*.wild.example <none> a.example a2.example b.example c.example" \
  "$(expo labelvalues "$S7" fulcrum_requests_total site | tr '\n' ' ' | sed 's/ $//')"

# ── [8/8] 收尾前的一条：抓取自己也被计进去了 ────────────────────────────────
echo "=== [8/8] 抓取端点自己也在被计（它不是一条特权请求）==="
#
# ★ 它把「第 N 次抓取看到的是前 N−1 次的量」这句话钉成判据：`outcome="metrics"`
#   那一格必须随着抓取次数增长。⚠ 一个「抓取不计数」的实现会让上面那条一致性门
#   的 `+1` 变成 `+0`，而那条判据会红得像是别的地方出了问题 —— 这里说清楚它。
Y1=$(expo sum "$S7" fulcrum_requests_total site=a.example outcome=metrics)
S8="$WORK/s8.txt"
CODE=$(scrape "$S8")
eq "抓取回 200" 200 "$CODE"
eq "★★ outcome=metrics 那一格随抓取增长（上一次抓取被这一次看见了）" \
  1 "$(($(expo sum "$S8" fulcrum_requests_total site=a.example outcome=metrics) - Y1))"

# ── fulcrum_tls_requests_total：走 TLS 的请求（M2 批 M′ 任务 3，G122 / G127）──
#
# ★ ★ ★ 这一族的判据**全部挂在「与访问日志那一行读出来的值对得上」上**，
#   ⛔ 不挂在写死的套件名上：把 `TLS_AES_256_GCM_SHA384` 写进判据，等于让这道门
#   在 BoringSSL 换一次默认套件时红，而那时红的地方指向的是一个完全正确的实现。
#   ⇒ 每一条 TLS 请求先从日志里读出这条连接真的协商出了什么，再拿它去指标里找那条 series。
#
# ⚠ ⚠ **⛔ 这里没有、也不许有「这个族的总数 = requests_total 的总数」那种判据**：
#   明文请求的 `record.tls` 是 `None` ⇒ 不计（G122 / 计划 S5）。那条判据会在第一条
#   明文请求上红，而那条请求什么问题都没有。★ 说得出口的关系只有「小于等于」。
echo "=== fulcrum_tls_requests_total（M2 批 M′ 任务 3，G122 的 TLS 半 / G127）==="

# 一条走 TLS 的请求。$1 = 路径，其余 = 额外的 curl 参数。回 HTTP 码。
# ⚠ `-k`：证书是现签自签的。⚠ `--resolve`：SNI 要是 `a.example`（证书的 SAN 就是它）。
tls_req() {
  local path=$1
  shift
  curl -sS -o /dev/null -w '%{http_code}' -k --max-time 5 \
    --resolve "a.example:$TLS_PORT:$HOST" "$@" \
    "https://a.example:$TLS_PORT$path" 2>/dev/null || echo 000
}

# 这个族此刻的总数。★ 只有一个取数处，写成函数免得三处各抄一遍。
tls_total() { expo sum "$1" fulcrum_tls_requests_total; }

# ── ★ 夹具自证：`--http3-only` 真的在走 QUIC，不是悄悄回落到 TCP ─────────────
#
# ⚠ ⚠ `--http3-only` 不许写成 `--http3`：后者在 QUIC 不通时**回落到 TCP**，
#   于是下面 h3 那半会在「h3 根本没起来」时照样全绿（本仓在 tests/h3/run.sh 里
#   白纸黑字记过这一条）。★ 对着一个**没有 UDP 监听**的端口（站点 A 是纯 HTTP）
#   打一次，它必须失败 —— 少了这条，一个「curl 没编进 QUIC 后端」的镜像会让
#   h3 那一整半变成空转。
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

# ── ① 明文请求**一笔都不记** ────────────────────────────────────────────────
CODE=$(req "$BC_PORT" b.example "/p1"); eq "  明文 1（站点 B）" 200 "$CODE"
CODE=$(req "$BC_PORT" b.example "/p2"); eq "  明文 2（站点 B）" 200 "$CODE"
CODE=$(req "$BC_PORT" b.example "/p3"); eq "  明文 3（站点 B）" 200 "$CODE"
sleep 0.3
ST1="$WORK/st1.txt"
CODE=$(scrape "$ST1"); eq "抓取回 200" 200 "$CODE"
# ★ 先自证这三条请求真的被数到了 —— 否则下面那条「TLS 族没涨」在一个
#   「三条请求根本没发出去」的世界里也全绿。⚠ 4 = 3 条明文 + ST0 那次抓取自己
#   （计数在渲染之后 ⇒ 第 N 次抓取看到的是前 N−1 次的量）。
eq "★ 夹具自证：requests_total 涨了 4（3 条明文 + ST0 那次抓取自己）" \
  4 "$(($(expo sum "$ST1" fulcrum_requests_total) - R0))"
eq "★★★ 而 TLS 族一笔都没涨 —— 明文请求的 record.tls 是 None（G122，⛔ 别写成两族总数相等）" \
  0 "$(($(tls_total "$ST1") - T0))"

# ── ② h1 与 h2 各一条：分开断，⛔ 不合成一条 ────────────────────────────────
#
# ⚠ ⚠ 合成一条（「打两次、涨 2」）的话，一个**只在 h2 上记**的实现照样全绿：
#   两条请求走的是同一份 `{version,cipher}`，两次 +1 与一次 +2 在总数上分不开。
#   ⇒ 一条一条打、一条一条断。
# 打一条 TLS 请求并把它量完。$1 = 这一格的名字（也用作抓取文件名与路径）·
# $2 = curl 的协议开关 · $3 = 期望的 `proto` 字段 · $4 = 上一份抓取正文（做差的左端）。
# ★ 量完把这一次的抓取正文路径写进 `$LAST_SCRAPE`，给下一格当左端。
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
  # ★ 夹具自证：h1/h2 上套件**真的问得出来** —— 否则下面那条交叉断言会退化成
  #   「<unknown> 那一格涨了 1」，而它在一个什么都读不出来的实现上也全绿。
  if [ -n "$cipher" ] && [ "$cipher" != "<缺>" ] && [ "$cipher" != "<unknown>" ]; then
    ok "  ★ 夹具自证：$name 上真的协商出了套件（$cipher），不是 <unknown>"
  else
    fail "  ★ $name 上 tls_cipher 读不出真值（拿到「$cipher」）—— 交叉断言会退化成空转"
  fi
  here="$WORK/st-$name.txt"
  code=$(scrape "$here")
  eq "  抓取回 200" 200 "$code"
  eq "★★★ $name：TLS 族正好 +1" 1 "$(($(tls_total "$here") - $(tls_total "$prev")))"
  # ★ ★ ★ 交叉对照：涨的那一笔必须落在**访问日志刚刚报出来的那一对值**上，
  #   ⛔ 不是落在写死的套件名上，也不是「随便哪条 series 涨了」。
  before=$(expo sum "$prev" fulcrum_tls_requests_total "version=$ver" "cipher=$cipher")
  after=$(expo sum "$here" fulcrum_tls_requests_total "version=$ver" "cipher=$cipher")
  eq "★★★ $name：涨的正好是 {version=$ver, cipher=$cipher} 那一条（与访问日志同一对值）" \
    1 "$((after - before))"
  LAST_SCRAPE="$here"
}

# ⚠ ⚠ 两格**分开打、分开断**：合成一条（「打两次、涨 2」）的话，一个**只在 h2 上记**
#   的实现照样全绿 —— 两条请求走的是同一份 `{version,cipher}`，两次 +1 与一次 +2
#   在总数上分不开。
tls_case http1.1 --http1.1 "HTTP/1.1" "$ST1"
tls_case h2 --http2 "HTTP/2.0" "$LAST_SCRAPE"
ST2="$LAST_SCRAPE"

# ── ③ h3：取不到的那一格记 `<unknown>`，⛔ 不记空串（G127）─────────────────
CODE=$(tls_req "/t-h3" --http3-only)
eq "  TLS 请求（h3）回 200" 200 "$CODE"
sleep 0.3
eq "  ★ 它真的走的是 HTTP/3.0" "HTTP/3.0" "$(field proto)"
# RFC 9001 §4.2：QUIC 只能用 TLS 1.3。★ 这是规范，不是我们猜的。
eq "  ★ h3 的 tls_version 是 TLSv1.3（RFC 9001 §4.2）" "TLSv1.3" "$(field tls_version)"
# ⚠ ⚠ **两侧处置不同，这里一并钉住**：访问日志里那一格**不出现**（契约：取不到的
#   字段不出现）；而指标里它必须是 `<unknown>`（指标没有「那一格不出现」这回事）。
#   ★ 少了上面这半，「日志与指标是同一套处置」这句假话没有任何东西挡得住。
eq "★★★ h3：访问日志里 tls_cipher **不出现**（取不到的字段不出现）" "<缺>" "$(field tls_cipher)"
ST3="$WORK/st-h3.txt"
CODE=$(scrape "$ST3"); eq "  抓取回 200" 200 "$CODE"
eq "★★★ h3：TLS 族正好 +1" 1 "$(($(tls_total "$ST3") - $(tls_total "$ST2")))"
UNK_BEFORE=$(expo sum "$ST2" fulcrum_tls_requests_total version=TLSv1.3 'cipher=<unknown>')
UNK_AFTER=$(expo sum "$ST3" fulcrum_tls_requests_total version=TLSv1.3 'cipher=<unknown>')
eq "★★★ h3：涨的正好是 {version=TLSv1.3, cipher=<unknown>} 那一条（G127）" \
  1 "$((UNK_AFTER - UNK_BEFORE))"

# ── ④ ★ ★ ★ G127 的正主：这个族里**一个空标签值都不许有** ──────────────────
#
# ⚠ ⚠ 在 Prometheus 数据模型里空标签值等同于「这个标签不存在」⇒ 记空串的话，
#   「今天这一格取不到」与「明天把 cipher 整个删掉」在抓取端**分不开**；
#   而 `cipher=""` 恰好是 PromQL 里「该标签不存在」的惯用写法。
# ★ ★ 这一条**不看是不是 h3**：它问的是「有没有哪条 series 的哪一格是空的」——
#   与 G127 把判据落在「值为空」上、⛔ 不落在「是不是 h3」上逐字同一个形状。
# ⚠ 取样前先自证样本里真有东西：一个恒答空集的读法会让下面那条恒绿。
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

# ── ⑤ 标签键逐字，⛔ 不许多出 sni / alpn ────────────────────────────────────
#
# ★ 判的是**键的集合逐字相等**（`labelkeys` 打的是排序后的键名）：写成「含有
#   version 与 cipher」的话，多出一个 `sni` 标签照样绿 —— 而那正是 G122 明令
#   挡住的东西（`sni`/`alpn` 是**访问者给的**，与 G121 挡住 host 是同一件事）。
eq "★★★ 这个族只带 {cipher, version} 两个标签（⛔ 不许有 sni / alpn）" \
  "cipher,version" "$(expo labelkeys "$ST3" fulcrum_tls_requests_total)"

# ── ⑥ 与 requests_total 的关系：**小于**，⛔ 不是相等 ───────────────────────
TLS_SUM=$(tls_total "$ST3")
REQ_SUM=$(expo sum "$ST3" fulcrum_requests_total)
if [ "$TLS_SUM" -gt 0 ] && [ "$TLS_SUM" -lt "$REQ_SUM" ]; then
  ok "★★★ sum(tls_requests_total)=$TLS_SUM 严格小于 sum(requests_total)=$REQ_SUM（明文请求不计，G122）"
else
  fail "★★★ 期望 0 < tls_requests_total($TLS_SUM) < requests_total($REQ_SUM) —— \
明文请求被计进 TLS 族了，或者 TLS 请求一条都没被计"
fi

# ── fulcrum_overrides_active：与 GET /stats 同源、悬空的照样计入 ───────────
#   （M2 批 N 任务 6.5，G126，裁决 R13）
#
# ⚠ ⚠ ⚠ 判据 1/2/3/5 必须从真的 /metrics 出口上抓，不许只在 Rust 单测里
#   直接调渲染函数（任务 4 的教训：16 条判据全直接调 handler，删掉路由那一行
#   21 条判据照样全绿）。/stats 那一侧同理必须走真 admin socket，不直接调
#   `AdminApp::stats()`——否则「路由真的接在生产会走的那条线上」这件事
#   本身就没有判据在守。
echo "=== fulcrum_overrides_active（M2 批 N 任务 6.5，G126，R13）==="

admin_post() {
  # $1 = 路径，$2 = body。回状态码，正文落 $WORK/admin.out（与 tests/serve/run.sh 同一个约定）。
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

admin_get() {
  # $1 = 路径。回状态码，正文落 $WORK/admin.out。
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X GET \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# /stats 那一侧只服务本节：overrides 数组的总条目数、其中 dangling=true 的条数。
# ★ 与 tests/serve/run.sh 的 stats_check.py 是同一种分工（判据本身落在 bash 这层
# 的 eq 上，python3 只负责把 JSON 读成一个数）。
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
# ★ 沿用任务 6 报告点名过的那个「幽灵端口」惯例：19999 从不监听，只用来登记
#   覆盖键，不需要真的发起连接——不在 AGENTS.md 的端口表里，因为它从不是
#   真正被占用的端口。
OV_UP="127.0.0.1:19999"

# ★ ★ 两代配置都在**原有四个站点原封不动**的基础上，只新增 `ov.example` 这
#   一个站点——不碰 A_PORT/BC_PORT 已经在监听的那两个监听器（`listen_ports`
#   只按「端口 + 是否 TLS」比较，加一个新主机名不会让它们对不上而撞 409）。
#   ⚠ ⚠ 不能把这个站点摆进**第一代**：那样一来 [2/8] 那条「没有 reverse_proxy
#   ⇒ upstream_inflight/healthy 零样本」的判据就会被这里的真实上游破坏——
#   「两族族在、样本无」这条判据与本节的覆盖夹具互斥，天然只能分两代摆。
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

# 同样的骨架，但 `ov.example` 只留默认 handle 那一条路——写了 `id ova` 的那把
# 键因此悬空，没写 id 的那把仍然活着（与 tests/serve/run.sh 的 ov-dangling
# 手法同一个形状）。
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

# ── 判据 4（更强的一版）：这一刻真的有 reverse_proxy 了，但一项覆盖都还没设 ──
#   ⇒ fulcrum_overrides_active 仍然是 0——证明它数的是「覆盖」，不是「随便什么
#   跟 reverse_proxy 沾边的东西」。
S9A="$WORK/s9a.txt"
CODE=$(scrape "$S9A")
eq "重新抓取回 200" 200 "$CODE"
# ⚠ ⚠ `expo sum` 在「一条样本都没有」与「样本存在、值是 0」两种情况下算出
#   的都是 0（没有匹配项时 `sum` 的初值就是 0）——光看 sum 挡不住「count==0
#   就整族不出样本」这种实现。⇒ 先用 `series` 证「样本真的存在」，sum 那条
#   才谈得上是在量「值」而不是在量一个巧合。
eq "★★★ fulcrum_overrides_active 此刻仍出一条样本（不是被 0 值悄悄吞掉）" \
  1 "$(expo series "$S9A" fulcrum_overrides_active)"
eq "★★★ 有上游了，但还没设覆盖 ⇒ fulcrum_overrides_active 仍是 0" \
  0 "$(expo sum "$S9A" fulcrum_overrides_active)"

# ── 摆两把覆盖键：没写 id 的那把（之后仍然活着）+ id=ova 的那把（之后会悬空）──
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$OV_SITE\",\"upstream\":\"$OV_UP\"}]}")
eq "POST /runtime：摘掉默认 handle 那条路（没写 id）" 200 "$CODE"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$OV_SITE\",\"id\":\"ova\",\"upstream\":\"$OV_UP\"}]}")
eq "POST /runtime：摘掉 id=ova 那条路" 200 "$CODE"

# ── 判据 2（第一次，0 悬空）：GET /stats 与 GET /metrics 同一时刻各抓一次 ──
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

# ── 换成只留默认路那条的配置 ⇒ id=ova 那把键悬空，没写 id 的那把仍然活着 ──
CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov-dangling.json")")
eq "overrides=keep：换成不带 id=ova 那条路的配置" 200 "$CODE"

# ── 判据 2（第二次，1 悬空）+ 判据 3：悬空的照样计入 ─────────────────────
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
# ⚠ ⚠ 判据写法纪律：总数（2）与悬空数（1）必须不相等，否则一个把两者读串了
#   的实现（比如把 fulcrum_overrides_active 接成悬空数、或接成「非悬空数」）
#   在这份夹具上也可能蒙对——上面这条断言先把「$ST2_TOTAL != $ST2_DANGLING」
#   钉死，下面两条才真的分得清读对了还是读串了。
if [ "$ST2_TOTAL" = "$ST2_DANGLING" ]; then
  fail "夹具写法纪律没守住：总数与悬空数相等（$ST2_TOTAL），两者会被读串也测不出来"
else
  ok "★ 夹具写法纪律：总数（$ST2_TOTAL）与悬空数（$ST2_DANGLING）不相等"
fi
eq "★★★ 判据 3：悬空的照样计入——fulcrum_overrides_active 是总数 2，不是悬空数 1" \
  2 "$M2_VALUE"
eq "★★★ 判据 2：换代之后仍与 /stats 的 overrides 总条目数同源（1 悬空这一刻）" \
  "$ST2_TOTAL" "$M2_VALUE"

# ── 判据 5（非退化版）：登记处里明明有 2 项覆盖，正文里仍然只有一行 ──────────
eq "★★★ 判据 5：即便有 2 项覆盖，fulcrum_overrides_active 也只出一条 series（基数恒为 1）" \
  1 "$(expo series "$S9C" fulcrum_overrides_active)"
eq "★ 那一行不带任何标签" "" "$(expo labelkeys "$S9C" fulcrum_overrides_active)"

# ── status_class="none"：那条路真的走得到（M2 批 M′ 任务 4，G124 / D30 结案）──
#
# ★ ★ ★ G124 明写这一格要**两边一起断**：访问日志真的多了一行 `status=0`，
#   且 `fulcrum_requests_total{status_class="none"}` 正好 +1。
#   ⇒ 在这之前，闭集里的第六个值只存在于代码与文档里，**一条端到端判据都没走过**。
#
# ⚠ ⚠ ⚠ **「下游断开」本身不足以让 status 变成 0**，而那句话读起来像是够的。
#   开工前的探针实测（三种构型，同一棵树同一天）：
#     · `respond` 站点 + 发完请求立刻 RST                      ⇒ status **200**
#     · 慢上游（**带** Content-Length）+ RST 之后 1.2s 才写     ⇒ status **200**（resp_size 是 0）
#     · 慢上游（**不带** Content-Length）+ RST                  ⇒ status **0** ✅
#
#   机制在 pingora 的 `write_response_header`（`v1/server.rs`）：响应头 `write_all`
#   进的是**带缓冲的流**，而 flush 只在「1xx 或**没有** Content-Length」时才发生
#   ⇒ 带 CL 的响应头**根本不产生 syscall**，`write_all` 返回 Ok，`response_written`
#   照样被置上，于是 status 是 200。★ 只有会 flush 的那一支才真的碰 socket，
#   也只有它会在一条被 RST 掉的连接上失败 —— 失败了 `response_written` 才留在 None。
#
# ⇒ 夹具用的是**不带 Content-Length 的慢上游**：它把「服务端此刻还一个字节都没写」
#   与「写的时候真的会有一次 syscall」两件事同时钉住 —— ⛔ **这不是一场竞赛**。
# ★ 对照组（同一条路、客户端正常收完）拿到 200，那证明这把尺子量得了两边。
echo "=== status_class=\"none\"（M2 批 M′ 任务 4，G124 / D30 结案）==="

# 慢上游：睡 1.5s，再回一个**不带 Content-Length** 的响应（关闭定界）。
# ⚠ 每条连接一个线程：`wait_port` 的探测连接会被 accept 一次，单线程的话它会把
#   accept 循环占住 1.5s，真正那条请求就排在后面了。
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

# 客户端。$1 = read | rst · $2 端口 · $3 路径 · $4 Host。
# ⚠ curl 做不了这件事：要的是 **SO_LINGER(1,0) + close ⇒ 立刻发 RST**，
#   而不是正常的 FIN —— 半关之下服务端第一次写照样成功（探针实测过）。
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
# ⛔ 这句话有意**只说端口起来了**：「它睡 1.5s 且不带 Content-Length」是夹具的性质，
#   写进这一行就成了一句**没有任何东西验证的散文** —— 实测过：给上游加回
#   `Content-Length` 之后这一行照样打印「且不带 Content-Length」，而下面三条判据红。
# ★ 那两个性质**有门守着，只是门在下面**：带了 CL 的话 status 就是 200，
#   「那一行的 status 是 0」当场红。⇒ 属性由判据说，不由这一行说。
ok "慢上游起来了（$ABORT_UP_PORT）"

SN0="$WORK/sn0.txt"
CODE=$(scrape "$SN0"); eq "抓取回 200（none 那一格的基线）" 200 "$CODE"
NONE0=$(expo sum "$SN0" fulcrum_requests_total status_class=none)
sleep 0.3
LN0=$(lines)

# ── ★ 对照组先跑：同一条路、客户端**正常收完** ⇒ 200，而 none 那一格不涨 ──────
#   ⚠ 少了它，下面那两条在一个「status 恒为 0」的实现上也全绿。
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
# ⚠ 基线要在**这次抓取自己那一行**写进去之后再取：抓取也是一条被记的请求，
#   拿 LN1 当左端的话，下面那条「多了一行」会算成 2。
LN1B=$(lines)

# ── ★ ★ ★ 正主：发完请求立刻 RST，而上游还要 1.5s 才回话 ────────────────────
capture_ok "abort 请求（发完立刻 RST）" python3 "$WORK/abort_req.py" rst "$A_PORT" /abort a.example
sleep 2.5
LN2=$(lines)
eq "★★★ G124 ①：访问日志真的多了一行（站点匹配上了 ⇒ 它记得进去）" 1 "$((LN2 - LN1B))"
eq "★★★ G124 ①：那一行的 status 是 0 —— 一个响应头都没写出去" 0 "$(field status)"
# ⚠ ⚠ 这一条守的是契约里那句「此时 outcome 仍是执行链给的那个值」。
# ★ 它立起来时防的是 `Record` 上那个 `aborted` 默认值；那个字段今天已经删掉
#   （outcome 改成 serve_one 的返回值）⇒ 防的东西在类型上没有了，而**判据留着**：
#   它守的是「链给的值不被收尾那一步覆盖掉」，那件事与字段在不在无关。
eq "★★★ 而 outcome 仍是执行链给的那个值" reverse_proxy "$(field outcome)"
eq "★ 那一行也有 site（它属于站点 A，所以才记得进访问日志）" \
  "http://a.example:$A_PORT" "$(field site)"
SN2="$WORK/sn2.txt"
CODE=$(scrape "$SN2"); eq "抓取回 200" 200 "$CODE"
eq "★★★ G124 ②：fulcrum_requests_total{status_class=\"none\"} 正好 +1" \
  1 "$(($(expo sum "$SN2" fulcrum_requests_total status_class=none) - NONE1))"
# ★ 两边一起断的最后一句：那一笔落在**站点 A** 上，而不是 site=<none> ——
#   ⚠ 它证明「日志有那一行、指标有那一笔」说的是**同一条请求**，不是两件碰巧同时发生的事。
NONE_A_BEFORE=$(expo sum "$SN1" fulcrum_requests_total site=a.example status_class=none)
NONE_A_AFTER=$(expo sum "$SN2" fulcrum_requests_total site=a.example status_class=none)
eq "★★★ 而且那一笔落在 site=a.example 上（与日志那一行是同一条请求）" \
  1 "$((NONE_A_AFTER - NONE_A_BEFORE))"

# ── 连接那两个族（M2 批 O，G122 的连接那半）─────────────────────────────────
#
# ★ ★ ★ 这一节的核心断言只有一句：**四个互不相干的 accept 循环都记进了同一个族，
#   而 entrypoint 分得开。** 五种入口（http / admin / quic / l4_tcp / l4_udp）
#   必须在**同一个进程**里同时存在，那句话才验得了 —— 这就是 l4 块摆进本场景的理由。
#
# ⚠ 判据一律**从夹具派生** listen 的值，⛔ 不写死：它随 --bind-host 变，
#   而 admin 那条是每次跑都不同的 $WORK/admin.sock。
echo "=== 连接那两个族（M2 批 O，G122 的连接那半）==="

# UDP 回显上游（L4 UDP 那一格要）。★ 接住 SIGINT/SIGTERM 干净退出，
# 否则收尾那段要走到 kill -9，日志末尾会多出一行「Killed」被读成「有东西崩了」。
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

# 这个族此刻的某一格。$1 抓取文件 · $2 族名 · $3 entrypoint · $4 listen。
conn_val() { expo sum "$1" "$2" "entrypoint=$3" "listen=$4"; }
# 这个族此刻的全部 (entrypoint,listen) 对，排序后一行一个。
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

# ── 判据 1：series 集合 == 本进程的监听器集合（逐字，从夹具派生）──────────────
#
# ★ 一条断言同时验掉两件事：「建成就出样本」（不是有连接才出现）与
#   「**四处都记进了同一个族**」—— 少接一处，这一条当场少一行。
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

# ── 判据 2：五个 entrypoint **各打一条连接 ⇒ 那一格 total 正好 +1** ───────────
#
# ⚠ ⚠ **分开打、分开断**：合成一条（「打五次、涨 5」）的话，一个只在 http 上记的
#   实现照样全绿 —— 与批 M′ 任务 3 里 h1/h2 那条同一形状。
conn_case() {
  # $1 说明 · $2 entrypoint · $3 listen · $4 上一份抓取 · 其余 = 打一条连接的命令
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

# ⚠ http 那一格：抓取自己也走 http ⇒ 用一条**别的** http 请求量它会多算抓取那一笔。
#   ⇒ 这一格改成量 BC_PORT（抓取走的是 A_PORT），两者是不同的监听器、不同的 series。
conn_case "l4_tcp：连一下 L4 TCP 端口" l4_tcp "$HOST:$L4T_PORT" "$SC0" \
  timeout 3 bash -c "exec 3<>/dev/tcp/$HOST/$L4T_PORT"
conn_case "http：打一条到站点 B 的请求" http "$HOST:$BC_PORT" "$LAST_CONN_SCRAPE" \
  curl -sS --max-time 5 -H "Host: b.example" -o /dev/null "http://$HOST:$BC_PORT/conn"
conn_case "admin：一次 GET /stats" admin "$ADMIN_SOCK" "$LAST_CONN_SCRAPE" \
  curl -sS --max-time 5 --unix-socket "$ADMIN_SOCK" -o /dev/null "http://localhost/stats"
conn_case "quic：一次 --http3-only" quic "$HOST:$TLS_PORT" "$LAST_CONN_SCRAPE" \
  curl -sS -o /dev/null -k --max-time 5 --http3-only \
    --resolve "a.example:$TLS_PORT:$HOST" "https://a.example:$TLS_PORT/conn-h3"
# ⚠ L4 UDP 那一格的 total 是**事件点**（新建会话那一处），active 才是派生的。
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

# ── 判据 6：L4 UDP 那一格的 active 是从**会话表**派生的 ─────────────────────
#
# ★ ★ UDP 上没有连接，只有会话。上面那个数据报刚建了一条会话（空闲超时 60s，
#   此刻它还在表里）⇒ active 必须是 1。
# ⚠ ⚠ 这一条是**唯一**碰得到 `BoundConn::set_active` 的判据 —— 判据 2 走的是
#   `bump_total`，两者有意是两条不同的路。少了这一条，把循环开头那行 `set_active`
#   整个删掉**不会有任何东西红**。
eq "★★★ 判据 6：发过一个数据报之后，l4_udp 那一格 active = 1（从 sessions.len() 派生）" \
  1 "$(conn_val "$SC1" fulcrum_connections_active l4_udp "$HOST:$L4U_PORT")"

# ── 判据 3：★★★ **握手中也算** —— Drop 守卫在非正常退出路径上真的在守 ─────────
#
# 连上 TLS 端口**什么都不发**并保持 ⇒ 那条连接卡在握手里，而 enter 在握手之前
# ⇒ active 必须 +1。★ 这一条是「enter 在 spawn 之前」那条口径的唯一实据，
#   也是「run_endpoint 里那句 let _conn_guard 写成裸 let _」唯一抓得住的地方。
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

# ── 判据 4：握手失败 ⇒ total +1 而 active 回到基线 ──────────────────────────
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

# ── 判据 5：active ≤ total 逐格成立 ─────────────────────────────────────────
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

# ⚠ ⚠ **两条路径有意不验，理由写在这里**，免得下一个人当成漏了：
#   ① fork 的握手超时（60s，写死）：它与握手失败是**同一个 future 的两种结束方式**，
#      而判据 4 已经走过那个 future 的非正常结束。等 60 秒买不到新信息。
#   ② L4 UDP 的会话空闲超时（60s）：那一格是 sessions.len() 的**恒等式派生**
#      （一处写入、一个表达式），且 UdpSessionTable::sweep 自己有注入时钟的单测
#      「到点才回收而且回收的是空闲的那条」。
# ⛔ 另外不写「sum(fulcrum_connections_total) 等于某个数」：抓取自己走 http，
#   每抓一次就 +1，那种判据会在自己身上红。

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
