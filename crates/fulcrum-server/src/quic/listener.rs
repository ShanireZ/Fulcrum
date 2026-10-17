//! QUIC 監聽器（**M2 批 J 第四步**）—— 自建 `Service` + **參與 socket 移交**。
//!
//! # 這一頁分成三層，而分層本身是判據的前提
//!
//! | 層 | 內容 | 測得動嗎 |
//! |---|---|---|
//! | **判定** | [`decide`]：一個資料報進來該做什麼 —— **純函式，不做任何 I/O** | ✅ 單測 |
//! | **設定** | [`build_quic_config`]：G104 的落點，把 `SniResolver` 掛進 quiche | ✅ 單測 |
//! | **I/O** | [`QuicListenerService`]：收包、按判定散佈、per-連接任務 | ✅ 端到端 |
//!
//! ★ ★ **把判定從 I/O 裡拆出來，與 `fulcrum-runtime`「純邏輯不引用 pingora」是同一條理由**：
//! 它一旦長在收包循環裡，就只能靠真流量去測，而那樣紅了指不到具體哪條規則。
//!
//! # 這裡做了什麼
//!
//! - fd 移交、Retry、版本協商、歸屬散佈、**QUIC 傳輸層握手真的能走完**；
//! - **h3 事件循環** —— 見 [`super::h3_conn`]，請求經 [`H3RequestHandler`] 交出去；
//! - **按 G110 串接**：有 `tls` 的連接埠自動開，串接在 `crate::run` 裡，
//!   端到端判據是 `tests/h3/run.sh`；
//! - **[`Action::Relay`] 把資料報送給它那一代**（[`super::relay`]）——
//!   而**老一代收到停機信號之後繼續收轉交，直到自己的連接排空**。
//!
//! # ★ 一條會影響 G109 的硬約束：**本批不簽發額外的連接 ID**
//!
//! 連接表按**我們在 accept 時鑄的那一個 SCID** 索引。這成立的前提是我們**從不**呼叫
//! `Connection::new_source_cid()` —— 一旦簽發額外 CID，用戶端可能換用它，
//! 而那個 CID 不在表裡、也**不一定帶 `gen_id` 前綴**。
//! ⇒ ★ ★ **將來要簽發額外 CID 時，它必須同樣出自 [`GenId::mint_scid`]**，
//! 否則換代時那條連接會認不出來 —— 這與「SCID 前綴必須在批 J 就做」是同一條理由。

use crate::quic::gen_id::{GenId, Ownership, SCID_LEN, ownership};
use crate::quic::h3_conn::{self, H3RequestHandler};
use crate::quic::relay::{RelayInbox, RelayOutbox, Relayed};
use crate::quic::retry::{self, RetryKey};
use async_trait::async_trait;
use boring::ssl::{SslContextBuilder, SslMethod};
use fulcrum_tls::resolver::SniResolver;
use log::{debug, error, info, warn};
use pingora_core::server::{ListenFds, ShutdownWatch};
use pingora_core::services::Service;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// 一個 UDP 資料報的上限。
///
/// ⚠ 取 1500 是不夠的：QUIC 允許對端用更大的 MTU，而**讀短了會把包截斷**，
/// 截斷之後 `Header::from_slice` 仍可能成功 ⇒ 得到一個「看起來正常」的壞包。
/// ⇒ 取 65535（UDP 載荷上限），寧可多佔一點棧外記憶體。
const DATAGRAM_MAX: usize = 65535;

/// 每條連接的入站佇列深度。★ 滿了就丟 —— QUIC 自己會重傳，而無界佇列會被用來打記憶體。
const CONN_QUEUE: usize = 128;

/// 一個資料報被丟棄的原因。
///
/// ⚠ ⚠ ★ **只給紀錄與判據用，絕不影響回給對端的東西** —— 與 [`retry::Reject`] 同一條紀律。
/// ★ 它存在的理由也一樣：斷言「被丟了」**太寬**，一個把所有輸入都丟掉的實作照樣全綠。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropWhy {
    /// 包頭解析不出來。
    Unparseable,
    /// 判不出歸屬（DCID 太短、或伺服器端本就不該收到這種包）。
    Undecidable,
    /// Retry token 沒通過。★ 帶上原因**只為紀錄**。
    BadToken(retry::Reject),
    /// 前綴是本代、但表裡沒有這條連接（老連接的殘包、或已經關掉了）。
    NoSuchConnection,
}

/// 收到一個資料報之後該做什麼。**純判定的產物，不含任何 I/O。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 用戶端要的 QUIC 版本我們不支援 ⇒ 回一個版本協商包。
    NegotiateVersion,
    /// 首度推出 Initial ⇒ 回一個 Retry（位址驗證）。**G109 要求必須開。**
    Retry,
    /// 交給本代已有的那條連接。
    Existing,
    /// 建一條新連接。`odcid` 是從 token 裡取回來的**用戶端第一發 Initial 的 DCID**
    /// （RFC 9000 §7.3 要求它進 `original_destination_connection_id`）。
    Accept { odcid: Vec<u8> },
    /// 屬於另一代 ⇒ 轉交：走 [`super::relay`] 那條 unix datagram 通道（**M2 批 K**）。
    Relay(GenId),
    /// 丟棄，並說清為什麼。
    Drop(DropWhy),
}

/// 一個資料報該怎麼處置 —— **純函式**。
///
/// `known` 回答「本代的連接表裡有沒有這個 DCID」。
///
/// # 判定順序，以及為什麼是這個順序
///
/// 1. **版本**只對 `Initial` 查。⚠ 短包沒有版本欄位（`hdr.version` 是 0），
///    對它查版本會把每一個正常的短包都判成「版本不支援」——
///    ★ 而那個錯誤**看起來完全像是對端的問題**。
/// 2. 然後才是歸屬（[`ownership`]）。
/// 3. `Local` 且表裡有 ⇒ 交給它；表裡沒有 ⇒ 只有「帶 token 的 Initial」才可能是
///    Retry 之後那一發，驗票通過才 accept；其餘一律丟。
pub fn decide(
    me: &GenId,
    key: &RetryKey,
    peer: &SocketAddr,
    now_secs: u64,
    hdr: &quiche::Header<'_>,
    known: &dyn Fn(&[u8]) -> bool,
) -> Action {
    // ① 版本 —— 只對 Initial。
    if hdr.ty == quiche::Type::Initial && !quiche::version_is_supported(hdr.version) {
        return Action::NegotiateVersion;
    }

    let token = hdr.token.as_deref().unwrap_or(&[]);
    let has_token = !token.is_empty();

    // ② 歸屬。
    match ownership(me, hdr.ty, &hdr.dcid, has_token) {
        Ownership::Relay(g) => Action::Relay(g),
        Ownership::Undecidable => Action::Drop(DropWhy::Undecidable),
        // 首度推出 Initial：用戶端自選的 DCID，它還沒有狀態 ⇒ 本代回 Retry。
        Ownership::NewConnection => Action::Retry,
        Ownership::Local => {
            if known(&hdr.dcid) {
                return Action::Existing;
            }
            // 表裡沒有：只有 Retry 之後那一發 Initial 才該在這裡出現。
            if hdr.ty == quiche::Type::Initial && has_token {
                match key.validate(peer, token, now_secs) {
                    Ok(odcid) => Action::Accept { odcid },
                    Err(why) => Action::Drop(DropWhy::BadToken(why)),
                }
            } else {
                Action::Drop(DropWhy::NoSuchConnection)
            }
        }
    }
}

/// 建一份 QUIC 設定 —— ★ ★ ★ **G104 的機械落點就在這裡**。
///
/// h1/h2 入口與 h3 入口掛的是**同一個** [`SniResolver`]，
/// 於是「兩個入口各有一套挑證書實作」在結構上做不到
/// （`crates/fulcrum-server/src/lib.rs` 裡那行 `plan.resolver.install_into(&mut settings)`
/// 與這裡這行 `resolver.install_into(&mut builder)` 收的是**同一個類型**）。
///
/// ⚠ **`SslContextBuilder::new(SslMethod::tls())` 是照著 quiche 自己 `Context::new()` 來的**
/// （它內部就是 `SSL_CTX_new(TLS_method())`）。⚠ 它**不載入 CA 證書** ——
/// quiche 的 `from_boring()` 也不載入，而伺服器端本就不驗用戶端證書。
pub fn build_quic_config(resolver: &Arc<SniResolver>) -> Result<quiche::Config, String> {
    let mut builder =
        SslContextBuilder::new(SslMethod::tls()).map_err(|e| format!("建不出 SSL context：{e}"))?;
    resolver.install_into(&mut builder);

    let mut cfg = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)
        .map_err(|e| format!("建不出 quiche 配置：{e}"))?;

    // ALPN：h3 只有這一個。★ 與 h1/h2 入口那份**有意分開** —— 那邊要
    //   `h2` / `http/1.1` / `acme-tls/1`，而它們在 QUIC 上一個都不成立。
    //   ⚠ `APPLICATION_PROTOCOL` 本身就是 `&[&[u8]]`（＝那張清單），**不要再包一層**。
    cfg.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
        .map_err(|e| format!("设不上 ALPN：{e}"))?;

    // 傳輸參數。★ 這幾個數與 h1/h2 那側的 keep-alive 不是一回事，別照抄。
    cfg.set_max_idle_timeout(30_000);
    cfg.set_max_recv_udp_payload_size(1350);
    cfg.set_max_send_udp_payload_size(1350);
    cfg.set_initial_max_data(10_000_000);
    cfg.set_initial_max_stream_data_bidi_local(1_000_000);
    cfg.set_initial_max_stream_data_bidi_remote(1_000_000);
    cfg.set_initial_max_stream_data_uni(1_000_000);
    cfg.set_initial_max_streams_bidi(100);
    // ⚠ h3 要三條單向流（控制流 + QPACK 編/解碼器），**給少了握不完手**。
    cfg.set_initial_max_streams_uni(100);
    // ⚠ ⚠ **不禁用主動遷移**：用戶端換網（Wi-Fi → 蜂窩）時 QUIC 的整個賣點就在這兒。
    cfg.set_disable_active_migration(false);

    Ok(cfg)
}

