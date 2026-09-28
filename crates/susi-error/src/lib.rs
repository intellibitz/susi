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
//! sink behind it. Error events go to the `susi-error` service
//! (`POST 127.0.0.1:<service_port()>/log_error`, served by
//! `susi-leaf-services`); when
//! it is unreachable, or when this process *is* the service, they are
//! appended to the shared `error_metrics.jsonl` directly, so the metrics
//! guarantee never depends on service health.

pub mod redact;
#[cfg(test)]
#[path = "redact_tests.rs"]
mod redact_test_suite;

use std::path::PathBuf;
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
pub use contract::{EaiError, EaiResult, ResultExt};

/// Error-event sink for the contract: the `susi-error` service when
/// reachable, else the local metrics file.
fn record_event(entry: &serde_json::Value) {
    if susi_paths::is_service_mode() || !post_event(entry) {
        // Best effort: nowhere left to report a failed error report.
        let _ = sink::append_metrics_line(&error_metrics_path(), entry);
    }
}

/// Best-effort `POST /log_error` against the `susi-error` service; `true`
/// only when the service accepted the event (2xx), so a rejected or
/// unanswered post still falls back to the local metrics file.
fn post_event(entry: &serde_json::Value) -> bool {
    susi_paths::loopback::request(
        &susi_paths::loopback::Endpoint {
            port: service_port(),
            timeout: Duration::from_millis(200),
            auth: susi_paths::loopback::Auth::None,
        },
        "POST",
        "/log_error",
        Some(&entry.to_string()),
    )
    .is_some_and(|resp| resp.is_success())
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

/// Default `susi-error` service port before the instance port offset.
pub const DEFAULT_SERVICE_PORT: u16 = susi_paths::ports::ERROR_SERVICE;

/// Port the error-event client posts to: `SUSI_ERROR_PORT` wins, else the
/// default shifted by the instance offset — a dev instance (offset 100)
/// reports to its own service, not the release instance's.
#[must_use]
pub fn service_port() -> u16 {
    std::env::var("SUSI_ERROR_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or_else(|| susi_paths::ports::effective(DEFAULT_SERVICE_PORT))
}

/// Marks this process as the `susi-error` service: its own events go
/// straight to the metrics file instead of being posted back to itself.
pub fn enter_service_mode() {
    susi_paths::enter_service_mode();
}

/// An error event as posted to `POST /log_error` by any local client.
#[derive(Debug, Default, serde::Deserialize)]
pub struct PostedErrorEvent {
    pub variant: Option<String>,
    pub message: Option<String>,
    pub kind: Option<String>,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    pub ts: Option<u64>,
    pub error: Option<String>,
}

/// Normalise, redact and append a posted event to the metrics sink.
///
/// # Errors
/// Fails when the metrics file cannot be written.
pub fn append_posted_event(event: PostedErrorEvent) -> std::io::Result<()> {
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
    let message = contract::redact_for_metrics(&event.error.or(event.message).unwrap_or_default());
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
    sink::append_metrics_line(&error_metrics_path(), &entry)
}

/// The newest `limit` (≤ 500) metrics entries, newest first. Bounded: never
/// reads more than the last 512 KiB of the (capped, rotated) file.
#[must_use]
pub fn recent_error_entries(limit: usize) -> Vec<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    let limit = limit.min(500);
    let path = error_metrics_path();
    let Ok(meta) = std::fs::metadata(&path) else {
        return Vec::new();
    };
    let Ok(mut f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let skip = meta.len().saturating_sub(512 * 1024);
    let mut buf = String::new();
    if f.seek(SeekFrom::Start(skip)).is_err() || f.read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    // Starting mid-file leaves a partial first line — drop it.
    let mut lines: Vec<&str> = buf.lines().collect();
    if skip > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
        .iter()
        .rev()
        .take(limit)
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
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
