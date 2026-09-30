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
