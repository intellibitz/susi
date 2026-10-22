//! The mission-control view of an agent graph.
//!
//! The graph writes every handoff to its bus as JSON lines; this module turns
//! that file into what a cockpit renders: the handoffs in order, the roles that
//! produced them, and the reviewer's verdicts. It is read-only and recomputed
//! on every call, so the cockpit never becomes a second source of truth.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// One handoff read from the bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphMessage {
    /// Sending role.
    pub from: String,
    /// Receiving role.
    pub to: String,
    /// plan, work, review, approval or rejection.
    pub kind: String,
    /// Payload as JSON text.
    pub payload: String,
}

/// The graph as a mission-control surface sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GraphView {
    /// Handoffs, in the order they happened.
    pub messages: Vec<GraphMessage>,
    /// Roles that produced a handoff, ordered by name.
    pub roles: Vec<String>,
    /// Approvals the reviewer recorded.
    pub approvals: usize,
    /// Rejections the reviewer recorded.
    pub rejections: usize,
    /// The reviewer's latest verdict (`approval` or `rejection`).
    pub verdict: Option<String>,
}

impl GraphView {
    /// Read a bus file, skipping any line that is not a handoff.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut view = Self::default();
        let mut roles = BTreeSet::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let from = value
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let to = value
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let kind = value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match kind.as_str() {
                "approval" => {
                    view.approvals += 1;
                    view.verdict = Some(kind.clone());
                }
                "rejection" => {
                    view.rejections += 1;
                    view.verdict = Some(kind.clone());
                }
                _ => {}
            }
            roles.insert(from.clone());
            view.messages.push(GraphMessage {
                from,
                to,
                kind,
                payload: value
                    .get("payload")
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
        }
        view.roles = roles.into_iter().collect();
        view
    }

    /// Whether the bus held no handoffs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Whether the reviewer's latest verdict was acceptance.
    #[must_use]
    pub fn approved(&self) -> bool {
        self.verdict.as_deref() == Some("approval")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bus(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("graph.jsonl");
        std::fs::write(
            &path,
            "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{\"summary\":\"p\"}}\n\
             {\"from\":\"coder-a\",\"to\":\"reviewer\",\"kind\":\"work\",\"payload\":{}}\n\
             not json\n\
             {\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"rejection\",\"payload\":{\"summary\":\"no\"}}\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn mission_control_reads_handoffs_and_counts_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_bus(dir.path());
        let view = GraphView::load(&path);
        assert_eq!(view.messages.len(), 3);
        assert_eq!(view.roles, vec!["coder-a", "planner", "reviewer"]);
        assert_eq!(view.approvals, 0);
        assert_eq!(view.rejections, 1);
        assert_eq!(view.verdict.as_deref(), Some("rejection"));
        assert!(!view.approved());
        assert_eq!(view.messages[0].kind, "plan");
        assert_eq!(view.messages[0].payload, "{\"summary\":\"p\"}");
    }

    #[test]
    fn mission_control_missing_bus_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let view = GraphView::load(&dir.path().join("absent.jsonl"));
        assert!(view.is_empty());
        assert!(view.roles.is_empty());
        assert!(view.verdict.is_none());
    }

    #[test]
    fn mission_control_approval_sets_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.jsonl");
        std::fs::write(
            &path,
            "{\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"approval\",\"payload\":{}}\n",
        )
        .unwrap();
        let view = GraphView::load(&path);
        assert_eq!(view.approvals, 1);
        assert!(view.approved());
        assert_eq!(view.roles, vec!["reviewer"]);
    }
}
