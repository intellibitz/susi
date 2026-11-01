use crate::cloud_rsi::{ReviewVerdict, Verification};
use crate::rsi_promotion::{
    decide_promotion, produce_release_candidate, rejects_target_install, PromotionAttempt,
    PromotionDecision, PromotionPath, ALLOWED_PROMOTERS,
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

#[test]
fn vc_201_018_passing_experiment_yields_reviewable_rc() {
    let rc = produce_release_candidate(&verified(), "e1", "sha256:abc", "evidence-v1").unwrap();
    let ok = decide_promotion(&PromotionAttempt {
        candidate: rc,
        via: PromotionPath::SusiRelease,
    });
    assert_eq!(ok, PromotionDecision::AcceptedReviewable);
    let sync = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate(&verified(), "e1", "sha256:abc", "evidence-v1")
            .unwrap(),
        via: PromotionPath::ReleaseSyncScript,
    });
    assert_eq!(sync, PromotionDecision::AcceptedReviewable);
    assert!(ALLOWED_PROMOTERS.iter().any(|p| p.contains("release")));
}

#[test]
fn vc_201_018_rejects_target_bin_install_into_susi_home() {
    let root = std::env::temp_dir().join("susi-vc-201-018");
    let bad = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate(&verified(), "e2", "d", "e").unwrap(),
        via: PromotionPath::DirectTargetInstall,
    });
    assert!(matches!(bad, PromotionDecision::Rejected(_)));
    assert!(rejects_target_install(
        &root.join("repo/target/release/susi"),
        &root.join("home/.susi/bin/susi"),
    ));
    assert!(!rejects_target_install(
        &root.join("repo/target/release/susi"),
        &root.join("repo/target/debug/susi"),
    ));
}

#[test]
fn vc_201_018_self_build_requires_verified_local_production_promotion() {
    let brief = crate::susi_core::self_build::BRIEF;
    for requirement in [
        "Debug and optimized candidates stay separate from the installed production",
        "Never copy an unqualified candidate over production",
        "Production promotion requires exact",
        "source/lock/toolchain/target evidence",
        "executable smoke tests",
        "artifact digest",
        "atomic replacement",
        "health checks",
        "tested rollback to the previous verified",
    ] {
        assert!(
            brief.contains(requirement),
            "delegated production contract lost: {requirement}"
        );
    }
}

#[test]
fn vc_201_018_debug_and_staged_candidates_cannot_enter_production_directly() {
    let root = std::env::temp_dir().join("susi-vc-201-018");
    for source in [
        "repo/target/debug/susi",
        "repo/target/release/susi",
        "staging/susi",
    ] {
        assert!(
            rejects_target_install(&root.join(source), &root.join("home/.susi/bin/susi")),
            "direct production install admitted {source}"
        );
    }
}
