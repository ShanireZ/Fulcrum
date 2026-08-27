#!/usr/bin/env bash
# M1 第四个 systemd 场景（批 12 / G78）：**跑产品二进制**。
#
# ★ ★ ★ 它为什么必须单独存在：另外三个场景跑的是 spike（`target/release/m1-systemd`），
#   而 spike 把 sd_notify、pid 文件、SIGUSR2 换代**自己实现了一遍**。
#   于是那三个场景全绿的同时，产品二进制 `systemctl start` 会**超时失败**
#   （实测：MainPID=0、Result=timeout、/run/fulcrum/ 空）。
#
#   > ★ 一个 spike 证明的是「这条路走得通」，不是「产品走在这条路上」。
#     夹具喂给门的那个二进制，本身也是夹具的一部分。
#
# 本场景逐条钉住批 12 补上的四件事，外加两条 M1 退出条件：
#
#   | 步 | 钉住什么 | 不补的话它红在哪 |
#   |---|---|---|
#   | 1 | ① `sd_notify(READY=1)` | `systemctl start` 超时失败，脚本第一步就停 |
#   | 1 | ④ 停机预算（默认 35s 而不是 pingora 的 305s）| 启动日志那行数字 |
#   | 1 | ② pid 文件 | 文件不存在 / 内容对不上 MainPID |
#   | 3 | ③ SIGUSR2 换代触发器 | ⚠ 不接的话 SIGUSR2 走**默认动作＝杀进程**，unit 当场 failed |
#   | 5 | 换代之后管理面还在 | 批 12 查出：`-u` 那趟继承 fd，再 unlink 路径就永久失联 |
#   | 4/6 | 改配置 + reload 生效、再 reload 回滚 | M1 退出条件第 3 条 |
#   | 7 | 换二进制 + reload | 批 12 实测：只用 current_exe() 时 spawn 拿 ENOENT，**而 reload 报成功** |
#   | 9 | ④ 的另一半：`systemctl stop` 走完排空且不是被打死的 | Result≠success / 耗时越界 |
#
# 在**以 systemd 为 PID 1 的容器**里跑（由 tests/m1/systemd-run.sh 拉起），退出码即结论。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# ★ 这三个必须在 source 之前设：lib.sh 用 `${VAR:-默认}` 取值，且 CGROUP_DIR 由 UNIT 算出。
UNIT=${UNIT:-fulcrum-prod.service}
PID_FILE=${PID_FILE:-/run/fulcrum/fulcrum.pid}
# ⚠ 收尾要按它找残留进程。写死 `m1-systemd` 的话这里一个都收不到，而收尾照样报成功。
PROC_PATTERN=${PROC_PATTERN:-fulcrum serve}
export UNIT PID_FILE PROC_PATTERN
# shellcheck source=tests/m1/lib.sh
. "$HERE/lib.sh"

BIN=${BIN:-/w/target/release}
# ★ unit 指的是这个**容器本地**的副本，不是共享 target 卷里的那个原件。
#   [7/9] 要把它换掉（rename 换 inode，升级的标准做法），换共享卷里的会波及别的场景。
EXE=/opt/fulcrum/fulcrum
BIND_HOST=${M1_BIND_HOST:-127.0.0.1}
HTTP_PORT=${HTTP_PORT:-8080}
CONF_DIR=/etc/fulcrum
CONF="$CONF_DIR/Fulcrumfile"
ADMIN_SOCK=/run/fulcrum/admin.sock
PROBE_OUT=/tmp/fulcrum-prod-probe.txt

# ★ ★ 这两个数必须与 crates/fulcrum-server/src/process.rs 的两个常量一致，
#   而**本场景有意不在 Fulcrumfile 里写 `grace_period`** —— 走的就是默认值那条路。
#   ⚠ 批 12 之前那条路是 `None`，pingora 会用它自己的 EXIT_TIMEOUT=300，
#   于是 unit 的 TimeoutStopSec=60 会在排空进行到 1/5 时 SIGKILL。
EXPECT_GRACE=30
EXPECT_BUDGET=35

trap m1_cleanup EXIT

