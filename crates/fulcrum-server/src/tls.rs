//! 把設定裡的 TLS 意圖變成一個可以掛到監聽器上的證書解析器。
//!
//! ★ ★ **這一步在裝載時做，而且會回報錯誤。** 一份路徑寫錯、私鑰與證書對不上、
//! 或者根本還沒簽發的設定，應當在 `fulcrum validate` 就說清楚——
//! 而不是等第一個用戶端連上來的時候變成一次握手失敗。
//! ⚠ 握手失敗在伺服器端紀錄裡只是一行「TLS error」，看不出是設定寫錯了。

use fulcrum_config::model::TlsMode;
use fulcrum_runtime::Runtime;
use fulcrum_tls::{CertStore, LoadedCert, SniResolver, load_pem_pair, to_loaded};
use log::{info, warn};
use std::path::Path;
use std::sync::Arc;

/// 裝載 TLS 的結果。
pub struct TlsPlan {
    pub resolver: Arc<SniResolver>,
    /// 裝載時要說出來的話。★ 與 `Vec<String>` 的錯誤分開：
    /// 「這個站台還沒有證書」不該讓整個設定裝不上（別的站台還要服務），
    /// 但它**必須被說出來**。
    pub notes: Vec<String>,
}

/// 預設的簽發者目錄名。★ 它是 `<state>/certs/<issuer>/` 裡那一層。
///
/// ⚠ **只是預設值**：`acme_ca` 換了 CA，目錄名就跟著換（見
/// [`fulcrum_acme::issuer_slug`]）。寫死它會讓「換 CA」變成「拿舊 CA 的證書去跑」——
/// 而那張證書在新 CA 那邊根本不存在，續期時會一路錯到底。
pub const DEFAULT_ISSUER: &str = "letsencrypt";

