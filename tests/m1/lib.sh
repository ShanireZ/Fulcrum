#!/usr/bin/env bash
# M1 兩個場景（run.sh / no-handover.sh）共用的判據原語。
#
# ★ 為什麼一開始就抽出來：兩份複製粘貼的收尾邏輯遲早分頭長歪，
#   同一個缺陷在其中一份裡躲過了整整一輪複審（見 tests/m0/lifecycle.sh 頂部）。
#   M1 的兩個場景是**同一件事的正反兩面**，判據必須逐字相同，否則「反證」證明不了任何東西。

UNIT=${UNIT:-fulcrum-m1.service}
CGROUP_DIR="/sys/fs/cgroup/system.slice/${UNIT}"
# ★ 必須與 conf/m1.yaml 的 `pid_file` 一致。這裡是**當前是哪一代**的唯一權威：
#   `ExitType=cgroup` 之下 unit 的 MainPID 在第一次升級後就歸零了。
PID_FILE=${PID_FILE:-/run/fulcrum-m1/fulcrum.pid}
# 收尾與現場轉儲要按這個模式找殘留處理程序。
# ★ 批 12 抽成變數：產品場景（product.sh）跑的是 `fulcrum serve`，
#   不是 spike。⚠ 寫死 `m1-systemd` 的話，產品場景的收尾會**一個處理程序都收不到**，
#   而收尾函式照樣回傳成功 —— 下一個場景撞上「連接埠被佔」，看起來像它自己的新問題。
PROC_PATTERN=${PROC_PATTERN:-m1-systemd}

unit_prop() { systemctl show "$UNIT" -p "$1" --value; }
unit_state() { printf '%s/%s' "$(unit_prop ActiveState)" "$(unit_prop SubState)"; }
main_pid() { unit_prop MainPID; }

# unit 的 cgroup 裡現在有哪些處理程序。unit 停掉後目錄會消失，此時輸出為空。
cgroup_pids() { cat "${CGROUP_DIR}/cgroup.procs" 2>/dev/null || true; }
pid_in_cgroup() { cgroup_pids | grep -qx "$1"; }

alive() { kill -0 "$1" 2>/dev/null; }

# ── 連接埠探測 ────────────────────────────────────────────────────────────────
#
# ★ 這道檢查**自帶反向測試**：場景開頭斷言三個連接埠都沒在監聽，start 之後斷言都在監聽。
#   一次通過的執行同時走了 true 與 false 兩個方向 —— 這正是 AGENTS.md 推薦的形狀
#   （`tests/m0/unclaimed.sh` 的 port_listening 就是這麼用的）。
#   若哪天 ss 的輸出格式變了讓它恆回傳 false，[1/9] 會立刻紅，而不是安靜地永遠綠。
port_listening() {
  local port=$1 proto=$2
  case "$proto" in
    tcp) ss -lnt "sport = :${port}" | tail -n +2 | grep -q . ;;
    udp) ss -lnu "sport = :${port}" | tail -n +2 | grep -q . ;;
    *) echo "port_listening: 未知协议 $proto" >&2; return 2 ;;
  esac
}

# ── 監聽 socket 被幾個 fd 指著 ───────────────────────────────────────────────
#
# ★ `ss -p` 會把**指向同一個 socket 的每一個 fd 都列出來**（`users:(("m1-systemd",pid=341,
#   fd=5),("m1-systemd",pid=341,fd=11))`），所以数 `fd=` 的個數就是這個監聽 socket
#   在本處理程序裡被 dup 出了幾份。M1 靠它量「fork 出來的下一代會不會同時從兩條路拿到監聽 fd」。
#   ⚠ 必須是 `grep -o … | wc -l`，**不能寫 `grep -c -o`**：`-c` 數的是「有匹配的行數」，
#     它會蓋過 `-o`。而 ss 把同一個 socket 的所有 fd 打在**同一行**裡
#     （`users:(("m1-systemd",pid=341,fd=5),("m1-systemd",pid=341,fd=11))`），
#     於是 `grep -c -o` 永遠回傳 1 —— 一個恆等於「沒洩漏」的假結論。
listener_fd_count() {
  local port=$1 proto=$2 flag=-lntp
  [ "$proto" = udp ] && flag=-lnup
  ss "$flag" "sport = :${port}" | grep -o 'fd=[0-9]*' | wc -l
}

# 這個監聽 socket 現在掛在哪些 fd 號上。
listener_fds() {
  local port=$1 proto=$2 flag=-lntp
  [ "$proto" = udp ] && flag=-lnup
  ss "$flag" "sport = :${port}" | grep -o 'fd=[0-9]*' | cut -d= -f2
}

# 三個監聽連接埠各被幾個 fd 指著，打成一行，例如 "8080:1 8081:1 8082:1"。
listener_fd_profile() {
  local out="" p proto
  for p in 8080 8081 8082; do
    proto=tcp; [ "$p" = 8082 ] && proto=udp
    out="$out $p:$(listener_fd_count "$p" "$proto")"
  done
  printf '%s' "${out# }"
}

