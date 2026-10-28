# Fulcrum: product and implementation plan

This file is the authority for product scope, milestone exits, G-decisions and open D-questions. The [architecture baseline](docs/architecture/index.md) supplies technical details and yields to this plan on conflict. Decisions retain their identifiers: source, tests and documentation refer to them. Superseded decisions remain identified below; historical dialogue, approval quotations, operating records and validation receipts belong outside the repository.

<a id="1-当前状态"></a>

## 1. Current state

| Milestone | State and remaining exit |
|---|---|
| M0: integration seams | Passed: native HTTP and custom TCP/UDP listeners survived a generation upgrade. |
| M1: serve one machine | Passed, including the product binary under systemd and the real-server smoke-test exit. |
| M2: serve two machines | In progress. Implementation batches and all three observability surfaces are complete; two machines serving real traffic is the remaining operational exit. |
| M3: performance acceptance | In progress. The methodology is implemented in [bench/](bench/README.md). The latest qualified set is [window 4](bench/results/2026-10-05-window-4/README.md), covering three of seven categories, all FAIL: static throughput 21652.9 rps versus nginx 55193.5, threshold 49674.2; reverse proxy 7026.0 versus nginx 13846.0, threshold 12461.4; high-concurrency short connections 12843.1 versus nginx 15741.5, threshold 14167.4. These are historical readings, not results for every later commit. Four categories remain unmeasured; no overall or root-cause conclusion follows. |
| M4: release | Not started. |

The six product crates own configuration compilation (`fulcrum-config`), a Pingora-independent runtime graph (`fulcrum-runtime`), certificate selection/storage (`fulcrum-tls`), ACME (`fulcrum-acme`), the data plane (`fulcrum-server`) and CLI (`fulcrum`). [The Pingora fork](vendor/pingora/FORK.md) is based on 0.9.0; `spikes/` contains experiments rather than release products.

Implemented capabilities include the DSL-to-runtime-to-TLS path; reverse proxy and four load-balancing policies; DNS refresh, active checks and passive circuit breaking; all three ACME challenges; atomic configuration load; systemd generation upgrades; TCP/UDP forwarding and SNI/ALPN routing; PROXY protocol reception; static files; memory/disk HTTP cache, request coalescing and purge; compression; HTTP/3 with interprocess QUIC relay; JSON access logs, metrics and runtime stats.

<!-- contract:unimplemented-summary -->
**Not implemented**: sending PROXY protocol to HTTP upstreams.

This is a summary, not the exhaustive DSL `UNWIRED` contract. Internal CA TLS, On-Demand TLS and tracing are parsed but unwired; see the [DSL reference](docs/architecture/dsl-reference.md). Validate the complete applicable development gate with `bash tests/m0/docker-run.sh`. Rust runs only in Docker (G107); [build and test](docs/platform/build-and-test.md) defines its coverage.

## 2. Positioning

One Rust static binary serves as a web server, reverse proxy and load balancer: simple configurations stay simple, and complex deployments can keep the same software. The intended result is one process, one configuration and one observability system for non-Kubernetes deployments. The primary users are the owner's fleet and self-hosting/homelab operators, with equal priority; migration users and small teams needing L4 plus L7 are secondary.

## 3. Problem and competitive position

The product hypothesis is that automatic HTTPS, L4/L7 scheduling, static serving and disk caching can share an implementation without inheriting the configuration and operational costs of assembling several servers. Relevant strengths are nginx static serving and disk cache, HAProxy scheduling/health/runtime control, and Caddy automatic HTTPS and transactional configuration. Avoid surprising DSL semantics, silent loss of runtime changes and an unauthenticated management socket available to every local process.

