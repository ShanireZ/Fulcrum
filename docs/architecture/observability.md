---
type: architecture
title: Observability
description: Flat access logs, bounded Prometheus metrics and runtime stats.
---
# Observability

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


G16 defines metrics, structured logs and live stats. Current milestone state belongs in PLAN, not this contract. OpenTelemetry tracing and a Web UI are outside first-release scope.

## Access-log contract (G113/G114)

One newline-terminated flat JSON object per record; no nested objects or arrays. Omit unavailable optional fields rather than null or invented values. Outcomes describe execution paths independently of status: respond 503 has outcome=respond and level=error; outcome=error means the built-in/handle_errors path.

| Fixed field | Type | Meaning |
|---|---|---|
| `ts` | string | UTC RFC3339 with millisecond precision. |
| `level` | string | Status-derived: <400 info, 4xx warn, 5xx error. Separate from configured threshold. |
| `proto` | string | HTTP/1.1, HTTP/2.0 or HTTP/3.0 from the actual entry, not inferred. |
| `method` | string | Original method. |
| `host` | string | Host/:authority without port. |
| `uri` | string | Original request target path/query before rewrite; Pingora normalization applies to absolute-form input. |
| `status` | number | Written response status; 0 when no response header was successfully written. Buffer acceptance is not proof of peer delivery. |
| `resp_size` | number | Body bytes only, excluding headers; fork change 13 fixes h1 accounting to match h2/h3. |
| `duration_ms` | number | Request-header read through response completion, milliseconds with three decimals; idle wait is excluded. |
| `outcome` | string | Closed path set: reverse_proxy, file_server, respond, redir, error, no_site_match, acme_http01, metrics. |
| `remote_ip` | string | Socket peer, or original client supplied by explicitly trusted PROXY reception. |
| `remote_port` | number | Corresponding client port. |

outcomes! generates the constants/closed set from the same literal token; OutcomeName's private field lives in outcome.rs, outside the constructing caller module. serve_one/write_error return the typed value rather than mutating a Record field. This makes an unset/default aborted state unrepresentable and forces every return path to report its outcome. Duplicate literals are separately checked.

| Conditional field | Availability |
|---|---|
| `site` | Matched site name: original first configured address, including scheme/port. Absent for no_site_match. |
| `upstream` | Reverse proxy actually selected an upstream. |
| `cache` | Live cache status, same source as X-Fulcrum-Cache (HIT/HIT-DISK/REVALIDATED/etc.); origin contact has none. |
| `tls_version` / `tls_cipher` | Nonempty negotiated fields; empty omits. |
| `tls_sni` | Client supplied SNI, lowercase; independent of request host. |
| `tls_alpn` | Actual negotiated ALPN; absent when no ALPN, even though proto still exists. |
| `req_hdr_<name>` / `resp_hdr_<name>` | Allowlisted and actually present request/final sent response headers. |

Normalize header names to lowercase with '-'→'_', prefixed req_hdr_/resp_hdr_. Join multiple values with comma+space; deduplicate allowlist names case-insensitively. Response values are after header middleware, not merely upstream originals.

Both entrances read the same SslDigest. h1/h2 fills it in from_ssl; h3 constructs it from quiche's public server_name/application_proto. h3 version is TLSv1.3 by RFC9001; cipher is unavailable through quiche's private API and therefore omitted. Never invent it. Metrics use <unknown> for the same unavailable datum because every series needs its label value.

### Logging configuration and failure boundaries

log output defaults stderr/journal or file <absolute-path>. Open the file during load: failure rejects startup or /load=400 and leaves the previous graph untouched. level is a threshold: debug/info=all, warn=4xx/5xx, error=5xx. Request headers and response headers require explicit nonempty allowlists; absent means no headers. Names must be valid RFC9110 tokens. Authorization, Cookie, Set-Cookie and Proxy-Authorization reject case-insensitively (FUL-DSL-0036), including the independent JSON Runtime::build path.

No matched site means no site log configuration, hence no access-log line for no_site_match/421. It still appears in metrics. Request-header parse failures before a Downstream/Record exists have neither a record nor request metrics. Never place private keys, ACME credentials or upstream authentication in ordinary logs; previews and Secret formatting redact by default.

## Metrics endpoint and collection

metrics is a site terminal at order 75, between respond and reverse_proxy. Protect it inside handle before fallback respond, using source restriction/TLS/logging from existing mechanisms. It returns 200 and `text/plain; version=0.0.4; charset=utf-8`. This is separate from the Unix management API; requiring a Unix-only exporter would violate the single-installation goal. A mistaken matcher can expose metrics. FUL-DSL-0037 recognizes remote_ip/header protection (including enclosing containers/negation), not path/host/method/query alone, and cannot prove a correct CIDR or secret header.

