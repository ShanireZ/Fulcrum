//! 簽發與續期的執行路徑。
//!
//! 一輪巡檢對每個網域做同一件事：
//!
//! ```text
//! 拿跨程序鎖 ──▶ 重讀儲存 ──▶ 問 ARI ──▶ 判定該不該續（G56）
//!                                            │否 ──▶ 裝上現有的那張，收工
//!                                            │是
//!                                            ▼
//!                    下單 ──▶ 解挑戰 ──▶ 等 ready ──▶ finalize ──▶ 取證書
//!                                            │
//!                                            ▼
//!                              寫儲存（原子，G55）──▶ 回讀校驗 ──▶ 熱裝到解析器
//! ```
//!
//! ★ **順序必須是「先拿鎖、再讀」**：升級視窗裡兩代共享同一個證書目錄，
//! 第二代在鎖上等到的時候證書可能已經在那兒了。再簽一張不只是白跑 ——
//! CA 的速率配額**按帳戶**算，耗盡之後簽不出來的不只是這一個網域。
//!
//! ★ **寫完回讀一次再裝**：直接裝記憶體裡那份，等於從沒驗證過寫入磁碟那份能不能讀，
//! 而它讀不出來的那天症狀是「重新啟動之後所有 HTTPS 全掛」。

use crate::{AcmeManager, Report, Target};
use fulcrum_tls::renewal::{AriWindow, backoff_remaining, should_renew};
use fulcrum_tls::store::DomainLock;
use fulcrum_tls::{CertStore, Meta, to_loaded};
use instant_acme::{
    Account, AuthorizationStatus, CertificateIdentifier, ChallengeType, Identifier, NewOrder,
    OrderStatus, RetryPolicy,
};
use log::{debug, error, info, warn};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 不走 DNS-01 時，這一次用哪種挑戰。**G54：TLS-ALPN-01 主、HTTP-01 備。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonDnsChallenge {
    TlsAlpn01,
    Http01,
}

impl NonDnsChallenge {
    /// 寫進 `meta.json` 的名字，與 RFC 8555 的挑戰類型串一致。
    pub fn as_str(self) -> &'static str {
        match self {
            NonDnsChallenge::TlsAlpn01 => "tls-alpn-01",
            NonDnsChallenge::Http01 => "http-01",
        }
    }
}

/// 這一次該用主的還是備的。
///
/// ★ **這是 G54 那句「HTTP-01 是備」的全部落點**：只試 TLS-ALPN-01 的實作在 443 被擋住的
/// 機器上永遠簽不出來，而紀錄每輪只說「CA 說驗不過」。
///
/// 規則一條：**上一次失敗時用的那種，這一次換另一種。**
/// 成功即清空（否則一次偶發失敗會把網域永久釘在備用挑戰上）；
/// ⚠ 換挑戰**不重置**失敗計數，否則退避在「兩種都不通」時永遠長不大。
pub fn pick_non_dns_challenge(last_failed: Option<&str>) -> NonDnsChallenge {
    match last_failed {
        Some("tls-alpn-01") => NonDnsChallenge::Http01,
        // ★ 認不出來的值（舊版本寫的、手動改壞的）一律走主的——
        //   與「從來沒失敗過」同一處置，而不是當成錯誤。
        _ => NonDnsChallenge::TlsAlpn01,
    }
}

impl AcmeManager {
    /// **G59 第 2 條**：啟動時把每份 DNS 憑證檢核一遍。
    ///
    /// 回傳 `Err` = **拒絕啟動**。形狀照 G15：錯誤在啟動時暴露，不等被濫用才發現。
    ///
    /// ★ **兩種失敗處置相反，分法掛在類型上**（[`crate::provider::VerifyError`]）：
    /// `Fatal`（對端說不行 / 憑證讀不出來）⇒ 拒絕啟動；`Inconclusive`（沒連上）⇒ 打 error 繼續。
    ///
    /// ⚠ 後者不拒絕啟動，是因為一次網路抖動不該讓整臺機器上所有站台都起不來。
    /// ★ 這與「『沒能檢查』不許當成『檢查通過』」有張力 ⇒ 處置**不是悄悄跳過**，
    /// 而是打 error 說出「這份憑證沒被驗過」。**一處知情的取捨。**
    pub async fn verify_credentials(&self) -> Result<(), String> {
        let mut seen: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for target in &self.targets {
            let Some(dns01) = &target.dns01 else { continue };
            let key = (dns01.provider.name(), target.site.clone());
            if !seen.insert(key) {
                continue;
            }
            match dns01.provider.verify().await {
                Ok(()) => info!(
                    "站点 {} 的 {} 凭据校验通过（G59）",
                    target.site,
                    dns01.provider.name()
                ),
                Err(crate::provider::VerifyError::Fatal(m)) => {
                    return Err(format!("站点 {} 的 DNS 凭据不可用：{m}", target.site));
                }
                Err(crate::provider::VerifyError::Inconclusive(m)) => {
                    // ⚠ error 不是 warn：這份憑證**沒有被驗過**，而它有可能是壞的。
                    error!(
                        "⚠ 站点 {} 的 {} 凭据**没能校验**（{m}）—— 不拒绝启动（一次网络抖动\
                         不该让整台机器起不来），但它没被验过；签发时若失败，先查这条",
                        target.site,
                        dns01.provider.name()
                    );
                }
            }
        }
        Ok(())
    }

    /// 巡檢一輪：該簽的簽、該續的續、已經好的裝上。
    ///
    /// # ⚠ ⚠ 計數記在這一層，正體在 [`Self::poll_once`]
    ///
    /// 那一層有**三處早退**（沒有目標 / 這一批一個都接不了 / 帳戶拿不到）。
    /// 逐處記一遍遲早漏掉一處，而漏掉的那一處**不會有任何症狀**：
    /// `fulcrum_acme_issue_total` 照樣在漲，只是少了一類。
    /// ★ 與 `access_log::Record::finish` 同一條形狀 —— 把「一定要做的收尾」
    /// 放到一個**沒有早退分支**的外層，讓漏掉在結構上做不到。
    pub async fn run_once(&self) -> Report {
        let report = self.poll_once().await;
        self.issue_counts.record(&report);
        report
    }

