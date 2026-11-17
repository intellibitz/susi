//! Ambient start and pulse forward to `susi-plug-host`.
//!
//! `susi ambient start` / MCP `ambient_start` use `daemon.ambient.start`.
//! `susi ambient pulse` uses `daemon.ambient.pulse`.

use serde_json::{Value, json};
use std::path::Path;
use susi_core::plane_bus::{PlaneBus, daemon, topics};

use crate::plug_forward;

/// Start ambient indexing for `workspace` (idempotent in the plug host).
pub fn start_ambient_indexer(workspace: &Path) {
    let _ = start_report(workspace);
}

/// `susi ambient start` / MCP `ambient_start`: start the indexer and report
/// what happened.
pub fn start_report(workspace: &Path) -> Value {
    plug_forward::call(
        daemon::ambient_start(workspace),
        topics::DAEMON_AMBIENT_START,
    )
    .unwrap_or_else(|e| json!({ "error": e }))
}

const TOPIC_AMBIENT_PULSE: &str = "daemon.ambient.pulse";

/// One-shot ambient pulse (also used by CLI).
pub fn pulse(workspace: &Path) -> Value {
    let response = PlaneBus::global().request(
        TOPIC_AMBIENT_PULSE,
        json!({ "workspace": workspace.display().to_string() }),
    );
    let result = match response {
        Ok(value) => {
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                Err(error.to_owned())
            } else {
                Ok(value)
            }
        }
        Err(error) => Err(error),
    };
    plug_forward::call(result, TOPIC_AMBIENT_PULSE).unwrap_or_else(|e| json!({ "error": e }))
}
