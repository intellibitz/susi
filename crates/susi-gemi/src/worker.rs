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
    /// Free tier: zero marginal until the window cap, then saturated — the
    /// same shape as a subscription, declared for provider keys that carry
    /// one (rather than for agent seats, which are always subscription).
    FreeTier {
        calls_per_window: u64,
        window_secs: u64,
    },
    /// Prepaid balance: every call depletes the stock by its expected cost;
    /// once the remaining credit cannot cover the next call the worker is
    /// saturated. No reset — the window is the balance's lifetime.
    PrepaidBalance { allowance_usd: f64 },
}

impl BillingMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Metered => "metered",
            Self::Subscription { .. } => "subscription",
            Self::FreeTier { .. } => "free-tier",
            Self::PrepaidBalance { .. } => "prepaid",
        }
    }

    /// True for every mode whose next call is bounded by a cap/balance.
    pub fn capped(self) -> bool {
        !matches!(self, Self::Metered)
    }
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

/// A non-seat billing declaration from `SUSI_BILLING`:
/// `scope:subscription:calls/secs`, `scope:free:calls/secs`,
/// `scope:prepaid:usd`, `scope:metered` — comma-separated. This is how a
/// provider key that is NOT a metered price gets its real billing mode:
/// a plan with a daily allowance, a free tier, or a prepaid credit stock.
fn parse_billing(scope: &str, raw: &str) -> Option<(String, BillingMode)> {
    let mut it = raw.trim().split(':');
    let name = it.next()?.trim().to_string();
    if name != scope {
        return None;
    }
    let mode = match it.next()?.trim() {
        "subscription" => {
            let (calls, secs) = it.next()?.split_once('/')?;
            BillingMode::Subscription {
                calls_per_window: calls.parse().ok()?,
                window_secs: secs.parse().ok()?,
            }
        }
        "free" => {
            let (calls, secs) = it.next()?.split_once('/')?;
            BillingMode::FreeTier {
                calls_per_window: calls.parse().ok()?,
                window_secs: secs.parse().ok()?,
            }
        }
        "prepaid" => BillingMode::PrepaidBalance {
            allowance_usd: it.next()?.parse().ok()?,
        },
        "metered" => BillingMode::Metered,
        _ => return None,
    };
    Some((name, mode))
}

