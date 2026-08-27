//! musl + BoringSSL 静态链接探针（G103/G104 的未验前置，`PLAN.md` §10）。
//!
//! ★ ★ ★ **这份探针要答的不是「能不能链接」，是「链上之后跑不跑」。**
//! 一个静态链接成功、启动就崩的二进制与一个链接失败的二进制，对 G103 是同一个结论；
//! 而它们在 `cargo build` 那一层长得完全不一样 —— 前者**是绿的**。
//! ⚠ musl 上最爱被引用的那一类失效正是这种形状：**线程栈比 glibc 小得多**，
//! 链接、启动、`main` 全都好好的，**直到握手真的在工作线程上跑起来**。
//! ⇒ 握手因此**故意跑在 `std::thread::spawn` 起的线程上**，不是主线程 ——
//! 主线程那一份是进程栈（8 MB 量级），测它等于测了一个产品不会走的路径，
//! 而产品里的握手同样发生在 tokio 的工作线程上。
//!
//! ★ ★ **而「musl 默认线程栈 128 KB」这句话对 Rust 程序并不直接成立** ——
//! Rust 的 `std::thread` 不用 libc 的默认值，它自己指定栈大小。
//! ⇒ 与其引用一个二手数字，**探针把这条线程真实的栈大小量出来印在日志里**
//! （`pthread_getattr_np`）。★ 一个量出来的数字与一句引用来的话，在读者那里分量不同。
//!
//! 四条判据（都要过，缺一条这份探针就没答完题）：
//!
//! | | 判据 | 为什么它单独存在 |
//! |---|---|---|
//! | A | `boring-sys` 能在 musl 目标上把 BoringSSL 编出来 | D11 取 quiche 的前提 |
//! | B | 产物是**完全静态**的（没有解释器、没有动态段）| §5 的分发口径 = 单静态二进制（G13）|
//! | C | 在 musl 上真的跑完一次 QUIC 握手并**收发一次应用数据** | 分开写：握手证密钥交换，数据证 1-RTT AEAD |
//! | D | `set_select_certificate_callback` **真的被调用了** | ★ G104 整条决策就压在这一个回调上 |
//!
//! ⚠ B 由 `probe.sh` 判（那是二进制的属性，不是程序自己能诚实回答的）；
//!   A/C/D 由本程序判，各自打一行 `✓`，任何一条不成立就非零退出。
//!
//! 用法：`musl-boringssl-probe <cert.pem> <key.pem>`

use std::sync::atomic::{AtomicUsize, Ordering};

use boring::ssl::{SelectCertError, SslContextBuilder, SslFiletype, SslMethod};

/// 回调被调用的次数。★ 用计数而不是布尔：**「一次」与「很多次」是两件不同的事**，
/// 而一个只会翻成 true 的旗子把它们读成同一个值。
static CALLBACK_HITS: AtomicUsize = AtomicUsize::new(0);

const ALPN: &[&[u8]] = &[b"h3"];
const MAX_DATAGRAM: usize = 1350;

fn main() {
    let mut args = std::env::args().skip(1);
    let cert = args.next().unwrap_or_else(|| usage());
    let key = args.next().unwrap_or_else(|| usage());

    println!("[probe] 目标三元组：{}", env!("PROBE_TARGET"));
    println!(
        "[probe] QUIC 版本 0x{:08x}（quiche::PROTOCOL_VERSION）",
        quiche::PROTOCOL_VERSION
    );

    // ★ 握手跑在一个**新起的线程**上，而不是主线程 —— 产品里的握手同样发生在
    //   tokio 的工作线程上，主线程那一份是进程栈（8 MB 量级）。
    //   ⚠ 这条线程的栈**由 Rust 自己定**，不是 libc 的默认值 —— 具体多少见
    //   `handshake()` 开头量出来的那一行，别在这里写死一个数。
    let handle = std::thread::Builder::new()
        .name("handshake".into())
        .spawn(move || handshake(&cert, &key))
        .expect("起不来握手线程");

    let outcome = match handle.join() {
        Ok(r) => r,
        // ★ 栈爆掉在这里的形状是线程 panic（或者整个进程被 SIGSEGV 带走 ——
        //   后者连这行都印不出来，那正是 probe.sh 还要看退出码的原因）。
        Err(_) => {
            eprintln!("✗ 握手线程 panic 了 —— 上面那行印出来的栈大小是第一个要看的");
            std::process::exit(1);
        }
    };

    if let Err(e) = outcome {
        eprintln!("✗ {e}");
        std::process::exit(1);
    }

    let hits = CALLBACK_HITS.load(Ordering::SeqCst);
    if hits == 0 {
        // ★ ★ 这一条是**反向**的那半边，必须单独判：握手可以在回调从未被调用的
        //   情况下完全成功（BoringSSL 会直接用 context 上那张证书）。
        //   ⇒ 「握手通过」证不了 G104，只有「回调真的进来过」才证得了。
        eprintln!("✗ D：握手成功了，但 set_select_certificate_callback 一次都没被调用");
        eprintln!("   ⇒ G104 压着的那个机制没有被这次握手验到，不许当它验过了");
        std::process::exit(1);
    }
    println!("✓ D：set_select_certificate_callback 被调用 {hits} 次");

    println!("PROBE OK");
}

