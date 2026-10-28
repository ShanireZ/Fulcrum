# Fulcrum's Pingora fork

Baseline: [cloudflare/pingora](https://github.com/cloudflare/pingora), tag
`0.9.0`, commit `702f690`, Apache-2.0. The fork began on 2026-08-12 from
`0.8.1`/`719ef6c` (PLAN G30) and rebased to 0.9.0 on 2026-09-24.
[PLAN](../../PLAN.md) owns decisions; this document owns the technical delta,
its preservation conditions and regression coverage. Upstream licenses remain intact.

## Scope and rationale

The fork contains eight crates:

```text
pingora-core pingora-error pingora-http pingora-pool
pingora-runtime pingora-timeout pingora-rustls pingora-boringssl
```

Seven participate in the product graph. `pingora-rustls` remains a tested vendor
workspace member to preserve the upstream optional feature, although G104 removed
it from the product graph. `pingora-openssl` and `pingora-s2n` are not vendored;
their optional path dependencies/features are removed, with dormant source cfg
names registered in workspace check-cfg.

Cargo caret requirements impose upper bounds, and `[patch.crates-io]` cannot
select a replacement outside the dependent crate's requirement. Replacing the
consumer is therefore necessary for these dependency changes. The fork also
contains individually approved capabilities, fixes and allocation changes;
compilation alone does not establish unchanged semantics.

The 0.9.0 rebase absorbed lru/nix/prometheus changes, fd hygiene, rustls default
feature fixes, the UDS short-read fix, BoringSSL feature alignment and h1 body-byte
accounting. It introduced short/per-address listener locking and unclaimed-fd
cleanup; product/custom services declare fd-table keys through listen_addresses().
The upstream pool became a global LRU; Fulcrum sets `128 × threads_l7` explicitly.
The accepted h2 defaults are 100 concurrent streams and a 64 KiB header table;
shutdown finishes already accepted streams. These are upstream behavior changes,
not evidence of performance equivalence.

## Numbered change ledger

Numbers remain stable because source and tests cite them. Absorbed or removed
entries identify their replacement instead of preserving obsolete instructions.

### 1. Dependency requirements

Relative to 0.9.0, retain `brotli = "8"`, `rand = "0.10"`, `sfv = "0.15"`,
and `x509-parser = "0.18.1"` in the relevant manifests. x509-parser alignment
avoids testing a different dependency combination from the product.

Earlier `lru 0.18.2` (RUSTSEC-2026-0253), `prometheus 0.14`/protobuf
(RUSTSEC-2024-0437) and nix changes were absorbed by 0.9.0 (`6463ad6`,
`842ddd9`, `7e29246`). Decorative strum/daggy/openssl-probe/windows-sys changes
were reverted: version novelty without a safety benefit did not justify rebase cost.

### 2. Call-site adaptations

Keep rand's `rng`, `random_range`, `IndexedRandom` and `RngExt` changes in
pingora-runtime and core connectors/offload. 0.9.0 moved offload.rs out of
connectors/. sfv's structured-field API adaptation remains in HTTP compression.
Malformed Accept-Encoding inputs are not exhaustively compared across sfv
versions; parsing failure warns and leaves compression algorithms empty. Reassess
if sfv is used on a path with consequences beyond declining compression.

The old nix migration is upstream now. Its durable trap was semantic: cmsg_space!
now supplies a nonempty zeroed buffer; clearing it made recvmsg see zero space.
Use fill(0), allocate for three timestamp values, and handle fallible cmsgs().
Those bugs compiled and passed M0 while rx-timestamp reads failed with ENOBUFS.
Review upstream semantic changes, not only compiler errors, when migrating APIs.

### 3. derivative → educe

Replace derivative (RUSTSEC-2024-0388) with educe 0.7.6 for PeerOptions Debug,
ignoring the same three function-pointer fields. Preserve equivalent output;
this replacement also removed the sole syn-1 consumer.

### 4. Received-fd hygiene: absorbed

Upstream 0.9.0 contains `b2b35fd` ([PR #960](https://github.com/cloudflare/pingora/pull/960)).
The accepted transfer socket uses OwnedFd so every exit closes it; recvmsg uses
MSG_CMSG_CLOEXEC atomically. In fork+exec upgrades, missing CLOEXEC creates both
inherited and SCM_RIGHTS copies, accumulating from the third generation.
Keep M1's three-generation assertions: one fd per listener, CLOEXEC set, and no
connected upgrade.sock residue. A single-process upstream unit test is insufficient.

### 5. rustls feature selection and native certificates

The default-feature fixes in rustls, tokio-rustls and core's dev-dependency were
absorbed as `5396a1f` ([PR #966](https://github.com/cloudflare/pingora/pull/966)).
Selecting ring alone does not disable aws-lc defaults. Check normal and dev edges;
`cargo tree -e normal` cannot establish the latter. The dev-dependency consumes
only types, so it must not independently select another provider.

Retain rustls-native-certs 0.8.4 and the CertificateResult adaptation: fail only
when `certs.is_empty() && !errors.is_empty()`. Preserve partial success and warn
about errors; failing whenever errors is nonempty would reject stores accepted
by 0.7. The rustls optional path remains in the vendor regression suite.

### 6. rustls-pemfile → rustls-pki-types

Remove the unmaintained rustls-pemfile (RUSTSEC-2025-0134), use PemObject APIs
with pki-types >=1.9, and retain all 18 characterization tests. This migrates the
same underlying parser, not a new certificate parser. Preserve the seven-kind
RUSTLS_PEMFILE_ITEM_KINDS whitelist: ECHCONFIG and future non-exhaustive kinds
must remain silently skipped. A direct all-kinds migration changes CA behavior.

Retain file-order key selection (RSA/EC/PKCS8), full-file CA validation after the
first certificate, and early return after the first private key. Negative controls
cover the whitelist, trailing malformed CA input and first-key ordering.

### 7. UDS short read: absorbed

Upstream `91b880d` ([PR #968](https://github.com/cloudflare/pingora/pull/968))
uses read_exact in test_connect_uds. A deterministic split write (1 byte,
20 ms pause, 8 bytes) reproduced the old invalid full-buffer assertion and
passed after repair. The separate UDS TOCTOU issue (#998) remains subject to
targeted rerun logic; do not conflate it with short reads.

### 8. Dynamic rustls certificate resolver: removed

G104 removed with_cert_resolver and its reexports on 2026-08-25. BoringSSL's
TlsSettings already dereferences to SslAcceptorBuilder and exposes the required
set_select_certificate_callback. Do not restore the obsolete rustls extension.

### 8b. Mixed ALPN setter: removed

G104 also removed set_alpn_wire. The old obstacle was not absence of Custom ALPN,
but inability to express built-in protocols plus acme-tls/1 in one rustls enum
variant. BoringSSL's existing ALPN callback handles that mixture. Normal protocol
preference and isolation of challenge certificates remain product test obligations.

### 9. daemonize: superseded upstream

0.9.0 replaced daemonize with daemonix (`ae96f7e`), removing RUSTSEC-2025-0069
and its audit exemption. The earlier decision avoided hand-written privilege
handling. Fulcrum runs foreground under systemd; do not reintroduce the old
accepted-advisory claim as current supply-chain status.

### 10. rustls Acceptor reexport: removed

G104 replaced ClientHello pre-reading with BoringSSL's early callback, then
removed the Accepted/Acceptor reexport. Early parsing can route a ClientHello
that rustls would reject for missing signature_algorithms; Fulcrum deliberately
leaves final handshake acceptance to the upstream. Retain that L4 test contract.

### 11. BoringSSL crate restored; feature delta absorbed

G104 restored pingora-boringssl and its core feature/path declarations. Its code
is upstream. The former removal of boring/pq-experimental is obsolete in boring 5;
0.9.0 uses boring 5 without that feature, and quiche 0.30 accepts `>=4.19, <6`.
Keep the product, Pingora and quiche on one compatible boring implementation.
The prior blanket prohibition on boring 5 belonged to quiche 0.29, not this baseline.

### 12. Pre-TLS callback on cleartext ports

In UninitializedStream::handshake, invoke upstream PreTlsProcess before the TLS
branch, including cleartext ports. Upstream 0.9.0 otherwise invokes it only for
TLS. PROXY parsing, trust and socket reads live in Fulcrum's proxyproto modules,
not in the fork. A peer-address OnceCell can already have been initialized before
handshake; replace the whole SocketDigest after reading PROXY data instead of
ignoring a failed set. Local/original addresses derive again from the same fd.

Removal condition: upstream runs an equivalent pre-TLS callback for cleartext
connections. Retain the product's real-socket PROXY tests when rebasing.

### 13. h1 body_bytes_sent: absorbed

Upstream `e7de90a` excludes response-header bytes, matching h2 and Fulcrum h3.
The upstream regression body_bytes_sent_excludes_response_header now guards it.
Access-log resp_size remains body-only; status-code success cannot detect a wrong
size counter. A negative-control failure must arise from the counter, not missing
mock-IO expectations.

### 14. SNI and ALPN in SslDigest (G128)

Keep public `sni` and `alpn` Option<String> fields initialized to None in new()
without changing its signature. BoringSSL from_ssl fills them through servername
and selected_alpn_protocol; other backends need no changes. Fulcrum h3 builds the
same digest from quiche APIs with TLSv1.3 and shares access-log extraction.

The alternative listener extension callback forces a certificate suspend/resume
cycle even for synchronous certificate selection. The digest avoids that path.
Keep tls_digest_gate.rs field/assignment/source-method checks and real h1/h2/h3
handshakes in tests/log, including no-ALPN negative coverage. Compilation alone
cannot detect from_ssl silently leaving the fields None.
Removal condition: equivalent upstream digest fields/population.

### 15. Connection lifetime counter (G122)

Keep ConnectionCounter enter/leave and ConnGuard in listeners/mod.rs. Propagate
counter handles through existing and future listeners, including add_listener().
run_endpoint captures the handle/listen address once, constructs the guard before
spawn and moves it into the async task through a named binding. Bare `let _ =`
or deleting that move drops it early, leaving active gauges permanently zero;
omitting a guard while calling enter leaks active counts instead.

ConnGuard owns the enter/Drop-leave invariant. The fork knows neither Prometheus
nor label semantics; Fulcrum conn_stats supplies those. Keep separate behavior,
source-wiring and real TLS-active-connection checks. Source checks strip full-line
comments, with self-tests for warning text that mentions forbidden forms; trailing
comments remain a known parser limit. No equivalent upstream removal path is assumed.
Related upstream work: [issue #994](https://github.com/cloudflare/pingora/issues/994),
[PR #995](https://github.com/cloudflare/pingora/pull/995).

### 16. Request-path overhead (G157)

- Remove pending-duration recording after poll_read/write/flush/write_vectored;
  retained getters return zero. This is unobserved in Fulcrum's eight crates, but
  upstream pingora-proxy reads write-pending time: the deletion is not an
  upstream-wide semantic no-op.
- Use httparse parse_with_uninit_headers for the default path, avoiding initial
  filling of 256 slots. Preserve parser/default configuration and partial-input
  behavior. The pre-existing patched_http1 path still requires Cloudflare's
  parse_unchecked extension unavailable in crates.io httparse 1.10.1.
- Store header offsets on the stack in populate_header_refs; retain the existing
  client populate_headers path and identical offsets.

Keep pending-read tests that prove an actual wait occurred, exact-MAX_HEADERS
name/value checks with one-extra rejection, and the isolated thread-local
allocation counter: an ordinary read_request allocates 8 times, versus 9 when
its offset Vec is restored. Existing split-input/escaped-URI tests cover reentry.
Rebase changes to whole-call allocation counts require attribution before changing
expectations. Private instruction/rate diagnostics are not public performance evidence.
Removal condition: equivalent upstream parsing/storage improvements; duration removal
remains a fork-specific obligation while upstream has readers.

### 17. Title-case lookup (G162)

Add and publicly reexport Allow, Alt-Svc, Content-Range, ETag, Last-Modified,
Location and Vary in titled_header_name_str. This permits no-case response
construction when user-supplied spelling still serializes unchanged. It deliberately
changes constant-inserted etag/vary to ETag/Vary on h1, including compression and
some h2/h3-to-h1 forwarded names. This is not a wire-invariant optimization.
Keep lookup/constant/no-case serialization tests, files status/header tests and
encode ETag/Vary assertions. Removal condition: upstream supplies the entries;
the public lookup reexport may remain independently necessary.

### 18. Optional request-name case table (G163)

HttpSession preserve_header_case defaults true. Its setter chooses build versus
build_no_case on every h1 request; Session forwards it to h1 and is a no-op for
h2/subrequests/custom sessions. Callers must set it for each new request session.
Fulcrum disables it only on ports without reverse_proxy; default behavior remains
upstream-compatible. Preserve default/on/off semantic tests and the differential
allocation test (off saves exactly 2 allocations). Removal condition: equivalent
upstream option or lazy case-table construction.

### 19. Response-header allocations (G165)

Use HeaderValue::from_static for Connection keep-alive/close. Return BytesMut from
prepare_response_header and freeze only in the proxy-task caller that requires
Bytes. Direct write_all borrows the buffer without allocating a Shared block.
Wire bytes and sensitivity are unchanged; the proxy task still freezes once.

Keep whole-call allocation tests (1 remaining serialization-buffer allocation,
versus 3 upstream) for both Connection values and byte-exact response tests,
excluding Date's changing value while validating its HTTP-date form. Warm the
thread-local Date cache and take the minimum of three counts. The product does
not enable proxy tasks; a missing required proxy freeze is a compile error.
Removal condition: equivalent upstream static values and buffer handling.

### 20. h2 0.4.20 conflicting-Content-Length tests

Only tests change. h2 now resets conflicting duplicate Content-Length streams
with PROTOCOL_ERROR before Pingora accept sees them. First from_h2_conn returns
None within 1s; the malformed budget remains 31. Retain client reset assertions
and the renamed not-charged-to-budget test. Other upstream tests still cover
budget exhaustion. These streams now count against h2's local-reset limit (1024),
not Pingora's per-connection malformed budget (32). Downgrading to h2 0.4.19 was
the negative control. Removal condition: equivalent upstream tests; this fork
did not add the h2 lower-bound change proposed in submission drafts.

### 21. serde_yaml → serde-saphyr

Replace deprecated serde_yaml/unsafe-libyaml (RUSTSEC-2024-0449) with
serde-saphyr 2, defaults off, deserialize/serialize on, comments off. Adapt
configuration from_str/to_string and the working-directory roundtrip test;
serde-saphyr has no Value tree API. Preserve all 14 configuration tests and the
M0/M1 Server::new paths. Fulcrum's product constructs ServerConf in Rust and does
not parse YAML, but the replacement still adds compiled dependencies.

Serialized sequence strings gain quotes; roundtrip remains unchanged. Parser
edge cases were not exhaustively compared. serde-saphyr 2 requires Rust 1.89,
above upstream's 1.85 MSRV, so this is not a drop-in upstream submission.
Removal condition: upstream replaces or makes its YAML dependency configurable
([issue #779](https://github.com/cloudflare/pingora/issues/779)).

### 22. Optional L4 read buffering with Tokio 1.54

Tokio 1.54 rejects BufReader/BufStream read capacity zero. Replace the private
BufStream wrapper with L4BufStream: read capacity zero uses only BufWriter;
positive capacity uses BufReader<BufWriter<...>>. Write capacity zero remains
valid and writes through. TCP/virtual/Unix conversions start unbuffered, and
set_buffer selects the state. This preserves Tokio 1.53's direct poll_read path,
wire bytes, allocation count and unbuffered upstream connections. Debug type
names change; poll_fill_buf's zero-capacity EOF problem is not used here.

Keep tests for direct socket reads, read prefetch, write-only buffering,
zero-capacity write-through and construction allocating exactly once. A tiny
read buffer can evade behavior probes but must fail the allocation check.
Removal condition: adopt upstream's fix for
[issue #1059](https://github.com/cloudflare/pingora/issues/1059), retaining these
checks; investigate any changed buffering/allocation expectation before accepting it.

## Inventory, verification and rebase method

The recorded 2026-10-23 comparison has 22 changed source/manifest files relative
to 0.9.0 (excluding lockfiles and this guide). Recompute rather than maintaining
line-count snapshots. Compare every vendored crate and the workspace manifest,
not just pingora-core. Export source without checkout newline conversion:

```bash
git clone --depth 1 --branch 0.9.0 https://github.com/cloudflare/pingora /tmp/pingora-stock
for c in pingora-core pingora-error pingora-http pingora-pool \
         pingora-runtime pingora-timeout pingora-rustls pingora-boringssl; do
  git --no-pager diff --no-index --stat "/tmp/pingora-stock/$c" "vendor/pingora/$c"
done
git --no-pager diff --no-index --stat /tmp/pingora-stock/Cargo.toml vendor/pingora/Cargo.toml
```

On Windows, use `git -c core.autocrlf=false archive <tag>` when materializing the
upstream tree for byte comparison. Preserve source identity and license notices.
A release tag and main can diverge: the historical 0.8.1/main comparison found
security fixes only on the tag. Do not assume main is a superset or replace the
baseline without reviewing both sides. dep-check.py reports release/main state;
its version baseline comes from pingora-core/Cargo.toml, not a second cache.

For rebase:

1. Read the new release, compare all numbered deltas and remove those truly absorbed.
2. Reapply remaining requirements/call sites/capabilities. Review semantics even
   where compilation succeeds, including auxiliary test and optional-feature paths.
3. Remove unvendored openssl/s2n paths/features and dial9 feature/dependency entries
   (dial9-tokio-telemetry/aws-sdk-s3); retain dormant cfg names in check-cfg and
   workspace lints. Otherwise the vendor lock can admit unwanted aws-lc dependencies.
4. Update locks incrementally in one-off build containers. Do not regenerate locks
   or bypass G29's quarantine. Explicitly update path Pingora packages; a minimal
   metadata re-resolution can pull the old registry package back into the graph.
5. Remeasure stock upstream failures in the same container/network environment,
   including rustls and dev edges. EXPECTED_FAILURES currently is empty; both
   unexpected failure and unexpected success of a listed failure are errors.
   Keep targeted rerun logic for intermittent failures rather than broad exemptions.
6. Run both vendor regression and integration coverage through the documented gate:

```bash
VENDOR_ONLY=1 bash tests/m0/docker-run.sh
bash tests/m0/docker-run.sh
```

Rust commands run only in Docker (G107). BUILD_ONLY or narrowed scenarios do not
establish the full gate. The suite verifies actual test execution, lock alignment
and network timeout preconditions; compilation and M0 alone do not cover all fork
semantics. Dependency changes also require the current milestone gates (G29/G36/G152).
Public contributions follow [upstream contribution guidance](../../docs/platform/upstream-pr.md)
and [public provenance links](../../upstream-pr/README.md); drafts and run receipts
are not the fork's source of truth.

Known upstream-equivalent cleanup limit: cmsgs()? can return before unlink(path)
after truncated ancillary data, leaving a socket file. MAX_FDS=32 covers normal
upgrade traffic; preserve this investigation clue without claiming it is newly fixed.
