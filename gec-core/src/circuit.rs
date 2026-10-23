//! 简易熔断器（决策 / L3 / 嵌入 HTTP 客户端用）
//!
//! 目标服务宕机时，无熔断的每次调用都要等满超时（10s）；熔断后
//! 连续失败达到阈值即「跳闸」，冷却期内后续请求**立即失败**（毫秒级、
//! 不发 HTTP），冷却到期后自动放一个探测请求尝试恢复。
//!
//! 三态：
//! - **closed**（正常）：请求照常放行；
//! - **open**（跳闸）：连续失败达 `threshold` 后跳闸 `cooldown`，
//!   期间 [`CircuitBreaker::allow_request`] 返回 `false`；
//! - **half-open**（探测）：冷却到期后放行请求探测——成功则闭合
//!   （重置失败计数），失败则重新跳闸。
//!
//! 两个原子量（连续失败数 + 跳闸时间戳）实现，无锁、跨线程安全；
//! 每个客户端持有独立实例（各服务独立熔断，互不影响）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 简易熔断器
///
/// 用法：请求前 `allow_request()`（`false` = 立即失败，不发请求）；
/// 请求成功后 `on_success()`，失败后 `on_failure()`。
#[derive(Debug)]
pub struct CircuitBreaker {
    /// 连续失败计数（成功即清零）
    failures: AtomicU64,
    /// 跳闸时间戳（进程启动后毫秒数）；0 = 未跳闸
    opened_at: AtomicU64,
    /// 连续失败多少次后跳闸
    threshold: u64,
    /// 跳闸冷却时长（毫秒）
    cooldown_ms: u64,
}

impl CircuitBreaker {
    /// 创建熔断器：连续失败 `threshold` 次跳闸，冷却 `cooldown`
    pub fn new(threshold: u64, cooldown: Duration) -> Self {
        Self {
            failures: AtomicU64::new(0),
            opened_at: AtomicU64::new(0),
            threshold: threshold.max(1),
            cooldown_ms: cooldown.as_millis().min(u64::MAX as u128) as u64,
        }
    }

    /// 是否放行请求：`false` = 已跳闸且冷却未到期（调用方应立即失败）
    ///
    /// 冷却到期后返回 `true`（half-open 探测）；探测成功经 `on_success`
    /// 闭合，失败经 `on_failure` 重新跳闸。
    pub fn allow_request(&self) -> bool {
        let opened = self.opened_at.load(Ordering::Acquire);
        if opened == 0 {
            return true;
        }
        now_ms().saturating_sub(opened) >= self.cooldown_ms
    }

    /// 上报成功：重置失败计数并闭合
    pub fn on_success(&self) {
        self.failures.store(0, Ordering::Release);
        self.opened_at.store(0, Ordering::Release);
    }

    /// 上报失败：连续失败达阈值时跳闸（冷却 `cooldown`）
    pub fn on_failure(&self) {
        let n = self.failures.fetch_add(1, Ordering::AcqRel) + 1;
        if n >= self.threshold {
            self.opened_at.store(now_ms(), Ordering::Release);
        }
    }
}

impl Default for CircuitBreaker {
    /// 缺省：连续失败 3 次跳闸，冷却 30s
    fn default() -> Self {
        Self::new(3, Duration::from_secs(30))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_by_default() {
        let b = CircuitBreaker::default();
        assert!(b.allow_request());
    }

    #[test]
    fn trips_after_threshold_consecutive_failures() {
        let b = CircuitBreaker::new(2, Duration::from_secs(30));
        b.on_failure();
        assert!(b.allow_request(), "未达阈值前应放行");
        b.on_failure();
        assert!(!b.allow_request(), "达到阈值后应跳闸");
        b.on_failure();
        assert!(!b.allow_request(), "跳闸后持续拒绝");
    }

    #[test]
    fn success_resets_failure_count() {
        let b = CircuitBreaker::new(2, Duration::from_secs(30));
        b.on_failure();
        b.on_success();
        b.on_failure();
        assert!(b.allow_request(), "成功应重置连续失败计数");
        b.on_failure();
        assert!(!b.allow_request());
    }

    #[tokio::test]
    async fn half_open_probe_recovers_and_retrips() {
        let b = CircuitBreaker::new(2, Duration::from_millis(50));
        b.on_failure();
        assert!(b.allow_request(), "未达阈值前应放行");
        b.on_failure();
        assert!(!b.allow_request(), "跳闸后拒绝");
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(b.allow_request(), "冷却到期后应放行探测");
        // 探测失败 → 重新跳闸
        b.on_failure();
        assert!(!b.allow_request());
        tokio::time::sleep(Duration::from_millis(60)).await;
        // 探测成功 → 闭合
        assert!(b.allow_request());
        b.on_success();
        // 闭合后重新计数：1 < 2，仍放行
        b.on_failure();
        assert!(b.allow_request(), "闭合后重新计数，未达阈值应放行");
    }
}
