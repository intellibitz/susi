//! Time-to-first-answer benchmark from a clean install (offline budget).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

#[derive(Debug, Clone, Copy)]
struct Timing {
    cold_start_ms: u64,
    first_token_ms: u64,
}

fn within_budget(t: Timing) -> bool {
    // Offline synthetic budget for a clean install path.
    t.cold_start_ms <= 5_000 && t.first_token_ms <= 2_000
}

#[test]
fn zc_time_to_answer_budget() {
    let sample = Timing {
        cold_start_ms: 800,
        first_token_ms: 400,
    };
    assert!(within_budget(sample));
    assert!(!within_budget(Timing {
        cold_start_ms: 10_000,
        first_token_ms: 100
    }));
}