/// The worker's billing mode: a configured seat is always a subscription;
/// `SUSI_BILLING` declares modes for provider keys; everything else is
/// metered pay-as-you-go.
pub fn billing_mode(provider: &str) -> BillingMode {
    if let Some(spec) = seat_spec_for(provider) {
        return BillingMode::Subscription {
            calls_per_window: spec.calls_per_window,
            window_secs: spec.window_secs,
        };
    }
    std::env::var("SUSI_BILLING")
        .unwrap_or_default()
        .split(',')
        .find_map(|raw| parse_billing(provider, raw).map(|(_, m)| m))
        .unwrap_or(BillingMode::Metered)
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

// ── Per-worker window/balance accounting ──────────────────────────────────

/// window_start unix → calls spent in that window, per worker name —
/// covers every call-capped billing mode (seat or `SUSI_BILLING`-declared
/// subscription/free tier).
type WindowUsage = HashMap<String, (u64, u64)>;

fn windows() -> &'static Mutex<WindowUsage> {
    static U: OnceLock<Mutex<WindowUsage>> = OnceLock::new();
    U.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Prepaid stock depleted so far, per worker name (usd).
fn prepaid_used() -> &'static Mutex<HashMap<String, f64>> {
    static U: OnceLock<Mutex<HashMap<String, f64>>> = OnceLock::new();
    U.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Calls spent in the worker's current window plus the window reset time.
/// `None` when `provider` is not a configured seat.
pub fn seat_window_state(provider: &str, now: u64) -> Option<(u64, u64, u64)> {
    let spec = seat_spec_for(provider)?;
    let map = windows().lock().unwrap_or_else(|e| e.into_inner());
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
    let mut map = windows().lock().unwrap_or_else(|e| e.into_inner());
    let entry = map.entry(provider.to_string()).or_insert((now, 0));
    if now.saturating_sub(entry.0) >= spec.window_secs {
        *entry = (now, 0);
    }
    entry.1 = entry.1.saturating_add(1);
}

/// One dispatch attributed in worker terms — `spend_tracker::record` calls
/// this for every attempted provider call, so a declared cap or prepaid
/// balance depletes from real dispatch, not from bookkeeping no caller
/// performs. `usd` is the expected cost the call was billed at.
pub fn record_spend(provider: &str, usd: f64, now: u64) {
    match billing_mode(provider) {
        BillingMode::Subscription {
            calls_per_window: _,
            window_secs,
        }
        | BillingMode::FreeTier {
            calls_per_window: _,
            window_secs,
        } => {
            if seat_spec_for(provider).is_some() {
                // A seat counts itself inside `generate` (record_seat_call)
                // — counting again here would burn two calls per dispatch.
                return;
            }
            let mut map = windows().lock().unwrap_or_else(|e| e.into_inner());
            let entry = map.entry(provider.to_string()).or_insert((now, 0));
            if now.saturating_sub(entry.0) >= window_secs {
                *entry = (now, 0);
            }
            entry.1 = entry.1.saturating_add(1);
        }
        BillingMode::PrepaidBalance { .. } => {
            let mut map = prepaid_used().lock().unwrap_or_else(|e| e.into_inner());
            *map.entry(provider.to_string()).or_insert(0.0) += usd;
        }
        BillingMode::Metered => {}
    }
}

/// The worker's cap state in uniform units: `(spent, allowance, reset_unix)`
/// — calls for windowed modes, usd for prepaid, `None` reset for prepaid
/// (the balance's lifetime is the window). `None` for uncapped workers.
pub fn quota_state(provider: &str, now: u64) -> Option<(f64, f64, Option<u64>)> {
    match billing_mode(provider) {
        BillingMode::Subscription {
            calls_per_window,
            window_secs,
        }
        | BillingMode::FreeTier {
            calls_per_window,
            window_secs,
        } => {
            let map = windows().lock().unwrap_or_else(|e| e.into_inner());
            let (start, used) = map.get(provider).copied().unwrap_or((0, 0));
            let in_window = start > 0 && now.saturating_sub(start) < window_secs;
            Some((
                if in_window { used as f64 } else { 0.0 },
                calls_per_window as f64,
                Some(if in_window {
                    start + window_secs
                } else {
                    now + window_secs
                }),
            ))
        }
        BillingMode::PrepaidBalance { allowance_usd } => {
            let map = prepaid_used().lock().unwrap_or_else(|e| e.into_inner());
            Some((
                map.get(provider).copied().unwrap_or(0.0),
                allowance_usd,
                None,
            ))
        }
        BillingMode::Metered => None,
    }
}

/// Remaining fraction of the worker's tightest cap: `1.0` untouched,
/// `0.0` spent. `None` for uncapped (metered) workers — where the key
/// arbiter's own quota windows are the scarcity signal instead.
pub fn remaining_fraction(provider: &str, now: u64) -> Option<f64> {
    quota_state(provider, now).map(|(used, allowance, _)| {
        if allowance <= 0.0 {
            0.0
        } else {
            (1.0 - used / allowance).clamp(0.0, 1.0)
        }
    })
}

/// A capped worker whose allowance is spent is saturated: it steps down
/// rather than being hammered until the window resets (or forever, for a
/// prepaid stock with no reset).
pub fn saturated(provider: &str, now: u64) -> bool {
    quota_state(provider, now)
        .map(|(used, allowance, _)| used >= allowance)
        .unwrap_or(false)
}

/// A seat whose window allowance is spent is saturated.
pub fn seat_saturated(provider: &str, now: u64) -> bool {
    saturated(provider, now)
}

/// The marginal cost of one more task on `provider`, in billing terms:
/// subscription/free tier is zero under the cap, `f64::MAX` once
/// saturated; prepaid is the smaller of the expected cost and the
/// remaining stock, `f64::MAX` once depleted; metered passes `expected`
/// through untouched. `None` only for a metered worker with no price.
pub fn marginal_cost_usd(provider: &str, expected: Option<f64>, now: u64) -> Option<f64> {
    match billing_mode(provider) {
        BillingMode::Metered => expected,
        BillingMode::Subscription { .. } | BillingMode::FreeTier { .. } => {
            Some(if saturated(provider, now) {
                f64::MAX
            } else {
                0.0
            })
        }
        BillingMode::PrepaidBalance { .. } => quota_state(provider, now).map(|(used, allow, _)| {
            let remaining = (allow - used).max(0.0);
            match expected {
                Some(e) if e <= remaining => e,
                _ => f64::MAX,
            }
        }),
    }
}

/// Kept for the seat-only callers: the seat's marginal cost.
pub fn seat_marginal_cost_usd(provider: &str, now: u64) -> Option<f64> {
    seat_spec_for(provider).map(|_| {
        if seat_saturated(provider, now) {
            f64::MAX
        } else {
            0.0
        }
    })
}

/// Scarcity penalty on a marginal cost: how deliberately the remaining cap
/// should be conserved. Zero when the cap is untouched; rises as the
/// fraction remaining shrinks (`frac` → 0 is unbounded); `None` means
/// uncapped and never penalised. Deliberate draining, not accidental.
pub fn scarcity_penalty(remaining_fraction: f64) -> f64 {
    if remaining_fraction <= 0.0 {
        f64::MAX
    } else {
        (1.0 - remaining_fraction) / remaining_fraction
    }
}

/// The effective ranking cost: marginal cost per verified outcome scaled
/// by the scarcity penalty. Uncapped and untouched workers are unchanged;
/// a nearly-spent allowance is priced as the scarce resource it is.
pub fn effective_cost_usd(
    cost_per_outcome_usd: Option<f64>,
    remaining_fraction: Option<f64>,
) -> Option<f64> {
    match (cost_per_outcome_usd, remaining_fraction) {
        (_, Some(frac)) if frac <= 0.0 => Some(f64::MAX),
        (Some(cpo), Some(frac)) => Some(cpo * (1.0 + scarcity_penalty(frac))),
        (cpo, None) => cpo,
        (None, Some(_)) => None,
    }
}

/// The headroom view the ranking consumes: seats and declared caps come
/// from worker windows; other provider keys consult the key arbiter's own
/// quota windows — one function, every capped worker covered.
pub fn headroom(provider: &str) -> Option<(u64, u64)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some((used, allowance, _)) = quota_state(provider, now) {
        return Some(((allowance - used).max(0.0) as u64, allowance as u64));
    }
    crate::key_arbitration::quota_headroom(provider)
}

