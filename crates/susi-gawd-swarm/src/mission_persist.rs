//! Persist executable mission DAG state (VC-201-021).
//!
//! Task inputs, dependencies, outputs, and terminal states survive restart;
//! unfinished nodes stay unfinished so resume only dispatches ready work.

use crate::susi_error::{EaiError, EaiResult};
use crate::task_lease::{LeaseRecovery, OwnershipEpoch};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeTerminal {
    Pending,
    Running,
    Completed,
    Failed,
}

impl NodeTerminal {
    /// The serialized name — the same snake_case the JSON record carries.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedNode {
    pub id: String,
    pub input: String,
    pub dependencies: Vec<String>,
    pub output: Option<String>,
    pub state: NodeTerminal,
}

/// One restart's record of what it folded back: the nodes that were
/// `Running` when the process died, reset to `Pending` for re-dispatch.
/// The interruption is named, not silent (T-DEEPSEEK-93, VC-202-011) —
/// the durable record keeps *which* work was in flight when the mission
/// was interrupted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interruption {
    pub unix: u64,
    pub nodes: Vec<String>,
}

/// What a restart may do with a loaded mission — decided from the record
/// alone, before any node re-runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeVerdict {
    /// Every node is terminal-`Completed` — nothing left to drive.
    Complete,
    /// At least one node carries a terminal `Failed` verdict and its
    /// recorded output — the mission fails cleanly, naming it, rather
    /// than wedging on a dispatch refusal. `Failed` is never retried
    /// silently: a recorded verdict is kept, not rewritten.
    TerminalFailed(Vec<(String, String)>),
    /// Unfinished work with no recorded failure — resume continues.
    Resumable,
}

/// One rollback's record (T-DEEPSEEK-94): the snapshot that was restored
/// and the node states it replaced — the restore is visible in the
/// mission's own record, not a silent rewrite of history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restore {
    pub unix: u64,
    /// Snapshot file name that was restored.
    pub snapshot: String,
    /// `id:state` pairs the restored record replaced.
    pub replaced: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedMission {
    pub mission_id: String,
    pub nodes: BTreeMap<String, PersistedNode>,
    /// Monotonic mission and coordinator authority.  A restart advances this
    /// before any worker is redispatched, so an old token cannot be reused.
    #[serde(default)]
    pub ownership: OwnershipEpoch,
    /// Stable coordinator identity for audit and recovery diagnostics.
    #[serde(default)]
    pub coordinator: String,
    /// Durable lease/fence state (T-DEVIN-9): survives restart so stale
    /// workers can't collide with freshly issued fences.
    #[serde(default)]
    pub leases: crate::task_lease::LeaseTable,
    /// Recovery state is kept beside leases and written in the same mission
    /// transaction.  The field is explicit in the mission schema so callers
    /// do not accidentally persist nodes without authority state.
    #[serde(default)]
    pub recovery: LeaseRecovery,
    /// Durable side-effect intent journal (T-DEVIN-10): intents recorded
    /// before dispatch survive the crash so replay reconciles them.
    #[serde(default)]
    pub side_effect_journal: crate::side_effect_journal::IntentJournal,
    /// Every restart's record of the nodes it found mid-flight and folded
    /// back for re-dispatch (T-DEEPSEEK-93) — the resume point is durable.
    #[serde(default)]
    pub interruptions: Vec<Interruption>,
    /// Every rollback's record — which snapshot was restored and the node
    /// states it replaced (T-DEEPSEEK-94).
    #[serde(default)]
    pub restores: Vec<Restore>,
}

impl PersistedMission {
    #[must_use]
    pub fn new(mission_id: impl Into<String>) -> Self {
        Self {
            mission_id: mission_id.into(),
            nodes: BTreeMap::new(),
            ownership: OwnershipEpoch::default(),
            coordinator: String::new(),
            leases: crate::task_lease::LeaseTable::new(),
            recovery: LeaseRecovery::default(),
            side_effect_journal: crate::side_effect_journal::IntentJournal::new(),
            interruptions: Vec::new(),
            restores: Vec::new(),
        }
    }

