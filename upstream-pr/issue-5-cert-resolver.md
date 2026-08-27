# 投稿五（**已撤销，不发**）· rustls 监听器无法使用自定义证书解析器

> ❌ **状态：2026-08-20 撤销，owner 拍板「什么都不做」。**
> **不开 issue、不开 PR、也不去别人的线程里留言。** 下面的立论**没有错**，
> 但它**不该由我们再报一遍**——理由见下。fork 里那条改动照旧自己养着。
>
> ## ★ ★ ★ 撤销的理由：这件事上游早就有人报了，而且比我们做得多
>
> 按 G32「先查上游做没做、有没有人报过」搜了一遍（2026-08-20，`gh` 实测，不是读网页），
> **同一主题已有 9 条** issue/PR，最早的是 **2025-04-27**：
>
> | | 类型 | 状态 | 说的是什么 |
> |---|---|---|---|
> | [#594](https://github.com/cloudflare/pingora/issues/594) | issue | open，10 条评论 | 最早那条诉求：SNI based resolver |
> | ★ [#632](https://github.com/cloudflare/pingora/pull/632) | PR | open，`mergeable=false` | **就是我们这个写法**，见下 |
> | [#599](https://github.com/cloudflare/pingora/pull/599) | PR | open | 另一条路（cert bundle） |
> | [#726](https://github.com/cloudflare/pingora/pull/726) | PR | open | 收 `ServerConfig` |
> | [#832](https://github.com/cloudflare/pingora/issues/832) | issue | open | 同一诉求，换个说法 |
> | [#916](https://github.com/cloudflare/pingora/issues/916) | issue | open | 暴露 `Acceptor` 构造器 |
> | [#833](https://github.com/cloudflare/pingora/pull/833) | PR | **closed** | TlsAcceptCallbacks，作者转投 openssl |
> | ★ [#908](https://github.com/cloudflare/pingora/pull/908) | PR | open，8 条评论 | 接棒 #833，**当前最活跃**，一直在催评审 |
> | [#877](https://github.com/cloudflare/pingora/pull/877) | PR | **closed** | resolver support |
>
> ★ ★ **#632 与我们的做法逐点相同，而且更完整**：同样是给 `TlsSettings` 加
> `Option<Arc<dyn ResolvesServerCert>>`、`build()` 里二选一、`pingora-rustls` 再导出那几个类型；
> **它还顺手把 `build()` 那个 panic 改成了 `Result`**——那是我们 fork 里没动的。
> 而且已有 **3 个独立使用者**（JockeTF / tarka / sebadob 线上的项目）证实可用、各自维着 rebase。
>
> ★ ★ **它卡住的原因不是设计分歧，是合并冲突 + 维护者评审带宽**：
> #908 那条线里 `dpfeifer2` 从 6 月催到 8 月（`nojima` 说过「周末看一下」，之后没下文）。
> ⇒ **再开第 7 份只会让那个队列更堵**，而我们自己的 `README.md` 里就记着那条教训：
> **「我可以帮你报」在别人早就报过时，等于宣告自己没搜。**
>
> ## ⚠ 这条撤销**不影响** fork 里那条改动
>
> `TlsSettings::with_cert_resolver`（`FORK.md` §8）照旧留着——它是 M1 的承重墙。
> ★ 但要记住它的**归零条件变了**：不再是「等我们的投稿被接受」，而是
> **等 #632 或 #908 落地**。到那天把 fork 那条删掉，换成上游的接口。
>
> ---
>
> 以下是当初的草稿，**保留仅供归档**（如果哪天 owner 改主意要参与那几条线，立论现成）。

---

## 标题

rustls listener cannot use a custom `ResolvesServerCert` (no SNI-based certificate selection)

## 描述（正文草稿）

用 `feature = "rustls"` 时，`listeners::tls::TlsSettings` 没有任何办法提供
rustls 的 `ResolvesServerCert`：

| 入口 | 现状 |
|---|---|
| `TlsSettings::intermediate(cert_path, key_path)` | 只接受**文件路径**，`build()` 里写死 `with_single_cert` |
| `TlsSettings::with_callbacks()` | 在 rustls 后端**直接返回错误** |
| 自己构造 `Acceptor` | 字段私有 |
| `add_address(ServerAddress)` | 枚举只有 `Tcp` / `Uds` |

后果：

- 一个监听端口只能有**一张**证书；
- 换证书要重启或重载配置；
- **按 SNI 在握手期选证书 / 现签**（ACME 的 on-demand 形态）无从实现。

而 rustls 提供的正是这条路（`ResolvesServerCert`），且它是**唯一**一条——
openssl / boringssl 后端的 `certificate_callback` 在 rustls 下不存在，
`with_callbacks()` 自己也这么说。

## 建议的改法（最小）

给 `TlsSettings` 加一个可选的 resolver，并在 `build()` 里优先用它：

```rust
pub fn with_cert_resolver(resolver: Arc<dyn ResolvesServerCert>) -> Result<Self>;
```

有 resolver 时**完全不读证书文件**（于是 `intermediate()` 那个「文件读不到就 panic」
的行为与这条路无关）。

配套还要在 `pingora-rustls` 里再导出 `ResolvesServerCert` / `CertifiedKey` / `ClientHello`——
否则依赖方拿不到那几个类型，只能自己再依赖一份 `rustls`，
而那会**多一处 feature 声明**（`rustls` 的 default 里含 `aws_lc_rs`，
参见投稿三 #965/#966 里那条「两个 provider 一起编进产物」）。

## 为什么这不是「用别的后端就好」

`certificate_callback` 只在 openssl / boringssl 后端存在。选 rustls 的项目
（纯 Rust、可 musl 静态链接）目前**没有任何**做 SNI 动态证书的办法。

## 环境

- 上游 tag `0.8.1`
- 我们在 fork 里按上面的形状加了这条并跑通：`--cacert` 验签通过、
  ALPN 协商到 h2、未知 SNI 被拒绝握手（rustls 报
  `no server certificate chain resolved`，正是期望的行为）。
