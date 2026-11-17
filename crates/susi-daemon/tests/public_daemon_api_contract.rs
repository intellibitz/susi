//! Downstream Rust contracts for the five daemon feature groups.

use std::path::Path;
use susi_daemon::ambient::{self, AmbientPulseReport, AmbientStart};
use susi_daemon::context_adapters;
use susi_daemon::os_provision::{self, CloudInventory, CloudKind};
use susi_daemon::{privacy, service_control};

#[test]
fn reports_and_cloud_kinds_retain_their_types_and_values() {
    let report = AmbientPulseReport {
        files_changed: 3,
        docs_indexed: 2,
    };
    assert_eq!(report.to_json()["files_changed"], 3);
    assert_eq!(AmbientStart::InternalPath.as_str(), "refused_internal_path");
    assert_eq!(
        CloudKind::parse("kubectl").ok(),
        Some(CloudKind::Kubernetes)
    );
    assert_eq!(
        CloudKind::parse("google").map(|k| k.as_str()).ok(),
        Some("gcp")
    );
    assert_eq!(CloudKind::all().len(), 5);
}

#[test]
fn public_daemon_operations_keep_their_callable_signatures() {
    let _: fn(&Path) -> AmbientStart = ambient::start;
    let _: fn(&Path) -> AmbientPulseReport = ambient::pulse;
    let _: fn() -> Vec<CloudInventory> = os_provision::probe_all;
    let _: fn(CloudKind) -> Result<String, String> = os_provision::list_nodes;
    let _: fn(CloudKind, &str) -> Result<String, String> = os_provision::apply_manifest;
    let _: fn(Option<&Path>, Option<&str>) -> usize = context_adapters::sample_os_context_once;
    let _ = context_adapters::sample_os_report;
    let _ = context_adapters::unavailable_os_context;
    let _ = privacy::persist_privacy_mode;
    let _ = privacy::set_privacy_mode;
    let _ = service_control::control;
    let _ = service_control::start;
    let _ = service_control::stop;
    let _ = service_control::restart;
}

#[test]
fn fresh_workspace_reports_one_changed_file_and_starts_once() {
    let path = std::env::temp_dir().join(format!("susi-ambient-behavior-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    assert!(std::fs::create_dir_all(&path).is_ok());
    assert!(std::fs::write(path.join("fixture.txt"), "an ordinary test document").is_ok());
    let report = ambient::pulse(&path);
    let first = ambient::start(&path);
    let second = ambient::start(&path);
    let _ = std::fs::remove_dir_all(&path);
    assert_eq!(
        (report.files_changed, first, second),
        (1, AmbientStart::Started, AmbientStart::AlreadyRunning)
    );
}

#[test]
fn sample_os_context_once_still_samples_when_the_plug_host_is_absent() {
    let count = context_adapters::sample_os_context_once(None, None);
    if !cfg!(target_os = "linux") {
        assert_eq!(count, 1);
    }
    let report = context_adapters::sample_os_report(None, None);
    assert!(report.get("error").is_none(), "{report}");
    assert!(report["sampled"].as_u64().is_some());
}

#[test]
fn probe_all_does_not_treat_a_missing_plug_host_as_unavailable_inventory() {
    let rows = os_provision::probe_all();
    assert_eq!(rows.len(), 5);
    for row in &rows {
        assert!(
            !row.summary.contains("susi-plug-host")
                && !row.summary.contains("Susi Commercial Apps"),
            "{row:?}"
        );
    }
}
