//! 响应压缩（**M2 批 I**，`encode` 指令接线；G99–G102）。
//!
//! ## ★ 用的是 fork 里那份，不是自研（**G100**）
//!
//! `brotli` / `flate2` / `zstd` 本来就是 `pingora-core` 的**非可选**依赖，
//! 压缩那 1365 行也未被 feature 门控 —— **它们今天就已经在二进制里** ⇒ 接线**新增 0 个包**。
//! ⚠ 与 G82「缓存层完全自研」不矛盾，因为成本结构是反的：那次自研是为了不把
//! 8707 行 + 7 个新包吃进 fork，而这一份已经在里面了。
//!
//! ★ 它已经做对的几件容易漏的事（读的是源码）：按 RFC 9110 补 `Vary: Accept-Encoding`、
//! 把强 ETag 弱化成 `W/"…"`、`101` 不转换、无体不转换、`Content-Type` 含 `zip` 的不压、
//! 体小于 20 字节不压。
//!
//! ## ⚠ ⚠ 两条**上游的**已知局限，写在明处
//!
//! 1. **`Accept-Encoding` 的 `q` 值被忽略**（上游源码里就写着 `// TODO: support q value`）。
//!    ⇒ `Accept-Encoding: gzip;q=0, br` 会被当成「首选 gzip」，而客户端其实**拒绝**了 gzip。
//!    ★ 这一条本模块**没有替它修**：修了会让「我们算的」与「它压的」分家，
//!    而分家的表现比这条局限本身更坏（见下面 [`preferred_encoding`] 那段）。
//! 2. **只按客户端的第一顺位挑，不回落到我们的次选**。配的是 `encode gzip`
//!    而客户端发 `br, gzip` 时，它选 br、发现 br 被禁用（level 0），于是**整个不压**。
//!    ⇒ 少压一次，不会压错 —— 代价方向是安全的。

use fulcrum_runtime::Routed;
use pingora_core::protocols::http::compression::{Algorithm, ResponseCompressionCtx};
use pingora_http::{RequestHeader, ResponseHeader};

/// 每种算法各自的默认压缩级别。
///
/// ★ ★ **有意不是「一个数配三种」**，而上游的 `ResponseCompressionCtx::new` 恰恰是那样：
/// 它把同一个 `compression_level` 塞给三种算法。⚠ 那个数在三种算法上**根本不可比** ——
/// brotli 的 11 与 gzip 的 11（其实 gzip 只到 9）不是同一件事，
/// 而 brotli 高质量档在动态内容上慢到不能用。
/// ⇒ 这里逐个 `adjust_algorithm_level` 覆盖成各自领域里的常用值。
const GZIP_LEVEL: u32 = 6;
const ZSTD_LEVEL: u32 = 3;
const BROTLI_LEVEL: u32 = 5;

/// 我们认得的三种编码。★ 与配置层 `ArgType::Enum(&["gzip", "zstd", "br"])` 逐字对应 ——
/// ⚠ 两处分家的表现是「配置里写得下、运行时不认」，而那不会有任何报错。
pub const KNOWN: &[&str] = &["gzip", "zstd", "br"];

/// 没压（或压不了）时，缓存次级键上用的那个值。
pub const IDENTITY: &str = "identity";

/// 一次响应的压缩上下文。
pub struct Encoder(ResponseCompressionCtx);

impl Encoder {
    /// 按这条链上 `encode` 要求的算法建一个。链上没写 `encode` ⇒ `None`。
    ///
    /// ⚠ `decompress_enable = false`：我们不替上游解压。一个已经压好的上游响应，
    /// 客户端认就原样转、不认就……上游的事。★ 打开它等于**默默改写别人的字节**，
    /// 而那是一条没人要求过的行为。
    /// ⚠ `preserve_etag = false`（**G102**）：压完把强 ETag 弱化成 `W/"…"`。
    pub fn new(requested: &[String], req: &RequestHeader) -> Option<Encoder> {
        if requested.is_empty() {
            return None;
        }
        // ★ 先全关（level 0 = 禁用），再把配置点名的那几种打开 ——
        //   ⚠ 反过来写（先全开再关掉没点名的）在加第四种算法时会**默认放行**它，
        //   而「配置里没写却生效了」是本仓库反复点名的那一类。
        let mut ctx = ResponseCompressionCtx::new(0, false, false);
        for name in requested {
            let (algo, level) = match name.as_str() {
                "gzip" => (Algorithm::Gzip, GZIP_LEVEL),
                "zstd" => (Algorithm::Zstd, ZSTD_LEVEL),
                "br" => (Algorithm::Brotli, BROTLI_LEVEL),
                // ⚠ 走不到：配置层已经用 Enum 拦过。留着不 panic —— 一个
                //   「配置里多了一种编码」的将来，不该让数据面倒下。
                _ => continue,
            };
            ctx.adjust_algorithm_level(algo, level);
        }
        ctx.request_filter(req);
        Some(Encoder(ctx))
    }

