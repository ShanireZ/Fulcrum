#!/bin/sh
# **产物里真的链接了哪几套 TLS**（结案 D23）。
#
# 用法：`sh tests/ci/tls-linkage.sh <二进制>` —— 往标准输出打若干行 `键=值`。
# ⛔ 它自己**不判红**：判据在调用方（`tests/musl/product.sh`）。
#   ★ 分开是有意的，与那份 `evidence.txt` 的既有形状一致：取数与判定各一处，
#   一份「读起来像结论」的自由文本会诱人用 `grep 通过` 去判。
#
# ## ★ ★ 这一格与门 4 / 门 5 问的**不是同一个问题**
#
# | 问题 | 谁答 |
# |---|---|
# | `Cargo.lock` 里写着哪些 | 供应链门 4 |
# | 依赖图里真有哪些 | 门 5（`cargo tree -e all --target all`）|
# | **产物里真的链接了哪些** | **本脚本** |
#
# ⚠ `Cargo.lock` 是依赖图的超集（「锁 ≠ 图」已被实测抓到过一次），
# 而「图 ≠ 产物」同样不能靠推理当成同一件事 —— 图里有、而产物里其实没链接，
# 是门 5 **原理上**看不见的那一半。
#
# ## ⚠ ⚠ 为什么判符号而不是判字符串
#
# `strings` 里 `boringssl` 出现三百多次，几乎全是**源码路径**（panic 位置、调试信息）；
# 而 `rustls` 出现 4 次，**全部**来自 `rustls_pki_types`。
# ⇒ 一条 `strings | grep -i rustls` 的门会**永远误报**，然后有人给它加一条豁免 ——
# 而那条豁免早晚会盖住一次真的。★ 符号表说的是「哪些代码真的被链接进来了」。
#
# ## ★ ★ ★ `rustls_pki_types` 是允许的，而 `rustls` 不是
#
# 前者是 `instant_acme` 拉进来的**纯类型 crate**（`CertificateDer` / `PrivatePkcs8KeyDer`），
# 里面**没有协议实现、没有密码学**；后者才是 TLS 栈本身。
# ⇒ 判据用 Rust v0 名字修饰的**长度前缀**把两者精确分开：
# `_6rustls` 是 crate 名恰好为 `rustls`，而 `_16rustls_pki_types` 里 `_` 与 `6` 之间隔着 `1`
# ⇒ **`_6rustls` 不是它的子串**。⛔ 别改成 `grep rustls`，那正是上面那条陷阱。
set -eu

BIN=${1:?用法：tls-linkage.sh <二进制>}

# ⚠ 读不到符号表时**不许静默给 0** —— 那会让每一条「必须为 0」的判据凭空全绿。
#   ⇒ 打一个 `TLS_NM=missing`，由调用方判红（「没能检查」不算「检查通过」）。
if ! command -v nm >/dev/null 2>&1; then
  echo "TLS_NM=missing"
  exit 0
fi
# ⚠ ⚠ ★ **`nm` 失败** 与 **符号表真的被剥掉** 是两件事，⛔ 原先它们都打 `stripped`。
#   实测（2026-09-08）：路径写错、或者传进来的根本不是目标文件，得到的也是
#   `TLS_NM=stripped` —— 于是人拿着「它被剥了符号」这句话去查一个**不存在的文件**。
#   nm 的原话是 `file format not recognized` / `No such file or directory`，
#   那比任何分类都准。
# ★ ★ 这个分法**站得住，是量出来的**（2026-09-08 在 `fulcrum-build:local` 里实测
#   GNU binutils 的退出码，⛔ 不是推断）：
#     · 有符号            ⇒ rc=0，stdout 非空          ⇒ ok
#     · `strip` 过的       ⇒ rc=**0**，stderr `no symbols` ⇒ stripped（真被剥了）
#     · 不是目标文件       ⇒ rc=1，`file format not recognized`
#     · 文件不存在         ⇒ rc=1，`No such file`
#   ⇒ 「rc≠0」与「被剥符号」在这个平台上**确实是两条不相交的路**，
#   下面那句「这不是被剥掉」才敢写。⚠ 哪天 binutils 改了退出码约定，
#   要红的是这张表，⛔ 别默默把那句断言留着。
# ★ 失败路径上**再问一次**只为取 stderr（`2>&1 >/dev/null`，⛔ 顺序不能反）——
#   与 `tests/ci/shellcheck-all.sh`、`tests/musl/arm64-trigger.sh` 同一套写法。
# ⚠ `|| nm_rc=$?` 接住，⛔ 否则 `set -e` 在这里就把脚本掐了。
# ★ 新取值 `nm-failed` 对调用方是安全的：`tests/musl/product.sh` 判的是
#   `[ "$tls_nm" != "ok" ]` ⇒ 任何非 ok 一律判红，**不会**因为多一种取值而放行。
nm_rc=0
SYMS=$(nm "$BIN" 2>/dev/null) || nm_rc=$?
if [ "$nm_rc" -ne 0 ]; then
  nm_err=$(nm "$BIN" 2>&1 >/dev/null) || true
  echo "★ nm 读不了这个文件（退出码 $nm_rc）——⛔ 这**不是**「符号表被剥掉」。" >&2
  echo "  \`nm $BIN\` 自己的原话：" >&2
  printf '%s\n' "$nm_err" | sed 's/^/      /' >&2
  echo "TLS_NM=nm-failed"
  exit 0
