//! Mastery verification for VC-201-018: a passing experiment produces a
//! reviewable release candidate promoted only via release paths, and
//! target/ installs into ~/.susi/bin are rejected.

use crate::cloud_rsi::{ReviewVerdict, Verification};
use crate::cloud_rsi_delivery::{promote_via, refuse_dev_binary_install, FakeRelease};
use crate::rsi_promotion::{
    decide_promotion, produce_release_candidate, rejects_target_install, PromotionAttempt,
    PromotionDecision, PromotionPath,
};
use std::path::Path;

fn verified() -> Verification {
    Verification {
        verified: true,
        review: Some(ReviewVerdict {
            verdict: "approve".into(),
            reasons: vec!["ok".into()],
        }),
        reviewer: Some("rev-opaque".into()),
        gates: vec![("accept".into(), true)],
        regressions: Vec::new(),
        fabricated: Vec::new(),
        failover: None,
        reasons: Vec::new(),
    }
}

fn unverified() -> Verification {
    let mut v = verified();
    v.verified = false;
    v.reasons.push("gate failed".into());
    v
}

/// Fixed: `produce_release_candidate` now requires the experiment's own
/// independent verification outcome and refuses unless it actually
/// passed — an experiment that never ran, failed, or was never
/// independently reviewed can no longer mint a reviewable candidate.
#[test]
fn vc_201_018_mastery_failed_experiment_promotes() {
    let err = produce_release_candidate(&unverified(), "e-failed", "sha256:abc", "ev")
        .expect_err("an unverified experiment must not mint a release candidate");
    assert!(err.contains("verification"), "{err}");

    // A genuinely verified experiment still promotes.
    let rc = produce_release_candidate(&verified(), "e-ok", "sha256:abc", "ev").unwrap();
    let d = decide_promotion(&PromotionAttempt {
        candidate: rc,
        via: PromotionPath::SusiRelease,
    });
    assert_eq!(d, PromotionDecision::AcceptedReviewable);
}

/// Fixed: the destination alone governs the guard now, component-wise —
/// staging the binary through a neutral path no longer evades it, and a
/// relative destination (which cannot be proven safe without the
/// caller's cwd) is treated as forbidden rather than waved through.
#[test]
fn vc_201_018_mastery_staged_install_evades_guard() {
    let mut rel = FakeRelease::default();
    // Binary was copied out of target/ to /tmp first — src no longer
    // contains the substring, but the destination is still the release home.
    let r = refuse_dev_binary_install(
        &mut rel,
        Path::new("/tmp/susi"),
        Path::new("/home/u/.susi/bin/susi"),
    );
    assert!(
        r.is_err(),
        "a staged dev binary must still be refused — the destination governs"
    );
    assert!(rel.installs.is_empty());
    // A cwd-relative destination can't be proven safe, so it is refused too.
    assert!(rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("bin/susi")
    ));
}

/// Fixed: whitespace-only 'evidence' and 'digest' no longer satisfy the
/// manifest check — `trim().is_empty()` catches a content-free manifest.
#[test]
fn vc_201_018_mastery_empty_evidence_promotes() {
    let rc = produce_release_candidate(&verified(), "e1", " ", " ").unwrap();
    assert_eq!(
        decide_promotion(&PromotionAttempt {
            candidate: rc,
            via: PromotionPath::SusiRelease,
        }),
        PromotionDecision::Rejected("missing evidence or digest")
    );
}

/// Fixed: `promote_via` now requires real verification and no longer
/// promotes on a whitespace-only evidence/digest pair.
#[test]
fn vc_201_018_mastery_promote_via_trusts_callers_evidence() {
    let mut rel = FakeRelease::default();
    let err = promote_via(
        &mut rel,
        PromotionPath::SusiRelease,
        &verified(),
        "v9.9",
        " ",
        " ",
    )
    .expect_err("blank evidence/digest must be refused");
    assert!(matches!(
        err,
        crate::cloud_rsi_delivery::DeliveryError::ForbiddenPromotion(_)
    ));
    assert!(rel.promoted.is_empty());

    // An unverified experiment is refused even with well-formed evidence.
    let err2 = promote_via(
        &mut rel,
        PromotionPath::SusiRelease,
        &unverified(),
        "v9.9",
        "sha256:abc",
        "real evidence",
    )
    .expect_err("an unverified experiment must not promote");
    assert!(matches!(
        err2,
        crate::cloud_rsi_delivery::DeliveryError::ForbiddenPromotion(_)
    ));
    assert!(rel.promoted.is_empty());
}