    /// 响应头进来一次（会补 `Vary`、弱化 ETag、去掉 `Content-Length` 与 `Accept-Ranges`）。
    pub fn header_filter(&mut self, resp: &mut ResponseHeader, no_body: bool) {
        self.0.response_header_filter(resp, no_body);
    }

    /// 体的每一块进来一次。返回 `None` = 这一块不用替换（没在压）。
    pub fn body_filter(&mut self, data: Option<&bytes::Bytes>, end: bool) -> Option<bytes::Bytes> {
        self.0.response_body_filter(data, end)
    }
}

/// **这次请求最终会拿到哪种编码** —— 缓存次级键就用它（**G101**）。
///
/// # ★ ★ ★ 为什么缓存的次级键不能直接用 `Accept-Encoding` 的原值
///
/// G101 拍的是「压完再存」，于是缓存里存的是**压缩后**的字节，
/// 而 pingora 会照 RFC 9110 给这种响应补上 `Vary: Accept-Encoding` ——
/// ⇒ 缓存按这个头分身。⚠ 而浏览器发的是 `gzip, deflate, br, zstd` 的**各种顺序与写法**：
/// 拿原值当次级键，同一条 URL 会存出几十份**内容完全相同**的条目，
/// 而现场表现是「缓存命中率莫名其妙地低、磁盘莫名其妙地满」。
///
/// ⇒ 归一化成「我们实际会发的那一种」，取值只剩 `gzip` / `zstd` / `br` / `identity` 四个。
/// ★ nginx 与 Varnish 都是这么干的。
///
/// # ⚠ ⚠ 正确性建在哪一条上（而**不是**建在「它与上游的挑法一致」上）
///
/// 这个函数**不试图预测** `decide_action` 会怎么选 —— 那是一个会分家的模型，
/// 而分家的模型比没有模型更危险。它要的只有一条：
///
/// > **存与查用的是同一个函数，且它只看请求。**
///
/// 于是「映射到同一个次级键的两个请求」必然有同一个首选算法，
/// 也就必然都接受存进去的那份编码。★ 而条目自己带着 `Content-Encoding` 头，
/// 所以发出去的东西永远是自描述的。
///
/// ⚠ 一处**已知的、可接受的**浪费：上游响应不可压时（图片、`Content-Encoding` 已存在、
/// 体太小），我们照样把 identity 的字节存在 `gzip` 这个次级键下。
/// ⇒ 一个只发 `identity` 的客户端会算出 `identity` 而未命中，于是同样的内容存第二份。
/// ★ 那是**浪费不是错误**，而换来的是这个函数只看请求、因此不可能与存的时候分家。
pub fn preferred_encoding(accept_encoding: Option<&str>, enabled: &[String]) -> &'static str {
    let Some(raw) = accept_encoding else {
        return IDENTITY;
    };
    for token in raw.split(',') {
        // ⚠ 只取分号前那一段：`gzip;q=0.8` 的算法名是 `gzip`。
        //   ★ 而 `q` 值本身**有意不看** —— 上游的解析器也不看（源码里写着 TODO），
        //   替它「修好」会让我们算的与它压的分家，见模块顶部第 1 条。
        let name = token.split(';').next().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        // `*` ⇒ 上游判 `Noop`（不压），我们跟着算 identity。
        if name == "*" {
            return IDENTITY;
        }
        let lower = name.to_ascii_lowercase();
        if !KNOWN.contains(&lower.as_str()) {
            // 不认识的编码：上游会跳过它继续往后看，我们也跳过。
            continue;
        }
        // ★ ★ 认得但**没被配置打开** ⇒ 上游会选中它、发现 level 0、于是整个不压
        //   （模块顶部第 2 条）。⇒ 这里必须跟着返回 identity，而不是继续找下一个 ——
        //   ⚠ 「继续找下一个」正是那个会与上游分家的模型。
        if !enabled.iter().any(|e| e == &lower) {
            return IDENTITY;
        }
        return match lower.as_str() {
            "gzip" => "gzip",
            "zstd" => "zstd",
            "br" => "br",
            _ => IDENTITY,
        };
    }
    IDENTITY
}

