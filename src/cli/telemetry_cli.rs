//! Sample and inspect host telemetry.
use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum TelemetryCommands {
    /// Sample thermal/battery/load telemetry now.
    Sample,
}

pub fn execute(_action: Option<TelemetryCommands>) -> Result<()> {
    let snapshot = susi_daemon::telemetry::sample();
    let _ = susi_core::context_graph::ContextGraph::global().record_telemetry(&snapshot, None);
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}
