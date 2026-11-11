//! The embedded workflow engine.
//!
//! The native coordinator (`local workflow`) refuses a claim whose scopes
//! overlap a live claim, and a task whose dependencies have not landed has
//! nothing to build on. This module makes both rules mechanical inside the
//! harness, so a mission is planned into waves that the coordinator will
//! accept instead of discovering a collision halfway through a batch:
//!
//! - [`normalize_scope`] and [`scopes_overlap`] are the scope algebra: a
//!   scope owns itself and everything below it, compared by path component
//!   and case-insensitively, so `crates/susi-core` owns
//!   `crates/susi-core/src/lib.rs` but not `crates/susi-core-defs`.
//! - [`validate_dependencies`] rejects unknown dependencies and cycles.
//! - [`next_wave`] and [`WorkflowSchedule`] partition pending tasks into
//!   waves whose members are dependency-ready and pairwise scope-disjoint.
//! - [`inspect_repo_workflow`] reads the coordinator's claims, verification
//!   slots and integration head straight from the git common directory, with
//!   no lock taken and no process spawned.
//! - [`IntegrationPolicy`] is the remote-agent mandate (2026-11-08) made
//!   mechanical: one `local-agent/<agent>` branch per agent, published to
//!   `refs/agents/<agent>` (docs/agent-refs.md), never `main`, task trailers
//!   on every commit, destination-first migrations with no stubbed ports, and
//!   the brief lines every worker receives.

use crate::supervisor::PlanTask;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

/// The claim record header the native coordinator writes.
const CLAIM_HEADER: &str = "susi-local-claim-v2";

/// The ref the native coordinator moves on every integration.
const INTEGRATION_REF: &str = "refs/susi/integration/current";

/// Canonicalise a reserved scope.
///
/// Trims whitespace, turns `\` into `/`, strips a leading `./` and trailing
/// `/`, collapses repeated separators, and lowercases ASCII so a Windows and a
/// Linux spelling of the same path compare equal.
///
/// # Errors
///
/// Returns `EaiError::config` for an empty scope, `.` or `..` components, or
/// an absolute path (a leading `/` or a drive letter).
pub fn normalize_scope(raw: &str) -> EaiResult<String> {
    let slashed = raw.trim().replace('\\', "/");
    if slashed.starts_with('/') {
        return Err(EaiError::config(format!(
            "scope {raw:?} must be repo-relative"
        )));
    }
    if slashed.as_bytes().get(1) == Some(&b':') {
        return Err(EaiError::config(format!(
            "scope {raw:?} must be repo-relative, not name a drive"
        )));
    }
    let mut rest = slashed.as_str();
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part {
            "" => {}
            "." | ".." => {
                return Err(EaiError::config(format!(
                    "scope {raw:?} must not contain `.` or `..`"
                )))
            }
            other => parts.push(other.to_ascii_lowercase()),
        }
    }
    if parts.is_empty() {
        return Err(EaiError::config(format!("scope {raw:?} is empty")));
    }
    Ok(parts.join("/"))
}

/// True when two normalized scopes reserve any path in common: they are
/// equal, or one is a path-component prefix of the other.
#[must_use]
pub fn scopes_overlap(a: &str, b: &str) -> bool {
    a == b
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
}

/// True iff no scope in `a_scopes` overlaps any scope in `b_scopes`, after
/// normalization.
///
/// A scope that cannot be normalized is treated as overlapping everything:
/// an unreadable reservation must never be reported as free.
#[must_use]
pub fn task_scopes_disjoint(a_scopes: &[String], b_scopes: &[String]) -> bool {
    let (Ok(a), Ok(b)) = (normalize_all(a_scopes), normalize_all(b_scopes)) else {
        return false;
    };
    disjoint(&a, &b)
}

fn normalize_all(scopes: &[String]) -> EaiResult<Vec<String>> {
    scopes.iter().map(|scope| normalize_scope(scope)).collect()
}

fn disjoint(a: &[String], b: &[String]) -> bool {
    !a.iter()
        .any(|left| b.iter().any(|right| scopes_overlap(left, right)))
}

/// What the scheduler needs to know about a task.
pub trait WorkflowTask {
    /// The task's unique key within its plan.
    fn key(&self) -> &str;
    /// Keys of the tasks that must land first.
    fn deps(&self) -> &[String];
    /// The paths the task reserves.
    fn scopes(&self) -> &[String];
}

impl WorkflowTask for PlanTask {
    fn key(&self) -> &str {
        &self.key
    }

    fn deps(&self) -> &[String] {
        &self.deps
    }

    fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

impl<T: WorkflowTask + ?Sized> WorkflowTask for &T {
    fn key(&self) -> &str {
        (**self).key()
    }

    fn deps(&self) -> &[String] {
        (**self).deps()
    }

