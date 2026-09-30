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

/// One-line CLI status for durable mission resume. Partial/resumable missions
/// never claim full success (VC-201-030).
#[must_use]
pub fn cli_status_line(view: &MissionView) -> String {
    let targets: Vec<&str> = view
        .resume_targets()
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    match view.summary() {
        MissionSummary::FullyComplete => {
            format!("mission {} fully complete", view.mission_id)
        }
        MissionSummary::Resumable => format!(
            "mission {} resumable (partial); resume nodes: {}",
            view.mission_id,
            if targets.is_empty() {
                "(none)".to_string()
            } else {
                targets.join(", ")
            }
        ),
        MissionSummary::Partial => format!(
            "mission {} partial (not fully complete); no resumable nodes",
            view.mission_id
        ),
    }
}
