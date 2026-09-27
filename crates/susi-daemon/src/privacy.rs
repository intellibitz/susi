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
    let path = crate::susi_paths::SusiDirs::substrate_home().join("privacy_mode");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, mode.as_str())?;
    MacPolicy::global().set_mode(mode);
    Ok(())
}
