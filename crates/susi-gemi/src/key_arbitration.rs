//! Process-wide per-key rate + concurrency arbitration (T-DEEPSEEK-116).
//!
//! Every cloud `provider.generate` call in this crate admits through the
//! shared [`KeyArbiter`] before the request is issued. The arbiter enforces
//! three bounds per scope (a scope is the registered provider name — each
//! registration carries one credential, so the name *is* the key pool):
//!
//! - `per_key_concurrent` — how many requests may be in flight against one
//!   key at once;
//! - `per_key_requests`/`window_secs` — a sliding request-rate window per
//!   key, so a burst of parallel missions does not all race the provider's
//!   own rate limit;
//! - `global_concurrent` — a ceiling across every key, so delegated fan-out
//!   cannot crowd out the whole process.
//!
//! Denial is non-blocking: the cascade skips the saturated key and tries
//! the next provider, the same way it skips a provider inside its
//! post-failure cooldown. The arbiter is process-global (a `static`), so it
//! is shared by every agent, mission, and delegated worker running in this
//! daemon process. Cross-process fan-out (external agent CLIs) arbitrates
//! in their own process; provider-side limits remain the backstop there.
//!
//! Operator overrides (`SUSI_KEY_ARBITRATION_*` env) follow the
//! `capacity_admission` convention: explicit escapes, never defaults.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Limits enforced by the arbiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArbiterLimits {
    /// Max concurrent in-flight requests against one key scope.
    pub per_key_concurrent: u32,
    /// Max requests per key scope per `window_secs`.
    pub per_key_requests: u32,
    /// Sliding rate window length in seconds.
    pub window_secs: u64,
    /// Max concurrent in-flight requests across every scope.
    pub global_concurrent: u32,
}

impl Default for ArbiterLimits {
    fn default() -> Self {
        Self {
            per_key_concurrent: 4,
            per_key_requests: 60,
            window_secs: 60,
            global_concurrent: 32,
        }
    }
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
}

/// Environment overrides: `SUSI_KEY_ARBITRATION_CONCURRENT`,
/// `SUSI_KEY_ARBITRATION_REQUESTS`, `SUSI_KEY_ARBITRATION_WINDOW_SECS`,
/// `SUSI_KEY_ARBITRATION_GLOBAL`.
pub fn limits_from_env() -> ArbiterLimits {
    let mut limits = ArbiterLimits::default();
    if let Some(v) = env_u32("SUSI_KEY_ARBITRATION_CONCURRENT") {
        limits.per_key_concurrent = v;
    }
    if let Some(v) = env_u32("SUSI_KEY_ARBITRATION_REQUESTS") {
        limits.per_key_requests = v;
    }
    if let Some(v) = env_u32("SUSI_KEY_ARBITRATION_WINDOW_SECS") {
        limits.window_secs = u64::from(v);
    }
    if let Some(v) = env_u32("SUSI_KEY_ARBITRATION_GLOBAL") {
        limits.global_concurrent = v;
    }
    limits
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Default)]
struct KeyWindow {
    in_flight: u32,
    window_start: u64,
    used: u32,
}

/// Why a scope was refused — surfaced in the failover detail line so an
/// operator sees "saturated" rather than a silent skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// The key's in-flight requests hit `per_key_concurrent`.
    Concurrency,
    /// The key's requests in the current window hit `per_key_requests`.
    RateWindow,
    /// In-flight requests across all keys hit `global_concurrent`.
    Global,
}

impl Denial {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Concurrency => "key concurrency saturated",
            Self::RateWindow => "key rate window exhausted",
            Self::Global => "global concurrency saturated",
        }
    }
}

/// Per-key + global request admission. Cloning shares the same inner state
/// so a mission can hand the arbiter to delegated workers.
#[derive(Debug)]
pub struct KeyArbiter {
    limits: ArbiterLimits,
    inner: Mutex<KeyArbiterInner>,
}

#[derive(Debug, Default)]
struct KeyArbiterInner {
    keys: HashMap<String, KeyWindow>,
    global_in_flight: u32,
}

impl KeyArbiter {
    pub fn new(limits: ArbiterLimits) -> Self {
        Self {
            limits,
            inner: Mutex::new(KeyArbiterInner::default()),
        }
    }

    pub fn limits(&self) -> ArbiterLimits {
        self.limits
    }

    /// Try to admit one request against `scope`. On success the caller
    /// holds a [`KeyPermit`]; dropping it releases the concurrency slot.
    /// The rate window counts the attempt at acquire time.
    pub fn try_acquire(&self, scope: &str) -> Result<KeyPermit<'_>, Denial> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.global_in_flight >= self.limits.global_concurrent {
            return Err(Denial::Global);
        }
        let now = now_unix();
        let key = inner.keys.entry(scope.to_string()).or_default();
        if key.in_flight >= self.limits.per_key_concurrent {
            return Err(Denial::Concurrency);
        }
        if now.saturating_sub(key.window_start) >= self.limits.window_secs {
            key.window_start = now;
            key.used = 0;
        }
        if key.used >= self.limits.per_key_requests {
            return Err(Denial::RateWindow);
        }
        key.used = key.used.saturating_add(1);
        key.in_flight = key.in_flight.saturating_add(1);
        inner.global_in_flight = inner.global_in_flight.saturating_add(1);
        Ok(KeyPermit {
            arbiter: self,
            scope: scope.to_string(),
        })
    }

    fn release(&self, scope: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = inner.keys.get_mut(scope) {
            key.in_flight = key.in_flight.saturating_sub(1);
        }
        inner.global_in_flight = inner.global_in_flight.saturating_sub(1);
    }

    /// Snapshot of one scope for observability/tests.
    pub fn scope_status(&self, scope: &str) -> ScopeStatus {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let key = inner.keys.entry(scope.to_string()).or_default();
        if now.saturating_sub(key.window_start) >= self.limits.window_secs {
            key.window_start = now;
            key.used = 0;
        }
        ScopeStatus {
            in_flight: key.in_flight,
            window_used: key.used,
            global_in_flight: inner.global_in_flight,
        }
    }
}

/// Observable state of one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeStatus {
    pub in_flight: u32,
    pub window_used: u32,
    pub global_in_flight: u32,
}

/// An admission ticket. Dropping it releases the concurrency slots; the
/// rate-window consumption is intentionally retained (the attempt counted
/// against the provider's rate budget even when it failed fast).
pub struct KeyPermit<'a> {
    arbiter: &'a KeyArbiter,
    scope: String,
}

impl Drop for KeyPermit<'_> {
    fn drop(&mut self) {
        self.arbiter.release(&self.scope);
    }
}

/// The process-wide arbiter shared by every agent and delegated worker in
/// this daemon. Limits resolve once from `SUSI_KEY_ARBITRATION_*`.
pub fn arbiter() -> &'static KeyArbiter {
    static ARBITER: OnceLock<KeyArbiter> = OnceLock::new();
    ARBITER.get_or_init(|| KeyArbiter::new(limits_from_env()))
}

/// Admit one request against `scope` on the shared arbiter.
pub fn try_acquire(scope: &str) -> Result<KeyPermit<'static>, Denial> {
    arbiter().try_acquire(scope)
}

/// Scope snapshot on the shared arbiter (observability).
pub fn scope_status(scope: &str) -> ScopeStatus {
    arbiter().scope_status(scope)
}
