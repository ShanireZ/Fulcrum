# Fulcrum comparison benchmarks (M3)

[PLAN.md §8](../PLAN.md) (G19) defines acceptance: compare Caddy, HAProxy and
nginx on the same host, cases and load generator; meet at least 90% of the
strongest applicable competitor in every category; publish scripts and raw data.
Milestone state belongs to PLAN. Historical readings and their qualifications
remain in the dated [results](results/) directories. Preserve those snapshots
byte-for-byte (G147), including unfavorable readings and redaction disclosures.

## Running the pipeline

```bash
bash tests/bench/run.sh
```

This development gate builds the pinned benchmark image, exercises the pipeline
and tests both directions of its criteria. It is not a qualified performance run.
For measurements, use the launcher on a qualified host, with explicit CPU topology,
workers and load settings:

```bash
BENCH_HOST_ATTEST="Dedicated host; no TUN proxy or VPN on the network path" \
BENCH_SERVER_CPUS=0-1 BENCH_LOAD_CPUS=2-3 \
BENCH_DURATION=60s BENCH_CONNECTIONS=200 BENCH_WORKERS=2 \
BENCH_FULCRUM_BIN=/path/to/fulcrum \
  bash bench/docker-run.sh bench/out
```

CPU sets above are examples, not a topology recommendation. The launcher checks
host-only sysctls, applies container sysctls and passes host observations into the
snapshot. Calling `bench/run.sh` directly bypasses that evidence and produces an
unqualified run.

Order is significant: criterion self-tests → environment snapshot/qualification →
case raw data → diagnostics → verdict. Diagnostic failure does not suppress case
verdicts. Defaults are defined once in [lib.sh](lib.sh), not independently in each
case: duration 10s, connections 50, workers 1, payload 4096 bytes (G147).
`env.json.load_params` records effective values. Set workers explicitly for a
qualified measurement window.

## Host qualification

`bench_disqualifiers` in lib.sh is a pure function with synthetic positive and
negative controls (`bash bench/lib.sh --self-check`). No disqualifiers means qualified.

| Criterion | Disqualifying condition |
|---|---|
| `kernel` | Kernel string contains `microsoft` or `wsl`. |
| `cpus` | Fewer than `BENCH_MIN_CPUS` (default 4). |
| `load` | One-minute load exceeds `BENCH_MAX_IDLE_LOAD` (default 0.50). |
| `affinity` | Server/load CPU sets are missing, invalid or overlap. |
| `kernel-params` | Required kernel/resource settings or launcher evidence are absent or differ. |
| `attest` | No explicit `BENCH_HOST_ATTEST` declaration of a dedicated host and no TUN proxy/VPN. |

Logical affinity does not establish physical-core separation. `host.cpu_topology`
records sibling threads, server/load physical cores and `shared_cores`; topology
is disclosed, not an additional qualification decision. Three earlier historical
sets predate topology capture. Attestation records a human declaration, not proof;
disclose resident probes or tunnel clients honestly.

### Kernel parameters and limits (G145)

| Location | Required settings | Check |
|---|---|---|
| Container network namespace | `net.core.somaxconn=65535`, `net.ipv4.tcp_max_syn_backlog=65535`, `net.ipv4.ip_local_port_range=10240 65535`, `net.ipv4.tcp_tw_reuse=2` | Launcher derives `--sysctl` from `bench_container_sysctls`; snapshot compares actual container values with requirements. |
| Host | `net.core.netdev_max_backlog=16384` | Configure using [sysctl.conf](sysctl.conf); launcher reads host `/proc/sys`, refuses mismatches and passes `BENCH_HOST_SYSCTLS`. |
| Process resource limit | `nofile=1048576` | Docker `--ulimit`; snapshot checks `ulimit -n`. |

Host per-netns settings do not configure a new container namespace. Conversely,
`netdev_max_backlog` is not available to configure/read inside this container.
`tcp_tw_reuse=2` covers loopback without enabling global reuse. `fs.file-max` is
intentionally excluded: lowering an already larger host limit is harmful, while
the relevant process restriction is RLIMIT_NOFILE. Congestion control, socket
buffers and `vm.*` are excluded because they change the experiment itself.

