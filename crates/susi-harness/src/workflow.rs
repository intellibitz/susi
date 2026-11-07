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

use crate::supervisor::PlanTask;
use serde::{Deserialize, Serialize};
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

    /// Verify that the schedule's waves are non-empty, pairwise scope-disjoint within each wave,
    /// topologically ordered across waves, and account for all `input_tasks` without duplicates.
    ///
    /// # Errors
    ///
    /// Returns a human-readable violation string if any invariant is broken.
    pub fn verify_wave_plan_invariants(&self, input_tasks: &[T]) -> Result<(), String> {
        let mut input_keys = BTreeSet::new();
        for task in input_tasks {
            let key = task.key().trim();
            if key.is_empty() {
                return Err("input task key must not be empty".to_string());
            }
            if !input_keys.insert(task.key().to_string()) {
                return Err(format!("duplicate input task key `{}`", task.key()));
            }
        }
        let mut seen = BTreeSet::new();
        let mut completed_in_prior_waves = BTreeSet::new();
        for (expected_idx, wave) in self.waves.iter().enumerate() {
            if wave.index != expected_idx {
                return Err(format!(
                    "wave index mismatch: expected {expected_idx}, got {}",
                    wave.index
                ));
            }
            if wave.tasks.is_empty() {
                return Err(format!("wave {expected_idx} is empty"));
            }
            let mut wave_scopes: Vec<String> = Vec::new();
            let mut current_wave_keys = BTreeSet::new();
            for task in &wave.tasks {
                let key = task.key();
                if !input_keys.contains(key) {
                    return Err(format!(
                        "task `{key}` in wave {expected_idx} is not in input graph"
                    ));
                }
                if !seen.insert(key.to_string()) {
                    return Err(format!("duplicate task `{key}` across waves"));
                }
                for dep in task.deps() {
                    if !completed_in_prior_waves.contains(dep) {
                        return Err(format!(
                            "task `{key}` in wave {expected_idx} depends on `{dep}` which is not in an earlier wave"
                        ));
                    }
                }
                if task.scopes().is_empty() {
                    return Err(format!("task `{key}` reserves no scopes"));
                }
                let normalized = normalize_all(task.scopes()).map_err(|e| e.to_string())?;
                if !disjoint(&normalized, &wave_scopes) {
                    return Err(format!(
                        "task `{key}` in wave {expected_idx} has overlapping scopes within the same wave"
                    ));
                }
                wave_scopes.extend(normalized);
                current_wave_keys.insert(key.to_string());
            }
            completed_in_prior_waves.extend(current_wave_keys);
        }
        for task in &self.blocked {
            let key = task.key();
            if !input_keys.contains(key) {
                return Err(format!("blocked task `{key}` is not in input graph"));
            }
            if !seen.insert(key.to_string()) {
                return Err(format!("duplicate task `{key}` in blocked list"));
            }
        }
        if seen.len() != input_keys.len() {
            return Err(format!(
                "scheduled task count ({}) does not equal input graph size ({})",
                seen.len(),
                input_keys.len()
            ));
        }
        Ok(())
    }
}

/// One node in an embedded workflow wave plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNode {
    /// Unique task key within the wave plan.
    pub key: String,
    /// Task keys that must complete before this node starts.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Repository-relative file or directory scopes reserved by this node.
    pub scopes: Vec<String>,
}

impl WorkflowTask for WorkflowNode {
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

/// Wave schedule produced by [`EmbeddedWorkflowOrchestrator`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowWavePlan {
    /// Identifier of this wave plan.
    pub wave_id: String,
    /// Ordered waves; each wave lists task keys that can run concurrently without scope or dependency conflicts.
    pub waves: Vec<Vec<String>>,
    /// Task keys blocked by external active claims.
    pub blocked: Vec<String>,
    /// Input graph nodes that produced this wave plan, enabling self-contained invariant verification.
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
}

