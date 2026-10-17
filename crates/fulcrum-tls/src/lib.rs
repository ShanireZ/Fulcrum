//! 樞衡的 TLS 層。
//!
//! | 模組 | 管什麼 | 拍板 |
//! |---|---|---|
//! | [`store`] | 證書儲存：每域一目錄的 PEM、原子寫、檔案鎖 | G55 |
//! | [`resolver`] | 按 SNI 挑證書（BoringSSL 的 `select_certificate_callback`）| **G104** / G12 |
//! | [`renewal`] | 續期判定：ARI 優先，退回剩餘壽命 1/3，退避 + 抖動 | G56 |
//!
//! # ⚠ ⚠ 後端是 BoringSSL（G104）
//!
//! §5.1 第 1 條原本把 TLS 後端鎖死在 rustls（理由：rustls 不支援 `certificate_callback`，
//! 動態證書選擇必須實作 `ResolvesServerCert`）。**G104 推翻了它** —— 本專案第一次推翻
//! 一條寫著「不可回頭」的約束，成因是 G103 取 `quiche` 做 HTTP/3 而 quiche 用 BoringSSL。
//!
//! ⇒ 本 crate 的載體類型整條換過：
//!
//! | 原來（rustls）| 現在（BoringSSL）|
//! |---|---|
//! | `CertifiedKey`（鏈 + 簽名金鑰）| [`CertKey`]（`Vec<X509>` + `PKey<Private>`）|
//! | `impl ResolvesServerCert`（同步 `resolve()`）| [`SniResolver::install_into`] 掛 `set_select_certificate_callback` |
//! | `CertificateDer` / `PrivateKeyDer` | `X509` / `PKey<Private>` |
//!
//! ★ ★ **換的是載體，不是對證書的理解**：有效期與 SAN 仍由 [`store::validity_of`]
//! 走 `x509-parser` 從 DER 裡讀，逐字未改。⇒ 這一批紅了，紅的一定是後端那一層。
//!
//! ★ ★ **BoringSSL 的類型只經 `pingora-boringssl` 拿**，本 crate 不直接依賴 `boring`。
//! 理由與「只經 `pingora-rustls` 拿 rustls」完全一樣，而且更硬：
//! `boring` 的版本必須與 `quiche` 解到同一個（G103/G104），
//! **多一處版本宣告就多一個把它們劈成兩份的機會**，而劈開之後
//! `SslContextBuilder` 同名卻是兩個類型。

pub mod renewal;
pub mod resolver;
pub mod store;

pub use resolver::{ACME_TLS_ALPN, CertKey, ChallengeCert, SniResolver, alpn_list, offers_alpn};
pub use store::{CertStore, Meta, StoredCert};

use pingora_boringssl::pkey::{PKey, Private};
use pingora_boringssl::x509::X509;
use std::sync::Arc;
use std::time::SystemTime;

