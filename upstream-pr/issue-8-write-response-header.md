# Issue — 投稿八（⏳ **材料已备，未发**；owner 2026-10-16 拍「投稿材料先备不发」）

> 对应 fork **改动 19（PLAN G165）**的 ① ②，两处一起投。
>
> 模板：上游 `.github/ISSUE_TEMPLATE/feature_request.md` 的四个小标题（性能改进不是缺陷，⛔ 不用 bug_report）。
> 补丁：[`0009-…`](0009-Avoid-two-allocations-when-writing-an-HTTP-1-response-header.patch)，
> 基于上游 `main` = `4487f7b`（2026-09-11；2026-10-16 克隆时仍是它）。PR 正文草稿：[`pr-8-write-response-header.md`](pr-8-write-response-header.md)。
>
> ⚠ 文中每个数都能在上游复现：分配次数来自补丁自带的测试（`tests/write_response_header_allocations.rs`），
> 4 KiB 是 `INIT_HEADER_BUF_SIZE`。⛔ fork 侧诊断台的计数（每请求 malloc 18 → 16 等）**没有**写进来。

**Title**

```
HTTP/1 write_response_header() makes two avoidable allocations per response
```

---

**Body**

## What is the problem your feature solves, or the need it fulfills?

`HttpSession::prepare_response_header()` (`pingora-core/src/protocols/http/v1/server.rs`) makes two
heap allocations on every HTTP/1 response that the response does not need:

1. The `Connection` value is inserted as a `&str` (`"keep-alive"` or `"close"`). `&str` converts to
   `HeaderValue` through `HeaderValue::from_str()`, which copies it into a new `Bytes` each time.
   The comment above it already says `TODO: make these lazy static`.
2. The serialized header is returned as `Bytes`, frozen from a `BytesMut` allocated with
   `INIT_HEADER_BUF_SIZE` (4 KiB). A typical header is much shorter than that, and freezing a
   `BytesMut` whose length differs from its capacity allocates a shared header for the new `Bytes`.
   `write_response_header()` then only borrows those bytes for `write_all()` and drops them.

Neither is observable behavior; both are paid on every downstream HTTP/1 response.

## Describe the solution you'd like

1. Insert `HeaderValue::from_static("keep-alive")` / `HeaderValue::from_static("close")`, which do
   not allocate.
2. Have `prepare_response_header()` return the `BytesMut`. The direct write path borrows it as it
   does now; the proxy-task path, which hands the header to `send_header_task()` and so needs
   `Bytes`, freezes it at the call site, exactly as before.

The bytes written do not change. With both, `write_response_header()` makes one allocation for a
simple response (the serialization buffer) instead of three, counted with a counting global
allocator in a test that is part of the change.

I have this as one commit with tests and can open a PR if this direction is welcome.

## Describe alternatives you've considered

- Only (1): it is the TODO, but (2) is the same kind of per-response cost in the same function.
- Reusing one serialization buffer per connection, or serializing straight into the stream's write
  buffer, would also remove the remaining allocation, but it changes more code (the session is
  created per request); out of scope here.

## Additional context

Existing tests of `write_response_header()` mostly turn `update_resp_headers` off, so nothing pins
the `Date` / `Connection` lines it adds. The change adds a test that writes a keep-alive and a close
response and compares the bytes (except the date value).
