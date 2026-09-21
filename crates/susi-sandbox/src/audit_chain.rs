//! Immutable accountability chain for audit events (Pillar Audit).
//!
//! Append-only: each entry is hash-linked to the previous tip and authenticated
//! with HMAC-SHA256 under a host-local key (`~/.susi/audit.hmac.key`). Rewriting
//! or deleting any historical line breaks the chain or the MAC — the history is
//! cryptographically immutable under the host key.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const GENESIS: &str = "SUSI_AUDIT_GENESIS_v1";
const BLOCK: usize = 64; // SHA-256 block size

static CHAIN_LOCK: Mutex<()> = Mutex::new(());

fn key_path() -> PathBuf {
    susi_paths::SusiDirs::substrate_home().join("audit.hmac.key")
}

fn tip_path(audit_file: &Path) -> PathBuf {
    audit_file.with_extension("chain.tip")
}

/// Load or create the 32-byte host HMAC key.
pub fn load_or_create_hmac_key() -> std::io::Result<[u8; 32]> {
    let path = key_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if path.exists() {
        let bytes = fs::read(&path)?;
        if bytes.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            return Ok(key);
        }
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| std::io::Error::other(e.to_string()))?;
    fs::write(&path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// HMAC-SHA256 without a digest-version-mismatched `hmac` crate.
fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    let mut keyed = [0u8; BLOCK];
    keyed[..32].copy_from_slice(key);
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= keyed[i];
        opad[i] ^= keyed[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut mac = [0u8; 32];
    mac.copy_from_slice(&out);
    mac
}

fn last_hash(audit_file: &Path) -> String {
    let tip = tip_path(audit_file);
    if let Ok(h) = fs::read_to_string(&tip) {
        let t = h.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Ok(content) = fs::read_to_string(audit_file) {
        for line in content.lines().rev() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(h) = v.get("entry_hash").and_then(|x| x.as_str()) {
                    return h.to_string();
                }
            }
        }
    }
    GENESIS.to_string()
}

fn mac_hex(key: &[u8; 32], entry_hash: &str) -> String {
    hex::encode(hmac_sha256(key, entry_hash.as_bytes()))
}

/// Append a cryptographically linked, HMAC-authenticated audit entry.
/// Returns the new `entry_hash`.
pub fn append_signed_entry(
    audit_file: &Path,
    level: &str,
    event_type: &str,
    details: &str,
    pid: u32,
) -> std::io::Result<String> {
    let _guard = CHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let key = load_or_create_hmac_key()?;
    let prev = last_hash(audit_file);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    hasher.update(prev.as_bytes());
    hasher.update(b"|");
    hasher.update(ts.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(level.as_bytes());
    hasher.update(b"|");
    hasher.update(event_type.as_bytes());
    hasher.update(b"|");
    hasher.update(details.as_bytes());
    hasher.update(b"|");
    hasher.update(pid.to_string().as_bytes());
    let entry_hash = hex::encode(hasher.finalize());
    let signature = mac_hex(&key, &entry_hash);

    let log_entry = serde_json::json!({
        "ts": ts,
        "level": level,
        "type": event_type,
        "details": details,
        "pid": pid,
        "prev_hash": prev,
        "entry_hash": entry_hash,
        "hmac": signature,
    });

    if let Some(parent) = audit_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_file)?;
    writeln!(f, "{}", log_entry)?;
    fs::write(tip_path(audit_file), &entry_hash)?;
    Ok(entry_hash)
}

/// Verify the HMAC + hash-link chain in an audit log.
pub fn verify_chain(audit_file: &Path) -> Result<usize, String> {
    let key = load_or_create_hmac_key().map_err(|e| e.to_string())?;
    let content = fs::read_to_string(audit_file).unwrap_or_default();
    if content.trim().is_empty() {
        return Ok(0);
    }
    let mut expected_prev = GENESIS.to_string();
    let mut count = 0usize;
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return Err(format!("line {}: invalid JSON", idx + 1));
        };
        let Some(entry_hash) = v.get("entry_hash").and_then(|x| x.as_str()) else {
            if count > 0 {
                return Err(format!(
                    "line {}: unsigned entry after signed chain began",
                    idx + 1
                ));
            }
            continue;
        };
        let prev = v.get("prev_hash").and_then(|x| x.as_str()).unwrap_or("");
        let hmac = v.get("hmac").and_then(|x| x.as_str()).unwrap_or("");
        let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("");
        let event_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let details = v.get("details").and_then(|x| x.as_str()).unwrap_or("");
        let pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0);
        let ts = v.get("ts").and_then(|x| x.as_u64()).unwrap_or(0);

        if count == 0 && prev == GENESIS {
            expected_prev = GENESIS.to_string();
        }
        if prev != expected_prev {
            return Err(format!(
                "line {}: hash-link break (expected prev {}, got {})",
                idx + 1,
                expected_prev,
                prev
            ));
        }

        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(b"|");
        hasher.update(ts.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(level.as_bytes());
        hasher.update(b"|");
        hasher.update(event_type.as_bytes());
        hasher.update(b"|");
        hasher.update(details.as_bytes());
        hasher.update(b"|");
        hasher.update(pid.to_string().as_bytes());
        let recomputed = hex::encode(hasher.finalize());
        if recomputed != entry_hash {
            return Err(format!("line {}: entry_hash mismatch", idx + 1));
        }
        let expect_mac = mac_hex(&key, entry_hash);
        if expect_mac != hmac {
            return Err(format!("line {}: HMAC signature invalid", idx + 1));
        }
        expected_prev = entry_hash.to_string();
        count += 1;
    }
    Ok(count)
}

/// Public verify helper used by CLI / tests.
pub fn verify_workspace_audit(workspace: &Path) -> Result<usize, String> {
    verify_chain(&workspace.join(".susi/audit.log"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_audit() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("susi_audit_chain_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("audit.log")
    }

    #[test]
    fn signed_entries_verify_and_detect_tamper() {
        let path = temp_audit();
        append_signed_entry(&path, "Info", "TEST_A", "alpha-payload", 1).unwrap();
        append_signed_entry(&path, "Info", "TEST_B", "beta-payload", 1).unwrap();
        assert_eq!(verify_chain(&path).unwrap(), 2);

        let mut content = fs::read_to_string(&path).unwrap();
        content = content.replace("alpha-payload", "EVIL-payload");
        fs::write(&path, &content).unwrap();
        assert!(verify_chain(&path).is_err());
    }
}
