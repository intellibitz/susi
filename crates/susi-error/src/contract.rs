//! The `EaiError` contract shared by every crate. Error events are recorded
//! through the crate root's `record_event`.

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
    Unknown(Box<dyn StdError + Send + Sync>, Backtrace),
}

/// `governance.secret_tokens` from the bundled default config — the same
/// list `redact_credentials` falls back to. The error contract sits below
/// the config crate and cannot load the host config, so it redacts with
/// the bundled list; an unparseable bundle yields no patterns, and the
/// environment credential mask still applies.
fn bundled_secret_tokens() -> &'static [String] {
    static TOKENS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    TOKENS.get_or_init(|| {
        serde_json::from_str::<serde_json::Value>(include_str!(
            "../../../config/config.default.json"
        ))
        .ok()
        .and_then(|cfg| {
            cfg.pointer("/governance/secret_tokens")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
        })
        .unwrap_or_default()
    })
}

/// Error text is recorded in `error_metrics.jsonl` (and posted to the
/// `susi-error` service); it routinely carries command stderr, URLs, and
/// provider responses, so credentials are masked before it leaves the
/// process (Mandate 10).
pub(crate) fn redact_for_metrics(text: &str) -> String {
    super::redact::redact_patterns(
        bundled_secret_tokens(),
        &super::redact::mask_env_credentials(text),
    )
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
            "error": redact_for_metrics(&self.to_string()),
            "kind": self.kind_name(),
            "code": self.code(),
            "retryable": self.retryable(),
        });

        super::record_event(&entry);
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
impl_from_err!(std::env::VarError, Config);
impl_from_err!(std::fs::TryLockError, Io);
impl_from_err!(String, Internal);
impl_from_err!(&str, Internal);

pub type EaiResult<T> = Result<T, EaiError>;

impl EaiError {
    /// Prefix the message with `ctx` ("ctx: message"), keeping the kind.
    /// Rewrites in place — no second metrics event for the same failure.
    #[must_use]
    pub fn context(self, ctx: impl fmt::Display) -> Self {
        let prefix = |msg: String| format!("{ctx}: {msg}");
        match self {
            Self::Governance(m, b) => Self::Governance(prefix(m), b),
            Self::Hardware(m, b) => Self::Hardware(prefix(m), b),
            Self::Protocol(m, b) => Self::Protocol(prefix(m), b),
            Self::Inference(m, b) => Self::Inference(prefix(m), b),
            Self::Sandbox(m, b) => Self::Sandbox(prefix(m), b),
            Self::Config(m, b) => Self::Config(prefix(m), b),
            Self::Io(m, b) => Self::Io(prefix(m), b),
            Self::Network(m, b) => Self::Network(prefix(m), b),
            Self::Filesystem(m, b) => Self::Filesystem(prefix(m), b),
            Self::Process(m, b) => Self::Process(prefix(m), b),
            Self::Authentication(m, b) => Self::Authentication(prefix(m), b),
            Self::Authorization(m, b) => Self::Authorization(prefix(m), b),
            Self::Internal(m, b) => Self::Internal(prefix(m), b),
            Self::Unknown(e, b) => Self::Internal(prefix(e.to_string()), b),
        }
    }
}

/// `anyhow`-style context for results and options, producing [`EaiError`].
/// An error that already converts into `EaiError` (I/O, JSON, …) keeps
/// its kind; a `None` becomes `Internal` with the context as the message.
pub trait ResultExt<T> {
    /// Attach `ctx` to the failure.
    ///
    /// # Errors
    /// The original failure, as an `EaiError` prefixed with `ctx`.
    fn context<C: fmt::Display>(self, ctx: C) -> EaiResult<T>;

    /// Attach a lazily built context to the failure.
    ///
    /// # Errors
    /// The original failure, as an `EaiError` prefixed with `f()`.
    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> EaiResult<T>;
}

impl<T, E: Into<EaiError>> ResultExt<T> for Result<T, E> {
    fn context<C: fmt::Display>(self, ctx: C) -> EaiResult<T> {
        self.map_err(|e| e.into().context(ctx))
    }

    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> EaiResult<T> {
        self.map_err(|e| e.into().context(f()))
    }
}