/// 按執行環境圖把證書裝進一個 [`SniResolver`]。
///
/// 硬錯誤（回傳 `Err`）：設定裡顯式給了 PEM 路徑，而它讀不出來。
/// 軟提示（進 `notes`）：站台要自動簽發而儲存裡還沒有那張證書。
///
/// `issuer` 是證書儲存裡的簽發者目錄名，由 `acme_ca` 決定。
/// `default_sni` 是全域選項那一格，**用戶端不帶 SNI 時當作它報了這個名字**。
///
/// ⚠ 它按參數收、不從 `rt` 上取：`Runtime` 只帶路由要用的那幾格全域選項，
/// 而 `issuer`（來自全域塊的 `acme_ca`）本來就是這麼收的 —— 同一類東西走同一條路。
/// ★ 代價是**多一處「呼叫方可能忘了傳」**，而 `None` 與「沒配」在類型上分不開
/// ⇒ 「裝載路徑上真的傳了」這件事由 `tests/serve/run.sh` 9d 在真流量上判。
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
        // 不需要 TLS 的站台跳過。
        if matches!(site.tls.mode, TlsMode::Off) {
            continue;
        }
        if site.hostnames.is_empty() {
            // `:8080` 這種不帶主機名的位址簽不出證書——ACME 要一個網域。
            // ★ 但它也不該被當成錯誤：它可能就是個純 HTTP 的備援站台，
            //   而「連接埠協定不能混」那道門已經保證它不會落在 TLS 連接埠上。
            continue;
        }

        // ★ ★ **覆蓋判據是按「站台」算的，不是按「每一張證書」算的。**
        //
        //   在 example.com 上撞到的假警報就出在這裡：一個站台寫了
        //   `example.com, www.example.com` 兩個主機名，而自動簽發是**每域一張證書**，
        //   於是裝第一張時「www 不在證書裡」、裝第二張時「apex 不在證書裡」——
        //   兩條都在說用戶端會看到證書不匹配，而實測兩邊都 `verify=0`，完全正常。
        //   ⚠ 假警報會訓練人忽略整張表，連帶把這張表上**真的**那幾條一起埋掉。
        //   ⇒ 累計「被任意一張已裝證書覆蓋到的主機名」，站台處理完之後再比一次。
        let mut covered: Vec<String> = Vec::new();

        match &site.tls.mode {
            TlsMode::Manual { cert, key } => match load_pem_pair(cert, key) {
                Ok(loaded) => install(&resolver, site, loaded, &mut covered, "配置里给的 PEM"),
                Err(e) => errors.push(format!("站点 {} 的 `tls {cert} {key}`：{e}", site.name)),
            },
            TlsMode::Automatic => {
                // 儲存裡已經有就用，沒有就說清楚。
                let mut got_any = false;
                for host in &site.hostnames {
                    match store.load(issuer, host) {
                        Ok(Some(sc)) => match to_loaded(sc) {
                            Ok(loaded) => {
                                install(&resolver, site, loaded, &mut covered, "证书存储");
                                got_any = true;
                            }
                            // ★ 儲存裡那張讀出來了卻裝不進 rustls，是**硬錯誤**：
                            //   它不是「還沒簽」，而是「簽了但壞了」，需要人來看。
                            Err(e) => errors
                                .push(format!("站点 {} 的 {host} 的已存证书装不进 rustls：{e}", site.name)),
                        },
                        Ok(None) => {}
                        Err(e) => errors.push(format!("站点 {} 的 {host}：{e}", site.name)),
                    }
                }
                if !got_any {
                    // ★ 這條從「ACME 還沒串接」改成了「還沒簽下來」——**語氣變了，
                    //   因為事實變了**。前者是永久缺口，後者是啟動時的正常瞬態：
                    //   後台巡檢起來之後它會自己消失。
                    //   ⚠ 一條不再成立的警告比沒有警告更糟：它會訓練人忽略警告。
                    // ★ ★ ：這條又一次到了「事實變了，措辭必須跟著變」的時刻。
                    //   DNS-01 已經串接，所以**只有沒配 `dns` 的通配符**才是永久缺口；
                    //   配了的那些，簽發正在路上，和普通網域一樣只是啟動瞬態。
                    //   ⚠ 不分這個岔的話，一個**設定完全正確**的通配符站台每次啟動都會
                    //   收到一句「DNS-01 是下一批」——**假警告會訓練人忽略警告**，
                    //   而這正是本儲存庫上一批剛記過的那條教訓。
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
            // ⚠ ★ ★ **（G104 第 ② 處）：這句話裡的理由換過了。**
            //   舊文寫的是「`resolve()` 是同步的，需要一座橋」—— 那在 BoringSSL 這一側
            //   **已經不成立**，而它是**打給維運看的**：一句說得振振有詞、
            //   卻與程式碼相反的理由，比沒有理由更糟。理由的正體見 `UNWIRED` 那一條。
            notes.push(format!(
                "站点 {} 开了 On-Demand TLS，而它这一批还没接线（欠 `ask` 端点、并发闸门与失败缓存）",
                site.name
            ));
        }

        // ── 覆蓋判據（見上面那段累加器的理由）─────────────────────────────
        //
        // ★ 只在**裝上了至少一張**的時候比：一張都沒裝的站台，
        //   上面那條「還沒簽下來」已經把話說完了，再補一條只是同一件事說兩遍。
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

    // ── 全域 `default_sni`：用戶端不帶 SNI 時當作它報了哪個名字 ────────────────
    //
    // ★ ★ **這裡是這條指令唯一的串接點。** 在它之前兩頭都在——DSL 認得、編譯進
    //   `Global.default_sni`、`SniResolver` 那側的槽與握手期的取數也都在——
    //   **中間沒有人接**：`set_default` 全倉唯一的呼叫方在一條 `#[cfg(test)]` 裡。
    // ⚠ ⚠ 而它**也不在 `UNWIRED` 裡**，所以裝載紀錄一個字都不會說 ⇒ 配了它的人
    //   只會看到不帶 SNI 的用戶端照樣被拒絕握手，且紀錄還在說「你沒配 default_sni」。
    //   ★ 那正是那張清單存在的全部理由所要防的形狀：**要麼真的做，要麼被說出來**。
    if let Some(name) = default_sni {
        resolver.set_default_name(name);
        // ⚠ 判「設定面上有沒有站台服務這個名字」，**不判「現在有沒有證書」**：
        //   自動簽發那張多半是啟動之後才簽下來的，按證書判會把一個正常的啟動瞬態
        //   報成設定錯誤 —— 而假警告會訓練人忽略整張表。
        // ★ 於是兩種缺口分得開：名字打錯是**永久缺口**，在這裡說；證書還沒簽下來是
        //   **啟動瞬態**，由上面那條「儲存裡還沒有它的證書」說。兩條同時出現也不重複。
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

/// 把一張證書裝到解析器上。
///
/// ★ ★ **按證書自己的 SAN 裝，而不是按設定裡寫的站台名。**
/// 理由：握手期該用哪張證書，取決於**證書說它是誰的**——一張買來的證書可能
/// 覆蓋比設定裡寫的更多或更少的網域。而「設定裡寫了 a.com，而**沒有任何一張**
/// 裝上的證書覆蓋 a.com」是一個真問題，它的表現是「用戶端看到證書錯誤，
/// 而伺服器端紀錄裡是一次成功的握手」——除了這裡說出來，沒有別的地方會說。
///
/// ⚠ ⚠ **但那句話必須按「站台」說，不能按「每一張證書」說**：自動簽發是每域一張，
/// 逐張比的話，一個兩主機名的站台每次裝載都會收到兩條互相矛盾的告警，
/// 而兩條都是假的（實測：兩邊都 `verify=0`）。
/// ⇒ 本函式**只負責把覆蓋到的主機名記回 `covered`**，判定交給呼叫方。
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
    // ★ 設定裡寫了、證書裡沒有的那些**不裝**——裝了等於用一張不匹配的證書去應答，
    //   而那比拒絕握手更難查。
}

/// 證書的 SAN 裡有沒有覆蓋這個主機名（含通配符語義）。
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
        // 設定裡寫 `*.example.com`，而證書上正好也是 `*.example.com`：上面第一條已覆蓋。
    }
    false
}

