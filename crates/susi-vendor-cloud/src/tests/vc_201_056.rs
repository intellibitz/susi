use crate::autoscale::{decide, AutoscalePolicy, EndpointMetrics, ScaleDecision};

fn policy() -> AutoscalePolicy {
    AutoscalePolicy {
        min_replicas: 1,
        max_replicas: 8,
        target_queue_per_replica: 4,
        latency_ceiling_ms: 800,
        stabilization_ticks: 10,
        spend_cap_cents_per_hour: 1000,
        cost_per_replica_hour_cents: 100,
    }
}

fn metrics(replicas: u32, queue: u32, p95: u64) -> EndpointMetrics {
    EndpointMetrics {
        replicas,
        queue_depth: queue,
        p95_latency_ms: p95,
        in_flight: 0,
        last_change_tick: 0,
    }
}

#[test]
fn vc_201_056_scale_out_on_queue_depth_and_latency() {
    // 3 replicas, queue 24 → 8/replica > target 4 → wants 6.
    let d = decide(&metrics(3, 24, 200), &policy(), 5, 0);
    assert_eq!(
        d,
        ScaleDecision::ScaleOut {
            add: 3,
            reason: "queue/latency above target"
        }
    );
    // Latency alone triggers too.
    let d = decide(&metrics(4, 0, 2000), &policy(), 5, 0);
    assert!(matches!(d, ScaleDecision::ScaleOut { add: 1, .. }));
}

#[test]
fn vc_201_056_scale_bounded_by_max_and_spend_cap() {
    // Queue wants 16 replicas; max caps at 8.
    let d = decide(&metrics(2, 100, 100), &policy(), 5, 0);
    assert!(matches!(d, ScaleDecision::ScaleOut { add: 6, .. }));
    // Spend cap: 800 spent of 1000 → 2 replica-hours affordable.
    let d = decide(&metrics(2, 100, 100), &policy(), 5, 800);
    assert!(matches!(d, ScaleDecision::ScaleOut { add: 2, .. }));
    // Budget exhausted → explicit hold naming the cap.
    let d = decide(&metrics(2, 100, 100), &policy(), 5, 1000);
    assert!(matches!(
        d,
        ScaleDecision::Hold {
            reason: "spend-cap: no replica-hour budget left"
        }
    ));
}

#[test]
fn vc_201_056_scale_in_waits_for_drain_and_stabilization() {
    // Idle but requests in flight → hold for drain, not a kill.
    let mut m = metrics(5, 0, 10);
    m.in_flight = 3;
    let d = decide(&m, &policy(), 100, 0);
    assert!(matches!(
        d,
        ScaleDecision::Hold {
            reason: "draining: in-flight requests on surplus replicas"
        }
    ));
    // Drained but window still open (changed at 95, now 100 < 95+10).
    m.in_flight = 0;
    m.last_change_tick = 95;
    let d = decide(&m, &policy(), 100, 0);
    assert!(matches!(
        d,
        ScaleDecision::Hold {
            reason: "stabilization window still open"
        }
    ));
    // Window elapsed → scale in one replica at a time.
    let d = decide(&m, &policy(), 200, 0);
    assert!(matches!(d, ScaleDecision::ScaleIn { remove: 1, .. }));
    // Never below floor.
    let d = decide(&metrics(1, 0, 10), &policy(), 200, 0);
    assert!(matches!(d, ScaleDecision::Hold { .. }));
}

#[test]
fn vc_201_056_recovers_below_minimum() {
    let d = decide(&metrics(0, 0, 0), &policy(), 1, 0);
    assert_eq!(
        d,
        ScaleDecision::ScaleOut {
            add: 1,
            reason: "below configured minimum"
        }
    );
}
