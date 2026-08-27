# PR draft (open only AFTER the issue exists, and reference it)

**Title**

```
Bump lru dependency from 0.16.3 to 0.18.2
```

**Base**: `cloudflare/pingora` `main` · **Head**: `ShanireZ:lru-0.18.2`

---

**Body**

Refs #<ISSUE_NUMBER>

`[workspace.dependencies] lru = "0.16.3"` resolves to `0.16.4`, which
[RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253) flags for a potential
use-after-free caused by a lack of panic safety in `LruCache::pop()`. `0.18.2` is the first fixed
release ([lru-rs#238](https://github.com/jeromefroe/lru-rs/pull/238)).

This is a one-line change. It moves `pingora-cache`, `pingora-core`, `pingora-lru`, `pingora-pool` and
`TinyUFO` onto `0.18.2`; no source changes are required.

**MSRV is unaffected, but the headroom is now gone.** `lru` 0.17 and later declare
`rust-version = 1.85.0`, exactly the MSRV floor in the `build.yml` matrix; 0.16.x declared 1.70.0. So
this passes today, and it would have to be revisited if the floor were ever lowered below 1.85.0.

**This does not silence `cargo audit`.** Every `lru` below `0.18.2` is in the advisory's range, and
`aws-sdk-s3` brings in its own. It is an **optional** dependency of `pingora-runtime`, reachable only
through the non-default `dial9-worker-s3` feature — `cargo tree -i aws-sdk-s3 --workspace` finds
nothing on a default build — but it is in the lock file, and the lock file is what `cargo audit`
scans. Measured with the audit job's own lock (`cargo generate-lockfile --ignore-rust-version`):

| | `lru` in lock | `cargo audit` |
|---|---|---|
| `main` today | `0.16.4` | warns, exit 0 (677 crates) |
| with this patch | `0.16.4` + `0.18.2` | warns, exit 0 (678 crates) |

That edge is outside this repository's control. The point of this change is that pingora's own crates
(`pingora-cache`, `pingora-core`, `pingora-lru`, `pingora-pool`, `TinyUFO`) stop using an affected
version.

### Verification

Commands taken from `build.yml`; run against `main` (`0046038`) with this patch applied:

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo check --workspace` (1.97.1) | pass, no source changes |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | pass |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | pass |
| `cargo machete` | pass |
| `cargo audit` | unchanged, see above |
| `cargo test --lib --bins --tests --no-fail-fast` | 116 failures on unmodified `main`, 117 with this patch, in my environment (the integration tests need openresty, which I did not install; a few network-dependent unit tests fail too). **The one extra is not a regression** — see below. |

I could not reproduce a clean test run locally because I did not install openresty, so that row is a
before/after comparison rather than an absolute pass — CI will give the real answer.

### About the one extra test failure

My before/after comparison is not a clean "identical failure set": the patched run had one more
failure, `pingora-memory-cache`'s `tests::test_eviction`. Two independent reasons it is not caused by
this change:

- **It is already flaky on unmodified `main`.** Running that crate's lib test binary 20 times on each
  side: 1 failure in 20 on `main`, 1 failure in 20 with the patch. Same rate.
- **It cannot reach `lru`.** `pingora-memory-cache` does not depend on `lru`; it caches through
  `TinyUFO`, whose library code does not use `lru` either — `lru` is a dev-dependency there, used by
  its benchmarks.

I have not investigated the flakiness itself and it is unrelated to this PR; happy to file it
separately if that is useful.

One note on running the MSRV leg locally, in case it saves someone time: `cargo check` has to be run
against a normally generated lock file. Reusing the `--ignore-rust-version` lock from the audit step
makes the 1.85.0 leg fail with `s2n-tls requires rustc 1.91`, which has nothing to do with this
change — it reproduces identically on unmodified `main`.
