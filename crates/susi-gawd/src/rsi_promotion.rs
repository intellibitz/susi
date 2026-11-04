//! RSI promotion bound to release-only self-build (VC-201-018).

use crate::cloud_rsi::Verification;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    pub experiment_id: String,
    pub evidence_manifest: String,
    pub artifact_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionAttempt {
    pub candidate: ReleaseCandidate,
    pub via: PromotionPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionPath {
    SusiRelease,
    ReleaseSyncScript,
    /// Forbidden: copying target/ into ~/.susi/bin
    DirectTargetInstall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionDecision {
    AcceptedReviewable,
    Rejected(&'static str),
}

/// A passing experiment yields a reviewable RC + evidence; only release
/// paths promote. Requires the experiment's own independent verification
/// outcome, not just an id string — an experiment that never ran, failed,
/// or was never independently reviewed can no longer mint a candidate.
pub fn produce_release_candidate(
    verification: &Verification,
    experiment_id: &str,
    digest: &str,
    evidence: &str,
) -> Result<ReleaseCandidate, &'static str> {
    if !verification.verified {
        return Err("experiment did not pass independent verification");
    }
    Ok(ReleaseCandidate {
        experiment_id: experiment_id.to_string(),
        evidence_manifest: evidence.to_string(),
        artifact_digest: digest.to_string(),
    })
}

#[must_use]
pub fn decide_promotion(attempt: &PromotionAttempt) -> PromotionDecision {
    match attempt.via {
        PromotionPath::SusiRelease | PromotionPath::ReleaseSyncScript => {
            // A whitespace-only string is not evidence: `is_empty()` alone
            // let " " satisfy this check.
            if attempt.candidate.evidence_manifest.trim().is_empty()
                || attempt.candidate.artifact_digest.trim().is_empty()
            {
                PromotionDecision::Rejected("missing evidence or digest")
            } else {
                PromotionDecision::AcceptedReviewable
            }
        }
        PromotionPath::DirectTargetInstall => {
            PromotionDecision::Rejected("target/ install into ~/.susi/bin is forbidden")
        }
    }
}

/// Reject a direct install whose destination is (or could be, once
/// resolved) the release home's `~/.susi/bin` — component-wise, not a
/// substring match that a differently-spelled but equivalent path
/// evades. `src` is intentionally not consulted: staging a dev binary
/// through a neutral path (e.g. `/tmp`) must not let it slip past a check
/// that only looked for `target/` in the source. A *relative* `dest_bin`
/// cannot be proven safe without the caller's cwd, so it is treated as
/// forbidden rather than waved through.
#[must_use]
pub fn rejects_target_install(src: &Path, dest_bin: &Path) -> bool {
    let _ = src; // the destination alone governs; see doc comment.
    if dest_bin.is_relative() {
        return true;
    }
    let components: Vec<Component<'_>> = dest_bin.components().collect();
    components.windows(2).any(|pair| {
        pair[0].as_os_str().eq_ignore_ascii_case(".susi")
            && pair[1].as_os_str().eq_ignore_ascii_case("bin")
    })
}

/// Allowed promotion command names (contract text).
pub const ALLOWED_PROMOTERS: &[&str] = &["susi release", "scripts/susi-release-sync.sh"];

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    fn sample_verification(verified: bool) -> Verification {
        Verification {
            verified,
            review: None,
            reviewer: None,
            gates: Vec::new(),
            regressions: Vec::new(),
            fabricated: Vec::new(),
            failover: None,
            reasons: Vec::new(),
        }
    }

    #[test]
    fn produce_release_candidate_requires_independent_verification() {
        let unverified = sample_verification(false);
        assert!(produce_release_candidate(&unverified, "exp-1", "sha256:abc", "manifest").is_err());

        let verified = sample_verification(true);
        let rc = produce_release_candidate(&verified, "exp-1", "sha256:abc", "manifest").unwrap();
        assert_eq!(rc.experiment_id, "exp-1");
        assert_eq!(rc.artifact_digest, "sha256:abc");
    }

    #[test]
    fn decide_promotion_accepts_release_paths_with_non_empty_evidence() {
        let rc = ReleaseCandidate {
            experiment_id: "exp-2".to_string(),
            evidence_manifest: "manifest.json".to_string(),
            artifact_digest: "sha256:def".to_string(),
        };
        assert_eq!(
            decide_promotion(&PromotionAttempt {
                candidate: rc.clone(),
                via: PromotionPath::SusiRelease,
            }),
            PromotionDecision::AcceptedReviewable
        );
        assert_eq!(
            decide_promotion(&PromotionAttempt {
                candidate: rc,
                via: PromotionPath::ReleaseSyncScript,
            }),
            PromotionDecision::AcceptedReviewable
        );
    }

    #[test]
    fn decide_promotion_rejects_direct_install_and_whitespace_evidence() {
        let rc = ReleaseCandidate {
            experiment_id: "exp-3".to_string(),
            evidence_manifest: "   ".to_string(),
            artifact_digest: "sha256:def".to_string(),
        };
        assert_eq!(
            decide_promotion(&PromotionAttempt {
                candidate: rc.clone(),
                via: PromotionPath::SusiRelease,
            }),
            PromotionDecision::Rejected("missing evidence or digest")
        );
        assert_eq!(
            decide_promotion(&PromotionAttempt {
                candidate: ReleaseCandidate {
                    evidence_manifest: "ok".to_string(),
                    ..rc
                },
                via: PromotionPath::DirectTargetInstall,
            }),
            PromotionDecision::Rejected("target/ install into ~/.susi/bin is forbidden")
        );
    }

    #[test]
    fn rejects_target_install_blocks_relative_and_susi_bin_destinations() {
        assert!(rejects_target_install(
            Path::new("/tmp/susi"),
            Path::new("relative/bin/susi")
        ));
        let home_bin = if cfg!(windows) {
            Path::new(r"C:\Users\alice\.susi\bin\susi.exe")
        } else {
            Path::new("/home/alice/.susi/bin/susi")
        };
        assert!(rejects_target_install(Path::new("/tmp/susi"), home_bin));

        let staging = if cfg!(windows) {
            Path::new(r"C:\Temp\staging\susi.exe")
        } else {
            Path::new("/tmp/staging/susi")
        };
        assert!(!rejects_target_install(Path::new("/tmp/susi"), staging));
    }

    #[test]
    fn decide_promotion_rejects_blank_artifact_digest_and_accepts_susi_release() {
        let blank_digest = ReleaseCandidate {
            experiment_id: "exp-4".to_string(),
            evidence_manifest: "manifest-ok".to_string(),
            artifact_digest: "   ".to_string(),
        };
        assert_eq!(
            decide_promotion(&PromotionAttempt {
                candidate: blank_digest.clone(),
                via: PromotionPath::SusiRelease,
            }),
            PromotionDecision::Rejected("missing evidence or digest")
        );
        let valid = ReleaseCandidate {
            artifact_digest: "sha256:abc123".to_string(),
            ..blank_digest
        };
        assert_eq!(
            decide_promotion(&PromotionAttempt {
                candidate: valid,
                via: PromotionPath::SusiRelease,
            }),
            PromotionDecision::AcceptedReviewable
        );
    }
}