# 起一个持续的 HTTP 探针。★ 它跨越整个换代窗口，判据是**一次都不许失败**。
probe_start() {
  local secs=$1
  rm -f "$PROBE_OUT"
  (
    ok=0
    bad=0
    end=$(($(date +%s) + secs))
    while [ "$(date +%s)" -lt "$end" ]; do
      if curl -sS --max-time 2 -o /dev/null "http://$BIND_HOST:$HTTP_PORT/" 2>/dev/null; then
        ok=$((ok + 1))
      else
        bad=$((bad + 1))
      fi
      sleep 0.05
    done
    printf '%s %s\n' "$ok" "$bad" > "$PROBE_OUT"
  ) &
  PROBE=$!
}

body() { curl -sS --max-time 3 "http://$BIND_HOST:$HTTP_PORT/" 2>/dev/null || echo "<curl 失败>"; }

# 等响应体收敛到 $1，最多 $2 个 0.5 秒。打印实际用了多久。
#
# ★ ★ ★ **为什么这里必须是「等收敛」而不是「立刻断言」** —— CI 上抓到的：
#
#   换代时老一代收到 SIGQUIT 后，先把监听 fd 送给新一代，**然后还要再等
#   `CLOSE_TIMEOUT`（5 秒，pingora 写死在 server/mod.rs）才停止 accept**。
#   也就是说那 5 秒里**新旧两代都在同一个监听 socket 上 accept**，内核在两者之间分发。
#   ⇒ 刚看到 pid 文件变了就发一个请求，它**完全可能落在老一代身上**，
#     而老一代读的是旧配置 —— 本机跑十次都落在新一代，CI 上第一次就落在了老一代。
#
#   ⚠ 这不是「测试不稳定」，是**产品在换代窗口内的真实行为**，运维要知道：
#     `systemctl reload` 之后的几秒内，两份配置都可能在服务。
#     ⇒ 判据的正确形态是「**会收敛**，且在有界的时间内收敛」。
#   ★ 而「老一代退干净之后只剩新配置」那条强判据在 [8/9] —— 两条都要有。
wait_body() {
  local want=$1 tries=$2 i=0 got
  while [ "$i" -lt "$tries" ]; do
    got=$(body)
    if [ "$got" = "$want" ]; then
      printf '%s' "$i"
      return 0
    fi
    sleep 0.5
    i=$((i + 1))
  done
  printf '%s' "$got"
  return 1
}

# 往 $CONF 写一份配置。$1 = 响应体里的标记。
#
# ★ 有意**不写 `grace_period`**（见上面 EXPECT_GRACE 那段）。
write_conf() {
  local mark=$1
  mkdir -p "$CONF_DIR"
  {
    printf '%s\n' "{"
    printf '    admin unix/%s\n' "$ADMIN_SOCK"
    printf '%s\n' "}" ""
    printf ':%s {\n' "$HTTP_PORT"
    printf '    respond 200 "%s"\n' "$mark"
    printf '%s\n' "}"
  } > "$CONF"
}

# 管理面存活探测：不认识的路径回 404。
# ★ 取 404 而不是 200，是因为它**不改变任何状态**，却要求连接真的建起来并被应答。
admin_probe() {
  curl -s -o /dev/null -w '%{http_code}' --unix-socket "$ADMIN_SOCK" \
    -X POST --data-binary '{}' http://localhost/nope 2>/dev/null || echo "000"
}

# 等 pid 文件指向一个与 $1 不同的进程，最多 $2 个 0.2 秒。全程盯住 ActiveState。
# 打印新一代的 pid；等不到就返回空。
wait_new_generation() {
  local prev=$1 tries=$2 st cur
  for _ in $(seq 1 "$tries"); do
    st=$(unit_state)
    [ "$st" = "active/running" ] || fail "换代窗口内 unit 掉到了 $st —— 换代不该让 unit 离开 active"
    cur=$(cat "$PID_FILE" 2>/dev/null || true)
    if [ -n "$cur" ] && [ "$cur" != "$prev" ]; then
      printf '%s' "$cur"
      return 0
    fi
    sleep 0.2
  done
  return 1
}

