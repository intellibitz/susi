//! Authoritative memory provenance and retention (VC-201-081).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub recorded_unix: u64,
    pub retention_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub body: String,
    pub provenance: Provenance,
}

impl MemoryRecord {
    #[must_use]
    pub fn expired(&self, now: u64) -> bool {
        now.saturating_sub(self.provenance.recorded_unix) >= self.provenance.retention_secs
    }

    #[must_use]
    pub fn authoritative_source(&self) -> &str {
        &self.provenance.source
    }
}
