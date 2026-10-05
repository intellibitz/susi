//! Deliver validated cloud-assisted improvements through PR + release gates
//! (T-CODEX-21 / VC-201-018).
//!
//! A candidate that has already passed independent review and measured
//! validation (`Verification.verified` + lifecycle `Phase::Promoted`) is
//! promoted into the reviewable delivery path:
//!
//! 1. Sync feature branch with `origin/main`.
//! 2. Commit on the feature branch with a `Task:` trailer and bound evidence
//!    (artifact digest, reviewer provenance, acceptance receipts).
//! 3. Rerun required gates after any material integration merge.
//! 4. Close the task only when acceptance passes.
//! 5. Open/merge via the existing PR auto-merge workflow — never direct
//!    `main` pushes.
//! 6. Promote installed Susi **only** through the verified tagged release
//!    path (`susi release` / `scripts/susi-release-sync.sh`). Dev `target/`
//!    binaries cannot be installed into `~/.susi/bin`.
//!
//! States are reported distinctly: `Proposed → Implemented → Validated →
//! Merged → Released`. Rejected candidates cannot advance.
//!
//! All git/release effects go through injected boundaries so tests stay
//! hermetic (Mandate 52) — no real `~/.susi`, no live network git.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cloud_rsi::Verification;
use crate::cloud_rsi_lifecycle::Phase;
use crate::rsi_promotion::{
    decide_promotion, produce_release_candidate, rejects_target_install, PromotionAttempt,
    PromotionDecision, PromotionPath, ALLOWED_PROMOTERS,
};

/// Distinct delivery states reported to operators and the lifecycle ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    /// Proposal minted; not yet implemented.
    Proposed,
    /// Edits applied in a worktree.
    Implemented,
    /// Independent review + measured gates passed.
    Validated,
    /// Feature-branch commit merged via PR (not a direct main push).
    Merged,
    /// Tagged release promoted into the installed home via the allowed path.
    Released,
    /// Terminal refusal — cannot be promoted.
    Rejected,
}

impl DeliveryState {
    /// Human label used in manifests / receipts.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DeliveryState::Proposed => "proposed",
            DeliveryState::Implemented => "implemented",
            DeliveryState::Validated => "validated",
            DeliveryState::Merged => "merged",
            DeliveryState::Released => "released",
            DeliveryState::Rejected => "rejected",
        }
    }
}

/// Evidence bound into the delivered commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryBinding {
    /// Queue task id (must appear as `Task: <id>` trailer).
    pub task_id: String,
    /// Candidate / experiment id.
    pub candidate_id: String,
    /// Content digest of the delivered artifact tree.
    pub artifact_digest: String,
    /// Independent reviewer opaque id (≠ implementer).
    pub reviewer: String,
    /// Implementer opaque id.
    pub implementer: String,
    /// Acceptance / gate receipts that justified validation.
    pub acceptance_receipts: Vec<String>,
    /// Lifecycle phase at delivery time — must be `Promoted`.
    pub lifecycle_phase: Phase,
}

/// A commit authored on the feature branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchCommit {
    /// Feature branch name (never `main`).
    pub branch: String,
    /// Commit SHA produced by the fake/real git boundary.
    pub sha: String,
    /// Full commit message including the Task trailer.
    pub message: String,
    /// Bound evidence serialized into the commit note / body.
    pub binding: DeliveryBinding,
}

/// Result of attempting delivery through PR + release gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReport {
    /// Latest state reached.
    pub state: DeliveryState,
    /// Feature-branch commit (set once Merged path starts).
    pub commit: Option<BranchCommit>,
    /// PR number opened for auto-merge (hermetic fake may invent one).
    pub pr_number: Option<u64>,
    /// Release tag when Released.
    pub release_tag: Option<String>,
    /// Why delivery stopped (if Rejected or blocked mid-flight).
    pub reasons: Vec<String>,
    /// Gate results re-run after merging origin/main.
    pub post_merge_gates: Vec<(String, bool)>,
    /// Whether the task was closed after acceptance passed.
    pub task_closed: bool,
}

