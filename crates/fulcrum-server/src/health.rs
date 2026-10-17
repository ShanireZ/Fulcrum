//! 主動健康檢查（`health_uri`）—— **M1 功能清單的最後一項**。
//!
//! # 它與 `dns_refresh`（批 10）共用同一套機械，但**分開兩批做**
//!
//! 兩者的形狀確實一樣：**每上游一格狀態 + 一個後台任務 + `pick()` 裡多一條篩子**。
//! owner 明確要求分開：**各自可審、各自有門**。★ 事後看這個要求是對的 ——
//! 批 10 那一趟真正值錢的東西（每請求一次 panic、夾具裡全是 IP 字面值）
//! 與「定期刷新」幾乎無關，混在一批裡會被健康檢查的篇幅蓋掉。
//!
//! # ⚠ 它**不是**被動熔斷
//!
//! `passive_fail` / `passive_window`（按真實流量的失敗率摘上游）仍在 [`UNWIRED`] 裡，
//! 而且**不在 M1 清單上**。兩者的差別不是程度而是種類：
//! 主動檢查打的是一個**專門的探測路徑**，一個「`/health` 回 200 而真實業務在 500」
//! 的上游，本模組**判它健康**。寫下來是因為「以為已經會繞過掛掉的上游」
//! 是這一批最容易產生的誤解。
//!
//! [`UNWIRED`]: fulcrum_runtime::UNWIRED
//!
//! # 三條有意的取捨
//!
//! 1. **初值健康**（見 `Upstream::healthy` 的文件）：否則處理程序剛起來到第一次探測
//!    之間是**全站 502**。代價是那一個週期內 `health_uri` 等於沒有。
//! 2. **每個目標各記自己的到期時刻**，而不是像 `dns_refresh` 那樣全庫取最小值。
//!    ⚠ 那邊刷得更勤只是多幾次 `getaddrinfo`；這邊刷得更勤是**打在別人服務上的流量**。
//! 3. **一個上游只要有任意一個候選位址應答就算活**（與轉發路徑的候選退回一致）。
//!    ⚠ 反過來（要求全部候選都活）會讓一臺雙棧機器上 IPv6 沒通就整臺被摘掉。

use fulcrum_runtime::{ProxyTarget, SharedRuntime, Upstream};
use log::{debug, info, warn};
use pingora_core::connectors::http::Connector;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::RequestHeader;
use std::sync::Arc;
use std::time::Duration;

/// 打點間隔的下界。★ 防一份寫了 `health_interval 100ms` 的設定把上游打爆。
pub const MIN_TICK: Duration = Duration::from_millis(500);
/// 沒有任何目標配了 `health_uri` 時的備援（那時任務根本不會起）。
pub const DEFAULT_TICK: Duration = Duration::from_secs(5);

/// 打點節奏 = 所有 `health_interval` 裡最小的那個，再夾上下界。
///
/// ★ **注意這與「多久探一次」不是一回事**：打點只是「醒來看看誰到期了」，
/// 真正的節奏由每個目標自己的 `health_interval` 決定
/// （[`ProxyTarget::take_probe_slot`]）。⚠ 打點比最小間隔慢的話，
/// 那個最小的目標就永遠達不到它配的頻率 —— 所以這裡取 min 而不是別的。
pub fn tick_interval(cfg: &fulcrum_config::StructuredConfig) -> Duration {
    match min_health_interval_ms(cfg) {
        Some(ms) => Duration::from_millis(ms).max(MIN_TICK),
        None => DEFAULT_TICK,
    }
}

/// 這份設定裡有沒有任何一個目標配了 `health_uri`。沒有就不起這個任務。
///
/// ★ ★ 它與 [`tick_interval`] **共用同一次判定**（`min_health_interval_ms`），
/// 不是各寫一套。⚠ 各寫一套的兩種爛法都完全無聲：
/// 「起了任務但打點取的是備援值」／「有目標卻根本不起任務」。
/// 本儲存庫對「同一個判定絕不抄兩份」有過明確的教訓（D18/G66）。
pub fn any_health_check(cfg: &fulcrum_config::StructuredConfig) -> bool {
    min_health_interval_ms(cfg).is_some()
}

/// 所有**配了 `health_uri`** 的目標裡，`health_interval` 最小的那個。
///
/// ⚠ 只有真配了 `health_uri` 的才算數：`health_interval` 有預設值（10s），
/// 讓一個**永遠不會被探測**的目標參與取最小，會把打點拉快而什麼都不多做。
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
                // ⚠ 不寫 `_`：這類「走遍整張圖找某一類步驟」的 match 裡，`_` 還兼著
                //   「這個變體不裝步驟、不用下鑽」這層意思 —— 而新增一個裝步驟的容器
                //   變體會讓那層意思悄悄變假，夾具裡只有當時已有的容器，擋不住它。
                //   ⇒ 規則：新增變體時這一族 match 必須一起改，由編譯期窮舉來逼。
                // ★ 這一族**跨兩層**：執行環境圖 `BodyRt` 與設定圖 `StepBody` 各有一批
                //   同形遍歷。⛔ 名單不抄進註解（寫死的名單沒有門守著，當天就會過期）
                //   —— 要名單就現場 grep 這兩個枚舉名。
                // ★ 這一處漏下鑽的後果正是上面 `any_health_check` 點名要防的那兩種
                //   無聲爛法之一：設定裡寫著 `health_uri`，而探測任務一次都不起。
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

