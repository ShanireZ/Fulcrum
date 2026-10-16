# PR — 投稿八（⏳ **材料已备，未发**；owner 2026-10-16 拍「投稿材料先备不发」）

> ★ 本目录纪律：**先 issue、后 PR**。先发 [`issue-8-write-response-header.md`](issue-8-write-response-header.md)，
> 拿到编号后把正文第一行的 `#<ISSUE_NUMBER>` 换掉。
> ⚠ 正文末段的 `#<H2_ISSUE_NUMBER>` 是**投稿九**的 issue 号（2026-10-16 补，那两条 h2 测试的根因）；
> 投稿九没发就把「which fail with h2 0.4.20, see …」那半句删掉。
>
> 补丁（基于上游 `main` = `4487f7b`，带 `Signed-off-by`，**一份 PR、一个提交**）：
> [`0009-Avoid-two-allocations-when-writing-an-HTTP-1-response-header.patch`](0009-Avoid-two-allocations-when-writing-an-HTTP-1-response-header.patch)。
> 验证与注入反证见 [`README.md`](README.md) 投稿八一节。
>
> ⚠ 机制上发 PR 前还要先把补丁推成 `ShanireZ/pingora` 上的一个分支（建议名 `write-response-header-allocations`）——
> 那也是一次以 owner 名义的外部写入，⛔ 未做。
>
> ⚠ 正文 Verification 一节的环境与数字是 2026-10-16 在 `4487f7b` 上跑的；发前若 `main` 动了，**重跑后再改**。

**Title**

```
Avoid two allocations when writing an HTTP/1 response header
```

**Base**: `cloudflare/pingora` `main` · **Head**: `ShanireZ/pingora` `write-response-header-allocations`

---

**Body**

Refs #<ISSUE_NUMBER>

`HttpSession::prepare_response_header()` makes two heap allocations per HTTP/1 response that are
not needed. This removes both; the bytes written do not change.

1. **Static `Connection` values.** `"keep-alive"` / `"close"` were inserted as `&str`, which
   `HeaderValue::from_str()` copies into a new buffer on every response. They are now inserted with
   `HeaderValue::from_static()`, as the `TODO: make these lazy static` comment asked.
2. **Unfrozen serialization buffer.** The header was serialized into a `BytesMut` of
   `INIT_HEADER_BUF_SIZE` and returned frozen. Freezing a `BytesMut` whose length differs from its
   capacity allocates a shared header, but `write_response_header()` only borrows the bytes for
   `write_all()` and drops them. `prepare_response_header()` now returns the `BytesMut`; the
   proxy-task path, which hands the header to `send_header_task()` and so needs `Bytes`, freezes it
   at the call site as before.

### Why behavior does not change

- `HeaderValue::from_static()` and `HeaderValue::from_str()` produce equal values for these
  strings (neither is marked sensitive), so the serialized header is the same.
- (2) changes where the buffer is frozen, not its contents. The direct path writes the same slice;
  the proxy-task path still freezes exactly once, and a missing `freeze()` there would not compile.

### Tests

`tests/write_response_header_allocations.rs`, in its own test binary with a counting global
allocator (only the counting thread is counted, on a current-thread runtime):

- `write_response_header_allocation_budget`: after reading a simple request, writing a
  `200` / `Content-Length: 0` response header makes at most 1 allocation (the serialization
  buffer), for both a keep-alive and a `Connection: close` request. On `main` it makes 3; with
  either change reverted, 2. The cached `Date` value is refreshed once a second and the first write
  after a refresh allocates more, so the test takes the fewest of three consecutive writes.
- `write_response_header_wire_bytes`: the same two responses are written to an in-memory stream and
  compared byte for byte, except the date value, which is checked to parse as an HTTP date. In
  `pingora-core`, the tests of `write_response_header()` mostly turn `update_resp_headers` off, so
  the `Date` and `Connection` lines it adds were not pinned there; the `Connection` value is checked
  end to end only by the `pingora-proxy` integration tests (`test_basic`, `test_upstream`), which
  need OpenResty.
- `counting_allocator_counts`: the allocator does count.

The stream is a `std::io::Cursor<Vec<u8>>` with its capacity reserved up front, so writing to it
does not allocate, and the written bytes are read back through `HttpSession::stream()`.

### Verification

I ran the steps of `.github/workflows/build.yml` on `main` (`4487f7b`) and on this patch applied
with `git am`, in a container built from `rust:1.97.1` with the CI's build dependencies and
OpenResty 1.27.1.2, plus `nightly` (1.101.0-nightly, 2026-10-15) and `1.85.0`. Each tree was
extracted with `git archive` and built in its own target directory; the lockfiles the toolchains
generated were identical.

- 1.97.1: `fmt`, `check --workspace`, `clippy --all-targets --all` (`--deny=warnings`) and
  `cargo machete` pass on both. `cargo test --verbose --lib --bins --tests --no-fail-fast`: 1280
  passed / 4 failed / 6 ignored on `main`, 1283 / 4 / 6 with the patch; the three extra are the new
  tests, and the failing sets are identical by test name. Doc tests: 7 passed on both.
- nightly: the same test and doc-test results; `cargo fmt --check` reports one diff in
  `pingora-cache/src/key.rs` on both trees, none in the files this touches.
- 1.85.0: `fmt` and `check --workspace --exclude pingora-foundations` pass on both.
- `cargo audit` reports the same advisories on both (for `rustls` 0.21.12, which this does not
  touch).
- The `pingora-proxy` integration tests that check the `Connection` value (`test_connect_close`,
  `test_duplex`, `test_close_on_response_before_downstream_finish`) pass with the patch.
- The new test file on unpatched `main`: the budget test fails with 3 allocations, the wire-bytes
  test passes (the bytes are the same). With only the `Connection` change or only the `freeze`
  change reverted, the budget test fails with 2.

The four tests failing on both trees are `connectors::l4::tests::test_bind_to_port_range_on_connect`,
two `http::v2::server` tests (`test_req_conflicting_content_length_rejected`,
`test_req_malformed_stream_budget_exhausted`, which fail with h2 0.4.20, see #<H2_ISSUE_NUMBER>) and
`pingora-load-balancing`'s `health_check::test::test_tcp_check`; they fail on `main` in the same
container and do not go through this code.