impl<T> ResultExt<T> for Option<T> {
    fn context<C: fmt::Display>(self, ctx: C) -> EaiResult<T> {
        self.ok_or_else(|| EaiError::internal(ctx.to_string()))
    }

    fn with_context<C: fmt::Display, F: FnOnce() -> C>(self, f: F) -> EaiResult<T> {
        self.ok_or_else(|| EaiError::internal(f().to_string()))
    }
}

/// `EaiError::internal(format!(...))` — the `anyhow!` shape for crates on
/// the workspace error contract.
#[macro_export]
macro_rules! eai_err {
    ($fmt:literal $($arg:tt)*) => {
        $crate::EaiError::internal(format!($fmt $($arg)*))
    };
    ($err:expr $(,)?) => {
        $crate::EaiError::internal($err.to_string())
    };
}

/// `return Err(EaiError::internal(format!(...)))` — the `anyhow::bail!`
/// shape for crates on the workspace error contract.
#[macro_export]
macro_rules! eai_bail {
    ($($arg:tt)*) => {
        return ::core::result::Result::Err($crate::EaiError::internal(format!($($arg)*)))
    };
}

#[cfg(test)]
mod metrics_redaction_tests {
    use super::{bundled_secret_tokens, redact_for_metrics};

    #[test]
    fn metrics_text_masks_bundled_secret_tokens() {
        // Proves the bundled config is embedded and its pointer resolves.
        assert!(bundled_secret_tokens().iter().any(|t| t == "ghp_"));
        let text = redact_for_metrics(
            "git push failed: https://x:ghp_abcDEF123456@github.com and key sk-live_9f8e7d",
        );
        assert!(!text.contains("ghp_abcDEF123456"), "{text}");
        assert!(!text.contains("sk-live_9f8e7d"), "{text}");
        assert!(text.contains("git push failed"));
    }
}

#[cfg(test)]
mod contract_tests {
    use super::{EaiError, EaiResult, ResultExt};
    use std::error::Error as _;

    type Ctor = fn(String) -> EaiError;

    /// Every named constructor with its kind, code, retry class and
    /// display prefix — one row per variant.
    fn constructors() -> Vec<(Ctor, &'static str, &'static str, bool, &'static str)> {
        vec![
            (
                |m| EaiError::governance(m),
                "Governance",
                "susi.governance",
                false,
                "Governance Violation: ",
            ),
            (
                |m| EaiError::hardware(m),
                "Hardware",
                "susi.hardware",
                true,
                "Hardware Error: ",
            ),
            (
                |m| EaiError::protocol(m),
                "Protocol",
                "susi.protocol",
                false,
                "Protocol Error: ",
            ),
            (
                |m| EaiError::inference(m),
                "Inference",
                "susi.inference",
                true,
                "Inference Error: ",
            ),
            (
                |m| EaiError::sandbox(m),
                "Sandbox",
                "susi.sandbox",
                true,
                "Sandbox Error: ",
            ),
            (
                |m| EaiError::config(m),
                "Config",
                "susi.config",
                false,
                "Configuration Error: ",
            ),
            (|m| EaiError::io(m), "Io", "susi.io", true, "I/O Error: "),
            (
                |m| EaiError::network(m),
                "Network",
                "susi.network",
                true,
                "Network Error: ",
            ),
            (
                |m| EaiError::filesystem(m),
                "Filesystem",
                "susi.filesystem",
                false,
                "Filesystem Error: ",
            ),
            (
                |m| EaiError::process(m),
                "Process",
                "susi.process",
                true,
                "Process Error: ",
            ),
            (
                |m| EaiError::authentication(m),
                "Authentication",
                "susi.authentication",
                false,
                "Authentication Error: ",
            ),
            (
                |m| EaiError::authorization(m),
                "Authorization",
                "susi.authorization",
                false,
                "Authorization Error: ",
            ),
            (
                |m| EaiError::internal(m),
                "Internal",
                "susi.internal",
                false,
                "Internal Engine Error: ",
            ),
        ]
    }

