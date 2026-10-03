//! Mastery verification for VC-201-091: bounded, redacted observability
//! carrying mission/task/placement/experiment/deployment ids end to end
//! without recording credentials.

use crate::otel_export::{export_spans, mission_span, OtelExportTarget, OtelKeyValue, OtelSpan};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-otel-m-{tag}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Falsification: 'without recording credentials' — there is no
/// redaction. A span attribute carrying an Authorization bearer token
/// (or an api_key) is written to the export file verbatim.
#[test]
fn vc_201_091_mastery_credentials_exported_verbatim() {
    let dir = scratch("cred");
    let path = dir.join("spans.jsonl");
    let mut s = mission_span("trace1", "m-1", "deploy");
    s.attributes.push(OtelKeyValue {
        key: "http.request.header.authorization".into(),
        value: serde_json::Value::String("Bearer sk-live-SECRET".into()),
    });
    export_spans(&OtelExportTarget::File { path: path.clone() }, &[s]).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("sk-live-SECRET"),
        "a credential rode the export verbatim"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Falsification: Http export never posts. A permitted endpoint returns
/// WouldPost — 'record intent without a live collector' — so spans
/// claimed to reach a collector are only counted. Correlation 'end to
/// end' ends at the local process.
#[test]
fn vc_201_091_mastery_http_export_is_a_labeled_noop() {
    use crate::otel_export::ExportOutcome;
    let s = mission_span("t", "m", "g");
    let out = export_spans(
        &OtelExportTarget::Http {
            endpoint: "http://localhost:4318/v1/traces".into(),
        },
        &[s],
    )
    .unwrap();
    // Even where egress permits, nothing is transmitted.
    assert!(matches!(
        out,
        ExportOutcome::WouldPost { .. } | ExportOutcome::EgressBlocked { .. }
    ));
}

/// Falsification: 'one failed distributed request' cannot be
/// represented — OtelSpan has no status or error field. A failed span
/// is indistinguishable from a successful one, so the failure cannot be
/// 'followed' at all.
#[test]
fn vc_201_091_mastery_no_failure_can_be_recorded() {
    let ok = mission_span("t", "m", "g");
    let mut failed = mission_span("t", "m", "g");
    failed.attributes.push(OtelKeyValue {
        key: "status".into(),
        value: serde_json::Value::String("error".into()),
    });
    // 'status' is just another attribute — nothing typed; a trace
    // backend sees no failure semantic.
    let ser = serde_json::to_string(&failed).unwrap();
    assert!(
        !ser.contains("\"status_code\""),
        "no OTLP status field exists"
    );
    assert!(ser.contains("susi.mission"));
}

/// Falsification: 'bounded' — export appends to a file forever; no size
/// cap, no rotation, no span cap. File growth is unbounded.
#[test]
fn vc_201_091_mastery_file_export_is_unbounded() {
    let dir = scratch("unb");
    let path = dir.join("spans.jsonl");
    for _ in 0..3 {
        let s = mission_span("t", "m", "g");
        export_spans(&OtelExportTarget::File { path: path.clone() }, &[s]).unwrap();
    }
    let meta = std::fs::metadata(&path).unwrap();
    assert!(meta.len() > 0);
    // No rotation or cap exists: 3 writes = 3 appended lines, forever.
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body.lines().count(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What holds: parent/child span linkage propagates a trace id through
/// mission → routing → provider, file export writes them, and an empty
/// batch is a no-op.
#[test]
fn vc_201_091_mastery_trace_linkage_holds() {
    use crate::otel_export::{new_trace_id, provider_span, routing_span};
    let dir = scratch("link");
    let path = dir.join("spans.jsonl");
    let trace = new_trace_id();
    let mission = mission_span(&trace, "m-1", "list files");
    let routing = routing_span(&trace, &mission.span_id, "eng", "local");
    let provider = provider_span(&trace, &routing.span_id, "prov", "mod");
    export_spans(
        &OtelExportTarget::File { path: path.clone() },
        &[mission, routing, provider],
    )
    .unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(body.matches(&trace).count(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}
