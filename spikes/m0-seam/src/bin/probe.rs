//! M0 探针：在优雅升级窗口前后持续打三类流量，统计中断。
//!
//! 判据（PLAN.md §7 M0 退出条件）：
//!   - `http_failures  == 0`  新建连接的 HTTP 请求全部成功
//!   - `tcp_disconnects == 0` 那条**跨越升级**的长连接一次都没断
//!   - `udp_losses     == 0`  UDP 回声零丢失（跑在回环、无拥塞，丢了就只可能是 socket 失守）
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

/// 每轮新建一条连接 —— 这条打的是 **accept 路径**，升级期间由哪一代 accept 都算成功。
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

/// 一条**贯穿全程**的长连接 —— 这条打的是「升级时已建立的连接会不会被切断」。
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
            // 断了就重连，好让后续统计仍有意义（但 disconnects>0 已经判失败）
            conn = TcpStream::connect(&addr).await.ok();
        }
        tokio::time::sleep(interval).await;
    }
}

/// UDP 回声 —— 这条打的是**自建 UDP 监听器的 fd 有没有被交接过去**。
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
        // ★ ★ 必须**丢弃迟到的旧回声**再判本轮。
        //
        // tokio 的 `UdpSocket::recv` 是 cancel-safe：超时把它取消掉时**数据报并没有被消费**，
        // 仍留在接收队列里。若直接拿下一轮的 recv 结果去比对，读到的会是上一轮的 payload，
        // 比对失败又记一次丢包，而本轮的回声再排到队尾——**一次抖动会被放大成
        // 「剩余所有轮次全部丢包」**，让 `udp_losses == 0` 这道判据变成间歇性红。
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
                // 收到的是别的东西（几乎一定是上一轮迟到的回声）：丢掉，继续等本轮的
                Ok(Ok(_)) => continue,
                // 超时或 socket 出错
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
