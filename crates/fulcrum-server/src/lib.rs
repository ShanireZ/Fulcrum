//! 資料面：把[執行環境物件圖](fulcrum_runtime::Runtime)掛到 Pingora 上。
//!
//! ```text
//! 结构化配置 ──▶ Runtime（纯逻辑，决定「该做什么」）
//!                   │
//!                   ▼
//!            本 crate（用 Pingora 把它做出来）
//! ```
//!
//! # 模組地圖
//!
//! | 模組 | 管什麼 |
//! |---|---|
//! | 本檔案 | h1/h2 入口、執行鏈、路由、裝載摘要 |
//! | [`l4`] | L4 面：TCP/UDP 透傳、SNI/ALPN 分流、PROXY protocol |
//! | [`files`] | 靜態檔案 |
//! | [`cache`] | HTTP 快取（語義層 + 記憶體/磁碟後端 + 防驚群） |
//! | [`encode`] | 回應壓縮與預壓縮旁檔案 |
//! | [`quic`] | HTTP/3 入口與換代時的跨處理程序連接轉交 |
//! | [`access_log`] | 結構化訪問紀錄 |
//! | [`metrics`] | Prometheus 指標：註冊表 + text exposition 繪製 |
//! | [`acme`] · [`health`] · [`dns`] | 自動 HTTPS · 主動健康檢查 · 上游網域重解析 |
//! | [`admin`] · [`process`] | 管理面 · 處理程序託管（`sd_notify` / pid 檔案 / `SIGUSR2` 換代） |
//!
//! ⚠ **哪些能力認得但還沒串接，以 [`fulcrum_runtime::UNWIRED`] 為準** ——
//! 這裡不復述一份會爛掉的清單。
//!
//! # ⛔ 回落層已整層刪除（G98）
//!
//! 寫了 `fallback_nginx` / `fallback_caddy` 的設定會編譯不過，並給一條專門的診斷
//! （`compile.rs` 的 `removed_global_option`）。★ 它留下的兩句話在別處照樣成立：
//! ① **501 與 502 是兩個不同的事實，不許合併**（設定遺漏 vs 後端故障）；
//! ② **一個靜靜躺著的死設定，與一個正在生效的設定，在設定檔案裡長得一模一樣。**
//!
//! # ★ 用戶端 IP 只取 socket 對端，絕不取 XFF
//!
//! `X-Forwarded-For` 的最左項**用戶端可以隨便寫** —— 拿它喂 `remote_ip` 匹配器，
//! 等於讓任何人自稱來自 `10.0.0.0/8`。⚠ 將來支援「信任前置代理」時，
//! 判據是**最近一跳**，且必須顯式設定。

pub mod access_log;
pub mod acme;
pub mod admin;
pub mod cache;
/// 連接計數的登記處（**M2 批 O**，G122 的連接那半）。
pub mod conn_stats;
pub mod dns;
/// 回應壓縮（M2 批 I）：`encode` 串接，用的是 fork 裡那份壓縮模組（G100）。
pub mod encode;
pub mod files;
pub mod health;
/// keep-alive 空閒視窗：每條連接一個計時器、跨請求帶著走（G160）。
mod keepalive;
/// L4 面（M2 批 A：TCP，批 B：UDP）：自建監聽器 + socket 移交。
pub mod l4;
/// Prometheus 指標（M2 批 M）：處理程序級註冊表 + text exposition 的自研繪製器（G117）。
pub mod metrics;
/// `outcome` 閉集與只能由它建構取值的類型。★ 單獨成模組是判據本身，見模組文件。
pub mod outcome;
pub mod process;
mod proxyproto;
/// HTTP/3 入口（M2 批 J）：QUIC 傳輸層 + `quiche::h3`。
pub mod quic;
pub mod tls;

// ★ 重匯出，保住 `crate::OUTCOME_*` / `crate::OUTCOMES` 這些既有路徑（上千處引用按號走同一形狀）。
pub use outcome::{
    OUTCOME_ACME_HTTP01, OUTCOME_ERROR, OUTCOME_FILE_SERVER, OUTCOME_METRICS,
    OUTCOME_NO_SITE_MATCH, OUTCOME_REDIR, OUTCOME_RESPOND, OUTCOME_REVERSE_PROXY,
};
pub use outcome::{OUTCOMES, OutcomeName};

use async_trait::async_trait;
use bytes::Bytes;
use fulcrum_acme::Http01Store;
use fulcrum_runtime::request::{Headers, RequestCtx, ResponseCtx};
use fulcrum_runtime::template::Template;
use fulcrum_runtime::{
    CacheRt, HeaderOpRt, Outcome, ProxyTarget, Routed, Runtime, SiteRt, Upstream,
};
use http::HeaderValue;
use log::{debug, error, info, warn};
use pingora_core::apps::{HttpPersistentSettings, HttpServerApp, ReusedHttpStream};
use pingora_core::connectors::ConnectorOptions;
use pingora_core::connectors::http::Connector;
use pingora_core::listeners::{L4BufferSettings, ListenerConfig};
use pingora_core::protocols::http::ServerSession;
use pingora_core::server::ShutdownWatch;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::SystemTime;

/// keep-alive 空閒視窗：從開始等下一條請求到它的請求頭讀完，**合計**這麼久（G160，見 [`keepalive`]）。
/// ★ 停機時會被改成 `None`（不續），讓連接自然收斂。
const KEEPALIVE_SECS: u64 = 60;

/// L7 資料面監聽（明文與 TLS）給接入連接的緩衝：**寫 16 KiB**，讀仍用 pingora 的預設（64 KiB）。
///
/// ★ pingora 預設的寫緩衝只有 1460 位元組，而 h1 寫回應是先把頭 `write_all` 進緩衝、再寫正文
///   ⇒ 「緩衝裡的 + 這次的」一超過容量，頭就先被衝出去、正文再寫穿
///   ⇒ 頭 + 約 4 KiB 正文的回應是**兩次 send、兩個 TCP 段**（nginx / haproxy 是一次）。
///   16 KiB 讓**帶 `Content-Length`** 且頭 + 正文不超過約 16 KiB 的回應一次發出；更大的仍然先衝頭。
///   ⚠ 沒有 `Content-Length` 的回應（chunked、`encode` 壓縮後、讀到關閉）寫完頭就 flush，
///   緩衝多大都還是 ≥ 2 次 send（`G155`）。
/// ⚠ 代價：寫緩衝在建連時整塊分配 ⇒ 每條連接多約 15 KiB；分幾段寫的 `Content-Length` 正文
///   攢滿約 16 KiB 或寫完最後一段才發，**回應頭也一起等**（帶 CL 的頭本來就不 flush）
///   ⇒ 反代一個先發頭和不到 16 KiB 正文、然後停住的上游時，用戶端在停頓結束前**連狀態行都
///   收不到，而且沒有時間上限**（改前的門檻約 1.4 KiB）。分幾段寫 CL 正文的只有反代分支
///   （含快取未命中回源）；chunked 與讀到關閉的正文每寫必 flush，不受影響。
/// ★ 守它的是 `tests/serve/run.sh` 第 10 節：用戶端讀 `TCP_INFO`，數帶資料的段。
const L7_L4_BUFFER: L4BufferSettings = L4BufferSettings {
    read: None,
    write: Some(16 * 1024),
};

/// `&RequestHeader` 的 [`Headers`] 適配器。零拷貝。
struct ReqHeaders<'a>(&'a RequestHeader);

impl Headers for ReqHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        // `http::HeaderMap` 的查找本身就是大小寫不敏感的。
        self.0.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// `&ResponseHeader` 的適配器，給 `{resp_header.*}` 用。
struct RespHeaders<'a>(&'a ResponseHeader);

impl Headers for RespHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// 在飛連接計數的 RAII 守衛。
///
/// ★ ★ **為什麼必須是守衛而不是手寫一對呼叫**：轉發路徑上每一步都可能 `?` 提前回傳，
/// 而漏掉一次 `release` 會讓 `least_conn` 的計數**單調漂移**，最終永遠不選這個上游——
/// **而這件事不會有任何回報錯誤**。把配對交給 `Drop`，就沒有「忘了」這個選項。
struct Inflight<'a>(&'a Upstream);

impl<'a> Inflight<'a> {
    fn acquire(u: &'a Upstream) -> Inflight<'a> {
        u.acquire();
        Inflight(u)
    }
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// 下游回應的**唯一出口**（**M2 批 J 第六步**，G110 的落點）。
///
/// # 為什麼是一個類型，而不是「記得在四個地方各加一行」
///
/// G110 要求 h1/h2 的**每一條**回應都發 `Alt-Svc`，而寫回應頭的地方有四處，
/// 第五處隨時會出現。⇒ 換成這個類型：它 `Deref` 到 `ServerSession`，
/// 而 [`Downstream::write_response_header`] 是**內在方法**
/// ⇒ **內在方法優先於 `Deref` 過去的同名方法**，既有呼叫點一個字都不用改。
///
/// ⚠ 同名是**有意的**：換個名字等於要求下一個人記得用新名字。
/// 第二道保險是 `crates/fulcrum/tests/response_gate.rs`，它釘住
/// 「`.write_response_header(` 只許出現在這一處實作裡」。
///
/// ⚠ **h1/h2 用 [`Downstream::advertising`]，h3 用 [`Downstream::plain`]** ——
/// 已經在 h3 上的用戶端不需要被告知有 h3。★ 這個差別落在兩個呼叫點上，
/// 而不是落在一處「這條連接是不是 h3」的推斷上。
pub(crate) struct Downstream<'a> {
    session: &'a mut ServerSession,
    /// 要發的 `Alt-Svc` 值；`None` = 不發。
    alt_svc: Option<&'a str>,
    /// 這一次請求在訪問紀錄裡的樣子，**逐段填**（M2 批 L 第 ② 步）。
    pub(crate) record: access_log::Record,
}

impl<'a> Downstream<'a> {
    /// **h1/h2 入口**。
    ///
    /// ⚠ ⚠ 從 `advertising` 改名成 `h1h2`，而改名是有理由的：
    /// 這個建構函式現在決定**兩件**事 —— 發不發 `Alt-Svc`（G110），
    /// 以及訪問紀錄裡的 `proto`。★ 兩件事按**同一個岔口**分開
    /// （h1/h2 入口 vs h3 入口），所以名字應當叫那個岔口，
    /// 而不是叫其中一件事的效果 —— 否則下一個人會以為另一件事是「順帶的」。
    pub(crate) fn h1h2(session: &'a mut ServerSession, alt_svc: Option<&'a str>) -> Downstream<'a> {
        // ★ h1 與 h2 的區別問 session 自己 —— 那是它**知道**的事，不是推斷。
        // ★ 用常數而不是字面值：`access_log::PROTOS` 是基數表那道門算上界用的閉集，
        //   而它必須與**產生這些值的這一處**是同一批名字，⛔ 不能是一份抄件。
        let proto = if session.is_http2() {
            access_log::PROTO_H2
        } else {
            access_log::PROTO_H1
        };
        Downstream {
            session,
            alt_svc,
            record: access_log::Record::new(proto),
        }
    }

    /// **h3 入口**：不發 `Alt-Svc`（用戶端已經在 h3 上了），`proto` 恆為 `HTTP/3.0`。
    pub(crate) fn h3(session: &'a mut ServerSession) -> Downstream<'a> {
        Downstream {
            session,
            alt_svc: None,
            record: access_log::Record::new(access_log::PROTO_H3),
        }
    }

    /// ★ ★ **跑一次請求，然後一定收尾一次** —— 兩件事綁在同一個方法裡，
    /// 於是「每一條請求都記一行、都記一次數」不是一件要靠記性的事，
    /// 加新入口時這裡不用改。
    ///
    /// ⚠ 收尾走 [`access_log::Record::finish`] 而不是 `emit`：**指標不跟著
    /// `log` 設定早退**（沒配紀錄的站台照樣要有指標），理由寫在 `finish` 上。
    ///
    /// ⚠ `status` 與 `resp_size` **現問 `ServerSession`**，不在 `Record` 裡另存一份 ——
    /// 兩份遲早不一致，而不一致的那天沒有任何東西會說。
    /// ★ ★ **`outcome` 從此走同一條規矩**：它是 [`FulcrumApp::serve_one`] 的**回傳值**，
    /// 不是 `Record` 上的一個欄位 ⇒ 「某條回傳路徑忘了寫它」不再是一個靜默缺陷，
    /// 而是一個編譯錯誤（那條路徑求不出 [`OutcomeName`] 就回傳不了）。
    ///
    /// ★ 沒寫出任何回應頭時 `status` 是 **0**：那不是「未知」，是「什麼都沒發生」。
    /// ⚠ ⚠ 此時 `outcome` **仍是執行鏈給的那個值** —— ⛔ 不是什麼「aborted」：
    /// 那個預設值連同它的欄位一起沒有了。判據在 `tests/metrics/run.sh`
    /// 「而 outcome 仍是執行鏈給的那個值」那一條。
    pub(crate) async fn serve(&mut self, app: &FulcrumApp) {
        let outcome = app.serve_one(self).await;
        let status = self
            .session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        let size = self.session.body_bytes_sent();
        // ── 白名單頭與 TLS 資訊（**M2 批 L 第 ③ 步**）──────────────────────
        //
        // ★ ★ 三樣都在**這裡**問工作階段，而不是在執行鏈中途各記一份 ——
        //   與 `status` / `resp_size` 逐字同一條理由。
        // ⚠ 而它們**只能**在 `serve_one` 之後問，各有各的原因：
        //   ① 白名單是**路由之後**才知道的（`log` 是站台塊內的指令 ⇒ `target` 那時才有）；
        //   ② 回應頭要等回應真的寫出去；
        //   ③ TLS 那份 digest 從連接建起來就有，放這裡只是為了三件事在一處。
        if let Some(cfg) = self.record.target.clone() {
            if !cfg.req_headers.is_empty() {
                let picked =
                    access_log::collect(&cfg.req_headers, &self.session.req_header().headers);
                self.record.req_headers = picked;
            }
            if !cfg.resp_headers.is_empty()
                && let Some(resp) = self.session.response_written()
            {
                let picked = access_log::collect(&cfg.resp_headers, &resp.headers);
                self.record.resp_headers = picked;
            }
            self.record.tls = tls_fields(self.session);
        }
        self.record.finish(outcome, status, size);
    }

    /// 寫下游回應頭 —— **本 crate 資料面唯一的一處**。
    ///
    /// ⚠ 與 `ServerSession::write_response_header` **同名是有意的**，見類型文件。
    ///
    /// ★ 用 `insert_header` 而不是 `append_header`：一條回應上出現兩個 `Alt-Svc`
    /// 不是錯誤（RFC 7838 允許），但**上游若已經發了一個，那是它的 h3、不是我們的**
    /// —— 我們是終結這條 TLS 連接的人，連接埠的話語權在我們這裡。
    pub(crate) async fn write_response_header(
        &mut self,
        mut resp: Box<ResponseHeader>,
    ) -> pingora_core::Result<()> {
        if let Some(v) = self.alt_svc {
            // ⚠ 失敗只可能是頭值非法，而這個值是我們自己拼的常數形狀 ⇒ 不該發生；
            //   真發生了也不能把整條回應搭進去。
            if let Err(e) = resp.insert_header("Alt-Svc", v) {
                warn!("Alt-Svc 装不进响应头（{v}）：{e}");
            }
        }
        self.session.write_response_header(resp).await
    }
}

impl std::ops::Deref for Downstream<'_> {
    type Target = ServerSession;
    fn deref(&self) -> &ServerSession {
        self.session
    }
}

impl std::ops::DerefMut for Downstream<'_> {
    fn deref_mut(&mut self) -> &mut ServerSession {
        self.session
    }
}

/// 一次請求在路由與寫回應之間反覆要用的幾個事實 —— 都不借請求頭（**HTTP 層診斷發現 2 · 請求頭不整份克隆**）。
///
/// ★ 頭視圖**不在裡面**：同一次請求要對著不同的頭視圖各建一個 [`RequestCtx`] ——
///   借 `session` 裡那一份（`respond` 一類、`file_server`），或反代自己克隆的那一份。
///   ⚠ 反過來「先建一個帶空頭的 ctx 再改 `headers`」是個陷阱：那個空頭 ctx 一旦被直接拿去展開，
///   `{header.X}` 就靜靜地變成空串 —— 正是 `tests/serve/run.sh` 第 11 節守的那種迴歸。
#[derive(Clone, Copy)]
struct ReqFacts<'a> {
    host: &'a str,
    port: u16,
    scheme: &'static str,
    method: &'a str,
    path: &'a str,
    query: &'a str,
    remote_ip: Option<IpAddr>,
    remote_port: u16,
}

impl<'a> ReqFacts<'a> {
    fn ctx(self, headers: &'a (dyn Headers + Sync)) -> RequestCtx<'a> {
        RequestCtx {
            host: self.host,
            port: self.port,
            scheme: self.scheme,
            method: self.method,
            path: self.path,
            query: self.query,
            headers,
            remote_ip: self.remote_ip,
            remote_port: self.remote_port,
        }
    }
}

/// 一次請求的唯讀視圖，打包傳給轉發路徑。
///
/// ★ clippy 的 `too_many_arguments` 在這裡說得對：八個參數裡有四個是
/// **同一次請求的不同側面**，本來就該是一個東西。把它們捆起來之後，
/// 轉發函式的簽名也讀得出「它需要請求的哪幾個面」。
#[derive(Clone, Copy)]
struct ReqView<'a> {
    ctx: &'a RequestCtx<'a>,
    downstream_req: &'a RequestHeader,
    effective_path: &'a str,
    started: SystemTime,
}