/// 取 fd（繼承）或新建 UDP 監聽 socket，並把新建的放回表裡供下一代繼承。
///
/// ⚠ ⚠ **這段與 `l4.rs::build_udp_listener` 逐字同構，而兩份都留著是有意的** ——
/// 那裡的註解每一條都在描述一個**已經發生過**的失效（`ManuallyDrop`、`from_std` 失敗後的
/// 殘餘風險、拿不到 fd 表時不許裝作沒事）。抽成泛型只會把那幾段註解擠走。
/// ★ 唯一的實質差別是 `key` 的前綴：`fulcrum-quic:` 與 `fulcrum-l4-udp:` **必須分開**，
/// 否則同一個連接埠上的兩種服務會互相搶對方的 fd。
async fn build_quic_listener(
    bind: &str,
    key: &str,
    fds: Option<ListenFds>,
    upgrading: bool,
) -> std::io::Result<UdpSocket> {
    let Some(table) = fds else {
        if upgrading {
            return Err(std::io::Error::other(format!(
                "以 -u 升级启动却拿不到 fd 表，无法继承 {bind}：socket 移交已经失败了"
            )));
        }
        warn!("[quic] 没有 fd 表，只能在 {bind} 上新 bind —— ★ 它不会参与下一次 socket 移交");
        return UdpSocket::bind(bind).await;
    };

    // ★ 0.9.0（上游 1d93711）：ListenFds 換成 parking_lot 鎖 ⇒ 守衛不 Send、⛔ 不許跨 `.await`。
    //   只在兩個同步的瞬間持鎖（查表、登記）；bind 期間不持鎖 —— 每個服務的鍵獨佔，不需要按位址串行。
    let inherited = table.lock().get(key).copied();

    if let Some(fd) = inherited {
        info!("[quic] 继承了 UDP 监听 fd={fd}（{key}）—— 升级窗口内这个端口没有重新 bind 过");
        // SAFETY: fd 由上一代經 SCM_RIGHTS 傳來，此處接管其所有權，且同一個鍵只取用一次。
        // ★ ManuallyDrop 的理由與 l4.rs 那兩份完全相同：提前析構會 `close(fd)`，
        //   而表裡那條記錄仍指著這個號碼 —— 號碼隨後會被任意 `open()` 重複使用，
        //   下一次升級就把一個無關的 fd 當成監聽 socket 發給下一代。`Fds` 沒有 `remove()`。
        let std_sock = ManuallyDrop::new(unsafe { std::net::UdpSocket::from_raw_fd(fd) });
        std_sock.set_nonblocking(true)?;
        let owned = ManuallyDrop::into_inner(std_sock);
        return match UdpSocket::from_std(owned) {
            Ok(s) => Ok(s),
            Err(e) => {
                error!(
                    "[quic] UDP from_std 失败：fd={fd} 已被关闭，而 fd 表里 key={key} 仍指向它。\
                     ★ 下一次升级会把这个已失效的号码传给下一代，必须重启进程而不是继续升级。"
                );
                Err(e)
            }
        };
    }

    let sock = UdpSocket::bind(bind).await?;
    let fd = sock.as_raw_fd();
    table.lock().add(key.to_string(), fd);
    info!("[quic] 监听 {bind}（QUIC/UDP），fd={fd} 已登记为 {key}，下一代继承得到");
    Ok(sock)
}

/// 表裡的一條連接。
struct LiveConn {
    /// 入站資料報送給連接任務。
    ///
    /// ★ ★ **每一個資料報都帶上自己的 `(from, to)`**（**M2 批 K**）——
    /// 而不是讓連接任務用建連時那一對。⚠ 兩個理由：
    /// ① 轉交進來的資料報是**別的處理程序**收到的，它的 `from` 只能由訊息自己帶；
    /// ② 順帶把「用戶端換了位址」這件事做對了 —— 此前 `RecvInfo` 恆用建連時那一對，
    ///    一次連接遷移會被算成原來那個對端，而**那是一處此前沒人看見的舊缺口**。
    tx: mpsc::Sender<(Vec<u8>, SocketAddr, SocketAddr)>,
    task: tokio::task::AbortHandle,
}

/// QUIC/h3 的監聽服務。
pub struct QuicListenerService {
    /// 這個 service 用幾個執行緒（**G35 / G140**）。`None` = 跟全域 `conf.threads`。
    ///
    /// ★ 與 pingora `ListeningService.threads` 同名同義 —— 自建 service 也參與
    /// 同一套角色分配，⛔ 不許在這裡另立一套。由 `serve()` 在建好之後設。
    pub threads: Option<usize>,
    bind: String,
    fd_key: String,
    name: String,
    upgrading: bool,
    /// 本代的身分 —— SCID 的前綴與轉交路徑都由它推導（G109 ①）。
    /// ⚠ 欄位名不能叫 `gen`：它在 Rust 2024 edition 裡是保留字。
    gen_id: GenId,
    /// Retry 的簽發金鑰。★ **每代一把**，理由見 [`retry`] 那一頁。
    retry_key: Arc<RetryKey>,
    resolver: Arc<SniResolver>,
    /// 一條 h3 請求交給誰去跑。
    /// ★ 監聽器**不認識** `FulcrumApp` —— 那會讓 HTTP/3 入口反向相依資料面本體。
    handler: Arc<dyn H3RequestHandler>,
    /// 換代轉交 socket 落在哪個目錄（**M2 批 K**，G109 ②）。
    ///
    /// ★ 取的是**換代 socket 的父目錄** —— 兩者本來就是同一件事的兩半
    /// （都只在升級視窗裡有意義），⇒ 一個能換代的部署，這個目錄必然可寫。
    run_dir: std::path::PathBuf,
    /// 這個監聽器的連接計數格（**M2 批 O**）。★ 與 HTTP 那一側共用同一個 `ConnGuard`。
    conn: crate::conn_stats::BoundConn,
}

impl QuicListenerService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bind: String,
        upgrading: bool,
        resolver: Arc<SniResolver>,
        gen_id: GenId,
        handler: Arc<dyn H3RequestHandler>,
        run_dir: std::path::PathBuf,
        conn_reg: Arc<crate::conn_stats::ConnRegistry>,
    ) -> Self {
        // ★ `bind()` 順手宣告這一格 ⇒ 從第一秒起就有一條 `0` 的樣本。
        let conn = conn_reg.bind(crate::conn_stats::Entrypoint::Quic, &bind);
        QuicListenerService {
            // ★ 預設 `None` = 跟全域；由 `serve()` 按角色設（G140）。
            threads: None,
            fd_key: format!("fulcrum-quic:{bind}"),
            name: format!("fulcrum-quic-{bind}"),
            bind,
            upgrading,
            gen_id,
            retry_key: Arc::new(RetryKey::random()),
            resolver,
            handler,
            run_dir,
            conn,
        }
    }
}

/// 換代轉交 socket 落在哪個目錄：**換代 socket 的父目錄**。
///
/// ⚠ ⚠ **這是一條推導，所以它有自己的判據**（見本頁測試）——
/// 而不是「反正 `/run/fulcrum` 就在那兒」。
/// ★ 取父目錄而不是新加一個 CLI 參數，理由是兩者**必然同生共死**：
/// 轉交只在升級視窗裡有意義，而升級本身就要那個目錄可寫。
/// ⇒ 多一個參數只會多一個「兩個路徑配到兩個地方」的失效形態。
///
/// `upgrade_sock` 沒有父目錄（例如只寫了個檔案名）時回 `.` —— ★ 那是它實際會落的地方。
pub fn run_dir_of(upgrade_sock: &str) -> std::path::PathBuf {
    std::path::Path::new(upgrade_sock)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        // ⚠ 鍾在 1970 之前 ⇒ 判成 0。它會讓所有票立刻過期（安全的那一側），
        //   而不是 panic 或恆久有效。
        .unwrap_or(0)
}

