#!/usr/bin/env bash
# M1 spike #1 主場景：systemd `Type=notify` 前台執行下，一次 `systemctl reload` 觸發的
# 優雅升級要做到——**unit 全程不離開 active、三類流量零中斷、停機仍走完排空**。
#
# ★ 這裡跑的是**實測之後定下來的形狀**（`ExitType=cgroup`，不交接 MainPID），
#   而不是 G31 拍板時推斷的那個（交接 MainPID）。為什麼改，見
#   `tests/m1/mainpid-handover.sh` 與 `docs/verification/m1-systemd.md`。
#
# 這個指令碼在**以 systemd 為 PID 1 的容器**裡跑（由 tests/m1/systemd-run.sh 拉起），
# 結束代碼即結論。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/m1/lib.sh
. "$HERE/lib.sh"

BIN=${BIN:-/w/target/release}
DURATION_MS=${DURATION_MS:-8000}
INTERVAL_MS=${INTERVAL_MS:-20}
BIND_HOST=${M1_BIND_HOST:-127.0.0.1}

# conf/m1.yaml 裡的兩個值，這裡只用來把「停機應該花多久」寫成判據。
GRACE=5
GRACEFUL=5

trap m1_cleanup EXIT

echo "=== [0/9] 前置检查 ==="
for b in m1-systemd m0-probe; do
  [ -x "$BIN/$b" ] || fail "找不到可执行文件 $BIN/$b（构建没跑？）"
done
[ "$(unit_prop LoadState)" = "loaded" ] || fail "unit $UNIT 没被 systemd 加载"
[ "$(unit_prop ActiveState)" = "inactive" ] || fail "开跑前 unit 就已经是 $(unit_state)"
[ "$(unit_prop ExitType)" = "cgroup" ] \
  || fail "unit 的 ExitType 是 '$(unit_prop ExitType)'，本场景的全部前提是 cgroup。
       （ExitType=main 时会发生什么，由 tests/m1/exit-type-main.sh 负责证明。）"
rm -f "$PID_FILE"
# ★ 這三條是 port_listening 的 **false 方向**。與 [1/9] 的 true 方向合起來，
#   一次通過的執行就證明了這個原語兩個方向都能報——它不是恆 false 的空操作。
for p in 8080 8081 8082; do
  proto=tcp; [ "$p" = 8082 ] && proto=udp
  ! port_listening "$p" "$proto" || fail "开跑前 $proto/$p 就已经在监听，端口被别的东西占着"
done
echo "  ✓ unit 已加载未启动、ExitType=cgroup；8080/8081/8082 都空着"

echo "=== [1/9] systemctl start —— 前台 + Type=notify 能不能起来 ==="
systemctl start "$UNIT" || fail "systemctl start 失败（多半是没等到 READY=1）"
assert_active "刚 start 完"
GEN1=$(main_pid)
[ "$GEN1" -gt 0 ] 2>/dev/null || fail "MainPID 是 '$GEN1'，systemd 没跟上主进程"
echo "  ✓ unit active/running，MainPID=gen1=$GEN1"

# ★ port_listening 的 **true 方向**（見 [0/9]）。
for p in 8080 8081 8082; do
  proto=tcp; [ "$p" = 8082 ] && proto=udp
  port_listening "$p" "$proto" || fail "start 之后 $proto/$p 仍然没在监听"
done
pid_in_cgroup "$GEN1" || fail "gen1=$GEN1 不在 unit 的 cgroup 里"

# ★ 兩條關於 pid 檔案的斷言，方向相反，缺一不可：
#   · pingora 預設的 /tmp/pingora.pid **不該存在** —— 它只由 daemonize() 寫，
#     不存在即證明前台模式生效（G31 的前提），而不只是設定裡寫了 daemon: false。
#   · conf 裡指定的那個**必須存在且內容正確** —— 它由 m1-systemd 自己兌現，
#     `ExecReload` 整條路都建在它上面。
[ ! -e /tmp/pingora.pid ] || fail "出现了 /tmp/pingora.pid —— 说明 daemonize() 跑了，前台模式没生效"
[ -s "$PID_FILE" ] || fail "pid 文件 $PID_FILE 没出现，reload 将无处可查"
[ "$(cat "$PID_FILE")" = "$GEN1" ] || fail "pid 文件里是 $(cat "$PID_FILE")，而 MainPID 是 $GEN1"
echo "  ✓ 三个端口都在监听；gen1 在 cgroup 里；pid 文件 = $GEN1；没有 daemonize 留下的 /tmp/pingora.pid"

