//! 換代視窗內的 **QUIC 資料報跨處理程序轉交**（**M2 批 K**，G109 ②③④）。
//!
//! 換代時兩代持有**同一個** UDP socket（那正是 fd 移交成功的表現），兩邊都在 `recv_from`
//! ⇒ 核心把資料報任意分給其中一代，而 QUIC 的連接狀態只在某一代的記憶體裡。
//! [`crate::quic::gen_id`] 判出「這個資料報屬於誰」，本模組負責把它送過去。
//!
//! # 形狀：一條 unix datagram socket，路徑由 `gen_id` 推導
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
//! ★ ★ **通道是單向的**：回包不轉交 —— 兩代持有同一個 UDP socket，老一代直接 `send_to`
//! 給用戶端。⇒ 本模組只有「發出去」與「收進來」兩半，沒有回程。
//!
//! # ⚠ ⚠ 三條要寫在明處的性質
//!
//! 1. **`SOCK_DGRAM` 不是隨便挑的**：它保住**資料報邊界**。換成 `SOCK_STREAM` 就要
//!    自己加長度前綴並處理粘包，而那是一份新的、要釘住的線上格式。
//! 2. **老一代已經退出** ⇒ `send` 拿到 `ENOENT` / `ECONNREFUSED` ⇒ **丟棄**。
//!    ★ 那是「丟一個包」，不是「殺一條連接」—— 而老一代退出之後那條連接本來就沒了。
//! 3. ★ ★ ★ **防環：轉交只走一跳。** 從 relay socket 收進來的資料報
//!    **不再做歸屬判定**（[`decode`] 的結果直接交給本代連接表，不認識就丟）。
//!    ⚠ 這一條不是靠註解守的：[`decode`] 的回傳值裡**沒有** `gen_id`，
//!    而處置它的那條路徑**拿不到** `GenId` / `RetryKey` ⇒ 想再判一次也判不出來。

use crate::quic::gen_id::GenId;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use tokio::net::UnixDatagram;

/// 一個位址在轉交訊息裡佔的位元組數：`1 族 + 16 地址 + 2 端口`。
///
/// ★ **定長**，於是解析不需要任何長度欄位 —— 而資料報邊界由 `SOCK_DGRAM` 保住。
/// ⚠ IPv4 也佔滿 16 位元組（前 12 位元組是零）：省那 12 位元組換來的是一份帶分支的格式，
/// 而**格式的分支是要被釘住的東西**。
pub const ADDR_LEN: usize = 19;
/// 轉交訊息的頭部長度（`from` + `to`）。
pub const HEADER_LEN: usize = ADDR_LEN * 2;

/// 這一代的 socket 路徑：`<run_dir>/quic-relay-<gen_id_hex>.sock`。
///
/// ⚠ 參數名叫 `gen_id` 而不是 `gen` —— **`gen` 在 Rust 2024 edition 裡是保留字**。
/// ★ 同樣的約定寫在 `quic/listener.rs`（那裡是欄位名）。
///
/// ★ ★ **路徑就在 DCID 那 8 個位元組裡** —— 兩代之間因此不需要任何握手或協商。
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
        // ⚠ 認不出的族**不是**「按 IPv4 試試看」——一個畸形訊息不該產出一個像樣的位址。
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// 拼一份轉交訊息。
pub fn encode(from: &SocketAddr, to: &SocketAddr, pkt: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(HEADER_LEN + pkt.len());
    put_addr(from, out);
    put_addr(to, out);
    out.extend_from_slice(pkt);
}

/// 拆一份轉交訊息。`None` = 它不是一份合法的轉交訊息。
///
/// ⚠ ⚠ **回傳值裡沒有 `gen_id`，這是有意的**：處置它的那條路徑因此**沒有東西可以再判一次**
/// ⇒ G109 ④ 的「只走一跳」在結構上成立，不靠記性。
pub fn decode(frame: &[u8]) -> Option<(SocketAddr, SocketAddr, &[u8])> {
    // ★ 空載荷（`>` 而不是 `>=`）也算畸形：一個不帶資料報的轉交訊息沒有意義，
    //   而把它放行會讓下游拿到一個零長度的「QUIC 包」。
    if frame.len() <= HEADER_LEN {
        return None;
    }
    let from = take_addr(&frame[..ADDR_LEN])?;
    let to = take_addr(&frame[ADDR_LEN..HEADER_LEN])?;
    Some((from, to, &frame[HEADER_LEN..]))
}

/// **本代的收件口**：綁在自己那條路徑上，等別的代把資料報轉過來。
pub struct RelayInbox {
    sock: UnixDatagram,
    path: PathBuf,
}

