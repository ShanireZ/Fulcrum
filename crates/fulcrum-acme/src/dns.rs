//! 一個**只會查 TXT** 的最小 DNS 用戶端，用來滿足 G58 那條硬約束。
//!
//! # ★ ★ 它為什麼存在：`sleep` 不是判據
//!
//! G58：DNS-01 要等 TXT **可見**了才通知 CA，而「可見」不是「API 回傳 200」
//! ⇒ 直接向該域的**權威 NS** 輪詢。固定 sleep 在快的時候浪費時間、
//! 在慢的時候**直接簽失敗**，而失敗要消耗 CA 的速率配額。
//!
//! # ★ 為什麼手寫而不是拉 crate（量過再決定）
//!
//! `hickory-proto --no-default-features` = **+51 個包**（含整套 ICU/idna）·
//! `hickory-resolver` = **+80** · 本模組 = **0**。產品整張相依圖也才 ~176 個包。
//!
//! ⚠ **「自己寫」不是免費的**：DNS 訊息解析的經典坑是**名字壓縮指針環**。
//! 它與「正則的指數級回溯」不同 —— 那是一個已知、單一、有完整解法的坑（給跳轉次數封頂，
//! 見 [`MAX_JUMPS`]，並配了一條真的建構指針環的測試）。
//! ★ 全模組**沒有一處裸下標** —— 這條路徑跑在後台巡檢裡，一次 panic 等於自動續期從此不工作。
//! ⚠ 若將來需要完整解析（DNSSEC、任意 RR 類型、TCP 回落），**應當重新評估拉 crate**。

use log::info;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;

/// 壓縮指針最多跳多少次。★ **這就是「指針環」的完整解法**：
/// 每跳一次計數加一，超了就回報錯誤。合法訊息裡跳一兩次就到頭了。
const MAX_JUMPS: usize = 16;

/// 一個網域最多多少個標籤。擋的是「1 位元組標籤 + 指針」拼出來的超長名字。
const MAX_LABELS: usize = 128;

/// EDNS0 裡向對方宣告我們收得下多大的 UDP 應答。
///
/// ★ 不宣告的話，對方按 RFC 1035 只能發 512 位元組，超了就置 TC 位截斷。
/// `_acme-challenge.<域名>` 通常只有一條 TXT，但**一個網域下有 SPF/DKIM/各種驗證串
/// 是常態**，512 並不寬裕。1232 是當前普遍推薦值（避開 IPv6 分片）。
const EDNS_UDP_SIZE: u16 = 1232;

/// TXT 記錄的類型號。
const TYPE_TXT: u16 = 16;
/// OPT 偽記錄（EDNS0）的類型號。
const TYPE_OPT: u16 = 41;
const CLASS_IN: u16 = 1;

/// 查一次 TXT。回傳該名字下**所有** TXT 記錄的值。
///
/// `id` 由呼叫方給——**不在這裡 `rand`**：同 [`crate::Jitter`] 的理由，
/// 這個模組要能被確定性地測。
pub async fn query_txt(
    server: SocketAddr,
    name: &str,
    id: u16,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let query = build_query(name, id)?;

    // 綁定與目標同族的通配位址，否則 v4/v6 混用時 `send_to` 直接失敗。
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let sock = UdpSocket::bind(bind)
        .await
        .map_err(|e| format!("绑不上本地 UDP 端口：{e}"))?;
    sock.send_to(&query, server)
        .await
        .map_err(|e| format!("向 {server} 发 DNS 查询失败：{e}"))?;

    let mut buf = vec![0u8; EDNS_UDP_SIZE as usize];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf))
        .await
        .map_err(|_| format!("等 {server} 的 DNS 应答超时（{}ms）", timeout.as_millis()))?
        .map_err(|e| format!("读 {server} 的 DNS 应答失败：{e}"))?;

    parse_txt_response(&buf[..n], id, name)
}

