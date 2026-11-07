//! Scoped lockout recovery for cloud credentials: cooldowns, circuit
//! breaking, and bounded half-open probes, shared by concurrent workers.
//!
//! The tracker sits between eligibility evidence (`cloud_eligibility`) and
//! dispatch (`cloud_intent::select` + real calls). Its job is *rate-limiting
//! retries*, not judging truth: eligibility verdicts say what happened; this
//! module decides when a scoped target may be attempted again.
//!
//! - Temporary conditions (rate limit, outage, quota until reset) get
//!   cooldowns: `Retry-After` is honored, otherwise bounded exponential
//!   backoff with deterministic jitter.
//! - Permanent conditions (invalid credential, insufficient credit, access
//!   denied, unsupported request) never retry on a timer — unfunded accounts
//!   are not re-probed. They lift only on fresh positive evidence.
//! - A circuit opens after `max_consecutive_failures`; when it cools it goes
//!   half-open and allows at most `half_open_probes` concurrent probes —
//!   shared across workers, so a fleet cannot stampede a recovering vendor.
//! - State is persisted per scope so restarts keep cooldowns; a backwards
//!   clock clamps rather than extending lockouts.
//!
//! Scopes: cooldowns apply per scope key — caller chooses granularity
//! (`provider`, `provider:region`, `provider:cred8:model`). Keys are opaque;
//! the scope string never contains key material (use
//! `susi_vendor_models::cloud_eligibility::credential_fingerprint`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use susi_core::model_health::{FailureClass, ModelHealth};
use susi_vendor_models::cloud_eligibility::{EligibilityKind, InferenceResult};

/// Clock injection point: `fn() -> u64` seconds since epoch. Tests pass a
/// controllable counter; production passes [`now_unix`].
pub type Clock = fn() -> u64;

fn default_clock() -> Clock {
    now_unix
}

/// Current wall clock — the default [`Clock`].
#[must_use]
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Tunables for cooldown and circuit behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LockoutPolicy {
    /// First cooldown after a transient failure, ms.
    pub base_cooldown_ms: u64,
    /// Hard cap on any cooldown, ms.
    pub max_cooldown_ms: u64,
    /// Consecutive transient failures before the circuit opens.
    pub max_consecutive_failures: u32,
    /// Concurrent probes allowed while the circuit is half-open — shared
    /// budget across all workers, not per worker.
    pub half_open_probes: u32,
    /// Jitter as a fraction in parts-per-thousand of the computed cooldown
    /// (0–1000). Deterministic per (scope, attempt) — no RNG needed.
    pub jitter_ppt: u32,
}

impl Default for LockoutPolicy {
    fn default() -> Self {
        Self {
            base_cooldown_ms: 1_000,
            max_cooldown_ms: 300_000,
            max_consecutive_failures: 3,
            half_open_probes: 1,
            jitter_ppt: 200,
        }
    }
}

/// Circuit state for one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Circuit {
    /// Normal dispatch.
    Closed,
    /// Cooling down until `until_ms`; no dispatch.
    Open,
    /// Cooldown elapsed; limited probes may run.
    HalfOpen,
}

/// Whether an attempt may proceed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permit {
    /// Dispatch allowed.
    Allowed,
    /// Wait — transient cooldown. `retry_at_ms` is the earliest retry.
    Cooldown { retry_at_ms: u64 },
    /// Circuit open — too many consecutive transient failures.
    CircuitOpen { retry_at_ms: u64 },
    /// Half-open and the probe budget is spent — another worker is probing.
    ProbesExhausted,
    /// Permanently blocked (auth/credit/access) — do not retry on a timer.
    /// Lifts only via fresh positive evidence.
    Permanent { reason: &'static str },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    circuit: Circuit,
    consecutive_failures: u32,
    /// Cooldown end (ms, wall clock as reported by `clock`).
    cooldown_until_ms: u64,
    /// Provider-supplied `Retry-After` floor (ms) — honored over backoff.
    retry_after_until_ms: u64,
    /// Probes currently in flight while half-open (shared across workers).
    probes_in_flight: u32,
    /// Permanent block reason; cleared only by fresh positive evidence.
    permanent: Option<String>,
    /// Typed kind of the permanent block — lets [`LockoutTracker::health`]
    /// report a `FailureClass` without parsing `permanent`'s display text.
    /// `default` keeps files written before this field loadable.
    #[serde(default)]
    permanent_kind: Option<EligibilityKind>,
    /// Kind of the most recent recorded failure — lets `health` classify a
    /// scope correctly when the caller passes `EligibilityKind::Unknown`.
    #[serde(default)]
    last_kind: Option<EligibilityKind>,
    /// Clock time when last updated — detects backwards clock jumps.
    last_seen_ms: u64,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            circuit: Circuit::Closed,
            consecutive_failures: 0,
            cooldown_until_ms: 0,
            retry_after_until_ms: 0,
            probes_in_flight: 0,
            permanent: None,
            permanent_kind: None,
            last_kind: None,
            last_seen_ms: 0,
        }
    }
}

