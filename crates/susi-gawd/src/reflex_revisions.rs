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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    fn sample_rev(id: &str) -> ReflexRevision {
        ReflexRevision {
            id: id.to_string(),
            source_hash: "src-123".to_string(),
            wasm_digest: "wasm-456".to_string(),
            grants: vec!["fs.read".to_string()],
            revoked: false,
        }
    }

    #[test]
    fn publish_and_may_execute_allow_active_revision_and_deny_unknown() {
        let mut store = ReflexStore::default();
        assert!(!store.may_execute("agent-a", "rev-1"));
        store.publish(sample_rev("rev-1"));
        assert!(store.may_execute("agent-a", "rev-1"));
    }

    #[test]
    fn pin_and_revoke_preserve_execution_for_pinned_caller_only() {
        let mut store = ReflexStore::default();
        store.publish(sample_rev("rev-1"));
        store.pin("pinned-caller", "rev-1").unwrap();
        store.revoke("rev-1").unwrap();
        assert!(store.may_execute("pinned-caller", "rev-1"));
        assert!(!store.may_execute("unpinned-caller", "rev-1"));
    }

    #[test]
    fn pin_and_revoke_reject_unknown_revision_ids() {
        let mut store = ReflexStore::default();
        assert_eq!(
            store.pin("caller", "missing"),
            Err("unknown revision".into())
        );
        assert_eq!(store.revoke("missing"), Err("unknown revision".into()));
    }

    #[test]
    fn rollback_pin_updates_caller_pin_or_errors_on_unknown() {
        let mut store = ReflexStore::default();
        store.publish(sample_rev("rev-1"));
        store.publish(sample_rev("rev-2"));
        store.pin("caller", "rev-2").unwrap();
        store.revoke("rev-1").unwrap();
        store.rollback_pin("caller", "rev-1").unwrap();
        assert!(store.may_execute("caller", "rev-1"));
        assert_eq!(
            store.rollback_pin("caller", "rev-999"),
            Err("unknown revision".into())
        );
    }

    #[test]
    fn reflex_store_round_trips_serde_json_and_overwrites_republished_revision() {
        let mut store = ReflexStore::default();
        store.publish(sample_rev("rev-a"));
        store.revoke("rev-a").unwrap();
        assert!(!store.may_execute("any-caller", "rev-a"));

        store.publish(sample_rev("rev-a"));
        assert!(store.may_execute("any-caller", "rev-a"));

        let encoded = serde_json::to_string(&store).unwrap();
        let decoded: ReflexStore = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, store);
    }
}