/// Why a delivery step was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// Candidate is not validated / lifecycle not Promoted.
    NotReady(String),
    /// Attempted to commit on `main` or the primary checkout.
    ForbiddenBranch(String),
    /// Commit message missing `Task: <id>` trailer.
    MissingTaskTrailer,
    /// Post-merge gates failed — candidate cannot close or merge.
    GatesFailed(Vec<String>),
    /// Direct main push / unauthorized release / target install.
    ForbiddenPromotion(String),
    /// Injected boundary failure.
    Boundary(String),
}

/// Hermetic git boundary — production wraps real git; tests use fakes.
pub trait GitBoundary: Send + Sync {
    /// Current branch name.
    fn current_branch(&self) -> String;
    /// Merge `origin/main` into the current feature branch.
    fn merge_origin_main(&mut self) -> Result<(), String>;
    /// Create a commit on the current branch; returns SHA.
    fn commit(&mut self, message: &str) -> Result<String, String>;
    /// Push the feature branch (never `main`).
    fn push_feature(&mut self, branch: &str) -> Result<(), String>;
    /// Open a PR for auto-merge; returns PR number.
    fn open_pr(&mut self, branch: &str, title: &str) -> Result<u64, String>;
    /// Record that auto-merge completed for `pr`.
    fn note_merged(&mut self, pr: u64, sha: &str) -> Result<(), String>;
}

/// Hermetic release boundary — production wraps `susi admin release` + sync.
pub trait ReleaseBoundary: Send + Sync {
    /// Cut/promote a tagged release via an allowed path only.
    fn promote_tagged(
        &mut self,
        path: PromotionPath,
        tag: &str,
        digest: &str,
        evidence: &str,
    ) -> Result<(), String>;
    /// Attempt to install a binary — tests prove target→~/.susi/bin is refused.
    fn try_install_binary(&mut self, src: &Path, dest_bin: &Path) -> Result<(), String>;
}

/// Gate runner used after material integration merges.
pub trait DeliveryGates: Send + Sync {
    /// Run named gates; return (name, ok) pairs.
    fn rerun(&self, worktree: &Path, gates: &[(String, Vec<String>)]) -> Vec<(String, bool)>;
}

/// Task-queue close seam (production: `susi tasks close`).
pub trait TaskCloser: Send + Sync {
    /// Close `task_id` only when acceptance already passed.
    fn close_passing(&mut self, task_id: &str) -> Result<(), String>;
}

/// Policy for the delivery stage.
#[derive(Debug, Clone)]
pub struct DeliveryPolicy {
    /// Feature branch that will carry the commit (must not be `main`).
    pub feature_branch: String,
    /// Gates re-run after merging origin/main (name, argv).
    pub post_merge_gates: Vec<(String, Vec<String>)>,
    /// Optional release tag to cut after merge (None = stop at Merged).
    pub release_tag: Option<String>,
}

impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self {
            feature_branch: "rsi/candidate".into(),
            post_merge_gates: vec![
                ("fmt".into(), vec!["cargo".into(), "fmt".into()]),
                ("clippy".into(), vec!["cargo".into(), "clippy".into()]),
                ("test".into(), vec!["cargo".into(), "test".into()]),
            ],
            release_tag: None,
        }
    }
}

/// Inputs required to attempt delivery.
pub struct DeliveryRequest<'a> {
    /// Binding evidence.
    pub binding: DeliveryBinding,
    /// Independent verification outcome — must be `verified`.
    pub verification: &'a Verification,
    /// Workspace root (hermetic temp dir in tests).
    pub worktree: &'a Path,
}

