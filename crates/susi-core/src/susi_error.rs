//! Vendored `susi-error` contract + IPC reporter.
//!
//! Decoupled crates own their error type locally; error *events* still reach
//! the shared `error_metrics.jsonl` sink through the standalone `susi-error`
//! service (`POST 127.0.0.1:18081/log_error`, override via `SUSI_ERROR_PORT`).
//! When the service is unreachable the event is appended to the local file
//! directly so the metrics guarantee never depends on service health.
//!
//! Keep this file identical across the workspace.

use std::backtrace::Backtrace;
use std::error::Error as StdError;
use std::fmt;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

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
        if !post_event(&entry) {
            append_local(&entry);
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
impl_from_err!(std::string::FromUtf8Error, Protocol);
impl_from_err!(std::num::ParseIntError, Protocol);

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

pub type EaiResult<T> = Result<T, EaiError>;
