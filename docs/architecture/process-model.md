---
type: architecture
title: Process and component boundaries
description: One process owns control, data and lifecycle management.
---
# Process and component boundaries

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


Fulcrum's parser, validator, transactional loader, runtime and background services share one process. The traffic path normally has one Fulcrum hop to upstream services or local files. The external-engine fallback has been removed; Fulcrum never supervises another server.

The Pingora Server hosts native h1/h2 services, custom L4 TCP/UDP, QUIC/h3 and background services. Native listener keys are address strings; custom services may use their own key namespace. Custom Service implementations must obtain inherited fds and return them during transfer; fd transfer does not happen merely by using the trait.

## systemd lifecycle

Use foreground `Type=notify`, `ExitType=cgroup` (systemd >=250), `NotifyAccess=main`, systemd User/Group privilege dropping and stderr→journal. `conf.daemon` stays false. systemd owns ConfigurationDirectory=/etc/fulcrum, StateDirectory=/var/lib/fulcrum, RuntimeDirectory=/run/fulcrum and CacheDirectory=/var/cache/fulcrum; every path is overridable. Foreground operation also avoids daemon-fork loss of threads created before run(). Pingora 0.9 uses daemonix, but Fulcrum does not exercise it.

Do not transfer MainPID to the forked generation: systemd treats that pid as alien and can skip graceful draining on later stop. ExitType=cgroup keeps the unit active while either generation exists; after the first upgrade MainPID may be zero, so ExecReload must read the current generation's atomically written pid file rather than `$MAINPID`. A systemd PIDFile= directive is unnecessary; that does not eliminate the product's pid-file requirement.

Product process.rs wires four operations: wait for ExecutionPhase::Running; atomically write pid before notifying READY=1; handle SIGUSR2 by fork/exec of itself with -u, sending self SIGQUIT only after successful spawn; explicitly set the 30s drain and 5s final shutdown budgets. Log the effective stop budget. TimeoutStopSec must cover their sum (60s covers defaults); leaving shutdown timeout None would use Pingora's 300s fallback. On generation inheritance, keep the admin UDS pathname: unlinking the inherited listening inode leaves both generations listening on an unnamed socket and clients get ENOENT.

`tests/m1/product.sh` exercises the product lifecycle; a spike proving a mechanism is not evidence the product uses it. See [deployment](../platform/deploy.md).

## Threads

Pingora threads are not shared across services: total workers approximately sum each service's allocation. A generic global CPU-count value would oversubscribe a multi-service server. work_stealing=true is global and remains selected (G35); NoSteal current_handle chooses a random worker, so same-thread handoff cannot be assumed.

| Role | Global DSL override | Default per service |
|---|---|---|
| L7 h1/h2/h3 | threads_l7 | CPU count |
| L4 TCP/UDP | threads_l4 | 2 |
| Admin/background/otherwise unoverridden | threads_admin | 1 |

Values are integers 1–1024, FUL-DSL-0046 otherwise; zero would panic when constructing the runtime. process::default_threads is the single default source. threads_admin also fills ServerConf.threads; L7/L4 override Service::threads. These are initial values awaiting M3 evidence, not proven optimal defaults. Introducing G140 changed L7 from the former accidentally implicit 1 to CPU count; that upgrade changes resources/latency distribution. Tests must inspect actual OS-thread count, not merely echo the computed value in a log. QUIC's sans-IO loop remains its implementation's responsibility while using the L7 role.
