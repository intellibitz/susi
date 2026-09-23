//! Host MAC / privacy policy bootstrap (HMAC key + config mode).

use std::path::Path;
use susi_core::mac_policy::{MacPolicy, PrivacyMode};
use susi_sandbox::manager::SusiConfig;

fn mac_key_path() -> std::path::PathBuf {
    crate::susi_paths::SusiDirs::substrate_home().join("mac.hmac.key")
}

fn load_or_create_mac_key() -> [u8; 32] {
    let path = mac_key_path();
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() >= 32
    {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes[..32]);
        return key;
    }
    // Two 128-bit nonces → 32 bytes of key material.
    let a = crate::susi_config::cluster_key::random_nonce_hex();
    let b = crate::susi_config::cluster_key::random_nonce_hex();
    let combined = format!("{a}{b}");
    let mut key = [0u8; 32];
    for (i, chunk) in combined.as_bytes().chunks(2).take(32).enumerate() {
        if chunk.len() == 2 {
            let hex_byte = std::str::from_utf8(chunk).unwrap_or("00");
            key[i] = u8::from_str_radix(hex_byte, 16).unwrap_or(0);
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, key);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    key
}

/// Wire MAC policy from host config. Idempotent (OnceLock first-wins).
pub fn wire_mac_policy(_substrate: &Path) {
    let cfg = SusiConfig::load_global().unwrap_or_default();
    let privacy = cfg.privacy();
    let mode = PrivacyMode::parse(&privacy.mode);
    let mandatory = privacy.mandatory_sandbox_for_exec || matches!(mode, PrivacyMode::LocalOnly);
    let key = load_or_create_mac_key();
    let policy = MacPolicy::init_global(key, mode, mandatory);
    policy.set_mandatory_sandbox(mandatory);
    let sticky = crate::susi_paths::SusiDirs::substrate_home().join("privacy_mode");
    if let Ok(text) = std::fs::read_to_string(&sticky) {
        let m = PrivacyMode::parse(text.trim());
        policy.set_mode(m);
        policy.set_mandatory_sandbox(
            privacy.mandatory_sandbox_for_exec || matches!(m, PrivacyMode::LocalOnly),
        );
    }
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
