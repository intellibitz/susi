//! The `EaiError` contract shared by every crate. Error events are recorded
//! through the crate root's `record_event`.

use std::backtrace::Backtrace;
use std::error::Error as StdError;
use std::fmt;

pub(super) type VariantSpec = (
    &'static str,
    &'static str,
    bool,
    &'static str,
    fn(String) -> EaiError,
);

/// `governance.secret_tokens` from the bundled default config.
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

/// Masks bundled secret tokens and environment credentials before recording metrics.
#[must_use]
pub fn redact_for_metrics(text: &str) -> String {
    super::redact::redact_patterns(
        bundled_secret_tokens(),
        &super::redact::mask_env_credentials(text),
    )
}

macro_rules! eai_variants {
    ($(($ctor:ident, $Variant:ident, $kind:literal, $code:literal, $retry:literal, $prefix:literal)),+ $(,)?) => {
        #[derive(Debug)]
        pub enum EaiError {
            $($Variant(String, Backtrace),)+
            Unknown(Box<dyn StdError + Send + Sync>, Backtrace),
        }

        pub(super) const VARIANT_SPECS: &[VariantSpec] = &[
            $(($kind, $code, $retry, $prefix, |m| EaiError::$ctor(m)),)+
        ];

        impl EaiError {
            $(
                pub fn $ctor(msg: impl Into<String>) -> Self {
                    let e = Self::$Variant(msg.into(), Backtrace::capture());
                    e.log_to_metrics();
                    e
                }
            )+

            fn log_to_metrics(&self) {
                let entry = serde_json::json!({
                    "ts": susi_paths::unix_epoch_elapsed().as_secs(),
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
                    $(Self::$Variant(_, _) => $kind,)+
                    Self::Unknown(_, _) => "Unknown",
                }
            }

            #[must_use]
            pub const fn code(&self) -> &'static str {
                match self {
                    $(Self::$Variant(_, _) => $code,)+
                    Self::Unknown(_, _) => "susi.unknown",
                }
            }

            #[must_use]
            pub const fn retryable(&self) -> bool {
                match self {
                    $(Self::$Variant(_, _) => $retry,)+
                    Self::Unknown(_, _) => false,
                }
            }

            /// Prefix the message with `ctx` ("ctx: message"), keeping the kind.
            #[must_use]
            pub fn context(self, ctx: impl fmt::Display) -> Self {
                let prefix = |msg: String| format!("{ctx}: {msg}");
                match self {
                    $(Self::$Variant(m, b) => Self::$Variant(prefix(m), b),)+
                    Self::Unknown(e, b) => Self::Internal(prefix(e.to_string()), b),
                }
            }
        }

        impl fmt::Display for EaiError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(Self::$Variant(msg, _) => write!(f, "{}{}", $prefix, msg),)+
                    Self::Unknown(e, _) => write!(f, "Unknown Error: {}", e),
                }
            }
        }

        impl StdError for EaiError {
            fn source(&self) -> Option<&(dyn StdError + 'static)> {
                match self {
                    Self::Unknown(e, _) => Some(e.as_ref()),
                    $(Self::$Variant(..) => None,)+
                }
            }
        }
    };
}

eai_variants!(
    (
        governance,
        Governance,
        "Governance",
        "susi.governance",
        false,
        "Governance Violation: "
    ),
    (
        hardware,
        Hardware,
        "Hardware",
        "susi.hardware",
        true,
        "Hardware Error: "
    ),
    (
        protocol,
        Protocol,
        "Protocol",
        "susi.protocol",
        false,
        "Protocol Error: "
    ),
    (
        inference,
        Inference,
        "Inference",
        "susi.inference",
        true,
        "Inference Error: "
    ),
    (
        sandbox,
        Sandbox,
        "Sandbox",
        "susi.sandbox",
        true,
        "Sandbox Error: "
    ),
    (
        config,
        Config,
        "Config",
        "susi.config",
        false,
        "Configuration Error: "
    ),
    (io, Io, "Io", "susi.io", true, "I/O Error: "),
    (
        network,
        Network,
        "Network",
        "susi.network",
        true,
        "Network Error: "
    ),
    (
        filesystem,
        Filesystem,
        "Filesystem",
        "susi.filesystem",
        false,
        "Filesystem Error: "
    ),
    (
        process,
        Process,
        "Process",
        "susi.process",
        true,
        "Process Error: "
    ),
    (
        authentication,
        Authentication,
        "Authentication",
        "susi.authentication",
        false,
        "Authentication Error: "
    ),
    (
        authorization,
        Authorization,
        "Authorization",
        "susi.authorization",
        false,
        "Authorization Error: "
    ),
    (
        internal,
        Internal,
        "Internal",
        "susi.internal",
        false,
        "Internal Engine Error: "
    ),
);

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
impl_from_err!(std::env::VarError, Config);
impl_from_err!(std::fs::TryLockError, Io);
impl_from_err!(String, Internal);
impl_from_err!(&str, Internal);

