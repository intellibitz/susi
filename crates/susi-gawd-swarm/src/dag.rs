// Dependency-ordered task graph: agents can spawn sub-tasks with
// dependencies on parent tasks, executed in ready-batches via rayon.

use crate::mission_persist::{NodeTerminal, PersistedMission, PersistedNode};
use crate::susi_core::evidence::EvidenceRecord;
use crate::susi_error::{EaiError, EaiResult};
use crate::task_lease::{CompleteVerdict, LeaseTable, TaskLease};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use susi_gawd_agents::agents::MissionBlackboard;

#[derive(Debug, Clone)]
pub struct TaskNode {
    pub task_id: usize,
    pub title: String,
    pub goal: String,
    pub dependencies: Vec<usize>,
    pub assigned_agent: Option<String>,
    pub completed: bool,
}

pub struct MissionDag {
    pub nodes: Vec<TaskNode>,
    /// Per-dispatch ownership fences (VC-201-022). Completions without the
    /// live fence are refused so a recovered worker cannot publish.
    pub leases: LeaseTable,
}

pub type SwarmDag = MissionDag;

/// Borrowed mission persistence handle passed through DAG execution.
pub struct MissionPersistCtx<'a> {
    pub mission: &'a mut PersistedMission,
    pub dir: &'a Path,
}

