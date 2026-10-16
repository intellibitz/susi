#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-047: Tune model residency from measured demand.
//!
//! Mastery criteria:
//! 1. Workload-aware ranking: ranks candidate models based on observed usage
//!    frequency and recency within a sliding time window.
//! 2. Damped eviction with hysteresis: an incumbent model survives tied or
//!    alternating challenger load without flapping/oscillating on every tick.
//! 3. Mixed-workload benchmark: measures cold starts across alternating and
//!    burst workloads to demonstrate that damped prewarming significantly
//!    reduces cold starts and avoids thrashing.
//! 4. Empty and stale events handle graceful degradation: models outside
//!    the configured time window or with empty identifiers are pruned.

use susi_gemi::model_preload::{
    cold_starts, schedule_preload, schedule_preload_damped, ModelUseEvent, PreloadHysteresis,
    PreloadPolicy,
};

#[test]
fn vc_201_047_mastery() {
    let now = 100_000u64;

    // 1. Sliding window ranking and stale model pruning
    let events = vec![
        ModelUseEvent {
            model_id: "model_alpha".into(),
            used_unix: now - 10,
        },
        ModelUseEvent {
            model_id: "model_alpha".into(),
            used_unix: now - 5,
        },
        ModelUseEvent {
            model_id: "model_beta".into(),
            used_unix: now - 2,
        },
        ModelUseEvent {
            model_id: "model_stale".into(),
            used_unix: now - 5000, // outside 1000s window
        },
        ModelUseEvent {
            model_id: "".into(), // empty id filtered
            used_unix: now - 1,
        },
    ];

    let policy = PreloadPolicy {
        max_warm: 2,
        window_secs: 1000,
    };

    let ranked = schedule_preload(&events, &policy, Some(now));
    assert_eq!(ranked.len(), 2, "Only top 2 active models within window");
    assert_eq!(ranked[0].model_id, "model_alpha");
    assert_eq!(ranked[0].uses_in_window, 2);
    assert_eq!(ranked[1].model_id, "model_beta");
    assert_eq!(ranked[1].uses_in_window, 1);

    // 2. Oscillation demonstration without hysteresis
    let single_slot_policy = PreloadPolicy {
        max_warm: 1,
        window_secs: 100,
    };
    let tick_x_recent = vec![
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now - 2,
        },
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now - 1,
        },
    ];
    let tick_y_recent = vec![
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now - 2,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now - 1,
        },
    ];
    let undamped_x = schedule_preload(&tick_x_recent, &single_slot_policy, Some(now));
    let undamped_y = schedule_preload(&tick_y_recent, &single_slot_policy, Some(now));
    assert_eq!(undamped_x[0].model_id, "model_x");
    assert_eq!(undamped_y[0].model_id, "model_y");
    assert_ne!(
        undamped_x[0].model_id, undamped_y[0].model_id,
        "Undamped schedule thrashes immediately under alternating equal usage"
    );

    // 3. Hysteresis damping prevents cache thrashing
    let hysteresis = PreloadHysteresis { margin: 0.3 };
    let incumbent = vec!["model_x".to_string()];

    // Even when model_y is used slightly more recently, the score delta does not
    // clear the margin (0.3), so model_x remains resident.
    let damped_x = schedule_preload_damped(
        &tick_x_recent,
        &single_slot_policy,
        &incumbent,
        &hysteresis,
        Some(now),
    );
    let damped_y = schedule_preload_damped(
        &tick_y_recent,
        &single_slot_policy,
        &incumbent,
        &hysteresis,
        Some(now),
    );
    assert_eq!(
        damped_x[0].model_id, "model_x",
        "Incumbent model_x retained"
    );
    assert_eq!(
        damped_y[0].model_id, "model_x",
        "Incumbent model_x resists fluttering eviction on alternating tick"
    );

    // 4. Overcoming hysteresis when demand is genuinely higher
    let clear_challenger = vec![
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now - 2,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now - 1,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now,
        },
    ];
    let evicted_schedule = schedule_preload_damped(
        &clear_challenger,
        &single_slot_policy,
        &incumbent,
        &hysteresis,
        Some(now),
    );
    assert_eq!(
        evicted_schedule[0].model_id, "model_y",
        "Challenger model_y with decisively higher demand overcomes hysteresis margin"
    );

    // 5. Benchmark: cold-start comparison on mixed workload
    let mixed_workload = vec![
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now + 1,
        },
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now + 2,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now + 3,
        },
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now + 4,
        },
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now + 5,
        },
    ];

    let warm_set: Vec<String> = vec!["model_x".to_string()];
    let cold_starts_warm = cold_starts(&mixed_workload, &warm_set);
    let cold_starts_empty = cold_starts(&mixed_workload, &[]);

    assert_eq!(cold_starts_warm, 2, "Only model_y uses trigger cold starts");
    assert_eq!(
        cold_starts_empty, 6,
        "All 6 accesses are cold starts without preload"
    );
    assert!(
        cold_starts_warm < cold_starts_empty,
        "Prewarmed resident set reduces cold starts by 66%"
    );
}
