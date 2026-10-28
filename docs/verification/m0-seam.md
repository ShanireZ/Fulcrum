---
type: verification
title: M0 listener-transfer seam
description: Historical listener-transfer findings, reproducible checks and scope limits.
resource: ../../tests/m0/run.sh
tags: [verification, technical-evidence]
status: stable
sources:
  - id: plan
    resource: ../../PLAN.md
    title: Product scope, decisions and milestone exits
---

# M0 listener-transfer seam

The 2026-08-12 experiment on Pingora 0.8.1 demonstrated that custom raw TCP and
UDP listeners can participate in the same SCM_RIGHTS transfer table as native
HTTP. Repeated runs, including after the dependency fork, observed no HTTP
failures, TCP disconnections or UDP echo loss across SIGQUIT plus a new `-u`
generation. This is historical seam evidence, not certification of later binaries.

## Scope and implementation

The original experiment used daemon mode, a two-thread work-stealing runtime,
and one listener task per fd. It did not test systemd foreground operation or
QUIC connection ownership. [M1](m1-systemd.md) separately answers the process-model
question; later QUIC relay tests answer connection ownership.

| Service | Port | Transfer responsibility |
|---|---|---|
| Native HTTP | TCP 8080 | Pingora listening Service. |
| Raw TCP | TCP 8081 | Custom Service obtains/registers its own listener fd. |
| Raw UDP | UDP 8082 | Custom Service obtains/registers its own listener fd. |

Service receives an fd table, claims its required keys, or creates and registers
missing sockets for the next generation. The table maps string keys to RawFd;
it does not depend on the socket type. Custom key prefixes avoid native listener
key collisions. The code is in [spikes/m0-seam](../../spikes/m0-seam/src/main.rs).

SCM_RIGHTS creates descriptors in the receiver's table; their numeric values are
not stable across generations or runs and must not be asserted. Since 0.9.0,
custom listen_addresses() implementations must return the actual fd-table keys,
not simply bind addresses, to preserve unclaimed-fd cleanup.

## Checks and interpretation

The shared [probe](../../spikes/m0-seam/src/bin/probe.rs) creates a new connection
for each HTTP request, carries one TCP connection across the upgrade, and sends
UDP echoes on uncongested loopback. Check process-generation changes and actual
fd inheritance alongside traffic results. An initial “No incoming socket transfer”
log can be a normal retry; later inheritance and exit status determine success.

The initial bind was 0.0.0.0; the reproducible test later defaulted to 127.0.0.1
with M0_BIND_HOST override to reduce the echo-reflection surface. Historical
outputs must not be rewritten to make them match the later default.

UDP echoes do not prove QUIC continuity: while both generations hold the same
socket, recv_from distributes packets between them, whereas connection state
belongs to only one process. The experiment also established that custom services
can use Pingora's multithreaded current_handle runtime; it did not require a
single-thread service model. G35 remains the threading authority.

## Reproduce

```bash
bash tests/m0/docker-run.sh
```

Rust runs inside Docker; caches use named volumes and test outputs remain in
ignored run/m0/. [Build and test](../platform/build-and-test.md) defines the full
gate and prerequisites. A spike establishes that an integration is possible;
product wiring needs its own tests.
