//! Process-wide MAC policy bootstrap (HMAC key + config mode).
//!
//! Privacy *mode changes* (`susi privacy mode`, MCP `privacy_mode`) are
//! served by `susi-plug-host` on `daemon.privacy.mode`. Public Rust
//! facades keep the original signatures and forward over the plane bus.

use std::path::Path;
use susi_core::mac_policy::{MacPolicy, PrivacyMode};
use susi_core::plane_bus::{daemon, topics};

use crate::plug_forward;

/// Wire the process-wide MAC policy. Idempotent (OnceLock first-wins).
pub fn wire_mac_policy(_substrate: &Path) {
    let _ = MacPolicy::global();
}

/// Persist privacy mode for subsequent boots.
pub fn persist_privacy_mode(mode: PrivacyMode) -> std::io::Result<()> {
    set_privacy_mode(mode.as_str()).map(|_| ())
}

/// `susi privacy mode <mode>` / MCP `privacy_mode`: persist the mode and
/// report what is now in force. An unknown mode is refused before the hop
/// so a typo never relaxes a stricter mode into `balanced`.
pub fn set_privacy_mode(mode: &str) -> std::io::Result<serde_json::Value> {
    let parsed = PrivacyMode::parse_requested(mode).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "unknown privacy mode `{mode}` (expected balanced, local_only, host_only or open)"
            ),
        )
    })?;
    match plug_forward::call(
        daemon::privacy_mode(parsed.as_str()),
        topics::DAEMON_PRIVACY_MODE,
    ) {
        Ok(value) => Ok(value),
        Err(error) => Err(std::io::Error::other(error)),
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn an_unknown_requested_mode_is_refused_before_anything_is_written() {
        let err = set_privacy_mode("host_onyl").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("host_only"), "{err}");
    }
}
