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

impl Lane {
    /// The earliest start the caps allow a call that arrives at `now_ms`,
    /// once the window has moved on to it. `limits` is read on the lane's
    /// first call when given as `None`.
    fn next_start(&mut self, provider: &str, now_ms: u64, limits: Option<Limits>) -> u64 {
        let limits = *self
            .limits
            .get_or_insert_with(|| limits.unwrap_or_else(|| Limits::configured(provider)));
        let horizon = now_ms.saturating_sub(WINDOW_MS);
        while self.calls.front().is_some_and(|start| *start < horizon) {
            self.calls.pop_front();
        }
        while self.tokens.front().is_some_and(|(at, _)| *at < horizon) {
            self.tokens.pop_front();
        }
        let mut start = now_ms.max(self.paused_until);
        if let Some(rpm) = limits.rpm.filter(|rpm| *rpm > 0) {
            let rpm = usize::try_from(rpm).unwrap_or(usize::MAX);
            // The window may hold at most `rpm` starts: this call starts a
            // minute after the start `rpm` places back.
            if self.calls.len() >= rpm {
                if let Some(oldest) = self.calls.get(self.calls.len() - rpm) {
                    start = start.max(oldest.saturating_add(WINDOW_MS));
                }
            }
        }
        if let Some(tpm) = limits.tpm.filter(|tpm| *tpm > 0) {
            // Wait until enough of the last minute's tokens have aged out to
            // bring the total under the cap.
            let mut used: u64 = self.tokens.iter().map(|(_, tokens)| *tokens).sum();
            for (at, tokens) in &self.tokens {
                if used < tpm {
                    break;
                }
                used = used.saturating_sub(*tokens);
                start = start.max(at.saturating_add(WINDOW_MS));
            }
        }
        start
    }

    /// Count `start` against the window.
    fn take(&mut self, start: u64) {
        let position = self.calls.partition_point(|reserved| *reserved <= start);
        self.calls.insert(position, start);
    }
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
        // The name `susi-harness env` advertises (fleet::environment): every
        // character a variable name cannot hold is written as '_', so an
        // overlay id like `acme.v2` is read from SUSI_HARNESS_RPM_ACME_V2.
        let upper = crate::balance::env_suffix(provider);
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
    ///
    /// The slot is kept even if the call never goes; a caller that can be
    /// abandoned while it waits takes a [`Pacer::hold`] instead.
    pub fn reserve(&self, provider: &str, now_ms: u64, limits: Option<Limits>) -> u64 {
        let reservation = self.hold(provider, now_ms, limits);
        let wait = reservation.wait_ms();
        reservation.keep();
        wait
    }

    /// Reserve a start as [`Pacer::reserve`] does, as a [`Reservation`] that
    /// gives the slot back when it is dropped without [`Reservation::keep`].
    pub fn hold(&self, provider: &str, now_ms: u64, limits: Option<Limits>) -> Reservation<'_> {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(provider.to_string()).or_default();
        let start = lane.next_start(provider, now_ms, limits);
        lane.take(start);
        self.reservation(provider, start, now_ms)
    }

    /// Reserve a start as [`Pacer::hold`] does, unless the call would wait
    /// longer than `max_wait_ms` for it: then nothing is reserved, and the
    /// error is the wait the call would have had.
    ///
    /// Giving back the slots of abandoned waiters does not keep an overloaded
    /// provider serving. Every new call still queues behind the live waiters,
    /// so under load that outruns the limit the queue grows past every
    /// client's timeout, and from then on no call is served while the load
    /// lasts: one arrival every 3 s against 10 a minute, each client giving up
    /// after 120 s, served 50 calls in the first ten minutes and none in the
    /// fifty after. Turned away at once, a caller backs off instead, and the
    /// calls the queue does take each start while their client still waits.
    ///
    /// # Errors
    ///
    /// Returns the wait in milliseconds when it is longer than `max_wait_ms`.
    pub fn hold_within(
        &self,
        provider: &str,
        now_ms: u64,
        limits: Option<Limits>,
        max_wait_ms: u64,
    ) -> Result<Reservation<'_>, u64> {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(provider.to_string()).or_default();
        let start = lane.next_start(provider, now_ms, limits);
        let wait_ms = start.saturating_sub(now_ms);
        if wait_ms > max_wait_ms {
            return Err(wait_ms);
        }
        lane.take(start);
        Ok(self.reservation(provider, start, now_ms))
    }

    fn reservation(&self, provider: &str, start: u64, now_ms: u64) -> Reservation<'_> {
        Reservation {
            pacer: self,
            provider: provider.to_string(),
            start,
            wait_ms: start.saturating_sub(now_ms),
            kept: false,
        }
    }

    /// Give back one reserved start that was never used.
    fn release(&self, provider: &str, start: u64) {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(lane) = lanes.get_mut(provider) {
            let position = lane.calls.partition_point(|reserved| *reserved < start);
            if lane.calls.get(position) == Some(&start) {
                lane.calls.remove(position);
            }
        }
    }

    /// How many starts `provider` holds in (or reserved for) the window.
    #[cfg(test)]
    pub(crate) fn reserved(&self, provider: &str) -> usize {
        let lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        lanes.get(provider).map_or(0, |lane| lane.calls.len())
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

/// One call's reserved start, given back unless the call goes ahead.
///
/// A call waits for its slot inside its request. When the client gives up
/// meanwhile (its own timeout, a killed worker), the server drops that wait
/// and this guard with it, and the slot is free again: kept, it counted
/// against the provider's minute for a call nobody made. That alone does not
/// keep an overloaded provider serving; [`Pacer::hold_within`] bounds the
/// queue for that.
#[must_use = "a reservation dropped without `keep` gives its slot back"]
#[derive(Debug)]
pub struct Reservation<'a> {
    pacer: &'a Pacer,
    provider: String,
    start: u64,
    wait_ms: u64,
    kept: bool,
}

