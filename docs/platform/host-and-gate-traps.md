---
type: reference
title: Host traps, gate discipline, scenario flags and port map
description: Detail split out of AGENTS.md when the workspace put project guidelines on a resident-context budget.
tags: [gate, host, ports, dependencies]
status: current
---

# Host traps, gate discipline, scenario flags and port map

> **Everything here is still a hard rule** — `AGENTS.md` keeps only the traps that bite most often and points here.

## Scenario flags

Narrow it with one `*_ONLY=1`:

| flag | scenario |
|---|---|
| `BUILD_ONLY` / `LINT_ONLY` | build only · `fmt` + `clippy -D warnings` + `shellcheck` only |
| `COMPILE_ONLY` | `shellcheck` + compile, **all targets**, zero tests run — what the `pre-push` hook uses. ⚠ `BUILD_ONLY` is not the same thing: `cargo build --release` never compiles test targets |
| `UNIT_ONLY` | Fulcrum's own crate tests |
| `VENDOR_ONLY` | the fork regression net (**first gate after a rebase**) |
| `SERVE_ONLY` | data plane end-to-end, real traffic |
| `L4_ONLY` | L4 (TCP/UDP passthrough, SNI/ALPN split, PROXY protocol, socket handover) |
| `FILES_ONLY` / `CACHE_ONLY` / `CACHEDISK_ONLY` / `ENCODE_ONLY` | static files · cache · disk backend · compression |
| `H3_ONLY` / `RELAY_ONLY` | HTTP/3 end-to-end · cross-generation QUIC relay |
| `PP_ONLY` / `LOG_ONLY` | PROXY protocol on the HTTP side · structured access log |
| `METRICS_ONLY` | the Prometheus scrape endpoint (`metrics`, a terminal directive) end-to-end |
| `STATS_ONLY` | `GET /stats` — the optional fields and the degenerate states, not "can it be scraped" |
| `ACME_ONLY` / `RENEW_ONLY` | ACME against pebble (a real local CA) · renewal |
| `SMOKE_ONLY` / `STRESS_ONLY` / `MUSL_ONLY` / `UNCLAIMED_ONLY` | smoke self-check · sustained load · musl static artifact · unclaimed inherited fd |

Most flags have a matching `<NAME>_TESTS=0` that skips that scenario instead (`LINT=0` for lint).
The systemd scenarios run in a **separate container** (systemd as PID 1) and are invoked last;
`M1_TESTS=0` skips them, `bash tests/m1/systemd-run.sh` runs them alone.

⚠ `docker-run.sh` self-tests its `*_ONLY` dispatch on every run — adding a scenario means adding
a row to that self-test and the `<NAME>_ONLY` / `<NAME>_TESTS` pair. The shellcheck scan is **no
longer on that list**: it is derived from the tree (`tests/ci/shellcheck-all.sh`), so a new
directory is covered without anyone remembering.

## Port allocation

Scenarios run sequentially, but each one claims its own range so a leftover process from a
previous run cannot be mistaken for the one under test. A new scenario takes a free range.

| scenario | ports |
|---|---|
| `tests/m0/` | 8081 · 9000 |
| `tests/serve/` | 9100–9106 |
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
| `tests/metrics/` | 9920–9926 (9923 and 9926 are python upstreams, not Fulcrum listeners) |
| `tests/stats/` | 9930–9932 (range 9930–9935 reserved) |
| *(registered, never listened on)* | 19999 — see below |
| *(shared, hardcoded)* | 80 — see below |

**19999 is registered precisely because nothing ever listens on it.** `tests/serve/run.sh` and
`tests/metrics/run.sh` both use `127.0.0.1:19999` as a `reverse_proxy` address whose only job is
to exist as an override key and then go dangling — neither scenario ever connects to it. That is
why it is safe today, and it is also why it is easy to miss: it appears in no listener list. ⚠ It
stops being safe the moment a scenario adds a *real* probe through it, because the connection
would go to whatever happens to occupy 19999 on the host. Treat this row as a claim about
intent — **if you need an address that is actually connected to, take one from your own range.**

