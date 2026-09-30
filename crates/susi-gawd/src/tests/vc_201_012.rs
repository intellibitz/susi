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