# 某個處理程序持有幾個指向**升級 socket** 的 fd。
#
# ★ 正常值恆為 0：`get_fds_from()` 裡 accept 出來的那個連接用完就該關掉。
#   ★ 2026-09-24 起上游 0.9.0 已修（`b2b35fd`，owner 的 PR #960）；本判據照舊留著 ——
#   上游單測只看單個處理程序，看不到 fork+exec 下的逐代累加。以下是 0.8.1 時的實測：
#   上游（0.8.1 與當時的 main 均）**從來不關它**——只關了 listen_fd，
#   而 accept 的回傳值是裸 RawFd、沒有 Drop。於是**每完成一次優雅升級就永久洩漏一個
#   已連接的 unix socket**（`/proc/net/unix` 裡 St=03 CONNECTED、路徑是 upgrade.sock）。
#   判據取 `/proc/net/unix` 的**路徑列**，而不是數 fd 總數——總數會被流量、執行緒數干擾。
upgrade_sock_fds() {
  local pid=$1 fdpath link ino n=0
  # ★ 用 glob 而不是 `$(ls ...)`（SC2045）。處理程序已經沒了的話 glob 不展開，
  #   留下字面值路徑，下一行的 readlink 會失敗並 continue —— 與原來的行為一致。
  for fdpath in "/proc/$pid/fd/"*; do
    link=$(readlink "$fdpath" 2>/dev/null) || continue
    case "$link" in socket:\[*\]) ino=${link#socket:[}; ino=${ino%]} ;; *) continue ;; esac
    grep -qE "[[:space:]]${ino}[[:space:]]+/run/fulcrum-m1/upgrade\.sock$" /proc/net/unix && n=$((n + 1))
  done
  printf '%s' "$n"
}

# 某個 fd 有沒有設 FD_CLOEXEC。
# ★ `/proc/<pid>/fdinfo/<fd>` 的 flags 是**八進制**（O_CLOEXEC = 02000000）。
#   按十進位讀會得到一個看起來很像結果的錯誤答案，所以這裡顯式寫 `8#`。
fd_has_cloexec() {
  local pid=$1 fd=$2 flags
  flags=$(sed -n 's/^flags:[[:space:]]*//p' "/proc/$pid/fdinfo/$fd" 2>/dev/null || true)
  [ -n "$flags" ] || return 2
  [ $((8#$flags & 8#2000000)) -ne 0 ]
}

# ── 失敗處理 ────────────────────────────────────────────────────────────────
#
# ★ 失敗時必須把現場打出來。systemd 的判據大半是「某一刻的狀態」，事後無法重建。
# 現存的被測處理程序 pid（可能一個都沒有），按 `$PROC_PATTERN` 找。
# ★ 用 pgrep 而不是 `ps | grep … | grep -v grep`：後者靠「把自己從結果裡濾掉」成立，
#   而那一步一旦被改動（比如換個匹配詞）就會安靜地把自己算進去。pgrep 從不匹配自己。
m1_pids() { pgrep -f "$PROC_PATTERN" || true; }

dump_scene() {
  local pids=()
  echo "--- systemctl status ---"
  systemctl status "$UNIT" --no-pager -l 2>&1 | head -30 || true
  echo "--- cgroup procs ---"
  cgroup_pids | tr '\n' ' '; echo
  echo "--- ps ---"
  mapfile -t pids < <(m1_pids)
  if [ "${#pids[@]}" -gt 0 ]; then
    ps -o pid,ppid,stat,args -p "${pids[@]}"
  else
    echo "(无 $PROC_PATTERN 进程)"
  fi
  echo "--- journal（本 unit，含 systemd 自己的话）---"
  journalctl -u "$UNIT" --no-pager -o short-monotonic 2>&1 | tail -60 || true
}

fail() {
  echo
  echo "M1 FAILED: $*"
  dump_scene
  exit 1
}

# ── 收尾 ────────────────────────────────────────────────────────────────────
#
# ★ 掛在 EXIT trap 上，失敗路徑也要收 —— 與 M0 同一條紀律。
#   這裡比 M0 簡單，因為**收尾的物件是 unit 而不是一堆 pid**：
#   `KillMode=control-group` 保證 systemd 會把整個 cgroup 收乾淨，這正是本 spike
#   要驗的那個機制的另一面。收完再驗一次「真的空了」，不然就是在信任而不是在檢查。
m1_cleanup() {
  systemctl stop "$UNIT" >/dev/null 2>&1 || true
  systemctl reset-failed "$UNIT" >/dev/null 2>&1 || true
  local leftover=()
  mapfile -t leftover < <(m1_pids)
  if [ "${#leftover[@]}" -gt 0 ]; then
    echo "⚠ 收尾之后仍有 $PROC_PATTERN 进程活着，下一个场景的端口会被占：" >&2
    ps -o pid,ppid,stat,args -p "${leftover[@]}" >&2 || true
    pkill -KILL -f "$PROC_PATTERN" || true
  fi
}

# 斷言 unit 現在是 active/running，附帶說明這一步為什麼要看它。
assert_active() {
  local why=$1 st
  st=$(unit_state)
  [ "$st" = "active/running" ] || fail "$why —— unit 现在是 $st，期望 active/running"
}
