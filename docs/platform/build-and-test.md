---
type: Platform Contract
title: Build and Test
description: Docker prerequisites, full and scoped gates, scenario criteria, hooks, images, caches, and coverage limits.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:15:33Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../tests/m0/docker-run.sh
  - resource: ../../tests/lib/vol-lock.sh
  - resource: ../../tests/ci/pre-push.sh
  - resource: ../../tests/vendor/run.sh
---

# Build and test

Run from the repository root in Bash; use Git Bash on Windows. Rust build/test/lint runs **only in Docker** (G26/G107). Host cargo/rustup is editor tooling, not an alternative gate. Do not install toolchains or add compiler directories, `CARGO_TARGET_DIR`, or `LIBCLANG_PATH` to the host PATH to work around a build failure.

The project needs Linux descriptor transfer, Unix permissions/flock, signals, and systemd/cgroup behavior. A host Windows build cannot establish those properties. Host-side text checks need Python; Rust and native dependencies belong in the images.

## Commands and scope

```bash
bash tests/m0/docker-run.sh
BUILD_ONLY=1 bash tests/m0/docker-run.sh
COMPILE_ONLY=1 bash tests/m0/docker-run.sh
LINT_ONLY=1 bash tests/m0/docker-run.sh
UNIT_ONLY=1 bash tests/m0/docker-run.sh
bash tests/m1/systemd-run.sh
M1_ONLY=main bash tests/m1/systemd-run.sh
M1_KEEP=1 bash tests/m1/systemd-run.sh
```

The first command is the complete applicable development gate. `BUILD_ONLY` builds release binaries without test targets. `COMPILE_ONLY` compiles **all** test targets and runs no Rust tests; it normally also runs shellcheck. `LINT_ONLY` runs fmt, clippy, and shellcheck. `UNIT_ONLY` runs Fulcrum's own crate tests. `M1_KEEP=1` retains a failed systemd container for investigation.