The gate deliberately omits host evidence: it verifies container sysctls positively
and host-evidence rejection negatively (C13/C12). Do not fabricate evidence to
make that output green. Snapshots distinguish `sysctl_as_seen_in_container` from
`kernel_params`. The launcher reads host values once; it cannot prove they remain
unchanged throughout the run. The 16384 backlog value is not proven sufficient;
`/proc/net/softnet_stat` drop deltas are not automatically collected.

## Verdict and invalidation

[verdict.sh](verdict.sh) first checks qualification. An unqualified host produces
`VERDICT: UNQUALIFIED` and reasons, without computing a strongest competitor,
threshold or PASS/FAIL. Raw data remains useful only as pipeline evidence.

For qualified, valid, higher-is-better cases:

```text
strongest = max(all non-Fulcrum readings in this category)
threshold = strongest × 0.9
PASS iff Fulcrum >= threshold
```

Derive competitors from raw files; do not hard-code a subset or average across
competitors/categories. A first lower-is-better category requires an explicit
direction, rather than guessing from its name.

[read-raw.py](read-raw.py) requires `successRate == 1.0`, only status 200, and no
transport errors before comparison. Any invalid reading gives that category
`NO-VERDICT`. Preflight additionally checks status and exact body bytes; preflight
and load-time validity cover different times.

### External ceilings (criterion ④)

- **B, full spread:** `(highest-lowest)/highest < BENCH_MIN_SPREAD` (default 0.05)
  gives `VOID`. Convergence suggests an external limit; it does not identify oha,
  loopback or affinity as the cause.
- **C, top convergence (G146):** compare the best and second-best readings, but
  invalidate only an apparent PASS. An external ceiling lowers the competitor
  threshold, so a FAIL remains a FAIL. One slow implementation must not conceal
  convergence among faster ones. Equality at 0.05 counts as separated.
- **A, optional absolute ceiling:** with positive `raw/<case>/ceiling.txt`, check
  the strongest against generator ceiling × `BENCH_GEN_HEADROOM` (default 0.90).
  Without that file this criterion is inactive. A reference must actually exceed
  all compared servers; measuring a slower server underestimates the ceiling.

Insufficient readings or invalid ceiling inputs give `VOID`, not acceptance.
The 0.05/0.90 thresholds remain judgment values without empirical validation.
The first qualified 2026-09-06 set exposed the full-spread blind spot that led to
G146; it did not validate the numerical thresholds. Without A, separated readings
do not prove the generator has spare capacity.

## Aligning the implementations

[conf/](conf/) contains the actual reproducible server templates.

| Dimension | Contract |
|---|---|
| Server scheduling | Run one competitor at a time. |
| Payload | Same bytes and URL within a case; no compression. Static uses `/dev/urandom`; inline respond/proxy cases use printable base64 bytes. Do not compare absolute results across these different payloads. |
| Logging/protocol | Access logs off; HTTP/1.1 cleartext except TLS-handshake; keep-alive except short-connection/handshake cases. |
| Identity | All four run as root inside the container, including explicit nginx `user root`. |
| Workers | Fulcrum `threads_l7`, nginx `worker_processes`, HAProxy `nbthread`, Caddy `GOMAXPROCS`, all set from `BENCH_WORKERS`. |
| Upstream reuse | Configure all four explicitly. Caddy/nginx idle capacity follows `bench_upstream_idle_cap = max(64, connections)`. |
| Site filesystem | Launcher uses an anonymous volume at `BENCH_WWW_ROOT`; empty `BENCH_WWW_ROOT` selects the container writable layer. Snapshot actual `site_root.path/fs/rwf_nowait`. |

Filesystem is part of the method. Fulcrum first tries open/fstat/RWF_NOWAIT on the
current thread, then falls back to blocking work. The 2026-09-06 probe on kernel
6.18 read a hot 4096-byte file fully on ext4, while overlayfs/tmpfs returned
`EOPNOTSUPP`. This changes the exercised code path. Earlier overlay readings and
later volume readings are not directly comparable. `unknown` means the probe
could not determine support, not that support is absent. Read fs and rwf_nowait together.

HAProxy's static case uses `http-request return ... file`, not a file server: no
directory handling, conditional requests or ranges. Its fixed-body throughput
therefore includes less work; retain this caveat with every cited result.

## Pinned versions

