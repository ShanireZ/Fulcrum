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
            health_uri /health
            health_interval 1s
            health_timeout 1s
        }
    }
    handle /sickok/* {
        reverse_proxy 127.0.0.1:SICK_PORT {
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
CODE=$(admin_post /load '{ not json')
expect_status "管理面：坏载荷" 400 "$CODE"
CODE=$(run_curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://$HOST:$PROXY_PORT/")
expect_status "坏载荷之后旧配置还在服务" 200 "$CODE"

# ★ ★ 端口集变了 → 409，**且旧配置还在服务**。
#   这一条是「原子」的判据：一个「先换后校验」的实现在「换得动」那条上表现完全相同。
printf '%s\n' ":$((PROXY_PORT + 40)) {" "    respond 200 \"moved-port\"" "}" \
  > "$WORK/otherport.Fulcrumfile"
"$BIN" compile "$WORK/otherport.Fulcrumfile" > "$WORK/otherport.json" 2>/dev/null || {
  echo "SERVE TESTS FAILED: compile 生成不出结构化配置" >&2
  exit 1
}
CODE=$(admin_post /load "$(cat "$WORK/otherport.json")")
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
CODE=$(admin_post /load "$(cat "$WORK/next.json")")
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