echo "=== [0/9] 前置检查 ==="
[ -x "$BIN/fulcrum" ] || fail "找不到产品二进制 $BIN/fulcrum（构建没跑？）"
# ★ 先证明量流量的家伙自己在。⚠ 少了这一条，镜像里没有 curl 时每一次请求都返回
#   「<curl 失败>」，而报出来的判据是「数据面本身就不对」—— 一句指错方向的结论。
#   （第一次跑本场景时真的发生了：镜像里当时确实没有 curl。）
command -v curl >/dev/null 2>&1 \
  || fail "测试宿主镜像里没有 curl —— 本场景的流量判据全部量不了。见 docker/Dockerfile.systemd"
[ "$(unit_prop LoadState)" = "loaded" ] || fail "unit $UNIT 没被 systemd 加载"
[ "$(unit_prop ActiveState)" = "inactive" ] || fail "开跑前 unit 就已经是 $(unit_state)"
[ "$(unit_prop ExitType)" = "cgroup" ] \
  || fail "unit 的 ExitType 是 '$(unit_prop ExitType)'，本场景的前提是 cgroup"
rm -f "$PID_FILE"
# ★ port_listening 的 **false 方向**（true 方向在 [1/9]）。一次通过的运行走完两个方向。
! port_listening "$HTTP_PORT" tcp || fail "开跑前 tcp/$HTTP_PORT 就已经在监听，端口被别的东西占着"
[ ! -e "$ADMIN_SOCK" ] || fail "开跑前 $ADMIN_SOCK 就已经存在"
# 把二进制装到容器本地路径（unit 的 ExecStart 指着它）。[7/9] 会把它换掉。
install -D -m 0755 "$BIN/fulcrum" "$EXE"
write_conf gen-a
echo "  ✓ 产品二进制已装到 $EXE；unit 已加载未启动、ExitType=cgroup；$HTTP_PORT 空着；配置已写入 $CONF"

echo "=== [1/9] systemctl start —— ★ 缺口① sd_notify(READY=1) ==="
# ⚠ 这一步就是实测失败的那一步：没有 READY=1 时它会等满 TimeoutStartSec
#   然后返回 1（"Job for … failed because a timeout was exceeded"）。
systemctl start "$UNIT" \
  || fail "★ systemctl start 失败 —— 这正是缺口①的形状：Type=notify 等不到 READY=1。
       检查 fulcrum-server 有没有在 ExecutionPhase::Running 之后发 sd_notify。"
assert_active "刚 start 完"
GEN1=$(main_pid)
[ "$GEN1" -gt 0 ] 2>/dev/null || fail "MainPID 是 '$GEN1'，systemd 没跟上主进程"
port_listening "$HTTP_PORT" tcp || fail "start 之后 tcp/$HTTP_PORT 仍然没在监听"
pid_in_cgroup "$GEN1" || fail "gen1=$GEN1 不在 unit 的 cgroup 里"
echo "  ✓ unit active/running，MainPID=gen1=$GEN1，$HTTP_PORT 在监听"

# ★ 缺口② —— 两条方向相反的断言，缺一不可：
#   · pingora 默认的 /tmp/pingora.pid **不该存在**（它只由 daemonize() 写）；
#   · 配置里指定的那个**必须存在且内容正确**（ExecReload 整条路建在它上面）。
[ ! -e /tmp/pingora.pid ] || fail "出现了 /tmp/pingora.pid —— 说明 daemonize() 跑了，前台模式没生效"
[ -s "$PID_FILE" ] \
  || fail "★ pid 文件 $PID_FILE 没出现 —— 缺口②：systemctl reload 将无处可查当前这一代。
       ⚠ 注意 pingora **只在 daemonize() 里读 pid_file**，前台模式下要产品自己写。"
[ "$(cat "$PID_FILE")" = "$GEN1" ] || fail "pid 文件里是 $(cat "$PID_FILE")，而 MainPID 是 $GEN1"
echo "  ✓ pid 文件 = $GEN1；没有 daemonize 留下的 /tmp/pingora.pid"