/// 一張裝好的證書，連同它自己宣告覆蓋哪些網域。
pub struct LoadedCert {
    pub key: Arc<CertKey>,
    /// 來自證書的 SAN，**不是**設定裡寫的站台名。
    pub domains: Vec<String>,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

/// 把證書鏈與私鑰變成握手期能用的 [`CertKey`]。
///
/// ★ ★ **G104 之前這裡要先 `install_default_crypto_provider()`** ——
/// rustls 0.23+ 把「用哪個 crypto provider」做成一個處理程序級的全域選擇，
/// 不裝就會在**第一次簽名時**才炸，而那時已經在握手路徑上了。
/// **BoringSSL 沒有這個概念**：演算法實作就在庫裡，沒有第二個候選，也就沒有裝錯的可能。
/// ⇒ 這裡因此不再需要那一步，而**那條「不調就會在握手時才炸」的坑整條消失了**。
///
/// ★ ★ **`not_after` 在這裡從 leaf 自己讀出來**（`M2 批 M`）：這個函式是
/// [`CertKey`] 唯一的建構點，於是「一張證書說自己什麼時候到期」在結構上只有一個答案。
/// ⚠ 改成收一個參數就等於在證書旁邊再記一份 —— 而 `fulcrum_cert_expiry_seconds`
/// 報出一個與證書本身不符的時刻時，告警規則看不出任何異樣。
pub fn cert_key(chain: Vec<X509>, key: PKey<Private>) -> Result<Arc<CertKey>, String> {
    let not_after = match chain.first() {
        None => return Err("证书链是空的".to_string()),
        // ⚠ 真證書那兩條路（[`load_pem_pair`] / [`to_loaded`]）在到這裡之前已經
        //   解析成功過一次，所以這一支實際只對現造的挑戰證書生效；留著它是因為
        //   「BoringSSL 收得下、x509-parser 讀不懂」在類型上是可表達的。
        Some(leaf) => {
            store::validity_of(leaf)
                .map_err(|e| format!("读不出证书的有效期：{e}"))?
                .1
        }
    };
    Ok(Arc::new(CertKey {
        chain,
        key,
        not_after,
    }))
}

/// 把一張**裸 DER** 證書與一份 PKCS#8 DER 私鑰變成 [`CertKey`]。
///
/// ★ 只給 TLS-ALPN-01 的挑戰證書用（RFC 8737）：那張證書是現造的、只活幾秒、
/// 從不寫入磁碟，走 PEM 編解碼一圈純屬多餘。
/// ⚠ 它**不檢核證書的內容**——呼叫方造出來的東西自己負責（有效期由
/// [`cert_key`] 讀一次，那是 [`CertKey`] 的建構契約，不是對這張證書的檢查）。
/// 真證書那條路走 [`load_pem_pair`] / [`to_loaded`]，前面還有 SAN 抽取與有效期檢查。
pub fn cert_key_from_der(
    cert_der: Vec<u8>,
    pkcs8_key_der: Vec<u8>,
) -> Result<Arc<CertKey>, String> {
    let cert = X509::from_der(&cert_der).map_err(|e| format!("挑战证书不是合法 DER：{e}"))?;
    let key = PKey::private_key_from_pkcs8(&pkcs8_key_der)
        .map_err(|e| format!("挑战证书的私钥不是合法 PKCS#8 DER：{e}"))?;
    cert_key(vec![cert], key)
}

/// 讀一對 PEM 檔案（`tls <cert> <key>`）。
///
/// ★ ★ **這一步必須在裝載時做，不能等到握手**：一份路徑寫錯、權限不對、或者
/// 私鑰與證書對不上的設定，應當在 `fulcrum validate` 就紅，
/// 而不是在第一個用戶端連上來的時候變成一次握手失敗。
pub fn load_pem_pair(cert_path: &str, key_path: &str) -> Result<LoadedCert, String> {
    let cert_pem = std::fs::read(cert_path).map_err(|e| format!("读不动 `{cert_path}`：{e}"))?;
    let key_pem = std::fs::read(key_path).map_err(|e| format!("读不动 `{key_path}`：{e}"))?;
    let chain =
        X509::stack_from_pem(&cert_pem).map_err(|e| format!("`{cert_path}` 解析失败：{e}"))?;
    if chain.is_empty() {
        return Err(format!("`{cert_path}` 里没有证书"));
    }
    let key = PKey::private_key_from_pem(&key_pem)
        .map_err(|e| format!("`{key_path}` 解析失败（里面没有私钥？）：{e}"))?;
    let first = chain
        .first()
        .ok_or_else(|| format!("`{cert_path}` 的证书链是空的"))?;
    let (not_before, not_after, domains) = store::validity_of(first)?;
    let key = cert_key(chain, key)?;
    Ok(LoadedCert {
        key,
        domains,
        not_before,
        not_after,
    })
}

/// 把儲存裡讀出來的一張證書變成可裝載的形態。
pub fn to_loaded(sc: StoredCert) -> Result<LoadedCert, String> {
    let StoredCert {
        chain,
        key,
        not_before,
        not_after,
        domains,
        ..
    } = sc;
    let key = cert_key(chain, key)?;
    Ok(LoadedCert {
        key,
        domains,
        not_before,
        not_after,
    })
}
