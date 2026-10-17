//! M0 接縫驗證伺服器。
//!
//! 目的只有一個（PLAN.md §7 M0 / G27）：證明**不由 Pingora 託管的監聽器**也能參與它的
//! socket 移交，從而在 `SIGQUIT` + `-u` 優雅升級中零中斷。
//!
//! 同一個 `Server` 上掛三類服務：
//!
//! | 服務 | 監聽 | 由誰託管 | 證明什麼 |
//! |---|---|---|---|
//! | `m0-http`     | TCP 8080 | Pingora `listening::Service` | 原生服務照常工作，且與自建服務共用一張 fd 表不撞鍵 |
//! | `m0-raw-tcp`  | TCP 8081 | **自建 `Service`** | 自建 TCP 監聽器能取到/放回 fd |
//! | `m0-raw-udp`  | UDP 8082 | **自建 `Service`** | ★ 自建 **UDP** 監聽器能取到/放回 fd —— 這是整個 M0 最不確定的一條 |
//!
//! QUIC 不在 M0 內（G28）：能移交裸 UDP 的 fd 就能移交 QUIC 的，而把 QUIC 排除在外，
//! 失敗時才能立刻分清是接縫的問題還是 QUIC 庫的問題。

use std::io::Write;

use pingora_core::server::Server;
use pingora_core::server::configuration::Opt;
use pingora_core::services::listening::Service as ListeningService;

// ★ 四個模組住在同 crate 的 lib 裡（`src/lib.rs`），不再是本檔案的私有 `mod`。
//   動機是 M1 spike 要重複使用同一份流量服務與同一個探針——**判據有兩份就等於沒有判據**，
//   理由寫在 `lib.rs` 頂部。這次改動對 M0 的行為是零影響：模組內容一字未動。
use m0_seam::{fd_inspect, http_app, raw_tcp, raw_udp};

/// ★ **預設只綁回環**，不綁 `0.0.0.0`（設計原則 5「預設即安全」）。
///
/// 這裡有一個真實的攻擊面：`raw-udp` 是一個**回聲**服務，而 UDP 源位址可偽造——
/// 一旦它能從外部到達，任何人都能拿它把流量反射給第三方（放大比 1:1，但反射是真的）。
/// 今天它不可達，因為 `docker run` 沒有 `-p`，連接埠只在容器網路裡；但那是**部署方式**
/// 在備援，不是服務本身安全。★ 綁定位址是這條鏈上唯一由程式碼控制的一環，就把它收緊。
///
/// 三個測試場景連的都是 `127.0.0.1`，所以收緊不影響任何判據。
/// 需要跨容器訪問時用 `M0_BIND_HOST=0.0.0.0` 顯式打開——**顯式比預設危險要好**。
fn bind_host() -> String {
    std::env::var("M0_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn main() {
    // ★ 每一行都帶上 pid。理由不是好看：優雅升級的視窗內**兩代處理程序同時**往同一個
    //   `error.log` 寫（老一代還在排空連接），沒有 pid 就沒有任何辦法把某一行釘到
    //   某一代身上。`tests/m0/run.sh` 曾用「升級前的行數」切分紀錄，那隻能切出
    //   「某個時間點之後的所有代」，而斷言聲稱的是「只看第二代」——
    //   今天不出錯只是因為老一代恰好不打那兩句 INHERITED。
    //
    //   `std::process::id()` 在**每條記錄**求值，所以 pingora daemonize 分叉之後
    //   寫進 error.log 的都是子處理程序（也就是 pid 檔案裡那個）的 pid。
    //
    //   格式照抄 env_logger 的預設形狀（`[时间级别目标] 正文`），只在中間插一段 pid，
    //   免得既有那些按 `INFO`/目標名 grep 的判據失配。
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|buf, record| {
            writeln!(
                buf,
                "[{} {:<5} pid={} {}] {}",
                buf.timestamp(),
                record.level(),
                std::process::id(),
                record.target(),
                record.args()
            )
        })
        .init();

    let opt = Opt::parse_args();
    // ★ 記下來：自建服務需要區分「首次啟動沒有 fd 表（正常）」與
    //   「以 -u 升級卻拿不到 fd 表（交接失敗，必須回報錯誤）」。
    let upgrading = opt.upgrade;
    let mut server = Server::new(Some(opt)).expect("failed to create server");
    server.bootstrap();

    // 監聽位址三處共用一個 host，預設迴環（見 `bind_host()`）。
    let host = bind_host();
    let http_bind = format!("{host}:8080");
    let raw_tcp_bind = format!("{host}:8081");
    let raw_udp_bind = format!("{host}:8082");

    // 1) Pingora 原生監聽服務。它自己會把 fd 以 `<host>:8080` 為鍵放進 fd 表。
    let mut http = ListeningService::new("m0-http".to_string(), http_app::MinimalHttp);
    http.add_tcp(&http_bind);
    server.add_service(http);

    // 2) 自建裸 TCP 服務。鍵刻意加前綴，避免與 Pingora 原生鍵空間相撞。
    //
    // ★ `M0_DROP_RAW_TCP=1` 時**不掛它**——用來模擬「設定裡刪掉了一個監聽器」，
    //   於是上一代傳來的那個 fd 就成了**未被認領的 fd**。
    //   這是 open-seams.md 裡 `f82478ae` 那條風險的可重現形態，由
    //   `tests/m0/unclaimed.sh` 驅動。★ 它驗的是**當前（未修）行為**。
    let drop_raw_tcp = std::env::var("M0_DROP_RAW_TCP").as_deref() == Ok("1");
    if drop_raw_tcp {
        log::warn!(
            "M0_DROP_RAW_TCP=1: raw-tcp service NOT started (simulating a removed listener)"
        );
    } else {
        server.add_service(raw_tcp::TcpEchoService::new(&raw_tcp_bind, upgrading));
    }

    // 3) 自建裸 UDP 服務。
    server.add_service(raw_udp::UdpEchoService::new(&raw_udp_bind, upgrading));

    // 4) 唯讀探查服務：把整張 fd 表打出來。`Server::listen_fds()` 是私有的，
    //    沒有它就沒法證明「那個 fd 確實還在表裡」。
    server.add_service(fd_inspect::FdInspectService);

    log::info!(
        "m0-seam up: http={http_bind} raw-tcp={} raw-udp={raw_udp_bind} pid={}",
        if drop_raw_tcp {
            "DROPPED"
        } else {
            raw_tcp_bind.as_str()
        },
        std::process::id()
    );

    server.run_forever();
}
