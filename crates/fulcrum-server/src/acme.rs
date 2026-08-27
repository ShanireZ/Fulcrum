//! 把 [`fulcrum_acme`] 挂到 Pingora 的后台服务上。
//!
//! ★ ★ **这个文件是薄的，而且必须薄。** ACME 的全部逻辑（什么时候该签、怎么签、
//! 失败怎么退避）都在 `fulcrum-acme` 里，那个 crate **不依赖 pingora**——
//! 与 `fulcrum-runtime` 同一条纪律。这里只做三件事：
//! 从运行时图挑出要签的域名、把 `ShutdownWatch` 递进去、给它一个能跑的线程。
//!
//! # ★ 为什么是后台服务而不是启动时同步签一遍
//!
//! **HTTP-01 要求 CA 能连上本机的 80 端口**（RFC 8555 §8.3）。而监听器是
//! `run_forever()` 里才起来的——在那之前签发，CA 来验的时候没人应答，
//! 必然失败一次，还白消耗一次速率配额。
//! 后台服务在监听器起来之后才跑，顺序天然是对的。
//!
//! ⚠ 代价要说清楚：**进程刚起来的那几秒，自动签发的站点还没有证书**，
//! 对它们的握手会被拒绝。这不是缺陷，是 ACME 本身的形状（Caddy 同样如此），
//! 但它必须出现在装载日志里，而不是让人自己去猜。

use async_trait::async_trait;
use fulcrum_acme::dns::ResolverSpec;
use fulcrum_acme::{
    AcmeConfig, AcmeManager, Dns01, DnsProvider, ExecHook, Http01Store, Target, TxtChecker,
};
use fulcrum_config::model::TlsMode;
use fulcrum_runtime::Runtime;
use fulcrum_tls::SniResolver;
use log::{error, info, warn};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use std::sync::Arc;
use std::time::Duration;

/// exec hook 单次调用的超时。
///
/// ★ 一个卡住的 hook 会把整条签发链挂住，而症状只是「证书永远签不下来」。
const EXEC_TIMEOUT: Duration = Duration::from_secs(30);
/// 单次 DNS 查询的超时。
const DNS_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// 两次轮询之间等多久。
const DNS_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 等 TXT 可见最多等多久。
///
/// ★ 超了就当次失败并走退避（G56），**不通知 CA 来验**——
/// 拿一个还没可见的记录去招呼 CA，换来的只是一次必然失败的校验与一次配额消耗。
const DNS_VISIBLE_DEADLINE: Duration = Duration::from_secs(120);

/// 从运行时图挑出「要自动签发」的域名。
///
/// ★ 判据是**站点的 TLS 模式**，不是「地址写成了 https」：`tls <cert> <key>` 的站点
/// 也是 https，但它的证书是用户自己给的，去给它签一张只会白消耗配额，
/// 而且**握手时用哪一张会变成两份配置打架**。
pub fn targets_of(rt: &Runtime) -> Vec<Target> {
    let mut out = Vec::new();
    for site in rt.sites() {
        if !matches!(site.tls.mode, TlsMode::Automatic) {
            continue;
        }
        // ★ ★ DNS-01 的配置是**站点级**的，所以在这里逐站点造一份。
        //   做成全局会在「两个域名分属两家托管商」时直接错——
        //   而 G57 选那两家的理由正是 example.com 在 DNSPod、example.net 在 Cloudflare。
        let dns01 = build_dns01(site);
        for host in &site.hostnames {
            // ⚠ 同一个域名可能被多个站点写到（不同端口），只签一次。
            if out.iter().any(|t: &Target| t.domain == *host) {
                continue;
            }
            out.push(Target {
                domain: host.clone(),
                site: site.name.clone(),
                dns01: dns01.clone(),
            });
        }
    }
    out
}

/// 全进程共用一个 HTTPS 客户端。
///
/// ★ 连接池是它的价值所在：每个站点各建一个，等于每次签发都重新握手一遍。
/// ⚠ 建失败只发生在「平台信任库读不出来」这一种，那是机器级问题，不是站点级的。
fn shared_transport() -> Option<Arc<dyn fulcrum_acme::http::HttpTransport>> {
    static ONCE: std::sync::OnceLock<Option<Arc<dyn fulcrum_acme::http::HttpTransport>>> =
        std::sync::OnceLock::new();
    ONCE.get_or_init(|| match fulcrum_acme::http::HyperTransport::new() {
        Ok(t) => Some(Arc::new(t) as Arc<dyn fulcrum_acme::http::HttpTransport>),
        Err(e) => {
            error!("建不出 HTTPS 客户端（原生 DNS 供应商要用它）：{e}");
            None
        }
    })
    .clone()
}

