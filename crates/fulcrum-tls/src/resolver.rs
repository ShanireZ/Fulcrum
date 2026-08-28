//! 按 SNI 挑证书 —— **BoringSSL 的 `select_certificate_callback`**（G104）。
//!
//! §5.1 第 1 条原本要求实现 rustls 的 `ResolvesServerCert`；**G104 推翻了它**。现在走的是
//! `SslContextBuilder::set_select_certificate_callback(|ClientHello| …)` ——
//! 它包的是 BoringSSL 的 `SSL_early_callback_ctx`，**在几乎所有 ClientHello 处理之前**被调用。
//!
//! # ★ ★ 换过来之后**变简单**的两件事，与**变麻烦**的一件
//!
//! **变简单 ①：不再需要 fork 的 `TlsSettings::with_cert_resolver`。**
//! boringssl 那侧的 `TlsSettings` 对 `SslAcceptorBuilder` 实现了 `Deref`/`DerefMut`，
//! 这个 setter 本来就够得到（FORK.md 改动 8 因此可以删掉）。
//!
//! **变简单 ②：`resolve()` 那条「必须同步」的约束没有了。**
//! rustls 在握手线程上同步调用 `resolve()`，里面不能 `await` ——
//! 而这正是 `UNWIRED` 里 `on_demand` 登记「需要一座桥」的原因。
//! BoringSSL 这侧有 `set_async_select_certificate_callback`（原生异步），
//! 而且 `pingora-boringssl` 自带 `ext::suspend_when_need_ssl_cert` / `unblock_ssl_cert`。
//! ⏳ 本批不做 On-Demand，但**那条阻塞理由已经到期，别再照抄它**。
//!
//! **变麻烦的一件：ALPN 要自己从扩展里看。**
//! ⚠ ⚠ 早回调发生在 **ALPN 协商之前**，所以拿不到「协商结果」，只能看**客户端提供了什么**。
//! rustls 那侧 `ClientHello::alpn()` 直接给一个迭代器；这边给的是扩展的原始字节，
//! 要按 RFC 7301 的框架走一遍（见 [`alpn_list`]，[`offers_alpn`] 建在它上面）。
//! ★ 这**不是**「手写 ClientHello 解析」——BoringSSL 已经把扩展切出来了，
//! 这里读的是一段长度前缀列表，而且它有自己的判据（下面那组单测，命中与错过都覆盖）。
//!
//! ★ ★ **（G104 第 ② 处）：这份框架读法现在有第二个使用者** ——
//! L4 的 ClientHello 预读（`fulcrum-server` 的 `peek_client_hello`）要的是**整张清单**，
//! 这里要的是「有没有 `acme-tls/1`」。⇒ 翻出 [`alpn_list`] 让两边共用**一份**实现，
//! 而不是各写一份（D18/G66 同一条理由：**让分家在结构上做不到**）。

use log::{debug, warn};
use pingora_boringssl::error::ErrorStack;
use pingora_boringssl::ext::{ssl_add_chain_cert, ssl_use_certificate, ssl_use_private_key};
use pingora_boringssl::pkey::{PKey, Private};
use pingora_boringssl::ssl::{
    ClientHello, ExtensionType, NameType, SelectCertError, SslContextBuilder, SslRef,
};
use pingora_boringssl::x509::X509;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// 一张证书连同它的私钥。**leaf 在 `chain[0]`**（PEM 文件里的顺序）。
///
/// ★ 它就是 rustls 那边 `CertifiedKey` 的位置，而形状更朴素：
/// BoringSSL 不需要「签名密钥对象」这一层抽象，握手期直接把 `X509` 与 `PKey` 装到
/// `SSL` 上即可。
pub struct CertKey {
    pub chain: Vec<X509>,
    pub key: PKey<Private>,
    /// leaf 的 `notAfter`。
    ///
    /// ★ ★ **它由 [`crate::cert_key`] 从 `chain[0]` 自己读出来，不由调用方传进来。**
    /// 收一个参数的话，「这张证书说自己什么时候到期」与「我们以为它什么时候到期」
    /// 就成了两份，而不一致的那天没有任何东西会说 —— 表现是一条静静报错值的
    /// `fulcrum_cert_expiry_seconds`，它在告警规则里长得完全正常。
    /// ⇒ 让分家在结构上做不到（D18/G66 同一条理由）。
    pub not_after: SystemTime,
}

