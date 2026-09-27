#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! The `EaiError` contract every SUSI crate returns, plus the error-event
//! sink behind it. Error events go to the standalone `susi-error` service
//! (`POST 127.0.0.1:18081/log_error`, override via `SUSI_ERROR_PORT`); when
//! it is unreachable, or when this process *is* the service, they are
//! appended to the shared `error_metrics.jsonl` directly, so the metrics
//! guarantee never depends on service health.

pub mod redact;
#[cfg(test)]
#[path = "redact_tests.rs"]
mod redact_test_suite;

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Substrate data dir, through the same `SusiDirs` contract as every crate.
fn data_dir() -> PathBuf {
    susi_paths::SusiDirs::data_dir()
}

/// JSONL sink every error event (local and REST-reported) is appended to.
#[must_use]
pub fn error_metrics_path() -> PathBuf {
    data_dir().join("error_metrics.jsonl")
}

pub mod sink;
use sink::METRICS_CAP_BYTES;

/// Rotated metrics generation that predates the cap: a `.1` file larger
/// than `METRICS_CAP_BYTES` is residue the current rotation can never
/// produce again (178MB observed pre-cap). Surfaces like `susi os clean`
/// may reclaim it without losing anything the scheme still writes.
pub fn oversized_rotated_metrics() -> Option<(PathBuf, u64)> {
    let rotated = error_metrics_path().with_file_name("error_metrics.jsonl.1");
    let len = std::fs::metadata(&rotated).ok()?.len();
    (len > METRICS_CAP_BYTES).then_some((rotated, len))
}

/// Pre-rotation audit sink residue: tracing used to append to a single
/// `audit.log` (`rolling::never`); the daily-rotation builder now writes
/// `audit.YYYY-MM-DD.log` files. The flat file is still the Builder's
/// *fallback* sink, so it is only reclaimable when a dated file is newer
/// — proving the rotating sink is the live one and the flat file is dead.
///
/// The same path is also the HMAC-signed accountability chain of any
/// workspace rooted at `$HOME` (`<workspace>/.susi/audit.log`), so it is
/// never reported while it is, or ever was, a signed chain: a sibling
/// `audit.chain.tip` or any `entry_hash` line means deleting it would
/// destroy tamper-evident history, not reclaim tracing residue.
pub fn stale_flat_audit_log() -> Option<(PathBuf, u64)> {
    stale_flat_audit_log_in(&data_dir())
}

fn stale_flat_audit_log_in(home: &std::path::Path) -> Option<(PathBuf, u64)> {
    let flat = home.join("audit.log");
    let flat_meta = std::fs::metadata(&flat).ok()?;
    if !flat_meta.is_file() {
        return None;
    }
    if is_signed_audit_chain(&flat) {
        return None;
    }
    let flat_mtime = flat_meta.modified().ok()?;
    let dated_is_newer = std::fs::read_dir(home).ok()?.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        name.starts_with("audit.")
            && name.ends_with(".log")
            && name.len() > "audit..log".len()
            && e.metadata()
                .and_then(|m| m.modified())
                .map(|t| t > flat_mtime)
                .unwrap_or(false)
    });
    dated_is_newer.then_some((flat, flat_meta.len()))
}

