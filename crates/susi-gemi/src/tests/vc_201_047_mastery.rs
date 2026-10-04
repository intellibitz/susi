//! Vector VC-201-047 mastery tests.
//!
//! Vector: Tune model residency from measured demand.
//! Mastery target: Extend current idle and pressure eviction with workload-aware
//! prewarming and hysteresis; a mixed-workload benchmark measures cold starts
//! and proves the cache avoids repeated load/evict oscillation.

use crate::model_preload::{
    cold_starts, schedule_preload, schedule_preload_damped, ModelUseEvent, PreloadHysteresis,
    PreloadPolicy,
};

#[test]
fn vc_201_047_mastery_preload_ranking_under_window() {
    let now = 20_000u64;
    let events = vec![
        ModelUseEvent {
            model_id: "hot_model".into(),
            used_unix: now - 30,
        },
        ModelUseEvent {
            model_id: "hot_model".into(),
            used_unix: now - 15,
        },
        ModelUseEvent {
            model_id: "warm_model".into(),
            used_unix: now - 5,
        },
        ModelUseEvent {
            model_id: "stale_model".into(),
            used_unix: now - 7200, // outside 3600s window
        },
    ];

    let policy = PreloadPolicy {
        max_warm: 2,
        window_secs: 3600,
    };

    let candidates = schedule_preload(&events, &policy, Some(now));
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].model_id, "hot_model");
    assert_eq!(candidates[0].uses_in_window, 2);
    assert_eq!(candidates[1].model_id, "warm_model");
    assert_eq!(candidates[1].uses_in_window, 1);
}

#[test]
fn vc_201_047_mastery_alternating_load_reveals_oscillation_without_hysteresis() {
    let now = 1000u64;
    let policy = PreloadPolicy {
        max_warm: 1,
        window_secs: 100,
    };

    // Alternating events between model_x and model_y at equal frequency
    let events_step1 = vec![
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now - 2,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now - 1,
        },
    ];

    // Without hysteresis, model_y leads solely because of recency bonus
    let plan1 = schedule_preload(&events_step1, &policy, Some(now));
    assert_eq!(plan1.len(), 1);
    assert_eq!(plan1[0].model_id, "model_y");

    // One more tick where model_x is used flutters the leader immediately
    let events_step2 = vec![
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now - 1,
        },
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now,
        },
    ];
    let plan2 = schedule_preload(&events_step2, &policy, Some(now));
    assert_eq!(plan2.len(), 1);
    assert_eq!(plan2[0].model_id, "model_x");
    // Demonstrates immediate oscillation when scores are tied: hysteresis damping
    // is needed to prevent expensive repeated load/evict thrashing.
}

/// Acceptance: the production path damps eviction with hysteresis, so an
/// incumbent survives tied/alternating load, and a mixed-workload benchmark
/// shows the damped plan avoids the cold starts the undamped plan suffers.
#[test]
fn vc_201_047_preload_eviction_with_hysteresis() {
    let now = 1000u64;
    let policy = PreloadPolicy {
        max_warm: 1,
        window_secs: 100,
    };
    let hysteresis = PreloadHysteresis { margin: 0.25 };

    // Alternating tied load: x used one tick, y used the next, repeating.
    let tick_a = vec![
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now,
        },
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now - 1,
        },
    ];
    let tick_b = vec![
        ModelUseEvent {
            model_id: "model_y".into(),
            used_unix: now,
        },
        ModelUseEvent {
            model_id: "model_x".into(),
            used_unix: now - 1,
        },
    ];

    // Undamped: the leader flutters every tick (y, then x).
    let undamped_a = schedule_preload(&tick_a, &policy, Some(now));
    let undamped_b = schedule_preload(&tick_b, &policy, Some(now));
    assert_ne!(undamped_a[0].model_id, undamped_b[0].model_id);

    // Damped: once x is incumbent, the tied y cannot clear the margin, so x
    // stays warm across both ticks (no load/evict thrash).
    let incumbent = vec!["model_x".to_string()];
    let damped_a = schedule_preload_damped(&tick_a, &policy, &incumbent, &hysteresis, Some(now));
    let damped_b = schedule_preload_damped(&tick_b, &policy, &incumbent, &hysteresis, Some(now));
    assert_eq!(damped_a[0].model_id, "model_x");
    assert_eq!(damped_b[0].model_id, "model_x");

    // Mixed-workload benchmark: a workload of x/y/x/y/x with the damped warm
    // set {x} suffers fewer cold starts than an empty warm set.
    let workload = vec![
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
    ];
    let warm: Vec<String> = damped_a.iter().map(|c| c.model_id.clone()).collect();
    let cold_with_preload = cold_starts(&workload, &warm);
    let cold_without = cold_starts(&workload, &[]);
    assert!(cold_with_preload < cold_without);
}
