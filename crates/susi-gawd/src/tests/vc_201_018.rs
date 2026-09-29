use crate::rsi_promotion::{
    decide_promotion, produce_release_candidate, rejects_target_install,
    self_build_brief_mentions_release_only, PromotionAttempt, PromotionDecision, PromotionPath,
    ALLOWED_PROMOTERS,
};
use std::path::Path;

#[test]
fn vc_201_018_passing_experiment_yields_reviewable_rc() {
    let rc = produce_release_candidate("e1", "sha256:abc", "evidence-v1");
    let ok = decide_promotion(&PromotionAttempt {
        candidate: rc,
        via: PromotionPath::SusiRelease,
    });
    assert_eq!(ok, PromotionDecision::AcceptedReviewable);
    let sync = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate("e1", "sha256:abc", "evidence-v1"),
        via: PromotionPath::ReleaseSyncScript,
    });
    assert_eq!(sync, PromotionDecision::AcceptedReviewable);
    assert!(ALLOWED_PROMOTERS.iter().any(|p| p.contains("release")));
}

#[test]
fn vc_201_018_rejects_target_bin_install_into_susi_home() {
    let bad = decide_promotion(&PromotionAttempt {
        candidate: produce_release_candidate("e2", "d", "e"),
        via: PromotionPath::DirectTargetInstall,
    });
    assert!(matches!(bad, PromotionDecision::Rejected(_)));
    assert!(rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("/home/u/.susi/bin/susi"),
    ));
    assert!(!rejects_target_install(
        Path::new("/repo/target/release/susi"),
        Path::new("/repo/target/debug/susi"),
    ));
    assert!(self_build_brief_mentions_release_only());
}