FDPROF_GEN1=$(listener_fd_profile)
echo "  监听 fd 重数（gen1）：$FDPROF_GEN1"

echo "=== [2/9] 起探针（持续 ${DURATION_MS}ms）==="
"$BIN/m0-probe" "$DURATION_MS" "$INTERVAL_MS" \
  "$BIND_HOST:8080" "$BIND_HOST:8081" "$BIND_HOST:8082" > /tmp/m1-probe.json &
PROBE=$!
sleep 2

echo "=== [3/9] systemctl reload —— 触发升级 ==="
systemctl reload "$UNIT" || fail "systemctl reload 失败"
echo "  reload 已返回（ExecReload 只是发了 SIGUSR2，换代是异步的）"

echo "=== [4/9] 等新一代接手，并**逐次**盯住 ActiveState ==="
# ★ ★ 核心判據有兩條，缺一不可：
#     · pid 檔案必須指向一個新的、活著的、在本 unit cgroup 裡的處理程序
#     · 換代過程中 unit **一次都不許**離開 active —— 只看首尾兩個快照會漏掉中間的抖動，
#       而 G31 擔心的正是「老處理程序一退出 unit 即被判定結束」這種瞬間事件。
GEN2=""
for _ in $(seq 1 100); do   # 100 × 0.2s = 20s 上限
  st=$(unit_state)
  [ "$st" = "active/running" ] || fail "换代窗口内 unit 掉到了 $st —— 这正是 G31 担心的那个失败"
  cur=$(cat "$PID_FILE" 2>/dev/null || true)
  if [ -n "$cur" ] && [ "$cur" != "$GEN1" ]; then GEN2=$cur; break; fi
  sleep 0.2
done
[ -n "$GEN2" ] || fail "20 秒内 pid 文件一直是 $GEN1，换代没有发生"
echo "  ✓ pid 文件：$GEN1 → $GEN2，全程 active/running"
pid_in_cgroup "$GEN2" || fail "新一代 gen2=$GEN2 不在 unit 的 cgroup 里 —— 那它随时会被漏杀或漏管"
alive "$GEN1" || fail "gen1 在换代完成时就已经退出了，这一轮没有真正的重叠窗口，判据无效"
echo "  ✓ gen2 在 unit 的 cgroup 里；此刻两代同时活着（重叠窗口成立）"

echo "=== [5/9] 等第一代自己退出 —— 这一步才是 G31 那条推断的真正判据 ==="
for _ in $(seq 1 150); do   # 30s 上限：CLOSE_TIMEOUT(5) + grace(5) + graceful(5) 還有富餘
  alive "$GEN1" || break
  sleep 0.2
done
alive "$GEN1" && fail "30 秒后 gen1=$GEN1 还没退出，排空卡住了"
sleep 1   # 給 systemd 一點時間對「主處理程序退了」做出反應
assert_active "第一代退出之后"
alive "$GEN2" || fail "★ gen1 退出把 gen2 一起带走了 —— ExitType=cgroup 没有起作用"
# ★ 把「MainPID 歸零」這個代價也釘成判據。它不是缺陷，是 ExitType=cgroup 的直接後果，
#   而**整個 pid 檔案方案正是為它存在的**。哪天它不再歸零（systemd 改了行為），
#   ExecReload 就可以退回 $MAINPID，這條斷言會紅並提醒我們去簡化。
[ "$(main_pid)" = "0" ] \
  || fail "老代退出后 MainPID 是 $(main_pid)，而 ExitType=cgroup 下实测应当归零。
       行为变了：若它现在稳定指向新一代，ExecReload 可以退回 \$MAINPID，pid 文件那套可以删。"
echo "  ✓ gen1 已退出；unit 仍 active/running，gen2 活着；MainPID 已归零（ExitType=cgroup 的已知代价）"
FDPROF_GEN2=$(listener_fd_profile)
echo "  监听 fd 重数（gen2）：$FDPROF_GEN2"

