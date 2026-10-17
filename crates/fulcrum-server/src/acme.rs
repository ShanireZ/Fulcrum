//! 把 [`fulcrum_acme`] 掛到 Pingora 的後台服務上。
//!
//! ★ ★ **這個檔案是薄的，而且必須薄。** ACME 的全部邏輯（什麼時候該簽、怎麼簽、
//! 失敗怎麼退避）都在 `fulcrum-acme` 裡，那個 crate **不相依 pingora**——
//! 與 `fulcrum-runtime` 同一條紀律。這裡只做三件事：
//! 從執行環境圖挑出要簽的網域、把 `ShutdownWatch` 遞進去、給它一個能跑的執行緒。
//!
//! # ★ 為什麼是後台服務而不是啟動時同步簽一遍
//!
//! **HTTP-01 要求 CA 能連上本機的 80 連接埠**（RFC 8555 §8.3）。而監聽器是
//! `run_forever()` 裡才起來的——在那之前簽發，CA 來驗的時候沒人應答，
//! 必然失敗一次，還白消耗一次速率配額。
//! 後台服務在監聽器起來之後才跑，順序天然是對的。
//!
//! ⚠ 代價要說清楚：**處理程序剛起來的那幾秒，自動簽發的站台還沒有證書**，
//! 對它們的握手會被拒絕。這不是缺陷，是 ACME 本身的形狀（Caddy 同樣如此），
//! 但它必須出現在裝載紀錄裡，而不是讓人自己去猜。

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

/// exec hook 單次呼叫的超時。
///
/// ★ 一個卡住的 hook 會把整條簽發鏈掛住，而症狀只是「證書永遠簽不下來」。
const EXEC_TIMEOUT: Duration = Duration::from_secs(30);
/// 單次 DNS 查詢的超時。
const DNS_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// 兩次輪詢之間等多久。
const DNS_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 等 TXT 可見最多等多久。
///
/// ★ 超了就當次失敗並走退避（G56），**不通知 CA 來驗**——
/// 拿一個還沒可見的記錄去招呼 CA，換來的只是一次必然失敗的檢核與一次配額消耗。
const DNS_VISIBLE_DEADLINE: Duration = Duration::from_secs(120);

