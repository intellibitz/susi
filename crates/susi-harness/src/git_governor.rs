//! Susi-governed pure-Git branch integration and verified release promotion.
//!
//! Git hosting is pure storage (`git fetch`, `git push`, branches and tags); every
//! development reconciliation (`sweep_pending_branches`, `submit_main`) and verified
//! release promotion (`promote_release_candidate`) executes inside Susi (`susi-harness`
//! or `daemon.harness`), keeping Stream 1 (`unqualified_development_main`) strictly
//! separated from Stream 2 (`qualified_release_promotion` / `susi-distill/*`).
//! Agent refs `refs/agents/<a>` are swept as `local-agent/<a>` (docs/agent-refs.md).
#![forbid(unsafe_code)]

use crate::workflow::{classify_git_ref_stream, WorkflowStreamKind, AGENT_REF_PREFIX};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// Receipt schema for automated multi-branch development sweeps onto `main`.
pub const AUTO_MERGE_RECEIPT_SCHEMA: &str = "susi-auto-merge-receipt-v1";
/// Receipt schema for single-revision reconciliation submissions onto `main`.
pub const SUBMIT_MAIN_RECEIPT_SCHEMA: &str = "susi-main-submission-receipt-v1";
/// Receipt schema for release-stream audits and verified candidate promotions.
pub const RELEASE_PROMOTION_RECEIPT_SCHEMA: &str = "susi-release-promotion-receipt-v1";
/// Receipt schema for Susi-governed remote branch clutter preservation and retirement.
pub const RETIRE_BRANCHES_RECEIPT_SCHEMA: &str = "susi-branch-retirement-receipt-v1";

const DISTILL_RECEIPT_SCHEMA: &str = "susi-distillation-receipt-v1";
const HOST_COMMITTER_CONFIG_KEY: &str = "susi.hostCommitter";
const BUILTIN_HOST_COMMITTER: &str = "noreply@github.com";
const GOVERNOR_AUTHOR_NAME: &str = "Susi Harness Governor";
const GOVERNOR_AUTHOR_EMAIL: &str = "governor@susi.local";
const US: char = '\u{1f}';
const RS: char = '\u{1e}';

/// Options governing an automated pending-branch sweep into unqualified `main`.
#[derive(Debug, Clone)]
pub struct AutoMergeConfig {
    /// Target Susi repository or linked worktree.
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
    /// Stream classification under Mandate 73.
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
#[derive(Debug, Clone)]
pub struct SubmitMainConfig {
    /// Repository or linked worktree.
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
    /// Canonical SHA-256 digest.
    pub receipt_sha256: String,
}

/// Options governing verified release-stream audit and candidate promotion.
#[derive(Debug, Clone)]
pub struct ReleasePromotionConfig {
    /// Repository or worktree path.
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
#[derive(Debug, Clone, Default)]
pub struct RetireBranchesConfig {
    /// Repository or linked worktree path.
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

#[derive(Debug, Clone)]
struct GitOut {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}

impl GitOut {
    fn ok(&self) -> bool {
        self.code == 0
    }

    fn stdout_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

#[derive(Debug, Clone)]
struct RetiredRule {
    task_id: String,
    branch_short: String,
    head_prefixes: Vec<String>,
    reason: String,
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_remote_name(remote: &str) -> EaiResult<&str> {
    let trimmed = remote.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('-')
        || trimmed.contains("..")
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(EaiError::config(format!(
            "invalid Git remote name {remote:?}"
        )));
    }
    Ok(trimmed)
}

fn run_git(
    repo: &Path,
    args: &[&str],
    stdin_bytes: Option<&[u8]>,
    extra_env: &[(&str, &str)],
) -> EaiResult<GitOut> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo)
        .args([
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "--no-replace-objects",
        ])
        .args(args);
    susi_paths::console::apply_non_interactive_env(&mut cmd);
    for &(k, v) in extra_env {
        cmd.env(k, v);
    }
    let out =
        susi_paths::console::run_output_within(&mut cmd, stdin_bytes, Duration::from_secs(120))
            .map_err(|e| EaiError::process(format!("git {} failed: {e}", args.join(" "))))?;
    Ok(GitOut {
        code: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

fn git_ok(repo: &Path, args: &[&str]) -> EaiResult<String> {
    let out = run_git(repo, args, None, &[])?;
    if !out.ok() {
        return Err(EaiError::process(format!(
            "git {} exited {}: {}",
            args.join(" "),
            out.code,
            out.stderr
        )));
    }
    Ok(out.stdout_trimmed())
}

fn is_ancestor(repo: &Path, anc: &str, desc: &str) -> EaiResult<bool> {
    let out = run_git(repo, &["merge-base", "--is-ancestor", anc, desc], None, &[])?;
    match out.code {
        0 => Ok(true),
        1 => Ok(false),
        other => Err(EaiError::process(format!(
            "git merge-base --is-ancestor {anc} {desc} exited {other}: {}",
            out.stderr
        ))),
    }
}

fn compute_receipt_sha256(value: &mut Value) -> EaiResult<String> {
    if let Some(map) = value.as_object_mut() {
        map.remove("receipt_sha256");
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|e| EaiError::internal(format!("serialize receipt for digest: {e}")))?;
    let digest = Sha256::digest(&bytes);
    let hex = susi_paths::hex::encode(digest.as_slice());
    if let Some(map) = value.as_object_mut() {
        map.insert("receipt_sha256".to_string(), Value::String(hex.clone()));
    }
    Ok(hex)
}

fn write_receipt_if_requested(path: Option<&Path>, value: &Value) -> EaiResult<()> {
    let Some(target) = path else {
        return Ok(());
    };
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                EaiError::io(format!(
                    "create receipt parent dir {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    let mut pretty = serde_json::to_string_pretty(value)
        .map_err(|e| EaiError::internal(format!("format receipt JSON: {e}")))?;
    pretty.push('\n');
    fs::write(target, pretty)
        .map_err(|e| EaiError::io(format!("write receipt {}: {e}", target.display())))
}

/// Parse comma- or whitespace-separated `--adopt-host-commit` full OIDs.
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

fn configured_host_committers(repo: &Path) -> EaiResult<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    set.insert(BUILTIN_HOST_COMMITTER.to_string());
    let out = run_git(
        repo,
        &["config", "--get-all", HOST_COMMITTER_CONFIG_KEY],
        None,
        &[],
    )?;
    if out.code == 0 {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let norm = line
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .trim()
                .to_ascii_lowercase();
            if !norm.is_empty() {
                set.insert(norm);
            }
        }
    }
    Ok(set)
}

fn agent_mirror_prefix(remote: &str) -> String {
    format!("refs/remotes/{}-agents/", remote.trim())
}

fn normalize_branch_short(raw: &str, remote: &str) -> String {
    let trimmed = raw.trim();
    if let Some(agent) = trimmed
        .strip_prefix(AGENT_REF_PREFIX)
        .or_else(|| trimmed.strip_prefix(&agent_mirror_prefix(remote)))
    {
        return format!("local-agent/{agent}");
    }
    trimmed
        .strip_prefix("refs/heads/")
        .or_else(|| trimmed.strip_prefix(&format!("refs/remotes/{remote}/")))
        .or_else(|| trimmed.strip_prefix(&format!("{remote}/")))
        .unwrap_or(trimmed)
        .to_string()
}

fn matches_filters(branch: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.iter().any(|pat| {
        let pat = pat.trim();
        if let Some(prefix) = pat.strip_suffix('*') {
            branch.starts_with(prefix)
        } else {
            branch == pat
        }
    })
}

/// Read `RETIRE` verdicts recorded in `.agents/tasks/**` directly from `main_oid`'s tree.
fn load_retired_rules(repo: &Path, main_oid: &str, remote: &str) -> EaiResult<Vec<RetiredRule>> {
    let ls = run_git(
        repo,
        &[
            "ls-tree",
            "-r",
            "--full-tree",
            main_oid,
            "--",
            ".agents/tasks",
        ],
        None,
        &[],
    )?;
    if !ls.ok() {
        return Ok(Vec::new());
    }
    let mut blob_oids = Vec::new();
    for line in String::from_utf8_lossy(&ls.stdout).lines() {
        // `<mode> <type> <oid>\t<path>`
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        if !path.ends_with(".json") {
            continue;
        }
        let parts: Vec<&str> = meta.split_whitespace().collect();
        if parts.len() == 3 && parts[1] == "blob" && is_oid(parts[2]) {
            blob_oids.push(parts[2].to_string());
        }
    }
    if blob_oids.is_empty() {
        return Ok(Vec::new());
    }
    let batch_input = format!("{}\n", blob_oids.join("\n"));
    let batch = run_git(
        repo,
        &["cat-file", "--batch"],
        Some(batch_input.as_bytes()),
        &[],
    )?;
    if !batch.ok() {
        return Ok(Vec::new());
    }
    let mut rules = Vec::new();
    let bytes = &batch.stdout;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(nl_rel) = bytes[cursor..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&bytes[cursor..cursor + nl_rel]);
        cursor += nl_rel + 1;
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 3 || parts[1] != "blob" {
            continue;
        }
        let Ok(size) = parts[2].parse::<usize>() else {
            break;
        };
        if cursor + size > bytes.len() {
            break;
        }
        let payload = &bytes[cursor..cursor + size];
        cursor = (cursor + size + 1).min(bytes.len());
        let Ok(doc) = serde_json::from_slice::<Value>(payload) else {
            continue;
        };
        let task_id = doc["id"].as_str().unwrap_or("UNKNOWN").to_string();
        let Some(results) = doc["results"].as_array() else {
            continue;
        };
        for entry in results {
            let verdict = entry["verdict"].as_str().unwrap_or("");
            if !verdict.eq_ignore_ascii_case("RETIRE") {
                continue;
            }
            let Some(raw_branch) = entry["branch"].as_str() else {
                continue;
            };
            let branch_short = normalize_branch_short(raw_branch, remote);
            let head_prefixes: Vec<String> = entry["heads"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .collect();
            let reason = entry["evidence"]
                .as_str()
                .and_then(|s| s.lines().next())
                .unwrap_or("retired by governance record")
                .chars()
                .take(160)
                .collect();
            rules.push(RetiredRule {
                task_id: task_id.clone(),
                branch_short,
                head_prefixes,
                reason,
            });
        }
    }
    Ok(rules)
}

fn find_retired_rule<'a>(
    branch_short: &str,
    head_oid: &str,
    rules: &'a [RetiredRule],
) -> Option<&'a RetiredRule> {
    let lower_oid = head_oid.to_ascii_lowercase();
    rules.iter().find(|r| {
        r.branch_short == branch_short
            && (r.head_prefixes.is_empty()
                || r.head_prefixes.iter().any(|p| lower_oid.starts_with(p)))
    })
}

/// Map local branch short names -> `(worktree_path, is_dirty_or_claimed)`.
fn inspect_local_worktree_hazards(repo: &Path) -> EaiResult<BTreeMap<String, String>> {
    let mut hazards = BTreeMap::new();
    let wt_out = run_git(repo, &["worktree", "list", "--porcelain"], None, &[])?;
    if wt_out.ok() {
        let mut cur_wt: Option<PathBuf> = None;
        let mut cur_branch: Option<String> = None;
        let text = String::from_utf8_lossy(&wt_out.stdout);
        for line in text.lines().chain(std::iter::once("")) {
            if let Some(rest) = line.strip_prefix("worktree ") {
                cur_wt = Some(PathBuf::from(rest.trim()));
            } else if let Some(rest) = line.strip_prefix("branch ") {
                if let Some(short) = rest.trim().strip_prefix("refs/heads/") {
                    cur_branch = Some(short.to_string());
                }
            } else if line.trim().is_empty() {
                if let (Some(wt), Some(br)) = (cur_wt.take(), cur_branch.take()) {
                    if wt.is_dir() {
                        let st = run_git(&wt, &["status", "--porcelain"], None, &[])?;
                        if !st.ok() || !st.stdout_trimmed().is_empty() {
                            hazards.insert(
                                br,
                                format!("worktree {} has uncommitted changes", wt.display()),
                            );
                        }
                    }
                }
                cur_wt = None;
                cur_branch = None;
            }
        }
    }
    if let Some(common) = crate::adapter::common_dir(repo) {
        let claims_dir = common.join("susi-local-workflow/claims");
        if let Ok(entries) = fs::read_dir(claims_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().is_none_or(|e| e != "claim") {
                    continue;
                }
                if let Ok(body) = fs::read_to_string(&p) {
                    let lines: Vec<&str> = body.lines().collect();
                    if lines.len() >= 3 && lines[0] == "susi-local-claim-v2" {
                        let agent = lines[1].trim();
                        hazards
                            .entry(format!("local-agent/{}", agent.to_ascii_lowercase()))
                            .or_insert_with(|| {
                                format!("active native claim `{agent}` holds reserved scopes")
                            });
                    }
                }
            }
        }
    }
    Ok(hazards)
}

fn is_forbidden_secret_path(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let file = norm.rsplit('/').next().unwrap_or(&norm);
    let lower = file.to_ascii_lowercase();
    if lower == ".env" {
        return true;
    }
    if lower.starts_with(".env.")
        && !matches!(
            lower.as_str(),
            ".env.example" | ".env.sample" | ".env.template" | ".env.test"
        )
    {
        return true;
    }
    if matches!(
        lower.as_str(),
        "id_rsa" | "id_ed25519" | "id_ecdsa" | "credentials.json" | ".npmrc" | ".pypirc" | ".netrc"
    ) {
        return true;
    }
    if lower.starts_with("service-account") && lower.ends_with(".json") {
        return true;
    }
    Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "pem" | "key" | "p12" | "pfx" | "keystore"))
}

