#!/usr/bin/env bash
# fork 回归网（G30）：跑 vendor/pingora 自带的单元测试。
# 这个脚本在容器里跑（G26），退出码即结论。
#
# ★ 为什么需要它：`vendor/pingora` 自带 `[workspace]` 且被根 `Cargo.toml` `exclude`，
#   所以根目录的 cargo **一个字都跑不到它**——而 fork 改过的每一处上界，
#   上游都自带正对着的单元测试：
#
#     sfv    0.10→0.15  →  compression/mod.rs   test_accept_encoding_req_header
#                                               test_decide_on_accept_encoding
#     brotli 3→8        →  compression/brotli.rs 压缩/解压往返
#     nix    0.24→0.31  →  server/transfer_fd/mod.rs、protocols/l4/stream.rs
#     lru    0.16→0.18  →  pingora-pool/src/lru.rs、connection.rs
#
#   在这个脚本存在之前，FORK.md 里那句「M0 接缝验证是这次改动唯一的回归网」
#   是**字面属实**的：M0 一个字节的压缩协商都不测。
set -euo pipefail

REPO=${REPO:-/w}
MANIFEST="$REPO/vendor/pingora/Cargo.toml"
LOCK="$REPO/vendor/pingora/Cargo.lock"
ROOT_LOCK="$REPO/Cargo.lock"
TARGET=${VENDOR_TARGET:-$REPO/target/vendor}

fail() { echo; echo "VENDOR TESTS FAILED: $*"; exit 1; }

# ── [1/5] 锁文件 ────────────────────────────────────────────────────────────
# vendor 是独立 workspace，有自己的一份解析结果。没有锁文件，每次跑用的版本都可能不同。
if [ ! -f "$LOCK" ]; then
  echo "=== [1/5] 生成 vendor/pingora/Cargo.lock ==="
  cargo generate-lockfile --manifest-path "$MANIFEST"
else
  echo "=== [1/5] 已有 vendor/pingora/Cargo.lock ==="
  # ★ 锁文件存在 ≠ 锁文件还对得上清单。
  #   FORK.md 的 rebase 步骤第 3、4 步改的正是 vendor 里的 Cargo.toml，
  #   改完之后这份锁就过期了，而下面用的是 `--locked`——cargo 会抛一句
  #   与真正修法（删锁重生成）毫无关系的原始错误。**每次 rebase 必踩**，
  #   所以这里提前用一次不编译的 `cargo metadata --locked` 把它问出来。
  if ! cargo metadata --manifest-path "$MANIFEST" --format-version 1 --locked >/dev/null 2>&1; then
    echo
    echo "★ vendor/pingora/Cargo.lock 与清单对不上了（多半是刚 rebase 过、改了某个 Cargo.toml）。"
    echo "  修法就一步——删掉它，重跑本脚本会自动重新生成："
    echo "      rm vendor/pingora/Cargo.lock"
    echo "  ★ 重新生成之后记得**连同新锁一起提交**（G29 第 2 条：Cargo.lock 入库）。"
    fail "锁文件已过期"
  fi
  echo "  ✓ 锁文件与清单一致"
fi

# ── [2/5] ★ 证明这次跑的是「二进制里那套版本」 ─────────────────────────────
# 这一步不能省。vendor 独立解析，如果它解出来的 brotli 跟根 Cargo.lock 里的
# 不是同一个，那测试全绿也证明不了任何关于实际产物的事——只证明了另一套组合能跑。
echo "=== [2/5] 比对 vendor 与根 Cargo.lock 的版本一致性 ==="

