//! 主动健康检查（`health_uri`）—— **M1 功能清单的最后一项**。
//!
//! # 它与 `dns_refresh`（批 10）共用同一套机械，但**分开两批做**
//!
//! 两者的形状确实一样：**每上游一格状态 + 一个后台任务 + `pick()` 里多一条筛子**。
//! owner 明确要求分开：**各自可审、各自有门**。★ 事后看这个要求是对的 ——
//! 批 10 那一趟真正值钱的东西（每请求一次 panic、夹具里全是 IP 字面量）
//! 与「定期刷新」几乎无关，混在一批里会被健康检查的篇幅盖掉。
//!
//! # ⚠ 它**不是**被动熔断
//!
//! `passive_fail` / `passive_window`（按真实流量的失败率摘上游）仍在 [`UNWIRED`] 里，
//! 而且**不在 M1 清单上**。两者的差别不是程度而是种类：
//! 主动检查打的是一个**专门的探测路径**，一个「`/health` 回 200 而真实业务在 500」
//! 的上游，本模块**判它健康**。写下来是因为「以为已经会绕过挂掉的上游」
//! 是这一批最容易产生的误解。
//!
//! [`UNWIRED`]: fulcrum_runtime::UNWIRED
//!
//! # 三条有意的取舍
//!
//! 1. **初值健康**（见 `Upstream::healthy` 的文档）：否则进程刚起来到第一次探测
//!    之间是**全站 502**。代价是那一个周期内 `health_uri` 等于没有。
//! 2. **每个目标各记自己的到期时刻**，而不是像 `dns_refresh` 那样全库取最小值。
//!    ⚠ 那边刷得更勤只是多几次 `getaddrinfo`；这边刷得更勤是**打在别人服务上的流量**。
//! 3. **一个上游只要有任意一个候选地址应答就算活**（与转发路径的候选回退一致）。
//!    ⚠ 反过来（要求全部候选都活）会让一台双栈机器上 IPv6 没通就整台被摘掉。

use fulcrum_runtime::{ProxyTarget, SharedRuntime, Upstream};
use log::{debug, info, warn};
use pingora_core::connectors::http::Connector;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::RequestHeader;
use std::sync::Arc;
use std::time::Duration;

/// 打点间隔的下界。★ 防一份写了 `health_interval 100ms` 的配置把上游打爆。
pub const MIN_TICK: Duration = Duration::from_millis(500);
/// 没有任何目标配了 `health_uri` 时的兜底（那时任务根本不会起）。
pub const DEFAULT_TICK: Duration = Duration::from_secs(5);

/// 打点节奏 = 所有 `health_interval` 里最小的那个，再夹上下界。
///
/// ★ **注意这与「多久探一次」不是一回事**：打点只是「醒来看看谁到期了」，
/// 真正的节奏由每个目标自己的 `health_interval` 决定
/// （[`ProxyTarget::take_probe_slot`]）。⚠ 打点比最小间隔慢的话，
/// 那个最小的目标就永远达不到它配的频率 —— 所以这里取 min 而不是别的。
pub fn tick_interval(cfg: &fulcrum_config::StructuredConfig) -> Duration {
    match min_health_interval_ms(cfg) {
        Some(ms) => Duration::from_millis(ms).max(MIN_TICK),
        None => DEFAULT_TICK,
    }
}

/// 这份配置里有没有任何一个目标配了 `health_uri`。没有就不起这个任务。
///
/// ★ ★ 它与 [`tick_interval`] **共用同一次判定**（`min_health_interval_ms`），
/// 不是各写一套。⚠ 各写一套的两种烂法都完全无声：
/// 「起了任务但打点取的是兜底值」／「有目标却根本不起任务」。
/// 本仓库对「同一个判定绝不抄两份」有过明确的教训（D18/G66）。
pub fn any_health_check(cfg: &fulcrum_config::StructuredConfig) -> bool {
    min_health_interval_ms(cfg).is_some()
}

