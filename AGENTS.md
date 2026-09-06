# AGENTS.md — 枢衡 Fulcrum

> Read [workspace rules](../AGENTS.md) and [dev_guide](../Docs/dev_guide.md) on entry; only Claude imports the latter via the root shim.

## Authorities and boundaries

- [`PLAN.md`](PLAN.md) owns scope, milestone exits, approved G-decisions and open D-questions. It wins on conflict. Current milestone and execution status belong only there; never copy status into README or this guide.
- [`docs/architecture/`](docs/architecture/index.md) is the technical baseline; the rest of the OKF bundle navigates the plan. [`vendor/pingora/FORK.md`](vendor/pingora/FORK.md) owns fork changes.
- G-rules require a new owner decision to change. Commit on `main`, without branches/PRs or amend/force. Agents never push; the owner batches pushes. Per-call approvals still apply.
- Performance claims require reproducible end-to-end measurement against PLAN §8; never write “N times faster”. Do not hand-write TLS, HTTP/2 state machines, HPACK or QUIC; use audited libraries.
- Expected state is the sole persistent authority. Runtime overrides are temporary and must remain visible in stats/API; never add another persistent write path.
- G115: docs/comments retain conclusions, not running accounts. `handoff/` stays local and ignored; `docs/` may hold stable conclusions and dated verification evidence, not parallel progress status.

## Interfaces that must survive changes

- G6/G104 fixes the TLS backend to BoringSSL. Dynamic certificate selection uses the same `SslContextBuilder::set_select_certificate_callback` for h1/h2 and h3/QUIC; do not split certificate-picking logic. The former rustls sites are converted.
- Lock, dependency-graph and linked-artifact checks cover different claims; see [supply chain](docs/platform/supply-chain.md). The artifact check runs only in the musl scenario and proves nothing about the glibc development artifact.
- tower middleware does not compose with Pingora phases. This project uses `HttpServerApp`/`ServerSession`, not `ProxyHttp`; `pingora-proxy` is not a dependency.
- When changing the admin API, read `crates/fulcrum-server/src/admin.rs` and PLAN G120. `POST /load?overrides=keep|clear` requires the query parameter, with no default or JSON envelope. Missing/invalid values reject with 400 before changing configuration. Structured configuration stays the same format as on disk; clearing must report each removed override.

## Commands and acceptance

Run from the Fulcrum repository root in Git Bash on this Windows host. Rust runs only in Docker (G107); native cargo/rustup presence is editor tooling, never permission to build. Do not install a native toolchain. The Linux gate requires Unix sockets, signals/fd handover, systemd, permissions, locking and network-failure scenarios.

| Trigger | Command | Preconditions and result boundary |
|---|---|---|
| Full development validation | `bash tests/m0/docker-run.sh` | Docker Desktop with Linux containers available; no concurrent gate on this checkout. Exit 0 with all applicable scenarios completed; reduced/skipped runs do not prove the full gate |
| Documentation changes | `python tools/docs-check.py` and `python tools/plan-refs.py` | Python available; use the current docs/PLAN tree. Exit 0 checks document structure/reachability and supported plan-reference consistency, not prose truth or product correctness |
| Dependency report | `python tools/dep-check.py` | Read supply-chain policy first; report uses upstream metadata/network and never authorizes adoption |
| Approved dependency adoption | `python tools/dep-check.py --apply` | Separate intentional dependency work under G29; adopts only updates clearing the 24-hour quarantine, including breaking majors. Inspect all result bits; a report alone is not acceptance |

Never run bare `cargo update`: it bypasses G29. Dependency changes require supply-chain checks and the applicable full gate.

Detailed flags, prerequisites, failure codes and per-scenario acceptance live in [build and test](docs/platform/build-and-test.md) and [host/gate traps](docs/platform/host-and-gate-traps.md). BUILD_ONLY omits tests; COMPILE_ONLY compiles test targets without running them; *_ONLY and *_TESTS switches narrow coverage. README's musl installation build is not the development gate. No gate success proves milestone completion or production readiness beyond PLAN exits.

## Gate and host discipline

- Prove each new gate can detect its intended failure, then restore the condition; prefer built-in negative controls. A green result alone is insufficient.
- After rebase/branch switch, stale cargo binaries can omit new tests. Require fresh `Compiling <crate>` evidence and follow the container-only mtime refresh in the traps guide before trusting the run.
- Target-cache volume names bind both image and checkout path. New worktrees build cold; the same tree shares its cache and takes a lock which refuses, names the holding PID and exits instead of queuing. Naming/lock authority: `tests/lib/vol-lock.sh`, self-tested by the gate.
- Scenarios share `:80`; an occupied port can block unrelated listeners. Every scenario must return ports on exit; follow the self-checking cleanup in `tests/quic-relay/run.sh`.
- The local pre-push hook runs shellcheck and all-target compilation, not tests. Fresh clones lack untracked hooks; installation and limits are in the build guide. A hook is not full-gate evidence and does not authorize an agent push.
- Preserve MSYS path handling: `MSYS_NO_PATHCONV=1` is essential for documented Docker invocations; without it Git Bash rewrites container paths.
- Do not edit shell scripts using heredoc-fed inline generators; escaping may change while bash -n still passes. Use patch/edit tools or a generator saved as a file.
- MSYS grep normalizes line endings; use byte counting such as `tr -dc '\r' < file | wc -c` for CR detection, and prove scanners can hit and miss.
- Do not edit shell scripts during a gate: bind-mounted Bash reads by byte offset. Rust/Markdown edits do not have this shell-reader failure mode.
- New top-level script directories must be covered by the shell inventory. “Derived” scans cover only their supplied roots; `tests/ci/shellcheck-all.sh` and its wider tracked-file probe must keep detecting omissions.

## Collaboration

Use this repository's GitHub Issues and [agent conventions](docs/agents/index.md). Preserve declared licensing and PLAN publication boundaries; public source is not a release.