    async fn poll_once(&self) -> Report {
        let mut report = Report::default();
        if self.targets.is_empty() {
            return report;
        }

        // ── 0. 先把這一批接不了的挑出來 ───────────────────────────────────
        //
        // ★ ★ **這一步必須在建帳戶之前。** 放在帳戶之後的話，一份只有通配符站台的
        //   設定會先去 CA 那邊建一個**一次都用不上的帳戶**；而 CA 連不上的時候，
        //   那些本該被記成「推遲」的網域會被記成「失敗」，進退避、進失敗計數、進告警。
        //   ⚠ **「這一批還沒做」與「這一次沒做成」是兩件事，混在一起會讓監控報假警。**
        let mut actionable: Vec<&Target> = Vec::new();
        for target in &self.targets {
            if target.actionable() {
                actionable.push(target);
            } else {
                // ⏳ 走到這裡只有一種情況：**通配符，但這個站台沒配 `tls { dns … }`**。
                //   通配符只能走 DNS-01（G54），而 DNS-01 需要有人去改 TXT。
                //   ★ 說清楚「誰、為什麼、怎麼辦」——一個只說「跳過」的紀錄，
                //     等於讓人去猜是設定寫錯了還是功能沒做。
                warn!(
                    "⏳ 站点 {} 的 {} 是通配符，只能走 DNS-01（G54/G58），而这个站点没有配 \
                     `tls {{ dns … }}` 与 `resolvers …` —— 它不会被签发，对它的 TLS 握手会被拒绝",
                    target.site, target.domain
                );
                report.deferred.push(target.domain.clone());
            }
        }
        if actionable.is_empty() {
            info!(
                "ACME 本轮：没有这一批接得了的域名（推迟 {}）",
                report.deferred.len()
            );
            return report;
        }

        // ★ 帳戶是**懶建**的：一份只有 `tls <cert> <key>` 的設定不該在啟動時
        //   去 CA 那邊建一個用不上的帳戶。走到這裡說明確實有網域要自動簽發。
        let account = match self
            .accounts
            .load_or_create(
                &self.issuer,
                &self.cfg.directory_url,
                self.cfg.contact.as_deref(),
            )
            .await
        {
            Ok(a) => a,
            Err(e) => {
                // ⚠ 帳戶拿不到 = 這一輪一個都簽不了。逐個網域記一遍失敗，
                //   而不是打一行「ACME 掛了」——後者在有十個網域時看不出影響面。
                error!("ACME 账户不可用：{e}");
                // ⚠ 只把**接得了的**那些記成失敗：推遲的那些與帳戶能不能用無關。
                for t in &actionable {
                    report.failed.push((t.domain.clone(), e.clone()));
                }
                report.note_next_check(Duration::from_secs(300));
                return report;
            }
        };

        for target in actionable {
            match self.ensure_one(&account, target, &mut report).await {
                Ok(()) => {}
                Err(e) => {
                    error!("ACME 处理 {} 失败：{e}", target.domain);
                    report.failed.push((target.domain.clone(), e));
                }
            }
        }

        info!(
            "ACME 本轮：签发 {}，已是最新 {}，推迟 {}，退避 {}，失败 {}；下次巡检 {}s 后",
            report.issued.len(),
            report.fresh.len(),
            report.deferred.len(),
            report.backed_off.len(),
            report.failed.len(),
            report.sleep_for().as_secs()
        );
        report
    }

    /// 請求強制續期一個網域（**G74**）。管理面調它。
    ///
    /// 回傳 `false` = **這個網域不在本處理程序的自動簽發目標裡**，什麼都沒做。
    /// ⚠ 判據必須是「它在不在 `targets` 裡」，不能只看網域格式 —— 一個靜靜接受任意網域
    /// 然後什麼都不發生的介面，比回報錯誤難查得多。
    ///
    /// # 兩檔，預設那一檔**不動退避**
    ///
    /// | `clear_backoff` | 越過「還不到時候」 | 越過退避 |
    /// |---|---|---|
    /// | `false`（預設）| ✅ | ❌ |
    /// | `true` | ✅ | ✅ **並把失敗計數清零** |
    ///
    /// ★ 預設不越過：一個隨手繞開退避的口子等於給「反覆重簽把 CA 配額燒光」開一扇門，
    /// 而配額**按帳戶**算。⇒ 第二檔要顯式 `"force": true`，且**每次清掉非零計數都打 warn**。
    /// ⚠ 沒有第二檔時，憑證補好後想立刻重試就只能手刪 `meta.json` 裡的失敗計數 ——
    /// 那是把標準動作定義成了危險動作（刪錯一個檔案就是刪掉證書）。
    pub fn request_renew(&self, domain: &str, clear_backoff: bool) -> bool {
        let d = domain.trim().to_ascii_lowercase();
        if !self
            .targets
            .iter()
            .any(|t| t.domain.eq_ignore_ascii_case(&d))
        {
            return false;
        }
        if let Ok(mut f) = self.force.lock() {
            // ★ 同一輪裡兩次請求取**較強**的那一檔：先 renew 再 force 不該被降級。
            let e = f.entry(d.clone()).or_insert(false);
            *e = *e || clear_backoff;
        }
        // ★ **先入隊再叫醒**。反過來的話，被叫醒的那一輪可能剛好讀到空佇列，
        //   然後一覺睡到 12 小時之後——而呼叫方那邊看起來「命令成功了」。
        self.wake.notify_one();
        if clear_backoff {
            info!("{d} 被要求强制续期（G74，**连退避一起清**），已叫醒巡检");
        } else {
            info!("{d} 被要求强制续期（G74），已叫醒巡检");
        }
        true
    }

    /// 這個網域這一輪是不是被強制續期了。**取走即清**（一次性）。
    ///
    /// 回傳 `None` = 沒被強制；`Some(clear_backoff)` = 被強制，值說明是哪一檔。
    ///
    /// ⚠ ⚠ **呼叫點必須在「有沒有證書」這個分支之外**。之前它只長在
    /// 「已經有一張證書」那一支裡，於是**對一個從來沒簽成過的網域按強制續期，
    /// 那個標記永遠不會被取走** —— 它會一直躺在佇列裡，直到某一輪證書終於簽出來了
    /// 才被消費掉，而那時沒有任何人還記得自己按過。
    fn take_forced(&self, domain: &str) -> Option<bool> {
        match self.force.lock() {
            Ok(mut f) => f.remove(domain),
            // 鎖中毒不該讓簽發停擺；當成「沒有強制」繼續走正常判定。
            Err(_) => None,
        }
    }