struct DeltaAudit {
    secret_paths: Vec<String>,
    host_commits: Vec<(String, String, String)>,
    missing_trailer_subjects: Vec<String>,
    tasks: Vec<String>,
}

fn extract_task_ids_from_message(msg: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in msg.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("Task:").or_else(|| t.strip_prefix("task:")) else {
            continue;
        };
        for raw in rest
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if raw.len() >= 4
                && (raw.starts_with("T-") || raw.starts_with("t-"))
                && raw
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                let up = raw.to_ascii_uppercase();
                if !found.contains(&up) {
                    found.push(up);
                }
            }
        }
    }
    found
}

fn audit_delta(
    repo: &Path,
    base_oid: &str,
    tip_oid: &str,
    host_committers: &BTreeSet<String>,
    adopted_host_commits: &BTreeSet<String>,
) -> EaiResult<DeltaAudit> {
    let range = format!("{base_oid}..{tip_oid}");
    // 1. Secret paths introduced or modified in net diff from merge-base to tip
    let merge_base = git_ok(repo, &["merge-base", base_oid, tip_oid])?;
    let diff = git_ok(
        repo,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMR",
            &merge_base,
            tip_oid,
            "--",
        ],
    )?;
    let mut secret_paths = Vec::new();
    for p in diff.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if is_forbidden_secret_path(p) {
            secret_paths.push(p.to_string());
        }
    }

    // 2. Commits in base..tip: check host committers and non-merge Task trailers
    let fmt = "--format=%x1e%H%x1f%P%x1f%ce%x1f%s%x1f%B";
    let log = git_ok(repo, &["log", "--topo-order", fmt, &range, "--"])?;
    let mut host_commits = Vec::new();
    let mut missing_trailer_subjects = Vec::new();
    let mut task_set = BTreeSet::new();

    for rec in log.split(RS) {
        let rec = rec.trim_matches(['\n', '\r']);
        if rec.is_empty() {
            continue;
        }
        let mut fields = rec.splitn(5, US);
        let oid = fields.next().unwrap_or("").trim().to_ascii_lowercase();
        let parents = fields.next().unwrap_or("");
        let committer_email = fields
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_ascii_lowercase();
        let subject = fields.next().unwrap_or("").trim().to_string();
        let body = fields.next().unwrap_or("");
        if !is_oid(&oid) {
            continue;
        }
        if host_committers.contains(&committer_email) && !adopted_host_commits.contains(&oid) {
            host_commits.push((oid.clone(), committer_email, subject.clone()));
        }
        let is_merge = parents.split_whitespace().count() > 1;
        let ids = extract_task_ids_from_message(body);
        if !is_merge && ids.is_empty() {
            missing_trailer_subjects.push(subject);
        }
        for t in ids {
            task_set.insert(t);
        }
    }

    Ok(DeltaAudit {
        secret_paths,
        host_commits,
        missing_trailer_subjects,
        tasks: task_set.into_iter().collect(),
    })
}

enum MergeTreeOutcome {
    Clean(String),
    Conflicts(Vec<String>),
}

fn try_three_way_merge_tree(
    repo: &Path,
    main_oid: &str,
    branch_oid: &str,
) -> EaiResult<MergeTreeOutcome> {
    let out = run_git(
        repo,
        &["merge-tree", "--write-tree", main_oid, branch_oid],
        None,
        &[],
    )?;
    let stdout_str = String::from_utf8_lossy(&out.stdout);
    if out.ok() {
        let tree_oid = stdout_str.lines().next().unwrap_or("").trim().to_string();
        if !is_oid(&tree_oid) {
            return Err(EaiError::process(format!(
                "git merge-tree --write-tree returned invalid tree OID: {tree_oid:?}"
            )));
        }
        return Ok(MergeTreeOutcome::Clean(tree_oid));
    }
    if out.code == 1 {
        let mut conflicts = BTreeSet::new();
        for line in stdout_str.lines().skip(1) {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("CONFLICT ") {
                conflicts.insert(rest.to_string());
            } else if let Some((_, path)) = trimmed.split_once('\t') {
                conflicts.insert(path.trim().to_string());
            }
        }
        let list: Vec<String> = if conflicts.is_empty() {
            vec!["content conflict reported by git merge-tree".to_string()]
        } else {
            conflicts.into_iter().collect()
        };
        return Ok(MergeTreeOutcome::Conflicts(list));
    }
    Err(EaiError::process(format!(
        "git merge-tree --write-tree {main_oid} {branch_oid} failed ({}): {}",
        out.code, out.stderr
    )))
}

