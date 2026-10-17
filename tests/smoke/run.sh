#!/usr/bin/env bash
# 冒煙：**對著一個已經在跑的樞衡**逐項體檢（M1 退出條件第 2 條的一半）。
#
#   bash tests/smoke/run.sh https://example.com           # 真網域（上線時用這個）
#   bash tests/smoke/run.sh http://127.0.0.1:9200       # 本機執行個體（門禁裡用這個）
#   SMOKE_REQUIRE_OFFBOX=1 bash tests/smoke/run.sh https://example.com   # ★ 上線核驗：必須從機器之外跑
#
# ★ ★ ★ **從哪臺機器跑，決定了這份清單證明的是什麼。** 實測過一次：機器的公用網路入口上
#   有一個按明文 `Host` 與 SNI 攔截的中間設備，而**從機器內跑，這兩樣都不出網**，
#   它根本不在路徑裡 —— 同一份清單機內跑與機外跑驗的是兩件事，而結語說的是同一句話。
#   ⇒ 本指令碼自己判**觀察點**並寫進開頭與結語；上線核驗加 `SMOKE_REQUIRE_OFFBOX=1`，
#     那時機內觀察判紅。⚠ 預設不判紅是有意的：機內跑仍然回答著「本機這個服務好不好」。
#
# ★ ★ **它與門禁裡其餘場景分工不同。**
#   其餘場景**自己起服務、自己造夾具**，驗的是「程式碼寫對了沒有」。
#   本指令碼**什麼都不起**，它對著一個**別人部署好的**樞衡問：這臺機器現在是好的嗎？
#   ⇒ 它是 M1 退出條件裡那句「兩個真網域由樞衡承載、跑通冒煙」唯一能完成實作的東西：
#     那句話沒法靠門禁裡的夾具證明，只能靠一份**能對著真網域跑的清單**。
#
# ★ 判據分兩檔，因為對真網域與對本機執行個體能問的問題不一樣：
#   · **必查**：任何一個跑著的樞衡都該滿足（連得上、回得對、頭對、不洩漏版本）。
#   · **HTTPS 檔**：目標是 https:// 時才查（證書鏈、有效期、SNI、HTTP→HTTPS 跳轉）。
#
# ⚠ **它有意不做「效能」判斷**。壓力那半邊在 tests/stress/run.sh，
#   而任何效能宣告都要走 §8 / G19 的口徑（三家對拍、逐類設門、指令碼與原始資料公開）。
set -euo pipefail

TARGET=${1:-}
if [ -z "$TARGET" ]; then
  echo "用法：bash tests/smoke/run.sh <目标 URL>（如 https://example.com）" >&2
  exit 2
fi
TARGET=${TARGET%/}

# 期望的最短剩餘證書有效期（天）。★ 不是「沒過期就行」：ACME 的價值在於
# **在到期前很久就換掉**，而一張只剩 3 天的證書說明續期那條路沒在工作。
MIN_CERT_DAYS=${MIN_CERT_DAYS:-20}

FAILS=0
CHECKS=0
ok() { CHECKS=$((CHECKS + 1)); echo "  ✓ $*"; }
bad() { CHECKS=$((CHECKS + 1)); FAILS=$((FAILS + 1)); echo "  ✗ $*" >&2; }
note() { echo "  · $*"; }

# ── 前置：量東西的傢伙自己得在 ─────────────────────────────────────────────
#
# ★ 少了這一條，`curl` 不在時每一項都會「失敗」，而報出來的結論會指向被測的那臺機器。
#   （在 systemd 場景上真的踩過一次：鏡像裡沒有 curl，
#     而指令碼報的是「資料面本身就不對」。）
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

