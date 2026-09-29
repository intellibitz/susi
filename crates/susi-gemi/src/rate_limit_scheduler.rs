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

/// Snapshot derived from one provider response's headers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitState {
    pub retry_after_secs: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_unix: Option<u64>,
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