fi
if [ -z "$SYMS" ]; then
  # ★ 走到这里 nm **成功**了、只是一条符号都没有 ⇒ 「被剥掉」这句话现在是有据的。
  echo "TLS_NM=stripped"
  exit 0
fi
echo "TLS_NM=ok"

count() { printf '%s\n' "$SYMS" | grep -c "$1" || true; }

# ── 正向：BoringSSL 独有的符号 ──────────────────────────────────────────────
#
# ★ ★ `SSL_CTX_set_select_certificate_cb` **不是随便挑的**：它正是 G6 第 1 条
#   「两个入口（h1/h2 与 h3/QUIC）共用同一个证书选择回调」的执行者
#   ⇒ 这条判据与那条锁死的架构约束**是同一件事**，而不是一个凑数的探针。
# ⚠ 另两个也各自只在 BoringSSL 里有：`SSL_error_description`（OpenSSL 无）、
#   `CRYPTO_BUFFER_new`（BoringSSL 特有的缓冲类型）。
# ⛔ **不要用 `BORINGSSL_self_test`** —— 它只在 FIPS 构建里存在，非 FIPS 下恒为 0
#   （写这个脚本时当场量到的，差点选它当判据）。
BORING=0
for s in SSL_CTX_set_select_certificate_cb SSL_error_description CRYPTO_BUFFER_new; do
  n=$(count " [TtWw] $s\$")
  BORING=$((BORING + n))
done
echo "TLS_BORINGSSL_ONLY=$BORING"

# ── 反向 A：OpenSSL 独有的符号，一个都不许有 ────────────────────────────────
#
# ★ 这四个在 BoringSSL 里**不存在**（它有意删掉了 ENGINE 那一套与 OpenSSL 的初始化门面）
# ⇒ 它们分得开 BoringSSL 与 OpenSSL，而单看 `SSL_CTX_new` 一类共有符号分不开。
OSSL=0
for s in OPENSSL_init_ssl ENGINE_init OpenSSL_version SSL_CTX_set_ssl_version; do
  n=$(count " [TtWwDdBb] $s\$")
  OSSL=$((OSSL + n))
done
echo "TLS_OPENSSL_ONLY=$OSSL"

# ── 反向 B：rustls **本体**一个符号都不许有 ─────────────────────────────────
echo "TLS_RUSTLS_IMPL=$(count '_6rustls')"
# ★ 只报不判：它是允许的（纯类型 crate），但**数出来写在证据里** ——
#   哪天它变成 0，说明 `instant_acme` 换了依赖，那是一件该被看见的事。
echo "TLS_RUSTLS_PKI_TYPES=$(count '_16rustls_pki_types')"

# ── ★ ★ ★ 扫描器自证：这个形状**命中得了** ─────────────────────────────────
#
# ⚠ 上面三条「必须为 0」的判据，**failure mode 是模式写错而不是真的没有** ——
#   一个写错的 `grep` 与「真的一个都没有」给出完全相同的 0。
# ⇒ 拿一个**必然存在**的同形状模式做对照（本产品自己的 crate 名）。
#   调用方要求它 > 0，否则整组判据一律不可信。
echo "TLS_PROBE_CONTROL=$(count '_14fulcrum_server')"
