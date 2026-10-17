# 投稿四 · `test_connect_uds` 的短讀

> ✅ **2026-08-20 已按 G40 重審並行出。** 下面「正文」一節是**逐字發出去的英文**，
> 上面的中文是給我們自己看的賬。

## ★ 發之前重查的三件事（2026-08-20，實測，不是讀網頁）

1. **上游修了沒有** —— 克隆 `cloudflare/pingora`，`main` 仍停在 `0046038`
   （2026-08-07 起未動），`pingora-core/src/connectors/mod.rs:659` 一字未改。
2. **有沒有人報過** —— 搜 `test_connect_uds` / `read_exact` / `short read`：
   **零命中**；搜 `flaky` 命中的是 `#591`/`#740`（`test_eviction`，另一回事）
   與我們自己那幾條。
3. **上游規範** —— `.github/CONTRIBUTING.md` 說小修可以不開 issue，但這一條帶
   根因與重現，開 issue 更有價值（也與前三份投稿一致）；正文按
   `.github/ISSUE_TEMPLATE/bug_report.md` 的小標題排。

## ★ ★ ★ 複審推翻了草稿裡的一句話：**不能說「你們 CI 會紅」**

草稿原本把它寫成一條會間歇性打紅建置的 flaky 測試。**實測下來那句話是錯的**：

- 這個測試整塊在 `#[cfg(feature = "any_tls")]` 裡（`connectors/mod.rs:584-586`）；
- 而 `pingora-core` 與 `pingora` 的 `default` **都是 `[]`**，
  上游 CI 跑的 `cargo test --workspace --lib --bins --tests` 用的就是預設 feature。
- **實測**：`cargo test --workspace --lib --bins --tests -- --list | grep -c test_connect_uds`
  → **0**。它在上游 CI 裡**根本沒被編譯**。

★ 所以立論必須是「**任何帶 TLS backend 跑測試的人**會撞上」，而不是「你們 CI 紅了」。
⚠ 這與投稿一那條教訓是同一個形狀：**把立論寫成一件審閱者一查就知道是假的事，
會當場失去可信度**。

## ★ 實測矩陣（上游 `main` `0046038`，容器內，rustc 1.97.1，`--features rustls`）

| | 無擾動 | 擾動（`write_all` 拆成 1 位元組 + 20ms + 8 位元組）|
|---|---|---|
| 未打補丁 | 通過（偶發才紅）| ★ **3/3 全紅**，`left: [105, 0, 0, 0, 0, 0, 0, 0, 0]` |
| 打了補丁 | **5/5 綠** | **5/5 綠** |

## ★ 上游 CI 那幾道門（同一棵樹，打補丁前後各跑一遍）

| 門 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | ✅ rc=0 |
| `cargo check --workspace` | ✅ rc=0 |
| `cargo clippy --all-targets --all -- --allow=unknown-lints --deny=warnings` | ✅ rc=0 |
| `cargo test -p pingora-core --lib --features rustls --no-fail-fast` | **572 passed / 1 failed / 2 ignored，前後逐項相同** |
| `cargo +1.85.0 check`（MSRV）| 未跑（容器裡只有 1.97.1）。★ 但 MSRV 那檔跑的是 `cargo check`，**不編譯測試程式碼**，而本改動只動測試 ⇒ 結構上夠不到 |
| `cargo audit` / `cargo machete` | 未跑（本機沒裝），留給上游 CI |
| `git am` 重播 | ✅ 在 `0046038` 上乾淨應用，且與分支樹**逐位元組一致** |

⚠ ★ **那唯一一條失敗（`test_bind_to_port_range_on_connect`）在基線上也失敗**，與本改動無關。
★ ★ 而第一次跑出來的是「基線 7 條、打補丁後 6 條」——**差集不為空**。
沒有把它解釋掉，而是**把環境修對**：容器裡沒裝
`iptables -A OUTPUT -d 192.0.2.0/24 -j DROP`，於是那批拿 RFC 5737 位址當黑洞的連接超時測試
被 Docker 的網路替它應答（實測 1.7ms 就 CONNECTED）而恆紅且**抖**。
裝上之後兩側都變成 572/1/2，**差集為空**。
> ★ 「把環境修對，比把名單加長強」——名單越長，判據越鈍。

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
