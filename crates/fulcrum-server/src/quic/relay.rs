//! 换代窗口内的 **QUIC 数据报跨进程转交**（**M2 批 K**，G109 ②③④）。
//!
//! 换代时两代持有**同一个** UDP socket（那正是 fd 移交成功的表现），两边都在 `recv_from`
//! ⇒ 内核把数据报任意分给其中一代，而 QUIC 的连接状态只在某一代的内存里。
//! [`crate::quic::gen_id`] 判出「这个数据报属于谁」，本模块负责把它送过去。
//!
//! # 形状：一条 unix datagram socket，路径由 `gen_id` 推导
//!
//! ```text
//!   新一代收到一个 DCID 前缀不是自己的数据报
//!        │
//!        ├─ 从前缀里读出对方的 gen_id（8 字节）
//!        ├─ 路径 = <run_dir>/quic-relay-<hex>.sock     ← ★ 不需要任何握手或协商
//!        └─ send([from][to][原始数据报])
//!                              │
//!                    老一代的 RelayInbox 收下 ──▶ 按 DCID 查自己的连接表
//! ```
//!
//! ★ ★ **通道是单向的**：回包不转交 —— 两代持有同一个 UDP socket，老一代直接 `send_to`
//! 给客户端。⇒ 本模块只有「发出去」与「收进来」两半，没有回程。
//!
//! # ⚠ ⚠ 三条要写在明处的性质
//!
//! 1. **`SOCK_DGRAM` 不是随便挑的**：它保住**数据报边界**。换成 `SOCK_STREAM` 就要
//!    自己加长度前缀并处理粘包，而那是一份新的、要钉住的线上格式。
//! 2. **老一代已经退出** ⇒ `send` 拿到 `ENOENT` / `ECONNREFUSED` ⇒ **丢弃**。
//!    ★ 那是「丢一个包」，不是「杀一条连接」—— 而老一代退出之后那条连接本来就没了。
//! 3. ★ ★ ★ **防环：转交只走一跳。** 从 relay socket 收进来的数据报
//!    **不再做归属判定**（[`decode`] 的结果直接交给本代连接表，不认识就丢）。
//!    ⚠ 这一条不是靠注释守的：[`decode`] 的返回值里**没有** `gen_id`，
//!    而处置它的那条路径**拿不到** `GenId` / `RetryKey` ⇒ 想再判一次也判不出来。

use crate::quic::gen_id::GenId;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use tokio::net::UnixDatagram;

/// 一个地址在转交报文里占的字节数：`1 族 + 16 地址 + 2 端口`。
///
/// ★ **定长**，于是解析不需要任何长度字段 —— 而数据报边界由 `SOCK_DGRAM` 保住。
/// ⚠ IPv4 也占满 16 字节（前 12 字节是零）：省那 12 字节换来的是一份带分支的格式，
/// 而**格式的分支是要被钉住的东西**。
pub const ADDR_LEN: usize = 19;
/// 转交报文的头部长度（`from` + `to`）。
pub const HEADER_LEN: usize = ADDR_LEN * 2;

/// 这一代的 socket 路径：`<run_dir>/quic-relay-<gen_id_hex>.sock`。
///
/// ⚠ 参数名叫 `gen_id` 而不是 `gen` —— **`gen` 在 Rust 2024 edition 里是保留字**。
/// ★ 同样的约定写在 `quic/listener.rs`（那里是字段名）。
///
/// ★ ★ **路径就在 DCID 那 8 个字节里** —— 两代之间因此不需要任何握手或协商。
pub fn sock_path(run_dir: &Path, gen_id: &GenId) -> PathBuf {
    run_dir.join(format!("quic-relay-{}.sock", gen_id.hex()))
}

fn put_addr(a: &SocketAddr, out: &mut Vec<u8>) {
    match a.ip() {
        IpAddr::V4(v4) => {
            out.push(4);
            out.extend_from_slice(&[0u8; 12]);
            out.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            out.push(6);
            out.extend_from_slice(&v6.octets());
        }
    }
    out.extend_from_slice(&a.port().to_be_bytes());
}

