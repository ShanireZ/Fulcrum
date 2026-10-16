# Issue — 投稿九（⏳ **材料已备，未发**；owner 2026-10-16 拍「改准材料＋备投稿九」，发不发另拍）

> 对应 fork **改动 20**（`vendor/pingora/FORK.md`）：两条上游 h2 测试在 h2 0.4.20 下红。
>
> 模板：上游 `.github/ISSUE_TEMPLATE/bug_report.md` 的小标题（测试与依赖的行为对不上，是缺陷不是新功能）。
> 补丁：[`0010-…`](0010-Adapt-two-h2-server-tests-to-h2-0.4.20.patch)，基于上游 `main` = `4487f7b`（2026-09-11；2026-10-16 查仍是它）。
> PR 正文草稿：[`pr-9-h2-content-length-tests.md`](pr-9-h2-content-length-tests.md)。
>
> ★ **上游已经有人提过，但只在别人的 PR 里提了一句**：PR #816 作者 2026-10-06 17:05 UTC 的评论
> （https://github.com/cloudflare/pingora/pull/816#issuecomment-6021364933）—— 同样的两条、同样「0.4.20 红 / 0.4.19 绿」，
> 并预言 `main` 下次跑也会红。没有 issue、没有修它的 PR（G46 查重见 README 投稿九一节）⇒ 正文引用它，⛔ 不另起炉灶地重述成「我们发现的」。
>
> ⚠ 「CI 会红」这句**是核过的**（投稿四那条教训）：两条测试在默认 feature 下就编、就跑（`cargo test -p pingora-core --lib` 实测 14 条 v2 server 测试都跑了）；
> 上游 CI 不带锁、现生成 ⇒ 2026-10-06 之后必解析到 h2 0.4.20；#816 那次 CI 正是红在这两条。

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
