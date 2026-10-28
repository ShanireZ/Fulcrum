---
type: verification
title: musl and BoringSSL static/runtime proof
description: Historical static-link experiments and the boundary between a probe and the product.
resource: ../../tests/musl/probe.sh
tags: [verification, technical-evidence]
status: stable
sources:
  - id: plan
    resource: ../../PLAN.md
    title: Product scope, decisions and milestone exits
---

# musl and BoringSSL static/runtime proof

On 2026-08-25 the probe produced x86_64 and emulated-aarch64 musl binaries with
no INTERP segment or NEEDED entries. Each ran in FROM scratch, completed a real
QUIC handshake and 1-RTT application-data exchange, and invoked
set_select_certificate_callback with readable ClientHello SNI. This supported
G103/G104's library choice; it did not by itself validate Fulcrum's product graph.

A 2026-09-24 amd64 rerun used quiche 0.30.0 and boring 5.2.0 and repeated those
checks plus the dynamic negative control. That rerun did not cover aarch64.
Historical aarch64 evidence used qemu for build and execution, not physical ARM hardware.

## Independent proof obligations

| Obligation | Why a separate check is required |
|---|---|
| Compile BoringSSL for musl | Establishes target build compatibility. |
| Fully static artifact | No interpreter/shared-library dependency; execute the binary alone in FROM scratch. |
| Handshake and application exchange | Link/start success does not prove key exchange, certificate and 1-RTT AEAD runtime paths. |
| Certificate callback invoked | A successful handshake can use the context certificate without entering the callback. |

Run the handshake on a spawned thread, matching the product's worker-thread
shape. The initial “musl threads default to 128 KB” rationale did not apply to
Rust std::thread, which supplies a stack size; actual glibc/musl observations were
about 2 MiB. Preserve the worker-thread test without repeating the false premise.

The negative control uses the same scratch Dockerfile and a dynamic artifact
(`-crt-static`); it fails because the interpreter is absent. `file(1)` describes
ELF properties correctly, while scratch execution answers the stronger question
of whether this artifact can run without additional files. Check both meanings.

## Host and target are different problems

The first Alpine build compiled BoringSSL, then bindgen failed with
“Dynamic loading not supported”. The build script itself was statically linked;
bindgen/clang-sys runtime mode needed dlopen(libclang). This was a build-host
problem, not a failure of BoringSSL on the musl target.

The successful two-stage method builds dependencies/build scripts dynamically,
then applies static linkage only to the final crate through cargo rustc's trailing
arguments. A global second-pass RUSTFLAGS would rebuild the scripts statically and
recreate the failure. These commands belong inside the pinned build container:

```bash
RUSTFLAGS="-C target-feature=-crt-static" cargo build --release --locked
cargo rustc --release --locked --bin <binary> -- -C target-feature=+crt-static
```

BoringSSL build prerequisites include cmake, C/C++ compilers, git and libclang.
A missing git can surface only as an unnamed file-not-found error. The tested
Debian musl-tools package provided C tooling, not musl-g++; Alpine native builds
supply the C++/libstdc++ side without adding a cross-compiler supply chain.
G131 selects Alpine-native builds with qemu for aarch64, accepting slower emulation.

Keep quiche, Pingora and product boring versions compatible and singular;
quiche 0.29 required boring 4, while 0.30 permits `>=4.19, <6`. Do not generalize
a dependency-range constraint into a permanent prohibition on another major.
Source patches/features must also match the artifact actually tested.

## Reproduce and limits

```bash
bash tests/musl/probe.sh
ARCHES="amd64" bash tests/musl/probe.sh
```

The probe remains outside the full product gate as historical/library evidence.
It has a standalone Cargo workspace/lock outside root supply and workspace clippy
coverage. Formatting explicitly includes it; clippy is not silently claimed.
It cannot prove a single TLS implementation in Fulcrum, physical aarch64 behavior,
release-pipeline readiness or current product static linkage.

[tests/musl/product.sh](../../tests/musl/product.sh) separately builds and executes
the actual product in scratch (G108). The first x86_64 product check on 2026-08-26
validated no INTERP/NEEDED, configuration validation, a dynamic negative control
and a specifically rejected invalid configuration. Later product coverage and
architecture triggers belong to the current gate, not these historical readings.
G138's product symbol-table check establishes linked TLS implementations;
root dependency graphs and strings searches answer different questions.
[Build and test](../platform/build-and-test.md) · [TLS](../architecture/tls.md)
