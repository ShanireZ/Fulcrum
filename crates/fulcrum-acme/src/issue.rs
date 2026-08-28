//! 签发与续期的执行路径。
//!
//! 一轮巡检对每个域名做同一件事：
//!
//! ```text
//! 拿跨进程锁 ──▶ 重读存储 ──▶ 问 ARI ──▶ 判定该不该续（G56）
//!                                            │否 ──▶ 装上现有的那张，收工
//!                                            │是
//!                                            ▼
//!                    下单 ──▶ 解挑战 ──▶ 等 ready ──▶ finalize ──▶ 取证书
//!                                            │
//!                                            ▼
//!                              写存储（原子，G55）──▶ 回读校验 ──▶ 热装到解析器
//! ```
//!
//! ★ **顺序必须是「先拿锁、再读」**：升级窗口里两代共享同一个证书目录，
//! 第二代在锁上等到的时候证书可能已经在那儿了。再签一张不只是白跑 ——
//! CA 的速率配额**按账户**算，耗尽之后签不出来的不只是这一个域名。
//!
//! ★ **写完回读一次再装**：直接装内存里那份，等于从没验证过落盘那份能不能读，
//! 而它读不出来的那天症状是「重启之后所有 HTTPS 全挂」。

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

/// 不走 DNS-01 时，这一次用哪种挑战。**G54：TLS-ALPN-01 主、HTTP-01 备。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonDnsChallenge {
    TlsAlpn01,
    Http01,
}

impl NonDnsChallenge {
    /// 写进 `meta.json` 的名字，与 RFC 8555 的挑战类型串一致。
    pub fn as_str(self) -> &'static str {
        match self {
            NonDnsChallenge::TlsAlpn01 => "tls-alpn-01",
            NonDnsChallenge::Http01 => "http-01",
        }
    }
}

/// 这一次该用主的还是备的。
///
/// ★ **这是 G54 那句「HTTP-01 是备」的全部落点**：只试 TLS-ALPN-01 的实现在 443 被挡住的
/// 机器上永远签不出来，而日志每轮只说「CA 说验不过」。
///
/// 规则一条：**上一次失败时用的那种，这一次换另一种。**
/// 成功即清空（否则一次偶发失败会把域名永久钉在备用挑战上）；
/// ⚠ 换挑战**不重置**失败计数，否则退避在「两种都不通」时永远长不大。
pub fn pick_non_dns_challenge(last_failed: Option<&str>) -> NonDnsChallenge {
    match last_failed {
        Some("tls-alpn-01") => NonDnsChallenge::Http01,
        // ★ 认不出来的值（旧版本写的、手工改坏的）一律走主的——
        //   与「从来没失败过」同一处置，而不是当成错误。
        _ => NonDnsChallenge::TlsAlpn01,
    }
}

impl AcmeManager {
    /// **G59 第 2 条**：启动时把每份 DNS 凭据校验一遍。
    ///
    /// 返回 `Err` = **拒绝启动**。形状照 G15：错误在启动时暴露，不等被滥用才发现。
    ///
    /// ★ **两种失败处置相反，分法挂在类型上**（[`crate::provider::VerifyError`]）：
    /// `Fatal`（对端说不行 / 凭据读不出来）⇒ 拒绝启动；`Inconclusive`（没连上）⇒ 打 error 继续。
    ///
    /// ⚠ 后者不拒绝启动，是因为一次网络抖动不该让整台机器上所有站点都起不来。
    /// ★ 这与「『没能检查』不许当成『检查通过』」有张力 ⇒ 处置**不是悄悄跳过**，
    /// 而是打 error 说出「这份凭据没被验过」。**一处知情的取舍。**
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
                    // ⚠ error 不是 warn：这份凭据**没有被验过**，而它有可能是坏的。
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