struct MergeCommitSpec<'a> {
    tree_oid: &'a str,
    first_parent: &'a str,
    second_parent: &'a str,
    label: &'a str,
    tasks: &'a [String],
}

fn create_governed_merge_commit(repo: &Path, spec: &MergeCommitSpec<'_>) -> EaiResult<String> {
    let now = now_unix_secs();
    let date_val = format!("{now} +0000");
    let short_src = spec.second_parent.get(..12).unwrap_or(spec.second_parent);
    let mut msg = format!(
        "Reconcile {} ({short_src}) onto development main\n",
        spec.label
    );
    if !spec.tasks.is_empty() {
        msg.push('\n');
        for t in spec.tasks {
            msg.push_str(&format!("Task: {t}\n"));
        }
    }
    let out = run_git(
        repo,
        &[
            "commit-tree",
            spec.tree_oid,
            "-p",
            spec.first_parent,
            "-p",
            spec.second_parent,
        ],
        Some(msg.as_bytes()),
        &[
            ("GIT_AUTHOR_NAME", GOVERNOR_AUTHOR_NAME),
            ("GIT_AUTHOR_EMAIL", GOVERNOR_AUTHOR_EMAIL),
            ("GIT_AUTHOR_DATE", &date_val),
            ("GIT_COMMITTER_NAME", GOVERNOR_AUTHOR_NAME),
            ("GIT_COMMITTER_EMAIL", GOVERNOR_AUTHOR_EMAIL),
            ("GIT_COMMITTER_DATE", &date_val),
        ],
    )?;
    if !out.ok() {
        return Err(EaiError::process(format!(
            "git commit-tree failed ({}): {}",
            out.code, out.stderr
        )));
    }
    let commit_oid = out.stdout_trimmed();
    if !is_oid(&commit_oid) {
        return Err(EaiError::process(format!(
            "git commit-tree returned invalid OID: {commit_oid:?}"
        )));
    }
    Ok(commit_oid)
}

fn confirm_remote_ref(repo: &Path, remote: &str, full_ref: &str) -> EaiResult<Option<String>> {
    let out = run_git(repo, &["ls-remote", remote, full_ref], None, &[])?;
    if !out.ok() {
        return Err(EaiError::process(format!(
            "git ls-remote {remote} {full_ref} failed: {}",
            out.stderr
        )));
    }
    let text = out.stdout_trimmed();
    let oid = text.lines().find_map(|line| {
        let (sha, r) = line.split_once('\t')?;
        (r.trim() == full_ref && is_oid(sha.trim())).then(|| sha.trim().to_string())
    });
    Ok(oid)
}

fn sync_local_main_if_safe(repo: &Path, remote: &str, new_main: &str) {
    let _ = run_git(
        repo,
        &[
            "update-ref",
            &format!("refs/remotes/{remote}/main"),
            new_main,
        ],
        None,
        &[],
    );
    // Advance local refs/heads/main only if no worktree currently has `main` checked out,
    // or if it is a clean fast-forward in a non-checked-out ref.
    let wt_out = run_git(repo, &["worktree", "list", "--porcelain"], None, &[]);
    let main_checked_out = wt_out.is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.trim() == "branch refs/heads/main")
    });
    if !main_checked_out {
        if let Ok(cur_local) = git_ok(repo, &["rev-parse", "--verify", "refs/heads/main"]) {
            if is_ancestor(repo, &cur_local, new_main).unwrap_or(false) {
                let _ = run_git(
                    repo,
                    &["update-ref", "refs/heads/main", new_main, &cur_local],
                    None,
                    &[],
                );
            }
        }
    }
}

