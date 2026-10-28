---
type: architecture
title: DSL reference
description: Complete configuration surface, execution order, matchers, placeholders and defaults.
---
# DSL reference

This technical baseline yields to [PLAN.md](../../PLAN.md). [Configuration](configuration.md) explains the DSL→JSON→runtime layers. DSL and structured JSON are public inputs and both validate semantic constraints.

**Parsed but unwired**: `tls internal`, `on_demand`, `tracing` — these are registered in fulcrum_runtime::UNWIRED and reported at load. Explicit PEM TLS and automatic ACME are implemented. HTTP PROXY reception is implemented; HTTP upstream PROXY sending remains unsupported at runtime, although reserved proxy_protocol syntax exists. L4 PROXY reception/sending are implemented independently.

# 1. Overview

```text
# Optional global options must precede all sites.
{
    acme_email ops@example.com
    admin unix//run/fulcrum/admin.sock
}

example.com {
    encode gzip
    header /static/* Cache-Control "public, max-age=3600"
    reverse_proxy 127.0.0.1:3000
}

api.example.com {
    @internal remote_ip 10.0.0.0/8
    handle @internal {
        reverse_proxy 10.0.0.1:8080 10.0.0.2:8080 {
            lb_policy least_conn
            health_uri /health
            health_interval 5s
            dns_refresh 30s
        }
    }
    respond 403
}
```

handle must execute before the fallback respond; this is the ordered-chain contract, not incidental formatting.

## Global options

| Option | Syntax and semantics |
|---|---|
| `acme_email` | ACME account email. |
| `acme_ca` | CA directory URL; defaults to Let's Encrypt. |
| `admin` | `admin unix/<path>`; mode 0600. Omitted means no socket; network address rejects startup. |
| `default_sni` | Name used only when the client supplies no SNI. Unknown supplied names still reject; no fallback_sni behavior. |
| `auto_http_redirect` | true/false, default true. Synthesizes the :80 HTTP redirect site described below. |
| `grace_period` | Drain duration, default 30s. Final shutdown is explicitly 5s; match systemd stop budget. |
| `threads_l7` | Integer 1–1024, per L7 h1/h2/h3 service; default CPU count. |
| `threads_l4` | Integer 1–1024, per TCP/UDP service; default 2. |
| `threads_admin` | Integer 1–1024, admin and background/default services; default 1. |
| `proxy_protocol_from` | Trusted source CIDRs for all HTTP listeners; absent trusts nobody. |
| `tls_session_resumption` | true/false, default true. false disables session cache and emits zero tickets for h1/h2 and h3. |
| `tls_groups` | Allowed BoringSSL group set: X25519MLKEM768, X25519, P-256, P-384, P-521, MLKEM1024. Case-sensitive official names only; errors, aliases and duplicates reject FUL-DSL-0047. Client preference selects within the set. Absent uses BoringSSL defaults. |

Threads are not shared across services; worker totals sum. There is no generic threads setting. Invalid/zero/unit-suffixed/noninteger thread values reject FUL-DSL-0046; role defaults are initial choices awaiting M3 evidence. threads_admin fills Pingora's fallback ServerConf.threads; it is not just the admin listener. See [process model](process-model.md).

TLS contexts are startup-bound: /load changes to either global TLS option return 409 without applying anything, directing the operator to systemctl reload. tls_groups compares list order despite selection being set semantics. Post-quantum-only lists reject TLS1.2; ECDSA P-256/TLS1.2 also needs P-256. Restriction applies to acme-tls/1 CA validation and can break issuance/renewal. Absent options perform no BoringSSL override calls.

### PROXY reception and trust

For a trusted source, consume the PROXY header before application/TLS bytes and use its client address in remote_ip/logs. For an untrusted source, read zero bytes: the prefix remains application data. Reading and discarding would alter the stream and parse an attacker-controlled v2 length. A misconfigured trust list can therefore expose the prefix to an upstream, producing an explicit protocol failure rather than guessed identity. Trusted sources must send a header; otherwise close the connection. LOCAL and PROXY UNKNOWN are valid (e.g. health checks): consume the header but retain socket peer identity.