    fn scopes(&self) -> &[String] {
        (**self).scopes()
    }
}

/// Check that every dependency names a task in `tasks` and that the
/// dependency graph is acyclic.
///
/// # Errors
///
/// Returns `EaiError::config` naming the unknown dependency, or the cycle as
/// `a -> b -> a`.
pub fn validate_dependencies<T: WorkflowTask>(tasks: &[T]) -> EaiResult<()> {
    let index: BTreeMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(position, task)| (task.key(), position))
        .collect();
    for task in tasks {
        for dep in task.deps() {
            if !index.contains_key(dep.as_str()) {
                return Err(EaiError::config(format!(
                    "task {} depends on unknown task {dep}",
                    task.key()
                )));
            }
        }
    }
    // Iterative depth-first search: 0 = unvisited, 1 = on the current path,
    // 2 = finished. Reaching a node on the current path closes a cycle.
    let mut mark = vec![0_u8; tasks.len()];
    for root in 0..tasks.len() {
        if mark.get(root).copied() != Some(0) {
            continue;
        }
        let mut path: Vec<usize> = vec![root];
        let mut cursor: Vec<usize> = vec![0];
        if let Some(slot) = mark.get_mut(root) {
            *slot = 1;
        }
        while let (Some(&node), Some(next)) = (path.last(), cursor.last_mut()) {
            let deps = tasks.get(node).map(WorkflowTask::deps).unwrap_or(&[]);
            let Some(dep) = deps.get(*next) else {
                if let Some(slot) = mark.get_mut(node) {
                    *slot = 2;
                }
                path.pop();
                cursor.pop();
                continue;
            };
            *next += 1;
            let Some(&child) = index.get(dep.as_str()) else {
                continue;
            };
            match mark.get(child).copied() {
                Some(0) => {
                    if let Some(slot) = mark.get_mut(child) {
                        *slot = 1;
                    }
                    path.push(child);
                    cursor.push(0);
                }
                Some(1) => {
                    let start = path.iter().position(|&member| member == child).unwrap_or(0);
                    let mut names: Vec<&str> = path
                        .get(start..)
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|&member| tasks.get(member).map(WorkflowTask::key))
                        .collect();
                    names.push(dep.as_str());
                    return Err(EaiError::config(format!(
                        "dependency cycle: {}",
                        names.join(" -> ")
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// One batch of tasks that may prepare at the same time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowWave<T> {
    /// Zero-based position of the wave in the schedule.
    pub index: usize,
    /// The wave's tasks, in plan order.
    pub tasks: Vec<T>,
}

/// Choose the next wave from `pending`.
///
/// A task is eligible once every dependency is in `completed`. Eligible tasks
/// are admitted greedily in plan order, up to `max_parallel`, while their
/// scopes are disjoint from every task already admitted and from
/// `active_claimed_scopes`. A task that collides is deferred, never dropped.
///
/// An empty result with tasks still pending means nothing can start now:
/// dependencies are outstanding or an external claim holds the scopes.
///
/// # Errors
///
/// Returns `EaiError::config` when a task scope or an active claim scope
/// cannot be normalized.
pub fn next_wave<'a, T: WorkflowTask>(
    pending: &[&'a T],
    completed: &BTreeSet<String>,
    active_claimed_scopes: &[String],
    max_parallel: usize,
) -> EaiResult<Vec<&'a T>> {
    let limit = max_parallel.max(1);
    let external = normalize_all(active_claimed_scopes)?;
    let mut reserved: Vec<String> = Vec::new();
    let mut wave = Vec::new();
    for &task in pending {
        if wave.len() >= limit {
            break;
        }
        if !task.deps().iter().all(|dep| completed.contains(dep)) {
            continue;
        }
        let scopes = normalize_all(task.scopes()).map_err(|error| {
            EaiError::config(format!("task {} has an invalid scope: {error}", task.key()))
        })?;
        if !disjoint(&scopes, &external) || !disjoint(&scopes, &reserved) {
            continue;
        }
        reserved.extend(scopes);
        wave.push(task);
    }
    Ok(wave)
}

/// A whole plan partitioned into waves, assuming every task lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSchedule<T> {
    /// The waves, in run order.
    pub waves: Vec<WorkflowWave<T>>,
    /// Tasks that can never start: an active claim holds their scopes, or
    /// they depend (transitively) on such a task.
    pub blocked: Vec<T>,
}

impl<'a, T: WorkflowTask> WorkflowSchedule<&'a T> {
    /// Partition the tasks not yet in `completed` into waves.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` for an unknown dependency, a dependency
    /// cycle, or a scope that cannot be normalized.
    pub fn build(
        tasks: &'a [T],
        completed: &BTreeSet<String>,
        active_claimed_scopes: &[String],
        max_parallel: usize,
    ) -> EaiResult<Self> {
        validate_dependencies(tasks)?;
        let mut done = completed.clone();
        let mut pending: Vec<&'a T> = tasks
            .iter()
            .filter(|task| !completed.contains(task.key()))
            .collect();
        let mut waves = Vec::new();
        while !pending.is_empty() {
            let wave = next_wave(&pending, &done, active_claimed_scopes, max_parallel)?;
            if wave.is_empty() {
                break;
            }
            let admitted: BTreeSet<&str> = wave.iter().map(|task| task.key()).collect();
            pending.retain(|task| !admitted.contains(task.key()));
            done.extend(admitted.iter().map(|key| (*key).to_string()));
            waves.push(WorkflowWave {
                index: waves.len(),
                tasks: wave,
            });
        }
        Ok(Self {
            waves,
            blocked: pending,
        })
    }
}

/// A live claim held with the native coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRecord {
    /// The claim (and worktree) name.
    pub name: String,
    /// The worktree that owns the claim.
    pub worktree: PathBuf,
    /// The commit the claim extends.
    pub base: String,
    /// The paths the claim reserves.
    pub scopes: Vec<String>,
}

/// A verification slot with a recorded owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRecord {
    /// Who holds the slot: the owner record's `agent=`, else the slot name.
    pub agent: String,
    /// The owner's process id, when recorded.
    pub pid: Option<u32>,
    /// The scope key the owner recorded, when any.
    pub scope_key: Option<String>,
}

/// The native coordinator's state as read from disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowSnapshot {
    /// Live claims, sorted by name.
    pub claims: Vec<ClaimRecord>,
    /// Slots with an owner record, sorted by agent.
    pub active_slots: Vec<SlotRecord>,
    /// The integration head, when one was recorded.
    pub integration_head: Option<String>,
}

impl WorkflowSnapshot {
    /// Every scope a live claim reserves, for [`next_wave`].
    #[must_use]
    pub fn claimed_scopes(&self) -> Vec<String> {
        self.claims
            .iter()
            .flat_map(|claim| claim.scopes.iter().cloned())
            .collect()
    }
}

/// Read the native coordinator's claims, verification slots and integration
/// head for the checkout at `repo`.
///
/// Everything is read directly from the git common directory: no lock is
/// taken and no process is spawned, so inspection never waits on, or
/// disturbs, a running coordinator. A slot's owner record is informational;
/// whether its OS lock is still held is not probed.
///
/// # Errors
///
/// Returns `EaiError::config` when `repo` is not a git checkout or a claim is
/// malformed, and `EaiError::io` when a present directory cannot be read.
pub fn inspect_repo_workflow(repo: &Path) -> EaiResult<WorkflowSnapshot> {
    let common = crate::adapter::common_dir(repo)
        .ok_or_else(|| EaiError::config(format!("{} is not a git checkout", repo.display())))?;
    let state = common.join("susi-local-workflow");
    Ok(WorkflowSnapshot {
        claims: read_claims(&state.join("claims"))?,
        active_slots: read_slots(&state.join("slots"))?,
        integration_head: read_integration_head(&common),
    })
}

fn entries(directory: &Path) -> EaiResult<Vec<PathBuf>> {
    let listing = match std::fs::read_dir(directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(EaiError::io(format!(
                "read {}: {error}",
                directory.display()
            )))
        }
    };
    let mut paths = Vec::new();
    for entry in listing {
        let entry = entry
            .map_err(|error| EaiError::io(format!("read {}: {error}", directory.display())))?;
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

fn read_claims(directory: &Path) -> EaiResult<Vec<ClaimRecord>> {
    let mut claims = Vec::new();
    for path in entries(directory)? {
        if path.extension().is_none_or(|ext| ext != "claim") {
            continue;
        }
        claims.push(parse_claim(&path)?);
    }
    claims.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(claims)
}

fn parse_claim(path: &Path) -> EaiResult<ClaimRecord> {
    let invalid = |why: &str| EaiError::config(format!("claim {}: {why}", path.display()));
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| EaiError::io(format!("stat {}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() {
        return Err(invalid("must not be a symlink"));
    }
    let data = std::fs::read_to_string(path)
        .map_err(|error| EaiError::io(format!("read {}: {error}", path.display())))?;
    let mut lines = data.lines();
    if lines.next() != Some(CLAIM_HEADER) {
        return Err(invalid("not a susi-local-claim-v2 record"));
    }
    let name = lines
        .next()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| invalid("missing claim name"))?
        .to_string();
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(name.as_str()) {
        return Err(invalid("file name does not match the claim name"));
    }
    let worktree = lines
        .next()
        .filter(|worktree| !worktree.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| invalid("missing worktree"))?;
    let base = lines
        .next()
        .filter(|base| is_oid(base))
        .ok_or_else(|| invalid("missing or invalid base commit"))?
        .to_string();
    let scopes: Vec<String> = lines
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    if scopes.is_empty() {
        return Err(invalid("reserves no scopes"));
    }
    Ok(ClaimRecord {
        name,
        worktree,
        base,
        scopes,
    })
}

fn read_slots(directory: &Path) -> EaiResult<Vec<SlotRecord>> {
    let mut slots = Vec::new();
    for path in entries(directory)? {
        // `<n>.lock` is the OS lock itself; opening it to probe would race
        // the coordinator, so only the informational owner record is read.
        if path.extension().is_some_and(|ext| ext == "lock") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let owner = if path.is_dir() {
            path.join("owner")
        } else {
            path.clone()
        };
        let Ok(text) = std::fs::read_to_string(&owner) else {
            continue;
        };
        slots.push(parse_slot(name, &text));
    }
    slots.sort_by(|a, b| a.agent.cmp(&b.agent));
    Ok(slots)
}

fn parse_slot(name: &str, text: &str) -> SlotRecord {
    let mut slot = SlotRecord {
        agent: name.to_string(),
        pid: None,
        scope_key: None,
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "pid" => slot.pid = value.parse().ok(),
            "agent" if !value.is_empty() => slot.agent = value.to_string(),
            "scope" | "scope_key" if !value.is_empty() => slot.scope_key = Some(value.to_string()),
            _ => {}
        }
    }
    slot
}

fn read_integration_head(common: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(common.join(INTEGRATION_REF)) {
        let oid = text.trim();
        if is_oid(oid) {
            return Some(oid.to_string());
        }
    }
    // After `git pack-refs` the loose ref is gone and the head lives here.
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (oid, reference) = line.split_once(' ')?;
        (reference.trim() == INTEGRATION_REF && is_oid(oid)).then(|| oid.to_string())
    })
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// Integration policy for autonomous and remote agents (Mandates 2026-11-08..11).
const GATE_ONLY_REFS: &[&str] = &["main", "refs/heads/main", "HEAD"];

