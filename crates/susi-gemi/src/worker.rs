//! Worker unification (VC-202-022): a brain candidate is a *worker* — either
//! a metered model API or a subscription-billed agent seat. One descriptor
//! carries kind, billing mode, rate and concurrency limits, health,
//! capability evidence and cost attribution. Election, ranking, the
//! capability matrix, the routing ladder and the budget all key off the
//! worker's registered provider name, so a seat is treated uniformly by
//! construction rather than by special-casing every consumer.
//!
//! Billing semantics: a metered model's marginal cost is its expected task
//! cost from the price catalog; a subscription seat's marginal cost is zero
//! until its declared window cap is spent, after which one more call is
//! unaffordable — represented as `f64::MAX` so every cost-ordered surface
//! (rank comparator, pre-dispatch spend ceiling, ladder expected-cost
//! display) steps the saturated seat down instead of hammering it.
//!
//! Seat usage windows are tracked process-wide, like the key arbiter's rate
//! windows: the daemon's lifetime is the accounting scope, and a restart
//! resets the window counter (the arbiter has the same documented limit).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// What kind of worker a brain candidate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerKind {
    /// Pay-per-token model endpoint (cloud API or local engine).
    ModelApi,
    /// Subscription-billed external agent seat (claude, codex, cursor, …)
    /// reachable through the `agents.external.managed` capability-bus path.
    AgentSeat,
}

/// Billing semantics that decide a worker's marginal cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BillingMode {
    /// Pay per call; marginal cost is the expected task cost from the
    /// installed price catalog.
    Metered,
    /// Fixed subscription; marginal cost is zero until `calls_per_window`
    /// calls have been spent in `window_secs`, then the seat is saturated.
    Subscription {
        calls_per_window: u64,
        window_secs: u64,
    },
}

/// One configured agent seat: `id:calls_per_window/window_secs[:concurrent[:rate_per_min]]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SeatSpec {
    /// Managed-catalog agent id (`claude`, `codex`, `cursor`, `devin`, …).
    pub agent: String,
    /// Subscription cap: at most this many delegated calls per window.
    pub calls_per_window: u64,
    /// Window length in seconds (rolling from first call in the window).
    pub window_secs: u64,
    /// Per-seat concurrent delegation bound (default 1).
    pub max_concurrent: u32,
    /// Per-seat request-rate bound per minute (default 30).
    pub rate_per_min: u32,
}

fn parse_seat_spec(raw: &str) -> Option<SeatSpec> {
    let mut it = raw.trim().split(':');
    let agent = it.next()?.trim().to_string();
    if agent.is_empty() {
        return None;
    }
    let (calls, secs) = it.next()?.split_once('/')?;
    let calls_per_window = calls.parse::<u64>().ok()?;
    let window_secs = secs.parse::<u64>().ok()?;
    if calls_per_window == 0 || window_secs == 0 {
        return None;
    }
    Some(SeatSpec {
        agent,
        calls_per_window,
        window_secs,
        max_concurrent: it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(1),
        rate_per_min: it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(30),
    })
}

/// The configured seats: `SUSI_AGENT_SEATS`, comma-separated seat specs.
/// A seat that is not declared is never a worker — declaration is the
/// opt-in, so an undeclared `seat-*` name is just an ordinary string.
pub fn configured_seats() -> Vec<SeatSpec> {
    std::env::var("SUSI_AGENT_SEATS")
        .unwrap_or_default()
        .split(',')
        .filter_map(parse_seat_spec)
        .collect()
}

/// The registered provider name a seat carries (`claude` → `seat-claude`).
pub fn seat_name(agent: &str) -> String {
    format!("seat-{}", agent.trim())
}

/// The seat spec for a provider name, when it names a configured seat.
pub fn seat_spec_for(provider: &str) -> Option<SeatSpec> {
    configured_seats()
        .into_iter()
        .find(|s| seat_name(&s.agent) == provider)
}

/// True when `provider` is a configured agent seat.
pub fn is_seat(provider: &str) -> bool {
    seat_spec_for(provider).is_some()
}

// ── Per-seat window accounting ────────────────────────────────────────────

/// window_start unix → calls spent in that window, per seat name.
type SeatUsage = HashMap<String, (u64, u64)>;

fn usage() -> &'static Mutex<SeatUsage> {
    static U: OnceLock<Mutex<SeatUsage>> = OnceLock::new();
    U.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Calls spent in the seat's current window plus the window reset time.
/// `None` when `provider` is not a configured seat.
pub fn seat_window_state(provider: &str, now: u64) -> Option<(u64, u64, u64)> {
    let spec = seat_spec_for(provider)?;
    let map = usage().lock().unwrap_or_else(|e| e.into_inner());
    let (start, used) = map.get(provider).copied().unwrap_or((0, 0));
    let in_window = start > 0 && now.saturating_sub(start) < spec.window_secs;
    let used = if in_window { used } else { 0 };
    let reset = if in_window {
        start + spec.window_secs
    } else {
        now + spec.window_secs
    };
    Some((used, spec.calls_per_window, reset))
}

