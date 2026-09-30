//! Autoscale managed endpoints inside a spend ceiling (VC-201-056).
//!
//! Decisions are bounded: scale-out is capped by `max_replicas` *and* the
//! hourly spend ceiling; scale-in waits out the stabilization window and
//! drains — replicas are marked `Draining` so in-flight requests finish
//! rather than being cut mid-request.

/// Live metrics for one admitted deployment.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointMetrics {
    pub replicas: u32,
    /// Requests waiting for a replica.
    pub queue_depth: u32,
    pub p95_latency_ms: u64,
    /// Requests currently being served — blocks instant scale-in.
    pub in_flight: u32,
    /// Tick of the last applied replica change (stabilization).
    pub last_change_tick: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoscalePolicy {
    pub min_replicas: u32,
    pub max_replicas: u32,
    /// Queue depth per replica above which scale-out triggers.
    pub target_queue_per_replica: u32,
    /// p95 latency above which scale-out triggers.
    pub latency_ceiling_ms: u64,
    /// Ticks a scale-in must see sustained idle before acting.
    pub stabilization_ticks: u64,
    /// Hard hourly spend ceiling for this deployment.
    pub spend_cap_cents_per_hour: u64,
    /// Cost of one replica-hour.
    pub cost_per_replica_hour_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleDecision {
    /// Add `n` replicas.
    ScaleOut { add: u32, reason: &'static str },
    /// Remove `n` replicas; callers mark them `Draining` until their
    /// in-flight set empties — never kill mid-request.
    ScaleIn { remove: u32, reason: &'static str },
    /// No change; `reason` names the binding constraint for the audit log.
    Hold { reason: &'static str },
}

/// One bounded autoscale step.
///
/// - Scale-out fires when queue-per-replica or p95 latency exceeds its
///   target, bounded by `max_replicas` and by however many replicas the
///   remaining hourly budget can still pay for this hour.
/// - Scale-in fires only when load is below target *and* the deployment
///   has been quiet for `stabilization_ticks` since the last change —
///   replicas are removed one drain at a time.
/// - In-flight requests always hold a scale-in: a replica with live
///   requests must drain first.
#[must_use]
pub fn decide(
    m: &EndpointMetrics,
    p: &AutoscalePolicy,
    now_tick: u64,
    hourly_spend_so_far_cents: u64,
) -> ScaleDecision {
    let replicas = m.replicas.max(1);
    let queue_per_replica = f64::from(m.queue_depth) / f64::from(replicas);

    let need_out = queue_per_replica > f64::from(p.target_queue_per_replica)
        || m.p95_latency_ms > p.latency_ceiling_ms;

    if need_out && m.replicas < p.max_replicas {
        // How many replica-hours does the remaining budget still buy?
        let remaining = p
            .spend_cap_cents_per_hour
            .saturating_sub(hourly_spend_so_far_cents);
        let affordable = remaining
            .checked_div(p.cost_per_replica_hour_cents)
            .map_or(u32::MAX, |n| u32::try_from(n).unwrap_or(u32::MAX));
        if affordable == 0 {
            return ScaleDecision::Hold {
                reason: "spend-cap: no replica-hour budget left",
            };
        }
        // Want enough replicas to bring queue-per-replica back to target.
        let want = m
            .queue_depth
            .div_ceil(p.target_queue_per_replica.max(1))
            .max(m.replicas + 1);
        let add = want
            .min(p.max_replicas)
            .min(m.replicas.saturating_add(affordable))
            .saturating_sub(m.replicas);
        if add > 0 {
            return ScaleDecision::ScaleOut {
                add,
                reason: "queue/latency above target",
            };
        }
    }

    let idle = queue_per_replica * 2.0 < f64::from(p.target_queue_per_replica)
        && m.p95_latency_ms * 2 <= p.latency_ceiling_ms;
    let quiet_long_enough = now_tick.saturating_sub(m.last_change_tick) >= p.stabilization_ticks;

    if idle && m.replicas > p.min_replicas {
        if m.in_flight > 0 {
            return ScaleDecision::Hold {
                reason: "draining: in-flight requests on surplus replicas",
            };
        }
        if !quiet_long_enough {
            return ScaleDecision::Hold {
                reason: "stabilization window still open",
            };
        }
        return ScaleDecision::ScaleIn {
            remove: (m.replicas - p.min_replicas).min(1),
            reason: "sustained idle past stabilization window",
        };
    }

    if m.replicas < p.min_replicas {
        return ScaleDecision::ScaleOut {
            add: p.min_replicas - m.replicas,
            reason: "below configured minimum",
        };
    }
    ScaleDecision::Hold {
        reason: "within targets",
    }
}
