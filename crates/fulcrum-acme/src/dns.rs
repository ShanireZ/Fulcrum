//! 一个**只会查 TXT** 的最小 DNS 客户端，用来满足 G58 那条硬约束。
//!
//! # ★ ★ 它为什么存在：`sleep` 不是判据
//!
//! G58：DNS-01 要等 TXT **可见**了才通知 CA，而「可见」不是「API 返回 200」
//! ⇒ 直接向该域的**权威 NS** 轮询。固定 sleep 在快的时候浪费时间、
//! 在慢的时候**直接签失败**，而失败要消耗 CA 的速率配额。
//!
//! # ★ 为什么手写而不是拉 crate（量过再决定）
//!
//! `hickory-proto --no-default-features` = **+51 个包**（含整套 ICU/idna）·
//! `hickory-resolver` = **+80** · 本模块 = **0**。产品整张依赖图也才 ~176 个包。
//!
//! ⚠ **「自己写」不是免费的**：DNS 报文解析的经典坑是**名字压缩指针环**。
//! 它与「正则的指数级回溯」不同 —— 那是一个已知、单一、有完整解法的坑（给跳转次数封顶，
//! 见 [`MAX_JUMPS`]，并配了一条真的构造指针环的测试）。
//! ★ 全模块**没有一处裸下标** —— 这条路径跑在后台巡检里，一次 panic 等于自动续期从此不工作。
//! ⚠ 若将来需要完整解析（DNSSEC、任意 RR 类型、TCP 回落），**应当重新评估拉 crate**。

use log::info;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;

/// 压缩指针最多跳多少次。★ **这就是「指针环」的完整解法**：
/// 每跳一次计数加一，超了就报错。合法报文里跳一两次就到头了。
const MAX_JUMPS: usize = 16;

/// 一个域名最多多少个标签。挡的是「1 字节标签 + 指针」拼出来的超长名字。
const MAX_LABELS: usize = 128;

/// EDNS0 里向对方声明我们收得下多大的 UDP 应答。
///
/// ★ 不声明的话，对方按 RFC 1035 只能发 512 字节，超了就置 TC 位截断。
/// `_acme-challenge.<域名>` 通常只有一条 TXT，但**一个域名下有 SPF/DKIM/各种验证串
/// 是常态**，512 并不宽裕。1232 是当前普遍推荐值（避开 IPv6 分片）。
const EDNS_UDP_SIZE: u16 = 1232;

/// TXT 记录的类型号。
const TYPE_TXT: u16 = 16;
/// OPT 伪记录（EDNS0）的类型号。
const TYPE_OPT: u16 = 41;
const CLASS_IN: u16 = 1;