pub type EaiResult<T> = Result<T, EaiError>;

/// `anyhow`-style context for results and options, producing [`EaiError`].
pub trait ResultExt<T> {
    fn context<C: fmt::Display>(self, ctx: C) -> EaiResult<T>;
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

#[macro_export]
macro_rules! eai_err {
    ($fmt:literal $($arg:tt)*) => {
        $crate::EaiError::internal(format!($fmt $($arg)*))
    };
    ($err:expr $(,)?) => {
        $crate::EaiError::internal($err.to_string())
    };
}

#[macro_export]
macro_rules! eai_bail {
    ($($arg:tt)*) => {
        return ::core::result::Result::Err($crate::EaiError::internal(format!($($arg)*)))
    };
}

#[cfg(test)]
mod contract_tests {
    use super::{
        bundled_secret_tokens, redact_for_metrics, EaiError, EaiResult, ResultExt, VARIANT_SPECS,
    };
    use std::error::Error as _;

    #[test]
    fn metrics_text_masks_bundled_secret_tokens() {
        assert!(bundled_secret_tokens().iter().any(|t| t == "ghp_"));
        let text = redact_for_metrics(
            "git push failed: https://x:ghp_abcDEF123456@github.com and key sk-live_9f8e7d",
        );
        assert!(!text.contains("ghp_abcDEF123456"), "{text}");
        assert!(!text.contains("sk-live_9f8e7d"), "{text}");
        assert!(text.contains("git push failed"));
    }

    #[test]
    fn every_constructor_reports_its_kind_code_retry_class_display_and_context() {
        for &(kind, code, retryable, prefix, ctor) in VARIANT_SPECS {
            let err = ctor("probe".to_string());
            assert_eq!(err.kind_name(), kind);
            assert_eq!(err.code(), code);
            assert_eq!(err.retryable(), retryable, "{kind}");
            assert_eq!(err.to_string(), format!("{prefix}probe"));
            assert!(err.source().is_none(), "{kind} has no source");

            let ctx = ctor("inner".to_string()).context("outer").context("top");
            assert_eq!(ctx.kind_name(), kind);
            assert_eq!(ctx.code(), code);
            assert_eq!(ctx.to_string(), format!("{prefix}top: outer: inner"));
        }
    }

    #[test]
    fn unknown_carries_its_source_and_becomes_internal_under_context() {
        let inner = std::io::Error::other("disk gone");
        let unknown = EaiError::Unknown(Box::new(inner), Backtrace::disabled());
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
    fn result_and_option_context_preserve_kinds_and_stay_lazy() {
        let io: Result<(), std::io::Error> = Err(std::io::Error::other("denied"));
        let err = io.context("open file").unwrap_err();
        assert_eq!(err.kind_name(), "Io");
        assert_eq!(err.to_string(), "I/O Error: open file: denied");

        let parse: Result<u8, std::num::ParseIntError> = "x".parse::<u8>();
        let err = parse.with_context(|| format!("port {}", 7)).unwrap_err();
        assert_eq!(err.kind_name(), "Protocol");
        assert!(err.to_string().starts_with("Protocol Error: port 7: "));

        let mut called = false;
        assert_eq!(
            Ok::<u8, std::io::Error>(3)
                .with_context(|| {
                    called = true;
                    "never"
                })
                .unwrap(),
            3
        );
        assert!(!called);

        assert_eq!(Some(5).context("missing").unwrap(), 5);
        assert_eq!(
            None::<u8>.context("missing value").unwrap_err().to_string(),
            "Internal Engine Error: missing value"
        );
        assert_eq!(
            None::<u8>
                .with_context(|| "lazy none")
                .unwrap_err()
                .to_string(),
            "Internal Engine Error: lazy none"
        );
    }

    #[test]
    fn eai_err_and_eai_bail_build_internal_errors() {
        assert_eq!(
            crate::eai_err!("bad {} of {}", "row", 3).to_string(),
            "Internal Engine Error: bad row of 3"
        );
        assert_eq!(
            crate::eai_err!(std::io::Error::other("wrapped")).to_string(),
            "Internal Engine Error: wrapped"
        );
        fn bails(fail: bool) -> EaiResult<u8> {
            if fail {
                crate::eai_bail!("stopped at {}", 9);
            }
            Ok(1)
        }
        assert_eq!(bails(false).unwrap(), 1);
        assert_eq!(
            bails(true).unwrap_err().to_string(),
            "Internal Engine Error: stopped at 9"
        );
    }
}