/// 掛在一個監聽連接埠上的樞衡應用。
///
/// ⚠ **每個連接埠一個執行個體**：站台索引是按 `(Host, 本地端口)` 兩維查的，
/// 而 Pingora 的 `ServerSession` 拿不到「這條連接落在哪個監聽器上」。
/// 把連接埠寫進 app 裡，是唯一不靠猜的辦法。
pub struct FulcrumApp {
    /// ★ **可在執行期整體換掉**（G8 的全量原子 load）。
    /// ⚠ 每個請求只在入口取一次快照——理由見 `SharedRuntime` 的類型文件：
    /// 一次發生在請求中途的 load 會讓同一個請求按舊設定路由、按新設定轉發，
    /// 那正是 G8 明令禁止的「部分生效」。
    rt: Arc<fulcrum_runtime::SharedRuntime>,
    port: u16,
    /// 這個監聽連接埠是不是 TLS。★ 它決定 `{scheme}` 展開成什麼，
    /// 也決定 `redir` 裡的 `https://{host}` 是否自洽。
    https: bool,
    connector: Connector,
    /// ACME 的 HTTP-01 應答表。★ 沒開自動簽發時它是空的，而**空表什麼都不接**——
    /// 於是「有沒有 ACME」不改變資料面的任何一條行為。
    http01: Arc<Http01Store>,
    /// 自研 HTTP 快取（M2 批 G）。
    ///
    /// ⚠ ⚠ **它掛在 app 上而不是在 `Runtime` 裡**，這一點是有意的：
    /// G8 的全量原子 load 會把整棵執行環境圖換掉，而**快取內容不該跟著沒** ——
    /// 每 reload 一次就清空快取，等於每次改設定都讓上游挨一次滿負荷。
    /// ★ 於是「換設定」與「快取裡有什麼」是兩件獨立的事，正如它們本來就該是。
    cache: Arc<cache::CacheHandle>,
    /// 這個連接埠上要對 h1/h2 回應發的 `Alt-Svc`（**G110**）；`None` = 不發。
    ///
    /// ★ 它是**串接那一步**算好的常數串，不是每條回應現拼：值只取決於
    /// 「這個監聽連接埠開沒開 h3」，而連接埠集變了只能走真的換代。
    /// ⇒ 資料面這一路上沒有一處需要再判「h3 開了嗎」。
    ///
    /// ⚠ **不發它，瀏覽器永遠不會主動嘗試 h3** —— h3 入口對真實使用者等於不存在。
    alt_svc: Option<String>,
}

impl FulcrumApp {
    pub fn new(
        rt: Arc<fulcrum_runtime::SharedRuntime>,
        port: u16,
        https: bool,
        http01: Arc<Http01Store>,
        cache: Arc<cache::CacheHandle>,
        alt_svc: Option<String>,
        upstream_keepalive_pool: usize,
    ) -> FulcrumApp {
        FulcrumApp {
            rt,
            port,
            https,
            // ★ pingora 0.9.0 把上游空閒連接池從「每個執行緒一個 LRU」改成「整處理程序一個分片的全域 LRU」，
            //   `Connector::new(None)` 的預設 128 從此是**整處理程序**的上限（0.8.1 時是每執行緒 128）。
            //   上游自己在 `ConnectorOptions::from_server_conf` 裡乘了執行緒數來保持原意 ⇒ 這裡照做：
            //   容量由呼叫方按「每執行緒池大小 × L7 執行緒數」給（owner 2026-09-24 拍板保持 0.8.1 的總量）。
            connector: Connector::new(Some(ConnectorOptions::new(upstream_keepalive_pool))),
            http01,
            cache,
            alt_svc,
        }
    }
}

/// h1/h2 那一側交給 Pingora 的入口 —— 只是把 [`FulcrumApp`] 包了一層。
///
/// # 它存在的唯一理由：讓兩個入口共用**同一個** app 執行個體
///
/// h3 入口要 `Arc<FulcrumApp>`，而 pingora 的 `Service::new` 收的是 `A` 不是 `Arc<A>`。
/// ⇒ 不套這一層就得建**兩個** `FulcrumApp`，也就是**兩個上游連接池**
/// （`Connector` 每執行個體一個）—— ⚠ 這件事不會有任何回報錯誤，只表現為上游連接數翻倍。
/// ★ 更貴的是「兩個執行個體必須建構得一模一樣」又是一條靠人記著的約束。
///
/// ⚠ 孤兒規則擋住了更直接的寫法（`impl HttpServerApp for Arc<FulcrumApp>` 不合法），
/// 所以必須是一個本機 newtype。
pub struct H1Entry(Arc<FulcrumApp>);

impl H1Entry {
    pub fn new(app: Arc<FulcrumApp>) -> H1Entry {
        H1Entry(app)
    }
}

#[async_trait]
impl HttpServerApp for H1Entry {
    async fn process_new_http(
        self: &Arc<Self>,
        session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        // ★ 調固有方法、⛔ 不再經 `HttpServerApp` 轉一手（2026-10-15 · respond 剩的分配）：
        //   `#[async_trait]` 每過一層就把那一層的 future 裝一次箱 —— pingora 調到這裡已經裝過一次，
        //   再經 trait 調 `FulcrumApp` 等於每條請求多一次分配。判據：`tests/respond_alloc.rs`。
        self.0.serve_h1(session, shutdown).await
    }
}

/// h3 那一側的入口 —— **同一個 [`FulcrumApp`]、同一條執行鏈**。
///
/// ★ 相依方向：`quic` 模組只認 [`quic::h3_conn::H3RequestHandler`] 這個 trait，
/// **不認識** `FulcrumApp` ⇒ HTTP/3 入口不會反向相依資料面本體。
///
/// ⚠ 與 h1/h2 的**唯一**差別是 [`Downstream::plain`]（已經在 h3 上的用戶端
/// 不需要再被告知有 h3），而它落在**這一行**上。
#[async_trait]
impl quic::h3_conn::H3RequestHandler for FulcrumApp {
    async fn handle(&self, mut session: ServerSession) {
        // ⚠ 請求頭由 h3 那一層在建 `H3Session` 時就填好了 —— 這裡**不能**再
        //   `read_request()`：h1 那側它是「從 socket 上讀一個請求頭」，
        //   而 h3 上沒有那個動作。
        {
            let mut out = Downstream::h3(&mut session);
            out.serve(self).await;
        }
        // ★ h3 上 `finish()` 收的是「這條流完了」，不是「這條連接完了」——
        //   一條 QUIC 連接上還會有別的流。
        if let Err(e) = session.finish().await {
            debug!("h3 收尾失败：{e}");
        }
    }
}

/// `keepalive` 只拿得到這幾樣（G160）；⛔ 不含寫回應 —— 回應只從 [`Downstream`] 出（G110）。
impl keepalive::Conn for ServerSession {
    fn keepalive(&self) -> Option<u64> {
        self.get_keepalive()
    }
    fn set_keepalive(&mut self, secs: Option<u64>) {
        ServerSession::set_keepalive(self, secs)
    }
    fn take_carried(&mut self) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.take_connection_user_context()
    }
    fn set_preserve_header_case(&mut self, preserve: bool) {
        ServerSession::set_preserve_header_case(self, preserve)
    }
    fn read_request(
        &mut self,
    ) -> impl std::future::Future<Output = pingora_core::Result<bool>> + Send {
        ServerSession::read_request(self)
    }
}

impl FulcrumApp {
    /// h1/h2 的一條請求：讀請求頭 → 執行鏈 → 收尾，把連接交回 pingora。只經 [`H1Entry`] 進來。
    ///
    /// ★ 是固有方法、⛔ 不是 `HttpServerApp` 的實作：那樣 [`H1Entry`] 每轉一手就多裝一次箱（見那裡）。
    /// ★ pub 只為 `tests/respond_alloc.rs` 拿它當對照，數「入口那一層多幾次分配」。
    pub async fn serve_h1(
        &self,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        // ★ keep-alive 的空閒計時由 `keepalive` 接管（G160）：每條連接一個計時器，跨請求帶著走。
        // ★ 請求頭名的原樣大小寫只有原樣轉發給上游時才用得到 ⇒ 連接埠上沒有反代站台就不保留（G163）。
        //   讀請求頭之前還不知道 Host ⇒ 只能按連接埠；查的是點陣圖，⛔ 不為它再拿一次執行環境的讀鎖。
        let preserve_case = self.rt.port_has_proxy(self.port);
        let (read, idle_timer) = keepalive::read_request(&mut session, preserve_case).await;
        match read {
            Ok(true) => {}
            Ok(false) => {
                debug!("读不到请求头");
                return None;
            }
            Err(e) => {
                debug!("下游读失败：{e}");
                return None;
            }
        }

        // ★ 續不續、續多久（G161）：停機視窗內不續（讓連接自己收斂，而不是等被砍斷）；
        //   pingora 判了「不續」的也不續 —— 樞衡只管時長。
        keepalive::settle_after_read(&mut session, *shutdown.borrow(), KEEPALIVE_SECS);

        // ★ ★ `Alt-Svc` 在這裡接上（**G110**）：h1/h2 的每一條回應都帶它，
        //   而「每一條」是由 `Downstream` 這個類型保證的，不是由記性保證的。
        {
            let mut out = Downstream::h1h2(&mut session, self.alt_svc.as_deref());
            out.serve(self).await;
        }

        let mut persistent = HttpPersistentSettings::for_session(&session);
        keepalive::carry(&mut persistent, idle_timer);
        match session.finish().await {
            // ★ pingora 0.9.0：`finish()` 回傳 `ReusableHttpStream`（帶著 pipelining 預讀到的位元組）⇒ 照上游
            //   `apps/http_app.rs` 用 `from_reusable_stream`；⛔ 別寫 `into_parts().0`，那會把預讀的位元組丟掉。
            Ok(c) => c.map(|s| ReusedHttpStream::from_reusable_stream(s, persistent)),
            Err(e) => {
                debug!("收尾失败：{e}");
                None
            }
        }
    }
}

