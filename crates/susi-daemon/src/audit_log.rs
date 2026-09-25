//! Tamper-proof Capability Audit Log (Swarm OS Bullet 77)
//!
//! Every capability grant is appended to a SHA-256 hash-chained ledger:
//! each entry's hash covers its own fields plus the previous entry's hash,
//! so editing, dropping, or reordering a past record breaks the chain and
//! `verify_chain` catches it. Optionally mirrored to a tab-separated file
//! for out-of-process inspection.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::RwLock;

use sha2::{Digest, Sha256};

use crate::susi_error::EaiError;

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub cell_id: String,
    pub capability: String,
    pub ts: u64,
    pub prev_hash: [u8; 32],
    pub hash: [u8; 32],
}

fn entry_hash(cell_id: &str, capability: &str, ts: u64, prev_hash: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash);
    hasher.update(cell_id.as_bytes());
    hasher.update(capability.as_bytes());
    hasher.update(ts.to_le_bytes());
    hasher.finalize().into()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct AuditLogger {
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
            path,
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Appends a tamper-evident grant record, chaining it to the prior entry.
    pub fn log_grant(&self, cell_id: &str, capability: &str) -> Result<(), EaiError> {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let prev_hash = entries.last().map(|e| e.hash).unwrap_or([0u8; 32]);
        let ts = now();
        let hash = entry_hash(cell_id, capability, ts, &prev_hash);

        if let Some(path) = &self.path {
            let line = format!("{ts}\t{cell_id}\t{capability}\t{}\n", hex::encode(hash));
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
            prev_hash,
            hash,
        });
        Ok(())
    }

    /// Recomputes the hash chain over the in-memory log, returning `false`
    /// the instant a link is inconsistent with its recorded fields.
    pub fn verify_chain(&self) -> bool {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut prev_hash = [0u8; 32];
        for entry in entries.iter() {
            if entry.prev_hash != prev_hash {
                return false;
            }
            if entry_hash(&entry.cell_id, &entry.capability, entry.ts, &prev_hash) != entry.hash {
                return false;
            }
            prev_hash = entry.hash;
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
        // without recomputing its hash, simulating a tampered record.
        logger.entries.write().unwrap()[0].capability = "filesystem.write".to_string();
        assert!(!logger.verify_chain());
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