/// 所有**配了 `health_uri`** 的目标里，`health_interval` 最小的那个。
///
/// ⚠ 只有真配了 `health_uri` 的才算数：`health_interval` 有默认值（10s），
/// 让一个**永远不会被探测**的目标参与取最小，会把打点拉快而什么都不多做。
fn min_health_interval_ms(cfg: &fulcrum_config::StructuredConfig) -> Option<u64> {
    let mut best: Option<u64> = None;
    fn walk(steps: &[fulcrum_config::model::Step], best: &mut Option<u64>) {
        for s in steps {
            match &s.body {
                fulcrum_config::model::StepBody::ReverseProxy { health, .. } => {
                    if health.uri.is_some() {
                        let ms = health.interval_ms;
                        *best = Some(best.map_or(ms, |b: u64| b.min(ms)));
                    }
                }
                fulcrum_config::model::StepBody::Handle { arms } => {
                    for a in arms {
                        walk(&a.steps, best);
                    }
                }
                fulcrum_config::model::StepBody::Route { steps } => walk(steps, best),
                // ⚠ 不写 `_`：这类「走遍整张图找某一类步骤」的 match 里，`_` 还兼着
                //   「这个变体不装步骤、不用下钻」这层意思 —— 而新增一个装步骤的容器
                //   变体会让那层意思悄悄变假，夹具里只有当时已有的容器，挡不住它。
                //   ⇒ 规则：新增变体时这一族 match 必须一起改，由编译期穷举来逼。
                // ★ 这一族**跨两层**：运行时图 `BodyRt` 与配置图 `StepBody` 各有一批
                //   同形遍历。⛔ 名单不抄进注释（写死的名单没有门守着，当天就会过期）
                //   —— 要名单就现场 grep 这两个枚举名。
                // ★ 这一处漏下钻的后果正是上面 `any_health_check` 点名要防的那两种
                //   无声烂法之一：配置里写着 `health_uri`，而探测任务一次都不起。
                fulcrum_config::model::StepBody::Tracing
                | fulcrum_config::model::StepBody::Header { .. }
                | fulcrum_config::model::StepBody::Rewrite { .. }
                | fulcrum_config::model::StepBody::Encode { .. }
                | fulcrum_config::model::StepBody::Cache { .. }
                | fulcrum_config::model::StepBody::Redir { .. }
                | fulcrum_config::model::StepBody::Respond { .. }
                | fulcrum_config::model::StepBody::Metrics
                | fulcrum_config::model::StepBody::FileServer { .. } => {}
            }
        }
    }
    for site in &cfg.sites {
        walk(&site.chain, &mut best);
        walk(&site.error_handler, &mut best);
    }
    best
}

/// 探一个上游。`Ok(status)` = 拿到了响应头；`Err(理由)` = 连不上 / 超时 / 协议错。
///
/// ⚠ **它绝不接受 `&str` 形式的地址**：`HttpPeer::new` 对字符串会做一次
/// **阻塞** `to_socket_addrs().unwrap()`（批 10 修的就是那件事）。
/// 这里只吃 [`Upstream::dial_candidates`] 给出的 `SocketAddr`。
async fn probe_one(
    connector: &Connector,
    up: &Upstream,
    target: &ProxyTarget,
    uri: &str,
    timeout: Duration,
) -> Result<u16, String> {
    let candidates = up.dial_candidates();
    if candidates.is_empty() {
        // ★ 解析不出地址**不算探测失败**：`pick()` 已经按第一条筛子跳过它了，
        //   在这里再判一次「不健康」只会让日志上出现两条起因不同的同义警告，
        //   而 DNS 恢复之后还要等一个探测周期才回来。
        return Err("还没解析出地址（本轮跳过，不改判定）".to_string());
    }
    let sni = up.addr.split(':').next().unwrap_or("").to_string();
    let mut last = String::new();
    for dial in &candidates {
        let peer = HttpPeer::new(*dial, target.tls, sni.clone());
        match tokio::time::timeout(timeout, probe_peer(connector, &peer, uri, &up.addr)).await {
            // ⚠ 超时要罩住**整趟**（连接 + 写 + 读），不是只罩其中一段。
            //   本仓库在 exec hook 上栽过一次：`timeout` 只罩了 `wait()`，
            //   而读输出那一步先阻塞了 30 秒 —— 判据是「那条测试跑了 30.01 秒」。
            Err(_) => last = format!("{dial}：超过 {timeout:?} 没回话"),
            Ok(Ok(code)) => return Ok(code),
            Ok(Err(e)) => last = format!("{dial}：{e}"),
        }
    }
    Err(last)
}

