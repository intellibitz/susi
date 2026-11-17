//! Process-wide MAC policy bootstrap (HMAC key + config mode).
//!
//! Privacy *mode changes* (`susi privacy mode`, MCP `privacy_mode`) are
//! served by `susi-plug-host` on `daemon.privacy.mode`.

use std::path::Path;
use susi_core::mac_policy::MacPolicy;

/// Wire the process-wide MAC policy. Idempotent (OnceLock first-wins).
pub fn wire_mac_policy(_substrate: &Path) {
    let _ = MacPolicy::global();
}