impl FulcrumApp {
    /// 一次請求的「事實」，**全部借自 `hdr`**（HTTP 層診斷 10-05 · 請求路徑上的字串分配）。
    ///
    /// ★ 單獨一個函式，是因為同一次請求要對著**不同的那份請求頭**各取一次：
    ///   `serve_one` 開頭借 `session` 裡那份；反代借它自己克隆的那份；`file_server` 出錯後重新借一次 ——
    ///   後兩處都要跨過「寫回應」（那要 `&mut session`），不能沿用開頭那份借用。
    fn facts_of<'a>(
        &self,
        hdr: &'a RequestHeader,
        remote_ip: Option<IpAddr>,
        remote_port: u16,
    ) -> ReqFacts<'a> {
        ReqFacts {
            // Host 裡可能帶連接埠（`a.com:8443`）——站台索引只認主機名。
            host: host_of(hdr).split(':').next().unwrap_or(""),
            port: self.port,
            scheme: if self.https { "https" } else { "http" },
            method: hdr.method.as_str(),
            path: hdr.uri.path(),
            query: hdr.uri.query().unwrap_or(""),
            remote_ip,
            remote_port,
        }
    }

    pub(crate) async fn serve_one(&self, session: &mut Downstream<'_>) -> OutcomeName {
        let started = SystemTime::now();
        // ★ ★ **整次請求的設定快照，只在這裡取一次。**
        //   之後所有階段用的都是 `rt` 這一份，而不是再去問 `self.rt`。
        //   ⚠ 每階段各取一次的話，一次發生在請求中途的全量 load 會讓**同一個請求**
        //   按舊設定路由、按新設定轉發——G8 明令禁止的「部分生效」。
        let rt = self.rt.current();
        // ── 1. 請求頭：**借**，⛔ 不整份克隆（HTTP 層診斷發現 2 · 請求頭不整份克隆，2026-09-26）─────────
        //
        // ★ 按**欄位**借（`session.session`），⛔ 不經 `Deref`：借的只是那一個欄位，
        //   於是下面照樣能寫 `session.record`。
        // ⚠ 寫回應要 `&mut session`，而請求頭就在 `session` 裡 ⇒ 每條分支都**先算後寫**：
        //   借用期內把要寫的東西算完（`Prepared` / `ErrorPage` / `FileReq`），放掉借用再寫。
        //   ★ 反代是唯一的例外：它邊寫邊讀請求頭（`header_down` 的 `{header.X}` 要等上游回來才展開），
        //   所以它在自己那一支裡克隆一份 —— 就是這裡原來那一次，挪過去了。
        let hdr: &RequestHeader = session.session.req_header();
        // ⚠ 這一行照舊經 `Deref` 借整個 `session`：它是**共享**借用、當場就還，與上面按欄位借的 `hdr` 共存得了。
        let (remote_ip, remote_port) = client_addr(session);
        // ★ host / path / query / method 四樣都**借**請求頭、⛔ 不 `to_string`（HTTP 層診斷 10-05）。
        //   ⚠ 代價是它們的借用不能跨過「寫回應」：`respond` 一類本來就先算後寫；
        //   要跨過去的兩支（`file_server` 出錯後算錯誤頁、反代）各自重新借（見 [`Self::facts_of`]）。
        let facts = self.facts_of(hdr, remote_ip, remote_port);
        let ReqFacts {
            host,
            path,
            query,
            method,
            ..
        } = facts;
        let headers = ReqHeaders(hdr);
        let ctx = facts.ctx(&headers);

        // ── 訪問紀錄：請求那一半里每請求都要的兩格（**M2 批 L 第 ② 步**）──────
        //
        // ★ method / host / uri / site 四格**路由之後、站台配了 `log` 才填**（見下面「站台那一半」）。
        session.record.remote_ip = remote_ip;
        session.record.remote_port = remote_port;

        // ── 1.5 ACME 的 HTTP-01 挑戰（RFC 8555 §8.3）────────────────────────
        //
        // ★ ★ **必須在路由之前**。一份 `respond 403` 的設定完全合法，
        //   而如果挑戰應答走正常路由，**使用者的設定會把自己的證書簽發擋掉**——
        //   現場看到的只是「CA 說驗不過」，設定裡沒有任何一行看得出問題。
        // ★ 表空的時候（沒開自動簽發、或已經簽完摘掉了）這裡什麼都不接，
        //   請求原樣落迴路由：**有沒有 ACME 不改變資料面的任何一條行為**。
        if let Some(key_auth) = self.http01.answer(path) {
            debug!("HTTP-01 应答 {path}");
            // ⚠ 它在路由**之前**，所以沒有站台、也沒有站台的 `log` 設定
            //   ⇒ 這一條記不進訪問紀錄（與 421 同一個形狀 —— 那是 D26，
            //     ✅ 已由 G118 結案：給 `no_site_match` 一個計數器）。
            //   ★ 仍然把 `outcome` 填對：它不進紀錄，但填對了不白填。
            // RFC 8555 §8.3 建議 `application/octet-stream`。
            write_with_headers(
                session,
                200,
                Some(Bytes::from(key_auth)),
                vec![("Content-Type".into(), "application/octet-stream".into())],
            )
            .await;
            return OUTCOME_ACME_HTTP01;
        }

        // ── 2. 路由 ────────────────────────────────────────────────────────
        let Some(routed) = rt.route(&ctx) else {
            // ★ 無站台匹配 → 421（G63）。**不靜默交給某個站台**——那是 nginx
            //   `default_server` 那類行為的溫床。
            let status = rt.defaults.no_site_match;
            debug!("无站点匹配：Host={host} port={}", self.port);
            // ── `fulcrum_no_site_match_total{host}`（G118）─────────────────
            //
            // ★ 記在**寫 `outcome` 的同一處**：這兩句說的是同一件事，
            //   分開就有一天會有人只改其中一句。
            // ⚠ ⚠ `host` 由請求方給、攻擊者可控 ⇒ **只有出現在設定裡的位址字面值
            //   才帶真值**，其餘一律 `<other>` —— 上界由設定定、不由訪問者定。
            //   ★ 通配字面值 `*.wild.example` 的子網域判**未知**：它不是一條位址
            //     字面值，而那正是這一句話的全部含義（見 `has_address_literal`）。
            // ⚠ 集合**從 `rt` 現問**，不在指標那邊另存一份 —— 換一次設定那份就過期，
            //   而過期的樣子是「某個 host 從此歸 <other>」，沒有任何東西會說。
            let h = host.to_ascii_lowercase();
            let label = if rt.has_address_literal(&h) {
                h.as_str()
            } else {
                "<other>"
            };
            metrics::NO_SITE_MATCH_TOTAL.inc(&[label]);
            let p = prepare_simple(status, None, &[], &ctx, started);
            write_prepared(session, p).await;
            return OUTCOME_NO_SITE_MATCH;
        };

        // ── 訪問紀錄：站台那一半 ──────────────────────────────────────────
        //
        // ★ ★ `outcome` 從 [`Outcome`] 那個枚舉**一次算出來**，而不是在下面
        //   五個分支裡各寫一行。⇒ 將來加一種終結方式時，
        //   `outcome_name` 的窮盡匹配會**編不過**，而那正是契約裡
        //   「`outcome` 是閉集」那句話的落法。
        // ★ `site_addr` 每請求都要（指標的 `site` 標籤）；它是 `Arc<str>`，克隆不分配。
        session.record.site_addr = Some(routed.site_addr.clone());
        session.record.target = routed.site.log.clone();
        // ★ ★ method / host / uri / site 四格**只有 `to_json_line` 讀**，而站台沒配 `log` 時那一行根本不寫
        //   ⇒ 只在配了 `log` 時才填（HTTP 層診斷 10-05：照填等於每請求白分配四次）。值與以前逐字相同。
        // ★ `uri` 取的是**原始**請求目標（`rewrite` 之前）—— 契約裡寫死的。
        //   ⚠ 取 `effective_path` 的話，一條 `rewrite` 會讓紀錄說出一個
        //   **用戶端從沒請求過**的位址，而疑難排解時那是最誤導人的一種。
        if session.record.target.is_some() {
            session.record.method = method.to_owned();
            session.record.host = host.to_owned();
            session.record.uri = if query.is_empty() {
                path.to_owned()
            } else {
                format!("{path}?{query}")
            };
            session.record.site = Some(routed.site.name.clone());
        }
        let base = outcome_name(&routed.outcome);

        // 改寫過的路徑要帶給上游。★ 沒改寫時借 `path`，⛔ 不克隆。
        let effective_path: &str = routed.rewritten_path.as_deref().unwrap_or(path);

        match &routed.outcome {
            Outcome::Respond { status, body } => {
                let text = body.map(|t| {
                    respond_body(t, &ctx, &resp_ctx(*status, None), &routed.captures, started)
                });
                let p = prepare_simple(*status, text, &routed.response_headers, &ctx, started);
                write_prepared(session, p).await;
                base
            }
            Outcome::Redirect { to, code } => {
                let rc = resp_ctx(*code, None);
                let target = to.expand(&ctx, &rc, &routed.captures, started);
                let mut extra: Vec<(String, String)> = vec![("Location".into(), target)];
                collect_ops(
                    routed.response_headers.iter().copied(),
                    &ctx,
                    &rc,
                    &routed.captures,
                    started,
                    &mut extra,
                );
                write_with_headers(session, *code, None, extra).await;
                base
            }
            // ── Prometheus 抓取端點（M2 批 M，G116）────────────────────────
            //
            // ★ 走的是 `Outcome::Redirect` 那條一模一樣的路：先擺自己那個頭，
            //   再讓站台的 `header` 指令疊上去，最後交給 `write_with_headers`。
            //   ⚠ **不新開一條寫回應的路** —— 另起一條的代價不是重複程式碼，
            //   而是訪問紀錄的 `resp_size`、壓縮、頭處理三件事會在這一條路上
            //   與別的終結類**慢慢長得不一樣**，而不一樣的那天沒有任何東西會說。
            // ⚠ `Content-Type` 擺在 `collect_ops` **之前**：寫了
            //   `header Content-Type …` 的人是在明確要求換掉它，而
            //   `write_with_headers` 裡那個 `insert_header` 是覆蓋語義。
            Outcome::Metrics => {
                let rc = resp_ctx(200, None);
                let mut extra: Vec<(String, String)> = vec![(
                    "Content-Type".into(),
                    // ⚠ 這一串是 exposition 格式的版本標識，**不是隨便寫的 MIME**：
                    //   抓取端按它決定用哪個解析器。
                    "text/plain; version=0.0.4; charset=utf-8".into(),
                )];
                collect_ops(
                    routed.response_headers.iter().copied(),
                    &ctx,
                    &rc,
                    &routed.captures,
                    started,
                    &mut extra,
                );
                write_with_headers(session, 200, Some(Bytes::from(metrics::render())), extra).await;
                base
            }
            Outcome::NoRouteMatch => {
                let status = rt.defaults.no_route_match;
                let page = prepare_error(&rt, routed.site, status, &routed, &ctx, started);
                write_error_page(session, page).await
            }
            // ── 自研靜態檔案（M2 批 F）─────────────────────────────────────
            //
            // ★ 路徑用的是 `effective_path`，也就是 `rewrite` **之後**那一個 ——
            //   與轉發那條路取同一個值。⚠ 取 `ctx.path` 的話，一條
            //   `rewrite` 就會在「轉發」與「發檔案」兩條路上給出兩個不同的答案。
            // ★ 404 / 403 / 400 交回給站台的 `handle_errors`，與 `NoRouteMatch`
            //   同一條路 —— 一個站台的錯誤頁不該因為它是靜態站就長得不一樣。
            Outcome::FileServer(fs) => {
                // ★ ★ 壓縮（M2 批 I）：`encode` 是中介軟體，靜態檔案是終結類 ——
                //   與 Caddy 一樣，中介軟體裹在終結類外面。
                //   ⚠ 而**預壓縮旁檔案優先**：那條路在 `files` 裡面判，
                //   判中了它會把這個 encoder 丟掉（旁檔案已經是壓好的）。
                // ★ 在借用期內取好 `file_server` 要的全部東西（`FileReq` + `Encoder`），
                //   之後這一支不再借請求頭 ⇒ 下面才能把 `&mut session` 交出去（⛔ 不克隆）。
                let enc = encode::Encoder::new(encode::wanted(&routed), hdr);
                let freq = files::FileReq::from_header(hdr);
                // ⚠ 下面要把 `&mut session` 交出去，而 `effective_path` / `query` 可能借著 session 裡的請求頭
                //   ⇒ 這一支各拿一份自有的（`query` 為空時不分配）。
                let url_path = effective_path.to_owned();
                let query = query.to_owned();
                match files::serve(session, &freq, fs, &url_path, &query, enc).await {
                    Ok(()) => base,
                    Err(status) => {
                        // ★ 走到這裡時一個位元組都還沒寫（`files` 裡所有 `Err(status)` 都在任何寫之前回傳，
                        //   寫出去的 405 / 304 / 416 / 正文都回傳 `Ok`）⇒ 重新借一次請求頭來算錯誤頁（⛔ 不克隆）。
                        let hdr = session.session.req_header();
                        let headers = ReqHeaders(hdr);
                        let ctx = self.facts_of(hdr, remote_ip, remote_port).ctx(&headers);
                        let page = prepare_error(&rt, routed.site, status, &routed, &ctx, started);
                        write_error_page(session, page).await
                    }
                }
            }
            // ── 轉發（可能被快取裹住，M2 批 G）──────────────────────────────
            //
            // ★ ★ `cache` 是**中介軟體**，所以它不在這個 `match` 裡佔一支 ——
            //   它在 `routed.cache` 上，由這裡決定要不要走帶快取的那條路。
            // ⚠ 快取**只裹 `reverse_proxy`**：`respond` / `redir` 沒有上游可省，
            //   而 `file_server` 的位元組已經在本機磁碟上、再存一份記憶體是淨虧。
            //   ★ 寫在別處時裝載紀錄會說出來（`log_load_summary`），不是靜默忽略。
            Outcome::Proxy(target) => {
                // ★ 反代邊寫邊讀請求頭（`header_down` 的 `{header.X}` 要等上游回來才展開）
                //   ⇒ 它克隆一份 —— 就是 `serve_one` 開頭原來那一次，挪到這裡（次數沒變）。
                let owned = hdr.clone();
                let oh = ReqHeaders(&owned);
                // ★ 這一支要跨過寫回應還在用請求的各項 ⇒ 改借這份克隆（⛔ 不再借 session 裡那份）。
                let ctx = self.facts_of(&owned, remote_ip, remote_port).ctx(&oh);
                let view = ReqView {
                    ctx: &ctx,
                    downstream_req: &owned,
                    effective_path: routed.rewritten_path.as_deref().unwrap_or(owned.uri.path()),
                    started,
                };
                let r = match routed.cache {
                    Some(c) => {
                        self.proxy_cached(&rt, session, target, &routed, &view, c)
                            .await
                    }
                    None => self.proxy(&rt, session, target, &routed, &view).await,
                };
                match r {
                    Ok(_) => base,
                    Err(status) => {
                        self.write_error(&rt, session, routed.site, status, &routed, &ctx, started)
                            .await
                    }
                }
            }
        }
    }

    /// 錯誤回應。優先用站台的 `handle_errors`。
    ///
    /// ⚠ 只有 `ctx` 借的**不是** `session` 裡那份頭時才能調它（今天只有反代那一支）；
    ///   `NoRouteMatch` 與 `file_server` 出錯那兩條用拆開的 [`prepare_error`] + [`write_error_page`]。
    // ⚠ clippy 說它 8 個參數太多。這裡放行而不是硬拆：其中 7 個本來就是
    //   「同一次請求的不同側面」，而 `rt` 是批 9 加的**設定快照** ——
    //   把它塞進 `Routed` 或 `ReqView` 會讓那兩個類型多背一份生命週期，
    //   換來的只是參數表短一格。★ 真正要守的是「整次請求用同一份快照」，
    //   而那由 `serve_one` 裡那一次 `current()` 保證，不是由參數個數保證。
    #[allow(clippy::too_many_arguments)]
    async fn write_error(
        &self,
        rt: &Runtime,
        // ⚠ 進到這裡就意味著這一條最終是**錯誤頁**，無論它原本要去哪 ——
        //   所以 `outcome` 在函式體第一行被改寫成 `error`（見下）。
        session: &mut Downstream<'_>,
        site: &SiteRt,
        status: u16,
        routed: &Routed<'_>,
        ctx: &RequestCtx<'_>,
        started: SystemTime,
    ) -> OutcomeName {
        // ★ 進到這裡就意味著這一條最終是**錯誤頁** —— 無論它原本要去哪。
        //   ⚠ ⚠ 它是**回傳值**而不是往 `Record` 上寫一筆：寫錯誤頁的呼叫點不止一處
        //   （[`write_error_page`] 今天有三處），而「下一處隨時會出現」（G110 那次的原話）—— 回傳值那種寫法下，
        //   新呼叫點**不處理這個值就編不過**，而副作用那種寫法下它只是少一行。
        //   ★ 拆成「算 / 寫」兩半之後這條照舊：`OUTCOME_ERROR` 只從 [`write_error_page`] 出來。
        let page = prepare_error(rt, site, status, routed, ctx, started);
        write_error_page(session, page).await
    }

    /// 帶快取的轉發。出錯時回傳該回給下游的狀態碼。
    ///
    /// 次序：算鍵 → 查快取 →（新鮮 ⇒ 直接發）→（陳的 ⇒ 帶檢核器去重驗證）
    /// →（沒有 ⇒ 搶防驚群的閘，leader 回源、follower 等完**重新查一次**）。
    ///
    /// ⚠ ⚠ **follower 等完必須重新查快取**，不能用 leader 遞過來的東西 ——
    /// leader 可能取回了 `no-store` / `private` 的回應，重複使用它等於把一個私有回應
    /// 發給了另外 N 個用戶端。
    async fn proxy_cached(
        &self,
        rt: &Runtime,
        session: &mut Downstream<'_>,
        target: &ProxyTarget,
        routed: &Routed<'_>,
        view: &ReqView<'_>,
        cfg: &CacheRt,
    ) -> Result<(), u16> {
        let ctx = view.ctx;
        let req = view.downstream_req;
        let primary = cache::key::primary(
            ctx.method,
            ctx.scheme,
            ctx.host,
            view.effective_path,
            ctx.query,
        );
        // ★ 這條鏈上 `encode` 要求的演算法 —— 次級鍵歸一化要用（G101）。
        //   ⚠ 查與存必須用**同一個**列表，所以它從這裡一路帶下去，
        //   而不是在兩處各自從 `Routed` 取一遍。
        let encodings = encode::wanted(routed);
        let req_cc = cache::cc::parse_request(header_str(req, "cache-control").unwrap_or(""));

        // ── 第一次查 ──────────────────────────────────────────────────
        match self
            .cache_try_serve(session, &primary, req, &req_cc, encodings)
            .await
        {
            CacheAttempt::Served(r) => return r,
            CacheAttempt::Revalidate { secondary, entry } => {
                let pass = CachePass {
                    cfg,
                    handle: &self.cache,
                    primary: primary.clone(),
                    secondary,
                    revalidate: Some(entry),
                    req_cc: req_cc.clone(),
                    encodings: encodings.to_vec(),
                };
                return self
                    .proxy_with(rt, session, target, routed, view, Some(pass))
                    .await;
            }
            CacheAttempt::Miss => {}
        }

        // ⚠ ⚠ `only-if-cached`：用戶端說「只要快取裡的」。沒有就回 **504**
        //   （RFC 9111 §5.2.1.7），**不是**回源。
        if req_cc.only_if_cached {
            debug!("only-if-cached 而缓存里没有 → 504（{primary}）");
            return Err(504);
        }

        // ── 防驚群 ────────────────────────────────────────────────────
        let slot = self.cache.coalescer.acquire(&primary);
        let _leader = match slot {
            cache::coalesce::Slot::Leader(l) => Some(l),
            cache::coalesce::Slot::Follower(n) => {
                // ★ 等 leader 放閘。⚠ 帶超時：leader 若卡在一個不回話的上游上，
                //   follower 不該跟著一起掛 —— 超時之後自己回源，那只是多一次回源。
                let waited =
                    tokio::time::timeout(std::time::Duration::from_secs(15), n.notified()).await;
                if waited.is_err() {
                    debug!("等 leader 超时，自己回源（{primary}）");
                }
                // ★ ★ **重新查**，而不是用 leader 的結果（理由見本函式文件）。
                if let CacheAttempt::Served(r) = self
                    .cache_try_serve(session, &primary, req, &req_cc, encodings)
                    .await
                {
                    return r;
                }
                None
            }
        };

        let pass = CachePass {
            cfg,
            handle: &self.cache,
            primary,
            secondary: String::new(),
            revalidate: None,
            req_cc,
            encodings: encodings.to_vec(),
        };
        self.proxy_with(rt, session, target, routed, view, Some(pass))
            .await
    }

    /// 查一次快取，能直接發就發了。
    async fn cache_try_serve(
        &self,
        session: &mut Downstream<'_>,
        primary: &str,
        req: &RequestHeader,
        req_cc: &cache::cc::RequestCc,
        encodings: &[String],
    ) -> CacheAttempt {
        let hit = self
            .cache
            .store
            .get(primary, cache_key_header(req, encodings));
        let entry = match hit {
            cache::store::Lookup::Hit(e) => e,
            // ★ 兩種未命中在紀錄裡是分開的（見 `store::Lookup` 的註解），
            //   而在這裡的處置相同：回源。
            cache::store::Lookup::VaryMiss { .. } | cache::store::Lookup::Miss => {
                return CacheAttempt::Miss;
            }
        };
        let now = now_unix();
        let age = (now - entry.stored_at).max(0) as u64;
        match cache::policy::freshness(&entry.cc, req_cc, entry.fresh_for, age) {
            cache::policy::Freshness::Fresh => CacheAttempt::Served(
                write_cached(
                    session,
                    &entry,
                    now,
                    cache::CacheState::Hit,
                    &self.cache.store,
                )
                .await,
            ),
            // ⚠ 陳的與「必須重驗證」在這裡**同一條路**：都去問上游。
            //   ★ 差別在於陳的那條**可以**在上游掛掉時退回來用（stale-if-error），
            //   而那一條這一批不做（G97 的最小集之外），所以現在兩者確實一樣。
            cache::policy::Freshness::Stale | cache::policy::Freshness::MustRevalidate => {
                let secondary =
                    cache::key::secondary(&entry.vary, cache_key_header(req, encodings));
                CacheAttempt::Revalidate { secondary, entry }
            }
        }
    }

    /// 原樣轉發（不快取）。
    async fn proxy(
        &self,
        rt: &Runtime,
        session: &mut Downstream<'_>,
        target: &ProxyTarget,
        routed: &Routed<'_>,
        view: &ReqView<'_>,
    ) -> Result<(), u16> {
        self.proxy_with(rt, session, target, routed, view, None)
            .await
    }

    /// ★ ★ ★ **轉發只有這一條路徑，快取疊在它上面。**
    ///
    /// 這不是省事：**能力翻譯與語義映射一旦有了第二條路徑就必然發生** ——
    /// 一個「帶快取的轉發」與一個「不帶快取的轉發」若是兩份程式碼，
    /// 它們對 `header_up` / `rewrite` / 候選位址退回的處理遲早會分家，
    /// ⚠ 而現場表現是「配了快取之後某個頭就沒了」。
    /// 把一次上游結果記進被動熔斷，並在**狀態翻面**時各打一行（**G136**）。
    ///
    /// ★ ★ **只有這一份推導**：三個落賬點（連不上 / 讀不到回應頭 / 上游真的回了 5xx）
    /// 全走它。⚠ 各寫一遍的失效形態是「某一條路上的失敗不算數」——
    /// 而那條路恰好是最常見的那條時，整個特性就等於不存在，且沒有任何症狀。
    ///
    /// ⚠ ⚠ **只在翻面時打紀錄，⛔ 不是每次失敗一行**：一個正在故障的上游每秒會產生
    /// 成百上千次失敗，逐次打紀錄會把現場淹掉 —— 而現場正是那時候最需要讀的東西。
    fn record_passive(target: &ProxyTarget, up: &fulcrum_runtime::Upstream, ok: bool) {
        if ok {
            if up.record_passive_success(target.passive.as_ref()) {
                info!(
                    "上游 {} 恢复：半开探针成功，重新进调度（passive_fail）",
                    up.addr
                );
            }
        } else if up.record_passive_failure(target.passive.as_ref()) {
            warn!(
                "上游 {} 被动熔断：窗口内失败达到阈值，暂时摘出调度（passive_fail）",
                up.addr
            );
        }
    }

    async fn proxy_with(
        &self,
        rt: &Runtime,
        session: &mut Downstream<'_>,
        target: &ProxyTarget,
        routed: &Routed<'_>,
        view: &ReqView<'_>,
        mut pass: Option<CachePass<'_>>,
    ) -> Result<(), u16> {
        let ReqView {
            ctx,
            downstream_req,
            effective_path,
            started,
        } = *view;
        let Some(up) = target.pick(ctx) else {
            error!("站点 {} 的 reverse_proxy 没有可用上游", routed.site.name);
            return Err(rt.defaults.all_upstreams_down);
        };
        let _guard = Inflight::acquire(up);

        // SNI 取上游**主機名**；`transport http` 時用不到。
        // ⚠ SNI 必須來自設定裡寫的那個名字，**不能用解析出來的 IP** ——
        //   拿 IP 去握手，上游的證書一定對不上。
        let sni = up.addr.split(':').next().unwrap_or("").to_string();
        // ★ ★ ★ **請求路徑上絕不做 DNS**：這裡拿的是後台任務解析好的位址。
        //   ⚠ 改之前這裡是 `HttpPeer::new(up.addr.clone(), …)`，而
        //   `HttpPeer::new` 裡是 `to_socket_addrs().unwrap()` ——
        //   於是每個請求做一次**阻塞** getaddrinfo，解析不了就 **panic**（實測過）。
        //   現在傳的是 `SocketAddr`，它的 `to_socket_addrs()` 不會失敗。
        //   ★ `pick()` 已經篩掉了解析不出來的上游，所以走到這裡通常是 Some；
        //   ⚠ 但不 unwrap —— **「通常」不是判據**。
        let candidates = up.dial_candidates();
        if candidates.is_empty() {
            error!(
                "上游 {} 还没解析出地址（DNS 没通？）—— 本次请求回 {}",
                up.addr, rt.defaults.all_upstreams_down
            );
            return Err(rt.defaults.all_upstreams_down);
        }

        // ★ **逐個候選試，不是只試第一個** —— `localhost` 的第一個位址可能是 `[::1]`
        //   而上游只聽 `127.0.0.1`，現場只有一句「連不上上游」。
        // ⚠ 只在**建連接**這一步退回：請求發出去之後再換上游那是重試語義，不歸這裡。
        let mut last_err: Option<String> = None;
        let mut got = None;
        // ⚠ 連上的**那一個** peer 要留著：收尾歸還連接（`release_http_session`）
        //   必須用同一個 peer，否則連接池會按另一個 key 歸檔 —— 那等於池永遠不復用。
        let mut used_peer: Option<HttpPeer> = None;
        // ★ 訪問紀錄要的是**位址**。⚠ 別拿 `peer.to_string()` —— 實測它給的是
        //   `addr: …, scheme: HTTP, sni: …` 那樣一串給人看的偵錯文字，
        //   而這一格是要被機器消費的。
        let mut used_dial: Option<std::net::SocketAddr> = None;
        for (i, dial) in candidates.iter().enumerate() {
            let peer = HttpPeer::new(*dial, target.tls, sni.clone());
            match self.connector.get_http_session(&peer).await {
                Ok(v) => {
                    if i > 0 {
                        debug!("上游 {} 的第 {} 个候选 {dial} 连上了", up.addr, i + 1);
                    }
                    got = Some(v);
                    used_dial = Some(*dial);
                    used_peer = Some(peer);
                    break;
                }
                Err(e) => last_err = Some(format!("{dial}：{e}")),
            }
        }
        // ★ G136 落賬點①：**一個候選位址都沒連上** —— 這是被動熔斷要數的第一類失敗。
        //   ⚠ 它記的是「連不上」這件事本身，⛔ 不是下面那個合成出來的 502：
        //   樞衡自己合成的 5xx 一律不落賬（否則一次全站故障會把每個上游再熔一遍）。
        if got.is_none() {
            Self::record_passive(target, up, false);
        }
        let (Some((mut upstream, reused)), Some(peer)) = (got, used_peer) else {
            error!(
                "连不上上游 {}（{} 个候选全都失败，最后一条：{}）",
                up.addr,
                candidates.len(),
                last_err.unwrap_or_else(|| "（没有错误信息）".into())
            );
            return Err(rt.defaults.all_upstreams_down);
        };
        debug!("上游 {} （复用={reused}）", up.addr);
        // ★ 記**真的連上的那一個**，不是設定裡寫的那串候選 ——
        //   疑難排解時想知道的是「這條請求最後落到誰身上」。
        session.record.upstream = used_dial.map(|d| d.to_string());

        // ── 上游請求頭 ────────────────────────────────────────────────────
        let mut ureq = downstream_req.clone();
        if effective_path != ctx.path {
            // `rewrite` 改過路徑：要帶到上游去。查詢串按原樣保留。
            let new_uri = if ctx.query.is_empty() {
                effective_path.to_string()
            } else {
                format!("{effective_path}?{}", ctx.query)
            };
            match new_uri.parse::<http::Uri>() {
                Ok(u) => ureq.set_uri(u),
                Err(e) => {
                    error!("改写后的路径不是合法 URI（{new_uri}）：{e}");
                    return Err(500);
                }
            }
        }
        apply_ops_to_request(&mut ureq, &target.header_up, ctx, &routed.captures, started);
        // ★ 重驗證：把快取裡那條的檢核器帶給上游（M2 批 G）。
        //   ⚠ 順序在 `header_up` **之後**：使用者顯式寫的 `header_up If-None-Match`
        //   應當贏 —— 那是他明確要求的，而這幾條是我們替他加的。
        if let Some(cp) = &pass
            && let Some(e) = &cp.revalidate
        {
            if let Some(t) = &e.etag {
                let _ = ureq.insert_header("If-None-Match", t.clone());
            }
            if let Some(lm) = &e.last_modified {
                let _ = ureq.insert_header("If-Modified-Since", lm.clone());
            }
        }

        if let Err(e) = upstream.write_request_header(Box::new(ureq)).await {
            error!("写上游请求头失败：{e}");
            return Err(rt.defaults.all_upstreams_down);
        }

        // ── 請求體 ────────────────────────────────────────────────────────
        loop {
            match session.read_request_body().await {
                Ok(Some(chunk)) => {
                    if let Err(e) = upstream.write_request_body(chunk, false).await {
                        error!("写上游请求体失败：{e}");
                        return Err(rt.defaults.all_upstreams_down);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    debug!("读下游请求体失败：{e}");
                    return Err(400);
                }
            }
        }
        if let Err(e) = upstream.finish_request_body().await {
            error!("收尾上游请求体失败：{e}");
            return Err(rt.defaults.all_upstreams_down);
        }

        // ── 回應頭 ────────────────────────────────────────────────────────
        // ★ G136 落賬點②：連上了、但**拿不回一個回應頭**（超時、對端半路關了、協定錯）。
        //   ⚠ 它與①分開是有意的：①說的是「撥不通」，②說的是「撥通了但它不幹活」，
        //   而後者恰恰是主動健康檢查最容易漏掉的一類。
        if let Err(e) = upstream.read_response_header().await {
            error!("读上游响应头失败：{e}");
            Self::record_passive(target, up, false);
            return Err(rt.defaults.all_upstreams_down);
        }
        let Some(uresp) = upstream.response_header() else {
            error!("上游没给响应头");
            Self::record_passive(target, up, false);
            return Err(rt.defaults.all_upstreams_down);
        };
        let mut dresp = uresp.clone();
        let status = dresp.status.as_u16();

        // ★ ★ ★ G136 落賬點③：**上游真的發回來的**狀態碼。這一條是這個特性的立身之本 ——
        //   一個 `/health` 回 200 而真實業務在 500 的上游，主動健康檢查看不出來，只有它看得見。
        //   ⚠ ⚠ 判據是 `>= 500`，而這裡拿到的是**上游發回來的那一個**（`uresp`），
        //   ⛔ 不是樞衡合成的那些（`all_upstreams_down` / `no_route_match` / 錯誤頁）——
        //   那些根本不經過這一行。★ 這一點是「不自我強化」那條設計的**執行者**：
        //   合成的 5xx 若也落賬，一次全站故障會把每個上游輪流再熔一遍。
        Self::record_passive(target, up, status < 500);

        // ── ★ ★ 重驗證命中：304 ⇒ **只更新中繼資料，不動 body**（G83 那條理由的記憶體版）
        //
        // ⚠ ⚠ 這裡回給用戶端的是**快取裡那份完整回應**，不是上游這個 304 ——
        //   304 是給我們看的，不是給用戶端看的。★ 直接把 304 轉下去，
        //   用戶端會拿到一個「沒變」的答覆，而它手上**根本沒有那份內容**。
        if status == 304
            && let Some(cp) = &pass
            && let Some(entry) = cp.revalidate.clone()
        {
            let fresh_for = cache_freshness_of(&uresp.headers, cp.cfg, now_unix());
            let cc = cache_cc_of(&uresp.headers);
            cp.handle
                .store
                .refresh(&cp.primary, &cp.secondary, fresh_for, now_unix(), cc);
            debug!("缓存重验证命中 304 —— 只更新元数据（{}）", cp.primary);
            self.connector
                .release_http_session(upstream, &peer, None)
                .await;
            return write_cached(
                session,
                &entry,
                now_unix(),
                cache::CacheState::Revalidated,
                &cp.handle.store,
            )
            .await;
        }

        // ── 快取事件：回源（R7 的 `miss`）──────────────────────────────────
        //
        // ★ ★ **這一行是「這條回應的內容來自上游」唯一的判定點**，而那正是 `miss`
        //   的定義。上面那個 304 分支已經把「重驗證成功、發出去的還是快取裡那一份」
        //   摘走了（它在 `write_cached` 裡記 `stale`）⇒ 走到這裡的每一條都在往下游
        //   發上遊給的位元組，無論它此前是沒命中還是重驗證沒驗住。
        // ⚠ 記在這裡而不是「查快取沒命中」那一處：那一處之後還可能拐彎 ——
        //   `only-if-cached` 回 504（根本沒回源）、防驚群的 follower 等完重查一次
        //   **命中了**。在那裡記，兩種都會被算成 `miss`，而**數字看起來完全正常**。
        // ⚠ `pass` 是 `None` 說明這個站台根本沒配 `cache`：它沒有「命中/未命中」
        //   可言，一條都不記。★ 記了的話，`hit/(hit+miss)` 會被沒開快取的流量
        //   稀釋成一個假比例 —— 而那個比例讀起來同樣完全正常。
        if pass.is_some() {
            cache::CacheEvent::Miss.record(1);
        }

        // ★ 存進快取的是**改完頭之後**那一份（`header_down` 已經施加）——
        //   ⚠ 存改頭之前那份的話，命中時那些頭會**消失**：一條 `header_down`
        //   在未命中時生效、命中時不生效，而兩次請求的設定一模一樣。
        //   ⇒ 兩份快照都留著：`uresp_snapshot` 判可快取性（那是上游說的話），
        //   `dresp_snapshot` 是要存下來的內容。
        let uresp_snapshot = uresp.clone();
        {
            // ★ 先算好取值再改頭：`{resp_header.X}` 說的是**上游給的**那份，
            //   而不是我們改到一半的那份。借用規則恰好把這件事逼成顯式的。
            let snapshot = dresp.clone();
            let snap_headers = RespHeaders(&snapshot);
            let rc = ResponseCtx {
                status: Some(status),
                upstream: Some(&up.addr),
                headers: Some(&snap_headers),
            };
            let mut pairs = Vec::new();
            collect_ops(
                &target.header_down,
                ctx,
                &rc,
                &routed.captures,
                started,
                &mut pairs,
            );
            collect_ops(
                routed.response_headers.iter().copied(),
                ctx,
                &rc,
                &routed.captures,
                started,
                &mut pairs,
            );
            apply_pairs(&mut dresp, &pairs);
        }
        // ── ★ ★ 壓縮（**M2 批 I**，G99–G102）────────────────────────────────
        //
        // 它的位置是判據的一部分，兩側都不能挪：
        // · 排在 `header_down` **之後** —— 使用者改的頭要被壓縮層看見
        //   （`Content-Type` 決定壓不壓、`Content-Encoding` 決定要不要放過）；
        // · 排在快取捕獲**之前** —— G101 拍的是「壓完再存」，
        //   ⇒ 快取裡那份就是壓縮後的位元組，命中時直接發、不必再壓一遍。
        // ⚠ `status_has_no_body`：沒有體的回應（204/304）不許被加上 `chunked`。
        let mut encoder = encode::Encoder::new(encode::wanted(routed), downstream_req);
        if let Some(enc) = &mut encoder {
            enc.header_filter(&mut dresp, encode::status_has_no_body(status));
        }
        let dresp_snapshot = dresp.clone();
        if let Err(e) = session.write_response_header(Box::new(dresp)).await {
            debug!("写下游响应头失败：{e}");
            return Ok(()); // 下游沒了，不是上游的錯
        }

        // ── 回應體 ────────────────────────────────────────────────────────
        //
        // ★ ★ **邊發邊捕獲**，而不是先全收下來再發：一個正在下載 2GB 的用戶端
        //   不該等我們把 2GB 收完。⚠ 捕獲只到 `max_size` 為止 ——
        //   超過就**放棄快取**並繼續照發，而不是把記憶體吃光。
        let mut captured: Option<Vec<u8>> = pass.as_ref().map(|_| Vec::new());
        let cap_limit = pass.as_ref().map(|p| p.cfg.max_size_bytes).unwrap_or(0);
        loop {
            match upstream.read_response_body().await {
                Ok(Some(chunk)) => {
                    // ★ ★ 壓縮排在捕獲之前（G101：壓完再存）。
                    //   ⚠ 壓縮層會**攢**資料：一塊進去可能什麼都不出來，
                    //   而**空塊不能寫下去** —— HTTP/1.1 的分塊編碼裡
                    //   一個零長塊就是**體結束**，寫下去等於把回應截斷在這裡。
                    let out = match &mut encoder {
                        Some(e) => e.body_filter(Some(&chunk), false),
                        None => None,
                    };
                    let chunk = out.unwrap_or(chunk);
                    if chunk.is_empty() {
                        continue;
                    }
                    if let Some(buf) = &mut captured {
                        if buf.len() as u64 + chunk.len() as u64 <= cap_limit {
                            buf.extend_from_slice(&chunk);
                        } else {
                            // ⚠ 一超上限就**整條放棄**，而不是存半截。
                            //   ★ 存半截 = 此後每個命中都拿到一個被截斷的回應，
                            //   而 `Content-Length` 還是原來那個 —— 用戶端會掛在那兒等。
                            debug!("响应超过 max_size，本条不缓存");
                            captured = None;
                        }
                    }
                    if let Err(e) = session.write_response_body(chunk, false).await {
                        debug!("写下游响应体失败：{e}");
                        return Ok(());
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    error!("读上游响应体失败：{e}");
                    // 頭已經發出去了，改不了狀態碼——只能斷開。
                    // ⚠ 這時**不許存**：我們手上是個不完整的回應。
                    return Ok(());
                }
            }
        }
        // ★ ★ 壓縮的收尾：壓縮層手上還攢著最後一段（gzip 的 footer、brotli 的收尾塊）。
        //   ⚠ 漏了這一步，用戶端拿到的是一個**少了尾巴**的壓縮流 —— 而
        //   `Content-Length` 已經被壓縮層去掉了，所以它不會「長度對不上」，
        //   只會在解壓到最後一刻報「意外的流結束」。★ 快取裡那份同樣會少尾巴，
        //   於是**每一次命中都重現同一個壞回應**。
        if let Some(enc) = &mut encoder
            && let Some(tail) = enc.body_filter(None, true)
            && !tail.is_empty()
        {
            if let Some(buf) = &mut captured {
                if buf.len() as u64 + tail.len() as u64 <= cap_limit {
                    buf.extend_from_slice(&tail);
                } else {
                    captured = None;
                }
            }
            if let Err(e) = session.write_response_body(tail, false).await {
                debug!("写下游压缩收尾失败：{e}");
                return Ok(());
            }
        }
        let _ = session.write_response_body(Bytes::new(), true).await;

        // ── 存進快取 ──────────────────────────────────────────────────────
        if let (Some(cp), Some(body)) = (pass.take(), captured) {
            store_if_allowed(
                &cp,
                &uresp_snapshot,
                &dresp_snapshot,
                body,
                ctx,
                downstream_req,
            );
        }

        // ★ 歸還連接到池子裡。失敗不影響本次請求，但要說出來。
        self.connector
            .release_http_session(upstream, &peer, None)
            .await;
        Ok(())
    }
}

/// 一次「帶快取的轉發」要帶著的東西。
struct CachePass<'a> {
    cfg: &'a CacheRt,
    handle: &'a cache::CacheHandle,
    primary: String,
    /// 重驗證時是那條已有條目的次級鍵；未命中時先空著，存的時候按回應的 `Vary` 算。
    secondary: String,
    /// 非 `None` = 這次是**重驗證**，快取裡已經有一條。
    revalidate: Option<Box<cache::store::Entry>>,
    req_cc: cache::cc::RequestCc,
    /// 這條鏈上 `encode` 要求的演算法（**M2 批 I**）。
    ///
    /// ⚠ ⚠ 它進這個結構**只為一件事**：存的時候算次級鍵要用它歸一化
    /// `Accept-Encoding`（G101）—— 而查的時候用的必須是**同一個**列表，
    /// 否則存與查落在兩個鍵上，快取永遠不命中。
    /// ★ 帶進來而不是在 `store_if_allowed` 裡從 `Routed` 現取，是因為那裡拿不到 `Routed`。
    encodings: Vec<String>,
}