/// Protected release/candidate refs that worker pushes never move directly.
#[rustfmt::skip]
const PROTECTED_RELEASE_PREFIXES: &[&str] = &[
    "release/", "release-staging/", "refs/heads/release/", "refs/heads/release-staging/",
    "refs/tags/v", "refs/tags/susi-release/", "refs/tags/susi-snapshot/",
];

/// Distillation staging prefixes governed separately from unqualified `main`.
#[rustfmt::skip]
const DISTILLATION_REF_PREFIXES: &[&str] = &[
    "distill/", "refs/heads/distill/", "refs/tags/susi-distill/",
];

/// Ref prefixes the native coordinator owns (claims, receipts, integration
/// pointers).
const COORDINATOR_REF_PREFIXES: &[&str] = &["refs/susi/", "refs/backup/"];

/// Where agents publish: a non-branch ref, so the host offers no pull request.
pub const AGENT_REF_PREFIX: &str = "refs/agents/";

/// Mandate 73 Git storage stream classification.
///
/// Separates fast, ungated development integration onto `main` from distilled
/// candidate staging and fully verified release promotion (`release/*`, `v*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStreamKind {
    /// Unqualified development integration (`main` / `refs/heads/main`).
    UnqualifiedDevelopmentMain,
    /// Agent ref (`refs/agents/*`) or development branch (`local-agent/*`, `wip/*`, etc.).
    DevelopmentAgentBranch,
    /// Distilled candidate branch or distillation receipt tag (`distill/*`, `susi-distill/*`).
    DistillationStream,
    /// Qualified release candidate branch or immutable release tag (`release/*`, `v*`).
    QualifiedReleasePromotion,
    /// Internal coordinator, backup, or symbolic ref (`refs/susi/*`, `HEAD`).
    InternalCoordinationRef,
}