/// True when `path` is (or was) an HMAC-signed audit chain rather than
/// plain tracing output: its chain tip sibling exists, or any line carries
/// an `entry_hash`. Unreadable files count as signed — refusing to reclaim
/// is the safe failure.
fn is_signed_audit_chain(path: &std::path::Path) -> bool {
    use std::io::BufRead;
    if path.with_extension("chain.tip").exists() {
        return true;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    std::io::BufReader::new(file)
        .lines()
        .any(|line| line.map_or(true, |l| l.contains("\"entry_hash\"")))
}

mod contract;
pub use contract::{EaiError, EaiResult};

/// Set by [`serve`]: the service must not post events to itself.
static SERVICE_MODE: AtomicBool = AtomicBool::new(false);

/// Error-event sink for the contract: the `susi-error` service when
/// reachable, else the local metrics file.
fn record_event(entry: &serde_json::Value) {
    if SERVICE_MODE.load(Ordering::Relaxed) || !post_event(entry) {
        // Best effort: nowhere left to report a failed error report.
        let _ = sink::append_metrics_line(&error_metrics_path(), entry);
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

/// Rebuilds an error from another boundary's rendered text, preserving its
/// kind/code so the metrics stream stays truthful.
#[must_use]
pub fn rewrap(kind_name: &str, msg: String) -> EaiError {
    // Rendered errors already carry the kind's Display prefix — peel it (and
    // nested repeats) so errors don't render "Authorization Error:
    // Authorization Error: …". Foreign-kind prefixes are kept: they record
    // where the error originated.
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
        "Config" => EaiError::config(msg),
        "Io" => EaiError::io(msg),
        "Network" => EaiError::network(msg),
        "Filesystem" => EaiError::filesystem(msg),
        "Process" => EaiError::process(msg),
        "Authentication" => EaiError::authentication(msg),
        "Authorization" => EaiError::authorization(msg),
        _ => EaiError::internal(msg),
    }
}

/// Embedded REST service mode: accepts error events over HTTP and appends
/// them to the shared `error_metrics.jsonl` sink. Shared by the standalone
/// `susi-error` binary and the root `susi` binary's `service-run` dispatch.
/// Bearer check for a dependency-free leaf service. The daemon's supervisor
/// passes the host token in `SUSI_HOST_TOKEN`; a bare instance started
/// without it (local dev, CI harness) stays open.
fn leaf_authorized(header: Option<&str>) -> bool {
    let Some(expected) = std::env::var("SUSI_HOST_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    else {
        return true;
    };
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let (a, b) = (presented.trim().as_bytes(), expected.trim().as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_bearer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if leaf_authorized(header) {
        next.run(req).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED)
    }
}

pub fn serve(port: u16) -> std::io::Result<()> {
    SERVICE_MODE.store(true, Ordering::Relaxed);
    use axum::{extract::Query, http::StatusCode, routing::get, routing::post, Json, Router};
    use serde::Deserialize;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

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
        // Posted events are masked here too: any local client can post.
        let message =
            crate::contract::redact_for_metrics(&event.error.or(event.message).unwrap_or_default());
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

        match crate::sink::append_metrics_line(&error_metrics_path(), &entry) {
            Ok(()) => StatusCode::NO_CONTENT,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    #[derive(Deserialize)]
    struct RecentQuery {
        n: Option<usize>,
    }

    /// `GET /errors/recent?n=<limit>` — the read side of the metrics
    /// sink. Bounded: never reads more than the last 512 KiB of the
    /// (capped, rotated) file, never returns more than 500 entries.
    async fn recent_errors(Query(q): Query<RecentQuery>) -> Json<serde_json::Value> {
        let limit = q.n.unwrap_or(50).min(500);
        let path = error_metrics_path();
        let entries: Vec<serde_json::Value> = std::fs::metadata(&path)
            .ok()
            .and_then(|m| {
                use std::io::{Read, Seek, SeekFrom};
                let mut f = std::fs::File::open(&path).ok()?;
                let skip = m.len().saturating_sub(512 * 1024);
                if f.seek(SeekFrom::Start(skip)).is_err() {
                    return None;
                }
                let mut buf = String::new();
                if f.read_to_string(&mut buf).is_err() {
                    return None;
                }
                // Starting mid-file leaves a partial first line — drop it.
                let mut lines: Vec<&str> = buf.lines().collect();
                if skip > 0 && !lines.is_empty() {
                    lines.remove(0);
                }
                Some(
                    lines
                        .iter()
                        .rev()
                        .take(limit)
                        .filter_map(|l| serde_json::from_str(l).ok())
                        .collect(),
                )
            })
            .unwrap_or_default();
        Json(serde_json::json!({ "entries": entries, "returned": entries.len() }))
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            // Error history can carry paths and details: token-gated.
            // log_error stays open — best-effort, write-only logging.
            let app = Router::new()
                .route(
                    "/errors/recent",
                    get(recent_errors).layer(axum::middleware::from_fn(require_bearer)),
                )
                .route("/log_error", post(log_error));
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!("susi-error service listening on {addr}");
            axum::serve(listener, app).await
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_and_retryable_are_stable() {
        let net = EaiError::network("down");
        assert_eq!(net.code(), "susi.network");
        assert!(net.retryable());
        let gov = EaiError::governance("veto");
        assert_eq!(gov.code(), "susi.governance");
        assert!(!gov.retryable());
    }

    fn audit_home(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("susi_flat_audit_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_newer_dated_log(home: &std::path::Path) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(home.join("audit.2026-09-25.log"), "{\"timestamp\":\"t\"}\n").unwrap();
    }

    #[test]
    fn tracing_residue_is_reclaimable() {
        let home = audit_home("tracing");
        std::fs::write(
            home.join("audit.log"),
            "{\"timestamp\":\"t\",\"level\":\"INFO\"}\n",
        )
        .unwrap();
        write_newer_dated_log(&home);
        assert!(stale_flat_audit_log_in(&home).is_some());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn signed_chain_is_never_reclaimable() {
        let home = audit_home("signed");
        std::fs::write(
            home.join("audit.log"),
            "{\"ts\":1,\"entry_hash\":\"ab\",\"hmac\":\"cd\"}\n",
        )
        .unwrap();
        write_newer_dated_log(&home);
        assert!(stale_flat_audit_log_in(&home).is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn chain_tip_alone_blocks_reclaim() {
        let home = audit_home("tip");
        std::fs::write(home.join("audit.log"), "{\"timestamp\":\"t\"}\n").unwrap();
        std::fs::write(home.join("audit.chain.tip"), "ab").unwrap();
        write_newer_dated_log(&home);
        assert!(stale_flat_audit_log_in(&home).is_none());
        let _ = std::fs::remove_dir_all(&home);
    }
}