All scenario-specific `*_ONLY`, matching `*_TESTS=0`, and assigned ports are maintained in [host and gate traps](host-and-gate-traps.md#scenario-flags). `LINT=0` disables lint; `DOCS_GATE=0` disables both host documentation prechecks. Narrowed or skipped gates prove only their stated scope, never full acceptance. New scenarios must update dispatch, dispatch self-test, documented switches, and port ownership.

## Host prechecks

These run before containers, including scoped modes:

| Check | Evidence | Limit |
|---|---|---|
| `python tools/docs-check.py` | Required Markdown metadata, repository/bundle links, index reachability. | Does not prove factual content. |
| `python tools/plan-refs.py` | The open-question projection agrees with PLAN and recognized references do not claim a closed D-question is still open. | Prose/semantic coverage is bounded by the parser; an unrecognized sentence is not validated. |

Use the real authorities and current parser output. A clean Git tree, matching remote, or green text check cannot establish that Rust test targets compile.

## Local pre-push hook

[tests/ci/pre-push.sh](../../tests/ci/pre-push.sh) invokes the shared `COMPILE_ONLY` path rather than building a second Docker command. It compiles all targets, runs applicable shellcheck and host checks, and runs **no Rust tests**. It does not establish test behavior, fmt, or clippy acceptance. `pre-commit` intentionally does not block partial local safety commits.

The hook's shellcheck decision uses the net pushed-tree change in `*.sh`, `*shellcheckrc`, and `docker/Dockerfile.build` (G158). New branches, missing remote objects, or failed comparison run shellcheck conservatively. A pushed range with no shell-input change does not check an unrelated uncommitted broken shell file.

G159 host self-tests skip only when this worktree's `.git/fulcrum-host-selftest.ok` matches script contents (`docker-run.sh`, `tests/lib/*.sh`) and host-tool identity (Bash/MSYS/Git for Windows). Missing/stale/unreadable fingerprints run them. A passing real run alone writes the record; an attempted new run invalidates the previous record first. The full gate ignores the shortcut and reruns/refreshes all host self-tests. Fingerprint omissions and a broken always-match implementation remain limitations checked by the full path.

The hook explicitly passes `COMPILE_SHELLCHECK=<decision> HOST_SELFTESTS=auto VOL_REPORT=0`; inherited environment values cannot override those assignments. The full gate does not set them. Values other than `auto` do not unconditionally skip host self-tests. The hook measures the **working tree**, not a reconstructed pushed commit, and says so on a dirty tree. Deletion-only pushes skip compilation.

Fresh clones do not inherit `.git/hooks`; install the hook once:

```bash
printf '#!/usr/bin/env bash\nexec bash "$(git rev-parse --show-toplevel)/tests/ci/pre-push.sh" "$@"\n' > .git/hooks/pre-push && chmod +x .git/hooks/pre-push
```

There is no custom bypass variable. An explicit Git bypass remains an external-operation decision, never ordinary successful validation. Built-in `--self-check` exercises hook decisions with local fixtures; representative faulty-input tests must show both blocking and permitted cases.

## Scenario criteria

The scripts are authoritative. A new/modified criterion needs a deterministic defect probe, not only another green run.

| Scenario and entry | Required distinguishing evidence |
|---|---|
| Lint: `tests/ci/shellcheck-all.sh` and container commands | Root `cargo fmt --all -- --check`, separate fmt for `spikes/musl-boringssl` and `vendor/pingora`, plus `cargo clippy --workspace --all-targets -- -D warnings` and derived shellcheck. |
| Own tests: `tests/unit/run.sh` | `cargo test --workspace --locked --no-fail-fast`; count must meet `MIN_TESTS` (source default 60), and each intended test binary must execute. A failing test and an unreachable high lower bound both fail. |
| Vendor: `tests/vendor/run.sh` | Match the official **0.9.0** failure set in both directions; `EXPECTED_FAILURES` is currently empty. Separately require execution of host-dependent `connectors::l4::tests::test_bind_to_port_range_on_connect`, whether it passes or fails. Do not add unexplained intermittent failures as permanent exemptions. |
| Data plane: `tests/serve/run.sh` | Real routing/forwarding, rewrite, upstream headers, redirects, unknown-site 421, 501/502 boundaries, keep-alive, thread roles, header casing, and original request-header error substitutions. A roughly 4 KiB response uses one data segment for the buffering criterion (G155). |
| L4: `tests/l4/run.sh` | TCP/UDP passthrough; SNI/ALPN splitting without TLS termination and with exact byte replay; receiving/sending PROXY protocol; long connections survive listener handover. |
| Files: `tests/files/run.sh` | Indexes, trailing-slash 301, browse, ETag/304, single ranges, MIME, and **path traversal/hide** rejection. Serving normal files cannot prove absence of leaked files. |
| Memory cache: `tests/cache/run.sh` | RFC 9111 hits/origin, no-store/private/Set-Cookie exclusions, fallback TTL, coexisting Vary variants, request cache-control, purge/coalescing, and no sharing of unauthorized Authorization responses. |
| Disk cache: `tests/cache/disk.sh` | Persistence after process kill/restart, with a memory-only negative control in the same run; no startup scan, graceful index save, metadata-only revalidation, cold-index purge, bad-entry discard/delete, and forwarding still works when cache directories fail. |
| Compression: `tests/encode/run.sh` | Decompressed bytes equal source on proxy/files; excluded responses remain plain; precompressed bytes retain strong ETag/range support; stale sidecars are absent; cache variants normalize equivalent preferences and never give compressed bytes to an incapable client. |
| TLS policy: `tests/tls-policy/run.sh` | Real groups/resumption behavior on h1/h2 and h3. First prove default instances negotiate X25519, issue tickets, and resume TLS1.2/1.3 and h3. Policy instances reject X25519-only clients, emit zero tickets, and perform new handshakes. `/load` changes to either global policy return 409 with old behavior, unchanged values return 200; generation upgrade activates the new values and startup logs declare policy. Every config disables automatic HTTP redirect to avoid implicit shared :80. |
| HTTP/3: `tests/h3/run.sh` | Independent curl OpenSSL-QUIC client, always `--http3-only`, plus an empty-UDP negative control. h3 works and shares h1 outcomes; hop-by-hop headers disappear only on h3; Alt-Svc covers six response paths and is absent on cleartext and h3 itself. A same-library client cannot establish interoperability. |
| HTTP PROXY: `tests/proxyproto/run.sh` | v1/v2 prefixes before TLS, coalesced first packet, LOCAL preserving peer, untrusted peers causing zero prefix reads, an unconfigured negative instance, and immediate policy activation. |
| Access log: `tests/log/run.sh` | Parse JSON fields, not grep formatting. Exactly one record/request; pre-rewrite URI, load-time log-open failure, closed outcome set, header allowlist, raw-byte absence of Authorization/Cookie through both entry paths, and actual TLS/ALPN observations including `--no-alpn`. |
| Metrics: `tests/metrics/run.sh` | Parse with a self-tested parser; compare scrape deltas and access-log counts, accounting for `Record::finish` after response completion. Exact family HELP/TYPE, content type, label-key sets, upstream families present without samples, access-control success/denial, and absent metrics on unconfigured sites. Unknown hosts must both add the precise counter and not grow series. Site label values form a closed set. h1/h2 TLS increments are separate and match logged version/cipher; h3 uses `<unknown>` cipher while omitting unavailable log fields, never empty labels. A reset after a slow no-Content-Length origin response cross-checks log status 0 and `status_class="none"`; Content-Length changes the observation. |
| Stats: `tests/stats/run.sh` | Optional fields and degenerate runtime states, including compile-time listener-set checks; a scrape alone is insufficient. |
| QUIC relay: `tests/quic-relay/run.sh` | Two generations via SIGQUIT/-u, different bodies identifying the responder, empty-port failure, three requests on one pre-upgrade connection, and summed `num_connects=1`. Reconnecting clients must not fake survival. |
| ACME issue: `tests/acme/run.sh` | Real local pebble CA, HTTP-01 and wildcard DNS-01 issuance. |
| ACME renewal: `tests/acme/renew.sh` | About 1.5-day fixture lifetime and the running product's ARI inspection loop issuing renewal, with the TLS contract's distinguishing conditions. |
| Smoke: `tests/smoke/self-check.sh` | The real-domain smoke tool accepts a healthy fixture and rejects a dead port/self-signed target appropriately. |
| Stress: `tests/stress/run.sh` | Sustained zero errors, no fd/memory growth, and zero failures through full load mid-traffic. This makes no performance claim. |
| Benchmark: `tests/bench/run.sh` | Pinned versions match bench authority and generator identity agrees; all contenders really serve and write raw output; host qualification can refuse a result. Synthetic qualified inputs must produce both PASS and FAIL so an always-refuse evaluator cannot pass. This produces no performance numbers/M3 acceptance. |
| M0: `tests/m0/run.sh` | Native HTTP and raw TCP/UDP survive generations with zero interruptions, at least one success per traffic class, and explicit inherited TCP/UDP fd log evidence. |
| Unclaimed fd: `tests/m0/unclaimed.sh` | After 0.9.0, unclaimed fds close in generation two, refuse connections after the old generation exits, and do not reach generation three. Every service's `listen_addresses()` matters; any `None` disables cleanup process-wide. |
| M1 product: `tests/m1/product.sh` | Real `fulcrum serve`, not spike: Type=notify readiness, pid file, reload config/change-back/binary replacement, and drained shutdown. |
| M1 mechanism: `tests/m1/run.sh` | Spike proves descriptor/CLOEXEC and two reloads plus shutdown. `exit-type-main.sh` and `mainpid-handover.sh` are negative reproductions: green means the bad design was reproduced. |

Vendor connector tests require public-network access to `1.1.1.1:80/443` (`one.one.one.one`). A network prerequisite failure is not a demonstrated fork regression. Fix intermittent short reads with framed/read-exact semantics; a valid split of 1 byte, 20 ms, then the remaining 8 bytes is a deterministic probe. Repeated absence of a flake is insufficient.

## Lint and shell inventory

Rustfmt/clippy and shellcheck are installed in the image, not fetched at test time. Workspace lint discovers new product crates; root fmt cannot reach independent workspaces. Dependency warnings are not equivalent to warnings in a crate directly linted by clippy's `-D warnings`.

Shellcheck discovers `.sh` under its configured roots. A **wider** `git ls-files --cached --others --exclude-standard` census rejects scripts outside those roots: tracked-only misses new files, while arbitrary filesystem scans wrongly include ignored handoffs. Only upstream `vendor/` is explicitly excluded. Do not add another silencing exclusion.

Enumerator self-tests cover deep paths, spaces, misleading directory names, unrelated files/backups, and empty discovery. Noninteractive Bash lacks globstar; `**` can silently become one level. A zero-input `xargs -r` pipeline can incorrectly report success. A new top-level shell root must fail the wider census until covered.

Shellcheck cannot detect every function mutation hidden behind `$(...)`; caller-side pid registration and post-cleanup port checks catch that class. Also run `tests/ci/dump-cache.sh --self-check` and `tests/acme/self-check.sh`: failure-only cache/forensics paths need explicit positive/negative fixtures, including owner-PID identification before/after closing a target socket.

## Containers and artifacts

[Dockerfile.build](../../docker/Dockerfile.build) creates `fulcrum-build:local`; the runner rebuilds when absent or when the Dockerfile hash changes. The pinned tag/digest and hash label `cool.cnb.fulcrum.dockerfile-sha256` both matter; an unreadable hash forces rebuild. cmake builds zlib-ng; clang serves bindgen/native dependencies; test networking may require the image's additional utilities.

Exact rustc/tag/digest authority is in Dockerfiles, with runtime tool versions printed from `/etc/fulcrum-toolchain`. OS package versions are recorded for visible drift rather than fully snapshot-pinned. G167's offline `python tools/dep-check.py --check-pins` derives `docker/Dockerfile.*` files with `^FROM rust:` and requires matching rustc versions. It does not compare OS/digests or recognize lowercase/`--platform` FROM forms. Update all recognized pins together only through approved maintenance.

Before a compiler upgrade, resolve the prospective digest with `docker manifest inspect`; then update exact tags/digests, run `--check-pins`, and the full gate. `dep-check.py` reports changed rustc/missing pin/unverified lookup as 40, refuses auto-apply, and treats Debian-only digest drift as informational. **M3 freezes the compiler (G36) and the whole benchmark image (G152).**

M1 uses [Dockerfile.systemd](../../docker/Dockerfile.systemd), systemd PID 1, `--privileged --cgroupns=private`, and cgroup v2. Do not bind the host `/sys/fs/cgroup` into that private namespace: mismatched trees break journald/MAINPID notifications. Its image major is separate G39 governance.

The musl product scenario builds the actual binary in Alpine and validates it in `FROM scratch`; `tests/musl/probe.sh` remains a feasibility spike, not a permanent product gate. The product build context is repository root and reads root `.dockerignore`; other Dockerfile contexts do not. BuildKit registry/target cache mounts make rebuilds practical, and target identity includes `$CRT_STATIC` so positive/negative RUSTFLAGS do not evict each other.

x86_64 is the default product build. G144 triggers aarch64 when the lock or relevant rustc Dockerfiles change; otherwise it checks [arm64-verified.txt](../../tests/musl/arm64-verified.txt) through [arm64-trigger.sh](../../tests/musl/arm64-trigger.sh). Missing binfmt when triggered fails rather than silently skipping. Product artifact checks do not replace glibc functional scenarios. CI named-volume cache does not preserve BuildKit cache mounts, so CI can pay cold musl-build cost.

## Cache identity, locks, and cleanup

`fulcrum-cargo` is shared registry data. Target volumes are `fulcrum-target-<12-char Dockerfile hash>-<12-char normalized-tree hash>`; [vol-lock.sh](../../tests/lib/vol-lock.sh) alone derives names for main/systemd/cache consumers. Cargo fingerprints do not cover all C-toolchain inputs. Sharing a target across images or checkouts can run stale test binaries and falsely omit new tests.

Acquire the per-volume atomic `mkdir` lock before use; refuse concurrent gates, name the PID, reclaim a dead holder explicitly, and release on exit. Locks live in `${TMPDIR:-/tmp}/fulcrum-gate-locks`, outside the checkout. `FULCRUM_GATE_LOCK_HELD` permits nested systemd invocation in the same process tree; standalone invocation obtains its own lock. Replacing atomic acquisition with a check-then-create introduces a race.

Current volumes label normalized owner paths using `cool.cnb.fulcrum.tree`; label creation is verified because recreating an existing Docker volume does not update labels. Live owners always preserve volumes. Gone owners can be offered removal; current-shape unlabeled/unknown volumes stay conservative. A name today's derivation cannot produce can be treated as legacy only while shape self-verification succeeds. Old scripts on another checkout can still use old-name caches; removing an unused cache means recompilation, not source loss, and Docker rejects removal while mounted.

Report volume sizes only when there is something to report; unknown size is not zero. A separate unfiltered census reports unattributed volumes, excluding known jurisdiction, anchored other-project prefixes, and exactly 64-hex anonymous names. It **never offers deletion** for unknown owners. Prefix lists must identify owners and cannot swallow `fulcrum*`; an unrelated-looking experiment name still cannot be attributed automatically. These checks supplement rather than replace the current target-volume rules.

CI cache `tests/ci/cache.sh save|restore` streams tar/compression without multi-GB intermediates. Verify Dockerfile hash and exact target volume in `meta.txt` before restore, select decompressor by artifact magic, and check restored volume is nonempty. Mismatched metadata skips visibly; never restore another tree's newer mtimes. Cache hits skip export and do not refresh the stored artifact; a lock/key change refreshes it, and an explicit key-version change invalidates it.

Clean up on EXIT including failures: signal the newest active generation with SIGINT, and SIGKILL earlier already-draining generations once criteria finish. A draining generation no longer reads catchable signals; waiting on SIGINT cannot force it out. Spike timing can include CLOSE_TIMEOUT 5 + grace 30 + runtime drain up to 30 seconds; the product budget is documented separately in [deployment](deploy.md#shutdown-budget). Return every owned port, including synthesized :80, and wait for final exit before another scenario probes the same address.

After branch switches/rebases, prove fresh `Compiling <crate>` rather than trusting mtimes. The [host guide](host-and-gate-traps.md#gate-discipline) gives the container-only refresh and further failure modes.

## CI and completion claims

[gate.yml](../../.github/workflows/gate.yml) runs on main pushes and manual dispatch, invoking the same full runner. It pins Ubuntu 24.04 for cgroup v2, has no documentation path-ignore, cancels older same-ref runs, and uses a 120-minute safety timeout. Documentation can change compiled `include_str!` contracts. Prepare sufficient disk for images, registry, and release targets; cache does not remove those requirements.

Before delivery run the applicable full gate. New/changed criteria must detect representative defects on copies, then pass when restored. Report the exact command, scope, failures, skips, actual artifact/toolchain, and any external prerequisites not verified. Performance acceptance remains [PLAN §8](../../PLAN.md), not this development gate.
