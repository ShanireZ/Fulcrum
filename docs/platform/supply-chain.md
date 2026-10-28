---
type: Platform Contract
title: Supply-Chain Verification
description: Separate resolution, activation, reachability, linkage, advisories, and adoption evidence.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:13:31Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../tools/supply-audit.py
  - resource: ../../tools/dep-check.py
  - resource: ../../crates/fulcrum/tests/supply_gates.rs
  - resource: ../../tests/ci/tls-linkage.sh
---

# Supply-chain verification

[Dependency policy](dependency-policy.md) owns G29 adoption. Current versions, accepted advisories, and fork deltas come from lockfiles, the executable registry, and [FORK.md](../../vendor/pingora/FORK.md). Historical counts, audit snapshots, and adoption receipts are archived outside Git; this page makes no current online-audit claim.

## Evidence layers

| Question | Evidence |
|---|---|
| What is resolved? | Root and `vendor/pingora/Cargo.lock`. |
| What is activated? | Container `cargo tree -e all --workspace --target all --locked`, narrowed to the claimed target/features. |
| Does the relevant call site compile/execute? | `cfg`, feature selection, entrypoints, actual instantiated types. |
| What TLS implementation is linked? | `tests/ci/tls-linkage.sh` symbol evidence judged by `tests/musl/product.sh` (G138). |
| Can it be adopted? | All version requirements, quarantine, and final-lock verification through `dep-check.py`. |

A lock may omit disabled optional dependencies and retain packages outside the activated graph. Weak edges such as `hyper-rustls?/ring` can affect lock resolution without activating that runtime path. A compiled crate may have its sole relevant call site gated out. These layers cannot substitute for one another.

The product gate requires BoringSSL-specific symbols and rejects OpenSSL/rustls implementation symbols. Pure `rustls_pki_types` is permitted; its appearance in `strings` does not prove another TLS stack. This covers the musl product artifact, not the glibc development binary.

## Upper bounds and buckets

Cargo interprets `"3"` as `^3`, meaning `>=3.0.0, <4.0.0`; `~0.24.3` also caps the minor line. All consuming requirements must hold. A local unbounded requirement cannot override a transitive parent's upper bound.

`[patch.crates-io]` changes source, not consuming version requirements. Patching a leaf to 8 cannot satisfy its parent's `^3`; G30 changes parent Pingora manifests in the fork. Do not recursively fork unrelated packages merely to improve version counts without a security/product need.

Separate stale packages compiled for the actual target, resolved only for other targets/features, and blocked upstream. Count Windows-only resolution honestly for Linux x86_64/aarch64 distribution. A parent can be current while excluding a newer transitive major.

## Audit commands

```bash
python tools/supply-audit.py
python tools/supply-audit.py --lock vendor/pingora/Cargo.lock
python tools/supply-audit.py --cargo "<container cargo prefix>"
python tools/supply-audit.py --cargo "<container cargo prefix>" --markdown
```

Without `--cargo`, evidence is lock-level only. With it the tool separates compiled/lock-only packages using a target-specific normal-dependency graph; default target is `x86_64-unknown-linux-gnu`.

The prefix must run `docker run ... fulcrum-build:local cargo`, bind the intended checkout at `/w`, use working directory `/w`, and mount a volume at `/w/target`. Reuse registry volume `fulcrum-cargo`. For ad-hoc vendor commands use `--target-dir /w/target/<name>`, not a mount at `/w/vendor/pingora/target` that leaves a host directory later receiving `.rustc_info.json`. Git Bash requires `MSYS_NO_PATHCONV=1` and Windows-form absolute bind sources. See [host traps](host-and-gate-traps.md).

The tool compares every registry package with crates.io `max_stable_version` and queries OSV for the whole selected lock. Weekly `dep-check.py` scans advisories in both locks using the same `ACCEPTED` registry; the deep audit adds staleness and buckets.

| Exit | Meaning |
|---|---|
| `0` | No unregistered advisory and adequate verification coverage. |
| `20` | Unregistered advisory. |
| `40` | Unresolved coverage exceeds `--max-unresolved-pct`, default **10%**, on either version or advisory side. |
| `60` | Both conditions. |
| `1` | Error. |

Staleness alone does not fail. Accepted advisories stay visible without failing; each needs an acceptance reason and exit path. Remove dead entries. Missing/short OSV responses are unverified, not clean; incomplete advisory scans must not print an unqualified no-advisories claim. Proportional thresholds tolerate limited rate limiting while refusing to call an unreachable registry successful.

## Reachability and fixes

Advisory existence, reachable trigger, available fix, and implemented remediation are separate. Recheck reachability after dependency changes. An integer key cannot trigger a custom-key `Drop` panic in an old LRU `pop()` issue; encoding Prometheus protobuf differs from parsing attacker input. Those illustrate scoped reasoning, not permanent exemptions or current vulnerability readings.

Prefer an eligible verified upgrade/removal when a safe fix exists. Certificate/key parser migrations need characterization and negative tests. Keep vendor controls when codec upgrades move malformed-input rejection.

Dependency bumps are not automatically behavior-free. Current fork deltas include the Tokio zero-capacity-buffer compatibility fix and safe YAML-parser replacement; [FORK.md](../../vendor/pingora/FORK.md) owns exact implementation/limits.

## Licensing

Review changed licenses during adoption. Distribution retains notices, copyright, and disclaimers, including BSD-3-Clause obligations for the zstd family and `encoding_rs`. Permissive licensing does not remove notice duties. Preserve immutable upstream license text.
