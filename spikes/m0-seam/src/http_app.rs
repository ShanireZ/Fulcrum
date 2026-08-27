//! 一个最小的 HTTP/1.1 应答器，挂在 Pingora 原生的 `listening::Service` 上。
//!
//! 它不是产品代码，只是 M0 里的对照组：证明自建服务加进来之后，原生服务照常工作。

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
        // ★ 必须**读到头部结束**再应答。原先只 read 一次就回 200：请求一旦被拆成
        //   两个 TCP 段（头部跨 MSS、或慢客户端），服务端会在收到第一段时就应答，
        //   第二段随后被当成新请求又回一个 200 —— 协议就错位了。
        //   ⚠ 它仍然**不解析请求行也不看方法/路径**，只判「头部收完没有」。
        //     这是 M0 的对照组，不是 HTTP 实现；M1 不要拿它当模板。
        const MAX_HEAD: usize = 8 * 1024;
        // ★ 读头部要有**空闲超时**：没有它时，连上来什么都不发（或每 29 秒发一个字节）
        //   的对端会永久占住一个任务——慢速连接耗尽最基本的形态。
        //   ⚠ 这是**整段头部**的总预算，不是每次 read 的预算：后者挡不住「慢慢滴」。
        //   ⚠ ★ 它是**每请求**的预算。keep-alive 连接上每来一个请求就重新计时，所以
        //     「每 14 秒发一个完整请求」的对端能无限期占住连接。真正堵死它需要**连接级**
        //     的总预算 + 并发连接数上限——那是 M1 的事，这里记下边界，别让人以为已经防住了。
        const HEAD_TIMEOUT: Duration = Duration::from_secs(15);

        let read_head = async {
            let mut head_buf: Vec<u8> = Vec::with_capacity(1024);
            let mut chunk = [0u8; 1024];
            loop {
                let n = session.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None; // 对端关闭
                }
                // ★ 先判上限**再**收下这一段：原先是先 extend 后判，缓冲最多会涨到
                //   MAX_HEAD + 1024 才断——上限就不是它声称的那个数了。
                if head_buf.len() + n > MAX_HEAD {
                    return None; // 头部过大，直接断开，别把内存交给对端控制
                }
                // 只从「新数据可能参与的最小窗口」开始找，避免每次重扫全缓冲
                let scan_from = head_buf.len().saturating_sub(3);
                head_buf.extend_from_slice(&chunk[..n]);
                if head_buf[scan_from..].windows(4).any(|w| w == b"\r\n\r\n") {
                    return Some(head_buf);
                }
            }
        };
        let head_buf = tokio::time::timeout(HEAD_TIMEOUT, read_head).await.ok()??;

        // ★ 尊重客户端的 `Connection: close`。原先固定回 keep-alive 并留着连接，
        //   与客户端的意图相反；探针发的正是 `Connection: close`。
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
            None // 交还 None = 关闭连接
        } else {
            Some(session) // 连接可复用，交还给 service 等待下一个请求
        }
    }
}
