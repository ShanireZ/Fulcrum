//! 請求視圖：路由決策需要知道的全部東西，**和 Pingora 無關**。
//!
//! ★ ★ 這一層刻意不引用 `pingora-core`。理由不是潔癖：
//! 路由語義（哪個站台、哪條匹配器、跑到第幾步、終結成什麼）是本專案**最需要被測**的邏輯，
//! 而它一旦長在 `HttpServerApp` 裡，測一條路由就要起監聽、發真流量、解析回應。
//! 那樣的測試貴、慢、而且**紅了指不到具體哪一條規則**。
//!
//! 代價是資料面那一側要做一次轉換（`&RequestHeader` → `RequestCtx`），
//! 而它是零拷貝的：全部欄位都是借用。

use std::net::IpAddr;

/// 請求頭查詢。資料面給一個 Pingora 的實作，測試給 [`HeaderList`]。
pub trait Headers {
    /// 取一個頭的值。★ 名字**大小寫不敏感**（HTTP 的規定，不是可選項）。
    fn get(&self, name: &str) -> Option<&str>;
}

/// 一組 `(名字, 值)`。
///
/// ★ 為什麼要包一層而不直接 `impl Headers for [(&str, &str)]`：
/// `&[T]` 本身已經是**胖指針**，沒法再變成 `&dyn Headers`（那也是胖指針，
/// 但要求資料指針是細的）。包一個 sized 的殼是唯一的走法。
/// ⚠ 順帶避開另一個坑：切片自帶一個 `get`，與 trait 裡的同名方法**在切片上會贏**，
/// 於是 `h.get("x")` 會靜靜地變成按下標取元素。
pub struct HeaderList<'a>(pub &'a [(&'a str, &'a str)]);

impl Headers for HeaderList<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }
}

/// 路由決策要用到的請求側事實。
#[derive(Clone, Copy)]
pub struct RequestCtx<'a> {
    /// Host（**不含連接埠**）。空串表示請求沒帶 Host。
    pub host: &'a str,
    /// 請求打到的**本機**連接埠——站台索引按它來分，不是按 Host 裡寫的那個。
    pub port: u16,
    pub scheme: &'a str,
    pub method: &'a str,
    /// 路徑，**不含查詢串**。
    pub path: &'a str,
    /// 查詢串，**不含 `?`**。
    pub query: &'a str,
    /// ⚠ **`+ Sync` 不是裝飾**：資料面在 `await` 之間拿著這個 ctx，
    /// 而 Pingora 的 `HttpServerApp` 要求 future 是 `Send`。
    /// 少了這條界，報出來的是一句「future cannot be sent between threads safely」，
    /// **指向 trait 定義而不是這一行**——查起來很繞。
    pub headers: &'a (dyn Headers + Sync),
    /// ★ ★ **用戶端 IP 的取值口徑見安全基線：不能取 XFF 最左項**（用戶端可偽造）。
    /// 本層只負責用它，取值是資料面的責任——所以這裡是一個已經定好的值，
    /// 而不是一個「從頭裡現算」的函式。
    pub remote_ip: Option<IpAddr>,
    pub remote_port: u16,
}

impl RequestCtx<'_> {
    /// `{uri}`：路徑 + 查詢串。
    pub fn uri(&self) -> String {
        if self.query.is_empty() {
            self.path.to_string()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }
}

/// 回應側的事實。只有到了寫回應那一刻才知道，所以它是**後來才有**的。
///
/// ★ 這就是為什麼 `header` 那些取值在路由階段**不展開**、只帶著範本走：
/// `{status}` 與 `{upstream}` 在路由階段還不存在，提前展開只能得到空串——
/// 而空串正是 G61 點名要避免的那種無聲失敗。
#[derive(Default)]
pub struct ResponseCtx<'a> {
    pub status: Option<u16>,
    /// 本次選中的上游位址。
    pub upstream: Option<&'a str>,
    pub headers: Option<&'a (dyn Headers + Sync)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 头名大小写不敏感() {
        let h = HeaderList(&[("Content-Type", "text/html")]);
        assert_eq!(h.get("content-type"), Some("text/html"));
        assert_eq!(h.get("CONTENT-TYPE"), Some("text/html"));
        assert_eq!(h.get("x-nope"), None);
    }

    #[test]
    fn uri_拼装() {
        let h = HeaderList(&[]);
        let ctx = RequestCtx {
            host: "a.com",
            port: 443,
            scheme: "https",
            method: "GET",
            path: "/x",
            query: "",
            headers: &h,
            remote_ip: None,
            remote_port: 0,
        };
        assert_eq!(ctx.uri(), "/x");
        let ctx2 = RequestCtx {
            query: "a=1",
            ..ctx
        };
        assert_eq!(ctx2.uri(), "/x?a=1");
    }
}
