//! RSI promotion bound to release-only self-build (VC-201-018).

use crate::susi_core::self_build;
use serde::{Deserialize, Serialize};
use std::path::Path;

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

/// A passing experiment yields a reviewable RC + evidence; only release paths promote.
#[must_use]
pub fn produce_release_candidate(
    experiment_id: &str,
    digest: &str,
    evidence: &str,
) -> ReleaseCandidate {
    ReleaseCandidate {
        experiment_id: experiment_id.to_string(),
        evidence_manifest: evidence.to_string(),
        artifact_digest: digest.to_string(),
    }
}

#[must_use]
pub fn decide_promotion(attempt: &PromotionAttempt) -> PromotionDecision {
    match attempt.via {
        PromotionPath::SusiRelease | PromotionPath::ReleaseSyncScript => {
            if attempt.candidate.evidence_manifest.is_empty()
                || attempt.candidate.artifact_digest.is_empty()
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

/// Reject paths that would install a target/ binary into a release home bin.
#[must_use]
pub fn rejects_target_install(src: &Path, dest_bin: &Path) -> bool {
    let src_s = src.to_string_lossy();
    let dest_s = dest_bin.to_string_lossy();
    src_s.contains("target/")
        && (dest_s.contains(".susi/bin") || dest_s.ends_with("/.susi/bin/susi"))
}

/// Allowed promotion command names (contract text).
pub const ALLOWED_PROMOTERS: &[&str] = &["susi release", "scripts/susi-release-sync.sh"];

#[must_use]
pub fn self_build_brief_mentions_release_only() -> bool {
    self_build::BRIEF.contains("~/.susi/bin") && self_build::BRIEF.contains("release")
}
