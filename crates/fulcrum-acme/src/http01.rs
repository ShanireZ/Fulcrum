//! HTTP-01 的应答表（G54 的「备」那一条）。
//!
//! ACME 的 HTTP-01 要求服务器在
//! `http://<域名>/.well-known/acme-challenge/<token>` 上回一段 key authorization
//! （RFC 8555 §8.3）。这个表就是「token → 该回什么」，由签发流程填、由数据面读。
//!
//! # ★ ★ 为什么它必须绕过路由
//!
//! 一份配置完全可以是 `respond 403`（或者 `handle` 里没有任何分支接得住这条路径）。
//! 如果挑战应答走正常路由，**用户的配置会把自己的证书签发挡掉**，
//! 而现场看到的只是「CA 说验不过」——配置里没有任何一行看得出问题。
//! 所以数据面在路由**之前**先问这张表，问不到才继续正常流程。
//!
//! ⚠ 绕过路由意味着这条路径**不受配置控制**，所以面收到最小：
//! 只认那一个前缀且 token 当前有效 · 表空时什么都不接（请求照常落回路由）·
//! 只做等值查找，不做任何模式匹配。
//!
//! ★ **token 用完就删，删由 `Drop` 管**（[`Provisioned`] 是守卫）：
//! 手写一对 insert/remove 迟早会漏一次，而漏掉的后果是一个早已失效的 token
//! 永远留在表上 —— 一条免费的信息泄露面。

use log::debug;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// 挑战路径的前缀。RFC 8555 §8.3 写死的，不可配置。
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

    /// 挂一个 token 上去，拿到一个到期自动摘掉的守卫。
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

    /// 数据面调用：这条路径该不该由我们直接应答？
    ///
    /// 返回 `None` 表示「不是挑战请求，或这个 token 不认识」——两种情况数据面的处置相同：
    /// 照常走路由。★ 不把「不认识的 token」变成 404：那会让这条路径的存在**可被探测**，
    /// 而它本该与任何别的路径没有区别。
    pub fn answer(&self, path: &str) -> Option<String> {
        let token = path.strip_prefix(WELL_KNOWN_PREFIX)?;
        // ★ 只认最后一段：`/.well-known/acme-challenge/a/b` 不是一个 token。
        if token.is_empty() || token.contains('/') {
            return None;
        }
        self.tokens.read().ok()?.get(token).cloned()
    }

    /// 当前挂着几个 token。给日志与测试用。
    pub fn len(&self) -> usize {
        self.tokens.read().map(|t| t.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 一个挂在表上的 token。析构时摘掉。
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
        // ★ 判据是「守卫掉了就没了」，而不是「记得调用 remove」。
        assert!(s.is_empty());
        assert!(s.answer("/.well-known/acme-challenge/tok123").is_none());
    }

    #[test]
    fn 不认识的路径与不认识的_token_一律返回_none() {
        let s = Arc::new(Http01Store::new());
        let _g = s.provision("tok", "value");
        // 不是挑战路径
        assert!(s.answer("/").is_none());
        assert!(s.answer("/.well-known/").is_none());
        assert!(s.answer("/.well-known/acme-challenge").is_none());
        // 空 token
        assert!(s.answer("/.well-known/acme-challenge/").is_none());
        // 不认识的 token
        assert!(s.answer("/.well-known/acme-challenge/other").is_none());
        // ⚠ 多一段就不是 token 了 —— 否则 `…/tok/../../x` 这类东西会进到查表里
        assert!(s.answer("/.well-known/acme-challenge/tok/more").is_none());
        // 前缀必须完整匹配，不能只是包含
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
        // b 走了，a 还在
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
        // ★ 这条是「没开自动签发时数据面行为不变」的判据。
        let s = Http01Store::new();
        assert!(s.answer("/.well-known/acme-challenge/anything").is_none());
    }
}
