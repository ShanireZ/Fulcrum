# AGENTS.md — 枢衡 Fulcrum project guide

> Workspace-wide rules live in [`../AGENTS.md`](../AGENTS.md).

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
- **Status sentences drift.** Current milestone and execution status live only in `PLAN.md` §1;
  other entry points link there and must not restate it.
- **Docs and comments carry conclusions, not a running account** (G115). `handoff/` is
  gitignored and stays local; milestone status belongs in `PLAN.md`, while `docs/` keeps stable
  technical conclusions or dated verification evidence rather than a second progress account.

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

Scenario flags (`*_ONLY=1` / `<NAME>_TESTS=0`), the per-scenario port map, and the shared-`:80`
trap are in [docs/platform/host-and-gate-traps.md](docs/platform/host-and-gate-traps.md).
⚠ **Port `:80` is shared on purpose and an occupied `:80` can take down listeners that have
nothing to do with it** — the symptom lands on an innocent port. A scenario must hand `:80` back
when it exits; copy the `cleanup` check in `tests/quic-relay/run.sh`.

⚠ **A green run right after a `git rebase` or a branch switch is not to be trusted on its own** —
cargo can reuse a stale test binary and the new tests never run. See **Gate discipline**.

★ **The cargo `target` cache lives in a docker volume named after *both* the build image and the
checkout path**, so every git worktree gets its own and the first run in a fresh worktree is a
cold build. One shared volume let cargo reuse another worktree's `.rlib` by mtime, and the
compile error that came out of it named an innocent file. Splitting by tree only handles *other*
trees: two gates on the **same** tree still share that one volume, so a run first takes a lock
named after it — ⚠ **it refuses and exits, naming the holding pid. It does not queue.** The
naming rule and the lock are both in `tests/lib/vol-lock.sh`, and `docker-run.sh` self-tests
both on every run.

Per-scenario detail: [docs/platform/build-and-test.md](docs/platform/build-and-test.md).

## Gate discipline

**A gate that has only ever been observed green is indistinguishable from a gate that does not
run.** Flip the condition, break the input, or delete the fix — and watch it fail, then put it
back. If a gate cannot be made to fail, it is not a gate. Prefer gates carrying their own reverse
test (`tests/m0/unclaimed.sh` asserts false at step 0 and true at step 2, so one run does both).

⚠ ★ **A cross-crate injection is not fully reported by one compile** — cargo stops at the first
failing crate and the truncated output reads exactly like a complete list (measured: 7 sites
reported as 1). The criterion, and why one pass is sometimes enough by luck: the traps doc below.

⚠ ★ **On this host `cargo` can reuse a stale test binary after a `git rebase` or branch switch:
the run is green and the new tests are simply not in it.** Measured: two identical `UNIT_ONLY`
runs reported **289** and **288** tests while the source had **291**. Anchor on `Compiling
<crate>` in the log, not on the green summary, and refresh mtimes from inside the container first:

```bash
MSYS_NO_PATHCONV=1 docker run --rm -v "$(cygpath -m "$PWD"):/w" -w /w fulcrum-build:local bash -c 'find crates spikes -name "*.rs" -o -name Cargo.toml | xargs touch'
```

⚠ **`MSYS_NO_PATHCONV=1` is load-bearing here, not decoration** — without it Git Bash rewrites
`-w /w` into `W:/` and docker exits 125 with an error that says nothing about path translation.

The other failure shapes and their corollaries: [docs/platform/host-and-gate-traps.md](docs/platform/host-and-gate-traps.md).

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
Traps 1 and 2 are gates: `tests/ci/shellcheck-all.sh` runs `shellcheck` over every
`tests/**/*.sh` — the file set is **derived** with `find`, never listed, and its enumerator
self-tests against a fixture tree (two levels deep, a path with a space) on every run — and
`docker-run.sh` self-tests its byte probes against known-CRLF, known-LF and known-binary fixtures
on every run.

## Dependencies

Updates follow G29 — chase latest, including breaking majors, with a 24-hour quarantine. Exit
codes stack one bit per check; all of them report and refuse to auto-apply. Codes and caveats:
[docs/platform/host-and-gate-traps.md](docs/platform/host-and-gate-traps.md).

```bash
python tools/dep-check.py            # report only
python tools/dep-check.py --apply    # adopt whatever cleared quarantine
```

⛔ Never run a bare `cargo update`; it skips the quarantine window G29 exists to enforce.

Read [docs/platform/supply-chain.md](docs/platform/supply-chain.md) before touching dependencies.

## Agent workflows

- **Issue tracker** — GitHub Issues in this repository: `docs/agents/issue-tracker.md`.
- Triage labels, domain-doc layout and the OKF documentation contract follow the workspace defaults: `docs/agents/index.md`.
- Read the workspace-root [`Docs/dev_guide.md`](../Docs/dev_guide.md) when entering this workspace for phase-by-phase rules, completion criteria, and the skill mapping. Claude imports it through the root `CLAUDE.md`; other runtimes must not assume automatic loading.
