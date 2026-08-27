#!/usr/bin/env bash
# 冒烟：**对着一个已经在跑的枢衡**逐项体检（M1 退出条件第 2 条的一半）。
#
#   bash tests/smoke/run.sh https://example.com           # 真域名（上线时用这个）
#   bash tests/smoke/run.sh http://127.0.0.1:9200       # 本地实例（门禁里用这个）
#   SMOKE_REQUIRE_OFFBOX=1 bash tests/smoke/run.sh https://example.com   # ★ 上线核验：必须从机器之外跑
#
# ★ ★ ★ **从哪台机器跑，决定了这份清单证明的是什么。** 实测过一次：机器的公网入口上
#   有一个按明文 `Host` 与 SNI 拦截的中间设备，而**从机器内跑，这两样都不出网**，
#   它根本不在路径里 —— 同一份清单机内跑与机外跑验的是两件事，而结语说的是同一句话。
#   ⇒ 本脚本自己判**观察点**并写进开头与结语；上线核验加 `SMOKE_REQUIRE_OFFBOX=1`，
#     那时机内观察判红。⚠ 默认不判红是有意的：机内跑仍然回答着「本机这个服务好不好」。
#
# ★ ★ **它与门禁里其余场景分工不同。**
#   其余场景**自己起服务、自己造夹具**，验的是「代码写对了没有」。
#   本脚本**什么都不起**，它对着一个**别人部署好的**枢衡问：这台机器现在是好的吗？
#   ⇒ 它是 M1 退出条件里那句「两个真域名由枢衡承载、跑通冒烟」唯一能落地的东西：
#     那句话没法靠门禁里的夹具证明，只能靠一份**能对着真域名跑的清单**。
#
# ★ 判据分两档，因为对真域名与对本地实例能问的问题不一样：
#   · **必查**：任何一个跑着的枢衡都该满足（连得上、回得对、头对、不泄漏版本）。
#   · **HTTPS 档**：目标是 https:// 时才查（证书链、有效期、SNI、HTTP→HTTPS 跳转）。
#
# ⚠ **它有意不做「性能」判断**。压力那半边在 tests/stress/run.sh，
#   而任何性能声明都要走 §8 / G19 的口径（三家对拍、逐类设门、脚本与原始数据公开）。
set -euo pipefail

TARGET=${1:-}
if [ -z "$TARGET" ]; then
  echo "用法：bash tests/smoke/run.sh <目标 URL>（如 https://example.com）" >&2
  exit 2
fi
TARGET=${TARGET%/}

# 期望的最短剩余证书有效期（天）。★ 不是「没过期就行」：ACME 的价值在于
# **在到期前很久就换掉**，而一张只剩 3 天的证书说明续期那条路没在工作。
MIN_CERT_DAYS=${MIN_CERT_DAYS:-20}

FAILS=0
CHECKS=0
ok() { CHECKS=$((CHECKS + 1)); echo "  ✓ $*"; }
bad() { CHECKS=$((CHECKS + 1)); FAILS=$((FAILS + 1)); echo "  ✗ $*" >&2; }
note() { echo "  · $*"; }

# ── 前置：量东西的家伙自己得在 ─────────────────────────────────────────────
#
# ★ 少了这一条，`curl` 不在时每一项都会「失败」，而报出来的结论会指向被测的那台机器。
#   （在 systemd 场景上真的踩过一次：镜像里没有 curl，
#     而脚本报的是「数据面本身就不对」。）
command -v curl >/dev/null 2>&1 || {
  echo "SMOKE FAILED: 本机没有 curl —— 冒烟的每一项都量不了。" >&2
  exit 1
}
HAVE_OPENSSL=1
command -v openssl >/dev/null 2>&1 || HAVE_OPENSSL=0