    /// 巡检一轮：该签的签、该续的续、已经好的装上。
    ///
    /// # ⚠ ⚠ 计数记在这一层，正体在 [`Self::poll_once`]
    ///
    /// 那一层有**三处早退**（没有目标 / 这一批一个都接不了 / 账户拿不到）。
    /// 逐处记一遍迟早漏掉一处，而漏掉的那一处**不会有任何症状**：
    /// `fulcrum_acme_issue_total` 照样在涨，只是少了一类。
    /// ★ 与 `access_log::Record::finish` 同一条形状 —— 把「一定要做的收尾」
    /// 放到一个**没有早退分支**的外层，让漏掉在结构上做不到。
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

        // ── 0. 先把这一批接不了的挑出来 ───────────────────────────────────
        //
        // ★ ★ **这一步必须在建账户之前。** 放在账户之后的话，一份只有通配符站点的
        //   配置会先去 CA 那边建一个**一次都用不上的账户**；而 CA 连不上的时候，
        //   那些本该被记成「推迟」的域名会被记成「失败」，进退避、进失败计数、进告警。
        //   ⚠ **「这一批还没做」与「这一次没做成」是两件事，混在一起会让监控报假警。**
        let mut actionable: Vec<&Target> = Vec::new();
        for target in &self.targets {
            if target.actionable() {
                actionable.push(target);
            } else {
                // ⏳ 走到这里只有一种情况：**通配符，但这个站点没配 `tls { dns … }`**。
                //   通配符只能走 DNS-01（G54），而 DNS-01 需要有人去改 TXT。
                //   ★ 说清楚「谁、为什么、怎么办」——一个只说「跳过」的日志，
                //     等于让人去猜是配置写错了还是功能没做。
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

        // ★ 账户是**懒建**的：一份只有 `tls <cert> <key>` 的配置不该在启动时
        //   去 CA 那边建一个用不上的账户。走到这里说明确实有域名要自动签发。
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
                // ⚠ 账户拿不到 = 这一轮一个都签不了。逐个域名记一遍失败，
                //   而不是打一行「ACME 挂了」——后者在有十个域名时看不出影响面。
                error!("ACME 账户不可用：{e}");
                // ⚠ 只把**接得了的**那些记成失败：推迟的那些与账户能不能用无关。
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

    /// 请求强制续期一个域名（**G74**）。管理面调它。
    ///
    /// 返回 `false` = **这个域名不在本进程的自动签发目标里**，什么都没做。
    /// ⚠ 判据必须是「它在不在 `targets` 里」，不能只看域名格式 —— 一个静静接受任意域名
    /// 然后什么都不发生的接口，比报错难查得多。
    ///
    /// # 两档，默认那一档**不动退避**
    ///
    /// | `clear_backoff` | 越过「还不到时候」 | 越过退避 |
    /// |---|---|---|
    /// | `false`（默认）| ✅ | ❌ |
    /// | `true` | ✅ | ✅ **并把失败计数清零** |
    ///
    /// ★ 默认不越过：一个随手绕开退避的口子等于给「反复重签把 CA 配额烧光」开一扇门，
    /// 而配额**按账户**算。⇒ 第二档要显式 `"force": true`，且**每次清掉非零计数都打 warn**。
    /// ⚠ 没有第二档时，凭据补好后想立刻重试就只能手删 `meta.json` 里的失败计数 ——
    /// 那是把标准动作定义成了危险动作（删错一个文件就是删掉证书）。
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
            // ★ 同一轮里两次请求取**较强**的那一档：先 renew 再 force 不该被降级。
            let e = f.entry(d.clone()).or_insert(false);
            *e = *e || clear_backoff;
        }
        // ★ **先入队再叫醒**。反过来的话，被叫醒的那一轮可能刚好读到空队列，
        //   然后一觉睡到 12 小时之后——而调用方那边看起来「命令成功了」。
        self.wake.notify_one();
        if clear_backoff {
            info!("{d} 被要求强制续期（G74，**连退避一起清**），已叫醒巡检");
        } else {
            info!("{d} 被要求强制续期（G74），已叫醒巡检");
        }
        true
    }

