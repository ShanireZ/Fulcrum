---
type: verification
title: Historical rustls feature seam
description: Optional-feature audit boundaries, provider defaults and timeout preconditions.
resource: ../../vendor/pingora/pingora-rustls/Cargo.toml
tags: [verification, technical-evidence]
status: stable
sources:
  - id: plan
    resource: ../../PLAN.md
    title: Product scope, decisions and milestone exits
---

# Historical rustls feature seam

The 2026-08-14/19 investigation concerned the former rustls product backend.
G104 later replaced that backend with BoringSSL; pingora-rustls remains in the
vendor workspace/regression coverage. This page preserves technical lessons,
not an instruction to restore rustls to the product.

## Coverage must match the question

The initial root lock audit, vendor tests without features, and root supply audit
all omitted the optional rustls path. Enabling it changed the normal graph from
133 to 175 crates: 42 packages had been outside those checks. A product decision
naming a feature does not activate it in Cargo. Vendor regression now explicitly
exercises pingora-core/rustls; audit normal and dev edges separately.

Selecting `features = ["ring"]` adds ring to defaults rather than replacing them.
rustls and tokio-rustls each admitted aws-lc through defaults, while runtime code
installed ring. Disable defaults at both edges and retain logging/std/tls12 as
applicable. prefer-post-quantum belonged to aws-lc and was inactive under ring.
The unconditional core dev-dependency was a third entry point invisible to
`cargo tree -e normal`; type-only dev usage must not select a separate provider.
These fixes were absorbed by 0.9.0 (`5396a1f`,
[PR #966](https://github.com/cloudflare/pingora/pull/966)).

Feature activation can change a test-failure set independently of a local patch.
Use stock and changed controls with the same feature selection and environment;
compare named failures rather than only totals. An environmental failure is an
unresolved investigation, not a reason to keep adding exemptions.

## Timeout tests require a real black hole

The historical Docker network answered 192.0.2.1:79, despite its TEST-NET status.
Tests expecting ConnectTimedout instead succeeded or got a different error.
Dropping SYN packets to 192.0.2.0/24 in the test container corrected the premise;
a blackhole route or REJECT fails immediately with ConnectError and is not equivalent.

The vendor gate needs NET_ADMIN and verifies actual timeout behavior before tests.
It must distinguish reachable, immediately rejected and silently dropped traffic;
listing an iptables rule alone cannot establish that the rule takes effect.
This fixed environment reduced rather than expanded the old expected-failure list.
The current baseline's exact list belongs to tests/vendor/run.sh.

## Preserve native-certificate partial success

rustls-native-certs 0.8 exposes CertificateResult { certs, errors }, while the old
API returned an error only if it had no certificates to return. Retain
`certs.is_empty() && !errors.is_empty()` as the failure condition and log errors
otherwise. Failing on any error would make one unreadable certificate reject an
otherwise usable store. The migration and PEM characterization tests remain in
[fork changes 5/6](../../vendor/pingora/FORK.md).

Reusable conclusions: audit every optional/default/dev entry, prove test
environment premises behaviorally, preserve old API semantics before adapting
new result shapes, and compare every vendored crate plus workspace metadata.
[TLS baseline](../architecture/tls.md) · [Supply chain](../platform/supply-chain.md)