impl WorkflowStreamKind {
    /// Stable machine-readable identifier recorded in Susi integration receipts.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::UnqualifiedDevelopmentMain => "unqualified_development_main",
            Self::DevelopmentAgentBranch => "development_agent_branch",
            Self::DistillationStream => "distillation_stream",
            Self::QualifiedReleasePromotion => "qualified_release_promotion",
            Self::InternalCoordinationRef => "internal_coordination_ref",
        }
    }

    /// True iff this ref belongs to a development branch eligible for
    /// reconciliation into unqualified development `main`.
    #[must_use]
    pub const fn eligible_for_unqualified_main_sweep(&self) -> bool {
        matches!(self, Self::DevelopmentAgentBranch)
    }
}

/// Classify a local, remote-tracking, or full Git ref under Mandate 73's
/// stream boundary rules.
#[must_use]
pub fn classify_git_ref_stream(refname: &str, remote: Option<&str>) -> WorkflowStreamKind {
    let trimmed = refname.trim();
    if trimmed.is_empty() || trimmed == "HEAD" || trimmed.ends_with("/HEAD") {
        return WorkflowStreamKind::InternalCoordinationRef;
    }
    if COORDINATOR_REF_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return WorkflowStreamKind::InternalCoordinationRef;
    }
    let short = trimmed
        .strip_prefix("refs/heads/")
        .or_else(|| {
            remote.and_then(|r| {
                let r = r.trim();
                trimmed
                    .strip_prefix(&format!("refs/remotes/{r}/"))
                    .or_else(|| trimmed.strip_prefix(&format!("{r}/")))
            })
        })
        .unwrap_or(trimmed);
    if PROTECTED_RELEASE_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix) || short.starts_with(prefix))
    {
        return WorkflowStreamKind::QualifiedReleasePromotion;
    }
    if DISTILLATION_REF_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix) || short.starts_with(prefix))
    {
        return WorkflowStreamKind::DistillationStream;
    }
    if trimmed.starts_with("refs/tags/") {
        return WorkflowStreamKind::InternalCoordinationRef;
    }
    if short == "main" {
        return WorkflowStreamKind::UnqualifiedDevelopmentMain;
    }
    WorkflowStreamKind::DevelopmentAgentBranch
}

