//! 上游域名的**定期重解析**（`dns_refresh`）。
//!
//! # ★ ★ 它修的是三条**请求路径**上的缺陷，而不只是「定期刷新」
//!
//! 根因：`HttpPeer::new` 是 `address.to_socket_addrs().unwrap()`，而数据面
//! **每个请求**都构造一次 peer ⇒ ① 每请求一次**阻塞** `getaddrinfo`（跑在 async
//! worker 线程上）；② 解析失败 **panic**，每请求一次（进程活着，站点永久坏掉）；
//! ③ 客户端拿不到那个干净的 502，只看到连接被丢弃。
//!
//! ★ ★ **既有的门一条都没抓到，原因很具体**：判据里所有上游都写的是
//! `127.0.0.1:端口` —— **IP 字面量不走 DNS**，整套判据从没喂过一个域名上游。
//!
//! # 处置
//!
//! 解析结果存进 [`fulcrum_runtime::Upstream`] 的槽里，**请求路径只读槽**。
//! 解析发生在三处、且**都不在请求路径上**：
//!
//! 1. `serve` 启动时（同步跑一遍，还没收流量）；
//! 2. 全量 load 生效**之前**（管理面那条路）——⚠ 少了这一步，一次 load
//!    会让所有域名上游短暂地「没有地址」，而那是一次自找的抖动；
//! 3. 本模块这个后台任务，按 `dns_refresh` 的节奏。
//!
//! # ⚠ 节奏取**全局最小值**，一处有意的简化
//!
//! 按所有 `dns_refresh` 里最小的那个打点，每次把全部域名上游刷一遍。
//! ★ 代价是「配了 60s 的那条实际上每 15s 被刷一次」——**更频繁，不是更稀疏**，
//! 对正确性没有影响。⚠ 写下来是因为不写的话 `dns_refresh 60s` 看起来像是
//! 做了什么它没做的事。

use fulcrum_runtime::SharedRuntime;
use log::{error, info, warn};
use std::sync::Arc;
use std::time::Duration;

/// 打点间隔的下界。★ 防一份写了 `dns_refresh 1s` 的配置把解析器打爆。
pub const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// 没有任何 `reverse_proxy` 时的兜底间隔（其实这时任务根本不会起）。
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// 从结构化配置里算出打点间隔 = 所有 `dns_refresh` 里最小的那个，再夹上下界。
pub fn tick_interval(cfg: &fulcrum_config::StructuredConfig) -> Duration {
    let mut best: Option<u64> = None;
    fn walk(steps: &[fulcrum_config::model::Step], best: &mut Option<u64>) {
        for s in steps {
            match &s.body {
                fulcrum_config::model::StepBody::ReverseProxy { dns_refresh_ms, .. } => {
                    let ms = *dns_refresh_ms;
                    *best = Some(best.map_or(ms, |b: u64| b.min(ms)));
                }
                fulcrum_config::model::StepBody::Handle { arms } => {
                    for a in arms {
                        walk(&a.steps, best);
                    }
                }
                fulcrum_config::model::StepBody::Route { steps } => walk(steps, best),
                _ => {}
            }
        }
    }
    for site in &cfg.sites {
        walk(&site.chain, &mut best);
        walk(&site.error_handler, &mut best);
    }
    match best {
        Some(ms) => Duration::from_millis(ms).max(MIN_INTERVAL),
        None => DEFAULT_INTERVAL,
    }
}

/// 启动时（或全量 load 生效前）同步跑一遍。**不在请求路径上。**
///
/// ★ 解析不出来**不拒绝启动**：一次 DNS 抖动不该让整台机器上所有站点都起不来，
/// 包括那些根本没有域名上游的。⚠ 所以它打的是 **error** 而不是 warn ——
/// 那个上游现在是**用不了**的状态，`pick()` 会跳过它，而运维需要立刻看到这件事。
/// ★ 这与 G59 那条凭据校验的取舍是同一个形状，理由也一样。
pub fn resolve_now(rt: &fulcrum_runtime::Runtime, occasion: &str) {
    let r = fulcrum_runtime::resolve_upstreams(rt);
    if r.queried == 0 {
        return;
    }
    for (addr, addrs) in &r.changed {
        let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
        info!("{occasion}：上游 {addr} 解析到 {}", list.join(" "));
    }
    for (addr, why) in &r.failed {
        error!(
            "⚠ {occasion}：上游 {addr} **解析不出来**（{why}）—— 它会被负载均衡跳过；全都跳过时回 502"
        );
    }
    if r.is_quiet() {
        info!("{occasion}：{} 个域名上游，解析结果没有变化", r.queried);
    }
}

/// 挂在 Pingora 上的后台任务。
pub struct DnsRefreshService {
    rt: Arc<SharedRuntime>,
    interval: Duration,
}