The HTTP path uses Pingora's existing 60s per-connection handshake timeout plus a 4096-byte hard buffer limit. L4 uses a separate 3s PROXY wait. Do not claim these are the same bound. Never infer identity from the leftmost X-Forwarded-For.

### Removed fallback options

fallback_nginx/fallback_caddy reject FUL-DSL-0034. L4/files/cache are native; Fulcrum neither supervises external engines nor retains a dormant fallback configuration field. See [removed fallback](fallback.md).

# 2. Site addresses

| Address | Meaning |
|---|---|
| `example.com` | Automatic HTTPS and a synthesized HTTP redirect site on :80. |
| `*.example.com` | Exactly one wildcard label; requires DNS-01. No apex or deeper-label match. |
| `http://example.com` | Explicit HTTP; no automatic upgrade or certificate. |
| `example.com:8443` | Explicit port. |
| `:8080` | Any Host on this port. |
| `a.com, b.com { ... }` | Multiple addresses share one site block. |

Routing and certificate wildcard checks use the same wildcard_covers helper. Add *.b.example.com explicitly to cover another level. IPv6 literal site addresses are currently unsupported and get a dedicated explanatory diagnostic.

Automatic redirect is a real synthesized site, visible in compile JSON and plan output:

```text
http://example.com {
    redir https://{host}{uri} 308
}
```

308 preserves method/body. An explicit HTTP block for that hostname suppresses synthesis. A :80 catchall does not suppress it, but warning FUL-DSL-0030 explains exact→wildcard→port-catchall precedence. auto_http_redirect false disables it. Port :80 is fixed (G137): HTTP-01 requires it, and challenge handling runs before routes. A front proxy occupying :80 must yield it or implement redirects there. Unmatched sites receive 421 rather than an arbitrary default site.

# 3. Execution order

The table is a public contract compared row-for-row against chain_directives! by doc_contract.rs. Site-top-level directives are stably sorted; inside route, written order applies. A new directive requires both explicit numeric placement and exhaustive compiler handling (no wildcard fallback).

| Order | Directive | Kind | Ownership |
|---|---|---|---|
| 10 | `tracing` | Middleware | Reserved |
| 20 | `header` | Middleware | M1 native |
| 30 | `rewrite` | Middleware | M1 native |
| 40 | `encode` | Middleware | M2 native |
| 50 | `cache` | Middleware | M2 native |
| 55 | `handle` | Container | M1 native |
| 56 | `route` | Container | M1 native |
| 60 | `redir` | Terminal | M1 native |
| 70 | `respond` | Terminal | M1 native |
| 75 | `metrics` | Terminal | M2 native |
| 80 | `reverse_proxy` | Terminal | M1 native |
| 90 | `file_server` | Terminal | M2 native |

Middleware accumulates. Containers dispatch their internal chain; the first matching terminal ends the request. Containers have positions after middleware and before terminal fallback so a site-level respond does not swallow a handle. route's escape applies inside its block; route itself remains order 56. FUL-DSL-0028 explains an earlier unconditional terminal and actual order; fulcrum plan prints steps and their positions.

# 4. Directives

## 4.1 Terminals

| Directive | Form |
|---|---|
| `reverse_proxy` | `reverse_proxy [matcher] <upstream...> { ... }` |
| `respond` | `respond [matcher] <status> [body]` |
| `redir` | `redir [matcher] <target> [code]`, default 302. |
| `file_server` | `file_server [matcher] [browse] { root ... }` |
| `metrics` | `metrics [matcher]`; no arguments or subblock. |

### `metrics`

```text
metrics.example:9443 {
    tls /etc/fulcrum/m.crt /etc/fulcrum/m.key
    @internal remote_ip 10.0.0.0/8
    handle @internal { metrics }
    respond 403
}
```

metrics is an ordinary site terminal reusing matchers/TLS/logging, not an admin endpoint/new listener. It returns 200 with `Content-Type: text/plain; version=0.0.4; charset=utf-8`. encode currently does not compress metrics/respond/redir; it is wired only to files/proxy. This can cost ongoing scrape bandwidth and has no runtime warning.

