//! HTTP 面的 PROXY protocol —— **「收」那半边**（M2 批 L 第 ① 步）。
//!
//! # 它是什么
//!
//! 全局块的 `proxy_protocol_from <网段…>` 声明「**这些网段的流量是经代理来的**」。
//! 落到这里，就是：这条连接的对端在清单里时，**在 TLS 握手之前**读掉它的 PROXY 头，
//! 并把这条连接的「对端地址」换成头里报的那个真实客户端。
//!
//! ⇒ 于是 `remote_ip` 匹配器与（批 L 第 ② 步的）访问日志拿到的是**真实客户端**，
//! 而不是那台 LB 自己的地址。
//!
//! # ★ ★ 本模块只有「接线」，没有「解析」，也没有「判断」
//!
//! | 谁 | 干什么 |
//! |---|---|
//! | [`fulcrum_runtime::proxyproto`] | v1/v2 **编解码**（纯逻辑，28 条单测）|
//! | [`fulcrum_runtime::Runtime::trusts_proxy_protocol`] | **唯一**的信任判断入口 |
//! | `pingora_core::listeners`（fork 改动 12）| 循环读 + 覆盖地址 |
//! | **本模块** | 把上面三者接起来，**一行协议逻辑都没有** |
//!
//! ★ 这个分法不是洁癖：`feed` 会在同一条连接上被反复调用，而
//! **fork 里那段代码不认识 PROXY protocol 的任何一个字节** —— rebase 时它不会成为负担。
//!
//! # ⚠ ⚠ 一条会被读错的语义：**清单内的来源不发头 ⇒ 关连接**
//!
//! 那不是缺陷，是 owner 拍板的口径（§10）：若允许清单内的来源**选择性地**
//! 不发头，它就能让枢衡改用 socket 对端 —— 而那个地址正是 LB 自己，
//! 于是一条 `remote_ip 10.0.0.0/8` 规则会**命中它**。
//! ⇒ 「可选」把一个显式的信任声明，变成一个**可以被对端单方面关掉的开关**。

use fulcrum_runtime::SharedRuntime;
use fulcrum_runtime::proxyproto::{self, Verdict};
use pingora_core::listeners::{ProxyProtocolPolicy, ProxyProtocolVerdict};
use std::sync::Arc;

/// 挂到监听器上的策略。
///
/// # ★ ★ ★ 为什么拿的是 `SharedRuntime` 而不是一份 `Vec<Cidr>` 快照
///
/// 快照会在**装载时**定死，于是改了 `proxy_protocol_from` 再 `POST /load`
/// **不生效** —— 而配置文件上完全看不出来。
/// ⚠ 那正是 **D19** 那个形状（`cache { capacity }` 改了要重启）—— 它已由 **G135** 结案。
/// ⛔ **这条交叉引用有意留着**：结案的是那一个实例，**形状本身照旧会复发** ——
/// 「装载时定死一份快照」这种写法在任何新代码里都会长回同一个样子。
/// ⇒ 这里每条连接现读一次当前快照，**换配置立刻生效**，不新欠一条 D 号。
///
/// ★ 代价写在明处：每条**新连接**多一次 `RwLock` 读锁 + 一次 `Arc` 克隆。
/// 它落在 accept 之后、TLS 握手之前那一步上，而那一步本来就要做几十微秒的密码学。
pub(crate) struct HttpProxyProtocol {
    rt: Arc<SharedRuntime>,
}

impl HttpProxyProtocol {
    pub(crate) fn new(rt: Arc<SharedRuntime>) -> Arc<HttpProxyProtocol> {
        Arc::new(HttpProxyProtocol { rt })
    }
}

impl std::fmt::Debug for HttpProxyProtocol {
    /// ⚠ 手写而不是 `derive`：`SharedRuntime` 里是整张运行时图，
    /// 把它打进日志既没用又可能带出配置内容（安全基线：配置预览必须脱敏）。
    /// ★ 打出来的是**当前清单有几条** —— 那才是排障时想知道的。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HttpProxyProtocol({} 个受信网段)",
            self.rt.current().proxy_protocol_from.len()
        )
    }
}

impl ProxyProtocolPolicy for HttpProxyProtocol {
    /// ⚠ ⚠ `peer` 为 `None`（拿不到 inet 对端，例如 Unix domain socket）⇒ **不信任**。
    ///
    /// ★ 与「清单为空时恒 false」同一条纪律：**拿不到证据不等于证据成立**。
    fn trusts(&self, peer: Option<&std::net::SocketAddr>) -> bool {
        match peer {
            Some(a) => self.rt.current().trusts_proxy_protocol(a.ip()),
            None => false,
        }
    }

