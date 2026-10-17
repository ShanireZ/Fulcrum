//! 結構化訪問紀錄（**M2 批 L 第 ② + ③ 步**；D7 由 **G113 + G114** 結案）。
//!
//! 欄位契約定稿在 [`docs/architecture/observability.md`](../../../docs/architecture/observability.md)
//! —— **那一份是權威**，本模組是它的實作。
//!
//! ★ 第 ② 步做的是**固定集**；第 ③ 步加的是**白名單頭**（[`collect`]）
//! 與 **TLS 四格**（[`TlsFields`]）。⚠ 後者在 **h3 上只有三格**（`tls_cipher` 拿不到），
//! 見 [`TlsFields`] 的類型文件 —— ✅ **D27 已結案**（落點 fork 改動 14），
//! 那不是登記在案的缺口，是寫進契約的一條邊界。
//!
//! # ★ ★ 三件被有意分開的事
//!
//! | | 誰 |
//! |---|---|
//! | 一行長什麼樣（欄位清單、順序、預設怎麼表示）| 本模組的 [`Record::to_json_line`] |
//! | 一行**寫到哪** | 本模組的 [`Sink`]（處理程序級註冊表，按路徑去重）|
//! | 一行**要不要寫** | [`fulcrum_runtime::LogLevel::records`]（閾值）|
//!
//! ★ 分開的理由是它們各自會獨立變化：加欄位不該動寫入口，換輸出不該動欄位清單。
//!
//! # ⚠ ⚠ 收尾這一處**同時**喂指標（**M2 批 M**）
//!
//! [`Record::finish`] 先無條件喂 `fulcrum_requests_total` 與
//! `fulcrum_request_duration_seconds`，**再按 `tls` 有沒有值**決定要不要喂
//! `fulcrum_tls_requests_total`（**M2 批 M′ 任務 3**），最後調 [`Record::emit`]
//! （照舊按 `log` 早退）。⚠ 第三族**只對 TLS 請求記賬** ⇒ 它的總數比前兩族小，
//! ⛔ 「三個族總數相等」是一句會在第一條明文請求上紅的話。
//! ★ 綁在一處的理由與代價都寫在 `finish` 的文件上，一句話：
//! **兩處各算一遍 `outcome` / `status` 遲早分家，而分家的表現是兩個數字都言之鑿鑿、卻對不上。**
//! ⚠ 收尾路徑上**唯讀一次時鐘** ⇒ 直方圖那一格與 `duration_ms` 是同一個讀數的兩種單位。
//!
//! # ⚠ ⚠ 「取不到的欄位不出現，而不是給 `null`」
//!
//! 那是契約裡寫死的一條，落法在 [`JsonLine`] 上：只有 `Some` 才會被寫進去。
//! ★ 理由是 `null` 在 logfmt 裡沒有對應物 —— 而 G113 取「JSON 單一格式、欄位扁平」
//! 的全部意義就是讓「以後想加 logfmt」只需換一個序列化器。
//!
//! # ⚠ 代價寫在明處：這一行是**同步**寫的
//!
//! 每條請求收尾時在一把 `Mutex` 裡做一次 `write_all`（一行幾百位元組，追加模式）。
//! ★ 與 §10 **G150**（原 `D20`，磁碟快取的同步 I/O，2026-09-07 結案）
//! 是同一類取捨，而這一處小得多：那邊是一次冷盤**讀**，這邊是一次小**寫**。
//! ⚠ 但它同樣在請求路徑上，所以照 `G150` 的口徑記在這裡 —— **M3 對拍時一起量**，
//! 別現在猜。★ `G150` 定的正是「先把這個數量出來，⛔ 閾值等有數之後再定」。

use fulcrum_runtime::{HeaderPick, LogLevel, LogOutput, LogRt};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// 一行訪問紀錄的**寫入口**。
///
/// ★ ★ 按**絕對路徑**去重：多個站台寫同一個檔案時，它們共用同一個句柄與同一把鎖
/// ⇒ 兩條紀錄行不會互相插進對方中間。⚠ 各開各的話，交錯是**隨機出現**的，
/// 而那種壞法在小流量下永遠不顯形。
#[derive(Clone)]
pub(crate) enum Sink {
    Stderr,
    File(Arc<Mutex<std::fs::File>>),
}

/// 處理程序級的檔案句柄註冊表：**絕對路徑 → 句柄**。
fn files() -> &'static Mutex<BTreeMap<String, Arc<Mutex<std::fs::File>>>> {
    static FILES: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<std::fs::File>>>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 把這份執行環境圖裡配到的每一個紀錄檔案都**在裝載時打開一次**。
///
/// # ★ ★ ★ 為什麼這一步必須在裝載時做，而且失敗要判紅
///
/// 一個打不開的紀錄檔案（路徑打錯、目錄不存在、權限不夠）如果拖到第一個請求才發現，
/// 現場是**服務完全正常、而紀錄一行都沒有** —— 那正是本儲存庫反覆點名的那種
/// 「沒有任何東西會說出來的失效」，而且它偏偏發生在**觀測**這一塊上：
/// ⚠ ⚠ **一個用來「出了事你能知道」的東西，自己壞掉時沒人知道。**
///
/// ⇒ 與 `plan_tls` 對「設定裡給的 PEM 讀不出來」的處置逐字同形：**硬錯誤，裝不上**。
/// ★ 而它天然是原子的：`POST /load` 那條路在換之前呼叫本函式，失敗就整份不生效。
pub(crate) fn open_all(rt: &fulcrum_runtime::Runtime) -> Result<usize, Vec<String>> {
    let mut errors = Vec::new();
    let mut opened = 0usize;
    for site in rt.sites() {
        let Some(cfg) = site.log.as_ref() else {
            continue;
        };
        let LogOutput::File(path) = &cfg.output else {
            continue;
        };
        match open_file(path) {
            Ok(true) => opened += 1,
            Ok(false) => {}
            Err(e) => errors.push(format!(
                "站点 {} 的 `log {{ output file {path} }}`：{e}",
                site.name
            )),
        }
    }
    if errors.is_empty() {
        Ok(opened)
    } else {
        Err(errors)
    }
}

/// 打開（或重複使用）一個紀錄檔案。回傳 `Ok(true)` 表示這一次真的新開了一個。
fn open_file(path: &str) -> std::io::Result<bool> {
    let mut map = files().lock().unwrap_or_else(|p| p.into_inner());
    if map.contains_key(path) {
        return Ok(false);
    }
    // ⚠ `append(true)`：換代時**兩代會同時寫同一個檔案**（老一代還在排空），
    //   而 append 模式下每次 write 的定位與寫入在核心裡是原子的 ⇒ 不會互相覆蓋。
    //   ★ 這一條是「零停機換代」這個產品性質對紀錄提出的要求，不是通用習慣。
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    map.insert(path.to_string(), Arc::new(Mutex::new(f)));
    Ok(true)
}

impl Sink {
    /// 按設定取一個寫入口。★ 檔案那條**只查不開** —— 開在 [`open_all`] 裡，
    /// 而那一步失敗時這份設定根本裝不上。
    fn for_output(out: &LogOutput) -> Option<Sink> {
        match out {
            LogOutput::Stderr => Some(Sink::Stderr),
            LogOutput::File(p) => {
                let map = files().lock().unwrap_or_else(|e| e.into_inner());
                map.get(p).map(|f| Sink::File(f.clone()))
            }
        }
    }

