//! Host MAC / privacy policy bootstrap (HMAC key + config mode).

use std::path::Path;
use susi_core::mac_policy::{MacPolicy, PrivacyMode};

/// Wire the process-wide MAC policy. Idempotent (OnceLock first-wins).
///
/// Delegates to `MacPolicy::wired()` — the one construction path every
/// vendored copy uses — so the daemon and the leaf crates share the key
/// loader, the sticky-mode precedence, and its fail-closed `local_only`
/// fallback when the substrate key is unavailable.
pub fn wire_mac_policy(_substrate: &Path) {
    let _ = MacPolicy::global();
}

/// Persist privacy mode for subsequent boots.
pub fn persist_privacy_mode(mode: PrivacyMode) -> std::io::Result<()> {
    // Atomic: an empty/torn read parses as `balanced`, which would briefly
    // downgrade a `local_only` host in every process polling this file.
    let path = crate::susi_paths::SusiDirs::substrate_home().join("privacy_mode");
    crate::susi_config::atomic_write_bytes(&path, mode.as_str().as_bytes())?;
    MacPolicy::global().set_mode(mode);
    Ok(())
}