    #[test]
    fn every_constructor_reports_its_kind_code_retry_class_and_display() {
        for (ctor, kind, code, retryable, prefix) in constructors() {
            let err = ctor("probe".to_string());
            assert_eq!(err.kind_name(), kind);
            assert_eq!(err.code(), code);
            assert_eq!(err.retryable(), retryable, "{kind}");
            assert_eq!(err.to_string(), format!("{prefix}probe"));
            assert!(err.source().is_none(), "{kind} has no source");
        }
    }

    #[test]
    fn context_prefixes_in_place_and_keeps_the_kind() {
        for (ctor, kind, code, _, prefix) in constructors() {
            let err = ctor("inner".to_string()).context("outer").context("top");
            assert_eq!(err.kind_name(), kind);
            assert_eq!(err.code(), code);
            assert_eq!(err.to_string(), format!("{prefix}top: outer: inner"));
        }
    }

    #[test]
    fn unknown_carries_its_source_and_becomes_internal_under_context() {
        let inner = std::io::Error::other("disk gone");
        let unknown = EaiError::Unknown(Box::new(inner), std::backtrace::Backtrace::disabled());
        assert_eq!(unknown.kind_name(), "Unknown");
        assert_eq!(unknown.code(), "susi.unknown");
        assert!(!unknown.retryable());
        assert_eq!(unknown.to_string(), "Unknown Error: disk gone");
        assert_eq!(unknown.source().unwrap().to_string(), "disk gone");

        let ctx = unknown.context("reading state");
        assert_eq!(ctx.kind_name(), "Internal");
        assert_eq!(
            ctx.to_string(),
            "Internal Engine Error: reading state: disk gone"
        );
    }

    #[test]
    fn result_context_keeps_the_converted_kind_and_with_context_is_lazy() {
        let io: Result<(), std::io::Error> = Err(std::io::Error::other("denied"));
        let err = io.context("open file").unwrap_err();
        assert_eq!(err.kind_name(), "Io");
        assert_eq!(err.to_string(), "I/O Error: open file: denied");

        let parse: Result<u8, std::num::ParseIntError> = "x".parse::<u8>();
        let err = parse.with_context(|| format!("port {}", 7)).unwrap_err();
        assert_eq!(err.kind_name(), "Protocol");
        assert!(err.to_string().starts_with("Protocol Error: port 7: "));

        let ok: Result<u8, std::io::Error> = Ok(3);
        let mut called = false;
        assert_eq!(
            ok.with_context(|| {
                called = true;
                "never"
            })
            .unwrap(),
            3
        );
        assert!(!called, "context closure must not run on success");
    }

    #[test]
    fn option_context_passes_some_and_turns_none_into_internal() {
        assert_eq!(Some(5).context("missing").unwrap(), 5);
        let err = None::<u8>.context("missing value").unwrap_err();
        assert_eq!(err.kind_name(), "Internal");
        assert_eq!(err.to_string(), "Internal Engine Error: missing value");

        let mut called = false;
        assert_eq!(
            Some(1)
                .with_context(|| {
                    called = true;
                    "never"
                })
                .unwrap(),
            1
        );
        assert!(!called);
        let err = None::<u8>.with_context(|| "lazy none").unwrap_err();
        assert_eq!(err.to_string(), "Internal Engine Error: lazy none");
    }

    #[test]
    fn eai_err_and_eai_bail_build_internal_errors() {
        let literal = crate::eai_err!("bad {} of {}", "row", 3);
        assert_eq!(literal.kind_name(), "Internal");
        assert_eq!(literal.to_string(), "Internal Engine Error: bad row of 3");

        let source = std::io::Error::other("wrapped");
        let expr = crate::eai_err!(source);
        assert_eq!(expr.to_string(), "Internal Engine Error: wrapped");

        fn bails(fail: bool) -> EaiResult<u8> {
            if fail {
                crate::eai_bail!("stopped at {}", 9);
            }
            Ok(1)
        }
        assert_eq!(bails(false).unwrap(), 1);
        let err = bails(true).unwrap_err();
        assert_eq!(err.kind_name(), "Internal");
        assert_eq!(err.to_string(), "Internal Engine Error: stopped at 9");
    }
}