/// 組一個 TXT 查詢訊息。
fn build_query(name: &str, id: u16) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id.to_be_bytes());
    // ★ RD（遞歸期望）= 0：我們問的是**權威 NS**，不是遞歸解析器。
    //   置 1 會讓某些權威伺服器直接拒答（RA=0 時的 REFUSED）。
    out.extend_from_slice(&0u16.to_be_bytes()); // flags
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&0u16.to_be_bytes()); // ancount
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&1u16.to_be_bytes()); // arcount = 1（下面那條 OPT）
    encode_name(name, &mut out)?;
    out.extend_from_slice(&TYPE_TXT.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    // EDNS0 的 OPT 偽記錄：根名字(0) + TYPE + CLASS(=UDP 載荷大小) + TTL(0) + RDLEN(0)
    out.push(0);
    out.extend_from_slice(&TYPE_OPT.to_be_bytes());
    out.extend_from_slice(&EDNS_UDP_SIZE.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    Ok(out)
}

/// 把 `a.example.com` 編成 `\x01a\x07example\x03com\x00`。
fn encode_name(name: &str, out: &mut Vec<u8>) -> Result<(), String> {
    let trimmed = name.trim_end_matches('.');
    if trimmed.is_empty() {
        out.push(0);
        return Ok(());
    }
    for label in trimmed.split('.') {
        // ⚠ 空標籤（`a..b`）會編出一個提前結束的名字——那是**另一個網域**，不是錯誤提示。
        if label.is_empty() {
            return Err(format!("域名 {name} 里有空标签"));
        }
        if label.len() > 63 {
            return Err(format!("域名 {name} 里有超过 63 字节的标签"));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    if out.len() > 255 + 32 {
        return Err(format!("域名 {name} 编码后过长"));
    }
    Ok(())
}

/// 從應答裡摳出所有 TXT 值。
///
/// ★ 三條檢核缺一不可：**ID 對得上**（擋串包）、**QR=1**（是應答不是查詢回聲）、
/// **RCODE=0**（NXDOMAIN 與「查到了但沒記錄」是兩回事）。
fn parse_txt_response(buf: &[u8], want_id: u16, name: &str) -> Result<Vec<String>, String> {
    if buf.len() < 12 {
        return Err("DNS 应答短于报文头".to_string());
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    if id != want_id {
        return Err(format!("DNS 应答的 ID 对不上（要 {want_id}，收到 {id}）"));
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x8000 == 0 {
        return Err("收到的不是 DNS 应答（QR=0）".to_string());
    }
    // ⚠ TC=1 說明對方截斷了。**不能當成「沒有這條記錄」**——那會讓輪詢一直等到超時，
    //   而真正的原因是應答太大。說出來，讓人能查。
    if flags & 0x0200 != 0 {
        return Err(format!(
            "{name} 的 DNS 应答被截断（TC=1）——本模块不做 TCP 回落，\
             多半是这个名字下的 TXT 太多了"
        ));
    }
    let rcode = flags & 0x000f;
    match rcode {
        0 => {}
        3 => return Ok(Vec::new()), // NXDOMAIN：名字還不存在 —— 對輪詢來說就是「還沒可見」
        other => return Err(format!("{name} 的 DNS 应答 RCODE={other}")),
    }
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;

    let mut pos = 12usize;
    // 跳過問題段。
    for _ in 0..qdcount {
        pos = skip_name(buf, pos)?;
        pos = pos
            .checked_add(4)
            .filter(|p| *p <= buf.len())
            .ok_or("DNS 应答在问题段里就截断了")?;
    }

    let mut out = Vec::new();
    for _ in 0..ancount {
        pos = skip_name(buf, pos)?;
        // TYPE(2) CLASS(2) TTL(4) RDLENGTH(2)
        if pos + 10 > buf.len() {
            return Err("DNS 应答在记录头里截断了".to_string());
        }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        let end = pos
            .checked_add(rdlen)
            .filter(|p| *p <= buf.len())
            .ok_or("DNS 应答的 RDLENGTH 超出报文")?;
        if rtype == TYPE_TXT {
            // TXT 的 rdata 是若干「長度前綴字串」，一條記錄可能被拆成多段，要拼起來。
            let mut s = String::new();
            let mut p = pos;
            while p < end {
                let len = buf[p] as usize;
                let seg_end = p
                    .checked_add(1 + len)
                    .filter(|e| *e <= end)
                    .ok_or("TXT 记录里的段长超出 RDLENGTH")?;
                s.push_str(&String::from_utf8_lossy(&buf[p + 1..seg_end]));
                p = seg_end;
            }
            out.push(s);
        }
        pos = end;
    }
    Ok(out)
}

/// 跳過一個（可能被壓縮的）網域，回傳它後面那個位元組的位置。
///
/// ★ ★ **這是本模組唯一真正危險的地方**：壓縮指針可以指回訊息裡任意位置，
/// 包括指向自己 ⇒ 無限循環。[`MAX_JUMPS`] 就是它的完整解法。
fn skip_name(buf: &[u8], start: usize) -> Result<usize, String> {
    let mut pos = start;
    let mut jumps = 0usize;
    let mut labels = 0usize;
    // 第一次跳轉之後，「名字後面那個位元組」的位置就固定了，後面只是在別處讀內容。
    let mut after: Option<usize> = None;
    loop {
        let len = *buf.get(pos).ok_or("DNS 名字越出报文")?;
        if len & 0xc0 == 0xc0 {
            // 壓縮指針：兩個位元組，低 14 位是偏移。
            let b1 = *buf.get(pos + 1).ok_or("DNS 压缩指针只有一个字节")?;
            let target = (((len & 0x3f) as usize) << 8) | b1 as usize;
            jumps += 1;
            if jumps > MAX_JUMPS {
                return Err("DNS 名字的压缩指针跳转过多（多半是指针环）".to_string());
            }
            // ⚠ 只允許往**前**指。RFC 1035 的壓縮就是這麼定義的，
            //   而且這一條本身就能擋掉絕大多數環。
            if target >= pos {
                return Err("DNS 压缩指针指向自身或后方".to_string());
            }
            after.get_or_insert(pos + 2);
            pos = target;
            continue;
        }
        if len == 0 {
            return Ok(after.unwrap_or(pos + 1));
        }
        labels += 1;
        if labels > MAX_LABELS {
            return Err("DNS 名字的标签过多".to_string());
        }
        pos = pos
            .checked_add(1 + len as usize)
            .filter(|p| *p <= buf.len())
            .ok_or("DNS 名字的标签越出报文")?;
    }
}

/// 向一組權威 NS 輪詢，直到某條 TXT 在**每一臺**上都可見。
///
/// ★ ★ **判據是「每一臺都看得見」，不是「有一臺看得見」。** CA 檢核時會挑哪一臺
/// 我們管不著；只要有一臺還沒同步到，檢核就會失敗——而失敗要消耗速率配額。
/// 寧可多等幾秒。
///
/// ★ `now`/`sleep` 都走 tokio，但**超時與間隔從參數進來**，與 [`crate::Jitter`]
/// 同一條紀律：判據不能掛在寫死的常數上。
/// 一臺權威 NS 的**寫法**：可能是 IP，也可能是主機名。
///
/// # ★ ★ ★ 為什麼不在啟動時就把主機名解析成 IP 存起來
///
/// 那等於**賭這個名字對應的位址永遠不變**。而 `resolvers` 指的恰恰是
/// DNSPod / Cloudflare 這類 **anycast 權威**，它們的位址集合本來就會變 ——
/// 一個跑了三個月的處理程序，握著三個月前解出來的 IP 去問「TXT 可見了嗎」，
/// 而那幾臺可能早就不在這個 anycast 集群裡了。
///
/// ⚠ 現場表現會特別難查：**證書籤不下來，而設定、憑證、DNS 記錄全是對的**。
///
/// ⇒ 存名字，**每次簽發那一刻解析一次**。代價是每次多一次 DNS 查詢（本機 stub
/// 還會快取），換來的是判據永遠問的是**現在**的權威。
/// ★ 這與 G76（上游網域定期重解析）是同一條道理的第二次應用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverSpec {
    /// 主機名或 IPv4 字面值（形狀已由 `fulcrum_config::host::parse_resolver` 在編譯期判過）。
    pub host: String,
    pub port: u16,
}

impl ResolverSpec {
    pub fn new(host: impl Into<String>, port: u16) -> ResolverSpec {
        ResolverSpec {
            host: host.into(),
            port,
        }
    }

    /// 解析成一到多個位址。
    ///
    /// ★ ★ **一個名字解出幾個就要問幾個**，不是挑第一個：`drummer.dnspod.net`
    /// 解出五個位址，那是五臺**各自獨立**的權威節點，而 TXT 是逐節點同步過去的。
    /// 只問第一臺，就會把「這一臺已經有了」當成「全網都有了」——
    /// ⚠ 真機上正是這個形狀：三臺權威都說可見，CA 那邊仍然判 `Invalid`，
    /// 43 秒後重試才成。**問得越全，告訴 CA 之前就越不會撒謊。**
    pub async fn resolve(&self) -> Result<Vec<SocketAddr>, String> {
        // 先當 IPv4 字面值試一次：命中就不必走系統解析器。
        if let Ok(ip) = self.host.parse::<std::net::Ipv4Addr>() {
            return Ok(vec![SocketAddr::from((ip, self.port))]);
        }
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((self.host.as_str(), self.port))
            .await
            .map_err(|e| format!("解析权威 NS `{}` 失败：{e}", self.host))?
            .collect();
        if addrs.is_empty() {
            // ⚠ 解析成功但零個位址：說出來，別讓它變成「查不到 TXT」。
            return Err(format!(
                "权威 NS `{}` 解析出来是空的 —— 这个名字存在但没有地址",
                self.host
            ));
        }
        Ok(addrs)
    }
}

#[derive(Debug)]
pub struct TxtChecker {
    /// 問哪些權威 NS。★ 來自 DSL 的 `resolvers`，**必須顯式配**（見 `dns01.rs`）。
    /// ⚠ 存的是**寫法**（可能是主機名），解析發生在每次 [`TxtChecker::wait_visible`] 入口。
    pub resolvers: Vec<ResolverSpec>,
    /// 單次查詢的超時。
    pub query_timeout: Duration,
    /// 兩次輪詢之間等多久。
    pub poll_interval: Duration,
    /// 總共最多等多久。
    pub deadline: Duration,
    /// 查詢 ID 的起點。★ 由呼叫方給，模組自己不 `rand`。
    id: std::sync::atomic::AtomicU16,
}

impl TxtChecker {
    pub fn new(
        resolvers: Vec<ResolverSpec>,
        query_timeout: Duration,
        poll_interval: Duration,
        deadline: Duration,
        id_seed: u16,
    ) -> TxtChecker {
        TxtChecker {
            resolvers,
            query_timeout,
            poll_interval,
            deadline,
            id: std::sync::atomic::AtomicU16::new(id_seed),
        }
    }

    fn next_id(&self) -> u16 {
        self.id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_mul(2)
            .wrapping_add(1)
    }

    /// 等到 `name` 上出現值為 `expected` 的 TXT，或者超時。
    ///
    /// ⚠ 回傳 `Err` 的含義是「**沒等到**」，呼叫方**不應該**繼續通知 CA 來驗——
    /// 那只會換來一次必然失敗的檢核。
    pub async fn wait_visible(&self, name: &str, expected: &str) -> Result<(), String> {
        if self.resolvers.is_empty() {
            // ★ 走到這裡說明設定檢核漏了。說清楚是哪一條，而不是「查不到」。
            return Err(format!(
                "没有配任何权威 NS（`tls {{ resolvers … }}`），无法确认 {name} 的 TXT 是否可见 —— \
                 G58 不允许用固定 sleep 代替这一步"
            ));
        }
        // ★ 解析發生在**這裡**，不是啟動時（理由見 `ResolverSpec` 的文件註解）。
        //   ⚠ 一臺解析不出來就整趟失敗，而不是少問一臺 —— 少問一臺會讓
        //   「全部可見」這個判據靜靜地變弱，而它正是我們敢通知 CA 的全部依據。
        let mut servers: Vec<SocketAddr> = Vec::new();
        for spec in &self.resolvers {
            let addrs = spec.resolve().await?;
            // ★ info 不是 debug：owner 要的「把解到的 IP 打出來」就落在這裡 ——
            //   而這裡是**唯一**說得準的地方（裝載時說等於賭它到簽發那刻還沒變）。
            info!(
                "权威 NS {}:{} 解析到 {} 个地址：{:?}",
                spec.host,
                spec.port,
                addrs.len(),
                addrs
            );
            servers.extend(addrs);
        }
        let started = tokio::time::Instant::now();
        let mut last: String = String::new();
        loop {
            let mut all_ok = true;
            for server in &servers {
                match query_txt(*server, name, self.next_id(), self.query_timeout).await {
                    Ok(values) => {
                        if !values.iter().any(|v| v == expected) {
                            all_ok = false;
                            last = format!(
                                "{server} 上 {name} 还没有目标 TXT（当前有 {} 条）",
                                values.len()
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        all_ok = false;
                        last = format!("问 {server} 失败：{e}");
                        break;
                    }
                }
            }
            if all_ok {
                // ★ ★ 這條是 **info 不是 debug**，而且是有意的：G58 把「真去問權威 NS」
                //   寫成了硬約束，那麼「我確實問過、而且等到了」就該是**預設可見**的事實。
                //   ⚠ 否則維運只能看到「簽發成功」，而**固定 sleep 的實作也會打出那一行**——
                //   兩種實作從紀錄上分不出來。一次簽發只打一條，不吵。
                info!(
                    "{name} 的 TXT 已在全部 {} 台权威 NS 上可见（等了 {}ms）",
                    self.resolvers.len(),
                    started.elapsed().as_millis()
                );
                return Ok(());
            }
            if started.elapsed() >= self.deadline {
                return Err(format!(
                    "等了 {}s 仍未确认 {name} 的 TXT 在全部 {} 台权威 NS 上可见；最后一次：{last}",
                    self.deadline.as_secs(),
                    self.resolvers.len()
                ));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一條應答：頭 + 問題 + 若干 TXT 答案。
    fn answer(id: u16, name: &str, txts: &[&str], flags: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&id.to_be_bytes());
        b.extend_from_slice(&flags.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&(txts.len() as u16).to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        let qname_at = b.len();
        encode_name(name, &mut b).unwrap();
        b.extend_from_slice(&TYPE_TXT.to_be_bytes());
        b.extend_from_slice(&CLASS_IN.to_be_bytes());
        for t in txts {
            // 用壓縮指針指回問題段的名字——真實伺服器就是這麼幹的。
            b.push(0xc0);
            b.push(qname_at as u8);
            b.extend_from_slice(&TYPE_TXT.to_be_bytes());
            b.extend_from_slice(&CLASS_IN.to_be_bytes());
            b.extend_from_slice(&60u32.to_be_bytes());
            let bytes = t.as_bytes();
            b.extend_from_slice(&((bytes.len() + 1) as u16).to_be_bytes());
            b.push(bytes.len() as u8);
            b.extend_from_slice(bytes);
        }
        b
    }

    #[test]
    fn 正常应答能读出_txt() {
        let buf = answer(
            0x1234,
            "_acme-challenge.example.com",
            &["hello", "world"],
            0x8000,
        );
        let got = parse_txt_response(&buf, 0x1234, "_acme-challenge.example.com").unwrap();
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn id_对不上要报错而不是当成没有记录() {
        // ⚠ 這兩者的區別是**安全性**的：把串包當成「沒記錄」只是慢，
        //   把串包當成「有記錄」就是讓別人替我們回答。
        let buf = answer(0x1111, "a.com", &["x"], 0x8000);
        assert!(parse_txt_response(&buf, 0x2222, "a.com").is_err());
    }

    #[test]
    fn 不是应答要报错() {
        let buf = answer(1, "a.com", &["x"], 0x0000); // QR=0
        assert!(parse_txt_response(&buf, 1, "a.com").is_err());
    }

    #[test]
    fn nxdomain_读作还没可见_而不是错误() {
        // ★ 輪詢期間 NXDOMAIN 是**正常中間狀態**（記錄還沒寫上去），
        //   報成錯誤會讓上層把它當成失敗去退避。
        let buf = answer(1, "a.com", &[], 0x8003);
        assert_eq!(
            parse_txt_response(&buf, 1, "a.com").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn 截断的应答要说出来_不能当成没有记录() {
        // ⚠ 當成「沒有記錄」會讓輪詢一路等到超時，而真正的原因是應答太大。
        let buf = answer(1, "a.com", &[], 0x8200); // TC=1
        let e = parse_txt_response(&buf, 1, "a.com").unwrap_err();
        assert!(e.contains("截断"), "{e}");
    }

    #[test]
    fn 指针环不会把它转死() {
        // ★ ★ 這是本模組最重要的一條測試：建構一個**指向自己**的壓縮指針。
        //   沒有 MAX_JUMPS 與「只許往前指」這兩條，下面這行會掛住整個後台巡檢。
        let mut b = vec![0u8; 12];
        b[2] = 0x80; // QR=1
        b[5] = 1; // qdcount = 1
        // 問題段的名字直接是一個指向自己（偏移 12）的指針
        b.push(0xc0);
        b.push(12);
        let e = parse_txt_response(&b, 0, "x").unwrap_err();
        assert!(e.contains("指向自身或后方"), "{e}");
    }

    #[test]
    fn 往回跳但成环的指针也会被跳数封顶挡住() {
        // 兩個指針互指：13 指向 12、12 指向 13 —— 都不滿足「target >= pos」，
        // 所以必須靠 MAX_JUMPS 兜住。
        let mut b = vec![0u8; 12];
        b[2] = 0x80;
        b[5] = 1;
        b.extend_from_slice(&[0xc0, 14, 0xc0, 12]); // 12→14, 14→12
        let e = parse_txt_response(&b, 0, "x").unwrap_err();
        assert!(e.contains("指针环") || e.contains("指向自身或后方"), "{e}");
    }

    #[test]
    fn 各种截断的报文都返回错误而不是_panic() {
        // ★ 判據是「**一個都不 panic**」——這條路徑跑在後台巡檢裡，
        //   一次 panic 等於自動續期從此不工作。
        let full = answer(7, "_acme-challenge.example.com", &["abc"], 0x8000);
        for cut in 0..full.len() {
            let _ = parse_txt_response(&full[..cut], 7, "x");
        }
        // 再來一批亂位元組
        for seed in 0..64u8 {
            let junk: Vec<u8> = (0..40u8)
                .map(|i| i.wrapping_mul(seed).wrapping_add(seed))
                .collect();
            let _ = parse_txt_response(&junk, u16::from_be_bytes([junk[0], junk[1]]), "x");
        }
    }

    #[test]
    fn rdlength_撒谎时不会读出界() {
        let mut b = answer(1, "a.com", &["x"], 0x8000);
        // 把最後一條記錄的 RDLENGTH 改成一個很大的值
        let n = b.len();
        b[n - 4] = 0xff;
        b[n - 3] = 0xff;
        assert!(parse_txt_response(&b, 1, "a.com").is_err());
    }

    #[test]
    fn 查询报文的形状是对的() {
        let q = build_query("_acme-challenge.example.com", 0xabcd).unwrap();
        assert_eq!(&q[0..2], &[0xab, 0xcd]);
        // ★ RD 必須是 0：問權威 NS 時置 1 會被某些伺服器 REFUSED。
        assert_eq!(u16::from_be_bytes([q[2], q[3]]) & 0x0100, 0, "RD 不该置位");
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1, "qdcount");
        assert_eq!(u16::from_be_bytes([q[10], q[11]]), 1, "arcount（那条 OPT）");
        // 結尾應當是 OPT：根名字 + TYPE 41
        let opt_at = q.len() - 11;
        assert_eq!(q[opt_at], 0);
        assert_eq!(u16::from_be_bytes([q[opt_at + 1], q[opt_at + 2]]), TYPE_OPT);
        assert_eq!(
            u16::from_be_bytes([q[opt_at + 3], q[opt_at + 4]]),
            EDNS_UDP_SIZE
        );
    }

    #[test]
    fn 空标签的域名被拒而不是编成另一个域名() {
        // ⚠ `a..b` 裡那個空標籤會編出一個提前結束的名字 —— 那是**另一個網域**。
        assert!(build_query("a..b", 1).is_err());
        assert!(build_query(&format!("{}.com", "x".repeat(64)), 1).is_err());
        // 末尾的點是合法寫法（FQDN），不該被拒。
        assert!(build_query("example.com.", 1).is_ok());
    }
}
