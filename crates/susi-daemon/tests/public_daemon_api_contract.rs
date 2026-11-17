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
    assert_eq!(CloudKind::parse("kubectl").ok(), Some(CloudKind::Kubernetes));
    assert_eq!(CloudKind::parse("google").map(|k| k.as_str()).ok(), Some("gcp"));
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
