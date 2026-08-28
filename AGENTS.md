# AGENTS.md — 枢衡 Fulcrum project guide

Read this before touching anything. It is short on purpose; everything it points at is longer.

## Source of truth

- [PLAN.md](PLAN.md) — scope, milestones and exit conditions, the §10 decision table
  (G-numbers) and the §11 open list (D-numbers). On conflict, `PLAN.md` wins.
- [docs/](docs/index.md) — an [Open Knowledge Format v0.2](docs/references/okf-spec.md) bundle.
  [docs/architecture/](docs/architecture/index.md) is the technical baseline and carries real
  content; the rest mostly navigates `PLAN.md`.
- [vendor/pingora/FORK.md](vendor/pingora/FORK.md) — the authority on what the fork changes.

## Working rules

- **Commit directly on `main`.** No branches, no PRs. Never `amend` / `force`.
  ⛔ Agents do not `git push`; the owner batches commits into one push.
- **Every G-number in `PLAN.md` §10 was decided by the owner.** If one looks wrong, say so and
  ask — do not implement around it.
- **No performance claim without reproducible end-to-end measurement** (`PLAN.md` §8). Never
  write "N times faster".
- **Never hand-write TLS, the HTTP/2 state machine, HPACK, or QUIC.** Use audited libraries.
- **Expected state is the only authority.** Runtime overrides are not persisted but must be
  visible in stats and API output. Do not add a second persistent write path.
- **Status sentences drift.** After any status change, sweep as one checklist:
  `README.md` → `PLAN.md` §1 → `PLAN.md` §7.
- **Docs and comments carry conclusions, not a running account** (G115). `handoff/` is
  gitignored and stays local; anything worth keeping goes into `PLAN.md` or `docs/` as a
  present-tense statement of what is done, not done, or still open.

### Two locked constraints (G6, as amended by G104)

1. **The TLS backend is BoringSSL.** Dynamic cert selection goes through
   `SslContextBuilder::set_select_certificate_callback` — the *same* callback for both entrypoints
   (h1/h2 and h3/QUIC), so "two entrypoints each with their own cert-picking code" is structurally
   impossible. All three former rustls sites are converted.
   ⚠ Say "nothing in the dependency graph", **not "in the artifact"**: `Cargo.lock` is a superset
   of the graph (gate 4), `cargo tree` reads the graph (gate 5), and what the artifact links is
   still unanswered (D23).
2. **tower middleware does not compose with Pingora's phase model.** ⚠ But we have never used
   `ProxyHttp` — `pingora-proxy` is not a dependency. Our execution chain hangs off
   `HttpServerApp` / `ServerSession`.

## Building and testing

⛔ **Rust runs in Docker, full stop (G107).** Do not install a native toolchain on the host.
Four of six product crates use `std::os::unix`, and the gate needs `SIGQUIT` + fd handover,
systemd, unix sockets, `flock`, `0600` and an iptables black hole.

```bash
bash tests/m0/docker-run.sh    # build + lint + unit + fork regression + every end-to-end scenario
```

Narrow it with one `*_ONLY=1`:

| flag | scenario |
|---|---|
| `BUILD_ONLY` / `LINT_ONLY` | build only · `fmt` + `clippy -D warnings` + `shellcheck` only |
| `UNIT_ONLY` | Fulcrum's own crate tests |
| `VENDOR_ONLY` | the fork regression net (**first gate after a rebase**) |
| `SERVE_ONLY` | data plane end-to-end, real traffic |
| `L4_ONLY` | L4 (TCP/UDP passthrough, SNI/ALPN split, PROXY protocol, socket handover) |
| `FILES_ONLY` / `CACHE_ONLY` / `CACHEDISK_ONLY` / `ENCODE_ONLY` | static files · cache · disk backend · compression |
| `H3_ONLY` / `RELAY_ONLY` | HTTP/3 end-to-end · cross-generation QUIC relay |
| `PP_ONLY` / `LOG_ONLY` | PROXY protocol on the HTTP side · structured access log |
| `ACME_ONLY` / `RENEW_ONLY` | ACME against pebble (a real local CA) · renewal |
| `SMOKE_ONLY` / `STRESS_ONLY` / `MUSL_ONLY` / `UNCLAIMED_ONLY` | smoke self-check · sustained load · musl static artifact · unclaimed inherited fd |

Most flags have a matching `<NAME>_TESTS=0` that skips that scenario instead (`LINT=0` for lint).
The systemd scenarios run in a **separate container** (systemd as PID 1) and are invoked last;
`M1_TESTS=0` skips them, `bash tests/m1/systemd-run.sh` runs them alone.

