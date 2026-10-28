---
type: architecture
title: Data path
description: Shared HTTP routing, L4 forwarding, static files and cache.
---
# Data path

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


`fulcrum-runtime` owns the `(Host, local port)` site index, matchers and execution chain without referencing Pingora. `fulcrum-server` connects it to HttpServerApp/ServerSession and upstream connectors. One app per listening port supplies the local port that ServerSession cannot expose. Runtime unit tests and real data-plane tests cover distinct claims.

## Entrypoints

| Entry | Contract |
|---|---|
| h1/h2 TCP/TLS | Pingora HttpServerApp with keep-alive, WebSocket upgrade and gRPC/h2. |
| h3 UDP/QUIC | Custom quiche::h3 Service sharing the same FulcrumApp/router/upstream pool, including errors and files. TLS automatically adds same-port UDP and Alt-Svc. |
| L4 TCP/UDP | Custom Services participate in fd transfer and share upstream selection with reverse_proxy; protocol semantics remain distinct. |

The h3 handler is the same app instance, with an H1Entry newtype avoiding a second pool. Compare several outcomes over real h1/h3, not only a canned 200 page. During upgrade two generations share a UDP socket but only one owns each connection; authenticated generation prefixes in server CIDs route foreign DCIDs over Unix datagram relay. Decode returns no ownership information for another hop, enforcing one-hop relay. Drop when the owner exited; never ask another library to reset an unrecognized DCID.

Client identity is the socket peer unless explicitly trusted PROXY reception supplies it. Never use the leftmost X-Forwarded-For for remote_ip. HTTP reception happens before TLS through the fork seam; L4 receives and sends independently. Sending PROXY to HTTP upstreams remains unwired despite its reserved configuration syntax.

## HTTP boundaries

Pingora 0.9 rejects ambiguous requests before Fulcrum creates a request record. h1 duplicate Host, userinfo, absolute-target/Host mismatches (including spelling/case/port), malformed absolute targets and backslash authority close the connection without a 400; no access-log/metric record exists. h2 rejects duplicate Host, userinfo, Host/:authority disagreement or both absent, absolute/ambiguous :path and CR/LF in :path with 400 or stream reset; 32 cumulative bad streams can close the connection. h3 does not share this validation: the authority/duplicate-Host difference is known and unresolved.

Absolute-form uri.path() is the path with fragment stripped; forwarded raw_path stays unchanged. Identical duplicate Content-Length (including comma form) is accepted, including upstream responses. NUL/LF/CR/space in an upstream request target makes upstream writing fail; upstream obs-fold normalizes to one space. Incomplete upstream h1 bodies do not return the connection to the pool. Downstream h1 pipelining remains opt-in/off by default. h2 defaults are 100 concurrent streams and 64 KiB decoded headers (Fulcrum does not override h2_options); received h2 streams drain before shutdown.

For h1 keep-alive, one connection timer bounds total time from beginning to wait for the next request to completion of its headers at 60s (G160). Respect Pingora's continuation decision: HTTP/1.0 without opt-in, Connection: close and TE+CL do not continue; shutdown never continues (G161). No timer is added for h2/h3 or unlimited/noncontinuing reads.

## L7 write buffering (G155)

Clear/TLS L7 listeners use 16 KiB write buffers, versus Pingora's 1460-byte default; reads remain 64 KiB. Known Content-Length responses whose headers+body fit can leave in one send (TLS records can share the send). Large responses still flush headers separately; unknown-length/chunked/compressed responses flush headers regardless of buffer size. Admin uses defaults; L4/h3 do not use this buffer.

Costs are about 15 KiB additional allocation per connection, and an unbounded header delay when a reverse-proxy upstream sends headers plus a partial Content-Length body below 16 KiB then pauses. The client may receive no status line until the pause ends; this includes cache misses. Chunked/close-delimited bodies flush each write and are unaffected. Exposing flush or combining header/first body in the fork remains unscheduled option B. TCP_INFO tests cover clear/TLS h1 4 KiB single-segment responses with a 32 KiB multi-segment control; h2 has not been separately measured.

## Upstreams

Use static address lists or DNS names with periodic re-resolution. DNS refresh schedules at the repository-wide minimum interval and refreshes all upstreams; an individual longer period can therefore refresh more often. Active health checks are per-proxy, with whole-request timeout and configurable interval/status; absent health_uri means no probes. Passive circuit breaking independently watches real business failures (connect/timeout and genuine upstream 5xx), with a single half-open probe after cooldown. Do not conflate active healthy and breaker state or retry after a request has been sent. See [DSL](dsl-reference.md) for defaults, limits and weight semantics.

## Files and HTTP cache

Fulcrum implements file range/validator/index/listing behavior and complete cache semantics; pingora-cache is not vendored. Cache shares the ordinary forwarding path, applies only to reverse_proxy, compresses before storage and normalizes representation keys. This is a shared cache: Authorization responses require explicit public/s-maxage/must-revalidate permission, otherwise storing them risks cross-user disclosure.

Disk paths use two hashed directory levels, one metadata file per primary-key family and a body file per Vary variant. A response tells us Vary only after lookup, so family metadata locates variants with two opens. Write temporary files then rename. 304 revalidation changes metadata without rewriting bodies. Read directly by calculated paths without depending on the eviction index, so the first request after restart can hit. Verify length/metadata on reads and delete corrupt entries. Incremental background rebuild visits one first-level shard per second and removes orphan bodies/bad metadata; saved eviction state is advisory. Initial occupancy undercount delays eviction but does not block hits. Do not perform a full startup scan before readiness.

The backend is process-wide: inconsistent disk choices, including mixed disk/no-disk, reject FUL-DSL-0035. If a disk directory is unusable, log error, disable cache and keep forwarding; do not fall back to memory (disk capacity could OOM) or abort the whole generation. Disabled cache emits no X-Fulcrum-Cache header. Memory/disk use the same synchronous seven-operation shape; D20 stays closed pending G151's cache-hit criterion. `capacity` is the maximum across blocks, dynamic on /load and lazily shrunk; `ttl` is only fallback freshness. See [DSL](dsl-reference.md) and [observability](observability.md) for the full protocol and reporting contract.