/// One node's run: index, output, elapsed ms, receipt args, lease owner, fence.
type NodeRun = (usize, EaiResult<String>, u64, Vec<String>, String, u64);

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MissionDag {
    pub fn new(initial_goal: &str) -> Self {
        Self {
            nodes: vec![TaskNode {
                task_id: 0,
                title: "Primary Mission Analysis".to_string(),
                goal: initial_goal.to_string(),
                dependencies: vec![],
                assigned_agent: None,
                completed: false,
            }],
            leases: LeaseTable::new(),
        }
    }

    /// Stable persisted node id for a DAG index (`n0`, `n1`, …).
    #[must_use]
    pub fn persist_id(idx: usize) -> String {
        format!("n{idx}")
    }

    /// Append a dependent task node; returns its index.
    pub fn push_node(
        &mut self,
        title: impl Into<String>,
        goal: impl Into<String>,
        deps: Vec<usize>,
    ) -> usize {
        let task_id = self.nodes.len();
        self.nodes.push(TaskNode {
            task_id,
            title: title.into(),
            goal: goal.into(),
            dependencies: deps,
            assigned_agent: None,
            completed: false,
        });
        task_id
    }

    /// Snapshot live DAG state into a [`PersistedMission`] (VC-201-021 wiring).
    #[must_use]
    pub fn to_persisted(&self, mission_id: &str) -> PersistedMission {
        let mut mission = PersistedMission::new(mission_id);
        for (idx, node) in self.nodes.iter().enumerate() {
            let id = Self::persist_id(idx);
            let state = if node.completed {
                NodeTerminal::Completed
            } else {
                NodeTerminal::Pending
            };
            mission.upsert_node(PersistedNode {
                id,
                input: node.goal.clone(),
                dependencies: node
                    .dependencies
                    .iter()
                    .map(|&d| Self::persist_id(d))
                    .collect(),
                output: None,
                state,
            });
        }
        mission
    }

    /// Rebuild a DAG from persisted state. Completed nodes stay completed so
    /// resume skips them; unfinished nodes stay pending/runnable.
    #[must_use]
    pub fn from_persisted(mission: &PersistedMission) -> Self {
        // Ordered by numeric suffix of n<idx> when parseable, else by id.
        let mut entries: Vec<&PersistedNode> = mission.nodes.values().collect();
        entries.sort_by_key(|n| {
            n.id.strip_prefix('n')
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(usize::MAX)
        });
        let id_to_idx: std::collections::BTreeMap<&str, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let nodes = entries
            .iter()
            .enumerate()
            .map(|(idx, n)| TaskNode {
                task_id: idx,
                title: n.id.clone(),
                goal: n.input.clone(),
                dependencies: n
                    .dependencies
                    .iter()
                    .filter_map(|d| id_to_idx.get(d.as_str()).copied())
                    .collect(),
                assigned_agent: None,
                completed: n.state == NodeTerminal::Completed,
            })
            .collect();
        Self {
            nodes,
            leases: LeaseTable::new(),
        }
    }

    /// Issue an expiring ownership fence for a ready DAG node (dispatch).
    pub fn lease_dispatch(&mut self, idx: usize, owner: &str, now: u64, ttl: u64) -> TaskLease {
        self.leases
            .dispatch(&Self::persist_id(idx), owner, now, ttl)
    }

    /// Authoritative completion under the live fence. Stale/expired tokens
    /// leave the node unfinished.
    pub fn lease_complete(
        &mut self,
        idx: usize,
        owner: &str,
        fence: u64,
        now: u64,
    ) -> CompleteVerdict {
        let verdict = self
            .leases
            .complete(&Self::persist_id(idx), owner, fence, now);
        if verdict == CompleteVerdict::Accepted {
            if let Some(node) = self.nodes.get_mut(idx) {
                node.completed = true;
            }
        }
        verdict
    }

    /// Apply a node completion into the persisted mission and save it.
    pub fn persist_node_complete(
        persist: &mut PersistedMission,
        persist_dir: &Path,
        idx: usize,
        output: &str,
    ) -> EaiResult<()> {
        let id = Self::persist_id(idx);
        let Some(node) = persist.nodes.get_mut(&id) else {
            return Err(EaiError::governance(format!(
                "persist missing node {id} on complete"
            )));
        };
        // Force authoritative completion regardless of prior Pending/Running —
        // resume must see Completed or it will re-dispatch.
        node.output = Some(output.to_string());
        node.state = NodeTerminal::Completed;
        persist.save(persist_dir)?;
        Ok(())
    }

    /// Seed persist from this DAG when the mission file has no nodes yet.
    pub fn seed_persist(&self, persist: &mut PersistedMission) {
        if !persist.nodes.is_empty() {
            return;
        }
        *persist = self.to_persisted(&persist.mission_id);
    }

    /// Execute the DAG topologically using work-stealing parallel execution
    pub fn execute_dag(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.execute_dag_inner(workspace, blackboard, event_sender, None)
    }

    /// Execute while persisting each successful node completion (resume-safe).
    pub fn execute_dag_persisted(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        persist: &mut MissionPersistCtx<'_>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.seed_persist(persist.mission);
        persist.mission.save(persist.dir)?;
        self.execute_dag_inner(workspace, blackboard, event_sender, Some(persist))
    }

    fn execute_dag_inner(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        mut persist_slot: Option<&mut MissionPersistCtx<'_>>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        use rayon::prelude::*;
        let mut all_evidence = Vec::new();

        let mut executed_count = self.nodes.iter().filter(|node| node.completed).count();
        let total_nodes = self.nodes.len();

        while executed_count < total_nodes {
            let ready_indices: Vec<usize> = self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    !node.completed
                        && node
                            .dependencies
                            .iter()
                            .all(|&dep| self.nodes.get(dep).is_some_and(|node| node.completed))
                })
                .map(|(idx, _)| idx)
                .collect();

            if ready_indices.is_empty() {
                return Err(EaiError::governance(
                    "DAG_EXECUTION_FAILED: unresolved dependencies (cycle or missing task)",
                ));
            }

            // Mark ready nodes Running in persist before the parallel batch.
            if let Some(ctx) = persist_slot.as_mut() {
                for &idx in &ready_indices {
                    let id = Self::persist_id(idx);
                    let _ = ctx.mission.dispatch(&id);
                }
                let _ = ctx.mission.save(ctx.dir);
            }

            // Assign expiring ownership fences before workers run (VC-201-022).
            let now = now_unix();
            let lease_ttl = 3_600;
            let mut batch_leases: Vec<(usize, String, u64)> =
                Vec::with_capacity(ready_indices.len());
            for &idx in &ready_indices {
                let owner = format!("worker-{idx}");
                let lease = self.lease_dispatch(idx, &owner, now, lease_ttl);
                batch_leases.push((idx, owner, lease.fence));
            }

            let ws = workspace.to_path_buf();
            let bb = Arc::clone(blackboard);
            let tx = event_sender.clone();

            let batch_results: Vec<NodeRun> = batch_leases
                .into_par_iter()
                .map(|(idx, owner, fence)| {
                    let node = &self.nodes[idx];
                    let start = std::time::Instant::now();
                    let _ = tx.send(crate::susi_core::bus::SwarmEventType::AgentStarted {
                        agent_name: node.title.clone(),
                    });

                    // Mandate 48: in SUSI's own tree the node that turns model
                    // output into shell commands carries the self-build contract.
                    let prompt = crate::susi_core::self_build::brief_task(&ws, &format!(
                        "Execute task node '{}': {}. If you need to execute a shell command, provide it in a ```bash codeblock. The command must perform every requested side effect: printing intended file content is not file creation. For file writes, write the named workspace path and then verify that exact path and its contents.",
                        node.title, node.goal
                    ));
                    // Deep path, same rule as plan_steps: the node template is
                    // full of capability words and clears Tier-0's support
                    // gate, so a trained reflex could answer `ACTION:
                    // write_file` with no ```bash block — the node would
                    // "complete" having executed nothing (Claude's measured
                    // 0.60–0.64 support on everyday intents).
                    let mut res =
                        crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
                            &prompt, &ws,
                        );

                    let mut executed_scripts = String::new();
                    // Receipt arguments this node produced (JSON of the
                    // exec_command argument), for binding its own evidence.
                    let mut node_calls = Vec::new();
                    for block in res.split("```").skip(1).step_by(2) {
                        if let Some(cmd) = shell_block_command(block) {
                            eprintln!("[DAG Agent] Detected shell block. Executing native tool...");
                            let wrapped_cmd = format!("sh -c '{}'", cmd.replace('\'', "'\\''"));
                            let call = serde_json::Value::String(wrapped_cmd);
                            node_calls.push(call.to_string());
                            let result = crate::susi_core::plane_bus::tools::execute_tool("exec_command", &call, &ws).unwrap_or_else(|e| format!("[Error] {e}"));
                            executed_scripts.push_str(&format!("\n\nExecution Result for `{cmd}`:\n{}\n", result));
                        }
                    }

                    if !executed_scripts.is_empty() {
                        res.push_str(&executed_scripts);
                        if let Some(citations) = crate::susi_core::capture::EvidenceSession::auto_format_truth(&ws) {
                            res.push_str("\n\n");
                            res.push_str(&citations);
                        }
                    }

                    let elapsed = start.elapsed().as_millis() as u64;
                    (idx, Ok(res), elapsed, node_calls, owner, fence)
                })
                .collect();

            for (idx, res, elapsed, node_calls, owner, fence) in batch_results {
                if let Ok(output) = res {
                    // Crown path: citation answers resolve from the live ledger;
                    // narratives without required citations fail TRUTH_UNVERIFIED.
                    match crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                        &self.nodes[idx].goal,
                        &self.nodes[idx].title,
                        &output,
                        workspace,
                    ) {
                        Ok(verified) => {
                            let complete_now = now_unix();
                            match self.lease_complete(idx, &owner, fence, complete_now) {
                                CompleteVerdict::Accepted => {}
                                CompleteVerdict::StaleFence
                                | CompleteVerdict::NotOwner
                                | CompleteVerdict::Expired
                                | CompleteVerdict::UnknownTask => {
                                    return Err(EaiError::governance(format!(
                                        "DAG_EXECUTION_FAILED: lease fence refused completion of n{idx}"
                                    )));
                                }
                            }
                            let _ =
                                event_sender.send(crate::susi_core::bus::SwarmEventType::AgentCompleted {
                                    agent_name: self.nodes[idx].title.clone(),
                                    elapsed_ms: elapsed,
                                });
                            // lease_complete already marked completed=true
                            executed_count += 1;
                            bb.insert(format!("TaskNode_{}", idx), verified.clone());

                            if let Some(ctx) = persist_slot.as_mut() {
                                let _ = Self::persist_node_complete(
                                    ctx.mission,
                                    ctx.dir,
                                    idx,
                                    &verified,
                                );
                            }

                            // Pillar Evidence: only store Claim trails that assess Verified
                            // (ToolReceipt-bound). Naked AgentObservation IR is not a trail.
                            if let Some(session) =
                                crate::susi_core::capture::EvidenceSession::for_workspace(workspace)
                            {
                                // This node's own successful call — not the
                                // session's first receipt, which bound every
                                // node to whatever tool ran earliest (often
                                // another node's, or an unrelated call).
                                if let Some(receipt) = session.receipts().into_iter().rev().find(|r| {
                                    r.successful
                                        && r.tool == "exec_command"
                                        && node_calls.contains(&r.arguments)
                                }) {
                                    if let Ok(record) = session.bind_receipt(
                                        &receipt.id,
                                        &self.nodes[idx].title,
                                        crate::susi_core::evidence::Claim {
                                            subject: self.nodes[idx].title.clone(),
                                            predicate: "observed".to_string(),
                                            value: receipt.output_hash.clone(),
                                        },
                                    ) {
                                        if record.verify_reality(workspace) {
                                            all_evidence.push(record);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let msg = format!("[TRUTH_VIOLATION] {}", e);
                            bb.insert(format!("TaskNode_{}", idx), msg);
                            return Err(e);
                        }
                    }
                }
            }
        }

        Ok(all_evidence)
    }
}