# ★ 缺口④ —— 停机预算这行数字。
#   ⚠ 判据取的是**具体的数**而不是「有这一行」：批 12 之前 grace 是 None，
#     pingora 会等 EXIT_TIMEOUT=300，这行会写 305 —— 而 305 > TimeoutStopSec=60。
BUDGET_LINE=$(journalctl -u "$UNIT" --no-pager -o cat | grep -F '停机预算约' | tail -1 || true)
[ -n "$BUDGET_LINE" ] || fail "启动日志里没有「停机预算约」这一行 —— 运维无从知道 TimeoutStopSec 该设多大"
case "$BUDGET_LINE" in
  *"停机预算约 ${EXPECT_BUDGET}s"*) ;;
  *) fail "★ 停机预算是「$BUDGET_LINE」，期望 ${EXPECT_BUDGET}s。
       ⚠ 若它是 305s，说明 grace_period_seconds 又变回 None 了 —— pingora 会用
       它自己的 EXIT_TIMEOUT=300，而本 unit 的 TimeoutStopSec 是 60：
       systemctl stop 会在排空进行到 1/5 时 SIGKILL。" ;;
esac
echo "  ✓ 停机预算：$BUDGET_LINE"

echo "=== [2/9] 流量基线 + 管理面基线 ==="
BODY=$(body)
[ "$BODY" = "gen-a" ] || fail "首次请求拿到的是「$BODY」，期望 gen-a —— 数据面本身就不对，后面的判据无效"
[ -S "$ADMIN_SOCK" ] || fail "管理 socket $ADMIN_SOCK 没建起来"
CODE=$(admin_probe)
[ "$CODE" = "404" ] || fail "管理面基线：不认识的路径回 $CODE，期望 404"
echo "  ✓ GET / → gen-a；管理面在 $ADMIN_SOCK 上应答（404）"

echo "=== [3/9] 改配置 + systemctl reload —— ★ 缺口③ SIGUSR2 换代触发器 ==="
# ⚠ ⚠ 不接 SIGUSR2 的话，它走**默认动作＝终止进程**：这一步不是「reload 没反应」，
#   而是**服务当场死掉**（Restart=no，unit 变 failed）。比缺口②描述的更糟。
write_conf gen-b
# ★ 30 秒罩住**三次**换代（第 3、6、7 步）。它不额外花墙钟时间：
#   第 8 步本来就要等老几代排空（默认 30s）。
probe_start 30
sleep 1
systemctl reload "$UNIT" || fail "systemctl reload 返回非零"
GEN2=$(wait_new_generation "$GEN1" 100) \
  || fail "★ 20 秒内 pid 文件一直是 $GEN1，换代没有发生。
       缺口③：产品若没装 SIGUSR2 处理器，这一刀会走默认动作把进程杀掉；
       若 unit 现在是 failed，那就是这个形状。"
echo "  ✓ pid 文件：$GEN1 → $GEN2，换代窗口内 unit 全程 active/running"
pid_in_cgroup "$GEN2" || fail "新一代 gen2=$GEN2 不在 unit 的 cgroup 里 —— 它随时会被漏杀或漏管"
alive "$GEN1" || fail "gen1 在换代完成时就已经退出了，没有重叠窗口，本轮判据无效"
echo "  ✓ gen2 在 cgroup 里；此刻两代同时活着（重叠窗口成立）"

echo "=== [4/9] 新配置真的生效了（M1 退出条件第 3 条的前半）==="
# ⚠ 上界给 30 × 0.5s = 15s：CLOSE_TIMEOUT 是 5s，留三倍余量。
#   收不敛才是真问题（新一代读的不是新配置）。
N=$(wait_body gen-b 30) \
  || fail "15 秒内响应体一直没收敛到 gen-b（最后拿到「$N」）—— 换代发生了，但新一代读的不是新配置"
echo "  ✓ GET / → gen-b（下一代重新读了 $CONF；等了 $((N / 2)).$((N % 2 * 5)) 秒收敛）"

