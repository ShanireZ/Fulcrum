# 投稿四 · `test_connect_uds` 的短读

> ✅ **2026-08-20 已按 G40 重审并发出。** 下面「正文」一节是**逐字发出去的英文**，
> 上面的中文是给我们自己看的账。

## ★ 发之前重查的三件事（2026-08-20，实测，不是读网页）

1. **上游修了没有** —— 克隆 `cloudflare/pingora`，`main` 仍停在 `0046038`
   （2026-08-07 起未动），`pingora-core/src/connectors/mod.rs:659` 一字未改。
2. **有没有人报过** —— 搜 `test_connect_uds` / `read_exact` / `short read`：
   **零命中**；搜 `flaky` 命中的是 `#591`/`#740`（`test_eviction`，另一回事）
   与我们自己那几条。
3. **上游规范** —— `.github/CONTRIBUTING.md` 说小修可以不开 issue，但这一条带
   根因与复现，开 issue 更有价值（也与前三份投稿一致）；正文按
   `.github/ISSUE_TEMPLATE/bug_report.md` 的小标题排。

## ★ ★ ★ 复审推翻了草稿里的一句话：**不能说「你们 CI 会红」**

草稿原本把它写成一条会间歇性打红构建的 flaky 测试。**实测下来那句话是错的**：

- 这个测试整块在 `#[cfg(feature = "any_tls")]` 里（`connectors/mod.rs:584-586`）；
- 而 `pingora-core` 与 `pingora` 的 `default` **都是 `[]`**，
  上游 CI 跑的 `cargo test --workspace --lib --bins --tests` 用的就是默认 feature。
- **实测**：`cargo test --workspace --lib --bins --tests -- --list | grep -c test_connect_uds`
  → **0**。它在上游 CI 里**根本没被编译**。

★ 所以立论必须是「**任何带 TLS backend 跑测试的人**会撞上」，而不是「你们 CI 红了」。
⚠ 这与投稿一那条教训是同一个形状：**把立论写成一件审阅者一查就知道是假的事，
会当场失去可信度**。

## ★ 实测矩阵（上游 `main` `0046038`，容器内，rustc 1.97.1，`--features rustls`）

| | 无扰动 | 扰动（`write_all` 拆成 1 字节 + 20ms + 8 字节）|
|---|---|---|
| 未打补丁 | 通过（偶发才红）| ★ **3/3 全红**，`left: [105, 0, 0, 0, 0, 0, 0, 0, 0]` |
| 打了补丁 | **5/5 绿** | **5/5 绿** |

## ★ 上游 CI 那几道门（同一棵树，打补丁前后各跑一遍）

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ rc=0 |
| `cargo check --workspace` | ✅ rc=0 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ rc=0 |
| `cargo test -p pingora-core --lib --features rustls --no-fail-fast` | **572 passed / 1 failed / 2 ignored，前后逐项相同** |
| `cargo +1.85.0 check`（MSRV）| 未跑（容器里只有 1.97.1）。★ 但 MSRV 那档跑的是 `cargo check`，**不编译测试代码**，而本改动只动测试 ⇒ 结构上够不到 |
| `cargo audit` / `cargo machete` | 未跑（本地没装），留给上游 CI |
| `git am` 回放 | ✅ 在 `0046038` 上干净应用，且与分支树**逐字节一致** |

⚠ ★ **那唯一一条失败（`test_bind_to_port_range_on_connect`）在基线上也失败**，与本改动无关。
★ ★ 而第一次跑出来的是「基线 7 条、打补丁后 6 条」——**差集不为空**。
没有把它解释掉，而是**把环境修对**：容器里没装
`iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`，于是那批拿 RFC 5737 地址当黑洞的连接超时测试
被 Docker 的网络替它应答（实测 1.7ms 就 CONNECTED）而恒红且**抖**。
装上之后两侧都变成 572/1/2，**差集为空**。
> ★ 「把环境修对，比把名单加长强」——名单越长，判据越钝。

---

# 正文（GitHub issue，逐字）

**Title**: `test_connect_uds` can fail on a short read

## Describe the bug

`connectors::tests::test_connect_uds` reads the mock server's 9-byte response with
`AsyncReadExt::read` and discards the returned length:

```rust
let mut buf = [0; 9];
let _ = stream.read(&mut buf).await.unwrap();
assert_eq!(&buf, b"it works!");
```

`read` completes as soon as *any* bytes are available, and a stream socket is free to
deliver `write_all(b"it works!")` in more than one segment. When that happens, `buf`
holds a partial message padded with zeros and the assertion fails. Because the length
is dropped, nothing in the output says "we only read N bytes" — the failure looks like
the server sent the wrong thing.

This is a latent flake rather than a guaranteed failure: on a loopback UDS the response
usually arrives in one piece.

Note this test lives under `#[cfg(feature = "any_tls")]`, and both `pingora-core` and
`pingora` default to no TLS feature, so the default `cargo test --workspace` used by CI
does not compile it. It affects anyone running the test suite with a TLS backend
enabled (`--features rustls`, `openssl`, `boringssl`, ...).

## Pingora info

**Pingora version**: `main` @ `0046038`
**Rust version**: `cargo 1.97.1`
**Operating system version**: Debian 13 (trixie), x86_64

## Steps to reproduce

The flake can be made deterministic by having the mock server split its write, which is
legal stream-socket behaviour. In `spawn_mock_uds_server`, replace

```rust
let _ = stream.write_all(response).await;
```

with

```rust
let (head, tail) = response.split_at(1);
let _ = stream.write_all(head).await;
tokio::time::sleep(std::time::Duration::from_millis(20)).await;
let _ = stream.write_all(tail).await;
```

then run:

```
cargo test -p pingora-core --lib --features rustls connectors::tests::test_connect_uds
```

## Expected results

The test reads the full 9-byte response and passes.

## Observed results

With the perturbation above, the test fails every time (3/3 runs):

```
assertion `left == right` failed
  left: [105, 0, 0, 0, 0, 0, 0, 0, 0]
 right: [105, 116, 32, 119, 111, 114, 107, 115, 33]
```

`105` is `i` — only the first byte had arrived. That `[105, 0, 0, ...]` shape is also
what the intermittent failure looks like when it happens on its own.

## Additional context

`stream.read_exact(&mut buf).await.unwrap()` fixes it; with the fix the test passes both
with and without the perturbation (5/5 each). I have a patch ready and will open a PR
referencing this issue.

For what it's worth, `pingora-core/src/listeners/mod.rs` has the same
`let _ = stream.read(&mut buf)` shape in `test_listen_tls`, but that one only drains the
request and never asserts on the contents, so it is not affected. I have left it alone.
