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

pub mod redact;

use std::backtrace::Backtrace;
use std::error::Error as StdError;
use std::fmt;
use std::path::PathBuf;

/// Substrate data dir, resolved locally so this crate stays a self-contained
/// leaf service. Mirrors `susi-paths`' XDG/legacy rule: `~/.susi` wins when it
/// already exists unless `SUSI_XDG=1|true`; otherwise the platform data dir.
fn data_dir() -> PathBuf {
    let legacy = home_dir().join(".susi");
    let use_xdg = if legacy.is_dir() {
        std::env::var("SUSI_XDG")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
    } else {
        true
    };
    if use_xdg {
        if let Some(p) = directories::ProjectDirs::from("", "intellibitz", "susi") {
            return p.data_local_dir().to_path_buf();
        }
    }
    legacy
}

fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        })
}

/// JSONL sink every error event (local and REST-reported) is appended to.
#[must_use]
pub fn error_metrics_path() -> PathBuf {
    data_dir().join("error_metrics.jsonl")
}

/// The metrics sink is append-only — without a cap the file grows
/// without bound (178MB observed). Past the cap it rotates one
/// generation (`error_metrics.jsonl.1`); a racing writer may lose a
/// line across the rename, which a metrics sink tolerates.
const METRICS_CAP_BYTES: u64 = 64 * 1024 * 1024;

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
pub fn stale_flat_audit_log() -> Option<(PathBuf, u64)> {
    let home = data_dir();
    let flat = home.join("audit.log");
    let flat_meta = std::fs::metadata(&flat).ok()?;
    if !flat_meta.is_file() {
        return None;
    }
    let flat_mtime = flat_meta.modified().ok()?;
    let dated_is_newer = std::fs::read_dir(&home).ok()?.flatten().any(|e| {
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

fn open_metrics_append() -> Option<std::fs::File> {
    let path = error_metrics_path();
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

#[derive(Debug)]
pub enum EaiError {
    Governance(String, Backtrace),
    Hardware(String, Backtrace),
    Protocol(String, Backtrace),
    Inference(String, Backtrace),
    Sandbox(String, Backtrace),
    Config(String, Backtrace),
    Io(String, Backtrace),
    Network(String, Backtrace),
    Filesystem(String, Backtrace),
    Process(String, Backtrace),
    Authentication(String, Backtrace),
    Authorization(String, Backtrace),
    Internal(String, Backtrace),
    #[allow(dead_code)]
    Unknown(Box<dyn StdError + Send + Sync>, Backtrace),
}

macro_rules! impl_eai_err {
    ($fn_name:ident, $variant:ident) => {
        pub fn $fn_name(msg: impl Into<String>) -> Self {
            let e = Self::$variant(msg.into(), Backtrace::capture());
            e.log_to_metrics();
            e
        }
    };
}

impl EaiError {
    impl_eai_err!(governance, Governance);
    impl_eai_err!(hardware, Hardware);
    impl_eai_err!(protocol, Protocol);
    impl_eai_err!(inference, Inference);
    impl_eai_err!(sandbox, Sandbox);
    impl_eai_err!(config, Config);
    impl_eai_err!(io, Io);
    impl_eai_err!(network, Network);
    impl_eai_err!(filesystem, Filesystem);
    impl_eai_err!(process, Process);
    impl_eai_err!(authentication, Authentication);
    impl_eai_err!(authorization, Authorization);
    impl_eai_err!(internal, Internal);

    fn log_to_metrics(&self) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let entry = serde_json::json!({
            "ts": ts,
            "error": self.to_string(),
            "kind": self.kind_name(),
            "code": self.code(),
            "retryable": self.retryable(),
        });

        if let Some(mut f) = open_metrics_append() {
            use std::io::Write;
            let _ = writeln!(f, "{}", entry);
        }
    }

    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Governance(_, _) => "Governance",
            Self::Hardware(_, _) => "Hardware",
            Self::Protocol(_, _) => "Protocol",
            Self::Inference(_, _) => "Inference",
            Self::Sandbox(_, _) => "Sandbox",
            Self::Config(_, _) => "Config",
            Self::Io(_, _) => "Io",
            Self::Network(_, _) => "Network",
            Self::Filesystem(_, _) => "Filesystem",
            Self::Process(_, _) => "Process",
            Self::Authentication(_, _) => "Authentication",
            Self::Authorization(_, _) => "Authorization",
            Self::Internal(_, _) => "Internal",
            Self::Unknown(_, _) => "Unknown",
        }
    }

    /// Stable machine-readable error code (additive; does not rename variants).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Governance(_, _) => "susi.governance",
            Self::Hardware(_, _) => "susi.hardware",
            Self::Protocol(_, _) => "susi.protocol",
            Self::Inference(_, _) => "susi.inference",
            Self::Sandbox(_, _) => "susi.sandbox",
            Self::Config(_, _) => "susi.config",
            Self::Io(_, _) => "susi.io",
            Self::Network(_, _) => "susi.network",
            Self::Filesystem(_, _) => "susi.filesystem",
            Self::Process(_, _) => "susi.process",
            Self::Authentication(_, _) => "susi.authentication",
            Self::Authorization(_, _) => "susi.authorization",
            Self::Internal(_, _) => "susi.internal",
            Self::Unknown(_, _) => "susi.unknown",
        }
    }

    /// Whether a caller may reasonably retry the failed operation.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Network(_, _)
            | Self::Io(_, _)
            | Self::Hardware(_, _)
            | Self::Process(_, _)
            | Self::Inference(_, _)
            | Self::Sandbox(_, _) => true,
            Self::Governance(_, _)
            | Self::Protocol(_, _)
            | Self::Config(_, _)
            | Self::Filesystem(_, _)
            | Self::Authentication(_, _)
            | Self::Authorization(_, _)
            | Self::Internal(_, _)
            | Self::Unknown(_, _) => false,
        }
    }
}

