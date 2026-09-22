// Dependency-ordered task graph: agents can spawn sub-tasks with
// dependencies on parent tasks, executed in ready-batches via rayon.

use std::path::Path;
use std::sync::Arc;
use susi_core::evidence::EvidenceRecord;
use susi_error::{EaiError, EaiResult};
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
}

pub type SwarmDag = MissionDag;

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
        }
    }

    /// Execute the DAG topologically using work-stealing parallel execution
    pub fn execute_dag(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<susi_core::bus::SwarmEventType>,
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

            let ws = workspace.to_path_buf();
            let bb = Arc::clone(blackboard);
            let tx = event_sender.clone();

            let batch_results: Vec<(usize, EaiResult<String>, u64)> = ready_indices
                .into_par_iter()
                .map(|idx| {
                    let node = &self.nodes[idx];
                    let start = std::time::Instant::now();
                    let _ = tx.send(susi_core::bus::SwarmEventType::AgentStarted {
                        agent_name: node.title.clone(),
                    });

                    let prompt = format!("Execute task node '{}': {}", node.title, node.goal);
                    let res = susi_gemi::engine::GemiEngine::generate_reasoning(&prompt, &ws);

                    let elapsed = start.elapsed().as_millis() as u64;
                    (idx, Ok(res), elapsed)
                })
                .collect();

            for (idx, res, elapsed) in batch_results {
                if let Ok(output) = res {
                    // Crown path: citation answers resolve from the live ledger;
                    // narratives without required citations fail TRUTH_UNVERIFIED.
                    match susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                        &self.nodes[idx].goal,
                        &self.nodes[idx].title,
                        &output,
                        workspace,
                    ) {
                        Ok(verified) => {
                            let _ =
                                event_sender.send(susi_core::bus::SwarmEventType::AgentCompleted {
                                    agent_name: self.nodes[idx].title.clone(),
                                    elapsed_ms: elapsed,
                                });
                            self.nodes[idx].completed = true;
                            executed_count += 1;
                            bb.insert(format!("TaskNode_{}", idx), verified);

                            // Pillar Evidence: only store Claim trails that assess Verified
                            // (ToolReceipt-bound). Naked AgentObservation IR is not a trail.
                            if let Some(session) =
                                susi_core::capture::EvidenceSession::for_workspace(workspace)
                            {
                                if let Some(receipt) =
                                    session.receipts().into_iter().find(|r| r.successful)
                                {
                                    if let Ok(record) = session.bind_receipt(
                                        &receipt.id,
                                        &self.nodes[idx].title,
                                        susi_core::evidence::Claim {
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

/// Hook body registered into `susi_gawd_agents::dag_hooks` by [`crate::init`].
pub fn dispatch_mission_dag(
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut dag = MissionDag::new(goal);
    let (event_tx, _event_rx) = susi_core::bus::create_swarm_bus();
    match dag.execute_dag(workspace, blackboard, &event_tx) {
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
            let (tx, rx) = susi_core::bus::create_swarm_bus();
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
        let (tx, rx) = susi_core::bus::create_swarm_bus();
        assert!(dag
            .execute_dag(Path::new("."), &board, &tx)
            .unwrap()
            .is_empty());
        assert!(rx.is_empty());
    }
}
