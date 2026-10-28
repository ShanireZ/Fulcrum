---
type: verification
title: Performance acceptance (G19)
description: Reproducible category comparisons and the limits of benchmark evidence.
resource: ../../bench/README.md
tags: [verification, technical-evidence]
status: stable
sources:
  - id: plan
    resource: ../../PLAN.md
    title: Product scope, decisions and milestone exits
---

# Performance acceptance (G19)

This page describes the acceptance contract, not current results or milestone
state. [PLAN §8](../../PLAN.md) is authoritative; [bench/](../../bench/README.md)
owns the executable methodology, host qualification, server alignment and
invalidation rules. Dated raw snapshots remain in bench/results/.

## Acceptance contract

| Dimension | Requirement |
|---|---|
| Competitors | Caddy, HAProxy and nginx, with any missing-competitor treatment explicitly decided before a category. |
| Method | Same machine, cases and load generator. |
| Threshold | Fulcrum meets at least 90% of the strongest applicable competitor in each category, including equality. |
| Publication | Publish reproducible scripts, raw data and qualifications, including unfavorable results. |

<a id="用例按類劃分"></a>

## Categories

Static throughput, reverse-proxy throughput, TLS handshakes, L4 forwarding,
cache hits, long connections and high-concurrency short connections. Determine
the strongest separately per category; never average competitors or borrow a
winner from another category. These are seven independently falsifiable parts
of the product hypothesis, not one aggregate score.

## Evidence boundaries

The development benchmark gate exercises pipeline behavior and criterion
positive/negative controls; green does not mean acceptance. On an unqualified
host the verdict is UNQUALIFIED, without a strongest/threshold/PASS/FAIL result.
Raw output from that path is pipeline evidence only. Host qualification does
not by itself prove the load generator has spare capacity.

Script kernel parameters and CPU affinity, record complete environment snapshots,
retain comparison caveats, and apply the raw-data/invalidation rules in bench/.
Performance claims require reproducible end-to-end measurements; local diagnostics,
implementation language and a green development gate do not establish them.
Do not use unqualified “N times faster” claims.

<a id="與依賴升級的關係"></a>

## Dependency changes

G29 requires the current milestone's complete applicable verification for every
adoption. After M3 that includes full comparisons. G36 freezes build-compiler
changes during M3; G152 freezes the benchmark image and constrains missing Caddy
support in L4/cache categories. See the [dependency policy](../platform/dependency-policy.md).