impl CertKey {
    /// 把这张证书装到一条正在握手的连接上。
    ///
    /// ★ ★ **顺序是有讲究的**：先 `SSL_use_certificate`（leaf），再逐张
    /// `SSL_add1_chain_cert`（中间证书）。反过来会把中间证书当成 leaf 发出去 ——
    /// 而那种连接在多数客户端上表现为「证书链不完整」，**不是**一个当场可见的错。
    pub fn install_on(&self, ssl: &mut SslRef) -> Result<(), ErrorStack> {
        let mut it = self.chain.iter();
        let Some(leaf) = it.next() else {
            // 空链在 `cert_key()` 就被挡掉了；这里只是不 panic。
            return Ok(());
        };
        ssl_use_certificate(ssl, leaf)?;
        ssl_use_private_key(ssl, &self.key)?;
        for inter in it {
            ssl_add_chain_cert(ssl, inter)?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for CertKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CertKey({} 张)", self.chain.len())
    }
}

/// 一份证书表。
#[derive(Default)]
struct Table {
    /// 精确域名 → 证书。键一律小写。
    exact: BTreeMap<String, Arc<CertKey>>,
    /// `(".example.com", 证书)`，**按后缀长度降序**。
    wildcard: Vec<(String, Arc<CertKey>)>,
    /// 客户端不带 SNI 时用哪一张（全局选项 `default_sni`）。
    default: Option<Arc<CertKey>>,
}

/// ACME 的 TLS-ALPN-01 用的那个 ALPN 协议名（RFC 8737 §3）。写死，不可配置。
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// 按 SNI 挑证书。可以在运行期热装新证书（ACME 签发完就装进来）。
#[derive(Default)]
pub struct SniResolver {
    table: RwLock<Table>,
    /// ★ TLS-ALPN-01 的挑战证书（G54 的「主」）：域名 → 那张一次性的自签证书。
    ///
    /// ⚠ ⚠ **它与 `table` 是两张互不相通的表，这是安全属性不是洁癖**：
    ///   · 客户端提供了 `acme-tls/1` 时**只**看这张表 —— 否则会把用户的真证书
    ///     交给一个只想验证域名归属的对端；
    ///   · 没提供 `acme-tls/1` 时**绝不**看这张表 —— 否则普通访客会拿到
    ///     一张自签的挑战证书，浏览器当场报证书错误，而服务端日志里是一次成功的握手。
    ///   两个方向各由一条断言钉着（`tests/acme/run.sh`）。
    challenge: RwLock<BTreeMap<String, Arc<CertKey>>>,
}

impl std::fmt::Debug for SniResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.table.read().map(|t| t.exact.len()).unwrap_or(0);
        write!(f, "SniResolver({n} 个精确域名)")
    }
}

impl SniResolver {
    pub fn new() -> SniResolver {
        SniResolver::default()
    }

    /// ★ ★ **把自己挂成这个 context 的证书选择回调。**
    ///
    /// 这是 G104 那条决策的落点：h1/h2 入口与（将来的）h3 入口**挂同一个回调**，
    /// 于是「两个入口各有一套挑证书实现」在结构上做不到 —— 那正是取「统一 BoringSSL」
    /// 而不是「两套并存」的全部理由（D18/G66 同源）。
    pub fn install_into(self: &Arc<Self>, builder: &mut SslContextBuilder) {
        let me = self.clone();
        builder.set_select_certificate_callback(move |mut hello| me.select(&mut hello));
    }