/// How agents publish development contributions under [`IntegrationPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublicationMode {
    /// Branch-only mode (2026-11-08): agents push only `refs/agents/<agent>`
    /// once per gated cycle.
    #[default]
    BranchOnly,
    /// Mandate 73 autonomous fast-main mode (2026-11-10): every agent works in
    /// isolation (own worktree, branch, build output and scratch state) and may
    /// publish non-force fast-forwards directly to unqualified development
    /// `main` (`main` / `refs/heads/main`) or its own `refs/agents/<agent>`
    /// ref with zero mandatory cooldown, while coordinator refs, release
    /// branches/tags, and foreign agent refs stay strictly refused.
    FastMainStream,
}

/// Where an agent's work may be published and what a move out of core must
/// prove before core deletes anything.
///
/// Supports both the 2026-11-08 branch-only policy ([`Self::branch_only`]) and
/// CEO Mandate 73's autonomous fast-main policy ([`Self::fast_main`]), while
/// preserving every safety invariant: owned identity, disjoint scopes, `Task:`
/// trailers, protected coordinator/release refs, and destination-first
/// migrations with zero stubbed ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPolicy {
    /// Prefix of the per-agent isolated local worktree branch (`local-agent/`).
    pub agent_branch_prefix: String,
    /// Shortest gated cycle in seconds (`0` in [`PublicationMode::FastMainStream`]).
    pub gate_cycle_secs: u64,
    /// Active publication mode (`BranchOnly` or `FastMainStream`).
    pub mode: PublicationMode,
}

impl Default for IntegrationPolicy {
    fn default() -> Self {
        Self::branch_only()
    }
}

/// Why a publish or a migration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyViolation {
    /// The agent name cannot name a branch.
    BadAgentName(String),
    /// The target is `main` or `HEAD` when restricted to branch-only mode.
    GateOnlyRef(String),
    /// The target is a protected release branch or immutable release tag.
    ProtectedReleaseRef(String),
    /// The target is the coordinator's own ref space.
    CoordinatorRef(String),
    /// The target is another agent's ref.
    ForeignBranch {
        /// The ref the agent tried to push.
        target: String,
        /// The ref the agent owns.
        own: String,
    },
    /// The target is not under the agent ref prefix at all.
    NotAnAgentBranch(String),
    /// A remote `local-agent/*` branch (retired 2026-11-11): (target, agent ref).
    RetiredAgentBranch(String, String),
    /// A commit in the batch carries no `Task: T-<AGENT>-<n>` trailer.
    MissingTaskTrailer {
        /// The commit's first line.
        subject: String,
    },
    /// A commit's trailer names another agent's task.
    ForeignTaskTrailer {
        /// The commit's first line.
        subject: String,
        /// The trailer found.
        trailer: String,
    },
    /// A core deletion with no destination commit on the commercial origin.
    DeletionBeforeDestination {
        /// The module being removed.
        module: String,
    },
    /// The destination commit exists but the module is not declared, or
    /// did not compile and pass its tests there.
    DestinationUnproven {
        /// The module being removed.
        module: String,
        /// Which proof is missing.
        missing: &'static str,
    },
    /// A capability's port is kept while its implementation is removed.
    StubbedCapability {
        /// The port or function left answering an error.
        port: String,
    },
}

