//! Records host telemetry into the context graph. Sampling itself is the
//! shared `susi_core::telemetry::sample` (re-exported here for callers).

use std::path::Path;
use susi_core::context_graph::ContextGraph;
use susi_core::telemetry::TelemetrySnapshot;
pub use susi_core::telemetry::sample;

/// Sample host telemetry and optionally record it into the context graph.
pub fn sample_and_record(workspace: Option<&Path>) -> TelemetrySnapshot {
    let snapshot = sample();
    if !snapshot.thermal_zones.is_empty()
        || !snapshot.batteries.is_empty()
        || snapshot.load_avg_1m.is_some()
    {
        ContextGraph::global().record_telemetry(&snapshot, workspace);
    }
    snapshot
}