/// Build the commit message with required Task trailer and bound evidence.
#[must_use]
pub fn commit_message(binding: &DeliveryBinding, summary: &str) -> String {
    format!(
        "{summary}\n\n\
         Candidate: {cid}\n\
         Artifact: {digest}\n\
         Reviewer: {rev}\n\
         Implementer: {imp}\n\
         Receipts:\n{receipts}\n\n\
         Task: {tid}\n",
        summary = summary,
        cid = binding.candidate_id,
        digest = binding.artifact_digest,
        rev = binding.reviewer,
        imp = binding.implementer,
        receipts = binding
            .acceptance_receipts
            .iter()
            .map(|r| format!("- {r}"))
            .collect::<Vec<_>>()
            .join("\n"),
        tid = binding.task_id,
    )
}

/// True iff `message` ends with (or contains) the required Task trailer.
#[must_use]
pub fn has_task_trailer(message: &str, task_id: &str) -> bool {
    let needle = format!("Task: {task_id}");
    message.lines().any(|l| l.trim() == needle)
}

/// Map lifecycle + verification into the reported delivery state *before*
/// any git/release action.
#[must_use]
pub fn state_from_inputs(phase: Phase, verified: bool) -> DeliveryState {
    match (phase, verified) {
        (Phase::Proposed | Phase::Claimed, _) => DeliveryState::Proposed,
        (
            Phase::Implementing | Phase::Implemented | Phase::Reviewing | Phase::Validating,
            false,
        ) => DeliveryState::Implemented,
        (Phase::Implementing | Phase::Implemented | Phase::Reviewing | Phase::Validating, true) => {
            DeliveryState::Validated
        }
        (Phase::Promoted, true) => DeliveryState::Validated,
        (Phase::Promoted, false) | (Phase::Rejected | Phase::Cancelled, _) => {
            DeliveryState::Rejected
        }
    }
}

fn preflight(req: &DeliveryRequest<'_>, policy: &DeliveryPolicy) -> Result<(), DeliveryError> {
    if policy.feature_branch == "main" || policy.feature_branch.is_empty() {
        return Err(DeliveryError::ForbiddenBranch(
            policy.feature_branch.clone(),
        ));
    }
    if req.binding.lifecycle_phase != Phase::Promoted {
        return Err(DeliveryError::NotReady(format!(
            "lifecycle phase {:?} — need Promoted",
            req.binding.lifecycle_phase
        )));
    }
    if !req.verification.verified {
        return Err(DeliveryError::NotReady(
            "verification.verified is false".into(),
        ));
    }
    let Some(reviewer) = req.verification.reviewer.as_deref() else {
        return Err(DeliveryError::NotReady(
            "missing reviewer provenance".into(),
        ));
    };
    if reviewer == req.binding.implementer || reviewer != req.binding.reviewer {
        return Err(DeliveryError::NotReady(
            "reviewer provenance does not match binding".into(),
        ));
    }
    if req.binding.artifact_digest.is_empty() || req.binding.acceptance_receipts.is_empty() {
        return Err(DeliveryError::NotReady(
            "artifact digest and acceptance receipts are required".into(),
        ));
    }
    if req.verification.gates.iter().any(|(_, ok)| !ok) {
        return Err(DeliveryError::NotReady(
            "verification recorded a failing gate".into(),
        ));
    }
    Ok(())
}

