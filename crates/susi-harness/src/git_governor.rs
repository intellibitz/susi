//! Public workflow contracts delegated through the authorized MCP socket.
#![forbid(unsafe_code)]

use serde::{de::DeserializeOwned, Deserialize, Serialize};
#[cfg(test)]
use serde_json::{json, Value};
use std::collections::BTreeSet;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use susi_error::{EaiError, EaiResult};

/// Receipt schema for automated multi-branch development sweeps onto `main`.
pub const AUTO_MERGE_RECEIPT_SCHEMA: &str = "susi-auto-merge-receipt-v1";
/// Receipt schema for single-revision reconciliation submissions onto `main`.
pub const SUBMIT_MAIN_RECEIPT_SCHEMA: &str = "susi-main-submission-receipt-v1";
/// Receipt schema for release-stream audits and verified candidate promotions.
pub const RELEASE_PROMOTION_RECEIPT_SCHEMA: &str = "susi-release-promotion-receipt-v1";
/// Receipt schema for Susi-governed remote branch clutter preservation and retirement.
pub const RETIRE_BRANCHES_RECEIPT_SCHEMA: &str = "susi-branch-retirement-receipt-v1";

/// Options governing an automated pending-branch sweep into unqualified `main`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoMergeConfig {
    /// Target Susi repository or linked worktree.
    #[serde(with = "susi_config::native_path")]
    pub repo: PathBuf,
    /// Configured Git storage remote (default: `"origin"`).
    pub remote: String,
    /// Fetch latest remote branch tips before classifying (`true` by default).
    pub fetch_remote: bool,
    /// Include local `refs/heads/*` branches in addition to the remote and agent refs.
    pub include_local: bool,
    /// Optional short-name filters (exact name or `prefix*` pattern).
    pub branch_filters: Vec<String>,
    /// Full commit OIDs of host-created commits explicitly adopted by an operator.
    pub adopted_host_commits: BTreeSet<String>,
    /// Require every non-merge delta commit to carry a valid `Task: T-...` trailer.
    pub require_task_trailer: bool,
    /// Inspect, classify, and compose in-memory without pushing `refs/heads/main`.
    pub dry_run: bool,
    /// Maximum compare-and-swap push retries when `main` advances concurrently.
    pub max_attempts: usize,
    /// Optional path where the signed/digested JSON receipt is written.
    #[serde(default, with = "susi_config::native_path::option")]
    pub receipt_out: Option<PathBuf>,
}

impl Default for AutoMergeConfig {
    fn default() -> Self {
        Self {
            repo: PathBuf::from("."),
            remote: "origin".to_string(),
            fetch_remote: true,
            include_local: false,
            branch_filters: Vec::new(),
            adopted_host_commits: BTreeSet::new(),
            require_task_trailer: true,
            dry_run: false,
            max_attempts: 4,
            receipt_out: None,
        }
    }
}

/// Deterministic disposition assigned to each inspected branch during `sweep_pending_branches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchSweepDisposition {
    /// Branch tip is already an ancestor of `main`.
    AlreadyMerged,
    /// Protected `release/*`, `release-staging/*`, `distill/*`, or internal ref.
    ProtectedStreamBoundary,
    /// Explicitly marked `RETIRE` in committed `.agents/tasks/**` governance records.
    RetiredByGovernanceLedger,
    /// Local branch attached to a dirty worktree or active in-progress claim.
    ActiveWorktreeOrClaim,
    /// Delta introduces a forbidden credential or secret file path.
    ConfidentialityRefused,
    /// Delta includes an unadopted commit authored/committed by the hosting service.
    HostCreatedCommitRefused,
    /// Delta includes a non-merge commit without a `Task: T-...` trailer.
    MissingTaskGovernance,
    /// Three-way `git merge-tree --write-tree` reported content conflicts; isolated and deferred.
    DeferredConflict,
    /// Integrated cleanly via direct fast-forward.
    MergedFastForward,
    /// Integrated cleanly via 2-parent object-store three-way merge commit.
    MergedThreeWay,
}

/// Per-branch audit record in [`AutoMergeReceipt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSweepEntry {
    /// Canonical ref inspected (`refs/remotes/origin[-agents]/...` or `refs/heads/...`).
    pub git_ref: String,
    /// Short branch name (`local-agent/...` also for an agent ref, `release/...`, etc.).
    pub branch: String,
    /// Full commit OID at branch tip.
    pub head_oid: String,
    /// Classification under the workflow stream contract.
    pub stream: String,
    /// Deterministic disposition.
    pub disposition: BranchSweepDisposition,
    /// Resulting `main` candidate OID after integrating this branch (when merged).
    pub resulting_main_oid: Option<String>,
    /// Extracted `Task:` identifiers carried by integrated commits.
    pub tasks: Vec<String>,
    /// Conflicting paths or refusal details when not merged.
    pub detail: String,
}

