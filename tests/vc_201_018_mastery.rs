#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-018: a passing experiment produces a reviewable release
//! candidate and evidence manifest; only `susi release` and
//! `scripts/susi-release-sync.sh` promote tagged builds, and attempts to
//! install `target/` binaries into `~/.susi/bin` are rejected.
//!
//! The full falsification suite lives at
//! `crates/susi-gawd/src/tests/vc_201_018_mastery.rs` (exercised by
//! `cargo nextest run -p susi-gawd -E test(vc_201_018_mastery)`); this is
//! the root-package integration test the task's own bare accept command
//! (`cargo nextest run -E test(vc_201_018_mastery)`) actually discovers —
//! bare `nextest run` without `-p`/`--workspace` only discovers the root
//! `susi` package's own `tests/*.rs`, never a member crate's unit tests.

use std::path::Path;
use susi_gawd::cloud_rsi::{ReviewVerdict, Verification};
use susi_gawd::cloud_rsi_delivery::{
    promote_via, refuse_dev_binary_install, DeliveryError, FakeRelease,
};
use susi_gawd::rsi_promotion::{
    decide_promotion, produce_release_candidate, rejects_target_install, PromotionAttempt,
    PromotionDecision, PromotionPath,
};

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

#[test]
fn vc_201_018_mastery() {
    // An unverified experiment cannot mint a release candidate; a
    // genuinely verified one still promotes.
    let err = produce_release_candidate(&unverified(), "e-failed", "sha256:abc", "ev")
        .expect_err("an unverified experiment must not mint a release candidate");
    assert!(err.contains("verification"), "{err}");
    let rc = produce_release_candidate(&verified(), "e-ok", "sha256:abc", "ev").unwrap();
    assert_eq!(
        decide_promotion(&PromotionAttempt {
            candidate: rc,
            via: PromotionPath::SusiRelease,
        }),
        PromotionDecision::AcceptedReviewable
    );

    // Whitespace-only evidence/digest is refused, not waved through.
    let blank_rc = produce_release_candidate(&verified(), "e1", " ", " ").unwrap();
    assert_eq!(
        decide_promotion(&PromotionAttempt {
            candidate: blank_rc,
            via: PromotionPath::SusiRelease,
        }),
        PromotionDecision::Rejected("missing evidence or digest")
    );

    // The destination alone governs the dev-binary-install guard:
    // staging the binary through a neutral path first does not evade it,
    // and a cwd-relative destination can't be proven safe, so it is
    // refused too.
    let mut rel = FakeRelease::default();
    let staged = refuse_dev_binary_install(
        &mut rel,
        Path::new("/tmp/susi"),
        Path::new("/home/u/.susi/bin/susi"),
    );
    assert!(staged.is_err(), "a staged dev binary must still be refused");
    assert!(rel.installs.is_empty());
    assert!(rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("bin/susi")
    ));

    // promote_via requires real verification and refuses a blank
    // evidence/digest pair even for a verified experiment.
    let err = promote_via(
        &mut rel,
        PromotionPath::SusiRelease,
        &verified(),
        "v9.9",
        " ",
        " ",
    )
    .expect_err("blank evidence/digest must be refused");
    assert!(matches!(err, DeliveryError::ForbiddenPromotion(_)));
    let err2 = promote_via(
        &mut rel,
        PromotionPath::SusiRelease,
        &unverified(),
        "v9.9",
        "sha256:abc",
        "real evidence",
    )
    .expect_err("an unverified experiment must not promote");
    assert!(matches!(err2, DeliveryError::ForbiddenPromotion(_)));
    assert!(rel.promoted.is_empty());

    // Nominal: DirectTargetInstall is refused outright, and a verbatim
    // target→.susi/bin pair is caught by the guard.
    let direct = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate(&verified(), "e1", "d", "e").unwrap(),
        via: PromotionPath::DirectTargetInstall,
    });
    assert!(matches!(direct, PromotionDecision::Rejected(_)));
    assert!(rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("/home/u/.susi/bin/susi")
    ));
}