/// Automatically discover and integrate all eligible pending development branches into
/// unqualified development `main`, strictly preserving stream boundaries and peer work.
pub fn sweep_pending_branches(config: &AutoMergeConfig) -> EaiResult<AutoMergeReceipt> {
    let remote = validate_remote_name(&config.remote)?;
    let max_attempts = config.max_attempts.max(1);
    let host_committers = configured_host_committers(&config.repo)?;

    for attempt in 1..=max_attempts {
        if config.fetch_remote {
            let fetch = run_git(
                &config.repo,
                &[
                    "fetch",
                    "--no-tags",
                    "--prune",
                    remote,
                    &format!("+refs/heads/*:refs/remotes/{remote}/*"),
                    &format!("+{AGENT_REF_PREFIX}*:{}*", agent_mirror_prefix(remote)),
                ],
                None,
                &[],
            )?;
            if !fetch.ok() {
                return Err(EaiError::process(format!(
                    "git fetch {remote} failed ({}): {}",
                    fetch.code, fetch.stderr
                )));
            }
        }

        let initial_main = git_ok(
            &config.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/remotes/{remote}/main"),
            ],
        )
        .or_else(|_| git_ok(&config.repo, &["rev-parse", "--verify", "refs/heads/main"]))?;

        let retired_rules = load_retired_rules(&config.repo, &initial_main, remote)?;
        let local_hazards = if config.include_local {
            inspect_local_worktree_hazards(&config.repo)?
        } else {
            BTreeMap::new()
        };

        // Gather candidate refs deterministically: remote refs first, then optional local refs
        // not already represented at the exact same OID on remote.
        let mut candidates: Vec<(String, String, String, bool)> = Vec::new();
        let mut seen_branch_oid: BTreeSet<(String, String)> = BTreeSet::new();

        let agent_prefix = agent_mirror_prefix(remote); // sorts before the branches
        let rem_prefix = format!("refs/remotes/{remote}/");
        let rem_refs = git_ok(
            &config.repo,
            &[
                "for-each-ref",
                "--format=%(refname)\t%(objectname)",
                &agent_prefix,
                &rem_prefix,
            ],
        )?;
        for line in rem_refs.lines() {
            let Some((full_ref, oid)) = line.split_once('\t') else {
                continue;
            };
            let full_ref = full_ref.trim();
            let oid = oid.trim();
            if full_ref.ends_with("/HEAD") || !is_oid(oid) {
                continue;
            }
            let short = normalize_branch_short(full_ref, remote);
            if short == "main" || !matches_filters(&short, &config.branch_filters) {
                continue;
            }
            if seen_branch_oid.insert((short.clone(), oid.to_string())) {
                candidates.push((full_ref.to_string(), short, oid.to_string(), false));
            }
        }

        if config.include_local {
            let loc_refs = git_ok(
                &config.repo,
                &[
                    "for-each-ref",
                    "--format=%(refname)\t%(objectname)",
                    "refs/heads/",
                ],
            )?;
            for line in loc_refs.lines() {
                let Some((full_ref, oid)) = line.split_once('\t') else {
                    continue;
                };
                let full_ref = full_ref.trim();
                let oid = oid.trim();
                if !is_oid(oid) {
                    continue;
                }
                let short = normalize_branch_short(full_ref, remote);
                if short == "main"
                    || !matches_filters(&short, &config.branch_filters)
                    || seen_branch_oid.contains(&(short.clone(), oid.to_string()))
                {
                    continue;
                }
                candidates.push((full_ref.to_string(), short, oid.to_string(), true));
            }
        }

        candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        let mut running_main = initial_main.clone();
        let mut entries = Vec::with_capacity(candidates.len());
        let mut merged_count = 0usize;
        let mut already_merged_count = 0usize;
        let mut protected_stream_count = 0usize;
        let mut retired_count = 0usize;
        let mut deferred_or_refused_count = 0usize;

        for (full_ref, short, head_oid, is_local) in candidates {
            let stream = classify_git_ref_stream(&full_ref, Some(remote));
            let mut rec = BranchSweepEntry {
                git_ref: full_ref,
                branch: short.clone(),
                head_oid: head_oid.clone(),
                stream: stream.as_str().to_string(),
                disposition: BranchSweepDisposition::AlreadyMerged,
                resulting_main_oid: None,
                tasks: Vec::new(),
                detail: String::new(),
            };
            if !stream.eligible_for_unqualified_main_sweep() {
                protected_stream_count += 1;
                rec.disposition = BranchSweepDisposition::ProtectedStreamBoundary;
                rec.detail = format!(
                    "stream `{}` is governed separately from unqualified development main",
                    stream.as_str()
                );
                entries.push(rec);
                continue;
            }
            if is_ancestor(&config.repo, &head_oid, &running_main)? {
                already_merged_count += 1;
                rec.detail = "already an ancestor of main".to_string();
                entries.push(rec);
                continue;
            }
            if let Some(rule) = find_retired_rule(&short, &head_oid, &retired_rules) {
                retired_count += 1;
                rec.disposition = BranchSweepDisposition::RetiredByGovernanceLedger;
                rec.tasks = vec![rule.task_id.clone()];
                rec.detail = format!("retired by {}: {}", rule.task_id, rule.reason);
                entries.push(rec);
                continue;
            }
            if is_local {
                if let Some(hazard) = local_hazards.get(&short) {
                    deferred_or_refused_count += 1;
                    rec.disposition = BranchSweepDisposition::ActiveWorktreeOrClaim;
                    rec.detail = hazard.clone();
                    entries.push(rec);
                    continue;
                }
            }
            let delta = audit_delta(
                &config.repo,
                &running_main,
                &head_oid,
                &host_committers,
                &config.adopted_host_commits,
            )?;
            rec.tasks = delta.tasks.clone();
            if !delta.secret_paths.is_empty() {
                deferred_or_refused_count += 1;
                rec.disposition = BranchSweepDisposition::ConfidentialityRefused;
                rec.detail = format!(
                    "forbidden credential/secret paths: {}",
                    delta.secret_paths.join(", ")
                );
                entries.push(rec);
                continue;
            }
            if !delta.host_commits.is_empty() {
                deferred_or_refused_count += 1;
                let summary: Vec<String> = delta
                    .host_commits
                    .iter()
                    .map(|(oid, email, subj)| {
                        format!("{} ({email}: {subj})", oid.get(..10).unwrap_or(oid))
                    })
                    .collect();
                rec.disposition = BranchSweepDisposition::HostCreatedCommitRefused;
                rec.detail = format!(
                    "merged_outside_susi (pass --adopt-host-commit <full-oid> to adopt): {}",
                    summary.join("; ")
                );
                entries.push(rec);
                continue;
            }
            if config.require_task_trailer && !delta.missing_trailer_subjects.is_empty() {
                deferred_or_refused_count += 1;
                rec.disposition = BranchSweepDisposition::MissingTaskGovernance;
                rec.detail = format!(
                    "commit(s) missing `Task: T-...` trailer: {}",
                    delta.missing_trailer_subjects.join("; ")
                );
                entries.push(rec);
                continue;
            }
            if is_ancestor(&config.repo, &running_main, &head_oid)? {
                running_main = head_oid.clone();
                merged_count += 1;
                rec.disposition = BranchSweepDisposition::MergedFastForward;
                rec.resulting_main_oid = Some(head_oid);
                rec.detail = "integrated via clean fast-forward".to_string();
                entries.push(rec);
                continue;
            }
            match try_three_way_merge_tree(&config.repo, &running_main, &head_oid)? {
                MergeTreeOutcome::Conflicts(conflicts) => {
                    deferred_or_refused_count += 1;
                    rec.disposition = BranchSweepDisposition::DeferredConflict;
                    rec.detail = format!("merge conflict: {}", conflicts.join(", "));
                    entries.push(rec);
                }
                MergeTreeOutcome::Clean(tree_oid) => {
                    let merge_commit = create_governed_merge_commit(
                        &config.repo,
                        &MergeCommitSpec {
                            tree_oid: &tree_oid,
                            first_parent: &running_main,
                            second_parent: &head_oid,
                            label: &short,
                            tasks: &delta.tasks,
                        },
                    )?;
                    running_main = merge_commit.clone();
                    merged_count += 1;
                    rec.disposition = BranchSweepDisposition::MergedThreeWay;
                    rec.resulting_main_oid = Some(merge_commit);
                    rec.detail = "integrated via governed 3-way object-store merge".to_string();
                    entries.push(rec);
                }
            }
        }

        let mut published_remote_main_oid = None;
        if !config.dry_run && running_main != initial_main {
            let push = run_git(
                &config.repo,
                &[
                    "push",
                    &format!("--force-with-lease=refs/heads/main:{initial_main}"),
                    remote,
                    &format!("{running_main}:refs/heads/main"),
                ],
                None,
                &[],
            )?;
            if !push.ok() {
                if attempt < max_attempts {
                    continue;
                }
                return Err(EaiError::process(format!(
                    "atomic fast-forward push to {remote} refs/heads/main failed after {attempt} attempt(s): {}",
                    push.stderr
                )));
            }
            let confirmed = confirm_remote_ref(&config.repo, remote, "refs/heads/main")?;
            published_remote_main_oid = confirmed;
            sync_local_main_if_safe(&config.repo, remote, &running_main);
        } else if !config.dry_run {
            published_remote_main_oid = Some(initial_main.clone());
        }

        let mut receipt = AutoMergeReceipt {
            schema: AUTO_MERGE_RECEIPT_SCHEMA.to_string(),
            stream: WorkflowStreamKind::UnqualifiedDevelopmentMain
                .as_str()
                .to_string(),
            qualification_status: "unqualified_dev_integration".to_string(),
            remote: remote.to_string(),
            dry_run: config.dry_run,
            generated_unix: now_unix_secs(),
            attempt,
            initial_main_oid: initial_main,
            final_main_oid: running_main,
            published_remote_main_oid,
            merged_count,
            already_merged_count,
            protected_stream_count,
            retired_count,
            deferred_or_refused_count,
            branches: entries,
            receipt_sha256: String::new(),
        };
        let mut val = serde_json::to_value(&receipt)
            .map_err(|e| EaiError::internal(format!("serialize auto-merge receipt: {e}")))?;
        receipt.receipt_sha256 = compute_receipt_sha256(&mut val)?;
        write_receipt_if_requested(config.receipt_out.as_deref(), &val)?;
        return Ok(receipt);
    }

    Err(EaiError::process(
        "exhausted auto-merge compare-and-swap attempts",
    ))
}

