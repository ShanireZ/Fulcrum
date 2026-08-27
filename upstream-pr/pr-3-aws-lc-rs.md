# PR draft — open only AFTER the issue exists, and reference it

**Title**

```
Don't compile aws-lc-rs when the ring provider is requested
```

**Base**: `cloudflare/pingora` `main` · **Head**: `ShanireZ/pingora` `no-aws-lc-rs-with-ring`

> ★ **2026-08-19 复审重写**，三处改动：① 补上与 #630 / #887 的关系（上一版漏查了未合并的 PR）；
> ② 提交信息里的依赖图数字由 fork 的「175 → 173」改为上游实测的「178 → 176」；
> ③ 验证表按上游 `.github/workflows/build.yml` 的原命令在 pristine 克隆上重跑。

---

**Body**

Refs #<ISSUE_NUMBER>

`pingora-rustls` asks `rustls` and `tokio-rustls` for the `ring` provider, but neither
dependency disables default features, and both crates enable `aws_lc_rs` by default:

```
rustls 0.23.43        default = ["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]
tokio-rustls 0.26.4   default = ["logging", "tls12", "aws_lc_rs"]
```

So `features = ["ring"]` adds ring *alongside* aws-lc-rs rather than instead of it, and both
providers are compiled into every build that enables `pingora-core/rustls`. The aws-lc-rs one
is never used: `install_default_crypto_provider()` installs ring explicitly, and the crate's
only other crypto call is `ring::digest`.

`pingora-core`'s own `rustls = "0.23"` dev-dependency is a third such entry. Since it is
unconditional, it reaches even the **default (openssl) test build**: `cargo tree -p
pingora-core -e normal,dev` with no features shows `aws-lc-rs` and `aws-lc-sys` today.
That entry is only used for types in `connectors/http` tests (`ServerCertVerifier`,
`CertificateDer`, `DigitallySignedStruct`), so this patch gives it
`default-features = false` and **names no provider at all** — selecting one there would
repeat the mistake the rest of the patch fixes.

`aws-lc-sys` vendors ~69 MB of C source and builds it through cmake (ring is ~8.5 MB), so this
costs every `rustls`-feature consumer a C toolchain and the compile time, and it is a
particular obstacle for static musl builds.

This patch names the defaults that are actually wanted, since `default-features = false` also
drops `logging`, `std` and `tls12`. (`tokio-rustls` has no `std` feature; its `ring`,
`logging` and `tls12` features forward to the rustls features of the same name.) The one
default not restored is `prefer-post-quantum`, which is defined as `["aws_lc_rs"]` and whose
every `#[cfg]` site is under `src/crypto/aws_lc_rs/` — it does nothing under ring, and
restoring it would pull aws-lc-rs back in.

### Relation to #630 and #887

Both of those make the provider *selectable*, which is a larger design question. This PR is
deliberately orthogonal and manifest-only: it stops aws-lc-rs being compiled on today's
`main`, whichever way that question is eventually settled.

The reason it is not redundant with either is that `aws_lc_rs` arrives through **two**
independent doors, and both PRs change only the `rustls` line. Measured on `main`
(`0046038`), unique crates in `cargo tree -p pingora-core -e normal`:

| manifest shape | crates | `aws-lc-rs` / `aws-lc-sys` |
|---|---|---|
| `main` as-is, `--features rustls` | 178 | present |
| #630's `rustls` line applied alone, `--features rustls` | 178 | **still present** |
| #887 applied in full, `--features rustls` | 178 | **still present** |
| #887 applied in full, `--features rustls-no-provider` | 177 | **still present** (`ring` is gone) |
| this PR | **176** | **gone** |

The fourth row is worth a look from #887's side: `rustls-no-provider` does drop `ring`, but a
consumer who picks it in order to bring their own `CryptoProvider` still compiles aws-lc-rs
and its 69 MB of C.

If #887 lands first, what remains of this patch is the `tokio-rustls` entry, and its `ring`
feature should then follow the same optionality that #887 gives the `rustls` entry. Happy to
rebase it into that shape, or to close this in favour of a combined change — whichever the
maintainers prefer.

### Verification

All run against `main` (`0046038`) in a container, unpatched vs patched, using the commands
from `.github/workflows/build.yml`:

| Gate | Result |
|---|---|
| `cargo tree -p pingora-core --features rustls -e normal` | **178 → 176** crates; the two removed are exactly `aws-lc-rs` and `aws-lc-sys`, and **nothing is added** |
| `cargo tree -p pingora-core --features rustls -e normal,dev` | **260 → 258**, same two |
| `cargo tree -p pingora-core -e normal,dev` (no features) | **228 → 226**, same two |
| `cargo fmt --all -- --check` | PASS both |
| `cargo check --workspace` | PASS both |
| `cargo check -p pingora-core --all-targets`, with and without `rustls` | PASS both |
| `cargo build -p pingora-core --features rustls` | PASS both |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | PASS both |
| `cargo test -p pingora-core --lib --no-fail-fast --features rustls` | 566 passed / 7 failed / 2 ignored on both sides — **the failure set is identical**, both-way set difference empty. All seven are environmental in this container (no openresty; and Docker's default network answers `192.0.2.1`, so the connect-timeout family cannot time out) |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | PASS both — MSRV unaffected |
| `cargo audit` / `cargo machete` | not run locally — left to CI |

No source changes; the diff is one manifest.

### Note on TLS-backend feature combinations

This only touches the `rustls` path. The `openssl`, `boringssl` and `s2n` backends select
their own crates and are unaffected — `pingora-rustls` is only compiled when the `rustls`
feature is on.
