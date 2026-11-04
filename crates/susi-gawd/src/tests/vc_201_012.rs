use crate::gap_priority::prioritize;

#[test]
fn vc_201_012_dedupes_and_ranks_by_impact() {
    let failures = vec![
        ("tool_missing".into(), 2.0, "r1".into()),
        ("tool_missing".into(), 2.0, "r2".into()),
        ("tool_missing".into(), 3.0, "r3".into()),
        ("auth_fail".into(), 10.0, "r4".into()),
    ];
    let p = prioritize(&failures);
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].pattern, "auth_fail");
    let missing = p.iter().find(|x| x.pattern == "tool_missing").unwrap();
    assert_eq!(missing.frequency, 3);
    assert_eq!(missing.receipts.len(), 3);
}

#[test]
fn vc_201_012_prioritize_empty_slice_returns_empty_vec() {
    let p = prioritize(&[]);
    assert!(p.is_empty());
}

#[test]
fn vc_201_012_ranks_higher_frequency_times_impact_score_first() {
    let failures = vec![
        ("frequent_medium".into(), 4.0, "r1".into()),
        ("frequent_medium".into(), 4.0, "r2".into()),
        ("frequent_medium".into(), 4.0, "r3".into()),
        ("rare_high".into(), 10.0, "r4".into()),
    ];
    let ranked = prioritize(&failures);
    assert_eq!(ranked.len(), 2);
    // frequent_medium has score 3 * 4.0 = 12.0 > rare_high score 1 * 10.0 = 10.0
    assert_eq!(ranked[0].pattern, "frequent_medium");
    assert!((ranked[0].score() - 12.0).abs() < f64::EPSILON);
    assert_eq!(ranked[1].pattern, "rare_high");
}

#[test]
fn vc_201_012_deduplicates_identical_receipts_while_incrementing_frequency() {
    let failures = vec![
        ("timeout_spike".into(), 1.5, "dup-rcpt".into()),
        ("timeout_spike".into(), 2.5, "dup-rcpt".into()),
        ("timeout_spike".into(), 2.0, "unique-rcpt".into()),
    ];
    let ranked = prioritize(&failures);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].frequency, 3);
    assert!((ranked[0].impact - 2.5).abs() < f64::EPSILON);
    assert_eq!(
        ranked[0].receipts,
        vec!["dup-rcpt".to_string(), "unique-rcpt".to_string()]
    );
}
