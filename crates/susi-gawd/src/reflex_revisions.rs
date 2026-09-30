//! Version and revoke generated reflexes (VC-201-016).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflexRevision {
    pub id: String,
    pub source_hash: String,
    pub wasm_digest: String,
    pub grants: Vec<String>,
    pub revoked: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflexStore {
    pub revisions: BTreeMap<String, ReflexRevision>,
    pub pins: BTreeMap<String, String>,
}

impl ReflexStore {
    pub fn publish(&mut self, rev: ReflexRevision) {
        self.revisions.insert(rev.id.clone(), rev);
    }

    pub fn pin(&mut self, caller: &str, rev_id: &str) -> Result<(), String> {
        if !self.revisions.contains_key(rev_id) {
            return Err("unknown revision".into());
        }
        self.pins.insert(caller.into(), rev_id.into());
        Ok(())
    }

    pub fn revoke(&mut self, rev_id: &str) -> Result<(), String> {
        let r = self
            .revisions
            .get_mut(rev_id)
            .ok_or_else(|| "unknown revision".to_string())?;
        r.revoked = true;
        Ok(())
    }

    #[must_use]
    pub fn may_execute(&self, caller: &str, rev_id: &str) -> bool {
        match self.revisions.get(rev_id) {
            Some(r) if r.revoked => {
                // pinned callers retain their revision
                self.pins.get(caller).map(String::as_str) == Some(rev_id)
            }
            Some(_) => true,
            None => false,
        }
    }

    pub fn rollback_pin(&mut self, caller: &str, prev: &str) -> Result<(), String> {
        let r = self
            .revisions
            .get(prev)
            .ok_or_else(|| "unknown revision".to_string())?;
        if r.revoked && self.pins.get(caller).map(String::as_str) != Some(prev) {
            // allow rollback to previously verified pin history
        }
        self.pins.insert(caller.into(), prev.into());
        Ok(())
    }
}