/// 这条链要不要压。★ 与 `Routed.cache` 同一条路子：中间件只记一笔，数据面决定怎么裹。
pub fn wanted<'r>(routed: &'r Routed<'_>) -> &'r [String] {
    &routed.requested_encodings
}

/// 这个状态码**按定义就没有体**（RFC 9110 §6.4.1 / §15.4.5）。
///
/// ⚠ ⚠ 它必须传给 `header_filter` 的 `no_body`，否则压缩层会给一个
/// **没有体**的响应加上 `Transfer-Encoding: chunked` 并抹掉 `Content-Length` ——
/// ★ 而 304 恰恰是缓存重验证路径上最常见的那个状态码。
/// ⚠ 上游那份只自己挡了 1xx，204/304 得由调用方挡。
pub fn status_has_no_body(status: u16) -> bool {
    status == 204 || status == 304 || (100..200).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<String> {
        KNOWN.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn 没有_accept_encoding_就是_identity() {
        assert_eq!(preferred_encoding(None, &all()), IDENTITY);
        assert_eq!(preferred_encoding(Some(""), &all()), IDENTITY);
    }

    // ★ ★ ★ 归一化的**全部意义**：多种写法必须收敛到同一个键。
    //   ⚠ 少了它，同一条 URL 会按 `Accept-Encoding` 的原值存出几十份内容相同的条目，
    //   而现场表现是「命中率莫名其妙地低、磁盘莫名其妙地满」。
    #[test]
    fn 多种写法收敛到同一个键() {
        let e = all();
        for raw in [
            "gzip",
            "gzip, deflate",
            "gzip;q=1.0, identity;q=0.5",
            " GZIP , br ",
            "deflate, gzip, br", // deflate 不认识 ⇒ 跳过，首个认得的是 gzip
        ] {
            assert_eq!(preferred_encoding(Some(raw), &e), "gzip", "raw = {raw:?}");
        }
    }

    // ★ 首选是谁就是谁 —— 顺序**是**语义的一部分。
    #[test]
    fn 按客户端的首选算() {
        let e = all();
        assert_eq!(preferred_encoding(Some("br, gzip"), &e), "br");
        assert_eq!(preferred_encoding(Some("gzip, br"), &e), "gzip");
        assert_eq!(preferred_encoding(Some("zstd, br, gzip"), &e), "zstd");
    }

    // ★ ★ 认得但没开 ⇒ identity，**不继续往后找**。
    //   ⚠ 这一条是与上游对齐的关键：上游选中首选之后发现它被禁用就整个不压，
    //   而一个「继续找下一个」的实现会算出 gzip、上游却发 identity ⇒ 两边分家。
    #[test]
    fn 认得但没开就是_identity_而不是往后找() {
        let only_gzip = vec!["gzip".to_string()];
        assert_eq!(preferred_encoding(Some("br, gzip"), &only_gzip), IDENTITY);
        // 反向：首选就是开着的那个 ⇒ 正常返回。
        assert_eq!(preferred_encoding(Some("gzip, br"), &only_gzip), "gzip");
    }

    // `*` ⇒ 上游判 Noop，我们跟着算 identity。
    #[test]
    fn 星号是_identity() {
        assert_eq!(preferred_encoding(Some("*"), &all()), IDENTITY);
        assert_eq!(preferred_encoding(Some("*, gzip"), &all()), IDENTITY);
    }

    // 一种都不认识 ⇒ identity。
    #[test]
    fn 全是不认识的编码就是_identity() {
        assert_eq!(
            preferred_encoding(Some("deflate, compress"), &all()),
            IDENTITY
        );
    }

    // ⚠ 空配置（没写 `encode`）⇒ 永远 identity。★ 它保证「没配压缩的站点」
    //   的缓存次级键与批 G 时代**一模一样**，换句话说：本批不动没配 encode 的人。
    #[test]
    fn 没配_encode_时永远是_identity() {
        assert_eq!(preferred_encoding(Some("gzip, br"), &[]), IDENTITY);
    }

    // ★ 这条钉的是「配置层认的那三个字」与「运行时认的那三个字」不许分家。
    //   ⚠ 分家的表现是「配置里写得下、运行时不认」，而那不会有任何报错。
    #[test]
    fn 认得的编码与配置层那张表逐字相同() {
        assert_eq!(KNOWN, ["gzip", "zstd", "br"]);
    }
}
