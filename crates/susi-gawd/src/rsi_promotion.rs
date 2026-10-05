//! RSI promotion bound to release-only self-build (VC-201-018).

use crate::cloud_rsi::Verification;
use crate::susi_core::self_build;
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

#[must_use]
pub fn self_build_brief_mentions_release_only() -> bool {
    self_build::BRIEF.contains("~/.susi/bin") && self_build::BRIEF.contains("release")
}