fn usage() -> ! {
    eprintln!("用法：musl-boringssl-probe <cert.pem> <key.pem>");
    std::process::exit(2);
}

/// 量出**当前线程**的栈大小。拿不到就回 `None` —— ★ 拿不到要说「拿不到」，
/// 不许回一个看起来很正常的默认值：那样这行日志在两种情况下读数相同。
fn current_stack_bytes() -> Option<usize> {
    // SAFETY: 全程只碰本函数栈上的 attr；两个调用都按 musl / glibc 的契约配对
    // （`pthread_getattr_np` 成功后必须 `pthread_attr_destroy`）。
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return None;
        }
        let mut addr: *mut libc::c_void = std::ptr::null_mut();
        let mut size: libc::size_t = 0;
        let rc = libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        if rc == 0 { Some(size) } else { None }
    }
}

/// 在内存里对跑一次 QUIC 握手 + 一次应用数据往返。全程不碰网络（quiche 是 sans-IO）。
fn handshake(cert: &str, key: &str) -> Result<(), String> {
    match current_stack_bytes() {
        Some(n) => println!("[probe] 握手线程的栈：{n} 字节（{} KB）", n / 1024),
        None => println!("[probe] 握手线程的栈：拿不到（pthread_getattr_np 失败）"),
    }

    let client_addr = "127.0.0.1:44444".parse().unwrap();
    let server_addr = "127.0.0.1:44443".parse().unwrap();

    let mut server_config = server_config(cert, key)?;
    let mut client_config = client_config()?;

    let client_scid = quiche::ConnectionId::from_ref(&[0x11; quiche::MAX_CONN_ID_LEN]);
    let server_scid = quiche::ConnectionId::from_ref(&[0x22; quiche::MAX_CONN_ID_LEN]);

    let mut client = quiche::connect(
        Some("probe.fulcrum.invalid"),
        &client_scid,
        client_addr,
        server_addr,
        &mut client_config,
    )
    .map_err(|e| format!("quiche::connect：{e}"))?;

    let mut server = quiche::accept(
        &server_scid,
        None,
        server_addr,
        client_addr,
        &mut server_config,
    )
    .map_err(|e| format!("quiche::accept：{e}"))?;

    // ── 判据 C 的前半：握手 ──────────────────────────────────────────────
    pump(&mut client, &mut server, client_addr, server_addr)?;

    if !client.is_established() || !server.is_established() {
        return Err(format!(
            "C：握手没走完（client established={}，server established={}）",
            client.is_established(),
            server.is_established()
        ));
    }

    let alpn = client.application_proto().to_vec();
    if alpn != b"h3" {
        return Err(format!("C：ALPN 协商到的是 {alpn:?}，不是 h3"));
    }
    // ★ 不要写死「在 musl 上走完」—— 它在 glibc 上跑起来也照样这么印 ——
    //   一句在两种情况下读数相同的话，分不出这两种情况。改成印真的目标三元组。
    println!(
        "✓ C1：QUIC 握手在 {} 上走完，ALPN = h3",
        env!("PROBE_TARGET")
    );

    // ── 判据 C 的后半：1-RTT 应用数据 ───────────────────────────────────
    // ★ 分开判是因为它们证的不是同一件事：握手证的是密钥交换与证书链，
    //   而应用数据走的是 1-RTT AEAD —— BoringSSL 里另一段代码。
    const PAYLOAD: &[u8] = b"fulcrum musl boringssl probe";
    client
        .stream_send(4, PAYLOAD, true)
        .map_err(|e| format!("C：stream_send：{e}"))?;
    pump(&mut client, &mut server, client_addr, server_addr)?;

    let mut got = [0u8; 64];
    let mut seen = Vec::new();
    for stream in server.readable() {
        while let Ok((n, _fin)) = server.stream_recv(stream, &mut got) {
            seen.extend_from_slice(&got[..n]);
        }
    }
    if seen != PAYLOAD {
        return Err(format!(
            "C：应用数据对不上 —— 期望 {:?}，实际 {:?}",
            String::from_utf8_lossy(PAYLOAD),
            String::from_utf8_lossy(&seen)
        ));
    }
    println!("✓ C2：1-RTT 应用数据往返 {} 字节，逐字节相同", seen.len());

    Ok(())
}