/// Reconcile and fast-forward a single revision (`config.rev`) onto unqualified `<remote>/main`.
pub fn submit_main(config: &SubmitMainConfig) -> EaiResult<SubmitMainReceipt> {
    let remote = validate_remote_name(&config.remote)?;
    let source_oid = git_ok(
        &config.repo,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", config.rev.trim()),
        ],
    )?;
    let max_attempts = config.max_attempts.max(1);
    let host_committers = configured_host_committers(&config.repo)?;

    for attempt in 1..=max_attempts {
        let fetch = run_git(
            &config.repo,
            &[
                "fetch",
                "--no-tags",
                remote,
                &format!("+refs/heads/main:refs/remotes/{remote}/main"),
            ],
            None,
            &[],
        )?;
        if !fetch.ok() {
            return Err(EaiError::process(format!(
                "git fetch {remote} main failed: {}",
                fetch.stderr
            )));
        }
        let initial_main = git_ok(
            &config.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/remotes/{remote}/main"),
            ],
        )?;

        if is_ancestor(&config.repo, &source_oid, &initial_main)? {
            let mut receipt = SubmitMainReceipt {
                schema: SUBMIT_MAIN_RECEIPT_SCHEMA.to_string(),
                stream: WorkflowStreamKind::UnqualifiedDevelopmentMain
                    .as_str()
                    .to_string(),
                qualification_status: "unqualified_dev_integration".to_string(),
                remote: remote.to_string(),
                dry_run: config.dry_run,
                source_oid,
                initial_main_oid: initial_main.clone(),
                final_main_oid: initial_main.clone(),
                mode: "already_merged".to_string(),
                tasks: Vec::new(),
                published_remote_main_oid: (!config.dry_run).then_some(initial_main),
                receipt_sha256: String::new(),
            };
            let mut val = serde_json::to_value(&receipt)
                .map_err(|e| EaiError::internal(format!("serialize submit-main receipt: {e}")))?;
            receipt.receipt_sha256 = compute_receipt_sha256(&mut val)?;
            return Ok(receipt);
        }

        let delta = audit_delta(
            &config.repo,
            &initial_main,
            &source_oid,
            &host_committers,
            &config.adopted_host_commits,
        )?;
        if !delta.secret_paths.is_empty() {
            return Err(EaiError::config(format!(
                "refusing submit-main: forbidden credential/secret paths: {}",
                delta.secret_paths.join(", ")
            )));
        }
        if !delta.host_commits.is_empty() {
            return Err(EaiError::config(format!(
                "refusing submit-main: unadopted host-created commit(s) in delta ({})",
                delta
                    .host_commits
                    .iter()
                    .map(|(sha, email, _)| format!("{sha} <{email}>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if config.require_task_trailer && !delta.missing_trailer_subjects.is_empty() {
            return Err(EaiError::config(format!(
                "refusing submit-main: commit(s) missing `Task: T-...` trailer: {}",
                delta.missing_trailer_subjects.join("; ")
            )));
        }

        let (final_main, mode) = if is_ancestor(&config.repo, &initial_main, &source_oid)? {
            (source_oid.clone(), "fast_forward".to_string())
        } else {
            match try_three_way_merge_tree(&config.repo, &initial_main, &source_oid)? {
                MergeTreeOutcome::Conflicts(conflicts) => {
                    return Err(EaiError::process(format!(
                        "submit-main three-way merge conflict against {remote}/main: {}",
                        conflicts.join(", ")
                    )));
                }
                MergeTreeOutcome::Clean(tree_oid) => {
                    let merged = create_governed_merge_commit(
                        &config.repo,
                        &MergeCommitSpec {
                            tree_oid: &tree_oid,
                            first_parent: &initial_main,
                            second_parent: &source_oid,
                            label: config.rev.trim(),
                            tasks: &delta.tasks,
                        },
                    )?;
                    (merged, "reconciled_three_way".to_string())
                }
            }
        };

        let mut published_remote = None;
        if !config.dry_run {
            let push = run_git(
                &config.repo,
                &[
                    "push",
                    &format!("--force-with-lease=refs/heads/main:{initial_main}"),
                    remote,
                    &format!("{final_main}:refs/heads/main"),
                ],
                None,
                &[],
            )?;
            if !push.ok() {
                if attempt < max_attempts {
                    continue;
                }
                return Err(EaiError::process(format!(
                    "submit-main push failed after {attempt} attempt(s): {}",
                    push.stderr
                )));
            }
            published_remote = confirm_remote_ref(&config.repo, remote, "refs/heads/main")?;
            sync_local_main_if_safe(&config.repo, remote, &final_main);
        }

        let mut receipt = SubmitMainReceipt {
            schema: SUBMIT_MAIN_RECEIPT_SCHEMA.to_string(),
            stream: WorkflowStreamKind::UnqualifiedDevelopmentMain
                .as_str()
                .to_string(),
            qualification_status: "unqualified_dev_integration".to_string(),
            remote: remote.to_string(),
            dry_run: config.dry_run,
            source_oid,
            initial_main_oid: initial_main,
            final_main_oid: final_main,
            mode,
            tasks: delta.tasks,
            published_remote_main_oid: published_remote,
            receipt_sha256: String::new(),
        };
        let mut val = serde_json::to_value(&receipt)
            .map_err(|e| EaiError::internal(format!("serialize submit-main receipt: {e}")))?;
        receipt.receipt_sha256 = compute_receipt_sha256(&mut val)?;
        return Ok(receipt);
    }

    Err(EaiError::process("exhausted submit-main attempts"))
}

fn valid_release_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !name.is_empty()
        && name.len() <= 48
        && !device
        && !name.starts_with(['-', '.'])
        && !name.ends_with(['-', '.'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
}

fn inspect_distill_envelope(
    repo: &Path,
    commit_oid: &str,
    expected_name: Option<&str>,
) -> Result<(String, String, Value), String> {
    let lineage = git_ok(repo, &["rev-list", "--parents", "-n", "1", commit_oid])
        .map_err(|e| e.to_string())?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    let [parent] = parents.as_slice() else {
        return Err("commit is not a single-parent distillation receipt envelope".to_string());
    };
    let changed = git_ok(
        repo,
        &[
            "diff-tree",
            "--no-commit-id",
            "--no-renames",
            "--name-only",
            "-r",
            parent,
            commit_oid,
        ],
    )
    .map_err(|e| e.to_string())?;

    let (rel_path, verified_name) = if let Some(name) = expected_name {
        let wanted = format!("releases/distill/{name}.json");
        if changed != wanted {
            return Err(format!("expected envelope diff {wanted}, got {changed:?}"));
        }
        (wanted, name.to_string())
    } else if let Some(stem) = changed
        .strip_prefix("releases/distill/")
        .and_then(|s| s.strip_suffix(".json"))
        .filter(|s| !s.contains('/') && !s.contains('\n') && valid_release_name(s))
    {
        (changed.clone(), stem.to_string())
    } else {
        return Err(format!(
            "commit does not add a single releases/distill/<name>.json receipt ({changed:?})"
        ));
    };

    let blob = run_git(
        repo,
        &["cat-file", "blob", &format!("{commit_oid}:{rel_path}")],
        None,
        &[],
    )
    .map_err(|e| e.to_string())?;
    if !blob.ok() {
        return Err(format!("cannot read {rel_path} from {commit_oid}"));
    }
    let mut record: Value = serde_json::from_slice(&blob.stdout)
        .map_err(|e| format!("{rel_path} invalid JSON: {e}"))?;
    let recorded_sha = record["receipt_sha256"]
        .as_str()
        .ok_or("missing receipt_sha256")?
        .to_string();
    if let Some(map) = record.as_object_mut() {
        map.remove("receipt_sha256");
    }
    let canonical = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    let computed = susi_paths::hex::encode(Sha256::digest(&canonical).as_slice());
    if computed != recorded_sha {
        return Err(format!(
            "receipt_sha256 mismatch (recorded {recorded_sha}, computed {computed})"
        ));
    }
    if let Some(map) = record.as_object_mut() {
        map.insert("receipt_sha256".to_string(), Value::String(recorded_sha));
    }
    if record["schema"].as_str() != Some(DISTILL_RECEIPT_SCHEMA) {
        return Err("unexpected receipt schema".to_string());
    }
    if record["qualification_status"].as_str() != Some("accept_verified_candidate") {
        return Err(format!(
            "qualification_status is {:?}, expected accept_verified_candidate",
            record["qualification_status"].as_str()
        ));
    }
    if record["final_commit"].as_str() != Some(*parent) {
        return Err("receipt final_commit does not match envelope parent".to_string());
    }
    let tree =
        git_ok(repo, &["rev-parse", &format!("{parent}^{{tree}}")]).map_err(|e| e.to_string())?;
    if record["final_tree"].as_str() != Some(tree.as_str()) {
        return Err("receipt final_tree does not match parent tree".to_string());
    }
    if !record["rejected"]
        .as_array()
        .ok_or("missing rejected array")?
        .is_empty()
    {
        return Err("receipt has non-empty rejected tasks".to_string());
    }
    let included = record["included"]
        .as_array()
        .ok_or("missing included array")?;
    let base_sat = record["satisfied_in_base"]
        .as_array()
        .ok_or("missing satisfied_in_base array")?;
    if included.is_empty() && base_sat.is_empty() {
        return Err("receipt includes no functional tasks".to_string());
    }
    let tasks = record["accept_verification"]
        .as_array()
        .ok_or("missing accept_verification array")?;
    if !included.is_empty() && tasks.len() != included.len() {
        return Err("accept_verification count does not match included tasks".to_string());
    }
    if tasks
        .iter()
        .any(|t| t["exit_code"].as_i64() != Some(0) || t["passed"].as_bool() != Some(true))
    {
        return Err("one or more accept_verification checks did not pass".to_string());
    }
    Ok(((*parent).to_string(), verified_name, record))
}

fn synthesize_promoted_release_envelope(
    repo: &Path,
    final_commit: &str,
    source_record: &Value,
    target_name: &str,
) -> EaiResult<String> {
    let mut promoted = source_record.clone();
    let Some(map) = promoted.as_object_mut() else {
        return Err(EaiError::internal(
            "distillation receipt is not a JSON object",
        ));
    };
    map.insert("name".to_string(), json!(target_name));
    map.insert(
        "branch_ref".to_string(),
        json!(format!("refs/heads/release/{target_name}")),
    );
    map.remove("receipt_sha256");
    let canonical = serde_json::to_vec(&promoted)
        .map_err(|e| EaiError::internal(format!("canonicalize promoted receipt: {e}")))?;
    let sha = susi_paths::hex::encode(Sha256::digest(&canonical).as_slice());
    if let Some(m) = promoted.as_object_mut() {
        m.insert("receipt_sha256".to_string(), Value::String(sha));
    }
    let mut pretty = serde_json::to_vec_pretty(&promoted)
        .map_err(|e| EaiError::internal(format!("format promoted receipt: {e}")))?;
    pretty.push(b'\n');

    let blob_out = run_git(repo, &["hash-object", "-w", "--stdin"], Some(&pretty), &[])?;
    if !blob_out.ok() {
        return Err(EaiError::process(format!(
            "git hash-object failed: {}",
            blob_out.stderr
        )));
    }
    let blob_oid = blob_out.stdout_trimmed();

    // Build updated `releases/distill/<target_name>.json` inside `final_commit^{tree}` via temporary index.
    let tmp_idx = std::env::temp_dir().join(format!(
        "susi-promote-idx-{}-{}",
        std::process::id(),
        now_unix_secs()
    ));
    let idx_str = tmp_idx.to_string_lossy().to_string();
    let env = [("GIT_INDEX_FILE", idx_str.as_str())];
    let read_res = run_git(repo, &["read-tree", final_commit], None, &env);
    let update_res = read_res.and_then(|r| {
        if !r.ok() {
            return Err(EaiError::process(format!(
                "git read-tree failed: {}",
                r.stderr
            )));
        }
        run_git(
            repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob_oid},releases/distill/{target_name}.json"),
            ],
            None,
            &env,
        )
    });
    let tree_res = update_res.and_then(|u| {
        if !u.ok() {
            return Err(EaiError::process(format!(
                "git update-index failed: {}",
                u.stderr
            )));
        }
        run_git(repo, &["write-tree"], None, &env)
    });
    let _ = fs::remove_file(&tmp_idx);
    let tree_out = tree_res?;
    if !tree_out.ok() {
        return Err(EaiError::process(format!(
            "git write-tree failed: {}",
            tree_out.stderr
        )));
    }
    let tree_oid = tree_out.stdout_trimmed();
    let now = now_unix_secs();
    let date_val = format!("{now} +0000");
    let msg = format!("evidence(distill): promote verified release candidate {target_name}\n");
    let commit_out = run_git(
        repo,
        &["commit-tree", &tree_oid, "-p", final_commit],
        Some(msg.as_bytes()),
        &[
            ("GIT_AUTHOR_NAME", GOVERNOR_AUTHOR_NAME),
            ("GIT_AUTHOR_EMAIL", GOVERNOR_AUTHOR_EMAIL),
            ("GIT_AUTHOR_DATE", &date_val),
            ("GIT_COMMITTER_NAME", GOVERNOR_AUTHOR_NAME),
            ("GIT_COMMITTER_EMAIL", GOVERNOR_AUTHOR_EMAIL),
            ("GIT_COMMITTER_DATE", &date_val),
        ],
    )?;
    if !commit_out.ok() {
        return Err(EaiError::process(format!(
            "git commit-tree failed: {}",
            commit_out.stderr
        )));
    }
    Ok(commit_out.stdout_trimmed())
}

/// Audit all local `release/*`, legacy remote `release/*`, and immutable `susi-distill/*`
/// candidates, and optionally promote the latest verified candidate onto local-only
/// `refs/heads/release/<name>` and remote `refs/tags/susi-distill/<name>`.
pub fn promote_release_candidate(
    config: &ReleasePromotionConfig,
) -> EaiResult<ReleasePromotionReceipt> {
    let remote = validate_remote_name(&config.remote)?;
    if config.fetch_remote {
        let fetch = run_git(
            &config.repo,
            &[
                "fetch",
                "--no-tags",
                remote,
                &format!("+refs/heads/release/*:refs/remotes/{remote}/release/*"),
                "+refs/tags/susi-distill/*:refs/tags/susi-distill/*",
            ],
            None,
            &[],
        )?;
        if !fetch.ok() {
            return Err(EaiError::process(format!(
                "git fetch {remote} release candidates failed: {}",
                fetch.stderr
            )));
        }
    }

    let rem_prefix = format!("refs/remotes/{remote}/release/");
    let loc_prefix = "refs/heads/release/";
    let tag_prefix = "refs/tags/susi-distill/";
    let refs_raw = git_ok(
        &config.repo,
        &[
            "for-each-ref",
            "--format=%(refname)\t%(objectname)\t%(*objectname)",
            loc_prefix,
            &rem_prefix,
            tag_prefix,
        ],
    )?;

    struct ParsedReleaseCandidate {
        audit: ReleaseCandidateAudit,
        verified_payload: Option<(String, Value)>,
    }

    let mut seen_oid: BTreeSet<String> = BTreeSet::new();
    let mut parsed_candidates: Vec<ParsedReleaseCandidate> = Vec::new();
    for line in refs_raw.lines() {
        let mut parts = line.split('\t');
        let (Some(full_ref), Some(obj_oid)) =
            (parts.next().map(str::trim), parts.next().map(str::trim))
        else {
            continue;
        };
        let peeled = parts.next().map(str::trim).unwrap_or("");
        let oid = if !peeled.is_empty() && is_oid(peeled) {
            peeled
        } else {
            obj_oid
        };
        if !is_oid(oid) || !seen_oid.insert(oid.to_string()) {
            continue;
        }
        let (label_fallback, expected_name) = if let Some(rel) = full_ref
            .strip_prefix(loc_prefix)
            .or_else(|| full_ref.strip_prefix(&rem_prefix))
        {
            (rel.to_string(), Some(rel))
        } else if let Some(tag_name) = full_ref.strip_prefix(tag_prefix) {
            (tag_name.to_string(), None)
        } else {
            continue;
        };
        match inspect_distill_envelope(&config.repo, oid, expected_name) {
            Ok((final_commit, verified_name, record)) => {
                let inc = record["included"].as_array().map_or(0, Vec::len);
                let sat = record["satisfied_in_base"].as_array().map_or(0, Vec::len);
                parsed_candidates.push(ParsedReleaseCandidate {
                    audit: ReleaseCandidateAudit {
                        git_ref: full_ref.to_string(),
                        release_name: verified_name,
                        head_oid: oid.to_string(),
                        final_commit_oid: Some(final_commit.clone()),
                        receipt_verified: true,
                        qualification_status: "accept_verified_candidate".to_string(),
                        included_task_count: inc,
                        satisfied_in_base_count: sat,
                        role: String::new(),
                        detail: format!(
                            "verified susi-distillation-receipt-v1 ({inc} included, {sat} base-satisfied)"
                        ),
                    },
                    verified_payload: Some((final_commit, record)),
                });
            }
            Err(reason) => {
                parsed_candidates.push(ParsedReleaseCandidate {
                    audit: ReleaseCandidateAudit {
                        git_ref: full_ref.to_string(),
                        release_name: label_fallback,
                        head_oid: oid.to_string(),
                        final_commit_oid: None,
                        receipt_verified: false,
                        qualification_status: "historical_or_composed_snapshot".to_string(),
                        included_task_count: 0,
                        satisfied_in_base_count: 0,
                        role: "unverified_or_historical".to_string(),
                        detail: reason,
                    },
                    verified_payload: None,
                });
            }
        }
    }

    let is_semver =
        |s: &str| s.len() >= 2 && s.starts_with('v') && s.as_bytes()[1].is_ascii_digit();
    let best_idx = parsed_candidates
        .iter()
        .enumerate()
        .filter(|(_, item)| item.audit.receipt_verified)
        .max_by(|(_, a), (_, b)| {
            let sa = a.audit.included_task_count + a.audit.satisfied_in_base_count;
            let sb = b.audit.included_task_count + b.audit.satisfied_in_base_count;
            is_semver(&a.audit.release_name)
                .cmp(&is_semver(&b.audit.release_name))
                .then_with(|| {
                    natural_rc_key(&a.audit.release_name)
                        .cmp(&natural_rc_key(&b.audit.release_name))
                })
                .then_with(|| sa.cmp(&sb))
        })
        .map(|(i, _)| i);

    let mut latest_verified_branch = None;
    for (idx, item) in parsed_candidates.iter_mut().enumerate() {
        if item.audit.receipt_verified {
            if Some(idx) == best_idx {
                item.audit.role = "latest_verified_candidate".to_string();
                latest_verified_branch = Some(format!("release/{}", item.audit.release_name));
            } else {
                item.audit.role = "superseded_verified_candidate".to_string();
            }
        }
    }

    let mut promoted_branch_ref = None;
    let mut promoted_commit_oid = None;
    let mut published_snapshot_tag = None;
    let mut published_remote_oid = None;

    if let Some(target_name) = config.target_release_name.as_deref() {
        let target_name = target_name.trim();
        if !valid_release_name(target_name) {
            return Err(EaiError::config(format!(
                "invalid release branch name {target_name:?} (expected lowercase ASCII, digits, `-`, `.`)"
            )));
        }
        let (final_commit, source_record) = if let Some(explicit_rev) =
            config.candidate_rev.as_deref()
        {
            let oid = git_ok(
                &config.repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", explicit_rev.trim()),
                ],
            )?;
            let (fc, _, rec) = inspect_distill_envelope(&config.repo, &oid, None).map_err(|e| {
                EaiError::config(format!(
                    "refusing release promotion of unqualified commit {oid}: {e}"
                ))
            })?;
            (fc, rec)
        } else if let Some(idx) = best_idx {
            parsed_candidates[idx]
                .verified_payload
                .clone()
                .ok_or_else(|| EaiError::internal("missing verified candidate payload"))?
        } else {
            return Err(EaiError::config(
                "no verified distillation candidate found to promote",
            ));
        };

        let local_release_ref = format!("refs/heads/release/{target_name}");
        let snapshot_tag_ref = format!("refs/tags/susi-distill/{target_name}");

        let envelope_oid = if let Ok(existing_local) =
            git_ok(&config.repo, &["rev-parse", "--verify", &local_release_ref])
        {
            let (existing_final, _, _) =
                inspect_distill_envelope(&config.repo, &existing_local, Some(target_name))
                    .map_err(|e| {
                        EaiError::config(format!(
                            "existing local {local_release_ref} is invalid: {e}"
                        ))
                    })?;
            if existing_final != final_commit {
                return Err(EaiError::config(format!(
                    "local {local_release_ref} already wraps verified commit {existing_final}; choose a fresh candidate name for {final_commit}"
                )));
            }
            existing_local
        } else {
            let created = synthesize_promoted_release_envelope(
                &config.repo,
                &final_commit,
                &source_record,
                target_name,
            )?;
            if !config.dry_run {
                let zero = "0".repeat(created.len());
                let upd = run_git(
                    &config.repo,
                    &["update-ref", &local_release_ref, &created, &zero],
                    None,
                    &[],
                )?;
                if !upd.ok() {
                    return Err(EaiError::process(format!(
                        "git update-ref {local_release_ref} failed: {}",
                        upd.stderr
                    )));
                }
            }
            created
        };

        promoted_branch_ref = Some(local_release_ref);
        promoted_commit_oid = Some(envelope_oid.clone());
        published_snapshot_tag = Some(snapshot_tag_ref.clone());

        if !config.dry_run {
            if let Some(existing_tag) = confirm_remote_ref(&config.repo, remote, &snapshot_tag_ref)?
            {
                let _ = run_git(
                    &config.repo,
                    &["fetch", "--no-tags", remote, &existing_tag],
                    None,
                    &[],
                );
                let ok_same = existing_tag == envelope_oid
                    || existing_tag == final_commit
                    || inspect_distill_envelope(&config.repo, &existing_tag, Some(target_name))
                        .is_ok_and(|(fc, _, _)| fc == final_commit);
                if !ok_same {
                    return Err(EaiError::config(format!(
                        "immutable snapshot {snapshot_tag_ref} already exists on {remote} at {existing_tag}; choose a fresh candidate name"
                    )));
                }
                published_remote_oid = Some(existing_tag);
            } else {
                let _ = run_git(
                    &config.repo,
                    &["update-ref", &snapshot_tag_ref, &envelope_oid],
                    None,
                    &[],
                );
                let push = run_git(
                    &config.repo,
                    &[
                        "push",
                        remote,
                        &format!("{envelope_oid}:{snapshot_tag_ref}"),
                    ],
                    None,
                    &[],
                )?;
                if !push.ok() {
                    return Err(EaiError::process(format!(
                        "git push {remote} {envelope_oid}:{snapshot_tag_ref} failed: {}",
                        push.stderr
                    )));
                }
                published_remote_oid = confirm_remote_ref(&config.repo, remote, &snapshot_tag_ref)?;
            }
        }
    }

    let candidates: Vec<ReleaseCandidateAudit> = parsed_candidates
        .into_iter()
        .map(|item| item.audit)
        .collect();
    let blockers = vec![
        "Immutable production tag `refs/tags/vX.Y.Z` requires committed exact-tree qualification record `releases/qualification/vX.Y.Z.json` (`release-qualify`).".to_string(),
        "Official dual-platform core release requires both native `x86_64-unknown-linux-gnu` and native `x86_64-pc-windows-msvc` host qualification.".to_string(),
    ];

    let mut receipt = ReleasePromotionReceipt {
        schema: RELEASE_PROMOTION_RECEIPT_SCHEMA.to_string(),
        stream: WorkflowStreamKind::QualifiedReleasePromotion
            .as_str()
            .to_string(),
        remote: remote.to_string(),
        dry_run: config.dry_run,
        generated_unix: now_unix_secs(),
        latest_verified_branch,
        promoted_branch_ref,
        promoted_commit_oid,
        published_snapshot_tag,
        published_remote_oid,
        qualification_status: "accept_verified_candidate".to_string(),
        production_tag_blockers: blockers,
        candidates,
        receipt_sha256: String::new(),
    };
    let mut val = serde_json::to_value(&receipt)
        .map_err(|e| EaiError::internal(format!("serialize release promotion receipt: {e}")))?;
    receipt.receipt_sha256 = compute_receipt_sha256(&mut val)?;
    write_receipt_if_requested(config.receipt_out.as_deref(), &val)?;
    Ok(receipt)
}

fn sanitize_legacy_slug(branch_short: &str) -> String {
    let mut out = String::with_capacity(branch_short.len());
    let mut prev_dash = false;
    for ch in branch_short.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_') {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches(['-', '.']);
    if trimmed.is_empty() {
        "branch".to_string()
    } else {
        trimmed.to_string()
    }
}

fn discover_active_claim_branches(repo: &Path) -> EaiResult<BTreeMap<String, String>> {
    // Native claim files (`susi-local-claim-v2` text, the only format the
    // coordinator writes) are read by the worktree-hazard scan.
    let mut claimed = inspect_local_worktree_hazards(repo)?;
    let refs_out = run_git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)\t%(objectname)",
            "refs/claims",
        ],
        None,
        &[],
    )?;
    if refs_out.ok() {
        for line in String::from_utf8_lossy(&refs_out.stdout).lines() {
            let mut parts = line.split('\t');
            let (Some(cref), Some(oid)) = (parts.next(), parts.next()) else {
                continue;
            };
            if !is_oid(oid.trim()) {
                continue;
            }
            let blob = run_git(repo, &["cat-file", "blob", oid.trim()], None, &[])?;
            if !blob.ok() {
                continue;
            }
            if let Ok(val) = serde_json::from_slice::<Value>(&blob.stdout) {
                if let Some(br) = val["branch"].as_str().map(str::trim) {
                    let short = br.strip_prefix("refs/heads/").unwrap_or(br);
                    if !short.is_empty() {
                        claimed
                            .entry(short.to_string())
                            .or_insert_with(|| format!("active queue claim {cref} on `{short}`"));
                    }
                }
                if let Some(agent) = val["agent"].as_str().map(str::trim) {
                    if !agent.is_empty() {
                        claimed
                            .entry(format!("local-agent/{}", agent.to_ascii_lowercase()))
                            .or_insert_with(|| format!("active queue claim {cref} by `{agent}`"));
                    }
                }
            }
        }
    }
    Ok(claimed)
}

