//! Causal World Model: Event-Identity Binding (Swarm OS Bullet 33)
//!
//! Ties an event (by its `event_sourcing::EventStore` sequence number) to
//! the authenticated cell identity that caused it. The binding is only
//! recorded once the claimed cell's registered Ed25519 key (via
//! `identity::IdentityManager`) verifies a signature over the event's
//! payload — "every state change is tied to an event and an authenticated
//! cell identity" is enforced here, not just asserted by an unverified
//! `cell_id` field.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::identity::IdentityManager;

pub struct CausalLedger {
    bindings: RwLock<HashMap<u64, String>>, // event_seq -> cell_id
}

impl Default for CausalLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl CausalLedger {
    pub fn new() -> Self {
        Self {
            bindings: RwLock::new(HashMap::new()),
        }
    }

    /// Binds `event_seq` to `cell_id`, but only after `identities` verifies
    /// that `signature_hex` is `cell_id`'s real signature over `payload`.
    /// An unauthenticated or forged claim is rejected, not silently
    /// recorded.
    #[allow(clippy::too_many_arguments)] // 5 independent facts the authentication check needs; a params struct would just move the count to every call site
    pub fn bind(
        &self,
        identities: &IdentityManager,
        event_seq: u64,
        cell_id: &str,
        payload: &[u8],
        signature_hex: &str,
    ) -> Result<(), String> {
        if !identities.verify_signature(cell_id, payload, signature_hex) {
            return Err(format!("signature does not verify for cell '{cell_id}'"));
        }
        self.bindings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(event_seq, cell_id.to_string());
        Ok(())
    }

    /// The authenticated cell responsible for `event_seq`, if bound.
    pub fn responsible_cell(&self, event_seq: u64) -> Option<String> {
        self.bindings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&event_seq)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn genuine_signature_binds_the_event_to_its_authenticated_cell() {
        let mut identities = IdentityManager::new();
        let (identity, signing_key) = IdentityManager::generate_identity("cell-a").unwrap();
        identities.register_identity(&identity).unwrap();

        let payload = b"context_graph:node:42:write";
        let sig_hex = hex::encode(signing_key.sign(payload).to_bytes());

        let ledger = CausalLedger::new();
        assert!(
            ledger
                .bind(&identities, 7, "cell-a", payload, &sig_hex)
                .is_ok()
        );
        assert_eq!(ledger.responsible_cell(7), Some("cell-a".to_string()));
    }

    #[test]
    fn a_forged_claim_is_rejected_and_never_bound() {
        let mut identities = IdentityManager::new();
        let (identity, _real_signing_key) = IdentityManager::generate_identity("cell-a").unwrap();
        identities.register_identity(&identity).unwrap();

        // Forger has no key for cell-a; signs with an unrelated key and
        // claims the event happened under cell-a's name.
        let (_forger_identity, forger_key) = IdentityManager::generate_identity("forger").unwrap();
        let payload = b"context_graph:node:42:write";
        let forged_sig_hex = hex::encode(forger_key.sign(payload).to_bytes());

        let ledger = CausalLedger::new();
        assert!(
            ledger
                .bind(&identities, 7, "cell-a", payload, &forged_sig_hex)
                .is_err()
        );
        assert_eq!(ledger.responsible_cell(7), None);
    }

    #[test]
    fn unregistered_cell_is_rejected() {
        let identities = IdentityManager::new();
        let (_identity, signing_key) = IdentityManager::generate_identity("ghost-cell").unwrap();
        let payload = b"payload";
        let sig_hex = hex::encode(signing_key.sign(payload).to_bytes());

        let ledger = CausalLedger::new();
        assert!(
            ledger
                .bind(&identities, 1, "ghost-cell", payload, &sig_hex)
                .is_err()
        );
    }
}
