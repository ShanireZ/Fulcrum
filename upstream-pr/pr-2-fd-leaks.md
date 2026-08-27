# PR draft — open only AFTER the issue exists, and reference it

**Title**

```
Close the transfer socket and set CLOEXEC on received listener fds
```

**Base**: `cloudflare/pingora` `main` · **Head**: your fork's `fd-leaks-in-get-fds-from`

---

**Body**

Refs #<ISSUE_NUMBER>

`get_fds_from()` leaks two kinds of file descriptors on every graceful upgrade:

- The connection returned by `accept()` is a bare `RawFd` that is never closed; only
  `listen_fd` is. Every completed upgrade leaks one connected unix socket for the lifetime of
  the process.
- `recvmsg()` is called with `MsgFlags::empty()`, so the listening sockets received over
  `SCM_RIGHTS` do not have `FD_CLOEXEC`. They are held for the rest of the process's lifetime
  and are therefore inherited by every child it later `exec()`s.

The second one also interacts with the cleanup added in *"Close unclaimed inherited listening
sockets on graceful upgrade"*: an fd that reaches a process through `exec()` is not in the
`Fds` table, so `listen_addresses()` cannot close it.

This takes ownership of the accepted connection with `OwnedFd` so it is closed on every path
out of the function (including the early return from `cmsgs()?`), and passes
`MSG_CMSG_CLOEXEC` so the received descriptors get `FD_CLOEXEC` atomically rather than via a
follow-up `fcntl()`, which would race with a concurrent `fork()`.

### Setting CLOEXEC cannot affect daemonization

Worth stating explicitly, since that is the obvious thing to worry about: `FD_CLOEXEC` only
takes effect on `exec()`, and nothing in this workspace execs. Daemonization goes through
`daemonix::Daemonize`, which forks — `fork()` copies the descriptor table regardless of
`FD_CLOEXEC`, so the daemon child keeps the received listeners exactly as before. A repo-wide
search finds no `Command::new`, `exec*` or `CommandExt` outside of tests.

### The test covers both defects independently

`test_receive_does_not_leak_fds` checks the `FD_CLOEXEC` bit on the received fd, and counts
the descriptors in this process that refer to a unix socket bound to the transfer path
(via `/proc/net/unix`, so it is not affected by unrelated fds opened by tests running in
parallel). Reverting either half of the fix makes it fail, with distinct messages:

| reverted | failure |
|---|---|
| `MSG_CMSG_CLOEXEC` | `fd received over SCM_RIGHTS is missing FD_CLOEXEC` |
| the `OwnedFd` | `the accepted transfer socket was left open` |

### Verification

Commands taken from `.github/workflows/build.yml`, run against `main` (`0046038`) with this
patch applied:

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo check --workspace` (1.97.1) | pass |
| `cargo +1.85.0 check --workspace --exclude pingora-foundations` | pass (MSRV unaffected) |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | pass |
| `cargo test -p pingora-core --lib --no-fail-fast` | 544 passed / 2 failed — **the same two** that fail on unmodified `main` (`connectors::l4::tests::test_conn_timeout`, `test_bind_to_port_range_on_connect`); two-way diff of the failure sets is empty |
| `cargo audit` | unaffected — no dependency change |
| `cargo machete` | not run (not installed locally); no manifest is touched |

The full workspace test suite was not run end to end here: it needs openresty as a test
backend, which this environment does not have. The change is confined to `pingora-core`, and
that crate's full lib test suite is in the table above with a baseline comparison.

### Scope note

`get_fds_from()` also creates its listening socket with `SockFlag::SOCK_NONBLOCK` but without
`SOCK_CLOEXEC`, and `accept_with_retry_timeout()` uses `accept()` rather than `accept4()`.
Those descriptors are closed before the function returns, so they only expose a narrow window
to a concurrent `fork()`. I left them alone to keep this focused on the two descriptors that
actually outlive the call — happy to add them if you'd rather close the window too.
