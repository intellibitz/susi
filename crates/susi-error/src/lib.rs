use std::backtrace::Backtrace;
use std::error::Error as StdError;
use std::fmt;

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
        let metrics_file = susi_paths::SusiDirs::data_dir().join("error_metrics.jsonl");
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

        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(metrics_file)
        {
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
            _ => None,
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