echo "=== [5/9] 换代之后管理面还在 —— 批 12 查出来的那个缺陷 ==="
# ★ ★ ★ 优雅换代时监听 fd 是**从上一代继承**的（按地址字符串查 fd 表，UDS 的键就是路径），
#   新一代不会重新 bind。此时若把路径 unlink 掉，两代都在一个**没有名字的** inode 上
#   accept，而客户端按路径连过去只有 ENOENT —— **一次 reload 之后管理面永久失联**，
#   且日志里一切正常、socket 也确实在 listen。
[ -S "$ADMIN_SOCK" ] \
  || fail "★ 换代之后 $ADMIN_SOCK 不见了 —— 新一代把继承来的 socket 路径 unlink 了。
       见 fulcrum-server/src/lib.rs 里那句 not-upgrade 判断（换代那趟不许 unlink）。"
CODE=$(admin_probe)
[ "$CODE" = "404" ] \
  || fail "★ 换代之后管理面回 $CODE（期望 404）—— POST /load 这条路已经断了。
       ⚠ 这条与上一条方向不同：文件在、却连不上，说明它是个陈旧 inode 上的孤儿。"
echo "  ✓ 管理面跨换代仍然可用（$ADMIN_SOCK 仍应答 404）"

echo "=== [6/9] 回滚：再改回去 + 第二次 reload（M1 退出条件第 3 条的后半）==="
# ★ 第二次 reload 必须也能成。`ExitType=cgroup` 之后 MainPID 已归零，
#   若 ExecReload 还写着 `$MAINPID`，**第一次能成、第二次失败**（实测：退出码 1、
#   journal 里是 kill 的 Usage，而 unit 仍 active —— 报错了，但换代没发生）。
#   只换一次代的测试**看不见**这个洞。
write_conf gen-a
systemctl reload "$UNIT" || fail "第二次 systemctl reload 返回非零"
GEN3=$(wait_new_generation "$GEN2" 100) \
  || fail "★ 第二次换代没有发生（pid 文件仍是 $GEN2）——
       这正是 ExecReload 写成 \$MAINPID 时的形状：第一次能成、第二次失败。"
N=$(wait_body gen-a 30) \
  || fail "15 秒内响应体一直没收敛回 gen-a（最后拿到「$N」）—— 回滚没生效"
echo "  ✓ pid 文件：$GEN2 → $GEN3，配置已回滚（GET / → gen-a；等了 $((N / 2)).$((N % 2 * 5)) 秒）"

echo "=== [7/9] ★ 换二进制 + reload —— 零停机升级那条路 ==="
# ★ ★ ★ 这一步是实测出来的一个真缺陷的门。
#
#   换二进制的标准做法是 rename 到原路径（直接往正在跑的可执行文件里写会 ETXTBSY），
#   而 rename **换的是 inode**：Linux 上 /proc/self/exe 立刻变成 "…/fulcrum (deleted)"，
#   `std::env::current_exe()` 原样返回这个带后缀的路径，exec 它必然 ENOENT。
#   ⚠ ⚠ 而 `systemctl reload` **返回成功**（journal 里写着 Reloaded）——
#   运维看到的是一次成功的升级，跑着的却还是旧二进制。
# ⚠ ⚠ **判据一律走 `/proc/<pid>/exe`，不拿它跟路径的 `stat` 比。**
#   实测（本容器是 overlayfs）：**什么都不替换**的情况下，
#   `stat -c %i /opt/fulcrum/fulcrum` 与 `stat -c %i /proc/<pid>/exe` 就已经不同
#   （325751 vs 235854）—— 路径那一侧报的是 overlay inode，`/proc` 那一侧报的是
#   底层 upper 层的 inode。拿这两个数去比会得到一道**永远红**的门，
#   而永远红的门等于没有门。⇒ 两边都从 `/proc` 取，才是同一个 inode 命名空间。
GEN3_EXE_INO=$(stat -c '%i' "/proc/$GEN3/exe")
cp "$BIN/fulcrum" "$EXE.new"
chmod 0755 "$EXE.new"
mv "$EXE.new" "$EXE"
# ★ 自证：老一代的 /proc/<pid>/exe 现在必须带 (deleted)。
#   它证明「旧 inode 真的从路径上消失了」——也就是本步要防的那个形态确实出现了。
#   ⚠ 少了这一条，一个根本没换成功的替换会让下面的断言轻松通过。
GEN3_EXE=$(readlink "/proc/$GEN3/exe" 2>/dev/null || true)
case "$GEN3_EXE" in
  *"(deleted)") ;;
  *) fail "老一代的 /proc/$GEN3/exe 是「$GEN3_EXE」，期望带 (deleted) —— 二进制没被真正替换，本步判据无效" ;;