/// Bounded per-scope lockout tracker. Persisted as JSON; holds no secrets.
#[derive(Debug, Serialize, Deserialize)]
pub struct LockoutTracker {
    entries: BTreeMap<String, Entry>,
    cap: usize,
    #[serde(skip, default = "default_clock")]
    clock: Clock,
    #[serde(skip, default)]
    policy: LockoutPolicy,
}

impl Default for LockoutTracker {
    fn default() -> Self {
        Self::new(LockoutPolicy::default(), now_unix)
    }
}

impl LockoutTracker {
    #[must_use]
    pub fn new(policy: LockoutPolicy, clock: Clock) -> Self {
        Self {
            entries: BTreeMap::new(),
            cap: 4096,
            clock,
            policy,
        }
    }

    #[must_use]
    pub fn with_capacity(cap: usize, policy: LockoutPolicy, clock: Clock) -> Self {
        Self {
            entries: BTreeMap::new(),
            cap: cap.max(1),
            clock,
            policy,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Deterministic jitter for (scope, attempt): a cheap multiplicative
    /// hash — stable across processes so concurrent workers compute the
    /// same cooldown for the same evidence.
    fn jitter_ms(&self, scope: &str, attempt: u32) -> u64 {
        let mut h = 0x9e3779b97f4a7c15u64 ^ attempt as u64;
        for b in scope.bytes() {
            h = h.wrapping_mul(0x100000001b3) ^ b as u64;
        }
        h % 1000
    }

    /// Backoff for the n'th consecutive failure, plus jitter, capped.
    fn backoff_ms(&self, scope: &str, failures: u32) -> u64 {
        let shift = failures.saturating_sub(1).min(16);
        let base = self.policy.base_cooldown_ms.saturating_mul(1u64 << shift);
        let jitter = base.saturating_mul(u64::from(self.policy.jitter_ppt.min(1000))) / 1000
            * self.jitter_ms(scope, failures)
            / 1000;
        base.saturating_add(jitter).min(self.policy.max_cooldown_ms)
    }

    fn entry(&mut self, scope: &str, now_ms: u64) -> &mut Entry {
        if !self.entries.contains_key(scope) && self.entries.len() >= self.cap {
            // Evict the stalest entry — never unbounded.
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_seen_ms)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        let e = self.entries.entry(scope.to_string()).or_default();
        // Backwards clock: never let a time jump extend a lockout.
        e.last_seen_ms = e.last_seen_ms.max(now_ms);
        e
    }

    /// Effective "now" in ms, never behind any entry's last observation.
    fn now_ms(&self) -> u64 {
        (self.clock)().saturating_mul(1000)
    }

    /// May a dispatch proceed for `scope` right now? Consults both the
    /// cooldown state and the caller-supplied eligibility verdict (already
    /// resolved from `EligibilityStore`), so permanent vendor facts (dead
    /// key, unfunded account) block without any cooldown bookkeeping.
    #[must_use]
    pub fn permit(&self, scope: &str, verdict: EligibilityKind) -> Permit {
        let now = self.now_ms();
        let Some(e) = self.entries.get(scope) else {
            return Permit::Allowed;
        };
        if e.permanent.is_some() {
            return Permit::Permanent {
                reason: "hard provider rejection (auth/credit/access)",
            };
        }
        // Eligibility already knows it's dead — agree without bookkeeping.
        match verdict {
            EligibilityKind::InvalidCredential
            | EligibilityKind::InsufficientCredit
            | EligibilityKind::AccessDenied
            | EligibilityKind::UnsupportedRequest => {
                return Permit::Permanent {
                    reason: "hard provider rejection (auth/credit/access)",
                };
            }
            EligibilityKind::Usable
            | EligibilityKind::Unknown
            | EligibilityKind::RateLimited
            | EligibilityKind::ServiceUnavailable
            | EligibilityKind::QuotaExhausted => {}
        }
        let until = e.cooldown_until_ms.max(e.retry_after_until_ms);
        match e.circuit {
            Circuit::Closed => Permit::Allowed,
            Circuit::Open => {
                if now >= until {
                    Permit::Allowed // caller transitions via note_attempt
                } else {
                    Permit::CircuitOpen { retry_at_ms: until }
                }
            }
            Circuit::HalfOpen => {
                if now < until {
                    Permit::Cooldown { retry_at_ms: until }
                } else if e.probes_in_flight >= self.policy.half_open_probes {
                    Permit::ProbesExhausted
                } else {
                    Permit::Allowed
                }
            }
        }
    }

    /// Typed health view of one scope — the availability axis, reported in
    /// `susi_core::model_health` terms so dispatch surfaces carry the same
    /// vocabulary everywhere. Mirrors `permit`: verdicts and tracker state
    /// only ever describe *availability*; capability evidence never enters
    /// here. `verdict` is the caller's current eligibility verdict — pass
    /// `EligibilityKind::Unknown` when none is resolved.
    ///
    /// States: `Healthy` (proven usable), `Unhealthy{RateLimited}` (cooling
    /// — the deadline honours `Retry-After`), `Degraded` (timeouts or 5xx,
    /// intermittent), `Unhealthy{ServiceError}` (repeated transient failure
    /// tripped the circuit), `Dead{InvalidCredential|Unsupported}`
    /// (key-dead), `Dead{InsufficientQuota}` (unfunded account),
    /// `Unhealthy{InsufficientQuota}` (out of quota until the reset).
    #[must_use]
    pub fn health(&self, scope: &str, verdict: EligibilityKind) -> ModelHealth {
        let now = self.now_ms();
        let Some(e) = self.entries.get(scope) else {
            return match verdict {
                EligibilityKind::Usable => ModelHealth::Healthy,
                EligibilityKind::InvalidCredential | EligibilityKind::AccessDenied => {
                    ModelHealth::Dead {
                        class: FailureClass::InvalidCredential,
                    }
                }
                EligibilityKind::InsufficientCredit => ModelHealth::Dead {
                    class: FailureClass::InsufficientQuota,
                },
                EligibilityKind::UnsupportedRequest => ModelHealth::Dead {
                    class: FailureClass::Unsupported,
                },
                EligibilityKind::QuotaExhausted => ModelHealth::Unhealthy {
                    class: FailureClass::InsufficientQuota,
                    reason: "out of quota until provider reset".to_string(),
                },
                EligibilityKind::RateLimited => ModelHealth::Unhealthy {
                    class: FailureClass::RateLimited,
                    reason: "rate limited".to_string(),
                },
                EligibilityKind::ServiceUnavailable => ModelHealth::Degraded {
                    reason: "timeouts or provider 5xx".to_string(),
                },
                EligibilityKind::Unknown => ModelHealth::Unknown,
            };
        };
        // Permanent block — tracker entry or the caller's verdict.
        if e.permanent.is_some() {
            return ModelHealth::Dead {
                class: Self::permanent_class(e),
            };
        }
        // The effective failure kind: the caller's verdict when it carries
        // evidence, else the last kind recorded against this scope.
        let kind = if verdict == EligibilityKind::Unknown {
            e.last_kind.unwrap_or(EligibilityKind::Unknown)
        } else {
            verdict
        };
        match kind {
            EligibilityKind::InvalidCredential | EligibilityKind::AccessDenied => {
                return ModelHealth::Dead {
                    class: FailureClass::InvalidCredential,
                };
            }
            EligibilityKind::InsufficientCredit => {
                return ModelHealth::Dead {
                    class: FailureClass::InsufficientQuota,
                };
            }
            EligibilityKind::UnsupportedRequest => {
                return ModelHealth::Dead {
                    class: FailureClass::Unsupported,
                };
            }
            EligibilityKind::Usable
            | EligibilityKind::Unknown
            | EligibilityKind::RateLimited
            | EligibilityKind::QuotaExhausted
            | EligibilityKind::ServiceUnavailable => {}
        }
        let until = e.cooldown_until_ms.max(e.retry_after_until_ms);
        let cooling = now < until;
        if matches!(kind, EligibilityKind::RateLimited) {
            let reason = if cooling {
                format!("cooling until {until} ms (Retry-After honoured)")
            } else {
                "rate limited".to_string()
            };
            return ModelHealth::Unhealthy {
                class: FailureClass::RateLimited,
                reason,
            };
        }
        if matches!(kind, EligibilityKind::QuotaExhausted) {
            let reason = if cooling {
                format!("out of quota until {until} ms")
            } else {
                "out of quota".to_string()
            };
            return ModelHealth::Unhealthy {
                class: FailureClass::InsufficientQuota,
                reason,
            };
        }
        if e.circuit == Circuit::Open && cooling {
            return ModelHealth::Unhealthy {
                class: FailureClass::ServiceError,
                reason: format!("circuit open until {until} ms"),
            };
        }
        if e.consecutive_failures > 0 {
            let reason = if cooling {
                format!("intermittent failures; recovering at {until} ms")
            } else {
                "intermittent failures".to_string()
            };
            return ModelHealth::Degraded { reason };
        }
        match kind {
            EligibilityKind::Usable => ModelHealth::Healthy,
            EligibilityKind::ServiceUnavailable => ModelHealth::Degraded {
                reason: "timeouts or provider 5xx".to_string(),
            },
            EligibilityKind::Unknown
            | EligibilityKind::InvalidCredential
            | EligibilityKind::AccessDenied
            | EligibilityKind::InsufficientCredit
            | EligibilityKind::QuotaExhausted
            | EligibilityKind::RateLimited
            | EligibilityKind::UnsupportedRequest => ModelHealth::Unknown,
        }
    }

    /// Typed class of a recorded permanent block. Reads `permanent_kind`
    /// when present, falling back to the legacy display reason persisted
    /// before that field existed.
    fn permanent_class(e: &Entry) -> FailureClass {
        if let Some(kind) = e.permanent_kind {
            return Self::failure_class(kind);
        }
        let reason = e.permanent.as_deref().unwrap_or("");
        if reason.contains("InsufficientCredit") {
            FailureClass::InsufficientQuota
        } else if reason.contains("Unsupported") {
            FailureClass::Unsupported
        } else {
            FailureClass::InvalidCredential
        }
    }

    /// Map an eligibility verdict to a typed failure class. `Usable` and
    /// `Unknown` carry no failure and are never reached from a recorded
    /// block — they map to `Unknown` defensively.
    fn failure_class(kind: EligibilityKind) -> FailureClass {
        match kind {
            EligibilityKind::RateLimited => FailureClass::RateLimited,
            EligibilityKind::QuotaExhausted | EligibilityKind::InsufficientCredit => {
                FailureClass::InsufficientQuota
            }
            EligibilityKind::InvalidCredential | EligibilityKind::AccessDenied => {
                FailureClass::InvalidCredential
            }
            EligibilityKind::ServiceUnavailable => FailureClass::ServiceError,
            EligibilityKind::UnsupportedRequest => FailureClass::Unsupported,
            EligibilityKind::Usable | EligibilityKind::Unknown => FailureClass::Unknown,
        }
    }

    /// Record an in-flight half-open probe. Call before dispatching when
    /// `permit` allowed; other workers then see `ProbesExhausted`.
    /// Returns false when the probe budget was already spent (race with
    /// another worker) — the caller must not dispatch.
    pub fn acquire_probe(&mut self, scope: &str) -> bool {
        let now = self.now_ms();
        let max_probes = self.policy.half_open_probes;
        let e = self.entry(scope, now);
        if e.circuit == Circuit::HalfOpen && e.probes_in_flight < max_probes {
            e.probes_in_flight += 1;
            return true;
        }
        e.circuit == Circuit::Closed
    }

    /// Mark a probe finished (success or failure is reported separately via
    /// [`Self::record`]); always releases the in-flight slot.
    pub fn release_probe(&mut self, scope: &str) {
        let now = self.now_ms();
        let e = self.entry(scope, now);
        e.probes_in_flight = e.probes_in_flight.saturating_sub(1);
    }

    /// Record a dispatch outcome. Success closes the circuit and clears
    /// every cooldown and permanent flag — fresh positive evidence is the
    /// only way a dead key or unfunded account becomes eligible again.
    /// Failures are classified through `EligibilityKind` semantics:
    /// transient kinds cool down (Retry-After honored, else backoff+jitter),
    /// permanent kinds never re-arm on a timer.
    pub fn record(&mut self, scope: &str, result: &InferenceResult) {
        let now = self.now_ms();
        match result {
            InferenceResult::Success => {
                self.entries.remove(scope); // fresh evidence — full recovery
            }
            InferenceResult::Failed {
                status,
                body_snippet,
                retry_after_secs,
            } => {
                let (kind, _level) =
                    susi_vendor_models::cloud_eligibility::classify_failure(*status, body_snippet);
                self.entry(scope, now).last_kind = Some(kind);
                match kind {
                    EligibilityKind::InvalidCredential
                    | EligibilityKind::InsufficientCredit
                    | EligibilityKind::AccessDenied
                    | EligibilityKind::UnsupportedRequest => {
                        let e = self.entry(scope, now);
                        e.permanent = Some(format!("{kind:?}"));
                        e.permanent_kind = Some(kind);
                        e.probes_in_flight = 0;
                    }
                    EligibilityKind::RateLimited => {
                        let failures = {
                            let e = self.entry(scope, now);
                            e.consecutive_failures = e.consecutive_failures.saturating_add(1);
                            e.probes_in_flight = 0;
                            e.consecutive_failures
                        };
                        let until = retry_after_secs
                            .map(|s| now.saturating_add(s.saturating_mul(1000)))
                            .unwrap_or_else(|| {
                                now.saturating_add(self.backoff_ms(scope, failures))
                            });
                        self.entry(scope, now).retry_after_until_ms = until;
                        self.trip_if_needed(scope, until);
                    }
                    EligibilityKind::ServiceUnavailable | EligibilityKind::Unknown => {
                        let failures = {
                            let e = self.entry(scope, now);
                            e.consecutive_failures = e.consecutive_failures.saturating_add(1);
                            e.probes_in_flight = 0;
                            e.consecutive_failures
                        };
                        let until = now.saturating_add(self.backoff_ms(scope, failures));
                        self.entry(scope, now).cooldown_until_ms = until;
                        self.trip_if_needed(scope, until);
                    }
                    EligibilityKind::QuotaExhausted => {
                        // Recoverable once the reset window passes — the
                        // cooldown doubles as the "when to try again".
                        let failures = {
                            let e = self.entry(scope, now);
                            e.consecutive_failures = e.consecutive_failures.saturating_add(1);
                            e.probes_in_flight = 0;
                            e.consecutive_failures
                        };
                        let until = retry_after_secs
                            .map(|s| now.saturating_add(s.saturating_mul(1000)))
                            .unwrap_or_else(|| {
                                now.saturating_add(self.backoff_ms(scope, failures))
                            });
                        self.entry(scope, now).cooldown_until_ms = until;
                        self.trip_if_needed(scope, until);
                    }
                    EligibilityKind::Usable => {
                        self.entries.remove(scope);
                    }
                }
            }
        }
    }

    fn trip_if_needed(&mut self, scope: &str, until_ms: u64) {
        let max_failures = self.policy.max_consecutive_failures;
        let e = self.entry(scope, until_ms);
        e.circuit = if e.consecutive_failures >= max_failures {
            Circuit::Open
        } else {
            Circuit::HalfOpen
        };
    }

    /// Advance a cooled `Open`/`HalfOpen` circuit when `permit` returned
    /// `Allowed` — transitions Open → HalfOpen so probe accounting begins.
    pub fn note_attempt(&mut self, scope: &str) {
        let now = self.now_ms();
        let e = self.entry(scope, now);
        if e.circuit == Circuit::Open && now >= e.cooldown_until_ms.max(e.retry_after_until_ms) {
            e.circuit = Circuit::HalfOpen;
        }
    }

    /// Lift a permanent block on fresh positive evidence (a live success or
    /// a newly valid key observed elsewhere). Without this call permanent
    /// entries never expire — unfunded accounts are never re-probed.
    pub fn lift_if_fresh_evidence(&mut self, scope: &str, usable: bool) {
        if usable {
            self.entries.remove(scope);
        }
    }

    /// Drop expired cooldowns (Open→HalfOpen when due); permanent entries
    /// stay until fresh evidence lifts them.
    pub fn tick(&mut self) {
        let now = self.now_ms();
        for e in self.entries.values_mut() {
            if e.circuit == Circuit::Open && now >= e.cooldown_until_ms.max(e.retry_after_until_ms)
            {
                e.circuit = Circuit::HalfOpen;
            }
        }
    }

    /// Persist as JSON — contains scope strings only, never key material.
    pub fn save(&self, path: &Path) -> susi_error::EaiResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                susi_error::EaiError::io(format!("create {}: {e}", parent.display()))
            })?;
        }
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| susi_error::EaiError::config(format!("encode lockouts: {e}")))?;
        std::fs::write(path, body)
            .map_err(|e| susi_error::EaiError::io(format!("write {}: {e}", path.display())))
    }

    /// Load persisted state; `clock` and `policy` are injected again.
    /// Missing/corrupt files start fresh (a lost cooldown degrades to one
    /// extra attempt — the eligibility ledger still gates hard blocks).
    #[must_use]
    pub fn load(path: &Path, policy: LockoutPolicy, clock: Clock) -> Self {
        let mut t: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|b| serde_json::from_str(&b).ok())
            .unwrap_or_else(|| Self::new(policy, clock));
        t.clock = clock;
        t.policy = policy;
        t
    }
}

