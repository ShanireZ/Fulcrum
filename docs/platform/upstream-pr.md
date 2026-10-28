---
type: Platform Contract
title: Upstream Contribution
description: Reusable scope, evidence, attribution, and fork-maintenance rules.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:13:53Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../vendor/pingora/FORK.md
---

# Contributing upstream

[PLAN.md](../../PLAN.md) G32/G38/G40 and [FORK.md](../../vendor/pingora/FORK.md) own approved local scope. Draft history, source snapshots, receipts, and unpublished materials stay outside Git. This guide grants no issue/PR/comment/push authorization.

## Preparation

1. Inspect current upstream code, issues/PRs, and contribution rules. A delta may already be merged but unreleased.
2. Read contribution/CI requirements at the exact target revision. Old MSRV, matrices, signing automation, audit ignores, or review mechanics must be refreshed before a new submission.
3. Discuss a nontrivial fix or dependency-major migration in an issue first; reference it from the PR. Separate unrelated modernization from a security/behavior fix.
4. Cover upstream scope, not only Fulcrum's vendored/activated subset. Optional backends and additional workspace members still need review and gates.

External authorities: [contribution guide](https://github.com/cloudflare/pingora/blob/main/.github/CONTRIBUTING.md), [build workflow](https://github.com/cloudflare/pingora/blob/main/.github/workflows/build.yml), [code of conduct](https://github.com/cloudflare/.github/blob/26b37ca2ba7ab3d91050ead9f2c0e30674d3b91e/CODE_OF_CONDUCT.md).

## Evidence and content

- Reproduce on unmodified upstream and prove the patch changes that outcome. For intermittent I/O bugs, force legal fragmentation or another deterministic trigger rather than treating reruns as proof.
- Run required fmt, check, tests, doc tests, clippy, audit, and unused-dependency tools for the target's toolchains/backends. Rust remains container-only (G107); prepare services such as openresty when required.
- Separate MSRV resolution from locks rewritten by a newer compiler or `--ignore-rust-version`. A contaminated lock can manufacture an MSRV failure; resolve from the appropriate clean upstream state.
- Compare failure sets in **both directions** against unmodified upstream. Missing services or host prerequisites are limitations, not a claim all tests passed.
- Distinguish audit warnings from failing vulnerabilities. `cargo audit` can exit zero while reporting unsound/unmaintained warnings; absence from an ignore list does not prove CI fails.
- State which packages/call paths leave an affected version. Another optional dependency may retain the advisory in the lock; a targeted bump does not necessarily eliminate every occurrence.
- Explain intentionally excluded adjacent fixes and risk boundaries. An immediate-close descriptor path or narrow fork race differs from a retained fd leak; scope must reflect evidence.

Use short imperative commit titles with an optional meaningful subsystem prefix. Preserve authorship and approved `Signed-off-by` identity. Recheck current DCO/CLA requirements rather than inferring them from old files.

## Publication and maintenance

Every external publication needs explicit authorization for operation and identity. G40 permits an authorized delegate using the owner's logged-in identity; it is not standing permission for future drafts. Review concrete final issue/PR text and evidence before sending.

Do not assume direct merge or a promised review schedule. Some workflows replay accepted commits through a separate batch and close the original PR; inspect the target branch implementation, not only open/closed status.

Maintain the fork independently of acceptance timing. Remove deltas upstream has adopted on rebase, preserve local contracts, and rerun vendor/integration gates. Source visibility does not satisfy M4 release acceptance.

See [dependency policy](dependency-policy.md), [supply chain](supply-chain.md), and [contributor guide](../../AGENTS.md).
