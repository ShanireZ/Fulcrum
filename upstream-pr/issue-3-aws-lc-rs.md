# Issue draft — file this FIRST, then reference its number from the PR

> `.github/CONTRIBUTING.md`: *"Non-trivial PRs will also require a GitHub issue."*
> This changes which crypto provider gets compiled into every `rustls`-feature build,
> so it is not in their trivial list (typos / small refactors / docs). Issue goes first.
>
> ★ **2026-08-19 复审重写**。上一版有两处不合格：
> ① 全仓只查了代码、**没查未合并的 PR**，而 [#630](https://github.com/cloudflare/pingora/pull/630)
> 与 [#887](https://github.com/cloudflare/pingora/pull/887) 就在这片地上；
> ② 提交信息里的「175 → 173」是 **fork 的数字**，上游实测是 **178 → 176**。
> 两处都已修正，且新增一格实测：**那两个 PR 都没有把 aws-lc-rs 从图里拿掉。**

**Title**

```
pingora-rustls compiles aws-lc-rs even though it installs the ring provider
```

**Labels**: none preselected (the repo's issue templates don't require any)

---

**Body**

## Describe the bug

`pingora-rustls` asks `rustls` and `tokio-rustls` for the `ring` provider, but neither
dependency disables default features:

```toml
# pingora-rustls/Cargo.toml  (main 0046038, and 0.8.1)
rustls = { version = "0.23.12", features = ["ring"] }
tokio-rustls = "0.26.0"
```

Both crates enable `aws_lc_rs` by default:

```
rustls 0.23.43        default = ["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]
tokio-rustls 0.26.4   default = ["logging", "tls12", "aws_lc_rs"]
```

So `features = ["ring"]` does not select ring *instead of* aws-lc-rs — it adds ring
*alongside* it, and **both providers are compiled into every build that turns on
`pingora-core/rustls`**.

Note that aws-lc-rs arrives through **two independent doors**, `rustls` and `tokio-rustls`.
Closing only one has no effect on the dependency graph; the measurements below show this.

## Pingora info

**Pingora version**: `main` @ `0046038`; the same two lines are present in `0.8.1`
**Rust version**: `cargo 1.97.1 (c980f4866 2026-06-30)`
**Operating system version**: Debian 13 (trixie), container

## Steps to reproduce

```bash
git clone https://github.com/cloudflare/pingora && cd pingora
cargo tree -p pingora-core --features rustls -e normal -i aws-lc-rs
```

## Expected results

`pingora-rustls` installs ring explicitly:

```rust
pub fn install_default_crypto_provider() {
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}
```

and the only other crypto call in the crate is `ring::digest` (in `hash_certificate()`).
There is no code path that reaches the aws-lc-rs provider, so it should not be built.

## Observed results

```
aws-lc-rs v1.18.0
├── rustls v0.23.43
│   ├── pingora-rustls v0.8.0
│   │   └── pingora-core v0.8.0
│   └── tokio-rustls v0.26.4
│       └── pingora-rustls v0.8.0 (*)
└── rustls-webpki v0.103.14
    └── rustls v0.23.43 (*)
```

Both `aws-lc-rs v1.18.0` and `aws-lc-sys v0.44.0` are in the graph, reached through the
`rustls` dependency *and* through `tokio-rustls`.

### What it costs

`aws-lc-sys` vendors roughly **69 MB of C source** (AWS-LC, a BoringSSL fork) and builds it
through cmake; `ring` is about 8.5 MB. Every consumer of `pingora-core`'s `rustls` feature
therefore needs a working C toolchain and pays the compile time, for code that never runs.

For anyone linking statically against musl this is a concrete obstacle rather than just
overhead, and it enlarges the amount of compiled-in cryptographic code for no benefit.

## Additional context — relation to #630 and #887

Both open PRs make the provider *selectable*, which is a larger design question than this
issue. This issue is about a separate fact that neither of them addresses: **the
`tokio-rustls` entry is a second, independent path to `aws_lc_rs`, and neither PR touches
that line.**

Measured on `main` (`0046038`), `cargo tree -p pingora-core -e normal`, counting unique
crates:

| manifest shape | crates | `aws-lc-rs` / `aws-lc-sys` |
|---|---|---|
| `main` as-is, `--features rustls` | 178 | present |
| #630's `rustls` line applied alone, `--features rustls` | 178 | **still present** |
| #887 applied in full, `--features rustls` | 178 | **still present** |
| #887 applied in full, `--features rustls-no-provider` | 177 | **still present** (`ring` is gone) |
| both `rustls` **and** `tokio-rustls` defaults disabled | **176** | **gone** |

The fourth row is the one I would flag to the author of #887: `rustls-no-provider` succeeds
in dropping `ring`, but a consumer who selects it in order to bring their own
`CryptoProvider` still compiles aws-lc-rs and its 69 MB of C — which is a large part of what
they were trying to avoid. One extra line on the `tokio-rustls` entry closes it.

I have a patch for the minimal, orthogonal fix (manifest only, no source changes) and will
open a PR referencing this issue. It composes with either #630 or #887; if one of them lands
first, what remains of my patch is the `tokio-rustls` line.

## Suggested fix

Name the defaults that are actually wanted, since `default-features = false` also drops
`logging`, `std` and `tls12`:

```toml
rustls = { version = "0.23.12", default-features = false,
           features = ["ring", "logging", "std", "tls12"] }
tokio-rustls = { version = "0.26.0", default-features = false,
                 features = ["ring", "logging", "tls12"] }
```

(`tokio-rustls` has no `std` feature; its `ring`, `logging` and `tls12` features forward to
the rustls features of the same name.)

The one default deliberately not restored is `prefer-post-quantum`. It is defined as
`prefer-post-quantum = ["aws_lc_rs"]`, and every `#[cfg(feature = "prefer-post-quantum")]`
site in rustls lives under `src/crypto/aws_lc_rs/` — it has no effect under the ring
provider, and restoring it would pull aws-lc-rs straight back in.
