//! Host MAC / privacy policy bootstrap (HMAC key + config mode).

use std::path::Path;
use susi_core::mac_policy::{MacPolicy, PrivacyMode};

/// Wire the process-wide MAC policy. Idempotent (OnceLock first-wins).
///
/// Delegates to `MacPolicy::wired()` — the one construction path every
/// process uses — so the daemon and the cell binaries share the key
/// loader, the sticky-mode precedence, and its fail-closed `local_only`
/// fallback when the substrate key is unavailable.
pub fn wire_mac_policy(_substrate: &Path) {
    let _ = MacPolicy::global();
}

/// Persist privacy mode for subsequent boots.
pub fn persist_privacy_mode(mode: PrivacyMode) -> std::io::Result<()> {
    // A wired policy's `set_mode` owns the one atomic write of the sticky
    // file (a second, plain write there reopened the torn-read window).
    // An unwired policy (substrate key unavailable, forced local_only)
    // still records the operator's choice for the next boot.
    let policy = MacPolicy::global();
    if !policy.persists_mode() {
        let path = susi_paths::SusiDirs::substrate_home().join("privacy_mode");
        crate::susi_config::atomic_write_bytes(&path, mode.as_str().as_bytes())?;
    }
    policy.set_mode(mode)
}

/// `susi privacy mode <mode>` / MCP `privacy_mode`: persist the mode for
/// subsequent boots and, under `local_only`, pin inference routing to local
/// (through the router, so it lands where the router reads it). Returns the
/// mode now in force and what it implies.
pub fn set_privacy_mode(mode: &str) -> std::io::Result<serde_json::Value> {
    let m = PrivacyMode::parse(mode);
    persist_privacy_mode(m)?;
    if matches!(m, PrivacyMode::LocalOnly) {
        susi_gemi::engines::routing::InferenceRouter::set_local_only();
    }
    let policy = MacPolicy::global();
    Ok(serde_json::json!({
        "mode": m.as_str(),
        "network_scope": m.network_scope(),
        "mandatory_sandbox": policy.mandatory_sandbox(),
        "blocks_cloud_inference": policy.blocks_cloud_inference(),
    }))
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn wire_mac_policy_is_idempotent_across_calls() {
        let dir = std::env::temp_dir().join("susi-privacy-inline-wire");
        wire_mac_policy(&dir);
        wire_mac_policy(&dir);
        let _ = MacPolicy::global().mode();
    }

    #[test]
    fn privacy_mode_parse_maps_synonyms_and_defaults_to_balanced() {
        assert_eq!(PrivacyMode::parse("local_only"), PrivacyMode::LocalOnly);
        assert_eq!(PrivacyMode::parse("edge"), PrivacyMode::LocalOnly);
        assert_eq!(PrivacyMode::parse("open"), PrivacyMode::Open);
        assert_eq!(PrivacyMode::parse("unknown_mode"), PrivacyMode::Balanced);
    }

    #[test]
    fn privacy_mode_parse_persisted_rejects_unknown_strings() {
        assert_eq!(
            PrivacyMode::parse_persisted("balanced"),
            Some(PrivacyMode::Balanced)
        );
        assert_eq!(
            PrivacyMode::parse_persisted("local_only"),
            Some(PrivacyMode::LocalOnly)
        );
        assert_eq!(
            PrivacyMode::parse_persisted("open"),
            Some(PrivacyMode::Open)
        );
        assert_eq!(PrivacyMode::parse_persisted("corrupted"), None);
    }

    #[test]
    fn privacy_mode_as_str_roundtrips_with_parse_persisted() {
        for mode in [
            PrivacyMode::Balanced,
            PrivacyMode::LocalOnly,
            PrivacyMode::Open,
        ] {
            assert_eq!(PrivacyMode::parse_persisted(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn privacy_mode_parse_handles_case_and_whitespace_cleanly() {
        assert_eq!(PrivacyMode::parse("  LOCAL_ONLY  "), PrivacyMode::LocalOnly);
        assert_eq!(PrivacyMode::parse("OPEN"), PrivacyMode::Open);
        assert_eq!(PrivacyMode::parse("Balanced"), PrivacyMode::Balanced);
    }
}
