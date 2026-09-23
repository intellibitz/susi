//! Standalone `susi-error` REST service.
//!
//! Accepts error events from decoupled crates over HTTP and appends them to
//! the shared `error_metrics.jsonl` sink — the IPC replacement for calling
//! `EaiError::log_to_metrics` through a source dependency.
//! Default bind: `127.0.0.1:18081` (override with `SUSI_ERROR_PORT`).

use axum::{http::StatusCode, routing::post, Json, Router};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const DEFAULT_PORT: u16 = 18081;

#[derive(Deserialize)]
struct ErrorEvent {
    variant: Option<String>,
    message: Option<String>,
    kind: Option<String>,
    code: Option<String>,
    retryable: Option<bool>,
    ts: Option<u64>,
    error: Option<String>,
}

async fn log_error(Json(event): Json<ErrorEvent>) -> StatusCode {
    let ts = event.ts.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    let kind = event
        .kind
        .or(event.variant)
        .unwrap_or_else(|| "External".to_string());
    let message = event.error.or(event.message).unwrap_or_default();
    let code = event
        .code
        .unwrap_or_else(|| format!("susi.{}", kind.to_lowercase()));

    let entry = serde_json::json!({
        "ts": ts,
        "error": message,
        "kind": kind,
        "code": code,
        "retryable": event.retryable.unwrap_or(false),
    });

    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(susi_error::error_metrics_path())
    else {
        return StatusCode::INTERNAL_SERVER_ERROR;
    };
    use std::io::Write;
    match writeln!(f, "{entry}") {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new().route("/log_error", post(log_error));

    let port = std::env::var("SUSI_ERROR_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("susi-error service listening on {addr}");
    axum::serve(listener, app).await
}