encode currently has no effect on metrics/respond/redir; exposition is uncompressed even when configured, at a potentially significant continuing scrape-bandwidth cost. Plain-text format is self-built with no new package; tests own label escaping, histogram bucket/sum/count and _total naming.

Request metrics are recorded only in access_log::Record::finish, before optional emit filters, independent of site log enablement. Outcome/status are calculated once. The same single clock read supplies duration_seconds and duration_ms (wall-clock reversal clamps to zero). Logged-site increments must agree with log rows; no-log/no-site requests are counted separately, and scrape requests can themselves affect deltas.

## Metric families and cardinality

FAMILIES in crates/fulcrum-server/src/metrics.rs is the sole declaration. This table follows its rendering order; series sort by label values. “Event” means increment/observe at an event point; “Live” means read the actual runtime/cert/ACME/connection source at scrape time, never maintain a second registry copy. Each family has one Source. Render HELP/TYPE even without samples so nonexistent and not-yet-observed differ. No family may add uri as a label.

| Family | Type | Source | Labels | Bound |
|---|---|---|---|---|
| `fulcrum_requests_total` | counter | Event | `site` `outcome` `status_class` `proto` | (addresses + 1) × 8 × 6 × 3 |
| `fulcrum_request_duration_seconds` | histogram | Event | `site` `outcome` | (addresses + 1) × 8; fixed buckets |
| `fulcrum_cache_events_total` | counter | Event | `event` | hit/miss/stale: 3 request outcomes |
| `fulcrum_no_site_match_total` | counter | Event | `host` | Configured literal hosts plus <other> |
| `fulcrum_upstream_inflight` | gauge | Live | `upstream` | Configuration-bound |
| `fulcrum_upstream_healthy` | gauge | Live | `upstream` | Configuration-bound |
| `fulcrum_upstream_passive_open` | gauge | Live | `upstream` | Configuration-bound |
| `fulcrum_cert_expiry_seconds` | gauge | Live | `domain` | Loaded certificates; currently configuration-bound only while on_demand is UNWIRED |
| `fulcrum_acme_issue_total` | counter | Live | `result` | ok/fail/deferred: 3 |
| `fulcrum_build_info` | gauge | Live | `version` | 1 series, sample value always 1 |
| `fulcrum_overrides_active` | gauge | Live | None | Always 1 series; value is registered overrides including dangling |
| `fulcrum_cache_purged_entries_total` | counter | Event | None | Always 1 series; cache entries, not requests |
| `fulcrum_tls_requests_total` | counter | Event | `version` `cipher` | Server-compiled version/cipher sets plus <unknown> for each |
| `fulcrum_connections_total` | counter | Live | `listen` `entrypoint` | Listener count, each one of 5 entrypoints |
| `fulcrum_connections_active` | gauge | Live | `listen` `entrypoint` | Listener count, each one of 5 entrypoints |

The compiled contract compares family sets in both directions, types, Source, ordered labels, unlabeled-series bound and closed-domain cardinalities. Free-form configuration-bound formulas still require human reasoning. On-Demand integration must reconsider cert-domain cardinality: visitor SNI could otherwise create permanently unbounded series.

### Registry implementation (G156)

Event writes use 64 fixed shards aligned to 128 bytes; a writing thread claims a shard round-robin and keeps it. More than 64 writer threads share some locks while remaining correct. Borrowed &[&str] lookup avoids steady-state key allocation; first series insertion may allocate. Check family kind/label arity before locking and again inside to avoid poisoning shards on invalid input. Gauges use a separate global table since cross-shard last-writer semantics are undefined.

Scrapes lock/merge/release one shard at a time then render the same ordered format. They are not process-atomic snapshots; request count and duration were already separate writes. Floating histogram-sum low bits can differ due to addition order; single-writer rendering is byte-identical. Cardinality is unchanged but each series can have 64 stored copies (worst-case example around 20MB for all combinations of ten addresses; practical hot subsets are smaller). Concurrent totals, independent-shard progress, borrowed-key ordering, no-allocation steady state and TLS-destructor writes have dedicated tests.

### Request/site/status semantics

status_class is 1xx/2xx/3xx/4xx/5xx/none. status=0, 1–99 and 600+ use none, not an invented 0xx. A downstream RST does not by itself imply status 0: Content-Length headers can succeed into a buffer with no syscall, yielding 200 even though the peer already left. A slow upstream without Content-Length forces header flush, making status 0 observable. tests/metrics/run.sh checks the real log status/outcome/site plus exactly one none increment, and a successful-200 control.