/// Deliver a validated candidate through feature-branch commit → PR merge →
/// optional tagged release. Rejected / unverified candidates cannot advance.
#[allow(clippy::too_many_arguments)] // every parameter is an injected seam
pub fn deliver_candidate(
    req: &DeliveryRequest<'_>,
    policy: &DeliveryPolicy,
    git: &mut dyn GitBoundary,
    release: &mut dyn ReleaseBoundary,
    gates: &dyn DeliveryGates,
    closer: &mut dyn TaskCloser,
) -> Result<DeliveryReport, DeliveryError> {
    let mut report = DeliveryReport {
        state: state_from_inputs(req.binding.lifecycle_phase, req.verification.verified),
        commit: None,
        pr_number: None,
        release_tag: None,
        reasons: Vec::new(),
        post_merge_gates: Vec::new(),
        task_closed: false,
    };

    if let Err(e) = preflight(req, policy) {
        report.state = DeliveryState::Rejected;
        report.reasons.push(match &e {
            DeliveryError::NotReady(s)
            | DeliveryError::ForbiddenBranch(s)
            | DeliveryError::ForbiddenPromotion(s)
            | DeliveryError::Boundary(s) => s.clone(),
            DeliveryError::MissingTaskTrailer => "missing Task trailer".into(),
            DeliveryError::GatesFailed(g) => format!("gates failed: {}", g.join(", ")),
        });
        return Ok(report);
    }

    // 1. Sync with origin/main on the feature branch.
    let branch = git.current_branch();
    if branch == "main" {
        report.state = DeliveryState::Rejected;
        report
            .reasons
            .push("refusing to deliver from main — use a feature branch".into());
        return Ok(report);
    }
    if branch != policy.feature_branch {
        // Soft check: allow the boundary's current branch when it matches policy
        // after an explicit switch; otherwise refuse.
        report.state = DeliveryState::Rejected;
        report.reasons.push(format!(
            "current branch `{branch}` != policy feature branch `{}`",
            policy.feature_branch
        ));
        return Ok(report);
    }
    git.merge_origin_main().map_err(DeliveryError::Boundary)?;

    // 2. Rerun gates after the material integration merge.
    report.post_merge_gates = gates.rerun(req.worktree, &policy.post_merge_gates);
    let failed: Vec<String> = report
        .post_merge_gates
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n.clone())
        .collect();
    if !failed.is_empty() {
        report.state = DeliveryState::Rejected;
        report
            .reasons
            .push(format!("post-merge gates failed: {}", failed.join(", ")));
        return Ok(report);
    }

    // 3. Commit on the feature branch with Task trailer + bound evidence.
    let msg = commit_message(
        &req.binding,
        &format!(
            "Deliver validated cloud RSI candidate {}",
            req.binding.candidate_id
        ),
    );
    if !has_task_trailer(&msg, &req.binding.task_id) {
        return Err(DeliveryError::MissingTaskTrailer);
    }
    let sha = git.commit(&msg).map_err(DeliveryError::Boundary)?;
    let commit = BranchCommit {
        branch: policy.feature_branch.clone(),
        sha: sha.clone(),
        message: msg,
        binding: req.binding.clone(),
    };

    // 4. Push feature branch + open PR (auto-merge path — never push main).
    git.push_feature(&policy.feature_branch)
        .map_err(DeliveryError::Boundary)?;
    let pr = git
        .open_pr(
            &policy.feature_branch,
            &format!("Deliver {}", req.binding.candidate_id),
        )
        .map_err(DeliveryError::Boundary)?;
    git.note_merged(pr, &sha).map_err(DeliveryError::Boundary)?;
    report.pr_number = Some(pr);
    report.commit = Some(commit);
    report.state = DeliveryState::Merged;

    // 5. Close the task only after acceptance/gates passed.
    closer
        .close_passing(&req.binding.task_id)
        .map_err(DeliveryError::Boundary)?;
    report.task_closed = true;

    // 6. Optional tagged release via allowed promoters only.
    if let Some(tag) = &policy.release_tag {
        let evidence = format!(
            "pr={pr};sha={sha};reviewer={};receipts={}",
            req.binding.reviewer,
            req.binding.acceptance_receipts.join("|")
        );
        let rc = match produce_release_candidate(
            req.verification,
            &req.binding.candidate_id,
            &req.binding.artifact_digest,
            &evidence,
        ) {
            Ok(rc) => rc,
            Err(why) => {
                report.reasons.push(format!("release refused: {why}"));
                return Ok(report);
            }
        };
        let decision = decide_promotion(&PromotionAttempt {
            candidate: rc.clone(),
            via: PromotionPath::SusiRelease,
        });
        if decision != PromotionDecision::AcceptedReviewable {
            report
                .reasons
                .push(format!("release refused: {decision:?}"));
            return Ok(report);
        }
        release
            .promote_tagged(
                PromotionPath::SusiRelease,
                tag,
                &req.binding.artifact_digest,
                &evidence,
            )
            .map_err(DeliveryError::Boundary)?;
        report.release_tag = Some(tag.clone());
        report.state = DeliveryState::Released;
        let _ = rc; // evidence already embedded
        let _ = ALLOWED_PROMOTERS;
    }

    Ok(report)
}