    fn write_line(&self, line: &str) {
        match self {
            Sink::Stderr => {
                // ⚠ 一次 `write_all` 而不是 `eprintln!` 兩次寫：後者可能把
                //   行與換行分成兩次 syscall，而兩代處理程序同時寫時那就會交錯。
                let mut out = std::io::stderr().lock();
                let _ = out.write_all(line.as_bytes());
            }
            Sink::File(f) => {
                let mut g = f.lock().unwrap_or_else(|e| e.into_inner());
                let _ = g.write_all(line.as_bytes());
            }
        }
    }
}

/// 一行扁平 JSON 的拼裝器。
///
/// ★ 不用 `serde_json::Map`，理由有兩條，都不是風格：
/// ① **欄位順序**要按契約裡那個順序（`Map` 預設按字典序，讀起來是亂的）；
/// ② **預設要「不出現」而不是 `null`** —— 那是 `Option` 在這裡唯一的表達方式。
/// ⚠ 值的轉義仍然交給 `serde_json`：手寫轉義是本儲存庫明令不做的那一類。
struct JsonLine(String);

impl JsonLine {
    fn new() -> JsonLine {
        JsonLine(String::with_capacity(384))
    }

    fn sep(&mut self) {
        self.0.push(if self.0.is_empty() { '{' } else { ',' });
    }

    fn str(&mut self, k: &str, v: &str) {
        self.sep();
        let _ = write!(
            self.0,
            "{}:{}",
            serde_json::Value::from(k),
            serde_json::Value::from(v)
        );
    }

    fn opt_str(&mut self, k: &str, v: Option<&str>) {
        if let Some(v) = v {
            self.str(k, v);
        }
    }

    fn num(&mut self, k: &str, v: impl std::fmt::Display) {
        self.sep();
        let _ = write!(self.0, "{}:{v}", serde_json::Value::from(k));
    }

    fn finish(mut self) -> String {
        if self.0.is_empty() {
            self.0.push('{');
        }
        self.0.push_str("}\n");
        self.0
    }
}

/// `proto` 這一格的三個取值。★ **兩個入口建構函式直接用這三個常數**
/// （[`crate::Downstream::h1h2`] / [`crate::Downstream::h3`]）⇒ [`PROTOS`] 與產生它們的
/// 那段程式碼是**同一批名字**，不是抄來的。
///
/// ⚠ 它答不了的：**新加一個入口而忘了把它寫進 [`PROTOS`]** —— 那時基數表那道門看不見。
/// ★ 但 `Record::new` 只有那兩個非測試呼叫點，加第三個入口時人一定會經過它們。
pub const PROTO_H1: &str = "HTTP/1.1";
pub const PROTO_H2: &str = "HTTP/2.0";
pub const PROTO_H3: &str = "HTTP/3.0";

/// `fulcrum_requests_total{proto}` 的取值閉集。見 [`PROTO_H1`]。
pub const PROTOS: &[&str] = &[PROTO_H1, PROTO_H2, PROTO_H3];

/// 一次請求在紀錄裡的樣子。**由 [`crate::Downstream`] 持有，逐段填。**
///
/// ⚠ ⚠ 它**不是**「一份紀錄行的快取」：`status` / `resp_size` 不在這裡，
/// 它們在收尾時直接問 `ServerSession` —— ★ **能從被測物件本身問到的東西，
/// 就不要在旁邊再記一份**，否則兩份遲早會不一致，而不一致的那一天沒有任何東西會說。
pub(crate) struct Record {
    /// 這個站台要不要記、記到哪。`None` = 不記（沒配 `log`，或還沒路由到站台）。
    pub target: Option<Arc<LogRt>>,
    pub started: SystemTime,
    /// ★ 取自**入口**，不是推斷出來的 —— 見 [`crate::Downstream`] 的兩個建構函式。
    pub proto: &'static str,
    pub method: String,
    pub host: String,
    /// **原始**請求目標（path + query），`rewrite` **之前**。
    pub uri: String,
    pub remote_ip: Option<IpAddr>,
    pub remote_port: u16,
    // ⚠ ⚠ **`outcome` 有意不在這裡存一份** —— 它由 [`Record::finish`] 收尾時傳進來。
    //   ★ 與緊鄰的 `status` / `resp_size` 逐字同一條理由（見 [`crate::Downstream::serve`]）：
    //     「兩份遲早不一致，而不一致的那天沒有任何東西會說」。
    //   ★ ★ 換來的是一條更硬的性質：欄位沒了，就沒有「預設值」可言 ⇒
    //     **「某條回傳路徑忘了寫 outcome」從一個靜默缺陷變成一個編譯錯誤**
    //     （`serve_one` 必須求值出一個 [`crate::OutcomeName`] 才能回傳）。
    pub site: Option<String>,
    /// 請求**實際匹配到的那條位址字面值**（G121），批 M 的 `fulcrum_requests_total`
    /// 取數點用它當 `site` 標籤 —— **不進 `to_json_line`**，只是箇中轉站。
    ///
    /// ⚠ ⚠ 它與上面的 `site` 是兩件不同的事，長得像是巧合：`site` = 站台的名字 =
    /// 第一個位址的原文（訪問紀錄的欄位契約，一個字不動）；這一格 = 命中的那一條，
    /// 只留主機名。兩者同名純屬巧合，互不派生，見 [`fulcrum_runtime::Routed`] 的
    /// `site_addr` 欄位文件。
    pub site_addr: Option<Arc<str>>,
    pub upstream: Option<String>,
    pub cache: Option<String>,
    /// TLS 那四格（**M2 批 L 第 ③ 步**）。`None` = **這條連接不是 TLS**，
    /// h1/h2/h3 同一口徑 —— ⇒ 它同時是 `fulcrum_tls_requests_total` 記不記這一筆的判據。
    ///
    /// ⚠ ⚠ **h3 上它不是 `None`**：[`crate::quic::h3_session::quic_digest`] 在連接那一層
    /// 現造一份同類型的 `Digest`（**D27 結案**，落點 fork 改動 14）⇒ 三格有值，
    /// 只有 `cipher` 是空串，見 [`TlsFields`]。
    ///
    /// ★ ★ 這裡曾經寫著「今天它在 h3 上恆為 `None`」並引著 D27。那句話在 D27 結案的
    /// 那一刻就成了假話，而它**一直讀起來完全成立** —— 同一個檔案下面 25 行的類型文件、
    /// `lib.rs` 的 `tls_fields()` 與 `tests/log/run.sh` 第八步三處都在說反話，
    /// 卻沒有任何一處**看得見**這句註解。⇒ 過期註解在本儲存庫內原理上沒有門守得住，
    /// 唯一的守法是**改的那一筆順手把它一起改掉**。
    pub tls: Option<TlsFields>,
    /// 白名單命中的請求頭，**已經是最終形態**（紀錄鍵 + 值）。
    ///
    /// ★ 存成 `Vec<(String, String)>` 而不是在這裡存一份頭映射：
    /// 白名單是**裝載時**就算完的（[`fulcrum_runtime::HeaderPick`]），
    /// 而「這條請求上有沒有這個頭」是**收尾時**問工作階段的 ——
    /// ⇒ 到這裡兩件事都已經有答案了，剩下的只是寫出去。
    pub req_headers: Vec<(String, String)>,
    /// 白名單命中的回應頭。同上。
    pub resp_headers: Vec<(String, String)>,
}