/// 查快取的三種結果。
enum CacheAttempt {
    /// 已經發完了。
    Served(Result<(), u16>),
    /// 快取裡有，但要先問上游。
    ///
    /// ⚠ `entry` 裝了箱，理由與 `store::Lookup::Hit` 一樣：這個枚舉在
    /// **每一次未命中時也要被建構**，而條目本身比別的變體大一個數量級。
    Revalidate {
        secondary: String,
        entry: Box<cache::store::Entry>,
    },
    Miss,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn header_str<'a>(req: &'a RequestHeader, name: &str) -> Option<&'a str> {
    req.headers.get(name).and_then(|v| v.to_str().ok())
}

/// 算快取次級鍵時用的「取一個請求頭」——**只有 `Accept-Encoding` 被歸一化**（**G101**）。
///
/// G101 拍了「壓完再存」⇒ 快取裡那份是壓過的，`Vary: Accept-Encoding` 讓快取按這個頭分身。
/// ⚠ 瀏覽器發的寫法與順序千奇百怪 —— 拿原值當次級鍵，同一條 URL 會存出幾十份
/// **內容完全相同**的條目。
///
/// ⚠ ⚠ **「查」和「存」兩處必須用同一個函式** —— 正確性就建立在這一點上。
/// ★ 它不改動 `key::secondary` 的簽名，於是兩個後端一個字都不用動。
fn cache_key_header<'a>(
    req: &'a RequestHeader,
    encodings: &'a [String],
) -> impl FnMut(&str) -> Option<&'a str> + 'a {
    move |name: &str| {
        if name.eq_ignore_ascii_case("accept-encoding") {
            // ★ 回傳的是 `&'static str`，所以這裡沒有生命週期上的麻煩。
            Some(encode::preferred_encoding(header_str(req, name), encodings))
        } else {
            header_str(req, name)
        }
    }
}