#[async_trait]
impl Service for QuicListenerService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        if listeners_per_fd > 1 {
            error!(
                "[quic] listener_tasks_per_fd={listeners_per_fd} 不被 QUIC 服务支持\
                 （它只会开 1 个收包任务）。拒绝启动，以免配置与实际行为不符。"
            );
            return;
        }

        let sock = match build_quic_listener(&self.bind, &self.fd_key, fds, self.upgrading).await {
            Ok(s) => Arc::new(s),
            Err(e) => {
                error!("[quic] 在 {} 上建不出 UDP 监听器：{e}", self.bind);
                return;
            }
        };
        let config = match build_quic_config(&self.resolver) {
            Ok(c) => c,
            Err(e) => {
                error!("[quic] {e}");
                return;
            }
        };

        info!(
            "[quic] 监听 {} （HTTP/3，gen={}）",
            self.bind,
            self.gen_id.hex()
        );

        // ── 換代轉交（**M2 批 K**，G109 ②）────────────────────────────────
        //
        // ⚠ ⚠ **綁不上時說一句、然後照常服務 h3** —— 而這與訪問紀錄那條
        //   「打不開就裝不上」的口徑**有意不同**，理由要寫清楚：
        //   拒絕啟動會把一個 `/run` 的權限問題變成「整個 h3 入口不存在」，
        //   而那損失更大；★ 而這條錯誤在**啟動紀錄裡當場可見**，
        //   不是那種「沒有任何東西會說」的失效。
        // ⚠ 代價說全：沒有轉交口 ⇒ **換代時在飛的 h3 連接會斷**。
        let relay = match RelayInbox::bind(&self.run_dir, &self.gen_id) {
            Ok(inbox) => {
                info!("[quic] 换代转交口：{}", inbox.path().display());
                Some(Relay {
                    inbox,
                    outbox: RelayOutbox::new(&self.run_dir),
                })
            }
            Err(e) => {
                error!(
                    "[quic] 建不出换代转交口（{}/quic-relay-{}.sock）：{e} ——                      h3 照常服务，但**换代时在飞的连接会断**",
                    self.run_dir.display(),
                    self.gen_id.hex()
                );
                None
            }
        };

        serve(
            sock,
            shutdown,
            self.gen_id,
            self.retry_key.clone(),
            config,
            self.handler.clone(),
            relay,
            Some(self.conn.clone()),
        )
        .await;
    }

    /// 這個 service 用幾個執行緒（**G35 / G140**）。⚠ 回傳 `Some` 時 pingora
    /// **不再看**全域 `conf.threads` —— 見 `server/mod.rs` 的
    /// `service.threads().unwrap_or(conf.threads)`。
    fn threads(&self) -> Option<usize> {
        self.threads
    }

    fn name(&self) -> &str {
        &self.name
    }

    /// pingora 0.9.0（上游 `f82478a`）：新一代據此關掉「沒有任何服務認領」的繼承 fd。
    /// ⚠ 必須是 fd 表的**鍵**（帶前綴），⛔ 不是 bind 位址 —— 寫成位址，新一代會先關掉自己要繼承的
    ///   那個 fd；不實作（預設 `None`）會讓**整個處理程序**的清理靜默關閉。釘在單測 `*_服务声明的正是它的_fd_表键`。
    fn listen_addresses(&self) -> Option<Vec<String>> {
        Some(vec![self.fd_key.clone()])
    }
}

/// 一代處理程序的轉交兩半（**M2 批 K**，G109 ②）。
///
/// ★ **收件口與發件口捆在一起交給 [`serve`]**，而不是各傳各的：
/// 一代要麼兩半都有，要麼一半都沒有 —— 拆成兩個 `Option` 會多出兩個說不出意義的狀態。
pub struct Relay {
    pub inbox: RelayInbox,
    pub outbox: RelayOutbox,
}

/// 排空階段的輪詢間隔。
///
/// ⚠ 收到停機信號之後**不再 `recv_from`**，於是那條 `select!` 只剩轉交一支 ——
/// 而如果沒有任何轉交進來，它會一直等下去、永遠不回頭看「連接排空了沒有」。
/// ★ 這個 tick 就是那一眼。
const DRAIN_POLL: Duration = Duration::from_millis(200);

/// 從轉交口收進來的一份資料報，交給本代的連接表。
///
/// # ★ ★ ★ G109 ④「轉交只走一跳」的**結構性**落點
///
/// ⚠ ⚠ **本函式拿不到 `GenId`、`RetryKey`，也拿不到 [`RelayOutbox`]** ——
/// 於是「再判一次歸屬」「再轉交一次」這兩件事在這裡**寫不出來**。
/// ★ 那比一句「記得別再轉交」可靠：環路不是靠自覺避免的，是靠**手上沒有那個東西**。
///
/// 回傳 `true` = 送進了某條連接。`false` = 不認識這個 DCID（丟棄）。
fn dispatch_relayed(
    conns: &HashMap<Vec<u8>, LiveConn>,
    from: SocketAddr,
    to: SocketAddr,
    pkt: &mut [u8],
) -> bool {
    // ⚠ 只為讀 DCID 而解析包頭 —— 解析不出來就丟，**不猜**。
    let Ok(hdr) = quiche::Header::from_slice(pkt, SCID_LEN) else {
        return false;
    };
    match conns.get(hdr.dcid.as_ref()) {
        // ⚠ `try_send` 而不是 `send`：與本代自己收包那條逐字同一條理由 ——
        //   佇列滿了就丟這一個資料報，絕不能把收包循環卡住。
        Some(c) => c.tx.try_send((pkt.to_vec(), from, to)).is_ok(),
        None => false,
    }
}

/// 從轉交口收一份訊息。
///
/// * `None` 收件口不存在或已被摘掉 ⇒ **永遠不完成**（那一支在 `select!` 裡等於不存在）。
/// * `Some(frame)` 收到一份。
/// * 讀出錯 ⇒ `None` ⇒ 呼叫方摘掉它。
///
/// ⚠ 寫成一個 helper 而不是把 `if inbox.is_some()` 塞進 `select!` 的前置條件：
/// 那樣寫在 `inbox` 為 `None` 時會**沒有任何分支可選**，而 `tokio::select!` 在那種情況下
/// 會 panic。★ 這裡讓它「永遠 pending」，於是那一支只是安靜地不參與。
async fn recv_relay(inbox: Option<&RelayInbox>, broken: bool) -> Option<Vec<u8>> {
    let Some(inbox) = inbox.filter(|_| !broken) else {
        return std::future::pending().await;
    };
    let mut buf = vec![0u8; DATAGRAM_MAX + crate::quic::relay::HEADER_LEN];
    match inbox.recv(&mut buf).await {
        Ok(n) => {
            buf.truncate(n);
            Some(buf)
        }
        Err(_) => None,
    }
}

