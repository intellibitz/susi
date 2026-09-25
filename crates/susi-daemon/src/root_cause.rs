//! Root-Cause Analysis (Swarm OS Bullet 77)
//!
//! Walks the event store (Bullet 35's log) backward from a failed
//! sequence number and keeps the events `CausalLedger` (Bullet 33)
//! attributes to the same authenticated cell. The earliest of those
//! events is the root. Events with no verified binding are not guessed
//! into the chain.

use crate::causal_ledger::CausalLedger;
use crate::event_sourcing::EventStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalStep {
    pub seq: u64,
    pub cell_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootCause {
    pub failed_seq: u64,
    pub responsible_cell: Option<String>,
    /// Oldest first, including the failure. When the failure is bound to
    /// a cell, only that cell's bound events at or before `failed_seq`
    /// are included, so the first step is that cell's earliest decision
    /// in the log. When the failure itself is unbound, the chain is the
    /// whole prefix — there is no cell to filter on.
    pub chain: Vec<CausalStep>,
}

/// `None` when `failed_seq` is not in the store.
pub fn trace_failure(
    store: &EventStore,
    ledger: &CausalLedger,
    failed_seq: u64,
) -> Option<RootCause> {
    let events = store.events();
    if !events.iter().any(|event| event.seq == failed_seq) {
        return None;
    }
    let responsible_cell = ledger.responsible_cell(failed_seq);
    let chain = events
        .into_iter()
        .filter(|event| event.seq <= failed_seq)
        .filter(|event| match &responsible_cell {
            Some(cell_id) => {
                ledger.responsible_cell(event.seq).as_deref() == Some(cell_id.as_str())
            }
            None => true,
        })
        .map(|event| CausalStep {
            seq: event.seq,
            cell_id: ledger.responsible_cell(event.seq),
        })
        .collect();
    Some(RootCause {
        failed_seq,
        responsible_cell,
        chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityManager;
    use ed25519_dalek::Signer;

    fn bind(
        ledger: &CausalLedger,
        identities: &IdentityManager,
        seq: u64,
        cell_id: &str,
        key: &ed25519_dalek::SigningKey,
    ) {
        let payload = format!("event-{seq}").into_bytes();
        let sig = hex::encode(key.sign(&payload).to_bytes());
        ledger
            .bind(identities, seq, cell_id, &payload, &sig)
            .unwrap();
    }

    #[test]
    fn chain_starts_at_the_failing_cells_earliest_bound_event() {
        let store = EventStore::new();
        let seq_other = store.append_event(b"other");
        let seq_root = store.append_event(b"root");
        let seq_fail = store.append_event(b"fail");
        let seq_later = store.append_event(b"later");

        let mut identities = IdentityManager::new();
        let (id_a, key_a) = IdentityManager::generate_identity("cell-a").unwrap();
        let (id_b, key_b) = IdentityManager::generate_identity("cell-b").unwrap();
        identities.register_identity(&id_a).unwrap();
        identities.register_identity(&id_b).unwrap();

        let ledger = CausalLedger::new();
        bind(&ledger, &identities, seq_other, "cell-b", &key_b);
        bind(&ledger, &identities, seq_root, "cell-a", &key_a);
        bind(&ledger, &identities, seq_fail, "cell-a", &key_a);
        bind(&ledger, &identities, seq_later, "cell-a", &key_a);

        let cause = trace_failure(&store, &ledger, seq_fail).unwrap();
        assert_eq!(cause.responsible_cell.as_deref(), Some("cell-a"));
        assert_eq!(
            cause.chain.iter().map(|step| step.seq).collect::<Vec<_>>(),
            vec![seq_root, seq_fail]
        );
        assert_eq!(cause.chain[0].cell_id.as_deref(), Some("cell-a"));
    }

    #[test]
    fn an_unknown_sequence_is_not_a_cause() {
        let store = EventStore::new();
        store.append_event(b"only");
        assert!(trace_failure(&store, &CausalLedger::new(), 5).is_none());
    }
}