/// Audited receipt emitted by [`sweep_pending_branches`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoMergeReceipt {
    /// Schema identifier (`susi-auto-merge-receipt-v1`).
    pub schema: String,
    /// Governed stream (`unqualified_development_main`).
    pub stream: String,
    /// Qualification tier (`unqualified_dev_integration` — never a release claim).
    pub qualification_status: String,
    /// Configured remote name.
    pub remote: String,
    /// Whether this run was a dry-run simulation.
    pub dry_run: bool,
    /// Unix timestamp (seconds) when reconciliation finished.
    pub generated_unix: u64,
    /// Compare-and-swap attempt index (`1..=max_attempts`).
    pub attempt: usize,
    /// Initial `main` commit OID before reconciliation.
    pub initial_main_oid: String,
    /// Final composed `main` commit OID.
    pub final_main_oid: String,
    /// Remote `refs/heads/main` OID confirmed via `git ls-remote` after push (`None` in dry-run).
    pub published_remote_main_oid: Option<String>,
    /// Accepted publication whose optional record cleanup needs attention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_warning: Option<String>,
    /// Count of branches integrated in this sweep.
    pub merged_count: usize,
    /// Count of branches already in `main`.
    pub already_merged_count: usize,
    /// Count of branches skipped as protected release/distillation streams.
    pub protected_stream_count: usize,
    /// Count of branches skipped by governance `RETIRE` ledgers.
    pub retired_count: usize,
    /// Count of branches deferred due to conflicts or policy refusals.
    pub deferred_or_refused_count: usize,
    /// Ordered per-branch classifications.
    pub branches: Vec<BranchSweepEntry>,
    /// Canonical SHA-256 over the receipt with `receipt_sha256` omitted.
    pub receipt_sha256: String,
}

/// Options for single-revision reconciliation onto unqualified development `main`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitMainConfig {
    /// Repository or linked worktree.
    #[serde(with = "susi_config::native_path")]
    pub repo: PathBuf,
    /// Revision or branch to reconcile onto `main` (default: `"HEAD"`).
    pub rev: String,
    /// Storage remote (default: `"origin"`).
    pub remote: String,
    /// Require `Task: T-...` trailers on non-merge delta commits.
    pub require_task_trailer: bool,
    /// Explicitly adopted host-created commit OIDs.
    pub adopted_host_commits: BTreeSet<String>,
    /// Compose without pushing when `true`.
    pub dry_run: bool,
    /// Maximum compare-and-swap retries when `<remote>/main` advances concurrently.
    pub max_attempts: usize,
}

/// Receipt emitted by [`submit_main`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitMainReceipt {
    /// Schema identifier (`susi-main-submission-receipt-v1`).
    pub schema: String,
    /// Governed stream (`unqualified_development_main`).
    pub stream: String,
    /// Qualification status (`unqualified_dev_integration`).
    pub qualification_status: String,
    /// Remote name.
    pub remote: String,
    /// Dry-run indicator.
    pub dry_run: bool,
    /// Source commit OID submitted.
    pub source_oid: String,
    /// Initial `<remote>/main` OID.
    pub initial_main_oid: String,
    /// Final reconciled `main` OID.
    pub final_main_oid: String,
    /// `"already_merged"`, `"fast_forward"`, or `"reconciled_three_way"`.
    pub mode: String,
    /// Tasks carried in the delta.
    pub tasks: Vec<String>,
    /// Confirmed remote `refs/heads/main` OID when pushed.
    pub published_remote_main_oid: Option<String>,
    /// Accepted publication whose optional record cleanup needs attention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_warning: Option<String>,
    /// Canonical SHA-256 digest.
    pub receipt_sha256: String,
}

/// Options governing verified release-stream audit and candidate promotion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleasePromotionConfig {
    /// Repository or worktree path.
    #[serde(with = "susi_config::native_path")]
    pub repo: PathBuf,
    /// Storage remote (default: `"origin"`).
    pub remote: String,
    /// Fetch remote `release/*` and `distill/*` refs first.
    pub fetch_remote: bool,
    /// Optional target release name (`<name>` for `refs/heads/release/<name>`).
    /// When `None`, audits all remote `release/*` candidates and reports the latest verified candidate.
    pub target_release_name: Option<String>,
    /// Optional source candidate ref or commit OID (defaults to the latest verified `release/*` candidate when promoting).
    pub candidate_rev: Option<String>,
    /// Audit and synthesize envelope without pushing when `true`.
    pub dry_run: bool,
    /// Optional path to write the release-promotion receipt JSON.
    #[serde(default, with = "susi_config::native_path::option")]
    pub receipt_out: Option<PathBuf>,
}