/// 收包循環本體 —— **socket 與設定都由呼叫方給**。
///
/// ★ ★ 把「取 socket」與「在它上面服務」拆開，與本頁開頭那張表是同一條理由：
/// `start_service` 裡那一段要 fd 表、要 `SniResolver`、要一整個 `Service` 生命週期，
/// 而**收包循環本身只需要一個 socket** —— 焊在一起的話，
/// 判據就只能從 `Service` 那一頭進去，那意味著**沒法給它一個自己挑的連接埠**。
/// ⇒ 拆開之後判據可以 bind `127.0.0.1:0`、拿到真連接埠，再拿真用戶端打過來。
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    sock: Arc<UdpSocket>,
    mut shutdown: ShutdownWatch,
    gen_id: GenId,
    retry_key: Arc<RetryKey>,
    mut config: quiche::Config,
    handler: Arc<dyn H3RequestHandler>,
    relay: Option<Relay>,
    // 這個監聽器的連接計數格（**M2 批 O**）。`None` = 不數（單測走這條）。
    // ⚠ 名字不叫 `conn`：這個函式裡 `conn` 已經是**一條 quiche 連接**了。
    conn_stats_slot: Option<crate::conn_stats::BoundConn>,
) {
    let local: SocketAddr = match sock.local_addr() {
        Ok(a) => a,
        Err(e) => {
            error!("[quic] 拿不到本地地址：{e}");
            return;
        }
    };

    let mut conns: HashMap<Vec<u8>, LiveConn> = HashMap::new();
    let mut buf = vec![0u8; DATAGRAM_MAX];
    let mut out = vec![0u8; DATAGRAM_MAX];
    let (inbox, mut outbox) = match relay {
        Some(r) => (Some(r.inbox), Some(r.outbox)),
        None => (None, None),
    };
    // ⚠ ⚠ **收到停機信號之後不是 `break`，是轉進「排空」**（**M2 批 K**）。
    //   ★ 那一段裡本代**不再 `recv_from`**，但**繼續收轉交** ——
    //     否則新一代轉回來的資料報沒人接，而那正是「換代零中斷」要防的事。
    let mut draining = false;
    // ⚠ 轉交口自己讀不下去了（fd 壞了那一類）⇒ 摘掉它，**而不是把整個監聽器停掉**：
    //   ★ 那樣至少 h3 還在服務，只是換代時會斷 —— 比「h3 整個不存在」小得多。
    let mut relay_broken = false;

    loop {
        tokio::select! {
            _ = shutdown.changed(), if !draining => {
                // ★ ★ ★ **與 L4 UDP 那條紀律逐字相同**：停機信號一到就**不再 `recv_from`**。
                //   ⚠ 繼續收的話，老一代會與新一代搶同一個 socket 上的資料報
                //   （兩代持有的是同一個 fd），把本該屬於新一代的首包吃掉。
                //   ⇒ 已建立的連接任務**不動**，它們只 `send_to`，不搶 `recv`。
                info!(
                    "[quic] {} 收到停机信号，停止收包（{} 条连接的任务继续；\
                     转交口继续收，直到它们排空）",
                    local,
                    conns.len()
                );
                draining = true;
            }
            // ★ 排空階段的那一眼：沒有它，只剩轉交一支的 `select!` 會一直等下去，
            //   永遠不回頭看「連接排空了沒有」。
            _ = tokio::time::sleep(DRAIN_POLL), if draining => {}
            // ★ ★ ★ **轉交進來的那一支。** 它**不經過 `decide`** —— G109 ④ 的「只走一跳」。
            //
            // ⚠ ⚠ 這裡收成一個**自有的 `Vec`** 而不是借外面那份緩衝，理由是
            //   `tokio::select!` 的規矩：分支的 future 會一直藉著它捕獲的東西，
            //   而處理體要用同一份緩衝 —— 借用檢查器不讓。
            //   ★ 代價是每一份**轉交進來的**資料報一次分配，而它只在換代視窗裡出現。
            got = recv_relay(inbox.as_ref(), relay_broken) => {
                match got {
                    Some(mut frame) => {
                        let n = frame.len();
                        let ok = match crate::quic::relay::decode(&frame) {
                            Some((from, to, _)) => {
                                // ⚠ 拆完頭之後把載荷**原地**交出去
                                //   （`quiche::Header::from_slice` 要 `&mut [u8]`）。
                                let body = &mut frame[crate::quic::relay::HEADER_LEN..];
                                dispatch_relayed(&conns, from, to, body)
                            }
                            None => {
                                debug!("[quic] 转交口收到一份拆不开的报文（{n} 字节），丢弃");
                                false
                            }
                        };
                        // ⚠ **成功那一半也要說一句** —— 只在失敗時打紀錄的話，
                        //   **「沒有紀錄」既可能是「沒收到」、也可能是「收到並且一切正常」**。
                        if ok {
                            debug!("[quic] 转交进来一个数据报（{n} 字节）⇒ 已送进那条连接");
                        } else {
                            debug!("[quic] 转交进来的数据报没人认领，丢弃（{n} 字节）");
                        }
                    }
                    None => {
                        error!(
                            "[quic] {local} 的转交收件口读不下去了，摘掉它 —— \
                             h3 照常服务，但**换代时在飞的连接会断**"
                        );
                        relay_broken = true;
                    }
                }
            }
            r = sock.recv_from(&mut buf), if !draining => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(e) => {
                        error!("[quic] {local} recv_from 出错：{e}");
                        // fd 耗盡時這個循環會滿速空轉 —— 與 l4.rs 同一條防護。
                        if matches!(e.raw_os_error(), Some(24) | Some(23)) {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                        continue;
                    }
                };
                let pkt = &mut buf[..n];

                let hdr = match quiche::Header::from_slice(pkt, SCID_LEN) {
                    Ok(h) => h,
                    Err(e) => {
                        debug!("[quic] {peer} 的包头解析不出来（{e}），丢弃");
                        continue;
                    }
                };

                let action = {
                    let known = |dcid: &[u8]| conns.contains_key(dcid);
                    decide(
                        &gen_id,
                        &retry_key,
                        &peer,
                        now_secs(),
                        &hdr,
                        &known,
                    )
                };

                match action {
                    Action::NegotiateVersion => {
                        match quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut out) {
                            Ok(len) => {
                                let _ = sock.send_to(&out[..len], peer).await;
                                debug!("[quic] 给 {peer} 回了版本协商（它要 {:x}）", hdr.version);
                            }
                            Err(e) => debug!("[quic] 造不出版本协商包：{e}"),
                        }
                    }
                    Action::Retry => {
                        // ★ 這裡鑄的 SCID 就是用戶端此後要用的 DCID ——
                        //   它**必須帶 gen_id 前綴**，否則換代時那條連接認不出來（G109 ①）。
                        let new_scid = gen_id.mint_scid();
                        let token = match retry_key.mint(&peer, &hdr.dcid, now_secs()) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!("[quic] 签不出 Retry token：{e}");
                                continue;
                            }
                        };
                        match quiche::retry(
                            &hdr.scid, &hdr.dcid, &new_scid, &token, hdr.version, &mut out,
                        ) {
                            Ok(len) => {
                                let _ = sock.send_to(&out[..len], peer).await;
                                debug!("[quic] 给 {peer} 回了 Retry");
                            }
                            Err(e) => debug!("[quic] 造不出 Retry 包：{e}"),
                        }
                    }
                    Action::Existing => {
                        let dcid = hdr.dcid.to_vec();
                        let dead = match conns.get(&dcid) {
                            // ⚠ `try_send` 而不是 `send`：佇列滿了就丟這一個資料報，
                            //   **絕不能把收包循環卡住** —— 一條慢連接會拖垮整個監聽器。
                            //   ★ QUIC 自己會重傳，丟一個是可恢復的。
                            Some(c) => c.tx.try_send((pkt.to_vec(), peer, local)).is_err(),
                            None => false,
                        };
                        if dead {
                            debug!("[quic] {peer} 那条连接的队列满了或任务没了，丢一个数据报");
                        }
                    }
                    Action::Accept { odcid } => {
                        // 用戶端現在用的 DCID 就是我們在 Retry 裡給它的 SCID。
                        let scid = quiche::ConnectionId::from_ref(&hdr.dcid).into_owned();
                        let odcid = quiche::ConnectionId::from_ref(&odcid).into_owned();
                        let conn = match quiche::accept(
                            &scid, Some(&odcid), local, peer, &mut config,
                        ) {
                            Ok(c) => c,
                            Err(e) => {
                                warn!("[quic] accept 失败（{peer}）：{e}");
                                continue;
                            }
                        };
                        let (tx, rx) = mpsc::channel(CONN_QUEUE);
                        // ★ 連接計數（**M2 批 O**）：包一層而不改 `h3_conn::run` 的簽名。
                        let g = conn_stats_slot.as_ref().map(|b| b.guard());
                        let sock_c = sock.clone();
                        let handler_c = handler.clone();
                        let handle = tokio::spawn(async move {
                            // ⚠ ⚠ **必須綁名字**：寫成 `let _ = g;` 會當場 drop，
                            //   於是 active 恆為 0 而 total 照漲，且不會有東西紅。
                            let _g = g;
                            h3_conn::run(conn, sock_c, rx, local, peer, handler_c).await
                        });
                        let key = scid.to_vec();
                        // 首包要立刻餵給它，否則握手不會開始。
                        let _ = tx.try_send((pkt.to_vec(), peer, local));
                        conns.insert(
                            key,
                            LiveConn { tx, task: handle.abort_handle() },
                        );
                        debug!("[quic] 接受了来自 {peer} 的新连接（现共 {} 条）", conns.len());
                    }
                    Action::Relay(g) => {
                        // ✅ ✅ ✅ **M2 批 K：這一支從「丟棄」換成「送給它真正的主人」。**
                        //   ⚠ 批 J 時這裡只有一行 debug，而那一行是「換代零中斷不成立」
                        //     這句話唯一的落點。★ 現在它成立了 —— 改它之前先讀 G109 ②③④。
                        match outbox.as_mut() {
                            Some(ob) => match ob.send(g, &peer, &local, pkt).await {
                                Relayed::Sent => {
                                    debug!("[quic] {peer} 的包转交给了 {}", g.hex());
                                }
                                // ★ 「那一代已經退出」是換代的**常態**：它一走，
                                //   那條連接本來就沒了 ⇒ 丟一個包，不是錯誤。
                                Relayed::Gone => {
                                    debug!(
                                        "[quic] {peer} 的包属于 {}，而那一代已经退出，丢弃",
                                        g.hex()
                                    );
                                }
                                // ⚠ 而這一種要有人看：它不是換代的常態。
                                Relayed::Failed => {
                                    warn!("[quic] 往 {} 转交失败（不是「它已退出」那一种）", g.hex());
                                }
                            },
                            None => debug!(
                                "[quic] {peer} 的包属于另一代（{}），而本代没有转交口，丢弃",
                                g.hex()
                            ),
                        }
                    }
                    Action::Drop(why) => {
                        debug!("[quic] 丢弃来自 {peer} 的数据报：{why:?}");
                    }
                }
            }
            // ⚠ 所有分支都被前置條件關掉時 `tokio::select!` 會 panic ——
            //   這一支是那種情況的出口（沒有轉交口 + 已進入排空）。
            else => break,
        }

        // 順手收掉已經結束的連接任務。★ 不做的話這張表只增不減。
        // ⚠ ⚠ **移到 `select!` 外面**（批 K）：排空階段要靠它才看得見「排完了沒有」，
        //   而它此前只在「收到一個資料報」那一支裡跑。
        conns.retain(|_, c| !c.task.is_finished());

        // ★ 排空完成 —— 本代該走了。
        //   ⚠ 上界不由這裡定：pingora 停機時**不等 service 回傳**，
        //     它 sleep 完寬限期就強關 runtime（`server/mod.rs` 那一段）。
        //     ⇒ 這裡只負責「排完就早點走」，走不掉也不會拖住誰。
        if draining && conns.is_empty() {
            info!("[quic] {local} 的连接已排空，收包任务退出");
            break;
        }
    }
}