impl fmt::Display for EaiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Governance(msg, _) => write!(f, "Governance Violation: {}", msg),
            Self::Hardware(msg, _) => write!(f, "Hardware Error: {}", msg),
            Self::Protocol(msg, _) => write!(f, "Protocol Error: {}", msg),
            Self::Inference(msg, _) => write!(f, "Inference Error: {}", msg),
            Self::Sandbox(msg, _) => write!(f, "Sandbox Error: {}", msg),
            Self::Config(msg, _) => write!(f, "Configuration Error: {}", msg),
            Self::Io(msg, _) => write!(f, "I/O Error: {}", msg),
            Self::Network(msg, _) => write!(f, "Network Error: {}", msg),
            Self::Filesystem(msg, _) => write!(f, "Filesystem Error: {}", msg),
            Self::Process(msg, _) => write!(f, "Process Error: {}", msg),
            Self::Authentication(msg, _) => write!(f, "Authentication Error: {}", msg),
            Self::Authorization(msg, _) => write!(f, "Authorization Error: {}", msg),
            Self::Internal(msg, _) => write!(f, "Internal Engine Error: {}", msg),
            Self::Unknown(e, _) => write!(f, "Unknown Error: {}", e),
        }
    }
}

impl StdError for EaiError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Unknown(e, _) => Some(e.as_ref()),
            Self::Governance(..)
            | Self::Hardware(..)
            | Self::Protocol(..)
            | Self::Inference(..)
            | Self::Sandbox(..)
            | Self::Config(..)
            | Self::Io(..)
            | Self::Network(..)
            | Self::Filesystem(..)
            | Self::Process(..)
            | Self::Authentication(..)
            | Self::Authorization(..)
            | Self::Internal(..) => None,
        }
    }
}

macro_rules! impl_from_err {
    ($type:ty, $variant:ident) => {
        impl From<$type> for EaiError {
            fn from(err: $type) -> Self {
                let e = Self::$variant(err.to_string(), Backtrace::capture());
                e.log_to_metrics();
                e
            }
        }
    };
}

impl_from_err!(std::io::Error, Io);
impl_from_err!(serde_json::Error, Config);
// Candle / provider errors convert at the GEMI (adapter) edge — not here —
// so `susi-error` stays free of inference-framework dependencies.
impl_from_err!(std::string::FromUtf8Error, Protocol);
impl_from_err!(std::num::ParseIntError, Protocol);

pub type EaiResult<T> = Result<T, EaiError>;

/// Embedded REST service mode: accepts error events over HTTP and appends
/// them to the shared `error_metrics.jsonl` sink. Shared by the standalone
/// `susi-error` binary and the root `susi` binary's `service-run` dispatch.
pub fn serve(port: u16) -> std::io::Result<()> {
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

        let Some(mut f) = open_metrics_append() else {
            return StatusCode::INTERNAL_SERVER_ERROR;
        };
        use std::io::Write;
        match writeln!(f, "{entry}") {
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
            let app = Router::new()
                .route("/log_error", post(log_error))
                .route("/errors/recent", get(recent_errors));
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
}
