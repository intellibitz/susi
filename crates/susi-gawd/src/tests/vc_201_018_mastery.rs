//! Mastery verification for VC-201-018: a passing experiment produces a
//! reviewable release candidate promoted only via release paths, and
//! target/ installs into ~/.susi/bin are rejected.

use crate::cloud_rsi_delivery::{promote_via, refuse_dev_binary_install, FakeRelease};
use crate::rsi_promotion::{
    decide_promotion, produce_release_candidate, rejects_target_install, PromotionAttempt,
    PromotionDecision, PromotionPath,
};
use std::path::Path;

/// Falsification: 'a PASSING experiment produces a reviewable RC' — but
/// nothing checks the experiment passed, or even exists. Any string id
/// mints a reviewable candidate; a failed experiment promotes.
#[test]
fn vc_201_018_mastery_failed_experiment_promotes() {
    let rc = produce_release_candidate("e-failed", "sha256:abc", "ev");
    let d = decide_promotion(&PromotionAttempt {
        candidate: rc,
        via: PromotionPath::SusiRelease,
    });
    assert_eq!(
        d,
        PromotionDecision::AcceptedReviewable,
        "an experiment that never ran or failed promotes"
    );
}

/// Falsification: the path guard is substring matching. Staging the
/// binary through a neutral path strips the 'target/' marker, and the
/// install proceeds — refuse_dev_binary_install calls the boundary.
#[test]
fn vc_201_018_mastery_staged_install_evades_guard() {
    let mut rel = FakeRelease::default();
    // Binary was copied out of target/ to /tmp first — src no longer
    // contains the substring.
    let r = refuse_dev_binary_install(
        &mut rel,
        Path::new("/tmp/susi"),
        Path::new("/home/u/.susi/bin/susi"),
    );
    assert!(r.is_ok(), "staged dev binary installed into release home");
    // And a cwd-relative destination lacks the '.susi/bin' substring.
    assert!(!rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("bin/susi")
    ));
}

/// Falsification: whitespace-only 'evidence' and 'digest' satisfy the
/// manifest check — is_empty() on " " is false, so a content-free
/// manifest promotes.
#[test]
fn vc_201_018_mastery_empty_evidence_promotes() {
    let rc = produce_release_candidate("e1", " ", " ");
    assert_eq!(
        decide_promotion(&PromotionAttempt {
            candidate: rc,
            via: PromotionPath::SusiRelease,
        }),
        PromotionDecision::AcceptedReviewable
    );
}

/// Falsification: promote_via accepts evidence/digest the caller
/// fabricates — the promote_tagged boundary receives them unverified.
#[test]
fn vc_201_018_mastery_promote_via_trusts_callers_evidence() {
    let mut rel = FakeRelease::default();
    promote_via(&mut rel, PromotionPath::SusiRelease, "v9.9", " ", " ").unwrap();
    assert_eq!(rel.promoted.len(), 1, "unverified promotion recorded");
}

/// What holds: the DirectTargetInstall enum path is refused, a verbatim
/// target→.susi/bin pair is caught by the guard, and missing (truly
/// empty) evidence is refused.
#[test]
fn vc_201_018_mastery_nominal_refusals_hold() {
    let d = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate("e1", "d", "e"),
        via: PromotionPath::DirectTargetInstall,
    });
    assert!(matches!(d, PromotionDecision::Rejected(_)));
    assert!(rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("/home/u/.susi/bin/susi")
    ));
    let d2 = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate("e1", "", "e"),
        via: PromotionPath::SusiRelease,
    });
    assert!(matches!(d2, PromotionDecision::Rejected(_)));
}
