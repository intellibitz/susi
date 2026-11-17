//! OS-level context adapters for the Universal Context Graph.
//!
//! Sampling is served by `susi-plug-host` on `daemon.context.sample_os`.
//! Public Rust facades keep the original signatures and forward over the
//! plane bus.

use std::path::Path;
use susi_core::plane_bus::{daemon, topics};

fn absent() -> String {
    format!(
        "`{}` is served by Susi Commercial Apps: install it and run `susi-plug-host` \
         (or start the commercial plug host so the plane bus can reach it)",
        topics::DAEMON_CONTEXT_SAMPLE_OS
    )
}

fn call_plug(result: Result<serde_json::Value, String>) -> Result<serde_json::Value, String> {
    match result {
        Err(e) if e.contains("no handler for topic") || e.contains("unhandled topic") => {
            Err(absent())
        }
        other => other,
    }
}

fn workspace_or_dot(workspace: Option<&Path>) -> &Path {
    workspace.unwrap_or_else(|| Path::new("."))
}

/// Sample one round of OS context and record it into the global context graph.
/// Returns how many context records were written.
pub fn sample_os_context_once(workspace: Option<&Path>, user: Option<&str>) -> usize {
    match sample_os_report(workspace, user).get("sampled") {
        Some(value) => value.as_u64().unwrap_or(0) as usize,
        None => 0,
    }
}

/// `susi context-graph sample-os` / MCP `context_graph_sample_os`: sample
/// once and report how many records were written, the OS, and the sources
/// this OS cannot sample (with the reason).
pub fn sample_os_report(workspace: Option<&Path>, user: Option<&str>) -> serde_json::Value {
    match call_plug(daemon::context_sample_os(workspace_or_dot(workspace), user)) {
        Ok(value) => value,
        Err(error) => serde_json::json!({
            "error": error,
            "sampled": 0,
            "os": std::env::consts::OS,
            "unavailable": unavailable_os_context()
                .into_iter()
                .map(|(source, reason)| (source.to_string(), serde_json::Value::from(reason)))
                .collect::<serde_json::Map<_, _>>(),
        }),
    }
}

/// OS context adapters this platform cannot run, with the reason ? so a
/// sample that records fewer sources says why instead of looking empty.
pub fn unavailable_os_context() -> Vec<(&'static str, String)> {
    if cfg!(target_os = "linux") {
        Vec::new()
    } else {
        vec![(
            "active_window",
            format!(
                "active-window sampling is Linux-only (xdotool); not sampled on {}",
                std::env::consts::OS
            ),
        )]
    }
}