SCHEME=${TARGET%%://*}
HOSTPORT=${TARGET#*://}
HOST=${HOSTPORT%%[:/]*}

echo "=== 冒烟：$TARGET ==="
note "curl $(curl --version | head -1 | cut -d' ' -f1-2)"
if [ "$HAVE_OPENSSL" = 1 ]; then
  note "openssl $(openssl version)"
else
  note "没有 openssl —— 证书那几项会**跳过并点名**，不是静默略过"
fi

# ── 1. 连得上，而且回的是 HTTP ─────────────────────────────────────────────
#
# ⚠ ⚠ **不能写成 `curl … -w '%{http_code}' || echo "000"`。**
#   curl 连不上时 `-w` **已经输出了 `000`**，`|| echo` 再补一个 ⇒ 结果是 `000000`，
#   于是 `[ "$CODE" = "000" ]` 不成立，脚本会带着一个六位数继续往下跑。
#   ★ 同样的坑 `tests/serve/run.sh` 里也白纸黑字记着 ——
#     **「先弄清工具已经做了什么，再决定补什么」**。
#   ⚠ 而 `|| true` 又是必须的：`set -e` 下 `X=$(失败的命令)` 会让脚本**当场退出**，
#     于是「连不上」这件事根本走不到下面那句 bad —— 判据在它该说话之前就没了。
#     ★ 两个坑方向相反：`|| echo 000` 是往输出里补，`|| true` 是接住退出码。补错了地方就白搭。
# ★ 顺手把**对端 IP** 一起量出来：下面「观察点」那一条要靠它，而它不值得多打一发请求。
#   ⚠ 两个 `-w` 字段之间隔一个空格；`-o /dev/null` 保证 stdout 上只有 `-w` 的输出。
#   ⚠ 连不上时 curl 仍会写出 `000 `（remote_ip 是空的）—— 所以下面要按「有没有空格」拆，
#     `${PROBE#* }` 在没有空格时会原样回整个串，那会把 `000` 当成一个 IP。
PROBE=$(curl -sS -o /dev/null -w '%{http_code} %{remote_ip}' --max-time 10 "$TARGET/" 2>/dev/null) || true
CODE=${PROBE%% *}
REMOTE_IP=""
case "$PROBE" in *' '*) REMOTE_IP=${PROBE#* } ;; esac
# ★ ★ **「连不上」与「证书验不过」是两个不同的事故，修法完全不同**，不许合并：
#   前者要去看进程／防火墙／DNS，后者要去看 ACME 与证书链。
#   而 curl 对这两种情况给的都是 `000` —— 所以再问一次「绕过校验能不能连上」。
#   ⇒ 是证书的问题就**继续往下跑**，让 HTTPS 档给出精确诊断；真连不上才当场停。
INSECURE=()
if [ -z "$CODE" ] || [ "$CODE" = "000" ]; then
  if [ "$SCHEME" = "https" ] && curl -sSk -o /dev/null --max-time 10 "$TARGET/" 2>/dev/null; then
    bad "TLS 那一层没过（加 -k 就连得上）—— 具体是哪一条见下面的 HTTPS 档"
    # ⚠ 余下几项改用 -k，并且**在报告里说明它们是绕过校验跑的**。
    #   不这么做的话，后面每一项都会以「连不上」的形式红，把真正的原因埋掉。
    INSECURE=(-k)
    note "★ 下面的头／路由／keep-alive 三项是**绕过证书校验**量的（否则它们只会重复报同一个错）"
  else
    bad "根路径连不上（curl 拿不到任何状态码）—— 后面每一项都不用看了"
    echo
    echo "SMOKE FAILED: 目标根本连不上"
    exit 1
  fi
else
  ok "根路径连得上，HTTP $CODE"
fi

# ── 1.5 观察点：这一轮是**站在哪里**量的 ───────────────────────────────────
#
# ★ 判据挂在**对端 IP** 上，而不是挂在「目标里写的是不是域名」上：
#   `https://example.com` 在被测机器上跑，对端 IP 就是它自己 —— 而那正是要认出来的那种情况。
# ⚠ ⚠ **拿不准的时候要说出拿不准**：容器里通常没有 `ip`（iproute2 只装在 systemd 测试镜像里），
#   那时只能按「不是回环」判 —— 这条结论比「在本机地址表里查过」弱，**弱在哪要写在输出里**，
#   不许伪装成一个确定结论。
local_addrs() {
  if command -v ip >/dev/null 2>&1; then
    ip -o addr show 2>/dev/null | awk '{print $4}' | cut -d/ -f1
    return 0
  fi
  if command -v hostname >/dev/null 2>&1 && hostname -I >/dev/null 2>&1; then
    hostname -I | tr ' ' '\n'
    return 0
  fi
  return 1
}
OBSERVER=$(hostname 2>/dev/null | head -1)
[ -n "$OBSERVER" ] || OBSERVER="?"
OBSERVE=unknown
OBSERVE_WHY=""
case "$REMOTE_IP" in
  '')
    OBSERVE_WHY="没量到对端 IP"
    ;;
  127.* | ::1)
    OBSERVE=onbox
    OBSERVE_WHY="对端是回环地址 $REMOTE_IP"
    ;;
  *)
    if ADDRS=$(local_addrs); then
      if printf '%s\n' "$ADDRS" | grep -qxF "$REMOTE_IP"; then
        OBSERVE=onbox
        OBSERVE_WHY="对端 $REMOTE_IP 就在本机地址表里"
      else
        OBSERVE=offbox
        OBSERVE_WHY="对端 $REMOTE_IP 不在本机地址表里"
      fi
    else
      OBSERVE=offbox
      OBSERVE_WHY="⚠ 本机既没有 ip 也没有 hostname -I，只能按「$REMOTE_IP 不是回环」判 —— 这条比查过地址表弱"
    fi
    ;;
