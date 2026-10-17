# PR — 投稿九（⏳ **材料已備，未發**；owner 2026-10-16 拍「改準材料＋備投稿九」，發不發另拍）

> ★ 本目錄紀律：**先 issue、後 PR**。先發 [`issue-9-h2-content-length-tests.md`](issue-9-h2-content-length-tests.md)，
> 拿到編號後把正文第一行的 `#<ISSUE_NUMBER>` 換掉。
>
> 補丁（基於上游 `main` = `4487f7b`，帶 `Signed-off-by`，**一份 PR、一個提交**）：
> [`0010-Adapt-two-h2-server-tests-to-h2-0.4.20.patch`](0010-Adapt-two-h2-server-tests-to-h2-0.4.20.patch)。
> 驗證見 [`README.md`](README.md) 投稿九一節。
>
> ⚠ 機制上發 PR 前還要先把補丁推成 `ShanireZ/pingora` 上的一個分支（建議名 `h2-0420-content-length-tests`）——
> 那也是一次以 owner 名義的外部寫入，⛔ 未做。
>
> ⚠ ★ **傳送順序**：它最好先於投稿七、八 —— 在它完成實作之前，任何新 PR 的 CI 都會紅在這兩條上。

**Title**

```
Adapt two h2 server tests to h2 0.4.20 rejecting conflicting Content-Length
```

**Base**: `cloudflare/pingora` `main` · **Head**: `ShanireZ/pingora` `h2-0420-content-length-tests`

---

**Body**

Fixes #<ISSUE_NUMBER>

h2 0.4.20 resets a request stream whose `content-length` values conflict with `PROTOCOL_ERROR` inside
the codec, before `accept()` returns it ("Fix rejecting duplicate `content-length` headers with
different values"). Two tests send exactly such a request and asserted what Pingora's own acceptance
check does with it; they fail on `main` once the lock resolves h2 0.4.20, as CI's fresh lock does
(also reported in https://github.com/cloudflare/pingora/pull/816#issuecomment-6021364933).

- `test_req_conflicting_content_length_rejected` now expects `from_h2_conn()` to return `None` once the
  client closes (the stream never reaches Pingora), within a 1 s timeout. The client side still asserts
  `PROTOCOL_ERROR`.
- `test_req_malformed_stream_budget_exhausted` used the request to exhaust
  `MAX_MALFORMED_STREAMS_PER_CONN`. That no longer happens (h2 charges its own local error-reset
  limit), so it is renamed `test_req_conflicting_content_length_not_charged_to_malformed_budget` and
  asserts the budget is left as it was. Exhausting the budget stays covered by
  `test_authority_mismatch_exhausts_malformed_stream_budget` and
  `test_forbidden_target_exhausts_malformed_stream_budget`.
- The workspace `h2` requirement goes from `>=0.4.16` to `>=0.4.20`, because both tests now depend on
  that behavior.

No non-test code changes. On the wire the request is still rejected with `RST_STREAM(PROTOCOL_ERROR)`.

**Verification** (container, Debian 13, rustc 1.98.0, `main` @ `4487f7b`, fresh lock):

- `cargo fmt --all -- --check`: pass.
- `cargo test -p pingora-core --lib -- protocols::http::v2::server`: 14 passed (h2 0.4.20).
- `cargo clippy -p pingora-core --all-targets -- --allow=unknown-lints --deny=warnings`: pass.
- With the patch, `cargo update -p h2@0.4.20 --precise 0.4.19` is refused by the new requirement.
  The two updated tests fail on h2 0.4.19, and the original tests fail on h2 0.4.20 and pass on 0.4.19.

**Alternative considered**: keep the old requirement and make the tests accept either outcome. That
would leave the budget test asserting nothing in particular, so I raised the requirement instead.