/// Attempt a forbidden install path — always refused when src is under
/// `target/` and dest is `~/.susi/bin`.
pub fn refuse_dev_binary_install(
    release: &mut dyn ReleaseBoundary,
    src: &Path,
    dest_bin: &Path,
) -> Result<(), DeliveryError> {
    if rejects_target_install(src, dest_bin) {
        return Err(DeliveryError::ForbiddenPromotion(
            "target/ install into ~/.susi/bin is forbidden".into(),
        ));
    }
    release
        .try_install_binary(src, dest_bin)
        .map_err(DeliveryError::Boundary)
}

/// Attempt promotion via an explicit path — DirectTargetInstall is always
/// rejected; only SusiRelease / ReleaseSyncScript may succeed, and only
/// when `verification` actually passed.
#[allow(clippy::too_many_arguments)] // every parameter is an injected seam
pub fn promote_via(
    release: &mut dyn ReleaseBoundary,
    path: PromotionPath,
    verification: &Verification,
    tag: &str,
    digest: &str,
    evidence: &str,
) -> Result<(), DeliveryError> {
    let rc = produce_release_candidate(verification, tag, digest, evidence)
        .map_err(|why| DeliveryError::ForbiddenPromotion(why.into()))?;
    match decide_promotion(&PromotionAttempt {
        candidate: rc,
        via: path,
    }) {
        PromotionDecision::AcceptedReviewable => release
            .promote_tagged(path, tag, digest, evidence)
            .map_err(DeliveryError::Boundary),
        PromotionDecision::Rejected(why) => Err(DeliveryError::ForbiddenPromotion(why.into())),
    }
}

// ---------------------------------------------------------------------------
// Hermetic fakes used by tests (and available for higher-level harnesses).
// ---------------------------------------------------------------------------

/// In-memory git boundary for hermetic delivery tests.
#[derive(Debug, Default)]
pub struct FakeGit {
    /// Current branch.
    pub branch: String,
    /// Commits: sha → message.
    pub commits: BTreeMap<String, String>,
    /// Whether origin/main was merged.
    pub merged_main: bool,
    /// Pushed branches.
    pub pushed: Vec<String>,
    /// Opened PRs: number → (branch, title).
    pub prs: BTreeMap<u64, (String, String)>,
    /// Merged PR numbers.
    pub merged_prs: Vec<(u64, String)>,
    /// Next SHA counter.
    next_sha: u64,
    /// Next PR number.
    next_pr: u64,
    /// Force merge_origin_main to fail.
    pub fail_merge: bool,
}

impl FakeGit {
    /// Feature-branch fake starting ready to commit.
    #[must_use]
    pub fn on_branch(branch: &str) -> Self {
        Self {
            branch: branch.into(),
            next_sha: 1,
            next_pr: 100,
            ..Self::default()
        }
    }
}

impl GitBoundary for FakeGit {
    fn current_branch(&self) -> String {
        self.branch.clone()
    }
    fn merge_origin_main(&mut self) -> Result<(), String> {
        if self.fail_merge {
            return Err("merge conflict".into());
        }
        self.merged_main = true;
        Ok(())
    }
    fn commit(&mut self, message: &str) -> Result<String, String> {
        if self.branch == "main" {
            return Err("refusing commit on main".into());
        }
        let sha = format!("sha{:04}", self.next_sha);
        self.next_sha += 1;
        self.commits.insert(sha.clone(), message.to_string());
        Ok(sha)
    }
    fn push_feature(&mut self, branch: &str) -> Result<(), String> {
        if branch == "main" {
            return Err("refusing push to main".into());
        }
        self.pushed.push(branch.to_string());
        Ok(())
    }
    fn open_pr(&mut self, branch: &str, title: &str) -> Result<u64, String> {
        let n = self.next_pr;
        self.next_pr += 1;
        self.prs.insert(n, (branch.to_string(), title.to_string()));
        Ok(n)
    }
    fn note_merged(&mut self, pr: u64, sha: &str) -> Result<(), String> {
        self.merged_prs.push((pr, sha.to_string()));
        Ok(())
    }
}

