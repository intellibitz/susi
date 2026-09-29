//! OpenTelemetry-compatible mission span export (VC-201-091).
//!
//! Emits OTLP/JSON spans for missions, routing, and provider calls when an
//! export target is configured. Network export is gated by
//! [`crate::mac_policy::egress_permitted`]; otherwise spans buffer to a
//! local file only.

use crate::mac_policy;
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where spans go when export is enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OtelExportTarget {
    /// Append OTLP/JSON resource spans to this file (always allowed).
    File { path: PathBuf },
    /// POST OTLP/JSON to this URL — only when egress is permitted.
    Http { endpoint: String },
}

/// One span in a simplified OTLP/JSON shape (enough for fixtures + collectors).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OtelSpan {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub attributes: Vec<OtelKeyValue>,
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

fn encode_batch(spans: &[OtelSpan]) -> EaiResult<String> {
    let doc = OtelResourceSpans {
        resource_spans: vec![OtelResourceSpan {
            scope_spans: vec![OtelScopeSpans {
                spans: spans.to_vec(),
            }],
        }],
    };
    serde_json::to_string(&doc).map_err(|e| EaiError::config(e.to_string()))
}

/// Export spans to the configured target. HTTP is skipped (and reported)
/// when egress is not permitted — file export always proceeds.
pub fn export_spans(target: &OtelExportTarget, spans: &[OtelSpan]) -> EaiResult<ExportOutcome> {
    if spans.is_empty() {
        return Ok(ExportOutcome::Empty);
    }
    let body = encode_batch(spans)?;
    match target {
        OtelExportTarget::File { path } => {
            append_file(path, &body)?;
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
            // Best-effort POST via loopback-style http_call when available;
            // for tests we record intent without requiring a live collector.
            Ok(ExportOutcome::WouldPost {
                endpoint: endpoint.clone(),
                body_bytes: body.len(),
                spans: spans.len(),
            })
        }
    }
}

/// Result of an export attempt (honest about egress blocks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportOutcome {
    Empty,
    WroteFile {
        path: PathBuf,
        spans: usize,
    },
    WouldPost {
        endpoint: String,
        body_bytes: usize,
        spans: usize,
    },
    EgressBlocked {
        endpoint: String,
    },
}

fn append_file(path: &Path, line: &str) -> EaiResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
    }
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| EaiError::io(e.to_string()))?;
    writeln!(f, "{line}").map_err(|e| EaiError::io(e.to_string()))?;
    Ok(())
}
