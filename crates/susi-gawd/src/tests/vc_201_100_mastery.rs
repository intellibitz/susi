//! Mastery verification for the complete multi-stage release qualification.

use crate::release_qualify::{qualify_multi_stage, QualificationInput, ReleaseDrill};

fn input(env: &str, scorecard_delta: f64) -> QualificationInput {
    QualificationInput {
        drill: ReleaseDrill {
            env: env.into(),
            configured: true,
            mission_ok: true,
            recovered: true,
            self_patch_evaluated: true,
            promoted_via_tag: true,
            scorecard_delta,
            predeclared_2x_metric: Some(20.0),
            baseline_2x_metric: Some(10.0),
        },
        budget_preserved: true,
        privacy_preserved: true,
    }
}

#[test]
fn vc_201_100_mastery() {
    let report = qualify_multi_stage(&[input("local", 0.25), input("cloud", 0.5)]);
    assert!(report.qualified, "qualification report: {report:?}");
    assert!(report.claim_2x);
    assert_eq!(report.scorecard_deltas.len(), 2);
    assert_eq!(report.scorecard_deltas[0].environment, "cloud");
    assert_eq!(report.scorecard_deltas[1].environment, "local");
    assert!(report.limitations.is_empty());
}

#[test]
fn vc_201_100_mastery_requires_explicit_cloud_and_preserves_limitations() {
    let report = qualify_multi_stage(&[input("local", 0.25)]);
    assert!(!report.qualified);
    assert!(report
        .limitations
        .iter()
        .any(|limitation| limitation.contains("cloud")));

    let mut cloud = input("cloud", 0.5);
    cloud.budget_preserved = false;
    let report = qualify_multi_stage(&[input("local", 0.25), cloud]);
    assert!(!report.qualified);
    assert!(report
        .limitations
        .iter()
        .any(|limitation| limitation.contains("budget preservation")));
}

#[test]
fn vc_201_100_mastery_release_path_consumes_explicit_evidence() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/admin/release.rs"))
            .expect("release admin source must exist");
    assert!(source.contains("SUSI_RELEASE_CLOUD_EVIDENCE"));
    assert!(source.contains("qualify_multi_stage"));
    assert!(source.contains("SUSI_RELEASE_SELF_PATCH_EVIDENCE"));
}
