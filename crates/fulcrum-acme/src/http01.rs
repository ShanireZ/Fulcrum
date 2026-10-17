//! HTTP-01 的應答表（G54 的「備」那一條）。
//!
//! ACME 的 HTTP-01 要求伺服器在
//! `http://<域名>/.well-known/acme-challenge/<token>` 上回一段 key authorization
//! （RFC 8555 §8.3）。這個表就是「token → 該回什麼」，由簽發流程填、由資料面讀。
//!
//! # ★ ★ 為什麼它必須繞過路由
//!
//! 一份設定完全可以是 `respond 403`（或者 `handle` 裡沒有任何分支接得住這條路徑）。
//! 如果挑戰應答走正常路由，**使用者的設定會把自己的證書簽發擋掉**，
//! 而現場看到的只是「CA 說驗不過」——設定裡沒有任何一行看得出問題。
//! 所以資料面在路由**之前**先問這張表，問不到才繼續正常流程。
//!
//! ⚠ 繞過路由意味著這條路徑**不受設定控制**，所以面收到最小：
//! 只認那一個前綴且 token 當前有效 · 表空時什麼都不接（請求照常落迴路由）·
//! 只做等值查找，不做任何模式匹配。
//!
//! ★ **token 用完就刪，刪由 `Drop` 管**（[`Provisioned`] 是守衛）：
//! 手寫一對 insert/remove 遲早會漏一次，而漏掉的後果是一個早已失效的 token
//! 永遠留在表上 —— 一條免費的資訊洩露面。

use log::debug;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// 挑戰路徑的前綴。RFC 8555 §8.3 寫死的，不可設定。
pub const WELL_KNOWN_PREFIX: &str = "/.well-known/acme-challenge/";

/// token → key authorization。
#[derive(Default, Debug)]
pub struct Http01Store {
    tokens: RwLock<BTreeMap<String, String>>,
}

impl Http01Store {
    pub fn new() -> Http01Store {
        Http01Store::default()
    }

    /// 掛一個 token 上去，拿到一個到期自動摘掉的守衛。
    pub fn provision(self: &Arc<Self>, token: &str, key_authorization: &str) -> Provisioned {
        if let Ok(mut t) = self.tokens.write() {
            t.insert(token.to_string(), key_authorization.to_string());
            debug!("HTTP-01 挂上 token {token}");
        }
        Provisioned {
            store: self.clone(),
            token: token.to_string(),
        }
    }

    /// 資料面呼叫：這條路徑該不該由我們直接應答？
    ///
    /// 回傳 `None` 表示「不是挑戰請求，或這個 token 不認識」——兩種情況資料面的處置相同：
    /// 照常走路由。★ 不把「不認識的 token」變成 404：那會讓這條路徑的存在**可被探測**，
    /// 而它本該與任何別的路徑沒有區別。
    pub fn answer(&self, path: &str) -> Option<String> {
        let token = path.strip_prefix(WELL_KNOWN_PREFIX)?;
        // ★ 只認最後一段：`/.well-known/acme-challenge/a/b` 不是一個 token。
        if token.is_empty() || token.contains('/') {
            return None;
        }
        self.tokens.read().ok()?.get(token).cloned()
    }

    /// 當前掛著幾個 token。給紀錄與測試用。
    pub fn len(&self) -> usize {
        self.tokens.read().map(|t| t.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 一個掛在表上的 token。析構時摘掉。
pub struct Provisioned {
    store: Arc<Http01Store>,
    token: String,
}

impl Drop for Provisioned {
    fn drop(&mut self) {
        if let Ok(mut t) = self.store.tokens.write() {
            t.remove(&self.token);
            debug!("HTTP-01 摘掉 token {}", self.token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 挂上能查到_析构就查不到() {
        let s = Arc::new(Http01Store::new());
        assert!(s.is_empty());
        {
            let _g = s.provision("tok123", "tok123.thumbprint");
            assert_eq!(s.len(), 1);
            assert_eq!(
                s.answer("/.well-known/acme-challenge/tok123").as_deref(),
                Some("tok123.thumbprint")
            );
        }
        // ★ 判據是「守衛掉了就沒了」，而不是「記得呼叫 remove」。
        assert!(s.is_empty());
        assert!(s.answer("/.well-known/acme-challenge/tok123").is_none());
    }

    #[test]
    fn 不认识的路径与不认识的_token_一律返回_none() {
        let s = Arc::new(Http01Store::new());
        let _g = s.provision("tok", "value");
        // 不是挑戰路徑
        assert!(s.answer("/").is_none());
        assert!(s.answer("/.well-known/").is_none());
        assert!(s.answer("/.well-known/acme-challenge").is_none());
        // 空 token
        assert!(s.answer("/.well-known/acme-challenge/").is_none());
        // 不認識的 token
        assert!(s.answer("/.well-known/acme-challenge/other").is_none());
        // ⚠ 多一段就不是 token 了 —— 否則 `…/tok/../../x` 這類東西會進到查表裡
        assert!(s.answer("/.well-known/acme-challenge/tok/more").is_none());
        // 前綴必須完整匹配，不能只是包含
        assert!(s.answer("/x/.well-known/acme-challenge/tok").is_none());
    }

    #[test]
    fn 多个_token_互不影响() {
        let s = Arc::new(Http01Store::new());
        let g1 = s.provision("a", "A");
        {
            let _g2 = s.provision("b", "B");
            assert_eq!(s.len(), 2);
        }
        // b 走了，a 還在
        assert_eq!(s.len(), 1);
        assert_eq!(
            s.answer("/.well-known/acme-challenge/a").as_deref(),
            Some("A")
        );
        assert!(s.answer("/.well-known/acme-challenge/b").is_none());
        drop(g1);
        assert!(s.is_empty());
    }

    #[test]
    fn 空表什么都不接() {
        // ★ 這條是「沒開自動簽發時資料面行為不變」的判據。
        let s = Http01Store::new();
        assert!(s.answer("/.well-known/acme-challenge/anything").is_none());
    }
}
