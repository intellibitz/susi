//! Per-provider pacing: stay under a provider's rate limits instead of
//! running into them.
//!
//! A gateway serves many agents at once, and every one of them would
//! otherwise send as fast as it can until the provider answers 429. Three
//! controls keep the gateway's own traffic inside the provider's limits:
//!
//! - **Requests per minute**, `SUSI_HARNESS_RPM_<PROVIDER>`: a call waits for
//!   a free slot in the sliding minute.
//! - **Tokens per minute**, `SUSI_HARNESS_TPM_<PROVIDER>`: a call waits while
//!   the tokens the provider served in the last minute are at the cap.
//! - **`Retry-After`**: when a provider answers 429 with it, every call to
//!   that provider waits it out (up to [`MAX_PAUSE_MS`]) instead of
//!   hitting the limit again.
//!
//! Limits are read like keys, from the process, user or machine environment,
//! and a provider with no limit set is not paced. The pacing is in-process:
//! one gateway paces all the agents that share it.

use crate::secrets;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock, PoisonError};

/// The sliding window, in milliseconds.
pub const WINDOW_MS: u64 = 60_000;

/// The longest a `Retry-After` pauses a provider. A provider asking for
/// longer is cooled down by the ledger instead
/// ([`crate::policy::cooling_providers`]).
pub const MAX_PAUSE_MS: u64 = 120_000;

/// One provider's limits and recent traffic.
#[derive(Debug, Default)]
struct Lane {
    /// The configured caps, read once.
    limits: Option<Limits>,
    /// Start times of the calls in (or reserved for) the window, ascending.
    calls: VecDeque<u64>,
    /// When each recent call finished and how many tokens it used.
    tokens: VecDeque<(u64, u64)>,
    /// No call starts before this time (a provider's `Retry-After`).
    paused_until: u64,
}

/// A provider's caps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// Requests per minute.
    pub rpm: Option<u32>,
    /// Tokens per minute.
    pub tpm: Option<u64>,
}

impl Limits {
    /// The caps configured for `provider`, from
    /// `SUSI_HARNESS_RPM_<PROVIDER>` and `SUSI_HARNESS_TPM_<PROVIDER>`.
    #[must_use]
    pub fn configured(provider: &str) -> Self {
        let upper = provider.to_ascii_uppercase().replace('-', "_");
        let read = |name: String| secrets::setting(&name).and_then(|text| text.trim().parse().ok());
        Self {
            rpm: read(format!("SUSI_HARNESS_RPM_{upper}"))
                .and_then(|value: u64| u32::try_from(value).ok()),
            tpm: read(format!("SUSI_HARNESS_TPM_{upper}")),
        }
    }
}

/// The pacing state for every provider.
#[derive(Debug, Default)]
pub struct Pacer {
    lanes: Mutex<BTreeMap<String, Lane>>,
}

impl Pacer {
    /// The process-wide pacer the gateway uses.
    #[must_use]
    pub fn global() -> &'static Self {
        static PACER: OnceLock<Pacer> = OnceLock::new();
        PACER.get_or_init(Self::default)
    }

    /// Reserve a start for one call to `provider` at or after `now_ms`, and
    /// return how many milliseconds the call must wait first. `limits` is
    /// read on the provider's first call when given as `None`.
    pub fn reserve(&self, provider: &str, now_ms: u64, limits: Option<Limits>) -> u64 {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(provider.to_string()).or_default();
        let limits = *lane
            .limits
            .get_or_insert_with(|| limits.unwrap_or_else(|| Limits::configured(provider)));
        let horizon = now_ms.saturating_sub(WINDOW_MS);
        while lane.calls.front().is_some_and(|start| *start < horizon) {
            lane.calls.pop_front();
        }
        while lane.tokens.front().is_some_and(|(at, _)| *at < horizon) {
            lane.tokens.pop_front();
        }
        let mut start = now_ms.max(lane.paused_until);
        if let Some(rpm) = limits.rpm.filter(|rpm| *rpm > 0) {
            let rpm = usize::try_from(rpm).unwrap_or(usize::MAX);
            // The window may hold at most `rpm` starts: this call starts a
            // minute after the start `rpm` places back.
            if lane.calls.len() >= rpm {
                if let Some(oldest) = lane.calls.get(lane.calls.len() - rpm) {
                    start = start.max(oldest.saturating_add(WINDOW_MS));
                }
            }
        }
        if let Some(tpm) = limits.tpm.filter(|tpm| *tpm > 0) {
            // Wait until enough of the last minute's tokens have aged out to
            // bring the total under the cap.
            let mut used: u64 = lane.tokens.iter().map(|(_, tokens)| *tokens).sum();
            for (at, tokens) in &lane.tokens {
                if used < tpm {
                    break;
                }
                used = used.saturating_sub(*tokens);
                start = start.max(at.saturating_add(WINDOW_MS));
            }
        }
        let position = lane.calls.partition_point(|reserved| *reserved <= start);
        lane.calls.insert(position, start);
        start.saturating_sub(now_ms)
    }

    /// Record the tokens a finished call to `provider` used.
    pub fn record_tokens(&self, provider: &str, at_ms: u64, tokens: u64) {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(provider.to_string()).or_default();
        let position = lane.tokens.partition_point(|(seen, _)| *seen <= at_ms);
        lane.tokens.insert(position, (at_ms, tokens));
    }

    /// Hold every call to `provider` until `retry_after_ms` from `now_ms`,
    /// capped at [`MAX_PAUSE_MS`].
    pub fn pause(&self, provider: &str, now_ms: u64, retry_after_ms: u64) {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(provider.to_string()).or_default();
        let until = now_ms.saturating_add(retry_after_ms.min(MAX_PAUSE_MS));
        lane.paused_until = lane.paused_until.max(until);
    }
}

