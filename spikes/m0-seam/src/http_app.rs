//! 一個最小的 HTTP/1.1 應答器，掛在 Pingora 原生的 `listening::Service` 上。
//!
//! 它不是產品程式碼，只是 M0 裡的對照組：證明自建服務加進來之後，原生服務照常工作。

use async_trait::async_trait;
use pingora_core::apps::ServerApp;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct MinimalHttp;

const BODY: &[u8] = b"m0-http-ok";

#[async_trait]
impl ServerApp for MinimalHttp {
    async fn process_new(
        self: &Arc<Self>,
        mut session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        // ★ 必須**讀到頭部結束**再應答。原先只 read 一次就回 200：請求一旦被拆成
        //   兩個 TCP 段（頭部跨 MSS、或慢用戶端），伺服器端會在收到第一段時就應答，
        //   第二段隨後被當成新請求又回一個 200 —— 協定就錯位了。
        //   ⚠ 它仍然**不解析請求行也不看方法/路徑**，只判「頭部收完沒有」。
        //     這是 M0 的對照組，不是 HTTP 實作；M1 不要拿它當範本。
        const MAX_HEAD: usize = 8 * 1024;
        // ★ 讀頭部要有**空閒超時**：沒有它時，連上來什麼都不發（或每 29 秒發一個位元組）
        //   的對端會永久佔住一個任務——慢速連接耗盡最基本的形態。
        //   ⚠ 這是**整段頭部**的總預算，不是每次 read 的預算：後者擋不住「慢慢滴」。
        //   ⚠ ★ 它是**每請求**的預算。keep-alive 連接上每來一個請求就重新計時，所以
        //     「每 14 秒發一個完整請求」的對端能無限期佔住連接。真正堵死它需要**連接級**
        //     的總預算 + 並行連接數上限——那是 M1 的事，這裡記下邊界，別讓人以為已經防住了。
        const HEAD_TIMEOUT: Duration = Duration::from_secs(15);

        let read_head = async {
            let mut head_buf: Vec<u8> = Vec::with_capacity(1024);
            let mut chunk = [0u8; 1024];
            loop {
                let n = session.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None; // 對端關閉
                }
                // ★ 先判上限**再**收下這一段：原先是先 extend 後判，緩衝最多會漲到
                //   MAX_HEAD + 1024 才斷——上限就不是它聲稱的那個數了。
                if head_buf.len() + n > MAX_HEAD {
                    return None; // 頭部過大，直接斷開，別把記憶體交給對端控制
                }
                // 只從「新資料可能參與的最小視窗」開始找，避免每次重掃全緩衝
                let scan_from = head_buf.len().saturating_sub(3);
                head_buf.extend_from_slice(&chunk[..n]);
                if head_buf[scan_from..].windows(4).any(|w| w == b"\r\n\r\n") {
                    return Some(head_buf);
                }
            }
        };
        let head_buf = tokio::time::timeout(HEAD_TIMEOUT, read_head).await.ok()??;

        // ★ 尊重用戶端的 `Connection: close`。原先固定回 keep-alive 並留著連接，
        //   與用戶端的意圖相反；探針發的正是 `Connection: close`。
        let lowered = head_buf.to_ascii_lowercase();
        let wants_close = lowered
            .windows(b"connection: close".len())
            .any(|w| w == b"connection: close");

        let conn_hdr = if wants_close { "close" } else { "keep-alive" };
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: {}\r\n\r\n",
            BODY.len(),
            conn_hdr
        );
        session.write_all(head.as_bytes()).await.ok()?;
        session.write_all(BODY).await.ok()?;
        session.flush().await.ok()?;

        if wants_close {
            None // 交還 None = 關閉連接
        } else {
            Some(session) // 連接可重複使用，交還給 service 等待下一個請求
        }
    }
}
