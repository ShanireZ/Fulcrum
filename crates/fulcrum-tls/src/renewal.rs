//! 续期判定（G56）：**ARI 优先，回退「剩余寿命 1/3」；失败走指数退避 + 随机抖动**。
//!
//! ★ ★ **整个模块是纯函数**：时间与抖动都从参数进来，一个 `SystemTime::now()` 都没有。
//! 理由是「判据不能挂在会抖的量上」——续期策略最要紧的性质（6 天短证书不会被判成
//! 「永远在续期」、退避真的在退、抖动真的在抖）只有在**可以指定时间**的前提下才测得出来。
//!
//! # ★ 为什么不写死天数
//!
//! **Let's Encrypt 正在推 6 天短寿命证书**。「到期前 30 天续」在那种证书上
//! 等于**永远在续期**——每次检查都满足条件，于是每次检查都发一次签发请求，
//! 而失败要消耗 CA 的速率配额。比例制对寿命免疫。
//!
//! # ★ ARI 还有比例制给不了的东西
//!
//! CA 因安全事件需要**提前批量撤销重签**时，ARI（RFC 9773）是它通知你的唯一渠道。
//! 不接就只能等出事——所以顺序是「先问 ARI，问不到才用比例」，不是「两者取其一」。

use std::time::{Duration, SystemTime};

/// CA 通过 ARI 给出的建议续期窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AriWindow {
    pub start: SystemTime,
    pub end: SystemTime,
}

/// 一张证书的续期状态。存在 `meta.json` 里——**它记的是证书里没有的东西**。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenewalState {
    /// 连续失败次数。成功一次就归零。
    pub failures: u32,
    /// 上一次尝试的时刻（Unix 秒）。`None` = 还没试过。
    pub last_attempt: Option<u64>,
}

/// 退避参数。★ 做成结构体而不是常量，是为了让测试能用小数值跑完整条曲线。
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub base: Duration,
    pub cap: Duration,
    /// 抖动幅度占该次退避的比例（0.0 = 不抖）。
    pub jitter_ratio: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            base: Duration::from_secs(60),
            cap: Duration::from_secs(6 * 3600),
            // ⚠ **抖动不是可选项**：同一台机上几十个域名同时到期时，
            //   不抖动会把重试打成尖峰——而尖峰正好是 CA 速率限制针对的形状。
            jitter_ratio: 0.5,
        }
    }
}

impl Backoff {
    /// 第 `failures` 次失败之后要等多久。`jitter_unit` 取 `[0, 1)`，由调用方给。
    ///
    /// ★ 抖动值从参数进来而不是在这里 `rand`：否则这个函数没法被确定性地测，
    /// 而「时快时慢的反证等于没有反证」是本仓库的老纪律。
    pub fn delay(&self, failures: u32, jitter_unit: f64) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }
        // 2^(n-1) * base，封顶。★ 用 checked/saturating：failures 大到 40 时
        //   `1u64 << 39` 秒是一万多年，乘出来会溢出——而溢出 panic 在续期路径上
        //   等于把一次签发失败升级成进程崩溃。
        let shift = (failures - 1).min(32);
        let mult = 1u64 << shift;
        let raw = self
            .base
            .checked_mul(mult.min(u32::MAX as u64) as u32)
            .unwrap_or(self.cap)
            .min(self.cap);
        let j = jitter_unit.clamp(0.0, 1.0) * self.jitter_ratio;
        // 抖动是**减法**：在 [raw*(1-j), raw] 之间。往小抖不会把重试推到更远的将来，
        // 而往大抖会让「封顶」这件事失效。
        let secs = raw.as_secs_f64() * (1.0 - j);
        Duration::from_secs_f64(secs.max(0.0))
    }
}

/// 判定结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// 现在就该续。
    pub renew_now: bool,
    /// 人读的理由——★ 它会进日志，所以必须说清楚「为什么是现在」。
    pub reason: String,
    /// 下一次该在多久之后再检查。
    pub next_check: Duration,
}

/// 最长与最短的检查间隔。
const MAX_CHECK: Duration = Duration::from_secs(12 * 3600);
const MIN_CHECK: Duration = Duration::from_secs(60);

