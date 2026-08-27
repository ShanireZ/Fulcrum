//! M0 接缝验证服务器。
//!
//! 目的只有一个（PLAN.md §7 M0 / G27）：证明**不由 Pingora 托管的监听器**也能参与它的
//! socket 移交，从而在 `SIGQUIT` + `-u` 优雅升级中零中断。
//!
//! 同一个 `Server` 上挂三类服务：
//!
//! | 服务 | 监听 | 由谁托管 | 证明什么 |
//! |---|---|---|---|
//! | `m0-http`     | TCP 8080 | Pingora `listening::Service` | 原生服务照常工作，且与自建服务共用一张 fd 表不撞键 |
//! | `m0-raw-tcp`  | TCP 8081 | **自建 `Service`** | 自建 TCP 监听器能取到/放回 fd |
//! | `m0-raw-udp`  | UDP 8082 | **自建 `Service`** | ★ 自建 **UDP** 监听器能取到/放回 fd —— 这是整个 M0 最不确定的一条 |
//!
//! QUIC 不在 M0 内（G28）：能移交裸 UDP 的 fd 就能移交 QUIC 的，而把 QUIC 排除在外，
//! 失败时才能立刻分清是接缝的问题还是 QUIC 库的问题。

use std::io::Write;

use pingora_core::server::Server;
use pingora_core::server::configuration::Opt;
use pingora_core::services::listening::Service as ListeningService;

// ★ 四个模块住在同 crate 的 lib 里（`src/lib.rs`），不再是本文件的私有 `mod`。
//   动机是 M1 spike 要复用同一份流量服务与同一个探针——**判据有两份就等于没有判据**，
//   理由写在 `lib.rs` 顶部。这次改动对 M0 的行为是零影响：模块内容一字未动。
use m0_seam::{fd_inspect, http_app, raw_tcp, raw_udp};

/// ★ **默认只绑回环**，不绑 `0.0.0.0`（设计原则 5「默认即安全」）。
///
/// 这里有一个真实的攻击面：`raw-udp` 是一个**回声**服务，而 UDP 源地址可伪造——
/// 一旦它能从外部到达，任何人都能拿它把流量反射给第三方（放大比 1:1，但反射是真的）。
/// 今天它不可达，因为 `docker run` 没有 `-p`，端口只在容器网络里；但那是**部署方式**
/// 在兜底，不是服务本身安全。★ 绑定地址是这条链上唯一由代码控制的一环，就把它收紧。
///
/// 三个测试场景连的都是 `127.0.0.1`，所以收紧不影响任何判据。
/// 需要跨容器访问时用 `M0_BIND_HOST=0.0.0.0` 显式打开——**显式比默认危险要好**。
fn bind_host() -> String {
    std::env::var("M0_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn main() {
    // ★ 每一行都带上 pid。理由不是好看：优雅升级的窗口内**两代进程同时**往同一个
    //   `error.log` 写（老一代还在排空连接），没有 pid 就没有任何办法把某一行钉到
    //   某一代身上。`tests/m0/run.sh` 曾用「升级前的行数」切分日志，那只能切出
    //   「某个时间点之后的所有代」，而断言声称的是「只看第二代」——
    //   今天不出错只是因为老一代恰好不打那两句 INHERITED。
    //
    //   `std::process::id()` 在**每条记录**求值，所以 pingora daemonize 分叉之后
    //   写进 error.log 的都是子进程（也就是 pid 文件里那个）的 pid。
    //
    //   格式照抄 env_logger 的默认形状（`[时间级别目标] 正文`），只在中间插一段 pid，
    //   免得既有那些按 `INFO`/目标名 grep 的判据失配。
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
    // ★ 记下来：自建服务需要区分「首次启动没有 fd 表（正常）」与
    //   「以 -u 升级却拿不到 fd 表（交接失败，必须报错）」。
    let upgrading = opt.upgrade;
    let mut server = Server::new(Some(opt)).expect("failed to create server");
    server.bootstrap();

    // 监听地址三处共用一个 host，默认回环（见 `bind_host()`）。
    let host = bind_host();
    let http_bind = format!("{host}:8080");
    let raw_tcp_bind = format!("{host}:8081");
    let raw_udp_bind = format!("{host}:8082");

    // 1) Pingora 原生监听服务。它自己会把 fd 以 `<host>:8080` 为键放进 fd 表。
    let mut http = ListeningService::new("m0-http".to_string(), http_app::MinimalHttp);
    http.add_tcp(&http_bind);
    server.add_service(http);

    // 2) 自建裸 TCP 服务。键刻意加前缀，避免与 Pingora 原生键空间相撞。
    //
    // ★ `M0_DROP_RAW_TCP=1` 时**不挂它**——用来模拟「配置里删掉了一个监听器」，
    //   于是上一代传来的那个 fd 就成了**未被认领的 fd**。
    //   这是 open-seams.md 里 `f82478ae` 那条风险的可复现形态，由
    //   `tests/m0/unclaimed.sh` 驱动。★ 它验的是**当前（未修）行为**。
    let drop_raw_tcp = std::env::var("M0_DROP_RAW_TCP").as_deref() == Ok("1");
    if drop_raw_tcp {
        log::warn!(
            "M0_DROP_RAW_TCP=1: raw-tcp service NOT started (simulating a removed listener)"
        );
    } else {
        server.add_service(raw_tcp::TcpEchoService::new(&raw_tcp_bind, upgrading));
    }

    // 3) 自建裸 UDP 服务。
    server.add_service(raw_udp::UdpEchoService::new(&raw_udp_bind, upgrading));

    // 4) 只读探查服务：把整张 fd 表打出来。`Server::listen_fds()` 是私有的，
    //    没有它就没法证明「那个 fd 确实还在表里」。
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
