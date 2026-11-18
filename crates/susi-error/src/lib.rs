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
pub fn rewrap(kind_name: &str, msg: String) -> EaiError {
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
}