Metric site is the actually matched address literal, not visitor host or first site address: lowercase exact host, wildcard folded to its literal, hostless fallback as :port. This bounds series and keeps address order irrelevant; per-tenant wildcard detail remains in logs. <none> covers no site and pre-routing ACME HTTP-01, distinguished by outcome. Log site instead identifies the block's original first address with scheme/port; these similarly named fields intentionally differ.

no_site_match host labels retain only configured hosts, all others <other>; visiting many unknown hosts must both leave series count unchanged and increase <other>. Visitor-supplied SNI/ALPN/URI never become labels. <other> bounds visitor values, <none> marks unmatched/no-site, <unknown> means unavailable server data; sentinel syntax cannot collide with valid values.

### Cache metrics

hit/miss/stale count requests that queried cache and whose content source is decided. hit and stale are recorded at write_cached, from the same state as response/log headers. miss is recorded at origin contact, not initial lookup failure: only-if-cached can return 504 and coalesced followers can later hit. Origin-connect failures are absent from all three, biasing hit/(hit+miss) high during outage; consult upstream health and outcome=error. Uncached routes do not contribute. Memory/disk hit kinds fold together because backend is process-constant; detailed type stays in the cache log field.

purged_entries counts entries removed by key/prefix/all, not purge calls, with no redundant mode label. Record +0 after a zero-entry purge so “never purged” differs from “purged nothing”. Keep it separate from request events; summing mixed entries and requests would create meaningless traffic spikes.

### TLS and connection metrics

TLS counts requests, not handshakes: keep-alive permits many requests per handshake. Clear requests contribute nothing, so sum(TLS)<=sum(requests), with strict inequality only when measured traffic includes clear requests. Both labels are server-negotiated, not visitor-supplied, and empty values become <unknown> through a transport-independent helper. For h3 this gives TLSv1.3/<unknown>; logs omit cipher. Never collapse empty into absent-label semantics.

Connection instrumentation covers four accept loops: fork Service::run_endpoint for HTTP/admin, L4 TCP, L4 UDP and QUIC. All use one ConnRegistry/label definition. entrypoint's closed set is http/admin/quic/l4_tcp/l4_udp, deliberately named differently from request proto. listen plus entrypoint distinguishes same-port TCP/UDP and multiple addresses; cardinality is listener count, not five times it. Create zero samples when a listener is declared. An undeclared source warns once and uses <undeclared> rather than dropping evidence of missing wiring.

Count accepts before TLS handshake, including waiting/failed/time-out handshakes. One Drop guard decrements active for every exit. l4_udp instead derives active from sessions.len(), measured at loop start so continue paths cannot skip refresh; UDP has sessions, not connections. Check active<=total, each entrypoint, pre-handshake active and failed-handshake cleanup with actual connections.

### Live values and generations

Group same-address upstream inflight by sum, healthy by AND and passive_open by OR, preserving evidence of any failure. A no-active-check reference is always healthy, so OR would mask a failing checked reference. Certificate expiry is absolute notAfter Unix seconds; consumers subtract time(). Read loaded production certificates only; challenge certificates live in a separate table. Do not periodically maintain a redundant remaining-time counter.

Build info identifies the binary using FULCRUM_BUILD_ID, direct .git then explicit package-version+unknown; no dirty-tree detection, so packaging supplies identity. No gen_id/pid label: every upgrade would leave abandoned series. Counters reset per process and never transfer across generations; Prometheus rate/reset semantics handle this without another interprocess state channel.

## Runtime stats/control

POST /runtime accepts set_weight/disable/enable verbs without persistence. POST /purge shares the same management socket. GET /stats returns JSON for generation/load time, upstream inflight/healthy/passive state, certificate expiry, cache occupancy and full overrides (keys, affected routes, disabled/weight/dangling/set_at_unix). Read the same live atomics as metrics instead of counting separately. Scrapes hold one runtime snapshot and use override_entries_of rather than taking another snapshot.

`POST /load?overrides=keep|clear` requires exactly one parameter, no default or JSON envelope. clear names every removal; keep names dangling entries. Load/runtime text responses carry active/dangling counts; stats stays valid JSON. Last-modification timestamps are absolute wall-clock seconds, not ages. See [control plane](control-plane.md) for atomic-load refusals, state authority and ACL boundaries.

## Validation boundaries

The real metrics scenario checks format/Content-Type/label key sets, access-control allow+deny, configuration diagnostics, absence without metrics, bounded unknown hosts, and log/count consistency. TLS cases separately check h1/h2/h3 increments, label values against real logs, no cleartext count, no empty labels or SNI/ALPN labels. Family declaration presence and active source wiring are distinct assertions; positive counts alone do not prove the correct label bucket. Parser fixtures witness hit/miss behavior before deltas are trusted. No synthetic gate substitutes for production observation.