/// 退避还要压多久。`None` = 现在可以试。
///
/// ★ ★ **它被抽出来，是因为它有两个调用点** —— 只长在其中一个上就是一半的实现。
/// [`should_renew`] 只在「存储里已经有一张证书」时才被调用——于是一个**从来没签成过**
/// 的域名（域名写错、80 端口不通、CAA 挡着）根本走不到这里：
/// 它每一轮都直接去下单，退避形同虚设。
/// ⚠ 后果不是「多试几次」：CA 的失败验证配额是**按账户**算的，
/// 一个签不出来的域名会把整个账户的配额耗光，**连累同一台机器上别的域名也签不出来**。
/// 那正是 G56 里退避存在的理由，却恰好在它最该生效的那一种情形上缺席。
/// 由 ACME 门禁的反证跑抓到：日志里连着三轮都是「连续第 1 次失败」。
pub fn backoff_remaining(
    now: SystemTime,
    state: &RenewalState,
    backoff: Backoff,
    jitter_unit: f64,
) -> Option<Duration> {
    if state.failures == 0 {
        return None;
    }
    let last = SystemTime::UNIX_EPOCH + Duration::from_secs(state.last_attempt?);
    let ready_at = last + backoff.delay(state.failures, jitter_unit);
    if now < ready_at {
        Some(ready_at.duration_since(now).unwrap_or(Duration::ZERO))
    } else {
        None
    }
}

/// 该不该现在续期。
///
/// 顺序：**退避 → ARI → 比例**。退避排最前是因为它是**否决项**：
/// 上一次失败之后还没到重试时刻，就算 ARI 说该续也不能续（那只会再撞一次速率限制）。
pub fn should_renew(
    now: SystemTime,
    not_before: SystemTime,
    not_after: SystemTime,
    ari: Option<AriWindow>,
    state: &RenewalState,
    backoff: Backoff,
    jitter_unit: f64,
) -> Decision {
    // ── 1. 退避（否决项）───────────────────────────────────────────────────
    if let Some(remaining) = backoff_remaining(now, state, backoff, jitter_unit) {
        return Decision {
            renew_now: false,
            reason: format!(
                "上次续期失败 {} 次，退避到 {}s 之后再试",
                state.failures,
                remaining.as_secs()
            ),
            next_check: clamp_check(remaining),
        };
    }

    // ── 2. ARI（CA 说话优先）───────────────────────────────────────────────
    if let Some(w) = ari {
        if now >= w.start {
            return Decision {
                renew_now: true,
                reason: "CA 的 ARI 建议窗口已经开始".to_string(),
                next_check: MIN_CHECK,
            };
        }
        let until = w.start.duration_since(now).unwrap_or(Duration::ZERO);
        return Decision {
            renew_now: false,
            reason: format!("ARI 建议窗口还有 {}s 才开始", until.as_secs()),
            next_check: clamp_check(until),
        };
    }

    // ── 3. 比例制：剩余寿命 <= 总寿命的 1/3 ────────────────────────────────
    let total = not_after
        .duration_since(not_before)
        .unwrap_or(Duration::ZERO);
    let remaining = not_after.duration_since(now).unwrap_or(Duration::ZERO);
    let threshold = total / 3;
    if remaining <= threshold {
        return Decision {
            renew_now: true,
            reason: format!(
                "剩余寿命 {}s 已不足总寿命 {}s 的 1/3（CA 没给 ARI）",
                remaining.as_secs(),
                total.as_secs()
            ),
            next_check: MIN_CHECK,
        };
    }
    let until = remaining - threshold;
    Decision {
        renew_now: false,
        reason: format!(
            "剩余寿命还有 {}s，离 1/3 线还有 {}s",
            remaining.as_secs(),
            until.as_secs()
        ),
        next_check: clamp_check(until),
    }
}

