// Dependency-ordered task graph: agents can spawn sub-tasks with
// dependencies on parent tasks, executed in ready-batches via rayon.

use super::agents::MissionBlackboard;
use super::evidence::EvidenceRecord;
use susi_error::EaiResult;
use std::path::Path;
use std::sync::Arc;

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

        let mut executed_count = 0;
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
                            .all(|&dep| self.nodes[dep].completed)
                })
                .map(|(idx, _)| idx)
                .collect();

            if ready_indices.is_empty() {
                break;
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
                    let _ = tx.send(super::bus::SwarmEventType::AgentCompleted {
                        agent_name: node.title.clone(),
                        elapsed_ms: elapsed,
                    });

                    (idx, Ok(res), elapsed)
                })
                .collect();

            for (idx, res, _elapsed) in batch_results {
                if let Ok(output) = res {
                    self.nodes[idx].completed = true;
                    executed_count += 1;

                    let record = EvidenceRecord::new(
                        self.nodes[idx].title.clone(),
                        0.95,
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        super::evidence::Claim {
                            subject: self.nodes[idx].title.clone(),
                            predicate: "achieved_goal".to_string(),
                            value: output.chars().take(120).collect(),
                        },
                        super::evidence::EvidenceSource::AgentObservation {
                            observation: output.clone(),
                            reasoning_trace: "DAG Work-Stealing Parallel Loop".to_string(),
                        },
                        0.92,
                    );

                    bb.insert(format!("TaskNode_{}", idx), output);
                    all_evidence.push(record);
                }
            }
        }

        Ok(all_evidence)
    }
}
