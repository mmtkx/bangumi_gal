use std::sync::Mutex;
use std::time::Duration;

use tokio::time::{sleep_until, Instant};

/// 写入限速器：保证相邻两次调用之间至少间隔 `min_interval`。
///
/// ADR-007：本地到云端写入默认每 3 秒 1 条。并发调用会各自排队，
/// 时间片在锁内一次性预留，避免多个任务同时「等 3 秒」后一起发出。
pub struct RateLimiter {
    min_interval: Duration,
    next_slot: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            next_slot: Mutex::new(None),
        }
    }

    /// ADR-007 默认值。
    pub fn default_interval() -> Self {
        Self::new(Duration::from_secs(3))
    }

    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// 等待直到获得一个发送时间片。
    pub async fn acquire(&self) {
        let target = {
            let mut guard = match self.next_slot.lock() {
                Ok(guard) => guard,
                // 锁中毒时退化为不限制，宁可多发一条也不阻塞同步。
                Err(poisoned) => poisoned.into_inner(),
            };
            let now = Instant::now();
            let slot = match *guard {
                Some(previous) => {
                    // 下一个可用时间点 = 上次预约 + 最小间隔；已过期则立即可用。
                    let candidate = previous + self.min_interval;
                    if candidate > now {
                        candidate
                    } else {
                        now
                    }
                }
                None => now,
            };
            *guard = Some(slot);
            slot
        };
        sleep_until(target).await;
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::default_interval()
    }
}

/// 失败后的指数退避：`base * 2^(attempts-1)`，上限 `cap`。
///
/// attempts 从 1 起（第一次失败等待 base）。ADR-007：失败指数退避并持久化重试。
pub fn backoff_delay(attempts: i64, base: Duration, cap: Duration) -> Duration {
    let exponent = attempts.max(1).saturating_sub(1).min(16) as u32;
    let multiplier = 1u64 << exponent;
    let millis = base.as_millis().saturating_mul(multiplier as u128);
    let capped = millis.min(cap.as_millis());
    Duration::from_millis(u64::try_from(capped).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let base = Duration::from_secs(3);
        let cap = Duration::from_secs(60);
        assert_eq!(backoff_delay(1, base, cap), Duration::from_secs(3));
        assert_eq!(backoff_delay(2, base, cap), Duration::from_secs(6));
        assert_eq!(backoff_delay(3, base, cap), Duration::from_secs(12));
        assert_eq!(backoff_delay(4, base, cap), Duration::from_secs(24));
        assert_eq!(backoff_delay(5, base, cap), Duration::from_secs(48));
        // 第 6 次本应 96s，被上限截断到 60s。
        assert_eq!(backoff_delay(6, base, cap), Duration::from_secs(60));
        // attempts 为 0 或负数按 1 处理，不 panic。
        assert_eq!(backoff_delay(0, base, cap), Duration::from_secs(3));
    }

    #[tokio::test]
    async fn acquire_enforces_min_interval() {
        let limiter = RateLimiter::new(Duration::from_millis(40));
        let started = Instant::now();
        limiter.acquire().await; // 首个时间片立刻可用
        limiter.acquire().await;
        limiter.acquire().await;
        // 三次获取之间至少两段间隔。
        assert!(
            started.elapsed() >= Duration::from_millis(75),
            "实际耗时 {:?}",
            started.elapsed()
        );
    }
}
