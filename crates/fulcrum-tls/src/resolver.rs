//! 按 SNI 挑證書 —— **BoringSSL 的 `select_certificate_callback`**（G104）。
//!
//! §5.1 第 1 條原本要求實作 rustls 的 `ResolvesServerCert`；**G104 推翻了它**。現在走的是
//! `SslContextBuilder::set_select_certificate_callback(|ClientHello| …)` ——
//! 它包的是 BoringSSL 的 `SSL_early_callback_ctx`，**在幾乎所有 ClientHello 處理之前**被呼叫。
//!
//! # ★ ★ 換過來之後**變簡單**的兩件事，與**變麻煩**的一件
//!
//! **變簡單 ①：不再需要 fork 的 `TlsSettings::with_cert_resolver`。**
//! boringssl 那側的 `TlsSettings` 對 `SslAcceptorBuilder` 實作了 `Deref`/`DerefMut`，
//! 這個 setter 本來就夠得到（FORK.md 改動 8 因此可以刪掉）。
//!
//! **變簡單 ②：`resolve()` 那條「必須同步」的約束沒有了。**
//! rustls 在握手執行緒上同步呼叫 `resolve()`，裡面不能 `await` ——
//! 而這正是 `UNWIRED` 裡 `on_demand` 登記「需要一座橋」的原因。
//! BoringSSL 這側有 `set_async_select_certificate_callback`（原生非同步），
//! 而且 `pingora-boringssl` 自帶 `ext::suspend_when_need_ssl_cert` / `unblock_ssl_cert`。
//! ⏳ 本批不做 On-Demand，但**那條阻塞理由已經到期，別再照抄它**。
//!
//! **變麻煩的一件：ALPN 要自己從擴充裡看。**
//! ⚠ ⚠ 早回呼發生在 **ALPN 協商之前**，所以拿不到「協商結果」，只能看**用戶端提供了什麼**。
//! rustls 那側 `ClientHello::alpn()` 直接給一個迭代器；這邊給的是擴充的原始位元組，
//! 要按 RFC 7301 的框架走一遍（見 [`alpn_list`]，[`offers_alpn`] 建在它上面）。
//! ★ 這**不是**「手寫 ClientHello 解析」——BoringSSL 已經把擴充切出來了，
//! 這裡讀的是一段長度前綴列表，而且它有自己的判據（下面那組單測，命中與錯過都覆蓋）。
//!
//! ★ ★ **（G104 第 ② 處）：這份框架讀法現在有第二個使用者** ——
//! L4 的 ClientHello 預讀（`fulcrum-server` 的 `peek_client_hello`）要的是**整張清單**，
//! 這裡要的是「有沒有 `acme-tls/1`」。⇒ 翻出 [`alpn_list`] 讓兩邊共用**一份**實作，
//! 而不是各寫一份（D18/G66 同一條理由：**讓分家在結構上做不到**）。

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

/// 一張證書連同它的私鑰。**leaf 在 `chain[0]`**（PEM 檔案裡的順序）。
///
/// ★ 它就是 rustls 那邊 `CertifiedKey` 的位置，而形狀更樸素：
/// BoringSSL 不需要「簽名金鑰物件」這一層抽象，握手期直接把 `X509` 與 `PKey` 裝到
/// `SSL` 上即可。
pub struct CertKey {
    pub chain: Vec<X509>,
    pub key: PKey<Private>,
    /// leaf 的 `notAfter`。
    ///
    /// ★ ★ **它由 [`crate::cert_key`] 從 `chain[0]` 自己讀出來，不由呼叫方傳進來。**
    /// 收一個參數的話，「這張證書說自己什麼時候到期」與「我們以為它什麼時候到期」
    /// 就成了兩份，而不一致的那天沒有任何東西會說 —— 表現是一條靜靜回報錯誤值的
    /// `fulcrum_cert_expiry_seconds`，它在告警規則里長得完全正常。
    /// ⇒ 讓分家在結構上做不到（D18/G66 同一條理由）。
    pub not_after: SystemTime,
}

