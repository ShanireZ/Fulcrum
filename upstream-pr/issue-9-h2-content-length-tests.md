# Issue — 投稿九（⏳ **材料已備，未發**；owner 2026-10-16 拍「改準材料＋備投稿九」，發不發另拍）

> 對應 fork **改動 20**（`vendor/pingora/FORK.md`）：兩條上游 h2 測試在 h2 0.4.20 下紅。
>
> 範本：上游 `.github/ISSUE_TEMPLATE/bug_report.md` 的小標題（測試與相依的行為對不上，是缺陷不是新功能）。
> 補丁：[`0010-…`](0010-Adapt-two-h2-server-tests-to-h2-0.4.20.patch)，基於上游 `main` = `4487f7b`（2026-09-11；2026-10-16 查仍是它）。
> PR 正文草稿：[`pr-9-h2-content-length-tests.md`](pr-9-h2-content-length-tests.md)。
>
> ★ **上游已經有人提過，但只在別人的 PR 裡提了一句**：PR #816 作者 2026-10-06 17:05 UTC 的評論
> （https://github.com/cloudflare/pingora/pull/816#issuecomment-6021364933）—— 同樣的兩條、同樣「0.4.20 紅 / 0.4.19 綠」，
> 並預言 `main` 下次跑也會紅。沒有 issue、沒有修它的 PR（G46 查重見 README 投稿九一節）⇒ 正文引用它，⛔ 不另起爐灶地重述成「我們發現的」。
>
> ⚠ 「CI 會紅」這句**是核過的**（投稿四那條教訓）：兩條測試在預設 feature 下就編、就跑（`cargo test -p pingora-core --lib` 實測 14 條 v2 server 測試都跑了）；
> 上游 CI 不帶鎖、現生成 ⇒ 2026-10-06 之後必解析到 h2 0.4.20；#816 那次 CI 正是紅在這兩條。

**Title**

```
Two h2 server tests fail with h2 0.4.20, which rejects conflicting Content-Length itself
```

---

**Body**

## Describe the bug

`protocols::http::v2::server::test::test_req_conflicting_content_length_rejected` and
`protocols::http::v2::server::test::test_req_malformed_stream_budget_exhausted` fail on `main` whenever
the lock resolves `h2` 0.4.20 (released 2026-10-06), which a fresh `cargo generate-lockfile` does, as
CI does. They pass with `h2` 0.4.19.

h2 0.4.20's changelog has "Fix rejecting duplicate `content-length` headers with different values":
`recv_headers` now resets a stream whose `content-length` values conflict with `PROTOCOL_ERROR`, inside
the codec, so `accept()` never returns that request. Both tests send exactly such a request
(`content-length: 5` and `content-length: 6`) and assert what Pingora's own acceptance check does with
it, which no longer runs.

This was already noticed in a comment on #816
(https://github.com/cloudflare/pingora/pull/816#issuecomment-6021364933); there is no issue for it yet.

## Pingora info

**Pingora version**: `main` @ `4487f7b`
**Rust version**: `cargo 1.98.0`
**Operating system version**: Debian 13 (trixie), x86_64, in a container

## Steps to reproduce

```sh
git checkout 4487f7b
cargo generate-lockfile                      # resolves h2 0.4.20
cargo test -p pingora-core --lib -- protocols::http::v2::server::test::test_req_
cargo update -p h2@0.4.20 --precise 0.4.19
cargo test -p pingora-core --lib -- protocols::http::v2::server::test::test_req_
```

## Expected results

Both tests pass regardless of which h2 version the lock picks, or the manifest requires the version
the tests assume.

## Observed results

With h2 0.4.20:

```
test protocols::http::v2::server::test::test_req_conflicting_content_length_rejected ... FAILED
test protocols::http::v2::server::test::test_req_malformed_stream_budget_exhausted ... FAILED
panicked at pingora-core/src/protocols/http/v2/server.rs:1860:9:
malformed request must not surface as a session
panicked at pingora-core/src/protocols/http/v2/server.rs:1917:25:
connection ended before malformed budget was exhausted
```

With h2 0.4.19 both pass.

## Additional context

The request is still rejected on the wire with `RST_STREAM(PROTOCOL_ERROR)`; only who sends it changed.
One real difference: such streams are no longer charged to `MAX_MALFORMED_STREAMS_PER_CONN` but to h2's
own local error-reset limit (`DEFAULT_LOCAL_RESET_COUNT_MAX`, 1024). Pingora's budget is still exercised
by the authority-mismatch and forbidden-target tests.

I have a commit that updates the two tests to the new behavior and raises the workspace `h2`
requirement to `>=0.4.20`, so the tests' assumption is in the manifest. I can open a PR if that
direction is acceptable.