Keep metrics inside handle when paired with respond 403: respond's order 70 would win over top-level metrics at 75. FUL-DSL-0037 warns when no source-restricting matcher exists, including inherited handle/route conditions. remote_ip and header count as potential protection (including negation); path/path_regexp/host/method/query do not distinguish clients issuing the same request. The check cannot establish a correct CIDR or secrecy of a header value. JSON representation is `{"order":75,"matcher":...,"directive":"metrics"}` with no second directive payload key. See [observability](observability.md).

### reverse_proxy subblock

| Subdirective | Meaning |
|---|---|
| `lb_policy` | round_robin (default), least_conn, ip_hash or random. |
| `health_uri` | Enables active probes; absent means no probes even when other health defaults exist. |
| `health_interval` | Default 10s, 500ms lower scheduling bound; each proxy's interval applies individually. |
| `health_timeout` | Default 3s across connect+write+read. |
| `health_status` | Default 2xx; invalid syntax rejects load rather than reverting. |
| `dns_refresh` | Default 30s, minimum 5s. Global scheduler uses the smallest interval and refreshes all upstreams, so longer configured intervals may refresh sooner. |
| `passive_fail` | Positive failure threshold; absent disables, invalid/zero rejects FUL-DSL-0044. Counts connect/timeout and genuine upstream 5xx, not synthesized Fulcrum failures. |
| `passive_window` | Failure-count window, default 10s. Without passive_fail warns FUL-DSL-0045. |
| `passive_cooldown` | Default 30s, separate from failure window. After expiry exactly one half-open probe; success restores, failure reopens. |
| `header_up` | Modify outgoing request headers. |
| `header_down` | Modify final response headers. |
| `transport` | http (default) or https. |
| `tls_insecure_skip_verify` | Skip upstream HTTPS certificate verification for explicitly intended self-signed internal services. |
| `id` | Optional stable route identity, one value 1–64 characters [A-Za-z0-9_.-]. Duplicate lines reject FUL-DSL-0041, explicit empty rejects FUL-DSL-0042, invalid domain rejects FUL-DSL-0043. |
| `proxy_protocol` | Reserved HTTP-upstream syntax, v1/v2 (omitted value=v2); no current HTTP data-plane sending implementation. L4 sending is independently usable below. |
| `weight` | `weight <exact-upstream-token> <integer>`; default 1, range 1–65535. |

Passive state and active healthy are independent: active /health success cannot clear a business-traffic breaker. Stats and metrics expose both; log breaker transitions rather than every failure. No request is retried on another upstream after being sent. A single half-open probe bounds repeated failure cost even under concurrency.

Override addressing is (site name,id,normalized upstream). Omitted id is empty; collisions deliberately share a slot without warning, including backend and backend:80 after normalization. Stats reports exact keys and reference count; add id to separate. Cross-site keys never merge; duplicated same-address tokens in one route count once. Do not derive identities by order or content hash. The id restriction avoids reserved <none>/<other>/<unknown>/<undeclared> collision and multiline output, not metric cardinality; validate through the same helper on DSL and JSON input paths.

Weight tokens match the declaration byte-for-byte without config-layer normalization; mismatch=FUL-DSL-0038, repeated weight=FUL-DSL-0039, zero/invalid/out-of-range=FUL-DSL-0040. Disabling has one spelling: runtime disable, not weight 0. All four policies filter availability first, then cumulative weights; least_conn compares inflight/weight by integer cross-products. Weighted round-robin is bursty (3:1→a a a b), not smooth. ip_hash affinity changes when upstream availability or weights change.

### file_server subblock

| Subdirective | Meaning |
|---|---|
| `root` | Required absolute path; cwd-dependent relative roots reject. |
| `index` | One or more names, default index.html. |
| `follow_symlinks` | true default; false canonicalizes and requires containment under root. |
| `hide` | Append named path segments to the 404 list; multiple lines append. |
| `hide_defaults` | true default; false disables the built-in list. |
| `precompressed` | gzip/zstd/br, multiple values/lines append; select acceptable fresh sibling representation. |

