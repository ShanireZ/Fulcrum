---
type: Platform Contract
title: Deployment
description: Systemd lifecycle, privileged binds, atomic load, certificate renewal, binary replacement, and deployment acceptance.
tags: [platform, engineering]
status: stable
generated:
  by: openai/codex
  at: 2026-10-28T05:16:30Z
sources:
  - resource: ../../PLAN.md
  - resource: ../../tests/m1/fulcrum-prod.service
  - resource: ../../crates/fulcrum-server/src/process.rs
  - resource: ../../crates/fulcrum-server/src/admin.rs
---

# Deployment

This technical baseline yields to [PLAN.md](../../PLAN.md). Deployment is one static binary, one `Fulcrumfile`, and one systemd unit. Actual installation, provider changes, and production tests require authorization; this runbook supplies no standing permission.

## Production unit

The [product fixture](../../tests/m1/fulcrum-prod.service) tests readiness, pid-file signaling, generation upgrades, and shutdown. The production example additionally declares a service user, privileged-bind capabilities, IPv6 binding, and `Restart=always`; the fixture intentionally uses root and `Restart=no` so automatic recovery cannot mask a failed start. Do not claim those deployment differences were verified by that fixture.

```ini
[Unit]
Description=Fulcrum
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExitType=cgroup
NotifyAccess=main
User=fulcrum
Group=fulcrum
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
ExecStart=/usr/local/bin/fulcrum serve /etc/fulcrum/Fulcrumfile --bind-host [::]
ExecReload=/bin/sh -c 'kill -USR2 "$(cat /run/fulcrum/fulcrum.pid)"'
ConfigurationDirectory=fulcrum
StateDirectory=fulcrum
RuntimeDirectory=fulcrum
RuntimeDirectoryPreserve=yes
KillMode=control-group
TimeoutStopSec=60
Restart=always
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

| Directory | Contents | Created by |
|---|---|---|
| `/etc/fulcrum` | `Fulcrumfile` | ConfigurationDirectory |
| `/var/lib/fulcrum` | `certs/`, ACME account state | StateDirectory |
| `/run/fulcrum` | pid, upgrade socket, admin socket | RuntimeDirectory |

`ExitType=cgroup` keeps the unit alive through generation handover (G37). Transferring systemd's MainPID is the rejected alternative because it loses graceful-stop semantics. After the first upgrade `$MAINPID` is no longer a usable reload target; read the **current** pid file. Keep `NotifyAccess=main` and `KillMode=control-group`.

`RuntimeDirectoryPreserve=yes` is retained insurance; the test fixture has not established behavior without it. Create the service identity and verify owner/permissions before production use. The container fixture does not prove the non-root user/capability path.

## Network and privilege acceptance

`fulcrum serve` defaults to `0.0.0.0`, which is IPv4-only. If the domain has AAAA, configure `--bind-host [::]` and verify real IPv6 reachability. On Linux with `bindv6only=0` and no IPV6_V6ONLY restriction, this binds both families; confirm the actual host setting rather than assuming it. IPv4-only local fixtures do not cover IPv6 CA validation.

For the first actual deployment verify:

1. The pid-file process runs as `fulcrum`, for example `ps -o user= -p $(cat /run/fulcrum/fulcrum.pid)`.
2. Ports 80/443 bind with only CAP_NET_BIND_SERVICE; do not add unrelated capabilities such as CAP_NET_ADMIN.
3. Certificate/account directories under `/var/lib/fulcrum` have the intended owner and restrictive permissions, and ACME can write them.
4. IPv4 and IPv6 reach the expected listeners when advertised by DNS.

A public network can intercept cleartext HTTP based on Host even when IP-only :80 reaches Fulcrum and loopback redirect works. Diagnose each path separately. If HTTP-01 validation is intercepted, G54's HTTP fallback is unavailable on that deployment; TLS-ALPN-01 and wildcard DNS-01 need their own evidence. Old operator observations do not establish which path works on another machine/domain.

## Restart policy

Use `Restart=always`, not `on-failure`, in the production example. A new generation can fail during load after the old generation has handed over and exited normally; the cgroup/unit may then end successfully, so `on-failure` does not recover the unavailable service. An explicit `systemctl stop` does not trigger automatic restart even with `always`.

## Shutdown budget

Product shutdown budget is approximately the configured grace period plus the fixed five-second runtime shutdown budget:

| Part | Default | Configuration |
|---|---:|---|
| Grace period | 30 seconds | Global DSL `grace_period <duration>` |
| Runtime shutdown | 5 seconds | Product process constant |
| Combined budget | About 35 seconds | Set TimeoutStopSec **above** this budget; example uses 60. |

Startup logs print the actual budget. Increase TimeoutStopSec when increasing grace, such as `grace_period 120s`. Generation handover can additionally spend up to five seconds transferring fds (`CLOSE_TIMEOUT`). The old Pingora 0.8 duplicate shutdown sleep is absent in 0.9; do not reuse old timing receipts as current-host evidence.

A generation already draining no longer consumes catchable signals from the main loop. If systemd reaches TimeoutStopSec, escalation kills connections; this is not merely slower completion. Budget both intended drain and the actual process model.

## Configuration changes

| Path | Command | Contract |
|---|---|---|
| Atomic load (G8/G120) | `curl --unix-socket /run/fulcrum/admin.sock -X POST --data-binary @cfg.json "http://localhost/load?overrides=clear"` | No new generation. Routes/upstreams/headers/certificate sources can change; changed listener sets or global `tls_groups`/`tls_session_resumption` return 409 (G168). |
| Generation upgrade | `systemctl reload fulcrum` | Re-exec reads the configured Fulcrumfile again, including listener/global-policy changes. Old generation drains. |

`/load` consumes the on-disk JSON shape without an envelope and requires **exactly one** `overrides=keep|clear` query value. Reject missing/duplicate/invalid input before mutation. Use `clear` for deployment back to expected state and inspect individually reported removed overrides; incident workflows may explicitly choose `keep` to preserve temporary patches. Expected state remains the sole persistent authority.

Changing the file and reloading is the standard generation workflow; rollback restores the file and reloads again. The old generation continues accepting on the shared listener for CLOSE_TIMEOUT (five seconds) after transfer, so both old/new configurations can briefly serve. Wait for response convergence, as `wait_body` does in the product test, rather than trusting one request immediately after reload. Leave enough interval between upgrades for handover plus drain to avoid accumulating generations.

## Certificate renewal

```bash
curl --unix-socket /run/fulcrum/admin.sock -X POST --data '{"domain":"*.example.com"}' http://localhost/renew
curl --unix-socket /run/fulcrum/admin.sock -X POST --data '{"domain":"*.example.com","force":true}' http://localhost/renew
```

Ordinary renewal bypasses not-yet-due scheduling but **does not bypass failure backoff**. `force:true` also resets failure count and bypasses backoff; use it after fixing the cause, such as credentials, DNS, or firewall. Clearing a nonzero count logs a warning. This avoids unsafe manual edits/deletion of certificate state and prevents repeated failing validation from exhausting account-wide CA quotas for other domains.

## Binary replacement

```bash
install -m 0755 fulcrum.new /usr/local/bin/fulcrum.tmp
mv /usr/local/bin/fulcrum.tmp /usr/local/bin/fulcrum
systemctl reload fulcrum
```

Use rename rather than writing the executing inode (`ETXTBSY`). The old executable can appear as `(deleted)` in `/proc/self/exe`; re-exec falls back from a missing `current_exe()` path to argv[0]. The product test checks the new inode. `systemctl reload` returning zero alone does not prove re-exec succeeded: check the pid file, `/proc/<pid>/exe`, and converged traffic. `systemctl restart` is a real drained stop/rebind, not a zero-downtime binary upgrade.

### Crossing the G140 configuration boundary

Old binaries do not recognize `threads_l7`, `threads_l4`, or `threads_admin`. Adding them before the new binary makes old validation fail. When upgrading across G140, either accept the new role defaults or install the new binary and explicit thread configuration together. L7 changed from a hardcoded one thread to the machine's CPU count; never copy another host's count. Explicit `threads_l7 1` controls the timing of that change, but role defaults still need the performance evidence required by PLAN. Use a separate minimal config to test whether the running version recognizes a key; do not probe unsupported syntax in production config.

## Authorized deployment checklist

1. `fulcrum validate /etc/fulcrum/Fulcrumfile`: diagnostics, structured config, runtime graph, TLS loading.
2. `fulcrum plan /etc/fulcrum/Fulcrumfile`: inspect instruction phases and any parsed-but-unwired capability. Removed fallback directives are errors, not active forwarding.
3. Start and inspect status/journal. Type=notify readiness and runtime files must appear; a clean start claim requires actual readiness, not just a sent notification.
4. Check pid file against the initial MainPID, listener families/ports, effective user/capabilities, and certificate permissions.
5. Reload once; confirm changed current pid, converged new behavior, and uninterrupted traffic. For binary replacement verify the inode/executable too.
6. Keep actual host/domain/account facts, observation windows, approval quotations, and publication receipts in the operator workspace.

See [process model](../architecture/process-model.md), [control plane](../architecture/control-plane.md), and [build and test](build-and-test.md).
