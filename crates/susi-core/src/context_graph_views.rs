//! Documents shared by native graph frontends.

use crate::context_graph::{ContextGraph, NodeId};
use serde_json::{json, Value};
use std::path::Path;
use susi_error::EaiResult;

impl ContextGraph {
    /// Access the current graph after its best-effort incremental replay.
    pub fn replayed_global() -> &'static Self {
        let graph = Self::global();
        let _ = graph.replay();
        graph
    }

    pub fn workspace_event_report(&self, workspace: &Path) -> Value {
        let events = self.workspace_events(workspace);
        json!({
            "workspace": workspace.display().to_string(),
            "event_count": events.len(),
            "events": events,
        })
    }

    /// Record one external event with the workspace selected by its caller.
    pub fn record_external_event(&self, event: &Value, workspace: Option<&Path>) -> NodeId {
        self.record_external_context(
            event["source"].as_str().unwrap_or("unknown"),
            event["label"].as_str().unwrap_or("external context"),
            &event["payload"],
            workspace,
            event["user"].as_str(),
        )
    }

    pub fn ingest_report(&self, event: &Value, default_workspace: &Path) -> Value {
        let workspace = event["workspace"]
            .as_str()
            .map(Path::new)
            .unwrap_or(default_workspace);
        let id = self.record_external_event(event, Some(workspace));
        json!({"ingested": true, "node_id": id.0})
    }

    pub fn compact_report(&self) -> EaiResult<Value> {
        let (old_lines, new_lines) = self.compact()?;
        Ok(json!({"compacted": true, "old_lines": old_lines, "new_lines": new_lines}))
    }
}