    /// 一直巡檢到收到停機信號。
    pub async fn run_loop(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                info!("ACME 巡检收到停机信号，退出");
                return;
            }
            let report = self.run_once().await;
            let nap = report.sleep_for();
            // ★ `select!` 而不是先 sleep 再看信號：一個 12 小時的 sleep 會把
            //   優雅停機拖成 12 小時——而排空視窗只有幾十秒。
            // ★ 第三條臂是 G74 的強制續期：沒有它，一次強制最長要等 12 小時才生效。
            tokio::select! {
                _ = tokio::time::sleep(nap) => {}
                _ = self.wake.notified() => {
                    debug!("巡检被强制续期请求叫醒");
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        info!("ACME 巡检收到停机信号，退出");
                        return;
                    }
                }
            }
        }
    }

    /// 一個網域的完整處置。
    async fn ensure_one(
        &self,
        account: &Account,
        target: &Target,
        report: &mut Report,
    ) -> Result<(), String> {
        let domain = target.domain.as_str();
        // ★ 抖動**一輪只取一次**：下面退避會被問到兩次（`should_renew` 裡一次、
        //   步驟 3.5 一次），兩次取到不同的抖動值會讓它們對同一件事給出不同答案，
        //   於是剛被 `should_renew` 放行的網域可能立刻被 3.5 攔下。
        let jitter = self.jitter.unit();

        // ── 1. 跨處理程序鎖（G55 第 2 條）。**先拿鎖，再讀儲存。**───────────────
        let _lock = self.lock_domain(domain).await?;

        // ── 2. 重讀儲存 ───────────────────────────────────────────────────
        let existing = self
            .store
            .load(&self.issuer, domain)
            .map_err(|e| format!("读证书存储失败：{e}"))?;

        // ★ ★ 續期狀態**單獨讀**，不從證書那份裡取。
        //   `load()` 缺 `cert.pem` 就回傳 `None`，於是一個從來沒簽成過的網域
        //   連它自己的失敗計數也讀不回來——每一輪都從「零次失敗」重新開始。
        let mut meta = self.store.load_meta(&self.issuer, domain);
        meta.issuer = self.issuer.clone();

        // ── 2.5 強制續期標記：**在分支之外取一次**（G74）───────────────────
        //
        // ★ 位置本身就是判據：放在「已經有一張證書」那一支裡，會讓一個從來沒簽成過的
        //   網域的強制請求永遠取不走（見 `take_forced` 的註解）。
        let forced = self.take_forced(domain);

        // ── 3. 已經有一張：先判它是不是我們這個 CA 簽的，再判要不要續 ────────
        let mut replaces: Option<CertificateIdentifier<'static>> = None;
        if let Some(sc) = existing {
            let foreign = match &sc.meta.issuer_url {
                // ★ `None` = 不是本處理程序簽的（手動放進來的，或舊版本寫的）。
                //   **不當成外來證書**：那會把使用者手動放進去的證書每次都重簽一遍。
                None => false,
                Some(url) => url != &self.cfg.directory_url,
            };
            if foreign {
                warn!(
                    "{domain} 存储里那张是 {} 签的，而现在配的是 {} —— 目录名撞了，按重签处理",
                    sc.meta.issuer_url.as_deref().unwrap_or("?"),
                    self.cfg.directory_url
                );
            } else {
                // ARI 優先（G56）。★ 問不到不是錯誤：CA 可能根本不支援。
                let ari = self.fetch_ari(account, &sc, &mut meta, &mut replaces).await;
                let decision = should_renew(
                    SystemTime::now(),
                    sc.not_before,
                    sc.not_after,
                    ari,
                    &meta.renewal,
                    self.backoff,
                    jitter,
                );
                report.note_next_check(decision.next_check);
                // ★ ★ **G74：強制續期在這裡越過續期判定**（不是越過退避，見下）。
                //   ⚠ 只越過「還不到時候」，**不越過 3.5 那道退避**：
                //   一個能繞過退避的強制口子，等於給「反覆按重簽按鈕把 CA 配額燒光」
                //   開了一扇門——而配額是**按帳戶**算的，燒光之後連累同機所有網域。
                //   ★ 所以現場表現是：強制之後若上一次剛失敗過，它會說「退避中」並給出剩餘秒數，
                //   而不是靜靜地什麼都不做。
                if forced.is_some() {
                    info!(
                        "{domain} 收到强制续期请求（G74），越过续期判定：{}",
                        decision.reason
                    );
                }
                if forced.is_none() && !decision.renew_now {
                    debug!("{domain} 暂不续期：{}", decision.reason);
                    self.install(sc)?;
                    report.fresh.push(domain.to_string());
                    // meta 裡可能剛記下新的 ARI 視窗，落一次盤（不動證書）。
                    let _ = self.store.save_meta(&self.issuer, domain, &meta);
                    return Ok(());
                }
                info!("{domain} 该续期了：{}", decision.reason);
            }
        }

        // ── 3.5 退避（否決項）★ 對「從來沒簽成過」的網域同樣有效 ─────────────
        //
        // ★ 走到這裡有兩種網域：要續期的，與**從來沒簽成過的**。⚠ 第二種恰恰是退避
        //   最該管的那一種（網域寫錯、80 不通、CAA 擋著 ⇒ 永遠簽不出來），
        //   而 CA 的失敗驗證配額**按帳戶**算 —— 一個簽不出來的網域能把整個帳戶耗光。
        // ⚠ 第二檔清零時**打 warn 而不是 info**：它把 CA 的失敗配額重新暴露出來，
        //   現場必須看得見是誰、清掉了多少。
        if forced == Some(true) && meta.renewal.failures > 0 {
            warn!(
                "{domain} 按请求清掉退避：失败计数 {} → 0（G74 的第二档）。\
                 ★ 这不是退避坏了，是有人显式要求的；\
                 ⚠ 根因没修好的话，下一次失败会从头开始重新长。",
                meta.renewal.failures
            );
            meta.renewal.failures = 0;
            meta.renewal.last_attempt = None;
            // 立刻寫入磁碟：中途崩了也不該讓計數又冒出來。
            let _ = self.store.save_meta(&self.issuer, domain, &meta);
        }
        if let Some(wait) =
            backoff_remaining(SystemTime::now(), &meta.renewal, self.backoff, jitter)
        {
            // ⚠ 這是 info 不是 warn：**退避正在工作**是正常狀態，不是異常 ——
            //   真正該報警的是那次失敗本身，而它已經報過了。
            info!(
                "{domain} 上次签发失败 {} 次，退避中，{}s 后再试（本轮不去试）",
                meta.renewal.failures,
                wait.as_secs()
            );
            report.backed_off.push(domain.to_string());
            report.note_next_check(wait);
            return Ok(());
        }

        // ── 4. 簽 ─────────────────────────────────────────────────────────
        //
        // ⚠ 無論成敗都要落一次 meta：失敗計數與上次嘗試時刻是退避的**唯一輸入**，
        //   不寫入磁碟就等於每次重新啟動都從「零次失敗」開始，退避形同虛設。
        meta.renewal.last_attempt = Some(unix_now());
        // ★ G54 的主/備在這裡選定。DNS-01 的站台不看它（通配符只有那一條路）。
        let challenge = pick_non_dns_challenge(meta.last_challenge_failed.as_deref());
        if target.dns01.is_none() {
            debug!("{domain} 本次用 {} 挑战", challenge.as_str());
        }
        match self.issue(account, target, replaces, challenge).await {
            Ok((cert_pem, key_pem)) => {
                meta.renewal.failures = 0;
                // ★ 成功即清空：一次偶發失敗不該把這個網域永久釘在備用挑戰上。
                meta.last_challenge_failed = None;
                meta.issuer_url = Some(self.cfg.directory_url.clone());
                // ★ ARI 視窗是**跟著某一張證書**的：新簽一張之後舊視窗立刻失效，
                //   留著它會讓新證書一上來就被判成「該續了」。
                meta.ari_start = None;
                meta.ari_end = None;
                self.store
                    .save(&self.issuer, domain, &cert_pem, &key_pem, &meta)
                    .map_err(|e| format!("证书写不进存储：{e}"))?;
                // 回讀一次再裝 —— 裝上去的必須是盤上那份。
                let sc = self
                    .store
                    .load(&self.issuer, domain)
                    .map_err(|e| format!("刚写下去的证书回读失败：{e}"))?
                    .ok_or_else(|| "刚写下去的证书回读时不见了".to_string())?;
                let not_after = sc.not_after;
                self.install(sc)?;
                info!("ACME 签发成功：{domain}（有效期至 {not_after:?}）");
                report.issued.push(domain.to_string());
                report.note_next_check(Duration::from_secs(3600));
                Ok(())
            }
            Err(e) => {
                meta.renewal.failures = meta.renewal.failures.saturating_add(1);
                // ★ 記下這次用的是哪種挑戰 —— 下一輪換另一種（G54 的「備」）。
                //   ⚠ 只對不走 DNS-01 的站台有意義；通配符換不了別的挑戰，
                //   記了反而會讓它下一輪去試一個它根本用不了的。
                if target.dns01.is_none() {
                    meta.last_challenge_failed = Some(challenge.as_str().to_string());
                }
                let wait = self
                    .backoff
                    .delay(meta.renewal.failures, self.jitter.unit());
                if let Err(e2) = self.store.save_meta(&self.issuer, domain, &meta) {
                    // meta 寫不進去時退避就失憶了 —— 說出來，別咽掉。
                    warn!("{domain} 的续期状态没写进去（{e2}），退避会从头算");
                }
                report.note_next_check(wait);
                Err(format!(
                    "{e}（连续第 {} 次失败，{}s 后再试）",
                    meta.renewal.failures,
                    wait.as_secs()
                ))
            }
        }
    }

    /// 走一遍 RFC 8555 的下單流程，回傳 `(证书链 PEM, 私钥 PEM)`。
    ///
    /// ★ ★ **DNS-01 留下的 TXT 必須被摘掉，而摘不掉不能讓簽發失敗**——證書已經到手了。
    /// 所以這裡不用 `Drop` 守衛（析構裡不能 await），而是把清理寫成顯式的一段，
    /// **成功與失敗兩條路都會走到**。
    async fn issue(
        &self,
        account: &Account,
        target: &Target,
        replaces: Option<CertificateIdentifier<'_>>,
        challenge: NonDnsChallenge,
    ) -> Result<(String, String), String> {
        let mut planted: Vec<(String, String)> = Vec::new();
        let out = self
            .issue_inner(account, target, replaces, challenge, &mut planted)
            .await;
        // ⚠ 清理排在**拿到結果之後**：提前清會讓 CA 在 finalize 之後回來複驗時撲空，
        //   而那時的錯誤資訊只說「驗不過」。
        if let Some(dns01) = &target.dns01 {
            for (name, value) in &planted {
                if let Err(e) = dns01.provider.clear_txt(name, value).await {
                    // ★ 只 warn：留一條 TXT 是衛生問題，不是可用性問題。
                    //   升級成錯誤會讓一次**成功的簽發**被記成失敗 ⇒ 進退避、進告警。
                    warn!("{name} 的挑战 TXT 没摘干净（{e}）—— 不影响服务，但要有人清");
                }
            }
        }
        out
    }

    async fn issue_inner(
        &self,
        account: &Account,
        target: &Target,
        replaces: Option<CertificateIdentifier<'_>>,
        challenge_kind: NonDnsChallenge,
        planted: &mut Vec<(String, String)>,
    ) -> Result<(String, String), String> {
        let domain = target.domain.as_str();
        let identifiers = [Identifier::Dns(domain.to_string())];
        let ids = &identifiers;
        let mut order = new_order_with_fallback(domain, replaces, |replaces| async move {
            let mut new_order = NewOrder::new(ids);
            if let Some(id) = replaces {
                // ★ 只有確認 CA 支援 ARI 時才帶上它：`new_order` 在 CA 不支援時會**直接回報錯誤**，
                //   於是「順手帶一個能拿到優惠速率的欄位」會變成「換個 CA 就一張都簽不出來」。
                new_order = new_order.replaces(id);
            }
            account.new_order(&new_order).await
        })
        .await
        .map_err(|e| format!("下单失败：{e}"))?;

        // ── 解挑戰 ────────────────────────────────────────────────────────
        //
        // ★ 守衛收在一個 Vec 裡，活到訂單 ready 為止。token 摘早了，
        //   CA 來驗的時候就 404 —— 而那時的錯誤資訊只說「驗不過」。
        let mut provisioned = Vec::new();
        // ★ TLS-ALPN-01 的守衛是另一個類型（掛的是證書不是 token），單獨收一個 Vec。
        //   兩個 Vec 都活到訂單 ready 為止。
        let mut challenge_certs = Vec::new();
        {
            let mut authorizations = order.authorizations();
            while let Some(result) = authorizations.next().await {
                let mut authz = result.map_err(|e| format!("取授权失败：{e}"))?;
                match authz.status {
                    AuthorizationStatus::Valid => continue,
                    AuthorizationStatus::Pending => {}
                    other => return Err(format!("授权状态是 {other:?}，签不下去")),
                }
                if let Some(dns01) = &target.dns01 {
                    // ── DNS-01（G54 裡通配符唯一可行的那條路 / G57 / G58）──────
                    let Some(mut challenge) = authz.challenge(ChallengeType::Dns01) else {
                        return Err(format!("CA 没给 DNS-01 挑战 —— 域名 {domain}"));
                    };
                    let record = target.challenge_record();
                    let value = challenge.key_authorization().dns_value();
                    dns01
                        .provider
                        .set_txt(&record, &value)
                        .await
                        .map_err(|e| format!("写 TXT 失败（{}）：{e}", dns01.provider.name()))?;
                    // ★ **先記進清理清單，再去等可見**：中間任何一步失敗，
                    //   那條已經寫上去的 TXT 都必須被摘掉。反過來寫會漏掉失敗路徑。
                    planted.push((record.clone(), value.clone()));

                    // ★ ★ ★ G58 那條硬約束就落在這一行：**真去問權威 NS，不 sleep**。
                    //   固定 sleep 在快的時候浪費時間、在慢的時候直接簽失敗，
                    //   而失敗要消耗 CA 的速率配額。
                    dns01.checker.wait_visible(&record, &value).await?;

                    debug!("{domain}：{record} 的 TXT 已在权威 NS 上可见，通知 CA 来验");
                    challenge
                        .set_ready()
                        .await
                        .map_err(|e| format!("通知 CA 验 DNS-01 失败：{e}"))?;
                } else {
                    // ── TLS-ALPN-01（G54 的「主」，RFC 8737）───────────────
                    //
                    // ★ 整件事在 TLS 握手裡完成：CA 連過來、ALPN 只提 `acme-tls/1`、
                    //   看一眼我們回的那張自簽證書裡的 acmeIdentifier 擴充就斷開。
                    //   **零路由佔用** —— 使用者的設定擋不住自己的證書簽發。
                    //
                    // ⚠ **`authz.challenge()` 一個授權只能調一次**（它借的是 handle
                    //   自己的 `'a`）⇒「先試這個不行再試那個」直接編不過（E0499）。
                    //   → 先用不可變視圖看清 CA 給了哪幾種，決定之後**只取一次 handle**。
                    let has_tls_alpn = authz
                        .challenges
                        .iter()
                        .any(|c| c.r#type == ChallengeType::TlsAlpn01);
                    let use_tls_alpn = challenge_kind == NonDnsChallenge::TlsAlpn01 && has_tls_alpn;
                    if challenge_kind == NonDnsChallenge::TlsAlpn01 && !has_tls_alpn {
                        // ★ CA 不提供這種挑戰不是錯誤，落回備用的那條。說出來即可。
                        debug!("{domain}：CA 没给 TLS-ALPN-01 挑战，落回 HTTP-01");
                    }
                    let want = if use_tls_alpn {
                        ChallengeType::TlsAlpn01
                    } else {
                        ChallengeType::Http01
                    };
                    let Some(mut challenge) = authz.challenge(want) else {
                        return Err(format!(
                            "CA 既没给 HTTP-01 也没给可用的 TLS-ALPN-01 挑战，\
                             而这个站点也没配 `tls {{ dns … }}` —— 域名 {domain}"
                        ));
                    };
                    let key_auth = challenge.key_authorization();
                    if use_tls_alpn {
                        // ⚠ 驗證連接用的 SNI 是**被授權的那個名字**，不是站台名。
                        //   通配符站台走的是 DNS-01，不會落到這一支。
                        let digest = key_auth.digest();
                        challenge_certs.push(crate::tlsalpn01::provision(
                            &self.resolver,
                            domain,
                            digest.as_ref(),
                        )?);
                        // ★ ★ **info 而不是 debug，理由與 G58 那條輪詢紀錄一模一樣**：
                        //   「這一次用的是哪種挑戰」是**唯一**可從外部觀察到的痕跡。
                        //   壓成 debug 就等於把判據藏起來——而維運排查
                        //   「為什麼這臺機器簽不出證書」時，第一個要知道的就是它走了哪條路。
                        info!("{domain}：本次挑战走 TLS-ALPN-01（G54 的主），挑战证书已挂上");
                    } else {
                        // ── HTTP-01（G54 的「備」）──────────────────────────
                        let token = challenge.token.clone();
                        provisioned.push(self.http01.provision(&token, key_auth.as_str()));
                        // ⚠ token **不進 info 紀錄**：它連著 key authorization，
                        //   而紀錄會被貼進 issue、進集中式紀錄系統。要看它請開 debug。
                        info!("{domain}：本次挑战走 HTTP-01（G54 的备）");
                        debug!("{domain}：HTTP-01 token {token} 已就位");
                    }
                    let kind_str = if use_tls_alpn {
                        "TLS-ALPN-01"
                    } else {
                        "HTTP-01"
                    };
                    challenge
                        .set_ready()
                        .await
                        .map_err(|e| format!("通知 CA 验 {kind_str} 失败：{e}"))?;
                }
            }
        }

        let status = order
            .poll_ready(&RetryPolicy::default())
            .await
            .map_err(|e| format!("等订单就绪失败：{e}"))?;
        if status != OrderStatus::Ready {
            // ★ ★ ★ **CA 已經把「為什麼」寫在授權裡了，不去取它，現場就只剩「Invalid」三個字。**
            //
            //   在 `example.com` 上真的被這句話擋了一次：DNS-01 的 TXT
            //   明明已在三臺權威 NS 上可見，訂單卻回 Invalid，而紀錄裡**沒有一個字**
            //   說得出 CA 到底在抱怨什麼 —— 那一刻手上唯一的線索是「換個地方猜」。
            //   ⚠ RFC 8555 §7.1.4/§6.7 的 problem document 就掛在挑戰上（`challenge.error`），
            //   取一次就有；不取，就等於**把對端說過的話丟掉，然後自己去猜**。
            //
            //   ★ 只在**失敗路徑**上多問一次授權：成功路徑一個請求都不多發。
            let mut why: Vec<String> = Vec::new();
            let mut authorizations = order.authorizations();
            while let Some(result) = authorizations.next().await {
                match result {
                    Ok(authz) => {
                        let ident = format!("{}", authz.identifier());
                        for c in &authz.challenges {
                            if let Some(p) = &c.error {
                                why.push(format!(
                                    "{ident} 的 {:?}：{}{}",
                                    c.r#type,
                                    p.detail.as_deref().unwrap_or("（CA 没给 detail）"),
                                    p.r#type
                                        .as_deref()
                                        .map(|t| format!("（{t}）"))
                                        .unwrap_or_default(),
                                ));
                            }
                        }
                        if why.is_empty() {
                            why.push(format!("{ident} 的授权状态是 {:?}", authz.status));
                        }
                    }
                    // ⚠ 取不到就說取不到，別讓它變成「沒有原因」。
                    Err(e) => why.push(format!("取授权详情失败：{e}")),
                }
            }
            return Err(format!(
                "订单最终状态是 {status:?}，不是 Ready —— CA 给的原因：{}",
                why.join("；")
            ));
        }

        let key_pem = order
            .finalize()
            .await
            .map_err(|e| format!("finalize 失败：{e}"))?;
        let cert_pem = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(|e| format!("取证书失败：{e}"))?;
        // ★ 守衛在這裡才析構：CA 有可能在 finalize 之後還回來驗一次。
        drop(provisioned);
        drop(challenge_certs);
        Ok((cert_pem, key_pem))
    }

    /// 問一次 ARI（RFC 9773）。問不到就回傳 `None`，**不算失敗**。
    async fn fetch_ari(
        &self,
        account: &Account,
        sc: &fulcrum_tls::StoredCert,
        meta: &mut Meta,
        replaces: &mut Option<CertificateIdentifier<'static>>,
    ) -> Option<AriWindow> {
        let leaf = sc.chain.first()?;
        let id = match ari_cert_id(leaf) {
            Ok(id) => id,
            Err(e) => {
                debug!("{e}");
                return None;
            }
        };
        match account.renewal_info(&id).await {
            Ok((info, _retry_after)) => {
                let start = info.suggested_window.start.unix_timestamp().max(0) as u64;
                let end = info.suggested_window.end.unix_timestamp().max(0) as u64;
                meta.ari_start = Some(start);
                meta.ari_end = Some(end);
                // ★ ARI 能問到 ⇒ 這個 CA 支援 ARI ⇒ 續期時可以帶 `replaces`。
                //   這條因果是**實測出來的**，不是推的：`new_order` 在 CA 不支援時直接回報錯誤。
                *replaces = Some(id);
                if let Some(url) = &info.explanation_url {
                    info!("CA 给了 ARI 窗口，理由页：{url}");
                }
                Some(AriWindow {
                    start: UNIX_EPOCH + Duration::from_secs(start),
                    end: UNIX_EPOCH + Duration::from_secs(end),
                })
            }
            Err(e) => {
                // ⚠ 這裡**必須**是 debug 而不是 warn：絕大多數 CA 不支援 ARI，
                //   把它打成警告會讓紀錄裡每天都有一條假警報——而假警報會把真的埋掉。
                debug!("问不到 ARI（多半是这个 CA 不支持）：{e}");
                meta.ari_start = None;
                meta.ari_end = None;
                None
            }
        }
    }

    /// 拿一個網域的跨處理程序簽發鎖。
    ///
    /// ★ `flock` 是**阻塞**的，扔到 blocking 池裡。直接在 runtime 執行緒上等，
    /// 等的時候這條執行緒上的**所有**請求都停著——而它等的正是另一代處理程序簽完一張證書。
    async fn lock_domain(&self, domain: &str) -> Result<DomainLock, String> {
        let root: PathBuf = self.store.root().to_path_buf();
        let issuer = self.issuer.clone();
        let domain = domain.to_string();
        tokio::task::spawn_blocking(move || CertStore::new(root).lock(&issuer, &domain))
            .await
            .map_err(|e| format!("拿签发锁的任务没跑完：{e}"))?
            .map_err(|e| format!("拿不到签发锁：{e}"))
    }

    /// 把一張證書裝到 SNI 解析器上（熱裝，立刻對新握手生效）。
    ///
    /// ★ 按所有權接，不按引用接：`CertifiedKey` 本來就要持有自己的那份鏈與金鑰，
    /// 接引用只會逼著這裡克隆一遍，然後把「誰擁有這份私鑰」變模糊。
    fn install(&self, sc: fulcrum_tls::StoredCert) -> Result<(), String> {
        let loaded = to_loaded(sc)?;
        if loaded.domains.is_empty() {
            // ★ 一張沒有 DNS SAN 的證書裝上去也永遠挑不中——**靜靜地裝完**是最難查的形態：
            //   紀錄說「裝載成功」，而每一次握手都被拒絕。
            warn!("要装的证书里没有任何 DNS SAN —— 它不会被任何 SNI 选中");
            return Ok(());
        }
        self.resolver.install(&loaded.domains, loaded.key);
        Ok(())
    }
}

