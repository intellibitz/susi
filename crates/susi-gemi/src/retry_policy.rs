//! Uniform retry with capped exponential backoff, jitter, and idempotency.

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryDecision {
    pub retry: bool,
    pub delay_ms: u64,
}

/// Decide whether to retry; never retry auth/funds/non-idempotent failures.
#[must_use]
pub fn should_retry(kind: FailureKind, attempt: u32, jitter_ms: u64) -> RetryDecision {
    let retryable = matches!(
        kind,
        FailureKind::Transient5xx | FailureKind::Timeout | FailureKind::ConnectionReset
    );
    if !retryable || attempt >= 5 {
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
}