esac
case "$OBSERVE" in
  onbox) OBS_LABEL="机内" ;;
  offbox) OBS_LABEL="机外" ;;
  *) OBS_LABEL="未知" ;;
esac
note "观察点：$OBS_LABEL（$OBSERVER → $TARGET；$OBSERVE_WHY）"

# ── 2. 响应头：不许泄漏版本 ────────────────────────────────────────────────
#
# ★ 判据是**不存在**，而这类判据最容易写成恒真。所以先证明这次真的取到了头。
HDRS=$(curl -sS "${INSECURE[@]}" -D - -o /dev/null --max-time 10 "$TARGET/" 2>/dev/null || true)
if [ -z "$HDRS" ]; then
  bad "一个响应头都没取到 —— 下面「不泄漏版本」那条判据本次无效"
else
  ok "取到了 $(printf '%s\n' "$HDRS" | grep -c ':') 个响应头（下面那条「不存在」判据才有意义）"
  if printf '%s\n' "$HDRS" | grep -qiE '^server:.*(nginx|caddy|apache)/[0-9]'; then
    bad "Server 头里带了后端的版本号：$(printf '%s\n' "$HDRS" | grep -i '^server:' | tr -d '\r')"
  else
    ok "Server 头没有泄漏后端版本号"
  fi
fi

# ── 3. 未知 Host → 421（G63）────────────────────────────────────────────────
#
# ★ 这一条同时验了「站点索引真的按 Host 查」。⚠ 它对**本地实例**才稳定：
#   真域名前面可能还有 CDN / 防火墙，它们会先一步拦掉。所以真域名上只记录不判红。
UNKNOWN=$(curl -sS "${INSECURE[@]}" -o /dev/null -w '%{http_code}' --max-time 10 \
  -H "Host: no-such-site.invalid" "$TARGET/" 2>/dev/null) || true
case "$HOST" in
  127.0.0.1 | localhost | ::1)
    if [ "$UNKNOWN" = "421" ]; then
      ok "未知 Host → 421（G63：不静默交给某个站点）"
    else
      bad "未知 Host → $UNKNOWN，期望 421"
    fi
    ;;
  *)
    note "未知 Host → $UNKNOWN（真域名上只记录不判红：前面可能还有 CDN／防火墙）"
    ;;
esac

