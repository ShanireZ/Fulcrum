---
type: architecture
title: TLS and automatic HTTPS
description: One BoringSSL stack, shared certificate selection and ACME lifecycle.
---
# TLS and automatic HTTPS

This is the approved technical baseline and yields to [PLAN.md](../../PLAN.md) on conflict.


## One TLS stack

G104 replaces the former rustls-only decision with BoringSSL throughout h1/h2, QUIC, L4 ClientHello inspection and outbound ACME/DNS HTTPS. Both HTTP entrances install one SniResolver through `SslContextBuilder::set_select_certificate_callback`; quiche receives the same builder. One certificate store, ALPN policy and session policy prevent h2/h3 selection drift. Use TlsSettings::from(builder), never data-plane TlsAccept callbacks: the latter force a suspend/resume on every handshake.

The outbound connector implements tower_service::Service<Uri> over pingora_boringssl::tokio_ssl::SslStream without adding a package; default verify paths and hostname verification remain library checks. ACME and DNS clients share trust/verification/ALPN. Lock contents, activated dependency graph and linked product symbols are distinct gates; rustls optional packages can remain in the lock without an activated TLS implementation. The musl artifact gate proves the distribution binary, not the glibc development binary.

L4 pre-read drives a real BoringSSL handshake machine over memory transport, copies SNI/ALPN in its early callback, then aborts. Discard all outgoing handshake bytes: Fulcrum is not the upstream TLS peer and must not emit even an alert to the real client. Parsing succeeds if SNI is readable even when a complete TLS handshake would reject the hello. Replay consumed bytes unchanged; unreadable hello uses the configured fallback.

boring 5 enables post-quantum groups by default. Larger key shares can span QUIC Initial packets; h3/relay gates must retain real handshake coverage. G168 adds global tls_groups (official names, client preference within allowed set) and tls_session_resumption. One TlsPolicy::apply_to covers h1/h2/h3; defaults make no override call. false emits zero tickets and disables server session cache. Changing either requires a new generation (/load=409). A post-quantum-only group set prevents TLS1.2, and ECDSA P-256/TLS1.2 requires P-256. Narrow groups can also block CA TLS-ALPN validation; CA supported-group sets are unverified. See [DSL](dsl-reference.md).

## Certificate lookup and storage

Explicit PEM TLS is live; unknown SNI rejects the handshake. default_sni applies only when no SNI is supplied, never as fallback for a supplied unknown name. Store the configured name rather than a certificate snapshot so later issuance/renewal is visible. Wildcards cover exactly one label, excluding apex and deeper descendants; routing and TLS share fulcrum_config::host::wildcard_covers.

Store `<state>/certs/<issuer>/<domain>/{cert.pem,key.pem,meta.json}`, private key 0600, in the configurable StateDirectory. During generation overlap both processes share the directory: write temporary files and rename atomically; use per-domain cross-process file locking to avoid duplicate issuance and quota consumption. Renewal failure state exists even without a certificate.

## ACME

instant-acme handles RFC 8555; Fulcrum owns acceptor integration, challenge solving, admission, persistence and scheduling. A high-level acceptor-owning ACME library cannot replace that model. Set default-features=false and explicitly ring; defaults otherwise introduce aws-lc-rs. Use TLS-ALPN-01 primary, HTTP-01 fallback, DNS-01 for wildcard certificates.

TLS-ALPN-01 uses ALPN acme-tls/1 and a temporary self-signed certificate with critical id-pe-acmeIdentifier containing SHA256(key authorization). It bypasses routes. Challenge and production certificate tables are isolated: acme-tls/1 consults only challenges and rejects a miss; other ALPN never sees challenge certificates. Prove success using CA records, since HTTP-01 fallback can hide a broken primary challenge.

Renew using the CA's ARI window first, final-third-of-lifetime fallback otherwise; fixed “30 days before expiry” fails short-lived certificates and misses emergency ARI rescheduling. Failures use bounded exponential backoff with jitter and alerts. POST /renew bypasses timing but not backoff. When a replaces order receives a CA problem document (Error::Api), warn and retry once without replaces; network/nonproblem/unparseable failures do not trigger that fallback. Preserve RFC 9773 DER sign bytes in serial identifiers (G166).

## DNS-01 and credential safety

Native Cloudflare/DNSPod and direct exec hooks cover providers actually needed, without a provider framework. dns and resolvers must appear together. Poll authoritative TXT visibility, not API success or fixed sleep. Resolve authority hostnames at issuance time, query every resulting address (independent propagation), and add port 53 when absent. DNS lookup failures are loud issuance failures with backoff, not compile-time network dependencies. Resolver syntax rejects invalid ports/hostnames and currently IPv6 literals.

Native providers require zones. Match on label boundaries and use the longest declared parent/child zone; reject out-of-zone writes before any API request. Zone declaration is a local permission boundary, not proof the provider token is least-privilege. DNSPod tokens are account-wide and provide no scope query. Cloudflare startup checks GET /zones?name for each declared zone, supporting user/account tokens and proving zone existence/access; DNSPod checks Info.Version. Fatal unreadable/invalid credentials reject startup; inconclusive network/timeouts log error and continue so unrelated static-cert sites stay available. Keep this availability tradeoff explicit.

Credentials may be env:, file:, direct literals or literal: values containing a colon. Unknown source-looking prefixes reject FUL-DSL-0031. Inline credentials make configuration secret: reject files with any other-user permission; 0640 root:service-group is valid, 0600 ownership by the service is not required. compile redacts by default, --with-secrets requires explicit choice and stderr notice, Secret Debug/Display redact, and redacted artifacts cannot be loaded. The existing non-English redaction sentinel is a compatibility token, not prose.

Exec receives separate argv `set|clear <record-name> <value>`, without shell interpretation or privilege elevation, with timeout/kill and bounded captured output. Wildcards strip the `*.` before forming _acme-challenge. DNSPod reports API errors inside HTTP 200 and carries credentials in the body; Cloudflare uses the auth header, requires Zone:Read plus DNS:Edit, and DNSPod free-tier TTL has a 600s minimum.

On-Demand TLS remains UNWIRED: configuration requires admission (ask/domain allowlist), but runtime admission call, concurrency limit and failure cache still need integration. Rate bounds/backoff/alerts remain required. tls internal is also UNWIRED; neither should be described as usable merely because it compiles.

## Validation boundaries

Local ACME gates use architecture-pinned Pebble/challtestsrv checksums, a root+serverAuth leaf certificate (not CA-as-leaf), and explicit IPv6 behavior. Product HTTPS, not curl alone, must trust the CA. Pebble deliberately rejects some valid nonces; retain protocol retry semantics. Separate issuance and renewal CA/state instances prevent short-lifetime fixture interference. A running scheduler must first decline premature renewal, later choose ARI, replace serial and serve the new certificate in a real handshake; changed files or continuing 200 alone do not prove activation. CA authorization reuse determines whether challenges rerun. For Pebble 2.10.1's replaces serial bug, G166's bounded first-generation fixture replacement is version-pinned and happens before measured renewal; changing CA version must force reconsideration/removal. Provider mocks prove internal request/response handling only; real-provider acceptance needs separately authorized live-domain evidence.