⚠ `docker-run.sh` self-tests its `*_ONLY` dispatch on every run — adding a scenario means adding
a row to that self-test, a row to the `LINT_CMD` shellcheck list, and the `<NAME>_ONLY` /
`<NAME>_TESTS` pair.

Per-scenario detail: [docs/platform/build-and-test.md](docs/platform/build-and-test.md).

### Port allocation

Scenarios run sequentially, but each one claims its own range so a leftover process from a
previous run cannot be mistaken for the one under test. A new scenario takes a free range.

| scenario | ports |
|---|---|
| `tests/m0/` | 8081 · 9000 |
| `tests/serve/` | 9100–9105 |
| `tests/stress/` · `tests/smoke/` | 9200–9201 · 9210–9212 |
| `tests/l4/` | 9300–9317 |
| `tests/files/` | 9400–9401 |
| `tests/acme/run.sh` | 8053 · 8055 · 8083 · 9443–9444 · 14000 · 15000 |
| `tests/acme/renew.sh` | 8054 · 8056 · 8084 · 9445–9446 · 14001 · 15001 |
| `tests/cache/run.sh` · `tests/cache/disk.sh` | 9500–9501 · 9502–9503 |
| `tests/encode/` | 9600–9601 |
| `tests/h3/` | 9700–9702 |
| `tests/proxyproto/` | 9800–9803 |
| `tests/log/` | 9900–9906 |
| `tests/quic-relay/` | 9910–9911 |
| *(shared, hardcoded)* | 80 — see below |

**Port 80 is the one exception, and it is shared on purpose.** `synthesize_http_redirect`
(`crates/fulcrum-config/src/compile.rs`) gives every auto-HTTPS site a 308 redirect site on a
hardcoded `:80`, so seven scenarios bind it implicitly: `tests/acme/run.sh` ·
`tests/acme/renew.sh` · `tests/serve/` · `tests/log/` · `tests/h3/` · `tests/proxyproto/` ·
`tests/quic-relay/`. Making it configurable is open as **D29**.

⛔ **Do not add `:80` to any "these ports must be free" baseline check.** Occupying it is
harmless — measured: hold `127.0.0.1:80` for a whole run and `tests/acme/run.sh` still passes
41 ✓ / 0 ✗. Each pingora service binds on its own runtime (`Server::run_service` spawns per
service), so one listener stuck retrying `:80` does not delay the others. A gate that can only
ever stop correct output is as bad as one that never goes red.

⚠ What *is* true: any LISTEN on `:80` makes that bind fail — all four shapes (`0.0.0.0` or
`127.0.0.1`, with or without `SO_REUSEADDR`) return `EADDRINUSE`, and `bind_tcp` then retries 30
times at 1 s. So no scenario may assume the redirect listener came up, and none asserts it.

## Gate discipline

**A gate that has only ever been observed green is indistinguishable from a gate that does not
run.** Flip the condition, break the input, or delete the fix — and watch it fail. Then put it
back. If a gate cannot be made to fail, it is not a gate. Prefer gates that carry their own
reverse test: `tests/m0/unclaimed.sh` asserts `port_listening` is false at step 0 and true at
step 2, so one passing run exercises both directions.

Four ways a green gate stops meaning anything:

- **The blind spot is in the fixture.** When a code path branches on the *shape* of an input
  (IP literal vs hostname, absolute vs relative, ASCII vs not), check the fixtures contain both
  shapes. The count of gates tells you nothing about this.
- **A gate anchored on something *missing* needs re-anchoring every time that thing ships.**
  Anchor the positive half on a fixed prefix, and add the reverse half — a capability that is
  already wired must not appear — because that direction never goes red on its own. Pin such
  lists in both directions, docs included.
- **The binary the gate runs is part of the fixture.** A spike proves the road exists; it does
  not prove the product is on it. When a scenario proves a *product* property, check what
  `ExecStart=` actually points at.
- **A ruler that reads the same in both cases cannot tell them apart.** Before trusting a gauge,
  ask what it reads in the good case and in the bad case, and make sure those readings differ.

Two corollaries:

- **Never anchor a judgment on the exit code of the thing that triggers the work.** `systemctl
  reload` returns 0 whether or not the re-exec succeeded. Anchor on what the world looks like
  afterwards (did the pid file change? what does `/proc/<pid>/exe` say?).
- **A broken measuring instrument reports a broken subject**, and the report reads exactly like
  a real defect. When a new gate goes red, first run it on a clean tree.

## Editing shell scripts on this host

