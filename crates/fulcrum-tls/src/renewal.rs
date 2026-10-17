//! 續期判定（G56）：**ARI 優先，退回「剩餘壽命 1/3」；失敗走指數退避 + 隨機抖動**。
//!
//! ★ ★ **整個模組是純函式**：時間與抖動都從參數進來，一個 `SystemTime::now()` 都沒有。
//! 理由是「判據不能掛在會抖的量上」——續期策略最要緊的性質（6 天短證書不會被判成
//! 「永遠在續期」、退避真的在退、抖動真的在抖）只有在**可以指定時間**的前提下才測得出來。
//!
//! # ★ 為什麼不寫死天數
//!
//! **Let's Encrypt 正在推 6 天短壽命證書**。「到期前 30 天續」在那種證書上
//! 等於**永遠在續期**——每次檢查都滿足條件，於是每次檢查都發一次簽發請求，
//! 而失敗要消耗 CA 的速率配額。比例制對壽命免疫。
//!
//! # ★ ARI 還有比例制給不了的東西
//!
//! CA 因安全事件需要**提前批量撤銷重簽**時，ARI（RFC 9773）是它通知你的唯一渠道。
//! 不接就只能等出事——所以順序是「先問 ARI，問不到才用比例」，不是「兩者取其一」。

use std::time::{Duration, SystemTime};

/// CA 通過 ARI 給出的建議續期視窗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AriWindow {
    pub start: SystemTime,
    pub end: SystemTime,
}

/// 一張證書的續期狀態。存在 `meta.json` 裡——**它記的是證書裡沒有的東西**。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenewalState {
    /// 連續失敗次數。成功一次就歸零。
    pub failures: u32,
    /// 上一次嘗試的時刻（Unix 秒）。`None` = 還沒試過。
    pub last_attempt: Option<u64>,
}

/// 退避參數。★ 做成結構體而不是常數，是為了讓測試能用小數值跑完整條曲線。
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub base: Duration,
    pub cap: Duration,
    /// 抖動幅度佔該次退避的比例（0.0 = 不抖）。
    pub jitter_ratio: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            base: Duration::from_secs(60),
            cap: Duration::from_secs(6 * 3600),
            // ⚠ **抖動不是可選項**：同一臺機上幾十個網域同時到期時，
            //   不抖動會把重試打成尖峰——而尖峰正好是 CA 速率限制針對的形狀。
            jitter_ratio: 0.5,
        }
    }
}

impl Backoff {
    /// 第 `failures` 次失敗之後要等多久。`jitter_unit` 取 `[0, 1)`，由呼叫方給。
    ///
    /// ★ 抖動值從參數進來而不是在這裡 `rand`：否則這個函式沒法被確定性地測，
    /// 而「時快時慢的反證等於沒有反證」是本儲存庫的老紀律。
    pub fn delay(&self, failures: u32, jitter_unit: f64) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }
        // 2^(n-1) * base，封頂。★ 用 checked/saturating：failures 大到 40 時
        //   `1u64 << 39` 秒是一萬多年，乘出來會溢出——而溢出 panic 在續期路徑上
        //   等於把一次簽發失敗升級成處理程序崩潰。
        let shift = (failures - 1).min(32);
        let mult = 1u64 << shift;
        let raw = self
            .base
            .checked_mul(mult.min(u32::MAX as u64) as u32)
            .unwrap_or(self.cap)
            .min(self.cap);
        let j = jitter_unit.clamp(0.0, 1.0) * self.jitter_ratio;
        // 抖動是**減法**：在 [raw*(1-j), raw] 之間。往小抖不會把重試推到更遠的將來，
        // 而往大抖會讓「封頂」這件事失效。
        let secs = raw.as_secs_f64() * (1.0 - j);
        Duration::from_secs_f64(secs.max(0.0))
    }
}

/// 判定結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// 現在就該續。
    pub renew_now: bool,
    /// 人讀的理由——★ 它會進紀錄，所以必須說清楚「為什麼是現在」。
    pub reason: String,
    /// 下一次該在多久之後再檢查。
    pub next_check: Duration,
}

/// 最長與最短的檢查間隔。
const MAX_CHECK: Duration = Duration::from_secs(12 * 3600);
const MIN_CHECK: Duration = Duration::from_secs(60);

/// 退避還要壓多久。`None` = 現在可以試。
///
/// ★ ★ **它被抽出來，是因為它有兩個呼叫點** —— 只長在其中一個上就是一半的實作。
/// [`should_renew`] 只在「儲存裡已經有一張證書」時才被呼叫——於是一個**從來沒簽成過**
/// 的網域（網域寫錯、80 連接埠不通、CAA 擋著）根本走不到這裡：
/// 它每一輪都直接去下單，退避形同虛設。
/// ⚠ 後果不是「多試幾次」：CA 的失敗驗證配額是**按帳戶**算的，
/// 一個簽不出來的網域會把整個帳戶的配額耗光，**連累同一臺機器上別的網域也簽不出來**。
/// 那正是 G56 裡退避存在的理由，卻恰好在它最該生效的那一種情形上缺席。
/// 由 ACME 門禁的反證跑抓到：紀錄裡連著三輪都是「連續第 1 次失敗」。
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