impl WorkflowWavePlan {
    /// Verify that:
    /// 1. `wave_id` is non-empty and `waves` is non-empty when `nodes` is non-empty (or all nodes are accounted for).
    /// 2. Every wave in `self.waves` is non-empty, and tasks within the same wave have disjoint `scopes`
    ///    (no path prefix overlap between any two tasks in the same wave).
    /// 3. Every dependency of a task in wave `k` appears in an earlier wave `< k` (topological wave ordering).
    /// 4. Total tasks across waves (plus `blocked`) equals the input graph size with no duplicate task IDs.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string when any wave plan invariant is violated.
    pub fn verify_wave_plan_invariants(&self) -> Result<(), String> {
        if self.wave_id.trim().is_empty() {
            return Err("wave_id must not be empty".to_string());
        }
        let mut node_map: BTreeMap<&str, &WorkflowNode> = BTreeMap::new();
        for node in &self.nodes {
            if node.key.trim().is_empty() {
                return Err("workflow node key must not be empty".to_string());
            }
            if node.scopes.is_empty() {
                return Err(format!("task `{}` reserves no scopes", node.key));
            }
            if node_map.insert(node.key.as_str(), node).is_some() {
                return Err(format!("duplicate task key `{}` in input nodes", node.key));
            }
        }

        let mut seen = BTreeSet::new();
        let mut completed_in_prior_waves = BTreeSet::new();

        for (wave_idx, wave) in self.waves.iter().enumerate() {
            if wave.is_empty() {
                return Err(format!("wave {wave_idx} must not be empty"));
            }
            let mut wave_scopes: Vec<String> = Vec::new();
            let mut current_wave_keys = BTreeSet::new();
            for key in wave {
                if key.trim().is_empty() {
                    return Err(format!("empty task key in wave {wave_idx}"));
                }
                if !seen.insert(key.clone()) {
                    return Err(format!("duplicate task key `{key}` across waves"));
                }
                if !node_map.is_empty() {
                    let Some(node) = node_map.get(key.as_str()) else {
                        return Err(format!(
                            "task `{key}` in wave {wave_idx} is not present in input nodes"
                        ));
                    };
                    for dep in &node.deps {
                        if !completed_in_prior_waves.contains(dep) {
                            return Err(format!(
                                "task `{key}` in wave {wave_idx} depends on `{dep}` which did not complete in an earlier wave (< {wave_idx})"
                            ));
                        }
                    }
                    let normalized = normalize_all(&node.scopes).map_err(|e| e.to_string())?;
                    if !disjoint(&normalized, &wave_scopes) {
                        return Err(format!(
                            "task `{key}` in wave {wave_idx} has overlapping scopes with another task in wave {wave_idx}"
                        ));
                    }
                    wave_scopes.extend(normalized);
                }
                current_wave_keys.insert(key.clone());
            }
            completed_in_prior_waves.extend(current_wave_keys);
        }

        for key in &self.blocked {
            if key.trim().is_empty() {
                return Err("empty task key in blocked list".to_string());
            }
            if !seen.insert(key.clone()) {
                return Err(format!("duplicate task key `{key}` in blocked list"));
            }
            if !node_map.is_empty() && !node_map.contains_key(key.as_str()) {
                return Err(format!(
                    "blocked task `{key}` is not present in input nodes"
                ));
            }
        }

        if !node_map.is_empty() && seen.len() != node_map.len() {
            return Err(format!(
                "total tasks across waves and blocked ({}) does not match input graph size ({})",
                seen.len(),
                node_map.len()
            ));
        }

        Ok(())
    }
}

/// Error returned when planning an embedded workflow wave fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEmbedError {
    /// Reason the workflow plan was rejected.
    pub message: String,
}

impl std::fmt::Display for WorkflowEmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowEmbedError {}

/// In-process orchestrator for dependency- and scope-disjoint wave planning.
#[derive(Debug, Clone)]
pub struct EmbeddedWorkflowOrchestrator {
    /// Maximum concurrent tasks per wave.
    pub max_parallel: usize,
    /// Already-completed task keys.
    pub completed: BTreeSet<String>,
    /// Scopes currently claimed by external agents.
    pub active_claimed_scopes: Vec<String>,
}

impl Default for EmbeddedWorkflowOrchestrator {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            completed: BTreeSet::new(),
            active_claimed_scopes: Vec::new(),
        }
    }
}

impl EmbeddedWorkflowOrchestrator {
    /// Create an orchestrator with `max_parallel` concurrency.
    #[must_use]
    pub fn new(max_parallel: usize) -> Self {
        Self {
            max_parallel: max_parallel.max(1),
            ..Self::default()
        }
    }