/// What holds: the DirectTargetInstall enum path is refused, a verbatim
/// target→.susi/bin pair is caught by the guard, and missing (truly
/// empty) evidence is refused.
#[test]
fn vc_201_018_mastery_nominal_refusals_hold() {
    let d = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate(&verified(), "e1", "d", "e").unwrap(),
        via: PromotionPath::DirectTargetInstall,
    });
    assert!(matches!(d, PromotionDecision::Rejected(_)));
    assert!(rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("/home/u/.susi/bin/susi")
    ));
    let d2 = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate(&verified(), "e1", "", "e").unwrap(),
        via: PromotionPath::SusiRelease,
    });
    assert!(matches!(d2, PromotionDecision::Rejected(_)));
}

/// Production reachability: the real `evaluate_and_record` path — the
/// same one `susi tasks eval-corpus` runs — leaves a reviewable release
/// candidate under `.susi/release-candidates/` for a PromotionReady
/// experiment, and only for one. The persisted manifest binds the eval
/// evidence, corpus revision, measured scorecard, promotion spec and
/// the allowed promoters, and the recorded decision is the one
/// `decide_promotion` reached — never a silent promotion.
#[test]
fn vc_201_018_mastery_promotion_ready_experiment_leaves_a_release_candidate() {
    use crate::eval_separation::{
        evaluate_and_record, CandidateArtifact, EvalRunInput, ReleaseCandidateRecord,
    };
    use crate::rsi_corpus::{
        make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };
    use crate::scorecard::{DimensionScore, DimensionSpec, ImprovementScorecard, ScorecardSpec};
    use std::collections::BTreeSet;

    let dim = |name: &str, v: f64, u: f64| DimensionScore {
        name: name.into(),
        value: v,
        uncertainty: u,
    };
    let spec = ScorecardSpec {
        quality: DimensionSpec {
            threshold: 0.8,
            lower_is_better: false,
        },
        reliability: DimensionSpec {
            threshold: 0.9,
            lower_is_better: false,
        },
        latency: DimensionSpec {
            threshold: 100.0,
            lower_is_better: true,
        },
        resource: DimensionSpec {
            threshold: 0.9,
            lower_is_better: false,
        },
        operator_effort: DimensionSpec {
            threshold: 5.0,
            lower_is_better: true,
        },
        min_task_count: 30,
    };
    let measured = ImprovementScorecard {
        quality: dim("quality", 0.95, 0.01),
        reliability: dim("reliability", 0.99, 0.01),
        latency: dim("latency", 50.0, 5.0),
        resource: dim("resource", 0.99, 0.01),
        operator_effort: dim("operator_effort", 1.0, 0.1),
        task_count: 50,
    };
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-rc".into(),
        fixtures: vec![make_fixture(FixtureSpec {
            id: "h1",
            class: FixtureClass::Coding,
            input: "held-out input",
            split: FixtureSplit::HeldOut,
            seed: 1,
            evaluator_expected: Some("expected-output".into()),
        })],
        promotion_spec: Some(spec),
    };
    let root = std::env::temp_dir().join(format!("susi-vc018-prod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let run = |id: &str, m: Option<ImprovementScorecard>| EvalRunInput {
        candidate: CandidateArtifact {
            id: id.into(),
            actual_output: BTreeSet::from(["expected-output".to_string()]),
            expected_results: BTreeSet::new(),
        },
        candidate_can_write: false,
        training_access: BTreeSet::new(),
        memory_access: BTreeSet::new(),
        measured: m,
    };
    let rc_dir = root.join(".susi").join("release-candidates");

    // A cleared experiment mints its reviewable candidate + manifest.
    evaluate_and_record(&root, &corpus, &run("cand-rc", Some(measured))).unwrap();
    let record_path = rc_dir.join("cand-rc.json");
    assert!(
        record_path.is_file(),
        "a PromotionReady experiment must leave a release candidate"
    );
    let record: ReleaseCandidateRecord =
        serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
    assert_eq!(record.candidate.experiment_id, "cand-rc");
    assert_eq!(record.decision, "accepted_reviewable");
    assert!(!record.candidate.artifact_digest.trim().is_empty());
    let manifest: serde_json::Value =
        serde_json::from_str(&record.candidate.evidence_manifest).unwrap();
    assert_eq!(manifest["corpus_revision"], "rev-rc");
    assert_eq!(
        manifest["allowed_promoters"],
        serde_json::json!(["susi release", "scripts/susi-release-sync.sh"]),
        "the manifest names the only paths that may promote this candidate"
    );
    assert!(manifest["eval_evidence"]["verdict"].is_string());
    assert!(manifest["promotion_spec"].is_object());

    // An experiment that passes the gate but carries no measured
    // scorecard stays Evaluated — no release candidate is minted.
    evaluate_and_record(&root, &corpus, &run("cand-nospec", None)).unwrap();
    assert!(
        !rc_dir.join("cand-nospec.json").exists(),
        "an Evaluated experiment must not mint a release candidate"
    );

    let _ = std::fs::remove_dir_all(&root);
}
