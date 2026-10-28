# Pingora contribution provenance

The implemented Fulcrum fork and its removal conditions are documented in
[FORK.md](../vendor/pingora/FORK.md). Reusable contribution rules are in the
[upstream contribution guide](../docs/platform/upstream-pr.md).

Related public upstream contributions:

| Topic | Issue | Pull request |
|---|---|---|
| lru security requirement | [#961](https://github.com/cloudflare/pingora/issues/961) | [#962](https://github.com/cloudflare/pingora/pull/962) |
| Transfer socket ownership and CLOEXEC | [#959](https://github.com/cloudflare/pingora/issues/959) | [#960](https://github.com/cloudflare/pingora/pull/960) |
| rustls provider default features | [#965](https://github.com/cloudflare/pingora/issues/965) | [#966](https://github.com/cloudflare/pingora/pull/966) |
| UDS short-read test | [#967](https://github.com/cloudflare/pingora/issues/967) | [#968](https://github.com/cloudflare/pingora/pull/968) |
| Listener connection lifetime | [#994](https://github.com/cloudflare/pingora/issues/994) | [#995](https://github.com/cloudflare/pingora/pull/995) |

These are provenance links, not a live status report. Check the upstream source
and release ancestry before removing a fork delta; PR closure alone does not
establish either adoption or rejection. Draft issue/PR text, prepared patches,
authorization records and execution receipts are maintained outside this repository.
