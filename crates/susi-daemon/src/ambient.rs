//! Ambient start and pulse forward to `susi-plug-host`.
//!
//! `susi ambient start` / MCP `ambient_start` use `daemon.ambient.start`.
//! `susi ambient pulse` uses `daemon.ambient.pulse`. Public Rust facades
//! keep the original types and signatures.

use serde_json::{Value, json};
use std::path::Path;
use susi_core::plane_bus::{PlaneBus, daemon, topics};

use crate::plug_forward;

/// What one start request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbientStart {
    /// The supervised indexer was spawned for this workspace.
    Started,
    /// An indexer already runs in this process (one per process).
    AlreadyRunning,
    /// The workspace is a SUSI-internal root (substrate, config or data
    /// dir); indexing it would observe SUSI's own writes.
    InternalPath,
}

impl AmbientStart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::AlreadyRunning => "already_running",
            Self::InternalPath => "refused_internal_path",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AmbientPulseReport {
    pub files_changed: usize,
    pub docs_indexed: usize,
}

impl AmbientPulseReport {
    pub fn to_json(&self) -> Value {
        json!({
            "files_changed": self.files_changed,
            "docs_indexed": self.docs_indexed,
        })
    }
}

fn internal_path(path: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    [
        susi_paths::SusiDirs::substrate_home(),
        susi_paths::SusiDirs::config_dir(),
        susi_paths::SusiDirs::data_dir(),
    ]
    .into_iter()
    .filter_map(|root| root.canonicalize().ok())
    .any(|root| path == root)
}

fn outcome_from_value(value: &Value) -> Option<AmbientStart> {
    match value.get("ambient").and_then(Value::as_str) {
        Some("started") => Some(AmbientStart::Started),
        Some("already_running") => Some(AmbientStart::AlreadyRunning),
        Some("refused_internal_path") => Some(AmbientStart::InternalPath),
        _ => None,
    }
}

/// Start ambient indexing for `workspace` (idempotent in the plug host).
pub fn start_ambient_indexer(workspace: &Path) {
    let _ = start(workspace);
}

/// Start ambient indexing for `workspace` and say what happened.
pub fn start(workspace: &Path) -> AmbientStart {
    if internal_path(workspace) {
        return AmbientStart::InternalPath;
    }
    match plug_forward::call(
        daemon::ambient_start(workspace),
        topics::DAEMON_AMBIENT_START,
    ) {
        Ok(value) => outcome_from_value(&value).unwrap_or(AmbientStart::Started),
        // The indexer did not start in this process.
        Err(_) => AmbientStart::AlreadyRunning,
    }
}

/// `susi ambient start` / MCP `ambient_start`: start the indexer and report
/// what happened.
pub fn start_report(workspace: &Path) -> Value {
    if internal_path(workspace) {
        return json!({
            "ambient": AmbientStart::InternalPath.as_str(),
            "workspace": workspace.display().to_string(),
        });
    }
    plug_forward::call(
        daemon::ambient_start(workspace),
        topics::DAEMON_AMBIENT_START,
    )
    .unwrap_or_else(|e| json!({ "error": e }))
}

const TOPIC_AMBIENT_PULSE: &str = "daemon.ambient.pulse";

fn pulse_value(workspace: &Path) -> Value {
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

/// One-shot ambient pulse report as JSON (CLI / MCP).
pub fn pulse_report(workspace: &Path) -> Value {
    pulse_value(workspace)
}

/// One-shot ambient pulse (also used by CLI).
pub fn pulse(workspace: &Path) -> AmbientPulseReport {
    let value = pulse_value(workspace);
    AmbientPulseReport {
        files_changed: value
            .get("files_changed")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        docs_indexed: value
            .get("docs_indexed")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
    }
}