/// 該不該現在續期。
///
/// 順序：**退避 → ARI → 比例**。退避排最前是因為它是**否決項**：
/// 上一次失敗之後還沒到重試時刻，就算 ARI 說該續也不能續（那只會再撞一次速率限制）。
pub fn should_renew(
    now: SystemTime,
    not_before: SystemTime,
    not_after: SystemTime,
    ari: Option<AriWindow>,
    state: &RenewalState,
    backoff: Backoff,
    jitter_unit: f64,
) -> Decision {
    // ── 1. 退避（否決項）───────────────────────────────────────────────────
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

    // ── 2. ARI（CA 說話優先）───────────────────────────────────────────────
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

    // ── 3. 比例制：剩餘壽命 <= 總壽命的 1/3 ────────────────────────────────
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
        // 剩 31 天：還不到 1/3（30 天）
        let d = should_renew(t(59 * DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(!d.renew_now, "{}", d.reason);
        // 剩 30 天：正好到線
        let d = should_renew(t(60 * DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(d.renew_now, "{}", d.reason);
    }

    #[test]
    fn 六天短证书不会被判成永远在续期() {
        // ★ ★ 這條是 G56 不寫死天數的**全部理由**：
        //   「到期前 30 天續」在 6 天壽命的證書上，從簽發那一刻就成立。
        let nb = t(0);
        let na = t(6 * DAY);
        // 第 1 天：剩 5 天 > 2 天（1/3），不該續
        let d = should_renew(t(DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(!d.renew_now, "刚签发就要续 = 永远在续期：{}", d.reason);
        // 第 4 天：剩 2 天 = 1/3，該續
        let d = should_renew(t(4 * DAY), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(d.renew_now, "{}", d.reason);
    }

    #[test]
    fn ari_优先于比例制_两个方向都要对() {
        let nb = t(0);
        let na = t(90 * DAY);
        // ① ARI 說「現在就續」，而比例制還沒到 → 聽 ARI
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

        // ② ARI 說「還沒到」，而比例制已經到了 → **也聽 ARI**。
        //    ★ 這一條容易寫反：ARI 是 CA 的排期，它比我們的比例制更知道該什麼時候續。
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
        // 第 3 次失敗 → 等 4*60 = 240s；now = 1000 + 100 還沒到
        let d = should_renew(t(1100), nb, na, Some(ari), &state, bo, 0.0);
        assert!(!d.renew_now, "退避没到就不许续：{}", d.reason);
        assert!(d.reason.contains("退避"));
        // 到了之後就放行
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
        // 封頂
        assert_eq!(bo.delay(5, 0.0), Duration::from_secs(100));
        assert_eq!(bo.delay(50, 0.0), Duration::from_secs(100));
        // ★ 大失敗次數不許溢出 panic：一次簽發失敗不該升級成處理程序崩潰
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
        // ★ 只往小抖：抖動不能把重試推到比封頂更遠的將來
        assert!(full <= none && half <= none);
        // ★ 而它必須真的在抖——ratio 為 0 時才允許恆定
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
        // ★ ★ 這條守的是那個缺陷：退避原先只長在 `should_renew` 裡，
        //   而 `should_renew` **只在儲存裡已經有一張證書時**才被呼叫。
        //   於是一個永遠簽不出來的網域（網域寫錯、80 不通、CAA 擋著）走的是另一條路，
        //   每一輪都直接下單 —— 紀錄裡連著三輪都是「連續第 1 次失敗」。
        let bo = Backoff::default(); // base 60s
        let st = RenewalState {
            failures: 3,
            last_attempt: Some(1000),
        };
        // 第 3 次失敗後要等 2^2 * 60 = 240s（jitter=0 時不打折）
        let waiting = backoff_remaining(t(1000 + 10), &st, bo, 0.0);
        assert_eq!(waiting, Some(Duration::from_secs(230)));
        // 等夠了就放行
        assert_eq!(backoff_remaining(t(1000 + 240), &st, bo, 0.0), None);
        // ★ 反向：沒失敗過的、以及失敗過但沒記下時刻的，都不該被壓住——
        //   否則一個剛加進設定的新網域會被一個不存在的退避擋在門外。
        assert_eq!(backoff_remaining(t(1000), &fresh(), bo, 0.0), None);
        let no_time = RenewalState {
            failures: 5,
            last_attempt: None,
        };
        assert_eq!(backoff_remaining(t(1000), &no_time, bo, 0.0), None);
    }

    #[test]
    fn 退避真的在指数增长() {
        // ★ 判據不是「有退避」，是**它隨失敗次數變長**。
        //   原先那個缺陷的表現恰恰是它**不變**（永遠停在第 1 次）。
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
        // 封頂之後不再漲（否則一次長期故障會把重試推到幾萬年後）。
        assert_eq!(w(30), w(40));
    }

    #[test]
    fn 下次检查时间被钳在合理区间() {
        let nb = t(0);
        let na = t(3650 * DAY); // 十年
        let d = should_renew(t(1), nb, na, None, &fresh(), Backoff::default(), 0.0);
        assert!(!d.renew_now);
        // ★ 不能因為「離續期還有六年」就六年不檢查一次：
        //   ARI 隨時可能出現，設定也隨時可能變。
        assert!(d.next_check <= MAX_CHECK, "{:?}", d.next_check);
        assert!(d.next_check >= MIN_CHECK);
    }
}
