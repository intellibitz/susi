//! Tests for warm-model preload scheduler (`model_preload_*`).

use crate::model_preload::{schedule_preload, ModelUseEvent, PreloadPolicy};

#[test]
fn model_preload_ranks_by_frequency_then_recency_with_fake_clock() {
    let now = 10_000u64;
    let events = vec![
        ModelUseEvent {
            model_id: "a".into(),
            used_unix: now - 10,
        },
        ModelUseEvent {
            model_id: "a".into(),
            used_unix: now - 5,
        },
        ModelUseEvent {
            model_id: "b".into(),
            used_unix: now - 1,
        },
        ModelUseEvent {
            model_id: "c".into(),
            used_unix: now - 9_000, // outside window
        },
    ];
    let policy = PreloadPolicy {
        max_warm: 2,
        window_secs: 3600,
    };
    let plan = schedule_preload(&events, &policy, Some(now));
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].model_id, "a");
    assert_eq!(plan[0].uses_in_window, 2);
    assert_eq!(plan[1].model_id, "b");
    assert!(!plan.iter().any(|c| c.model_id == "c"));
}

#[test]
fn model_preload_respects_max_warm_bound() {
    let now = 100u64;
    let events: Vec<_> = (0..10)
        .map(|i| ModelUseEvent {
            model_id: format!("m{i}"),
            used_unix: now - i,
        })
        .collect();
    let policy = PreloadPolicy {
        max_warm: 3,
        window_secs: 1000,
    };
    let plan = schedule_preload(&events, &policy, Some(now));
    assert_eq!(plan.len(), 3);
}

#[test]
fn model_preload_empty_events_or_zero_max_warm_returns_empty_plan() {
    let policy = PreloadPolicy {
        max_warm: 2,
        window_secs: 3600,
    };
    assert!(schedule_preload(&[], &policy, Some(5_000)).is_empty());

    let zero_warm = PreloadPolicy {
        max_warm: 0,
        window_secs: 3600,
    };
    let events = vec![ModelUseEvent {
        model_id: "m1".into(),
        used_unix: 4_999,
    }];
    assert!(schedule_preload(&events, &zero_warm, Some(5_000)).is_empty());
}

#[test]
fn model_preload_breaks_frequency_ties_by_most_recent_timestamp() {
    let now = 2_000u64;
    let events = vec![
        ModelUseEvent {
            model_id: "older".into(),
            used_unix: now - 50,
        },
        ModelUseEvent {
            model_id: "newer".into(),
            used_unix: now - 5,
        },
    ];
    let policy = PreloadPolicy {
        max_warm: 2,
        window_secs: 600,
    };
    let plan = schedule_preload(&events, &policy, Some(now));
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].model_id, "newer");
    assert_eq!(plan[1].model_id, "older");
}
