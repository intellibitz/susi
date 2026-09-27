//! Shared host-config store helpers for catalog override JSON (Mandate 3 DRY).

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Create a private (0700 on Unix) directory for host overrides.
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomically write pretty JSON with 0600 perms on Unix — the canonical
/// race-safe writer, adapted to this crate's `anyhow` results.
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    Ok(crate::susi_config::atomic_write_json_pretty(path, value)?)
}
