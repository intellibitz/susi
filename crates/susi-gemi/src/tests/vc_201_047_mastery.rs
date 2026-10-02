//! Vector VC-201-047 mastery tests.
//!
//! Vector: Tune model residency from measured demand.
//! Mastery target: Extend current idle and pressure eviction with workload-aware
//! prewarming and hysteresis; a mixed-workload benchmark measures cold starts
//! and proves the cache avoids repeated load/evict oscillation.

use crate::model_preload::{schedule_preload, ModelUseEvent, PreloadPolicy};

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
