//! 回應壓縮（**M2 批 I**，`encode` 指令串接；G99–G102）。
//!
//! ## ★ 用的是 fork 裡那份，不是自研（**G100**）
//!
//! `brotli` / `flate2` / `zstd` 本來就是 `pingora-core` 的**非可選**相依，
//! 壓縮那 1365 行也未被 feature 門控 —— **它們今天就已經在二進位裡** ⇒ 串接**新增 0 個包**。
//! ⚠ 與 G82「快取層完全自研」不矛盾，因為成本結構是反的：那次自研是為了不把
//! 8707 行 + 7 個新包吃進 fork，而這一份已經在裡面了。
//!
//! ★ 它已經做對的幾件容易漏的事（讀的是原始碼）：按 RFC 9110 補 `Vary: Accept-Encoding`、
//! 把強 ETag 弱化成 `W/"…"`、`101` 不轉換、無體不轉換、`Content-Type` 含 `zip` 的不壓、
//! 體小於 20 位元組不壓。
//!
//! ## ⚠ ⚠ 兩條**上游的**已知侷限，寫在明處
//!
//! 1. **`Accept-Encoding` 的 `q` 值被忽略**（上游原始碼裡就寫著 `// TODO: support q value`）。
//!    ⇒ `Accept-Encoding: gzip;q=0, br` 會被當成「首選 gzip」，而用戶端其實**拒絕**了 gzip。
//!    ★ 這一條本模組**沒有替它修**：修了會讓「我們算的」與「它壓的」分家，
//!    而分家的表現比這條侷限本身更壞（見下面 [`preferred_encoding`] 那段）。
//! 2. **只按用戶端的第一順位挑，不回落到我們的次選**。配的是 `encode gzip`
//!    而用戶端發 `br, gzip` 時，它選 br、發現 br 被禁用（level 0），於是**整個不壓**。
//!    ⇒ 少壓一次，不會壓錯 —— 代價方向是安全的。

use fulcrum_runtime::Routed;
use pingora_core::protocols::http::compression::{Algorithm, ResponseCompressionCtx};
use pingora_http::{RequestHeader, ResponseHeader};

/// 每種演算法各自的預設壓縮級別。
///
/// ★ ★ **有意不是「一個數配三種」**，而上游的 `ResponseCompressionCtx::new` 恰恰是那樣：
/// 它把同一個 `compression_level` 塞給三種演算法。⚠ 那個數在三種演算法上**根本不可比** ——
/// brotli 的 11 與 gzip 的 11（其實 gzip 只到 9）不是同一件事，
/// 而 brotli 高品質檔在動態內容上慢到不能用。
/// ⇒ 這裡逐個 `adjust_algorithm_level` 覆蓋成各自領域裡的常用值。
const GZIP_LEVEL: u32 = 6;
const ZSTD_LEVEL: u32 = 3;
const BROTLI_LEVEL: u32 = 5;

/// 我們認得的三種編碼。★ 與設定層 `ArgType::Enum(&["gzip", "zstd", "br"])` 逐字對應 ——
/// ⚠ 兩處分家的表現是「設定裡寫得下、執行環境不認」，而那不會有任何回報錯誤。
pub const KNOWN: &[&str] = &["gzip", "zstd", "br"];

/// 沒壓（或壓不了）時，快取次級鍵上用的那個值。
pub const IDENTITY: &str = "identity";

/// 一次回應的壓縮上下文。
pub struct Encoder(ResponseCompressionCtx);

impl Encoder {
    /// 按這條鏈上 `encode` 要求的演算法建一個。鏈上沒寫 `encode` ⇒ `None`。
    ///
    /// ⚠ `decompress_enable = false`：我們不替上游解壓。一個已經壓好的上游回應，
    /// 用戶端認就原樣轉、不認就……上游的事。★ 打開它等於**默默改寫別人的位元組**，
    /// 而那是一條沒人要求過的行為。
    /// ⚠ `preserve_etag = false`（**G102**）：壓完把強 ETag 弱化成 `W/"…"`。
    pub fn new(requested: &[String], req: &RequestHeader) -> Option<Encoder> {
        if requested.is_empty() {
            return None;
        }
        // ★ 先全關（level 0 = 禁用），再把設定點名的那幾種打開 ——
        //   ⚠ 反過來寫（先全開再關掉沒點名的）在加第四種演算法時會**預設放行**它，
        //   而「設定裡沒寫卻生效了」是本儲存庫反覆點名的那一類。
        let mut ctx = ResponseCompressionCtx::new(0, false, false);
        for name in requested {
            let (algo, level) = match name.as_str() {
                "gzip" => (Algorithm::Gzip, GZIP_LEVEL),
                "zstd" => (Algorithm::Zstd, ZSTD_LEVEL),
                "br" => (Algorithm::Brotli, BROTLI_LEVEL),
                // ⚠ 走不到：設定層已經用 Enum 攔過。留著不 panic —— 一個
                //   「設定裡多了一種編碼」的將來，不該讓資料面倒下。
                _ => continue,
            };
            ctx.adjust_algorithm_level(algo, level);
        }
        ctx.request_filter(req);
        Some(Encoder(ctx))
    }

