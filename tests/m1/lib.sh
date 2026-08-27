#!/usr/bin/env bash
# M1 两个场景（run.sh / no-handover.sh）共用的判据原语。
#
# ★ 为什么一开始就抽出来：两份复制粘贴的收尾逻辑迟早分头长歪，
#   同一个缺陷在其中一份里躲过了整整一轮复审（见 tests/m0/lifecycle.sh 顶部）。
#   M1 的两个场景是**同一件事的正反两面**，判据必须逐字相同，否则「反证」证明不了任何东西。

UNIT=${UNIT:-fulcrum-m1.service}
CGROUP_DIR="/sys/fs/cgroup/system.slice/${UNIT}"
# ★ 必须与 conf/m1.yaml 的 `pid_file` 一致。这里是**当前是哪一代**的唯一权威：
#   `ExitType=cgroup` 之下 unit 的 MainPID 在第一次升级后就归零了。
PID_FILE=${PID_FILE:-/run/fulcrum-m1/fulcrum.pid}
# 收尾与现场转储要按这个模式找残留进程。
# ★ 批 12 抽成变量：产品场景（product.sh）跑的是 `fulcrum serve`，
#   不是 spike。⚠ 写死 `m1-systemd` 的话，产品场景的收尾会**一个进程都收不到**，
#   而收尾函数照样返回成功 —— 下一个场景撞上「端口被占」，看起来像它自己的新问题。
PROC_PATTERN=${PROC_PATTERN:-m1-systemd}

unit_prop() { systemctl show "$UNIT" -p "$1" --value; }
unit_state() { printf '%s/%s' "$(unit_prop ActiveState)" "$(unit_prop SubState)"; }
main_pid() { unit_prop MainPID; }

# unit 的 cgroup 里现在有哪些进程。unit 停掉后目录会消失，此时输出为空。
cgroup_pids() { cat "${CGROUP_DIR}/cgroup.procs" 2>/dev/null || true; }
pid_in_cgroup() { cgroup_pids | grep -qx "$1"; }

alive() { kill -0 "$1" 2>/dev/null; }

# ── 端口探测 ────────────────────────────────────────────────────────────────
#
# ★ 这道检查**自带反向测试**：场景开头断言三个端口都没在监听，start 之后断言都在监听。
#   一次通过的运行同时走了 true 与 false 两个方向 —— 这正是 AGENTS.md 推荐的形状
#   （`tests/m0/unclaimed.sh` 的 port_listening 就是这么用的）。
#   若哪天 ss 的输出格式变了让它恒返回 false，[1/9] 会立刻红，而不是安静地永远绿。
port_listening() {
  local port=$1 proto=$2
  case "$proto" in
    tcp) ss -lnt "sport = :${port}" | tail -n +2 | grep -q . ;;
    udp) ss -lnu "sport = :${port}" | tail -n +2 | grep -q . ;;
    *) echo "port_listening: 未知协议 $proto" >&2; return 2 ;;
  esac
}

# ── 监听 socket 被几个 fd 指着 ───────────────────────────────────────────────
#
# ★ `ss -p` 会把**指向同一个 socket 的每一个 fd 都列出来**（`users:(("m1-systemd",pid=341,
#   fd=5),("m1-systemd",pid=341,fd=11))`），所以数 `fd=` 的个数就是这个监听 socket
#   在本进程里被 dup 出了几份。M1 靠它量「fork 出来的下一代会不会同时从两条路拿到监听 fd」。
#   ⚠ 必须是 `grep -o … | wc -l`，**不能写 `grep -c -o`**：`-c` 数的是「有匹配的行数」，
#     它会盖过 `-o`。而 ss 把同一个 socket 的所有 fd 打在**同一行**里
#     （`users:(("m1-systemd",pid=341,fd=5),("m1-systemd",pid=341,fd=11))`），
#     于是 `grep -c -o` 永远返回 1 —— 一个恒等于「没泄漏」的假结论。
listener_fd_count() {
  local port=$1 proto=$2 flag=-lntp
  [ "$proto" = udp ] && flag=-lnup
  ss "$flag" "sport = :${port}" | grep -o 'fd=[0-9]*' | wc -l
}

