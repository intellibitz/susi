//! Durable peer identity revocation (VC-201-033).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMaterial {
    pub peer_id: String,
    pub key_id: String,
    pub public: String,
}

#[derive(Debug, Default, Clone)]
pub struct IdentityStore {
    pub active: BTreeMap<String, KeyMaterial>,
    pub revoked: BTreeSet<String>,
    pub historical: Vec<KeyMaterial>,
}

impl IdentityStore {
    pub fn rotate(
        &mut self,
        peer_id: &str,
        new_key: KeyMaterial,
        endorsements: &BTreeSet<String>,
        electorate: &BTreeSet<String>,
    ) -> Result<(), String> {
        let quorum = electorate.len() / 2 + 1;
        let votes = endorsements.intersection(electorate).count();
        if votes < quorum {
            return Err("quorum failed".into());
        }
        if let Some(old) = self.active.remove(peer_id) {
            self.historical.push(old);
        }
        self.active.insert(peer_id.to_string(), new_key);
        Ok(())
    }

    pub fn revoke(
        &mut self,
        key_id: &str,
        endorsements: &BTreeSet<String>,
        electorate: &BTreeSet<String>,
    ) -> Result<(), String> {
        let quorum = electorate.len() / 2 + 1;
        if endorsements.intersection(electorate).count() < quorum {
            return Err("quorum failed".into());
        }
        self.revoked.insert(key_id.to_string());
        self.active.retain(|_, k| k.key_id != key_id);
        Ok(())
    }

    #[must_use]
    pub fn credential_ok_after_restart(&self, key_id: &str) -> bool {
        !self.revoked.contains(key_id) && self.active.values().any(|k| k.key_id == key_id)
    }

    #[must_use]
    pub fn historical_verifiable(&self, key_id: &str) -> bool {
        self.historical.iter().any(|k| k.key_id == key_id)
            || self.active.values().any(|k| k.key_id == key_id)
            || self.revoked.contains(key_id)
    }
}
