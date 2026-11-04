//! Records host telemetry into the context graph. Sampling itself is the
//! shared `susi_core::telemetry::sample` (re-exported here for callers).

use std::path::Path;
use susi_core::context_graph::ContextGraph;
use susi_core::telemetry::TelemetrySnapshot;
pub use susi_core::telemetry::sample;

/// Sample host telemetry and optionally record it into the context graph.
pub fn sample_and_record(workspace: Option<&Path>) -> TelemetrySnapshot {
    let snapshot = sample();
    if snapshot.has_readings() {
        ContextGraph::global().record_telemetry(&snapshot, workspace);
    }
    snapshot
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn sample_returns_telemetry_snapshot_with_valid_ranges() {
        let snap = sample();
        if let Some(cpu) = snap.cpu_util_pct {
            assert!((0.0..=100.0).contains(&cpu));
        }
    }

    #[test]
    fn sample_and_record_without_workspace_succeeds() {
        let snap = sample_and_record(None);
        let _ = snap.has_readings();
    }

    #[test]
    fn sample_and_record_with_workspace_succeeds() {
        let ws = std::env::temp_dir().join("susi-telemetry-inline-ws");
        let snap = sample_and_record(Some(&ws));
        let _ = snap.has_readings();
    }

    #[test]
    fn empty_telemetry_snapshot_reports_no_readings() {
        let empty = TelemetrySnapshot::empty();
        assert!(!empty.has_readings());
    }

    #[test]
    fn sample_os_field_matches_std_env_consts_os() {
        let snap = sample();
        assert_eq!(snap.os, std::env::consts::OS);
    }
}
