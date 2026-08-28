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
/// `default_sni` 是全局选项那一格，**客户端不带 SNI 时当作它报了这个名字**。
///
/// ⚠ 它按参数收、不从 `rt` 上取：`Runtime` 只带路由要用的那几格全局选项，
/// 而 `issuer`（来自全局块的 `acme_ca`）本来就是这么收的 —— 同一类东西走同一条路。
/// ★ 代价是**多一处「调用方可能忘了传」**，而 `None` 与「没配」在类型上分不开
/// ⇒ 「装载路径上真的传了」这件事由 `tests/serve/run.sh` 9d 在真流量上判。
pub fn plan_tls(
    rt: &Runtime,
    cert_root: &Path,
    issuer: &str,
    default_sni: Option<&str>,
) -> Result<TlsPlan, Vec<String>> {
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

    // ── 全局 `default_sni`：客户端不带 SNI 时当作它报了哪个名字 ────────────────
    //
    // ★ ★ **这里是这条指令唯一的接线点。** 在它之前两头都在——DSL 认得、编译进
    //   `Global.default_sni`、`SniResolver` 那侧的槽与握手期的取数也都在——
    //   **中间没有人接**：`set_default` 全仓唯一的调用方在一条 `#[cfg(test)]` 里。
    // ⚠ ⚠ 而它**也不在 `UNWIRED` 里**，所以装载日志一个字都不会说 ⇒ 配了它的人
    //   只会看到不带 SNI 的客户端照样被拒绝握手，且日志还在说「你没配 default_sni」。
    //   ★ 那正是那张清单存在的全部理由所要防的形状：**要么真的做，要么被说出来**。
    if let Some(name) = default_sni {
        resolver.set_default_name(name);
        // ⚠ 判「配置面上有没有站点服务这个名字」，**不判「现在有没有证书」**：
        //   自动签发那张多半是启动之后才签下来的，按证书判会把一个正常的启动瞬态
        //   报成配置错误 —— 而假警告会训练人忽略整张表。
        // ★ 于是两种缺口分得开：名字打错是**永久缺口**，在这里说；证书还没签下来是
        //   **启动瞬态**，由上面那条「存储里还没有它的证书」说。两条同时出现也不重复。
        let served = rt
            .sites()
            .iter()
            .any(|s| !matches!(s.tls.mode, TlsMode::Off) && covers(&s.hostnames, name));
        if !served {
            notes.push(format!(
                "全局 `default_sni {name}`：本配置里没有任何 TLS 站点服务这个名字 —— \
                 不带 SNI 的客户端仍然会被拒绝握手"
            ));
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
    // ★ 「不带 SNI 的握手会怎样」也要说出来：那是 `default_sni` 在运行中**唯一**
    //   看得见的地方。⚠ 不说的话，「配了它」与「它生效了」在现场分不开 ——
    //   而这条指令此前正是卡在这两者之间：写得下、编译得过、运行时零调用方。
    if let Some(d) = plan.resolver.default_name() {
        info!("{resolver_desc}：不带 SNI 的握手按 `default_sni {d}` 挑证书");
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

    // ── 全局 `default_sni` 的装载判据 ──────────────────────────────────────────

    fn 临时目录(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("fulcrum-plan-tls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建临时目录");
        p
    }

    /// 现签一对 PEM 放进 `dir`，返回 `(证书路径, 私钥路径)`。
    /// ★ 现签而不是提交一张进仓库：提交的那张迟早过期，而过期那天红的是「TLS 坏了」。
    fn 现签一对pem(dir: &Path, domain: &str) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("测试密钥");
        let params = rcgen::CertificateParams::new(vec![domain.to_string()]).expect("测试参数");
        let cert = params.self_signed(&key).expect("自签");
        let crt = dir.join(format!("{domain}.crt"));
        let pkey = dir.join(format!("{domain}.key"));
        std::fs::write(&crt, cert.pem()).expect("写证书");
        std::fs::write(&pkey, key.serialize_pem()).expect("写私钥");
        (crt.display().to_string(), pkey.display().to_string())
    }

    fn 建运行时(dsl: &str) -> Runtime {
        let o = fulcrum_config::compile_str("t.Fulcrumfile", dsl);
        assert!(!o.diagnostics.has_errors(), "{}", o.render_diagnostics());
        Runtime::build(&o.config.expect("配置")).expect("运行时图建不起来")
    }

    #[test]
    fn 装载时把_default_sni_接到解析器上() {
        // ★ ★ 这一条钉的就是那个缺口本身：`default_sni` 曾经 DSL 认得、编译得过、
        //   `SniResolver` 那侧也备好了槽，**而装载路径上没有任何人接** ——
        //   全仓唯一的 `set_default` 调用方在一条 `#[cfg(test)]` 里。
        let dir = 临时目录("default-sni");
        let (crt, key) = 现签一对pem(&dir, "a.com");
        let rt = 建运行时(&format!(
            "a.com:8443 {{\n  tls {crt} {key}\n  respond 200\n}}\n"
        ));

        let plan = plan_tls(&rt, &dir, DEFAULT_ISSUER, Some("A.COM")).expect("装载");
        assert_eq!(
            plan.resolver.default_name().as_deref(),
            Some("a.com"),
            "`default_sni` 没被接到解析器上 —— 配了它的人仍然会被拒绝握手"
        );
        assert!(
            plan.notes.is_empty(),
            "配得好好的不该有话说：{:?}",
            plan.notes
        );

        // ★ 反向那半：没配就**不许**有默认名字。少了它，一个恒填一个名字的实现照样绿。
        let plan = plan_tls(&rt, &dir, DEFAULT_ISSUER, None).expect("装载");
        assert_eq!(plan.resolver.default_name(), None);
    }

    #[test]
    fn default_sni_指着没人服务的名字要在装载时说出来() {
        // ⚠ 名字打错是**永久缺口**：它与「证书还没签下来」（启动瞬态）必须分开说，
        //   否则运维只能看着一条「握手被拒」去猜是哪一种。
        let dir = 临时目录("default-sni-orphan");
        let (crt, key) = 现签一对pem(&dir, "a.com");
        let rt = 建运行时(&format!(
            "a.com:8443 {{\n  tls {crt} {key}\n  respond 200\n}}\n"
        ));

        let plan = plan_tls(&rt, &dir, DEFAULT_ISSUER, Some("typo.example")).expect("装载");
        assert!(
            plan.notes.iter().any(|n| n.contains("typo.example")),
            "打错的 default_sni 一个字都没说：{:?}",
            plan.notes
        );
        // ⚠ 名字照样装上去：装载时说清楚，运行时按配置办 —— 不替用户改配置。
        assert_eq!(
            plan.resolver.default_name().as_deref(),
            Some("typo.example")
        );
    }

    #[test]
    fn default_sni_指着通配站点下的名字不算没人服务() {
        // ⚠ 反向判据：「有没有站点服务这个名字」若写成「与某个 hostname 逐字相等」，
        //   一份 `*.a.com` + `default_sni www.a.com` 的**正确**配置每次装载都会挨一句假警告，
        //   而假警告会训练人忽略整张表。
        let dir = 临时目录("default-sni-wild");
        let rt = 建运行时("*.a.com {\n  respond 200\n}\n");
        let plan = plan_tls(&rt, &dir, DEFAULT_ISSUER, Some("www.a.com")).expect("装载");
        assert!(
            !plan
                .notes
                .iter()
                .any(|n| n.contains("没有任何 TLS 站点服务")),
            "{:?}",
            plan.notes
        );
        // ★ 而通配只吃一层这件事对它同样成立 —— 两层的名字确实没人服务。
        let plan = plan_tls(&rt, &dir, DEFAULT_ISSUER, Some("x.y.a.com")).expect("装载");
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("没有任何 TLS 站点服务")),
            "{:?}",
            plan.notes
        );
    }
}