    /// 这个域名这一轮是不是被强制续期了。**取走即清**（一次性）。
    ///
    /// 返回 `None` = 没被强制；`Some(clear_backoff)` = 被强制，值说明是哪一档。
    ///
    /// ⚠ ⚠ **调用点必须在「有没有证书」这个分支之外**。之前它只长在
    /// 「已经有一张证书」那一支里，于是**对一个从来没签成过的域名按强制续期，
    /// 那个标记永远不会被取走** —— 它会一直躺在队列里，直到某一轮证书终于签出来了
    /// 才被消费掉，而那时没有任何人还记得自己按过。
    fn take_forced(&self, domain: &str) -> Option<bool> {
        match self.force.lock() {
            Ok(mut f) => f.remove(domain),
            // 锁中毒不该让签发停摆；当成「没有强制」继续走正常判定。
            Err(_) => None,
        }
    }

    /// 一直巡检到收到停机信号。
    pub async fn run_loop(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                info!("ACME 巡检收到停机信号，退出");
                return;
            }
            let report = self.run_once().await;
            let nap = report.sleep_for();
            // ★ `select!` 而不是先 sleep 再看信号：一个 12 小时的 sleep 会把
            //   优雅停机拖成 12 小时——而排空窗口只有几十秒。
            // ★ 第三条臂是 G74 的强制续期：没有它，一次强制最长要等 12 小时才生效。
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

    /// 一个域名的完整处置。
    async fn ensure_one(
        &self,
        account: &Account,
        target: &Target,
        report: &mut Report,
    ) -> Result<(), String> {
        let domain = target.domain.as_str();
        // ★ 抖动**一轮只取一次**：下面退避会被问到两次（`should_renew` 里一次、
        //   步骤 3.5 一次），两次取到不同的抖动值会让它们对同一件事给出不同答案，
        //   于是刚被 `should_renew` 放行的域名可能立刻被 3.5 拦下。
        let jitter = self.jitter.unit();

        // ── 1. 跨进程锁（G55 第 2 条）。**先拿锁，再读存储。**───────────────
        let _lock = self.lock_domain(domain).await?;

        // ── 2. 重读存储 ───────────────────────────────────────────────────
        let existing = self
            .store
            .load(&self.issuer, domain)
            .map_err(|e| format!("读证书存储失败：{e}"))?;

        // ★ ★ 续期状态**单独读**，不从证书那份里取。
        //   `load()` 缺 `cert.pem` 就返回 `None`，于是一个从来没签成过的域名
        //   连它自己的失败计数也读不回来——每一轮都从「零次失败」重新开始。
        let mut meta = self.store.load_meta(&self.issuer, domain);
        meta.issuer = self.issuer.clone();

        // ── 2.5 强制续期标记：**在分支之外取一次**（G74）───────────────────
        //
        // ★ 位置本身就是判据：放在「已经有一张证书」那一支里，会让一个从来没签成过的
        //   域名的强制请求永远取不走（见 `take_forced` 的注释）。
        let forced = self.take_forced(domain);

        // ── 3. 已经有一张：先判它是不是我们这个 CA 签的，再判要不要续 ────────
        let mut replaces: Option<CertificateIdentifier<'static>> = None;
        if let Some(sc) = existing {
            let foreign = match &sc.meta.issuer_url {
                // ★ `None` = 不是本程序签的（手工放进来的，或旧版本写的）。
                //   **不当成外来证书**：那会把用户手工放进去的证书每次都重签一遍。
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
                // ARI 优先（G56）。★ 问不到不是错误：CA 可能根本不支持。
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
                // ★ ★ **G74：强制续期在这里越过续期判定**（不是越过退避，见下）。
                //   ⚠ 只越过「还不到时候」，**不越过 3.5 那道退避**：
                //   一个能绕过退避的强制口子，等于给「反复按重签按钮把 CA 配额烧光」
                //   开了一扇门——而配额是**按账户**算的，烧光之后连累同机所有域名。
                //   ★ 所以现场表现是：强制之后若上一次刚失败过，它会说「退避中」并给出剩余秒数，
                //   而不是静静地什么都不做。
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
                    // meta 里可能刚记下新的 ARI 窗口，落一次盘（不动证书）。
                    let _ = self.store.save_meta(&self.issuer, domain, &meta);
                    return Ok(());
                }
                info!("{domain} 该续期了：{}", decision.reason);
            }
        }

