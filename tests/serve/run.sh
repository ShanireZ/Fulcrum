#!/usr/bin/env bash
# 数据面端到端：起两个 `fulcrum serve`，用真流量验证路由决策**被执行对了**。
#
# ★ ★ 这一层与 `crates/fulcrum-runtime/tests/routing.rs` 分工明确：
#   那边测「该做什么」（纯逻辑、脱网、红了指到具体规则），
#   这边测「做出来了没有」（真 socket、真 HTTP、真上游）。
#   **两边都要有**：只有前者，一个把决策算对却写错响应的数据面照样全绿；
#   只有后者，一条规则错了只会得到一个状态码，查起来要靠猜。
#
# ★ **上游就是枢衡自己**（另一个 `fulcrum serve` 实例）。这不是偷懒：
#   它让整套测试**零外部依赖**，而且上游那边用 `{path}` / `{header.X-Up}`
#   把收到的东西回显出来——于是「rewrite 有没有带到上游」「header_up 有没有生效」
#   这两件事可以被**看见**，而不是只能推断。
set -euo pipefail

REPO=${REPO:-/w}
cd "$REPO"
BIN="$REPO/target/release/fulcrum"
WORK=$(mktemp -d)
HOST=127.0.0.1
PROXY_PORT=${PROXY_PORT:-9100}
UP_PORT=${UP_PORT:-9101}
NAMED_PORT=${NAMED_PORT:-9102}
TLS_PORT=${TLS_PORT:-9103}
# ★ 批 11（主动健康检查）用的两个上游：
#   SICK_PORT —— 进程好好的、业务路径回 200，**但探测路径回 500**；
#   LATE_PORT —— 一开始**没人在听**，测到一半才起来（验「摘得掉」也「回得来」）。
# ⚠ 名字有意不叫 `*UP_PORT`：下面那条 `sed "s/:UP_PORT/…/g"` 是全局替换，
#   而本文件已经为「一条只认得一种写法的替换」栽过一次。
SICK_PORT=${SICK_PORT:-9104}
LATE_PORT=${LATE_PORT:-9105}
# ★ 9106 / 9107 现在是空的（回落层删除后两个端口退场），AGENTS.md 的端口表已跟着改。
# ★ 管理面走 Unix socket（G14），所以它**不占端口**，也不进上面那张端口表。
ADMIN_SOCK="$WORK/admin.sock"

FAILS=0
PIDS=()

fail() { echo "  ✗ $*" >&2; FAILS=$((FAILS + 1)); }
ok() { echo "  ✓ $*"; }

cleanup() {
  local pid
  # ★ 先还原 /etc/hosts —— 备份就在 $WORK 里，而本函数末尾会把 $WORK 删掉。
  #   这台容器后面还要跑别的场景，留下一条 rotate.test 是给后面下绊子。
  [ -f "$WORK/hosts.orig" ] && cat "$WORK/hosts.orig" > /etc/hosts 2>/dev/null
  true
  # ★ 用 SIGINT 而不是 SIGTERM：Pingora 把 SIGTERM 当**优雅停机**，会等完整的排空窗口；
  #   SIGINT 是立刻退。测试收尾要的是后者——等排空只会让每次跑都慢几秒，
  #   而 bash 随后还会为被 SIGKILL 收掉的后台任务打一行 `Killed` 噪音。
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill -INT "$pid" 2>/dev/null || true
  done
  # ★ 等它们真的走掉。不等的话，下一次跑会撞上「端口还被占着」，
  #   而那看起来像是本次的新问题。
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 50 ]; do
      sleep 0.1
      waited=$((waited + 1))
    done
    kill -9 "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
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

# ── [0/4] 基线：四个端口必须**都还没被占** ──────────────────────────────────
#
# ★ ★ 这一步不是形式。本仓库栽过一次：一个「基线探针」对着**上一个场景遗留的进程**
#   报了绿。端口没清干净时，后面每一条断言测的都是别人的服务。
echo "=== [0/4] 基线：端口未被占用 ==="
for p in "$PROXY_PORT" "$UP_PORT" "$NAMED_PORT" "$TLS_PORT" "$SICK_PORT" "$LATE_PORT"; do
  if port_listening "$p"; then
    echo "SERVE TESTS FAILED: 端口 $p 已经被占用了 —— 先清掉再跑，否则下面测的是别人的服务。" >&2
    exit 1
  fi
done
ok "$PROXY_PORT / $UP_PORT / $NAMED_PORT / $TLS_PORT / $SICK_PORT / $LATE_PORT 都是空的"

[ -x "$BIN" ] || {
  echo "SERVE TESTS FAILED: 找不到 $BIN（先跑 cargo build --release）" >&2
  exit 1
}

# ── 配置 ────────────────────────────────────────────────────────────────────
#
# 上游把收到的**路径**与 `X-Up` 头回显出来 —— 于是 rewrite 与 header_up
# 是否真的生效，可以直接从响应体里读出来。
# ★ 回落层已整层删除（G98），它留下的那句话仍然写在数据面的模块文档里：
#   **501 与 502 是两个不同的事实，不许合并** —— 合并之后，一次配置遗漏
#   与一次后端故障在现场长得一模一样。
cat > "$WORK/upstream.Fulcrumfile" <<'CONF'
:UP_PORT {
    respond 200 "up path={path} xup={header.X-Up}"
}
CONF
sed -i "s/:UP_PORT/:$UP_PORT/" "$WORK/upstream.Fulcrumfile"

# ★ ★ ★ 「病号」上游：**业务路径完全正常，只有探测路径回 500**。
#
#   它是本批最重要的那个夹具。⚠ 少了它，「健康检查」与「连得上吗」
#   在判据上分不开 —— 而一个只会 TCP 连一下的实现会全绿。
cat > "$WORK/sick.Fulcrumfile" <<'CONF'
:SICK_PORT {
    # ⚠ 回落层整层删除（G98）之后，这里那条守「没配回落 ⇒ 501」的判据没了对象。
    #   ★ **改判据的时候要看清楚原来那条在守什么，别把它一起搬走。**
    handle /health {
        respond 500 sick-health
    }
    handle {
        respond 200 "sick but serving path={path}"
    }
}
CONF
sed -i "s/:SICK_PORT/:$SICK_PORT/" "$WORK/sick.Fulcrumfile"

# 「迟到」的上游：这份配置先写好，但**进程要到本场景中途才起**。
# ★ ★ 一条值得继承的取值方法（缓存那一格用的也是它）：
#   让上游回显一个客户端现给的值，把「这份东西是谁给的」变成**看得见**的。

cat > "$WORK/late.Fulcrumfile" <<'CONF'
:LATE_PORT {
    respond 200 "late path={path}"
}
CONF
sed -i "s/:LATE_PORT/:$LATE_PORT/" "$WORK/late.Fulcrumfile"

cat > "$WORK/proxy.Fulcrumfile" <<'CONF'
{
    admin unix/ADMIN_SOCK
    # ★ 全局 `default_sni`：客户端不带 SNI 时当作它报了这个名字（判据 9d）。
    # ⚠ 它同时让 9c 那条**变得有意义** —— 配着 default_sni 还要拒掉未知 SNI，
    #   才说明这条指令没有顺手变成「谁来都发一张证书」的兜底。
    default_sni secure.example
}