/// In-memory release boundary.
#[derive(Debug, Default)]
pub struct FakeRelease {
    /// Successful promotions: (path, tag, digest).
    pub promoted: Vec<(PromotionPath, String, String)>,
    /// Install attempts that were allowed through (should stay empty for
    /// target→~/.susi/bin in tests).
    pub installs: Vec<(PathBuf, PathBuf)>,
}

impl ReleaseBoundary for FakeRelease {
    fn promote_tagged(
        &mut self,
        path: PromotionPath,
        tag: &str,
        digest: &str,
        _evidence: &str,
    ) -> Result<(), String> {
        match path {
            PromotionPath::SusiRelease | PromotionPath::ReleaseSyncScript => {
                self.promoted
                    .push((path, tag.to_string(), digest.to_string()));
                Ok(())
            }
            PromotionPath::DirectTargetInstall => {
                Err("direct target install is not a release path".into())
            }
        }
    }
    fn try_install_binary(&mut self, src: &Path, dest_bin: &Path) -> Result<(), String> {
        if rejects_target_install(src, dest_bin) {
            return Err("target/ install into ~/.susi/bin is forbidden".into());
        }
        self.installs
            .push((src.to_path_buf(), dest_bin.to_path_buf()));
        Ok(())
    }
}

/// Scripted post-merge gates.
#[derive(Debug, Default)]
pub struct FakeDeliveryGates {
    /// name → pass/fail (missing defaults to true).
    pub results: BTreeMap<String, bool>,
}

impl DeliveryGates for FakeDeliveryGates {
    fn rerun(&self, _worktree: &Path, gates: &[(String, Vec<String>)]) -> Vec<(String, bool)> {
        gates
            .iter()
            .map(|(n, _)| (n.clone(), *self.results.get(n).unwrap_or(&true)))
            .collect()
    }
}

/// Records task closes.
#[derive(Debug, Default)]
pub struct FakeCloser {
    /// Closed task ids.
    pub closed: Vec<String>,
}

