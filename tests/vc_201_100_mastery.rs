#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // the checked-in qualification source is a required fixture

use susi_gawd::release_qualify::{qualify_multi_stage, QualificationInput, ReleaseDrill};

fn evidence(environment: &str) -> QualificationInput {
    QualificationInput {
        drill: ReleaseDrill {
            env: environment.into(),
            configured: true,
            mission_ok: true,
            recovered: true,
            self_patch_evaluated: true,
            promoted_via_tag: true,
            scorecard_delta: 0.5,
            predeclared_2x_metric: Some(20.0),
            baseline_2x_metric: Some(10.0),
        },
        budget_preserved: true,
        privacy_preserved: true,
    }
}

#[test]
fn vc_201_100_mastery() {
    let report = qualify_multi_stage(&[evidence("local"), evidence("cloud")]);
    assert!(report.qualified);
    assert!(report.claim_2x);
    assert_eq!(report.scorecard_deltas.len(), 2);
}