    /// Partition `nodes` into dependency-ready, pairwise scope-disjoint waves.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowEmbedError`] if any scope is invalid or if dependencies contain an unknown key or cycle.
    pub fn plan_wave(
        &self,
        wave_id: &str,
        nodes: &[WorkflowNode],
    ) -> Result<WorkflowWavePlan, WorkflowEmbedError> {
        if wave_id.trim().is_empty() {
            return Err(WorkflowEmbedError {
                message: "wave_id must not be empty".to_string(),
            });
        }
        for node in nodes {
            if node.scopes.is_empty() {
                return Err(WorkflowEmbedError {
                    message: format!("task {} reserves no scopes", node.key),
                });
            }
            for scope in &node.scopes {
                normalize_scope(scope).map_err(|error| WorkflowEmbedError {
                    message: format!("task {} scope: {error}", node.key),
                })?;
            }
        }
        let schedule = WorkflowSchedule::build(
            nodes,
            &self.completed,
            &self.active_claimed_scopes,
            self.max_parallel.max(1),
        )
        .map_err(|error| WorkflowEmbedError {
            message: error.to_string(),
        })?;
        let retained_nodes: Vec<WorkflowNode> = nodes
            .iter()
            .filter(|n| !self.completed.contains(&n.key))
            .cloned()
            .collect();
        let plan = WorkflowWavePlan {
            wave_id: wave_id.to_string(),
            waves: schedule
                .waves
                .into_iter()
                .map(|w| w.tasks.into_iter().map(|t| t.key.clone()).collect())
                .collect(),
            blocked: schedule
                .blocked
                .into_iter()
                .map(|t| t.key.clone())
                .collect(),
            nodes: retained_nodes,
        };
        plan.verify_wave_plan_invariants()
            .map_err(|message| WorkflowEmbedError { message })?;
        Ok(plan)
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

    #[test]
    fn verify_wave_plan_invariants_validates_disjoint_scopes_topological_order_and_totality() {
        let nodes = vec![
            WorkflowNode {
                key: "t-core".to_string(),
                deps: vec![],
                scopes: vec!["crates/susi-core".to_string()],
            },
            WorkflowNode {
                key: "t-harness".to_string(),
                deps: vec![],
                scopes: vec!["crates/susi-harness".to_string()],
            },
            WorkflowNode {
                key: "t-core-leaf".to_string(),
                deps: vec!["t-core".to_string()],
                scopes: vec!["crates/susi-core/src/lib.rs".to_string()],
            },
        ];
        let orchestrator = EmbeddedWorkflowOrchestrator::new(4);
        let plan = orchestrator.plan_wave("wave-4", &nodes).unwrap();
        assert!(plan.verify_wave_plan_invariants().is_ok());

        let schedule = WorkflowSchedule::build(&nodes, &BTreeSet::new(), &[], 4).unwrap();
        assert!(schedule.verify_wave_plan_invariants(&nodes).is_ok());

        // Empty wave is rejected.
        let mut empty_wave = plan.clone();
        empty_wave.waves.push(vec![]);
        assert!(empty_wave
            .verify_wave_plan_invariants()
            .unwrap_err()
            .contains("must not be empty"));

        // Overlapping scopes in the same wave are rejected.
        let mut overlapping = plan.clone();
        overlapping.nodes[2].deps.clear();
        overlapping.waves = vec![vec![
            "t-core".to_string(),
            "t-harness".to_string(),
            "t-core-leaf".to_string(),
        ]];
        assert!(overlapping
            .verify_wave_plan_invariants()
            .unwrap_err()
            .contains("overlapping scopes"));

        // Dependency placed in the same wave or a later wave is rejected.
        let mut bad_dep_order = plan.clone();
        bad_dep_order.nodes[2].scopes = vec!["docs/leaf.md".to_string()];
        bad_dep_order.waves = vec![vec![
            "t-core".to_string(),
            "t-harness".to_string(),
            "t-core-leaf".to_string(),
        ]];
        assert!(bad_dep_order
            .verify_wave_plan_invariants()
            .unwrap_err()
            .contains("did not complete in an earlier wave"));

        // Duplicate task ID across waves is rejected.
        let mut dup = plan.clone();
        dup.waves[1].push("t-core".to_string());
        assert!(dup
            .verify_wave_plan_invariants()
            .unwrap_err()
            .contains("duplicate task key"));

        // Missing task (total tasks across waves != input graph size) is rejected.
        let mut missing = plan;
        missing.waves.pop();
        assert!(missing
            .verify_wave_plan_invariants()
            .unwrap_err()
            .contains("does not match input graph size"));
    }
}