        // ── 3.5 退避（否决项）★ 对「从来没签成过」的域名同样有效 ─────────────
        //
        // ★ 走到这里有两种域名：要续期的，与**从来没签成过的**。⚠ 第二种恰恰是退避
        //   最该管的那一种（域名写错、80 不通、CAA 挡着 ⇒ 永远签不出来），
        //   而 CA 的失败验证配额**按账户**算 —— 一个签不出来的域名能把整个账户耗光。
        // ⚠ 第二档清零时**打 warn 而不是 info**：它把 CA 的失败配额重新暴露出来，
        //   现场必须看得见是谁、清掉了多少。
        if forced == Some(true) && meta.renewal.failures > 0 {
            warn!(
                "{domain} 按请求清掉退避：失败计数 {} → 0（G74 的第二档）。\
                 ★ 这不是退避坏了，是有人显式要求的；\
                 ⚠ 根因没修好的话，下一次失败会从头开始重新长。",
                meta.renewal.failures
            );
            meta.renewal.failures = 0;
            meta.renewal.last_attempt = None;
            // 立刻落盘：中途崩了也不该让计数又冒出来。
            let _ = self.store.save_meta(&self.issuer, domain, &meta);
        }
        if let Some(wait) =
            backoff_remaining(SystemTime::now(), &meta.renewal, self.backoff, jitter)
        {
            // ⚠ 这是 info 不是 warn：**退避正在工作**是正常状态，不是异常 ——
            //   真正该报警的是那次失败本身，而它已经报过了。
            info!(
                "{domain} 上次签发失败 {} 次，退避中，{}s 后再试（本轮不去试）",
                meta.renewal.failures,
                wait.as_secs()
            );
            report.backed_off.push(domain.to_string());
            report.note_next_check(wait);
            return Ok(());
        }

