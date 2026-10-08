# Issue — 投稿七（⏳ **材料已备，未发**；owner 2026-10-08 拍「先备材料、暂不发」）

> 对应 fork **改动 16（PLAN G157）的 ② ③**（C1 / C2）。① （B2，l4 流不再记等待计时）**不在本份**：
> 上游有读者，在上游不是语义不变 —— 见 [`README.md`](README.md) 投稿七一节「B2 为什么不投」。
>
> 模板：上游 `.github/ISSUE_TEMPLATE/feature_request.md` 的四个小标题（性能改进不是缺陷，⛔ 不用 bug_report）。
> 补丁：[`0007-…`](0007-Parse-request-headers-into-uninitialized-header-slots.patch)（C1）·
> [`0008-…`](0008-Keep-request-header-refs-on-the-stack-instead-of-in-a-Vec.patch)（C2），
> 基于上游 `main` = `4487f7b`（2026-09-11）。PR 正文草稿：[`pr-7-read-request.md`](pr-7-read-request.md)。
>
> ⚠ 文中每个数都能在上游复现：`8 KiB` 是 `256 × size_of::<httparse::Header>()`（x86_64 上 32，容器实测）；
> 分配次数来自补丁自带的测试（`tests/read_request_allocations.rs`）。⛔ fork 侧诊断台的指令计数（−4.7% 等）**没有**写进来。

**Title**

```
HTTP/1 read_request() initializes 256 header slots and allocates a Vec<KVRef> on every request
```

---

**Body**

## What is the problem your feature solves, or the need it fulfills?

`HttpSession::read_request()` (`pingora-core/src/protocols/http/v1/server.rs`) does two pieces of
per-request work that the parse does not need:

1. Before every parse attempt it builds `[httparse::EMPTY_HEADER; MAX_HEADERS]`: 256 slots of
   32 bytes on 64-bit targets, so 8 KiB of stores per request, done again after every partial
   read. httparse overwrites each slot it hands back and never reads the others.
2. Once the header is complete it allocates a `Vec<KVRef>` (`Vec::with_capacity(req.headers.len())`)
   to hold the header offsets while the read buffer is frozen into `Bytes`. The `Vec` only lives
   until the headers are appended to the `RequestHeader` a few lines later.

Neither is observable behavior; both are paid on every downstream HTTP/1 request.

## Describe the solution you'd like

1. Parse with `httparse::Request::parse_with_uninit_headers()` into
   `[MaybeUninit<httparse::Header>; MAX_HEADERS]`. In httparse (1.10.1, the version that resolves
   today), `Request::parse()` is `parse_with_config(buf, &Default::default())`, which calls
   `parse_with_config_and_uninit_headers()`; `parse_with_uninit_headers()` calls that same function
   with the same default config. The only difference is what `req.headers` holds when the result is
   not `Complete`, and `read_request()` does not look at `req` in that case. The `patched_http1`
   path (`parse_unchecked()`) would keep its initialized slots.
2. Write the offsets into a stack array of `MaybeUninit<KVRef>` (there are at most `MAX_HEADERS`)
   through a sibling of `populate_headers()` that applies the same rule (unnamed entries are
   skipped) and returns the initialized prefix. That needs one small, commented `unsafe` block for
   the prefix cast. `populate_headers()` stays as it is for `client.rs`.

The one effect that can be checked in-tree is the allocation count: for a simple request with three
headers, `read_request()` makes 9 heap allocations on `main` and 8 with (2), counted with a
counting global allocator (the test is part of the change). (1) saves stores, not allocations, so it
has no in-tree number beyond the 8 KiB above.

I have both as two separate commits with tests, each applying to `main` on its own, and can open a
PR if this direction is welcome.

## Describe alternatives you've considered

- For (2), a fully initialized `[EMPTY_KV_REF; MAX_HEADERS]` avoids `unsafe`, but `KVRef` is also
  32 bytes, so it would trade the allocation for another 8 KiB of stores per request, the same kind
  of cost (1) removes.
- Keeping a reusable `Vec<KVRef>` in `HttpSession` does not help much: on a reused connection a new
  session is created for every request (`apps/mod.rs`, the keep-alive loop in `process_new`), so it
  would still allocate once per request.
- `read_response()` in `client.rs` has the same two patterns (and httparse has
  `ParserConfig::parse_response_with_uninit_headers()`). I left it out to keep the change small and
  can follow up if wanted.

## Additional context

- #1000 changes the same `httparse::Request::new(&mut headers)` line to
  `Request::new(&mut headers[..max_headers])`. The two compose (the uninit slots can be sliced the
  same way); I'm happy to rebase on whichever lands first.
- `parse_with_uninit_headers()` exists since httparse 1.5.0. `pingora-core` already needs at least
  1.6.0 (`ParserConfig::allow_obsolete_multiline_headers_in_responses()` in `client.rs`), so the
  effective minimum does not move. Inline `const` blocks in array repeat expressions need Rust 1.79,
  below the 1.85 MSRV.
