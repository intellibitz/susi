//! Vendored `susi-error` contract + IPC reporter.
//!
//! Decoupled crates own their error type locally; error *events* still reach
//! the shared `error_metrics.jsonl` sink through the standalone `susi-error`
//! service (`POST 127.0.0.1:18081/log_error`, override via `SUSI_ERROR_PORT`).
//! When the service is unreachable the event is appended to the local file
//! directly so the metrics guarantee never depends on service health.
//!
//! Keep this file identical across the workspace.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

#[path = "../../susi-error/src/contract.rs"]
mod contract;
pub use contract::{EaiError, EaiResult};

/// Error-event sink for the shared contract: the `susi-error` service when
/// reachable, else the local metrics file.
fn record_event(entry: &serde_json::Value) {
    if !post_event(entry) {
        append_local(entry);
    }
}

/// Best-effort `POST /log_error` against the `susi-error` service.
fn post_event(entry: &serde_json::Value) -> bool {
    let port = std::env::var("SUSI_ERROR_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(18081);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let timeout = Duration::from_millis(200);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let body = entry.to_string();
    let req = format!(
        "POST /log_error HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).is_ok()
}

#[path = "../../susi-error/src/sink.rs"]
pub mod sink;

/// Local sink used when the `susi-error` service is unreachable. The path
/// comes from this crate's `SusiDirs`, so it honors `SUSI_HOME` instances
/// and the platform data dir exactly like the service; the old inline
/// resolver ignored both and could write a different file.
fn append_local(entry: &serde_json::Value) {
    // Best effort: nowhere left to report a failed error report.
    let _ = sink::append_metrics_line(
        &susi_paths::SusiDirs::data_dir().join("error_metrics.jsonl"),
        entry,
    );
}

#[path = "../../susi-error/src/redact.rs"]
pub mod redact;

/// Re-wraps a foreign `susi-error`-shaped error at a crate boundary,
/// preserving its kind/code so the metrics stream stays truthful.
#[must_use]
pub fn rewrap(kind_name: &str, msg: String) -> EaiError {
    // Each boundary re-wraps `e.to_string()`, which already carries the
    // kind's Display prefix — peel it (and nested repeats) so errors don't
    // render "Authorization Error: Authorization Error: …". Foreign-kind
    // prefixes are kept: they record where the error originated.
    let prefix = match kind_name {
        "Governance" => "Governance Violation: ",
        "Hardware" => "Hardware Error: ",
        "Protocol" => "Protocol Error: ",
        "Inference" => "Inference Error: ",
        "Sandbox" => "Sandbox Error: ",
        "Config" => "Configuration Error: ",
        "Io" => "I/O Error: ",
        "Network" => "Network Error: ",
        "Filesystem" => "Filesystem Error: ",
        "Process" => "Process Error: ",
        "Authentication" => "Authentication Error: ",
        "Authorization" => "Authorization Error: ",
        "Internal" => "Internal Engine Error: ",
        _ => "",
    };
    let mut msg = msg;
    while !prefix.is_empty() {
        if let Some(rest) = msg.strip_prefix(prefix) {
            msg = rest.to_string();
        } else {
            break;
        }
    }
    match kind_name {
        "Governance" => EaiError::governance(msg),
        "Hardware" => EaiError::hardware(msg),
        "Protocol" => EaiError::protocol(msg),
        "Inference" => EaiError::inference(msg),
        "Sandbox" => EaiError::sandbox(msg),
        "Io" => EaiError::io(msg),
        "Network" => EaiError::network(msg),
        "Filesystem" => EaiError::filesystem(msg),
        "Process" => EaiError::process(msg),
        "Authentication" => EaiError::authentication(msg),
        "Authorization" => EaiError::authorization(msg),
        "Internal" => EaiError::internal(msg),
        _ => EaiError::internal(msg),
    }
}
