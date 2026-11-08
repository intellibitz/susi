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
//!   mechanical: one `local-agent/<agent>` branch per agent, never `main`,
//!   task trailers on every commit, destination-first migrations with no
//!   stubbed ports, and the brief lines every worker receives.

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

// ---------------------------------------------------------------------------
// Integration policy for remote and cloud agents (CEO mandate 2026-11-08).
// ---------------------------------------------------------------------------

/// The refs only the gated host moves: direct pushes here are never an
/// agent's to make.
const GATE_ONLY_REFS: &[&str] = &["main", "refs/heads/main", "HEAD"];

/// Ref prefixes the native coordinator owns (claims, receipts, integration
/// pointers).
const COORDINATOR_REF_PREFIXES: &[&str] = &["refs/susi/"];

/// Where a remote agent's work may be published and what a move out of
/// core must prove before core deletes anything.
///
/// Written after the 2026-11-07 incident: a remote agent pushed about sixty
/// ungated commits straight to `main`, deleting roughly 20,000 lines under
/// the message "migrated to commercial" while the destination origin
/// received none of it. The rules are mechanical here so a wave that would
/// repeat that is refused before a worker starts, and every worker brief
/// carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPolicy {
    /// Prefix of the one branch per agent and repository that an agent may
    /// push (`local-agent/`).
    pub agent_branch_prefix: String,
    /// Shortest gated cycle on the integrating host, in seconds; an agent
    /// batches its iterations into one push per cycle.
    pub gate_cycle_secs: u64,
}

impl Default for IntegrationPolicy {
    fn default() -> Self {
        Self {
            agent_branch_prefix: "local-agent/".to_string(),
            gate_cycle_secs: 20 * 60,
        }
    }
}

