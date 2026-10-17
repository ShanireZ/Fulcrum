#!/bin/sh
# **產物裡真的連結了哪幾套 TLS**（結案 D23）。
#
# 用法：`sh tests/ci/tls-linkage.sh <二进制>` —— 往標準輸出打若干行 `键=值`。
# ⛔ 它自己**不判紅**：判據在呼叫方（`tests/musl/product.sh`）。
#   ★ 分開是有意的，與那份 `evidence.txt` 的既有形狀一致：取數與判定各一處，
#   一份「讀起來像結論」的自由文字會誘人用 `grep 通过` 去判。
#
# ## ★ ★ 這一格與門 4 / 門 5 問的**不是同一個問題**
#
# | 問題 | 誰答 |
# |---|---|
# | `Cargo.lock` 裡寫著哪些 | 供應鏈門 4 |
# | 相依圖裡真有哪些 | 門 5（`cargo tree -e all --target all`）|
# | **產物裡真的連結了哪些** | **本指令碼** |
#
# ⚠ `Cargo.lock` 是相依圖的超集（「鎖 ≠ 圖」已被實測抓到過一次），
# 而「圖 ≠ 產物」同樣不能靠推理當成同一件事 —— 圖裡有、而產物裡其實沒連結，
# 是門 5 **原理上**看不見的那一半。
#
# ## ⚠ ⚠ 為什麼判符號而不是判字串
#
# `strings` 裡 `boringssl` 出現三百多次，幾乎全是**原始碼路徑**（panic 位置、偵錯資訊）；
# 而 `rustls` 出現 4 次，**全部**來自 `rustls_pki_types`。
# ⇒ 一條 `strings | grep -i rustls` 的門會**永遠誤報**，然後有人給它加一條豁免 ——
# 而那條豁免早晚會蓋住一次真的。★ 符號表說的是「哪些程式碼真的被連結進來了」。
#
# ## ★ ★ ★ `rustls_pki_types` 是允許的，而 `rustls` 不是
#
# 前者是 `instant_acme` 拉進來的**純類型 crate**（`CertificateDer` / `PrivatePkcs8KeyDer`），
# 裡面**沒有協定實作、沒有密碼學**；後者才是 TLS 棧本身。
# ⇒ 判據用 Rust v0 名字修飾的**長度前綴**把兩者精確分開：
# `_6rustls` 是 crate 名恰好為 `rustls`，而 `_16rustls_pki_types` 裡 `_` 與 `6` 之間隔著 `1`
# ⇒ **`_6rustls` 不是它的子串**。⛔ 別改成 `grep rustls`，那正是上面那條陷阱。
set -eu

BIN=${1:?用法：tls-linkage.sh <二进制>}

# ⚠ 讀不到符號表時**不許靜默給 0** —— 那會讓每一條「必須為 0」的判據憑空全綠。
#   ⇒ 打一個 `TLS_NM=missing`，由呼叫方判紅（「沒能檢查」不算「檢查通過」）。
if ! command -v nm >/dev/null 2>&1; then
  echo "TLS_NM=missing"
  exit 0