/// The command in a fenced block tagged `bash`, `sh`, or `shell`. The tag
/// is the whole first line: chained `trim_start_matches` calls used to eat
/// the `sh` of `shell` and run a command starting with `ell`.
fn shell_block_command(block: &str) -> Option<&str> {
    let (tag, body) = block.trim_start().split_once('\n')?;
    if !matches!(tag.trim(), "bash" | "sh" | "shell") {
        return None;
    }
    let cmd = body.trim();
    (!cmd.is_empty() && !cmd.starts_with('!')).then_some(cmd)
}

/// Hook body registered into `susi_gawd_agents::dag_hooks` by [`crate::init`].
///
/// Loads any prior [`PersistedMission`] under `<workspace>/.susi/missions`,
/// resumes completed nodes without re-running them, and persists each new
/// completion so a crash mid-mission does not treat unfinished work as done.
pub fn dispatch_mission_dag(
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let persist_dir = PersistedMission::missions_dir(workspace);
    let mission_id = PersistedMission::mission_id_for_goal(goal);
    let mut persist = PersistedMission::load_or_new(&persist_dir, &mission_id)
        .unwrap_or_else(|_| PersistedMission::new(mission_id.clone()));

    let mut dag = if persist.nodes.is_empty() {
        let dag = MissionDag::new(goal);
        dag.seed_persist(&mut persist);
        let _ = persist.save(&persist_dir);
        dag
    } else {
        MissionDag::from_persisted(&persist)
    };

    let (event_tx, _event_rx) = crate::susi_core::bus::create_swarm_bus();
    let mut persist_ctx = MissionPersistCtx {
        mission: &mut persist,
        dir: &persist_dir,
    };
    match dag.execute_dag_persisted(workspace, blackboard, &event_tx, &mut persist_ctx) {
        Ok(evidence_records) => {
            for record in &evidence_records {
                let payload =
                    serde_json::to_string(record).unwrap_or_else(|_| record.render_for_gemi());
                blackboard.insert(format!("EvidenceRecord::{}", record.claim.subject), payload);
            }
            if let Some(record) = evidence_records.first() {
                results.push(("MissionDag".to_string(), record.claim.value.clone()));
            }
        }
        Err(e) => {
            results.push((
                "MissionDag".to_string(),
                format!("[DAG_EXECUTION_FAILED] {}", e),
            ));
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_gawd_agents::HighDensityContextStore;

    #[test]
    fn invalid_dependencies_fail_without_completing_tasks() {
        for dependency in [0, 99] {
            let mut dag = MissionDag::new("inspect");
            dag.nodes[0].dependencies = vec![dependency];
            let board = Arc::new(HighDensityContextStore::new(1024));
            let (tx, rx) = crate::susi_core::bus::create_swarm_bus();
            assert!(dag.execute_dag(Path::new("."), &board, &tx).is_err());
            assert!(!dag.nodes[0].completed);
            assert!(board.is_empty());
            assert!(rx.is_empty());
        }
    }

    #[test]
    fn completed_graph_is_not_executed_again() {
        let mut dag = MissionDag::new("inspect");
        dag.nodes[0].completed = true;
        let board = Arc::new(HighDensityContextStore::new(1024));
        let (tx, rx) = crate::susi_core::bus::create_swarm_bus();
        assert!(dag
            .execute_dag(Path::new("."), &board, &tx)
            .unwrap()
            .is_empty());
        assert!(rx.is_empty());
    }
}

#[cfg(test)]
mod shell_block_tests {
    use super::shell_block_command;

    #[test]
    fn every_tag_yields_the_whole_command() {
        assert_eq!(shell_block_command("bash\necho hi\n"), Some("echo hi"));
        assert_eq!(shell_block_command("sh\nls -la"), Some("ls -la"));
        assert_eq!(shell_block_command("shell\nls -la"), Some("ls -la"));
        assert_eq!(shell_block_command("shellscript\nls"), None);
        assert_eq!(shell_block_command("rust\nfn main() {}"), None);
        assert_eq!(shell_block_command("bash\n!sudo reboot"), None);
        assert_eq!(shell_block_command("bash\n   "), None);
    }
}