/// Why a publish or a migration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyViolation {
    /// The agent name cannot name a branch.
    BadAgentName(String),
    /// The target is `main` or another ref only the gate moves.
    GateOnlyRef(String),
    /// The target is the coordinator's own ref space.
    CoordinatorRef(String),
    /// The target is another agent's branch.
    ForeignBranch {
        /// The branch the agent tried to push.
        target: String,
        /// The branch the agent owns.
        own: String,
    },
    /// The target is not under the agent branch prefix at all.
    NotAnAgentBranch(String),
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
                "{target} is moved only by the gated host; publish on your local-agent branch"
            ),
            Self::CoordinatorRef(target) => {
                write!(f, "{target} belongs to the coordinator; agents never move refs/susi/*")
            }
            Self::ForeignBranch { target, own } => {
                write!(f, "{target} is another agent's branch; yours is {own}")
            }
            Self::NotAnAgentBranch(target) => {
                write!(f, "{target} is not a local-agent branch; agents publish nowhere else")
            }
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
    /// The one branch `agent` publishes on.
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

    /// Whether `agent` may push `target` (a branch name or a full ref).
    ///
    /// # Errors
    ///
    /// Returns the violation: `main` and the coordinator's refs are never an
    /// agent's; another agent's branch is not either; anything outside the
    /// agent branch prefix is refused.
    pub fn check_push_target(&self, agent: &str, target: &str) -> Result<(), PolicyViolation> {
        let own = self.publish_branch(agent)?;
        let target = target.trim();
        let short = target.strip_prefix("refs/heads/").unwrap_or(target);
        if GATE_ONLY_REFS.contains(&target) || GATE_ONLY_REFS.contains(&short) {
            return Err(PolicyViolation::GateOnlyRef(target.to_string()));
        }
        if COORDINATOR_REF_PREFIXES
            .iter()
            .any(|prefix| target.starts_with(prefix))
        {
            return Err(PolicyViolation::CoordinatorRef(target.to_string()));
        }
        if !short.starts_with(&self.agent_branch_prefix) {
            return Err(PolicyViolation::NotAnAgentBranch(target.to_string()));
        }
        if short.to_ascii_lowercase() != own {
            return Err(PolicyViolation::ForeignBranch {
                target: target.to_string(),
                own,
            });
        }
        Ok(())
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

    /// The lines every worker brief carries, so a remote worker reads the
    /// rules with its task instead of discovering them from a refused push.
    pub fn brief_lines(&self, agent: &str) -> Result<Vec<String>, PolicyViolation> {
        let branch = self.publish_branch(agent)?;
        let minutes = self.gate_cycle_secs.div_ceil(60);
        Ok(vec![
            format!("Publish only on `{branch}`; never push `main`, never move `refs/susi/*`; the gated host integrates your branch."),
            format!("Every commit ends with `Task: T-{}-<n>`; the task record in `.agents/tasks/` lists exactly the claimed scopes.", agent.trim().to_ascii_uppercase()),
            "A move out of core counts only when the destination commit is on the commercial origin, declared, compiling and tested there; core deletes afterwards, in a change that names that commit. Never leave a port answering an error for a removed capability.".to_string(),
            "Before every push, on the exact tree: cargo fmt --all --check; cargo check --workspace --all-targets --keep-going --offline --locked; cargo clippy --workspace --all-targets --offline --locked -- -D warnings; the touched crates' tests; the guard tests; GMCP evidence re-recorded for any gmcp change.".to_string(),
            format!("One push per gated cycle (about {minutes} minutes); read the coordination board and your brief first; a stop note halts pushes at once."),
            "Never edit a file in another agent's claim; keep files LF-ended; report `landed` or `migrated` only with the commit on the target origin.".to_string(),
        ])
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
mod tests {
    use super::*;

    struct Task {
        key: String,
        deps: Vec<String>,
        scopes: Vec<String>,
    }

    fn task(key: &str, deps: &[&str], scopes: &[&str]) -> Task {
        Task {
            key: key.into(),
            deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        }
    }

    impl WorkflowTask for Task {
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

    fn keys<T: WorkflowTask>(tasks: &[T]) -> Vec<&str> {
        tasks.iter().map(WorkflowTask::key).collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn normalize_scope_canonicalises_separators_case_and_dots() {
        assert_eq!(
            normalize_scope(" .\\Crates\\Susi-Core\\ ").unwrap(),
            "crates/susi-core"
        );
        assert_eq!(normalize_scope("./docs//a.md/").unwrap(), "docs/a.md");
        assert_eq!(normalize_scope("././x").unwrap(), "x");
    }

    #[test]
    fn normalize_scope_rejects_empty_dot_parent_and_absolute() {
        for raw in [
            "", "  ", ".", "./", "..", "../x", "a/../b", "a/./b", "/etc", "\\x", "C:/x",
        ] {
            assert!(normalize_scope(raw).is_err(), "{raw:?} should be rejected");
        }
    }

    #[test]
    fn scopes_overlap_on_equality_and_component_prefixes_only() {
        assert!(scopes_overlap("crates/susi-core", "crates/susi-core"));
        assert!(scopes_overlap(
            "crates/susi-core",
            "crates/susi-core/src/lib.rs"
        ));
        assert!(scopes_overlap(
            "crates/susi-core/src/lib.rs",
            "crates/susi-core"
        ));
        assert!(!scopes_overlap("crates/susi-core", "crates/susi-core-defs"));
        assert!(!scopes_overlap("crates/susi-core-defs", "crates/susi-core"));
        assert!(!scopes_overlap("docs/a.md", "docs/b.md"));
    }

    #[test]
    fn task_scopes_disjoint_normalizes_and_fails_closed() {
        assert!(!task_scopes_disjoint(
            &strings(&["Crates\\Susi-Core"]),
            &strings(&["crates/susi-core/src/lib.rs"])
        ));
        assert!(task_scopes_disjoint(
            &strings(&["crates/susi-core", "docs/a.md"]),
            &strings(&["crates/susi-core-defs", "docs/b.md"])
        ));
        assert!(!task_scopes_disjoint(
            &strings(&["../escape"]),
            &strings(&["docs/b.md"])
        ));
    }

    #[test]
    fn validate_dependencies_rejects_unknown_and_cycles() {
        let fine = [task("a", &[], &["x"]), task("b", &["a"], &["y"])];
        assert!(validate_dependencies(&fine).is_ok());

        let unknown = [task("a", &["ghost"], &["x"])];
        let error = validate_dependencies(&unknown).unwrap_err().to_string();
        assert!(error.contains("ghost"), "{error}");

        let cycle = [
            task("a", &["c"], &["x"]),
            task("b", &["a"], &["y"]),
            task("c", &["b"], &["z"]),
        ];
        let error = validate_dependencies(&cycle).unwrap_err().to_string();
        assert!(error.contains("dependency cycle"), "{error}");
        assert!(error.contains("a -> c -> b -> a"), "{error}");

        let selfish = [task("a", &["a"], &["x"])];
        let error = validate_dependencies(&selfish).unwrap_err().to_string();
        assert!(error.contains("a -> a"), "{error}");
    }

    #[test]
    fn overlapping_scopes_are_deferred_to_later_waves_in_plan_order() {
        let tasks = [
            task("a", &[], &["crates/susi-core"]),
            task("b", &[], &["crates/susi-core/src/lib.rs"]),
            task("c", &[], &["crates/susi-core-defs"]),
            task("d", &[], &["docs/d.md"]),
        ];
        let schedule = WorkflowSchedule::build(&tasks, &BTreeSet::new(), &[], 4).unwrap();
        assert_eq!(schedule.waves.len(), 2);
        assert_eq!(schedule.waves[0].index, 0);
        assert_eq!(keys(&schedule.waves[0].tasks), vec!["a", "c", "d"]);
        assert_eq!(schedule.waves[1].index, 1);
        assert_eq!(keys(&schedule.waves[1].tasks), vec!["b"]);
        assert!(schedule.blocked.is_empty());
    }

    #[test]
    fn waves_respect_max_parallel_and_dependencies() {
        let tasks = [
            task("a", &[], &["a"]),
            task("b", &[], &["b"]),
            task("c", &["a"], &["c"]),
            task("d", &["c", "b"], &["d"]),
        ];
        let schedule = WorkflowSchedule::build(&tasks, &BTreeSet::new(), &[], 2).unwrap();
        let waves: Vec<Vec<&str>> = schedule
            .waves
            .iter()
            .map(|wave| keys(&wave.tasks))
            .collect();
        assert_eq!(waves, vec![vec!["a", "b"], vec!["c"], vec!["d"]]);

        let landed: BTreeSet<String> = ["a".to_string()].into();
        let resumed = WorkflowSchedule::build(&tasks, &landed, &[], 2).unwrap();
        assert_eq!(keys(&resumed.waves[0].tasks), vec!["b", "c"]);
    }

    #[test]
    fn active_claims_block_overlapping_tasks_and_their_dependents() {
        let tasks = [
            task("a", &[], &["crates/susi-core/src"]),
            task("b", &[], &["docs/b.md"]),
            task("c", &["a"], &["docs/c.md"]),
        ];
        let claimed = strings(&["crates/susi-core"]);
        let schedule = WorkflowSchedule::build(&tasks, &BTreeSet::new(), &claimed, 4).unwrap();
        assert_eq!(schedule.waves.len(), 1);
        assert_eq!(keys(&schedule.waves[0].tasks), vec!["b"]);
        assert_eq!(keys(&schedule.blocked), vec!["a", "c"]);
    }

    #[test]
    fn next_wave_reports_an_invalid_scope() {
        let tasks = [task("a", &[], &["../escape"])];
        let pending: Vec<&Task> = tasks.iter().collect();
        assert!(next_wave(&pending, &BTreeSet::new(), &[], 2).is_err());
    }

    fn write_claim(claims: &Path, name: &str, worktree: &Path, scopes: &[&str]) {
        std::fs::write(
            claims.join(format!("{name}.claim")),
            format!(
                "{CLAIM_HEADER}\n{name}\n{}\n{}\n{}\n",
                worktree.display(),
                "b".repeat(40),
                scopes.join("\n")
            ),
        )
        .unwrap();
    }

    #[test]
    fn inspect_repo_workflow_reads_claims_slots_and_head_without_locking() {
        let repo = tempfile::tempdir().unwrap();
        let state = repo.path().join(".git/susi-local-workflow");
        let claims = state.join("claims");
        let slots = state.join("slots");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::create_dir_all(slots.join("1")).unwrap();
        std::fs::create_dir_all(slots.join("2")).unwrap();
        write_claim(
            &claims,
            "zeta",
            &repo.path().join("wt-zeta"),
            &["crates/susi-harness", ".agents/tasks/t-z.json"],
        );
        write_claim(&claims, "alpha", &repo.path().join("wt-alpha"), &["docs"]);
        std::fs::write(claims.join("notes.txt"), "ignored").unwrap();
        // The OS lock file is held open while we inspect: a reader that
        // tried to lock it would block or fail; the inspector must not care.
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(slots.join("1.lock"))
            .unwrap();
        held.lock().unwrap();
        std::fs::write(slots.join("1/owner"), "pid=4242\n").unwrap();
        std::fs::write(
            slots.join("2/owner"),
            "pid=7\nagent=antigravity\nscope=crates/susi-harness\n",
        )
        .unwrap();
        std::fs::create_dir_all(slots.join("3")).unwrap();
        let head = "c".repeat(40);
        std::fs::create_dir_all(repo.path().join(".git/refs/susi/integration")).unwrap();
        std::fs::write(
            repo.path().join(".git/refs/susi/integration/current"),
            format!("{head}\n"),
        )
        .unwrap();

        let snapshot = inspect_repo_workflow(repo.path()).unwrap();
        held.unlock().unwrap();

        assert_eq!(
            snapshot
                .claims
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        let zeta = &snapshot.claims[1];
        assert_eq!(zeta.worktree, repo.path().join("wt-zeta"));
        assert_eq!(zeta.base, "b".repeat(40));
        assert_eq!(
            zeta.scopes,
            strings(&["crates/susi-harness", ".agents/tasks/t-z.json"])
        );
        assert_eq!(
            snapshot.active_slots,
            vec![
                SlotRecord {
                    agent: "1".into(),
                    pid: Some(4242),
                    scope_key: None,
                },
                SlotRecord {
                    agent: "antigravity".into(),
                    pid: Some(7),
                    scope_key: Some("crates/susi-harness".into()),
                },
            ]
        );
        assert_eq!(snapshot.integration_head.as_deref(), Some(head.as_str()));
        assert_eq!(
            snapshot.claimed_scopes(),
            strings(&["docs", "crates/susi-harness", ".agents/tasks/t-z.json"])
        );
    }

    #[test]
    fn inspect_repo_workflow_handles_empty_state_and_packed_refs() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        assert_eq!(
            inspect_repo_workflow(repo.path()).unwrap(),
            WorkflowSnapshot::default()
        );
        let head = "d".repeat(40);
        std::fs::write(
            repo.path().join(".git/packed-refs"),
            format!("# pack-refs with: peeled\n{head} {INTEGRATION_REF}\n"),
        )
        .unwrap();
        assert_eq!(
            inspect_repo_workflow(repo.path())
                .unwrap()
                .integration_head
                .as_deref(),
            Some(head.as_str())
        );
    }

    #[test]
    fn inspect_repo_workflow_rejects_a_malformed_claim() {
        let repo = tempfile::tempdir().unwrap();
        let claims = repo.path().join(".git/susi-local-workflow/claims");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::write(claims.join("bad.claim"), "susi-local-claim-v1\nbad\n").unwrap();
        assert!(inspect_repo_workflow(repo.path()).is_err());
        assert!(inspect_repo_workflow(&repo.path().join("missing")).is_err());
    }

    // --- integration policy ---------------------------------------------

    #[test]
    fn an_agent_publishes_on_its_own_branch_and_nowhere_else() {
        let policy = IntegrationPolicy::default();
        assert_eq!(
            policy.publish_branch("Antigravity").ok(),
            Some("local-agent/antigravity".to_string())
        );
        assert!(policy
            .check_push_target("antigravity", "local-agent/antigravity")
            .is_ok());
        assert!(policy
            .check_push_target("antigravity", "refs/heads/local-agent/antigravity")
            .is_ok());
        assert_eq!(
            policy.check_push_target("antigravity", "main"),
            Err(PolicyViolation::GateOnlyRef("main".into()))
        );
        assert_eq!(
            policy.check_push_target("antigravity", "refs/heads/main"),
            Err(PolicyViolation::GateOnlyRef("refs/heads/main".into()))
        );
        assert_eq!(
            policy.check_push_target("antigravity", "refs/susi/integration/current"),
            Err(PolicyViolation::CoordinatorRef(
                "refs/susi/integration/current".into()
            ))
        );
        assert_eq!(
            policy.check_push_target("antigravity", "local-agent/gemini"),
            Err(PolicyViolation::ForeignBranch {
                target: "local-agent/gemini".into(),
                own: "local-agent/antigravity".into(),
            })
        );
        assert_eq!(
            policy.check_push_target("antigravity", "feature/x"),
            Err(PolicyViolation::NotAnAgentBranch("feature/x".into()))
        );
        assert!(matches!(
            policy.publish_branch("bad name/"),
            Err(PolicyViolation::BadAgentName(_))
        ));
    }

    #[test]
    fn every_commit_in_a_batch_names_the_agents_task() {
        let policy = IntegrationPolicy::default();
        let good = [
            "refactor: x\n\nTask: T-GEMINI-1\n",
            "fix: y\n\nTask: t-gemini-2",
        ];
        assert!(policy.check_commit_trailers("gemini", good).is_ok());
        assert_eq!(
            policy.check_commit_trailers("gemini", ["[iter 320/400] delete crate"]),
            Err(PolicyViolation::MissingTaskTrailer {
                subject: "[iter 320/400] delete crate".into()
            })
        );
        assert_eq!(
            policy.check_commit_trailers("gemini", ["chore\n\nTask: T-LEAN-1"]),
            Err(PolicyViolation::ForeignTaskTrailer {
                subject: "chore".into(),
                trailer: "T-LEAN-1".into()
            })
        );
    }

    #[test]
    fn core_deletes_only_after_the_destination_is_proven() {
        let policy = IntegrationPolicy::default();
        let module = "crates/susi-gemi/src/engines/alpha.rs";
        let mut proof = MigrationProof {
            module: module.into(),
            ..MigrationProof::default()
        };
        assert_eq!(
            policy.check_migration(&proof),
            Err(PolicyViolation::DeletionBeforeDestination {
                module: module.into()
            })
        );
        proof.destination_commit = Some("c128ef95".into());
        assert!(matches!(
            policy.check_migration(&proof),
            Err(PolicyViolation::DestinationUnproven { missing, .. })
                if missing.contains("not declared")
        ));
        proof.destination_declared = true;
        proof.destination_compiles = true;
        assert!(matches!(
            policy.check_migration(&proof),
            Err(PolicyViolation::DestinationUnproven { missing, .. })
                if missing.contains("tests")
        ));
        proof.destination_tests_pass = true;
        assert!(policy.check_migration(&proof).is_ok());
        proof.ports_kept = vec![("gemi /v1/chat/completions local engine".into(), None)];
        assert!(matches!(
            policy.check_migration(&proof),
            Err(PolicyViolation::StubbedCapability { .. })
        ));
        proof.ports_kept = vec![(
            "gemi /v1/chat/completions local engine".into(),
            Some("susi-gpu-fabric plug over GEMI 9091".into()),
        )];
        assert!(policy.check_migration(&proof).is_ok());
    }

    #[test]
    fn worker_briefs_carry_the_rules_and_plans_cannot_share_a_branch() {
        let policy = IntegrationPolicy::default();
        let lines = policy.brief_lines("Gemini").unwrap_or_default();
        assert_eq!(lines.len(), 6);
        assert!(lines[0].contains("`local-agent/gemini`"));
        assert!(lines[1].contains("Task: T-GEMINI-<n>"));
        assert!(lines[4].contains("20 minutes"));
        let ok = [task("alpha", &[], &["a"]), task("beta", &[], &["b"])];
        assert!(policy.validate_plan(&ok).is_ok());
        let clash = [task("Alpha", &[], &["a"]), task("alpha", &[], &["b"])];
        assert!(policy.validate_plan(&clash).is_err());
        let bad = [task("no spaces", &[], &["a"])];
        assert!(policy.validate_plan(&bad).is_err());
    }
}