/// 一張證書的 ARI 標識（RFC 9773 §4.1）：`base64url(AKI keyIdentifier) "." base64url(序列号)`。
///
/// ⚠ 這裡多一次 DER 往返：`instant-acme` 的 `CertificateIdentifier` 只有
///   `TryFrom<&CertificateDer>` 這一個入口，而我們手裡是 BoringSSL 的 `X509`。
/// ★ **不自己摳 AKI 與序列號** —— ARI 標識摳錯的後果是「續期請求被 CA 靜靜忽略」。
/// ⚠ `rustls-pki-types` 因此是本 crate 的直接相依（**新增 0 個包**）：它是純類型
///   定義 crate，沒有 crypto / provider / default feature，與 `rustls` 不是一類東西。
/// ★ ★ 序列號取的是 DER INTEGER 的內容位元組：首位元組 ≥ 0x80 時**帶著**那個符號位元組 `00`，
///   RFC 9773 §4.1 的原例（`00:87:65:43:21` → `AIdlQyE`）就是這樣。
///   ⛔ 不許為了遷就哪個 CA 把它剝掉（G166）：剝了就是對規範 CA 發錯標識。
///   CA 不認的那一頭由 [`new_order_with_fallback`] 兜。
fn ari_cert_id(
    leaf: &pingora_boringssl::x509::X509,
) -> Result<CertificateIdentifier<'static>, String> {
    let der = leaf.to_der().map_err(|e| format!("证书转不回 DER：{e}"))?;
    let der = rustls_pki_types::CertificateDer::from(der);
    CertificateIdentifier::try_from(&der)
        .map(CertificateIdentifier::into_owned)
        .map_err(|e| format!("从证书里抠不出 ARI 标识：{e}"))
}

