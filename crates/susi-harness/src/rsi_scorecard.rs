//! Scorecard contracts delegated through the authorized MCP socket.
use crate::git_governor::{caller_wire_path, wire_path, workflow_call, workflow_error_value};
use crate::lifecycle::DEFAULT_LIFECYCLE_PATH;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use susi_error::EaiResult;

/// Score one task; provider failures are explicit typed error objects.
pub fn score_task(t: &Value, evs: &[Value], imp: Option<u64>, att: Option<&Value>) -> Value {
    workflow_call(
        "score_task",
        &json!({
            "task":t, "events":evs, "implemented":imp, "attestation":att,
        }),
    )
    .unwrap_or_else(workflow_error_value)
}

/// Score selected tasks with optional events, defaulting to `.susi/records/rsi-loop.jsonl`.
/// Provider failures are explicit typed error objects, never fabricated scores.
pub fn score(root: &Path, sel: Option<&BTreeSet<String>>, ev_path: Option<&Path>) -> Value {
    let default_events = root.join(DEFAULT_LIFECYCLE_PATH);
    let call = || -> EaiResult<Value> {
        workflow_call(
            "score",
            &json!({
                "repo":caller_wire_path(root)?, "tasks":sel,
                "events":caller_wire_path(ev_path.unwrap_or(&default_events))?,
            }),
        )
    };
    call().unwrap_or_else(workflow_error_value)
}

/// Render a scorecard, preserving the provider's typed errors and completion checks.
pub fn run(
    root: &Path,
    ev: &Path,
    tasks: &[String],
    json: bool,
    req_done: bool,
) -> EaiResult<String> {
    workflow_call(
        "scorecard",
        &json!({
            "repo":caller_wire_path(root)?, "events":wire_path(ev)?, "tasks":tasks,
            "json":json, "require_complete":req_done,
        }),
    )
}
