//! Uniform retry with capped exponential backoff, AWS/Brookers full decorrelated
//! jitter, idempotency guards, and a token-bucket retry storm budget.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    Transient5xx,
    Timeout,
    ConnectionReset,
    Auth,
    Funds,
    NonIdempotent,
    Client4xx,
}

impl FailureKind {
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Transient5xx | Self::Timeout | Self::ConnectionReset
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryDecision {
    pub retry: bool,
    pub delay_ms: u64,
}

/// Decide whether to retry; never retry auth/funds/non-idempotent failures.
#[must_use]
pub fn should_retry(kind: FailureKind, attempt: u32, jitter_ms: u64) -> RetryDecision {
    if !kind.is_retryable() || attempt >= 5 {
        return RetryDecision {
            retry: false,
            delay_ms: 0,
        };
    }
    let base = 100u64.saturating_mul(1u64 << attempt.min(4));
    let capped = base.min(5_000);
    RetryDecision {
        retry: true,
        delay_ms: capped.saturating_add(jitter_ms % 100),
    }
}

/// SplitMix64 deterministic mixer for reproducible decorrelated jitter without external RNG state.
#[must_use]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Brookers / AWS Architecture Decorrelated Jitter backoff:
/// $d_t = \min(D_{\max}, \text{Uniform}(D_{\min}, 3 \cdot d_{t-1}))$.
/// Eliminates synchronized retry waves across concurrent workers while keeping
/// deterministic testability from `(attempt, prev_delay_ms, seed)`.
#[must_use]
pub fn decorrelated_retry(
    kind: FailureKind,
    attempt: u32,
    prev_delay_ms: u64,
    seed: u64,
) -> RetryDecision {
    const BASE_MS: u64 = 100;
    const CAP_MS: u64 = 5_000;
    if !kind.is_retryable() || attempt >= 5 {
        return RetryDecision {
            retry: false,
            delay_ms: 0,
        };
    }
    let prev = prev_delay_ms.max(BASE_MS);
    let upper = prev.saturating_mul(3).max(BASE_MS + 1);
    let span = upper.saturating_sub(BASE_MS).max(1);
    let rand_u64 = splitmix64(seed ^ (u64::from(attempt).wrapping_mul(0x517c_c1b7_2722_0a95)));
    let sampled = BASE_MS.saturating_add(rand_u64 % span).min(CAP_MS);
    RetryDecision {
        retry: true,
        delay_ms: sampled,
    }
}

/// Token-bucket Retry Storm Guard: enforces a strict mathematical upper bound on
/// retry amplification (e.g. retries cannot exceed 10% of normal request traffic
/// once the burst reserve is spent), preventing cascading retry storms during
/// provider brownouts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryStormBudget {
    pub capacity_tokens: f64,
    pub available_tokens: f64,
    /// Tokens deposited per primary request (e.g. `0.1` = 10% retry ratio).
    pub deposit_per_request: f64,
    /// Tokens consumed per retry (`1.0`).
    pub cost_per_retry: f64,
}

impl Default for RetryStormBudget {
    fn default() -> Self {
        Self::new(10.0, 0.1)
    }
}

impl RetryStormBudget {
    #[must_use]
    pub fn new(capacity_tokens: f64, retry_ratio: f64) -> Self {
        let cap = capacity_tokens.max(1.0);
        Self {
            capacity_tokens: cap,
            available_tokens: cap,
            deposit_per_request: retry_ratio.clamp(0.01, 1.0),
            cost_per_retry: 1.0,
        }
    }

    /// Record a primary outbound request, replenishing the retry token bucket.
    pub fn note_primary_request(&mut self) {
        self.available_tokens =
            (self.available_tokens + self.deposit_per_request).min(self.capacity_tokens);
    }

