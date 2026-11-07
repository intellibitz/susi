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
    oversized_rotated_metrics_in(&data_dir())
}

/// Path-parameterized form of [`oversized_rotated_metrics`] against `home`.
#[must_use]
pub fn oversized_rotated_metrics_in(home: &std::path::Path) -> Option<(PathBuf, u64)> {
    let rotated = home.join("error_metrics.jsonl.1");
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
    record_event_to(service_port(), &error_metrics_path(), entry);
}

fn record_event_to(port: u16, metrics_path: &std::path::Path, entry: &serde_json::Value) {
    if susi_paths::is_service_mode() || !post_event_to(port, entry) {
        // Best effort: nowhere left to report a failed error report.
        let _ = sink::append_metrics_line(metrics_path, entry);
    }
}

/// Best-effort `POST /log_error` against the `susi-error` service on `port`;
/// `true` only when the service accepted the event (2xx), so a rejected or
/// unanswered post still falls back to the local metrics file.
fn post_event_to(port: u16, entry: &serde_json::Value) -> bool {
    susi_paths::loopback::request_with_fallback(
        &susi_paths::loopback::Endpoint {
            port,
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

/// Normalise and redact a [`PostedErrorEvent`] into a JSONL metrics record.
#[must_use]
pub fn normalize_posted_event(event: PostedErrorEvent) -> serde_json::Value {
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
    serde_json::json!({
        "ts": ts,
        "error": message,
        "kind": kind,
        "code": code,
        "retryable": event.retryable.unwrap_or(false),
    })
}

/// Normalise, redact and append a posted event to the metrics sink.
///
/// # Errors
/// Fails when the metrics file cannot be written.
pub fn append_posted_event(event: PostedErrorEvent) -> std::io::Result<()> {
    let entry = normalize_posted_event(event);
    sink::append_metrics_line(&error_metrics_path(), &entry)
}

/// The newest `limit` (≤ 500) metrics entries, newest first. Bounded: never
/// reads more than the last 512 KiB of the (capped, rotated) file.
#[must_use]
pub fn recent_error_entries(limit: usize) -> Vec<serde_json::Value> {
    recent_error_entries_from(&error_metrics_path(), limit)
}

/// Path-parameterized form of [`recent_error_entries`] against `path`.
#[must_use]
pub fn recent_error_entries_from(path: &std::path::Path, limit: usize) -> Vec<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    let limit = limit.min(500);
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let Ok(mut f) = std::fs::File::open(path) else {
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

    #[test]
    fn rewrap_strips_nested_same_kind_prefixes_and_preserves_foreign_prefixes() {
        let cases = [
            ("Governance", "Governance Violation: ", "susi.governance"),
            ("Hardware", "Hardware Error: ", "susi.hardware"),
            ("Protocol", "Protocol Error: ", "susi.protocol"),
            ("Inference", "Inference Error: ", "susi.inference"),
            ("Sandbox", "Sandbox Error: ", "susi.sandbox"),
            ("Config", "Configuration Error: ", "susi.config"),
            ("Io", "I/O Error: ", "susi.io"),
            ("Network", "Network Error: ", "susi.network"),
            ("Filesystem", "Filesystem Error: ", "susi.filesystem"),
            ("Process", "Process Error: ", "susi.process"),
            (
                "Authentication",
                "Authentication Error: ",
                "susi.authentication",
            ),
            (
                "Authorization",
                "Authorization Error: ",
                "susi.authorization",
            ),
            ("Internal", "Internal Engine Error: ", "susi.internal"),
        ];
        for (kind, prefix, expected_code) in cases {
            let nested = format!("{prefix}{prefix}denied");
            let err = rewrap(kind, nested);
            assert_eq!(err.code(), expected_code);
            assert_eq!(err.to_string(), format!("{prefix}denied"));
        }

        let cross = rewrap("Governance", "Network Error: timeout".into());
        assert_eq!(cross.code(), "susi.governance");
        assert_eq!(
            cross.to_string(),
            "Governance Violation: Network Error: timeout"
        );

        let fallback = rewrap("UnknownKind", "boom".into());
        assert_eq!(fallback.code(), "susi.internal");
        assert_eq!(fallback.to_string(), "Internal Engine Error: boom");
    }

    #[test]
    fn recent_error_entries_from_returns_newest_first_and_drops_partial_first_line_past_512kib() {
        let home = audit_home("recent_entries");
        let path = home.join("error_metrics.jsonl");
        assert!(recent_error_entries_from(&path, 10).is_empty());

        let mut payload = String::with_capacity(600 * 1024);
        let pad = "x".repeat(1024);
        for seq in 0..600u64 {
            payload.push_str(&format!(
                "{{\"seq\":{seq},\"pad\":\"{pad}\",\"kind\":\"Network\"}}\n"
            ));
        }
        assert!(payload.len() > 512 * 1024);
        std::fs::write(&path, payload).unwrap();

        let entries = recent_error_entries_from(&path, 5);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0]["seq"], 599);
        assert_eq!(entries[1]["seq"], 598);
        assert_eq!(entries[4]["seq"], 595);

        let all_capped = recent_error_entries_from(&path, 999);
        assert!(all_capped.len() <= 500);
        assert_eq!(all_capped[0]["seq"], 599);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn oversized_rotated_metrics_in_detects_pre_cap_residue_only() {
        let home = audit_home("oversized_rotated");
        assert!(oversized_rotated_metrics_in(&home).is_none());

        let rotated = home.join("error_metrics.jsonl.1");
        let f = std::fs::File::create(&rotated).unwrap();
        f.set_len(METRICS_CAP_BYTES).unwrap();
        assert!(
            oversized_rotated_metrics_in(&home).is_none(),
            "file at exact cap must not be flagged as oversized residue"
        );

        f.set_len(METRICS_CAP_BYTES + 1).unwrap();
        let (detected_path, detected_len) =
            oversized_rotated_metrics_in(&home).expect("file above cap must be flagged");
        assert_eq!(detected_path, rotated);
        assert_eq!(detected_len, METRICS_CAP_BYTES + 1);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn normalize_posted_event_redacts_secrets_and_derives_code() {
        let fake_sk = format!("sk-{}", "a".repeat(32));
        let event = PostedErrorEvent {
            variant: Some("Authentication".into()),
            message: Some(format!("Bearer {fake_sk} rejected")),
            kind: None,
            code: None,
            retryable: Some(true),
            ts: Some(1_700_000_000),
            error: None,
        };
        let norm = normalize_posted_event(event);
        assert_eq!(norm["ts"], 1_700_000_000);
        assert_eq!(norm["kind"], "Authentication");
        assert_eq!(norm["code"], "susi.authentication");
        assert_eq!(norm["retryable"], true);
        let msg = norm["error"].as_str().unwrap();
        assert!(
            !msg.contains(&fake_sk),
            "secret token must be redacted in normalized metrics event: {msg}"
        );
    }

    #[test]
    fn record_event_posts_over_loopback_when_accepted_and_falls_back_to_local_sink_on_5xx() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        fn read_req(stream: &mut TcpStream) -> (String, String) {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            let header_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                buf.extend_from_slice(&chunk[..n]);
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let content_len = head
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buf.len() < body_start + content_len {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                buf.extend_from_slice(&chunk[..n]);
            }
            let body =
                String::from_utf8_lossy(&buf[body_start..body_start + content_len]).to_string();
            (head, body)
        }

        let home = audit_home("record_event_ipc");
        let metrics_file = home.join("error_metrics.jsonl");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            // 1st call: 204 No Content -> accepted by service, no local file write
            let (mut s1, _) = listener.accept().unwrap();
            let (h1, b1) = read_req(&mut s1);
            assert!(h1.starts_with("POST /log_error HTTP/1.0"), "{h1}");
            let j1: serde_json::Value = serde_json::from_str(&b1).unwrap();
            assert_eq!(j1["seq"], 1);
            write!(s1, "HTTP/1.0 204 No Content\r\nContent-Length: 0\r\n\r\n").unwrap();
            s1.flush().unwrap();
            drop(s1);

            // 2nd call: 500 Internal Server Error -> rejected, falls back to local sink
            let (mut s2, _) = listener.accept().unwrap();
            let (h2, b2) = read_req(&mut s2);
            assert!(h2.starts_with("POST /log_error HTTP/1.0"), "{h2}");
            let j2: serde_json::Value = serde_json::from_str(&b2).unwrap();
            assert_eq!(j2["seq"], 2);
            write!(
                s2,
                "HTTP/1.0 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"
            )
            .unwrap();
            s2.flush().unwrap();
            drop(s2);
        });

        record_event_to(port, &metrics_file, &serde_json::json!({"seq": 1}));
        assert!(
            !metrics_file.exists(),
            "2xx service acceptance must not write to local fallback sink"
        );

        record_event_to(port, &metrics_file, &serde_json::json!({"seq": 2}));
        let local_contents = std::fs::read_to_string(&metrics_file).unwrap();
        assert_eq!(local_contents, "{\"seq\":2}\n");

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }
}
