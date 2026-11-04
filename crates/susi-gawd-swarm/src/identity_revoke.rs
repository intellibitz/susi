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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    fn three_peer_electorate() -> BTreeSet<String> {
        BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()])
    }

    #[test]
    fn rotate_requires_majority_quorum_and_moves_previous_key_to_historical() {
        let mut store = IdentityStore::default();
        let electorate = three_peer_electorate();
        let minority = BTreeSet::from(["a".to_string()]);
        let majority = BTreeSet::from(["a".to_string(), "b".to_string()]);

        let k1 = KeyMaterial {
            peer_id: "p1".to_string(),
            key_id: "k1".to_string(),
            public: "pub1".to_string(),
        };
        assert!(store
            .rotate("p1", k1.clone(), &minority, &electorate)
            .is_err());
        assert!(store.rotate("p1", k1, &majority, &electorate).is_ok());

        let k2 = KeyMaterial {
            peer_id: "p1".to_string(),
            key_id: "k2".to_string(),
            public: "pub2".to_string(),
        };
        assert!(store.rotate("p1", k2, &majority, &electorate).is_ok());
        assert_eq!(store.historical.len(), 1);
        assert_eq!(store.historical[0].key_id, "k1");
    }

    #[test]
    fn revoke_requires_majority_quorum_and_removes_key_from_active_set() {
        let mut store = IdentityStore::default();
        let electorate = three_peer_electorate();
        let majority = BTreeSet::from(["a".to_string(), "b".to_string()]);
        store
            .rotate(
                "p1",
                KeyMaterial {
                    peer_id: "p1".to_string(),
                    key_id: "k1".to_string(),
                    public: "pub1".to_string(),
                },
                &majority,
                &electorate,
            )
            .expect("initial rotate");

        let minority = BTreeSet::from(["c".to_string()]);
        assert!(store.revoke("k1", &minority, &electorate).is_err());
        assert!(store.revoke("k1", &majority, &electorate).is_ok());
        assert!(store.revoked.contains("k1"));
        assert!(!store.active.contains_key("p1"));
    }

    #[test]
    fn credential_ok_after_restart_admits_only_unrevoked_active_key_ids() {
        let mut store = IdentityStore::default();
        let electorate = three_peer_electorate();
        let majority = BTreeSet::from(["a".to_string(), "b".to_string()]);
        store
            .rotate(
                "p1",
                KeyMaterial {
                    peer_id: "p1".to_string(),
                    key_id: "k-active".to_string(),
                    public: "pub".to_string(),
                },
                &majority,
                &electorate,
            )
            .expect("rotate");
        assert!(store.credential_ok_after_restart("k-active"));
        assert!(!store.credential_ok_after_restart("k-unknown"));

        store
            .revoke("k-active", &majority, &electorate)
            .expect("revoke");
        assert!(!store.credential_ok_after_restart("k-active"));
    }

    #[test]
    fn historical_verifiable_remains_true_for_active_rotated_and_revoked_keys() {
        let mut store = IdentityStore::default();
        let electorate = three_peer_electorate();
        let majority = BTreeSet::from(["a".to_string(), "b".to_string()]);
        store
            .rotate(
                "p1",
                KeyMaterial {
                    peer_id: "p1".to_string(),
                    key_id: "k-old".to_string(),
                    public: "pub1".to_string(),
                },
                &majority,
                &electorate,
            )
            .expect("rotate k-old");
        store
            .rotate(
                "p1",
                KeyMaterial {
                    peer_id: "p1".to_string(),
                    key_id: "k-cur".to_string(),
                    public: "pub2".to_string(),
                },
                &majority,
                &electorate,
            )
            .expect("rotate k-cur");
        store
            .revoke("k-revoked", &majority, &electorate)
            .expect("revoke");

        assert!(store.historical_verifiable("k-old"));
        assert!(store.historical_verifiable("k-cur"));
        assert!(store.historical_verifiable("k-revoked"));
        assert!(!store.historical_verifiable("k-never-existed"));
    }
}
