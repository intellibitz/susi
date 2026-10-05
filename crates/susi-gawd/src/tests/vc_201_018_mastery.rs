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