async fn probe_peer(
    connector: &Connector,
    peer: &HttpPeer,
    uri: &str,
    host: &str,
) -> Result<u16, String> {
    let (mut sess, _reused) = connector
        .get_http_session(peer)
        .await
        .map_err(|e| e.to_string())?;
    let mut req = RequestHeader::build("GET", uri.as_bytes(), None).map_err(|e| e.to_string())?;
    // ⚠ `Host` 必须给：HTTP/1.1 少了它是非法请求，而很多后端会回 400 ——
    //   那会被判成「不健康」，症状是「上游明明好好的却被摘掉」。
    //   ★ 给的是**配置里写的那个 host:port**，不是解析出来的 IP。
    req.insert_header("Host", host).map_err(|e| e.to_string())?;
    req.insert_header("User-Agent", "fulcrum-healthcheck")
        .map_err(|e| e.to_string())?;
    sess.write_request_header(Box::new(req))
        .await
        .map_err(|e| e.to_string())?;
    sess.finish_request_body()
        .await
        .map_err(|e| e.to_string())?;
    sess.read_response_header()
        .await
        .map_err(|e| e.to_string())?;
    let code = sess
        .response_header()
        .map(|h| h.status.as_u16())
        .ok_or_else(|| "上游没给响应头".to_string())?;
    // ★ 必须把响应体读干净再归还，否则那条连接下一次被复用时会读到上一次的残留。
    //   ⚠ 探测请求很省事地「读完头就扔」的话，代价会落在**业务请求**上，
    //   而现场看起来像是上游在乱回。
    loop {
        match sess.read_response_body().await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => return Err(format!("读探测响应体失败：{e}")),
        }
    }
    connector.release_http_session(sess, peer, None).await;
    Ok(code)
}

/// 跑一轮：把所有**到期的**目标探一遍。返回这一轮真的探了几个上游。
///
/// ★ 拆成一个独立函数（而不是埋在 `start()` 的循环里）是为了让
/// 「到期判定 + 翻转日志」这两件事可以被单独读；`now` 从参数进来。
pub async fn sweep(
    rt: &fulcrum_runtime::Runtime,
    connector: &Connector,
    now: std::time::Instant,
) -> usize {
    let mut probed = 0usize;
    for t in rt.all_proxy_targets() {
        let Some(h) = &t.health else { continue };
        if !t.take_probe_slot(now) {
            continue;
        }
        for up in &t.upstreams {
            probed += 1;
            let verdict = probe_one(connector, up, t, &h.uri, h.timeout).await;
            match verdict {
                Ok(code) => {
                    let ok = h.status.matches(code);
                    if up.set_healthy(ok) {
                        // ★ **只在状态翻转时说话** —— 否则一个持续坏着的上游会刷屏。
                        if ok {
                            info!("上游 {} 健康检查恢复了（{} → {code}）", up.addr, h.uri);
                        } else {
                            warn!(
                                "上游 {} 被健康检查摘掉：{} 回了 {code}，而 health_status 要 {:?}",
                                up.addr, h.uri, h.status
                            );
                        }
                    } else {
                        debug!("上游 {} 健康检查 {} → {code}", up.addr, h.uri);
                    }
                }
                Err(why) if why.starts_with("还没解析出地址") => {
                    // 见 `probe_one`：这一类**不改判定**。
                    debug!("上游 {} 本轮跳过健康检查：{why}", up.addr);
                }
                Err(why) => {
                    if up.set_healthy(false) {
                        warn!("上游 {} 被健康检查摘掉：{why}", up.addr);
                    } else {
                        debug!("上游 {} 仍然不健康：{why}", up.addr);
                    }
                }
            }
        }
    }
    probed
}

/// 挂在 Pingora 上的后台任务。
pub struct HealthCheckService {
    rt: Arc<SharedRuntime>,
    tick: Duration,
    connector: Connector,
}

