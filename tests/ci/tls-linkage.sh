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
SYMS=$(nm "$BIN" 2>/dev/null || true)
if [ -z "$SYMS" ]; then
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