fn list_remote_heads(repo: &Path, remote: &str) -> EaiResult<Vec<(String, String)>> {
    let out = git_ok(repo, &["ls-remote", "--heads", remote])?;
    let mut heads = Vec::new();
    for line in out.lines() {
        let mut fields = line.split_whitespace();
        let (Some(oid), Some(full_ref)) = (fields.next(), fields.next()) else {
            continue;
        };
        if !is_oid(oid) {
            continue;
        }
        if let Some(short) = full_ref.strip_prefix("refs/heads/") {
            heads.push((short.to_string(), oid.to_string()));
        }
    }
    heads.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(heads)
}

fn ensure_remote_preservation_tag(
    repo: &Path,
    remote: &str,
    slug: &str,
    oid: &str,
) -> EaiResult<String> {
    let primary_tag = format!("refs/tags/susi-distill/legacy-{slug}");
    let chosen_tag = if let Some(existing) = confirm_remote_ref(repo, remote, &primary_tag)? {
        if existing == oid {
            let _ = run_git(repo, &["update-ref", &primary_tag, oid], None, &[]);
            return Ok(primary_tag);
        }
        let short_oid = oid.get(..12).unwrap_or(oid);
        format!("refs/tags/susi-distill/legacy-{slug}-{short_oid}")
    } else {
        primary_tag
    };

    if let Some(existing) = confirm_remote_ref(repo, remote, &chosen_tag)? {
        if existing == oid {
            let _ = run_git(repo, &["update-ref", &chosen_tag, oid], None, &[]);
            return Ok(chosen_tag);
        }
        return Err(EaiError::config(format!(
            "preservation tag {chosen_tag} already exists on {remote} at {existing} (expected {oid})"
        )));
    }

    let _ = run_git(repo, &["update-ref", &chosen_tag, oid], None, &[]);
    let push = run_git(
        repo,
        &["push", remote, &format!("{oid}:{chosen_tag}")],
        None,
        &[],
    )?;
    if !push.ok() {
        return Err(EaiError::process(format!(
            "failed to publish preservation tag {chosen_tag} on {remote}: {}",
            push.stderr
        )));
    }
    match confirm_remote_ref(repo, remote, &chosen_tag)? {
        Some(confirmed) if confirmed == oid => Ok(chosen_tag),
        other => Err(EaiError::process(format!(
            "remote {remote} did not confirm {chosen_tag} at {oid} (got {other:?})"
        ))),
    }
}