The image in [Dockerfile.bench](../docker/Dockerfile.bench) is pinned by digest,
not floating tags. Keep this table synchronized; tests/bench/run.sh checks it and
requires matching oha versions/SHA256 in build and benchmark images. G152 freezes
the entire benchmark image during M3; G36 separately governs compiler changes.

| Purpose | Version | Source digest |
|---|---|---|
| nginx | 1.29.1 | `nginx@sha256:8adbdcb969e2676478ee2c7ad333956f0c8e0e4c5a7463f4611d7a2e7a7ff5dc` |
| HAProxy | 3.2.3 | `haproxy@sha256:baecd003ae5c2376e58d04a5b2ffc7d82d30e9bebe2869f76222b819f719dee3` |
| Caddy | 2.10.2 | `caddy@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d` |
| Runtime base | Debian trixie slim | `debian@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132` |
| Generator | oha v1.15.0 | Must match Dockerfile.build version and SHA256. |

Build/runtime Debian codenames must match because Fulcrum links glibc. nginx and
HAProxy binaries originate on bookworm; trixie uses different package names for
some identical SONAMEs (for example `libssl3t64`).

## Categories from PLAN §8

| Case | Method |
|---|---|
| [Static throughput](case/static-throughput.sh) | All four serve one fixed resource; retain HAProxy's reduced-work caveat. |
| [Reverse proxy throughput](case/reverse-proxy-throughput.sh) | One persistent Fulcrum respond origin, reused across all four proxy runs. |
| [Short-connection throughput](case/short-connection-throughput.sh) | Memory responses, new connection per request, server closes first. |
| [TLS handshake](case/tls-handshake.sh) | HTTPS short connections with a full TLS 1.3 handshake per connection. |

L4 forwarding, cache-hit acceptance and long connections do not yet have case
scripts. The frozen official Caddy binary has neither L4 nor HTTP cache support;
those categories require a missing-competitor rule before implementation. HAProxy
has a limited small-object memory cache; its use/alignment is not yet designed.
Case execution and verdict discovery derive from `case/*.sh` and `raw/*/`.

### Reverse proxy

The origin and tested proxy share server CPUs. This preserves load-generator cores
but prevents absolute comparison with static throughput. Fulcrum's own turn is
Fulcrum→Fulcrum; the qualitative bias is conservative for PASS and cannot excuse
FAIL. The earlier 2×2 proxy/origin diagnostic confounded same-binary and origin
main effects, so it did not quantify that cost. Printable payloads also differ
from static throughput.

Criterion ⑤ measures new origin connections per thousand requests using
`Tcp: PassiveOpens` deltas, saved as `upstream-conns.txt`. The maximum is
`BENCH_UPSTREAM_REUSE_MAX=50`. Small gate loads detect one-new-connection-per-request
but can miss partial non-reuse visible at measurement-window concurrency. Do not
tighten the threshold to small-run noise; lib.sh self-tests retain that blind spot.

### Short connections

All four return the same memory body. Use both `--disable-keepalive` and
`-H 'Connection: close'`: oha 1.15.0 already inserts that header, but the explicit
argument documents the method. Templates are named by response shape, not category.

Criterion ⑥ checks:

1. `Tcp: PassiveOpens` delta/request × 1000 is at least
   `BENCH_SHORT_NEW_CONNS_MIN=990`, saved in `new-conns.txt`.
2. The actual request contains `Connection: close`, captured before load with the
   same `OHA_SHORT_ARGS` by [close-probe.py](close-probe.py) `--capture-request` on :9960.
3. Each server's preflight reaches EOF within a deadline; inspect behavior rather
   than trusting a response header. The probe has positive/negative controls.

TIME_WAIT server/client counts remain diagnostic only: table overflow and the
close race make them unsuitable as verdict criteria. Omitting a redundant flag
need not fail when wire behavior is unchanged; forcing keep-alive must fail the
header criterion even if every request still opens a new connection.

nginx cannot hold a 4096-byte return argument in its 4096-byte configuration buffer,
so [nginx-respond.conf](conf/nginx-respond.conf) joins two variables. This adds
allocation/copy relative to nginx's optimal literal response; its size has not
been measured. If nginx leads, the threshold is biased downward: discount PASS,
not FAIL. Retain differing response headers and printable-versus-static payloads
as further caveats. Client connect/close cost makes generator saturation a risk.

### TLS handshake (G168)