/// 一條連接的 TLS 資訊（**M2 批 L 第 ③ 步**）。
///
/// # h1/h2 四格、h3 三格 —— 少的那一格是有意的
///
/// `version` / `cipher` 來自 `ServerSession::digest()` 裡的 `SslDigest`，
/// `sni` / `alpn` 來自 [`crate::tls::HandshakeInfo`] 塞進 `SslDigest.extension` 的那一份。
/// ★ h3 走同一段程式碼：[`crate::quic::h3_session::quic_digest`] 在連接那一層現造一個同類型的
/// `Digest`，於是 `digest()` 在 h3 上也有值 —— 兩條路在這一層**沒有分叉**。
///
/// ⚠ **`tls_cipher` 在 h3 上恆為空**，那一格因此不出現：quiche 的 `Handshake::cipher()`
/// 鎖在私有 `mod tls` 裡，取不到。⇒ 寧可缺一格，也不寫一個編出來的值。
/// ★ 守它的是 [`tests/log/run.sh`] 第八步：h1/h2 四格都在、h3 三格都在，
/// **且 h3 的紀錄行裡 `tls_cipher` 必須不出現** —— 那條反向斷言防的是有人給它編一個值。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TlsFields {
    /// `TLSv1.3` 這種。⚠ 空串 = 問不出來 ⇒ 那一格不出現。
    pub version: String,
    /// `TLS_AES_256_GCM_SHA384` 這種。⚠ 空串同上。
    pub cipher: String,
    /// 用戶端發的 SNI。`None` = 它沒發（按 IP 直連就沒有）。
    pub sni: Option<String>,
    /// 協商出來的 ALPN（`h2` / `http/1.1` / `acme-tls/1`）。`None` = 沒協商出來。
    pub alpn: Option<String>,
}

/// 按白名單從一份頭映射裡取值。
///
/// ★ ★ 一個函式同時服務請求頭與回應頭 —— 兩者的規範化規則**逐字相同**
/// （小寫、`-` 換 `_`、加前綴），差別只在前綴，而前綴已經在
/// [`HeaderPick::key`] 裡算完了。⚠ 各寫一份的話，將來改多值連接符會漏掉一處。
///
/// ⚠ ⚠ **值用 lossy 轉 UTF-8，而不是「轉不了就丟掉」**：頭值是 opaque 位元組
/// （RFC 9110 的 `obs-text` 允許 0x80–0xFF），而「丟掉」在紀錄裡的樣子是
/// **這個頭不存在** —— 那是一句假話。★ 轉義交給 `serde_json`，與 `uri` 那格同源。
pub(crate) fn collect(picks: &[HeaderPick], map: &http::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for p in picks {
        // ⚠ 多值頭按 `, ` 連接（RFC 9110 §5.3：同名欄位行等價於逗號連接的一行）。
        let joined = map
            .get_all(p.lookup.as_str())
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        // ★ 「在白名單裡」與「這條請求上真的有」是兩件事 —— 後者不成立就不出現。
        if map.get(p.lookup.as_str()).is_some() {
            out.push((p.key.clone(), joined));
        }
    }
    out
}

impl Record {
    pub(crate) fn new(proto: &'static str) -> Record {
        Record {
            target: None,
            started: SystemTime::now(),
            proto,
            method: String::new(),
            host: String::new(),
            uri: String::new(),
            remote_ip: None,
            remote_port: 0,
            site: None,
            site_addr: None,
            upstream: None,
            cache: None,
            tls: None,
            req_headers: Vec::new(),
            resp_headers: Vec::new(),
        }
    }