**Port 80 is the one exception, and it is shared on purpose.** `synthesize_http_redirect`
(`crates/fulcrum-config/src/compile.rs`) gives every auto-HTTPS site a 308 redirect site on a
hardcoded `:80`, so these scenarios bind it implicitly: `tests/acme/run.sh` ·
`tests/acme/renew.sh` · `tests/serve/` · `tests/log/` · `tests/h3/` · `tests/proxyproto/` ·
`tests/quic-relay/`. Making it configurable is open as **D29**.

⚠ ★ **`tests/stats/` was on that list on 2026-09-03 and was taken back off the same day** —
worth reading, because it shows both failure modes. Its `gen3.Fulcrumfile` has
`t.example:9932 { tls … }`, and `fulcrum compile` on exactly that shape emits **two** sites,
the second being `http://t.example:80` — so it *was* binding `:80`, while its own file comment
said the opposite and this list did not name it. ⇒ Fixed by giving that generation a global
`auto_http_redirect false`, guarded by a **compile-time** assertion in the scenario (its port
set must be exactly `{9932}`). ★ That gate is deterministic — no runtime timing, and deleting
the `auto_http_redirect false` line turns it red immediately.

⚠ ★ **A scenario is on that list the moment it has one auto-HTTPS address**, which is any
address with a hostname and no explicit `http://` — the scheme defaults to `https` in
`parse_address`, and `auto_http_redirect` defaults to *on*. Writing `tls <crt> <key>` does not
take it off the list; only `http://` on every address, or a global `auto_http_redirect false`,
does. ⇒ `tests/metrics/` has a TLS site on 9922 and stays off the list **because its three
configs each carry `auto_http_redirect false`** — that line is load-bearing, not boilerplate.
⚠ ⚠ The list above is maintained by hand and **nothing checks it**: a scenario that grows a
TLS site silently becomes the next implicit binder, and the symptom lands on an innocent port
in some *other* scenario. Re-derive it before trusting it.

⚠ ⚠ ⚠ **An occupied `:80` can take down listeners that have nothing to do with it**, and the
mechanism is not obvious. `ListenerEndpoint::listen` takes the process-wide `ListenFds` mutex and
then calls `bind()` **while still holding it** (`listeners/l4.rs`; upstream's own comment on that
line says "consider make this mutex std::sync::Mutex or OnceCell"). `bind_tcp` retries 30 times at
1 s. So one occupied port parks the shared lock for up to 30 s, and **every listener that has not
yet taken the lock never binds** — regardless of which service or runtime it belongs to. Whether
your port is before or after the stuck one is a race, so the symptom is intermittent and lands on
an innocent port. Measured: a leaked `:80` holder made `tests/acme/run.sh` fail on **`:8083`**,
with the only error in the log being about `:80`.

⚠ Any LISTEN on `:80` makes that bind fail — all four shapes (`0.0.0.0` or `127.0.0.1`, with or
without `SO_REUSEADDR`) return `EADDRINUSE`. No scenario may assume the redirect listener came up,
and none asserts it.

★ **The fix belongs at the source: a scenario must hand `:80` back when it exits.** Because `:80`
is synthesized, no scenario mentions it, so a leak names nothing and the next scenario takes the
blame. `tests/quic-relay/run.sh` now asserts in its own `cleanup` that the ports it used — `:80`
included — are free again, and skips `:80` if it was already busy on entry so it cannot blame
someone else. Copy that check into any scenario that starts a generation the same way.
⚠ The bug it caught was `GEN2=$(start_gen …)`: `$(…)` is a subshell, so `PIDS+=` updated a copy
and `cleanup` killed nothing — while the scenario still reported PASSED.

## Gate discipline — the ways a green gate stops meaning anything

### 0. An early stop truncates the probe, and the truncated output looks complete

cargo stops at the **first** crate in the dependency chain that fails to compile, so everything
downstream is never built. Measured on this repo: an injection placed at **7 sites across 4
crates** came back naming **1**, from `fulcrum-config` alone — and that output reads exactly like
a full list. Four rebuild passes were needed to collect them all.

★ The criterion is **"the last pass produced no red crate downstream"**, not "the compiler printed
N errors". A single pass that does report every site means the symbol happened to be private to
one crate — **that is luck, not method**, and it will not hold for the next injection.

⇒ **Print the injected count next to an independent census of the same category.** You only learn
that something was missed when the two numbers disagree; one number alone can never tell you.

