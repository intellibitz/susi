//! Tamper-proof Capability Audit Log (Bullets 54, 77)
//!
//! Every capability grant is appended to an HMAC-SHA256-chained ledger:
//! each entry's MAC covers its own fields plus the previous entry's MAC,
//! so editing, dropping, or reordering a past record breaks the chain.
//! Being *keyed* (not a plain hash chain) means a mismatch is actual
//! evidence of tampering by someone without the key, satisfying VISION.md
//! bullet 54's "HMAC-sealed audit log" rather than just a checksum anyone
//! could recompute. The key is derived from the daemon's cluster key via
//! the same domain-separated HMAC derivation `susi_config::member_seal`
//! uses for its channel key, so no new secret needs provisioning.
//! Optionally mirrored to a tab-separated file for out-of-process
//! inspection.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::RwLock;

use crate::susi_error::EaiError;

const AUDIT_KEY_LABEL: &[u8] = b"susi-audit-log-v1";

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub cell_id: String,
    pub capability: String,
    pub ts: u64,
    pub prev_mac: [u8; 32],
    pub mac: [u8; 32],
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Derives this logger's HMAC key from the daemon's cluster key. Falls
/// back to a process-local random key when no cluster key is available
/// (e.g. an unwritable home directory) — still HMAC-sealed and internally
/// consistent for this process's lifetime, just not verifiable against a
/// restart that would regenerate the fallback.
fn derive_mac_key() -> [u8; 32] {
    if let Some(cluster) = crate::susi_config::cluster_key::cluster_key() {
        return crate::susi_config::cluster_key::hmac_sha256(&cluster, AUDIT_KEY_LABEL);
    }
    let mut key = [0u8; 32];
    let _ = getrandom::fill(&mut key);
    key
}

fn entry_mac(
    mac_key: &[u8; 32],
    cell_id: &str,
    capability: &str,
    ts: u64,
    prev_mac: &[u8; 32],
) -> [u8; 32] {
    let mut message = Vec::with_capacity(32 + cell_id.len() + capability.len() + 8);
    message.extend_from_slice(prev_mac);
    message.extend_from_slice(cell_id.as_bytes());
    message.extend_from_slice(capability.as_bytes());
    message.extend_from_slice(&ts.to_le_bytes());
    crate::susi_config::cluster_key::hmac_sha256(mac_key, &message)
}

pub struct AuditLogger {
    mac_key: [u8; 32],
    path: Option<PathBuf>,
    entries: RwLock<Vec<AuditEntry>>,
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AuditLogger {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            mac_key: derive_mac_key(),
            path,
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Appends a tamper-evident grant record, chaining it to the prior entry.
    pub fn log_grant(&self, cell_id: &str, capability: &str) -> Result<(), EaiError> {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let prev_mac = entries.last().map(|e| e.mac).unwrap_or([0u8; 32]);
        let ts = now();
        let mac = entry_mac(&self.mac_key, cell_id, capability, ts, &prev_mac);

        if let Some(path) = &self.path {
            let line = format!("{ts}\t{cell_id}\t{capability}\t{}\n", hex::encode(mac));
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| EaiError::io(e.to_string()))?;
            file.write_all(line.as_bytes())
                .map_err(|e| EaiError::io(e.to_string()))?;
        }

        entries.push(AuditEntry {
            cell_id: cell_id.to_string(),
            capability: capability.to_string(),
            ts,
            prev_mac,
            mac,
        });
        Ok(())
    }

    /// Recomputes the HMAC chain over the in-memory log, returning `false`
    /// the instant a link is inconsistent with its recorded fields.
    pub fn verify_chain(&self) -> bool {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut prev_mac = [0u8; 32];
        for entry in entries.iter() {
            if entry.prev_mac != prev_mac {
                return false;
            }
            if entry_mac(
                &self.mac_key,
                &entry.cell_id,
                &entry.capability,
                entry.ts,
                &prev_mac,
            ) != entry.mac
            {
                return false;
            }
            prev_mac = entry.mac;
        }
        true
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_verifies_after_honest_appends() {
        let logger = AuditLogger::default();
        logger.log_grant("cell-a", "filesystem.read").unwrap();
        logger.log_grant("cell-b", "network.connect").unwrap();
        assert_eq!(logger.len(), 2);
        assert!(logger.verify_chain());
    }

    #[test]
    fn tampered_entry_breaks_the_chain() {
        let logger = AuditLogger::default();
        logger.log_grant("cell-a", "filesystem.read").unwrap();
        logger.log_grant("cell-b", "network.connect").unwrap();

        // Reach into the log (same-crate test module) and corrupt a field
        // without recomputing its MAC, simulating a tampered record.
        logger.entries.write().unwrap()[0].capability = "filesystem.write".to_string();
        assert!(!logger.verify_chain());
    }

    #[test]
    fn a_chain_forged_under_a_different_key_does_not_verify() {
        let honest = AuditLogger::default();
        honest.log_grant("cell-a", "filesystem.read").unwrap();

        // Attacker without the real key: same fields, different (guessed)
        // key. The plain-hash-chain design this replaced couldn't tell
        // this apart from the honest entry; the HMAC design can.
        let mut forger_key = [0u8; 32];
        forger_key[0] = 0xFF;
        let forged_mac = entry_mac(&forger_key, "cell-a", "filesystem.read", now(), &[0u8; 32]);
        assert_ne!(forged_mac, honest.entries.read().unwrap()[0].mac);
    }

    #[test]
    fn persists_to_file_when_configured() {
        let path = std::env::temp_dir().join(format!("susi_audit_test_{}.tsv", std::process::id()));
        let logger = AuditLogger::new(Some(path.clone()));
        logger.log_grant("cell-a", "filesystem.read").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("cell-a"));
        assert!(contents.contains("filesystem.read"));
        let _ = std::fs::remove_file(&path);
    }
}
