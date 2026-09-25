//! Durable Workflows & Task Journal (Swarm OS Bullet 8)
//!
//! A durable event journal that persists pending tasks and orchestrates
//! workflow state. If a node crashes, the Swarm OS rehydrates state and
//! resumes execution of pending tasks without human intervention.
//! Uses an append-only JSONL log (to avoid unsafe SQLite C-FFI).

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

/// The state of a distributed Swarm OS Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Pending,
    Assigned,
    Running,
    Completed,
    Failed,
}

/// A durable task within the Swarm OS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableTask {
    pub task_id: String,
    pub capability_req: String,
    pub payload: String,
    pub state: TaskState,
    pub assigned_cell: Option<String>,
    pub created_at: u64,
}

/// Task state transition event for the append-only journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum JournalEvent {
    Create(DurableTask),
    UpdateState {
        task_id: String,
        state: TaskState,
        cell: Option<String>,
    },
}

/// The Durable Workflow Engine.
pub struct WorkflowEngine {
    journal_file: Mutex<File>,
    active_tasks: RwLock<HashMap<String, DurableTask>>,
}

impl WorkflowEngine {
    /// Creates or loads a durable workflow engine from the given workspace.
    pub fn new(workspace: &Path) -> std::io::Result<Self> {
        let journal_path = workspace.join("workflows.journal");

        let mut active_tasks = HashMap::new();

        // Rehydrate state if journal exists
        if journal_path.exists() {
            let file = File::open(&journal_path)?;
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<JournalEvent>(&line) {
                    match event {
                        JournalEvent::Create(task) => {
                            active_tasks.insert(task.task_id.clone(), task);
                        }
                        JournalEvent::UpdateState {
                            task_id,
                            state,
                            cell,
                        } => {
                            if let Some(task) = active_tasks.get_mut(&task_id) {
                                task.state = state;
                                if cell.is_some() {
                                    task.assigned_cell = cell;
                                }
                            }
                        }
                    }
                }
            }
        }

        let journal_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal_path)?;

        Ok(Self {
            journal_file: Mutex::new(journal_file),
            active_tasks: RwLock::new(active_tasks),
        })
    }

    /// Appends an event to the durable journal.
    fn append_event(&self, event: &JournalEvent) -> std::io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut file = self.journal_file.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(file, "{}", line)?;
        file.sync_data()?; // fsync for durability
        Ok(())
    }

    /// Submits a new durable task to the workflow engine.
    pub fn submit_task(
        &self,
        task_id: String,
        capability_req: String,
        payload: String,
    ) -> std::io::Result<()> {
        let task = DurableTask {
            task_id: task_id.clone(),
            capability_req,
            payload,
            state: TaskState::Pending,
            assigned_cell: None,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        self.append_event(&JournalEvent::Create(task.clone()))?;
        self.active_tasks.write().insert(task_id, task);
        Ok(())
    }

    /// Transitions a task to a new state.
    pub fn update_task_state(
        &self,
        task_id: &str,
        state: TaskState,
        cell: Option<String>,
    ) -> std::io::Result<()> {
        self.append_event(&JournalEvent::UpdateState {
            task_id: task_id.to_string(),
            state,
            cell: cell.clone(),
        })?;

        if let Some(task) = self.active_tasks.write().get_mut(task_id) {
            task.state = state;
            if cell.is_some() {
                task.assigned_cell = cell;
            }
        }
        Ok(())
    }

    /// Returns all tasks currently pending assignment.
    pub fn get_pending_tasks(&self) -> Vec<DurableTask> {
        self.active_tasks
            .read()
            .values()
            .filter(|t| t.state == TaskState::Pending)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn test_durable_workflow_rehydration() {
        let temp_dir = env::temp_dir().join(format!("susi_workflow_test_{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();

        // 1. Start engine, submit task
        {
            let engine = WorkflowEngine::new(&temp_dir).unwrap();
            engine
                .submit_task("task-1".into(), "git_diff".into(), "{}".into())
                .unwrap();
            engine
                .update_task_state("task-1", TaskState::Assigned, Some("cell-x".into()))
                .unwrap();
        }

        // 2. Simulate crash, start new engine and rehydrate
        {
            let engine = WorkflowEngine::new(&temp_dir).unwrap();
            let tasks = engine.active_tasks.read();
            assert_eq!(tasks.len(), 1);
            let t = tasks.get("task-1").unwrap();
            assert_eq!(t.state, TaskState::Assigned);
            assert_eq!(t.assigned_cell.as_deref(), Some("cell-x"));
        }

        let _ = fs::remove_dir_all(temp_dir);
    }
}