/// 裝載 TLS 之後要說的話，打到紀錄裡。
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
    // ★ 「不帶 SNI 的握手會怎樣」也要說出來：那是 `default_sni` 在執行中**唯一**
    //   看得見的地方。⚠ 不說的話，「配了它」與「它生效了」在現場分不開 ——
    //   而這條指令此前正是卡在這兩者之間：寫得下、編譯得過、執行環境零呼叫方。
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
        // ★ 只吃一層
        assert!(!covers(&sans, "a.b.example.com"));
        assert!(!covers(&sans, "other.org"));
        // 設定裡寫通配符、證書上也是通配符
        assert!(covers(&sans, "*.example.com"));
        // 設定裡寫通配符、證書上只有裸域 → 不覆蓋
        assert!(!covers(&["example.com".to_string()], "*.example.com"));
    }

    // ── 全域 `default_sni` 的裝載判據 ──────────────────────────────────────────

    fn 临时目录(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("fulcrum-plan-tls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建临时目录");
        p
    }

    /// 現簽一對 PEM 放進 `dir`，回傳 `(证书路径, 私钥路径)`。
    /// ★ 現簽而不是提交一張進儲存庫：提交的那張遲早過期，而過期那天紅的是「TLS 壞了」。
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
        // ★ ★ 這一條釘的就是那個缺口本身：`default_sni` 曾經 DSL 認得、編譯得過、
        //   `SniResolver` 那側也備好了槽，**而裝載路徑上沒有任何人接** ——
        //   全倉唯一的 `set_default` 呼叫方在一條 `#[cfg(test)]` 裡。
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

        // ★ 反向那半：沒配就**不許**有預設名字。少了它，一個恆填一個名字的實作照樣綠。
        let plan = plan_tls(&rt, &dir, DEFAULT_ISSUER, None).expect("装载");
        assert_eq!(plan.resolver.default_name(), None);
    }

    #[test]
    fn default_sni_指着没人服务的名字要在装载时说出来() {
        // ⚠ 名字打錯是**永久缺口**：它與「證書還沒簽下來」（啟動瞬態）必須分開說，
        //   否則維運只能看著一條「握手被拒」去猜是哪一種。
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
        // ⚠ 名字照樣裝上去：裝載時說清楚，執行環境按設定辦 —— 不替使用者改設定。
        assert_eq!(
            plan.resolver.default_name().as_deref(),
            Some("typo.example")
        );
    }

    #[test]
    fn default_sni_指着通配站点下的名字不算没人服务() {
        // ⚠ 反向判據：「有沒有站台服務這個名字」若寫成「與某個 hostname 逐字相等」，
        //   一份 `*.a.com` + `default_sni www.a.com` 的**正確**設定每次裝載都會挨一句假警告，
        //   而假警告會訓練人忽略整張表。
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
        // ★ 而通配只吃一層這件事對它同樣成立 —— 兩層的名字確實沒人服務。
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
