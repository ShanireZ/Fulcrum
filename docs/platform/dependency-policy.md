---
type: Platform Contract
title: Dependency Policy
description: G29 adoption, quarantine, dual-lock verification, monitoring, and compiler freeze.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:13:31Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../tools/dep-check.py
  - resource: ../../tools/supply-audit.py
---

# Dependency policy (G29)

[PLAN.md](../../PLAN.md) owns this policy and approved exceptions:

1. Direct requirements use `>=` lower bounds without upper bounds.
2. Commit `Cargo.lock` for reproducible resolution.
3. Check/follow updates weekly.
4. Adopt only releases at least 24 hours old and not yanked.
5. Run complete applicable milestone verification; M3 adds qualified comparisons where required.

Unbounded requirements intentionally admit breaking major versions. A weekly update may require a real migration. Quarantine mitigates the interval between a malicious publication and discovery/yank; manifests cannot enforce publication age. Do not use bare `cargo update` outside the age-checked workflow.

## Entry points

```bash
python tools/dep-check.py
python tools/dep-check.py --apply
python tools/dep-check.py --hours 48
python tools/dep-check.py --check-pins
python tools/dep-check.py --self-check
```

Default cargo execution uses `fulcrum-build:local`, the gate's registry volume, and anonymous `/w/target` (G107). Prepare the image through the documented gate if absent. `--cargo` uses its supplied prefix exactly; host `--cargo cargo` violates the container-only contract and can leave `target/.rustc_info.json` in the checkout.

`--apply` intentionally adopts packages. It does not authorize compiler changes, fork rebase, upstream submission, or publication.

## Additive exit codes

| Code | Meaning |
|---|---|
| `0` | No reported action. |
| `10` | Eligible cargo updates; in apply mode, completed adoption. |
| `20` | Upstream Pingora release needs a manual fork rebase. |
| `40` | Build image: newer rustc, missing digest pin, or unverified check. |
| `80` | Systemd test image: changed major or unverified check. |
| `160` | Unregistered advisory or incomplete advisory check. |
| `1` | Error, including failed/rolled-back adoption. |

Flags add: cargo updates plus fork drift report `30`. Interpret the categories rather than treating every nonzero as a source-adoption failure. Advisory scanning precedes adoption; rescan both resulting locks for final-state evidence.

## Candidate and adoption algorithm

1. Run `cargo update --dry-run` separately for root and `vendor/pingora/Cargo.toml`. Parse crates.io `Updating`/`Adding` lines including trailing available-version, MSRV, and publication-age annotations. `Downgrading`, `Removing`, git, and path sources are not adoption candidates. `--self-check` pins actual output forms.
2. Root candidates own versions in the root lock. Vendor-only candidates have old `name@version` absent from root, or the new version for additions. Both sets use the same age/yank criteria and eligible changes report `10`.
3. Query each candidate's crates.io publication time and yank status. Reject young, yanked, or unverifiable versions with a reason.
4. Snapshot both locks byte for byte. Apply eligible root candidates using `cargo update -p <name> --precise <version>`, then vendor-only candidates against the vendor manifest. Judge the **resulting locks**: target present, old version absent, shared-package alignment satisfied. A sibling may already have updated a package, making the later command nonzero despite a correct lock.
5. Exact-pinned families such as `wasm-bindgen` plus `js-sys`/`web-sys` may need one combined `cargo update -p a@old -p b@old ...` without `--precise`. Recheck final target versions; a release newer than the candidate collection is not automatically approved.
6. Compare resulting locks with snapshots. Every newly introduced non-target version independently passes the same current age/yank/verification check, including transitives pulled by `--precise`. Any failure restores **both** byte snapshots, rejects the entire batch, names the version/introducing operation, and returns `1`. Older eligible incidental versions are reported without failure.
7. Verify vendor/root agreement for packages actually used by Pingora. A root-only package no Pingora crate consumes need not be fabricated into the vendor lock; inspect the inverse graph.

## Separate monitoring surfaces

- **Advisories:** vendor lock first, root second; import the single `ACCEPTED` registry from `supply-audit.py`. Lock-only packages still receive review. Duplicated exemptions create conflicting authorities.
- **Fork releases:** `[patch.crates-io]` removes ordinary registry discovery for the fork. Read baseline directly from vendored `pingora-core/Cargo.toml`, check upstream separately, apply quarantine, and refuse automatic rebase. `--skip-fork` explicitly disables this branch.
- **Unreleased upstream:** report main's lead over its tag and likely dependency commits. This is advisory, does not affect the exit code, and is not mandatory adoption.
- **Build compiler (G36):** read Dockerfile `FROM`, compare rustc with official image HTTP metadata. Debian-package digest drift is informational because this is not a runtime image. Missing `@sha256`, changed rustc, or inability to verify reports `40`; never auto-apply. `--skip-image` explicitly disables this branch.
- **Systemd host (G39):** the major version is part of M1 evidence. If MainPID-handover's negative scenario changes behavior, revisit G37 rather than editing an assertion to pass.

G36 freezes the compiler during M3; G152 freezes benchmark competitors/generator/image. G167 requires matching rustc pins across the derived Dockerfile set. See [build and test](build-and-test.md).

## Limits and follow-up

`cargo update` moves within all upstream requirements. Caret/tilde upper bounds can block newer packages despite unbounded local requirements. No candidates does not mean every dependency is latest or safe. [Supply-chain audit](supply-chain.md) separates lock, compiled graph, call-site activation, linkage, and full staleness/advisory review.

After adoption run `bash tests/m0/docker-run.sh` and required qualified comparisons. Before rebase inspect which deltas upstream already implemented; afterwards run vendor and integration gates. See [fork](../../vendor/pingora/FORK.md) and [contributor guide](../../AGENTS.md).