impl std::fmt::Display for PolicyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadAgentName(name) => write!(f, "agent name {name:?} cannot name a branch"),
            Self::GateOnlyRef(target) => write!(
                f,
                "{target} is moved only by the gated host; publish to your refs/agents/<agent> ref"
            ),
            Self::ProtectedReleaseRef(target) => write!(
                f,
                "{target} is a protected release ref; release branches and immutable tags require full exact-tree qualification"
            ),
            Self::CoordinatorRef(target) => {
                write!(f, "{target} belongs to the coordinator; agents never move refs/susi/*")
            }
            Self::ForeignBranch { target, own } => {
                write!(f, "{target} belongs to another agent; yours is {own}")
            }
            Self::NotAnAgentBranch(target) => {
                write!(f, "{target} is not your refs/agents/<agent> ref; agents publish nowhere else")
            }
            Self::RetiredAgentBranch(target, publish_ref) => write!(
                f,
                "{target} is a retired remote agent branch; publish to {publish_ref} instead (docs/agent-refs.md)"
            ),
            Self::MissingTaskTrailer { subject } => {
                write!(f, "commit {subject:?} carries no `Task: T-<AGENT>-<n>` trailer")
            }
            Self::ForeignTaskTrailer { subject, trailer } => {
                write!(f, "commit {subject:?} names another agent's task ({trailer})")
            }
            Self::DeletionBeforeDestination { module } => write!(
                f,
                "{module}: core may delete only after the destination commit is on the commercial origin"
            ),
            Self::DestinationUnproven { module, missing } => {
                write!(f, "{module}: destination commit exists but {missing}")
            }
            Self::StubbedCapability { port } => write!(
                f,
                "{port}: a port that answers an error for a removed capability is a defect; keep an implementation, serve a plug, or drop the port with its callers"
            ),
        }
    }
}

impl std::error::Error for PolicyViolation {}

/// What a migration out of core must prove before core deletes the module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationProof {
    /// The module leaving core (`crates/susi-gemi/src/engines/alpha.rs`).
    pub module: String,
    /// The commit on the commercial origin that holds it, when there is one.
    pub destination_commit: Option<String>,
    /// The module is declared in its destination crate's module tree (not a
    /// copy nothing reaches).
    pub destination_declared: bool,
    /// The destination crate compiles with it.
    pub destination_compiles: bool,
    /// The destination's tests for it pass.
    pub destination_tests_pass: bool,
    /// Ports that core keeps for the capability; each must name what serves
    /// it after the move (`Some(plug)`), or be dropped with its callers
    /// (`None` is only allowed when `port_dropped` says so).
    pub ports_kept: Vec<(String, Option<String>)>,
}

impl IntegrationPolicy {
    /// Branch-only integration policy (2026-11-08 remote rule).
    #[must_use]
    pub fn branch_only() -> Self {
        Self {
            agent_branch_prefix: "local-agent/".to_string(),
            gate_cycle_secs: 20 * 60,
            mode: PublicationMode::BranchOnly,
        }
    }

    /// Mandate 73 autonomous fast-main policy (2026-11-10): every agent works
    /// in full isolation and publishes directly to unqualified development
    /// `main` or its own `refs/agents/<agent>` ref with zero gate cooldown
    /// and zero dependency on any single coordinator agent.
    #[must_use]
    pub fn fast_main() -> Self {
        Self {
            agent_branch_prefix: "local-agent/".to_string(),
            gate_cycle_secs: 0,
            mode: PublicationMode::FastMainStream,
        }
    }

