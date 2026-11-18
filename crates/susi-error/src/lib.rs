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

//! The `EaiError` contract every SUSI crate returns, plus the local JSONL
//! error-event sink behind it.

pub mod redact;
pub mod sink;

mod contract;
pub use contract::{redact_for_metrics, EaiError, EaiResult, ResultExt};

use std::path::PathBuf;

/// JSONL sink every error event is appended to.
#[must_use]
pub fn error_metrics_path() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("error_metrics.jsonl")
}

fn record_event(entry: &serde_json::Value) {
    let _ = sink::append_metrics_line(&error_metrics_path(), entry);
}

/// Rebuilds an error from another boundary's rendered text, preserving its
/// kind/code so the metrics stream stays truthful.
#[must_use]
pub fn rewrap(kind_name: &str, mut msg: String) -> EaiError {
    if let Some(&(_, _, _, prefix, ctor)) = contract::VARIANT_SPECS
        .iter()
        .find(|&&(kind, ..)| kind == kind_name)
    {
        while let Some(rest) = msg.strip_prefix(prefix) {
            msg = rest.to_string();
        }
        return ctor(msg);
    }
    EaiError::internal(msg)
}

/// Rebuilds an `EaiError` from a displayed `"<Prefix>: <msg>"` string, falling
/// back to `EaiError::protocol` when no known prefix matches.
#[must_use]
pub fn rewrap_display(display: &str) -> EaiError {
    contract::VARIANT_SPECS
        .iter()
        .find(|&&(_, _, _, prefix, _)| display.starts_with(prefix))
        .map_or_else(
            || EaiError::protocol(display),
            |&(kind, ..)| rewrap(kind, display.to_string()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_and_retryable_are_stable() {
        let _env = susi_paths::test_env::EnvGuard::isolated();
        let net = EaiError::network("down");
        assert_eq!(net.code(), "susi.network");
        assert!(net.retryable());
        let gov = EaiError::governance("veto");
        assert_eq!(gov.code(), "susi.governance");
        assert!(!gov.retryable());
        assert!(error_metrics_path().is_file());
    }

    #[test]
    fn rewrap_strips_nested_same_kind_prefixes_and_preserves_foreign_prefixes() {
        let _env = susi_paths::test_env::EnvGuard::isolated();
        for &(kind, expected_code, _, prefix, _) in contract::VARIANT_SPECS {
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
}