impl RelayInbox {
    /// 綁上本代的路徑。
    ///
    /// # ⚠ 為什麼先 `remove_file`
    ///
    /// 一個被 SIGKILL 掉的處理程序會把 socket 檔案留在那裡，而 `bind` 撞上它是 `EADDRINUSE` ——
    /// 回報錯誤只說「位址已被使用」，看不出是一個陳舊的檔案。
    /// ★ ★ **而這裡刪它是安全的，理由與管理面那處不同**：路徑由**隨機的 8 位元組 `gen_id`**
    /// 推導，兩代不可能撞同一個路徑 ⇒ 能撞上的只可能是**已經死掉的自己**。
    /// ⚠ 管理面那處的路徑是固定的，所以它必須分「換代與否」——這裡不需要，
    /// **而兩處的差別正是「路徑由什麼決定」**。
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
    /// 退出時 `unlink` 自己那條路徑（G109 ③）。
    ///
    /// ⚠ SIGKILL 時它不會跑 —— 那時留下的陳舊檔案由下一次 [`RelayInbox::bind`] 清掉，
    /// 而**能撞上它的只可能是同一個 `gen_id`，也就是已經死掉的自己**。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// **發件口**：把不屬於本代的資料報送給它真正的主人。
///
/// ★ 按 `gen_id` 快取已經連上的 socket —— 換代視窗裡通常只有一兩代，這張表很小。
pub struct RelayOutbox {
    run_dir: PathBuf,
    socks: HashMap<GenId, UnixDatagram>,
    frame: Vec<u8>,
}

/// 一次轉交的結果。★ 分三種而不是 `bool`，是因為**它們要在紀錄裡分得開**：
/// 「對方已經退出」是換代的常態，而「發不出去」是我們這側的問題。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relayed {
    /// 送出去了。
    Sent,
    /// 那一代已經不在了（`ENOENT` / `ECONNREFUSED`）⇒ 丟棄。
    Gone,
    /// 別的 I/O 錯。
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

    /// 把一個資料報轉交給 `to_gen` 那一代。
    ///
    /// ⚠ **不快取失敗**：快取了的話，一代在我們試過之後才起來就永遠聯繫不上。
    /// ★ 而失敗的代價只有一次 syscall —— 屬於某一代的包在它死後很快就不再來了
    /// （用戶端那條連接跟著斷）。
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
            // ⚠ `connect` 到一個不存在的路徑就是 `ENOENT` —— 那正是「那一代已經退出」。
            if let Err(e) = sock.connect(&path) {
                return classify(&e);
            }
            self.socks.insert(to_gen, sock);
        }

        // ⚠ 先把結果取出來再動 `self.socks` —— 借用檢查器不讓「拿著表裡的引用去改表」，
        //   而下面那條錯誤處置正要把這一項刪掉。
        let res = match self.socks.get(&to_gen) {
            Some(s) => s.send(&self.frame).await,
            None => return Relayed::Failed,
        };
        match res {
            Ok(_) => Relayed::Sent,
            Err(e) => {
                // ★ 對端在我們連上之後才退出 ⇒ 這條快取已經作廢，丟掉它，
                //   免得後面每一個包都往一個死掉的 socket 上發。
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
        // ★ ★ 這一條守的是 G109 ② 的核心：**收件人的位址就在 DCID 那 8 個位元組裡**。
        //   ⚠ 換成任何「先握個手交換路徑」的做法，換代視窗裡就多了一個會失敗的步驟。
    }

    #[test]
    fn 一来一回逐字节还原_v4_与_v6_都要() {
        for (from, to) in [
            (v4("192.0.2.9:44300"), v4("192.0.2.1:443")),
            (v6("[2001:db8::9]:44300"), v6("[2001:db8::1]:443")),
            // ⚠ 混著來也要成立：用戶端是 v4 而監聽在 v6 上（v4-mapped）是真實形態。
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
        // ⚠ ⚠ 這一組守的是同一句話：**判據失效時它不該給一個看起來合理的答案**。
        assert!(decode(&[]).is_none(), "空报文");
        assert!(decode(&[0u8; HEADER_LEN]).is_none(), "只有头、没有载荷");
        assert!(decode(&[0u8; HEADER_LEN - 1]).is_none(), "头都不完整");
        // 族位元組認不出來 ⇒ 整份拆不開（★ 不許「按 v4 試試看」）。
        let mut bad = vec![0u8; HEADER_LEN + 4];
        bad[0] = 9;
        assert!(decode(&bad).is_none(), "族字节是 9");
        // ★ 第二個位址壞掉也要拆不開 —— 只查第一個的實作會通過上面那幾條。
        let mut half = Vec::new();
        put_addr(&v4("192.0.2.9:1"), &mut half);
        half.push(7);
        half.extend_from_slice(&[0u8; ADDR_LEN - 1]);
        half.extend_from_slice(b"x");
        assert!(decode(&half).is_none(), "第二个地址的族字节是 7");
    }

    #[test]
    fn 解出来的东西里没有_gen_id_所以再判一次是判不出来的() {
        // ★ ★ ★ G109 ④「轉交只走一跳」的**結構性**判據。
        //   ⚠ 它不是在測一段邏輯，是在釘住一個**類型的形狀**：
        //   `decode` 回的是 `(from, to, &[u8])` —— 處置它的路徑手裡沒有代標識，
        //   於是「再轉交一次」這件事寫不出來。
        //   ⇒ 這條判據會在有人給回傳值加上 gen_id 的那一刻編不過。
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
        // ★ Drop 之後路徑要沒（G109 ③）。
        assert!(!path.exists(), "退出时要 unlink 自己的 sock");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 那一代已经退出_是_gone_而不是_failed() {
        // ⚠ ⚠ 這兩種**必須分得開**：「對方已經退出」是換代的常態（丟一個包），
        //   而「發不出去」是我們這側的問題（要有人看）。
        //   ★ 合成一個 `bool` 的話，一次真的故障會被讀成一次正常的換代。
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
        // ★ 一個被 SIGKILL 掉的處理程序會留下它，而 `bind` 撞上去是 EADDRINUSE ——
        //   回報錯誤只說「位址已被使用」，看不出是一個陳舊的檔案。
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