esac
echo "  ✓ 二进制已替换；老一代（exe inode $GEN3_EXE_INO）的 exe 现在是 $GEN3_EXE"

systemctl reload "$UNIT" || fail "第三次 systemctl reload 返回非零"
GEN4=$(wait_new_generation "$GEN3" 100) \
  || fail "★ 换掉二进制之后换代没有发生（pid 文件仍是 $GEN3）。
       这正是只用 current_exe() 时的形状：它返回「…/fulcrum (deleted)」，
       spawn 拿到 ENOENT，本代继续服务 —— 而 systemctl reload 报的是成功。
       journal 里应当有「拉起下一代失败」。见 process.rs 的 next_generation_program()。"
# ★ ★ 核心判据：新一代跑的必须是**磁盘上现在那一份**，不是旧 inode。
#   ⚠ 只断言「换代发生了」不够：一个从 /proc/self/exe 的 **fd** 起下一代的实现
#   也会换代成功，而它起来的是**旧二进制** —— 升级照样没发生，且更难看出来。
#   ★ 两条断言合起来才够，而且它们用的是**同一把尺子的两个读数**
#     （上面刚量到老一代是 `…(deleted)`，这里量到新一代是干净路径）——
#     一次通过的运行就证明了这把尺子两个方向都能报。
GEN4_EXE=$(readlink "/proc/$GEN4/exe" 2>/dev/null || true)
[ "$GEN4_EXE" = "$EXE" ] \
  || fail "★ 新一代的 /proc/$GEN4/exe 是「$GEN4_EXE」，期望「$EXE」（不带 (deleted)）——
       带 (deleted) 就说明它跑的是**已经被换掉的那个旧文件**：升级没有发生。"
GEN4_EXE_INO=$(stat -c '%i' "/proc/$GEN4/exe")
[ "$GEN4_EXE_INO" != "$GEN3_EXE_INO" ] \
  || fail "★ 新一代跑的还是老一代那个 inode（$GEN3_EXE_INO）——
       换代成功了，但它起来的是**旧二进制**：升级没有发生。
       （一个从 /proc/self/exe 的 fd 起下一代的实现就是这个形状。）"
echo "  ✓ pid 文件：$GEN3 → $GEN4，新一代的 exe 是 $GEN4_EXE（inode $GEN3_EXE_INO → $GEN4_EXE_INO）"

echo "=== [8/9] 老几代自己退出，unit 不许跟着死 ==="
wait "$PROBE" || true
# ⚠ 探针没留下结果就必须当场判红，而不是让 `read` 在 set -e 下抛一句看不懂的错。
[ -s "$PROBE_OUT" ] || fail "探针没有留下结果文件 $PROBE_OUT —— 它多半是被收尾提前收掉了，本轮流量判据无效"
read -r P_OK P_BAD < "$PROBE_OUT"
[ "${P_OK:-0}" -gt 0 ] || fail "HTTP 探针一次都没成功过，测试本身无效"
[ "${P_BAD:-1}" -eq 0 ] || fail "跨换代窗口 HTTP 请求失败 $P_BAD 次（成功 $P_OK 次）"
echo "  ✓ 换代窗口内 HTTP $P_OK 次全成功、0 次失败"

# ⚠ 上界要给足：老一代先等 CLOSE_TIMEOUT(5s) 送 fd，再走满 grace(${EXPECT_GRACE}s)。
#   两代是先后开始排空的，所以按最晚那一代算。
for g in "$GEN1" "$GEN2" "$GEN3"; do
  for _ in $(seq 1 450); do   # 450 × 0.2s = 90s
    alive "$g" || break
    sleep 0.2
  done
  alive "$g" && fail "90 秒后老一代 $g 还没退出，排空卡住了"
