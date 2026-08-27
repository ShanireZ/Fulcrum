# Issue draft (file this first — CONTRIBUTING requires it for non-trivial PRs)

**Title**

```
Update lru to 0.18.2 to move pingora's crates off RUSTSEC-2026-0253 (currently 0.16.4)
```

**Labels**: none (leave to maintainers)

---

**Body**

`[workspace.dependencies]` currently pins `lru = "0.16.3"`, which resolves to `0.16.4`.

[RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253) (published 2026-05-12) reports a
potential use-after-free caused by a lack of panic safety in `LruCache::pop()`. The advisory's affected
range is everything below `0.18.2`, and `0.18.2` is the first fixed release
([lru-rs#238](https://github.com/jeromefroe/lru-rs/pull/238)).

### Where it shows up

`cargo audit` — which `build.yml` runs on the 1.97.1 leg — already lists it on `main`:

```
Crate:     lru
Version:   0.16.4
Warning:   unsound
Title:     Potential use-after-free due to lack of panic safety in `LruCache::pop()`
Date:      2026-05-12
ID:        RUSTSEC-2026-0253
```

The job still exits 0, because RustSec classifies this as `unsound` rather than a vulnerability and
plain `cargo audit` does not fail on warnings. So this is easy to miss rather than obviously broken —
and the advisory has been out since 2026-05-12 without one of the automated `RUSTSEC-…` issues being
opened for it, which is why I am filing this by hand.

### In-tree consumers

`pingora-cache` (`eviction::simple_lru`, `hashtable`), `pingora-pool`, `pingora-core` (the s2n session
cache, behind the `s2n` feature), `pingora-lru`, and `TinyUFO`.

### Reachability

The advisory needs a key or value whose `Drop` implementation panics while `pop()` is running.

The in-tree instantiations are all keyed by integers (`u64` in `eviction::simple_lru`, `u128` in
`hashtable`, the connection id in `pingora-pool`), so no key can panic on drop. Values are a different
matter: `ConcurrentLruCache<V, N>` is generic, so reachability there depends on what a caller puts in
rather than on anything in this repository.

I have not tried to turn that into a claim that the advisory is unreachable — it is a property of
today's instantiations, not an invariant. Please correct me if you have already assessed this; I could
not find it in `.cargo/audit.toml`, which only carries the three `rustls-webpki` entries.

### Proposed change

Bump `[workspace.dependencies] lru` to `0.18.2`. That is a one-line change; no source changes are
required.

**MSRV is unaffected.** `lru` 0.17 and later declare `rust-version = 1.85.0`, which is exactly the MSRV
floor in the `build.yml` matrix. (`lru` 0.16.x declared 1.70.0, so this does consume the remaining
headroom — if the MSRV floor is ever lowered below 1.85.0, this bump would have to be revisited.)

### One caveat, stated up front

**This will not make the `cargo audit` line disappear.** `aws-sdk-s3` pulls in its own `lru`, and
every version below `0.18.2` is in the advisory's range. Which version that is depends on how the lock
file was generated — the audit job uses `cargo generate-lockfile --ignore-rust-version`:

| lock generated with | `main` today | with the bump |
|---|---|---|
| `--ignore-rust-version` (what `audit.yml` does) | `0.16.4` | `0.16.4` + `0.18.2` |
| normal resolution | `0.12.5` + `0.16.4` | `0.12.5` + `0.18.2` |

So after the bump the remaining affected entry belongs to `aws-sdk-s3` alone. It is an **optional**
dependency of `pingora-runtime`, reachable only through the non-default `dial9-worker-s3` feature —
`cargo tree -i aws-sdk-s3 --workspace` finds nothing on a default build — but it is in the lock file,
and the lock file is what `cargo audit` scans. That edge is outside this repository's control.

So the value of this change is narrower than "fixes the advisory": it is that **pingora's own crates
(`pingora-cache`, `pingora-core`, `pingora-lru`, `pingora-pool`, `TinyUFO`) stop using an affected
version of `lru`**.

### Verification

Run against `main` (`0046038`) with the bump applied, using the same commands as `build.yml`:

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo check --workspace` (1.97.1) | pass, no source changes |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | pass |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | pass |
| `cargo machete` | pass |
| `cargo test --lib --bins --tests --no-fail-fast` | 116 failures on unmodified `main`, 117 with the bump, in my environment (integration tests need openresty, which I did not install). The one extra is `pingora-memory-cache`'s `tests::test_eviction`, which is **already flaky on `main`** (1 failure in 20 runs on each side) and cannot reach `lru` — that crate does not depend on it, and `TinyUFO` has `lru` only as a dev-dependency for benchmarks. |
| `cargo audit` | unchanged, see the caveat above |

I have the one-line patch ready and am happy to open a PR if you would like it.