/// Audit status of one `release/*` candidate branch in Git storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidateAudit {
    /// Full remote ref (`refs/remotes/origin/release/...`).
    pub git_ref: String,
    /// Release name suffix (`<name>` under `release/<name>`).
    pub release_name: String,
    /// Branch tip commit OID.
    pub head_oid: String,
    /// Underlying composed source commit (`final_commit`) when backed by a distillation envelope.
    pub final_commit_oid: Option<String>,
    /// Whether `releases/distill/<name>.json` verified cryptographically and passed all `accept_verification` checks.
    pub receipt_verified: bool,
    /// Qualification status recorded in the receipt (`accept_verified_candidate`, `historical_or_custom`, etc.).
    pub qualification_status: String,
    /// Number of included tasks in the receipt.
    pub included_task_count: usize,
    /// Number of tasks already satisfied in base.
    pub satisfied_in_base_count: usize,
    /// Role in the release stream (`latest_verified_candidate`, `superseded_verified_candidate`, `unverified_or_historical`).
    pub role: String,
    /// Diagnostic summary.
    pub detail: String,
}

/// Receipt emitted by [`promote_release_candidate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePromotionReceipt {
    /// Schema identifier (`susi-release-promotion-receipt-v1`).
    pub schema: String,
    /// Governed stream (`qualified_release_promotion`).
    pub stream: String,
    /// Remote name.
    pub remote: String,
    /// Dry-run indicator.
    pub dry_run: bool,
    /// Unix timestamp (seconds).
    pub generated_unix: u64,
    /// Latest verified release candidate branch discovered on local or remote storage.
    pub latest_verified_branch: Option<String>,
    /// Local-only promoted target `refs/heads/release/<name>` when promotion ran.
    pub promoted_branch_ref: Option<String>,
    /// Promoted envelope commit OID when promotion ran.
    pub promoted_commit_oid: Option<String>,
    /// Immutable storage snapshot tag (`refs/tags/susi-distill/<name>`) published on `<remote>`.
    pub published_snapshot_tag: Option<String>,
    /// Confirmed remote OID of `published_snapshot_tag` on `<remote>` (`None` in audit-only or dry-run mode).
    pub published_remote_oid: Option<String>,
    /// Explicit qualification tier (`accept_verified_candidate` vs production tag readiness).
    pub qualification_status: String,
    /// Truthful blockers remaining before an immutable `refs/tags/vX.Y.Z` production tag.
    pub production_tag_blockers: Vec<String>,
    /// Audited `release/*` branches in deterministic order.
    pub candidates: Vec<ReleaseCandidateAudit>,
    /// Canonical SHA-256 digest.
    pub receipt_sha256: String,
}

/// Options governing Susi-managed remote branch clutter preservation and retirement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetireBranchesConfig {
    /// Repository or linked worktree path.
    #[serde(with = "susi_config::native_path")]
    pub repo: PathBuf,
    /// Storage remote (default: `"origin"`).
    pub remote: String,
    /// Fetch remote refs before auditing.
    pub fetch_remote: bool,
    /// Also retire `refs/heads/local-agent/<name>` branches whose tips are already in `main` or marked `RETIRE`.
    pub include_local_agent: bool,
    /// Optional exact short names or `prefix*` filters.
    pub branch_filters: Vec<String>,
    /// Preserve/audit in dry-run simulation without pushing or deleting remote refs.
    pub dry_run: bool,
    /// Optional file path for the signed/digested JSON receipt.
    #[serde(default, with = "susi_config::native_path::option")]
    pub receipt_out: Option<PathBuf>,
}

/// Outcome of inspecting one remote `refs/heads/*` branch during clutter retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchRetireDisposition {
    /// Protected trunk (`refs/heads/main`), never deleted.
    ProtectedMain,
    /// Retired from `refs/heads/*` after confirming commit preservation on `<remote>`.
    Retired,
    /// Dry-run candidate verified ready to preserve and retire.
    WouldRetire,
    /// Skipped `local-agent/*` because `--include-local-agent` was not enabled.
    SkippedLocalAgentOptOut,
    /// Skipped because an active local worktree or task claim references this branch/agent.
    SkippedActiveClaim,
    /// Skipped because `local-agent/*` carries unmerged commits not marked `RETIRE`.
    SkippedUnmergedActive,
    /// Preservation ref confirmed, but remote server or transport hook refused branch deletion.
    RefusedByRemote,
}

