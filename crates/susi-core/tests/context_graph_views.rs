use serde_json::json;
use susi_core::context_graph::{ContextGraph, NodeId};
use susi_error::EaiResult;

#[test]
fn explicit_workspace_and_default_workspace_keep_their_own_reports() -> EaiResult<()> {
    let directory = susi_paths::tempfile::tempdir()?;
    let fallback = directory.path().join("fallback");
    let explicit = directory.path().join("explicit");
    let graph = ContextGraph::with_storage(directory.path().join("context.jsonl"));
    let empty = graph.workspace_event_report(&fallback);
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["event_count"], 0);
    let entry = json!({"workspace": explicit, "payload":{"code":42}});
    let receipt = graph.ingest_report(&entry, &fallback);
    let id = receipt["node_id"].as_str().unwrap_or_default();
    let recorded = graph.node(&NodeId(id.to_owned()));
    assert_eq!(
        recorded.as_ref().map(|node| node.label.as_str()),
        Some("external context")
    );
    assert_eq!(graph.workspace_event_report(&fallback), empty);
    let populated = graph.workspace_event_report(&explicit);
    assert!(populated["events"]
        .as_array()
        .is_some_and(|events| events.iter().any(|event| event["id"] == id)));
    assert_eq!(graph.workspace_show(&explicit, 0)["event_count"], 0);
    let compact = graph.compact_report()?;
    assert_eq!(compact["compacted"], true);
    assert!(compact["new_lines"].as_u64().is_some_and(|count| count > 0));
    let reader = ContextGraph::with_storage(directory.path().join("context.jsonl"));
    reader.replay()?;
    assert_eq!(
        reader.workspace_event_report(&explicit)["event_count"],
        populated["event_count"]
    );
    Ok(())
}