browse is inline, not a subdirective. Default hidden segments are .git, .env, .svn, .hg, .bzr and .DS_Store, reported at load. Match segments rather than prefixes (.git does not match .gitlab-ci.yml); return 404 to avoid confirming existence. .well-known stays usable. true/false are the only boolean spellings.

GET/HEAD support indexes, trailing-slash 301 preserving query, optional directory listing, ETag/Last-Modified, If-None-Match/If-Modified-Since→304, single Range and If-Range. Other methods return 405+Allow. Multipart ranges intentionally receive a full 200 response. MIME uses a built-in small extension table, default application/octet-stream; parse all three HTTP date formats, including RFC850 two-digit-year rules.

Precompressed siblings older than the original are ignored to prevent partial-client stale deployments. The selected sibling is its own representation: keep strong ETag, Content-Length, Accept-Ranges and ranges. Otherwise dynamically compress; do not compress an existing Content-Encoding or a selected sibling twice. Dynamic compression weakens ETag and loses those full-representation properties.

## 4.2 Middleware

| Directive | Form |
|---|---|
| `header` | `header [matcher] [+-]<name> [value]` |
| `rewrite` | `rewrite [matcher] <target>`, internal rewrite without redirect. |
| `encode` | `encode gzip zstd br`, reusing the existing fork compressor. |
| `cache` | `cache [matcher] { ttl ... }`, reverse_proxy only. |

### cache subblock

| Subdirective | Meaning |
|---|---|
| `ttl` | Fallback freshness only when upstream gives no max-age/s-maxage/Expires; absent means no caching without upstream freshness. Never overrides no-store. |
| `max_size` | Per-entry cap, default 8MB; oversized responses still reach the client. |
| `capacity` | Backend-wide LRU budget, default 256MB; maximum across blocks, memory budget without disk and disk budget with disk. /load applies and reads back effective value; shrink evicts on next put, not during load. |
| `disk` | Absolute root (FUL-DSL-0033), process-wide. Different paths or mixed disk/no-disk blocks reject FUL-DSL-0035. |

Supports cacheability, freshness/key/Vary, conditional revalidation (304 updates metadata only), request coalescing, purge, and upstream no-store/no-cache/private (both forms), public/max-age/s-maxage/must-revalidate/proxy-revalidate/immutable/no-transform. stale-while-revalidate and stale-if-error parse but are not applied; HEAD and 206 responses are not cached. This is a shared cache: Authorization responses default uncacheable except explicit public/s-maxage/must-revalidate.

Compression precedes storage, with normalized secondary representation keys (G101). Hits send Age plus X-Fulcrum-Cache: HIT/REVALIDATED for memory, HIT-DISK/REVALIDATED-DISK for disk. These report the selected live backend; origin responses have no cache-hit status. An unusable disk backend logs error, disables cache and forwards without this header, never switches the disk budget to memory or aborts the generation.

Disk layout/read-time integrity/background recovery and the synchronous memory/disk interface are specified in [data path](data-path.md). Reads bypass the eviction index so restart can immediately hit; startup never scans the full disk. Temporary occupancy undercount affects eviction only. Purge shares the 0600 management socket, with key/prefix/all scopes.

## 4.3 Containers

| Directive | Semantics |
|---|---|
| `route { ... }` | Preserve written order inside; the container itself remains order 56. |
| `handle [matcher] { ... }` | Mutually exclusive: only the first matching handle executes. |
| `handle_errors { ... }` | Error chain; supports {status}. |

## 4.4 Site attributes

| Directive | Form |
|---|---|
| `tls` | Automatic tls (default), tls <cert> <key>, tls internal (UNWIRED), or tls { on_demand ... } (UNWIRED). |
| `log` | Flat JSON logging; full field contract in [observability](observability.md). |

### tls subblock

| Subdirective | Meaning |
|---|---|
| `on_demand` | Issue by handshake SNI; UNWIRED runtime admission/concurrency/failure cache remains. |
| `ask` | Admission endpoint required when on_demand is configured; omitted rejects compilation. |
| `dns` | Native cloudflare/dnspod or exec hook. |
| `resolvers` | Authoritative TXT-visibility servers; required with dns and vice versa. |
| `zones` | Explicit allowed zones, required by native providers; label-boundary check before writes. |