done
sleep 1   # 给 systemd 一点时间对「主进程退了」做出反应
assert_active "老几代退出之后"
alive "$GEN4" || fail "★ 老一代退出把 gen4 一起带走了 —— ExitType=cgroup 没有起作用"
[ "$(main_pid)" = "0" ] \
  || fail "老代退出后 MainPID 是 $(main_pid)，而 ExitType=cgroup 下实测应当归零。
       行为变了：若它现在稳定指向新一代，ExecReload 可以退回 \$MAINPID。"
echo "  ✓ gen1/gen2/gen3 已退出；unit 仍 active/running，gen4 活着；MainPID 已归零"
# ★ ★ 现在只剩最后一代了，重叠窗口已经关闭 —— 这一条是**不带等待**的强判据：
#   连打 5 次，每一次都必须是回滚后的那份配置。
#   ⚠ [4/9]/[6/9] 那两条只证明「会收敛」；这一条证明「收敛之后不会再飘回去」。
for i in 1 2 3 4 5; do
  BODY=$(body)
  [ "$BODY" = "gen-a" ] \
    || fail "老几代都退干净之后，第 $i 次请求仍拿到「$BODY」，期望 gen-a —— 配置状态没有稳定下来"
done
echo "  ✓ 重叠窗口关闭后连打 5 次，每次都是回滚后的那份配置"

echo "=== [9/9] systemctl stop —— ★ 缺口④ 的另一半：排空要走完，且不能是被打死的 ==="
STOP_T0=$(date +%s)
systemctl stop "$UNIT" || fail "systemctl stop 返回非零"
STOP_T1=$(date +%s)
STOP_SECS=$((STOP_T1 - STOP_T0))
STATE=$(unit_state)
[ "$STATE" = "inactive/dead" ] || fail "stop 之后 unit 是 $STATE，期望 inactive/dead"
RESULT_PROP=$(unit_prop Result)
[ "$RESULT_PROP" = "success" ] \
  || fail "★ unit 的 Result=$RESULT_PROP，期望 success。
       ⚠ 'timeout' 正是缺口④的形状：排空窗口（默认 ${EXPECT_GRACE}s）若变回 pingora 的
       300s，systemd 会在 TimeoutStopSec=60 到点时 SIGKILL。"
[ -z "$(cgroup_pids)" ] || fail "stop 之后 cgroup 里还有进程：$(cgroup_pids | tr '\n' ' ')"
# ★ 下界比上界更值钱：stop 若**秒回**，说明它根本没等排空，
#   那「TimeoutStopSec 按预算设就够」这条结论就是假的。
[ "$STOP_SECS" -ge "$EXPECT_GRACE" ] \
  || fail "stop 只用了 ${STOP_SECS}s，比默认排空窗口 ${EXPECT_GRACE}s 还短 —— systemd 没等排空就把进程收掉了"
# 上界：它必须明显小于 TimeoutStopSec(60)，否则「没被打死」只是侥幸。
[ "$STOP_SECS" -le 55 ] \
  || fail "stop 用了 ${STOP_SECS}s，已经贴着 TimeoutStopSec=60 —— 预算与 unit 对不上了"
echo "  ✓ stop 用了 ${STOP_SECS}s（预算 ${EXPECT_BUDGET}s，TimeoutStopSec=60），Result=success，cgroup 已空"

echo "=== 判定 ==="
echo
echo "M1-PRODUCT PASSED —— **产品二进制**在 systemd Type=notify + ExitType=cgroup 下："
echo "  · systemctl start 拿到了 READY=1（缺口①），pid 文件落地并等于 MainPID（缺口②）"
echo "  · 启动日志报出停机预算 ${EXPECT_BUDGET}s，而不是 pingora 默认的 305s（缺口④）"
echo "  · 三次 systemctl reload 各完成一次零停机换代（$GEN1 → $GEN2 → $GEN3 → $GEN4，缺口③），unit 全程 active"
echo "  · 改配置 → 生效 → 回滚 → 回得去（M1 退出条件第 3 条），管理面跨换代仍可用"
echo "  · 换掉磁盘上的二进制再 reload，新一代跑的是**替换后的** inode（零停机升级那条路）"
echo "  · 换代窗口内 HTTP $P_OK 次零失败；stop 走完排空（${STOP_SECS}s）且 Result=success"