Integration itself is not an empty market. [Zoraxy](https://github.com/tobychui/zoraxy) lists reverse proxy, load balancing, ACME including DNS challenges, TCP/UDP forwarding and a static webroot. [River](https://www.memorysafety.org/initiative/reverse-proxy/) has supported static serving. nginx offers [dynamic upstream resolution](https://nginx.org/en/docs/http/ngx_http_upstream_module.html) with explicit `resolve`, `resolver` and `zone`, and an [ACME module](https://nginx.org/en/docs/http/ngx_http_acme_module.html). HAProxy provides a [memory cache](https://www.haproxy.com/documentation/haproxy-configuration-tutorials/performance/caching/). Traefik, APISIX, Kong, Nginx Proxy Manager and Kubernetes Gateway implementations are adjacent products.

The comparison is integration shipped by default (L4, files, disk cache, HTTP/3 and three ACME challenges without custom plugins/builds), configuration governance (DSL and JSON inputs, atomic load, visible temporary overrides), and operating cost. These are comparison dimensions, not claims of unique features or superior performance. Language choice cannot establish performance; §8 defines the required evidence.

## 4. Design principles

1. One process, one configuration, one observability system. Requiring another installation needs a product justification.
2. Adopt existing strengths while identifying the corresponding failure mode being avoided.
3. Expected state is the sole persistent authority; temporary exceptions remain visible.
4. Validate before activation; configuration changes are transactions with complete rollback.
5. Secure defaults: local management and explicit admission for dangerous capabilities.
6. Performance claims require reproducible end-to-end measurements and public raw data; never claim “N times faster”.
7. Fallback was temporary scaffolding. A change making it harder to remove violates its purpose; it has been removed (G98).

## 5. Technical foundation

| Area | Baseline |
|---|---|
| Language/runtime | Rust and Tokio; audited libraries implement security-sensitive protocols. |
| HTTP foundation | Vendored Pingora 0.9.0: HTTP/1 and HTTP/2, connection pooling and listener transfer. Routing, load balancing, health checks and cache semantics are Fulcrum code. |
| HTTP/3 | `quiche::h3`; custom sans-IO event loop, connection ownership and shared routing. QUIC, HTTP/3 framing and QPACK remain library responsibilities. |
| TLS | BoringSSL throughout inbound TLS, L4 ClientHello inspection and outbound HTTPS. |
| Configuration | Caddyfile-style DSL compiles to JSON, the sole internal configuration representation. |
| Distribution | Linux x86_64/aarch64 static musl binary, systemd unit, deb/rpm. No official container image; a `FROM scratch` example is sufficient. |

### 5.1 Architectural constraints

Both h1/h2 and h3 use the same `SslContextBuilder::set_select_certificate_callback`. Do not split certificate selection. BoringSSL also backs the L4 pre-read handshake machine and the custom outbound `tower_service::Service<Uri>` connector supplied to `instant_acme::HttpClient`; implementing that connector trait does not compose Tower middleware into Pingora phases.

Pingora's upstream `ProxyHttp` phase model does not accept Tower `Service`/`Layer`. Fulcrum uses `HttpServerApp`/`ServerSession`, and does not depend on `pingora-proxy`.

Custom TCP/UDP services must explicitly obtain and return listener descriptors through Pingora's transfer table. M0 proves this integration is possible; it does not make arbitrary custom services participate automatically.

Lock contents, dependency graphs and linked artifacts answer separate questions. Optional Rust TLS packages may remain in `Cargo.lock` without appearing in the activated graph. The musl product gate additionally checks the symbol table for BoringSSL-specific symbols and absence of OpenSSL/rustls implementation symbols; `rustls_pki_types` is permitted. That artifact check does not cover the glibc development binary.

## 6. First-release scope

### 6.1 Included

L7 reverse proxy (HTTP/1.1, HTTP/2, TLS termination, WebSocket and gRPC); L4 TCP/UDP, SNI/ALPN routing and PROXY protocol; static files with ranges, validators, compression/precompressed files and indexes; a fully self-built HTTP cache including disk storage; and HTTP/3 sharing the HTTP execution chain. “Self-built HTTP/3” means entrypoint, ownership, routing and middleware integration, never the QUIC/QPACK protocol implementation.

### 6.2 Excluded

Web UI; Consul/etcd/Docker-label service discovery; Kubernetes endpoint watching/Gateway API; OpenTelemetry tracing; Windows/macOS; a distributed control plane, RBAC or approval workflows; and official container images.

### 6.3 Removed fallback

The temporary Caddy/nginx forwarding layer was removed once L4, files and cache had native implementations. `fallback_nginx` and `fallback_caddy` now fail with `FUL-DSL-0034`. Durable lessons remain in [fallback](docs/architecture/fallback.md): distinguish unconfigured capability (501) from unreachable upstream (502), report unused configuration, reuse one forwarding implementation, and retain self-loop warnings when deleting a former consumer.

## 7. Milestones and outstanding work

| Milestone | Scope | Exit |
|---|---|---|
| M0 | Native HTTP plus custom raw TCP/UDP services, listener fd transfer; intentionally exclude QUIC to isolate the seam. | Zero interruptions across `SIGQUIT` and the new `-u` generation. |
| M1 | Configuration-to-runtime path, reverse proxy/basic balancing, automatic HTTPS including wildcard issuance and renewal, DNS refresh, active checks, atomic load and product systemd integration. | One real server serves through Fulcrum and begins a pressure smoke test; the former seven-day requirement is removed (G73). |
| M2 | L4, files, complete memory/disk cache, compression, HTTP/3, fallback removal, logs, metrics and runtime control/stats. | Two machines serve through Fulcrum; implementation alone does not satisfy this exit (G81). |
| M3 | Reproducible same-host comparisons in every §8 category. | Every applicable category meets §8 and third parties can reproduce scripts and raw data. |
| M4 | User/reference/migration/security-disclosure docs, systemd and deb/rpm packaging. | At least 90 continuous production days and M3 acceptance. Public source does not satisfy this release exit. |

M3 methodology gates are implemented, but green synthetic/development checks are not qualified performance results. The historical sets in `bench/results/` retain their measured commits and limits. Reuse-aligning upstream connections made window 4's reverse-proxy best/threshold comparable; cross-window changes cannot be attributed solely to Fulcrum because competitor drift remains a confounder. The `ws-off` diagnostic variant is not an M3 product reading. Turning work stealing off improved some categories but hurt short connections and proxy latency; G35 remains in force and the mechanisms remain unexplained.

Outstanding M3 work and constraints:

- Obtain qualified readings for the remaining categories, including TLS handshakes (the case and G168 controls are implemented). Define long connections. L4/cache-hit category designs must decide how to judge the missing Caddy competitor under G152.
- G36 freezes build-compiler upgrades until M3 ends; it became active at the first qualified reading. G152 freezes the entire benchmark image, including competitors and generator.
- Keep the disk-minus-memory hit-p99 diagnostic, but apply no millisecond threshold (G151). Only the eventual cache-hit category may reopen D20; it remains closed meanwhile.
- Static-file cache option 3 is suspended pending HTTP-layer investigation. It cannot alone close the measured static gap, nginx in the comparison has no `open_file_cache`, and a default-off feature would not change untuned benchmark results. Its four unresolved design choices remain unresolved.
- User-space file-path/response allocations, metrics registry contention and selected Pingora parsing/serialization overhead have been addressed. Their local instruction/allocation diagnostics are not performance claims. A qualitative attribution found the extra qualified-host gap in the Pingora/Tokio foundation rather than Fulcrum's own layer; the private attribution readings are not published.
- Keep `SharedRuntime::current()` on the hot path after G164's withdrawal. Before another snapshot-cache attempt, attribute the approximately 8 µs diagnostic regression (35a9c5f versus e8adcd8; data local and untracked, not a performance claim) on a machine with enough cores and hardware PMU access. Account for old-graph destruction during a `RefCell` borrow if a destructor can reenter snapshot acquisition.
- Header serialization-buffer reuse, G155's combined header/body write or explicit flush mitigation, the remaining kernel-side file-path cost, the residual per-request `within_root()` blocking transition, and connection-pool behavior after Pingora 0.9 need investigation. No implementation authorization or schedule follows from this list.

## 8. Performance acceptance (G19)

Compare Caddy, HAProxy and nginx on the same machine with the same cases and load generator. Categories are static throughput, reverse proxy throughput, TLS handshake, L4 forwarding, cache hit, long connections and high-concurrency short connections. Fulcrum must be within 10% of the strongest applicable competitor in each category. Publish scripts and raw data. Host qualification, invalidation and scope caveats are part of the result; development diagnostics never substitute for these measurements.

## 9. Risks and mitigations

| Risk | Required treatment |
|---|---|
| Network-path security with a small team and AI assistance | Rust for memory safety; audited TLS, HTTP/2, HPACK/QPACK and QUIC libraries; never implement those protocol stacks by hand. |
| Disk-cache crash recovery | Atomic writes, read-time verification/discard and incremental rebuilding; correctness tests must detect wrong content, not merely hit rate. |
| On-Demand issuance abuse | Mandatory admission, issuance rate bounds, failure backoff and alerts. |
| Unreproducible benchmarking | Script environment, CPU affinity and kernel limits; retain the full environment snapshot with raw data. |
| Maintaining the Pingora fork | Record each delta in FORK.md, monitor releases and unmerged upstream work, rerun the vendor regression suite and integration gate after rebase, and full comparisons when required by M3. Dependency relaxation is not proof of unchanged behavior. |
| Distribution/license obligations | No Caddy/nginx static linkage; the fallback layer is gone. Review packaging obligations before release. |

Resolved risks remain constrained by their fixes: QUIC DCID relay handles shared UDP sockets; Alpine-native musl builds and qemu cover aarch64 without adding a C/C++ cross-toolchain; process cleanup returns occupied ports; derived shell inventory guards unscanned script roots; Pingora 0.9 replaces the old global listener lock with short/per-address locking. These do not justify weakening the corresponding gates.

## 10. Decisions

G-identifiers are append-only. A superseded choice is identified explicitly. Product decisions remain binding; discussion transcripts and owner wording reviews are local governance records. The following table retains technical conclusions and their constraints.

| ID | Decision |
|---|---|
| **G1** | Implement in Rust for memory safety and performance control; performance still requires §8 evidence. |
| **G2** | Fulcrum owns the data plane; originally forward unfinished capabilities to mature engines, then take them over individually. Fallback is now removed by G98. |
| **G3** | The first release covers the complete web-server/proxy/load-balancer capability surface, rather than only proxy plus automatic HTTPS. |
| **G4** | Use a custom Caddyfile-style DSL. |
| **G5** | “Complete” refers to the external capability surface; implementation could originally replace fallback in stages. |
| **G6** | Pingora foundation with a separate QUIC entrypoint and §5.1 constraints. The current base is 0.9.0; G103/G104 replace the original quinn/h3/rustls selection. |
| **G7** | Build the L4, static-serving, disk-cache and HTTP/3 integration capabilities. G82/G105 clarify cache and protocol ownership. |
| **G8** | Two control channels: atomic full load and immediate incremental runtime operations without reload or connection interruption. |
| **G9** | Fallback was temporary, removed per capability and gone by 1.0; no capability/semantic translation layer or second product. |
| **G10** | GPL-3.0 source and M4-governed release. G115 permits earlier source visibility without changing release exits. |
| **G11** | DSL and structured JSON are both public inputs; JSON is the sole internal fact for diff/versioning/atomic rollback. No JSON-to-DSL round trip. |
| **G12** | Automatic HTTPS by default; support On-Demand TLS subject to G15. |
| **G13** | Linux x86_64/aarch64 static musl binary, systemd, deb/rpm; no official container image. |
| **G14** | Management defaults to a filesystem-controlled Unix socket; remote management requires explicit mTLS. The present implementation rejects network admin addresses. |
| **G15** | On-Demand TLS requires an ask endpoint or domain allowlist at startup. |
| **G16** | Observability comprises Prometheus metrics, structured access logs and live runtime stats. |
| **G17** | Static/DNS upstreams, periodic DNS refresh, active health checking and passive circuit breaking. nginx's limitation is its default, not absence of optional dynamic resolution. |
| **G18** | Runtime modifications are a nonpersistent override layer, always visible through API responses and stats. Expected state remains the only persistent authority. |
| **G19** | All three competitors, per-category strongest-minus-10% threshold, public scripts and raw data (§8). |
| **G20** | Site-address-first brace syntax; avoid nginx-like syntax that invites incompatible semantic assumptions. |
| **G21** | Product name: Fulcrum / 樞衡. The Chinese name combines the ideas of a pivot/gateway and balance; Fulcrum carries both senses. |
| **G22** | The owner's fleet and self-hosting/homelab operators are equally primary users; docs, defaults and migration tools serve them first. |
| **G23** | No first-release Web UI; DSL, CLI and API plus external Prometheus/Grafana consumption suffice. |
| **G24** | Milestones follow usable deployment increments with explicit exits (§7). |
| **G25** | Historical fallback targets: nginx for files/cache, Caddy otherwise. Extra temporary implementation cost was accepted; superseded operationally by G98. |
| **G26** | Docker Desktop/Linux containers are the development validation environment. |
| **G27** | M0 isolates custom raw TCP/UDP fd transfer; defer QUIC so protocol failures cannot mask seam failures. |
| **G28** | Defer HTTP/3 library selection to M2; closed by G103. |
| **G29** | Track current dependency releases, including breaking majors, with a 24-hour quarantine. Use dep-check, never bare cargo update; adoption requires the applicable complete validation. G153 was one bounded exception. |
| **G30** | Fork pingora-core to relax dependency ceilings; maintain each approved additional delta explicitly. |
| **G31** | Foreground systemd Type=notify process; remove daemonize. G37 corrects the generation/MainPID model. |
| **G32** | Offer advisory-backed dependency relaxations upstream and maintain the rest by rebase. |
| **G33** | systemd owns standard configuration/state/runtime/cache directories; every path remains configurable. |
| **G34** | Historical external engines use existing units through Wants/After; Fulcrum never starts, supervises, restarts or reads another engine's configuration. |
| **G35** | Keep work_stealing=true; set overridable defaults by service role. Per-core alternatives require M3 evidence. |
| **G36** | Weekly dep-check reports build rustc changes, never auto-applies them; freeze the compiler during M3 comparisons. |
| **G37** | Type=notify, foreground and ExitType=cgroup; never transfer MainPID. Reload reads the current generation's pid file. Requires systemd >=250. |
| **G38** | Fix both get_fds_from fd leaks in the fork; upstream submission is a separate external action. |
| **G39** | Weekly dependency reporting also tracks the systemd host image's major systemd version. |
| **G40** | Upstream publication requires explicit owner authorization; identity and prepublication checks remain required. |
| **G41** | Fix the two pingora-rustls manifest defects in the fork. |
| **G42** | Cover rustls in vendor regression tests and repair test prerequisites instead of growing expected-failure lists. |
| **G43** | Fork audit commands must cover the whole intended set rather than a partial manually selected subset. |
| **G44** | Group composed shell gates: left-associative &&/|| must never swallow a failing lint exit. |
| **G45** | Replace rustls-pemfile with rustls-pki-types PemObject. |
| **G46** | Check existing upstream code, open issues and unmerged PRs before preparing/publishing a submission. |
| **G47** | Historical unfinished capabilities used implicit fallback, with no DSL fallback directive; G98 removes the mechanism. |
| **G48** | Structured configuration is JSON, including the on-disk and load-API representation. |
| **G49** | Built-in directive order, with public table, order-preserving route blocks, exhaustive placement of new directives and position-explaining diagnostics. |
| **G50** | Named @matchers plus path-only inline shorthand. |
| **G51** | Aggregate diagnostics with line/column, source excerpt/caret and stable FUL-DSL-NNNN codes whose meanings never change. |
| **G52** | Historical implicit fallback routes were individually reported at startup/load. |
| **G53** | instant-acme implements ACME protocol; Fulcrum owns challenge solving, certificate storage/scheduling and admission. Disable defaults and explicitly select ring. |
| **G54** | TLS-ALPN-01 primary, HTTP-01 fallback and DNS-01 support. |
| **G55** | Per-domain PEM storage, atomic replacement and cross-process issuance locking. |
| **G56** | ARI renewal window first; otherwise renew in the final third of certificate lifetime. Bounded exponential failure backoff with jitter. |
| **G57** | Native Cloudflare and DNSPod; exec hooks for other DNS providers without a plugin framework. |
| **G58** | Wildcard issuance and renewal are M1 exits. Poll authoritative nameservers for TXT visibility; fixed sleeps are insufficient. |
| **G59** | Verify credential/zone scope when possible, require explicit zones otherwise. Literal credentials were subsequently permitted with load-time permission checks, redacted output and rejection of redacted load input; see the TLS/DSL baseline. |
| **G60** | M1 DSL describes the complete external first-release capability surface, while unwired capabilities remain explicitly visible. |
| **G61** | Small fixed placeholder set; no expressions, functions or conditionals. |
| **G62** | One configuration file; no import or conf.d in M1. |
| **G63** | Built-in error defaults with handle_errors overrides; no site=421, no route=404, no healthy upstream=502. |
| **G64** | Use pinned local Pebble/challtestsrv in ACME gates rather than public CA/rate quota. |
| **G65** | Renewal failure state is independent of certificate presence, including domains that never successfully issued. |
| **G66** | Single-label wildcard semantics shared by routing and certificate selection through one implementation. |
| **G67** | The short-read upstream submission is published; publication history is an operating record outside the product contract. |
| **G68** | The with_cert_resolver submission is withdrawn; do not revive it implicitly. |
| **G69** | Implement the small DNS client directly without adding a crate, based on dependency-cost measurement. |
| **G70** | A separate renewal scenario must exercise the running product's scheduler, not merely initial issuance. |
| **G71** | Implement RFC 8737 TLS-ALPN-01 with isolated challenge certificates and CA-side success assertions. |
| **G72** | Native Cloudflare/DNSPod integration adds no HTTP-client package. |
| **G73** | M1 exits on real-server deployment plus pressure-smoke start, replacing the former seven-day condition. |
| **G74** | POST /renew forces renewal timing but never bypasses failure backoff. |
| **G75** | admin unix/<path>, mode 0600; absent admin creates no management socket; a network address rejects startup. Initial endpoints are /load and /renew. |
| **G76** | DNS resolution results live in upstream slots and refresh periodically. |
| **G77** | Wire health_uri active checks. |
| **G78** | Product systemd integration must notify readiness, atomically write its pid before READY, handle SIGUSR2 fork/exec then SIGQUIT, and use explicit 30s drain plus 5s shutdown. Preserve inherited admin-socket pathname on upgrade. |
| **G79** | Historical file/cache forwarding actually used the shared proxy path; unconfigured=501, unreachable=502. Removed by G98. |
| **G80** | Historical M1 real-domain selection, superseded by G85/G86; concrete deployment values are local operating data. |
| **G81** | M2's operational exit is two machines rather than five. |
| **G82** | Build the complete HTTP cache; do not vendor pingora-cache and its additional dependency surface. |
| **G83** | Two-level hashed disk directories, one metadata file per primary-key family and one body file per variant; temporary files plus atomic rename. Revalidation updates metadata without rewriting body. |
| **G84** | No startup disk scan; verify on read and incrementally rebuild eviction state. Initial undercount delays eviction only. Purge uses management. |
| **G85** | Historical production-domain replacement, superseded by G86; no ongoing product contract. |
| **G86** | Final production-domain selection is deployment data, not a portable product default. |
| **G87** | Follow static-file symlinks by default; follow_symlinks false provides structural containment. |
| **G88** | Configurable named hide list rather than rejecting every dot segment. User hide entries append; hide_defaults false disables defaults. |
| **G89** | Full file-serving slice: files, index (default index.html), directory trailing-slash 301, ranges, conditional requests and precompressed siblings. |
| **G90** | A small built-in MIME table, no new package; unknown extension=application/octet-stream. |
| **G91** | root is required and absolute. |
| **G92** | Distinguish SelfBuiltM2 ownership in plan output; documentation ownership is not limited to M1. |
| **G93** | Parse all three If-Modified-Since formats, including RFC 9110's RFC 850 two-digit-year rule. |
| **G94** | Workflow shell logic belongs in checked scripts, including CI cache dumping. |
| **G95** | Implement cache semantics/memory/purge/coalescing before disk storage. |
| **G96** | ttl is an upstream-freshness fallback, never an override. |
| **G97** | Support the minimum cache semantics plus the full upstream Cache-Control directive set. |
| **G98** | Remove the whole fallback layer once its three consumers are native; obsolete options receive FUL-DSL-0034. |
| **G99** | Compression and precompressed files form the next implementation slice. |
| **G100** | Reuse existing fork compression rather than increasing the graph with a second implementation. |
| **G101** | Compress before storage and normalize the secondary variant key. |
| **G102** | Weaken compressed ETags to W/"...". |
| **G103** | Close D11 with quiche: integrated HTTP/3 semantics, sans-IO and public header parsing permit DCID ownership routing; quinn's reset of unknown DCIDs would kill another generation's connections. |
| **G104** | Unify all TLS on BoringSSL, including restored pingora-boringssl; replace the former rustls-only constraint instead of maintaining two TLS stacks. |
| **G105** | Use quiche::h3; hand-written QUIC/QPACK violates the security baseline. |
| **G106** | Withdrawn entirely by G107: host Rust installation/build guidance is not permitted. |
| **G107** | Run Rust only in Docker; remove host-generated Rust build artifacts and use the repository gate. |
| **G108** | Prove the actual product's musl static binary with validate in FROM scratch; the old probe remains historical and is not gate evidence. |
| **G109** | Route QUIC datagrams by DCID generation prefix over Unix datagram relay sockets <run_dir>/quic-relay-<gen_id_hex>.sock. Messages contain source/destination/original datagram; one hop only, drop if the owning generation exited. |
| **G110** | TLS automatically enables same-port UDP HTTP/3 and h1/h2 Alt-Svc; h3 must share the same app/router/upstream pool. |
| **G111** | fulcrum-server may directly depend on boring for HMAC/AEAD rather than obtaining every BoringSSL type through pingora-boringssl. |
| **G112** | Implement logs and HTTP PROXY reception before cross-generation relay so relay behavior is observable. |
| **G113** | Access logs have one flat JSON format. |
| **G114** | The fixed/conditional field contract is defined in observability.md; missing optional transport data is omitted, never invented. |
| **G115** | Source visibility may precede M4. Keep handoff ignored and docs/comments focused on conclusions. The historical initial-publication history operation is not authorization to rewrite history again. |
| **G116** | metrics is a site-block terminal (order 75), reusing matchers/TLS/logging without a listener or auth system. It is not the admin surface; a mistaken matcher can expose it. |
| **G117** | Implement Prometheus text exposition without new dependencies; tests own bucket arithmetic, escaping and counter suffixes. |
| **G118** | no_site_match gets a counter; only configured host literals become host label values, all others use <other>. Unknown visitor hosts remain in logs rather than unbounded metric labels. |
| **G119** | Implement incremental control, overrides, then stats; stats includes overrides from its first version. |
| **G120** | POST /load?overrides=keep\|clear requires an explicit query value and no JSON envelope. Missing/invalid returns 400 before changes. clear reports every removed override; keep retains and identifies dangling keys. |
| **G121** | Request site labels use the actually matched address literal, including the wildcard literal; never visitor host or first block address. |
| **G122** | TLS metrics count requests by server-negotiated version/cipher, not handshakes or visitor-controlled SNI/ALPN. Connection total/active use listener/entrypoint; instrument native HTTP/admin plus independent L4 TCP, L4 UDP and QUIC accepts. One shared ConnStats and Drop decrement avoid split exit logic. Fork exposes hooks/counts, not a Prometheus implementation. |
| **G123** | Cache events are hit/miss/stale with one request denominator; purged entries get their own family. Count miss on successful origin contact, not initial lookup miss; coalesced followers can become hits and only-if-cached can return 504. Upstream connection failures are excluded, biasing hit ratio during failure; consult health/error metrics. Backend kind is process-constant, not a label. |
| **G124** | status_class has six values, including none for status 0, 1–99 and 600+. status 0 is reachable when header flush fails (RST plus slow upstream without Content-Length); buffered success does not prove delivery. The e2e gate checks log status=0 and none increment, with a normal-200 control. |
| **G125** | Override key=(site name, optional reverse_proxy id, normalized upstream address). Collisions deliberately share one slot without warning; use id to separate. Stats reports affected-route count. Never derive id from content or positional order. |
| **G126** | fulcrum_overrides_active is one unlabeled gauge including dangling entries, from the same entries source as /stats. Scrapes reuse their held runtime snapshot, never acquire a second inconsistent snapshot. |
| **G127** | Empty negotiated metric label values become <unknown>; apply to both TLS version and cipher regardless of transport. Empty labels are equivalent to missing labels in Prometheus. Logs instead omit unavailable optional fields. <other> bounds visitor values; <unknown> reports unavailable server data. |
| **G128** | Fork SslDigest gains public sni/alpn populated in from_ssl. Use TlsSettings::from(builder), never a data-plane TlsAccept callback; callbacks otherwise force an unconditional suspend/resume on every handshake. h3 constructs the same digest type and shares log finalization; quiche's unavailable cipher is omitted in logs and <unknown> in metrics. |
| **G129** | Override set_at_unix is last modification's absolute wall-clock Unix time, including repeated same-value sets; store SystemTime rather than integer seconds, accept NTP ordering limits, add no second monotonic timestamp or metric. Emit self-loop warnings from server load summaries, including nested routes and negative controls. id is 1–64 characters [A-Za-z0-9_.-], validated by one helper in DSL and JSON build paths (FUL-DSL-0043/400); this prevents sentinel collision and broken-line output, not metric cardinality. |
| **G130** | One outcomes! literal token generates constants and closed set; OutcomeName's private field lives in its own module; every serve/error return yields an OutcomeName. Bare strings, missing outcome returns and undeclared values must fail compilation. No Record-side default aborted state. Duplicate literals remain checked separately. |
| **G131** | Close D21 with Alpine-native musl plus qemu aarch64, avoiding another C/C++ cross-toolchain. Apply crt-static only to the final crate because bindgen build scripts need dlopen. |
| **G132** | Establish methodology using one category against all competitors before claiming numbers. Windows/Docker Desktop with interfering proxy and production-shared CPUs are unqualified. Methodology deliverables are scripts/environment/verdict criteria; a qualified result is a separate exit. |
| **G133** | Inventory all Docker volumes without a name filter; subtract attributed, explicitly foreign-prefix and exactly-64-hex anonymous volumes. Report remaining names/sizes without deletion commands. Foreign prefix lists can become stale and cannot prove ownership; exact field matching prevents another volume's size being claimed. Synthetic and set-level controls must distinguish empty from unexecuted reporting. |
| **G134** | COMPILE_ONLY includes shellcheck plus cargo test --no-run --workspace --all-targets, grouped so failures propagate. It excludes tests/fmt/clippy; G158 controls shellcheck triggering. |
| **G135** | Cache capacity changes really apply on /load to both memory/disk backends and response reads back effective capacity. Shared effective_capacity takes the maximum of blocks. Shrinking is lazy until next put, never an O(n) load-time LRU sweep. |
| **G136** | Passive failures include connect/timeout and real upstream 5xx, never synthesized Fulcrum 5xx. Keep breaker state separate from active healthy. After cooldown permit exactly one half-open probe through CAS on open_until, not a separately claimable token that can strand an unselected upstream. passive_fail absent disables; positive threshold required, invalid/zero=FUL-DSL-0044. Defaults at config layer: window 10s, cooldown 30s; knobs without threshold warn FUL-DSL-0045. Stats reports passive_open/fails; merged passive-open uses OR; log only transitions. No retry after a request is sent. |
| **G137** | Automatic HTTP redirect stays fixed on :80 because HTTP-01 requires it. Deployments with a front proxy occupying :80 must yield it or perform redirects there; no alternate redirect-port feature. |
| **G138** | The musl product gate inspects symbols: all three BoringSSL-exclusive markers present, OpenSSL-exclusive symbols absent, rustls implementation absent using v0 crate-length matching that permits rustls_pki_types. Require a Fulcrum symbol witness; stripped/missing symbol table fails. Non-FIPS BORINGSSL_self_test is not a valid witness. Glibc is outside scope. |
| **G139** | Unqualified hosts produce raw data and UNQUALIFIED, with no strongest/threshold/PASS/FAIL. Qualification combines machine-checkable environment with explicit dedicated-host/no-TUN attestation, which cannot prove its own truth. Benchmark delivery stays in bench/, one gate in tests/bench/. Competitors have a separate digest-pinned image and identical pinned oha across images. HAProxy's fixed-body static case does less file work; preserve that caveat. Only deadline-aborted requests <=1% of successes are tolerated, not arbitrary errors. Shell inventory includes tracked and unignored untracked scripts, excludes ignored handoff, and witnesses a nonempty scanned set. |
| **G140** | Three DSL thread keys [1,1024], FUL-DSL-0046 for invalid input. One default_threads source: L7=CPU count, L4=2, admin/background=1. h3 is L7; threads_admin is ServerConf fallback. Per-service runtimes sum, so no generic global threads knob. Moving from old implicit 1 to CPU-count L7 changes resource use. Defaults await M3 validation; OS-thread e2e assertions prove wiring. |
| **G141** | Classify breaking changes across DSL, structured JSON, stats JSON, metrics and diagnostics; Rust APIs are internal (publish=false). Classification applies now, compatibility promises start at M4. Build identity is <package-version>+metadata from FULCRUM_BUILD_ID, direct .git reading or explicit unknown. No dirty-tree detection; release builds must supply explicit identity. |
| **G142** | Invalidate generator/external-ceiling convergence before emitting a result: full-field spread below BENCH_MIN_SPREAD=0.05; optionally strongest above ceiling.txt*BENCH_GEN_HEADROOM=0.90. Never name ceiling input .json (it would become a competitor). Insufficient/invalid data is VOID. These numeric parameters are judgments, not validated measurements; the absolute-ceiling path remains unexercised on qualified data. G146 adds top-pair handling. |
| **G143** | PLAN §1 summary's backticked keys must still be UNWIRED; test only this direction because prose summary is not exhaustive and HTTP PROXY sending has no DSL key. Bound the summary explicitly and keep positive/negative synthetic controls plus stale-key injection. |
| **G144** | aarch64 product validation triggers on root Cargo.lock or derived docker/* files with FROM rust:, not source-only changes or benchmark/systemd images. Missing prerequisites fail rather than skip. arm64-verified.txt is generated only after real success; changing source-trigger assumptions needs reassessment after an actual source-induced regression. |
| **G145** | Verify benchmark limits where they act: container sysctls for somaxconn, tcp_max_syn_backlog, ip_local_port_range and tcp_tw_reuse; host netdev_max_backlog; container nofile. One declaration feeds flags and assertions, including whitespace-safe tokenization. Remove fs.file-max lowering; tcp_tw_reuse=2 matches loopback. Host attestations still do not prove dedication/no TUN or adequacy of backlog sizing. |
| **G146** | Close D34 with top-two convergence only for provisional PASS: a ceiling cannot make a FAIL falsely fail because the true threshold would be higher. Compute before output, emit no invalidated PASS anywhere. Include Fulcrum in top-two selection; invalid/insufficient data is VOID and exact boundary counts as separated. 0.05 itself remains unvalidated. |
| **G147** | Define duration/connections/workers/payload defaults once in bench/lib.sh (10s/50/1/4096). Snapshot actual effective values. Missing shared defaults must fail, not silently diverge. Historical raw snapshots are immutable; replace defective sets with new qualified runs rather than backfilling. |
| **G148** | Keep a correctness test proving probe_file returns bytes when RWF_NOWAIT is unsupported. The separate one-versus-three blocking-transition cost has no permanent performance gate; accept that diagnostic-only boundary until qualified respond data and static-cache design justify a threshold. Syscall counts would be the wrong unit for this question. |
| **G149** | Product versions stay 0.x before M4, 1.0 when compatibility promises begin. Product crates are 0.1.0 but not separately published; spike crates remain 0.0.0. Diagnostic meanings never deprecate. Metrics/stats transitions require >=6 months and >=1 minor; DSL/JSON >=1 minor with a clear warning. Revisit before M4 if release cadence makes these proposed durations unsuitable. |
| **G150** | Disk request-path I/O stays synchronous and shares MemStore's seven-operation shape; no speculative async/block_in_place split. Record disk-hit p99 minus memory-hit p99 in M3 diagnostics. Reopening rules are superseded by G151. |
| **G151** | No millisecond threshold on saturated closed-loop disk-minus-memory p99: it mostly measures throughput-derived queueing, not tail latency. Continue recording; only the future §8 cache-hit category decides whether to reopen D20, alongside missing-competitor treatment. |
| **G152** | Freeze the entire benchmark image during M3: no additions, removals or upgrades of servers/oha/python. L4 and cache-hit must therefore lack Caddy and explicitly define the resulting comparison rule. This does not freeze the systemd host image or change G36's compiler rule. Even comments changing frozen Dockerfile hashes can trigger rebuilds; avoid them. |
| **G153** | A single historical exception admitted thiserror/thiserror-impl 2.0.21 inside quarantine to resolve an advisory. It did not change G29; later adoption still uses 24 hours. Historical verification outputs were not retained. |
| **G154** | Root-only dependency versions require cargo-tree attribution: Pingora consumer present=lock-alignment failure; none=information; unable to determine=failure. dep-check applies this rule to updated names; vendor gate covers all common names. Synthetic tests guard Python decisions; no cross-language implementation-consistency gate is claimed. |
| **G155** | L7 clear/TLS write buffer=16 KiB rather than 1460 bytes; admin, L4 and h3 unchanged. Cost is ~15 KiB per connection and potentially unbounded header delay for a slow partial Content-Length upstream body; chunked/close-delimited flush behavior is unaffected. Header+first-body coalescing/explicit flush mitigation remains unscheduled. Permanent TCP_INFO checks cover h1 clear/TLS 4 KiB one-segment responses with a 32 KiB negative control; h2 is not separately measured. |
| **G156** | Event metrics use 64 write shards, 128-byte aligned; gauges stay global. Borrowed label-key lookup removes steady-state allocations; validate type/arity before locking and again inside. Scrape merges shards with unchanged family/series order and exposition. It is not a global atomic snapshot; histogram sum low bits can differ, series storage can have 64 copies, >64 threads share shards. |
| **G157** | Fork change 16 removes L4 pending-time recording (getters now always zero), avoids filling 256 header slots, and stores offsets without a per-request Vec. Same parser/config and header-count bound; Fulcrum/fork had no pending-time readers. Case tables and idle timers change behavior and require separate decisions. |
| **G158** | Pre-push shellcheck depends on net tree changes in *.sh, *shellcheckrc or docker/Dockerfile.build; unknown remote/diff failures run it. Compile remains mandatory. Volume reporting stays in full gate; G159 governs host self-tests. Checked files are the working tree, so dirty-tree compilation cannot certify the pushed commit. |
| **G159** | Host self-tests in pre-push run when script/tool fingerprint changes or no matching worktree-local success stamp exists. Fingerprint includes docker-run.sh, all tests/lib/*.sh, Bash/MSYS/Git identity. Remove stamp before running; only success rewrites it. Full gate always reruns. Unfingerprinted tool changes or broken match logic remain blind spots until full validation. |
| **G160** | One Tokio Sleep per keep-alive connection carried in user_context. Bound total wait from starting the next request to complete request headers by 60s; immediate reads avoid timer work, pending reads track start and rearm only as required. Skip non-keepalive/unlimited/h2/h3. Implement through narrow Conn in Fulcrum, no fork or premature Downstream/log timing. Slow trickled headers can close earlier than the old per-read budget; light-load kernel wait behavior is a known cost. |
| **G161** | Respect Pingora's no-keepalive determination after reading headers (HTTP/1.0 without opt-in, Connection: close, TE+CL), and always close during shutdown; Fulcrum only supplies duration when continuing. Unit/wiring tests plus raw reverse-proxy e2e requests cover the actual path. |
| **G162** | Fork title-case table adds/exports Allow, Alt-Svc, Content-Range, ETag, Last-Modified, Location and Vary. Self-built h1 responses omit the case map only when every user-provided name still serializes as written; file_server always uses no-case build. Cache hits/admin/h3 and upstream response forwarding unchanged. Compression ETag/Vary spelling changes to ETag/Vary are accepted. |
| **G163** | Before each h1 read, disable request-name case map only on ports without any reverse_proxy, including nested/error routes and all three address indexes. One 65536-bit atomic map updates at new/swap; no second runtime lock. Default fork behavior remains preserving. Map and runtime swap are not atomic: one transition request may forward title-case/lowercase names when proxying is newly added; reverse transition only allocates extra. |
| **G164** | Withdrawn. The thread-local generation+Arc snapshot cache preserved one-request-one-snapshot and post-swap freshness but showed no qualified-host benefit and a confirmed proxy regression under one affinity layout. The A/B compared 35a9c5f with e8adcd8; readings remain local/untracked diagnostics, not a performance claim. Restore SharedRuntime::current; do not revive before attributing the additional ~8 µs on sufficient-core/PMU hardware. Historical implementation/tests/readings are archived, not current product code. |
| **G165** | Fork response writing uses static Connection header values and returns BytesMut without freezing on direct writes; the proxy task freezes once where Bytes is required. Wire bytes remain unchanged. Keep allocation and wire-byte tests in the existing counter binary. Serialization-buffer allocation itself remains and belongs with reuse/G155 option B. Preparing an upstream submission does not authorize publication. |
| **G166** | If an ARI replaces order receives a CA problem response (Error::Api), warn and retry once without replaces; network/unparseable/nonproblem failures, including a missing replaces echo, follow normal backoff. Preserve RFC 9773 DER sign bytes. Pebble 2.10.1's serial bug is handled by replacing sign-byte-bearing first-generation fixtures before renewal (at most 3), version-pinned until fixed; assert actual old-order replacement and no fallback during successful renewal. |
| **G167** | Derive every docker/Dockerfile.* containing FROM rust:. Require exact identical X.Y.Z rustc pins and all required witnesses; do not compare distro/digest. Offline dep-check --check-pins runs in lint; adding a fourth file must enter automatically. G36 still controls when to upgrade. |
| **G168** | Global tls_session_resumption defaults true and tls_groups defaults to BoringSSL; one TlsPolicy::apply_to configures h1/h2 and h3. false means zero tickets plus disabled session cache. Groups are an allowed set chosen in client order, only official case-sensitive names X25519MLKEM768/X25519/P-256/P-384/P-521/MLKEM1024; aliases, duplicates and errors reject with FUL-DSL-0047, including JSON Runtime::build. Absent settings call no BoringSSL override. /load differences return 409 (groups compared in order). TLS1.2/ECDSA and CA validation may fail under an overly narrow group list. TLS handshake comparisons pin all four to X25519MLKEM768 and no tickets; failed preflight invalidates only that category. Gates test both entrances, ticket/resumption behavior and shared-policy wiring. |
| **G169** | Diagnostic numbers in decisions/§7 require measured commit/tree identities and data location; explicitly distinguish local untracked data and mark them as diagnostics, not performance claims. §8 and G139 qualification remain unchanged. Existing entries need provenance when touched; this does not reopen private HTTP-attribution publication boundaries. |

## 11. Open questions

Only open D-identifiers appear in this table. The set is currently empty; the last D9/D20 questions closed in G149/G150. Empty does not mean M3 work or suspended designs are approved or complete; outstanding work is in §7. The [navigation mirror](docs/governance/open-questions.md) must contain exactly the same open D set.

| ID | Question | Required before |
|---|---|---|

## 12. Technical references

[Pingora](https://github.com/cloudflare/pingora) · [quiche](https://github.com/cloudflare/quiche) · [Caddy API](https://caddyserver.com/docs/api) · [HAProxy Runtime API](https://www.haproxy.com/documentation/haproxy-runtime-api/) · [nginx process control](https://nginx.org/en/docs/control.html)
