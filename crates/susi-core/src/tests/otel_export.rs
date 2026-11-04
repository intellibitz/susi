//! Tests for OTLP mission span export (`otel_export_*`).

use crate::mac_policy::PrivacyMode;
use crate::otel_export::{
    export_spans, mission_span, new_trace_id, provider_span, routing_span, ExportOutcome,
    OtelExportTarget,
};
use std::fs;
use std::path::PathBuf;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_otel_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn otel_export_writes_mission_routing_provider_spans_to_file() {
    let dir = scratch("file");
    let path = dir.join("spans.jsonl");
    let trace = new_trace_id();
    let mission = mission_span(&trace, "m-1", "list files");
    let routing = routing_span(&trace, &mission.span_id, "susi-offline", "local");
    let provider = provider_span(&trace, &routing.span_id, "local", "qwen");
    let outcome = export_spans(
        &OtelExportTarget::File { path: path.clone() },
        &[mission, routing, provider],
    )
    .unwrap();
    assert_eq!(
        outcome,
        ExportOutcome::WroteFile {
            path: path.clone(),
            spans: 3
        }
    );
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("susi.mission"));
    assert!(body.contains("susi.routing"));
    assert!(body.contains("susi.provider"));
    assert!(body.contains(&trace));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn otel_export_http_respects_egress_policy() {
    use crate::mac_policy::MacPolicy;
    let policy = MacPolicy::global();
    let prev = policy.mode();
    policy.set_mode(PrivacyMode::LocalOnly).unwrap();
    let outcome = export_spans(
        &OtelExportTarget::Http {
            endpoint: "https://otel.example.com/v1/traces".into(),
        },
        &[mission_span(&new_trace_id(), "m", "goal")],
    )
    .unwrap();
    let _ = policy.set_mode(prev);
    match outcome {
        ExportOutcome::EgressBlocked { endpoint } => {
            assert!(endpoint.contains("otel.example.com"));
        }
        other => panic!("expected EgressBlocked, got {other:?}"),
    }
}

#[test]
fn otel_export_empty_batch_is_noop() {
    let dir = scratch("empty");
    let path = dir.join("spans.jsonl");
    let outcome = export_spans(&OtelExportTarget::File { path }, &[]).unwrap();
    assert_eq!(outcome, ExportOutcome::Empty);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn otel_export_new_trace_id_and_span_builders_populate_parent_and_status_lifecycle() {
    use crate::otel_export::OtelStatusCode;

    let trace = new_trace_id();
    assert!(!trace.is_empty());
    let mut mission = mission_span(&trace, "m-9", "goal");
    assert_eq!(mission.trace_id, trace);
    assert!(mission.parent_span_id.is_none());
    assert_eq!(mission.status_code, OtelStatusCode::Unset);
    mission.record_error("transient failure");
    assert_eq!(mission.status_code, OtelStatusCode::Error);
    assert_eq!(mission.status_message.as_deref(), Some("transient failure"));
    mission.record_ok();
    assert_eq!(mission.status_code, OtelStatusCode::Ok);
    assert!(mission.status_message.is_none());

    let routing = routing_span(&trace, &mission.span_id, "susi-offline", "local");
    assert_eq!(
        routing.parent_span_id.as_deref(),
        Some(mission.span_id.as_str())
    );
    let provider = provider_span(&trace, &routing.span_id, "local", "qwen");
    assert_eq!(
        provider.parent_span_id.as_deref(),
        Some(routing.span_id.as_str())
    );
}

#[test]
fn otel_export_mission_span_attributes_include_mission_id_and_goal() {
    let trace = new_trace_id();
    let span = mission_span(&trace, "mission-42", "verify telemetry");
    assert_eq!(span.name, "susi.mission");
    assert!(span
        .attributes
        .iter()
        .any(|kv| kv.key == "susi.mission_id" && kv.value == "mission-42"));
    assert!(span
        .attributes
        .iter()
        .any(|kv| kv.key == "susi.goal" && kv.value == "verify telemetry"));
}
