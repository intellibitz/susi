//! Durable mission resume and partial outcome views (VC-201-030).
//!
//! CLI/API views of blocked, running, cancelled, uncertain, and completed
//! DAG nodes; restart resumes only supported work and never reports partial
//! success as full completion.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::mission_persist::{NodeTerminal, PersistedMission, ResumeVerdict};
use crate::susi_error::EaiResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeView {
    Blocked,
    Running,
    Cancelled,
    Uncertain,
    Completed,
    /// Terminal recorded failure — never resumable, never silent.
    Failed,
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

    /// Rebuild the operator view from the durable mission record
    /// (T-DEEPSEEK-93, VC-202-011): the resume point comes from persisted
    /// node state — what a restart actually sees — not an in-memory guess.
    /// `Pending` and crash-interrupted `Running` nodes are resumable
    /// (recovery folds `Running` back to `Pending`); `Failed` is a
    /// recorded terminal verdict and is never silently retried.
    #[must_use]
    pub fn from_persisted(mission: &PersistedMission) -> Self {
        let mut view = Self::new(mission.mission_id.clone());
        for node in mission.nodes.values() {
            let (nv, resumable) = match node.state {
                NodeTerminal::Pending => (NodeView::Blocked, true),
                NodeTerminal::Running => (NodeView::Running, true),
                NodeTerminal::Completed => (NodeView::Completed, false),
                NodeTerminal::Failed => (NodeView::Failed, false),
            };
            view.upsert(DagNodeView {
                id: node.id.clone(),
                view: nv,
                resumable,
                output: node.output.clone(),
            });
        }
        view
    }
}

/// Enumerate every workspace mission whose durable record says a restart
/// may resume it — unfinished nodes remain and none carries a recorded
/// terminal `Failed` verdict (T-DEEPSEEK-93). This is the production
/// enumeration that makes the resume point explicit rather than silent.
#[must_use]
pub fn resumable_missions(workspace: &Path) -> Vec<MissionView> {
    let dir = PersistedMission::missions_dir(workspace);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(mission) = PersistedMission::load(&path) else {
            continue;
        };
        if mission.resume_verdict() == ResumeVerdict::Resumable {
            out.push(MissionView::from_persisted(&mission));
        }
    }
    out.sort_by(|a, b| a.mission_id.cmp(&b.mission_id));
    out
}

/// Roll one mission back to a snapshot — the newest when `snapshot` is
/// `None` — and return the restored record's operator view and status
/// line (T-DEEPSEEK-94). The rollback is recorded on the mission itself
/// (`restores`), every other mission's file is untouched, and the next
/// dispatch resumes the restored state through the normal recovery
/// transaction rather than restarting from scratch.
pub fn rollback_mission(
    workspace: &Path,
    mission_id: &str,
    snapshot: Option<&str>,
    now: u64,
) -> EaiResult<(MissionView, String)> {
    let restored = PersistedMission::rollback(workspace, mission_id, snapshot, now)?;
    let view = MissionView::from_persisted(&restored);
    let line = cli_status_line(&view);
    Ok((view, line))
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

#[cfg(test)]
mod inline_tests {
    use super::*;
    use crate::mission_persist::PersistedNode;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn summary_and_reports_full_success_distinguish_empty_complete_resumable_and_partial() {
        let mut view = MissionView::new("m-1");
        assert_eq!(view.summary(), MissionSummary::Partial);
        assert!(!view.reports_full_success());

        view.upsert(DagNodeView {
            id: "n1".into(),
            view: NodeView::Completed,
            resumable: false,
            output: Some("done".into()),
        });
        assert_eq!(view.summary(), MissionSummary::FullyComplete);
        assert!(view.reports_full_success());

        view.upsert(DagNodeView {
            id: "n2".into(),
            view: NodeView::Blocked,
            resumable: true,
            output: None,
        });
        assert_eq!(view.summary(), MissionSummary::Resumable);
        assert!(!view.reports_full_success());
    }

    #[test]
    fn resume_targets_returns_only_resumable_blocked_running_or_uncertain_nodes() {
        let mut view = MissionView::new("m-targets");
        view.upsert(DagNodeView {
            id: "a-done".into(),
            view: NodeView::Completed,
            resumable: false,
            output: None,
        });
        view.upsert(DagNodeView {
            id: "b-run".into(),
            view: NodeView::Running,
            resumable: true,
            output: None,
        });
        view.upsert(DagNodeView {
            id: "c-fail".into(),
            view: NodeView::Failed,
            resumable: false,
            output: Some("err".into()),
        });
        let targets: Vec<&str> = view
            .resume_targets()
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(targets, vec!["b-run"]);
    }

    #[test]
    fn from_persisted_maps_node_terminals_to_resumable_and_terminal_views() {
        let mut persisted = PersistedMission::new("m-persisted");
        persisted.upsert_node(PersistedNode {
            id: "p".into(),
            input: "i".into(),
            dependencies: vec![],
            output: None,
            state: NodeTerminal::Pending,
        });
        persisted.upsert_node(PersistedNode {
            id: "f".into(),
            input: "i".into(),
            dependencies: vec![],
            output: Some("bad".into()),
            state: NodeTerminal::Failed,
        });
        let view = MissionView::from_persisted(&persisted);
        assert_eq!(view.nodes["p"].view, NodeView::Blocked);
        assert!(view.nodes["p"].resumable);
        assert_eq!(view.nodes["f"].view, NodeView::Failed);
        assert!(!view.nodes["f"].resumable);
    }

    #[test]
    fn cli_status_line_and_resumable_missions_report_resumable_state_honestly() {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let ws = std::env::temp_dir().join(format!(
            "susi-mission-resume-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut m = PersistedMission::new("m-cli");
        m.upsert_node(PersistedNode {
            id: "n1".into(),
            input: "work".into(),
            dependencies: vec![],
            output: None,
            state: NodeTerminal::Pending,
        });
        m.save(&PersistedMission::missions_dir(&ws)).unwrap();

        let listed = resumable_missions(&ws);
        assert_eq!(listed.len(), 1);
        let line = cli_status_line(&listed[0]);
        assert!(line.contains("mission m-cli resumable (partial); resume nodes: n1"));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