        // ── 4. 签 ─────────────────────────────────────────────────────────
        //
        // ⚠ 无论成败都要落一次 meta：失败计数与上次尝试时刻是退避的**唯一输入**，
        //   不落盘就等于每次重启都从「零次失败」开始，退避形同虚设。
        meta.renewal.last_attempt = Some(unix_now());
        // ★ G54 的主/备在这里选定。DNS-01 的站点不看它（通配符只有那一条路）。
        let challenge = pick_non_dns_challenge(meta.last_challenge_failed.as_deref());
        if target.dns01.is_none() {
            debug!("{domain} 本次用 {} 挑战", challenge.as_str());
        }
        match self.issue(account, target, replaces, challenge).await {
            Ok((cert_pem, key_pem)) => {
                meta.renewal.failures = 0;
                // ★ 成功即清空：一次偶发失败不该把这个域名永久钉在备用挑战上。
                meta.last_challenge_failed = None;
                meta.issuer_url = Some(self.cfg.directory_url.clone());
                // ★ ARI 窗口是**跟着某一张证书**的：新签一张之后旧窗口立刻失效，
                //   留着它会让新证书一上来就被判成「该续了」。
                meta.ari_start = None;
                meta.ari_end = None;
                self.store
                    .save(&self.issuer, domain, &cert_pem, &key_pem, &meta)
                    .map_err(|e| format!("证书写不进存储：{e}"))?;
                // 回读一次再装 —— 装上去的必须是盘上那份。
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
                // ★ 记下这次用的是哪种挑战 —— 下一轮换另一种（G54 的「备」）。
                //   ⚠ 只对不走 DNS-01 的站点有意义；通配符换不了别的挑战，
                //   记了反而会让它下一轮去试一个它根本用不了的。
                if target.dns01.is_none() {
                    meta.last_challenge_failed = Some(challenge.as_str().to_string());
                }
                let wait = self
                    .backoff
                    .delay(meta.renewal.failures, self.jitter.unit());
                if let Err(e2) = self.store.save_meta(&self.issuer, domain, &meta) {
                    // meta 写不进去时退避就失忆了 —— 说出来，别咽掉。
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

    /// 走一遍 RFC 8555 的下单流程，返回 `(证书链 PEM, 私钥 PEM)`。
    ///
    /// ★ ★ **DNS-01 留下的 TXT 必须被摘掉，而摘不掉不能让签发失败**——证书已经到手了。
    /// 所以这里不用 `Drop` 守卫（析构里不能 await），而是把清理写成显式的一段，
    /// **成功与失败两条路都会走到**。
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
        // ⚠ 清理排在**拿到结果之后**：提前清会让 CA 在 finalize 之后回来复验时扑空，
        //   而那时的错误信息只说「验不过」。
        if let Some(dns01) = &target.dns01 {
            for (name, value) in &planted {
                if let Err(e) = dns01.provider.clear_txt(name, value).await {
                    // ★ 只 warn：留一条 TXT 是卫生问题，不是可用性问题。
                    //   升级成错误会让一次**成功的签发**被记成失败 ⇒ 进退避、进告警。
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
        let mut new_order = NewOrder::new(&identifiers);
        if let Some(id) = replaces {
            // ★ 只有确认 CA 支持 ARI 时才带上它：`new_order` 在 CA 不支持时会**直接报错**，
            //   于是「顺手带一个能拿到优惠速率的字段」会变成「换个 CA 就一张都签不出来」。
            new_order = new_order.replaces(id);
        }
        let mut order = account
            .new_order(&new_order)
            .await
            .map_err(|e| format!("下单失败：{e}"))?;

        // ── 解挑战 ────────────────────────────────────────────────────────
        //
        // ★ 守卫收在一个 Vec 里，活到订单 ready 为止。token 摘早了，
        //   CA 来验的时候就 404 —— 而那时的错误信息只说「验不过」。
        let mut provisioned = Vec::new();
        // ★ TLS-ALPN-01 的守卫是另一个类型（挂的是证书不是 token），单独收一个 Vec。
        //   两个 Vec 都活到订单 ready 为止。
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
                    // ── DNS-01（G54 里通配符唯一可行的那条路 / G57 / G58）──────
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
                    // ★ **先记进清理清单，再去等可见**：中间任何一步失败，
                    //   那条已经写上去的 TXT 都必须被摘掉。反过来写会漏掉失败路径。
                    planted.push((record.clone(), value.clone()));

                    // ★ ★ ★ G58 那条硬约束就落在这一行：**真去问权威 NS，不 sleep**。
                    //   固定 sleep 在快的时候浪费时间、在慢的时候直接签失败，
                    //   而失败要消耗 CA 的速率配额。
                    dns01.checker.wait_visible(&record, &value).await?;

                    debug!("{domain}：{record} 的 TXT 已在权威 NS 上可见，通知 CA 来验");
                    challenge
                        .set_ready()
                        .await
                        .map_err(|e| format!("通知 CA 验 DNS-01 失败：{e}"))?;
                } else {
                    // ── TLS-ALPN-01（G54 的「主」，RFC 8737）───────────────
                    //
                    // ★ 整件事在 TLS 握手里完成：CA 连过来、ALPN 只提 `acme-tls/1`、
                    //   看一眼我们回的那张自签证书里的 acmeIdentifier 扩展就断开。
                    //   **零路由占用** —— 用户的配置挡不住自己的证书签发。
                    //
                    // ⚠ **`authz.challenge()` 一个授权只能调一次**（它借的是 handle
                    //   自己的 `'a`）⇒「先试这个不行再试那个」直接编不过（E0499）。
                    //   → 先用不可变视图看清 CA 给了哪几种，决定之后**只取一次 handle**。
                    let has_tls_alpn = authz
                        .challenges
                        .iter()
                        .any(|c| c.r#type == ChallengeType::TlsAlpn01);
                    let use_tls_alpn = challenge_kind == NonDnsChallenge::TlsAlpn01 && has_tls_alpn;
                    if challenge_kind == NonDnsChallenge::TlsAlpn01 && !has_tls_alpn {
                        // ★ CA 不提供这种挑战不是错误，落回备用的那条。说出来即可。
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
                        // ⚠ 验证连接用的 SNI 是**被授权的那个名字**，不是站点名。
                        //   通配符站点走的是 DNS-01，不会落到这一支。
                        let digest = key_auth.digest();
                        challenge_certs.push(crate::tlsalpn01::provision(
                            &self.resolver,
                            domain,
                            digest.as_ref(),
                        )?);
                        // ★ ★ **info 而不是 debug，理由与 G58 那条轮询日志一模一样**：
                        //   「这一次用的是哪种挑战」是**唯一**可从外部观察到的痕迹。
                        //   压成 debug 就等于把判据藏起来——而运维排查
                        //   「为什么这台机器签不出证书」时，第一个要知道的就是它走了哪条路。
                        info!("{domain}：本次挑战走 TLS-ALPN-01（G54 的主），挑战证书已挂上");
                    } else {
                        // ── HTTP-01（G54 的「备」）──────────────────────────
                        let token = challenge.token.clone();
                        provisioned.push(self.http01.provision(&token, key_auth.as_str()));
                        // ⚠ token **不进 info 日志**：它连着 key authorization，
                        //   而日志会被贴进 issue、进集中式日志系统。要看它请开 debug。
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
            // ★ ★ ★ **CA 已经把「为什么」写在授权里了，不去取它，现场就只剩「Invalid」三个字。**
            //
            //   在 `example.com` 上真的被这句话挡了一次：DNS-01 的 TXT
            //   明明已在三台权威 NS 上可见，订单却回 Invalid，而日志里**没有一个字**
            //   说得出 CA 到底在抱怨什么 —— 那一刻手上唯一的线索是「换个地方猜」。
            //   ⚠ RFC 8555 §7.1.4/§6.7 的 problem document 就挂在挑战上（`challenge.error`），
            //   取一次就有；不取，就等于**把对端说过的话丢掉，然后自己去猜**。
            //
            //   ★ 只在**失败路径**上多问一次授权：成功路径一个请求都不多发。
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
                    // ⚠ 取不到就说取不到，别让它变成「没有原因」。
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
        // ★ 守卫在这里才析构：CA 有可能在 finalize 之后还回来验一次。
        drop(provisioned);
        drop(challenge_certs);
        Ok((cert_pem, key_pem))
    }

    /// 问一次 ARI（RFC 9773）。问不到就返回 `None`，**不算失败**。
    async fn fetch_ari(
        &self,
        account: &Account,
        sc: &fulcrum_tls::StoredCert,
        meta: &mut Meta,
        replaces: &mut Option<CertificateIdentifier<'static>>,
    ) -> Option<AriWindow> {
        let leaf = sc.chain.first()?;
        // ⚠ 这里多一次 DER 往返：`instant-acme` 的 `CertificateIdentifier` 只有
        //   `TryFrom<&CertificateDer>` 这一个入口，而我们手里是 BoringSSL 的 `X509`。
        // ★ **不自己抠 AKI 与序列号** —— ARI 标识抠错的后果是「续期请求被 CA 静静忽略」。
        // ⚠ `rustls-pki-types` 因此是本 crate 的直接依赖（**新增 0 个包**）：它是纯类型
        //   定义 crate，没有 crypto / provider / default feature，与 `rustls` 不是一类东西。
        let der = match leaf.to_der() {
            Ok(d) => rustls_pki_types::CertificateDer::from(d),
            Err(e) => {
                debug!("证书转不回 DER：{e}");
                return None;
            }
        };
        let id = match CertificateIdentifier::try_from(&der) {
            Ok(id) => id.into_owned(),
            Err(e) => {
                debug!("从证书里抠不出 ARI 标识：{e}");
                return None;
            }
        };
        match account.renewal_info(&id).await {
            Ok((info, _retry_after)) => {
                let start = info.suggested_window.start.unix_timestamp().max(0) as u64;
                let end = info.suggested_window.end.unix_timestamp().max(0) as u64;
                meta.ari_start = Some(start);
                meta.ari_end = Some(end);
                // ★ ARI 能问到 ⇒ 这个 CA 支持 ARI ⇒ 续期时可以带 `replaces`。
                //   这条因果是**实测出来的**，不是推的：`new_order` 在 CA 不支持时直接报错。
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
                // ⚠ 这里**必须**是 debug 而不是 warn：绝大多数 CA 不支持 ARI，
                //   把它打成警告会让日志里每天都有一条假警报——而假警报会把真的埋掉。
                debug!("问不到 ARI（多半是这个 CA 不支持）：{e}");
                meta.ari_start = None;
                meta.ari_end = None;
                None
            }
        }
    }

    /// 拿一个域名的跨进程签发锁。
    ///
    /// ★ `flock` 是**阻塞**的，扔到 blocking 池里。直接在 runtime 线程上等，
    /// 等的时候这条线程上的**所有**请求都停着——而它等的正是另一代进程签完一张证书。
    async fn lock_domain(&self, domain: &str) -> Result<DomainLock, String> {
        let root: PathBuf = self.store.root().to_path_buf();
        let issuer = self.issuer.clone();
        let domain = domain.to_string();
        tokio::task::spawn_blocking(move || CertStore::new(root).lock(&issuer, &domain))
            .await
            .map_err(|e| format!("拿签发锁的任务没跑完：{e}"))?
            .map_err(|e| format!("拿不到签发锁：{e}"))
    }

    /// 把一张证书装到 SNI 解析器上（热装，立刻对新握手生效）。
    ///
    /// ★ 按所有权接，不按引用接：`CertifiedKey` 本来就要持有自己的那份链与密钥，
    /// 接引用只会逼着这里克隆一遍，然后把「谁拥有这份私钥」变模糊。
    fn install(&self, sc: fulcrum_tls::StoredCert) -> Result<(), String> {
        let loaded = to_loaded(sc)?;
        if loaded.domains.is_empty() {
            // ★ 一张没有 DNS SAN 的证书装上去也永远挑不中——**静静地装完**是最难查的形态：
            //   日志说「装载成功」，而每一次握手都被拒绝。
            warn!("要装的证书里没有任何 DNS SAN —— 它不会被任何 SNI 选中");
            return Ok(());
        }
        self.resolver.install(&loaded.domains, loaded.key);
        Ok(())
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
        // ★ 判据挂在「不会去建账户」上：一份只有手工证书的配置，
        //   不该因为起了个后台任务就去 CA 那边留一条记录。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = rt.block_on(m.run_once());
        assert!(report.is_empty());
        assert_eq!(report.next_check, None);
        // ★ **批 M** 的反向那半：一轮「什么都没有」不许在签发计数上留下任何一笔。
        assert_eq!(m.issue_counts().snapshot(), (0, 0, 0));
    }

    #[test]
    fn 通配符被推迟而不是被当成失败() {
        // ⚠ 两者的区别不是措辞：失败会进退避、会计数、会在监控里报警，
        //   而「这一批还没做」不该产生任何一条告警。
        let cfg = AcmeConfig::new(Some("https://localhost:1/dir"), None, "/nonexistent");
        let m = AcmeManager::new(
            cfg,
            Arc::new(SniResolver::new()),
            Arc::new(Http01Store::new()),
            vec![Target {
                domain: "*.example.com".into(),
                site: "s".into(),
                // ★ 没配 DNS-01 —— 这正是「推迟」的那一种。
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
        // ★ ★ **批 M**：这一轮走的是「一个都接不了」那处**早退**。
        //   计数记在 `run_once` 外层，所以早退照样记得到 —— 记在 `poll_once` 里面的话，
        //   这一格会是 `(0,0,0)`，而 `fulcrum_acme_issue_total` 不会有任何异样。
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

        // ① 不在目标里 ⇒ 什么都不做，而且**不入队**（静静接受是最难查的那种）。
        assert!(!m.request_renew("nope.example.com", true));
        assert_eq!(m.take_forced("nope.example.com"), None);

        // ② 默认档
        assert!(m.request_renew("a.example.com", false));
        assert_eq!(m.take_forced("a.example.com"), Some(false));
        // ★ 取走即清：第二次就没有了。
        assert_eq!(m.take_forced("a.example.com"), None);

        // ③ 第二档
        assert!(m.request_renew("a.example.com", true));
        assert_eq!(m.take_forced("a.example.com"), Some(true));

        // ④ ★ 同一轮里两次请求取**较强**的那一档，两个方向都验：
        //   否则「先按了 force、又按了一次普通」会把 force 悄悄降级。
        assert!(m.request_renew("a.example.com", false));
        assert!(m.request_renew("a.example.com", true));
        assert_eq!(m.take_forced("a.example.com"), Some(true));
        assert!(m.request_renew("a.example.com", true));
        assert!(m.request_renew("a.example.com", false));
        assert_eq!(m.take_forced("a.example.com"), Some(true));

        // ⑤ 大小写与空白按目标那边的口径归一
        assert!(m.request_renew("  A.Example.COM  ", true));
        assert_eq!(m.take_forced("a.example.com"), Some(true));
    }

    #[test]
    fn 账户拿不到时强制标记不被消费() {
        // ★ 这一条钉的是**取标记的时机**：账户都拿不到的那一轮压根没试过，
        //   所以标记要留着等下一轮。⚠ 取走等于把一次「什么都没发生」记成
        //   「你的请求已经用掉了」，而调用方那边看起来完全成功。
        // （「取标记的位置」另有判据：`take_forced` 必须在「已经有证书」那个分支之外取，
        //   否则从没签成过的域名按下强制之后标记永远取不走。）
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
        // ★ ★ **批 M**：这一轮走的是「账户拿不到」那处**早退**，一个域名记一次失败。
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

    // ── G54 的主/备 ─────────────────────────────────────────────────────────

    #[test]
    fn 默认走主的_tls_alpn_01() {
        // ★ G54 明写 TLS-ALPN-01 是「主」。它零路由占用，而且只要 443 ——
        //   对 80 端口常年受限的 `.cn` 机房尤其要紧。
        assert_eq!(pick_non_dns_challenge(None), NonDnsChallenge::TlsAlpn01);
    }

    #[test]
    fn 主的失败过一次就换备的() {
        // ★ ★ 这一条就是「HTTP-01 是**备**」的全部含义。没有它，「主/备」只是措辞：
        //   一个永远只试 TLS-ALPN-01 的实现，在 443 被挡住的机器上**永远签不出来**，
        //   而日志里每一轮都只说「CA 说验不过」。
        assert_eq!(
            pick_non_dns_challenge(Some("tls-alpn-01")),
            NonDnsChallenge::Http01
        );
    }

    #[test]
    fn 备的也失败过就换回主的() {
        // ⚠ 判据是**两个方向都要动**：一个「失败过就永远用 HTTP-01」的实现
        //   在上一条里表现完全相同，而它会让一台只有 443 的机器永远签不出来。
        assert_eq!(
            pick_non_dns_challenge(Some("http-01")),
            NonDnsChallenge::TlsAlpn01
        );
    }

    #[test]
    fn 认不出来的值当成没失败过() {
        // 旧版本写的、手工改坏的 meta.json 不该让签发停摆。
        assert_eq!(
            pick_non_dns_challenge(Some("dns-01")),
            NonDnsChallenge::TlsAlpn01
        );
        assert_eq!(pick_non_dns_challenge(Some("")), NonDnsChallenge::TlsAlpn01);
    }

    #[test]
    fn 写进_meta_的名字就是_rfc_8555_的挑战类型串() {
        // ★ 这两个串会落进 `meta.json`，而下一轮靠它们分岔。
        //   写错一个字母的后果不是报错，是**主/备永远不切换**——没有任何症状。
        assert_eq!(NonDnsChallenge::TlsAlpn01.as_str(), "tls-alpn-01");
        assert_eq!(NonDnsChallenge::Http01.as_str(), "http-01");
        // 自证：这两个串真的能被 `pick` 认回来（否则上面两条断言等于没测）。
        assert_eq!(
            pick_non_dns_challenge(Some(NonDnsChallenge::TlsAlpn01.as_str())),
            NonDnsChallenge::Http01
        );
        assert_eq!(
            pick_non_dns_challenge(Some(NonDnsChallenge::Http01.as_str())),
            NonDnsChallenge::TlsAlpn01
        );
    }
}