fn is_patch_equivalent_in_main(repo: &Path, main_oid: &str, branch_oid: &str) -> bool {
    let Ok(out) = run_git(repo, &["cherry", main_oid, branch_oid], None, &[]) else {
        return false;
    };
    if !out.ok() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut saw_any = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        saw_any = true;
        if !line.starts_with("- ") {
            return false;
        }
    }
    saw_any
}

/// Preserve and retire non-`main` remote `refs/heads/*` branch clutter on `<remote>`
/// so GitHub serves as pure storage without PR banners, while guaranteeing zero
/// lost commits via immutable `refs/tags/susi-distill/legacy-*` snapshots verified
/// on `<remote>` before any compare-and-swap branch deletion.
pub fn retire_remote_clutter(config: &RetireBranchesConfig) -> EaiResult<RetireBranchesReceipt> {
    let remote = validate_remote_name(&config.remote)?;
    if config.fetch_remote {
        let fetch = run_git(
            &config.repo,
            &[
                "fetch",
                "--prune",
                "--no-tags",
                remote,
                &format!("+refs/heads/*:refs/remotes/{remote}/*"),
                "+refs/tags/susi-distill/*:refs/tags/susi-distill/*",
                "+refs/claims/*:refs/claims/*",
            ],
            None,
            &[],
        )?;
        if !fetch.ok() {
            return Err(EaiError::process(format!(
                "git fetch {remote} failed: {}",
                fetch.stderr
            )));
        }
    }

    let main_oid = git_ok(
        &config.repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/{remote}/main"),
        ],
    )
    .or_else(|_| git_ok(&config.repo, &["rev-parse", "--verify", "refs/heads/main"]))?;
    let retired_rules = load_retired_rules(&config.repo, &main_oid, remote)?;
    let claimed_branches = discover_active_claim_branches(&config.repo)?;
    let remote_heads = list_remote_heads(&config.repo, remote)?;

    let mut entries = Vec::with_capacity(remote_heads.len());
    let mut retired_count = 0usize;
    let mut skipped_count = 0usize;
    let mut refused_count = 0usize;

    for (short, oid) in remote_heads {
        let remote_ref = format!("refs/heads/{short}");
        if short == "main" {
            skipped_count += 1;
            entries.push(RetiredBranchEntry {
                branch: short,
                remote_ref,
                head_oid: oid,
                in_main: true,
                preserved_snapshot_tag: None,
                disposition: BranchRetireDisposition::ProtectedMain,
                detail: "trunk refs/heads/main is never retired".to_string(),
            });
            continue;
        }
        if !matches_filters(&short, &config.branch_filters) {
            skipped_count += 1;
            continue;
        }

        if run_git(
            &config.repo,
            &["cat-file", "-e", &format!("{oid}^{{commit}}")],
            None,
            &[],
        )
        .map_or(true, |o| !o.ok())
        {
            let _ = run_git(
                &config.repo,
                &["fetch", "--no-tags", remote, &oid],
                None,
                &[],
            );
        }

        let in_main = is_ancestor(&config.repo, &oid, &main_oid)?
            || is_patch_equivalent_in_main(&config.repo, &main_oid, &oid);
        let retired_rule = find_retired_rule(&short, &oid, &retired_rules);

        if short.starts_with("local-agent/") {
            if !config.include_local_agent {
                skipped_count += 1;
                entries.push(RetiredBranchEntry {
                    branch: short,
                    remote_ref,
                    head_oid: oid,
                    in_main,
                    preserved_snapshot_tag: None,
                    disposition: BranchRetireDisposition::SkippedLocalAgentOptOut,
                    detail:
                        "pass --include-local-agent to retire merged/RETIRE local-agent/* heads"
                            .to_string(),
                });
                continue;
            }
            if let Some(claim_why) = claimed_branches.get(&short) {
                skipped_count += 1;
                entries.push(RetiredBranchEntry {
                    branch: short,
                    remote_ref,
                    head_oid: oid,
                    in_main,
                    preserved_snapshot_tag: None,
                    disposition: BranchRetireDisposition::SkippedActiveClaim,
                    detail: claim_why.clone(),
                });
                continue;
            }
            if !in_main && retired_rule.is_none() {
                skipped_count += 1;
                entries.push(RetiredBranchEntry {
                    branch: short,
                    remote_ref,
                    head_oid: oid,
                    in_main: false,
                    preserved_snapshot_tag: None,
                    disposition: BranchRetireDisposition::SkippedUnmergedActive,
                    detail: "local-agent/* tip is not in main and has no RETIRE governance record"
                        .to_string(),
                });
                continue;
            }
        } else if let Some(claim_why) = claimed_branches.get(&short) {
            skipped_count += 1;
            entries.push(RetiredBranchEntry {
                branch: short,
                remote_ref,
                head_oid: oid,
                in_main,
                preserved_snapshot_tag: None,
                disposition: BranchRetireDisposition::SkippedActiveClaim,
                detail: claim_why.clone(),
            });
            continue;
        }

        let slug = sanitize_legacy_slug(&short);
        if config.dry_run {
            retired_count += 1;
            entries.push(RetiredBranchEntry {
                branch: short,
                remote_ref,
                head_oid: oid,
                in_main,
                preserved_snapshot_tag: Some(format!("refs/tags/susi-distill/legacy-{slug}")),
                disposition: BranchRetireDisposition::WouldRetire,
                detail: if in_main {
                    "dry-run: already in main; would confirm legacy snapshot tag and delete remote branch"
                        .to_string()
                } else if let Some(r) = retired_rule {
                    format!(
                        "dry-run: RETIRE ledger {} ({}); would preserve snapshot tag and delete remote branch",
                        r.task_id, r.reason
                    )
                } else {
                    "dry-run: would preserve immutable snapshot tag on remote before deleting remote branch"
                        .to_string()
                },
            });
            continue;
        }

        let preserved_tag = ensure_remote_preservation_tag(&config.repo, remote, &slug, &oid)?;

        let lease = format!("--force-with-lease={remote_ref}:{oid}");
        let del_spec = format!(":{remote_ref}");
        let del = run_git(
            &config.repo,
            &["push", &lease, remote, &del_spec],
            None,
            &[],
        )?;
        if del.ok() && confirm_remote_ref(&config.repo, remote, &remote_ref)?.is_none() {
            let _ = run_git(
                &config.repo,
                &[
                    "update-ref",
                    "-d",
                    &format!("refs/remotes/{remote}/{short}"),
                ],
                None,
                &[],
            );
            retired_count += 1;
            entries.push(RetiredBranchEntry {
                branch: short,
                remote_ref,
                head_oid: oid,
                in_main,
                preserved_snapshot_tag: Some(preserved_tag.clone()),
                disposition: BranchRetireDisposition::Retired,
                detail: format!("preserved at {preserved_tag} and retired from refs/heads/*"),
            });
        } else {
            refused_count += 1;
            entries.push(RetiredBranchEntry {
                branch: short,
                remote_ref,
                head_oid: oid,
                in_main,
                preserved_snapshot_tag: Some(preserved_tag),
                disposition: BranchRetireDisposition::RefusedByRemote,
                detail: format!(
                    "preserved on remote, but deletion exited {}: {}",
                    del.code, del.stderr
                ),
            });
        }
    }

    let remaining_remote_head_count = list_remote_heads(&config.repo, remote)?.len();
    let mut receipt = RetireBranchesReceipt {
        schema: RETIRE_BRANCHES_RECEIPT_SCHEMA.to_string(),
        remote: remote.to_string(),
        dry_run: config.dry_run,
        generated_unix: now_unix_secs(),
        main_oid,
        retired_count,
        skipped_count,
        refused_count,
        remaining_remote_head_count,
        branches: entries,
        receipt_sha256: String::new(),
    };
    let mut val = serde_json::to_value(&receipt)
        .map_err(|e| EaiError::internal(format!("serialize retire-branches receipt: {e}")))?;
    receipt.receipt_sha256 = compute_receipt_sha256(&mut val)?;
    write_receipt_if_requested(config.receipt_out.as_deref(), &val)?;
    Ok(receipt)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RcToken {
    Text(String),
    Num(u64),
}

fn natural_rc_key(name: &str) -> Vec<RcToken> {
    let mut tokens = Vec::new();
    let bytes = name.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            tokens.push(RcToken::Num(name[start..i].parse::<u64>().unwrap_or(0)));
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_digit() {
                i += 1;
            }
            tokens.push(RcToken::Text(name[start..i].to_string()));
        }
    }
    tokens
}

#[cfg(test)]
#[path = "tests/git_governor.rs"]
mod tests;