impl CertKey {
    /// 把這張證書裝到一條正在握手的連接上。
    ///
    /// ★ ★ **順序是有講究的**：先 `SSL_use_certificate`（leaf），再逐張
    /// `SSL_add1_chain_cert`（中間證書）。反過來會把中間證書當成 leaf 發出去 ——
    /// 而那種連接在多數用戶端上表現為「證書鏈不完整」，**不是**一個當場可見的錯。
    pub fn install_on(&self, ssl: &mut SslRef) -> Result<(), ErrorStack> {
        let mut it = self.chain.iter();
        let Some(leaf) = it.next() else {
            // 空鏈在 `cert_key()` 就被擋掉了；這裡只是不 panic。
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

/// 一份證書表。
#[derive(Default)]
struct Table {
    /// 精確網域 → 證書。鍵一律小寫。
    exact: BTreeMap<String, Arc<CertKey>>,
    /// `(".example.com", 证书)`，**按後綴長度降序**。
    wildcard: Vec<(String, Arc<CertKey>)>,
    /// 用戶端不帶 SNI 時**當作**它報了哪個名字（全域選項 `default_sni`）。鍵一律小寫。
    ///
    /// ★ ★ **存的是名字，不是那張證書。** 存 `Arc<CertKey>` 的話它就是上面兩張表裡
    /// 那張的第二份副本，而副本會在兩個方向上分家，兩次都沒有任何東西會說：
    ///   · 自動簽發的那張多半在**裝載之後**才裝進表 —— 裝載時按名字取一次是空的，
    ///     於是 `default_sni` 對一個自動簽發的站台等於沒配；
    ///   · 續期換掉表裡那張之後，副本仍指著**舊證書** —— 不帶 SNI 的用戶端拿到一張
    ///     過期證書，而 [`SniResolver::expiries`] 只走 exact/wildcard，
    ///     `fulcrum_cert_expiry_seconds` 上一切正常。
    /// ⇒ 讓分家在結構上做不到（D18/G66 同一條理由）。
    default_name: Option<String>,
}

/// ACME 的 TLS-ALPN-01 用的那個 ALPN 協定名（RFC 8737 §3）。寫死，不可設定。
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// 按 SNI 挑證書。可以在執行期熱裝新證書（ACME 簽發完就裝進來）。
#[derive(Default)]
pub struct SniResolver {
    table: RwLock<Table>,
    /// ★ TLS-ALPN-01 的挑戰證書（G54 的「主」）：網域 → 那張一次性的自簽證書。
    ///
    /// ⚠ ⚠ **它與 `table` 是兩張互不相通的表，這是安全屬性不是潔癖**：
    ///   · 用戶端提供了 `acme-tls/1` 時**只**看這張表 —— 否則會把使用者的真證書
    ///     交給一個只想驗證網域歸屬的對端；
    ///   · 沒提供 `acme-tls/1` 時**絕不**看這張表 —— 否則普通訪客會拿到
    ///     一張自籤的挑戰證書，瀏覽器當場報證書錯誤，而伺服器端紀錄裡是一次成功的握手。
    ///   兩個方向各由一條斷言釘著（`tests/acme/run.sh`）。
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

    /// ★ ★ **把自己掛成這個 context 的證書選擇回呼。**
    ///
    /// 這是 G104 那條決策的落點：h1/h2 入口與（將來的）h3 入口**掛同一個回呼**，
    /// 於是「兩個入口各有一套挑證書實作」在結構上做不到 —— 那正是取「統一 BoringSSL」
    /// 而不是「兩套並存」的全部理由（D18/G66 同源）。
    pub fn install_into(self: &Arc<Self>, builder: &mut SslContextBuilder) {
        let me = self.clone();
        builder.set_select_certificate_callback(move |mut hello| me.select(&mut hello));
    }

    /// 早回呼的正體。
    ///
    /// ⚠ 回傳 `Err(SelectCertError::ERROR)` = **拒絕這次握手**。
    /// ★ 那比「挑一張不匹配的證書」好：後者讓用戶端看到證書錯誤，
    /// 而維運在伺服器端只看到一次成功的握手。
    fn select(&self, hello: &mut ClientHello<'_>) -> Result<(), SelectCertError> {
        // ★ SNI 先抄成 owned：下面要 `ssl_mut()`（`&mut`），
        //   而 `servername()` 借的是 `hello` 本身。
        let sni: Option<String> = hello
            .servername(NameType::HOST_NAME)
            .map(|s| s.to_ascii_lowercase());

        // ── TLS-ALPN-01（RFC 8737）────────────────────────────────────────
        //
        // ★ ★ **提供了 `acme-tls/1` 的連接走一條完全獨立的路**：只查挑戰表，
        //   查不到就**拒絕握手**，絕不回落到真證書。
        //   ⚠ 回落會把使用者的真證書交給一個只需要驗證網域歸屬的對端；
        //   而且 CA 拿到一張沒有 acmeIdentifier 擴充的證書本來就會判失敗，
        //   所以回落連「讓它能用」都換不到——純粹是白送一次證書暴露。
        //
        // ⚠ ⚠ **這裡判的是「用戶端提供了」，不是「協商到了」** —— 早回呼發生在
        //   ALPN 協商之前。兩者的差別在這條路上不重要（真正的 CA 只提供這一個協定），
        //   ★ 但**措辭必須準確**：寫成「協商到」會讓下一個人以為這裡能拿到協商結果。
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
                // ⚠ ⚠ 這一句以前**恆說「沒有配 default_sni」**，而在配了的時候它是**假的**：
                //   `default_sni` 那時編譯得過、結構化設定裡也寫著，卻**沒有任何人接**
                //   （`set_default` 全倉唯一的呼叫方在一條 `#[cfg(test)]` 裡）⇒ 現場是
                //   「配了它，握手照樣被拒，而紀錄說你沒配」——最難查的那一種。
                // ★ 現在這句話只說**解析器自己知道的東西**：預設名字有沒有、夠不夠得到證書。
                //   兩件事分開說，因為處置完全不同：前者要去加一行設定，
                //   後者要去看那個名字的證書為什麼沒簽下來。
                (None, None) => match self.default_name() {
                    None => debug!("客户端没给 SNI，且没有配 default_sni，拒绝握手"),
                    Some(d) => {
                        debug!("客户端没给 SNI，而 default_sni 指的 {d} 现在没有可用证书，拒绝握手")
                    }
                },
                _ => {}
            }
            got
        };

        let Some(ck) = picked else {
            return Err(SelectCertError::ERROR);
        };
        ck.install_on(hello.ssl_mut()).map_err(|e| {
            // ⚠ 這一條與「挑不到」是兩回事：證書挑到了卻裝不上，說明存的東西本身有問題
            //   （私鑰與證書對不上之類）。它必須能與上面那幾行區分開。
            warn!("证书挑到了却装不到连接上：{e}");
            SelectCertError::ERROR
        })
    }

    /// 把一張證書裝到它覆蓋的每個網域上。
    ///
    /// `domains` 通常來自證書自己的 SAN——**不是**設定裡寫的站台名。
    /// ★ 這一點很重要：一張買來的證書可能覆蓋比設定裡寫的更多（或更少）的網域，
    /// 而握手期該用哪張，取決於**證書說它是誰的**。
    pub fn install(&self, domains: &[String], ck: Arc<CertKey>) {
        let mut t = match self.table.write() {
            Ok(t) => t,
            Err(e) => {
                // ⚠ 鎖中毒說明某個持鎖執行緒 panic 過。這裡不 unwrap——
                //   一次裝載失敗不該把整個處理程序帶走。
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
        // ★ 長後綴優先：`*.a.example.com` 必須先於 `*.example.com` 被試到。
        t.wildcard
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    /// 用戶端不帶 SNI 時**當作**它報了這個名字（全域選項 `default_sni`）。
    ///
    /// ⚠ **這裡不檢查這個名字現在有沒有證書**：自動簽發的那張多半是啟動之後才簽下來的，
    /// 裝載時按「取不到就回報錯誤」處置，等於把一個正常的啟動瞬態說成設定錯誤。
    /// 夠不夠得到由**每次握手現查**；裝載時該說的那句話由
    /// `fulcrum_server::tls::plan_tls` 說 —— 它看得見設定裡有沒有站台服務這個名字，
    /// 而這裡只看得見證書表。
    pub fn set_default_name(&self, name: &str) {
        if let Ok(mut t) = self.table.write() {
            t.default_name = Some(name.to_ascii_lowercase());
        }
    }

    /// 現在配著的 `default_sni` 是哪個名字。給裝載紀錄與握手紀錄用。
    pub fn default_name(&self) -> Option<String> {
        self.table.read().ok()?.default_name.clone()
    }

    /// 已裝載的精確網域與通配後綴，給裝載紀錄用。
    ///
    /// ★ 它從 [`Self::expiries`] 派生 —— 兩處各拼一遍鍵的寫法遲早分家，
    /// 而分家之後「裝載紀錄裡的那批網域」與「指標裡的那批網域」看起來都言之鑿鑿。
    pub fn known(&self) -> Vec<String> {
        self.expiries().into_iter().map(|(d, _)| d).collect()
    }

    /// 每個已裝載網域的證書 `notAfter`。**鍵的寫法與 [`Self::known`] 是同一份。**
    ///
    /// ⛔ ⚠ **挑戰表不在裡面**：TLS-ALPN-01 那張一次性自簽證書只活幾秒、也不是站台證書，
    /// 混進來會讓「快過期了」那類告警一直在叫，而叫的是一個根本不該被續期的東西。
    /// ★ 兩張表本來就互不相通（見 `challenge` 欄位），這裡只是不去把它們合起來。
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

    /// 一張證書都沒裝上。
    ///
    /// ⚠ `default_sni` **不算**：它是個名字，不是一張證書。把它算進來的話，一份
    /// 「配了 `default_sni`、而一張證書都沒簽下來」的部署會被說成「裝上了東西」，
    /// 而那正是裝載紀錄最該喊的一種情形。
    pub fn is_empty(&self) -> bool {
        self.table
            .read()
            .map(|t| t.exact.is_empty() && t.wildcard.is_empty())
            .unwrap_or(true)
    }

    /// 掛一張 TLS-ALPN-01 的挑戰證書上去，拿到一個到期自動摘掉的守衛。
    ///
    /// ★ 形狀照 `fulcrum_acme::Http01Store::provision`：**手寫一對 insert/remove
    /// 遲早會漏一次**，而漏掉的後果是一張自簽證書永遠留在表上——
    /// 雖然它只在 `acme-tls/1` 那條路上才會被挑到，但那仍是一條不該留的面。
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

    /// 現在掛著幾張挑戰證書。給紀錄與測試用。
    pub fn challenge_len(&self) -> usize {
        self.challenge.read().map(|c| c.len()).unwrap_or(0)
    }

    /// ★ 挑戰證書**只做精確匹配**，不吃通配、不落 default。
    ///
    /// ⚠ 通配符網域的 TLS-ALPN-01 挑戰，CA 連的是**被授權的那個名字本身**
    /// （`example.com`，不是 `*.example.com`），所以這裡按 CA 會給的 SNI 存取即可。
    /// 讓它吃通配或落 default，等於給「隨便一個 SNI 都能換到一張挑戰證書」開門。
    fn lookup_challenge(&self, sni: Option<&str>) -> Option<Arc<CertKey>> {
        let name = sni?.to_ascii_lowercase();
        self.challenge.read().ok()?.get(&name).cloned()
    }

    fn lookup(&self, sni: Option<&str>) -> Option<Arc<CertKey>> {
        let t = self.table.read().ok()?;
        let lowered;
        let name = match sni {
            Some(n) => {
                lowered = n.to_ascii_lowercase();
                lowered.as_str()
            }
            // 不帶 SNI（老用戶端、或直接用 IP 訪問）→ **當作它報了 `default_sni` 那個名字**。
            // ★ 「當作報了這個名字」而不是「取那張存好的證書」，是這條指令唯一正確的形狀：
            //   名字每次現查，於是後來才簽下來的、以及續期換過的，都自動跟著走。
            None => t.default_name.as_deref()?,
        };
        lookup_named(&t, name)
    }
}

/// 一個**已經摺成小寫**的名字，在這張表裡對應哪張證書。精確 → 通配（長後綴優先）。
///
/// ⚠ ⚠ **它不回落到 `default_sni`。** 用戶端報了一個我們沒有證書的名字，正確處置是
/// 拒絕握手（理由見 `SniResolver::select` 上面那段：拿一張不匹配的證書去應答，
/// 用戶端看到證書錯誤，而維運在伺服器端只看到一次成功的握手）。
/// ★ Caddy 把這兩件事分成兩個選項：`default_sni`（用戶端**沒報**名字）與
/// `fallback_sni`（報了、但沒有對應的證書）。**我們只有前者**，
/// `dsl-reference.md` 全域選項那一行也是這麼寫的 —— 兩邊一致。
/// 反向那半由 `tests/serve/run.sh` 9c 與 `tests/smoke/run.sh` 4c 釘著：
/// **配著 `default_sni` 的部署上，未知 SNI 仍然必須被拒絕握手。**
fn lookup_named(t: &Table, name: &str) -> Option<Arc<CertKey>> {
    if let Some(ck) = t.exact.get(name) {
        return Some(ck.clone());
    }
    t.wildcard
        .iter()
        .find(|(suffix, _)| wildcard_covers(suffix, name))
        .map(|(_, ck)| ck.clone())
}

/// 用戶端在 ALPN 擴充裡提供的**全部**協定名，按原順序。
///
/// 收的是 `ClientHello::get_extension(APPLICATION_LAYER_PROTOCOL_NEGOTIATION)` 的原始體，
/// 按 RFC 7301 §3.1 的框架：**2 位元組列表總長，隨後是若干「1 位元組長度 + 名字」**。
///
/// ★ ★ **這不是「手寫 ClientHello 解析」**：BoringSSL 已經把擴充切出來了，
/// 這裡讀的是一段自帶長度的列表，邊界全部顯式檢查，而且下面那組單測**命中與錯過都覆蓋**。
///
/// ⚠ ⚠ **`None` 與 `Some(vec![])` 是兩件事，不許合併**：
/// 前者是「這段位元組我們沒讀懂」，後者是「一份合法的空清單」。
/// 兩個呼叫方對它們的處置**恰好相同**（都按「沒提供」算），
/// ★ 但那是兩條各自成立的判斷，不是同一條 —— 把它們在這裡壓成一個 `bool`，
/// 下一個需要區分的呼叫方就再也拿不回來了。
///
/// ★ ★ ★ **（G104 第 ② 處）：本函式從 [`offers_alpn`] 裡翻出來了。**
/// L4 的 ClientHello 預讀換到 BoringSSL 之後要的是**整張清單**（分流規則按集合比），
/// 而這裡要的是「有沒有 `acme-tls/1`」。⇒ 兩個問題共用**一份** RFC 7301 框架讀法，
/// 而不是各寫一份 —— 與 D18/G66 同一條理由：**讓分家在結構上做不到**。
pub fn alpn_list(ext_body: &[u8]) -> Option<Vec<Vec<u8>>> {
    if ext_body.len() < 2 {
        return None;
    }
    let declared = u16::from_be_bytes([ext_body[0], ext_body[1]]) as usize;
    let list = &ext_body[2..];
    // ★ 宣告長度與實際長度必須嚴絲合縫。寬容一個位元組，就等於接受一份我們沒讀懂的輸入。
    if declared != list.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < list.len() {
        let n = list[i] as usize;
        i += 1;
        // 零長名字不合法（RFC 7301：`ProtocolName<1..2^8-1>`）；越界同理。
        if n == 0 || i + n > list.len() {
            return None;
        }
        out.push(list[i..i + n].to_vec());
        i += n;
    }
    Some(out)
}

/// 用戶端在 ALPN 擴充裡**提供**了某個協定嗎。
///
/// ⚠ 一切對不上的輸入一律回 `false`（= 當作沒提供），而不是「盡力猜」——
/// 猜錯的方向是「把真證書發給一個 ACME 驗證連接」，那是這一整塊要防的事。
pub fn offers_alpn(ext_body: &[u8], want: &[u8]) -> bool {
    if want.is_empty() {
        return false;
    }
    alpn_list(ext_body).is_some_and(|l| l.iter().any(|p| p == want))
}

/// `*.example.com`（這裡傳的是 `.example.com`）覆蓋誰——**語義在別處，這裡只是轉出去**。
///
/// ★ ★ （D18 / G66）之前這裡自己有一份實作，而站台索引那邊**另有一份、
/// 而且不一樣**（`ends_with` 的後綴匹配）。後果不是抽象的：`a.b.example.com`
/// 被路由到通配站台、然後在這裡拿不到證書 ⇒ 握手失敗，而設定裡看不出問題。
/// → 現在兩邊都調 [`fulcrum_config::host::wildcard_covers`]，**只有一份語義**。
pub use fulcrum_config::host::wildcard_covers;

/// 一張掛著的 TLS-ALPN-01 挑戰證書。析構時自動摘掉。
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
        // ★ 這一條是 RFC 6125 與瀏覽器的實際行為，不是我們的偏好。
        assert!(wildcard_covers(".example.com", "a.example.com"));
        assert!(wildcard_covers(".example.com", "www.example.com"));
        // 裸域不在覆蓋範圍內
        assert!(!wildcard_covers(".example.com", "example.com"));
        // 兩層不在覆蓋範圍內
        assert!(!wildcard_covers(".example.com", "a.b.example.com"));
        // 別的網域當然不在
        assert!(!wildcard_covers(".example.com", "a.example.org"));
        // 空標籤不算
        assert!(!wildcard_covers(".example.com", ".example.com"));
    }

    #[test]
    fn 空解析器什么都挑不出来() {
        let r = SniResolver::new();
        assert!(r.is_empty());
        assert!(r.lookup(Some("a.com")).is_none());
        assert!(r.lookup(None).is_none());
        assert!(r.known().is_empty());
        assert_eq!(r.default_name(), None);
    }

    /// 一張現造的自簽證書。★ 每次調都是**新的**一張，所以下面可以按 `Arc::ptr_eq` 判身分。
    fn 自签(domains: &[&str]) -> Arc<CertKey> {
        let key = rcgen::KeyPair::generate().expect("测试密钥");
        let names: Vec<String> = domains.iter().map(|d| d.to_string()).collect();
        let params = rcgen::CertificateParams::new(names).expect("测试参数");
        let cert = params.self_signed(&key).expect("自签");
        crate::cert_key_from_der(cert.der().to_vec(), key.serialize_der()).expect("造 CertKey")
    }

    #[test]
    fn default_sni_存的是名字而不是配它那一刻的那张证书() {
        // ★ ★ 這一條釘的是 `default_name` 那段註解裡說的兩次分家。一個存
        //   `Arc<CertKey>` 的實作在①就取到空、在③還發著舊證書，而兩處都沒人會說。
        let r = SniResolver::new();

        // ① 先配 default_sni，此刻表裡還沒有它的證書 —— 自動簽發就是這個時序：
        //    證書要等後台巡檢簽下來，那是**裝載之後**的事。
        r.set_default_name("A.Com");
        assert_eq!(r.default_name().as_deref(), Some("a.com"), "名字要折成小写");
        assert!(r.lookup(None).is_none(), "还没有证书，这时候不该挑出东西来");

        // ② 證書後來才裝進來 ⇒ 不帶 SNI 的握手必須**跟著**有。
        let 第一张 = 自签(&["a.com"]);
        r.install(&["a.com".to_string()], 第一张.clone());
        let got = r.lookup(None).expect("装上了就该挑得到");
        assert!(
            Arc::ptr_eq(&got, &第一张),
            "default_sni 指的名字后来才有证书，而不带 SNI 的握手没跟上"
        );

        // ③ 續期換掉表裡那張 ⇒ 必須發**新的**那張。
        //    ⚠ 發舊的那張的表現是「不帶 SNI 的用戶端拿到一張過期證書」，
        //    而 `expiries()` 裡看不到它 —— 到期指標上一切正常。
        let 续期后 = 自签(&["a.com"]);
        r.install(&["a.com".to_string()], 续期后.clone());
        let got = r.lookup(None).expect("续期后照样该挑得到");
        assert!(
            Arc::ptr_eq(&got, &续期后),
            "续期换了证书，而不带 SNI 的握手还在发旧的那张"
        );
    }

    #[test]
    fn default_sni_不给未知的_sni_兜底() {
        // ⚠ 反向那半：配了 `default_sni` 之後，**報了名字**的用戶端不許被備援。
        //   備援會讓用戶端看到證書錯誤，而伺服器端紀錄裡是一次**成功**的握手。
        //   ★ 同一條紀律在真流量上由 `tests/serve/run.sh` 9c 與 `tests/smoke/run.sh` 4c 釘著；
        //   少了這一條，把 `lookup_named` 的結果 `.or(default)` 一下，兩邊都還是綠的
        //   （那兩個場景的部署此前根本沒配 `default_sni`）。
        let r = SniResolver::new();
        r.install(&["a.com".to_string()], 自签(&["a.com"]));
        r.set_default_name("a.com");
        assert!(
            r.lookup(Some("b.com")).is_none(),
            "未知 SNI 被 default_sni 兜住了 —— 它必须被拒绝握手"
        );
        assert!(r.lookup(None).is_some(), "不带 SNI 的那一半仍然要挑得到");
    }

    #[test]
    fn default_sni_走的是与_sni_同一套匹配() {
        // ★ 通配站台下的一個名字也能當 default_sni —— 因為兩條路查的是**同一份**
        //   `lookup_named`。另寫一份「只查精確表」的實作，這一條會紅。
        let r = SniResolver::new();
        r.install(&["*.a.com".to_string()], 自签(&["*.a.com"]));
        r.set_default_name("www.a.com");
        assert!(r.lookup(None).is_some());
        // 通配只吃一層，這一條對 default_sni 同樣成立。
        r.set_default_name("x.y.a.com");
        assert!(r.lookup(None).is_none());
    }

    /// 造一份合法的 ALPN 擴充體。
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
        // 在第一個也算
        assert!(offers_alpn(
            &alpn_ext(&[ACME_TLS_ALPN, b"h2"]),
            ACME_TLS_ALPN
        ));
    }

    #[test]
    fn alpn_扫描器也能错过() {
        // ★ 這一半同樣重要：一個恆真的掃描器會把每一條普通連接都當成 ACME 驗證連接，
        //   於是普通訪客拿到自籤的挑戰證書。
        assert!(!offers_alpn(
            &alpn_ext(&[b"h2", b"http/1.1"]),
            ACME_TLS_ALPN
        ));
        assert!(!offers_alpn(&alpn_ext(&[]), ACME_TLS_ALPN));
        // 前綴不算命中（長度必須相等）
        assert!(!offers_alpn(&alpn_ext(&[b"acme-tls/"]), ACME_TLS_ALPN));
        assert!(!offers_alpn(&alpn_ext(&[b"acme-tls/11"]), ACME_TLS_ALPN));
    }

    #[test]
    fn alpn_扫描器对畸形输入一律回假() {
        // 太短
        assert!(!offers_alpn(&[], ACME_TLS_ALPN));
        assert!(!offers_alpn(&[0x00], ACME_TLS_ALPN));
        // 宣告長度與實際不符（多一個位元組）
        let mut bad = alpn_ext(&[ACME_TLS_ALPN]);
        bad.push(0x00);
        assert!(!offers_alpn(&bad, ACME_TLS_ALPN));
        // 宣告長度與實際不符（少一個位元組）
        let mut short = alpn_ext(&[ACME_TLS_ALPN]);
        short.pop();
        assert!(!offers_alpn(&short, ACME_TLS_ALPN));
        // 名字長度越界：宣告 200 位元組而後面只有幾個
        let body = [0x00u8, 0x03, 200, b'a', b'b'];
        assert!(!offers_alpn(&body, ACME_TLS_ALPN));
        // 零長名字
        let body = [0x00u8, 0x01, 0x00];
        assert!(!offers_alpn(&body, ACME_TLS_ALPN));
        // 想找的東西是空的
        assert!(!offers_alpn(&alpn_ext(&[b"h2"]), b""));
    }

    #[test]
    fn alpn_清单读得出原顺序与原字节() {
        // ★ L4 的分流規則按**集合**比，但清單必須是原樣的位元組 ——
        //   `h2c` 不許被讀成 `h2`，那正是 tests/l4/run.sh ⑤ 釘的那條。
        let got = alpn_list(&alpn_ext(&[b"h2", b"http/1.1", ACME_TLS_ALPN])).expect("该读得出");
        assert_eq!(
            got,
            vec![b"h2".to_vec(), b"http/1.1".to_vec(), ACME_TLS_ALPN.to_vec()]
        );
    }

    #[test]
    fn alpn_清单把没读懂与空清单分开() {
        // ⚠ ⚠ 這一條是本函式存在的全部理由：兩者**恰好**都按「沒提供」處置，
        //   而它們是兩條各自成立的判斷。合併成一個 bool 就再也拿不回來了。
        assert_eq!(alpn_list(&alpn_ext(&[])), Some(Vec::new()), "合法的空清单");
        assert_eq!(alpn_list(&[]), None, "两个字节都没有 ⇒ 没读懂");
        assert_eq!(alpn_list(&[0x00]), None, "只有一个字节 ⇒ 没读懂");
        // 宣告長度與實際不符（兩個方向）
        let mut long = alpn_ext(&[ACME_TLS_ALPN]);
        long.push(0x00);
        assert_eq!(alpn_list(&long), None);
        let mut short = alpn_ext(&[ACME_TLS_ALPN]);
        short.pop();
        assert_eq!(alpn_list(&short), None);
        // 名字長度越界 / 零長名字
        assert_eq!(alpn_list(&[0x00, 0x03, 200, b'a', b'b']), None);
        assert_eq!(alpn_list(&[0x00, 0x01, 0x00]), None);
    }

    #[test]
    fn alpn_扫描器不会因为跨条目而误命中() {
        // ⚠ 一個按「在整段位元組裡 memmem」寫的實作會在這裡假命中：
        //   `h2` + `acme-tls/1` 拼起來含有子串，但按條目掃就不會。
        //   ★ 這一條是特意衝著「省事的寫法」來的。
        let body = alpn_ext(&[b"xacme-tls/1x"]);
        assert!(!offers_alpn(&body, ACME_TLS_ALPN));
    }
}