Use one freshly generated self-signed ECDSA P-256 leaf certificate/key for all
four servers, hostname `bench-tls.test`, and oha `--connect-to` loopback. Verify
TLS 1.3 rather than imposing a server-only version restriction unavailable to
Fulcrum. Requests/second equals handshakes/second because criterion ⑥ proves one
request per new connection.

Disable session resumption on every server, including issuance of all tickets:
Fulcrum `tls_session_resumption false`; nginx `ssl_conf_command NumTickets 0`;
HAProxy's private `OPENSSL_CONF` with `NumTickets = 0`; Caddy JSON
`session_tickets.disabled`. Disabling only cache/tickets flags under OpenSSL 3.5
can still issue two stateful tickets.

All four explicitly use `BENCH_TLS_GROUP=X25519MLKEM768`, one of oha's offered
key shares. Choosing P-256 instead would add HelloRetryRequest. `bench_tls_probe`
runs `openssl s_client -trace` twice, reusing the first session on the second
attempt. Save `probe-<server>.txt`; require TLSv1.3, the selected group, no HRR,
zero tickets and a fresh second handshake. Failure gives only this category
`VERDICT: VOID`. C23 checks ticket/resumption visibility against negative controls.

TLS 1.3 cipher selection cannot be fully aligned: Go/Caddy may choose AES-128-GCM
while the others choose AES-256-GCM. Record `cipher=` diagnostically and disclose
the actual result when citing readings. nginx's joined-body caveat also applies.

## Diagnostics: outside §8 acceptance

[diag/](diag/) contains Fulcrum-only diagnostics. Save them under `diag/`, never
`raw/`, and exclude them from strongest/threshold/PASS/FAIL calculations. C17 derives
the diagnostic names and verifies that none becomes a raw category.

- [respond-ceiling.sh](diag/respond-ceiling.sh) compares file_server and respond
  back-to-back. Neither uses encode; both use the same printable payload. Their
  response paths differ, and these readings cannot be compared directly with the
  separate static-throughput payload.
- [cache-hit-p99.sh](diag/cache-hit-p99.sh) compares memory and disk backends
  back-to-back, with a file_server origin behind reverse_proxy. Cache only wraps
  reverse_proxy. Backends are process-wide (`FUL-DSL-0035`), so derive memory
  configuration by removing disk, with only that setting and necessary listen-port
  changes. Run one variant at a time; a hit does not visit the origin.

Derive cache TTL as 10× BENCH_DURATION. Pre/post state must be HIT/HIT-DISK, and
post-load `Age >= duration`: revalidation resets stored_at and Age, then becomes
Hit again, so a final HIT alone cannot prove the entry stayed fresh. Save
`cache_state` before/after `{state, age}` and `duration_secs` in delta.json; C18
independently checks them. C19 verifies `cache_root.path/fs/rwf_nowait`, matching
the site-root disclosure. Development overlay and qualified-host volumes are
not directly comparable.

G151 assigns no millisecond threshold to disk-p99 minus memory-p99 under saturated
closed-loop load: much of it is throughput-derived queueing, not tail latency.
Keep recording it; only the eventual cache-hit acceptance category may reopen D20.

## Output and interpretation

```text
<output>/
  env.json                       environment/qualification; fulcrum-bench-env/1
  raw/<case>/<server>.json        unmodified oha output
  diag/<diagnostic>/<variant>.json
  verdict.txt                    verdict or reasons for withholding one
```

Each raw-case `*.json` is interpreted as a competitor. Auxiliary observations must
use `ceiling.txt`, `upstream-conns.txt`, `new-conns.txt`, `oha-request.txt` or probe
text files, not additional JSON files in that directory. Preserve all raw and
diagnostic data; publication cannot select only favorable runs.

A green development gate proves pipeline/criterion behavior, not M3 acceptance,
production readiness or a root cause for a throughput gap. Incomplete categories,
unverified attestations and external-ceiling uncertainty remain explicit limits.

The immutable 2026-09-06 set has two known documentation defects: its REDACTION
text describes more than one textual change despite saying one; its final README
link points to nonexistent bench/results/README.md instead of this page. Preserve
the original evidence and apply these corrections when reading it.

[Performance acceptance](../docs/verification/performance-bar.md) ·
[Build and test](../docs/platform/build-and-test.md) · [PLAN](../PLAN.md)