fn resp_header_str<'a>(h: &'a http::HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn cache_cc_of(h: &http::HeaderMap) -> cache::cc::ResponseCc {
    cache::cc::parse_response(resp_header_str(h, "cache-control").unwrap_or(""))
}

/// 這次回應的新鮮期（秒）。★ `ttl` 是**備援**（G96）—— 走到最後一步才用它。
fn cache_freshness_of(h: &http::HeaderMap, cfg: &CacheRt, now: i64) -> u64 {
    let cc = cache_cc_of(h);
    if let Some(n) = cc.shared_max_age() {
        return n;
    }
    if let Some(exp) = resp_header_str(h, "expires").and_then(cache::files_httpdate) {
        let date = resp_header_str(h, "date")
            .and_then(cache::files_httpdate)
            .unwrap_or(now);
        return (exp - date).max(0) as u64;
    }
    cfg.ttl_ms.map(|ms| ms / 1000).unwrap_or(0)
}

/// 把快取裡那條發給用戶端。
///
/// ⚠ ⚠ **`Age` 頭是必須的**（RFC 9111 §5.1）：下游可能還有一層快取，
/// 它要拿 `Age` 去算自己的新鮮度。★ 不發的話，一條已經在我們這兒放了 59 分鐘的回應，
/// 在下一層看起來是**剛出爐的** —— 於是同一份內容會被一路放大到兩倍壽命。
async fn write_cached(
    session: &mut Downstream<'_>,
    entry: &cache::store::Entry,
    now: i64,
    state: cache::CacheState,
    store: &cache::Backend,
) -> Result<(), u16> {
    let age = (now - entry.stored_at).max(0);
    let mut resp = match ResponseHeader::build(entry.status, None) {
        Ok(r) => r,
        Err(e) => {
            error!("缓存命中却建不出响应头：{e}");
            return Err(500);
        }
    };
    for (k, v) in &entry.headers {
        let _ = resp.append_header(k.clone(), v);
    }
    let _ = resp.insert_header("Age", age.to_string());
    // ★ 讓「這一條是從哪來的」在**回應裡**看得見，而不是只在紀錄裡。
    //   ⚠ 判據也靠它：只看狀態碼與體，命中與回源長得一模一樣。
    // ⚠ 後端後綴（`-DISK`）由 `store` 加 —— 它知道本處理程序挑中了哪個後端。
    let header = store.state(state);
    let _ = resp.insert_header("X-Fulcrum-Cache", header.as_str());
    // ★ ★ ★ 回應頭、訪問紀錄那一格、指標那一格**三者取同一個 `state`，在同一處賦** ——
    //   分幾處各算一遍的話，它們哪天不一致，每一處讀起來都很正常（G66 的形狀）。
    //   ⚠ 回源那條路**沒有**這個頭，所以紀錄裡也沒有 `cache` 這一格（契約裡寫死的）；
    //     它的 `miss` 記在 `proxy_with` 裡回源那一處。
    //   ⚠ 指標那一格比回應頭**粗**：`HIT` 與 `HIT-DISK` 折成同一個 `hit`（R7）。
    session.record.cache = Some(header);
    state.event().record(1);
    let _ = resp.insert_header("Content-Length", entry.body.len().to_string());
    if let Err(e) = session.write_response_header(Box::new(resp)).await {
        debug!("写缓存响应头失败：{e}");
        return Ok(());
    }
    let _ = session
        .write_response_body(Bytes::from(entry.body.clone()), true)
        .await;
    Ok(())
}

/// 逐跳頭：**不許存**，也不許原樣轉發（RFC 9110 §7.6.1）。
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// 判一次回應能不能存，能就存。
fn store_if_allowed(
    cp: &CachePass<'_>,
    uresp: &ResponseHeader,
    dresp: &ResponseHeader,
    body: Vec<u8>,
    ctx: &RequestCtx<'_>,
    req: &RequestHeader,
) {
    let now = now_unix();
    let cc = cache_cc_of(&uresp.headers);
    // ★ ★ ★ **可快取性看上游那份（`uresp`），次級鍵看我們發出去的那份（`dresp`）。**
    //   `Vary: Accept-Encoding` 是**壓縮層加在 `dresp` 上的**，`uresp` 裡一個字沒有 ⇒
    //   從 `uresp` 取會得到空 vary ⇒ 次級鍵恆空 ⇒ 所有用戶端共用同一條，而那條是壓過的
    //   ⇒ **一個從沒說過自己認 gzip 的用戶端會收到 gzip 的位元組**。
    //   ⚠ 現場是「有些用戶端看到亂碼」，而兩邊的頭都完全正常。
    // ⇒ **承諾是誰做的，鍵就跟誰走。**
    // ★ 而 `vary_star`（`Vary: *`）仍看 `uresp`：那是可快取性判斷，屬於上游說的話。
    let vary_raw = resp_header_str(&dresp.headers, "vary").unwrap_or("");
    let upstream_vary = resp_header_str(&uresp.headers, "vary").unwrap_or("");
    let input = cache::policy::StoreInput {
        method: ctx.method,
        status: uresp.status.as_u16(),
        has_authorization: header_str(req, "authorization").is_some(),
        resp_cc: &cc,
        req_cc: &cp.req_cc,
        expires: resp_header_str(&uresp.headers, "expires").and_then(cache::files_httpdate),
        date: resp_header_str(&uresp.headers, "date")
            .and_then(cache::files_httpdate)
            .unwrap_or(now),
        body_len: Some(body.len() as u64),
        max_size: cp.cfg.max_size_bytes,
        default_ttl_secs: cp.cfg.ttl_ms.map(|ms| ms / 1000),
        // ★ 這一條看**上游**那份（理由見上面 `upstream_vary` 那段）。
        vary_star: cache::key::is_vary_star(upstream_vary),
        has_set_cookie: uresp.headers.get("set-cookie").is_some(),
    };
    let fresh_for = match cache::policy::is_storable(&input) {
        cache::policy::Storable::Yes { fresh_for_secs } => fresh_for_secs,
        cache::policy::Storable::No(why) => {
            debug!("不缓存（{why}）：{}", cp.primary);
            return;
        }
    };

    let vary = cache::key::parse_vary(vary_raw);
    let secondary = cache::key::secondary(&vary, cache_key_header(req, &cp.encodings));

    // ★ ★ 存的是**改完頭之後**那一份（`header_down` 已經施加），但要剝兩類頭：
    //   ① 逐跳頭 —— 它們屬於**這一條連接**，存下來發給別人是錯的；
    //   ② `no-cache="字段名"` 與 `private="字段名"` 點名的那幾個 —— RFC 9111 §5.2.2
    //      說得很明確，共享快取不許把它們發給別人。
    //   ⚠ ⚠ ② 這一條最容易漏：`no-cache="Set-Cookie"` 的意思正是
    //   「你可以存這個回應，但**別把那個頭轉給下一個人**」。漏了它就是串號。
    let mut headers: Vec<(String, String)> = Vec::new();
    for (k, v) in dresp.headers.iter() {
        let name = k.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&name.as_str()) {
            continue;
        }
        if cc.no_cache_fields.contains(&name) || cc.private_fields.contains(&name) {
            continue;
        }
        // `Content-Length` 由 `write_cached` 自己按體長重算 —— 存下來會打架。
        if name == "content-length" || name == "age" {
            continue;
        }
        if let Ok(sv) = v.to_str() {
            headers.push((k.as_str().to_string(), sv.to_string()));
        }
    }

    let entry = cache::store::Entry {
        status: uresp.status.as_u16(),
        headers,
        body,
        stored_at: now,
        fresh_for,
        cc: cc.clone(),
        etag: resp_header_str(&uresp.headers, "etag").map(str::to_string),
        last_modified: resp_header_str(&uresp.headers, "last-modified").map(str::to_string),
        vary,
    };
    debug!("存进缓存（新鲜 {fresh_for}s）：{}", cp.primary);
    cp.handle.store.put(&cp.primary, secondary, entry);
}

/// 取 Host：h1 看 `Host` 頭，h2 看 `:authority`（Pingora 會把它填進 uri）。
/// ★ 借請求頭、⛔ 不分配（HTTP 層診斷 10-05 · 請求路徑上的字串分配）。
fn host_of(req: &RequestHeader) -> &str {
    if let Some(v) = req.headers.get("host")
        && let Ok(s) = v.to_str()
        && !s.is_empty()
    {
        return s;
    }
    req.uri.host().unwrap_or("")
}

/// 把一個 [`Outcome`] 翻成訪問紀錄契約裡那個**閉集**的取值。
///
/// ★ **窮盡 `match` 是判據**：契約寫著 `outcome` 是閉集，而「閉集」要有東西守著才算數
/// ⇒ 給 [`Outcome`] 加一種終結方式時**這裡編不過**。
///
/// ★ ★ 回傳 [`OutcomeName`] 而不是 `&'static str`，堵的是另一條路：
/// 編譯器逼你補一條臂，但**逼不了你在那條臂裡寫一個正當的值** ——
/// 回傳類型一收緊，`=> "foo"` 就當場是類型錯誤。
fn outcome_name(o: &Outcome<'_>) -> OutcomeName {
    match o {
        Outcome::Respond { .. } => OUTCOME_RESPOND,
        Outcome::Redirect { .. } => OUTCOME_REDIR,
        Outcome::Proxy(_) => OUTCOME_REVERSE_PROXY,
        Outcome::FileServer(_) => OUTCOME_FILE_SERVER,
        Outcome::Metrics => OUTCOME_METRICS,
        Outcome::NoRouteMatch => OUTCOME_ERROR,
    }
}

/// ★ ★ 用戶端 IP 與連接埠 = **socket 對端**。不看 `X-Forwarded-For`（用戶端可偽造）。
///
/// ★ Unix socket 上沒有 IP：回傳 `None` 而不是編一個 `127.0.0.1`——
/// `remote_ip` 匹配器在取不到 IP 時判**不命中**，那是安全的一側。
fn client_addr(session: &ServerSession) -> (Option<IpAddr>, u16) {
    match session.client_addr().and_then(|a| a.as_inet()) {
        Some(a) => (Some(a.ip()), a.port()),
        None => (None, 0),
    }
}

/// 這條連接的 TLS 四格（**M2 批 L 第 ③ 步**；**改成單一來源**）。
/// `None` = 它不是 TLS。
///
/// ★ **四格全部來自同一個 `SslDigest`**，h1/h2 與 h3 走同一段程式碼 —— 差別只在
/// 誰造了那份 digest（h1/h2 是上游的 `from_ssl()`，h3 是 [`crate::quic::h3_session`]）。
/// ⇒ 「同一格資料兩個填法」在這一層做不到。
///
/// ⚠ **h3 上 `cipher` 是空的** —— quiche 的 `Handshake::cipher()` 鎖在私有 `mod tls` 裡。
/// 空串在 [`access_log::TlsFields`] 裡就是「這一格不出現」。
fn tls_fields(session: &ServerSession) -> Option<access_log::TlsFields> {
    let ssl = session.digest()?.ssl_digest.as_ref()?;
    Some(access_log::TlsFields {
        version: ssl.version.to_string(),
        cipher: ssl.cipher.to_string(),
        // ★ fork 改動 14：上游的 `SslDigest` 本來沒有這兩格。
        sni: ssl.sni.clone(),
        alpn: ssl.alpn.clone(),
    })
}

/// 只帶狀態碼與上游位址的回應上下文。
///
/// ⚠ **不在這裡塞回應頭**：`ResponseCtx.headers` 要一個 `&dyn Headers`，
/// 而它必須借用一個活著的 `RespHeaders`。⚠ 不要用 `Box::leak` 繞過它 ——
/// 那是**每個請求洩漏一次**，而一個「能編過、跑得動、記憶體慢慢漲」的寫法
/// 正是最難在測試裡看見的那種。需要回應頭的地方在棧上自己建。
fn resp_ctx<'a>(status: u16, upstream: Option<&'a str>) -> ResponseCtx<'a> {
    ResponseCtx {
        status: Some(status),
        upstream,
        headers: None,
    }
}

/// 把一串頭操作展開成 `(名字, 取值)`。名字前綴 `+` = 追加，`-` = 刪除。
///
/// ★ 泛型接 `&HeaderOpRt` 的迭代器，於是 `&[HeaderOpRt]` 與 `&[&HeaderOpRt]`
/// 兩種形狀都能直接喂進來，不必為了統一類型先 clone 一份。
fn collect_ops<'o>(
    ops: impl IntoIterator<Item = &'o HeaderOpRt>,
    ctx: &RequestCtx<'_>,
    rc: &ResponseCtx<'_>,
    caps: &[String],
    now: SystemTime,
    out: &mut Vec<(String, String)>,
) {
    for op in ops {
        match op.op.as_str() {
            "remove" => out.push((format!("-{}", op.name), String::new())),
            other => {
                let v = op
                    .value
                    .as_ref()
                    .map(|t| t.expand(ctx, rc, caps, now))
                    .unwrap_or_default();
                let prefix = if other == "add" { "+" } else { "" };
                out.push((format!("{prefix}{}", op.name), v));
            }
        }
    }
}

/// 把 `(名字, 取值)` 施加到回應頭上。
fn apply_pairs(resp: &mut ResponseHeader, pairs: &[(String, String)]) {
    for (name, value) in pairs {
        if let Some(n) = name.strip_prefix('-') {
            resp.remove_header(n);
        } else if let Some(n) = name.strip_prefix('+') {
            let _ = resp.append_header(n.to_string(), value);
        } else {
            let _ = resp.insert_header(name.to_string(), value);
        }
    }
}

fn apply_ops_to_request(
    req: &mut RequestHeader,
    ops: &[HeaderOpRt],
    ctx: &RequestCtx<'_>,
    caps: &[String],
    now: SystemTime,
) {
    let rc = ResponseCtx::default();
    for op in ops {
        match op.op.as_str() {
            "remove" => {
                req.remove_header(&op.name);
            }
            other => {
                let v = op
                    .value
                    .as_ref()
                    .map(|t| t.expand(ctx, &rc, caps, now))
                    .unwrap_or_default();
                if other == "add" {
                    let _ = req.append_header(op.name.clone(), &v);
                } else {
                    let _ = req.insert_header(op.name.clone(), &v);
                }
            }
        }
    }
}

/// `respond` 的正文。
///
/// ★ 整段都是字面值時**不拷**：回傳的 `Bytes` 直接指著範本裝載時建好的那一份
///   （[`Template::shared_literal`]），每個請求只剩 `from_owner` 那一個幾十位元組的小分配。
///   ⚠ 沒讓 `fulcrum-runtime` 直接存 `Bytes`（那樣連小分配都沒有）：要給它加 `bytes` 相依、
///   改 `Cargo.lock`，而 aarch64 那一格的觸發集雜湊的正是整份 `Cargo.lock`。
/// 有佔位符的照舊按請求展開。
fn respond_body(
    t: &Template,
    ctx: &RequestCtx<'_>,
    rc: &ResponseCtx<'_>,
    caps: &[String],
    now: SystemTime,
) -> Bytes {
    match t.shared_literal() {
        Some(lit) => Bytes::from_owner(Arc::clone(lit)),
        None => Bytes::from(t.expand(ctx, rc, caps, now)),
    }
}

