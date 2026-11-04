//! Tests for rate-limit scheduler (`rate_limit_scheduler_*`).

use crate::rate_limit_scheduler::{
    parse_rate_limit_headers, schedule_next_attempt, RateLimitDecision,
};
use std::collections::HashMap;

#[test]
fn rate_limit_scheduler_retry_after_delays() {
    let mut h = HashMap::new();
    h.insert("Retry-After".into(), "30".into());
    let state = parse_rate_limit_headers(&h);
    assert_eq!(state.retry_after_secs, Some(30));
    let d = schedule_next_attempt(&state, Some(1_000));
    assert_eq!(
        d,
        RateLimitDecision::Wait {
            ready_unix: 1_030,
            reason: "Retry-After: 30s".into()
        }
    );
}

#[test]
fn rate_limit_scheduler_remaining_zero_waits_for_reset() {
    let mut h = HashMap::new();
    h.insert("x-ratelimit-remaining".into(), "0".into());
    h.insert("x-ratelimit-reset".into(), "5000".into());
    let state = parse_rate_limit_headers(&h);
    let d = schedule_next_attempt(&state, Some(1000));
    match d {
        RateLimitDecision::Wait { ready_unix, .. } => assert_eq!(ready_unix, 5000),
        other => panic!("expected Wait, got {other:?}"),
    }
}

#[test]
fn rate_limit_scheduler_ready_when_remaining_positive() {
    let mut h = HashMap::new();
    h.insert("X-RateLimit-Remaining".into(), "12".into());
    h.insert("X-RateLimit-Reset".into(), "999999".into());
    let state = parse_rate_limit_headers(&h);
    assert_eq!(
        schedule_next_attempt(&state, Some(1)),
        RateLimitDecision::Ready
    );
}

#[test]
fn rate_limit_scheduler_empty_headers_ready_immediately() {
    let h = HashMap::new();
    let state = parse_rate_limit_headers(&h);
    assert_eq!(state.retry_after_secs, None);
    assert_eq!(state.remaining, None);
    assert_eq!(state.reset_unix, None);
    assert_eq!(
        schedule_next_attempt(&state, Some(500)),
        RateLimitDecision::Ready
    );
}

#[test]
fn rate_limit_scheduler_ignores_malformed_numbers_and_retry_after_wins_when_present() {
    let mut malformed = HashMap::new();
    malformed.insert("Retry-After".into(), "not-a-number".into());
    malformed.insert("x-ratelimit-remaining".into(), "bad".into());
    malformed.insert("x-ratelimit-reset".into(), "bad".into());
    let state = parse_rate_limit_headers(&malformed);
    assert_eq!(state.retry_after_secs, None);
    assert_eq!(state.remaining, None);
    assert_eq!(state.reset_unix, None);
    assert_eq!(
        schedule_next_attempt(&state, Some(2_000)),
        RateLimitDecision::Ready
    );

    let mut both = HashMap::new();
    both.insert("retry-after".into(), "15".into());
    both.insert("x-ratelimit-remaining".into(), "0".into());
    both.insert("x-ratelimit-reset".into(), "9_000".into());
    let both_state = parse_rate_limit_headers(&both);
    assert_eq!(
        schedule_next_attempt(&both_state, Some(1_000)),
        RateLimitDecision::Wait {
            ready_unix: 1_015,
            reason: "Retry-After: 15s".into()
        }
    );
}