```text
*.example.net {
    tls {
        dns cloudflare env:CF_API_TOKEN
        zones example.net
        resolvers west.ns.cloudflare.com:53 arely.ns.cloudflare.com:53
    }
    respond 200
}
```

Resolver names resolve at every issuance; query every resulting address. Missing port becomes 53; invalid port/hostname and currently IPv6 literals reject FUL-DSL-0029. Resolution failure itself is runtime issuance failure/backoff. Confirm authoritative visibility before notifying the CA; fixed sleeps are prohibited.

Native credential forms: env:NAME, file:/absolute/path (trim, empty means missing), a direct literal, or literal:<value> for colon-containing credentials. Source-looking typos reject FUL-DSL-0031. Inline values make the whole config secret: load refuses any other-user mode bits, accepts 0640 root:service-group. compile redacts by default, --with-secrets warns on stderr, Secret Debug/Display redact, and loading redacted output rejects explicitly. Keep the implementation's redaction sentinel unchanged. File pointers are not the values themselves.

zones chooses the longest matching declared zone, not a suffix that accepts evilexample.net. Cloudflare startup verifies every zone via GET /zones?name, supporting user/account tokens; DNSPod verifies Info.Version but cannot prove scope. Fatal credential errors reject startup; network/timeouts log error and continue (unverified), preserving unrelated sites. Cloudflare needs Zone:Read+DNS:Edit; DNSPod free TTL minimum is 600 and HTTP200 still requires checking body status.code. Body credentials require redaction as well as auth-header redaction.

Exec convention: `<program> set <record-name> <value>` and `clear` to remove. Arguments are separate argv, never shell-interpreted; nonzero exit fails, timeout kills, output is bounded/truncated, and service identity has no elevation. Form _acme-challenge.example.com for *.example.com, stripping wildcard prefix. See [TLS](tls.md) for certificate storage, ARI and challenge isolation.

### log subblock

| Subdirective | Meaning |
|---|---|
| `output` | stderr default, or file <absolute-path>. Open at load; failure rejects startup or /load=400 preserving old config. |
| `level` | Threshold: debug/info record all; warn records 4xx/5xx; error records 5xx. Each output line's level is independently derived from status. |
| `headers` | Request header allowlist, at least one name; absent logs no headers. |
| `resp_headers` | Final sent response-header allowlist, after header middleware. |

Allowlist entries must be valid RFC9110 tokens. Authorization, Cookie, Set-Cookie and Proxy-Authorization reject case-insensitively (FUL-DSL-0036); JSON Runtime::build independently rejects them with 400. Deduplicate names case-insensitively. No-site 421 has no site logging configuration and therefore no access-log line; metrics account for it. h3 omits unavailable tls_cipher, rather than fabricating a cipher name.

## 4.5 Top-level L4 blocks

```text
l4 {
    tcp :3306 {
        proxy 10.0.0.5:3306
    }
    udp :53 {
        proxy 10.0.0.6:53
    }
    tcp :443 {
        sni api.example.com {
            proxy 10.0.0.1:8443
        }
        sni *.internal.example.com {
            proxy 10.0.0.2:8443
        }
        alpn h2 {
            proxy 10.0.0.3:8443
        }
        proxy 10.0.0.9:8443
    }
}
```

TCP supports direct proxy plus ordered first-match sni/alpn routing; multiple values within a rule are OR. SNI is case-insensitive exact or single-label wildcard using the common helper. ALPN is byte-exact membership, not prefix (h2 differs from h2c). A bare proxy is fallback; absent fallback closes unrecognized connections with a log. This L4 rule order is written order, not the HTTP directive table.

Inspection does not terminate TLS. Replay every consumed byte unchanged. A readable SNI can route a hello a full TLS stack would reject; unreadable/non-TLS/timeout/oversize uses fallback. Bound inspection by 3s and 16KiB. UDP cannot use sni/alpn because TLS ClientHello is not present and QUIC Initial is encrypted; reject with an explanatory diagnostic.

