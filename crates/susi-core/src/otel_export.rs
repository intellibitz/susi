//! OpenTelemetry-compatible mission span export (VC-201-091).
//!
//! Emits OTLP/JSON spans for missions, routing, and provider calls when an
//! export target is configured. Network export is gated by
//! [`crate::mac_policy::egress_permitted`]; otherwise spans buffer to a
//! bounded local file only. Redacts credentials before export, supports
//! typed OTLP status/error fields, and bounds file export with rotation.

use crate::mac_policy;
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum bytes for an export file before rotating to `.1`.
pub const MAX_EXPORT_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum number of rotated generations kept (`path.1` .. `path.N`).
pub const KEEP_GENERATIONS: u32 = 4;

/// Where spans go when export is enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OtelExportTarget {
    /// Append OTLP/JSON resource spans to this file (always allowed, rotated when large).
    File { path: PathBuf },
    /// POST OTLP/JSON to this URL — only when egress is permitted.
    Http { endpoint: String },
}

/// OTLP Span status code representing the outcome of a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OtelStatusCode {
    #[default]
    Unset,
    Ok,
    Error,
}

/// One span in an OTLP/JSON shape carrying trace linkage, status, and attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OtelSpan {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    #[serde(default)]
    pub status_code: OtelStatusCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub attributes: Vec<OtelKeyValue>,
}

impl OtelSpan {
    /// Mark this span as failed with an error message.
    pub fn record_error(&mut self, message: impl Into<String>) {
        self.status_code = OtelStatusCode::Error;
        self.status_message = Some(message.into());
    }

