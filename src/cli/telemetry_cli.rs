//! Sample and inspect host telemetry.
use crate::anyhow::Result;

pub use susi_cli_defs::telemetry::*;

pub fn execute(_action: Option<TelemetryCommands>) -> Result<()> {
    let snapshot = susi_daemon::telemetry::sample();
    let _ = susi_core::context_graph::ContextGraph::global().record_telemetry(&snapshot, None);
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}
