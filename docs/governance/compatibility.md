---
type: contract
title: Compatibility
description: Public change surfaces, build identity and release transition policy.
---
# Compatibility

[PLAN G141/G149](../../PLAN.md) own this policy. Classification applies now; compatibility promises start at M4/1.0. Until then product versions remain 0.x and breaking changes are possible. Internal Rust APIs are not public surfaces: all product crates use publish=false. Spike crate versions are not release versions.

| Surface | Breaking change |
|---|---|
| DSL | Previously accepted input fails or changes behavior. Accepted input with different behavior can be more costly than explicit validation failure. |
| Structured JSON/load API | Previously accepted payload is rejected. |
| /stats JSON | Remove a field or change its type/meaning; additive fields are compatible. |
| Metrics | Rename/remove families or labels, or narrow label values. Adding families/samples is compatible; removals can silently disable alerts. |
| FUL-DSL-NNNN diagnostics | Once issued, never change a code's meaning. |

After promises begin, metrics/stats transitions require at least six months and one minor release; DSL/JSON requires at least one minor with clear deprecation warnings. Reassess durations before M4 if release cadence differs from expectations. Diagnostics never deprecate their established meaning.

`fulcrum_build_info{version}` identifies a build, not its compatibility promise. Build metadata follows FULCRUM_BUILD_ID, direct .git inspection, then explicit `<package-version>+unknown`. The git fallback does not detect dirty worktrees; packaging must supply an explicit build identity. Semver build metadata does not affect version precedence. Fork maintenance follows [FORK.md](../../vendor/pingora/FORK.md), not this public-interface policy.
