//! Mastery verification for VC-201-091: bounded, redacted observability
//! carrying mission/task/placement/experiment/deployment ids end to end
//! without recording credentials.

use crate::otel_export::{
    export_spans, export_spans_with_limit, mission_span, ExportOutcome, OtelExportTarget,
    OtelKeyValue, OtelStatusCode,
};
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

/// Verification: credentials (Authorization bearer tokens, api keys, tokens, secrets)
/// are automatically redacted in exported span attributes rather than written verbatim.
#[test]
fn vc_201_091_mastery_credentials_are_redacted() {
    let dir = scratch("cred");
    let path = dir.join("spans.jsonl");
    let mut s = mission_span("trace1", "m-1", "deploy");
    s.attributes.push(OtelKeyValue {
        key: "http.request.header.authorization".into(),
        value: serde_json::Value::String("Bearer sk-live-SECRET".into()),
    });
    s.attributes.push(OtelKeyValue {
        key: "openai.api_key".into(),
        value: serde_json::Value::String("sk-abcdef123456789".into()),
    });
    export_spans(&OtelExportTarget::File { path: path.clone() }, &[s]).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        !body.contains("sk-live-SECRET"),
        "credential must not be exported verbatim: got {body}"
    );
    assert!(
        !body.contains("sk-abcdef123456789"),
        "api key must not be exported verbatim: got {body}"
    );
    assert!(
        body.contains("[REDACTED]"),
        "expected redacted token marker: got {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Verification: Http export actually transmits via HTTP transport when egress permits,
/// returning real HTTP status (e.g. Posted or PostFailed) rather than a labeled noop (WouldPost).
#[test]
fn vc_201_091_mastery_http_export_actually_transmits() {
    let s = mission_span("t", "m", "g");
    let out = export_spans(
        &OtelExportTarget::Http {
            // Egress to non-existent port on localhost fails at connect time, proving network transmission attempt
            endpoint: "http://127.0.0.1:49999/v1/traces".into(),
        },
        &[s],
    )
    .unwrap();
    // Proves transmission is attempted: it returns Posted or PostFailed, never a mock WouldPost
    assert!(
        matches!(
            out,
            ExportOutcome::Posted { .. } | ExportOutcome::PostFailed { .. }
        ),
        "HTTP export must attempt real transmission, got: {out:?}"
    );
}

/// Verification: OtelSpan includes typed status_code and status_message fields,
/// allowing failed distributed requests and errors to be represented and followed.
#[test]
fn vc_201_091_mastery_failure_can_be_recorded() {
    let mut failed = mission_span("t", "m", "g");
    failed.record_error("connection refused to upstream peer");
    assert_eq!(failed.status_code, OtelStatusCode::Error);
    assert_eq!(
        failed.status_message.as_deref(),
        Some("connection refused to upstream peer")
    );

    let ser = serde_json::to_string(&failed).unwrap();
    assert!(
        ser.contains("\"status_code\":\"error\""),
        "OTLP status_code field must be present in serialization: {ser}"
    );
    assert!(
        ser.contains("\"status_message\":\"connection refused to upstream peer\""),
        "status_message field must be present: {ser}"
    );
}

/// Verification: File export is bounded and rotates when reaching capacity.
#[test]
fn vc_201_091_mastery_file_export_is_bounded_and_rotates() {
    let dir = scratch("rot");
    let path = dir.join("spans.jsonl");
    // Write 5 spans with a low rotation threshold (500 bytes)
    for i in 0..5 {
        let s = mission_span(&format!("t{i}"), "m", "goal");
        export_spans_with_limit(&OtelExportTarget::File { path: path.clone() }, &[s], 500).unwrap();
    }
    // Verify rotated generation `.1` exists
    let rotated = dir.join("spans.jsonl.1");
    assert!(
        rotated.exists(),
        "rotated generation spans.jsonl.1 must exist"
    );
    let meta = std::fs::metadata(&path).unwrap();
    assert!(
        meta.len() > 0,
        "active spans.jsonl must exist and be bounded"
    );
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