/// 探一個上游。`Ok(status)` = 拿到了回應頭；`Err(理由)` = 連不上 / 超時 / 協定錯。
///
/// ⚠ **它絕不接受 `&str` 形式的位址**：`HttpPeer::new` 對字串會做一次
/// **阻塞** `to_socket_addrs().unwrap()`（批 10 修的就是那件事）。
/// 這裡只吃 [`Upstream::dial_candidates`] 給出的 `SocketAddr`。
async fn probe_one(
    connector: &Connector,
    up: &Upstream,
    target: &ProxyTarget,
    uri: &str,
    timeout: Duration,
) -> Result<u16, String> {
    let candidates = up.dial_candidates();
    if candidates.is_empty() {
        // ★ 解析不出位址**不算探測失敗**：`pick()` 已經按第一條篩子跳過它了，
        //   在這裡再判一次「不健康」只會讓紀錄上出現兩條起因不同的同義警告，
        //   而 DNS 恢復之後還要等一個探測週期才回來。
        return Err("还没解析出地址（本轮跳过，不改判定）".to_string());
    }
    let sni = up.addr.split(':').next().unwrap_or("").to_string();
    let mut last = String::new();
    for dial in &candidates {
        let peer = HttpPeer::new(*dial, target.tls, sni.clone());
        match tokio::time::timeout(timeout, probe_peer(connector, &peer, uri, &up.addr)).await {
            // ⚠ 超時要罩住**整趟**（連接 + 寫 + 讀），不是隻罩其中一段。
            //   本儲存庫在 exec hook 上栽過一次：`timeout` 只罩了 `wait()`，
            //   而讀輸出那一步先阻塞了 30 秒 —— 判據是「那條測試跑了 30.01 秒」。
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
    // ⚠ `Host` 必須給：HTTP/1.1 少了它是非法請求，而很多後端會回 400 ——
    //   那會被判成「不健康」，症狀是「上游明明好好的卻被摘掉」。
    //   ★ 給的是**設定裡寫的那個 host:port**，不是解析出來的 IP。
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
    // ★ 必須把回應體讀乾淨再歸還，否則那條連接下一次被重複使用時會讀到上一次的殘留。
    //   ⚠ 探測請求很省事地「讀完頭就扔」的話，代價會落在**業務請求**上，
    //   而現場看起來像是上游在亂回。
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

/// 跑一輪：把所有**到期的**目標探一遍。回傳這一輪真的探了幾個上游。
///
/// ★ 拆成一個獨立函式（而不是埋在 `start()` 的循環裡）是為了讓
/// 「到期判定 + 翻轉紀錄」這兩件事可以被單獨讀；`now` 從參數進來。
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
                        // ★ **只在狀態翻轉時說話** —— 否則一個持續壞著的上游會刷屏。
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
                    // 見 `probe_one`：這一類**不改判定**。
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

/// 掛在 Pingora 上的後台任務。
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
            // ★ 探測**自己一個連接池**，不與業務流量共用 —— 否則一次探測
            //   可能佔掉業務的空閒連接，而探測的失敗也會汙染業務那邊的池。
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
            // ★ `select!` 而不是先 sleep 再看信號（與 ACME 巡檢、DNS 重解析同一條紀律）。
            tokio::select! {
                _ = tokio::time::sleep(self.tick) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        info!("主动健康检查收到停机信号，退出");
                        return;
                    }
                }
            }
            // ⚠ 快照只取一次：一輪掃描中途換設定的話，
            //   「判定寫到哪一份圖上」會變得不確定 —— 而寫錯那一份是完全無聲的。
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
        // ⚠ 反向判據：`health_interval` 有預設值 10s，而一個**不會被探測**的目標
        //   把它帶進來取最小，會讓打點無謂地變快。
        //   ★ 這裡 1s 那條沒有 `health_uri`，所以答案必須是 9s 而不是 1s。
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
        // ⚠ 與 `dns_refresh` 那邊同一個盲區：只掃站台頂層的話，
        //   一個藏在 `handle` 裡的目標會**永遠不被探測**，而它看起來只是「一直健康」。
        let c = cfg(
            "http://a.com {\n  handle /x/* {\n    reverse_proxy p:1 {\n      health_uri /h\n      health_interval 2s\n    }\n  }\n}\n",
        );
        assert!(any_health_check(&c));
        assert_eq!(tick_interval(&c), Duration::from_secs(2));
    }
}