    /// 早回调的正体。
    ///
    /// ⚠ 返回 `Err(SelectCertError::ERROR)` = **拒绝这次握手**。
    /// ★ 那比「挑一张不匹配的证书」好：后者让客户端看到证书错误，
    /// 而运维在服务端只看到一次成功的握手。
    fn select(&self, hello: &mut ClientHello<'_>) -> Result<(), SelectCertError> {
        // ★ SNI 先抄成 owned：下面要 `ssl_mut()`（`&mut`），
        //   而 `servername()` 借的是 `hello` 本身。
        let sni: Option<String> = hello
            .servername(NameType::HOST_NAME)
            .map(|s| s.to_ascii_lowercase());

        // ── TLS-ALPN-01（RFC 8737）────────────────────────────────────────
        //
        // ★ ★ **提供了 `acme-tls/1` 的连接走一条完全独立的路**：只查挑战表，
        //   查不到就**拒绝握手**，绝不回落到真证书。
        //   ⚠ 回落会把用户的真证书交给一个只需要验证域名归属的对端；
        //   而且 CA 拿到一张没有 acmeIdentifier 扩展的证书本来就会判失败，
        //   所以回落连「让它能用」都换不到——纯粹是白送一次证书暴露。
        //
        // ⚠ ⚠ **这里判的是「客户端提供了」，不是「协商到了」** —— 早回调发生在
        //   ALPN 协商之前。两者的差别在这条路上不重要（真正的 CA 只提供这一个协议），
        //   ★ 但**措辞必须准确**：写成「协商到」会让下一个人以为这里能拿到协商结果。
        let offers_acme_tls = hello
            .get_extension(ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION)
            .is_some_and(|body| offers_alpn(body, ACME_TLS_ALPN));

        let picked = if offers_acme_tls {
            let got = self.lookup_challenge(sni.as_deref());
            if got.is_none() {
                debug!(
                    "客户端提供了 acme-tls/1 但 {} 没有挂着的挑战证书，拒绝握手",
                    sni.as_deref().unwrap_or("(无 SNI)")
                );
            }
            got
        } else {
            let got = self.lookup(sni.as_deref());
            match (&sni, &got) {
                (Some(n), None) => debug!("SNI {n} 没有可用证书，拒绝握手"),
                (None, None) => debug!("客户端没给 SNI，且没有配 default_sni，拒绝握手"),
                _ => {}
            }
            got
        };

        let Some(ck) = picked else {
            return Err(SelectCertError::ERROR);
        };
        ck.install_on(hello.ssl_mut()).map_err(|e| {
            // ⚠ 这一条与「挑不到」是两回事：证书挑到了却装不上，说明存的东西本身有问题
            //   （私钥与证书对不上之类）。它必须能与上面那几行区分开。
            warn!("证书挑到了却装不到连接上：{e}");
            SelectCertError::ERROR
        })
    }

