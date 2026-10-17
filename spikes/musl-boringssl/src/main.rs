//! musl + BoringSSL 靜態連結探針（G103/G104 的未驗前置，`PLAN.md` §10）。
//!
//! ★ ★ ★ **這份探針要答的不是「能不能連結」，是「鏈上之後跑不跑」。**
//! 一個靜態連結成功、啟動就崩的二進位與一個連結失敗的二進位，對 G103 是同一個結論；
//! 而它們在 `cargo build` 那一層長得完全不一樣 —— 前者**是綠的**。
//! ⚠ musl 上最愛被引用的那一類失效正是這種形狀：**執行緒棧比 glibc 小得多**，
//! 連結、啟動、`main` 全都好好的，**直到握手真的在工作執行緒上跑起來**。
//! ⇒ 握手因此**故意跑在 `std::thread::spawn` 起的執行緒上**，不是主執行緒 ——
//! 主執行緒那一份是處理程序棧（8 MB 量級），測它等於測了一個產品不會走的路徑，
//! 而產品裡的握手同樣發生在 tokio 的工作執行緒上。
//!
//! ★ ★ **而「musl 預設執行緒棧 128 KB」這句話對 Rust 處理程序並不直接成立** ——
//! Rust 的 `std::thread` 不用 libc 的預設值，它自己指定棧大小。
//! ⇒ 與其引用一個二手數字，**探針把這條執行緒真實的棧大小量出來印在紀錄裡**
//! （`pthread_getattr_np`）。★ 一個量出來的數字與一句引用來的話，在讀者那裡分量不同。
//!
//! 四條判據（都要過，缺一條這份探針就沒答完題）：
//!
//! | | 判據 | 為什麼它單獨存在 |
//! |---|---|---|
//! | A | `boring-sys` 能在 musl 目標上把 BoringSSL 編出來 | D11 取 quiche 的前提 |
//! | B | 產物是**完全靜態**的（沒有解釋器、沒有動態段）| §5 的散佈口徑 = 單靜態二進位（G13）|
//! | C | 在 musl 上真的跑完一次 QUIC 握手並**收發一次應用資料** | 分開寫：握手證金鑰交換，資料證 1-RTT AEAD |
//! | D | `set_select_certificate_callback` **真的被呼叫了** | ★ G104 整條決策就壓在這一個回呼上 |
//!
//! ⚠ B 由 `probe.sh` 判（那是二進位的屬性，不是處理程序自己能誠實回答的）；
//!   A/C/D 由本處理程序判，各自打一行 `✓`，任何一條不成立就非零退出。
//!
//! 用法：`musl-boringssl-probe <cert.pem> <key.pem>`

use std::sync::atomic::{AtomicUsize, Ordering};

use boring::ssl::{SelectCertError, SslContextBuilder, SslFiletype, SslMethod};

/// 回呼被呼叫的次數。★ 用計數而不是布爾：**「一次」與「很多次」是兩件不同的事**，
/// 而一個只會翻成 true 的旗子把它們讀成同一個值。
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

    // ★ 握手跑在一個**新起的執行緒**上，而不是主執行緒 —— 產品裡的握手同樣發生在
    //   tokio 的工作執行緒上，主執行緒那一份是處理程序棧（8 MB 量級）。
    //   ⚠ 這條執行緒的棧**由 Rust 自己定**，不是 libc 的預設值 —— 具體多少見
    //   `handshake()` 開頭量出來的那一行，別在這裡寫死一個數。
    let handle = std::thread::Builder::new()
        .name("handshake".into())
        .spawn(move || handshake(&cert, &key))
        .expect("起不来握手线程");

    let outcome = match handle.join() {
        Ok(r) => r,
        // ★ 棧爆掉在這裡的形狀是執行緒 panic（或者整個處理程序被 SIGSEGV 帶走 ——
        //   後者連這行都印不出來，那正是 probe.sh 還要看結束代碼的原因）。
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
        // ★ ★ 這一條是**反向**的那半邊，必須單獨判：握手可以在回呼從未被呼叫的
        //   情況下完全成功（BoringSSL 會直接用 context 上那張證書）。
        //   ⇒ 「握手通過」證不了 G104，只有「回呼真的進來過」才證得了。
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

/// 量出**當前執行緒**的棧大小。拿不到就回 `None` —— ★ 拿不到要說「拿不到」，
/// 不許回一個看起來很正常的預設值：那樣這行紀錄在兩種情況下讀數相同。
fn current_stack_bytes() -> Option<usize> {
    // SAFETY: 全程只碰本函式棧上的 attr；兩個呼叫都按 musl / glibc 的契約配對
    // （`pthread_getattr_np` 成功後必須 `pthread_attr_destroy`）。
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

/// 在記憶體裡對跑一次 QUIC 握手 + 一次應用資料往返。全程不碰網路（quiche 是 sans-IO）。
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

    // ── 判據 C 的前半：握手 ──────────────────────────────────────────────
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
    // ★ 不要寫死「在 musl 上走完」—— 它在 glibc 上跑起來也照樣這麼印 ——
    //   一句在兩種情況下讀數相同的話，分不出這兩種情況。改成印真的目標三元組。
    println!(
        "✓ C1：QUIC 握手在 {} 上走完，ALPN = h3",
        env!("PROBE_TARGET")
    );

    // ── 判據 C 的後半：1-RTT 應用資料 ───────────────────────────────────
    // ★ 分開判是因為它們證的不是同一件事：握手證的是金鑰交換與證書鏈，
    //   而應用資料走的是 1-RTT AEAD —— BoringSSL 裡另一段程式碼。
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
    // ★ ★ **這裡就是 G104 那條決策的落點**：證書不是通過 quiche 自己的
    //   `load_cert_chain_from_pem_file` 裝的，而是我們自己造一個
    //   `SslContextBuilder`、掛上回呼、再交給 quiche。
    //   走前者的話這份探針會綠，而 G104 的機制一個字都沒被碰到。
    let mut builder = SslContextBuilder::new(SslMethod::tls())
        .map_err(|e| format!("造不出 SslContextBuilder：{e}"))?;

    builder
        .set_certificate_chain_file(cert)
        .map_err(|e| format!("装不上证书 {cert}：{e}"))?;
    builder
        .set_private_key_file(key, SslFiletype::PEM)
        .map_err(|e| format!("装不上私钥 {key}：{e}"))?;

    // 判據 D。★ 回呼裡**讀一下 SNI 再放行** —— 空跑一個 `Ok(())` 也能讓計數漲，
    //   但那證不了「拿得到 ClientHello 的內容」，而按 SNI 動態挑證書要的正是這個。
    builder.set_select_certificate_callback(|hello| {
        CALLBACK_HITS.fetch_add(1, Ordering::SeqCst);
        let sni = hello
            .servername(boring::ssl::NameType::HOST_NAME)
            .unwrap_or("<无 SNI>");
        println!("[probe]   回调进来了，ClientHello 里的 SNI = {sni}");
        if sni == "<无 SNI>" {
            // 探針的用戶端一定發 SNI；收不到說明我們讀錯了地方。
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
    // 自簽證書，探針不驗鏈 —— 驗的是 BoringSSL 跑不跑，不是 PKI。
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

/// 把兩端的資料報來回搬，直到雙方都沒話說。
///
/// ★ 上界是**有的**：一個不收斂的握手在這裡的形狀是死循環，而死循環在門禁裡
///   與「跑得慢」分不開。給它一個明確的上界，超了就是一條會說話的失敗。
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

/// 把 `from` 攢著的資料報全部交給 `to`，回傳搬了幾個。
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