impl DnsRefreshService {
    pub fn new(rt: Arc<SharedRuntime>, interval: Duration) -> DnsRefreshService {
        DnsRefreshService { rt, interval }
    }
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for DnsRefreshService {
    async fn start(&self, mut shutdown: pingora_core::server::ShutdownWatch) {
        info!(
            "上游 DNS 定期重解析已启动，每 {}s 一次",
            self.interval.as_secs()
        );
        // ★ ★ 上一轮**解析不出来**的那些。用来把失败日志压成「状态翻转时才说」。
        //   ⚠ 没有它的话，一个永久坏掉的上游会每 `interval` warn 一行 ——
        //   5 秒一次就是一天一万七千行，而日志被淹掉之后，
        //   真正**新**出现的那一条也没人看得见。
        //   ★ 这与下面 `changed` 那条「只在有变化时说话」是同一条纪律，
        //     只把它用在 `changed` 上等于同一条纪律只落实了一半。
        let mut failing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        loop {
            if *shutdown.borrow() {
                info!("上游 DNS 重解析收到停机信号，退出");
                return;
            }
            // ★ `select!` 而不是先 sleep 再看信号：一个 30 秒的 sleep 会把
            //   优雅停机拖成 30 秒。（与 ACME 巡检同一条纪律。）
            tokio::select! {
                _ = tokio::time::sleep(self.interval) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        info!("上游 DNS 重解析收到停机信号，退出");
                        return;
                    }
                }
            }
            let snapshot = self.rt.current();
            // ⚠ ⚠ **`getaddrinfo` 是阻塞的**，必须扔进 blocking 池。
            //   直接在 runtime 线程上跑，一台慢 DNS 就能把一个 worker 占住几秒——
            //   而那正是本批要修的那件事，在这里重犯一次会格外讽刺。
            let joined = tokio::task::spawn_blocking(move || {
                let r = fulcrum_runtime::resolve_upstreams(&snapshot);
                (r.queried, r.changed, r.failed)
            })
            .await;
            match joined {
                Ok((queried, changed, failed)) => {
                    if queried == 0 {
                        continue;
                    }
                    // ★ **只在有变化时说话**。每 30 秒打一行「没变化」会把日志淹掉，
                    //   而淹掉之后真正变化的那一行也没人看得见。
                    for (addr, addrs) in changed {
                        let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
                        info!("上游 {addr} 的地址变了 → {}（dns_refresh）", list.join(" "));
                    }
                    // ★ 失败**也只在状态翻转时说话**，理由见 `failing` 上那段。
                    let now_failing: std::collections::BTreeSet<String> =
                        failed.iter().map(|(a, _)| a.clone()).collect();
                    for (addr, why) in &failed {
                        if !failing.contains(addr) {
                            warn!("上游 {addr} 开始解析不出来（{why}）—— 保留上一次的结果继续用");
                        }
                    }
                    // ⚠ 先收集再赋值：`difference` 借着 `failing`，不能边借边换。
                    let recovered: Vec<String> =
                        failing.difference(&now_failing).cloned().collect();
                    for addr in recovered {
                        // ★ 恢复也要说一声。只报「开始坏」不报「好了」的日志，
                        //   会让一次早已过去的故障看起来还在持续。
                        info!("上游 {addr} 又解析得出来了");
                    }
                    failing = now_failing;
                }
                Err(e) => error!("上游 DNS 重解析那一步没跑成：{e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dsl: &str) -> fulcrum_config::StructuredConfig {
        fulcrum_config::compile_str("t.Fulcrumfile", dsl)
            .config
            .expect("样例配置应当能编译")
    }

    #[test]
    fn 打点间隔取所有值里最小的那个() {
        let c = cfg(
            "http://a.com {\n  handle /x {\n    reverse_proxy u1:80 {\n      dns_refresh 60s\n    }\n  }\n  handle {\n    reverse_proxy u2:80 {\n      dns_refresh 20s\n    }\n  }\n}\n",
        );
        assert_eq!(tick_interval(&c), Duration::from_secs(20));
    }

    #[test]
    fn 容器里面的那条也要被看见() {
        // ⚠ 反向判据：只扫站点顶层的话，这一条会被漏掉，而漏掉的表现是
        //   「按兜底 30s 打点」——看起来完全正常。
        let c = cfg(
            "http://a.com {\n  route {\n    reverse_proxy u:80 {\n      dns_refresh 7s\n    }\n  }\n}\n",
        );
        // 7s 会被下界夹到 5s 以上 —— 这里它本来就大于下界。
        assert_eq!(tick_interval(&c), Duration::from_secs(7));
    }

    #[test]
    fn 下界夹得住() {
        // ★ 一份写了 1s 的配置不该把解析器打爆。
        let c = cfg("http://a.com {\n  reverse_proxy u:80 {\n    dns_refresh 1s\n  }\n}\n");
        assert_eq!(tick_interval(&c), MIN_INTERVAL);
    }

    #[test]
    fn 没有反代时用兜底值() {
        let c = cfg("http://a.com {\n  respond 200\n}\n");
        assert_eq!(tick_interval(&c), DEFAULT_INTERVAL);
    }
}
