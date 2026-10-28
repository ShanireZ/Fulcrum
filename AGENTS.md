# Fulcrum contributor and agent guide

In a managed workspace, read `../AGENTS.md` when it exists. A standalone clone does not require sibling workspace files.

Keep each document's prose consistent with its intended language: English editions use English, and localized editions use their respective languages. Keep the English, Simplified Chinese, Taiwan Traditional Chinese and Japanese README editions synchronized with links to all four. Preserve exact interface identifiers, diagnostic/output tokens, upstream/vendor legal text and immutable evidence; explain those exceptions in the document's language rather than silently translating their contents.

## Authorities

- [PLAN.md](PLAN.md) owns product scope, milestone exits, approved G-decisions and open D-questions. Change a G-rule only through a new owner decision. Keep current milestone state there.
- [Architecture](docs/architecture/index.md) is the approved technical baseline and yields to PLAN. [FORK.md](vendor/pingora/FORK.md) owns vendored changes.
- Keep operating records, approval quotations, temporary designs and handoffs outside Git. `handoff/` is ignored. Preserve unrelated working-tree changes; inspect status/log before and after edits.

## Implementation boundaries

- Rust builds/tests run only in Docker (G107). A host toolchain is editor tooling. No native cargo/rustup builds or toolchain installation.
- TLS uses BoringSSL (G104). Both h1/h2 and h3 use one `set_select_certificate_callback` implementation. Do not hand-write TLS, HTTP/2 state machines, HPACK/QPACK or QUIC.
- Use HttpServerApp/ServerSession; pingora-proxy is not a dependency and Tower middleware does not compose with Pingora phases.
- Expected state is the sole persistent authority. Runtime overrides remain temporary and visible. For admin changes read `crates/fulcrum-server/src/admin.rs`, PLAN G120 and the control-plane baseline.
- `/load?overrides=keep|clear` requires exactly one query value, rejects invalid/missing input before mutation, accepts the on-disk JSON format without an envelope, and reports removed overrides individually.
- Performance claims require reproducible §8 comparisons; diagnostics and a green development gate do not establish production readiness or milestone completion.

## Commands

Run from this repository root in Bash (Git Bash on Windows):

| Change | Validation |
|---|---|
| Development/full applicable gate | `bash tests/m0/docker-run.sh` |
| Documentation | `python tools/docs-check.py` and `python tools/plan-refs.py`, plus affected compiled documentation contracts |
| Dependency report | `python tools/dep-check.py` (container cargo by default) |
| Intentional approved dependency adoption | `python tools/dep-check.py --apply`, supply-chain checks and the applicable full gate |

Never run bare cargo update: G29's 24-hour quarantine and supply-chain gates apply. Compiler pins must agree (G167); M3 freeze rules still apply. The [build guide](docs/platform/build-and-test.md) and [host/gate traps](docs/platform/host-and-gate-traps.md) define prerequisites, flags and coverage. BUILD_ONLY/COMPILE_ONLY or narrowed scenarios do not prove a full gate. README's installation build is separate.

## Gate discipline

- Prove new/changed criteria detect representative defects, using copies and built-in negative controls. An unreadable/unexecuted check is not a pass.
- Require fresh compilation after branch switches; stale target mtimes can omit tests. Use the documented container-only refresh.
- Target volumes bind image and checkout path; the per-tree lock refuses concurrent gates and names the holding PID. Authority: tests/lib/vol-lock.sh. Return all scenario ports, including implicit :80, on exit.
- Preserve MSYS_NO_PATHCONV=1 for documented Docker commands in Git Bash. Inspect bytes for CR detection; MSYS grep normalizes line endings.
- Do not edit bind-mounted shell scripts during a gate, or generate them through heredoc-fed inline shell editors. Derived shell inventory must cover new script roots and unignored untracked scripts.
- The local pre-push hook compiles every target. Shellcheck depends on pushed net shell-input changes; host self-tests depend on a worktree-local fingerprint. It is not full-test evidence. Fresh clones need hook installation; see the build guide.

Work on main without amend/force/history rewrites. Public writes (push, PR, merge, release, deployment or upstream submission) require explicit authorization for the particular operation. GitHub is the development authority; mirrors are separate operations.