    /// 回應頭進來一次（會補 `Vary`、弱化 ETag、去掉 `Content-Length` 與 `Accept-Ranges`）。
    pub fn header_filter(&mut self, resp: &mut ResponseHeader, no_body: bool) {
        self.0.response_header_filter(resp, no_body);
    }

    /// 體的每一塊進來一次。回傳 `None` = 這一塊不用替換（沒在壓）。
    pub fn body_filter(&mut self, data: Option<&bytes::Bytes>, end: bool) -> Option<bytes::Bytes> {
        self.0.response_body_filter(data, end)
    }
}

/// **這次請求最終會拿到哪種編碼** —— 快取次級鍵就用它（**G101**）。
///
/// # ★ ★ ★ 為什麼快取的次級鍵不能直接用 `Accept-Encoding` 的原值
///
/// G101 拍的是「壓完再存」，於是快取裡存的是**壓縮後**的位元組，
/// 而 pingora 會照 RFC 9110 給這種回應補上 `Vary: Accept-Encoding` ——
/// ⇒ 快取按這個頭分身。⚠ 而瀏覽器發的是 `gzip, deflate, br, zstd` 的**各種順序與寫法**：
/// 拿原值當次級鍵，同一條 URL 會存出幾十份**內容完全相同**的條目，
/// 而現場表現是「快取命中率莫名其妙地低、磁碟莫名其妙地滿」。
///
/// ⇒ 歸一化成「我們實際會發的那一種」，取值只剩 `gzip` / `zstd` / `br` / `identity` 四個。
/// ★ nginx 與 Varnish 都是這麼幹的。
///
/// # ⚠ ⚠ 正確性建在哪一條上（而**不是**建在「它與上游的挑法一致」上）
///
/// 這個函式**不試圖預測** `decide_action` 會怎麼選 —— 那是一個會分家的模型，
/// 而分家的模型比沒有模型更危險。它要的只有一條：
///
/// > **存與查用的是同一個函式，且它只看請求。**
///
/// 於是「映射到同一個次級鍵的兩個請求」必然有同一個首選演算法，
/// 也就必然都接受存進去的那份編碼。★ 而條目自己帶著 `Content-Encoding` 頭，
/// 所以發出去的東西永遠是自描述的。
///
/// ⚠ 一處**已知的、可接受的**浪費：上游回應不可壓時（圖片、`Content-Encoding` 已存在、
/// 體太小），我們照樣把 identity 的位元組存在 `gzip` 這個次級鍵下。
/// ⇒ 一個只發 `identity` 的用戶端會算出 `identity` 而未命中，於是同樣的內容存第二份。
/// ★ 那是**浪費不是錯誤**，而換來的是這個函式只看請求、因此不可能與存的時候分家。
pub fn preferred_encoding(accept_encoding: Option<&str>, enabled: &[String]) -> &'static str {
    let Some(raw) = accept_encoding else {
        return IDENTITY;
    };
    for token in raw.split(',') {
        // ⚠ 只取分號前那一段：`gzip;q=0.8` 的演算法名是 `gzip`。
        //   ★ 而 `q` 值本身**有意不看** —— 上游的解析器也不看（原始碼裡寫著 TODO），
        //   替它「修好」會讓我們算的與它壓的分家，見模組頂部第 1 條。
        let name = token.split(';').next().unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        // `*` ⇒ 上游判 `Noop`（不壓），我們跟著算 identity。
        if name == "*" {
            return IDENTITY;
        }
        let lower = name.to_ascii_lowercase();
        if !KNOWN.contains(&lower.as_str()) {
            // 不認識的編碼：上游會跳過它繼續往後看，我們也跳過。
            continue;
        }
        // ★ ★ 認得但**沒被設定打開** ⇒ 上游會選中它、發現 level 0、於是整個不壓
        //   （模組頂部第 2 條）。⇒ 這裡必須跟著回傳 identity，而不是繼續找下一個 ——
        //   ⚠ 「繼續找下一個」正是那個會與上游分家的模型。
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

/// 這條鏈要不要壓。★ 與 `Routed.cache` 同一條路子：中介軟體只記一筆，資料面決定怎麼裹。
pub fn wanted<'r>(routed: &'r Routed<'_>) -> &'r [String] {
    &routed.requested_encodings
}

/// 這個狀態碼**按定義就沒有體**（RFC 9110 §6.4.1 / §15.4.5）。
///
/// ⚠ ⚠ 它必須傳給 `header_filter` 的 `no_body`，否則壓縮層會給一個
/// **沒有體**的回應加上 `Transfer-Encoding: chunked` 並抹掉 `Content-Length` ——
/// ★ 而 304 恰恰是快取重驗證路徑上最常見的那個狀態碼。
/// ⚠ 上游那份只自己擋了 1xx，204/304 得由呼叫方擋。
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

    // ★ ★ ★ 歸一化的**全部意義**：多種寫法必須收斂到同一個鍵。
    //   ⚠ 少了它，同一條 URL 會按 `Accept-Encoding` 的原值存出幾十份內容相同的條目，
    //   而現場表現是「命中率莫名其妙地低、磁碟莫名其妙地滿」。
    #[test]
    fn 多种写法收敛到同一个键() {
        let e = all();
        for raw in [
            "gzip",
            "gzip, deflate",
            "gzip;q=1.0, identity;q=0.5",
            " GZIP , br ",
            "deflate, gzip, br", // deflate 不認識 ⇒ 跳過，首個認得的是 gzip
        ] {
            assert_eq!(preferred_encoding(Some(raw), &e), "gzip", "raw = {raw:?}");
        }
    }

    // ★ 首選是誰就是誰 —— 順序**是**語義的一部分。
    #[test]
    fn 按客户端的首选算() {
        let e = all();
        assert_eq!(preferred_encoding(Some("br, gzip"), &e), "br");
        assert_eq!(preferred_encoding(Some("gzip, br"), &e), "gzip");
        assert_eq!(preferred_encoding(Some("zstd, br, gzip"), &e), "zstd");
    }

    // ★ ★ 認得但沒開 ⇒ identity，**不繼續往後找**。
    //   ⚠ 這一條是與上游對齊的關鍵：上游選中首選之後發現它被禁用就整個不壓，
    //   而一個「繼續找下一個」的實作會算出 gzip、上游卻發 identity ⇒ 兩邊分家。
    #[test]
    fn 认得但没开就是_identity_而不是往后找() {
        let only_gzip = vec!["gzip".to_string()];
        assert_eq!(preferred_encoding(Some("br, gzip"), &only_gzip), IDENTITY);
        // 反向：首選就是開著的那個 ⇒ 正常回傳。
        assert_eq!(preferred_encoding(Some("gzip, br"), &only_gzip), "gzip");
    }

    // `*` ⇒ 上游判 Noop，我們跟著算 identity。
    #[test]
    fn 星号是_identity() {
        assert_eq!(preferred_encoding(Some("*"), &all()), IDENTITY);
        assert_eq!(preferred_encoding(Some("*, gzip"), &all()), IDENTITY);
    }

    // 一種都不認識 ⇒ identity。
    #[test]
    fn 全是不认识的编码就是_identity() {
        assert_eq!(
            preferred_encoding(Some("deflate, compress"), &all()),
            IDENTITY
        );
    }

    // ⚠ 空設定（沒寫 `encode`）⇒ 永遠 identity。★ 它保證「沒配壓縮的站台」
    //   的快取次級鍵與批 G 時代**一模一樣**，換句話說：本批不動沒配 encode 的人。
    #[test]
    fn 没配_encode_时永远是_identity() {
        assert_eq!(preferred_encoding(Some("gzip, br"), &[]), IDENTITY);
    }

    // ★ 這條釘的是「設定層認的那三個字」與「執行環境認的那三個字」不許分家。
    //   ⚠ 分家的表現是「設定裡寫得下、執行環境不認」，而那不會有任何回報錯誤。
    #[test]
    fn 认得的编码与配置层那张表逐字相同() {
        assert_eq!(KNOWN, ["gzip", "zstd", "br"]);
    }
}