impl Reservation<'_> {
    /// How many milliseconds the call must wait before it starts.
    #[must_use]
    pub fn wait_ms(&self) -> u64 {
        self.wait_ms
    }

    /// The call is going ahead: its slot stays counted.
    pub fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.kept {
            self.pacer.release(&self.provider, self.start);
        }
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
    fn a_reservation_dropped_unkept_gives_its_slot_back() {
        let pacer = Pacer::default();
        assert_eq!(pacer.reserve("p", 0, caps(Some(1), None)), 0);
        let abandoned = pacer.hold("p", 1_000, caps(Some(1), None));
        assert_eq!(abandoned.wait_ms(), 59_000);
        drop(abandoned);
        assert_eq!(pacer.reserved("p"), 1);
        // The next call takes the freed slot, not the one after it.
        let kept = pacer.hold("p", 2_000, caps(Some(1), None));
        assert_eq!(kept.wait_ms(), 58_000);
        kept.keep();
        assert_eq!(pacer.reserved("p"), 2, "a kept slot stays counted");
        assert_eq!(pacer.reserve("p", 2_000, caps(Some(1), None)), 118_000);
    }

    #[test]
    fn a_call_that_would_wait_past_the_cap_is_refused_and_reserves_nothing() {
        let pacer = Pacer::default();
        assert_eq!(pacer.reserve("p", 0, caps(Some(1), None)), 0);
        let refused = pacer.hold_within("p", 1_000, caps(Some(1), None), 30_000);
        assert_eq!(refused.err(), Some(59_000), "the error is the wait");
        assert_eq!(pacer.reserved("p"), 1, "a refused call holds no slot");
        let within = pacer
            .hold_within("p", 1_000, caps(Some(1), None), 59_000)
            .unwrap();
        assert_eq!(within.wait_ms(), 59_000);
        within.keep();
        assert_eq!(pacer.reserved("p"), 2);
    }

    /// One arrival every 3 s for an hour against 10 calls a minute, each
    /// client giving up 120 s after it asked, with the queue bounded at `cap`
    /// as the gateway bounds it: the calls served in each ten minutes.
    fn served_under_overload(cap: u64) -> [u32; 6] {
        const TIMEOUT_MS: u64 = 120_000;
        let pacer = Pacer::default();
        let limits = caps(Some(10), None);
        // (start, the client's deadline, its reservation)
        let mut waiting: Vec<(u64, u64, Reservation<'_>)> = Vec::new();
        let mut served = [0u32; 6];
        for now in (0..3_600_000u64).step_by(3_000) {
            // A waiter's slot comes up, or its client gives up, whichever is
            // first; an abandoned waiter's slot goes back.
            for (start, deadline, reservation) in std::mem::take(&mut waiting) {
                if start.min(deadline) > now {
                    waiting.push((start, deadline, reservation));
                } else if start <= deadline {
                    reservation.keep();
                    served[usize::try_from(start / 600_000).unwrap()] += 1;
                } else {
                    drop(reservation);
                }
            }
            // A call turned away holds nothing and backs off.
            if let Ok(reservation) = pacer.hold_within("p", now, limits, cap) {
                waiting.push((now + reservation.wait_ms(), now + TIMEOUT_MS, reservation));
            }
        }
        served
    }

    /// Arrivals faster than the limit, each client giving up after 120 s.
    /// Queued with `hold` alone, abandoned slots given back, this served
    /// [50, 0, 0, 0, 0, 0]: the queue ran past every client's timeout. Bounded
    /// at the clients' timeout, the provider is used at its limit for as
    /// long as the load lasts.
    #[test]
    fn sustained_overload_is_served_at_the_limit_when_the_queue_is_bounded() {
        let served = served_under_overload(120_000);
        for (window, calls) in served.iter().enumerate() {
            assert!(
                (90..=100).contains(calls),
                "ten minutes #{window} served {calls} of 100: {served:?}"
            );
        }
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