    /// Mark this span as successfully completed.
    pub fn record_ok(&mut self) {
        self.status_code = OtelStatusCode::Ok;
        self.status_message = None;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OtelKeyValue {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OtelResourceSpans {
    resource_spans: Vec<OtelResourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OtelResourceSpan {
    scope_spans: Vec<OtelScopeSpans>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OtelScopeSpans {
    spans: Vec<OtelSpan>,
}

fn now_nano() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn hex_id(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Mint a fresh 16-byte trace id / 8-byte span id.
#[must_use]
pub fn new_trace_id() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    hex_id(&b)
}

#[must_use]
pub fn new_span_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    hex_id(&b)
}

/// Build a mission-root span.
#[must_use]
pub fn mission_span(trace_id: &str, mission_id: &str, goal_summary: &str) -> OtelSpan {
    let start = now_nano();
    OtelSpan {
        name: "susi.mission".to_string(),
        trace_id: trace_id.to_string(),
        span_id: new_span_id(),
        parent_span_id: None,
        start_time_unix_nano: start,
        end_time_unix_nano: start,
        status_code: OtelStatusCode::Unset,
        status_message: None,
        attributes: vec![
            OtelKeyValue {
                key: "susi.mission_id".into(),
                value: serde_json::Value::String(mission_id.into()),
            },
            OtelKeyValue {
                key: "susi.goal".into(),
                value: serde_json::Value::String(goal_summary.into()),
            },
        ],
    }
}

/// Child span for a routing decision.
#[must_use]
pub fn routing_span(trace_id: &str, parent: &str, engine: &str, reason: &str) -> OtelSpan {
    let start = now_nano();
    OtelSpan {
        name: "susi.routing".to_string(),
        trace_id: trace_id.to_string(),
        span_id: new_span_id(),
        parent_span_id: Some(parent.to_string()),
        start_time_unix_nano: start,
        end_time_unix_nano: start,
        status_code: OtelStatusCode::Unset,
        status_message: None,
        attributes: vec![
            OtelKeyValue {
                key: "susi.engine".into(),
                value: serde_json::Value::String(engine.into()),
            },
            OtelKeyValue {
                key: "susi.reason".into(),
                value: serde_json::Value::String(reason.into()),
            },
        ],
    }
}

/// Child span for a provider call.
#[must_use]
pub fn provider_span(trace_id: &str, parent: &str, provider: &str, model: &str) -> OtelSpan {
    let start = now_nano();
    OtelSpan {
        name: "susi.provider".to_string(),
        trace_id: trace_id.to_string(),
        span_id: new_span_id(),
        parent_span_id: Some(parent.to_string()),
        start_time_unix_nano: start,
        end_time_unix_nano: start,
        status_code: OtelStatusCode::Unset,
        status_message: None,
        attributes: vec![
            OtelKeyValue {
                key: "susi.provider".into(),
                value: serde_json::Value::String(provider.into()),
            },
            OtelKeyValue {
                key: "susi.model".into(),
                value: serde_json::Value::String(model.into()),
            },
        ],
    }
}

/// Redact credentials from a string attribute or payload using standard config rules
/// plus key name inspection.
fn redact_attr_value(key: &str, value: &serde_json::Value) -> serde_json::Value {
    let key_lower = key.to_ascii_lowercase();
    let is_cred_key = key_lower.contains("authorization")
        || key_lower.contains("api_key")
        || key_lower.contains("apikey")
        || key_lower.contains("token")
        || key_lower.contains("secret")
        || key_lower.contains("password")
        || key_lower.contains("credential");

    match value {
        serde_json::Value::String(s) => {
            if is_cred_key {
                if s.to_ascii_lowercase().starts_with("bearer ") {
                    serde_json::Value::String("Bearer [REDACTED]".to_string())
                } else {
                    serde_json::Value::String("[REDACTED]".to_string())
                }
            } else {
                serde_json::Value::String(susi_config::redact_credentials(s))
            }
        }
        serde_json::Value::Array(arr) => {
            let redacted = arr
                .iter()
                .map(|item| redact_attr_value(key, item))
                .collect();
            serde_json::Value::Array(redacted)
        }
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), redact_attr_value(k, v));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            if is_cred_key {
                serde_json::Value::String("[REDACTED]".to_string())
            } else {
                value.clone()
            }
        }
    }
}

/// Redact credentials across all attributes in a span.
fn redact_span(span: &OtelSpan) -> OtelSpan {
    let mut scrubbed = span.clone();
    for attr in &mut scrubbed.attributes {
        attr.value = redact_attr_value(&attr.key, &attr.value);
    }
    if let Some(msg) = &scrubbed.status_message {
        scrubbed.status_message = Some(susi_config::redact_credentials(msg));
    }
    scrubbed
}

fn encode_batch(spans: &[OtelSpan]) -> EaiResult<String> {
    let redacted_spans: Vec<OtelSpan> = spans.iter().map(redact_span).collect();
    let doc = OtelResourceSpans {
        resource_spans: vec![OtelResourceSpan {
            scope_spans: vec![OtelScopeSpans {
                spans: redacted_spans,
            }],
        }],
    };
    serde_json::to_string(&doc).map_err(|e| EaiError::config(e.to_string()))
}

/// Export spans to the configured target. HTTP is skipped (and reported)
/// when egress is not permitted — file export always proceeds.
pub fn export_spans(target: &OtelExportTarget, spans: &[OtelSpan]) -> EaiResult<ExportOutcome> {
    export_spans_with_limit(target, spans, MAX_EXPORT_BYTES)
}

/// Export spans with an explicit file size threshold for rotation testing.
pub fn export_spans_with_limit(
    target: &OtelExportTarget,
    spans: &[OtelSpan],
    max_bytes: u64,
) -> EaiResult<ExportOutcome> {
    if spans.is_empty() {
        return Ok(ExportOutcome::Empty);
    }
    let body = encode_batch(spans)?;
    match target {
        OtelExportTarget::File { path } => {
            append_file_bounded(path, &body, max_bytes)?;
            Ok(ExportOutcome::WroteFile {
                path: path.clone(),
                spans: spans.len(),
            })
        }
        OtelExportTarget::Http { endpoint } => {
            if !mac_policy::egress_permitted(endpoint) {
                return Ok(ExportOutcome::EgressBlocked {
                    endpoint: endpoint.clone(),
                });
            }
            // Transmit via shared http transport
            let headers = [("content-type", "application/json")];
            match susi_http_transport::http_call_with_body(
                "POST",
                endpoint,
                &headers,
                Some(body.as_bytes()),
                10,
                2,
            ) {
                Ok(call) => Ok(ExportOutcome::Posted {
                    endpoint: endpoint.clone(),
                    status: call.status,
                    spans: spans.len(),
                }),
                Err(err) => Ok(ExportOutcome::PostFailed {
                    endpoint: endpoint.clone(),
                    error: err,
                    spans: spans.len(),
                }),
            }
        }
    }
}

/// Result of an export attempt (honest about egress blocks and network outcome).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportOutcome {
    Empty,
    WroteFile {
        path: PathBuf,
        spans: usize,
    },
    Posted {
        endpoint: String,
        status: u16,
        spans: usize,
    },
    PostFailed {
        endpoint: String,
        error: String,
        spans: usize,
    },
    EgressBlocked {
        endpoint: String,
    },
}

fn rotate_if_large(path: &Path, max_bytes: u64) {
    let oversized = fs::metadata(path)
        .map(|m| m.len() > max_bytes)
        .unwrap_or(false);
    if !oversized {
        return;
    }
    let oldest = path.with_file_name(format!(
        "{}.{KEEP_GENERATIONS}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    ));
    let _ = fs::remove_file(&oldest);
    for g in (1..KEEP_GENERATIONS).rev() {
        let from = path.with_file_name(format!(
            "{}.{g}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
        ));
        if from.exists() {
            let to = path.with_file_name(format!(
                "{}.{}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default(),
                g + 1
            ));
            let _ = fs::rename(&from, &to);
        }
    }
    let first = path.with_file_name(format!(
        "{}.1",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    ));
    let _ = fs::rename(path, &first);
}

fn append_file_bounded(path: &Path, line: &str, max_bytes: u64) -> EaiResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
    }
    rotate_if_large(path, max_bytes);
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| EaiError::io(e.to_string()))?;
    writeln!(f, "{line}").map_err(|e| EaiError::io(e.to_string()))?;
    Ok(())
}