/// 判定層的判據。**全部脫網**：一個 socket 都不開，而用戶端那一半是**真的 quiche**。
///
/// ⚠ ⚠ ★ **首包不手動拼**：讓 **quiche 自己**去造 Initial、去消化我們的 Retry
/// （`l4.rs::peek_tests` 讓 BoringSSL 自己造 ClientHello，同一條紀律）。
/// ★ 一份手動拼的訊息只能證明「我對格式的理解與我的實作一致」，那是同義反復。
#[cfg(test)]
mod tests {
    use super::*;
    use fulcrum_tls::cert_key_from_der;
    use pingora_core::protocols::http::ServerSession;
    // ★ `h3::Header` 的 `name()` / `value()` 掛在這個 trait 上。
    use quiche::h3::NameValue;

    const SNI: &str = "h3.test";
    const NOW: u64 = 1_800_000_000;

    fn s_addr() -> SocketAddr {
        "192.0.2.1:443".parse().expect("常量地址")
    }
    fn c_addr() -> SocketAddr {
        "192.0.2.9:44300".parse().expect("常量地址")
    }

    /// 一份自簽證書 + 掛上它的 resolver + 伺服器端 quiche 設定。
    fn server_config() -> quiche::Config {
        let key = rcgen::KeyPair::generate().expect("测试密钥");
        let params = rcgen::CertificateParams::new(vec![SNI.to_string()]).expect("测试参数");
        let cert = params.self_signed(&key).expect("自签");
        let ck = cert_key_from_der(cert.der().to_vec(), key.serialize_der()).expect("造 CertKey");

        let resolver = Arc::new(SniResolver::new());
        // ★ 按名字裝，而不是塞進 default 槽：下面的用戶端**報了 SNI**（`quiche::connect`
        //   的第一個參數），所以這裡走的是與真流量同一條挑證書的路。
        //   ⚠ 之前這裡用的是 `set_default` —— 而那是全倉**唯一**的呼叫方，
        //   於是「default 槽有人填」這件事只在測試裡成立過。
        resolver.install(&[SNI.to_string()], ck);
        build_quic_config(&resolver).expect("服务端配置")
    }

    fn client_config() -> quiche::Config {
        let mut c = quiche::Config::new(quiche::PROTOCOL_VERSION).expect("客户端配置");
        // 自簽證書，測試裡不驗鏈 —— ★ 被測的是**我們的**散佈與握手，不是 PKI。
        c.verify_peer(false);
        c.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
            .expect("客户端 ALPN");
        c.set_max_idle_timeout(5_000);
        c.set_max_recv_udp_payload_size(1350);
        c.set_max_send_udp_payload_size(1350);
        c.set_initial_max_data(10_000_000);
        c.set_initial_max_stream_data_bidi_local(1_000_000);
        c.set_initial_max_stream_data_bidi_remote(1_000_000);
        c.set_initial_max_stream_data_uni(1_000_000);
        c.set_initial_max_streams_bidi(100);
        c.set_initial_max_streams_uni(100);
        c
    }

