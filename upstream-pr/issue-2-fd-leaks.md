# Issue draft — file this FIRST, then reference its number from the PR

> `CONTRIBUTING.md`: *"Non-trivial PRs will also require a GitHub issue."*
> A behaviour change in the graceful-upgrade path is not in their list of trivial fixes
> (typos / small refactors / docs), so the issue goes first.

**Title**

```
get_fds_from() leaks the accepted transfer socket, and received listener fds are not CLOEXEC
```

**Labels**: none preselected (the repo's issue templates don't require any)

---

**Body**

`get_fds_from()` in `pingora-core/src/server/transfer_fd/mod.rs` leaks two kinds of file
descriptors on every graceful upgrade. Both are present on `main` (`0046038`) and in `0.8.1`.

### 1. The accepted connection is never closed

```rust
let fd = match accept_with_retry_timeout(listen_fd.as_raw_fd(), max_retry) { ... };
...
//cleanup
if nix::unistd::close(listen_fd).is_ok() {
    nix::unistd::unlink(path).unwrap();
}
Ok((fds, msg.bytes))
```

`listen_fd` is closed, but `fd` — the connection returned by `accept()` — is a bare `RawFd`
with no `Drop`, and nothing closes it. Every completed upgrade leaks one connected unix
socket for the lifetime of the process. The early return from `msg.cmsgs()?` leaks it too.

Measured on a server that had gone through the upgrade path (`/proc/<pid>/fd` resolved
against `/proc/net/unix`):

| process | fds bound to the upgrade socket path |
|---|---|
| first generation (never received a transfer) | 0 |
| second generation (one transfer) | 1, `St=03` (CONNECTED) |

`St=03` rather than `01` confirms it is the accepted connection and not the listener.

### 2. Descriptors received over `SCM_RIGHTS` do not get `FD_CLOEXEC`

```rust
let msg: RecvMsg<UnixAddr> = socket::recvmsg(
    fd,
    &mut io_vec,
    Some(&mut cmsg_buf),
    socket::MsgFlags::empty(),   // no MSG_CMSG_CLOEXEC
)
```

These are listening sockets held for the rest of the process's lifetime, so they are
inherited by every child the process later `exec()`s. A child that outlives the server keeps
the port bound, which is the usual reason a restart fails with `EADDRINUSE` long after the
server is gone.

It also interacts with the cleanup added in *"Close unclaimed inherited listening sockets on
graceful upgrade"*: an fd that arrives through `exec()` is **not in the `Fds` table**, so
`listen_addresses()` cannot close it. Deployments that start the new generation by forking
from the old one — which is what you have to do when the service manager tracks the process's
cgroup rather than an arbitrary new process — receive each listening socket **twice**, and
only one of the two copies is visible to that cleanup.

Measured in that setup, counting how many fds refer to each listening socket:

| generation | fds per listening socket |
|---|---|
| 1st (binds them itself) | 1 |
| 2nd (receives them over `SCM_RIGHTS`) | 1 |
| 3rd (receives them, and inherits the 2nd's copies) | 2 |

The first generation's own fds come from `TcpListener::bind`, which sets `FD_CLOEXEC`, so they
are not inherited — which is why the doubling only appears from the third generation on.

### Suggested fix

Both are small and local to `get_fds_from()`:

- take ownership of the accepted connection with `OwnedFd` so it is closed on every path out
  of the function (including the `cmsgs()?` early return);
- pass `MsgFlags::MSG_CMSG_CLOEXEC` so the received descriptors get `FD_CLOEXEC` atomically,
  rather than setting it afterwards with `fcntl()` (which races with a concurrent `fork()`).

I have a patch with a regression test that fails on each defect independently, and I'd be
happy to open a PR if you'd like it in this form. Happy to split it into two commits, or to
drop the test, if you prefer.

### Environment

- `main` at `0046038`, and `0.8.1`
- Linux (the whole module is `#[cfg(target_os = "linux")]`)
