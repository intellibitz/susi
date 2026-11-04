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

#[test]
fn vc_201_100_mastery_empty_stages_do_not_qualify() {
    let report = qualify_multi_stage(&[]);
    assert!(!report.qualified);
    assert!(!report.claim_2x);
    assert!(report.scorecard_deltas.is_empty());
}

#[test]
fn vc_201_100_mastery_budget_or_privacy_breach_blocks_qualification() {
    let mut bad_budget = evidence("local");
    bad_budget.budget_preserved = false;
    let report_budget = qualify_multi_stage(&[bad_budget, evidence("cloud")]);
    assert!(!report_budget.qualified);

    let mut bad_privacy = evidence("cloud");
    bad_privacy.privacy_preserved = false;
    let report_privacy = qualify_multi_stage(&[evidence("local"), bad_privacy]);
    assert!(!report_privacy.qualified);
}

#[test]
fn vc_201_100_mastery_sub_2x_metric_qualifies_without_claiming_2x() {
    let mut modest = evidence("local");
    modest.drill.predeclared_2x_metric = Some(15.0);
    modest.drill.baseline_2x_metric = Some(10.0);
    let report = qualify_multi_stage(&[modest, evidence("cloud")]);
    assert!(report.qualified);
    assert!(!report.claim_2x);
    assert_eq!(report.scorecard_deltas.len(), 2);
}

#[test]
fn vc_201_100_mastery_unconfigured_or_unpromoted_stage_blocks_qualification() {
    let mut unconfigured = evidence("local");
    unconfigured.drill.configured = false;
    assert!(!qualify_multi_stage(&[unconfigured, evidence("cloud")]).qualified);

    let mut unpromoted = evidence("cloud");
    unpromoted.drill.promoted_via_tag = false;
    assert!(!qualify_multi_stage(&[evidence("local"), unpromoted]).qualified);
}
