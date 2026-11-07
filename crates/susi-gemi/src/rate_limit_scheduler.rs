//! Rate-limit-aware scheduler using response headers (VC-201-053).
//!
//! Parses `Retry-After`, `x-ratelimit-remaining`, and `x-ratelimit-reset`
//! (common OpenAI/Anthropic/GitHub shapes) and decides when the next
//! request may fire.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Decision for the next outbound attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitDecision {
    /// May send immediately.
    Ready,
    /// Wait until `ready_unix` (inclusive).
    Wait { ready_unix: u64, reason: String },
}

impl RateLimitDecision {
    /// Verify structural invariants on a rate-limit scheduling decision.
    pub fn verify_decision_invariants(&self) -> Result<(), String> {
        match self {
            Self::Ready => Ok(()),
            Self::Wait { ready_unix, reason } => {
                if *ready_unix == 0 {
                    return Err("RateLimitDecision::Wait ready_unix must be non-zero".to_string());
                }
                if reason.trim().is_empty() {
                    return Err("RateLimitDecision::Wait reason must not be empty".to_string());
                }
                Ok(())
            }
        }
    }
}

/// Snapshot derived from one provider response's headers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitState {
    pub retry_after_secs: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_unix: Option<u64>,
}

impl RateLimitState {
    /// Verify structural invariants on parsed rate-limit header state.
    pub fn verify_state_invariants(&self) -> Result<(), String> {
        if let Some(secs) = self.retry_after_secs {
            if secs > 86_400 {
                return Err(format!(
                    "RateLimitState retry_after_secs ({secs}) exceeds 86400s"
                ));
            }
        }
        if let Some(reset) = self.reset_unix {
            if reset == 0 || reset > 10_000_000_000 {
                return Err(format!(
                    "RateLimitState reset_unix ({reset}) must be normalized unix seconds in 1..=10_000_000_000"
                ));
            }
        }
        Ok(())
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn header_get<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Parse rate-limit headers (case-insensitive keys).
#[must_use]
pub fn parse_rate_limit_headers(headers: &HashMap<String, String>) -> RateLimitState {
    let retry_after_secs = header_get(headers, "retry-after").and_then(|v| {
        // Integer seconds, or HTTP-date ignored (treat as None).
        v.trim().parse::<u64>().ok()
    });
    let remaining = header_get(headers, "x-ratelimit-remaining")
        .or_else(|| header_get(headers, "x-ratelimit-remaining-requests"))
        .and_then(|v| v.trim().parse::<u64>().ok());
    let reset_unix = header_get(headers, "x-ratelimit-reset")
        .or_else(|| header_get(headers, "x-ratelimit-reset-requests"))
        .and_then(|v| {
            let t = v.trim();
            if let Ok(secs) = t.parse::<u64>() {
                // Heuristic: values under 1e11 are unix seconds; larger are ms.
                return Some(if secs > 10_000_000_000 {
                    secs / 1000
                } else {
                    secs
                });
            }
            None
        });
    RateLimitState {
        retry_after_secs,
        remaining,
        reset_unix,
    }
}

/// Decide whether to send now given parsed state and wall clock.
#[must_use]
pub fn schedule_next_attempt(
    state: &RateLimitState,
    now_unix_secs: Option<u64>,
) -> RateLimitDecision {
    let now = now_unix_secs.unwrap_or_else(now_unix);
    if let Some(secs) = state.retry_after_secs {
        return RateLimitDecision::Wait {
            ready_unix: now.saturating_add(secs),
            reason: format!("Retry-After: {secs}s"),
        };
    }
    if state.remaining == Some(0) {
        let ready = state.reset_unix.unwrap_or_else(|| now.saturating_add(1));
        return RateLimitDecision::Wait {
            ready_unix: ready.max(now),
            reason: "x-ratelimit-remaining is 0".to_string(),
        };
    }
    if let Some(reset) = state.reset_unix {
        if state.remaining == Some(0) && reset > now {
            return RateLimitDecision::Wait {
                ready_unix: reset,
                reason: "waiting for x-ratelimit-reset".to_string(),
            };
        }
    }
    RateLimitDecision::Ready
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn parse_rate_limit_headers_matches_case_insensitively_and_normalizes_millis() {
        let mut headers = HashMap::new();
        headers.insert("Retry-After".to_string(), " 15 ".to_string());
        headers.insert(
            "X-RateLimit-Remaining-Requests".to_string(),
            "42".to_string(),
        );
        headers.insert("X-RateLimit-Reset".to_string(), "1700000000000".to_string());

        let state = parse_rate_limit_headers(&headers);
        assert_eq!(state.retry_after_secs, Some(15));
        assert_eq!(state.remaining, Some(42));
        assert_eq!(state.reset_unix, Some(1_700_000_000));
    }

    #[test]
    fn parse_rate_limit_headers_ignores_non_numeric_values() {
        let mut headers = HashMap::new();
        headers.insert(
            "retry-after".to_string(),
            "Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
        );
        headers.insert("x-ratelimit-remaining".to_string(), "invalid".to_string());
        let state = parse_rate_limit_headers(&headers);
        assert_eq!(state.retry_after_secs, None);
        assert_eq!(state.remaining, None);
        assert_eq!(state.reset_unix, None);
    }

    #[test]
    fn schedule_next_attempt_prioritizes_retry_after_and_zero_remaining() {
        let retry_state = RateLimitState {
            retry_after_secs: Some(30),
            remaining: Some(10),
            reset_unix: None,
        };
        assert_eq!(
            schedule_next_attempt(&retry_state, Some(1_000)),
            RateLimitDecision::Wait {
                ready_unix: 1_030,
                reason: "Retry-After: 30s".to_string(),
            }
        );

        let exhausted = RateLimitState {
            retry_after_secs: None,
            remaining: Some(0),
            reset_unix: Some(1_050),
        };
        assert_eq!(
            schedule_next_attempt(&exhausted, Some(1_000)),
            RateLimitDecision::Wait {
                ready_unix: 1_050,
                reason: "x-ratelimit-remaining is 0".to_string(),
            }
        );
    }

    #[test]
    fn schedule_next_attempt_returns_ready_when_quota_remains() {
        let ready_state = RateLimitState {
            retry_after_secs: None,
            remaining: Some(5),
            reset_unix: Some(2_000),
        };
        assert_eq!(
            schedule_next_attempt(&ready_state, Some(1_000)),
            RateLimitDecision::Ready
        );
    }

    #[test]
    fn schedule_next_attempt_defaults_to_one_second_wait_when_zero_remaining_lacks_reset() {
        let state = RateLimitState {
            retry_after_secs: None,
            remaining: Some(0),
            reset_unix: None,
        };
        assert_eq!(
            schedule_next_attempt(&state, Some(500)),
            RateLimitDecision::Wait {
                ready_unix: 501,
                reason: "x-ratelimit-remaining is 0".to_string(),
            }
        );
    }

    #[test]
    fn verify_rate_limit_state_and_decision_invariants() {
        assert!(RateLimitDecision::Ready
            .verify_decision_invariants()
            .is_ok());
        assert!(RateLimitDecision::Wait {
            ready_unix: 1_700_000_000,
            reason: "Retry-After: 10s".to_string(),
        }
        .verify_decision_invariants()
        .is_ok());
        assert!(RateLimitDecision::Wait {
            ready_unix: 0,
            reason: "Retry-After: 10s".to_string(),
        }
        .verify_decision_invariants()
        .is_err());
        assert!(RateLimitDecision::Wait {
            ready_unix: 1_700_000_000,
            reason: "   ".to_string(),
        }
        .verify_decision_invariants()
        .is_err());

        let valid_state = RateLimitState {
            retry_after_secs: Some(60),
            remaining: Some(10),
            reset_unix: Some(1_700_000_000),
        };
        assert!(valid_state.verify_state_invariants().is_ok());

        let excessive_retry = RateLimitState {
            retry_after_secs: Some(90_000),
            ..valid_state
        };
        assert!(excessive_retry.verify_state_invariants().is_err());

        let unnormalized_reset = RateLimitState {
            reset_unix: Some(1_700_000_000_000),
            ..valid_state
        };
        assert!(unnormalized_reset.verify_state_invariants().is_err());
    }
}