impl HealthCheckService {
    pub fn new(rt: Arc<SharedRuntime>, tick: Duration) -> HealthCheckService {
        HealthCheckService {
            rt,
            tick,
            // ★ 探测**自己一个连接池**，不与业务流量共用 —— 否则一次探测
            //   可能占掉业务的空闲连接，而探测的失败也会污染业务那边的池。
            connector: Connector::new(None),
        }
    }
}

#[async_trait::async_trait]
impl pingora_core::services::background::BackgroundService for HealthCheckService {
    async fn start(&self, mut shutdown: pingora_core::server::ShutdownWatch) {
        info!(
            "主动健康检查已启动，每 {}ms 打一次点（真正的间隔由每条 reverse_proxy 的 health_interval 决定）",
            self.tick.as_millis()
        );
        loop {
            if *shutdown.borrow() {
                info!("主动健康检查收到停机信号，退出");
                return;
            }
            // ★ `select!` 而不是先 sleep 再看信号（与 ACME 巡检、DNS 重解析同一条纪律）。
            tokio::select! {
                _ = tokio::time::sleep(self.tick) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        info!("主动健康检查收到停机信号，退出");
                        return;
                    }
                }
            }
            // ⚠ 快照只取一次：一轮扫描中途换配置的话，
            //   「判定写到哪一份图上」会变得不确定 —— 而写错那一份是完全无声的。
            let snapshot = self.rt.current();
            let n = sweep(&snapshot, &self.connector, std::time::Instant::now()).await;
            if n > 0 {
                debug!("这一轮探了 {n} 个上游");
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
    fn 打点间隔取所有配了health_uri的里面最小的() {
        let c = cfg(
            "http://a.com {\n  handle /x/* {\n    reverse_proxy p:1 {\n      health_uri /h\n      health_interval 9s\n    }\n  }\n  handle /y/* {\n    reverse_proxy q:1 {\n      health_uri /h\n      health_interval 3s\n    }\n  }\n}\n",
        );
        assert_eq!(tick_interval(&c), Duration::from_secs(3));
    }

    #[test]
    fn 没配health_uri的那条不参与取最小() {
        // ⚠ 反向判据：`health_interval` 有默认值 10s，而一个**不会被探测**的目标
        //   把它带进来取最小，会让打点无谓地变快。
        //   ★ 这里 1s 那条没有 `health_uri`，所以答案必须是 9s 而不是 1s。
        let c = cfg(
            "http://a.com {\n  handle /x/* {\n    reverse_proxy p:1 {\n      health_uri /h\n      health_interval 9s\n    }\n  }\n  handle /y/* {\n    reverse_proxy q:1 {\n      health_interval 1s\n    }\n  }\n}\n",
        );
        assert_eq!(tick_interval(&c), Duration::from_secs(9));
    }

    #[test]
    fn 下界夹得住() {
        let c = cfg(
            "http://a.com {\n  reverse_proxy p:1 {\n    health_uri /h\n    health_interval 100ms\n  }\n}\n",
        );
        assert_eq!(tick_interval(&c), MIN_TICK);
    }

    #[test]
    fn 一条都没配时不起任务() {
        let c = cfg("http://a.com {\n  reverse_proxy p:1\n}\n");
        assert!(!any_health_check(&c));
        assert_eq!(tick_interval(&c), DEFAULT_TICK);
        // ★ 正向那一半：配了就要起。
        let c2 = cfg("http://a.com {\n  reverse_proxy p:1 {\n    health_uri /h\n  }\n}\n");
        assert!(any_health_check(&c2));
    }

    #[test]
    fn 容器里面的那条也要被看见() {
        // ⚠ 与 `dns_refresh` 那边同一个盲区：只扫站点顶层的话，
        //   一个藏在 `handle` 里的目标会**永远不被探测**，而它看起来只是「一直健康」。
        let c = cfg(
            "http://a.com {\n  handle /x/* {\n    reverse_proxy p:1 {\n      health_uri /h\n      health_interval 2s\n    }\n  }\n}\n",
        );
        assert!(any_health_check(&c));
        assert_eq!(tick_interval(&c), Duration::from_secs(2));
    }
}