fn clamp_check(d: Duration) -> Duration {
    d.clamp(MIN_CHECK, MAX_CHECK)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn fresh() -> RenewalState {
        RenewalState::default()
    }

    #[test]
    fn 九十天证书在剩余三十天时才续() {
        let nb = t(0);
        let na = t(90 * DAY);
        // 剩 31 天：还不到 1/3（30 天）
        let d = should_renew(t(59 * DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(!d.renew_now, "{}", d.reason);
        // 剩 30 天：正好到线
        let d = should_renew(t(60 * DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(d.renew_now, "{}", d.reason);
    }

    #[test]
    fn 六天短证书不会被判成永远在续期() {
        // ★ ★ 这条是 G56 不写死天数的**全部理由**：
        //   「到期前 30 天续」在 6 天寿命的证书上，从签发那一刻就成立。
        let nb = t(0);
        let na = t(6 * DAY);
        // 第 1 天：剩 5 天 > 2 天（1/3），不该续
        let d = should_renew(t(DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(!d.renew_now, "刚签发就要续 = 永远在续期：{}", d.reason);
        // 第 4 天：剩 2 天 = 1/3，该续
        let d = should_renew(t(4 * DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(d.renew_now, "{}", d.reason);
    }

    #[test]
    fn ari_优先于比例制_两个方向都要对() {
        let nb = t(0);
        let na = t(90 * DAY);
        // ① ARI 说「现在就续」，而比例制还没到 → 听 ARI
        let ari = AriWindow {
            start: t(10 * DAY),
            end: t(20 * DAY),
        };
        let d = should_renew(
            t(11 * DAY),
            nb,
            na,
            Some(ari),
            &fresh(),
            Backoff::default(),
            0.0,
        );
        assert!(d.renew_now, "ARI 窗口内必须续：{}", d.reason);
        assert!(d.reason.contains("ARI"));

        // ② ARI 说「还没到」，而比例制已经到了 → **也听 ARI**。
        //    ★ 这一条容易写反：ARI 是 CA 的排期，它比我们的比例制更知道该什么时候续。
        let ari_late = AriWindow {
            start: t(80 * DAY),
            end: t(85 * DAY),
        };
        let d = should_renew(
            t(70 * DAY),
            nb,
            na,
            Some(ari_late),
            &fresh(),
            Backoff::default(),
            0.0,
        );
        assert!(!d.renew_now, "ARI 说还没到就不该续：{}", d.reason);
    }

    #[test]
    fn 退避是否决项连_ari_都压得住() {
        let nb = t(0);
        let na = t(90 * DAY);
        let ari = AriWindow {
            start: t(1),
            end: t(90 * DAY),
        };
        let state = RenewalState {
            failures: 3,
            last_attempt: Some(1000),
        };
        let bo = Backoff {
            base: Duration::from_secs(60),
            cap: Duration::from_secs(3600),
            jitter_ratio: 0.0,
        };
        // 第 3 次失败 → 等 4*60 = 240s；now = 1000 + 100 还没到
        let d = should_renew(t(1100), nb, na, Some(ari), &state, bo, 0.0);
        assert!(!d.renew_now, "退避没到就不许续：{}", d.reason);
        assert!(d.reason.contains("退避"));
        // 到了之后就放行
        let d = should_renew(t(1000 + 240), nb, na, Some(ari), &state, bo, 0.0);
        assert!(d.renew_now, "{}", d.reason);
    }

    #[test]
    fn 退避是指数的并且封顶() {
        let bo = Backoff {
            base: Duration::from_secs(10),
            cap: Duration::from_secs(100),
            jitter_ratio: 0.0,
        };
        assert_eq!(bo.delay(0, 0.0), Duration::ZERO);
        assert_eq!(bo.delay(1, 0.0), Duration::from_secs(10));
        assert_eq!(bo.delay(2, 0.0), Duration::from_secs(20));
        assert_eq!(bo.delay(3, 0.0), Duration::from_secs(40));
        assert_eq!(bo.delay(4, 0.0), Duration::from_secs(80));
        // 封顶
        assert_eq!(bo.delay(5, 0.0), Duration::from_secs(100));
        assert_eq!(bo.delay(50, 0.0), Duration::from_secs(100));
        // ★ 大失败次数不许溢出 panic：一次签发失败不该升级成进程崩溃
        assert_eq!(bo.delay(u32::MAX, 0.0), Duration::from_secs(100));
    }

    #[test]
    fn 抖动只往小抖而且真的在抖() {
        let bo = Backoff {
            base: Duration::from_secs(100),
            cap: Duration::from_secs(1000),
            jitter_ratio: 0.5,
        };
        let none = bo.delay(1, 0.0);
        let half = bo.delay(1, 0.5);
        let full = bo.delay(1, 1.0);
        assert_eq!(none, Duration::from_secs(100));
        assert_eq!(half, Duration::from_secs(75)); // 100 * (1 - 0.25)
        assert_eq!(full, Duration::from_secs(50)); // 100 * (1 - 0.5)
        // ★ 只往小抖：抖动不能把重试推到比封顶更远的将来
        assert!(full <= none && half <= none);
        // ★ 而它必须真的在抖——ratio 为 0 时才允许恒定
        let flat = Backoff {
            jitter_ratio: 0.0,
            ..bo
        };
        assert_eq!(flat.delay(1, 0.0), flat.delay(1, 1.0));
        assert_ne!(bo.delay(1, 0.0), bo.delay(1, 1.0));
    }

    #[test]
    fn 已过期的证书立刻续而不是算出负数() {
        let nb = t(0);
        let na = t(10);
        let d = should_renew(t(1000), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(d.renew_now);
    }

    #[test]
    fn 退避对从来没签成过的域名同样生效() {
        // ★ ★ 这条守的是那个缺陷：退避原先只长在 `should_renew` 里，
        //   而 `should_renew` **只在存储里已经有一张证书时**才被调用。
        //   于是一个永远签不出来的域名（域名写错、80 不通、CAA 挡着）走的是另一条路，
        //   每一轮都直接下单 —— 日志里连着三轮都是「连续第 1 次失败」。
        let bo = Backoff::default(); // base 60s
        let st = RenewalState {
            failures: 3,
            last_attempt: Some(1000),
        };
        // 第 3 次失败后要等 2^2 * 60 = 240s（jitter=0 时不打折）
        let waiting = backoff_remaining(t(1000 + 10), &st, bo, 0.0);
        assert_eq!(waiting, Some(Duration::from_secs(230)));
        // 等够了就放行
        assert_eq!(backoff_remaining(t(1000 + 240), &st, bo, 0.0), None);
        // ★ 反向：没失败过的、以及失败过但没记下时刻的，都不该被压住——
        //   否则一个刚加进配置的新域名会被一个不存在的退避挡在门外。
        assert_eq!(backoff_remaining(t(1000), &fresh(), bo, 0.0), None);
        let no_time = RenewalState {
            failures: 5,
            last_attempt: None,
        };
        assert_eq!(backoff_remaining(t(1000), &no_time, bo, 0.0), None);
    }

    #[test]
    fn 退避真的在指数增长() {
        // ★ 判据不是「有退避」，是**它随失败次数变长**。
        //   原先那个缺陷的表现恰恰是它**不变**（永远停在第 1 次）。
        let bo = Backoff::default();
        let w = |n: u32| {
            let st = RenewalState {
                failures: n,
                last_attempt: Some(0),
            };
            backoff_remaining(t(0), &st, bo, 0.0).unwrap()
        };
        assert!(w(1) < w(2), "{:?} 不小于 {:?}", w(1), w(2));
        assert!(w(2) < w(3));
        assert!(w(3) < w(4));
        // 封顶之后不再涨（否则一次长期故障会把重试推到几万年后）。
        assert_eq!(w(30), w(40));
    }

    #[test]
    fn 下次检查时间被钳在合理区间() {
        let nb = t(0);
        let na = t(3650 * DAY); // 十年
        let d = should_renew(t(1), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(!d.renew_now);
        // ★ 不能因为「离续期还有六年」就六年不检查一次：
        //   ARI 随时可能出现，配置也随时可能变。
        assert!(d.next_check <= MAX_CHECK, "{:?}", d.next_check);
        assert!(d.next_check >= MIN_CHECK);
    }
}
