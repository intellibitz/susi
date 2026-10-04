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
//!
//! Long-horizon quota windows (T-DEEPSEEK-119, VC-202-021) sit beside the
//! short rate window: `SUSI_KEY_QUOTA_DAILY`, `SUSI_KEY_QUOTA_WEEKLY` and
//! `SUSI_KEY_QUOTA_ROLLING=<secs>:<requests>` declare a provider plan's
//! allowance per key. An exhausted window refuses admission with its reset
//! time, and [`KeyArbiter::quota_headroom`] exposes the remaining allowance
//! so routing treats a nearly-spent cap as scarce rather than free.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// A long-horizon quota window: how a provider plan's allowance resets.
/// Distinct from the sliding rate window — these model real plan limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// Resets `period_secs` after the window was opened.
    Rolling { period_secs: u64 },
    /// Resets at the next UTC midnight.
    DailyUtc,
    /// Resets at the next Monday 00:00 UTC.
    WeeklyUtc,
}

impl WindowKind {
    /// The reset instant for a window opened at `now`.
    fn reset_after(self, now: u64) -> u64 {
        const DAY: u64 = 86_400;
        match self {
            Self::Rolling { period_secs } => now.saturating_add(period_secs),
            Self::DailyUtc => (now / DAY + 1) * DAY,
            Self::WeeklyUtc => {
                // Day 0 (1970-01-01) was a Thursday = index 3 in a Mon=0 week.
                let dow = ((now / DAY) + 3) % 7;
                let mut days = (7 - dow) % 7;
                if days == 0 {
                    days = 7;
                }
                (now / DAY + days) * DAY
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rolling { .. } => "rolling",
            Self::DailyUtc => "daily",
            Self::WeeklyUtc => "weekly",
        }
    }
}

/// One declared quota window: `allowance` requests per `kind` period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaWindowSpec {
    pub kind: WindowKind,
    pub allowance: u64,
}

/// Limits enforced by the arbiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArbiterLimits {
    /// Max concurrent in-flight requests against one key scope.
    pub per_key_concurrent: u32,
    /// Max requests per key scope per `window_secs`.
    pub per_key_requests: u32,
    /// Sliding rate window length in seconds.
    pub window_secs: u64,
    /// Max concurrent in-flight requests across every scope.
    pub global_concurrent: u32,
    /// Long-horizon quota windows per key scope (empty: no plan cap).
    pub quota: Vec<QuotaWindowSpec>,
}

impl Default for ArbiterLimits {
    fn default() -> Self {
        Self {
            per_key_concurrent: 4,
            per_key_requests: 60,
            window_secs: 60,
            global_concurrent: 32,
            quota: Vec::new(),
        }
    }
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
}

fn quota_env(key: &str, kind: WindowKind) -> Option<QuotaWindowSpec> {
    env_u32(key).map(|allowance| QuotaWindowSpec {
        kind,
        allowance: u64::from(allowance),
    })
}

/// Environment overrides: `SUSI_KEY_ARBITRATION_CONCURRENT`,
/// `SUSI_KEY_ARBITRATION_REQUESTS`, `SUSI_KEY_ARBITRATION_WINDOW_SECS`,
/// `SUSI_KEY_ARBITRATION_GLOBAL`; quota windows via `SUSI_KEY_QUOTA_DAILY`,
/// `SUSI_KEY_QUOTA_WEEKLY`, `SUSI_KEY_QUOTA_ROLLING=<secs>:<requests>`.
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
    if let Some(spec) = quota_env("SUSI_KEY_QUOTA_DAILY", WindowKind::DailyUtc) {
        limits.quota.push(spec);
    }
    if let Some(spec) = quota_env("SUSI_KEY_QUOTA_WEEKLY", WindowKind::WeeklyUtc) {
        limits.quota.push(spec);
    }
    if let Ok(v) = std::env::var("SUSI_KEY_QUOTA_ROLLING") {
        if let Some((secs, requests)) = v.split_once(':') {
            if let (Ok(period_secs), Ok(allowance)) = (secs.parse::<u64>(), requests.parse::<u64>())
            {
                if period_secs > 0 && allowance > 0 {
                    limits.quota.push(QuotaWindowSpec {
                        kind: WindowKind::Rolling { period_secs },
                        allowance,
                    });
                }
            }
        }
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
    /// Long-horizon quota counters, one per declared `QuotaWindowSpec`.
    quota_windows: Vec<QuotaWindow>,
}

#[derive(Debug)]
struct QuotaWindow {
    spec: QuotaWindowSpec,
    used: u64,
    reset_unix: u64,
}

impl QuotaWindow {
    fn new(spec: QuotaWindowSpec, now: u64) -> Self {
        Self {
            spec,
            used: 0,
            reset_unix: spec.kind.reset_after(now),
        }
    }

    /// Roll the window forward if `now` is past its reset instant.
    fn roll(&mut self, now: u64) {
        while now >= self.reset_unix {
            self.used = 0;
            self.reset_unix = self.spec.kind.reset_after(self.reset_unix);
        }
    }
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
    /// A declared quota window spent its allowance; refuses until reset.
    Quota { reset_unix: u64 },
}

