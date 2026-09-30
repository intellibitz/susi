//! Durable mission resume and partial outcome views (VC-201-030).
//!
//! CLI/API views of blocked, running, cancelled, uncertain, and completed
//! DAG nodes; restart resumes only supported work and never reports partial
//! success as full completion.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeView {
    Blocked,
    Running,
    Cancelled,
    Uncertain,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNodeView {
    pub id: String,
    pub view: NodeView,
    pub resumable: bool,
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionView {
    pub mission_id: String,
    pub nodes: BTreeMap<String, DagNodeView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionSummary {
    /// Every node Completed.
    FullyComplete,
    /// Mix of terminal states — must not be reported as full success.
    Partial,
    /// Work remains that a restart may resume.
    Resumable,
}

impl MissionView {
    #[must_use]
    pub fn new(mission_id: impl Into<String>) -> Self {
        Self {
            mission_id: mission_id.into(),
            nodes: BTreeMap::new(),
        }
    }

    pub fn upsert(&mut self, node: DagNodeView) {
        self.nodes.insert(node.id.clone(), node);
    }

    #[must_use]
    pub fn summary(&self) -> MissionSummary {
        if self.nodes.is_empty() {
            return MissionSummary::Partial;
        }
        let all_completed = self.nodes.values().all(|n| n.view == NodeView::Completed);
        if all_completed {
            return MissionSummary::FullyComplete;
        }
        let any_resumable = self.nodes.values().any(|n| n.resumable);
        if any_resumable {
            MissionSummary::Resumable
        } else {
            MissionSummary::Partial
        }
    }

    /// Nodes a restart drill may re-dispatch.
    #[must_use]
    pub fn resume_targets(&self) -> Vec<&DagNodeView> {
        self.nodes
            .values()
            .filter(|n| {
                n.resumable
                    && matches!(
                        n.view,
                        NodeView::Blocked | NodeView::Running | NodeView::Uncertain
                    )
            })
            .collect()
    }

    /// Never treat partial success as full completion.
    #[must_use]
    pub fn reports_full_success(&self) -> bool {
        self.summary() == MissionSummary::FullyComplete
    }
}
