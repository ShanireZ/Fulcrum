---
type: architecture
title: Control plane
description: Atomic full loads and explicit nonpersistent runtime overrides.
---
# Control plane

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


| Channel | Contract |
|---|---|
| Full load | Validate a complete expected state and activate atomically; validation/health failure preserves the previous version. |
| Runtime increments | Immediate set_weight, disable, enable and purge without reload or connection interruption. |

Effective state = expected-state baseline plus temporary overrides. Only the baseline is persistent, diffable and reversible. Overrides disappear on process restart and never create another persistent write path.

`POST /load?overrides=keep` or `POST /load?overrides=clear` requires exactly one explicit query parameter. Missing, invalid or repeated values (even identical repeats) return 400 before any mutation. The body is plain structured JSON configuration, not `{"overrides":...,"config":...}`. A release pipeline may need clear while an incident operator needs keep; either default could silently do the wrong thing (G120).

`clear` lists every removed override. `keep` preserves all slots, marks keys no longer resolving to an upstream as dangling, and names them individually. Dangling entries still count as active registered overrides. Every `/load` and `/runtime` response ends with active/dangling counts; `/stats` instead includes the full list in valid JSON and never appends plain text. The unlabeled `fulcrum_overrides_active` gauge reads the same source.

The override key is (site name, optional reverse_proxy id, normalized upstream address). Shared keys intentionally affect all corresponding routes without warning; stats reports how many. Add a stable id to separate them. IDs are 1–64 characters `[A-Za-z0-9_.-]`, preventing sentinel collision and multiline ambiguity; they are not metric labels. Do not generate IDs from content hashes or positions, which change addressing across deployments/reordering.

Each slot reports disabled, weight, dangling and set_at_unix alongside the exact key. set_at_unix is the last modification's absolute wall-clock Unix time, including same-value sets; store SystemTime internally, not integer seconds. NTP jumps can reorder timestamps. Do not add a second monotonic timestamp or convert to elapsed seconds. There is no actor field: Unix socket access is authorized by filesystem ACLs and requests carry no principal identity.

Full load returns 409 without partial activation when startup-bound configuration changes: HTTP listener-port set, L4 listener set, cache backend, or global tls_groups/tls_session_resumption. Group lists compare item by item in order. Diagnostics direct the operator to `systemctl reload`, which performs a new generation. Runtime cache-capacity changes are supported (G135): compute the shared maximum once, update both backends and report the backend's read-back value; shrinking waits for next insertion rather than sweeping LRU during load.

Management defaults to a mode-0600 Unix domain socket. No admin option creates none; network addresses currently reject startup. The product policy for future explicitly enabled remote management is mTLS (G14). [Metrics](observability.md) is an ordinary protected site endpoint, not a second management API.
