---
type: Platform Contract
title: Host and Gate Traps
description: Scenario switches, port ownership, cleanup, Docker/MSYS limits, and criteria that prevent false green results.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:17:25Z
sources:
  - resource: ../../AGENTS.md
  - resource: ../../tests/m0/docker-run.sh
  - resource: ../../tests/lib/vol-lock.sh
  - resource: ../../tests/ci/shellcheck-all.sh
---

# Host traps, gate discipline, scenario flags and port map

These are contributor requirements. [AGENTS.md](../../AGENTS.md) gives the short entrypoint; [build and test](build-and-test.md) defines gate scope. Source scripts own actual dispatch and checks. Historical machine observations are archived outside Git; reusable failure mechanisms and limitations remain here.

## Scenario flags

Set one `*_ONLY=1` on `bash tests/m0/docker-run.sh`:

| Flag | Scope |
|---|---|
| `BUILD_ONLY` | Release build only; does not compile test targets. |
| `COMPILE_ONLY` | All-target compilation and applicable shellcheck; no Rust tests. Hook optimizations are described in build-and-test.md. |
| `LINT_ONLY` | fmt, clippy -D warnings, shellcheck. |
| `UNIT_ONLY` | Fulcrum's own crate tests. |
| `VENDOR_ONLY` | Fork regression net; first gate after rebase. |
| `SERVE_ONLY` | Real-traffic data-plane end-to-end. |
| `L4_ONLY` | TCP/UDP, SNI/ALPN, PROXY, listener handover. |
| `FILES_ONLY`, `CACHE_ONLY`, `CACHEDISK_ONLY`, `ENCODE_ONLY` | Files, memory cache, disk backend, compression. |
| `H3_ONLY`, `RELAY_ONLY` | HTTP/3, cross-generation QUIC relay. |
| `TLSPOL_ONLY` | Global TLS group/resumption policy on real handshakes (G168). |
| `PP_ONLY`, `LOG_ONLY` | HTTP PROXY protocol, structured access logs. |
| `METRICS_ONLY` | Prometheus endpoint and quantitative consistency. |
| `STATS_ONLY` | Runtime optional fields and degenerate states. |
| `ACME_ONLY`, `RENEW_ONLY` | Real local pebble issuance and renewal. |
| `SMOKE_ONLY`, `STRESS_ONLY`, `MUSL_ONLY`, `UNCLAIMED_ONLY` | Smoke self-check, sustained load, product static artifact, inherited-fd cleanup. |
| `BENCH_ONLY` | M3 harness/criteria in the comparison image; **no performance result** (G132). |

Most have matching `<NAME>_TESTS=0`; lint uses `LINT=0`. `M1_TESTS=0` skips systemd, and `DOCS_GATE=0` disables both documentation prechecks. Systemd, product-musl, and benchmark stages use their own containers. Systemd can run through `bash tests/m1/systemd-run.sh`; benchmark through `bash tests/bench/run.sh`.

The full gate never uses hook shortcuts `COMPILE_SHELLCHECK=0 HOST_SELFTESTS=auto VOL_REPORT=0`. `auto` skips host self-tests only with a matching worktree-local content/tool fingerprint; no value unconditionally skips them. Dispatch self-tests must recognize each new ONLY/TESTS pair.

Shell inventory is derived within configured roots and checked by a wider `git ls-files --cached --others --exclude-standard` census. New untracked scripts count; ignored workspace drafts do not. Only upstream `vendor/` is explicitly excluded. A new top-level script root must fail until its coverage is declared.

## Port allocation

Run scenarios sequentially. Each owns its range; a new scenario takes a free range and returns all listeners on exit so a stale process cannot satisfy the next baseline.