    /// 拼出那一行（**不含**寫出去這一步）。
    ///
    /// ★ 拆成一個純函式，是為了讓判據可以直接量它 —— 端到端只能證「紀錄裡有這麼一行」，
    /// 證不了「每一個欄位都按契約來」。
    pub(crate) fn to_json_line(
        &self,
        outcome: crate::OutcomeName,
        status: u16,
        resp_size: usize,
        now: SystemTime,
    ) -> String {
        let mut j = JsonLine::new();
        j.str(
            "ts",
            &fulcrum_runtime::template::format_rfc3339_millis(self.started),
        );
        j.str("level", LogLevel::name_for(status));
        j.str("proto", self.proto);
        j.str("method", &self.method);
        j.str("host", &self.host);
        j.str("uri", &self.uri);
        j.num("status", status);
        j.num("resp_size", resp_size);
        // ⚠ 三位小數：毫秒以下沒有意義，而 `{:.3}` 讓它在紀錄裡是定長的。
        let ms = now
            .duration_since(self.started)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        j.num("duration_ms", format!("{ms:.3}"));
        j.str("outcome", outcome.as_str());
        j.opt_str("site", self.site.as_deref());
        j.str(
            "remote_ip",
            &self
                .remote_ip
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        j.num("remote_port", self.remote_port);
        j.opt_str("upstream", self.upstream.as_deref());
        j.opt_str("cache", self.cache.as_deref());
        // ── TLS 那四格（**M2 批 L 第 ③ 步**）─────────────────────────────
        //
        // ⚠ 空串當成「問不出來」而不是寫一個空值：`tls_cipher=""` 在紀錄裡
        //   讀起來像「協商出了一個名字叫空的套件」，而事實是這條連接沒有 TLS
        //   或者那個欄位拿不到。★ 與「取不到的欄位不出現」是同一條契約。
        if let Some(t) = self.tls.as_ref() {
            if !t.version.is_empty() {
                j.str("tls_version", &t.version);
            }
            if !t.cipher.is_empty() {
                j.str("tls_cipher", &t.cipher);
            }
            j.opt_str("tls_sni", t.sni.as_deref());
            j.opt_str("tls_alpn", t.alpn.as_deref());
        }
        // ── 白名單頭（同上）。★ 順序 = 設定裡寫的順序，不是字典序 ──────
        //   ⚠ 兩組分開寫，是為了讓 `req_hdr_*` 與 `resp_hdr_*` 在一行裡挨著 ——
        //     它們本來就來自兩個不同的東西，混在一起讀起來要靠前綴去分。
        for (k, v) in &self.req_headers {
            j.str(k, v);
        }
        for (k, v) in &self.resp_headers {
            j.str(k, v);
        }
        j.finish()
    }

    /// 記一行 —— 如果這個站台配了 `log`，而且這條狀態碼過得了閾值。
    ///
    /// ⚠ ⚠ `now` **由呼叫方給**，不在這裡現取：它與耗時直方圖那一格用的是
    /// **同一個時鐘讀數**（見 [`Record::finish_at`]）。★ 自己現取一次的話，
    /// `duration_ms` 與 `fulcrum_request_duration_seconds` 就成了同一條請求上
    /// **兩次不同的測量** —— 而兩邊的註解都會寫著「它們說的是同一件事」。
    pub(crate) fn emit(
        &self,
        outcome: crate::OutcomeName,
        status: u16,
        resp_size: usize,
        now: SystemTime,
    ) {
        let Some(cfg) = self.target.as_ref() else {
            return;
        };
        if !cfg.level.records(status) {
            return;
        }
        let Some(sink) = Sink::for_output(&cfg.output) else {
            // ⚠ 走到這裡意味著裝載時開過的那個句柄不見了 —— `open_all` 保證不會。
            //   ★ 不 panic、也不靜默：說一句，然後這條請求照常收尾。
            log::error!(
                "访问日志的写入口不见了（{:?}）—— 这一条没记下来",
                cfg.output
            );
            return;
        };
        sink.write_line(&self.to_json_line(outcome, status, resp_size, now));
    }

    /// 一次請求的**收尾**：先喂指標，再記那一行（**M2 批 M**，G116–G121）。
    ///
    /// # ★ ★ ★ 為什麼兩件事必須綁在同一個方法上
    ///
    /// `fulcrum_requests_total` 與 `fulcrum_request_duration_seconds` 的取數點
    /// **只能有這一處**。⚠ 指標與紀錄各算一遍 `outcome` / `status` 的話遲早分家，
    /// 而**分家的表現是兩個數字都言之鑿鑿、卻對不上**（D18 / G66 的形狀）——
    /// 兩邊各自都自洽，於是沒有任何東西會說出來。
    /// ⇒ 同一個 [`Record`]、同一份 `status` / `resp_size`，一處算完兩處用。
    ///
    /// # ⚠ ⚠ 指標在前，而且**不受站台的 `log` 設定管**
    ///
    /// [`Record::emit`] 在「這個站台沒配 `log`」與「這條狀態碼過不了閾值」時早退，
    /// 而**沒配紀錄的站台照樣要有指標**：把指標也掛到 `log` 底下，代價是一整個站台
    /// 在監控裡憑空消失，而設定裡看不出任何問題 —— 那是更壞的一側。
    /// ⇒ 「與訪問紀錄同一段程式碼」說的是**同一份取值**，不是「同一個早退分支」。
    ///
    /// ★ 代價寫在明處：**「指標增量 == 訪問紀錄行數」這條一致性只在配了
    /// `log { level info }` 的站台上成立**，判據場景裡寫死這一點。
    ///
    /// # ⚠ ⚠ 整條收尾路徑上**唯讀一次時鐘**
    ///
    /// 這個函式唯一做的事就是把 `SystemTime::now()` 取出來交給 [`Record::finish_at`]。
    /// ★ 「一處算完兩處用」裡的**耗時**也在內：`fulcrum_request_duration_seconds`
    /// 與紀錄行裡的 `duration_ms` 是同一個讀數的兩種單位（秒 / 毫秒）。
    /// ⚠ 各取各的話，兩邊**處置一致而輸入不一致** —— 註解會寫著「它們說的是同一件事」，
    /// 而它們給不出同一個答案。那比差幾微秒本身糟得多：**程式碼說的和它做的成了兩回事**。
    ///
    /// ⚠ [`Record::emit`] 從此**只剩 [`Record::finish_at`] 一個呼叫方**。
    pub(crate) fn finish(&self, outcome: crate::OutcomeName, status: u16, resp_size: usize) {
        self.finish_at(outcome, status, resp_size, SystemTime::now());
    }

    /// [`Record::finish`] 的全部實作，**收尾時刻由呼叫方給**。
    ///
    /// ★ 拆出這一層，是為了讓「兩條路拿到的是同一個 `now`」變成**判據量得到的事**：
    /// 判據喂一個已知時刻進來，然後同時量指標那一格與紀錄那一行。
    /// ⚠ 不拆的話，那條判據只能去比兩次真即時鐘讀數的差 —— 而那個差是微秒級的，
    /// **一條在理論上成立、實際上永遠紅不起來的斷言**比沒有斷言更壞。
    fn finish_at(
        &self,
        outcome: crate::OutcomeName,
        status: u16,
        resp_size: usize,
        now: SystemTime,
    ) {
        self.meter(outcome, status, now);
        self.emit(outcome, status, resp_size, now);
    }

    /// 把這一條餵給指標註冊表。
    ///
    /// ★ 收 `now` 而不是自己取，理由與 [`Record::to_json_line`] 一樣：
    /// 判據要能不靠真即時鐘量它。⚠ 而且它與 [`Record::emit`] 收的是**同一個**讀數。
    fn meter(&self, outcome: crate::OutcomeName, status: u16, now: SystemTime) {
        // ★ `site` 取**請求實際命中的那條位址字面值**（G121）；沒匹配到站台時取
        //   `<none>`（R2）—— 於是 `fulcrum_requests_total` 是**真的總數**，
        //   一致性門才有東西可對。⚠ 尖括號形式在合法位址字面值裡不可能出現，
        //   與 G118 的 `<other>` 是同一族寫法。
        let site = self.site_addr.as_deref().unwrap_or("<none>");
        // ⚠ 標籤的個數與順序照 `metrics::FAMILIES` 的宣告，對不上就地 panic。
        crate::metrics::REQUESTS_TOTAL.inc(&[
            site,
            outcome.as_str(),
            status_class(status),
            self.proto,
        ]);
        // ⚠ 單位是**秒**，與桶邊界和名字裡的 `_seconds` 同一口徑。
        // ⚠ ⚠ 與 `to_json_line` 裡的 `duration_ms` **同一個 `now`、同一種處置**
        //   （`duration_since` 失敗即時鐘回撥時按 0 記）⇒ 兩處對同一件事給同一個答案。
        //   ★ 只對齊處置、不對齊輸入，是一句讀起來完全成立而實際不成立的話。
        let secs = now
            .duration_since(self.started)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        crate::metrics::REQUEST_DURATION_SECONDS.observe(&[site, outcome.as_str()], secs);

        // ── TLS 那一族（**M2 批 M′ 任務 3**，G122 的 TLS 半 / G127）────────────
        //
        // ⚠ ⚠ **明文請求一筆都不記** —— `tls` 是 `None` 就是「這條連接不是 TLS」。
        //   ⇒ 這個族的總數**小於** `fulcrum_requests_total` 的總數，那是有意的，
        //   ⛔ 不許寫「兩個族總數相等」那種判據：它會在第一條明文請求上紅。
        // ★ 取數點與上面兩族**逐字同一處**（`REQUESTS_TOTAL` 那條「只能有一處」的紀律
        //   管的是同一件事：兩處各算一遍，遲早分家，而分家時兩個數各自都言之鑿鑿）。
        if let Some(t) = self.tls.as_ref() {
            // ★ ★ 取不到就記 `<unknown>`（G127）。⚠ 判據是**值為空**，
            //   ⛔ 不是「這條請求走的是 h3 嗎」—— 後者是把一個今天成立的巧合釘進程式碼：
            //   哪天別的傳輸也讀不出套件，它會走到分支外面記一個空串，而那時不會有東西紅。
            // ⚠ `version` 與 `cipher` **同一條規則**，⛔ 不給 `cipher` 開特例。
            crate::metrics::TLS_REQUESTS_TOTAL.inc(&[
                crate::metrics::label_or_unknown(&t.version),
                crate::metrics::label_or_unknown(&t.cipher),
            ]);
        }
    }
}

/// `fulcrum_requests_total{status_class}` 那一格：狀態碼的**類**，閉集 **6 個值**（R1）。
///
/// # ★ `status == 0` 取 `none`，不是 `0xx`，也不是「不計」
///
/// 0 = **一個回應頭都沒寫出去**；按契約那不是「未知」，是**什麼都沒發生**。
/// ⚠ 它可達，而且會被記進訪問紀錄（`LogLevel::All.records(0)` 為真）——
/// 站台匹配上了、執行鏈跑完了、下游在我們寫回應之前斷開時就是這個形狀。
/// ⇒ 閉集是 `1xx` `2xx` `3xx` `4xx` `5xx` `none`，**6 個不是 5 個**。
///
/// # ⚠ 不是合法狀態碼的那些也歸 `none`，而不是各開一格
///
/// 1–99 與 600 以上不是 HTTP 狀態碼，只可能來自上游給的一個編出來的數。
/// ★ 給它們開新格，就是把這一格的**上界交回給別人** —— 而基數那張表管的正是這件事。
///
/// `status_class` 這一格的**取值閉集**，`metrics.rs` 的基數表判據按它算上界。
///
/// ⚠ ⚠ ★ **它不是一份手抄**：下面那條單測**窮舉整個 `u16`**，斷言
/// [`status_class`] 的值域與本常數**互為子集**（兩個方向）——
/// ⇒ 這裡多一個、少一個、拼錯一個，或者 `status_class` 多一條臂，**當場紅**。
/// ★ 立它的直接原因：`observability.md` 那張基數表裡的 `6` 此前是**抄**過來的，
/// 而這一格從 5 變 6 的那一刻，那張表說了假話且**沒有任何東西會紅**。
pub const STATUS_CLASSES: &[&str] = &["1xx", "2xx", "3xx", "4xx", "5xx", "none"];

/// ⚠ 有意**不寫 `_ =>` 備援臂**：範圍鋪滿整個 `u16`，閉集是不是窮盡的由編譯器證。
pub(crate) fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        0..=99 | 600.. => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rec() -> Record {
        let mut r = Record::new("HTTP/1.1");
        r.started = SystemTime::UNIX_EPOCH + Duration::from_millis(1_787_000_000_123);
        r.method = "GET".into();
        r.host = "a.example".into();
        r.uri = "/x?y=1".into();
        r.remote_ip = Some("192.0.2.7".parse().unwrap());
        r.remote_port = 56324;
        r.site = Some("a.example".into());
        r
    }