/// The remaining-fraction view, arbiter-aware: the ranking's scarcity
/// signal for every worker that has one.
pub fn remaining_fraction_unified(provider: &str) -> Option<f64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    remaining_fraction(provider, now).or_else(|| {
        crate::key_arbitration::quota_headroom(provider).map(|(rem, allow)| {
            if allow == 0 {
                0.0
            } else {
                rem as f64 / allow as f64
            }
        })
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
    /// Cap accounting (calls for windowed modes, usd for prepaid),
    /// present for capped workers only.
    pub window_used: Option<f64>,
    pub window_cap: Option<f64>,
    /// Saturated seats step down; this is the same fact the surfaces read.
    pub saturated: bool,
}

impl WorkerDescriptor {
    /// The marginal cost of one more unit of work on this worker: the
    /// ranking currency. A capped worker under its cap is cheaper than any
    /// metered model at equal measured capability.
    pub fn marginal_cost_usd(&self) -> Option<f64> {
        if self.saturated {
            return Some(f64::MAX);
        }
        match self.billing {
            BillingMode::Subscription { .. } | BillingMode::FreeTier { .. } => Some(0.0),
            BillingMode::PrepaidBalance { .. } => match (self.window_used, self.window_cap) {
                (Some(used), Some(allowance)) => {
                    let remaining = (allowance - used).max(0.0);
                    self.expected_cost_usd
                        .map(|e| if e <= remaining { e } else { f64::MAX })
                }
                _ => self.expected_cost_usd,
            },
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
    let billing = billing_mode(provider);
    let (kind, rate_per_min, max_concurrent) = match &spec {
        Some(s) => (
            WorkerKind::AgentSeat,
            Some(s.rate_per_min),
            Some(s.max_concurrent),
        ),
        None => (WorkerKind::ModelApi, None, None),
    };
    let (used, cap) = quota_state(provider, now)
        .map(|(u, c, _)| (Some(u), Some(c)))
        .unwrap_or((None, None));
    let saturated = saturated(provider, now);
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
pub(crate) fn reset_usage_for_test() {
    windows().lock().unwrap_or_else(|e| e.into_inner()).clear();
    prepaid_used()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}