L4 upstreams require an explicit port (no HTTP scheme to supply it). Listener sets bind at startup and require systemctl reload to change (/load=409), but upstream targets can change live. TCP uses shared selection and can try another target during connection establishment; after connecting, no idle timeout is imposed (long-idle DB connections are valid). UDP connect is not a handshake; choose once and do not infer health from it. UDP sessions key by client address, expire at 60s idle and cap at 1024 per listener to bound spoofed-source fd use; stop recv_from immediately on shutdown so the old generation cannot steal new datagrams.

### L4 PROXY protocol

```text
l4 {
    tcp :5432 {
        proxy_protocol_from 10.0.0.0/8
        proxy_protocol
        proxy 10.0.0.7:5432
    }
}
```

Reception and sending are independent. proxy_protocol_from declares trusted sources and requires their header; others read zero bytes. proxy_protocol [v1|v2] sends, default v2; chained reception+sending forwards the original client address. A too-broad trust list rejects otherwise normal headerless traffic, intentionally preventing trusted proxy IPs from bypassing remote_ip controls. LOCAL/UNKNOWN retain peer identity. Header wait is 3s, then ClientHello inspection can add another 3s. UDP rejects both directives: a one-time connection-prefix header has no datagram connection-start location.

# 5. Matchers

Named @blocks carry complex conditions; inline matcher positions accept @name, a path beginning /, or explicit * (match all). Use `rewrite * /new/x`: otherwise a slash-prefixed destination is consumed as a path matcher. Diagnostics suggest the explicit escape.

| Condition | Meaning |
|---|---|
| `path` | Path wildcard matching. |
| `path_regexp [name] <re>` | Regex with capture placeholders. |
| `host` | Host header. |
| `method` | One or more methods. |
| `header <name> [value]` | Presence/equality. |
| `query <key>=<value>` | Query parameter. |
| `remote_ip <cidr...>` | Peer/trusted-PROXY client IP, never untrusted XFF. |
| `not { ... }` | Negation. |

Multiple conditions in a named matcher are AND; multiple values within one condition are OR.

# 6. Placeholders

The fixed set has no expressions, functions or conditionals. An unavailable-position placeholder is a compile error, never an empty-string fallback.

| Placeholder | Meaning/availability |
|---|---|
| `{host}` | Request Host. |
| `{uri}` | Full URI with query. |
| `{path}` | Path. |
| `{query}` | Query without ?. |
| `{method}` | Method. |
| `{scheme}` | http/https. |
| `{remote_ip}` | Client IP. |
| `{remote_port}` | Client port. |
| `{header.<Name>}` | Request header. |
| `{resp_header.<Name>}` | Response header, response-side/handle_errors only. |
| `{path.<N>}` | Regex capture N. |
| `{status}` | Error status, handle_errors only. |
| `{upstream}` | Selected upstream, response side only. |
| `{time}` | Request start, RFC3339. |

# 7. Value types

Durations require units (5s, 1m, 2h, 500ms), never bare numbers. Sizes accept decimal/binary prefixes (10MB,1GiB). Booleans are only true/false, rejecting yes/no/on/off. Strings are bare or quoted; quote spaces, braces and #. Outside quotes, # begins a whole-line or trailing comment. M1 supports one file without import/conf.d; cross-file locations, cycles and relative roots are outside scope.

# 8. Default responses

| Condition | Response |
|---|---|
| No site match | 421 Misdirected Request |
| No route within site | 404 |
| All upstreams unhealthy | 502 |

`handle_errors` overrides built-in error responses and can use {status}. A default site must not silently receive unmatched traffic.

# 9. Diagnostics

Aggregate recoverable errors with stable codes, line/column, source excerpt and caret:

```text
error[FUL-DSL-0007]: unknown directive `reverse-proxy`
  --> /etc/fulcrum/Fulcrumfile:12:5
   |
12 |     reverse-proxy 127.0.0.1:3000
   |     ^^^^^^^^^^^^^ unknown directive
   |
   = help: did you mean `reverse_proxy`?
```

Once emitted, FUL-DSL-NNNN meanings are stable for docs/automation. Explain execution order where written order is misleading. Examples and the directive/subdirective/placeholder inventory are executable documentation contracts; changing prose never authorizes removing their coverage.