# ⚠ ⚠ ⚠ **这个站点里「写了 `id`」与「没写 `id`」两种形状都是有意的**
#   （M2 批 N 任务 2.9 / G125，裁决 R6 ③ **第二轮**）。
#   覆盖层的键是 `(站点名, id, 归一化后的上游地址)`，而代码按「键相同 / 键不同」分支
#   ⇒ 夹具必须**两种形状都有**，否则其中一条路在端到端这一层一次都没走过
#   （AGENTS.md 门禁纪律第一条：盲区在夹具里）。
#   ① **没写 `id`**：`/api` · `/rw` · `/cached` · `/hc` 四条都指着 `127.0.0.1:UP_PORT`
#      ⇒ 四条的键**完全相同**，共享同一个覆盖格子，一次 `disable` 四条一起摘掉。
#      ★ 这正是反代最常见的写法（一个后端挂在几组 `handle` 路由后面），而它
#      **一个字节都不用改就装得上** —— 这份夹具就是那句话的实测。
#      ⚠ 任务 2.8 曾照第一轮口径把这个形状在装载期拒掉，四个端到端场景当场全红。
#   ② **写了 `id`**：`/sick` 与 `/sickok` 都指着 `127.0.0.1:SICK_PORT`，而两条的
#      `health_status` 口径不同 ⇒ 写 `id` 才能把它们分成两格、各摘各的。
:PROXY_PORT {
    header X-Fulcrum test
    @api path /api/*
    handle @api {
        reverse_proxy 127.0.0.1:UP_PORT {
            header_up X-Up 1
            passive_fail 3
        }
    }
    handle /rw/* {
        rewrite * /rewritten
        reverse_proxy 127.0.0.1:UP_PORT
    }
    handle /redir {
        redir * https://example.com/moved 301
    }
    # ⚠ ⚠ **这一格（M2 批 F）换了方向。**
    #   旧契约：`file_server` 回落给 nginx ⇒ `/static/x` 应当回 200 且**响应体来自
    #   回落后端**（`fallback-nginx path=/static/x`）。
    #   新契约：`file_server` **自研** ⇒ `/static/x` 应当回 200 且响应体是**磁盘上那个文件**。
    #   ★ 换的不是「测不测回落」，是「file_server 归谁」——
    #   「回落已接线 ⇒ 转发」那条判据由下面的 `/cached/*` 全额接住，一条都没少。
    handle /static/* {
        file_server {
            root WWW_ROOT
        }
    }
    # ⚠ ⚠ **这一格（M2 批 G）换了方向。**
    #   旧契约：`cache` 回落给 nginx ⇒ `/cached/x` 回 200 且**响应体来自回落后端**。
    #   新契约：`cache` **自研** ⇒ 它是**中间件**，裹住下面那条 `reverse_proxy`。
    #   ★ 这里只验「配了 `cache` 不会把转发弄坏」—— 缓存语义本身由
    #   **`tests/cache/run.sh`** 管。⚠ 两处各测各的，别在这里重复一遍：
    #   一份夹具同时背两套判据，改的时候两套都会被顺手改坏。
    handle /cached/* {
        cache {
            ttl 30s
        }
        reverse_proxy 127.0.0.1:UP_PORT
    }
    handle /err {
        respond 418 teapot
    }
    handle /byname/* {
        reverse_proxy localhost:UP_PORT
    }
    handle /deadname/* {
        reverse_proxy no-such-host-zzz.invalid:80
    }
    handle /rotate/* {
        reverse_proxy rotate.test:UP_PORT {
            dns_refresh 5s
        }
    }
    handle /hc/* {
        reverse_proxy 127.0.0.1:UP_PORT 127.0.0.1:LATE_PORT {
            health_uri /health
            health_interval 1s
            health_timeout 1s
        }
    }
    handle /sick/* {
        reverse_proxy 127.0.0.1:SICK_PORT {
            id sick
            health_uri /health
            health_interval 1s
            health_timeout 1s
        }
    }
    handle /sickok/* {
        reverse_proxy 127.0.0.1:SICK_PORT {
            id sickok
            health_uri /health
            health_status 5xx
            health_interval 1s
            health_timeout 1s
        }
    }
    handle {
        respond 200 root
    }
}

http://only.example:NAMED_PORT {
    respond 200 named-only
}

secure.example:TLS_PORT {
    tls TLS_CRT TLS_KEY
    respond 200 secure-ok
}
CONF

# ── 自签证书（给 `tls <cert> <key>` 那条路用）────────────────────────────────
#
# ★ 用 openssl 现签一张，而**不是**把一张证书提交进仓库：
#   仓库里的测试证书迟早会过期，而过期那天红的是「TLS 坏了」，
#   查起来要绕一圈才发现是证书到期。现签的永远新鲜。
# ⚠ SAN 必须有 `secure.example`：枢衡是**按证书自己的 SAN** 决定这张证书装在哪些
#   SNI 上的（配置里写的站点名只用来核对），只给 CN 是装不上的。
openssl req -x509 -newkey rsa:2048 -sha256 -days 2 -nodes \
  -keyout "$WORK/tls.key" -out "$WORK/tls.crt" \
  -subj "/CN=secure.example" \
  -addext "subjectAltName=DNS:secure.example" \
  -addext "basicConstraints=critical,CA:TRUE" \
  >/dev/null 2>&1 || {
  echo "SERVE TESTS FAILED: openssl 生成自签证书失败" >&2
  exit 1
}

# ⚠ `UP_PORT` 在两种前缀下都出现（`127.0.0.1:` 与 `localhost:`），两边都要替。
#   ★ **一条只认得一种写法的替换，在另一种写法上等于没有替换。**
sed -i "s/:PROXY_PORT/:$PROXY_PORT/; s/:UP_PORT/:$UP_PORT/g; s/:NAMED_PORT/:$NAMED_PORT/; s/:TLS_PORT/:$TLS_PORT/; s/:SICK_PORT/:$SICK_PORT/g; s/:LATE_PORT/:$LATE_PORT/g" \
  "$WORK/proxy.Fulcrumfile"
# ★ 自研 file_server 的根 —— 内容在这里现造（M2 批 F）。
#   ⚠ 必须是**绝对路径**（G91），`$WORK` 本来就是。
mkdir -p "$WORK/www/static"
printf 'self-built-file-server\n' > "$WORK/www/static/x"
# 路径里有斜杠，用 | 当分隔符
sed -i "s|TLS_CRT|$WORK/tls.crt|; s|TLS_KEY|$WORK/tls.key|; s|ADMIN_SOCK|$ADMIN_SOCK|; s|WWW_ROOT|$WORK/www|" \
  "$WORK/proxy.Fulcrumfile"

# ── ★ ★ ★ 会变的域名：给「后台重解析」准备夹具（批 10）──────────────────────
#
# ⚠ `/byname/` 与 `/deadname/` 只验到**启动那一次**解析对了。
#   「每 N 秒重解析一次」是另一件事，而它坏掉的样子**完全无声**：
#   启动那次照常成功，此后地址永远停在启动时那一份 —— 这正是 nginx OSS
#   那个经典事故（`dns_refresh` 这条指令存在的全部理由）。
#   ⇒ 判据必须让**同一个域名在进程活着的时候换 IP**，再看它跟不跟得上。
#
# ★ 手法：直接改容器的 `/etc/hosts`。glibc 的 nss_files 每次查询都重读它，
#   进程内没有缓存 —— 所以这是一次真的「DNS 答案变了」。
# ⚠ Docker 把 /etc/hosts 做成 bind-mount 的**文件**：只能**就地截断重写**
#   （`cat >`）。`sed -i` / `mv` 要换 inode，在 bind-mount 的文件上做不到。
cp /etc/hosts "$WORK/hosts.orig"
set_rotate_ip() {
  { cat "$WORK/hosts.orig"; echo "$1 rotate.test"; } > /etc/hosts 2>/dev/null
}

# ⚠ 写不进去就**当场退出**，不「跳过」。
#   ★ 纪律：「没能检查」不许当成「检查通过」。
if ! set_rotate_ip 127.0.0.1; then
  echo "SERVE TESTS FAILED: 改不了 /etc/hosts —— 后台重解析那一节没法验。" >&2
  echo "  容器默认以 root 跑；传了 DOCKER_USER 就会撞到这里。" >&2
  exit 1
fi
ok "rotate.test 先指向真上游（127.0.0.1）"

# ── [1/4] 起服务 ────────────────────────────────────────────────────────────
echo "=== [1/4] 起上游与代理 ==="
start() {
  local name=$1 conf=$2
  RUST_LOG=${RUST_LOG:-info} "$BIN" serve "$conf" \
    --bind-host "$HOST" \
    --pid-file "$WORK/$name.pid" \
    --upgrade-sock "$WORK/$name.sock" \
    > "$WORK/$name.log" 2>&1 &
  PIDS+=($!)
}
start upstream "$WORK/upstream.Fulcrumfile"
start sick "$WORK/sick.Fulcrumfile"
# ⚠ `late` **故意不起** —— 它要到 [2c/4] 那一节才上场。
start proxy "$WORK/proxy.Fulcrumfile"

for p in "$UP_PORT" "$SICK_PORT" "$PROXY_PORT" "$NAMED_PORT" "$TLS_PORT"; do
  wait_port "$p" || {
    echo "SERVE TESTS FAILED: 端口 $p 起不来。日志：" >&2
    cat "$WORK"/*.log >&2
    exit 1
  }
done
ok "六个监听都起来了（迟到那个还没起，那是有意的）"
# ★ ★ 一条「靠某个前提成立」的判据，要**把那个前提也测一遍** ——
#   否则前提悄悄不成立的那天，判据仍然是绿的。

# ── [2/4] 断言 ──────────────────────────────────────────────────────────────
echo "=== [2/4] 断言 ==="

# ★ ★ 统一的 curl 包装。**退出码与 `-w` 的输出必须分开拿。**
#
# ⚠ 不要写成 `curl … -w '%{http_code}' || echo "000"`：curl 连不上时 `-w`
#   **已经输出了 `000`**，`|| echo` 再补一个就成了 `000000`。
#   ★ **先弄清工具已经做了什么，再决定补什么。**
# 只取 `-w` 的输出（不关心退出码）。
# ⚠ `|| true` 是必需的：`set -e` 之下，赋值里的命令替换失败会让整个脚本退出。
run_curl() {
  curl "$@" 2>/dev/null || true
}

# ★ ★ 要**退出码**时不能走命令替换。
#
# ⚠ 不要写成「函数里 `CURL_RC=$?`、调用方 `X=$(run_curl …)`」：
#   **命令替换开的是子 shell，里面设的变量不会传回父 shell**，
#   于是 `CURL_RC` 永远是初值 0，读起来像「curl 成功了」。
# 结果写文件，退出码留在当前 shell 的 `CURL_RC` 里。
CURL_RC=0
curl_capture() {
  set +e
  curl "$@" > "$WORK/curlw" 2>/dev/null
  CURL_RC=$?
  set -e
}

# ★ ★ ★ 通用版（任意命令可能非零退出、又想要它的输出时，用它代替裸的
#   `VAR=$(cmd)`；修复轮 2，评审 N1）连同「它为什么必须存在」都搬进了
#   `tests/lib/capture.sh`（任务 7 收敛：本文件与 `tests/metrics/run.sh` 曾各有一份）。
#   ⚠ 上面那个 `curl_capture()` **有意留在本文件**：它只服务 curl，且把正文落到
#   `$WORK/curlw` 而不是变量里，与通用版不是同一件事。
# shellcheck source=tests/lib/capture.sh
. "$REPO/tests/lib/capture.sh"

# 取状态码 + 响应体 + 某个响应头，一次请求全拿到。
probe() {
  local url=$1 host_hdr=${2:-}
  local args=(-s -o "$WORK/body" -D "$WORK/hdr" -w '%{http_code}' --max-time 5)
  [ -z "$host_hdr" ] || args+=(-H "Host: $host_hdr")
  run_curl "${args[@]}" "$url"
}

expect_status() {
  local what=$1 want=$2 got=$3
  if [ "$got" = "$want" ]; then ok "$what → $got"; else fail "$what 期望 $want，实际 $got"; fi
}

expect_body() {
  local what=$1 want=$2
  local got
  got=$(cat "$WORK/body")
  if [ "$got" = "$want" ]; then ok "$what 体 = $got"; else fail "$what 体期望「$want」，实际「$got」"; fi
}

expect_header() {
  local what=$1 name=$2 want=$3
  local got
  # 头名大小写不敏感；只取第一次出现，去掉行尾的 CR。
  got=$(grep -i "^$name:" "$WORK/hdr" | head -1 | cut -d' ' -f2- | tr -d '\r')
  if [ "$got" = "$want" ]; then ok "$what 的 $name = $got"; else fail "$what 的 $name 期望「$want」，实际「$got」"; fi
}

BASE="http://$HOST:$PROXY_PORT"

# 1) 兜底 handle + 站点级 header（G49：header 排在 handle 之前，所以两者都生效）
expect_status "GET /" 200 "$(probe "$BASE/")"
expect_body "GET /" "root"
expect_header "GET /" "X-Fulcrum" "test"

# 2) ★ 转发：命名匹配器 + header_up 都要真的生效
expect_status "GET /api/x（转发）" 200 "$(probe "$BASE/api/x")"
expect_body "GET /api/x（转发）" "up path=/api/x xup=1"

# 3) ★ rewrite 要带到上游去
expect_status "GET /rw/y（改写后转发）" 200 "$(probe "$BASE/rw/y")"
expect_body "GET /rw/y（改写后转发）" "up path=/rewritten xup="

# 4) 重定向
expect_status "GET /redir" 301 "$(probe "$BASE/redir")"
expect_header "GET /redir" "Location" "https://example.com/moved"

# 5) respond 带状态码与体
expect_status "GET /err" 418 "$(probe "$BASE/err")"
expect_body "GET /err" "teapot"

# 6) ★ 自研的两块（`file_server` 批 F、`cache` 批 G）
#
# ★ 判据取**响应体**而不只是状态码：只看 200 的话，一个仍然在回落的实现照样绿
#   （回落后端也回 200）。响应体是唯一能把「自研发的」与「别人发的」分开的东西。
expect_status "GET /static/x（file_server 自研 → 直接发文件）" 200 "$(probe "$BASE/static/x")"
#   ★ `expect_body` 是**精确**比对，所以它同时也是反向那一半：一个仍然在回落的
#   实现会得到 `fallback-nginx path=/static/x …`，当场不等。
expect_body "GET /static/x（file_server 自研 → 直接发文件）" "self-built-file-server"
# ★ 自研之后 Content-Type 由 G90 那张小表决定。`x` 没有扩展名 ⇒ 缺省值。
#   ⚠ 这一条是「表真的被查了」的唯一痕迹 —— 回落后端给的会是它自己的判断。
expect_header "GET /static/x" "Content-Type" "application/octet-stream"
# ★ `cache` 现在是**中间件**（批 G），裹住 `/cached/*` 那条 `reverse_proxy`。
#   这里只验「配了 `cache` 不会把转发弄坏」—— 判据取**回显出来的路径**，
#   而不只是状态码：它证明请求是原样被带到上游的。
#   ⚠ 缓存语义本身（命中/回源/Vary/Authorization 不串号/防惊群）由 `tests/cache/run.sh` 管。
expect_status "GET /cached/x（cache 裹着 reverse_proxy）" 200 "$(probe "$BASE/cached/x")"
expect_body "GET /cached/x（cache 裹着 reverse_proxy）" "up path=/cached/x xup="
# ★ ★ 第二次必须**命中**，而命中这件事靠状态码看不出来（两次都是 200）
#   ⇒ 看 `X-Fulcrum-Cache`。⚠ 少了这一条，一个「装上缓存却从不命中」的实现
#   在本场景里与正确实现完全一样。
probe "$BASE/cached/x" > /dev/null
expect_header "GET /cached/x 第二次" "X-Fulcrum-Cache" "HIT"

# 7) ★ ★ 无站点匹配 → 421（G63）。`:9102` 上只有一个具名站点。
NAMED="http://$HOST:$NAMED_PORT"
expect_status "Host=only.example" 200 "$(probe "$NAMED/" only.example)"
expect_body "Host=only.example" "named-only"
expect_status "Host=nope（无站点匹配）" 421 "$(probe "$NAMED/" nope.example)"

# 8) keep-alive：一次连接上两个请求
# ⚠ `-o` 是**按 URL** 生效的，不是全局的：两个 URL 只给一个 `-o /dev/null`，
#   第二个响应体会漏到 stdout 里，把 `-w` 的输出搅成 `200,teapot418,`。
#   ★ 这不是 curl 的坑，是「以为选项是全局的」——每个 URL 配一个 `-o`。
KA=$(curl -s -o /dev/null -o /dev/null -w '%{http_code},' --max-time 5 "$BASE/" "$BASE/err" 2>/dev/null || echo "000,")
if [ "$KA" = "200,418," ]; then ok "同一连接上两个请求：$KA"; else fail "keep-alive 期望 200,418, 实际 $KA"; fi

# ── 9) ★ ★ TLS：按 SNI 动态挑证书（§5.1 第 1 条锁死的那条路）────────────────
#
# ★ 动态挑证书走的是 BoringSSL 的 `set_select_certificate_callback`（G104），
#   两个入口（h1/h2 与 h3）共用同一个回调。
TLSURL="https://secure.example:$TLS_PORT/"
RESOLVE="secure.example:$TLS_PORT:$HOST"

# 9a) 证书链要**真的能验过**——用 `--cacert` 而不是 `-k`。
#     ★ `-k` 只能证明「握手成功了」，证不了「服务端给的是我们那张证书」。
TLS_CODE=$(curl -s -o "$WORK/body" -w '%{http_code}' --max-time 5 \
  --cacert "$WORK/tls.crt" --resolve "$RESOLVE" "$TLSURL" 2>/dev/null || echo "000")
expect_status "HTTPS（--cacert 验签）" 200 "$TLS_CODE"
expect_body "HTTPS（--cacert 验签）" "secure-ok"

# 9b) ★ ALPN 要能协商到 h2。`enable_h2()` 少调一次，这里会掉回 1.1。
H2V=$(run_curl -s -o /dev/null -w '%{http_version}' --max-time 5 --http2 \
  --cacert "$WORK/tls.crt" --resolve "$RESOLVE" "$TLSURL")
if [ "$H2V" = "2" ]; then ok "HTTPS 上 ALPN 协商到 HTTP/2"; else fail "HTTPS 期望 http_version=2，实际 $H2V"; fi

# 9c) ★ ★ 没有证书的 SNI 必须**握手失败**，而不是拿一张不匹配的证书去应答。
#     后者会让客户端看到证书错误，而运维在服务端只看到一次成功的握手。
#     ★ 判据挂在 **curl 的退出码**上，不只挂在 `%{http_code}` 上：后者连不上时是 `000`，
#     而 `000` 也可能来自超时、端口没开、DNS 失败——分不出「被拒绝握手」这一种。
curl_capture -s -o /dev/null -w '%{http_code}' --max-time 5 -k \
  --resolve "nocert.example:$TLS_PORT:$HOST" "https://nocert.example:$TLS_PORT/"
NOCERT=$(cat "$WORK/curlw")
if [ "$CURL_RC" -ne 0 ] && [ "$NOCERT" = "000" ]; then
  ok "未知 SNI 被拒绝握手（curl 退出码 $CURL_RC），没有拿别人的证书应答"
else
  fail "未知 SNI 期望握手失败，实际 curl 退出码 $CURL_RC、HTTP $NOCERT"
fi

# 9d) ★ ★ 不带 SNI 的客户端（老客户端、或直接按 IP 访问）→ 全局 `default_sni`。
#
#     ⚠ ⚠ 这条指令曾经是**「DSL 认得、编译得过、运行时零调用方」**的活样本：
#     `SniResolver::set_default` 全仓唯一的调用方在一条 `#[cfg(test)]` 里，
#     而它又**不在 `UNWIRED` 里** ⇒ 装载日志一个字都不说，配了它的人只会看到
#     不带 SNI 的客户端照样被拒绝握手。
#     ★ 判据挂在**拿到的是哪一张证书**上，不挂在「握手成功了吗」上：后者在服务端
#     随便发一张证书时同样成立，而那正是 9c 要防的那种「成功」。
#     ★ 与它成对的反向那半就是上面的 9c —— 本场景现在**配着** default_sni，
#     所以 9c 从此还多守一件事：这条指令没有顺手变成未知 SNI 的兜底
#     （Caddy 把那件事分成另一个选项 `fallback_sni`，我们没有它）。
NOSNI_SUBJ=$(echo | openssl s_client -connect "$HOST:$TLS_PORT" -noservername 2>/dev/null \
             | openssl x509 -noout -subject 2>/dev/null || true)
case "$NOSNI_SUBJ" in
  *secure.example*) ok "不带 SNI 的握手拿到 default_sni 那张证书（$NOSNI_SUBJ）" ;;
  *) fail "不带 SNI 期望拿到 secure.example 那张证书，实际：${NOSNI_SUBJ:-（一张都没拿到，多半是握手被拒）}" ;;
esac

# ── ★ ★ ★ 域名上游（批 10）──────────────────────────────────────────────────
#
# ⚠ ⚠ **这两条补的是一个具体的夹具缺口**：在它们之前，本场景里**所有上游
#   都写的是 `127.0.0.1:端口`** —— 而 IP 字面量**不走 DNS**。
#   于是「域名上游」这条路从来没有被任何一道门喂过，而它上面躺着一个真缺陷：
#   `HttpPeer::new` 在**每个请求**上做一次阻塞 `getaddrinfo`，失败还 panic
#   （实测：每请求一次 panic，客户端只看到连接被丢弃，不是干净的 502）。
#
# ★ 判据两个方向都要：**解析得出来的照常转发**，**解析不出来的回干净的 502**。
expect_status "GET /byname/x（域名上游，能解析）" 200 "$(probe "$BASE/byname/x")"
expect_body "GET /byname/x（域名上游，能解析）" "up path=/byname/x xup="

# ★ ★ 解析不出来的那个：**502，而不是连接被丢弃**。
#   ⚠ 判据必须同时看状态码与 curl 的退出码 —— 改之前这里拿到的是
#   `000`（连接断了）而不是 502，而只断言「不是 200」两者都满足。
curl_capture -s -o /dev/null -w '%{http_code}' --max-time 10 "$BASE/deadname/x"
DEAD_CODE=$(cat "$WORK/curlw")
if [ "$CURL_RC" -eq 0 ] && [ "$DEAD_CODE" = "502" ]; then
  ok "★ 解析不出来的域名上游 → 干净的 502（curl 退出码 0）"
else
  fail "解析不出来的域名上游期望 502 且 curl 成功，实际 curl 退出码 $CURL_RC、HTTP $DEAD_CODE"
fi

# ★ ★ ★ 而且**进程没有 panic**。这一条是本批的头条判据：
#   改之前那里是每请求一次 `called Result::unwrap() on an Err value`。
if grep -qiE "panicked at|Result::unwrap" "$WORK/proxy.log"; then
  fail "日志里出现了 panic —— 请求路径上还在做 DNS"
  grep -iE "panicked at|Result::unwrap" "$WORK/proxy.log" | head -3 >&2
else
  ok "★ 请求路径上没有任何 panic（改之前这里是每请求一次）"
fi

# ★ 装载时那条 error 必须说出来「哪个上游解析不出来」——
#   一个静静跳过的实现会让运维对着 502 查半天。
if grep -q "no-such-host-zzz.invalid:80" "$WORK/proxy.log"; then
  ok "装载日志点名了解析不出来的那个上游"
else
  fail "装载日志没说哪个上游解析不出来"
fi

# ── ★ ★ ★ 后台重解析：域名在进程活着的时候换了 IP（批 10）────────────────────
#
# ★ 三步，两个方向都要走：**跟得上变坏** → **也跟得上变好**。
#   ⚠ 只验前者，一个「解析失败就把上游永久摘掉」的实现照样全绿；
#     只验后者，一个「压根没重解析、一直连着启动那一份」的实现在
#     127.0.0.1 → 127.0.0.1 上也照样全绿。

# ★ 先验一件更基本的事：**那个后台任务真的起来了，而且用的是配置里那个间隔**。
#   ⚠ 少了这条，下面两步一旦超时，分不出「没重解析」还是「间隔取错成 30s」。
if grep -qF "上游 DNS 定期重解析已启动，每 5s 一次" "$WORK/proxy.log"; then
  ok "后台重解析任务起来了，间隔取自配置里的 dns_refresh 5s"
else
  fail "日志里没有后台重解析的启动行（或者间隔不是 5s）"
  grep -i '重解析' "$WORK/proxy.log" >&2 || true
fi

# 轮询直到状态码等于期望值，或超时。回显最后一次拿到的码。
POLL_TIMEOUT=30
poll_status() {
  local url=$1 want=$2 tries=0 got=
  while [ "$tries" -lt "$POLL_TIMEOUT" ]; do
    got=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$url")
    if [ "$got" = "$want" ]; then break; fi
    sleep 1
    tries=$((tries + 1))
  done
  echo "$got"
}

# 1) 启动时它指向真上游 —— 照常转发。
expect_status "GET /rotate/x（会变的域名，起初指向真上游）" 200 "$(probe "$BASE/rotate/x")"

# 2) 把它指到一个没人在听的地址，等后台那一轮把新答案取回来。
#    ⚠ 127.0.0.9 在 Linux 上属于本机的 lo，连它会**立刻**被拒 —— 干净、快、可判。
set_rotate_ip 127.0.0.9 || fail "改不了 /etc/hosts"
GOT=$(poll_status "$BASE/rotate/x" 502)
if [ "$GOT" = "502" ]; then
  ok "★ 后台重解析跟上了「域名换到一个连不上的 IP」→ 干净的 502"
else
  fail "域名换 IP 之后期望 502，$POLL_TIMEOUT 秒内一直是 $GOT —— 后台重解析没跑？"
fi

# ★ 而且要说出来「它变了、变成了什么」。
#   ⚠ 只看状态码分不出「重解析了」与「上游恰好挂了」——
#     日志里那一行是**谁做的决定**的唯一凭据。
if grep -F '的地址变了' "$WORK/proxy.log" | grep -qF '127.0.0.9'; then
  ok "★ 日志记下了新解析出来的地址（谁做的决定看得见）"
else
  fail "日志里没有「地址变了 → 127.0.0.9」那一行 —— 重解析没留痕"
  grep -i '重解析\|地址变了' "$WORK/proxy.log" >&2 || true
fi

# 3) 再指回来 —— **它得自己恢复**。
#    ★ ★ 这一步才是 `dns_refresh` 存在的全部理由：nginx OSS 那个事故就是
#      「域名换了 IP，而进程一直连着启动时解析到的那一份」。
set_rotate_ip 127.0.0.1 || fail "改不了 /etc/hosts"
GOT=$(poll_status "$BASE/rotate/x" 200)
if [ "$GOT" = "200" ]; then
  ok "★ ★ 域名指回来之后它自己恢复了（dns_refresh 的全部理由）"
else
  fail "域名指回真上游之后期望 200，$POLL_TIMEOUT 秒内一直是 $GOT"
fi

# ★ ★ 失败日志**不许每轮刷一行**。
#   `no-such-host-zzz.invalid` 从头到尾都解析不出来，而上面这几步跑了十几秒
#   ＝ 后台任务转了好几轮。一个「每轮都 warn」的实现在这里会留下好几行。
#   ⚠ 判据用 `-le 1` 不用 `-eq 1`：本节耗时短时可能一轮都还没转到，
#     那时 0 行是对的；而**刷屏的实现给不出 ≤1**。
SPAM=$(grep -cF '开始解析不出来' "$WORK/proxy.log" || true)
if [ "$SPAM" -le 1 ]; then
  ok "★ 一个永久坏掉的上游只报一次，不是每轮刷一行（实际 $SPAM 行）"
else
  fail "后台重解析把同一个失败刷了 $SPAM 行 —— 日志会被淹掉"
fi


# ── ★ ★ ★ 主动健康检查（批 11）──────────────────────────────────────────────
echo "=== [2c/4] 主动健康检查（health_uri）==="

# ★ 先钉住「后台任务真的起来了」。少了它，下面每一条超时都分不出
#   「判定没生效」还是「任务压根没起」。
if grep -qF "主动健康检查已启动" "$WORK/proxy.log"; then
  ok "主动健康检查任务起来了"
else
  fail "日志里没有主动健康检查的启动行"
  grep -i '健康' "$WORK/proxy.log" >&2 || true
fi

# 连打 n 次，回显命中 want 的次数。
burst_status() {
  local url=$1 want=$2 n=$3 hit=0 i=0 got=
  while [ "$i" -lt "$n" ]; do
    got=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$url")
    if [ "$got" = "$want" ]; then hit=$((hit + 1)); fi
    i=$((i + 1))
  done
  echo "$hit"
}

# 轮询直到「连打 n 次全中」，或超时。回显最后一轮的命中数。
poll_burst() {
  local url=$1 want=$2 n=$3 tries=0 hit=0
  while [ "$tries" -lt "$POLL_TIMEOUT" ]; do
    hit=$(burst_status "$url" "$want" "$n")
    if [ "$hit" = "$n" ]; then break; fi
    sleep 1
    tries=$((tries + 1))
  done
  echo "$hit"
}

# ── ★ ★ ★ 本节的锚：那台「病号」的服务能力**完全正常** ─────────────────────
#
# ⚠ 少了这一条，下面那个 502 说明不了任何事情 —— 它可能只是「连不上」。
#   有了它，502 唯一可能的来源就是**探测路径的判定**。
expect_status "直连病号上游（业务路径）" 200 "$(probe "http://$HOST:$SICK_PORT/whatever")"
expect_body "直连病号上游（业务路径）" "sick but serving path=/whatever"
expect_status "直连病号上游的探测路径" 500 "$(probe "http://$HOST:$SICK_PORT/health")"

# 1) 探测路径回 500 ⇒ 它被摘掉，那条路由只剩 502。
HIT=$(poll_burst "$BASE/sick/x" 502 5)
if [ "$HIT" = "5" ]; then
  ok "★ ★ 探测路径回 500 的上游被摘掉了（而它的业务路径明明是 200）"
else
  fail "病号上游期望被摘掉（连 5 次 502），$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/5"
fi

# 2) ★ ★ ★ 同一个进程、同一条 health_uri，只把 `health_status` 写成 5xx
#    ⇒ 判定**相反**。这一条证明的是「状态码模式真的被读了」，
#    而不是「凡是 500 就摘」这种写死的实现。
HIT=$(poll_burst "$BASE/sickok/x" 200 5)
if [ "$HIT" = "5" ]; then
  ok "★ ★ ★ 同一个上游、health_status 写 5xx ⇒ 判它健康（状态码模式真的被读了）"
else
  fail "health_status 5xx 那条期望一直 200，$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/5"
fi

# 3) 日志要说清楚**谁**被摘了、**为什么**。
#    ⚠ 只看状态码的话，运维面对 502 分不出「上游挂了」「被健康检查摘了」
#      「DNS 没解析出来」——这三种的处置完全不同。
if grep -F '被健康检查摘掉' "$WORK/proxy.log" | grep -qF "$SICK_PORT"; then
  ok "日志点名了被摘掉的那个上游与原因"
else
  fail "日志里没有「被健康检查摘掉 … $SICK_PORT」那一行"
  grep -i '健康检查' "$WORK/proxy.log" | head -5 >&2 || true
fi

# 4) 两个上游、其中一个**根本没人在听** ⇒ 探测把它摘掉之后，轮询必须一直落在活的那个上。
#    ⚠ 没有健康检查时这里是「每两个请求坏一个」——而那正是 round_robin 的固有行为，
#      不是缺陷；本批要的就是让它不再发生。
HIT=$(poll_burst "$BASE/hc/x" 200 10)
if [ "$HIT" = "10" ]; then
  ok "★ 挂掉的那个上游被摘掉之后，连打 10 次全是 200"
else
  fail "期望连打 10 次全 200，$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/10"
fi

# 5) ★ ★ 恢复那一半：把迟到的上游起起来，它必须**自己回到轮询里**。
#    ⚠ 只验「摘得掉」的话，一个单向的实现照样全绿 —— 而那意味着
#      上游修好之后**永远回不来**，运维只能重启枢衡。
echo "  · 现在把迟到的那个上游起起来"
start late "$WORK/late.Fulcrumfile"
wait_port "$LATE_PORT" || fail "迟到的上游起不来"

seen_late() {
  local tries=0 i=0
  local body=''
  while [ "$tries" -lt "$POLL_TIMEOUT" ]; do
    i=0
    while [ "$i" -lt 8 ]; do
      body=$(run_curl -s --max-time 5 "$BASE/hc/x")
      case "$body" in
        *"late path="*)
          echo yes
          return
          ;;
      esac
      i=$((i + 1))
    done
    sleep 1
    tries=$((tries + 1))
  done
  echo no
}
if [ "$(seen_late)" = "yes" ]; then
  ok "★ ★ 上游修好之后它自己回到了轮询里（恢复方向也通）"
else
  fail "迟到的上游起来了 $POLL_TIMEOUT 秒，仍然没有一个请求落到它身上"
fi
# ★ 而且恢复也要留痕。
if grep -qF '健康检查恢复了' "$WORK/proxy.log"; then
  ok "日志记下了恢复"
else
  fail "日志里没有「健康检查恢复了」"
fi

# 6) ★ 没配 `health_uri` 的路由**完全不受影响**（`/api/` 上有 health_uri，
#    但 `/rw/` 那条没有）—— 一个「对所有上游都探一遍」的实现会去打
#    一个用户从没说过的路径，而那在很多后端上是 404 ⇒ 全部被判死。
expect_status "没配 health_uri 的路由照常转发" 200 "$(probe "$BASE/rw/anything")"

# ── [3/4] 管理面：全量原子 load（G8）与访问控制（G14）──────────────────────
#
# ⚠ ⚠ **本节必须排在上面所有数据面断言之后**：它会真的把配置换掉。
echo "=== [3/4] 管理面（Unix socket，G14）==="

# ★ ★ 权限就是这个管理面的**全部**访问控制（G14：交给文件系统 ACL）。
#   一个 0666 的 socket 等于「同机任意进程可改配置」—— 正是 G14 要堵的那个短处。
if [ -S "$ADMIN_SOCK" ]; then
  ok "管理 socket 建起来了：$ADMIN_SOCK"
  SOCK_MODE=$(stat -c '%a' "$ADMIN_SOCK")
  if [ "$SOCK_MODE" = "600" ]; then
    ok "socket 权限 600（G14：这是管理面的全部访问控制）"
  else
    fail "socket 权限是 $SOCK_MODE，期望 600 —— 同机任意进程都能改配置了"
  fi
else
  fail "管理 socket 没建起来：$ADMIN_SOCK"
fi

# ★ 反向那一半：**没配 `admin` 的那个实例不该有任何 socket**。
#   ⚠ 少了这一条，一个「不管配没配都开一个」的实现照样全绿 ——
#   而 G14 的第一句话是「管理面默认不出机器」。
if [ -e "$WORK/upstream.sock" ] && [ -S "$WORK/upstream.sock" ]; then
  fail "上游实例没配 admin，却出现了一个 socket"
else
  ok "没配 admin 的实例没有建任何 socket（G14：默认不出机器）"
fi

admin_post() {
  # $1 = 路径，$2 = body。回 "状态码<TAB>正文"
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X POST --data-binary "$2" \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# 不认识的路径 → 404。⚠ 一个「什么都收下」的管理面会让打错的命令看起来成功了。
CODE=$(admin_post /nope '{}')
expect_status "管理面：不认识的路径" 404 "$CODE"

# 坏 JSON → 400，**且旧配置一个字节都没动**。
CODE=$(admin_post "/load?overrides=clear" '{ not json')
expect_status "管理面：坏载荷" 400 "$CODE"
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$PROXY_PORT/")
expect_status "坏载荷之后旧配置还在服务" 200 "$CODE"

# ── ★ ★ ★ 增量通道：POST /runtime（M2 批 N 任务 4，裁决 R10；修复轮 1，评审 I1）──
#
# ⚠ ⚠ ⚠ 少了这一段，`/runtime` 这条路由**在生产上可以整个不存在**而没有任何
#   门会红：全部 21 条 admin.rs 判据都是直接调 `self.runtime(&body)`，一条都没有
#   经过 `process_new_http` 里的路由分发（那个 `match (method, path)`）。
#   把路由表里 `("POST","/runtime") => self.runtime(&body)` 那一行删掉，
#   21 条判据照样全绿，而这里回 404——`admin.rs` 自己的注释写着「一个判据漏掉
#   一个入口，等于在那个入口上没有判据」。
#   ★ ★ 判据挂在**数据面**上，不是挂在「`/runtime` 回了 200」上：
#   `disable` 之后要走真流量确认那台上游**真的不再收流量**，`enable` 之后
#   要走真流量确认它**真的收回来了**。
echo "  · 增量通道：POST /runtime（disable 一个上游 ⇒ 数据面真的不再收流量）"

# 基线：/api 现在应该是健康的（它指着 UP_PORT，没配 health_uri，没被摘过）。
expect_status "/runtime 基线：/api 现在是健康的" 200 "$(probe "$BASE/api/x")"

# ⚠ `site` 取的是这个站点块在 DSL 里的**原文**（`SiteRt::name`），不是拼出来的——
#   本场景的主站点块头就是裸端口 `:PROXY_PORT`（没有 host、没有 scheme），
#   替换之后就是 `:$PROXY_PORT`。上游地址是 IP 字面量，不需要归一化，
#   与 `reverse_proxy` 那一行逐字相同。
RUNTIME_SITE=":$PROXY_PORT"
RUNTIME_UP="127.0.0.1:$UP_PORT"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "POST /runtime disable" 200 "$CODE"

# ★ ★ ★ 真正的判据：数据面上它必须真的不再收流量（不是「回了 200」）。
#   `/rw`、`/cached` 与 `/api` 共享同一个覆盖格子（三条都没写 `id`，键相同）——
#   这里只验 `/api`，上面那条 200 断言已经确认过它原本是健康的。
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/api/x")
expect_status "★ ★ disable 之后 /api 在数据面上真的不再收流量" 502 "$CODE"

# ★ 反向那一半：`enable` 把它撤回来，数据面也要真的恢复——只验「摘得掉」
#   而不验「回得来」，一个单向的实现会让运维在真正需要撤销的时候束手无策。
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"enable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "POST /runtime enable" 200 "$CODE"
expect_status "★ enable 之后 /api 在数据面上恢复了" 200 "$(probe "$BASE/api/x")"
expect_body "enable 之后 /api" "up path=/api/x xup=1"

# ★ ★ 端口集变了 → 409，**且旧配置还在服务**。
#   这一条是「原子」的判据：一个「先换后校验」的实现在「换得动」那条上表现完全相同。
printf '%s\n' ":$((PROXY_PORT + 40)) {" "    respond 200 \"moved-port\"" "}" \
  > "$WORK/otherport.Fulcrumfile"
"$BIN" compile "$WORK/otherport.Fulcrumfile" > "$WORK/otherport.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile 生成不出结构化配置" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/otherport.json")")
expect_status "管理面：端口集变了" 409 "$CODE"
if grep -q 'systemctl reload' "$WORK/admin.out"; then
  ok "409 的正文说清了该怎么办（走 systemctl reload）"
else
  fail "409 没说该怎么办：$(cat "$WORK/admin.out")"
fi
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$PROXY_PORT/")
expect_status "被拒之后旧配置还在服务（原子）" 200 "$CODE"

# ★ ★ ★ 真的换一份：**判据挂在数据面上，不是挂在「返回了 200」上**。
#   ⚠ 只断言 200 的话，一个「收下但什么都不做」的实现照样全绿。
printf '%s\n' "{" "    admin unix/$ADMIN_SOCK" "}" "" ":$PROXY_PORT {" \
  "    respond 200 \"after-load\"" "}" "" "http://only.example:$NAMED_PORT {" \
  "    respond 200 named-only" "}" "" "secure.example:$TLS_PORT {" \
  "    tls $WORK/tls.crt $WORK/tls.key" "    respond 200 secure-ok" "}" \
  > "$WORK/next.Fulcrumfile"
"$BIN" compile "$WORK/next.Fulcrumfile" > "$WORK/next.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile 生成不出新配置" >&2
  cat "$WORK/next.Fulcrumfile" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/next.json")")
expect_status "管理面：全量 load" 200 "$CODE"
BODY=$(run_curl -s --max-time 5 "http://$HOST:$PROXY_PORT/")
if [ "$BODY" = "after-load" ]; then
  ok "★ 换配置**真的落到数据面上了**（响应体 = after-load）"
else
  fail "load 返回 200，但数据面还是老的：拿到「$BODY」"
fi
# ★ 同一次 load 里没动的那个站点必须**照旧能用** —— 换的是整份，不是只换命中的那个。
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
  --resolve "only.example:$NAMED_PORT:$HOST" "http://only.example:$NAMED_PORT/")
expect_status "同一份里没改动的站点照旧" 200 "$CODE"

# ── [3.5/4] `overrides` 必填 + R11 覆盖计数（M2 批 N 任务 5，G120 / R9 / R11）──
echo "=== [3.5/4] POST /load?overrides= 与每次响应的覆盖计数 ==="

# 上一节末尾把 :$PROXY_PORT 换成了裸 respond（after-load）——这里重新换出一份
# 带 reverse_proxy 的，好让 overrides 的 keep/clear 有个真上游可以 disable。
# ⚠ 监听端口集必须与当前完全相同（:$PROXY_PORT 裸 HTTP、only.example:$NAMED_PORT、
#   secure.example:$TLS_PORT 走 tls），否则会撞 409，而那不是本节要测的东西。
#   ★ ★ ★ 修复轮 1，评审 F1：`:$PROXY_PORT` 下面挂**两个**互不相同的覆盖键
#   （没写 `id` 的兜底 handle + 写了 `id up2` 的 `/second/*`，两者共享同一台
#   真机 `$UP_PORT`，只是 `id` 不同）——R11 那一节要摆「两项生效中、其中一项
#   悬空」（N≠M），否则 N 与 M 永远是同一个数，`{n}`/`{m}` 写反也测不出来。
cat > "$WORK/ov.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    handle /second/* {
        reverse_proxy 127.0.0.1:$UP_PORT {
            id up2
        }
    }
    handle {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/ov.Fulcrumfile" > "$WORK/ov.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile ov.Fulcrumfile 失败" >&2
  exit 1
}
# 同样的骨架，但兜底 handle 换成裸 respond——没写 id 的那把键没了上游，拿来验
# 悬空（判据 5）；`/second/*`（id up2）原样留着，R11 那一节要靠它撑住「另一项
# 仍然生效中、但不悬空」。
cat > "$WORK/ov-dangling.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    handle /second/* {
        reverse_proxy 127.0.0.1:$UP_PORT {
            id up2
        }
    }
    handle {
        respond 200 "ov-dangling"
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/ov-dangling.Fulcrumfile" > "$WORK/ov-dangling.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile ov-dangling.Fulcrumfile 失败" >&2
  exit 1
}

# 重新建立带上游的那份基线——`overrides` 必填，即使这次只是「重建夹具」也不例外。
# ⚠ `RUNTIME_SITE` / `RUNTIME_UP` 是上面 [3/4] 那一节留下的（`:$PROXY_PORT` /
#   `127.0.0.1:$UP_PORT`），这份 ov.Fulcrumfile 用的是同一个站点、同一个上游。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "重建 overrides 夹具（首次也要带 overrides）" 200 "$CODE"
expect_status "重建之后 / 走真上游" 200 "$(probe "$BASE/")"

# ── 判据 1：缺 overrides ⇒ 400，且旧配置一个字节没动 ─────────────────────
#   ⚠ ⚠ 判据写法纪律：body 是完全合法、会真的生效的 ov.json——唯一的毛病
#   是没带 `?overrides=`，否则 400 可能来自别的原因，判据就 confound 了。
CODE=$(admin_post /load "$(cat "$WORK/ov.json")")
expect_status "缺 overrides ⇒ 400" 400 "$CODE"
if grep -q 'overrides' "$WORK/admin.out"; then
  ok "400 的正文点了 overrides 的名"
else
  fail "400 没说清是 overrides 缺了：$(cat "$WORK/admin.out")"
fi
expect_status "缺 overrides 被拒之后旧配置还在服务" 200 "$(probe "$BASE/")"

# ── 判据 2：overrides 写了别的值 ⇒ 400，同上；夹具「除了这一处，其余完全合法」──
CODE=$(admin_post "/load?overrides=bogus" "$(cat "$WORK/ov.json")")
expect_status "overrides 写了不认识的值 ⇒ 400" 400 "$CODE"
expect_status "写错值被拒之后旧配置还在服务" 200 "$(probe "$BASE/")"

# ── 修复轮 1，评审 F3（提级 Minor）：overrides= 重复且冲突 ⇒ 400，不是静默取
#   第一个 ────────────────────────────────────────────────────────────────
#   G120 的立身之本是「两种现实互相冲突 ⇒ 不猜」，同一个请求里写了两个 overrides=
#   正是这个形状的教科书实例。⚠ ⚠ 判据写法纪律：body 依旧是完全合法的 ov.json，
#   唯一的毛病是查询串里 `overrides` 出现了两次。
CODE=$(admin_post "/load?overrides=keep&overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "overrides 重复且冲突 ⇒ 400" 400 "$CODE"
if grep -q '重复' "$WORK/admin.out"; then
  ok "400 的正文说清是「重复」而不是「值不认识」"
else
  fail "400 没说清是「重复」：$(cat "$WORK/admin.out")"
fi
expect_status "重复被拒之后旧配置还在服务" 200 "$(probe "$BASE/")"

# ── 判据 3：overrides=keep ⇒ 覆盖还在，且仍作用在数据面上 ───────────────────
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "先摘掉这个上游（走 /runtime）" 200 "$CODE"
expect_status "摘掉之后数据面确实不通了" 502 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"

CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov.json")")
expect_status "overrides=keep 的 load" 200 "$CODE"
expect_status "★ ★ keep 之后覆盖还在数据面上生效（继续 502）" 502 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"

# ── 判据 5：悬空——keep 之后键落不到任何上游的覆盖仍在、标了 dangling、回话点名 ──
#   ⚠ 判据写法纪律：断言的地址（$RUNTIME_UP）是这份夹具自己的真实上游地址，
#   不会出现在任何错误模板的固定字面量里。
CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov-dangling.json")")
expect_status "keep：换成不带这个上游的配置" 200 "$CODE"
if grep -qF "$RUNTIME_UP" "$WORK/admin.out"; then
  ok "★ 悬空覆盖在回话里被逐条点名（地址 $RUNTIME_UP 出现在正文里）"
else
  fail "回话没点名悬空覆盖：$(cat "$WORK/admin.out")"
fi
if grep -q '悬空' "$WORK/admin.out"; then
  ok "回话说清了这是「悬空」"
else
  fail "回话没说这是悬空：$(cat "$WORK/admin.out")"
fi

# ── 判据 4：overrides=clear ⇒ 覆盖没了，且回话里逐项列出被清掉的 ──────────────
#   现在登记处里那一项是悬空的（上面 keep 换配置之后）。带回真有上游的那份，
#   走 clear——clear 不分悬空不悬空，全清。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "overrides=clear 的 load" 200 "$CODE"
if grep -qF "$RUNTIME_UP" "$WORK/admin.out"; then
  ok "★ clear 的回话逐项列出了被清掉的那一项（地址 $RUNTIME_UP 出现在正文里）"
else
  fail "clear 的回话没有逐项列出被清掉的：$(cat "$WORK/admin.out")"
fi
expect_status "★ clear 之后数据面恢复（不再是 502）" 200 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"

# ── 判据 10：clear 之后再设一次覆盖，数据面当场生效（身份没断的行为侧对照）──
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "clear 之后再 disable 一次" 200 "$CODE"
expect_status "★ 身份没断：再 disable 立刻又生效" 502 "$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$BASE/")"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"enable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "收尾：enable 撤回来" 200 "$CODE"
expect_status "撤回之后数据面恢复" 200 "$(probe "$BASE/")"

# ── 判据 6 / 7：R11 —— 每一次响应都带「当前有 N 项临时覆盖生效中（其中 M 项悬空）」──
#   ⚠ ⚠ 「每一次」就是每一次：200/400/404/413 都要带；413 那条在 process_new_http
#   的另一个 reply() 调用点上（读 body 超上界），主路径的判据测不到它。
#   ★ ★ ★ 修复轮 1，评审 F1（Critical）：夹具必须是**两项覆盖、其中一项悬空**
#   （N≠M）。原先只摆一项、且那一项就是要悬空的那个 ⇒ N 与 M 永远是同一个数
#   （都是 1），`reply()` 里把 `{n}`/`{m}` 写反、或两格印同一个值，都测不出来——
#   这正是判据写法纪律第 2 条点名的「好坏两种情况下读数相同的尺」。
#   ⇒ 这里摆两把不同的键：没写 id 的兜底（$RUNTIME_UP）与写了 id 的 up2——
#   下面这次 load 用 ov-dangling.json（只留 up2 那条路），没写 id 的那把因此
#   悬空，up2 那把仍然活着。
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "为 R11 摆第一项生效中的覆盖（没写 id 的那把，之后会悬空）" 200 "$CODE"
CODE=$(admin_post /runtime "{\"actions\":[{\"verb\":\"disable\",\"site\":\"$RUNTIME_SITE\",\"id\":\"up2\",\"upstream\":\"$RUNTIME_UP\"}]}")
expect_status "为 R11 摆第二项生效中的覆盖（id=up2，之后仍然活着）" 200 "$CODE"
CODE=$(admin_post "/load?overrides=keep" "$(cat "$WORK/ov-dangling.json")")
expect_status "为 R11 换成只留 up2 那条路的配置（没写 id 的那把悬空）" 200 "$CODE"
# 现在：登记处应该有 2 项生效中的覆盖，其中 1 项（没写 id 的那把）悬空 ⇒ N=2、M=1，
# 两个数不同——一把真正能分得清「读对了」与「读串了」的尺。
COUNT_LINE="当前有 2 项临时覆盖生效中（其中 1 项悬空）"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ 200 响应带着正确的计数行：$COUNT_LINE"
else
  fail "200 响应没带正确的计数行，期望包含「$COUNT_LINE」：$(cat "$WORK/admin.out")"
fi

CODE=$(admin_post /nope '{}')
expect_status "R11／404：不认识的路径" 404 "$CODE"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ 404 响应也带着计数行"
else
  fail "404 响应没带计数行：$(cat "$WORK/admin.out")"
fi

CODE=$(admin_post "/load?overrides=clear" '{ not json')
expect_status "R11／400：坏 JSON" 400 "$CODE"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ 400 响应也带着计数行"
else
  fail "400 响应没带计数行：$(cat "$WORK/admin.out")"
fi

# ★ ★ ★ 413——在 process_new_http 的另一个 reply() 调用点上，主路径测不到它。
#   真发一个超过 4 MiB 上界的载荷；写文件再用 @file，避免把 ~5MB 塞进单个
#   shell 参数（ARG_MAX 会炸）。
head -c 5000000 /dev/zero | tr '\0' 'a' > "$WORK/big.bin"
CODE=$(curl -s -o "$WORK/admin.out" -w '%{http_code}' \
  --unix-socket "$ADMIN_SOCK" -X POST --data-binary "@$WORK/big.bin" \
  "http://localhost/load" 2>/dev/null || echo "000")
expect_status "R11／413：载荷太大" 413 "$CODE"
if grep -qF "$COUNT_LINE" "$WORK/admin.out"; then
  ok "★ ★ ★ 413 响应也带着计数行——它在主路径之外，最容易被漏"
else
  fail "★ ★ ★ 413 响应没带计数行（brief 点名最容易漏的那条）：$(cat "$WORK/admin.out")"
fi

# 收尾：clear 掉，恢复数据面，别影响后面 [4/4] 的判据。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "overrides 小节收尾：clear 恢复" 200 "$CODE"
expect_status "收尾之后数据面恢复" 200 "$(probe "$BASE/")"

# ── [3.6/4] GET /stats（M2 批 N 任务 6，R12「与 /metrics 同源」）─────────────
#
# ⚠ ⚠ ⚠ 这一节是 brief §7 点名「R12 的同源判据本来就必须走真路径」的落地：
#   下面每一条判据都从**真 socket**上打——`/stats` 走 admin unix socket，
#   `/metrics` 走数据面 HTTP 端口，两者在**同一时刻**各抓一次。admin.rs 里
#   逐字段的深度判据已经在 Rust 单测里做过（同源、fanout、overrides、
#   证书/缓存缺省、装载时间），这里**不重复**那些细节，只证明「路由真的接
#   在生产会走的那条线上」——任务 4 那次「16 条判据全部直接调 handler，
#   把 `("GET","/stats") => …` 那一行删掉全部照样绿」的教训点名过这件事。
echo "=== [3.6/4] GET /stats 与 GET /metrics 同源 ==="

admin_get() {
  # $1 = 路径。回状态码，正文写进 $WORK/admin.out（与 admin_post 同一个约定）。
  curl -s -o "$WORK/admin.out" -w '%{http_code}' \
    --unix-socket "$ADMIN_SOCK" -X GET \
    "http://localhost$1" 2>/dev/null || echo "000"
}

# ★ ★ 一把共享键（两条 reverse_proxy 都没写 id，指同一台 $UP_PORT）+ 一把独立
#   键（id=solo，同样指 $UP_PORT——与共享键**同地址不同 id**，R12 的地址级
#   归并因此要把三条 reverse_proxy 的 upstream 行并成 /metrics 上的一行）+
#   一把指不存在地址的独立键（id=third，纯 IP 字面量，不需要能连上——只用来
#   把 fanout 总键数拉到 3，与共享键的 proxies=2 错开，避免「总数与 proxies
#   恰好相等」的判据写法陷阱）。`/__metrics` 挂在数据面，与 admin socket 上的
#   `/stats` 是两个不同的出口。
#
# ★ ★ ★ 修复轮 1，评审 I2：`solo` 那条 `reverse_proxy` 挂了 `health_uri`，
#   而 `health_status` 故意写成 `500`——$UP_PORT 那台上游对**任何**路径都回
#   200（它的配置就是裸 `respond 200 "up path=… xup=…"`，没有一条路径会回
#   500），于是这条探测**恒被判失败**，不需要真的去改 $UP_PORT 自己的配置
#   （那个配置被本文件几十处别的判据共用，不该为了这一节去动它）。
#   ⇒ `solo` 那一行的 `healthy` 会稳定落在 `false`，`shared-a`/`shared-b`
#   没配 `health_uri`、恒 `true`——三行合取 = `false`，R12 的 healthy 归并
#   因此走的是**非退化值**，不再是「一刻都没配 health_uri、恒 true」那种
#   两边都读到同一个平凡值的退化场景。
cat > "$WORK/stats.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    handle /__metrics {
        metrics
    }
    handle /shared-a/* {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /shared-b/* {
        reverse_proxy 127.0.0.1:$UP_PORT
    }
    handle /solo/* {
        reverse_proxy 127.0.0.1:$UP_PORT {
            id solo
            health_uri /solo-health-probe
            health_status 500
            health_interval 1s
        }
    }
    handle /third/* {
        reverse_proxy 127.0.0.1:19999 {
            id third
        }
    }
    handle {
        respond 200 "stats-default"
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/stats.Fulcrumfile" > "$WORK/stats.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile stats.Fulcrumfile 失败" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/stats.json")")
expect_status "装载 /stats 夹具（含撞键 + /__metrics + 一条恒不健康的探测）" 200 "$CODE"
expect_status "夹具装好之后 /shared-a 走真上游" 200 "$(probe "$BASE/shared-a/x")"

# ★ ★ ★ 等主动健康检查真的把 solo 判成不健康——`poll_burst`/`POLL_TIMEOUT`
#   是 [2c/4] 那一节留下的现成 helper（同一份脚本，函数在前面定义过就能在
#   这里用）。判据挂在**数据面行为**上（502），不是「配了 health_uri 就当它
#   已经生效」——刚装载完的那一小段窗口里 healthy 恒为初值 true（同 [2c/4]
#   的教训）。
HIT=$(poll_burst "$BASE/solo/x" 502 5)
if [ "$HIT" = "5" ]; then
  ok "★ solo 探测路径恒 500 ⇒ 它被判不健康（数据面上 /solo/* 变成 502）"
else
  fail "solo 期望被判不健康（连 5 次 502），$POLL_TIMEOUT 秒内最好的一轮只中了 $HIT/5"
fi

# 只做一件事的小工具：解析 /stats 的 JSON 与 /metrics 的 exposition 文本，
# 按地址做归并再互相比对。★ 判据本身（同源相等）落在 bash 这一层的
# `expect_status`/`ok`/`fail` 上，python3 只负责把两份格式都不同的正文
# 读成同一种形状——这与 tests/metrics/run.sh 的 EXPO 是同一个分工。
cat > "$WORK/stats_check.py" <<'PY'
#!/usr/bin/env python3
"""只服务于 tests/serve/run.sh 的 [3.6/4] 一节：读 /stats 的 JSON 与
/metrics 的 Prometheus exposition，按子命令回答一个具体问题。
⚠ 读不懂就当场非零退出——不猜、不吞。"""
import json
import re
import sys


def load_stats(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def metrics_gauge(path, family, addr):
    """/metrics 那一侧：Prometheus text exposition 里某个 gauge 的读数（float）。
    找不到就抛——调用方不该拿到一个假装存在的默认值。"""
    pat = re.compile(
        r"^" + re.escape(family) + r"\{upstream=\"" + re.escape(addr) + r"\"\}\s+(\S+)$"
    )
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = pat.match(line.rstrip("\n"))
            if m:
                return float(m.group(1))
    raise SystemExit(f"没找到 {family}{{upstream=\"{addr}\"}} 这一行（{path}）")


def merge_stats_upstreams(v, addr):
    """/stats 那一侧按地址现算：inflight 求和、healthy 取合取。
    返回 (行数, inflight 和, healthy 是否全真)。"""
    rows = [r for r in v["upstreams"] if r["addr"] == addr]
    if not rows:
        raise SystemExit(f"upstreams 里没有 addr={addr}")
    return len(rows), sum(r["inflight"] for r in rows), all(r["healthy"] for r in rows)


cmd = sys.argv[1]

if cmd == "shape":
    (stats_path,) = sys.argv[2:]
    v = load_stats(stats_path)
    need = [
        "pid",
        "config_loaded_at_unix",
        "upstreams",
        "overrides",
        "fanout",
        "fanout_shared",
        "cache",
        "certs",
    ]
    missing = [k for k in need if k not in v]
    if missing:
        print(f"缺顶层字段：{missing}", file=sys.stderr)
        sys.exit(1)
    for k in ("upstreams", "overrides", "fanout", "fanout_shared"):
        if not isinstance(v[k], list):
            print(f"`{k}` 应该是数组，实际是 {type(v[k])}", file=sys.stderr)
            sys.exit(1)
    print("OK")

elif cmd == "fanout_proxies":
    # 打印 fanout_shared 里某个 addr 的 proxies；找不到、或混进了
    # proxies<=1 的项，就非零退出。
    stats_path, addr = sys.argv[2:]
    v = load_stats(stats_path)
    rows = [r for r in v["fanout_shared"] if r["addr"] == addr]
    if not rows:
        print(f"fanout_shared 里没有 addr={addr}", file=sys.stderr)
        sys.exit(1)
    if any(r["proxies"] <= 1 for r in rows):
        print(f"fanout_shared 混进了 proxies<=1 的项：{rows}", file=sys.stderr)
        sys.exit(1)
    print(rows[0]["proxies"])

elif cmd == "fanout_total":
    (stats_path,) = sys.argv[2:]
    print(len(load_stats(stats_path)["fanout"]))

elif cmd == "rows_for_addr":
    stats_path, addr = sys.argv[2:]
    n, _, _ = merge_stats_upstreams(load_stats(stats_path), addr)
    print(n)

elif cmd == "field":
    # 打印一个顶层标量字段（比如 pid）——只给不需要专门子命令的简单读取用。
    stats_path, key = sys.argv[2:]
    v = load_stats(stats_path)
    if key not in v:
        print(f"顶层没有字段 `{key}`", file=sys.stderr)
        sys.exit(1)
    print(v[key])

elif cmd == "addrs":
    (stats_path,) = sys.argv[2:]
    for r in load_stats(stats_path)["upstreams"]:
        print(r["addr"])

elif cmd == "same_source":
    # ★ ★ ★ R12 的核心判据：把 /stats 那一份按地址求和/取合取之后，与
    # /metrics 那一份逐项相等。⛔ ⛔ 不是「两边都不是 0」——这里对**每一个
    # 家族**都用真的数值相等比较，哪怕两边这一刻都读到 0 / true，比较本身
    # 仍然是在问「这两个数是不是同一个源」，不是在问「这两个数是不是非零」。
    stats_path, metrics_path, addr = sys.argv[2:]
    v = load_stats(stats_path)
    _, stats_inflight, stats_healthy = merge_stats_upstreams(v, addr)
    metrics_inflight = metrics_gauge(metrics_path, "fulcrum_upstream_inflight", addr)
    metrics_healthy = metrics_gauge(metrics_path, "fulcrum_upstream_healthy", addr)
    bad = []
    if float(stats_inflight) != metrics_inflight:
        bad.append(f"inflight：/stats 归并={stats_inflight}，/metrics={metrics_inflight}")
    want_healthy = 1.0 if stats_healthy else 0.0
    if want_healthy != metrics_healthy:
        bad.append(f"healthy：/stats 归并={stats_healthy}，/metrics={metrics_healthy}")
    if bad:
        print("；".join(bad), file=sys.stderr)
        sys.exit(1)
    print(f"OK inflight={stats_inflight} healthy={stats_healthy}")

else:
    print(f"不认识的子命令：{cmd}", file=sys.stderr)
    sys.exit(1)
PY

# ── 判据 1：GET /stats 从真 socket 上打，回 200 + 合法 JSON ──────────────────
CODE=$(admin_get /stats)
expect_status "GET /stats（真 socket）" 200 "$CODE"
cp "$WORK/admin.out" "$WORK/stats1.json"
if python3 "$WORK/stats_check.py" shape "$WORK/stats1.json"; then
  ok "/stats 的正文是合法 JSON，且带齐 8 个顶层字段"
else
  fail "/stats 的正文形状不对（见上面 python3 的报错）"
fi

# ★ ★ 修复轮 1，评审 M1：单测里 handler 与断言同进程，`pid` 那条判据近乎
#   恒真（好坏两种情况读数相同）。这里换一个**独立**的对照物——
#   `--pid-file "$WORK/proxy.pid"` 是 `proxy` 那个 `fulcrum serve` 进程
#   自己落盘的、装的正是它自己的 OS pid（见 `process.rs::write_pid_file`），
#   与 JSON 里的 `pid` 字段是两条完全不同的路径各自量出来的同一件事。
#   ★ 分辨力由这个构造保证（不同源码位置 + 跨真实进程边界），可注入的
#   错法是存在的——比如把 `stats()` 的 `pid` 接成 `std::process::id() + 1`
#   或任何写死值，这条判据会抓住。
#
# ★ ★ ★ 修复轮 2，评审 N1：这两行原来是裸的 `VAR=$(...)`——`field` 子命令
#   在字段不存在时 `sys.exit(1)`，`cat` 在文件不存在时也非零退出，两者在
#   set -e 下都会让脚本半路硬中止而不是优雅报红（与判据 3 那条
#   `SAME_SOURCE_OUT` 是同一类坑，这两行当时漏包了）。⇒ 改用上面新写的
#   `capture()`——本文件现在「捕获一个可能失败的命令」只有这一种写法。
capture python3 "$WORK/stats_check.py" field "$WORK/stats1.json" pid
JSON_PID=$CAPTURE_OUT
JSON_PID_RC=$CAPTURE_RC
capture cat "$WORK/proxy.pid"
PROXY_PID=$CAPTURE_OUT
PROXY_PID_RC=$CAPTURE_RC
if [ "$JSON_PID_RC" -eq 0 ] && [ "$PROXY_PID_RC" -eq 0 ] && [ -n "$JSON_PID" ] && [ "$JSON_PID" = "$PROXY_PID" ]; then
  ok "★ ★ /stats 的 pid（$JSON_PID）与 proxy 进程自己的 pid 文件（$PROXY_PID）一致"
else
  fail "pid 判据没通过：JSON_PID=「$JSON_PID」(rc=$JSON_PID_RC)，PROXY_PID=「$PROXY_PID」(rc=$PROXY_PID_RC)"
fi

# ── 判据 2：fanout_shared 里那把撞键的 proxies == 2，走真 HTTP ──────────────
PROXIES=$(python3 "$WORK/stats_check.py" fanout_proxies "$WORK/stats1.json" "127.0.0.1:$UP_PORT" || echo "ERR")
if [ "$PROXIES" = "2" ]; then
  ok "★ ★ fanout_shared 里 127.0.0.1:$UP_PORT 显示 proxies=2"
else
  fail "fanout_shared 的 proxies 应该是 2，实际「$PROXIES」"
fi
# ★ ★ ★ 修复轮 1，评审 I1：这里要的是**等值**断言，不是「不等于 2」——
#   夹具的真实总键数是**已知的字面量 3**（共享地址一把 + solo 一把 + third
#   一把）。写成 `!= 2` 的话，`python3` 硬失败时 `TOTAL="ERR"`，
#   `"ERR" != "2"` 为真 ⇒ 会打出一个**绿的** ✓（`fail` 只累加计数不退出，
#   上游 `shape` 先红也挡不住这一条静默通过）；即便 `python3` 没坏，`0`/`1`/
#   `99` 这类错误值同样会被 `!= 2` 放行——那是一把「不等值」判据，只能挡住
#   `TOTAL` 恰好等于 2 的那一种错法。改成 `= 3` 之后，`ERR` 与任何非 3 的值
#   都会被正确判红。
TOTAL=$(python3 "$WORK/stats_check.py" fanout_total "$WORK/stats1.json" || echo "ERR")
if [ "$TOTAL" = "3" ]; then
  ok "★ fanout 总键数 = 3（共享地址 + solo + third），与 proxies（2）不相等——判据写法纪律"
else
  fail "fanout 总键数应该是 3，实际「$TOTAL」"
fi

# ── 判据 3：R12 同源——GET /metrics 与 GET /stats 同一时刻各抓一次，
#   /stats 按地址归并之后与 /metrics 逐项相等 ────────────────────────────────
CODE=$(probe "$BASE/__metrics")
expect_status "GET /metrics（数据面，真 HTTP）" 200 "$CODE"
cp "$WORK/body" "$WORK/metrics1.txt"

ROWS=$(python3 "$WORK/stats_check.py" rows_for_addr "$WORK/stats1.json" "127.0.0.1:$UP_PORT" || echo "ERR")
if [ "$ROWS" = "3" ]; then
  ok "127.0.0.1:$UP_PORT 在 /stats 里出了 3 行（shared-a/shared-b/solo 各一行，不归并）"
else
  fail "应该有 3 行未归并的 upstream 记录，实际 $ROWS 行"
fi

# ⚠ ⚠ 判据写法纪律：不许「两边都不是 0」——`same_source` 比的是**逐项相等**
#   （数值比较，不是字符串比较）。inflight 这一格这一刻大概率仍然是退化的
#   0（真实并发流量在这段脚本里没有制造出来，与 admin.rs 里 Rust 单测那条
#   分开覆盖）；★ ★ ★ 但 healthy 这一格**不是**——上面刚等到 solo 被判
#   不健康（502 已经证实），三行 healthy 取合取的结果是**非退化的 false**，
#   不再是「没配 health_uri、恒 true」那种两边巧合读到同一个平凡值的场景。
#   把 /stats 那一侧改成「自己再数一遍」而不读同一组原子量，这条判据必须红
#   （反证见任务报告——本轮修复要求做实这一条，不许只在报告里写一句了事）。
# ⚠ ⚠ 用 `capture()`（`same_source` 判定「不同源」时会非零退出，裸的
#   `$(...)` 赋值在 set -e 下会让脚本半路硬中止——那正是修复轮 1 顺手
#   加固过的那类坑，见 §4.5 反证；修复轮 2 又在 M1 那两行新栽了一次，
#   见 §11——现在全文件这一族统一走 `capture()`，不再各自手包一次）。
capture python3 "$WORK/stats_check.py" same_source \
     "$WORK/stats1.json" "$WORK/metrics1.txt" "127.0.0.1:$UP_PORT"
SAME_SOURCE_OUT=$CAPTURE_OUT
SAME_SOURCE_RC=$CAPTURE_RC
if [ "$SAME_SOURCE_RC" -eq 0 ]; then
  ok "★ ★ ★ R12：/stats 按地址归并（求和 inflight / 取合取 healthy）之后与 /metrics 逐项相等（$SAME_SOURCE_OUT）"
else
  fail "★ ★ ★ R12：/stats 与 /metrics 不同源：$SAME_SOURCE_OUT"
fi
# ★ ★ 非退化确认（呼应 admin.rs 那条 Rust 判据收尾的「顺带确认」）：光
#   「逐项相等」挡不住两边**都**读到同一个平凡值——这里再核一次 healthy
#   落的确实是 False，不是巧合的 True。
case "$SAME_SOURCE_OUT" in
  *"healthy=False"*) ok "★ 确认 healthy 归并结果是非退化的 False（不是两边巧合都读到 True）" ;;
  *) fail "healthy 应该归并成 False（solo 那一行不健康），实际：$SAME_SOURCE_OUT" ;;
esac

# ── 判据 4：不缓存 Runtime 快照——POST /load 换掉上游之后，/stats 立刻反映
#   新的那份（不是继续举着换配置之前那份） ───────────────────────────────────
cat > "$WORK/stats-swapped.Fulcrumfile" <<CONF
{
    admin unix/$ADMIN_SOCK
}
:$PROXY_PORT {
    reverse_proxy 127.0.0.1:$UP_PORT {
        id swapped
    }
}
http://only.example:$NAMED_PORT {
    respond 200 named-only
}
secure.example:$TLS_PORT {
    tls $WORK/tls.crt $WORK/tls.key
    respond 200 secure-ok
}
CONF
"$BIN" compile "$WORK/stats-swapped.Fulcrumfile" > "$WORK/stats-swapped.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile stats-swapped.Fulcrumfile 失败" >&2
  exit 1
}
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/stats-swapped.json")")
expect_status "换掉 /stats 夹具的上游（拆掉撞键，只留 id=swapped 一条）" 200 "$CODE"

CODE=$(admin_get /stats)
expect_status "换配置之后再打一次 GET /stats" 200 "$CODE"
if python3 "$WORK/stats_check.py" shape "$WORK/admin.out"; then
  ok "换配置之后 /stats 仍然是合法 JSON"
else
  fail "换配置之后 /stats 的形状坏了"
fi
if python3 "$WORK/stats_check.py" addrs "$WORK/admin.out" | grep -qx "127.0.0.1:19999"; then
  fail "★ ★ ★ R12：/stats 还看得到换配置之前的老上游 127.0.0.1:19999 —— 像是缓存了一份旧 Runtime"
else
  ok "★ ★ ★ 换配置之后老上游 127.0.0.1:19999（third 那条）不在 /stats 里了"
fi
FANOUT_TOTAL_AFTER=$(python3 "$WORK/stats_check.py" fanout_total "$WORK/admin.out" || echo "ERR")
if [ "$FANOUT_TOTAL_AFTER" = "1" ]; then
  ok "★ ★ ★ 换配置之后 fanout 只剩新配置里那一把键（不再是撞键的 2）——/stats 没举着旧的 Runtime 快照"
else
  fail "换配置之后 fanout 应该只有 1 把键（新配置只有一条 reverse_proxy），实际 $FANOUT_TOTAL_AFTER"
fi

# ── 判据 5：404 的「可用」清单里有 GET /stats（真 socket）───────────────────
CODE=$(admin_post /nope '{}')
expect_status "GET /stats 判据同一节里顺带核一次：/nope 仍然 404" 404 "$CODE"
if grep -q 'GET /stats' "$WORK/admin.out"; then
  ok "404 的可用清单里有 GET /stats"
else
  fail "404 的可用清单没有提到 GET /stats：$(cat "$WORK/admin.out")"
fi

# 收尾：clear 掉、换回 [4/4] 需要的干净基线，避免影响后面的装载日志判据。
CODE=$(admin_post "/load?overrides=clear" "$(cat "$WORK/ov.json")")
expect_status "[3.6/4] 收尾：clear 恢复" 200 "$CODE"
expect_status "收尾之后数据面恢复" 200 "$(probe "$BASE/")"

# ── [4/4] 装载日志该说的话 ──────────────────────────────────────────────────
echo "=== [4/4] 装载日志 ==="
# ★ ★ G52（隐式的、用户没写出来的东西必须可见）没有作废，它换了对象：
#   回落层删完之后，承担它的是缓存与静态文件那两处「把生效的默认值打出来」。
#   ⇒ 这条判据因此钉两件事：**回落那几个字不许再出现**，
#   而**缓存的生效设置必须出现**。
if grep -q '回落' "$WORK/proxy.log"; then
  fail "装载日志里还有「回落」—— 那一层已整层删除（G98）"
  grep '回落' "$WORK/proxy.log" >&2
else
  ok "★ 装载日志里再没有回落（那一层已归零）"
fi
# ★ G52 的接班人：`cache` 的生效设置必须被打出来，且要说清 `ttl` 是**兜底**。
#   ⚠ 少了后半句，从 nginx 迁过来的人会以为它是覆盖 —— 而那是一个
#   与用户预期相反、却不说出来的默认。
if grep '缓存：站点' "$WORK/proxy.log" | grep -q '兜底'; then
  ok "★ 装载日志说出了缓存的生效设置（含「ttl 是兜底」这一句）"
else
  fail "装载日志没把缓存的生效设置说出来（G52 的接班人）"
  grep -i '缓存' "$WORK/proxy.log" >&2 || sed -n '1,20p' "$WORK/proxy.log" >&2
fi
# ── ⏳ 未接线的能力也要说出来，**而已经接线的不许再被说** ──────────────────
#
# ⚠ ⚠ 这条断言的锚点在两天里被迫换了两次（批 10 `dns_refresh` 接线、
#   批 11 `health_uri` 接线），两次都是**当场红给我看的** —— 那是它在正常工作。
#   ★ 但它暴露了一个真问题：**锚点是个会移动的靶子**。等最后一条未接线能力
#   也做完，这条断言会无处可锚，而那一天最省事的做法是把它删掉。
#
# ⇒ 改成两半，各自守一个方向：
#   ① **正向**：这行公告的**固定前缀**必须出现（它不随任何一条能力的死活而变），
#      而且这一份夹具确实用到了一条仍未接线的能力（`passive_fail`）。
#   ② ★ ★ **反向**：**已经接线的能力不许出现在这行公告里**。
#      这一半才是真正会**无声**的那种烂法 —— 一条假警告不会红，
#      它只会让人渐渐不看这张表。批 8 接线完 ACME 之后，
#      `dsl-reference.md` 那句清单挂着「自动签发」躺了四天没人发现，
#      就是因为**没有任何东西在看反向那一半**。
UNWIRED_LINES=$(grep -F '这一批还没接线' "$WORK/proxy.log" || true)
if [ -n "$UNWIRED_LINES" ]; then
  ok "装载日志有未接线能力的公告"
else
  fail "装载日志里一条未接线公告都没有（而夹具里写了 passive_fail）"
fi
if printf '%s' "$UNWIRED_LINES" | grep -qF 'passive_fail'; then
  ok "公告点名了 passive_fail（它确实还没接线）"
else
  fail "公告里没有 passive_fail：$UNWIRED_LINES"
fi
# ★ ★ 反向：这几条**已经接线**了，公告里再出现就是假警告。
STALE=""
for cap in health_uri dns_refresh admin acme; do
  if printf '%s' "$UNWIRED_LINES" | grep -qF "$cap"; then
    STALE="$STALE $cap"
  fi
done
if [ -z "$STALE" ]; then
  ok "★ 已经接线的能力没有出现在未接线公告里（没有假警告）"
else
  fail "未接线公告里还挂着已经接线的能力：$STALE"
fi
# ★ 证书装载也要在日志里留痕：装了几个 SNI、哪些。
if grep -q '已装载 1 个 SNI' "$WORK/proxy.log"; then
  ok "装载日志说出了装了几张证书"
else
  fail "装载日志里没有证书装载那一行"
  grep -i 'SNI\|证书' "$WORK/proxy.log" >&2 || true
fi
# ★ ★ 装载日志是 `default_sni` 在运行中**唯一**看得见的地方。
#   ⚠ 它此前正是卡在「配了」与「生效了」分不开这一点上：编译得过、运行时零调用方，
#   而日志一个字都不说 ⇒ 配了它的人没有任何办法在现场发现它没接。
#   ★ 这一条与 9d 分工：9d 问行为，这一条问**运维看不看得见**。
if grep -qF 'default_sni secure.example' "$WORK/proxy.log"; then
  ok "装载日志说出了不带 SNI 的握手会用哪张证书"
else
  fail "装载日志没说 default_sni 生效到了哪个名字"
  grep -i 'SNI' "$WORK/proxy.log" >&2 || true
fi

echo
if [ "$FAILS" -ne 0 ]; then
  echo "SERVE TESTS FAILED: $FAILS 条断言不通过" >&2
  echo "── 代理日志 ──" >&2
  cat "$WORK/proxy.log" >&2
  echo "── 上游日志 ──" >&2
  cat "$WORK/upstream.log" >&2
  exit 1
fi
echo "SERVE TESTS PASSED —— 路由决策被真流量执行对了（转发 / 改写 / header_up / 重定向 / 421 / file_server 自研 / cache 裹转发 / keep-alive / HTTPS+SNI+h2）。"