/// 一條待寫的簡單回應 —— 就是 [`write_with_headers`] 收的那三樣（**HTTP 層診斷發現 2 · 請求頭不整份克隆**）。
///
/// ★ 把「算」與「寫」拆開的理由：算要借請求頭（`ctx`），寫要 `&mut session`，
///   而請求頭就在 `session` 裡 ⇒ 兩件事不能同時進行；先算完、放掉借用、再寫。
/// ★ `#[must_use]`：算出來卻沒寫出去，這條請求就一個位元組都不發、狀態記成 0 —— 拆開之前
///   `write_simple(..)` 回傳 future，忘了 `.await` 會被 lint 抓到；拆開之後要靠這一行。
#[must_use]
struct Prepared {
    status: u16,
    body: Option<Bytes>,
    extra: Vec<(String, String)>,
}

/// 算：站台的 `header` 指令按這條回應展開。
fn prepare_simple(
    status: u16,
    body: Option<Bytes>,
    ops: &[&HeaderOpRt],
    ctx: &RequestCtx<'_>,
    started: SystemTime,
) -> Prepared {
    let mut extra = Vec::new();
    let rc = resp_ctx(status, None);
    collect_ops(ops.iter().copied(), ctx, &rc, &[], started, &mut extra);
    Prepared {
        status,
        body,
        extra,
    }
}

/// 寫：交給 [`write_with_headers`]。
async fn write_prepared(session: &mut Downstream<'_>, p: Prepared) {
    write_with_headers(session, p.status, p.body, p.extra).await;
}

/// 算好的錯誤頁。★ `#[must_use]`：拿到它就得交給 [`write_error_page`]，
/// 而 `outcome = error` 只從那裡出來 —— 與拆開之前「`write_error` 的回傳值逼著處理」同一條紀律。
#[must_use]
struct ErrorPage(Prepared);

/// 錯誤頁：算。優先用站台的 `handle_errors`。
fn prepare_error(
    rt: &Runtime,
    site: &SiteRt,
    status: u16,
    routed: &Routed<'_>,
    ctx: &RequestCtx<'_>,
    started: SystemTime,
) -> ErrorPage {
    ErrorPage(match rt.error_page(site) {
        Some(page) => {
            // ★ `handle_errors` 塊內 `{status}` 指的是**原始**錯誤碼，
            //   不是它自己那條 respond 的碼——否則那個佔位符毫無用處。
            let rc = resp_ctx(status, None);
            let body = page
                .body
                .map(|t| Bytes::from(t.expand(ctx, &rc, &routed.captures, started)));
            prepare_simple(page.status, body, &routed.response_headers, ctx, started)
        }
        None => prepare_simple(status, None, &routed.response_headers, ctx, started),
    })
}

/// 錯誤頁：寫。⚠ 進到這裡就意味著這一條最終是**錯誤頁** —— `outcome` 從這裡出來。
async fn write_error_page(session: &mut Downstream<'_>, page: ErrorPage) -> OutcomeName {
    write_prepared(session, page.0).await;
    OUTCOME_ERROR
}

/// 不建大小寫表時，這個頭名在 h1 上會不會**照原樣**寫出去（G162）。
///
/// pingora 沒表時按 `titled_header_name_str` 寫：表裡有就寫那一種，沒有就寫小寫 ⇒ 只有恰好是那一種寫法的名，
/// 換 `build_no_case` 才逐位元組不變。`X-Frame-Options`、`content-type` 都不是（前者會變小寫，後者會變 `Content-Type`）。
pub(crate) fn h1_name_as_is(name: &str) -> bool {
    match http::HeaderName::from_bytes(name.as_bytes()) {
        Ok(h) => pingora_http::titled_header_name_str(&h).unwrap_or(h.as_str()) == name,
        Err(_) => false,
    }
}

/// 樞衡自己構的回應頭（G162）：要插的名都照原樣寫得出就**不建大小寫表**（每個回應省一張表），否則照舊建。
///
/// `extra` 是 [`apply_pairs`] 收的那種對子：`-名` 只刪不插 ⇒ 不礙事；`+名` 與 `名` 看名本身。
/// ★ 使用者 `header` 指令的名大小寫隨意 ⇒ 寫了不合預設寫法的名，那一條回應仍建表、照他寫的發。
/// ⚠ 本函式之外再往這個頭裡插的名（下面兩個字面值、漏斗裡的 `Alt-Svc`、pingora 寫回應時用常數補的）
///   都必須照原樣寫得出 —— 由 `自建响应里固定插的名都照原样写得出` 那條測試釘著。
fn self_built_header(
    status: u16,
    extra: &[(String, String)],
) -> pingora_core::Result<ResponseHeader> {
    let as_is = extra
        .iter()
        .all(|(n, _)| n.starts_with('-') || h1_name_as_is(n.strip_prefix('+').unwrap_or(n)));
    if as_is {
        ResponseHeader::build_no_case(status, None)
    } else {
        ResponseHeader::build(status, None)
    }
}

/// 帶正文的自建回應預設的 `Content-Type`。
const TEXT_PLAIN: &str = "text/plain; charset=utf-8";

/// [`write_with_headers`] 要寫的那份回應頭：只算、⛔ 不寫。`body_len` 為 `None` = 沒有正文。
///
/// ★ 除了頭表本身，只有 `Content-Length` 的值分配一次（2026-10-15 · respond 剩的分配）：
///   `Content-Type` 走 `from_static`，⛔ 別換回 `&str` —— 那會經 `HeaderValue::from_str` 每個回應拷一份。
/// ★ pub 只為 `tests/respond_alloc.rs` 數分配。
pub fn simple_response_header(
    status: u16,
    body_len: Option<usize>,
    extra: &[(String, String)],
) -> pingora_core::Result<ResponseHeader> {
    let mut resp = self_built_header(status, extra)?;
    let len = body_len.unwrap_or(0) as u64;
    let _ = resp.insert_header("Content-Length", files::content_length_value(len));
    if body_len.is_some() {
        let _ = resp.insert_header("Content-Type", HeaderValue::from_static(TEXT_PLAIN));
    }
    apply_pairs(&mut resp, extra);
    Ok(resp)
}

async fn write_with_headers(
    session: &mut Downstream<'_>,
    status: u16,
    bytes: Option<Bytes>,
    extra: Vec<(String, String)>,
) {
    let resp = match simple_response_header(status, bytes.as_ref().map(|b| b.len()), &extra) {
        Ok(r) => r,
        Err(e) => {
            error!("构不出响应头（status={status}）：{e}");
            return;
        }
    };
    if let Err(e) = session.write_response_header(Box::new(resp)).await {
        debug!("写响应头失败：{e}");
        return;
    }
    match bytes {
        Some(b) => {
            let _ = session.write_response_body(b, true).await;
        }
        None => {
            let _ = session.write_response_body(Bytes::new(), true).await;
        }
    }
}

/// 把裝載結論打出來：生效的預設值（G52 的接班人）、自環、
/// 未串接能力（[`fulcrum_runtime::UNWIRED`]）。
///
/// ★ 裝載時說清楚，而不是等出事再查——回落是真實的效能與維運成本，
/// 未串接的能力是真實的功能缺口，兩者都不該需要另外去問才知道。
pub fn log_load_summary(
    cfg: &fulcrum_config::StructuredConfig,
    rt: &Runtime,
    cache: &cache::CacheHandle,
) {
    // ★ ★ ★ **自研靜態檔案：把生效的 hide 清單逐字打出來**（G88 的「可見性」那一格，M2 批 F）。
    //
    // ⚠ ⚠ 預設表**非空**（`.git` `.env` `.svn` `.hg` `.bzr` `.DS_Store`），
    //   而一個不說出來的非空預設就是一次**靜默行為** —— 現場是「我的檔案明明在，
    //   卻一直 404」，而設定檔案裡一個字都看不出來。批 20 為「靜默失能」專門堵過一次。
    // ★ 打的是**執行環境手裡那一份**（`rt.file_servers()`），不是照設定再算一遍：
    //   兩份各算各的時，紀錄說的和伺服器做的可以不是一回事。
    for (site, fs) in rt.file_servers() {
        info!(
            "静态文件：站点 {site} root={} index={} browse={} follow_symlinks={}",
            fs.root,
            fs.index.join(","),
            fs.browse,
            fs.follow_symlinks,
        );
        info!(
            "  hide（生效的完整清单，按路径段匹配、命中回 404）：{}",
            fs.hide.join(" ")
        );
        // ★ 預壓縮旁檔案（M2 批 I）：**配了才說**，沒配時不打這一行 ——
        //   一行「precompressed：（無）」對每一個靜態站台都印一遍，就是噪音。
        if !fs.precompressed.is_empty() {
            info!(
                "  预压缩旁文件：{}（发 /x.css 时先找 /x.css.{{gz,br,zst}}；                 ⚠ 旁文件比原文件旧就当它不存在）",
                fs.precompressed.join(" ")
            );
        }
    }
    // ★ ★ ★ **自研壓縮：把生效的演算法打出來**（M2 批 I）。
    //
    // ⚠ ⚠ `encode` 從 M1 起就寫得下、而執行環境一步都不做（它在 `UNWIRED` 裡躺了
    //   整整一段）。⇒ 它**剛剛開始真的生效**這件事必須說出來 ——
    //   一個從舊版本升上來的站台，行為在這一刻變了，而設定一個字都沒改。
    // ★ 打的是**執行環境手裡那一份**，不是照設定再算一遍。
    for (site, algos) in rt.encodings() {
        info!(
            "压缩：站点 {site} encode={}（按客户端 `Accept-Encoding` 的**首选**挑；             ⚠ 被压的响应没有 `Content-Length` 与 Range，强 ETag 会被弱化成 W/\"…\"）",
            algos.join(" ")
        );
    }
    // ★ ★ ★ **自研快取：把生效的設定打出來**（G96 的可見性，M2 批 G）。
    //
    // ⚠ ⚠ `ttl` 是**備援**不是覆蓋 —— 而從 nginx `proxy_cache_valid` 遷過來的人
    //   預設會以為它是覆蓋。★ 一個與使用者預期相反的預設，如果不說出來，
    //   現場是「我明明配了 5 分鐘，怎麼 30 秒就變了」，而設定裡一個字都看不出問題。
    // ⚠ 這幾行由 `tests/cache/run.sh` 的兩條裝載紀錄斷言守著 —— 一個「寫好了卻沒人調」
    //   的摘要函式與不存在的摘要函式，在紀錄上長得一模一樣。
    for (site, c) in rt.cache_settings() {
        info!(
            "缓存：站点 {site} ttl={} max_size={}B capacity={}B",
            match c.ttl_ms {
                Some(ms) => format!("{}s（**兜底** —— 上游给了新鲜度就听上游的）", ms / 1000),
                None => "（未配 ⇒ 上游没给新鲜度就不缓存）".to_string(),
            },
            c.max_size_bytes,
            c.capacity_bytes,
        );
        // ★ ★ ★ **後端說的是「真的挑中了哪一個」，不是設定裡寫了什麼。**
        //   ⚠ 兩句話在一種情況下恰好不一樣：配了 `disk` 而那個目錄用不了 ——
        //   那時快取是**關掉**的（照常轉發，只是不再有命中），而這一行是
        //   維運在啟動那一刻唯一看得見它的地方。
        //   ★ 第二個信號在執行環境：`X-Fulcrum-Cache` 那個頭不再出現。
        match &cache.store {
            cache::Backend::Mem(s) => info!(
                "  内存后端（没写 `disk`）；生效容量 {}B；只裹 reverse_proxy，别处不生效",
                s.capacity()
            ),
            cache::Backend::Disk(s) => info!(
                "  磁盘后端 {}（两级分片 · meta/body 两文件 · 启动不扫盘，G83/G84）；\
                 生效容量 {}B；只裹 reverse_proxy，别处不生效",
                s.root().display(),
                s.capacity()
            ),
            cache::Backend::Off => warn!(
                "  缓存后端：已关闭 —— 配了 `disk {}` 而那个目录用不了\
                 （上面那行 error 说了为什么）；请求照常转发，只是不再有命中",
                c.disk_dir.as_deref().unwrap_or("?")
            ),
        }
    }
    // ★ ★ 反向那一半：配了 `cache` 卻沒有 `reverse_proxy` 可裹 ⇒ **說出來**。
    //   ⚠ 一條不生效的 `cache` 與一條正在生效的，在設定檔案裡長得一模一樣 ——
    //   與回落層那條「死設定」是同一個形狀（那一層沒了，教訓留下）。
    if !rt.cache_settings().is_empty() && rt.all_upstreams().is_empty() {
        warn!("★ 配了 `cache`，而这份配置里**没有任何 reverse_proxy** —— 缓存不会生效");
    }
    // ★ ★ ★ **自環：`reverse_proxy` 指回樞衡自己**（M2 批 G 從回落層挪過來）。
    //
    // ⚠ ⚠ 這幾句話此前**算了卻沒人說**：`Runtime::self_loop_warnings` 在建圖那一刻
    //   就算好了，而全倉唯一的讀者是單測 ⇒ 「算得對」與「維運看得見」在同一個欄位上
    //   長得一模一樣，而現場讀到的是一片安靜。★ `fulcrum-runtime` 有意不相依 `log`
    //   （它回傳話、不自己列印），所以「說出來」這一步只能落在這裡。
    // ⚠ **是 warn 不是 error**：指回自己可能是有意的（自己終止 TLS 再回自己的明文口）
    //   ⇒ 說出來，但不替使用者拒絕。判據在 `tests/serve/run.sh` 的 [4/4]（含反向那一半：
    //   不自環的上游不許被說進去），單測那側釘的是扁平站台、這裡釘的是容器下鑽。
    for w in &rt.self_loop_warnings {
        warn!("{w}");
    }
    for (k, why) in rt.unwired_in_use(cfg) {
        warn!("⏳ `{k}` 这一批还没接线：{why}");
    }
    // ⚠ 這裡原來有一行「連接埠上有 https 站台，而 TLS 還沒串接，會以明文 HTTP 監聽」。
    //   TLS 接上之後它就成了**假警告**，而假警告比沒有警告更糟：它訓練人忽略警告，
    //   連帶把真的那幾條一起埋掉。TLS 的現狀由 `tls::log_tls_notes` 說，
    //   它說的是「哪些站台還缺證書」——那是真的還缺的東西。
}

/// 展開一個範本（給外部呼叫方用，例如 CLI 的自檢）。
pub fn expand_for_test(t: &Template, ctx: &RequestCtx<'_>) -> String {
    t.expand(ctx, &ResponseCtx::default(), &[], SystemTime::now())
}

// ── 裝配與執行 ──────────────────────────────────────────────────────────────

/// 起服務需要的、設定層管不到的那幾項。
///
/// ★ 它們屬於**處理程序模型**（G13/G31/G33/G37），而不是站台設定：
/// pid 檔案與升級 socket 的路徑由 systemd 託管、可覆蓋，
/// 所以它們從命令行進來，不進 DSL。
pub struct ServeOptions {
    /// 監聽位址的主機部分。預設 `0.0.0.0`。
    pub bind_host: String,
    pub pid_file: String,
    pub upgrade_sock: String,
    /// 狀態目錄（G33 的 `StateDirectory=`）。證書存在 `<state>/certs/` 下面。
    pub state_dir: String,
    /// 是否從正在跑的舊世代接管（Pingora 的 `-u`）。
    pub upgrade: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        // ★ 預設值照 G33：systemd 託管為主，全部可覆蓋。
        ServeOptions {
            bind_host: "0.0.0.0".to_string(),
            pid_file: "/run/fulcrum/fulcrum.pid".to_string(),
            upgrade_sock: "/run/fulcrum/upgrade.sock".to_string(),
            state_dir: "/var/lib/fulcrum".to_string(),
            upgrade: false,
        }
    }
}

