use crate::release_qualify::{qualify_release, ReleaseDrill};

#[test]
fn vc_201_100_qualifies_tagged_path_and_gates_2x_claim() {
    let ok = ReleaseDrill {
        env: "local".into(),
        configured: true,
        mission_ok: true,
        recovered: true,
        self_patch_evaluated: true,
        promoted_via_tag: true,
        scorecard_delta: 0.1,
        predeclared_2x_metric: Some(20.0),
        baseline_2x_metric: Some(10.0),
    };
    let r = qualify_release(&ok);
    assert!(r.qualified);
    assert!(r.claim_2x);

    let bypass = ReleaseDrill {
        promoted_via_tag: false,
        predeclared_2x_metric: Some(11.0),
        baseline_2x_metric: Some(10.0),
        ..ok
    };
    let r2 = qualify_release(&bypass);
    assert!(!r2.qualified);
    assert!(!r2.claim_2x);
    assert!(r2
        .limitations
        .iter()
        .any(|l| l.contains("tagged") || l.contains("2x")));
}

#[test]
fn vc_201_100_unrecovered_or_failed_mission_disqualifies_release() {
    let failed_mission = ReleaseDrill {
        env: "staging".into(),
        configured: true,
        mission_ok: false,
        recovered: false,
        self_patch_evaluated: true,
        promoted_via_tag: true,
        scorecard_delta: 0.2,
        predeclared_2x_metric: None,
        baseline_2x_metric: None,
    };
    let r = qualify_release(&failed_mission);
    assert!(!r.qualified);
    assert!(!r.limitations.is_empty());
}

#[test]
fn vc_201_100_unconfigured_or_unevaluated_self_patch_disqualifies_release_and_records_delta() {
    let unconfigured = ReleaseDrill {
        env: "local".into(),
        configured: false,
        mission_ok: true,
        recovered: true,
        self_patch_evaluated: false,
        promoted_via_tag: true,
        scorecard_delta: 0.0,
        predeclared_2x_metric: None,
        baseline_2x_metric: None,
    };
    let r = qualify_release(&unconfigured);
    assert!(!r.qualified);
    assert_eq!(r.scorecard_deltas.len(), 1);
    assert_eq!(r.scorecard_deltas[0].environment, "local");
    assert_eq!(r.scorecard_deltas[0].delta, 0.0);
    assert!(r
        .limitations
        .iter()
        .any(|l| l.contains("ecosystem not configured")));
    assert!(r
        .limitations
        .iter()
        .any(|l| l.contains("self-patch not evaluated")));
}

#[test]
fn vc_201_100_qualifies_without_predeclared_2x_metric_keeping_claim_false() {
    let standard = ReleaseDrill {
        env: "prod".into(),
        configured: true,
        mission_ok: true,
        recovered: true,
        self_patch_evaluated: true,
        promoted_via_tag: true,
        scorecard_delta: 0.15,
        predeclared_2x_metric: None,
        baseline_2x_metric: None,
    };
    let r = qualify_release(&standard);
    assert!(r.qualified);
    assert!(!r.claim_2x);
}