/// 把一个站点的 `tls { dns … resolvers … }` 造成可用的 [`Dns01`]。
///
/// ★ 这一步里的错误都只能“说出来并返回 None”：配置层（`compile.rs`）已经把
/// 「配了 dns 却没配 resolvers」与「`dns exec` 没给程序」两条在**编译期**拦下了，
/// 能走到这里的只剩「地址写得不对」一种。
fn build_dns01(site: &fulcrum_runtime::SiteRt) -> Option<Arc<Dns01>> {
    let provider_name = site.tls.dns_provider.as_deref()?;
    let provider = match provider_name {
        "exec" => {
            let arg = site.tls.dns_arg.as_ref()?;
            // ⚠ ⚠ 脱敏产物不许被当成真值用。⇒ 见下面那段与 `Secret::is_redacted`。
            if arg.is_redacted() {
                error!(
                    "站点 {} 的 `dns exec` 参数是**脱敏过的**（来自 `fulcrum compile` 的默认产物）—— \
                     用 `fulcrum compile --with-secrets` 重新生成再 load",
                    site.name
                );
                return None;
            }
            DnsProvider::Exec(ExecHook::new(arg.expose(), EXEC_TIMEOUT))
        }
        // ★ 原生两家（G57）。⚠ 凭据来源与 `zones` 都由 `compile.rs` 在**编译期**
        //   拦过一道（G59 第 1、3 条），所以这里剩下的只有「来源串解析不了」，
        //   而那一条在编译期已经按白名单挡掉了——走到这里基本只会成功。
        //   ⚠ 但仍然不 unwrap：**「基本只会成功」不是判据**。
        "cloudflare" | "dnspod" => {
            let arg = site.tls.dns_arg.as_ref()?;
            // ★ ★ ★ **脱敏过的凭据不许被当成凭据。**
            //
            //   `fulcrum compile` 默认把字面量凭据换成 `«已脱敏»`，而那份 JSON 正是
            //   `POST /load` 的载荷。⚠ 少了这一条，那个标记会被原样发给 CA ——
            //   现场表现是「凭据不对」，而**没有任何一处会说「你 load 的是一份脱敏产物」**。
            //   ⇒ 在这里说清楚，并告诉他怎么重新生成。
            if arg.is_redacted() {
                error!(
                    "站点 {} 的凭据是**脱敏过的**（`«已脱敏»`，来自 `fulcrum compile` 的默认产物）—— \
                     本站点的 DNS-01 不会启用。★ 要 load 一份带凭据的配置，\
                     用 `fulcrum compile --with-secrets` 重新生成",
                    site.name
                );
                return None;
            }
            let source = match fulcrum_acme::credential::CredentialSource::parse(arg.expose()) {
                Ok(s) => s,
                Err(e) => {
                    error!(
                        "站点 {} 的 DNS 凭据来源写不对（{e}）—— 本站点的 DNS-01 不会启用",
                        site.name
                    );
                    return None;
                }
            };
            let transport = match shared_transport() {
                Some(t) => t,
                None => {
                    error!(
                        "站点 {} 建不出 HTTPS 客户端 —— 本站点的 DNS-01 不会启用",
                        site.name
                    );
                    return None;
                }
            };
            let zones = site.tls.zones.clone();
            if provider_name == "cloudflare" {
                DnsProvider::Cloudflare(fulcrum_acme::cloudflare::Cloudflare::new(
                    transport, source, zones,
                ))
            } else {
                DnsProvider::Dnspod(fulcrum_acme::dnspod::Dnspod::new(transport, source, zones))
            }
        }
        _ => return None,
    };

    // ── 权威 NS：**存写法，不存解析结果** ──────────────────────────────────
    //
    // ★ ★ 这一段改过两件事：
    //   ① **认主机名**（此前只认 `IP:port`，而 DSL 参考 §4.4 的示例写的就是主机名 ——
    //      文档与实现对不上，写主机名的人得到的是「本站点 DNS-01 不启用」）；
    //   ② **形状判据搬到编译期**（`fulcrum_config::host::parse_resolver`），
    //      所以走到这里的写法**已经是对的**；这里剩下的只有「现在解不解析得出来」，
    //      而那是**网络问题，不是配置问题** —— 处置必须不同。
    //
    // ⚠ ⚠ 关键的一条：解析失败**不再让站点的 DNS-01 静默失能**。
    //   真正的解析发生在每次签发那一刻（见 `ResolverSpec`），失败会变成一次
    //   **响亮的签发失败 + 退避**，而不是一个「配置看起来完全正常、证书永远不来」的黑洞。
    let mut resolvers = Vec::new();
    for raw in &site.tls.resolvers {
        match fulcrum_config::host::parse_resolver(raw) {
            Ok((host, port)) => resolvers.push(ResolverSpec::new(host, port)),
            // 走到这里说明编译期那道门漏了 —— 说清楚是我们的缺陷，别让人去查配置。
            Err(why) => {
                error!(
                    "站点 {} 的 `resolvers {raw}` 没能在编译期被拦下（{why}）—— \
                     这是枢衡自己的缺陷，请连同配置一起报告",
                    site.name
                );
                return None;
            }
        }
    }
    if resolvers.is_empty() {
        return None;
    }

    info!(
        "站点 {} 的 DNS-01：供应商 {provider_name}，向 {} 台权威 NS 确认 TXT 可见（{}）",
        site.name,
        resolvers.len(),
        resolvers
            .iter()
            .map(|r| format!("{}:{}", r.host, r.port))
            .collect::<Vec<_>>()
            .join(" ")
    );

    Some(Arc::new(Dns01 {
        provider,
        checker: TxtChecker::new(
            resolvers,
            DNS_QUERY_TIMEOUT,
            DNS_POLL_INTERVAL,
            DNS_VISIBLE_DEADLINE,
            // ★ 查询 ID 的种子带上 pid：升级窗口里两代进程同时在跑，
            //   用同一串 ID 会让两边的应答彼此看起来像“串包”。
            (std::process::id() & 0xffff) as u16,
        ),
    }))
}

