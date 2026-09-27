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

static CHAIN_LOCK: Mutex<()> = Mutex::new(());

fn key_path() -> PathBuf {
    crate::susi_paths::SusiDirs::substrate_home().join("audit.hmac.key")
}

fn tip_path(audit_file: &Path) -> PathBuf {
    audit_file.with_extension("chain.tip")
}

/// Load or create the 32-byte host HMAC key.
///
/// Creation is atomic and never replaces an existing key: the key is written
/// in full to a process-unique temporary file and then hard-linked into
/// place, which fails if another writer won the race. Concurrent first use
/// (daemon + CLI, parallel tests) therefore converges on one key instead of
/// each signing with its own. A key file of the wrong length is an error,
/// never silently regenerated — regeneration would invalidate every signed
/// entry in history.
pub fn load_or_create_hmac_key() -> std::io::Result<[u8; 32]> {
    load_or_create_key_at(&key_path())
}

fn read_key(path: &Path) -> std::io::Result<[u8; 32]> {
    let bytes = fs::read(path)?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "audit HMAC key {} is {} bytes, expected 32",
                path.display(),
                bytes.len()
            ),
        )
    })
}

fn load_or_create_key_at(path: &Path) -> std::io::Result<[u8; 32]> {
    match read_key(path) {
        Ok(key) => return Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| std::io::Error::other(e.to_string()))?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "key.tmp.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let staged = write_private(&tmp, &key).and_then(|()| fs::hard_link(&tmp, path));
    let _ = fs::remove_file(&tmp);
    match staged {
        Ok(()) => Ok(key),
        // Another writer installed its key first — use theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read_key(path),
        Err(e) => Err(e),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// HMAC-SHA256 — shared implementation lives in `crate::susi_config::cluster_key`
/// (also used by the cluster-key peer handshake).
fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    crate::susi_config::cluster_key::hmac_sha256(key, message)
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
        if mac_hex(&key, entry_hash) != hmac {
            return Err(format!("line {}: HMAC signature invalid", idx + 1));
        }
        expected_prev = entry_hash.to_string();
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi_audit_chain_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn concurrent_key_creation_converges_on_one_key() {
        let path = scratch("race").join("audit.hmac.key");
        let keys: Vec<[u8; 32]> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| scope.spawn(|| load_or_create_key_at(&path).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(keys.iter().all(|k| *k == keys[0]));
        assert_eq!(read_key(&path).unwrap(), keys[0]);
    }

    #[test]
    fn truncated_key_is_an_error_not_a_regeneration() {
        let path = scratch("short").join("audit.hmac.key");
        fs::write(&path, [7u8; 5]).unwrap();
        let err = load_or_create_key_at(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), vec![7u8; 5]);
    }

    #[test]
    fn forged_mac_fails_verification() {
        let log = scratch("forge").join("audit.log");
        append_signed_entry(&log, "Info", "TEST", "genuine", 1).unwrap();
        assert_eq!(verify_chain(&log), Ok(1));
        let content = fs::read_to_string(&log).unwrap();
        let mut entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        entry["hmac"] = serde_json::json!("00".repeat(32));
        fs::write(&log, format!("{entry}\n")).unwrap();
        let err = verify_chain(&log).unwrap_err();
        assert!(err.contains("HMAC signature invalid"), "{err}");
    }
}
