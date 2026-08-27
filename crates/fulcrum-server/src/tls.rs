//! 把配置里的 TLS 意图变成一个可以挂到监听器上的证书解析器。
//!
//! ★ ★ **这一步在装载时做，而且会报错。** 一份路径写错、私钥与证书对不上、
//! 或者根本还没签发的配置，应当在 `fulcrum validate` 就说清楚——
//! 而不是等第一个客户端连上来的时候变成一次握手失败。
//! ⚠ 握手失败在服务端日志里只是一行「TLS error」，看不出是配置写错了。

use fulcrum_config::model::TlsMode;
use fulcrum_runtime::Runtime;
use fulcrum_tls::{CertStore, LoadedCert, SniResolver, load_pem_pair, to_loaded};
use log::{info, warn};
use std::path::Path;
use std::sync::Arc;

/// 装载 TLS 的结果。
pub struct TlsPlan {
    pub resolver: Arc<SniResolver>,
    /// 装载时要说出来的话。★ 与 `Vec<String>` 的错误分开：
    /// 「这个站点还没有证书」不该让整个配置装不上（别的站点还要服务），
    /// 但它**必须被说出来**。
    pub notes: Vec<String>,
}

/// 默认的签发者目录名。★ 它是 `<state>/certs/<issuer>/` 里那一层。
///
/// ⚠ **只是默认值**：`acme_ca` 换了 CA，目录名就跟着换（见
/// [`fulcrum_acme::issuer_slug`]）。写死它会让「换 CA」变成「拿旧 CA 的证书去跑」——
/// 而那张证书在新 CA 那边根本不存在，续期时会一路错到底。
pub const DEFAULT_ISSUER: &str = "letsencrypt";

