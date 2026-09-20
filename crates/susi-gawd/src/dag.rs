// Dependency-ordered task graph: agents can spawn sub-tasks with
// dependencies on parent tasks, executed in ready-batches via rayon.

use super::agents::MissionBlackboard;
use super::evidence::EvidenceRecord;
use std::path::Path;
use std::sync::Arc;
use susi_error::{EaiError, EaiResult};

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

    /// Dynamically spawn a sub-task dependent on parent task completion
    pub fn spawn_subtask(&mut self, title: &str, goal: &str, parent_id: usize) -> usize {
        let new_id = self.nodes.len();
        self.nodes.push(TaskNode {
            task_id: new_id,
            title: title.to_string(),
            goal: goal.to_string(),
            dependencies: vec![parent_id],
            assigned_agent: None,
            completed: false,
        });
        eprintln!(
            "[DAG Sub-Task Spawned] Task #{} '{}' dependent on Task #{}",
            new_id, title, parent_id
        );
        new_id
    }

    /// Execute the DAG topologically using work-stealing parallel execution
    pub fn execute_dag(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<super::bus::SwarmEventType>,
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
                    let _ = tx.send(super::bus::SwarmEventType::AgentStarted {
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
                    let record = EvidenceRecord::new(
                        self.nodes[idx].title.clone(),
                        0.95,
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        super::evidence::Claim {
                            subject: self.nodes[idx].title.clone(),
                            predicate: "reported_result".to_string(),
                            value: output.chars().take(120).collect(),
                        },
                        super::evidence::EvidenceSource::AgentObservation {
                            observation: output.chars().take(500).collect(),
                            reasoning_trace: format!(
                                "Task '{}': {}\n\nOutput:\n{}",
                                self.nodes[idx].title, self.nodes[idx].goal, output
                            ),
                        },
                        0.92,
                    );

                    // Dual-pipeline truth: physical signature + semantic
                    // cross-examine via discovered CapabilityRegistry providers.
                    let verification = super::truth::TruthTransformer::verify_mission_reality(
                        &self.nodes[idx].goal,
                        &self.nodes[idx].title,
                        &output,
                        workspace,
                    )
                    .and_then(|_| {
                        super::truth::TruthTransformer::cross_examine_sync(&record, workspace)
                    });
                    match verification {
                        Ok(()) => {
                            let _ = event_sender.send(super::bus::SwarmEventType::AgentCompleted {
                                agent_name: self.nodes[idx].title.clone(),
                                elapsed_ms: elapsed,
                            });
                            self.nodes[idx].completed = true;
                            executed_count += 1;
                            bb.insert(format!("TaskNode_{}", idx), output);
                            all_evidence.push(record);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::HighDensityContextStore;

    #[test]
    fn invalid_dependencies_fail_without_completing_tasks() {
        for dependency in [0, 99] {
            let mut dag = MissionDag::new("inspect");
            dag.nodes[0].dependencies = vec![dependency];
            let board = Arc::new(HighDensityContextStore::new(1024));
            let (tx, rx) = crate::bus::create_swarm_bus();
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
        let (tx, rx) = crate::bus::create_swarm_bus();
        assert!(dag
            .execute_dag(Path::new("."), &board, &tx)
            .unwrap()
            .is_empty());
        assert!(rx.is_empty());
    }
}