/// A `Retry-After` header value in milliseconds: delay-seconds, as the
/// providers send it. An HTTP date is not parsed and yields `None`.
#[must_use]
pub fn retry_after_ms(value: &str) -> Option<u64> {
    let text = value.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) {
        return None;
    }
    let seconds: u64 = whole.parse().ok()?;
    // Milliseconds from the first three fractional digits; any further
    // non-zero digit rounds up, so the wait is never cut short.
    let mut rest = fraction.bytes();
    let mut millis = 0u64;
    for _ in 0..3 {
        millis = millis * 10 + rest.next().map_or(0, |byte| u64::from(byte - b'0'));
    }
    if rest.any(|byte| byte != b'0') {
        millis += 1;
    }
    Some(seconds.saturating_mul(1000).saturating_add(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(rpm: Option<u32>, tpm: Option<u64>) -> Option<Limits> {
        Some(Limits { rpm, tpm })
    }

    #[test]
    fn an_unlimited_provider_never_waits() {
        let pacer = Pacer::default();
        for at in 0..100 {
            assert_eq!(pacer.reserve("p", at, caps(None, None)), 0);
        }
    }

    #[test]
    fn requests_per_minute_hold_the_next_call_until_a_slot_frees() {
        let pacer = Pacer::default();
        assert_eq!(pacer.reserve("p", 1_000, caps(Some(2), None)), 0);
        assert_eq!(pacer.reserve("p", 2_000, caps(Some(2), None)), 0);
        // The third call in the minute waits for the first to age out.
        assert_eq!(pacer.reserve("p", 3_000, caps(Some(2), None)), 58_000);
        // The fourth waits for the second, and does not jump the third.
        assert_eq!(pacer.reserve("p", 3_000, caps(Some(2), None)), 59_000);
        // Another provider is not held.
        assert_eq!(pacer.reserve("q", 3_000, caps(Some(2), None)), 0);
    }

    #[test]
    fn tokens_per_minute_hold_calls_while_the_window_is_full() {
        let pacer = Pacer::default();
        assert_eq!(pacer.reserve("p", 0, caps(None, Some(1_000))), 0);
        pacer.record_tokens("p", 10_000, 600);
        pacer.record_tokens("p", 20_000, 500);
        // 1,100 tokens in the window: wait until the first 600 age out.
        assert_eq!(pacer.reserve("p", 30_000, caps(None, Some(1_000))), 40_000);
        // Once they have, the next call goes at once.
        assert_eq!(pacer.reserve("p", 71_000, caps(None, Some(1_000))), 0);
    }

    #[test]
    fn retry_after_pauses_a_provider_up_to_the_cap() {
        let pacer = Pacer::default();
        pacer.pause("p", 1_000, 5_000);
        assert_eq!(pacer.reserve("p", 2_000, caps(None, None)), 4_000);
        pacer.pause("p", 10_000, 10_000_000);
        assert_eq!(pacer.reserve("p", 10_000, caps(None, None)), MAX_PAUSE_MS);
        assert_eq!(retry_after_ms("7"), Some(7_000));
        assert_eq!(retry_after_ms(" 1.5 "), Some(1_500));
        assert_eq!(retry_after_ms("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(retry_after_ms("-1"), None);
    }
}