| scenario | ports |
|---|---|
| `tests/m0/` | 8080–8082 for actual M0 listeners; 9000 remains reserved (also used as an absent-port synthetic parser fixture). |
| `tests/serve/` | 9100–9110 (9107–9109 are the thread-model section, G140; 9110 is a python upstream echoing request header names, G163) |
| `tests/stress/` · `tests/smoke/` | 9200–9201 · 9210–9212 |
| `tests/l4/` | 9300–9317 |
| `tests/files/` | 9400–9401 |
| `tests/acme/run.sh` | 8053 · 8055 · 8083 · 9443–9444 · 14000 · 15000 |
| `tests/acme/renew.sh` | 8054 · 8056 · 8084 · 9445–9446 · 14001 · 15001 |
| `tests/cache/run.sh` · `tests/cache/disk.sh` | 9500–9501 · 9502–9503 |
| `tests/encode/` | 9600–9601 |
| `tests/h3/` | 9700–9702 |
| `tests/tls-policy/` | 9450–9453 (TCP and h3; every config sets `auto_http_redirect false`, so it does **not** bind `:80`) |
| `tests/proxyproto/` | 9800–9803 |
| `tests/log/` | 9900–9906 |
| `tests/quic-relay/` | 9910–9911 |
| `tests/metrics/` | 9920–9926 (9923 and 9926 are python upstreams, not Fulcrum listeners) |
| `tests/stats/` | 9930–9932 (range 9930–9935 reserved) |
| `bench/` (M3) | 9940–9953, 9956–9967 (range 9940–9967 reserved) — 9940–9943 the four contenders in `bench/case/static-throughput.sh`, 9944–9945 the two variants in `bench/diag/respond-ceiling.sh`, 9946–9948 the origin and two cache-backend variants in `bench/diag/cache-hit-p99.sh`, 9949–9953 the origin and four contenders in `bench/case/reverse-proxy-throughput.sh`, 9956–9959 the four contenders and 9960 the request-capture stub in `bench/case/short-connection-throughput.sh`, 9961–9964 the four contenders and 9965 the request-capture stub in `bench/case/tls-handshake.sh`, 9966–9967 the two real-server reverse checks of gate criterion C23 (`tests/bench/gate.sh`) — ⚠ **different container, own netns**, see below |
| *(registered, never listened on)* | 19999 — see below |
| *(shared, hardcoded)* | 80 — see below |

The benchmark ranges remain reserved even though their container currently has a separate network namespace. A future host-network change must not silently collide with another scenario. Read the table rather than inventing a one-off scanner; a pattern such as `[A-Z_]+` misses names with digits, including `H3_PORT`.

Port 19999 is intentionally not listened on and not connected to: serve/metrics use it only as an override key that becomes dangling. A real connection probe must use its scenario's assigned range instead of whatever host process might occupy 19999.

### Implicit port 80

`synthesize_http_redirect` emits a fixed :80 308 site for auto-HTTPS addresses (G137, RFC8555 HTTP-01 requires :80). An address with a hostname and no explicit `http://` defaults to HTTPS; manually writing `tls` does not disable the redirect. Only explicit HTTP on all addresses or global `auto_http_redirect false` avoids it. A deployment with :80 already owned elsewhere cannot use this automatic redirect.

Known implicit binders include ACME issuance/renewal, serve, log, h3, proxyproto, and quic-relay. Metrics and stats deliberately disable redirect; stats also asserts the compiled listener set is exactly `{9932}`. TLS-policy configs disable it while running multiple policy instances. This list is manually maintained, so re-derive it from compiled configs after adding TLS sites rather than trusting an old comment.

Any IPv4 LISTEN on :80, including loopback and different SO_REUSEADDR settings, can obstruct the redirect. The old Pingora0.8 process-wide lock held through a retrying bind could stall unrelated listeners; current0.9 uses short/per-address locking, so do not present that old global-lock failure as current implementation. A busy port still requires diagnosis, cleanup, and real listener checks.

Every generation-spawning scenario returns :80 too, skipping blame for a port already occupied on entry. Register PIDs in the caller: `GEN2=$(start_gen ...)` runs in a subshell, so `PIDS+=` there mutates a copy and can leave cleanup with no recorded process despite green assertions.

## Gate discipline

### An early stop truncates the probe

Cargo stops a dependency chain at its first compile failure; downstream injected sites may never compile. Completion means the final pass leaves no unexplored red downstream crate, not that one pass printed N errors. Print an injection count beside an independent census. The same subset trap appears with fail-fast tests, skipped dependent CI jobs, `grep -m 1`, and short-circuit operators.

Other required distinctions:

- Fixture shape must cover branch boundaries: IP/hostname, relative/absolute, ASCII/non-ASCII. Test quantity cannot establish that coverage.
- When a previously unwired capability ships, re-anchor positive/negative lists. A wired feature must not remain listed as unimplemented; guard both directions.
- A product claim must execute the product binary. A spike only establishes mechanism feasibility; inspect ExecStart and the actual artifact.
- Bulk rewrites/branch changes can leave apparently fresh stale binaries across a named target/bind mount. Require `Compiling <crate>` and actual new test execution. Treat mtime skew as a hypothesis until established, not a universal explanation.
- A gauge must distinguish known good and bad cases; a permanently empty output or refusal can look like a valid result.
- Measure delivery/framing, not TCP segment arrival. A single recv/read can split header/payload legally. Use a structural sender completion marker, such as half-close then read-to-EOF, with timeout only as a backstop. A deterministic payload delay can expose the faulty single-read assumption; increasing sleeps does not fix the criterion.
- `systemctl reload` zero only reports the trigger, not successful re-exec. Check changed pid, executable/inode, and converged response.
- Run a new failing criterion on a clean control tree before attributing failure to the patch.

After rebase or branch switch, refresh source mtimes **inside the container** before the gate if needed. In Git Bash:

```bash
export MSYS_NO_PATHCONV=1
docker run --rm -v "$(cygpath -m "$PWD"):/w" -w /w fulcrum-build:local bash -c 'find crates spikes -name "*.rs" -o -name Cargo.toml | xargs touch'
```

This is a refresh step, not test evidence. Avoid concurrent gates or edits to bind-mounted shell inputs. Linux hosts use their real absolute bind path instead of cygpath.

## Host traps

1. Export `MSYS_NO_PATHCONV=1` for documented Docker commands in Git Bash; otherwise container paths such as `/w` can be rewritten.
2. Use Windows-form absolute bind sources on MSYS (`cygpath -m`), and use the shared normalization routine for image/tree/cache identity. Docker Desktop staging paths must not recursively generate new tree identities; do not reimplement volume naming.
3. Keep shell/gate inputs LF via `.gitattributes`. Git status can be clean after checkout converted bytes. Inspect CR bytes with `tr -dc '\r' < file | wc -c`; MSYS grep normalizes line endings and is not a byte check.
4. Container `/etc/hosts` is a bind-mounted file: truncate/rewrite in place (`cat > /etc/hosts`), then restore in cleanup. `sed -i` and rename replace the inode and can be rejected.
5. If explicit exits make shellcheck label an EXIT trap unreachable (SC2317), remove unnecessary trailing `exit 0`; do not add a suppression.
6. Trap SIGPIPE (`trap '' PIPE`) when testing broken sockets, so a write becomes a checked return rather than silent exit141.
7. Bash read is not a whole-UDP-datagram client; use a proper datagram helper.
8. `curl --rate N` defaults to per hour without a unit; `-o` and `-w` apply per URL.
9. Shellcheck0.10 does not detect every function mutation behind command substitution. Keep caller pid ownership and cleanup-port assertions. A comment beginning `# shellcheck` is parsed as a directive; reword ordinary prose.
10. `git archive` can honor global autocrlf. Extract official comparisons with `git -c core.autocrlf=false archive ...` and verify bytes, or a line-ending-only difference can obscure the actual patch.
11. Host cargo writes `target/.rustc_info.json` even without building, violating G107's empty-host-target criterion. `dep-check.py` defaults to container cargo; supplying host cargo reintroduces the failure. An empty target mountpoint left by a subpath volume can cause the same artifact from later container `cargo metadata`. For ad-hoc vendor cargo use `--target-dir /w/target/<name>` and mount a target volume at `/w/target`, not a nested vendor target. Root target needs that mount even for `cargo tree`/metadata. Remove accidental host artifacts as such, without deleting unrelated work.

## Dependency-check limits

Exit categories add: 10 eligible cargo updates, 20 manual fork rebase, 40 build-image attention/unverified, 80 systemd-major attention/unverified, 160 advisory/unverified, 1 error. Only package updates belong to cargo update's view; other branches need separate monitoring. These branches report rather than automatically rebase or change images.

Scan vendor/root advisories with one imported ACCEPTED registry. No cargo candidates means only that current upstream requirements permit no move, not that every package is latest. Inspect vendored Pingora, not crates.io. Rebase first checks what upstream already fixed and then runs all relevant gates. Changed systemd behavior in the MainPID-handover control revisits G37; it does not justify weakening the assertion. Inspect locked-version library source/docs rather than inferring API compatibility across releases.

See [dependency policy](dependency-policy.md), [supply chain](supply-chain.md), and [build and test](build-and-test.md).