fn default_path() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("cloud_lockouts.json")
}

/// Process-global tracker for dispatch paths — loaded once, mutated via
/// `update_global`, persisted best-effort.
static GLOBAL: std::sync::OnceLock<parking_lot::Mutex<LockoutTracker>> = std::sync::OnceLock::new();

fn global() -> &'static parking_lot::Mutex<LockoutTracker> {
    GLOBAL.get_or_init(|| {
        parking_lot::Mutex::new(LockoutTracker::load(
            &default_path(),
            LockoutPolicy::default(),
            now_unix,
        ))
    })
}

/// Mutate the shared tracker and persist it. Never fails loudly: lockout
/// bookkeeping must not turn a dispatch into an error.
pub fn update_global(f: impl FnOnce(&mut LockoutTracker)) {
    let mut t = global().lock();
    f(&mut t);
    let _ = t.save(&default_path());
}

/// Dispatch-path helpers on the shared tracker: `permit` before an attempt,
/// `record` after. Scoped by `provider:cred8:model` so one credential's
/// throttle never cools an unrelated model or account.
pub fn permit_global(scope: &str, verdict: EligibilityKind) -> Permit {
    global().lock().permit(scope, verdict)
}
/// See [`update_global`] — convenience wrapper for the dispatch path.
pub fn record_global(scope: &str, result: &InferenceResult) {
    update_global(|t| t.record(scope, result));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // Per-test-thread clock — parallel tests never race each other's time.
    thread_local! {
        static TEST_NOW: Cell<u64> = const { Cell::new(1_700_000_000) };
    }

    fn test_clock() -> u64 {
        TEST_NOW.get()
    }

    fn set_now(secs: u64) {
        TEST_NOW.set(secs);
    }

    fn tracker() -> LockoutTracker {
        LockoutTracker::new(
            LockoutPolicy {
                base_cooldown_ms: 1_000,
                max_cooldown_ms: 60_000,
                max_consecutive_failures: 3,
                half_open_probes: 2,
                jitter_ppt: 0, // deterministic backoff for boundary asserts
            },
            test_clock,
        )
    }

    fn fail(status: u16, body: &str) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.into(),
            retry_after_secs: None,
        }
    }

    fn fail_ra(status: u16, body: &str, ra: u64) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.into(),
            retry_after_secs: Some(ra),
        }
    }

    #[test]
    fn cloud_lockout_recovery_backoff_is_bounded_exponential() {
        set_now(1_700_000_000);
        let mut t = tracker();
        for _ in 0..3 {
            t.record("p:c:m", &fail(503, "down"));
        }
        // 3 failures → circuit Open; cooldown = min(1s*4, 60s) = 4s past now.
        match t.permit("p:c:m", EligibilityKind::ServiceUnavailable) {
            Permit::CircuitOpen { retry_at_ms } => {
                assert_eq!(retry_at_ms, 1_700_000_000_000 + 4_000);
            }
            other => panic!("expected open circuit, got {other:?}"),
        }
        // After cooldown the circuit admits a probe (half-open).
        set_now(1_700_000_005);
        assert_eq!(
            t.permit("p:c:m", EligibilityKind::ServiceUnavailable),
            Permit::Allowed
        );
        t.note_attempt("p:c:m");
        assert!(t.acquire_probe("p:c:m"));
        // Success closes everything.
        t.release_probe("p:c:m");
        t.record("p:c:m", &InferenceResult::Success);
        assert_eq!(t.permit("p:c:m", EligibilityKind::Unknown), Permit::Allowed);
        assert!(t.is_empty());
    }

    #[test]
    fn cloud_lockout_recovery_retry_after_overrides_backoff() {
        set_now(1_700_000_000);
        let mut t = tracker();
        t.record("p:c:m", &fail_ra(429, "slow down", 30));
        match t.permit("p:c:m", EligibilityKind::RateLimited) {
            Permit::Cooldown { retry_at_ms } | Permit::CircuitOpen { retry_at_ms } => {
                assert_eq!(retry_at_ms, 1_700_000_000_000 + 30_000);
            }
            other => panic!("expected cooldown honoring Retry-After, got {other:?}"),
        }
        set_now(1_700_000_029);
        assert_ne!(
            t.permit("p:c:m", EligibilityKind::RateLimited),
            Permit::Allowed
        );
        set_now(1_700_000_030);
        assert_eq!(
            t.permit("p:c:m", EligibilityKind::RateLimited),
            Permit::Allowed
        );
    }

    #[test]
    fn cloud_lockout_recovery_permanent_blocks_never_rearm() {
        set_now(1_700_000_000);
        let mut t = tracker();
        // Unfunded account — never retried on a timer.
        t.record("p:cbroke:m", &fail(402, "insufficient credit"));
        assert!(matches!(
            t.permit("p:cbroke:m", EligibilityKind::InsufficientCredit),
            Permit::Permanent { .. }
        ));
        // Time passing must not heal it.
        set_now(1_800_000_000);
        t.tick();
        assert!(matches!(
            t.permit("p:cbroke:m", EligibilityKind::InsufficientCredit),
            Permit::Permanent { .. }
        ));
        // Only fresh positive evidence lifts it.
        t.lift_if_fresh_evidence("p:cbroke:m", true);
        assert_eq!(
            t.permit("p:cbroke:m", EligibilityKind::Unknown),
            Permit::Allowed
        );
        // Same for a dead key.
        t.record("p:cdead:m", &fail(401, "invalid api key"));
        set_now(1_900_000_000);
        t.tick();
        assert!(matches!(
            t.permit("p:cdead:m", EligibilityKind::InvalidCredential),
            Permit::Permanent { .. }
        ));
    }

    #[test]
    fn cloud_lockout_recovery_quota_resets_at_window() {
        set_now(1_700_000_000);
        let mut t = tracker();
        // Provider says quota resets in 60s (carried via retry_after).
        t.record("p:cquota:m", &fail_ra(429, "quota exceeded", 60));
        set_now(1_700_000_050);
        assert_ne!(
            t.permit("p:cquota:m", EligibilityKind::QuotaExhausted),
            Permit::Allowed
        );
        set_now(1_700_000_061);
        assert_eq!(
            t.permit("p:cquota:m", EligibilityKind::QuotaExhausted),
            Permit::Allowed
        );
    }

    #[test]
    fn cloud_lockout_recovery_half_open_probe_budget_is_shared() {
        set_now(1_700_000_000);
        let mut t = tracker();
        // One transient failure → half-open after backoff, 2 probe slots.
        t.record("p:c:m", &fail(503, "blip"));
        set_now(1_700_000_002); // past the ~1s cooldown
        t.tick();
        // Two workers acquire the shared probe budget; the third waits.
        assert!(t.acquire_probe("p:c:m"));
        assert!(t.acquire_probe("p:c:m"));
        assert!(!t.acquire_probe("p:c:m"));
        assert_eq!(
            t.permit("p:c:m", EligibilityKind::ServiceUnavailable),
            Permit::ProbesExhausted
        );
        // One probe fails → escalation re-cools; workers still blocked.
        t.release_probe("p:c:m");
        t.record("p:c:m", &fail(503, "still down"));
        assert!(
            !t.acquire_probe("p:c:m")
                || t.permit("p:c:m", EligibilityKind::ServiceUnavailable) != Permit::Allowed
        );
    }

    #[test]
    fn cloud_lockout_recovery_persists_across_restart() {
        set_now(1_700_000_000);
        let dir = std::env::temp_dir().join(format!("susi-lock-{}", std::process::id()));
        let path = dir.join("lock.json");
        let mut t = tracker();
        t.record("p:c:m", &fail(402, "insufficient credit"));
        t.record("p:c2:m", &fail_ra(429, "throttled", 30));
        t.save(&path).unwrap();
        let loaded = LockoutTracker::load(&path, tracker().policy, test_clock);
        assert!(matches!(
            loaded.permit("p:c:m", EligibilityKind::InsufficientCredit),
            Permit::Permanent { .. }
        ));
        assert!(!matches!(
            loaded.permit("p:c2:m", EligibilityKind::RateLimited),
            Permit::Allowed
        ));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("sk-"), "persisted state leaked key material");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_lockout_recovery_backwards_clock_never_extends() {
        set_now(1_700_000_100);
        let mut t = tracker();
        t.record("p:c:m", &fail_ra(429, "retry", 10));
        // Clock jumps backwards — retry deadline must not move further out.
        set_now(1_700_000_050);
        match t.permit("p:c:m", EligibilityKind::RateLimited) {
            Permit::Cooldown { retry_at_ms } | Permit::CircuitOpen { retry_at_ms } => {
                assert_eq!(retry_at_ms, 1_700_000_110_000);
            }
            other => panic!("clock skew should not have cleared the cooldown: {other:?}"),
        }
    }

    #[test]
    fn cloud_lockout_recovery_fresh_success_supersedes_cooldown() {
        set_now(1_700_000_000);
        let mut t = tracker();
        t.record("p:c:m", &fail(503, "down"));
        // A success recorded by any worker clears the cooldown immediately.
        t.record("p:c:m", &InferenceResult::Success);
        assert_eq!(t.permit("p:c:m", EligibilityKind::Unknown), Permit::Allowed);
    }

    #[test]
    fn cloud_lockout_recovery_is_bounded() {
        set_now(1_700_000_000);
        let mut t = LockoutTracker::with_capacity(4, LockoutPolicy::default(), test_clock);
        for i in 0..10u64 {
            t.record(&format!("p:c{i}:m"), &fail(503, "down"));
        }
        assert!(t.len() <= 4);
    }
}
