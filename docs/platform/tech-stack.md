---
type: Platform Contract
title: Technology Stack
description: Current language, protocol, TLS, configuration, and distribution boundaries.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:13:04Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../Cargo.toml
  - resource: ../../vendor/pingora/FORK.md
---

# Technology stack

[PLAN.md](../../PLAN.md) §5 and its G-decisions are authoritative. This page maps those decisions to the repository; dependency versions come from manifests and lockfiles, not a manually maintained latest-version table.

| Area | Current contract |
|---|---|
| Language | Rust, edition 2024, Cargo resolver 3. Memory safety is a design property; language choice alone establishes no performance claim. |
| HTTP foundation | Vendored Pingora 0.9.0. Use `HttpServerApp` / `ServerSession`; `pingora-proxy` is not a dependency. Fulcrum owns routing, balancing, health checks, cache semantics, and protocol integration. |
| HTTP/3 | `quiche::h3` supplies semantics and quiche supplies QUIC. Fulcrum owns the sans-IO event loop, connection ownership, UDP listeners, cross-generation relay, and shared HTTP execution chain (G103/G105/G109). |
| TLS | BoringSSL for incoming h1/h2, h3, L4 ClientHello inspection, and outgoing HTTPS (G104). Both HTTP entrypoints use one `set_select_certificate_callback`. |
| Configuration | Human DSL compiles to JSON, the sole internal configuration authority. No JSON-to-DSL round trip. |
| Distribution | Linux x86_64/aarch64 static musl binary, systemd, deb/rpm (G13). No official runtime container image; a `FROM scratch` example suffices. |
| Build | Docker-only Rust/toolchain execution (G107). Debian-based development image; Alpine-native product musl build with qemu for aarch64 (G131). |

## Repository and type identity

The six product crates are `fulcrum-config`, `fulcrum-runtime`, `fulcrum-tls`, `fulcrum-acme`, `fulcrum-server`, and CLI crate `fulcrum`. `spikes/m0-seam` and `spikes/m1-systemd` validate mechanisms; they are not release products.

Root `[patch.crates-io]` routes `pingora-core`, `pingora-http`, and `pingora-boringssl` to [vendor/pingora](../../vendor/pingora/FORK.md), its own Cargo workspace. Inspect this fork for actual Pingora behavior. Using a crates.io copy beside its path-dependent equivalent can create distinct same-named Rust types; patch only crates actually used by the graph.

Quiche explicitly disables defaults and enables `boringssl-boring-crate`. It and `pingora-boringssl` must resolve one compatible `boring` package so `SslContextBuilder` is the same type. Direct `boring` is G111's narrow exception for AES-256-GCM Retry-token address validation through `symm`; authentication/tag comparison remains in BoringSSL, without hand-written cryptographic FFI.

Optional TLS packages can remain in locks without activation. Separate manifest/feature, resolved-graph, and linked-symbol evidence; see [supply chain](supply-chain.md).

## Integration constraints

- One certificate-selection implementation serves h1/h2 and h3. A separate rustls path changes G104 and needs a new decision.
- Tower `Service` / `Layer` middleware does not compose with Pingora HTTP phases. The ACME connector's implementation of a Tower trait does not change that boundary.
- Every custom TCP/UDP service explicitly obtains/returns listener fds through Pingora's transfer table. M0 validates the seam, not automatic inheritance for arbitrary services.
- Musl/BoringSSL feasibility depends on the build host: bindgen needs dynamic `dlopen`, whereas Alpine build scripts default to static linking. Follow Alpine-native compilation rather than adding a C/C++ cross-toolchain.

See [HTTP/3](http3-libraries.md), [build and test](build-and-test.md), and [security](security-baseline.md).