fn take_addr(b: &[u8]) -> Option<SocketAddr> {
    let fam = *b.first()?;
    let port = u16::from_be_bytes([*b.get(17)?, *b.get(18)?]);
    let ip = match fam {
        4 => {
            let o: [u8; 4] = b.get(13..17)?.try_into().ok()?;
            IpAddr::V4(Ipv4Addr::from(o))
        }
        6 => {
            let o: [u8; 16] = b.get(1..17)?.try_into().ok()?;
            IpAddr::V6(Ipv6Addr::from(o))
        }
        // ⚠ 认不出的族**不是**「按 IPv4 试试看」——一个畸形报文不该产出一个像样的地址。
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// 拼一份转交报文。
pub fn encode(from: &SocketAddr, to: &SocketAddr, pkt: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(HEADER_LEN + pkt.len());
    put_addr(from, out);
    put_addr(to, out);
    out.extend_from_slice(pkt);
}

/// 拆一份转交报文。`None` = 它不是一份合法的转交报文。
///
/// ⚠ ⚠ **返回值里没有 `gen_id`，这是有意的**：处置它的那条路径因此**没有东西可以再判一次**
/// ⇒ G109 ④ 的「只走一跳」在结构上成立，不靠记性。
pub fn decode(frame: &[u8]) -> Option<(SocketAddr, SocketAddr, &[u8])> {
    // ★ 空载荷（`>` 而不是 `>=`）也算畸形：一个不带数据报的转交报文没有意义，
    //   而把它放行会让下游拿到一个零长度的「QUIC 包」。
    if frame.len() <= HEADER_LEN {
        return None;
    }
    let from = take_addr(&frame[..ADDR_LEN])?;
    let to = take_addr(&frame[ADDR_LEN..HEADER_LEN])?;
    Some((from, to, &frame[HEADER_LEN..]))
}

/// **本代的收件口**：绑在自己那条路径上，等别的代把数据报转过来。
pub struct RelayInbox {
    sock: UnixDatagram,
    path: PathBuf,
}

impl RelayInbox {
    /// 绑上本代的路径。
    ///
    /// # ⚠ 为什么先 `remove_file`
    ///
    /// 一个被 SIGKILL 掉的进程会把 socket 文件留在那里，而 `bind` 撞上它是 `EADDRINUSE` ——
    /// 报错只说「地址已被使用」，看不出是一个陈旧的文件。
    /// ★ ★ **而这里删它是安全的，理由与管理面那处不同**：路径由**随机的 8 字节 `gen_id`**
    /// 推导，两代不可能撞同一个路径 ⇒ 能撞上的只可能是**已经死掉的自己**。
    /// ⚠ 管理面那处的路径是固定的，所以它必须分「换代与否」——这里不需要，
    /// **而两处的差别正是「路径由什么决定」**。
    pub fn bind(run_dir: &Path, gen_id: &GenId) -> std::io::Result<RelayInbox> {
        let path = sock_path(run_dir, gen_id);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let sock = UnixDatagram::bind(&path)?;
        Ok(RelayInbox { sock, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.sock.recv(buf).await
    }
}

impl Drop for RelayInbox {
    /// 退出时 `unlink` 自己那条路径（G109 ③）。
    ///
    /// ⚠ SIGKILL 时它不会跑 —— 那时留下的陈旧文件由下一次 [`RelayInbox::bind`] 清掉，
    /// 而**能撞上它的只可能是同一个 `gen_id`，也就是已经死掉的自己**。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// **发件口**：把不属于本代的数据报送给它真正的主人。
///
/// ★ 按 `gen_id` 缓存已经连上的 socket —— 换代窗口里通常只有一两代，这张表很小。
pub struct RelayOutbox {
    run_dir: PathBuf,
    socks: HashMap<GenId, UnixDatagram>,
    frame: Vec<u8>,
}

/// 一次转交的结果。★ 分三种而不是 `bool`，是因为**它们要在日志里分得开**：
/// 「对方已经退出」是换代的常态，而「发不出去」是我们这侧的问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relayed {
    /// 送出去了。
    Sent,
    /// 那一代已经不在了（`ENOENT` / `ECONNREFUSED`）⇒ 丢弃。
    Gone,
    /// 别的 I/O 错。
    Failed,
}

impl RelayOutbox {
    pub fn new(run_dir: &Path) -> RelayOutbox {
        RelayOutbox {
            run_dir: run_dir.to_path_buf(),
            socks: HashMap::new(),
            frame: Vec::with_capacity(2048),
        }
    }

    /// 把一个数据报转交给 `to_gen` 那一代。
    ///
    /// ⚠ **不缓存失败**：缓存了的话，一代在我们试过之后才起来就永远联系不上。
    /// ★ 而失败的代价只有一次 syscall —— 属于某一代的包在它死后很快就不再来了
    /// （客户端那条连接跟着断）。
    pub async fn send(
        &mut self,
        to_gen: GenId,
        from: &SocketAddr,
        to: &SocketAddr,
        pkt: &[u8],
    ) -> Relayed {
        encode(from, to, pkt, &mut self.frame);

        if !self.socks.contains_key(&to_gen) {
            let path = sock_path(&self.run_dir, &to_gen);
            let sock = match UnixDatagram::unbound() {
                Ok(s) => s,
                Err(_) => return Relayed::Failed,
            };
            // ⚠ `connect` 到一个不存在的路径就是 `ENOENT` —— 那正是「那一代已经退出」。
            if let Err(e) = sock.connect(&path) {
                return classify(&e);
            }
            self.socks.insert(to_gen, sock);
        }

        // ⚠ 先把结果取出来再动 `self.socks` —— 借用检查器不让「拿着表里的引用去改表」，
        //   而下面那条错误处置正要把这一项删掉。
        let res = match self.socks.get(&to_gen) {
            Some(s) => s.send(&self.frame).await,
            None => return Relayed::Failed,
        };
        match res {
            Ok(_) => Relayed::Sent,
            Err(e) => {
                // ★ 对端在我们连上之后才退出 ⇒ 这条缓存已经作废，丢掉它，
                //   免得后面每一个包都往一个死掉的 socket 上发。
                self.socks.remove(&to_gen);
                classify(&e)
            }
        }
    }
}

fn classify(e: &std::io::Error) -> Relayed {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Relayed::Gone,
        _ => Relayed::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> SocketAddr {
        s.parse().expect("测试地址")
    }
    fn v6(s: &str) -> SocketAddr {
        s.parse().expect("测试地址")
    }

    #[test]
    fn 路径由_gen_id_推导_而不是由任何协商决定() {
        let g = GenId::from_bytes([0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33]);
        assert_eq!(
            sock_path(Path::new("/run/fulcrum"), &g),
            PathBuf::from("/run/fulcrum/quic-relay-deadbeef00112233.sock")
        );
        // ★ ★ 这一条守的是 G109 ② 的核心：**收件人的地址就在 DCID 那 8 个字节里**。
        //   ⚠ 换成任何「先握个手交换路径」的做法，换代窗口里就多了一个会失败的步骤。
    }

    #[test]
    fn 一来一回逐字节还原_v4_与_v6_都要() {
        for (from, to) in [
            (v4("192.0.2.9:44300"), v4("192.0.2.1:443")),
            (v6("[2001:db8::9]:44300"), v6("[2001:db8::1]:443")),
            // ⚠ 混着来也要成立：客户端是 v4 而监听在 v6 上（v4-mapped）是真实形态。
            (v4("192.0.2.9:1"), v6("[::1]:65535")),
        ] {
            let mut buf = Vec::new();
            encode(&from, &to, b"quic-bytes", &mut buf);
            assert_eq!(buf.len(), HEADER_LEN + 10);
            let (f2, t2, pkt) = decode(&buf).expect("拆得开");
            assert_eq!((f2, t2), (from, to));
            assert_eq!(pkt, b"quic-bytes");
        }
    }

    #[test]
    fn 畸形报文一律拆不开_而不是拆出一个像样的东西() {
        // ⚠ ⚠ 这一组守的是同一句话：**判据失效时它不该给一个看起来合理的答案**。
        assert!(decode(&[]).is_none(), "空报文");
        assert!(decode(&[0u8; HEADER_LEN]).is_none(), "只有头、没有载荷");
        assert!(decode(&[0u8; HEADER_LEN - 1]).is_none(), "头都不完整");
        // 族字节认不出来 ⇒ 整份拆不开（★ 不许「按 v4 试试看」）。
        let mut bad = vec![0u8; HEADER_LEN + 4];
        bad[0] = 9;
        assert!(decode(&bad).is_none(), "族字节是 9");
        // ★ 第二个地址坏掉也要拆不开 —— 只查第一个的实现会通过上面那几条。
        let mut half = Vec::new();
        put_addr(&v4("192.0.2.9:1"), &mut half);
        half.push(7);
        half.extend_from_slice(&[0u8; ADDR_LEN - 1]);
        half.extend_from_slice(b"x");
        assert!(decode(&half).is_none(), "第二个地址的族字节是 7");
    }

    #[test]
    fn 解出来的东西里没有_gen_id_所以再判一次是判不出来的() {
        // ★ ★ ★ G109 ④「转交只走一跳」的**结构性**判据。
        //   ⚠ 它不是在测一段逻辑，是在钉住一个**类型的形状**：
        //   `decode` 回的是 `(from, to, &[u8])` —— 处置它的路径手里没有代标识，
        //   于是「再转交一次」这件事写不出来。
        //   ⇒ 这条判据会在有人给返回值加上 gen_id 的那一刻编不过。
        let mut buf = Vec::new();
        encode(&v4("192.0.2.9:1"), &v4("192.0.2.1:443"), b"x", &mut buf);
        let got: (SocketAddr, SocketAddr, &[u8]) = decode(&buf).expect("拆得开");
        let _ = got;
    }

    #[tokio::test]
    async fn 收件口绑得上_收得到_退出时把路径清掉() {
        let dir = std::env::temp_dir().join(format!("fulcrum-relay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let g = GenId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
        let path = sock_path(&dir, &g);

        {
            let inbox = RelayInbox::bind(&dir, &g).expect("绑得上");
            assert!(path.exists(), "绑上之后路径要在");

            let mut out = RelayOutbox::new(&dir);
            let r = out
                .send(g, &v4("192.0.2.9:44300"), &v4("192.0.2.1:443"), b"hello")
                .await;
            assert_eq!(r, Relayed::Sent);

            let mut buf = vec![0u8; 1500];
            let n = inbox.recv(&mut buf).await.expect("收得到");
            let (f, t, pkt) = decode(&buf[..n]).expect("拆得开");
            assert_eq!(f, v4("192.0.2.9:44300"));
            assert_eq!(t, v4("192.0.2.1:443"));
            assert_eq!(pkt, b"hello");
        }
        // ★ Drop 之后路径要没（G109 ③）。
        assert!(!path.exists(), "退出时要 unlink 自己的 sock");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 那一代已经退出_是_gone_而不是_failed() {
        // ⚠ ⚠ 这两种**必须分得开**：「对方已经退出」是换代的常态（丢一个包），
        //   而「发不出去」是我们这侧的问题（要有人看）。
        //   ★ 合成一个 `bool` 的话，一次真的故障会被读成一次正常的换代。
        let dir = std::env::temp_dir().join(format!("fulcrum-relay-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let mut out = RelayOutbox::new(&dir);
        let nobody = GenId::from_bytes([9, 9, 9, 9, 9, 9, 9, 9]);
        let r = out
            .send(nobody, &v4("192.0.2.9:1"), &v4("192.0.2.1:443"), b"x")
            .await;
        assert_eq!(r, Relayed::Gone, "路径不存在 ⇒ ENOENT ⇒ Gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 陈旧的_socket_文件不该让绑定失败() {
        // ★ 一个被 SIGKILL 掉的进程会留下它，而 `bind` 撞上去是 EADDRINUSE ——
        //   报错只说「地址已被使用」，看不出是一个陈旧的文件。
        let dir = std::env::temp_dir().join(format!("fulcrum-relay-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let g = GenId::from_bytes([7, 7, 7, 7, 7, 7, 7, 7]);
        let path = sock_path(&dir, &g);
        std::fs::write(&path, b"stale").expect("造一个陈旧文件");
        let inbox = RelayInbox::bind(&dir, &g).expect("陈旧文件不该让它绑不上");
        assert!(inbox.path().exists());
        drop(inbox);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
