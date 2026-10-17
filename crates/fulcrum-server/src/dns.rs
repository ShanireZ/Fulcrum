//! 上游網域的**定期重解析**（`dns_refresh`）。
//!
//! # ★ ★ 它修的是三條**請求路徑**上的缺陷，而不只是「定期刷新」
//!
//! 根因：`HttpPeer::new` 是 `address.to_socket_addrs().unwrap()`，而資料面
//! **每個請求**都建構一次 peer ⇒ ① 每請求一次**阻塞** `getaddrinfo`（跑在 async
//! worker 執行緒上）；② 解析失敗 **panic**，每請求一次（處理程序活著，站台永久壞掉）；
//! ③ 用戶端拿不到那個乾淨的 502，只看到連接被丟棄。
//!
//! ★ ★ **既有的門一條都沒抓到，原因很具體**：判據裡所有上游都寫的是
//! `127.0.0.1:端口` —— **IP 字面值不走 DNS**，整套判據從沒餵過一個網域上游。
//!
//! # 處置
//!
//! 解析結果存進 [`fulcrum_runtime::Upstream`] 的槽裡，**請求路徑唯讀槽**。
//! 解析發生在三處、且**都不在請求路徑上**：
//!
//! 1. `serve` 啟動時（同步跑一遍，還沒收流量）；
//! 2. 全量 load 生效**之前**（管理面那條路）——⚠ 少了這一步，一次 load
//!    會讓所有網域上游短暫地「沒有位址」，而那是一次自找的抖動；
//! 3. 本模組這個後台任務，按 `dns_refresh` 的節奏。
//!
//! # ⚠ 節奏取**全域最小值**，一處有意的簡化
//!
//! 按所有 `dns_refresh` 裡最小的那個打點，每次把全部網域上游刷一遍。
//! ★ 代價是「配了 60s 的那條實際上每 15s 被刷一次」——**更頻繁，不是更稀疏**，
//! 對正確性沒有影響。⚠ 寫下來是因為不寫的話 `dns_refresh 60s` 看起來像是
//! 做了什麼它沒做的事。

use fulcrum_runtime::SharedRuntime;
use log::{error, info, warn};
use std::sync::Arc;
use std::time::Duration;

/// 打點間隔的下界。★ 防一份寫了 `dns_refresh 1s` 的設定把解析器打爆。
pub const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// 沒有任何 `reverse_proxy` 時的備援間隔（其實這時任務根本不會起）。
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// 從結構化設定裡算出打點間隔 = 所有 `dns_refresh` 裡最小的那個，再夾上下界。
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
                // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                //   —— 要名單就現場 grep 這兩個枚舉名。
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
    match best {
        Some(ms) => Duration::from_millis(ms).max(MIN_INTERVAL),
        None => DEFAULT_INTERVAL,
    }
}

/// 啟動時（或全量 load 生效前）同步跑一遍。**不在請求路徑上。**
///
/// ★ 解析不出來**不拒絕啟動**：一次 DNS 抖動不該讓整臺機器上所有站台都起不來，
/// 包括那些根本沒有網域上游的。⚠ 所以它打的是 **error** 而不是 warn ——
/// 那個上游現在是**用不了**的狀態，`pick()` 會跳過它，而維運需要立刻看到這件事。
/// ★ 這與 G59 那條憑證檢核的取捨是同一個形狀，理由也一樣。
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

/// 掛在 Pingora 上的後台任務。
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
        // ★ ★ 上一輪**解析不出來**的那些。用來把失敗紀錄壓成「狀態翻轉時才說」。
        //   ⚠ 沒有它的話，一個永久壞掉的上游會每 `interval` warn 一行 ——
        //   5 秒一次就是一天一萬七千行，而紀錄被淹掉之後，
        //   真正**新**出現的那一條也沒人看得見。
        //   ★ 這與下面 `changed` 那條「只在有變化時說話」是同一條紀律，
        //     只把它用在 `changed` 上等於同一條紀律只落實了一半。
        let mut failing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        loop {
            if *shutdown.borrow() {
                info!("上游 DNS 重解析收到停机信号，退出");
                return;
            }
            // ★ `select!` 而不是先 sleep 再看信號：一個 30 秒的 sleep 會把
            //   優雅停機拖成 30 秒。（與 ACME 巡檢同一條紀律。）
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
            // ⚠ ⚠ **`getaddrinfo` 是阻塞的**，必須扔進 blocking 池。
            //   直接在 runtime 執行緒上跑，一臺慢 DNS 就能把一個 worker 佔住幾秒——
            //   而那正是本批要修的那件事，在這裡重犯一次會格外諷刺。
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
                    // ★ **只在有變化時說話**。每 30 秒打一行「沒變化」會把紀錄淹掉，
                    //   而淹掉之後真正變化的那一行也沒人看得見。
                    for (addr, addrs) in changed {
                        let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
                        info!("上游 {addr} 的地址变了 → {}（dns_refresh）", list.join(" "));
                    }
                    // ★ 失敗**也只在狀態翻轉時說話**，理由見 `failing` 上那段。
                    let now_failing: std::collections::BTreeSet<String> =
                        failed.iter().map(|(a, _)| a.clone()).collect();
                    for (addr, why) in &failed {
                        if !failing.contains(addr) {
                            warn!("上游 {addr} 开始解析不出来（{why}）—— 保留上一次的结果继续用");
                        }
                    }
                    // ⚠ 先收集再賦值：`difference` 藉著 `failing`，不能邊借邊換。
                    let recovered: Vec<String> =
                        failing.difference(&now_failing).cloned().collect();
                    for addr in recovered {
                        // ★ 恢復也要說一聲。只報「開始壞」不報「好了」的紀錄，
                        //   會讓一次早已過去的故障看起來還在持續。
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
        // ⚠ 反向判據：只掃站台頂層的話，這一條會被漏掉，而漏掉的表現是
        //   「按備援 30s 打點」——看起來完全正常。
        let c = cfg(
            "http://a.com {\n  route {\n    reverse_proxy u:80 {\n      dns_refresh 7s\n    }\n  }\n}\n",
        );
        // 7s 會被下界夾到 5s 以上 —— 這裡它本來就大於下界。
        assert_eq!(tick_interval(&c), Duration::from_secs(7));
    }

    #[test]
    fn 下界夹得住() {
        // ★ 一份寫了 1s 的設定不該把解析器打爆。
        let c = cfg("http://a.com {\n  reverse_proxy u:80 {\n    dns_refresh 1s\n  }\n}\n");
        assert_eq!(tick_interval(&c), MIN_INTERVAL);
    }

    #[test]
    fn 没有反代时用兜底值() {
        let c = cfg("http://a.com {\n  respond 200\n}\n");
        assert_eq!(tick_interval(&c), DEFAULT_INTERVAL);
    }
}