# ── 4. HTTPS 档 ────────────────────────────────────────────────────────────
if [ "$SCHEME" = "https" ]; then
  # ★ 端口跟着 TARGET 走，不写死 443：门禁里的自证跑在 9211 上，
  #   而写死 443 会让证书那几项**去问一台根本不是被测目标的机器** ——
  #   它多半连不上，于是报出来的是「取不到证书」，指向一个不存在的问题。
  case "$HOSTPORT" in
    *:*) TLS_HOSTPORT=${HOSTPORT%%/*} ;;
    *) TLS_HOSTPORT="${HOST}:443" ;;
  esac
  # 4a. 证书链**能被默认信任库验过**。★ 不加 -k：那正是这一条要验的东西。
  if curl -sS -o /dev/null --max-time 10 "$TARGET/" 2>/dev/null; then
    ok "证书链被系统信任库验过（没有用 -k 绕过）"
  else
    bad "证书链验不过 —— 浏览器会红。用 \"curl -v $TARGET/\" 看具体原因"
  fi

  # 4b. 剩余有效期。★ 判据不是「没过期」，是「还剩得够多」：
  #     一张只剩几天的证书说明**续期那条路没在工作**，而那时它还没坏。
  if [ "$HAVE_OPENSSL" = 1 ]; then
    END=$(echo | openssl s_client -connect "$TLS_HOSTPORT" -servername "$HOST" 2>/dev/null \
          | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2 || true)
    if [ -n "$END" ]; then
      END_TS=$(date -d "$END" +%s 2>/dev/null || echo 0)
      NOW_TS=$(date +%s)
      if [ "$END_TS" -gt 0 ]; then
        DAYS=$(( (END_TS - NOW_TS) / 86400 ))
        if [ "$DAYS" -ge "$MIN_CERT_DAYS" ]; then
          ok "证书还剩 ${DAYS} 天（下界 ${MIN_CERT_DAYS} 天）"
        else
          bad "证书只剩 ${DAYS} 天（下界 ${MIN_CERT_DAYS} 天）—— 续期多半没在工作，而它现在还没坏"
        fi
      else
        bad "证书到期时间解析不出来：$END"
      fi
    else
      bad "取不到证书到期时间（openssl s_client 连不上 $TLS_HOSTPORT？）"
    fi

    # 4c. ★ 未知 SNI 必须**拒绝握手**，不能回落到某张证书。
    #     ⚠ 回落会让服务端挑一张客户端拒绝的证书，而服务端日志里只有一次**成功**的握手。
    if echo | openssl s_client -connect "$TLS_HOSTPORT" -servername "no-such.invalid" \
         >/dev/null 2>&1; then
      bad "未知 SNI 握手**成功**了 —— 它应当被拒绝（回落会让服务端日志看起来一切正常）"
    else
      ok "未知 SNI 被拒绝握手"
    fi
  else
    bad "没有 openssl —— 证书有效期与未知 SNI 这两项**没能检查**（不当成通过）"
  fi

  # 4d. HTTP → HTTPS 跳转（自动 HTTPS 的可见部分）
  PLAIN="http://${HOSTPORT}"
  RCODE=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 10 "$PLAIN/" 2>/dev/null) || true
  case "$RCODE" in
    30*) ok "http:// 被 $RCODE 跳到 https://" ;;
    000) note "http:// 连不上（80 端口没开也可能是有意的，只记录）" ;;
    *) note "http:// 回 $RCODE，没有跳转（只记录：是否该跳取决于配置）" ;;
  esac
fi

# ── 5. keep-alive：同一条连接上连打两次 ────────────────────────────────────
#
# ★ 判据取 `num_connects`：curl 在一次调用里请求两个 URL，复用了连接的话它是 1。
#   ⚠ 只看两次都 200 的话，一个每次都重建连接的实现也全绿。
# ⚠ ⚠ **`-o` 与 `-w` 都是按 URL 生效的。** 只给一个 `-o /dev/null` 的话，
#   第二个 URL 的**响应体**会混进 `-w` 的输出 —— 实测拿到过 `1smoke-ok0` 这种东西，
#   而它长得就像一个数字。⇒ 每个 URL 各给一个 `-o`，并让 `-w` 每次换行。
# ★ 判据取**第二次**的 `num_connects`：它必须是 0（复用了第一条连接）。
#   这比「总数是 1」更准 —— 后者在只发出一次请求时也成立。
NCONN=$(curl -sS "${INSECURE[@]}" -o /dev/null -o /dev/null -w '%{num_connects}\n' --max-time 15 \
  "$TARGET/" "$TARGET/" 2>/dev/null | tr -d '\r') || true
N1=$(printf '%s\n' "$NCONN" | sed -n '1p')
N2=$(printf '%s\n' "$NCONN" | sed -n '2p')
if [ "$N1" = "1" ] && [ "$N2" = "0" ]; then
  ok "keep-alive 生效（第一次建连 1，第二次复用 0）"
else
  bad "两次请求的 num_connects 是「$N1」「$N2」，期望 1 与 0 —— keep-alive 没生效"
fi

echo
# ★ ★ ★ **观察点进结语**（§10 登记、落地）。
#   在此之前，机内跑与机外跑的结语**逐字相同** —— 而它们证明的不是同一件事。
if [ "$FAILS" -ne 0 ]; then
  echo "SMOKE FAILED: $CHECKS 项里有 $FAILS 项不通过（$TARGET；观察点：$OBS_LABEL）" >&2
  exit 1
fi
if [ "${SMOKE_REQUIRE_OFFBOX:-0}" = "1" ] && [ "$OBSERVE" != "offbox" ]; then
  echo "SMOKE FAILED: $CHECKS 项全过，但**观察点是「$OBS_LABEL」**（$OBSERVE_WHY）。" >&2
  echo "  要求机器外的观察点（SMOKE_REQUIRE_OFFBOX=1）时，机内 / 未知都不算数：" >&2
  echo "  从被测机器上跑，明文 Host 与 SNI 都不出网，路径上的中间设备根本不在判据里。" >&2
  exit 1
fi
echo "SMOKE PASSED —— $CHECKS 项全过（$TARGET；观察点：$OBS_LABEL，$OBSERVER）"
if [ "$OBSERVE" != "offbox" ]; then
  echo "  ⚠ 观察点是「$OBS_LABEL」⇒ 这一轮**证不了公网可达**：明文 Host 与 SNI 都没出过这台机器。"
  echo "    上线核验要到机器之外再跑一遍；要让它自己判红，加 SMOKE_REQUIRE_OFFBOX=1。"
fi