/// 挂在 Pingora 上的 ACME 巡检服务。
pub struct AcmeService {
    manager: Arc<AcmeManager>,
}

impl AcmeService {
    pub fn new(manager: Arc<AcmeManager>) -> AcmeService {
        AcmeService { manager }
    }
}

#[async_trait]
impl BackgroundService for AcmeService {
    async fn start(&self, shutdown: ShutdownWatch) {
        // ★ `ShutdownWatch` 就是 `tokio::sync::watch::Receiver<bool>`，
        //   所以这里不需要任何翻译层——`fulcrum-acme` 直接收它。
        self.manager.run_loop(shutdown).await;
    }
}

/// 按配置造一个 ACME 巡检服务；没有任何域名要自动签发时返回 `None`。
///
/// ★ 返回 `Option` 而不是「造一个什么都不做的服务」：后者会在进程里留一条
/// 空转的线程与一行看不懂的日志，而**「这份配置里没有自动签发」是一个值得说出来的事实**。
pub fn build(
    cfg: &fulcrum_config::StructuredConfig,
    rt: &Runtime,
    resolver: Arc<SniResolver>,
    http01: Arc<Http01Store>,
    state_dir: &str,
) -> Option<Arc<AcmeManager>> {
    let targets = targets_of(rt);
    if targets.is_empty() {
        info!(
            "没有站点需要自动签发（都是 `tls <cert> <key>` / `tls off` / 不带主机名）—— 不起 ACME 巡检"
        );
        return None;
    }
    let acme = AcmeConfig::new(
        cfg.global.acme_ca.as_deref(),
        cfg.global.acme_email.as_deref(),
        state_dir,
    );
    let names: Vec<&str> = targets.iter().map(|t| t.domain.as_str()).collect();
    info!(
        "自动签发：CA {}（存储目录 {}），{} 个域名 {:?}",
        acme.directory_url,
        acme.issuer(),
        names.len(),
        names
    );
    // ⚠ 这一条必须在启动时说：ACME 是**异步**的，进程起来的头几秒证书还没到位。
    //   不说的话，现场看到的是「刚重启就有一批握手失败」，而那看起来像是缺陷。
    //
    // ★ ★ **但只对「现在真的还没有证书」的那几个说。** 在 example.com 上
    //   看见的形状：三张证书都已经从存储里装上了，这一行却照旧说「这 3 个域名的握手
    //   会被拒绝」——它在**每一次 reload 之后**都出现，而每一次都是假的。
    //   ⚠ 与同日修掉的那条「配置里这些主机名不在证书里」是同一族：
    //   **一条在事实已经变了之后照旧输出的警告，会训练人忽略整张表。**
    let pending = pending_of(&names, &resolver.known());
    if pending.is_empty() {
        info!(
            "✅ 这 {} 个域名的证书都已从存储装上 —— 后台巡检只负责到期前续期",
            names.len()
        );
    } else {
        warn!(
            "⏳ 自动签发在监听器起来之后才开始 —— 首次签发完成前，这 {} 个域名的 TLS 握手会被拒绝：{:?}",
            pending.len(),
            pending
        );
    }
    Some(Arc::new(AcmeManager::new(acme, resolver, http01, targets)))
}