/// 按运行时图把证书装进一个 [`SniResolver`]。
///
/// 硬错误（返回 `Err`）：配置里显式给了 PEM 路径，而它读不出来。
/// 软提示（进 `notes`）：站点要自动签发而存储里还没有那张证书。
///
/// `issuer` 是证书存储里的签发者目录名，由 `acme_ca` 决定。
pub fn plan_tls(rt: &Runtime, cert_root: &Path, issuer: &str) -> Result<TlsPlan, Vec<String>> {
    let resolver = Arc::new(SniResolver::new());
    let store = CertStore::new(cert_root);
    let mut errors: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for site in rt.sites() {
        // 不需要 TLS 的站点跳过。
        if matches!(site.tls.mode, TlsMode::Off) {
            continue;
        }
        if site.hostnames.is_empty() {
            // `:8080` 这种不带主机名的地址签不出证书——ACME 要一个域名。
            // ★ 但它也不该被当成错误：它可能就是个纯 HTTP 的兜底站点，
            //   而「端口协议不能混」那道门已经保证它不会落在 TLS 端口上。
            continue;
        }

        // ★ ★ **覆盖判据是按「站点」算的，不是按「每一张证书」算的。**
        //
        //   在 example.com 上撞到的假警报就出在这里：一个站点写了
        //   `example.com, www.example.com` 两个主机名，而自动签发是**每域一张证书**，
        //   于是装第一张时「www 不在证书里」、装第二张时「apex 不在证书里」——
        //   两条都在说客户端会看到证书不匹配，而实测两边都 `verify=0`，完全正常。
        //   ⚠ 假警报会训练人忽略整张表，连带把这张表上**真的**那几条一起埋掉。
        //   ⇒ 累计「被任意一张已装证书覆盖到的主机名」，站点处理完之后再比一次。
        let mut covered: Vec<String> = Vec::new();

        match &site.tls.mode {
            TlsMode::Manual { cert, key } => match load_pem_pair(cert, key) {
                Ok(loaded) => install(&resolver, site, loaded, &mut covered, "配置里给的 PEM"),
                Err(e) => errors.push(format!("站点 {} 的 `tls {cert} {key}`：{e}", site.name)),
            },
            TlsMode::Automatic => {
                // 存储里已经有就用，没有就说清楚。
                let mut got_any = false;
                for host in &site.hostnames {
                    match store.load(issuer, host) {
                        Ok(Some(sc)) => match to_loaded(sc) {
                            Ok(loaded) => {
                                install(&resolver, site, loaded, &mut covered, "证书存储");
                                got_any = true;
                            }
                            // ★ 存储里那张读出来了却装不进 rustls，是**硬错误**：
                            //   它不是「还没签」，而是「签了但坏了」，需要人来看。
                            Err(e) => errors
                                .push(format!("站点 {} 的 {host} 的已存证书装不进 rustls：{e}", site.name)),
                        },
                        Ok(None) => {}
                        Err(e) => errors.push(format!("站点 {} 的 {host}：{e}", site.name)),
                    }
                }
                if !got_any {
                    // ★ 这条从「ACME 还没接线」改成了「还没签下来」——**语气变了，
                    //   因为事实变了**。前者是永久缺口，后者是启动时的正常瞬态：
                    //   后台巡检起来之后它会自己消失。
                    //   ⚠ 一条不再成立的警告比没有警告更糟：它会训练人忽略警告。
                    // ★ ★ ：这条又一次到了「事实变了，措辞必须跟着变」的时刻。
                    //   DNS-01 已经接线，所以**只有没配 `dns` 的通配符**才是永久缺口；
                    //   配了的那些，签发正在路上，和普通域名一样只是启动瞬态。
                    //   ⚠ 不分这个岔的话，一个**配置完全正确**的通配符站点每次启动都会
                    //   收到一句「DNS-01 是下一批」——**假警告会训练人忽略警告**，
                    //   而这正是本仓库上一批刚记过的那条教训。
                    let wildcard =
                        site.hostnames.iter().any(|h| h.starts_with("*."))
                            && site.tls.dns_provider.is_none();
                    notes.push(format!(
                        "站点 {} 要自动签发，而存储 {}/{} 里还没有它的证书 —— \
                         在后台巡检签下来之前，对它的 TLS 握手会被拒绝{}",
                        site.name,
                        cert_root.display(),
                        issuer,
                        if wildcard {
                            "。★ 它是通配符且没有配 `tls { dns … }`，只能走 DNS-01（G54/G58）—— 它不会被签发"
                        } else {
                            ""
                        }
                    ));
                }
            }
            TlsMode::Internal => notes.push(format!(
                "站点 {} 写的是 `tls internal`（自签），而它这一批还没接线 —— 对它的 TLS 握手会被拒绝",
                site.name
            )),
            TlsMode::Off => {}
        }

        if site.tls.on_demand {
            // ⚠ ★ ★ **（G104 第 ② 处）：这句话里的理由换过了。**
            //   旧文写的是「`resolve()` 是同步的，需要一座桥」—— 那在 BoringSSL 这一侧
            //   **已经不成立**，而它是**打给运维看的**：一句说得振振有词、
            //   却与代码相反的理由，比没有理由更糟。理由的正体见 `UNWIRED` 那一条。
            notes.push(format!(
                "站点 {} 开了 On-Demand TLS，而它这一批还没接线（欠 `ask` 端点、并发闸门与失败缓存）",
                site.name
            ));
        }

        // ── 覆盖判据（见上面那段累加器的理由）─────────────────────────────
        //
        // ★ 只在**装上了至少一张**的时候比：一张都没装的站点，
        //   上面那条「还没签下来」已经把话说完了，再补一条只是同一件事说两遍。
        if !covered.is_empty() {
            let missing: Vec<&String> = site
                .hostnames
                .iter()
                .filter(|h| !covered.iter().any(|c| c == *h))
                .collect();
            if !missing.is_empty() {
                notes.push(format!(
                    "站点 {} 装上的证书**合起来**也覆盖不到这些主机名：{:?}\
                     —— 客户端访问它们会看到证书不匹配（已覆盖：{:?}）",
                    site.name, missing, covered
                ));
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(TlsPlan { resolver, notes })
}

/// 把一张证书装到解析器上。
///
/// ★ ★ **按证书自己的 SAN 装，而不是按配置里写的站点名。**
/// 理由：握手期该用哪张证书，取决于**证书说它是谁的**——一张买来的证书可能
/// 覆盖比配置里写的更多或更少的域名。而「配置里写了 a.com，而**没有任何一张**
/// 装上的证书覆盖 a.com」是一个真问题，它的表现是「客户端看到证书错误，
/// 而服务端日志里是一次成功的握手」——除了这里说出来，没有别的地方会说。
///
/// ⚠ ⚠ **但那句话必须按「站点」说，不能按「每一张证书」说**：自动签发是每域一张，
/// 逐张比的话，一个两主机名的站点每次装载都会收到两条互相矛盾的告警，
/// 而两条都是假的（实测：两边都 `verify=0`）。
/// ⇒ 本函数**只负责把覆盖到的主机名记回 `covered`**，判定交给调用方。
fn install(
    resolver: &SniResolver,
    site: &fulcrum_runtime::SiteRt,
    loaded: LoadedCert,
    covered: &mut Vec<String>,
    source: &str,
) {
    for h in &site.hostnames {
        if covers(&loaded.domains, h) && !covered.iter().any(|c| c == h) {
            covered.push(h.clone());
        }
    }
    info!(
        "装载证书（{source}）：{:?}，有效期至 {:?}",
        loaded.domains, loaded.not_after
    );
    resolver.install(&loaded.domains, loaded.key.clone());
    // ★ 配置里写了、证书里没有的那些**不装**——装了等于用一张不匹配的证书去应答，
    //   而那比拒绝握手更难查。
}

/// 证书的 SAN 里有没有覆盖这个主机名（含通配符语义）。
fn covers(sans: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    for s in sans {
        let s = s.to_ascii_lowercase();
        if s == host {
            return true;
        }
        if let Some(rest) = s.strip_prefix("*.")
            && fulcrum_tls::resolver::wildcard_covers(&format!(".{rest}"), &host)
        {
            return true;
        }
        // 配置里写 `*.example.com`，而证书上正好也是 `*.example.com`：上面第一条已覆盖。
    }
    false
}

/// 装载 TLS 之后要说的话，打到日志里。
pub fn log_tls_notes(plan: &TlsPlan, resolver_desc: &str) {
    for n in &plan.notes {
        warn!("⏳ {n}");
    }
    let known = plan.resolver.known();
    if known.is_empty() {
        warn!("{resolver_desc}：**一张证书都没有装上** —— 所有 TLS 握手都会被拒绝");
    } else {
        info!(
            "{resolver_desc}：已装载 {} 个 SNI：{:?}",
            known.len(),
            known
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_遵守通配符只吃一层() {
        let sans = vec!["example.com".to_string(), "*.example.com".to_string()];
        assert!(covers(&sans, "example.com"));
        assert!(covers(&sans, "a.example.com"));
        assert!(covers(&sans, "A.EXAMPLE.COM"), "大小写不敏感");
        // ★ 只吃一层
        assert!(!covers(&sans, "a.b.example.com"));
        assert!(!covers(&sans, "other.org"));
        // 配置里写通配符、证书上也是通配符
        assert!(covers(&sans, "*.example.com"));
        // 配置里写通配符、证书上只有裸域 → 不覆盖
        assert!(!covers(&["example.com".to_string()], "*.example.com"));
    }
}