# 这个监听 socket 现在挂在哪些 fd 号上。
listener_fds() {
  local port=$1 proto=$2 flag=-lntp
  [ "$proto" = udp ] && flag=-lnup
  ss "$flag" "sport = :${port}" | grep -o 'fd=[0-9]*' | cut -d= -f2
}

# 三个监听端口各被几个 fd 指着，打成一行，例如 "8080:1 8081:1 8082:1"。
listener_fd_profile() {
  local out="" p proto
  for p in 8080 8081 8082; do
    proto=tcp; [ "$p" = 8082 ] && proto=udp
    out="$out $p:$(listener_fd_count "$p" "$proto")"
  done
  printf '%s' "${out# }"
}

# 某个进程持有几个指向**升级 socket** 的 fd。
#
# ★ 正常值恒为 0：`get_fds_from()` 里 accept 出来的那个连接用完就该关掉。
#   实测上游（0.8.1 与 main 均）**从来不关它**——只关了 listen_fd，
#   而 accept 的返回值是裸 RawFd、没有 Drop。于是**每完成一次优雅升级就永久泄漏一个
#   已连接的 unix socket**（`/proc/net/unix` 里 St=03 CONNECTED、路径是 upgrade.sock）。
#   判据取 `/proc/net/unix` 的**路径列**，而不是数 fd 总数——总数会被流量、线程数干扰。
upgrade_sock_fds() {
  local pid=$1 fdpath link ino n=0
  # ★ 用 glob 而不是 `$(ls ...)`（SC2045）。进程已经没了的话 glob 不展开，
  #   留下字面量路径，下一行的 readlink 会失败并 continue —— 与原来的行为一致。
  for fdpath in "/proc/$pid/fd/"*; do
    link=$(readlink "$fdpath" 2>/dev/null) || continue
    case "$link" in socket:\[*\]) ino=${link#socket:[}; ino=${ino%]} ;; *) continue ;; esac
    grep -qE "[[:space:]]${ino}[[:space:]]+/run/fulcrum-m1/upgrade\.sock$" /proc/net/unix && n=$((n + 1))
  done
  printf '%s' "$n"
}

# 某个 fd 有没有设 FD_CLOEXEC。
# ★ `/proc/<pid>/fdinfo/<fd>` 的 flags 是**八进制**（O_CLOEXEC = 02000000）。
#   按十进制读会得到一个看起来很像结果的错误答案，所以这里显式写 `8#`。
fd_has_cloexec() {
  local pid=$1 fd=$2 flags
  flags=$(sed -n 's/^flags:[[:space:]]*//p' "/proc/$pid/fdinfo/$fd" 2>/dev/null || true)
  [ -n "$flags" ] || return 2
  [ $((8#$flags & 8#2000000)) -ne 0 ]
}

# ── 失败处理 ────────────────────────────────────────────────────────────────
#
# ★ 失败时必须把现场打出来。systemd 的判据大半是「某一刻的状态」，事后无法重建。
# 现存的被测进程 pid（可能一个都没有），按 `$PROC_PATTERN` 找。
# ★ 用 pgrep 而不是 `ps | grep … | grep -v grep`：后者靠「把自己从结果里滤掉」成立，
#   而那一步一旦被改动（比如换个匹配词）就会安静地把自己算进去。pgrep 从不匹配自己。
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
# ★ 挂在 EXIT trap 上，失败路径也要收 —— 与 M0 同一条纪律。
#   这里比 M0 简单，因为**收尾的对象是 unit 而不是一堆 pid**：
#   `KillMode=control-group` 保证 systemd 会把整个 cgroup 收干净，这正是本 spike
#   要验的那个机制的另一面。收完再验一次「真的空了」，不然就是在信任而不是在检查。
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

# 断言 unit 现在是 active/running，附带说明这一步为什么要看它。
assert_active() {
  local why=$1 st
  st=$(unit_state)
  [ "$st" = "active/running" ] || fail "$why —— unit 现在是 $st，期望 active/running"
}
