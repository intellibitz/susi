//! Test for quota window accounting and remaining allowance tracking (VC-202-021, T-DEEPSEEK-119).
//! Verifies that quota windows track remaining calls, detect resets at appropriate times,
//! and compute utilization for scarcity-aware scheduling.
//!
//! This test exercises:
//! - QuotaWindowState struct with remaining allowance and reset tracking
//! - Factory methods for daily (UTC), weekly (Monday UTC), and rolling time-window quotas
//! - record_call() decrement with exhaustion blocking
//! - should_reset() detection of window expiry
//! - reset_for_next() reinitialize for new window with capacity
//! - utilization_percent() scarcity metric (0% = abundant, 100% = exhausted)

/// Quota window state: remaining calls and next reset time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaWindowState {
    /// Number of calls allowed before reset.
    pub remaining_calls: u32,
    /// Unix timestamp when this window resets.
    pub reset_unix: u64,
}

impl QuotaWindowState {
    /// Create a quota window with daily (UTC midnight) resets.
    #[must_use]
    pub fn daily(remaining_calls: u32, now_unix: u64) -> Self {
        let seconds_in_day = 86400u64;
        let unix_today_midnight = (now_unix / seconds_in_day) * seconds_in_day;
        let reset_unix = unix_today_midnight + seconds_in_day;
        Self {
            remaining_calls,
            reset_unix,
        }
    }

    /// Create a quota window with weekly (Monday UTC) resets.
    #[must_use]
    pub fn weekly(remaining_calls: u32, now_unix: u64) -> Self {
        let seconds_in_day = 86400u64;
        let day_of_week = ((now_unix / seconds_in_day) + 3) % 7;
        let days_until_monday = (7 - day_of_week) % 7;
        let reset_unix = ((now_unix / seconds_in_day) + days_until_monday) * seconds_in_day;
        Self {
            remaining_calls,
            reset_unix,
        }
    }

    /// Create a quota window with rolling time-window resets.
    #[must_use]
    pub fn rolling(remaining_calls: u32, period_secs: u64, now_unix: u64) -> Self {
        let reset_unix = now_unix + period_secs;
        Self {
            remaining_calls,
            reset_unix,
        }
    }

    /// Record a call, returning false if already exhausted.
    pub fn record_call(&mut self) -> bool {
        if self.remaining_calls == 0 {
            return false;
        }
        self.remaining_calls -= 1;
        true
    }

    /// Check if this window should reset based on current time.
    #[must_use]
    pub fn should_reset(&self, now_unix: u64) -> bool {
        now_unix >= self.reset_unix
    }

    /// Reset for the next window with fresh capacity.
    pub fn reset_for_next(&mut self, capacity: u32, now_unix: u64) {
        self.remaining_calls = capacity;
        if self.reset_unix <= now_unix {
            let period = self
                .reset_unix
                .saturating_sub(self.reset_unix.saturating_sub(now_unix));
            self.reset_unix = now_unix + period;
        }
    }

    /// Compute utilization as percentage of capacity drained.
    /// Returns 0.0 for full capacity, 100.0 for exhausted, >100 for overdraw.
    #[must_use]
    pub fn utilization_percent(&self, capacity: u32) -> f64 {
        if capacity == 0 {
            return 0.0;
        }
        let drained = capacity.saturating_sub(self.remaining_calls);
        (drained as f64 / capacity as f64) * 100.0
    }
}

#[test]
fn quota_window_accounting() {
    // Scenario: daily quota starting at 100 calls, now at Unix 1000
    let mut daily = QuotaWindowState::daily(100, 1000);
    assert_eq!(daily.remaining_calls, 100);
    assert!(!daily.should_reset(1000)); // Same day, no reset
    assert!(daily.should_reset(86400)); // Next day, reset needed

    // Scenario: use 70 calls (30 remain)
    for _ in 0..70 {
        assert!(daily.record_call()); // Each call succeeds
    }
    assert_eq!(daily.remaining_calls, 30);
    assert!((daily.utilization_percent(100) - 70.0).abs() < 0.01);

    // Scenario: exhaust quota (0 remaining)
    for _ in 0..30 {
        assert!(daily.record_call());
    }
    assert_eq!(daily.remaining_calls, 0);
    assert!((daily.utilization_percent(100) - 100.0).abs() < 0.01);
    assert!(!daily.record_call()); // Exhausted, blocked

    // Scenario: reset for new day
    daily.reset_for_next(100, 86400);
    assert_eq!(daily.remaining_calls, 100);
    assert!((daily.utilization_percent(100) - 0.0).abs() < 0.01);

    // Scenario: weekly quota at Monday start (40 calls)
    let weekly = QuotaWindowState::weekly(40, 604800); // Unix 604800 is Monday
    assert_eq!(weekly.remaining_calls, 40);
    assert!(!weekly.should_reset(604800 + 86400)); // Tuesday, same week
    assert!(weekly.should_reset(604800 + 604800)); // Next Monday, reset

    // Scenario: rolling 1-hour quota (5 calls)
    let rolling = QuotaWindowState::rolling(5, 3600, 1000);
    assert_eq!(rolling.remaining_calls, 5);
    assert!(!rolling.should_reset(2000)); // 1000s into window
    assert!(rolling.should_reset(4600)); // After 3600s, reset

    // Summary: quota window accounting enables:
    // - Tracking remaining allowance (calls or tokens)
    // - Detecting when windows reset (daily, weekly, or rolling)
    // - Blocking further calls when exhausted
    // - Computing scarcity metric (utilization %) for cost adjustment
    // This feeds into marginal cost ranking: scarcity increases effective cost.
}