/// Record one delegated call against the seat's window.
pub fn record_seat_call(provider: &str, now: u64) {
    let Some(spec) = seat_spec_for(provider) else {
        return;
    };
    let mut map = usage().lock().unwrap_or_else(|e| e.into_inner());
    let entry = map.entry(provider.to_string()).or_insert((now, 0));
    if now.saturating_sub(entry.0) >= spec.window_secs {
        *entry = (now, 0);
    }
    entry.1 = entry.1.saturating_add(1);
}

/// A seat whose window allowance is spent is saturated: it steps down
/// rather than being hammered until the window resets.
pub fn seat_saturated(provider: &str, now: u64) -> bool {
    seat_window_state(provider, now)
        .map(|(used, cap, _)| used >= cap)
        .unwrap_or(false)
}

/// The seat's marginal cost for one more task: zero under the subscription
/// cap, `f64::MAX` once saturated — unaffordable on every cost surface.
/// `None` when `provider` is not a configured seat.
pub fn seat_marginal_cost_usd(provider: &str, now: u64) -> Option<f64> {
    seat_spec_for(provider).map(|_| {
        if seat_saturated(provider, now) {
            f64::MAX
        } else {
            0.0
        }
    })
}

/// One uniform view of a brain candidate, whatever it is: kind, billing
/// mode, limits, health, capability evidence and cost attribution.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerDescriptor {
    pub name: String,
    pub kind: WorkerKind,
    pub billing: BillingMode,
    /// Per-worker rate bound (requests/min); `None` = arbiter default.
    pub rate_per_min: Option<u32>,
    /// Per-worker concurrency bound; `None` = arbiter default.
    pub max_concurrent: Option<u32>,
    /// Health axis: not unfit and not in a quarantine window.
    pub healthy: bool,
    /// Capability evidence for this task class.
    pub samples: u64,
    pub success_rate: Option<f64>,
    /// Measured attribution (expected-cost-per-verified-outcome surface).
    pub expected_cost_usd: Option<f64>,
    pub cost_per_outcome_usd: Option<f64>,
    /// Subscription accounting, present for seats only.
    pub window_used: Option<u64>,
    pub window_cap: Option<u64>,
    /// Saturated seats step down; this is the same fact the surfaces read.
    pub saturated: bool,
}

impl WorkerDescriptor {
    /// The marginal cost of one more unit of work on this worker: the
    /// ranking currency. A subscription seat under its cap is cheaper than
    /// any metered model at equal measured capability.
    pub fn marginal_cost_usd(&self) -> Option<f64> {
        if self.saturated {
            return Some(f64::MAX);
        }
        match self.billing {
            BillingMode::Subscription { .. } => Some(0.0),
            BillingMode::Metered => self.expected_cost_usd.or(self.cost_per_outcome_usd),
        }
    }
}

/// Build the descriptor for one worker from the evidence store, the price
/// catalog (through the ranking surfaces) and the seat configuration.
pub fn descriptor(
    store: &crate::engines::brain::Store,
    provider: &str,
    class: crate::engines::brain::TaskClass,
    now: u64,
) -> WorkerDescriptor {
    let ranked = store.rank(&[provider.to_string()], class);
    let r = ranked.first();
    let spec = seat_spec_for(provider);
    let (kind, billing, rate_per_min, max_concurrent) = match &spec {
        Some(s) => (
            WorkerKind::AgentSeat,
            BillingMode::Subscription {
                calls_per_window: s.calls_per_window,
                window_secs: s.window_secs,
            },
            Some(s.rate_per_min),
            Some(s.max_concurrent),
        ),
        None => (WorkerKind::ModelApi, BillingMode::Metered, None, None),
    };
    let (used, cap) = seat_window_state(provider, now)
        .map(|(u, c, _)| (Some(u), Some(c)))
        .unwrap_or((None, None));
    let saturated = seat_saturated(provider, now);
    WorkerDescriptor {
        name: provider.to_string(),
        kind,
        billing,
        rate_per_min,
        max_concurrent,
        healthy: r.map(|r| !r.unfit).unwrap_or(true) && !saturated,
        samples: r.map(|r| u64::from(r.samples)).unwrap_or(0),
        success_rate: r.and_then(|r| r.success_rate.map(f64::from)),
        expected_cost_usd: r.and_then(|r| r.expected_cost_usd),
        cost_per_outcome_usd: r.and_then(|r| r.cost_per_outcome_usd),
        window_used: used,
        window_cap: cap,
        saturated,
    }
}

#[cfg(test)]
pub(crate) fn reset_seat_usage_for_test() {
    usage().lock().unwrap_or_else(|e| e.into_inner()).clear();
}
