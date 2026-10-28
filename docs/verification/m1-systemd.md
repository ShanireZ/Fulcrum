---
type: verification
title: M1 systemd process model
description: Historical evidence for cgroup lifetime, repeated reload and graceful shutdown.
resource: ../../tests/m1/systemd-run.sh
tags: [verification, technical-evidence]
status: stable
sources:
  - id: plan
    resource: ../../PLAN.md
    title: Product scope, decisions and milestone exits
---

# M1 systemd process model

The 2026-08-14 experiment used systemd 257.13-1~deb13u1 on pinned Debian trixie,
a Rust 1.97.1 build and Pingora 0.8.1. It demonstrated foreground Type=notify
with ExitType=cgroup, without MainPID handover: reload preserved active state
and HTTP/TCP/UDP traffic, while stop still drained. G37 records the resulting
product decision. These versioned findings are not a fresh systemd compatibility run.

## Why MainPID handover was rejected

Handing systemd the child created by the old generation kept the unit active
but made stop return immediately with a signal failure instead of waiting for
drain. Combining handover with ExitType=cgroup did not fix that. systemd marked
the new PID as an alien child; the warning's suggestion that exit might go
unnoticed was not the observed defect. The defect was loss of graceful waiting.
Upgrade traffic alone could not detect a failure that appeared only at the next stop.

ExitType=cgroup (systemd >=250) keeps the unit alive while any process remains
in its cgroup. After the first generation exits, MainPID becomes zero. Therefore
an ExecReload based on `$MAINPID` succeeds only once; later reload reports an
error while the old generation continues serving. Read the current generation
from Fulcrum's own pid file and test at least two reloads.

## Coverage

| Scenario | Purpose |
|---|---|
| [run.sh](../../tests/m1/run.sh) | Two reloads, active unit, traffic continuity, fd hygiene and graceful stop. |
| [exit-type-main.sh](../../tests/m1/exit-type-main.sh) | Negative control without ExitType=cgroup. |
| [mainpid-handover.sh](../../tests/m1/mainpid-handover.sh) | Reproduces the rejected MainPID-handover behavior. |
| [product.sh](../../tests/m1/product.sh) | Exercises the product rather than relying on a spike. |

A passing negative-control script can mean the expected bad behavior was
reproduced. Shared M0 traffic probes avoid a second definition of zero interruption.
Stop's lower bound must prove actual waiting; a fast success cannot certify drain.
The 0.9.0 rebase removed a redundant graceful sleep (`6f15714`): the spike's
observed stop changed from 10s to 5s; the product scenario with grace=30 and
graceful=5 observed 30s within a 35s budget. Do not retain the old accidental
sum as a general timing formula. Tests/lifecycle behavior and configured budgets
control TimeoutStopSec; a draining generation no longer handles later catchable signals.

## Descriptor hygiene and version boundaries

Fork+exec revealed defects invisible to M0's shell-launched second generation:
received listener fds lacked CLOEXEC, and accepted transfer sockets leaked.
A third generation inherited an untracked copy and received another via SCM_RIGHTS.
The accepted socket also accumulated each generation. OwnedFd and atomic
MSG_CMSG_CLOEXEC fixed both; upstream 0.9.0 contains `b2b35fd` (fork change 4).

Keep all three M1 assertions after absorption: one fd per listener, CLOEXEC set,
and zero connected upgrade.sock leftovers, measured after the preceding generation
exits. These cover the fork+exec accumulation absent from a single-process unit test.

Foreground Pingora ignores its daemon-mode pid_file/error_log settings; Fulcrum
supplies the generation pid file. Foreground operation preserves pre-run threads,
and NotifyAccess=main is sufficient when MainPID is not handed over.

## Host and reproduction

A private cgroup namespace combined with an explicit host cgroup-tree mount can
break journald and notification-to-unit attribution. Under the tested cgroup-v2
privileged-container arrangement, use its correct rw view rather than adding a
mismatched host-tree bind mount. Confirm the measurement host works before
attributing discarded notifications to systemd semantics.

```bash
bash tests/m1/systemd-run.sh
```

The [unit](../../tests/m1/fulcrum-m1.service), [configuration](../../conf/m1.yaml),
[shared checks](../../tests/m1/lib.sh), [spike](../../spikes/m1-systemd/src/main.rs)
and [pinned systemd image](../../docker/Dockerfile.systemd) define the reproducible
experiment. See [process model](../architecture/process-model.md) for the product baseline.