/// 從執行環境圖挑出「要自動簽發」的網域。
///
/// ★ 判據是**站台的 TLS 模式**，不是「位址寫成了 https」：`tls <cert> <key>` 的站台
/// 也是 https，但它的證書是使用者自己給的，去給它簽一張只會白消耗配額，
/// 而且**握手時用哪一張會變成兩份設定打架**。
pub fn targets_of(rt: &Runtime) -> Vec<Target> {
    let mut out = Vec::new();
    for site in rt.sites() {
        if !matches!(site.tls.mode, TlsMode::Automatic) {
            continue;
        }
        // ★ ★ DNS-01 的設定是**站台級**的，所以在這裡逐站台造一份。
        //   做成全域會在「兩個網域分屬兩家託管商」時直接錯——
        //   而 G57 選那兩家的理由正是 example.com 在 DNSPod、example.net 在 Cloudflare。
        let dns01 = build_dns01(site);
        for host in &site.hostnames {
            // ⚠ 同一個網域可能被多個站台寫到（不同連接埠），只簽一次。
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

/// 全處理程序共用一個 HTTPS 用戶端。
///
/// ★ 連接池是它的價值所在：每個站台各建一個，等於每次簽發都重新握手一遍。
/// ⚠ 建失敗只發生在「平台信任庫讀不出來」這一種，那是機器級問題，不是站台級的。
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

/// 把一個站台的 `tls { dns … resolvers … }` 造成可用的 [`Dns01`]。
///
/// ★ 這一步裡的錯誤都只能“說出來並回傳 None”：設定層（`compile.rs`）已經把
/// 「配了 dns 卻沒配 resolvers」與「`dns exec` 沒給處理程序」兩條在**編譯期**攔下了，
/// 能走到這裡的只剩「位址寫得不對」一種。
fn build_dns01(site: &fulcrum_runtime::SiteRt) -> Option<Arc<Dns01>> {
    let provider_name = site.tls.dns_provider.as_deref()?;
    let provider = match provider_name {
        "exec" => {
            let arg = site.tls.dns_arg.as_ref()?;
            // ⚠ ⚠ 脫敏產物不許被當成真值用。⇒ 見下面那段與 `Secret::is_redacted`。
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
        // ★ 原生兩家（G57）。⚠ 憑證來源與 `zones` 都由 `compile.rs` 在**編譯期**
        //   攔過一道（G59 第 1、3 條），所以這裡剩下的只有「來源串解析不了」，
        //   而那一條在編譯期已經按白名單擋掉了——走到這裡基本只會成功。
        //   ⚠ 但仍然不 unwrap：**「基本只會成功」不是判據**。
        "cloudflare" | "dnspod" => {
            let arg = site.tls.dns_arg.as_ref()?;
            // ★ ★ ★ **脫敏過的憑證不許被當成憑證。**
            //
            //   `fulcrum compile` 預設把字面值憑證換成 `«已脱敏»`，而那份 JSON 正是
            //   `POST /load` 的載荷。⚠ 少了這一條，那個標記會被原樣發給 CA ——
            //   現場表現是「憑證不對」，而**沒有任何一處會說「你 load 的是一份脫敏產物」**。
            //   ⇒ 在這裡說清楚，並告訴他怎麼重新生成。
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

    // ── 權威 NS：**存寫法，不存解析結果** ──────────────────────────────────
    //
    // ★ ★ 這一段改過兩件事：
    //   ① **認主機名**（此前只認 `IP:port`，而 DSL 參考 §4.4 的範例寫的就是主機名 ——
    //      文件與實作對不上，寫主機名的人得到的是「本站台 DNS-01 不啟用」）；
    //   ② **形狀判據搬到編譯期**（`fulcrum_config::host::parse_resolver`），
    //      所以走到這裡的寫法**已經是對的**；這裡剩下的只有「現在解不解析得出來」，
    //      而那是**網路問題，不是設定問題** —— 處置必須不同。
    //
    // ⚠ ⚠ 關鍵的一條：解析失敗**不再讓站台的 DNS-01 靜默失能**。
    //   真正的解析發生在每次簽發那一刻（見 `ResolverSpec`），失敗會變成一次
    //   **響亮的簽發失敗 + 退避**，而不是一個「設定看起來完全正常、證書永遠不來」的黑洞。
    let mut resolvers = Vec::new();
    for raw in &site.tls.resolvers {
        match fulcrum_config::host::parse_resolver(raw) {
            Ok((host, port)) => resolvers.push(ResolverSpec::new(host, port)),
            // 走到這裡說明編譯期那道門漏了 —— 說清楚是我們的缺陷，別讓人去查設定。
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
            // ★ 查詢 ID 的種子帶上 pid：升級視窗裡兩代處理程序同時在跑，
            //   用同一串 ID 會讓兩邊的應答彼此看起來像“串包”。
            (std::process::id() & 0xffff) as u16,
        ),
    }))
}

/// 掛在 Pingora 上的 ACME 巡檢服務。
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
        //   所以這裡不需要任何翻譯層——`fulcrum-acme` 直接收它。
        self.manager.run_loop(shutdown).await;
    }
}

/// 按設定造一個 ACME 巡檢服務；沒有任何網域要自動簽發時回傳 `None`。
///
/// ★ 回傳 `Option` 而不是「造一個什麼都不做的服務」：後者會在處理程序裡留一條
/// 空轉的執行緒與一行看不懂的紀錄，而**「這份設定裡沒有自動簽發」是一個值得說出來的事實**。
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
    // ⚠ 這一條必須在啟動時說：ACME 是**非同步**的，處理程序起來的頭幾秒證書還沒到位。
    //   不說的話，現場看到的是「剛重新啟動就有一批握手失敗」，而那看起來像是缺陷。
    //
    // ★ ★ **但只對「現在真的還沒有證書」的那幾個說。** 在 example.com 上
    //   看見的形狀：三張證書都已經從儲存裡裝上了，這一行卻照舊說「這 3 個網域的握手
    //   會被拒絕」——它在**每一次 reload 之後**都出現，而每一次都是假的。
    //   ⚠ 與同日修掉的那條「設定裡這些主機名不在證書裡」是同一族：
    //   **一條在事實已經變了之後照舊輸出的警告，會訓練人忽略整張表。**
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

/// 這些網域裡，**現在還沒有證書裝上**的是哪幾個。
///
/// ★ 抽成一個純函式是為了能測：那條 ⏳ 紀錄本身測不了，而**「算得對不對」才是判據**。
/// ⚠ 大小寫不敏感：證書儲存的目錄名與設定裡的寫法可能不同一種大小寫，
/// 而一次大小寫造成的「假待簽」會讓這行警告在每次 reload 後都出現 —— 又變回假警報。
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
        // ★ `tls <cert> <key>` 的站台**不能**進來：給它簽一張會白耗配額，
        //   而且握手時用哪一張會變成兩份設定打架。
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
        // ★ 之前這條判據根本不存在：那行 ⏳ 是**無條件**打的，
        //   於是三張證書都在儲存裡、也都裝上了，紀錄照舊說「握手會被拒絕」——
        //   而它在**每一次 reload 之後**都出現，每一次都是假的。
        let names = vec!["a.example.com", "b.example.com", "*.c.example.com"];
        let known = vec!["a.example.com".to_string(), "*.c.example.com".to_string()];
        assert_eq!(pending_of(&names, &known), vec!["b.example.com"]);

        // 全都裝上了 ⇒ 一個都不待簽（這一支對應那條 ✅ 紀錄）
        let all = vec![
            "a.example.com".to_string(),
            "b.example.com".to_string(),
            "*.c.example.com".to_string(),
        ];
        assert!(pending_of(&names, &all).is_empty());

        // 一個都沒裝 ⇒ 全都待簽（處理程序第一次起來時的真實形態）
        assert_eq!(pending_of(&names, &[]), names);

        // ⚠ 大小寫不敏感：不然一次大小寫差異就會讓它變回假警報
        let upper = vec!["A.Example.COM".to_string()];
        assert_eq!(
            pending_of(&["a.example.com"], &upper),
            Vec::<&str>::new(),
            "大小写不同被当成了没装上"
        );
    }

    #[test]
    fn 不带主机名的站点不进清单() {
        // `:8080` 簽不出證書：ACME 要一個網域。
        let (_cfg, rt) = rt_of("http://:8080 {\n  respond 200\n}\n");
        assert!(targets_of(&rt).is_empty());
    }

    #[test]
    fn 没配_dns_的通配符进清单但接不了() {
        // ★ 進清單是對的：G58 要求 M1 能簽通配符。把它從清單裡刪掉，
        //   就沒有東西提醒維運「你這個站台差一段 dns 設定」。
        let (_cfg, rt) = rt_of("*.example.com {\n  respond 200\n}\n");
        let targets = targets_of(&rt);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].is_wildcard());
        assert!(targets[0].dns01.is_none(), "没写 `tls {{ dns … }}`");
        assert!(!targets[0].actionable(), "通配符没有 DNS-01 就接不了");
    }

    #[test]
    fn 配了_dns_exec_与_resolvers_的通配符就接得了() {
        // ★ ★ 這條是 DNS-01 串接的結構判據：同一份通配符設定，
        //   加上 `dns` 與 `resolvers` 之後就從「推遲」變成「可辦」。
        let (_cfg, rt) = rt_of(
            "*.example.com {\n  tls {\n    dns exec /bin/true\n    resolvers 127.0.0.1:8053\n  }\n  respond 200\n}\n",
        );
        let targets = targets_of(&rt);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].actionable(), "配齐了就该接得了");
        assert!(targets[0].use_dns01());
        // ★ 挑戰記錄名要把 `*.` 剝掉（RFC 8555 §8.4）。
        assert_eq!(targets[0].challenge_record(), "_acme-challenge.example.com");
    }

    #[test]
    fn resolvers_地址写错时在编译期就红_而不是装载时静默失能() {
        // ★ ★ ★ 這條測試**換過一次契約**（批 20），換法本身要留在這裡：
        //
        //   舊契約：位址寫錯 ⇒ 裝載時打一行 ERROR，**這個站台的 DNS-01 整條不啟用**。
        //   ⚠ 而 `validate` 結束代碼是 **0**、站台照常起來、其餘紀錄一切正常 ——
        //   於是一份寫錯的設定**在每一處都顯得正常**，直到那張證書永遠簽不下來。
        //
        //   新契約：**形狀在編譯期判**（`FUL-DSL-0029`），設定根本編不過。
        //   ⇒ 判據也跟著換：不再斷言「執行環境圖裡 dns01 是 None」，
        //   而是斷言**編譯這一步就報了錯**，且報的是那一條錯。
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
        // ⚠ 這一條與上面那條是一對：上面釘「寫錯要紅」，這一條釘**寫對不許被誤傷**。
        //   ★ 少了它，一個「把所有非 IP 都判錯」的實作會讓上面那條照常綠。
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
