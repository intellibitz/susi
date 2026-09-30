//! Persist executable mission DAG state (VC-201-021).
//!
//! Task inputs, dependencies, outputs, and terminal states survive restart;
//! unfinished nodes stay unfinished so resume only dispatches ready work.

use crate::susi_error::{EaiError, EaiResult};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedNode {
    pub id: String,
    pub input: String,
    pub dependencies: Vec<String>,
    pub output: Option<String>,
    pub state: NodeTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedMission {
    pub mission_id: String,
    pub nodes: BTreeMap<String, PersistedNode>,
    /// Durable lease/fence state (T-DEVIN-9): survives restart so stale
    /// workers can't collide with freshly issued fences.
    #[serde(default)]
    pub leases: crate::task_lease::LeaseTable,
}

impl PersistedMission {
    #[must_use]
    pub fn new(mission_id: impl Into<String>) -> Self {
        Self {
            mission_id: mission_id.into(),
            nodes: BTreeMap::new(),
            leases: crate::task_lease::LeaseTable::new(),
        }
    }

    pub fn upsert_node(&mut self, node: PersistedNode) {
        self.nodes.insert(node.id.clone(), node);
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
        serde_json::from_str(&raw).map_err(|e| EaiError::internal(format!("parse mission: {e}")))
    }

    /// Workspace-local missions directory (`<workspace>/.susi/missions`).
    #[must_use]
    pub fn missions_dir(workspace: &Path) -> PathBuf {
        workspace.join(".susi").join("missions")
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