fn server_config(cert: &str, key: &str) -> Result<quiche::Config, String> {
    // ★ ★ **这里就是 G104 那条决策的落点**：证书不是通过 quiche 自己的
    //   `load_cert_chain_from_pem_file` 装的，而是我们自己造一个
    //   `SslContextBuilder`、挂上回调、再交给 quiche。
    //   走前者的话这份探针会绿，而 G104 的机制一个字都没被碰到。
    let mut builder = SslContextBuilder::new(SslMethod::tls())
        .map_err(|e| format!("造不出 SslContextBuilder：{e}"))?;

    builder
        .set_certificate_chain_file(cert)
        .map_err(|e| format!("装不上证书 {cert}：{e}"))?;
    builder
        .set_private_key_file(key, SslFiletype::PEM)
        .map_err(|e| format!("装不上私钥 {key}：{e}"))?;

    // 判据 D。★ 回调里**读一下 SNI 再放行** —— 空跑一个 `Ok(())` 也能让计数涨，
    //   但那证不了「拿得到 ClientHello 的内容」，而按 SNI 动态挑证书要的正是这个。
    builder.set_select_certificate_callback(|hello| {
        CALLBACK_HITS.fetch_add(1, Ordering::SeqCst);
        let sni = hello
            .servername(boring::ssl::NameType::HOST_NAME)
            .unwrap_or("<无 SNI>");
        println!("[probe]   回调进来了，ClientHello 里的 SNI = {sni}");
        if sni == "<无 SNI>" {
            // 探针的客户端一定发 SNI；收不到说明我们读错了地方。
            return Err(SelectCertError::ERROR);
        }
        Ok(())
    });

    let mut config = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)
        .map_err(|e| format!("with_boring_ssl_ctx_builder：{e}"))?;
    tune(&mut config)?;
    Ok(config)
}

fn client_config() -> Result<quiche::Config, String> {
    let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)
        .map_err(|e| format!("quiche::Config::new：{e}"))?;
    // 自签证书，探针不验链 —— 验的是 BoringSSL 跑不跑，不是 PKI。
    config.verify_peer(false);
    tune(&mut config)?;
    Ok(config)
}

fn tune(config: &mut quiche::Config) -> Result<(), String> {
    config
        .set_application_protos(ALPN)
        .map_err(|e| format!("set_application_protos：{e}"))?;
    config.set_max_idle_timeout(5_000);
    config.set_max_recv_udp_payload_size(MAX_DATAGRAM);
    config.set_max_send_udp_payload_size(MAX_DATAGRAM);
    config.set_initial_max_data(1_000_000);
    config.set_initial_max_stream_data_bidi_local(100_000);
    config.set_initial_max_stream_data_bidi_remote(100_000);
    config.set_initial_max_streams_bidi(10);
    config.set_disable_active_migration(true);
    Ok(())
}

/// 把两端的数据报来回搬，直到双方都没话说。
///
/// ★ 上界是**有的**：一个不收敛的握手在这里的形状是死循环，而死循环在门禁里
///   与「跑得慢」分不开。给它一个明确的上界，超了就是一条会说话的失败。
fn pump(
    client: &mut quiche::Connection,
    server: &mut quiche::Connection,
    client_addr: std::net::SocketAddr,
    server_addr: std::net::SocketAddr,
) -> Result<(), String> {
    let mut buf = [0u8; 65535];
    for round in 0..64 {
        let mut moved = 0usize;
        moved += flush(client, server, client_addr, server_addr, &mut buf)?;
        moved += flush(server, client, server_addr, client_addr, &mut buf)?;
        if moved == 0 {
            return Ok(());
        }
        let _ = round;
    }
    Err("pump：64 轮之后两端还在互相发包，握手没有收敛".into())
}

/// 把 `from` 攒着的数据报全部交给 `to`，返回搬了几个。
fn flush(
    from: &mut quiche::Connection,
    to: &mut quiche::Connection,
    from_addr: std::net::SocketAddr,
    to_addr: std::net::SocketAddr,
    buf: &mut [u8; 65535],
) -> Result<usize, String> {
    let mut n_datagrams = 0;
    loop {
        let (len, _send_info) = match from.send(buf) {
            Ok(v) => v,
            Err(quiche::Error::Done) => break,
            Err(e) => return Err(format!("send：{e}")),
        };
        let info = quiche::RecvInfo {
            from: from_addr,
            to: to_addr,
        };
        to.recv(&mut buf[..len], info)
            .map_err(|e| format!("recv：{e}"))?;
        n_datagrams += 1;
    }
    Ok(n_datagrams)
}