    /// 把一张证书装到它覆盖的每个域名上。
    ///
    /// `domains` 通常来自证书自己的 SAN——**不是**配置里写的站点名。
    /// ★ 这一点很重要：一张买来的证书可能覆盖比配置里写的更多（或更少）的域名，
    /// 而握手期该用哪张，取决于**证书说它是谁的**。
    pub fn install(&self, domains: &[String], ck: Arc<CertKey>) {
        let mut t = match self.table.write() {
            Ok(t) => t,
            Err(e) => {
                // ⚠ 锁中毒说明某个持锁线程 panic 过。这里不 unwrap——
                //   一次装载失败不该把整个进程带走。
                warn!("证书表的锁中毒了，本次装载被跳过：{e}");
                return;
            }
        };
        for d in domains {
            let d = d.to_ascii_lowercase();
            if let Some(rest) = d.strip_prefix("*.") {
                let suffix = format!(".{rest}");
                t.wildcard.retain(|(s, _)| *s != suffix);
                t.wildcard.push((suffix, ck.clone()));
            } else {
                t.exact.insert(d, ck.clone());
            }
        }
        // ★ 长后缀优先：`*.a.example.com` 必须先于 `*.example.com` 被试到。
        t.wildcard
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    /// 客户端不带 SNI 时用哪一张。
    pub fn set_default(&self, ck: Arc<CertKey>) {
        if let Ok(mut t) = self.table.write() {
            t.default = Some(ck);
        }
    }

    /// 已装载的精确域名与通配后缀，给装载日志用。
    ///
    /// ★ 它从 [`Self::expiries`] 派生 —— 两处各拼一遍键的写法迟早分家，
    /// 而分家之后「装载日志里的那批域名」与「指标里的那批域名」看起来都言之凿凿。
    pub fn known(&self) -> Vec<String> {
        self.expiries().into_iter().map(|(d, _)| d).collect()
    }

    /// 每个已装载域名的证书 `notAfter`。**键的写法与 [`Self::known`] 是同一份。**
    ///
    /// ⛔ ⚠ **挑战表不在里面**：TLS-ALPN-01 那张一次性自签证书只活几秒、也不是站点证书，
    /// 混进来会让「快过期了」那类告警一直在叫，而叫的是一个根本不该被续期的东西。
    /// ★ 两张表本来就互不相通（见 `challenge` 字段），这里只是不去把它们合起来。
    pub fn expiries(&self) -> Vec<(String, SystemTime)> {
        let Ok(t) = self.table.read() else {
            return Vec::new();
        };
        let mut out: Vec<(String, SystemTime)> = t
            .exact
            .iter()
            .map(|(d, ck)| (d.clone(), ck.not_after))
            .collect();
        out.extend(
            t.wildcard
                .iter()
                .map(|(s, ck)| (format!("*{s}"), ck.not_after)),
        );
        out
    }

    pub fn is_empty(&self) -> bool {
        self.table
            .read()
            .map(|t| t.exact.is_empty() && t.wildcard.is_empty() && t.default.is_none())
            .unwrap_or(true)
    }

    /// 挂一张 TLS-ALPN-01 的挑战证书上去，拿到一个到期自动摘掉的守卫。
    ///
    /// ★ 形状照 `fulcrum_acme::Http01Store::provision`：**手写一对 insert/remove
    /// 迟早会漏一次**，而漏掉的后果是一张自签证书永远留在表上——
    /// 虽然它只在 `acme-tls/1` 那条路上才会被挑到，但那仍是一条不该留的面。
    pub fn provision_challenge(self: &Arc<Self>, domain: &str, ck: Arc<CertKey>) -> ChallengeCert {
        let domain = domain.to_ascii_lowercase();
        if let Ok(mut c) = self.challenge.write() {
            c.insert(domain.clone(), ck);
            debug!("TLS-ALPN-01 挂上 {domain} 的挑战证书");
        }
        ChallengeCert {
            resolver: self.clone(),
            domain,
        }
    }

    /// 现在挂着几张挑战证书。给日志与测试用。
    pub fn challenge_len(&self) -> usize {
        self.challenge.read().map(|c| c.len()).unwrap_or(0)
    }

    /// ★ 挑战证书**只做精确匹配**，不吃通配、不落 default。
    ///
    /// ⚠ 通配符域名的 TLS-ALPN-01 挑战，CA 连的是**被授权的那个名字本身**
    /// （`example.com`，不是 `*.example.com`），所以这里按 CA 会给的 SNI 存取即可。
    /// 让它吃通配或落 default，等于给「随便一个 SNI 都能换到一张挑战证书」开门。
    fn lookup_challenge(&self, sni: Option<&str>) -> Option<Arc<CertKey>> {
        let name = sni?.to_ascii_lowercase();
        self.challenge.read().ok()?.get(&name).cloned()
    }

    fn lookup(&self, sni: Option<&str>) -> Option<Arc<CertKey>> {
        let t = self.table.read().ok()?;
        let Some(name) = sni else {
            // 不带 SNI（老客户端、或直接用 IP 访问）→ default_sni。
            return t.default.clone();
        };
        let name = name.to_ascii_lowercase();
        if let Some(ck) = t.exact.get(&name) {
            return Some(ck.clone());
        }
        for (suffix, ck) in &t.wildcard {
            if wildcard_covers(suffix, &name) {
                return Some(ck.clone());
            }
        }
        t.default.clone()
    }
}

/// 客户端在 ALPN 扩展里提供的**全部**协议名，按原顺序。
///
/// 收的是 `ClientHello::get_extension(APPLICATION_LAYER_PROTOCOL_NEGOTIATION)` 的原始体，
/// 按 RFC 7301 §3.1 的框架：**2 字节列表总长，随后是若干「1 字节长度 + 名字」**。
///
/// ★ ★ **这不是「手写 ClientHello 解析」**：BoringSSL 已经把扩展切出来了，
/// 这里读的是一段自带长度的列表，边界全部显式检查，而且下面那组单测**命中与错过都覆盖**。
///
/// ⚠ ⚠ **`None` 与 `Some(vec![])` 是两件事，不许合并**：
/// 前者是「这段字节我们没读懂」，后者是「一份合法的空清单」。
/// 两个调用方对它们的处置**恰好相同**（都按「没提供」算），
/// ★ 但那是两条各自成立的判断，不是同一条 —— 把它们在这里压成一个 `bool`，
/// 下一个需要区分的调用方就再也拿不回来了。
///
/// ★ ★ ★ **（G104 第 ② 处）：本函数从 [`offers_alpn`] 里翻出来了。**
/// L4 的 ClientHello 预读换到 BoringSSL 之后要的是**整张清单**（分流规则按集合比），
/// 而这里要的是「有没有 `acme-tls/1`」。⇒ 两个问题共用**一份** RFC 7301 框架读法，
/// 而不是各写一份 —— 与 D18/G66 同一条理由：**让分家在结构上做不到**。
pub fn alpn_list(ext_body: &[u8]) -> Option<Vec<Vec<u8>>> {
    if ext_body.len() < 2 {
        return None;
    }
    let declared = u16::from_be_bytes([ext_body[0], ext_body[1]]) as usize;
    let list = &ext_body[2..];
    // ★ 声明长度与实际长度必须严丝合缝。宽容一个字节，就等于接受一份我们没读懂的输入。
    if declared != list.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < list.len() {
        let n = list[i] as usize;
        i += 1;
        // 零长名字不合法（RFC 7301：`ProtocolName<1..2^8-1>`）；越界同理。
        if n == 0 || i + n > list.len() {
            return None;
        }
        out.push(list[i..i + n].to_vec());
        i += n;
    }
    Some(out)
}

/// 客户端在 ALPN 扩展里**提供**了某个协议吗。
///
/// ⚠ 一切对不上的输入一律回 `false`（= 当作没提供），而不是「尽力猜」——
/// 猜错的方向是「把真证书发给一个 ACME 验证连接」，那是这一整块要防的事。
pub fn offers_alpn(ext_body: &[u8], want: &[u8]) -> bool {
    if want.is_empty() {
        return false;
    }
    alpn_list(ext_body).is_some_and(|l| l.iter().any(|p| p == want))
}

/// `*.example.com`（这里传的是 `.example.com`）覆盖谁——**语义在别处，这里只是转出去**。
///
/// ★ ★ （D18 / G66）之前这里自己有一份实现，而站点索引那边**另有一份、
/// 而且不一样**（`ends_with` 的后缀匹配）。后果不是抽象的：`a.b.example.com`
/// 被路由到通配站点、然后在这里拿不到证书 ⇒ 握手失败，而配置里看不出问题。
/// → 现在两边都调 [`fulcrum_config::host::wildcard_covers`]，**只有一份语义**。
pub use fulcrum_config::host::wildcard_covers;

/// 一张挂着的 TLS-ALPN-01 挑战证书。析构时自动摘掉。
pub struct ChallengeCert {
    resolver: Arc<SniResolver>,
    domain: String,
}

impl Drop for ChallengeCert {
    fn drop(&mut self) {
        if let Ok(mut c) = self.resolver.challenge.write() {
            c.remove(&self.domain);
            debug!("TLS-ALPN-01 摘掉 {} 的挑战证书", self.domain);
        }
    }
}

impl std::fmt::Debug for ChallengeCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChallengeCert({})", self.domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 通配符只吃一层() {
        // ★ 这一条是 RFC 6125 与浏览器的实际行为，不是我们的偏好。
        assert!(wildcard_covers(".example.com", "a.example.com"));
        assert!(wildcard_covers(".example.com", "www.example.com"));
        // 裸域不在覆盖范围内
        assert!(!wildcard_covers(".example.com", "example.com"));
        // 两层不在覆盖范围内
        assert!(!wildcard_covers(".example.com", "a.b.example.com"));
        // 别的域名当然不在
        assert!(!wildcard_covers(".example.com", "a.example.org"));
        // 空标签不算
        assert!(!wildcard_covers(".example.com", ".example.com"));
    }

    #[test]
    fn 空解析器什么都挑不出来() {
        let r = SniResolver::new();
        assert!(r.is_empty());
        assert!(r.lookup(Some("a.com")).is_none());
        assert!(r.lookup(None).is_none());
        assert!(r.known().is_empty());
    }

    /// 造一份合法的 ALPN 扩展体。
    fn alpn_ext(protos: &[&[u8]]) -> Vec<u8> {
        let mut list = Vec::new();
        for p in protos {
            list.push(p.len() as u8);
            list.extend_from_slice(p);
        }
        let mut out = (list.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&list);
        out
    }

    #[test]
    fn alpn_扫描器能命中() {
        assert!(offers_alpn(&alpn_ext(&[ACME_TLS_ALPN]), ACME_TLS_ALPN));
        assert!(offers_alpn(
            &alpn_ext(&[b"h2", b"http/1.1", ACME_TLS_ALPN]),
            ACME_TLS_ALPN
        ));
        // 在第一个也算
        assert!(offers_alpn(
            &alpn_ext(&[ACME_TLS_ALPN, b"h2"]),
            ACME_TLS_ALPN
        ));
    }

    #[test]
    fn alpn_扫描器也能错过() {
        // ★ 这一半同样重要：一个恒真的扫描器会把每一条普通连接都当成 ACME 验证连接，
        //   于是普通访客拿到自签的挑战证书。
        assert!(!offers_alpn(
            &alpn_ext(&[b"h2", b"http/1.1"]),
            ACME_TLS_ALPN
        ));
        assert!(!offers_alpn(&alpn_ext(&[]), ACME_TLS_ALPN));
        // 前缀不算命中（长度必须相等）
        assert!(!offers_alpn(&alpn_ext(&[b"acme-tls/"]), ACME_TLS_ALPN));
        assert!(!offers_alpn(&alpn_ext(&[b"acme-tls/11"]), ACME_TLS_ALPN));
    }

    #[test]
    fn alpn_扫描器对畸形输入一律回假() {
        // 太短
        assert!(!offers_alpn(&[], ACME_TLS_ALPN));
        assert!(!offers_alpn(&[0x00], ACME_TLS_ALPN));
        // 声明长度与实际不符（多一个字节）
        let mut bad = alpn_ext(&[ACME_TLS_ALPN]);
        bad.push(0x00);
        assert!(!offers_alpn(&bad, ACME_TLS_ALPN));
        // 声明长度与实际不符（少一个字节）
        let mut short = alpn_ext(&[ACME_TLS_ALPN]);
        short.pop();
        assert!(!offers_alpn(&short, ACME_TLS_ALPN));
        // 名字长度越界：声明 200 字节而后面只有几个
        let body = [0x00u8, 0x03, 200, b'a', b'b'];
        assert!(!offers_alpn(&body, ACME_TLS_ALPN));
        // 零长名字
        let body = [0x00u8, 0x01, 0x00];
        assert!(!offers_alpn(&body, ACME_TLS_ALPN));
        // 想找的东西是空的
        assert!(!offers_alpn(&alpn_ext(&[b"h2"]), b""));
    }

    #[test]
    fn alpn_清单读得出原顺序与原字节() {
        // ★ L4 的分流规则按**集合**比，但清单必须是原样的字节 ——
        //   `h2c` 不许被读成 `h2`，那正是 tests/l4/run.sh ⑤ 钉的那条。
        let got = alpn_list(&alpn_ext(&[b"h2", b"http/1.1", ACME_TLS_ALPN])).expect("该读得出");
        assert_eq!(
            got,
            vec![b"h2".to_vec(), b"http/1.1".to_vec(), ACME_TLS_ALPN.to_vec()]
        );
    }

    #[test]
    fn alpn_清单把没读懂与空清单分开() {
        // ⚠ ⚠ 这一条是本函数存在的全部理由：两者**恰好**都按「没提供」处置，
        //   而它们是两条各自成立的判断。合并成一个 bool 就再也拿不回来了。
        assert_eq!(alpn_list(&alpn_ext(&[])), Some(Vec::new()), "合法的空清单");
        assert_eq!(alpn_list(&[]), None, "两个字节都没有 ⇒ 没读懂");
        assert_eq!(alpn_list(&[0x00]), None, "只有一个字节 ⇒ 没读懂");
        // 声明长度与实际不符（两个方向）
        let mut long = alpn_ext(&[ACME_TLS_ALPN]);
        long.push(0x00);
        assert_eq!(alpn_list(&long), None);
        let mut short = alpn_ext(&[ACME_TLS_ALPN]);
        short.pop();
        assert_eq!(alpn_list(&short), None);
        // 名字长度越界 / 零长名字
        assert_eq!(alpn_list(&[0x00, 0x03, 200, b'a', b'b']), None);
        assert_eq!(alpn_list(&[0x00, 0x01, 0x00]), None);
    }

    #[test]
    fn alpn_扫描器不会因为跨条目而误命中() {
        // ⚠ 一个按「在整段字节里 memmem」写的实现会在这里假命中：
        //   `h2` + `acme-tls/1` 拼起来含有子串，但按条目扫就不会。
        //   ★ 这一条是特意冲着「省事的写法」来的。
        let body = alpn_ext(&[b"xacme-tls/1x"]);
        assert!(!offers_alpn(&body, ACME_TLS_ALPN));
    }
}