    /// 纯翻译：`fulcrum_runtime::proxyproto::Verdict` → fork 那侧的同形枚举。
    ///
    /// ★ 两个枚举**有意不共用一个类型**：共用就意味着 `pingora-core` 要依赖
    /// `fulcrum-runtime`，而 fork 的改动面必须保持「只加接缝」。
    fn feed(&self, buf: &[u8]) -> ProxyProtocolVerdict {
        match proxyproto::decode(buf) {
            Verdict::Need(n) => ProxyProtocolVerdict::Need(n),
            Verdict::Done { client, consumed } => ProxyProtocolVerdict::Done { client, consumed },
            Verdict::Invalid(why) => ProxyProtocolVerdict::Invalid(why.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulcrum_config::compile_str;

    fn rt_with(global: &str) -> Arc<SharedRuntime> {
        let src = format!("{global}\nhttp://a.com {{\n  respond 200 \"ok\"\n}}\n");
        let o = compile_str("t.Fulcrumfile", &src);
        let diags = o.render_diagnostics();
        let cfg = o.config.unwrap_or_else(|| panic!("编不过：\n{diags}"));
        let rt = fulcrum_runtime::Runtime::build(&cfg).expect("建不出运行时图");
        SharedRuntime::new(Arc::new(rt))
    }

    #[test]
    fn 清单为空时谁都不信() {
        let p = HttpProxyProtocol::new(rt_with(""));
        let a: std::net::SocketAddr = "10.0.0.5:1234".parse().unwrap();
        // ★ ★ 一份空清单**不是**「信任所有人」—— 这条如果反了，
        //   任何人都能自称是任意 IP，而 `remote_ip` 匹配器会当真。
        assert!(!p.trusts(Some(&a)), "空清单必须谁都不信");
    }

    #[test]
    fn 只信清单里的网段() {
        let p = HttpProxyProtocol::new(rt_with("{\n  proxy_protocol_from 10.0.0.0/8\n}"));
        let inside: std::net::SocketAddr = "10.1.2.3:1234".parse().unwrap();
        let outside: std::net::SocketAddr = "192.168.1.1:1234".parse().unwrap();
        assert!(p.trusts(Some(&inside)), "清单内的应当被信任");
        assert!(!p.trusts(Some(&outside)), "清单外的不该被信任");
    }

    #[test]
    fn 拿不到对端地址时不信任() {
        let p = HttpProxyProtocol::new(rt_with("{\n  proxy_protocol_from 0.0.0.0/0\n}"));
        // ⚠ 连 `0.0.0.0/0` 都不该让「拿不到对端」变成信任 ——
        //   那是 UDS 之类的形态，它根本没有 inet 对端可比。
        assert!(!p.trusts(None), "拿不到 inet 对端时必须不信任");
    }

    #[test]
    fn 换配置立刻生效_不需要重启() {
        let shared = rt_with("");
        let p = HttpProxyProtocol::new(shared.clone());
        let a: std::net::SocketAddr = "10.1.2.3:1234".parse().unwrap();
        assert!(!p.trusts(Some(&a)), "换之前不该信任");

        // ★ ★ ★ 这一条守的是 D19 那个形状：**改了配置再 load 却不生效**。
        //   ⚠ D19 本身已由 G135 结案（缓存容量现在改得动），但**这条测试守的是形状
        //   不是那个实例** —— 它在本文件里挡的是 `proxy_protocol_from`，与缓存无关。
        //   若 `HttpProxyProtocol` 拿的是一份快照而不是 `SharedRuntime`，
        //   下面这次替换之后 `trusts` 仍然是 false —— 而配置文件上看不出任何问题。
        let src =
            "{\n  proxy_protocol_from 10.0.0.0/8\n}\nhttp://a.com {\n  respond 200 \"ok\"\n}\n";
        let o = compile_str("t.Fulcrumfile", src);
        let cfg = o.config.expect("编得过");
        shared.swap(fulcrum_runtime::Runtime::build(&cfg).expect("建得出"));

        assert!(p.trusts(Some(&a)), "★ 换过配置之后必须立刻生效");
    }

    #[test]
    fn feed_把三种判决逐条翻过去() {
        let p = HttpProxyProtocol::new(rt_with(""));

        // ⚠ 空输入：还判不出来。
        assert!(matches!(p.feed(b""), ProxyProtocolVerdict::Need(_)));

        // 一个真的 v1 头。★ 用真样本而不是自己造的形状 —— 夹具写错时，
        //   一个更宽的断言会让它悄悄通过（本仓库为这件事付过账）。
        let v1 = b"PROXY TCP4 192.0.2.7 10.0.0.1 56324 443\r\nGET / HTTP/1.1\r\n";
        match p.feed(v1) {
            ProxyProtocolVerdict::Done { client, consumed } => {
                assert_eq!(
                    client.map(|a| a.ip().to_string()).as_deref(),
                    Some("192.0.2.7")
                );
                // ★ 断言 `consumed` 只吃掉头那一段 —— 多吃一个字节，
                //   后面那个 `GET` 就废了，而那不会有任何报错。
                assert_eq!(consumed, 41, "只该吃掉 PROXY 那一行（含 CRLF）");
            }
            other => panic!("v1 头应当判为 Done，实际 {other:?}"),
        }

        // 坏头。
        assert!(matches!(
            p.feed(b"PROXY TCP4 not-an-ip 10.0.0.1 1 2\r\n"),
            ProxyProtocolVerdict::Invalid(_)
        ));
    }
}