impl Denial {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Concurrency => "key concurrency saturated",
            Self::RateWindow => "key rate window exhausted",
            Self::Global => "global concurrency saturated",
            Self::Quota { .. } => "key quota window exhausted",
        }
    }
}

/// Per-key + global request admission. Cloning shares the same inner state
/// so a mission can hand the arbiter to delegated workers.
pub struct KeyArbiter {
    limits: ArbiterLimits,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    inner: Mutex<KeyArbiterInner>,
}

#[derive(Debug, Default)]
struct KeyArbiterInner {
    keys: HashMap<String, KeyWindow>,
    global_in_flight: u32,
}

impl KeyArbiter {
    pub fn new(limits: ArbiterLimits) -> Self {
        Self::with_clock(limits, Arc::new(now_unix))
    }

    /// Arbiter with an injected clock — tests drive window resets
    /// deterministically instead of sleeping for real hours.
    pub fn with_clock(limits: ArbiterLimits, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            limits,
            clock,
            inner: Mutex::new(KeyArbiterInner::default()),
        }
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    pub fn limits(&self) -> ArbiterLimits {
        self.limits.clone()
    }

    /// Try to admit one request against `scope`. On success the caller
    /// holds a [`KeyPermit`]; dropping it releases the concurrency slot.
    /// The rate window and every quota window count the attempt at
    /// acquire time.
    pub fn try_acquire(&self, scope: &str) -> Result<KeyPermit<'_>, Denial> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.global_in_flight >= self.limits.global_concurrent {
            return Err(Denial::Global);
        }
        let now = self.now();
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
        if key.quota_windows.len() != self.limits.quota.len() {
            key.quota_windows = self
                .limits
                .quota
                .iter()
                .map(|spec| QuotaWindow::new(*spec, now))
                .collect();
        }
        for window in &mut key.quota_windows {
            window.roll(now);
            if window.used >= window.spec.allowance {
                return Err(Denial::Quota {
                    reset_unix: window.reset_unix,
                });
            }
        }
        key.used = key.used.saturating_add(1);
        for window in &mut key.quota_windows {
            window.used = window.used.saturating_add(1);
        }
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
        let now = self.now();
        let global_in_flight = inner.global_in_flight;
        let key = inner.keys.entry(scope.to_string()).or_default();
        if now.saturating_sub(key.window_start) >= self.limits.window_secs {
            key.window_start = now;
            key.used = 0;
        }
        if key.quota_windows.len() != self.limits.quota.len() {
            key.quota_windows = self
                .limits
                .quota
                .iter()
                .map(|spec| QuotaWindow::new(*spec, now))
                .collect();
        }
        ScopeStatus {
            in_flight: key.in_flight,
            window_used: key.used,
            global_in_flight,
            quota: key
                .quota_windows
                .iter_mut()
                .map(|window| {
                    window.roll(now);
                    QuotaView {
                        kind: window.spec.kind,
                        remaining: window.spec.allowance.saturating_sub(window.used),
                        allowance: window.spec.allowance,
                        reset_unix: window.reset_unix,
                    }
                })
                .collect(),
        }
    }

    /// Remaining allowance of the scope's tightest quota window, as
    /// `(remaining, allowance)`; `None` when no quota windows are declared.
    /// Routing reads this so a nearly-spent cap is treated as scarce.
    pub fn quota_headroom(&self, scope: &str) -> Option<(u64, u64)> {
        self.scope_status(scope)
            .quota
            .iter()
            .map(|view| (view.remaining, view.allowance))
            .min_by_key(|(remaining, _)| *remaining)
    }
}

/// Observable state of one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeStatus {
    pub in_flight: u32,
    pub window_used: u32,
    pub global_in_flight: u32,
    /// Per-window remaining allowance and reset instant.
    pub quota: Vec<QuotaView>,
}

/// One quota window's observable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaView {
    pub kind: WindowKind,
    pub remaining: u64,
    pub allowance: u64,
    pub reset_unix: u64,
}

impl QuotaView {
    /// Fraction of the window spent — the scarcity signal routing reads.
    pub fn utilization_percent(&self) -> u8 {
        if self.allowance == 0 {
            return 0;
        }
        u8::try_from(
            (self.allowance - self.remaining)
                .saturating_mul(100)
                .min(u64::from(u8::MAX))
                / self.allowance,
        )
        .unwrap_or(0)
    }
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

/// Quota headroom of `scope` on the shared arbiter: `(remaining,
/// allowance)` of its tightest window, `None` when no windows are declared.
pub fn quota_headroom(scope: &str) -> Option<(u64, u64)> {
    arbiter().quota_headroom(scope)
}

/// A quota-headroom lookup: `(remaining, allowance)` of the scope's
/// tightest window, `None` when no windows are declared. Routing takes it
/// as a parameter so tests inject headroom without the global arbiter.
pub type HeadroomFn = dyn Fn(&str) -> Option<(u64, u64)>;