fi
# ⚠ ⚠ ★ **`nm` 失敗** 與 **符號表真的被剝掉** 是兩件事，⛔ 原先它們都打 `stripped`。
#   實測（2026-09-08）：路徑寫錯、或者傳進來的根本不是目標檔案，得到的也是
#   `TLS_NM=stripped` —— 於是人拿著「它被剝了符號」這句話去查一個**不存在的檔案**。
#   nm 的原話是 `file format not recognized` / `No such file or directory`，
#   那比任何分類都準。
# ★ ★ 這個分法**站得住，是量出來的**（2026-09-08 在 `fulcrum-build:local` 裡實測
#   GNU binutils 的結束代碼，⛔ 不是推斷）：
#     · 有符號            ⇒ rc=0，stdout 非空          ⇒ ok
#     · `strip` 過的       ⇒ rc=**0**，stderr `no symbols` ⇒ stripped（真被剝了）
#     · 不是目標檔案       ⇒ rc=1，`file format not recognized`
#     · 檔案不存在         ⇒ rc=1，`No such file`
#   ⇒ 「rc≠0」與「被剝符號」在這個平台上**確實是兩條不相交的路**，
#   下面那句「這不是被剝掉」才敢寫。⚠ 哪天 binutils 改了結束代碼約定，
#   要紅的是這張表，⛔ 別默默把那句斷言留著。
# ★ 失敗路徑上**再問一次**只為取 stderr（`2>&1 >/dev/null`，⛔ 順序不能反）——
#   與 `tests/ci/shellcheck-all.sh`、`tests/musl/arm64-trigger.sh` 同一套寫法。
# ⚠ `|| nm_rc=$?` 接住，⛔ 否則 `set -e` 在這裡就把指令碼掐了。
# ★ 新取值 `nm-failed` 對呼叫方是安全的：`tests/musl/product.sh` 判的是
#   `[ "$tls_nm" != "ok" ]` ⇒ 任何非 ok 一律判紅，**不會**因為多一種取值而放行。
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
  # ★ 走到這裡 nm **成功**了、只是一條符號都沒有 ⇒ 「被剝掉」這句話現在是有據的。
  echo "TLS_NM=stripped"
  exit 0
fi
echo "TLS_NM=ok"

count() { printf '%s\n' "$SYMS" | grep -c "$1" || true; }

# ── 正向：BoringSSL 獨有的符號 ──────────────────────────────────────────────
#
# ★ ★ `SSL_CTX_set_select_certificate_cb` **不是隨便挑的**：它正是 G6 第 1 條
#   「兩個入口（h1/h2 與 h3/QUIC）共用同一個證書選擇回呼」的執行者
#   ⇒ 這條判據與那條鎖死的架構約束**是同一件事**，而不是一個湊數的探針。
# ⚠ 另兩個也各自只在 BoringSSL 裡有：`SSL_error_description`（OpenSSL 無）、
#   `CRYPTO_BUFFER_new`（BoringSSL 特有的緩衝類型）。
# ⛔ **不要用 `BORINGSSL_self_test`** —— 它只在 FIPS 建置裡存在，非 FIPS 下恆為 0
#   （寫這個指令碼時當場量到的，差點選它當判據）。
BORING=0
for s in SSL_CTX_set_select_certificate_cb SSL_error_description CRYPTO_BUFFER_new; do
  n=$(count " [TtWw] $s\$")
  BORING=$((BORING + n))
done
echo "TLS_BORINGSSL_ONLY=$BORING"

# ── 反向 A：OpenSSL 獨有的符號，一個都不許有 ────────────────────────────────
#
# ★ 這四個在 BoringSSL 裡**不存在**（它有意刪掉了 ENGINE 那一套與 OpenSSL 的初始化門面）
# ⇒ 它們分得開 BoringSSL 與 OpenSSL，而單看 `SSL_CTX_new` 一類共有符號分不開。
OSSL=0
for s in OPENSSL_init_ssl ENGINE_init OpenSSL_version SSL_CTX_set_ssl_version; do
  n=$(count " [TtWwDdBb] $s\$")
  OSSL=$((OSSL + n))
done
echo "TLS_OPENSSL_ONLY=$OSSL"

# ── 反向 B：rustls **本體**一個符號都不許有 ─────────────────────────────────
echo "TLS_RUSTLS_IMPL=$(count '_6rustls')"
# ★ 只報不判：它是允許的（純類型 crate），但**數出來寫在證據裡** ——
#   哪天它變成 0，說明 `instant_acme` 換了相依，那是一件該被看見的事。
echo "TLS_RUSTLS_PKI_TYPES=$(count '_16rustls_pki_types')"

# ── ★ ★ ★ 掃描器自證：這個形狀**命中得了** ─────────────────────────────────
#
# ⚠ 上面三條「必須為 0」的判據，**failure mode 是模式寫錯而不是真的沒有** ——
#   一個寫錯的 `grep` 與「真的一個都沒有」給出完全相同的 0。
# ⇒ 拿一個**必然存在**的同形狀模式做對照（本產品自己的 crate 名）。
#   呼叫方要求它 > 0，否則整組判據一律不可信。
echo "TLS_PROBE_CONTROL=$(count '_14fulcrum_server')"