echo "=== [6/9] 收探针 —— 升级窗口内三类流量 ==="
wait "$PROBE" || true
RESULT=$(cat /tmp/m1-probe.json)
echo "  probe: $RESULT"
num() { echo "$RESULT" | sed -n "s/.*\"$1\":\([0-9]*\).*/\1/p"; }
HTTP_OK=$(num http_ok);   HTTP_FAIL=$(num http_failures)
TCP_OK=$(num tcp_ok);     TCP_DISC=$(num tcp_disconnects)
UDP_OK=$(num udp_ok);     UDP_LOSS=$(num udp_losses)
[ "${HTTP_OK:-0}" -gt 0 ] || fail "HTTP 探针一次都没成功过，测试本身无效"
[ "${TCP_OK:-0}" -gt 0 ]  || fail "TCP 探针一次都没成功过，测试本身无效"
[ "${UDP_OK:-0}" -gt 0 ]  || fail "UDP 探针一次都没成功过，测试本身无效"
[ "${HTTP_FAIL:-1}" -eq 0 ] || fail "HTTP 请求失败 $HTTP_FAIL 次"
[ "${TCP_DISC:-1}" -eq 0 ]  || fail "跨升级的长连接断开 $TCP_DISC 次"
[ "${UDP_LOSS:-1}" -eq 0 ]  || fail "UDP 回声丢失 $UDP_LOSS 次"
echo "  ✓ http $HTTP_OK/0  tcp $TCP_OK/0  udp $UDP_OK/0（成功/失败）"

echo "=== [7/9] 再升一次（gen2 → gen3）——三件事一起验 ==="
# 其一：**第二次 reload 必須也能成**。這不是湊數——`ExitType=cgroup` 之後 MainPID 已歸零，
#       若 ExecReload 還寫著 `$MAINPID`，第一次 reload 能成、第二次失敗（實測：結束代碼 1，
#       journal 裡是 kill 的 Usage，而 unit 仍 active —— **升級沒發生**）。
#       只升一次的測試**看不見**這個洞。
#
# 其二、其三：★ ★ 守住在 vendor/pingora 裡修掉的兩個 fd 缺陷。
#       **它們只有到第三代才顯形**，所以必須連升兩次：
#         ② 移交來的監聽 fd 沒有 CLOEXEC → gen3 每個監聽 socket 被 2 個 fd 指著
#         ① accept 出來的升級 socket 從不 close → gen2 漏 1 個、gen3 漏 2 個（疊乘）
#       兩條在 M0 裡都不可能被覆蓋：M0 的第二代是從 shell 起的，與第一代沒有 fork 關係。
#       ★ 修復在 fork 裡（`FORK.md` 改動 ①②），所以這兩條斷言同時是**rebase 的守門人**：
#         上游沒接受、而 rebase 時漏了重做，它們會紅。
systemctl reload "$UNIT" || fail "第二次 reload 失败"
GEN3=""
for _ in $(seq 1 100); do
  st=$(unit_state)
  [ "$st" = "active/running" ] || fail "第二次换代窗口内 unit 掉到了 $st"
  cur=$(cat "$PID_FILE" 2>/dev/null || true)
  if [ -n "$cur" ] && [ "$cur" != "$GEN2" ]; then GEN3=$cur; break; fi
  sleep 0.2
done
[ -n "$GEN3" ] || fail "★ 第二次换代没有发生。pid 文件仍是 $GEN2 ——
       这正是 ExecReload 若写成 \$MAINPID 时会出现的形状：第一次能成，第二次失败且升级没发生。"
for _ in $(seq 1 150); do alive "$GEN2" || break; sleep 0.2; done
alive "$GEN2" && fail "gen2 没能退出"
sleep 1
assert_active "第二次升级之后"
FDPROF_GEN3=$(listener_fd_profile)
echo "  ✓ 连续两次升级都成立：$GEN1 → $GEN2 → $GEN3"
echo "  监听 fd 重数：gen1=[$FDPROF_GEN1] gen2=[$FDPROF_GEN2] gen3=[$FDPROF_GEN3]"

for g in "gen1:$FDPROF_GEN1" "gen2:$FDPROF_GEN2" "gen3:$FDPROF_GEN3"; do
  [ "${g#*:}" = "8080:1 8081:1 8082:1" ] \
    || fail "${g%%:*} 的监听 fd 重数是 [${g#*:}]，期望全 1。
       ★ 全 2 是**修复前**的形态（继承 1 + SCM_RIGHTS 1）。若它回来了，多半是
       get_fds_from() 里 MSG_CMSG_CLOEXEC 那处修复在某次 rebase / 升级里丢了 ——
       它 0.9.0 起在上游（b2b35fd，owner 的 PR #960），此前是 vendor/pingora/FORK.md §4 ②。"