    /// The one local branch `agent` works on; it publishes [`Self::publish_ref`].
    ///
    /// # Errors
    ///
    /// Returns [`PolicyViolation::BadAgentName`] when the name is empty or
    /// contains anything but ASCII letters, digits, `-` and `_`.
    pub fn publish_branch(&self, agent: &str) -> Result<String, PolicyViolation> {
        let name = agent.trim();
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(PolicyViolation::BadAgentName(agent.to_string()));
        }
        Ok(format!(
            "{}{}",
            self.agent_branch_prefix,
            name.to_ascii_lowercase()
        ))
    }

    /// The one remote ref `agent` publishes to (`refs/agents/<agent>`).
    ///
    /// # Errors
    ///
    /// The same [`PolicyViolation::BadAgentName`] as [`Self::publish_branch`].
    pub fn publish_ref(&self, agent: &str) -> Result<String, PolicyViolation> {
        let branch = self.publish_branch(agent)?;
        Ok(branch.replacen(&self.agent_branch_prefix, AGENT_REF_PREFIX, 1))
    }

    /// Whether `agent` may push `target` (a branch name or a full ref).
    ///
    /// # Errors
    ///
    /// Refuses coordinator refs (`refs/susi/*`), protected release refs
    /// (`release/*`, `refs/tags/v*`), symbolic `HEAD`, foreign agent refs,
    /// remote `local-agent/*` branches, and — in [`PublicationMode::BranchOnly`] —
    /// `main` / `refs/heads/main`. In [`PublicationMode::FastMainStream`],
    /// normal fast-forward pushes to `main` and `refs/agents/<agent>` are permitted.
    pub fn check_push_target(&self, agent: &str, target: &str) -> Result<(), PolicyViolation> {
        let own = self.publish_ref(agent)?;
        let target = target.trim();
        let short = target.strip_prefix("refs/heads/").unwrap_or(target);
        if COORDINATOR_REF_PREFIXES
            .iter()
            .any(|prefix| target.starts_with(prefix))
        {
            return Err(PolicyViolation::CoordinatorRef(target.to_string()));
        }
        if PROTECTED_RELEASE_PREFIXES
            .iter()
            .chain(DISTILLATION_REF_PREFIXES)
            .chain(["wip/", "refs/heads/wip/"].iter())
            .any(|prefix| target.starts_with(prefix) || short.starts_with(prefix))
        {
            return Err(PolicyViolation::ProtectedReleaseRef(target.to_string()));
        }
        if target == "HEAD" {
            return Err(PolicyViolation::GateOnlyRef(target.to_string()));
        }
        if target.starts_with(AGENT_REF_PREFIX) && target.to_ascii_lowercase() == own {
            return Ok(());
        } else if target.starts_with(AGENT_REF_PREFIX) {
            let target = target.to_string();
            return Err(PolicyViolation::ForeignBranch { target, own });
        }
        if target == "main" || target == "refs/heads/main" || short == "main" {
            return match self.mode {
                PublicationMode::BranchOnly => {
                    Err(PolicyViolation::GateOnlyRef(target.to_string()))
                }
                PublicationMode::FastMainStream => Ok(()),
            };
        }
        if GATE_ONLY_REFS.contains(&target) || GATE_ONLY_REFS.contains(&short) {
            return Err(PolicyViolation::GateOnlyRef(target.to_string()));
        }
        if short.starts_with(&self.agent_branch_prefix) {
            return Err(PolicyViolation::RetiredAgentBranch(target.into(), own));
        }
        Err(PolicyViolation::NotAnAgentBranch(target.to_string()))
    }

    /// Check that every commit message in a batch carries a `Task:` trailer
    /// naming one of `agent`'s tasks (`T-<AGENT>-<n>`, case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns the first commit without a trailer, or whose trailer names
    /// another agent.
    pub fn check_commit_trailers<'a, I>(
        &self,
        agent: &str,
        messages: I,
    ) -> Result<(), PolicyViolation>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let prefix = format!("t-{}-", agent.trim().to_ascii_lowercase());
        for message in messages {
            let subject = message.lines().next().unwrap_or("").to_string();
            let trailer = message
                .lines()
                .rev()
                .map(str::trim)
                .find_map(|line| line.strip_prefix("Task:").map(str::trim))
                .filter(|value| !value.is_empty());
            let Some(trailer) = trailer else {
                return Err(PolicyViolation::MissingTaskTrailer { subject });
            };
            if !trailer.to_ascii_lowercase().starts_with(&prefix) {
                return Err(PolicyViolation::ForeignTaskTrailer {
                    subject,
                    trailer: trailer.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Check that every commit in a multi-agent composed batch carries a valid
    /// `Task: T-<ID>` trailer (used when any pluggable driver reconciles and
    /// submits peer contributions to development `main` without overwriting
    /// authorship).
    ///
    /// # Errors
    ///
    /// Returns [`PolicyViolation::MissingTaskTrailer`] if any commit lacks a
    /// well-formed `T-...` task identifier.
    pub fn check_composed_task_trailers<'a, I>(&self, messages: I) -> Result<(), PolicyViolation>
    where
        I: IntoIterator<Item = &'a str>,
    {
        for message in messages {
            let subject = message.lines().next().unwrap_or("").to_string();
            let trailer = message
                .lines()
                .rev()
                .map(str::trim)
                .find_map(|line| line.strip_prefix("Task:").map(str::trim))
                .filter(|value| {
                    value.len() >= 5
                        && (value.starts_with("T-") || value.starts_with("t-"))
                        && value
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                });
            if trailer.is_none() {
                return Err(PolicyViolation::MissingTaskTrailer { subject });
            }
        }
        Ok(())
    }

    /// Whether core may delete the module `proof` describes.
    ///
    /// # Errors
    ///
    /// Destination first: no commit on the commercial origin, no deletion;
    /// an undeclared, non-compiling or untested destination is not a
    /// destination; a port kept in core without a plug is a stub.
    pub fn check_migration(&self, proof: &MigrationProof) -> Result<(), PolicyViolation> {
        let module = proof.module.clone();
        if proof
            .destination_commit
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            return Err(PolicyViolation::DeletionBeforeDestination { module });
        }
        if !proof.destination_declared {
            return Err(PolicyViolation::DestinationUnproven {
                module,
                missing: "the module is not declared in the destination crate's module tree",
            });
        }
        if !proof.destination_compiles {
            return Err(PolicyViolation::DestinationUnproven {
                module,
                missing: "the destination crate does not compile with it",
            });
        }
        if !proof.destination_tests_pass {
            return Err(PolicyViolation::DestinationUnproven {
                module,
                missing: "the destination's tests for it have not passed",
            });
        }
        if let Some((port, _)) = proof.ports_kept.iter().find(|(_, served_by)| {
            served_by
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        }) {
            return Err(PolicyViolation::StubbedCapability { port: port.clone() });
        }
        Ok(())
    }

    /// The lines every worker brief carries, so a worker reads the exact rules
    /// for its publication mode (`BranchOnly` or `FastMainStream`).
    pub fn brief_lines(&self, agent: &str) -> Result<Vec<String>, PolicyViolation> {
        let branch = self.publish_branch(agent)?;
        let agent_ref = self.publish_ref(agent)?;
        let push = format!("git push origin {branch}:{agent_ref}");
        let upper = agent.trim().to_ascii_uppercase();
        match self.mode {
            PublicationMode::BranchOnly => {
                let minutes = self.gate_cycle_secs.div_ceil(60);
                Ok(vec![
                    format!("Work on the local branch `{branch}` and publish only to `{agent_ref}` (`{push}`); never push a remote branch (`main`, `local-agent/*`, `release/*`, `distill/*`, `wip/*`), or `refs/susi/*`, and never open/merge a pull request; the gated host fetches `{agent_ref}` and integrates it."),
                    format!("Every commit ends with `Task: T-{upper}-<n>`; the task record in `.agents/tasks/` lists exactly the claimed scopes."),
                    "A move out of core counts only when the destination commit is on the commercial origin, declared, compiling and tested there; core deletes afterwards, in a change that names that commit. Never leave a port answering an error for a removed capability.".to_string(),
                    "Before every push, on the exact tree: cargo fmt --all --check; cargo check --workspace --all-targets --keep-going --offline --locked; cargo clippy --workspace --all-targets --offline --locked -- -D warnings; the touched crates' tests; the guard tests; GMCP evidence re-recorded for any gmcp change.".to_string(),
                    format!("One push per gated cycle (about {minutes} minutes); read the coordination board and your brief first; a stop note halts pushes at once."),
                    "Never edit a file in another agent's claim; keep files LF-ended; report `landed` or `migrated` only with the commit on the target origin.".to_string(),
                ])
            }
            PublicationMode::FastMainStream => Ok(vec![
                format!("Work in full isolation (own worktree, private target dir, scratch home, and the local branch `{branch}`); publish non-force fast-forwards to unqualified development `main` or to your agent ref `{agent_ref}` (`{push}`, never a remote `local-agent/*` branch) with zero mandatory cooldown and zero dependency on any single coordinator."),
                format!("Every commit ends with `Task: T-{upper}-<n>`; record base commit, scopes, deps, `accept.cmd`, actual results and limitations in `.agents/tasks/` for distillation."),
                "A move out of core counts only when the destination commit is on the commercial origin, declared, compiling and tested there; core deletes afterwards, in a change that names that commit. Never leave a port answering an error for a removed capability.".to_string(),
                "Run focused checks in your private target dir (`fmt`, touched-crate `clippy -D warnings`, touched-crate tests, relevant guards) and report results honestly; full workspace gates belong to distilled release snapshots.".to_string(),
                "Reconcile with the latest `origin/main` head and use atomic normal fast-forward updates; never force-push, never open/merge a pull request, never push `local-agent/*`, `release/*`, `distill/*`, `wip/*`, `refs/susi/*`, or `refs/tags/v*`, and never touch another agent's ref or claim.".to_string(),
                "Keep files LF-ended; report `main` as unqualified development and `landed` only after `git ls-remote` confirms the commit on origin.".to_string(),
            ]),
        }
    }

    /// Refuse a plan whose task keys cannot name branches or collide after
    /// lowercasing (two tasks would publish on one branch).
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` naming the key.
    pub fn validate_plan<T: WorkflowTask>(&self, tasks: &[T]) -> EaiResult<()> {
        let mut branches: BTreeMap<String, &str> = BTreeMap::new();
        for task in tasks {
            let branch = self
                .publish_branch(task.key())
                .map_err(|violation| EaiError::config(violation.to_string()))?;
            if let Some(previous) = branches.insert(branch.clone(), task.key()) {
                return Err(EaiError::config(format!(
                    "tasks {previous} and {} would both publish on {branch}",
                    task.key()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/workflow.rs"]
mod tests;
