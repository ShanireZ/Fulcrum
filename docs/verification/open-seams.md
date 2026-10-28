---
type: verification
title: Integration seams and validation limits
description: Resolved integration findings and their continuing verification boundaries.
resource: ../../PLAN.md
tags: [verification, technical-evidence]
status: stable
sources:
  - id: plan
    resource: ../../PLAN.md
    title: Product scope, decisions and milestone exits
---

# Integration seams and validation limits

[PLAN](../../PLAN.md) owns outstanding work and open decisions. This page retains
the evidence boundaries behind resolved integration risks; a past spike or green
gate does not establish a later operational milestone.

## Resolved seams and continuing checks

| Seam | Resolution and coverage |
|---|---|
| Custom TCP/UDP descriptor transfer | [M0](m0-seam.md) exercised custom services with native HTTP. Product listeners separately declare their fd-table keys. |
| Foreground systemd lifetime | G37 selects ExitType=cgroup without MainPID handover; [M1](m1-systemd.md) covers repeated reload and graceful stop. |
| Received-fd duplication/transfer leaks | G38 fix is upstream in 0.9.0 (`b2b35fd`); M1 retains three-generation fd/CLOEXEC/socket assertions. |
| Unclaimed inherited descriptors | 0.9.0 `f82478a` plus every custom service's listen_addresses() enables cleanup. [unclaimed.sh](../../tests/m0/unclaimed.sh) checks closure, refusal after the old generation exits, and no third-generation transfer. Returning None from any service disables process-wide cleanup without a compiler error. |
| Global ListenFds lock across bind/await | 0.9.0 uses a short parking_lot lock and per-address async locks (`1d9371191`); product/spike callers use short synchronous holds. |
| Certificate selection shared by h1/h2/h3 | G104 replaced the former rustls premise with one BoringSSL select-certificate callback; this is a changed premise, not proof of the retired resolver path. |
| musl/BoringSSL | [Probe and product coverage](musl-boringssl.md) distinguish target runtime, build host, linked symbols and qemu/hardware boundaries. G131 selects Alpine/qemu; G138 checks product TLS symbols. |
| QUIC ownership during generation overlap | G109 routes by DCID across processes; [quic-relay tests](../../tests/quic-relay/run.sh) cover the product integration. |
| L4 UDP overlap | Shutdown stops recv_from, narrowing rather than eliminating the interval in which both generations receive. |
| pingora-cache abstraction | G82 selected a self-built cache; the old MemCache-only Storage finding is no longer an implementation dependency. |

## Lessons that still constrain tests

### Probe after the old generation exits

In the old 0.8.1 behavior an unclaimed inherited socket stayed LISTEN, accepted a
TCP handshake and then swallowed application traffic. During overlap the old
generation could still answer, hiding the defect. A port-open check or fixed
sleep after upgrade was insufficient: wait for old-generation exit, then test
application behavior. 0.9.0 fixed this when all services declare the correct keys.

### A packet drop and a connection reset are not equivalent

The examined quinn Endpoint::handle behavior sent stateless reset for an unknown
DCID. During overlap, a packet arriving in the wrong generation could therefore
terminate a live peer connection rather than merely cause retransmission. The
Option<DatagramEvent> signature did not establish the desired behavior. This
historical finding informed the quiche selection; product relay coverage remains
necessary regardless of library choice.

### Put prerequisites where implementation decisions are made

Record unresolved technical prerequisites in PLAN's decision/work tracking,
with triggers and completion criteria, rather than only in an old verification
page. Distinguish a verified path from a premise removed by design, and a spike
from product wiring. Test a source-derived hypothesis with real traffic before
presenting it as an integration fact.
