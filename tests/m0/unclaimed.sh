#!/usr/bin/env bash
# 未被認領的繼承 fd 會怎樣 —— 補上 M0 只驗了一面的那個缺口。
# 這個指令碼在容器裡跑（G26），結束代碼即結論。
#
# ★ 背景：M0 證明的是**認領成功**那條路（自建監聽器能取到 fd、升級零中斷）。
#   上游 `f82478ae` 指出另一面：**沒被認領的繼承 fd 會黑洞化連接**。
#   見 docs/verification/open-seams.md。
#
# ★ ★ **口徑 2026-09-24 反寫（pingora 0.9.0 rebase）：本指令碼現在驗的是「修好了」。**
#   上游 0.9.0 含 `f82478a`（新一代按各服務的 `listen_addresses()` 剪掉沒人認領的繼承 fd），
#   而 spike 的三個服務都宣告了鍵 ⇒ 判據是：第二代關掉那個 fd（上游那條 WARN）、
#   老一代退出後連接埠**拒連**、它不再傳給第三代。
#   ⚠ 它守的最要緊的一條：**任何一個服務的 `listen_addresses()` 回傳 `None`（pingora 的預設）
#     都會讓整個處理程序的清理靜默關閉** —— 編譯器不報、紀錄不提。反證：讓 raw_udp 回傳 `None`
#     ⇒ [4/6] 當場紅（2026-09-24 實測）。
#   ★ 此前（0.8.1）它驗的是「壞行為已重現」：綠 = 孤兒 fd 保持 LISTEN、吞連接、逐代傳遞。
#
# 造法：gen2 用 `M0_DROP_RAW_TCP=1` 啟動，模擬「設定裡刪掉了 raw-tcp 這個監聽器」，
# 於是 gen1 傳來的 8081 那個 fd 沒有任何服務去認領。
set -euo pipefail

RUN=${RUN:-/w/run/m0-unclaimed}
BIN=${BIN:-/w/target/release}
CONF=${CONF:-/w/conf/m0-unclaimed.yaml}
PORT=8081
# ★ 與 spike 的 `bind_host()` 預設保持一致（迴環）。判據裡出現的位址不能寫死——
#   spike 從 0.0.0.0 收緊到 127.0.0.1，寫死的 grep 會當場全部失配，
#   而失配的方向是「找不到 = 判紅」，看起來像功能壞了，其實只是位址變了。
BIND_HOST=${M0_BIND_HOST:-127.0.0.1}
export M0_BIND_HOST="$BIND_HOST"

export RUST_LOG=${RUST_LOG:-info}

