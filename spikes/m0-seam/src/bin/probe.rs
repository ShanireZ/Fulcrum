//! M0 探針：在優雅升級視窗前後持續打三類流量，統計中斷。
//!
//! 判據（PLAN.md §7 M0 退出條件）：
//!   - `http_failures  == 0`  新建連接的 HTTP 請求全部成功
//!   - `tcp_disconnects == 0` 那條**跨越升級**的長連接一次都沒斷
//!   - `udp_losses     == 0`  UDP 回聲零丟失（跑在迴環、無擁塞，丟了就只可能是 socket 失守）
//!
//! 用法：
//!   m0-probe <duration_ms> <interval_ms> <http_addr> <tcp_addr> <udp_addr>

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

#[derive(Default)]
struct Counters {
    http_ok: AtomicU64,
    http_failures: AtomicU64,
    tcp_ok: AtomicU64,
    tcp_disconnects: AtomicU64,
    udp_ok: AtomicU64,
    udp_losses: AtomicU64,
}

fn arg(n: usize, default: &str) -> String {
    std::env::args()
        .nth(n)
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let duration = Duration::from_millis(arg(1, "6000").parse().expect("duration_ms"));
    let interval = Duration::from_millis(arg(2, "20").parse().expect("interval_ms"));
    let http_addr = arg(3, "127.0.0.1:8080");
    let tcp_addr = arg(4, "127.0.0.1:8081");
    let udp_addr = arg(5, "127.0.0.1:8082");

    let c = Arc::new(Counters::default());
    let deadline = Instant::now() + duration;

    let h = tokio::spawn(http_probe(c.clone(), http_addr, interval, deadline));
    let t = tokio::spawn(tcp_probe(c.clone(), tcp_addr, interval, deadline));
    let u = tokio::spawn(udp_probe(c.clone(), udp_addr, interval, deadline));

    let _ = tokio::join!(h, t, u);

    let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
    println!(
        "{{\"http_ok\":{},\"http_failures\":{},\"tcp_ok\":{},\"tcp_disconnects\":{},\"udp_ok\":{},\"udp_losses\":{}}}",
        g(&c.http_ok),
        g(&c.http_failures),
        g(&c.tcp_ok),
        g(&c.tcp_disconnects),
        g(&c.udp_ok),
        g(&c.udp_losses),
    );
}

/// 每輪新建一條連接 —— 這條打的是 **accept 路徑**，升級期間由哪一代 accept 都算成功。
async fn http_probe(c: Arc<Counters>, addr: String, interval: Duration, deadline: Instant) {
    let req = format!("GET /m0 HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    while Instant::now() < deadline {
        let round = async {
            let mut s = TcpStream::connect(&addr).await.ok()?;
            s.write_all(req.as_bytes()).await.ok()?;
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).await.ok()?;
            if n > 0 && buf[..n].starts_with(b"HTTP/1.1 200") {
                Some(())
            } else {
                None
            }
        };
        match tokio::time::timeout(Duration::from_secs(2), round).await {
            Ok(Some(())) => c.http_ok.fetch_add(1, Ordering::Relaxed),
            _ => c.http_failures.fetch_add(1, Ordering::Relaxed),
        };
        tokio::time::sleep(interval).await;
    }
}

/// 一條**貫穿全程**的長連接 —— 這條打的是「升級時已建立的連接會不會被切斷」。
async fn tcp_probe(c: Arc<Counters>, addr: String, interval: Duration, deadline: Instant) {
    let mut conn: Option<TcpStream> = TcpStream::connect(&addr).await.ok();
    if conn.is_none() {
        c.tcp_disconnects.fetch_add(1, Ordering::Relaxed);
    }
    let mut seq: u64 = 0;

    while Instant::now() < deadline {
        seq += 1;
        let payload = format!("seq={seq}\n");

        let alive = match conn.as_mut() {
            None => false,
            Some(s) => {
                let exchange = async {
                    s.write_all(payload.as_bytes()).await.ok()?;
                    let mut buf = vec![0u8; payload.len()];
                    s.read_exact(&mut buf).await.ok()?;
                    if buf == payload.as_bytes() {
                        Some(())
                    } else {
                        None
                    }
                };
                matches!(
                    tokio::time::timeout(Duration::from_secs(2), exchange).await,
                    Ok(Some(()))
                )
            }
        };

        if alive {
            c.tcp_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            c.tcp_disconnects.fetch_add(1, Ordering::Relaxed);
            // 斷了就重連，好讓後續統計仍有意義（但 disconnects>0 已經判失敗）
            conn = TcpStream::connect(&addr).await.ok();
        }
        tokio::time::sleep(interval).await;
    }
}

/// UDP 回聲 —— 這條打的是**自建 UDP 監聽器的 fd 有沒有被交接過去**。
async fn udp_probe(c: Arc<Counters>, addr: String, interval: Duration, deadline: Instant) {
    let sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => {
            c.udp_losses.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    if sock.connect(&addr).await.is_err() {
        c.udp_losses.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let mut seq: u64 = 0;
    let mut buf = [0u8; 512];

    while Instant::now() < deadline {
        seq += 1;
        let payload = format!("seq={seq}");
        if sock.send(payload.as_bytes()).await.is_err() {
            c.udp_losses.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(interval).await;
            continue;
        }
        // ★ ★ 必須**丟棄遲到的舊回聲**再判本輪。
        //
        // tokio 的 `UdpSocket::recv` 是 cancel-safe：超時把它取消掉時**資料報並沒有被消費**，
        // 仍留在接收佇列裡。若直接拿下一輪的 recv 結果去比對，讀到的會是上一輪的 payload，
        // 比對失敗又記一次丟包，而本輪的回聲再排到隊尾——**一次抖動會被放大成
        // 「剩餘所有輪次全部丟包」**，讓 `udp_losses == 0` 這道判據變成間歇性紅。
        let want = payload.as_bytes();
        let round_deadline = Instant::now() + Duration::from_millis(1000);
        let mut matched = false;
        loop {
            let left = round_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, sock.recv(&mut buf)).await {
                Ok(Ok(n)) if &buf[..n] == want => {
                    matched = true;
                    break;
                }
                // 收到的是別的東西（幾乎一定是上一輪遲到的回聲）：丟掉，繼續等本輪的
                Ok(Ok(_)) => continue,
                // 超時或 socket 出錯
                _ => break,
            }
        }
        if matched {
            c.udp_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            c.udp_losses.fetch_add(1, Ordering::Relaxed);
        }
        tokio::time::sleep(interval).await;
    }
}