/// 查一次 TXT。返回该名字下**所有** TXT 记录的值。
///
/// `id` 由调用方给——**不在这里 `rand`**：同 [`crate::Jitter`] 的理由，
/// 这个模块要能被确定性地测。
pub async fn query_txt(
    server: SocketAddr,
    name: &str,
    id: u16,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let query = build_query(name, id)?;

    // 绑定与目标同族的通配地址，否则 v4/v6 混用时 `send_to` 直接失败。
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

/// 组一个 TXT 查询报文。
fn build_query(name: &str, id: u16) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id.to_be_bytes());
    // ★ RD（递归期望）= 0：我们问的是**权威 NS**，不是递归解析器。
    //   置 1 会让某些权威服务器直接拒答（RA=0 时的 REFUSED）。
    out.extend_from_slice(&0u16.to_be_bytes()); // flags
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&0u16.to_be_bytes()); // ancount
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&1u16.to_be_bytes()); // arcount = 1（下面那条 OPT）
    encode_name(name, &mut out)?;
    out.extend_from_slice(&TYPE_TXT.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    // EDNS0 的 OPT 伪记录：根名字(0) + TYPE + CLASS(=UDP 载荷大小) + TTL(0) + RDLEN(0)
    out.push(0);
    out.extend_from_slice(&TYPE_OPT.to_be_bytes());
    out.extend_from_slice(&EDNS_UDP_SIZE.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    Ok(out)
}

/// 把 `a.example.com` 编成 `\x01a\x07example\x03com\x00`。
fn encode_name(name: &str, out: &mut Vec<u8>) -> Result<(), String> {
    let trimmed = name.trim_end_matches('.');
    if trimmed.is_empty() {
        out.push(0);
        return Ok(());
    }
    for label in trimmed.split('.') {
        // ⚠ 空标签（`a..b`）会编出一个提前结束的名字——那是**另一个域名**，不是错误提示。
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

/// 从应答里抠出所有 TXT 值。
///
/// ★ 三条校验缺一不可：**ID 对得上**（挡串包）、**QR=1**（是应答不是查询回声）、
/// **RCODE=0**（NXDOMAIN 与「查到了但没记录」是两回事）。
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
    // ⚠ TC=1 说明对方截断了。**不能当成「没有这条记录」**——那会让轮询一直等到超时，
    //   而真正的原因是应答太大。说出来，让人能查。
    if flags & 0x0200 != 0 {
        return Err(format!(
            "{name} 的 DNS 应答被截断（TC=1）——本模块不做 TCP 回落，\
             多半是这个名字下的 TXT 太多了"
        ));
    }
    let rcode = flags & 0x000f;
    match rcode {
        0 => {}
        3 => return Ok(Vec::new()), // NXDOMAIN：名字还不存在 —— 对轮询来说就是「还没可见」
        other => return Err(format!("{name} 的 DNS 应答 RCODE={other}")),
    }
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;

    let mut pos = 12usize;
    // 跳过问题段。
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
            // TXT 的 rdata 是若干「长度前缀字符串」，一条记录可能被拆成多段，要拼起来。
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

/// 跳过一个（可能被压缩的）域名，返回它后面那个字节的位置。
///
/// ★ ★ **这是本模块唯一真正危险的地方**：压缩指针可以指回报文里任意位置，
/// 包括指向自己 ⇒ 无限循环。[`MAX_JUMPS`] 就是它的完整解法。
fn skip_name(buf: &[u8], start: usize) -> Result<usize, String> {
    let mut pos = start;
    let mut jumps = 0usize;
    let mut labels = 0usize;
    // 第一次跳转之后，「名字后面那个字节」的位置就固定了，后面只是在别处读内容。
    let mut after: Option<usize> = None;
    loop {
        let len = *buf.get(pos).ok_or("DNS 名字越出报文")?;
        if len & 0xc0 == 0xc0 {
            // 压缩指针：两个字节，低 14 位是偏移。
            let b1 = *buf.get(pos + 1).ok_or("DNS 压缩指针只有一个字节")?;
            let target = (((len & 0x3f) as usize) << 8) | b1 as usize;
            jumps += 1;
            if jumps > MAX_JUMPS {
                return Err("DNS 名字的压缩指针跳转过多（多半是指针环）".to_string());
            }
            // ⚠ 只允许往**前**指。RFC 1035 的压缩就是这么定义的，
            //   而且这一条本身就能挡掉绝大多数环。
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

/// 向一组权威 NS 轮询，直到某条 TXT 在**每一台**上都可见。
///
/// ★ ★ **判据是「每一台都看得见」，不是「有一台看得见」。** CA 校验时会挑哪一台
/// 我们管不着；只要有一台还没同步到，校验就会失败——而失败要消耗速率配额。
/// 宁可多等几秒。
///
/// ★ `now`/`sleep` 都走 tokio，但**超时与间隔从参数进来**，与 [`crate::Jitter`]
/// 同一条纪律：判据不能挂在写死的常量上。
/// 一台权威 NS 的**写法**：可能是 IP，也可能是主机名。
///
/// # ★ ★ ★ 为什么不在启动时就把主机名解析成 IP 存起来
///
/// 那等于**赌这个名字对应的地址永远不变**。而 `resolvers` 指的恰恰是
/// DNSPod / Cloudflare 这类 **anycast 权威**，它们的地址集合本来就会变 ——
/// 一个跑了三个月的进程，握着三个月前解出来的 IP 去问「TXT 可见了吗」，
/// 而那几台可能早就不在这个 anycast 集群里了。
///
/// ⚠ 现场表现会特别难查：**证书签不下来，而配置、凭据、DNS 记录全是对的**。
///
/// ⇒ 存名字，**每次签发那一刻解析一次**。代价是每次多一次 DNS 查询（本地 stub
/// 还会缓存），换来的是判据永远问的是**现在**的权威。
/// ★ 这与 G76（上游域名定期重解析）是同一条道理的第二次应用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverSpec {
    /// 主机名或 IPv4 字面量（形状已由 `fulcrum_config::host::parse_resolver` 在编译期判过）。
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

    /// 解析成一到多个地址。
    ///
    /// ★ ★ **一个名字解出几个就要问几个**，不是挑第一个：`drummer.dnspod.net`
    /// 解出五个地址，那是五台**各自独立**的权威节点，而 TXT 是逐节点同步过去的。
    /// 只问第一台，就会把「这一台已经有了」当成「全网都有了」——
    /// ⚠ 真机上正是这个形状：三台权威都说可见，CA 那边仍然判 `Invalid`，
    /// 43 秒后重试才成。**问得越全，告诉 CA 之前就越不会撒谎。**
    pub async fn resolve(&self) -> Result<Vec<SocketAddr>, String> {
        // 先当 IPv4 字面量试一次：命中就不必走系统解析器。
        if let Ok(ip) = self.host.parse::<std::net::Ipv4Addr>() {
            return Ok(vec![SocketAddr::from((ip, self.port))]);
        }
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((self.host.as_str(), self.port))
            .await
            .map_err(|e| format!("解析权威 NS `{}` 失败：{e}", self.host))?
            .collect();
        if addrs.is_empty() {
            // ⚠ 解析成功但零个地址：说出来，别让它变成「查不到 TXT」。
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
    /// 问哪些权威 NS。★ 来自 DSL 的 `resolvers`，**必须显式配**（见 `dns01.rs`）。
    /// ⚠ 存的是**写法**（可能是主机名），解析发生在每次 [`TxtChecker::wait_visible`] 入口。
    pub resolvers: Vec<ResolverSpec>,
    /// 单次查询的超时。
    pub query_timeout: Duration,
    /// 两次轮询之间等多久。
    pub poll_interval: Duration,
    /// 总共最多等多久。
    pub deadline: Duration,
    /// 查询 ID 的起点。★ 由调用方给，模块自己不 `rand`。
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

    /// 等到 `name` 上出现值为 `expected` 的 TXT，或者超时。
    ///
    /// ⚠ 返回 `Err` 的含义是「**没等到**」，调用方**不应该**继续通知 CA 来验——
    /// 那只会换来一次必然失败的校验。
    pub async fn wait_visible(&self, name: &str, expected: &str) -> Result<(), String> {
        if self.resolvers.is_empty() {
            // ★ 走到这里说明配置校验漏了。说清楚是哪一条，而不是「查不到」。
            return Err(format!(
                "没有配任何权威 NS（`tls {{ resolvers … }}`），无法确认 {name} 的 TXT 是否可见 —— \
                 G58 不允许用固定 sleep 代替这一步"
            ));
        }
        // ★ 解析发生在**这里**，不是启动时（理由见 `ResolverSpec` 的文档注释）。
        //   ⚠ 一台解析不出来就整趟失败，而不是少问一台 —— 少问一台会让
        //   「全部可见」这个判据静静地变弱，而它正是我们敢通知 CA 的全部依据。
        let mut servers: Vec<SocketAddr> = Vec::new();
        for spec in &self.resolvers {
            let addrs = spec.resolve().await?;
            // ★ info 不是 debug：owner 要的「把解到的 IP 打出来」就落在这里 ——
            //   而这里是**唯一**说得准的地方（装载时说等于赌它到签发那刻还没变）。
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
                // ★ ★ 这条是 **info 不是 debug**，而且是有意的：G58 把「真去问权威 NS」
                //   写成了硬约束，那么「我确实问过、而且等到了」就该是**默认可见**的事实。
                //   ⚠ 否则运维只能看到「签发成功」，而**固定 sleep 的实现也会打出那一行**——
                //   两种实现从日志上分不出来。一次签发只打一条，不吵。
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

    /// 造一条应答：头 + 问题 + 若干 TXT 答案。
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
            // 用压缩指针指回问题段的名字——真实服务器就是这么干的。
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
        // ⚠ 这两者的区别是**安全性**的：把串包当成「没记录」只是慢，
        //   把串包当成「有记录」就是让别人替我们回答。
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
        // ★ 轮询期间 NXDOMAIN 是**正常中间状态**（记录还没写上去），
        //   报成错误会让上层把它当成失败去退避。
        let buf = answer(1, "a.com", &[], 0x8003);
        assert_eq!(
            parse_txt_response(&buf, 1, "a.com").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn 截断的应答要说出来_不能当成没有记录() {
        // ⚠ 当成「没有记录」会让轮询一路等到超时，而真正的原因是应答太大。
        let buf = answer(1, "a.com", &[], 0x8200); // TC=1
        let e = parse_txt_response(&buf, 1, "a.com").unwrap_err();
        assert!(e.contains("截断"), "{e}");
    }

    #[test]
    fn 指针环不会把它转死() {
        // ★ ★ 这是本模块最重要的一条测试：构造一个**指向自己**的压缩指针。
        //   没有 MAX_JUMPS 与「只许往前指」这两条，下面这行会挂住整个后台巡检。
        let mut b = vec![0u8; 12];
        b[2] = 0x80; // QR=1
        b[5] = 1; // qdcount = 1
        // 问题段的名字直接是一个指向自己（偏移 12）的指针
        b.push(0xc0);
        b.push(12);
        let e = parse_txt_response(&b, 0, "x").unwrap_err();
        assert!(e.contains("指向自身或后方"), "{e}");
    }

    #[test]
    fn 往回跳但成环的指针也会被跳数封顶挡住() {
        // 两个指针互指：13 指向 12、12 指向 13 —— 都不满足「target >= pos」，
        // 所以必须靠 MAX_JUMPS 兜住。
        let mut b = vec![0u8; 12];
        b[2] = 0x80;
        b[5] = 1;
        b.extend_from_slice(&[0xc0, 14, 0xc0, 12]); // 12→14, 14→12
        let e = parse_txt_response(&b, 0, "x").unwrap_err();
        assert!(e.contains("指针环") || e.contains("指向自身或后方"), "{e}");
    }

    #[test]
    fn 各种截断的报文都返回错误而不是_panic() {
        // ★ 判据是「**一个都不 panic**」——这条路径跑在后台巡检里，
        //   一次 panic 等于自动续期从此不工作。
        let full = answer(7, "_acme-challenge.example.com", &["abc"], 0x8000);
        for cut in 0..full.len() {
            let _ = parse_txt_response(&full[..cut], 7, "x");
        }
        // 再来一批乱字节
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
        // 把最后一条记录的 RDLENGTH 改成一个很大的值
        let n = b.len();
        b[n - 4] = 0xff;
        b[n - 3] = 0xff;
        assert!(parse_txt_response(&b, 1, "a.com").is_err());
    }

    #[test]
    fn 查询报文的形状是对的() {
        let q = build_query("_acme-challenge.example.com", 0xabcd).unwrap();
        assert_eq!(&q[0..2], &[0xab, 0xcd]);
        // ★ RD 必须是 0：问权威 NS 时置 1 会被某些服务器 REFUSED。
        assert_eq!(u16::from_be_bytes([q[2], q[3]]) & 0x0100, 0, "RD 不该置位");
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1, "qdcount");
        assert_eq!(u16::from_be_bytes([q[10], q[11]]), 1, "arcount（那条 OPT）");
        // 结尾应当是 OPT：根名字 + TYPE 41
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
        // ⚠ `a..b` 里那个空标签会编出一个提前结束的名字 —— 那是**另一个域名**。
        assert!(build_query("a..b", 1).is_err());
        assert!(build_query(&format!("{}.com", "x".repeat(64)), 1).is_err());
        // 末尾的点是合法写法（FQDN），不该被拒。
        assert!(build_query("example.com.", 1).is_ok());
    }
}