mkdir -p "$RUN"
rm -f "$RUN"/*.pid "$RUN"/*.sock "$RUN"/*.log "$RUN"/*.pids

LOG="$RUN/error.log"

fail() { echo; echo "UNCLAIMED FAILED: $*"; echo "--- server log ---"; cat "$LOG" 2>/dev/null || true; exit 1; }

# ── 收尾 ────────────────────────────────────────────────────────────────────
# 規則與全部理由在 lifecycle.sh 裡。對本場景尤其要緊：結束時最後一代手裡正持著 8081 上
# 那個**孤兒 LISTEN socket**，留著它，同一容器裡後續用到這批連接埠的場景都會撞 EADDRINUSE。
# ★ 找不到庫就必須炸，不能默默不收。
LIFECYCLE_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lifecycle.sh"
[ -f "$LIFECYCLE_LIB" ] || { echo "找不到 $LIFECYCLE_LIB —— 收尾逻辑缺失，拒绝继续。" >&2; exit 1; }
# shellcheck source=tests/m0/lifecycle.sh
. "$LIFECYCLE_LIB"
lifecycle_init "$RUN"
trap lifecycle_cleanup EXIT

# ★ 連接埠是否有人 LISTEN —— 讀 /proc/net/tcp{,6}，**不依賴 bash 的 /dev/tcp**。
#   /dev/tcp 是編譯期特性（--enable-net-redirections），精簡鏡像裡可能沒有；
#   之前這裡寫過一句「自證它可用」的檢查，實際測的是 /dev/null 且結果被丟棄——
#   是個假的守衛。改成兩套互不依賴的機制，還能互相交叉驗證（見 [2/6]）。
#
# ★ ★ **兩個檔案都要掃。** tcp6 裡放的是全部 AF_INET6 socket，雙棧監聽 `[::]:port`
#   **只**出現在那裡，/proc/net/tcp 一行都沒有（在本鏡像裡實測過）。
#   當前 spike 綁的是 127.0.0.1（純 IPv4），所以唯讀 tcp 僥倖沒錯；而雙棧是絕大多數
#   網路庫的預設綁法，M1 的真監聽器一旦綁 `[::]`，這個函式就會**恆假**——
#   於是 [0/6] 的前置檢查靜默通過，回到它本來就是為了防的那件事：
#   **基線對著別人的處理程序變綠**。
PROC_NET_FILES=(/proc/net/tcp /proc/net/tcp6)
port_listening() {
  local hex f seen=0
  hex=$(printf '%04X' "$1")
  for f in "${PROC_NET_FILES[@]}"; do
    [ -r "$f" ] || continue
    seen=1
    # local_address 是 `0100007F:1F90`（v4，8 位十六進制）或
    # `00000000000000000000000000000000:1FA3`（v6，32 位）。連接埠段的取法一樣，
    # 所以只錨「冒號之後就是連接埠且到詞尾」，不關心前面多長。$4 == 0A 即 TCP_LISTEN。
    if awk -v want="$hex" \
         '$4 == "0A" && $2 ~ ("^[0-9A-Fa-f]+:" want "$") { found = 1 } END { exit !found }' "$f"
    then
      return 0
    fi
  done
  # ★ 讀不到就必須炸，不能當成「沒人監聽」。fail-open 的守衛是本專案已經栽過的形狀。
  [ "$seen" = 1 ] || fail "${PROC_NET_FILES[*]} 一个都读不到——无法判断端口占用，本脚本的判据全部失效"
  return 1
}

# ★ port_listening 在 [0/6] 只被用來**放行**，所以一個恆假的實作會讓那道門靜默通過。
#   開跑前先拿固定樣本證明它兩個方向都認得——這就是 AGENTS.md 說的「門自帶反向測試」。
#
#   ★ 樣本是**真實核心輸出**，不是照著文件編的：在建置鏡像裡讓 python 綁
#     `[::]:8099`（=0x1FA3）之後，從 /proc/net/tcp6 原樣抄下來的；同一時刻 /proc/net/tcp
#     除表頭外**一行都沒有**。這份樣本本身就是本次盲區的實物證據。
selftest_port_listening() {
  local dir saved=("${PROC_NET_FILES[@]}")
  dir=$(mktemp -d)
  {
    echo '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
    echo '   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0'
    echo '   1: 0100007F:1F91 0100007F:8000 01 00000000:00000000 00:00000000 00000000     0        0 12346 1 0000000000000000 20 0 0 10 -1'
  } > "$dir/tcp"
  {
    echo '  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
    echo '   0: 00000000000000000000000000000000:1FA3 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 52691 1 00000000c0a6790d 100 0 0 10 0'
  } > "$dir/tcp6"

  PROC_NET_FILES=("$dir/tcp" "$dir/tcp6")
  local bad=""
  port_listening 8080 || bad="$bad 8080(v4-LISTEN 应为真)"
  # ★ 這一條就是本次要修的那個盲區：8099 只在 tcp6 裡。舊實作在這裡必紅。
  port_listening 8099 || bad="$bad 8099(v6-LISTEN 应为真)"
  ! port_listening 8081 || bad="$bad 8081(状态是 ESTABLISHED 不是 LISTEN，应为假)"
  ! port_listening 9000 || bad="$bad 9000(压根不在样本里，应为假)"
  PROC_NET_FILES=("${saved[@]}")
  rm -rf "$dir"

  [ -z "$bad" ] || fail "port_listening 自测未通过：$bad
       它是 [0/6] 唯一的判据，且只用于放行——先修它，后面所有结论都不可信。"
  echo "  ✓ port_listening 自测通过（v4/v6 各一条 LISTEN 认得出，非 LISTEN 与不存在的端口都判假）"
}

probe_connect() { timeout 2 bash -c "exec 3<>/dev/tcp/$BIND_HOST/$1" 2>/dev/null; }
# 發一個位元組並等回聲。回來了 → 0；連得上但沒回應 → 124（timeout）；連不上 → 非 0 且非 124
probe_echo() {
  timeout 3 bash -c "
    exec 3<>/dev/tcp/$BIND_HOST/$1 || exit 7
    printf 'ping' >&3
    head -c 4 <&3
  " 2>/dev/null
}

# ★ 開跑前連接埠必須是乾淨的。
#   第一次合併進全量跑時就栽在這裡：M0 場景的處理程序還沒走乾淨、佔著 8081，
#   本場景的 gen1 綁不上（EADDRINUSE），而**第 2 步的基線回聲照樣是綠的**——
#   因為它對著的是別人的處理程序。判據必須能把「是我們自己的處理程序在服務」釘死。
echo "=== [0/6] 前置：判据先自证，然后端口必须干净 ==="
selftest_port_listening
if port_listening "$PORT"; then
  fail "开跑前 $PORT 上就已经有人在 LISTEN（据 ${PROC_NET_FILES[*]}）——多半是上一个场景的进程没退干净。
       本场景要求端口干净，否则基线会对着别人的进程变绿。"
fi
echo "  ✓ $PORT 无人监听"

echo "=== [1/6] 起第一代（raw-tcp 正常挂着）==="
"$BIN/m0-seam" -c "$CONF" -d
sleep 1
[ -f "$RUN/m0.pid" ] || fail "第一代没有写出 pid 文件"
GEN1=$(cat "$RUN/m0.pid")
record_gen "$GEN1"
echo "gen1 pid = $GEN1"

echo "=== [2/6] 基线：raw-tcp 确实在工作，且**是我们这一代在服务** ==="
# ★ 兩條判據缺一不可：
#   ① 我們自己的 gen1 確實 bind 成功並把 fd 註冊進了表（看**我們的** error.log）
#   ② 回聲通
#   只有 ② 的話，別人的處理程序也能讓它變綠——那正是第一次合併跑時踩的坑。
grep -q "\[raw-tcp\] bound fresh on $BIND_HOST:$PORT, registered fd=" "$LOG" \
  || fail "第一代的 raw-tcp 没有成功 bind 并注册 fd（看 $LOG）。
       端口八成被别的进程占着——那样后面所有判据都是假的。"

# ★ 兩套機制在這裡交叉驗證：/proc 說它在 LISTEN，而 /dev/tcp 連不上，
#   那就是本 shell 不支援 /dev/tcp，而不是服務的問題。把這兩種情形分開報，
#   免得把缺失的 shell 特性誤讀成「行為變了」。
port_listening "$PORT" || fail "/proc/net/tcp 里看不到 $PORT 在 LISTEN，但日志说已经 bind 了——自相矛盾，需要人工看 $LOG"
OUT=$(probe_echo "$PORT" || true)
if [ "$OUT" != "ping" ]; then
  fail "基线不通（收到 '$OUT'）。★ 注意 /proc 显示 $PORT 确实在 LISTEN，
       所以这更像是**本 shell 不支持 /dev/tcp**（编译期特性），而不是服务出问题。"
fi
echo "  ✓ 我们这一代已注册 fd、端口在 LISTEN、回声正常（收到 '$OUT'）"

echo "=== [3/6] 升级到第二代，但**丢掉** raw-tcp 服务 ==="
# ★ error.log 是三代共用、追加寫的 ⇒ 下面一律用「起下一代前後的計數增量」判，不用絕對值。
INSPECT_KEY="\[fd-inspect\] entry key=m0-raw-tcp:$BIND_HOST:$PORT"
INSPECT_BEFORE_GEN2=$(grep -c "$INSPECT_KEY" "$LOG" || true)
kill -QUIT "$GEN1"
M0_DROP_RAW_TCP=1 "$BIN/m0-seam" -c "$CONF" -d -u
sleep 2
GEN2=$(cat "$RUN/m0.pid")
record_gen "$GEN2"
echo "gen2 pid = $GEN2"
[ "$GEN1" != "$GEN2" ] || fail "pid 没变，根本没换进程"

echo "=== [4/6] ★ 第二代把那个没人认领的 fd 关掉了（上游 0.9.0 f82478a）==="
# ★ 判據錨在 pingora 自己打的啟動順序行上，而不是 spike 自證的那句 WARN——
#   那句在 daemonize **之前**打，進的是啟動 shell 的 stdout，不在 error.log 裡。
LAST_ORDER=$(grep "Starting services in dependency order" "$LOG" | tail -1)
echo "  第二代启动的服务：$LAST_ORDER"
case "$LAST_ORDER" in
  *m0-raw-tcp*) fail "第二代并没有真的丢掉 raw-tcp 服务" ;;
esac

# ★ ★ 上游那條 WARN 在 daemonize **之後**、起服務之前打（bootstrap_services.rs），進 error.log。
#   第一代是冷啟動、沒有繼承任何 fd ⇒ 走到這裡時這條 WARN 只可能來自第二代。
grep -qF "Closed 1 inherited listening socket(s) not claimed by any service: [\"m0-raw-tcp:$BIND_HOST:$PORT\"]" "$LOG" \
  || fail "第二代没有关掉那个没人认领的 fd（error.log 里没有上游那条 WARN）。
       ★ 最常见的原因：某个服务的 listen_addresses() 返回了 None（pingora 的缺省实现）
         ⇒ **整个进程**的清理被静默关闭 —— 编译器不报、日志也不提。"
# ★ 交叉驗證：第二代的 fd-inspect 在清理**之後**才起，它看到的表裡不該再有這個鍵。
INSPECT_AFTER_GEN2=$(grep -c "$INSPECT_KEY" "$LOG" || true)
[ "$INSPECT_AFTER_GEN2" -eq "$INSPECT_BEFORE_GEN2" ] \
  || fail "WARN 说关了，而第二代的 fd 表里仍有 m0-raw-tcp 的条目（计数 $INSPECT_BEFORE_GEN2 → $INSPECT_AFTER_GEN2）"
echo "  ✓ 第二代关掉并移出了那个 fd（上游 WARN 在；fd 表计数 $INSPECT_BEFORE_GEN2 → $INSPECT_AFTER_GEN2，没有增长）"

echo "=== [5/6] ★ 核心判据：老一代退出之后，这个端口**拒连**（⛔ 不再黑洞）==="
# ★ ★ 必須先等第一代真正退出，否則測的是它而不是孤兒 socket。
#   pingora 在發完 fd 後要硬等 CLOSE_TIMEOUT（5 秒，server/mod.rs:59）才廣播停機，
#   這段時間**兩代都持有同一個監聽 socket，而老一代照常 accept**。
#   第一次寫這個指令碼時就是在這個視窗裡探的，結果收到了正常回聲——
#   不是行為不對，是判據取早了。
echo "  等第一代退出（pid=$GEN1）..."
for _ in $(seq 1 40); do
  kill -0 "$GEN1" 2>/dev/null || break
  sleep 1
done
if kill -0 "$GEN1" 2>/dev/null; then
  fail "第一代 40 秒还没退出，无法在干净状态下判断端口是否已释放"
fi
echo "  ✓ 第一代已退出"
# ★ 兩套機制各判一次（與 [2/6] 同一個道理）：/proc 說沒人 LISTEN，而且真連一次被拒。
if port_listening "$PORT"; then
  fail "第一代已退出，而 $PORT 仍在 LISTEN —— 那个没人认领的 fd 没被关掉（孤儿 socket 还在吞连接）"
fi
set +e
probe_connect "$PORT"
RC=$?
set -e
if [ "$RC" -eq 0 ]; then
  fail "居然连上了 $PORT —— 还有人持着这个监听 socket（/proc 却说没人 LISTEN，需要人工看 $LOG）"
elif [ "$RC" -eq 124 ]; then
  fail "连接**超时**（exit 124）而不是被拒 —— 行为与预期不同，需要人工看 $LOG"
fi
echo "  ✓ $PORT 不在 LISTEN、连接被拒（exit $RC）—— 没人认领的 fd 已随老一代退出彻底消失，⛔ 不再黑洞"

echo "=== [6/6] 它不会再传给第三代 ==="
BEFORE=$(grep -c "$INSPECT_KEY" "$LOG" || true)
kill -QUIT "$GEN2"
M0_DROP_RAW_TCP=1 "$BIN/m0-seam" -c "$CONF" -d -u
sleep 2
GEN3=$(cat "$RUN/m0.pid")
record_gen "$GEN3"
echo "gen3 pid = $GEN3"
[ "$GEN2" != "$GEN3" ] || fail "pid 没变，第三代没起来"
AFTER=$(grep -c "$INSPECT_KEY" "$LOG" || true)
[ "$AFTER" -eq "$BEFORE" ] \
  || fail "第三代的 fd 表里又出现了 m0-raw-tcp 的条目（计数 $BEFORE → $AFTER）—— 它本该在第二代就被剪掉"
echo "  ✓ 第三代收不到它（计数 $BEFORE → $AFTER）"

echo
echo "UNCLAIMED FIXED —— 未被认领的继承 fd 被新一代关掉：老一代退出后端口拒连，也不再逐代传递。"
echo "★ 依赖两件事同时成立：上游 0.9.0 的 f82478a，与**每个**服务都声明了 listen_addresses()"
echo "  （任何一个返回 None 都会让整个进程的清理静默关闭）。见 docs/verification/open-seams.md。"
