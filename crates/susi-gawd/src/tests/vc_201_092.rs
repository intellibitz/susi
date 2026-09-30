use crate::ops_slo::{publish_slos, SloSample};

#[test]
fn vc_201_092_marks_unknown_when_samples_insufficient() {
    let s = SloSample {
        availability: 0.99,
        task_success: 0.95,
        queue_delay_ms: 10.0,
        inference_latency_ms: 50.0,
        recovery_duration_ms: 100.0,
        sample_count: 2,
    };
    let windows = publish_slos(&s, 10);
    assert!(windows.iter().all(|w| w.unknown && w.value.is_none()));
}

#[test]
fn vc_201_092_publishes_values_with_denominator() {
    let s = SloSample {
        availability: 0.99,
        task_success: 0.95,
        queue_delay_ms: 10.0,
        inference_latency_ms: 50.0,
        recovery_duration_ms: 100.0,
        sample_count: 20,
    };
    let windows = publish_slos(&s, 10);
    assert_eq!(windows.len(), 5);
    assert!(windows.iter().all(|w| !w.unknown && w.denominator == 20));
    assert_eq!(windows[0].value, Some(0.99));
}