/// Per-branch record inside [`RetireBranchesReceipt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredBranchEntry {
    /// Short branch name under `refs/heads/`.
    pub branch: String,
    /// Full remote branch ref (`refs/heads/<branch>`).
    pub remote_ref: String,
    /// Observed commit OID at `remote_ref`.
    pub head_oid: String,
    /// Whether `head_oid` is already in `<remote>/main`.
    pub in_main: bool,
    /// Immutable snapshot tag (`refs/tags/susi-distill/legacy-...`) preserving `head_oid`.
    pub preserved_snapshot_tag: Option<String>,
    /// Final disposition.
    pub disposition: BranchRetireDisposition,
    /// Human-readable diagnostic or remote refusal stderr.
    pub detail: String,
}

/// Receipt emitted by [`retire_remote_clutter`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetireBranchesReceipt {
    /// Schema identifier (`susi-branch-retirement-receipt-v1`).
    pub schema: String,
    /// Configured remote name.
    pub remote: String,
    /// Dry-run indicator.
    pub dry_run: bool,
    /// Unix timestamp (seconds).
    pub generated_unix: u64,
    /// `<remote>/main` commit OID observed during audit.
    pub main_oid: String,
    /// Count of remote `refs/heads/*` branches retired (or `WouldRetire` in dry-run).
    pub retired_count: usize,
    /// Count of branches skipped by safety guards (`main`, active claim, unmerged inbox, opt-out).
    pub skipped_count: usize,
    /// Count of branches whose deletion was refused by the remote after preservation.
    pub refused_count: usize,
    /// Remaining `refs/heads/*` count on `<remote>` after execution.
    pub remaining_remote_head_count: usize,
    /// Ordered per-branch records.
    pub branches: Vec<RetiredBranchEntry>,
    /// Canonical SHA-256 digest.
    pub receipt_sha256: String,
}

/// Validate explicitly adopted object IDs before sending a workflow request.
pub fn parse_adopted_oids(raw: &[String]) -> EaiResult<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    for item in raw {
        for tok in item
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !is_oid(tok) {
                return Err(EaiError::config(format!(
                    "adopted host commit must be a full 40/64-hex object ID, got {tok:?}"
                )));
            }
            set.insert(tok.to_ascii_lowercase());
        }
    }
    Ok(set)
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Inspect and integrate pending branches through the installed workflow provider.
pub fn sweep_pending_branches(config: &AutoMergeConfig) -> EaiResult<AutoMergeReceipt> {
    workflow_call("sweep", config)
}

/// Reconcile a single revision through the installed workflow provider.
pub fn submit_main(config: &SubmitMainConfig) -> EaiResult<SubmitMainReceipt> {
    workflow_call("submit", config)
}

/// Audit or promote a verified release candidate through the workflow provider.
pub fn promote_release_candidate(
    config: &ReleasePromotionConfig,
) -> EaiResult<ReleasePromotionReceipt> {
    workflow_call("promote", config)
}

/// Preserve and retire eligible remote branches through the workflow provider.
pub fn retire_remote_clutter(config: &RetireBranchesConfig) -> EaiResult<RetireBranchesReceipt> {
    workflow_call("retire", config)
}

#[cfg(test)]
use susi_config::workflow_wire::ERROR_SCHEMA;
use susi_config::workflow_wire::{decode_workflow_result, workflow_request, WORKFLOW_TOOL};
// Relative paths and their diagnostic labels keep the caller's native cwd.
pub(crate) use susi_config::workflow_wire::{
    wire_path, wire_path as caller_wire_path, workflow_error_value,
};

pub(crate) fn workflow_call<T: DeserializeOwned>(
    operation: &str,
    config: &impl Serialize,
) -> EaiResult<T> {
    let (request, cwd) = workflow_request(operation, config)?;
    decode_workflow_result(crate::tool_backend::call_result(
        WORKFLOW_TOOL,
        &request,
        cwd,
    )?)
}

/// The fallback runs only after discovery proves that no provider claimed this tool.
pub(crate) fn workflow_call_or<T: DeserializeOwned>(
    operation: &str,
    config: &impl Serialize,
    fallback: impl FnOnce() -> EaiResult<T>,
) -> EaiResult<T> {
    susi_config::workflow_wire::workflow_call_or_with(
        operation,
        config,
        crate::tool_backend::try_call_result,
        fallback,
    )
}

#[cfg(test)]
#[path = "tests/workflow_contract.rs"]
mod tests;