/// 这些域名里，**现在还没有证书装上**的是哪几个。
///
/// ★ 抽成一个纯函数是为了能测：那条 ⏳ 日志本身测不了，而**「算得对不对」才是判据**。
/// ⚠ 大小写不敏感：证书存储的目录名与配置里的写法可能不同一种大小写，
/// 而一次大小写造成的「假待签」会让这行警告在每次 reload 后都出现 —— 又变回假警报。
fn pending_of<'a>(names: &[&'a str], known: &[String]) -> Vec<&'a str> {
    names
        .iter()
        .copied()
        .filter(|d| !known.iter().any(|k| k.eq_ignore_ascii_case(d)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulcrum_config::compile_str;

    fn rt_of(src: &str) -> (fulcrum_config::StructuredConfig, Runtime) {
        let outcome = compile_str("t.Fulcrumfile", src);
        let cfg = outcome.config.expect("配置编译不过");
        let rt = Runtime::build(&cfg).expect("运行时图建不起来");
        (cfg, rt)
    }

    #[test]
    fn 只有自动模式的站点进签发清单() {
        let (_cfg, rt) = rt_of(
            "auto.example.com {\n  respond 200\n}\n\
             manual.example.com {\n  tls /c.pem /k.pem\n  respond 200\n}\n\
             http://plain.example.com {\n  respond 200\n}\n",
        );
        let names: Vec<String> = targets_of(&rt).into_iter().map(|t| t.domain).collect();
        // ★ `tls <cert> <key>` 的站点**不能**进来：给它签一张会白耗配额，
        //   而且握手时用哪一张会变成两份配置打架。
        assert_eq!(names, vec!["auto.example.com".to_string()]);
    }

    #[test]
    fn 同一个域名写在两个端口上只签一次() {
        let (_cfg, rt) = rt_of(
            "a.example.com {\n  respond 200\n}\n\
             a.example.com:8443 {\n  respond 200\n}\n",
        );
        let names: Vec<String> = targets_of(&rt).into_iter().map(|t| t.domain).collect();
        assert_eq!(
            names,
            vec!["a.example.com".to_string()],
            "同一个域名签了两次 —— 那是两次订单、两份速率配额"
        );
    }

    #[test]
    fn 待签清单只列真的还没有证书的那几个() {
        // ★ 之前这条判据根本不存在：那行 ⏳ 是**无条件**打的，
        //   于是三张证书都在存储里、也都装上了，日志照旧说「握手会被拒绝」——
        //   而它在**每一次 reload 之后**都出现，每一次都是假的。
        let names = vec!["a.example.com", "b.example.com", "*.c.example.com"];
        let known = vec!["a.example.com".to_string(), "*.c.example.com".to_string()];
        assert_eq!(pending_of(&names, &known), vec!["b.example.com"]);

        // 全都装上了 ⇒ 一个都不待签（这一支对应那条 ✅ 日志）
        let all = vec![
            "a.example.com".to_string(),
            "b.example.com".to_string(),
            "*.c.example.com".to_string(),
        ];
        assert!(pending_of(&names, &all).is_empty());

        // 一个都没装 ⇒ 全都待签（进程第一次起来时的真实形态）
        assert_eq!(pending_of(&names, &[]), names);

        // ⚠ 大小写不敏感：不然一次大小写差异就会让它变回假警报
        let upper = vec!["A.Example.COM".to_string()];
        assert_eq!(
            pending_of(&["a.example.com"], &upper),
            Vec::<&str>::new(),
            "大小写不同被当成了没装上"
        );
    }

    #[test]
    fn 不带主机名的站点不进清单() {
        // `:8080` 签不出证书：ACME 要一个域名。
        let (_cfg, rt) = rt_of("http://:8080 {\n  respond 200\n}\n");
        assert!(targets_of(&rt).is_empty());
    }

    #[test]
    fn 没配_dns_的通配符进清单但接不了() {
        // ★ 进清单是对的：G58 要求 M1 能签通配符。把它从清单里删掉，
        //   就没有东西提醒运维「你这个站点差一段 dns 配置」。
        let (_cfg, rt) = rt_of("*.example.com {\n  respond 200\n}\n");
        let targets = targets_of(&rt);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].is_wildcard());
        assert!(targets[0].dns01.is_none(), "没写 `tls {{ dns … }}`");
        assert!(!targets[0].actionable(), "通配符没有 DNS-01 就接不了");
    }

    #[test]
    fn 配了_dns_exec_与_resolvers_的通配符就接得了() {
        // ★ ★ 这条是 DNS-01 接线的结构判据：同一份通配符配置，
        //   加上 `dns` 与 `resolvers` 之后就从「推迟」变成「可办」。
        let (_cfg, rt) = rt_of(
            "*.example.com {\n  tls {\n    dns exec /bin/true\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n",
        );
        let targets = targets_of(&rt);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].actionable(), "配齐了就该接得了");
        assert!(targets[0].use_dns01());
        // ★ 挑战记录名要把 `*.` 剥掉（RFC 8555 §8.4）。
        assert_eq!(targets[0].challenge_record(), "_acme-challenge.example.com");
    }

    #[test]
    fn resolvers_地址写错时在编译期就红_而不是装载时静默失能() {
        // ★ ★ ★ 这条测试**换过一次契约**（批 20），换法本身要留在这里：
        //
        //   旧契约：地址写错 ⇒ 装载时打一行 ERROR，**这个站点的 DNS-01 整条不启用**。
        //   ⚠ 而 `validate` 退出码是 **0**、站点照常起来、其余日志一切正常 ——
        //   于是一份写错的配置**在每一处都显得正常**，直到那张证书永远签不下来。
        //
        //   新契约：**形状在编译期判**（`FUL-DSL-0029`），配置根本编不过。
        //   ⇒ 判据也跟着换：不再断言「运行时图里 dns01 是 None」，
        //   而是断言**编译这一步就报了错**，且报的是那一条错。
        let outcome = fulcrum_config::compile_str(
            "t.Fulcrumfile",
            "*.example.com {\n  tls {\n    dns exec /bin/true\n    resolvers 不是地址\n  }\n  respond 200\n}\n",
        );
        let codes: Vec<String> = outcome
            .diagnostics
            .items()
            .iter()
            .map(|d| d.code.as_str())
            .collect();
        assert!(
            codes.iter().any(|c| c == "FUL-DSL-0029"),
            "写错的 resolvers 没在编译期被拦下，诊断只有：{codes:?}"
        );
        assert!(
            outcome.config.is_none(),
            "有 error 级诊断却还产出了配置 —— 那等于把错误放行"
        );
    }

    #[test]
    fn resolvers_写主机名是合法的_并且会进到运行时图里() {
        // ⚠ 这一条与上面那条是一对：上面钉「写错要红」，这一条钉**写对不许被误伤**。
        //   ★ 少了它，一个「把所有非 IP 都判错」的实现会让上面那条照常绿。
        let (_cfg, rt) = rt_of(
            "*.example.com {\n  tls {\n    dns exec /bin/true\n    resolvers ns1.example.net ns2.example.net:5353\n  }\n  respond 200\n}\n",
        );
        let targets = targets_of(&rt);
        assert_eq!(targets.len(), 1);
        let dns01 = targets[0]
            .dns01
            .as_ref()
            .expect("主机名写法应当建得出 DNS-01");
        let specs: Vec<(String, u16)> = dns01
            .checker
            .resolvers
            .iter()
            .map(|r| (r.host.clone(), r.port))
            .collect();
        assert_eq!(
            specs,
            vec![
                ("ns1.example.net".to_string(), 53),
                ("ns2.example.net".to_string(), 5353)
            ],
            "★ 存的必须是**名字**，不是启动那一刻解出来的 IP"
        );
        assert!(targets[0].actionable());
    }
}
