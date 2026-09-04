# PR draft — 投稿六（⛔ 未发；发要 owner 按 G40 单独批）

> ★ 依据本目录纪律：**先 issue、后 PR**。issue 已发＝
> [cloudflare/pingora#994](https://github.com/cloudflare/pingora/issues/994)，
> 并已在其下发过一条**更正评论**（形状因此从「给 `ConnectionFilter` 加 `connection_closed`」
> 换成「让监听器侧能挂 `Tracer`」）。⚠ **本 PR 对应的是更正后的形状。**
>
> 补丁本体：
> [`0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch`](0006-Report-downstream-connection-lifetime-through-a-listener-Tracer.patch)
> （基于上游 `main` = `09696b5`，带 `Signed-off-by`）。验证表见
> [`issue-6-connection-counter.md`](issue-6-connection-counter.md) §2.2。
>
> ⚠ **机制上还差一步**：要先把补丁推成 `ShanireZ/pingora` 上的一个分支
> （建议名 `listener-tracer`）—— 那也是一次以 owner 名义的外部写入。

**Title**

```
Report downstream connection lifetime through a listener Tracer
```

**Base**: `cloudflare/pingora` `main` · **Head**: `ShanireZ/pingora` `listener-tracer`

---

**Body**

Refs #994

`Stream` already carries an optional `Tracer` and reports `on_disconnected()` from its `Drop`,
but only the upstream connector ever sets it (`connectors::l4`). Downstream connections have no
equivalent. An application implementing `ServerApp` can set `session.tracer` once `process_new`
hands it the stream, but by then the handshake has already succeeded, so connections that time
out or fail during `io.handshake()` are never seen — and for an entry point those are the
interesting ones.

This adds an optional `Tracer` to `Listeners`, threads it through to each `TransportStack`, and
attaches a clone of it to every accepted stream, the same way `connectors::l4` does for outbound
connections:

```rust
let tracer = tracer.clone();
tracer.0.on_connected();
stream.tracer = Some(tracer);
```

`Stream`'s `Drop` already reports the disconnect, so the two calls pair by construction: no
ordering question, no double counting, and no way to add one without the other.

### Why this covers the handshake window

The tracer rides on the L4 stream, and `UninitializedStream::handshake()` keeps that stream
alive in both branches — `Ok(Box::new(self.l4))` when there is no TLS, and
`tls.tls_handshake(self.l4)` when there is. A connection that never completes the handshake
therefore still reports its disconnect when the stream is dropped.

### Scope

- One file. One new public method (`Listeners::set_tracer`), shaped after the existing
  `set_pre_tls_callback`. No signature changes and no new trait.
- No behaviour change when no tracer is set, which remains the default.
- Nothing here knows about metrics: the tracer is the caller's, exactly as it already is for
  upstream connections.
- Attributing connections to a particular listener is deliberately out of scope; #941 would
  provide that by construction, since a filter instance attached to one address already knows
  its address.

### Tests

Two are added, both in `listeners::test`:

- a connection dropped without ever handshaking reports `on_connected` and `on_disconnected`
  exactly once each — the window an application-side counter cannot see;
- the no-tracer default path is unchanged, which fails if `accept()` ever starts assuming a
  tracer is present.

### Verification

Against `main` @ `09696b5`, in a container:

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | passes |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | passes |
| `cargo test -p pingora-core --lib` | 557 passed / 2 failed, against 555 / 2 on the unpatched baseline |
| `cargo test --workspace --lib --bins --tests` | identical set of failing test names before and after; only the passed count moves, by the two tests added here |
| `git am` replay | applies cleanly on `09696b5`, and the replayed tree is byte-identical to the development tree |

The two `pingora-core` failures (`connectors::l4::tests::test_conn_timeout` and
`test_bind_to_port_range_on_connect`) and the `pingora-proxy` integration failures are present on
the unpatched baseline as well; the latter need openresty, which the container does not have.
MSRV (`cargo +1.85.0 check`), `cargo audit` and `cargo machete` were not available locally and
are left to CI.