The host is **Windows + Git Bash (MSYS)**. Several of its tools fail by producing
confident-looking wrong output rather than an error.

1. **Do not edit shell scripts through a heredoc-fed inline script.** Heredocs — quoted ones
   included — eat one level of backslash escaping, and `bash -n` passes the result. Use the Edit
   tool, or write a generator to a file and run the file.
2. **Do not use MSYS `grep` to look for control characters.** It normalises line endings while
   reading, so `grep $'\r'` on a CRLF file returns zero matches. Count bytes instead:
   `tr -dc '\r' < file | wc -c`. Every scanner must prove it can both hit and miss.
3. **Do not edit a shell script while the gate is running.** The tree is bind-mounted live and
   `bash` reads a script by byte offset; an edit makes it resume mid-line and abort the whole run.
   (Editing Rust or Markdown mid-run is fine.)
4. **`/etc/hosts` inside the container is a bind-mounted *file*.** Truncate and rewrite in place
   (`cat > /etc/hosts`); `sed -i` and `mv` replace the inode, which a bind mount rejects. Restore
   it in `cleanup`.
5. **A scenario where every path ends in an explicit `exit` makes shellcheck disown its own
   `trap … EXIT`** (SC2317). Drop the trailing `exit 0` rather than adding a `disable` comment.
6. **Trap `SIGPIPE`.** A scenario that writes to a socket it just broke dies with exit 141 and no
   failure line; `trap '' PIPE` turns that write into a return value.
7. **`bash` cannot read a UDP datagram** — one `read()` consumes the datagram and returns its
   first byte. Use a client that reads whole datagrams (the L4 scenario uses a python3 helper).
8. **`curl --rate N` is per *hour* without a unit**, and `-o` / `-w` apply per URL.

Traps 1 and 2 are gates: `shellcheck` runs over every `tests/**/*.sh`, and `docker-run.sh`
self-tests its byte probes against known-CRLF, known-LF and known-binary fixtures on every run.

## Dependencies

Updates follow G29 — chase latest, including breaking majors, with a 24-hour quarantine:

```bash
python tools/dep-check.py            # report only
python tools/dep-check.py --apply    # adopt whatever cleared quarantine
```

⛔ Never run a bare `cargo update`; it skips the quarantine window G29 exists to enforce.

Exit codes stack, one bit per check: `10` cargo-side updates · `20` the pingora fork's upstream
moved and needs a manual rebase · `40` build image needs attention (G36 — rustc moved, the
`@sha256` pin is gone, **or the check could not complete**) · `80` systemd test-host image
(G39, keyed on systemd's *major* version) · `160` unregistered security advisory (or the scan
could not complete) · `1` error. Any sum of those is a valid code.

Only the first is inside `cargo update`'s field of view; the rest are structurally invisible to
it, which is why they are watched separately. All of them report and refuse to auto-apply.

- **`dep-check.py` reporting "all current" does not mean the dependencies are current.** It only
  sees what `cargo update` can move, and that never crosses an upper bound in an upstream
  manifest. Measured once at 44 of 176 packages behind while it reported no updates available.
- **The advisory check scans both lockfiles**, `vendor/pingora/Cargo.lock` first and the root one
  second — `[patch.crates-io]` points `pingora-core` at the vendor tree. The `ACCEPTED` registry
  is imported from `supply-audit.py`, not copied.
- **pingora does not come from crates.io.** Read pingora source under `vendor/pingora/`, not the
  crates.io copy. When upstream releases, check first whether it already raised the bounds, and
  always re-run the gate afterwards — one fork change lands in `server/transfer_fd/mod.rs`.
- **systemd gets its own check (G39)** because the test host's systemd *is* part of the M1
  conclusion. When it goes red, look at `tests/m1/mainpid-handover.sh`: if that scenario turns
  green, systemd changed its behaviour and **G37's trade-off must be re-evaluated rather than the
  assertion edited**.
- **Pin library versions when reading docs.** Do not infer Pingora or BoringSSL API compatibility
  across versions — check `cargo doc` against the version actually locked.

Read [docs/platform/supply-chain.md](docs/platform/supply-chain.md) before touching dependencies.

## Agent workflows

- **Issue tracker** — GitHub Issues in this repository: `docs/agents/issue-tracker.md`.
- **Triage labels** — the five canonical labels: `docs/agents/triage-labels.md`.
- **Domain docs** — single-context layout: `docs/agents/domain.md`.
- **Engineering skills** — when to use them and how they compose: `docs/agents/skill-workflows.md`.
- **Documentation** — maintain `docs/` as an OKF bundle: `docs/agents/documentation.md`.