dump_lock() {
  awk '
    $1 == "name"    { n = $3; gsub(/"/, "", n); next }
    $1 == "version" && n != "" { v = $3; gsub(/"/, "", v); print n, v; n = "" }
  ' "$1"
}

dump_lock "$ROOT_LOCK" > /tmp/root.pkgs
dump_lock "$LOCK"      > /tmp/vendor.pkgs

# ★ 不变量是「根 lock 里出现的每个版本，vendor lock 里都必须有」，**不是**「两边逐字相等」。
#   Cargo.lock 允许同名包多版本共存：vendor 侧多解析出 dev-dependencies 与未启用的可选依赖
#   （sentry、reqwest、rstest 这些），它们会合法地拖进老版本——例如 rand 0.8.7 与 0.10.2 并存。
#   那些不进产物，不该判红。**只有根侧那个版本在 vendor 侧缺席才是真问题。**
#
# ★ ★ **查的是两边都有的全部包，不是一张手写名单。**
#   原先这里是 `BUMPED="lru prometheus protobuf nix brotli ..."` —— 一张 16 项的手写清单，
#   是 FORK.md §1 那张上界表的**人工镜像**。它的危险方向不是「多写了几项」（无害），
#   而是**下一次 rebase 抬了一条新上界、没人往名单里加**：这道门会安静地少覆盖一个包，
#   而它看起来照样是绿的。这与 `supply-audit.py` 的 `ACCEPTED`、`unclaimed.sh` 的忽略名单
#   是同一个形状——**不自省的名单**。
#
#   改成全量之后没有名单可维护，覆盖面从 16 个变成 170 个。
#   实测：root 171 包 / vendor 332 包 / 共有 170，**零不一致**；
#   唯一只在 root 侧的是 `m0-seam` 自己（工作区成员，vendor 本就测不到，按规则自动跳过）。
versions_of() { awk -v k="$2" '$1 == k { print $2 }' "$1" | sort -u; }

# 把多行／多空格的版本列表压成一行、单空格分隔。
# ★ 原先写的是 `$(echo $rvs)`——靠**未加引号的词拆分**再重新拼起来。它能工作，
#   但和 CRLF 门里那个 `printf '...' $CRLF_FILES` 是同一个形状：把「分词」当字符串处理手段。
#   SC2116 / SC2086 正是冲这个来的。
#   ⚠ 注意别让注释里某一行以 `#` + 空白 + 那个工具名开头——它会被当成 **directive** 解析，
#     报 SC1072/SC1073。这一条本身就是写这段注释时踩出来的。
squash() { printf '%s' "$1" | tr -s '[:space:]' ' ' | sed 's/^ *//; s/ *$//'; }

# 两边都有的包 = 本次真正要比的对象。只在 root 一侧的（工作区成员）vendor 本就测不到；
# 只在 vendor 一侧的是 dev-dep，不进产物。
cut -d' ' -f1 /tmp/root.pkgs   | sort -u > /tmp/root.names
cut -d' ' -f1 /tmp/vendor.pkgs | sort -u > /tmp/vendor.names
comm -12 /tmp/root.names /tmp/vendor.names > /tmp/shared.names
comm -23 /tmp/root.names /tmp/vendor.names > /tmp/rootonly.names

SHARED=$(wc -l < /tmp/shared.names)
ROOTONLY=$(wc -l < /tmp/rootonly.names)
MISMATCH=0
DRAGGED=0

while IFS= read -r p; do
  rvs=$(versions_of /tmp/root.pkgs   "$p")
  vvs=$(versions_of /tmp/vendor.pkgs "$p")

  missing=
  for v in $rvs; do
    printf '%s\n' "$vvs" | grep -qx "$v" || missing="$missing $v"
  done

  if [ -n "$missing" ]; then
    printf '  ✗  %-24s 根侧的%s 在 vendor 侧缺席（vendor 有：%s）\n' \
           "$p" "$missing" "$(squash "$vvs")"
    MISMATCH=$((MISMATCH + 1))
  elif [ "$(printf '%s\n' "$vvs" | wc -l)" -gt "$(printf '%s\n' "$rvs" | wc -l)" ]; then
    # vendor 侧多解析出别的版本——合法（dev-dep／未启用的可选依赖），只作信息
    DRAGGED=$((DRAGGED + 1))
  fi
done < /tmp/shared.names

if [ "$MISMATCH" -ne 0 ]; then
  echo
  echo "★ $MISMATCH 个包：产物里用的版本在 vendor 侧根本没解析出来。"
  echo "  这意味着 vendor 测试跑的不是产物里那套组合，结果不可采信。"
  echo "  对齐方式（对每个不一致的包）："
  echo "    cargo update -p <name> --precise <root 侧版本> --manifest-path vendor/pingora/Cargo.toml"
  fail "版本不一致，拒绝在不可采信的组合上跑测试"
fi
printf '  ✓  %s 个共有包，root 侧的版本在 vendor 侧全部在场' "$SHARED"
printf '（其中 %s 个 vendor 另有旧版本，来自 dev-dep／未启用的可选依赖，不进产物）\n' "$DRAGGED"
[ "$ROOTONLY" -eq 0 ] || printf '  ·  另有 %s 个只在 root 侧（工作区成员，vendor 本就测不到）：%s\n' \
     "$ROOTONLY" "$(squash "$(cat /tmp/rootonly.names)")"

# ★ 这道门必须能红。它唯一的失效方式是「共有包数量掉到 0」——那时循环一次都不跑，
#   `MISMATCH` 恒为 0，于是它在**什么都没比**的情况下报绿。给个下界钉住。
[ "$SHARED" -ge 100 ] || fail "只比出 $SHARED 个共有包（正常应有 170 上下）——
       两份 lock 的解析结果之一多半是空的或格式变了，本次比对没有意义。"
echo "  ✓ 产物里那套版本在 vendor 侧全部在场——下面的测试结果对实际产物有效"

# ── [3/5] ★ ★ 自证「黑洞」真的是黑洞 ────────────────────────────────────────
#
# 下面那批连接超时测试全部依赖一个前提：`192.0.2.1:79` 发出去的 SYN 被**静默丢弃**。
# 这个前提**不是天然成立的**——Docker 默认网络会替这个地址应答（实测 1.7ms 就
# CONNECTED），要靠 docker-run.sh 装的那条 iptables DROP 规则才成立。
#
# ★ ★ 判据挂在**行为**上，不挂在「规则在不在」上。`iptables -L` 能列出规则，
#   不等于它生效了：容器可能没有 NET_ADMIN、内核模块可能缺、也可能被前面的链
#   抢先 ACCEPT。**能证明它生效的只有「连过去真的会超时」这一件事。**
#
# 三种结果必须分清，它们对应完全不同的修法：
#   超时      → 真丢包，前提成立
#   连上      → Docker 网络在替它应答，规则没装或没生效
#   立刻失败  → EHOSTUNREACH/ECONNREFUSED 一类。★ 这**也不对**——测试等的是
#               ConnectTimedout，立刻失败给的是 ConnectError，照样红。
#               （所以不能用 `ip route add blackhole` 代替 DROP。）
echo "=== [3/5] 自证 192.0.2.1:79 是真黑洞 ==="

BH_RC=0
BH_T0=$(date +%s%N)
timeout 2 bash -c 'exec 3<>/dev/tcp/192.0.2.1/79' 2>/dev/null || BH_RC=$?
BH_MS=$(( ($(date +%s%N) - BH_T0) / 1000000 ))

case "$BH_RC" in
  124)
    echo "  ✓ ${BH_MS}ms 后超时 —— SYN 确实被丢弃，下面的超时测试前提成立"
    ;;
  0)
    fail "192.0.2.1:79 在 ${BH_MS}ms 内**连上了** —— 它不是黑洞。
       上游用它当不可达地址（BLACK_HOLE 常量），连得上的话那批连接超时测试会恒红。
       修法：容器要带 --cap-add=NET_ADMIN 起，并装上
           iptables -A OUTPUT -d 192.0.2.0/24 -j DROP
       正常走 tests/m0/docker-run.sh 会自动做这件事；直接跑本脚本则要自己装。"
    ;;
  *)
    fail "192.0.2.1:79 在 ${BH_MS}ms 内**立刻失败**（退出码 $BH_RC），不是超时。
       立刻失败给出的是 ConnectError，而测试等的是 ConnectTimedout —— 照样会红。
       多半是用了路由黑洞（EHOSTUNREACH）而不是 DROP。必须让 SYN 被**静默丢弃**：
           iptables -A OUTPUT -d 192.0.2.0/24 -j DROP"
    ;;
esac
echo

# ── [4/5] ★ ★ 自证公网可达 ─────────────────────────────────────────────────
#
# `connectors::http::*` 里有一批测试**连的是真的 1.1.1.1:443 / :80**
# （`one.one.one.one`，见 pingora-core/src/connectors/http/{mod,v1,v2}.rs）。
# 也就是说这道门**一直挂着一个没人写下来的外部前提**，而 §8 要求环境可复现。
#
# ★ ★ 这一步不修那个前提（改上游测试的目标地址是另一件事，见 FORK.md §3 的讨论），
#   它做的是**把隐式前提变成显式前提**：连不上就当场说清楚，
#   而不是让人对着 6 条名字古怪的失败去猜是不是自己刚改坏了什么。
#   ★ 判据同 [3/5]：挂在**行为**上（真连一次），不挂在「有没有网卡」上。
echo "=== [4/5] 自证 1.1.1.1 可达（那批 connectors 测试的前提）==="

net_probe() {
  local port=$1 rc=0 t0 ms
  t0=$(date +%s%N)
  timeout 5 bash -c "exec 3<>/dev/tcp/1.1.1.1/$port" 2>/dev/null || rc=$?
  ms=$(( ($(date +%s%N) - t0) / 1000000 ))
  printf '%s %s' "$rc" "$ms"
}

NET_BAD=0
for p in 443 80; do
  read -r rc ms <<EOF
$(net_probe "$p")
EOF
  if [ "$rc" -eq 0 ]; then
    printf '  ✓ 1.1.1.1:%s %sms 连通\n' "$p" "$ms"
  else
    printf '  ✗ 1.1.1.1:%s 连不上（退出码 %s，%sms）\n' "$p" "$rc" "$ms"
    NET_BAD=1
  fi
done

if [ "$NET_BAD" -ne 0 ]; then
  fail "容器上不了公网，而 connectors::http::* 那一批测试连的是真的 1.1.1.1。
       它们会一起失败，而失败信息与真正的原因毫无关系。
       ★ 这不是本仓库引入的依赖，是上游测试自带的；这里只负责**把它说清楚**。
       处置：把网络接通再跑；或 VENDOR_TESTS=0 跳过整个回归网（但那样就没有回归网了）。"
fi
echo

# ── [5/5] 跑测试 ───────────────────────────────────────────────────────────
#
# ★ 判据不是「零失败」，是「与官方原版 0.8.1 的失败集合逐项相同」。
#
# 这个容器里有 2 条测试**在官方原版上同样失败**，它们依赖容器外的行为，与 fork 无关。
# 硬要求零失败会让这道门**永远红**，而永远亮着的告警等于没有告警。
#
# ★ ★ 但也不能简单 --skip 掉：那样「某条环境性失败变成了真回归」和
#   「环境修好了、该把它从名单里删掉」这两件事都会被吞掉。所以**双向比对**。
#
# 名单的来历（可重跑的对照实验）：
#   git clone --depth 1 --branch 0.8.1 https://github.com/cloudflare/pingora   # 719ef6c
#   cargo test -p pingora-core --lib
#
# ★ ★ **「登记为环境性失败」是把问题挂起，不是把问题解决。** 名单里的每一条都欠着一次
#   根因调查 —— `test_conn_timeout` 挂了很久，查出来是 Docker 默认网络会替
#   `192.0.2.1`（RFC 5737 TEST-NET-1）应答，于是 1ms 的连接超时永远等不到；
#   docker-run.sh 装一条 `iptables … -j DROP` 把它变成真丢包之后，这条测试就真的过了。
EXPECTED_FAILURES="protocols::http::v2::server::test::test_req_header_no_eos_empty_data_with_eos"

# ★ ★ ★ 第三类：**宿主机相关**的失败（CI 上线当天量出来的）。
#
# 它既不是「在官方原版上也失败」那一类（那是 EXPECTED_FAILURES，两个方向都严），
# 也不是「偶发」那一类（那是下面那段重跑逻辑）。它是**在这台机器上稳定失败、
# 在另一台机器上稳定通过** —— 而两边跑的是同一份 fork。
#
# ⚠ ⚠ **一条在不同宿主机上给出相反结论的测试，不能充当回归信号**：
#   把它留在 EXPECTED_FAILURES 里，CI 上会因为「名单里的这条现在过了」判红；
#   把它删掉，本机会因为「出现了名单外的失败」判红。**两边都对，而门两边都红。**
#
# ── `connectors::l4::tests::test_bind_to_port_range_on_connect` 的实测记录 ──
#
#   · Docker Desktop / WSL2：**10/10 失败**　· GitHub Actions（ubuntu-24.04）：**通过**
#
#   根因在断言本身：测试把源端口夹到 **2 个**，顺序连 10 次、每次连完就 drop session，
#   然后断言「至少有一次 AddrNotAvailable」—— 成立与否取决于**宿主机回收源端口有多快**。
#   ★ 不在 fork 里改这条断言：它是上游测试**要测的那件事本身**（源端口夹紧到耗尽），
#     改断言＝改这条测试的含义，那不叫修，叫让它闭嘴。
#   ★ 代价说在明处：这一条**不再充当 fork 的回归信号**；可接受的理由是 fork 从没动过
#     `BindTo` / 端口范围那条路径（见 FORK.md 的改动清单）。
#
# ⚠ **这张表要短，而且每加一条都要带上「在哪台机器上量的、错在哪一行」**。
#   没有实测记录的条目就是一句「我猜它是环境问题」—— 那正是
#   `test_conn_timeout` 那条被挂了一年多的形状。
HOST_DEPENDENT_FAILURES="connectors::l4::tests::test_bind_to_port_range_on_connect"

# ★ ★ ★ （G45 那一轮顺手撞出来的）：这里此前**没有 `--no-fail-fast`**。
#   `pingora-core` 的 lib 测试恒有 2 条已知失败，于是 cargo **每一次都停在第一个二进制**——
#   后面 6 个 crate 的单测、pingora-core 自己的 3 个集成测试二进制、以及全部文档测试
#   **一次都没跑过**，而这道门一直是绿的、脚本自己 echo 的是「7 个 crate」。
#   实测补上 `--no-fail-fast` 之后多跑出 **64 条单测 + 3 个文档测试**：
#     server_phase_fastshutdown 1 · gracefulshutdown 1 · test_basic 3 ·
#     pingora-error 8 · pingora-http 8 · pingora-pool 9 · pingora-runtime 3 ·
#     pingora-rustls 18 · pingora-timeout 13
#   ★ 是被 G45 逼出来的：那一轮给 pingora-rustls 新写了 18 条测试，跑完总数却还是 359——
#     **「新加的测试没让计数变化」是这类缺陷唯一会露头的地方**。
#   ★ 判据本身不用改：失败集合是扫全部 `test ... FAILED` 行得来的，跨二进制天然成立。
echo "=== [5/5] cargo test（vendor/pingora，8 个 crate + 集成测试 + 文档测试）==="
echo "  ★ 已知环境性失败 1 条（在官方 0.8.1 上同样失败，见脚本注释）："
# ★ 一行一条，**不靠词拆分**。这与 tests/m0/docker-run.sh 的 CRLF 门是**同一个缺陷**，
#   那边上午修掉了，这边直到同日下午 shellcheck 上线才被揪出来——
#   ★ 「修完一个形状要当场把同形的全扫一遍」，靠人眼扫是扫不干净的，得有工具。
printf '%s\n' "$EXPECTED_FAILURES" | while IFS= read -r ef; do
  [ -n "$ef" ] && printf '      %s\n' "$ef"
done

OUTFILE=${VENDOR_TEST_OUT:-/tmp/vendor-test.out}
# --locked：锁文件已入库，测试不许顺手改它（同 G29 第 2 条的精神）
# --target-dir：落进 docker 命名卷，不写 Windows 挂载点，也不污染 vendor 目录
set +e
cargo test --manifest-path "$MANIFEST" --target-dir "$TARGET" --locked \
  --features pingora-core/rustls --no-fail-fast 2>&1 | tee "$OUTFILE"
CARGO_RC=${PIPESTATUS[0]}
set -e

# 编译不过 / 跑不起来，和「测试跑完有失败」是两回事，不能混在一起判
if ! grep -q "^test result:" "$OUTFILE"; then
  fail "cargo test 没能跑到出结果（退出码 $CARGO_RC）——多半是编译错误，看上面的输出"
fi

# 用字段而不是正则，避开 `test result: FAILED.` 那一行
awk '$1 == "test" && $(NF-1) == "..." && $NF == "FAILED" { print $2 }' "$OUTFILE" \
  | sort -u > /tmp/vendor-actual.f
printf '%s\n' "$EXPECTED_FAILURES" | sed '/^$/d' | sort -u > /tmp/vendor-expected.f
printf '%s\n' "$HOST_DEPENDENT_FAILURES" | sed '/^$/d' | sort -u > /tmp/vendor-hostdep.f

# ★ ★ ★ **自证：宿主机相关名单里的每一条都必须真的跑过。**
#
# 少了这一条，一个**打错字的**条目会安静地什么都不做：它永远不匹配任何东西，
# 于是「两个方向都豁免」变成「豁免了一个不存在的测试」，而门照样全绿。
# ⚠ 这与本仓库反复抓到的「不自省的名单」是同一个形状
#   （vendor 那张 16 项手写清单、`unclaimed.sh` 的忽略名单、`supply-audit.py` 的 ACCEPTED）。
# 判据取「这个名字在测试输出里出现过」——出现成 ok 还是 FAILED 都行，那正是本类的定义。
while IFS= read -r hd; do
  [ -n "$hd" ] || continue
  if ! awk -v t="$hd" '$1 == "test" && $2 == t { found = 1 } END { exit !found }' "$OUTFILE"; then
    fail "宿主机相关名单里的这一条 **在本次测试输出里根本没出现过**：$hd
       名字写错了？还是上游把这条测试删了/改名了？
       ★ 一个永远不匹配的豁免条目，与没有这条豁免在门上看不出区别，
         但它会让人以为那条测试还在被看着。"
  fi
done < /tmp/vendor-hostdep.f

# ★ 摘出去之后再比对。⚠ grep 无匹配时返回 1，set -e 下要接住。
grep -vxF -f /tmp/vendor-hostdep.f /tmp/vendor-actual.f > /tmp/vendor-actual2.f || true
mv /tmp/vendor-actual2.f /tmp/vendor-actual.f

comm -23 /tmp/vendor-actual.f /tmp/vendor-expected.f > /tmp/vendor-new.f
comm -13 /tmp/vendor-actual.f /tmp/vendor-expected.f > /tmp/vendor-gone.f

echo
# ★ 名单外的失败先**定向重跑一次**再定性。偶发有**两个已知来源**：
#
#   ① 时间竞态。上游的 `protocols::l4::stream::tests::test_rx_timestamp` 自己的注释写着
#      「setsockopt for SO_TIMESTAMPING is asynchronous so sleep a little bit」，而那个 sleep
#      只有 100µs。空载连跑 12 次全过，机器有负载时会挂在 `assert!(rx_ts.is_some())`。
#
#   ② `connectors::*` 那一批，偶发过几次而**归因没有查实**（「公网抖动」解释不了其中那条
#      纯本地的 UDS 测试）。⚠ **在查实之前不许把它们登记进 EXPECTED_FAILURES。**
#
#   ★ 一条相关事实：`connectors::*` 里那批测试**连的是真的 1.1.1.1:443 / :80**
#     （见 `pingora-core/src/connectors/http/{mod,v1,v2}.rs`）——**这道门一直悄悄依赖容器
#     能上公网**，而 §8 要求环境可复现。真要处置，方向是把它们指向容器内的假上游。
#
#   ★ ★ **不能把偶发项塞进 EXPECTED_FAILURES**——它平时是过的，塞进去会让「少一条也红」
#     那条规则每次都触发。也不能放任它间歇性红：那会训练人「重跑一次就好」，
#     等于废掉这道门。所以：重跑一次，仍失败才判红，**而且不稳定要打出来**。
if [ -s /tmp/vendor-new.f ]; then
  echo "★ 名单外的失败，定向重跑一次以区分「真回归」与「偶发（时间竞态 / 公网抖动）」："
  sed 's/^/      /' /tmp/vendor-new.f
  RETRY_ARGS=()
  while IFS= read -r t; do RETRY_ARGS+=("$t"); done < /tmp/vendor-new.f
  set +e
  cargo test --manifest-path "$MANIFEST" --target-dir "$TARGET" --locked \
    --features pingora-core/rustls --no-fail-fast \
    -- --exact "${RETRY_ARGS[@]}" > /tmp/vendor-retry.out 2>&1
  set -e
  awk '$1 == "test" && $(NF-1) == "..." && $NF == "FAILED" { print $2 }' /tmp/vendor-retry.out \
    | sort -u > /tmp/vendor-still.f
  comm -23 /tmp/vendor-new.f /tmp/vendor-still.f > /tmp/vendor-flaky.f
  if [ -s /tmp/vendor-flaky.f ]; then
    echo "  ⚠ 重跑通过 —— 判定为**不稳定**，不判红（但请注意它确实红过一次）："
    sed 's/^/      /' /tmp/vendor-flaky.f
  fi
  cp /tmp/vendor-still.f /tmp/vendor-new.f
  if [ -s /tmp/vendor-new.f ]; then
    echo "  ★ 重跑仍失败 —— 这才是 fork 该被判红的东西："
    sed 's/^/      /' /tmp/vendor-new.f
  fi
fi
if [ -s /tmp/vendor-gone.f ]; then
  echo "★ 名单里的这几条**现在过了**——环境变了，把它们从 EXPECTED_FAILURES 删掉："
  sed 's/^/      /' /tmp/vendor-gone.f
fi
# ★ ★ 宿主机相关的那几条：两个方向都不判红，但**必须说出这一轮是哪个方向**。
#   一条被豁免又不被报告的测试，等于从这道门上消失了。
if [ -s /tmp/vendor-hostdep.f ]; then
  echo "★ 宿主机相关（两个方向都不判红，见脚本里的实测记录）："
  while IFS= read -r hd; do
    [ -n "$hd" ] || continue
    if awk -v t="$hd" '$1 == "test" && $2 == t && $NF == "FAILED" { f = 1 } END { exit !f }' "$OUTFILE"; then
      printf '      本轮**失败**：%s\n' "$hd"
    else
      printf '      本轮**通过**：%s\n' "$hd"
    fi
  done < /tmp/vendor-hostdep.f
fi
# ★ 这里必须用 if，不能写 `[ -s f ] && fail ...`——set -e 下条件为假时
#   整条 && 列表返回 1，脚本会当场静默退出，红绿全乱。
if [ -s /tmp/vendor-new.f ]; then fail "出现了官方原版上没有的失败"; fi
if [ -s /tmp/vendor-gone.f ]; then fail "已知失败名单已过期，请更新"; fi

echo "VENDOR TESTS PASSED —— 失败集合与官方原版 0.8.1 逐项相同（宿主机相关的那几条已单列），fork 没有引入新的回归。"