/// 下單；帶著 `replaces` 被 CA 拒了，就去掉 `replaces` 再下一次（G166）。
///
/// ★ ★ 不退回的後果是**這個網域永遠續不上**：失敗進退避，下一輪 `fetch_ari` 拿同一張證書
///   算出同一個 `replaces`，CA 再拒 —— 一直到證書過期。`replaces` 換來的只是 CA 那邊的
///   速率優惠，不值得拿續期本身去賭。
/// ⚠ 真碰上過的一例：pebble v2.10.1 對序列號首位元組 ≥ 0x80 的證書（約 1/255）一律回
///   `serverInternal`（它存訂單的鍵不帶符號位元組，查的時候卻帶著；上游 issue #550）。
/// ⚠ 只認 CA 回的 problem 文件（`Error::Api`）：連不上、超時這類，去掉 `replaces`
///   也照樣失敗，交給退避。⛔ 也不收窄成「只認 `alreadyReplaced`」—— 上面那一例就不是它。
async fn new_order_with_fallback<'a, T, F, Fut>(
    domain: &str,
    replaces: Option<CertificateIdentifier<'a>>,
    mut place: F,
) -> Result<T, instant_acme::Error>
where
    F: FnMut(Option<CertificateIdentifier<'a>>) -> Fut,
    Fut: Future<Output = Result<T, instant_acme::Error>>,
{
    let Some(id) = replaces else {
        return place(None).await;
    };
    match place(Some(id)).await {
        Err(instant_acme::Error::Api(problem)) => {
            // ⚠ `tests/acme/renew.sh` 逐字 grep 這句的開頭「带 replaces 下单被 CA 拒了」判「續期沒走退回」
            //   ⇒ 改字形或措辭要兩處一起改，否則那條判據會恆綠。
            warn!(
                "{domain} 带 replaces 下单被 CA 拒了（{problem}），去掉 replaces 再下一次 \
                 —— 照常签，只是拿不到 ARI 续期的速率优惠"
            );
            place(None).await
        }
        other => other,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{NonDnsChallenge, pick_non_dns_challenge};
    use crate::{AcmeConfig, AcmeManager, Http01Store, Report, Target};
    use fulcrum_tls::SniResolver;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn 没有目标时一轮什么都不做() {
        let cfg = AcmeConfig::new(None, None, "/nonexistent");
        let m = AcmeManager::new(
            cfg,
            Arc::new(SniResolver::new()),
            Arc::new(Http01Store::new()),
            Vec::new(),
        );
        // ★ 判據掛在「不會去建帳戶」上：一份只有手動證書的設定，
        //   不該因為起了個後台任務就去 CA 那邊留一條記錄。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = rt.block_on(m.run_once());
        assert!(report.is_empty());
        assert_eq!(report.next_check, None);
        // ★ **批 M** 的反向那半：一輪「什麼都沒有」不許在簽發計數上留下任何一筆。
        assert_eq!(m.issue_counts().snapshot(), (0, 0, 0));
    }

    #[test]
    fn 通配符被推迟而不是被当成失败() {
        // ⚠ 兩者的區別不是措辭：失敗會進退避、會計數、會在監控裡報警，
        //   而「這一批還沒做」不該產生任何一條告警。
        let cfg = AcmeConfig::new(Some("https://localhost:1/dir"), None, "/nonexistent");
        let m = AcmeManager::new(
            cfg,
            Arc::new(SniResolver::new()),
            Arc::new(Http01Store::new()),
            vec![Target {
                domain: "*.example.com".into(),
                site: "s".into(),
                // ★ 沒配 DNS-01 —— 這正是「推遲」的那一種。
                dns01: None,
            }],
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = rt.block_on(m.run_once());
        assert_eq!(report.deferred, vec!["*.example.com".to_string()]);
        assert!(report.failed.is_empty(), "通配符被记成失败了：{report:?}");
        assert!(report.issued.is_empty());
        // ★ ★ **批 M**：這一輪走的是「一個都接不了」那處**早退**。
        //   計數記在 `run_once` 外層，所以早退照樣記得到 —— 記在 `poll_once` 裡面的話，
        //   這一格會是 `(0,0,0)`，而 `fulcrum_acme_issue_total` 不會有任何異樣。
        assert_eq!(m.issue_counts().snapshot(), (0, 0, 1));
    }

    #[test]
    fn 强制续期两档_默认不清退避_force_才清() {
        let cfg = AcmeConfig::new(Some("https://localhost:1/dir"), None, "/nonexistent");
        let m = AcmeManager::new(
            cfg,
            Arc::new(SniResolver::new()),
            Arc::new(Http01Store::new()),
            vec![Target {
                domain: "a.example.com".into(),
                site: "s".into(),
                dns01: None,
            }],
        );

        // ① 不在目標裡 ⇒ 什麼都不做，而且**不入隊**（靜靜接受是最難查的那種）。
        assert!(!m.request_renew("nope.example.com", true));
        assert_eq!(m.take_forced("nope.example.com"), None);

        // ② 預設檔
        assert!(m.request_renew("a.example.com", false));
        assert_eq!(m.take_forced("a.example.com"), Some(false));
        // ★ 取走即清：第二次就沒有了。
        assert_eq!(m.take_forced("a.example.com"), None);

        // ③ 第二檔
        assert!(m.request_renew("a.example.com", true));
        assert_eq!(m.take_forced("a.example.com"), Some(true));

        // ④ ★ 同一輪裡兩次請求取**較強**的那一檔，兩個方向都驗：
        //   否則「先按了 force、又按了一次普通」會把 force 悄悄降級。
        assert!(m.request_renew("a.example.com", false));
        assert!(m.request_renew("a.example.com", true));
        assert_eq!(m.take_forced("a.example.com"), Some(true));
        assert!(m.request_renew("a.example.com", true));
        assert!(m.request_renew("a.example.com", false));
        assert_eq!(m.take_forced("a.example.com"), Some(true));

        // ⑤ 大小寫與空白按目標那邊的口徑歸一
        assert!(m.request_renew("  A.Example.COM  ", true));
        assert_eq!(m.take_forced("a.example.com"), Some(true));
    }

    #[test]
    fn 账户拿不到时强制标记不被消费() {
        // ★ 這一條釘的是**取標記的時機**：帳戶都拿不到的那一輪壓根沒試過，
        //   所以標記要留著等下一輪。⚠ 取走等於把一次「什麼都沒發生」記成
        //   「你的請求已經用掉了」，而呼叫方那邊看起來完全成功。
        // （「取標記的位置」另有判據：`take_forced` 必須在「已經有證書」那個分支之外取，
        //   否則從沒簽成過的網域按下強制之後標記永遠取不走。）
        let cfg = AcmeConfig::new(Some("https://localhost:1/dir"), None, "/nonexistent");
        let m = AcmeManager::new(
            cfg,
            Arc::new(SniResolver::new()),
            Arc::new(Http01Store::new()),
            vec![Target {
                domain: "never.example.com".into(),
                site: "s".into(),
                dns01: None,
            }],
        );
        assert!(m.request_renew("never.example.com", true));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = rt.block_on(m.run_once());
        assert!(
            !report.failed.is_empty(),
            "前提没成立：这一轮本该因为账户拿不到而记失败"
        );
        assert_eq!(
            m.take_forced("never.example.com"),
            Some(true),
            "★ 标记被一轮「什么都没试」的巡检吃掉了 —— 调用方会以为已经续过了"
        );
        // ★ ★ **批 M**：這一輪走的是「帳戶拿不到」那處**早退**，一個網域記一次失敗。
        assert_eq!(m.issue_counts().snapshot(), (0, 1, 0));
    }

    #[test]
    fn 睡多久被夹在上下界之间() {
        let mut r = Report::default();
        assert_eq!(r.sleep_for(), crate::MAX_IDLE, "没有建议时要有上界兜底");
        r.note_next_check(Duration::from_secs(1));
        assert_eq!(r.sleep_for(), crate::MIN_IDLE, "下界挡不住立刻重来");
        r.note_next_check(Duration::from_secs(9_999_999));
        assert_eq!(
            r.sleep_for(),
            crate::MIN_IDLE,
            "取的是最紧的那条，不是最后一条"
        );
        let mut r2 = Report::default();
        r2.note_next_check(Duration::from_secs(9_999_999));
        assert_eq!(r2.sleep_for(), crate::MAX_IDLE);
    }

    // ── G54 的主/備 ─────────────────────────────────────────────────────────

    #[test]
    fn 默认走主的_tls_alpn_01() {
        // ★ G54 明寫 TLS-ALPN-01 是「主」。它零路由佔用，而且只要 443 ——
        //   對 80 連接埠常年受限的 `.cn` 機房尤其要緊。
        assert_eq!(pick_non_dns_challenge(None), NonDnsChallenge::TlsAlpn01);
    }

    #[test]
    fn 主的失败过一次就换备的() {
        // ★ ★ 這一條就是「HTTP-01 是**備**」的全部含義。沒有它，「主/備」只是措辭：
        //   一個永遠只試 TLS-ALPN-01 的實作，在 443 被擋住的機器上**永遠簽不出來**，
        //   而紀錄裡每一輪都只說「CA 說驗不過」。
        assert_eq!(
            pick_non_dns_challenge(Some("tls-alpn-01")),
            NonDnsChallenge::Http01
        );
    }

    #[test]
    fn 备的也失败过就换回主的() {
        // ⚠ 判據是**兩個方向都要動**：一個「失敗過就永遠用 HTTP-01」的實作
        //   在上一條裡表現完全相同，而它會讓一臺只有 443 的機器永遠簽不出來。
        assert_eq!(
            pick_non_dns_challenge(Some("http-01")),
            NonDnsChallenge::TlsAlpn01
        );
    }

    #[test]
    fn 认不出来的值当成没失败过() {
        // 舊版本寫的、手動改壞的 meta.json 不該讓簽發停擺。
        assert_eq!(
            pick_non_dns_challenge(Some("dns-01")),
            NonDnsChallenge::TlsAlpn01
        );
        assert_eq!(pick_non_dns_challenge(Some("")), NonDnsChallenge::TlsAlpn01);
    }

    #[test]
    fn 写进_meta_的名字就是_rfc_8555_的挑战类型串() {
        // ★ 這兩個串會落進 `meta.json`，而下一輪靠它們分岔。
        //   寫錯一個字母的後果不是回報錯誤，是**主/備永遠不切換**——沒有任何症狀。
        assert_eq!(NonDnsChallenge::TlsAlpn01.as_str(), "tls-alpn-01");
        assert_eq!(NonDnsChallenge::Http01.as_str(), "http-01");
        // 自證：這兩個串真的能被 `pick` 認回來（否則上面兩條斷言等於沒測）。
        assert_eq!(
            pick_non_dns_challenge(Some(NonDnsChallenge::TlsAlpn01.as_str())),
            NonDnsChallenge::Http01
        );
        assert_eq!(
            pick_non_dns_challenge(Some(NonDnsChallenge::Http01.as_str())),
            NonDnsChallenge::TlsAlpn01
        );
    }

    // ── ARI 標識與 `replaces` 退回（G166）────────────────────────────────

    /// 序列號 `0xb36f9903f33bc7`（首位元組 ≥ 0x80）：2026-10-16 全量門禁裡真撞上的那一個。
    /// 固定位元組，`openssl x509 -set_serial` 簽的，AKI = `5f8a17d7…83d2`。
    const 高位序列号证书: &str = "-----BEGIN CERTIFICATE-----
MIIBgTCCASegAwIBAgIIALNvmQPzO8cwCgYIKoZIzj0EAwIwFzEVMBMGA1UEAwwM
QVJJIHJlcHJvIENBMB4XDTI2MTAxNjA2NDk0MloXDTM2MTAxMzA2NDk0MlowGDEW
MBQGA1UEAwwNcmVwcm8uZXhhbXBsZTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IA
BLENn0DpwTsh7JrVLX+DsqnTvL3TL8JGf2Bl7DRU+xJBLZ3ko749XMy/5rG/oq/7
eUvm5FNCJ8agczycd4azwCyjXDBaMBgGA1UdEQQRMA+CDXJlcHJvLmV4YW1wbGUw
HwYDVR0jBBgwFoAUX4oX19Tqp1+ctI6gLUtWdqoPg9IwHQYDVR0OBBYEFAqqk7v6
Xj//9sW2CrzStr98SqjkMAoGCCqGSM49BAMCA0gAMEUCIDteCExkVKCEc2hR2HJN
flqVKGGOMVOnsjMhB5F7yFMCAiEA1s8HbiyZwpsuEKCKtkyuSXtZ3hxS55Wqj5BG
E8VL9Ew=
-----END CERTIFICATE-----
";
    /// 對照：同一個 CA、同樣的序列號去掉最高位（`0x336f9903f33bc7`）。
    const 低位序列号证书: &str = "-----BEGIN CERTIFICATE-----
MIIBgDCCASagAwIBAgIHM2+ZA/M7xzAKBggqhkjOPQQDAjAXMRUwEwYDVQQDDAxB
UkkgcmVwcm8gQ0EwHhcNMjYxMDE2MDY0OTQyWhcNMzYxMDEzMDY0OTQyWjAYMRYw
FAYDVQQDDA1yZXByby5leGFtcGxlMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE
in5ClQn7qSYDK4IS0XgjENo5BvX3aV07a8bjrMhy8bpjGmFw4nK5IkVDhdar1ytE
GEu6el5CBR/vaE+uh+lsxqNcMFowGAYDVR0RBBEwD4INcmVwcm8uZXhhbXBsZTAf
BgNVHSMEGDAWgBRfihfX1OqnX5y0jqAtS1Z2qg+D0jAdBgNVHQ4EFgQUQRALGzGv
PsdvzDy7WB46xiKFqE0wCgYIKoZIzj0EAwIDSAAwRQIhAKi5LMG05tSnygG/wvu3
KhXWlp3Zpoy7tOIyxEANfobAAiBfH2c6qafOxJWYpTpyn2qkK/QwyWF2ajgozLZq
mYRjTQ==
-----END CERTIFICATE-----
";

    #[test]
    fn ari_标识照_rfc_9773_原例编码() {
        use instant_acme::CertificateIdentifier;
        use rustls_pki_types::Der;
        // RFC 9773 §4.1 原文的例子：序列號 0x87654321 的 DER 內容是 `00:87:65:43:21`
        //   （補一個 `00` 讓它保持正數）⇒ 標識裡的序列號是 `AIdlQyE`。
        let aki = [
            0x69, 0x88, 0x5b, 0x6b, 0x87, 0x46, 0x40, 0x41, 0xe1, 0xb3, 0x7b, 0x84, 0x7b, 0xa0,
            0xae, 0x2c, 0xde, 0x01, 0xc8, 0xd4,
        ];
        let id = CertificateIdentifier::new(
            Der::from_slice(&aki),
            Der::from_slice(&[0x00, 0x87, 0x65, 0x43, 0x21]),
        );
        assert_eq!(id.to_string(), "aYhba4dGQEHhs3uEe6CuLN4ByNQ.AIdlQyE");
    }

    #[test]
    fn 高位序列号的_ari_标识带着符号字节() {
        use super::ari_cert_id;
        use pingora_boringssl::x509::X509;
        // ★ 走的是 `fetch_ari` 那條真路徑：BoringSSL `X509` → DER → `CertificateIdentifier`。
        // `ALNvmQPzO8c` = `00 b3 6f 99 03 f3 3b c7`。⚠ 剝掉 `00` 會變成 `s2-ZA_M7xw`，
        //   pebble v2.10.1 認那個，規範 CA 不認 —— ⛔ 不許為了前者改成後者（G166）。
        let high = X509::from_pem(高位序列号证书.as_bytes()).unwrap();
        assert_eq!(
            ari_cert_id(&high).unwrap().to_string(),
            "X4oX19Tqp1-ctI6gLUtWdqoPg9I.ALNvmQPzO8c"
        );
        // 對照：首位元組 < 0x80 時沒有符號位元組 —— 證明上面那個 `00` 不是每次都補的。
        let low = X509::from_pem(低位序列号证书.as_bytes()).unwrap();
        assert_eq!(
            ari_cert_id(&low).unwrap().to_string(),
            "X4oX19Tqp1-ctI6gLUtWdqoPg9I.M2-ZA_M7xw"
        );
    }

    fn 跑完(
        f: impl Future<Output = Result<u8, instant_acme::Error>>,
    ) -> Result<u8, instant_acme::Error> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn 某个标识() -> instant_acme::CertificateIdentifier<'static> {
        use rustls_pki_types::Der;
        instant_acme::CertificateIdentifier::new(Der::from_slice(b"aki"), Der::from_slice(b"sn"))
    }

    /// pebble v2.10.1 那一句拒絕的形狀（`serverInternal` 不是 `alreadyReplaced`）；`detail` 只取了原話的前半。
    fn ca_的拒绝() -> instant_acme::Error {
        instant_acme::Error::Api(
            serde_json::from_str(
                r#"{"type":"urn:ietf:params:acme:error:serverInternal",
                    "detail":"could not find an order for the given certificate","status":500}"#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn 带_replaces_被_ca_拒了就去掉它再下一次() {
        use super::new_order_with_fallback;
        let mut 下单时带没带 = Vec::new();
        let r = 跑完(new_order_with_fallback(
            "a.example",
            Some(某个标识()),
            |replaces| {
                下单时带没带.push(replaces.is_some());
                let res = if replaces.is_some() {
                    Err(ca_的拒绝())
                } else {
                    Ok(7)
                };
                async move { res }
            },
        ));
        assert_eq!(r.unwrap(), 7);
        // ★ 恰好兩次，第二次**沒帶**：判據掛在呼叫上，不只掛在結果上。
        assert_eq!(下单时带没带, [true, false]);
    }

    #[test]
    fn 没带_replaces_时被拒就照实报错() {
        use super::new_order_with_fallback;
        let mut 下单时带没带 = Vec::new();
        let r = 跑完(new_order_with_fallback("a.example", None, |replaces| {
            下单时带没带.push(replaces.is_some());
            async move { Err(ca_的拒绝()) }
        }));
        assert!(r.is_err());
        // 沒有可去掉的東西 ⇒ 不許再下第二次（那只是把同一個請求重發一遍）。
        assert_eq!(下单时带没带, [false]);
    }

    #[test]
    fn 不是_ca_回的拒绝就不回退() {
        use super::new_order_with_fallback;
        let mut 下单时带没带 = Vec::new();
        let r = 跑完(new_order_with_fallback(
            "a.example",
            Some(某个标识()),
            |replaces| {
                下单时带没带.push(replaces.is_some());
                async move { Err(instant_acme::Error::Str("connection reset")) }
            },
        ));
        assert!(r.is_err());
        // ⚠ 連不上去掉 `replaces` 也照樣連不上 —— 交給退避，不在這裡多打一次。
        assert_eq!(下单时带没带, [true]);
    }
}
