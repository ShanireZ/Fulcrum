---
type: Platform Contract
title: Security Baseline
description: Management admission, secrets, privileges, protocol libraries, and supply-chain responsibilities.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:13:04Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../crates/fulcrum-server/src/admin.rs
  - resource: ../../tests/m0/run.sh
---

# Security baseline

This technical baseline yields to [PLAN.md](../../PLAN.md). Requirements apply to design and implementation; their presence here does not claim every deployment or future capability has passed acceptance.

1. Management defaults to a filesystem-controlled Unix socket (G14). Remote management requires an explicit mTLS design; current implementation rejects network admin addresses. See [control plane](../architecture/control-plane.md).
2. On-Demand TLS requires an ask endpoint or domain allowlist at startup (G15). Unconfigured dangerous capabilities fail closed. See [TLS](../architecture/tls.md) for implemented versus unwired behavior.
3. Private keys, ACME credentials, and upstream credentials stay out of ordinary logs. Redact configuration previews.
4. Minimize file/process privileges. Use the necessary bind capability rather than long-lived root; [deployment](deploy.md) assigns privilege management to systemd.
5. Mature libraries implement TLS, HTTP/2 state machines, HPACK, QPACK, and QUIC. Do not replace them with hand-written protocol or cryptographic implementations.
6. Audit configuration changes with identity, version, time, and outcome.
7. Do not statically link fallback competitors. Fallback is removed; its [boundary lessons](../architecture/fallback.md) still inform integration/licensing review.

Rust memory safety and audited libraries support the network-path safety argument. Neither replaces authorization, secret handling, review, or negative tests.

## Issuance and observability

On-Demand admission also needs issuance-rate bounds, failure backoff, and alerts. Prometheus domain labels can disclose tenant/domain information; decide deliberately what is safe to expose. Certificate/key parser migrations, storage permissions, and privilege changes are security-sensitive even with a small manifest diff.

## Test-fixture protections and limits

M0 is not a public service. Its UDP echo defaults to `127.0.0.1`; exposure requires `M0_BIND_HOST=0.0.0.0`. A spoofable UDP echo is a reflection surface even at 1:1 amplification. Product UDP needs source/rate controls rather than relying on a container's unpublished ports.

TCP echo has a 30-second idle timeout. HTTP header reads have a 15-second total budget, not a per-read budget a slow sender can extend forever. EMFILE/ENFILE accept errors back off one second rather than exhausting CPU/log storage during descriptor scarcity.

The Linux override `DOCKER_USER="$(id -u):$(id -g)"` is available but is not a universally verified default. Host UID mappings can make named volumes unwritable on Windows. Linux root-owned checkout outputs and UID/volume behavior need actual host evidence.

## Supply chain

[Dependency policy](dependency-policy.md) governs adoption/quarantine; [supply chain](supply-chain.md) separates advisory, activation, and linkage evidence. Old clean snapshots do not establish current safety. Accepted advisories need a reason and exit path in the executable registry; remove stale exemptions.