    pub fn upsert_node(&mut self, node: PersistedNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Advance authority for a coordinator restart and invalidate every
    /// previous worker lease.  The returned scopes are the scopes that must
    /// be cancelled or quarantined before new workers are dispatched.
    pub fn begin_recovery(&mut self, coordinator: &str, now: u64) -> Vec<String> {
        let stale_scopes: Vec<String> =
            self.leases.recovery.active_scopes.iter().cloned().collect();
        self.ownership = self.ownership.next();
        self.coordinator = coordinator.to_string();
        self.leases.begin_recovery(self.ownership, now);
        self.leases.recovery.active_scopes.clear();
        self.recovery = self.leases.recovery.clone();
        self.side_effect_journal.reconcile_after_crash();
        stale_scopes
    }

    /// Atomically publish the recovery transition before the caller cancels
    /// old scopes or redispatches any node.
    pub fn begin_recovery_atomic(
        &mut self,
        dir: &Path,
        coordinator: &str,
        now: u64,
    ) -> EaiResult<Vec<String>> {
        let mut candidate = self.clone();
        let stale_scopes = candidate.begin_recovery(coordinator, now);
        candidate.save(dir)?;
        *self = candidate;
        Ok(stale_scopes)
    }

    /// Record that a worker scope is live before it can issue a side effect.
    pub fn set_active_scope(&mut self, scope: &str) {
        self.leases.recovery.active_scopes.insert(scope.to_string());
        self.recovery = self.leases.recovery.clone();
    }

    /// Remove a scope after its batch has been reconciled and folded.
    pub fn clear_active_scope(&mut self, scope: &str) {
        self.leases.recovery.active_scopes.remove(scope);
        self.recovery = self.leases.recovery.clone();
    }

    /// Apply a mission mutation and publish the complete candidate in one
    /// atomic file replacement.  If the callback or write fails, the caller's
    /// in-memory state is unchanged and no partial authority state is
    /// visible to a recovering coordinator.
    pub fn transaction<F>(&mut self, dir: &Path, update: F) -> EaiResult<()>
    where
        F: FnOnce(&mut Self) -> EaiResult<()>,
    {
        let mut candidate = self.clone();
        update(&mut candidate)?;
        candidate.save(dir)?;
        *self = candidate;
        Ok(())
    }

    /// Current persisted authority token for a live lease.
    #[must_use]
    pub fn authority_for(&self, task_id: &str) -> Option<crate::task_lease::AuthorityToken> {
        self.leases.authority_for(task_id)
    }

    /// Renew a lease and publish the renewal timestamp with the same atomic
    /// mission replacement as the lease deadline.
    #[allow(clippy::too_many_arguments)] // persistence path plus lease identity, authority, clock and TTL are the complete boundary
    pub fn renew_lease(
        &mut self,
        dir: &Path,
        task_id: &str,
        owner: &str,
        token: crate::task_lease::AuthorityToken,
        now: u64,
        ttl: u64,
    ) -> EaiResult<crate::task_lease::CompleteVerdict> {
        let mut candidate = self.clone();
        let verdict = candidate.leases.renew(task_id, owner, token, now, ttl);
        if verdict != crate::task_lease::CompleteVerdict::Accepted {
            return Ok(verdict);
        }
        candidate.recovery = candidate.leases.recovery.clone();
        candidate.save(dir)?;
        *self = candidate;
        Ok(verdict)
    }

    /// Nodes whose dependencies are all `Completed` and that are still `Pending`.
    #[must_use]
    pub fn runnable(&self) -> Vec<&PersistedNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.state == NodeTerminal::Pending
                    && n.dependencies.iter().all(|d| {
                        self.nodes
                            .get(d)
                            .is_some_and(|dep| dep.state == NodeTerminal::Completed)
                    })
            })
            .collect()
    }

    /// Fold crash-interrupted `Running` nodes back to `Pending` and record
    /// the fold (T-DEEPSEEK-93). A worker that died with the process left
    /// its node `Running`; `dispatch` refuses non-Pending states, so
    /// without this fold the node would wedge the mission forever. The
    /// interrupted node ids are recorded on the mission so the resume
    /// point is explicit and durable.
    ///
    /// Called inside the recovery transaction on every persisted resume,
    /// after ownership advances — the fold and the authority advance land
    /// in the same atomic write.
    pub fn reconcile_interrupted(&mut self, now: u64) -> Vec<String> {
        let interrupted: Vec<String> = self
            .nodes
            .values()
            .filter(|n| n.state == NodeTerminal::Running)
            .map(|n| n.id.clone())
            .collect();
        if interrupted.is_empty() {
            return Vec::new();
        }
        for n in self.nodes.values_mut() {
            if n.state == NodeTerminal::Running {
                n.state = NodeTerminal::Pending;
            }
        }
        self.interruptions.push(Interruption {
            unix: now,
            nodes: interrupted.clone(),
        });
        interrupted
    }

    /// Decide what a restart may do with this mission from the record
    /// alone (T-DEEPSEEK-93): a mission with a recorded `Failed` node
    /// fails cleanly naming the verdict rather than wedging on a dispatch
    /// refusal, and a fully completed mission has nothing left to drive.
    /// An empty node set is a fresh mission — resumable by definition.
    pub fn resume_verdict(&self) -> ResumeVerdict {
        if !self.nodes.is_empty()
            && self
                .nodes
                .values()
                .all(|n| n.state == NodeTerminal::Completed)
        {
            return ResumeVerdict::Complete;
        }
        let failed: Vec<(String, String)> = self
            .nodes
            .values()
            .filter(|n| n.state == NodeTerminal::Failed)
            .map(|n| {
                (
                    n.id.clone(),
                    n.output
                        .clone()
                        .unwrap_or_else(|| "(no output)".to_string()),
                )
            })
            .collect();
        if !failed.is_empty() {
            return ResumeVerdict::TerminalFailed(failed);
        }
        ResumeVerdict::Resumable
    }

    /// Mark a pending runnable node as running (dispatch).
    pub fn dispatch(&mut self, id: &str) -> EaiResult<()> {
        let deps = {
            let node = self
                .nodes
                .get(id)
                .ok_or_else(|| EaiError::governance(format!("unknown node {id}")))?;
            if node.state != NodeTerminal::Pending {
                return Err(EaiError::governance(format!(
                    "refuse dispatch of non-pending node {id}"
                )));
            }
            node.dependencies.clone()
        };
        for d in &deps {
            let ok = self
                .nodes
                .get(d)
                .is_some_and(|dep| dep.state == NodeTerminal::Completed);
            if !ok {
                return Err(EaiError::governance(format!("deps incomplete for {id}")));
            }
        }
        if let Some(node) = self.nodes.get_mut(id) {
            node.state = NodeTerminal::Running;
        }
        Ok(())
    }

    pub fn complete(&mut self, id: &str, output: String) -> EaiResult<()> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| EaiError::governance(format!("unknown node {id}")))?;
        if node.state != NodeTerminal::Running {
            return Err(EaiError::governance(format!(
                "refuse complete of non-running node {id}"
            )));
        }
        node.output = Some(output);
        node.state = NodeTerminal::Completed;
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> EaiResult<PathBuf> {
        std::fs::create_dir_all(dir).map_err(|e| EaiError::io(e.to_string()))?;
        let path = dir.join(format!("{}.json", self.mission_id));
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| EaiError::internal(format!("serialize mission: {e}")))?;
        crate::susi_config::atomic_write_bytes(&path, body.as_bytes())
            .map_err(|e| EaiError::filesystem(format!("write {}: {e}", path.display())))?;
        Ok(path)
    }

    pub fn load(path: &Path) -> EaiResult<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| EaiError::filesystem(format!("read {}: {e}", path.display())))?;
        let mut mission: Self = serde_json::from_str(&raw)
            .map_err(|e| EaiError::internal(format!("parse mission: {e}")))?;
        // Older mission records stored leases before the explicit mission
        // ownership fields existed.  Preserve the stronger state and union
        // recovery markers when loading either schema.
        mission.ownership = mission.ownership.max(mission.leases.epoch);
        mission
            .leases
            .recovery
            .active_scopes
            .extend(mission.recovery.active_scopes.iter().cloned());
        mission.recovery = mission.leases.recovery.clone();
        Ok(mission)
    }

    /// Workspace-local missions directory (`<workspace>/.susi/missions`).
    #[must_use]
    pub fn missions_dir(workspace: &Path) -> PathBuf {
        workspace.join(".susi").join("missions")
    }

    /// Workspace-local snapshots directory — one subdirectory per mission
    /// (`<workspace>/.susi/mission-snapshots/<mission_id>/`), so a
    /// rollback touches only its own mission's files (T-DEEPSEEK-94).
    #[must_use]
    pub fn snapshots_dir(workspace: &Path, mission_id: &str) -> PathBuf {
        workspace
            .join(".susi")
            .join("mission-snapshots")
            .join(mission_id)
    }

    /// Copy the current durable record into the snapshots dir as
    /// `<label>.json` — a verbatim restore point taken before risky work
    /// (the resume path snapshots the pre-resume state). `keep_last`
    /// bounds the pile: the oldest snapshots are pruned first. Returns
    /// the snapshot file name.
    pub fn snapshot(
        &self,
        workspace: &Path,
        label: &str,
        keep_last: usize,
        now: u64,
    ) -> EaiResult<String> {
        let dir = Self::snapshots_dir(workspace, &self.mission_id);
        std::fs::create_dir_all(&dir).map_err(|e| EaiError::io(e.to_string()))?;
        let safe: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let name = format!("{now:020}-{safe}.json");
        let path = dir.join(&name);
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| EaiError::internal(format!("serialize snapshot: {e}")))?;
        crate::susi_config::atomic_write_bytes(&path, body.as_bytes())
            .map_err(|e| EaiError::filesystem(format!("write {}: {e}", path.display())))?;
        self.prune_snapshots(workspace, keep_last);
        Ok(name)
    }

    /// Snapshot file names for this mission, oldest first.
    #[must_use]
    pub fn list_snapshots(workspace: &Path, mission_id: &str) -> Vec<String> {
        let dir = Self::snapshots_dir(workspace, mission_id);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                name.ends_with(".json").then_some(name)
            })
            .collect();
        names.sort();
        names
    }

    /// The newest snapshot name, if any — the latest known-good point.
    #[must_use]
    pub fn latest_snapshot(workspace: &Path, mission_id: &str) -> Option<String> {
        Self::list_snapshots(workspace, mission_id).pop()
    }

    /// Drop the oldest snapshots past `keep_last`. Names sort by their
    /// zero-padded timestamp prefix, so lexical order is chronological.
    fn prune_snapshots(&self, workspace: &Path, keep_last: usize) {
        let names = Self::list_snapshots(workspace, &self.mission_id);
        if names.len() <= keep_last {
            return;
        }
        for name in &names[..names.len() - keep_last] {
            let _ =
                std::fs::remove_file(Self::snapshots_dir(workspace, &self.mission_id).join(name));
        }
    }

    /// Roll this mission back to a snapshot — the newest when `snapshot`
    /// is `None`. The restored record is written back atomically as the
    /// mission's current state with the restore appended to `restores`,
    /// naming the node states it replaced; every other mission's file is
    /// untouched (T-DEEPSEEK-94). The normal resume machinery then
    /// continues the restored record — completed nodes stay done,
    /// interrupted `Running` nodes fold back, and a previously recorded
    /// `Failed` verdict is replaced by the pre-failure state rather than
    /// silently retried.
    pub fn rollback(
        workspace: &Path,
        mission_id: &str,
        snapshot: Option<&str>,
        now: u64,
    ) -> EaiResult<PersistedMission> {
        let name = match snapshot {
            Some(n) => n.to_string(),
            None => Self::latest_snapshot(workspace, mission_id).ok_or_else(|| {
                EaiError::governance(format!("no snapshot to roll mission {mission_id} back to"))
            })?,
        };
        if name.contains('/') || name.contains("..") {
            return Err(EaiError::governance(format!("bad snapshot name {name}")));
        }
        let snap_path = Self::snapshots_dir(workspace, mission_id).join(&name);
        let mut restored = Self::load(&snap_path)
            .map_err(|e| EaiError::filesystem(format!("load snapshot {name}: {e}")))?;
        restored.mission_id = mission_id.to_string();
        // The record of what the rollback replaced — from the current
        // mission file when one exists.
        let current_path = Self::missions_dir(workspace).join(format!("{mission_id}.json"));
        let replaced: Vec<String> = match Self::load(&current_path) {
            Ok(current) => current
                .nodes
                .values()
                .map(|n| format!("{}:{}", n.id, n.state.name()))
                .collect(),
            Err(_) => vec!["(no current record)".to_string()],
        };
        restored.restores.push(Restore {
            unix: now,
            snapshot: name,
            replaced,
        });
        restored
            .save(&Self::missions_dir(workspace))
            .map_err(|e| EaiError::filesystem(format!("restore mission {mission_id}: {e}")))?;
        Ok(restored)
    }

    /// Stable mission id derived from the goal (hex of DefaultHasher).
    #[must_use]
    pub fn mission_id_for_goal(goal: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        goal.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Load an existing mission file or create an empty one for `mission_id`.
    pub fn load_or_new(dir: &Path, mission_id: &str) -> EaiResult<Self> {
        let path = dir.join(format!("{mission_id}.json"));
        if path.is_file() {
            Self::load(&path)
        } else {
            Ok(Self::new(mission_id))
        }
    }
}