    #[test]
    fn 一行是合法的扁平_json_而且字段按契约来() {
        let r = rec();
        let s = r.to_json_line(
            crate::OUTCOME_REVERSE_PROXY,
            200,
            12,
            r.started + Duration::from_micros(1500),
        );
        assert!(s.ends_with("}\n"), "一行一条，必须以换行收尾：{s}");
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).expect("必须是合法 JSON");
        let o = v.as_object().expect("顶层必须是对象");
        // ★ ★ **扁平**：一個嵌套物件/陣列都不許有 —— 那是 G113 的全部意義
        //   （以後想加 logfmt 時只換序列化器，欄位清單一個字不動）。
        for (k, val) in o {
            assert!(
                !val.is_object() && !val.is_array(),
                "字段 `{k}` 不是标量 —— 契约要求扁平"
            );
        }
        // ★ ★ 斷言的是**本層的責任**：`ts` 是「這條請求開始的那一刻」。
        //   ⚠ 寫一個字面值就等於把 `format_rfc3339_millis` 的正確性也抄進本層，
        //   而那件事在 `fulcrum-runtime` 裡已經有六個已知時刻釘著 ——
        //   抄一份的唯一效果是它哪天漂了會有兩個地方一起紅，且都指不到根因。
        assert_eq!(
            o["ts"],
            fulcrum_runtime::template::format_rfc3339_millis(r.started)
        );
        assert!(
            o["ts"].as_str().unwrap().ends_with("Z"),
            "契约要求 UTC 且带 Z"
        );
        assert_eq!(o["level"], "info");
        assert_eq!(o["proto"], "HTTP/1.1");
        assert_eq!(o["method"], "GET");
        assert_eq!(o["host"], "a.example");
        assert_eq!(o["uri"], "/x?y=1");
        assert_eq!(o["status"], 200);
        assert_eq!(o["resp_size"], 12);
        assert_eq!(o["outcome"], "reverse_proxy");
        assert_eq!(o["site"], "a.example");
        assert_eq!(o["remote_ip"], "192.0.2.7");
        assert_eq!(o["remote_port"], 56324);
        assert_eq!(o["duration_ms"], 1.5);
    }

    // ★ ★ ★ R3 的反向判據（任務 2 · G121）：`site_addr` 是 `fulcrum_requests_total`
    //   取數的中轉站，**不屬於**訪問紀錄的欄位契約。少了這一條，日後有人往
    //   `to_json_line` 裡順手補一行 `j.opt_str("site_addr", ...)`，會靜默地把
    //   訪問紀錄欄位契約改掉——而這正是任務 2 明令「一個字都不許改」的那一處。
    #[test]
    fn site_addr_不进访问日志的_json_行() {
        let mut r = rec();
        r.site_addr = Some(Arc::from("a.example"));
        let s = r.to_json_line(crate::OUTCOME_RESPOND, 200, 0, r.started);
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        let o = v.as_object().unwrap();
        assert!(
            !o.contains_key("site_addr"),
            "`site_addr` 不该出现在访问日志的 JSON 行里——它只是指标取数的中转站"
        );
        // `site` 欄位本身不受影響，仍然是契約裡那個「站台名字 = 第一個位址原文」。
        assert_eq!(o["site"], "a.example");
    }

    #[test]
    fn 取不到的字段不出现_而不是给_null() {
        let r = rec();
        let s = r.to_json_line(crate::OUTCOME_RESPOND, 200, 0, r.started);
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        let o = v.as_object().unwrap();
        // ★ ★ ★ 這一條守的是契約裡那句話本身。⚠ 若換成 `null`，
        //   `upstream=` 與「這條請求沒有上游」在 logfmt 裡就分不開了。
        assert!(!o.contains_key("upstream"), "没有上游时这个键不该出现");
        assert!(!o.contains_key("cache"), "没过缓存时这个键不该出现");
        // 而給了值的那些必須出現。
        let mut r2 = rec();
        r2.upstream = Some("10.0.0.1:8080".into());
        r2.cache = Some("HIT".into());
        let v2: serde_json::Value = serde_json::from_str(
            r2.to_json_line(crate::OUTCOME_RESPOND, 200, 0, r2.started)
                .trim_end(),
        )
        .unwrap();
        assert_eq!(v2["upstream"], "10.0.0.1:8080");
        assert_eq!(v2["cache"], "HIT");
    }

    #[test]
    fn level_按状态码派生_与阈值无关() {
        let r = rec();
        let lv = |st: u16| {
            let s = r.to_json_line(crate::OUTCOME_RESPOND, st, 0, r.started);
            let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
            v["level"].as_str().unwrap().to_string()
        };
        assert_eq!(lv(200), "info");
        assert_eq!(lv(304), "info");
        assert_eq!(lv(404), "warn");
        assert_eq!(lv(421), "warn");
        assert_eq!(lv(502), "error");
    }

    #[test]
    fn 阈值决定哪些行被写出来() {
        // ⚠ 這是與上一條**不同**的一件事，而它們的取值名長得一樣 ——
        //   契約裡為此專門寫了一段。
        assert!(LogLevel::All.records(200) && LogLevel::All.records(500));
        assert!(!LogLevel::Warn.records(200));
        assert!(LogLevel::Warn.records(404) && LogLevel::Warn.records(500));
        assert!(!LogLevel::Error.records(404));
        assert!(LogLevel::Error.records(500));
    }

    #[test]
    fn 值里的引号与换行被转义_而不是把那一行拆成两行() {
        // ⚠ ⚠ `uri` 是**用戶端給的**。一個帶引號或換行的請求目標，
        //   若不轉義就能**偽造出一整行紀錄** —— 那是紀錄注入。
        //   ★ 這條判據的對照物是「解析得回來」：只要 `serde_json` 認得，
        //     就說明那些位元組沒有跑到 JSON 結構外面去。
        let mut r = rec();
        r.uri = "/a\"b\nc\\d".into();
        let s = r.to_json_line(crate::OUTCOME_RESPOND, 200, 0, r.started);
        assert_eq!(
            s.matches('\n').count(),
            1,
            "整条日志只许有行尾那一个换行：{s:?}"
        );
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).expect("必须仍是合法 JSON");
        assert_eq!(v["uri"], "/a\"b\nc\\d", "转义之后必须还原得回来");
    }

    #[test]
    fn 没配_log_的站点一行都不写() {
        // ★ `target` 為 `None` 時 `emit` 直接回傳 —— 判據取「它不會 panic 也不寫」，
        //   真正「有沒有寫出去」由第二十三個場景在真檔案上量。
        let r = rec();
        assert!(r.target.is_none());
        r.emit(crate::OUTCOME_RESPOND, 200, 0, r.started); // 不該有任何副作用
    }

    // ★ ★ **換契約留痕（⛔ 有意不刪這一格）。**
    //
    // 舊判據叫「未成形的請求也有一個說得出口的 `outcome`」，釘的是 `Record` 上那個
    // `outcome: "aborted"` 預設欄位。⇒ 那個欄位與那個預設值**今天都不存在了**：
    // `outcome` 改成收尾時傳參，於是「沒被寫過」這個狀態在類型上表達不出來。
    // ⚠ ⚠ 順帶糾正舊判據註解裡的一句錯話：它說這一格覆蓋「讀不到請求頭那一類」——
    // 而那一類在 `process_new_http` 裡是在 `Downstream` 造出來**之前** `return None`，
    // **根本不產生 `Record`**，既沒有紀錄行也沒有指標。
    //
    // ⇒ 換成釘**新**契約的這一條：紀錄裡那一格逐字就是收尾時傳進來的那個值。
    #[test]
    fn 日志里的_outcome_就是收尾时传进来的那个值() {
        let r = Record::new("HTTP/1.1");
        for want in [
            crate::OUTCOME_ACME_HTTP01,
            crate::OUTCOME_NO_SITE_MATCH,
            crate::OUTCOME_ERROR,
        ] {
            let s = r.to_json_line(want, 400, 0, r.started);
            let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
            assert_eq!(
                v["outcome"],
                want.as_str(),
                "日志那一格必须逐字是传进来的值"
            );
        }
    }

    #[test]
    fn 多个站点写同一个路径时共用同一个句柄() {
        // ★ ★ 各開各的話，兩條紀錄行互相插進對方中間是**隨機出現**的，
        //   而那種壞法在小流量下永遠不顯形。
        let dir = std::env::temp_dir().join(format!("fulcrum-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.json");
        let path = p.to_string_lossy().to_string();
        assert!(open_file(&path).unwrap(), "第一次应当真的新开一个");
        assert!(!open_file(&path).unwrap(), "第二次必须复用，不许再开一个");
        let a = Sink::for_output(&LogOutput::File(path.clone())).expect("拿得到");
        let b = Sink::for_output(&LogOutput::File(path.clone())).expect("拿得到");
        match (a, b) {
            (Sink::File(x), Sink::File(y)) => {
                assert!(Arc::ptr_eq(&x, &y), "两次拿到的必须是同一个句柄")
            }
            _ => panic!("应当是文件那一支"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 打不开的路径是错误_不是静默降级() {
        // ★ ★ ★ 一個用來「出了事你能知道」的東西，自己壞掉時必須有人知道。
        let bad = "/proc/这个目录不存在/x.json";
        assert!(open_file(bad).is_err(), "打不开必须是 Err");
    }
}

#[cfg(test)]
mod tests_batch_l3 {
    //! **M2 批 L 第 ③ 步**：白名單頭與 TLS 四格。
    //!
    //! ★ 單開一個模組，是因為上面那一組是「一行長什麼樣」的判據，
    //! 而這一組多了一件事：**取值從哪兒來**（頭映射 / `SslDigest`）。

    use super::*;
    use std::time::Duration;

    fn rec() -> Record {
        let mut r = Record::new("HTTP/1.1");
        r.started = SystemTime::UNIX_EPOCH + Duration::from_millis(1_787_000_000_123);
        r.method = "GET".into();
        r.host = "a.example".into();
        r.uri = "/x".into();
        r
    }

    fn parse(r: &Record) -> serde_json::Value {
        let s = r.to_json_line(crate::OUTCOME_RESPOND, 200, 0, r.started);
        assert_eq!(s.matches('\n').count(), 1, "整条日志只许有行尾那一个换行");
        serde_json::from_str(s.trim_end()).expect("必须是合法 JSON")
    }

    fn pick(name: &str, prefix: &str) -> HeaderPick {
        HeaderPick {
            lookup: name.to_ascii_lowercase(),
            key: format!("{prefix}{}", name.to_ascii_lowercase().replace('-', "_")),
        }
    }

    #[test]
    fn tls_四格按契约出现() {
        let mut r = rec();
        r.tls = Some(TlsFields {
            version: "TLSv1.3".into(),
            cipher: "TLS_AES_256_GCM_SHA384".into(),
            sni: Some("a.example".into()),
            alpn: Some("h2".into()),
        });
        let v = parse(&r);
        assert_eq!(v["tls_version"], "TLSv1.3");
        assert_eq!(v["tls_cipher"], "TLS_AES_256_GCM_SHA384");
        assert_eq!(v["tls_sni"], "a.example");
        assert_eq!(v["tls_alpn"], "h2");
    }

    #[test]
    fn 不是_tls_的连接四格一个都不出现() {
        // ★ ★ 反證。⚠ 少了它，一條「恆寫 tls_version」的實作會讓上面那條全綠，
        //   而它在每一條明文請求上都說了一句假話。
        let v = parse(&rec());
        for k in ["tls_version", "tls_cipher", "tls_sni", "tls_alpn"] {
            assert!(v.as_object().unwrap().get(k).is_none(), "{k} 不该出现");
        }
    }

    #[test]
    fn 问不出来的那一格不出现_而不是给一个空串() {
        // ⚠ `tls_cipher=""` 在紀錄裡讀起來像「協商出了一個名字是空的套件」。
        //   ★ 上游 `SslDigest::from_ssl` 在拿不到 cipher 時給的正是空串 ——
        //     這一條守的是「那個空串不會原樣落進紀錄」。
        let mut r = rec();
        r.tls = Some(TlsFields {
            version: "TLSv1.3".into(),
            cipher: String::new(),
            sni: None,
            alpn: None,
        });
        let v = parse(&r);
        assert_eq!(v["tls_version"], "TLSv1.3");
        let o = v.as_object().unwrap();
        assert!(o.get("tls_cipher").is_none(), "空 cipher 不该出现");
        assert!(o.get("tls_sni").is_none(), "没发 SNI 时不该出现");
        assert!(o.get("tls_alpn").is_none(), "没协商 ALPN 时不该出现");
    }

    #[test]
    fn 白名单头按契约的键名出现() {
        let mut r = rec();
        r.req_headers = vec![("req_hdr_user_agent".into(), "curl/8".into())];
        r.resp_headers = vec![("resp_hdr_content_type".into(), "text/plain".into())];
        let v = parse(&r);
        assert_eq!(v["req_hdr_user_agent"], "curl/8");
        assert_eq!(v["resp_hdr_content_type"], "text/plain");
        // ★ ★ 仍然**扁平** —— G113 的全部意義。
        for (k, val) in v.as_object().unwrap() {
            assert!(
                !val.is_object() && !val.is_array(),
                "字段 `{k}` 不是标量 —— 契约要求扁平"
            );
        }
    }

    #[test]
    fn 多值头按逗号空格连接() {
        // RFC 9110 §5.3：同名欄位行等價於用逗號連接的一行。
        let mut m = http::HeaderMap::new();
        m.append("accept", "text/html".parse().unwrap());
        m.append("accept", "application/json".parse().unwrap());
        let got = collect(&[pick("Accept", "req_hdr_")], &m);
        assert_eq!(
            got,
            vec![(
                "req_hdr_accept".to_string(),
                "text/html, application/json".to_string()
            )]
        );
    }

    #[test]
    fn 白名单里有而这条请求上没有的头不出现() {
        // ⚠ 「在白名單裡」與「這條請求上真的有」是兩件事 —— 契約裡分開寫的那一句。
        let m = http::HeaderMap::new();
        assert!(collect(&[pick("User-Agent", "req_hdr_")], &m).is_empty());
    }

    #[test]
    fn 不在白名单里的头一个都不取() {
        // ★ ★ ★ 反證：預設**一個頭都不記**，而這條守的是「白名單真的在篩」。
        //   ⚠ 少了它，一條「把整個頭映射倒進紀錄」的實作在別的判據下全綠 ——
        //     而那正好是 G114 那半條理由要防的東西（憑證進紀錄）。
        let mut m = http::HeaderMap::new();
        m.insert("authorization", "Bearer 秘密".parse().unwrap());
        m.insert("user-agent", "curl/8".parse().unwrap());
        let got = collect(&[pick("User-Agent", "req_hdr_")], &m);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "req_hdr_user_agent");
    }

    #[test]
    fn 头值里的引号与换行被转义_而不是把那一行拆成两行() {
        // ⚠ ⚠ 頭值是**用戶端給的** —— 與 `uri` 那一格同一個威脅：一個帶引號或
        //   換行的頭值若不轉義就能**偽造出一整行紀錄**。
        let mut r = rec();
        r.req_headers = vec![("req_hdr_x_evil".into(), "a\"b\nc\\d".into())];
        let v = parse(&r);
        assert_eq!(v["req_hdr_x_evil"], "a\"b\nc\\d", "转义之后必须还原得回来");
    }

    #[test]
    fn 不是_utf8_的头值转成有损字符_而不是被当成不存在() {
        // ★ ★ 頭值是 opaque 位元組（RFC 9110 的 `obs-text` 允許 0x80–0xFF）。
        //   ⚠ 「轉不了就丟掉」在紀錄裡的樣子是**這個頭不存在** —— 那是一句假話。
        let mut m = http::HeaderMap::new();
        m.insert(
            "x-raw",
            http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        let got = collect(&[pick("X-Raw", "req_hdr_")], &m);
        assert_eq!(got.len(), 1, "这个头确实在这条请求上");
        assert!(!got[0].1.is_empty());
    }
}

#[cfg(test)]
mod tests_batch_m {
    //! **M2 批 M**：收尾這一處**同時**喂指標（裁決 R1 / R2 / R4）。
    //!
    //! ⚠ ⚠ 這一組走的是**真的**處理程序級指標註冊表 —— 於是每條判據用的 `site` 標籤值
    //! 都只此一處用過，否則同一個測試二進位裡幾條測試會把數寫進對方的斷言裡。

    use super::*;
    use std::time::Duration;

    fn rec(site_addr: &str) -> Record {
        let mut r = Record::new("HTTP/1.1");
        r.started = SystemTime::now();
        r.method = "GET".into();
        r.host = "a.example".into();
        r.uri = "/x".into();
        r.site_addr = Some(Arc::from(site_addr));
        r
    }

    /// 從處理程序級註冊表裡讀這一格的讀數；沒有這條 series 就是 0。
    fn counter(site: &str, outcome: &str, class: &str) -> u64 {
        let want = format!(
            "fulcrum_requests_total{{site=\"{site}\",outcome=\"{outcome}\",\
             status_class=\"{class}\",proto=\"HTTP/1.1\"}} "
        );
        let out = crate::metrics::render();
        out.lines()
            .find_map(|l| l.strip_prefix(&want))
            .map(|v| v.parse().expect("读数不是个数"))
            .unwrap_or(0)
    }

    fn lines(path: &str) -> usize {
        std::fs::read_to_string(path)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// 這條 histogram series 的 `_sum`（**秒**）；沒有這條 series 就是 0。
    fn hist_sum(site: &str, outcome: &str) -> f64 {
        let want = format!(
            "fulcrum_request_duration_seconds_sum{{site=\"{site}\",outcome=\"{outcome}\"}} "
        );
        let out = crate::metrics::render();
        out.lines()
            .find_map(|l| l.strip_prefix(&want))
            .map(|v| v.parse().expect("读数不是个数"))
            .unwrap_or(0.0)
    }

    /// 紀錄檔案最後一行裡的 `duration_ms`（**毫秒**）。
    fn last_duration_ms(path: &str) -> f64 {
        let s = std::fs::read_to_string(path).expect("日志文件要读得到");
        let line = s.lines().next_back().expect("至少要有一行");
        let v: serde_json::Value = serde_json::from_str(line).expect("必须是合法 JSON");
        v["duration_ms"].as_f64().expect("`duration_ms` 不是个数")
    }

    #[test]
    fn status_class_是六个值的闭集_而_0_取_none_r1() {
        // ★ R1 的**前提**：`status = 0` 可達，而且會被記進訪問紀錄。
        //   ⚠ 少了這一句，「0 該不該有一格」就成了一個憑空的選擇題。
        assert!(LogLevel::All.records(0));

        // ★ 含兩個端點：`0`（什麼都沒發生）與 `599`。
        assert_eq!(status_class(0), "none");
        assert_eq!(status_class(99), "none");
        assert_eq!(status_class(100), "1xx");
        assert_eq!(status_class(199), "1xx");
        assert_eq!(status_class(200), "2xx");
        assert_eq!(status_class(299), "2xx");
        assert_eq!(status_class(300), "3xx");
        assert_eq!(status_class(399), "3xx");
        assert_eq!(status_class(400), "4xx");
        assert_eq!(status_class(499), "4xx");
        assert_eq!(status_class(500), "5xx");
        assert_eq!(status_class(599), "5xx");
        // ⚠ 600 以上不是 HTTP 狀態碼 —— 歸 `none`，不許各開一格。
        assert_eq!(status_class(600), "none");
        assert_eq!(status_class(u16::MAX), "none");

        // ★ ★ 反向那半：整個 `u16` 值域上**只出現這 6 個標籤值**。
        //   ⚠ 上面每一條都只問「這個碼是不是那個類」，問不出「有沒有第 7 個類」——
        //     一個多出 `0xx` 的實作在它們那裡完全看不出來。
        let seen: std::collections::BTreeSet<&str> = (0..=u16::MAX).map(status_class).collect();
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            ["1xx", "2xx", "3xx", "4xx", "5xx", "none"]
        );
    }

    #[test]
    fn 站点没配_log_时指标照样加一_而日志一行都不写_r4() {
        // ★ ★ ★ R4 的正反兩半，而且**在同一個檔案上**對照 ——
        //   「檔案是空的」這一半單獨看什麼都不證明：夾具沒接上時它同樣是空的。
        let dir = std::env::temp_dir().join(format!("fulcrum-metrics-r4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r4.json").to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);
        open_file(&path).expect("测试用的日志文件要开得起来");
        let cfg = LogRt {
            output: LogOutput::File(path.clone()),
            level: LogLevel::All,
            req_headers: Vec::new(),
            resp_headers: Vec::new(),
        };

        // ① 沒配 `log`（`target` 是 `None`）：指標 +1，而檔案一行都不多。
        let a = rec("<ut-m-nolog>");
        assert!(a.target.is_none());
        a.finish(crate::OUTCOME_RESPOND, 200, 12);
        assert_eq!(lines(&path), 0, "没配 `log` 的站点不许写出任何一行");
        assert_eq!(
            counter("<ut-m-nolog>", "respond", "2xx"),
            1,
            "没配 `log` 的站点必须照样有指标 —— 那是 R4 取这一侧的全部理由"
        );

        // ② 配了 `log { level info }`：指標 +1，而且這一次**真的寫出去了一行**。
        //    ★ 這一半證明上面那個 0 不是夾具壞了。
        let mut b = rec("<ut-m-log>");
        b.target = Some(Arc::new(cfg.clone()));
        b.finish(crate::OUTCOME_RESPOND, 200, 12);
        assert_eq!(lines(&path), 1, "配了 `log` 就该有一行 —— 夹具是通的");
        assert_eq!(counter("<ut-m-log>", "respond", "2xx"), 1);

        // ③ 閾值擋住（`level error` 遇上 200）：紀錄仍然不寫，指標仍然 +1。
        //    ⚠ 這是 `emit` 的**第二個**早退分支，與 ① 那個是兩條不同的路。
        let mut c = rec("<ut-m-thresh>");
        c.target = Some(Arc::new(LogRt {
            level: LogLevel::Error,
            ..cfg.clone()
        }));
        c.finish(crate::OUTCOME_RESPOND, 200, 12);
        assert_eq!(lines(&path), 1, "阈值挡住的那一条不许被写出去");
        assert_eq!(counter("<ut-m-thresh>", "respond", "2xx"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 没匹配到站点的请求也进总数_site_取尖括号_none_r2() {
        // ★ R2：這樣 `fulcrum_requests_total` 才是**真的總數**，一致性門才有東西可對。
        //   ⚠ 尖括號形式在合法的位址字面值裡不可能出現 ⇒ 它撞不上真站台。
        let mut r = rec("<不该用到>");
        r.site_addr = None;
        let r_outcome = crate::OUTCOME_NO_SITE_MATCH;
        r.finish(r_outcome, 421, 0);
        assert_eq!(counter("<none>", "no_site_match", "4xx"), 1);
    }

    #[test]
    fn 耗时按秒记_而算不出来时按_0_记_与日志那一格逐字一致() {
        // 正向：已知的 250ms ⇒ `_sum` 是 0.25 秒（**秒**，不是毫秒）。
        let mut r = rec("<ut-m-dur>");
        r.started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        r.meter(
            crate::OUTCOME_RESPOND,
            200,
            r.started + Duration::from_millis(250),
        );
        let out = crate::metrics::render();
        assert!(
            out.contains(
                "fulcrum_request_duration_seconds_sum{site=\"<ut-m-dur>\",outcome=\"respond\"} 0.25\n"
            ),
            "{out}"
        );

        // ★ ★ 反向：`now` 早於 `started`（時鐘回撥）⇒ `duration_since` 失敗 ⇒ 按 **0** 記。
        //   ⚠ 這一條同時釘住「兩處對同一件事給同一個答案」：`to_json_line` 的
        //     `duration_ms` 在同一份輸入上也必須是 0。
        let mut b = rec("<ut-m-back>");
        b.started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let 回拨 = b.started - Duration::from_secs(5);
        b.meter(crate::OUTCOME_RESPOND, 200, 回拨);
        let out = crate::metrics::render();
        assert!(
            out.contains(
                "fulcrum_request_duration_seconds_sum{site=\"<ut-m-back>\",outcome=\"respond\"} 0\n"
            ),
            "{out}"
        );
        let v: serde_json::Value = serde_json::from_str(
            b.to_json_line(crate::OUTCOME_RESPOND, 200, 0, 回拨)
                .trim_end(),
        )
        .unwrap();
        assert_eq!(v["duration_ms"], 0.0, "日志那一格与指标必须给同一个答案");
    }

    #[test]
    fn 标签取的是命中的地址字面量与那条请求自己的协议() {
        // ⚠ `site` 取 `site_addr`（G121）而**不是** `site`（那是站台的名字，R3），
        //   `proto` 取 `Record` 入口填的那個 —— 兩者都不是推斷出來的。
        let mut r = Record::new("HTTP/3.0");
        r.started = SystemTime::now();
        let r_outcome = crate::OUTCOME_FILE_SERVER;
        r.site = Some("http://first.example:9900".into());
        r.site_addr = Some(Arc::from("<ut-m-h3>"));
        r.finish(r_outcome, 503, 0);
        let out = crate::metrics::render();
        assert!(
            out.contains(
                "fulcrum_requests_total{site=\"<ut-m-h3>\",outcome=\"file_server\",\
                 status_class=\"5xx\",proto=\"HTTP/3.0\"} 1\n"
            ),
            "{out}"
        );
        // ★ 反向：站台的名字**不該**出現在指標裡 —— 它是訪問紀錄那一側的東西。
        assert!(
            !out.contains("first.example"),
            "站点名字漏进了指标标签：{out}"
        );
    }

    #[test]
    fn 指标的耗时与日志的_duration_ms_来自同一次时钟读数() {
        // ★ ★ ★ 「處置一致」不等於「答案一致」。`duration_since` 失敗按 0 記那一條
        //   兩邊逐字相同（上一條判據釘的就是它），可只要**輸入**是兩次不同的
        //   `SystemTime::now()`，同一條請求上就會有兩個耗時 ——
        //   而兩邊的註解都寫著「它們說的是同一件事」。
        //   ⇒ 這一條釘的是**輸入**：兩邊拿到的是同一個讀數。
        let dir =
            std::env::temp_dir().join(format!("fulcrum-metrics-clock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clock.json").to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);
        open_file(&path).expect("测试用的日志文件要开得起来");

        let mut r = rec("<ut-m-clock>");
        // ⚠ ⚠ `started` 有意釘在 1970 年代：**任何一側若偷偷去讀真即時鐘**，
        //   它算出來的就是十幾億秒而不是 0.25 秒 —— 這條判據因此按**九個數量級**紅，
        //   而不是按微秒紅。★ 那正是「反證要紅得起來」的意思：一條只在理論上
        //   成立、實際上永遠紅不起來的斷言，比沒有斷言更壞。
        r.started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        r.target = Some(Arc::new(LogRt {
            output: LogOutput::File(path.clone()),
            level: LogLevel::All,
            req_headers: Vec::new(),
            resp_headers: Vec::new(),
        }));

        let 收尾时刻 = r.started + Duration::from_millis(250);
        r.finish_at(crate::OUTCOME_RESPOND, 200, 12, 收尾时刻);

        let 秒 = hist_sum("<ut-m-clock>", "respond");
        let 毫秒 = last_duration_ms(&path);
        // ★ ★ 本判據真正要釘的那一條**排在最前面**：兩邊換算後逐位對得上。
        //   ⚠ 排在下面兩條之後的話，注入缺陷時先紅的是它們，而這一條究竟判不判得動
        //     就從來沒有被驗過 —— 一條被別的斷言擋在後面的斷言，與不存在沒有區別。
        assert!(
            (秒 * 1000.0 - 毫秒).abs() < 1e-9,
            "指标说 {秒} 秒、日志说 {毫秒} 毫秒 —— 它们不是同一个读数"
        );
        // ★ 再釘住那個讀數**就是喂進來的那一個**（否則「兩邊一致地錯」也能通過上面那條）。
        assert_eq!(秒, 0.25, "指标那一格没用喂进来的那个收尾时刻");
        assert_eq!(毫秒, 250.0, "日志那一格没用喂进来的那个收尾时刻");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