/// 起 Pingora，把每個監聽連接埠掛上一個 [`FulcrumApp`]，然後**不回傳**。
///
/// ⚠ **每個連接埠一個 service**：站台索引按 `(Host, 本地端口)` 兩維查，
/// 而工作階段本身問不出「我落在哪個監聽器上」。一個連接埠一個 app 執行個體是唯一不靠猜的辦法。
pub fn serve(cfg: &fulcrum_config::StructuredConfig, rt: Arc<Runtime>, opts: ServeOptions) -> ! {
    use pingora_core::server::Server;
    use pingora_core::server::configuration::Opt;
    use pingora_core::services::listening::Service as ListeningService;

    // ★ 造 `ServerConf` 的活兒在 `process::build_server_conf` 裡，因為**它有門**：
    //   `serve()` 回傳 `!`、還要綁連接埠，單測碰不了它，而「排空視窗不許留 None」
    //   這類判據恰恰最需要門。見 process.rs 頂部那張四條缺口表。
    // ★ 探測一次、往下傳（G140）。⛔ 別在每個 service 那裡各問一次系統：
    //   那樣一趟裡可能得到不同的答案（cgroup 配額是會變的），
    //   而「同一趟裡兩個 service 按不同的核數算」沒有任何東西會說。
    let cpus = process::detected_cpus();
    let threads_l7 = process::threads_for(process::ThreadRole::L7, &cfg.global, cpus);
    let threads_l4 = process::threads_for(process::ThreadRole::L4, &cfg.global, cpus);

    let conf = process::build_server_conf(cfg, &opts, cpus);
    let shutdown_budget = process::shutdown_budget_secs(&conf);

    // ★ ★ **把這一趟真的用了幾個執行緒打進啟動紀錄**（D19 / G135 剛教過的那條：
    //   一個沒有執行環境出口的旋鈕，設了也無從核對它到底生效了沒）。
    //   ⚠ 管理面那一格由 `conf.threads` 承載，⇒ 從 `conf` 上讀回來，
    //   ⛔ 不是把剛算出來的那個值再回顯一遍 —— 回顯自己剛寫進去的值，
    //   對「它有沒有真的落到 `ServerConf` 上」零判別力。
    info!(
        "线程模型（G35）：L7 每个 service {threads_l7} · L4 每个 service {threads_l4} · \
         管理面与后台 {}（探测到 {cpus} 个可用核）。⚠ 线程不跨 service 共享 ⇒ 总数 ≈ Σ(各 service)",
        conf.threads
    );

    let opt = Opt {
        upgrade: opts.upgrade,
        ..Default::default()
    };
    let mut server = Server::new_with_opt_and_conf(opt, conf);
    server.bootstrap();

    // ★ ★ 換代觸發器要在 `run_forever()` **之前**裝上：否則從「服務起來」到
    //   「run() 建好 runtime」之間的那一小段時間裡，`systemctl reload` 會丟。
    //   ⚠ 更要緊的是——不裝它的話 SIGUSR2 走預設動作，即**終止處理程序**：
    //   一次 `systemctl reload` 會把服務打死。
    process::spawn_upgrade_trigger();

    // ★ **快取執行個體只建一次，全部監聽連接埠共用一份**：每連接埠一份的話，同一條 URL
    //   在 :80 與 :443 上會各存一份，而容量上限也會變成設定值的 N 倍。
    // ★ ★ 「各塊取最大值」那條推導在 [`cache::effective_capacity`] 裡，**只有一處** ——
    //   `POST /load` 換設定之後要算同一個值（D19），兩處各寫一遍的失效形態是
    //   「啟動時算出來的容量與 load 之後算出來的不是同一個數」，而兩處各自都自洽。
    let cache_capacity = cache::effective_capacity(&rt.cache_settings());
    let cache_dir = rt
        .cache_settings()
        .iter()
        .find_map(|(_, c)| c.disk_dir.clone());
    // ⚠ ⚠ **它必須建在 `log_load_summary` 之前**：裝載紀錄要說的是
    //   「真的挑中了哪個後端」，而不是「設定裡寫了什麼」。★ 目錄用不了時這兩句話
    //   恰好不一樣 —— 而那正是最需要說出來的那一刻。
    let cache = cache::CacheHandle::new(cache::Backend::open(cache_dir.as_deref(), cache_capacity));

    log_load_summary(cfg, &rt, &cache);

    // ★ 把停機預算打出來，因為 `TimeoutStopSec` 必須按它設，而它由設定決定。
    //   ⚠ 不打的話，這個數只存在於原始碼裡，維運只能靠猜或靠翻文件。
    info!(
        "停机预算约 {shutdown_budget}s（排空 + 收尾）—— systemd unit 的 TimeoutStopSec 要大于它；\
         换代时老一代另需最多 5s 送 fd"
    );

    // ── TLS：按 SNI 挑證書（§5.1 第 1 條鎖死的那條路）────────────────────
    let cert_root = std::path::Path::new(&opts.state_dir).join("certs");
    let issuer = fulcrum_acme::issuer_slug(
        cfg.global
            .acme_ca
            .as_deref()
            .unwrap_or(fulcrum_acme::LETSENCRYPT_PRODUCTION),
    );
    let plan = match tls::plan_tls(&rt, &cert_root, &issuer, cfg.global.default_sni.as_deref()) {
        Ok(p) => p,
        Err(errs) => {
            // ⚠ 走到這裡說明設定裡顯式給的 PEM 讀不出來。**不許降級成告警**：
            //   一個「起來了但那個站台永遠握手失敗」的處理程序比起不來更難查。
            for e in &errs {
                error!("TLS 装载失败：{e}");
            }
            std::process::exit(1);
        }
    };
    tls::log_tls_notes(&plan, &format!("证书存储 {}", cert_root.display()));

    // ── 訪問紀錄：**裝載時就把每個紀錄檔案打開一次**（M2 批 L 第 ② 步）────────
    //
    // ⚠ ⚠ 與上面 TLS 那條逐字同一個理由，而這裡更刺眼：一個打不開的紀錄檔案
    //   若拖到第一個請求才發現，現場是**服務完全正常、而紀錄一行都沒有** ——
    //   ★ ★ ★ 一個用來「出了事你能知道」的東西，自己壞掉時沒人知道。
    //   ⇒ 硬錯誤，不許降級成告警。
    match access_log::open_all(&rt) {
        Ok(0) => {}
        Ok(n) => info!("访问日志：打开了 {n} 个日志文件"),
        Err(errs) => {
            for e in &errs {
                error!("访问日志装载失败：{e}");
            }
            std::process::exit(1);
        }
    }

    // ── ACME：自動簽發與續期（G53/G54/G56）────────────────────────────────
    //
    // ★ 巡檢跑在**後台服務**裡，也就是監聽器起來之後——HTTP-01 要 CA 能連上來，
    //   在監聽器之前簽必然失敗一次，還白耗一次速率配額。
    let http01 = Arc::new(fulcrum_acme::Http01Store::new());
    let acme = acme::build(
        cfg,
        &rt,
        plan.resolver.clone(),
        http01.clone(),
        &opts.state_dir,
    );

    // ★ 監聽連接埠從**建起來的那一份**取。⚠ 換設定換不了監聽連接埠——
    //   Pingora 在啟動時綁定，連接埠集變了只能走 `systemctl reload`（真的換代）。
    //   全量 load 那條路會顯式拒絕這種設定，理由寫在 admin 模組裡。
    let listen_ports = rt.listen_ports.clone();
    // 管理面要拿它當「連接埠集有沒有變」的判據（全量 load 那條路）。
    let listen_ports_for_admin = listen_ports.clone();
    // ★ ★ **收流量之前先把網域上游解析一遍。**
    //   ⚠ 少了這一步，第一批請求會撞上「還沒解析出來」而被回 502；
    //   而在改這一批之前，它們撞上的是**每請求一次 panic**（實測過）。
    dns::resolve_now(&rt, "启动");
    let dns_interval = dns::tick_interval(cfg);
    let shared = fulcrum_runtime::SharedRuntime::new(rt);

    // ── ★ ★ 指標裡「抓取時去問活體」那幾個族的取數物件（**M2 批 M**）───────────
    //
    // ★ ★ **登記的是句柄，不是一份讀數**：上游在途數、健康位、證書到期時刻、
    //   ACME 簽發計數全都在繪製那一刻現問 —— 能從被測物件本身問到的東西，
    //   就不要在旁邊再記一份，否則兩份遲早不一致，而不一致的那天沒有任何東西會說。
    // ★ 登記 `shared` 而不是某一份 `Runtime` 快照：全量 load 換掉的正是它裡面那一份。
    // ⚠ `acme` 這一格可以是 `None`（這份設定裡沒有自動簽發）—— 那時
    //   `fulcrum_acme_issue_total` 只出 HELP/TYPE、不出樣本，而**不是整族消失**：
    //   整族消失會讓「沒接上」與「沒資料」在抓取端看起來一模一樣。
    // ── 連接計數的登記處（**M2 批 O**，G122 的連接那半）────────────────────────
    //
    // ★ ★ 它的格子由**建立服務的那一行**順手宣告（`view` / `guard_for` 都會宣告）——
    //   ⛔ 有意不在這裡先枚舉一遍監聽器：那會是**第二條枚舉路徑**，而
    //   `Runtime::all_proxy_targets` 的文件正是為這個形狀寫的警告
    //   （「⚠ ⚠ 有意不自己再走一遍……走法分家的表現是某條在某一張清單裡不存在」）。
    let conn_reg = conn_stats::ConnRegistry::new();

    metrics::register_live(metrics::LiveSources {
        runtime: Some(shared.clone()),
        resolver: Some(plan.resolver.clone()),
        acme: acme.clone(),
        conn: Some(conn_reg.clone()),
    });

    // ── ★ ★ 本代的身分（**G109 ①**）——**一個處理程序一把，不是一個連接埠一把** ─────
    //
    // ⚠ 每個監聽連接埠各鑄一把也能跑，而且**判據全綠** —— 直到換代那天：
    //   轉交是按 DCID 前綴反查「這是哪一代」的，一代處理程序有多個身分，
    //   老一代就認不全自己的連接。★ 那種紅在批 K 才出現，而根因在這一行。
    // ★ 鑄在循環外，是讓「一代一個」在結構上成立，而不是靠每個分支都記得傳同一個。
    let gen_id = quic::gen_id::GenId::random();
    // ── HTTP 面的 PROXY protocol 「收」半邊（M2 批 L 第 ① 步）—─────────
    //
    // ★ 一個處理程序一份，所有連接埠共用 —— 信任清單本來就是**全域**的
    //   （它是**連接級**判斷：一條連接上還沒有 Host，還不知道會落到哪個站台）。
    // ☆ 它拿的是 `SharedRuntime` 不是快照，理由見 `proxyproto` 模組的類型文件。
    let proxy_protocol_policy = proxyproto::HttpProxyProtocol::new(shared.clone());
    for (port, is_tls) in listen_ports {
        // ── ★ ★ ★ **G110：HTTP/3 跟著 `tls` 自動開** ───────────────────────
        //
        // 有 TLS 的連接埠自動在**同一個連接埠號**上聽 UDP，並對 h1/h2 的回應發
        // `Alt-Svc: h3=":<端口>"`。⚠ 不發它，瀏覽器永遠不會主動嘗試 h3。
        //
        // ★ `ma` 取一天（RFC 7838 §3.1 的預設值，這裡寫顯式）：取更長只會讓
        //   「哪天關掉 h3」之後用戶端多試很久的 UDP，而重新廣播本來就不花什麼。
        let alt_svc = is_tls.then(|| format!("h3=\":{port}\"; ma=86400"));
        let app = Arc::new(FulcrumApp::new(
            shared.clone(),
            port,
            is_tls,
            http01.clone(),
            cache.clone(),
            alt_svc,
            // 每執行緒池大小取 ServerConf 的值（預設 128），乘 L7 執行緒數 ⇒ 與 0.8.1 的總容量相同。
            server
                .configuration
                .upstream_keepalive_pool_size
                .saturating_mul(threads_l7.max(1)),
        ));
        let kind = if is_tls { "https" } else { "http" };
        // ⚠ `H1Entry` 是為了讓 h3 那一側拿到**同一個** app 執行個體，理由見它的類型文件。
        let mut svc =
            ListeningService::new(format!("fulcrum-{kind}-{port}"), H1Entry::new(app.clone()));

        // ── ★ ★ ★ HTTP 面的 PROXY protocol（**M2 批 L 第 ① 步**，fork 改動 12）────
        //
        // ★ ★ **無條件掛，不看清單空不空** —— 有條件掛的話，一份後來才寫上
        //   `proxy_protocol_from` 的設定做 `POST /load` 會**裝得上、不回報錯誤、也不生效**（D19 那個形狀）。
        // ★ 「掛了」不等於「會讀位元組」：清單空時 `trusts()` 恆 false，
        //   而 `proxyproto` 的 `PreTlsProcess` 實作在 false 時一個位元組都不讀。
        // ★ pingora 0.9.0 起走上游的 `set_pre_tls_callback`（fork 改動 12 只剩「明文連接埠也調」，見 FORK.md §12）。
        svc.endpoints()
            .set_pre_tls_callback(proxy_protocol_policy.clone());

        let bind = format!("{}:{port}", opts.bind_host);

        // ── 連接計數（**M2 批 O**，fork 改動 15 的接縫）─────────────────────────
        //
        // ★ `view()` 順手**宣告**這一格 ⇒ 這個監聽器從處理程序起來的第一秒就有一條 `0`
        //   的樣本，而不是「有連接了才出現」。⚠ 那個區分正是「我配了這個連接埠怎麼
        //   沒流量」要靠的東西：沒有 series 與「有 series 但值是 0」在抓取端分不開。
        svc.endpoints()
            .set_connection_counter(conn_reg.view(conn_stats::Entrypoint::Http, &bind));
        if is_tls {
            // ⚠ ⚠ ★ **別把這一行換成 `TlsSettings::with_callbacks`「順手拿點什麼」。**
            //   帶回呼時上游走 `handshake_with_callback()`，每次握手都要多走一趟
            //   「掛起 → `certificate_callback` → `resume_accept`」
            //   （`start_accept()` 無條件裝一個恆回 `-1` 的 `cert_cb`）。
            //   訪問紀錄要的 SNI / ALPN 由 **fork 改動 14** 直接記進 `SslDigest`
            //   （`from_ssl()` 握手結束後本來就握著 `&SslRef`）⇒ 那趟開銷歸零。
            // ★ 守它的是 `crates/fulcrum/tests/tls_digest_gate.rs`。
            match pingora_core::tls::ssl::SslAcceptor::mozilla_intermediate_v5(
                pingora_core::tls::ssl::SslMethod::tls(),
            )
            .map(pingora_core::listeners::tls::TlsSettings::from)
            {
                Ok(mut settings) => {
                    // ★ ★ **G104 的落點就在這一行**：h1/h2 入口與（將來的）h3 入口
                    //   掛**同一個**回呼，於是「兩個入口各有一套挑證書實作」在結構上做不到。
                    //   那正是取「統一 BoringSSL」而不是「兩套並存」的全部理由（D18/G66 同源）。
                    plan.resolver.install_into(&mut settings);

                    // h2 + http/1.1 —— ALPN 裡兩個都要有，否則只會 h2 的用戶端連不上。
                    // ★ ★ 再加一條 `acme-tls/1`：TLS-ALPN-01（RFC 8737 / G54 的「主」）
                    //   要在**同一個連接埠**上完成，挑戰挪不到別處（RFC 8737 §3 寫死 443）。
                    // ★ 順序即偏好序：`acme-tls/1` 放末尾，**正常流量的偏好完全不受影響**，
                    //   而只提供它的 CA 驗證連接會且只會拿到它。
                    // ⚠ 這裡寫的是 **wire 格式**（每條前面一個長度位元組），
                    //   與 `set_alpn_protos` 收的格式一致。
                    settings.set_alpn_select_callback(|_ssl, client| {
                        const PREF: &[u8] = b"h2http/1.1
acme-tls/1";
                        pingora_core::tls::ssl::select_next_proto(PREF, client)
                            .ok_or(pingora_core::tls::ssl::AlpnError::NOACK)
                    });
                    info!(
                        "监听 {bind}（HTTPS，按 SNI 动态挑证书，ALPN: h2 / http/1.1 / acme-tls/1）"
                    );
                    // ★ 與 `add_tls_with_settings(&bind, None, settings)` 只差寫緩衝
                    //   （pingora 裡那個函式就是這一句去掉 `.l4_buffer(…)`），理由見 [`L7_L4_BUFFER`]。
                    svc.add_listener(
                        ListenerConfig::tcp(&bind)
                            .tls(settings)
                            .l4_buffer(L7_L4_BUFFER),
                    );
                }
                Err(e) => {
                    error!("建不出 TLS 监听设置：{e}");
                    std::process::exit(1);
                }
            }
        } else {
            info!("监听 {bind}（HTTP）");
            // ★ 與 `add_tcp(&bind)` 只差寫緩衝，理由見 [`L7_L4_BUFFER`]。
            svc.add_listener(ListenerConfig::tcp(&bind).l4_buffer(L7_L4_BUFFER));
        }
        // ★ L7 資料面的執行緒數（G35 / G140）。⚠ 這是**逐 service** 的覆蓋，
        //   `Service::threads()` 回傳 `Some` 時 pingora 就不看全域 `conf.threads` 了。
        //   ⇒ 這一行是 `threads_l7` 在 h1/h2 那條路上唯一的落點。
        svc.threads = Some(threads_l7);
        server.add_service(svc);

        // ── ★ ★ ★ HTTP/3 入口（**M2 批 J**，G110）───────────────────────────
        //
        // ★ 它是**另一個 `Service`**：pingora 的 `ListeningService` 只管 TCP/UDS，
        //   而 QUIC 要自己收 UDP 包、按 DCID 散佈 ⇒ `QuicListenerService` 自建監聽器
        //   並自己參與 socket 移交（fd 鍵前綴 `fulcrum-quic:`）。
        // ⚠ **跟在 `add_service(svc)` 之後加**，於是啟動紀錄裡同一連接埠的 h1/h2 與 h3
        //   兩行是挨著的 —— 「443 上到底開沒開 h3」是維運第一個會問的問題。
        if is_tls {
            let mut q = quic::listener::QuicListenerService::new(
                bind.clone(),
                opts.upgrade,
                plan.resolver.clone(),
                gen_id,
                app.clone(),
                // ★ ★ **M2 批 K**：換代轉交 socket 落在**換代 socket 的父目錄**裡。
                //   ⚠ 那是一條推導，所以 `run_dir_of` 有自己的判據 —— 見它的文件。
                quic::listener::run_dir_of(&opts.upgrade_sock),
                conn_reg.clone(),
            );
            // ★ h3 與 h1/h2 **同一個角色**（G140 ③，owner 拍板）：它就是資料面入口，
            //   與 h1/h2 服務同一批流量、走同一條執行鏈。
            q.threads = Some(threads_l7);
            server.add_service(q);
        }
    }

    // ── L4 面：TCP 透傳（M2 批 A）────────────────────────────────────────
    //
    // ★ 排在 HTTP 監聽器之後加：兩者互不相依，而按「先七層後四層」的順序加，
    //   啟動紀錄讀起來與設定檔案的形狀一致。
    // ⚠ `--bind-host` 只在設定寫成 `:3306`（沒有主機部分）時才補上，
    //   寫了 `127.0.0.1:3306` 就按寫的來 —— 與 HTTP 那邊**不同**：那邊連接埠來自站台位址，
    //   主機部分永遠由 `--bind-host` 決定，而 L4 的監聽位址是**整串**寫在設定裡的。
    {
        let cur = shared.current();
        for l in &cur.l4_listeners {
            let bind = match &l.listen_host {
                Some(h) => format!("{h}:{}", l.listen_port),
                None => format!("{}:{}", opts.bind_host, l.listen_port),
            };
            // ★ ★ 裝載紀錄要**逐條說出分流規則**：一條「按 SNI 走別處」的規則如果只活在
            //   設定檔案裡，維運在排查「為什麼這個網域去了另一臺機器」時無從下手。
            let ups: Vec<&str> = l
                .target
                .as_ref()
                .map(|t| t.upstreams.iter().map(|u| u.addr.as_str()).collect())
                .unwrap_or_default();
            if ups.is_empty() {
                info!(
                    "L4 {} {} → **没有兜底**：只服务下面这 {} 条规则认得的连接",
                    l.proto.as_str(),
                    bind,
                    l.rules.len()
                );
            } else {
                info!(
                    "L4 {} {} → 兜底 {} 个上游（{}），轮询",
                    l.proto.as_str(),
                    bind,
                    ups.len(),
                    ups.join(" ")
                );
            }
            for r in &l.rules {
                let rups: Vec<&str> = r.target.upstreams.iter().map(|u| u.addr.as_str()).collect();
                info!(
                    "  ├ {} {} → {}（按书写顺序，第一个命中即用）",
                    r.kind.as_str(),
                    r.values.join(" "),
                    rups.join(" ")
                );
            }
            // ★ 兩種協定各起各的服務：它們的**失效面完全不同**（見 l4.rs 那張對照表），
            //   共用一個 `Service` 只會讓兩套語義在同一個 `match` 裡互相打架。
            match l.proto {
                fulcrum_runtime::L4Proto::Tcp => {
                    let mut s = l4::TcpProxyService::new(
                        shared.clone(),
                        &l.listen,
                        bind,
                        opts.upgrade,
                        conn_reg.clone(),
                    );
                    s.threads = Some(threads_l4);
                    server.add_service(s);
                }
                fulcrum_runtime::L4Proto::Udp => {
                    let mut s = l4::UdpProxyService::new(
                        shared.clone(),
                        &l.listen,
                        bind,
                        opts.upgrade,
                        conn_reg.clone(),
                    );
                    s.threads = Some(threads_l4);
                    server.add_service(s);
                }
            }
        }
    }

    // ── ★ ★ G59 第 2 條：**啟動時檢核 DNS 憑證** ────────────────────────────
    //
    // 形狀照 G15（On-Demand 沒配存取資格就拒絕啟動）：**錯誤在啟動時暴露，
    // 不等被濫用才發現**。⚠ 理由不是潔癖 —— 拿到某域的 DNS 寫權限
    // 等於能為該域簽發任意證書，還能改 MX 劫持電子郵件。
    //
    // ★ 這一步跑在 `run_forever()` **之前**，所以「拒絕啟動」是真的拒絕。
    //   ⚠ 這裡要一個臨時 runtime：檢核是非同步的，而 Pingora 的 runtime 還沒起來。
    if let Some(manager) = &acme {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                if let Err(e) = rt.block_on(manager.verify_credentials()) {
                    error!("{e}");
                    error!("拒绝启动（G59 第 2 条：凭据校验不过就不启动）");
                    std::process::exit(1);
                }
            }
            // ⚠ 建不出 runtime 是本機資源問題，與憑證無關。說出來，不假裝檢核過了。
            Err(e) => error!("⚠ 建不出临时 runtime（{e}）—— 本次**没有**校验 DNS 凭据"),
        }
    }

    // ── ★ ★ 管理面（G8 的全量原子 load + G74 的強制續期），走 Unix socket（G14）──
    //
    // ⚠ 沒配 `admin` 就**一個 socket 都不建**：管理面預設不出機器，
    //   而「預設開著」正是 G14 要堵的那個 Caddy 短處。
    if let Some(spec) = &cfg.global.admin {
        match admin::socket_path(spec) {
            Ok(path) => {
                // ⚠ 上一代留下的 socket 檔案會讓 bind 失敗（EADDRINUSE），而回報錯誤只說
                //   「位址已被使用」，看不出是一個陳舊的檔案。systemd 重新啟動、非優雅退出都會留下它。
                // ★ ★ ★ **但換代（`-u`）那一趟絕對不能清**：監聽 fd 是從上一代繼承來的
                //   （`ListenerEndpoint::listen` 按位址字串查 fd 表，UDS 的鍵就是這個路徑），
                //   新一代根本不會重新 `bind()`。此時 unlink 掉，兩代都在一個**沒有名字的**
                //   inode 上 accept，用戶端按路徑連過去只拿到 ENOENT ——
                //   **一次 `systemctl reload` 之後管理面永久失聯**，而紀錄裡一切正常。
                if !opts.upgrade && std::path::Path::new(&path).exists() {
                    match std::fs::remove_file(&path) {
                        Ok(()) => info!("清掉上一代留下的管理 socket {path}"),
                        Err(e) => error!("清不掉旧的管理 socket {path}（{e}），下面多半绑不上"),
                    }
                }
                let app = admin::AdminApp::new(
                    shared.clone(),
                    listen_ports_for_admin,
                    acme.clone(),
                    Some(cache.clone()),
                    // ★ ★ 與 `metrics::LiveSources.resolver`（上面 `register_live`
                    //   那一行）拿的是**同一個** `Arc<SniResolver>`——`/stats` 與
                    //   `/metrics` 的證書到期因此只有一個源（M2 批 N 任務 6）。
                    Some(plan.resolver.clone()),
                );
                let mut svc = ListeningService::new("fulcrum-admin".to_string(), app);
                // ★ 連接計數（**M2 批 O**）：管理面單列一格 `entrypoint="admin"`，
                //   ⛔ 不併進 `http` —— 跨平面求和之前先按 entrypoint 過濾，
                //   是抓取端唯一能把資料面與管理面分開的辦法。
                svc.endpoints()
                    .set_connection_counter(conn_reg.view(conn_stats::Entrypoint::Admin, &path));
                // ★ 0600：**這就是管理面的全部訪問控制**（G14：交給檔案系統 ACL）。
                svc.add_uds(
                    &path,
                    Some(std::os::unix::fs::PermissionsExt::from_mode(
                        admin::SOCKET_MODE,
                    )),
                );
                info!(
                    "管理面监听 unix:{path}（权限 {:o}）—— POST /load、POST /renew",
                    admin::SOCKET_MODE
                );
                server.add_service(svc);
            }
            // ⚠ 寫錯了要**拒絕啟動**，不是打條警告繼續跑：一個「以為管理面開著、
            //   其實沒開」的處理程序，比一個起不來的處理程序難查得多。形狀照 G15。
            Err(e) => {
                error!("{e}");
                std::process::exit(1);
            }
        }
    }

    // ── 主動健康檢查（`health_uri`）────────────────────────────────────────
    //
    // ★ 只在**真的有目標配了 `health_uri`** 時才起（同 DNS 那條的理由）。
    //   ⚠ 判據與打點節奏共用一次判定，見 `health::any_health_check` 上那段。
    if health::any_health_check(cfg) {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-health-check",
            health::HealthCheckService::new(shared.clone(), health::tick_interval(cfg)),
        ));
    }

    // ── 磁碟快取的後台維護（**M2 批 H**，G84）──────────────────────────────
    //
    // ★ 只在**真的是磁碟後端**時才起（同 DNS / 健康檢查那條的理由）：
    //   記憶體後端下它沒有任何事可做，而一個每秒醒一次什麼都不做的任務，
    //   會讓「有沒有磁碟快取」在 `top` 裡看不出區別。
    // ★ ★ 它掛成 `BackgroundService` 而不是裸 `tokio::spawn`，是為了**停機那一刻**：
    //   淘汰索引要在那時存下去（G84 的 save 那一半），而裸 spawn 的任務會被直接丟掉。
    if cache.disk().is_some() {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-cache-maintenance",
            cache::CacheMaintenanceService::new(cache.clone()),
        ));
    }

    // ── 上游網域的定期重解析（`dns_refresh`）────────────────────────────────
    //
    // ★ 只在**真的有網域上游**時才起：一份全是 IP 字面值的設定起這個任務，
    //   等於每 30 秒醒來一次什麼都不做，而紀錄裡多一行沒人需要的噪音。
    if shared
        .current()
        .all_upstreams()
        .iter()
        .any(|u| !u.is_literal_ip())
    {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-dns-refresh",
            dns::DnsRefreshService::new(shared.clone(), dns_interval),
        ));
    } else {
        info!("没有域名上游（全是 IP 字面量）—— 不起 DNS 重解析任务");
    }

    // ★ 排在監聽器**之後**加：Pingora 按加入順序起服務，而 HTTP-01 的應答
    //   要有人在連接埠上聽著才有意義。
    if let Some(manager) = acme {
        server.add_service(pingora_core::services::background::background_service(
            "fulcrum-acme",
            acme::AcmeService::new(manager),
        ));
    }

    // ── ★ ★ 就緒信號 + pid 檔案（`Type=notify` 與 `ExecReload` 各自的那半邊）──
    //
    // ⚠ **必須在 `run_forever()` 之前訂閱**：它會 move 掉 `server`。
    // ★ 訂閱的是 `ExecutionPhase::Running`——pingora 在所有 service 都啟動完之後
    //   才發這個相位。換代時這正是要的那一刻：新一代已經把 fd 取走並開始 accept 了，
    //   此刻才寫 pid 檔案、才報就緒，是誠實的。
    process::spawn_readiness(server.watch_execution_phase(), opts.pid_file.clone());

    server.run_forever()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulcrum_runtime::request::HeaderList;
    use std::time::UNIX_EPOCH;

    /// G162：「不建大小寫表也照原樣」兩個方向都有 —— 標題化表裡的名只認那一種寫法，表外的名只認小寫。
    #[test]
    fn h1_name_as_is_两个方向() {
        for yes in [
            "Content-Type",
            "ETag",
            "Alt-Svc",
            "Location",
            "x-frame-options",
            "x-lower",
        ] {
            assert!(h1_name_as_is(yes), "{yes} 该照原样");
        }
        for no in [
            "content-type",
            "etag",
            "CONTENT-LENGTH",
            "X-Frame-Options",
            "X-Lower",
            "bad name",
            "",
        ] {
            assert!(!h1_name_as_is(no), "{no} 不该照原样");
        }
    }

    /// G162：`self_built_header` 之外往自建回應裡插的固定名（`write_with_headers` 的兩個、漏斗的 `Alt-Svc`）
    ///   也都必須照原樣寫得出 —— 否則不建表的那一條會把它們寫變樣。
    #[test]
    fn 自建响应里固定插的名都照原样写得出() {
        for n in ["Content-Length", "Content-Type", "Alt-Svc"] {
            assert!(h1_name_as_is(n), "{n}");
        }
    }

    /// ★ `HeaderValue::from_static` 遇到非法字元會**在請求路徑上 panic** ⇒ 餵給它的固定值釘住（同 `files` 裡的同名單測）。
    #[test]
    fn 喂给_from_static_的固定值都是合法头值() {
        assert!(HeaderValue::from_str(TEXT_PLAIN).is_ok(), "{TEXT_PLAIN}");
    }

    /// G162：什麼時候不建大小寫表。看得見的只有 `case_header_iter`：沒有表時它一個都不給。
    #[test]
    fn self_built_header_名都照原样才不建表() {
        let has_case_map = |extra: &[(&str, &str)]| {
            let extra: Vec<(String, String)> = extra
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let mut h = self_built_header(200, &extra).unwrap();
            h.insert_header("Content-Length", "0").unwrap();
            h.case_header_iter().next().is_some()
        };
        assert!(!has_case_map(&[]), "没有用户的头：不建表");
        assert!(
            !has_case_map(&[("Location", "/x"), ("x-lower", "v")]),
            "都照原样：不建表"
        );
        assert!(!has_case_map(&[("-X-Removed", "")]), "只删不插：不碍事");
        assert!(!has_case_map(&[("+Vary", "Origin")]), "追加看名本身");
        assert!(
            has_case_map(&[("X-Frame-Options", "DENY")]),
            "表外又不是小写：要建表才保得住"
        );
        assert!(
            has_case_map(&[("+content-type", "text/x")]),
            "表里的名写成小写：要建表"
        );
        assert!(
            has_case_map(&[("x-lower", "v"), ("X-Upper", "v")]),
            "一个不照原样就整条建表"
        );
    }

    /// 只為給 [`respond_body`] 一個請求視圖。
    fn ctx<'a>(headers: &'a HeaderList<'a>) -> RequestCtx<'a> {
        RequestCtx {
            host: "a.example",
            port: 80,
            scheme: "http",
            method: "GET",
            path: "/",
            query: "",
            headers,
            remote_ip: None,
            remote_port: 0,
        }
    }

    /// ★ 字面值正文**每個請求都不拷**：兩次呼叫拿到的是同一塊記憶體。
    /// ⚠ 兩份 `Bytes` 必須同時活著 —— 先放掉第一份的話，分配器很可能把同一個位址
    ///   再發給第二份，於是一個每次都拷的實作也會讓指針相等。
    #[test]
    fn 字面量正文两次调用指着同一块内存() {
        let t = Template::parse("literal body, no placeholders");
        let h = HeaderList(&[]);
        let rc = ResponseCtx::default();
        let a = respond_body(&t, &ctx(&h), &rc, &[], UNIX_EPOCH);
        let b = respond_body(&t, &ctx(&h), &rc, &[], UNIX_EPOCH);
        assert_eq!(&a[..], b"literal body, no placeholders");
        assert_eq!(a.as_ptr(), b.as_ptr(), "字面量正文每个请求都新拷了一份");
    }

    /// 反方向：帶佔位符的正文照樣按請求展開 —— ⛔ 不能被當成字面值存住第一次的結果。
    #[test]
    fn 带占位符的正文照样按请求展开() {
        let t = Template::parse("method={method}");
        let h = HeaderList(&[]);
        let rc = ResponseCtx::default();
        let mut c = ctx(&h);
        let a = respond_body(&t, &c, &rc, &[], UNIX_EPOCH);
        c.method = "POST";
        let b = respond_body(&t, &c, &rc, &[], UNIX_EPOCH);
        assert_eq!(&a[..], b"method=GET");
        assert_eq!(&b[..], b"method=POST");
    }

    /// 真跑一次 `process_new_http`（經 pingora 實際調的那個 [`H1Entry`]；只有一個 `respond` 站台，`duplex` 當連接），
    /// 回傳交給下一條請求的 keep-alive：外層 `None` = 這條連接不續。
    async fn reuse_after(raw: &[u8]) -> Option<Option<u64>> {
        let cfg = fulcrum_config::compile_str("t.Fulcrumfile", ":8080 {\n  respond 200 ok\n}\n")
            .config
            .unwrap();
        let app = Arc::new(H1Entry::new(Arc::new(FulcrumApp::new(
            fulcrum_runtime::SharedRuntime::new(Arc::new(Runtime::build(&cfg).unwrap())),
            8080,
            false,
            Arc::new(Http01Store::new()),
            cache::CacheHandle::new(cache::Backend::open(None, 4096)),
            None,
            1,
        ))));
        let (server, mut client) = tokio::io::duplex(64 * 1024);
        let mut session = ServerSession::new_http1(Box::new(server));
        // 與 pingora `apps/mod.rs` 給新連接第一條請求設的一樣。
        session.set_keepalive(Some(KEEPALIVE_SECS));
        tokio::io::AsyncWriteExt::write_all(&mut client, raw)
            .await
            .unwrap();
        let (_tx, shutdown) = tokio::sync::watch::channel(false);
        let reused = app.process_new_http(session, &shutdown).await?;
        let (stream, settings) = reused.consume();
        let mut next = ServerSession::new_http1(stream);
        settings.expect("带着设置").apply_to_session(&mut next);
        Some(next.get_keepalive())
    }

    /// G161 的串接：`process_new_http` 真的經 `keepalive::settle_after_read` 定續不續、續多久。
    /// ⚠ 端到端（`tests/serve` 第 12 節）判不出「那一行被刪掉」—— 刪掉時 pingora 的判定原樣留著、
    ///   關與續照樣全對，丟的是時長（續變成不限時、空閒永遠不關），而 60 s 在端到端裡量不起。
    #[tokio::test]
    async fn process_new_http_续的连接带着空闲窗口交给下一条_不续的不交() {
        assert_eq!(
            reuse_after(b"GET / HTTP/1.1\r\nHost: a.example\r\n\r\n").await,
            Some(Some(KEEPALIVE_SECS)),
            "HTTP/1.1 缺省续，且带着 KEEPALIVE_SECS（⛔ 不是不限时的 0）"
        );
        assert_eq!(
            reuse_after(b"GET / HTTP/1.0\r\nHost: a.example\r\n\r\n").await,
            None,
            "HTTP/1.0 没要求 keep-alive ⇒ 这条连接不续"
        );
    }
}