impl TaskCloser for FakeCloser {
    fn close_passing(&mut self, task_id: &str) -> Result<(), String> {
        self.closed.push(task_id.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_rsi::{ReviewVerdict, Verification};

    fn verified() -> Verification {
        Verification {
            verified: true,
            review: Some(ReviewVerdict {
                verdict: "approve".into(),
                reasons: vec!["ok".into()],
            }),
            reviewer: Some("rev-opaque".into()),
            gates: vec![("accept".into(), true), ("clippy".into(), true)],
            regressions: Vec::new(),
            fabricated: Vec::new(),
            failover: None,
            reasons: Vec::new(),
        }
    }

    fn unverified() -> Verification {
        let mut v = verified();
        v.verified = false;
        v.reasons.push("gate failed: clippy".into());
        v.gates = vec![("clippy".into(), false)];
        v
    }

    fn binding() -> DeliveryBinding {
        DeliveryBinding {
            task_id: "T-CODEX-21".into(),
            candidate_id: "cand-1".into(),
            artifact_digest: "sha256:deadbeef".into(),
            reviewer: "rev-opaque".into(),
            implementer: "impl-opaque".into(),
            acceptance_receipts: vec!["cargo test -> 0".into(), "clippy -> 0".into()],
            lifecycle_phase: Phase::Promoted,
        }
    }

    fn policy() -> DeliveryPolicy {
        DeliveryPolicy {
            feature_branch: "rsi/cand-1".into(),
            post_merge_gates: vec![
                ("fmt".into(), vec!["cargo".into(), "fmt".into()]),
                ("test".into(), vec!["cargo".into(), "test".into()]),
            ],
            release_tag: Some("v0.20.1-rsi".into()),
        }
    }

    #[allow(clippy::too_many_arguments)] // test harness packs injected fakes
    fn run(
        v: &Verification,
        b: DeliveryBinding,
        git: &mut FakeGit,
        release: &mut FakeRelease,
        gates: &FakeDeliveryGates,
        closer: &mut FakeCloser,
        pol: &DeliveryPolicy,
    ) -> DeliveryReport {
        let wt = std::env::temp_dir();
        let req = DeliveryRequest {
            binding: b,
            verification: v,
            worktree: &wt,
        };
        deliver_candidate(&req, pol, git, release, gates, closer).expect("deliver")
    }

    #[test]
    fn cloud_rsi_delivery_validated_candidate_merges_and_releases() {
        let v = verified();
        let mut git = FakeGit::on_branch("rsi/cand-1");
        let mut release = FakeRelease::default();
        let gates = FakeDeliveryGates::default();
        let mut closer = FakeCloser::default();
        let report = run(
            &v,
            binding(),
            &mut git,
            &mut release,
            &gates,
            &mut closer,
            &policy(),
        );
        assert_eq!(report.state, DeliveryState::Released);
        assert_eq!(report.state.label(), "released");
        assert!(report.task_closed);
        assert_eq!(report.pr_number, Some(100));
        assert_eq!(report.release_tag.as_deref(), Some("v0.20.1-rsi"));
        let commit = report.commit.expect("commit");
        assert!(has_task_trailer(&commit.message, "T-CODEX-21"));
        assert_eq!(commit.binding.artifact_digest, "sha256:deadbeef");
        assert_eq!(commit.binding.reviewer, "rev-opaque");
        assert!(git.merged_main);
        assert_eq!(git.pushed, vec!["rsi/cand-1".to_string()]);
        assert_eq!(release.promoted.len(), 1);
        assert_eq!(closer.closed, vec!["T-CODEX-21".to_string()]);
        // Distinct earlier states are still expressible.
        assert_eq!(
            state_from_inputs(Phase::Proposed, false),
            DeliveryState::Proposed
        );
        assert_eq!(
            state_from_inputs(Phase::Implemented, false),
            DeliveryState::Implemented
        );
        assert_eq!(
            state_from_inputs(Phase::Promoted, true),
            DeliveryState::Validated
        );
    }

    #[test]
    fn cloud_rsi_delivery_unverified_candidate_cannot_promote() {
        let v = unverified();
        let mut git = FakeGit::on_branch("rsi/cand-1");
        let mut release = FakeRelease::default();
        let gates = FakeDeliveryGates::default();
        let mut closer = FakeCloser::default();
        let report = run(
            &v,
            binding(),
            &mut git,
            &mut release,
            &gates,
            &mut closer,
            &policy(),
        );
        assert_eq!(report.state, DeliveryState::Rejected);
        assert!(report.commit.is_none());
        assert!(!report.task_closed);
        assert!(release.promoted.is_empty());
        assert!(git.commits.is_empty());
    }

    #[test]
    fn cloud_rsi_delivery_rejects_main_branch_and_missing_trailer_inputs() {
        let v = verified();
        let mut git = FakeGit::on_branch("main");
        let mut release = FakeRelease::default();
        let gates = FakeDeliveryGates::default();
        let mut closer = FakeCloser::default();
        let mut pol = policy();
        // Policy still names a feature branch, but git is on main → refuse.
        let report = run(
            &v,
            binding(),
            &mut git,
            &mut release,
            &gates,
            &mut closer,
            &pol,
        );
        assert_eq!(report.state, DeliveryState::Rejected);
        assert!(report.reasons.iter().any(|r| r.contains("main")));

        // Policy that tries to deliver *as* main is also refused at preflight.
        pol.feature_branch = "main".into();
        let mut git2 = FakeGit::on_branch("main");
        let report2 = run(
            &v,
            binding(),
            &mut git2,
            &mut release,
            &gates,
            &mut closer,
            &pol,
        );
        assert_eq!(report2.state, DeliveryState::Rejected);
        assert!(has_task_trailer(
            &commit_message(&binding(), "x"),
            "T-CODEX-21"
        ));
        assert!(!has_task_trailer("no trailer here", "T-CODEX-21"));
    }

    #[test]
    fn cloud_rsi_delivery_post_merge_gate_failure_blocks_close() {
        let v = verified();
        let mut git = FakeGit::on_branch("rsi/cand-1");
        let mut release = FakeRelease::default();
        let mut gates = FakeDeliveryGates::default();
        gates.results.insert("test".into(), false);
        let mut closer = FakeCloser::default();
        let report = run(
            &v,
            binding(),
            &mut git,
            &mut release,
            &gates,
            &mut closer,
            &policy(),
        );
        assert_eq!(report.state, DeliveryState::Rejected);
        assert!(!report.task_closed);
        assert!(report.reasons.iter().any(|r| r.contains("post-merge")));
        assert!(git.commits.is_empty());
        assert!(release.promoted.is_empty());
    }

    #[test]
    fn cloud_rsi_delivery_rejects_dev_binary_hot_swap_and_direct_target() {
        let mut release = FakeRelease::default();
        let src = Path::new("/repo/target/release/susi");
        let dest = Path::new("/home/u/.susi/bin/susi");
        let err = refuse_dev_binary_install(&mut release, src, dest).unwrap_err();
        assert!(matches!(err, DeliveryError::ForbiddenPromotion(_)));
        assert!(release.installs.is_empty());

        let bad = promote_via(
            &mut release,
            PromotionPath::DirectTargetInstall,
            &verified(),
            "v0.1.0",
            "digest",
            "evidence",
        );
        assert!(matches!(bad, Err(DeliveryError::ForbiddenPromotion(_))));

        // Allowed path succeeds on the fake, given real passing verification.
        promote_via(
            &mut release,
            PromotionPath::ReleaseSyncScript,
            &verified(),
            "v0.20.1",
            "digest",
            "evidence",
        )
        .unwrap();
        assert_eq!(release.promoted.len(), 1);
        assert!(ALLOWED_PROMOTERS.contains(&"scripts/susi-release-sync.sh"));
    }

    #[test]
    fn cloud_rsi_delivery_binds_receipts_and_reviewer_to_commit() {
        let v = verified();
        let mut git = FakeGit::on_branch("rsi/cand-1");
        let mut release = FakeRelease::default();
        let gates = FakeDeliveryGates::default();
        let mut closer = FakeCloser::default();
        let mut pol = policy();
        pol.release_tag = None; // stop at Merged
        let report = run(
            &v,
            binding(),
            &mut git,
            &mut release,
            &gates,
            &mut closer,
            &pol,
        );
        assert_eq!(report.state, DeliveryState::Merged);
        let c = report.commit.expect("commit");
        assert!(c.message.contains("Reviewer: rev-opaque"));
        assert!(c.message.contains("Artifact: sha256:deadbeef"));
        assert!(c.message.contains("- cargo test -> 0"));
        assert_eq!(c.binding.acceptance_receipts.len(), 2);
        assert!(release.promoted.is_empty());
        assert_eq!(git.merged_prs, vec![(100, c.sha.clone())]);
    }

    #[test]
    fn cloud_rsi_delivery_non_promoted_lifecycle_is_rejected() {
        let v = verified();
        let mut b = binding();
        b.lifecycle_phase = Phase::Validating;
        let mut git = FakeGit::on_branch("rsi/cand-1");
        let mut release = FakeRelease::default();
        let gates = FakeDeliveryGates::default();
        let mut closer = FakeCloser::default();
        let report = run(
            &v,
            b,
            &mut git,
            &mut release,
            &gates,
            &mut closer,
            &policy(),
        );
        assert_eq!(report.state, DeliveryState::Rejected);
        assert!(report.reasons.iter().any(|r| r.contains("Promoted")));
    }
}