⚠ Same shape, different clothes: `--fail-fast`, a CI job skipped because its `needs:` failed,
`grep -m 1`, and short-circuiting boolean operators. Each returns a well-formed answer about a
subset while looking like an answer about the whole.

And the ways a gate that does run still stops meaning anything:

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
- **The binary the gate runs may be one the tree no longer describes.** On this host `cargo`
  sometimes calls a crate fresh after a bulk rewrite (`git rebase`, branch switch) and reuses the
  previous test binary: the run is green and the new tests are simply **not in it**. Measured:
  after one rebase, two identical `UNIT_ONLY` runs on the same tree reported **289** and **288**
  tests for `fulcrum-server` while the source had **291** — ⚠ two readings that disagree with
  *each other*, and the three newly added tests never ran. The file was identical inside the
  container (`md5sum` matched) and `cargo test -p fulcrum-server --lib -- --list` found all 291,
  so only the path through the named `target` volume was stale ⇒ mtime skew across the Docker
  Desktop bind mount is the likely cause, but that half is a hypothesis, not a measurement.
  ★ **Anchor on `Compiling <crate>` in the log** — a green summary does not say what was built.
  After a rebase or a branch switch, refresh mtimes from inside the container first:

  ```bash
  docker run --rm -v "$(cygpath -m "$PWD"):/w" -w /w fulcrum-build:local bash -c 'find crates spikes -name "*.rs" -o -name Cargo.toml | xargs touch'
  ```
- **A ruler that reads the same in both cases cannot tell them apart.** Before trusting a gauge,
  ask what it reads in the good case and in the bad case, and make sure those readings differ.
- **A criterion can name one quantity and measure another — and the tell is that it goes red at
  random.** `tests/l4`'s PROXY-protocol case asserted "the payload follows the header" by having
  the upstream fixture do a **single** `recv`. But `l4.rs` writes to the upstream three times:
  the PROXY header at connect, the peeked prelude, then whatever `copy_bidirectional` relays. On
  the send-only listener the prelude is empty, so header and payload are separated by a client
  round trip ⇒ whether they land in one TCP segment is a race, and the assertion was reading
  **segment boundaries**, not delivery. Measured 2026-09-02: red in a full gate run, green on a
  rerun of the same tree with **zero** changes; forcing a 150 ms gap before the payload made it
  red **100 %** of the time, which is what turned a guess into a mechanism.
  ★ ★ **The expensive part is not the flake — it is what a flake teaches people.** Once a line is
  known to go red at random, a real regression on that line reads as "that flaky one again", and
  the criterion has quietly become worse than no criterion.
  ⇒ Fix by making the **sender** say it is finished — half-close (`shutdown(SHUT_WR)`) and read to
  EOF — a structural end marker instead of a timing one. Keep a socket timeout as a backstop and
  say in the comment that the backstop is **not** the criterion, or the next reader will tune it.
  ⚠ Same shape, different clothes: any assertion whose subject is "what had arrived by the time I
  looked" — one `recv`, one `read`, a fixed `sleep` before scraping a log, a single poll of a
  metrics endpoint. Ask what makes the observation point the *right* one; "it passed" does not.

Two corollaries:

- **Never anchor a judgment on the exit code of the thing that triggers the work.** `systemctl
  reload` returns 0 whether or not the re-exec succeeded. Anchor on what the world looks like
  afterwards (did the pid file change? what does `/proc/<pid>/exe` say?).
- **A broken measuring instrument reports a broken subject**, and the report reads exactly like
  a real defect. When a new gate goes red, first run it on a clean tree.

## Host traps 4-9 (Windows + Git Bash / MSYS)

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
9. **`shellcheck` does not see a variable mutated inside a function that is called in `$(…)`.**
   Measured on 0.10: SC2030/SC2031 fire for `( B=1 )` and `X=$(D=9; …)`, and say nothing about
   `f(){ A+=(x); }; X=$(f)` — the exact shape that leaked a process for three months. Register the
   pid in the caller, and let `cleanup` assert the ports came back. ⚠ A comment line starting with
   `# shellcheck` is parsed as a *directive* (SC1073) — reword rather than indent it.

## Dependency check exit codes and caveats

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
