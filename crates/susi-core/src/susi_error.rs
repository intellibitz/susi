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

/// The metrics sink is append-only and error paths are hot — without a
/// cap the file grows without bound (178MB observed). Past the cap the
/// file rotates one generation (`error_metrics.jsonl.1`); a racing
/// writer may lose a line across the rename, which a metrics sink
/// tolerates — bounded beats unbounded.
const METRICS_CAP_BYTES: u64 = 64 * 1024 * 1024;

fn open_metrics_append() -> Option<std::fs::File> {
    let path = metrics_path();
    if std::fs::metadata(&path)
        .map(|m| m.len() > METRICS_CAP_BYTES)
        .unwrap_or(false)
    {
        let rotated = path.with_file_name("error_metrics.jsonl.1");
        let _ = std::fs::rename(&path, rotated);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Local sink used when the `susi-error` service is unreachable.
fn append_local(entry: &serde_json::Value) {
    if let Some(mut f) = open_metrics_append() {
        let _ = writeln!(f, "{entry}");
    }
}

/// Inline `~/.susi`/XDG data-dir fallback (same rule as `susi-paths`'
/// `SusiDirs`) so this vendored file stays self-contained.
fn metrics_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let legacy = home.join(".susi");
    let use_xdg = if legacy.is_dir() {
        std::env::var("SUSI_XDG")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
    } else {
        true
    };
    let data_dir = if use_xdg {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"))
            .join("susi")
    } else {
        legacy
    };
    data_dir.join("error_metrics.jsonl")
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