    /// 造一個真的用戶端，並拿到它的第一個資料報。
    ///
    /// 回傳 `(连接, 首包, 它自选的 DCID)` —— 那個 DCID 就是 RFC 9000 §7.3 的 `odcid`。
    fn client_first_flight(ccfg: &mut quiche::Config) -> (quiche::Connection, Vec<u8>, Vec<u8>) {
        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut conn =
            quiche::connect(Some(SNI), &scid, c_addr(), s_addr(), ccfg).expect("造客户端");
        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n, _) = conn.send(&mut buf).expect("客户端首包");
        let pkt = buf[..n].to_vec();
        let odcid = {
            let mut probe = pkt.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析首包");
            h.dcid.to_vec()
        };
        (conn, pkt, odcid)
    }

    fn judge(me: &GenId, key: &RetryKey, pkt: &[u8], known: &dyn Fn(&[u8]) -> bool) -> Action {
        let mut buf = pkt.to_vec();
        let hdr = quiche::Header::from_slice(&mut buf, SCID_LEN).expect("解析包头");
        decide(me, key, &c_addr(), NOW, &hdr, known)
    }

    fn none_known(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn 真客户端的首包判成_retry() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let mut ccfg = client_config();
        let (_c, pkt, _) = client_first_flight(&mut ccfg);
        assert_eq!(judge(&me, &key, &pkt, &none_known), Action::Retry);
    }

    /// ★★★ 這一條走完 **Retry 那一整輪**，並且釘住 `odcid` 被原樣帶了回來 ——
    /// RFC 9000 §7.3 要求它進 `original_destination_connection_id`，
    /// 少帶或帶錯的話用戶端會在握手末尾判傳輸參數不符而斷開，
    /// ⚠ **而那個失敗看起來像「TLS 出了問題」**。
    #[test]
    fn retry_之后那一发判成_accept_并且_odcid_原样带回() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let mut ccfg = client_config();
        let (mut c, pkt1, odcid) = client_first_flight(&mut ccfg);
        assert_eq!(judge(&me, &key, &pkt1, &none_known), Action::Retry);

        // 我們回一個真的 Retry。
        let new_scid = me.mint_scid();
        let token = key.mint(&c_addr(), &odcid, NOW).expect("签票");
        let mut out = vec![0u8; DATAGRAM_MAX];
        let (scid1, ver) = {
            let mut probe = pkt1.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析");
            (h.scid.to_vec(), h.version)
        };
        let n = quiche::retry(
            &quiche::ConnectionId::from_ref(&scid1),
            &quiche::ConnectionId::from_ref(&odcid),
            &new_scid,
            &token,
            ver,
            &mut out,
        )
        .expect("造 Retry");

        // 交給真用戶端消化 —— ★ 它認不認，是它說了算，不是我們說了算。
        c.recv(
            &mut out[..n],
            quiche::RecvInfo {
                from: s_addr(),
                to: c_addr(),
            },
        )
        .expect("客户端收 Retry");

        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n2, _) = c.send(&mut buf).expect("客户端第二发");
        match judge(&me, &key, &buf[..n2], &none_known) {
            Action::Accept { odcid: got } => assert_eq!(got, odcid, "odcid 没被原样带回来"),
            other => panic!("Retry 之后那一发应当判成 Accept，实际是 {other:?}"),
        }
    }

    /// ★★★ **端到端：一條真的 QUIC 連接握完手。**
    ///
    /// 它證的不是某一條判定，而是**整條鏈條合得上**：判定 → Retry → 判定 → accept →
    /// 握手。⚠ 每一步都可以單獨是綠的而鏈條斷掉（`odcid` 帶錯、ALPN 不匹配、
    /// 單向流給少了）—— 那幾種失敗**全都表現為「握手就是不完成」**，而單測看不見。
    #[test]
    fn 走完_retry_之后真的能握完手() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let mut ccfg = client_config();
        let mut scfg = server_config();

        let (mut c, pkt1, odcid) = client_first_flight(&mut ccfg);
        assert_eq!(judge(&me, &key, &pkt1, &none_known), Action::Retry);

        let new_scid = me.mint_scid();
        let token = key.mint(&c_addr(), &odcid, NOW).expect("签票");
        let mut out = vec![0u8; DATAGRAM_MAX];
        let (scid1, ver) = {
            let mut probe = pkt1.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析");
            (h.scid.to_vec(), h.version)
        };
        let n = quiche::retry(
            &quiche::ConnectionId::from_ref(&scid1),
            &quiche::ConnectionId::from_ref(&odcid),
            &new_scid,
            &token,
            ver,
            &mut out,
        )
        .expect("造 Retry");
        c.recv(
            &mut out[..n],
            quiche::RecvInfo {
                from: s_addr(),
                to: c_addr(),
            },
        )
        .expect("客户端收 Retry");

        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n2, _) = c.send(&mut buf).expect("客户端第二发");
        let got_odcid = match judge(&me, &key, &buf[..n2], &none_known) {
            Action::Accept { odcid } => odcid,
            other => panic!("应当 Accept，实际 {other:?}"),
        };

        // 按判定的產物 accept —— ★ 這幾行與 `start_service` 裡那幾行是同一套。
        let mut s = quiche::accept(
            &new_scid,
            Some(&quiche::ConnectionId::from_ref(&got_odcid)),
            s_addr(),
            c_addr(),
            &mut scfg,
        )
        .expect("accept");
        s.recv(
            &mut buf[..n2],
            quiche::RecvInfo {
                from: c_addr(),
                to: s_addr(),
            },
        )
        .expect("服务端收第二发");

        // 手動把兩邊的資料報倒來倒去，直到都握完手。
        for _ in 0..40 {
            if c.is_established() && s.is_established() {
                break;
            }
            pump(&mut s, &mut c, s_addr(), c_addr());
            pump(&mut c, &mut s, c_addr(), s_addr());
        }

        assert!(s.is_established(), "服务端没握完手：{:?}", s.stats());
        assert!(c.is_established(), "客户端没握完手：{:?}", c.stats());
        assert_eq!(
            s.application_proto(),
            b"h3",
            "ALPN 没协商到 h3 —— 那样 h3 层根本起不来"
        );
        // ★ 而這條連接的 CID **必須帶本代的前綴**，否則換代時它認不出來（G109 ①）。
        assert!(
            me.owns(&new_scid),
            "握成的这条连接，CID 前缀不是本代 —— G109 的整条判定会落空"
        );
    }

    /// 把 `from` 排著的包全部搬給 `to`。
    fn pump(
        from: &mut quiche::Connection,
        to: &mut quiche::Connection,
        from_addr: SocketAddr,
        to_addr: SocketAddr,
    ) {
        let mut buf = vec![0u8; DATAGRAM_MAX];
        loop {
            let (n, _) = match from.send(&mut buf) {
                Ok(v) => v,
                Err(quiche::Error::Done) => return,
                Err(e) => panic!("send 出错：{e}"),
            };
            to.recv(
                &mut buf[..n],
                quiche::RecvInfo {
                    from: from_addr,
                    to: to_addr,
                },
            )
            .expect("recv");
        }
    }

    /// ★★★ **短包不許被判成「版本不支援」。**
    ///
    /// 短包沒有版本欄位（`hdr.version` 是 0），而 0 不在支援列表裡 ——
    /// 少了 `ty == Initial` 那道閘，**每一個正常的短包都會被回一個版本協商**，
    /// ⚠ 而那個錯誤看起來**完全像是對端的問題**。
    #[test]
    fn 短包不许被判成版本不支持() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let cid = me.mint_scid();
        // 一個最小的短包：首位元組最高位為 0（短包），後面跟 DCID。
        let mut pkt = vec![0x40u8];
        pkt.extend_from_slice(&cid);
        pkt.extend_from_slice(&[0u8; 16]);

        let action = judge(&me, &key, &pkt, &none_known);
        assert_ne!(action, Action::NegotiateVersion, "短包被判成了版本不支持");
        // 表裡沒有這條連接 ⇒ 該判「沒有這條連接」，而不是別的。
        assert_eq!(action, Action::Drop(DropWhy::NoSuchConnection));
        // 表裡有的話就該交給它。
        let known = |_: &[u8]| true;
        assert_eq!(judge(&me, &key, &pkt, &known), Action::Existing);
    }

    #[test]
    fn 别代的短包判成_relay() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let other = GenId::random();
        let cid = other.mint_scid();
        let mut pkt = vec![0x40u8];
        pkt.extend_from_slice(&cid);
        pkt.extend_from_slice(&[0u8; 16]);
        assert_eq!(judge(&me, &key, &pkt, &none_known), Action::Relay(other));
    }

    /// 票是別人簽的 / 過期了 / 被改過 ⇒ 一律 `BadToken`，而**不是** `Accept`。
    #[test]
    fn 坏票判成_badtoken_而不是_accept() {
        let (me, key) = (GenId::random(), RetryKey::random());
        let other_key = RetryKey::random();
        let mut ccfg = client_config();
        let (mut c, pkt1, odcid) = client_first_flight(&mut ccfg);

        // 用**另一把鑰匙**簽票，其餘流程一模一樣。
        let new_scid = me.mint_scid();
        let token = other_key.mint(&c_addr(), &odcid, NOW).expect("签票");
        let mut out = vec![0u8; DATAGRAM_MAX];
        let (scid1, ver) = {
            let mut probe = pkt1.clone();
            let h = quiche::Header::from_slice(&mut probe, SCID_LEN).expect("解析");
            (h.scid.to_vec(), h.version)
        };
        let n = quiche::retry(
            &quiche::ConnectionId::from_ref(&scid1),
            &quiche::ConnectionId::from_ref(&odcid),
            &new_scid,
            &token,
            ver,
            &mut out,
        )
        .expect("造 Retry");
        c.recv(
            &mut out[..n],
            quiche::RecvInfo {
                from: s_addr(),
                to: c_addr(),
            },
        )
        .expect("客户端收 Retry");
        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n2, _) = c.send(&mut buf).expect("客户端第二发");

        assert_eq!(
            judge(&me, &key, &buf[..n2], &none_known),
            Action::Drop(DropWhy::BadToken(retry::Reject::BadSeal))
        );
    }

    /// 用戶端要一個我們不支援的 QUIC 版本 ⇒ 回版本協商。
    ///
    /// ⚠ ★ **寫成 `if let Ok(hdr) = Header::from_slice(…) { assert!(…) }` 是錯的** ——
    /// 解析失敗時它一條斷言都不跑，而測試照樣是綠的。
    /// ★ **一道門寫完後要問：它自己有沒有靜默分支。**
    /// ⇒ 這一條用**真用戶端發一個真的壞版本**，它沒有可以靜默的分支。
    /// （「DCID 太短」那一條由 `gen_id::tests::dcid_短于八字节判不出来而不是判成本代`
    /// 直接釘在 `ownership` 上，而 `decide` 的版本閘只對 `Initial` 生效、繞不過它。）
    #[test]
    fn 不支持的版本判成版本协商() {
        let (me, key) = (GenId::random(), RetryKey::random());
        // ★ `0xbabababa` 是 quiche 自己文件裡用的那個「保留給測試」的版本號。
        let mut ccfg = quiche::Config::new(0xbabababa).expect("坏版本的客户端配置");
        ccfg.verify_peer(false);
        ccfg.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
            .expect("ALPN");

        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut c = quiche::connect(Some(SNI), &scid, c_addr(), s_addr(), &mut ccfg)
            .expect("造坏版本客户端");
        let mut buf = vec![0u8; DATAGRAM_MAX];
        let (n, _) = c.send(&mut buf).expect("首包");

        assert_eq!(
            judge(&me, &key, &buf[..n], &none_known),
            Action::NegotiateVersion
        );
        // ★ 反向的一半：**同一條連線路徑上**版本對的用戶端不能也被判成版本協商，
        //   否則這條判據與「恆回傳 NegotiateVersion」無法區分。
        let mut ok_cfg = client_config();
        let (_c2, pkt_ok, _) = client_first_flight(&mut ok_cfg);
        assert_eq!(judge(&me, &key, &pkt_ok, &none_known), Action::Retry);
    }

    #[test]
    fn 配置真的把_h3_设成了_alpn() {
        // ★ 直接的判據是上面那條端到端裡 `application_proto() == b"h3"`；
        //   這裡只確認設定本身建得出來（證書裝得上、context 建得成）。
        let _ = server_config();
    }

    // ── 端到端：真 UDP、真 quiche 用戶端、真 h3 請求與回應 ──────────────────
    //
    // ★ ★ ★ 上面那些判據各自只聲稱自己那一格；這一條把**整條鏈**跑通：
    //   收包 → 判定 → Retry → accept → 握手 → h3 層 → 翻請求頭 → 執行鏈 →
    //   翻回應頭（**濾掉逐跳頭**）→ 發體 → 用戶端讀到。
    // ⚠ 走的是 `127.0.0.1` 上一對真 socket，**連接埠由核心挑**（bind `:0` 再問它） ——
    //   寫死連接埠會在並行跑測試時互相撞，而那種紅看起來像產品缺陷。

    /// 判據用的處理器：回一個固定回應，並把它看見的請求頭**回傳**出來。
    ///
    /// ⚠ ⚠ ★ 它**故意插一個 `connection` 逐跳頭** —— 用戶端如果看得見它，
    /// 說明 RFC 9114 §4.2 那道過濾器在真連線路徑上沒生效
    /// （而單測裡它是綠的：那隻證明函式本身對，證不了它被接在鏈上）。
    struct EchoHandler;

    #[async_trait]
    impl H3RequestHandler for EchoHandler {
        async fn handle(&self, mut session: ServerSession) {
            let path = session.req_header().uri.path().to_string();
            let host = session
                .req_header()
                .headers
                .get("host")
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                .unwrap_or_default();
            let ua = session
                .req_header()
                .headers
                .get("user-agent")
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                .unwrap_or_default();

            let mut resp = pingora_http::ResponseHeader::build(200, None).expect("造响应头");
            resp.insert_header("content-type", "text/plain")
                .expect("插");
            resp.insert_header("x-seen-path", path).expect("插");
            resp.insert_header("x-seen-host", host).expect("插");
            resp.insert_header("x-seen-ua", ua).expect("插");
            // ★ 故意的：它必須在用戶端那側消失。
            resp.insert_header("connection", "keep-alive").expect("插");

            let _ = session.write_response_header(Box::new(resp)).await;
            let _ = session
                .write_response_body(bytes::Bytes::from_static(b"hello from h3"), true)
                .await;
            let _ = session.finish().await;
        }
    }

    /// 用戶端讀到的東西。
    struct Got {
        status: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    /// 一個最小的真 h3 用戶端：連上去、發一個請求、把回應讀回來。
    async fn one_h3_request(server: SocketAddr, path: &str) -> Got {
        let sock = UdpSocket::bind("127.0.0.1:0").await.expect("客户端 socket");
        let local = sock.local_addr().expect("客户端地址");
        let mut cfg = client_config();
        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut conn =
            quiche::connect(Some(SNI), &scid, local, server, &mut cfg).expect("客户端连接");

        let h3_cfg = quiche::h3::Config::new().expect("h3 配置");
        let mut h3: Option<quiche::h3::Connection> = None;
        let mut sent = false;
        let mut got = Got {
            status: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let mut done = false;
        let mut out = vec![0u8; DATAGRAM_MAX];
        let mut inbuf = vec![0u8; DATAGRAM_MAX];

        while !done {
            // ① 發。
            loop {
                match conn.send(&mut out) {
                    Ok((n, info)) => {
                        sock.send_to(&out[..n], info.to).await.expect("客户端发包");
                    }
                    Err(quiche::Error::Done) => break,
                    Err(e) => panic!("客户端 send 出错：{e}"),
                }
            }
            if conn.is_closed() {
                break;
            }
            // ② h3 層 + 發請求。
            if h3.is_none() && conn.is_established() {
                h3 = Some(
                    quiche::h3::Connection::with_transport(&mut conn, &h3_cfg).expect("客户端 h3"),
                );
            }
            if let Some(h) = h3.as_mut()
                && !sent
            {
                let req = [
                    quiche::h3::Header::new(b":method", b"GET"),
                    quiche::h3::Header::new(b":scheme", b"https"),
                    quiche::h3::Header::new(b":authority", SNI.as_bytes()),
                    quiche::h3::Header::new(b":path", path.as_bytes()),
                    quiche::h3::Header::new(b"user-agent", b"fulcrum-test"),
                ];
                h.send_request(&mut conn, &req, true).expect("客户端发请求");
                sent = true;
                continue; // 讓它先發出去
            }
            // ③ 收 h3 事件。
            if let Some(h) = h3.as_mut() {
                loop {
                    match h.poll(&mut conn) {
                        Ok((sid, quiche::h3::Event::Headers { list, .. })) => {
                            let _ = sid;
                            for x in list {
                                let n = String::from_utf8_lossy(x.name()).into_owned();
                                let v = String::from_utf8_lossy(x.value()).into_owned();
                                if n == ":status" {
                                    got.status = v;
                                } else {
                                    got.headers.push((n, v));
                                }
                            }
                        }
                        Ok((sid, quiche::h3::Event::Data)) => {
                            let mut chunk = vec![0u8; 4096];
                            while let Ok(n) = h.recv_body(&mut conn, sid, &mut chunk) {
                                got.body.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Ok((_, quiche::h3::Event::Finished)) => done = true,
                        Ok(_) => {}
                        Err(quiche::h3::Error::Done) => break,
                        Err(e) => panic!("客户端 poll 出错：{e}"),
                    }
                }
            }
            if done {
                break;
            }
            // ④ 等一個資料報，或者等定時器。
            let t = conn.timeout().unwrap_or(Duration::from_millis(20));
            match tokio::time::timeout(t, sock.recv_from(&mut inbuf)).await {
                Ok(Ok((n, from))) => {
                    let info = quiche::RecvInfo { from, to: local };
                    let _ = conn.recv(&mut inbuf[..n], info);
                }
                Ok(Err(e)) => panic!("客户端 recv 出错：{e}"),
                Err(_) => conn.on_timeout(),
            }
        }
        got
    }

    /// ★★★ **整條鏈跑通：一個真的 h3 請求進來，一個真的 h3 回應回去。**
    #[tokio::test]
    async fn 一个真的_h3_请求端到端走通() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("服务端 socket"));
        let server = sock.local_addr().expect("服务端地址");
        let (_tx, shutdown) = tokio::sync::watch::channel(false);
        let cfg = server_config();
        let gen_id = GenId::random();

        let srv = tokio::spawn(serve(
            sock,
            shutdown,
            gen_id,
            Arc::new(RetryKey::random()),
            cfg,
            Arc::new(EchoHandler),
            // ★ 這兩條端到端只驗 h3 本體，不驗換代 ⇒ 不給轉交口。
            //   ⚠ 而「沒有轉交口時 select! 不許 panic」正好被它們順帶釘住。
            None,
            None,
        ));

        let got = tokio::time::timeout(Duration::from_secs(20), one_h3_request(server, "/hi?x=1"))
            .await
            .expect("端到端超时了（20s）—— 链条某处没走通");

        srv.abort();

        assert_eq!(got.status, "200");
        assert_eq!(got.body, b"hello from h3");

        let find = |k: &str| {
            got.headers
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        // ★ 請求頭真的翻譯過去了（:path / :authority→host / 普通頭）。
        assert_eq!(find("x-seen-path").as_deref(), Some("/hi"));
        assert_eq!(find("x-seen-host").as_deref(), Some(SNI));
        assert_eq!(find("x-seen-ua").as_deref(), Some("fulcrum-test"));
        // ★ 回應頭也翻回來了。
        assert_eq!(find("content-type").as_deref(), Some("text/plain"));
        // ★ ★ ★ 而處理器**故意插的那個逐跳頭必須消失** ——
        //   單測裡那條過濾器是綠的，但那隻證明函式本身對，證不了它接在鏈上。
        assert!(
            find("connection").is_none(),
            "逐跳头 connection 漏到了客户端 —— RFC 9114 §4.2 的过滤器没接在链上"
        );
    }

    /// 把請求體原樣回顯的處理器。
    struct EchoBodyHandler;

    #[async_trait]
    impl H3RequestHandler for EchoBodyHandler {
        async fn handle(&self, mut session: ServerSession) {
            let mut body = Vec::new();
            // ⚠ 讀到 `None` 為止 —— 少讀一次就少一段。
            while let Ok(Some(chunk)) = session.read_request_body().await {
                body.extend_from_slice(&chunk);
            }
            let mut resp = pingora_http::ResponseHeader::build(200, None).expect("造响应头");
            resp.insert_header("x-body-len", body.len().to_string())
                .expect("插");
            let _ = session.write_response_header(Box::new(resp)).await;
            let _ = session
                .write_response_body(bytes::Bytes::from(body), true)
                .await;
            let _ = session.finish().await;
        }
    }

    /// 用戶端：發一個帶體的請求，把回應體讀回來。
    async fn one_h3_post(server: SocketAddr, payload: &[u8]) -> Got {
        let sock = UdpSocket::bind("127.0.0.1:0").await.expect("客户端 socket");
        let local = sock.local_addr().expect("客户端地址");
        let mut cfg = client_config();
        let mut scid = [0u8; SCID_LEN];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut scid);
        let scid = quiche::ConnectionId::from_ref(&scid).into_owned();
        let mut conn =
            quiche::connect(Some(SNI), &scid, local, server, &mut cfg).expect("客户端连接");

        let h3_cfg = quiche::h3::Config::new().expect("h3 配置");
        let mut h3: Option<quiche::h3::Connection> = None;
        let mut req_sid: Option<u64> = None;
        let mut left = payload;
        let mut got = Got {
            status: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let mut done = false;
        let mut out = vec![0u8; DATAGRAM_MAX];
        let mut inbuf = vec![0u8; DATAGRAM_MAX];

        while !done {
            loop {
                match conn.send(&mut out) {
                    Ok((n, info)) => {
                        sock.send_to(&out[..n], info.to).await.expect("客户端发包");
                    }
                    Err(quiche::Error::Done) => break,
                    Err(e) => panic!("客户端 send 出错：{e}"),
                }
            }
            if conn.is_closed() {
                break;
            }
            if h3.is_none() && conn.is_established() {
                h3 = Some(
                    quiche::h3::Connection::with_transport(&mut conn, &h3_cfg).expect("客户端 h3"),
                );
            }
            if let Some(h) = h3.as_mut() {
                if req_sid.is_none() {
                    let req = [
                        quiche::h3::Header::new(b":method", b"POST"),
                        quiche::h3::Header::new(b":scheme", b"https"),
                        quiche::h3::Header::new(b":authority", SNI.as_bytes()),
                        quiche::h3::Header::new(b":path", b"/echo"),
                    ];
                    req_sid = Some(h.send_request(&mut conn, &req, false).expect("发请求头"));
                }
                // ★ ★ 用戶端這一側同樣會短寫 —— 寫不完就留著下一輪。
                if let Some(sid) = req_sid
                    && !left.is_empty()
                {
                    match h.send_body(&mut conn, sid, left, left.len() <= 1) {
                        Ok(n) => left = &left[n..],
                        Err(quiche::h3::Error::Done) => {}
                        Err(e) => panic!("客户端发体出错：{e}"),
                    }
                    if left.is_empty() {
                        // 體發完了，補一個空的 fin。
                        let _ = h.send_body(&mut conn, sid, &[], true);
                    }
                }
                loop {
                    match h.poll(&mut conn) {
                        Ok((_, quiche::h3::Event::Headers { list, .. })) => {
                            for x in list {
                                let n = String::from_utf8_lossy(x.name()).into_owned();
                                let v = String::from_utf8_lossy(x.value()).into_owned();
                                if n == ":status" {
                                    got.status = v;
                                } else {
                                    got.headers.push((n, v));
                                }
                            }
                        }
                        Ok((sid, quiche::h3::Event::Data)) => {
                            let mut chunk = vec![0u8; 8192];
                            while let Ok(n) = h.recv_body(&mut conn, sid, &mut chunk) {
                                got.body.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Ok((_, quiche::h3::Event::Finished)) => done = true,
                        Ok(_) => {}
                        Err(quiche::h3::Error::Done) => break,
                        Err(e) => panic!("客户端 poll 出错：{e}"),
                    }
                }
            }
            if done {
                break;
            }
            let t = conn.timeout().unwrap_or(Duration::from_millis(10));
            match tokio::time::timeout(t, sock.recv_from(&mut inbuf)).await {
                Ok(Ok((n, from))) => {
                    let info = quiche::RecvInfo { from, to: local };
                    let _ = conn.recv(&mut inbuf[..n], info);
                }
                Ok(Err(e)) => panic!("客户端 recv 出错：{e}"),
                Err(_) => conn.on_timeout(),
            }
        }
        got
    }

    /// ★★★ **一次覆蓋兩條已寫進文件、而此前一條判據都沒有的陷阱**：
    ///
    /// 1. `recv_body` **必須讀到 `Done`** —— `Data` 事件不會重新武裝，
    ///    少讀一次這條流剩下的體就再也不會來通知；
    /// 2. `send_body` **會短寫** —— 不留待發佇列的話，**大回應會被靜默截斷**。
    ///
    /// ⚠ 兩者的失效表現**都是「內容少了一截，而連接看起來好好的」** ——
    /// 小回應的判據（上面那條）**原理上看不見它們**：一次就寫完了、一次就讀完了。
    /// ⇒ 體積必須**大於一次收發能裝下的量**，這裡取 **256 KiB**
    /// （遠大於 `BODY_CHUNK` 的 16 KiB，也大於初始流控視窗）。
    #[tokio::test]
    async fn 大体积请求与响应不被截断() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("服务端 socket"));
        let server = sock.local_addr().expect("服务端地址");
        let (_tx, shutdown) = tokio::sync::watch::channel(false);

        let srv = tokio::spawn(serve(
            sock,
            shutdown,
            GenId::random(),
            Arc::new(RetryKey::random()),
            server_config(),
            Arc::new(EchoBodyHandler),
            None,
            None,
        ));

        // 一段可檢核的載荷：每個位元組都由它的位置決定 ⇒ **錯位也看得出來**，
        // 而不只是「長度對不對」。
        let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();

        let got = tokio::time::timeout(Duration::from_secs(30), one_h3_post(server, &payload))
            .await
            .expect("大体积端到端超时了（30s）");

        srv.abort();

        assert_eq!(got.status, "200");
        let seen = got
            .headers
            .iter()
            .find(|(n, _)| n == "x-body-len")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(
            seen,
            payload.len().to_string(),
            "服务端收到的请求体长度不对 —— `recv_body` 多半没读到 Done"
        );
        assert_eq!(
            got.body.len(),
            payload.len(),
            "客户端收到的响应体长度不对 —— `send_body` 的短写多半没留待发队列"
        );
        assert!(
            got.body == payload,
            "回显的内容与发出去的不一致（错位或串了）"
        );
    }

    // ── M2 批 K：轉交串接的兩條判據 ─────────────────────────────────────────

    /// `run_dir` 是**推導**出來的，所以它有自己的判據。
    ///
    /// ⚠ 「取 `upgrade_sock` 的父目錄」是一條**推導**，要麼坐實它、要麼加一個顯式欄位，
    /// 別預設它成立 —— 這條判據就是坐實它的那一半。
    #[test]
    fn 转交目录取的是换代_socket_的父目录() {
        use std::path::Path;
        assert_eq!(
            run_dir_of("/run/fulcrum/upgrade.sock"),
            Path::new("/run/fulcrum")
        );
        // 判據裡兩個執行個體共用一個 $WORK，各給一個不同的 sock 名 —— 那時目錄是同一個，
        // ★ 而它們的 `gen_id` 不同 ⇒ 路徑仍然不撞。
        assert_eq!(run_dir_of("/tmp/x/a.sock"), Path::new("/tmp/x"));
        // ⚠ 沒有父目錄時回 `.` —— **那是它實際會落的地方**，不是一個佔位。
        assert_eq!(run_dir_of("upgrade.sock"), Path::new("."));
        assert_eq!(run_dir_of(""), Path::new("."));
    }

    /// ★ ★ ★ G109 ④「轉交只走一跳」—— **行為那一半**。
    ///
    /// 結構那一半由 [`dispatch_relayed`] 的簽名保證（它拿不到 `GenId` / `RetryKey` /
    /// `RelayOutbox`，所以「再轉交一次」寫不出來）；這一條量的是它**認不出來就丟**，
    /// ⚠ 而不是「順手當成新連接」——那樣一個畸形資料報就能讓我們建一份狀態。
    #[tokio::test]
    async fn 转交进来的包认不出_dcid_就丢_而不是走别的路() {
        let conns: HashMap<Vec<u8>, LiveConn> = HashMap::new();
        let from: SocketAddr = "192.0.2.9:44300".parse().expect("测试地址");
        let to: SocketAddr = "192.0.2.1:443".parse().expect("测试地址");

        // ① 完全不是 QUIC 包 ⇒ 包頭解析不出來 ⇒ 丟。
        let mut junk = *b"not-a-quic-datagram-at-all";
        assert!(!dispatch_relayed(&conns, from, to, &mut junk));

        // ② 是個像樣的短包頭、DCID 也夠長，但**沒有人認領** ⇒ 丟。
        //   ★ 這一條才是「只走一跳」的正體：認不出來時**沒有任何後路**。
        let mut short = vec![0u8; 1 + SCID_LEN + 8];
        short[0] = 0x40; // 短包頭（固定位置 1、長包頭位 0）
        short[1..1 + SCID_LEN].copy_from_slice(&[0xab; SCID_LEN]);
        assert!(!dispatch_relayed(&conns, from, to, &mut short));

        // ③ 反向的一半：**認得出來的就該送進去** —— 否則上面兩條與「恆回 false」無法區分。
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(async {});
        let mut conns2: HashMap<Vec<u8>, LiveConn> = HashMap::new();
        conns2.insert(
            vec![0xab; SCID_LEN],
            LiveConn {
                tx,
                task: task.abort_handle(),
            },
        );
        let mut ok = short.clone();
        assert!(
            dispatch_relayed(&conns2, from, to, &mut ok),
            "认得出的 DCID 必须送进那条连接 —— 否则上面两条什么都没证明"
        );
        let (_pkt, f, t) = rx.recv().await.expect("那条连接该收到它");
        // ★ ★ 送進去的是**訊息自帶的那一對位址**，不是建連時那一對 ——
        //   轉交進來的資料報是別的處理程序收到的，只有它自己知道 `from`。
        assert_eq!((f, t), (from, to));
    }

    /// ★ ★ pingora 0.9.0（上游 `f82478a`）：QUIC 服務也必須宣告它的 fd 表鍵 —— 只要開了
    /// TLS 連接埠就會掛上它，而它回傳 `None`（預設）就會讓**整個處理程序**的未認領 fd 清理靜默關閉。
    #[test]
    fn quic_服务声明的正是它的_fd_表键() {
        let s = QuicListenerService::new(
            "127.0.0.1:19003".to_string(),
            false,
            Arc::new(SniResolver::new()),
            GenId::random(),
            Arc::new(EchoHandler),
            std::env::temp_dir(),
            crate::conn_stats::ConnRegistry::new(),
        );
        assert_eq!(
            s.listen_addresses(),
            Some(vec![s.fd_key.clone()]),
            "QUIC 服务必须声明它的 fd 表键（缺省的 None 会关掉整个进程的清理）"
        );
        assert_eq!(s.fd_key, "fulcrum-quic:127.0.0.1:19003");
    }
}
