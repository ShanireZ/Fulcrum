---
type: Platform Contract
title: HTTP/3 Integration
description: Quiche/BoringSSL boundaries and the connection-ownership requirements that library choice does not remove.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:13:53Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../Cargo.toml
  - resource: ../../crates/fulcrum-server/src/quic/mod.rs
  - resource: ../../crates/fulcrum/tests/supply_gates.rs
---

# HTTP/3 integration

G103–G105 select quiche, one BoringSSL stack, and library-provided `quiche::h3` semantics. D11 is closed; this page does not reopen selection. The immutable 2026-08-25 comparison and later corrections are archived byte for byte outside Git. Its candidate versions, activity/download counts, and old API signatures are historical observations, not current guarantees.

## Layer boundaries

QUIC transport and HTTP/3 semantics are separate layers. Quinn plus `h3` uses the `h3-quinn` adapter; quiche provides transport and `quiche::h3` in one crate. The studied s2n-quic route supplied QUIC and still required HTTP/3/backend code. Comparing `h3` alone with a combined library confuses layers; release inactivity and repository activity also answer different questions.

Fulcrum owns I/O, timers, connection ownership, routing, and integration. Libraries own QUIC, TLS, HTTP/3 framing and QPACK. Library selection never authorizes writing these protocol stacks by hand.

## TLS and provider identity

Quiche uses BoringSSL rather than the former rustls resolver interface. Keeping rustls for h1/h2 would create two TLS implementations and certificate paths. G104 uses the same BoringSSL `set_select_certificate_callback` for both entrypoints.

The manifest sets `default-features = false` plus `boringssl-boring-crate`. Pingora, quiche, and the approved direct `boring` use must resolve one compatible package. Do not introduce providers through defaults. Supply gates check disabled-default declarations, forbidden `aws-lc*` names in both locks, graph/provider identity, and product symbols. Extend a hand-maintained provider list when adopting a new provider-bearing dependency; an old clean list says nothing about an omitted new package.

## Cross-generation ownership

Two upgrading processes may hold one UDP socket while each QUIC connection belongs to only one generation's memory. A datagram reaching the wrong process cannot be handled by substituting another session. G109 supplies DCID-based cross-process relay; descriptor handover alone does not preserve QUIC connections.

Quiche is sans-IO. Public header parsing exposes DCID/SCID before a connection exists, the caller supplies server CID bytes to `accept`, and the application retains the datagram to choose its generation before invoking the library.

The historical quinn-proto source study found that an unknown apparently valid connection could receive a stateless reset. Generation-specific hashed CID validation could reduce that to dropping, but **dropping is not relaying**. Forwarding still belongs before endpoint processing, for example through a controlled `AsyncUdpSocket::poll_recv`, plus a cross-process transport. The studied s2n validator took raw bytes and returned CID length; its unknown-CID failure behavior was not established. A signature alone does not establish drop/reset/recovery behavior. Re-read the exact pinned implementation before using an alternative.

## Listeners and builds

Custom UDP listeners obtain/return fds through Pingora's transfer table. Every studied route accepted pre-bound sockets, so that did not distinguish selection. Every service declares addresses for unclaimed-fd cleanup; one service returning `None` affects cleanup process-wide.

BoringSSL's C dependency must pass product musl and TLS-linkage gates. A spike establishes feasibility, not distribution. Bindgen needs dynamic `dlopen`, making the Alpine build host part of the contract. G131 selects Alpine-native compilation plus qemu for aarch64, avoiding an extra C/C++ cross-toolchain.

## Verification

- Use curl's independent OpenSSL-QUIC client with `--http3-only`; `--http3` may fall back.
- First prove the client fails against an empty UDP port.
- Compare h1/h3 through the same execution chain, hop-by-hop headers, and Alt-Svc positive/negative paths.
- Upgrade tests need two generations, distinguishable bodies, continued requests on one connection, and summed `num_connects` equal to one.
- Keep provider/linkage, real TLS-policy handshakes, listener transfer, relay, and product-artifact evidence distinct.

See [build and test](build-and-test.md), [process model](../architecture/process-model.md), and [TLS](../architecture/tls.md).