    /// Attempt to withdraw one retry token on top of [`decorrelated_retry`].
    pub fn try_retry(
        &mut self,
        kind: FailureKind,
        attempt: u32,
        prev_delay_ms: u64,
        seed: u64,
    ) -> RetryDecision {
        let base = decorrelated_retry(kind, attempt, prev_delay_ms, seed);
        if !base.retry || self.available_tokens + 1e-9 < self.cost_per_retry {
            return RetryDecision {
                retry: false,
                delay_ms: 0,
            };
        }
        self.available_tokens = (self.available_tokens - self.cost_per_retry).max(0.0);
        base
    }
}

#[cfg(test)]
mod retry_policy_tests {
    use super::*;

    #[test]
    fn retry_policy_skips_auth_and_caps_backoff() {
        assert!(!should_retry(FailureKind::Auth, 0, 10).retry);
        assert!(!should_retry(FailureKind::Funds, 0, 10).retry);
        assert!(!should_retry(FailureKind::NonIdempotent, 0, 10).retry);
        let r = should_retry(FailureKind::Timeout, 0, 25);
        assert!(r.retry);
        assert!(r.delay_ms >= 100);
        let late = should_retry(FailureKind::Transient5xx, 5, 0);
        assert!(!late.retry);
    }

    #[test]
    fn decorrelated_jitter_and_storm_budget_bound_retry_amplification() {
        let mut budget = RetryStormBudget::new(3.0, 0.1);
        let mut prev = 100;
        for attempt in 0..3 {
            let d = budget.try_retry(FailureKind::Transient5xx, attempt, prev, 42);
            assert!(d.retry);
            assert!((100..=5_000).contains(&d.delay_ms));
            prev = d.delay_ms;
        }
        // 4th consecutive retry without primary requests depletes the storm bucket:
        let blocked = budget.try_retry(FailureKind::Transient5xx, 3, prev, 42);
        assert!(!blocked.retry);

        // 10 primary requests replenish 10 * 0.1 = 1.0 retry token:
        for _ in 0..10 {
            budget.note_primary_request();
        }
        assert!(budget.try_retry(FailureKind::Timeout, 0, 100, 99).retry);
    }

    #[test]
    fn failure_kind_is_retryable_partitions_transient_vs_permanent() {
        assert!(FailureKind::Transient5xx.is_retryable());
        assert!(FailureKind::Timeout.is_retryable());
        assert!(FailureKind::ConnectionReset.is_retryable());

        assert!(!FailureKind::Auth.is_retryable());
        assert!(!FailureKind::Funds.is_retryable());
        assert!(!FailureKind::NonIdempotent.is_retryable());
        assert!(!FailureKind::Client4xx.is_retryable());
    }

    #[test]
    fn decorrelated_retry_is_deterministic_capped_and_stops_at_max_attempts() {
        let d1 = decorrelated_retry(FailureKind::ConnectionReset, 2, 400, 12345);
        let d2 = decorrelated_retry(FailureKind::ConnectionReset, 2, 400, 12345);
        assert_eq!(d1, d2);
        assert!(d1.retry);
        assert!((100..=5_000).contains(&d1.delay_ms));

        let capped = decorrelated_retry(FailureKind::Transient5xx, 4, 50_000, 999);
        assert!(capped.retry);
        assert!(capped.delay_ms <= 5_000);

        let exhausted = decorrelated_retry(FailureKind::Transient5xx, 5, 400, 12345);
        assert_eq!(
            exhausted,
            RetryDecision {
                retry: false,
                delay_ms: 0
            }
        );
    }

    #[test]
    fn retry_storm_budget_default_and_clamping_keep_tokens_bounded() {
        let mut default_budget = RetryStormBudget::default();
        assert_eq!(default_budget.capacity_tokens, 10.0);
        assert_eq!(default_budget.available_tokens, 10.0);
        assert_eq!(default_budget.deposit_per_request, 0.1);
        default_budget.note_primary_request();
        assert_eq!(default_budget.available_tokens, 10.0);

        let clamped = RetryStormBudget::new(0.2, 5.0);
        assert_eq!(clamped.capacity_tokens, 1.0);
        assert_eq!(clamped.available_tokens, 1.0);
        assert_eq!(clamped.deposit_per_request, 1.0);
    }
}