# ── 1. 連得上，而且回的是 HTTP ─────────────────────────────────────────────
#
# ⚠ ⚠ **不能寫成 `curl … -w '%{http_code}' || echo "000"`。**
#   curl 連不上時 `-w` **已經輸出了 `000`**，`|| echo` 再補一個 ⇒ 結果是 `000000`，
#   於是 `[ "$CODE" = "000" ]` 不成立，指令碼會帶著一個六位數繼續往下跑。
#   ★ 同樣的坑 `tests/serve/run.sh` 裡也白紙黑字記著 ——
#     **「先弄清工具已經做了什麼，再決定補什麼」**。
#   ⚠ 而 `|| true` 又是必須的：`set -e` 下 `X=$(失败的命令)` 會讓指令碼**當場退出**，
#     於是「連不上」這件事根本走不到下面那句 bad —— 判據在它該說話之前就沒了。
#     ★ 兩個坑方向相反：`|| echo 000` 是往輸出裡補，`|| true` 是接住結束代碼。補錯了地方就白搭。
# ★ 順手把**對端 IP** 一起量出來：下面「觀察點」那一條要靠它，而它不值得多打一發請求。
#   ⚠ 兩個 `-w` 欄位之間隔一個空格；`-o /dev/null` 保證 stdout 上只有 `-w` 的輸出。
#   ⚠ 連不上時 curl 仍會寫出 `000 `（remote_ip 是空的）—— 所以下面要按「有沒有空格」拆，
#     `${PROBE#* }` 在沒有空格時會原樣回整個串，那會把 `000` 當成一個 IP。
PROBE=$(curl -sS -o /dev/null -w '%{http_code} %{remote_ip}' --max-time 10 "$TARGET/" 2>/dev/null) || true
CODE=${PROBE%% *}
REMOTE_IP=""
case "$PROBE" in *' '*) REMOTE_IP=${PROBE#* } ;; esac
# ★ ★ **「連不上」與「證書驗不過」是兩個不同的事故，修法完全不同**，不許合併：
#   前者要去看處理程序／防火牆／DNS，後者要去看 ACME 與證書鏈。
#   而 curl 對這兩種情況給的都是 `000` —— 所以再問一次「繞過檢核能不能連上」。
#   ⇒ 是證書的問題就**繼續往下跑**，讓 HTTPS 檔給出精確診斷；真連不上才當場停。
INSECURE=()
if [ -z "$CODE" ] || [ "$CODE" = "000" ]; then
  if [ "$SCHEME" = "https" ] && curl -sSk -o /dev/null --max-time 10 "$TARGET/" 2>/dev/null; then
    bad "TLS 那一层没过（加 -k 就连得上）—— 具体是哪一条见下面的 HTTPS 档"
    # ⚠ 餘下幾項改用 -k，並且**在報告裡說明它們是繞過檢核跑的**。
    #   不這麼做的話，後面每一項都會以「連不上」的形式紅，把真正的原因埋掉。
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

# ── 1.5 觀察點：這一輪是**站在哪裡**量的 ───────────────────────────────────
#
# ★ 判據掛在**對端 IP** 上，而不是掛在「目標裡寫的是不是網域」上：
#   `https://example.com` 在被測機器上跑，對端 IP 就是它自己 —— 而那正是要認出來的那種情況。
# ⚠ ⚠ **拿不準的時候要說出拿不準**：容器裡通常沒有 `ip`（iproute2 只裝在 systemd 測試鏡像裡），
#   那時只能按「不是迴環」判 —— 這條結論比「在本機位址表裡查過」弱，**弱在哪要寫在輸出裡**，
#   不許偽裝成一個確定結論。
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

# ── 2. 回應頭：不許洩漏版本 ────────────────────────────────────────────────
#
# ★ 判據是**不存在**，而這類判據最容易寫成恆真。所以先證明這次真的取到了頭。
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
# ★ 這一條同時驗了「站台索引真的按 Host 查」。⚠ 它對**本機執行個體**才穩定：
#   真網域前面可能還有 CDN / 防火牆，它們會先一步攔掉。所以真網域上只記錄不判紅。
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

# ── 4. HTTPS 檔 ────────────────────────────────────────────────────────────
if [ "$SCHEME" = "https" ]; then
  # ★ 連接埠跟著 TARGET 走，不寫死 443：門禁裡的自證跑在 9211 上，
  #   而寫死 443 會讓證書那幾項**去問一臺根本不是被測目標的機器** ——
  #   它多半連不上，於是報出來的是「取不到證書」，指向一個不存在的問題。
  case "$HOSTPORT" in
    *:*) TLS_HOSTPORT=${HOSTPORT%%/*} ;;
    *) TLS_HOSTPORT="${HOST}:443" ;;
  esac
  # 4a. 證書鏈**能被預設信任庫驗過**。★ 不加 -k：那正是這一條要驗的東西。
  if curl -sS -o /dev/null --max-time 10 "$TARGET/" 2>/dev/null; then
    ok "证书链被系统信任库验过（没有用 -k 绕过）"
  else
    bad "证书链验不过 —— 浏览器会红。用 \"curl -v $TARGET/\" 看具体原因"
  fi

  # 4b. 剩餘有效期。★ 判據不是「沒過期」，是「還剩得夠多」：
  #     一張只剩幾天的證書說明**續期那條路沒在工作**，而那時它還沒壞。
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

    # 4c. ★ 未知 SNI 必須**拒絕握手**，不能回落到某張證書。
    #     ⚠ 回落會讓伺服器端挑一張用戶端拒絕的證書，而伺服器端紀錄裡只有一次**成功**的握手。
    if echo | openssl s_client -connect "$TLS_HOSTPORT" -servername "no-such.invalid" \
         >/dev/null 2>&1; then
      bad "未知 SNI 握手**成功**了 —— 它应当被拒绝（回落会让服务端日志看起来一切正常）"
    else
      ok "未知 SNI 被拒绝握手"
    fi
  else
    bad "没有 openssl —— 证书有效期与未知 SNI 这两项**没能检查**（不当成通过）"
  fi

  # 4d. HTTP → HTTPS 跳轉（自動 HTTPS 的可見部分）
  PLAIN="http://${HOSTPORT}"
  RCODE=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 10 "$PLAIN/" 2>/dev/null) || true
  case "$RCODE" in
    30*) ok "http:// 被 $RCODE 跳到 https://" ;;
    000) note "http:// 连不上（80 端口没开也可能是有意的，只记录）" ;;
    *) note "http:// 回 $RCODE，没有跳转（只记录：是否该跳取决于配置）" ;;
  esac
fi

# ── 5. keep-alive：同一條連接上連打兩次 ────────────────────────────────────
#
# ★ 判據取 `num_connects`：curl 在一次呼叫裡請求兩個 URL，重複使用了連接的話它是 1。
#   ⚠ 只看兩次都 200 的話，一個每次都重建連接的實作也全綠。
# ⚠ ⚠ **`-o` 與 `-w` 都是按 URL 生效的。** 只給一個 `-o /dev/null` 的話，
#   第二個 URL 的**回應體**會混進 `-w` 的輸出 —— 實測拿到過 `1smoke-ok0` 這種東西，
#   而它長得就像一個數字。⇒ 每個 URL 各給一個 `-o`，並讓 `-w` 每次換行。
# ★ 判據取**第二次**的 `num_connects`：它必須是 0（重複使用了第一條連接）。
#   這比「總數是 1」更準 —— 後者在只發出一次請求時也成立。
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
# ★ ★ ★ **觀察點進結語**（§10 登記、完成實作）。
#   在此之前，機內跑與機外跑的結語**逐字相同** —— 而它們證明的不是同一件事。
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