done
echo "  ✓ 三代的监听 fd 都是每个 socket 一个（移交来的 fd 带 CLOEXEC，没有被 fork 带进下一代）"

# ★ 把**原因**也釘住，而不是隻釘後果。上面那條只說「沒有多出來」，
#   下面這條說明**為什麼**沒有多出來——兩者一起才擋得住「換了別的機制碰巧也是 1」。
for fd in $(listener_fds 8081 tcp); do
  fd_has_cloexec "$GEN3" "$fd" \
    || fail "gen3 的监听 fd=$fd **没有** FD_CLOEXEC。它会被 fork 带进下一代，
       而继承进来的那一份不在 pingora 的 fd 表里，上游 listen_addresses() 的清理够不到它。"
done
echo "  ✓ gen3 的监听 fd 都带着 FD_CLOEXEC（原因，不只是后果）"

# ★ ★ 第二條修復的判據：accept 出來的升級 socket 必須被關掉。
#   修復前實測：gen1=0、gen2=1、gen3=2（自己洩漏一個 + 從 gen2 繼承一個）——
#   **兩個缺陷會疊乘**：洩漏的那個也沒有 CLOEXEC，於是逐代累加。
for g in "gen1:$GEN1" "gen2:$GEN2" "gen3:$GEN3"; do
  n=$(upgrade_sock_fds "${g#*:}")
  [ "$n" = "0" ] \
    || fail "${g%%:*} 还攥着 $n 个升级 socket 的 fd（应为 0）。
       ★ 这是 get_fds_from() 里 accept 出来的连接没被 close 造成的永久泄漏，
       每升一次漏一个。0.9.0 起上游已修（b2b35fd 把它交给 OwnedFd，owner 的 PR #960），
       此前是 vendor/pingora/FORK.md §4 ① ⇒ 这里红，说明那处修复在某次 rebase / 升级里丢了。"
done
echo "  ✓ 三代都没有攥着升级 socket（accept 出来的连接已被关闭）"

echo "=== [8/9] systemctl stop —— 排空要走完，且不能是被 SIGKILL 掉的 ==="
STOP_T0=$(date +%s)
systemctl stop "$UNIT" || fail "systemctl stop 返回非零"
STOP_T1=$(date +%s)
STOP_SECS=$((STOP_T1 - STOP_T0))
STATE=$(unit_state)
[ "$STATE" = "inactive/dead" ] || fail "stop 之后 unit 是 $STATE，期望 inactive/dead（failed 说明它是被打死的）"
RESULT_PROP=$(unit_prop Result)
[ "$RESULT_PROP" = "success" ] \
  || fail "unit 的 Result=$RESULT_PROP，期望 success。
       ★ 若是 'signal'，那正是「交接过 MainPID」时的形状（见 mainpid-handover.sh）——
       检查有没有人把 MainPID 交接又加了回来。"
[ -z "$(cgroup_pids)" ] || fail "stop 之后 cgroup 里还有进程：$(cgroup_pids | tr '\n' ' ')"
# ★ 下界比上界更值錢：如果 stop **秒回**，說明它根本沒等排空——那 TimeoutStopSec 這條
#   結論就是假的，而且那恰好就是 MainPID 交接被否掉的原因。
[ "$STOP_SECS" -ge "$GRACE" ] || fail "stop 只用了 ${STOP_SECS}s，比 grace_period_seconds=${GRACE} 还短 ——
       说明 systemd 没有等排空就把进程收掉了。这正是 MainPID 交接那条路的失败形状。"
echo "  ✓ stop 用了 ${STOP_SECS}s（grace=${GRACE} + graceful=${GRACEFUL}），Result=success，cgroup 已空"

echo "=== [9/9] 判定 ==="
echo
echo "M1-SPIKE-1 PASSED —— systemd Type=notify + ExitType=cgroup 前台运行下："
echo "  · 两次 systemctl reload 各完成一次零停机换代（$GEN1 → $GEN2 → $GEN3），unit 全程未离开 active"
echo "  · 老一代退出既没有把 unit 判定为结束，也没有把新一代连锅端"
echo "  · 升级窗口内 http/tcp/udp 三类流量零中断"
echo "  · stop 走完了排空（${STOP_SECS}s）且不是被强杀的"
echo "  · 两个 fd 缺陷都已在 fork 里修掉并由 [7/9] 守住（FORK.md 改动 ①②）"
